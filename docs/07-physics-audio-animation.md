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
