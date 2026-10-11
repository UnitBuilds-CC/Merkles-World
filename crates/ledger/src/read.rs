//! The read side of the ledger: what a voxel holds at the end of an epoch, from the records alone.
//!
//! The commit path turns records into a root; this answers the other question the pitch demo needs —
//! *did my edit persist?* — by walking a position back through the epoch's slot table to the record
//! that covers it. It is `contracts::delta::EpochHeader::voxel_of` run in reverse, and it is the
//! precondition for [OQ-47]'s gate: nothing can measure *when* an edit becomes visible until
//! something can say what a voxel commits to.
//!
//! The base terrain is deliberately out of scope. A voxel nobody edited is reported as absent, not
//! as `0`: what a base rule would generate there is [SPEC-04]'s terrain half, which is
//! [OQ-27](https://github.com/UnitBuilds-CC/Merkles-World/blob/main/docs/13-risks-open-questions.md),
//! and no document in this repo specifies it yet. A caller that wants terrain fills the gap with the
//! generator; this module refuses to invent one.
//!
//! [OQ-47]: https://github.com/UnitBuilds-CC/Merkles-World/blob/main/docs/13-risks-open-questions.md
//! [SPEC-04]: https://github.com/UnitBuilds-CC/Merkles-World/blob/main/docs/13-risks-open-questions.md

use super::commit::RegionEdits;
use contracts::coords::{GlobalVoxelPos, RegionKey};

/// What the ledger commits a voxel to, for one epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Committed {
    /// A record in this epoch's slab for this region writes this payload. `append` is the record's
    /// position in the epoch's causal order, so a caller can say *which* write is live, not merely
    /// what it holds — that is what makes a visibility gate statable.
    Edited { payload: u16, append: usize },
    /// No record in this epoch touched the voxel. Absence, not a value.
    Unedited,
}

/// The slot `region` holds in this epoch's table, or `None` if the epoch never addressed it.
///
/// Linear in the slot count, which is right: the table is ADR-029's per-epoch array (bounded by
/// 65,536) and the write side assigns slots by first-touch scan already, so a map here would be a
/// second index to keep in sync — and on the digest path an unranked hash would be an
/// [OQ-44](https://github.com/UnitBuilds-CC/Merkles-World/blob/main/docs/13-risks-open-questions.md)
/// exposure for the sake of asymptotics nobody has asked for.
#[inline]
pub fn slot_of(region_slots: &[RegionKey], region: &RegionKey) -> Option<u16> {
    let mut index = 0;
    while index < region_slots.len() {
        if region_slots[index] == *region {
            return Some(index as u16);
        }
        index += 1;
    }
    None
}

/// What the epoch commits at `position`.
///
/// Later records win, in the order the caller supplied them — the same rule
/// `crates/ledger`'s tree builder applies at the leaf, so the read and the root cannot disagree
/// about which write is live. That equivalence is what `the_read_and_the_root_never_disagree`
/// tests, and it is the property an `M_active` projection has to be able to rely on.
#[inline]
pub fn committed_at(
    region_slots: &[RegionKey],
    edits: &[RegionEdits],
    position: &GlobalVoxelPos,
) -> Committed {
    let offset = position.local_offset();
    let Some(slot) = slot_of(region_slots, &position.region_key()) else {
        return Committed::Unedited;
    };
    let mut answer = Committed::Unedited;
    for row in edits {
        if row.slot != slot {
            continue;
        }
        for (index, record) in row.records.iter().enumerate() {
            let intra = record.intra_region();
            if intra.x == offset.x && intra.y == offset.y && intra.z == offset.z {
                answer = Committed::Edited {
                    payload: record.payload(),
                    append: index,
                };
            }
        }
    }
    answer
}

/// Every position the epoch edits, as global coordinates, into a caller-owned buffer — `None` when
/// the buffer is short. The set is what the region root commits to, so a caller can diff two epochs
/// or rebuild a region's overlay without walking the tree, and the ledger still allocates nothing.
pub fn edited_positions<'out>(
    region_slots: &[RegionKey],
    edits: &[RegionEdits],
    out: &'out mut [GlobalVoxelPos],
) -> Option<&'out mut [GlobalVoxelPos]> {
    let total: usize = edits.iter().map(|row| row.records.len()).sum();
    if out.len() < total {
        return None;
    }
    let mut index = 0;
    for row in edits {
        let region = *region_slots.get(row.slot as usize)?;
        for record in row.records {
            out[index] = contracts::delta::voxel_at(&region, &record.intra_region());
            index += 1;
        }
    }
    Some(&mut out[..total])
}
