# 01 — Architecture overview

**Status:** architecture specified. **A-01 `contracts/coords.rs` is frozen** (2026-10-11, on
[ADR-029](decisions.md)'s sign-off) — every layer may now implement against the coordinate surface.
The rest of `contracts/` is **not** frozen. A layer whose subsystem doc cites an
open `SPEC`/`OQ` is a description, not a build target.
**Depends on:** [doc 00](00-vision.md), [docs/13](13-risks-open-questions.md), the ADR log.
**Consumed by:** docs 02–14.

The complete blueprint, one screen. Each layer has its own doc; this file is the contract
between them.

## The stack

| Layer | Technology | Key property | Doc |
|-------|-----------|--------------|-----|
| Engine runtime (Phase 1) | Rust core + WebGPU/WGSL, desktop and browser | Memory-safe, fearless concurrency, cross-platform | — |
| Coordinate / spatial reference | HDCS: i64 global + f32 camera-relative, `RegionKey` (4096³) | Infinite range, bit-identical across machines | [02](02-coordinate-system.md) |
| Macro state | Merkle hash ledger + octree roots, LSM-style compaction on epoch/sleep | World size bounded by hash+disk, not RAM/CPU ticks | [03](03-state-ledger.md) |
| Micro edits | Sparse delta mask `M_local`, bitpacked `u64` deltas in slabs | O(1) write, cleared at commit | [03](03-state-ledger.md) |
| Memory | Slab allocator, bump pointer, L1 VRAM / L2 RAM, LRU eviction | Zero fragmentation, O(1) mass reset | [03](03-state-ledger.md) |
| Rendering | GPU compute WGSL, voxel SDF raymarching | No meshes, no PNGs, infinite zoom detail | [04](04-rendering.md) |
| Culling | Spatial BVH / bounding spheres mapped to slabs | O(log N) skip for un-hit entities | [04](04-rendering.md) |
| Physics | Analytic SDF gradient queries (`∇SDF`) | Normals/distances without vertex buffers | [07](07-physics-audio-animation.md) |
| Animation | Domain deformation / time-warping of SDF space | No skeletons or keyframes | [07](07-physics-audio-animation.md) |
| Audio | Sphere-tracing the same SDFs for occlusion/reverb | Sound knows the geometry | [07](07-physics-audio-animation.md) |
| Circuits / electricity | Compiled graph topology, solved analytically `f(I, t)` | O(1) regardless of wire length; runs unloaded | [06](06-circuits.md) |
| Macro simulation | Time functions `f(t)` + cellular automata + event signals, WASM handlers | Lazy, fast-forwarded at observation | [05](05-simulation-causality.md) |
| Logic & modding | Polyglot WASM sandbox behind WIT contracts | Deterministic, hot-reloaded, crash-isolated | [08](08-wasm-modding.md) |
| Asset definitions | Universal authoring (RON/JSON/KDL/TOML) → FlatBuffers / static WASM memory | Human to write, zero-copy to run | [09](09-asset-pipeline.md) |
| Multiplayer | Signed intent streams (`ActionTx`), optimistic local + rollback, epoch root verification | 32-byte proof of a whole world | [10](10-multiplayer.md) |
| Development process | 10-agent LLM swarm, contract-first, CI gatekeeper | Humans as architects, not typists | [12](12-agent-operations.md) |

## The data lifecycle

```
┌─────────────────────────────────────────────────────────────────────────┐
│  AUTHORING LAYER        modders / LLM agents write text                   │
│  RON · JSON · KDL · TOML · Rust/TS/C++/Python/Go · WGSL                   │
└──────┬───────────────┬────────────────┬─────────────────┬───────────────┘
       │               │                │                 │
       └───────────────┴───────┬────────┴─────────────────┘
                               ▼   compile-on-save / build pack  (<5 ms, never at frame time)
┌─────────────────────────────────────────────────────────────────────────┐
│  CONTRACT LAYER         canonical FlatBuffers .fbs + WIT worlds           │
│  zero-copy byte layouts, versioned, the only thing the runtime trusts     │
└──────────────────────────────┬──────────────────────────────────────────┘
                               ▼   mmap / memcpy → GPU storage buffers
┌─────────────────────────────────────────────────────────────────────────┐
│                        ENGINE RUNTIME CORE                              │
│  Macro State : Merkle ledger (LSM compaction at epoch/sleep)             │
│  Local Edits : O(1) slab writes (bitpacked u64 deltas)                   │
│  Culling     : spatial BVH for GPU SDF raymarching                       │
│  Physics     : analytic gradient vectors on CPU/WASM                     │
│  Logic       : sandboxed polyglot WASM executables                       │
│  Circuits    : compiled graph topologies, evaluated as f(t)              │
│  Swarm Dev   : contract-driven agent CI/CD loop                          │
└──────────────────────────────┬──────────────────────────────────────────┘
                               ▼   per-frame, camera-relative f32
┌─────────────────────────────────────────────────────────────────────────┐
│  RENDER PROJECTION      WGSL compute raymarcher → pixels                 │
└─────────────────────────────────────────────────────────────────────────┘
```

**The one rule this diagram encodes:** text exists only on the left of the compile step.
The runtime never parses.

## Two-layer state model

```
                       ┌────────────────────────────────────────┐
                       │        GLOBAL MERKLE LEDGER            │
                       │   (stateless macro progression)        │
                       └───────────────────┬────────────────────┘
                                           │  re-anchors base state
                            epoch commit / │  player sleep
                                           ▼
┌───────────────────────────┐        ┌───────────────────────────┐
│     LOCAL DELTA MASK      │ ─────► │   GPU RENDER PROJECTION   │
│  (pending, unbaked edits) │        │  (lazy evaluation @ frame)│
└───────────────────────────┘        └───────────────────────────┘
```

- **Ledger layer:** slow, cheap, authoritative, hashable. Records *what happened* as
  transactions and event tags. Never touches 3D allocations or GPU buffers.
- **Projection layer:** fast, local, disposable. Computes *what you see* from
  `MerkleRoot_N ⊕ LocalDelta` for the pixels in the frustum this frame.

## Core equations

| Name | Form | Meaning |
|------|------|---------|
| View-time state | `BlockState = f(Position, CurrentTick) ⊕ PlayerDelta` | O(1) evaluation; history irrelevant to unedited land |
| Rendered state | `Final = S_base ⊕ M_local` | Base state overridden by sparse edits |
| Epoch commit | `S_new_genesis = S_old_genesis ⊕ M_local`, then `M_local = ∅` | Sleep = compaction |
| Render pass | `pixels = Render(SlabState(FrustumSlabs), C_global)` — state arrives by buffer, the shader only projects it | Server broadcasts a 32-byte root, clients do the work |
| Clock circuit | `state(t) = ⌊(t + φ) · ω⌋ mod 2` | A running clock costs zero ticks |
| Factory output | `items = ∫[t_start → t_sleep] ProductionRate(t) dt` | Machines produce while nobody watches |
| Epoch proof | `EpochHash = MerkleHash(BaseGenesisRoot ⊕ CommittedSlabs)` | One hash proves whole-world agreement |
| Surface normal | `N = ∇SDF(p)` via central differences | Collision response without meshes |
| Camera transform | `P_local = P_global − C_global` | Precision survives an infinite world |

**Note on the render-pass row.** The source wrote it as
`VoxelOutput = ComputeShader(XYZ, MerkleRoot_N) ⊕ LocalDelta` — the GPU deriving voxel state from a
Merkle root. That form is **rejected** ([SPEC-02](13-risks-open-questions.md),
[ADR-017](decisions.md)): `f(pos, epoch)` runs in deterministic Rust/WASM, because WGSL float math
differs per adapter and two GPUs would produce two roots. The row above is the sanctioned reading;
the original is preserved here so the correction is visible rather than silent.

## Invariants

The system is only correct if all of these hold. Tests in doc 11 exist to enforce them.

1. **One coordinate truth.** Every subsystem resolves space through HDCS (doc 02). No
   subsystem keeps its own axis convention, its own origin, or its own units.
2. **Simulation never reads the GPU.** Gradients used for collision are evaluated from SDF
   *source* on CPU/WASM, not read back from VRAM (doc 07).
3. **Hot path is allocation-free.** Frame-time code writes into pre-reserved slab slots only
   (doc 03).
4. **Runtime parses nothing.** Only FlatBuffers / static WASM memory cross into the engine
   (doc 09).
5. **Deterministic or not shipped.** Anything that must match across peers uses integer /
   deterministic float math in Rust→WASM, never shader math (doc 03 § determinism boundary,
   ADR-017).
6. **The delta mask is the memory.** Anything the player changes survives in `M_local` or in
   a committed epoch — unobserved base land is regenerable from seed + tick and is *never*
   treated as authoritative storage.
7. **Epochs are total.** No state exists "between" epochs in the ledger. Mid-epoch truth is
   always `base ⊕ pending`.
8. **Mods are transactions.** A mod mutates the world by emitting ledger transactions, never
   by writing block arrays (doc 08).
9. **Contracts change by ADR only.** No silent interface edits (doc 12).

## What this architecture buys (and what it costs)

**Buys:** world scale limited by hash/disk rather than RAM or tick rate; no loading screens
for *state* (fast-forwarding centuries is a hash+flag update); near-zero storage for untouched
chunks; a living, visibly aged world with no per-entity pathfinding; every asset expressible
as text, hence LLM- and modder-authorable; verifiable multiplayer at 32 bytes per epoch.

**Costs (accepted, each tracked):** traditional redstone-style sequential logic is a real
design problem, not a solved one ([OQ-02](13-risks-open-questions.md)); distant causality
needs an explicit macro-field model ([OQ-01](13-risks-open-questions.md)); SDF raymarching
costs fill-rate and needs BVH discipline to hold 60 FPS; and the world loses *exact*
mid-simulation history — a player who returns at tick 100 is told the endpoint, not the story
(accepted under pillar 2).
