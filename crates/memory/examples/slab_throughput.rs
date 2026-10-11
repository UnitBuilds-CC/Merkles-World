//! Spike S2, first half: what a slab append actually costs on this machine.
//!
//! Gate **P1.2** asks for 10⁶ writes/sec sustained with `p99 < 1 µs` (docs/03 § Validation gates —
//! the target is ours; the transcript only claims "a pointer offset store instruction… nanoseconds",
//! GS 1594). This prints a measurement, it does not gate: a latency assertion on a shared CI runner
//! is a flake with a number attached, so the number belongs in the doc and the assertion stays out of
//! the workflow.
//!
//! Run: `cargo run --release --example slab_throughput -p memory`

use contracts::coords::LocalOffset;
use contracts::delta::{DeltaRecord, RegionSlot};
use core::time::Duration;
use memory::{Slab, SLAB_1MB_RECORDS, SLAB_64KB_RECORDS};
use std::time::Instant;

/// Edits appended in the sustained run. Ten rounds of the 1 MB budget, so the bump-pointer reset
/// path is inside the measurement rather than assumed out of it.
const WRITES: usize = 1_000_000;

/// Latency samples taken individually. Every append is timestamped in this window because a `p99`
/// over a batch average is not a percentile; outside it, only the total is timed.
const SAMPLES: usize = 200_000;

fn record(index: usize) -> DeltaRecord {
    DeltaRecord::new(
        RegionSlot((index % 64) as u16),
        LocalOffset {
            x: (index % 4096) as u16,
            y: ((index / 4096) % 4096) as u16,
            z: ((index / 64) % 4096) as u16,
        },
        (index % 4096) as u16,
    )
}

fn percentile(sorted: &[Duration], p: usize) -> Duration {
    let index = (sorted.len() * p) / 100;
    let index = index.min(sorted.len() - 1);
    sorted[index]
}

fn main() {
    let mut buffer = vec![0u64; SLAB_1MB_RECORDS];
    let mut slab = Slab::new(&mut buffer);
    let mut stamps: Vec<Duration> = Vec::with_capacity(SAMPLES);
    let mut previous = Instant::now();
    let start = Instant::now();

    for index in 0..WRITES {
        if slab.is_full() {
            slab.reset();
        }
        slab.append(record(index)).expect("budget");
        if index < SAMPLES {
            let now = Instant::now();
            stamps.push(now.duration_since(previous));
            previous = now;
        }
    }
    let total = start.elapsed();

    let writes_per_second = (WRITES as f64) / total.as_secs_f64();
    stamps.sort_unstable();

    println!("// spike S2, part one — slab append cost. Measured, not asserted.");
    println!("host            : {}", std::env::consts::OS);
    println!("arch            : {}", std::env::consts::ARCH);
    println!("budget          : 1 MB = {SLAB_1MB_RECORDS} records (docs/03's other candidate is 64 KB = {SLAB_64KB_RECORDS})");
    println!(
        "writes          : {WRITES} appends, {:?} resets exercised",
        WRITES / SLAB_1MB_RECORDS
    );
    println!("total           : {total:?}");
    println!("writes/sec      : {writes_per_second:.0}   (gate P1.2 target: 1_000_000 sustained)");
    println!("p50             : {:?}", percentile(&stamps, 50));
    println!("p99             : {:?}", percentile(&stamps, 99));
    println!("p99.9           : {:?}", percentile(&stamps, 999 / 10));
    println!("max             : {:?}", stamps[stamps.len() - 1]);
    println!(
        "samples         : {} individually timestamped; per-append clock overhead is INCLUDED,",
        SAMPLES
    );
    println!("                  so these latencies are conservative, not the append's own cost.");

    // The same run through the 64 KB budget, because S2's actual question is which survives.
    let mut small = vec![0u64; SLAB_64KB_RECORDS];
    let mut slab = Slab::new(&mut small);
    let start = Instant::now();
    for index in 0..WRITES {
        if slab.is_full() {
            slab.reset();
        }
        slab.append(record(index)).expect("budget");
    }
    let small_total = start.elapsed();
    println!(
        "64 KB budget    : {small_total:?} = {:.0} writes/sec, {:?} resets",
        (WRITES as f64) / small_total.as_secs_f64(),
        WRITES / SLAB_64KB_RECORDS
    );
    println!(
        "gate P1.2       : {}",
        if writes_per_second >= 1_000_000.0 && percentile(&stamps, 99) < Duration::from_micros(1) {
            "MET on this machine — which is one machine, not the four of gate P1.5"
        } else {
            "NOT met here; reported as measured, not re-targeted"
        }
    );
}
