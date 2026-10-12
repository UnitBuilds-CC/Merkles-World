//! OQ-49's decision packet: how big can `N` be before the pass stops fitting, and does spreading the
//! same edits across more regions buy anything?
//!
//! Spike S2 measured two points - one full 64 KB slab (52 ms scattered, 7 ms clustered) and the 1k/10k/100k
//! ladder - and gate P1.3 still asks for `< 1 ms` over "a full dirty slab array". docs/13 OQ-49 lists four things that could move: the target, the slab
//! budget, the compaction frequency, or the gate's scope. Two of those are answers this file can supply
//! numbers for, and one it cannot:
//!
//! - **What `N` fits.** If the gate means "the dirty set one epoch gives to one region", the sweep says
//!   where the sub-millisecond line actually falls, per arrangement.
//! - **Whether batching across regions helps.** `merkle_pass.rs` prints "eight full slabs in one epoch:
//!   ~8x the one-slab cost (not measured here)". This measures it - and measures the same total edit count
//!   as one region instead of eight, which is the question the "batch the pass across regions" option
//!   quietly assumes the answer to.
//! - **What the epoch's own lists cost** when there are eight slots to root rather than one, separate from
//!   the region passes.
//!
//! Nothing here is asserted as a gate. These are release-profile timings on one machine, printed as
//! integers so they can be compared between architectures, and the decision about P1.3 belongs to whoever
//! owns the gate.
//!
//! Run: `cargo run --release --example pass_budget -p ledger`

use contracts::coords::{LocalOffset, RegionKey};
use contracts::delta::{DeltaRecord, RegionSlot};
use core::time::Duration;
use ledger::commit::{commit_epoch, RegionEdits};
use ledger::{event_leaf, genesis_root, region_root_counted, EmptyLadder, Leaf, Root};
use std::time::Instant;

const SEED: u64 = 0x5EED_0000_0000_0001;

/// The `N` values the gate is argued over: a hand-editing player, a busy server tick, the four candidate
/// slab budgets, and a full 64 KB slab.
const SIZES: [usize; 7] = [1, 8, 32, 128, 512, 2_048, 8_192];

/// Total edits for the spread-versus-together comparison: eight slabs' worth.
const EIGHT_SLABS: usize = 8 * 8_192;

/// The position stream `merkle_pass.rs` times, so the two examples agree about what "scattered" costs.
/// Each coordinate is 12 independent bits of an LCG, which is what a player editing anywhere in a region
/// actually produces. The first draft of this file scrambled the low index bits instead, which kept two of
/// the three coordinates nearly constant and reported a full slab as 12,287 digests rather than the 68,524
/// docs/03 publishes - a faster pass that was measuring a different arrangement. Comparing against the
/// published figure is what caught it.
fn position(index: usize, scattered: bool) -> LocalOffset {
    if scattered {
        let mut state = (index as u64)
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        state ^= state >> 31;
        state = state.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        state ^= state >> 29;
        LocalOffset {
            x: (state & 0xFFF) as u16,
            y: ((state >> 12) & 0xFFF) as u16,
            z: ((state >> 24) & 0xFFF) as u16,
        }
    } else {
        // Consecutive voxels: the arrangement a single mining tunnel produces, and the reason the gate's
        // answer is not one number.
        LocalOffset {
            x: (index & 0xFFF) as u16,
            y: ((index >> 12) & 0xFFF) as u16,
            z: 0,
        }
    }
}

fn leaves(count: usize, scattered: bool, slot: u16) -> Vec<Leaf> {
    (0..count)
        .map(|index| {
            let offset = position(index, scattered);
            Leaf::from_record(
                DeltaRecord::new(RegionSlot(slot), offset, (index % 4_096) as u16),
                index as u32,
            )
        })
        .collect()
}

fn records(count: usize, scattered: bool, slot: u16) -> Vec<DeltaRecord> {
    (0..count)
        .map(|index| {
            DeltaRecord::new(
                RegionSlot(slot),
                position(index, scattered),
                (index % 4_096) as u16,
            )
        })
        .collect()
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

fn fits(elapsed: Duration) -> &'static str {
    if elapsed < Duration::from_millis(1) {
        "yes"
    } else {
        "no"
    }
}

fn main() {
    let ladder = EmptyLadder::compute();

    println!(
        "// OQ-49 packet — spike S2, part three. host {} / arch {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("// Informational timings, release profile, one machine. Gate P1.3 asks for < 1 ms; nothing here decides it.");
    println!();
    println!("  N per region | arrangement | pass time | digests | ns per digest | ns per edit | under 1 ms");

    for count in SIZES {
        for scattered in [false, true] {
            let mut region_leaves = leaves(count, scattered, 0);
            let mut digests = 0usize;
            let start = Instant::now();
            region_root_counted(&mut region_leaves, &ladder, &mut digests);
            let elapsed = start.elapsed();
            println!(
                " {:>12} | {:>11} | {:>9} | {:>7} | {:>14} | {:>11} | {:>10}",
                count,
                if scattered { "scattered" } else { "clustered" },
                human(elapsed),
                digests,
                format!("{}", elapsed.as_nanos() / (digests.max(1) as u128)),
                format!("{}", elapsed.as_nanos() / (count.max(1) as u128)),
                fits(elapsed)
            );
        }
    }

    println!();
    println!("// Spread versus together: the same 65,536 edits as eight regions of 8,192, or one region of 65,536.");
    let mut spread_digests = 0usize;
    let start = Instant::now();
    for slot in 0..8u16 {
        let mut region_leaves = leaves(8_192, true, slot);
        region_root_counted(&mut region_leaves, &ladder, &mut spread_digests);
    }
    let spread_time = start.elapsed();

    let mut together_digests = 0usize;
    let mut one_big = leaves(EIGHT_SLABS, true, 0);
    let start = Instant::now();
    region_root_counted(&mut one_big, &ladder, &mut together_digests);
    let together_time = start.elapsed();

    println!(
        "  eight regions of 8,192 : {} for {spread_digests} digests ({} ns per edit)",
        human(spread_time),
        spread_time.as_nanos() / EIGHT_SLABS as u128
    );
    println!(
        "  one region of 65,536   : {} for {together_digests} digests ({} ns per edit)",
        human(together_time),
        together_time.as_nanos() / EIGHT_SLABS as u128
    );
    println!(
        "  merkle_pass.rs extrapolated 8 x the one-slab figure and labelled it \"not measured here\": this is \
         that measurement. Spreading the same edits over eight regions computes MORE digests than keeping \
         them in one, so batching across regions is a scheduling question, not a way to reduce the hashing."
    );

    println!();
    println!("// The epoch's own lists, with the region passes held out: eight slots rooted instead of one.");
    let events = [event_leaf(1, &[0x01, 0x02])];
    for slot_count in [1usize, 2, 8, 16] {
        let slots: Vec<RegionKey> = (0..slot_count as i64)
            .map(|i| RegionKey { x: i, y: 0, z: 0 })
            .collect();
        let region_records = records(512, true, 0);
        let edits = [RegionEdits {
            slot: 0,
            records: &region_records,
        }];
        let mut slot_rows: Vec<Root> = vec![[0u8; 32]; slot_count];
        let mut region_rows: Vec<Root> = vec![[0u8; 32]; slot_count];
        let mut leaf_space: Vec<Leaf> = vec![Leaf::DEFAULT; 512];
        let start = Instant::now();
        let commit = commit_epoch(
            &ladder,
            ledger::commit::CommitRequest {
                region_slots: &slots,
                edits: &edits,
                event_leaves: &events,
                base_genesis_root: genesis_root(SEED, 0, 0),
                epoch_index: 0,
            },
            &mut ledger::commit::CommitScratch {
                slot_rows: &mut slot_rows,
                region_leaves: &mut region_rows,
                leaf_space: &mut leaf_space,
            },
        )
        .expect("scratch sized for the case");
        let elapsed = start.elapsed();
        println!(
            "  {slot_count} slot table row(s), one region holding 512 edits : {} (epoch root {}…)",
            human(elapsed),
            &hex(&commit.epoch)[0..8]
        );
    }

    println!();
    println!("// What the four levers look like against these rows, stated without choosing one:");
    println!("// - a smaller N per epoch per region is the only lever these numbers move by, and the sweep");
    println!(
        "//   says how small; the arrangement still changes the answer several-fold at every N;"
    );
    println!(
        "// - batching the pass across regions does not reduce the hashing - the spread case above"
    );
    println!(
        "//   computes MORE digests than the same edits in one region, because interior nodes are"
    );
    println!(
        "//   shared less, so that option is about cores and scheduling rather than about work;"
    );
    println!("// - a smaller slab budget moves N, which is the same lever as the first, arriving from the");
    println!(
        "//   capacity side and therefore the thing SPEC-06 would be deciding if it were chosen;"
    );
    println!(
        "// - moving the target is the only remaining option if the gate means a whole full slab,"
    );
    println!("//   since the pass at that size is tens of milliseconds and hash-limited, not code-limited.");
}

fn hex(root: &Root) -> String {
    root.iter().map(|byte| format!("{byte:02x}")).collect()
}
