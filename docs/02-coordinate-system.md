# 02 — Coordinate system (HDCS)

**Status:** specified. SPEC-01's `u16` answer is verified against the contract snippet below but
still needs sign-off (docs/13 lists it as a proposal);
SPEC-03/SPEC-06 are **proposals blocked by [SPEC-09](13-risks-open-questions.md)** (the delta
record cannot address the slab geometry they assume) — **none of the rest of the engine may be
implemented until SPEC-01/03/06/09 are resolved and `contracts/coords.rs` is frozen.**
**Implements:** ADR-016.

> *"I think probably the most important part is that we're missing a coordinates system...
> That I feel should probably be the first part of the engine."* — GS 1690; so this file is the
> first artifact to be frozen (A-01, Phase 0 in [docs/11](11-roadmap.md)), and it is the reference
> frame every other doc resolves through.

## Why the obvious choices fail

- **32-bit float world coordinates** lose sub-millimeter precision past ≈10,000 units: camera
  jitter, render tearing at distance, drifting physics — the Minecraft "Far Lands" failure mode.
- **64-bit float world coordinates** fix precision but *break determinism*: f64 arithmetic and
  libm transcendentals are not bit-identical across CPUs/compilers, and a coordinate that hashes
  differently on two machines destroys Merkle verification.
- **3×i64 everywhere** is bit-exact and infinite, but 24 bytes per voxel delta and a WASM ABI
  liability (192 bits of position per record).

**HDCS = 64-bit Hierarchical Double-Origin Coordinate System:** integers for *truth*, floats for
*view*, two levels of granularity so neither has to do the other's job.

```
GLOBAL WORLD SPACE (integers, discrete, infinite)
├── RegionKey   : (i64, i64, i64)     1 Region = 4096×4096×4096 voxels
└── LocalOffset : intra-region voxel  0..4095 per axis (12-bit range — storage width is SPEC-01)
        │
        │  P_local = P_global − C_global     (re-anchored every frame)
        ▼
LOCAL CAMERA SPACE (f32, continuous)  → GPU raymarching, physics, audio
```

## Conventions (contractual)

| Item | Value |
|------|-------|
| Up | **+Y** |
| Gravity / falling / fluid flow | **−Y** |
| North / South | **+Z / −Z** |
| East / West | **+X / −X** |
| Handedness | right-handed |
| Voxel unit | `i32`-addressable, unit scale; 1 voxel = 1 world unit |
| Zero anchor | `(0, 0, 0)` = **World Genesis Core** |

There is exactly one axis convention in the project. Any subsystem that wants a different one
converts once at its boundary and never leaks it.

## Regions and addressing

| Level | Extent | Address |
|-------|--------|---------|
| Voxel | 1 unit | `GlobalVoxelPos` (3×i64) |
| Slab | plane tile: 256² voxels × 64 deep = 2²² voxels (4 MiB @ 1 B/voxel), 3 orientations | `(RegionKey, axis, plane_index)` — **geometry pending [SPEC-09](13-risks-open-questions.md)** |
| Region | 4096³ voxels = 2³⁶ voxels | `RegionKey` (3×i64) |
| Sector | 16 regions per axis (16³ = 4096 regions) | `(Rx>>4, Ry>>4, Rz>>4)` |
| Epoch | whole world per commit | BLAKE3 digest ([ADR-028](decisions.md)) |

**Why slabs are not volumes:** a whole region is 4096³ = **2³⁶ voxels — 64 GiB at one byte per
voxel** before any delta layer, so a region-sized allocation is not something the engine can hold.
One 4096² cross-section of it is 2²⁴ voxels (16 MiB); the unit doc 03 actually allocates is a
**256² × 64 tile = 2²² voxels (4 MiB)**, *tileable in all three orientations* for terrain
streaming, and it matches how raymarching and streaming really move (through a column over time,
and through a plane sideways). Chunks, if used at all, are 64³ sub-tiles of a plane, never a
fourth authority.

**Canonical slab order** (SPEC-06): `(region_key, axis, plane_index)` — the plane's index along
its normal axis. This one tuple is the key for slab lookup, the Merkle leaf, the `M_active ↔
M_cold` transport unit, the GPU binding slot, the network region identifier, and the
`global_origin` anchor. The last two elements are derived by division:
`axis, plane_index = f(GlobalVoxelPos, RegionKey)`.

**Delta addressing is bounded — and today it does not close (SPEC-09).** An epoch can only contain
edits inside the player's interaction radius, so a 36-bit intra-region position is not obviously
needed. The record is:

```
| region-slot 16 bits | intra-slab offset 16 bits | material+flags value 32 bits |  = u64
```

Regions get a temporary per-epoch slot index (0..65535) assigned in first-touch order; the slot
table lives in the epoch header and is rebuilt deterministically at replay. But 16 bits addresses
**2¹⁶ = 65,536** positions, while the slab above holds **2²²** voxels and a region holds **2³⁶** —
so `intra-slab offset` cannot be a voxel position in either. Three ways out, none chosen: widen the
record (16 + 36 + 32 = 84 bits → a 96- or 128-bit record), shrink the slab to ≤2¹⁶ voxels, or make
the 16 bits an index into a per-region *sparse edit list* whose entries carry the full position.
This is [SPEC-09](13-risks-open-questions.md), and it gates freezing both A-01 (`coords.rs`) and
A-03 (the record layout). A recommendation is now on file as
[ADR-029](decisions.md): take the slab out of the address and split the record as
`| region-slot 16 | intra-region 36 (3×12) | payload 12 |` — exactly 64 bits, with the 36 being the
three 12-bit local offsets this doc already defines. **ADR-029 is `proposed`, not accepted**, so the
three ways out above stay live and both artifacts carry DRAFT headers until it closes.
Coordinates that exceed a plane's range roll into the next plane —
**never clamped** (a clamped negative index silently writes to the wrong region).

## Octree keys and Morton codes

Region traversal and tree indexing use Morton/Z-order:
`key = morton3(RegionKey)`, or `octant path = (Rx>>k, Ry>>k, Rz>>k)` per tree level.

**Morton codes are computed on the `RegionKey`, never on raw voxel coordinates.** Two reasons:
1. A voxel Morton code over i64 needs ~192 bits and overflows any practical key type.
2. A *global* Morton code has no fixed bit-width, so the interleave order at the top of a tree
   can't be agreed between peers — which would fork the Merkle tree into two different shapes
   while both peers believed they were consistent. Tree depth is bounded by the region-level key
   (≤21 bits for `RegionKey`; voxel-level Morton codes are forbidden).

## The Rust contract

The transcript's version had `rem_euclid(...) as u16` (a 65535-wide type holding a 4095-wide
value) and a `to_voxel_offset()` whose name promised voxel granularity while returning region
locality. Both are fixed here.

```rust
// contracts/coords.rs — artifact A-01, DRAFT: freeze is gated on SPEC-01/03/06/09 (docs/13).
// This block is mirrored by docs/02-coordinate-system.md § The Rust contract; the
// `contract_matches_spec_document` test in crates/coords fails if the two ever diverge.

/// Absolute voxel position in the infinite world, axis order +X east / +Y up / +Z north.
/// Integer-only: two clients at (50000000, 64, -12000000) must hash to identical bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlobalVoxelPos {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

/// One 4096³ region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegionKey {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

/// Plane-tile slab = 256²×64 voxels (4 MiB at 1 byte/voxel), addressable along X (terrain),
/// Y, or Z. Final geometry is pending SPEC-09.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlabKey {
    pub region: RegionKey,
    pub axis: Axis,
    pub plane: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    X,
    Y,
    Z,
}

/// 16³ regions — the granularity named event tags are written at (docs/05).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SectorKey {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

/// Axis-aligned box in global integer space. Deliberately not f32: frustum bounds feed region
/// queries, and a query whose result depends on float rounding is not reproducible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AabbI64 {
    pub min: [i64; 3],
    pub max: [i64; 3],
}

/// Position inside a region, 0..4095 per axis. Storage width is u16 (SPEC-01), range is 12 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalOffset {
    pub x: u16,
    pub y: u16,
    pub z: u16,
}

impl GlobalVoxelPos {
    pub const REGION_SHIFT: u32 = 12; // 4096
    pub const REGION_SIZE: i64 = 1 << Self::REGION_SHIFT;

    /// Floor division by the region size, so region 0 is [0, 4096) and never straddles zero.
    #[inline]
    pub fn region_key(&self) -> RegionKey {
        RegionKey {
            x: self.x >> Self::REGION_SHIFT, // arithmetic shift == floor for negatives
            y: self.y >> Self::REGION_SHIFT,
            z: self.z >> Self::REGION_SHIFT,
        }
    }

    /// The 12-bit remainder of `region_key`. Masked, never clamped — that is what makes the
    /// C1 round trip hold for negative positions.
    #[inline]
    pub fn local_offset(&self) -> LocalOffset {
        let m = Self::REGION_SIZE - 1;
        LocalOffset {
            x: ((self.x & m) as u32) as u16,
            y: ((self.y & m) as u32) as u16,
            z: ((self.z & m) as u32) as u16,
        }
    }

    /// Re-anchor into camera-relative f32 space (the only space the GPU sees).
    #[inline]
    pub fn to_local_camera_space(&self, camera: &GlobalVoxelPos) -> [f32; 3] {
        [
            (self.x.wrapping_sub(camera.x)) as f32,
            (self.y.wrapping_sub(camera.y)) as f32,
            (self.z.wrapping_sub(camera.z)) as f32,
        ]
    }
}

impl RegionKey {
    /// The 16³ region block this region belongs to.
    #[inline]
    pub const fn sector(&self) -> SectorKey {
        SectorKey {
            x: self.x >> 4,
            y: self.y >> 4,
            z: self.z >> 4,
        }
    }
}

/// Camera anchor: the integer origin of the current local space. Changes when the player
/// crosses a voxel boundary; the GPU camera is always (0,0,0).
#[derive(Debug, Clone, Copy)]
pub struct CameraAnchor {
    pub global: GlobalVoxelPos,
    pub frustum_aabb: AabbI64,
}

```

`div_euclid`/`rem_euclid` are equally correct and clearer about intent; the shift form above is
what we actually benchmark. Whichever is chosen, **rounding must be floor-based for negatives** —
`trunc` division puts region 0 asymmetrically (−4095..4095) and produces wrong hashes across
zero. Property test: for all `x` in `[−2^34, 2^34]`, `region_key(x)*4096 + local_offset(x) == x`.

## Float discipline (local space)

`f32` gives ~7 significant decimal digits. Camera-relative offsets stay small (the render radius
is ≤ 4096 units), so precision is a non-issue — **as long as the subtraction happens on integers
first**. At a 4096-unit offset the f32 spacing is 2⁻¹¹ ≈ 4.9·10⁻⁴ world units, i.e. about half a
millimetre if a voxel is a metre; double the radius and the spacing doubles with it. Converting to
f32 and *then* subtracting global positions loses the mantissa and re-introduces the Far Lands bug
at a different address. This is the single most common way to break this design;
`to_local_camera_space` is the only sanctioned path.

Entity kinematics on the network are camera-relative `f32` — they are transient presentation
data, never hashed. Anything that must be canonical uses integers.

## Integration matrix

| Subsystem | What it keys off | Rule |
|-----------|------------------|------|
| Slab allocator (03) | `RegionKey` / `SlabKey` | `O(1)` bit-shift lookup; no string keys |
| Merkle ledger (03, 10) | octree path from `RegionKey` | region-local roots, never a global world tree |
| Renderer (04) | `CameraAnchor` uniform | camera sits at origin; `M_active` region origin bound as `global_origin` |
| Macro sim (05) | `SectorKey`, region roots | fields are per-region scalars/vectors, not per-voxel |
| CA rules (05) | relative neighbor offsets | wrapped at region boundary so a border cell sees its neighbor's border cell |
| Circuits (06) | relative `(Δx, Δy, Δz)` edges | **relative offsets, never absolute**, so a build can be pasted/queried by region; node coordinates are region-local |
| Physics (07) | `P_local` + gravity `−Y` | gradients evaluated in camera space |
| Audio (07) | source/ear `P_local` | SDF traced through the same local space |
| Entities / mobs | 3×`i64` global + `f32` velocity | interpolated against `CameraAnchor` each frame |
| Network (10) | region-slot index | no global coordinate crosses the wire; the delta record is 8 bytes (`doc 03`), the `ActionTx` envelope that carries it is larger (`doc 10`) |

## Accepted costs

- **Region-boundary asymmetry.** Region 0 spans 0..4095 while regions ±1 are −4096..−1 /
  4096..8191. Everything stays correct; only the *shape* of the origin region differs, and a
  symmetric origin (−2048..2047) was rejected because it would make region indexing asymmetric
  instead.
- **3×i64 Merkle leaves** (24 bytes per region key) were accepted over 2×i64 packing — region
  keys are sparse, so a hash-map key is cheap and correctness beats 8 saved bytes.
- **Y-axis range.** Regions are symmetric in Y: the world is 4096³ voxels deep *including* below
  the surface, with an explicit bedrock floor and no world-gen cave system below it. A
  4096-voxel-deep cave system is a content decision we're not making yet; the coordinate system
  supports both, so this costs nothing to revisit.

## Validation gates

| Gate | Test | Source |
|------|------|--------|
| C1 | `region*4096 + offset == pos` for all sampled positions across `[−2^34, 2^34]`, including negatives — **run 2026-10-11 ✅ `cargo test -p coords --test gates`: `c1_region_local_round_trip` (25,196,588 sampled axis values) and `c1_region_index_is_floor_not_trunc`, both pass** | ADR-016 |
| C2 | Identical hashes for the same `(seed, pos)` on x86-64 / ARM64 / WASM — **run 2026-10-11 ✅ `c2_test_vectors_are_frozen` pins six `(seed, pos) → u64` vectors produced by `crates/coords/src/lattice.rs`; a target that computes different bytes fails the test rather than disagreeing quietly. Regenerate with `cargo run --release --example gate_vectors`.** **Measured cross-architecture, not assumed:** CI run `38124937828` executed the gate on x86-64 Linux, x86-64 Windows and ARM64 macOS and all three printed the same six digests (`0xa4659597a9d8aeca`, `0x0efd8daf4bed9faa`, `0xff0da13f3d3a3f07`, `0x33c90d2fda549bc5`, `0x0170bd6a62398861`, `0x8e6da94e370cbc70`) — two ISA families agreeing byte for byte. **WASM execution is not gated yet**: CI compiles the crate for `wasm32-unknown-unknown` but runs nothing there, which is [OQ-41](13-risks-open-questions.md)'s CI-runner half; until a WASM runner is wired in, C2 covers x86-64 + ARM64 execution and WASM compilation only. | design's "integer math has no rounding error" |
| C3 | No precision jitter at `x = 10^9` (Far Lands regression test) — **run 2026-10-11 ✅ `c3_no_jitter_at_1e9` passes** | ADR-016 |
| C4 | Camera-space error ≤ **1 f32 ulp** at the tested radius: 4.9·10⁻⁴ units at radius 4096, 2.4·10⁻⁴ at 2048. The "< 1e-4" figure first written here is unreachable at 4096 — it presumes a radius under ~840 units. — **run 2026-10-11 ✅ `cargo run --release --example camera_f32_demo`, camera anchored at `(10⁹, 10⁹, 10⁹)`: max `\|f32 − exact\|` is `0` at radii 512, 2048 and 4096 (integer deltas under 2²⁴ are exactly representable, so the correct path is error-free, not merely inside budget). The counterfactual is measured in the same run: casting the globals before subtracting returns `0.0` for a one-voxel step at `10⁹`, and 4032 of 4096 consecutive global positions collapse to the same `f32`. `c4_camera_error_within_one_ulp` asserts the bound; `c4_casting_globals_to_f32_first_is_the_far_lands_bug` asserts the failure. The example is a CI step, and CI run `38124937828` printed `max \|f32 − exact\| = 0e0` at 512 / 2048 / 4096 on all three gate runners** | "sub-millimeter precision anywhere", at 1 voxel ≈ 1 m |

## Open questions

- [SPEC-01](13-risks-open-questions.md) — `LocalOffset` width: 12-bit range stored in `u16`
  (transcript wrote both). **Proposal, verified: `u16` fields, 12-bit valid range, masked not
  clamped** — docs/13 still lists it as a proposal, not a resolution.
  Needs sign-off because it changes slab byte sizes. Verified in code by
  `spec01_local_offset_masks_instead_of_clamping`: `-1` masks to `4095`, and the C1 round-trip holds
  across `[−2^34, 2^34]`.
- [SPEC-03](13-risks-open-questions.md) — delta record bit layout: 3×12 offset + 12 value = 48
  bits of a `u64` cannot carry an `i64` triple. **Proposal: region-slot indirection. The
  original 16/16/32 split is superseded by [ADR-029](decisions.md)'s proposed 16/36/12 — not
  accepted, see SPEC-09 below.**
- [SPEC-06](13-risks-open-questions.md) — slab identity: plane vs volume vs column. **Proposal:
  256²×64 plane tile, 3 orientations, canonical `(region_key, axis, plane_index)` — not closed.
  Under ADR-029 it stops being an addressing constraint and becomes an allocation/streaming
  choice; still undecided.**
- **SPEC-09** — the proposal above does not fit: 16 intra-slab bits address 2¹⁶ positions, the
  slab holds 2²² voxels and a region holds 2³⁶. Widen the record, shrink the slab, or make the
  16 bits an index into a per-region sparse edit list — or take the slab out of the address, which
  is what [ADR-029](decisions.md) proposes. **Still open, still blocks A-01 and A-03**; A-01 is
  drafted in the tree (`contracts/coords.rs`) under a DRAFT header for exactly this reason. Full
  row in [docs/13](13-risks-open-questions.md).
- [OQ-15](13-risks-open-questions.md) — does anything ever need sub-voxel coordinate precision,
  and if so which layer supplies it (camera-space f32 only)?
