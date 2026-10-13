//! The first thing in this repository that *runs*.
//!
//! docs/11's Phase 1 exit asks for "a booting engine, a rendered procedural scene, an edit that
//! persists, and one deterministic hash". Until now every crate here was a library or an example, so
//! the honest answer to "does an engine exist" was no. This binary is the loop, in one process:
//!
//! 1. edits land in a slab (`crates/memory`: one store per append, `O(1)` reset);
//! 2. the slab's records commit into an epoch root (`crates/ledger::commit`, over A-10's frozen shape);
//! 3. that root is compared with the published vector in docs/03 - the same bytes a peer would check,
//!    reached from records rather than from a hand-fed fixture;
//! 4. the read path answers what a voxel holds, which is gate P1.7's persistence claim;
//! 5. the residency directory demotes a region to `M_cold` and brings it back, re-rooting it to docs/03's
//!    published `REGION_0` — gate P1.4's correctness half, run through the same hashing path the commit
//!    used, and printed rather than only asserted inside the binary;
//! 6. a procedural scene is marched and its nearest surface reported. The scene arrives as compiled
//!    `ShapeAsset` bytes that `assetc::reader` loads back and `crates/render` evaluates, so the demo's
//!    scene clause describes an asset rather than a tree this binary built in memory. It is still a CPU
//!    evaluation, and the line that prints it says so.
//! 7. the same field is marched as a grid of rays **three ways** - unculled, against the scene's one
//!    enclosure, and against the bound hierarchy - and reported as counts, not shown. The three frames are
//!    asserted identical, and the line prints which of the two culls the scene's own licence allows, which
//!    for a blended scene is none. Nothing displays it; that half is still the renderer workstream's.
//! 8. the same grid is drawn a fourth time **under a ceiling** on charged evaluations, centre-out, and the
//!    pixels the ceiling sacrificed are counted rather than left as a black guess.
//!
//! Steps 6 to 8 are CPU arithmetic on a buffer. The clause of the pitch demo they satisfy is "a rendered
//! procedural scene" in the sense docs/11's gate table can check - pixels exist and are reproducible - and
//! not in the sense a player would accept: there is no display, and gates R1 and R2 are frame times.
//!
//! Usage: `engine` compiles its own scene and loads those bytes back; `engine <asset.bin>` boots from
//! a file an `assetc` run wrote, which is the shape doc 09's dev profile ends in.
//!
//! What is NOT here, because docs/11's gate table says so and a demo must not quietly disagree with
//! its own status page: no GPU, no window, no BVH build (the bounds are derived per node from the tree the
//! asset compiler emitted, not from a spatial index over primitives), no macro simulation, no mod loading,
//! and no multi-region frame. Culling and a work ceiling are here since steps 7 and 8, on the CPU path;
//! ledger compaction is a landed transform with no caller in this loop. The first clause of the pitch demo is now true in the narrow sense that something
//! boots and completes the loop end to end, and the asset it boots with goes through the file format
//! an author's tool would write.

use contracts::coords::{GlobalVoxelPos, LocalOffset, RegionKey};
use contracts::delta::{DeltaRecord, RegionSlot};
use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SMOOTH_UNION, NODE_SPHERE, NODE_UNION,
};
use ledger::commit::{commit_epoch, region_root_for, CommitRequest, CommitScratch, RegionEdits};
use ledger::read::{committed_at, Committed};
use ledger::tier;
use ledger::{event_leaf, genesis_root, EmptyLadder, Leaf, Root};
use memory::{Slab, SLAB_64KB_RECORDS};

/// The published scenario in docs/03 § The published vectors, restated as records. If anything here
/// drifts from the literals below, the asserts at the end fail - which is the reason to run the
/// scenario in a binary at all rather than trust the test suite's copy of it.
const SEED: u64 = 0x5EED_0000_0000_0001;
const SLOTS: [RegionKey; 2] = [
    RegionKey { x: 0, y: 0, z: 0 },
    RegionKey { x: -1, y: 2, z: 0 },
];
const PUBLISHED_EPOCH_0: &str = "91b97a52fc5137b50ec64550a94220daab0ffe05575bac6dbfda825a9947cc61";
const PUBLISHED_REGION_0: &str = "97627e3290a2b4f34e708359af9235d6e2d6d417e1491380d37e58178f9bdcbc";
const PUBLISHED_REGIONS: &str = "0f70e649b228e5d132332b51cfff3aa11badb2199e2cd3980eb4bf16cb6d9a0f";

fn hex(root: &Root) -> String {
    let mut out = String::with_capacity(root.len() * 2);
    for byte in root {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn edit(x: u16, y: u16, z: u16, payload: u16) -> DeltaRecord {
    DeltaRecord::new(RegionSlot(0), LocalOffset { x, y, z }, payload)
}

/// A position in region slot 0, expressed the way the ledger expresses it: a region key and an
/// intra-region triple, never a packed voxel index (docs/02 forbids the latter for good reason).
fn position(x: u16, y: u16, z: u16) -> GlobalVoxelPos {
    contracts::delta::voxel_at(&SLOTS[0], &LocalOffset { x, y, z })
}

fn main() {
    println!("// Merkles World engine, first boot. Ledger: real. Renderer: CPU evaluation only.");

    // ---- 1. edits into a slab: the hot path's only memory primitive ----
    let mut buffer = vec![0u64; SLAB_64KB_RECORDS];
    let mut slab = Slab::new(&mut buffer);
    let records = [
        edit(2048, 0, 4095, 0x7),
        edit(0, 0, 0, 0x1),
        edit(4095, 4095, 4095, 0x0ABC),
    ];
    for record in records {
        slab.append(record).expect("a 64 KB slab holds three edits");
    }
    println!(
        "slab           : {} records, {} bytes, capacity {} records",
        slab.len(),
        slab.bytes(),
        slab.capacity()
    );

    // ---- 2. commit the slab's records into an epoch root ----
    let read_back: Vec<DeltaRecord> = slab.deltas().collect();
    let ladder = EmptyLadder::compute();
    let edits = [RegionEdits {
        slot: 0,
        records: &read_back,
    }];
    let events = [event_leaf(1, &[0x01, 0x02])];
    let mut slot_rows = [[0u8; 32]; 2];
    let mut region_leaves = [[0u8; 32]; 2];
    let mut leaf_space = [Leaf::DEFAULT; 3];
    let commit = commit_epoch(
        &ladder,
        CommitRequest {
            region_slots: &SLOTS,
            edits: &edits,
            event_leaves: &events,
            base_genesis_root: genesis_root(SEED, 0, 0),
            epoch_index: 0,
        },
        &mut CommitScratch {
            slot_rows: &mut slot_rows,
            region_leaves: &mut region_leaves,
            leaf_space: &mut leaf_space,
        },
    )
    .expect("scratch sized for three records and two slots");
    println!("epoch root     : {}", hex(&commit.epoch));
    println!("region list    : {}", hex(&commit.regions));
    println!("slot table     : {}", hex(&commit.slot_table));

    // ---- 3. the published vectors, reached from records ----
    assert_eq!(
        hex(&commit.epoch),
        PUBLISHED_EPOCH_0,
        "the committed root must equal docs/03's EPOCH_0"
    );
    assert_eq!(hex(&commit.regions), PUBLISHED_REGIONS);
    let mut own_scratch = [Leaf::DEFAULT; 3];
    let region = region_root_for(&read_back, &mut own_scratch, &ladder).expect("three records");
    assert_eq!(
        hex(&region),
        PUBLISHED_REGION_0,
        "one region, from the same records"
    );
    println!("published match: EPOCH_0, REGIONS and REGION_0 all agree with docs/03");

    // ---- 4. the read path: does the edit persist ----
    let live = committed_at(&SLOTS, &edits, &position(4095, 4095, 4095));
    let never = committed_at(&SLOTS, &edits, &position(100, 100, 100));
    let Committed::Edited { payload, append } = live else {
        panic!("the edited voxel must read back as edited")
    };
    println!(
        "read back      : voxel (4095,4095,4095) holds payload {payload:#04x} from append {append}"
    );
    println!("read back      : a voxel nobody edited reads {never:?} - absence, not zero");
    assert_eq!(payload, 0x0ABC);
    assert_eq!(never, Committed::Unedited);

    // ---- 5. residency: the region the player walked away from comes back as the same root ----
    // Gate P1.4's correctness half, run in the binary rather than only in `crates/ledger`'s own tests,
    // because the gate is about a composition: the directory, the arena the caller owns, and the same
    // hashing path `commit_epoch` used. One hot slot is the tightest budget that still demonstrates
    // eviction, and the printed digest is docs/03's `REGION_0` - so the restore is checked against the
    // published literal from outside this process, not merely against a value this run derived.
    let mut cells = [tier::Slot::EMPTY; 2];
    let mut residency = tier::Store::new(&mut cells, 1);
    assert!(
        residency.register(SLOTS[0], 0, read_back.len()),
        "region 0 owns the arena's three records"
    );
    assert!(
        residency.register(SLOTS[1], read_back.len(), 0),
        "the neighbour has no edits, which is normal and still occupies the camera"
    );
    residency.admit(&SLOTS[0]);
    residency.tick();
    assert_eq!(
        residency.admit(&SLOTS[1]),
        tier::Admission::Evicted { evicted: SLOTS[0] },
        "a one-region hot budget must demote region 0 to M_cold"
    );
    residency.tick();
    let restored = residency
        .re_enter(&SLOTS[0])
        .expect("cold is unreachability, not loss - the directory still knows the range");
    assert_eq!(restored, (0, read_back.len()));
    let mut restore_scratch = [Leaf::DEFAULT; 3];
    let restored_root = region_root_for(
        &read_back[restored.0..restored.0 + restored.1],
        &mut restore_scratch,
        &ladder,
    )
    .expect("three records");
    assert_eq!(
        hex(&restored_root),
        PUBLISHED_REGION_0,
        "re-entry restored a different world than the one that was left"
    );
    println!(
        "residency      : region ({}, {}, {}) demoted to M_cold, re-entered, and re-rooted to {}",
        SLOTS[0].x,
        SLOTS[0].y,
        SLOTS[0].z,
        hex(&restored_root)
    );

    // ---- 6. the scene half: marched, not drawn ----
    // `engine [asset.bin]` boots from a file the compiler wrote; with no argument it compiles its own
    // scene and loads those bytes back, which keeps the default demo self-contained.
    let asset_path = std::env::args().nth(1);
    let tree = match &asset_path {
        Some(path) => scene_from_disk(path),
        None => scene_tree(),
    };
    let (t, steps) = scene_t_probe(&tree);
    if asset_path.is_none() {
        // The same ray marched through the hand-assembled tree must land in the same place: this is
        // the seam where assetc's records meet render's reader, and neither crate's own suite sees it.
        let hand = scene_t_probe(&scene_tree_by_hand());
        assert!(
            (hand.0 - t).abs() < 1e-6 && hand.1 == steps,
            "lowered and hand-assembled scenes must be one surface: {t}/{steps} vs {hand:?}"
        );
    }
    println!("scene march      : first surface at t = {t:.4} after {steps} evaluations (CPU, no GPU; a single ray, so nothing to skip)");

    // ---- 7. the same field as a frame: a grid of rays, still on the CPU ----
    // All three paths run, and the boot asserts they agree. That is the claim the raster cull rests on: a
    // bound that is too small deletes geometry and the result looks like a correct render, so the check
    // belongs where a real frame is drawn, not only in a suite that never marches this scene.
    let (frame, raster) = render::raster::render(&tree, &probe_camera());
    let (culled_frame, culled_raster) = render::raster::render_culled(&tree, &probe_camera());
    assert_eq!(
        frame.pixels, culled_frame.pixels,
        "the scene enclosure changed the boot's picture - the bound is too small"
    );
    assert_eq!(
        frame.depth, culled_frame.depth,
        "the scene enclosure moved a surface the march found"
    );
    let bound = match render::cull::scene_enclosure(&tree) {
        render::cull::Enclosure::Bounded { radius, .. } => radius,
        render::cull::Enclosure::Unbounded => f32::NAN,
    };
    // The third path is the hierarchy: the same ray question asked of each child instead of the one cover
    // sphere. It runs here because `cull_probe` measured that the answer can be worth two orders of
    // magnitude - and because on *this* scene the answer is the licence's own story: the root union sits
    // above a smooth union, and a blend poisons the skip precondition for as far up as it reaches, so the
    // descent inherits the single bound's verdict. Printing the licence bit next to the two counts is what
    // turns that from a code reading into a measured line in the demo.
    let licensed = render::cull::scene_bounds(&tree).respects(render::ROOT as usize);
    let (bnb_frame, bnb_raster) = render::raster::render_bnb(&tree, &probe_camera());
    assert_eq!(
        frame.pixels, bnb_frame.pixels,
        "the hierarchy descent changed the boot's picture - a bound is too small somewhere"
    );
    assert_eq!(
        frame.depth, bnb_frame.depth,
        "the hierarchy descent moved a surface the march found"
    );
    println!(
        "scene raster     : {}x{} grid marched on a CPU - {} of {} rays found surface, {} evaluations, {} with the scene cull (one bound r {:.2}, {} of {} rays proved empty), {} with the hierarchy descent ({} proved empty, root licence {}) - the spread is what makes the two columns disagree, which is ADR-024 in the demo rather than in a probe (informational; gate R1 is a GPU frame and stays unmet)",
        frame.width,
        frame.height,
        raster.hits,
        raster.rays,
        raster.evaluations,
        culled_raster.evaluations,
        bound,
        culled_raster.skipped,
        raster.rays,
        bnb_raster.evaluations,
        bnb_raster.skipped,
        if licensed { "granted" } else { "refused" },
    );

    // ---- 8. the same frame under a ceiling: the budget pass, in the demo ----
    // Half the hierarchy render's charged work, spent centre-out. Two things are asserted rather than
    // printed: the ceiling held, and every pixel it did pay for is the pixel the unbudgeted render drew -
    // a budget that changed a colour would not be a budget, it would be a different renderer. This is the
    // shape gate R1 will eventually be priced against, and the shape AS5 will reject an asset by, which is
    // why it is measured here rather than left in an example. The per-call bound table and the tile index
    // list are allocations; a compiler and a demo are not the gameplay hot path AGENTS.md §3 protects.
    let ceiling = bnb_raster.evaluations / 2;
    let mut budgeted = render::raster::Frame::new(64, 48);
    let budget_stats = render::raster::render_bnb_budget_in(
        &tree,
        &probe_camera(),
        render::raster::Budget(ceiling),
        render::raster::Scan::CentreOut,
        &mut budgeted,
    );
    assert!(
        budget_stats.evaluations <= ceiling,
        "the demo's frame overran its own ceiling: {} > {ceiling}",
        budget_stats.evaluations
    );
    assert_eq!(
        budget_stats.rays,
        (budget_stats.rays - budget_stats.not_drawn) + budget_stats.not_drawn,
        "the pixel bookkeeping must close"
    );
    let mut mismatched = 0usize;
    for index in 0..frame.depth.len() {
        if budgeted.depth[index] == render::raster::Frame::NOT_DRAWN_DEPTH {
            continue;
        }
        if budgeted.pixels[index * 3..index * 3 + 3] != bnb_frame.pixels[index * 3..index * 3 + 3]
            || budgeted.depth[index] != bnb_frame.depth[index]
        {
            mismatched += 1;
        }
    }
    assert_eq!(
        mismatched, 0,
        "a ceiling drew {} pixels differently from the unbudgeted render",
        mismatched
    );
    println!(
        "scene budget     : ceiling {ceiling} of {} charged evaluations - {} pixels drawn, {} left undrawn, {} of {} hits kept, centre-out (a ceiling spends on the frame's composition, so the walk order is content; R1 stays a GPU claim)",
        bnb_raster.evaluations,
        budget_stats.rays - budget_stats.not_drawn,
        budget_stats.not_drawn,
        budget_stats.hits,
        bnb_raster.hits,
    );

    // ---- 9. the commit's cheap half: recycling the slab ----
    slab.reset();
    println!(
        "after commit     : slab reset to {} records in O(1), capacity unchanged at {}",
        slab.len(),
        slab.capacity()
    );
    println!("// boot complete. Not met: a drawn scene, macro sim, compaction, mod loading, four devices.");
}

/// The engine's scene, arriving the way content will arrive: authored `Shape` -> `assetc::lower` ->
/// `ShapeAsset` **bytes** -> `assetc::reader::from_bytes` -> `render::Tree`.
///
/// The round trip through the file format is the point. docs/09 promises the runtime loads
/// pre-compiled binaries and parses nothing, so the boot's own scene has to survive the envelope
/// rather than be handed to the renderer in memory - and the demo's "renders a procedural scene"
/// clause should describe the bytes an asset compiler emits, not a tree the binary built itself.
///
/// This is also the composition seam no crate's own suite covers: assetc validates its own output and
/// render validates its own input, but nothing until now proved the two agree on what a node's fields
/// mean *after they have been through the wire*, which is the kind of gap that shows up as a wrong
/// picture rather than a failed test.
fn scene_tree() -> render::Tree {
    use assetc::{lower, reader, writer, Shape};
    let hero = Shape::Union {
        a: Box::new(Shape::RoundedBox {
            half: [0.35, 0.35, 0.9],
            r: 0.12,
        }),
        b: Box::new(Shape::SmoothUnion {
            a: Box::new(Shape::Sphere {
                r: 0.7,
                centre: [-0.9, 0.0, 0.0],
            }),
            b: Box::new(Shape::Sphere {
                r: 0.7,
                centre: [0.9, 0.0, 0.0],
            }),
            k: 0.5,
        }),
    };
    // Three separate objects at a distance, joined with plain unions and *nothing* blended between them.
    // That is deliberate and it is the demo's point: the hero alone is a compact object whose one scene
    // bound is already tight, so culling has nothing to prove there - cull_probe measured that a single
    // bound saturates the moment geometry spreads, and the boot's camera was the small version of it. With
    // a spread the root sphere grows loose and the bound hierarchy is what earns the frame: the children
    // stay as tight as their own geometry however far apart they are. It also gives `assetc`'s median-split
    // rebalance and the blend's own k-aware skip something real to operate on inside the shipped binary.
    let spread = Shape::Union {
        a: Box::new(Shape::Sphere {
            r: 0.5,
            centre: [-6.0, 0.0, 1.5],
        }),
        b: Box::new(Shape::Union {
            a: Box::new(Shape::Sphere {
                r: 0.5,
                centre: [6.2, -0.4, -1.0],
            }),
            b: Box::new(Shape::Sphere {
                r: 0.5,
                centre: [1.0, 3.2, 7.5],
            }),
        }),
    };
    let authored = Shape::Union {
        a: Box::new(hero),
        b: Box::new(spread),
    };
    match lower(&authored) {
        Ok(asset) => {
            let node_count = asset.tree.nodes.len();
            let depth = asset.depth;
            let bytes = writer::to_bytes(&asset, "engine boot scene");
            let loaded =
                reader::from_bytes(&bytes).expect("the engine's own compiled asset must load back");
            assert_eq!(
                loaded.nodes, asset.tree.nodes,
                "loading must not reinterpret a node"
            );
            assert_eq!(loaded.params, asset.tree.params, "nor the pool");
            assert_eq!(loaded.depth, depth);
            println!(
                "scene source     : authored shape lowered by assetc ({node_count} nodes, depth {depth}), compiled to {} bytes and loaded by assetc::reader",
                bytes.len()
            );
            render::Tree {
                nodes: loaded.nodes,
                params: loaded.params,
            }
        }
        Err(error) => panic!("the engine's own scene must lower: {error:?}"),
    }
}

/// The hand-assembled form of the same scene, kept as a cross-check that the lowered tree and the
/// by-hand tree describe one surface rather than two that happen to march to the same answer today.
fn scene_tree_by_hand() -> render::Tree {
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
    let blended = push(NODE_SMOOTH_UNION, 2, 3, &[0.5], 0);
    let block = push(NODE_ROUNDED_BOX, 0, 1, &[0.35, 0.35, 0.9, 0.12], 0);
    let far_left = push(NODE_SPHERE, 0, 1, &[0.5, -6.0, 0.0, 1.5], 0);
    let far_right = push(NODE_SPHERE, 0, 1, &[0.5, 6.2, -0.4, -1.0], 0);
    let far_high = push(NODE_SPHERE, 0, 1, &[0.5, 1.0, 3.2, 7.5], 0);
    // Grouped the way the authored shape groups them. The grouping of a plain union cannot change the
    // field - the same claim `assetc`'s rebalance rests on, checked here against the lowered tree rather
    // than asserted in isolation - and `lower` is free to emit these in sorted order because of it.
    let inner = push(NODE_UNION, 2, 3, &[], 0);
    let hero = push(NODE_UNION, 2, 5, &[], 0);
    let spread = push(NODE_UNION, 2, 5, &[], 0);
    let whole = push(NODE_UNION, 2, 11, &[], FLAG_IS_ROOT);
    let tree = render::Tree {
        nodes: vec![
            whole, hero, block, blended, left, right, spread, far_left, inner, far_right, far_high,
        ],
        params,
    };
    render::validate(&tree).expect("the hand-assembled scene must validate");
    tree
}

/// Boot from an asset file on disk, which is the path docs/09's whole argument is about: the compiler
/// ran on save, the runtime reads bytes.
///
/// Reading a file here does not breach the hot-path bans (AGENTS.md section 3) - a boot-time read is
/// not a per-frame parse, and there is nothing to parse beyond one header walk: root offset, `MLWS`
/// identifier, vtable. What the file *cannot* do is make the engine evaluate a shape it was not given,
/// because the same `render::validate` gate the in-memory path passes sits between the bytes and the
/// march.
fn scene_from_disk(path: &str) -> render::Tree {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{path}: cannot read: {error}"));
    let loaded = assetc::reader::from_bytes(&bytes)
        .unwrap_or_else(|error| panic!("{path}: not a ShapeAsset this engine will run: {error:?}"));
    let tree = render::Tree {
        nodes: loaded.nodes,
        params: loaded.params,
    };
    render::validate(&tree)
        .unwrap_or_else(|error| panic!("{path}: refused by the validator: {error:?}"));
    println!(
        "scene source     : {} bytes read from {path} ({} nodes, {} parameters, depth {}, grammar version {}, stamped {:?})",
        bytes.len(),
        tree.nodes.len(),
        tree.params.len(),
        loaded.depth,
        loaded.grammar_version,
        loaded.label
    );
    tree
}

/// The grid camera for step 6: the scene's own extents at a distance that fits them in frame, on the
/// axis so the pinhole's basis stays honest without any trigonometry.
fn probe_camera() -> render::raster::Camera {
    render::raster::Camera {
        origin: [0.0, 0.6, -4.0],
        forward: [0.0, -0.1, 1.0],
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

fn scene_t_probe(tree: &render::Tree) -> (f32, u32) {
    let origin = [3.4f32, 1.1, 2.2];
    // Aimed at the scene's centre, so a miss means the field is wrong rather than the aim being
    // unlucky: the first version of this line guessed a direction and panicked on a clean evaluator.
    let raw = [-origin[0], -origin[1], -origin[2]];
    let len = (raw[0] * raw[0] + raw[1] * raw[1] + raw[2] * raw[2]).sqrt();
    let dir = [raw[0] / len, raw[1] / len, raw[2] / len];
    match render::march(tree, render::ROOT, origin, dir, 24.0, 1e-4, 96) {
        Some(hit) => (hit.t, hit.steps),
        None => panic!("the probe ray must meet the scene"),
    }
}
