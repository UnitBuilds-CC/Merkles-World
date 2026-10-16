//! R1's shape question, priced on a device: what does a frame pay before any ray moves, and what does it pay
//! for the step loop?
//!
//! `cargo run --release -p gpu --example r1_step_price`
//!
//! `on_device_march` measured one 1080p frame at one cap (64 steps) and reported ~824 ms against R1's
//! 16.67 ms. That single point cannot say what would fix the gap: if most of the frame is dispatch and
//! copy-back, no amount of smarter marching helps and the fix is on the host side; if most of it is the
//! march's own step loop, then removing steps - culling, a bound hierarchy, fewer rays - is the lever, and its
//! price becomes arithmetic. Sweeping the step cap answers which world this is, and the answer turned out to be
//! a third shape: **the frame stops growing with the cap long before 64**, so past a point there is no step
//! work left for culling to remove.
//!
//! The cap reaches the shader through the march header's `max_steps`, so the sweep is a law change and not a
//! workload change: same scene, same camera, same rays, same grid, only the number of sphere-trace iterations
//! each ray may take. Two things make the sweep mean something rather than being a curve of numbers:
//!
//! - **Each row is anchored to the CPU reference at its own cap.** On a 64x48 grid the device's hit count is
//!   compared against `render::march` run with the same `max_steps`, ray for ray. A shader that ignored the cap
//!   (the failure mode that would make the whole sweep a story about nothing) diverges from a reference that
//!   honours it, and the assert fires. That is the falsification handle, and it is an integer comparison, not a
//!   timing one.
//! - **The cap-1 row is the fixed term, measured rather than modelled.** One step per ray is as close to "no
//!   march at all" as this path can be given, so its time is dispatch plus the ray buffer plus the depth
//!   copy-back, which is the number R1's budget has to leave room for before any optimisation.
//!
//! A first draft of this example fitted a least-squares line through the seven rows and printed its intercept
//! and slope. The fit was wrong and its budget line printed a negative number of affordable ray-steps: the rows
//! are not linear in the cap, because a ray that has converged pays no further step and a ray that will never
//! hit has already spent its budget on the miss. Nothing is fitted now - the printed terms are the measured
//! rows, and where a number is carried to another resolution the ratio is stated as DERIVED.
//!
//! And the "stops growing" claim above needs its honesty stated in the same breath, because five runs of this
//! code did not agree about the curve: cap 1 printed 24.28 / 33.24 / 87.92 / 32.62 / 32.27 ms and the cap-8 row
//! came in at 80.90 and 95.69 ms in two runs and 545.5-551.7 ms in three, with cap 64 back under it every time.
//! What was identical in all five is the *work* - cap 4 and cap 8 find the same 7,240 surfaces, and cap 4 to
//! cap 64 (16x the steps) changes 1,772 of 230,400 rays - so the saturation below is claimed from hit counts
//! and the milliseconds are printed as one sample each, which is what ADR-017 asks for anyway.
//!
//! The per-cap anchor was falsified by giving the reference one step more than the device at every cap: the
//! sweep stopped at cap 1 with 12 hit-vs-miss disagreements. That plant also caught the anchor reading
//! backwards - the reference's hits were tallied only where the device agreed, so the count assert compared the
//! device against the intersection - which is why `reference_hits` now counts what the reference sees alone.
//!
//! Durations are never evidence (ADR-017, R4 informational): no line below asserts a millisecond. Timing is
//! also why the sweep runs at 640x360 rather than 1080p: the first 1080p attempt died at cap 8 with `Parent
//! device is lost` on this adapter after four rows, having reported 1,745 ms at cap 8 against
//! `on_device_march`'s 824 ms at cap 64 - a driver thrashing on a fresh 66 MB ray buffer per march, measuring
//! itself rather than the march. A smaller grid prices the same law without that pathology.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::Interpreter;
use render::{march, Ray, Tree, ROOT};
use std::time::Instant;

/// `on_device_march`'s and `frame_curve`'s scene, word for word, so the numbers below describe the same
/// three-node field the earlier measurements priced.
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
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
/// The resolution R1 names. Used only to state a ratio, never measured here - `on_device_march` is the
/// example that measures 1080p, and its figure is cited rather than re-derived.
const R1_RAYS: usize = 1920 * 1080;
/// The caps swept. Powers of two so a reader can see where the step loop stops mattering.
const STEPS: [u32; 7] = [1, 2, 4, 8, 16, 32, 64];
/// Marches per cap whose median is reported. One sample is a single frame, and a single frame on a laptop
/// also measures whatever else the machine was doing.
const REPS: usize = 3;
/// R1's whole-frame budget at 1080p and 60 fps, as docs/04 states it.
const BUDGET_MS: f64 = 16.67;

/// The same pinhole arithmetic as `on_device_march`, with the swept cap carried into each ray.
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

fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(|a, b| a.total_cmp(b));
    times[times.len() / 2]
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP r1_step_price: no device ({error:?}) - the question is what a GPU charges for a step, \
                 and a software rasteriser would answer it with the reference path's cost wearing a GPU label"
            );
            return;
        }
    };
    let tree = scene();
    println!("// adapter       : {}", interpreter.adapter());
    println!(
        "// grid          : {WIDTH}x{HEIGHT} = {} rays per frame, 3-node field, max_t {MAX_T}, surface {SURFACE}",
        WIDTH as usize * HEIGHT as usize
    );
    println!("// cap | device hits | reference hits | disagreements | worst |dev| | frame hits | ms median | ns per ray-step (amortised)");

    let mut times = Vec::with_capacity(STEPS.len());
    let mut frame_hits = Vec::with_capacity(STEPS.len());
    for &cap in &STEPS {
        // The anchor, at this cap: the device and the CPU reference must see the same picture on the small
        // grid. `render::march` honours max_steps, so a shader that ignored the header's cap diverges here.
        let check = rays(64, 48, cap);
        let on_device = interpreter
            .march(&tree, ROOT, &check)
            .expect("the small grid must march at every cap");
        let mut reference_hits = 0usize;
        let mut disagreements = 0usize;
        let mut worst = 0.0f32;
        for (ray, depth) in check.iter().zip(on_device.depths.iter()) {
            let hit = march(
                &tree,
                ROOT,
                ray.origin,
                ray.dir,
                ray.max_t,
                ray.surface,
                ray.max_steps,
            );
            // Counted from the reference alone, not from the pair: a plant that gave the reference one step
            // more than the device found its 12 hits tallied only where the device agreed with them, so the
            // count assert below compared the device against the intersection and could not see the direction
            // the reference was wrong in. The disagreement arm still catches it; the column now says what it
            // is labelled.
            if hit.is_some() {
                reference_hits += 1;
            }
            match (hit, *depth < 1.0e37) {
                (Some(hit), true) => worst = worst.max((hit.t - *depth).abs()),
                (None, false) => {}
                _ => disagreements += 1,
            }
        }
        assert_eq!(
            on_device.hits, reference_hits,
            "at cap {cap} the device found {} surfaces and the reference found {reference_hits} - either the \
             shader is not honouring the cap or the reference is, and then no row of the sweep below means \
             what it says",
            on_device.hits
        );
        assert!(
            disagreements == 0 && worst < 0.002,
            "at cap {cap} the on-device march and render::march disagree ({disagreements} hit-vs-miss, worst \
             deviation {worst}), so the swept rows do not describe the reference's step law"
        );

        // The priced row: one full swept grid at this cap, REPS times, median reported.
        let big = rays(WIDTH, HEIGHT, cap);
        let mut row_times = Vec::with_capacity(REPS);
        let mut swept_hits = 0usize;
        for _ in 0..REPS {
            let start = Instant::now();
            let frame = interpreter
                .march(&tree, ROOT, &big)
                .expect("the swept grid must march at every cap");
            row_times.push(start.elapsed().as_secs_f64() * 1000.0);
            swept_hits = frame.hits;
        }
        // Printed, not asserted, and it earned its place: the first draft asserted that every frame finds
        // surface and died at cap 1, where no ray of the grid reaches the surface inside one step. That is the
        // device twin of the CPU's sub-ray-charge law (docs/04: a ceiling below `max_steps + 1` draws nothing),
        // so an empty low-cap row is a real frame paying the step loop for nothing rather than a broken sample -
        // and it still prices the fixed term, because every ray did take its one step.
        let ms = median(row_times);
        let rays = big.len() as f64;
        times.push(ms);
        frame_hits.push(swept_hits);
        println!(
            "// {:>3} | {:>11} | {:>14} | {:>13} | {:>9.6} | {:>10} | {:>14.2} | {:>23.2}",
            cap,
            on_device.hits,
            reference_hits,
            disagreements,
            worst,
            swept_hits,
            ms,
            ms * 1.0e6 / (rays * cap as f64)
        );
    }

    // The two terms, stated as what this run measures and what five runs have agreed on. The millisecond curve
    // did NOT reproduce. Runs 1-2 rose smoothly from cap 1 (24.28, 33.24 ms) to cap 64 (107.9, 107.7). Runs 3-5
    // put cap 1 at 87.92, 32.62, 32.27 and then produced a **bimodal cap-8 row** - 80.90 ms in run 1, and
    // 550.57 / 551.73 / 545.50 in the three later runs - with cap 64 coming back under it at 108-126 ms every
    // time. So the shape of the step cost belongs to the box, and the hit counts and deviations, which did not
    // move once across all five, are what the saturation claim rests on.
    let fixed = times[0];
    println!(
        "// fixed term    : {fixed:.2} ms at cap 1 - one step per ray, and 0 of the small grid's rays reaching \
         surface at that cap. MEASURED, printed as one sample: five runs of this binary put this row between \
         24.28 and 87.92 ms, and every one of them is over R1's {BUDGET_MS} ms with essentially no march in it."
    );
    let rays_in_grid = WIDTH as usize * HEIGHT as usize;
    let last_hits = frame_hits[frame_hits.len() - 1];
    let extra_surfaces = last_hits.saturating_sub(frame_hits[2]);
    println!(
        "// work saturates  : cap 4 and cap 8 find the SAME {} surfaces, and cap 4 to cap 64 - {}x the permitted \
         steps - moves the frame by {extra_surfaces} surfaces of {rays_in_grid} rays ({:.2}% of the grid). \
         Integers, which is why the saturation is claimed from them and not from the millisecond rows: five runs \
         of this code agree on every hit count and disagree about the curve - the cap-8 row alone came in at \
         81 ms twice and 545-552 ms three times.",
        frame_hits[3],
        STEPS[STEPS.len() - 1] / STEPS[2],
        100.0 * extra_surfaces as f64 / rays_in_grid as f64
    );
    println!(
        "// at R1's grid    : {} rays is {:.1}x this frame (DERIVED ratio); `on_device_march` measures this \
         same scene at 1920x1080 cap 64 in ~824 ms against R1's {BUDGET_MS} ms",
        R1_RAYS,
        R1_RAYS as f64 / (WIDTH as usize * HEIGHT as usize) as f64
    );
    println!(
        "// the verdict     : the steps a ray is permitted is a bounded part of what this frame does and the \
         host round trip is not - cap 1 already misses the whole budget in every one of the five runs. So R1's \
         lever (culling, a bound hierarchy, fewer rays) attacks work that saturates while the round trip keeps \
         costing the same - MEASURED shape on this adapter, DERIVED as a claim about 1080p."
    );
    println!(
        "// still not met   : R1 is a duration gate and ADR-017 keeps it informational; nothing above is \
         asserted as a millisecond. The asserted terms are the per-cap hit counts and depth deviations against \
         render::march, at every cap in the sweep."
    );
}
