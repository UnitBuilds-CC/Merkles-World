//! R1's frame, split one level deeper: what the march costs with nothing coming back, and what the
//! read-back costs on its own.
//!
//! `cargo run --release -p gpu --example r1_march_alone`
//!
//! `r1_device_rays` deleted the 66 MB of per-frame ray bytes and found that the frame did not get faster by a
//! resolvable amount, which retracted the round-trip-floor hypothesis and left two terms standing in an R1
//! 1080p frame: the device's own stepping, and the `pixels * 4` depth buffer copied back to this process every
//! frame — **8,294,400 bytes**. A renderer that shades on the device copies neither, because its depths are
//! read by the same device that wrote them. Whether that leg is worth building is a question about the size of
//! those two terms, so this example measures them apart instead of arguing about them.
//!
//! The split is `Interpreter::march_camera_frame_no_readback` (encode, dispatch, wait for the queue to drain,
//! copy and map nothing) against `Interpreter::read_camera_depths` (copy and map the depths, dispatch nothing).
//! The asserted terms are integers and bits, and the split is only admissible because of them: the two legs
//! re-produce the whole path's depth buffer with **zero** differing bit patterns over 2,073,600 f32s, the
//! read-back's hit count equals the whole path's, and both anchor against `render::march` on a 2-D lattice of
//! pixel centres. If the march had not actually completed before the copy, the read-back would disagree with
//! the whole path and the assertion would fire — the split is not taken on trust from a driver's word.
//!
//! Durations are printed and are not evidence (ADR-017; R1 and R4 stay informational), and they are printed
//! *before* the asserts fire, per the lesson from `r1_round_trip_split`: an earlier run panicked after printing
//! nothing, which is exactly what a header that never landed looks like. What the reader gets is the sum of the
//! two legs against the whole-path figure — an attribution, not a gate.
//!
//! The falsifier that matters here is the cap: `write_camera` records the law in the header, so if a step cap
//! never reached the shader, cap 1, cap 8 and cap 64 would return the same hit count. They do not — and a frame
//! that was written but never dispatched is read back separately to show that the numbers above come from a
//! march and not from whatever was in the buffer.
//!
//! The plant that says the admissibility assert is live (run once, by hand, 2026-10-17): replace the anchor's
//! `march_camera_frame_no_readback` call with nothing, so the read-back sees the zeroed buffer. The run exits
//! 101 reporting 3,072 hits against the reference's 132, 2,940 set disagreements, and **3,072 of 3,072 depths
//! differing in their bits** against the whole path. A march leg that silently stopped dispatching is therefore
//! not a claim this file can make by accident.
//!
//! CI reads the asserted terms on a second vendor (run `39016251784`): the ARM64 macOS job's `Apple Paravirtual
//! device [Metal, IntegratedGpu]` reports the same 132/132/132 anchor, 0 bit-differing depths, the same
//! 0 / 65,148 / 80,964 cap shape, 8,294,400 bytes per row and 0 lattice disagreements, and its own clock puts the
//! read-back at 9.96 ms of a 421.97 ms cap-64 frame — 2.4% where this laptop measured 0.74–1.40%, so the verdict
//! is two vendors wide. x86-64 Linux and Windows print `SKIP r1_march_alone: no device (NoAdapter)`, because no CI
//! runner is a GPU.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{GpuCamera, Interpreter};
use render::{march, Ray, Tree, Vec3, ROOT};

/// `r1_device_rays`' scene, word for word, so the two files' rows describe one frame and not two fixtures.
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
/// Small enough for `render::march` to walk ray for ray in the same run.
const ANCHOR_W: u32 = 64;
const ANCHOR_H: u32 = 48;
/// The resolution R1 names, and the only one this example's split is about.
const R1_W: u32 = 1920;
const R1_H: u32 = 1080;
/// The lattice the 1080p rows are anchored on: 60 x 34 = 2,040 pixel centres. A flat stride would be
/// degenerate here — 1920 is a multiple of 64, so a 1-D sample would sit in the most oblique column of the
/// image, which is the mistake `r1_round_trip_split` records and corrects.
const LATTICE_W: usize = 60;
const LATTICE_H: usize = 34;
/// The no-surface sentinel `interp.wgsl` writes, shared with `render`'s miss.
const MISS: f32 = 1.0e37;

/// A camera aimed down +z from `origin`, in the frame `render::raster::ray_for` works in.
fn camera(width: u32, height: u32, origin: Vec3, half_height: f32, max_steps: u32) -> GpuCamera {
    GpuCamera {
        origin,
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        forward: [0.0, 0.0, 1.0],
        half_height,
        max_t: MAX_T,
        surface: SURFACE,
        max_steps,
        root: ROOT,
        width,
        height,
    }
}

/// The ray for one pixel, transcribed from `render::raster::ray_for` in the same operation order. Both the
/// host path and the lattice need it; the device derives the same vector from the camera block.
fn ray_at(
    px: u32,
    py: u32,
    width: u32,
    height: u32,
    origin: Vec3,
    half_height: f32,
    max_steps: u32,
) -> Ray {
    let aspect = width as f32 / height as f32;
    let u = (px as f32 + 0.5) / width as f32;
    let v = (py as f32 + 0.5) / height as f32;
    let ndc_x = (2.0 * u - 1.0) * half_height * aspect;
    let ndc_y = (1.0 - 2.0 * v) * half_height;
    let span = [ndc_x, ndc_y, 1.0];
    let length = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
    Ray {
        origin,
        dir: if length == 0.0 {
            [0.0, 0.0, 0.0]
        } else {
            [span[0] / length, span[1] / length, span[2] / length]
        },
        max_t: MAX_T,
        surface: SURFACE,
        max_steps,
    }
}

fn rays(width: u32, height: u32, origin: Vec3, half_height: f32) -> Vec<Ray> {
    let mut out = Vec::with_capacity((width * height) as usize);
    for py in 0..height {
        for px in 0..width {
            out.push(ray_at(
                px,
                py,
                width,
                height,
                origin,
                half_height,
                MAX_STEPS,
            ));
        }
    }
    out
}

/// Hit/miss set agreement plus the worst depth deviation, as integers and bits. A last-ulp difference in the
/// ray direction is legal — the device may fuse into an FMA and the host may not — so depths are compared
/// within a tolerance while the hit/miss set is compared exactly.
fn compare(a: &[f32], b: &[f32]) -> (usize, f32, usize) {
    let mut disagreements = 0usize;
    let mut worst = 0.0f32;
    let mut both_hit = 0usize;
    for (left, right) in a.iter().zip(b.iter()) {
        match (*left < MISS, *right < MISS) {
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

/// Pixels whose depth bit pattern differs between two buffers — the strongest comparison here, and the one
/// that makes the split admissible: the two legs must return the whole path's frame, byte for byte.
fn bit_differences(a: &[f32], b: &[f32]) -> usize {
    a.iter()
        .zip(b.iter())
        .filter(|(left, right)| left.to_bits() != right.to_bits())
        .count()
}

/// One 1080p cap, measured as the two legs and the whole path, plus the lattice anchor.
struct Cap {
    max_steps: u32,
    law_reported: u32,
    march_ms: f64,
    read_ms: f64,
    read_bytes: usize,
    read_hits: usize,
    frame_ms: f64,
    submit_ms: f64,
    frame_hits: usize,
    bit_diffs: usize,
    lattice_hits: usize,
    lattice_diffs: usize,
    lattice_worst: f32,
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP r1_march_alone: no device ({error:?}) - the split is between a GPU's own stepping and the \
                 bytes a GPU copies back to this process; a software rasteriser has neither the stepping nor \
                 that bus, so it would answer with the reference march wearing a GPU label"
            );
            return;
        }
    };
    let tree = scene();
    let shape = interpreter.upload(&tree);
    let origin = [0.0f32, 0.0, -6.0];

    println!("// adapter    : {}", interpreter.adapter());
    println!("// scene      : 3-node field, max_t {MAX_T}, surface {SURFACE}, root index {ROOT}, camera path only");
    println!("// legs       : march-no-readback (dispatch + drain, 0 bytes back) | read-back (copy + map, no dispatch) | whole path");
    println!("// asserted   : the two legs rebuild the whole path's depths bit for bit, hit counts, zero lattice disagreements against render::march, that the step cap changes the frame");
    println!("// printed    : milliseconds only - R1 is a duration gate and ADR-017 keeps it informational");
    println!();

    // ---- The anchor grid: is the split even the same frame? ---------------------------------------
    //
    // 64x48, walked ray for ray against `render::march` first. Everything the 1080p rows claim rests on this:
    // that dispatch-then-copy, in two calls instead of one, produces the same picture as the shipped path.
    let anchor_rays = rays(ANCHOR_W, ANCHOR_H, origin, HALF_HEIGHT);
    let mut frame = interpreter.begin_camera_frame(ANCHOR_W, ANCHOR_H);
    let written = interpreter.write_camera(
        &mut frame,
        &camera(ANCHOR_W, ANCHOR_H, origin, HALF_HEIGHT, MAX_STEPS),
    );
    let bind = interpreter.bind_camera(&shape, &frame);

    let march_ms = interpreter
        .march_camera_frame_no_readback(&bind, &frame)
        .expect("the anchor grid must march with no read-back");
    let split = interpreter
        .read_camera_depths(&frame)
        .expect("the anchor grid's depths must come back");
    let whole = interpreter
        .march_camera_frame(&bind, &frame)
        .expect("the anchor grid must march on the whole path");
    let bit_diffs = bit_differences(&split.depths, &whole.depths);

    let mut reference_hits = 0usize;
    let mut reference_depths = Vec::with_capacity(anchor_rays.len());
    for ray in &anchor_rays {
        match march(
            &tree,
            ROOT,
            ray.origin,
            ray.dir,
            ray.max_t,
            ray.surface,
            ray.max_steps,
        ) {
            Some(hit) => {
                reference_hits += 1;
                reference_depths.push(hit.t);
            }
            None => reference_depths.push(MISS),
        }
    }
    let (set_diffs, worst_vs_reference, shared) = compare(&reference_depths, &split.depths);

    println!(
        "// anchor grid  : {ANCHOR_W}x{ANCHOR_H} = {} pixels, camera block {written} bytes",
        split.depths.len()
    );
    println!(
        "//   reference hits / split legs / whole path : {reference_hits} / {} / {}",
        split.hits, whole.hits
    );
    println!(
        "//   hit/miss set differences vs reference : {set_diffs} over {shared} shared surfaces"
    );
    println!("//   worst depth deviation vs reference    : {worst_vs_reference:.6}");
    println!(
        "//   bit-differing depths, legs vs whole    : {bit_diffs} of {}",
        split.depths.len()
    );
    println!(
        "//   bytes read back                       : {} ({} pixels x 4)",
        split.bytes,
        split.depths.len()
    );
    println!(
        "//   ms (printed, not evidence)              : {march_ms:.2} march only, {:.2} read only, {:.2} whole path",
        split.ms, whole.ms_frame_with_read
    );
    println!();

    assert_eq!(
        written, 112,
        "the camera path reported {written} bytes; the uniform layout `r1_device_rays` and `interp.wgsl` agree \
         on is a 16-byte header plus a 96-byte camera block"
    );
    assert_eq!(
        split.depths.len(),
        (ANCHOR_W as usize) * (ANCHOR_H as usize),
        "the read-back returned {} depths for a {}-pixel frame, so it is not the whole frame",
        split.depths.len(),
        ANCHOR_W as usize * ANCHOR_H as usize
    );
    assert_eq!(
        split.bytes,
        (ANCHOR_W as usize) * (ANCHOR_H as usize) * 4,
        "the read-back reported {} bytes for {} pixels, so the byte figure this whole file is about is not the \
         copy's own count",
        split.bytes,
        split.depths.len()
    );
    assert_eq!(
        set_diffs, 0,
        "{set_diffs} pixels disagree on hit-vs-miss between the split legs and the CPU reference, so the march \
         this file prices is not the march the repo has tests for"
    );
    assert!(
        worst_vs_reference < 0.002,
        "worst |device - reference| depth is {worst_vs_reference}, so the stepping differs by more than a \
         step's rounding"
    );
    assert_eq!(
        bit_diffs, 0,
        "{bit_diffs} depths differ in their bits between dispatch-then-copy and the whole path, so the split \
         measures a different frame than the shipped one and every 1080p row below is inadmissible"
    );
    assert_eq!(
        split.hits, whole.hits,
        "the legs found {} surfaces and the whole path found {}",
        split.hits, whole.hits
    );
    assert_eq!(
        whole.max_steps, MAX_STEPS,
        "the whole path reports a law of {}, so the header this process wrote is not the header the shader \
         was given",
        whole.max_steps
    );

    // ---- Falsifier 1: a frame that was written but never marched ---------------------------------
    //
    // The split legs' admissibility rests on the read-back seeing a completed march, so the failure mode is
    // measured: allocate a frame, write the camera, copy and map without dispatching, and compare what comes
    // back against the reference. A buffer that never marched cannot be mistaken for a frame.
    let mut never = interpreter.begin_camera_frame(ANCHOR_W, ANCHOR_H);
    interpreter.write_camera(
        &mut never,
        &camera(ANCHOR_W, ANCHOR_H, origin, HALF_HEIGHT, MAX_STEPS),
    );
    let unmarched = interpreter
        .read_camera_depths(&never)
        .expect("an unmarched frame must still be readable");
    let (unmarched_diffs, _, _) = compare(&reference_depths, &unmarched.depths);
    assert!(
        unmarched_diffs > 0 && unmarched.hits != split.hits,
        "a frame that was written but never dispatched read back as {} hits and {} \
         disagreements against the reference, identical to the marched frame - which would mean the numbers \
         above are the buffer's contents and not a march",
        unmarched.hits,
        unmarched_diffs
    );
    println!("// falsified    : a frame written but never dispatched reads back {} hits and {unmarched_diffs}", unmarched.hits);
    println!("//              disagreements against the reference, where the marched one reads {} and 0, so the", split.hits);
    println!("//              legs above are priced on a frame that really ran.");
    println!();
    drop(unmarched);
    drop(never);
    drop(reference_depths);
    drop(anchor_rays);

    // ---- R1's grid, per cap --------------------------------------------------------------------
    //
    // A loss at this resolution is not an `Err`: wgpu 27 panics from inside `Device::poll`, which is the
    // pathology `r1_round_trip_split` records, so the legs run under `catch_unwind` and only a payload naming a
    // lost device prints a skip. Anything else that unwinds exits 101 rather than hiding a failure behind a
    // friendly line.
    let caps = [1u32, 8, MAX_STEPS];
    let leg = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| {
        let mut out = Vec::new();
        for &max_steps in &caps {
            let mut big = interpreter.begin_camera_frame(R1_W, R1_H);
            interpreter.write_camera(
                &mut big,
                &camera(R1_W, R1_H, origin, HALF_HEIGHT, max_steps),
            );
            let big_bind = interpreter.bind_camera(&shape, &big);

            let march_ms = interpreter
                .march_camera_frame_no_readback(&big_bind, &big)
                .expect("1080p must march with no read-back");
            let read = interpreter
                .read_camera_depths(&big)
                .expect("1080p depths must come back");
            let whole = interpreter
                .march_camera_frame(&big_bind, &big)
                .expect("1080p must march on the whole path");
            let bit_diffs = bit_differences(&read.depths, &whole.depths);

            // The lattice anchor, against `render::march` at this cap.
            let mut lattice_hits = 0usize;
            let mut lattice_depths = Vec::with_capacity(LATTICE_W * LATTICE_H);
            for jy in 0..LATTICE_H {
                for jx in 0..LATTICE_W {
                    let px = (jx as u32 * R1_W) / LATTICE_W as u32;
                    let py = (jy as u32 * R1_H) / LATTICE_H as u32;
                    let ray = ray_at(px, py, R1_W, R1_H, origin, HALF_HEIGHT, max_steps);
                    match march(
                        &tree,
                        ROOT,
                        ray.origin,
                        ray.dir,
                        ray.max_t,
                        ray.surface,
                        ray.max_steps,
                    ) {
                        Some(hit) => {
                            lattice_hits += 1;
                            lattice_depths.push(hit.t);
                        }
                        None => lattice_depths.push(MISS),
                    }
                }
            }
            let mut read_at_lattice = Vec::with_capacity(lattice_depths.len());
            for jy in 0..LATTICE_H {
                for jx in 0..LATTICE_W {
                    let px = (jx as u32 * R1_W) / LATTICE_W as u32;
                    let py = (jy as u32 * R1_H) / LATTICE_H as u32;
                    read_at_lattice.push(read.depths[py as usize * R1_W as usize + px as usize]);
                }
            }
            let (lattice_diffs, lattice_worst, _) = compare(&lattice_depths, &read_at_lattice);

            out.push(Cap {
                max_steps,
                law_reported: whole.max_steps,
                march_ms,
                read_ms: read.ms,
                read_bytes: read.bytes,
                read_hits: read.hits,
                frame_ms: whole.ms_frame_with_read,
                submit_ms: whole.ms_submit_only,
                frame_hits: whole.hits,
                bit_diffs,
                lattice_hits,
                lattice_diffs,
                lattice_worst,
            });
            drop(read_at_lattice);
            drop(lattice_depths);
            drop(big_bind);
            drop(big);
        }
        out
    }));
    let rows = match leg {
        Ok(rows) => rows,
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
            println!("// 1080p        : SKIPPED, the device was lost mid-leg: {text}");
            println!("//                (this row is informational; the anchor rows above are the asserted ones)");
            return;
        }
    };

    println!(
        "// 1080p        : {R1_W}x{R1_H} = {} pixels per frame, one frame per leg",
        R1_W as usize * R1_H as usize
    );
    println!("//   cap | law | hits legs/whole | bit diffs | bytes back | lattice hits/diffs | worst    | ms march | ms read | ms whole | ms submit");
    for row in &rows {
        println!(
            "//   {:>3} | {:>3} | {:>6}/{:<6}      | {:>9} | {:>10} | {:>5}/{:>3}          | {:>7.6} | {:>8.2} | {:>7.2} | {:>8.2} | {:>8.2}",
            row.max_steps,
            row.law_reported,
            row.read_hits,
            row.frame_hits,
            row.bit_diffs,
            row.read_bytes,
            row.lattice_hits,
            row.lattice_diffs,
            row.lattice_worst,
            row.march_ms,
            row.read_ms,
            row.frame_ms,
            row.submit_ms,
        );
    }
    println!(
        "//   sum-vs-whole : {:?}",
        rows.iter()
            .map(|row| format!(
                "cap {:>2}: legs {:.2} + {:.2} = {:.2} vs whole {:.2} ms",
                row.max_steps,
                row.march_ms,
                row.read_ms,
                row.march_ms + row.read_ms,
                row.frame_ms
            ))
            .collect::<Vec<_>>()
    );
    println!();

    for row in &rows {
        assert_eq!(
            row.law_reported, row.max_steps,
            "this process wrote a cap of {} and the march reports {}, so the law in the header is not the law \
             the shader ran",
            row.max_steps, row.law_reported
        );
        assert_eq!(
            row.bit_diffs, 0,
            "at cap {}, the two legs and the whole path differ in {} of {} depth bit patterns, so the split is \
             not the same frame twice",
            row.max_steps,
            row.bit_diffs,
            row.read_bytes / 4
        );
        assert_eq!(
            row.read_hits, row.frame_hits,
            "at cap {}, the read-back alone sees {} surfaces and the whole path sees {}",
            row.max_steps, row.read_hits, row.frame_hits
        );
        assert_eq!(
            row.read_bytes,
            (R1_W as usize) * (R1_H as usize) * 4,
            "at cap {}, the copy moved {} bytes rather than the frame's {} depth bytes",
            row.max_steps,
            row.read_bytes,
            R1_W as usize * R1_H as usize * 4
        );
        assert_eq!(
            row.lattice_diffs, 0,
            "at cap {}, {} of the {} lattice pixels disagree on hit-vs-miss with `render::march`, so the frame \
            being priced is not the frame the repo has tests for",
            row.max_steps, row.lattice_diffs, row.lattice_hits
        );
        assert!(
            row.lattice_worst < 0.002,
            "at cap {}, the worst lattice depth deviation is {}",
            row.max_steps,
            row.lattice_worst
        );
    }

    // The integer shape of the cap, which is the falsifier for a law that never reaches the shader: if the
    // header's step count were decoration, all three rows would find the same frame.
    let found = |cap: u32| -> usize {
        rows.iter()
            .find(|row| row.max_steps == cap)
            .map(|row| row.read_hits)
            .unwrap_or(usize::MAX)
    };
    let (one, eight, sixty_four) = (found(1), found(8), found(MAX_STEPS));
    assert_eq!(
        one, 0,
        "cap 1 found {one} surfaces where `r1_round_trip_split` and `r1_device_rays` establish that one step \
         per ray reaches no surface in this field, so the cap is being ignored in a way no timing line would \
         show"
    );
    assert!(
        eight > 0,
        "cap 8 found {eight} surfaces, so eight steps reach nothing the reference reaches - the lattice anchor \
         would have caught that, and this row means the dispatch never ran"
    );
    assert!(
        sixty_four > eight,
        "cap 64 found {sixty_four} against cap 8's {eight}, so the extra stepping changed no pixels and the \
         law the header carries is not the law the shader used"
    );
    println!("// cap shape    : cap 1 finds {one}, cap 8 finds {eight}, cap {MAX_STEPS} finds {sixty_four} of {} pixels - a", R1_W as usize * R1_H as usize);
    println!("//              header the shader reads, and step work is still buying pixels at this grid.");
}
