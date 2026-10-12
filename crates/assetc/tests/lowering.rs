//! The lowering stage, checked against the evaluator it feeds.
//!
//! Deliberately **not** a byte-for-byte comparison with `crates/render`'s hand-built tree: a node's
//! pool offset depends on emission order, so demanding identical words would pin an accident of the
//! implementation instead of the contract. What is asserted is what must hold - the lowered tree
//! answers distance questions identically, its spans are pre-order-consistent (`render::validate`
//! recomputes them independently), the root is flagged once, and the refusals refuse.

use assetc::{lower, LowerError, Shape};
use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SMOOTH_UNION, NODE_SPHERE, NODE_UNION,
};
use render::sdf_eval;

fn scene() -> Shape {
    Shape::Union {
        a: Box::new(Shape::RoundedBox {
            half: [0.35, 0.35, 0.9],
            r: 0.12,
        }),
        b: Box::new(Shape::SmoothUnion {
            a: Box::new(Shape::Sphere {
                r: 0.7,
                centre: [-0.9, 0.0, 0.0],
            }),
            b: Box::new(Shape::Sphere {
                r: 0.7,
                centre: [0.9, 0.0, 0.0],
            }),
            k: 0.5,
        }),
    }
}

/// The same scene as `crates/render/examples/scene_steps.rs` builds it by hand.
fn hand_built() -> render::Tree {
    let mut pool: Vec<f32> = Vec::new();
    let mut push = |kind: u16, arity: u16, subtree: u32, vals: &[f32], flags: u16| -> NodeRecord {
        let first = pool.len() as u16;
        pool.extend_from_slice(vals);
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, flags),
            pack_params_len(vals.len() as u16),
        ])
    };
    let left = push(NODE_SPHERE, 0, 1, &[0.7, -0.9, 0.0, 0.0], 0);
    let right = push(NODE_SPHERE, 0, 1, &[0.7, 0.9, 0.0, 0.0], 0);
    let blended = push(NODE_SMOOTH_UNION, 2, 3, &[0.5], 0);
    let block = push(NODE_ROUNDED_BOX, 0, 1, &[0.35, 0.35, 0.9, 0.12], 0);
    let whole = push(NODE_UNION, 2, 5, &[], FLAG_IS_ROOT);
    render::Tree {
        nodes: vec![whole, block, blended, left, right],
        params: pool,
    }
}

#[test]
fn a_lowered_scene_answers_distance_questions_like_the_hand_built_one() {
    let asset = lower(&scene()).expect("the scene lowers");
    let hand = hand_built();
    render::validate(&hand).expect("the hand-built tree is the same layout");
    render::validate(&asset.tree).expect("and so is the lowered one");
    assert_eq!(asset.tree.nodes.len(), 5);
    assert_eq!(asset.depth, 3, "union -> blend -> sphere");

    for probe in [
        [0.0f32, 0.0, 0.0],
        [0.0, 0.0, 0.95],
        [-0.9, 0.0, 0.0],
        [3.0, 0.0, 0.0],
        [0.0, 0.0, -3.0],
    ] {
        let lowered = sdf_eval(&asset.tree, render::ROOT, probe);
        let built = sdf_eval(&hand, render::ROOT, probe);
        assert!(
            (lowered - built).abs() < 1e-6,
            "at {probe:?}: lowered {lowered} vs hand-built {built}"
        );
    }
}

#[test]
fn the_root_is_flagged_once_and_nothing_claims_a_material() {
    let asset = lower(&scene()).expect("lowers");
    let roots = asset
        .tree
        .nodes
        .iter()
        .filter(|record| record.flags() & FLAG_IS_ROOT != 0)
        .count();
    assert_eq!(roots, 1, "exactly one root, on the outermost node");
    assert_eq!(
        asset.tree.nodes[0].subtree_len() as usize,
        asset.tree.nodes.len(),
        "a root's span covers the buffer, which is what makes index zero the entry point"
    );
    assert!(!asset.has_material(), "no Shape variant carries one yet");
}

#[test]
fn refusals_refuse_before_any_byte_exists() {
    assert_eq!(
        lower(&Shape::Mirror {
            axis: 7,
            child: Box::new(Shape::Sphere {
                r: 1.0,
                centre: [0.0; 3]
            }),
        }),
        Err(LowerError::BadAxis),
        "an out-of-range axis is refused, not clamped into a different shape"
    );
    assert_eq!(
        lower(&Shape::Sphere {
            r: f32::NAN,
            centre: [0.0; 3]
        }),
        Err(LowerError::NonFiniteParameter),
        "NaN travels a distance field into every pixel and raises nothing"
    );

    let mut deep = Shape::Sphere {
        r: 0.5,
        centre: [0.0; 3],
    };
    for _ in 0..20 {
        deep = Shape::Repeat {
            period: 2.0,
            child: Box::new(deep),
        };
    }
    assert!(matches!(lower(&deep), Err(LowerError::TooDeep { .. })));
}

#[test]
fn subtracts_argument_order_belongs_to_the_keeper() {
    // The evaluator's `max(dKept, -dCutter)` and the authoring field named `kept` have to agree, or a
    // compiled asset cuts the wrong half of a shape - the exact inversion the evaluator's first draft
    // carried.
    let asset = lower(&Shape::Subtract {
        kept: Box::new(Shape::RoundedBox {
            half: [0.5, 0.5, 0.5],
            r: 0.1,
        }),
        cutter: Box::new(Shape::Plane {
            normal: [0.0, 0.0, 1.0],
            offset: 0.2,
        }),
    })
    .expect("lowers");
    assert!(
        sdf_eval(&asset.tree, render::ROOT, [0.0, 0.0, 0.3]) < 0.0,
        "inside the box and above the cut plane, so kept"
    );
    assert!(
        sdf_eval(&asset.tree, render::ROOT, [0.0, 0.0, -0.3]) > 0.0,
        "below it, removed"
    );
}
