# 06 — Circuits & electricity

**Status:** model locked; stateful logic unresolved ([OQ-02](13-risks-open-questions.md)).
**Depends on:** docs 02, 03, 05. **Implements:** ADR-010.

## Why redstone can't be ported

Minecraft redstone is a block-by-block cellular automaton: every tick, source tells wire, wire
tells repeater, repeater tells lamp. That requires loaded chunks, lockstep execution, and it
scales `O(N)` with the number of wire blocks — precisely the bottleneck this engine exists to
remove. A wire run 40,000 blocks long that nobody is looking at is, in redstone, 40,000 block
updates per tick or a frozen circuit.

The user's verdict: *"Sounds appropriate"* — accept the trade, keep the ledger philosophy.

## Decision: circuits are compiled graphs

When wire, switches and gates are placed into the delta mask, the engine does **not** run a tick
loop through blocks. It runs a graph traversal (BFS over connected voxel adjacency) and compiles
the network into one object:

```
Circuit = { Inputs: [I₁…Iₙ], Outputs: [O₁…Oₘ], TransferFunction: f(I₁…Iₙ, t) }
```

- **Voxels are representation.** The redstone dust on screen is cosmetic geometry over a graph
  edge.
- **Nodes** are sources, switches, gates, actuators. **Edges** are zero-latency wire paths.
- **Evaluation** is a topological sort then direct boolean/signal solving.

| Metric | Minecraft (CA) | This engine (compiled graph) |
|--------|----------------|------------------------------|
| Execution model | block-by-block spatial propagation | instant boolean / signal graph solve |
| Long-wire latency | 1 tick per 15 blocks | 0, any length |
| Unloaded behavior | frozen unless chunk-loaded | evaluates analytically as `f(t)` |
| CPU cost | `O(N)` wire blocks | `O(K)` active gates |

## Clocks are functions, not loops

A repeater-ring clock in redstone burns CPU forever. Here it compiles to a pure function of time:

```
state_clock(t) = ⌊(t + φ) · ω⌋ mod 2        # square wave, frequency ω, phase φ
```

Leave the chunk for 1,000,000 ticks, come back, and the engine queries `t_now` and evaluates in
`O(1)` whether the line is HIGH or LOW right now. No catch-up simulation, no drift.

## Machines that run while unobserved

A circuit driving a macro action (a sawmill, a smelter, a farm) registers its output as a
**periodic event signal** in the ledger. At commit the ledger integrates:

```
items_produced = ∫[t_start → t_sleep] ProductionRate(t) dt
```

and appends the scalar result to the new epoch root (`IronIngots += 480`). The factory was never
simulated; its output was computed. See the stage-2 accrual in
[doc 05](05-simulation-causality.md) for where this happens per tick.

## Active play vs unloaded

| Situation | Path |
|-----------|------|
| Player flips a lever on screen | transaction into `M_active`; local graph re-solved via topological order; lamp state correct on the next frame |
| Circuit entirely in view | solved eagerly, every change |
| Circuit off-screen, only inputs change | solved lazily at next observation |
| Circuit off-screen with macro outputs | accrues into the epoch pool; integral at commit |
| Circuit in view but **unchanged** | **still solved every frame** — see [OQ-28](13-risks-open-questions.md): the "only re-solve on change" rule would freeze a clock's displayed state |

## Node taxonomy

| Class | Examples | Stateful? |
|-------|----------|-----------|
| Source | power block, solar (function of `t`, daylight), lever, button, pressure plate | manual only |
| Combinational gate | AND, OR, NOT, XOR, NAND | no |
| Sequential element | latch, flip-flop, counter, register, timer/delay element | **yes — the open problem** |
| Delay | repeater-equivalent, expressed as `state(t − δ)` | time-based, no storage |
| Actuator | lamp, piston, door, emitter (→ ledger event), inserter | yes (position) |
| Comparator / analog | signal strength, thresholds | no |
| Bus / mux | labeled wires, bundling | no |

## The unsolved part: sequential logic ([OQ-02](13-risks-open-questions.md))

An acyclic boolean graph evaluated as `f(inputs, t)` can express every *combinational* circuit,
and the "clock = `⌊(t+φ)·ω⌋ mod 2`" trick plus pure delay elements `state(t − δ)` cover a lot of
what players build. It cannot express a **feedback loop** — a T-flip-flop (toggle on button
press), a counter, a register, a RS-latch — because those need state that persists between
evaluations, and a topological sort of a cyclic graph is undefined.

**Proposed direction** (needs an ADR before implementation): stateful node types that own a
`LedgerStateCell` (a small committed value in the epoch root, keyed by node id) and transition as
`next = g(current, inputs, t)`. Feedback is broken by *cutting* the cycle at a stateful node:
compile each strongly-connected component, treat stateful node outputs as graph inputs, and
iterate the SCC to a fixed point at each evaluation. Delay edges make a clocked loop well-formed
without a state cell, which is why delay is a first-class node rather than a wire property.

**Consequences we must decide, not discover later:**
- Every button-press that flips a latch is a ledger transaction, so sequential circuits inherit
  the commit cadence — a counter that changes state 20×/sec would pressure the mempool. Likely
  answer: sequential state commits per epoch, with an *observed* fast path while in view.
- **Observation must not change results** (a hard invariant, and the thing that makes this
  different from redstone). If it does, we have rebuilt chunk-loading by accident.
- A latch whose state is committed at epoch boundaries is a *clocked* design; players used to
  asynchronous redstone will find the timing surprising. That is a content/UX problem
  ([OQ-19](13-risks-open-questions.md)), not an engine bug.

## Graph lifecycle

| Event | Action | Cost |
|-------|--------|------|
| Place/remove a wire or gate | recompile the affected connected component | `O(nodes in component)` |
| Place adjacent to two existing networks | merge components (or split) | `O(component)` |
| Enter a region | load its compiled graphs from the epoch snapshot | `O(graph count)` |
| Circuit emits a macro action | append event to the pool | `O(1)` |

**Compilation happens on the CPU/WASM side, not in the renderer** (ADR-017). Compiled graphs are
cached per region and serialized into the epoch so a returning player doesn't re-derive them.
Invalidation granularity (whole component vs. dirty subgraph) is unresolved
([SPEC-07](13-risks-open-questions.md)).

## Determinism

Graph topology solves in integer/boolean terms. Analog quantities (signal strength 0–15,
comparator thresholds) are fixed-point `u8`/`i16`, never float, because circuit state feeds the
ledger and is compared across peers.

## Rejected alternatives

| Alternative | Why |
|-------------|-----|
| Keep block-CA redstone | breaks lazy evaluation, `O(N)` in wire length, freezes off-screen |
| Chunk loaders / always-loaded machinery | the exact pattern this architecture removes |
| Tick clocks even when unobserved | burns CPU on invisible state; contradicts pillar 1 |
| Ban automation | it's a Minecraft pillar — the least acceptable option, hence doc 06 exists |
| Let the shader compute signal propagation | visuals only; state must be authoritative on the sim side |

## Validation gates

| Gate | Test | Source |
|------|------|--------|
| E1 | 1,000,000-block wire: zero latency, `O(1)` solve | design claim "O(1) zero latency across any wire length" |
| E2 | Clock running since t=0, arrived at after 1,000,000 ticks, evaluates correctly | design claim |
| E3 | Sawmill accrues items over an epoch, and the amount equals the integral | design claim; the **480 ingot figure is illustrative, not a fixture** |
| E4 | Same circuit, same input sequence → identical output on two machines | ADR-017 |
| E5 | *(after OQ-02)* A T-flip-flop toggles once per button press, in view and out of view | open |
| E6 | *(after OQ-02)* Observation of a running circuit does not alter its committed state | open |

## Open questions

[OQ-02](13-risks-open-questions.md) stateful sequential logic ·
[OQ-28](13-risks-open-questions.md) laziness vs. in-view evaluation of unchanged circuits ·
[OQ-22](13-risks-open-questions.md) recompile triggers and cost ·
[OQ-19](13-risks-open-questions.md) player-facing electricity semantics ·
[SPEC-07](13-risks-open-questions.md) graph cache invalidation granularity.
