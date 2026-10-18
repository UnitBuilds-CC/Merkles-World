//! The price of a device-side ray cull, computed before the arm exists.
//!
//! `cargo run --release -p gpu --example r1_cull_price`
//!
//! Task #69 is the lever docs/04 § Culling says R1's remaining gap needs: on R1's own 1080p grid 85.4% of the
//! rays find no surface (303,427 of 2,073,600 at the boot's 96-step law), and every one of them is currently
//! marched to exhaustion. Before writing an arm into `march_rays_from_camera` this file asks the question #64's
//! lesson says to ask first - *what would the arm actually refuse?* - and answers it with the enclosures
//! `render::cull` already derives, on the CPU, with no device and no timing.
//!
//! Four rows, in the order a designer would reach for them:
//!
//! 1. **root bound, per ray** - one sphere test against the scene's own covering sphere. The cheapest possible
//!    arm: four words already resident in the pool tail this march binds, one `length`, one compare.
//! 2. **licensed descent, per ray** - `SceneBounds::may_reach_subtree` from the root, which is the CPU's own law
//!    walked down the tree, so a ray is refused only where every node on the path licenses it.
//! 3. the same descent with a **bare radius** (margin 0), and
//! 4. the same descent **shrunk by a world unit**.
//!
//! Rows 2 to 4 are one function at three margins, because `may_reach_subtree` takes the margin as a parameter.
//! That is what turns the pair into an instrument: `cull.rs`'s own first bug was a bare radius where a grown one
//! belongs - a ray passing within `surface` of a sphere but outside it is a hit as far as the marcher is
//! concerned - so a refusal count that did not rise as the bound lies more would not be measuring the bound.
//!
//! The rows are *shares of rays*. Nothing here is a duration, so nothing here gates anything (ADR-017). It runs
//! on a machine with no adapter, which is the point: the design question is answerable without the GTX 1070, so
//! the GTX 1070 is only ever asked the question that needs it.

use render::cull::{self, Enclosure};
use render::{Tree, Vec3, ROOT};

/// The boot's law, taken from `r1_specialized_frame`'s product rows rather than restated: a different surface
/// epsilon and max_t from the fixture's, because the demo content is the content R1 is spent on.
const PRODUCT_HALF_HEIGHT: f32 = 0.5;
const PRODUCT_MAX_T: f32 = 40.0;
const PRODUCT_SURFACE: f32 = 1.0e-3;
const R1_W: u32 = 1920;
const R1_H: u32 = 1080;
/// The 1080p surface count `r1_specialized_frame` measured on the GTX 1070 for this scene at the 96-step law.
/// Quoted as the ceiling a ray-level cull cannot beat, not as anything this file computes.
const SURFACES_ON_DEVICE: usize = 303_427;

fn product_origin() -> Vec3 {
    [0.0f32, 0.6, -4.0]
}

fn product_forward() -> [f32; 3] {
    let v = [0.0f32, -0.1, 1.0];
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}

/// `r1_specialized_frame`'s `product_ray`, in the same order, so a ray counted here is the ray the device
/// marches: the aspect, the row flip, the basis combination and the divide-by-length are all `render::raster`'s.
fn product_ray(px: u32, py: u32, width: u32, height: u32) -> (Vec3, Vec3) {
    let aspect = width as f32 / height as f32;
    let u = (px as f32 + 0.5) / width as f32;
    let v = (py as f32 + 0.5) / height as f32;
    let ndc_x = (2.0 * u - 1.0) * PRODUCT_HALF_HEIGHT * aspect;
    let ndc_y = (1.0 - 2.0 * v) * PRODUCT_HALF_HEIGHT;
    let forward = product_forward();
    let span = [forward[0] + ndc_x, forward[1] + ndc_y, forward[2]];
    let len = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
    (
        product_origin(),
        [span[0] / len, span[1] / len, span[2] / len],
    )
}

/// The demo's content, compiled and loaded exactly the way the shipped pipeline sees it.
fn product_scene() -> Tree {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../assetc/tests/fixtures/boot_scene.ron");
    let input = std::fs::canonicalize(&raw)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", raw.display()));
    let output =
        std::env::temp_dir().join(format!("merkles_cull_price_{}.bin", std::process::id()));
    let mut watch = assetc::watch::Watch::default();
    match watch.observe(
        input.to_str().expect("the fixture path is utf-8"),
        output.to_str().expect("the temp path is utf-8"),
    ) {
        assetc::watch::Outcome::Rebuilt { .. } => {}
        other => panic!("a first observation of a fresh file must compile, got {other:?}"),
    }
    let bytes = std::fs::read(&output).expect("the compiler renamed its asset into place");
    let loaded = assetc::reader::from_bytes(&bytes)
        .unwrap_or_else(|error| panic!("{} must load: {error:?}", output.display()));
    let _ = std::fs::remove_file(&output);
    let tree = Tree {
        nodes: loaded.nodes,
        params: loaded.params,
    };
    render::validate(&tree).expect("the demo scene must pass the validator the march requires");
    tree
}

/// [`render::cull::SceneBounds::may_reach_subtree`] re-walked against `encoded()`'s four words per node, which
/// is the shape a WGSL arm would have to read, counting the sphere tests it pays. The refusal it reports must
/// match the library's for every ray or the count is describing a different law - asserted below, not assumed.
fn counted_descent(
    tree: &Tree,
    rows: &[[f32; 4]],
    origin: Vec3,
    dir: Vec3,
    margin: f32,
) -> (bool, usize, usize) {
    let mut asked = 0usize;
    // Stack of nodes still to be asked, with the deepest point the walk reached.
    let mut open = vec![ROOT as usize];
    let mut deepest = 1usize;
    while let Some(node) = open.pop() {
        deepest = deepest.max(open.len() + 1);
        let row = rows[node];
        asked += 1;
        if !cull::ray_may_hit(
            Enclosure::Bounded {
                centre: [row[0], row[1], row[2]],
                radius: row[3],
            }
            .grown(margin),
            origin,
            dir,
            PRODUCT_MAX_T,
        ) {
            continue;
        }
        let record = tree.nodes[node];
        if record.arity() == 0 {
            // A reachable leaf is a reachable scene: the ray survives the pre-pass.
            return (false, asked, deepest);
        }
        let first = node + 1;
        let second = first + tree.nodes[first].subtree_len() as usize;
        match record.kind() {
            contracts::sdf::NODE_UNION => open.extend([first, second]),
            contracts::sdf::NODE_SUBTRACT | contracts::sdf::NODE_INTERSECT => open.push(first),
            // A kind with no granted rule keeps the node's own answer, exactly as the library does.
            _ => return (false, asked, deepest),
        }
    }
    (open.is_empty(), asked, deepest)
}

fn main() {
    let tree = product_scene();
    let rays = R1_W as usize * R1_H as usize;
    let missed = rays - SURFACES_ON_DEVICE;

    let mut kinds: Vec<(u16, usize)> = Vec::new();
    for record in &tree.nodes {
        let kind = record.kind();
        if let Some(entry) = kinds.iter_mut().find(|(k, _)| *k == kind) {
            entry.1 += 1;
        } else {
            kinds.push((kind, 1));
        }
    }
    kinds.sort_by_key(|(kind, _)| *kind);

    let bounds = cull::scene_bounds(&tree);
    let licensed_rows = bounds.encoded();
    let root_enclosure = cull::scene_enclosure(&tree);
    let licensed = bounds.encoded()[ROOT as usize];
    println!(
        "// scene        : {} nodes, {} params, depth {} - the boot's own compiled asset",
        tree.nodes.len(),
        tree.params.len(),
        render::tree_depth(&tree)
    );
    println!(
        "// kinds        : {}",
        kinds
            .iter()
            .map(|(kind, count)| format!("{kind}:{count}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "// root bound   : {}, encoded[root] [{}, {}, {}, {}]",
        match root_enclosure {
            Enclosure::Bounded { centre, radius } => format!(
                "centre ({}, {}, {}) radius {radius}",
                centre[0], centre[1], centre[2]
            ),
            Enclosure::Unbounded => "UNBOUNDED".to_string(),
        },
        licensed[0],
        licensed[1],
        licensed[2],
        licensed[3],
    );
    println!(
        "// licence      : pruning {}, respects[root] {}",
        bounds.prunes(),
        bounds.respects(ROOT as usize)
    );

    let grown = root_enclosure.grown(PRODUCT_SURFACE);
    let mut root_refused = 0usize;
    let mut margins = [
        ("descent, grown by surface", PRODUCT_SURFACE, 0usize),
        ("descent, bare radius", 0.0f32, 0usize),
        ("descent, shrunk by 1.0", -1.0f32, 0usize),
    ];
    for py in 0..R1_H {
        for px in 0..R1_W {
            let (origin, dir) = product_ray(px, py, R1_W, R1_H);
            if !cull::ray_may_hit(grown, origin, dir, PRODUCT_MAX_T) {
                root_refused += 1;
            }
            for (_, margin, refused) in margins.iter_mut() {
                if !bounds.may_reach_subtree(&tree, ROOT as usize, origin, dir, *margin) {
                    *refused += 1;
                }
            }
        }
    }
    let descent_refused = margins[0].2;

    // The arm's own bill, as an integer rather than a hope: a ray-level pre-pass is paid by EVERY ray, including
    // the 59% it does not refuse, so what decides the lever is tests-per-ray against the walk it removes. This
    // counts them by re-walking the tree with `encoded()`'s rows - the same four words the shader would read -
    // and the twin assert below makes the re-implementation earn its place: it must refuse exactly the rays the
    // library function refuses, or the count belongs to a different law.
    let mut tests = 0usize;
    let mut twin_refused = 0usize;
    let mut deepest = 0usize;
    for py in 0..R1_H {
        for px in 0..R1_W {
            let (origin, dir) = product_ray(px, py, R1_W, R1_H);
            let (refused, asked, depth) =
                counted_descent(&tree, &licensed_rows, origin, dir, PRODUCT_SURFACE);
            tests += asked;
            deepest = deepest.max(depth);
            if refused {
                twin_refused += 1;
            }
        }
    }
    assert_eq!(
        twin_refused, descent_refused,
        "the counting walk refused {twin_refused} rays where `may_reach_subtree` refused {descent_refused} - the \
         per-ray test count below describes the arm only while both refuse the same set"
    );

    println!();
    println!("// arm's bill   : {tests} sphere tests over {rays} rays = {:.2} per ray, deepest walk {} nodes", tests as f64 / rays as f64, deepest);
    println!("// grid         : {R1_W}x{R1_H} = {rays} rays, R1's own resolution and the boot's own camera");
    println!("// device misses: {missed} of {rays} rays found no surface at the 96-step law, the ceiling on any ray-level cull");
    for (name, refused) in [
        ("root bound, per ray", root_refused),
        ("licensed descent", descent_refused),
        ("descent, bare radius", margins[1].2),
        ("descent, shrunk by 1.0", margins[2].2),
    ] {
        println!(
            "// {name:22} : {refused:>9} of {rays} refused = {:.1}% of the grid, {:.1}% of the misses",
            100.0 * refused as f64 / rays as f64,
            100.0 * refused as f64 / missed as f64,
        );
    }

    // Containment: a ray the root sphere refuses is refused by the descent too, because the descent's first
    // test IS the root's own enclosure. A row that broke this would mean the 0% and the 41% were not one law
    // at two depths, which is the whole comparison this file is making.
    assert!(
        root_refused <= descent_refused,
        "the descent refused {descent_refused} rays but the root bound alone refused {root_refused} - the \
         descent's first test is the root's own enclosure, so a larger root count means the two rows are not \
         one law at two depths"
    );
    // Bound sensitivity, in the direction that loses geometry. A bare radius must refuse STRICTLY more rays,
    // and the difference is the number of rays an arm carrying that bound would drop out of the picture.
    assert!(
        margins[1].2 > descent_refused,
        "a bare-radius descent refused {} rays against the grown bound's {descent_refused}: the margin is what \
         the count is sensitive to, and an identical count would mean no ray on this grid grazes a bound - the \
         `cull.rs` bug class this row exists to keep visible",
        margins[1].2
    );
    assert!(
        margins[2].2 > margins[1].2,
        "shrinking every enclosure by a world unit refused {} rays against the bare radius's {} - a bound test \
         that refuses more as it lies more is the only shape this instrument can have",
        margins[2].2,
        margins[1].2
    );
    println!(
        "// falsifiers   : root {root_refused} <= descent {descent_refused} ; a bare radius would wrongly drop \
         {} rays, a bound shrunk by 1.0 unit {} rays",
        margins[1].2 - descent_refused,
        margins[2].2 - descent_refused
    );
}
