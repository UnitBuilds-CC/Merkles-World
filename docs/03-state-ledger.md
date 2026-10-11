# 03 — State, ledger and memory

**Status:** specified; implementation blocked on SPEC-01, SPEC-03, SPEC-06, **SPEC-09**,
SPEC-08, OQ-01, OQ-23.
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
- Addressing is `GlobalVoxelPos → (RegionKey, [u16; 3])`; `LocalOffset` is 3×`u16` (SPEC-01).
- The packed delta is 8 bytes: 16 bits region-slot, 16 bits intra-slab, 32 bits value
  ([SPEC-03](13-risks-open-questions.md)).
  16 bits indexes a set of **≤65536 distinct regions per epoch**, which is a hard, checked
  limit — exceeding it before compaction triggers an early micro-commit rather than corruption.
- **The middle field does not fit (SPEC-09).** 16 bits gives 65,536 addresses against a slab of
  2²² voxels, so it cannot be a voxel index inside the slab; read as a *record index* instead, it
  caps a slab at 65,536 × 8 B = 512 KiB, which is a third number next to the 4 MiB slab and the
  64 KB / 1 MB allocation budgets below. The three figures are one unresolved decision, not three
  tunables. [ADR-029](decisions.md) proposes the resolution: take the slab out of the address and
  split the record as 16 region-slot / **36 intra-region (three 12-bit local offsets)** / 12 payload
  — exactly 64 bits, which dissolves the middle-field conflict and leaves slab geometry an
  allocation question. **Until ADR-029 is signed off this file stays unimplemented**, and the 16/16/32
  line above is the superseded draft, not the plan.

## Slab allocation

Pre-allocate contiguous fixed-size slabs keyed by region index. The two sizes historically
written here (**64 KB / 1 MB**) are unreconciled budgets, not a decision: 1 MB of 8-byte records
is 131,072 entries, which the 16-bit record index above cannot address. Spike S2 measures the
real per-epoch edit volume, and SPEC-09 fixes the addressing width; whichever survives sets this
number.

```rust
let slot = slab.base.add(offset * 8);          // pointer arithmetic, no allocator
core::ptr::write_unaligned(slot as *mut u64, packed_delta);
```

- **`O(1)` write** (a store, nanoseconds), no `malloc`, no GC, no OS memory-lock contention.
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
**auto-commit epoch timer** (proposal, unfixed: a wall-clock fallback *and* an N-region change
count) — needed because some players won't sleep, and because multiplayer has no single sleeper
([SPEC-08](13-risks-open-questions.md)). The source's cadence was "every 1,000 ticks or when a
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

**Proposed: BLAKE3-256, 32-byte digests** (ADR-028). Domain-separated prefixes for leaf /
branch / event-tag / entity-state nodes so a delta can't collide with a macro event, and
published test vectors so Rust, WASM and any future language binding agree bit-for-bit. The
source conversation wrote the root as `Hash(...)` without choosing an algorithm — the choice is
ours, and the Merkle pass plus every network comparison inherit it, so it is logged here
explicitly rather than silently picked in code.

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
| P1.1 | Every delta encodes/decodes to the same `(RegionKey, [u16; 3], value)` across the i64 range, including negatives | doc 02 — **tuple shape provisional until SPEC-09** |
| P1.2 | 10⁶ writes/sec sustained, `p99 < 1 µs` | **our target** — the transcript only claims a slab write is "a pointer offset store instruction… This takes nanoseconds" (GS 1594) and "<1 ns (Direct RAM write)" |
| P1.3 | Merkle pass over a full dirty slab array `< 1 ms` | "Sub-millisecond batch pass" (GS 1606) — a claim, not a measurement |
| P1.4 | LRU eviction + region re-entry restores all edits with no visible pop-in | ADR-009 |
| P1.5 | Byte-identical roots for the same seed/transaction sequence on N2600 / RTX 3060 / Linux-Mesa / M1-browser | ADR-017 |
| P1.6 | Two clients with one divergent voxel detect it via 32-byte root exchange and repair by bisection | ADR-023 |
