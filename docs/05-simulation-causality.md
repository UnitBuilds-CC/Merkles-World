# 05 — Simulation & causality

**Status:** direction locked, mechanism unresolved. **This doc contains the project's biggest
research risk** ([OQ-01](13-risks-open-questions.md)) — read the open questions before writing
any macro-simulation code.

## The problem this layer solves

Minecraft's world simulation is deliberately shallow, and the transcript is clear about why:

1. **Pathfinding and 3D AI are expensive per entity.** Thirty villagers pathfinding to beds
   through dynamic voxel terrain tanks TPS; adding personalities and schedules multiplies that.
2. **Autonomy breaks player builds.** Players treat the world as a deterministic machine — iron
   farms, trading halls, redstone. Self-wanting agents trample them, so Mojang keeps villagers
   simple on purpose.
3. **Entities cease to exist outside simulation distance.** Predator-prey cycles can't survive a
   player walking 128 blocks away.

Games that *do* have deep world simulation — Dwarf Fortress, RimWorld, Mount & Blade,
X4: Foundations — get there by **not rendering most of it**: a distant village is a few
arithmetic updates, not 3D collision and A\* (`Food_{t+1} = Food_t + (Farmers × Yield) −
PopulationConsumption`).

**The lever: simulation is cheap; rendered simulation is expensive.** So this engine separates
them at the architectural level rather than the mod level (MineColonies and Millénaire already
implement exactly this two-tier trick — unloaded towns compute population, resources and
construction numerically, then "realize" structures and NPCs when a player arrives).

## Two tiers

| Tier | Where | What it is | Cost |
|------|-------|-----------|------|
| **Macro** | ledger / epoch | scalar and vector **fields per region**: water table, fertility, temperature, growth stage, population density, resource stock | a few numbers per 4096³ region |
| **Micro** | slabs / GPU, only in view | actual voxels, entities, fluids, circuits | bounded by interaction radius |

Macro fields decide what the micro tier *looks like* when someone arrives. Nothing in the macro
tier has a position finer than a region, and nothing in the micro tier is simulated while
unobserved.

## Causality as observation

The player-facing contract:

> If a river should have dried up at tick 50 but is first viewed at tick 100, then at tick 100
> the river is dry and the trees are dead. The game never simulated the 50 intermediate steps —
> the evaluation at t=100 outputs the correct **endpoint**, and the player's mind supplies the
> narrative.

This is lazy evaluation with the flavor of wavefunction collapse: unobserved = equation,
observed = resolved geometry. It is *deterministic*, not random: any player arriving at the same
coordinate in the same epoch gets the same dry riverbed, because the epoch root anchors the seed.

**What is therefore *not* guaranteed:** the intermediate history. The transcript first sketched
"fast-forward decades in milliseconds", then correctly identified that a state with true
iterative dependencies can't skip steps (a cell at t=10 depends on t=9). **ADR-005 resolves the
contradiction by rejecting the hash chain**, and what remains is a concrete requirement: *every
macro rule must ship a jump method* — closed-form, matrix exponentiation, or a bounded iteration
with a published cap — otherwise the engine can only offer the endpoint of a rule it could
evaluate. See [SPEC-05](13-risks-open-questions.md): `S_{t+1} = f(S_t)` and "O(log Δt)" are not
the same claim, and general CA is neither computable in o(Δt) nor predictable in its long-term
behavior.

## Event signals: how distant causes travel

A pure function of `(position, tick)` cannot know that a dam exists 50 miles away. Distant
causes therefore travel as **named event tags committed into the epoch root**:

```
Tx_dam = { Region: Sector 4, Type: FlowBlocker, Magnitude: 100 }
```

*(The source writes the sector as a scalar label; the contract field is a 3×`i64`
`SectorKey` — doc 02. Below, "Sector 4" means `SectorKey(4, 0, 0)`. The exact tag schema — field
names, types, versioning — is [OQ-08](13-risks-open-questions.md), and artifact A-05 is where it
gets pinned.)*

When region B evaluates at view time, it reads the active event tags and adjusts its output (the
water table downstream of Sector 4 falls to zero, vegetation browns, fauna migrates). Without the
tag, B's math would happily keep a river that should have died three days ago.

The **event log is the canonical form** of macro causality (doc 03): tags are stored as data in
the ledger and *materialized* into region fields at commit. A field value is derived from the
log, never authoritative over it — otherwise two epochs could disagree with no record of who was
right.

**Rejected alternatives**
- Store mutated voxel data for off-screen regions (the "voxel ground truth" problem: reconciling
  abstract growth with a TNT crater the player dug produces state-desync bugs and disk I/O
  bottlenecks).
- Let the background sim overwrite player edits (destroys builds; the delta mask outranks the
  automaton — that precedence is the `S_old_genesis ⊕ M_local` overlay in
  [doc 03 § Compaction](03-state-ledger.md), not a separate rule).
- Full global recomputation each epoch (defeats the purpose).

## The three-stage resolution model *(proposal — validated by spike S7, gate SM1)*

How "the simulation knows" about an action without simulating miles of terrain per action.
Proposed as the answer to OQ-01; **it must be validated before doc 05 is implementable.**

| Stage | Trigger | Work | Cost |
|-------|---------|------|------|
| **1. Local realization** | the edit itself | apply the delta to slabs; recompile the touched circuit graph; realize immediate geometry | `O(1)` write + local graph pass |
| **2. Pool accounting** | each tick | the transaction enters the mempool; the macro sim accrues its *local, bounded* effect (production integrals, consumption deltas) | `O(events)` arithmetic, no geometry |
| **3. Epoch propagation** | commit | Δt of CA/field updates for affected regions **plus** one hop of neighbors; event tags appended; roots re-hashed for changed regions only | `O(regions in range)` at commit, not per action |

**Worked example (the dam).** Build the dam → stage 1 carves it into slabs and renders the wall
immediately; the downstream water column is realized microscopically because it is nearby.
Stage 2 accrues "flow blocked at Sector 4" every tick. At the next commit, stage 3 propagates a
`FlowBlocker(Sector 4, 100)` event, re-evaluates the water-table field for downstream regions and
their one-ring of neighbors, and the lake bed, dead vegetation and wolf-migration pressure become
*inevitable results encoded in the new root*. A player who then travels to region B finds the
GPU rendering a dry lake bed instantly, because the epoch root says Sector 4's water table is
zero.

**Worked example (deforestation).** Chopping a forest's trees is a micro edit. The *macro*
consequence — population decline, soil fertility drop, erosion — is a field update at commit,
computed from resource-stock counters per region, not from re-simulating every stump.

**Open (OQ-01):** how does region B know its neighbor's water table without loading it? The
one-ring propagation at commit is our answer, and its failure mode is the multi-epoch chain
(dam built → riverbed dries next epoch → wells fail the epoch after). If the ring is too small,
effects arrive late; too large, and commit cost explodes. **Spike S7 and gate SM1 are exactly this
question.**

**S7 first run (2026-10-12, `cargo run --release --example s7_propagation -p engine`).** The question was
answered as a measurement: a 512-region river of `i32` water tables, a dam at the source, and a frontier
that simulates only the disturbed width per commit before advancing by the ring. **Latency is distance
over ring:** the mouth goes dry after 501 / 251 / 126 / 64 commits at ring 1 / 2 / 4 / 8, so the
multi-epoch chain named above is exactly what the proposal produces — not a bug in it, a property of it.
**Cost is a curve, not the cliff warned about:** measured against simulating the whole river every
commit, ring 1 spends 57% of that work and ring 8 spends 95%, so widening the ring buys latency with
work pulled forward rather than multiplying work. The claim that does survive is the one the gate leans
on: no region is loaded, simulated or written before the frontier reaches it, so a commit's cost is
bounded by the disturbed area rather than by the world or by the single edit. What the run could not
touch is SM1's other half — *identically for two clients* — because that needs a hashed macro field, and
A-04's schema and A-05's tag registry do not exist yet. The field here is the spike's own stand-in, and
nothing in it is hashed.

**(The final two sentences are half superseded, 2026-10-17.)** SM1's consensus half has since been run and is
now *asserted*: `cargo run --release --example s7_field_root -p engine` takes two clients that applied the same
600 commits — one simulating all 512 regions, one using stage 3's frontier — hashes each `i32` field with
`crates/ledger`, and reaches **one final field root `4a99a06ad6c1180d…`**, with the equilibrium root
`134d7a3fc5efab7a…` pinned beside it. Both are `assert_eq!` constants, so a target that disagrees goes red
rather than printing a different number politely — and CI run `38933143080` prints that pinned line on all three
`gates` jobs, each reporting the same 512 / 257 / 129 / 66 windows and the same final digest. The window in which
the two roots legitimately differ while
both peers hold the same commits is **512 / 257 / 129 / 66 commits at ring 1 / 2 / 4 / 8** — OQ-01's
"acceptable multi-epoch lag" with a measured cost in front of it, though *choosing* an acceptable value is still
OQ-01's open half. What remains true from the sentences above: **A-04 and A-05 still have no contract file**, so
the field leaf rooted there is deliberately spike-local — its `FIELD_KIND` borrows no frozen `contracts::tree`
kind — which means SM1's clause is measured against a stand-in schema, not the one content will ship with.

## Rule library (what content can express)

| Family | Example | Form | Jump method |
|--------|---------|------|-------------|
| **Decay / recovery** | soil fertility, moisture | exponential relaxation toward an equilibrium | analytic closed form |
| **Linear growth/cycle** | crop growth, seasonal temperature | periodic or ramp functions of `t` | analytic |
| **Integrator** | resource stock, population | `dX/dt = inflow − outflow` | closed form for constants; Euler-bounded otherwise |
| **Linear CA** | diffusion-like fields (heat, humidity) | matrix `S_t = A^t S_0` | **matrix exponentiation, `O(log Δt)`** |
| **Nonlinear CA** | forest spread, fire, Conway-style | local rule over a neighborhood | ❌ no general shortcut — bounded iterations only ([OQ-09](13-risks-open-questions.md)) |
| **Event-driven** | migration triggers, spawning | threshold/condition on fields + tags | evaluated at commit |
| **Discrete state machine** | biome phase, weather regime | small state set with transition table | exponentiate the transition matrix |

**CA's proper place.** The transcript is right that voxel engines are full of cellular automata —
Minecraft's water and lava flow, redstone signal propagation, and most background ecosystem sims
are grid state machines running in `O(N)` with tiny constants. In this engine, CA lives in two
places and nowhere else: **micro** (fluids and fire spread only while observed, in view — that
is where block-by-block accuracy actually matters to the player) and **bounded macro** (a fixed,
published generation count per commit). Unlimited CA that must fast-forward is not allowed, and
any CA rule added must state which of these two boxes it lives in. **Neighborhood wrapping is a
doc 02 responsibility**: a cell at a region edge must resolve its neighbor through the region key
so border cells agree across regions, or the automaton silently differs on either side of a
boundary.

## Determinism across versions

Determinism holds for *a fixed implementation and a fixed epoch anchor*, not across engine
releases. Content (rule sets, field schemas, noise trees) is **versioned and referenced by hash
from the epoch root**, so an old world evaluates old rules exactly:

- Each epoch records the rule-set version it was computed under.
- On upgrade, we do **not** re-derive old epochs with new rules; new epochs use the new set.
- Long-lived fields keep **anchor points** — per-region base values — so a formula change can't
  teleport a river 5 km or resurrect a forest the player burned. This is the honest answer to
  the drift the design would otherwise show (a region unseen for a year evaluated by a new
  formula will jump): re-anchoring is a content decision requiring an ADR.

**Rejected:** re-evaluating the whole world under new rules (teleports everything); pretending
upgrades are invisible (silent world corruption).

## Gameplay stance on AI

- **Micro AI stays simple and predictable on purpose.** Player builds must keep working. This is
  a pillar, not a limitation to be eroded: if a mob's behavior can break an iron farm, it belongs
  in the macro tier as a population statistic instead.
- **Complex agents belong to mods** (the Comes Alive / Eco System precedent) — modders get the
  same two tiers plus WASM handlers (doc 08).
- **Villagers** are macro entities: a trading hall's stock, population and growth are fields;
  only the villagers currently in view are realized as entities with paths and schedules.

## Validation gates

| Gate | Test | Status |
|------|------|--------|
| SM1 | Dam built in region A; region B (far, never loaded) evaluates to a dry lake bed at the next epoch, identically for two clients | **spike S7 — the project's key unknown.** Latency and cost measured (S7 first run above): distance over ring, 57% → 95% of whole-river work across ring 1 → 8, and the no-far-region-touched property holding. Consensus half now measured and asserted (`examples/s7_field_root.rs`, a CI step): the frontier client and the whole-river client reach the same final field root `4a99a06a…` after 600 commits, with the disagreement window 512 / 257 / 129 / 66 commits at ring 1 / 2 / 4 / 8, and both digests pinned as `assert_eq!` constants. What still keeps SM1 unmet as a gate: the rooted field is spike-local, because A-04's schema and A-05's tag registry have no file, and OQ-01's acceptable-lag half is an uncalled decision. |
| SM2 | Fast-forward 100,000 ticks of macro state in `< 50 ms` | invented target |
| SM3 | Two runs of the same seed + transaction sequence produce identical epoch roots | ADR-017 |
| SM4 | A rule upgraded mid-world does not teleport existing fields | § Determinism across versions |
| SM5 | A 500-region world with no player edits commits in bounded time | cost model |
| SM6 | Region-edge CA cells agree with their cross-region neighbors | doc 02 integration |

## Open questions

[OQ-01](13-risks-open-questions.md) macro-causality mechanism (incl. propagation radius,
per-field clocks for water/fuel depletion, and whether Δt means ticks or seconds) ·
[OQ-08](13-risks-open-questions.md) event tag vocabulary ·
[OQ-09](13-risks-open-questions.md) which CA rules admit jump evaluation ·
[OQ-13](13-risks-open-questions.md) where "emergent unpredictability" goes once the delta mask
becomes the world · [SPEC-05](13-risks-open-questions.md) closed-form vs CA.
