//! Coarse-to-fine marching potential analysis: measures how many rays we could skip with hierarchical marching.
//!
//! `cargo run --release -p gpu --example r1_coarse_to_fine`
//!
//! This example does NOT implement the full two-pass approach yet. Instead, it analyzes the current frame
//! to determine what fraction of pixels are near surface hits, which tells us the maximum possible speedup
//! from coarse-to-fine marching.
//!
//! The analysis:
//! 1. March full grid normally
//! 2. Identify which pixels hit surface
//! 3. For each fine pixel, check if any neighbor in a COARSE_FACTOR×COARSE_FACTOR block hit
//! 4. Report what fraction of pixels we'd need to march vs skip

use render::{march, Ray, Tree, ROOT};

const PRODUCT_HALF_HEIGHT: f32 = 0.5;
const PRODUCT_MAX_T: f32 = 40.0;
const PRODUCT_SURFACE: f32 = 1.0e-3;
const PRODUCT_STEPS: u32 = 96;
const R1_W: u32 = 1920;
const R1_H: u32 = 1080;
const COARSE_FACTOR: u32 = 4; // Analyze at 1/4 resolution blocks
const MISS: f32 = 1.0e37;

fn product_origin() -> [f32; 3] {
    [0.0f32, 0.6, -4.0]
}

fn product_forward() -> [f32; 3] {
    let v = [0.0f32, -0.1, 1.0];
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len == 0.0 {
        [0.0, 0.0, 0.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

fn ray_at(px: u32, py: u32, width: u32, height: u32) -> Ray {
    let aspect = width as f32 / height as f32;
    let u = (px as f32 + 0.5) / width as f32;
    let v = (py as f32 + 0.5) / height as f32;
    let ndc_x = (2.0 * u - 1.0) * PRODUCT_HALF_HEIGHT * aspect;
    let ndc_y = (1.0 - 2.0 * v) * PRODUCT_HALF_HEIGHT;
    let forward = product_forward();
    let right = [1.0f32, 0.0, 0.0];
    let up = [0.0f32, 1.0, 0.0];
    let span = [
        forward[0] + right[0] * ndc_x + up[0] * ndc_y,
        forward[1] + right[1] * ndc_x + up[1] * ndc_y,
        forward[2] + right[2] * ndc_x + up[2] * ndc_y,
    ];
    Ray {
        origin: product_origin(),
        dir: normalize3(span),
        max_t: PRODUCT_MAX_T,
        surface: PRODUCT_SURFACE,
        max_steps: PRODUCT_STEPS,
    }
}

fn product_scene() -> Tree {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../assetc/tests/fixtures/boot_scene.ron");
    let input = std::fs::canonicalize(&raw)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", raw.display()));
    let mut watch = assetc::watch::Watch::default();
    let output = std::env::temp_dir().join(format!("merkles_c2f_{}.bin", std::process::id()));
    let outcome = watch.observe(
        input.to_str().expect("fixture path is utf-8"),
        output.to_str().expect("temp path is utf-8"),
    );
    match outcome {
        assetc::watch::Outcome::Rebuilt { .. } => {}
        assetc::watch::Outcome::Unchanged => {}
        assetc::watch::Outcome::Failed(reason) => {
            panic!("boot scene must compile: {reason}")
        }
    }
    let bytes = std::fs::read(&output).expect("asset was written");
    let loaded = assetc::reader::from_bytes(&bytes)
        .unwrap_or_else(|error| panic!("{} must load: {error:?}", output.display()));
    let tree = Tree {
        nodes: loaded.nodes,
        params: loaded.params,
    };
    render::validate(&tree).expect("demo scene must validate");
    tree
}

fn main() {
    let tree = product_scene();
    println!(
        "// Scene: {} nodes, {} params, depth {}",
        tree.nodes.len(),
        tree.params.len(),
        render::tree_depth(&tree)
    );
    println!();

    // March the full grid on CPU reference
    println!("// Marching {}x{} grid on CPU reference...", R1_W, R1_H);
    let start = std::time::Instant::now();

    let mut depths = vec![MISS; (R1_W * R1_H) as usize];
    let mut hits = 0usize;

    for py in 0..R1_H {
        for px in 0..R1_W {
            let ray = ray_at(px, py, R1_W, R1_H);
            let result = march(
                &tree,
                ROOT,
                ray.origin,
                ray.dir,
                ray.max_t,
                ray.surface,
                ray.max_steps,
            );
            let idx = (py * R1_W + px) as usize;
            depths[idx] = match result {
                Some(hit) => hit.t,
                None => MISS,
            };
            if result.is_some() {
                hits += 1;
            }
        }
    }

    let elapsed = start.elapsed();
    println!(
        "// Full march: {} hits ({:.1}% of pixels) in {:.2} ms",
        hits,
        100.0 * hits as f64 / (R1_W * R1_H) as f64,
        elapsed.as_secs_f64() * 1000.0
    );

    // Analyze coarse-to-fine potential
    analyze_coarse_to_fine(&depths, R1_W, R1_H);
}

fn analyze_coarse_to_fine(depths: &[f32], width: u32, height: u32) {
    println!();
    println!("// === Coarse-to-Fine Analysis ===");

    let coarse_w = width / COARSE_FACTOR;
    let coarse_h = height / COARSE_FACTOR;

    // Step 1: Build coarse hit map (which coarse blocks have hits?)
    let mut coarse_blocks_with_hits = 0u64;
    let mut coarse_block_has_hit = vec![false; (coarse_w * coarse_h) as usize];

    for cy in 0..coarse_h {
        for cx in 0..coarse_w {
            let mut has_hit = false;
            // Check all fine pixels in this coarse block
            for dy in 0..COARSE_FACTOR {
                for dx in 0..COARSE_FACTOR {
                    let fx = cx * COARSE_FACTOR + dx;
                    let fy = cy * COARSE_FACTOR + dy;
                    let idx = (fy * width + fx) as usize;
                    if depths[idx] != MISS && depths[idx] < PRODUCT_MAX_T {
                        has_hit = true;
                    }
                }
            }
            if has_hit {
                coarse_blocks_with_hits += 1;
                coarse_block_has_hit[(cy * coarse_w + cx) as usize] = true;
            }
        }
    }

    println!(
        "// Coarse grid: {}x{} = {} blocks",
        coarse_w,
        coarse_h,
        coarse_w * coarse_h
    );
    println!(
        "// Blocks with hits: {} ({:.1}%)",
        coarse_blocks_with_hits,
        100.0 * coarse_blocks_with_hits as f64 / (coarse_w * coarse_h) as f64
    );

    // Step 2: Count how many fine pixels would need marching
    // A fine pixel needs marching if its coarse block OR any neighboring coarse block has a hit
    let mut pixels_to_march = 0u64;

    for fy in 0..height {
        for fx in 0..width {
            let cx = fx / COARSE_FACTOR;
            let cy = fy / COARSE_FACTOR;

            // Check this block and neighbors
            let mut should_march = false;
            for dy in -1i32..=1i32 {
                for dx in -1i32..=1i32 {
                    let nx = cx as i32 + dx;
                    let ny = cy as i32 + dy;
                    if nx >= 0 && nx < coarse_w as i32 && ny >= 0 && ny < coarse_h as i32 {
                        let nidx = (ny as u32 * coarse_w + nx as u32) as usize;
                        if coarse_block_has_hit[nidx] {
                            should_march = true;
                            break;
                        }
                    }
                }
                if should_march {
                    break;
                }
            }

            if should_march {
                pixels_to_march += 1;
            }
        }
    }

    let total_pixels = (width * height) as u64;
    let pixels_skipped = total_pixels - pixels_to_march;

    println!();
    println!(
        "// Fine pixels to march: {} ({:.1}% of {})",
        pixels_to_march,
        100.0 * pixels_to_march as f64 / total_pixels as f64,
        total_pixels
    );
    println!(
        "// Fine pixels to skip:  {} ({:.1}%)",
        pixels_skipped,
        100.0 * pixels_skipped as f64 / total_pixels as f64
    );

    // Estimate speedup (very rough - assumes coarse pass is free)
    let work_ratio = pixels_to_march as f64 / total_pixels as f64;
    println!();
    println!(
        "// Estimated work reduction: {:.1}x fewer ray marches",
        1.0 / work_ratio
    );
    println!("// (This ignores coarse pass cost and memory access patterns)");
}
