# 02 — Coordinate system (HDCS)

**Status:** specified; three numeric decisions pending (SPEC-01/03/06) — **none of the rest of
the engine may be implemented until they are resolved and `contracts/coords.rs` is frozen.**
**Implements:** ADR-016.

> *"I think probably the most important part is that we're missing a coordinates system...
> That I feel should probably be the first part of the engine."* — so this file is the first
> artifact of Phase 1, and it is the reference frame every other doc resolves through.

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
| Slab | 4096²·X × 64 Y × **one axis plane**, 3 orientations | `(RegionKey, axis, plane_index)` |
| Region | 4096³ voxels | `RegionKey` (3×i64) |
| Sector | 16 regions | `(Rx>>4, Ry>>4, Rz>>4)` |
| Epoch | whole world per commit | BLAKE3 digest ([ADR-028](decisions.md)) |

**Why slabs are planes, not volumes:** a 4096³ slab would be 2²⁴ voxels — 32 MiB at one byte per
voxel, before any delta layer. The 4096²×64 plane is 2²² voxels (4 MiB), *tileable in all three
orientations* for terrain streaming, and it matches how raymarching and streaming actually move
(through a column over time, and through a plane sideways). Chunks, if used at all, are 64³
sub-tiles of a plane, never a fourth authority.

**Canonical slab order** (SPEC-06): `(region_key, axis, plane_index)` — the plane's index along
its normal axis. This one tuple is the key for slab lookup, the Merkle leaf, the `M_active ↔
M_cold` transport unit, the GPU binding slot, the network region identifier, and the
`global_origin` anchor. The first two elements are derived by division:
`axis, plane_index = f(GlobalVoxelPos, RegionKey)`.

**Delta addressing is bounded, and that's fine.** An epoch can only contain edits inside the
player's interaction radius, so it never needs a 128-bit position. A delta record is:

```
| region-slot 16 bits | intra-slab offset 16 bits | material+flags value 32 bits |  = u64
```

Regions get a temporary per-epoch slot index (0..65535) assigned in first-touch order; the slot
table lives in the epoch header and is rebuilt deterministically at replay. Coordinates that
exceed a plane's range roll into the next plane — **never clamped** (a clamped negative index
silently writes to the wrong region).

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
// contracts/coords.rs — frozen artifact A-01; changes require a migration note + ADR (doc 12)

/// Absolute voxel position in the infinite world. Integer-only: two clients at
/// (50000000, 64, -12000000) must hash to identical bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlobalVoxelPos { pub x: i64, pub y: i64, pub z: i64 }  // +Y up, +Z north, +X east

/// One 4096³ region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegionKey { pub x: i64, pub y: i64, pub z: i64 }

/// Plane slab = 4096²×64, addressable along X (terrain), Y, or Z.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlabKey { pub region: RegionKey, pub axis: Axis, pub plane: u16 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis { X, Y, Z }

/// Position inside a region, 0..4095 per axis. Storage width is u16 (SPEC-01), range is 12 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalOffset { pub x: u16, pub y: u16, pub z: u16 }

impl GlobalVoxelPos {
    pub const REGION_SHIFT: u32 = 12;          // 4096
    pub const REGION_SIZE: i64 = 1 << Self::REGION_SHIFT;

    #[inline]
    pub fn region_key(&self) -> RegionKey {
        RegionKey {
            x: self.x >> Self::REGION_SHIFT,   // arithmetic shift == floor for negatives
            y: self.y >> Self::REGION_SHIFT,
            z: self.z >> Self::REGION_SHIFT,
        }
    }

    #[inline]
    pub fn local_offset(&self) -> LocalOffset {
        let m = Self::REGION_SIZE - 1;
        LocalOffset {
            x: ((self.x & m) as u32) as u16,   // masked, never clamped
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
    #[inline]
    pub const fn sector(&self) -> SectorKey {
        SectorKey { x: self.x >> 4, y: self.y >> 4, z: self.z >> 4 }   // 16³ regions
    }
}

/// Camera anchor: the integer origin of the current local space. Changes when the player
/// crosses a voxel boundary; the GPU camera is always (0,0,0).
#[derive(Debug, Clone, Copy)]
pub struct CameraAnchor { pub global: GlobalVoxelPos, pub frustum_aabb: AabbI64 }
```

`div_euclid`/`rem_euclid` are equally correct and clearer about intent; the shift form above is
what we actually benchmark. Whichever is chosen, **rounding must be floor-based for negatives** —
`trunc` division puts region 0 asymmetrically (−4095..4095) and produces wrong hashes across
zero. Property test: for all `x` in `[−2^34, 2^34]`, `region_key(x)*4096 + local_offset(x) == x`.

## Float discipline (local space)

`f32` gives ~7 significant decimal digits. Camera-relative offsets stay small (the render radius
is ≤ 4096 units), so precision is a non-issue — **as long as the subtraction happens on integers
first**. Converting to f32 and *then* subtracting global positions loses the mantissa and
re-introduces the Far Lands bug at a different address. This is the single most common way to
break this design; `to_local_camera_space` is the only sanctioned path.

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
| Network (10) | region-slot index | no coordinate exceeds the wire; a wire payload is 16 bytes |

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
| C1 | `region*4096 + offset == pos` for all sampled positions across `[−2^34, 2^34]`, including negatives | ADR-016 |
| C2 | Identical hashes for the same `(seed, pos)` on x86-64 / ARM64 / WASM | design's "integer math has no rounding error" |
| C3 | No precision jitter at `x = 10^9` (Far Lands regression test) | ADR-016 |
| C4 | Camera-space conversion error `< 1e-4` units at render-radius edges | "sub-millimeter precision anywhere" |

## Open questions

- [SPEC-01](13-risks-open-questions.md) — `LocalOffset` width: 12-bit range stored in `u16`
  (transcript wrote both). **Resolved: `u16` fields, 12-bit valid range, masked not clamped.**
  Needs sign-off because it changes slab byte sizes.
- [SPEC-03](13-risks-open-questions.md) — delta record bit layout: 3×12 offset + 12 value = 48
  bits of a `u64` cannot carry an `i64` triple. **Resolved: region-slot indirection, 16/16/32.**
- [SPEC-06](13-risks-open-questions.md) — slab identity: plane vs volume vs column. **Resolved:
  4096²×64 plane, 3 orientations, canonical `(region_key, axis, plane_index)`.**
- [OQ-15](13-risks-open-questions.md) — does anything ever need sub-voxel coordinate precision,
  and if so which layer supplies it (camera-space f32 only)?
