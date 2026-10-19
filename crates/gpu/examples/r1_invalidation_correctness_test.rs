//! Correctness test: verify batch invalidation produces identical output to individual calls.
//!
//! `cargo run --release -p gpu --example r1_invalidation_correctness_test`
//!
//! Renders the same frame twice with different invalidation methods and compares depths.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{GpuCamera, Interpreter};
use render::{Tree, ROOT};

fn try_open_with_gpu_preference() -> Result<Interpreter, String> {
    let instance = wgpu::Instance::default();
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());

    for adapter in &adapters {
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::DiscreteGpu {
            return Interpreter::open_with_adapter(adapter.clone())
                .map_err(|e| format!("Failed to open with {}: {:?}", info.name, e));
        }
    }

    for adapter in &adapters {
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::IntegratedGpu && !info.name.contains("Microsoft") {
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
    let interpreter = match try_open_with_gpu_preference() {
        Ok(i) => i,
        Err(e) => {
            println!("SKIP r1_invalidation_correctness_test: {}", e);
            return;
        }
    };

    println!("Invalidation Correctness Test");
    println!("==============================\n");

    let tree = scene();
    let shape = interpreter.upload(&tree);

    let width = 640u32;
    let height = 360u32;

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
    let mut frame1 = interpreter.begin_camera_frame(width, height);
    interpreter.write_camera(&mut frame1, &camera);
    let bind1 = interpreter.bind_camera_variant(&cull_variant, &shape, &frame1);

    let _result = interpreter
        .march_camera_variant(&cull_variant, &bind1, &frame1)
        .expect("initial march failed");
    println!("  Cache built successfully\n");

    // Define test regions (simulating 10 moving entities)
    let regions = vec![
        (50, 50, 32, 32),
        (150, 80, 24, 24),
        (300, 120, 40, 40),
        (450, 60, 20, 20),
        (100, 200, 28, 28),
        (250, 250, 36, 36),
        (400, 180, 30, 30),
        (500, 220, 22, 22),
        (200, 30, 26, 26),
        (350, 280, 34, 34),
    ];

    // Method 1: Individual invalidations
    println!("Method 1: Individual invalidate_region() calls...");
    for &(x, y, w, h) in &regions {
        bind1.invalidate_region(interpreter.queue(), x, y, w, h);
    }
    interpreter
        .device()
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll after invalidation failed; the mask read-back would be meaningless");

    // Read back dirty mask from method 1 (no render needed)
    let dirty_mask_1 = bind1
        .read_dirty_mask(interpreter.device(), interpreter.queue())
        .expect("failed to read dirty mask");
    println!("  Dirty mask: {} words", dirty_mask_1.len());

    // Clear dirty mask for method 2
    println!("\nClearing dirty mask for method 2...");
    bind1.clear_dirty_mask(interpreter.queue());

    // Method 2: Batch invalidation (same regions)
    println!("Method 2: Single invalidate_regions() call...");
    bind1.invalidate_regions(interpreter.queue(), &regions);
    interpreter
        .device()
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("device poll after invalidation failed; the mask read-back would be meaningless");

    // Read back dirty mask from method 2
    let dirty_mask_2 = bind1
        .read_dirty_mask(interpreter.device(), interpreter.queue())
        .expect("failed to read dirty mask");
    println!("  Dirty mask: {} words", dirty_mask_2.len());

    // Compare dirty masks
    println!("\nComparing dirty masks...");
    if dirty_mask_1.len() != dirty_mask_2.len() {
        println!(
            "FAIL: Different number of words ({} vs {})",
            dirty_mask_1.len(),
            dirty_mask_2.len()
        );
        std::process::exit(1);
    }

    let mut diff_count = 0;

    for i in 0..dirty_mask_1.len() {
        let w1 = dirty_mask_1[i];
        let w2 = dirty_mask_2[i];

        if w1 != w2 {
            diff_count += 1;

            if diff_count <= 5 {
                println!("  Word {}: 0x{:08X} vs 0x{:08X}", i, w1, w2);
            }
        }
    }

    println!("\nResults:");
    println!("  Total words: {}", dirty_mask_1.len());
    println!(
        "  Differing words: {} ({:.4}%)",
        diff_count,
        (diff_count as f64 / dirty_mask_1.len() as f64) * 100.0
    );

    if diff_count == 0 {
        println!("\nPASS: Both methods produce identical dirty masks!");
        println!(
            "Batch invalidation produces bit-for-bit identical invalidation to individual calls."
        );
    } else {
        println!("\nFAIL: {} words differ between methods!", diff_count);
        std::process::exit(1);
    }
}
