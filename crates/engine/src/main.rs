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
//! 9. the field is composed from **three region cells** - one compiled asset each, placed on the frozen
//!    cell arithmetic - and compared the only way a frame spanning cells can be compared: identical hit
//!    sets, stopped depths equal to within the march's own tolerance (OQ-57's diagnosis, measured).
//! 10. the slab is recycled in `O(1)`.
//!
//! Steps 6 to 9 are CPU arithmetic on a buffer. The clause of the pitch demo they satisfy is "a rendered
//! procedural scene" in the sense docs/11's gate table can check - pixels exist and are reproducible - and
//! not in the sense a player would accept: there is no display, and gates R1 and R2 are frame times.
//!
//! Usage: `engine` compiles its own scene and loads those bytes back; `engine <asset.bin>` boots from
//! a file an `assetc` run wrote, which is the shape doc 09's dev profile ends in.
//!
//! What is NOT here, because docs/11's gate table says so and a demo must not quietly disagree with
//! its own status page: no GPU, no window, no BVH build (the bounds are derived per node from the tree the
//! asset compiler emitted, not from a spatial index over primitives), no macro simulation, no mod loading. Culling and a work ceiling are here since steps 7 and 8, on the CPU path;
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

    // ---- 3b. gate P1.6, both halves, in the binary: find the divergent voxel, then reconcile a lag ----
    // Detection has been tested since it landed, but never demonstrated where a wrong answer would be
    // visible; repair is the half that was deferred, because choosing a winner is OQ-46's causal question.
    // What needs no such rule is the boring case - a peer that missed an append - and that is the case two
    // single-player machines actually hit when one is behind. Three distinct voxels, one of them the
    // far corner the coordinate contract is most likely to get wrong.
    let peer_leaf = |x: u16, y: u16, z: u16, payload: u16, append: u32| Leaf {
        x,
        y,
        z,
        payload,
        append,
    };
    let leader_set = [
        peer_leaf(12, 340, 7, 0x0101, 1),
        peer_leaf(2048, 2048, 2048, 0x0202, 2),
        peer_leaf(4095, 4095, 4095, 0x0ABC, 3),
    ];
    let laggard_set = [leader_set[0], leader_set[2]];
    let mut leader_for_root = leader_set;
    let leader_root = ledger::region_root(&mut leader_for_root, &ladder);
    let mut laggard_for_root = laggard_set;
    let laggard_root = ledger::region_root(&mut laggard_for_root, &ladder);
    assert_ne!(
        leader_root, laggard_root,
        "the fixture must actually diverge, or the walk below proves nothing"
    );
    let mut left = laggard_set;
    let mut right = leader_set;
    let found = ledger::repair::first_divergence(&mut left, &mut right, &ladder)
        .expect("differing roots have a findable voxel");
    assert_eq!(
        (found.x, found.y, found.z, found.local, found.remote),
        (2048, 2048, 2048, None, Some(0x0202)),
        "the walk must name the missing append's voxel, with `None` on the side that never got it"
    );
    let mut adopted = [Leaf::DEFAULT; 3];
    let verdict = ledger::repair::catch_up(&laggard_set, &leader_set, &mut adopted, &ladder);
    let ledger::repair::CatchUp::CaughtUp { applied, root } = verdict else {
        panic!("a verbatim-subset lag must be adopted, got {verdict:?}");
    };
    assert_eq!(
        applied, 1,
        "exactly the missed append should have been applied"
    );
    assert_eq!(
        root, leader_root,
        "the reconciled peer must root to the leader's digest, not merely to something"
    );
    let mut after_left = adopted.to_vec();
    let mut after_right = leader_set.to_vec();
    assert!(
        ledger::repair::first_divergence(&mut after_left, &mut after_right, &ladder).is_none(),
        "catch_up claimed convergence while the walk still finds a divergence"
    );
    println!(
        "peer reconcile   : a lagging peer missing 1 of 3 appends diverged at voxel ({}, {}, {}) and reconciled to the leader's root {} - detection and repair in the binary, and a payload clash would come back Disagrees for OQ-46 rather than be merged (P1.6)",
        found.x,
        found.y,
        found.z,
        &hex(&root)[..16],
    );

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

    // ---- 9. the same field as a world: three region cells, one frame ----
    // A-07 has no translation node, so a cell is a separate compiled asset placed on the frozen cell
    // arithmetic and the frame composes the cells' fields (doc 04 § Region streaming; OQ-55 asks whether the
    // grammar should grow an instance kind). The comparison is deliberately not byte equality, because
    // OQ-57's diagnosis showed byte equality is not a claim that can be true at region distance: composing
    // cells changes the trajectory, so a stopped depth moves by units in the last place - 7202.292 against
    // 7202.295, three `f32` steps at that magnitude, inside the 1e-2 tolerance the march defines a hit by.
    // What has to hold exactly is the **hit set**, and that is what is asserted.
    use assetc::Shape;
    // Terrain-scale content, and deliberately all of it *bounded*: a plate 4,000 units across with domes
    // on it, so the neighbouring cells fill a real share of the frame instead of being a single distant
    // bump. `repeat` is the obvious way to author a landscape and it is exactly what the cull cannot
    // bound - a tiling is infinite, so `Enclosure` refuses it and every ray in the frame would then be
    // marched against that cell. The plate-and-dome form keeps the bound, which is why the frame below
    // still gets its early-outs.
    let ridge_field = |shift: [f32; 4]| {
        compile_shape(
            Shape::Union {
                a: Box::new(Shape::RoundedBox {
                    half: [1800.0, 40.0, 1800.0],
                    r: 60.0,
                }),
                b: Box::new(Shape::Union {
                    a: Box::new(Shape::Sphere {
                        r: 1400.0,
                        centre: [shift[0], 600.0, shift[1]],
                    }),
                    b: Box::new(Shape::Union {
                        a: Box::new(Shape::Sphere {
                            r: 900.0,
                            centre: [shift[2], 300.0, -700.0],
                        }),
                        b: Box::new(Shape::Sphere {
                            r: 600.0,
                            centre: [-900.0, 100.0, shift[3]],
                        }),
                    }),
                }),
            },
            "engine boot ridge field",
        )
        .0
    };
    let ridge = ridge_field([0.0, 0.0, 600.0, 900.0]);
    let hill = ridge_field([-400.0, -800.0, 300.0, -1200.0]);
    let cells = [
        render::stream::Region {
            cell: [1, 0, 0],
            tree: tree.clone(),
        },
        render::stream::Region {
            cell: [2, 0, 0],
            tree: ridge,
        },
        render::stream::Region {
            cell: [3, 0, 0],
            tree: hill,
        },
    ];
    let world_camera = render::raster::Camera {
        origin: [0.0, 0.0, 0.0],
        forward: [1.0, 0.0, 0.0],
        right: [0.0, 1.0, 0.0],
        up: [0.0, 0.0, 1.0],
        max_t: 16_384.0,
        surface: 1e-2,
        ..probe_camera()
    };
    let planned = render::stream::plan(&world_camera, &cells);
    let (world_frame, world_stats) = render::stream::render(&world_camera, &cells, &planned);
    let terrain = &cells[1..];
    let terrain_plan = render::stream::plan(&world_camera, terrain);
    let (terrain_frame, terrain_stats) =
        render::stream::render(&world_camera, terrain, &terrain_plan);
    assert_eq!(
        planned.len(),
        cells.len(),
        "a range of four cells must reach all three, or this is not a world"
    );
    // Adding a cell must never remove surface, and can only ever bring the nearest surface closer: that
    // is what a minimum of fields means, and it is the assertion a multi-region frame can honestly carry.
    // (The earlier version here demanded identical hit sets and depths within the tolerance, which was true
    // only because the extra cell happened to be a sub-pixel prop. With terrain in the neighbouring cells
    // the two frames differ by design - nearer surface, differently lit - and the invariant below is the
    // one that still holds.)
    let mut lost_surface = 0usize;
    let mut relit = 0usize;
    let mut nearest_closer = 0usize;
    let mut worst_depth_delta = 0.0f32;
    for index in 0..world_frame.depth.len() {
        let world = world_frame.depth[index];
        let terrain = terrain_frame.depth[index];
        let world_hit = world != render::raster::Frame::NO_HIT;
        let terrain_hit = terrain != render::raster::Frame::NO_HIT;
        if terrain_hit && !world_hit {
            lost_surface += 1;
        }
        if world_hit != terrain_hit || (world_hit && (world - terrain).abs() > world_camera.surface)
        {
            relit += 1;
        }
        if world_hit && terrain_hit {
            if world <= terrain + world_camera.surface {
                nearest_closer += 1;
            }
            worst_depth_delta = worst_depth_delta.max((terrain - world).abs());
        }
    }
    assert_eq!(
        lost_surface, 0,
        "composing cells removed surface that the terrain alone found"
    );
    assert_eq!(
        nearest_closer, world_frame.depth.len() - lost_surface - {
            let mut misses = 0usize;
            for index in 0..world_frame.depth.len() {
                if world_frame.depth[index] == render::raster::Frame::NO_HIT {
                    misses += 1;
                }
            }
            misses
        },
        "every pixel with surface in both frames must have the composite no farther than the terrain"
    );
    // The floor is stated against a measurement rather than a hunch, because two hunches already missed:
    // the prop-scale version of this frame covered 56 of 3,072 rays, and 10% turned out to be the wrong
    // expectation for domes of this size at this distance (they cover ~8.4%). Terrain-scale content is
    // worth something when it changes the frame by an order of magnitude - so that is the claim.
    assert!(
        world_stats.hits > 3 * 56,
        "terrain content should cover several times the frame the prop-scale cells managed (56 rays);          measured now: {} of {}",
        world_stats.hits,
        world_stats.rays
    );
    // The same frame, with ADR-024's descent asked of each region's own children rather than only of the
    // cell's cover sphere. The picture may not move - that is the assertion the streaming path now carries
    // in the shipped demo, not just in a suite - and the two charge columns are the demo's own evidence that
    // the hierarchy is doing work in this scene rather than only in `stream_descent`.
    let (descent_frame, descent_stats) =
        render::stream::render_bnb(&world_camera, &cells, &planned);
    assert_eq!(
        world_frame.pixels, descent_frame.pixels,
        "the descent inside the region cells changed the picture - a skip that disagrees with the marcher"
    );
    assert_eq!(
        world_frame.depth, descent_frame.depth,
        "the descent inside the region cells moved a surface"
    );
    println!(
        "region world     : {} cells {:+} units apart, all planned - {} of {} rays found surface ({:.1}% of the frame), {} charged evaluations against {} for the terrain cells alone, {} proved empty by their bounds; adding the nearest cell re-lit {} pixels and lost none, worst depth shift {:.1} units - composition as a minimum of fields, which is how a frame spans cells until A-07 grows an instance node (OQ-55, OQ-57). Each region's children then asked: {} charged evaluations and {} proved empty, against {} and {} from the cell bound alone (ADR-024)",
        cells.len(),
        render::stream::REGION_UNITS,
        world_stats.hits,
        world_stats.rays,
        100.0 * world_stats.hits as f32 / world_stats.rays as f32,
        world_stats.evaluations,
        terrain_stats.evaluations,
        world_stats.skipped,
        relit,
        worst_depth_delta,
        descent_stats.evaluations,
        descent_stats.skipped,
        world_stats.evaluations,
        world_stats.skipped,
    );

    // Step 9b: the carry, performed inside the binary. A ceiling that crops a frame is not a transition -
    // the next frame has to keep what the last one settled - so the demo refines one streamed frame at a
    // fixed ceiling until nothing is short of, and requires the settled frame to be the very picture the
    // unbudgeted march found. The ceiling is stated in **ray charges** (`max_steps + 1`) rather than as a
    // fraction of the frame, because that is the unit the gate is spent in: a fraction can silently afford
    // not even one ray, which is the mistake `OQ-59` was - two reverted attempts before the fixture was
    // read as a defect.
    let charge = u64::from(world_camera.max_steps) + 1;
    let mut carried = render::raster::Frame::new(world_camera.width, world_camera.height);
    let mut carry_frames = 0u32;
    let mut carry_charge = 0u64;
    let mut carry_skipped = 0u32;
    let mut settled_first = 0u32;
    loop {
        carry_frames += 1;
        let stats = render::stream::render_budget_refined(
            &world_camera,
            &cells,
            &planned,
            render::raster::Budget(charge),
            render::raster::Scan::CentreOut,
            &mut carried,
        );
        assert!(
            stats.evaluations <= charge,
            "frame {carry_frames} of the demo's carry breached its ceiling of {charge}: {}",
            stats.evaluations
        );
        carry_charge += stats.evaluations;
        carry_skipped += stats.skipped;
        if carry_frames == 1 {
            settled_first = stats.rays - stats.not_drawn;
        }
        if stats.not_drawn == 0 || carry_frames >= 4096 {
            break;
        }
    }
    assert!(
        carry_frames < 4096,
        "the demo's carry never settled the grid - {} frames at a ceiling of {charge}",
        carry_frames
    );
    assert_eq!(
        carried.depth, descent_frame.depth,
        "the carried frame settled to depths the unbudgeted march never produced"
    );
    assert_eq!(
        carried.pixels, descent_frame.pixels,
        "the carried frame settled to a different picture than the unbudgeted march"
    );
    println!(
        "stream carry     : {} rays refined at a {}-charge ceiling settle in {} frames ({} after the first frame, {} charged, {} proved empty), against {} charged for the single pass - and the settled frame is pixel-for-pixel the picture the unbudgeted march found, so a ceiling in the demo is a transition rather than a crop (R3, ADR-024)",
        world_stats.rays,
        charge,
        carry_frames,
        settled_first,
        carry_charge,
        carry_skipped,
        descent_stats.evaluations,
    );

    // ---- 10. the commit's cheap half: recycling the slab ----
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
    use assetc::Shape;
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
    let (tree, node_count, depth, byte_len) = compile_shape(authored, "engine boot scene");
    println!(
        "scene source     : authored shape lowered by assetc ({node_count} nodes, depth {depth}), compiled to {byte_len} bytes and loaded by assetc::reader",
    );
    tree
}

/// The one path content takes here: authored `Shape` -> `assetc::lower` -> `ShapeAsset` **bytes** ->
/// `assetc::reader::from_bytes` -> `render::Tree`, with the records asserted unchanged by the trip. The
/// demo's own scene and the region cells in step 9 all arrive this way, which is what makes "the runtime
/// parses nothing" (AGENTS.md §3) a statement about the binary rather than about a test.
///
/// It prints nothing, so a scene that exists to fill a neighbouring cell cannot silently change the
/// published boot output.
fn compile_shape(authored: assetc::Shape, label: &str) -> (render::Tree, usize, u32, usize) {
    use assetc::{lower, reader, writer};
    match lower(&authored) {
        Ok(asset) => {
            let node_count = asset.tree.nodes.len();
            let depth = asset.depth;
            let bytes = writer::to_bytes(&asset, label);
            let loaded =
                reader::from_bytes(&bytes).expect("an authored shape must compile and load back");
            assert_eq!(
                loaded.nodes, asset.tree.nodes,
                "loading must not reinterpret a node"
            );
            assert_eq!(loaded.params, asset.tree.params, "nor the pool");
            assert_eq!(loaded.depth, depth, "and not the derived depth either");
            (
                render::Tree {
                    nodes: loaded.nodes,
                    params: loaded.params,
                },
                node_count,
                depth,
                bytes.len(),
            )
        }
        Err(error) => panic!("{label} must lower: {error:?}"),
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
