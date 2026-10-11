//! Gate P1.6: find the one voxel two replays disagree about, by descending the tree instead of
//! resending the world.
//!
//! ADR-028 made the digest, ADR-030 made the shape, and `contracts/errors.rs`'s `RootCompare` is the
//! evidence pair both sides carry. This is the walk that follows a failed compare. It is possible
//! because the shape says a region root is a *traversal*, so two roots that differ differ at a
//! findable node, and because positions come from the path rather than from any flat index docs/02
//! forbids — which is what makes the answer a voxel coordinate rather than an offset.
//!
//! What it deliberately is not: a repair. This locates the divergence; deciding which side wins is
//! the causal question ([OQ-46]) that [ADR-004] defers with multiplayer. Today's caller is
//! single-player, so a hit here means the replay was not deterministic and belongs in A-09's
//! determinism-violation band, not in a merge.
//!
//! Cost, stated rather than glossed: every level hashes both sides' children until it finds the
//! differing one, so the walk is up to 8 subtree hashes per level per side against a root pass that
//! computes them once. That is the price of locating a divergence without shipping the region, and
//! the path runs rarely by design; the measurement belongs with the spike that gates P1.6.
//!
//! [OQ-46]: https://github.com/UnitBuilds-CC/Merkles-World/blob/main/docs/13-risks-open-questions.md
//! [ADR-004]: https://github.com/UnitBuilds-CC/Merkles-World/blob/main/docs/decisions.md

use super::{build_branch, leaf_winner, octant_runs, EmptyLadder, Leaf, Root};
use contracts::tree::{OCTREE_ARITY, OCTREE_DEPTH};

/// Where two sides part company, and what each holds there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divergence {
    /// The voxel inside the region — three axes, never a packed index.
    pub x: u16,
    pub y: u16,
    pub z: u16,
    /// The payload each side commits to at that position. `None` means that side holds no record
    /// for the voxel at all: the ordinary desync is one peer having an edit the other never got,
    /// and a fake `0` there would read as agreement with an unedited voxel.
    pub local: Option<u16>,
    pub remote: Option<u16>,
}

/// `None` when the two leaf sets produce the same region root — which is the contract with the
/// compare it follows: this walk never reports a divergence the root comparison did not already see.
///
/// Both slices are rearranged in place as the walk partitions each level, exactly as `region_root`
/// does; contents never change.
pub fn first_divergence(
    local: &mut [Leaf],
    remote: &mut [Leaf],
    ladder: &EmptyLadder,
) -> Option<Divergence> {
    if super::region_root(local, ladder) == super::region_root(remote, ladder) {
        return None;
    }

    let mut level = OCTREE_DEPTH;
    let mut left = (0usize, local.len());
    let mut right = (0usize, remote.len());
    loop {
        if level == 0 {
            let mine = if left.1 > left.0 {
                Some(leaf_winner(&local[left.0..left.1]))
            } else {
                None
            };
            let theirs = if right.1 > right.0 {
                Some(leaf_winner(&remote[right.0..right.1]))
            } else {
                None
            };
            let anchor = mine
                .or(theirs)
                .expect("a differing subtree has at least one side holding a leaf");
            return Some(Divergence {
                x: anchor.x,
                y: anchor.y,
                z: anchor.z,
                local: mine.map(|leaf| leaf.payload),
                remote: theirs.map(|leaf| leaf.payload),
            });
        }

        let left_runs = octant_runs(level, &mut local[left.0..left.1]);
        let right_runs = octant_runs(level, &mut remote[right.0..right.1]);
        let mut descent = None;
        let mut k = 0;
        while k < OCTREE_ARITY {
            let (lstart, lend) = (left.0 + left_runs[k], left.0 + left_runs[k + 1]);
            let (rstart, rend) = (right.0 + right_runs[k], right.0 + right_runs[k + 1]);
            let mine: Root = if lstart == lend {
                ladder.empty(level - 1)
            } else {
                let mut counted = 0usize;
                build_branch(level - 1, &mut local[lstart..lend], ladder, &mut counted)
            };
            let theirs: Root = if rstart == rend {
                ladder.empty(level - 1)
            } else {
                let mut counted = 0usize;
                build_branch(level - 1, &mut remote[rstart..rend], ladder, &mut counted)
            };
            if mine != theirs {
                descent = Some(((lstart, lend), (rstart, rend)));
                break;
            }
            k += 1;
        }
        let (next_left, next_right) =
            descent.expect("roots that differ have at least one differing child");
        left = next_left;
        right = next_right;
        level -= 1;
    }
}
