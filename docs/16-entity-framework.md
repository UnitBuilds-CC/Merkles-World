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
  zero scale, non-unit quaternion, a non-finite `position` — is refused into `NOT_DRAWN` by the same
  `assetc::compile_entity` the shipped compiler uses, rather than marched with a guess.
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
- **The rotation has a widget, and it is the panel's, not the format's.** The Transform section shows an
  axis and a turn in degrees — the thing an author picks up — plus five whole-quarter presets, and writes
  the `[x, y, z, w]` quaternion `EntityDef.rotation` already carries. **OQ-63 is not settled by this**: the
  question of which representation the *file* uses stays open, and the panel only composes the field that
  exists. What makes the widget more than four sliders is that it normalises, because the quaternion comes
  out of it unit by construction and the `assetc::compile_entity` the viewport routes through refuses a
  non-unit one into
  `NOT_DRAWN` — a widget that wrote a raw four-tuple could hand the viewport a shape it will not march, and
  an authored non-unit quaternion is therefore named in red under the readout rather than silently fixed or
  silently dropped. The four numbers the file will carry are printed under the sliders so the author can see
  which convention the value is in. Two tests hold it up: the paint probe requires the readout for a
  quarter-turn state and the refusal text for `[0.0, 2.0, 0.0, 1.0]`, and requires neither to edit anything;
  the arithmetic test asserts the exact quaternion a 90° turn writes (derived in-test, not hard-coded), that
  `[0, 3, 0]` and `[0, 1, 0]` write the *same* one, that a zero-length axis reports that it wrote nothing and
  leaves the authored value alone, that `axis_angle` reads back what was written for three unequal turns, and
  that turning the other way is the same as turning about the opposite axis.
  The same unit moved the section from `ui.collapsing` to a `CollapsingHeader` open by default: egui does not
  run a closed section's closure, so the position, rotation and scale widgets — and the quaternion readout
  with them — did not exist until the heading had been clicked. That is how the paint probe found it, by
  failing to find the readout it was written to assert.
- **Any node in the tree can be edited, not only the root.** `shape_path` addresses a part by a route of
  steps from the root — `[Second, Second, Sole, Second]` is `root.b.cutter.child.b` — and the panel lists
  every node depth-first, one row each carrying its route, its kind and its own parameters, with the
  sliders below writing to whichever row is selected. Before it the inspector matched on the root variant
  alone, which for `pig.ron` (rooted at a `SmoothUnion`) meant the panel could set the whole animal's
  blend factor and could not reach the snout. The write-back replaces the selected node wholesale rather
  than field by field, so a combinator keeps its children. The field names in a route are the ones
  `assetc`'s refusals print — that crate's `edge`/`label` alternate kind and field, so an error reads
  `Subtract.cutter.Mirror` — and `shape_path::tests` *checks* the agreement by refusing a deep node and
  comparing the two routes, rather than trusting a doc comment. A selection the tree no longer contains
  (after a load, or after the root is rebuilt) names itself gone and hands back nothing to edit; sliders
  that quietly retargeted the root would author a change the file does not describe. `lib::tests` holds
  both halves of that: a dead selection resolves to `None`, and editing a selected part moves
  `preview_signature`, the key the repaint waits on, which a root-only key would have left alone.
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
256×192 preview 2.5–4.6 s; the same six-angle dump in a **release** build measured 0.85–1.16 s per
480×360 frame on the run that closed the part-selection unit (2026-10-20, this machine, the then-32-node
`pig.ron`), where the run earlier the same day measured 1.20–1.43 s — the same binary, so the spread is
the machine's, not the renderer's, and no number in this paragraph should be read tighter than "about a
second". Both are why the examples are run with `--release`. No number here is an R1 measurement —
§3's hot-path bans do not apply to a tool, but nothing it prints is evidence about a game frame.

The same instrument on the re-authored pig measures **2.2–2.6 s** per 480×360 frame (2026-10-22, this
machine, 36 nodes, every join a smooth union). That is roughly the doubling one would expect from four
more parts and a leg column that went from two blended spheres to three, but it is not a measurement of
the `k` cost the comment in `pig.ron` hypothesises: nothing here holds the node count fixed while it
varies the blend, so the same machine's load confounds it exactly as it confounded the numbers above.
It is a reason to build that control pair before anyone prices an authored creature into a frame budget.

### Driving the editor over MCP

`cargo run -p editor --bin editor_mcp` speaks MCP over stdin and stdout: one JSON-RPC line in, one reply
out. It hands an agent the same `EditorState` the window edits — one object, two front ends — so
"did the edit land" and "what does it look like now" are answerable without a display, a device, or
anyone at the keyboard. `crates/editor/src/mcp.rs` holds the protocol and `handle(state, line)` is all of
it; the binary is a loop over lines, which is why a test can drive the module and a pipe can drive the
same module rather than a mock of it.

Thirteen tools:

| Tool | Answers |
|------|---------|
| `editor_state` | file, dirty flag, `preview_signature`, the selected route, the transform block, the material block, `node_count`, and every node's route, kind, current summary and the parameters `set_part` takes for it |
| `list_materials` | every occupied slot, with all six authored fields |
| `set_material` | one slot and at least one of the six fields a material carries — `base_color` (three lanes, `0..=1`), `roughness` (`0..=1`), `emissive` (`>= 0`), `index_ior` (`>= 1`), `noise` (one of `Voronoi`, `Value`, `Perlin`, `None`) or `noise_frequency` (`> 0`) — written only after every field on the call validates. The reply separates `changed_a_pixel` from `changed_no_pixel`, because only `base_color` has a reader yet (OQ-10), and says that the registry is not in the entity file: the slot the entity points at persists, the values in it do not |
| `register_material` | a whole material into the registry's next free slot: all six fields at once, each held to the same range `set_material` is, because a field left off would stand as `Material::default`'s value rather than as one the caller chose. The slot is the registry's to hand out and not the call's — `register` is the only way to occupy one — so the reply reports where it landed; `set_transform`'s `material_id` is what points the entity at it. Registering paints nothing by itself, and the new material is as session-bound as a recolour |
| `load_entity` | a `.ron` path in, the state out; a file that will not parse answers with `ron`'s own error and leaves the held state untouched |
| `set_transform` | `position`, `rotation`, `scale`, `material_id` — every field validated before any field is applied |
| `select_part` | a route as `editor_state` prints it (`root`, `root.b`, `root.b.kept`) in, the node's kind, summary and parameter list out. Selecting is not editing: it marks nothing dirty and leaves `preview_signature` alone |
| `set_part` | one authored field of one node — a `Sphere`'s `r` or `centre`, a `RoundedBox`'s `half` or `r`, a `Plane`'s `normal` or `offset`, a `SmoothUnion`'s `k`, a `Repeat`'s `period`, a `Mirror`'s `axis` — with the route optional: without one it writes the node the editor already holds selected |
| `add_part` | grows the tree one of two ways. `Union`, `Subtract`, `Intersect` or `SmoothUnion` wraps the node a route names in that combinator, joined to a leaf authored on the call (`shape` as `{"kind": "Sphere", "r": 0.4, "centre": [...]}`), and `keep` says which slot the existing node stays in. `Mirror` or `Repeat` wraps it alone — no `shape`, no `keep`, and the wrapper's own `axis` or `period` beside `combine`, from the same table `set_part` writes. `route` optional: without one it wraps the selected node. A wrapper that discards part of what it wraps is not an error — it is legal grammar that draws less than it holds — so the wrap lands and answers with `wrapped.extent`, the box it measured, and `wrapped.notice`, which names the cell or the plane and what the wrap leaves of it; both are null when the wrapper's domain reaches the whole part |
| `delete_part` | a route in, the node out. A one-child wrapper goes by **unwrapping**, at any depth: the node it held takes the wrapper's place, the subtree below it comes along, and the selection stays on the route the wrapper held because that route still names a node — the promoted one. A two-operand combinator cannot hold one operand, so there its surviving operand takes the combinator's place, the tree loses two nodes, and the selection goes back to `root`, because every path under a collapse now means something else. Deleting the sole child of a wrapper is refused, and the refusal names the route that wrapper actually holds as the call that works |
| `move_part` | two routes in, both ends out. It is an **exchange**, not a reparent: the node at `from` and the node at `to` trade places, every other route keeps resolving to the same node it did, and `node_count` is unchanged. Refused: a pair on one line from the root (one holds the other, so one write would land inside the other and a subtree would be lost rather than moved), `root` as either end for the same reason, the same route twice, and two routes holding identical content — an exchange of equals moves nothing |
| `render_view` | marches the current state, writes a PNG to the temp dir, returns the path and the pixel counts. `view` names one of the six angles; only leaving it off steers by `yaw`, `pitch` and `distance`, and the file is then named `custom`. A `view` of any other type — a number, a boolean, an array, an object, `null` — is refused rather than read as "no view" |
| `save_entity` | writes where it is told to. It refuses to default the path, because the path it would default to is `assets/entities/pig.ron` — the engine's own input |

Twelve of those decisions are about honesty rather than features:

- **`set_transform` refuses what `assetc::compile_entity` refuses**, at the same tolerance (`|q|` within
  1e-4 of 1; scale finite and positive; every `position` lane finite), and validates the whole call before
  writing any of it. A
  half-applied edit leaves the panel holding a state the compiler will not take, and the caller's next
  render would show an animal that cannot exist. Assigning an *empty* material slot is refused for the
  same reason: the viewport paints an empty slot unlit white and prints a warning nobody reading a pipe
  would see.
- **A file can carry what the pipe cannot be handed, and the compiler is the gate behind that door.**
  A NaN has no JSON spelling and `set_transform` already refused an infinite lane, so the only way a
  broken placement reaches the state is `load_entity` — which takes the file exactly as it is, on
  purpose, so an author can open a bad `.ron` and repair it rather than being locked out of it. Behind
  that door sits `assetc::compile_entity`, and it checked `rotation` and `scale` and not `position`: an
  authored `position: (inf, 0.0, 0.0)` compiled. The viewport routes its march through that same call, so
  a refusal there is the only complaint an author would ever get — and with no clause for `position`,
  nothing refused: the march ran and answered **sky** (0 hits, 0 undrawn), which an author reads as
  *nothing in the scene today* and not as *your file is broken*. The clause is about finiteness, not
  magnitude: `f32::MAX` is a real
  distance, stays accepted, and draws an empty scene honestly, because how far a placement may sit from
  world zero is OQ-58 and still open. A refused placement now comes back as the same `NOT_DRAWN` frame
  a non-unit rotation already produced, named by `✗ preview refused by assetc: position carries a
  non-finite parameter (NaN or infinite)` on stderr and by `undrawn_pixels` in `render_view`'s reply.
- **`set_part`'s range is the panel's slider range, because it is the same number read from the same
  place.** `crates/editor/src/params.rs` holds one table — each authored field's name, label, lane count
  and limits, and whether the file carries it as an integer — and `inspector.rs` draws its sliders from it
  while `mcp.rs` validates against it. Two written-down ranges would drift into an edit an agent can make
  that the window cannot, which is the disagreement this gate exists to catch. The field names are the
  ones `.ron` files and `assetc`'s refusals use, so a caller can paste either.
- **A structural edit is compiled before it lands.** The three structural tools — `add_part`,
  `delete_part` and `move_part` — build their result on a clone and pass it through `assetc::lower` — the
  same pass the shipped `.bin` comes out of — so the depth cap, a non-unit `Mirror` axis and a NaN length
  are refused with the compiler's own words and the held tree never moves. An editor that wrote a tree the
  compiler refuses would hand an author a file that cannot become an asset and say nothing until the boot
  fails. `move_part` needs that gate more than the other two: an exchange that puts a deep subtree into a
  slot that held a leaf makes the tree deeper on one side, and nothing about the gesture is itself
  illegal — so the cap is the only clause that can see the hazard.
- **Adding is wrapping, and deleting is collapsing.** Every combinator in this grammar is full the moment
  it exists — a `Union` with one operand is not a shape — so there is no free slot to fill, and the only
  honest *add* is a new wrapper around the node the route names. The reply then reads both slots back out
  of the tree it left (`kept` and `added`, each with its route, kind and summary), so `Subtract.kept`
  versus `Subtract.cutter` — a hole and an animal — is stated, not implied. `delete_part` answers with the
  node it removed *and* the combinator that held it. Over a collapse it resets the selection to `root`: a
  path kept across a collapse would edit some unrelated node. Over an unwrap it keeps the selection where it
  is, because the route the wrapper held still names a node — the one promoted into its place — and an
  author who was editing a part before deleting its `Mirror` expects to still be editing that part after.
- **Moving is exchanging, and `root` is not a destination.** For the same reason there is no free slot to
  fill, there is no slot to *put* a node into — the only total move between two routes is a swap, where each
  end takes the other's content. A swap keeps every route outside the two ends resolving to the node it
  already did and leaves `node_count` alone, which is what makes it an edit an author can reason about
  rather than a tree rewrite. Two pairs cannot be swapped: a node and any descendant of it, because the
  write into the outer slot lands inside the node the inner write is replacing, so a subtree is lost rather
  than moved — and `root` is the outer end of every such pair, so it can never be one end of a call; and two
  routes holding identical content, because an answer that reports an exchange and changes nothing is the
  failure this pipe refuses hardest. The reply reads both ends back out of the tree it left — `was` and
  `now`, each with its kind, summary and child count — so a caller sees which content went which way instead
  of inferring it from a route it computed itself.
- **The one-child gesture is a different call, and says so.** A `Mirror` or `Repeat` wraps one node rather
  than joining two, so it has no operand to author and no slot to choose: `add_part` refuses a wrapper
  handed a `shape` or a `keep` instead of quietly dropping the argument the caller did not need, and asks
  for `axis` or `period` the way it asks a `SmoothUnion` for `k` — beside `combine`, validated against the
  same published range. Its reply reports the wrapper at the route the caller named and the node now held
  one level below it, because the wrapped node kept its route and gained a prefix. The inverse is
  `delete_part` taking a one-child wrapper out by unwrapping it, wherever it sits: without it, wrapping a
  node would be the one edit in this pipe an author cannot take back, and the only way out of a `Repeat`
  under a `Mirror` was deleting the `Mirror` over it and everything else it held.
- **A wrapper is an isometry of the *query point*, not of the part it holds.** The compiler folds the point
  into `[-period/2, period/2)` on every axis before the child is evaluated (`render::wrap`, transcribed
  identically in `interp.wgsl`), so a `Repeat`'s copies are of whatever the child has inside that cell — not
  of the child wherever it sits. A `Mirror` evaluates the field at `|p[axis]|`, so it consults its child only
  at `axis >= 0`, and the reflection *takes the place of* the authored negative half rather than adding to it.
  A part authored wholly outside the cell, or wholly below the plane, renders as nothing: the wrap compiles,
  lowers, lands, selects and marks dirty exactly as one that draws would, and only the frame knows. That is the
  grammar's meaning and not this pipe's, so `add_part` does not refuse it — but since increment 9 it does not
  stay silent either. `wrap_part` measures the held part's box with `assetc::order::extent`, the same function
  the union sort keys are built from, so the notice and the layout cannot drift into describing two different
  shapes; it overstates where it must (`Subtract` reports its kept operand, a `Mirror` its child plus the
  reflection), which is the safe direction — an overstated box warns about a part that in fact draws, and never
  blesses one that would be dropped. The wrap then answers with `wrapped.extent` and `wrapped.notice`: the box,
  and a sentence naming the cell or the plane, the part's authored coordinates, and which of "draws nothing",
  "drops the piece outside it" or "loses its negative half to the reflection" applies. A part with no finite
  box — a `Plane`, or a tiling nested inside it — gets the honesty that fits: the notice says the pipe cannot
  tell, and to look at the frame. `order::extent`'s `Mirror` arm was the understating kind of wrong before
  this call existed (doc 09), which is the one place an increment in the editor's pipe repaired the compiler.
- **`render_view` names the frame in the filename** (`{view}-{width}x{height}-{stage|bare}-{preview_signature:016x}.png`),
  so two renders that differ in state *or* in camera, size or stage cannot collide into one path, and a reader
  told "this is the pig after the scale edit" can check that against the signature the same call returns. The
  name keyed on the state alone until increment 10's pixel test rendered the same front view at 64x48 to a file
  another concurrent render had already claimed with the stage on: the two pictures overwrote one path, and a
  reader got a half-written PNG. Two calls with every field equal still name one file, which is intended — the
  bytes are the same picture.
- **The frame reports what it did not draw.** `surface_pixels` counts rays that hit the entity or the
  stage; `undrawn_pixels` counts the `Frame::NOT_DRAWN_DEPTH` sentinel a refused placement leaves behind.
  Counting only the first — which is what the first draft did — reports a frame the march refused to draw
  as one covered entirely by pig. The test for it loads a finite-but-non-unit quaternion from a file, the
  one such state `set_transform` cannot author and `load_entity` can, and requires 0 surface against 768
  undrawn of 768.
- **A material edit says which half of it you can see, because the other half has no reader yet.**
  `set_material` writes one registry slot from the same table `params.rs` hands the inspector, so the
  refusal, the published `inputSchema` and the panel's sliders are one statement: a field the panel cannot
  offer is one the pipe refuses, and a limit quoted in an error is the limit the schema publishes. Only
  `base_color` reaches a pixel — `tint_of` reads it and multiplies the lambert shade — so the reply splits
  what moved into `changed_a_pixel` and `changed_no_pixel` rather than reporting a flat `changed` and
  letting a caller believe a roughness edit drew something. The five that do not are named in the notice
  with OQ-10's reason: inventing their response is the shading model's decision, not the pipe's. The edit
  is also session-only, and says so: `save_entity` writes the `material_id` the entity carries, and there
  is no materials file, so a recolour cannot be something a save carries and does not mark the entity
  `dirty`. And the assigned slot's `base_color` is now part of `preview_signature`, because it is part of
  the picture: before this call existed nothing could recolour a slot, so the key could omit the one
  registry field a shader reads with nothing noticing. The moment a pipe can write it, a key that omits it
  means an author edits a colour and the viewport keeps showing the old animal while every other field of
  the reply agrees the edit landed.
- **A patch and a definition are two calls, because the registry says so.** `set_material` refuses an empty
  slot with the reason the registry itself gives: `MaterialRegistry::set` takes the slot's previous value
  first and only writes when there was one, so an empty slot stays empty and `register` is the only call
  that occupies one. `register_material` is that call's surface, and it therefore demands the whole
  definition: every field the table lists, all at once, validated before the slot is taken. A field left
  off would not be "unchanged" — there is nothing to leave unchanged — it would be `Material::default`'s
  value standing in for one the caller never wrote, which is the silent fabrication the range checks exist
  to prevent. The slot is not an argument: `register` hands out the next free one, which is how the engine
  builds its three presets, and a call that names a slot is refused by name rather than quietly ignored.
  So the reply reports the slot and the entity's *current* `material_id` side by side, and the notice says
  the assignment is the other call — registering paints nothing, because nothing points at the material
  yet, and the `preview_signature` staying put is that sentence measured. A registered material is no more
  durable than a recolour, with one consequence worth stating: an entity saved pointing at slot 3 loads
  into a process whose registry holds the three presets, so the slot is empty and the viewport paints it
  unlit white — which is why the notice names the new process rather than only repeating that the registry
  is not in the file.

A refusal says what was available. `select_part` with a name the node does not carry is refused with the
children it does carry (`root.b has no child "kept"; it names "a", "b"`), and a route into a leaf is refused
with the leaf's own fields and their limits. `set_part` with a field the kind does not have is refused with
the ones it has, and a combinator — which owns no field, only operands — is refused back to its routes. A
value outside its range is refused with that range quoted, and nothing in the tree moved: the checks all
run before the write, so `changed: false` and a clean `dirty` flag are the same fact. The structural tools
refuse in the same voice: `add_part` with `keep: "hole"` answers `Subtract has no slot "hole"; it names
"kept", "cutter"`, a shape field spelled `radious` is refused with the two a `Sphere` does take, a half
`Sphere` missing `centre` is refused by name, a `Mirror` with no `axis` is refused with the one field it
takes and its range, an `axis` of `1.5` is refused as a whole choice and one of `3` as outside `0..=2`, a
`Mirror` handed a `shape` is refused because it joins nothing, and `delete_part` on a wrapper's only child
says which wrapper would be left holding nothing and names the route that wrapper holds — `root.child` for a
`Repeat` under a `Mirror`, not the `root` that is the `Mirror` — because advice that points at the wrong node
is a dead end wearing the shape of a refusal.

The six angles come from `mcp::VIEWS`, one table of `(name, yaw, pitch, distance)` that
`examples/render_png` now reads as well; two lists of the same six would drift, and a reader handed "the
front view" would be looking at a different picture from the one the dump was described by. Frames are
capped at 768 on a side, because this is a CPU march and a 4096² request is a way to wedge a tool that is
supposed to answer.

The camera a frame is taken from has two ways to be asked for, and the tool keeps them apart. An unknown
*name* is refused with all six spelled out (`no view "profile"; the names are "front", "back", …`), because
the caller offered a name and one of the six was meant. A `view` that is not a string at all — `0`, `true`,
`["front"]`, `{"name": "front"}`, `null` — was offered too, and is refused in that same voice rather than
falling into the steer-me-myself arm: beside a complete `yaw`/`pitch`/`distance` camera, the old fallthrough
answered `{"view": 0, "yaw": 3.14159, …}` with a picture from an angle nobody asked for, filed under that
camera's own name (`custom-f68a37b65d24d271.png`, 85 surface pixels of 768) and every field of the reply
consistent with the render that was actually done. Only the key left out means the caller steers, and that is
the whole difference: a missing argument is an instruction, a malformed one is not. `route` and `keep` have
always read the same way — a `null` there answers "must be a string" — and so does the published schema,
which types `view` as a string; the fallthrough was the one place in the pipe where a schema violation could
turn into a picture.

What the pipe still does **not** do: it can exchange two routes but it cannot *re-parent* one — no call
lifts a finished subtree out and grafts it under a different parent while the tree stays whole, because
there is no slot to hold a node in transit. A symmetric creature is still authored by building one side and
wrapping it in a `Mirror`, which is what the shipped pig's tree does; `move_part` rearranges two parts that
already have parents. The one thing a wrapper's sole child still cannot do is go away by itself — deleting it
would leave the wrapper holding nothing, which is the grammar's refusal rather than this pipe's — and since
increment 7 the answer to that is one call, not a subtree: the wrapper over it unwraps wherever it sits, so a
`Repeat` inside a `Mirror` comes out and leaves the `Mirror` standing.
Material fields other than `base_color` are read by no shader
(see the `authored_but_not_shaded` block `editor_state` prints), so `list_materials` reports them,
`set_material` writes them, and writing them changes no pixel.
The registry itself still has no file. A recolour and a registration are both session-scoped — the entity
file carries `material_id` and nothing else about a slot — so an entity saved pointing at a slot a fresh
process does not register loads into an empty slot, which the viewport paints unlit white and warns about.
That is the gap a materials asset would close; it is a contract change, not a tool.

`crates/editor/tests/mcp.rs` is 60 tests — the handshake and a notification's silence, a schema and an
answer for every claimed tool, protocol errors kept distinct from tool failures, the shipped pig's tree
and the five material fields no shader reads, a broken file, a refused edit that moves nothing followed
by a real one that moves `preview_signature`, the two pixel counts, an unknown view naming the six that
exist and a `view` that is not a name refused instead of read as none, a save that round-trips back
through load, and a transport test that runs the real binary as a
child process and reads its replies off a pipe. Increment 2 adds six: a route taken from the parts list
selects the node it prints, does not dirty the entity, and is reached the same way with or without the
leading `root.`; a refused route names the children or fields that exist and leaves the selection where it
was; `set_part` moves one field of one node and the value is still there after `save_entity` and a fresh
`load_entity`; a field the kind does not carry is refused with the ones it does; a value outside the
published range is refused quoting it and moves no node, both included endpoints are accepted, and
re-writing the value a node already holds reports no change — which is the only way to see a field a table
publishes and a writer silently skips; and one test walks all nine authored kinds against names, lane
counts and limits written down *in the test* rather than read back from `params`, counting the nine fields
and the ten named child routes it covered so a kind or a field that stopped being exercised moves the
number. Increment 3 adds seven: `add_part` wrapping a node the parts list names, with the reply's `kept`
and `added` checked against that list rather than against values written into the test, and the tree it
reported reading back identically after `save_entity` and a fresh `load_entity`; `keep` naming which slot
the existing node stays in, and an invented slot refused with the two that exist; nine malformed leaves
refused one by one (no `shape`, a `kind` that needs operands, a missing field, a misspelled one, a radius
past the range, two lanes where three are asked for, a NaN, an infinite, a non-`SmoothUnion` carrying a
`k`); a chain of wraps until `assetc`'s own depth cap speaks, which requires a `Subtract` rather than a
`Union` because `lower` flattens a union chain and rebalances it before measuring, so a `Union` nest cannot
reach the cap however deep the file goes; `delete_part` taking a leaf and its parent and leaving every
other route's kind and summary where they were, round-tripping through the file; the five states a delete
refuses (a root that holds two operands, no route, a blank route, a child the tree does not have, a
one-child wrapper's child); and the frame that follows a structural edit — the render's own
`preview_signature` equal to the add's, a different PNG path, `undrawn_pixels` zero, and the surface count
strictly larger against a `total_pixels` the test first requires to be un-saturated, since a view whose
picture is already all surface cannot show a gain. Increment 4 adds five: a `Mirror` wrapped over a leaf
reports both halves — the wrapper at the route the caller named and the node at `root.child` — adds one
node rather than two, prints that route in the parts list, leaves the wrapped node selectable and
editable through it, refuses `root.a` by naming the slot a `Mirror` does have, and reads back out of the
saved file as the same tree; nine malformed wraps refused one by one (no `axis`, no `period`, an `axis` of
`3` and one of `1.5` and one spelled as text, a `period` under the table's floor, a `shape` handed to a
wrapper, a `keep` handed to one, a `k` handed to one); every kind in `mcp::WRAPPERS` built with a field
named in the test rather than read back from `params`, nested one inside the next so the count proves each
wrap added its one node and the compiler still took the tree; a root that holds one node deleted back to
the node it held, with `preview_signature` equal to the state that went in — wrapping the root and deleting
the wrapper is the identity — beside the two roots that still refuse, a leaf and a `Union`; and the frame
of a mirrored part, which first moves the sphere off the mirror plane, because a wrap of a centred sphere
is the same silhouette and the assertion would pass on a wrapper that did nothing; and the fold cell
itself, where a `Repeat` of period 0.4 over a sphere spanning 0.5 to 2.5 draws a frame of zero surface
against zero undrawn — the march drew the emptiness rather than refusing it — and the same tree at period 4
draws the sphere, so the blank frame is the cell's doing and not a wrapper the marcher ignores.
Increment 5 adds one more, on the compiler's side of the same door: `assetc/tests/lowering.rs` refuses a
NaN, a `+inf`, a `-inf` and a single NaN-in-the-third-lane `position` — each naming `position` as the
route — and accepts `f32::MAX`, `-f32::MAX` and `1e30` in the same test, so the clause cannot quietly
turn into a magnitude cap; and `tests/mcp.rs` loads a hand-authored `position: (inf, 0.0, 0.0)` and
follows it all the way through: the state holds it, the compiler refuses it naming the field, the preview
comes back as the whole frame undrawn rather than as sky, and one legal `set_transform` plus a save and a
reload makes the identical frame draw.
Increment 6 adds eight. An exchange of two of the shipped pig's own routes moves both ends and leaves every
other route's kind and summary exactly where it was — each end's `was` checked against the parts list
captured *before* the call rather than against a value written into the test, the subtree that arrived read
back as the subtree that left, `nodes_before` equal to `node_count`, and the selection on the end the moved
node now sits at; the same exchange reading back out of the file it saves and compiling from there; the same
pair exchanged twice putting the tree back, signature and selection included; every one of the nine authored
kinds carried across an exchange, each seeded by `add_part` and wrapped where it is not a leaf, with the
child count the reply reports written down in the test as 0, 1 or 2 by kind and a counter that moves if a
kind stops being exercised; the four one-line pairs refused — holder-then-held, held-then-holder, and `root`
both ways, those two also naming that `root` contains every route — against a tree that does not move, a
signature that does not change and a `dirty` flag that stays false; seven calls the tool cannot answer refused
in turn (the same route spelled twice, no `from`, no `to`, a blank one, a number where a route belongs, a
child the tree does not have, a step into a leaf) and then the twin the fixture authors — a second sphere
built to the default's exact radius and centre — refused because exchanging equals moves nothing, quoting the
summary it compared; an exchange that would push one branch past the compiler's cap refused with the
compiler's own words and nothing moved, reached by growing the branch until `assetc` speaks rather than by
hardcoding the number; and the picture two operands make, which is a `Subtract` and not a `Union` because a
`Union`'s operands are commutative and its frame would look identical however they were swapped — a sphere
biting a sphere, then the bite exchanged into a crescent, both frames positive, both below their
`total_pixels`, both with zero undrawn pixels and different paths against the signature the move returned.
Increment 7 adds six, and they all sit on the inverse of the wrap gesture. A `Repeat` wrapped inside a
`Mirror` comes out on its own: `delete_part` on `root.child` unwraps it, the `Union` it held takes that route,
and the whole subtree below is required to arrive content for content — every node's authored fields compared
against the parts list captured *before* the call, sorted so the order a combinator's children print in cannot
be what the assertion is actually checking — with the node count down by exactly one, `its_parent.replaced_by`
naming the promoted kind, `its_parent.route` still `root.child`, the selection left on `root.child` rather than
retreating, and the result compiling out of the file it saves. Wrapping a route and deleting that wrapper is
required to be the identity at that depth — same `preview_signature`, same per-route fingerprints, same node
count — and a `set_part` through the re-opened route proves the node underneath is the one that went in, not a
copy of its slot. A wrapper under a wrapper comes out one at a time, from both ends: `Mirror{Repeat{Sphere}}`
loses the `Repeat` and keeps the `Mirror`, then loses the `Mirror` and keeps the sphere, and only then does
`delete_part` on `root` answer that the whole entity cannot go; the same two kinds stacked the other way
unwraps its inner `Mirror` and leaves a `Repeat` holding the sphere with its radius intact. The refusal that
survives is the one for a node that is *not* a wrapper sitting under one that is, and it is tested as advice
rather than as text: it quotes the wrapper's own route, and making the call it names is required to succeed and
to leave the refused node exactly where it stood. And because a `Repeat` draws the tiles of its fold cell, the
frame is measured across the unwrap rather than the tree alone — a 0.15 sphere at x = 0.3 inside a period-1.2
cell under a `Mirror` must show tiled surface (positive, and below a `total_pixels` the test first requires to
be un-saturated, with zero undrawn), and taking the `Repeat` out must leave strictly less of it while the frame
still refuses nothing. That fixture was authored twice: a 1.0 sphere in a 1.6 cell measured **72** tiled
against **709** unwrapped, because the cell clips the part it tiles and the big sphere was mostly outside it —
a frame that grows when a node is deleted is the test failing, not the editor.
Increment 8 adds one, and it sits on the difference between an argument that is absent and an argument that
is malformed. A `render_view` call carrying `view: 0` beside a complete `yaw`/`pitch`/`distance` camera is
required to refuse, and to refuse in the same voice as an unknown name — all six spelled out, plus the
sentence that says how to ask for a custom angle (`Leave "view" off entirely`). The five spellings the field
is given in — a number, a boolean, an array, an object, `null` — each get that treatment, so the test runs
them in a loop rather than picking one; the array and object cases are the ones that would survive a fix
that only swapped `as_str()` for a null check, since both *contain* the word `front`, and `null` is there
because the pipe reads it that way for `route` and `keep` and the schema types `view` as a string. Then the
spelling that does mean "steer myself" is required to still work: the call with the key left out renders the
custom camera, reports `view == "custom"` with zero undrawn pixels, and its file is required to carry the
`custom-` prefix — the name being the half of the old defect that made the reply self-consistent.
Increment 9 adds three rows on the pipe's side and one on the compiler's. `a_repeat_that_drops_the_part_it_wraps_outside_its_cell_says_so`
walks one authored sphere — `r` 0.25 at `x = 2.0`, so its box is `1.75..=2.25` on x — through three periods and
requires a different answer from each: 1.0 opens a cell at `-0.5..=0.5`, wholly clear of the part, and the notice has
to say *none of which lies in it*; 4.0 reaches the part but not past it, and the notice has to say only the piece
inside the cell is tiled; 5.0 holds it entire, and the notice comes back null because there is nothing to report.
`wrapped.extent` is pinned to the part's authored box in all three, so the reply states which of the two boxes it
measured, and each period's own cell bounds are recomputed in the test from the period and required to appear in the
message — a notice quoting some other cell cannot pass. The row then wraps a `Plane`: no finite box means no claim,
so `extent` is null and the notice says the pipe cannot tell and sends the author to the frame.
`a_mirror_that_throws_its_child_to_the_wrong_side_of_its_plane_says_so` runs the same shape of check over the three
axes — a sphere at `x = -2.0` mirrored on `axis 0`, at `y = -2.0` on `axis 1`, at `z = -2.0` on `axis 2` each warns
*wholly on the negative side* and names that axis's `>= 0`, and the same part at positive x is required to report
nothing — and then the straddling case: a `r` 2.5 sphere at `x = -1.0` under `axis 0` has the authored box
`[-3.5, -2.5, -2.5]..=[1.5, 2.5, 2.5]`, which the reply is required to print, and the notice has to say the plane is
crossed and that the reflection replaces the authored negative half rather than adding to it.
`what_a_wrapper_discards_is_exactly_what_the_marched_frame_loses` is the row that stops the other two from being a
description of a claim instead of a check: it asks the same question of the march. A part at `y = -0.6`, `r` 0.4,
draws surface unwrapped; the `axis 1` mirror over it warns *draws nothing* and the very next `render_view` is
required to answer `surface_pixels == 0`; the same part moved to `y = +0.6` gets a null notice and draws again. The
stage has to be switched off for that — `render_view` draws the preview stage by default, and the first live run of
this increment reported 3,501 surface pixels for a wrap that had just been told draws nothing, which was the
instrument disagreeing rather than the notice. On the compiler's side,
`an_authored_extent_contains_every_point_the_lowered_asset_draws` states the containment invariant the notice leans
on and cannot see: over the 288-point lattice the suite already uses, every sample the *lowered* tree reports as
inside (`sdf_eval` at or below zero) is required to lie within the box `extent` claims for the *authored* shape — for
a mirror of an off-plane pair, a mirror below a union, a blend, a difference and a rounded box. It pins the mirrored
pair's box as `[-2.0, -0.5, -0.5]..=[2.0, 0.5, 0.5]`, which is the child's `0.5..=2.0` joined with its own
reflection, and it pins `None` for a `Plane` and for a `Repeat` rather than a guessed reach.

Each of those checks was run against a planted defect in the clause it is meant to police, on this machine,
and each plant is recorded with the one test it named: dropping `assetc::lower` from `commit` named the
depth-cap test; letting an unknown shape field through named the nine-refusal test; defaulting an unknown
`keep` to the first slot named the `keep` test; promoting the deleted node instead of its sibling named the
collapse test's summary line; keeping the pre-delete selection named the same test's `selected` line;
disabling the one-child guard named the refusal test; and pointing `render_view` at an unrelated entity
named the frame test (and the two older render tests, which is what a shared instrument should do).
Increment 4's nine plants ran the same way: deleting the wrapper's range gate named the malformed-wrap
test; never writing the published field into the wrapper named the report test and the every-wrapper test;
letting a wrapper accept a `shape` named the malformed-wrap test, and letting it accept `keep` named the
same test's other case; restoring the old "root is never deletable" answer named the unwrap test; turning
the root's one-child guard off named that test *and* the shipped pig's refusal test, because a `Union` root
walked to a `child` step it does not have; and assigning the wrapped node's own value back over the route —
a wrap that reports success and changes nothing — named the frame test, the report test, the unwrap test
and the every-wrapper test. The fold-cell test was planted in the renderer rather than in the pipe: making
`render::wrap` return its input named it with 474 surface pixels in a frame that should hold none, and
making the `NODE_REPEAT` arm answer `f32::MAX` — a tiling that never draws — named it from the other side,
on the widened period-4 assertion.
Increment 5's three plants sat on the clause itself: renaming its route to `centre` named both new rows at
their `location()` line; widening it to `lane.abs() < 1e6` — a magnitude cap the compiler must not become,
because OQ-58 owns the range question — was then planted alone and named only the `lowering` row's control
half, which is exactly the check the editor's row cannot express; and deleting the clause named both again,
one at its `{label} position compiled` panic and one where it asks for the refusal.
Increment 6's six plants ran the same way: turning the one-line guard off named only the refusal test;
writing only the end the node came from — the far content lands at `from` while `to` keeps its own, so a
subtree is duplicated rather than moved — named the
both-ends test, the every-kind walk, the cap test and the exchange-it-back test, but **not** the frame test,
because a `Subtract` whose two operands are equal marches as a zero-thickness shell rather than as nothing,
so its surface count stays positive and its crescent assertion never fires; selecting the end that vacated
instead of the one that arrived named that test's `selected` line and the pig test's; disabling the
equal-contents refusal named only the malformed-call test; reading the reply's child count off the content
that left rather than the one that arrived named only the every-kind walk, which is the one place a leaf and
a combinator are told apart by the number beside the route; and writing the candidate into the state without
`commit`'s compile named only the cap test — the row that says the exchange path really does go through the
gate the other two structural tools use. Every plant was
reverted and the suite re-run green afterwards.
Increment 7's plants are recorded one by one, because the sixth of them is the reason the increment has a
sixth test: allowing the unwrap only at the root (adding `&& steps.is_empty()` to the one-child condition)
named the four nested-unwrap tests; retreating the selection to `root` from the unwrap arm named the two rows
that assert `selected` is the promoted route; resolving the survivor one level too deep (`[Sole, Sole]`, the
child's child rather than the child) named all six unwrap rows including the two root ones, since the slot it
writes is one the wrapper's grandchild may not exist in; making the rule `Mirror`-only instead of
one-child-any-kind named exactly the four rows that carry a `Repeat` and left every `Mirror` row green, which
is the difference between a rule and a kind being what the suite polices; and deleting the parent-wrapper guard
from the refusal arm named the two refusal rows — including the older one, `a_node_that_cannot_be_deleted_says_why_and_leaves_the_tree_alone`,
which panics at the slot the guard used to refuse rather than answering with a sentence. The sixth plant was
the one worth the round trip: writing `"root"` where the message interpolates the wrapper's own route passed
the suite at 44 **green**. It is not a cosmetic slip — in `Mirror{Repeat{Sphere}}` it tells the author to
delete the `Repeat` *at the root*, and the root is the `Mirror`, so following the advice takes out a different
node than the one in the way. The row that cannot see it is a hole, so increment 7 carries a test for it:
`a_nested_wrappers_refusal_names_the_wrapper_where_it_actually_is` asserts the quoted route is `root.child` and
requires that the message not say `at "root"`, and re-running the plant against 45 named that one row and
nothing else. Every plant was reverted and the suite re-run green afterwards.
Increment 8's plants were the defect itself, restored, and then its narrower half. Merging the third arm
back into the second so a non-string `view` falls through to the custom camera named the new row and nothing
else, and its panic carried the whole old behaviour as evidence — `{"view": 0, …}` answering with
`custom-f68a37b65d24d271.png`, 85 surface pixels of 768, and a `yaw` of `3.1415927410125732` in a reply whose
every field agrees with the render that was actually done. The second plant left the refusal in and made only
`null` read as no view — the defect a fix that treated null as absence would have shipped — and named the
same row from its `null` entry with the same picture in the panic. No frame assertion anywhere in the suite
can see either: the picture a fallthrough draws, its path and its pixel counts are exactly what the same
camera produces when it is asked for legitimately, so the only observable is whether the call answers with a
result or with a refusal. Both reverted and the suite re-run green afterwards.
Increment 9's three plants each named the row that polices the clause they broke, and the third is the reason
the increment has a compiler test in it. Hard-coding the cell half to `0.5` instead of reading it from the
`period` the call was handed — a notice that quotes a cell the wrap does not open — named
`a_repeat_that_drops_the_part_it_wraps_outside_its_cell_says_so` alone, 48 green. Reading every `Mirror` as
`axis 0` — the wrapper consulted on the wrong side, so a `y` or `z` mirror of a negative part says nothing and an
`axis 1` mirror of a part below it claims the plane is crossed — named **two**: the prose row and
`what_a_wrapper_discards_is_exactly_what_the_marched_frame_loses`, whose `surface_pixels == 0` assertion is the
march's own answer to the same question; that is the honest report, not one row per plant. The third plant was in
the compiler rather than the pipe: reverting `order::extent`'s `Mirror` arm to the child alone — the understating
box this increment found and fixed — named the assetc row and nothing else, at
`a mirror of an off-plane pair draws the solid point [-2.0, 0.0, 0.0], outside the box [0.5, -0.5, -0.5]..=[2.0,
0.5, 0.5] it claims`, while all 49 editor rows stayed green beside it. An understated box is invisible to the
notice that is built from it: the warning still fires, still quotes a box, and is simply wrong about the geometry,
which is why the containment invariant had to be stated where the box is. All three reverted, files compared
byte-for-byte against the pre-plant copies, and both suites re-run green afterwards.
Increment 10 adds six, and the first of them exists because a material write exposed a defect in
`render_view` rather than in a material. The pixel row renders the front view with the stage off — the stage
floor is not tinted, so with a black `base_color` every pixel is either black or the sky — recolours slot 0
black through `set_material`, and requires the black count of the decoded PNG to equal `surface_pixels` and
the sky literal's count to equal the remainder: the two counts partition the file, so the tint is shown to
reach exactly the pixels the depth buffer calls surface, not merely that the bytes differ. The same row
asserts that `preview_signature` moved, because without that the viewport would keep the old frame. That
assertion was not true of the key as it stood: `preview_signature` hashed `material_id` and not the slot's
colour, and nothing could recolour a slot until this call existed, so the blind spot was unreachable rather
than harmless. While the row was being written, two tests in the file rendered one state at once — both at
64×48, one with the stage and one without — and one of them decoded a half-written PNG: `render_view` named
its file from `{view}-{signature}` alone, so the same state at 32×24 bare, 32×24 staged and 48×36 bare was
three pictures under one path. The name now carries
width, height and stage, and the sixth row pins it: three calls of one state agree on the key, write three
distinct files, and each file is the size its own call asked for.

The five fields no shader reads get their own row, and each is written with a value read out of the slot
first, so the row cannot pass by writing what was already there. Every write is observed three ways — the
reply's `changed` names exactly that field, the registry read-back holds the value, and `preview_signature`
stays where it was with `changed_a_pixel` empty — and after all five the dumped PNG is byte-identical to
the baseline frame, with a blue `base_color` control at the end so that "identical" is a fact about the
five fields rather than about an instrument that never moves. The refusal row walks fifteen calls — an
empty slot, an unknown field, a missing, string, fractional or out-of-range slot, a two-lane or non-numeric
colour, a roughness past its contract, a negative lane, a zero frequency, an ior below air, a noise kind
given as a number and one given as a misspelled string, and a call with a slot and nothing to write — and
after each it reads the whole registry and the state back, requiring that neither moved; the sixteenth call
writes a value the slot already carried and answers with the no-op sentence. The session row saves and
reloads: the entity's slot assignment comes back, and the slot's colour is the preset again, which is the
honest statement of what a save can carry. The schema row reads `tools/list` and compares the published
`inputSchema` to literals — the six fields, their limits and the noise kinds in `NoiseKind`'s declaration
order — rather than to the table, so a limit that moves has to be written down in the test as well as
published.

Ten plants ran. Ignoring the tint in the renderer, and dropping the slot's `base_color` from
`preview_signature`, each named `set_material_paints_base_color_into_exactly_the_pixels_the_march_reports`;
moving the sky literal named the same row's second count. Marking `roughness` as shaded and skipping the
`noise` write each named the five-fields row, by its `changed_a_pixel` and by its registry read-back
respectively. Skipping the `roughness` ceiling check and disabling the no-field refusal each named the
refusal row; the call the second plant let through answered "Every value you wrote is the one the slot
already carried" to a call that wrote no values, which is the sentence that made the refusal worth adding
rather than a nicety. Marking the entity dirty on a write named the two rows that assert `dirty` is false.
Reordering the schema's noise enum named the schema row. Restoring the old filename named the filename row.
All ten were reverted and the suite re-run green at 309.

The increment names two limits instead of working around them. The five unread fields are writable because
the registry is the contract's object, but OQ-10 owns whether they shade anything, so the pipe reports them
as no-pixel changes and does not invent a response. And the registry has no file: the entity's `material_id`
persists, the slot's contents do not, so a material edit is session-scoped until someone decides what a
materials asset is — a contract change, not a tool.

Increment 11 gave the registry's empty slots a way in, and it is deliberately the opposite shape from
`set_material`. That call refuses an empty slot with the registry's own reason — `set` will not write where
nothing was — so the missing verb was a *definition*, not a patch. `register_material` therefore requires all
six fields at once and validates every one of them before the registry moves: a field left off would not
mean "keep what was there", because there was nothing there, it would mean `Material::default`'s value
standing in for something the caller never wrote. The slot is not an argument, either; `register` hands out
the next free one, which is how the engine builds its three presets, so the call that names a slot is refused
by name and the reply reports where the material landed. Registering paints nothing — `preview_signature`
does not move — and the notice says why: the entity still points at slot 0, and `set_transform`'s
`material_id` is the call that assigns the slot. The five rows measure exactly that split: the panning row
registers slot 3, reads every field back out of `list_materials`, requires the key to stay put, then assigns
the slot and requires the frame's black pixels and sky pixels to partition it against the march's own
`surface_pixels`; the refusal row walks six definitions that are not whole and reads the registry, the dirty
flag and the key after each, then shows the refusals consumed no slot by landing the next whole call on
slot 3; the schema row requires the definition to publish all six fields, the same per-field schemas the
patch publishes, and to refuse `slot` the way `additionalProperties: false` says; the persistence row saves
an entity pointing at the new slot and requires the file to carry `material_id: 3` and none of the colour,
roughness, ior or frequency, with a fresh state loading that file to an *empty* slot 3 — which is the new
consequence of the session-only registry, and the reason the notice names the new process rather than only
the file; and the last row fills slots 3–255 until the 257th material is refused by the slot count.

Four plants ran against these five rows, and each named its own. Skipping the `noise` write named the
registration row's read-back (`"None"` where `"Value"` was asked for). Letting a partial definition past the
completeness check named the refusal row — the fallback still refused, so the plant showed the difference
between refusing and refusing with the sentence the caller needs. Reporting the new slot as the entity's
`material_id` named the registration row's assignment assertion. Taking the unknown-key loop out named the
schema row's `slot` refusal, which is the one place `additionalProperties: false` is executed rather than
printed. All four were reverted and the suite re-run green at 314.

The dumps live in `temp_dir` (§6), and the march is the CPU reference, so what it prints describes
authoring and never a frame budget (§7): the 64×48 front view of the 36-node `pig.ron` measured 494 ms in
a debug build on this machine, on the run that landed this section.

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
| E7 | An agent reaches the same state the window edits over MCP, with no window, device or human: refusals refuse, a part is editable by route inside the same limits the panel's sliders offer, a node is added, wrapped, removed — a wrapper at any depth taken out by unwrapping what it holds — and exchanged with another route only in a tree `assetc::lower` still compiles, a wrap that discards part of what it holds says which part and against the same box the march measures, a render follows the state and refuses a `view` it cannot name, a material slot's fields are written from the same table the schema publishes — the one shaded field reaching exactly the pixels the march reports, the five unread ones moving the registry and neither pixel nor key — a new material is defined in full, held to the same table, and paints nothing until the entity is assigned to its slot — and the transport answers on a pipe | `crates/editor/tests/mcp.rs` — 60 tests |

## Migration from current code

The temporal cache already has `EntityRange` for tracking tree node ranges. The migration is:

1. Move `EntityRange` from `render::temporal_cache` to `entity::component`
2. Replace hardcoded demo scene entities with spawned entities
3. Wire `EntityManager::update()` into the render loop (before `march_camera_variant_no_readback`)
4. Add dirty mask invalidation calls when entities move

No breaking changes to existing APIs — the entity framework is additive.
