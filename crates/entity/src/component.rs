//! Entity components: identity, transform, state, bounds.

use render::Vec3;

/// Stable entity identifier (doc 10).
/// Monotonically increasing u32, wraps after 2³².
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityId(pub u32);

impl EntityId {
    /// Invalid/unassigned entity marker
    pub const NONE: Self = Self(u32::MAX);

    pub fn is_valid(&self) -> bool {
        self.0 != u32::MAX
    }
}

/// World-space position and orientation.
/// Uses f32 for all fields — these are transient presentation values,
/// never hashed into Merkle roots (doc 02 float discipline).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub position: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Transform {
    /// Default transform at origin, no rotation, unit scale
    pub fn identity() -> Self {
        Self {
            position: [0.0, 0.0, 0.0],
            rotation: Quat::identity(),
            scale: [1.0, 1.0, 1.0],
        }
    }

    /// Create a transform at given position with default orientation
    pub fn at(position: Vec3) -> Self {
        Self {
            position,
            rotation: Quat::identity(),
            scale: [1.0, 1.0, 1.0],
        }
    }
}

/// Quaternion for rotation (avoids gimbal lock vs Euler angles, OQ-63).
/// Normalized after every update to prevent drift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quat(pub [f32; 4]); // x, y, z, w

impl Quat {
    /// Identity quaternion (no rotation)
    pub fn identity() -> Self {
        Self([0.0, 0.0, 0.0, 1.0])
    }

    /// Check if quaternion is approximately normalized
    pub fn is_normalized(&self) -> bool {
        let mag_sq = self.0[0].powi(2) + self.0[1].powi(2) + self.0[2].powi(2) + self.0[3].powi(2);
        (mag_sq - 1.0).abs() < 1e-5
    }

    /// Normalize the quaternion in place
    pub fn normalize(&mut self) {
        let mag = (self.0[0].powi(2) + self.0[1].powi(2) + self.0[2].powi(2) + self.0[3].powi(2)).sqrt();
        if mag > 1e-6 {
            self.0[0] /= mag;
            self.0[1] /= mag;
            self.0[2] /= mag;
            self.0[3] /= mag;
        }
    }
}

/// Axis-aligned bounding box in world space.
/// Used for broad-phase culling and dirty mask invalidation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AABB {
    pub min: Vec3,
    pub max: Vec3,
}

impl AABB {
    /// Create an empty AABB (inverted bounds)
    pub fn empty() -> Self {
        Self {
            min: [f32::INFINITY; 3],
            max: [f32::NEG_INFINITY; 3],
        }
    }

    /// Check if point is inside this AABB
    pub fn contains_point(&self, p: Vec3) -> bool {
        p[0] >= self.min[0] && p[0] <= self.max[0]
            && p[1] >= self.min[1] && p[1] <= self.max[1]
            && p[2] >= self.min[2] && p[2] <= self.max[2]
    }

    /// Check if this AABB intersects another
    pub fn intersects(&self, other: &AABB) -> bool {
        self.min[0] <= other.max[0] && self.max[0] >= other.min[0]
            && self.min[1] <= other.max[1] && self.max[1] >= other.min[1]
            && self.min[2] <= other.max[2] && self.max[2] >= other.min[2]
    }

    /// Expand this AABB to include a point
    pub fn expand_to_include(&mut self, p: Vec3) {
        self.min[0] = self.min[0].min(p[0]);
        self.min[1] = self.min[1].min(p[1]);
        self.min[2] = self.min[2].min(p[2]);
        self.max[0] = self.max[0].max(p[0]);
        self.max[1] = self.max[1].max(p[1]);
        self.max[2] = self.max[2].max(p[2]);
    }

    /// Compute center of this AABB
    pub fn center(&self) -> Vec3 {
        [
            (self.min[0] + self.max[0]) * 0.5,
            (self.min[1] + self.max[1]) * 0.5,
            (self.min[2] + self.max[2]) * 0.5,
        ]
    }

    /// Compute size (extent) of this AABB
    pub fn size(&self) -> Vec3 {
        [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ]
    }
}

/// Entity state record — persisted via ledger's 12-bit payload field (OQ-45).
///
/// Layout: `material_id:8 | flags:2 | quantity:2` = 12 bits total
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityState {
    /// Material registry index (0–255)
    pub material_id: u8,
    /// Bitfield: bit 0 = alive/dead, bit 1 = active/inactive
    pub flags: u8,
    /// Stack count (items), health tier (mobs), growth stage (plants): 0–3
    pub quantity: u8,
}

impl EntityState {
    /// Alive and active with no quantity
    pub fn alive(material_id: u8) -> Self {
        Self {
            material_id,
            flags: flag::ALIVE | flag::ACTIVE,
            quantity: 0,
        }
    }

    /// Dead entity (despawn candidate)
    pub fn dead(material_id: u8) -> Self {
        Self {
            material_id,
            flags: 0, // Neither alive nor active
            quantity: 0,
        }
    }

    /// Check if entity is alive
    pub fn is_alive(&self) -> bool {
        self.flags & flag::ALIVE != 0
    }

    /// Check if entity is active (not paused)
    pub fn is_active(&self) -> bool {
        self.flags & flag::ACTIVE != 0
    }

    /// Kill this entity (clear alive flag)
    pub fn kill(&mut self) {
        self.flags &= !flag::ALIVE;
    }

    /// Pause this entity (clear active flag)
    pub fn pause(&mut self) {
        self.flags &= !flag::ACTIVE;
    }

    /// Resume this entity (set active flag)
    pub fn resume(&mut self) {
        self.flags |= flag::ACTIVE;
    }

    /// Encode state into 12-bit payload for ledger records
    pub fn encode_payload(&self) -> u16 {
        let material_bits = (self.material_id as u16) & 0xFF; // 8 bits
        let flag_bits = (self.flags as u16) & 0x03; // 2 bits
        let quantity_bits = (self.quantity as u16) & 0x03; // 2 bits

        (material_bits << 4) | (flag_bits << 2) | quantity_bits
    }

    /// Decode state from 12-bit payload
    pub fn decode_payload(payload: u16) -> Self {
        let material_id = ((payload >> 4) & 0xFF) as u8;
        let flags = ((payload >> 2) & 0x03) as u8;
        let quantity = (payload & 0x03) as u8;

        Self {
            material_id,
            flags,
            quantity,
        }
    }
}

/// Flag constants for EntityState::flags
pub mod flag {
    pub const ALIVE: u8 = 0x01;  // Bit 0: entity is alive
    pub const ACTIVE: u8 = 0x02; // Bit 1: entity is active (not paused)
}

/// Identifies a subtree within the scene tree for selective invalidation.
/// Re-exported from render::temporal_cache for convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityRange {
    /// First node index in the entity's subtree (pre-order)
    pub start: u32,
    /// Number of nodes in the entity's subtree
    pub len: u32,
}

impl EntityRange {
    /// Check if a node index falls within this entity's range
    pub fn contains(&self, node: u32) -> bool {
        node >= self.start && node < self.start + self.len
    }
}
