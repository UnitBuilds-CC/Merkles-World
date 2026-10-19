//! Entity lifecycle management: spawn, update, despawn.

use crate::component::{EntityId, EntityState, Transform, AABB};
use render::temporal_cache::EntityRange;
use render::{Tree, Vec3};

/// Runtime entity instance
#[derive(Debug)]
pub struct Entity {
    pub id: EntityId,
    pub shape: Tree,             // SDF tree (compiled from RON)
    pub transform: Transform,    // World-space position/orientation
    pub bounds: AABB,            // World-space bounding box
    pub state: EntityState,      // Material, flags, quantity
    pub tree_range: EntityRange, // Which nodes in scene tree belong to this entity
}

impl Entity {
    /// Compute world-space AABB from local bounds and current transform
    pub fn compute_world_bounds(&self) -> AABB {
        // For now, use the stored bounds directly.
        // TODO: Apply rotation/scale transforms when non-uniform scaling is supported
        let center = self.transform.position;
        let size = self.bounds.size();

        AABB {
            min: [
                center[0] - size[0] * 0.5,
                center[1] - size[1] * 0.5,
                center[2] - size[2] * 0.5,
            ],
            max: [
                center[0] + size[0] * 0.5,
                center[1] + size[1] * 0.5,
                center[2] + size[2] * 0.5,
            ],
        }
    }
}

/// Manages entity lifecycle: spawning, updating, despawning.
///
/// Entities are stored in a flat vector indexed by EntityId.
/// Dead entities have their slots recycled after a grace period.
pub struct EntityManager {
    entities: Vec<Option<Entity>>,
    next_id: u32,
    recycled_ids: Vec<u32>,
    tick_counter: u64,
}

impl EntityManager {
    pub fn new() -> Self {
        Self {
            entities: Vec::new(),
            next_id: 0,
            recycled_ids: Vec::new(),
            tick_counter: 0,
        }
    }

    /// Get current tick counter
    pub fn tick(&self) -> u64 {
        self.tick_counter
    }

    /// Advance tick counter (called once per epoch/tick)
    pub fn advance_tick(&mut self) {
        self.tick_counter += 1;
    }

    /// Spawn a new entity with given shape, initial transform, and state.
    /// Returns the assigned EntityId.
    pub fn spawn(
        &mut self,
        shape: Tree,
        transform: Transform,
        state: EntityState,
        tree_range: EntityRange,
    ) -> EntityId {
        // Assign ID (reuse from pool if available)
        let id = if let Some(recycled) = self.recycled_ids.pop() {
            EntityId(recycled)
        } else {
            let id = self.next_id;
            self.next_id += 1;
            EntityId(id)
        };

        // Compute initial world-space bounds
        let local_bounds = AABB {
            min: shape
                .nodes
                .iter()
                .map(|_| [0.0f32; 3]) // Placeholder: actual bounds computed during asset compilation
                .next()
                .unwrap_or([0.0; 3]),
            max: [0.0; 3],
        };

        let entity = Entity {
            id,
            shape,
            transform,
            bounds: local_bounds,
            state,
            tree_range,
        };

        // Store entity (grow vector if needed)
        let index = id.0 as usize;
        if index >= self.entities.len() {
            self.entities.resize_with(index + 1, || None);
        }
        self.entities[index] = Some(entity);

        id
    }

    /// Despawn an entity (mark as dead, schedule for recycling).
    /// Returns true if entity existed and was despawned.
    pub fn despawn(&mut self, id: EntityId) -> bool {
        let index = id.0 as usize;
        if index < self.entities.len() {
            if let Some(_entity) = self.entities[index].take() {
                // Kill the entity (clear alive flag)
                // In a real implementation, we'd append a delta to the ledger here

                // Schedule ID for recycling after grace period
                // For now, recycle immediately; in production, wait 1000 epochs
                self.recycled_ids.push(id.0);

                return true;
            }
        }
        false
    }

    /// Get mutable reference to entity by ID
    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut Entity> {
        let index = id.0 as usize;
        self.entities.get_mut(index).and_then(|opt| opt.as_mut())
    }

    /// Get immutable reference to entity by ID
    pub fn get(&self, id: EntityId) -> Option<&Entity> {
        let index = id.0 as usize;
        self.entities.get(index).and_then(|opt| opt.as_ref())
    }

    /// Iterate over all live entities
    pub fn iter(&self) -> impl Iterator<Item = &Entity> {
        self.entities.iter().filter_map(|opt| opt.as_ref())
    }

    /// Iterate over all live entities (mutable)
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Entity> {
        self.entities.iter_mut().filter_map(|opt| opt.as_mut())
    }

    /// Count of live entities
    pub fn len(&self) -> usize {
        self.entities.iter().filter(|opt| opt.is_some()).count()
    }

    /// Check if no entities exist
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Update all active entities (call once per tick).
    /// Applies gravity, updates bounds, marks dirty regions.
    ///
    /// Returns list of entity IDs whose bounds changed significantly
    /// (caller should invalidate dirty mask for these).
    pub fn update_all(&mut self, dt: f32) -> Vec<EntityId> {
        let mut dirty_entities = Vec::new();

        for entity in self.iter_mut() {
            if !entity.state.is_alive() || !entity.state.is_active() {
                continue;
            }

            // Store old bounds for comparison
            let old_bounds = entity.compute_world_bounds();

            // Apply physics (gravity, etc.) — simplified for now
            // TODO: Integrate with doc 07's physics system
            entity.transform.position[1] -= 9.81 * dt; // Gravity along -Y

            // Compute new bounds
            let new_bounds = entity.compute_world_bounds();

            // Check if bounds changed significantly (> 0.1 voxel movement)
            let moved_significantly = (new_bounds.center()[0] - old_bounds.center()[0]).abs() > 0.1
                || (new_bounds.center()[1] - old_bounds.center()[1]).abs() > 0.1
                || (new_bounds.center()[2] - old_bounds.center()[2]).abs() > 0.1;

            if moved_significantly {
                dirty_entities.push(entity.id);
            }
        }

        dirty_entities
    }

    /// Find entities within a radius of a point (broad-phase spatial query).
    pub fn within_radius(&self, center: Vec3, radius: f32) -> Vec<EntityId> {
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

    /// Frustum culling: return entities whose AABB intersects the view frustum.
    /// Simplified: just checks distance from camera for now.
    /// TODO: Implement proper frustum-AABB intersection test
    pub fn in_view_frustum(&self, camera_pos: Vec3, view_distance: f32) -> Vec<EntityId> {
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
}

impl Default for EntityManager {
    fn default() -> Self {
        Self::new()
    }
}
