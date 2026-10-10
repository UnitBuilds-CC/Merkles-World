# 00 — Vision & pillars

## What the project is

A voxel sandbox in the Minecraft lineage, built on an engine where the world is not a
database of blocks but a **deterministic function of position and epoch**, and where every
asset, rule, and machine is **code that an LLM (or a modder) can write**.

Two deliverables, strictly separated ([ADR-001](decisions.md#adr-001)):

| Phase | Name | It knows about | Output |
|-------|------|----------------|--------|
| 1 | **Voxel-Ledger Engine** (generic runtime) | Nothing game-specific. State, hashing, slabs, SDF rendering, WASM host, graph compiler. | A platform that boots, renders a procedural sphere, edits it, and compacts the edit into a new Merkle root. |
| 2 | **Project Genesis** (the game) | Pigs, biomes, crafting, hunger, mobs, weather. | The content library that runs on Phase 1 — officially "Mod #1". |

## Design pillars

1. **Simulation ⊥ Rendering.** Rendering is expensive; *simulating* is cheap. So they never
   share a code path. Off-screen reality is lightweight state math; the GPU only projects the
   observed slice at frame time.
   > The point the user made and the whole architecture grew from: *"simulation is cheap,
   > rendered simulation is expensive. Hence separating the two allows for depth and performance."*
2. **Causality is an observation.** An unobserved river is an equation, not a tick history.
   When a player looks at tick 100, the engine evaluates the endpoint state — dry bed, dead
   trees — and never simulates the 50 intermediate steps. The *narrative* of causality is
   reconstructed by the player.
3. **The asset is the algorithm.** No sculpting, no PNGs. A pig is an SDF function plus a
   color function; animation is space deformed over time. This is what lets an all-LLM team
   have an art pipeline at all, and what makes mods 5 KB of text instead of 50 MB of binaries.
4. **The world is a ledger.** Player actions are signed transactions in a mempool; sleep /
   epoch boundaries commit them into a Merkle root. State is verifiable in 32 bytes, and two
   clients prove they see the same world by comparing one hash.
5. **Modding is the native interface.** The engine is built from text-defined assets, compiled
   circuits, and sandboxed WASM — so the public modding API *is* the internal API. Nothing is
   bolted on for modders later.
6. **One coordinate truth.** Every subsystem resolves space through a single hierarchical
   coordinate contract (doc 02) before anything else is written. Direction, gravity, and
   reference frames are specified, not implied.

## Why the Minecraft history matters here

The source conversation starts from "how did Minecraft get popular" and mines it for
transferable causes, not nostalgia:

| Minecraft's actual lever | Our version of it |
|--------------------------|-------------------|
| Sold unfinished alpha builds, updated fast on community feedback (2009 TIGSource) | Ship the Phase 1 engine as a usable platform early; treat players as co-developers. |
| YouTube Let's Plays = free organic marketing, because every playthrough was unique | A world that visibly *changes* between visits gives emergent, tellable stories for free. |
| Zero-friction access (Java applet in the browser) | WebGPU target means a browser tab is a first-class client. |
| Java's accidental superpower: bytecode was decompilable, so modding flourished (MCP → Forge → Fabric) | Make modding deliberate, not accidental: hot-reloaded text assets + WASM, no reverse engineering needed. |
| Emergent mechanics (redstone) attracted engineers and long-tail content | Circuits that stay computable when unobserved — factories keep producing mathematically while you're away. |
| Multi-platform reach (Pocket Edition, consoles) then Microsoft's Bedrock rewrite in C++ | Cross-platform from day one via WASM + WebGPU instead of a future rewrite. |
| Villager AI stayed dumb because 3D pathfinding per entity is expensive and player builds need predictability | Depth comes from the macro-simulation layer (populations, water tables, growth) rather than from more per-entity AI. |
| MineColonies / Millénaire already do the two-tier unloaded-town + realized-town trick | That two-tier trick, generalized, *is* the engine. |

Note what we are **not** copying: tick-by-tick cellular redstone (incompatible with pillar 2 —
[ADR-010](decisions.md#adr-010)), stored-per-block world saves, and mesh/texture asset
pipelines.

## What "done" feels like from the player's chair

You leave a region for a week of in-game time. You come back and the river you damned is
gone, the meadow beyond it has dried and gone brown, the wolf pack has moved on, and the
automated sawmill you built kept producing planks the entire time — all of it rendered
instantly, with no chunk loading, no catch-up lag, and no seed-specific luck. The world aged
while you weren't looking, and it can *prove* it aged the same way for everyone else.

## Traceability map (transcript → docs)

| Transcript section (approx. line) | Becomes |
|-----------------------------------|---------|
| 1–64 Minecraft popularity, why Java | This doc (pillars, lessons) |
| 65–137 C++ vs Rust, Rust benefits | [ADR-002](decisions.md#adr-002), doc 01 tech stack |
| 139–222 Why Minecraft AI is shallow; two-tier sim; ground-truth problem | Doc 05, doc 13 OQ-01 |
| 224–329 Game of Life, hash-anchored macro state, delta mask | Docs 03, 05 |
| 331–451 Merkle tick chain rejected; localized trees; O(1) fast-forward | Docs 03, 05, [ADR-005/006](decisions.md#adr-005) |
| 453–553 Sleep = LSM compaction; causality as observation; feasibility table | Docs 03, 05 |
| 555–623 Two-layer ledger, mempool transactions, epoch commitment | Docs 03, 10 |
| 625–729 Redstone as compiled graphs, O(1) clocks | Doc 06 |
| 731–838 "Code a pig, not sculpt it" — SDFs, raymarching, procedural color | Docs 04, 07 |
| 840–902 10-agent timeline estimates | Doc 11 (and doc 13's warning about them) |
| 904–964 Five accelerations (reuse runtime, WGSL, orchestration, noise trees, SP first) | Docs 04, 08, 11, 12 |
| 974–1169 Modder-friendly: JSON/WASM/WIT/polyglot | Docs 08, 09 |
| 1171–1240 Two-phase project split | Docs 00, 11 |
| 1242–1460 JSON overhead → FlatBuffers/Cap'n Proto/bitpacking; authoring vs runtime | Doc 09, [ADR-014/015/016](decisions.md#adr-014) |
| 1472–1557 Collision, audio, animation, input prediction | Docs 07, 10 |
| 1559–1609 Slab writes | Doc 03 |
| 1611–1688 BVH culling, agent CI/CD, VRAM budgeting, vector clocks | Docs 04, 12, 10 |
| 1690–1849 Coordinate system (HDCS) | Doc 02 |
| 1851–1931 Multiplayer intent streams | Doc 10 |

## Known blind spots

The source conversation is an *architecture* dialogue. It contains no gameplay design, no
art direction, no UI, no lighting model, no save-file format on disk, no performance budget,
and no legal/licensing position. Those are logged as **[gap]** items in
[13-risks-open-questions.md](13-risks-open-questions.md) rather than invented here.
