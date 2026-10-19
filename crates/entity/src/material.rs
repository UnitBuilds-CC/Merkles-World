//! Material registry: 256 slots for entity appearance (OQ-45).
//!
//! Each material defines base color, roughness, and optional procedural noise.
//! Materials are referenced by u8 index in EntityState::material_id.

use render::Vec3;

/// Procedural noise type for material variation
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NoiseKind {
    Voronoi,
    Value,
    Perlin,
    None,
}

/// Material definition for rendering
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Material {
    /// Base RGB color (0.0–1.0 per channel)
    pub base_color: Vec3,
    /// Surface roughness (0.0 = mirror, 1.0 = matte)
    pub roughness: f32,
    /// Emissive intensity (0.0 = none, >0.0 = glows)
    pub emissive: f32,
    /// Index of refraction (1.0 = air, 1.33 = water, 1.5 = glass)
    pub index_ior: f32,
    /// Procedural noise overlay
    pub noise: NoiseKind,
    /// Noise frequency (higher = finer detail)
    pub noise_frequency: f32,
}

impl Material {
    /// Default gray material
    pub fn default() -> Self {
        Self {
            base_color: [0.5, 0.5, 0.5],
            roughness: 0.8,
            emissive: 0.0,
            index_ior: 1.0,
            noise: NoiseKind::None,
            noise_frequency: 1.0,
        }
    }

    /// Pink pig skin material
    pub fn pig_skin() -> Self {
        Self {
            base_color: [1.0, 0.7, 0.75],
            roughness: 0.6,
            emissive: 0.0,
            index_ior: 1.4,
            noise: NoiseKind::Voronoi,
            noise_frequency: 5.0,
        }
    }

    /// Wood material (for tools/crafting)
    pub fn wood() -> Self {
        Self {
            base_color: [0.6, 0.4, 0.2],
            roughness: 0.9,
            emissive: 0.0,
            index_ior: 1.5,
            noise: NoiseKind::Value,
            noise_frequency: 3.0,
        }
    }

    /// Stone material
    pub fn stone() -> Self {
        Self {
            base_color: [0.5, 0.5, 0.55],
            roughness: 0.95,
            emissive: 0.0,
            index_ior: 1.6,
            noise: NoiseKind::Perlin,
            noise_frequency: 2.0,
        }
    }
}

/// Registry of 256 materials indexed by u8
pub struct MaterialRegistry {
    materials: [Option<Material>; 256],
    next_slot: u8,
}

impl MaterialRegistry {
    pub fn new() -> Self {
        Self {
            materials: [None; 256],
            next_slot: 0,
        }
    }

    /// Register a material, returning its slot index (0–255).
    /// Returns None if registry is full.
    pub fn register(&mut self, material: Material) -> Option<u8> {
        if self.next_slot == 255 && self.materials[255].is_some() {
            return None; // Registry full
        }

        let slot = self.next_slot;
        self.materials[slot as usize] = Some(material);
        self.next_slot = self.next_slot.saturating_add(1);

        Some(slot)
    }

    /// Get material by slot index
    pub fn get(&self, slot: u8) -> Option<&Material> {
        self.materials[slot as usize].as_ref()
    }

    /// Check if slot is occupied
    pub fn is_occupied(&self, slot: u8) -> bool {
        self.materials[slot as usize].is_some()
    }

    /// Count of registered materials
    pub fn len(&self) -> usize {
        self.materials.iter().filter(|m| m.is_some()).count()
    }

    /// Check if registry is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Initialize with default materials (pig_skin, wood, stone at slots 0, 1, 2)
    pub fn with_defaults() -> Self {
        let mut registry = Self::new();
        registry.register(Material::pig_skin());   // Slot 0
        registry.register(Material::wood());        // Slot 1
        registry.register(Material::stone());       // Slot 2
        registry
    }
}

impl Default for MaterialRegistry {
    fn default() -> Self {
        Self::new()
    }
}
