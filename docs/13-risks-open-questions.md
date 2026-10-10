# 13 — Risks & open questions

**This is the project's control document.** Every other doc links here. Read it before writing
code that touches a subsystem; a `SPEC-` or `OQ-` in **open** state on your subsystem means that
subsystem is not implementable yet.

Three registers: **SPEC** = internal contradiction in the design that must be resolved before
code; **OQ** = missing decision or unproven claim; **Claims** = quantitative assertions inherited
from the source conversation that nobody has measured.

Status vocabulary: `open` (nothing decided) · `proposal` (written here, needs sign-off) ·
`locked` (an ADR exists; change it only by amending the ADR) · `spike-required` (an experiment
decides it).

---

## SPEC — contradictions found in the design

These are genuine conflicts inside the source material. Resolving them is not optional and not
deferrable; each one silently breaks a pillar if code is written against the wrong reading.

| ID | Conflict | Resolution proposed | Status |
|----|----------|---------------------|--------|
| **SPEC-01** | `LocalOffset` is "12-bit uint (0..4095)" in prose but `u16` in the Rust code | keep `u16` field type, 12-bit valid range; masked, never clamped | proposal (doc 02) |
| **SPEC-02** | The sketch lets the GPU compute block state from the Merkle root, but WGSL f32 transcendentals differ per adapter → two GPUs disagree → ADR-023 reports desync everywhere | **ADR-017**: simulation state is computed in deterministic Rust/WASM; the GPU projects, never decides | **locked** |
| **SPEC-03** | A bitpacked `u64` delta holding X/Y/Z (12 bits each) + material ID (12) cannot also hold an `i64` global position | region-slot indirection: `\|16 region slot\|16 intra-slab\|32 value\|`, slot table in the epoch header; region key itself is 3×`i64` and never packs | proposal (doc 02/03) |
| **SPEC-04** | "cryptographic hash (like SHA-256)" vs "spatial hash / Morton" vs "Perlin/Simplex noise" for spatial continuity | **two hashing roles**: BLAKE3 for identity/roots; integer hash + spline interpolation for terrain (doc 05) | proposal (ADR-028) |
| **SPEC-05** | `S_{t+1} = f(S_t)` (iterative CA) vs "fast-forwarding takes milliseconds / `O(log Δt)`" (needs jumpable math) | **per-field clocks**: every rule declares `iterative {cap}` or `closed_form` or `integral`; iterative rules advance by `min(Δt, cap)` and are **never claimed exact** beyond the cap | proposal (doc 05) |
| **SPEC-06** | Slab identity: volume? column? plane? 4096³ slab = 2²⁴ voxels | canonical `(RegionKey, axis, plane_index)` — 4096²×64 **plane** slab, 3 orientations | proposal (doc 02/03) |
| **SPEC-07** | Circuit graph cache invalidation: whole component vs dirty subgraph | undefined in source; must be measured with S1-class spikes on real builds | open (doc 06) |
| **SPEC-08** | "The player sleeps" as the commit boundary is a single-player concept; multiplayer has no single sleeper | one global epoch on quorum-or-timer (ADR-027); compaction stays a policy callback (ADR-004) | proposal (doc 10) |

---

## OQ — open questions

### Architecture & simulation

| ID | Question | Why it matters | Next action |
|----|----------|----------------|-------------|
| **OQ-01** | Can distant causality actually propagate — dam in region A dries region B — within one epoch, without loading region B? | the project's central unknown; the ledger's value proposition depends on it | **Spike S7** (gate G1, SM1) |
| **OQ-02** | How do stateful/sequential circuits work (latches, counters, T-flip-flops, feedback loops) in an acyclic `f(inputs, t)` solver? | doc 06 is unimplementable without this; automation is a Minecraft pillar | design pass → ADR (doc 06) |
| **OQ-03** | What exactly does the server store and validate, if it holds no voxel arrays? | defines hosting cost, the whole "zero heavy lifting" claim, and the cheat model | write the server-role section in doc 10 |
| **OQ-08** | Event tag vocabulary and semantics: is `FlowBlocker{region, magnitude:100}` a one-shot, a latch, or an accumulating scalar? Is the field write a `set` (collision-prone) or a delta? | every macro rule and every mod depends on this registry | draft the tag registry (A-05) |
| **OQ-09** | Which CA rules admit jump evaluation (matrix exponentiation, closed form) and which are irreducibly iterative? | decides what content can promise "the forest grew while you were away" | survey + rule library taxonomy (doc 05) |
| **OQ-13** | If the delta mask becomes the memory, where does *emergent* unpredictability come from — and does a 10,000-edit-per-epoch player break the "near-zero storage" claim? | the design's headline claim vs. its heaviest user | measure with S2, set an edit budget |
| **OQ-15** | Does anything require sub-voxel coordinate precision (physics contacts, audio, camera)? | determines whether f32 local space is sufficient everywhere | decide during P0 |
| **OQ-29** | Are the cheapness claims (`O(1)`, `O(log N)`, sub-ms) verified on real hardware, or are they asymptotic hand-waving? | every one is currently an assertion | each subsystem's gates carry its own claim (doc 11) |
| **OQ-42** | What is Δt *in* for each macro field — ticks, wall-clock seconds, or in-game days? A river's water table, a forest's growth and a wolf migration cannot all use the same clock, and the source used "every 1,000 ticks or when a player sleeps" as if that were one number | decides compaction cost, catch-up semantics and whether "three days later" means anything to the player | per-field clock table in doc 05 |

### Rendering & feel

| ID | Question | Why it matters | Next action |
|----|----------|----------------|-------------|
| **OQ-10** | Lighting model: shadows, GI/AO, sky, day-night cycle. The source conversation never covered it | nothing can be authored until shading is defined — it would have to be redone | art-direction decision (doc 04) |
| **OQ-12** | Transparency, water, volumetrics under raymarching | SDFs and refraction interact badly with a budget; water is *the* voxel-game look | spike alongside S1 |
| **OQ-26** | Is `wgpu` (or `naga`) the substrate, or do we need a thinner custom WebGPU layer? | ADR-026 (reuse vs build) hangs on it; the source was self-contradictory about reuse | **Spike S1** includes this measurement |
| **OQ-27** | Procedural noise: runtime evaluation or precomputed texture? | source contradicts itself; costs GPU memory vs shader cost | resolve in doc 04 during P0 |
| **OQ-28** | Does laziness mean an unchanged in-view circuit stops being re-solved, freezing a clock's displayed state? | would break the "clocks just work" promise | decide with doc 06 |
| **OQ-39** | Minimum-spec performance budget: what GPU, what FPS, at what voxel/entity density? | no target = no definition of "done" for any renderer work | set before P0 |
| **OQ-40** | Audio engine / DSP library, and who owns voice management | audio occlusion is specified but nothing runs it | pick during P1 |

### State, storage & determinism

| ID | Question | Why it matters | Next action |
|----|----------|----------------|-------------|
| **OQ-17** | Ordering and interop when several mods mutate the same field in one epoch (beyond voxel LWW) | determinism promise, mod ecosystem health | doc 10 consistency model |
| **OQ-22** | Circuit recompile triggers and cost when a player edits mid-network | hot path | doc 06 |
| **OQ-23** | What physically persists to disk: event log only, or materialized region fields too? | save size, load time, the "infinite world, near-zero storage" story | doc 03 § storage |
| **OQ-24** | Compaction cadence beyond sleep: on logout? when the pool crosses a threshold? | the stale-world-on-return cost (accepted, but how often?) | doc 03 |
| **OQ-30** | FlatBuffer schema versioning, migration on load, and deterministic serialization | old worlds must load in new engines | doc 09 (A-?) |
| **OQ-37** | Naming and identity: engine name, game name, license, repo layout for two products | Phase 2 is "Mod #1" — the repo boundary must match | product decision |
| **OQ-38** | Target platforms and distribution (browser, Windows/Linux/macOS, mobile, console?) | browser means WASM+WGSL is a hard requirement, not a nice-to-have; console contradicts the WASM-mod story entirely | product decision |

### Multiplayer & trust

| ID | Question | Why it matters | Next action |
|----|----------|----------------|-------------|
| **OQ-11** | Mod signing, distribution, update channels | without it the sandbox promise is decorative | doc 08 trust model |
| **OQ-14** | Anti-cheat: is client authority over local deltas exploitable, and what does a divergent root cost the cheater? | the ledger makes verification cheap but detection is only as good as the rules that generated the state | doc 10 |
| **OQ-18** | Latency and concurrency targets (players/server, ms budget) | decides transport and whether `M_active` ownership survives real RTT | doc 10, Phase 1.5 |
| **OQ-31** | Exploit economics: mods that spawn unbounded entities to force expensive field evaluation | unique attack surface of this architecture | doc 08 |
| **OQ-34** | WASM permissions and quotas: byte caps per query, `MAX_FILTER_BYTES`, per-tick result caps — and who tunes them when legitimate content hits a cap | a mod enumerating world state is a memory-exhaustion vector | doc 08 (A-02) |
| **OQ-35** | Concurrent edits to the same *circuit input* or same entity field (aggregate merge rules cover fields, not arbitrary mod state) | every mod thinks its case is the exception | doc 10 |
| **OQ-36** | Watch-lists: who may register interest in another region's field, and the memory cost when players build thousands of reservoirs | macro causality on the wire needs both tags and continuous drain relationships | doc 10 |

### Modding & content

| ID | Question | Why it matters | Next action |
|----|----------|----------------|-------------|
| **OQ-04** | What do you actually *do* in the game? The design is an engine; no gameplay loop exists | an engine is not a game | vertical slice brief (doc 11) |
| **OQ-05** | UI/HUD, input mapping, accessibility | nothing specified anywhere | wireframe pass |
| **OQ-06** | Content scope for the vertical slice (one biome, one mob, one tool, one craft, one weather effect) — and does it look good, given "I'm not much of an artist" is an *aesthetic* constraint, not just a pipeline one | the SDF look is a real risk: procedural smooth-blended shapes may read as "blobby demo scene" rather than charming | build it, then judge |
| **OQ-07** | Save/on-disk layout, world directory structure, corruption handling, multi-world | players keep worlds; this is where engines die in production | doc 03 § storage |
| **OQ-16** | Physics cost for many entities (gradient queries per mob), LOD physics, navmesh for macro mobs | the CPU-bound part of the design | spike with S1 |
| **OQ-19** | How do players *learn* circuits that don't behave like redstone (zero latency, no visible propagation)? | the fun and the tutorials depend on it; also the honest answer to "will redstone fans accept this?" | design pass |
| **OQ-20** | Per-module execution/memory limits and fuel metering | a WASM infinite loop otherwise freezes the client | doc 08 |
| **OQ-21** | Prompt-to-mod: moderation, abuse, quality control, and which model (API, local, cost) | "infinite mods from English" is also infinite attack surface and moderation load | Phase 2 scope |
| **OQ-25** | Audio tracing cost per voice per frame; voice cap; material LUTs | unmeasured but specified | spike in P1 |
| **OQ-32** | Mod unload semantics and state cleanup | leaking mods across hot-reloads | doc 08 |
| **OQ-33** | Mod version migration and compatibility windows | long-lived worlds and long-lived mods | doc 08/09 |

---

### Process & tooling

| ID | Question | Why it matters | Next action |
|----|----------|----------------|-------------|
| **OQ-41** | The agent harness itself: orchestration tool, one shared worktree vs parallel branches, CI runner that can do headless WebGPU (real GPU, or Dawn/wgpu software adapter), and how prompts + contexts are versioned with the code | Phase 0's exit criterion is "the P0 pipeline works on a real change" — that is a tooling decision, not a code decision | decide during P0 (doc 12) |
| **OQ-43** | Public-release hygiene: the repo is already public, but the engine and game have no final names and no license chosen (OQ-37) — what ships under what name and terms? | an unlicensed public repo means no one may legally use it, and "Mod #1" needs a home | choose license before publishing anything beyond design docs |

---

## Claims inherited from the source, unmeasured

The conversation states performance facts with confidence. They are **hypotheses**. Each one is
attached to the gate that will confirm or kill it.

| Claim | Where it came from | Confirmed by |
|-------|--------------------|--------------|
| Naive evaluation of 500 entities "instantly drops your framerate to single digits"; BVH gives an `O(log N)` skip | culling gap section | R1 / S1 |
| "60 FPS with 500 culled entities in scene" | **our combined target** — the source separately aspires to "60+ FPS" and separately warns about 500 entities; it never states the pair | R1, spike S1 |
| `O(1)` or `O(log Δt)` time-jump for background automata via closed forms / matrix exponentiation | fast-forward fix section | S7, SM2 |
| "`O(1)` instant evaluation" of a noise function at any tick | procedural-vs-CA section | C-gates + S1 |
| "sub-millisecond batch pass" for Merkle compaction over slabs | slab lifecycle table | P1.3 |
| "GPUs can render millions of voxels per second" with zero disk reads | feasibility section | R1 |
| Voxel edit = a pointer-offset store, "takes nanoseconds" | slab write path | P1.2 (our number, not theirs) |
| "Zero runtime memory fragmentation, zero GC pauses, absolute zero-copy execution" | slab conclusion | P1.2, R2 |
| "Near-zero storage for unmodified chunks" | architecture wins | OQ-13, OQ-23 |
| World size "limited only by disk/hash storage, not RAM or CPU tick speeds" | ledger wins | OQ-23 |
| WASM runs at "~80–90% the speed of native C/Rust" | modding section | S5 |
| A 10-agent swarm "speeds up writing code by 10x–20x" | reality-check section | not measurable — treat as rhetoric |
| An agent writes "a new 400-line SDF GLSL file for an entity in minutes" vs artists' 3 weeks | content scaling section | Phase 2 content throughput |
| A from-scratch raymarcher/allocator/compute pipeline costs "2–3 months of pure low-level debugging" | acceleration #1 | ADR-026, spike S1 |

**Provenance note:** the numbers in the Confirmed-by column are the gates that will replace these
assertions with measurements. Until then they are quoted opinions from an AI conversation, not
data — several of them (single-digit framerates, sub-millisecond compaction) are exactly the kind
of claim that dies on first contact with a GPU.

## How to use this document

- **Before coding:** find your subsystem's rows. If any are `open`, write the decision, don't
  write the code.
- **After deciding:** move the row to `locked`, add or amend the ADR in
  [decisions.md](decisions.md), and update the subsystem doc — in the same commit
  (see [AGENTS.md](../AGENTS.md)).
- **New questions:** append with the next free `OQ-NN`. Never renumber or reuse an ID; the IDs
  are cited across every other doc and in commit messages.
- **The weekly review** picks the three highest-leverage `open` rows and assigns owners. S7
  (OQ-01) and OQ-02 stay on the list until closed.
