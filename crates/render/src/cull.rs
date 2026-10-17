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

/// Every node's enclosure plus one more bit per node, explained on [`SceneBounds::respects`].
///
/// Computed in one pass over the pre-order array, in reverse index order, because pre-order puts every
/// child after its parent: children are finished before the node that reads them, so no recursion and no
/// re-walking of subtrees is needed. The same is not true of [`enclose`], which is the on-demand form and
/// re-descends per call - fine for building this table once, wrong for consulting per march step.
pub fn scene_bounds(tree: &Tree) -> SceneBounds {
    collect(tree, true)
}

/// The same table with the skips disabled, which exists for one reason: to price the skips.
///
/// Counting while pruning is on would report the work the pruning avoided as work done, and the two are
/// then incomparable. This form walks the field exactly as the unpruned evaluator would - same values,
/// same trajectory - but keeps a tally, so `render_culled` (counting) and `render_bnb` (pruning) can be
/// compared against each other on identical frames. Without it there is no honest number for ADR-024,
/// only a step count that cannot see the effect at all.
pub fn counting_bounds(tree: &Tree) -> SceneBounds {
    collect(tree, false)
}

fn collect(tree: &Tree, pruning: bool) -> SceneBounds {
    let mut bounds = SceneBounds {
        enclosure: vec![Enclosure::Unbounded; tree.nodes.len()],
        respects: vec![false; tree.nodes.len()],
        pruning,
        visits: core::cell::Cell::new(0),
    };
    for index in (0..tree.nodes.len()).rev() {
        let record = tree.nodes[index];
        let params = tree.params_of(record);
        let first = if record.arity() > 0 { index + 1 } else { index };
        let second = if record.arity() > 1 {
            first + tree.nodes[first].subtree_len() as usize
        } else {
            first
        };
        let (first_enc, first_ok) = (bounds.enclosure[first], bounds.respects[first]);
        let (second_enc, second_ok) = (bounds.enclosure[second], bounds.respects[second]);
        let (enclosure, respects) = match record.kind() {
            NODE_SPHERE => (
                Enclosure::Bounded {
                    centre: [params[1], params[2], params[3]],
                    radius: params[0],
                },
                true,
            ),
            NODE_ROUNDED_BOX => (
                Enclosure::Bounded {
                    centre: [0.0, 0.0, 0.0],
                    radius: length([params[0], params[1], params[2]]) + params[3],
                },
                true,
            ),
            NODE_CYLINDER => (
                Enclosure::Bounded {
                    centre: [0.0, 0.0, 0.0],
                    radius: length([params[0], params[1], 0.0]),
                },
                true,
            ),
            NODE_TORUS => (
                Enclosure::Bounded {
                    centre: [0.0, 0.0, 0.0],
                    radius: params[0] + params[1],
                },
                true,
            ),
            // A plane has no bound at all, so the respecting bit is moot: no lower bound can be offered.
            NODE_PLANE | NODE_REPEAT => (Enclosure::Unbounded, false),
            // Twist and bend rotate or shear space without moving it *closer*: they are isometries only
            // per-point, and a bound that holds for the child need not bound the deformed field's surface
            // from the outside at all. Refused, not assumed.
            NODE_TWIST | NODE_BEND | NODE_NOISE => (Enclosure::Unbounded, false),
            NODE_UNION => (first_enc.cover(second_enc), first_ok && second_ok),
            // smin returns *less* than min - by up to k/4 - so the field can dip below the distance to
            // its own bound, which is exactly the inequality a skip needs. It is therefore not a node this
            // pass may prune below, whatever its enclosure says about coverage.
            // The refusal this arm used to carry was over-cautious, and the derivation is short enough to
            // write down rather than sample: outside the *grown* sphere the bound reads
            // `D_grown = D_cover - k`, while the field reads `smin >= min(a,b) - k/4`, and each respecting
            // child contributes `a >= D_first`, `b >= D_second` with `min(D_first, D_second) >= D_cover`
            // because a cover sphere contains both children. So `field >= D_grown + 3k/4` outside the grown
            // sphere - the dip is `k/4` and the growth spends `k`, which is why the growth is what licenses
            // the arm. Inside it the bound is zero and behaves like any other node's: the skip compares the
            // bound against the sibling's already-evaluated value and only fires when that bound is at
            // least as far, so a zero can never outrank a positive field, and where the sibling is itself
            // non-positive the march clamps the step to `surface` either way.
            // What the grant does NOT buy, measured in the same run (cull_probe's blend rows are unchanged:
            // 59.7% / 0.0% of rays proved empty and 0.0% node work avoided). The only skip site in this
            // evaluator is the plain-union arm, and in a scene whose branches are all blends there is no
            // plain union above them to take it. Nor may a descent continue *through* a blend into its
            // children: a smooth union's zero set lies between them, outside both child spheres, so the
            // grown cover sphere is the tightest claim a blend can make. The lever that would actually pay
            // is a k-aware arm in the smooth-union evaluator - smin equals min whenever |b - a| >= k, so a
            // second child bounded at a + k or beyond cannot change the answer. That is a separate change
            // with its own test, and OQ-54's remaining half; this line is a precondition, not the saving.
            // What the grant does NOT buy, measured in the same run (cull_probe's blend rows are unchanged:
            // 59.7% / 0.0% of rays proved empty and 0.0% node work avoided). The only skip site in this
            // evaluator is the plain-union arm, and in a scene whose branches are all blends there is no
            // plain union above them to take it. Nor may a descent continue *through* a blend into its
            // children: a smooth union's zero set lies between them, outside both child spheres, so the
            // grown cover sphere is the tightest claim a blend can make. The lever that would actually pay
            // is a k-aware arm in the smooth-union evaluator - smin equals min whenever |b - a| >= k, so a
            // second child bounded at a + k or beyond cannot change the answer. That is a separate change
            // with its own test, and OQ-54's remaining half; this line is a precondition, not the saving.
            NODE_SMOOTH_UNION => (
                first_enc.cover(second_enc).grown(params[0]),
                first_ok && second_ok,
            ),
            // Both combinators take a `max` that contains the first operand's distance, so the field is
            // at least the first child's, which bounds the whole node by induction.
            NODE_SUBTRACT | NODE_INTERSECT => (first_enc, first_ok),
            NODE_MIRROR => match first_enc {
                Enclosure::Bounded { centre, radius } => {
                    let axis = (params[0] as u8).min(2) as usize;
                    let mut mirrored = centre;
                    mirrored[axis] = -centre[axis];
                    (
                        first_enc.cover(Enclosure::Bounded {
                            centre: mirrored,
                            radius,
                        }),
                        first_ok,
                    )
                }
                Enclosure::Unbounded => (Enclosure::Unbounded, false),
            },
            _ => (Enclosure::Unbounded, false),
        };
        bounds.enclosure[index] = enclosure;
        bounds.respects[index] = respects;
    }
    bounds
}

/// One node's enclosure and respecting bit, indexed by node.
#[derive(Debug, Clone)]
pub struct SceneBounds {
    enclosure: Vec<Enclosure>,
    respects: Vec<bool>,
    pruning: bool,
    visits: core::cell::Cell<u64>,
}

impl SceneBounds {
    /// How many nodes the evaluator actually visited. Only meaningful after a frame, and only
    /// comparable between two runs of the same scene and camera - which is why the counting-only table
    /// exists alongside the pruning one.
    #[inline]
    pub fn visits(&self) -> u64 {
        self.visits.get()
    }

    /// Called by the evaluator on every node it enters. `Cell` rather than a parameter because the
    /// count belongs to the table, not to the call chain: threading `&mut u64` through `eval_at` would
    /// put an argument in the hottest function in the crate for a number no frame uses.
    #[inline]
    pub(crate) fn count_visit(&self) {
        self.visits.set(self.visits.get() + 1);
    }

    #[inline]
    pub fn enclosure_of(&self, node: usize) -> Enclosure {
        self.enclosure[node]
    }

    /// The table in the form a device can read it: one `[f32; 4]` row per node, laid out as the WGSL
    /// `vec4` `[centre.x, centre.y, centre.z, radius]`.
    ///
    /// A row for a node this table will not skip carries an **infinite radius** instead of a flag bit, so
    /// every row is a valid conservative enclosure and the shader needs no special case: an infinite sphere
    /// is reached by every ray, and the skip arm simply never fires. Folding the licence into the radius
    /// rather than into a second array is what keeps the whole table in one storage buffer, which matters
    /// because the WGSL interpreter already sits at `wgpu::Limits::downlevel_defaults()`'
    /// four-storage-buffer-per-stage ceiling.
    ///
    /// The `respects` half of the guard is contract shape rather than a live branch today: the two arrays are
    /// filled in the same pass, and on the licensed tables a bounded enclosure and a granted licence arrive
    /// together. What the guard does provably carry is the counting baseline, which fills the same real
    /// enclosures and refuses them only because `pruning` is `false` - delete the guard and that table hands a
    /// device radii for nodes the baseline never licensed, which is the failure the test catches.
    pub fn encoded(&self) -> Vec<[f32; 4]> {
        self.enclosure
            .iter()
            .enumerate()
            .map(|(node, enclosure)| match *enclosure {
                Enclosure::Bounded { centre, radius } if self.pruning && self.respects[node] => {
                    [centre[0], centre[1], centre[2], radius]
                }
                _ => [0.0, 0.0, 0.0, f32::INFINITY],
            })
            .collect()
    }

    /// Whether this table is allowed to license work avoidance at all.
    ///
    /// The counting table answers `false` and every licensed query then returns the neutral value, which is
    /// what keeps [`crate::raster::render_culled`] a baseline rather than a second optimisation to compare
    /// against — the two runs of one scene have to differ in exactly one thing before their difference
    /// means anything.
    #[inline]
    pub fn prunes(&self) -> bool {
        self.pruning
    }

    /// Whether this subtree's field is at least the distance to its own enclosure, everywhere.
    ///
    /// That is the condition a per-node skip needs, and it is *not* the same as "the enclosure covers the
    /// surface", which is what coverage requires and the weaker property. A hard union's field is a
    /// `min`, so it is at least each child's distance and hence at least the distance to the sphere that
    /// covers them; a subtract's or intersect's field is a `max` that contains the first operand's
    /// distance, so it inherits the first child's; a **smooth union's is not** - `smin` returns less than
    /// `min` by up to `k/4` - so a node below one cannot be skipped on a sphere test no matter how far
    /// away the sphere is. Getting this backwards is a frame with geometry missing, and it looks correct.
    #[inline]
    pub fn respects(&self, node: usize) -> bool {
        self.respects[node]
    }

    /// A lower bound on `node`'s field at `p`, when one is licensed. `None` means "you must evaluate it",
    /// which is a refusal rather than a zero.
    ///
    /// A counting-only table always answers `None`: it is there to measure the walk the evaluator would
    /// otherwise have done, so offering it a skip would make the baseline and the pruned run the same run.
    #[inline]
    pub fn lower_bound(&self, node: usize, p: [f32; 3]) -> Option<f32> {
        if !self.pruning || !self.respects[node] {
            return None;
        }
        let Enclosure::Bounded { centre, radius } = self.enclosure[node] else {
            return None;
        };
        let d = length([p[0] - centre[0], p[1] - centre[1], p[2] - centre[2]]);
        Some((d - radius).max(0.0))
    }

    /// Whether a unit-direction ray from `origin` along `dir` can reach *any* surface in this subtree,
    /// descending the hierarchy instead of asking the one question a root sphere can answer.
    ///
    /// This is the structure cull_probe said was missing. `ray_may_hit` tests one sphere, and the probe
    /// measured a single root bound collapsing to "everything might hit" as soon as a scene spreads out -
    /// 0.0% of the work avoided for 32 and 128 spheres along a line, because a sphere covering a long row
    /// covers the frustum with room to spare. The same question asked of each *child* does not degrade that
    /// way: a child's sphere stays as tight as its own geometry however far apart the children are, so the
    /// ray that passes between two spheres is proved empty by the pair even though their cover permits it.
    /// That is ADR-024's `O(log N)` claim in a form a harness can measure.
    ///
    /// **`false` is a proof, and it rests on one property.** `respects[node]` says the subtree's field is at
    /// least the distance to its own enclosure, so a point with field <= 0 lies inside that sphere, so a ray
    /// that touches the surface must reach the sphere first. Combining children takes only the rule each
    /// combinator actually grants: a hard union's zero set *is* the union of its children's zero sets, so
    /// either child reaching is enough; a subtract or an intersect has a field at least its first operand's,
    /// so it inherits the first operand's reach - the carve may delete surface, it cannot create any outside.
    ///
    /// `surface` is the marcher's hit tolerance, and it is *added* to every enclosure tested on the way
    /// down. A skip is a claim about the marcher, not about the geometry: the marcher reports a hit at any
    /// point whose field is within `surface` of zero, and the field of a sphere is distance-minus-radius, so
    /// a ray that passes `surface` outside the sphere still gets reported as hitting it. Testing the bare
    /// radius instead was not hypothetical - the 8-sphere cluster row of cull_probe found exactly that ray
    /// and the frames disagreed, which is the too-small enclosure this module exists to make impossible.
    ///
    /// Everything else answers `true`, which is the safe direction: a counting table (a baseline that prunes
    /// is not a baseline), a node whose `respects` bit is off (`smin` returns less than `min`, so its field
    /// can dip below its own bound), an unbounded node, an unknown kind.
    ///
    /// **Deliberately absent: the entry distance.** The obvious next step is to return it and start the march
    /// there instead of at the camera. That was built, measured, and removed on purpose. On the test scenes it
    /// saved 8 march steps out of 85,508 on a mirror scene while changing 4 to 12 pixels per frame by up to
    /// 3x the surface epsilon: the hit *set* stayed identical, the converged points did not, because a trace
    /// starting later takes a different sequence of steps and stops somewhere else inside its tolerance. This
    /// crate's invariant is byte-identical frames (docs/04), and trading it for a saving two orders of
    /// magnitude smaller than the skip's own is a renderer policy question, not a local optimisation - it is
    /// [OQ-53](../../../docs/13-risks-open-questions.md).
    pub fn may_reach_subtree(
        &self,
        tree: &Tree,
        node: usize,
        origin: [f32; 3],
        dir: [f32; 3],
        surface: f32,
    ) -> bool {
        if !self.pruning || !self.respects[node] {
            return true;
        }
        if !may_reach(self.enclosure[node].grown(surface), origin, dir) {
            return false;
        }
        let record = tree.nodes[node];
        if record.arity() == 0 {
            return true;
        }
        let first = node + 1;
        let second = first + tree.nodes[first].subtree_len() as usize;
        match record.kind() {
            NODE_UNION => {
                self.may_reach_subtree(tree, first, origin, dir, surface)
                    || self.may_reach_subtree(tree, second, origin, dir, surface)
            }
            NODE_SUBTRACT | NODE_INTERSECT => {
                self.may_reach_subtree(tree, first, origin, dir, surface)
            }
            // Mirror duplicates surface into the reflected copy, so the child's own bound does not cover
            // the node, and any kind not listed above has no granted rule. The node's own answer, already
            // computed, is the whole claim.
            _ => true,
        }
    }
}

/// The unbounded-distance form of [`ray_may_hit`]: can the ray reach this enclosure at all, with no `max_t`
/// stopping it. Same arithmetic and the same single comparison, so the sphere-entry rule exists in exactly
/// one place — a second copy of a bound test is how a renderer ends up with a too-small enclosure.
#[inline]
pub fn may_reach(enclosure: Enclosure, origin: [f32; 3], dir: [f32; 3]) -> bool {
    ray_may_hit(enclosure, origin, dir, f32::INFINITY)
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
