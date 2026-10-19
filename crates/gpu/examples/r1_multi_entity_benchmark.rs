//! Benchmark comparing single vs batch entity invalidation.
//!
//! `cargo run --release -p gpu --example r1_multi_entity_benchmark`
//!
//! Tests the performance difference between calling invalidate_region() N times
//! versus invalidate_regions() once with N regions, simulating a scene with
//! many moving entities.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{GpuCamera, Interpreter};
use render::{Tree, ROOT};
use std::time::Instant;

fn try_open_with_gpu_preference() -> Result<Interpreter, String> {
    let instance = wgpu::Instance::default();
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());
    
    for adapter in &adapters {
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::DiscreteGpu {
            println!("Using adapter: {} [{:?}, {:?}]", info.name, info.backend, info.device_type);
            return Interpreter::open_with_adapter(adapter.clone())
                .map_err(|e| format!("Failed to open with {}: {:?}", info.name, e));
        }
    }
    
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
    let instance = wgpu::Instance::default();
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());
    println!("Available adapters ({} total):\n", adapters.len());
    
    let interpreter = match try_open_with_gpu_preference() {
        Ok(i) => i,
        Err(e) => {
            println!("SKIP r1_multi_entity_benchmark: {}", e);
            return;
        }
    };

    println!("Multi-Entity Invalidation Benchmark");
    println!("====================================\n");
    println!("Adapter: {}\n", interpreter.adapter());

    let tree = scene();
    let shape = interpreter.upload(&tree);

    let width = 640u32;
    let height = 360u32;
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

    let adapter_name = interpreter.adapter();
    if adapter_name.contains("Microsoft Basic Render Driver") {
        println!("SKIP: temporal cache requires real GPU hardware");
        return;
    }
    
    let cull_variant = interpreter.camera_cull_variant().expect("culled variant");

    // Build initial cache
    println!("Building initial cache...");
    let mut frame = interpreter.begin_camera_frame(width, height);
    interpreter.write_camera(&mut frame, &camera);
    let bind = interpreter.bind_camera_variant(&cull_variant, &shape, &frame);
    
    let start = Instant::now();
    let _result = interpreter
        .march_camera_variant(&cull_variant, &bind, &frame)
        .expect("march failed");
    let elapsed = start.elapsed();
    println!("  Initial build: {:.2} ms\n", elapsed.as_secs_f64() * 1000.0);

    // Test different entity counts
    let entity_counts = vec![1, 5, 10, 20, 50, 100];
    
    println!("Comparing single vs batch invalidation:");
    println!("----------------------------------------");
    
    for &entity_count in &entity_counts {
        // Generate random entity positions (small 16x16 pixel entities)
        let mut regions = Vec::new();
        let entity_size = 16u32;
        
        for i in 0..entity_count {
            // Spread entities across the screen
            let cols = (entity_count as f32).sqrt().ceil() as u32;
            let col = i % cols;
            let row = i / cols;
            
            let x = (col * (width / cols)) / 2 + width / (cols * 4);
            let y = (row * (height / cols)) / 2 + height / (cols * 4);
            
            regions.push((x, y, entity_size, entity_size));
        }
        
        // Test individual invalidations
        let mut single_times = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            for &(x, y, w, h) in &regions {
                bind.invalidate_region(interpreter.queue(), x, y, w, h);
            }
            interpreter.device().poll(wgpu::PollType::wait_indefinitely());
            let elapsed = start.elapsed();
            single_times.push(elapsed);
        }
        single_times.sort();
        let single_median = single_times[single_times.len() / 2];
        
        // Test batch invalidation
        let mut batch_times = Vec::new();
        for _ in 0..3 {
            let start = Instant::now();
            bind.invalidate_regions(interpreter.queue(), &regions);
            interpreter.device().poll(wgpu::PollType::wait_indefinitely());
            let elapsed = start.elapsed();
            batch_times.push(elapsed);
        }
        batch_times.sort();
        let batch_median = batch_times[batch_times.len() / 2];
        
        let speedup = single_median.as_secs_f64() / batch_median.as_secs_f64();
        let single_ms = single_median.as_secs_f64() * 1000.0;
        let batch_ms = batch_median.as_secs_f64() * 1000.0;
        
        println!("  {:3} entities: single={:7.3} ms, batch={:7.3} ms ({:.2}x speedup)", 
                 entity_count, single_ms, batch_ms, speedup);
    }
    
    println!("\nNote: Times include GPU polling overhead.");
    println!("Batch API eliminates N-1 GPU transfers, saving bus bandwidth.");
}
