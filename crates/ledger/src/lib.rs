//! crates/ledger — the A-10 hasher. Behaviour, at last.
//!
//! `contracts/` is `#![no_std]` and holds no behaviour (AGENTS.md §2, docs/03 § What this shape
//! deliberately does not do), which is why artifact A-10 was able to pin a tree *shape* while
//! nothing in this workspace could compute a root. This crate is the executor: it takes
//! `contracts/tree.rs`'s kinds, levels, child order and encoders as the only vocabulary it is
//! allowed, and turns them into the 32-byte digests gate P1.5 compares across targets.
//!
//! Rules this crate keeps, and why:
//!
//! - **The grammar is never restated.** Every kind byte, level, domain prefix and integer width is
//!   imported from `contracts::tree`. A local copy of `NODE_VOXEL_LEAF` would be a second source of
//!   truth — the exact failure A-10 exists to close.
//! - **`no_std`, no allocator.** Not for elegance: the epoch root is the value four targets must
//!   reproduce bitwise, and M1-browser is one of them (docs/11 § The Bitwise Determinism Suite). What
//!   that costs is stated where it is paid, in `sort_walk_order` and in the caller-owned buffers.
//! - **Integer bytes only.** Nothing here takes a float, so no `f32` can reach a preimage
//!   (AGENTS.md §4). Every integer enters through A-10's own little-endian encoders.
//!
//! Still not decided here, because this crate does not own it: what the 12 payload bits mean
//! (OQ-45 — they are hashed opaquely), the event-tag registry (A-05 — kind 4 is a slot in the
//! grammar, not a schema), the on-disk and wire encodings (OQ-23, OQ-30).

#![no_std]

/// The epoch commit path: records out of the slabs, roots a peer compares.
pub mod commit;

/// Gate P1.6's bisection walk: locate the divergent voxel behind a differing root.
pub mod repair;

use contracts::coords::RegionKey;
use contracts::delta::{DeltaRecord, EpochHeader, PAYLOAD_BITS};
use contracts::errors::ROOT_BYTES;
use contracts::tree::{
    i64_bytes, octant_at, u16_bytes, u32_bytes, u64_bytes, DOMAIN, LEAF_PAYLOAD_BYTES, LEVEL_LEAF,
    LIST_ARITY, NODE_EMPTY_LIST, NODE_EMPTY_OCTREE, NODE_ENTITY_STATE, NODE_EPOCH_ROOT,
    NODE_EVENT_TAG, NODE_GENESIS, NODE_LIST_BRANCH, NODE_OCTANT_BRANCH, NODE_REGION_LEAF,
    NODE_SLOT_LEAF, NODE_VOXEL_LEAF, OCTREE_ARITY, OCTREE_DEPTH,
};

/// A canonical digest: BLAKE3-256, 32 bytes (ADR-028).
pub type Root = [u8; ROOT_BYTES];

/// The octree's height above the voxel leaves, plus the leaf level itself: the length of one empty
/// ladder.
pub const LADDER_LEN: usize = OCTREE_DEPTH as usize + 1;

/// The 12 bits a record's payload field actually carries (ADR-029). A leaf hashes the masked value,
/// so a wider `u16` handed to `voxel_leaf` cannot smuggle bits past the shape: the spare four bits
/// are room OQ-45 has not spoken for yet.
pub const PAYLOAD_MASK: u16 = (1u16 << PAYLOAD_BITS) - 1;

// ---------------------------------------------------------------------------
// The one hashing entry point
// ---------------------------------------------------------------------------

/// `DOMAIN ‖ kind ‖ level ‖ concat(parts)` → BLAKE3-256. A-10's preimage layout, executed.
///
/// Parts rather than one buffer, because an octree branch carries eight digests and an event tag
/// carries an opaque payload no `no_std` frame can size — and streaming them gives byte-identical
/// input. `contracts::tree::preimage` is the reference builder, and
/// `streamed_parts_match_the_reference_preimage` pins the two against each other.
#[inline]
pub fn node_parts(kind: u8, level: u8, parts: &[&[u8]]) -> Root {
    let mut hasher = blake3::Hasher::new();
    hasher.update(DOMAIN);
    hasher.update(&[kind, level]);
    let mut i = 0;
    while i < parts.len() {
        hasher.update(parts[i]);
        i += 1;
    }
    *hasher.finalize().as_bytes()
}

/// See `node_parts`.
#[inline]
pub fn node(kind: u8, level: u8, payload: &[u8]) -> Root {
    node_parts(kind, level, &[payload])
}

// ---------------------------------------------------------------------------
// The octree
// ---------------------------------------------------------------------------

/// One edited voxel as the octree sees it: `LocalOffset`'s three axes plus the record's payload.
/// A record's *slot* is not here — it selects which region's tree the leaf belongs to and never
/// enters the leaf's preimage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Leaf {
    pub x: u16,
    pub y: u16,
    pub z: u16,
    pub payload: u16,
    /// Where this record sat in the epoch's append order. **This is what resolves two writes to one
    /// voxel**: the walk takes the greatest `append` at the leaf, so last-write-wins costs one
    /// comparison instead of a pre-pass (docs/03 § Epoch roots, spike S2). Unique positions never
    /// consult it, and a hand-built leaf can leave it `0`.
    pub append: u32,
}

/// A zeroed leaf, for callers filling arrays before the records arrive.
impl Leaf {
    pub const DEFAULT: Self = Self {
        x: 0,
        y: 0,
        z: 0,
        payload: 0,
        append: 0,
    };
}

impl Leaf {
    /// The leaf a record writes inside its own region, at its position in the epoch's append order.
    #[inline]
    pub const fn from_record(record: DeltaRecord, append: u32) -> Self {
        let offset = record.intra_region();
        Self {
            x: offset.x,
            y: offset.y,
            z: offset.z,
            payload: record.payload(),
            append,
        }
    }
}

/// `leaf(x, y, z) = H(NODE_VOXEL_LEAF, level 0, LE_u16(masked payload))`.
///
/// Position is deliberately absent from the payload: it comes from the path, which is what keeps a
/// flat voxel index out of the digest entirely (docs/02 § Octree keys and Morton codes).
#[inline]
pub fn voxel_leaf(payload: u16) -> Root {
    let masked = u16_bytes(payload & PAYLOAD_MASK);
    node(NODE_VOXEL_LEAF, LEVEL_LEAF, &masked)
}

/// `node(l, children) = H(NODE_OCTANT_BRANCH, level l, d(children[0]) ‖ … ‖ d(children[7]))`.
///
/// The array *is* the order: children are hashed at index 0..7, and A-10's `octant_at` puts x in the
/// low bit, so index order means `(0,0,0) (1,0,0) (0,1,0) (1,1,0) (0,0,1) (1,0,1) (0,1,1) (1,1,1)`.
#[inline]
pub fn octant_branch(level: u32, children: &[Root; OCTREE_ARITY]) -> Root {
    let mut parts = [&[][..]; OCTREE_ARITY];
    let mut k = 0;
    while k < OCTREE_ARITY {
        parts[k] = &children[k];
        k += 1;
    }
    node_parts(NODE_OCTANT_BRANCH, level as u8, &parts)
}

/// The 13 digests an absent subtree takes, one per level, derived from A-10's single rule:
/// `empty(0) = H(NODE_EMPTY_OCTREE, 0, ∅)` and `empty(l) = branch(l, [empty(l−1); 8])`.
///
/// Computed once and carried by the caller — that is what docs/03 means by "a constant of the
/// grammar, 13 digests computed once at startup, not per region". There is no global cache because a
/// `no_std` one would need a lock or a dependency, and a root may not depend on either.
pub struct EmptyLadder {
    levels: [Root; LADDER_LEN],
}

impl EmptyLadder {
    /// `OCTREE_DEPTH + 1` branch hashes. Nothing else in the ledger has a startup cost.
    pub fn compute() -> Self {
        let mut levels = [[0u8; ROOT_BYTES]; LADDER_LEN];
        levels[0] = node(NODE_EMPTY_OCTREE, LEVEL_LEAF, &[]);
        let mut level = 1;
        while level < LADDER_LEN {
            let mut children = [[0u8; ROOT_BYTES]; OCTREE_ARITY];
            let mut k = 0;
            while k < OCTREE_ARITY {
                children[k] = levels[level - 1];
                k += 1;
            }
            levels[level] = octant_branch(level as u32, &children);
            level += 1;
        }
        Self { levels }
    }

    /// The digest an absent subtree at `level` carries. A level above the region root is a caller
    /// error, said out loud rather than indexed past.
    #[inline]
    pub fn empty(&self, level: u32) -> Root {
        debug_assert!((level as usize) < LADDER_LEN);
        self.levels[level as usize]
    }
}

/// Rearrange `leaves` into the octants of level `level`, in octant index order, so each octant's
/// leaves form one contiguous run and a subtree's slice can be recursed into directly.
///
/// **In place, allocation-free, and O(8·len) per call** — one forward selection scan per octant,
/// swapping matches to the front. That is deliberately not a comparison sort: `core` has no sort
/// without an allocator, and spike S2 measured the obvious allocation-free alternative (an insertion
/// sort over a walk-order comparator) at O(N²), which dominated the whole pass — 10,000 edits cost
/// 133 ms where the 120,000 BLAKE3 calls behind them cost about 8 ms. The ordering is a scaffolding
/// detail of the builder, not part of the digest: the root is position-keyed, so any arrangement
/// that keeps a subtree's leaves together hashes to the same value (proved by
/// `region_root_is_position_keyed_not_arrival_keyed`, and re-proved after each of these passes
/// changes).
fn partition_by_octant(level: u32, leaves: &mut [Leaf]) {
    let mut write = 0;
    let mut octant = 0usize;
    while octant < OCTREE_ARITY {
        let mut read = write;
        while read < leaves.len() {
            if octant_at(leaves[read].x, leaves[read].y, leaves[read].z, level) == octant {
                leaves.swap(write, read);
                write += 1;
            }
            read += 1;
        }
        octant += 1;
    }
}

/// A region's root: its edited voxels and nothing else, everything unedited being the ladder.
///
/// Duplicate positions are allowed and **resolved by the walk**: two leaves for one voxel commit the
/// one with the greater `append`, which is docs/03's last-write-wins in causal order, applied at the
/// leaf instead of in a pass before it. A caller with no causal order to offer (a fresh, unique set
/// of positions) can leave every `append` at `0` and get the same root either way.
pub fn region_root(leaves: &mut [Leaf], ladder: &EmptyLadder) -> Root {
    let mut counted = 0usize;
    build_branch(OCTREE_DEPTH, leaves, ladder, &mut counted)
}

/// `region_root`, but reporting how many digests it actually computed — one per node visited, leaf
/// and branch alike. Spike S2 needs this because docs/03's `12N` cost model is an **upper bound**:
/// leaves that share a path cost one branch between them, so a clustered epoch recomputes far fewer
/// nodes than `12N` and a scattered one approaches it. Dividing a measured pass by `12N` instead of
/// by the node count understates the per-hash cost, which is exactly the arithmetic OQ-49 turns on.
pub fn region_root_counted(leaves: &mut [Leaf], ladder: &EmptyLadder, digests: &mut usize) -> Root {
    build_branch(OCTREE_DEPTH, leaves, ladder, digests)
}

pub(crate) fn build_branch(
    level: u32,
    leaves: &mut [Leaf],
    ladder: &EmptyLadder,
    counted: &mut usize,
) -> Root {
    if leaves.is_empty() {
        return ladder.empty(level);
    }
    if level == 0 {
        *counted += 1;
        return voxel_leaf(leaf_winner(leaves).payload);
    }
    let runs = octant_runs(level, leaves);
    let mut children = [[0u8; ROOT_BYTES]; OCTREE_ARITY];
    let mut k = 0;
    while k < OCTREE_ARITY {
        children[k] = if runs[k] == runs[k + 1] {
            ladder.empty(level - 1)
        } else {
            build_branch(
                level - 1,
                &mut leaves[runs[k]..runs[k + 1]],
                ladder,
                counted,
            )
        };
        k += 1;
    }
    *counted += 1;
    octant_branch(level, &children)
}

/// The leaf a run of records commits to: the greatest `append`, which is docs/03's last-write-wins
///
/// Two records for one voxel sharing an `append` are the caller's ambiguity, not this function's:
/// docs/03 already says the root is not arrival-order-independent for two writes to one position, and
/// an unranked pair is that case with no rule to break it — so the winner falls to run order, and a
/// replay in another order will differ. `leaves_from_records` ranks by construction; a hand-built set
/// that wants the ranked behaviour must rank it too. `ranked_duplicates_are_order_independent_and_
/// unranked_ones_are_not` measures both halves rather than asserting them.
/// applied at the position instead of in a pre-pass (spike S2 measured the pre-pass at 46 ms per full
/// slab, more than the hashing itself).
#[inline]
pub(crate) fn leaf_winner(leaves: &[Leaf]) -> &Leaf {
    let mut winner = 0;
    let mut index = 1;
    while index < leaves.len() {
        if leaves[index].append > leaves[winner].append {
            winner = index;
        }
        index += 1;
    }
    &leaves[winner]
}

/// Partition `leaves` by octant at `level` and return the nine run boundaries. A run of length zero
/// is an absent octant, which costs no hashing at all — the ladder covers it for free.
pub(crate) fn octant_runs(level: u32, leaves: &mut [Leaf]) -> [usize; OCTREE_ARITY + 1] {
    partition_by_octant(level, leaves);
    let mut starts = [0usize; OCTREE_ARITY + 1];
    let mut octant = 0usize;
    let mut cursor = 0;
    while octant < OCTREE_ARITY {
        starts[octant] = cursor;
        while cursor < leaves.len()
            && octant_at(leaves[cursor].x, leaves[cursor].y, leaves[cursor].z, level) == octant
        {
            cursor += 1;
        }
        octant += 1;
    }
    starts[OCTREE_ARITY] = leaves.len();
    starts
}

// ---------------------------------------------------------------------------
// Ordered collections, as binary lists
// ---------------------------------------------------------------------------

/// `list(∅) = H(NODE_EMPTY_LIST, 0, ∅)`; `list([e]) = e`; otherwise a binary branch over the halves,
/// an odd trailing item paired with **itself**.
///
/// Both halves of that are load-bearing and both are pinned by tests: the single item is *not*
/// wrapped, so `[a]` differs from `[a, a]`; and pairing rather than carrying forward means a
/// truncated list can never agree with a longer one at the root (ADR-030 § Rejected).
pub fn list_root(items: &[Root]) -> Root {
    let count = items.len();
    if count == 0 {
        return node(NODE_EMPTY_LIST, LEVEL_LEAF, &[]);
    }
    if count == 1 {
        return items[0];
    }
    if count == LIST_ARITY {
        return list_branch(items[0], items[1]);
    }
    let mid = count.div_ceil(2);
    let left = list_root(&items[..mid]);
    let right = if mid + 1 == count {
        list_branch(items[mid], items[mid])
    } else {
        list_root(&items[mid..])
    };
    list_branch(left, right)
}

#[inline]
fn list_branch(left: Root, right: Root) -> Root {
    node_parts(NODE_LIST_BRANCH, LEVEL_LEAF, &[&left, &right])
}

// ---------------------------------------------------------------------------
// The epoch's four commitments
// ---------------------------------------------------------------------------

/// One row of the region-slot table:
/// `H(NODE_SLOT_LEAF, 0, LE_u16(slot) ‖ LE_i64(x) ‖ LE_i64(y) ‖ LE_i64(z))`.
#[inline]
pub fn slot_leaf(slot: u16, region: &RegionKey) -> Root {
    let slot_bytes = u16_bytes(slot);
    let x = i64_bytes(region.x);
    let y = i64_bytes(region.y);
    let z = i64_bytes(region.z);
    node_parts(NODE_SLOT_LEAF, LEVEL_LEAF, &[&slot_bytes, &x, &y, &z])
}

/// The slot table's rows, in ascending slot order, into a caller-owned buffer — `None` if the buffer
/// is short. Buffers belong to the epoch store (SPEC-06's slab discipline), not to the hasher: this
/// crate allocates nothing, so a 65,536-region epoch costs the caller one array and the hasher zero
/// churn.
pub fn slot_leaves<'buffer>(
    region_slots: &[RegionKey],
    buffer: &'buffer mut [Root],
) -> Option<&'buffer mut [Root]> {
    if buffer.len() < region_slots.len() {
        return None;
    }
    let mut index = 0;
    while index < region_slots.len() {
        buffer[index] = slot_leaf(index as u16, &region_slots[index]);
        index += 1;
    }
    Some(&mut buffer[..region_slots.len()])
}

/// A region's contribution to the epoch root:
/// `H(NODE_REGION_LEAF, 0, LE_u16(slot) ‖ region_root₃₂)`.
#[inline]
pub fn region_leaf(slot: u16, region: &Root) -> Root {
    let slot_bytes = u16_bytes(slot);
    node_parts(NODE_REGION_LEAF, LEVEL_LEAF, &[&slot_bytes, region])
}

/// An event tag: `H(NODE_EVENT_TAG, 0, LE_u16(tag_id) ‖ opaque bytes)`. What a `tag_id` *is* remains
/// A-05's (OQ-08); this is the shape its entries take.
#[inline]
pub fn event_leaf(tag_id: u16, bytes: &[u8]) -> Root {
    let tag_bytes = u16_bytes(tag_id);
    node_parts(NODE_EVENT_TAG, LEVEL_LEAF, &[&tag_bytes, bytes])
}

/// Base terrain, committed **intensionally**: the rule that generates it, never its 2³⁶ voxels.
///
/// `base_rule_id = 0` means OQ-27 is still open, recorded as a fact rather than a placeholder to
/// forget: the day the rule set is fixed, this value changes and every world root changes with it.
#[inline]
pub fn genesis_root(seed: u64, base_rule_id: u32, rule_version: u32) -> Root {
    let seed_bytes = u64_bytes(seed);
    let rule_bytes = u32_bytes(base_rule_id);
    let version_bytes = u32_bytes(rule_version);
    node_parts(
        NODE_GENESIS,
        LEVEL_LEAF,
        &[&seed_bytes, &rule_bytes, &version_bytes],
    )
}

/// An entity's state digest: the kind A-10 reserves so a stateful entity cannot be mistaken for a
/// field write. Its *contents* wait on OQ-08, so this takes the already-opaque state bytes.
#[inline]
pub fn entity_state_leaf(bytes: &[u8]) -> Root {
    node(NODE_ENTITY_STATE, LEVEL_LEAF, bytes)
}

/// The epoch root — the one thing a peer sends a peer (docs/10 § Verification):
///
/// ```text
/// H(NODE_EPOCH_ROOT, 0,
///   genesis ‖ slot_table ‖ regions ‖ events ‖ LE_u64(epoch_index) ‖ LE_u32(record_count))
/// ```
///
/// `genesis` and `events` are the digests `EpochHeader` already carries as `base_genesis_root` and
/// `event_log_root`, so they enter **once**, from the header: the header fields are the inputs, not a
/// second copy of them. `region_slots` is likewise the source of the caller's `slot_table`. docs/03
/// § Epoch roots now names these two fields instead of "the LE fields of EpochHeader", which was the
/// half-sentence ADR-030 had to settle before any vector could be published.
///
/// A root over no predecessor: chaining is a storage question (OQ-07, OQ-23), not a tree one, and
/// `EpochHeader` has no predecessor field to hash (ADR-030 § Rejected).
pub fn epoch_root(header: &EpochHeader, slot_table: &Root, regions: &Root, events: &Root) -> Root {
    let index_bytes = u64_bytes(header.epoch_index);
    let count_bytes = u32_bytes(header.record_count);
    node_parts(
        NODE_EPOCH_ROOT,
        LEVEL_LEAF,
        &[
            &header.base_genesis_root,
            slot_table,
            regions,
            events,
            &index_bytes,
            &count_bytes,
        ],
    )
}

/// A-10's domain prefix, re-exported so a binding that links only this crate can still build
/// reference bytes.
pub const DOMAIN_PREFIX: &[u8; 8] = DOMAIN;

/// A-10's leaf payload width, for callers that size against it.
pub const LEAF_BYTES: usize = LEAF_PAYLOAD_BYTES;
