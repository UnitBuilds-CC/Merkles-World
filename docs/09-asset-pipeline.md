# 09 — Asset pipeline

**Status:** pipeline locked; schema versioning and authoring tooling undecided.
**Depends on:** docs 02, 03, 04, 08. **Implements:** ADR-014, ADR-015.

## The split that makes this work

The objection to binary formats was correct and it is the reason this doc exists: *"wouldnt that
make it less modder friendly?"* — yes, if humans had to hand-write byte offsets. So authoring and
runtime are different layers with a compiler between them.

```
┌──────────────────────────────────────────────────────────────┐
│ AUTHORING      what modders and agents write                  │
│   RON (canonical) · JSON (accepted alias) · KDL · TOML · code  │
└──────────────────────────┬───────────────────────────────────┘
                           │  assetc: parse once, validate vs schema, emit binary
                           ▼  (<5 ms in dev; batch at build)
┌──────────────────────────────────────────────────────────────┐
│ RUNTIME        what the engine actually reads                  │
│   FlatBuffers (zero-copy) · static WASM memory · u64 records    │
└──────────────────────────┬───────────────────────────────────┘
                           ▼  memcpy / DMA
                     GPU storage buffers
```

**The rule: the engine runtime never parses text.** A format that gets parsed at load is a format
that gets parsed at load — during gameplay, in a hot loop, on a browser main thread. Text exists
only to the left of `assetc`.

## The shape envelope exists (A-07, DRAFT)

`contracts/sdf.fbs` is the container doc 09 assumed: a `ShapeAsset` holding `grammar_version`, the tree
depth, a vector of fixed-layout `NodeRecord` structs and the `f32` parameter pool, with `root_type`, a
`file_identifier` of `MLWS` and a `.bin` extension. Two decisions make it a compiled blob rather than a
parsed document:

- **Nodes are FlatBuffers *structs*, not tables.** A struct is fixed-layout, inline and little-endian, so
  the compiled file hands WGSL exactly the 16-byte record `contracts::sdf::NodeRecord` describes -
  no repacking step, no vtable walk on a per-pixel path. `flatc --rust` generates
  `pub struct NodeRecord(pub [u8; 16])`, which is the stride claim coming from a compiler instead of a
  comment.
- **`grammar_version` is refused at load, never hashed.** Shape files are presentation data (ADR-028,
  A-03, A-10 own what a peer must agree on); the field exists so a mismatched asset is one comparison
  rather than a silently misinterpreted kind table. `reader::from_bytes` makes that comparison before it
  parses a single node and returns `GrammarVersionMismatch { found, expected }` - the version decides what
  every other byte *means*, so reading on would produce a plausible tree instead of an error. The
  comparison is exact equality against `contracts::sdf::GRAMMAR_VERSION` today, because the accepted
  window and migration strategy are still OQ-30's; when a window exists it belongs to
  `AssetError::SchemaVersionUnsupported` in A-09, not to a policy invented in the loader.

`assetc` exists in three stages. `crates/assetc::lower` turns an authored `Shape` into the pre-order
`NodeRecord` array and `f32` pool the envelope carries, deriving `subtree_len` from the children
instead of asserting it and refusing a bad axis, a non-finite parameter and a tree past
`MAX_TREE_DEPTH`. Before any of that, `crates/assetc::order::rebalance` normalises union forests: the
children of a plain `Union` are flattened into a set, sorted along the widest axis by their extents and
rebuilt balanced, so eight spheres authored as a chain arrive at the marcher as depth 4 instead of 8 and
with spatially coherent bounds. It is a semantics-preserving transform and is tested as one — a chain and
a balanced tree of the same four spheres lower to *identical bytes*, and the lowered field equals the
minimum of those spheres computed in the test, at 288 sample points. The scope is deliberately narrow:
`Subtract`'s operand order is its meaning, a `SmoothUnion`'s children are not free to move past its blend,
and a `Plane` or `Repeat` has no finite extent to sort on, so those forests are left as authored rather
than given an invented coordinate. The reason to do this at all is measured in doc 04: tree shape is worth
2.6× to 3.0× of the renderer's per-node culling, and an author should not have to know that.
`order::extent` — the AABB that transform sorts on — is published rather than private, because the editor's
MCP pipe measures a wrapped part against it before it says anything about what the wrap leaves. Its contract
is then stated out loud: the box **contains** every surface the lowered asset can draw, and `None` means no
finite box may be claimed. Overstating is sound and understating is the bug, and the `Mirror` arm understated:
it reported its child's own box, which is harmless for a sort key and wrong for a claim, because a `Mirror`
evaluates its child at `|p[axis]|` and so draws the reflection of the side it keeps. The arm now joins the
child with its own reflection across the plane through zero — which is the enclosure `crates/render/src/cull.rs`
independently builds for a `NODE_MIRROR`, from the same axis and with the same clamp. Making that arm honest
moves no byte of anything this repo ships: `tools/check_boot_digest.sh` on the repaired `extent` measures
`db71f8a0bf5818d6…`, the published whole-output digest, unchanged. The tests in `crates/assetc/tests/lowering.rs` check the lowered tree against
`crates/render`'s hand-built scene, so a compiled tree answers the same distance questions as the
tree an author would have written by hand. `crates/assetc::writer::to_bytes` serialises that into real `ShapeAsset` bytes.
`crates/assetc::reader::from_bytes` is the load side doc 09 has always asked for: walk the root
offset, check the `MLWS` identifier, resolve each field through the vtable rather than by position
(a field equal to its schema default is absent from the table, so positional reading would return the
wrong number instead of an error), and hand back nodes, parameters and label. The `assetc` command
ties the first two together - `assetc <in.ron> <out.bin>` reads RON (ADR-014's authoring format),
lowers it, runs `render::validate` on its own output, and writes the asset, refusing loudly on
unparseable input rather than emitting a plausible file.

Two external checks, in both directions. CI compiles the fixture through the writing path and hands
the bytes to `flatc`, which reads them back with its own parser - the only verdict on the format that
does not come from the code that wrote it. And `crates/assetc/tests/asset_round_trip.rs` feeds the
reader a file `flatc` packed, committed as a fixture because a test cannot run a compiler it does not
have: a reader validated only against its own writer can share a mistake, one validated against
`flatc`'s output cannot. Seven refusals are tested too - a buffer too short for a header, a wrong
identifier, a root offset past the end, a vtable that does not fit below its table, a vtable from a
wider schema, a container declaring another grammar version, and a header declaring a tree deeper than
A-07's `MAX_TREE_DEPTH` - because a loader that guesses at a malformed asset shows the player a wrong
world instead of an error. The last two are the envelope's own stated purpose: `grammar_version` and
`depth` are carried *to be compared before anything is walked*, and both are, in that order - the kind
table decides what every later byte means, and the depth decides whether the marcher can budget the
tree at all. Note what the depth check is not: AS5's gate is `assetc` rejecting an over-budget tree at *compile* time, which needs a per-tree budget
the docs do not yet define. **The mechanism exists** (`crates/assetc/src/budget.rs`):
`assetc in.ron out.bin --budget nodes=32,depth=8,evaluations=6144[,steps=96]` reads the cost off the
*compiled* records - nodes, depth, pool entries, and how many of them are unbounded kinds that defeat
the ray early-out - and refuses on every clause the shape trips at once. A refusal behaves exactly like
a compile failure: the asset on disk is untouched, the watch loop's fingerprint does not advance, so a
later edit that fits is recompiled rather than dismissed as unchanged. The per-ray ceiling is doc 04's
cost law, `steps x nodes`, and `tests/budget.rs` checks that it actually bounds: a 25-record mixed tree
marched as a real 32x24 frame measured 88 evaluations per ray against the 2,400 the bound allows -
27.3x of headroom, loose in the safe direction rather than tight enough to be a prediction. **The number
is still not chosen here**: setting a default budget means setting a frame-cost claim, and gate R1 is a
GPU statement with no GPU behind it yet.

What still keeps AS1-AS5 open: `engine <asset.bin>` boots from a file the compiler wrote - the test
writes one with `assetc::writer` and checks the boot reports that file's own header and marches the
surface the file describes - but there is no GPU upload, no watch or hot-reload mode, the authoring
file schema is not bound to a named format version, and AS5's ceiling flag exists and refuses without writing, but no default budget is set (the number is the owner's call, so the gate stays open rather than passing on a made-up threshold: nothing rejects an over-budget SDF at compile
time. The headless WGSL check (S6, OQ-41) is still what stands between an authored tree and a drawn
one.

## Formats

| Role | Format | Notes |
|------|--------|-------|
| **Canonical in-repo authoring** | **RON** | trailing commas, comments, maps 1:1 onto Rust structs (Bevy ecosystem convention), existing parser |
| **Accepted alias input** | JSON | LLMs and web developers emit it reliably; `assetc` maps it to the same schema. Not canonical: no comments, no ordering guarantees |
| Backlog | KDL, TOML | KDL suits node trees visually; TOML suits flat configs. Ingest both when demand appears |
| **Runtime container** | FlatBuffers (+ FlexBuffers for genuinely dynamic payloads) | zero-copy byte-offset access, direct GPU upload, `.fbs` generates Rust/WASM/Python/C++ bindings |
| **Entity/material/biome data in mods** | static `#[repr(C)]` exports from WASM | `O(1)` pointer read from linear memory; the compiler enforces the type, so no runtime schema errors |
| **Ledger delta records** | bitpacked `u64` | SIMD-hashable, minimal storage — layout owned by [doc 02](02-coordinate-system.md)/[doc 03](03-state-ledger.md) |

**Rejected:** Cap'n Proto (its mutable zero-copy buffers solve a problem we don't have;
FlatBuffers' tooling and GPU-upload story win); BSON (still needs a parse step); XML/YAML
(nobody wants either); runtime JSON (the specific thing ADR-014 exists to avoid);
hand-authored binary (un-debuggable); JSON-only authoring (needless modder lock-in, and JSON is
a poor fit for node trees); one blessed text format (same).

## Authoring example

```ron
// assets/entities/pig.ron
Entity(
  id: "pig",
  sdf_tree: SmoothUnion(
    k: 0.1,
    children: [
      RoundedBox(size: (0.5, 0.4, 0.3)),                       // body
      Sphere(radius: 0.25, offset: (0.0, 0.3, -0.4)),          // head
      Cylinder(radius: 0.1, height: 0.1, offset: (0.0, 0.2, -0.6)), // snout
      Repeat(Mirror(axis: X, Cylinder(radius: 0.08, height: 0.2))), // legs
    ],
  ),
  material: ProceduralNoise(kind: Voronoi, base_color: (1.0, 0.7, 0.75)),
  logic: "wasm:pig_ai.v0",
)
```

A dragon is this file plus a WASM module — a few kilobytes of text, instead of the 50 MB of
models and textures the traditional pipeline would ship. That ratio *is* the modding story.

## `assetc` — the tool

Owns: parse (RON/JSON/…) → validate against schema → lower to FlatBuffer/WASM data → write the
`.bin` cache → report errors with source spans. Two modes:

- **watch** (dev): file watcher, per-file recompile, hot-reload signal to the engine, non-zero
  exit on schema violation so agents get a clean failure to fix.
- **pack** (release): whole-mod compile into a `.mpack` (FlatBuffers blob + manifest + content
  hashes + signature). Ships light, loads with zero parsing.

`assetc` is Rust, is not on the frame path, and may use whatever libraries it likes. It is also
the tool the agent swarm uses to self-validate generated content, which makes its error messages
part of the agent feedback loop (doc 12) — write them for a model, not just for a human.

## Schema & versioning (**[OQ-30](13-risks-open-questions.md)**)

The FlatBuffer `.fbs` schemas are the **source of truth** and live in `contracts/`. Open
decisions before Phase 1 ships content:

1. **Versioning:** embed a schema version in every `.bin`; the loader must accept the current
   version plus some window of older ones (old worlds outlive engine updates).
2. **Migration:** re-compile old mods on save-load (simple but silently rewrites content) vs.
   versioned loaders (more code, no surprises). Leaning: recompile with a reported diff, since a
   mod's text source is the durable artifact and the binary is a cache.
3. **Stable asset IDs** so mods can reference each other's content without collision.
4. **Deterministic serialization** — byte-for-byte reproducibility of a `.bin` from the same
   source, or asset hashes can't be used as epoch inputs.
5. **Schema evolution rules:** additive fields with defaults only; removals require an ADR.

## Error reporting

Authoring errors must be good, because both modders *and* agents hit them: source file + line +
column, schema path violated, and a pointer to the doc section. `assetc` also runs a
**budget pass** (doc 04's SDF complexity/step count) so an over-complex entity is rejected as a
budget violation rather than discovered as a frame drop.

'`assetc watch`' exists ('crates/assetc/src/watch.rs'), and its two rules are the ones that make a reload
loop safe to run while the engine is reading the asset. Change detection is a fingerprint over the
authoring bytes rather than an mtime, because a mtime is coarse enough to miss a save and loose enough to
fire on one that changed nothing. Replacement is atomic - bytes go to '<output>.tmp' and are renamed only
after a successful compile - and a failed compile leaves the previous asset on disk, untouched and
*unmarked*: the fingerprint does not advance, so repairing the file rebuilds. That last part is the whole
reason the module exists. A watch loop that reports success over a stale asset is how an author ends up
deleting a shape twice, and the test for it (`crates/assetc/tests/watch.rs`) asserts the bytes of the old
asset, the un-advanced fingerprint, and that no '.tmp' file survives a refusal. '--once' gives the same
state machine a deterministic, assertable shape for tests and CI.

What it does not settle: AS1's own words end at "→ GPU buffer", and the buffer needs a device, so AS1
stays unmet with **OQ-51** and **OQ-48** - the second has no gate for this path at all, which is said in
that question rather than papered over by a passing test suite.

## Validation gates

| Gate | Test | Source |
|------|------|--------|
| AS1 | `pig.ron` → `.bin` → GPU buffer with zero runtime parse, hot-reloaded | ADR-014 |
| AS2 | Identical `.bin` bytes from identical source on two machines (deterministic serialization). **Measured 2026-10-12:** CI compiles `sphere.ron` and `knot.ron` on x86-64 Linux, x86-64 Windows and ARM64 macOS and the following job fails unless the digests agree (`4f9da1a4…`, `52b85b4b…` on all three at `38302996493`). `knot.ron` exists for this gate: twelve nodes, depths to five, and values a parser is entitled to round differently - 0.1, 0.3, -0.0, 1e7 - plus the union ordering `order::rebalance` derives from `f32` sort keys. | OQ-30 |
| AS3 | Schema violation fails the build with a span-accurate message | doc 12 agent loop. **Advanced 2026-10-13:** syntax failures already carry byte spans from the parser (`1:32-1:33: Expected opening \`(\``), and semantic refusals now carry the **authored route** - `Union.b.Mirror.child.Subtract.kept.Sphere carries a non-finite parameter (NaN or infinite)` - because a shape can be perfectly well-formed RON and still be refused, and the walker knows where. `tests/lowering.rs` pins the route for three refusals and asserts the reload loop reports the same sentence and writes nothing. **Advanced 2026-10-23:** `compile_entity`'s two *placement* clauses used to borrow the shape walker's sentence, so a finite-but-unnormalised quaternion and a zero scale were both reported as "carries a non-finite parameter (NaN or infinite)" — a message that sends the author looking for a NaN that is not in the file, when the two mistakes have opposite fixes (one is deleted, the other normalised). They are now their own kinds, `NonUnitQuaternion` ("`rotation is not a unit quaternion: x² + y² + z² + w² must equal 1`") and `NonPositiveScale` ("`scale must be greater than zero; nothing has an inside at that scale`"), and each clause tests finiteness *before* the clause-specific check, because `(NaN - 1.0).abs() > 1e-4` is true and would otherwise report a NaN as an unnormalised rotation. `a_placement_refusal_names_the_clause_that_moved` pins all three readings. What is *not* done: a byte span for semantic violations, which needs RON's spanned deserialization (a dependency decision), so the gate stays open on that half. **The same day, the gate's third placement field was found missing:** `compile_entity` tested finiteness on `rotation` and `scale` and passed `position` straight through, so a hand-authored `position: (inf, 0.0, 0.0)` compiled — and because the editor's viewport marches through this same call, the broken placement never reached the viewport's refusal path either: it marched and answered **sky** (0 hits against 0 undrawn), which reads as an empty scene rather than a bad file. `position` now refuses for finiteness only (`position carries a non-finite parameter (NaN or infinite)`), which is why the editor's pipe could refuse a lane the compiler accepted, and why `the_placement_gate_refuses_a_non_finite_position_and_not_a_far_one` pins both halves — four non-finite placements refused by name, `f32::MAX` / `-f32::MAX` / `1e30` accepted, because how far a placement may sit from world zero is OQ-58 and stays open. |
| AS4 | A `.mpack` loads with no allocation churn and no parse step | ADR-015 |
| AS5 | An over-budget SDF is rejected at compile time | doc 04 budget |

## Open questions

[OQ-30](13-risks-open-questions.md) schema versioning/migration, and with it the **stable asset IDs**
of § Schema & versioning item 3 — namespacing and collisions between mods are not a separate
question yet ·
[OQ-27](13-risks-open-questions.md) whether procedural noise is authored as a node tree or a
precomputed texture, and who owns that call.
