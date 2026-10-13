//! Gate P1.4's last open clause: when a region comes back, how long until its surface is on screen.
//!
//! `cargo run --release --example region_pop_in -p render`
//!
//! `pop_in.rs` measured frames-of-work inside one tree. P1.4's eviction and re-entry has since had its
//! correctness proven (the residency directory restores a re-entered region to the root it had before) and
//! its cost measured (a re-entry pays a full Merkle pass, 68,524 digests), but the gate's actual wording is
//! about the *picture*: "region entry after LRU eviction shows prior edits with no visible pop-in". That
//! needs a different instrument, because the question is not when the frame is finished but **when the
//! arriving region's own pixels are** - and a composite frame settles the cheap, provably-empty majority
//! first, so those are not the same number.
//!
//! So this attributes pixels to a region by differencing the frame with and without it - the same trick the
//! boot uses for "adding a cell re-lit N pixels" - and then refines the composite at a fixed ceiling,
//! reporting how many frames pass before a given share of *that region's* footprint has arrived. Integers
//! and percentages only: no milliseconds, and no claim about a frame time (R1 is a GPU question, OQ-51).

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE, NODE_UNION,
};
use render::raster::{Budget, Camera, Frame, Scan};
use render::stream::{plan, render_bnb, render_budget_refined, Region, REGION_UNITS};
use render::Tree;

fn sphere(radius: f32) -> Tree {
    Tree {
        nodes: vec![NodeRecord([
            pack_kind_and_arity(NODE_SPHERE, 0),
            1,
            pack_first_and_flags(0, FLAG_IS_ROOT),
            pack_params_len(4),
        ])],
        params: vec![radius, 0.0, 0.0, 0.0],
    }
}

fn chain(count: usize, spacing: f32, x: f32, nodes: &mut Vec<NodeRecord>, params: &mut Vec<f32>) {
    let flag = if nodes.is_empty() { FLAG_IS_ROOT } else { 0 };
    if count == 1 {
        nodes.push(NodeRecord([
            pack_kind_and_arity(NODE_SPHERE, 0),
            1,
            pack_first_and_flags(params.len() as u16, flag),
            pack_params_len(4),
        ]));
        params.extend_from_slice(&[0.8, x, 0.0, 0.0]);
        return;
    }
    nodes.push(NodeRecord([
        pack_kind_and_arity(NODE_UNION, 2),
        (2 * count - 1) as u32,
        pack_first_and_flags(0, flag),
        pack_params_len(0),
    ]));
    chain(1, spacing, x, nodes, params);
    chain(count - 1, spacing, x + spacing, nodes, params);
}

fn ridge(count: usize, spacing: f32) -> Tree {
    let mut nodes = Vec::new();
    let mut params = Vec::new();
    chain(
        count,
        spacing,
        -((count as f32 - 1.0) * spacing / 2.0),
        &mut nodes,
        &mut params,
    );
    Tree { nodes, params }
}

fn main() {
    // The case P1.4 describes: the camera stands in one region's cell, and another region - a hill at
    // terrain scale, because a prop at this distance is sub-pixel (OQ-56) - has just been re-entered.
    let near = Region {
        cell: [0, 0, 0],
        tree: ridge(33, 8.0),
    };
    let arriving = Region {
        cell: [1, 0, 0],
        tree: sphere(400.0),
    };
    let camera = Camera {
        origin: [0.0, 0.0, -6.0],
        forward: [1.0, 0.0, 0.0],
        right: [0.0, 1.0, 0.0],
        up: [0.0, 0.0, 1.0],
        half_height: 0.5,
        width: 64,
        height: 48,
        max_t: 3.0 * REGION_UNITS,
        surface: 1e-2,
        max_steps: 96,
    };
    let cells = [near, arriving];
    let selected = plan(&camera, &cells);
    assert_eq!(selected.len(), cells.len(), "both cells must be in range");

    // The footprint: pixels the arriving region actually changes. Rendering without it is how the boot
    // measures "re-lit", and here it is the denominator - "no visible pop-in" is about these pixels, not
    // about the frame's average progress.
    let (with_hill, with_stats) = render_bnb(&camera, &cells, &selected);
    let without = [cells[0].clone()];
    let without_selected = plan(&camera, &without);
    let (without_hill, _) = render_bnb(&camera, &without, &without_selected);
    let footprint: Vec<usize> = (0..with_hill.depth.len())
        .filter(|&i| with_hill.depth[i] != without_hill.depth[i])
        .collect();
    let rays = u64::from(camera.width * camera.height);
    let charge = u64::from(camera.max_steps) + 1;
    println!("// P1.4's pop-in clause: when does the ARRIVING region's own surface appear");
    println!(
        "// host {} / arch {}; {} rays, of which {} ({:.1}%) belong to the re-entered region",
        std::env::consts::OS,
        std::env::consts::ARCH,
        rays,
        footprint.len(),
        100.0 * footprint.len() as f64 / rays as f64
    );
    println!(
        "// the composite frame costs {} charged evaluations; one ray's worst case is {charge}",
        with_stats.evaluations
    );
    println!();
    println!(" ceiling | order | frame 1 settles | frame 1 of the arriving region | 50% arrived by | 100% arrived by | whole frame by");
    for charges in [1u64, 2, 4, 8, 16] {
        let ceiling = charge * charges;
        for (name, scan) in [
            ("centre-out", Scan::CentreOut),
            ("tile-major", Scan::TileMajor),
        ] {
            let mut frame = Frame::new(camera.width, camera.height);
            let mut whole_by = 0u32;
            let mut full_by = 0u32;
            let mut half_by = 0u32;
            let mut first_all = 0usize;
            let mut first_foot = 0usize;
            for iteration in 1..=4096 {
                let stats = render_budget_refined(
                    &camera,
                    &cells,
                    &selected,
                    Budget(ceiling),
                    scan,
                    &mut frame,
                );
                assert!(stats.evaluations <= ceiling, "ceiling {ceiling} breached");
                let arrived = footprint
                    .iter()
                    .filter(|&&i| frame.depth[i] != Frame::NOT_DRAWN_DEPTH)
                    .count();
                let settled = usize::try_from(stats.rays - stats.not_drawn).unwrap_or(usize::MAX);
                if iteration == 1 {
                    first_all = settled;
                    first_foot = arrived;
                }
                if half_by == 0 && arrived * 2 >= footprint.len() {
                    half_by = iteration;
                }
                if full_by == 0 && arrived == footprint.len() {
                    full_by = iteration;
                }
                if stats.not_drawn == 0 {
                    whole_by = iteration;
                    break;
                }
            }
            assert_eq!(
                frame.depth, with_hill.depth,
                "the carried frame did not settle to the unbudgeted picture at {charges} charges ({name})"
            );
            assert_eq!(frame.pixels, with_hill.pixels);
            let pct = |n: u32| {
                if n == 0 {
                    "never".to_string()
                } else {
                    n.to_string()
                }
            };
            println!(
                " {:>7} | {:>10} | {:>15} | {:>30} | {:>14} | {:>16} | {:>13}",
                format!("{charges} x {charge}"),
                name,
                first_all,
                format!(
                    "{} of {} ({:.0}%)",
                    first_foot,
                    footprint.len(),
                    100.0 * first_foot as f64 / footprint.len().max(1) as f64
                ),
                pct(half_by),
                pct(full_by),
                pct(whole_by),
            );
        }
    }
    println!();
    println!("// The two middle columns are the gate's sentence. A composite frame can be nearly finished");
    println!("// while the region that just arrived is barely present, because the pixels the bounds prove");
    println!("// empty cost nothing and settle first - so 'the frame is done' is the wrong thing to ask.");
    println!("// What this still cannot answer is whether the result is *visible* pop-in on a display, which");
    println!("// is R1's question and needs a device (OQ-51); it is frames, not milliseconds.");
}
