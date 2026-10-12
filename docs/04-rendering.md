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

What the same probe learned about the harness is in [OQ-41](13-risks-open-questions.md) and
[OQ-51](13-risks-open-questions.md): PR #1 ran a `wgpu` adapter probe on the three gate runners and
found a device on two of them (Dx12 software on Windows, Metal paravirtual on ARM64 macOS) and none on
Linux. So "the WGSL interpreter must agree with this reference" is gateable in CI as soon as the
interpreter exists, on two architectures, with the third skipping out loud - and a software device
validating a shader is still not a machine on which gate R1's frame budget can be measured.

**Still not claimed, in this section's own words: this is not R1.** No GPU, no WGSL interpreter, no BVH,
no per-entity culling (the enclosure early-out two sections below is one bound for the whole scene, which is
a different thing and mostly does not help), no region streaming, and a normal-to-rgb colour that is
geometry evidence rather than
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

### The first early-out exists, and the 3× estimate in the reading above was the wrong axis (2026-10-12)

`crates/render/src/cull.rs` is the cheapest possible answer to the miss share: one conservative sphere per
subtree, derived by set relation — a union is covered by both children, a smooth union grows that cover by
`k` because `smin` rounds outward by at most `k`, a subtract or intersect takes its **first operand's**
bound because the result is a subset of it, and a mirror covers the child and its reflection. Then one
segment test per ray. `render::raster::render_culled` is the same march with that test in front of it, and
`cargo run --release --example cull_probe -p render` prices it on a 64×48 grid, 96-step budget, spheres of
radius 0.4, two arrangements of the same counts:

| Arrangement | Spheres | Bound r | Evals plain | Evals ray-cull | Evals per-node | Rays skipped | Saved by ray | Saved per-node |
|---|---|---|---|---|---|---|---|---|
| cluster | 1 | 0.40 | 294,544 | 16 | 16 | 99.9% | **100.0%** | 100.0% |
| cluster | 8 | 2.20 | 291,594 | 14,250 | 14,250 | 94.0% | 95.1% | 95.1% |
| cluster | 32 | 4.96 | 279,551 | 70,271 | 70,271 | 71.0% | 74.9% | 74.9% |
| cluster | 128 | 16.09 | 278,136 | 147,384 | 147,384 | 44.3% | 47.0% | 47.0% |
| line | 8 | 4.60 | 291,164 | 80,540 | 80,540 | 71.4% | 72.3% | 72.3% |
| line | 32 | 19.00 | 287,112 | 287,112 | 287,112 | 0.0% | **0.0%** | 0.0% |
| line | 128 | 76.60 | 287,112 | 287,112 | 287,112 | 0.0% | **0.0%** | 0.0% |

**The last two columns are identical, and that equality is the most useful number in the table.** The
per-node skips exist: `scene_bounds` builds one enclosure *per node* in a single reverse pre-order pass,
`sdf_eval_bnb` consults it inside the same `eval_at` (one evaluator, one field, no second implementation
free to drift), and the union arm declines to evaluate its second child when that child's own bound is
already farther than the value in hand. It is correct, and this instrument cannot see it, for one reason:
`Stats::evaluations` counts **march steps**, and a skip inside a step changes no step count — the distance
returned is the value the unpruned field would have returned, so the ray's trajectory is identical by
construction. A hierarchy shortens each step rather than their number. Pricing ADR-024 therefore needs a
counter of *primitive evaluations per frame*, which does not exist yet; until it does, the honest
statement about per-node skipping is **implemented, provably frame-preserving, unmeasured** — and no
saving may be quoted from this table for it.

Spike S1's third reading above guessed that "an early-out on a scene like this is worth up to about 3×".
The measurement says the guess was not a ceiling or a floor but the wrong variable: the same one-sphere
early-out saves **100.0%** of the work, and the same spheres laid down a line save **nothing at all** — the
root bound grows until it contains the whole frustum, every ray is merely *permitted* to march, and the hit
count stays where it was (88 hits, 0 rays skipped). What determines the saving is the bound's tightness
relative to the frustum, not whether an early-out exists.

That is ADR-024's hierarchy stated as a requirement rather than a promise, and it arrived by building the
weak version first: one bound is a fixed win that decays as the scene spreads, so the next structure has to
be **many bounds, chosen per ray** — the `O(log N)` skip the mechanism table below already names. The
`Work saved` column is the baseline a BVH is measured against at the same hit fraction.

Two honesty notes this section keeps rather than smooths over:

- **Correctness was the design constraint, not speed.** A bound that is too small silently deletes
  geometry — a frame that looks like a render. So the module's refusals are load-bearing (`Plane`,
  `Repeat`, `Twist`, `Bend`, `Noise` and any unknown kind answer `Unbounded`, which means "no early-out",
  never "a guessed box"), and `crates/render/tests/cull.rs`'s nine tests assert that a culled frame is
  **byte-identical** to an unculled one — pixels, depth and hit count — while requiring the evaluation
  count to drop by exactly `skipped × max_steps`. A second test samples the evaluator for interior points
  and demands the enclosure contain them, which makes the field, not this file's arithmetic, the oracle;
  a third asserts the two bound derivations agree — the re-descending `enclose` and the one-pass
  `scene_bounds` — because two implementations of one rule is the divergence this repository has been
  burned before.
  The subtle part is what a per-node skip is actually entitled to, and it is not coverage: the field must
  be **at least the distance to its own bound**, which is stronger than "the bound contains the surface".
  A `min` preserves that property (each child is at least its own distance and the covering sphere is no
  closer than either), a `max` inherits it from the first operand — which is why `Subtract` and `Intersect`
  can be pruned below — and a mirror keeps it, but **`smin` returns less than `min`, by up to `k/4`**, so
  no node under a smooth union may be skipped however far its sphere is. That bit is computed
  structurally per node in the same pass, and the test asserts the refusal rather than the speed.
  Those tests found nothing to fix first try, which is worth saying plainly: their value is that the next
  bound rule — a hierarchy's, or a deformation's — cannot be shipped on a guess.
- **No gate moved.** R1 and R2 are still frame-time claims with no GPU behind them. These are counts of
  work avoided, not nanoseconds, and the arithmetic is sqrt-and-multiply on purpose: a transcendental here
  could make two machines disagree about which rays were skipped, and while nothing hashed depends on it
  (docs/02's float discipline), a picture that differs per platform is still a bug.

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
