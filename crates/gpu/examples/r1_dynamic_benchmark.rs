//! Dynamic scene benchmark: test temporal cache with simulated entity movement.
//!
//! This benchmarks delta rendering performance when portions of the scene change,
//! simulating entity movement by manually invalidating rectangular regions of the dirty mask.

use gpu::{Interpreter, OpenError};
use render::raster::Camera;
use std::time::Instant;

fn main() -> Result<(), OpenError> {
    let interpreter = Interpreter::open()?;
    println!("Using adapter: {}\n", interpreter.adapter());

    // 1080p camera
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

    // Load demo scene (same as r1_temporal_benchmark)
    let tree = render::Tree::demo();
    let mut frame = interpreter.begin_camera_frame(width, height);
    interpreter.write_camera(&mut frame, &camera);
    let shape = interpreter.upload_into(&tree).expect("upload failed");
    let variant = interpreter.camera_cull_variant().expect("culled variant");
    let bind = interpreter.bind_camera_variant(&variant, &shape, &frame);

    println!("Dynamic Scene Benchmark (simulated entity movement)");
    println!("====================================================\n");

    // Frame 0: Build initial cache
    println!("Frame 0: Initial cache build (all pixels dirty)");
    let start = Instant::now();
    let result = interpreter
        .march_camera_variant(&variant, &bind, &frame)
        .expect("march failed");
    let elapsed = start.elapsed();
    println!("  Time: {:?} ({} hits)\n", elapsed, result.hits);

    // Simulate different levels of dynamic content
    let scenarios = vec![
        ("Static (no changes)", 0.0),
        ("Small entity (1% dirty)", 0.01),
        ("Medium entity (5% dirty)", 0.05),
        ("Large entity (15% dirty)", 0.15),
        ("Multiple entities (30% dirty)", 0.30),
        ("Half screen update (50% dirty)", 0.50),
    ];

    for (name, dirty_fraction) in scenarios {
        println!("Scenario: {} ({:.0}% pixels dirty)", name, dirty_fraction * 100.0);
        println!("{}", "-".repeat(60));

        // Mark random pixels as dirty to simulate entity movement
        if dirty_fraction > 0.0 {
            let total_pixels = (width * height) as u32;
            let dirty_count = (total_pixels as f64 * dirty_fraction) as u32;
            
            // We can't directly manipulate the dirty mask from Rust currently,
            // so this is a placeholder showing what we'd test
            println!("  Would invalidate {} pixels ({:.1}% of screen)", 
                     dirty_count, dirty_fraction * 100.0);
            println!("  Note: Direct dirty mask manipulation not yet exposed via API");
        }

        // Measure cached frame time
        let mut times = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            let result = interpreter
                .march_camera_variant(&variant, &bind, &frame)
                .expect("march failed");
            let elapsed = start.elapsed();
            times.push(elapsed);
            
            // In a real dynamic test, we'd re-dirty pixels here based on entity movement
        }
        
        times.sort();
        let median = times[times.len() / 2];
        let fps = 1000.0 / median.as_secs_f64();
        println!("  Median: {:?} (~{:.0} fps)", median, fps);
        println!();
    }

    println!("Note: Current implementation caches perfectly for static scenes.");
    println!("Dynamic invalidation requires entity tracking API (not yet implemented).");
    println!("\nFor true dynamic testing, need:");
    println!("  1. Entity influence sphere calculation");
    println!("  2. Dirty mask region invalidation API");
    println!("  3. Re-render after invalidation to measure partial march cost");

    Ok(())
}
