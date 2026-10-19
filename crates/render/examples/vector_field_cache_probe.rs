//! Vector-field cache probe: measure how many pixels' cached surfaces survive a camera move.
//!
//! This simulates the vector-field approach without implementing it on the GPU. The idea:
//! 1. March a frame normally, storing per-pixel {hit_point}
//! 2. Move camera by some delta (translation or rotation)
//! 3. For each pixel: evaluate the SDF at the cached world point and accept it if |SDF| is
//!    within tolerance
//! 4. Count how many validate successfully vs need remarching
//!
//! What this measures: the potential work reduction from temporal caching. If 90% of pixels
//! validate, we only march 10% of rays per frame instead of 100%.
//!
//! What it does NOT measure: this is an upper bound, not a re-hit test. Step 3 asks only whether
//! the surface still exists at that world point. It never re-marches the ray, so it cannot see a
//! surface that became occluded, and tolerance accepts a point that is merely near the surface
//! rather than the point the moved camera would actually hit. `render::temporal_cache` is the
//! path that validates by re-marching, and its numbers are the ones to quote.

use render::{march, raster::Camera, sdf_eval, Tree, Vec3, ROOT};

/// Cached surface data for one pixel
#[derive(Debug, Clone, Copy)]
struct SurfaceCache {
    hit_point: Vec3, // World-space hit position
    valid: bool,     // Whether this pixel found a surface
}

/// Simulate camera movement and measure cache hit rate
///
/// `_moved_camera` is deliberately unused: the validity test below asks only whether the cached
/// world point still lies near the surface, which a static scene answers the same way from every
/// camera. This probe therefore cannot report motion sensitivity — its rate is the initial hit
/// rate. See the header.
fn measure_cache_hit_rate(
    tree: &Tree,
    initial_camera: &Camera,
    _moved_camera: &Camera,
    surface_eps: f32,
) -> (usize, usize, Vec<bool>) {
    let width = initial_camera.width;
    let height = initial_camera.height;
    let total_pixels = (width * height) as usize;

    // Step 1: March initial frame and cache results
    let mut cache = Vec::with_capacity(total_pixels);
    let mut initial_hits = 0usize;
    for py in 0..height {
        for px in 0..width {
            let dir = ray_for_pixel(initial_camera, px, py);
            let hit = march(
                tree,
                ROOT,
                initial_camera.origin,
                dir,
                initial_camera.max_t,
                surface_eps,
                initial_camera.max_steps,
            );

            match hit {
                Some(hit) => {
                    initial_hits += 1;
                    cache.push(SurfaceCache {
                        hit_point: hit.point,
                        valid: true,
                    });
                }
                None => {
                    cache.push(SurfaceCache {
                        hit_point: [0.0, 0.0, 0.0],
                        valid: false,
                    });
                }
            }
        }
    }

    println!(
        "Initial frame: {} surface pixels / {} total ({:.1}% hit rate)\n",
        initial_hits,
        total_pixels,
        (initial_hits as f64 / total_pixels as f64) * 100.0
    );

    // Step 2: Move camera and test cache validity
    let mut hits = 0usize;
    let mut misses = 0usize;
    let mut validation_results = Vec::with_capacity(total_pixels);

    for py in 0..height {
        for px in 0..width {
            let idx = (py * width + px) as usize;
            let cached = &cache[idx];

            if !cached.valid {
                // Sky pixel - always a miss (no surface to cache)
                misses += 1;
                validation_results.push(false);
                continue;
            }

            // Quick validation: evaluate SDF at predicted hit point
            // If |SDF(predicted)| < tolerance, the cached surface is still approximately valid
            // Use a more generous tolerance since camera movement means the exact hit point shifts
            let tolerance = surface_eps * 10.0; // Allow larger error for temporal reuse
            let predicted_dist = sdf_eval(tree, ROOT, cached.hit_point);

            if predicted_dist.abs() < tolerance {
                // Cache is valid! Surface still exists at this location
                hits += 1;
                validation_results.push(true);
            } else {
                // Cache invalid - surface moved or occlusion changed
                misses += 1;
                validation_results.push(false);
            }
        }
    }

    (hits, misses, validation_results)
}

fn ray_for_pixel(camera: &Camera, px: u32, py: u32) -> Vec3 {
    let forward = normalize_vec3(camera.forward);
    let right = normalize_vec3(camera.right);
    let up = normalize_vec3(camera.up);

    let aspect = camera.width as f32 / camera.height as f32;
    let u = (px as f32 + 0.5) / camera.width as f32;
    let v = (py as f32 + 0.5) / camera.height as f32;
    let ndc_x = (2.0 * u - 1.0) * camera.half_height * aspect;
    let ndc_y = (1.0 - 2.0 * v) * camera.half_height;

    normalize_vec3([
        forward[0] + right[0] * ndc_x + up[0] * ndc_y,
        forward[1] + right[1] * ndc_x + up[1] * ndc_y,
        forward[2] + right[2] * ndc_x + up[2] * ndc_y,
    ])
}

fn normalize_vec3(v: Vec3) -> Vec3 {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len == 0.0 {
        return [0.0, 0.0, 0.0];
    }
    [v[0] / len, v[1] / len, v[2] / len]
}

fn main() {
    println!("Vector-field cache probe: measuring surviving surface coverage");
    println!("=====================================================\n");

    // Build a simple test scene: sphere on ground plane
    let tree = build_test_scene();

    // Initial camera setup
    let camera = Camera {
        origin: [0.0, 1.5, -6.0],
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: 320,
        height: 240,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 64,
    };

    println!("Scene: sphere on ground plane");
    println!("Camera: at {:?}, looking +Z", camera.origin);
    println!(
        "Grid: {}x{} = {} pixels\n",
        camera.width,
        camera.height,
        camera.width * camera.height
    );

    // Test different camera movements
    let movements = vec![
        ("Translate forward 0.5 units", [0.0, 0.0, 0.5]),
        ("Translate right 0.5 units", [0.5, 0.0, 0.0]),
        ("Translate up 0.3 units", [0.0, 0.3, 0.0]),
        ("Rotate slightly (not implemented)", [0.0, 0.0, 0.0]), // No origin delta; see the note below
        ("Large translation 2 units", [2.0, 0.0, 0.0]),
    ];

    for (name, delta) in &movements {
        let mut moved_camera = camera;
        moved_camera.origin = [
            camera.origin[0] + delta[0],
            camera.origin[1] + delta[1],
            camera.origin[2] + delta[2],
        ];

        let (hits, misses, _) =
            measure_cache_hit_rate(&tree, &camera, &moved_camera, camera.surface);
        let total = hits + misses;
        let hit_rate = if total > 0 {
            (hits as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        println!(
            "{:40} → {:5} hits / {:5} misses = {:.1}% cache hit rate",
            name, hits, misses, hit_rate
        );
    }

    println!("\nInterpretation:");
    println!("- Every row above is the same number by construction: the validity test never reads");
    println!("  the moved camera, so this table does not measure how reuse decays with motion.");
    println!("- What it does measure: the share of pixels that found a surface at all, since a");
    println!("  surface point that exists still evaluates to |SDF| ~ 0 however the camera moved.");
    println!("- Sky pixels are always misses (no surface to cache).");
    println!("- For a motion-dependent reuse rate use render::temporal_cache, which re-marches.");
}

fn build_test_scene() -> Tree {
    use contracts::sdf::{
        pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
        NODE_PLANE, NODE_SMOOTH_UNION, NODE_SPHERE,
    };

    fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
            pack_params_len(len),
        ])
    }

    // Simple scene: sphere on ground plane
    let mut params = Vec::new();

    // Node 0: SmoothUnion (root)
    let params_first = params.len() as u16;
    params.push(0.1); // smooth_union k parameter

    let mut nodes = vec![record(NODE_SMOOTH_UNION, 2, 3, params_first, 1, true)];

    // Node 1: Sphere
    let params_first = params.len() as u16;
    params.extend_from_slice(&[0.0, 1.0, 0.0]); // center
    params.push(1.0); // radius

    nodes.push(record(NODE_SPHERE, 0, 1, params_first, 4, false));

    // Node 2: Plane (ground)
    let params_first = params.len() as u16;
    params.extend_from_slice(&[0.0, 1.0, 0.0]); // normal
    params.push(0.0); // offset

    nodes.push(record(NODE_PLANE, 0, 1, params_first, 4, false));

    Tree { nodes, params }
}
