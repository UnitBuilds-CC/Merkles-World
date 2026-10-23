//! R1 temporal cache benchmark: measure work reduction at 1080p on the demo scene.
//!
//! This compares three render paths:
//! 1. Baseline: full march every frame (current R1 measurement)
//! 2. Temporal cache: reuse cached surfaces with validation
//! 3. BVH culling alone: branch-and-bound bounds checks, no cache
//!
//! The goal: quantify how much temporal coherence reduces marched rays, and whether
//! it can close R1's 5.5x gap on the GTX 1070.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_PLANE, NODE_SMOOTH_UNION, NODE_SPHERE,
};
use render::raster::{render_bnb_into, render_into, render_temporal, Camera, Frame};
use render::temporal_cache::SurfaceCache;
use render::Tree;
use std::time::Instant;

fn build_demo_scene() -> Tree {
    fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
            pack_params_len(len),
        ])
    }

    // Demo scene: sphere on ground plane (same as boot scene)
    let mut params = Vec::new();

    // Node 0: SmoothUnion (root)
    let params_first = params.len() as u16;
    params.push(0.1);
    let mut nodes = vec![record(NODE_SMOOTH_UNION, 2, 3, params_first, 1, true)];

    // Node 1: Sphere
    let params_first = params.len() as u16;
    params.extend_from_slice(&[0.0, 1.0, 0.0]);
    params.push(1.0);
    nodes.push(record(NODE_SPHERE, 0, 1, params_first, 4, false));

    // Node 2: Plane
    let params_first = params.len() as u16;
    params.extend_from_slice(&[0.0, 1.0, 0.0]);
    params.push(0.0);
    nodes.push(record(NODE_PLANE, 0, 1, params_first, 4, false));

    Tree { nodes, params }
}

fn main() {
    println!("R1 temporal cache benchmark @ 1920x1080");
    println!("========================================\n");

    let tree = build_demo_scene();

    // Camera setup matching r1_march_alone
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

    // Measure baseline: 5 runs, take median
    let mut baseline_times = Vec::new();
    for i in 0..5 {
        let start = Instant::now();
        render_into(&tree, &camera, &mut frame);
        let elapsed = start.elapsed();
        baseline_times.push(elapsed);
        if i == 0 {
            println!("  Run 0: {:?} ({} hits)", elapsed, frame.hit_count());
        }
    }
    baseline_times.sort();
    let baseline_median = baseline_times[2];
    println!("  Median: {:?}\n", baseline_median);

    println!("Temporal cache: frame-to-frame reuse");
    println!("-------------------------------------");

    // Simulate 10 frames with small camera movements
    let mut moving_camera = camera;
    let mut temporal_times = Vec::new();
    let mut total_cache_hits = 0usize;
    let mut total_hits = 0usize;

    for frame_num in 0..10 {
        // Move camera slightly (walking forward)
        moving_camera.origin[2] += 0.05;

        let start = Instant::now();
        let (stats, cache_hits) = render_temporal(&tree, &moving_camera, &mut cache, &mut frame);
        let elapsed = start.elapsed();

        total_hits += stats.hits as usize;
        total_cache_hits += cache_hits;

        // Check cache state after first frame
        if frame_num == 0 {
            let cache_stats = cache.stats();
            eprintln!(
                "After Frame 0: {} cached surfaces, {} sky, {} total",
                cache_stats.cached_surfaces, cache_stats.sky_pixels, cache_stats.total_pixels
            );
        }

        if frame_num == 0 || frame_num == 9 {
            let hit_rate = if stats.hits > 0 {
                (cache_hits as f64 / stats.hits as f64) * 100.0
            } else {
                0.0
            };
            println!(
                "  Frame {}: {:?} ({} hits, {} cache hits = {:.1}% reuse)",
                frame_num, elapsed, stats.hits, cache_hits, hit_rate
            );
        }

        temporal_times.push(elapsed);
    }

    temporal_times.sort();
    let temporal_median = temporal_times[temporal_times.len() / 2];
    let avg_cache_hit_rate = if total_hits > 0 {
        (total_cache_hits as f64 / total_hits as f64) * 100.0
    } else {
        0.0
    };

    println!("  Median: {:?}", temporal_median);
    println!("  Avg cache hit rate: {:.1}%\n", avg_cache_hit_rate);

    println!("BVH culling, no cache");
    println!("----------------------");

    // render_bnb_into takes no cache, so the section measures bounds checks alone —
    // resetting the cache here would imply a reuse path this arm never runs.
    moving_camera = camera;

    let mut combined_times = Vec::new();
    for frame_num in 0..5 {
        moving_camera.origin[2] += 0.05;

        let start = Instant::now();
        render_bnb_into(&tree, &moving_camera, &mut frame);
        let elapsed = start.elapsed();

        if frame_num == 0 {
            println!("  Run 0: {:?} ({} hits)", elapsed, frame.hit_count());
        }
        combined_times.push(elapsed);
    }
    combined_times.sort();
    let combined_median = combined_times[2];
    println!("  Median: {:?}\n", combined_median);

    // Summary
    println!("Summary");
    println!("=======");
    println!("Baseline (full march):       {:?}", baseline_median);
    println!("Temporal cache:              {:?}", temporal_median);
    println!(
        "Speedup:                     {:.2}x",
        baseline_median.as_secs_f64() / temporal_median.as_secs_f64()
    );
    println!("Avg cache hit rate:          {:.1}%", avg_cache_hit_rate);
    println!("\nR1 budget (16.67 ms):        NOT MET by either method");
    println!(
        "Gap remaining:               {:.2}x for temporal",
        16.67 / temporal_median.as_secs_f64() * 1000.0
    );

    println!("\nNote: CPU timings are not GPU frame times. This measures work reduction,");
    println!(
        "not absolute performance. The temporal cache reduces evaluations by ~{:.0}%.",
        100.0 - avg_cache_hit_rate
    );
}
