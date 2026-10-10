# 14 — Glossary

Terms and symbols used across the docs. Where a term is load-bearing, the canonical definition
lives in the doc named here.

## Terms

| Term | Definition | Canonical doc |
|------|-----------|---------------|
| **ActionTx** | A signed action intent broadcast to peers: `⟨SequenceID, ActorID, Kind, Target, Payload, TickEpoch, Signature⟩`. The unit of multiplayer traffic. | [10](10-multiplayer.md) |
| **assetc** | The asset compiler: parses authoring text, validates against schema, lowers to FlatBuffers/WASM data. Watch mode in dev, pack mode for release. | [09](09-asset-pipeline.md) |
| **BVH** | Bounding volume hierarchy over active SDFs, so a ray skips an entity in `O(log N)` unless it actually hits its coarse bounds. | [04](04-rendering.md) |
| **Camera anchor** | The integer world position that local (f32) space is measured from this frame. `P_local = P_global − C_global`. | [02](02-coordinate-system.md) |
| **Causal order** | The Lamport/HLC ordering of transactions inside an epoch; *not* wall-clock order. | [10](10-multiplayer.md) |
| **Compaction** | The epoch-commit step: bake outstanding deltas into a new genesis state, recompute roots, reset slab bump pointers. LSM-tree analogy. | [03](03-state-ledger.md) |
| **Circuit graph** | The compiled topology of a connected wire network: nodes, zero-latency edges, and a transfer function `f(I₁…Iₙ, t)`. | [06](06-circuits.md) |
| **Closed form** | A rule whose state at any tick can be computed directly, without stepping through intermediate ticks. What makes fast-forward legal. | [05](05-simulation-causality.md) |
| **Delta mask (`M_local`)** | The sparse set of player-authored differences from base state. It is the world's only memory of what the player did. | [03](03-state-ledger.md) |
| **Determinism boundary** | The rule that simulation state is computed in Rust/WASM and never by a shader. ADR-017. | [03](03-state-ledger.md) |
| **Domain deformation** | Animation by warping the space an SDF is evaluated in, instead of moving vertices. | [07](07-physics-audio-animation.md) |
| **Epoch** | One committed world state, identified by a root hash. The ledger is a sequence of epochs. | [03](03-state-ledger.md) |
| **Event tag** | A named, versioned macro-causal fact in the ledger (e.g. `FlowBlocker{region, magnitude}`). The canonical form of macro causality. | [05](05-simulation-causality.md) |
| **Field (macro)** | A per-region scalar/vector (water table, fertility, population density) evolved by a rule with a declared jump method. | [05](05-simulation-causality.md) |
| **FlexBuffers** | FlatBuffers' schema-free variant, for genuinely dynamic payloads. | [09](09-asset-pipeline.md) |
| **Fuel metering** | WASM instruction budget per call; without it a mod's infinite loop freezes the client. | [08](08-wasm-modding.md) |
| **HDCS** | 64-bit Hierarchical Double-Origin Coordinate System — integer global space, float camera-relative space. ADR-016. | [02](02-coordinate-system.md) |
| **Hot reload** | Edit a text asset or shader, see it in the running game without a restart. | [09](09-asset-pipeline.md) |
| **Jump method** | How a macro rule advances by Δt: `closed_form`, `matrix_exp`, `integral`, or `iterative {cap}`. Every rule must declare one. | [05](05-simulation-causality.md) |
| **Lazy evaluation** | Nothing is computed until something needs it. The engine's answer to unloaded chunks. | [01](01-architecture.md) |
| **Ledger** | The append-only record of epochs, transactions and event tags. Holds roots and deltas — never block arrays. | [03](03-state-ledger.md) |
| **Macro / micro** | Macro = region-level fields evolved per epoch (cheap, simulated). Micro = voxels, entities, fluids in view (expensive, rendered). | [05](05-simulation-causality.md) |
| **Material function** | A color/surface definition as a function of `(p, n, t)`. No texture files exist in this project. | [04](04-rendering.md) |
| **Mempool** | Transactions committed but not yet compacted into an epoch. | [03](03-state-ledger.md) |
| **Morton code** | Z-order curve key for region traversal and octree indexing. Computed on `RegionKey`, never on raw voxel coordinates. | [02](02-coordinate-system.md) |
| **Node grammar** | The declarative shape/material vocabulary (`RoundedBox`, `SmoothUnion`, `Repeat`, `Voronoi`…) interpreted by the master shader. ADR-012. | [04](04-rendering.md) |
| **Octree root** | A region's Merkle root derived from its children. Local, so edits don't re-hash the world. | [03](03-state-ledger.md) |
| **Prompt-to-mod** | In-game LLM authoring that emits RON + WGSL + WASM, then runs the same validation path as a hand-written mod. | [08](08-wasm-modding.md) |
| **Region** | 4096³ voxels, addressed by `RegionKey` (3×`i64`). The unit of the Merkle tree and of slab keying. | [02](02-coordinate-system.md) |
| **RON** | Rusty Object Notation — the canonical in-repo authoring format (comments, trailing commas, maps 1:1 to structs). | [09](09-asset-pipeline.md) |
| **Rollback** | Reverting an optimistic local edit by storing its previous `u64` back into its slab slot. | [03](03-state-ledger.md) |
| **Sector** | 16³ regions; the granularity of named event tags. | [02](02-coordinate-system.md) |
| **SDF** | Signed distance function: negative inside, zero at surface, positive outside. The only geometry primitive. | [04](04-rendering.md) |
| **Slab** | Pre-allocated contiguous fixed-size memory keyed by region. Writes are `O(1)` stores; resets are bump-pointer → 0. | [03](03-state-ledger.md) |
| **Sphere tracing** | Raymarching where each step equals the SDF value — a guaranteed-empty distance. | [04](04-rendering.md) |
| **Watch-list** | A region's registered interest in another region's field, for continuous drain/refill relationships. | [10](10-multiplayer.md) |
| **WIT** | WASM Interface Types — the typed contract between the host engine and mods. | [08](08-wasm-modding.md) |

## Symbols

| Symbol | Meaning |
|--------|---------|
| `S_base`, `S_epoch` | committed base state at an epoch |
| `M_local` | the sparse delta mask of pending edits |
| `M_active`, `M_cold` | hot (VRAM, in interaction radius) and cold (RAM write-ahead) slab tiers |
| `⊕` | apply-overlay: base state overridden by the delta mask |
| `f(pos, t)` | procedural evaluation of base state at position and tick |
| `N = ∇SDF(p)` | surface normal via central differences (6 evaluations) |
| `Δt` | elapsed ticks between an epoch and now — ambiguous in the source, must be declared per rule ([OQ-42](13-risks-open-questions.md)) |
| `EpochHash` | `MerkleHash(BaseGenesisRoot ⊕ CommittedSlabs)` — the 32-byte world-equality proof |
| `ω`, `φ` | clock frequency and phase in `⌊(t + φ)·ω⌋ mod 2` |
| `C_global` | camera anchor in global integer space |

## Abbreviations

| | |
|---|---|
| ADR | architecture decision record — [decisions.md](decisions.md) |
| CA | cellular automaton |
| DoD | definition of done — a gate list, [11](11-roadmap.md) |
| GI | global illumination |
| GS | the source transcript (*How did minecraft get popular.txt*) |
| HLC | hybrid logical clock |
| LOD | level of detail |
| LRU | least-recently-used eviction |
| LSM | log-structured merge-tree (RocksDB/LevelDB analogy) |
| OQ | open question — [13](13-risks-open-questions.md) |
| P0/P1/P1.5/P2 | roadmap phases — [11](11-roadmap.md) |
| SPEC | internal contradiction in the design register — [13](13-risks-open-questions.md) |
| SVO | sparse voxel octree (the original delta-mask storage idea, superseded by slabs) |
| TPS | ticks per second (the server-lag metric Minecraft-style engines are judged by) |
| VAD | virtual address descriptor (GPU memory model detail, doc 03/04) |
