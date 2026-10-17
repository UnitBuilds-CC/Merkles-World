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

One scene is a demo, so the check became a gate: `examples/agreement.rs` builds **one tree per
implemented kind** — five leaves, four binaries, four space-folding unaries — and compares 729 fixed
sample points for each. A single union-of-three exercises three arms and hides ten, and the arms that
hide are the ones worth checking: the unaries fold space, so they need their own point per stack frame
in a walker that has no recursion.

| Kind | Max deviation, GPU vs CPU |
|---|---|
| sphere, rounded box, cylinder, torus, plane | 0.000000 |
| union, smooth union, subtract, intersect | 0.000000 |
| repeat, mirror, bend | 0.000000 |
| **twist** | **0.000001** |

Twelve of thirteen agree to the last bit, and the thirteenth is the interesting one: `twist` is the
only arm that calls `sin`/`cos`, and it is the only arm with any deviation at all — 1e-6 world units,
the GPU's transcendental and libm's disagreeing by about a millionth. That is docs/02's float
discipline measured rather than asserted: a transcendental is fine in the renderer precisely because a
root never sees it, and the same property is why this comparison needs a tolerance instead of an
`==`. The other twelve agreeing exactly is the same fact pointing the other way — add, multiply and
`sqrt` are IEEE-determined, so a march's step counts can be and are reproducible across machines.

What is still missing, stated so none of the above is over-read: the shader covers thirteen kinds —
the whole implemented set — but `Noise` is refused on both sides (it needs docs/02's lattice hash and
OQ-45's payload meaning) and a material node has no WGSL arm, which is A-07's shape/material split
holding rather than a gap in it. There is no GPU march, no BVH and no culling, so gates R1 and R2 are
untouched by any of this, and none of these distances is hashed: presentation agreeing with
presentation is a contract check, not gate P1.5 or R4 evidence (ADR-017).

### The raster probe: the first frame-cost shape, on a CPU (2026-10-12)

`crates/render::raster` turns a tree into a pixel grid: a pinhole camera with an explicit image-plane
half-height (so ray generation needs no trigonometry at all), one `march` per pixel, and colour from the
surface normal. The colour is deliberately **not** a material model - `material_eval` and lighting are
[OQ-10](13-risks-open-questions.md)'s open question, so inventing shading here would answer a question the
project has not answered. `cargo run --release -p render --example raster_probe` prints the numbers and
writes a PPM into the temp directory, so there is a picture to look at as well as counts to argue with - and
since 2026-10-13 it reads that file back and requires every byte to equal the frame in memory, because a
printed path is a claim about a file nobody opened. The shipped binary does the same for the demo's own
three-cell world on request: `cargo run --release -p engine -- --frame-ppm` writes the frame step 9 just
marched and asserted equal to the descent's picture, upscaled nearest-neighbour 64x48 to 256x192 because two
cells meeting is not legible at 64 pixels. Measured on this host: `259 of 3072 rays found surface, every byte
equal to the frame in memory` - the same 259 the region line beside it reports. It is opt-in, it is not a
second march, and the default boot prints nothing extra: `cargo run -p engine` still hashes to the published
`db71f8a0bf5818d6…`, which is what every CI leg checks.

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
64x48 grid reported 590 of 3,072 rays finding surface (the hero scene; the shipped scene is a
spread now and reports 605), and the 2,482 that missed are charged 238,272 of
the 243,008 evaluations - **98.1% of that frame's cost spent finding nothing at all**. The counts are
integers on purpose: the march and the normal use only add, multiply and `sqrt`, all exact under IEEE-754,
so the same numbers can be read off three different architectures in CI - and were: run `38250242415`
prints `590 of 3072 rays found surface, 243008 evaluations` identically on x86-64 Linux, x86-64 Windows and
ARM64 macOS, which is the first time this repository has compared a whole pixel grid's results between
machines. That comparison is a diagnostic, not a gate: nothing here feeds a digest, and a frame hash stays
informational by policy (ADR-017, gate R4).

The boot now marches that same grid three ways and prints all of them - `243008 evaluations, 179072 with
the scene cull (bound r 2.10, 666 of 3072 rays proved empty), 179072 with the hierarchy descent (666
proved empty, root licence refused - this scene blends, so the descent inherits the single bound;
cull_probe prices that licence)` - and asserts inside the binary that the three frames
are identical pixels and depth, because a bound that is too small deletes geometry and the result looks
like a correct render, which is a fact worth checking where a real frame is drawn rather than only in a
suite. It also shows the limit in the demo's own numbers: 81% of the boot's rays miss and only 21.7% are
proved empty, because a 2.1-unit sphere around a scene fills a camera that close - the same saturation
`cull_probe` measures on the line arrangement, arriving by accident in the shipped scene. Older CI runs
quoted above print `178880 … 668 of 3072`: two rays moved back across the line when a skip began testing
the bound grown by the marcher's hit tolerance (see the third-layer section below), and they are the boot's
own grazing cases. The shape of the line is unchanged, the numbers in it are measured, and the whole
fifteen-line output hashes to `167a0cade1327d0e…` in `docs/evidence.json` (after `d80711e5fbdff94b…`, (current: `c5ca867b681e8971…`) (current: `db71f8a0bf5818d6…`)
`8cdd94f4c617922c…` and `15c1e8af1b523d73…`, each superseded by a printed line rather than a hashed (current: `c5ca867b681e8971…`)
value), which both the docs gate and CI check rather than believe.

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

### The march is on the device now, and it is 824 ms rather than 4,005 (2026-10-14)

The section below prices a *harness*: `frame_curve` queues 46 dispatches of one-evaluation-per-pixel over the
same 2M-point grid and takes the answer back once, so most of its 4,005 ms is carrying a grid across the bus
46 times. A renderer does not do that, and now this repo can ask the question the way R1 poses it.
`interp.wgsl`'s `march_rays` entry point sphere-traces inside the shader — the same step law as
`render::march_with`, transcribed expression for expression, including the `max(distance, surface)` advance —
using the buffers `evaluate_points` already binds (a stage may not hold six storage buffers on
`Limits::downlevel_defaults()`). `cargo run --release -p gpu --example on_device_march` on the MX250:

| grid | rays | hits | ms submit | ms for one dispatch + one copy back |
|---|---|---|---|---|
| 64x48 | 3,072 | 132 | 0.10 | 2.11 |
| 256x192 | 49,152 | 2,460 | 0.15 | 22.79 |
| 640x360 | 230,400 | 9,012 | 0.15 | 92.61 |
| 1280x720 | 921,600 | 35,964 | 0.18 | 362.0 |
| 1920x1080 | 2,073,600 | 80,964 | 0.17 | **824.2** |

So a 64-step march of a 1080p grid against a three-node field is **824 ms**, about **4.9× cheaper than the
4,005–4,015 ms** the 46-dispatch harness reported on the same device for roughly the same number of field
evaluations (95.4M there; ~80M hits-plus-misses here at a 64-step cap). The harness was a real cost, but it was
not the dominant one: **the frame's own work is 49× over R1's 16.67 ms budget on a laptop dGPU before any
culling exists**, which is the same verdict the earlier section reached by arithmetic and now reaches by
measurement. What remains is the lever, not the plumbing: ADR-024's descent and `stream`'s region bounds are
the mechanism, and this march does not use them yet.

The anchor is asserted in the binary, not argued: the 64×48 grid's depths are compared ray-for-ray against
`render::march`, and measured **132 device hits / 132 reference hits, 0 hit-vs-miss disagreements, max
deviation 0.000000**. The check was falsified before it was trusted — replacing the shader's advance with
`max(distance * 0.5, surface)` produced 16 disagreements and exited 101, and notably the depth tolerance alone
did not catch it (0.0073 sits under any tolerance a 64-step march needs), which is why the assertion is on the
hit/miss *set*. Timings repeat: 824.7 and 824.2 ms across two runs of the same binary.

### R1's frame split two ways, and the half R1 names is the one that stops costing (2026-10-16)

The section above says the 1080p frame is 49× over budget and points at culling. Before accepting that
direction, `cargo run --release -p gpu --example r1_step_price` prices the two terms separately on the same
three-node field: it marches one grid at seven step caps, and at each cap asserts the 64×48 anchor against
`render::march` (hits equal, 0 hit-vs-miss disagreements, max deviation 0.000000 — 132 hits at cap 64, the same
number `on_device_march` reports from a different binary), then times 230,400 rays at that cap. Three reps,
median printed, MX250/Vulkan:

| cap | small-grid device hits | reference hits | disagreements | worst &#124;dev&#124; | frame hits | ms | ns per ray-step (amortised) |
|---|---|---|---|---|---|---|---|
| 1 | 0 | 0 | 0 | 0.000000 | 0 | 24.28 | 105.36 |
| 2 | 12 | 12 | 0 | 0.000000 | 976 | 33.04 | 71.69 |
| 4 | 96 | 96 | 0 | 0.000000 | 7,240 | 51.11 | 55.46 |
| 8 | 96 | 96 | 0 | 0.000000 | 7,240 | 80.90 | 43.89 |
| 16 | 116 | 116 | 0 | 0.000000 | 8,108 | 93.50 | 25.36 |
| 32 | 116 | 116 | 0 | 0.000000 | 8,768 | 99.16 | 13.45 |
| 64 | 132 | 132 | 0 | 0.000000 | 9,012 | 107.90 | 7.32 |

Read it as a sum of two terms, and read the millisecond column as five separate runs said to be read that way.
**This table is run 1 of five.** **Cap 1 is a frame with essentially no march in it** — one step per ray, 0 of
the small grid's rays reaching the surface — and across five runs that row cost **24.28, 33.24, 87.92, 32.62 and
32.27 ms**: the dispatch, the 7.4 MB ray buffer and the depth copy-back, **every one of them over R1's entire
16.67 ms budget before the step loop matters**. The *curve*, on the other hand, did not reproduce, and the honest
version of this section has to say so: runs 1-2 rose smoothly to ~108 ms, while runs 3-5 put a **bimodal cap-8
row** in the middle — 80.90 and 95.69 ms in the first two, **550.6 / 551.7 / 545.5 ms** in the three later ones —
with cap 64 falling back under it at 108–126 ms in all three. Ratioed against their own cap-1 rows, five runs of
the same measurement price cap 8 anywhere from **2.9× to 16.9× cap 1**, which is not a measurement of a step
loop; it is a measurement of a laptop driver. The first draft
of this example answered the question by least-squares fitting the rows, the fit was invalid for data like that
(intercept 60.4 ms against a measured 33.2 ms, and its budget line printed a *negative* number of affordable
ray-steps), and the fit is gone rather than tuned.

What did survive all five runs is an integer, and it is the part worth pricing against. **Cap 4 and cap 8 find
exactly the same 7,240 surfaces of 230,400 rays, and cap 4 → cap 64 — sixteen times the permitted steps — moves
the swept frame by 1,772 surfaces, 0.77% of the grid.** CI run `38893054398` reads that out across vendors: the x86-64 Linux and x86-64 Windows
`gates` jobs print `SKIP r1_step_price: no device (NoAdapter)` because no CI runner is a GPU, while the ARM64
macOS runner opens `Apple Paravirtual device [Metal, IntegratedGpu]` and reports **the same 7,240 and the same
1,772 surfaces at 0.77%**, with its own cap-1 row at 27.47 ms — a sixth sample inside the 24–88 ms band, and a
second vendor for every asserted integer. That is the saturation, stated as work rather than as
milliseconds: past a small cap the rays that would converge have converged, and the rays that will never hit
anything pay the same at any cap. The amortised column agrees in shape (105 → 7 ns per ray-step) but is the
weaker evidence, for the reason above. And the 1080p figure stays a **DERIVED 9.0× ray ratio** against
`on_device_march`'s own measured 824 ms, because this sweep deliberately does not run at 1080p.

Two things the instrument does *not* establish, kept visible because they are the next questions. First, "fixed"
here means **cap-independent at one grid**, not resolution-independent — the ray buffer and the depth copy both
scale with pixels, and this sweep cannot separate a term that scales with resolution from one that does not.
Second, it does not split the fixed term into bus versus device: `on_device_march` reports 0.15 ms for
submit-only against 92.61 ms for one dispatch plus one copy-back at this grid, but the submit column is enqueue
only, so the march itself hides inside the second number and the 64×48-vs-1080p gap stays unattributed. The row
that would separate them is a cap-1 1080p frame, and this machine refused it: repeating a 66 MB ray-buffer
dispatch at 1080p lost the device after four rows (`Parent device is lost`), and cap 8 came in at 1,745 ms
there against 824 ms at cap 64 — driver thrash, not a frame. A persistent ray buffer, or a machine that survives
21 full-resolution dispatches, owns that measurement.

The anchor was falsified before it was trusted, and the plant returned a second finding. Giving the reference
**one step more than the device** at every cap made the sweep stop at cap 1 with `12 hit-vs-miss` and exit 101 —
so the check does test the step law rather than the scene. It also showed the hit-count assert was reading
backwards: `reference_hits` was tallied only for rays where *both* sides found surface, so at cap 1 it printed 0
against the reference's actual 12 and the count comparison was device-vs-intersection, not device-vs-reference.
The disagreement arm would still have caught the plant, which is exactly why the counter was rewritten to count
what the reference sees on its own — a check that can only fire in one direction is a check on half the bug
space.

The consequence for the roadmap queue: **R1's named lever (cull rays, bound hierarchy, fewer rays) attacks the
term that has stopped costing, and the term that has not is the per-frame host round trip.** AS4's clause — no
per-frame allocation, one upload — is the same conclusion from the allocation side, and docs/11's R1 row now
carries this split instead of only the 824 ms. Neither is asserted as a duration (ADR-017): what the binary
asserts is the seven per-cap hit counts and the seven per-cap depth deviations.

### The split at 1080p, and the sentence above that this run retracts (2026-10-16)

The previous section's closing claim was made on a 640×360 grid and its own text flagged the gap: "cap-independent
is not resolution-independent", and the bus-vs-device half of the round trip was unattributed because
`march_uploaded` creates its ray buffer with `create_buffer_init`, so the host→device copy of the grid happens
*before* any stopwatch starts. Both gaps are now closed by `Interpreter::begin_frame` / `write_rays` /
`march_frame` (buffers allocated once per ray count, the grid packed in place and pushed with two
`write_buffer` calls — which is also AS4's no-per-frame-allocation clause satisfied on the frame path rather than
asserted in prose), measured by `cargo run --release -p gpu --example r1_round_trip_split` at **1920×1080**, the
grid the previous attempt could not repeat.

Two consecutive runs, MX250/Vulkan, buffers allocated once for the whole sweep:
| cap | frame surfaces | sampled ref / device hits | disagreements | worst &#124;dev&#124; | write ms (run 1 / 2) | submit ms | wait after a write ms (run 1 / 2) | wait, nothing written (run 1 / 2) |
|---|---|---|---|---|---|---|---|---|
| 1 | 0 | 0 / 0 | 0 | 0.000000 | 73.93 / 66.32 | 0.30 / 0.74 | 113.84 / 115.91 | 126.42 / 96.10 |
| 8 | 65,148 | 66 / 66 | 0 | 0.000000 | 62.66 / 30.86 | 0.21 / 0.16 | 798.26 / 729.57 | — |
| 64 | 80,964 | 80 / 80 | 0 | 0.000000 | 42.34 / 32.45 | 0.29 / 0.23 | 881.17 / 875.39 | — |

**Every asserted integer reproduced between the two runs and the milliseconds did not**, which is the same
discipline the section above applies: the hit counts, the sampled agreements, the zero disagreements and the
0.000000 deviations are identical in both; `write_rays` moved from 73.93 to 66.32 ms at cap 1 and the refill
delta changed *sign* (−12.58 ms then +19.81 ms), so the refill cannot be separated from the wait by this
instrument and what it demonstrably costs is its own interval — the copy into the driver's staging, the number no
earlier measurement contained. The cap-64 wait (875–881 ms) agrees with `on_device_march`'s independently written
824 ms to within 6–7%, from a different binary and a different buffer path, and 2,073,600 rays × 32 bytes out
against 92-byte scene asset is 74,649,600 bytes per frame — integers, not timings. CI run `38914217642` puts a **third vendor** behind those same integers:
the ARM64 macOS runner opens `Apple Paravirtual device [Metal, IntegratedGpu]` and reports **0, 65,148 and 80,964
surfaces** at the three caps with the same 66/66 and 80/80 sampled agreements, zero disagreements and 0.000000
deviations, and the same verdict that the cap bites (+361.9% of the cap-1 wait); its own milliseconds are a
different machine again — `write_rays` 25.41–33.48 ms, waits of 99.70 / 372.87 / 460.51 ms, a 30 ns-per-ray floor
against the MX250's 46–61 — and it agrees with `on_device_march`'s 824 ms only to 44%, which is printed rather
than smoothed. The x86-64 Linux and Windows `gates` jobs print `SKIP r1_round_trip_split: no device (NoAdapter)`,
because no CI runner is a GPU.

What the run **retracts** is the shape claim. At 640×360 the step work saturates (cap 4 and cap 8 find the same
7,240 surfaces). At 1080p it does not: cap 8 → cap 64 adds 15,816 surfaces (65,148 → 80,964, 24% more of the
frame's own hits) and the wait grows from ~114 ms at cap 1 to 875 ms at cap 64 — 655–674% of the cap-1 wait for
64× the permitted steps. So the sentence "R1's named lever attacks work that saturates while the round trip keeps
costing the same" is **true of a 230,400-ray grid and false of R1's grid**, and the instrument that says so is
the newly added lattice anchor: 2,040 pixels (every 32nd of both axes) marched by `render::march` at each row's
cap and compared ray-for-ray against the device. That anchor was not decoration. The first version of it sampled
a flat stride of 64 over the ray index, which at width 1920 is degenerate — 1920 is a multiple of 64, so every
sampled ray sat in column 0, the most oblique column in the picture — and it refused the cap-8 row with the
device seeing 0 hits where the reference saw 1,020. Replacing the degenerate sample with the 2-D lattice made the
same row agree at 66/66, which is the difference between a found bug and an instrument bug, and it is why the
lattice prints its own ref/dev/disagreement columns *before* its asserts fire: a run of this binary that had
panicked on the earlier sample would have left no numbers behind, and the cap-64 row of the run before these two
had already shown what an unanchored 1080p row can print (the cap-8 hit count, with a wait matching its own cap-1
row — exactly what a header whose cap never landed would look like, and nothing in the program could disagree).

The floor is then the honest part of the earlier claim: **cap 1 — one step per ray, 0 surfaces found — waits
114–126 ms for a frame that also spends 66–74 ms in `write_rays`, i.e. 46–61 ns per ray before any ray marches,
5.8–7.6× R1's whole 16.67 ms.** At that per-ray cost the budget buys 273k–360k rays, 13–17% of the grid. So at
R1's resolution both halves are over budget and four levers survive, in the order cheapest to test: derive the
rays in the shader instead of uploading 2M of them, march fewer of them (culling, tiles, the streamed carry), cap
the steps — the lever the 640×360 sweep ruled out and this grid restores — and keep the depths on the device,
since a shipping frame maps nothing and most of the wait is this probe reading a buffer back to check it. The
first three are not measured here: the instrument has to write its rays and read them back to have anything to
assert, so it prices the frame it can check, not the frame an engine would draw. R1 stays informational (ADR-017,
gate R4); what the binary asserts is the per-cap hit counts, the lattice's ray-for-ray agreement with
`render::march`, bit-identity with the allocate-per-call path, four refusals run inside the program (a
mismatched grid, an unwritten frame, a re-bound shape, a moved camera) and the byte counts.


### The first number off a real GPU, and it is a design verdict rather than a pass (2026-10-13)

`cargo run --release -p gpu --example frame_curve` measures the one quantity that exists on a device today:
the A-07 WGSL master interpreter, **one field evaluation per pixel**, on a three-node tree, on a
**GeForce MX250 over Vulkan** (`open_hardware()` exists precisely because `open()` prefers the software
adapter — right for the agreement check, wrong for a timing). Two columns, because they are two different
costs and only one of them bounds a frame:

| grid | points | ms/pass submit-only | ms/pass with read-back |
|---|---|---|---|
| 128x128 | 16,384 | 0.189 | 0.886 |
| 512x512 | 262,144 | 0.083 | 12.743 |
| 1920x1080 | 2,073,600 | 0.137 | **91.350** |
| 1920x1080, 46 passes, one copy back | 95,385,600 evals | — | **4,005.2 / 4,015.4** |

Then the same probe on a **desktop discrete GPU** — buildbox's GTX 1070, Vulkan, `DiscreteGpu` — which is the
instrument R1 was actually written against:

| device | 1080p, one eval/pixel, completion | 1080p, 46 evals/pixel, one copy back |
|---|---|---|
| GeForce MX250 (Vulkan, laptop dGPU) | 91.35 ms | 4,005–4,015 ms |
| **GeForce GTX 1070 (Vulkan, desktop dGPU)** | **13.46 ms** | **551.5 ms** |

That changes the shape of the gate rather than just its numbers. One field evaluation per pixel across a full
1080p grid lands *inside* R1's 16.67 ms budget on this hardware, and the 46 a march needs land 33× outside it.
So R1 is not a shader-throughput question that tuning closes: the budget buys about one evaluation per pixel,
and closing a ~46× gap is the explicit job of culling, the hierarchy and the step budget — ADR-024's descent,
measured today at 96–97% of node work avoided on a wide ridge, is the mechanism the gate depends on, and
`stream`'s region bounds are what keep off-screen work at zero.

A caveat on the laptop row rather than on the desktop one: re-running the same probe on the MX250 gave
627 ms for the same single-evaluation pass that had read 91 ms minutes earlier, so a laptop dGPU's absolute
numbers are not reproducible across runs (power states, and nothing here pins clocks). The 1070 row is the one
the argument rests on, and even it is a rate, not a frame. This is exactly why timings sit outside the hashed
surface (ADR-017) and why R1's verification will need repeated runs reported as a distribution rather than a
single quoted figure.

**Why buildbox reported no adapters at all, when its Vulkan loader sees the GPU (2026-10-14).** Not the driver,
not permissions, not the ICD: its `gcc` is a **Nix-store wrapper** (`/nix/store/…gcc-wrapper-13.4.0`), and
binaries linked by it don't consult the system `ld.so.cache`, so `dlopen("libvulkan.so.1")` — and `libEGL.so` —
fail with "cannot open shared object file" while `vulkaninfo` runs happily. Linking with the distro's own
compiler fixes it, recorded machine-locally in `~/.cargo/config.toml` (`linker = "/usr/bin/gcc-13"`) rather
than in the repository, because it is a fact about one box and not a project requirement. Two diagnostics were
wrong before that one was right: "wgpu logs nothing" (I had grepped `lib.rs`, not `vulkan/adapter.rs`) and "the
loader is never reached" (the binary under test was a stale build with no logger wired in).

Submit-only is how fast a thread hands work to the queue; on a device queue that is nearly free and says
nothing about latency. The read-back column adds the 8 MiB copy-and-map this harness pays so a CPU can look at
the result. That column is the finding: **~91 ms to complete and retrieve one 1080p evaluation pass, against
R1's whole 16.67 ms frame budget** — so a renderer that produces distances on the GPU and brings them back
each frame cannot meet R1 on this class of hardware no matter what the shader does. The design consequence is
to consume the field on the device (a march in the same queue, results never copied out per frame), which is
also what ADR-012's WGSL renderer has always implied.

What this is **not**: not a march (the CPU reference measured ~45.5 evaluations per pixel), not culled, not a
mid-range dGPU (docs/04's bar), and not hashed or comparable across machines — timings never are (ADR-017, and
R4 is informational by rule). **R1 stays unmet, and R2 has its first measured loop (2026-10-14,
`cargo run --release --example r2_edit_loop -p engine`, a CI step)**: terrain edits accumulate in a real `Slab`
in stages of 1 / 100 / 1,000 / 6,000 records — 6,000 is ten minutes at ten edits a second — and at every stage
the same 64×48 frame is marched (3 warm-up passes discarded, 21 timed), with P1.7's placement invariant
asserted per frame inside the loop: the newest edit's surface must sit at `HEIGHT - 0.5`, and it does at every
occupancy.

**What is asserted is a count, not a duration.** Doc 03's claim is that a frame walks a region's compiled
shape rather than scanning what the player edited, so the charged work per frame must be the same at 6,000
records as at 1 — and `Stats::evaluations` says exactly that: **18,284 evaluations per frame at every
occupancy from 1 to 6,000**, asserted equal, and equal on any machine because it is an integer the march
reports. The milliseconds are printed and not asserted, for a reason this machine demonstrated during
development: two runs of identical code put the 6,000-record stage's median at 3.44 ms and at 8.69 ms and its
read-back at 0.046 ms and 1.108 ms. A wall-clock ratio on a laptop measures whatever else was running, and an
informational CI step that can redden trunk on machine load is a worse instrument than no step at all. Three
runs of the shipped loop here put a stage median anywhere between 2.09 ms and 3.52 ms (widest within-run
spread 1.60×, 13–21% of R1's budget), so the milliseconds are reported as a range and one run is never quoted
as the number.

The term that *does* grow is the one an epoch commit pays: `slab.deltas()` read-back is **0.046–0.052 ms at
6,000 records** against 0.001 ms at 100 and 1,000 — under 2% of a frame here — and the 1-record row is below
timer
resolution, which is why the column is ns/record rather than a rate. This is the proportional part of S2's law
re-measured rather than cited.

The count was then falsified by coupling the march to the ledger: growing the stop tolerance by one part in
`1e7` per record moved it from 18,284 to 18,280 and the assert fired on a 0.02% change. The *first* plant tried
exited 0 and taught the more useful lesson — raising `max_steps` from 96 to 189 changed the charge by nothing,
because every ray in this frame converges long before the cap, so the cap is not a term the frame pays. A
checker is only as live as the violation it can express, and the two firings that came before the plant were
both real: the 1.3× median bar failed at 2.50× under machine load (which is what demoted the milliseconds to
informational), and the count itself fired at 19,712 against 18,284 when the fixture's edit walk climbed into a
second `z` layer — the last edit then sat a full unit higher above the plane, genuinely changing the marched
scene. Held to one layer (96×64 columns, more than 6,000 voxels), the count is exactly constant. So the
assertion is live, and it is evidence about occupancy only because the scene was pinned still first.

**The same loop, marched by the shader (2026-10-14, `cargo run --release --example device_edit_loop -p gpu`,
a CI step).** The reason the loop above was the reference path was that `march_rays` had no ledger in front of
it. It now does: the same `Slab`, the same `deltas()` read-back, the same doc 02 placement law, and the same
64×48 grid handed to `interp.wgsl` instead of `render::raster` — measured on the MX250 over Vulkan. A device
march reports two terms that survive being asserted, and both are integers or geometry, never a duration. The
count of rays that find surface is **3072 at 1, 100, 1,000 and 6,000 records**, asserted equal across that
6,000× change in occupancy; and at every one of those occupancies the device's depths agree with the
reference's ray-for-ray — **0 hit/miss disagreements, worst depth deviation under 0.002**, the anchor
`on_device_march` uses, which is what keeps a dispatch the driver folded away from arriving here as a frame.
The centre pixel returns **7.5129** at every stage, where placement says 7.500 and the same grid over
unedited terrain returns 9.0000 — so an accepted edit arrives in a *device* frame at the distance doc 02 gives
it. That assert is live: marching the unedited tree inside the loop fired it at occupancy 1 ("the device put
the newest edit at 9 where doc 02 says 7.5") and exited 101.

Two things the device numbers do not say. The flat hit count is a **guard**, not the edit's signature: the
terrain plane is 2048 units wide, so all 3,072 rays find surface with or without an edit, and 3072 against
3072 only proves the frame never went empty — which is why the visible-edit claim is carried by the depth
under the eye instead. And the milliseconds here moved more than the CPU's did: two runs of identical code put
the stage medians at 3.679–3.900 ms and then at 7.222–20.559 ms, a 5.6× swing on the last stage of a laptop
that was doing other things. Nothing timing-shaped is asserted, for that reason (ADR-017).

**The CI leg reproduces it on a second, different device (run `38613746327`, 2026-10-14).** The Linux runner
prints `SKIP device_edit_loop: no device (NoAdapter)` — the expected refusal, since no CI runner is a GPU
(ADR-026's constraint as docs/11 records it) — and the ARM64 macOS runner opens `Apple Paravirtual device
[Metal, IntegratedGpu]` and reports **3072 rays at every occupancy and the same 7.5129 centre depth** the
NVIDIA laptop does, with stage medians 7.217–7.691 ms across that 6,000× occupancy change. Two vendors, two
APIs, one geometry answer, and that is the part worth naming: the assertable terms travel between devices
while the milliseconds never do. The paravirtual adapter is not this doc's mid-range dGPU, so the run says
nothing about R1's budget.

**One upload, bound per frame (AS4, 2026-10-14, `cargo run --release --example as4_upload_reuse -p gpu`, a CI
step).** `Interpreter::march` has always re-uploaded the node records and the parameter pool, which is right
for a probe and wrong for a frame: AS4's clause is that a loaded shape reaches the queue without a per-frame
allocation or parse. `upload` + `march_uploaded` now split it, and the asserts are the two kinds a device can
honestly carry. The reused frames match the per-frame path **bit for bit** (`f32::to_bits` equality on all
3,072 depths, plus equal hit counts), and that equivalence is falsified inside the binary by uploading a second
shape with one parameter moved — radius 2.0 against 3.5 — and requiring the depths to diverge; they do, first at
ray 0, 14.856176 against 14.850224. The cost term is a byte count, not a duration: the 80-byte shape is handed
over once where the per-frame path hands it over six times, and the 400 bytes never re-uploaded are 0.000% of a
frame's bus traffic at 64×48 (98,304 ray bytes) or at 1080p (66,355,200 ray bytes, DERIVED from the same 32
bytes per ray). That is the finding worth more than the clause it closes: **the shape is never the per-frame
cost — the rays are**, and a real renderer generates them on the device and uploads neither. Consistent with
that, the medians here were 11.123 ms reused against 11.662 ms per-frame upload, i.e. indistinguishable on a
machine whose runs move by multiples of that anyway.

What remains of the caveat is structural, and it is OQ-55's: the edit still reaches the device by **rebuilding
the tree on the host**, because A-07 has no instance/translate node in which to place a voxel inside the
grammar. So R2 is now measured on a renderer that keeps the ray loop on the GPU, and **still not met as a
frame** — R1's 16.67 ms at 1080p is unmet. The ten-minute span, which used to be the other half of that
sentence, is no longer derived: see the next section.

**R2's ten minutes, run (2026-10-14, `cargo run --release --example r2_ten_minute_span -p engine`, a CI step).**
`r2_edit_loop` measured the frame at four occupancies and then said, in its own output, that a 10-minute session
at 60 fps is 36,000 frames and was *derived from the flat median, not run*. That sentence is now false in the
way it matters: 36,000 consecutive frames of the 64×48 reference march have been marched, with one terrain edit
appended to a real `Slab` every sixth frame (ten a second at sixty — both numbers from the gate's own words, not
from tuning), so occupancy grows **continuously** rather than in four stages. Every frame is asserted, not
sampled: **18,284 charged evaluations on all 36,000 frames**, compared against frame 1's value at the moment each
frame is marched, plus doc 02's placement under the eye on every frame, plus the slab handing back exactly the
records it took. The O(1) hot path therefore holds across the span R2 names, on the reference path.

Three runs on this laptop put the median at 1.94, 2.40 and 3.54 ms, and the p99.9 at 8.51, 14.27 and 108.03 ms
— the *same* code's tail ranging 12.7× between runs, which is why the gate's claim rides on the integer column
and no duration is asserted anywhere (ADR-017). The span's own wall clock is the honest way to say what
"stable" would mean if a wall clock could gate it: p99.9 came out 4.40×, 5.95× and 30.5× the median over 36,000
samples, and that tail is this machine's load, not the ledger's.

**The CI leg reproduces the integer on three machines and the durations on none of them (run `38621355849`,
2026-10-14).** Each of the three `gates` jobs ran the step and printed the same line the laptop does — **18,284
evaluations per frame, asserted equal on all 36,000** — while their own timing columns disagree by an order of
magnitude: median 0.68 ms on x86-64 Linux (p99.9 0.86, a 1.27× tail), 0.69 ms on ARM64 macOS (p99.9 2.39, 3.47×),
0.97 ms on x86-64 Windows (p99.9 1.68, 1.73×), against 1.94–3.54 ms medians and a 8.51–108.03 ms tail here. Two
things follow, and the second is the reason the loop asserts nothing about time: the *count* is a property of the
march, so it travels between vendors untouched, and the *tail* is a property of the box — the quiet CI runners
produced the tightest tails, the busiest laptop the loosest, with the same 36,000 frames and the same code in all
four cases. A span that would gate on milliseconds would have reddened trunk on a laptop and passed on an
emulated CPU, which is the wrong way round for a claim about a player's frame.

The instrument was falsified twice successfully and once usefully, and the useful failure is the third. (1)
Letting the edit walk climb into a second `z` layer from record 3,000 — the scene changes, and the charge assert
fired at **frame 18,001 with 19,712 against 18,284 at 3,001 records**. (2) Marching `terrain(None)` after frame
20,000 — fired at frame 20,001 with **16,184**, because dropping the union changes the charge too. (3) Both were
caught by the *charge* assert, so a third plant was built to be charge-neutral and reach the placement assert
alone: shift the marched centre one whole voxel along `x` after frame 20,000. **It exited 0 — the plant was
inert.** It shifted the terrain *and* the camera by the same amount, and the march is camera-relative, so the
sphere stayed exactly under the eye and nothing about the frame changed. The honest residue: on this instrument
the placement assert has not been shown to fire on its own, because every violation tried so far also moves the
charge, and the charge assert is evaluated first. The equivalent depth assert *has* been falsified where it is
the only live term — `device_edit_loop`'s in-loop march of `terrain(None)` fired the depth check at occupancy 1
and exited 101, because there the hit count was saturated and could not catch anything. What would make this
loop's placement assert independently provable is a scene where the two asserts disagree, which is a fixture
question, not a reason to weaken either check.

Still open on R2, stated plainly: this is the **CPU reference** path at 64×48, so it is the shape of a frame and
not a frame budget; the device loop (`gpu`'s `device_edit_loop`) asserts the same invariants at four occupancies
and has not run 36,000 frames; and R1's 16.67 ms at 1080p stays unmet, gated on culling inside the GPU path
rather than on anything this loop measures.

**buildbox's zero adapters, so far (2026-10-14).** Four hypotheses are dead, each eliminated by a run rather
than by reasoning: the driver (580.178.04 loaded, `nvidia-smi` answers, `/dev/nvidia0` and `/dev/nvidia-uvm`
exist), permissions (`ian` added to `render`, and `vulkaninfo` sees both devices in the same session), loader
layers (`VK_LOADER_LAYERS_DISABLE` changes nothing), and a missing surface extension (`VK_KHR_surface`,
`VK_EXT_surface_maintenance1` and `VK_KHR_surface_protected_capabilities` are advertised with `DISPLAY` and
`WAYLAND_DISPLAY` both unset). It is not a build gap either: `LD_DEBUG=libs` shows the probe **does** load
`libvulkan.so.1`, while `VK_LOADER_DEBUG=all` emits nothing, so no `vkCreateInstance` is ever attempted.
`wgpu-hal` therefore declines the backend between loading the library and creating an instance, and it reports
why on a `tracing` channel the probe has no subscriber for - which makes installing one the next line of code
rather than another guess. Until that reads out the reason, the honest form of the claim is: a real desktop GPU,
enumerated by the Vulkan loader on this headless box and not by wgpu - and note CI's own Linux runner reports
the same empty list, so the pattern is not peculiar to the NVIDIA stack.

One more thing worth recording before anyone spends a build on it: grepping `wgpu-hal 27.0.4` for a warn or
error on the backend-decline path finds none, so installing a `tracing-subscriber` may print nothing at all. The
next experiment is to construct the hal's Vulkan platform instance directly and watch it fail - not another logger.

### A frame can be drawn into memory the caller owns (2026-10-12)

AGENTS.md §3 bans allocation churn during gameplay frames and doc 09 promises AS4's "no allocation
churn", and neither had a number until now. `render::raster` gained `render_into`, `render_culled_into`,
`render_bnb_into` and `render_bnb_with`, which draw into a `Frame` the caller keeps; `Frame::prepare`
clears and reserves without ever shrinking, so a frame rendered at a stable resolution stops allocating
in its own buffers after the first one. `examples/allocation_audit.rs` prints the evidence and names its
own limit: counting allocations would take a `#[global_allocator]`, that needs an `unsafe impl`, and
`unsafe_code = "forbid"` refuses it - so what is *measured* is buffer capacity that does not move, and
what is *read off the code* is the bound table, two vectors per call unless the caller hoists it, which is
why `render_bnb_with` exists. Six tests in `tests/frame_reuse.rs` hold the part that matters more than the
memory: a reused frame is the same picture, pixel for pixel, at every entry point.

### The first early-out exists, and spike S1's 3× estimate was the wrong variable (2026-10-12)

`crates/render/src/cull.rs` is the cheapest possible answer to the miss share: one conservative sphere per
subtree, derived by set relation — a union is covered by both children, a smooth union grows that cover by
`k` because `smin` rounds outward by at most `k`, a subtract or intersect takes its **first operand's**
bound because the result is a subset of it, and a mirror covers the child and its reflection. Then one
segment test per ray. `render::raster::render_culled` is the same march with that test in front of it, and
`cargo run --release --example cull_probe -p render` prices it on a 64×48 grid, 96-step budget, spheres of
radius 0.4, two arrangements of the same counts:

| Arrangement | Spheres | Bound r | Evals plain | Rays skipped by the root bound | Rays skipped by the descent | Evals after the descent | Nodes unpruned | Nodes pruned | Work avoided |
|---|---|---|---|---|---|---|---|---|---|
| cluster | 1 | 0.40 | 294,544 | 99.9% | 99.9% | 16 | 16 | 16 | 0.0% |
| line | 1 | 0.40 | 294,546 | 99.9% | 99.9% | 18 | 18 | 18 | 0.0% |
| cluster | 8 | 2.20 | 291,594 | 94.0% | 98.8% | 234 | 29,805 | 2,705 | 90.9% |
| line | 8 | 4.60 | 291,164 | 71.4% | 98.6% | 284 | 98,280 | 2,824 | 97.1% |
| cluster | 32 | 4.96 | 279,551 | 70.9% | 94.3% | 1,439 | 535,311 | 59,449 | 88.9% |
| line | 32 | 19.00 | 287,112 | **0.0%** | **97.1%** | 648 | 1,268,568 | 23,570 | 98.1% |
| cluster | 128 | 16.09 | 278,136 | 44.3% | 93.8% | 1,656 | 3,291,795 | 81,518 | 97.5% |
| line | 128 | 76.60 | 287,112 | **0.0%** | **97.1%** | 648 | 5,134,680 | 86,526 | 98.3% |
| scatter, authored order | 32 | 33.11 | 284,992 | 0.0% | 96.3% | 1,120 | 1,655,892 | 59,141 | 96.4% |
| scatter, median split | 32 | 22.98 | 284,992 | 0.0% | 96.3% | 1,120 | 1,655,892 | 38,569 | 97.7% |
| scatter, authored order | 128 | 138.01 | 284,800 | 0.0% | 96.2% | 1,024 | 6,474,195 | 207,263 | 96.8% |
| scatter, median split | 128 | 84.69 | 284,800 | 0.0% | 96.2% | 1,024 | 6,474,195 | 115,304 | 98.2% |

### Region streaming: what it saves is the test, not the march (2026-10-12)

`crates/render/src/stream.rs` renders a world of regions - one compiled shape per region cell, placed at
`cell * 4096` in gate C1's arithmetic - and the measurement corrected the expectation before it was
published. A region's enclosure is already tested per ray, so **the charged field work does not grow with
the size of the world at all**: at 1, 3, 7, 21 and 64 regions around the camera, the frame costs the same
1,248 evaluations and produces byte-identical pixels and depths, with `plan` selecting one region every
time. What streaming removes is the **bound test**: one sphere check per region per ray, so the arithmetic
of a big world lives in `regions x rays` - 3,072 checks for one region against 196,608 for 64 - and that
is the column planning flattens. Saying it saves march work would have been wrong, and the number was
checked before the sentence was written.

Why the module exists at all: **A-07 has no translation node**. The kinds tile (`repeat`), reflect
(`mirror`) and blend, but none can place a subtree at an arbitrary offset, so two authored shapes in
different cells cannot be joined into one `Tree`. The frame is therefore the **minimum of the regions'
fields**, which is legitimate for the reason `min` combinators are: a minimum of 1-Lipschitz fields is
1-Lipschitz, so sphere tracing it cannot step through a surface. The equivalence is tested rather than
asserted - `tests/stream.rs` requires a one-region frame to equal the single-tree renderer pixel for pixel
and depth for depth with the same charge, requires a region out of range to be invisible whether it is
planned in or out, and requires a seven-cell world to produce the same frame as a one-region plan.

One claim this section had to take back: an attempt to make the demo a row of three cells produced zero hits
at 4,096 units, was reverted, and looked like a precision or march failure. It was neither.
`examples/far_scale_probe.rs` walks the view axis out to 12,288 units and hits in **one step** at every
distance - 8, 64, 512, 4,096, 8,192, 12,288 - including at the finest 1e-3 tolerance, and `march` agrees, so
the march, the bound and the subtraction this probe hand-rolls are all sound at region scale - and that part
survived the 2026-10-13 correction below, because the ordering bug only bites where an `f32`'s ulp
approaches the feature's own size, which is nowhere near 12,288 units. The angular
size is the whole story, and it is now a test: a 1.6-unit object 4,096 away subtends 0.0004 radians in a
0.5 half-height frustum, about six parts per million of the grid, so **the empty frame was the correct
render**; give that cell a 400-unit feature - the scale region content actually is - and 16 of 768 pixels
meet it where the disc formula predicts 17, the centre ray on a chord at 3,712.8 against the 3,715 the
formula gives. Region streaming therefore needs no precision work, and the demo now carries it: the boot composes three cells and prints
`region world     : 3 cells +4096 units apart, all planned - 259 of 3072 rays found surface (8.4% of
                 the frame), 174881 charged evaluations against 174138 for the terrain cells alone,
                 1039 proved empty by their bounds; adding the nearest cell re-lit 0 pixels and lost
                 none, worst depth shift 0.0 units
[OQ-56](13-risks-open-questions.md) records the diagnosis and retires the camera-relative worry. Whether the
grammar should grow an instance/translate kind is [OQ-55](13-risks-open-questions.md) - a
contract decision with a cost attached (interpreter arm, validator, WGSL twin, asset graph). Until it is
answered, streaming is a rendering pattern rather than an asset feature. `cargo run --release --example
region_cost -p render` prints the table above; nothing in it is a frame time, and gate R1 still waits on a
GPU.

### The licence is an authoring choice, and the probe now prices it (2026-10-12)

The same scene written twice - identical spheres, identical tree shape, identical camera, and every branch
a `smooth union` with `k = 0.2` in one version and a plain `union` in the other - is the difference between
having a hierarchy and not having one:

| Arrangement, 128 spheres | Branch kind | Rays skipped by the root bound | Rays skipped by the descent | Evals after the descent | Node work avoided |
|---|---|---|---|---|---|
| line | plain union | 0.0% | **97.1%** | 648 | 98.3% |
| line | smooth union | 0.0% | **0.0%** | 287,128 | 0.0% |
| cluster | plain union | 44.3% | **93.8%** | 1,656 | 97.5% |
| cluster | smooth union | 32.8% | **32.8%** | 180,662 | 0.0% |

A blend poisons the skip precondition for as far up as it reaches, so **one blended branch at the root costs
the whole hierarchy - a factor of 443 on the line row** - and it costs the per-step skip too, which is why
the last column goes to zero rather than merely shrinking. The frames stay byte-identical in every row, so
this is a price, not a bug. `cargo run --release --example cull_probe -p render` prints the four blended
rows under their `cluster/blend` and `line/blend` labels.

Two consequences worth stating where content authors will read them. **For the asset pipeline (doc 09),**
the blending decision is a frame-cost decision, not only a look decision: a scene of plain unions over
sphere-like features can be culled hierarchically and a blended one cannot, and nothing in the current
grammar says so in the file itself. **For the demo,** the boot's own five-node scene is the refused case -
its root union sits above a smooth union - which is why the boot prints `root licence refused` beside two
identical culled counts rather than a saving.

Whether the licence is *too* conservative is [OQ-53](13-risks-open-questions.md)'s neighbour and now
**OQ-54**: a smooth union's enclosure is grown by `k`, while the dip `smin` can produce is bounded by
`k/4`, which suggests the grown bound may satisfy `field >= distance to bound` after all. The reasoning is
sketched and deliberately not acted on - the failure mode of being wrong is deleted geometry, and the
current refusal is what the tests assert. The lattice question was measured, then derived, then acted on:
(`tests/cull.rs::the_smooth_union_licence_is_measured_and_not_yet_relaxed`): four scenes on a 41-cubed grid
at 0.25 spacing plus dense 0.005 sweeps along the blend axis and both waist lines, and every blended scene
keeps `field >= distance to its grown enclosure` with a margin of **exactly +k** at the worst sample, zero
violations bigger than the render tolerance and zero hit-band points outside the bound - the growth pays for
the dip with `3k/4` to spare, and the licence bit has been **granted** (`first_ok && second_ok`), which is
sound by derivation and not merely by sampling: outside the grown sphere
`field >= min(a,b) - k/4 >= D_cover - k/4 = D_grown + 3k/4`. The licensed plain union, by contrast, reaches
-0.000000 at the far corner - its own bound is tight to `f32` rounding - which is
why the test measures violations relative to `surface` rather than against zero. Evidence, still not a
proof: a lattice cannot speak for every point in space, so OQ-54 stays open and the refusal stays in place
the inequality is now derived, and **the 443x did not move with the grant - the claim this section got wrong,
retracted in place.** A blend's zero set lies *between* its children, outside both child spheres, so no
descent may decompose a blend and its grown cover sphere is the tightest statement it can make; the grant
buys pruning below a parent plain union (the boot's own shape) and nothing else.

**The saving then appeared where the derivation said it could, in an arm a blend can take.** `smin(a, b, k)`
equals `min(a, b)` exactly when `h = max(0, 1 - |b - a| / k)` is zero - once the children are `k` apart the
blend has nothing left to add - so a second child whose licensed bound exceeds `a + k` cannot have bent the
result and is skipped. The comparison is strict, because `distance >= a + k` after `f32` rounding could sit a
step below the true separation and `h^2 * k / 4` is then tiny but not zero: an overestimate of the field is
exactly what a march cannot afford. Blended node work avoided, measured on the same four rows that used to
read 0.0%:

| Blended arrangement (k 0.2) | Spheres | Visits unpruned | Visits pruned | Work avoided |
|---|---|---|---|---|
| cluster | 32 | 688,086 | 548,603 | 20.3% |
| line | 32 | 1,277,892 | 820,930 | 35.8% |
| cluster | 128 | 3,898,950 | 1,336,864 | **65.7%** |
| line | 128 | 5,172,420 | 2,814,446 | **45.6%** |

Frames byte-identical throughout, and the belt is deliberately asymmetric:
`the_k_aware_arm_skips_a_second_child_the_blend_cannot_have_touched` also carries a tight-blend scene
(children 0.2 apart under a strength of 1.5, never `k` apart anywhere the camera reaches) and requires it to
avoid **nothing** - so a blanket skip and a dead skip both fail. The ray-level half of the earlier difference
stays unbuyable, and doc 09 can now say precisely what an author gets: a proved skip inside each blend, and
no hierarchy through one.

### A far cell rounds geometry away, and the streaming path had the subtraction backwards (2026-10-13)

Gate R5 says a camera can zoom to 1e-6 world units on a pig snout with no polygonation. The test the crate
had for it (`scene.rs::scale_changes_little_but_precision`) marches the scene **at the origin**, which is
the easy case and not the claim: the claim is about a feature a billion units from world zero.
`examples/deep_zoom.rs` now runs one snout - a body sphere of r 1.0 with a bump of r 0.02 in front of it -
through a 64x64 grid at `surface = 1e-6`, four ways, and compares every pixel's hit depth against the local
baseline. Measured on x86-64 Windows:

| regime | hits of 4,096 | max depth delta | mean normal angle (rad) | worst normal angle (rad) |
|--------|---------------|-----------------|-------------------------|--------------------------|
| local (the origin) | 4,096 | 0.000000 | 0.000193 | 0.000846 |
| far, camera-relative | 4,096 | 0.000000 | 0.000193 | 0.000846 |
| far, absolute | 4,096 | 0.004965 | 0.071092 | 0.145554 |
| far, region cell (244,140) | 4,096 | 0.000000 | not shaded | see below |

Row 3 is docs/02's rule stated as a price: the same shape at the same distance from its camera, with the
world coordinate carried into the field instead of cancelled out of it, reports depths three-and-a-half
thousandths off and normals a quarter-radian wrong at their worst. The number to watch is the **normal**,
not the depth - a polygonated snout still reports a plausible distance, which is exactly why "the depth
looks fine" is not R5's evidence.

Row 4 is the finding, and it was a bug in shipping code at the time of writing. `stream` placed its samples
by forming the world point from the absolute camera position and subtracting the cell origin afterwards,
which is row 3's order with an extra step. At cell 244,140 the cell origin is 999,997,440 and the `f32` ulp
there is 64 world units, so `camera.origin + dir * t` for any `t` under 32 rounded back to
`camera.origin` exactly and the subtraction returned a flat zero: the snout collapsed along the axis the
cell lay on and **1,280 of 4,096 pixels missed** (2,816 hits, 0.034 depth delta). The fix is the ordering
docs/02 already required - `Region::relative_origin` subtracts the cell from the eye *first*, where IEEE-754
gives the difference exactly for two floats of comparable magnitude (Sterbenz), and `t` accumulates in the
region's own frame; `enclosure_relative` does the same one level up, so no bound is ever a world coordinate.
After that, row 4 is row 2: all 4,096 pixels hit and every depth matches the local baseline bit for bit.
`tests/stream.rs::a_cell_a_billion_out_draws_the_same_picture_as_the_same_cell_at_zero` pins it as pixels,
depth, charged work and hit count.

The sweep the same example prints next is the one OQ-58 needs, and its headline is that the fixed path is
**distance-invariant**. Re-rendering the snout at nine cell indices from 0 to 2^28 + 1 — world zero to
1.0995e12 — every row returns 4,096 of 4,096 hits and a max depth delta of exactly 0.000000 against the
local baseline. Camera-relative ordering makes the *frame* independent of where in the world the cell sits,
because the eye and the cell origin cancel exactly and `t` never sees a large number. What degrades instead
is **placement**, which is a different failure and a quieter one:

| cell index | world origin | origin − exact `i64` (voxels) | `f32` ulp there (voxels) |
|------------|--------------|------------------------------|--------------------------|
| 1 | 4,096 | 0 | 0.0005 |
| 4,096 | 16,777,216 | 0 | 2.0 |
| 244,140 | 999,997,440 | 0 | 64.0 |
| 16,777,216 | 68,719,476,736 | 0 | 8,192.0 |
| 16,777,217 | 68,719,476,736 | −4,096 | 8,192.0 |
| 268,435,457 | 1,099,511,627,776 | −4,096 | 131,072.0 |

Three thresholds come out of that table, and the renderer is not the thing that sets them:

- **Sub-voxel placement stops at 2^24 world units** — cell index 4,096. Past it an `f32` origin cannot name
  a fraction of a voxel (the ulp is already 2.0 at 16,777,216), so the shape is drawn perfectly *somewhere
  near* where the ledger says it is. Nothing about the frame reports this: the hits and depths are identical.
- **The cell index itself stops rounding at 2^24** — from index 16,777,217 onward `cell as f32` is wrong, and
  here it is wrong by a whole region (−4,096 voxels), which is the first placement error in the table.
- **R5's own 1e-6 figure is only expressible within about 16 units of absolute zero** — an `f32`'s ulp crosses
  1e-6 near 2^3. Which is precisely why the gate's claim is meaningless without docs/02's rule: the tolerance
  is not what ran out at cell 244,140, the *coordinates* were, and camera-relative arithmetic is what makes
  1e-6 a reachable number a billion units from origin instead of an impossible one.

The sweep runs in CI on all three architectures (`Deep zoom, four coordinate regimes`, beside
`region_cost`, run `38332751812`), and the three runners print **the same table digit for digit** — the
region-cell row reads 4,096 hits and 0.000000 delta on x86-64 Linux, x86-64 Windows and ARM64 macOS alike,
as does the deepest row at world 1.0995e12. That is worth stating precisely because it is easy to over-claim:
these are presentation floats, nothing hashes them (AGENTS §4), so their agreement is **not** a P1.5 clause
and adds no device to S3's list. What it does establish is narrower and useful — the ordering rule is
compiler-independent, so a WGSL twin has no rounding excuse for disagreeing, and the geometry-loss bug above
was a real bug rather than one machine's arithmetic.

Two things this does **not** settle. R5 is a claim about an image on a display; what is measured here is the
reference evaluator's numerics on one machine, with the WGSL twin's own cross-vendor agreement (PR #1,
~2.4e-7) as the other half. And the correction lands on [OQ-56](13-risks-open-questions.md), whose
resolution retired "the camera-relative-placement worry" on the strength of a probe that only went to
12,288 units - a distance where the ulp is 0.00049 and this ordering mistake is invisible. The angular-size
conclusion stands (a sub-pixel object at four cells *should* render as nothing); the precision conclusion
did not, and the far-cell limit itself is now OQ-58.

### The descent moves inside the region, and there it is worth two orders of magnitude (2026-10-13)

`cull_probe` priced ADR-024's ray-level descent on a single tree; `region_cost` priced what a per-region
bound saves across cells. Neither priced the case a real world is made of, which is the two together: a cell
whose content is spread wide enough that **its own cover sphere swallows the frustum** — the `cull_probe`
rows that prove 0.0% of rays empty — so the coarse test does nothing and the children are all that is left.
`stream::render_bnb` asks the descent of each region's tree, in that region's coordinates from that region's
eye (the same ordering rule as the fix above; a bound table is a statement about the tree's frame, not the
world's), and `examples/stream_descent.rs` prices it against a counting table that prunes nothing. One cell,
a 64x48 grid, 3,072 rays, r 0.8 spheres:

| ridge | nodes | plain evaluations | with the descent | rays proved empty | node work avoided |
|-------|-------|-------------------|------------------|-------------------|-------------------|
| 9 spheres, spacing 1.2 | 17 | 229,888 | 5,632 | 0 → 2,336 | 85.6% |
| 9 spheres, spacing 8.0 | 17 | 283,884 | 876 | 0 → 2,948 | 97.4% |
| 33 spheres, spacing 8.0 | 65 | 283,884 | 876 | 0 → 2,948 | 96.4% |
| 65 spheres, spacing 6.0 | 129 | 294,912 | 11,904 | 0 → 2,948 | 96.0% |

Three readings, and the third is the one that changes the roadmap's arithmetic:

- **The prize is bigger than node work.** A ray the cell's cover sphere permits but no child can reach is
  proved empty *before the march starts*, so it is charged nothing: the evaluation column moves, not only
  the visit column. On the spread ridge that is 283,884 → 876, a factor of 324, with the frame asserted
  byte-identical in pixels and depth inside the example itself.
- **Cost stops tracking the world and starts tracking the neighbourhood**, which is the streaming path's
  whole claim, now priced at the node level too: 33 spheres cost exactly what 9 spread spheres cost — the
  same 876 evaluations — because 24 of them are simply never entered.
- **It is not free and not universal.** The tight row (spacing 1.2, where the cover sphere nearly fits) buys
  85.6% rather than 97.4%, and every frame pays a bound table per selected region — O(nodes), built once per
  frame rather than per ray, which is the honest cost of `stream` owning its own march loop - now priced rather than asserted: the probe's
  last column divides the evaluations removed by the bound records built, and it comes out between **1,319
  and 16,647 charged evaluations removed per record built** (worst for the densest cell, 283,008 removed
  against 129 records; best for the sparsest, the same 283,008 against 17). A frame that cannot afford 129
  derivations per region is not the frame this hierarchy is being sold to. Nothing here is
  a frame time: evaluations are not nanoseconds, and R1 still waits on a device (OQ-51).

`tests/stream.rs::the_descent_inside_a_region_avoids_node_work_and_moves_no_pixel` states the law the
structure has to obey, which is one-directional: it may convert permitted rays into skipped ones and may
never take a pixel, a depth or a hit away. The first version of that test asserted the *weaker* claim —
evaluations equal, saving confined to node visits — and failed, which is the measurement correcting the
expectation rather than the other way round.

### The same ceiling, spent across cells (2026-10-13)

The budget pass above lives in `raster`, which owns one tree. The frame the demo actually composes is a
minimum of several regions, and a ceiling across cells is a different claim unless it is the same law — so
`stream::render_budget` spends it the same way: the decision is made against the worst a ray can cost
(`max_steps + 1`) *before the ray starts*, tiles are walked in the order the caller asks for, and a pixel the
cut never reaches reports `Frame::NOT_DRAWN_DEPTH` / `NOT_DRAWN_RGB` rather than a plausible sky. It is not a
second march. The per-ray work moved into one private `march_ray` that the plain, descent, counting and
budgeted walkers all call, because two copies of the composite march is exactly the fork that would show up
as "the budgeted frame disagrees with the full one" — and the existing streaming tests are what prove the
extraction changed nothing: 56 render tests green before it and 57 after, with the boot's own three-cell
assertion still holding.

`tests/stream.rs::the_ceiling_across_cells_holds_and_every_pixel_it_drew_matches_the_full_frame` is the gate:
seven ceilings from zero to twice the full cost, each one never overran, every drawn pixel equal to the
unbudgeted frame in colour and depth, `rays == drawn + not_drawn` at every level, and an unlimited ceiling
byte-identical to the descent's frame in both buffers. What is *not* measured yet is the curve — which share
of a streamed frame's surface survives a given ceiling, in which walk order, on content at region scale.
That is `pop_in`'s question asked of cells rather than of one tree. It is now measured, and it came back
different from the single-tree case in a way worth stating:

| ceiling | order | charged | pixels drawn | rays proved empty | surface kept |
|---------|-------|---------|--------------|-------------------|--------------|
| 1/8 | centre-out | 13 | 22 | 21 | 0.8% |
| 1/8 | tile-major | 13 | 1,238 | 1,237 | 0.8% |
| 1/4 | centre-out | 129 | 43 | 29 | 11.3% |
| 1/4 | tile-major | 129 | 1,259 | 1,245 | 11.3% |
| 1/2 | centre-out | 343 | 106 | 60 | 37.1% |
| 1/2 | tile-major | 343 | 1,322 | 1,276 | 37.1% |
| 3/4 | centre-out | 564 | 158 | 75 | 66.9% |
| 3/4 | tile-major | 564 | 1,758 | 1,675 | 66.9% |

Same wide ridge (33 spheres, spacing 8.0), same 3,072-ray grid, same ceiling in each pair, fractions of that
frame's own unbudgeted cost. **Coverage differs by 11× to 56× and the share of surface kept does not differ
at all** - 0.8, 11.3, 37.1, 66.9 in both orders. The reason is the descent itself: it makes an off-subject
ray cost nothing (a proved-empty ray is charged zero), so a walk that spends its ceiling on cheap tiles
finalises most of the grid while barely touching the budget, and the expensive rays near the subject are the
same handful whichever tile it starts from. In `pop_in`'s single-tree case, where the scene fills the frame
and no per-region test throws the sky away, the same law produced the opposite advice - 25.1% of hits in the
first frame centre-out against 1.4% tile-major.

The practical reading for the demo: **on a streamed frame with the descent working, walk order is a decision
about how fast the *background settles*, not about when the subject appears.** Both are worth having, and
neither is free to guess - which is why the order stayed policy rather than becoming a default tuned to one
fixture.

Two things follow from the same law and are stated here because they were learned the hard way, twice.

- **A ceiling below one ray's worst case draws nothing, and that is correct.** The cut is decided against
  `max_steps + 1` before the ray starts, so a budget smaller than that cannot afford a single ray: the frame
  stays entirely at `NOT_DRAWN_*` and charges zero. This is not a degenerate corner to paper over - it is
  what makes the ceiling trustworthy as a gate rather than a statistic. It also means a ceiling must be
  *specified in ray charges*, not as a fraction of a frame: `full/8` of a 32×24 grid is less than one ray,
  and a caller who picks that number will watch a renderer do nothing and assume it is broken.
- **Refinement across frames is landed, and the ceiling's unit is what makes it sane.** `stream::render_budget_refined`
  carries the pixels an earlier frame decided: they cost nothing this frame and stop being a shortfall, and the
  converged frame is the unbudgeted one, depths *and* pixels, asserted in both walk orders by
  `tests/stream.rs::refining_a_streamed_frame_at_one_ceiling_converges_to_the_frame_it_was_cut_from`. What took
  two failed attempts to see is that the ceiling must be expressed in **ray charges**, not as a fraction of the
  frame, because the worst-case gate stops the frame as soon as one more ray could not possibly fit: at
  `4 × charge` this fixture's 768-ray grid finishes in a single frame (nothing to refine), at `full/8` it
  affords no ray at all (nothing drawn), and at one charge per frame the gate admits exactly one non-trivial
  ray and the grid takes hundreds of frames. `refine_probe` prints that progression per frame - at one charge
  frame one settles 22 pixels centre-out and 1,238 tile-major on the wide ridge, the cheap proved-empty
  majority arriving first and the expensive frontier dribbling in behind it - and it is why the
  `frames to finish` column `stream_descent` once *derived* deserves a stronger word than imprecise: its estimate of about
  three frames was wrong in both directions: at 1/8 the carry takes **57 frames in either walk order** where the derived
  figure predicted 140 centre-out and 3 tile-major, and at 1/2 and 3/4 it takes 3 and 2 frames. Two facts came
  out of measuring rather than estimating, and neither is the one expected. The frames to finish do not
  depend on the walk order at all - 57 centre-out, 57 tile-major at the same ceiling - even though the pixels
  settled per frame differ by 56x, so the order decides *which* pixels arrive, not how long the grid takes.
  And the carry costs nothing extra: cumulative charge across all 57 frames is 876, exactly what the single
  unbudgeted pass charged, asserted at every ceiling and in both orders. `stream_descent` now prints that carry **measured** (the derived column and its 335-pixel footnote are both gone), per ceiling and
  per walk order, with the convergence and the cumulative charge asserted rather than inferred.

The demo now performs the carry itself, as step 9b of `cargo run -p engine`: one streamed frame of the
shipped three-cell world is refined at a ceiling of **one ray charge** (`max_steps + 1` = 97) until nothing
is short of, and the settled frame is asserted inside the binary to be pixel-for-pixel and depth-for-depth
the picture the unbudgeted descent march found. Measured on that world: **813 frames**, cumulative charge
**57,665 — exactly the single pass's**, with 2,260 rays proved empty along the way. Two things came out of
running it on real content rather than on the synthetic ridge.

- **The total work is free; the wall of frames is not.** Refinement adds no charged work whatsoever - the
  figure lands on the single pass's number to the unit - but it takes 813 frames to place the picture, so a
  ceiling is a statement about *latency to a complete frame*, which is the thing a pitch demo is judged on,
  not about cost.
- **The ceiling is charged before the reachability test, so free rays are not harvested.** The boot's frame
  has 2,260 rays the bounds can prove empty at zero cost, yet frame one settles a single ray: the gate asks
  "can the next ray afford its worst case?" before asking "does this ray hit anything?", and once the
  answer is no the frame stops - leaving thousands of zero-cost pixels uncollected. Reordering those two
  questions is sound (a proved-empty ray charges nothing, so the ceiling cannot be breached by it) and would
  turn 813 frames into something like `3072 - 2260` over the same charge, but it changes the *prefix*
  property the budget tests pin - which pixels are drawn at a given ceiling - so it is a decision to make
  with those tests in hand, not a quiet edit. It is filed as **OQ-60**. The shape of the trade is measured now (`examples/refine_probe.rs`): on a grid whose whole frame costs 876 against a charge of 97, ceilings of 1, 2, 4, 8 and 16 charges settle it in 125, 9, 3, 2 and 1 frames - every row charging exactly the same 876 in total, asserted - while a sub-charge ceiling never settles at all. Frame one at a single charge takes 22 pixels of 3,072 even though 96.0% of the grid is proved free, which is option (a)'s whole prize: the same work, a much better first frame.

The pop-in question got the same machinery, and it needed its own denominator (`examples/region_pop_in.rs`).
P1.4's clause is about the picture a re-entered region contributes, so the instrument attributes pixels by
rendering the composite with and without that cell and differencing the depths: a 400-unit hill one cell away
owns **70 of 3,072 pixels (2.3%)** - OQ-56's angular-size lesson returning in demo form. At a one-charge
ceiling the first frame settles 8 pixels centre-out or 224 tile-major, and **not one of them belongs to the
arriving region, in either order**; its own footprint reaches 50% at frame 41 centre-out against 75
tile-major, completes at 76 against 110, and the whole frame is complete at 111.

So two things hold that the frame-average numbers hid. "The frame is done" is the wrong question, because
what arrives first is the cheap, provably-empty majority. And the walk order that made *no* difference to
frames-to-finish in `stream_descent` makes roughly a **2x** difference to the arriving region's own surface -
which is the reason OQ-60's reorder should be judged on a region-attributed measure rather than a frame
average: the two disagree about what a good first frame is.

### The budget pass: a ceiling that holds, and what it actually costs (2026-10-12)

### Refinement: the same ceiling spread across frames, losslessly (2026-10-13)

`render_bnb_refined` is the budget pass made incremental — pixels already in the frame stay, the ceiling is
spent only on what is still `NOT_DRAWN_DEPTH`, and the walk resumes where the last frame's ceiling stopped.
The property that makes that legitimate is asserted, not assumed: finishing a frame over several passes
produces **the same image byte for byte** as one pass at no ceiling, and the cumulative hits end exactly at
the single pass's (`tests/budget.rs`, which also checks that each pass respects its ceiling and that
coverage strictly grows). A budget that changed the picture would be a rendering mode, not a schedule.

That turns R3/P1.4's pop-in from an unanswerable question into a countable one. At a ceiling of 1/4 of the
culled frame's charged work, the demo region is complete in **5 frames**, with the first frame already
holding **25.1%** of the hits under `Scan::CentreOut`; the same ceiling with `Scan::TileMajor` takes **9
frames** and its first frame holds **1.4%**. Order is not a detail of the cut, it is the difference between
a subject appearing immediately and a subject arriving last. What is still not answered is the word
"visible": that needs a display, and the display needs the device decision (OQ-51).

### 1080p measured, derived, and what a ceiling buys at that resolution (2026-10-13)

`cargo run --release --example frame_cost -p render` runs the demo's own eleven-record scene at three
grids and then at 1920x1080 under ceilings, and it separates the three kinds of number AGENTS.md §7
insists on keeping apart:

- **Measured, on this CPU:** charged evaluations are flat per pixel - 78.02, 78.08, 78.15 per pixel at
  96x72, 192x144 and 320x240 - and the rate is ~25.5-26.1 ns per evaluation. Resolution costs work, it
  does not change the work per pixel, which is the law every budget below is extrapolated from.
- **Derived, not rendered:** an unbounded 1080p frame is therefore ~1.62e8 charged evaluations, ~4.1 s on
  this machine, ~248x gate R1's 16.7 ms. Labelled as arithmetic on measured numbers, because R1 is a GPU
  claim and there is no GPU in this repository's runtime path.
- **Measured at 1080p under ceilings** (this is the table AS5 needs and did not have):

| Ceiling (charged evaluations) | ms | Pixels drawn | Hits kept |
|---|---|---|---|
| 2,000,000 | 378.3 | 126,493 | 36.2% |
| 8,000,000 | 806.0 | 279,383 | 67.3% |
| 20,000,000 | 1,220.3 | 467,894 | 89.1% |
| 45,000,000 | 2,164.0 | 758,375 | 100.0% |

A ceiling is therefore not a safety margin, it is the dial between "the frame is mostly sky" and "the
frame kept its subject", in the same unit the compiler rejects on. Choosing a row is a frame-time decision
- which is gate R1's job, and R1 has no GPU - so `frame_cost` publishes the trade and stops short of
picking. The last row is also the honest shape of the cull: the first 2.07 million pixels' worth of work
buys the *second* 45 million evaluations' worth of nothing, because 1.3 million of those pixels are sky the
bounds already answered.

`render::raster::render_bnb_budget` draws as much of a grid as a stated number of **charged evaluations**
pays for, and the unit is deliberately the same one `Stats::evaluations` reports - so a budget and the
number it produced are comparable with no translation step. The decision is made against `max_steps + 1`,
the worst a single ray can cost, *before the ray starts*, which is what turns "about this much work" into
a bound that needs no measurement to believe. Pixels past the cut carry
`Frame::NOT_DRAWN_DEPTH`/`NOT_DRAWN_RGB` and `Stats::not_drawn` counts them: "never marched" and "marched
and found nothing" stay different facts, because the whole point of a budget pass is knowing what it
sacrificed.

`cargo run --release --example budget_probe -p render` prices the curve on a 64x48 grid (blend at the
centre, two distant spheres under a plain union; the unbounded hierarchy render costs 182,144 charged
evaluations and finds surface on 1,000 of 3,072 rays):

| Budget | Charged | Ceiling | Pixels drawn | Undrawn | Surface kept |
|---|---|---|---|---|---|
| 1/16 | 11,328 | 11,384 | 203 | 2,869 | 0.0% |
| 1/8 | 22,752 | 22,768 | 322 | 2,750 | 0.0% |
| 1/4 | 45,504 | 45,536 | 673 | 2,399 | 0.0% |
| 1/2 | 91,030 | 91,072 | 1,511 | 1,561 | 36.4% |
| 3/4 | 136,544 | 136,608 | 2,573 | 499 | **100.0%** |
| 7/8 | 159,296 | 159,376 | 2,822 | 250 | 100.0% |
| full | 182,048 | 182,144 | 3,068 | 4 | 100.0% |
| 2x | 182,144 | - | 3,072 | 0 | 100.0% |

Two readings matter, and the second one is now a measurement rather than an argument.

**The shape of the loss is not linear in pixels**: at a quarter of the work nothing visible survives,
because the walk is tile-major from the top left and the top-left tiles are sky; at three quarters the
frame loses 499 pixels and **every hit**, which is a 25% cut in charged work for no visible change.

**So the visit order is the cheaper lever, and `render::raster::Scan` makes it an argument.** The same
probe run, same ceilings, same field, same bound, same march - tile-major against centre-out (tiles sorted
by integer squared distance from the frame's centre, ties by row then column so the sequence is a total
order and cannot depend on a rounding):

| Ceiling | Pixels drawn, tile-major | Surface kept | Pixels drawn, centre-out | Surface kept |
|---|---|---|---|---|
| 1/16 | 203 | 0.0% | 352 | **25.0%** |
| 1/8 | 322 | 0.0% | 789 | **59.0%** |
| 1/4 | 673 | 0.0% | 1,147 | **72.6%** |
| 1/2 | 1,511 | 36.4% | 1,840 | **97.0%** |
| 3/4 | 2,573 | 100.0% | 2,389 | 100.0% |

At a sixteenth of the frame's cost, ordering is the difference between an empty image and a quarter of the
scene. Centre-out also buys *more* pixels for the same money at the low end - 352 against 203 - because it
reaches the cheap, provably-empty tiles earlier, and a skipped tile costs nothing to decide. That is the
honest shape of the finding: a ceiling is a statement about the frame's composition as much as about its
cost, so the order a renderer walks is a content decision, not an implementation detail. Tile-major stays
the default because it is what a top-down progressive refinement wants, and `render_bnb_budget` is
documented as `Scan::TileMajor` rather than as "the" order. **The `full` row is not clean**, and the reason is the conservative charge:
a ceiling equal to the measured total still leaves four pixels undrawn, because the last rays reserved
`max_steps + 1` and spent less. `tests/budget.rs` pins the useful half of that - an unlimited budget is
the unbudgeted frame byte for byte, every drawn pixel is the pixel the hierarchy render drew, a larger
budget never draws fewer pixels, the cut falls on tile boundaries so the drawn set is a prefix - and none
of it is a frame time: charged evaluations have never been calibrated to nanoseconds here, and gate R1
still needs a GPU to be answered at all.

### The third layer: a descent through the bounds, and the tolerance bug it found (2026-10-12)

The middle columns above did not exist an hour before it did. `SceneBounds::may_reach_subtree` asks the
question the ray early-out asks - can this sphere contain surface this ray can reach? - of each **child**,
and stops at any node the bound rule does not licence. The single root sphere collapses to 0.0% on the 32-
and 128-sphere line rows and on every scatter row, because one sphere around a long row swallows the
frustum; the children do not, and on those same rows the descent proves **96 to 98% of rays empty**. The
charged step count falls with it: the 128-sphere line goes from 287,112 evaluations to **648**, a factor of
443, on a frame asserted byte-identical. That is ADR-024's hierarchy measured rather than asserted, and the
first number in this repository that says what a BVH is worth instead of only that one is due.

**It cost a real bug to get here, and the bug is the part worth reading.** The first version tested each
sphere's bare radius, and the 8-sphere cluster row disagreed with the unculled frame by one pixel and one
hit: a grazing ray that passes within `surface` of a sphere yet outside it is a *hit* as far as the marcher
is concerned, because the marcher stops wherever `|field| < surface` and a sphere's field is distance minus
radius. A skip is a claim about the marcher's tolerance, not about geometry, so every enclosure consulted
for a skip is grown by `surface` first. The property that caught it is the one the module was built around -
compare the frames, never read the arithmetic - and it is the same reasoning `respects` already encodes for
the per-step skip, where `smin` dips below its own bound and is therefore refused outright.

**What the descent deliberately does not do.** The entry distance is available, and starting the march there
instead of at the camera would skip the empty approach. That was built, measured, and removed: on the test
scenes it saved 8 march steps out of 85,508 on a mirror row while shifting 4 to 12 pixels per frame by up to
3x `surface`, because a trace starting later takes a different sequence of steps and stops at a different
point inside its tolerance. The hit *set* is unchanged; the bytes are not. Trading byte-identical frames -
the invariant every row above rests on, and the reason the counts can be believed at all - for a saving two
orders of magnitude smaller than the skip's is a renderer-policy question, recorded as
[OQ-53](13-risks-open-questions.md).

**The last four rows are a build decision, not an algorithm one.** Same scattered spheres, same camera,
same picture, same steps, same unpruned node visits — only the order the leaves were emitted in differs,
and the median split turns 15.2% into 46.1% at 32 and 20.5% into 54.3% at 128: **2.6× to 3.0× more of the
work removed by nothing but tree shape**. It also tightens the root bound as a side effect (r 138.0 to
84.7), because covering two spheres by midpoint-and-max is order-dependent — so a tree shape is looseness
as well as traversal, and both are bought at build time. The safe place to spend that is `assetc`'s
lowering of plain unions, where commutativity of `min` makes reordering exactly semantics-preserving;
under `subtract` operand order *is* meaning, so the rule must be per kind and never a general sort.

Three layers, and the middle one is the one that failed first. **A single bound per ray** is the
`Rays skipped by the root bound` column: it proves 99.9% of rays empty on a compact scene and exactly
**0.0%** on the same 32 or 128 spheres laid along a line - the bound grows to r 19.0 and 76.6, contains the
frustum, every ray is merely *permitted* to march, and the hit count never budges. Nothing is wrong with
the early-out; it ran out of information. **A descent through the bounds per ray** is the next column, and
it is what that information looks like: 97.1% of the line's rays proved empty, 443x fewer charged steps.
**A bound per node inside a step** is the last two columns - the layer that also survives the spread, 98.1%
and 98.3% of node visits avoided on those same zero-saving rows, 97.5% on the 128-sphere cluster, the share
growing with the count instead of decaying, which is the shape ADR-024 claimed and this table now shows.

The counter in those last columns is itself the correction. `Stats::evaluations` counts **march steps**,
and a skip inside a step cannot change a step count: the skipped subtree would have returned a distance
larger than the one already in hand, so the trajectory is identical by construction — which is equally the
proof that skipping is safe, and the reason this probe's first run reported a saving of exactly zero for
the whole per-node layer. So `SceneBounds` carries a node-visit tally, `render_culled` is the walk that
counts **without** skipping and `render_bnb` the one that skips, and the two columns are the same frame measured twice -
same pixels, and since the descent arrived, fewer steps in the pruning run because whole rays were proved
empty before any of them was walked. `render` reports `visits: 0` for itself, because it has no table and
will not print a number it did not measure. One more reason to trust those counts: the probe runs as a CI
step on all three architectures and every number above — 3,291,795 unpruned and 747,137 pruned for the
cluster, 5,134,680 and 2,757,440 for the line — prints identically on x86-64 Linux, x86-64 Windows and
ARM64 macOS (`38292159934`). A f32 radius compared against a f32 distance is exactly where a cull could
go quietly machine-specific; here it does not.

Spike S1's third reading guessed that "an early-out on a scene like this is worth up to about 3×".
Between the two layers the truth is neither a ceiling nor a floor but the wrong variable: what determines
the saving is the bound's tightness relative to the frustum, times how many bounds there are to consult.

That is ADR-024's hierarchy stated as a requirement rather than a promise, and it arrived by building the
weak version first: one bound is a fixed win that decays as the scene spreads, so the structure that has
to exist is **many bounds, chosen per step** — the `O(log N)` skip the mechanism table below names, now
with a measured floor to beat (`Work avoided` at the same hit fraction, and `Nodes unpruned` as the
naive cost it is priced against).

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

### R2's span on the device: 36,000 frames marched by the shader (2026-10-16, `cargo run --release -p gpu --example r2_device_span`, MX250/Vulkan)

The CPU span above ran the ten minutes against `render::march` and said the device's span was still derived;
`device_edit_loop` ran the device at four occupancies. This joins them — 36,000 consecutive frames at 64×48
through `begin_frame` / `write_rays` / `march_frame`, one slab edit every sixth frame, 6,000 records = ten
minutes at ten a second. Two runs of the shipped binary: **every asserted integer identical** (hit counts, the
ray totals checked, the worst deviation, the byte and record counts), wall clock 66,902 ms and 63,030 ms —
9.0x and 9.5x faster than the session it reproduces.

Four asserted terms, all integers or geometry (ADR-017 forbids the rest):

| term | value |
|------|-------|
| rays finding surface, every frame | **3,072 at frame 1 and frame 36,000, asserted equal frame to frame** |
| device vs `render::march` | **6,900,480 lattice rays + 184,320 in 60 full-grid checks, 0 hit/miss disagreements, worst depth deviation 0.000005** against the 0.002 bound |
| placement, every frame | centre depth **7.5129** against doc 02's 7.500 ± 0.05, and more than half a unit nearer than the unedited terrain's **9.0000** |
| ledger | every edit reads back; **6,000** tree rebuilds + shape uploads + re-binds (480,000 shape bytes, 80 each) and **6,000** refills of 98,304 bytes, for **36,000** frames that created no buffer and no bind group |

Two findings the four-occupancy loop could not produce. First, **the frame path holds at span length**: a
reused `Frame` means the per-frame allocation count is zero across 36,000 frames, while an edit still reaching
the GPU still costs a host rebuild plus a re-upload plus a re-bind — OQ-55's price, now counted at the edit
cadence rather than sampled. Second, **the growing term lives in the read-back, not the frame**: marching the
ledger through `deltas().collect()` once per edit touched **18,003,000 record visits** over the span (3,000
average per edit, 500 per frame), where the device's own per-frame work is a constant 3,072 rays. The CPU span
took its read-back once, at the end, so this is the first measurement of the commit path amortised across a
whole session — and it is arithmetic on asserted record counts, not a duration.

Milliseconds are printed and asserted nowhere: on the run above, min 0.84, median 0.96, p95 6.28, p99.9 7.31,
max 26.35 ms over 36,000 device frames (0.81 / 0.94 / 6.25 / 7.33 / 22.11 on the second), a 7.6x tail here
where the same harness measured 1.27x–12.7x on the CPU reference. The frame count is settable through `MERKLES_R2_DEVICE_FRAMES`; CI runs 3,600 because a runner's
device frames cost 1–20 ms and the job has a timeout — the per-frame assert is the same one, and each run
prints the count it actually did.

The plant runs inside the binary rather than in a script: after the last frame, the same grid marched over the
**unedited** terrain must miss the placement bound. It returns 9.0000 against the 7.5 ± 0.05 window, so the
per-frame depth assert demonstrably tests whether the edit arrived rather than whether a buffer was filled.

R1's 16.67 ms at 1080p is untouched by this — a 64×48 grid prices the span, not the frame budget.

### The demo's scene as content, on a device (2026-10-16, `cargo run --release -p gpu --example boot_scene_on_device`, MX250/Vulkan)

Every device number above was marched against a tree the example itself assembled. That is the right fixture
for pricing a march and the wrong one for doc 09's promise, which is that content arrives as **bytes a
compiler emitted**. So the boot's scene is now also a `.ron` file
(`crates/assetc/tests/fixtures/boot_scene.ron`), compiled by the shipped `assetc::watch::Watch`, read back by
`assetc::reader`, validated by `render::validate`, uploaded, and marched:

| Term | Measured | Why it is the term |
|---|---|---|
| The two forms are one scene | 11 nodes, 25 parameters, depth 4; **345 bytes = 276 tree + 14 label + 55 header** against the boot's published 348 = the same tree + its 17-character label | Node count, pool length and depth come from the compiler's own `Outcome::Rebuilt`, and the byte identity is asserted, so the envelope difference is accounted for rather than noticed |
| Device against reference, ray for ray | 3,072 rays, 605 surfaces, **0 hit/miss disagreements**, worst deviation **0.000001** | The boot's own camera law (`probe_camera()`: surface 1e-3, 96 steps, max `t` 40), not a grid invented for the probe — `marched.max_steps` and `marched.surface` are asserted to be that law, so a clamped shader could not pass for the demo's frame |
| Reused frame against one-shot | **0 of 3,072 depths differ in any bit** | AS4's persistent buffers must be the same renderer; a bit-level comparison is the only version of that claim that is not a tolerance argument |
| The plant | `pool[0] += 2.0` → **80 rays flip between surface and sky**, surviving rays still within 0.000001 | The depth tolerance cannot see a scene that moved; the hit/miss set can, which is why the anchor is the set |

This is not R1: 3,072 rays on a CPU-law grid is a shape of a frame, and the 1080p budget question is
unchanged. It is also not the boot reading a file — the shipped binary still builds its scene through
`assetc` in memory, so the remaining step for the demo's rendering clause is to boot *from* the compiled
asset rather than beside it.

**Two vendors, one picture** (CI run `38925028343`, step "Demo scene from content on a device"). Every
integer in the table above reproduces on the ARM64 macOS runner's Apple Paravirtual device over Metal:
3,072 rays, **605 surfaces, 0 hit/miss disagreements, worst deviation 0.000001, 0 of 3,072 bits differing
between the two device paths, and the same 80-ray flip under the same plant**. The x86-64 Linux and Windows
runners compile the step and print their own `SKIP boot_scene_on_device: no device (NoAdapter)` — after
having already written and re-read the 345-byte asset, so the AS1–AS3 half of the seam is checked on all
three and the device half on the two that have a GPU.

### The binary boots from the asset, and 17 of its 18 lines never know (`bash tools/boot_from_asset.sh`)

The example above draws the content; the shipped binary now consumes it. The `assetc` CLI compiles
`boot_scene.ron` into a 345-byte `.bin`, `cargo run --release -p engine -- <that file>` goes through
`scene_from_disk`, and the script requires **every published line except `scene source` to be byte-identical
to the in-code boot** — same probe-ray `t` (3.0070 after 8 evaluations), same 64×48 raster line, same region,
budget, carry, ledger and epoch-root lines. The one line that differs prints the path it read, which is also
why this cannot become the published run: gate P1.5's whole-output digest `db71f8a0…` is what both named
devices are recorded against, and a machine-specific temp path cannot be hashed.

So the equality needed a failure to be worth anything, and it has one. Moving one sphere centre from
`z 7.5` to `z 1.0` leaves the asset **at the same 345 bytes** — same node count, same pool length, the
envelope cannot see it — and changes what the demo marches:

| Line | Honest asset | Tampered asset |
|---|---|---|
| `scene raster` | 605 of 3,072 rays, 241,779 evaluations, bound r 10.79 | 590 of 3,072 rays, 243,008 evaluations, bound r 8.20 |
| `scene budget` | ceiling 88,921 of 177,843, 599 of 605 hits kept | ceiling 89,536 of 179,072, 584 of 590 hits kept |
| `region world` | 174,881 charged evaluations | 174,879 |
| `stream carry` | 813 frames at a 97-charge ceiling | same 813 frames, 57,663 charged |

The script refuses three ways, all measured: a tampering `sed` that matches nothing, a tampered asset whose
byte count differs (the compiler stamps the *file name* as the label, so the copy has to keep it), and a
comparison that leaves the `scene source` line inside it — that last control exits 1 on this machine, which
is what makes the 17-line agreement a diff that could have failed.

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
| R1 | 1080p/60 with 500 culled entities in scene | **invented target** — the transcript's own "60 FPS" is aspirational; set from spike S1. **Measured blocker 2026-10-13**: CI's adapter probe (PR #1, run `38272893103`) finds no GPU on any runner — ARM64 macOS opens `Apple Paravirtual device` on Metal, x86-64 Windows has only the `Microsoft Basic Render Driver` (`device_type: Cpu`) on Dx12, x86-64 Linux enumerates no adapter at all. A paravirtual or software adapter can validate the shader against the CPU reference and cannot measure a frame, so R1 is unmeasurable in CI by construction: it waits on real hardware, not on code and no longer on a licence - OQ-51 was decided and `wgpu` landed on `main` on 2026-10-13 |
| R2 | Frame time stable while a player continuously edits terrain for 10 minutes | doc 03 tiering |
| R3 | Region entry after LRU eviction shows prior edits with no visible pop-in | gate P1.4. **The work half is measured 2026-10-13** (`examples/pop_in.rs`): with `render_bnb_refined` spending a ceiling across frames, the demo region is fully drawn in 5 frames at a ceiling of 1/4 the culled frame's cost, and its first frame keeps 25.1% of the hits — centre-out. Same ceiling, corner-first order: 9 frames, first frame 1.4%. "No visible" still needs a display (OQ-51); what no longer needs one is the number. The streamed case is now measured too, and performed in the binary: step 9b of `cargo run -p engine` refines a three-cell frame at a one-ray-charge ceiling and settles all 3,072 rays in 813 frames for exactly the single pass's charge (57,665), asserting the settled frame equals the unbudgeted picture. `pop_in`'s five-versus-nine frames remain the one-tree figure; what the demo adds is that refinement is free in work and expensive in frames, and that most of the frame's empty rays sit uncollected behind the charge check (OQ-60). |
| R4 | Frame hash of a scripted camera path is identical across `wgpu` backends | **informational only** — must not gate shipping, per ADR-017 |
| R5 | Zoom to 1e-6 world units on a pig snout with no polygonation | ADR-011's central claim. `examples/deep_zoom.rs` measures the numerics at four coordinate regimes (the far camera-relative frame is bit-identical to the local one; the absolute one is 0.146 rad off in its worst normal) and found the streaming path's subtraction order in the process - see § A far cell rounds geometry away. Still not met as a gate: it is a claim about a displayed image, and there is no device yet (OQ-51) |

## Device-generated rays: the 66 MB was real, and it was not the floor (2026-10-17)

`r1_step_price` and `r1_round_trip_split` left R1 with a shape: at 1080p the cap-1 frame — one step per ray,
essentially no march — costs 66–74 ms in `write_rays` plus a 114–126 ms wait, and the step term saturates. The
reading everyone (including the earlier draft of this doc) took from that was that the fixed cost is the host
round trip, and that its dominant term is the rays: 2,073,600 of them at 32 bytes apiece, packed by the host and
pushed down the bus every frame. So `crates/gpu/examples/r1_device_rays.rs` builds the smallest possible version
of the other world, where none of those bytes exist.

The mechanism: `interp.wgsl`'s march law was lifted out of `march_rays` into `march_one` (duplicating the loop
would have forked the law, which is what this file's tests exist to prevent), and a second entry point,
`march_rays_from_camera`, derives each thread's ray from `gl_global_invocation_id` and a `CameraUniform` —
`origin`/`right`/`up`/`forward` plus `(half_height, aspect, max_t)` and the grid — using `render::raster::ray_for`
transcribed in the same operation order. On the host side `begin_camera_frame` / `write_camera` / `bind_camera` /
`march_camera_frame` allocate a 16-byte header, a 96-byte camera and the depth buffers, and **no ray buffer**.
The camera block rides binding 5 as a *uniform* rather than a fifth storage buffer, because
`Limits::downlevel_defaults()` caps a stage at four storage buffers and `interp.wgsl` is already at that ceiling.

What is asserted, on a GeForce MX250:

| Term | Value |
|---|---|
| Anchor grid, device rays vs `render::march` (64×48) | 132 hits both sides, **0** hit/miss disagreements, worst depth deviation 0.000001 |
| Same frame, host rays vs device rays | 132 / 132, **0** set differences, worst 0.000001 |
| Bytes per anchor frame | 98,304 (rays) vs **112** (header + camera) |
| Falsification — host given `half_height` 0.25 against the device's 0.5 | 364 hit/miss differences, so the comparison sees a camera |
| Camera moved 3.5 units, 112 bytes rewritten | 1,276 pixels change, 0 disagreements against the host path — the uniform is an input |
| Degenerate camera (zero-length rays) | 0 surfaces of 3,072, matching `render`'s `unit` through the shader's `select` arm |
| **1080p, one frame per path** | **80,964 surfaces both sides, 0 of 2,073,600 set differences, worst deviation 0.000005; 66,355,200 bytes vs 112** |

And the duration hypothesis died there. Three completed runs put the 1080p pair (host / camera, each including its
read-back) at 893.00 / 967.56 ms, 832.59 / 827.18 ms and 822.83 / 799.37 ms — the camera path *slower* by 74 ms in
one run and *faster* by 5 and 23 ms in the other two, so even the sign of the difference does not reproduce — while
the same 3,072-ray anchor frame on that same laptop timed 1.95, 1.82, 18.91 and 1.91 ms. The only stable timing term,
submit-only at 0.11–0.50 ms for both paths, says the dispatch was never where the frame went either. A machine that cannot resolve 1.82 vs
18.91 ms for identical work cannot support "deleting a 66–74 ms term changed nothing" as a *measurement of* zero;
what it supports is that the copy is not the floor R1 is stuck against, because if it were, the frame would have
got 8–9% faster and it did not. Two further attempts did not reach a verdict at all: they lost the device mid-leg
with `Parent device is lost`, which wgpu 27 raises as a panic from `Device::poll` rather than an `Err`, so the
1080p leg runs under `catch_unwind` exactly as `r1_round_trip_split` does, and only a payload naming a lost device
prints a SKIP — any other unwind exits 101 rather than hiding a failure behind a friendly line.

**Consequence for the roadmap.** The retraction is of the specific claim that the per-frame host round trip is
what R1 cannot get under: the ray bytes are deletable and deleting them buys nothing measurable, so what remains
in a 1080p frame is the march's own device time and the **8,294,400-byte depth buffer copied back to the host per
frame**. A renderer that shades on the device copies neither — depths become a texture the same device reads — and
that leg is not built, because it is the material/lighting pipeline doc 04 § Materials describes and not a
scheduling trick. The next R1 number should come from there, not from fewer ray bytes.

*(Superseded by its own last sentence, 2026-10-17: the two terms this paragraph names as what remains were then
priced apart, and the read-back term is 0.7-1.4% of the frame. The proposal that the next number come from on-device
shading is measured against in § The march and the read-back, priced apart below. The retraction of the round-trip
floor stands on its own; only the recommendation did not survive.)*

**What still stands unchanged:** the host path is not retired. `render::raster`'s cull, enclosure bounds and
per-ray budget charge all need to know which pixels were thrown away, and a device that invents its own rays does
not answer that; the device-ray path is for a march that wants every pixel in the grid. And nothing here touches
§4 or SPEC-02: a camera ray is doc 02's presentation `f32` that no root reads, and the field it walks is still the
one interpreter the CPU reference agrees with — the reference anchor row above is the assertion that guards it.
The step-cap sweep is unchanged; the shipped `march_rays` path and its gates still run.

## The march and the read-back, priced apart: the 8 MB copy is 0.7-1.4% of the frame (2026-10-17)

`cargo run --release -p gpu --example r1_march_alone` (a CI step) splits the camera path's `march_camera_frame`
into its two intervals — `march_camera_frame_no_readback` (encode, dispatch, drain the queue, copy and map
nothing) and `read_camera_depths` (copy and map `pixels * 4`, dispatch nothing) — and runs the legs plus the whole
path on one 1920x1080 camera frame at step caps 1, 8 and 64. The buffers are the frame's own: no ray list is
uploaded at any point, so this is the frame § Device-generated rays left off with, not the shipped host path.

| | cap 1 | cap 8 | cap 64 |
|---|---|---|---|
| Surfaces found — legs, and the whole path, identically | 0 | 65,148 | 80,964 |
| Depths differing in their bit patterns, legs vs whole path | 0 of 2,073,600 | 0 of 2,073,600 | 0 of 2,073,600 |
| Bytes the read-back's own copy moved | 8,294,400 | 8,294,400 | 8,294,400 |
| 2,040-pixel lattice vs `render::march` — hits, disagreements, worst | 0, 0, 0.000000 | 33, 0, 0.000001 | 53, 0, 0.000001 |
| ms, march leg only (three runs) | 87.06, 85.66, **657.13** | 604.67, 710.48, 871.66 | 786.38, 788.19, 793.52 |
| ms, read-back leg only | 10.21, 10.13, 13.54 | 8.18, 9.89, 11.57 | 11.10, 10.43, 6.64 |
| ms, whole path | 90.66, 91.38, 663.76 | 610.37, 751.13, 701.32 | 795.55, 802.01, 893.51 |

**Why the split is admissible.** The two legs are not trusted to be the same frame as the shipped path; they are
compared against it. Dispatch-then-copy returns **zero** differing depth bit patterns out of 2,073,600 at every
cap, and the read-back's hit count equals the whole path's, so the interval being priced is one march, twice. The
anchor grid (64x48) carries the same check against the CPU reference — 132 hits, 0 set differences, worst
0.000001 — and the falsifier runs in the same binary: a frame that was *written but never dispatched* reads back
as **3,072 hits and 2,940 disagreements** against the reference at that grid (the copy returns the zeroed buffer),
so the rows above cannot be a buffer's contents standing in for a march. And the header's step count is not
decoration: 0 / 65,148 / 80,964, asserted strictly increasing, reproducing `r1_round_trip_split`'s host-ray hit
counts exactly on rays the device derived itself — one frame, two harnesses.

**What it settles.** The read-back is the smaller term by two orders of magnitude: across the three runs it prices
**6.64–13.54 ms** regardless of cap — 10.21/8.18/11.10, then 10.13/9.89/10.43, then 13.54/11.57/6.64 — while the
march's leg moves 86–657 ms to 794–871 ms over the same rows. Against the cap-64 frame the copy is **0.74–1.40%**
(11.10 of 795.55, 10.43 of 802.01, 6.64 of 893.51; DERIVED), and even at cap 1, where the march has almost nothing
to do, it is 11% of a 91 ms frame. A copy leg that were absorbing the stepping would have to grow with it, and it
does not. So on-device shading, the leg § Device-generated rays proposed as the next number, deletes 7–14 ms of a
664–894 ms frame on this GPU. It is worth building for what doc 04 § Materials and § VRAM describe, and it is not
an R1 lever; that recommendation is retracted above and here.

**What the frame's floor is.** Cap 1 is one step per ray: the march switched off in all but name, zero surfaces
found, no ray bytes uploaded and no read-back in the interval — and it still costs **87.06, 85.66 and 657.13 ms**
for 2,073,600 rays on the MX250. The first two reproduce to 1.6% and the third does not, so no figure here is
asserted and none is needed for the verdict: **all nine whole-frame rows across three runs are over budget**, from
90.66 ms (5.4x R1's 16.67 ms) to 893.51 ms (53.6x). The one claim that does not depend on the clock is that the
frame's cost is per-ray stepping, not per-frame copying — because the copy's own price is the same 7–14 ms whether
the march finds nothing or 80,964 surfaces.
Three of the four levers `r1_round_trip_split` listed are now measured and dismissed — the ray bytes (deletable,
bought nothing), the read-back (0.7-1.4%), and the step cap (which does still buy pixels at this grid, 0 → 65,148 →
80,964, but at 1 step the frame is already 5x over). What remains in the dominant term is *rays in flight*: fewer
rays per frame, or a cheaper per-ray law. Fewer rays is the cull and residency work's job (doc 04 § Culling, gate
R5's reachability), not a scheduling trick — so R1's next number comes from a smaller grid, a coarser march, or a
scene the SDF walk reaches sooner, and 1080p/60 on an MX250-class device with this per-ray law is not met.

**Instrument notes.** Every integer above reproduced in all three runs - the same 0 / 65,148 / 80,964, zero
bit-differing depths in all nine rows, the same 8,294,400 bytes, the same 0 / 33 / 53 lattice hits with zero
disagreements - and no duration did. The legs' sum against the whole path has no stable sign either (cap 1: 97.27 vs
90.66, 95.79 vs 91.38, 670.67 vs 663.76 ms; cap 8: 720.37 vs 751.13, 883.22 vs 701.32; cap 64: 798.62 vs 802.01,
800.17 vs 893.51) — the split is two submits and two waits against the
whole path's one, so the sum is an attribution printed beside the rows, not a quantity asserted to equal them.
Milliseconds did not reproduce across the three runs - the cap-8 march leg is 604.67, 710.48 and 871.66 ms and cap
1's is 87.06, 85.66 and 657.13 ms, a 7.6x swing on one machine - and they are
asserted nowhere, per ADR-017. `r1_round_trip_split`'s lattice lesson is carried: the anchor is a 60x34 grid of
pixel *centres*, not a flat stride, because at width 1920 a stride of 64 samples one oblique column. The 1080p legs
run under `catch_unwind` and print a SKIP only for a payload naming a lost device; any other unwind exits 101.
The admissibility check was planted, once, by hand: with the anchor's dispatch call deleted the run exits 101
reporting **3,072 hits against the reference's 132, 2,940 set disagreements, and 3,072 of 3,072 depths differing
in their bits** — so a march leg that quietly stopped dispatching cannot pass this file, which is the failure the
whole split would silently depend on.
