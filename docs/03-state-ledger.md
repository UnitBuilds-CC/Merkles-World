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
published test vectors so Rust, WASM and any future language binding agree bit-for-bit. The
source conversation wrote the root as `Hash(...)` without choosing an algorithm — the choice is
ours, and the Merkle pass plus every network comparison inherit it, so it is logged here
explicitly rather than silently picked in code.

**What the sign-off does *not* settle:** the tree **shape**. Arity, the order children are hashed
in, and the rule for an absent/empty node are written nowhere in this repo, and an epoch root is
not canonical until they are. Pinning them here is `crates/ledger`'s first job, and whatever it
picks becomes a contract with published vectors, because a peer that orders children differently
computes a different root while believing it is consistent — the same failure doc 02 guards against
when it forbids voxel-level Morton codes, one level up the tree.

**Rejected for this role:** SHA-256 (correct class, ~20x slower for no benefit), xxHash/SeaHash
(fast but **not** collision-resistant — a world root must be), raw CRC (not a hash),
Merkle–Damgård constructions in general (length-extension liability).

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
| P1.2 | 10⁶ writes/sec sustained, `p99 < 1 µs` | **our target** — the transcript only claims a slab write is "a pointer offset store instruction… This takes nanoseconds" (GS 1594) and "<1 ns (Direct RAM write)" |
| P1.3 | Merkle pass over a full dirty slab array `< 1 ms` | "Sub-millisecond batch pass" (GS 1606) — a claim, not a measurement |
| P1.4 | LRU eviction + region re-entry restores all edits with no visible pop-in | ADR-009 |
| P1.5 | Byte-identical roots for the same seed/transaction sequence on N2600 / RTX 3060 / Linux-Mesa / M1-browser | ADR-017 |
| P1.6 | Two clients with one divergent voxel detect it via 32-byte root exchange and repair by bisection | ADR-023 |
