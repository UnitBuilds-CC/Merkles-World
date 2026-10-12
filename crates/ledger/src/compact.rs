//! Compaction: shrink a region's write-ahead records without changing what they commit to.
//!
//! docs/03 § Compaction states the bake (`S_new_genesis = S_old_genesis ⊕ M_local`, `M_local = ∅`) and
//! names the one property a compaction must never break — it re-encodes state, it does not change it.
//! A-09 has `CompactionNotIdempotent` for the failure and nothing in this crate could produce or check
//! it. This module is where that becomes an implementation question rather than a wish.
//!
//! **What "not changing it" means here is precise, and it is worth reading before trusting the tests.**
//! The frozen A-10 shape hashes a voxel leaf at `H(NODE_VOXEL_LEAF, level, payload)` and partitions the
//! walk by position bits, so a region root depends on exactly one thing per voxel: *which payload won*.
//! The record's position in the list never enters a digest; the append rank only chooses the winner, at
//! the leaf, by greatest rank (the fix spike S2 found for the 46 ms O(N²) pre-pass). So compaction is
//! root-preserving when the survivor for each voxel is the record that would have won — and nothing else
//! about ordering matters.
//!
//! **Why a rank comes in from the caller.** "The record that would have won" is causal order, and the
//! ledger deliberately does not own causal order: commit-pipeline step 2 is the Lamport question
//! ([OQ-46](../../../docs/13-risks-open-questions.md)) and with one writer it is append order. Inside a
//! `#![no_std]` crate with no allocator there is a second reason: an in-place `sort_unstable` cannot keep
//! ties in their original order, because `sort` (stable) needs allocation. So a compaction takes ranks
//! explicitly — the same `u32` the commit path would have used as `append` — and that is why a total
//! order over `(voxel, rank)` is both deterministic and semantically the rule the root already applies.
//! [`assign_ranks`] builds them from a plain record slice for callers who *are* using append order, so
//! the common case does not have to know any of this.

use contracts::delta::{DeltaRecord, PAYLOAD_BITS};

/// One edit plus the rank that resolves it against another write to the same voxel. Larger rank wins,
/// which is the leaf rule of [docs/03 § Epoch roots](../../../docs/03-state-ledger.md) and of
/// [`crate::commit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit {
    pub rank: u32,
    pub record: DeltaRecord,
}

/// The voxel an edit addresses, as the single integer the packed word already contains: the record is
/// `slot | x | y | z | payload`, so dropping the payload bits leaves the position.
///
/// Payload is deliberately excluded, which is the whole point — two writes to one voxel with different
/// payloads collide here and one of them survives.
#[inline]
pub const fn voxel_of(record: DeltaRecord) -> u64 {
    record.as_u64() >> PAYLOAD_BITS
}

/// Interpret a plain record slice as append-ordered edits: rank `i` for the `i`th record, the same rank
/// [`crate::commit::leaves_from_records`] would have stamped. `None` when `out` is too short, because
/// this crate refuses a truncated view rather than quietly dropping the tail.
pub fn assign_ranks<'out>(
    records: &[DeltaRecord],
    out: &'out mut [Edit],
) -> Option<&'out mut [Edit]> {
    if out.len() < records.len() {
        return None;
    }
    let mut index = 0;
    while index < records.len() {
        out[index] = Edit {
            rank: index as u32,
            record: records[index],
        };
        index += 1;
    }
    Some(&mut out[..records.len()])
}

/// Collapse edits that address the same voxel, keeping the highest rank.
///
/// Returns the length of the live prefix of `edits`, which holds the survivors ordered by rank. The rest
/// of the slice is untouched scratch: compaction allocates nothing, so it writes into what the caller
/// already owns.
///
/// Idempotent by construction and cheap to check: a second call on the live prefix finds every voxel
/// already alone, so nothing is removed and the order does not move. `tests/compaction.rs` asserts that
/// against the root rather than trusting the argument - that pair is what `CompactionNotIdempotent`
/// exists to catch, and A-09's band-3 taxonomy treats "compaction changed state" as a determinism-class
/// error, not a bug report.
pub fn compact(edits: &mut [Edit]) -> usize {
    // A total order over (voxel, rank): voxel groups the collisions, rank makes the winner last within a
    // group and makes the whole comparison tie-free, so an unstable sort is deterministic anyway.
    edits.sort_unstable_by(|a, b| (voxel_of(a.record), a.rank).cmp(&(voxel_of(b.record), b.rank)));

    // Runs are now groups of one voxel. Keep the tail of each run - the greatest rank - and compact
    // toward the front. The write cursor never passes the read cursor, so this is in place.
    let mut read = 0;
    let mut write = 0;
    while read < edits.len() {
        let voxel = voxel_of(edits[read].record);
        let mut last = read;
        while last + 1 < edits.len() && voxel_of(edits[last + 1].record) == voxel {
            last += 1;
        }
        edits[write] = edits[last];
        write += 1;
        read = last + 1;
    }

    // Back to causal order, so a caller reading the live prefix sees the sequence it handed in minus the
    // shadowed writes. Rank order, not voxel order: the root does not care and everything else does.
    let live = &mut edits[..write];
    live.sort_unstable_by_key(|edit| edit.rank);
    write
}

/// Materialise the survivors as records, in rank order. `None` when `out` is too short - the same rule as
/// [`assign_ranks`], because a silently truncated compaction would commit to a different state.
pub fn write_records(edits: &[Edit], out: &mut [DeltaRecord]) -> Option<usize> {
    if out.len() < edits.len() {
        return None;
    }
    let mut index = 0;
    while index < edits.len() {
        out[index] = edits[index].record;
        index += 1;
    }
    Some(index)
}
