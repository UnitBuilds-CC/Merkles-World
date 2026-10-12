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
//! 5. a procedural scene is marched and its nearest surface reported: the scene half, evaluated on a
//!    CPU, and the line that prints it says so.
//!
//! What is NOT here, because docs/11's gate table says so and a demo must not quietly disagree with
//! its own status page: no GPU, no window, no culling, no BVH, no macro simulation, no compaction, no
//! mod loading. The first clause of the pitch demo is now true in the narrow sense that something
//! boots and completes the loop end to end.

use contracts::coords::{GlobalVoxelPos, LocalOffset, RegionKey};
use contracts::delta::{DeltaRecord, RegionSlot};
use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SMOOTH_UNION, NODE_SPHERE, NODE_UNION,
};
use ledger::commit::{commit_epoch, region_root_for, CommitRequest, CommitScratch, RegionEdits};
use ledger::read::{committed_at, Committed};
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

    // ---- 5. the scene half: marched, not drawn ----
    let (t, steps) = scene_probe();
    println!("scene march      : first surface at t = {t:.4} after {steps} evaluations (CPU, no GPU, no culling)");

    // ---- 6. the commit's cheap half: recycling the slab ----
    slab.reset();
    println!(
        "after commit     : slab reset to {} records in O(1), capacity unchanged at {}",
        slab.len(),
        slab.capacity()
    );
    println!("// boot complete. Not met: a drawn scene, macro sim, compaction, mod loading, four devices.");
}

/// The four-node scene from `crates/render`'s example: two spheres blended with `k = 0.5`, sharply
/// unioned with a rounded box. Built here rather than shared because the engine must prove it can
/// assemble a tree the way `assetc` eventually will.
fn scene_tree() -> render::Tree {
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
    let whole = push(NODE_UNION, 2, 5, &[], FLAG_IS_ROOT);
    let tree = render::Tree {
        nodes: vec![whole, block, blended, left, right],
        params,
    };
    render::validate(&tree).expect("the engine's own scene must validate");
    tree
}

fn scene_probe() -> (f32, u32) {
    let tree = scene_tree();
    let origin = [3.4f32, 1.1, 2.2];
    // Aimed at the scene's centre, so a miss means the field is wrong rather than the aim being
    // unlucky: the first version of this line guessed a direction and panicked on a clean evaluator.
    let raw = [-origin[0], -origin[1], -origin[2]];
    let len = (raw[0] * raw[0] + raw[1] * raw[1] + raw[2] * raw[2]).sqrt();
    let dir = [raw[0] / len, raw[1] / len, raw[2] / len];
    match render::march(&tree, render::ROOT, origin, dir, 24.0, 1e-4, 96) {
        Some(hit) => (hit.t, hit.steps),
        None => panic!("the probe ray must meet the scene"),
    }
}
