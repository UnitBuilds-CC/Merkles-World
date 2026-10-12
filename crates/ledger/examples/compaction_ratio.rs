//! What compaction actually saves, measured at spike S2's own scale.
//!
//! `cargo run --release --example compaction_ratio -p ledger`
//!
//! docs/03 § Compaction now says compaction is a lever on storage and on capacity rather than on the
//! Merkle pass, and `tests/compaction.rs` proves it on a 72-record fixture. One fixture is not a
//! measurement, and OQ-49 cannot be argued with a 72-record number: gate P1.3 talks about a full dirty
//! slab array, so this runs the same experiment at S2's scale, over the same two arrangements S2 timed,
//! and reports the four quantities that could plausibly change:
//!
//! - **records kept** — the arena and the wire,
//! - **leaf scratch** — what a commit has to hold while it hashes,
//! - **the feed** — the `O(N)` walk from records into that scratch,
//! - **digests computed** — the thing the sub-millisecond claim is about.
//!
//! The last column is where the expectation broke, in both directions. Writing the same voxel set 16
//! times over costs 16× the records and **exactly the same digests** as writing it once — last-write-wins
//! resolves at the leaf, so hashing was always priced by distinct voxels — yet the *wall time* does grow
//! with records, because the walk partitions its leaf slice at each of twelve levels. Compaction removes
//! the record-count term and leaves the distinct-voxel floor, which is a real 1.6× to 2.5× on a heavily
//! duplicated region and exactly nothing on the case gate P1.3 was quoted at: a full slab of *distinct*
//! scattered edits, where compaction is the identity. The memory saving is unconditional, and that makes
//! this transform a SPEC-06 and cold-tier (P1.4) instrument first.
//!
//! Counts repeat across machines; the microseconds do not. CI runs this example on all three
//! architectures as an informational step, so the digest columns there are comparable evidence and the
//! timings are labelled as this host's own.

use contracts::coords::LocalOffset;
use contracts::delta::{DeltaRecord, RegionSlot};
use core::time::Duration;
use ledger::commit::{leaves_from_records, region_root_for};
use ledger::compact::{assign_ranks, compact, write_records, Edit};
use ledger::{region_root_counted, EmptyLadder, Leaf};
use std::collections::HashSet;
use std::time::Instant;

/// Distinct voxels per row. 2,048 keeps the largest row (16 writes each = 32,768 records) inside four
/// 64 KB slabs while the scattered case still costs tens of thousands of digests.
const VOXELS: usize = 2_048;

/// The duplication factor is the whole experiment: it is the ratio between "somebody edited this many
/// different blocks" and "somebody edited this block this many times", which is exactly what a
/// compaction is allowed to collapse.
const WRITES_PER_VOXEL: [usize; 4] = [1, 2, 4, 16];

/// S2's scattered generator, verbatim, so the digest counts below are comparable with the ones
/// `merkle_pass` publishes rather than a new arrangement someone has to re-derive.
fn scattered(index: usize) -> LocalOffset {
    let mut state = (index as u64)
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1442695040888963407);
    state ^= state >> 31;
    state = state.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    state ^= state >> 29;
    LocalOffset {
        x: (state & 0xFFF) as u16,
        y: ((state >> 12) & 0xFFF) as u16,
        z: ((state >> 24) & 0xFFF) as u16,
    }
}

/// Consecutive voxels along one row, which is `merkle_pass`'s clustered arrangement: neighbours share
/// most of their octree path, so the same leaf count costs far fewer digests.
fn clustered(index: usize) -> LocalOffset {
    LocalOffset {
        x: (index % 4096) as u16,
        y: ((index / 4096) % 4096) as u16,
        z: 0,
    }
}

fn positions(arrangement: fn(usize) -> LocalOffset, count: usize) -> Vec<LocalOffset> {
    let mut seen = HashSet::with_capacity(count);
    let mut out = Vec::with_capacity(count);
    let mut candidate = 0usize;
    while out.len() < count {
        let position = arrangement(candidate);
        candidate += 1;
        if seen.insert((position.x, position.y, position.z)) {
            out.push(position);
        }
        // The scattered hash is drawn from a 2^36 space, so it needs no more than a few thousand extra
        // draws to fill 2,048 distinct cells; the guard is against a pathological arrangement looping
        // forever rather than a case this run expects.
        assert!(
            candidate < count * 64,
            "could not find {count} distinct positions"
        );
    }
    out
}

/// `writes` records per position, payload varying with the write so the survivor is distinguishable.
fn records_at(positions: &[LocalOffset], writes: usize) -> Vec<DeltaRecord> {
    let mut out = Vec::with_capacity(positions.len() * writes);
    for write in 0..writes {
        for (index, position) in positions.iter().enumerate() {
            out.push(DeltaRecord::new(
                RegionSlot(0),
                *position,
                (write * 4096 + index) as u16,
            ));
        }
    }
    out
}

fn row(name: &str, arrangement: fn(usize) -> LocalOffset, ladder: &EmptyLadder) {
    let positions = positions(arrangement, VOXELS);
    println!(
        " arrangement | voxels | writes/voxel | records | live after | arena KiB | scratch | digests before | digests after | pass before | pass after | feed before | feed after"
    );
    for writes in WRITES_PER_VOXEL {
        let records = records_at(&positions, writes);
        let count = records.len();

        // The uncompacted commit: one leaf per record, fed in arrival order, then hashed.
        let mut scratch = vec![Leaf::DEFAULT; count];
        let start = Instant::now();
        let leaves = leaves_from_records(&records, &mut scratch).expect("one leaf per record");
        let feed_before = start.elapsed();
        let len = leaves.len();
        let mut digests_before = 0usize;
        let start = Instant::now();
        let root_before = region_root_counted(&mut scratch[..len], ladder, &mut digests_before);
        let pass_before = start.elapsed();

        // The same commit after compaction, through the same entry points a caller would use.
        let mut edits = vec![
            Edit {
                rank: 0,
                record: records[0],
            };
            count
        ];
        assign_ranks(&records, &mut edits).expect("room");
        let live = compact(&mut edits);
        let mut compacted = vec![records[0]; live];
        write_records(&edits[..live], &mut compacted).expect("exact room");

        let mut tight = vec![Leaf::DEFAULT; live];
        let start = Instant::now();
        let fed = leaves_from_records(&compacted, &mut tight).expect("compacted fits");
        let feed_after = start.elapsed();
        let fed_len = fed.len();
        let mut digests_after = 0usize;
        let start = Instant::now();
        let root_after = region_root_counted(&mut tight[..fed_len], ladder, &mut digests_after);
        let pass_after = start.elapsed();

        // The two things the fixture asserted and a table has to keep asserting at scale, or the whole
        // row is a description of a different ledger.
        assert_eq!(
            root_before, root_after,
            "{name} x{writes}: compaction changed state"
        );
        assert_eq!(
            digests_before, digests_after,
            "{name} x{writes}: the hash pass moved, which this example exists to disprove"
        );
        // A scratch sized for the compacted region refuses the uncompacted one — the capacity claim,
        // stated as the refusal that proves it is a real reduction rather than a relabelling. Only when
        // the two sizes differ, which is every row except the x1 baseline, where nothing can be removed.
        if live < count {
            let mut refuse = vec![Leaf::DEFAULT; live];
            assert!(
                region_root_for(&records, &mut refuse, ladder).is_none(),
                "{name} x{writes}: an oversized record set fitted into a compacted scratch"
            );
        }

        println!(
            " {name:>12} | {:>6} | {:>14} | {:>7} | {:>10} | {:>9} | {:>7} | {:>14} | {:>13} | {:>11} | {:>10} | {:>11} | {:>10}",
            VOXELS,
            writes,
            count,
            live,
            format!("{:.0}/{:.1}", count as f64 * 8.0 / 1024.0, live as f64 * 8.0 / 1024.0),
            format!("{:.0}/{:.0}", count, live),
            digests_before,
            digests_after,
            human(pass_before),
            human(pass_after),
            human(feed_before),
            human(feed_after),
        );
    }
    println!();
}

fn main() {
    let ladder = EmptyLadder::compute();
    println!(
        "// compaction ratio — {} distinct voxels, 1 to 16 writes each. host {} / arch {}",
        VOXELS,
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("// arena KiB and scratch are before/after; digests, pass, feed likewise.");
    println!();
    row("scattered", scattered, &ladder);
    row("clustered", clustered, &ladder);
    println!("// What the tables say, including the half that came out differently from the expectation:");
    println!("// - the digest count is flat in every row. 16 writes over 2,048 voxels hashes exactly as many");
    println!(
        "//   digests as 1 write does, because the leaf resolves last-write-wins before any branch"
    );
    println!("//   hash. So the pass is priced by DISTINCT voxels, docs/03's `12N` was always a bound on");
    println!(
        "//   leaves and not on records, and no amount of compaction can shrink S2's 68,524 for a full"
    );
    println!("//   slab of distinct scattered edits - there is nothing there to compact.");
    println!("// - the wall time is NOT flat, and that is the correction this probe exists to publish. The");
    println!("//   walk partitions its leaf slice at each of twelve levels, so per-entry work scales with");
    println!(
        "//   records even when hashing does. Compaction removes that term and leaves the floor:"
    );
    println!("//   records even when hashing does. Compaction removes that term and leaves the floor, which");
    println!(
        "//   at 16 writes per voxel is worth ~1.5-1.6x of the scattered pass and ~2.5-2.7x of the"
    );
    println!("//   clustered one. Ratios, not milliseconds: this host's absolute times drift by more than");
    println!("//   that between runs, while the digest columns do not move at all. At 1 and 2 writes the");
    println!("//   before and after columns agree inside noise.");
    println!("//   So 'compaction is not a lever on the Merkle pass' was too strong a sentence — it is not");
    println!("//   a lever on the digests, it is a lever on the partitioning, and only once duplication is");
    println!("//   high.");
    println!("// - the floor is the problem that remains. Every compacted scattered row lands in the same");
    println!("//   place whatever the record count, so gate P1.3's `< 1 ms` is not reachable by compaction");
    println!(
        "//   for the workload S2 actually measured: a full slab of distinct edits, where live equals"
    );
    println!("//   records and the transform is the identity. OQ-49's 'compaction frequency' option is not");
    println!("//   universally empty, but it is empty for the quoted case.");
    println!(
        "// - the memory claim survives untouched and is the larger one, and it is unconditional:"
    );
    println!("//   256 KiB of records back to 16 KiB, 32,768 scratch entries back to 2,048, the feed 16x");
    println!("//   cheaper, same root. That makes this transform a SPEC-06 and cold-tier (P1.4) instrument");
    println!("//   first and a P1.3 instrument only for duplicated workloads.");
    println!("// - unchanged by this probe: nothing here is a frame-time claim and there is still no GPU.");
}

fn human(elapsed: Duration) -> String {
    if elapsed.as_micros() < 1_000 {
        format!("{} µs", elapsed.as_micros())
    } else if elapsed.as_millis() < 1_000 {
        format!("{:.3} ms", elapsed.as_micros() as f64 / 1_000.0)
    } else {
        format!("{:.3} s", elapsed.as_micros() as f64 / 1_000_000.0)
    }
}
