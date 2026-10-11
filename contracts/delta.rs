// contracts/delta.rs — artifact A-03, DRAFT 2026-10-11: the delta record layout, the epoch's
// region-slot table, and the epoch header. ADR-029 (docs/decisions.md, locked 2026-10-11) is what
// made this file writable — it fixed the record at `|16 region slot|36 intra-region|12 payload|`,
// exactly one u64, and turned every slab into an append-only record buffer (docs/03).
//
// The split below is deliberate, and it is the split A-09 already uses: everything ADR-029 settled
// is written as code, and everything it left open is written as a `// pending` note instead of
// being invented. Settled here: the field widths and their order in the word, the masked-not-clamped
// rule (SPEC-01), the 65,536-region ceiling and the class it raises, first-touch slot assignment,
// and the logical shape of the epoch header. Not settled here: how the 12-bit payload divides
// (OQ-45), what a slab physically holds (SPEC-06, spike S2), and every byte-level and on-disk
// encoding (OQ-23, OQ-30).
//
// Rules this contract keeps (AGENTS.md §3/§4):
// - **Integer math only.** Nothing here can feed a hash a float, because a record *is* hashed —
//   it is the leaf content of the region roots. Field extraction is shifts and masks, no rounding,
//   no comparison against a transcendental.
// - **No allocation.** A record is a u64 newtype and the slot table borrows the caller's buffer, so
//   no contract type can become the reason a frame touches an allocator.
// - **Core-only, zero dependencies.** Same guard as coords.rs: verified on wasm32-unknown-unknown.
// - **The packed form is the storage form.** docs/03 writes `record.as_u64()` straight into a slab
//   slot, so this type stays a zero-cost wrapper over one word: no padding, no niche, and no field
//   that does not fit in 64 bits. Widening the record is ADR-029's re-open condition, not an edit.
//
// This file is mirrored by docs/03-state-ledger.md § The delta record contract — the
// `delta_contract_matches_spec_document` test in contracts/tests fails if the two ever diverge.

use crate::coords::{GlobalVoxelPos, LocalOffset, RegionKey};
use crate::errors::{LedgerError, ROOT_BYTES};

/// Bits spent on the per-epoch region-slot index.
pub const REGION_SLOT_BITS: u32 = 16;
/// Bits spent inside the region: docs/02's three 12-bit `LocalOffset` components concatenated.
pub const INTRA_REGION_BITS: u32 = 36;
/// Bits per intra-region axis.
pub const LOCAL_AXIS_BITS: u32 = 12;
/// Bits left for the value.
pub const PAYLOAD_BITS: u32 = 12;
/// Width of the whole record.
pub const RECORD_BITS: u32 = 64;
/// Bytes one slab entry occupies — the `*mut u64` docs/03 stores through.
pub const RECORD_BYTES: usize = 8;

/// Distinct regions one epoch's record stream can address. A hard, checked ceiling rather than a
/// budget: past it the stream partitions per region or micro-commits early (docs/03), and the field
/// is never widened.
pub const MAX_REGIONS_PER_EPOCH: usize = 1 << REGION_SLOT_BITS;

// ADR-029's re-open trigger, stated as a compile-time fact instead of a comment: the middle field
// is 36 bits *because* a region is 4096³. If REGION_SHIFT stops being 12, this file stops
// compiling — which is the intended failure, not a surprise.
const _: () = assert!(REGION_SLOT_BITS + INTRA_REGION_BITS + PAYLOAD_BITS == RECORD_BITS);
const _: () = assert!(LOCAL_AXIS_BITS * 3 == INTRA_REGION_BITS);
const _: () = assert!(GlobalVoxelPos::REGION_SHIFT == LOCAL_AXIS_BITS);
const _: () = assert!(RECORD_BITS == core::mem::size_of::<u64>() as u32 * 8);

// Field order is MSB → LSB, reading ADR-029's layout line left to right, and the intra-region
// triple keeps docs/02's own x/y/z order within those 36 bits. Two peers that disagreed about
// which end is the top would hash different roots from identical edits.
const SLOT_SHIFT: u32 = INTRA_REGION_BITS + PAYLOAD_BITS;
const X_SHIFT: u32 = LOCAL_AXIS_BITS * 2 + PAYLOAD_BITS;
const Y_SHIFT: u32 = LOCAL_AXIS_BITS + PAYLOAD_BITS;
const Z_SHIFT: u32 = PAYLOAD_BITS;

const fn mask(bits: u32) -> u64 {
    (1u64 << bits) - 1
}

const SLOT_MASK: u64 = mask(REGION_SLOT_BITS);
const AXIS_MASK: u64 = mask(LOCAL_AXIS_BITS);
const PAYLOAD_MASK: u64 = mask(PAYLOAD_BITS);

/// A region's temporary index inside one epoch. `newtype`d because a slot and a `LocalOffset`
/// component and a payload are all `u16`s, and swapping two of them would still compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionSlot(pub u16);

impl RegionSlot {
    /// The last slot an epoch can hand out — `MAX_REGIONS_PER_EPOCH - 1`.
    pub const MAX: Self = Self(u16::MAX);

    /// The raw index, for a caller that wants to index an array with it.
    #[inline]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// One world change: which region, which voxel inside it, which value. Exactly one `u64`, with no
/// room left over.
///
/// The region key itself is 3×`i64` and is **never** packed (that was SPEC-03), so a record names a
/// region by lookup rather than by address — which is why `EpochHeader::resolve` is part of reading
/// one, and why a raw `RegionKey` never crosses the network (docs/10, ADR-021).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeltaRecord(u64);

impl DeltaRecord {
    /// Pack a record. Every field is **masked, never clamped**: a value wider than its field rolls
    /// the same way SPEC-01 makes `LocalOffset` roll, so the P1.1 round trip stays exact instead of
    /// saturating into a position that was never requested.
    #[inline]
    pub const fn new(slot: RegionSlot, offset: LocalOffset, payload: u16) -> Self {
        let bits = ((slot.0 as u64) << SLOT_SHIFT)
            | (((offset.x as u64) & AXIS_MASK) << X_SHIFT)
            | (((offset.y as u64) & AXIS_MASK) << Y_SHIFT)
            | (((offset.z as u64) & AXIS_MASK) << Z_SHIFT)
            | ((payload as u64) & PAYLOAD_MASK);
        Self(bits)
    }

    /// Decode the raw word a slab holds. Total by construction: the three fields consume all 64
    /// bits and each is read by masking, so a slab read cannot fail to *decode* — it can only
    /// resolve to a slot this epoch's header does not know, which is a different error.
    #[inline]
    pub const fn from_u64(bits: u64) -> Self {
        Self(bits)
    }

    /// The word to store. This is what docs/03's `write_unaligned` takes.
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    #[inline]
    pub const fn slot(self) -> RegionSlot {
        RegionSlot(((self.0 >> SLOT_SHIFT) & SLOT_MASK) as u16)
    }

    /// The voxel inside the region — 3×12 bits, in docs/02's axis order. Not a slab offset:
    /// ADR-029 dropped the slab from the address, and the field that used to claim otherwise was
    /// SPEC-09.
    #[inline]
    pub const fn intra_region(self) -> LocalOffset {
        LocalOffset {
            x: ((self.0 >> X_SHIFT) & AXIS_MASK) as u16,
            y: ((self.0 >> Y_SHIFT) & AXIS_MASK) as u16,
            z: ((self.0 >> Z_SHIFT) & AXIS_MASK) as u16,
        }
    }

    /// The value half of the record.
    // pending OQ-45: docs/03 has not decided how these 12 bits divide between material id, flags
    // and quantity, so this is an opaque 12-bit field rather than a `MaterialId`. The day it
    // becomes one is an AGENTS.md §2 contract change: the code moves, the widths do not.
    #[inline]
    pub const fn payload(self) -> u16 {
        (self.0 & PAYLOAD_MASK) as u16
    }

    /// Record for a global position in a region the epoch already has a slot for — the encoder the
    /// ledger calls when a player breaks a block. The 12-bit range comes from A-01's own masking,
    /// so this cannot construct a record that does not round trip.
    #[inline]
    pub fn at_position(slot: RegionSlot, pos: &GlobalVoxelPos, payload: u16) -> Self {
        Self::new(slot, pos.local_offset(), payload)
    }
}

/// The global voxel a record names, with the slot resolved. This is A-01's `region_key` /
/// `local_offset` split run backwards, and it is exact for every position those two produced:
/// the shift loses nothing, so P1.1 covers the whole `i64` range including negatives.
///
/// `wrapping_*` rather than `*`/`+` for the same reason A-01 uses `wrapping_sub`: a hand-typed
/// region key outside the representable span must not have its result depend on whether this was a
/// debug or a release build.
#[inline]
pub const fn voxel_at(region: &RegionKey, offset: &LocalOffset) -> GlobalVoxelPos {
    let scale = 1i64 << LOCAL_AXIS_BITS;
    GlobalVoxelPos {
        x: region.x.wrapping_mul(scale).wrapping_add(offset.x as i64),
        y: region.y.wrapping_mul(scale).wrapping_add(offset.y as i64),
        z: region.z.wrapping_mul(scale).wrapping_add(offset.z as i64),
    }
}

/// The epoch's `slot index → RegionKey` table, owned by the epoch header (docs/02) and rebuilt in
/// first-touch order at replay. The assignment *rule* lives in a contract rather than in
/// `crates/ledger` because the slot index is inside a hashed record: two builds that handed out
/// different slots for the same edit sequence would produce different roots from identical worlds.
///
/// The scan below is reference semantics, not the hot-path shape — a ledger implementation may
/// index this however it likes, as long as `assign` hands out the same slots in the same order.
/// Resolving a `RegionKey` still never hashes a string (AGENTS.md §3).
#[derive(Debug)]
pub struct RegionSlotTable<'a> {
    keys: &'a mut [RegionKey],
    count: usize,
}

impl<'a> RegionSlotTable<'a> {
    /// Wrap a caller-owned buffer. Capacity is a budget the caller decides (docs/03's slab
    /// budgets); the table never grows past it quietly.
    pub fn new(keys: &'a mut [RegionKey]) -> Self {
        Self { keys, count: 0 }
    }

    /// Slots handed out so far.
    #[inline]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Buffer size, which may exceed the field ceiling — that only changes when the table reports
    /// full, never what a record is able to address.
    #[inline]
    pub const fn capacity(&self) -> usize {
        self.keys.len()
    }

    /// The lesser of the 16-bit ceiling and the buffer: the real capacity of this table.
    const fn limit(&self) -> usize {
        if MAX_REGIONS_PER_EPOCH < self.keys.len() {
            MAX_REGIONS_PER_EPOCH
        } else {
            self.keys.len()
        }
    }

    /// True when a new region could not be given a slot.
    #[inline]
    pub const fn is_full(&self) -> bool {
        self.count >= self.limit()
    }

    /// Slot for `key`, appending it at the next free index the first time the epoch touches that
    /// region. The error is the checked limit docs/03 promises: the caller reacts by partitioning
    /// the stream or committing early, and never by widening the field.
    pub fn assign(&mut self, key: RegionKey) -> Result<RegionSlot, LedgerError> {
        if let Some(existing) = self.find(key) {
            return Ok(existing);
        }
        if self.is_full() {
            return Err(LedgerError::EpochRegionSlotsExhausted);
        }
        let slot = self.count;
        self.keys[slot] = key;
        self.count += 1;
        Ok(RegionSlot(slot as u16))
    }

    /// The region a slot stands for, or `None` when it is past what this table has handed out.
    pub fn resolve(&self, slot: RegionSlot) -> Option<RegionKey> {
        let index = slot.0 as usize;
        if index < self.count {
            Some(self.keys[index])
        } else {
            None
        }
    }

    /// Existing slot for a region, without claiming one.
    pub fn find(&self, key: RegionKey) -> Option<RegionSlot> {
        let mut index = 0usize;
        while index < self.count {
            if self.keys[index] == key {
                return Some(RegionSlot(index as u16));
            }
            index += 1;
        }
        None
    }

    /// Read-only view, for handing to the epoch header this table ends up in.
    pub fn slots(&self) -> &[RegionKey] {
        &self.keys[..self.count]
    }
}

/// What one epoch commits, in the shape docs/03's commit pipeline produces: the roots it re-anchors
/// on, the slot table its records were addressed through, and how many records it holds.
///
/// This is the **logical** header. Its byte layout is deliberately not decided here: that is OQ-23
/// (what physically persists) and OQ-30 (`.fbs` versioning), and the on-disk form has to be the
/// same FlatBuffers schema the network uses (ADR-015). Nothing below should be read as an offset
/// table, and no field order here is a claim about byte order there.
#[derive(Debug, Clone, Copy)]
pub struct EpochHeader<'a> {
    /// Ordinal of this epoch. Deliberately not a `TickId`: an epoch and a tick are different
    /// clocks, and OQ-42 has not fixed what either of them counts. docs/03 refuses to bake that
    /// ambiguity into a timer; this file refuses to bake it into a type name.
    pub epoch_index: u64,
    /// `BaseGenesisRoot` that the epoch's committed slabs compose with (docs/10 § Verification).
    pub base_genesis_root: [u8; ROOT_BYTES],
    /// Root over the epoch's event log — the second half of
    /// `Blake3(region_roots ⊕ event_log)` (docs/03 § Compaction). The log's own schema is OQ-08's
    /// and A-05's, so this stays an opaque digest rather than a count or an offset.
    pub event_log_root: [u8; ROOT_BYTES],
    /// The region-slot table, slot index = array index. Borrowed, because a header is a view onto
    /// an epoch's records rather than the owner of them.
    pub region_slots: &'a [RegionKey],
    /// High-water mark of records in the epoch stream — the bump pointer docs/03 resets to 0 at
    /// compaction.
    // pending SPEC-06 / spike S2: how many records a region can hold outstanding is unmeasured.
    // `u32` is a draft width, chosen because 4×10⁹ records is already far past any budget anyone
    // has proposed, not because anything has concluded it.
    pub record_count: u32,
}

impl<'a> EpochHeader<'a> {
    /// Regions this epoch addresses.
    #[inline]
    pub const fn slot_count(&self) -> usize {
        self.region_slots.len()
    }

    /// Resolve a slot against this epoch's table. `None` is not a corrupt record: it means the
    /// record arrived with a slot this epoch never handed out, which is the bisection repair's
    /// problem (docs/10), not the codec's.
    pub fn resolve(&self, slot: RegionSlot) -> Option<RegionKey> {
        self.region_slots.get(slot.0 as usize).copied()
    }

    /// The world position a record names under this epoch, when its slot is known.
    pub fn voxel_of(&self, record: DeltaRecord) -> Option<GlobalVoxelPos> {
        let region = self.resolve(record.slot())?;
        Some(voxel_at(&region, &record.intra_region()))
    }
}
