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
  rather than a silently misinterpreted kind table.

`assetc` still does not exist, so nothing emits this format yet - the missing piece is the compiler, not
the format. Its gates (AS1-AS5) stay open, and the headless WGSL check (S6, OQ-41) is still what stands
between an authored tree and a drawn one.

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

## Validation gates

| Gate | Test | Source |
|------|------|--------|
| AS1 | `pig.ron` → `.bin` → GPU buffer with zero runtime parse, hot-reloaded | ADR-014 |
| AS2 | Identical `.bin` bytes from identical source on two machines (deterministic serialization) | OQ-30 |
| AS3 | Schema violation fails the build with a span-accurate message | doc 12 agent loop |
| AS4 | A `.mpack` loads with no allocation churn and no parse step | ADR-015 |
| AS5 | An over-budget SDF is rejected at compile time | doc 04 budget |

## Open questions

[OQ-30](13-risks-open-questions.md) schema versioning/migration, and with it the **stable asset IDs**
of § Schema & versioning item 3 — namespacing and collisions between mods are not a separate
question yet ·
[OQ-27](13-risks-open-questions.md) whether procedural noise is authored as a node tree or a
precomputed texture, and who owns that call.
