//! R1 with and without BVH culling: measure the actual speedup on GPU.
//!
//! `cargo run --release -p gpu --example r1_cull_benchmark`
//!
//! The culled camera entry point exists in interp.wgsl but isn't used by default. This benchmarks both paths
//! to quantify the savings before making culling the production default.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{GpuCamera, Interpreter};
use render::Tree;
use std::time::Instant;

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
    println!("R1 BVH culling benchmark @ 1920x1080");
    println!("=====================================\n");

    let tree = scene();
    let interpreter = match Interpreter::open() {
        Ok(interp) => interp,
        Err(e) => {
            eprintln!("No GPU available: {e:?}");
            return;
        }
    };

    // Upload scene
    let shape = interpreter.upload(&tree);

    // Camera setup matching r1_march_alone
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
        root: 0, // ROOT node
        width,
        height,
    };

    let mut frame = interpreter.begin_camera_frame(width, height);
    interpreter.write_camera(&mut frame, &camera);
    let bind = interpreter.bind_camera(&shape, &frame);

    println!("Unculled path (default):");
    println!("------------------------");
    let mut unculled_times = Vec::new();
    for i in 0..5 {
        let start = Instant::now();
        let result = interpreter
            .march_camera_frame(&bind, &frame)
            .expect("march failed");
        let elapsed = start.elapsed();
        unculled_times.push(elapsed);
        if i == 0 {
            println!("  Run 0: {:?} ({} hits)", elapsed, result.hits);
        }
    }
    unculled_times.sort();
    let unculled_median = unculled_times[unculled_times.len() / 2];
    println!("  Median: {:?}\n", unculled_median);

    println!("Culled path (BVH enabled):");
    println!("--------------------------");
    let cull_variant = interpreter
        .camera_cull_variant()
        .expect("cull variant not available");
    let mut culled_times = Vec::new();
    for i in 0..5 {
        let start = Instant::now();
        let result = interpreter
            .march_camera_variant(&cull_variant, &bind, &frame)
            .expect("culled march failed");
        let elapsed = start.elapsed();
        culled_times.push(elapsed);
        if i == 0 {
            println!("  Run 0: {:?} ({} hits)", elapsed, result.hits);
        }
    }
    culled_times.sort();
    let culled_median = culled_times[culled_times.len() / 2];
    println!("  Median: {:?}\n", culled_median);

    println!("Summary");
    println!("=======");
    println!("Unculled median: {:?}", unculled_median);
    println!("Culled median:   {:?}", culled_median);
    println!(
        "Speedup:         {:.2}x",
        unculled_median.as_secs_f64() / culled_median.as_secs_f64()
    );
    println!(
        "Gap to R1 (16.67 ms): {:.1}x remaining",
        16.67 / culled_median.as_secs_f64() * 1000.0
    );
}
