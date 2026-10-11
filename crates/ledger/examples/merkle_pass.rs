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
use ledger::commit::leaves_from_records;
use ledger::{region_root_counted, EmptyLadder, Leaf};
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
            append: Leaf::DEFAULT.append,
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
        "N edits | arrangement | pass time | digests computed | 12N bound | ns per digest | region root"
    );

    for n in LADDER {
        // Two arrangements of the SAME count, because the cost model does not say which it means:
        // consecutive voxels share most of their paths, scattered ones share almost none.
        let mut clustered = distinct_leaves(n);
        let mut cluster_digests = 0usize;
        let start = Instant::now();
        let cluster_root = region_root_counted(&mut clustered, &ladder, &mut cluster_digests);
        let cluster_time = start.elapsed();

        let scattered: Vec<Leaf> = (0..n)
            .map(|index| Leaf::from_record(edit(index), index as u32))
            .collect();
        let mut scattered = scattered;
        let mut scatter_digests = 0usize;
        let start = Instant::now();
        let scatter_root = region_root_counted(&mut scattered, &ladder, &mut scatter_digests);
        let scatter_time = start.elapsed();

        println!(
            "{n:>7} | clustered  | {:>9} | {:>16} | {:>8} | {:>14} | {}",
            human(cluster_time),
            cluster_digests,
            12 * n,
            format!(
                "{} ns",
                cluster_time.as_nanos() / (cluster_digests.max(1) as u128)
            ),
            hex(&cluster_root)
        );
        println!(
            "{n:>7} | scattered  | {:>9} | {:>16} | {:>8} | {:>14} | {}",
            human(scatter_time),
            scatter_digests,
            12 * n,
            format!(
                "{} ns",
                scatter_time.as_nanos() / (scatter_digests.max(1) as u128)
            ),
            hex(&scatter_root)
        );
    }

    println!();
    println!("// One full 64 KB slab, end to end — the array gate P1.3 names:");
    let records: Vec<DeltaRecord> = (0..FULL_SLAB).map(edit).collect();
    let mut scratch = vec![Leaf::DEFAULT; FULL_SLAB];

    let start = Instant::now();
    let fed = leaves_from_records(&records, &mut scratch).expect("one leaf per record");
    let feed_time = start.elapsed();
    let count = fed.len();

    let mut digests = 0usize;
    let start = Instant::now();
    let root = region_root_counted(&mut scratch, &ladder, &mut digests);
    let pass_time = start.elapsed();
    let together = feed_time + pass_time;

    println!(
        "feed {count} records into the walk : {} — O(N); the O(N²) pre-pass it replaced cost 46 ms at this size",
        human(feed_time)
    );
    println!(
        "counted pass, scattered        : {} for {digests} digests (the 12N bound says {})",
        human(pass_time),
        12 * count
    );
    println!(
        "one slab, end to end           : {}   → gate P1.3 asks for < 1 ms",
        human(together)
    );
    println!(
        "eight full slabs in one epoch  : ~{} if the per-region cost is flat (not measured here)",
        human(together * 8)
    );
    println!("region root of the full slab   : {}", hex(&root));

    // Attribution. At ~660 ns per digest the pass is not BLAKE3-bound on its face — a 266-byte hash
    // should be far cheaper — so measure the hasher alone, over the same number of identical-size
    // preimages, and separate "what BLAKE3 costs" from "what our preimage construction costs".
    let preimage = [7u8; 8 + 2 + 256];
    let mut acc = 0u8;
    let start = Instant::now();
    for _ in 0..digests {
        acc = acc.wrapping_add(blake3::hash(&preimage).as_bytes()[0]);
    }
    let raw_time = start.elapsed();
    println!(
        "raw BLAKE3, {} identical {}-byte preimages : {} = {} ns each (sink {acc})",
        digests,
        preimage.len(),
        human(raw_time),
        raw_time.as_nanos() / (digests.max(1) as u128)
    );
    println!(
        "so the pass's per-digest cost is {} ns, of which BLAKE3 itself is {} ns",
        pass_time.as_nanos() / (digests.max(1) as u128),
        raw_time.as_nanos() / (digests.max(1) as u128)
    );
    println!();
    println!("// The finding is not only that the target is missed. The same N costs different");
    println!(
        "// passes depending on how the edits are arranged, so `12N` is a bound and not a cost"
    );
    println!(
        "// — and OQ-49 has to say whether P1.3 means the clustered case, the scattered case,"
    );
    println!("// the pass, or the whole commit before anyone can call it met or missed.");
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
