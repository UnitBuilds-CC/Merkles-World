//! P1.6's repair half, limited to the shape that needs no causal rule: a peer that is simply behind.
//!
//! `ledger::repair::first_divergence` locates a divergence; deciding which payload wins when two sides hold
//! different ones at the same voxel is OQ-46's question and ADR-004 defers it. What *can* be settled
//! without any policy is the lagging case - every record the lagging side holds, the leader holds verbatim,
//! and the leader holds more. Adopting the leader's records there cannot overwrite a competing write,
//! because there is no voxel where the two disagree.
//!
//! These tests pin the four things that make that claim safe rather than convenient: the adoption roots to
//! the leader's digest, the direction matters (the leader is not "behind"), a real disagreement is named
//! and refused, and a buffer that is too small is refused rather than silently truncated.

use ledger::repair::{catch_up, first_divergence, CatchUp};
use ledger::{EmptyLadder, Leaf};

/// One record, at a position distinct from every other in these tests: `x` alone separates them, since 97
/// is coprime with 4096 and `i` stays below 40.
fn leaf(index: u16, payload: u16) -> Leaf {
    Leaf {
        x: (index * 97) % 4096,
        y: (index * 211) % 4096,
        z: (index * 601) % 4096,
        payload,
        append: index as u32 + 1,
    }
}

/// A spread set across all eight top octants, so a root is a real traversal and not one branch's path.
fn leader_log() -> Vec<Leaf> {
    (0..40u16).map(|index| leaf(index, index * 7 + 1)).collect()
}

#[test]
fn a_lagging_peer_adopts_the_leaders_records_and_both_root_alike() {
    let ladder = EmptyLadder::compute();
    let leader = leader_log();
    // The lagging peer received every append but one.
    let laggard: Vec<Leaf> = leader
        .iter()
        .copied()
        .filter(|leaf| leaf.append != 17)
        .collect();
    assert_eq!(laggard.len(), leader.len() - 1);
    let roots_differ_before = {
        let mut a = laggard.clone();
        let mut b = leader.clone();
        ledger::region_root(&mut a, &ladder) != ledger::region_root(&mut b, &ladder)
    };
    assert!(roots_differ_before, "the fixture must actually diverge");

    let mut out = vec![Leaf::DEFAULT; leader.len()];
    let verdict = catch_up(&laggard, &leader, &mut out, &ladder);
    let CatchUp::CaughtUp { applied, root } = verdict else {
        panic!("a verbatim-subset lag must be adopted, got {verdict:?}");
    };
    assert_eq!(
        applied, 1,
        "exactly the missing append should have been applied"
    );

    let mut expected = leader.clone();
    assert_eq!(root, ledger::region_root(&mut expected, &ladder));
    // `region_root` partitions its slice in place - the same thing `first_divergence` warns about - so the
    // adopted buffer is the leader's records in *some* order, not their submission order. Comparing
    // element-by-element would have been a test of the walk's bookkeeping, which is the wrong claim; what
    // matters is that no record was added, dropped or edited.
    let mut adopted = out[..leader.len()].to_vec();
    let mut wanted = leader.clone();
    let by_position = |a: &Leaf, b: &Leaf| {
        (a.x, a.y, a.z, a.append, a.payload).cmp(&(b.x, b.y, b.z, b.append, b.payload))
    };
    adopted.sort_by(by_position);
    wanted.sort_by(by_position);
    assert_eq!(
        adopted, wanted,
        "the adopted set must be the leader's records exactly"
    );

    // And the walk agrees with the root: after the adoption there is nothing left to find.
    let mut left = out[..leader.len()].to_vec();
    let mut right = leader.clone();
    assert!(
        first_divergence(&mut left, &mut right, &ladder).is_none(),
        "catch_up claimed convergence while the bisection walk still finds a divergence"
    );
}

#[test]
fn the_leader_is_not_behind_and_the_other_direction_is_the_repair() {
    let ladder = EmptyLadder::compute();
    let leader = leader_log();
    let laggard: Vec<Leaf> = leader
        .iter()
        .copied()
        .filter(|leaf| leaf.append != 17)
        .collect();
    let mut out = vec![Leaf::DEFAULT; leader.len()];

    // Same pair, arguments swapped: the leader holds a record the laggard lacks, so *this* direction is not
    // a catch-up. The caller reads that as "ask the other way round", not as "reconciled".
    let verdict = catch_up(&leader, &laggard, &mut out, &ladder);
    assert_eq!(verdict, CatchUp::NotBehind);
    assert!(
        out.iter().all(|leaf| *leaf == Leaf::DEFAULT),
        "a refused direction must not have written anything"
    );
}

#[test]
fn a_genuine_disagreement_is_named_not_merged() {
    let ladder = EmptyLadder::compute();
    let mut leader = leader_log();
    let mut mine = leader_log();
    // Same position, same rank, different payload: last-write-wins has nothing to say here, so this is
    // OQ-46's case and the function's job is to point at it.
    let clash = 21usize;
    let at = leader[clash];
    leader[clash].payload = 0x0F01;
    mine[clash].payload = 0x0F02;

    let mut out = vec![Leaf::DEFAULT; leader.len()];
    let verdict = catch_up(&mine, &leader, &mut out, &ladder);
    let CatchUp::Disagrees {
        x,
        y,
        z,
        mine: my_payload,
        theirs,
    } = verdict
    else {
        panic!(
            "a payload clash at a shared voxel must be reported as a disagreement, got {verdict:?}"
        );
    };
    assert_eq!(
        (x, y, z),
        (at.x, at.y, at.z),
        "it must name the clashing voxel"
    );
    assert_eq!((my_payload, theirs), (0x0F02, 0x0F01));
}

#[test]
fn a_short_buffer_is_refused_rather_than_truncated() {
    let ladder = EmptyLadder::compute();
    let leader = leader_log();
    let laggard: Vec<Leaf> = leader
        .iter()
        .copied()
        .filter(|leaf| leaf.append != 17)
        .collect();
    // One record short of the leader's set: adopting into this would silently drop an append and root to a
    // state neither peer holds, which is the worst possible outcome of a reconciliation.
    let mut out = vec![Leaf::DEFAULT; leader.len() - 1];
    assert_eq!(
        catch_up(&laggard, &leader, &mut out, &ladder),
        CatchUp::NoRoom
    );
}

#[test]
fn identical_sides_need_no_repair_and_say_so_without_work() {
    let ladder = EmptyLadder::compute();
    let leader = leader_log();
    let mut out = vec![Leaf::DEFAULT; leader.len()];
    let verdict = catch_up(&leader, &leader, &mut out, &ladder);
    let CatchUp::CaughtUp { applied, root } = verdict else {
        panic!("equal sets are a catch-up with nothing to apply, got {verdict:?}");
    };
    assert_eq!(applied, 0);
    let mut other = leader.clone();
    assert_eq!(root, ledger::region_root(&mut other, &ladder));
}
