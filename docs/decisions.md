# Decision log (ADRs)

Every architectural choice the design conversation produced, with the alternative that was
rejected and the reason. Read the matching doc for the full specification.

**Statuses:** `locked` = implement as written; `locked-contingent` = decided, but depends on a
spike result or an open question; `proposed` = our recommendation, needs sign-off.

| ADR | Title | Status |
|-----|-------|--------|
| [001](#adr-001) | Two-phase project: engine runtime, then the game | locked |
| [002](#adr-002) | Rust for the engine core | locked |
| [003](#adr-003) | WebGPU + WGSL as the rendering API and shader language | locked |
| [004](#adr-004) | Single-player first; multiplayer consensus deferred | locked |
| [005](#adr-005) | Stateless view-time evaluation instead of a hash chain | locked-contingent |
| [006](#adr-006) | Localized octree Merkle roots, not one global world root | locked |
| [007](#adr-007) | Sleep/epoch compaction of the delta mask (LSM analogy) | locked-contingent |
| [008](#adr-008) | Slab allocator with bump-pointer reset for `O(1)` edits | locked |
| [009](#adr-009) | Tiered slabs (L1 VRAM / L2 RAM) with LRU + background micro-checkpoints | locked |
| [010](#adr-010) | Circuits as compiled graph topologies evaluated analytically | locked-contingent |
| [011](#adr-011) | Assets are code: SDF raymarching, no meshes, no textures | locked |
| [012](#adr-012) | Shape/material node grammar interpreted by one master shader | locked |
| [013](#adr-013) | WASM (Component Model + WIT) as the modding and logic ABI | locked |
| [014](#adr-014) | Universal authoring formats compiled on save; RON as canonical | locked |
| [015](#adr-015) | FlatBuffers + bitpacked `u64` deltas for runtime/ledger data | locked |
| [016](#adr-016) | 64-bit Hierarchical Double-Origin Coordinate System (HDCS) | locked |
| [017](#adr-017) | Determinism boundary: simulation in Rust/WASM, GPU never authoritative | **locked — highest priority** |
| [018](#adr-018) | Physics by analytic SDF gradient queries | locked |
| [019](#adr-019) | Animation by domain deformation / time-warping | locked |
| [020](#adr-020) | Audio occlusion by SDF sphere tracing | locked |
| [021](#adr-021) | Multiplayer transmits signed intent streams, not state | locked |
| [022](#adr-022) | WebRTC datachannels / QUIC with split reliable + unreliable channels | locked |
| [023](#adr-023) | Desync detection by 32-byte epoch root exchange + bisection repair | locked-contingent |
| [024](#adr-024) | Spatial BVH mapped to slabs for SDF culling | locked |
| [025](#adr-025) | Contract-first agent swarm with architect + CI-gatekeeper roles | locked |
| [026](#adr-026) | Reuse an existing GPU runtime; do not hand-write low-level plumbing | **proposed** |
| [027](#adr-027) | Epoch-scoped transaction pool for multiplayer | proposed |
| [028](#adr-028) | BLAKE3-256 as the ledger digest | proposed |

---

## ADR-001 — Two-phase project: engine runtime, then the game

**Context.** The design is a novel engine *and* a game. Mixing them tangles simulation,
rendering and ledger logic with inventory slots, mob AI and craft recipes.

**Decision.** Phase 1 builds **The Voxel-Ledger Engine**: a domain-agnostic runtime that knows
state storage, implicit-geometry rendering, WASM signal routing and time-based math, and knows
nothing about pigs, redstone or wood. Phase 2 (**Project Genesis**) treats Phase 1 as a
third-party dependency and authors the game purely through the public JSON/RON schemas, WGSL
shaders and WASM bindings. **The official game is Mod #1.**

**Consequences.** Phase 1 is unit-testable without any game content — a wrong-looking pig is
provably a content bug, not a renderer bug. External modders get exactly the API we used, so
"modder-friendly" is not a later feature. Cost: we must resist adding game-specific
conveniences into the engine.

**Rejected.** One codebase with game rules compiled in (untestable boundaries, no mod story).
A "generic engine" with a separate private mod API (drifts from reality; modders get the worse
API).

**Re-open if** Phase 2 needs an engine feature that is game-specific in disguise — promote it
to a documented engine primitive or keep it in content.

---

## ADR-002 — Rust for the engine core

**Context.** Minecraft picked Java because Notch knew it and it prototyped fast; Bedrock was
later rewritten in C++ for performance. Rust would have avoided both traps: C++-class speed
with compiler-enforced memory safety and fearless concurrency.

**Decision.** Engine core in Rust. Voxel/SDF math, the ledger, the WASM host, the graph
compiler — all Rust. Reference Rust→WASM modules ship as the Phase 2 logic examples.

**Why it matters specifically here.** The engine is CPU-bound (chunk meshing, entity AI,
circuit solving, hashing) and wants all 8+ cores. Rust's compile-time rejection of data races
is what makes "offload redstone processing, mob AI and chunk meshing across all cores"
achievable without a class of heisenbugs.

**Rejected.** C++ (manual memory management; the memory-safety bug classes the transcript
cites as >70% of high-severity vulns in large C++ codebases). Java (GC pauses — exactly the
problem Minecraft suffered). Go (GC again, weaker GPU/`unsafe` ergonomics).

**Note the asymmetry we accept:** native Rust compiles to tight machine code with no
reflection — a *host* Rust engine is fine, but mod-authored *logic* goes through WASM (ADR-013)
so it stays portable.

**Re-open if** a spike shows the WebGPU Rust binding layer can't express what the raymarcher
needs (then C++ + a thin Rust shell is the fallback, not a rewrite).

---

## ADR-003 — WebGPU + WGSL as the rendering API and shader language

**Context.** The transcript proposed GLSL/HLSL with Vulkan; the acceleration section argued
for WGSL/WebGPU because shaders can then be validated headlessly.

**Decision.** WebGPU with WGSL compute shaders. Native builds use `wgpu`; the browser build
uses the same WGSL. Vulkan is reached through `wgpu`'s backend, never hand-written.

**Why.** (a) Agents can write a shader, run it in a **headless WebGPU** test, capture the
buffer/depth output, and self-correct in a closed loop — no human watching for driver hangs.
(b) One shader language for desktop and browser, which is also the zero-friction distribution
story (the Java-applet lesson from doc 00). (c) Storage buffers take direct `memcpy`/DMA from
slabs.

**Rejected.** Hand-written Vulkan (2–3 months of low-level debugging, worst possible LLM
feedback loop). GLSL/HLSL native toolchains (complex build, no headless validation).

**Open (→ [OQ-26](13-risks-open-questions.md)).** WGSL's f32 transcendentals are only
approximations and differ per adapter. That is acceptable *because of ADR-017*, and it is the
reason ADR-017 is mandatory.

**Re-open if** `wgpu` overhead is measurable against budget on target hardware (spike S9).

---

## ADR-004 — Single-player first; multiplayer consensus deferred

**Context.** Distributed consensus (simultaneous epoch commits, conflict resolution, P2P Merkle
validation) is the largest source of engineering risk, and the architecture is already
deterministic.

**Decision.** Build the local ledger, delta mask and sleep compaction single-player first.
Multiplayer is *specified* now ([doc 10](10-multiplayer.md)) but not *built* until Phase 1.5.
Because state is deterministic Merkle-anchored data, adding multiplayer later is "broadcast
state proofs over WebSockets/WebRTC", not a rewrite.

**Consequence that must not be forgotten.** The compaction trigger is "the player sleeps" — a
single-player concept. Multiplayer needs a different commit boundary
([SPEC-08](13-risks-open-questions.md)). Any Phase 1 code must therefore keep epoch commit as a
**policy callback**, never inline it in the sleep action.

**Rejected.** Building co-op into the prototype (doubles the debug surface before the core loop
is proven).

**Re-open if** the design turns out to depend on other agents observing the world in real time.

---

## ADR-005 — Stateless view-time evaluation instead of a hash chain

**Context.** The proposal on the table was `TickRoot_t = Hash(TickRoot_{t-1} ⊕ MerkleRoot(chunks_t))`
— a blockchain-style chain. Two independent objections emerged in the conversation:
a chain forces `O(N)` step-by-step replay to reach tick *N* (no shortcut exists through a
one-way hash), and a single global Merkle root must be rebuilt whenever any chunk changes,
which serializes everything and destroys horizontal parallelism.

The user's answer: *"the merkle tick is isolated, blocks on screen are generated at view-time
off the current tick. The history of that block is irrelevant."*

**Decision.** Treat the world as a pure function:

```
BlockState = f(Position, CurrentTick) ⊕ PlayerDelta
```

Time is a coordinate (`f(x,y,z,t)`), never an accumulation. A chunk unseen for 1,000,000 ticks
costs one evaluation at the current tick, not 1,000,000 transitions. Consequences are resolved
by **closed-form / jump functions** (spike S8) — not by replay — with the ledger keeping a
*record* of transactions (which is history as data, not history as computation).

**What this abandons.** True iterative cellular automata as the base layer: a cell at *t*
depends on *t-1*, so unbounded CA can't jump. Where CA is wanted, it is bounded
(finite generations, region-local) or replaced by jumpable forms ([SPEC-05](13-risks-open-questions.md)).

**Precedent cited.** No Man's Sky / Boundless: whole universes as stateless seed functions,
nothing stored until a player digs a hole.

**Re-open if** a required behavior demonstrably cannot be expressed as `f(pos, t)` plus event
signals — that is the "macro-causality" risk ([OQ-01](13-risks-open-questions.md)).

---

## ADR-006 — Localized octree Merkle roots, not one global world root

**Context.** A single global root over an infinite world means every edit re-hashes
`O(log N)` nodes up to the world root, every tick, forever — and distant regions can never be
verified independently.

**Decision.** Hierarchical, region-localized roots. Spatial octree/quadtree: each region owns
a root derived from its children's hashes; a change in region A touches only A's branch
(`O(log N)` local update). The global structure is an **epoch anchor** — a compact root plus
active event tags — that the server broadcasts (32 bytes) rather than a full state tree.

**Verification.** A client entering a region receives a **Merkle inclusion proof** for that
region at the current epoch: verify without downloading or computing the rest of the world.

**Rejected.** One global root per tick (the "scalability wall" in the transcript); per-chunk
unhashed storage (loses verifiability).

**Re-open if** proof sizes dominate bandwidth on large edits (then batch proofs per epoch).

---

## ADR-007 — Sleep/epoch compaction of the delta mask (LSM analogy)

**Context.** If the base world is regenerable, then *everything the player touched* lives in the
delta mask — "the Delta Mask becomes the entire memory", which grows without bound.

**Decision.** The sleep/rest event is the **compaction phase**, modelled on LSM-trees
(RocksDB/LevelDB/Cassandra):

```
S_new_genesis = S_old_genesis ⊕ M_local        # bake
new Merkle root computed over the combined baseline
M_local = ∅                                    # clear overlay
procedural evaluation now anchors on S_new_genesis, not the original seed
```

Memory stays flat: consumption scales with edits *between* checkpoints, never with world size
or elapsed time.

**Why sleep specifically.** It's an existing, meaningful pause in the game loop, it's a natural
sync boundary in single- and multi-player, and it gives deterministic catch-up (compute Δt
steps of macro rules), batch compression (thousands of micro-edits → one root), and "inevitable"
far consequences (dried lake bed, dead vegetation, migrating fauna) encoded in the new root
before the player travels there.

**Rejected.** Compacting every N minutes (arbitrary, mid-session pop-in); never compacting
(mask growth); keeping the original seed as the only anchor (edits never become base truth).

**Contingent on** [SPEC-08](13-risks-open-questions.md) — the multiplayer commit boundary — and
on [OQ-23](13-risks-open-questions.md) (what "base state" physically lives on disk). The
mechanism must be implemented as an epoch-commit policy, per ADR-004.

---

## ADR-008 — Slab allocator with bump-pointer reset for `O(1)` edits

**Context.** A block break must appear on frame 0 at 60–144 Hz input rates. `malloc`/`free` and
dynamic vectors add heap overhead and OS memory-lock contention exactly where the game can't
afford it.

**Decision.** Pre-allocate large contiguous, fixed-size slabs (64 KB / 1 MB) keyed by
`RegionKey`. A voxel edit is `*slab_ptr = packed_u64` — a pointer-offset store, nanoseconds.
Dirty slabs `memcpy`/DMA straight into storage buffers. At epoch commit, after hashing and
baking, the allocator **resets its bump pointer to zero**: deallocation is `O(1)` regardless of
whether 10 or 10,000,000 blocks changed.

**Lifecycle.**

| Stage | Memory action | Latency |
|-------|---------------|---------|
| Frame 0 delta | bitpacked write to active slab slot | <1 ns |
| Render/physics | slab pointer read directly | 0 ms (no decode) |
| Epoch compaction | sequential Merkle hash pass over contiguous arrays | sub-ms batch |
| Reset | bump pointer → 0, slab recycled | `O(1)` |

**Rejected.** Per-edit heap allocation; copy-on-write chunk arrays (reallocates whole chunks);
`Vec` push with reallocation.

**Re-open if** fragmentation across region boundaries bites (slabs are per-region, so this is
mostly bounded).

---

## ADR-009 — Tiered slabs with LRU eviction and background micro-checkpoints

**Context.** VRAM is a hard limit. A player who edits for miles without sleeping has nowhere to
put the deltas, and ADR-007's reset never fires.

**Decision.** Three tiers: **L1 hot slabs** pinned in VRAM (the region around the player) →
**L2 cold slabs** in CPU RAM as the player moves away → when L2 fills without a sleep, an
asynchronous **background micro-checkpoint** commits the oldest slabs to disk and frees them,
without forcing a gameplay sleep.

**Rejected.** Forcing a sleep to free memory (gameplay-hostile); unbounded VRAM growth
(crashes); paging deltas back in from disk on demand inside a frame (stutter).

**Re-open if** micro-checkpoint storms appear during fast creative-mode terraforming.

---

## ADR-010 — Circuits as compiled graph topologies evaluated analytically

**Context.** Minecraft redstone is a block-by-block cellular automaton: every tick, source tells
wire tells repeater. It needs loaded chunks, lockstep execution, and scales `O(N)` with wire
length — the exact bottleneck this engine exists to avoid.

**Decision.** Wire networks are **compiled into circuit graph objects** at placement or at
compaction (BFS over connected components):

```
Circuit = { Inputs: [I₁…Iₙ], Outputs: [O₁…Oₘ], TransferFunction: f(I₁…Iₙ, t) }
```

Rendered dust is cosmetic. Nodes are sources/switches/gates/actuators; edges are zero-latency
paths. Clocks become pure functions of time — `state(t) = ⌊(t + φ)·ω⌋ mod 2` — so a clock
untouched for 1,000,000 ticks resolves in `O(1)`. A machine's long-term output is an integral
of its production rate over `[t_start, t_sleep]`, so factories keep producing while unloaded.

| Metric | Minecraft (CA) | This engine (graph) |
|--------|----------------|---------------------|
| Execution | block-by-block spatial propagation | instant boolean / signal graph solve |
| Long wire latency | 1 tick per 15 blocks | 0, any length |
| Unloaded | frozen without chunk-loaders | evaluates analytically as `f(t)` |
| CPU | `O(N)` wire blocks | `O(K)` active gates |

**Explicitly accepted loss.** *Traditional* Minecraft redstone will not run here: mid-computation
observation, sub-tick timing tricks, and 40,000-block wire runs won't behave identically. This
is a different electricity model with the same expressive intent.

**Contingent on** [OQ-02](13-risks-open-questions.md): combinational-only graphs cannot express
stateful sequential logic (T-flip-flops, counters, registers, feedback loops). Latching nodes
with explicit state stored in the ledger are the proposed answer and must be specced before
doc 06 is implementable.

**Rejected.** Keeping block-CI A redstone (breaks lazy evaluation); "no electricity feature".

---

## ADR-011 — Assets are code: SDF raymarching, no meshes, no textures

**Context.** "I'm not much of an artist and I want the entire game to be LLM developed, so 3D
sculpting is out of the question… a pig needs to be coded, not sculpted."

**Decision.** Geometry = Signed Distance Functions; surfaces = implicit, found by GPU voxel
raymarching. Color = a function of surface position and normal. **No `.obj`, no `.fbx`, no
PNG.** A pig is a `SmoothUnion` of a rounded box, a sphere, a cylinder snout and four mirrored
leg cylinders (doc 04 has the reference code), with nostrils and eyes painted by proximity
masks, not texture lookups.

**Why this is the right call, not just the artist-free call.** (a) Perfect decoupling: the
ledger only says "voxel at (X,Y,Z) = PIG"; the renderer picks the ID and runs the matching
function locally. (b) Infinite detail — SDFs never pixelate or show polygon edges; detail is
bounded only by float precision. (c) LLM fit — models are dramatically better at 500 lines of
shader than at 2 MB of valid binary OBJ/PNG. (d) Evolvability — "pigs develop brown spots over
5 generations" is a shader mutation committed to the ledger, auto-applied at lazy-evaluation
time. (e) Mod economics — a dragon ships as a few KB of text, not 50 MB of binaries.

**Rejected.** Mesh + PNG pipeline (needs artists or an LLM→binary path that doesn't work);
hybrid SDF-terrain/mesh-props (two asset pipelines and two physics systems for no gain).

**Cost we accept.** Raymarching is fill-rate heavy: every pixel marches and evaluates
functions. This is why ADR-024 (BVH) and the frame budget in doc 04 exist — and why the "60 FPS
with 500 entities" claim is a target to verify, not a fact (spike S1).

**Re-open if** frame budgets can't be met on mid-range GPUs even after culling (then
pre-baked SDF→mesh for static props, kept out of the authoring contract).

---

## ADR-012 — Shape/material node grammar interpreted by one master shader

**Context.** Hand-writing a shader per entity doesn't scale to Minecraft-level content diversity,
even for agents.

**Decision.** Define a standard node grammar for shapes and textures
(`Combine(Sphere(r), Cylinder(h), Noise(freq))`, with operators like `SmoothUnion`,
`RoundedBox`, `VoronoiScales`). One master WGSL shader interprets these trees; agents emit small
declarative files instead of raw shader source. "Generate 50 distinct procedural trees" becomes
50 config files, testable in parallel.

**Rejected.** One hand-written GLSL file per entity (content scaling bottleneck); a visual
node-graph editor (an art tool in disguise — see ADR-011).

**Consequence.** The grammar is a public API surface — it lives in `contracts/`, versioned
(ADR-025).

---

## ADR-013 — WASM (Component Model + WIT) as the modding and logic ABI

**Context.** Modding historically means decompiling bytecode or injecting into native code. We
want mod-proof isolation, portability and determinism — and Minecraft's own history shows
accidental moddability (Java bytecode → MCP → Forge/Fabric) was the single biggest growth lever
(doc 00).

**Decision.** All game logic and mod logic runs as sandboxed **WASM** behind WIT interfaces.
Modders use any toolchain that emits `wasm32` — Rust, C/C++, C# (NativeAOT), Zig, Swift, Go
(TinyGo), Python (componentize-py), TypeScript (AssemblyScript/Javy) — because the host only
sees the WIT contract, never source.

**Why WASM specifically.**
- **Determinism.** Strict IEEE-754 semantics and isolated linear memory mean the same
  transaction yields the same bits on x86 Windows, ARM macOS, Linux and in-browser. A desync
  would break the hash chain, so this is load-bearing.
- **Safety.** A null deref or panic in a mod traps inside the module; the host catches the
  trap, unloads, resets its buffer. No filesystem access, no arbitrary code execution, no GPU
  pipeline crash, no corrupted Merkle root.
- **Speed.** ~80–90% of native.
- **Zero marshalling.** WIT handles structs/strings/buffers across the boundary.

**Rejected.** Lua/JS embedding (single language, GC nondeterminism); native DLL plugins (unsafe,
platform-locked); Java-style bytecode injection (we have no bytecode to inject); C++ header
mods (breaks portability).

**Cost.** WASM's linear-memory model pushes us to 32-bit addressing (`memory64` is still new),
which is exactly why coordinates are 2×i64, not 3×i64 ([SPEC-03](13-risks-open-questions.md)).

**Re-open if** a needed host capability can't cross the WIT boundary cheaply — solve it with a
narrow host function, don't abandon the sandbox.

---

## ADR-014 — Universal authoring formats compiled on save; RON as canonical

**Context.** JSON is the lingua franca and LLMs emit it reliably, but runtime string parsing
wastes CPU, allocates heap and spikes GC. Binary-only formats (FlatBuffers, bitpacked u64) are
fast but miserable to hand-edit — "if modders were forced to write raw binary by hand, it would
be less modder-friendly".

**Decision.** Separate **authoring format** from **runtime format**. Modders and agents write
whatever they like — JSON, KDL, RON, TOML, or plain code — into one canonical schema. The asset
compiler (file watcher in dev; packer at build) parses **once**, validates against the schema,
and emits zero-copy binaries; the engine runtime never sees text.

```
modder edits pig.ron ──watcher: validate + compile (<5 ms)──► pig.bin (FlatBuffer) ──memcpy──► GPUBuffer
```

**Why RON as the canonical in-repo format.** Trailing commas, comments, 1:1 mapping to Rust
structs (Bevy ecosystem convention), and an existing Rust parser. JSON stays a first-class
*accepted input* because LLMs and web developers default to it; KDL/TOML ingestion is on the
backlog.

**Schema validation.** One canonical schema definition drives VS Code autocomplete/linting for
modders and acts as the compile-time gate that replaces runtime validation (JSON has no
comments and no ordering guarantees, which is why it isn't canonical).

**Rejected.** JSON at runtime (parsing cost, GC spikes, no schema enforcement); binary-only
authoring (unreadable, un-diffable); one blessed text format only (needless modder lock-in).

---

## ADR-015 — FlatBuffers + bitpacked `u64` deltas for runtime/ledger data

**Context.** Runtime containers must be zero-copy and SIMD-hashable.

**Decision.**

| Domain | Format | Why |
|--------|--------|-----|
| Asset/SDF definitions, scene graphs, network payloads | **FlatBuffers** (with FlexBuffers for dynamic cases) | zero-copy access at byte offsets, direct `memcpy` into storage buffers, `.fbs` generates Rust/WASM/Python/C++ bindings |
| Entity/material/biome configs | static data exported from WASM (`#[repr(C)]` arrays) | `O(1)` pointer read from linear memory; the compiler enforces types, so no runtime schema errors |
| Ledger delta records | bitpacked `u64` | hashes computed on raw integers in SIMD registers, no string conversion, minimal storage |
| Wire deltas | `{SlabID, u16 offset, u64 value, VectorClock}` | no global coordinate travels on the wire |

**Rejected.** Cap'n Proto (better mutability than we need; FlatBuffers' tooling and GPU-upload
story win). BSON (needs a parse step). Custom bit layouts everywhere (unmaintainable).
Runtime JSON (ADR-014).

**Contingent on** [SPEC-01](13-risks-open-questions.md) — the exact bit layout and where the
region identifier lives.

---

## ADR-016 — 64-bit Hierarchical Double-Origin Coordinate System (HDCS)

**Context.** "We're missing a coordinate system… that should be the first part of the engine."
Without an absolute reference nothing can define up, down, or a location — and bare `f32`
coordinates fail at distance (the Far Lands: jitter, tearing, and *broken Merkle hashes*).

**Decision.** Full specification in [doc 02](02-coordinate-system.md). Summary:

- **Global space:** discrete integers. `GlobalVoxelPos { x: i64, y: i64, z: i64 }` split into
  `RegionKey = (div_euclid 4096)` plus `LocalOffset = (rem_euclid 4096)`. No float rounding can
  exist; two players at `(50000000, 64, -12000000)` compute bit-identical hashes.
- **Local space:** `f32`, camera-relative, re-anchored every frame
  (`P_local = P_global − C_global`). The camera is always at the origin inside the shader, so
  sub-millimeter precision holds anywhere in an infinite universe.
- **Orientation is contractual:** `+Y` up, `-Y` gravity/fluid flow, `+Z` north, `+X` east,
  `(0,0,0)` = World Genesis Core. Right-handed, `i32` voxel units.
- Every subsystem keys off this: slabs by `RegionKey`, Merkle nodes by octree keys derived from
  `RegionKey`, shaders by the camera anchor uniform, circuits by relative `(Δx, Δy, Δz)` edges,
  physics gradients relative to `-Y`.

**Status caveat.** The source's own numbers conflict (12-bit vs `u16` offsets; 48-bit packed
delta vs `i64` coordinates; Y-axis range). Those are SPEC-01/03 — resolve them, *then* freeze
`contracts/coords.rs` as the first artifact of Phase 1 (the user asked for exactly that, and it
is the one part of the design with nothing downstream of it that can be built first).

**Rejected.** Single 64-bit float world coordinates (precision drift); f32 everywhere (breaks
hashing past ~10,000 units); 3×`i64` per delta (192 bits and a WASM ABI liability).

---

## ADR-017 — Determinism boundary: simulation in Rust/WASM, GPU never authoritative

**Status: locked, highest priority. It rescues ADR-005/006/023 from a fatal inconsistency.**

**Context.** The sketch says the GPU compute shader evaluates block state from the Merkle root
("every client independently evaluates its own screen view locally"). But WGSL `f32` transcendentals
and blend modes are *approximations that may differ per adapter* — so two GPUs evaluating
"the world" would disagree, and ADR-023's 32-byte epoch comparison would report a desync
everywhere. The ledger's verification promise dies at the shader boundary.

**Decision.**

1. All **canonical** state — voxel material IDs, macro fields, event tags, circuit outputs,
   physics resolution, hashes — is computed by deterministic Rust (host) and/or WASM (mods),
   using integers (or fixed-point, or the explicit float ops that `wiggle` allows) only.
2. The GPU is a **projector, not an authority**. It may read state produced by (1); it may
   *never* be the sole source of a value that the simulation or another peer depends on.
3. Shader math may be approximate/adapter-dependent only for visuals that can't desync:
   lighting, texture noise, interpolation, deformation for display.
4. Consequence for performance arguments: "the server broadcasts a 32-byte root and clients
   compute the rest" applies to *presentation*, not to state generation. State generation for a
   region happens on a deterministic host and is committed as slab bytes.
5. This is exactly what WASM determinism (ADR-013) buys us: `WASM_Execute(State_t, Input)`
   identical on all hardware.

**Rejected.** GPU-authoritative world state (elegant, unsound: unverifiable multiplayer,
unreproducible saves). Rejected as *canonical* path only — the raymarcher still does the visual
evaluation.

**Verification.** The Bitwise Determinism Suite (spike S3 / gate P1.5) is the acceptance test
for this ADR: same input on N2/N3060/Linux-Mesa/M1-Browser must produce identical bytes.

---

## ADR-018 — Physics by analytic SDF gradient queries

**Context.** There are no vertex buffers or polygon meshes for PhysX/Havok to raycast against.

**Decision.** Query the SDF *code* directly on CPU/WASM. Distance to surface is the function's
value; the surface normal is its gradient, by central differences:

```
N = ∇SDF(p) ≈ [ (SDF(p+εx) − SDF(p−εx)) / 2ε, … ]
```

Collision, penetration depth and contact normals come from evaluation — no mesh generation, no
VRAM readback. Gravity direction is contractual (`-Y`, ADR-016).

**Rejected.** Hidden mesh colliders generated from SDFs at commit time (reintroduces the mesh
pipeline and its memory cost); GPU readback per physics step (latency + violates ADR-017).

**Cost.** Gradient evaluation per contact query is 6 SDF evaluations — pathfinding for many
entities gets expensive. Mitigation: per-entity BVH spheres, coarse-navmesh-free
grid steering, and LOD physics ([OQ-16](13-risks-open-questions.md)).

---

## ADR-019 — Animation by domain deformation / time-warping

**Context.** Entities are functions; there is no skeleton to bind.

**Decision.** Animate by deforming the space the SDF is evaluated in. A walking pig evaluates
its leg SDFs at `z_leg = z + sin(ωt)`; breathing applies a periodic scale to the body's bounding
radius. A WASM logic module feeds pose parameters (velocity, limb phase) into uniform buffers
and the shader warps space.

**Rejected.** Bone rigs and skinned meshes (contradicts ADR-011); baked keyframe geometry
(same).

---

## ADR-020 — Audio occlusion by SDF sphere tracing

**Context.** Sound occlusion normally uses pre-baked zones or mesh raycasts; there are no meshes.

**Decision.** The same SDF functions used for rendering are sphere-traced from source to ear.
Negative/High-density distance along the path ⇒ low-pass filter the sound in real time. Caves
reverb, muffled explosions behind mountains, no baked data.

**Rejected.** Pre-baked sound zones (needs authored volumes); mesh raycasts (no meshes).

**Status caveat.** Plausible but unmeasured — audio tracing per voice per frame is real CPU
cost. Budget in [OQ-25](13-risks-open-questions.md).

---

## ADR-021 — Multiplayer transmits signed intent streams, not state

**Context.** Streaming block arrays or mesh transforms doesn't fit a world that is a function.
Every client already runs identical deterministic WASM logic and identical SDF code.

**Decision.** Broadcast intent:

```
ActionTx = ⟨SequenceID, EntityID, TickEpoch, ActionPayload, Signature⟩
```

Payload types: **slab delta mutate** (`RegionKey`, `u16 offset`, `u64 packed`) for breaks/places;
**entity kinematics** (position, velocity, quaternion) for movement; **WASM signal trigger**
(`ModuleID`, `FunctionIndex`, bytes) for switches, spells, inventories. Remote peers apply the
same mutation at the same spatial coordinate in their own slab.

**Rejected.** Authoritative server streaming chunk bytes (bandwidth, and it discards the whole
point of deterministic state); sending raw deltas without a `TickEpoch` (impossible to validate
or rollback).

---

## ADR-022 — WebRTC datachannels / QUIC with split channels

**Decision.** Two channel classes over WebRTC (browser/mobile) or QUIC (native):
**unreliable/unordered** for kinematics (a dropped frame is superseded by the next) and
**reliable/ordered** for slab mutations and WASM signals (every break and circuit trigger must
arrive, in sequence).

**Topology is pluggable over the same `ActionTx` stream:** authoritative dedicated server
(processes, validates against WASM rules, holds the canonical Merkle root) or P2P mesh with
CRDT/vector-clock consensus for co-op friend groups.

---

## ADR-023 — Desync detection by 32-byte epoch root exchange + bisection repair

**Context.** How do you know client A isn't cheating or subtly out of sync without a server
verifying every voxel?

**Decision.** At each epoch commit, every client computes
`EpochHash = MerkleHash(BaseGenesisRoot ⊕ CommittedSlabs)` and peers exchange the 32-byte roots.
Matching roots ⇒ worlds bitwise identical across voxels, WASM memory and circuit states.
Mismatch ⇒ binary-search the Merkle branches to pinpoint the affected 4096³ region and resend
only that region's slab bytes. Conflict ordering within a region uses vector clocks (per-voxel
vector clocks are explicitly out of the question — too expensive).

**Contingent on** ADR-017 (without a deterministic canonical path this mechanism fires
constantly) and [SPEC-08](13-risks-open-questions.md) (what counts as "the" epoch in
multiplayer).

**Rejected.** Per-action server validation (doesn't scale); checksum of the whole world per
frame (bandwidth); trusting clients (cheating).

---

## ADR-024 — Spatial BVH mapped to slabs for SDF culling

**Context.** Raymarching evaluates a function per step per pixel. With 500 entities in a
landscape, naive evaluation drops the framerate to single digits.

**Decision.** Maintain a lightweight bounding box/sphere hierarchy (BVH) over active SDFs, in
both CPU and GPU form, keyed alongside slabs. A ray executes an entity's detailed WGSL SDF only
after intersecting its coarse bounding volume — `O(log N)` skips.

**Rejected.** Evaluating all SDFs (unviable); converting SDFs to meshes for a raster BVH
(ADR-011).

---

## ADR-025 — Contract-first agent swarm with architect + CI-gatekeeper roles

**Context.** Ten uncoordinated agents writing concurrently produce merge conflicts, API
mismatches, broken interfaces and non-terminating debug loops. "LLMs don't fail at writing
code; they fail at coordinating."

**Decision.** `contracts/` is a **read-only** directory holding every cross-module type,
trait, WIT world, `.fbs` schema, WGSL signature and error enum. Agent 1 (Architect) writes
only contracts and dependency DAG, never implementation. Agents 2–9 implement against frozen
contracts. Agent 10 (CI/CD) runs `cargo test`, `cargo clippy -D warnings`, `cargo fmt --check`,
`cargo deny`, headless WGSL render-diff tests and WASM ABI checks, and rejects breaking merges
back to the responsible agent. No human merges interfaces; humans break ties and approve
contract changes via ADRs.

**Rejected.** Free-for-all file ownership; a human reviewing every line; shared mutable
interfaces.

---

## ADR-026 — Reuse an existing GPU runtime; do not hand-write low-level plumbing

**Status: proposed — the design was internally inconsistent and needs sign-off.**

**Context.** The acceleration analysis says writing a custom Vulkan/WebGPU raymarcher,
allocator and compute pipeline from scratch costs 2–3 months of the worst kind of LLM
debugging (silent driver hangs, no stack traces) and recommends building on an existing
engine: **Luanti/Minetest, wgpu/Bevy, or Raylib/WGSL**, saving 6–8 weeks. But the transcript
also says "your rendering pipeline (ideally Vulkan or WebGPU)", and doc 04's raymarcher is a
custom WGSL compute pipeline. An existing voxel engine (Luanti) has a *mesh* renderer, which
ADR-011 rejects.

**Decision (proposed).** Adopt **`wgpu` as the GPU layer** (it *is* the WebGPU-idiomatic Rust
runtime, satisfying both statements) and use Bevy *only* if its ECS/windowing/input plumbing
pays for itself. Reject Luanti/Minetest as a base — its mesh+node model conflicts with
ADR-011/ADR-005. Write the raymarcher ourselves as a WGSL compute pipeline on top of `wgpu`,
which is roughly 3 weeks, not 3 months.

**Rejected.** Hand-writing Vulkan; adopting a mesh-based voxel engine wholesale.

**Needs.** Spike S1/S9 confirm. If a third-party SDF/raymarcher runtime exists that meets the
budget, prefer it.

---

## ADR-027 — Epoch-scoped transaction pool for multiplayer

**Status: proposed.**

**Context.** "The simulation ledger doesn't force surrounding miles to recalculate; it just
registers a mutation event." The unresolved question is what an *epoch* means once several
players sleep at different moments ([SPEC-08](13-risks-open-questions.md)).

**Decision (proposed).** One global epoch advances when a configurable quorum of online players
is asleep (default 100% for co-op, 50% for servers, or a wall-clock fallback for hardcore
servers). Actions between epochs accumulate in a **transaction pool** (mempool) ordered by
vector clock. Epoch commitment applies the pool against the macro rules for Δt, bakes deltas,
and emits one new root.

**Rejected.** Per-player epochs (worlds diverge — the guarantee dies); immediate global
commit per action (the cost this architecture exists to avoid).

---

## ADR-028 — BLAKE3-256 as the ledger digest

**Status: proposed.**

**Context.** The source says "cryptographic hash (SHA-256 style)" without choosing one. The
hash is on the hot path of both compaction and verification, and the design's whole "sub-ms
batch hash pass" claim depends on that choice.

**Decision (proposed).** **BLAKE3-256** (32-byte digests): orders of magnitude faster than
SHA-256, SIMD and intrinsically parallel (perfect for hashing contiguous slab arrays and Merkle
levels), and widely implemented for Rust and WASM.

**Constraints.** Digest length fixed at 32 bytes; domain-separated per node type (leaf /
branch / event tag) to prevent second-preimage confusion between voxel deltas and macro events;
documented test vectors so all languages agree.

**Rejected.** SHA-256 (needlessly slow here, no benefit — nothing in the design needs SHA
compliance). xxHash/SeaHash (**not** collision-resistant; a world-state root must be). Poseidon
-type SNARK hashes (only if we later want zero-knowledge client proofs — revisit then).
