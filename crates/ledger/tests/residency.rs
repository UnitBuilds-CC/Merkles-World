//! Gate P1.4's first half: **re-entry restores all edits.** The pop-in half is a frame-time claim and is
//! measured in `examples/re_entry_cost.rs`, not asserted here.
//!
//! The property worth testing is the one a residency table can quietly break: a region is dropped on
//! capacity pressure, the player travels back, and the world they see must be the world they left — not
//! "the same edits in some order" but the same *root*, because the root is what a peer compares and what a
//! save replays against. Everything here therefore routes through `region_root_for`, the same path
//! `commit_epoch` takes, so a bookkeeping bug that loses or reorders a record surfaces as a digest that
//! does not match rather than as a plausible-looking list.

use contracts::coords::{LocalOffset, RegionKey};
use contracts::delta::{DeltaRecord, RegionSlot};
use ledger::commit::region_root_for;
use ledger::tier::{Admission, Slot, Store};
use ledger::{EmptyLadder, Leaf};

fn region(x: i64) -> RegionKey {
    RegionKey { x, y: 0, z: 0 }
}

/// `count` distinct edits inside one region, spread so the octree walks several paths rather than one.
fn edits(slot: u16, count: usize, offset: usize) -> Vec<DeltaRecord> {
    (0..count)
        .map(|i| {
            let n = (i + offset) % 4096;
            DeltaRecord::new(
                RegionSlot(slot),
                LocalOffset {
                    x: n as u16,
                    y: ((n * 7) % 4096) as u16,
                    z: ((n * 13) % 4096) as u16,
                },
                (n % 4096) as u16,
            )
        })
        .collect()
}

fn root_of(records: &[DeltaRecord], ladder: &EmptyLadder, scratch: &mut [Leaf]) -> [u8; 32] {
    region_root_for(records, scratch, ladder).expect("scratch sized for the run")
}

/// Three regions' worth of edits laid out in one arena, the way `M_cold` is append-only.
#[allow(clippy::type_complexity)]
fn arena_of(groups: &[Vec<DeltaRecord>]) -> (Vec<DeltaRecord>, Vec<(usize, usize)>) {
    let mut arena = Vec::new();
    let mut ranges = Vec::new();
    for group in groups {
        ranges.push((arena.len(), group.len()));
        arena.extend(group.iter().copied());
    }
    (arena, ranges)
}

#[test]
fn a_re_entered_region_recomputes_the_root_it_had_before_eviction() {
    let ladder = EmptyLadder::compute();
    let groups = vec![edits(0, 64, 0), edits(1, 64, 7), edits(2, 64, 19)];
    let (arena, ranges) = arena_of(&groups);

    // Four cells, two of which may be hot: the directory remembers, the budget limits visibility.
    let mut slots = [Slot::EMPTY; 4];
    let mut store = Store::new(&mut slots, 2);
    for (i, (start, len)) in ranges.iter().enumerate() {
        assert!(
            store.register(region(i as i64), *start, *len),
            "three regions fit in a four-cell directory"
        );
    }

    let mut scratch = vec![Leaf::DEFAULT; 64];
    let before: Vec<[u8; 32]> = ranges
        .iter()
        .map(|(start, len)| root_of(&arena[*start..*start + *len], &ladder, &mut scratch))
        .collect();

    // A walks in, B walks in, then C. With two hot slots, one of them has to go.
    store.admit(&region(0));
    store.tick();
    store.admit(&region(1));
    store.tick();
    assert_eq!(
        store.admit(&region(2)),
        Admission::Evicted { evicted: region(0) },
        "A was the least recently used hot region, so A is what goes"
    );
    assert!(!store.is_hot(&region(0)), "A is cold now");
    assert!(store.is_hot(&region(2)), "C is hot, which was the point");
    assert_eq!(
        store.edits_of(&region(0)),
        Some(ranges[0]),
        "cold A is still findable in the arena - that is the whole design"
    );

    // The player travels back to A. Same range, same root, nothing dropped on the way out.
    let restored = store
        .re_enter(&region(0))
        .expect("A is known to the table, so this is not a failed restore");
    assert_eq!(restored, ranges[0], "and at its exact range");
    let after_a = root_of(
        &arena[restored.0..restored.0 + restored.1],
        &ladder,
        &mut scratch,
    );
    assert_eq!(
        after_a, before[0],
        "re-entry restored a different edit set than the one that was evicted"
    );
    assert!(store.is_hot(&region(0)), "A is in view again");
    assert!(
        !store.is_hot(&region(1)),
        "B made room for it, and is merely cold"
    );
    let (b_start, b_len) = store.edits_of(&region(1)).expect("B is still tracked");
    assert_eq!(
        root_of(&arena[b_start..b_start + b_len], &ladder, &mut scratch),
        before[1],
        "a cold region's edits still root the same way"
    );
}

#[test]
fn re_admitting_a_region_already_in_view_is_not_reported_as_a_restore() {
    let mut slots = [Slot::EMPTY; 3];
    let mut store = Store::new(&mut slots, 2);
    assert!(store.register(region(0), 0, 5));
    assert_eq!(store.admit(&region(0)), Admission::Restored);
    assert_eq!(store.admit(&region(0)), Admission::AlreadyHot);
    assert_eq!(store.hot_count(), 1, "and no second region was displaced");
}

#[test]
fn a_restore_that_cannot_find_its_region_says_so_rather_than_returning_an_empty_one() {
    let mut slots = [Slot::EMPTY; 2];
    let mut store = Store::new(&mut slots, 1);
    assert!(store.register(region(0), 0, 3));
    assert_eq!(
        store.re_enter(&region(9)),
        None,
        "an unknown region must not silently restore as empty - that is TierRestoreIncomplete's case, and \
         an empty region is what a player would otherwise be shown"
    );
    assert_eq!(
        store.re_enter(&region(0)),
        Some((0, 3)),
        "a known one does restore"
    );
}

#[test]
fn an_empty_table_reports_no_room_instead_of_inventing_a_victim() {
    let mut slots: [Slot; 0] = [];
    let mut store = Store::new(&mut slots, 1);
    assert_eq!(store.admit(&region(0)), Admission::NoRoom);
    assert_eq!(store.hot_count(), 0);
    assert_eq!(store.cold_count(), 0);
    assert!(
        !store.register(region(3), 0, 4),
        "nothing can be tracked either"
    );
}

#[test]
fn a_directory_full_of_cold_regions_is_no_room_not_an_eviction() {
    // Every cell holds a remembered region and none is hot. Demoting cannot help: the loser's arena range
    // is the only thing pointing at its edits, so this is a refusal, not a swap.
    let mut slots = [Slot::EMPTY; 2];
    let mut store = Store::new(&mut slots, 1);
    assert!(store.register(region(0), 0, 4));
    assert!(store.register(region(1), 4, 4));
    assert_eq!(store.cold_count(), 2);
    assert_eq!(store.admit(&region(2)), Admission::NoRoom);
    assert_eq!(
        store.edits_of(&region(0)),
        Some((0, 4)),
        "and the refusal left both tracked regions intact"
    );
}

#[test]
fn a_zero_hot_budget_bounces_the_newcomer_instead_of_overspending() {
    // Nothing can be in view, so `admit` cannot honor a request by demoting someone else - the only hot
    // region is the one being admitted. Reporting the newcomer as evicted keeps the budget a hard
    // invariant; leaving it hot would make `hot_capacity` a suggestion.
    let mut slots = [Slot::EMPTY; 2];
    let mut store = Store::new(&mut slots, 0);
    assert!(store.register(region(0), 0, 4));
    assert_eq!(
        store.admit(&region(0)),
        Admission::Evicted { evicted: region(0) },
        "the caller's own region is what the budget rejected"
    );
    assert_eq!(store.hot_count(), 0, "and the budget still holds");
    assert_eq!(
        store.edits_of(&region(0)),
        Some((0, 4)),
        "rejected but not forgotten - the edits are still reachable"
    );
}

#[test]
fn regions_used_in_the_same_tick_evict_by_cell_order_and_oldest_use_beats_that_order() {
    // Two regions admitted without a tick between them share a `last_used`, so the only thing separating
    // them is the cell they sit in. Pinning that makes a replayed walk evict the same regions twice;
    // an LRU that ties arbitrarily is a determinism hazard wearing an LRU costume.
    let mut slots = [Slot::EMPTY; 3];
    let mut store = Store::new(&mut slots, 2);
    assert!(store.register(region(0), 0, 4));
    assert!(store.register(region(1), 4, 4));
    store.admit(&region(0));
    store.admit(&region(1));
    assert_eq!(
        store.admit(&region(2)),
        Admission::Evicted { evicted: region(0) },
        "tied on use, so the lower cell goes"
    );

    // Same shape one tick later: the tie is broken by age, not by index, and the older region is the one
    // that leaves.
    let mut slots = [Slot::EMPTY; 3];
    let mut store = Store::new(&mut slots, 2);
    assert!(store.register(region(0), 0, 4));
    assert!(store.register(region(1), 4, 4));
    store.admit(&region(0));
    store.tick();
    store.admit(&region(1));
    store.tick();
    assert_eq!(
        store.admit(&region(2)),
        Admission::Evicted { evicted: region(0) },
        "region(1) was used more recently, so region(0) is the LRU"
    );
}

#[test]
fn a_long_walk_never_breaks_the_hot_budget_and_every_region_still_comes_back_identical() {
    let ladder = EmptyLadder::compute();
    let groups: Vec<Vec<DeltaRecord>> = (0..8).map(|i| edits(i as u16, 8, i * 3)).collect();
    let (arena, ranges) = arena_of(&groups);

    let mut slots = [Slot::EMPTY; 8];
    let mut store = Store::new(&mut slots, 2);
    for (i, (start, len)) in ranges.iter().enumerate() {
        assert!(store.register(region(i as i64), *start, *len));
    }

    let mut scratch = vec![Leaf::DEFAULT; 8];
    let before: Vec<[u8; 32]> = groups
        .iter()
        .map(|group| root_of(group, &ladder, &mut scratch))
        .collect();

    for step in 0..40 {
        store.admit(&region((step % 8) as i64));
        store.tick();
        assert!(
            store.hot_count() <= 2,
            "the budget is a promise, not a suggestion"
        );
    }
    assert_eq!(
        store.hot_count() + store.cold_count(),
        8,
        "a walk this long must not forget a region"
    );

    for (i, expected) in before.iter().enumerate() {
        let (start, len) = store
            .re_enter(&region(i as i64))
            .unwrap_or_else(|| panic!("region {i} was forgotten by the directory"));
        assert_eq!(
            root_of(&arena[start..start + len], &ladder, &mut scratch),
            *expected,
            "region {i} came back as a different world"
        );
    }
}
