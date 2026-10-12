//! Enclosure and the early-out, tested the only way that matters: **a culled frame must be the same
//! frame.**
//!
//! The bound arithmetic is not the property. If an enclosure were too small, the failure would be a
//! picture with a piece missing - which is exactly what a render looks like when it is right, so no
//! amount of staring at an image catches it and no assertion about radii catches it either unless the
//! radius is checked against the field itself. Two tests therefore carry the weight here:
//! `every_bounded_scene_holds_all_its_surface` samples the evaluator for inside points and requires the
//! enclosure to contain them (the evaluator is the oracle, not this file's arithmetic), and
//! `culling_changes_the_cost_and_nothing_else` renders the same camera twice and compares pixels, depth
//! and hit count while requiring the evaluation count to drop.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_CYLINDER, NODE_MIRROR, NODE_PLANE, NODE_REPEAT, NODE_ROUNDED_BOX, NODE_SMOOTH_UNION,
    NODE_SPHERE, NODE_SUBTRACT, NODE_TORUS, NODE_UNION,
};
use render::cull::{enclose, ray_may_hit, scene_bounds, scene_enclosure, Enclosure};
use render::raster::{render, render_bnb, render_culled, Camera};
use render::{sdf_eval, validate, Tree, ROOT};

const SIDE: u32 = 32;

fn leaf(kind: u16, params_first: u16, params: usize, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, 0),
        1,
        pack_first_and_flags(params_first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(params as u16),
    ])
}

fn branch(kind: u16, arity: u16, subtree_len: u32, params_first: u16, params: usize) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree_len,
        pack_first_and_flags(params_first, FLAG_IS_ROOT),
        pack_params_len(params as u16),
    ])
}

fn sphere_at(centre: [f32; 3], radius: f32) -> Tree {
    Tree {
        nodes: vec![leaf(NODE_SPHERE, 0, 4, true)],
        params: vec![radius, centre[0], centre[1], centre[2]],
    }
}

/// `A` minus `B`, both spheres: the shape whose enclosure is `A`'s, which is the one an implementation
/// could get wrong by taking the union and thereby promising more than it needs to.
fn subtract(r_big: f32, r_small: f32, offset: f32) -> Tree {
    Tree {
        nodes: vec![
            branch(NODE_SUBTRACT, 2, 3, 0, 0),
            leaf(NODE_SPHERE, 0, 4, false),
            leaf(NODE_SPHERE, 4, 4, false),
        ],
        params: vec![r_big, 0.0, 0.0, 0.0, r_small, offset, 0.0, 0.0],
    }
}

fn union(offset: f32) -> Tree {
    Tree {
        nodes: vec![
            branch(NODE_UNION, 2, 3, 0, 0),
            leaf(NODE_SPHERE, 0, 4, false),
            leaf(NODE_SPHERE, 4, 4, false),
        ],
        params: vec![
            1.0, 0.0, 0.0, 0.0, //
            0.5, offset, 0.0, 0.0,
        ],
    }
}

fn smooth_union(k: f32, offset: f32) -> Tree {
    Tree {
        nodes: vec![
            branch(NODE_SMOOTH_UNION, 2, 3, 0, 1),
            leaf(NODE_SPHERE, 1, 4, false),
            leaf(NODE_SPHERE, 5, 4, false),
        ],
        params: vec![
            k, //
            1.0, 0.0, 0.0, 0.0, //
            1.0, offset, 0.0, 0.0,
        ],
    }
}

fn mirrored_sphere() -> Tree {
    Tree {
        nodes: vec![
            branch(NODE_MIRROR, 1, 2, 0, 1),
            leaf(NODE_SPHERE, 1, 4, false),
        ],
        params: vec![
            1.0, //
            1.0, 0.0, 2.5, 0.0,
        ],
    }
}

fn camera_at(origin: [f32; 3]) -> Camera {
    Camera {
        origin,
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: SIDE,
        height: SIDE,
        max_t: 20.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

/// Every point the field calls interior must be inside the claimed sphere. The evaluator is the oracle,
/// so this is a test of the bound and not of this file's arithmetic.
fn assert_holds_the_surface(tree: &Tree, label: &str) {
    validate(tree).unwrap_or_else(|error| panic!("{label} must validate: {error:?}"));
    let enclosure = match scene_enclosure(tree) {
        Enclosure::Bounded { centre, radius } => (centre, radius),
        Enclosure::Unbounded => panic!("{label} has a bound this file should derive, and refused"),
    };
    let mut inside = 0usize;
    for ix in 0..17 {
        for iy in 0..17 {
            for iz in 0..17 {
                let p = [
                    -4.0 + ix as f32 * 0.5,
                    -4.0 + iy as f32 * 0.5,
                    -4.0 + iz as f32 * 0.5,
                ];
                if sdf_eval(tree, ROOT, p) <= 0.0 {
                    inside += 1;
                    let dx = p[0] - enclosure.0[0];
                    let dy = p[1] - enclosure.0[1];
                    let dz = p[2] - enclosure.0[2];
                    let distance = (dx * dx + dy * dy + dz * dz).sqrt();
                    assert!(
                        distance <= enclosure.1,
                        "{label}: interior point {p:?} is {distance} from centre {:?}, outside the \
                         claimed radius {}",
                        enclosure.0,
                        enclosure.1
                    );
                }
            }
        }
    }
    assert!(inside > 0, "{label} produced no interior sample at all");
}

#[test]
fn every_bounded_scene_holds_all_its_surface() {
    assert_holds_the_surface(&sphere_at([1.5, -2.0, 0.5], 0.75), "sphere");
    let rounded = Tree {
        nodes: vec![leaf(NODE_ROUNDED_BOX, 0, 4, true)],
        params: vec![1.0, 0.25, 0.5, 0.1],
    };
    assert_holds_the_surface(&rounded, "rounded box");
    let cylinder = Tree {
        nodes: vec![leaf(NODE_CYLINDER, 0, 3, true)],
        params: vec![0.6, 1.4, 1.0],
    };
    assert_holds_the_surface(&cylinder, "cylinder");
    let torus = Tree {
        nodes: vec![leaf(NODE_TORUS, 0, 2, true)],
        params: vec![1.2, 0.35],
    };
    assert_holds_the_surface(&torus, "torus");
    assert_holds_the_surface(&union(3.0), "union");
    assert_holds_the_surface(&smooth_union(0.4, 2.0), "smooth union");
    assert_holds_the_surface(&subtract(1.5, 0.8, 0.9), "subtract");
    assert_holds_the_surface(&mirrored_sphere(), "mirror");
}

#[test]
fn an_unbounded_kind_refuses_the_bound_instead_of_inventing_a_box() {
    // A plane is infinite and a repeat tiles space. The honest answer is "no bound", because an invented
    // scene box would be a too-small enclosure on a scene that happens to be bigger than it.
    let plane = Tree {
        nodes: vec![leaf(NODE_PLANE, 0, 4, true)],
        params: vec![0.0, 0.0, 1.0, 0.0],
    };
    assert_eq!(scene_enclosure(&plane), Enclosure::Unbounded);

    let repeated = Tree {
        nodes: vec![
            branch(NODE_REPEAT, 1, 2, 0, 1),
            leaf(NODE_SPHERE, 1, 4, false),
        ],
        params: vec![2.0, 1.0, 0.0, 0.0, 0.0],
    };
    assert_eq!(scene_enclosure(&repeated), Enclosure::Unbounded);

    // And the refusal is felt, not swallowed: no bound means the early-out never fires, so the frame
    // still costs every step it used to.
    let (frame, stats) = render_culled(&repeated, &camera_at([0.0, 0.0, -6.0]));
    let (plain, plain_stats) = render(&repeated, &camera_at([0.0, 0.0, -6.0]));
    assert_eq!(stats.skipped, 0, "an unbounded scene cannot be culled");
    assert_eq!(frame.pixels, plain.pixels);
    assert_eq!(frame.depth, plain.depth);
    assert_eq!(stats.evaluations, plain_stats.evaluations);
}

#[test]
fn culling_changes_the_cost_and_nothing_else() {
    let scene = smooth_union(0.4, 2.0);
    let camera = camera_at([0.0, 0.0, -6.0]);
    let (plain, plain_stats) = render(&scene, &camera);
    let (culled, culled_stats) = render_culled(&scene, &camera);

    assert_eq!(
        plain.pixels, culled.pixels,
        "a culled frame dropped or added a pixel - the enclosure is too small somewhere"
    );
    assert_eq!(
        plain.depth, culled.depth,
        "a culled frame moved a surface, which is the same bug wearing depth"
    );
    assert_eq!(plain.hit_count(), culled.hit_count());
    assert_eq!(plain_stats.hits, culled_stats.hits);
    assert!(
        culled_stats.skipped > 0,
        "with a 32x32 grid aimed at a scene two units wide, some rays must be provably empty"
    );
    assert!(
        culled_stats.evaluations < plain_stats.evaluations,
        "skipping {} rays charged {} evaluations anyway",
        culled_stats.skipped,
        plain_stats.evaluations
    );
    // The saving is exactly the skipped rays' step budget: nothing subtler is being counted.
    assert_eq!(
        plain_stats.evaluations - culled_stats.evaluations,
        u64::from(culled_stats.skipped) * u64::from(camera.max_steps),
        "the report claims a saving the charge model does not explain"
    );
}

#[test]
fn a_ray_aimed_away_is_proved_empty_and_a_ray_aimed_through_is_not() {
    let scene = sphere_at([0.0, 0.0, 0.0], 1.0);
    let enclosure = scene_enclosure(&scene);

    // Straight at it.
    assert!(ray_may_hit(
        enclosure,
        [0.0, 0.0, -6.0],
        [0.0, 0.0, 1.0],
        20.0
    ));
    // Parallel and far to the side: no t brings the ray within the radius.
    assert!(!ray_may_hit(
        enclosure,
        [50.0, 0.0, -6.0],
        [0.0, 0.0, 1.0],
        20.0
    ));
    // Behind the camera: the projection would reach the centre, but the segment starts at t = 0, so the
    // clamp is doing real work and a test that only checks the line would miss this case.
    assert!(!ray_may_hit(
        enclosure,
        [0.0, 0.0, -6.0],
        [0.0, 0.0, -1.0],
        20.0
    ));
    // Short range: the sphere is at 12 units and the march stops at 5.
    assert!(!ray_may_hit(
        enclosure,
        [0.0, 0.0, -20.0],
        [0.0, 0.0, 1.0],
        5.0
    ));
    assert!(ray_may_hit(
        enclosure,
        [0.0, 0.0, -20.0],
        [0.0, 0.0, 1.0],
        25.0
    ));
    // The refusal case: no bound means no proof, and "no proof" must answer "march it".
    let plane = Tree {
        nodes: vec![leaf(NODE_PLANE, 0, 4, true)],
        params: vec![0.0, 1.0, 0.0, 0.0],
    };
    assert!(ray_may_hit(
        scene_enclosure(&plane),
        [99.0, 99.0, 99.0],
        [1.0, 0.0, 0.0],
        1.0
    ));
}

#[test]
fn the_two_bound_forms_agree_and_the_hierarchy_leaves_the_frame_untouched() {
    // Two derivations of the same rule exist in this module on purpose: `enclose` re-descends and is the
    // form the ray early-out and these tests read, `scene_bounds` is the one-pass table a march consults
    // per step. Two implementations of one contract is the divergence this repository has been burned by
    // before (the WGSL suite exists for the same reason), so they are asserted against each other rather
    // than trusted.
    for (name, tree) in [
        ("sphere", sphere_at([1.0, 2.0, -3.0], 0.75)),
        ("union", union(3.0)),
        ("blended", smooth_union(0.4, 2.0)),
        ("subtract", subtract(1.5, 0.8, 0.9)),
        ("mirror", mirrored_sphere()),
    ] {
        let recursive = scene_enclosure(&tree);
        let table = scene_bounds(&tree);
        assert_eq!(
            recursive,
            table.enclosure_of(ROOT as usize),
            "{name}: the two bound forms disagree"
        );

        let camera = camera_at([0.0, 0.0, -6.0]);
        let (plain, _) = render(&tree, &camera);
        let (bnb, bnb_stats) = render_bnb(&tree, &camera);
        assert_eq!(
            plain.pixels, bnb.pixels,
            "{name}: the per-node skip changed pixels"
        );
        assert_eq!(
            plain.depth, bnb.depth,
            "{name}: the per-node skip changed depth"
        );
        assert_eq!(plain.hit_count(), bnb_stats.hits);
    }
}

#[test]
fn a_smooth_union_is_the_one_combinator_that_may_not_be_skipped() {
    // The bound rule a skip needs is not "the sphere covers the surface" but "the field is at least the
    // distance to the sphere". A `min` keeps that; `smin` deliberately returns *less* than `min`, by up to
    // k/4, so a node below a smooth union cannot be skipped however far its sphere is. This test pins the
    // refusal on the tree where it matters, because the wrong answer here is a missing surface.
    let blended = smooth_union(0.4, 2.0);
    let bounds = scene_bounds(&blended);
    assert!(
        !bounds.respects(ROOT as usize),
        "a smooth union's root must not offer skips"
    );
    assert!(
        bounds.respects(1),
        "the sphere leaf below it is still a true distance"
    );

    let hard = union(2.0);
    let hard_bounds = scene_bounds(&hard);
    assert!(
        hard_bounds.respects(ROOT as usize),
        "the same pair under a plain union is prunable"
    );

    // And the refusal is not cosmetic: the blended scene renders identically either way.
    let camera = camera_at([0.0, 0.0, -6.0]);
    let (plain, _) = render(&blended, &camera);
    let (bnb, bnb_stats) = render_bnb(&blended, &camera);
    assert_eq!(plain.pixels, bnb.pixels);
    assert_eq!(plain.depth, bnb.depth);
    assert!(bnb_stats.hits > 0, "the blend should still be visible");
}

#[test]
fn the_counter_measures_the_thing_the_step_count_cannot() {
    // Two spheres, one in front of the camera and one off to the side. The march's step count is
    // identical with and without the per-node skip - a skipped subtree returns the distance the walk
    // would have returned anyway - so the only witness is nodes entered per frame. This asserts both
    // what the counter means (exactly one visit per node per field walk, unpruned) and that pruning
    // strictly reduces it on the same frame.
    let tree = union(3.0);
    let nodes = tree.nodes.len() as u64;
    let camera = camera_at([0.0, 0.0, -6.0]);

    let (plain_frame, plain) = render(&tree, &camera);
    let (_, unpruned) = render_culled(&tree, &camera);
    let (bnb_frame, pruned) = render_bnb(&tree, &camera);

    assert_eq!(
        plain.visits, 0,
        "the unculed path has no table, so it reports nothing rather than a number it did not measure"
    );
    // Exactly one visit per node per field walk, which is what the counter means. It cannot be pinned to
    // `evaluations` as an equality, because a missed ray is charged the whole step budget whether or not
    // the march used it - the same upper bound doc 04 records - so the relation is divisibility one way
    // and an inequality the other.
    assert_eq!(
        unpruned.visits % nodes,
        0,
        "a counting walk enters every node or none of it, never a partial subtree"
    );
    assert!(
        unpruned.visits / nodes <= unpruned.evaluations,
        "walks cannot exceed the steps charged: {} over {}",
        unpruned.visits / nodes,
        unpruned.evaluations
    );
    assert_eq!(unpruned.evaluations, pruned.evaluations);
    assert!(
        pruned.visits < unpruned.visits,
        "with a sphere 3 units to the side, some rays must provably not need it: {} vs {}",
        pruned.visits,
        unpruned.visits
    );
    assert_eq!(plain_frame.pixels, bnb_frame.pixels);
    assert_eq!(plain_frame.depth, bnb_frame.depth);
}

#[test]
fn a_subtract_keeps_the_first_operands_bound_rather_than_the_ones_the_carve_left() {
    // A difference is a subset of A, so A's sphere is conservative and correct. Asserting the radius is
    // A's rather than something tighter pins the design decision: this module bounds by set relation,
    // not by evaluating what survived, which is the part a later GPU path must agree with.
    let scene = subtract(1.5, 0.8, 0.9);
    let Enclosure::Bounded { centre, radius } = scene_enclosure(&scene) else {
        panic!("a subtract of two spheres is bounded");
    };
    assert_eq!(centre, [0.0, 0.0, 0.0]);
    assert!(
        (radius - 1.5).abs() < 1e-6,
        "the bound should be A's radius, and it reported {radius}"
    );
    // The per-node form of the same pass, because a later BVH walks children: A is the sphere at the
    // origin with r 1.5, B is the carving sphere at `offset`, and the root takes A's bound by set
    // relation, not either one's by accident.
    let Enclosure::Bounded {
        radius: a_radius,
        centre: a_centre,
    } = enclose(&scene, 1)
    else {
        panic!("a sphere leaf is bounded");
    };
    assert_eq!(a_centre, [0.0, 0.0, 0.0]);
    assert!((a_radius - 1.5).abs() < 1e-6);
    let Enclosure::Bounded {
        radius: b_radius,
        centre: b_centre,
    } = enclose(&scene, 2)
    else {
        panic!("the carving sphere is bounded too");
    };
    assert_eq!(b_centre, [0.9, 0.0, 0.0]);
    assert!((b_radius - 0.8).abs() < 1e-6);
}

#[test]
fn a_smooth_unions_envelope_costs_its_own_k_and_nothing_more() {
    // The blended surface bulges outward from either child by at most k, so the bound has to include it.
    // Without the growth, the pair below at 2.0 apart with k = 0.4 would be under-bounded exactly where
    // the blend is, which is the too-small enclosure the whole file exists to avoid.
    let tight = smooth_union(0.4, 2.0);
    let Enclosure::Bounded { radius, .. } = scene_enclosure(&tight) else {
        panic!("two spheres blended are bounded");
    };
    let plain = union(2.0);
    let Enclosure::Bounded {
        radius: plain_radius,
        ..
    } = scene_enclosure(&plain)
    else {
        panic!("two spheres unioned are bounded");
    };
    assert!(
        radius > plain_radius,
        "the blend's envelope is missing from the bound: {radius} vs {plain_radius}"
    );
    assert!((radius - plain_radius) - 0.4 < 1e-6, "k was spent twice");
    assert_holds_the_surface(&tight, "blended pair");
}

#[test]
fn the_frame_the_cull_produces_is_the_frame_the_marcher_produced_all_along() {
    // A second camera, closer, so the scene fills most of the frame. The point of running the equality
    // again here rather than once is that the earlier grid has lots to skip: agreement while skipping
    // almost nothing is the other half of "culling changed the cost and nothing else".
    let scene = subtract(1.5, 0.8, 0.9);
    let camera = camera_at([0.0, 0.0, -3.0]);
    let (plain, plain_stats) = render(&scene, &camera);
    let (culled, culled_stats) = render_culled(&scene, &camera);
    assert_eq!(plain.pixels, culled.pixels);
    assert_eq!(plain.depth, culled.depth);
    assert!(
        plain_stats.hits > 0,
        "a camera three units from a sphere of radius 1.5 must find surface"
    );
    assert!(
        culled_stats.skipped * 4 < plain_stats.rays,
        "aimed into a scene that fills the frame, the bound should still be skipping a minority of rays \
         - it skipped {} of {}",
        culled_stats.skipped,
        plain_stats.rays
    );
}
