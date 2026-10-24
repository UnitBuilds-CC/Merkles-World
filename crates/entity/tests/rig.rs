//! The rig's own laws, on a fixture small enough to check by hand.
//!
//! Every assertion here is a *rigid* invariant — a distance, a signed distance to a plane, bytes that must
//! not move — because the alternative is asserting particular `f32` centres, which only proves the test and
//! the implementation were written by the same arithmetic. Two carry their own falsifier on purpose: the
//! knee angle must change a hip-to-foot distance (`a_pose_bends_the_limb_it_was_given`, which fails if
//! `pose` silently does nothing), and the sole must stay 0.04 under its toe
//! (`the_sole_travels_with_the_hoof`, which is the defect that shows up as a lifted foot sliced by an
//! invisible cutter). The fixture builds itself through `render::validate`, so an illegal `subtree_len`
//! panics here rather than turning into a mystery about the selector.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_INTERSECT, NODE_PLANE, NODE_SPHERE, NODE_SUBTRACT, NODE_UNION,
};
use entity::rig::{self, Gait, LegSpec};
use render::{sdf_eval, validate, Tree, ROOT};

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// A body and one authored leg, in the shape the pig's `pig.ron` lowers to: a thigh and a cannon above
/// the knee, a foot below it cut flat by the sole, and a second plane inside the *body* — where a selector
/// that guessed by index distance would attach the wrong sole.
///
/// Pre-order, subtree lengths counted bottom-up: `10 Plane`, `9 Sphere`, `8 Intersect(9,10)=3`,
/// `7 Sphere`, `6 Union(7,8)=5`, `5 Sphere`, `4 Union(5,6)=7`, `3 Plane`, `2 Sphere`,
/// `1 Subtract(2,3)=3`, `0 Union(1,4)=11`.
fn animal() -> Tree {
    let tree = Tree {
        nodes: vec![
            record(NODE_UNION, 2, 11, 0, 0, true),
            record(NODE_SUBTRACT, 2, 3, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 0, 4, false),
            record(NODE_PLANE, 0, 1, 4, 4, false),
            record(NODE_UNION, 2, 7, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 8, 4, false),
            record(NODE_UNION, 2, 5, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 12, 4, false),
            record(NODE_INTERSECT, 2, 3, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 16, 4, false),
            record(NODE_PLANE, 0, 1, 20, 4, false),
        ],
        // `[r, cx, cy, cz]` per sphere and `[nx, ny, nz, d]` per plane: the layouts the evaluator arms read.
        params: vec![
            0.50, 0.00, 0.60, 0.00, // 0  body
            0.00, -1.00, 0.00, 0.30, // 4  belly cut
            0.082, 0.14, 0.135, -0.16, // 8  thigh
            0.082, 0.14, 0.000, -0.16, // 12 cannon, centred on the knee
            0.075, 0.10, -0.175, -0.16, // 16 foot
            0.00, -1.00, 0.00, 0.215, // 20 sole: 0.04 above the foot's bottom
        ],
    };
    validate(&tree).expect("fixture is a legal A-07 tree");
    tree
}

const SPEC: LegSpec = LegSpec {
    z: -0.16,
    tolerance: 1e-3,
    hip_y: 0.20,
    knee_y: 0.0,
};

fn posed(hip: f32, knee: f32) -> Tree {
    let mut tree = animal();
    let leg = rig::select_leg(&tree, &SPEC).expect("the leg selects");
    leg.pose(&mut tree, hip, knee);
    tree
}

fn centre(tree: &Tree, index: usize) -> [f32; 3] {
    rig::sphere_centre(tree, index)
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

#[test]
fn selects_the_limb_and_its_own_sole_only() {
    let tree = animal();
    let leg = rig::select_leg(&tree, &SPEC).expect("the leg selects");

    // Records 5 and 7 ride the hip; the foot at 9 and the sole at 10 ride the knee. The body's plane at
    // index 3 is outside the limb's subtree, and attaching it would cut the hoof at the belly's height.
    assert_eq!(leg.hip.rides, vec![5, 7]);
    assert_eq!(leg.knee.rides, vec![9, 10]);
    assert_eq!(leg.rides().count(), 4, "the pose writes four records");
}

#[test]
fn a_pose_keeps_each_bone_rigid() {
    let rest = animal();
    let tree = posed(0.45, -0.30);

    // Thigh and cannon are one bone; nothing below the knee may ride with them.
    assert!(
        (distance(centre(&rest, 5), centre(&rest, 7))
            - distance(centre(&tree, 5), centre(&tree, 7)))
        .abs()
            < 1e-5,
        "records 5 and 7 are one bone; a hinge may not stretch it"
    );
    // A radius is not a position: widths must not move, or a swing would fatten the limb.
    for index in [5, 7, 9] {
        let first = rest.nodes[index].params_first() as usize;
        assert_eq!(
            tree.nodes[index].params_first(),
            rest.nodes[index].params_first(),
            "record {index} keeps its pool slot"
        );
        assert_eq!(
            tree.params[first], rest.params[first],
            "record {index} keeps its radius"
        );
    }
}

#[test]
fn a_pose_bends_the_limb_it_was_given() {
    let rest = animal();
    let straight = posed(0.0, 0.0);
    let bent = posed(0.0, -0.6);

    let hip_to_foot = distance(centre(&rest, 5), centre(&rest, 9));
    assert_eq!(
        distance(centre(&straight, 5), centre(&straight, 9)),
        hip_to_foot,
        "the identity pose must not move anything, to any digit"
    );
    assert!(
        (distance(centre(&bent, 5), centre(&bent, 9)) - hip_to_foot).abs() > 1e-3,
        "a knee angle that moves nothing is a hinge that was never applied"
    );
}

#[test]
fn the_sole_travels_with_the_hoof() {
    let rest = animal();
    // The authored clearance: the foot's centre sits 0.04 inside the cut, which is what makes the hoof a
    // flat-soled foot rather than a ball.
    let depth = rig::plane_distance(&rest, 10, centre(&rest, 9));
    assert!(
        (depth + 0.04).abs() < 1e-6,
        "fixture depth into the cut, got {depth}"
    );

    for (hip, knee) in [(0.45, -0.3), (-0.6, 0.0), (0.2, -0.8)] {
        let tree = posed(hip, knee);
        let moved = rig::plane_distance(&tree, 10, centre(&tree, 9));
        assert!(
            (moved - depth).abs() < 1e-5,
            "hip {hip}, knee {knee}: the hoof came off its own cut ({moved} vs {depth})"
        );
        let squared = tree.params[20] * tree.params[20]
            + tree.params[21] * tree.params[21]
            + tree.params[22] * tree.params[22];
        assert!(
            (squared - 1.0).abs() < 1e-5,
            "a rotated plane normal must stay unit, got {squared}"
        );
    }
}

#[test]
fn a_zero_pose_writes_nothing() {
    let rest = animal();
    let tree = posed(0.0, 0.0);
    assert_eq!(
        tree.params, rest.params,
        "the identity pose is not a rounding"
    );
}

#[test]
fn a_hinge_undoes_its_own_rotation() {
    // One bone, out and back: a hinge about a *fixed* pivot is invertible, so the round trip is the
    // honest closure test for the swing half of a stride. It is `f32`, hence 1e-6 and not `==` - and the
    // number matters, because `sin(tau)` is not zero: the drift here is that transcendental's own error,
    // quoted rather than hidden. It stays three orders under the 1e-3 surface epsilon a march stops at.
    let mut tree = animal();
    let rest = tree.clone();
    let leg = rig::select_leg(&tree, &SPEC).expect("the leg selects");
    let angle = core::f32::consts::TAU / 7.0;
    leg.pose(&mut tree, angle, 0.0);
    leg.pose(&mut tree, -angle, 0.0);

    let home = leg
        .rides()
        .filter(|&index| tree.nodes[index].kind() == contracts::sdf::NODE_SPHERE)
        .map(|index| distance(centre(&tree, index), centre(&rest, index)))
        .fold(0.0f32, f32::max);
    assert!(home < 1e-6, "a full swing-and-back drifted {home}");
}

#[test]
fn the_gait_is_periodic_and_never_hyperextends() {
    let gait = Gait {
        omega: 1.0,
        swing: 0.42,
        flex: 0.35,
    };
    let mut min_swing = f32::MAX;
    let mut max_swing = f32::MIN;
    let mut step = 0;
    while step <= 126 {
        let phase = step as f32 * 0.05;
        let (hip, knee) = gait.angles(phase);
        assert!(
            knee <= 0.0,
            "phase {phase}: a knee must bend, never straighten past its rest"
        );
        min_swing = min_swing.min(hip);
        max_swing = max_swing.max(hip);
        let (wrapped_hip, wrapped_knee) = gait.angles(phase + core::f32::consts::TAU);
        assert!(
            (hip - wrapped_hip).abs() < 1e-5 && (knee - wrapped_knee).abs() < 1e-5,
            "phase {phase} and one whole cycle later must agree"
        );
        step += 1;
    }
    assert!(
        (min_swing + gait.swing).abs() < 1e-2 && (max_swing - gait.swing).abs() < 1e-2,
        "the hip should sweep the declared amplitude, got {min_swing}..{max_swing}"
    );
    assert!(
        gait.angles(0.0).1 == 0.0,
        "a limb at phase zero is on the ground, so its knee is straight"
    );
}

#[test]
fn an_unmatched_spec_selects_no_leg() {
    let tree = animal();
    assert_eq!(
        rig::select_leg(
            &tree,
            &LegSpec {
                z: 5.0,
                tolerance: 1e-3,
                hip_y: 0.20,
                knee_y: 0.0,
            }
        ),
        None,
        "a column that is not in the tree is not a leg"
    );
    // A knee below every sphere in the band leaves the shank hinged to nothing.
    assert_eq!(
        rig::select_leg(
            &tree,
            &LegSpec {
                z: -0.16,
                tolerance: 1e-3,
                hip_y: 0.20,
                knee_y: -9.0,
            }
        ),
        None
    );
    // A hip below the top of the bone it turns shears the column through its own flesh instead of
    // swinging it - the failure a shoulder would show as a gap, not as a bad angle.
    assert_eq!(
        rig::select_leg(
            &tree,
            &LegSpec {
                z: -0.16,
                tolerance: 1e-3,
                hip_y: 0.10,
                knee_y: 0.0,
            }
        ),
        None
    );
}

#[test]
fn a_non_unit_plane_normal_is_refused_not_carried() {
    let mut tree = animal();
    // The rigid carry re-solves `d` from the support point and assumes a unit normal; a scaled normal
    // would make the sole's offset mean something different every frame.
    tree.params[21] = -2.0;
    assert_eq!(rig::select_leg(&tree, &SPEC), None);
}

#[test]
fn a_posed_tree_still_evaluates_as_a_field() {
    // The strongest check available without a picture: the posed limb's own points stay inside their own
    // primitives, judged by the evaluator rather than by this file's arithmetic.
    let tree = posed(0.45, -0.3);
    assert!(
        sdf_eval(&tree, ROOT, centre(&tree, 9)) < 0.0,
        "the foot's centre must remain inside its own hoof"
    );
    assert!(
        sdf_eval(&tree, ROOT, centre(&tree, 5)) < 0.0,
        "the thigh's centre must remain inside the limb"
    );
    assert!(
        sdf_eval(&tree, ROOT, [4.0, 0.2, -0.16]) > 0.0,
        "empty space four units away must stay empty"
    );
}

#[test]
fn the_rest_support_is_the_authored_sole_not_the_sphere_bottom() {
    let tree = animal();
    let leg = rig::select_leg(&tree, &SPEC).expect("the leg selects");
    // The foot's own bottom is at -0.25, but the sole cut at -0.215 removes everything below it, so the
    // surface the animal stands on is the cut. A support height read off the sphere would sink the hoof
    // 0.035 into the floor and the stage would look like it had swallowed the feet.
    assert_eq!(leg.support_y(&tree), -0.215);
}

#[test]
fn a_hip_only_swing_carries_the_whole_limb() {
    // The falsifier for the half of forward kinematics that is easy to omit: a knee angle of zero is not
    // "the shank stays where it was authored", it is "the shank keeps its angle to the thigh". Turning only
    // the hip must therefore drag the cannon, the foot *and* the sole cut along, or the limb stretches out
    // of its socket on every frame where the knee happens to be straight — which in `Gait`'s stance half is
    // every frame. `the_sole_travels_with_the_hoof` cannot catch this: it compares a hoof to whatever cut
    // it currently rides, and both were left behind together.
    let rest = animal();
    let leg = rig::select_leg(&rest, &SPEC).expect("the leg selects");
    let tree = posed(0.30, 0.0);

    let foot = distance(centre(&tree, 9), centre(&rest, 9));
    assert!(
        foot > 1e-3,
        "the thigh swung and the foot did not follow: it moved {foot}"
    );
    // The limb below the hip is one rigid body at a straight knee, so every bone distance survives.
    for (a, b) in [(5usize, 9usize), (7, 9), (5, 7)] {
        let moved = (distance(centre(&rest, a), centre(&rest, b))
            - distance(centre(&tree, a), centre(&tree, b)))
        .abs();
        assert!(moved < 1e-5, "records {a} and {b} drifted {moved} apart");
    }
    // And the support rose: a hip above a foot cannot swing it sideways without lifting it.
    assert!(
        leg.support_y(&tree) > leg.support_y(&rest),
        "support stayed at {}",
        leg.support_y(&rest)
    );
}

#[test]
fn a_swing_lifts_the_foot_it_was_swung() {
    // The reason the strip needed a body line: a limb hinged above its own foot raises that foot by
    // `L(1 - cos theta)` however it turns, so an unmoved body floats. If this ever fails, the hinge has
    // started sliding the hoof along the ground instead of swinging the leg - which would be a different
    // and much better animation, and would need this file's prose rewritten.
    let rest = animal();
    let leg = rig::select_leg(&rest, &SPEC).expect("the leg selects");
    let tree = posed(0.24, -0.20);
    // The same leg, on the posed tree: a pose rewrites parameters and nothing else, so the record indices
    // the rest pose yielded are still the limb's records. Re-selecting here would fail, and correctly -
    // selection is geometric, and a swung column no longer sits at the authored `z`.
    assert!(
        leg.support_y(&tree) > leg.support_y(&rest) + 1e-3,
        "swing lifted the support to {}",
        leg.support_y(&tree)
    );
}

#[test]
fn a_ball_foot_falls_back_to_its_own_bottom() {
    // A limb with no sole cut: `Intersect` is authoring choice, not a grammar requirement.
    let tree = {
        let tree = Tree {
            nodes: vec![
                record(NODE_UNION, 2, 3, 0, 0, true),
                record(NODE_SPHERE, 0, 1, 0, 4, false),
                record(NODE_SPHERE, 0, 1, 4, 4, false),
            ],
            params: vec![
                0.082, 0.14, 0.135, -0.16, // thigh
                0.075, 0.14, -0.175, -0.16, // foot
            ],
        };
        validate(&tree).expect("ball-foot fixture is legal");
        tree
    };
    let leg = rig::select_leg(&tree, &SPEC).expect("the ball foot selects");
    assert_eq!(
        leg.knee.rides,
        vec![2],
        "no plane, so nothing but the sphere"
    );
    assert_eq!(leg.support_y(&tree), -0.25);
}
