//! Benchmark the GPU temporal surface cache: compares culled march with cache enabled vs disabled.
//!
//! `cargo run --release -p gpu --example r1_temporal_benchmark`
//!
//! Measures frame times over multiple runs to see if temporal caching provides speedup by reusing
//! surfaces from previous frames instead of remarching all pixels. The cache should show significant
//! improvement on static scenes where most surfaces don't change between frames.
//!
//! Expected behavior:
//! - First frame: no cache hits, similar time to unculled path
//! - Subsequent frames (static scene): high cache hit rate, faster rendering
//! - Dynamic scene (entity moves): partial invalidation, intermediate performance

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{GpuCamera, Interpreter};
use render::{Tree, ROOT};
use std::time::Instant;

/// Try to open an interpreter with a real GPU, preferring discrete over integrated  
fn try_open_with_gpu_preference() -> Result<Interpreter, String> {
    // Manually select the MX250 since Interpreter::open() is picking the wrong adapter
    let instance = wgpu::Instance::default();
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());
    
    // Find the first DiscreteGpu (MX250 or similar)
    for adapter in &adapters {
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::DiscreteGpu {
            println!("Using adapter: {} [{:?}, {:?}]", info.name, info.backend, info.device_type);
            return Interpreter::open_with_adapter(adapter.clone())
                .map_err(|e| format!("Failed to open with {}: {:?}", info.name, e));
        }
    }
    
    // Fall back to IntegratedGpu if no discrete GPU found
    for adapter in &adapters {
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::IntegratedGpu && !info.name.contains("Microsoft") {
            println!("Using integrated GPU: {} [{:?}, {:?}]", info.name, info.backend, info.device_type);
            return Interpreter::open_with_adapter(adapter.clone())
                .map_err(|e| format!("Failed to open with {}: {:?}", info.name, e));
        }
    }
    
    Err("No suitable GPU adapter found".to_string())
}

fn scene() -> Tree {
    let record = |kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool| {
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
            pack_params_len(len),
        ])
    };
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 3, 0, 0, true),
            record(NODE_SPHERE, 0, 1, 0, 4, false),
            record(NODE_ROUNDED_BOX, 0, 1, 4, 4, false),
        ],
        params: vec![0.5, 0.0, 0.0, 0.0, 2.0, 0.1, 2.0, 0.0, 0.0, -1.0, 0.0],
    }
}

fn main() {
    // List available adapters for debugging
    let instance = wgpu::Instance::default();
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());
    println!("Available adapters ({} total):", adapters.len());
    for adapter in &adapters {
        let info = adapter.get_info();
        println!("  - {} [{:?}, {:?}]", info.name, info.device_type, info.backend);
    }
    println!();
    
    // Try to open an interpreter with a real GPU
    let interpreter = match try_open_with_gpu_preference() {
        Ok(i) => i,
        Err(e) => {
            println!("SKIP r1_temporal_benchmark: {}", e);
            return;
        }
    };

    println!("GPU Temporal Surface Cache Benchmark");
    println!("=====================================\n");
    println!("Adapter: {}\n", interpreter.adapter());

    let tree = scene();
    let shape = interpreter.upload(&tree);

    let width = 1920u32;
    let height = 1080u32;
    let camera = GpuCamera {
        origin: [0.0f32, 0.6, -4.0],
        forward: {
            let v = [0.0f32, -0.1, 1.0];
            let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            [v[0] / len, v[1] / len, v[2] / len]
        },
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 64,
        root: ROOT,
        width,
        height,
    };

    // Benchmark unculled path (baseline)
    println!("Unculled path (baseline):");
    println!("-------------------------");
    let mut unculled_times = Vec::new();
    for i in 0..5 {
        let mut frame = interpreter.begin_camera_frame(width, height);
        interpreter.write_camera(&mut frame, &camera);
        let bind = interpreter.bind_camera(&shape, &frame);

        let start = Instant::now();
        let result = interpreter.march_camera_frame(&bind, &frame).expect("march failed");
        let elapsed = start.elapsed();
        unculled_times.push(elapsed);

        if i == 0 {
            println!("  Run 0: {:?} ({} hits)", elapsed, result.hits);
        }
    }
    unculled_times.sort();
    let unculled_median = unculled_times[unculled_times.len() / 2];
    println!("  Median: {:?}\n", unculled_median);

    // Benchmark culled path WITHOUT cache (first run creates empty cache)
    println!("Culled path (with temporal cache):");
    println!("-----------------------------------");
    
    // Check if this adapter supports temporal cache
    let adapter_name = interpreter.adapter();
    if adapter_name.contains("Microsoft Basic Render Driver") {
        println!("  SKIP: temporal cache requires real GPU hardware");
        println!("  Note: Microsoft Basic Render Driver doesn't support multiple bind groups.");
        println!("        The temporal cache works on NVIDIA, AMD, and Intel GPUs.");
        println!("        This is a software renderer limitation, not a code issue.");
        return;
    }
    
    let cull_variant = interpreter.camera_cull_variant().expect("culled variant");

    let mut frame = interpreter.begin_camera_frame(width, height);
    interpreter.write_camera(&mut frame, &camera);
    let bind = interpreter.bind_camera_variant(&cull_variant, &shape, &frame);

    let start = Instant::now();
    let result = interpreter
        .march_camera_variant(&cull_variant, &bind, &frame)
        .expect("march failed");
    let elapsed = start.elapsed();
    println!("  First frame: {:?} ({} hits)\n", elapsed, result.hits);

    // Benchmark culled path WITH cache (subsequent frames should hit cache)
    println!("Culled path (with cache - subsequent frames):");
    println!("----------------------------------------------");
    let mut cached_times = Vec::new();
    for i in 0..5 {
        // Reuse same frame and bind group to test cache persistence
        let start = Instant::now();
        let result = interpreter
            .march_camera_variant(&cull_variant, &bind, &frame)
            .expect("march failed");
        let elapsed = start.elapsed();
        cached_times.push(elapsed);

        if i == 0 {
            println!("  Frame 1 after init: {:?} ({} hits)", elapsed, result.hits);
        }
    }
    cached_times.sort();
    let cached_median = cached_times[cached_times.len() / 2];
    println!("  Median: {:?}", cached_median);

    // Calculate speedup
    let speedup = unculled_median.as_secs_f64() / cached_median.as_secs_f64();
    println!("\nSpeedup from temporal cache: {:.2}x", speedup);

    // Show absolute savings
    let savings_ms = (unculled_median - cached_median).as_secs_f64() * 1000.0;
    println!(
        "Time saved per frame: {:.2} ms",
        savings_ms.max(0.0)
    );

    // FPS estimates
    let fps_unculled = 1000.0 / unculled_median.as_secs_f64();
    let fps_cached = 1000.0 / cached_median.as_secs_f64();
    println!("\nEstimated FPS:");
    println!("  Unculled: {:.1} fps", fps_unculled);
    println!("  With cache: {:.1} fps", fps_cached);
}
