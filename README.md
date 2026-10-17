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

The gate-by-gate truth is [docs/11](docs/11-roadmap.md); this table is a summary and is expected to
be *behind* it, not ahead of it. Last reconciled with the code on 2026-10-13.

| Gate | State |
|------|-------|
| Architecture specified | **done** — design level only: 15 docs plus the ADR log, every claim traced to the transcript or marked **[gap]** |
| Implementation | **running, and it boots** — a cargo workspace of eight crates (`contracts`, `coords`, `ledger`, `memory`, `render`, `gpu`, `assetc`, `engine`) behind **201 tests in 31 suites**. `cargo run -p engine` performs the whole demo loop: edits into a slab, commit into the published epoch root, read the edit back, evict a region and re-enter it to the root it left, compile its own scene through `assetc` and march the loaded bytes as a frame — on a CPU, with a bound hierarchy, a work ceiling, and a frame composed from three region cells. There is no window: the boot draws on a CPU, and the device path lives in the `gpu` crate beside it with its frames asserted against this one, so the demo's scene is rendered and checked on both and shown on neither |
| Contracts (A-NN) | **A-01** coords and **A-10** Merkle tree shape **frozen**; **A-07** SDF grammar and envelope drafted and validated by an external `flatc`; **A-02** WIT interface drafted and parse-checked by `wit-parser`; **A-03/A-08/A-09** drafted, still DRAFT pending [OQ-45](docs/13-risks-open-questions.md)/[OQ-46](docs/13-risks-open-questions.md) and the error-payload question [OQ-52](docs/13-risks-open-questions.md) |
| Research spikes | Run: **S2** (slab throughput met; the sub-ms Merkle pass did **not** hold, hence OQ-49), **S7** (macro causality propagates; what it cannot answer is consensus, which needs A-04/A-05), **S1** and **S4** on their CPU halves, **S6** (headless WGSL agreement harness works — the twin disagrees by 2.4e-7 across vendors and the branch waits on the `wgpu` licence decision [OQ-51](docs/13-risks-open-questions.md)), **S3** partly: roots agree on CI's three architectures plus WASM-in-V8, Chromium and Safari/JavaScriptCore, and `tools/s3_device_check.sh` makes the remaining named machines a one-command check — **2 of 2 named targets have reported** so far, a count `tools/check_docs.mjs` reads out of `docs/evidence.json`'s `gate_p1_5_targets` register and fails the build if this page and that register disagree |
| Spec conflicts (SPEC-01…09) | 9 registered, 4 locked, 4 proposals, 1 open — see [docs/13](docs/13-risks-open-questions.md) |
| Open questions | **62 tracked** (OQ-01…OQ-62). The ones gating the most code: [OQ-49](docs/13-risks-open-questions.md) (what gate P1.3 means now that it is measured), [OQ-50](docs/13-risks-open-questions.md) (which world span a voxel coordinate names — blocks "an edit you can see"), [OQ-51](docs/13-risks-open-questions.md) (`wgpu` licence), [OQ-55](docs/13-risks-open-questions.md) (does A-07 grow an instance node) |
| Phase 0 | **largely closed.** CI runs ten job definitions on three architectures — fmt, clippy `-D warnings`, tests, the wasm build, `cargo-deny`, a docs integrity checker, an evidence manifest that CI measures and the docs gate cross-checks, executed-WASM/browser/JavaScriptCore parity legs, and byte-identical asset digests across machines. Still open: the RON-edit dev profile's remaining halves and the decisions above |
| Phase 1 engine | **in progress, gate by gate in docs/11**: ledger clauses P1.1/P1.2/P1.6/P1.7 met (P1.3 awaits its re-scope, P1.4's pop-in needs a renderer, P1.5 needs the named hardware); the renderer has a CPU raster, culling hierarchy, budget pass and region streaming but no GPU, so R1/R2/R4 stay unmeasured by design rather than by omission; asset compiler lowers, writes, reads, watches, packs nothing yet, and refuses over-budget shapes if given a ceiling |
| Phase 2 game content | **blocked** by Phase 1 |
| Repo | published: [UnitBuilds-CC/Merkles-World](https://github.com/UnitBuilds-CC/Merkles-World) (public, wiki enabled), licensed **MIT OR Apache-2.0** — see [License](#license) |

## Deliberate non-goals (for now)

- Traditional Minecraft-style tick-by-tick redstone propagation — the circuit model is
  analytical, not cellular. Accept the design consequence, don't re-litigate it
  (ADR-010).
- Mesh/poly assets, imported `.obj`/`.fbx`/PNG textures.
- Multiplayer consensus in the first playable (ADR-004; protocol is specified in doc 10,
  implementation deferred).
- Non-deterministic world state — if a value must be identical on two machines, it goes
  through the ledger path (doc 03), never the shader path (doc 04 § determinism boundary).

## License

Licensed under either of

- **MIT license** ([LICENSE-MIT](LICENSE-MIT))
- **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE))

at your option. Same terms for the code in `crates/` and `contracts/` — the crates declare
`license = "MIT OR Apache-2.0"` in `[workspace.package]` ([Cargo.toml](Cargo.toml)) and
`cargo deny check licenses` enforces it in CI.

This settles the licensing half of [OQ-43](docs/13-risks-open-questions.md) (2026-10-11). The
naming half is still open, so the crates stay `publish = false` until the engine and game have
final names.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
this work shall be dual licensed as above, without any additional terms or conditions.
