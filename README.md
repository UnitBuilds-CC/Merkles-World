# Project (working name: **Voxel-Ledger Engine** / Phase 2 game **"Project Genesis"**)

A voxel sandbox engine where the world is a **deterministic, cryptographically anchored
function of coordinate + epoch**, player edits are **sparse delta masks**, and every asset is
**code, not art** (SDF shaders + procedural materials, authored by LLM agents or modders).

No 3D modelling. No textures. No mesh pipeline. No per-tick chunk simulation.

---

## Where this came from

Design conversations recorded in the source transcript, cited throughout these docs as **`GS`**
(1930 lines, read 2026-10-11). It is a working document and stays **outside this repository** — see
[AGENTS.md](AGENTS.md).
Everything in `docs/` traces back to it — the traceability map in
[docs/00-vision.md](docs/00-vision.md) shows which section became which document. Anything
*not* in the transcript is explicitly marked **[gap]** and lives in
[docs/13-risks-open-questions.md](docs/13-risks-open-questions.md).

## Read in this order

| # | Doc | What it locks down |
|---|-----|--------------------|
| 00 | [Vision & pillars](docs/00-vision.md) | Why the game exists, what "alive world" means, lessons from Minecraft's rise |
| 01 | [Architecture overview](docs/01-architecture.md) | The full layer map, data lifecycle, core equations |
| 02 | [Coordinate system](docs/02-coordinate-system.md) | **Build this first.** HDCS: i64 global + camera-relative f32 |
| 03 | [State & ledger](docs/03-state-ledger.md) | Merkle epoch roots, sleep compaction, slab delta masks |
| 04 | [Rendering](docs/04-rendering.md) | WGSL SDF raymarching, BVH culling, VRAM budgeting |
| 05 | [Simulation & causality](docs/05-simulation-causality.md) | Macro-sim, event signals, the dam→lake problem |
| 06 | [Circuits & electricity](docs/06-circuits.md) | Compiled graph topologies, O(1) clock evaluation |
| 07 | [Physics, audio, animation](docs/07-physics-audio-animation.md) | SDF gradient collision, sphere-traced occlusion, domain warping |
| 08 | [Logic & modding (WASM)](docs/08-wasm-modding.md) | Polyglot sandbox, WIT contracts, prompt-to-mod |
| 09 | [Asset pipeline](docs/09-asset-pipeline.md) | Universal authoring formats → zero-copy binary |
| 10 | [Multiplayer](docs/10-multiplayer.md) | Intent streams, rollback, Merkle desync detection |
| 11 | [Roadmap & milestones](docs/11-roadmap.md) | Phase 1 (engine) / Phase 2 (game), DoDs, spike backlog |
| 12 | [Agent swarm operations](docs/12-agent-operations.md) | Roles, contract-first CI/CD, session workflow |
| 13 | [Risks & open questions](docs/13-risks-open-questions.md) | **OQ-NN and SPEC-NN registry — check here before coding** |
| 14 | [Glossary](docs/14-glossary.md) | Symbols and terms |
| — | [Decision log (ADRs)](docs/decisions.md) | Locked decisions, rejected alternatives, re-open triggers |

Working rules for agents and humans contributing to this repo: [AGENTS.md](AGENTS.md).
Public wiki mirror of these docs: <https://github.com/UnitBuilds-CC/Merkles-World/wiki>.

## Status board

| Gate | State |
|------|-------|
| Architecture specified | **done** — design level only, all 15 docs + ADR log written |
| Implementation | **none** — the repo contains documentation and contracts-to-be, no code |
| Spec conflicts to resolve before coding (SPEC-01…09) | **9 registered** — 1 locked (SPEC-02 → ADR-017), 6 proposals, 2 open (**SPEC-09** blocks the coords + record contracts, SPEC-07 blocks circuit caching) — see [docs/13](docs/13-risks-open-questions.md) |
| Open questions (OQ-01…43) | **43 tracked**; OQ-01 (macro causality) and OQ-02 (stateful circuits) gate the most code |
| Blocking artifacts (A-01…A-09) | **not started** — [docs/11](docs/11-roadmap.md) |
| Research spikes (S1–S10) | **not started**; S7 (macro causality) is the highest-risk |
| Phase 0 / Phase 1 engine | **not started** |
| Phase 2 game content | **blocked** by Phase 1 |
| Repo | published: [UnitBuilds-CC/Merkles-World](https://github.com/UnitBuilds-CC/Merkles-World) (public) |

## Deliberate non-goals (for now)

- Traditional Minecraft-style tick-by-tick redstone propagation — the circuit model is
  analytical, not cellular. Accept the design consequence, don't re-litigate it
  (ADR-010).
- Mesh/poly assets, imported `.obj`/`.fbx`/PNG textures.
- Multiplayer consensus in the first playable (ADR-004; protocol is specified in doc 10,
  implementation deferred).
- Non-deterministic world state — if a value must be identical on two machines, it goes
  through the ledger path (doc 03), never the shader path (doc 04 § determinism boundary).
