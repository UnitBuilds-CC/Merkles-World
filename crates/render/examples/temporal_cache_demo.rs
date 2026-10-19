//! Temporal cache demo: measure work reduction from frame-to-frame surface reuse.
//!
//! This simulates a camera moving through a scene and measures how many pixels can reuse
//! cached surface data vs need full remarching.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_PLANE, NODE_SMOOTH_UNION, NODE_SPHERE,
};
use render::raster::{render_into, Camera, Frame};
use render::temporal_cache::SurfaceCache;
use render::{march, normal, Tree, Vec3, ROOT};

fn build_scene() -> Tree {
    fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
            pack_params_len(len),
        ])
    }

    // Scene: sphere on ground plane
    let mut params = Vec::new();

    // Node 0: SmoothUnion (root)
    let params_first = params.len() as u16;
    params.push(0.1); // smooth_union k parameter
    let mut nodes = vec![record(NODE_SMOOTH_UNION, 2, 3, params_first, 1, true)];

    // Node 1: Sphere
    let params_first = params.len() as u16;
    params.extend_from_slice(&[0.0, 1.0, 0.0]); // center
    params.push(1.0); // radius
    nodes.push(record(NODE_SPHERE, 0, 1, params_first, 4, false));

    // Node 2: Plane (ground)
    let params_first = params.len() as u16;
    params.extend_from_slice(&[0.0, 1.0, 0.0]); // normal
    params.push(0.0); // offset
    nodes.push(record(NODE_PLANE, 0, 1, params_first, 4, false));

    Tree { nodes, params }
}

fn march_surface(tree: &Tree, origin: Vec3, dir: Vec3) -> Option<(f32, Vec3, Vec3)> {
    let hit = march(tree, ROOT, origin, dir, 40.0, 1e-3, 64)?;
    let n = normal(tree, ROOT, hit.point, 1e-3 * 16.0);
    Some((hit.t, hit.point, n))
}

fn main() {
    println!("Temporal cache demo: measuring frame-to-frame surface reuse");
    println!("=============================================================\n");

    let tree = build_scene();

    let width = 320;
    let height = 240;

    // Initial camera
    let mut camera = Camera {
        origin: [0.0, 1.5, -6.0],
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

    let mut cache = SurfaceCache::new(width, height);
    let mut frame = Frame::new(width, height);

    println!("Simulating 10 frames with camera movement...\n");

    // Simulate 10 frames with small camera movements
    for frame_num in 0..10 {
        // Move camera slightly each frame (walking forward)
        camera.origin[2] += 0.1; // Move forward 0.1 units per frame

        // Render with temporal cache
        let (hits, cache_hits) = cache.render_with_cache(&tree, &camera, march_surface);

        // Also do a full render for comparison
        render_into(&tree, &camera, &mut frame);
        let full_hits = frame.hit_count() as usize;

        let total_pixels = (width * height) as usize;
        let hit_rate = (hits as f64 / total_pixels as f64) * 100.0;
        let cache_hit_rate = if hits > 0 {
            (cache_hits as f64 / hits as f64) * 100.0
        } else {
            0.0
        };
        let work_saved = if hits > 0 {
            ((hits - cache_hits) as f64 / hits as f64) * 100.0
        } else {
            0.0
        };

        println!(
            "Frame {:2}: {:5} hits ({:.1}%), {:5} cache hits ({:.1}% of hits), {:5} remarched ({:.1}% saved) | full render {:5} hits{}",
            frame_num,
            hits,
            hit_rate,
            cache_hits,
            cache_hit_rate,
            hits - cache_hits,
            work_saved,
            full_hits,
            // Both paths see the same camera this frame, so a disagreement is a cached surface
            // the validator accepted while a full march would not have hit it. Agreement is only
            // on the aggregate count — offsetting per-pixel errors would still sum to the same
            // number, so this flags a suspected reuse bug, it does not clear the cache.
            if hits == full_hits {
                ""
            } else {
                " <- DISAGREES"
            }
        );
    }

    let stats = cache.stats();
    println!("\nFinal cache state:");
    println!("  Total pixels: {}", stats.total_pixels);
    println!("  Cached surfaces: {}", stats.cached_surfaces);
    println!("  Sky pixels: {}", stats.sky_pixels);
    println!("  Aged pixels (>10 frames old): {}", stats.aged_pixels);

    println!("\nInterpretation:");
    println!("  - Cache hit rate shows what % of surface pixels reused cached data");
    println!("  - Work saved = % of rays we didn't have to march thanks to caching");
    println!("  - Higher is better: 80%+ means temporal coherence is strong");
}
