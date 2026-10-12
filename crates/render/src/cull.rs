//! Enclosure: a conservative bound on where a subtree can have surface, so a ray that cannot reach it
//! spends zero evaluations.
//!
//! Why this exists as its own module rather than a line in [`crate::raster`]. docs/04's cost story names
//! the number culling has to move, and the raster probe measured it: on the boot's 64x48 grid, 33.0% of
//! rays find surface and the rest pay **69.6%** of all distance-field evaluations. That is not a shading
//! problem, it is a "we marched a ray through empty space ninety-six times" problem, and it is the same
//! gap ADR-024's BVH argument is about.
//!
//! **Conservative means superset, and the module is built so a mistake here cannot change a picture.** An
//! enclosure that is too *small* lets a ray skip geometry it should have hit: a silently wrong frame,
//! which is the worst failure a renderer can have because it still looks like a render. So every rule
//! below either bounds the set from outside or returns [`Enclosure::Unbounded`], and unbounded means the
//! early-out is not applied — never a guessed big box. The test that matters is therefore not about the
//! arithmetic here at all: it is that a culled frame and an unculled frame are byte-identical, which turns
//! the whole module into a property instead of a table of formulas.
//!
//! Floats, and allowed to be: nothing here feeds a Merkle root (docs/02's float discipline, AGENTS.md §4).
//! A float *can* decide which rays get skipped, which is why the arithmetic is add, multiply and `sqrt`
//! only — no `sin`, `cos` or `atan` — so two machines cannot disagree about a boundary through a
//! transcendental's rounding. `Plane`, `Repeat`, `Twist`, `Bend` and `Noise` are the deliberate refusals:
//! a plane and a tiling are genuinely infinite, and the two deformations have derivable but
//! currently-unproven displacement bounds. An unproven bound is the too-small enclosure.

use crate::{length, Tree, ROOT};
use contracts::sdf::{
    NODE_BEND, NODE_CYLINDER, NODE_INTERSECT, NODE_MIRROR, NODE_NOISE, NODE_PLANE, NODE_REPEAT,
    NODE_ROUNDED_BOX, NODE_SMOOTH_UNION, NODE_SPHERE, NODE_SUBTRACT, NODE_TORUS, NODE_TWIST,
    NODE_UNION,
};

/// A sphere that provably contains every surface point a subtree can produce, or the admission that no
/// such sphere was derived.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Enclosure {
    Bounded {
        centre: [f32; 3],
        radius: f32,
    },
    /// Plane, infinite tiling, unbounded deformation, or a kind this file does not know. The early-out
    /// reads this as "anything could be here", which is always true and therefore never wrong.
    Unbounded,
}

impl Enclosure {
    /// A sphere covering both. The midpoint-plus-half-distance rule is a bound rather than a minimum: for
    /// very unequal radii it is loose, and loose costs evaluations, not pixels.
    pub fn cover(self, other: Self) -> Self {
        let (
            Self::Bounded {
                centre: a,
                radius: ra,
            },
            Self::Bounded {
                centre: b,
                radius: rb,
            },
        ) = (self, other)
        else {
            return Self::Unbounded;
        };
        let mid = [
            (a[0] + b[0]) * 0.5,
            (a[1] + b[1]) * 0.5,
            (a[2] + b[2]) * 0.5,
        ];
        let half = length([a[0] - b[0], a[1] - b[1], a[2] - b[2]]) * 0.5;
        Self::Bounded {
            centre: mid,
            radius: half + ra.max(rb),
        }
    }

    /// Grow the bound by `margin`. This is what a smoothing parameter spends: `smin` with strength `k`
    /// rounds the surface outward by at most `k` beyond either child.
    pub fn grown(self, margin: f32) -> Self {
        match self {
            Self::Bounded { centre, radius } => Self::Bounded {
                centre,
                radius: radius + margin,
            },
            Self::Unbounded => Self::Unbounded,
        }
    }
}

/// The enclosure of the subtree rooted at `node`, walking pre-order the way `validate` and the evaluator
/// do.
pub fn enclose(tree: &Tree, node: u32) -> Enclosure {
    enclose_at(tree, node as usize, 0)
}

/// The root's enclosure, which is the form a caller wants when it is culling whole rays against a whole
/// scene.
#[inline]
pub fn scene_enclosure(tree: &Tree) -> Enclosure {
    enclose(tree, ROOT)
}

fn enclose_at(tree: &Tree, index: usize, depth: u32) -> Enclosure {
    // The same belt the evaluator carries: `validate` is the gate, and this makes an unvalidated deep tree
    // give up the bound rather than produce one that quietly excluded part of the scene.
    if depth > crate::MAX_TREE_DEPTH + 8 {
        return Enclosure::Unbounded;
    }
    let record = tree.nodes[index];
    let params = tree.params_of(record);
    let first = if record.arity() > 0 { index + 1 } else { index };
    let second = if record.arity() > 1 {
        first + tree.nodes[first].subtree_len() as usize
    } else {
        first
    };

    match record.kind() {
        NODE_SPHERE => Enclosure::Bounded {
            centre: [params[1], params[2], params[3]],
            radius: params[0],
        },
        // Half-extents plus the fillet: the box corner is the farthest point and the fillet pushes it out.
        NODE_ROUNDED_BOX => Enclosure::Bounded {
            centre: [0.0, 0.0, 0.0],
            radius: length([params[0], params[1], params[2]]) + params[3],
        },
        // Radius and half-height are the two extents, so the corner of the profile is `sqrt(r^2 + hh^2)`.
        NODE_CYLINDER => Enclosure::Bounded {
            centre: [0.0, 0.0, 0.0],
            radius: length([params[0], params[1], 0.0]),
        },
        NODE_TORUS => Enclosure::Bounded {
            centre: [0.0, 0.0, 0.0],
            radius: params[0] + params[1],
        },
        NODE_PLANE | NODE_REPEAT | NODE_TWIST | NODE_BEND | NODE_NOISE => Enclosure::Unbounded,
        NODE_UNION => enclose_at(tree, first, depth + 1).cover(enclose_at(tree, second, depth + 1)),
        NODE_SMOOTH_UNION => enclose_at(tree, first, depth + 1)
            .cover(enclose_at(tree, second, depth + 1))
            .grown(params[0]),
        // A difference and an intersection are both subsets of their first operand, so the first child's
        // bound is already conservative. Taking it is the honest half: it claims only "inside A", not
        // knowledge of what the carve removed.
        NODE_SUBTRACT | NODE_INTERSECT => enclose_at(tree, first, depth + 1),
        NODE_MIRROR => {
            let child = enclose_at(tree, first, depth + 1);
            let Enclosure::Bounded { centre, radius } = child else {
                return Enclosure::Unbounded;
            };
            let axis = (params[0] as u8).min(2) as usize;
            let mut mirrored = centre;
            mirrored[axis] = -centre[axis];
            child.cover(Enclosure::Bounded {
                centre: mirrored,
                radius,
            })
        }
        // Unknown or unimplemented kind: no bound, so no early out.
        _ => Enclosure::Unbounded,
    }
}

/// Whether the segment `origin + t * dir` for `t` in `[0, max_t]` can reach the enclosure at all.
/// `false` is a proof, and a caller that skips the ray on it spends nothing; `true` means march.
///
/// Closest point of the segment to the centre, projected and clamped, then one distance comparison. A
/// `Unbounded` enclosure always answers `true`.
pub fn ray_may_hit(enclosure: Enclosure, origin: [f32; 3], dir: [f32; 3], max_t: f32) -> bool {
    let Enclosure::Bounded { centre, radius } = enclosure else {
        return true;
    };
    let to_centre = [
        centre[0] - origin[0],
        centre[1] - origin[1],
        centre[2] - origin[2],
    ];
    let along = to_centre[0] * dir[0] + to_centre[1] * dir[1] + to_centre[2] * dir[2];
    let t = along.clamp(0.0, max_t);
    length([
        origin[0] + dir[0] * t - centre[0],
        origin[1] + dir[1] * t - centre[1],
        origin[2] + dir[2] * t - centre[2],
    ]) <= radius
}
