//! How far can a ray travel before this raster stops resolving surface? A diagnostic, not a demo.
//!
//! `cargo run --release --example far_scale_probe -p render`
//!
//! OQ-56 came out of trying to point the demo's camera down a row of region cells: at 4,096 units the frame
//! was empty, while the same scenes at cell-local distances hit exactly where the single-tree renderer hits.
//! Three explanations were on the list - a step into a solid that the step budget cannot clear, a bound test
//! that rejects what it should keep, and `f32` losing the detail a `1e-3` stop test needs once the
//! coordinates are four digits. This walks a single sphere out along one axis and reports which of them it
//! is, at each distance, with the march instrumented rather than inferred.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE,
};
use render::raster::Camera;
use render::{march, Tree, ROOT};

fn sphere(radius: f32) -> Tree {
    Tree {
        nodes: vec![NodeRecord([
            pack_kind_and_arity(NODE_SPHERE, 0),
            1,
            pack_first_and_flags(0, FLAG_IS_ROOT),
            pack_params_len(4),
        ])],
        params: vec![radius, 0.0, 0.0, 0.0],
    }
}

fn camera(surface: f32, max_steps: u32, max_t: f32) -> Camera {
    Camera {
        origin: [0.0, 0.0, 0.0],
        forward: [1.0, 0.0, 0.0],
        right: [0.0, 1.0, 0.0],
        up: [0.0, 0.0, 1.0],
        half_height: 0.5,
        width: 8,
        height: 8,
        max_t,
        surface,
        max_steps,
    }
}

/// March the centre ray by hand so the trajectory is visible, not just the verdict.
fn trace(distance: f32, surface: f32, max_steps: u32) -> (String, u32, f32) {
    let camera = camera(surface, max_steps, 3.0 * distance + 64.0);
    let origin_local = [distance, 0.0, 0.0];
    let mut t = 0.0f32;
    let mut steps = 0u32;
    let mut closest = f32::MAX;
    while t < camera.max_t && steps < max_steps {
        let point = [t, 0.0, 0.0];
        let local = [
            point[0] - origin_local[0],
            point[1] - origin_local[1],
            point[2] - origin_local[2],
        ];
        // The same expression `stream::Region::field_at` evaluates.
        let distance_field = {
            let centre = [0.0f32, 0.0, 0.0];
            let dx = local[0] - centre[0];
            let dy = local[1] - centre[1];
            let dz = local[2] - centre[2];
            (dx * dx + dy * dy + dz * dz).sqrt() - 1.6
        };
        if distance_field.abs() < closest {
            closest = distance_field.abs();
        }
        if distance_field.abs() < surface {
            return (format!("hit at t {t:.4}"), steps, closest);
        }
        t += distance_field.max(surface);
        steps += 1;
    }
    (
        format!("no hit (t {t:.3}, max_t {:.1})", camera.max_t),
        steps,
        closest,
    )
}

fn main() {
    println!("// how far is too far for a 1e-3 stop test? one sphere, r 1.6, along +x");
    println!(
        "// host {} / arch {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!();
    println!(
        " distance | surface | max_steps | verdict                | steps | smallest |field| seen"
    );
    for distance in [8.0f32, 64.0, 512.0, 4096.0, 8192.0, 12288.0] {
        for (surface, max_steps) in [(1e-3f32, 96u32), (1e-2, 96), (1e-2, 1024), (1e-1, 96)] {
            let (verdict, steps, closest) = trace(distance, surface, max_steps);
            println!(
                " {:>8} | {:>7} | {:>9} | {:<22} | {:>5} | {:>21.6}",
                format!("{distance:.0}"),
                format!("{surface:.0e}"),
                max_steps,
                verdict,
                steps,
                closest,
            );
        }
        println!();
    }
    println!("// Read the two 1e-3 rows against the 1e-2 ones: if a distance hits with a coarser tolerance");
    println!("// and misses with a finer one, the stop test is the limit, not the march. If it misses with");
    println!("// both, look at `smallest |field| seen` - a value well above the tolerance means the trace");
    println!("// stepped over the surface, and a value that grows with `t` means the coordinate subtraction");
    println!("// is the one running out of digits.");

    // Cross-check against the crate's own marcher, so this file cannot report a behaviour `march` does not
    // share: the same sphere, placed by shifting the camera back to its cell origin, which is what
    // `stream::Region::field_at` effectively does for a region at that cell.
    let tree = sphere(1.6);
    for distance in [8.0f32, 4096.0, 12288.0] {
        let hit = march(
            &tree,
            ROOT,
            [-distance, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            2.0 * distance + 64.0,
            1e-2,
            1024,
        );
        println!(
            "// `march` with the camera a cell back from a sphere at {distance:.0}: {}",
            match hit {
                Some(hit) => format!("hit at t {:.4} after {} steps", hit.t, hit.steps),
                None => "no hit".to_string(),
            }
        );
    }
}
