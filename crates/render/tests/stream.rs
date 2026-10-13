//! Region streaming, tested the way every culling structure in this crate is tested: **the picture may not
//! move**, and the saving has to be a number rather than an argument.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE, NODE_UNION,
};
use render::cull::{counting_bounds, SceneBounds};
use render::raster::{render, render_culled, Budget, Camera, Frame, Scan};
use render::stream::{
    plan, render as render_regions, render_bnb, render_budget, render_with_tables, Region,
    REGION_UNITS,
};
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

#[test]
fn a_cell_a_billion_out_draws_the_same_picture_as_the_same_cell_at_zero() {
    // Gate C1's coordinate arithmetic puts a region origin at `cell * 4096`, so a deep world reaches world
    // coordinates an `f32` can only hold in steps of 64 units at 10^9. Docs/02's rule - the camera is the
    // origin of the arithmetic - is what keeps a shape that small from being rounded away, and the earlier
    // order (form the world point, subtract the cell afterwards) did exactly that: `examples/deep_zoom.rs`
    // measured 1,280 of 4,096 pixels of a snout deleted at a 1e-6 tolerance. This is that claim as a test,
    // with the shape moved a whole billion units along x and the requirement that nothing may move: same
    // bytes, same depths, same charged work.
    let cell = (1.0e9_f32 / REGION_UNITS) as i64;
    let local = [Region {
        cell: [0, 0, 0],
        tree: pair([0.0, 0.0, 0.0]),
    }];
    let far = [Region {
        cell: [cell, 0, 0],
        tree: pair([0.0, 0.0, 0.0]),
    }];
    let origin = far[0].origin();
    let local_camera = camera([0.0, 0.0, -6.0], 40.0);
    let far_camera = camera([origin[0], 0.0, -6.0], 40.0);
    assert_eq!(
        far_camera.origin[0] - origin[0],
        0.0,
        "the fixture's premise: a camera sitting on its cell must have an exact relative origin"
    );

    let (want, want_stats) = render_regions(&local_camera, &local, &plan(&local_camera, &local));
    let (got, got_stats) = render_regions(&far_camera, &far, &plan(&far_camera, &far));
    assert_eq!(
        got.pixels, want.pixels,
        "a far cell shaded differently - the sample lost digits on the way to the field"
    );
    assert_eq!(
        got.depth, want.depth,
        "a far cell's surface moved, which is the same loss reported as geometry"
    );
    assert_eq!(got_stats.evaluations, want_stats.evaluations);
    assert_eq!(got_stats.hits, want_stats.hits);
    assert!(
        want_stats.hits > 0,
        "the fixture has to actually show the pair, or this test proves nothing"
    );
}

#[test]
fn the_descent_inside_a_region_avoids_node_work_and_moves_no_pixel() {
    // ADR-024's hierarchy was only ever applied *within* one tree, and the streaming path is where a real
    // world lives, so `render_bnb` asks the same descent of each region's own children - in that region's
    // coordinates, from that region's eye. The case that shows it is the one the region bound is blind to:
    // a ridge of spheres spread along x, wide enough that the cell's single cover sphere swallows the whole
    // frustum (which is exactly what `cull_probe` measured at 0.0% on the line arrangement) while each
    // child's own bound proves nearly everything. Nine spheres per cell, three cells, and a camera whose
    // Nine spheres in the camera's own cell, spread along x, and a frame about one unit wide at the target:
    // a side ray has no business walking the whole ridge.
    let regions = [Region {
        cell: [0, 0, 0],
        tree: ridge(9, 2.0),
    }];
    let camera = camera([0.0, 0.0, -6.0], 40.0);
    let selected = plan(&camera, &regions);
    assert_eq!(selected.len(), 1, "the cell the camera sits in is in range");

    let (plain, plain_stats) = render_regions(&camera, &regions, &selected);
    let (bnb, bnb_stats) = render_bnb(&camera, &regions, &selected);
    // The picture may not move. Not "may not move much": a skip that changes a pixel is a wrong skip, and
    // the crate has already been caught once by exactly that difference showing up as one pixel and one hit.
    assert_eq!(bnb.pixels, plain.pixels, "the descent changed the picture");
    assert_eq!(bnb.depth, plain.depth, "the descent moved a surface");
    assert_eq!(bnb_stats.hits, plain_stats.hits);
    // The saving turns out to be larger than node work, and this is where that showed up: a ray the cell's
    // own cover sphere permits but that no child can reach is now proved empty before the march starts, so
    // it is charged nothing at all. The law the descent must obey is therefore one-directional - it may
    // convert permitted rays into skipped ones, and may never take a hit or a pixel away - which is what
    // these two lines plus the pixel and depth equality above say together.
    assert!(
        bnb_stats.evaluations <= plain_stats.evaluations,
        "the descent charged {} evaluations against the plain walk's {} - it may only remove work",
        bnb_stats.evaluations,
        plain_stats.evaluations
    );
    assert!(
        bnb_stats.skipped > plain_stats.skipped,
        "a ridge wide enough that its own cover sphere is useless should have the children proving emptiness \
         for rays the region bound merely permits: {} skipped against the plain walk's {}",
        bnb_stats.skipped,
        plain_stats.skipped
    );

    // The baseline that prunes nothing, so the saving is a measured difference rather than an argument.
    let counting: Vec<SceneBounds> = selected
        .iter()
        .map(|&index| counting_bounds(&regions[index].tree))
        .collect();
    let (counted_frame, counted) = render_with_tables(&camera, &regions, &selected, &counting);
    assert_eq!(
        counted_frame.pixels, plain.pixels,
        "a counting table must answer the identical field, or the comparison is comparing two renderers"
    );
    assert!(
        counted.visits > 0,
        "the baseline tallied nothing, so the ratio below would be meaningless"
    );
    let avoided = 1.0 - (bnb_stats.visits as f64 / counted.visits as f64);
    assert!(
        avoided > 0.5,
        "a ridge wide enough for its own cover sphere to be useless should have the children carry the \
         frame: {} avoided of {} node visits ({:.1}%)",
        counted.visits - bnb_stats.visits,
        counted.visits,
        avoided * 100.0
    );
}

/// A right-leaning chain of plain unions over `count` unit-ish spheres along x, starting at x = -8 and
/// `spacing` apart. Plain unions because the descent's licence is theirs (a smooth union refuses it, and
/// `tests/cull.rs` keeps that honest), and pre-order with correct `subtree_len` because the validator
/// refuses a record that lies about its own extent - which is the point of hand-building it here rather
/// than lowering an authored shape.
fn ridge(count: usize, spacing: f32) -> Tree {
    let mut nodes = Vec::new();
    let mut params = Vec::new();
    chain(count, spacing, -8.0, &mut nodes, &mut params);
    Tree { nodes, params }
}

fn chain(count: usize, spacing: f32, x: f32, nodes: &mut Vec<NodeRecord>, params: &mut Vec<f32>) {
    if count == 1 {
        nodes.push(NodeRecord([
            pack_kind_and_arity(NODE_SPHERE, 0),
            1,
            pack_first_and_flags(
                params.len() as u16,
                if nodes.is_empty() { FLAG_IS_ROOT } else { 0 },
            ),
            pack_params_len(4),
        ]));
        params.extend_from_slice(&[0.8, x, 0.0, 0.0]);
        return;
    }
    nodes.push(NodeRecord([
        pack_kind_and_arity(NODE_UNION, 2),
        (2 * count - 1) as u32,
        pack_first_and_flags(0, if nodes.is_empty() { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(0),
    ]));
    chain(1, spacing, x, nodes, params);
    chain(count - 1, spacing, x + spacing, nodes, params);
}

#[test]
fn the_ceiling_across_cells_holds_and_every_pixel_it_drew_matches_the_full_frame() {
    // `raster` has spent a frame budget within one tree for a while; the streamed frame is the one the demo
    // actually composes, and a ceiling across cells is a different claim unless the cut is provably the same
    // law. So: the decision is made against the worst a ray can cost *before it starts* (so the ceiling
    // holds by construction, at every level including zero and double the full cost), every pixel the cut
    // reached equals the unbudgeted frame byte for byte, and every pixel it did not is the sentinel rather
    // than a plausible sky someone would have to squint at to notice was never marched.
    let regions = [
        Region {
            cell: [0, 0, 0],
            tree: ridge(9, 8.0),
        },
        Region {
            cell: [0, 0, 1],
            tree: sphere(0.9, [0.0, 0.0, 0.0]),
        },
    ];
    let camera = camera([0.0, 0.0, -6.0], 40.0);
    let selected = plan(&camera, &regions);
    let (full, full_stats) = render_bnb(&camera, &regions, &selected);

    for level in [
        0u64,
        8,
        64,
        512,
        4096,
        full_stats.evaluations,
        full_stats.evaluations * 2,
    ] {
        let (frame, stats) =
            render_budget(&camera, &regions, &selected, Budget(level), Scan::CentreOut);
        assert!(
            stats.evaluations <= level,
            "a ceiling of {level} was overran: {} charged",
            stats.evaluations
        );
        let mut drawn = 0usize;
        for index in 0..frame.depth.len() {
            if frame.depth[index] == Frame::NOT_DRAWN_DEPTH {
                assert_eq!(
                    &frame.pixels[index * 3..index * 3 + 3],
                    &Frame::NOT_DRAWN_RGB,
                    "an undrawn pixel reports a colour, so a shortfall is indistinguishable from sky"
                );
                continue;
            }
            drawn += 1;
            assert_eq!(
                frame.depth[index], full.depth[index],
                "pixel {index} moved once a ceiling was applied"
            );
            assert_eq!(
                &frame.pixels[index * 3..index * 3 + 3],
                &full.pixels[index * 3..index * 3 + 3]
            );
        }
        assert_eq!(
            u64::from(stats.rays),
            drawn as u64 + u64::from(stats.not_drawn),
            "rays must divide exactly into drawn and short of, at every ceiling"
        );
        if level == 0 {
            assert_eq!(stats.not_drawn, stats.rays, "zero draws nothing");
        }
    }

    // An unlimited ceiling is the same frame, sentinel-free, to the byte - the claim that the budget walker
    // and the full walker are one march and not two that happen to agree today.
    let (all, all_stats) = render_budget(
        &camera,
        &regions,
        &selected,
        Budget(full_stats.evaluations * 4),
        Scan::TileMajor,
    );
    assert_eq!(all_stats.not_drawn, 0);
    assert_eq!(all.depth, full.depth);
    assert_eq!(all.pixels, full.pixels);
    assert_eq!(all_stats.hits, full_stats.hits);
}

#[test]
fn a_distant_region_shows_up_at_the_angular_size_it_actually_has() {
    // What the demo attempt really ran into (OQ-56, resolved by this test rather than by a theory): nothing
    // was broken. A 1.6-unit sphere four cells away subtends about 0.0004 radians, and this camera's
    // half-height is 0.5 - a quarter of the frame's height spans 26 degrees - so the sphere covers some
    // 6 parts in a million of the pixels, and with 768 of them the honest expectation is **zero**. The
    // march and the bound are not the limit at 4,096 units; `examples/far_scale_probe.rs` walks a ray out
    // to 12,288 and hits in one step at a 1e-3 tolerance. What decides whether a region reads is its
    // angular size, so the second half of this test puts an object there at the scale a region's content
    // really is, and requires it to be seen at the depth it should have.
    let long = Camera {
        origin: [0.0, 0.0, 0.0],
        forward: [1.0, 0.0, 0.0],
        right: [0.0, 1.0, 0.0],
        up: [0.0, 0.0, 1.0],
        half_height: 0.5,
        width: 32,
        height: 24,
        max_t: 16_384.0,
        surface: 1e-2,
        max_steps: 96,
    };

    let pebble = [Region {
        cell: [1, 0, 0],
        tree: sphere(1.6, [0.0, 0.0, 0.0]),
    }];
    let selected = plan(&long, &pebble);
    assert_eq!(
        selected,
        vec![0],
        "the plan works in range, not in angular size - it must not be the thing that dropped the cell"
    );
    let (_, pebble_stats) = render_regions(&long, &pebble, &selected);
    assert_eq!(
        pebble_stats.hits, 0,
        "a sub-pixel object at this distance must not be invented out of rounding"
    );

    let hill = [Region {
        cell: [1, 0, 0],
        tree: sphere(400.0, [0.0, 0.0, 0.0]),
    }];
    let selected = plan(&long, &hill);
    let (frame, hill_stats) = render_regions(&long, &hill, &selected);
    // The arithmetic the expectation comes from, rather than a vibe: a 400-unit radius at 4,096 subtends
    // 0.0977 radians, and one pixel of this 32x24 grid spans 1.0 / 24 = 0.0417 in the vertical, so the
    // disc is about 4.7 pixels across and pi * 2.35^2 ~= 17 pixels. Anything in that neighbourhood is the
    // frame behaving; zero would be the bug the demo run looked like.
    assert!(
        hill_stats.hits >= 9 && hill_stats.hits <= 30,
        "a 400-unit feature four cells away should cover roughly 17 of these pixels, got {} of {}",
        hill_stats.hits,
        hill_stats.rays
    );
    // The depth band is the geometry, stated rather than tuned: a pixel at the frame's centre is not the
    // view axis exactly - the grid samples pixel centres, so this one is ~0.03 radians off it and meets the
    // sphere on a chord. A chord meets the surface *beyond* the near point (4,096 - 400) and *before* the
    // centre (4,096), so anything in that band is the hill being where a hill is; the exact figure at this
    // offset is 4,096 - sqrt(400^2 - 121^2) = 3,715, and the frame reports 3,712.8.
    let centre = ((long.height / 2) * long.width + long.width / 2) as usize;
    let depth = frame.depth[centre];
    assert!(
        depth > 4096.0 - 400.0 && depth < 4096.0,
        "the centre pixel should meet the hill on a chord, between its near point and its centre, saw {depth}"
    );
}
