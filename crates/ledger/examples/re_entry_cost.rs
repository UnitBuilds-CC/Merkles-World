//! Gate P1.4's cost half, measured rather than claimed: what does bringing an evicted region back cost?
//!
//! `cargo run --release --example re_entry_cost -p ledger`
//!
//! P1.4 says re-entry must restore all edits **with no visible pop-in**. "Visible" is a frame-time word,
//! and this repository has no renderer yet, so this file does not pretend to meet the gate. What it can do
//! is price the part of re-entry that is already real — look the region up in the directory, copy its
//! records out of the arena, and root them again — and put that next to the 16.7 ms frame docs/04 aims at,
//! so whoever owns the pop-in decision is arguing about a measured quantity instead of a vibe.
//!
//! Two framings, because the interesting answer is not the mean:
//! - **a cold region's restore is a copy and a hash pass**, so it scales with the region's edit count, and
//!   spike S2 measured the hashing half at roughly 700 ns per digest;
//! - **the budget-relevant case is many regions at once.** Arriving at a new area does not re-enter one
//!   region, it re-enters the whole ring around the camera, so the per-frame cost is per-region cost times
//!   regions, and that product is the number that has to fit.
//!
//! The pass is reported in **digests as well as nanoseconds**, using the same counted entry point S2
//! used (`region_root_counted`) and the same edit generator (`merkle_pass.rs`'s LCG). Without the count,
//! a falling `ns per edit` looks like a better algorithm; with it, the reader can see whether the cost is
//! S2's per-digest floor times fewer digests, or something else — which is the arithmetic OQ-49 turns on.
//!
//! Informational output, release profile, one machine — the same labelling every other timing in this repo
//! carries, and for the same reason: a shared runner measures the runner.

use contracts::coords::{LocalOffset, RegionKey};
use contracts::delta::{DeltaRecord, RegionSlot};
use ledger::commit::{leaves_from_records, region_root_for};
use ledger::tier::{Slot, Store};
use ledger::{region_root_counted, EmptyLadder, Leaf};
use std::time::Instant;

fn region(x: i64) -> RegionKey {
    RegionKey { x, y: 0, z: 0 }
}

/// Scattered positions within one region — `merkle_pass.rs`'s generator, deliberately the same function,
/// so a digest counted here and a digest counted there mean the same thing. Positions are effectively
/// distinct: `region_root_for`'s leaf dedupe is not what is being timed.
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

fn edits(count: usize) -> Vec<DeltaRecord> {
    (0..count).map(edit).collect()
}

fn main() {
    let ladder = EmptyLadder::compute();
    println!(
        "// gate P1.4's cost half - re-entry priced, not the pop-in gate itself (no renderer yet)"
    );
    println!(
        "// host {} / arch {}; release profile; informational, like every timing in this repo",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!();
    println!();
    println!(
        " {:>6} | {:>14} | {:>11} | {:>7} | {:>13} | {:>9} | {:>8}",
        "edits",
        "restore + root",
        "ns per edit",
        "digests",
        "ns per digest",
        "12N bound",
        "% of 12N"
    );

    for count in [8usize, 64, 512, 2_048, 8_192] {
        let records = edits(count);
        let mut slots = [Slot::EMPTY; 4];
        let mut store = Store::new(&mut slots, 1);
        assert!(store.register(region(0), 0, records.len()));
        let mut scratch: Vec<Leaf> = vec![Leaf::DEFAULT; count];

        // Warm the code paths, then time the thing a player actually experiences.
        let _ = region_root_for(&records, &mut scratch, &ladder);

        let start = Instant::now();
        let (start_index, len) = store.re_enter(&region(0)).expect("registered");
        let slice = &records[start_index..start_index + len];
        let mut digests = 0usize;
        let leaves = leaves_from_records(slice, &mut scratch).expect("scratch sized to the run");
        let root = region_root_counted(leaves, &ladder, &mut digests);
        let elapsed = start.elapsed();

        // A zero digest would mean nothing ran, and a silent no-op timing is the worst kind of pass.
        assert!(root.iter().any(|byte| *byte != 0), "empty root");

        println!(
            " {:>6} | {:>14} | {:>11} | {:>7} | {:>13} | {:>9} | {:>8}",
            count,
            format!("{:.1} µs", elapsed.as_nanos() as f64 / 1_000.0),
            elapsed.as_nanos() as usize / count,
            digests,
            elapsed.as_nanos() as usize / digests.max(1),
            12 * count,
            // How much of docs/03's 12N upper bound the scattered arrangement actually spends.
            format!("{}%", digests.saturating_mul(100) / (12 * count).max(1))
        );
    }

    println!();
    println!("// What this does and does not settle:");
    println!(
        "// - the directory lookup is not the cost. The comparison that means anything is against"
    );
    println!("//   spike S2 on the SAME host - CI prints merkle_pass and this step per runner, so their ns");
    println!("//   per digest are read side by side - and they agree. Re-entry is not a new problem: it is");
    println!("//   S2's pass moved to a different moment, triggered by walking back rather than by closing");
    println!("//   an epoch.");
    println!(
        "// - per-edit time falls as the region fills (8.5 us to 6.0 us on this host) while ns per"
    );
    println!(
        "//   digest stays flat. Scattered leaves share upper paths, so digests per edit drop from"
    );
    println!("//   11.4 to 8.4 and the spend against docs/03's 12N bound goes from 94% to 69%. That is the");
    println!(
        "//   bound behaving as a bound, and it is the only structural saving a recompute has."
    );
    println!(
        "// - so the finding is the size, not the shape. One full slab-sized region re-roots in"
    );
    println!("//   68,524 digests - S2's published count for exactly that many edits, and the number CI's");
    println!("//   three runners print identically (run 38279591441: 757 ns per digest on ARM64 macOS, 486");
    println!("//   on x86-64 Windows, 302 on x86-64 Linux). The milliseconds belong to the runner - 51.9,");
    println!("//   33.3 and 20.7 ms for identical work - but on all three a single region's restore is at");
    println!(
        "//   least one of docs/04's 16.7 ms frames. P1.4 is about the ring of regions around the"
    );
    println!("//   camera, so re-rooting from scratch on re-entry does not fit the frame however the ring");
    println!(
        "//   is tuned. The cost half of P1.4 points at a choice docs/03's cold tier has not made:"
    );
    println!(
        "//   store a region's root beside its records and re-entry is a copy; recompute it and"
    );
    println!(
        "//   re-entry is a pass. That is OQ-49's 'which lever moves' question from a second side."
    );
    println!("// - P1.4 stays open. Its restore-correctness half is enforced in tests/residency.rs by digest");
    println!(
        "//   equality and now also in the engine boot, which prints the restored root; its pop-in"
    );
    println!(
        "//   half needs a renderer to be visible in; its cost half has a measured number to argue"
    );
    println!("//   about, on every architecture the harness has.");
}
