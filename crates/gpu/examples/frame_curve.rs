//! One term of R1, measured on a device: how long a full grid of A-07 field evaluations takes on GPU.
//!
//! `cargo run --release -p gpu --example frame_curve`
//!
//! Gate R1 is "1080p/60 with 500 culled entities" - a march, with culling, with a BVH, on a mid-range GPU -
//! and none of that exists yet. What does exist is the WGSL master interpreter and a device that can run it,
//! so this measures the one quantity available and names it precisely: **one field evaluation per pixel**, on
//! a tree of a few nodes, timed two ways. That is a floor under R1, not a fraction of an answer: a real frame
//! is a march of tens of evaluations per pixel, and the CPU reference already measured the multiplier
//! (docs/04: steps x nodes, ~45.5 evaluations per pixel on the hero scene).
//!
//! Timings are never hashed and never gate evidence (ADR-017, R4 is informational); this example prints them
//! for that reason and asserts nothing about them. What it does assert is that the work happened: the same
//! grid is compared against `crates/render` before and after, so a number from a shader the driver optimised
//! away cannot arrive here as a speed.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::Interpreter;
use render::{Tree, Vec3};

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// A ball on a slab: three nodes, cheap, and enough structure that the interpreter walks rather than
/// short-circuits. Not the hero scene - a grid of 2M points against the hero's eleven nodes would be
/// measuring a different tree than the CPU law was taken from, and this number is already only one term.
fn scene() -> Tree {
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 3, 0, 0, true),
            record(NODE_SPHERE, 0, 1, 0, 4, false),
            record(NODE_ROUNDED_BOX, 0, 1, 4, 4, false),
        ],
        params: vec![0.5, 0.0, 0.0, 0.0, 2.0, 0.1, 2.0, 0.0, 0.0, -1.0, 0.0],
    }
}

/// A pixel grid laid over the scene in world units, z fixed: one evaluation per pixel, no ray.
fn grid(width: usize, height: usize) -> Vec<Vec3> {
    let extent = 6.0f32;
    let mut points = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let u = (x as f32 + 0.5) / width as f32 - 0.5;
            let v = (y as f32 + 0.5) / height as f32 - 0.5;
            points.push([u * extent, v * extent, 0.25]);
        }
    }
    points
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP frame_curve: no device ({error:?}) - this is a GPU measurement and a CPU adapter \
                 would report software rasterisation as if it were a frame"
            );
            return;
        }
    };
    let tree = scene();

    // The correctness anchor, before and after. Without it a dispatch that computed nothing could still
    // produce a beautiful number.
    let check = grid(64, 48);
    let before = interpreter
        .compare_with_reference(&tree, &check)
        .expect("the small grid must compare");
    println!("// adapter          : {}", interpreter.adapter());
    println!(
        "// correctness      : {} points, max deviation {:.6} before the bench",
        before.samples, before.max_abs_deviation
    );
    assert!(
        before.max_abs_deviation < 1e-4,
        "the device and the reference disagree, so no timing here describes the same function"
    );

    println!(
        "// grid             | points    | passes | evals      | ms/pass submit-only | ms/pass with read-back"
    );
    let mut largest_pass_ms = 0.0f64;
    for (w, h, passes) in [
        (128usize, 128usize, 64usize),
        (512, 512, 16),
        (1920, 1080, 4),
    ] {
        let points = grid(w, h);
        let bench = interpreter
            .bench(&tree, &points, passes, passes.min(8))
            .expect("the bench must run");
        let submit = bench.ms_submit_only_total / bench.dispatches as f64;
        if w == 1920 {
            largest_pass_ms = bench.ms_per_pass_with_read;
        }
        println!(
            "// {:>4}x{:<4} | {:>9} | {:>6} | {:>10} | {:>19.3} | {:>20.3}",
            w,
            h,
            bench.points,
            bench.dispatches,
            bench.evaluations,
            submit,
            bench.ms_per_pass_with_read
        );
    }

    let after = interpreter
        .compare_with_reference(&tree, &check)
        .expect("the small grid must compare again");
    assert!(
        after.max_abs_deviation < 1e-4,
        "the device drifted during the bench"
    );
    println!(
        "// correctness      : still {:+.6} after the bench",
        after.max_abs_deviation
    );
    println!(
        "// what the columns mean : submit-only is how fast a thread hands the pass over - on a device queue that is nearly free and says nothing about latency. The read-back column adds the 8 MiB copy-and-map this harness pays to look at a result; a renderer keeps distances on the GPU and does not pay it. Neither column is a march, and neither is culling."
    );
    // The workload a frame implies: the CPU reference counted ~45.5 field evaluations per pixel, so ask for
    // 46 and retrieve the grid once. Measured rather than extrapolated.
    let dense = grid(1920, 1080);
    match interpreter.bench(&tree, &dense, 46, 1) {
        Ok(bench) => println!(
            "// 1080p x46        : {:.1} ms for 46 evaluations per pixel with one copy back, against R1's 16.67 ms whole-frame budget",
            bench.ms_batch_with_single_read
        ),
        Err(error) => println!("// 1080p x46        : not measured ({error})"),
    }
    println!(
        "// 1080p read-back  : {largest_pass_ms:.3} ms/pass queueing alone, and the with-read column above is the completion figure - only that one bounds a frame"
    );
    println!("// R1               : NOT measured. submit-only is how fast a thread hands work over, which on a device queue is nearly free and means nothing about latency; 16.67 ms is the budget for a full march with culling at 60 Hz;");
    println!("//                    this is one field evaluation per pixel on a three-node tree, which is a floor");
    println!(
        "//                    and a rate, not a frame. Multiply by the reference's measured ~45.5"
    );
    println!("//                    evaluations per pixel before calling anything an estimate, and label it as");
    println!("//                    arithmetic on a device that is not the gate's mid-range GPU.");
}
