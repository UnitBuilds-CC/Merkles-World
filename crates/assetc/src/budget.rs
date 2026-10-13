//! Compile-time cost accounting for a lowered shape, so an over-budget SDF is refused by the compiler
//! rather than discovered as a frame drop (doc 09's gate AS5, doc 04's cost law).
//!
//! **What this is and is not.** The *mechanism* is here: a cost read off the compiled asset, a budget the
//! caller supplies, and a refusal that names the clause it tripped. The **number** is not chosen here -
//! doc 09 says so out loud ("which needs a per-tree budget the docs do not yet define"), and picking one
//! would be picking a frame budget for a GPU that does not exist yet. So `assetc --budget ...` is a flag
//! an author or CI can set today, and AS5 stays open until someone with the frame-time claim in hand sets
//! the default.
//!
//! The shape of the cost comes from doc 04's measured law - a frame's work is
//! `steps x nodes visited per step`, flat across resolutions on the boot's scene - so the per-ray bound is
//! `nodes * step_budget`. That is deliberately an upper bound rather than a prediction: the march can visit
//! fewer nodes than exist, the cull can skip subtrees, and the raster's own accounting charges a missed ray
//! the whole step budget. `tests/budget.rs` checks the bound actually bounds, comparing it against the
//! evaluations a real frame reports; an estimate that has never been compared to a measurement is the kind
//! of number a gate quietly stops meaning.
//!
//! Depth is reported too, though `MAX_TREE_DEPTH` already refuses a tree past the grammar's cap - a budget
//! tighter than the cap is the interesting case, because depth is what a marcher's step cost grows with
//! once the tree is deep rather than wide.

use crate::Asset;

/// What a compiled asset will cost a march, read off the records rather than guessed from the authored
/// shape - the bytes are the thing that ships, so the bytes are the thing measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cost {
    /// Node records in the tree. Every field evaluation walks at most this many.
    pub nodes: u32,
    /// Levels below the root, as the compiler derived them.
    pub depth: u32,
    /// `f32` pool entries, which is the asset's memory cost rather than its time cost.
    pub params: u32,
    /// Nodes whose subtree cannot be bounded by a sphere (`plane`, `repeat`, and the deformations the
    /// cull module refuses). Reported because they are what defeats the ray early-out, not because the
    /// node count hides them.
    pub unbounded: u32,
}

impl Cost {
    /// The upper bound on one ray's field evaluations at a given step budget: a ray may take that many
    /// steps, and no step can evaluate more nodes than exist.
    #[must_use]
    pub fn worst_evaluations_per_ray(&self, step_budget: u32) -> u64 {
        u64::from(self.nodes) * u64::from(step_budget)
    }
}

/// A caller-supplied ceiling. `step_budget` is the raster's per-ray `max_steps`, which is a camera
/// property rather than an asset property - so it is passed in per check, and a budget only means
/// something against a stated step count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub max_nodes: u32,
    pub max_depth: u32,
    pub max_evaluations_per_ray: u64,
}

/// Which clause a cost tripped. Enumerated rather than a sentence so `--budget` can report every failure
/// at once: an author fixing a tree one rejection at a time learns nothing about how far off they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Violation {
    TooManyNodes { found: u32, limit: u32 },
    TooDeep { found: u32, limit: u32 },
    TooExpensive { found: u64, limit: u64 },
}

impl core::fmt::Display for Violation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Violation::TooManyNodes { found, limit } => {
                write!(f, "{found} nodes, budget allows {limit}")
            }
            Violation::TooDeep { found, limit } => {
                write!(f, "depth {found}, budget allows {limit}")
            }
            Violation::TooExpensive { found, limit } => {
                write!(
                    f,
                    "worst-case {found} evaluations per ray, budget allows {limit}"
                )
            }
        }
    }
}

/// Read the cost off a compiled asset.
#[must_use]
pub fn cost_of(asset: &Asset) -> Cost {
    let unbounded = asset
        .tree
        .nodes
        .iter()
        .filter(|record| {
            matches!(
                record.kind(),
                contracts::sdf::NODE_PLANE | contracts::sdf::NODE_REPEAT
            )
        })
        .count() as u32;
    Cost {
        nodes: asset.tree.nodes.len() as u32,
        depth: asset.depth,
        params: asset.tree.params.len() as u32,
        unbounded,
    }
}

/// Every clause the cost fails, empty when it fits.
#[must_use]
pub fn violations(cost: &Cost, budget: &Budget, step_budget: u32) -> Vec<Violation> {
    let mut found = Vec::new();
    if cost.nodes > budget.max_nodes {
        found.push(Violation::TooManyNodes {
            found: cost.nodes,
            limit: budget.max_nodes,
        });
    }
    if cost.depth > budget.max_depth {
        found.push(Violation::TooDeep {
            found: cost.depth,
            limit: budget.max_depth,
        });
    }
    let worst = cost.worst_evaluations_per_ray(step_budget);
    if worst > budget.max_evaluations_per_ray {
        found.push(Violation::TooExpensive {
            found: worst,
            limit: budget.max_evaluations_per_ray,
        });
    }
    found
}
