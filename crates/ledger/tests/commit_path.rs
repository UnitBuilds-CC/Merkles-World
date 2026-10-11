//! The commit path has to reproduce the frozen vectors from **records**, not from hand-fed leaves.
//!
//! That is the whole point of this file: `root_vectors.rs` pins what the shape digests, and if the
//! pipeline that turns a player's edit into those records cannot reach the same epoch root, then the
//! published vectors describe a tree nobody builds. Everything below is measured against
//! [docs/03 § The published vectors](../../../docs/03-state-ledger.md), whose literals are copied
//! here rather than re-derived, so a change in either place fails loudly.

use contracts::coords::{LocalOffset, RegionKey};
use contracts::delta::{DeltaRecord, RegionSlot};
use ledger::commit::{
    commit_epoch, resolve_last_write_wins, CommitRequest, CommitScratch, RegionEdits,
};
use ledger::{event_leaf, genesis_root, EmptyLadder, Leaf};

const SEED: u64 = 0x5EED_0000_0000_0001;
const SLOTS: [RegionKey; 2] = [
    RegionKey { x: 0, y: 0, z: 0 },
    RegionKey { x: -1, y: 2, z: 0 },
];

const SLOT_TABLE: &str = "1aa6a154af1de795cafe245752d51df583008abd84ccec3976547a2477f069ab";
const REGION_0: &str = "97627e3290a2b4f34e708359af9235d6e2d6d417e1491380d37e58178f9bdcbc";
const REGIONS: &str = "0f70e649b228e5d132332b51cfff3aa11badb2199e2cd3980eb4bf16cb6d9a0f";
const EVENTS_ONE: &str = "ef8d20bb6bf31520bc8188ef8f9b47926536e3edd446999070c091d822fb2795";
const EPOCH_0: &str = "91b97a52fc5137b50ec64550a94220daab0ffe05575bac6dbfda825a9947cc61";

fn hex(root: &[u8; 32]) -> String {
    root.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn edit(x: u16, y: u16, z: u16, payload: u16) -> DeltaRecord {
    DeltaRecord::new(RegionSlot(0), LocalOffset { x, y, z }, payload)
}

/// The published scenario's three edits, in the arrival order the example uses — shuffled on purpose,
/// because the commit path is what has to put them in order.
fn published_records() -> [DeltaRecord; 3] {
    [
        edit(2048, 0, 4095, 0x7),
        edit(0, 0, 0, 0x1),
        edit(4095, 4095, 4095, 0x0ABC),
    ]
}

fn published_events() -> [[u8; 32]; 1] {
    [event_leaf(1, &[0x01, 0x02])]
}

#[test]
fn the_commit_path_reproduces_the_frozen_vectors() {
    let ladder = EmptyLadder::compute();
    let records = published_records();
    let events = published_events();

    let mut slot_scratch = [[0u8; 32]; 2];
    let mut region_scratch = [[0u8; 32]; 2];
    let mut leaf_scratch = [Leaf {
        x: 0,
        y: 0,
        z: 0,
        payload: 0,
    }; 3];

    let commit = commit_epoch(
        &ladder,
        CommitRequest {
            region_slots: &SLOTS,
            edits: &[RegionEdits {
                slot: 0,
                records: &records,
            }],
            event_leaves: &events,
            base_genesis_root: genesis_root(SEED, 0, 0),
            epoch_index: 0,
        },
        &mut CommitScratch {
            slot_rows: &mut slot_scratch,
            region_leaves: &mut region_scratch,
            leaf_space: &mut leaf_scratch,
        },
    )
    .expect("scratch sized for the published scenario");

    assert_eq!(hex(&commit.slot_table), SLOT_TABLE);
    assert_eq!(hex(&commit.regions), REGIONS);
    assert_eq!(hex(&commit.events), EVENTS_ONE);
    assert_eq!(hex(&commit.epoch), EPOCH_0);

    // And the region it built is the published region root, so the vector is reachable through the
    // pipeline rather than only beside it.
    let mut standalone = [Leaf {
        x: 0,
        y: 0,
        z: 0,
        payload: 0,
    }; 3];
    let leaves = resolve_last_write_wins(&records, &mut standalone).expect("three distinct voxels");
    assert_eq!(leaves.len(), 3);
    // The region the commit hashed is the published one, reached through records rather than beside
    // them.
    let mut region = [Leaf {
        x: 0,
        y: 0,
        z: 0,
        payload: 0,
    }; 3];
    let built = ledger::commit::region_root_for(&records, &mut region, &ladder).expect("fits");
    assert_eq!(hex(&built), REGION_0);
}

/// The root is **not** arrival-order-independent when two records touch the same voxel: the later
/// payload wins, and reversing the two reverses which one survives (docs/03 § Epoch roots — the
/// tie-break itself is OQ-46's, and this path takes append order as the causal order).
#[test]
fn last_write_wins_takes_the_later_payload_and_order_matters_there() {
    let ladder = EmptyLadder::compute();
    let a_then_b = [edit(7, 0, 0, 0x1), edit(7, 0, 0, 0x2)];
    let b_then_a = [edit(7, 0, 0, 0x2), edit(7, 0, 0, 0x1)];

    let mut scratch = [Leaf {
        x: 0,
        y: 0,
        z: 0,
        payload: 0,
    }; 4];
    let resolved = resolve_last_write_wins(&a_then_b, &mut scratch).expect("one voxel");
    assert_eq!(resolved.len(), 1, "two records, one position, one leaf");
    assert_eq!(resolved[0].payload, 0x2, "the later record wins");

    let mut root_a = [Leaf {
        x: 0,
        y: 0,
        z: 0,
        payload: 0,
    }; 4];
    let first = ledger::commit::region_root_for(&a_then_b, &mut root_a, &ladder).expect("fits");
    let mut root_b = [Leaf {
        x: 0,
        y: 0,
        z: 0,
        payload: 0,
    }; 4];
    let second = ledger::commit::region_root_for(&b_then_a, &mut root_b, &ladder).expect("fits");

    assert_ne!(
        first, second,
        "swapping two writes to one voxel changes the root"
    );
    assert_eq!(hex(&first), hex(&voxel_leaf_in_tree(&ladder, 0x2)));
}

/// A region's root for a single voxel holding `payload`, rebuilt the way the pipeline would.
fn voxel_leaf_in_tree(ladder: &EmptyLadder, payload: u16) -> [u8; 32] {
    let mut leaves = [Leaf {
        x: 7,
        y: 0,
        z: 0,
        payload,
    }];
    ledger::region_root(&mut leaves, ladder)
}

/// Distinct voxels are order-independent, at the epoch level this time: the same three edits in the
/// opposite arrival order commit to the same epoch root.
#[test]
fn distinct_voxels_commit_identically_whatever_the_arrival_order() {
    let ladder = EmptyLadder::compute();
    let forwards = published_records();
    let backwards = [
        edit(4095, 4095, 4095, 0x0ABC),
        edit(0, 0, 0, 0x1),
        edit(2048, 0, 4095, 0x7),
    ];
    let events = published_events();

    let commit_of = |records: &[DeltaRecord; 3]| {
        let mut slot_scratch = [[0u8; 32]; 2];
        let mut region_scratch = [[0u8; 32]; 2];
        let mut leaf_scratch = [Leaf {
            x: 0,
            y: 0,
            z: 0,
            payload: 0,
        }; 3];
        commit_epoch(
            &ladder,
            CommitRequest {
                region_slots: &SLOTS,
                edits: &[RegionEdits { slot: 0, records }],
                event_leaves: &events,
                base_genesis_root: genesis_root(SEED, 0, 0),
                epoch_index: 0,
            },
            &mut CommitScratch {
                slot_rows: &mut slot_scratch,
                region_leaves: &mut region_scratch,
                leaf_space: &mut leaf_scratch,
            },
        )
        .expect("scratch sized for the scenario")
    };

    assert_eq!(commit_of(&forwards).epoch, commit_of(&backwards).epoch);
    assert_eq!(hex(&commit_of(&forwards).epoch), EPOCH_0);
}

/// A slot the epoch touched but did not edit commits to the empty ladder's top, and stating that
/// explicitly in `edits` must not change anything: absent row and empty row are the same world.
#[test]
fn an_untouched_slot_and_an_empty_edit_row_are_the_same_world() {
    let ladder = EmptyLadder::compute();
    let records = [edit(0, 0, 0, 0x1)];
    let events = published_events();

    let mut without_row_slot_scratch = [[0u8; 32]; 2];
    let mut without_row_region_scratch = [[0u8; 32]; 2];
    let mut without_row_leaf_scratch = [Leaf {
        x: 0,
        y: 0,
        z: 0,
        payload: 0,
    }; 4];
    let without = commit_epoch(
        &ladder,
        CommitRequest {
            region_slots: &SLOTS,
            edits: &[RegionEdits {
                slot: 0,
                records: &records,
            }],
            event_leaves: &events,
            base_genesis_root: genesis_root(SEED, 0, 0),
            epoch_index: 0,
        },
        &mut CommitScratch {
            slot_rows: &mut without_row_slot_scratch,
            region_leaves: &mut without_row_region_scratch,
            leaf_space: &mut without_row_leaf_scratch,
        },
    )
    .expect("fits");

    let empty: [DeltaRecord; 0] = [];
    let mut with_row_slot_scratch = [[0u8; 32]; 2];
    let mut with_row_region_scratch = [[0u8; 32]; 2];
    let mut with_row_leaf_scratch = [Leaf {
        x: 0,
        y: 0,
        z: 0,
        payload: 0,
    }; 4];
    let with = commit_epoch(
        &ladder,
        CommitRequest {
            region_slots: &SLOTS,
            edits: &[
                RegionEdits {
                    slot: 0,
                    records: &records,
                },
                RegionEdits {
                    slot: 1,
                    records: &empty,
                },
            ],
            event_leaves: &events,
            base_genesis_root: genesis_root(SEED, 0, 0),
            epoch_index: 0,
        },
        &mut CommitScratch {
            slot_rows: &mut with_row_slot_scratch,
            region_leaves: &mut with_row_region_scratch,
            leaf_space: &mut with_row_leaf_scratch,
        },
    )
    .expect("fits");

    assert_eq!(without.epoch, with.epoch);
    assert_eq!(without.regions, with.regions);
}

/// Short scratch is a refusal, never a truncated root: a digest over half a slot table would be
/// believed by a peer and is the worst possible outcome of a budgeting mistake.
#[test]
fn short_scratch_refuses_rather_than_hashing_half_a_table() {
    let ladder = EmptyLadder::compute();
    let records = published_records();
    let events = published_events();

    let mut slot_scratch = [[0u8; 32]; 1];
    let mut region_scratch = [[0u8; 32]; 2];
    let mut leaf_scratch = [Leaf {
        x: 0,
        y: 0,
        z: 0,
        payload: 0,
    }; 3];
    assert!(
        commit_epoch(
            &ladder,
            CommitRequest {
                region_slots: &SLOTS,
                edits: &[RegionEdits {
                    slot: 0,
                    records: &records
                }],
                event_leaves: &events,
                base_genesis_root: genesis_root(SEED, 0, 0),
                epoch_index: 0,
            },
            &mut CommitScratch {
                slot_rows: &mut slot_scratch,
                region_leaves: &mut region_scratch,
                leaf_space: &mut leaf_scratch,
            },
        )
        .is_none(),
        "two slots, one row of scratch"
    );

    // The leaf scratch is the busiest region's distinct-voxel count, and it is checked too.
    let mut slot_scratch = [[0u8; 32]; 2];
    let mut region_scratch = [[0u8; 32]; 2];
    let mut too_small = [Leaf {
        x: 0,
        y: 0,
        z: 0,
        payload: 0,
    }; 2];
    assert!(
        commit_epoch(
            &ladder,
            CommitRequest {
                region_slots: &SLOTS,
                edits: &[RegionEdits {
                    slot: 0,
                    records: &records
                }],
                event_leaves: &events,
                base_genesis_root: genesis_root(SEED, 0, 0),
                epoch_index: 0,
            },
            &mut CommitScratch {
                slot_rows: &mut slot_scratch,
                region_leaves: &mut region_scratch,
                leaf_space: &mut too_small,
            },
        )
        .is_none(),
        "three voxels, two slots of leaf space"
    );
}
