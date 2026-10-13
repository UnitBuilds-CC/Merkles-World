# 03 — State, ledger and memory

**Status:** specified, and the delta record is now decidable — [ADR-029](decisions.md) (locked
2026-10-11) settles the record layout, and **SPEC-01/03/09** closed with A-01's freeze. A-03 is
**drafted** as `contracts/delta.rs` against that layout (2026-10-11), and gate P1.1 runs as
`p1_1_delta_record_round_trip`. Implementation still blocked on SPEC-06 (slab geometry, and how a
region's record buffer reads back), SPEC-08, OQ-01, OQ-23, and the payload field's internals (OQ-45).
**Depends on:** [doc 02](02-coordinate-system.md). **Consumed by:** docs 04, 05, 06, 10.

```
                    ┌──────────────────────────────────────────┐
   authoritative →  │  MERKLE LEDGER (epochs, roots, event log) │  slow, cheap, hashable
                    └───────────────────▲──────────────────────┘
                                        │ commit (epoch / sleep policy)
                    ┌───────────────────┴──────────────────────┐
     transient  →   │  SLABS  (M_active hot, M_cold write-ahead)│  fast, O(1), disposable
                    └───────────────────▲──────────────────────┘
                                        │ apply (frame 0)
                    ┌───────────────────┴──────────────────────┐
   presentation →   │  GPU render projection (doc 04)           │  read-only, never authoritative
                    └──────────────────────────────────────────┘
```

## Terminology

| Term | Meaning |
|------|---------|
| **epoch** | A committed world state, identified by a root hash. A version of the world. |
| **root / epoch root** | BLAKE3-256 digest over one epoch's region roots + event log. |
| **mempool** | Committed-but-uncompacted transactions, ordered by Lamport clock. |
| **event log** | Append-only named macro events, written at sector granularity: `{SectorKey, Kind, Magnitude, TickId}` (schema pending [OQ-08](13-risks-open-questions.md) / A-05). The **canonical form** of macro causality ([OQ-01](13-risks-open-questions.md)). |
| **`M_active`** | Slabs for regions in the interaction radius. Directly writable, bound to the GPU. |
| **`M_cold`** | Write-ahead delta log for everything outside that radius. Never bound to the GPU. |
| **compaction** | Bake outstanding deltas into a new epoch root, then reset the bump pointer. |

### The three things people keep confusing (say these out loud in review)

1. **The ledger does not store the world.** It stores roots, mempool transactions, the event
   log, and region epochs — small, hashable, replayable.
2. **`M_cold` is not the delta mask of the whole world.** It is a write-ahead log of
   player-authored differences only, and it is pruned at compaction. Terrain and macro state are
   recomputed, never stored. This is what keeps memory scaling with *edits per epoch* rather
   than with world size or elapsed time — the property the whole design exists for.
3. **The ledger is a bounded window.** Epochs older than the configured window (default 64) are
   pruned with a signed **state snapshot** as the anchor. Replay is `O(k)` from a snapshot, not
   `O(N)` from genesis. Full history is a stretch goal for a later release (a replay/evidence
   feature, not a correctness requirement).

## Identity (from doc 02)

- A delta targets **one slab per region**: a 256² × 64 plane tile = 2²² voxels, the edit envelope
  around the player, *not* a 4096² column and *not* a region-sized volume ([SPEC-06](13-risks-open-questions.md)).
- Addressing is `GlobalVoxelPos → (RegionKey, [u16; 3])`; `LocalOffset` is 3×`u16` masked to the
  12-bit range (SPEC-01, pinned by A-01's freeze).
- The packed delta is 8 bytes: 16 bits region-slot, **36 bits intra-region** (doc 02's three
  12-bit `LocalOffset` components), 12 bits payload
  ([SPEC-03](13-risks-open-questions.md), locked by [ADR-029](decisions.md) on 2026-10-11; the type
  is `contracts::delta::DeltaRecord`, A-03).
  16 bits indexes a set of **≤65536 distinct regions per epoch**, which is a hard, checked
  limit — exceeding it before compaction triggers an early micro-commit rather than corruption.
- **The middle field was pointed at the wrong thing (SPEC-09, now locked).** The split first
  written here spent 16 bits on an *intra-slab* offset: 2¹⁶ addresses inside a slab of 2²² voxels —
  neither a voxel index nor a usable record index, and it made one decision out of three different
  numbers (the 4 MiB tile, the 512 KiB cap, the 64 KB / 1 MB budgets). ADR-029 resolves it by
  dropping the slab from the address entirely and spending the 36 bits inside the region, which is
  exact: 3×12 = `log2(4096³)`.
- **What it deliberately leaves open:** how the 12-bit payload divides between material, flags and
  quantity — registered as [OQ-45](13-risks-open-questions.md), because a width nobody has measured
  should be a question with an id, not a paragraph; [SPEC-06](13-risks-open-questions.md)'s slab
  geometry, now an allocation/streaming choice rather than an addressing constraint; and how a
  region's append-only record buffer is read back — if it is replayed into a dense overlay on load
  the write below stays a pointer-offset store, if it is indexed it is not.
  Spike **S2** measures the sizes.
  **What is closed:** the addressing conflict, and with it the record itself — A-03 is drafted as
  `contracts/delta.rs`, A-01 froze alongside it, and gate P1.1 runs against the code.

## The delta record contract

Artifact **A-03**. What ADR-029 settled is code; what it left open is a `// pending` note on the
field it belongs to, so the file cannot be read as if it had decided more than it did. The block
below is `contracts/delta.rs` verbatim, and `delta_contract_matches_spec_document` in
`contracts/tests/delta_record.rs` fails if the two ever diverge — the same rule docs/02 § The Rust
contract sets for A-01, and the reason this doc carries code at all.

```rust
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
```

## Slab allocation

Pre-allocate contiguous fixed-size slabs keyed by region index. Under ADR-029 a slab is an
**append-only record buffer**, so its entry count is bounded by the buffer's size rather than by a
field width: 1 MB of 8-byte records holds 131,072 edits for one region, 64 KB holds 8,192. Whether
either is the right number is still **spike S2's** to measure against real per-epoch edit volume —
two sizes are written here (**64 KB / 1 MB**) precisely because nobody has measured which survives.
What is no longer open is the addressing: S2 chooses a capacity, it does not have to fit an index.

```rust
let slot = slab.base.add(offset * 8);          // pointer arithmetic, no allocator
core::ptr::write_unaligned(slot as *mut u64, packed_delta);
```

**How it is actually implemented, and where that differs from the snippet above.** `crates/memory`
borrows a `&mut [u64]` and indexes it; it does not use `core::ptr::write_unaligned`, because the
workspace lints forbid `unsafe` in every crate (`Cargo.toml` `[workspace.lints.rust]`, and CI runs
`clippy -D warnings`). Both halves want the same guarantee — one store, no allocator — and the safe
form over a pre-allocated slice gives it, so the divergence is a spelling change rather than a weaker
design. If a future measurement shows the safe form costing that guarantee, that belongs here as
evidence and an ADR, not as a quietly added `unsafe` block.

Measured on x86-64 Windows, release profile, `cargo run --release --example slab_throughput -p memory`
(spike S2, part one):

| What | Number |
|------|--------|
| Sustained appends | **57.2 M writes/sec** over 10⁶ appends into the 1 MB budget, 7 bump-pointer resets inside the timed loop |
| Per-append latency, clock overhead included | p50 100 ns, p99 200 ns, p99.9 200 ns, max 124.7 µs over 200,000 individually timestamped appends |
| Same run against the 64 KB budget | 1.006 ms for 10⁶ appends (994 M/sec, 122 resets) — the smaller buffer stays cache-resident, which is the only difference S2 saw between the two candidate sizes |
| Gate P1.2 (10⁶/sec sustained, `p99 < 1 µs`) | **met on this machine** — one machine, not the four gate P1.5 names, and the Windows clock's ~100 ns granularity floors the percentile resolution |

- **`O(1)` write** (a store, nanoseconds), no `malloc`, no GC, no OS memory-lock contention.
- The word stored is `DeltaRecord::as_u64()` (A-03, above), so the append and the addressing cannot
  drift apart — a slab is 8-byte-aligned records of one type, and nothing else writes into it.
- Contiguous, so dirty slabs go to the GPU in one `memcpy`/DMA and hash in one SIMD pass.
- **`O(1)` mass reset:** at commit, the bump pointer returns to 0 and the slab is recycled —
   whether 10 or 10,000,000 blocks changed. Freeing individual records is the thing we must
   never have to do.
- Slab size and count are **budgets, not guesses** — spike S2 measures them.

## Tiering and eviction

| Tier | Lives in | Policy |
|------|----------|--------|
| `M_active` | VRAM-pinned + RAM mirror | regions inside the interaction radius; overwritten wholesale when the camera anchor crosses a region boundary |
| `M_cold` | CPU RAM, append-only | deltas for regions outside the radius (write-ahead) |
| disk | epoch snapshots | background micro-checkpoints when `M_cold` fills — **without** forcing a gameplay sleep |

LRU eviction moves regions out of `M_active` as the player travels. Re-entering a modified region
**always** restores its edits — `M_cold` plus committed epochs make it reconstructible — but
restoration must not stutter, which is the pop-in acceptance test in gate P1.4.

**The bookkeeping exists (2026-10-12):** `crates/ledger::tier` is the directory that half of that
paragraph assumed. It is `no_std` and allocation-free like the rest of the crate — a caller-supplied
`[Slot]` slice plus a `hot_capacity`, no clock of its own (`store.tick()` is driven by the caller,
because `TickId` is a consensus-relevant quantity and this module must not invent one). `admit` returns
`Admission::{AlreadyHot, Restored, Evicted { evicted }, NoRoom}`, and the distinction it exists to
make is the one the table above implies: **cold is reachability, not loss.** `edits_of` answers for a
demoted region exactly as it does for a hot one, and the only case that is genuinely a failed restore
is a region the directory never knew — which `re_enter` reports as `None` rather than as an empty
range, because "nobody edited here" and "I could not find it" must not look the same to a player.
`tests/residency.rs` pins all of it by **root equality**: the property tested is not that the same
records come back in some order but that the region re-roots to the digest it had before eviction,
since that digest is what a peer compares and what a save replays against. Eviction ties break to the
lower cell index, so a replayed walk evicts the same regions twice. `crates/engine`'s boot performs
the round trip too — demote, re-enter, re-root — and prints the restored digest, which
`boot_is_reproducible.rs` compares against `REGION_0` from outside the process, so the directory has
a caller and the claim has a check that does not trust the binary's own asserts.

**And the cost half says something the policy above does not (2026-10-12).** `cargo run --release
--example re_entry_cost -p ledger` prices re-entry as the operation the paragraph describes: for a
full 64 KB slab's worth of scattered edits it computes **68,524 digests** — spike S2's own count for
the same edit total, through the same counted entry point — at the same digest rate S2 measures *on
that host*, so the directory lookup and the arena copy are noise and re-entry is *the hashing pass
moved to a different moment*: triggered by the player walking back instead of by the epoch closing.
CI now runs the step on all three architectures, and the shape of the answer is the point
(`38279591441`): the digest count is identical everywhere — 68,524, 69% of the `12N` bound — while
the price per digest is the runner's own (757 ns on ARM64 macOS, 486 on x86-64 Windows, 302 on
x86-64 Linux), so the same restore costs 51.9 ms, 33.3 ms and 20.7 ms. **On all three, one region's
re-entry is at least one 16.7 ms frame.**

That reframes P1.4. A camera does not re-enter one region, it re-enters the ring around it, so a
recompute-on-re-entry costs that number times the interaction radius — and no choice of radius fixes
a per-region cost that already exceeds the frame. Two consequences, neither of which this section
will decide silently:

- The cheap restore is the one that **does not re-root**: if `M_cold` stored each region's root
  alongside its records, re-entry would be a copy and a lookup, and the pop-in test would be about
  bandwidth instead of hash rate. The table above does not say whether cold rows carry roots, and
  storing them is redundant state a compaction can invalidate — so it needs a sentence and an
  invalidation rule, not an assumption. This is [OQ-49](13-risks-open-questions.md)'s "which lever
  moves" question reached from the read side rather than the commit side.
- Splitting a large restore into several small ones **does not help**: on one host the measured
  `ns per digest` is flat across region sizes (8 to 8,192 edits), while `ns per edit` only falls because a wider tree
  shares upper paths — 11.4 digests per edit at the top of the table against 8.4 at the bottom, i.e.
  94% then 69% of the `12N` bound. Per-edit time is a function of *arrangement*, exactly as S2
  concluded, and a scheduler that batches re-entries by region will not find work to shave.

The pop-in half of P1.4 stays open on one clause only: "visible" is a frame-time word, and there
is still no renderer to be visible on (doc 11's Phase 1 exit table). The half that was open only
because nobody had measured it is closed: `crates/render/examples/region_pop_in.rs` attributes
pixels to the arriving region (composite with its cell and without, difference the depths) and
counts frames until they land. At a ceiling of one ray charge, a re-entered region owning 70 of
3,072 pixels contributes none of them to frame one in either walk order, reaches half at frame
41 centre-out against 75 tile-major, and completes at 76 against 110. Pop-in is a number to
argue about now; only the display is still missing.

## Compaction (the sleep path)

```
S_new_genesis = S_old_genesis ⊕ M_local          # bake
root_new      = Blake3( region_roots' ⊕ event_log' )
M_local       = ∅                                 # bump pointer → 0
```

Sleep is a *meaningful* pause, which is why it was chosen as the commit boundary: deterministic
catch-up (apply the pool for Δt), batch compression (thousands of micro-edits → one root), and
far consequences (dried lake bed, dead vegetation, migrated fauna) already encoded in the new
root when the player arrives. It is an optimization of the same primitive as an
**auto-commit epoch timer** (unowned proposal — a wall-clock fallback, or a pending-pool size
threshold, neither decided, [OQ-24](13-risks-open-questions.md)) — needed because some players won't
sleep, and because multiplayer has no single sleeper
([SPEC-08](13-risks-open-questions.md)). Do not confuse that policy question with the
**capacity micro-commit**, which is not a policy: exceeding an epoch's 65,536 region slots forces a
commit because the 16-bit field cannot name another region ([ADR-029](decisions.md), checked in
`RegionSlotTable::assign`). The source's cadence was "every 1,000 ticks or when a
player sleeps" (GS 1898); neither 1,000 ticks nor any wall-clock equivalent is adopted here,
because Δt has no declared unit yet ([OQ-42](13-risks-open-questions.md)) — fixing this timer
*without* fixing OQ-42 would bake in an ambiguity. Compaction is a policy callback, never a call site
inside the sleep action (ADR-004).

**Cost (accepted):** a player who logs off without sleeping can see a stale world until the next
commit. Mitigations, none decided ([OQ-24](13-risks-open-questions.md)): commit-on-logout, a
background commit when the pending pool crosses a size threshold, or per-interaction-commit for
nearby regions with global consequences deferred.

### What a compaction may and may not do (`crates::ledger::compact`)

The bake above is a policy; the *transform* — take a region's outstanding records, hand back fewer
of them — is now an implementation, and it comes with one property and one measured surprise.

**The property:** a compaction re-encodes state, it never changes it, which here means the
**region root is bit-identical before and after**. That is a strong claim and it rests on a detail
of the frozen A-10 shape: a voxel leaf hashes only its payload, and duplicates resolve at the leaf
by greatest append rank, so a region root depends on exactly one thing per voxel — *which payload
won* — and not on list length, arrival order, or how many shadowed writes sit beside the winner.
Compaction is therefore root-preserving precisely when its survivor per voxel is the record that
would have won, and nothing else about ordering matters. `tests/compaction.rs` asserts the digest
equality rather than arguing it, checks idempotence the way A-09's `CompactionNotIdempotent` asks
for (a second pass removes nothing, and the survivor *set* is a set — a right-length answer with a
duplicate and an omission would slip past the digest check alone), and pins that the winner follows
arrival order under permuted input, because asserting a fixed payload would be asserting that
compaction chooses by value.

**Why a rank is passed in.** "The record that would have won" is causal order, and the ledger does
not own causal order — commit-pipeline step 2 is [OQ-46](13-risks-open-questions.md), and with one
writer it is append order. There is a mechanical reason too: with no allocator, the only in-place
sort available is unstable, so ties are not preserved by construction. Making the caller supply the
same `u32` the commit path stamps as `append` turns `(voxel, rank)` into a tie-free total order, so
the result is deterministic *because* it is ordered, not because a sort happened to behave.
`ledger::compact::assign_ranks` builds those ranks from a plain record slice for callers who are using
append order, so the common path needs none of this reasoning.

**The measured surprise, and its correction:** compaction does **not** make the *hashing* cheaper. On
the fixture the tests use — 72 records over 36 distinct voxels, i.e. every cell written twice — the
compacted set is exactly half the records and the same **79 digests**, root to root:

```
records 72 -> live 36; digests 79 -> 79; roots equal
```

Duplicates already collapse at the leaf for free: one cell, one leaf hash, the winner picked by
comparison. That is the same discovery spike S2 made when it deleted the 46 ms `O(N²)` resolution
pre-pass and moved last-write-wins into the walk. The first reading of that result — "so compaction is
not a lever on the Merkle pass" — was too strong, and `cargo run --release --example compaction_ratio
-p ledger` is what caught it. 2,048 distinct voxels written 1, 2, 4 and 16 times each, in both of S2's
arrangements:

| arrangement | writes per voxel | records → live | digests (before = after) | pass time before → after |
|---|---|---|---|---|
| scattered | 1 | 2,048 → 2,048 | 18,489 | 13.9 ms → 15.5 ms (noise) |
| scattered | 4 | 8,192 → 2,048 | 18,489 | 15.1 ms → 13.2 ms |
| scattered | 16 | 32,768 → 2,048 | 18,489 | 20.7 ms → 13.2 ms (≈1.6×) |
| clustered | 16 | 32,768 → 2,048 | 4,096 | 5.0 ms → 1.8 ms (≈2.7×) |

The digest count is *identical in every row* — hashing is priced by **distinct voxels**, never by
records, which is the sense in which `12N` was always a bound on leaves — while the wall time is not,
because the walk partitions its leaf slice at each of the twelve levels and partitioning is per entry.
So compaction removes the record-count term and leaves the distinct-voxel floor: ≈1.6× on a heavily
duplicated scattered region, ≈2.7× clustered, and nothing at all at one or two writes per voxel.
Timings are this host's own (they drift more than the ratios suggest between runs); the digest counts
are integers and CI runs the same probe on three architectures.

**What compaction unconditionally buys** is capacity, which is the larger and more reliable result: 16×
fewer records in the arena (256 KiB back to 16 KiB at the table's largest row), 16× fewer leaf scratch
entries for a commit to hold, a feed 16× shorter, same root. The refusal is tested in both directions —
scratch sized for the compacted region rejects the uncompacted set rather than committing to a partial
region.

**What this does *not* do:** it does not rescue gate P1.3. [OQ-49](13-risks-open-questions.md) lists
compaction frequency as one of four candidate levers, and for the workload the gate was actually quoted
at — a full dirty slab of *distinct* scattered edits, where `live == records` and the transform is the
identity — that option is empty: the pass there costs the floor and compaction has nothing to remove.
Where duplication is high it is a partial time lever, but the floor it exposes (18,489 digests for
2,048 scattered voxels) is still ~13 ms, two orders of magnitude from `< 1 ms`. The live candidates
remain the target, the per-epoch budget, and the gate's scope.

## The commit pipeline

1. Flush outstanding deltas into the epoch transaction pool.
2. Order by Lamport clock (vector clocks per voxel are unaffordable).
3. Apply the pool against the macro rule set for Δt ([doc 05](05-simulation-causality.md)).
4. Recompute **changed** region roots only.
5. Append event tags produced by step 3.
6. Emit the new epoch root; re-anchor base genesis on it.
7. Bump-pointer reset of the consumed slabs; retention per tier.

Step 3 is the hard one: "the engine applies all queued transactions from the mempool against
the cellular automaton rules for Δt steps" only closes once Δt semantics, fast-forwards and the
water/fuel clocks are pinned down. That is [OQ-01](13-risks-open-questions.md), the largest
research risk in the project.

## Determinism boundary

**The ledger's inputs are produced by deterministic Rust (host) and WASM (mod) code. Shader
output never enters the ledger** (ADR-017). `f(pos, epoch)` is deterministic *given identical
inputs and an identical implementation* — which is why the implementation must be one Rust
codebase, not a shader re-derived per vendor. Two machines that disagree about material IDs
would produce two different roots, and ADR-023's 32-byte comparison would report a desync on
every epoch, everywhere.

Corollaries:
- No transcendental results in state math; fixed-point or integer paths where fractional values
  are needed.
- No GPU readback feeding a hash.
- The raymarcher's material IDs come from slab bytes + the epoch root broadcast by the sim, not
  from its own evaluation of the macro state.

## Digest

**Locked 2026-10-11 — [ADR-028](decisions.md) signed off: BLAKE3-256, 32-byte digests.**
Domain-separated prefixes for leaf /
branch / event-tag / entity-state nodes so a delta can't collide with a macro event, and
published test vectors so Rust, WASM and any future language binding agree bit-for-bit. The domain
separation exists — it is [ADR-030](decisions.md)'s `DOMAIN ‖ kind ‖ level ‖ payload` preimage — but
**no vector is published yet**: nothing in this repo computes a digest, so the vectors are owed by
the hasher that will. The source conversation wrote the root as `Hash(...)` without choosing an
algorithm — the choice is ours, and the Merkle pass plus every network comparison inherit it, so it
is logged here explicitly rather than silently picked in code.

**What the sign-off did *not* settle, and what was settled after it:** the tree **shape** — arity,
the order children are hashed in, the rule for an absent/empty node, the byte order of every integer
in a preimage. Those were written nowhere, and an epoch root is not canonical until they are: a peer
that orders children differently computes a different root while believing it is consistent — the
same failure doc 02 guards against when it forbids voxel-level Morton codes, one level up the tree.
The shape is now written down: **artifact A-10**
(`contracts/tree.rs`, [ADR-030](decisions.md)) is that contract, and § The tree shape below is its
mirrored prose. The prose used to be all A-10 could supply on its own, because `contracts/` holds no
behaviour. `crates/ledger` — [docs/12](12-agent-operations.md)'s A2 crate — is the hasher that
executes these rules, so § The tree shape now publishes both the rules and the roots they produce.

**Rejected for this role:** SHA-256 (correct class, ~20x slower for no benefit), xxHash/SeaHash
(fast but **not** collision-resistant — a world root must be), raw CRC (not a hash),
Merkle–Damgård constructions in general (length-extension liability).

## The tree shape

**Drafted 2026-10-11 as artifact A-10 (`contracts/tree.rs`) — recorded in [ADR-030](decisions.md),
signed off and frozen 2026-10-12** once `crates/ledger` computed the vectors below and the two
under-specified halves (which `EpochHeader` fields, and what order the octree walks) were settled by
writing the code rather than by re-reading the prose.
§ Digest chose the hash; this chooses the structure fed to it. Six rules, and all six are load-bearing:
get one wrong and two honest machines produce different roots from the same world, in the same code,
with nothing raising an error.

### The rules, in one table

| Rule | Choice | Why this rather than the obvious alternative |
|------|--------|----------------------------------------------|
| Leaf payload width | 2 bytes for the record's 12 bits, masked | The leaf hashes the payload **opaquely**, so the shape does not wait on [OQ-45](13-risks-open-questions.md) deciding what those bits mean |
| Octree arity / depth | 8 children, 12 levels, one bit of each axis per level | Derived from the region: `4096³ = 2³⁶ = 12 levels × 3 bits`. Not a tunable — it moves only with ADR-029's re-open trigger |
| Child order | `x_bit \| y_bit<<1 \| z_bit<<2` — **x is the low bit**, at every level | Any fixed order works; a *different* fixed order does not. Pinning it as literals is the point |
| Level convention | `level` = branch **height above the voxel leaves** (leaf 0, region root 12) | The standard Merkle count. Mirrored indexing (`level 1` = top) reads more naturally in a loop and corrupts every published vector if the two are mixed |
| Empty subtree | `empty(0) = H(kind 2, level 0, ∅)`; `empty(l) = H(kind 1, level l, [empty(l−1); 8])` | One derived rule instead of a per-level special case, so an implementation cannot disagree about "how empty is level 7" |
| Integer encoding | fixed width, **little-endian**, always | Endianness is the classic silent fork. Pinned once as `i64_bytes`/`u64_bytes`/`u32_bytes`/`u16_bytes` instead of left to each binding |

### Region roots: the octree

A region's root commits to its edited voxels and nothing else. Everything unedited is the empty
ladder, which is a **constant of the grammar** — 13 digests computed once at startup, not per region.

```text
leaf(x, y, z)     = H(kind = NODE_VOXEL_LEAF,  level = 0, LE_u16(payload_12_bits))
node(l, children) = H(kind = NODE_OCTANT_BRANCH, level = l, d(children[0]) ‖ … ‖ d(children[7]))
region_root       = node(12, …)      # children selected by octant_at(offset, l) at level l
```

Position comes from the **path**, never from the payload: a leaf says only what the voxel holds.
That is what keeps the octree free of a flat voxel index — `octant_at` reads one bit of each
`LocalOffset` axis per call, and no 36-bit interleaved coordinate is ever formed
([docs/02 § Octree keys and Morton codes](02-coordinate-system.md)).

Multiple records for one voxel resolve **as the tree is walked**: the caller hands the ledger every
record in causal append order, each leaf carries that order as its `append`, and at level 0 the
greatest `append` in the run is the state the epoch commits. The octree therefore never holds two
leaves for one position in its digest, whichever way the run was partitioned. (It used to be a
separate O(N²) pre-pass; spike S2 measured that at 46 ms per full slab and it was replaced — same
digests, one pass, see § Region roots.) The consequence for determinism is precise and worth stating
twice —

- the root is **position-keyed**: reordering records that touch *different* voxels does not change it;
- the root is **not** arrival-order-independent: reordering two records that touch the *same* voxel
  changes which payload survives. The caller must feed the ledger the causal order (§ The commit
  pipeline, step 2), because tie-breaking by anything else — peer identity, wall clock — is
  [OQ-46](13-risks-open-questions.md)'s question, not this contract's.

Cost, as arithmetic — and now as a measurement too. The model says an epoch with *N* edits in one
region recomputes at most `N × 12` branches, so `N = 10⁵` is ≈1.3×10⁶ BLAKE3 calls. Spike S2 ran on
x86-64 Windows, release profile, `cargo run --release --example merkle_pass -p ledger`:

| Edits in one region | Arrangement | Pass time | **Digests computed** | `12N` bound | ns per digest |
|---|---|---|---|---|---|
| 10³ | clustered (consecutive voxels) | 1.236 ms | 2,003 | 12,000 | 617 |
| 10³ | scattered | 7.007 ms | 9,346 | 12,000 | 749 |
| 10⁴ | clustered | 7.245 ms | 14,999 | 120,000 | 483 |
| 10⁴ | scattered | 63.217 ms | 82,731 | 120,000 | 764 |
| 10⁵ | clustered | 58.758 ms | 134,507 | 1,200,000 | 436 |
| 10⁵ | scattered | 511.243 ms | 716,411 | 1,200,000 | 713 |
| 8,192 (one full 64 KB slab) | scattered | 52.058 ms | 68,524 | 98,304 | 759 |

The digest count is measured, not modelled (`region_root_counted`), which is what makes the table
worth anything: **`12N` is an upper bound, not a cost.** A thousand consecutive edits share most of
their paths and compute 2,003 digests; a thousand scattered ones compute 9,346. Same `N`, same shape,
4.6× the work — so any statement about P1.3 that does not name the arrangement is unfalsifiable.

Four things follow. **The pass scales with digests computed, not with `N`** — the two columns move
together across the six rows. **The first implementation was not that**: an insertion sort over a
walk-order comparator dominated everything — 10⁴ edits cost 133 ms and 10⁵ cost 18.2 s, superlinear,
because sorting is O(N²) while hashing is O(N). Replacing it with an in-place octant partition
(`partition_by_octant`, O(8·len) per level) produced the table above and changed **no digest**: the
roots before and after are identical (`af275f30…`, `a505e6b2…`, `e064430b…`, and `d919613c…` for a
full slab), which is this section's position-keyed claim doing real work rather than sitting in prose.
**And the per-digest cost is BLAKE3's, not ours**: hashing 68,524 identical 266-byte preimages
directly costs 508 ns each against the pass's 759 ns, so preimage construction adds ~250 ns and the
floor is the hash function. That is the number OQ-49 has to argue with.

And **gate P1.3's `< 1 ms` is not met, by about two orders of magnitude at a full scattered slab**
(52 ms, of which ~35 ms is BLAKE3 itself). Solving the gate for its own arithmetic means one of: a
much smaller outstanding `N` per epoch per region (the sweep below puts ~1 ms near 150 scattered or
~1,000 clustered edits, which is a *compaction-frequency* decision, not a hashing one), batching or parallelising the hash pass
across regions, or re-scoping what the gate measures. None of those is a change to make quietly, so
it is [OQ-49](13-risks-open-questions.md) with the numbers attached.

### Where the sub-millisecond line actually falls (S2 part three, 2026-10-12)

`cargo run --release --example pass_budget -p ledger` measures the two things the paragraph above
asserted rather than knew: how the pass behaves as `N` shrinks, and what "batching across regions" is
worth. Informational timings, release profile, one x86-64 Windows machine; the digest column is
measured, not modelled, and it is the reproducible part - the milliseconds moved by up to 30% between
two runs of the same binary, the counts did not move at all.

| Edits in one region | Clustered digests | Clustered time | Scattered digests | Scattered time |
|---|---|---|---|---|
| 1 | 13 | 12 µs | 13 | 10 µs |
| 8 | 24 | 14 µs | 91 | 63 µs |
| 32 | 70 | 35 µs | 354 | 249 µs |
| 128 | 260 | 124 µs | 1,321 | 895 µs |
| 512 | 1,026 | 496 µs | 4,938 | 3.383 ms |
| 2,048 | 4,096 | 1.985 ms | 18,489 | 12.861 ms |
| 8,192 (a full 64 KB slab) | 12,287 | 5.023 ms | 68,524 | 48.589 ms |

- **The 1 ms line sits between 128 and 512 edits, and which side you are on is the arrangement.** 128
  scattered edits cost 895 µs; 512 clustered ones cost 496 µs. So "under 1 ms" is currently true for a
  per-region dirty set of a few hundred edits and false for a full slab by two orders of magnitude, in
  either arrangement. Per digest the cost is steady at roughly 480-770 ns, which is consistent with the
  earlier finding that the floor is BLAKE3 rather than this code.
- **The full-slab scattered row reproduces the published figure independently.** `merkle_pass.rs` reports
  68,524 digests for one full slab of scattered edits; this second example, written later and generating
  its own positions, reports the same 68,524. A measurement two implementations agree on is worth more
  than one, and the first draft of this file disagreed - a too-regular position generator kept two of the
  three coordinates nearly constant and reported 12,287 digests for the same count, a *faster* pass over a
  different arrangement. Comparing against the published number is what caught it.
- **Batching across regions does not reduce the hashing; it increases it.** The same 65,536 edits as eight
  regions of 8,192 compute **548,192 digests**; kept in one region of 65,536 they compute
  **483,285**. Spreading costs 13.4% more digests, because separate trees cannot share interior nodes,
  and the counts are the part that repeats: CI run `38251942742` prints exactly those two numbers on
  x86-64 Linux, x86-64 Windows and ARM64 macOS. The milliseconds did not carry the conclusion. Spread
  versus together measured 9.2% slower here, 15.0% slower on Linux (232.9 vs 202.4 ms), 14.5% slower on
  Windows (262.7 vs 229.5) - and 10.1% *faster* on ARM64 macOS (276.6 vs 308.7). The work difference is
  real and portable; a shared runner cannot be trusted to rank two timings 10% apart, which is the same
  reason gate P1.2's throughput figure is quoted for one machine. So the "batch the pass across regions"
  option in OQ-49 is a *parallelism* idea, not a work-reduction one: it can only help if cores are
  available, and it never reduces the hashing.
- **The epoch's own lists are not where the time goes.** Committing 512 scattered edits with a slot table
  of 1, 2, 8 or 16 rows costs 4.10-4.43 ms in every case: the region pass dominates and the number of
  slots barely registers. More live regions per epoch is therefore cheap in ledger terms, which is the
  opposite of what the "smaller slab budget" option implies about scaling.

What this does **not** decide: OQ-49 stays open. These rows say what each lever would cost or buy, and the
choice among them - target, budget, compaction frequency, or scope - belongs to whoever owns gate P1.3.

What is **fixed** since the first S2 report: the resolution pass. `resolve_last_write_wins` was an
O(N²) pre-pass costing 46 ms per full slab — more than the hashing — and it is gone:
`leaves_from_records` now writes one leaf per record with its `append` index, and the walk resolves
collisions at the leaf by taking the greatest `append` (docs/03 § Epoch roots, the paragraph below).
The feed costs 20–80 µs, the digest for every published vector is unchanged, and a hash map never
appeared on the digest path, which would have been a fresh [OQ-44](13-risks-open-questions.md)
exposure dressed up as an optimisation.

### Epoch roots: the lists

Three ordered collections feed the epoch root, each hashed as a **binary** tree — binary so that a
disagreement bisects into a single differing leaf (gate **P1.6**) instead of rescanning an 8-way fan:

```text
list(∅)        = H(kind = NODE_EMPTY_LIST, level = 0, ∅)
list([e])      = e
list(items)    = H(kind = NODE_LIST_BRANCH, level = 0, d(left) ‖ d(right))
                 # halve at the midpoint; an odd trailing item is paired with itself
```

The odd-count rule is a decision, not a detail: pairing an odd item with *itself* makes a one-item
list and a two-item list of the same element produce different digests (the single item is not
wrapped), while the alternative — carrying it up unchanged — would let a truncated list and a longer
one agree at the top. Rejected: carry-forward.

| Collection | Leaf | Order |
|-----------|------|-------|
| Region-slot table | `H(NODE_SLOT_LEAF, 0, LE_u16(slot) ‖ LE_i64(x) ‖ LE_i64(y) ‖ LE_i64(z))` | ascending slot index |
| Region contributions | `H(NODE_REGION_LEAF, 0, LE_u16(slot) ‖ region_root₃₂)` | ascending slot index |
| Event tags | `H(NODE_EVENT_TAG, 0, LE_u16(tag_id) ‖ opaque bytes)` | emission order (§ commit pipeline, step 5) |

The slot-table root is the one piece that looks redundant and is not. ADR-029 assigns slots on
*first touch* within an epoch, so the mapping from a 16-bit slot to a `RegionKey` is derived from
arrival order. If the epoch root committed only to `(slot, region_root)` pairs, two peers that had
assigned slot 7 to different regions could still agree on the root while disagreeing about the
world — ADR-029's hazard, closed by putting the mapping itself under a root.

Then, in this byte order and no other:

```text
epoch_root = H(kind = NODE_EPOCH_ROOT, level = 0,
    genesis_root ‖ slot_table_root ‖ region_list_root ‖ event_list_root
    ‖ LE_u64(EpochHeader.epoch_index) ‖ LE_u32(EpochHeader.record_count))
```

**Which header fields, and why only those two.** "The LE fields of `EpochHeader`" was the phrase
ADR-030 signed off with, and `crates/ledger` could not compute a vector without naming them. Of
`EpochHeader`'s five fields: `base_genesis_root` **is** `genesis_root` and `event_log_root` **is**
`event_list_root`, so they enter once, from the header, rather than twice as a copy; `region_slots`
is the source of `slot_table_root`, not a separate commitment; and what remains is
`epoch_index` then `record_count`, in that order. A header whose `base_genesis_root` disagrees with
the genesis handed to the same call is a caller bug, and `epoch_root`'s doc comment says so instead
of hashing both and letting the world fork.

### What the base terrain commits to, and why it is not a hash

A region holds 2³⁶ voxels. Hashing the un-edited base field voxel by voxel is not an optimisation
problem, it is a category error — so the base state is committed **intensionally**:

```text
genesis_root = H(kind = NODE_GENESIS, level = 0, LE_u64(seed) ‖ LE_u32(base_rule_id) ‖ LE_u32(rule_version))
```

The root then commits to *the rule that generates the terrain*, not to its output, which is exactly
the move A-03's `EpochHeader::base_genesis_root` field was already making. Two consequences to keep
in view:

- `base_rule_id = 0` currently means "the base rule is not specified yet" —
  [OQ-27](13-risks-open-questions.md) is still open. That is honest, not a placeholder to be
  forgotten: the day the macro rule set is fixed, this field's value changes, and every world root
  changes with it. A shape break that visible cannot happen silently.
- Two peers with different `rule_version` cannot agree on a root, which is the property the
  broadcast-incompatibility check in [docs/10](10-multiplayer.md) wants.

### What this shape deliberately does not do

**No `prev_root`.** An epoch root commits to that epoch's contents and to no predecessor — because
A-03's `EpochHeader` has no predecessor field, and inventing one here would be a contract change to
A-03 rather than a decision about the tree. Chaining roots (a lineage a peer can walk backwards
without a store) is a **storage** concern: [OQ-07](13-risks-open-questions.md) and
[OQ-23](13-risks-open-questions.md) own it, and if chaining is adopted it becomes A-10 v2 with an
ADR and a migration note, not an extra 32 bytes in one implementation.

**No voxel-level Morton code**, per the paragraph above. This had a consequence writing the hasher
exposed: the order a caller hands records over in never reaches a preimage, because the root is
**position-keyed** — the octree places each leaf by the axis bits at each level, so arrival order,
`(x, y, z, append_index)` lexicographic order and octree order all produce the same root (pinned by
`region_root_is_position_keyed_not_arrival_keyed`). What the earlier sentence here claimed — that
lexicographic order *is* the order the octree walks — is simply false: the walk order is Z-order,
which groups by interleaved high bits rather than by `x`. It did not matter for the digest, which is
exactly why nobody had checked. `crates/ledger::walk_less` therefore sorts by octant index per level,
from `OCTREE_DEPTH` down to 1 — Z-order expressed as a comparator, so no 36-bit index is ever formed,
and a subtree's leaves stay one contiguous run for the builder to recurse over.

**No behaviour.** `contracts/` is the shared grammar; `crates/ledger` is the hasher. A WASM binding
builds its preimages with `preimage()` below and hashes them with its own BLAKE3, and that is the
entire agreement it has to keep.

#### Running the published vectors somewhere else, rather than agreeing about them

A published root is only evidence if a second execution produces it. The strongest form of that check
available without new hardware: build the engine for `wasm32-wasip1`, run the module under a WASM
engine, and diff its output against the native build's. CI does exactly this on all three of its
architectures, and the thirteen lines match on each - `parity ok on arm64`, `parity ok on x86_64` twice
over - `EPOCH_0 91b97a52…`, the region list and the slot table included - so the digest is now
produced by two different code generators, two link-time target configurations, and a different
machine-model of function calls, with nothing in this repository writing an `unsafe` block or an
export attribute to achieve it.

Read the scope honestly, because the temptation here is specific and named in docs/02: a WASM engine
is a fourth *kind* of target, not the fourth *device* spike S3 lists. And the lines this diff compares
also carry marched `f32` values and raster counts, which match only because the march uses add,
multiply and `sqrt`; those are presentation, they are not hashed, and no part of this check may be
quoted as gate R4 or R1 evidence (ADR-017 keeps them informational).

## The published vectors

ADR-028's second half — "published test vectors so Rust, WASM and any future language binding agree
bit-for-bit". They exist now, produced by `crates/ledger` and pinned as literals in
`crates/ledger/tests/root_vectors.rs`; reproduce them with

```text
cargo run --release --example root_vectors -p ledger
```

The scenario is deliberately small enough to rebuild by hand, because a vector nobody can trace is a
number, not evidence. Seed `0x5EED_0000_0000_0001`, `base_rule_id = 0`, `rule_version = 0` (OQ-27 still
open), two regions in the slot table — slot 0 at `(0, 0, 0)` and slot 1 at **`(-1, 2, 0)`**, so the
`i64` little-endian encoding is exercised on both sides of zero — and three edits in region slot 0,
handed over in arrival order rather than walk order:

| Input | Digest |
|-------|--------|
| `(0, 0, 0) → payload 1` | one leaf of the octree, position from the path |
| `(2048, 0, 4095) → payload 7` | sets bit 11 of `x` and bit 11 of `z` |
| `(4095, 4095, 4095) → payload 0x0ABC` | the far octant, every bit set |

| Name | 32-byte digest, hex | What it pins |
|------|---------------------|--------------|
| `EMPTY_0` | `427c29d266d450a69922e414274d5a1629d0d64eb3a14287b64e39ee15fe5c76` | `H(kind 2, 0, ∅)` — the ladder's base |
| `EMPTY_12` | `15773ea10c73687eba1425aa12f0979c80b072f699d1e4fecebf85a23553d4ce` | the derived top; also **any untouched region's** root |
| `LEAF_0ABC` | `48cb92891609b2b4c1666825fc734a3beec5f892dba4d4643530dbeba9aed21c` | `H(kind 0, 0, LE_u16(0x0ABC))` |
| `LEAF_0FFF` | `f396d68bd4df28e86892b9347bfa839639aa9d9c0a0c0ec938fc48346d33d4cc` | `voxel_leaf(0xFFFF)` **masked**, not widened (ADR-029's field arithmetic) |
| `GENESIS` | `a367342aaf99f03043f1bf26431e370a66018052e405a3d5d60f9b6436456fee` | the intensional base-terrain commitment |
| `SLOT_LEAF_0` | `e33aa6d522d2842324a0e67100616ebe80d47694685f2dce62dc370e591b598c` | `H(kind 7, 0, 0 ‖ (0,0,0))` |
| `SLOT_LEAF_1` | `305ed78e618947ab1bfab3e93048a0470f5e5412526af121d8cfacb4b7fa55ca` | the same over a **negative** region coordinate |
| `SLOT_TABLE` | `1aa6a154af1de795cafe245752d51df583008abd84ccec3976547a2477f069ab` | ADR-029's first-touch mapping, under its own root |
| `REGION_0` | `97627e3290a2b4f34e708359af9235d6e2d6d417e1491380d37e58178f9bdcbc` | the three edits above, in a 12-level octree |
| `REGIONS` | `0f70e649b228e5d132332b51cfff3aa11badb2199e2cd3980eb4bf16cb6d9a0f` | list over `(slot 0, REGION_0)` and `(slot 1, EMPTY_12)` |
| `EVENTS_EMPTY` | `23b60201bc628839bd07b1d9f891798e30f79183a4db0b1bcbcaefd235238cb8` | kind 3 — an empty epoch, distinct from `EMPTY_12` |
| `EVENTS_ONE` | `ef8d20bb6bf31520bc8188ef8f9b47926536e3edd446999070c091d822fb2795` | a one-item list is the item, **unwrapped** |
| `EVENTS_TWO` | `87b6b9984adcd5486c24746571ec399b04cb7459d995fec050ebfa9f5239bb10` | the kind-8 binary branch over two event leaves |
| `EPOCH_0` | `91b97a52fc5137b50ec64550a94220daab0ffe05575bac6dbfda825a9947cc61` | **the root a peer sends** |
| `EPOCH_1` | `44de758bdfb3eff0c7b431f78ae9b9a8bae4f5c02972e5c60da79c11ccb379a7` | same world, one more event, `epoch_index` 1 |

These are measured, not asserted: `REGION_0` is identical whether the three edits arrive in walk order
or in the shuffled order the example uses (`region_root_is_position_keyed_not_arrival_keyed`), and
`EPOCH_1 ≠ EPOCH_0` is what gate P1.5's comparison is even for. Cross-machine, as of CI run
`38181630859`: all fifteen digests print **byte-identically** on x86-64 Linux, x86-64 Windows and
ARM64 macOS — `EPOCH_0 91b97a52fc5137b50ec64550a94220daab0ffe05575bac6dbfda825a9947cc61` on each —
which is the first epoch root this project has had three machines agree on. It is not yet gate P1.5:
spike S3 names four particular devices and the browser half needs a WASM runner (OQ-41), not merely a
compile.

That the literals genuinely guard the shape was checked the way SPEC-01's masking rule was: the child
iteration in `octant_branch` was reversed — x's bit read as the high bit rather than the low one, a
peer's plausible mistake rather than a fictional one — and `published_vectors_are_frozen` failed with
`REGION_0` computing `d748c899bb444357a0e1f1bcc767bc2c275208719a5bb44bd0f33a7112cc1265` against the
pinned `97627e32…`; `region_root_is_position_keyed_not_arrival_keyed` failed with the same pair, and
the injection was reverted. Worth stating plainly: `child_order_is_x_low_bit_at_every_level` **stayed
green under that injection**, because it re-derives its expectation through the same builder. Only a
pinned literal catches a shape change — which is the argument for publishing vectors rather than
properties alone.

### The contract

Mirrored verbatim from `contracts/tree.rs`; the `tree_contract_matches_spec_document` test in
`contracts/tests/tree_shape.rs` fails the build if the two diverge.

```rust
// contracts/tree.rs — artifact A-10, FROZEN 2026-10-12 (ADR-030 signed off): the Merkle tree **shape**.
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
//   voxel index. Nor does an arrival order or a lexicographic one ever reach a preimage: the root is
//   position-keyed, so the only ordering that matters is the octant path itself (docs/03 § Epoch
//   roots, where `crates/ledger`'s `walk_less` sorts by octant index per level for its own sake).
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
```

## Rejected alternatives

| Alternative | Why rejected |
|-------------|--------------|
| Per-tick global hash chain | Forces `O(N)` replay to reach tick *N*; one global root re-hashes on every edit anywhere (ADR-005/006) |
| Store full block arrays for far chunks | This is exactly the storage/RAM model the design escapes; a dam at region A must not force the server to hold region B |
| Full `SmoothUnion`/`min` composition per record | Composition is a render-time concern; storing it per edit would multiply the log |
| Per-voxel heap allocation, `Vec` growth, copy-on-write chunk arrays | Breaks the `O(1)` write and the `O(1)` reset — the two properties slabs exist to provide |
| Compacting only on sleep | Single-player-only concept; multiplayer has no single sleeper (SPEC-08) |
| Per-voxel vector clocks | Correct but unaffordable; Lamport ordering per region is enough |
| SHA-256 / non-cryptographic hashes | Slow / not collision-resistant respectively |

## Validation gates

| Gate | Test | Source |
|------|------|--------|
| P1.1 | Every delta encodes/decodes to the same `(RegionKey, [u16; 3], value)` across the i64 range, including negatives | doc 02 + [ADR-029](decisions.md) — runs as `p1_1_delta_record_round_trip` (`contracts/tests/delta_record.rs`) against A-03; only the payload's internals are open, as [OQ-45](13-risks-open-questions.md) |
| P1.2 | 10⁶ writes/sec sustained, `p99 < 1 µs` | **our target** — the transcript only claims a slab write is "a pointer offset store instruction… This takes nanoseconds" (GS 1594) and "<1 ns (Direct RAM write)". **Measured 2026-10-12:** 57.2 M appends/sec with p99 200 ns on x86-64 Windows (`crates/memory/examples/slab_throughput.rs`) — met on that one machine, and its clock granularity (~100 ns) is why the percentiles read as multiples of 100 |
| P1.3 | Merkle pass over a full dirty slab array `< 1 ms` | "Sub-millisecond batch pass" (GS 1606) — a claim, not a measurement. **Measured 2026-10-12: NOT met.** One full 64 KB slab of scattered edits passes in **52 ms** (68,524 digests at 759 ns each, of which raw BLAKE3 on identical 266-byte preimages accounts for 508 ns), and the same edit count clustered takes 7 ms — so the pass tracks digests computed, not `N`, and `12N` is a bound rather than a cost. The O(N²) resolution pre-pass that once pushed this to 93 ms is gone (resolution now happens at the leaf, and the feed costs microseconds). Which arrangement the gate means, and whether the target, the budget, the compaction frequency or the scope moves, is [OQ-49](13-risks-open-questions.md) |
| P1.4 | LRU eviction + region re-entry restores all edits with no visible pop-in | ADR-009 |
| P1.5 | Byte-identical roots for the same seed/transaction sequence on N2600 / RTX 3060 / Linux-Mesa / M1-browser | ADR-017 |
| P1.6 | Two clients with one divergent voxel detect it via 32-byte root exchange and repair by bisection | ADR-023. **The bisection half is implemented and tested as of 2026-10-12**: `crates/ledger::repair::first_divergence` walks the octree from the region root, comparing both sides' child digests octant by octant until it descends to the one voxel that differs, and reports it as `(x, y, z)` plus each side's payload. `crates/ledger/tests/bisection.rs` pins the four properties that make the walk trustworthy: it says "none" exactly when the roots agree (including for a peer whose set is merely reordered), it names the voxel that actually changed at sixteen probed positions, an absent side reads as `None` rather than a payload of `0` (absence is the ordinary desync, and a zero would look like agreement with unedited terrain), and two records for one voxel resolve by `append` — **ranked duplicates are order-independent, unranked ones are not**, which is docs/03's "not arrival-order-independent" clause measured rather than asserted. Detection is the exchange (`contracts::errors::RootCompare`); *choosing* a winner is [OQ-46](13-risks-open-questions.md) and ADR-004 defers it, so a hit on this walk today means a non-deterministic replay, not a merge **Updated 2026-10-13:** the repair half that needs no causal rule is landed - `ledger::repair::catch_up` reconciles a peer whose records are a verbatim subset of the leader's and returns the leader's root, refusing every other shape (`Disagrees` names a payload clash, `NotBehind` refuses the wrong direction, `NoRoom` refuses to truncate into a state neither peer holds). Both halves now run in the shipped binary (step 3b of `cargo run -p engine`), and the walk's cost is measured rather than reasoned about: **1.26x to 2.23x** the two root passes a caller runs anyway, the ratio falling as the region grows (`cargo run --release --example p16_walk_cost -p ledger`, digest counts, not microseconds). |
| P1.7 | **A delta committed into epoch _e_ is the value the ledger reads back for that voxel in the same epoch.** `crates/ledger::read::committed_at` answers with the payload and the `append` rank of the live write, and `the_read_and_the_root_never_disagree` rebuilds the read side and hashes it to the region root the commit produced. Absence is its own answer (`Unedited`), never a `0`: what a base rule would generate is SPEC-04's terrain half and [OQ-27](13-risks-open-questions.md), unspecified. | Proposed 2026-10-12 as the commit-side half of [OQ-47](13-risks-open-questions.md), which asked for a named gate "before the ledger crate ships the commit path" — the path shipped, so the gate is named rather than left in prose. It measures *persistence*, not visibility: an edit reaching `M_active` and drawn on the frame it was made needs the renderer and doc 07's latency budget, and stays open. **Status: proposed, needs sign-off.** |
