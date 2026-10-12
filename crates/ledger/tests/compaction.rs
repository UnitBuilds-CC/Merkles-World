//! Compaction, tested the way A-09 says it must be: it may change how state is encoded and must not
//! change the state.
//!
//! The property is stated against the digest rather than against the record list, because the digest *is*
//! the state as far as a peer, a save and gate P1.6's bisection are concerned. A compaction that returns
//! "fewer records, same length, different root" would pass a length check and corrupt the ledger quietly,
//! which is the class of failure `CompactionNotIdempotent` names.

use contracts::coords::LocalOffset;
use contracts::delta::{DeltaRecord, RegionSlot};
use ledger::commit::region_root_for;
use ledger::compact::{assign_ranks, compact, voxel_of, write_records, Edit};
use ledger::region_root_counted;
use ledger::{EmptyLadder, Leaf};

fn edit_at(slot: u16, x: u16, y: u16, z: u16, payload: u16) -> DeltaRecord {
    DeltaRecord::new(RegionSlot(slot), LocalOffset { x, y, z }, payload)
}

fn root(records: &[DeltaRecord], ladder: &EmptyLadder) -> [u8; 32] {
    let mut scratch = vec![Leaf::DEFAULT; records.len().max(1)];
    region_root_for(records, &mut scratch, ladder).expect("scratch sized to the records")
}

fn counted(records: &[DeltaRecord], ladder: &EmptyLadder) -> ([u8; 32], usize) {
    // The append rank is part of the rule, not a detail: duplicates are resolved by greatest append, so
    // a helper that zeroes it changes which write survives and tests a different ledger.
    let mut scratch: Vec<Leaf> = records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let offset = record.intra_region();
            Leaf {
                x: offset.x,
                y: offset.y,
                z: offset.z,
                payload: record.payload(),
                append: index as u32,
            }
        })
        .collect();
    let mut digests = 0usize;
    let root = region_root_counted(&mut scratch, ladder, &mut digests);
    (root, digests)
}

/// The case compaction exists for: the same voxels written repeatedly. The interesting payload is the
/// *last* one per voxel, which is exactly the rule the leaf already applies.
fn dirty() -> Vec<DeltaRecord> {
    let mut out = Vec::new();
    for round in 0..6u16 {
        for voxel in 0..12u16 {
            out.push(edit_at(0, voxel, round % 3, voxel * 2, round * 16 + voxel));
        }
    }
    out
}

#[test]
fn compacting_a_dirty_region_leaves_its_root_exactly_where_it_was() {
    let ladder = EmptyLadder::compute();
    let records = dirty();
    let before = root(&records, &ladder);

    let mut edits = vec![
        Edit {
            rank: 0,
            record: edit_at(0, 0, 0, 0, 0),
        };
        records.len()
    ];
    {
        let assigned = assign_ranks(&records, &mut edits).expect("room for every record");
        assert_eq!(assigned.len(), records.len());
        let live = compact(assigned);
        let mut compacted = vec![edit_at(0, 0, 0, 0, 0); live];
        let written = write_records(&edits[..live], &mut compacted).expect("exact room");
        assert_eq!(written, live);
        assert_eq!(
            root(&compacted, &ladder),
            before,
            "compaction changed the state, not just its encoding"
        );
        assert!(
            live < records.len(),
            "six rounds over 72 writes should shrink, saw {live}"
        );
        // Twelve voxels, but each is written at three different y values, so 36 distinct cells - the
        // count the collapse must land on, and the reason "one survivor per voxel" needs the voxel to
        // mean the whole (x, y, z) triple rather than the interesting-looking half of it.
        assert_eq!(
            live, 36,
            "one survivor per distinct position, no exceptions"
        );
    }
}

#[test]
fn compaction_is_idempotent_and_a_second_pass_is_a_no_op() {
    let ladder = EmptyLadder::compute();
    let records = dirty();
    let mut edits = vec![
        Edit {
            rank: 0,
            record: edit_at(0, 0, 0, 0, 0),
        };
        records.len()
    ];
    assign_ranks(&records, &mut edits).expect("room");
    let once = compact(&mut edits);
    let root_once = {
        let mut out = vec![edit_at(0, 0, 0, 0, 0); once];
        write_records(&edits[..once], &mut out).expect("exact");
        root(&out, &ladder)
    };

    // A second pass over the live prefix: same length, same order, same digest. This is precisely what
    // A-09's band-3 `CompactionNotIdempotent` exists to report, so it is asserted rather than argued.
    let twice = compact(&mut edits[..once]);
    assert_eq!(
        twice, once,
        "a second compaction removed something it should not have"
    );
    let mut out = vec![edit_at(0, 0, 0, 0, 0); twice];
    write_records(&edits[..twice], &mut out).expect("exact");
    assert_eq!(root(&out, &ladder), root_once);
    // The output must be a set of voxels, not merely the right length: a bug that kept two records for one
    // voxel and dropped an unrelated one elsewhere would still satisfy the digest check above.
    let mut voxels: Vec<u64> = out.iter().map(|record| voxel_of(*record)).collect();
    let survivors = voxels.len();
    voxels.sort_unstable();
    voxels.dedup();
    assert_eq!(
        voxels.len(),
        survivors,
        "a compaction that leaves duplicates behind while dropping something else"
    );
}

#[test]
fn the_greatest_rank_wins_and_which_record_that_is_follows_the_arrival_order() {
    // Three writes to one cell. The winner is the record holding the greatest rank, which across
    // permuted arrivals is a *different payload each time* - asserting a fixed payload here would be
    // asserting that compaction chooses by value, which is neither the rule nor what the leaf does.
    let a = edit_at(0, 7, 1, 2, 0x11);
    let b = edit_at(0, 7, 1, 2, 0x22);
    let c = edit_at(0, 7, 1, 2, 0x33);

    for arrival in [[a, b, c], [c, a, b], [b, c, a]] {
        let expected = *arrival.last().expect("three writes");
        let mut edits: Vec<Edit> = arrival
            .iter()
            .enumerate()
            .map(|(index, record)| Edit {
                rank: index as u32,
                record: *record,
            })
            .collect();
        let live = compact(&mut edits);
        assert_eq!(live, 1, "three writes to one cell should leave one");
        assert_eq!(
            edits[0].record.payload(),
            expected.payload(),
            "the survivor should be the greatest rank, not the largest value"
        );
        assert_eq!(
            edits[0].rank, 2,
            "and it should carry the rank that made it win"
        );
    }
}

#[test]
fn a_region_with_no_duplicates_completes_unchanged_and_a_short_buffer_refuses() {
    let ladder = EmptyLadder::compute();
    let clean = vec![
        edit_at(0, 1, 1, 1, 0xaa),
        edit_at(0, 2, 2, 2, 0xbb),
        edit_at(0, 3, 3, 3, 0xcc),
    ];
    let mut edits = vec![
        Edit {
            rank: 0,
            record: edit_at(0, 0, 0, 0, 0),
        };
        clean.len()
    ];
    assign_ranks(&clean, &mut edits).expect("room");
    let live = compact(&mut edits);
    assert_eq!(
        live,
        clean.len(),
        "nothing was shadowed, nothing may vanish"
    );
    let mut out = vec![edit_at(0, 0, 0, 0, 0); live];
    write_records(&edits[..live], &mut out).expect("exact");
    assert_eq!(
        out, clean,
        "the survivors must be the records that went in, in order"
    );
    assert_eq!(root(&out, &ladder), root(&clean, &ladder));

    let mut too_short = vec![
        Edit {
            rank: 0,
            record: edit_at(0, 0, 0, 0, 0),
        };
        clean.len() - 1
    ];
    assert!(
        assign_ranks(&clean, &mut too_short).is_none(),
        "a compaction that silently drops the tail is the failure mode this module exists to avoid"
    );
    let mut small_out = vec![edit_at(0, 0, 0, 0, 0); live - 1];
    assert_eq!(write_records(&edits[..live], &mut small_out), None);
}

#[test]
fn compaction_shrinks_the_records_without_shrinking_the_digest_work_and_that_is_the_point() {
    // What this test was written to prove, and what it found instead. The expectation was that halving the
    // records halves the hashing: the assertion below says **the digests are the same count either way**,
    // because duplicates already collapse at the leaf for free - one cell, one leaf hash, the winner chosen
    // by comparison - which is exactly the fix spike S2 made when it moved last-write-wins out of a 46 ms
    // pre-pass and into the walk.
    //
    // So compaction is not a lever on the Merkle pass. It is a lever on storage and on capacity: half the
    // records to keep in an arena, half the scratch a commit needs (SPEC-06's question), and the same
    // root. That distinction is load-bearing for OQ-49, which asks whether compaction frequency could be
    // the thing that moves gate P1.3 - on this evidence, it cannot.
    let ladder = EmptyLadder::compute();
    let records = dirty();
    let (root_before, digests_before) = counted(&records, &ladder);

    let mut edits = vec![
        Edit {
            rank: 0,
            record: edit_at(0, 0, 0, 0, 0),
        };
        records.len()
    ];
    assign_ranks(&records, &mut edits).expect("room");
    let live = compact(&mut edits);
    let mut compacted = vec![edit_at(0, 0, 0, 0, 0); live];
    write_records(&edits[..live], &mut compacted).expect("exact");
    let (root_after, digests_after) = counted(&compacted, &ladder);

    assert_eq!(root_before, root_after, "compaction changed the state");
    assert_eq!(
        digests_before, digests_after,
        "the pass cost moved, which contradicts the finding this test records"
    );
    assert_eq!(
        live,
        records.len() / 2,
        "six rounds over each cell should halve the record count"
    );

    // The capacity saving is the real one, and it is a refusal the other way round: a leaf space sized for
    // the compacted region cannot serve the uncompacted one.
    let mut tight_scratch = vec![Leaf::DEFAULT; live];
    assert!(
        region_root_for(&records, &mut tight_scratch, &ladder).is_none(),
        "a too-short scratch must refuse rather than commit to a partial region"
    );
    assert!(
        region_root_for(&compacted, &mut tight_scratch, &ladder).is_some(),
        "the compacted set fits the space its own size implies"
    );
}
