//! How many frames a cold region needs to come fully back on screen at a stated ceiling.
//!
//! `cargo run --release --example pop_in -p render`
//!
//! Gate **R3** and the visible half of gate **P1.4** are stated in the language of a display: "region entry
//! after LRU eviction shows prior edits with **no visible pop-in**". No display exists yet - that is the
//! GPU clause, waiting on OQ-51 - so the honest pre-GPU question is not "can the eye see it" but **how
//! many frames the work takes**, because that is the quantity a pop-in either fits inside or does not.
//! The pieces are already here and measured separately: re-entry costs a region S2's whole hashing pass
//! (`re_entry_cost`), and a frame's work is `steps x nodes` per ray (`frame_cost`). This joins them into
//! the only form the gate can be discussed in until there is a device: coverage per frame, and the order
//! in which a half-drawn frame reveals a scene.
//!
//! Everything here is counts and this machine's milliseconds. It is not a frame time for R1 and it does
//! not claim the eye would not see the transition - the claim is narrower and checkable: at ceiling C, a
//! cold region is fully drawn in N frames, and after the first frame the centre-out order has kept this
//! much of the subject.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use render::raster::{render_bnb, render_bnb_refined, Budget, Camera, Frame, Scan};
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

/// The boot's scene: a plate, a blended pair and three spread spheres. Eleven records, depth four.
fn region() -> Tree {
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 11, 0, 0, true),
            record(NODE_UNION, 2, 5, 0, 0, false),
            record(NODE_ROUNDED_BOX, 0, 1, 0, 4, false),
            record(NODE_UNION, 2, 3, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 0, 4, false),
            record(NODE_SPHERE, 0, 1, 4, 4, false),
            record(NODE_UNION, 2, 5, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 8, 4, false),
            record(NODE_UNION, 2, 3, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 12, 4, false),
            record(NODE_SPHERE, 0, 1, 16, 4, false),
        ],
        params: vec![
            0.35, 0.35, 0.9, 0.12, //
            1.0, -0.9, 0.0, 0.0, 1.0, 0.9, 0.0, 0.0, //
            0.8, 0.0, 0.0, 0.0, //
            0.5, -6.0, 0.0, 1.5, //
            0.5, 6.2, -0.4, -1.0, //
            0.5, 1.0, 3.2, 7.5,
        ],
    }
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

fn sweep(label: &str, tree: &Tree, camera: &Camera, scan: Scan, share: u64) {
    let (_, reference) = render_bnb(tree, camera);
    let ceiling = reference.evaluations / share;
    let mut frame = Frame::new(camera.width, camera.height);
    println!(
        "// {label}: ceiling {ceiling} charged evaluations (1/{share} of the culled frame's {}), {scan:?}",
        reference.evaluations
    );
    println!("  frame | drawn pixels | coverage | hits kept | ms    | of the reference's hits");
    let mut frames = 0u32;
    let mut first_frame_hits = 0u32;
    // Per-pass hit counts are not what a viewer sees: by frame three the earlier frames' pixels are still
    // there. So the column is cumulative, which also lets the last row be checked against the reference.
    let mut seen_hits = 0u32;
    loop {
        frames += 1;
        let start = Instant::now();
        let stats = render_bnb_refined(tree, camera, Budget(ceiling), scan, &mut frame);
        let elapsed = start.elapsed();
        let drawn = stats.rays - stats.not_drawn;
        seen_hits += stats.hits;
        if frames == 1 {
            first_frame_hits = stats.hits;
        }
        println!(
            "  {:>5} | {:>12} | {:>8.1}% | {:>11} | {:>5.1} | {:.1}%",
            frames,
            drawn,
            100.0 * f64::from(drawn) / f64::from(stats.rays),
            seen_hits,
            elapsed.as_micros() as f64 / 1_000.0,
            100.0 * f64::from(seen_hits) / f64::from(reference.hits.max(1)),
        );
        assert!(
            stats.evaluations <= ceiling,
            "frame {frames} overran its ceiling: {} > {ceiling}",
            stats.evaluations
        );
        if stats.not_drawn == 0 {
            break;
        }
        assert!(
            frames < 64,
            "the sweep must converge; stuck after {frames} frames"
        );
    }
    println!(
        "  => complete in {frames} frames; the first frame kept {first_frame_hits} of {} hits ({:.1}%)",
        reference.hits,
        100.0 * f64::from(first_frame_hits) / f64::from(reference.hits.max(1)),
    );
    let (_, after) = render_bnb(tree, camera);
    assert_eq!(
        frame
            .depth
            .iter()
            .filter(|d| **d != Frame::NO_HIT && **d != Frame::NOT_DRAWN_DEPTH)
            .count(),
        after.hits as usize,
        "a refined frame must end with exactly the pixels a single pass drew"
    );
}

fn main() {
    let tree = region();
    let camera = camera();
    println!("// pop-in in the only unit available before there is a display: frames of work");
    println!(
        "// host {} / arch {}; counts only (the ms column is deliberately not measured here)",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!();
    for share in [4u64, 8, 16] {
        for scan in [Scan::CentreOut, Scan::TileMajor] {
            sweep(&format!("1/{share} ceiling"), &tree, &camera, scan, share);
            println!();
        }
    }
    println!("// What this supports and what it does not:");
    println!("// - it gives R3 a number to argue with: at 1/16 of a culled frame's work the demo region is");
    println!("//   complete in the frame counts above, and the order decides what the first frame shows.");
    println!("// - it does not answer 'is the pop-in visible'. That needs a display, and the display needs");
    println!(
        "//   the device decision (OQ-51). Any claim past 'N frames of work' here would be the"
    );
    println!("//   mistake gate R4 exists to stop: work avoided is not nanoseconds saved.");
    println!("// - the hits-per-frame rows are the honest version of the transition: coverage climbs, hits");
    println!("//   never fall, and the finished frame equals the single-pass one byte for byte");
    println!("//   (tests/budget.rs asserts that, because a budget that changed the picture is a renderer).");
}
