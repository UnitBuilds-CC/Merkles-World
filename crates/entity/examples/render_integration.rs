//! Entity render integration test: demonstrates wiring EntityManager into render loop.
//!
//! Shows the pattern for:
//! 1. Spawn entities with EntityManager
//! 2. Update entity positions each frame
//! 3. Get dirty entity list from update_all()
//! 4. Invalidate dirty regions via GPU's invalidate_entity()
//! 5. March frame with temporal cache respecting invalidation
//!
//! This is a headless simulation - no window, just the integration pattern.
//! Run with: cargo run --package entity --example render_integration

use entity::{EntityManager, Transform, EntityState, SpatialQuery};
use render::Tree;
use contracts::sdf::NodeRecord;

fn make_simple_entity() -> Tree {
    // Single sphere entity for testing
    let nodes = vec![
        NodeRecord([0x00010000, 1, 0x00010000, 4]), // SPHERE, root flag, first_param=0, len=4
    ];
    let params = vec![1.0, 0.0, 0.0, 0.0]; // r=1.0, centre=(0,0,0)
    Tree { nodes, params }
}

fn main() {
    println!("Entity Render Integration Pattern");
    println!("==================================\n");

    // 1. Create entity manager and spawn entities
    let mut entity_mgr = EntityManager::new();

    let shape = make_simple_entity();
    let entity_range = entity::EntityRange {
        start: 0,
        len: shape.nodes.len() as u32,
    };

    // Spawn 3 entities at different positions
    let id1 = entity_mgr.spawn(
        shape.clone(),
        Transform::at([0.0, 0.0, 0.0]),
        EntityState::alive(0),
        entity_range,
    );

    let id2 = entity_mgr.spawn(
        shape.clone(),
        Transform::at([5.0, 0.0, 0.0]),
        EntityState::alive(1),
        entity_range,
    );

    let id3 = entity_mgr.spawn(
        shape.clone(),
        Transform::at([10.0, 0.0, 0.0]),
        EntityState::alive(2),
        entity_range,
    );

    println!("Spawned {} entities", entity_mgr.len());

    // 2. Simulate render loop (no actual GPU calls, just the pattern)
    println!("\nSimulating 10-frame render loop:");
    println!("---------------------------------");

    for frame in 0..10 {
        // Update all entities (applies gravity, tracks movement)
        let dirty_entities = entity_mgr.update_all(1.0 / 60.0);

        // In real code, this is where you'd call GPU invalidation:
        // for dirty_id in &dirty_entities {
        //     if let Some(entity) = entity_mgr.get(*dirty_id) {
        //         let bounds = entity.compute_world_bounds();
        //         gpu_interpreter.invalidate_entity(
        //             queue,
        //             &camera,
        //             bounds.min,
        //             bounds.max,
        //         );
        //     }
        // }

        println!(
            "Frame {:2}: {} entities, {} dirty (need invalidation)",
            frame,
            entity_mgr.len(),
            dirty_entities.len()
        );

        // Advance tick counter
        entity_mgr.advance_tick();
    }

    // 3. Demonstrate spatial queries for culling
    println!("\nSpatial queries:");
    println!("----------------");

    let nearby = entity_mgr.within_radius([0.0, 0.0, 0.0], 6.0);
    println!("Entities within 6 units of origin: {}", nearby.len());

    let view = entity_mgr.in_view([5.0, 0.0, -10.0], 15.0);
    println!("Entities in view frustum: {}", view.len());

    // 4. Demonstrate despawn
    println!("\nDespawning entity {:?}", id2);
    entity_mgr.despawn(id2);
    println!("Remaining entities: {}", entity_mgr.len());

    println!("\nIntegration pattern complete.");
    println!("\nTo wire into actual render loop:");
    println!("1. Call entity_mgr.update_all(dt) before march");
    println!("2. For each dirty entity, call interpreter.invalidate_entity()");
    println!("3. March frame - temporal cache will reuse valid pixels");
}
