//! The build-time union reorder: an equivalence claim, tested as one.
//!
//! `order::rebalance` moves the children of plain unions around and rebuilds the tree balanced, because
//! `cull_probe` measured that tree shape is worth 2.6x to 3.0x of the culling. What it must *not* do is
//! change what the shape means, so every test here is stated as an equality - of bytes, of depths, or of
//! the distance a point evaluates to - rather than as a percentage.

use assetc::order::{extent, rebalance};
use assetc::{lower, Shape};
use render::{sdf_eval, Tree, ROOT};

fn sphere(r: f32, x: f32) -> Shape {
    Shape::Sphere {
        r,
        centre: [x, 0.0, 0.0],
    }
}

fn union(a: Shape, b: Shape) -> Shape {
    Shape::Union {
        a: Box::new(a),
        b: Box::new(b),
    }
}

/// Four spheres, authored as a left-leaning chain: the shape an author writes when they keep adding to
/// the left, and the one a marcher pays for at depth 4.
fn chain() -> Shape {
    union(
        union(
            union(sphere(1.0, 9.0), sphere(0.5, -6.0)),
            sphere(0.75, 3.0),
        ),
        sphere(0.25, -1.0),
    )
}

/// The same four spheres, authored balanced. Both must lower to one set of bytes.
fn balanced() -> Shape {
    union(
        union(sphere(0.5, -6.0), sphere(0.25, -1.0)),
        union(sphere(0.75, 3.0), sphere(1.0, 9.0)),
    )
}

fn sample_points() -> Vec<[f32; 3]> {
    let mut out = Vec::new();
    for x in -12..12 {
        for y in -6..6 {
            out.push([x as f32 * 0.5, y as f32 * 0.5, 0.0]);
        }
    }
    out
}

fn eval_at(tree: &Tree, p: [f32; 3]) -> f32 {
    sdf_eval(tree, ROOT, p)
}

#[test]
fn a_chain_and_a_balanced_tree_of_one_set_lower_to_the_same_bytes() {
    // The normalisation claim, stated where it is cheapest to check: two authored shapes, one asset.
    let chain_asset = lower(&chain()).expect("the chain lowers");
    let balanced_asset = lower(&balanced()).expect("the balanced tree lowers");
    assert_eq!(
        chain_asset.tree.nodes, balanced_asset.tree.nodes,
        "rebalancing is a normalisation, so authored shape must not survive in the record order"
    );
    assert_eq!(chain_asset.tree.params, balanced_asset.tree.params);
    assert_eq!(chain_asset.depth, balanced_asset.depth);
}

#[test]
fn the_rebalanced_field_is_the_minimum_of_the_four_spheres_computed_here() {
    // The outside calculation, so the test does not just compare the implementation with itself.
    let spheres = [(1.0f32, 9.0f32), (0.5, -6.0), (0.75, 3.0), (0.25, -1.0)];
    let tree = lower(&chain()).expect("lowers").tree;
    for p in sample_points() {
        let mut expected = f32::INFINITY;
        for (r, x) in spheres {
            let dx = p[0] - x;
            let distance = (dx * dx + p[1] * p[1] + p[2] * p[2]).sqrt() - r;
            expected = expected.min(distance);
        }
        assert_eq!(
            eval_at(&tree, p).to_bits(),
            expected.to_bits(),
            "the field at {p:?} is not the minimum of its own spheres"
        );
    }
}

#[test]
fn depth_falls_when_the_chain_is_rebuilt() {
    // What the marcher budgets: eight spheres authored as a chain are depth 8, and depth 4 rebalanced.
    let mut chained = sphere(0.3, 0.0);
    for i in 1..8 {
        chained = union(chained, sphere(0.3, i as f32 * 2.0));
    }
    let asset = lower(&chained).expect("lowers");
    assert_eq!(
        asset.depth, 4,
        "eight leaves balanced is four levels, not the eight the author wrote"
    );
    assert_eq!(asset.tree.nodes.len(), 15, "8 leaves need 7 unions");
}

#[test]
fn a_subtract_keeps_its_arguments_and_a_plane_stops_the_sort() {
    // `kept` minus `cutter` is not a set, so the names have to survive; and a plane has no finite extent
    // to sort on, which must degrade to "leave it alone" rather than to an invented coordinate.
    let difference = Shape::Subtract {
        kept: Box::new(sphere(2.0, 0.0)),
        cutter: Box::new(sphere(0.5, 1.5)),
    };
    assert_eq!(
        rebalance(&difference),
        difference,
        "a difference is not a set"
    );

    let with_plane = union(
        Shape::Plane {
            normal: [0.0, 0.0, 1.0],
            offset: 0.0,
        },
        sphere(1.0, 4.0),
    );
    assert_eq!(
        rebalance(&with_plane),
        with_plane,
        "an unbounded item means no sort key, so the order stands"
    );
}

#[test]
fn a_union_of_a_smooth_union_and_a_sphere_is_flattened_only_to_its_own_level() {
    // The forest is flattened through plain unions and nothing else: a smooth union below stays one opaque
    // item, because its blend is not a `min` and its two children are not free to move past it.
    let blended = Shape::SmoothUnion {
        a: Box::new(sphere(1.0, -8.0)),
        b: Box::new(sphere(1.0, -5.0)),
        k: 0.8,
    };
    let top = union(union(blended.clone(), sphere(0.5, 7.0)), sphere(0.4, 2.0));
    let moved = rebalance(&top);
    let Shape::Union { a, .. } = &moved else {
        panic!("the top node is still a union");
    };
    let Shape::SmoothUnion {
        a: left,
        b: right,
        k,
    } = a.as_ref()
    else {
        panic!("the blend must survive as one item, got {a:?}");
    };
    assert_eq!(*k, 0.8);
    assert_eq!(**left, sphere(1.0, -8.0));
    assert_eq!(**right, sphere(1.0, -5.0));
}

/// `extent` is published because a caller outside the compiler asks it a question the sort keys never had
/// to answer honestly: *does the box contain what the asset draws?* A sort key may be wrong and only
/// reorder badly; a box handed to an author that leaves a surface outside it is a lie about the picture.
///
/// So the claim is tested against the evaluator rather than against the box's own arithmetic: every lattice
/// point the lowered asset reports as inside the solid must sit inside the claimed box. The cases are the
/// arms where a box is cheapest to get wrong - a `Mirror` whose child sits off its plane (the reflection
/// draws where the child does not), a blend that bulges past both children by `k`, a difference, and a
/// rounded corner - and the two kinds that must answer `None` rather than invent a coordinate.
#[test]
fn an_authored_extent_contains_every_point_the_lowered_asset_draws() {
    let mirrored_pair = Shape::Mirror {
        axis: 0,
        child: Box::new(union(sphere(0.5, 1.5), sphere(0.25, 0.75))),
    };
    for (name, shape) in [
        ("a mirror of an off-plane pair", mirrored_pair.clone()),
        (
            "a mirror below a union",
            union(mirrored_pair.clone(), sphere(0.3, -4.0)),
        ),
        (
            "a blend of two spheres",
            Shape::SmoothUnion {
                a: Box::new(sphere(0.6, -2.0)),
                b: Box::new(sphere(0.6, 0.0)),
                k: 0.7,
            },
        ),
        (
            "a difference",
            Shape::Subtract {
                kept: Box::new(sphere(2.0, 0.0)),
                cutter: Box::new(sphere(0.5, 1.5)),
            },
        ),
        (
            "a rounded box",
            Shape::RoundedBox {
                half: [1.0, 0.5, 0.25],
                r: 0.2,
            },
        ),
    ] {
        let (low, high) =
            extent(&shape).unwrap_or_else(|| panic!("{name} has a finite box to state"));
        let tree = lower(&shape)
            .unwrap_or_else(|e| panic!("{name} does not lower: {e}"))
            .tree;
        for p in sample_points() {
            if eval_at(&tree, p) <= 0.0 {
                assert!(
                    (0..3).all(|axis| low[axis] <= p[axis] && p[axis] <= high[axis]),
                    "{name} draws the solid point {p:?}, outside the box {low:?}..={high:?} it claims"
                );
            }
        }
    }

    // The arithmetic, pinned beside the invariant that only catches a box that is too small somewhere:
    // the mirrored pair spans x 0.5..=2.0 as authored, and its reflection is what a viewer actually sees
    // at x = -1.5, so the box is the join of the two and not the child alone.
    assert_eq!(
        extent(&mirrored_pair),
        Some(([-2.0, -0.5, -0.5], [2.0, 0.5, 0.5])),
        "a Mirror's box is its child joined with the child's reflection across the plane through zero"
    );

    // And the two kinds that tile or extend forever state that instead of guessing a reach.
    assert_eq!(
        extent(&Shape::Plane {
            normal: [0.0, 0.0, 1.0],
            offset: 0.0,
        }),
        None,
        "a half-space has no finite box"
    );
    assert_eq!(
        extent(&Shape::Repeat {
            period: 2.0,
            child: Box::new(sphere(0.3, 0.5)),
        }),
        None,
        "a tiling repeats forever, so no box holds it"
    );
}
