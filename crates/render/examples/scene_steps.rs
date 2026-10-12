//! Spike S1, first CPU numbers: how many march steps a procedural scene costs per pixel.
//!
//! Not a frame time. The distinction is the whole point of running this: docs/04's R1 ("1080p/60 with
//! 500 culled entities") is a GPU claim, and the only honest thing a CPU can contribute before a WGSL
//! interpreter exists is the quantity the shader will also pay - **distance-field evaluations per
//! ray**. That number is arrangement- and epsilon-dependent, it is measurable today, and it is what
//! makes a budget discussion arithmetic instead of adjectives. The frame budget needs culling, a BVH,
//! and a GPU; none of those are claimed here.
//!
//! Run: `cargo run --release --example scene_steps -p render`

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SMOOTH_UNION, NODE_SPHERE, NODE_UNION,
};
use render::{march, Tree, ROOT};

/// A small scene with the joint that matters: two spheres blended, plus a rounded box joined sharply.
/// The blend is the interesting case because docs/04's step guarantee depends on it staying
/// 1-Lipschitz.
fn scene() -> Tree {
    let mut params: Vec<f32> = Vec::new();
    let mut push = |kind: u16, arity: u16, subtree: u32, p: &[f32], flags: u16| -> NodeRecord {
        let first = params.len() as u16;
        params.extend_from_slice(p);
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, flags),
            pack_params_len(p.len() as u16),
        ])
    };

    let left = push(NODE_SPHERE, 0, 1, &[0.7, -0.9, 0.0, 0.0], 0);
    let right = push(NODE_SPHERE, 0, 1, &[0.7, 0.9, 0.0, 0.0], 0);
    let blended = push(NODE_SMOOTH_UNION, 2, 3, &[0.5], FLAG_IS_ROOT);
    let block = push(NODE_ROUNDED_BOX, 0, 1, &[0.35, 0.35, 0.9, 0.12], 0);
    let whole = push(NODE_UNION, 2, 5, &[], 0);

    // Pre-order: union(box, smoothunion(spheres)). The records were built children-last, so the spans
    // are computed from the layout rather than asserted, and validate() is run before anything is
    // marched - an example that measures a broken tree measures nothing.
    let nodes = fix_spans(vec![whole, block, blended, left, right]);
    let tree = Tree { nodes, params };
    render::validate(&tree).expect("the example's own scene must validate");
    tree
}

/// Recompute `subtree_len` from pre-order, so the asset is derived rather than asserted. A mistake
/// here is exactly what `validate` exists to catch, and the assert below makes it loud.
fn fix_spans(mut nodes: Vec<NodeRecord>) -> Vec<NodeRecord> {
    let arity = |record: &NodeRecord| record.arity() as usize;
    let mut index = nodes.len();
    while index > 0 {
        index -= 1;
        // Walk children by their already-fixed spans; leaves are 1.
        let mut span = 1usize;
        let mut cursor = index + 1;
        let children = arity(&nodes[index]);
        for _ in 0..children {
            span += nodes[cursor].subtree_len() as usize;
            cursor += nodes[cursor].subtree_len() as usize;
        }
        nodes[index].0[1] = span as u32;
    }
    nodes
}

#[derive(Debug)]
struct Stats {
    hit: usize,
    miss: usize,
    steps_total: u64,
    steps_max: u32,
}

fn shoot(size: u32, surface: f32, budget: u32) -> Stats {
    let tree = scene();
    let mut stats = Stats {
        hit: 0,
        miss: 0,
        steps_total: 0,
        steps_max: 0,
    };
    let focal = 1.6f32;
    for py in 0..size {
        for px in 0..size {
            let x = (2.0 * ((px as f32 + 0.5) / size as f32) - 1.0) * focal;
            let y = (1.0 - 2.0 * ((py as f32 + 0.5) / size as f32)) * focal;
            let origin = [3.4f32, 1.1, 2.2];
            let mut dir = [x - origin[0], y - origin[1], -origin[2]];
            let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
            dir = [dir[0] / len, dir[1] / len, dir[2] / len];
            match march(&tree, ROOT, origin, dir, 24.0, surface, budget) {
                Some(hit) => {
                    stats.hit += 1;
                    stats.steps_total += hit.steps as u64;
                    stats.steps_max = stats.steps_max.max(hit.steps);
                }
                None => {
                    stats.miss += 1;
                    // A miss pays the whole budget, which is why sky rays are the expensive pixels.
                    stats.steps_total += budget as u64;
                    stats.steps_max = stats.steps_max.max(budget);
                }
            }
        }
    }
    stats
}

fn main() {
    println!(
        "// spike S1, part one: evaluations per ray on the CPU. NOT a frame time - see the header."
    );
    println!("// scene: smoothunion(sphere -0.9, sphere 0.9; k=0.5) UNION roundedBox(0.35,0.35,0.9; r=0.12)");
    println!();
    println!("resolution | surface eps | rays    | hit %  | evals/ray | max steps | sky rays");
    for size in [32u32, 64, 128] {
        for surface in [1e-3f32, 1e-4, 1e-5] {
            let s = shoot(size, surface, 96);
            let rays = (size * size) as u64;
            println!(
                "{:>8}x{:<5} | {:>11.0e} | {:>7} | {:>5.1} | {:>9.2} | {:>9} | {:>8}",
                size,
                size,
                surface,
                rays,
                100.0 * s.hit as f64 / rays as f64,
                s.steps_total as f64 / rays as f64,
                s.steps_max,
                s.miss,
            );
        }
    }
    println!();
    println!(
        "// Reading for docs/04 and R1: cost per pixel is dominated by rays that MISS, because a"
    );
    println!(
        "// miss spends the whole step budget proving emptiness. That is why docs/04 pairs the"
    );
    println!(
        "// march with a BVH and a bounding-volume early-out rather than trusting the field alone,"
    );
    println!(
        "// and why 'the marcher is 60 FPS' cannot be argued from these numbers: no culling, no"
    );
    println!("// BVH, no GPU, one thread, and a scene of two nodes more than a toy.");
}
