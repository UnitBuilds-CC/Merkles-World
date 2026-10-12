//! A-07 evaluated, validated and marched on the CPU.
//!
//! Every assertion here is one whose answer can be worked out on paper first: a unit sphere's distance
//! at a known point, the sign a CSG combination must have, the envelope a smooth union is bounded by,
//! the step count a scaled scene must not change. Where an answer is not analytic, this file says so
//! rather than guessing - a wrong assertion is worse than none, because it teaches the next reader
//! that the shape is wrong.
//!
//! The CSG direction is worth calling out, because the first draft of the evaluator had it
//! inverted: `SUBTRACT(A, B)` keeps **A** (`max(dA, -dB)`), and `max(-dA, dB)` - which is B minus A -
//! reads correctly at the surface and wrongly everywhere else. The probes below are on dedicated
//! trees for that reason: in the scene, a unit sphere swallows a half-metre box at the origin, so a
//! cut would be invisible and a probe would prove nothing.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_CYLINDER, NODE_INTERSECT, NODE_MIRROR, NODE_PLANE, NODE_REPEAT, NODE_ROUNDED_BOX,
    NODE_SMOOTH_UNION, NODE_SPHERE, NODE_SUBTRACT, NODE_TORUS, NODE_TWIST, NODE_UNION,
};
use render::{
    is_implemented, march, normal, sdf_eval, validate, Hit, Tree, ValidationError,
    IMPLEMENTED_KINDS, ROOT,
};

/// Builds pre-order node lists so the tests author a tree the way `assetc` will, with `subtree_len`
/// computed rather than asserted. `finish` clones the pool: a builder that hands its parameters over
/// and empties them makes a second tree's indices point at nothing.
struct Builder {
    params: Vec<f32>,
}

impl Builder {
    fn new() -> Self {
        Self { params: Vec::new() }
    }

    fn record(&mut self, kind: u16, arity: u16, subtree_len: u32, params: &[f32]) -> NodeRecord {
        let first = self.params.len() as u16;
        self.params.extend_from_slice(params);
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree_len,
            pack_first_and_flags(first, 0),
            pack_params_len(params.len() as u16),
        ])
    }

    fn leaf(&mut self, kind: u16, params: &[f32]) -> Vec<NodeRecord> {
        vec![self.record(kind, 0, 1, params)]
    }

    fn branch(
        &mut self,
        kind: u16,
        params: &[f32],
        children: Vec<Vec<NodeRecord>>,
    ) -> Vec<NodeRecord> {
        let total: u32 = 1 + children.iter().map(|child| child.len() as u32).sum::<u32>();
        let head = self.record(kind, children.len() as u16, total, params);
        let mut out = vec![head];
        for child in children {
            out.extend(child);
        }
        out
    }

    fn finish(&mut self, mut nodes: Vec<NodeRecord>) -> Tree {
        // Flags live in the high half of word 2, so they go in through the same shift the contract's
        // pack helper uses. OR-ing `FLAG_IS_ROOT` into the raw word corrupts `params_first`, which is
        // the trap this builder walked into first.
        nodes[0].0[2] |= (FLAG_IS_ROOT as u32) << 16;
        Tree {
            nodes,
            params: self.params.clone(),
        }
    }
}

fn sphere(r: f32) -> Tree {
    let mut b = Builder::new();
    let leaf = b.leaf(NODE_SPHERE, &[r, 0.0, 0.0, 0.0]);
    b.finish(leaf)
}

fn two_spheres(k_blend: Option<f32>) -> Tree {
    let mut b = Builder::new();
    let left = b.leaf(NODE_SPHERE, &[1.0, -0.5, 0.0, 0.0]);
    let right = b.leaf(NODE_SPHERE, &[1.0, 0.5, 0.0, 0.0]);
    let tree = match k_blend {
        None => b.branch(NODE_UNION, &[], vec![left, right]),
        Some(k) => b.branch(NODE_SMOOTH_UNION, &[k], vec![left, right]),
    };
    b.finish(tree)
}

#[test]
fn implemented_kinds_are_a_subset_of_the_grammar() {
    let implemented = IMPLEMENTED_KINDS.len();
    assert!(is_implemented(NODE_SPHERE));
    assert!(
        !is_implemented(NODE_TWIST),
        "in the grammar, not in this evaluator"
    );
    assert!(
        implemented < 15,
        "the grammar has 15 kinds; refusing the gap is the point"
    );
    assert_eq!(implemented, 11);
}

#[test]
fn a_scene_of_known_shapes_validates_and_evaluates_exactly() {
    let mut b = Builder::new();
    let ball = b.leaf(NODE_SPHERE, &[1.0, 0.0, 0.0, 0.0]);
    let block = b.leaf(NODE_ROUNDED_BOX, &[0.5, 0.5, 0.5, 0.1]);
    let scene = b.branch(NODE_UNION, &[], vec![ball, block]);
    let scene = b.finish(scene);
    validate(&scene).expect("the scene is well-formed by construction");

    // At the origin the nearer surface is the sphere's, one unit inside it.
    assert!((sdf_eval(&scene, ROOT, [0.0, 0.0, 0.0]) + 1.0).abs() < 1e-6);
    // On the surface, and six units out where the sphere is still the nearer of the two.
    assert!(sdf_eval(&scene, ROOT, [1.0, 0.0, 0.0]).abs() < 1e-6);
    assert!((sdf_eval(&scene, ROOT, [6.0, 0.0, 0.0]) - 5.0).abs() < 1e-6);
    // Along y at the origin the box is inside the sphere too, so the union stays at the sphere's -1.
    assert!((sdf_eval(&scene, ROOT, [0.0, 0.0, 0.0]) + 1.0).abs() < 1e-6);
}

#[test]
fn every_way_to_lie_about_a_tree_is_refused_with_the_right_reason() {
    let mut b = Builder::new();

    let unknown = b.finish(vec![NodeRecord([
        pack_kind_and_arity(63, 0),
        1,
        0,
        pack_params_len(0),
    ])]);
    assert_eq!(validate(&unknown), Err(ValidationError::UnknownKind(63)));

    let bead = b.leaf(NODE_SPHERE, &[1.0, 0.0, 0.0, 0.0]);
    let built = b.branch(NODE_TWIST, &[0.0], vec![bead]);
    let twist = b.finish(built);
    assert!(matches!(
        validate(&twist),
        Err(ValidationError::UnsupportedKind(k)) if k == NODE_TWIST
    ));

    let lying = b.finish(vec![NodeRecord([
        pack_kind_and_arity(NODE_SPHERE, 1),
        1,
        pack_first_and_flags(0, FLAG_IS_ROOT),
        pack_params_len(4),
    ])]);
    assert_eq!(
        validate(&lying),
        Err(ValidationError::ArityMismatch {
            kind: NODE_SPHERE,
            claimed: 1,
            expected: 0
        })
    );

    let ball = b.leaf(NODE_SPHERE, &[1.0, 0.0, 0.0, 0.0]);
    let wall = b.leaf(NODE_PLANE, &[0.0, 1.0, 0.0, 2.0]);
    let mut clipped = b.branch(NODE_UNION, &[], vec![ball, wall]);
    clipped[0].0[1] = 2; // claims its subtree is two records, not three
    let clipped = b.finish(clipped);
    assert_eq!(
        validate(&clipped),
        Err(ValidationError::SubtreeLength {
            index: 0,
            claimed: 2,
            actual: 3
        })
    );

    // The pool the earlier cases built holds nine floats, so a nine-parameter node at offset 0 would
    // be legal. Out of range means the index has to leave the pool, which is the condition tested.
    let starving = b.finish(vec![NodeRecord([
        pack_kind_and_arity(NODE_SPHERE, 0),
        1,
        pack_first_and_flags(4096, FLAG_IS_ROOT),
        pack_params_len(9),
    ])]);
    assert_eq!(
        validate(&starving),
        Err(ValidationError::ParametersOutOfRange { index: 0 })
    );

    let rootless = Tree {
        nodes: vec![NodeRecord([
            pack_kind_and_arity(NODE_SPHERE, 0),
            1,
            0,
            pack_params_len(0),
        ])],
        params: vec![],
    };
    assert_eq!(validate(&rootless), Err(ValidationError::RootCount(0)));

    // Eighteen nested repeats: unary, so cheap to build, and one past the draft depth cap.
    let mut deep = Builder::new();
    let mut inner = deep.leaf(NODE_SPHERE, &[1.0, 0.0, 0.0, 0.0]);
    for _ in 0..17 {
        let wrapped = deep.branch(NODE_REPEAT, &[2.0], vec![inner]);
        inner = wrapped;
    }
    let deep = deep.finish(inner);
    assert!(matches!(
        validate(&deep),
        Err(ValidationError::TooDeep { .. })
    ));
}

#[test]
fn subtract_keeps_the_first_child_and_intersect_agrees_on_the_overlap() {
    let mut b = Builder::new();
    let block = b.leaf(NODE_ROUNDED_BOX, &[0.5, 0.5, 0.5, 0.1]);
    let cut = b.leaf(NODE_PLANE, &[0.0, 0.0, 1.0, 0.2]);
    let built = b.branch(NODE_SUBTRACT, &[], vec![block, cut]);
    let carved = b.finish(built);
    validate(&carved).expect("two children, a binary kind");
    // Inside the box and above the plane, so kept.
    assert!(sdf_eval(&carved, ROOT, [0.2, 0.2, 0.3]) < 0.0);
    // Inside the box but below the plane, so removed. The inverted draft reported this as kept.
    assert!(sdf_eval(&carved, ROOT, [0.2, 0.2, -0.3]) > 0.0);
    // Outside the box in x: the kept solid's own distance, exactly 0.3 at this probe.
    let outside = sdf_eval(&carved, ROOT, [0.9, 0.0, 0.3]);
    assert!(
        (outside - 0.3).abs() < 1e-5,
        "expected the box surface, got {outside}"
    );

    let mut b = Builder::new();
    let block = b.leaf(NODE_ROUNDED_BOX, &[0.5, 0.5, 0.5, 0.1]);
    let cap = b.leaf(NODE_PLANE, &[0.0, 0.0, 1.0, 0.2]);
    let built = b.branch(NODE_INTERSECT, &[], vec![block, cap]);
    let common = b.finish(built);
    assert!(
        sdf_eval(&common, ROOT, [0.0, 0.0, 0.1]) < 0.0,
        "inside both"
    );
    assert!(
        sdf_eval(&common, ROOT, [0.0, 0.0, 0.45]) > 0.0,
        "the plane bounds the overlap from above"
    );
}

#[test]
fn the_smooth_union_envelope_is_the_bound_the_marcher_relies_on() {
    // Two unit spheres 1 apart: at their midpoint is 0.5 from each centre and therefore 0.5 INSIDE both surfaces: the sharp
    // answer is -0.5, and a blend of radius k lands in [min - k, min] = [-0.9, -0.5].
    let sharp = two_spheres(None);
    let blended = two_spheres(Some(0.4));
    let midpoint = [0.0, 0.0, 0.0];
    let min = sdf_eval(&sharp, ROOT, midpoint);
    let soft = sdf_eval(&blended, ROOT, midpoint);
    assert!(
        (min + 0.5).abs() < 1e-6,
        "the sharp min at the midpoint, got {min}"
    );
    assert!(soft <= min + 1e-6, "smin must never rise above min");
    assert!(soft > min - 0.4 - 1e-6, "smin must not fall below min - k");
    // The relaxation is exactly k/4 at the symmetric point, which is what makes the bound checkable.
    assert!((soft + 0.6).abs() < 1e-6, "expected -0.5 - k/4, got {soft}");
}

#[test]
fn repeat_and_mirror_fold_the_field_without_moving_the_surface() {
    let mut b = Builder::new();
    let bead = b.leaf(NODE_SPHERE, &[0.4, 0.0, 0.0, 0.0]);
    let built = b.branch(NODE_REPEAT, &[2.0], vec![bead]);
    let grid = b.finish(built);
    assert!(
        sdf_eval(&grid, ROOT, [0.0, 0.0, 0.0]) < 0.0,
        "a copy per cell"
    );
    assert!(
        sdf_eval(&grid, ROOT, [2.0, 0.0, 0.0]) < 0.0,
        "including its neighbour"
    );
    assert!(
        sdf_eval(&grid, ROOT, [1.0, 0.0, 0.0]) > 0.4,
        "the gap stays empty"
    );

    let mut b = Builder::new();
    let bead = b.leaf(NODE_SPHERE, &[0.5, 1.5, 0.0, 0.0]);
    let built = b.branch(NODE_MIRROR, &[0.0], vec![bead]);
    let mirrored = b.finish(built);
    assert!(sdf_eval(&mirrored, ROOT, [1.5, 0.0, 0.0]) < 0.0);
    assert!(
        sdf_eval(&mirrored, ROOT, [-1.5, 0.0, 0.0]) < 0.0,
        "mirrored across x"
    );
}

#[test]
fn a_guaranteed_empty_step_never_overshoots_the_surface() {
    let scene = sphere(1.0);
    let Hit { t, point, steps } = march(
        &scene,
        ROOT,
        [6.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        12.0,
        1e-4,
        64,
    )
    .expect("the ray meets the sphere");
    assert!((t - 5.0).abs() < 1e-3, "surface at t = 5, got {t}");
    assert!(
        steps <= 2,
        "the first step is already the distance: {steps}"
    );
    assert!(
        sdf_eval(&scene, ROOT, point).abs() < 1e-3,
        "the hit is on the surface"
    );
    assert!(
        march(
            &scene,
            ROOT,
            [6.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            20.0,
            1e-4,
            64
        )
        .is_none(),
        "a ray aimed away misses, and the step budget ends the loop"
    );
}

#[test]
fn scale_changes_little_but_precision() {
    // Gate R5's substance, measurable without a GPU: sphere tracing spends steps per *relative*
    // approach, so a scene and a millionth of it need nearly the same number. A mesh or voxel
    // representation cannot say this, which is the claim ADR-011 rests on.
    let big = sphere(1.0);
    let small = sphere(1e-6);
    let far = march(
        &big,
        ROOT,
        [9.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        10.0,
        1e-7,
        512,
    )
    .expect("hit the unit sphere");
    // At 1e-6 world units the f32 ulp is around 1e-12: an epsilon finer than the representation can
    // never be satisfied, and the ray walks past. That is a finding, not a tuning knob - absolute
    // sub-pixel promises at depth are what docs/02's camera-relative coordinates exist to avoid.
    let near = march(
        &small,
        ROOT,
        [9e-6, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        10e-6,
        1e-12,
        512,
    )
    .expect("hit the millionth-scale sphere");
    assert!(
        near.steps <= far.steps + 2,
        "scale must not change the step count much: {} vs {}",
        near.steps,
        far.steps
    );
    let drift = ((near.t * 1e6) - far.t).abs() / far.t;
    assert!(drift < 1e-2, "same point, scaled; relative drift {drift}");

    let n = normal(&small, ROOT, near.point, near.t * 1e-3);
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    assert!(
        (len - 1.0).abs() < 1e-3,
        "a unit normal at 1e-6 scale, got {len}"
    );
}

#[test]
fn cylinder_and_torus_answer_their_axial_geometry() {
    let mut b = Builder::new();
    let built = b.leaf(NODE_CYLINDER, &[1.0, 2.0, 1.0]);
    let column = b.finish(built); // r, half-height, along y
    assert!(
        (sdf_eval(&column, ROOT, [0.0, 0.0, 0.0]) + 1.0).abs() < 1e-6,
        "on the axis"
    );
    assert!(
        (sdf_eval(&column, ROOT, [2.0, 0.0, 0.0]) - 1.0).abs() < 1e-6,
        "radially out"
    );
    assert!(
        (sdf_eval(&column, ROOT, [0.0, 3.0, 0.0]) - 1.0).abs() < 1e-6,
        "past the cap"
    );

    let mut b = Builder::new();
    let built = b.leaf(NODE_TORUS, &[3.0, 1.0]);
    let ring = b.finish(built);
    assert!(
        (sdf_eval(&ring, ROOT, [3.0, 0.0, 0.0]) + 1.0).abs() < 1e-6,
        "on the circle"
    );
    assert!(
        (sdf_eval(&ring, ROOT, [0.0, 0.0, 0.0]) - 2.0).abs() < 1e-6,
        "at the hole centre"
    );
    assert!(
        (sdf_eval(&ring, ROOT, [3.0, 2.0, 0.0]) - 1.0).abs() < 1e-6,
        "above the tube"
    );
}
