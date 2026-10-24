# 07 — Physics, audio & animation

**Status:** model locked, unmeasured. **Depends on:** docs 02, 03, 04.
**Implements:** ADR-018, 019, 020.

These three were the "we're missing something" gap in the design conversation: everything had
been specified *except* how a player stands on a hill that exists only as a mathematical
function.

## Physics: analytic SDF gradients

There are no vertex buffers or meshes for PhysX/Havok to raycast against, so collision is
computed from the same SDF source the renderer uses — evaluated on the CPU/WASM side, never read
back from VRAM.

```
distance to surface : d = SDF(p)                    // negative ⇒ inside
surface normal      : N = ∇SDF(p) ≈ [(SDF(p+εx) − SDF(p−εx)) / 2ε, …]   // 6 evaluations
```

Resolution is the classic SDF push-out: `p += N * (-d)` until `d ≥ 0`, with velocity projected
onto the tangent plane. Gravity is `-Y` by contract ([doc 02](02-coordinate-system.md)), so
"grounded" is `N·(+Y) > cos(slope_limit)`.

**Determinism boundary.** All of the above runs in `f32` camera space and **nothing here is
hashed** — position, normal, contact depth and grounded-ness are presentation-grade, so AGENTS.md
§4 permits them (the rule is about roots, not about floats). The consequence is real though: two
peers watching the same mob can resolve its collision slightly differently, and no epoch root will
ever notice. Anything a mod commits through the ledger (a spawn, a field write, a circuit output)
must therefore be integer and peer-independent; anything derived from a gradient is treated as
transient and re-verified against the owner's `M_active` copy.

**Standing on a procedural hill** is therefore free of meshes: query the terrain SDF at the
player's `P_local`, get distance and normal, place them on the implicit surface. Because SDFs are
continuous, slopes and corners resolve without contact-geometry edge cases that plague AABB
voxel collisions.

| Query | Cost | Notes |
|-------|------|-------|
| point containment / distance | 1 SDF eval | terrain is a BVH-walked union of region SDFs |
| contact normal | 6 SDF evals | central differences |
| swept motion | march the SDF along the path | step count is the tuning knob |
| many entities | the risk | [OQ-16](13-risks-open-questions.md) |

**Rejected:** hidden mesh colliders generated at commit time (reintroduces the mesh pipeline and
its memory cost); VRAM readback per physics step (latency, and violates ADR-017); an external
physics middleware, which has nothing to chew on without meshes.

**Open ([OQ-16](13-risks-open-questions.md)):** gradient physics is cheap for one player and
expensive for a thousand mobs pathfinding. The mitigation set — LOD physics (tick rates by
distance and screen occupancy), coarse region-level navmeshes for macro mobs, fixed-size capsule
proxies for entities, sleeping bodies — is undecided. Remember the constraint from doc 05: micro
physics only exists **while observed**, so this is bounded by the interaction radius rather than
by world size, but it is still the main CPU risk of Phase 2.

**Fluids:** micro water/lava flow is a bounded cellular automaton that runs only while observed
(doc 05 rule library); the *macro* water table is a region field. A player who drains a pond and
leaves it sees the field's endpoint on return, not a frozen half-flow.

## Audio: sphere-traced occlusion

Traditional engines use pre-baked sound zones or mesh raycasts; there are no meshes and nothing
is baked. The same SDF functions used for rendering are **sphere-traced from the sound source to
the listener's ear**:

```
for each voice:
  march the SDF from source → ear
  accumulate occlusion = Σ blocked distance / density(sample)
  apply low-pass filter proportional to occlusion
```

Explosions behind a mountain muffle; footsteps in a stone cave get reverb; a wall placed between
source and listener changes the sound on the next frame, with no authored volumes.

**Rejected:** pre-baked sound zones (needs authored volumes, i.e. an art pipeline for audio);
mesh raycasts (no meshes); "always full volume" (loses the one spatial cue that makes a voxel
world feel solid).

**Unmeasured cost ([OQ-25](13-risks-open-questions.md)):** one march per voice per audio frame is
real CPU. A voice cap, distance culling, per-material occlusion LUTs and shared-geometry reuse
are all needed but unspecified. Audio also needs an engine choice (the design never named one).

## Animation: domain deformation, not bones

A pig is a function; there is no skeleton to bind. Animation deforms the *space* the SDF is
evaluated in — `SDF(T(p, t), t)` — so the surface stays smooth and the asset stays code.

```wgsl
// walking legs: shift the evaluation domain along Z with a phase per leg pair
let z_leg = p.z + sin(omega * t) * amp;
// breathing: periodic scale of the body's bounding radius
let breathe = 1.0 + 0.02 * sin(tau * t);
```

The WASM logic module owns pose parameters (velocity, limb phase, gait frequency) and writes them
into uniform buffers; the shader warps space on the fly. Because the transform is in the domain,
morphing between two poses is interpolation of the warp parameters — no vertex correspondence
problem, no rig, no skinning weights.

**Rejected:** bone rigs and skinned meshes (contradicts ADR-011); baked keyframe geometry (same,
and can't hot-reload); per-frame mesh regeneration from SDFs (reintroduces the mesh pipeline).

**What the grammar can and cannot say today (2026-10-24).** The snippet above shifts the domain
of a *leg*, which means warping one subtree and not the animal, and A-07 has no node that names a
part's placement: `Twist` and `Bend` are unary and both act about the origin, and the only
transform that exists is `EntityDef.position/rotation/scale`, applied host-side to the ray, once
per entity (`editor::gpu_renderer::Placement`). So this section's model is implementable per-part
only if [OQ-55](13-risks-open-questions.md) is decided in favour of a placement node — its drafted
wording is there, and the alternative reading is that locomotion is whole-entity motion plus a
host-side tree rebuild per pose, which is what the AS4 asset-reupload path already measures.

**Rigid per-part animation exists today, and it is not a warp (2026-10-24).**
`crates/entity/src/rig.rs` poses a limb by rewriting the parameters of the primitives that ride it.
A-07's sphere stores its centre as three `f32`s (`params = [r, cx, cy, cz]`, read by the evaluator arm
in `crates/render/src/lib.rs`), so a two-bone leg is one hinge applied to a column of sphere centres,
and the sole cut that flattens a hoof travels with its foot as a rotated normal plus a re-solved `d`.
The reason to prefer this over the paragraph above is a cost, not a taste: **record count, tree shape
and march budget are unchanged by a pose.** Doc 04's law is `steps × nodes`, so a stride buys
parameter writes where a tree rebuild per pose — or one walk per placed part — pays a full walk.
`cargo run --release -p editor --example render_gait` dumps eight frames of one stride and prints the
node count on every row (74 at every phase for the pig), so the claim is visible rather than asserted.

Three laws that instrument produced:

- **Selection is a rest-pose operation.** `select_leg` is geometric — a column of spheres at an
  authored `z` — and a swung limb no longer sits at that `z`, so the record indices are taken once from
  the rest tree and reused for every frame after it.
- **Forward kinematics floats the animal.** A limb hinged above its own foot raises that foot by
  `L(1 − cos θ)` at *any* non-zero angle, straight knee or not, so a body that stays put hovers in every
  frame but stance. `Leg::support_y` reports where a limb stands — the sole *cut*, not the sphere bottom,
  since the `Intersect` removed everything below it — and the caller keeps a hoof on the floor by moving
  the body. The first strip had no such line and floated; the frames now print the drop (`body −0.0079`
  at maximum reach).
- **One authored leg cannot trot.** The pig's column is `Mirror`-reflected about X, and an X-parallel
  hinge moves nothing in X, so both sides of a pair are the same records and swing in unison: a **pace**.
  A diagonal gait needs both sides authored (twice the nodes) or OQ-55's placement node.

What it does not buy: a primitive that stores no orientation cannot be turned by a parameter patch, so a
`RoundedBox` limb is immobile here, and soft-tissue deformation — a breathing belly, a floppy ear — still
needs the per-part warp OQ-55 exists to make expressible. `rig` poses rigid parts and nothing else.

**Consequences to watch:**
- Silhouette-changing animation (a mob opening its mouth wide, a tail swaying far) costs march
  steps, since the SDF gets deeper/branchier. Keep warp amplitudes modest.
- Hitboxes and visuals come from the same function, which is a feature: the collision SDF can be
  the animated SDF, so a swing actually connects.
- Animation state machines live in WASM (doc 08), so mods can animate their own creatures without
  touching the engine.

## Input & local prediction

Player input runs at 60–144 Hz; epochs commit on a much slower cadence. Edits therefore apply to
`M_active` on frame 0 and render immediately — the ledger processes the transaction asynchronously
and the engine only drops the entry if a server later rejects it
([doc 03](03-state-ledger.md), [doc 10](10-multiplayer.md)). Rollback is a single store of the old
`u64` back into the slab slot, because nothing downstream derived geometry from the edit.

## Validation gates

| Gate | Test | Source |
|------|------|--------|
| PH1 | Player walks and stands on a purely procedural SDF hill, no sinking or jitter | design gap #1 |
| PH2 | 100 entities with gradient physics at 60 FPS | **invented target** |
| AU1 | Placing a solid wall between source and listener attenuates within one frame | design claim, unmeasured |
| AN1 | The coded pig walks with no visible popping, at a distance and up close | ADR-019 |
| DE1 | Physics results are computed on CPU/WASM with zero VRAM readback | ADR-017 |
