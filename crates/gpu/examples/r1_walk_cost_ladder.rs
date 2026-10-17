//! What the interpreter's per-invocation stack capacity costs, measured rather than argued.
//!
//! `cargo run --release -p gpu --example r1_walk_cost_ladder`
//!
//! The 2026-10-17 same-session baseline (`r1_march_alone`, GTX 1070) priced R1's 1080p frame at 91.35 ms against
//! a 16.67 ms budget and found the march leg is ~9.5 tree walks per pixel at cap 64, while one walk per pixel
//! (cap 1) costs 9.30 ms for the whole grid — 4.49 ns each. It named two structural levers, and this file prices
//! the second one: `interp.wgsl` carries `var stack: array<Frame, DEPTH>` with `const DEPTH: u32 = 32u` for a
//! demo tree two frames deep, so every one of the 2,073,600 invocations allocates 32 × 32 = 1,024 bytes of
//! per-thread state, indexed dynamically by `sp`, which is the shape that cannot become registers.
//!
//! The ladder is one string substitution at pipeline-creation time (`Interpreter::camera_variant`): the shipped
//! source with its `DEPTH` literal replaced by another capacity, compiled as its own module. Nothing else varies
//! — same walk, same march law, same bindings, same camera block — so a difference between two rows is the
//! difference in the state each invocation carries. Duplicating the walk to vary its stack would have been the
//! fork `interp.wgsl`'s header comment exists to prevent.
//!
//! Why the rows are comparable at all, as an asserted integer instead of a hope: for this scene every capacity
//! from 2 up is at or above the depth the tree reaches, so no row may ever hit the walk's `sp >= DEPTH` guard,
//! and each row's depths must come back **bit-identical** to the shipped pipeline's at the same grid and cap. A
//! capacity that silently refused a subtree would show up as differing depths, not as a faster row. `DEPTH = 1`
//! is the falsifier, run on the anchor grid where the guard *is* reachable: it must find 0 surfaces where the
//! shipped pipeline finds 132, which is what proves the substituted literal reaches the shader and that the
//! ladder is not one module compiled six times.
//!
//! The `DEPTH = 32` variant row is the instrument's null control — the same literal, reached through the same
//! substitution path — so whatever "compile a second module and dispatch it" costs, that row carries it too and
//! the other capacities are not credited with a saving they did not make.
//!
//! **The timings are interleaved, and the first run of this file is why.** Taken one capacity after another in a
//! fixed order, the laptop's rows came out monotonic in `DEPTH` *and* the shipped control — dispatched first, in
//! the coolest part of the run — read 2× faster than the `DEPTH = 32` variant dispatched last, which is an
//! ordering artifact, not a capacity effect. So each pass dispatches every leg once, the leg order rotates by one
//! per pass, and the shipped pipeline is a leg in the same interleaving rather than a preamble to it. A row's
//! median is then taken over readings with each position in the sequence represented once.
//!
//! Durations are printed and assert nothing (ADR-017; R1 is a duration gate and stays informational), so what
//! this settles is the *shape* of the stack term — whether shrinking it is worth a change to the interpreter,
//! and by how much per walk. Nothing here changes what the shipped pipeline compiles.
//!
//! One edge this file deliberately does not probe: the walk checks `sp >= DEPTH` before *reading* a frame, then
//! pushes the child at `sp + 1`, so a refusal always writes one element past the array's end and relies on the
//! backend's index handling to stay in memory. Every ladder capacity is at or above the depth this tree reaches,
//! so no ladder row takes that path; only the 64x48 falsifier does, and it is kept there. The edge is recorded
//! in docs/04 rather than fixed here, because fixing it changes the shipped module every other R1 row is priced
//! against.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{Bind, CameraVariant, GpuCamera, Interpreter};
use render::{march, Ray, Tree, Vec3, ROOT};

/// `r1_march_alone`'s scene, word for word, so this file's rows describe the same frame the baseline priced.
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
const ANCHOR_W: u32 = 64;
const ANCHOR_H: u32 = 48;
const R1_W: u32 = 1920;
const R1_H: u32 = 1080;
/// The no-surface sentinel `interp.wgsl` writes, shared with `render`'s miss.
const MISS: f32 = 1.0e37;
/// `interp.wgsl`'s shipped capacity, and the row every other row is compared against.
const SHIPPED: u32 = 32;
/// `interp.wgsl`'s `struct Frame { node: u32, stage: u32, p: vec3<f32>, left: f32 }`: `vec3` aligns to 16, so
/// the record is 8 + 16 + 4 = 28 bytes padded to 32. A model of the private state, not a measured byte count —
/// a driver may keep part of it in registers, which is exactly what the ladder's spread is trying to find out.
const FRAME_BYTES: usize = 32;
/// Passes per cap. Each pass dispatches every leg once, so a leg's readings cover every position in the
/// rotation exactly once.
const PASSES: usize = 5;

/// `LADDER_GRID=WxH` prices the ladder on a grid the reading device survives; unset reproduces R1's own.
fn ladder_grid() -> (u32, u32) {
    std::env::var("LADDER_GRID")
        .ok()
        .and_then(|text| {
            let (w, h) = text.trim().split_once(['x', 'X'])?;
            let w = w.trim().parse::<u32>().ok()?;
            let h = h.trim().parse::<u32>().ok()?;
            Some((w, h))
        })
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((R1_W, R1_H))
}

/// The camera `r1_march_alone` aims down +z, so the two files march the same rays.
fn camera(width: u32, height: u32, origin: Vec3, max_steps: u32) -> GpuCamera {
    GpuCamera {
        origin,
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        forward: [0.0, 0.0, 1.0],
        half_height: HALF_HEIGHT,
        max_t: MAX_T,
        surface: SURFACE,
        max_steps,
        root: ROOT,
        width,
        height,
    }
}

fn ray_at(px: u32, py: u32, width: u32, height: u32, origin: Vec3) -> Ray {
    let aspect = width as f32 / height as f32;
    let u = (px as f32 + 0.5) / width as f32;
    let v = (py as f32 + 0.5) / height as f32;
    let ndc_x = (2.0 * u - 1.0) * HALF_HEIGHT * aspect;
    let ndc_y = (1.0 - 2.0 * v) * HALF_HEIGHT;
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
        max_steps: MAX_STEPS,
    }
}

/// The CPU reference over a whole grid, which is how the anchor row knows it is still the shipped march.
fn reference_depths(tree: &Tree, width: u32, height: u32, origin: Vec3) -> Vec<f32> {
    let mut out = Vec::with_capacity((width * height) as usize);
    for py in 0..height {
        for px in 0..width {
            let ray = ray_at(px, py, width, height, origin);
            out.push(
                match march(
                    tree,
                    ROOT,
                    ray.origin,
                    ray.dir,
                    ray.max_t,
                    ray.surface,
                    ray.max_steps,
                ) {
                    Some(hit) => hit.t,
                    None => MISS,
                },
            );
        }
    }
    out
}

/// Pixels whose depth bit pattern differs between two buffers. This is the term that makes a ladder row
/// admissible: a capacity that changed the field would change these bits before it changed a millisecond.
fn bit_differences(a: &[f32], b: &[f32]) -> usize {
    a.iter()
        .zip(b.iter())
        .filter(|(left, right)| left.to_bits() != right.to_bits())
        .count()
}

/// Median of the readings, so one throttled dispatch cannot move a row.
fn median(readings: &[f64]) -> f64 {
    let mut sorted = readings.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    sorted[sorted.len() / 2]
}

/// Worst minus best reading: the leg's own repeatability, printed so a 0.4% spread and a 40% one are not read
/// as the same kind of number.
fn spread(readings: &[f64]) -> f64 {
    let min = readings.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = readings.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    max - min
}

/// One timed leg of the ladder: the shipped pipeline (no `variant`) or a capacity, each with its own bind group
/// on the same frame, so the interleaved pass is one dispatch per leg and nothing else.
struct Leg {
    depth: u32,
    variant: Option<CameraVariant>,
    bind: Bind,
    readings: Vec<f64>,
}

impl Leg {
    fn dispatch(&self, interpreter: &Interpreter, frame: &gpu::device::CameraFrame) -> f64 {
        match &self.variant {
            Some(variant) => interpreter
                .march_camera_variant_no_readback(variant, &self.bind, frame)
                .expect("a ladder leg must march"),
            None => interpreter
                .march_camera_frame_no_readback(&self.bind, frame)
                .expect("the shipped leg must march"),
        }
    }

    fn shipped(&self) -> bool {
        self.variant.is_none()
    }
}

/// One printed row: a leg at one cap, with the integers the ladder asserts.
struct Row {
    depth: u32,
    shipped: bool,
    cap: u32,
    ms: f64,
    spread_ms: f64,
    hits: usize,
    bit_diffs: usize,
    pixels: usize,
}

impl Row {
    /// Nanoseconds of march per pixel. At cap 1 that is exactly one tree walk, which is the number the ladder
    /// is about; at cap 64 the baseline's ~9.5 walks per pixel is the divisor a reader has to apply.
    fn ns_per_pixel(&self) -> f64 {
        self.ms * 1.0e6 / self.pixels as f64
    }

    fn stack_bytes(&self) -> usize {
        FRAME_BYTES * self.depth as usize
    }
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP r1_walk_cost_ladder: no device ({error:?}) - the term priced here is the per-invocation \
                 private state a GPU thread carries, which a software rasteriser neither allocates nor spills"
            );
            return;
        }
    };
    let tree = scene();
    let shape = interpreter.upload(&tree);
    let origin = [0.0f32, 0.0, -6.0];

    println!("// adapter    : {}", interpreter.adapter());
    println!("// scene      : the 3-node field `r1_march_alone` baselines, root index {ROOT}, max_t {MAX_T}, surface {SURFACE}");
    println!("// rows       : the shipped interp.wgsl with its `const DEPTH: u32 = {SHIPPED}u;` literal replaced; nothing else in the module changes");
    println!("// asserted   : every capacity at or above the tree's depth returns the shipped pipeline's depths bit for bit; the shipped control returns render::march's picture on {ANCHOR_W}x{ANCHOR_H}; DEPTH = 1 finds 0 surfaces there");
    println!("// printed    : median of {PASSES} interleaved, order-rotated march-no-readback legs and ns per pixel - durations assert nothing (ADR-017)");
    println!();

    // ---- The anchor: what the shipped pipeline is, and what DEPTH = 1 is not -----------------------
    let mut anchor = interpreter.begin_camera_frame(ANCHOR_W, ANCHOR_H);
    let written =
        interpreter.write_camera(&mut anchor, &camera(ANCHOR_W, ANCHOR_H, origin, MAX_STEPS));
    let anchor_bind = interpreter.bind_camera(&shape, &anchor);
    let control = interpreter
        .march_camera_frame(&anchor_bind, &anchor)
        .expect("the anchor grid must march on the shipped pipeline");
    let reference = reference_depths(&tree, ANCHOR_W, ANCHOR_H, origin);
    let set_diffs = reference
        .iter()
        .zip(&control.depths)
        .filter(|(left, right)| (**left < MISS) != (**right < MISS))
        .count();
    let worst = reference
        .iter()
        .zip(&control.depths)
        .filter(|(left, right)| **left < MISS && **right < MISS)
        .map(|(left, right)| (left - right).abs())
        .fold(0.0f32, f32::max);
    let reference_hits = reference.iter().filter(|depth| **depth < MISS).count();

    println!("// anchor       : {ANCHOR_W}x{ANCHOR_H}, camera block {written} bytes");
    println!(
        "//   shipped hits / reference hits : {} / {reference_hits}",
        control.hits
    );
    println!("//   hit/miss set diffs            : {set_diffs}, worst depth deviation {worst:.6}");
    assert_eq!(
        written, 112,
        "the camera block is {written} bytes, not the 16-byte header plus 96-byte camera the baseline rows price"
    );
    assert_eq!(
        set_diffs, 0,
        "the shipped pipeline disagrees with `render::march` on {set_diffs} of {} anchor pixels, so the control \
         this ladder compares against is not the march the repo has tests for",
        control.depths.len()
    );
    assert!(
        worst < 0.002,
        "the worst anchor depth deviation is {worst}, so the control differs from the reference by more than a \
         step's rounding"
    );
    assert!(
        control.hits > 0,
        "the shipped pipeline found {} surfaces on the anchor grid, so there is nothing for the falsifier to \
         break and the ladder has no reference row",
        control.hits
    );
    assert_eq!(
        control.max_steps, MAX_STEPS,
        "the control reports a cap of {}, so the header this process wrote is not the header the shader ran",
        control.max_steps
    );
    let shipped_hits = control.hits;

    // The falsifier, and the only place the walk's `sp >= DEPTH` guard is reachable: a capacity of 1 cannot hold
    // a root plus a child, so every ray must return the no-surface sentinel. If this row still found
    // `shipped_hits` surfaces, the substituted literal would not be reaching the shader and every capacity row
    // below would be the shipped module wearing a different label.
    let refuse = interpreter
        .camera_variant(1)
        .expect("a DEPTH = 1 variant is a legal module; if the substitution cannot build one the ladder is dead");
    let refuse_bind = interpreter.bind_camera_variant(&refuse, &shape, &anchor);
    interpreter
        .march_camera_variant_no_readback(&refuse, &refuse_bind, &anchor)
        .expect("the DEPTH = 1 anchor must march");
    let refused = interpreter
        .read_camera_depths(&anchor)
        .expect("the DEPTH = 1 frame must come back");
    println!(
        "//   DEPTH = 1    : {} surfaces, {} of {} depth bit patterns differ from the shipped anchor - the \
         guard's sentinel, so the substitution is live",
        refused.hits,
        bit_differences(&control.depths, &refused.depths),
        refused.depths.len()
    );
    println!();
    assert_eq!(
        refused.hits, 0,
        "a stack capacity of 1 found {} surfaces on a tree the shipped pipeline walks into {shipped_hits}, so \
         the walk's own guard did not fire and the DEPTH literal this ladder varies is decoration",
        refused.hits
    );

    // ---- The ladder --------------------------------------------------------------------------------
    //
    // A loss here is not an `Err`: wgpu 27 panics from inside `Device::poll`, so the block runs under
    // `catch_unwind`, legs print as they are taken, and only a payload naming a lost device or a validation
    // refusal prints a skip. Anything else that unwinds exits 101 rather than hiding a failure behind a friendly
    // line.
    let (w, h) = ladder_grid();
    let grid_label = if (w, h) == (R1_W, R1_H) {
        "1080p".to_string()
    } else {
        format!("{w}x{h}")
    };
    let pixels = w as usize * h as usize;
    println!(
        "// grid         : {grid_label:<11} = {pixels} pixels per frame{}",
        if grid_label == "1080p" {
            ", R1's own resolution".to_string()
        } else {
            ", NOT R1's grid, so these rows price the frontier, not the gate".to_string()
        }
    );

    let depths = [2u32, 4, 8, 16, SHIPPED, 64];
    let caps = [1u32, MAX_STEPS];
    let outcome = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| {
        let mut rows = Vec::new();
        for &cap in &caps {
            // One frame per cap, re-dispatched by every leg. The buffer is 8 MB at R1's grid; allocating a fresh
            // one per row would put the allocator inside the number being measured.
            let mut big = interpreter.begin_camera_frame(w, h);
            interpreter.write_camera(&mut big, &camera(w, h, origin, cap));

            let mut legs = vec![Leg {
                depth: SHIPPED,
                variant: None,
                bind: interpreter.bind_camera(&shape, &big),
                readings: Vec::with_capacity(PASSES),
            }];
            for &depth in &depths {
                match interpreter.camera_variant(depth) {
                    Ok(variant) => legs.push(Leg {
                        depth,
                        bind: interpreter.bind_camera_variant(&variant, &shape, &big),
                        variant: Some(variant),
                        readings: Vec::with_capacity(PASSES),
                    }),
                    Err(error) => {
                        println!("//   DEPTH = {depth:>2} : could not be compiled - {error}")
                    }
                }
            }

            // Timing: every leg once per pass, the start position rotated by the pass index, so each leg is read
            // once in each slot of the sequence and the shipped row is not privileged by being first.
            for pass in 0..PASSES {
                for step in 0..legs.len() {
                    let index = (pass + step) % legs.len();
                    let ms = legs[index].dispatch(&interpreter, &big);
                    legs[index].readings.push(ms);
                }
            }

            // Integers: the shipped leg's depths are the reference, then each capacity's are compared against
            // them. Separate from the timing loop so a read (a copy plus a map) never sits inside a leg's ms.
            interpreter
                .march_camera_frame_no_readback(&legs[0].bind, &big)
                .expect("the shipped leg must march before its depths are read");
            let control_read = interpreter
                .read_camera_depths(&big)
                .expect("the shipped pipeline's grid depths must come back");
            let mut taken = 0usize;
            for leg in &legs {
                let (bit_diffs, hits) = if leg.shipped() {
                    (0, control_read.hits)
                } else {
                    interpreter
                        .march_camera_variant_no_readback(
                            leg.variant.as_ref().unwrap(),
                            &leg.bind,
                            &big,
                        )
                        .expect("this capacity must march before its depths are read");
                    let read = interpreter
                        .read_camera_depths(&big)
                        .expect("this capacity's grid depths must come back");
                    (
                        bit_differences(&control_read.depths, &read.depths),
                        read.hits,
                    )
                };
                let readings = &leg.readings;
                let row = Row {
                    depth: leg.depth,
                    shipped: leg.shipped(),
                    cap,
                    ms: median(readings),
                    spread_ms: spread(readings),
                    hits,
                    bit_diffs,
                    pixels,
                };
                println!(
                    "//   {:>13} | cap {cap:>3} | {:>8.3} ms | spread {:>6.3} | {:>8.3} ns/pixel | {} diffs | {} hits",
                    if row.shipped {
                        format!("{} shipped", SHIPPED)
                    } else {
                        format!("{:>5}", row.depth)
                    },
                    row.ms,
                    row.spread_ms,
                    row.ns_per_pixel(),
                    row.bit_diffs,
                    row.hits,
                );
                rows.push(row);
                taken += 1;
            }
            assert_eq!(
                taken,
                legs.len(),
                "the cap {cap} block printed {taken} of {} legs, so a leg was dropped rather than measured",
                legs.len()
            );
        }
        rows
    }));

    let rows = match outcome {
        Ok(rows) => rows,
        Err(payload) => {
            let text = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|held| held.to_string()))
                .unwrap_or_else(|| "a panic with no message".to_string());
            if !(text.contains("device is lost") || text.contains("Validation")) {
                println!("// {grid_label:<11}: UNEXPECTED PANIC, not a lost device, so it is a failure: {text}");
                std::process::exit(101);
            }
            println!("// {grid_label:<11}: SKIPPED, the device was lost mid-ladder: {text}");
            println!("//                (legs printed above are partial; the anchor asserts above are the shipped ones)");
            return;
        }
    };

    println!();
    println!(
        "//   depth | stack B/item |   cap |  hits | bit diffs vs shipped |  ms median/{PASSES} |  spread | ns/pixel"
    );
    for row in &rows {
        println!(
            "//   {:>5}{} | {:>12} | {:>5} | {:>5} | {:>20} | {:>14.3} | {:>7.3} | {:>8.3}",
            row.depth,
            if row.shipped { "*" } else { " " },
            row.stack_bytes(),
            row.cap,
            row.hits,
            row.bit_diffs,
            row.ms,
            row.spread_ms,
            row.ns_per_pixel(),
        );
    }
    println!("//   (* = the pipeline the crate ships, reached without the substitution path)");
    println!();

    // ---- The asserted integers ---------------------------------------------------------------------
    for &cap in &caps {
        let shipped = rows
            .iter()
            .find(|row| row.shipped && row.cap == cap)
            .expect("the shipped row is taken for every cap");
        assert_eq!(
            rows.iter().filter(|row| row.cap == cap).count(),
            depths.len() + 1,
            "cap {cap} produced {} rows where the ladder names {} capacities plus the shipped control, so a \
             row is missing and the percentages below are over an incomplete ladder",
            rows.iter().filter(|row| row.cap == cap).count(),
            depths.len()
        );
        for row in rows.iter().filter(|row| !row.shipped && row.cap == cap) {
            assert_eq!(
                row.bit_diffs, 0,
                "at cap {cap}, DEPTH = {} returned {} depths whose bits differ from the shipped pipeline's over \
                 {pixels} pixels, so the ladder's rows do not compute one field and their milliseconds are not \
                 comparable",
                row.depth, row.bit_diffs
            );
            assert_eq!(
                row.hits, shipped.hits,
                "at cap {cap}, DEPTH = {} found {} surfaces where the shipped pipeline found {}",
                row.depth, row.hits, shipped.hits
            );
        }
        // The twin of the shipped literal is the null control: same capacity, reached through the substitution
        // path. If it disagreed on a single bit, the second module would not be the same program and the
        // capacity rows would be measuring a compile difference rather than a state difference.
        let twin = rows
            .iter()
            .find(|row| !row.shipped && row.depth == SHIPPED && row.cap == cap)
            .expect("the DEPTH = 32 twin is in the ladder");
        assert_eq!(
            twin.bit_diffs, 0,
            "the twin disagrees with the shipped pipeline at cap {cap}"
        );
    }

    // The cap must still be the law the header carries, or a row bit-identical to its neighbour could be a
    // dispatch that never read it.
    let at = |cap: u32| -> usize {
        rows.iter()
            .find(|row| row.shipped && row.cap == cap)
            .map(|row| row.hits)
            .unwrap_or(usize::MAX)
    };
    println!(
        "// cap shape    : at {grid_label} the shipped row finds {} surfaces at cap 1 and {} at cap {MAX_STEPS}; \
         the {ANCHOR_W}x{ANCHOR_H} anchor found {shipped_hits} of {}",
        at(1),
        at(MAX_STEPS),
        ANCHOR_W as usize * ANCHOR_H as usize
    );
    assert!(
        at(MAX_STEPS) >= at(1),
        "the cap-{MAX_STEPS} row found {} surfaces and the cap-1 row found {}, so the stepping law is not the \
         one the header carries",
        at(MAX_STEPS),
        at(1)
    );

    // The number the ladder exists to print: the per-walk price at each capacity, against the shipped row.
    for &cap in &caps {
        let denominator = rows
            .iter()
            .find(|row| row.shipped && row.cap == cap)
            .expect("the shipped row is taken for every cap")
            .ns_per_pixel();
        println!(
            "// cap {cap:>3}      : shipped {denominator:.3} ns/pixel; {}",
            rows.iter()
                .filter(|row| !row.shipped && row.cap == cap)
                .map(|row| format!(
                    "DEPTH {}: {:.3} ({:+.2}%)",
                    row.depth,
                    row.ns_per_pixel(),
                    (row.ns_per_pixel() / denominator - 1.0) * 100.0
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!(
        "// reading      : at cap 1 each pixel is exactly one tree walk, so the cap-1 column IS the per-walk \
         price at {grid_label}; the 1080p baseline's own figure was 4.49 ns"
    );
}
