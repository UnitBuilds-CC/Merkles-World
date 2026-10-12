//! What a single scene enclosure actually buys, measured on scenes of increasing size.
//!
//! `cargo run --release --example cull_probe -p render`
//!
//! docs/04's culling claim is "naive evaluation of many entities collapses the frame rate; a BVH skips
//! them in `O(log N)`", and the raster probe turned the first half into a number: on the boot's grid the
//! third of rays that hit nothing pay 69.6% of all evaluations. [`render::cull`] is the cheapest possible
//! answer to that - one conservative sphere around the whole scene, one segment test per ray - so this
//! example exists to report what it saves and, more usefully, **where it stops helping**. A bound that
//! contains the scene is only tight if the scene is compact; spread the same spheres down a line and the
//! root sphere grows until every ray "might" hit, and the saving goes to zero without a single line of
//! the early-out being wrong. That is the case a real hierarchy is for, and having it measured is what
//! stops the next structure from being built on a vibe.
//!
//! Two arrangements per count, one line, one cluster, same spheres, same camera. The interesting column
//! is the last: if `skipped` collapses between the two, the enclosure is doing its job and the job it
//! cannot do is the loose bound.
//!
//! Informational output, release profile, one machine - the same labelling every timing here carries.
//! Unlike the other probes this one prints no milliseconds: everything it reports is a count, and counts
//! repeat across runners.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE, NODE_UNION,
};
use render::cull::{scene_enclosure, Enclosure};
use render::raster::{render, render_culled, Camera};
use render::{validate, Tree};

const RADIUS: f32 = 0.4;
const SPACING: f32 = 1.2;

/// Pre-order union tree over `centres`, built the way `assetc`'s lowering would: the branch's
/// `subtree_len` is arithmetic (a full binary tree over `count` leaves is `2 * count - 1` nodes), so no
/// fix-up pass is needed and the root flag lands on node 0 by construction.
fn scene(centres: &[[f32; 3]]) -> Tree {
    let mut nodes = Vec::new();
    let mut params: Vec<f32> = Vec::new();
    emit(&mut nodes, &mut params, centres, 0, centres.len(), true);
    Tree { nodes, params }
}

fn emit(
    nodes: &mut Vec<NodeRecord>,
    params: &mut Vec<f32>,
    centres: &[[f32; 3]],
    from: usize,
    to: usize,
    root: bool,
) {
    let flags = if root { FLAG_IS_ROOT } else { 0 };
    if to - from == 1 {
        let centre = centres[from];
        let first = params.len() as u16;
        params.extend([RADIUS, centre[0], centre[1], centre[2]]);
        nodes.push(NodeRecord([
            pack_kind_and_arity(NODE_SPHERE, 0),
            1,
            pack_first_and_flags(first, flags),
            pack_params_len(4),
        ]));
        return;
    }
    let count = (to - from) as u32;
    nodes.push(NodeRecord([
        pack_kind_and_arity(NODE_UNION, 2),
        count * 2 - 1,
        pack_first_and_flags(0, flags),
        pack_params_len(0),
    ]));
    let mid = from + count as usize / 2;
    emit(nodes, params, centres, from, mid, false);
    emit(nodes, params, centres, mid, to, false);
}

fn camera() -> Camera {
    Camera {
        origin: [0.0, 0.0, -14.0],
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: 64,
        height: 48,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

fn line(count: usize) -> Vec<[f32; 3]> {
    (0..count)
        .map(|i| [(i as f32 - count as f32 / 2.0) * SPACING, 0.0, 0.0])
        .collect()
}

fn cluster(count: usize) -> Vec<[f32; 3]> {
    // Same number of spheres, packed into a cube of side `2 * RADIUS + a little`, because the question
    // is about the bound's tightness rather than about the count.
    (0..count)
        .map(|i| {
            let step = 2.0 * RADIUS + 0.1;
            let per_row = 4usize;
            let row = i / per_row;
            let column = i % per_row;
            let depth = i / (per_row * per_row);
            [column as f32 * step, row as f32 * step, depth as f32 * step]
        })
        .collect()
}

fn report(arrangement: &str, tree: &Tree, camera: &Camera) {
    assert!(validate(tree).is_ok(), "{arrangement} must validate");
    let spheres = tree.nodes.iter().filter(|n| n.arity() == 0).count();
    let (plain, plain_stats) = render(tree, camera);
    let (culled, culled_stats) = render_culled(tree, camera);
    assert_eq!(
        plain.pixels, culled.pixels,
        "{arrangement}: culling changed pixels"
    );
    assert_eq!(
        plain.depth, culled.depth,
        "{arrangement}: culling changed depth"
    );
    assert_eq!(plain.hit_count(), culled.hit_count());
    let bound = match scene_enclosure(tree) {
        Enclosure::Bounded { radius, .. } => radius,
        Enclosure::Unbounded => f32::NAN,
    };
    let rays = u64::from(plain_stats.rays);
    let saved = plain_stats.evaluations - culled_stats.evaluations;
    println!(
        " {:>9} | {:>5} | {:>5} | {:>7.2} | {:>9} | {:>10} | {:>8} | {:>6.1}% | {:>6.1}%",
        arrangement,
        spheres,
        plain_stats.hits,
        bound,
        plain_stats.evaluations,
        culled_stats.evaluations,
        culled_stats.skipped,
        100.0 * culled_stats.skipped as f64 / rays as f64,
        100.0 * saved as f64 / plain_stats.evaluations as f64
    );
}

fn main() {
    println!(
        "// culling probe - one scene enclosure, two arrangements, identical pixels either way"
    );
    println!(
        "// host {} / arch {}; counts only, no milliseconds",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!();
    println!(
        " arrangement | spheres |  hits | bound r | evals plain | evals culled | rays skipped | skipped | work saved"
    );
    let camera = camera();
    for count in [1usize, 8, 32, 128] {
        report("cluster", &scene(&cluster(count)), &camera);
        report("line", &scene(&line(count)), &camera);
    }
    println!();
    println!("// What the table says:");
    println!(
        "// - the cluster keeps a tight sphere as it grows, so most rays are proved empty and the"
    );
    println!(
        "//   saving tracks the miss share docs/04 names. This is the case one bound can serve."
    );
    println!("// - the line's bound grows with the scene, and the moment it covers the frustum's corners");
    println!(
        "//   every ray 'might' hit: skipped falls toward zero and the saving with it, while the"
    );
    println!("//   pixels stay identical. The early-out did not fail; it ran out of information.");
    println!(
        "// - so the next structure is not optional polish, it is the only thing that can keep the"
    );
    println!(
        "//   bound tight as entities spread - a hierarchy of these same conservative spheres,"
    );
    println!(
        "//   traversed per ray or per tile. docs/04's O(log N) claim is now stated against a"
    );
    println!("//   measured baseline: the percentages in the last two columns are what a BVH has to beat");
    println!("//   at the same hit fraction.");
    println!(
        "// - unchanged by this probe: gate R1 and R2 are frame-time claims and there is still no"
    );
    println!("//   GPU. Counts here are work avoided, never nanoseconds saved.");
}
