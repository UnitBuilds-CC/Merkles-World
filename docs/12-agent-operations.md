# 12 — Agent swarm operations

**Status:** process specified; needs a human to pick the orchestration tooling.

## Why this document exists

> *"LLMs don't fail at writing code; they fail at coordinating."*

The design conversation's own reality check: with 10 uncoordinated agents you get merge
conflicts, API mismatches, broken interfaces and non-terminating debug loops, and as the codebase
grows agents write code that breaks patterns set by other agents (it even named this
**"architectural drift"** as one of three timeline-dictating bottlenecks). The fix is not better
prompts; it is a harness.

## Roles

| Agent | Role | May write | May not |
|-------|------|-----------|---------|
| **A1 Architect** | owns `contracts/` and the dependency DAG; reviews every contract change | contracts, docs | implementation code |
| **A2 Ledger** | Merkle tree, compaction, event log | `src/ledger` | — |
| **A3 Memory** | slabs, allocators, tiering, LRU | `src/memory` | — |
| **A4 Renderer** | WGSL master interpreter, BVH, frame budget | `src/render` | CPU-side state |
| **A5 Physics** | SDF gradients, collision, audio tracing | `src/physics` | — |
| **A6 Circuits** | graph compiler, topology solver, jump functions | `src/circuits` | — |
| **A7 Simulation** | macro fields, rule library, Δt catch-up | `src/sim` | — |
| **A8 WASM host** | WIT bindings, sandbox, fuel metering, hot reload | `src/host` | — |
| **A9 Content** | node trees, RON schemas, mobs, biomes (Phase 2) | `content/` | engine code |
| **A10 CI/CD** | runs the gate suite, rejects breaking merges, routes errors back | `tools/ci` | feature code |

Two humans sit above this: the **System Architect** (owns `docs/decisions.md`, approves contract
changes, breaks ties) and the **Producer** (owns scope and the gate list). The user is the System
Architect.

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
        ┌──────────────────────▼──────────────────────┐
        │  A10 CI/CD: cargo test · clippy -D warnings   │
        │   fmt --check · headless WGSL frame hash ·    │
        │   WASM ABI + determinism check · Merkle vectors│
        └──────┬────────────────────────────┬─────────┘
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
AU\*, AN\*, DE\*, W\*, AS\*, M\*). Gates are **code**: a gate that has no test is marked
`unverified` in doc 11 and cannot be claimed as passed. This is the mechanism that keeps the
design's many quantitative claims from silently becoming assumptions.

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
  coords/         (A1/A2)   doc 02 — the first artifact built
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

**`contracts/coords.rs` is artifact A-01 and is written first** — the design conversation said the
coordinate system "should probably be the first part of the engine", and doc 11 keeps it that way.

## Tooling choices to make (**[OQ-41](13-risks-open-questions.md)**)

Unspecified by the source: the agent orchestration harness, whether agents share one worktree or
use parallel branches, the CI runner (headless WebGPU needs a GPU or a software adapter like
Dawn/wgpu-sw), and how prompts/contexts are versioned alongside the code. Recommend deciding
during Phase 0, since the P0 exit criterion is "this pipeline works on a real change".
