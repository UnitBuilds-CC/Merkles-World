# Working rules — Voxel-Ledger Engine / Project Genesis

These rules apply to every LLM agent and human touching this repository. They exist because
the architecture's guarantees (determinism, O(1) hot path, zero art pipeline) are only broken
by careless code, not by hard problems.

## 1. Read before you write

1. `docs/13-risks-open-questions.md` — if your task touches an **open** `OQ-NN` or `SPEC-NN`,
   stop and resolve it or flag it; do not code around it silently.
2. `docs/01-architecture.md` — your layer must fit the contract table.
3. Your subsystem's doc (`docs/02`…`docs/10`) and the relevant ADRs in `docs/decisions.md`.

The docs are the spec. The transcript in `~/Documents/How did minecraft get popular.txt` is
the *origin*, not the spec — if they disagree, the docs win, and the disagreement gets logged.

## 2. Contract-first, then implementation

- `contracts/` holds type definitions, WIT interfaces, FlatBuffer `.fbs` schemas, WGSL
  function signatures, and error enums. It is the **only** shared surface between agents.
- Only the Architect role edits `contracts/`. Everyone else codes against it.
- A contract change requires: migration note + version bump + an ADR entry. Never edit a
  contract to make your own test pass.
- No cross-module reach-through: talk to another module only through its contract.

## 3. Hot-path bans (non-negotiable)

During gameplay frames, none of these may run:

- String parsing (JSON/TOML/RON/KDL), `malloc`/`free` churn, regex, reflection, GC pauses.
- Reading loose files from disk.
- Anything that resolves a `RegionKey` by hashing a string.

Runtime input is pre-compiled zero-copy binaries (doc 09) and raw slab pointers (doc 03).
Tools (CLI compiler, packer, validators) may use anything they like — they are not on the hot
path.

## 4. Determinism discipline

- **Anything another machine must reproduce bitwise** — ledger hashes, epoch roots, material IDs,
  circuit results, macro field values, event tags — is **integer math only** (i64/u64/i32). See
  ADR-017 and doc 02.
- Presentation values are floats and are *never* hashed: camera-relative `f32` positions, doc 07's
  SDF gradient collision, animation warps, audio occlusion. Transient `f32` kinematics may cross
  the wire precisely because nothing hashes them (doc 02 float discipline, doc 10). The question is
  never "is this float?" — it is "does a root depend on it?".
- No transcendental function (`sin`, `cos`, `exp`, `pow`, `atan`) — native or `libm` — may feed a
  Merkle root, epoch hash, or any payload a peer must hash to agree with you. Precomputed tables or
  fixed-point routines only. Visual uses of `sin` (doc 07's gait warp, doc 04's noise) are fine.
- Every cross-platform value is covered by the **Bitwise Determinism Suite**
  ([doc 11 § The Bitwise Determinism Suite](docs/11-roadmap.md), gate P1.5). A feature that produces
  a value other machines must see is not done until its row passes. Gate R4 (frame hashes) is
  informational and must not be promoted to a state gate.
- Shaders are renderers. If a shader computes something the simulation must agree with, it is
  a bug (SPEC-02).

## 5. Format & naming

| Purpose | Format | Notes |
|---------|--------|-------|
| Data definitions (entities, biomes, items, circuits, macros) | **RON** (doc 09) | JSON accepted as an alias input; `.kdl`/`.toml` planned |
| Code and comments | English | Matches the transcript and keeps identifiers stable |
| Docs | kebab-case markdown, numeric prefix where ordered | Update `docs/` in the same commit as the behavior it describes |
| Rust types | `UpperCamelCase`, newtypes for `GlobalVoxelPos`, `RegionKey`, `TickId`, `MaterialId` | No bare `i64` triples in APIs once newtypes exist |
| WASM exports | snake_case, via `wit-bindgen` | Never hand-written `#[no_mangle]`, and not only "once contracts freeze": `[workspace.lints.rust] unsafe_code = "forbid"` makes a hand-written `#[no_mangle]` fail the build today, because rustc counts the attribute as unsafe. So no WASM export can exist before A-02 gives `wit-bindgen` an interface to generate from - which is where OQ-41 (and gate P1.5's browser target) actually sits. Verified 2026-10-12 by building a cdylib that never compiled. |
| Merkle hashing | **BLAKE3** (truncated 32-byte digest) | Not SHA-256; not xxHash (doc 03, SPEC-04) |

## 6. No art, ever

No `.obj`, `.fbx`, `.png`, `.jpg`, `.hdr`, rig files, or imported meshes. Geometry is SDF
code, color/material is a function (doc 04). This is the project's core identity, not a
limitation to be eased later.

## 7. Verification before claims

Don't report a milestone as met without running its Definition of Done and pasting real
output: `cargo test`, cross-target **epoch/state root** hashes (gate P1.5 — doc 11's Bitwise
Determinism Suite), `wit-bindgen` checks, benchmark numbers. A headless WebGPU frame hash is worth
pasting as a diagnostic, never as proof of correctness (gate R4 is informational, ADR-017).
"If it compiles" is not evidence. Performance claims in the docs are **hypotheses** until a
spike measures them; say which is which.

## 8. Git hygiene (build in public)

Remote: **https://github.com/UnitBuilds-CC/Merkles-World** (public), with the wiki enabled at
`.../wiki`. Everything here is visible to strangers, so history quality is part of the product.

- Commits are **clean and conventional**: `docs:`, `feat:`, `fix:`, `spike:`, `chore:` prefixes,
  one logical change per commit, no WIP noise, no broken links. The message names the
  `OQ`/`SPEC`/ADR it moves.
- Author every commit as `UnitBuilds <ian@unitbuilds.com>` via per-command flags
  (`git -c user.name=... -c user.email=... commit`) — do not edit global git config.
- Push at the end of each meaningful unit of work; don't accumulate large local batches.
- **Docs ship with the change.** Update the affected doc (and the decision log if a decision
  moved) in the same commit. Mirror substantial doc changes to the wiki.
- Never commit generated artifacts (`target/`, `.cache/`, compiled `.bin`/`.wasm`) unless they
  are intentional distribution assets.
- The design transcript stays **outside** the repository; cite it as `GS`, never commit it.

## 9. Escalate these immediately

Anything that would silently weaken an architectural guarantee: a proposal to store chunk
block arrays, to tick entities on the main thread, to parse config at load, to let the server
hold authoritative voxel state, to special-case one coordinate system for one subsystem, or to
import a mesh asset. These are not style opinions — they are the project.
