//! What ADR-024's hierarchy buys **inside** a region cell, where the streaming path actually marches.
//!
//! `cargo run --release --example stream_descent -p render`
//!
//! `cull_probe` measured the descent on a single tree and `region_cost` measured what a per-region bound
//! saves across cells; neither priced the case the two together produce, which is the one a real world is
//! made of: a cell whose content is spread wide enough that its own cover sphere swallows the frustum - the
//! `cull_probe` rows that prove 0.0% of rays empty - so the coarse test is doing nothing and the children
//! are the only thing left. Informational, integers only: node visits, charged evaluations, rays proved
//! empty. No milliseconds, and no frame time until a device exists (OQ-51).

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE, NODE_UNION,
};
use render::cull::counting_bounds;
use render::raster::{Budget, Camera, Frame, Scan};
use render::stream::{
    plan, render, render_bnb, render_budget, render_budget_refined, render_with_tables, Region,
    REGION_UNITS,
};
use render::Tree;

/// A right-leaning chain of plain unions over `count` spheres along x - plain because the descent's licence
/// is theirs (a smooth union refuses it, and `tests/cull.rs` keeps that honest), and hand-built pre-order
/// with honest `subtree_len` because the validator refuses a record that lies about its own extent.
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

fn chain(count: usize, spacing: f32, x: f32, nodes: &mut Vec<NodeRecord>, params: &mut Vec<f32>) {
    let root = nodes.is_empty();
    if count == 1 {
        nodes.push(NodeRecord([
            pack_kind_and_arity(NODE_SPHERE, 0),
            1,
            pack_first_and_flags(params.len() as u16, if root { FLAG_IS_ROOT } else { 0 }),
            pack_params_len(4),
        ]));
        params.extend_from_slice(&[0.8, x, 0.0, 0.0]);
        return;
    }
    nodes.push(NodeRecord([
        pack_kind_and_arity(NODE_UNION, 2),
        (2 * count - 1) as u32,
        pack_first_and_flags(0, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(0),
    ]));
    chain(1, spacing, x, nodes, params);
    chain(count - 1, spacing, x + spacing, nodes, params);
}

fn main() {
    println!("// the descent, applied per region inside the streaming march");
    println!(
        "// host {} / arch {}; a cell is {:.0} units, the camera stands in the middle cell",
        std::env::consts::OS,
        std::env::consts::ARCH,
        REGION_UNITS
    );
    println!();
    println!(" spheres | nodes | rays | plain evals | descent evals | skipped plain -> descent | hits | visits counted -> pruned | node work avoided | evals removed per bound record built");
    let camera = Camera {
        origin: [0.0, 0.0, -6.0],
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: 64,
        height: 48,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 96,
    };
    // Four shapes for one cell: a tight ridge (the cover sphere is nearly useful), a wide one (it is not),
    // and two denser ones, which is the case a region of base terrain actually is.
    for (count, spacing) in [(9usize, 1.2f32), (9, 8.0), (33, 8.0), (65, 6.0)] {
        let regions = [Region {
            cell: [0, 0, 0],
            tree: ridge(count, spacing),
        }];
        let selected = plan(&camera, &regions);
        let (plain_frame, plain) = render(&camera, &regions, &selected);
        let counting = [counting_bounds(&regions[0].tree)];
        let (counted_frame, counted) = render_with_tables(&camera, &regions, &selected, &counting);
        let (bnb_frame, bnb) = render_bnb(&camera, &regions, &selected);
        // The instrument asserts its own premise, because a probe that silently rendered three different
        // pictures would report a saving that is really a different frame.
        assert_eq!(
            plain_frame.pixels, bnb_frame.pixels,
            "the descent changed the picture at {count} spheres"
        );
        assert_eq!(plain_frame.depth, bnb_frame.depth);
        assert_eq!(counted_frame.pixels, plain_frame.pixels);
        assert_eq!(plain.hits, bnb.hits);
        let avoided = if counted.visits > 0 {
            1.0 - (bnb.visits as f64 / counted.visits as f64)
        } else {
            0.0
        };
        // The descent's own fixed cost, which docs/04 claimed as "O(nodes) per frame" without pricing it:
        // one bound record per node of every selected region, built once per frame. Dividing the evaluations
        // it removes by the records it builds turns that claim into a ratio a reader can judge.
        let records: u64 = selected
            .iter()
            .map(|&index| regions[index].tree.nodes.len() as u64)
            .sum();
        let removed = plain.evaluations.saturating_sub(bnb.evaluations);
        let per_record = removed / records.max(1);
        println!(
            " {:>7} | {:>5} | {:>4} | {:>11} | {:>13} | {:>17} | {:>4} | {:>22} | {:>17.1}% | {:>17}",
            count,
            2 * count - 1,
            plain.rays,
            plain.evaluations,
            bnb.evaluations,
            format!("{} -> {}", plain.skipped, bnb.skipped),
            bnb.hits,
            format!("{} -> {}", counted.visits, bnb.visits),
            avoided * 100.0,
            format!("{} ({}/{})", per_record, removed, records),
        );
    }
    println!();
    println!(
        "// Two columns to read together. `skipped` is the ray-level prize: a ray the cell's cover"
    );
    println!("// sphere permits but that no child can reach is proved empty before the march starts, so it");
    println!(
        "// costs nothing at all - which is why the evaluation column moves and not only the visit"
    );
    println!("// column. `node work avoided` is the step-level prize, counted against a table that prunes");
    println!("// nothing, so the two walks differ only in what they decline to enter.");
    println!("// What this cannot say: a frame time. Evaluations are not nanoseconds, and the tight-ridge");
    println!(
        "// row is the honest counter-example - where the cover sphere already fits, the hierarchy"
    );
    println!("// buys close to nothing and still costs a table per region per frame.");

    // The curve docs/04 named as the next question rather than pretending to answer: what share of a
    // *streamed* frame's surface survives a given ceiling, and how much of that is a fact about the walk
    // order rather than about the work. Measured against the wide ridge - the case the descent exists for -
    // and charged against its own unbudgeted cost, so the fractions mean "of what this frame actually
    // needs", not of a budget someone picked.
    println!();
    println!(
        "// the same ceiling spent across cells: wide ridge (33 spheres, spacing 8.0), one cell"
    );
    let wide = [Region {
        cell: [0, 0, 0],
        tree: ridge(33, 8.0),
    }];
    let wide_selected = plan(&camera, &wide);
    let (full_frame, full_wide) = render_bnb(&camera, &wide, &wide_selected);
    println!();
    println!(" ceiling | order | charged | drawn pixels | rays proved empty | hits kept | frames to finish (measured carry)");
    for (label, num, den) in [
        ("1/8", 1u64, 8u64),
        ("1/4", 1, 4),
        ("1/2", 1, 2),
        ("3/4", 3, 4),
    ] {
        let ceiling = full_wide.evaluations * num / den;
        for (order_name, scan) in [
            ("centre-out", Scan::CentreOut),
            ("tile-major", Scan::TileMajor),
        ] {
            let (frame, stats) =
                render_budget(&camera, &wide, &wide_selected, Budget(ceiling), scan);
            // The example asserts its own premise: the ceiling holds, and what it drew is the full frame's
            // own pixels rather than a near miss. An earlier version asserted neither, and both are the
            // claims a reader would otherwise have to take on faith.
            assert!(
                stats.evaluations <= ceiling,
                "a ceiling of {ceiling} was overran by the {order_name} walk: {}",
                stats.evaluations
            );
            for index in 0..frame.depth.len() {
                if frame.depth[index] == Frame::NOT_DRAWN_DEPTH {
                    continue;
                }
                assert_eq!(
                    frame.depth[index], full_frame.depth[index],
                    "pixel {index} moved once a ceiling was applied ({order_name})"
                );
                assert_eq!(
                    &frame.pixels[index * 3..index * 3 + 3],
                    &full_frame.pixels[index * 3..index * 3 + 3]
                );
            }
            let drawn = u64::from(stats.rays) - u64::from(stats.not_drawn);
            let kept = 100.0 * stats.hits as f64 / full_wide.hits.max(1) as f64;
            // MEASURED, not derived: refine the same grid at this ceiling until nothing is short of, and
            // count the frames. The cap is here so that a ceiling below one ray's worst case - which draws
            // nothing, forever - reports itself instead of hanging the probe.
            let mut carry = Frame::new(camera.width, camera.height);
            let mut carry_frames = 0u32;
            let mut carry_charge = 0u64;
            loop {
                carry_frames += 1;
                let carried = render_budget_refined(
                    &camera,
                    &wide,
                    &wide_selected,
                    Budget(ceiling),
                    scan,
                    &mut carry,
                );
                assert!(
                    carried.evaluations <= ceiling,
                    "frame {carry_frames} of the carry breached a ceiling of {ceiling}: {}",
                    carried.evaluations
                );
                carry_charge += carried.evaluations;
                if carried.not_drawn == 0 || carry_frames >= 2048 {
                    break;
                }
            }
            assert!(
                carry_charge <= full_wide.evaluations,
                "the carry charged {carry_charge} across {carry_frames} frames against the single pass's {}",
                full_wide.evaluations
            );
            assert_eq!(
                carry.depth, full_frame.depth,
                "{carry_frames} frames of carry did not converge to the unbudgeted depths"
            );
            assert_eq!(carry.pixels, full_frame.pixels);
            let frames_label = if carry_frames >= 2048 {
                ">2048 (never converged)".to_string()
            } else {
                format!("{carry_frames} frames, {carry_charge} charged")
            };
            println!(
                " {:>7} | {:>11} | {:>7} | {:>12} | {:>17} | {:>9} | {:>30}",
                label,
                order_name,
                stats.evaluations,
                drawn,
                stats.skipped,
                format!("{kept:.1}%"),
                frames_label,
            );
        }
    }
    println!();
    println!(
        "// `frames to finish` is MEASURED now: the same grid is refined at that ceiling until nothing is"
    );
    println!("// short of, counting the frames and the cumulative charge, with convergence and the per-frame");
    println!("// ceiling asserted in the loop above. The derived version - the grid divided by the pixels one");
    println!(
        "// frame decided - was printed here first, and it is wrong in both directions: at 1/8 it"
    );
    println!("// predicted 140 centre-out and 3 tile-major, and the measured carry takes 57 frames in either");
    println!("// order. And the pairs disagree with the expectation this file was written to confirm: at a");
    println!("// given ceiling the two orders draw 11x to 56x different numbers of pixels while keeping the");
    println!("// SAME share of surface (0.8, 11.3, 37.1, 66.9 in both). The descent makes an off-subject ray");
    println!("// cost nothing, so a walk that spends its ceiling on cheap tiles finalises the grid for free,");
    println!("// and the expensive rays near the subject are the same handful either way. `pop_in` measured");
    println!("// the opposite advice on one tree - 25.1% of hits first frame centre-out against 1.4% - which");
    println!("// is the difference between a scene that fills the frame and a cell that mostly isn't one.");
}
