//! Entity framework for Phase 2 content (doc 16).
//!
//! Provides the runtime lifecycle for game entities: spawn, update, despawn.
//! Each entity has a shape tree (SDF), a transform (position/orientation),
//! and a state record (material_id, flags, quantity) persisted via the ledger.

pub mod component;
pub mod manager;
pub mod material;
pub mod query;

pub use component::{EntityId, EntityState, Transform, AABB};
pub use manager::EntityManager;
pub use material::{Material, MaterialRegistry, NoiseKind};
pub use query::SpatialQuery;

pub use render::temporal_cache::EntityRange;
/// Re-export render types we depend on
pub use render::{Tree, Vec3};
