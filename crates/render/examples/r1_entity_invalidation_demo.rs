//! Entity invalidation demo: show selective cache invalidation when entities move.
//!
//! This demonstrates the influence-sphere approach: when an entity moves, only pixels within
//! its bounding sphere are invalidated, leaving the rest of the cache intact.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SMOOTH_UNION, NODE_SPHERE,
};
use render::raster::{render_temporal, Camera, Frame};
use render::temporal_cache::{EntityRange, SurfaceCache};
use render::Tree;
use std::time::Instant;

fn build_two_sphere_scene() -> Tree {
    fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
            pack_params_len(len),
        ])
    }

    // Two spheres: one static (background), one dynamic (entity)
    let mut params = Vec::new();

    // Node 0: SmoothUnion (root)
    let params_first = params.len() as u16;
    params.push(0.1);
    let mut nodes = vec![record(NODE_SMOOTH_UNION, 2, 3, params_first, 1, true)];

    // Node 1: Static sphere at origin
    let params_first = params.len() as u16;
    params.extend_from_slice(&[0.0, 0.0, 0.0]);
    params.push(1.0);
    nodes.push(record(NODE_SPHERE, 0, 1, params_first, 4, false));

    // Node 2: Dynamic sphere (the "entity")
    let params_first = params.len() as u16;
    params.extend_from_slice(&[3.0, 0.0, 0.0]);
    params.push(0.8);
    nodes.push(record(NODE_SPHERE, 0, 1, params_first, 4, false));

    Tree { nodes, params }
}

fn main() {
    println!("Entity invalidation demo @ 1920x1080");
    println!("=====================================\n");

    let tree = build_two_sphere_scene();

    // Camera setup
    let width = 1920;
    let height = 1080;
    let camera = Camera {
        origin: [0.0, 1.5, -8.0],
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width,
        height,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 64,
    };

    let mut frame = Frame::new(width, height);
    let mut cache = SurfaceCache::new(width, height);

    println!("Frame 0: Build initial cache (static scene)");
    println!("--------------------------------------------");
    let start = Instant::now();
    let (stats, _) = render_temporal(&tree, &camera, &mut cache, &mut frame);
    let elapsed = start.elapsed();
    println!("  Time: {:?} ({} hits)", elapsed, stats.hits);

    let cache_stats = cache.stats();
    println!(
        "  Cached: {} surfaces, {} sky\n",
        cache_stats.cached_surfaces, cache_stats.sky_pixels
    );

    println!("Frame 1: Small camera movement (no entity move)");
    println!("------------------------------------------------");
    let mut moved_camera = camera;
    moved_camera.origin[2] += 0.1; // Move forward slightly

    let start = Instant::now();
    let (stats, cache_hits) = render_temporal(&tree, &moved_camera, &mut cache, &mut frame);
    let elapsed = start.elapsed();
    println!(
        "  Time: {:?} ({} hits, {} cache hits)",
        elapsed, stats.hits, cache_hits
    );
    println!(
        "  Speedup: {:.2}x vs baseline\n",
        1.97 / elapsed.as_secs_f64()
    );

    println!("Entity moves: Invalidate sphere at [3, 0, 0] with radius 2.0");
    println!("-------------------------------------------------------------");
    // The dynamic sphere is at [3, 0, 0] with radius 0.8, but we use a larger
    // influence sphere to account for motion and occlusion changes
    let invalidated = cache.invalidate_sphere([3.0, 0.0, 0.0], 2.0);
    println!("  Invalidated {} cached pixels", invalidated);

    println!("\nFrame 2: After entity invalidation");
    println!("-----------------------------------");
    // Entity has moved - simulate by changing its position in params
    // For this demo, we just show that invalidation worked
    let start = Instant::now();
    let (stats, cache_hits) = render_temporal(&tree, &moved_camera, &mut cache, &mut frame);
    let elapsed = start.elapsed();
    println!(
        "  Time: {:?} ({} hits, {} cache hits)",
        elapsed, stats.hits, cache_hits
    );

    let final_stats = cache.stats();
    println!("\nFinal cache state:");
    println!("  Surfaces: {}", final_stats.cached_surfaces);
    println!("  Sky:      {}", final_stats.sky_pixels);
    println!("  Total:    {}", final_stats.total_pixels);

    println!("\nNode-range invalidation (alternative approach):");
    println!("-----------------------------------------------");
    // Reset cache
    cache = SurfaceCache::new(width, height);
    render_temporal(&tree, &camera, &mut cache, &mut frame);

    // Invalidate by node range: the dynamic sphere is node 2, which is a leaf (len=1)
    let entity_range = EntityRange { start: 2, len: 1 };
    let invalidated = cache.invalidate_entities(&[entity_range]);
    println!("  Invalidated {} pixels attributed to node 2", invalidated);
    println!("  (Currently all surfaces attributed to ROOT, so this shows 0)");
    println!("\nNote: Fine-grained node attribution requires march-level tracking.");
    println!("Influence spheres work without modifying the evaluator.");
}
