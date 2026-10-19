# 16 — Entity framework

**Status:** design proposal (Phase 2 prerequisite).
**Depends on:** docs 02, 03, 04, 09. **Implements:** ADR-012 (master shader), OQ-04 (vertical slice content), OQ-45 (payload bit layout).

## Why this exists

Phase 1 proved the renderer can march SDF trees on a device. Phase 2 needs *content*: pigs, tools, biomes, weather effects — entities that move, interact, and persist. Creating 100 different animals without a schema would produce a mess of inconsistent RON files, ad-hoc movement logic scattered across examples, and no way to validate or budget them uniformly.

The entity framework is the contract layer between:
- **Content authors** (who write `.ron` files describing shapes, materials, behavior)
- **The render pipeline** (which needs bounding volumes, tree node ranges, dirty masks)
- **The simulation** (which needs position, velocity, lifecycle, persistence rules)
- **The ledger** (which commits state changes into 12-bit payload records)

This doc defines what an entity *is*, how it's authored, how it moves, and how its state persists. It does not define gameplay loops (OQ-04), lighting (OQ-10), or multiplayer trust (OQ-11/OQ-14) — those are separate decisions.

## What an entity is

An entity is **three things**:

1. **A shape tree** (SDF node subtree, authored in RON, compiled to `.bin`)
2. **A transform** (world-space position + orientation, updated every tick)
3. **A state record** (lifecycle flags, material ID, quantity — persisted via the ledger)

These three live in different subsystems but are linked by a stable `EntityId` (u32, doc 10). The shape never changes after compilation; the transform changes every frame; the state changes on ticks.

```rust
/// An entity at runtime. Not persisted — reconstructed from ledger state on load.
pub struct Entity {
    pub id: EntityId,              // Stable identifier (doc 10)
    pub shape: ShapeHandle,        // Compiled SDF tree (doc 09)
    pub transform: Transform,      // World-space position + orientation
    pub bounds: AABB,              // World-space bounding box (for culling)
    pub state: EntityState,        // Material, flags, quantity (ledger-persisted)
    pub tree_range: EntityRange,   // Which nodes in the scene tree belong to this entity
}

pub struct Transform {
    pub position: Vec3,            // f32 triple — camera-relative, never hashed (doc 02)
    pub rotation: Quat,            // Quaternion for orientation
    pub scale: Vec3,               // Uniform or non-uniform scaling (default 1.0)
}

pub struct EntityState {
    pub material_id: u8,           // Index into material registry (§ Material registry below)
    pub flags: u8,                 // Bitfield: alive/dead, active/inactive, custom flags
    pub quantity: u8,              // Stack count (items), health (mobs), growth stage (plants)
}
```

**Why these three fields for `EntityState`:** OQ-45 asks how the 12 payload bits divide. The answer is:

```
| material_id 8 bits | flags 2 bits | quantity 2 bits | = 12 bits total
```

- **material_id (0–255):** Index into the material registry. Enough for 256 distinct materials/colors/textures. This is the *only* field that affects rendering appearance.
- **flags (0–3):** Two bits for critical lifecycle: bit 0 = alive/dead, bit 1 = active/inactive (paused). Mods can use these for basic state machines without custom logic.
- **quantity (0–3):** For items: stack size (capped at 3 for this encoding — see alternatives below). For mobs: health tier (0=dead, 1=injured, 2=healthy, 3=full). For plants: growth stage.

**Alternative encodings considered:**
- `material_id 6 bits | flags 4 bits | quantity 2 bits` — more flags, fewer materials (64 max). Rejected because material diversity matters more than flag combinations for visual variety.
- `material_id 8 bits | flags 1 bit | quantity 3 bits` — quantity up to 7, but only one flag. Rejected because alive/dead and active/inactive are both essential.
- `material_id 10 bits | flags 1 bit | quantity 1 bit` — 1024 materials, but quantity is just present/absent. Rejected because stacking and health tiers need more than a boolean.

If a mod needs more than 256 materials, it must use texture variation within a material function (doc 04's noise nodes). If it needs more than 2-bit quantity, it uses multiple entities (a stack of 4 iron ingots = 4 entities at the same position, merged visually by the renderer).

## Authoring format (RON)

Entities are authored as RON files in `assets/entities/`. The schema is validated by `assetc` before compilation.

```ron
// assets/entities/pig.ron
Entity(
    id: "pig",
    sdf_tree: SmoothUnion(
        k: 0.1,
        children: [
            RoundedBox(size: (0.5, 0.4, 0.3), radius: 0.1),                       // body
            Sphere(radius: 0.25, offset: (0.0, 0.3, -0.4)),                       // head
            Cylinder(radius: 0.1, height: 0.1, offset: (0.0, 0.2, -0.6)),         // snout
            Repeat(axis: X, spacing: 0.5, Cylinder(radius: 0.08, height: 0.2)),   // legs
        ],
    ),
    material: ProceduralNoise(
        kind: Voronoi,
        base_color: (1.0, 0.7, 0.75),
        frequency: 5.0,
    ),
    bounds: AABB(min: (-0.5, -0.2, -0.7), max: (0.5, 0.6, 0.3)),  // Manual override; auto-computed if omitted
    behavior: "wasm:pig_ai.v0",                                     // Optional WASM module for AI/logic
    spawn_rules: SpawnRules(
        biome: ["plains", "forest"],
        light_level: (0.7, 1.0),     // Daytime spawning
        max_density: 5,              // Per region
    ),
)
```

**Required fields:**
- `id`: Unique string identifier (used for asset references, not the runtime `EntityId`)
- `sdf_tree`: The shape definition (doc 04's node grammar)
- `material`: Color/function definition (doc 04's material system)

**Optional fields:**
- `bounds`: Manual AABB override. If omitted, `assetc` computes tight bounds by sampling the SDF at grid points.
- `behavior`: Path to WASM module for AI/logic (doc 08). Omit for static entities (trees, rocks).
- `spawn_rules`: Conditions for procedural spawning. Omit for manually-placed entities.

**Validation rules (enforced by `assetc`):**
- SDF tree depth ≤ 8 (configurable, default from boot scene)
- Total node count ≤ 64 (prevents over-complex entities)
- Material parameters are finite (no NaN/Inf)
- Bounds contain the origin (entity is centered at its local origin before transform)
- WASM module path resolves to a compiled `.wasm` file

> **The example above is the proposal, not the parser.** What `assetc` and the editor read today is
> `EntityDef { shape, position, rotation, scale, material_id, flags, quantity }` — binary combiners,
> because ADR-011's wire form rejects n-ary ones — and the material is a `u8` slot in doc 04's registry
> rather than an inline noise description. `assets/entities/pig.ron` is the working instance. Reconciling
> this section with the shipped schema is an open editorial task, not a licence to write files in the
> proposed shape.

## Visual authoring

An entity is a program, and the whole point of §6 is that there is no mesh to look at before it runs.
That makes "does this look like a pig" unanswerable from a RON file, so `crates/editor` answers it from
pixels instead:

- **The viewport marches the real tree.** `gpu_renderer::CpuRenderer` lowers the authored `Shape` with
  `assetc::lower` — the same call the shipped compiler makes — and sphere-traces `render::march` over the
  result, so a preview cannot disagree with the asset the tool would emit. A refusal paints the frame in
  `Frame::NOT_DRAWN_RGB` and prints the reason; it never substitutes a guess.
- **The transform is applied to the ray, not to the records.** A-07's grammar has no translate or scale
  node (OQ-55), so `assetc::compile_entity` yields a tree that stays in *shape space* and the placement
  lives beside it. `gpu_renderer::Placement` reads `position`, `rotation` and `scale` off the
  `EntityDef`, and the march maps each world ray into shape space (`(p - position) / scale`, rotated by
  the conjugate quaternion), marches there, and converts the hit back: `t` by `scale`, point and normal
  by the same rotation. A rotation and a uniform scale take a straight ray to a straight ray, so this is
  exact rather than an approximation — and it is why the viewport can show what the file says while the
  emitted asset stays shape-space. Before it, `position`, `rotation` and `scale` were all silently
  ignored by the preview: an entity authored at `[0, 0.5, 0]` and one at the origin rendered the same
  image, so the inspector's transform sliders moved nothing you could see.
  `Placement::to_shape`/`to_world` are asserted as an exact round trip, the `[x, y, z, w]` quaternion
  order is pinned by the quarter-turn-it-takes-+X-to-Z test (read the tuple as `[w, x, y, z]` and every
  rotated entity faces a different way from the one the file describes), and an impossible placement —
  zero scale, non-unit quaternion — is refused into `NOT_DRAWN` by the same `compile_entity` the shipped
  compiler uses, rather than marched with a guess.
- **The stage is marched, not painted.** `Stage::under` puts the grid floor at the lowest surface the
  entity actually reaches, found by tracing one ray straight up per column of a grid over the enclosure.
  Deriving it from the authored parameters instead was tried and is wrong twice over: the enclosure is a
  *sphere*, so its bottom floats the animal above its own feet, and chaining each `SmoothUnion`'s `k`
  into the minimum overshoots at every level the leg passes through. The measurement is taken with the
  translation zeroed (`Placement::contact`) and the grid centred on the authored `position`, so the
  floor is the ground the *shape* would stand on: lifting the entity with `position.y` moves it off its
  own contact plane and shows the grid underneath, which is the only way a y-offset is distinguishable
  from a shape that simply has long legs.
- **The material reaches the pixel.** The viewport multiplies its lambert shade by
  `Material::base_color`, read from the single `MaterialRegistry` that `EditorState` owns — the same
  object the panel's swatches read, so a swatch cannot promise a colour the render disagrees with.
  (It did: the panel carried its own copy of `pig_skin` as `[1.0, 0.7, 0.8]` against the registry's
  `[1.0, 0.7, 0.75]`, and the viewport ignored materials entirely, so assigning one changed nothing
  you could see.) Only `base_color` is honoured. Doc 04's noise term is *not* evaluated, so an
  authored Voronoi `pig_skin` still reads as flat pink, and `gpu_renderer::tests` asserts exactly the
  part that is wired: two different slots must produce two different centre pixels, and a tinted slot
  must not come out with R == G. An empty slot renders white and prints a warning rather than
  refusing the frame, because the shape is still the thing under inspection.
- **The instrument is `cargo run -p editor --example render_png`.** It writes six camera angles to the
  temp dir (§6: never into the repo) and prints the hit-pixel count and wall time per frame, so a
  geometry change is judged by looking at it rather than by reading the code that made it. Every
  rejected revision of `pig.ron` is recorded in that file's header, which is how the second attempt at a
  mistake got avoided instead of re-litigated. The current revision of `pig.ron` was looked at that way
  on 2026-10-20: front, back, side, three-quarter, low and top dumps all read as a snouted quadruped
  standing on the grid, with the head still larger than the body — a bear cub's proportions, which is a
  content decision about `pig.ron`, not a defect in the instrument.
- **The interactive window** (`--example entity_editor`, run in release) adds orbit, zoom, the inspector,
  the material panel, and a facing compass projected through the same camera basis as the raster. It
  repaints only when `EditorState::preview_signature` moves — that digest covers the shape, the
  placement and the material slot, and deliberately *not* `flags` or `quantity`, which reach the file but
  no pixel. A key that missed a pixel-changing field would keep showing the old frame while the
  inspector believed it had moved the animal, so `lib::tests` edits each field and requires the key to
  change, and edits the two invisible ones and requires it not to.
- **The window's dependency surface is trimmed, and that is load-bearing.** `crates/editor` asks for
  `eframe` with `default-features = false` and re-enables only `default_fonts`, `glow` and `x11`.
  eframe's defaults also turn on `accesskit` and `web_screen_reader`, which on Linux pull
  `accesskit_unix → zbus → zbus_names`/`zvariant`/`zvariant_derive` (three RUSTSEC *unmaintained*
  advisories) and `zbus_xml → quick-xml` below 0.41 (two more, with real DoS ranges). Screen-reader
  support is not something this tool claims, so those five advisories went away by not taking the
  feature rather than by waiving them. What could not be trimmed is recorded in `deny.toml`:
  `ttf-parser` (RUSTSEC-2026-0192, *unmaintained* rather than vulnerable, reached through
  `epaint → ab_glyph → owned_ttf_parser`, which is a non-optional dependency — the only alternative is
  shipping a font file, which §6 bans), and the two licences `epaint_default_fonts` is declared under,
  `OFL-1.1` and `LicenseRef-UFL-1.0`, which cover font data inside a dependency rather than anything
  added to this repository.

The preview is a CPU march rather than a device march, so it is an authoring instrument and not a frame
budget. In a debug build the six-angle dump measured 11.0–17.1 s per 480×360 frame and the editor's own
256×192 preview 2.5–4.6 s; the same six-angle dump in a **release** build measures 1.20–1.43 s per
480×360 frame (2026-10-20, this machine, the 32-node `pig.ron`), which is why both examples are run with
`--release`. No number here is an R1 measurement — §3's hot-path bans do not apply to a tool, but nothing
it prints is evidence about a game frame.

## Runtime lifecycle

### Spawning

An entity spawns when:
1. **Procedurally:** spawn rules match current biome/time/light conditions (checked per region per tick)
2. **Manually:** player places it (crafting result, creative mode)
3. **Scripted:** circuit/WASM logic creates it (spawner block, spell)

On spawn:
- Assign next available `EntityId` (monotonically increasing u32, wraps after 2³²)
- Compile SDF tree to GPU buffer (if not already cached)
- Insert into scene tree at next available node range
- Compute initial AABB from bounds or SDF sampling
- Write initial `EntityState` to ledger (material_id, flags=alive+active, quantity=0)

### Updating

Every tick (doc 03's epoch):
1. **Behavior update:** If entity has WASM module, call `on_tick(entity_id, dt)` → returns new transform/state
2. **Physics integration:** Apply gravity, collision response (doc 07's gradient queries)
3. **Bounds update:** Recompute AABB from new transform (or mark dirty if unchanged)
4. **Dirty mask invalidation:** If AABB changed significantly, call `invalidate_entity()` (gpu crate's API)
5. **State commit:** If state changed (material, flags, quantity), append delta to slab

**LOD physics (OQ-16 mitigation):** Entities beyond 50 voxels distance tick at reduced frequency:
- 0–50 voxels: every tick
- 50–200 voxels: every 4th tick
- 200+ voxels: every 16th tick

This is configurable per entity type via `spawn_rules.lod_distance`.

### Despawning

An entity despawns when:
1. **Death:** flags.alive = false (mob killed, item consumed)
2. **Timeout:** No player within 100 voxels for 300 ticks (5 minutes at 60 Hz)
3. **Manual removal:** Player breaks it (pickaxe, explosion)

On despawn:
- Remove from scene tree (compact node range)
- Free GPU buffer if no other entity uses same shape
- Append deletion delta to ledger (flags.alive = false)
- Recycle `EntityId` after 1000 epochs (prevent immediate reuse conflicts)

## Integration with existing systems

### Render pipeline (doc 04)

The renderer sees entities as **tree node ranges**. When an entity moves:
1. Compute new AABB from updated transform
2. Call `interpreter.invalidate_entity(queue, camera, old_aabb, new_aabb)` (gpu crate)
3. Mark corresponding pixels dirty in temporal cache
4. Next frame remarches only dirty pixels

The temporal cache tracks which tree nodes produced each pixel (`entity_node` field in `CachedPixel`). When an entity's subtree parameters change (position, rotation), only pixels attributed to those nodes are invalidated.

### Ledger (doc 03)

Entity state persists via the 12-bit payload field in delta records:

```
| region-slot 16 bits | intra-region 36 bits (x:12, y:12, z:12) | payload 12 bits |
                                                                                   ↑
                                              material_id:8 | flags:2 | quantity:2
```

The `intra-region` coordinates name the voxel where the entity's origin sits. The payload encodes its state. This means:
- **One entity = one delta record** (not one per voxel it occupies)
- **Entity position is quantized to voxel precision** in the ledger (f32 transform is transient)
- **Multiple entities can occupy the same voxel** (different records, same coordinates, different payloads)

### Physics (doc 07)

Collision queries use the entity's AABB for broad-phase culling, then gradient descent for narrow-phase:
1. Broad phase: Test ray against entity AABB (cheap box intersection)
2. Narrow phase: March SDF tree within AABB bounds (expensive, but only if broad phase hits)

Entity-entity collision is **not** resolved by the engine. Mobs can walk through each other. Player-vs-mob collision uses simple sphere-sphere tests at the physics layer (doc 07), not SDF marching.

### Asset pipeline (doc 09)

`assetc` compiles entity `.ron` files to `.bin` FlatBuffers:
1. Parse RON → validate schema
2. Lower SDF tree to FlatBuffer binary (doc 09's node grammar)
3. Compute/material budget pass (reject if > 64 nodes or depth > 8)
4. Write `.bin` to cache directory
5. Hot-reload signal to engine (if running in watch mode)

Compiled entities are loaded at startup or on-demand (when a region containing them loads).

## Crate structure

New crate: `crates/entity/`

```
crates/entity/
├── Cargo.toml
├── src/
│   ├── lib.rs          # Public API: Entity, EntityManager, Transform
│   ├── component.rs    # Component traits (Position, Velocity, Health, etc.)
│   ├── manager.rs      # EntityManager: spawn/despawn/update lifecycle
│   ├── query.rs        # Spatial queries: nearest, within_radius, frustum_cull
│   └── spawn.rs        # Procedural spawning logic (biome matching, density limits)
├── tests/
│   ├── spawn_despawn.rs    # Lifecycle tests
│   └── spatial_query.rs    # BVH/culling tests
└── examples/
    └── pig_simulation.rs   # Demo: spawn 10 pigs, simulate movement, measure FPS impact
```

**Dependencies:**
- `render`: For SDF tree compilation, AABB computation
- `gpu`: For dirty mask invalidation, temporal cache integration
- `coords`: For world-space positioning, region key computation
- `assetc`: For loading compiled `.bin` entities

**Does NOT depend on:**
- `ledger`: Entity state persistence is optional (single-player doesn't need it)
- `engine`: Entity framework is library-only, no main loop

## Open questions

The four questions this doc raised live in the register where every `OQ` lives,
[doc 13 § Modding & content](13-risks-open-questions.md), and not here — a question defined in two
documents is a question that answers differently in each. In short, so the reader knows what is
waiting:

- **OQ-63** — entity rotation representation. Note that `[f32; 4]` is already what
  `assetc::EntityDef` carries, so this is now a ratification, not a choice.
- **OQ-64** — how many entities per region, which nothing in `crates/entity` currently bounds.
- **OQ-65** — CPU or WASM for entity AI, which is OQ-20's question for mods arriving for mobs, and
  is blocked on the mod side by OQ-41's export-lint decision.
- **OQ-66** — how two peers' edits to one entity merge, which is where OQ-61's refused divergence
  lands once the record is an entity rather than a voxel.

## Verification gates

| Gate | Test | Source |
|------|------|--------|
| E1 | Entity spawns, renders, and despawns without memory leak | Lifecycle test |
| E2 | Moving entity invalidates correct pixels (dirty mask matches AABB projection) | gpu invalidation test |
| E3 | 100 entities at 60 FPS on GTX 1070 (performance budget) | Benchmark |
| E4 | Entity state persists across save/load (ledger round-trip) | Persistence test |
| E5 | Compiled entity `.bin` is deterministic across platforms (byte-identical) | AS2 gate for entities |
| E6 | Over-budget entity (>64 nodes) rejected by `assetc` with span-accurate error | Validation test |

## Migration from current code

The temporal cache already has `EntityRange` for tracking tree node ranges. The migration is:

1. Move `EntityRange` from `render::temporal_cache` to `entity::component`
2. Replace hardcoded demo scene entities with spawned entities
3. Wire `EntityManager::update()` into the render loop (before `march_camera_variant_no_readback`)
4. Add dirty mask invalidation calls when entities move

No breaking changes to existing APIs — the entity framework is additive.
