//! Spike S2, second half: what a Merkle pass over dirty slabs costs, in the terms docs/03 guessed.
//!
//! Gate **P1.3** claims "Merkle pass over a full dirty slab array `< 1 ms`" — quoted from the source
//! conversation as a "Sub-millisecond batch pass" (GS 1606) and recorded in docs/03 as **a claim, not
//! a measurement**. docs/03 also derives a cost model: an epoch with *N* edits in one region
//! recomputes at most `12N` branch hashes. This measures both, so the next person can argue with a
//! number instead of with prose.
//!
//! Run: `cargo run --release --example merkle_pass -p ledger`

use contracts::coords::LocalOffset;
use contracts::delta::{DeltaRecord, RegionSlot};
use core::time::Duration;
use ledger::commit::{region_root_for, resolve_last_write_wins};
use ledger::{region_root, EmptyLadder, Leaf};
use std::time::Instant;

/// A full 64 KB slab's worth of records — the "full dirty slab array" of gate P1.3 at docs/03's
/// smaller candidate budget.
const FULL_SLAB: usize = 8_192;

/// The sizes the cost model is tested at. `100_000` is the figure docs/03 does the arithmetic at.
const LADDER: [usize; 3] = [1_000, 10_000, 100_000];

/// A deterministic integer position spread over one region. An LCG because it needs no clock, no
/// allocator and no float — the same discipline a hashed value would be held to, even though nothing
/// here is hashed from this generator's raw output.
fn edit(index: usize) -> DeltaRecord {
    let mut state = (index as u64)
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1442695040888963407);
    state ^= state >> 31;
    state = state.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    state ^= state >> 29;
    DeltaRecord::new(
        RegionSlot(0),
        LocalOffset {
            x: (state & 0xFFF) as u16,
            y: ((state >> 12) & 0xFFF) as u16,
            z: ((state >> 24) & 0xFFF) as u16,
        },
        (state >> 40) as u16,
    )
}

fn distinct_leaves(n: usize) -> Vec<Leaf> {
    // Positions are forced distinct: the hashing pass is what is being timed, not the dedupe.
    (0..n)
        .map(|index| Leaf {
            x: (index % 4096) as u16,
            y: ((index / 4096) % 4096) as u16,
            z: ((index / 1_048_576) % 4096) as u16,
            payload: (index % 4096) as u16,
        })
        .collect()
}

fn main() {
    let ladder = EmptyLadder::compute();

    println!(
        "// spike S2, part two — the Merkle pass. host {} / arch {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("// gate P1.3's claim: a full dirty slab array in < 1 ms. docs/03's model: at most 12N branch hashes for N edits.");
    println!();
    println!(
        "N edits in one region | sort+hash pass | hashes implied (12N) | ns per hash | digests"
    );

    for n in LADDER {
        let mut leaves = distinct_leaves(n);
        let start = Instant::now();
        let root = region_root(&mut leaves, &ladder);
        let elapsed = start.elapsed();
        let hashes = 12 * n;
        let per_hash = elapsed.as_nanos() / (hashes.max(1) as u128);
        println!(
            "{n:>18} | {:>13} | {:>17} | {:>10} | {}",
            human(elapsed),
            hashes,
            format!("{per_hash} ns"),
            hex(&root)
        );
    }

    println!();
    println!("// A full slab through the pipeline (resolve last-write-wins, then the pass):");
    let records: Vec<DeltaRecord> = (0..FULL_SLAB).map(edit).collect();
    let mut scratch = vec![
        Leaf {
            x: 0,
            y: 0,
            z: 0,
            payload: 0
        };
        FULL_SLAB
    ];

    let start = Instant::now();
    let resolved = resolve_last_write_wins(&records, &mut scratch).expect("one leaf per record");
    let resolve_time = start.elapsed();
    let count = resolved.len();

    let mut leaves = distinct_leaves(FULL_SLAB);
    let start = Instant::now();
    let _ = region_root(&mut leaves, &ladder);
    let pass_time = start.elapsed();

    let start = Instant::now();
    let root = region_root_for(&records, &mut scratch, &ladder).expect("fits");
    let together = start.elapsed();

    println!(
        "resolve {FULL_SLAB} records : {} ({} distinct voxels) — the O(n²) dedupe, priced",
        human(resolve_time),
        count
    );
    println!("hash pass {FULL_SLAB} leaves : {}", human(pass_time));
    println!(
        "both, one region           : {}   → gate P1.3 ({} vs < 1 ms) for ONE full slab",
        human(together),
        human(together)
    );
    println!(
        "eight full slabs           : ~{} if the per-region cost is flat (not measured here)",
        human(together * 8)
    );
    println!("digest of the full slab    : {}", hex(&root));
    println!();
    println!(
        "// What this says about the claim: the pass itself is one number and the pipeline around"
    );
    println!(
        "// it is another. docs/03 quotes the sub-1 ms figure as a batch pass over hashed records;"
    );
    println!(
        "// whether gate P1.3 means the pass or the whole commit is OQ-47's neighbour and is now"
    );
    println!("// a question with two measured answers next to it, which it did not have before.");
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

fn hex(root: &[u8; 32]) -> String {
    root.iter().map(|byte| format!("{byte:02x}")).collect()
}
