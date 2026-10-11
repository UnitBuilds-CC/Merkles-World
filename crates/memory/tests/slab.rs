//! Gate-facing tests for the slab: the two properties docs/03 claims for it, and the budget question
//! it does not answer.
//!
//! Throughput (gate **P1.2**'s 10⁶ writes/sec and `p99 < 1 µs`) is *not* tested here — a test that
//! asserts a rate on a shared CI runner is a flake with a number attached. That belongs to spike S2
//! (`cargo run --release --example slab_throughput -p memory`), which reports rather than gates.
//! What these tests do hold is the shape of the guarantee: one store per append, contents exactly
//! what was appended in append order, and a reset that costs the same whether the slab held ten
//! records or a hundred thousand.

use contracts::coords::LocalOffset;
use contracts::delta::{DeltaRecord, RegionSlot, RECORD_BYTES};
use memory::{Slab, SlabFull, SLAB_1MB_RECORDS, SLAB_64KB_RECORDS};

fn record(x: u16, payload: u16) -> DeltaRecord {
    DeltaRecord::new(RegionSlot(0), LocalOffset { x, y: 0, z: 0 }, payload)
}

#[test]
fn append_is_one_store_past_the_bump_pointer() {
    let mut buffer = [0u64; 8];
    let mut slab = Slab::new(&mut buffer);
    assert_eq!(slab.capacity(), 8);
    assert!(slab.is_empty());

    for index in 0..8 {
        assert_eq!(
            slab.append(record(index as u16, 0x10 + index as u16)),
            Ok(index)
        );
    }
    assert!(slab.is_full());
    assert_eq!(slab.len(), 8);
    assert_eq!(slab.bytes(), 8 * RECORD_BYTES);

    // The words are the records, in order, unchanged — no encoding of its own for the slab to drift.
    let written: u64 = slab.records()[3];
    assert_eq!(written, record(3, 0x13).as_u64());
    let decoded: Vec<DeltaRecord> = slab.deltas().collect();
    assert_eq!(decoded.len(), 8);
    assert_eq!(decoded[7].intra_region().x, 7);
    assert_eq!(decoded[7].payload(), 0x17);
}

#[test]
fn a_full_slab_refuses_rather_than_growing() {
    let mut buffer = [0u64; 2];
    let mut slab = Slab::new(&mut buffer);
    assert_eq!(slab.append(record(0, 1)), Ok(0));
    assert_eq!(slab.append(record(1, 2)), Ok(1));
    assert_eq!(slab.append(record(2, 3)), Err(SlabFull { capacity: 2 }));
    // The refusal changed nothing: capacity is the buffer, and there is no allocator to resize it.
    assert_eq!(slab.len(), 2);
    assert_eq!(slab.deltas().count(), 2);
}

#[test]
fn reset_is_o_one_regardless_of_how_much_was_written() {
    let mut small = [0u64; 16];
    let mut slab = Slab::new(&mut small);
    for index in 0..16 {
        slab.append(record(index, 1)).expect("capacity 16");
    }
    slab.reset();
    assert!(slab.is_empty());
    assert!(!slab.is_full());
    assert_eq!(slab.records(), &[] as &[u64]);

    // Recycled, not reallocated: the same buffer, the same capacity, appends from index 0 again.
    assert_eq!(slab.append(record(99, 7)), Ok(0));
    assert_eq!(slab.deltas().next().map(|r| r.intra_region().x), Some(99));
}

/// The two capacities docs/03 writes down are asserted as arithmetic, not chosen. If a commit picks
/// one of them, that is spike S2's call with a measurement behind it — and the entry counts here are
/// what that measurement will be asked to sustain.
#[test]
fn both_candidate_budgets_are_record_counts_not_guesses() {
    assert_eq!(SLAB_64KB_RECORDS, 8192);
    assert_eq!(SLAB_1MB_RECORDS, 131_072);
    assert_eq!(SLAB_64KB_RECORDS * RECORD_BYTES, 64 * 1024);
    assert_eq!(SLAB_1MB_RECORDS * RECORD_BYTES, 1024 * 1024);
}

/// The largest of the three budgets the design names, exercised end to end: 131,072 appends and a
/// reset, so "bounded by the buffer, not by a field width" (ADR-029) is demonstrated at size rather
/// than asserted in prose.
#[test]
fn a_one_megabyte_slab_holds_every_edit_a_region_made_in_that_epoch() {
    let mut buffer = vec![0u64; SLAB_1MB_RECORDS];
    let mut slab = Slab::new(&mut buffer);
    for index in 0..SLAB_1MB_RECORDS {
        slab.append(record(0, (index % 4096) as u16))
            .expect("sized exactly");
    }
    assert!(slab.is_full());
    assert_eq!(slab.len(), SLAB_1MB_RECORDS);
    slab.reset();
    assert_eq!(slab.len(), 0);
    assert_eq!(slab.capacity(), SLAB_1MB_RECORDS);
}
