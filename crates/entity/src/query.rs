//! Spatial queries over entities: nearest, within radius, frustum cull.

use crate::component::{EntityId, AABB};
use crate::manager::EntityManager;
use render::Vec3;

/// Spatial query interface for entity lookups.
pub trait SpatialQuery {
    /// Find the nearest entity to a point (by AABB center distance).
    /// Returns None if no entities exist.
    fn nearest_to(&self, point: Vec3) -> Option<EntityId>;

    /// Find all entities within a radius of a point.
    fn within_radius(&self, center: Vec3, radius: f32) -> Vec<EntityId>;

    /// Frustum culling: return entities potentially visible from camera.
    /// Simplified to distance-based culling for now.
    fn in_view(&self, camera_pos: Vec3, view_distance: f32) -> Vec<EntityId>;

    /// Find entities whose AABB intersects a given box.
    fn intersecting_box(&self, box_: &AABB) -> Vec<EntityId>;
}

impl SpatialQuery for EntityManager {
    fn nearest_to(&self, point: Vec3) -> Option<EntityId> {
        let mut nearest_id = None;
        let mut nearest_dist_sq = f32::INFINITY;

        for entity in self.iter() {
            let center = entity.compute_world_bounds().center();
            let dx = center[0] - point[0];
            let dy = center[1] - point[1];
            let dz = center[2] - point[2];
            let dist_sq = dx * dx + dy * dy + dz * dz;

            if dist_sq < nearest_dist_sq {
                nearest_dist_sq = dist_sq;
                nearest_id = Some(entity.id);
            }
        }

        nearest_id
    }

    fn within_radius(&self, center: Vec3, radius: f32) -> Vec<EntityId> {
        let radius_sq = radius * radius;

        self.iter()
            .filter(|entity| {
                let entity_center = entity.compute_world_bounds().center();
                let dx = entity_center[0] - center[0];
                let dy = entity_center[1] - center[1];
                let dz = entity_center[2] - center[2];
                dx * dx + dy * dy + dz * dz <= radius_sq
            })
            .map(|entity| entity.id)
            .collect()
    }

    fn in_view(&self, camera_pos: Vec3, view_distance: f32) -> Vec<EntityId> {
        let view_dist_sq = view_distance * view_distance;

        self.iter()
            .filter(|entity| {
                let entity_center = entity.compute_world_bounds().center();
                let dx = entity_center[0] - camera_pos[0];
                let dy = entity_center[1] - camera_pos[1];
                let dz = entity_center[2] - camera_pos[2];
                dx * dx + dy * dy + dz * dz <= view_dist_sq
            })
            .map(|entity| entity.id)
            .collect()
    }

    fn intersecting_box(&self, box_: &AABB) -> Vec<EntityId> {
        self.iter()
            .filter(|entity| entity.compute_world_bounds().intersects(box_))
            .map(|entity| entity.id)
            .collect()
    }
}
