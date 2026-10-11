//! The epoch commit path: records as the engine holds them in slabs, out as a root a peer compares.
//!
//! This is [docs/03 § The commit pipeline](../../../docs/03-state-ledger.md) restricted to the steps
//! that are specified today:
//!
//! | Pipeline step | Here? |
//! |---------------|-------|
//! | 1. flush outstanding deltas into the pool | the caller's slab contents are the pool; `RegionEdits` is that hand-off |
//! | 2. order by Lamport clock | **not here** — the caller hands records in causal append order, because the clock's meaning is [OQ-46](../../../docs/13-risks-open-questions.md) and step 2 is multiplayer's |
//! | 3. apply against the macro rule set for Δt | **not here** — that is [OQ-01](../../../docs/13-risks-open-questions.md), the project's largest research risk, and `crates/sim`'s |
//! | 4. recompute **changed** region roots only | yes: slots with no records keep the empty ladder and are never walked |
//! | 5. append event tags | the leaves arrive pre-formed; the registry is A-05's ([OQ-08](../../../docs/13-risks-open-questions.md)) |
//! | 6. emit the epoch root | yes, through `ledger::epoch_root` |
//! | 7. bump-pointer reset of consumed slabs | `crates/memory::Slab::reset`, the caller's job after this returns |
//!
//! Single-player first ([ADR-004](../../../docs/decisions.md)) is the reason 2 and 3 are the caller's:
//! with one writer, append order *is* causal order, and with no macro rules yet, nothing transforms
//! the payload. Both assumptions are recorded here rather than assumed silently, and the tests assert
//! the one property that follows — the root is **not** arrival-order-independent for two records that
//! touch the same voxel.

use super::{
    epoch_root, list_root, region_leaf, region_root, slot_leaves, EmptyLadder, Leaf, Root,
};
use contracts::coords::RegionKey;
use contracts::delta::{DeltaRecord, EpochHeader};
use contracts::tree::OCTREE_DEPTH;

/// One region's outstanding edits for the epoch, in the causal append order the caller resolved.
///
/// `slot` is this epoch's region-slot table index (ADR-029), not a region key: the table is what
/// turns it back into coordinates, and the table itself is under its own root for exactly that
/// reason.
pub struct RegionEdits<'records> {
    pub slot: u16,
    pub records: &'records [DeltaRecord],
}

/// The four commitments an epoch root is made of, kept so a caller can log or bisect without
/// recomputing them (gate P1.6's walk starts by comparing these, not the epoch root).
pub struct EpochCommit {
    pub slot_table: Root,
    pub regions: Root,
    pub events: Root,
    pub epoch: Root,
}

/// Records in append order → the leaves the octree walks, `append` carrying the causal position.
///
/// This used to be a separate O(N²) dedupe — `resolve_last_write_wins` — and spike S2 priced it at
/// 46 ms per full 64 KB slab, more than the entire hashing pass. It is now one pass that writes
/// `records.len()` leaves and lets the walk resolve collisions at the leaf (`build_branch`,
/// level 0), so resolution is `O(N)` here and `O(1)` per collision at the tree.
///
/// `None` when the leaf space cannot hold every record: SPEC-06's capacity question wearing its
/// operational clothes.
pub fn leaves_from_records<'out>(
    records: &[DeltaRecord],
    out: &'out mut [Leaf],
) -> Option<&'out mut [Leaf]> {
    if out.len() < records.len() {
        return None;
    }
    let mut index = 0;
    while index < records.len() {
        out[index] = Leaf::from_record(records[index], index as u32);
        index += 1;
    }
    Some(&mut out[..records.len()])
}

/// A region's root from the records its slab holds. `None` if the leaf space was too short.
pub fn region_root_for(
    records: &[DeltaRecord],
    scratch: &mut [Leaf],
    ladder: &EmptyLadder,
) -> Option<Root> {
    let leaves = leaves_from_records(records, scratch)?;
    Some(region_root(leaves, ladder))
}

/// What the caller hands in: the epoch's inputs, borrowed, already ordered the way the pipeline
/// expects.
pub struct CommitRequest<'a> {
    /// This epoch's region-slot table, in ascending slot order (ADR-029's first-touch mapping).
    pub region_slots: &'a [RegionKey],
    /// One row per region the epoch edited. A slot with no row commits to the empty ladder.
    pub edits: &'a [RegionEdits<'a>],
    /// Event leaves in emission order (pipeline step 5); the registry behind them is A-05's.
    pub event_leaves: &'a [Root],
    /// The intensional base-terrain commitment: `genesis_root(seed, base_rule_id, rule_version)`.
    pub base_genesis_root: Root,
    pub epoch_index: u64,
}

/// The caller's working memory. `no_std` means the hasher allocates nothing, so whoever owns the
/// arena owns the scratch — which is SPEC-06's question wearing a parameter.
///
/// `slot_rows` and `region_leaves` need `region_slots.len()` entries; `leaf_space` needs the busiest
/// region's record count. Short space is a refusal, never a truncated root.
pub struct CommitScratch<'a> {
    pub slot_rows: &'a mut [Root],
    pub region_leaves: &'a mut [Root],
    pub leaf_space: &'a mut [Leaf],
}

/// The whole epoch: slot table, region list, event list, and the root over all three plus the header.
///
/// Every slot in `region_slots` gets a region leaf, in ascending slot order; a slot with no
/// `RegionEdits` row commits to the empty ladder's top, which is why an untouched region and an
/// absent region cannot be confused.
pub fn commit_epoch(
    ladder: &EmptyLadder,
    request: CommitRequest<'_>,
    scratch: &mut CommitScratch,
) -> Option<EpochCommit> {
    let CommitRequest {
        region_slots,
        edits,
        event_leaves,
        base_genesis_root,
        epoch_index,
    } = request;
    if scratch.slot_rows.len() < region_slots.len()
        || scratch.region_leaves.len() < region_slots.len()
    {
        return None;
    }
    let rows = slot_leaves(region_slots, &mut *scratch.slot_rows)?;
    let slot_table = list_root(rows);

    let mut record_count: usize = 0;
    let mut slot_index = 0;
    while slot_index < region_slots.len() {
        let mut root = ladder.empty(OCTREE_DEPTH);
        for row in edits {
            if row.slot == slot_index as u16 {
                record_count += row.records.len();
                root = region_root_for(row.records, &mut *scratch.leaf_space, ladder)?;
                break;
            }
        }
        scratch.region_leaves[slot_index] = region_leaf(slot_index as u16, &root);
        slot_index += 1;
    }
    let regions = list_root(scratch.region_leaves);
    let events = list_root(event_leaves);

    let header = EpochHeader {
        epoch_index,
        base_genesis_root,
        event_log_root: events,
        region_slots,
        // docs/03: the high-water mark of records in the epoch stream. Here that is the count this
        // commit consumed; whether it is a bump pointer to reset or a cumulative figure is
        // SPEC-06's, and OQ-45's owner will care before a second epoch commits.
        record_count: record_count as u32,
    };
    let epoch = epoch_root(&header, &slot_table, &regions, &events);
    Some(EpochCommit {
        slot_table,
        regions,
        events,
        epoch,
    })
}
