//! R1's real question, on a device: how long does a *marched frame* take when the ray loop is the shader?
//!
//! `cargo run --release -p gpu --example on_device_march`
//!
//! `frame_curve` priced a frame as 46 dispatches of `evaluate_points` over the same grid, each one handing
//! the grid to the queue and taking the answer back, and reported 4,005-4,015 ms for 1080p against R1's
//! 16.67 ms. That number is mostly the harness: a renderer does not do that. This measures the shape the
//! gate actually names - upload the rays once, march them in `interp.wgsl`, copy back only the depths - and
//! so replaces an upper bound on the harness with a measurement of the work.
//!
//! What it is still not: the hero scene, culling, or a BVH. The tree is `frame_curve`'s three-node ball on
//! a slab, deliberately the same one so the two examples describe one scene, and the march is the naive
//! sphere trace with no hierarchy. docs/04's law says cost = steps x nodes, so a number from eleven nodes
//! would not be this number either way. Timing is never hashed and never gate evidence (ADR-017, R4
//! informational); what is asserted here is that the march *happened* and that it produced the same
//! picture as `render::march`, because a dispatch the driver optimised away would otherwise arrive here as
//! a beautiful frame time.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::Interpreter;
use render::{march, Ray, Tree, ROOT};

/// `frame_curve`'s scene, word for word, so the two examples price one field.
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

const SURFACE: f32 = 0.01;
const MAX_STEPS: u32 = 64;
const MAX_T: f32 = 24.0;

/// A pinhole grid at z-forward, the same arithmetic `render::raster` uses. The rays are handed to both
/// marchers unchanged, so this is not the term under test - the step law is.
fn rays(width: u32, height: u32) -> Vec<Ray> {
    let origin = [0.0f32, 0.0, -6.0];
    let half_height = 0.5;
    let aspect = width as f32 / height as f32;
    let mut out = Vec::with_capacity((width * height) as usize);
    for py in 0..height {
        for px in 0..width {
            let u = (px as f32 + 0.5) / width as f32;
            let v = (py as f32 + 0.5) / height as f32;
            let ndc_x = (2.0 * u - 1.0) * half_height * aspect;
            let ndc_y = (1.0 - 2.0 * v) * half_height;
            let dir = [ndc_x, ndc_y, 1.0];
            let length = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
            out.push(Ray {
                origin,
                dir: [dir[0] / length, dir[1] / length, dir[2] / length],
                max_t: MAX_T,
                surface: SURFACE,
                max_steps: MAX_STEPS,
            });
        }
    }
    out
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP on_device_march: no device ({error:?}) - a frame time from a software rasteriser \
                 would be quoted as a GPU measurement, which is the exact mistake open_hardware exists to avoid"
            );
            return;
        }
    };
    let tree = scene();
    println!("// adapter          : {}", interpreter.adapter());

    // The anchor, on the small grid: the device's march against `render::march`, ray for ray.
    let check = rays(64, 48);
    let on_device = interpreter
        .march(&tree, ROOT, &check)
        .expect("the small grid must march");
    let mut worst = 0.0f32;
    let mut cpu_hits = 0usize;
    let mut disagreements = 0usize;
    for (ray, depth) in check.iter().zip(on_device.depths.iter()) {
        let hit = march(
            &tree,
            ROOT,
            ray.origin,
            ray.dir,
            ray.max_t,
            ray.surface,
            ray.max_steps,
        );
        match (hit, *depth < 1.0e37) {
            (Some(hit), true) => {
                cpu_hits += 1;
                worst = worst.max((hit.t - *depth).abs());
            }
            (None, false) => {}
            _ => disagreements += 1,
        }
    }
    println!(
        "// anchor           : {} rays, device hits {} / reference hits {}, {} hit-vs-miss disagreements, max |t| deviation {:.6}",
        on_device.rays,
        on_device.hits,
        cpu_hits,
        disagreements,
        worst
    );
    assert!(
        on_device.hits > 0,
        "the march returned no surface at all, so the frame it timed is empty and the numbers below are the cost of computing nothing"
    );
    assert!(
        disagreements == 0 && worst < 0.002,
        "the on-device march and render::march disagree, so no timing below describes the reference's frame law"
    );
    // The anchor was falsified before it was trusted: replacing the shader's advance with
    // `max(distance * 0.5, surface)` - a march that converges to the same surfaces, just slower - gave
    // 16 hit-vs-miss disagreements and a 0.007294 depth deviation, and this assert exited 101. Note which
    // half caught it: the depth tolerance alone did not, because 0.007 sits under any tolerance loose
    // enough for a 64-step march. The hit/miss SET is the sensitive term, which is the same reason
    // `raster_probe` counts surfaces rather than trusting averages. The tolerance is 0.002 because the
    // honest deviation measured here is 0.000000, with room left for a vendor's step-around-surface.

    println!("// grid             | rays      | hits      | max steps | ms submit | ms frame (dispatch + one copy back)");
    let mut largest = (0usize, 0usize, 0.0f64);
    for (w, h) in [
        (64usize, 48usize),
        (256, 192),
        (640, 360),
        (1280, 720),
        (1920, 1080),
    ] {
        let grid = rays(w as u32, h as u32);
        let march = interpreter
            .march(&tree, ROOT, &grid)
            .expect("the grid must march");
        println!(
            "// {:>4}x{:<4} | {:>9} | {:>9} | {:>9} | {:>9.3} | {:>10.3}",
            w,
            h,
            march.rays,
            march.hits,
            march.max_steps,
            march.ms_submit_only,
            march.ms_frame_with_read
        );
        if march.ms_frame_with_read > largest.2 {
            largest = (march.rays, march.hits, march.ms_frame_with_read);
        }
    }

    let budget = 1000.0 / 60.0;
    let ratio = largest.2 / budget;
    println!(
        "// 1080p            : {} rays of a {}-step march, {} hits, {:.1} ms against R1's {:.2} ms whole-frame budget",
        largest.0,
        MAX_STEPS,
        largest.1,
        largest.2,
        budget
    );
    println!(
        "// R1               : {}",
        if ratio <= 1.0 {
            "met for THIS scene on THIS adapter - three nodes, no culling, no BVH, and a march whose rays are generated on the CPU, so it is a lower bound on the work and an upper bound on nothing. Read it alongside docs/04's step x node law, not as a green gate."
        } else {
            "not met, and now measured as a march rather than as 46 harness round-trips: this is the frame's own cost on a low-end discrete GPU for an unculed three-node field. The levers the gate names - culling, the bound hierarchy, fewer rays per pixel - are what remains, not a shader-speed problem."
        }
    );
    println!(
        "// vs frame_curve   : 46 dispatches with one copy back cost 4,005-4,015 ms at 1080p (MX250, GTX 1070). This is ONE dispatch of the same field work per pixel, so the two numbers together say how much of that was the harness."
    );
}
