//! Gate P1.6: find the one voxel two replays disagree about, by descending the tree instead of
//! resending the world.
//!
//! ADR-028 made the digest, ADR-030 made the shape, and `contracts/errors.rs`'s `RootCompare` is the
//! evidence pair both sides carry. This is the walk that follows a failed compare. It is possible
//! because the shape says a region root is a *traversal*, so two roots that differ differ at a
//! findable node, and because positions come from the path rather than from any flat index docs/02
//! forbids — which is what makes the answer a voxel coordinate rather than an offset.
//!
//! What it deliberately is not: a merge. [`first_divergence`] locates the divergence, and deciding which
//! payload wins when two sides hold different ones at the same voxel is the causal question ([OQ-46]) that
//! [ADR-004] defers with multiplayer. [`catch_up`] does the one piece that needs no such rule: a peer that
//! is simply **behind** - every record it holds the leader also holds, and the leader holds more. Adopting
//! the leader's records there cannot overwrite a competing write, because there is no voxel where the two
//! disagree. Anything else comes back as [`CatchUp::Disagrees`] or [`CatchUp::NotBehind`] for the caller to
//! treat as OQ-46's problem, not this module's.
//!
//! Today's caller is single-player, so a hit in the walk still means the replay was not deterministic and
//! belongs in A-09's determinism-violation band rather than in a merge.
//!
//! Cost, measured rather than reasoned about (`cargo run --release --example p16_walk_cost -p
//! ledger`, digest counts, not microseconds - timings do not repeat across machines and these do):
//! locating a divergence costs **1.26x to 2.23x the two root passes a caller has to run anyway just to
//! notice one exists** - 372 digests against 167 at 8 records, 46,903 against 37,287 at 2,048. The ratio
//! *falls* as the region grows, because the walk's extra work is one path down twelve levels while the
//! root pass touches every leaf: the bigger the region, the more absurd the alternative of shipping it.
//! Two paths cost nothing beyond the compare: sides that agree return after their two root passes (9,914
//! digests, the walk never starts), and a peer that merely *lagged* needs no walk at all -
//! [catch_up] settles that for one extra pass of verification, which is why it exists.

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
    let mut ignored = 0usize;
    first_divergence_counted(local, remote, ladder, &mut ignored)
}

/// [`first_divergence`], counting the subtree digests the walk computes.
///
/// The count is the measurement the module's cost note needed: the walk re-derives children the root pass
/// already paid for, so this reports how much more hashing a divergence hunt costs than simply rooting both
/// sides - which is the number a caller weighs before running it on a large region.
pub fn first_divergence_counted(
    local: &mut [Leaf],
    remote: &mut [Leaf],
    ladder: &EmptyLadder,
    digests: &mut usize,
) -> Option<Divergence> {
    let mine_root = super::region_root_counted(local, ladder, digests);
    let theirs_root = super::region_root_counted(remote, ladder, digests);
    if mine_root == theirs_root {
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
                build_branch(level - 1, &mut local[lstart..lend], ladder, digests)
            };
            let theirs: Root = if rstart == rend {
                ladder.empty(level - 1)
            } else {
                build_branch(level - 1, &mut remote[rstart..rend], ladder, digests)
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

/// What [`catch_up`] decided. Returned rather than panicked about, because the common case is boring: a
/// peer that missed some of the epoch's appends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchUp {
    /// Every record the lagging side held the leader holds too, so adopting the leader's set cannot
    /// overwrite a competing write. `applied` is how many records the lagging side was missing and `root`
    /// is the digest both sides now produce - reported, not assumed, because agreeing about the root *is*
    /// the agreement the caller wanted.
    CaughtUp { applied: u32, root: Root },
    /// A voxel carries a different winning payload on each side. Which one survives is [OQ-46]'s causal
    /// question, and this module's answer is to name the position and stop.
    Disagrees {
        x: u16,
        y: u16,
        z: u16,
        mine: u16,
        theirs: u16,
    },
    /// Not a lag in this direction: the other side holds records this one lacks. Swapping the arguments
    /// turns a `NotBehind` into a `CaughtUp` when one side is simply ahead; a `NotBehind` both ways is a
    /// shared history that has diverged in its records, which is OQ-46's problem too.
    NotBehind,
    /// The destination was shorter than the leader's record set. Refused rather than truncated: an adopted
    /// prefix that roots to something would be a wrong state that looks reconciled.
    NoRoom,
}

/// Do two leaves describe the same write? Field-by-field on purpose: it is the equality that matters
/// here (same voxel, same payload, same place in the append order), and it does not ask `Leaf` to promise
/// a `PartialEq` it may not need.
fn same_write(a: &Leaf, b: &Leaf) -> bool {
    a.x == b.x && a.y == b.y && a.z == b.z && a.payload == b.payload && a.append == b.append
}

/// Is every record of `small` present, verbatim, in `big`?
fn subset(small: &[Leaf], big: &[Leaf]) -> bool {
    small
        .iter()
        .all(|mine| big.iter().any(|theirs| same_write(mine, theirs)))
}

/// The payload a side commits to at one voxel: greatest `append`, which is the rule `region_root`'s leaf
/// pass uses, so a comparison here cannot disagree with the root it is explaining.
fn winner_at(leaves: &[Leaf], x: u16, y: u16, z: u16) -> Option<u16> {
    let mut best: Option<&Leaf> = None;
    for leaf in leaves {
        if leaf.x != x || leaf.y != y || leaf.z != z {
            continue;
        }
        best = Some(match best {
            None => leaf,
            Some(current) if leaf.append >= current.append => leaf,
            Some(current) => current,
        });
    }
    best.map(|leaf| leaf.payload)
}

/// Reconcile the one divergence shape that needs no causal rule: a lagging peer whose records are a
/// verbatim subset of the leader's.
///
/// On [`CatchUp::CaughtUp`] the leader's record set is copied into `out` (whose length must therefore
/// reach `theirs.len()`), and `out` roots to `root`; the caller swaps its own buffer for that. No
/// reordering, dedup or arbitration happens on the way: append order already decides a shared voxel, so
/// copying the leader's records carries its decision with them.
///
/// Cost, stated rather than glossed: `O(mine x theirs)` scans plus, on the success path, one full region
/// root. That is a reconciliation, run when two replays have already been shown to differ, not a hot
/// path - and unlike [`first_divergence`] it never hashes per level, because it is not looking for the
/// voxel: the walk already did.
pub fn catch_up(mine: &[Leaf], theirs: &[Leaf], out: &mut [Leaf], ladder: &EmptyLadder) -> CatchUp {
    if subset(mine, theirs) {
        if out.len() < theirs.len() {
            return CatchUp::NoRoom;
        }
        out[..theirs.len()].copy_from_slice(theirs);
        let root = super::region_root(&mut out[..theirs.len()], ladder);
        return CatchUp::CaughtUp {
            // Both sides' counts are exact here: `subset` proved every one of mine is in theirs.
            applied: (theirs.len() - mine.len()) as u32,
            root,
        };
    }
    if subset(theirs, mine) {
        return CatchUp::NotBehind;
    }
    for leaf in mine {
        if let (Some(mine_winner), Some(their_winner)) = (
            winner_at(mine, leaf.x, leaf.y, leaf.z),
            winner_at(theirs, leaf.x, leaf.y, leaf.z),
        ) {
            if mine_winner != their_winner {
                return CatchUp::Disagrees {
                    x: leaf.x,
                    y: leaf.y,
                    z: leaf.z,
                    mine: mine_winner,
                    theirs: their_winner,
                };
            }
        }
    }
    CatchUp::NotBehind
}
