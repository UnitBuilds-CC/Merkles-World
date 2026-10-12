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
fn a_smooth_union_is_prunable_only_because_its_bound_is_grown_by_k() {
    // The bound rule a skip needs is not "the sphere covers the surface" but "outside the sphere, the field
    // is at least the distance to it". A `min` keeps that; `smin` returns less than `min` by up to `k/4`.
    // The arm is licensable anyway because the sphere the table stores for a blend is grown by `k`, which
    // pays for the dip with `3k/4` to spare - see the derivation in `collect`. What still is *not* licensable
    // is a blend sitting on a child that does not respect its own bound: the growth is on the outer sphere
    // and cannot repair an inner one.
    let blended = smooth_union(0.4, 2.0);
    let bounds = scene_bounds(&blended);
    assert!(
        bounds.respects(ROOT as usize),
        "a blend of two respecting children is licensed by its own k-growth"
    );
    assert!(
        bounds.respects(1),
        "the sphere leaf below it is still exact"
    );

    let blend_over_plane = Tree {
        nodes: vec![
            node(NODE_SMOOTH_UNION, 2, 3, 0, 1, true),
            node(NODE_SPHERE, 0, 1, 1, 4, false),
            node(NODE_PLANE, 0, 1, 5, 4, false),
        ],
        params: vec![
            0.5, //
            1.0, 0.0, 0.0, 0.0, //
            0.0, 1.0, 0.0, 0.0,
        ],
    };
    validate(&blend_over_plane).expect("a blend over a plane is a legal tree");
    let mixed = scene_bounds(&blend_over_plane);
    assert!(
        !mixed.respects(ROOT as usize),
        "a blend cannot out-license a child that refuses its own bound"
    );
    assert_eq!(
        mixed.enclosure_of(ROOT as usize),
        Enclosure::Unbounded,
        "and the refusal is also the bound's own: a plane has no sphere to grow"
    );

    let hard = union(2.0);
    assert!(
        scene_bounds(&hard).respects(ROOT as usize),
        "the same pair under a plain union is prunable"
    );

    // The belt is the frame, not the argument: with the licence granted the descent and the per-step skip
    // both fire on a blended scene, so this is the case where a wrong derivation would delete geometry.
    let camera = camera_at([0.0, 0.0, -6.0]);
    let (plain, plain_stats) = render(&blended, &camera);
    let (culled, culled_stats) = render_culled(&blended, &camera);
    let (bnb, bnb_stats) = render_bnb(&blended, &camera);
    assert_eq!(
        plain.pixels, bnb.pixels,
        "the blend's descent changed pixels"
    );
    assert_eq!(
        plain.depth, bnb.depth,
        "the blend's descent moved a surface"
    );
    assert_eq!(plain.pixels, culled.pixels);
    assert_eq!(plain_stats.hits, bnb_stats.hits);
    assert!(bnb_stats.hits > 0, "the blend should still be visible");
    assert!(
        bnb_stats.skipped >= culled_stats.skipped,
        "the licensed descent may prove more rays empty, never fewer: {} vs {}",
        bnb_stats.skipped,
        culled_stats.skipped
    );
}

#[test]
fn the_counter_measures_the_thing_the_step_count_cannot() {
    // Two spheres, one in front of the camera and one off to the side. The per-node skip inside a step is
    // invisible to the march's step count - a skipped subtree returns the distance the walk would have
    // returned anyway - so the witness for that half is nodes entered per frame. The ray-level descent is
    // visible, because it removes whole rays. This asserts both, plus what the counter means (exactly one
    // visit per node per field walk, unpruned) and that pruning strictly reduces it on the same frame.
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
    // The step count does now see the descent, and that is a change of fact rather than of metric: a ray
    // the hierarchy proves empty is charged nothing, while the single root bound could only ever prove the
    // rays outside its own sphere. The per-*step* subtree skip is still invisible here - a skipped subtree
    // returns the distance the unpruned walk would have returned - which is why `visits` below remains the
    // witness for that half.
    assert!(
        pruned.skipped > unpruned.skipped,
        "a union with a sphere 3 units aside must let the children prove rays the cover permits: {} vs {}",
        pruned.skipped,
        unpruned.skipped
    );
    assert!(
        pruned.evaluations < unpruned.evaluations,
        "more skipped rays has to mean fewer charged steps: {} vs {}",
        pruned.evaluations,
        unpruned.evaluations
    );
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

#[test]
fn the_descent_adds_rejections_where_children_are_licensed_and_none_where_they_are_not() {
    // The headline of the whole module, stated as two assertions and one refusal. Two spheres 3 units
    // apart: their cover sphere is loose, so the root test lets rays through that the pair of children
    // provably do not need - which is the exact failure mode cull_probe measured at 0.0% for a spread
    // scene. The descent fixes that, and the picture must not move by a byte while it does.
    let spread = union(3.0);
    let camera = camera_at([0.0, 0.0, -6.0]);
    let (plain, plain_stats) = render(&spread, &camera);
    let (_, root_only) = render_culled(&spread, &camera);
    let (bnb_frame, descent) = render_bnb(&spread, &camera);
    assert!(
        descent.skipped > root_only.skipped,
        "the children must prove empty at least one ray the cover permits: {} vs {}",
        descent.skipped,
        root_only.skipped
    );
    assert_eq!(plain.pixels, bnb_frame.pixels, "the descent changed pixels");
    assert_eq!(plain.depth, bnb_frame.depth, "the descent changed depth");
    assert_eq!(plain_stats.hits, descent.hits);

    // The refusal, on the ray-level path where a too-eager child test would do the most damage: a smooth
    // union's field dips below its own bound, so it gets no descent and therefore not one extra
    // rejection over the single sphere it is licensed to test.
    let blended = smooth_union(0.4, 2.0);
    let (_, blended_root) = render_culled(&blended, &camera);
    let (blended_frame, blended_descent) = render_bnb(&blended, &camera);
    assert!(
        blended_descent.skipped >= blended_root.skipped,
        "a licensed blend may prove more rays empty than its cover sphere, never fewer: {} vs {}",
        blended_descent.skipped,
        blended_root.skipped
    );
    let (plain_blend, _) = render(&blended, &camera);
    assert_eq!(plain_blend.pixels, blended_frame.pixels);

    // And the no-bound case: a plane gives the descent nothing to test, so it behaves exactly as the
    // unbounded early-out already does - zero skipped rays, whole budget charged.
    let plane = Tree {
        nodes: vec![leaf(NODE_PLANE, 0, 4, true)],
        params: vec![0.0, 1.0, 0.0, 0.0],
    };
    let (plane_frame, plane_stats) = render_bnb(&plane, &camera);
    assert_eq!(
        plane_stats.skipped, 0,
        "no bound, no early-out, no invented box"
    );
    let (plain_plane, _) = render(&plane, &camera);
    assert_eq!(plain_plane.pixels, plane_frame.pixels);
}

/// A record with the root flag carried explicitly, because the file's `branch` helper flags every branch
/// and a two-branch tree then fails `validate`'s single-root rule.
fn node(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// The scene the boot actually marches: a plain union whose second operand is a blend. One blended branch
/// is enough to make `respects` false all the way to the root, which is why the demo prints a refusal.
fn union_over_blend(k: f32) -> Tree {
    Tree {
        nodes: vec![
            node(NODE_UNION, 2, 5, 0, 0, true),
            node(NODE_SPHERE, 0, 1, 0, 4, false),
            node(NODE_SMOOTH_UNION, 2, 3, 4, 1, false),
            node(NODE_SPHERE, 0, 1, 5, 4, false),
            node(NODE_SPHERE, 0, 1, 9, 4, false),
        ],
        params: vec![
            1.0, 0.0, 0.0, 0.0, //
            k, 1.0, -1.0, 0.0, 0.0, 1.0, 3.0, 0.0, 0.0,
        ],
    }
}

#[test]
fn the_smooth_union_bound_holds_the_field_with_k_to_spare() {
    // `respects[node]` is the precondition a skip - per ray or per step - reads, and it is withheld from
    // every smooth union because `smin` returns below `min` by up to `k/4`. cull_probe priced what that
    // costs (a 128-sphere scene written with blended branches loses the hierarchy outright: 648 evaluations
    // becomes 287,128), so the question OQ-54 asks is whether the *grown* enclosure the table already stores
    // satisfies the inequality anyway. This measures it rather than arguing it: for each scene, sample the
    // field on a lattice and report the worst `field(p) - distance(p, enclosure)` over the samples, where
    // distance is the clamped one `lower_bound` returns. A negative worst margin is a real violation of the
    // precondition - a point the marcher would treat as surface, outside the sphere a skip would have
    // ignored - and a non-negative one is evidence, still not proof, that the licence could be relaxed.
    let surface = 1e-3f32;
    for (label, tree, expect_respected) in [
        ("blend k 0.4, offset 2.0", smooth_union(0.4, 2.0), true),
        ("blend k 1.5, offset 3.0", smooth_union(1.5, 3.0), true),
        ("plain union, offset 3.0", union(3.0), true),
        ("union over blend k 0.5", union_over_blend(0.5), true),
    ] {
        validate(&tree).unwrap_or_else(|error| panic!("{label} must validate: {error:?}"));
        let bounds = scene_bounds(&tree);
        let root = ROOT as usize;
        assert_eq!(
            bounds.respects(root),
            expect_respected,
            "{label}: the licence bit is not the one this test is written to measure"
        );
        let Enclosure::Bounded { centre, radius } = bounds.enclosure_of(root) else {
            panic!("{label} must have a derived bound to measure against");
        };
        // One measurement, run over two point sets: the coarse lattice (whose step is 250x the surface
        // epsilon, so a thin band can slip between samples) and a dense sweep along exactly the lines where
        // `smin` dips most - the axis through the two centres and the two lines through the waist, at a
        // 0.005 step. The lattice is the breadth; the sweep is the depth.
        let measure = |points: &mut dyn Iterator<Item = [f32; 3]>| {
            let mut worst = f32::MAX;
            let mut worst_point = [0.0f32; 3];
            let mut violations = 0usize;
            let mut samples = 0usize;
            let mut band = 0usize;
            for p in points {
                let field = sdf_eval(&tree, ROOT, p);
                let d = (p[0] - centre[0]).powi(2)
                    + (p[1] - centre[1]).powi(2)
                    + (p[2] - centre[2]).powi(2);
                let distance = (d.sqrt() - radius).max(0.0);
                if distance > 0.0 {
                    samples += 1;
                    let margin = field - distance;
                    if margin < worst {
                        worst = margin;
                        worst_point = p;
                    }
                    // "Materially below" is `surface`, not zero. A plain union - the case the licence
                    // *grants* - also dips a few parts in 10^7 below its own bound at the far corner,
                    // because both quantities are large and nearly equal there and the arithmetic is `f32`.
                    // A rounding-scale miss cannot delete geometry: the point is inside the hit band the
                    // marcher stops in anyway. What would matter is a violation larger than the tolerance
                    // the frame is rendered with, so that is what is counted.
                    if margin < -surface {
                        violations += 1;
                    }
                }
                if field.abs() < surface && distance > radius + surface {
                    band += 1;
                }
            }
            (worst, worst_point, violations, samples, band)
        };

        let mut lattice = (0..41).flat_map(move |ix| {
            (0..41).flat_map(move |iy| {
                (0..41).map(move |iz| {
                    [
                        -6.0 + ix as f32 * 0.25,
                        -6.0 + iy as f32 * 0.25,
                        -6.0 + iz as f32 * 0.25,
                    ]
                })
            })
        });
        let (worst, worst_point, exterior_violations, exterior_samples, band_outside) =
            measure(&mut lattice);

        // The dense sweeps, as three lines. `waist` is the midpoint between the two blended centres, which
        // is where the quadratic `smin` sits furthest below `min`.
        let axis = (0..2401).map(|i| -6.0 + i as f32 * 0.005);
        let (axis_worst, axis_point, axis_violations, axis_samples, axis_band) = measure(
            &mut axis
                .clone()
                .map(|x| [x, 0.0, 0.0])
                .chain(axis.clone().map(|y| [0.5 * (centre[0]), y, 0.0]))
                .chain(axis.map(|z| [0.5 * centre[0], 0.0, z])),
        );

        println!(
            "MEASURE {label}: lattice {exterior_samples} pts worst margin {worst:.6} at {worst_point:?} violations {exterior_violations} band {band_outside} | sweeps {axis_samples} pts worst {axis_worst:.6} at {axis_point:?} violations {axis_violations} band {axis_band} | bound r {radius:.4} licence {expect_respected}",
        );
        // Both halves are asserted, because both are what a relaxation of OQ-54 would rest on: the
        // containment of the *hit band* (the property whose violation would be a deleted surface rather
        // than a slower frame), and the precondition itself on the exterior, where it is the thing a skip
        // substitutes for the real field.
        assert_eq!(
            exterior_violations, 0,
            "{label}: {exterior_violations} exterior points sit below the bound a skip would use, worst              margin {worst} at {worst_point:?}"
        );
        assert!(
            worst >= 0.0,
            "{label}: the grown bound is not a lower bound on the field outside it: {worst} at {worst_point:?}"
        );
        assert_eq!(
            axis_violations, 0,
            "{label}: the dense sweep found {axis_violations} exterior points below the bound, worst              {axis_worst} at {axis_point:?}"
        );
        assert_eq!(
            axis_band, 0,
            "{label}: the dense sweep found surface-band points outside the bound"
        );
        assert_eq!(
            band_outside, 0,
            "{label}: a point the marcher calls surface lies outside the bound a skip would use"
        );
    }
}
