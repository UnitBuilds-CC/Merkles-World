//! Benchmark temporal cache performance with dynamic scenes using entity invalidation API.
//!
//! `cargo run --release -p gpu --example r1_dynamic_benchmark`
//!
//! Tests how the temporal cache performs when entities move, forcing partial cache invalidation.
//! Measures frame times at various dirty pixel percentages to understand the cost of dynamics.
//!
//! Expected behavior:
//! - 0% dirty (static): fastest, all pixels cached
//! - 1-5% dirty (light dynamics): still fast, minimal remarching
//! - 10-20% dirty (moderate dynamics): intermediate performance
//! - 50% dirty (heavy dynamics): approaches unculled baseline

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{GpuCamera, Interpreter};
use render::{Tree, ROOT};
use std::time::Instant;

/// Try to open an interpreter with a real GPU, preferring discrete over integrated
fn try_open_with_gpu_preference() -> Result<Interpreter, String> {
    let instance = wgpu::Instance::default();
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());

    // Find the first DiscreteGpu
    for adapter in &adapters {
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::DiscreteGpu {
            println!(
                "Using adapter: {} [{:?}, {:?}]",
                info.name, info.backend, info.device_type
            );
            return Interpreter::open_with_adapter(adapter.clone())
                .map_err(|e| format!("Failed to open with {}: {:?}", info.name, e));
        }
    }

    // Fall back to IntegratedGpu
    for adapter in &adapters {
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::IntegratedGpu && !info.name.contains("Microsoft") {
            println!(
                "Using integrated GPU: {} [{:?}, {:?}]",
                info.name, info.backend, info.device_type
            );
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
    // List available adapters
    let instance = wgpu::Instance::default();
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());
    println!("Available adapters ({} total):", adapters.len());
    for adapter in &adapters {
        let info = adapter.get_info();
        println!(
            "  - {} [{:?}, {:?}]",
            info.name, info.device_type, info.backend
        );
    }
    println!();

    let interpreter = match try_open_with_gpu_preference() {
        Ok(i) => i,
        Err(e) => {
            println!("SKIP r1_dynamic_benchmark: {}", e);
            return;
        }
    };

    println!("GPU Dynamic Scene Benchmark");
    println!("=============================\n");
    println!("Adapter: {}\n", interpreter.adapter());

    let tree = scene();
    let shape = interpreter.upload(&tree);

    // Use lower resolution to avoid GPU timeout on integrated/mobile GPUs
    let width = 640u32;
    let height = 360u32;
    let total_pixels = (width * height) as f64;
    println!("Resolution: {}x{}\n", width, height);

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

    // Check if this adapter supports temporal cache
    let adapter_name = interpreter.adapter();
    if adapter_name.contains("Microsoft Basic Render Driver") {
        println!("SKIP: temporal cache requires real GPU hardware");
        println!("Note: Microsoft Basic Render Driver doesn't support multiple bind groups.");
        return;
    }

    let cull_variant = interpreter.camera_cull_variant().expect("culled variant");

    // Build cache by running one frame
    println!("Building initial cache (static scene)...");
    let mut frame = interpreter.begin_camera_frame(width, height);
    interpreter.write_camera(&mut frame, &camera);
    let bind = interpreter.bind_camera_variant(&cull_variant, &shape, &frame);

    let start = Instant::now();
    let result = interpreter
        .march_camera_variant(&cull_variant, &bind, &frame)
        .expect("march failed");
    let elapsed = start.elapsed();
    println!("  Initial build: {:?} ({} hits)\n", elapsed, result.hits);

    // Test different dirty pixel percentages
    let test_cases = vec![
        ("Static (0%)", 0.0),
        ("Light dynamics (1%)", 0.01),
        ("Moderate dynamics (5%)", 0.05),
        ("Heavy dynamics (10%)", 0.10),
        ("Very heavy dynamics (20%)", 0.20),
        ("Extreme dynamics (50%)", 0.50),
    ];

    println!("Dynamic scene performance:");
    println!("--------------------------");

    let mut results = Vec::new();

    for (label, dirty_fraction) in &test_cases {
        let dirty_pixels = (*dirty_fraction * total_pixels).round() as u32;

        // Calculate rectangle dimensions to achieve target dirty pixels
        // Use a square region centered on the screen
        let side = if dirty_pixels > 0 {
            (dirty_pixels as f64).sqrt().ceil() as u32
        } else {
            0
        };
        let x = (width - side) / 2;
        let y = (height - side) / 2;

        // Invalidate the region
        if dirty_pixels > 0 && side > 0 {
            println!(
                "  Invalidating {}x{} region at ({}, {}) = {} pixels",
                side, side, x, y, dirty_pixels
            );
            bind.invalidate_region(interpreter.queue(), x, y, side, side);
        }

        // Poll device to ensure invalidation completes
        interpreter
            .device()
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device poll failed; the frame times below would not follow a settled cache");

        // Measure frame time and collect cache stats
        let mut times = Vec::new();
        let mut cache_hits_total = 0u32;
        for i in 0..3 {
            let start = Instant::now();
            match interpreter.march_camera_variant(&cull_variant, &bind, &frame) {
                Ok(_result) => {
                    let elapsed = start.elapsed();
                    times.push(elapsed);

                    // Read cache stats on last iteration
                    if i == 2 {
                        if let Some((cache_hits, _dirty_checked)) =
                            bind.read_cache_stats(interpreter.device(), interpreter.queue())
                        {
                            cache_hits_total = cache_hits;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("  {} ERROR: {}", label, e);
                    break;
                }
            }
        }

        if times.is_empty() {
            println!("  {:30} SKIPPED (GPU error)", label);
            continue;
        }

        times.sort();
        let median = times[times.len() / 2];

        let fps = 1000.0 / median.as_secs_f64();
        let ms = median.as_secs_f64() * 1000.0;
        let hit_rate = if total_pixels > 0.0 {
            (cache_hits_total as f64 / total_pixels) * 100.0
        } else {
            0.0
        };

        println!(
            "  {:30} {:7.2} ms ({:6.1} fps) - {} dirty pixels - {:.1}% cache hits",
            label, ms, fps, dirty_pixels, hit_rate
        );

        results.push((*dirty_fraction, median, cache_hits_total));
    }

    // Show speedup relative to 50% dirty
    if let Some((_, baseline, _)) = results.last() {
        println!("\nSpeedup relative to 50% dirty:");
        println!("------------------------------");
        for (fraction, time, cache_hits) in &results {
            let speedup = baseline.as_secs_f64() / time.as_secs_f64();
            println!(
                "  {:5.0}% dirty: {:.2}x faster ({} cache hits)",
                fraction * 100.0,
                speedup,
                cache_hits
            );
        }
    }

    // Show absolute time savings
    if results.len() >= 2 {
        let (_, static_time, _) = &results[0];
        let (_, heavy_time, _) = results.last().unwrap();
        let savings_ms = (heavy_time.as_secs_f64() - static_time.as_secs_f64()) * 1000.0;
        println!(
            "\nTime saved (static vs 50% dirty): {:.2} ms per frame",
            savings_ms.max(0.0)
        );
    }
}
