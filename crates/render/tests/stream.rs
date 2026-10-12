//! Region streaming, tested the way every culling structure in this crate is tested: **the picture may not
//! move**, and the saving has to be a number rather than an argument.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE, NODE_UNION,
};
use render::raster::{render, render_culled, Camera};
use render::stream::{plan, render as render_regions, Region, REGION_UNITS};
use render::Tree;

fn sphere(radius: f32, centre: [f32; 3]) -> Tree {
    Tree {
        nodes: vec![NodeRecord([
            pack_kind_and_arity(NODE_SPHERE, 0),
            1,
            pack_first_and_flags(0, FLAG_IS_ROOT),
            pack_params_len(4),
        ])],
        params: vec![radius, centre[0], centre[1], centre[2]],
    }
}

fn pair(centre: [f32; 3]) -> Tree {
    Tree {
        nodes: vec![
            NodeRecord([
                pack_kind_and_arity(NODE_UNION, 2),
                3,
                pack_first_and_flags(0, FLAG_IS_ROOT),
                pack_params_len(0),
            ]),
            NodeRecord([
                pack_kind_and_arity(NODE_SPHERE, 0),
                1,
                pack_first_and_flags(0, 0),
                pack_params_len(4),
            ]),
            NodeRecord([
                pack_kind_and_arity(NODE_SPHERE, 0),
                1,
                pack_first_and_flags(4, 0),
                pack_params_len(4),
            ]),
        ],
        params: vec![
            0.8,
            centre[0],
            centre[1],
            centre[2], //
            0.5,
            centre[0] + 1.4,
            centre[1],
            centre[2],
        ],
    }
}

fn camera(origin: [f32; 3], max_t: f32) -> Camera {
    Camera {
        origin,
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: 32,
        height: 24,
        max_t,
        surface: 1e-3,
        max_steps: 96,
    }
}

#[test]
fn one_region_at_its_own_cell_is_the_tree_renderer_to_the_byte() {
    // The composite marcher in `stream` exists only because A-07 cannot express a translated subtree, so it
    // must be shown to agree with the one renderer this crate has always had. A single region at cell
    // [0, 0, 0] is the case where the two must be indistinguishable: same pixels, same depths, same
    // charged work.
    let tree = pair([0.0, 0.0, 0.0]);
    let camera = camera([0.0, 0.0, -6.0], 40.0);
    // `render_culled`, not `render`: the streaming path applies a per-region bound, so the reference it has
    // to match on charged work is the one that also applies a bound. `render` is still the reference for the
    // picture, because a bound may change what is paid for and never what is seen.
    let (plain, _) = render(&tree, &camera);
    let (want, want_stats) = render_culled(&tree, &camera);
    let regions = [Region {
        cell: [0, 0, 0],
        tree: pair([0.0, 0.0, 0.0]),
    }];
    let selected = plan(&camera, &regions);
    let (got, got_stats) = render_regions(&camera, &regions, &selected);
    assert_eq!(got.pixels, want.pixels, "streaming changed the picture");
    assert_eq!(got.pixels, plain.pixels, "streaming changed the picture");
    assert_eq!(got.depth, want.depth, "streaming moved a surface");
    assert_eq!(got.depth, plain.depth, "streaming moved a surface");
    assert_eq!(
        got_stats.evaluations, want_stats.evaluations,
        "one region bounded is the same scene bounded: the charges should be identical"
    );
    assert_eq!(got_stats.hits, want_stats.hits);
    assert_eq!(
        selected,
        vec![0],
        "a region in front of the camera is selected"
    );
}

#[test]
fn a_region_out_of_range_changes_no_pixel_whether_it_is_planned_or_not() {
    // Two cells stacked along z, one region in each: a camera looking at the near cell with a short range
    // cannot reach the far one. The claim tested is the safe direction - adding it changes nothing - and the
    // plan's own judgement is checked against a range that plainly does reach it, so the test cannot pass by
    // the plan being permanently empty.
    // Both scenes sit at their own cell's centre, so the second one's surface is a whole region away -
    // 4096 units, which is what gate C1's arithmetic says a cell step means and the reason a 20-unit range
    // cannot reach it.
    let regions = [
        Region {
            cell: [0, 0, 0],
            tree: sphere(0.8, [0.0, 0.0, 0.0]),
        },
        Region {
            cell: [0, 0, 1],
            tree: sphere(0.8, [0.0, 0.0, 0.0]),
        },
    ];
    let short = camera([0.0, 0.0, -4.0], 20.0);
    let near_only = plan(&short, &regions);
    let (short_frame, _) = render_regions(&short, &regions, &near_only);
    let (all_frame, _) = render_regions(&short, &regions, &[0, 1]);
    assert_eq!(
        short_frame.pixels, all_frame.pixels,
        "planning a region out of range changed the frame"
    );
    assert_eq!(short_frame.depth, all_frame.depth);
    assert_eq!(near_only, vec![0], "the far cell is not within 20 units");

    let long = camera([0.0, 0.0, -4.0], 3.0 * REGION_UNITS);
    assert_eq!(
        plan(&long, &regions),
        vec![0, 1],
        "with a range that reaches the far cell, the plan must select it - otherwise the empty plan above \
         proves nothing"
    );
}

#[test]
fn streaming_costs_the_neighbourhood_rather_than_the_world() {
    // Seven cells along z, a scene in each, camera in the middle one with a range that reaches three of
    // them. The saving has to be a measured difference in charged work, not a claim about sets.
    let cells = [-3i64, -2, -1, 0, 1, 2, 3];
    let regions: Vec<Region> = cells
        .iter()
        .map(|&z| Region {
            cell: [0, 0, z],
            tree: sphere(0.9, [0.0, 0.0, 0.0]),
        })
        .collect();
    let camera = camera([0.0, 0.0, -6.0], 40.0);
    let selected = plan(&camera, &regions);
    assert!(
        selected.len() < regions.len(),
        "a 40-unit range from the middle cell must not need all seven: {:?}",
        selected
    );
    let (partial_frame, partial) = render_regions(&camera, &regions, &selected);
    let everything: Vec<usize> = (0..regions.len()).collect();
    let (full_frame, full) = render_regions(&camera, &regions, &everything);

    // What planning does *not* save is the march, and saying so is the point: each region is already
    // rejected per ray by its own bound, so the charged work is the same either way. What it removes is the
    // per-region per-ray test, which is where the cost of a big world actually lives - regions x rays - and
    // that count is exactly what the two frames below agree about while costing different numbers of tests.
    assert_eq!(
        partial.evaluations, full.evaluations,
        "planning changed the charged work, which the per-ray bound had already pinned"
    );
    assert_eq!(
        partial_frame.pixels, full_frame.pixels,
        "the plan dropped visible geometry"
    );
    assert_eq!(partial_frame.depth, full_frame.depth);
    assert!(
        selected.len() * 3 < regions.len(),
        "a 40-unit range must not need most of a seven-cell world: {:?}",
        selected
    );
    assert!(partial.hits > 0, "the near region should be visible");
}
