# 10 — Multiplayer

**Status:** protocol specified; deliberately **deferred** to Phase 1.5 per [ADR-004](decisions.md).
Nothing here gets implemented until Phase 1 P1.5 passes.
**Depends on:** docs 02, 03, 05, 06, 08. **Implements:** ADR-021, 022, 023.

## The shift

Traditional engines stream state: block arrays, mesh transforms, serialized chunk data. Here
every client runs identical deterministic WASM logic and identical procedural SDF code, so there
is nothing to stream. You transmit **intent** and let each peer reproduce the consequences.

```
ActionTx = ⟨ SequenceID, ActorID, Kind, Target, Payload, TickEpoch, Signature ⟩
```

| Payload type | Contents | Use |
|--------------|----------|-----|
| **Slab delta mutate** | `RegionKey`, intra-slab offset, bitpacked `u64` value | block breaks, placements, terraforming |
| **Entity kinematics** | `EntityID`, position/velocity/quaternion | player and mob movement — transient, never hashed |
| **WASM signal trigger** | `ModuleID`, `FunctionIndex`, bytes | switches, inventories, spells, circuit inputs |

```
CLIENT A                                    CLIENT B
1. player breaks block                      1. validate ActionTx against current epoch
2. write to local slab (frame 0)            2. apply to its own slab at the same coordinate
3. render immediately                       3. WASM state updates
4. broadcast ActionTx ──── UDP/WebRTC ────► 4. renders on its next frame
```

No 3D block data crosses the wire, and the server never holds a voxel array — it holds roots,
the transaction pool and the event log
([SPEC-03](13-risks-open-questions.md): a wire payload is 16 bytes, which is only possible if
coordinates are region-slot relative per doc 02).

**Rejected:** streaming chunk bytes (bandwidth, and discards determinism); delta-compressed
world snapshots (still state, still large); authoritative-per-voxel server (the bottleneck this
architecture exists to avoid).

## Transport

WebRTC datachannels (browser and mobile hosts) or QUIC (native), with two channel classes:

- **unreliable / unordered** — kinematics. A dropped position frame is superseded by the next.
- **reliable / ordered** — slab mutations and WASM signals. Every break and every circuit trigger
  must arrive, in sequence.

**Targets are ours, not the source's** ([OQ-18](13-risks-open-questions.md)): the design
conversation gave no budget. Propose 200 ms end-to-end for mutations and a 64-player server, and
treat both as tunables to be measured in P1.5 — the "infinite multiplayer worlds" pitch is
otherwise unfalsifiable.

## Consistency model

Three cooperating rules:

1. **Optimistic local execution.** Apply on frame 0, broadcast, validate later.
2. **Causal order within an epoch is Lamport, not wall-clock.** `SequenceID` is
   `(HLC, PeerID)` — a hybrid logical clock. Per-voxel vector clocks were considered and rejected
   as unaffordable, and a client's `HLC_max` is also the staleness bound for its reads: a client
   may never assume a state it read at `HLC_max` can be rolled back.
3. **Conflict resolution is semantic, not last-write-wins.** Raw LWW corrupts
   *aggregate* quantities: if A sets water-table 100→0 and B sets it 100→80, LWW yields a
   whichever-client-wrote-last number, and "80" may be a value neither peer could have derived
   locally. So deltas are **operations** (`set`, `increment`, `append`, `remove`) and
   concurrent ops on aggregate fields are **merged** (`min` for depletion, `max` for accumulation,
   CRDT sets for entity membership). Per-voxel identity conflicts stay LWW-by-Lamport — two
   players painting one block genuinely is a race, and last-write-wins is the honest answer.

**Rollback** is cheap by construction: a rejected slab edit is one store of the old `u64` back
into its slot. Nothing derived geometry from the edit, so there is no rebuild or re-parse.
`M_active` is pinned to **one** client per region (its "owner") and remote deltas for that region
travel over `query-api`, not into the local slab — so a local roll-forward always equals the
canonical state, which is what keeps rollback from cascading into circuits and macro fields.

**Open ([OQ-35](13-risks-open-questions.md)):** when two clients write the same *circuit* input or
the same entity field in one epoch — the merge rules above cover aggregate fields but not
arbitrary mod-defined state, and every mod will think its case is the exception.

## Verification

At each epoch commit every client computes

```
EpochHash = MerkleHash(BaseGenesisRoot ⊕ CommittedSlabs)
```

and peers exchange the 32-byte roots. Matching roots mean the worlds are bitwise identical across
voxels, WASM memory and circuit states — no per-voxel auditing, no trust required.

Mismatch ⇒ the authority walks the Merkle branches by bisection, pinpoints the affected 4096³
region, and resends only that region's slab bytes. **This is a detection-and-repair path, not an
accusation engine** ([OQ-14](13-risks-open-questions.md)): it proves disagreement, never intent.
A client can lie about *which* region diverged, so repairs are authority-driven and a client's
broadcast root is never treated as a statement about another client's state. Detection also
assumes ADR-017 holds — a shader-derived value in the state path would fire desync alarms
everywhere and make the mechanism useless.

## Topology

| Model | How | For |
|-------|-----|-----|
| **Dedicated authoritative server** | processes `ActionTx`, validates via the mod's `validate-tx`, maintains the canonical root, broadcasts | public servers, competitive, modded communities |
| **P2P mesh** | clients exchange `ActionTx` directly over WebRTC; consensus via Lamport ordering + CRDT merges | co-op friend groups |

The same `ActionTx` stream works for both, so the choice is a deployment decision, not a fork.

**The server's role must be understood correctly:** it holds **roots, the transaction pool and
the event log**, *not* voxel arrays. This is what "zero heavy lifting" means
([OQ-03](13-risks-open-questions.md)) — the server validates intent against deterministic rules
rather than mirroring the world.

## Epochs in multiplayer ([SPEC-08](13-risks-open-questions.md), [ADR-027](decisions.md))

"The player sleeps" doesn't exist once several players are online. **Proposed:** one global epoch
advances on a quorum (100% of online players asleep for co-op; 50% for public servers) or a
wall-clock/tick fallback, whichever comes first; the pool between epochs is ordered by Lamport
clock and applied at commit. Rejected: per-player epochs (worlds diverge — the guarantee dies)
and per-action global commits (the cost the architecture avoids).

## Macro causality on the wire

Distant consequences need **both** mechanisms, and the boundary between them is a policy, not an
optimization: event tags propagate far effects (dam → downstream region dries), while explicit
**watch-lists** (region B registers interest in region A's water table) are required for
continuous drain/refill relationships, so the pool accrual doesn't have to "guess" which regions
are coupled. Unresolved: who may add a watch entry, and the memory cost of a watch-list that
grows with the number of player-built reservoirs ([OQ-36](13-risks-open-questions.md)).

## Validation gates

| Gate | Test | Source |
|------|------|--------|
| M1 | Two clients, same transaction sequence, identical 32-byte epoch roots | design claim |
| M2 | Same seed + 1000 actions in two different arrival orders → same epoch root | doc 03 |
| M3 | 100 clients, no world-state streaming: root exchange only | design claim |
| M4 | A rejected edit rolls back with no visual glitch and no circuit re-fire | design claim |
| M5 | Concurrent edits to the same region from 3 clients → deterministic merge on every peer | OQ-35 |
| M6 | Client A's `set` and client B's `increment` on one field → merged, not clobbered | consistency model |
| M7 | *(stretch)* 1000 concurrent editors, sub-second convergence | **invented target** |
