//! Build-time shape of a union forest: what `lower` does to an authored tree before a byte is written.
//!
//! `examples/cull_probe.rs` in `crates/render` measured the reason this file exists. It scattered spheres
//! deterministically and rendered the same scene twice - once with the leaves emitted in generation order,
//! once after a median split on the widest axis. Pixels, depth and march steps were identical; the work
//! the per-node bounds removed was not: 15.2% against 46.1% of 1,655,892 node visits at 32 spheres,
//! 20.5% against 54.3% at 128. Reordering is worth 2.6x to 3.0x of the culling, so it is a compiler
//! decision rather than a renderer one, and this is where a compiler gets to make it.
//!
//! **Why it is legal, and where it stops.** `min` is commutative and associative over `f32` exactly - no
//! rounding enters, so no reassociation changes a bit - which means the children of a plain `Union` are a
//! *set*, and a set may be laid down in any order the builder likes. Nothing else on the shape list has
//! that property: `Subtract`'s operand order is its meaning (`kept` minus `cutter` is not the reverse),
//! `Intersect` is a `max` and therefore commutative but is not the case measured here, and `SmoothUnion`
//! blends two fields through a kernel whose `k` is the only order-free part of it. So this module
//! flattens and rebuilds **only** through `Shape::Union`, treats every other node as one opaque item with
//! an extent, and refuses to reorder anything whose extent is unbounded (`Plane`, `Repeat`), because a
//! sort needs finite coordinates and an invented one is the too-small bound again.
//!
//! The transform is a normalisation, which is what makes it testable: a chain of four unions and a
//! balanced tree of the same four spheres lower to the *same bytes*, and the depth a marcher must budget
//! falls with the chain.

use crate::Shape;

/// A subtree's finite axis-aligned extent, or `None` when the shape is unbounded.
///
/// Deliberately conservative in the direction that costs work and not geometry: a rounded box reports its
/// corner including the fillet, a smooth union grows its children by `k`, and anything whose extent cannot
/// be stated finitely reports `None`, which makes the caller leave the order alone.
fn extent(shape: &Shape) -> Option<([f32; 3], [f32; 3])> {
    match shape {
        Shape::Sphere { r, centre } => Some((
            [centre[0] - r, centre[1] - r, centre[2] - r],
            [centre[0] + r, centre[1] + r, centre[2] + r],
        )),
        Shape::RoundedBox { half, r } => Some((
            [-half[0] - r, -half[1] - r, -half[2] - r],
            [half[0] + r, half[1] + r, half[2] + r],
        )),
        // A plane is a half-space: infinite along the surface. No finite box, so no sort key.
        Shape::Plane { .. } => None,
        Shape::Union { a, b } | Shape::Intersect { a, b } => join(&extent(a)?, &extent(b)?),
        Shape::SmoothUnion { a, b, k } => {
            let joined = join(&extent(a)?, &extent(b)?)?;
            Some((
                [joined.0[0] - k, joined.0[1] - k, joined.0[2] - k],
                [joined.1[0] + k, joined.1[1] + k, joined.1[2] + k],
            ))
        }
        // The kept solid contains the difference, so its extent is the extent of the result.
        Shape::Subtract { kept, .. } => extent(kept),
        // Tiling repeats forever; mirroring would only double a box that is already stated.
        Shape::Repeat { .. } => None,
        Shape::Mirror { child, .. } => extent(child),
    }
}

fn join(a: &([f32; 3], [f32; 3]), b: &([f32; 3], [f32; 3])) -> Option<([f32; 3], [f32; 3])> {
    Some((
        [a.0[0].min(b.0[0]), a.0[1].min(b.0[1]), a.0[2].min(b.0[2])],
        [a.1[0].max(b.1[0]), a.1[1].max(b.1[1]), a.1[2].max(b.1[2])],
    ))
}

/// Flatten a union forest into the items it unions, each already rebuilt below the top.
fn collect(shape: &Shape, items: &mut Vec<Shape>) {
    match shape {
        Shape::Union { a, b } => {
            collect(a, items);
            collect(b, items);
        }
        other => items.push(rebalance(other)),
    }
}

fn centre(item: &([f32; 3], [f32; 3])) -> [f32; 3] {
    [
        (item.0[0] + item.1[0]) * 0.5,
        (item.0[1] + item.1[1]) * 0.5,
        (item.0[2] + item.1[2]) * 0.5,
    ]
}

/// Rebuild `shape` with the children of every plain union laid down in spatial order and the tree
/// balanced, so a chain of N unions becomes a tree of depth `log2 N` over sorted items.
///
/// Returns an equivalent shape - equivalent in the strong sense that every point evaluates to the same
/// `f32`, because only union operands moved.
pub fn rebalance(shape: &Shape) -> Shape {
    match shape {
        Shape::Union { a, b } => {
            let mut items = Vec::new();
            collect(a, &mut items);
            collect(b, &mut items);
            let boxes: Vec<([f32; 3], [f32; 3])> =
                items.iter().filter_map(extent).collect::<Vec<_>>();
            // One unbounded item and the sort has no key for it: emit the forest as authored, rebuilt
            // below, rather than inventing a coordinate for a half-space.
            if boxes.len() != items.len() || items.len() < 3 {
                return Shape::Union {
                    a: Box::new(rebalance(a)),
                    b: Box::new(rebalance(b)),
                };
            }
            let mut spread = [0.0f32; 3];
            for axis in 0..3 {
                let mut lo = f32::INFINITY;
                let mut hi = f32::NEG_INFINITY;
                for (low, high) in &boxes {
                    lo = lo.min(low[axis]);
                    hi = hi.max(high[axis]);
                }
                spread[axis] = hi - lo;
            }
            let axis = if spread[1] > spread[0] && spread[1] >= spread[2] {
                1
            } else if spread[2] > spread[0] {
                2
            } else {
                0
            };
            let mut keyed: Vec<(f32, Shape)> = items
                .into_iter()
                .zip(boxes)
                .map(|(item, box_at)| (centre(&box_at)[axis], item))
                .collect();
            keyed.sort_by(|left, right| {
                left.0
                    .partial_cmp(&right.0)
                    .unwrap_or(core::cmp::Ordering::Equal)
            });
            let sorted: Vec<Shape> = keyed.into_iter().map(|(_, item)| item).collect();
            build(&sorted, 0, sorted.len())
        }
        Shape::SmoothUnion { a, b, k } => Shape::SmoothUnion {
            a: Box::new(rebalance(a)),
            b: Box::new(rebalance(b)),
            k: *k,
        },
        // Names preserved: `kept` and `cutter` are arguments, not a set.
        Shape::Subtract { kept, cutter } => Shape::Subtract {
            kept: Box::new(rebalance(kept)),
            cutter: Box::new(rebalance(cutter)),
        },
        Shape::Intersect { a, b } => Shape::Intersect {
            a: Box::new(rebalance(a)),
            b: Box::new(rebalance(b)),
        },
        Shape::Repeat { period, child } => Shape::Repeat {
            period: *period,
            child: Box::new(rebalance(child)),
        },
        Shape::Mirror { axis, child } => Shape::Mirror {
            axis: *axis,
            child: Box::new(rebalance(child)),
        },
        leaf => leaf.clone(),
    }
}

/// A balanced tree over an already-sorted slice: the median is the split, which is the whole of what a
/// BVH build does here beyond the sort.
fn build(items: &[Shape], from: usize, to: usize) -> Shape {
    if to - from == 1 {
        return items[from].clone();
    }
    let mid = from + (to - from) / 2;
    Shape::Union {
        a: Box::new(build(items, from, mid)),
        b: Box::new(build(items, mid, to)),
    }
}
