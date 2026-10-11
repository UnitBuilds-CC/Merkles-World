// contracts/tree.rs — artifact A-10, DRAFT 2026-10-11: the Merkle tree **shape**.
//
// ADR-028 (docs/decisions.md, locked 2026-10-11) chose the digest — BLAKE3-256, 32 bytes — and said
// plainly that it did *not* choose the shape. Arity, child order, the rule for an absent subtree and
// the byte order of every integer in a preimage appear nowhere else in this repo, and an epoch root
// is not canonical until they do: a peer that orders children differently computes a different root
// while believing it is consistent. ADR-030 records the choices made here.
//
// What this file is and is not. It is the shared, hashable *grammar* — the thing the ledger, the
// network (docs/10) and a future WASM binding must agree on word for word. It is not the hasher:
// `crates/ledger` owns the implementation, because hashing is behaviour and contracts hold none.
//
// Rules this contract keeps (AGENTS.md §3/§4):
// - **Integer math only.** Every preimage is built from shifts, masks and fixed-width little-endian
//   bytes. There is no float here and no way to feed one in: the encoders below take `u64`/`u32`/`u16`
//   and nothing else. This is the exposure [OQ-44](docs/13-risks-open-questions.md) asks about —
//   the type signature is the current guard, and it is a weak one that a lint should replace.
// - **No voxel-level Morton code.** docs/02 forbids one, so the octree path is derived *per level*
//   from `LocalOffset`'s own axis bits (`octant_at`), never by interleaving 36 bits into a flat
//   voxel index. Sorting uses `(x, y, z, append)` lexicographically for the same reason.
// - **Core-only, zero dependencies.** Same guard as coords.rs and delta.rs: verified on
//   wasm32-unknown-unknown in CI.
//
// Not settled here, and deliberately absent: what the 12-bit payload *means* (OQ-45 — the leaf
// hashes it opaquely, so the shape does not wait on that question), the event-tag registry (A-05,
// OQ-08 — kind 4 is a slot in the grammar, not a schema), the on-disk and wire encodings (OQ-23,
// OQ-30), and whether epoch roots are ever *chained* (see `NODE_EPOCH_ROOT`: this root commits to
// one epoch's contents and to no predecessor).
//
// This file is mirrored by docs/03-state-ledger.md § The tree shape — the
// `tree_contract_matches_spec_document` test in contracts/tests fails if the two ever diverge.

use crate::delta::{INTRA_REGION_BITS, LOCAL_AXIS_BITS, PAYLOAD_BITS, REGION_SLOT_BITS};
use crate::errors::ROOT_BYTES;

/// Prefix of every preimage, so a digest from this grammar can never be re-read as a digest of
/// raw data by anything else that happens to use BLAKE3. Changing it is a shape break: every world
/// root changes, so it moves only with an ADR and a migration note.
pub const DOMAIN: &[u8; 8] = b"mlworld1";

// ---------------------------------------------------------------------------
// Node kinds — the second byte of every preimage. One per node type, no reuse.
// ---------------------------------------------------------------------------

/// An edited voxel: `level = 0`, payload = the record's 12 bits in `LEAF_PAYLOAD_BYTES`. Position
/// comes from the path, not from the payload, so a leaf says nothing about where it is.
pub const NODE_VOXEL_LEAF: u8 = 0;
/// An octree internal node at `level` 1..=12: eight child digests in octant order.
pub const NODE_OCTANT_BRANCH: u8 = 1;
/// The octree's absent leaf at `level = 0`. Absent subtrees above it are **not** special-cased:
/// `empty(l) = branch(l, [empty(l-1); 8])`, one rule, derived identically by every implementation.
pub const NODE_EMPTY_OCTREE: u8 = 2;
/// An empty ordered list. Separate from kind 2 on purpose: an epoch with no edits and a region with
/// no edits must not share a digest while meaning two different things.
pub const NODE_EMPTY_LIST: u8 = 3;
/// An event tag (docs/05's macro-causal fact): `tag_id` then opaque bytes. The registry is A-05's.
pub const NODE_EVENT_TAG: u8 = 4;
/// An entity's state digest (docs/06). Reserved so a stateful entity cannot be mistaken for a
/// field write; its content waits on OQ-08.
pub const NODE_ENTITY_STATE: u8 = 5;
/// A region's contribution to an epoch: its 16-bit slot, then its 32-byte region root.
pub const NODE_REGION_LEAF: u8 = 6;
/// One row of the epoch's region-slot table: its 16-bit slot, then the three `i64` region
/// coordinates. Without it, two peers that assigned slots differently could agree on a root.
pub const NODE_SLOT_LEAF: u8 = 7;
/// A binary node over an ordered list: left digest then right digest.
pub const NODE_LIST_BRANCH: u8 = 8;
/// The epoch root itself: the whole commitment one peer sends another (docs/10 § Verification).
pub const NODE_EPOCH_ROOT: u8 = 9;
/// Base terrain, committed **intensionally**: seed, then base rule id, then rule version. A region
/// holds 2³⁶ voxels, so the base field is named by the rule that generates it rather than hashed
/// voxel by voxel — see docs/03 § The tree shape.
pub const NODE_GENESIS: u8 = 10;

// ---------------------------------------------------------------------------
// Shape constants
// ---------------------------------------------------------------------------

/// Children of an octree node. Three address bits per level, x lowest.
pub const OCTREE_ARITY: usize = 1 << 3;
/// Octree depth over one region. Equal to `LOCAL_AXIS_BITS` because one level consumes one bit of
/// each axis, and the 36-bit intra-region address is exactly 12 levels × 3 bits.
pub const OCTREE_DEPTH: u32 = LOCAL_AXIS_BITS;
/// Children of a list branch. Two, so a disagreement bisects (gate P1.6) instead of rescanning.
pub const LIST_ARITY: usize = 2;
/// Bytes of preimage for one leaf of the octree: the record's 12 payload bits, masked, held in a
/// `u16`. Opaque — OQ-45 decides what they mean, not how many bits they occupy.
pub const LEAF_PAYLOAD_BYTES: usize = 2;
/// The `level` byte an octree leaf carries, and the value every kind without a level must pass.
pub const LEVEL_LEAF: u8 = 0;

/// Octree depth derived from the region edge length instead of restated: 4096 = 2¹² levels.
const _: () = assert!(OCTREE_DEPTH * 3 == INTRA_REGION_BITS);
/// Eight children because three axes, one bit each.
const _: () = assert!(OCTREE_ARITY == 1 << 3 && LIST_ARITY == 2);
/// A 12-bit payload still fits the leaf's two bytes.
const _: () = assert!(LEAF_PAYLOAD_BYTES * 8 >= PAYLOAD_BITS as usize);
/// A slot still fits its leaf's two bytes.
const _: () = assert!(REGION_SLOT_BITS <= 16);
/// ADR-028's digest width, restated as a constraint on this grammar rather than a comment.
const _: () = assert!(ROOT_BYTES == 32);

// ---------------------------------------------------------------------------
// Canonical integer encoding: fixed width, little-endian, everywhere.
// ---------------------------------------------------------------------------

/// A `u64` in a preimage is 8 bytes, least significant first. Pinned once so no implementation
/// can "read better as big-endian" and quietly split the network in two.
#[inline]
pub const fn i64_bytes(v: i64) -> [u8; 8] {
    v.to_le_bytes()
}

/// See `i64_bytes`.
#[inline]
pub const fn u64_bytes(v: u64) -> [u8; 8] {
    v.to_le_bytes()
}

/// See `i64_bytes`.
#[inline]
pub const fn u32_bytes(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// See `i64_bytes`. A record's payload and an epoch's slot both go through this.
#[inline]
pub const fn u16_bytes(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

// ---------------------------------------------------------------------------
// Child order
// ---------------------------------------------------------------------------

/// Which octant of a level-`L` node the voxel at `(x, y, z)` falls in, `L` in `1..=OCTREE_DEPTH`.
///
/// `level` is a branch's **height above the voxel leaves**, the same count every Merkle
/// implementation uses: level 1 is the branch whose eight children are voxels, and level
/// `OCTREE_DEPTH` is the region root. A branch at level `L` consumes bit `L - 1` of each axis.
/// The octant index is `x_bit | y_bit << 1 | z_bit << 2`, i.e. **x is the low bit**, so children
/// are hashed in the order `(0,0,0) (1,0,0) (0,1,0) (1,1,0) (0,0,1) (1,0,1) (0,1,1) (1,1,1)` with
/// each coordinate's bit for that level.
///
/// This derives a path from `LocalOffset`'s own bits at every call. It is not a Morton code: no
/// 36-bit flat voxel index is ever formed, which is the thing docs/02 § Octree keys and Morton codes
/// forbids.
#[inline]
pub const fn octant_at(x: u16, y: u16, z: u16, level: u32) -> usize {
    // Levels outside 1..=12 have no octant; callers get 0 rather than a shift that would wrap.
    if level == 0 || level > OCTREE_DEPTH {
        return 0;
    }
    let shift = level - 1;
    let xb = ((x >> shift) & 1) as usize;
    let yb = ((y >> shift) & 1) as usize;
    let zb = ((z >> shift) & 1) as usize;
    xb | (yb << 1) | (zb << 2)
}

/// The preimage of one node, before hashing: `DOMAIN`, the kind byte, the level byte, the payload.
/// Exposed so a WASM or Rust binding builds the *same* bytes rather than trusting this crate's
/// hasher. Returns the length it wrote; `buffer` must be `DOMAIN.len() + 2 + payload.len()`.
///
/// A short buffer is a programmer error, not a runtime state, so this reports `None` instead of
/// panicking — a hash that silently truncated would be worse than no hash.
#[inline]
pub fn preimage<'buffer>(
    kind: u8,
    level: u8,
    payload: &[u8],
    buffer: &'buffer mut [u8],
) -> Option<&'buffer [u8]> {
    let need = DOMAIN.len() + 2 + payload.len();
    if buffer.len() < need {
        return None;
    }
    let mut i = 0;
    while i < DOMAIN.len() {
        buffer[i] = DOMAIN[i];
        i += 1;
    }
    buffer[i] = kind;
    buffer[i + 1] = level;
    i += 2;
    let mut j = 0;
    while j < payload.len() {
        buffer[i + j] = payload[j];
        j += 1;
    }
    Some(&buffer[..need])
}
