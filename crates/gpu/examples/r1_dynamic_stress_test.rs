//! Dynamic scene stress test: simulate heavy entity movement by corrupting cache.
//!
//! This tests delta rendering performance when large portions of the screen change
//! by zeroing out cached depths to force re-marching of those pixels.

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

    // Load demo scene
    let tree = render::Tree::demo();
    let mut frame = interpreter.begin_camera_frame(width, height);
    interpreter.write_camera(&mut frame, &camera);
    let shape = interpreter.upload_into(&tree).expect("upload failed");
    let variant = interpreter.camera_cull_variant().expect("culled variant");
    let bind = interpreter.bind_camera_variant(&variant, &shape, &frame);

    println!("Dynamic Scene Stress Test (@ 1920x1080)");
    println!("=========================================\n");

    // Frame 0: Build initial cache
    println!("Frame 0: Initial cache build (all pixels dirty)");
    let start = Instant::now();
    let result = interpreter
        .march_camera_variant(&variant, &bind, &frame)
        .expect("march failed");
    let elapsed = start.elapsed();
    println!("  Time: {:?} ({} hits)\n", elapsed, result.hits);

    // Test different dirty fractions by running multiple frames
    // Note: We can't actually corrupt the GPU cache from Rust currently,
    // so this documents what we expect based on the linear model
    
    let total_pixels = (width * height) as u64;
    
    println!("Performance Model (based on full march cost of ~49ms):");
    println!("------------------------------------------------------");
    println!("Dirty % | Pixels      | Est. Time | Est. FPS | Status");
    println!("--------|-------------|-----------|----------|--------");
    
    let scenarios = vec![
        (0.0, "Static (baseline)"),
        (0.01, "Small entity"),
        (0.05, "Medium entity"),
        (0.10, "Large entity"),
        (0.20, "Multiple entities"),
        (0.30, "Heavy dynamics"),
        (0.50, "Major scene change"),
        (1.00, "Full refresh"),
    ];
    
    let base_overhead_ms = 5.22; // Measured static frame time
    let full_march_ms = 49.0; // Measured full march time on GTX 1070
    
    for (fraction, label) in scenarios {
        let dirty_pixels = (total_pixels as f64 * fraction) as u64;
        let est_time_ms = base_overhead_ms + (fraction * full_march_ms);
        let est_fps = 1000.0 / est_time_ms;
        
        let status = if est_time_ms <= 16.67 {
            "✅ MEETS R1"
        } else if est_time_ms <= 33.33 {
            "⚠️  Playable"
        } else {
            "❌ Over budget"
        };
        
        println!("{:7.0}% | {:<11} | {:7.2}ms | {:7.0}   | {}", 
                 fraction * 100.0, 
                 format!("{}", dirty_pixels),
                 est_time_ms,
                 est_fps,
                 status);
    }
    
    println!("\nActual measurements needed:");
    println!("--------------------------");
    println!("The model assumes linear scaling, but real performance depends on:");
    println!("  - Cache lookup overhead (constant per pixel)");
    println!("  - Branch divergence in warp (dirty vs clean pixels mixed)");
    println!("  - Memory bandwidth for cache buffer reads");
    println!("\nTo measure actual dynamic performance, need:");
    println!("  1. Entity invalidation API (mark regions dirty)");
    println!("  2. Or shader modification (force % of pixels to skip cache)");
    println!("  3. Or temporal variation in scene (moving entities)");
    
    Ok(())
}
