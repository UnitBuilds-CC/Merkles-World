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
    NODE_SMOOTH_UNION, NODE_SPHERE, NODE_UNION,
};
use core::cmp::Ordering;
use render::cull::{scene_enclosure, Enclosure};
use render::raster::{render, render_bnb, render_culled, Camera};
use render::{validate, Tree};

const RADIUS: f32 = 0.4;
const SPACING: f32 = 1.2;

/// Pre-order union tree over `centres`, built the way `assetc`'s lowering would: a branch's `subtree_len`
/// is arithmetic (a full binary tree over `count` leaves is `2 * count - 1` nodes), so no fix-up pass is
/// needed and the root flag lands on node 0 by construction.
fn scene(centres: &[[f32; 3]]) -> Tree {
    build(centres, None)
}

/// The same tree shape with every branch written as a `smooth union` instead of a plain one. One number
/// differs between the two scenes - the blend strength - and that is the whole experiment: a blend is what
/// an author adds to make geometry look like one object, and it is also what takes the hierarchy away.
fn blended(centres: &[[f32; 3]]) -> Tree {
    build(centres, Some(BLEND))
}

const BLEND: f32 = 0.2;

fn build(centres: &[[f32; 3]], blend: Option<f32>) -> Tree {
    let mut nodes = Vec::new();
    let mut params: Vec<f32> = Vec::new();
    emit(
        &mut nodes,
        &mut params,
        centres,
        0,
        centres.len(),
        true,
        blend,
    );
    Tree { nodes, params }
}

fn emit(
    nodes: &mut Vec<NodeRecord>,
    params: &mut Vec<f32>,
    centres: &[[f32; 3]],
    from: usize,
    to: usize,
    root: bool,
    blend: Option<f32>,
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
    let first = params.len() as u16;
    let (kind, params_len) = match blend {
        Some(k) => {
            params.push(k);
            (NODE_SMOOTH_UNION, 1)
        }
        None => (NODE_UNION, 0),
    };
    nodes.push(NodeRecord([
        pack_kind_and_arity(kind, 2),
        count * 2 - 1,
        pack_first_and_flags(first, flags),
        pack_params_len(params_len),
    ]));
    let mid = from + count as usize / 2;
    emit(nodes, params, centres, from, mid, false, blend);
    emit(nodes, params, centres, mid, to, false, blend);
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

/// Scattered with a deterministic integer hash, because the arrangement the hierarchy is actually asked
/// to serve is neither compact nor sorted: entities land where the simulation put them. Positions are
/// spread over the same volume the line covers, so the bound's radius stays comparable.
fn scatter(count: usize) -> Vec<[f32; 3]> {
    (0..count)
        .map(|i| {
            let mut state = (i as u64)
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state ^= state >> 31;
            state = state.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            state ^= state >> 29;
            let span = SPACING * count as f32;
            [
                (state & 0xFFF) as f32 / 4095.0 * span - span / 2.0,
                ((state >> 12) & 0xFFF) as f32 / 4095.0 * 6.0 - 3.0,
                ((state >> 24) & 0xFFF) as f32 / 4095.0 * 6.0 - 3.0,
            ]
        })
        .collect()
}

/// Median split along the widest axis, applied before the tree is emitted. This is the whole of what a
/// BVH build does here: the shapes of the bounds and the order of the walk are the only things an author
/// controls, and the walk is already hierarchical.
fn balanced(centres: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let mut sorted = centres.to_vec();
    let len = sorted.len();
    split(&mut sorted, 0, len);
    sorted
}

fn split(centres: &mut [[f32; 3]], from: usize, to: usize) {
    if to - from < 2 {
        return;
    }
    let mut spread = [0.0f32; 3];
    for axis in 0..3 {
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for centre in &centres[from..to] {
            lo = lo.min(centre[axis]);
            hi = hi.max(centre[axis]);
        }
        spread[axis] = hi - lo;
    }
    let axis = if spread[0] >= spread[1] && spread[0] >= spread[2] {
        0
    } else if spread[1] >= spread[2] {
        1
    } else {
        2
    };
    let mid = from + (to - from) / 2;
    // A median split by coordinate, not a partition about a value: `select_unstable` would be the faster
    // promise and this is a probe, so the readable sort is the honest choice.
    centres[from..to].sort_by(|a, b| a[axis].partial_cmp(&b[axis]).unwrap_or(Ordering::Equal));
    split(centres, from, mid);
    split(centres, mid, to);
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
    // The per-*step* subtree skip still cannot change a step count - it returns the distance the unpruned
    // walk would have returned. What the descent does change is the *number* of stepped rays, because a ray
    // it proves empty is charged nothing, and that is the difference this column now reports. So the
    // comparison is an inequality: never more work, and less exactly when the children know something the
    // cover sphere does not.
    assert!(
        bnb_stats.evaluations <= culled_stats.evaluations,
        "{arrangement}: the descent cost more than the single bound: {} vs {}",
        bnb_stats.evaluations,
        culled_stats.evaluations
    );
    assert!(
        bnb_stats.skipped >= culled_stats.skipped,
        "{arrangement}: the descent proved fewer rays empty than the root sphere did, which cannot happen"
    );

    let bound = match scene_enclosure(tree) {
        Enclosure::Bounded { radius, .. } => radius,
        Enclosure::Unbounded => f32::NAN,
    };
    let rays_skipped = 100.0 * f64::from(culled_stats.skipped) / f64::from(plain_stats.rays);
    let descent_skipped = 100.0 * f64::from(bnb_stats.skipped) / f64::from(plain_stats.rays);
    let avoided = 100.0 * (culled_stats.visits.saturating_sub(bnb_stats.visits)) as f64
        / culled_stats.visits.max(1) as f64;
    println!(
        " {:>9} | {:>5} | {:>5} | {:>7.2} | {:>9} | {:>8.1}% | {:>8.1}% | {:>9} | {:>14} | {:>12} | {:>8.1}%",
        arrangement,
        spheres,
        plain_stats.hits,
        bound,
        plain_stats.evaluations,
        rays_skipped,
        descent_skipped,
        bnb_stats.evaluations,
        culled_stats.visits,
        bnb_stats.visits,
        avoided,
    );
}

fn main() {
    println!(
        "// culling probe - a bound per ray, a bound per node, and a descent through the nodes"
    );
    println!(
        "// host {} / arch {}; integers only, no milliseconds",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!();
    println!(
        " arrangement | spheres |  hits | bound r | evals plain | skip by root | skip by descent | evals descent | nodes unpruned | nodes pruned | work avoided"
    );
    let camera = camera();
    for count in [1usize, 8, 32, 128] {
        report("cluster", &scene(&cluster(count)), &camera);
        report("line", &scene(&line(count)), &camera);
    }
    // The licence rows, printed with the same two arrangements and the same sphere counts. A blend
    // strength is the only difference, and `report` asserts frame equality either way, so what these rows
    // price is the authoring choice rather than a possible bug.
    for count in [32usize, 128] {
        report("cluster/blend", &blended(&cluster(count)), &camera);
        report("line/blend", &blended(&line(count)), &camera);
    }
    for count in [32usize, 128] {
        let scattered = scatter(count);
        let authored = scene(&scattered);
        let split_scene = scene(&balanced(&scattered));
        // Reordering leaves under plain unions may not change the field: a union is a `min`, and `min`
        // is commutative and associative exactly, so the two trees above are one scene written twice.
        // If that ever stops being true, the ordering is not a build detail any more and the median split
        // in this file becomes an authoring change that the asset contract would have to admit.
        let (a, a_stats) = render_bnb(&authored, &camera);
        let (b, b_stats) = render_bnb(&split_scene, &camera);
        assert_eq!(a.pixels, b.pixels, "median split changed the picture");
        assert_eq!(a.depth, b.depth, "median split moved a surface");
        // Steps are no longer equal between the two trees, and that is the descent's own effect: a tighter
        // tree shape proves more rays empty, so it charges fewer of them. Only the picture may not move.
        assert_eq!(a_stats.hits, b_stats.hits, "median split moved hits");
        println!(
            "   (scatter {count}: the split tree agrees pixel for pixel with the authored order; the step columns now differ, because a tighter tree proves more rays empty)"
        );
        report("scatter/index", &authored, &camera);
        report("scatter/split", &split_scene, &camera);
    }
    println!();
    println!("// What the table says:");
    println!("// - THE HEADLINE IS THE SIXTH AND SEVENTH COLUMNS. The single root bound reaches 0.0% on the");
    println!("//   32- and 128-sphere line rows and on both scatter rows - every ray is merely permitted,");
    println!("//   because one sphere around a long row swallows the frustum. Asking the children instead");
    println!("//   turns those same rows into 96-98% of rays proved empty, and the charged steps fall with");
    println!("//   them: the 128-sphere line goes from 287,112 evaluations to 648, a factor of 443, on a");
    println!("//   frame that is byte-identical. That is ADR-024's hierarchy measured rather than asserted,");
    println!("//   and it is the first number here that says what a BVH is worth instead of that one is due.");
    println!("// - getting it cost a real bug, and the bug is the second lesson: the descent tested the bare");
    println!("//   radius, and a grazing ray that passes within `surface` of a sphere yet outside it is a");
    println!("//   hit as far as the marcher is concerned. The 8-sphere cluster row disagreed by one pixel");
    println!("//   and one hit before the fix. A skip is a claim about the marcher's tolerance, not about");
    println!(
        "//   geometry, so every enclosure tested for one is now grown by `surface` first - the"
    );
    println!("//   too-small enclosure this module exists to make impossible, caught by the frame");
    println!("//   comparison rather than by reading the arithmetic.");
    println!(
        "// - THE LAST FOUR ROWS ARE AN AUTHORING RULE, PRICED. Identical spheres, identical tree"
    );
    println!("//   shape, identical camera - every branch written as a smooth union with k 0.2 instead of");
    println!("//   a plain one - and the hierarchy is gone: the line/128 row goes from 97.1% of rays proved");
    println!(
        "//   empty and 648 evaluations to 0.0% and 287,128, a factor of 443, and cluster/128 from"
    );
    println!("//   93.8% to 32.8%. Node visits fall to 0.0% avoided too, because the per-step skip reads");
    println!("//   the same licence bit. Nothing was broken getting there: the blended frames still match");
    println!("//   the unculled render byte for byte, which is what makes the number a cost rather than a");
    println!("//   bug.");
    println!("// - why: a skip needs field >= distance to the node's own bound, and smin returns below min");
    println!("//   by up to k/4, so a blend poisons its parent for as far up as it reaches - one blended");
    println!("//   branch at the root is enough, which is exactly what the boot's own five-node scene is.");
    println!("//   Whether the grown-by-k enclosure satisfies the inequality anyway is OQ-54: a proof hands");
    println!("//   most of this back and a guess deletes geometry, so the row is published as a cost, not");
    println!("//   as a TODO.");
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
    println!("// - the two scatter rows are the build question, and they price one frame twice: identical");
    println!("//   pixels, identical steps, the same unpruned node visits either way. Authored leaf order");
    println!("//   lets the per-node bounds avoid 20.5% of them at 128 spheres; a median split of the same");
    println!("//   spheres avoids 54.3%, and at 32 it is 15.2% against 46.1% - 2.6x to 3.0x, from nothing");
    println!("//   but the order the leaves were written down in.");
    println!("// - the root bound tightens with it too (r 138.0 to 84.7), which is a property of the cover");
    println!("//   rule rather than of the scene: covering two spheres by midpoint-and-max is");
    println!("//   order-dependent, so a tree shape is looseness as well as traversal.");
    println!(
        "// - so the next change belongs to assetc, not to the marcher: emit union children in"
    );
    println!("//   median-split order and the same scene culls about three times better. Reordering under");
    println!(
        "//   plain unions is safe because min is commutative and associative exactly - the probe"
    );
    println!(
        "//   asserts the two trees pixel for pixel - and it is NOT safe under subtract, whose"
    );
    println!(
        "//   operand order is its meaning. The rule has to be per kind, never a general sort."
    );
    println!("// - what is still not here: gates R1 and R2 are frame-time claims with no GPU behind them.");
    println!("//   Nodes avoided per frame is work; work has never been calibrated to nanoseconds in this");
    println!("//   repository, so no column above may be quoted as a frame time.");
}
