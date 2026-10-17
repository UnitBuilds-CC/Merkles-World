//! R1's lever, measured: generate the rays on the device and ask whether the picture is the same picture.
//!
//! `cargo run --release -p gpu --example r1_device_rays`
//!
//! `r1_step_price` priced the two halves of a 1080p frame and found the wrong-looking one: the step loop
//! saturates (cap 4 and cap 8 find the same surfaces; cap 4 to cap 64 moves 0.77% of the grid) while the host
//! round trip does not, so cap 1 — one step per ray, essentially no march — was already over R1's 16.67 ms in
//! all five of its runs. Most of that round trip is not the dispatch. It is the rays: 2,073,600 of them at
//! 32 bytes each, built by the host and pushed down the bus every frame, for a camera that fits in 112 bytes.
//!
//! So this example runs one frame twice. The first path is the shipped one: `begin_frame` / `write_rays` /
//! `bind` / `march_frame`, with every ray packed by this process. The second is `interp.wgsl`'s
//! `march_rays_from_camera`: the same header, the same scene buffers, the same `march_one`, and a
//! `CameraUniform` instead of a ray list, with each thread deriving its own ray from `gl_global_invocation_id`
//! using the arithmetic `render::raster::ray_for` uses. What is compared is the depth buffer they produce.
//!
//! The asserted terms are integers and bits: the host path's byte count, per-pixel hit/miss set equality
//! against both `render::march` and the host-ray device path, the worst depth deviation, and the fact that a
//! moved camera changes the picture. Durations are printed and are not evidence (ADR-017; gate R4 stays
//! informational), which matters more here than anywhere else in this repo — a faster frame that draws a
//! different frame is not a win, and the timing lines cannot be what says otherwise.
//!
//! The verdict, since the file is now the record of a hypothesis dying: **the rays were 66 MB of real bytes
//! and they were not R1's floor.** Deleting the copy changed the frame's hit set by nothing (asserted: 0 of
//! 2,073,600 disagree, 80,964 surfaces both sides) and changed its duration by nothing this machine can
//! resolve. The remaining terms are the march on the device and the 8 MB depth buffer read back per frame —
//! a renderer that shades on the device copies neither, and that leg is not built here.
//!
//! Two things this deliberately does *not* claim. It does not retire the host path: `render::raster` still
//! owns the cull, the enclosure bounds and the budget arithmetic, and a device-generated ray has no answer for
//! "which pixels did the cull already throw away" — the honest scope is a march whose rays are all wanted.
//! And it is presentation-only: doc 02's camera-relative `f32`s are what cross the wire here and nothing
//! hashes them, so AGENTS.md §4 and SPEC-02 are satisfied by the rays being *where to look*, not by anything
//! the simulation agrees about. The field itself is still evaluated by the same interpreter the CPU reference
//! agrees with, which is the point of the anchor rows below.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{GpuCamera, Interpreter};
use render::{march, Ray, Tree, Vec3, ROOT};

/// `on_device_march`'s and `r1_step_price`'s scene, word for word, so this compares against their numbers
/// rather than inventing a fourth fixture.
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
const HALF_HEIGHT: f32 = 0.5;
const MAX_STEPS: u32 = 64;
/// The anchor grid: small enough for `render::march` to walk it ray for ray in the same run.
const ANCHOR_W: u32 = 64;
const ANCHOR_H: u32 = 48;
/// The resolution R1 names. Measured here, once per path, because the claim being tested is about a frame at
/// this size and nothing smaller.
const R1_W: u32 = 1920;
const R1_H: u32 = 1080;
/// The width of one ray on the bus, in the shipped path. Stated as an integer so the ratio below is arithmetic
/// a reader can check rather than a figure this file chose.
const RAY_BYTES: usize = 32;

/// A camera aimed down +z from `origin`, in the frame `render::raster::ray_for` works in.
fn camera(width: u32, height: u32, origin: Vec3, half_height: f32) -> GpuCamera {
    GpuCamera {
        origin,
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        forward: [0.0, 0.0, 1.0],
        half_height,
        max_t: MAX_T,
        surface: SURFACE,
        max_steps: MAX_STEPS,
        root: ROOT,
        width,
        height,
    }
}

/// The host's side of the same pinhole, transcribed from `render::raster::ray_for` in the same operation
/// order. These are the rays the shipped path uploads; the device derives them from the camera block instead.
fn rays(width: u32, height: u32, origin: Vec3, half_height: f32) -> Vec<Ray> {
    let aspect = width as f32 / height as f32;
    let mut out = Vec::with_capacity((width * height) as usize);
    for py in 0..height {
        for px in 0..width {
            let u = (px as f32 + 0.5) / width as f32;
            let v = (py as f32 + 0.5) / height as f32;
            let ndc_x = (2.0 * u - 1.0) * half_height * aspect;
            let ndc_y = (1.0 - 2.0 * v) * half_height;
            let span = [ndc_x, ndc_y, 1.0];
            let length = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
            out.push(Ray {
                origin,
                dir: if length == 0.0 {
                    [0.0, 0.0, 0.0]
                } else {
                    [span[0] / length, span[1] / length, span[2] / length]
                },
                max_t: MAX_T,
                surface: SURFACE,
                max_steps: MAX_STEPS,
            });
        }
    }
    out
}

/// Hit/miss set agreement plus the worst depth deviation, as integers and bits.
///
/// Returns `(disagreements, worst_depth_deviation, both_hit)` where `both_hit` counts the pixels both sides
/// saw a surface at. A last-ulp difference in the ray direction is legal — the device may fuse the basis
/// combination into an FMA and the host may not — so depths are compared within a tolerance while the
/// hit/miss set is compared exactly, which is the anchor every other device example here already uses.
fn compare(a: &[f32], b: &[f32]) -> (usize, f32, usize) {
    let mut disagreements = 0usize;
    let mut worst = 0.0f32;
    let mut both_hit = 0usize;
    for (left, right) in a.iter().zip(b.iter()) {
        match (*left < 1.0e37, *right < 1.0e37) {
            (true, true) => {
                both_hit += 1;
                worst = worst.max((left - right).abs());
            }
            (false, false) => {}
            _ => disagreements += 1,
        }
    }
    (disagreements, worst, both_hit)
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP r1_device_rays: no device ({error:?}) - the question is whether a ray built on the GPU \
                 marches to the same depth as the identical ray built on the CPU, and a software rasteriser \
                 only ever executes the CPU side of that"
            );
            return;
        }
    };
    let tree = scene();
    let shape = interpreter.upload(&tree);

    println!("// adapter    : {}", interpreter.adapter());
    println!("// scene      : 3-node field, max_t {MAX_T}, surface {SURFACE}, cap {MAX_STEPS}, root index {ROOT}");
    println!("// paths      : host rays ({RAY_BYTES} B each, packed here) vs one 16 + 96-byte camera block");
    println!("// asserted   : byte counts, per-pixel hit/miss sets, depth deviation, that a moved camera moves the picture");
    println!("// printed    : milliseconds only - R1 is a duration gate and ADR-017 keeps it informational");
    println!();

    // ---- The anchor: 64x48, device rays against the CPU reference, ray for ray ----------------------
    //
    // The camera path is checked against `render::march` first and only then against the host-ray path. The
    // order matters: two device paths agreeing proves nothing if both are wrong in the same way, and the
    // reference is the one implementation this repo has tests for.
    let origin = [0.0f32, 0.0, -6.0];
    let anchor_rays = rays(ANCHOR_W, ANCHOR_H, origin, HALF_HEIGHT);
    let anchor_camera = camera(ANCHOR_W, ANCHOR_H, origin, HALF_HEIGHT);
    let mut anchor_frame = interpreter.begin_camera_frame(ANCHOR_W, ANCHOR_H);
    let anchor_written = interpreter.write_camera(&mut anchor_frame, &anchor_camera);
    let anchor_bind = interpreter.bind_camera(&shape, &anchor_frame);
    let on_device = interpreter
        .march_camera_frame(&anchor_bind, &anchor_frame)
        .expect("the anchor grid must march on the camera path");

    let mut reference_hits = 0usize;
    let mut against_reference = 0usize;
    let mut worst_vs_reference = 0.0f32;
    for (ray, depth) in anchor_rays.iter().zip(on_device.depths.iter()) {
        let hit = march(
            &tree,
            ROOT,
            ray.origin,
            ray.dir,
            ray.max_t,
            ray.surface,
            ray.max_steps,
        );
        if hit.is_some() {
            reference_hits += 1;
        }
        match (hit, *depth < 1.0e37) {
            (Some(hit), true) => {
                worst_vs_reference = worst_vs_reference.max((hit.t - *depth).abs())
            }
            (None, false) => {}
            _ => against_reference += 1,
        }
    }
    assert_eq!(
        on_device.hits, reference_hits,
        "the camera path found {} surfaces and render::march found {reference_hits} - the rays the device \
         generates are not the rays the host generates, so nothing below compares the same frame twice",
        on_device.hits
    );
    assert_eq!(
        against_reference, 0,
        "{against_reference} pixels disagree on hit-vs-miss between the device-generated rays and the CPU \
         reference, which is the fork this example exists to detect"
    );
    assert!(
        worst_vs_reference < 0.002,
        "worst |device - reference| depth is {worst_vs_reference}, so the march differs by more than a step's \
         rounding and the paths are not running one law"
    );

    // ---- The same frame on the shipped path: what the comparison is against, and what it costs -------
    let mut host_frame = interpreter.begin_frame(anchor_rays.len());
    let host_written = interpreter
        .write_rays(&mut host_frame, ROOT, &anchor_rays)
        .expect("the anchor grid must load into a frame");
    let host_bind = interpreter.bind(&shape, &host_frame);
    let on_host = interpreter
        .march_frame(&host_bind, &host_frame)
        .expect("the anchor grid must march on the host path");
    let (set_diffs, worst_vs_host, shared) = compare(&on_host.depths, &on_device.depths);

    assert_eq!(
        host_written,
        anchor_rays.len() * RAY_BYTES,
        "the host path reported {host_written} bytes for {} rays, so the 32 B/ray figure the whole argument \
         rests on is not what that call writes",
        anchor_rays.len()
    );
    assert_eq!(
        anchor_written, 112,
        "the camera path reported {anchor_written} bytes; the uniform layout this file and `interp.wgsl` agree \
         on is a 16-byte header plus a 96-byte camera block, so a change here is a change to that contract"
    );
    assert_eq!(
        set_diffs, 0,
        "{set_diffs} pixels disagree on hit-vs-miss between the two device paths at the same camera, so the \
         device-generated rays are not the host-generated rays"
    );
    assert!(
        worst_vs_host < 0.002,
        "worst |host rays - camera rays| depth is {worst_vs_host} over {shared} shared surfaces"
    );

    println!(
        "// anchor grid  : {ANCHOR_W}x{ANCHOR_H} = {} pixels",
        on_device.rays
    );
    println!(
        "//   reference hits / device hits : {reference_hits} / {}",
        on_device.hits
    );
    println!("//   host-path hits               : {}", on_host.hits);
    println!("//   hit/miss set differences     : {set_diffs} host-vs-device, {against_reference} device-vs-reference");
    println!("//   worst depth deviation        : {worst_vs_host:.6} host-vs-device, {worst_vs_reference:.6} device-vs-reference");
    println!("//   bytes to the device          : {host_written} host rays, {anchor_written} camera block (DERIVED: {:.1}x less)", host_written as f64 / anchor_written as f64);
    println!(
        "//   ms (printed, not evidence)   : {:.2} host frame, {:.2} camera frame",
        on_host.ms_frame_with_read, on_device.ms_frame_with_read
    );
    println!();

    // ---- The check is sensitive: one camera in, two cameras out ------------------------------------
    //
    // A comparison that always passes is worth nothing, so the harness is falsified in the same run that uses
    // it. The host rays are rebuilt at half_height 0.25 while the device keeps 0.5 — a zoom mismatch the two
    // paths would have to be blind to in order to agree.
    let split_rays = rays(ANCHOR_W, ANCHOR_H, origin, 0.25);
    let mut reference_split = Vec::with_capacity(split_rays.len());
    for ray in &split_rays {
        reference_split.push(
            march(
                &tree,
                ROOT,
                ray.origin,
                ray.dir,
                ray.max_t,
                ray.surface,
                ray.max_steps,
            )
            .map(|hit| hit.t)
            .unwrap_or(1.0e37),
        );
    }
    let (blind, _, _) = compare(&reference_split, &on_device.depths);
    assert!(
        blind > 0,
        "a frame marched with half_height 0.25 is indistinguishable from the device's 0.5 frame at \
         {ANCHOR_W}x{ANCHOR_H}, which would mean this comparison cannot see a camera at all"
    );
    println!("// falsified    : the same comparison, given the host a half_height of 0.25 against the device's");
    println!("//              0.5, reports {blind} hit/miss differences - the check sees a camera, so the zero");
    println!("//              above is agreement and not a comparison that cannot fail.");
    println!();

    // ---- The uniform is actually read: move the camera, rewrite 112 bytes, look at the picture -------
    let moved = [0.0f32, 0.0, -2.5];
    let moved_written = interpreter.write_camera(
        &mut anchor_frame,
        &camera(ANCHOR_W, ANCHOR_H, moved, HALF_HEIGHT),
    );
    let moved_on_device = interpreter
        .march_camera_frame(&anchor_bind, &anchor_frame)
        .expect("the moved camera must march");
    let moved_host = interpreter
        .march(&tree, ROOT, &rays(ANCHOR_W, ANCHOR_H, moved, HALF_HEIGHT))
        .expect("the moved camera must march on the one-shot path too");
    let (moved_diffs, moved_worst, _) = compare(&moved_host.depths, &moved_on_device.depths);
    let (moved_picture, _, _) = compare(&on_device.depths, &moved_on_device.depths);
    assert_eq!(
        moved_diffs, 0,
        "{moved_diffs} pixels disagree after the camera moved, so the two paths disagree about a camera that \
         only one of them was told about"
    );
    assert!(
        moved_picture > 0,
        "moving the camera 3.5 units closer left the device's picture byte-identical: the shader is not \
         reading the camera block, and every agreement above is two paths ignoring the same input"
    );
    assert!(
        moved_worst < 0.002,
        "worst deviation after the move is {moved_worst}"
    );
    println!("// moved camera : {moved_picture} pixels changed, {moved_written} bytes rewritten, {moved_diffs} disagreements,");
    println!(
        "//              worst {moved_worst:.6} - the camera block is an input, not set dressing."
    );

    // ---- The zero-length arm ---------------------------------------------------------------------
    //
    // `ray_for`'s `unit` returns a zero vector for a zero-length span and the shader reaches the same answer
    // through `select`. Neither is exercised by a real camera, so a degenerate one is fed to both: every ray
    // is [0,0,0], the march cannot advance, and both paths must say "no surface" for all 3,072 pixels.
    let degenerate = GpuCamera {
        origin,
        right: [0.0, 0.0, 0.0],
        up: [0.0, 0.0, 0.0],
        forward: [0.0, 0.0, 0.0],
        half_height: HALF_HEIGHT,
        max_t: MAX_T,
        surface: SURFACE,
        max_steps: MAX_STEPS,
        root: ROOT,
        width: ANCHOR_W,
        height: ANCHOR_H,
    };
    let mut degenerate_frame = interpreter.begin_camera_frame(ANCHOR_W, ANCHOR_H);
    interpreter.write_camera(&mut degenerate_frame, &degenerate);
    let degenerate_bind = interpreter.bind_camera(&shape, &degenerate_frame);
    let degenerate_on_device = interpreter
        .march_camera_frame(&degenerate_bind, &degenerate_frame)
        .expect("the degenerate camera must march");
    assert_eq!(
        degenerate_on_device.hits, 0,
        "a frame of zero-length rays found {} surfaces; the shader's normalisation is not the host's",
        degenerate_on_device.hits
    );
    let degenerate_surfaces = degenerate_on_device.hits;
    println!("// zero rays    : {ANCHOR_W}x{ANCHOR_H} zero-length rays, {degenerate_surfaces} surfaces found, 0 expected - the");
    println!("//              `select` arm matches `render`'s `unit`.");
    println!();

    // ---- R1's grid -----------------------------------------------------------------------------
    //
    // The claim being tested is about 1080p, so it is measured there. Host path first and its buffers dropped
    // before the camera frame is created: `r1_step_price` lost the device at this resolution when a fresh
    // 66 MB ray buffer was allocated per march, so the frames do not coexist.
    // And a loss at this resolution is not an `Err`: wgpu 27 raises a *panic* from inside `Device::poll`, which
    // is the pathology `r1_round_trip_split` records (one of its attempts printed a row and then lost the
    // device), so the leg runs under `catch_unwind` and only a payload naming a lost device is treated as a
    // skip. Anything else that unwinds here exits with the panic's own text, because swallowing an assertion
    // failure into a friendly "SKIPPED" line would be the worst possible way to report one.
    let leg = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| {
        let r1_origin = [0.0f32, 0.0, -6.0];
        let pixels = (R1_W as usize) * (R1_H as usize);
        let r1_rays = rays(R1_W, R1_H, r1_origin, HALF_HEIGHT);
        let mut big_host = interpreter.begin_frame(pixels);
        let big_written = interpreter
            .write_rays(&mut big_host, ROOT, &r1_rays)
            .expect("1080p must load into a frame");
        let big_bind = interpreter.bind(&shape, &big_host);
        let big_on_host = interpreter
            .march_frame(&big_bind, &big_host)
            .expect("1080p must march on the host path");
        drop(big_bind);
        drop(big_host);
        drop(r1_rays);

        let mut big_camera = interpreter.begin_camera_frame(R1_W, R1_H);
        let big_camera_written =
            interpreter.write_camera(&mut big_camera, &camera(R1_W, R1_H, r1_origin, HALF_HEIGHT));
        let big_cam_bind = interpreter.bind_camera(&shape, &big_camera);
        let big_on_device = interpreter
            .march_camera_frame(&big_cam_bind, &big_camera)
            .expect("1080p must march on the camera path");
        let (big_diffs, big_worst, big_shared) =
            compare(&big_on_host.depths, &big_on_device.depths);
        (
            pixels,
            big_written,
            big_camera_written,
            big_on_host,
            big_on_device,
            big_diffs,
            big_worst,
            big_shared,
        )
    }));
    let (
        pixels,
        big_written,
        big_camera_written,
        big_on_host,
        big_on_device,
        big_diffs,
        big_worst,
        big_shared,
    ) = match leg {
        Ok(leg) => leg,
        Err(payload) => {
            let text = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|held| held.to_string()))
                .unwrap_or_else(|| "a panic with no message".to_string());
            if !(text.contains("device is lost") || text.contains("Validation")) {
                println!("// 1080p        : UNEXPECTED PANIC, not a lost device, so it is a failure: {text}");
                std::process::exit(101);
            }
            println!(
                "// 1080p        : SKIPPED - this box lost the device mid-leg ({text}); the anchor"
            );
            println!("//              claims above - reference, both paths, the plant, the move, the zero arm - stand");
            return;
        }
    };

    assert_eq!(
        big_written,
        pixels * RAY_BYTES,
        "1080p host rays are {big_written} bytes, not {pixels} x {RAY_BYTES}"
    );
    assert_eq!(
        big_on_host.hits, big_on_device.hits,
        "at 1080p the host path finds {} surfaces and the camera path {}",
        big_on_host.hits, big_on_device.hits
    );
    assert_eq!(
        big_diffs, 0,
        "at 1080p {big_diffs} of {pixels} pixels disagree on hit-vs-miss between the two paths"
    );
    assert!(
        big_worst < 0.002,
        "at 1080p the worst depth deviation is {big_worst} over {big_shared} shared surfaces"
    );

    println!("// 1080p        : {R1_W}x{R1_H} = {pixels} pixels, one frame per path");
    println!("//   hits       : {} host, {} device, {big_diffs} set differences, worst {big_worst:.6} over {big_shared} surfaces", big_on_host.hits, big_on_device.hits);
    println!(
        "//   bytes out  : {big_written} (rays + the same 16-byte header) vs {big_camera_written}"
    );
    println!("//   ms         : {:.2} host frame with read, {:.2} camera frame with read - PRINTED, not claimed", big_on_host.ms_frame_with_read, big_on_device.ms_frame_with_read);
    println!(
        "//   ms submit  : {:.2} vs {:.2} - the dispatch and copy-back, before the read-back",
        big_on_host.ms_submit_only, big_on_device.ms_submit_only
    );
    println!();
    println!("// What this settles, and what it does not:");
    let shrink = big_written / big_camera_written;
    println!("// - the round trip shrinks by a factor of {shrink} at 1080p and the picture does not move. That is the");
    println!(
        "//   integer statement of the lever: {big_written} bytes become {big_camera_written}, for the same hit set."
    );
    println!("// - whether that buys R1's 16.67 ms: NO, and this is where the hypothesis this example was written");
    println!("//   to test died. Three completed runs put the 1080p pair (host/camera, each with its read-back) at");
    println!("//   893.00/967.56, 832.59/827.18 and 822.83/799.37 ms - the camera path is 74 ms *slower* in one run");
    println!("//   and 5 and 23 ms *faster* in the other two, so the sign of the difference is not a reproducible");
    println!("//   quantity - and the same 3,072-ray anchor frame on this machine timed 1.95, 1.82, 18.91 and");
    println!("//   1.91 ms, so this box cannot resolve a difference of the size the ray copy should have been.");
    println!("//   The stable timing term is submit-only, 0.11-0.50 ms across those runs on both paths, and it");
    println!("//   says the dispatch was never where the frame went either.");
    println!("// - so the 66 MB is real bytes that are not the floor. `r1_round_trip_split` already priced");
    println!("//   `write_rays` itself at 66-74 ms per 1080p frame, which is what this path deletes, and");
    println!("//   deleting it changed nothing measurable - which retracts the claim that the host-to-device");
    println!("//   copy is R1's fixed term. What is left in the frame is the march's own device time and the");
    println!("//   8,294,400-byte depth buffer read back per frame; a renderer that shaded on the device would");
    println!("//   copy neither, and that leg is not built here.");
    println!("// - the host path is not retired. `render::raster`'s cull, enclosure bounds and per-ray budget");
    println!("//   charge all need to know which pixels they threw away, and a device that derives its own");
    println!("//   rays does not answer that. The path this example measures is the one where every pixel in");
    println!("//   the grid is wanted - the demo's full-frame march - and the two coexist.");
    println!(
        "// - SPEC-02 holds by construction, not by measurement: the camera block is doc 02's"
    );
    println!(
        "//   presentation `f32`, nothing hashed reads it, and the field the rays walk is the same"
    );
    println!("//   `march_one` over the same interpreter the CPU reference already agrees with. The assertion");
    println!(
        "//   that guards it is the reference anchor above, which would fire if the device started"
    );
    println!("//   evaluating a different shape while looking entirely correct.");
}
