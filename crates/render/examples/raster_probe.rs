//! The CPU raster probe, with timings. Informational output, never a gate: docs/11's R1 is a GPU frame
//! budget and steps per ray on a core are not nanoseconds per frame on a GPU.
//!
//! `cargo run --release -p render --example raster_probe`
//!
//! Writes a PPM into the system temp directory so there is something to look at, and prints the numbers
//! docs/04's cost story has been missing: hit fraction, evaluations, and evaluations per pixel, at three
//! resolutions on one tree. A BVH's job is to make the miss-heavy majority of those pixels cost
//! something near zero, so this is the line the culling work has to beat - measured, on this machine, in
//! this profile, and therefore labelled as such.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_PLANE, NODE_SPHERE, NODE_UNION,
};
use render::raster::{render, Camera};
use render::{validate, Tree};
use std::time::Instant;

/// The march's step budget, the same 96 docs/04's S1 numbers were taken at.
const MAX_STEPS: u32 = 96;

/// A ball on a ground plane: the smallest scene whose image is worth looking at, and one that separates
/// the curved shading on the sphere from the flat shading on the plane.
fn scene() -> Tree {
    let ball = NodeRecord([
        pack_kind_and_arity(NODE_SPHERE, 0),
        1,
        pack_first_and_flags(0, 0),
        pack_params_len(4),
    ]);
    let ground = NodeRecord([
        pack_kind_and_arity(NODE_PLANE, 0),
        1,
        pack_first_and_flags(4, 0),
        pack_params_len(4),
    ]);
    let whole = NodeRecord([
        pack_kind_and_arity(NODE_UNION, 2),
        3,
        pack_first_and_flags(8, FLAG_IS_ROOT),
        pack_params_len(0),
    ]);
    let tree = Tree {
        // Pre-order: root, then its left child, then the right - which is what `assetc` emits and what
        // the evaluator's `index + 1` / `index + 1 + subtree_len` walk expects.
        nodes: vec![whole, ball, ground],
        params: vec![1.0, 0.0, 0.7, 0.0, 0.0, 1.0, 0.0, 0.0],
    };
    validate(&tree).expect("the probe scene validates");
    tree
}

fn camera(width: u32, height: u32) -> Camera {
    Camera {
        origin: [0.0, 1.4, -4.2],
        forward: [0.0, -0.22, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width,
        height,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: MAX_STEPS,
    }
}

fn main() {
    let tree = scene();
    println!(
        "// CPU raster probe - informational, not gate R1. One tree, three grids, this machine."
    );
    let mut wrote = None;

    for (width, height) in [(96u32, 72u32), (192, 144), (320, 240)] {
        let start = Instant::now();
        let (frame, stats) = render(&tree, &camera(width, height));
        let elapsed = start.elapsed();
        let rays = u64::from(stats.rays);
        let misses = u64::from(stats.rays - stats.hits);
        // Charged, not counted: a miss is billed at the full step budget, which is an upper bound.
        let miss_cost = misses * u64::from(MAX_STEPS);
        let hit_cost = stats.evaluations - miss_cost;
        println!(
            "{width}x{height:4} : {:>8.3} ms | hits {:>5} of {rays} ({:.1}% of the frame) | {:>7} evaluations | {:.1} per pixel | {:.3} us per ray",
            elapsed.as_micros() as f64 / 1_000.0,
            stats.hits,
            100.0 * f64::from(stats.hits) / stats.rays as f64,
            stats.evaluations,
            stats.evaluations as f64 / rays as f64,
            elapsed.as_micros() as f64 / rays as f64,
        );
        println!(
            "           :  hits average {hit_avg:.1} steps, and the {:.1}% of rays that found nothing \
             pay {:.1}% of the evaluations",
            100.0 * misses as f64 / rays as f64,
            100.0 * miss_cost as f64 / stats.evaluations as f64,
            hit_avg = if stats.hits == 0 {
                0.0
            } else {
                hit_cost as f64 / f64::from(stats.hits)
            },
        );
        if width == 320 {
            wrote = Some(frame);
        }
    }

    if let Some(frame) = wrote {
        let path = std::env::temp_dir().join("merkle_raster_probe.ppm");
        let mut text = format!("P3\n{} {}\n255\n", frame.width, frame.height);
        for chunk in frame.pixels.chunks(3) {
            text.push_str(&format!("{} {} {}\n", chunk[0], chunk[1], chunk[2]));
        }
        match std::fs::write(&path, text) {
            Ok(()) => println!(
                "image            : {} ({}x{}, PPM, normal-mapped colour - not a material model)",
                path.display(),
                frame.width,
                frame.height
            ),
            Err(error) => println!("image            : not written, {error}"),
        }
    }

    println!("// What this says about the claim: the field becomes a picture, the per-pixel cost is flat");
    println!("// across resolution on one fixed tree, and a third of the rays - the ones that never find");
    println!(
        "// surface - pay about 70% of the evaluations. That is docs/04's culling argument with a"
    );
    println!("// measured number beside it instead of only a derived one, and the number the BVH (ADR-024)");
    println!("// has to move.");
    println!("// The timings are the least reproducible line here (this session saw 53 ms and 86 ms for the");
    println!("// same 76,800-ray grid); the evaluation counts were identical every run, which is the point");
    println!("// worth carrying forward. And none of it says the frame budget is met: no GPU, no WGSL, no");
    println!("// BVH - gate R1 stays unmet.");
}
