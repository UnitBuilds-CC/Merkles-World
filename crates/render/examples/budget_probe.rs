//! What a frame work budget buys, measured as the fraction of the scene that survives the cut.
//!
//! `cargo run --release --example budget_probe -p render`
//!
//! docs/04's raster probe established the shape of the cost - a third of the rays find nothing and pay
//! most of the evaluations - and `cull_probe` established how much of that a bound hierarchy removes.
//! Neither answers the question a renderer actually runs into: **given a ceiling, what do you lose?**
//! That is the budget pass's own measurement, and the answer has to be a curve rather than a promise,
//! because the ceiling is the number gate R1 will eventually be priced against and AS5 will eventually
//! reject an asset by.
//!
//! The interesting column is `surface kept`: the share of the frame's *hits* that survived the cut. A
//! budget that draws half the pixels and keeps 90% of the surface is a much better deal than one that
//! draws half and keeps half, and which of those you get is a fact about where the geometry sits on
//! screen - the walk is tile-major from the top left, so a ceiling is a statement about the frame's
//! composition as much as about its cost. That is why this prints a curve instead of a number, and why
//! ordering is policy rather than an implementation detail.
//!
//! Counts only, no milliseconds: the same integers on every CI runner, so this is evidence rather than one
//! machine's timings.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SMOOTH_UNION, NODE_SPHERE, NODE_UNION,
};
use render::raster::{render_bnb, render_bnb_budget, Budget, Camera, Frame};
use render::{validate, Tree};

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// A scene in the demo's shape: a blended pair at the centre and three separate objects spread around it.
/// The blend is there on purpose - it is the part the hierarchy cannot decompose, so the curve below is
/// measured on a scene where culling is partly available rather than on one where it is entirely.
fn scene() -> Tree {
    // Pre-order, as the grammar requires: the root joins a blended pair with a third sphere, and that
    // group with a pair of distant spheres. Nine records, and each node's own `first` points into one
    // shared parameter pool, so the whole tree is one buffer the evaluator walks without indexing twice.
    let nodes = vec![
        record(NODE_UNION, 2, 9, 0, 0, true),
        record(NODE_UNION, 2, 5, 0, 0, false),
        record(NODE_SMOOTH_UNION, 2, 3, 0, 1, false),
        record(NODE_SPHERE, 0, 1, 1, 4, false),
        record(NODE_SPHERE, 0, 1, 5, 4, false),
        record(NODE_SPHERE, 0, 1, 9, 4, false),
        record(NODE_UNION, 2, 3, 0, 0, false),
        record(NODE_SPHERE, 0, 1, 13, 4, false),
        record(NODE_SPHERE, 0, 1, 17, 4, false),
    ];
    let params = vec![
        0.4, //
        1.0, -0.9, 0.0, 0.0, //
        1.0, 0.9, 0.0, 0.0, //
        0.8, 0.0, 0.0, 0.0, //
        0.5, 6.0, 1.0, -1.0, //
        0.5, -7.0, -0.5, 2.0,
    ];
    Tree { nodes, params }
}

fn camera() -> Camera {
    Camera {
        origin: [0.0, 0.6, -4.0],
        forward: [0.0, -0.1, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: 64,
        height: 48,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

fn main() {
    let tree = scene();
    validate(&tree).expect("the probe scene must validate");
    let camera = camera();
    let (_, full) = render_bnb(&tree, &camera);

    println!(
        "// budget probe - what a ceiling costs, on a 64x48 grid. host {} / arch {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!(
        "// unbounded hierarchy render: {} charged evaluations, {} of {} rays find surface",
        full.evaluations, full.hits, full.rays
    );
    println!();
    println!(
        " budget share | charged evals | ceiling | pixels drawn | pixels undrawn | hits kept | surface kept"
    );

    let mut frame = Frame::new(camera.width, camera.height);
    for (label, share, divisor) in [
        ("none", 0u64, 0u64),
        ("1/16", 1, 16),
        ("1/8", 1, 8),
        ("1/4", 1, 4),
        ("1/2", 1, 2),
        ("3/4", 3, 4),
        ("7/8", 7, 8),
        ("full", 1, 1),
        ("2x", 2, 1),
    ] {
        let ceiling = if divisor == 0 {
            u64::MAX
        } else {
            full.evaluations * share / divisor
        };
        let stats = render_bnb_budget(&tree, &camera, Budget(ceiling), &mut frame);
        let drawn = stats.rays - stats.not_drawn;
        assert!(
            stats.evaluations <= ceiling,
            "budget {label} overran its ceiling: {} > {ceiling}",
            stats.evaluations
        );
        assert_eq!(
            stats.rays,
            drawn + stats.not_drawn,
            "the pixel bookkeeping must close"
        );
        let kept = if full.hits == 0 {
            100.0
        } else {
            100.0 * f64::from(stats.hits) / f64::from(full.hits)
        };
        println!(
            " {label:>12} | {:>13} | {:>7} | {:>12} | {:>14} | {:>9} | {:>12.1}%",
            stats.evaluations,
            if ceiling == u64::MAX {
                "none".to_string()
            } else {
                format!("{ceiling}")
            },
            drawn,
            stats.not_drawn,
            stats.hits,
            kept,
        );
    }

    println!();
    println!("// What the curve says:");
    println!("// - the ceiling holds at every point on it, by construction rather than by measurement: the");
    println!("//   decision is made against the worst a ray can cost, before the ray starts, so the charged");
    println!("//   total can never overrun. That is what makes the number usable as a gate rather than as an");
    println!("//   observation.");
    println!("// - `surface kept` is the column to read. Pixels are lost in whole tiles from the top left, so");
    println!("//   a ceiling spends its money somewhere specific, and how much of the scene survives depends");
    println!("//   on where the geometry happens to sit. A frame whose objects cluster top-left keeps more");
    println!("//   surface than one whose objects are bottom-right - which is why ordering is policy: the");
    println!("//   centre-out walk a real renderer wants would keep the subject and sacrifice the corners,");
    println!("//   and that is a change of the walk, not of the ceiling.");
    println!("// - `none` and `full` and `2x` must all agree with the unbounded render above, and");
    println!("//   tests/budget.rs asserts exactly that, byte for byte, plus that every drawn pixel equals");
    println!(
        "//   the unbudgeted one. A ceiling that changed a colour would be a different renderer."
    );
    println!("// - unchanged by this probe: a frame time is still not measured here, and there is still no");
    println!("//   GPU. Charged evaluations have never been calibrated to nanoseconds in this repository.");
}
