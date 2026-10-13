//! What the demo's scene costs at the resolution gate R1 names, measured on this CPU.
//!
//! `cargo run --release --example frame_cost -p render`
//!
//! Two documents ask for the same number from different directions. Gate **R1** is "1080p/60 with 500
//! culled entities" - a frame-time claim with no GPU behind it yet - and **AS5** wants a compile-time
//! ceiling, which needs someone to say what a frame may spend. Both have been waiting on arithmetic that
//! nobody had done, so this file does the arithmetic that *can* be done on a CPU: measure the rate, derive
//! the unbounded cost, then render 1080p under ceilings and report what survives the cut.
//!
//! Labelling, because AGENTS.md §7 is the rule and not a suggestion:
//! - **measured**: charged evaluations, wall milliseconds, pixels drawn, surface kept, on this host;
//! - **derived**: the unbounded 1080p cost, extrapolated from the measured per-pixel charge at a smaller
//!   grid - a resolution law the raster probe established is flat in evaluations per pixel;
//! - **not measured here, and the reason R1 stays unmet**: nanoseconds on a GPU. A CPU reference frame is
//!   the shape of the work, not its cost on the hardware the gate names.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SMOOTH_UNION, NODE_SPHERE, NODE_UNION,
};
use render::raster::{render, render_bnb, render_bnb_budget_in, Budget, Camera, Frame, Scan};
use render::Tree;
use std::time::Instant;

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// The boot's scene: a rounded box blended with a sphere pair, and three spread spheres - eleven records,
/// depth four, the shape `assetc::order::rebalance` emits.
fn demo_scene() -> Tree {
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 11, 0, 0, true),
            record(NODE_UNION, 2, 5, 0, 0, false),
            record(NODE_ROUNDED_BOX, 0, 1, 0, 4, false),
            record(NODE_SMOOTH_UNION, 2, 3, 4, 1, false),
            record(NODE_SPHERE, 0, 1, 5, 4, false),
            record(NODE_SPHERE, 0, 1, 9, 4, false),
            record(NODE_UNION, 2, 5, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 13, 4, false),
            record(NODE_UNION, 2, 3, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 17, 4, false),
            record(NODE_SPHERE, 0, 1, 21, 4, false),
        ],
        params: vec![
            0.35, 0.35, 0.9, 0.12, //
            0.5, 0.7, -0.9, 0.0, 0.0, //
            0.7, 0.9, 0.0, 0.0, //
            0.8, 0.0, 0.0, 0.0, //
            0.5, -6.0, 0.0, 1.5, //
            0.5, 6.2, -0.4, -1.0, //
            0.5, 1.0, 3.2, 7.5,
        ],
    }
}

fn camera(width: u32, height: u32) -> Camera {
    Camera {
        origin: [0.0, 0.6, -4.0],
        forward: [0.0, -0.1, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width,
        height,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

fn main() {
    let tree = demo_scene();
    println!(
        "// frame cost - the demo's scene, on this CPU. host {} / arch {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("// MEASURED columns are wall time on this machine. DERIVED rows say so. R1 is a GPU gate and stays unmet.");
    println!();

    // 1. The resolution law, measured: charged evaluations per pixel should not depend on the grid size.
    println!(" grid        | ms     | rays   | charged evals | evals/pixel | ns/eval");
    let mut rate = 0.0f64;
    for (width, height) in [(96u32, 72u32), (192, 144), (320, 240)] {
        let camera = camera(width, height);
        let start = Instant::now();
        let (_, stats) = render(&tree, &camera);
        let elapsed = start.elapsed();
        let pixels = f64::from(width * height);
        let per_pixel = stats.evaluations as f64 / pixels;
        rate = elapsed.as_nanos() as f64 / stats.evaluations as f64;
        println!(
            " {:>4}x{:<4}   | {:>6.1} | {:>6} | {:>13} | {:>11.2} | {:>7.1}",
            width,
            height,
            elapsed.as_micros() as f64 / 1_000.0,
            stats.rays,
            stats.evaluations,
            per_pixel,
            rate,
        );
    }
    println!();

    // 2. The derivation R1's resolution implies, stated as arithmetic on measured numbers.
    let pixels_1080: f64 = 1920.0 * 1080.0;
    let evals_per_pixel = {
        let camera = camera(320, 240);
        let (_, stats) = render(&tree, &camera);
        stats.evaluations as f64 / (320.0 * 240.0)
    };
    let derived_1080 = pixels_1080 * evals_per_pixel;
    let derived_seconds = derived_1080 * rate / 1.0e9;
    println!("// DERIVED, not rendered: an unbounded 1080p frame of this scene is");
    println!("//   {evals_per_pixel:.2} evals/pixel x {pixels_1080:.0} pixels = {derived_1080:.3e} charged evaluations");
    println!("//   at the measured {rate:.1} ns/evaluation = {derived_seconds:.1} s on this CPU");
    println!("//   against gate R1's 16.7 ms budget - a factor of {:.0} over, on a machine and a renderer", derived_seconds * 1000.0 / 16.7);
    println!("//   with no GPU in the loop. The point is not that the CPU is slow: it is that the work is");
    println!("//   per-pixel and the cull is what makes 1080p reachable at all.");
    println!();

    // 3. What a ceiling buys at that resolution: render 1080p *under budgets* and report what survives.
    //    Bounded work, so this actually finishes; the ceiling is the knob AS5 would turn.
    let camera = camera(1920, 1080);
    let mut frame = Frame::new(camera.width, camera.height);
    println!(" ceiling (evals) | ms      | pixels drawn | undrawn  | hits kept | of the ceiling-free frame's");
    // The reference is the culled renderer, not the raw one: identical picture, and at 2.07 million
    // pixels the uncullled march would take minutes to produce the number being compared.
    let (_, uncapped) = render_bnb(&tree, &camera);
    for ceiling in [2_000_000u64, 8_000_000, 20_000_000, 45_000_000] {
        let start = Instant::now();
        let stats =
            render_bnb_budget_in(&tree, &camera, Budget(ceiling), Scan::CentreOut, &mut frame);
        let elapsed = start.elapsed();
        let drawn = stats.rays - stats.not_drawn;
        println!(
            " {:>15} | {:>7.1} | {:>13} | {:>8} | {:>9} | {:.1}% of its {} hits",
            ceiling,
            elapsed.as_micros() as f64 / 1_000.0,
            drawn,
            stats.not_drawn,
            stats.hits,
            100.0 * stats.hits as f64 / uncapped.hits.max(1) as f64,
            uncapped.hits,
        );
        assert!(
            stats.evaluations <= ceiling,
            "the ceiling {ceiling} was overran by {}",
            stats.evaluations - ceiling
        );
    }
    println!();
    println!(
        "// What this says about AS5's number: a ceiling is not a rough safety margin, it is the"
    );
    println!("// dial between 'the frame is mostly sky' and 'the frame kept its subject'. The rows above");
    println!("// are the trade at 1080p on the demo scene, in the same unit the compiler would reject on");
    println!(
        "// (`steps x nodes` per ray, bounded per frame), which is why the gate can be armed the"
    );
    println!(
        "// moment someone picks a row. Picking it is a frame-time decision and R1 has no GPU yet,"
    );
    println!("// so no row is called the default here.");
    println!("// Unchanged by this file: nothing about the field or the picture. The frames compared are");
    println!("// the same scene; only how much of it was paid for differs.");
}
