# 12 — Agent swarm operations

**Status:** process specified; needs a human to pick the orchestration tooling.

## Why this document exists

> *"Having 10 uncoordinated agents write code leads to constant merge conflicts, API mismatches,
> and broken interfaces."* — GS 924

That is the design conversation's own reality check, and this document exists to answer it. Its
enumerated failure modes are merge conflicts, API mismatches and broken interfaces (GS 924),
agents getting **"stuck in non-compiling loops"** (GS 1628), **"GPU debugging loops"** listed as
the first of three timeline-dictating bottlenecks (GS 905), and — as the codebase grows to tens of
thousands of lines — agents "start writing code that breaks design patterns set by other agents",
which the source calls **Context Window Drift & Architectural Drift** (GS 888). The fix the source
names is not better prompts; it is a harness plus a specification document every agent reads
before emitting code (GS 889).

## Roles

**Two id families, never interchangeable:** **A1–A10** are the agents below. **A-01…A-10** are the
blocking *artifacts* in [docs/11](11-roadmap.md). `A1` writes `contracts/`; `A-01` is the file it
writes. When a commit or a doc cites a bare `A-0n`, it means an artifact. From **A-10** the two
families collide for the first time — agent `A10` and artifact `A-10` differ only by the hyphen — so
the rule is absolute: **a hyphen means the artifact, no hyphen means the agent**, in prose, in commit
messages and in test names. An agent id is never written with a hyphen.

| Agent | Role | May write | May not |
|-------|------|-----------|---------|
| **A1 Architect** | owns `contracts/` and the dependency DAG; reviews every contract change | contracts, docs | implementation code |
| **A2 Ledger** | Merkle tree, compaction, event log | `crates/ledger` | — |
| **A3 Memory** | slabs, allocators, tiering, LRU | `crates/memory` | — |
| **A4 Renderer** | WGSL master interpreter, BVH, frame budget | `crates/render` | CPU-side state |
| **A5 Physics** | SDF gradients, collision, audio tracing | `crates/physics` | — |
| **A6 Circuits** | graph compiler, topology solver, jump functions | `crates/circuits` | — |
| **A7 Simulation** | macro fields, rule library, Δt catch-up | `crates/sim` | — |
| **A8 WASM host** | WIT bindings, sandbox, fuel metering, hot reload | `crates/host` | — |
| **A9 Content** | node trees, RON schemas, mobs, biomes (Phase 2) | `content/` | engine code |
| **A10 CI/CD** | runs the gate suite, rejects breaking merges, routes errors back | `tools/ci` | feature code |

Two humans sit above this: the **System Architect** (owns `docs/decisions.md`, approves contract
changes, breaks ties) and the **Producer** (owns scope and the gate list). The user is the System
Architect. Where the table says A1 "may write docs", that means *draft*: an ADR is proposed by A1
and **recorded only by the System Architect**, and no `contracts/` change merges without that
approval.

## The loop

```
        ┌─────────────────────────────────────────────┐
        │  A1 ARCHITECT: writes/locks contracts/       │  ← interfaces only, no impl
        └──────────────────────┬──────────────────────┘
                               │ frozen contract + version
        ┌──────────────────────▼──────────────────────┐
        │  A2–A9: implement in their own module         │  ← cannot edit contracts/
        └──────────────────────┬──────────────────────┘
                               │ PR
        ┌──────────────────────▼──────────────────────────┐
        │  A10 CI/CD: cargo test · clippy -D warnings     │
        │   fmt --check · headless WGSL compile+render    │
        │   frame hash informational (R4) — never a gate  │
        │   WASM ABI + determinism check · Merkle vectors │
        └──────┬────────────────────────────┬─────────────┘
               │ green                      │ red
        merged │                            ▼
               │            error output fed back to the
               │            owning agent with a fixed prompt:
               │   "Fix this exact compiler/shader error.
               │    Do not alter contracts/. Do not change
               │    any public signature."
               └──── retry budget: 3 attempts, then escalate to human
```

## Gate list = the CI contract

The suite is exactly the validation gates written in docs 02–10 (C\*, P1.\*, R\*, SM\*, E\*, PH\*,
AU\*, AN\*, DE\*, W\*, AS\*, M\*). Gates are **code**: a gate is *passed* only when a test for it
exists, runs in CI, and that test's real output has been pasted into the PR
([AGENTS.md](../AGENTS.md) §7). Until then no doc, commit message, or status board may describe it
as green — doc 11 lists the gates and their definitions, and the README status board is the only
place that records state. This is the mechanism that keeps the design's many quantitative claims
from silently becoming assumptions.

Where that code actually lives, as of Phase 0. A gate whose test exercises one crate's API lives
with that crate; `tools/ci/` is reserved for what no single crate can own:

| Harness | File | How it runs |
|---------|------|-------------|
| single-crate gate tests | the owning crate's `tests/` — `crates/coords/tests/gates.rs` holds C1, C2, C3, C4 | `cargo test --workspace`, on every architecture in the CI matrix |
| reproducibility vectors | `cargo run --release --example gate_vectors` | CI prints them; they must equal the frozen vectors recorded in doc 02 |
| measured demos that *are* a gate | `cargo run --release --example camera_f32_demo` (gate C4's run) | CI, release profile |
| doc integrity | `tools/check_docs.mjs` | CI `docs` job — ids resolve, links and anchors exist, tables and fences balance, no personal paths |
| dependency policy | `deny.toml` | CI `deny` job, pinned `cargo-deny` binary |
| cross-cutting harnesses | `tools/ci/` | **does not exist yet** — the home for spike-scale, multi-crate and sandbox probes (the S-series, frame hashes) |

`.github/workflows/ci.yml` is the runner for all of the above. The **S6 headless shader step** that
doc 11's Phase 0 list names is deliberately **absent**: there is no WGSL in the tree to compile, and
adding a job that skips silently would be a green light on nothing
([OQ-41](13-risks-open-questions.md) is the runner decision that unblocks it).

## Merge rules

1. A PR touches **one** module plus its tests. Cross-module PRs are split by A1.
2. `contracts/` changes ship with a migration note, a version bump, an ADR, and the failing
   downstream tests listed.
3. A PR may not add a game-specific concept to the engine (ADR-001 boundary).
4. Hot-path bans (doc 03) are lint-enforced, not reviewed by eye.
5. Determinism: any new cross-platform value gets a test vector in the same PR.
6. Docs update in the same commit as the behavior — an out-of-date doc is a broken build.

## Retry & escalation policy

GPU shader debugging is called out in the source as the place agents thrash ("GPU crashes often
freeze graphics drivers without giving clean stack traces"). So: **max 3 automated fix attempts**,
then the branch is parked with the error transcript attached and a human looks. Repeating a failed
patch is the fastest way to burn a week.

**Determinism violations do not get the three attempts.** [A-09](11-roadmap.md)'s taxonomy
(`contracts/errors.rs`) puts every determinism violation in **band 1**, and
`EngineError::is_determinism_class()` is the machine-readable check CI reads off the integer code.
A band-1 failure means an architectural guarantee broke, and [AGENTS.md](../AGENTS.md) §9 escalates
that on the **first** occurrence — three retries would only produce three commits that each pretend
a root might agree again. The retry budget applies to subsystem bugs, not to this class.

Routing needs no lookup table, because A-09's bands were chosen to match the crate list in § Repo
layout one-for-one:

| Band | Subsystem | Routed to |
|------|-----------|-----------|
| 1 | determinism | parked immediately, escalated to the System Architect (§9, no retry) |
| 2 | coords | A2 implements `crates/coords`, A1 owns its contract file |
| 3 | ledger | A2 |
| 4 | memory | A3 |
| 5 | render | A4 |
| 6 | sim | A7 |
| 7 | circuits | A6 |
| 8 | physics | A5 |
| 9 | host | A8 |
| 10 | asset | `assetc` (tool, not on the frame path) |
| 11 | net | A2 + A8 jointly (Phase 1.5) |
| 12–127 | unassigned | reserved — adding a band is a contract change (merge rule 2) |

## Session workflow (how we actually run this)

1. Pick the highest-leverage open `OQ`/`SPEC` or the next unmet gate — not the most fun task.
2. Write or amend the doc section **first**, then the contract, then the code.
3. Run the gate; paste real output before claiming completion.
4. Commit under the project identity, push at the end of each meaningful unit of work.
5. Update `docs/11-roadmap.md` status and the README status board in the same commit.
6. Mirror substantial doc changes to the GitHub wiki.

## Repo layout (Phase 1)

```
contracts/        ← types, .fbs, .wit, wgsl signatures, error enums  (read-only for A2–A9)
crates/
  coords/         (A2)      doc 02 — contract file owned by A1
  ledger/         (A2)      doc 03
  memory/         (A3)      doc 03
  render/         (A4)      doc 04   (WGSL lives here)
  sim/            (A7)      doc 05
  circuits/       (A6)      doc 06
  physics/        (A5)      doc 07
  host/           (A8)      doc 08   (WASM runtime)
  assetc/         (tool)    doc 09
  net/            (A2/A8)   doc 10   (Phase 1.5)
content/          ← Phase 2 only: RON schemas, node trees, WASM mods (A9)
tools/ci/         ← gate harness (A10)
docs/             ← this documentation
```

**`contracts/coords.rs` is artifact A-01 and the first contract frozen** — the design conversation
said the coordinate system "should probably be the first part of the engine" (GS 1690). Its freeze
was gated on **SPEC-01/03/06/09**; [ADR-029](decisions.md)'s sign-off on 2026-10-11 discharged that
gate: SPEC-09 locked, SPEC-01 and SPEC-03 settled, and SPEC-06 moved out of the address into doc
03's allocation question. Doc 11's order was resolve → spike → freeze → build, and the resolve step
is what closed. From here a change to A-01 is a contract change in the AGENTS.md §2 sense:
migration note + version bump + ADR, never an edit to make one agent's test pass.

**What is in the tree now (Phase 0):**

- **A-01 `contracts/coords.rs`** — **frozen 2026-10-11**, with gates C1–C4 as real tests in
  `crates/coords/tests/gates.rs`. Its `SlabKey` stays a lookup identity; what a slab physically
  holds is SPEC-06's, and that is deliberately outside the frozen surface.
- **A-09 `contracts/errors.rs`** — drafted: banded integer `u16` codes, one stable literal per
  class, and a determinism band that routes separately (above). **DRAFT**: its remaining waits are
  A-02/A-05/A-07 payloads and A-08's freeze — A-08 now exists, so what it owes A-09 is a decision,
  not a file. Now that A-01 has frozen, adopting its newtypes for the
  coordinate payloads is a **scheduled migration** — one version bump, never a code change.
- **A-03 `contracts/delta.rs`** and **A-08 `contracts/tx.rs`** — **drafted 2026-10-11, both DRAFT**:
  the packed delta record with its epoch region-slot table, and the `ActionTx` envelope that carries
  one. Each is mirrored into its spec doc (docs/03 and docs/10) and a test compares the mirror to the
  file line by line, so an agent editing the contract without the doc — or the doc without the
  contract — fails `cargo test` rather than drifting silently.
- **A-10 `contracts/tree.rs`** — **drafted 2026-10-11, DRAFT** against [ADR-030](decisions.md): the
  Merkle tree *shape* (node kinds, octree arity and depth, child order, the two empty rules,
  little-endian byte order, epoch-root composition). Mirrored into docs/03 § The tree shape and pinned
  by `tree_contract_matches_spec_document`, same guard as the two above. A-10 is the grammar **A2
  Ledger** hashes against; the hasher itself is behaviour, so it lives in `crates/ledger` and not here.
  **A2's crate landed on 2026-10-12** with that hasher: `node_parts` is the only place a preimage is
  built, `EmptyLadder` derives the 13 absent-subtree digests, and
  `cargo run --release --example root_vectors -p ledger` prints the vectors docs/03 tabulates. A2 may
  not redefine any of it — a shape change is an ADR-030 re-open plus regenerated root vectors.
- The `contracts` crate is `#![no_std]`, which is a *structural* guard for
  [AGENTS.md](../AGENTS.md) §3/§4: no contract type can reach a clock, a filesystem, a process or
  `alloc`. CI proves the guard still bites by building the crate for `wasm32-unknown-unknown`.
- **A-02/A-04/A-05/A-06/A-07 are not written.** The loop above cannot run for those subsystems until
  they are, and doc 11's blocking-artifact table is the queue.

## Tooling choices to make (**[OQ-41](13-risks-open-questions.md)**)

Unspecified by the source: the agent orchestration harness, whether agents share one worktree or
use parallel branches, the CI runner (headless WebGPU needs a GPU or a software adapter like
Dawn/wgpu-sw), and how prompts/contexts are versioned alongside the code. Recommend deciding
during Phase 0, since the P0 exit criterion is "this pipeline works on a real change".
