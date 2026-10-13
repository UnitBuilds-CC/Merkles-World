//! What the P1.6 divergence walk actually costs, in subtree digests.
//!
//! `cargo run --release --example p16_walk_cost -p ledger`
//!
//! `repair.rs` carried a cost note that was a rationale rather than a measurement: the walk re-derives
//! children the root pass already paid for, "up to 8 subtree hashes per level per side". That is the shape
//! of the argument, not its size, and the module doc itself said the measurement belongs with the spike that
//! gates P1.6. So this measures it, against the thing a caller would otherwise do instead - root both sides
//! and notice they differ - because a walk is only worth running if it costs less than shipping the region.
//!
//! Integers only: digest counts, not microseconds. Timing a rarely-run reconciliation against a CPU hash
//! would be the wrong instrument, and milliseconds do not repeat across machines the way digest counts do.

use ledger::repair::{catch_up, first_divergence_counted, CatchUp};
use ledger::{region_root_counted, EmptyLadder, Leaf};

fn leaf(x: u16, y: u16, z: u16, payload: u16, append: u32) -> Leaf {
    Leaf {
        x,
        y,
        z,
        payload,
        append,
    }
}

/// A region's worth of edits, spread across all eight top octants so the walk has real branches to
/// descend: 512 records at positions coprime-strided across the 4096-axis, which keeps them distinct.
fn spread(count: u16) -> Vec<Leaf> {
    (0..count)
        .map(|i| {
            leaf(
                (i * 97) % 4096,
                (i * 211) % 4096,
                (i * 601) % 4096,
                (i % 4095) + 1,
                u32::from(i) + 1,
            )
        })
        .collect()
}

fn main() {
    let ladder = EmptyLadder::compute();
    println!(
        "// P1.6 walk cost, in subtree digests (no timings - those do not repeat across machines)"
    );
    println!(
        "// compare: rooting both sides is what a caller does anyway to notice a divergence at all"
    );
    println!();
    println!(" records | root-only digests | walk digests | walk / root-only | extra hashes");
    for count in [8u16, 64, 512, 2048] {
        let leader = spread(count);
        let mut laggard = leader.clone();
        // One voxel missing on the second side: the ordinary desync, and the case the walk is for.
        laggard.pop();

        let mut root_only = 0usize;
        let mut mine = leader.clone();
        let mut theirs = laggard.clone();
        region_root_counted(&mut mine, &ladder, &mut root_only);
        region_root_counted(&mut theirs, &ladder, &mut root_only);

        let mut walked = 0usize;
        let mut left = leader.clone();
        let mut right = laggard.clone();
        let found = first_divergence_counted(&mut left, &mut right, &ladder, &mut walked);
        let _ = found.expect("a dropped record must be locatable");

        let ratio = f64::from(walked as u32) / f64::from(root_only.max(1) as u32);
        println!(
            " {:>7} | {:>17} | {:>12} | {:>14.2} | {:>11}",
            count,
            root_only,
            walked,
            ratio,
            walked.saturating_sub(root_only),
        );
    }

    // The two paths the walk can take that cost nothing extra: identical sets return after the compare,
    // and a lag the caller can settle without any walk at all.
    let leader = spread(512);
    let mut same_left = leader.clone();
    let mut same_right = leader.clone();
    let mut digests = 0usize;
    assert!(
        first_divergence_counted(&mut same_left, &mut same_right, &ladder, &mut digests).is_none(),
        "identical record sets must not report a divergence"
    );
    println!();
    println!(
        "// agreeing sides: {digests} digests - exactly the two root passes, the walk never starts"
    );

    let laggard: Vec<Leaf> = leader.iter().copied().filter(|l| l.append != 257).collect();
    let mut out = vec![Leaf::DEFAULT; leader.len()];
    let mut before = 0usize;
    let mut a = leader.clone();
    let mut b = laggard.clone();
    region_root_counted(&mut a, &ladder, &mut before);
    region_root_counted(&mut b, &ladder, &mut before);
    let verdict = catch_up(&laggard, &leader, &mut out, &ladder);
    let CatchUp::CaughtUp { applied, root: _ } = verdict else {
        panic!("a verbatim-subset lag must be adopted, got {verdict:?}");
    };
    let mut expected = leader.clone();
    let mut after = 0usize;
    let root_check = region_root_counted(&mut expected, &ladder, &mut after);
    println!(
        "// the lag case needs no walk at all: catch_up applied {applied} record(s) and reached the \
         leader's root ({}), after {before} digests to notice and {after} to verify",
        &hex(&root_check)[..16],
    );
}

fn hex(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
