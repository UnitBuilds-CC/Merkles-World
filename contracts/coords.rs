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
