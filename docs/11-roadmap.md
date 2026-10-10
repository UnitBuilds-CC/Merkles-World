# 11 — Roadmap & milestones

**Status:** sequenced. All dates below are **estimates of unknown quality** — see
§ Timeline honesty.

## Sequencing principle

The design is deep and the temptation is to build the coolest layer first. Resist it: three
contracts gate almost everything downstream, and one research question can invalidate an entire
subsystem. So the order is **resolve → spike → freeze contracts → build**.

```
SPEC/OQ resolution  →  spikes S1–S9  →  contracts/ frozen  →  P0 harness  →  P1 core loop
   (doc 13)             (below)          (doc 12)              (doc 12)         (docs 02–09)
```

## Blocking artifacts (must be written *as artifacts*, before implementation)

| ID | Artifact | Blocked by |
|----|----------|-----------|
| A-01 | `contracts/coords.rs` — `GlobalVoxelPos`, `RegionKey`, `LocalOffset`, `SlabKey`, octree key derivation, `to_region_and_offset()` | SPEC-01/03/06 |
| A-02 | `contracts/ledger-api.wit` — the frozen v1 host surface | OQ-34, ADR-013 |
| A-03 | delta record layout — the packed `u64`, region-slot table, epoch header | SPEC-03, gate P1.1 |
| A-04 | macro field schema + rule library interfaces (what a field *is*) | OQ-01, OQ-08 |
| A-05 | event tag registry (enumerated, versioned) | OQ-08, gate G1 |
| A-06 | circuit graph IR + stateful node interface | OQ-02 |
| A-07 | SDF node grammar schema (`.fbs` + interpreter signature) | ADR-012 |
| A-08 | `ActionTx` wire layout | SPEC-03, doc 10 |
| A-09 | error taxonomy + determinism violation codes | doc 12 agent loop |

Each is a small, reviewable file. Writing them is a **couple of days** of work and they are the
only things that can be written while the open questions are still open — because the act of
writing them is how the questions get answered.

## Spikes (throwaway, timeboxed)

| ID | Question | Success looks like | Feeds |
|----|----------|--------------------|-------|
| **S1** | Can SDF raymarching hold 60 FPS on a mid-range dGPU at our content budgets? | a marching WGSL compute pass with N entities + a real frame-time curve | doc 04 budget, ADR-026 |
| **S2** | Is slab-allocator throughput real? | 10⁶ writes/sec, `p99 < 1 µs`, and a measured sub-ms Merkle pass (P1.2/P1.3) | doc 03 |
| **S3** | Are BLAKE3 roots bit-identical across targets? | N2600 / RTX 3060 / Linux-Mesa / M1-browser agree (P1.5) | ADR-017/028 |
| **S4** | FlatBuffers → GPU: zero-copy upload actually work? | RON→`.bin`→storage buffer, no allocation churn (AS1/AS4) | doc 09 |
| **S5** | wasmtime + WIT determinism across languages | Rust/TS/Python mods agree bit-for-bit (W1–W3) | doc 08 |
| **S6** | Headless WGSL validation harness | shader compiles, renders offscreen, hashes output in CI | doc 12, ADR-003 |
| **S7** | Can macro causality actually propagate? | dam in region A dries region B (SM1 / **gate G1**) | doc 05 — **the highest-risk spike** |
| **S8** | Do the jump methods actually work — closed forms and matrix exponentiation over Δt? | a linear field advanced 10⁶ ticks in `O(log Δt)`, and an honest list of rules that can't (SM2) | ADR-005, [OQ-09](13-risks-open-questions.md) |
| **S9** | What does `wgpu` cost versus raw WebGPU on our pipeline? | measured overhead of the abstraction against the doc 04 frame budget | ADR-003, ADR-026, [OQ-26](13-risks-open-questions.md) |

S7 is the one that can invalidate the architecture. Budget it first, not last.

## Phase 0 — Foundations

**Goal:** a project that can be built at all.

- Workspace, `contracts/` from A-01…A-09, CI running `cargo test`, `clippy -D warnings`,
  `fmt --check`, `cargo deny`, and the S6 headless shader check.
- Coordinate property tests (gate C1/C2/C3) passing on three targets.
- Camera-relative f32 demo: no precision jitter at `x = 10⁹`.
- A dev profile: hot-reload loop end to end (edit a RON file, see the change).

**Exit:** the P0 pipeline in doc 12 works on a real change, and every contract is frozen enough
that two agents could implement against it without talking.

## Phase 1 — Engine runtime (domain-agnostic)

**Goal:** the Voxel-Ledger Engine: state, memory, rendering, physics, circuits, WASM host — with
zero knowledge of pigs or wood.

| Workstream | Deliverable | Gates |
|------------|-------------|-------|
| Ledger & slabs | epoch roots, event log, compaction, tiering | P1.1–P1.6 |
| Renderer | master SDF interpreter, BVH, budget pass, region streaming | R1–R5 |
| Macro sim | field schema, rule library, Δt catch-up | SM1–SM6 (needs S7 ✅) |
| Circuits | graph compiler, combinational solve, stateful nodes | E1–E6 (needs OQ-02 ✅) |
| Physics/audio/anim | gradient collision, sphere-traced occlusion, domain deformation | PH1–DE1 |
| WASM host | WIT surface, fuel metering, hot reload, sandbox | W1–W5 |
| Asset compiler | `assetc` watch/pack modes, schema validation | AS1–AS5 |

**P1 exit — the completion test, which is also the pitch demo:** a booting engine, a rendered
procedural scene, an edit that persists, and one deterministic hash that matches across three
targets.

## Phase 1.5 — Multiplayer *(deferred, [ADR-004](decisions.md))*

`ActionTx` broadcast, prediction/rollback, epoch root exchange, bisection repair, one topology
chosen. Gates M1–M7. **Not before every Phase 1 gate is green.**

## Phase 2 — The game ("Project Genesis")

Content only, through the public API — the official game is Mod #1.

1. **Vertical slice:** one biome, one mob, one tool, one craft, one weather effect. This is where
   the *fun* question gets answered or the project gets re-scoped ([OQ-06](13-risks-open-questions.md)).
2. **Content expansion:** entity/biome flora libraries authored as node trees, progression,
   building tools, circuits as a player-facing system.
3. **Ecosystem & events:** macro rules for growth, weather cycles, migration, resource depletion.
4. **Optimization** for low-end GPUs and browsers; **moderation** and packaging for prompt-to-mod.

Gates: content count, frame budget on minimum spec, mod install → play loop, and the P1.5 replay
harness replaying a scripted 10-hour session.

## Timeline honesty

**Every number below is an unvalidated LLM estimate, quoted because the conversation produced it
and because a plan needs a shape — not because anyone measured it.**

| Milestone | Original estimate | "Accelerated" estimate |
|-----------|-------------------|------------------------|
| Proof of concept | 1 month | — |
| Playable core (walk, edit, clock, sleep-commit) | 3–6 months | 1.5–2 months |
| Minecraft-level diversity | 9–15 months | 4–5 months |

The accelerations were: reuse an existing runtime instead of writing GPU plumbing (6–8 weeks),
WGSL headless auto-testing (3–4 weeks), agent orchestration discipline (4–6 weeks), noise-tree
content bootstrapping (8–12 weeks), single-player-first (3–4 weeks). Treat them as **risk
removals, not time savings** — removing a risk you were about to hit isn't the same as finding
weeks you didn't spend.

**Planning position:** commit to the **gates, not the dates**. Nobody gets to declare a milestone
met by elapsed time; a milestone is met when its gates pass, and the first honest schedule comes
out of the P0/P1 gate data. Expect the accelerated numbers to be optimistic by a lot: three
subsystems are novel (macro causality, compiled-graph circuits with state, SDF frame budget), the
architecture has one unproven claim per pillar, and agent-written GPU code is a debugging
situation none of the estimates priced in.

## What we will NOT do

- Start content work before the lighting/shading contract exists ([OQ-10](13-risks-open-questions.md)).
- Add multiplayer before Phase 1 is green.
- Re-open locked ADRs in code — re-open them in the decision log first.
- Let a benchmark target be "whatever the current code happens to hit".
