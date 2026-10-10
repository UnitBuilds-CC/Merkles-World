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

## Culling and the frame budget

| Mechanism | Notes |
|-----------|-------|
| Frustum culling | region-level and entity-level boxes |
| Spatial BVH over active SDFs | `O(log N)` skip; a ray executes an entity's detailed function only after hitting its coarse bounding sphere (ADR-024) |
| Region-level early-out | empty regions cost one table read |
| Step-count caps + `MIN_STEP` floor | guards the worst-case march |
| Distance LOD | fewer SDF terms for distant entities; epsilon grows with distance |
| Tile-based marches | group pixels that hit the same region |

**Budget to be met, not assumed** (target 1080p/60, mid-range dGPU): geometry passes ≤ 8 ms,
shading ≤ 4 ms, culling ≤ 1 ms, upload ≤ 1 ms, ~3.6 ms headroom.

What the design conversation actually claims: raymarching means every pixel steps through space
evaluating functions, and *"if you have 500 procedural entities (pigs, trees, rocks) across a
landscape, evaluating every entity's distance function for every ray step will instantly drop
your framerate to single digits"* — while *"simple SDFs render fast"* and *"combining hundreds of
complex SDFs in a single GPU pass can tank frame rates"*.

**Our derivation, not the source's:** at 1080p, if entities cover ~30 px each and the march takes
~10 steps, 500 visible entities is on the order of 10⁸–10⁹ SDF evaluations per frame. That is the
arithmetic behind the "single digits" claim and behind the BVH being a requirement, not an
optimization — and it is exactly what spike S1 must measure instead of estimate.

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

## Validation gates

| Gate | Test | Target / source |
|------|------|-----------------|
| R1 | 1080p/60 with 500 culled entities in scene | **invented target** — the transcript's own "60 FPS" is aspirational; set from spike S1 |
| R2 | Frame time stable while a player continuously edits terrain for 10 minutes | doc 03 tiering |
| R3 | Region entry after LRU eviction shows prior edits with no visible pop-in | gate P1.4 |
| R4 | Frame hash of a scripted camera path is identical across `wgpu` backends | **informational only** — must not gate shipping, per ADR-017 |
| R5 | Zoom to 1e-6 world units on a pig snout with no polygonation | ADR-011's central claim |
