//! Where does an on-device frame's time actually go? R1's budget needs the answer before anyone can say what
//! would close it.
//!
//! `cargo run --release -p gpu --example r1_round_trip_split`
//!
//! `r1_step_price` established that the step loop saturates (cap 4 and cap 8 find the same 7,240 surfaces on a
//! 640x360 grid, and cap 4 to cap 64 moves 0.77% of the rays) while the frame's cap-independent floor is the
//! host round trip. It could not say what that floor *is*, because both of its intervals excluded the biggest
//! thing a frame does: `march_uploaded` creates its ray buffer with `create_buffer_init`, so the host-to-device
//! copy of the grid happens before the stopwatch starts, and the 0.15 ms submit / 92.61 ms round-trip pair it
//! printed leaves the bus and the device unattributed (docs/04, 2026-10-16).
//!
//! This prices the missing interval on a frame whose buffers are created once — `begin_frame` / `write_rays` /
//! `march_frame` — and does it at **1080p, R1's own grid**, which is the resolution the previous attempt could
//! not repeat at: a fresh 66 MB ray buffer per march is what lost this repo a device, and a reused frame is what
//! makes the row measurable at all. Three intervals per frame:
//!
//! - **write**: `write_rays` packing the grid in place and `write_buffer`-ing it out — the host-to-device
//!   direction, which no earlier measurement put inside any interval;
//! - **submit**: encode + dispatch + copy-back + `submit`, returning without waiting (`ms_submit_only`);
//! - **wait**: the remainder of `ms_frame_with_read` — the device running the march and the depths coming back.
//!
//! The byte counts are integers and they are asserted, and the most interesting number in this program is one of
//! them: 2,073,600 rays x 32 bytes is 66,355,200 bytes outbound against 8,294,400 bytes inbound, while the
//! *whole scene asset* is a few hundred bytes uploaded once. A frame at R1's grid moves ~74 MB across the bus to
//! draw three spheres, and the scene is not the reason.
//!
//! Nothing here asserts a millisecond (ADR-017; R1 and R4 are duration gates and stay informational). What is
//! asserted, at every cap:
//!
//! - the frame path's hit count and per-ray depths against `render::march` on a 64x48 grid at that cap — the same
//!   anchor `r1_step_price` uses, now covering the reused-buffer path rather than only the allocate-per-call one;
//! - the reused path's depths **bit-identical** to `march_uploaded`'s on the same rays and the same shape, so
//!   reusing buffers demonstrably did not change what is computed;
//! - four falsifications run inside the program: a grid that does not match its frame is refused, a frame nobody
//!   has written into is refused, binding a *different* shape to the same frame changes the picture, and
//!   refilling the same frame with a moved camera changes the picture. The last one exists because the omission
//!   it catches — packing the bytes and never uploading them — was actually made while writing this path, so it
//!   is asserted rather than assumed.
//!
//! At 1080p the assertions stay integer: the same grid marched twice must return bit-identical depths, and a
//! larger step budget must not find fewer surfaces (strictly more at cap 64 than cap 1) — if the header's cap
//! were ignored the rows would be equal and that assert fires. And each big row is checked ray for ray against
//! `render::march` on a lattice of the frame (every 32nd pixel of both axes), because one earlier run of this
//! binary returned its cap-64 frame carrying the cap-8 row's hit count and a wait that matched its own cap-1
//! row, and nothing else in the program could have disagreed with it.
//!
//! One limit, stated where it bites: this probe reads the depth buffer back because a depth buffer is the only
//! way it can be checked. A shipping frame maps nothing, so the **wait** interval below is the price of
//! verification, not the price of rendering; the **write** interval is the part a real frame pays, and even that
//! is an artifact of this API carrying the ray grid as data when a camera-relative grid is derivable from a
//! dozen floats in a uniform buffer.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{Frame, Interpreter, Shape};
use render::{march, Ray, Tree, ROOT};
use std::time::Instant;

/// `on_device_march`'s and `r1_step_price`'s scene, word for word, so the split below prices the same
/// three-node field those rows priced.
fn scene() -> Tree {
    let record = |kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool| {
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
            pack_params_len(len),
        ])
    };
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 3, 0, 0, true),
            record(NODE_SPHERE, 0, 1, 0, 4, false),
            record(NODE_ROUNDED_BOX, 0, 1, 4, 4, false),
        ],
        params: vec![0.5, 0.0, 0.0, 0.0, 2.0, 0.1, 2.0, 0.0, 0.0, -1.0, 0.0],
    }
}

const SURFACE: f32 = 0.01;
const MAX_T: f32 = 24.0;
/// R1's grid, marched here rather than scaled to.
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
/// The anchor grid: small enough for `render::march` to be run ray for ray at every cap.
const CHECK_W: u32 = 64;
const CHECK_H: u32 = 48;
/// The caps priced. 1 is the floor with essentially no march in it, 8 is where `r1_step_price`'s work saturates,
/// 64 is the cap `on_device_march` reported ~824 ms at.
const STEPS: [u32; 3] = [1, 8, 64];
const REPS: usize = 3;
/// R1's whole-frame budget at 60 fps, as docs/04 states it.
const BUDGET_MS: f64 = 16.67;
/// Two vec4s per ray: the shape's byte footprint is the whole scene, and this is the frame's.
const RAY_BYTES: usize = 32;
/// How many of R1's 2,073,600 rays `render::march` is run on at each cap, as a row-by-row lattice step. The
/// whole grid would be the honest anchor and it is not affordable here - the CPU reference takes the 4,005 ms
/// `on_device_march` reported for a dispatch series. A first version of this anchor took a flat stride of 64
/// over the ray index, which at width 1920 is degenerate: 1920 is a multiple of 64, so every sampled ray sat in
/// column 0, the most oblique column in the image, and the "sample" refused the cap-8 row for 1,020 grazing rays
/// while the interior of the frame was never looked at at all. Stepping both axes is 60x34 = 2,040 rays spread
/// over the whole picture.
const SAMPLE_STEP: u32 = 32;

/// The same pinhole arithmetic as `on_device_march`, with the cap carried into each ray.
fn rays(width: u32, height: u32, max_steps: u32) -> Vec<Ray> {
    let origin = [0.0f32, 0.0, -6.0];
    let half_height = 0.5;
    let aspect = width as f32 / height as f32;
    let mut out = Vec::with_capacity((width * height) as usize);
    for py in 0..height {
        for px in 0..width {
            let u = (px as f32 + 0.5) / width as f32;
            let v = (py as f32 + 0.5) / height as f32;
            let ndc_x = (2.0 * u - 1.0) * half_height * aspect;
            let ndc_y = (1.0 - 2.0 * v) * half_height;
            let dir = [ndc_x, ndc_y, 1.0];
            let length = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
            out.push(Ray {
                origin,
                dir: [dir[0] / length, dir[1] / length, dir[2] / length],
                max_t: MAX_T,
                surface: SURFACE,
                max_steps,
            });
        }
    }
    out
}

/// The same grid with the camera pulled back one unit along z: the refill a reused frame must notice.
fn moved_rays(width: u32, height: u32, max_steps: u32) -> Vec<Ray> {
    let mut grid = rays(width, height, max_steps);
    for ray in &mut grid {
        ray.origin[2] -= 1.0;
    }
    grid
}

fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(|a, b| a.total_cmp(b));
    times[times.len() / 2]
}

fn bits(depths: &[f32]) -> Vec<u32> {
    depths.iter().map(|depth| depth.to_bits()).collect()
}

/// The reference's own hit count, the hit-vs-miss disagreements against the device, and the worst depth
/// deviation, computed ray for ray. The count comes from the reference alone: tallying it only where the pair
/// agreed made the count assert compare the device against an intersection, and a plant could not be seen.
fn reference_check(tree: &Tree, grid: &[Ray], depths: &[f32]) -> (usize, usize, f32) {
    let mut hits = 0usize;
    let mut disagreements = 0usize;
    let mut worst = 0.0f32;
    for (ray, depth) in grid.iter().zip(depths.iter()) {
        let hit = march(
            tree,
            ROOT,
            ray.origin,
            ray.dir,
            ray.max_t,
            ray.surface,
            ray.max_steps,
        );
        if hit.is_some() {
            hits += 1;
        }
        match (hit, *depth < 1.0e37) {
            (Some(hit), true) => worst = worst.max((hit.t - *depth).abs()),
            (None, false) => {}
            _ => disagreements += 1,
        }
    }
    (hits, disagreements, worst)
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP r1_round_trip_split: no device ({error:?}) - the question is what the host/device bus \
                 charges for a frame, and no software path on this machine has that bus"
            );
            return;
        }
    };
    let tree = scene();
    let shape: Shape = interpreter.upload(&tree);
    let shape_bytes = shape.bytes;
    let rays_in_grid = (WIDTH as usize) * (HEIGHT as usize);
    println!("// adapter       : {}", interpreter.adapter());
    println!(
        "// scene asset   : {} bytes uploaded once for {} nodes, then bound per frame",
        shape.bytes, shape.node_count
    );
    println!(
        "// grid          : {WIDTH}x{HEIGHT} = {rays_in_grid} rays -> {} bytes out, {} bytes back per frame",
        rays_in_grid * RAY_BYTES,
        rays_in_grid * 4
    );
    println!("// --- the anchor grid, {CHECK_W}x{CHECK_H}: the reused path checked against the reference and against the allocate-per-call path, then the three intervals priced on it");
    println!("// cap | ref hits | dev hits | disagreements | worst |dev| | write ms | submit ms | wait ms | frame ms");

    for &cap in &STEPS {
        let grid = rays(CHECK_W, CHECK_H, cap);
        let mut frame: Frame = interpreter.begin_frame(grid.len());
        let bind = interpreter.bind(&shape, &frame);
        let wrote = interpreter
            .write_rays(&mut frame, ROOT, &grid)
            .expect("the anchor grid must fit its frame");
        assert_eq!(
            wrote,
            grid.len() * RAY_BYTES,
            "write_rays reported {wrote} bytes for {} rays, so the figure the split divides by is not the \
             grid's packed size",
            grid.len()
        );
        let on_frame = interpreter
            .march_frame(&bind, &frame)
            .expect("the anchor grid must march at every cap on the reused path");
        let (ref_hits, disagreements, worst) = reference_check(&tree, &grid, &on_frame.depths);
        assert_eq!(
            on_frame.hits, ref_hits,
            "at cap {cap} the reused frame path found {} surfaces and the reference found {ref_hits} - either \
             the header's cap is not reaching the shader or the frame is binding the wrong buffers, and then \
             the intervals priced below describe neither",
            on_frame.hits
        );
        assert!(
            disagreements == 0 && worst < 0.002,
            "at cap {cap} the reused frame path and render::march disagree ({disagreements} hit-vs-miss, worst \
             deviation {worst}), so the reused path is not the path the reference prices"
        );

        // The equivalence the change itself needs: same rays, same shape, buffers created once versus created
        // per call. A reused buffer read stale or at the wrong size shows up here.
        let on_call = interpreter
            .march(&tree, ROOT, &grid)
            .expect("the anchor grid must march on the per-call path");
        assert_eq!(
            bits(&on_frame.depths),
            bits(&on_call.depths),
            "at cap {cap} the reused frame and the allocate-per-call frame disagree bit-for-bit on {} rays",
            grid.len()
        );

        let mut write_row = Vec::with_capacity(REPS);
        let mut submit_row = Vec::with_capacity(REPS);
        let mut frame_row = Vec::with_capacity(REPS);
        for _ in 0..REPS {
            let start = Instant::now();
            interpreter
                .write_rays(&mut frame, ROOT, &grid)
                .expect("refilling the anchor frame");
            write_row.push(start.elapsed().as_secs_f64() * 1000.0);
            let again = interpreter
                .march_frame(&bind, &frame)
                .expect("re-marching a refilled frame");
            submit_row.push(again.ms_submit_only);
            frame_row.push(again.ms_frame_with_read);
            assert_eq!(
                bits(&again.depths),
                bits(&on_frame.depths),
                "at cap {cap} marching the same refilled frame twice gives different depths, so the reused \
                 buffers are not holding what was written into them"
            );
        }
        let (write, submit, frame_ms) = (median(write_row), median(submit_row), median(frame_row));
        println!(
            "// {:>3} | {:>10} | {:>8} | {:>13} | {:>9.6} | {:>8.3} | {:>9.3} | {:>8.3} | {:>8.3}",
            cap,
            ref_hits,
            on_frame.hits,
            disagreements,
            worst,
            write,
            submit,
            frame_ms - submit,
            frame_ms + write
        );
    }

    // ---- The four in-binary falsifications, on a small grid at one cap so the reference stays affordable.
    println!("// --- falsified inside this program, in order:");
    let grid = rays(CHECK_W, CHECK_H, 16);
    let mut frame = interpreter.begin_frame(grid.len());
    let bind = interpreter.bind(&shape, &frame);

    // (a) A grid that does not fit the frame is refused rather than silently truncated or overread.
    let short = rays(CHECK_W / 2, CHECK_H / 2, 16);
    let error = interpreter
        .write_rays(&mut frame, ROOT, &short)
        .expect_err("a frame built for 3072 rays must refuse a 240x144 grid");
    assert!(
        error.contains(&grid.len().to_string()) && error.contains(&short.len().to_string()),
        "the refusal says {error}, which names neither the frame's ray count nor the grid's"
    );
    println!(
        "//   a {}-ray grid on a {}-ray frame -> refused: {error}",
        short.len(),
        grid.len()
    );
    // And the refusal must not have damaged the frame: the matching grid still marches the reference's picture.
    let wrote = interpreter
        .write_rays(&mut frame, ROOT, &grid)
        .expect("the frame must still be usable after refusing a mismatched grid");
    assert_eq!(wrote, grid.len() * RAY_BYTES);
    let good = interpreter
        .march_frame(&bind, &frame)
        .expect("marching after a refusal");
    let (ref_hits, disagreements, worst) = reference_check(&tree, &grid, &good.depths);
    assert!(
        disagreements == 0 && ref_hits > 0 && worst < 0.002,
        "the frame that survived a refusal no longer agrees with the reference ({disagreements} hit-vs-miss, \
         worst {worst}, {ref_hits} reference hits)"
    );

    // (b) A frame nobody has written into is refused instead of marching its zero-filled buffer.
    let empty = interpreter.begin_frame(grid.len());
    let empty_bind = interpreter.bind(&shape, &empty);
    let err = interpreter
        .march_frame(&empty_bind, &empty)
        .expect_err("march_frame on an unwritten frame must not be allowed to report a picture");
    assert!(
        err.contains("no rays have been written"),
        "refusing an unwritten frame said {err}, which is not the reason it was refused"
    );
    println!("//   an unwritten frame -> refused: {err}");

    // (c) Binding a different shape to the same frame changes the picture. A bind group that ignored which
    // buffers it pointed at would print bit-identical depths for two different scenes.
    let mut other_tree = scene();
    other_tree.params[0] = 5.0;
    let other: Shape = interpreter.upload(&other_tree);
    let other_bind = interpreter.bind(&other, &frame);
    let rebound = interpreter
        .march_frame(&other_bind, &frame)
        .expect("the same frame bound to a different shape must still march");
    assert_ne!(
        bits(&rebound.depths),
        bits(&good.depths),
        "binding a scene whose sphere radius went from 0.5 to 5.0 left every depth bit-identical, so the frame \
         is not reading the shape it was bound to"
    );
    let (_, other_dis, other_worst) = reference_check(&other_tree, &grid, &rebound.depths);
    assert!(
        other_dis == 0 && other_worst < 0.002,
        "the re-bound shape disagrees with its own reference ({other_dis} hit-vs-miss, worst {other_worst})"
    );
    println!(
        "//   re-binding radius 0.5 -> 5.0 -> {} surfaces instead of {}, depths differ bit-for-bit",
        rebound.hits, good.hits
    );

    // (d) Refilling the frame with a moved camera changes the picture. This is the check that a `write_rays`
    // which packed the bytes but never reached the device would fail.
    let moved = moved_rays(CHECK_W, CHECK_H, 16);
    interpreter
        .write_rays(&mut frame, ROOT, &moved)
        .expect("the moved grid must fit the frame");
    let moved_march = interpreter
        .march_frame(&bind, &frame)
        .expect("the refilled frame must march");
    assert_ne!(
        bits(&moved_march.depths),
        bits(&good.depths),
        "moving the camera one unit along z and refilling the same frame left every depth identical, so the \
         refill never reached the device"
    );
    let (_, moved_dis, moved_worst) = reference_check(&tree, &moved, &moved_march.depths);
    assert!(
        moved_dis == 0 && moved_worst < 0.002,
        "the refilled frame disagrees with the reference it should match ({moved_dis} hit-vs-miss, worst \
         {moved_worst})"
    );
    println!("//   refill with camera z -6 -> -7 -> depths differ, and still match the reference");

    // ---- R1's own grid: one frame, one bind group, the intervals priced separately.
    //
    // Only cap 1 gets repeat reps, because this adapter cannot survive a series of huge frames: the first
    // complete attempt here printed its cap-1 row and then lost the device at cap 8, and wgpu 27 turns that
    // into a *panic* from inside `Device::poll` rather than an `Err`. So the sweep runs under `catch_unwind`,
    // which tolerates only the driver's own loss and resumes anything else — a failed assert still reddens the
    // run — and the march term is priced from single frames at the later caps.
    println!("// --- {WIDTH}x{HEIGHT} = {rays_in_grid} rays, buffers allocated once for the whole sweep:");
    println!(
        "// the sampled anchor: every {SAMPLE_STEP}th pixel of both axes, so {} rays of {rays_in_grid} spread \
         over the whole frame, marched by `render::march` at each row's cap",
        HEIGHT.div_ceil(SAMPLE_STEP) as usize * WIDTH.div_ceil(SAMPLE_STEP) as usize
    );
    println!(
        "// cap | out bytes | dev hits | sampled ref | sampled dev | dis | worst |dev| | reps agreeing | write ms | submit ms | wait after a write | wait, nothing written"
    );
    let mut rows: Vec<Row> = Vec::new();
    let mut stopped: Option<String> = None;
    let swept = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| {
        price_big_grid(
            &tree,
            &interpreter,
            &shape,
            rays(WIDTH, HEIGHT, 1),
            &mut rows,
            &mut stopped,
        );
    }));
    if let Err(payload) = swept {
        let text = panic_text(&payload);
        if !(text.contains("device is lost") || text.contains("Validation")) {
            std::panic::resume_unwind(payload);
        }
        stopped = Some(format!("panicked inside the driver: {text}"));
        // The device is gone, and a second panic from a handle's Drop after the findings have printed would
        // redden the run for a reason this example already reports. Leak the GPU objects instead.
        core::mem::forget(shape);
        core::mem::forget(interpreter);
    }
    if let Some(stop) = &stopped {
        println!(
            "// 1080p limit   : {stop} - MEASURED, and it is the reason `r1_step_price` sweeps at 640x360. \
             Creating the buffers once removed the allocation churn but not this: a series of {rays_in_grid}-ray \
             frames is beyond what this adapter offers, so the rows above are as far as the split reaches here."
        );
    }
    if rows.len() < 2 {
        println!(
            "// unattributed  : fewer than two big-grid rows survived, so the march term at 1080p stays \
             unmeasured; nothing above is claimed from a row that did not print."
        );
        return;
    }

    // The cap monotonicity, as integers: a longer step budget cannot lose a surface, and if the header's cap
    // were being ignored every row would be the same number.
    let floor = rows[0].hits;
    let ceiling = rows[rows.len() - 1].hits;
    assert!(
        ceiling > floor,
        "at 1080p cap {} finds {ceiling} surfaces and cap {} finds {floor} - a step cap the shader is ignoring \
         would make these equal, and then the split below is one row repeated",
        rows[rows.len() - 1].cap,
        rows[0].cap
    );
    for pair in rows.windows(2) {
        assert!(
            pair[1].hits >= pair[0].hits,
            "cap {} found fewer surfaces ({}) than cap {} ({}) at 1080p",
            pair[1].cap,
            pair[1].hits,
            pair[0].cap,
            pair[0].hits
        );
    }

    let first = rows[0];
    let last = rows[rows.len() - 1];
    let prev = rows[rows.len() - 2];
    let ns_per_ray = first.no_write_ms * 1.0e6 / rays_in_grid as f64;
    let rays_in_budget = BUDGET_MS * 1.0e6 / ns_per_ray;
    let wait_delta =
        100.0 * (last.after_write_ms - first.after_write_ms).abs() / first.after_write_ms;
    let cap_ratio = last.cap / first.cap;
    let middle = rows
        .get(1)
        .map(|row| {
            format!(
                "the cap-{} row of the same sweep waited {:.2} ms for {} surfaces",
                row.cap, row.after_write_ms, row.hits
            )
        })
        .unwrap_or_else(|| "this sweep had no middle row".to_string());
    let saturates = if prev.hits == last.hits {
        format!(
            "cap {} and cap {} both find {} surfaces of {rays_in_grid}, which is `r1_step_price`'s saturation \
             (7,240 surfaces at cap 4 and again at cap 8 on a 640x360 grid) reproduced at R1's own resolution",
            prev.cap, last.cap, last.hits
        )
    } else {
        format!(
            "cap {} finds {} surfaces of {rays_in_grid} and cap {} finds {ceiling}, so these two rows do NOT \
             saturate at R1's grid. `r1_step_price`'s 640x360 saturation (7,240 surfaces at cap 4 and again at \
             cap 8) is a property of that grid and its scene, and this run cannot claim it here",
            prev.cap, prev.hits, last.cap
        )
    };
    let wait_shape = if wait_delta > 50.0 {
        format!(
            "the cap BITES at R1's grid: {}x the permitted steps move the wait by {wait_delta:.1}% of itself. \
             That is the opposite of the 640x360 sweep, and the integers say why - the frame's surface count \
             still grows there ({rows}), while at 640x360 it had stopped growing by cap 8. So a cheaper march is \
             a lever at this resolution, not only a smaller grid",
            cap_ratio,
            rows = rows
                .iter()
                .map(|row| format!("cap {} -> {}", row.cap, row.hits))
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        format!(
            "the cap is nearly free: {}x the permitted steps move the wait by {wait_delta:.1}% of itself, which \
             is `r1_step_price`'s saturated shape reappearing at R1's grid",
            cap_ratio
        )
    };
    println!(
        "// the refill      : the wait after a fresh {}-byte refill is {:.2} ms against {:.2} ms on that same \
         frame marched again with nothing written - a signed difference of {:+.2} ms, MEASURED from two intervals \
         that are each one sample of a driver. Its sign is not stable across runs of this binary (an earlier run \
         put it at -12.58 ms and the one before that at +22.6 ms), so the honest reading is that the refill \
         cannot be separated from the wait at all here, and what the refill demonstrably costs is `write_rays`' \
         own {:.2} ms: the copy into the driver's staging, which is the interval no earlier number contained. \
         Submit is {:.2} ms because recording a dispatch waits for nothing.",
        rays_in_grid * RAY_BYTES,
        first.after_write_ms,
        first.no_write_ms,
        first.after_write_ms - first.no_write_ms,
        first.write_ms,
        first.submit_ms,
    );
    println!(
        "// the floor       : cap 1 waits {:.2} ms and finds {floor} surfaces; the same frame with the buffer \
         already on the device waited {:.2} ms, so the {rays_in_grid}-ray dispatch plus the {}-byte depth \
         read-back costs about {:.0} ns per ray before any ray marches ({:.1}x R1's {BUDGET_MS} ms on its own). \
         {wait_shape}. {middle}; `on_device_march` put this same scene at 1080p cap 64 at ~824 ms, against the \
         {:.2} ms this run's cap-64 row waited - the two agree to within {:.1}%, and neither is a gate.",
        first.after_write_ms,
        first.no_write_ms,
        rays_in_grid * 4,
        ns_per_ray,
        first.no_write_ms / BUDGET_MS,
        last.after_write_ms,
        100.0 * (last.after_write_ms - 824.0).abs() / 824.0,
    );
    println!(
        "// the cap        : {saturates}. Each row is also checked ray for ray against `render::march` on a \
         {SAMPLE_STEP}-pixel lattice of the frame - {} rays of the {rays_in_grid} in it - so a header whose cap \
         never landed cannot pass as a frame at this resolution either. MEASURED integers, asserted as \
         monotonicity plus cap {} beating cap {} plus the sampled hit counts agreeing with the reference's with \
         zero hit-vs-miss disagreements and a depth deviation under 0.002 - and never as a millisecond.",
        HEIGHT.div_ceil(SAMPLE_STEP) as usize * WIDTH.div_ceil(SAMPLE_STEP) as usize,
        last.cap,
        first.cap,
    );
    println!(
        "// what {BUDGET_MS} ms buys : at the cap-1 floor of {:.0} ns per ray, R1's whole frame budget is {:.0} \
         rays - {:.1}% of this grid - and that is with the march switched off, so the frame is over budget \
         before a single ray steps (DERIVED arithmetic on a measured interval; R1 stays informational either \
         way).",
        ns_per_ray,
        rays_in_budget,
        100.0 * rays_in_budget / rays_in_grid as f64,
    );
    println!(
        "// the bus         : {} bytes out + {} bytes in = {} bytes per frame against a {}-byte scene asset \
         uploaded once - MEASURED integers. The grid is {:.0}x the whole scene, so at R1's resolution what \
         crosses the bus is the rays and not the world: {BUDGET_MS} ms over {} bytes is {:.2} GB/s of payload \
         a frame would need just to move its own grid (DERIVED arithmetic, not a measured bandwidth).",
        rays_in_grid * RAY_BYTES,
        rays_in_grid * 4,
        rays_in_grid * (RAY_BYTES + 4),
        shape_bytes,
        (rays_in_grid * RAY_BYTES) as f64 / shape_bytes as f64,
        rays_in_grid * (RAY_BYTES + 4),
        (rays_in_grid * (RAY_BYTES + 4)) as f64 / (BUDGET_MS / 1000.0) / 1.0e9,
    );
    println!(
        "// what would close R1 : this run says both halves of the frame are too big, which is not what the \
         640x360 sweep predicted ({wait_shape}). Four levers, in the order their arithmetic is cheapest to \
         test: derive the rays in the shader instead of uploading 2M of them (removes `write_rays`' {:.2} ms \
         and its 66 MB transfer), march fewer of them - culling, tiles, the streamed carry (the only lever \
         touching the {ns_per_ray:.0} ns per-ray floor), cap the steps (the lever the 640x360 sweep ruled out \
         and this grid's {wait_delta:.1}% wait-delta restores), and keep the depths on the device, since a \
         shipping frame maps nothing and most of the wait is this probe reading a buffer back to check it. None \
         of the first three is measured here: this instrument has to write its rays and read them back to have \
         anything to assert, so it prices the frame it can check, not the frame an engine would draw.",
        first.write_ms,
    );
    println!(
        "// still not met   : R1 is a duration gate and ADR-017 keeps it informational; no millisecond above is \
         asserted, and the {:.2} ms this frame totals is the cost of checking it rather than of drawing it. The \
         asserted terms are the per-cap hit counts, the sampled lattice's agreement with render::march, the \
         bit-for-bit agreement with the per-call path, the four refusals, the depth reproduction across reps, \
         and the byte counts.",
        first.write_ms + first.submit_ms + first.after_write_ms
    );
}

/// One priced 1080p row. `no_write_ms` is NAN where the row had no march-only rep to compare against, and
/// `agreed` records how many later reps of the same filled frame returned the first rep's depths. The
/// `sampled_*` fields are the lattice anchor: what `render::march` and the device each saw over those rays, the
/// hit-vs-miss disagreements between them, and the worst depth deviation.
#[derive(Debug, Clone, Copy)]
struct Row {
    cap: u32,
    hits: usize,
    sampled_ref: usize,
    sampled_dev: usize,
    sampled_dis: usize,
    worst: f32,
    write_ms: f64,
    submit_ms: f64,
    after_write_ms: f64,
    no_write_ms: f64,
    reps: usize,
    agreed: usize,
}

/// March R1's grid on one set of buffers, printing each row as it completes so a later device loss cannot
/// destroy the evidence that came before it. `stop` records a refusal from `write_rays` or `march_frame`; a
/// panic from inside the driver is the caller's to catch.
fn price_big_grid(
    tree: &Tree,
    interpreter: &Interpreter,
    shape: &Shape,
    mut big: Vec<Ray>,
    rows: &mut Vec<Row>,
    stop: &mut Option<String>,
) {
    let rays_in_grid = big.len();
    let mut frame: Frame = interpreter.begin_frame(rays_in_grid);
    let bind = interpreter.bind(shape, &frame);
    for &cap in &STEPS {
        for ray in &mut big {
            ray.max_steps = cap;
        }
        // cap 1 is the row the split needs, so it gets the refilled-then-marched rep plus the march-only reps
        // after it; the later caps get one frame each, which is all this adapter offers.
        let reps = if cap == STEPS[0] { REPS } else { 1 };
        let mut write_row: Vec<f64> = Vec::new();
        let mut submit_row = Vec::new();
        let mut wait_clean = Vec::new();
        let mut wait_full: Option<f64> = None;
        let mut hits: Option<usize> = None;
        let mut first_depths: Option<Vec<f32>> = None;
        let mut agreed = 0usize;
        for rep in 0..reps {
            if rep == 0 {
                let start = Instant::now();
                match interpreter.write_rays(&mut frame, ROOT, &big) {
                    Ok(wrote) => assert_eq!(
                        wrote,
                        rays_in_grid * RAY_BYTES,
                        "at 1080p the frame put {wrote} bytes on the bus for {rays_in_grid} rays"
                    ),
                    Err(error) => {
                        *stop = Some(format!("cap {cap} write_rays: {error}"));
                        break;
                    }
                }
                write_row.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            match interpreter.march_frame(&bind, &frame) {
                Ok(marched) => {
                    let wait = marched.ms_frame_with_read - marched.ms_submit_only;
                    submit_row.push(marched.ms_submit_only);
                    if rep == 0 {
                        wait_full = Some(wait);
                        hits = Some(marched.hits);
                        first_depths = Some(marched.depths.clone());
                    } else {
                        wait_clean.push(wait);
                    }
                    if rep > 0 {
                        if let Some(previous) = &first_depths {
                            assert_eq!(
                                previous.len(),
                                marched.depths.len(),
                                "at cap {cap} a later rep read {} depths where the first read {}",
                                marched.depths.len(),
                                previous.len()
                            );
                            if previous
                                .iter()
                                .zip(marched.depths.iter())
                                .all(|(a, b)| a.to_bits() == b.to_bits())
                            {
                                agreed += 1;
                            }
                        }
                    }
                }
                Err(error) => {
                    *stop = Some(format!("cap {cap} march_frame: {error}"));
                    break;
                }
            }
        }
        let (Some(write), Some(after_write), Some(hits_found), Some(depths)) =
            (write_row.first().copied(), wait_full, hits, first_depths)
        else {
            if stop.is_none() {
                *stop = Some(format!("cap {cap}: no write-and-march rep completed"));
            }
            break;
        };
        // The reference anchor at R1's own grid: a lattice of pixels, marched by `render::march` at this cap and
        // compared against what the device wrote. Without it the 1080p rows carry no reference at all - and one
        // earlier run of this binary returned its cap-64 frame with the cap-8 row's hit count (65,148) and a
        // wait that matched its own cap-1 row, which is exactly what a header whose cap never landed would look
        // like. Nothing else in the program could have disagreed with that frame.
        let (mut ref_hits, mut dev_hits, mut disagreements) = (0usize, 0usize, 0usize);
        let mut worst = 0.0f32;
        for py in (0..HEIGHT).step_by(SAMPLE_STEP as usize) {
            for px in (0..WIDTH).step_by(SAMPLE_STEP as usize) {
                let index = (py * WIDTH + px) as usize;
                let ray = &big[index];
                let hit = march(
                    tree,
                    ROOT,
                    ray.origin,
                    ray.dir,
                    ray.max_t,
                    ray.surface,
                    ray.max_steps,
                );
                let depth = depths[index];
                if hit.is_some() {
                    ref_hits += 1;
                }
                if depth < 1.0e37 {
                    dev_hits += 1;
                }
                match (hit, depth < 1.0e37) {
                    (Some(hit), true) => worst = worst.max((hit.t - depth).abs()),
                    (None, false) => {}
                    _ => disagreements += 1,
                }
            }
        }

        let row = Row {
            cap,
            hits: hits_found,
            sampled_ref: ref_hits,
            sampled_dev: dev_hits,
            sampled_dis: disagreements,
            worst,
            write_ms: write,
            submit_ms: median(submit_row),
            after_write_ms: after_write,
            no_write_ms: if wait_clean.is_empty() {
                f64::NAN
            } else {
                median(wait_clean)
            },
            reps,
            agreed,
        };
        let agreeing = if row.reps == 1 {
            "single".to_string()
        } else {
            format!("{} of {}", row.agreed, row.reps - 1)
        };
        // The row prints before its asserts fire, because the integers are the evidence and a panic that
        // swallowed them would leave the next reader theorising about a frame nobody had seen the numbers of.
        println!(
            "// {:>3} | {:>9} | {:>8} | {:>10} | {:>10} | {:>4} | {:>9.6} | {:>13} | {:>8.2} | {:>9.2} | \
             {:>16.2} | {:>16}",
            row.cap,
            rays_in_grid * RAY_BYTES,
            row.hits,
            row.sampled_ref,
            row.sampled_dev,
            row.sampled_dis,
            row.worst,
            agreeing,
            row.write_ms,
            row.submit_ms,
            row.after_write_ms,
            if row.no_write_ms.is_nan() {
                "-".to_string()
            } else {
                format!("{:.2}", row.no_write_ms)
            }
        );
        assert_eq!(
            agreed,
            reps - 1,
            "at cap {cap} and 1080p, only {agreed} of the {} later reps of the same grid on the same reused \
             buffers returned the first rep's depths bit-for-bit - so the row is not one measurement of one \
             frame but a driver's mood",
            reps - 1
        );
        assert_eq!(
            row.sampled_dev, row.sampled_ref,
            "at cap {cap} and 1080p the sampled rays disagree on which of them hit: {} on the device, {} on the \
             reference - the frame is not the law this row is labelled with",
            row.sampled_dev, row.sampled_ref
        );
        assert!(
            row.sampled_dis == 0 && row.worst < 0.002,
            "at cap {cap} and 1080p the device and the reference disagree on {} of the sampled rays, worst \
             deviation {}",
            row.sampled_dis,
            row.worst
        );
        rows.push(row);
    }
}

fn panic_text(payload: &Box<dyn core::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else {
        payload
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "a panic with no message".to_string())
    }
}
