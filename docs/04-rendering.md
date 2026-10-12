# 04 — Rendering

**Status:** design locked; feasibility unproven (spikes S1, S9).
**Depends on:** docs 02, 03. **Implements:** ADR-011, 012, 024.

**Read the rule first:** the renderer is a **projector**. It displays state the simulation
already decided. See §Determinism boundary below — this is ADR-017, and violating it here
silently breaks multiplayer verification and save reproducibility.

## Pipeline

```
per frame:
  1. camera anchor C_global = floor(player pos in voxel units)      (doc 02)
  2. upload dirty slabs (memcpy/DMA) + epoch root into storage buffers   (doc 03)
  3. rebuild/refit BVH for in-range entities                       (ADR-024)
  4. compute pass: for each pixel, raymarch the scene SDF          (ADR-011)
  5. shading: material functions of (p, n, t)                      (ADR-011)
  6. present
```

No vertex buffers, no draw-index calls, no texture assets — the entire visible world comes out
of one fullscreen compute pass over functions.

## The march

```wgsl
var d = 0.0; var t = 0.0; var p = ro + rd * t;
loop {
  let s = worldSDF(p, epoch_root, delta_mask, t_now);   // coarse region lookup, then entity SDFs
  if (abs(s) < EPS) { break; }                          // hit
  t += max(s, MIN_STEP);                                 // sphere tracing
  if (t > FAR) { break; }                                // miss → sky
  p = ro + rd * t;
}
```

Sphere tracing is what makes raymarching affordable: the SDF's value is a guaranteed lower bound
on the distance to any surface, so steps are large in empty space and only shrink near geometry.
Cost is therefore governed by **surface complexity along the ray**, not by pixels alone — the
reason the BVH (ADR-024) and the step/epsilon budgets below are load-bearing rather than
tuning knobs.

Normals come from the gradient by central differences (six evaluations — the same math the
physics query uses, [doc 07](07-physics-audio-animation.md)).

## Master shader + node grammar (ADR-012)

One interpreter shader walks a shape tree; content is data, not source:

```
Sphere(r, offset) · RoundedBox(size, r) · Cylinder(r, h, axis) · Torus · Plane
SmoothUnion(k) · Union(min) · Subtract · Intersect · Repeat(grid) · Mirror(axis) · Twist/ Bend
material: { base_color, noise(Voronoi|Value|Perlin|domain-warped), roughness, emissive, index_ior }
```

The tree lives in a FlatBuffer (doc 09) so the interpreter reads it zero-copy. Hand-written
WGSL per entity remains possible for specialists, but it is the exception and must pass the same
frame budget.

**Why one interpreter and not generated code** (rejected alternative): LLMs produce *valid*
shaders less reliably than valid data, and a per-entity source file can't be budget-checked
uniformly. A tree can be measured, culled, and depth-capped automatically.

## Reference: coding a pig (from the design conversation)

Shape as distance, then color as a function of position and normal. Illustrative WGSL-flavoured
pseudocode — the real thing is one tree instance, not a hand-written shader:

```wgsl
fn sdf_pig(p: vec3<f32>) -> f32 {
  var d = sd_rounded_box(p, vec3(0.5, 0.4, 0.3), 0.1);           // body
  d = op_smooth_union(d, sd_sphere(p - vec3(0.0, 0.3, -0.4), 0.25), 0.1);  // head
  d = op_smooth_union(d, sd_cylinder(p - vec3(0.0, 0.2, -0.6), 0.1, 0.1), 0.05); // snout
  let leg   = sd_cylinder(vec3(abs(p.x) - 0.25, p.y + 0.3, p.z - 0.1), 0.08, 0.2);
  let leg_b = sd_cylinder(vec3(abs(p.x) - 0.25, p.y + 0.3, p.z + 0.3), 0.08, 0.2);
  return min(min(d, leg), leg_b);
}

fn color_pig(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
  var col = vec3(1.0, 0.7, 0.75);                                // base pink
  col = mix(col, col * 0.95, value_noise(p * 20.0) * 0.5);       // skin variation
  if in_circle(p, snout_holes, 0.015) { col = vec3(0.3, 0.1, 0.1); }  // nostrils
  if in_circle(p, eyes, 0.02)          { col = vec3(0.0, 0.0, 0.0); } // eyes
  return col;
}
```

Legs use `abs(p.x)` for bilateral symmetry — mirroring in the domain is the cheapest form of
"modelling".

## The CPU reference (crates/render)

**Landed 2026-10-12, extended the same day.** A Rust evaluator for the grammar above - `validate`, `sdf_eval`, `march`,
`normal` - covering thirteen of the fifteen kinds, including the two transforms `Twist` and `Bend`, and **refusing** `Noise`
rather than answering a distance question with a guess. It is the reference the WGSL interpreter has to
agree with, and it is what lets two of this doc's claims be measured before a shader exists:

- **The step guarantee.** A ray from six units out along x hits the unit sphere at `t = 5` in at most
  two steps, the hit point is on the surface, and a ray aimed away terminates on the step budget
  rather than spinning. That is "each step equals the SDF value" being a proven-empty jump, tested
  rather than asserted in prose.
- **R5, in the narrow sense it can be tested at this stage.** A scene and a millionth-scale copy of it,
  marched with proportionally scaled epsilons, need nearly the same number of steps and agree on the
  hit to under 1% relative, and the tetrahedral normal at 1e-6 still comes out unit length. The
  measurement also found its own limit: the `f32` ulp near 1e-6 is about 1e-12, so an absolute epsilon
  finer than that can never be satisfied and the ray walks past. That is not a shader bug - it is the
  concrete reason docs/02 keeps coordinates camera-relative and AGENTS.md §4 keeps floats off the
  hashed side.

What it does **not** measure: R1's 1080p/60 with 500 culled entities and R2's ten-minute edit loop.
Steps per ray on a CPU are not nanoseconds per frame on a GPU, and there is no WGSL interpreter, no
BVH, and no `assetc` to emit an asset yet - so those stay targets, and this file says so instead of
quoting the reference implementation's speed as if it were the renderer's.

### The WGSL interpreter agrees — having first disagreed (2026-10-12)

`crates/gpu` holds A-07's master interpreter in WGSL, and `cargo run --release -p gpu --example
agreement` uploads the engine's own five-node boot scene and compares the two implementations at
1,521 fixed sample points on a device. On the `Microsoft Basic Render Driver [Dx12, Cpu]` adapter the
maximum deviation was **0.000000000 world units** — not "close", identical, which is what you want
from two `f32` evaluators of the same formulas. CI, on two different vendors, is the more interesting
measurement (run `38268554246`): the Windows runner, also on the software adapter, reproduced the exact
zero, while **ARM64 macOS on Metal differed by 0.000000238 world units** at a point 2.44 units from the
surface — a relative 1e-7, which is one fused-multiply-add being contracted by one compiler and not the
other. The 1e-4 tolerance is written to pass that and fail the 0.042 divergence above, and the reason it
can be that loose without becoming weak is docs/02: nothing here is hashed, so a vendor-level `f32`
difference is a shading nuance rather than a desync. The Linux runner printed its SKIP and exited 0. CI runs it on every gate job; the x86-64 Linux runner
has no adapter and the step prints a SKIP and exits 0, so a missing device can never read as
agreement, and an adapter that refuses to open a device is a hard failure rather than a skip.

It disagreed first, and the reason is the whole point of writing it twice. The shader's smooth union
used the cubic blend that circulates in the SDF literature — `h³ · k / 6` — while
`crates/render::smin` is quadratic, `h² · k / 4`. Both are valid smooth minimums, both keep the
1-Lipschitz bound that makes a march step a guaranteed-empty sphere, and both read as "the" formula.
At `[-2.52, -1.48, -2.70]`-adjacent sample `…-1.48…` inside the blend region they differed by
**0.042 world units**, about 4% of a voxel, which in a shipped frame is a surface in the wrong place
and in a mod is two machines disagreeing about one shape. The shader now mirrors the reference
expression for expression, including the `k <= 0` collapse to a plain `min`. A device-free text guard
pins the three constants so the regression cannot come back on a runner with no GPU: `h * h * k *
0.25` present, `1.0 / 6.0` absent, the zero-radius guard present.

Two more things came out of running it rather than reading the docs. The interpreter needs five
storage buffers, and `Limits::downlevel_defaults()` allows four per stage — so the header moved to a
uniform binding, which is also the only shape that survives a WebGL-class backend, and that is a
constraint on the real renderer, not on a test. And a storage buffer cannot be mapped at all, so
results are copied into a `COPY_DST | MAP_READ` staging buffer before the CPU can look at them;
reading back is a two-buffer operation on every path that wants to check itself.

What is still missing here, stated so the agreement above is not over-read: the shader implements
sphere, rounded box, plane, union, smooth union, subtract and intersect — seven of A-07's fifteen
kinds, and the CPU reference's other six (cylinder, torus, repeat, mirror, twist, bend) have no WGSL
counterpart yet, while `Noise` is refused on both sides. There is no GPU march, no BVH and no
culling, so gate R1 and R2 are untouched by any of this, and none of these distances is hashed —
presentation agreeing with presentation is a contract check, not gate P1.5 or R4 evidence (ADR-017).

### The raster probe: the first frame-cost shape, on a CPU (2026-10-12)

`crates/render::raster` turns a tree into a pixel grid: a pinhole camera with an explicit image-plane
half-height (so ray generation needs no trigonometry at all), one `march` per pixel, and colour from the
surface normal. The colour is deliberately **not** a material model - `material_eval` and lighting are
[OQ-10](13-risks-open-questions.md)'s open question, so inventing shading here would answer a question the
project has not answered. `cargo run --release -p render --example raster_probe` prints the numbers and
writes a PPM into the temp directory, so there is a picture to look at as well as counts to argue with.

Measured on one machine, one 3-node tree (a ball on a ground plane), `surface = 1e-3`, 96-step budget,
release profile:

| Grid | Rays | Evaluations | Per pixel | Hit fraction |
|------|------|-------------|-----------|--------------|
| 96×72 | 6,912 | 314,554 | 45.5 | 67.0% |
| 192×144 | 27,648 | 1,258,840 | 45.5 | 67.0% |
| 320×240 | 76,800 | 3,495,846 | 45.5 | 67.0% |

Two things in that table matter more than the milliseconds:

- **Per-pixel cost is flat with resolution** - 45.5 evaluations at every grid size on a fixed tree, which
  is the expectation a frame-cost model needs before it can extrapolate to 1080p (2.07M pixels × 45.5 is
  the naive shape this doc's budget table has been deriving from guesses).
- **The rays that find nothing pay most of the bill.** Hits average 20.6 steps; the 33.0% of rays that
  miss are charged the full 96-step budget and account for **69.6% of all evaluations**. That is the
  BVH's target stated as a measured share rather than as ADR-024's promise, and it is the number the
  culling workstream has to move. The charge for a miss is an upper bound - `march` can also exit on
  range - so the true share is somewhat lower and the direction of the argument is unaffected.

The same example answers the second question the budget needs - what one more node costs. Holding the
frame fixed (96x72, `surface = 1e-3`, 96-step budget) and growing only the tree, as a balanced union of
spheres spread along x:

| Leaves | Nodes | March steps | ns per node-step | Marginal ns per node |
|---|---|---|---|---|
| 1 | 1 | 659,801 | 2.95 | - |
| 2 | 3 | 656,338 | 1.46 | 0.72 |
| 4 | 7 | 649,430 | 1.52 | 1.57 |
| 8 | 15 | 635,973 | 1.65 | 1.77 |
| 16 | 31 | 627,890 | 1.62 | 1.59 |

**The step count barely moves while the tree grows 31x** - 659,801 steps against 627,890, a 5% change -
because a step is a property of the field along the ray, not of the tree: a bigger union makes each step
*more expensive*, not the march *longer*. So the frame law is `steps x nodes`, and the two levers a
compile-time budget can bind are the step budget and the node count. That is the measured reason
`MAX_TREE_DEPTH` exists rather than being a tidy number, and the shape AS5 would enforce. The marginal cost came out near
1.6 ns per extra node per step on the laptop that ran it first - and then the sweep went into CI, where it
proved the part that matters. Run `38254332031` prints **659,801 steps** for the one-node tree and
**627,890** for the 31-node tree on x86-64 Linux, x86-64 Windows and ARM64 macOS alike, digit for digit on
three machines: **the step count is a property of the field along the ray, not of the tree and not of the
hardware.** The timings moved as expected between machines instead - the one-node grid took 1.04-1.07 ms on
the runners against 1.9-3.5 ms on the laptop, and `ns per node-step` at 31 nodes ranged 0.43 to 0.78.

So the practical conclusion for a compile-time budget (AS5) is: **bind node-steps, not milliseconds.**
`cost = steps x nodes` is portable across the architectures this project promises; the per-node constant
is not, varying by about 2x between the very runners that agree exactly on the work. A budget quoted in
nanoseconds would be a claim about a machine, and this project ships to several.

The boot prints the same shape at a smaller scale, and a sparser frame makes the point harder: its
64x48 grid reports 590 of 3,072 rays finding surface, and the 2,482 that missed are charged 238,272 of
the 243,008 evaluations - **98.1% of that frame's cost spent finding nothing at all**. The counts are
integers on purpose: the march and the normal use only add, multiply and `sqrt`, all exact under IEEE-754,
so the same numbers can be read off three different architectures in CI - and were: run `38250242415`
prints `590 of 3072 rays found surface, 243008 evaluations` identically on x86-64 Linux, x86-64 Windows and
ARM64 macOS, which is the first time this repository has compared a whole pixel grid's results between
machines. That comparison is a diagnostic, not a gate: nothing here feeds a digest, and a frame hash stays
informational by policy (ADR-017, gate R4).

The counts repeated identically run to run; the wall times did not (the 320×240 grid measured 53 ms and
86 ms in the same session on the same binary). That is the whole reason this section reports evaluations
and `crates/render/tests/raster.rs` pins coverage against the analytic projection -
`tan(asin(r / d))` of a unit sphere at six units should cover about 368 of 4,096 pixels, and the test
asserts it within 8% - instead of trusting a millisecond.

**Still not claimed, in this section's own words: this is not R1.** No GPU, no WGSL interpreter, no BVH,
no culling, no region streaming, and a normal-to-rgb colour that is geometry evidence rather than
rendering. What landed is that the field can become an image inside this repository, with a measured cost
shape, so the renderer workstream now has an on-CPU anchor to beat rather than only arithmetic.

## Culling and the frame budget

**Measured on the CPU first (`cargo run --release --example scene_steps -p render`),
2026-10-12, spike S1 part one.** One small scene - `smoothunion(sphere, sphere; k = 0.5)` unioned with a
rounded box - marched over square grids, step budget 96:

| Grid | Surface epsilon | Hit rate | Evaluations per ray | Sky (missed) rays |
|------|-----------------|----------|---------------------|-------------------|
| 32² | 1e-3 | 50.4% | 52.3 | 508 |
| 64² | 1e-4 | 49.8% | 54.3 | 2056 |
| 128² | 1e-5 | 49.7% | 55.9 | 8239 |

Three readings, in order of usefulness:

- **Misses dominate the cost.** About half the rays hit, and every missed ray spends the whole budget
  proving emptiness. Subtracting that, a *hit* pixel averages roughly 15 evaluations; the missed half is
  what drags the mean to ~53. This is the quantitative reason the pipeline pairs the march with
  bounding volumes and culling instead of trusting the field - an early-out on a scene like this is
  worth up to about 3x, and R1 without one is not a frame budget but a hope.
- **Convergence is not the sharp end.** Going two decades finer on the epsilon costs 6% more work
  (52.3 to 55.9), which is what a logarithmic approach looks like; the step count barely moves with
  resolution either (52.6 at 32² vs 52.6 at 128²), as it should for a per-ray property.
- **What this is not.** R1's number is 1080p/60 with 500 *culled* entities on a mid-range GPU. None of
  that is measured here: one thread, a CPU, four nodes, no BVH, no culling, no shader. The example
  exists to give spike S1 a curve to compare the WGSL marcher against, not to substitute for it.

| Mechanism | Notes |
|-----------|-------|
| Frustum culling | region-level and entity-level boxes |
| Spatial BVH over active SDFs | `O(log N)` skip; a ray executes an entity's detailed function only after hitting its coarse bounding sphere (ADR-024) |
| Region-level early-out | empty regions cost one table read |
| Step-count caps + `MIN_STEP` floor | guards the worst-case march |
| Distance LOD | fewer SDF terms for distant entities; epsilon grows with distance |
| Tile-based marches | group pixels that hit the same region |

**Budget to be met, not assumed** (target 1080p/60 on a mid-range dGPU → a 16.7 ms frame):
geometry ≤ 8 ms + shading ≤ 4 ms + culling ≤ 1 ms + upload ≤ 1 ms = 14 ms committed, **2.7 ms
headroom**. The source never allocated a frame; every one of these five numbers is ours and all
five are provisional until spike S1 measures a real frame-time curve.

What the design conversation actually claims: raymarching means every pixel steps through space
evaluating functions, and *"if you have 500 procedural entities (pigs, trees, rocks) across a
landscape, evaluating every entity's distance function for every ray step will instantly drop
your framerate to single digits"* (GS 1618) — while *"simple SDFs (cubes, spheres, pigs) render
fast"* and *"combining hundreds of complex SDFs in a single GPU pass can tank frame rates"*
(GS 893).

**Our derivation, not the source's.** 1080p is 1920×1080 = **2.07×10⁶ pixels**; take ~10 march
steps per pixel and 500 visible entities:

| Case | Arithmetic | Evaluations / frame |
|------|------------|---------------------|
| Naive — every pixel tests every entity at every step | 2.07×10⁶ × 500 × 10 | **1.0×10¹⁰** (6.2×10¹¹ / s at 60 fps) |
| Perfect culling — entities cover ~30 px each, so only those pixels pay | 500 × 30 × 10 | **1.5×10⁵** |
| Realistic BVH — a bounded candidate set (~8 entities) per stepped pixel | 2.07×10⁶ × 8 × 10 | **~1.7×10⁸** |

Naive versus the perfect-culling floor is **~7×10⁴** — that gap is what the "single digits" claim
is really about, and why the BVH (ADR-024) is a requirement rather than an optimization. The
realistic BVH row is only ~60× below naive, and that 60× is the reduction the 14 ms budget
actually depends on. Every row is a multiplication of guesses — spike S1 replaces all three with a
measured curve.

> Every number in this section is a target or a derivation. The transcript's "60 FPS" is
> aspirational; spike S1 replaces all of this with a measured frame-time curve.

## Materials, lighting, sky

Materials are functions of `(p, n, t)` — no texture files, ever. Beyond that the conversation
went quiet, so lighting is **an open question, not a solved section**
([OQ-10](13-risks-open-questions.md)): soft shadows from an extra shadow march, screen-space or
baked AO, direct+ambient model, sky gradient and day/night driving the material functions,
volumetrics for fog and water ([OQ-12](13-risks-open-questions.md)). **Do not start content work
until this is decided** — everything authored depends on the shading contract.

## VRAM

Storage buffers are pre-reserved; the renderer binds `M_active` slabs directly (doc 03 tiering)
and never reads `M_cold` (the CPU streams it in when a region enters range). Buffer memory is
ring-recycled across epochs, so nothing is allocated mid-frame.

## Determinism boundary

- The shader receives: `C_global` (uniform), dirty slab bytes (storage buffers), the epoch root,
  and the node trees. It **outputs pixels only**.
- No shader result is hashed, stored, sent on the wire, or fed back to the simulation.
- Approximation rules for the visuals themselves (where the design's cheapness claims live):
  `sin`/`cos` are **precomputed tables or polynomials with published coefficients** — WGSL
  leaves `sincos` to the implementation and results differ across drivers, so "the renderer is
  approximate" is only safe because state never depends on it. f16 is opt-in per pass. Derivative
  `fwidth` usage must be uniform within a quad. Full-width `dFdx`-free marches where possible.
- Rejected: evaluating block *state* in the shader because "GPUs love `(x,y,z,t)` in parallel".
  It is true, it is tempting, and it destroys the verification guarantee (ADR-017 is the fix).

## Rejected alternatives

| Alternative | Why |
|-------------|-----|
| Mesh + texture pipeline | Needs artists or an LLM→binary path that doesn't work; two asset/physics systems (ADR-011) |
| Per-entity hand-written shaders | Doesn't scale to content diversity; unbudgetable (ADR-012) |
| Rasterizing signed voxels | Reintroduces meshing; loses infinite-detail zoom |
| GPU-authoritative state | Breaks cross-GPU bit equality (ADR-017) |
| Ray tracing hardware acceleration (`NVBLAS` etc.) | Vendor-specific; the march is already coherent-ray friendly — revisit post-launch |

## The node grammar contract

**Drafted 2026-10-12 as artifact A-07 (`contracts/sdf.rs`), DRAFT, proposed in [ADR-031](decisions.md).** ADR-012 decided content is data rather than source and named the consequence itself: the grammar is a public API surface living in `contracts/`. This is that surface, and it pins five things, each because a second implementation would otherwise get it differently:

| Pinned | Choice | Why it is a contract and not a detail |
|--------|--------|---------------------------------------|
| Kind numbering | 15 kinds: primitives `0..=4`, combiners `8..=11`, transforms and noise `16..=20`, material leaf `24` | The numbering is on disk in every authored asset. A renumbering silently reinterprets them all, so values are never reused or shuffled |
| Arity | Fixed per kind, read from `arity_of`, never from the file. Combiners are **binary**: `assetc` flattens an n-ary authoring union into a left-leaning chain | One branch to budget instead of *n*, and a tree cannot hide a wide fan-out. The validator and the interpreter read the same table, so a tree that would make the shader guess is refused before upload |
| Record | 16 bytes, four words: `kind` + `arity`, `subtree_len`, `params_first` + `flags`, `params_len` + reserved | The tree is one WGSL storage buffer. Indices replace pointers, which do not survive upload, and pre-order plus `subtree_len` makes a sibling `index + subtree_len` - arithmetic, not a walk |
| Depth cap | `MAX_TREE_DEPTH = 16`, **a draft** | Chosen so a validator exists at all. Spike S1 s frame-time curve is what should set it, and saying so is the difference between a number and a guess |
| Float rule | Geometry is `f32` and never hashed; `sin` and `cos` are legal inside Twist, Bend and domain warp | [AGENTS.md](../AGENTS.md) section 4 and ADR-017, because this doc's determinism boundary says the renderer outputs pixels only. A material id a peer must agree on is a ledger value from A-03, not a grammar value |

**A lesson from the drafting, kept in the open.** The first record layout packed `params_first`, `params_len` **and** `flags` into one word, and shifting `flags` by 24 overwrote the top byte of `params_len`: a node wanting `0x0abc` parameters read back `0x0acc`. Nothing failed to compile and review did not catch it - a test did. A layout shared between `assetc`, WGSL and a WASM binding that loses bits on write is worse than one with fewer fields, because the loss is invisible in the writer and fatal in the reader. `the_record_round_trips_without_losing_fields` pins that case, and `a_record_can_be_caught_lying_about_its_arity` is the validator half of it.

**Why this section does not mirror the contract verbatim**, the way docs/03 mirrors A-10: a contract whose doc comments contain fenced code blocks unbalances the markdown block a mirror needs, and `tools/check_docs.mjs` then reports an odd fence count for the document. `contracts/sdf.rs` is the single source of truth for the grammar until A-07's FlatBuffer half (doc 09) and ADR-031's sign-off settle a mirror-safe form.

## Validation gates

| Gate | Test | Target / source |
|------|------|-----------------|
| R1 | 1080p/60 with 500 culled entities in scene | **invented target** — the transcript's own "60 FPS" is aspirational; set from spike S1 |
| R2 | Frame time stable while a player continuously edits terrain for 10 minutes | doc 03 tiering |
| R3 | Region entry after LRU eviction shows prior edits with no visible pop-in | gate P1.4 |
| R4 | Frame hash of a scripted camera path is identical across `wgpu` backends | **informational only** — must not gate shipping, per ADR-017 |
| R5 | Zoom to 1e-6 world units on a pig snout with no polygonation | ADR-011's central claim |
