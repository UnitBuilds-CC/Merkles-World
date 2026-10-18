//! Sky cache demo: show temporal caching working on a bounded scene where sky pixels can be validated.
//!
//! This uses two spheres instead of sphere+plane, so the scene enclosure is bounded and sky caching works.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SMOOTH_UNION, NODE_SPHERE,
};
use render::raster::{render_into, render_temporal, Camera, Frame};
use render::temporal_cache::SurfaceCache;
use render::Tree;
use std::time::Instant;

fn build_bounded_scene() -> Tree {
    fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
            pack_params_len(len),
        ])
    }

    // Bounded scene: two spheres (no plane)
    let mut params = Vec::new();

    // Node 0: SmoothUnion (root)
    let params_first = params.len() as u16;
    params.push(0.1);
    let mut nodes = vec![record(NODE_SMOOTH_UNION, 2, 3, params_first, 1, true)];

    // Node 1: Sphere at origin
    let params_first = params.len() as u16;
    params.extend_from_slice(&[0.0, 0.0, 0.0]);
    params.push(1.0);
    nodes.push(record(NODE_SPHERE, 0, 1, params_first, 4, false));

    // Node 2: Sphere offset
    let params_first = params.len() as u16;
    params.extend_from_slice(&[2.5, 0.0, 0.0]);
    params.push(0.8);
    nodes.push(record(NODE_SPHERE, 0, 1, params_first, 4, false));

    Tree { nodes, params }
}

fn main() {
    println!("Sky cache demo @ 1920x1080");
    println!("===========================\n");

    let tree = build_bounded_scene();

    // Camera setup
    let width = 1920;
    let height = 1080;
    let camera = Camera {
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

    let mut frame = Frame::new(width, height);
    let mut cache = SurfaceCache::new(width, height);

    println!("Baseline: full march (no cache)");
    println!("-------------------------------");

    // Measure baseline
    let start = Instant::now();
    render_into(&tree, &camera, &mut frame);
    let baseline_time = start.elapsed();
    println!("  Time: {:?} ({} hits, {} total pixels)", baseline_time, frame.hit_count(), width * height);

    let hit_rate = frame.hit_count() as f64 / (width * height) as f64 * 100.0;
    let sky_rate = 100.0 - hit_rate;
    println!("  Hit rate: {:.1}% (sky: {:.1}%)\n", hit_rate, sky_rate);

    println!("Temporal cache with sky validation");
    println!("-----------------------------------");

    // Simulate 5 frames with small camera movements
    let mut moving_camera = camera;
    let mut temporal_times = Vec::new();
    let mut total_cache_hits = 0usize;
    let mut total_surface_hits = 0usize;

    for frame_num in 0..5 {
        // Move camera slightly
        moving_camera.origin[2] += 0.05;

        let start = Instant::now();
        let (stats, cache_hits) = render_temporal(&tree, &moving_camera, &mut cache, &mut frame);
        let elapsed = start.elapsed();

        total_surface_hits += stats.hits as usize;
        total_cache_hits += cache_hits;

        let hit_rate = if stats.hits > 0 {
            (cache_hits as f64 / stats.hits as f64) * 100.0
        } else {
            0.0
        };

        println!(
            "  Frame {}: {:?} ({} surface hits, {} cache hits = {:.1}% reuse)",
            frame_num + 1, elapsed, stats.hits, cache_hits, hit_rate
        );

        temporal_times.push(elapsed);
    }

    temporal_times.sort();
    let temporal_median = temporal_times[temporal_times.len() / 2];
    let avg_cache_hit_rate = if total_surface_hits > 0 {
        (total_cache_hits as f64 / total_surface_hits as f64) * 100.0
    } else {
        0.0
    };

    println!("\nSummary");
    println!("=======");
    println!("Baseline (full march):       {:?}", baseline_time);
    println!("Temporal cache median:       {:?}", temporal_median);
    println!("Speedup:                     {:.2}x", baseline_time.as_secs_f64() / temporal_median.as_secs_f64());
    println!("Avg cache hit rate:          {:.1}%", avg_cache_hit_rate);

    let cache_stats = cache.stats();
    println!("\nCache state after 5 frames:");
    println!("  Cached surfaces: {}", cache_stats.cached_surfaces);
    println!("  Sky pixels:      {}", cache_stats.sky_pixels);
    println!("  Total pixels:    {}", cache_stats.total_pixels);

    println!("\nNote: Sky caching requires bounded scenes. With a plane present,");
    println!("the scene enclosure is Unbounded and sky pixels cannot be validated.");
}
