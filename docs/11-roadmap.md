# 11 — Roadmap & milestones

**Status:** sequenced. All dates below are **estimates of unknown quality** — see
§ Timeline honesty.

## Sequencing principle

The design is deep and the temptation is to build the coolest layer first. Resist it: three
contracts gate almost everything downstream, and one research question can invalidate an entire
subsystem. So the order is **resolve → spike → freeze contracts → build**.

```
SPEC/OQ resolution  →  spikes S1–S10  →  contracts/ frozen  →  P0 harness  →  P1 core loop
   (doc 13)             (below)          (doc 12)              (doc 12)         (docs 02–09)
```

## Blocking artifacts (must be written *as artifacts*, before implementation)

| ID | Artifact | Blocked by |
|----|----------|-----------|
| A-01 | `contracts/coords.rs` — `GlobalVoxelPos`, `RegionKey`, `LocalOffset`, `SlabKey`, `SectorKey`, `AabbI64`, octree key derivation, `region_key()` / `local_offset()` | **frozen 2026-10-11.** Was SPEC-01/03/06 + **SPEC-09**: [ADR-029](decisions.md) locks SPEC-09, pins SPEC-01/03, and reclassifies SPEC-06 as doc 03's allocation choice rather than an addressing constraint |
| A-02 | `contracts/ledger-api.wit` — the frozen v1 host surface | OQ-34, ADR-013 |
| A-03 | delta record layout — the packed `u64`, region-slot table, epoch header | **drafted 2026-10-11** as `contracts/delta.rs` (DRAFT) on ADR-029's `\|16 slot\|36 intra-region\|12 payload\|`; gate P1.1 already runs against it as `p1_1_delta_record_round_trip`. Its freeze waits on **OQ-45** (what the 12 payload bits *mean*) and on **SPEC-06**'s slab capacity, which spike S2 must measure |
| A-04 | macro field schema + rule library interfaces (what a field *is*) | OQ-01, OQ-08 |
| A-05 | event tag registry (enumerated, versioned) | OQ-08, gate SM1 / spike S7 |
| A-06 | circuit graph IR + stateful node interface | OQ-02 |
| A-07 | SDF node grammar schema (`.fbs` + interpreter signature) | ADR-012 |
| A-08 | `ActionTx` wire layout | **drafted 2026-10-11** as `contracts/tx.rs` (DRAFT) — doc 10's envelope, split as ADR-015 splits its wire-delta row: `Target` is the address, `TxPayload` the value, and a slab mutation's address+value *is* an A-03 record. Freeze waits on **OQ-46** (what the ids are, how wide, and which scheme signs) and on **OQ-30**/**OQ-23** for the serialized byte format |
| A-09 | error taxonomy + determinism violation codes | doc 12 agent loop |
| A-10 | Merkle tree shape — node kinds, arity, child order, empty rules, canonical byte order, epoch-root composition | **drafted 2026-10-11** as `contracts/tree.rs` (DRAFT) on [ADR-030](decisions.md), the half [ADR-028](decisions.md) explicitly left open. Mirrored into [docs/03 § The tree shape](03-state-ledger.md) and pinned by `tree_contract_matches_spec_document`; gates P1.3/P1.5/P1.6 are only meaningful once it exists. Freeze waits on **ADR-030 sign-off** — a changed rule is a shape break, so every published root vector moves with it |

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
| **S7** | Can macro causality actually propagate? | dam in region A dries region B (**gate SM1**) | doc 05 — **the highest-risk spike** |
| **S8** | Do the jump methods actually work — closed forms and matrix exponentiation over Δt? | a linear field advanced 10⁶ ticks in `O(log Δt)`, and an honest list of rules that can't (SM2) | ADR-005, [OQ-09](13-risks-open-questions.md) |
| **S9** | What does `wgpu` cost versus raw WebGPU on our pipeline? | measured overhead of the abstraction against the doc 04 frame budget | ADR-003, ADR-026, [OQ-26](13-risks-open-questions.md) |
| **S10** | What actually invalidates a compiled circuit graph — whole component, or dirty subgraph? | a real build (10⁴ nodes, lever flipped) recompiled in measurable time, with the invalidation granularity reported | [SPEC-07](13-risks-open-questions.md), [OQ-22](13-risks-open-questions.md), doc 06 gates E1/E4 |

S7 is the one that can invalidate the architecture. Budget it first, not last. S1, S2 and S10 are
the three that can invalidate a *cost model*, and every "instantaneous" claim in docs 03–06 is a
cost model.

## Phase 0 — Foundations

**Goal:** a project that can be built at all.

- Workspace, `contracts/` from A-01…A-09, CI running `cargo test`, `clippy -D warnings`,
  `fmt --check`, `cargo deny`, and the S6 headless shader check.
- Coordinate property tests (gates C1/C2/C3) passing on the three **architectures** C2 names —
  x86-64, ARM64, WASM — which is a smaller set than the four **targets** of the determinism suite
  (§ The Bitwise Determinism Suite below).
- Camera-relative f32 demo: no precision jitter at `x = 10⁹`. This demo **is** gate C4's run:
  camera-space error ≤ 1 f32 ulp at the radius chosen there (doc 02 shows "< 1e-4" is unreachable
  at 4096, so the radius and the epsilon are declared together, not assumed).
- A dev profile: hot-reload loop end to end (edit a RON file, see the change).

**Exit:** the P0 pipeline in doc 12 works on a real change, and every contract is frozen enough
that two agents could implement against it without talking.

### Phase 0 status — 2026-10-11

Measured locally on x86-64 Windows and three times on CI (`.github/workflows/ci.yml`) — run `38124937828`
before the ADR-029 sign-off, run `38150859744` after A-01's freeze and the licence, and run
`38159161144` at the current `main`, with A-03 and A-08 in the tree — each on x86-64 Linux, x86-64
Windows and ARM64 macOS, all at
`rustc 1.92.0 (ded5c06cf 2025-12-08)`.
Each row is the command's real result, not a plan; per-gate evidence lives in doc 02 § Validation gates.

| Check | Command | Result |
|-------|---------|--------|
| Formatting | `cargo fmt --all -- --check` | clean (no diff) |
| Lints | `cargo clippy --all-targets -- -D warnings` | `Finished` — zero warnings; workspace lints deny `clippy::all`, `unsafe_code = "forbid"`, and the hot-path lints `alloc_instead_of_core` / `std_instead_of_core` (their hyphenated spelling is deprecated cargo manifest syntax: it warns today and is documented to stop working in a future edition, which would silently drop the deny — verified the underscore form still fires by injecting a `std::mem::swap` probe and watching clippy exit non-zero) |
| Tests | `cargo test -q` | `4 passed` (contracts: code uniqueness, band placement, stable vectors, determinism routing) + `10 passed` (`contracts/tests/delta_record.rs`: gate P1.1's round trip and record→voxel resolve over the i64 boundaries, ADR-029's field order pinned as one literal word, the slot table's first-touch order and its 65,536 ceiling, and the docs/02 ↔ docs/03 ↔ A-03 layout agreement) + `7 passed` (`contracts/tests/action_tx.rs`: wire tags as literals, ADR-022's channel per kind, envelope class agreement, the A-03 seam, `SequenceId`'s total order, the docs/10 mirror) + `11 passed` (coords: C1/C2/C3/C4 gates, SPEC-01 masking, sector + SlabKey identity, `contract_matches_spec_document`) — **32 passed**. That SPEC-01's *masked, never clamped* rule is genuinely guarded was checked by injecting a clamp into `DeltaRecord::new`, watching `out_of_range_fields_roll_rather_than_clamp` fail with `x: 4095` against `x: 0`, and reverting the injection |
| CI, all three architectures | `gh run view 38159161144` (also green: `38150859744`, `38124937828`) | 6 jobs green: gates × (x86-64 Linux, x86-64 Windows, ARM64 macOS), `wasm32-unknown-unknown` build, `cargo-deny`, docs integrity. Each of the three gate jobs reports the identical `test result: ok.` lines — `4 passed`, `7 passed`, `10 passed`, `11 passed` (**32** total) — and prints the six C2 vectors **bit-identical** (`V0 0xa4659597a9d8aeca` … `V5 0x8e6da94e370cbc70`) with C4 `max \|f32 − exact\| = 0e0` at radii 512 / 2048 / 4096 on every one of them |
| `no_std` / WASM | `cargo build --target wasm32-unknown-unknown -p contracts -p coords` | `Finished` — the `#![no_std]` guard on `contracts/` still bites |
| C2 vectors | `cargo run --release --example gate_vectors` | prints the six `u64` digests that `c2_test_vectors_are_frozen` pins as literals in `crates/coords/tests/gates.rs` (doc 02 § C2 names the test, not the values) |
| C4 demo | `cargo run --release --example camera_f32_demo` | max error `0` at radii 512 / 2048 / 4096; the cast-then-subtract counterfactual returns `0.0` for one voxel at 10⁹ |
| Dependency policy | `cargo deny check all -s` | `advisories ok: 0 errors` / `bans ok: 0 errors` / `licenses ok: 0 errors, 0 warnings, 9 notes` / `sources ok: 0 errors`. The 9 notes are one per unused allowance in `deny.toml` — an empty dependency graph never encounters any of them. The count was 11 until the licence landed: the two extra were the workspace members being skipped as unlicensed, and they are now license-checked against **MIT OR Apache-2.0** ([OQ-43](13-risks-open-questions.md)), whose naming half still keeps `publish = false` |
| Doc integrity | `node tools/check_docs.mjs` | `18 files, 269 local links, 48 OQ + 9 SPEC + 29 ADR + 9 artifacts + 10 spikes defined` → `OK: no findings`. Getting there required fixing the checker itself: it opened a quoted region on **every** apostrophe, so the lifetime ticks in A-03's mirrored block made it swallow real braces and report a phantom imbalance on a block that is character-for-character its contract. Only a well-formed char literal opens a quote now, and a stray brace inserted into that same block is still reported |

**In the tree:** A-01 `contracts/coords.rs` — **frozen 2026-10-11**, its DRAFT header removed when
[ADR-029](decisions.md) was signed off and locked [SPEC-09](13-risks-open-questions.md). A-03
`contracts/delta.rs` and A-08 `contracts/tx.rs` — **drafted 2026-10-11, both DRAFT**: ADR-029 gave
them a byte layout to be written against, so they are the record and the envelope, mirrored verbatim
into docs/03 and docs/10 and pinned against drift by `delta_contract_matches_spec_document` and
`tx_contract_matches_spec_document`. Neither freezes while **OQ-45** and **OQ-46** are open: A-03's
payload field is opaque and A-08's identity widths are labelled drafts on purpose. A-09
`contracts/errors.rs` — still **DRAFT**: it waits on A-02/A-05/A-07 payload shapes and on A-08
freezing (A-08 now *exists*, so what it owes A-09 is a decision, not a file), and now
that A-01 is frozen, adopting its newtypes for coordinate payloads is a scheduled migration rather
than a blocked one.

**Not yet met (the rest of Phase 0's exit criteria):**

- The **S6 headless shader check** has no CI job, because no WGSL exists to compile
  ([OQ-41](13-risks-open-questions.md) is the runner half of that).
- The **dev profile** (edit a RON file, see the change) needs `assetc`, which is Phase 1.
- **WASM execution** of the C2 vectors: CI compiles for `wasm32-unknown-unknown` but runs nothing
  there, so C2 is gated on x86-64/ARM64 execution + WASM compilation, not on three executions.
- **A-02/A-04/A-05/A-06/A-07 are unwritten**, and A-03 and A-08 exist only as **DRAFT**, so the exit
  clause "every contract is frozen enough" is still not met — for a different reason than before:
  two artifacts now need *decisions* (OQ-45, OQ-46) rather than files, while the other five still
  wait on their own open questions (OQ-34, OQ-01, OQ-08, OQ-02, ADR-012).

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
procedural scene, an edit that persists, and one deterministic hash that matches across all four
determinism targets (gate P1.5).

## The Bitwise Determinism Suite

The name AGENTS.md §4 and [ADR-017](decisions.md) cite as the acceptance test for anything another
machine must see. It is not a new test framework — it is the set of gates that must all be green
before a value may be called canonical. It groups gates that already exist — the canonical test
text for each stays in the doc named in the last column, and this table adds no new gate:

| Part | What it proves | Where it runs |
|------|----------------|---------------|
| C2 | identical hashes for the same `(seed, pos)` on x86-64 / ARM64 / WASM | doc 02, Phase 0 |
| C3 | no precision jitter at `x = 10⁹` (Far Lands regression) | doc 02, Phase 0 |
| C4 | camera-space error ≤ 1 f32 ulp at the declared render radius | doc 02, Phase 0 demo |
| P1.5 | byte-identical epoch roots for one seed + transaction sequence on **N2600 / RTX 3060 / Linux-Mesa / M1-browser** | doc 03, produced by spike S3 |
| W1–W3 | Rust / TypeScript / Python mods over the same WIT surface agree bit-for-bit | doc 08, produced by spike S5 |

**Explicitly not in the suite:** gate R4 (frame hash across `wgpu` backends) is *informational* —
a renderer output is not state, and promoting it to a gate would silently re-adopt the
GPU-authoritative model ADR-017 rejects (doc 04).

A feature that produces a value other machines must see is not done until its row here passes.

## Phase 1.5 — Multiplayer *(deferred, [ADR-004](decisions.md))*

**Naming hazard:** "Phase 1.5" (this multiplayer stage) and "gate P1.5" (the determinism gate in
doc 03, run during Phase 1) collide by accident. Neither id is renumbered — commits and AGENTS.md
cite them — so write **"the P1.5 determinism gate"** and **"the multiplayer phase"** in prose.

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

Gates: content count, frame budget on minimum spec, mod install → play loop, and the
**snapshot-replay harness** replaying a scripted 10-hour session (`O(k)` replay from a signed
snapshot, doc 03 — not the P1.5 determinism gate).

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
