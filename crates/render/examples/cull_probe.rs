//! What culling actually buys, on scenes that grow and on scenes that spread.
//!
//! `cargo run --release --example cull_probe -p render`
//!
//! docs/04's culling claim is "naive evaluation of many entities collapses the frame rate; a BVH skips
//! them in `O(log N)`", and the raster probe turned the first half into a number: on the boot's grid the
//! third of rays that hit nothing pay 69.6% of all evaluations. [`render::cull`] answers that in two
//! layers - one bound for the whole scene, tested once per ray, and one bound per node, tested inside the
//! march - so this reports what each layer is worth and where each stops helping. A bound containing the
//! scene is tight only while the scene is compact: spread the same spheres down a line, the root sphere
//! grows until every ray "might" hit, and the ray layer's saving goes to zero without one line of it
//! being wrong. That is the case a hierarchy exists for.
//!
//! **Why the last three columns count nodes rather than steps.** That is the lesson of this probe's first
//! run, where the per-node layer looked worthless: a skipped subtree is one that would have returned a
//! distance *larger* than the one already in hand, so the march takes identical steps either way and a
//! step count cannot see the skip at all. The only unit it can show up in is nodes entered per frame.
//! `render_culled` therefore counts without skipping, so the two node columns are the same frame - same
//! rays, same steps, same pixels - measured twice, and their difference is exactly the work the per-node
//! bounds removed.
//!
//! Informational output, release profile, one machine. Unlike the other probes this prints no
//! milliseconds: every column is an integer or a ratio of integers, and integers are what repeat across
//! runners. No column here may be quoted as a frame time - gates R1 and R2 need a GPU that this
//! repository does not have yet.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE, NODE_UNION,
};
use render::cull::{scene_enclosure, Enclosure};
use render::raster::{render, render_bnb, render_culled, Camera};
use render::{validate, Tree};

const RADIUS: f32 = 0.4;
const SPACING: f32 = 1.2;

/// Pre-order union tree over `centres`, built the way `assetc`'s lowering would: a branch's `subtree_len`
/// is arithmetic (a full binary tree over `count` leaves is `2 * count - 1` nodes), so no fix-up pass is
/// needed and the root flag lands on node 0 by construction.
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

/// Spheres along x, so the scene bound grows with the count.
fn line(count: usize) -> Vec<[f32; 3]> {
    (0..count)
        .map(|i| [(i as f32 - count as f32 / 2.0) * SPACING, 0.0, 0.0])
        .collect()
}

/// The same spheres packed into a cube, so the bound stays tight. The question is the bound's shape,
/// never the count.
fn cluster(count: usize) -> Vec<[f32; 3]> {
    (0..count)
        .map(|i| {
            let step = 2.0 * RADIUS + 0.1;
            let per_row = 4usize;
            [
                (i % per_row) as f32 * step,
                (i / per_row) as f32 * step,
                (i / (per_row * per_row)) as f32 * step,
            ]
        })
        .collect()
}

fn report(arrangement: &str, tree: &Tree, camera: &Camera) {
    assert!(validate(tree).is_ok(), "{arrangement} must validate");
    let spheres = tree.nodes.iter().filter(|node| node.arity() == 0).count();
    let (plain, plain_stats) = render(tree, camera);
    let (culled, culled_stats) = render_culled(tree, camera);
    let (bnb, bnb_stats) = render_bnb(tree, camera);

    // Everything the columns mean rests on this: culling may change work, never the picture.
    assert_eq!(plain.pixels, culled.pixels, "{arrangement}: pixels changed");
    assert_eq!(plain.depth, culled.depth, "{arrangement}: depth changed");
    assert_eq!(plain.pixels, bnb.pixels, "{arrangement}: pixels changed");
    assert_eq!(plain.depth, bnb.depth, "{arrangement}: depth changed");
    assert_eq!(plain.hit_count(), culled.hit_count());
    assert_eq!(plain.hit_count(), bnb_stats.hits);
    assert_eq!(
        culled_stats.evaluations, bnb_stats.evaluations,
        "{arrangement}: a skip changed the step count, which is what it must never do"
    );

    let bound = match scene_enclosure(tree) {
        Enclosure::Bounded { radius, .. } => radius,
        Enclosure::Unbounded => f32::NAN,
    };
    let rays_skipped = 100.0 * f64::from(culled_stats.skipped) / f64::from(plain_stats.rays);
    let avoided = 100.0 * (culled_stats.visits.saturating_sub(bnb_stats.visits)) as f64
        / culled_stats.visits.max(1) as f64;
    println!(
        " {:>9} | {:>5} | {:>5} | {:>7.2} | {:>9} | {:>8.1}% | {:>14} | {:>12} | {:>8.1}%",
        arrangement,
        spheres,
        plain_stats.hits,
        bound,
        plain_stats.evaluations,
        rays_skipped,
        culled_stats.visits,
        bnb_stats.visits,
        avoided,
    );
}

fn main() {
    println!(
        "// culling probe - a bound per ray and a bound per node, identical pixels in all three paths"
    );
    println!(
        "// host {} / arch {}; integers only, no milliseconds",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!();
    println!(
        " arrangement | spheres |  hits | bound r | evals plain | rays skipped | nodes unpruned | nodes pruned | work avoided"
    );
    let camera = camera();
    for count in [1usize, 8, 32, 128] {
        report("cluster", &scene(&cluster(count)), &camera);
        report("line", &scene(&line(count)), &camera);
    }
    println!();
    println!("// What the table says:");
    println!(
        "// - `evals plain` and `rays skipped` are the step-level story. One tight bound proves"
    );
    println!(
        "//   99.9% of rays empty on a compact scene; the same 128 spheres laid along a line grow"
    );
    println!(
        "//   that bound to r 76.6, past the frustum, and the ray layer is then worth nothing."
    );
    println!(
        "// - `nodes unpruned` against `nodes pruned` is the per-node story, measured on the same"
    );
    println!("//   frame with the same steps: the two differ only by subtrees the bounds proved irrelevant");
    println!(
        "//   at that point in the march. Steps cannot show a skip; nodes can, which is why this"
    );
    println!("//   probe has both and why its first run reported a saving of exactly zero.");
    println!("// - `work avoided` is where ADR-024's argument lands. The layer that survives a spreading");
    println!(
        "//   scene is the one holding many bounds and choosing per step, not the one holding a"
    );
    println!("//   single bound per ray - and now that it is measurable, the claim can be priced instead");
    println!("//   of asserted.");
    println!("// - what is still not here: gates R1 and R2 are frame-time claims with no GPU behind them.");
    println!("//   Nodes avoided per frame is work; work has never been calibrated to nanoseconds in this");
    println!("//   repository, so no column above may be quoted as a frame time.");
}
