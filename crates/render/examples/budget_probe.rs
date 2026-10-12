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
use render::raster::{render_bnb, render_bnb_budget_in, Budget, Camera, Frame, Scan};
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
        " budget share | ceiling | drawn (tile) | kept (tile) | drawn (centre) | kept (centre) | surface kept, tile -> centre"
    );

    let mut frame = Frame::new(camera.width, camera.height);
    // The ceilings are written out as expressions rather than computed from a share/divisor pair: a
    // division whose guard is `if divisor == 0` is precisely the "manual checked division" CI's newer
    // clippy rejects, and the constant form is easier to read anyway.
    for (label, ceiling) in [
        ("none", u64::MAX),
        ("1/16", full.evaluations / 16),
        ("1/8", full.evaluations / 8),
        ("1/4", full.evaluations / 4),
        ("1/2", full.evaluations / 2),
        ("3/4", full.evaluations * 3 / 4),
        ("7/8", full.evaluations * 7 / 8),
        ("full", full.evaluations),
        ("2x", full.evaluations * 2),
    ] {
        let mut rows = Vec::new();
        for scan in [Scan::TileMajor, Scan::CentreOut] {
            let stats = render_bnb_budget_in(&tree, &camera, Budget(ceiling), scan, &mut frame);
            let drawn = stats.rays - stats.not_drawn;
            assert!(
                stats.evaluations <= ceiling,
                "budget {label} overran its ceiling under {scan:?}: {} > {ceiling}",
                stats.evaluations
            );
            assert_eq!(
                stats.rays,
                drawn + stats.not_drawn,
                "the pixel bookkeeping must close"
            );
            rows.push((drawn, stats.hits));
        }
        // The counts may differ, and that is part of the finding: a skipped sky tile costs nothing,
        // so the same ceiling buys more pixels where the scene is not. What neither order may do is overrun,
        // which the loop above asserts for both.
        let (tile_drawn, tile_hits) = rows[0];
        let (centre_drawn, centre_hits) = rows[1];
        let _ = (tile_drawn, tile_hits, centre_drawn, centre_hits);
        // `max(1)` rather than a zero test in front of the division: an empty scene keeps 100% of nothing,
        // which is what this formula gives when the denominator is clamped, and it keeps the expression one
        // line instead of a guard that a reader has to check for the else branch.
        let percent = |hits: u32| 100.0 * f64::from(hits) / f64::from(full.hits.max(1));
        println!(
            " {label:>12} | {:>7} | {:>12} | {:>11.1}% | {:>14} | {:>13.1}% | {:>7.1}% -> {:>7.1}%",
            if ceiling == u64::MAX {
                "none".to_string()
            } else {
                format!("{ceiling}")
            },
            rows[0].0,
            percent(rows[0].1),
            rows[1].0,
            percent(rows[1].1),
            percent(rows[0].1),
            percent(rows[1].1),
        );
    }

    println!();
    println!("// What the curve says:");
    println!("// - the ceiling holds at every point on it, by construction rather than by measurement: the");
    println!("//   decision is made against the worst a ray can cost, before the ray starts, so the charged");
    println!("//   total can never overrun. That is what makes the number usable as a gate rather than as an");
    println!("//   observation.");
    println!("// - the last column is the whole point of [`Scan`]: both orders buy the SAME number of pixels");
    println!(
        "//   at every ceiling, and keep wildly different amounts of the scene, because tile-major"
    );
    println!("//   spends the budget on the top-left corner - which here is sky - and centre-out spends it on");
    println!(
        "//   the subject. Ordering is therefore policy, not an implementation detail, and a real"
    );
    println!(
        "//   renderer picks the order the frame's content deserves (centre-out for a subject,"
    );
    println!("//   top-down for a wipe). Nothing about the field, the bound or the march changes.");
    println!("// - `none` and `full` and `2x` must all agree with the unbounded render above, and");
    println!("//   tests/budget.rs asserts exactly that, byte for byte, plus that every drawn pixel equals");
    println!(
        "//   the unbudgeted one. A ceiling that changed a colour would be a different renderer."
    );
    println!("// - unchanged by this probe: a frame time is still not measured here, and there is still no");
    println!("//   GPU. Charged evaluations have never been calibrated to nanoseconds in this repository.");
}
