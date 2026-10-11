//! "An edit that persists" is a claim about the read side, not the root — so it gets read-side tests.
//!
//! The pitch demo's second clause (docs/11 § Phase 1 exit) is that a player's edit survives into the
//! ledger. `commit_path.rs` already proves the records reach `EPOCH_0`; what could still be false is
//! the *value*: a root can commit faithfully to a payload no reader ever gets back. These tests tie
//! the two together, and they are also the ground OQ-47's visibility gate has to be stated on — you
//! cannot measure when an edit becomes visible before something can say what it commits to.

use contracts::coords::GlobalVoxelPos;
use contracts::coords::{LocalOffset, RegionKey};
use contracts::delta::{voxel_at, DeltaRecord, RegionSlot};
use ledger::commit::{commit_epoch, CommitRequest, CommitScratch, RegionEdits};
use ledger::read::{committed_at, edited_positions, slot_of, Committed};
use ledger::{genesis_root, EmptyLadder, Leaf};

const SEED: u64 = 0x5EED_0000_0000_0001;
const REGION: RegionKey = RegionKey { x: -1, y: 2, z: 0 };
const SLOTS: [RegionKey; 2] = [RegionKey { x: 0, y: 0, z: 0 }, REGION];

/// A position in the negative-axis region: `region_key` / `local_offset` split it with floor
/// semantics, so an edit here is the case a naive `>>` implementation loses.
fn far_position(x: u16, y: u16, z: u16) -> GlobalVoxelPos {
    voxel_at(&REGION, &LocalOffset { x, y, z })
}

fn record(position: &GlobalVoxelPos, payload: u16) -> DeltaRecord {
    DeltaRecord::at_position(RegionSlot(1), position, payload)
}

#[test]
fn an_edit_written_in_this_epoch_reads_back_from_the_ledger() {
    let position = far_position(17, 4095, 0);
    let records = [record(&position, 0x2A)];
    let edits = [RegionEdits {
        slot: 1,
        records: &records,
    }];

    assert_eq!(
        committed_at(&SLOTS, &edits, &position),
        Committed::Edited {
            payload: 0x2A,
            append: 0
        },
        "the whole point: the ledger says what the voxel holds"
    );
    assert_eq!(slot_of(&SLOTS, &position.region_key()), Some(1));
    assert_eq!(
        slot_of(&SLOTS, &far_position(0, 0, 0).region_key()),
        Some(1)
    );
}

/// The read and the root must never disagree about which write is live. Rebuilding the leaf set
/// *from the read side* and hashing it has to reproduce the region root the commit path produced.
#[test]
fn the_read_and_the_root_never_disagree() {
    let ladder = EmptyLadder::compute();
    let positions = [
        far_position(0, 0, 0),
        far_position(17, 4095, 0),
        far_position(4095, 4095, 4095),
    ];
    // Two writes to the middle voxel, in causal order: the second is live for both readers.
    let records = [
        record(&positions[0], 0x1),
        record(&positions[1], 0x2),
        record(&positions[1], 0x9),
        record(&positions[2], 0x4),
    ];
    let edits = [RegionEdits {
        slot: 1,
        records: &records,
    }];

    assert_eq!(
        committed_at(&SLOTS, &edits, &positions[1]),
        Committed::Edited {
            payload: 0x9,
            append: 2
        },
        "last write wins on the read side too, and it reports which record that was"
    );

    // Now the equivalence: the live payload at every distinct edited position, hashed, is the same
    // root the commit path hashed from the same records.
    let mut scratch_leaves = [Leaf::DEFAULT; 8];
    let committed_root = commit_epoch(
        &ladder,
        CommitRequest {
            region_slots: &SLOTS,
            edits: &edits,
            event_leaves: &[],
            base_genesis_root: genesis_root(SEED, 0, 0),
            epoch_index: 0,
        },
        &mut CommitScratch {
            slot_rows: &mut [[0u8; 32]; 2],
            region_leaves: &mut [[0u8; 32]; 2],
            leaf_space: &mut scratch_leaves,
        },
    )
    .expect("fits");

    let mut rebuilt = [Leaf::DEFAULT; 3];
    for (index, position) in positions.iter().enumerate() {
        let Committed::Edited { payload, append } = committed_at(&SLOTS, &edits, position) else {
            panic!("position {index} was edited");
        };
        let offset = position.local_offset();
        rebuilt[index] = Leaf {
            x: offset.x,
            y: offset.y,
            z: offset.z,
            payload,
            append: append as u32,
        };
    }
    let mut rebuilt_two = rebuilt;
    let from_read = ledger::region_root(&mut rebuilt_two, &ladder);

    let mut own_leaves = [Leaf::DEFAULT; 8];
    let from_records =
        ledger::commit::region_root_for(&records, &mut own_leaves, &ladder).expect("fits");
    assert_eq!(
        from_read, from_records,
        "the read side reproduces what the root commits to"
    );
    // Slot 0 was never addressed, so it contributes the ladder's top; slot 1 contributes what the
    // read side reproduced. That is the whole epoch, assembled from the reader's point of view.
    let expected_regions = ledger::list_root(&[
        ledger::region_leaf(0, &ladder.empty(12)),
        ledger::region_leaf(1, &from_records),
    ]);
    assert_eq!(
        committed_root.regions, expected_regions,
        "the region list the commit built is the one the read implies"
    );
}

/// A voxel nobody edited is *absent*, not zero. A base-rule value would be SPEC-04's terrain half
/// (OQ-27 open), and returning `0` here would let a caller read "air" out of an unspecified world.
#[test]
fn an_untouched_voxel_is_absent_rather_than_zero() {
    let records = [record(&far_position(17, 4095, 0), 0x2A)];
    let edits = [RegionEdits {
        slot: 1,
        records: &records,
    }];
    assert_eq!(
        committed_at(&SLOTS, &edits, &far_position(18, 4095, 0)),
        Committed::Unedited
    );
    // A region the epoch never addressed is absent too, by way of the slot table having no entry.
    let other = voxel_at(
        &RegionKey { x: 5, y: 5, z: 5 },
        &LocalOffset { x: 1, y: 1, z: 1 },
    );
    assert_eq!(committed_at(&SLOTS, &edits, &other), Committed::Unedited);
    // And an empty edit row is absence, not a payload of zero.
    let empty: [DeltaRecord; 0] = [];
    assert_eq!(
        committed_at(
            &SLOTS,
            &[RegionEdits {
                slot: 1,
                records: &empty
            }],
            &far_position(0, 0, 0)
        ),
        Committed::Unedited
    );
}

#[test]
fn the_edited_set_is_the_positions_the_root_covers() {
    let records = [
        record(&far_position(1, 2, 3), 0x1),
        record(&far_position(4, 5, 6), 0x2),
        record(&far_position(1, 2, 3), 0x3),
    ];
    let edits = [RegionEdits {
        slot: 1,
        records: &records,
    }];
    let mut buffer = [GlobalVoxelPos { x: 0, y: 0, z: 0 }; 3];
    let listed = edited_positions(&SLOTS, &edits, &mut buffer).expect("three records");
    assert_eq!(listed.len(), 3, "one entry per record, duplicates included");
    assert_eq!(listed[0], far_position(1, 2, 3));
    assert_eq!(listed[2], far_position(1, 2, 3));

    let mut too_small = [GlobalVoxelPos { x: 0, y: 0, z: 0 }; 2];
    assert!(edited_positions(&SLOTS, &edits, &mut too_small).is_none());
}
