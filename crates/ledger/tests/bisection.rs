//! Gate **P1.6**: "two clients with one divergent voxel detect it via 32-byte root exchange and
//! repair by bisection" (docs/03 § Validation gates, [ADR-023]). The detection half is
//! `contracts::errors::RootCompare`; this suite covers the bisection half, which is what makes the
//! desync localisable instead of merely noticed.
//!
//! Repair — choosing a winner — is deliberately untested here: that is [OQ-46]'s causal question and
//! ADR-004 defers it with multiplayer. What these tests do pin is the property the whole method rests
//! on, in both directions: the walk reports a divergence **exactly when** the roots differ, and it
//! names the voxel that actually changed.
//!
//! [OQ-46]: https://github.com/UnitBuilds-CC/Merkles-World/blob/main/docs/13-risks-open-questions.md

use ledger::repair::first_divergence;
use ledger::{EmptyLadder, Leaf};

fn leaf(x: u16, y: u16, z: u16, payload: u16) -> Leaf {
    Leaf {
        x,
        y,
        z,
        payload,
        append: 0,
    }
}

/// A base set spread across all eight top octants, so a descent has somewhere to go at every level.
fn base() -> Vec<Leaf> {
    let mut set = Vec::new();
    for index in 0..64u32 {
        set.push(leaf(
            ((index * 61) % 4096) as u16,
            ((index * 383) % 4096) as u16,
            ((index * 1021) % 4096) as u16,
            (index % 4095) as u16,
        ));
    }
    // The far corner — one of the two positions the shape is most likely to get wrong. The origin
    // is already in the loop's first entry, and a duplicate position here would need distinct
    // `append`s to be well ordered (see `an_unranked_pair_is_the_caller_not_the_tree`).
    set.push(leaf(4095, 4095, 4095, 0x0ABC));
    set
}

#[test]
fn agreement_on_the_root_means_no_divergence() {
    let ladder = EmptyLadder::compute();
    let mut local = base();
    let mut remote = base();
    assert!(first_divergence(&mut local, &mut remote, &ladder).is_none());
}

#[test]
fn a_reordered_set_is_not_a_divergence() {
    // The position-keyed property, on the repair side: shuffling one peer's arrival order changes
    // nothing, so the walk must not report the same world as desynced.
    let ladder = EmptyLadder::compute();
    let mut local = base();
    let mut remote = base();
    remote.reverse();
    assert!(first_divergence(&mut local, &mut remote, &ladder).is_none());
}

#[test]
fn one_changed_payload_is_found_at_its_own_voxel() {
    let ladder = EmptyLadder::compute();
    let mut local = base();
    let mut remote = base();
    // A deep interior position, not one of the extremes.
    let (x, y, z) = (3000u16, 2000u16, 1000u16);
    local.push(leaf(x, y, z, 0x2));
    remote.push(leaf(x, y, z, 0x3));
    let found = first_divergence(&mut local, &mut remote, &ladder).expect("roots differ");
    assert_eq!(
        found,
        ledger::repair::Divergence {
            x,
            y,
            z,
            local: Some(0x2),
            remote: Some(0x3)
        }
    );
}

#[test]
fn an_edit_one_side_never_received_is_the_other_being_absent() {
    let ladder = EmptyLadder::compute();
    let mut local = base();
    let mut remote = base();
    local.pop(); // the far corner only exists on one side
    let found = first_divergence(&mut local, &mut remote, &ladder).expect("roots differ");
    assert_eq!((found.x, found.y, found.z), (4095, 4095, 4095));
    assert_eq!(
        found.local, None,
        "this side popped the record: absence, not a zero payload"
    );
    assert_eq!(found.remote, Some(0x0ABC));
}

#[test]
fn the_walk_names_the_voxel_that_changed_for_every_position_it_is_shown_at() {
    // Sixteen single-voxel mutations across the region, each expected to be reported at its own
    // coordinate. A walk that bisected by arrival order rather than by path would fail here.
    let ladder = EmptyLadder::compute();
    for probe in 1..=16u32 {
        let position = (
            ((probe * 977) % 4096) as u16,
            ((probe * 601) % 4096) as u16,
            ((probe * 131) % 4096) as u16,
        );
        let mut local = base();
        let mut remote = base();
        local.push(leaf(position.0, position.1, position.2, 0x7));
        remote.push(leaf(position.0, position.1, position.2, 0x8));
        let found = first_divergence(&mut local, &mut remote, &ladder)
            .unwrap_or_else(|| panic!("probe {probe} at {position:?} was not found"));
        assert_eq!((found.x, found.y, found.z), position, "probe {probe}");
        assert_eq!((found.local, found.remote), (Some(0x7), Some(0x8)));
    }
}

/// docs/03 says the root is *not* arrival-order-independent for two records on one voxel — the
/// later one wins — and the rule only means anything if the caller ranked them. An unranked pair is
/// the caller's bug, and this shows both halves: ranked, order cannot change the root; unranked, it
/// can, which is why `leaf_winner` asserts against it in debug builds.
#[test]
fn ranked_duplicates_are_order_independent_and_unranked_ones_are_not() {
    let ladder = EmptyLadder::compute();
    let position = (100u16, 200u16, 300u16);

    let ranked = |swapped: bool| {
        let mut leaves = base();
        let (first, second) = if swapped {
            (
                Leaf {
                    x: position.0,
                    y: position.1,
                    z: position.2,
                    payload: 0x9,
                    append: 1,
                },
                Leaf {
                    x: position.0,
                    y: position.1,
                    z: position.2,
                    payload: 0x8,
                    append: 0,
                },
            )
        } else {
            (
                Leaf {
                    x: position.0,
                    y: position.1,
                    z: position.2,
                    payload: 0x8,
                    append: 0,
                },
                Leaf {
                    x: position.0,
                    y: position.1,
                    z: position.2,
                    payload: 0x9,
                    append: 1,
                },
            )
        };
        leaves.push(first);
        leaves.push(second);
        ledger::region_root(&mut leaves, &ladder)
    };
    assert_eq!(
        ranked(false),
        ranked(true),
        "ranked by append, arrival order cannot move the root"
    );
    assert_eq!(
        ranked(true),
        {
            // and the winner is the higher-ranked record's payload, not the last one written
            let mut single = vec![leaf(100, 200, 300, 0x9)];
            let mut padded = base();
            padded.append(&mut single);
            ledger::region_root(&mut padded, &ladder)
        },
        "the greatest append wins"
    );

    let unranked = |swapped: bool| {
        let mut leaves = base();
        let pair = if swapped {
            [leaf(100, 200, 300, 0x9), leaf(100, 200, 300, 0x8)]
        } else {
            [leaf(100, 200, 300, 0x8), leaf(100, 200, 300, 0x9)]
        };
        leaves.extend_from_slice(&pair);
        ledger::region_root(&mut leaves, &ladder)
    };
    assert_ne!(
        unranked(false),
        unranked(true),
        "two records with the same append are genuinely ambiguous — the tree resolves them by run          order, so an unranked input pair is the caller's determinism bug, not the shape's"
    );
}

#[test]
fn divergence_and_root_equality_never_disagree() {
    // The soundness claim, stated as a property: for both agreeing and differing sets, the walk
    // returns Some exactly when the two region roots differ.
    let ladder = EmptyLadder::compute();
    let mutations = [0u16, 1, 2, 7, 0x0ABC, 0xFFF];
    for mine in mutations {
        for theirs in mutations {
            let mut local = base();
            let mut remote = base();
            local.push(leaf(1234, 567, 3000, mine));
            remote.push(leaf(1234, 567, 3000, theirs));
            let roots_differ = ledger::region_root(&mut local, &ladder)
                != ledger::region_root(&mut remote, &ladder);
            let mut local_two = local.clone();
            let mut remote_two = remote.clone();
            let walked = first_divergence(&mut local_two, &mut remote_two, &ladder);
            assert_eq!(
                roots_differ,
                walked.is_some(),
                "payloads {mine:#04x} / {theirs:#04x}: roots differ {roots_differ}, walk said {walked:?}"
            );
        }
    }
}
