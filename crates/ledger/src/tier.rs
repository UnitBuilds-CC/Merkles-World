//! Region residency: the `M_active` / `M_cold` split of docs/03 § Tiering and eviction, as bookkeeping a
//! caller can be held to.
//!
//! Gate **P1.4** claims LRU eviction plus region re-entry restores all edits with no visible pop-in. The
//! pop-in half is a frame-time claim and belongs to a renderer that does not exist yet. The other half is
//! not: a region that was dropped must come back with *exactly the edits it had*, and that is a property
//! this module makes checkable — re-admit an evicted region, re-root its records, compare against the root
//! taken before it was dropped. If they differ, the bookkeeping lost something, and the player has arrived
//! at a different world than the one they left.
//!
//! **Two capacities, not one.** `hot_capacity` is how many regions may be `M_active` at once — the VRAM
//! and mirror budget. `runs.len()` is how many regions the table can *remember at all*, cold included.
//! Collapsing them is the bug this module had first draft: a table sized by hot slots could not record a
//! third region, so evicting it dropped its location and re-entry had nothing to restore — P1.4 failing
//! in the exact way the gate exists to catch, and failing quietly. Cold therefore means unreachable
//! without a restore pass, never forgotten.
//!
//! **Why bookkeeping and not a data mover.** `crates/ledger` is `#![no_std]` with no allocator on purpose:
//! the same code has to root an epoch inside a browser (gate P1.5). Nothing here copies records between
//! tiers. Records live once, in an arena the caller owns, and a [`Run`] says where a region's edits are and
//! whether it is hot. `re_enter` is the restore pass, and its cost is what the pop-in half will eventually
//! have to fit in a frame.
//!
//! Not decided here, because this module does not own it: the interaction radius that sets
//! `hot_capacity`, what persists to disk and in what form ([OQ-23]), and whether a cold region's edits may
//! eventually be compacted away — this type assumes the arena keeps them, which is docs/03's RAM-tier
//! contract and not a disk claim.

use contracts::coords::RegionKey;

/// One region's record range inside the caller's arena, plus its residency state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub key: RegionKey,
    /// First index of this region's records in the arena.
    pub start: usize,
    /// How many records belong to the region.
    pub len: usize,
    /// Whether the region is currently in `M_active`.
    pub hot: bool,
    /// The value of [`Store::tick`] when this region was last used. LRU reads this.
    pub last_used: u64,
}

/// One table cell. Public because [`Store::new`] borrows a slice of them: with no allocator, the storage
/// has to be the caller's. Read residency through [`Store`], not through this.
///
/// `RegionKey` has no sentinel that is not also a legal region — (0,0,0) *is* region zero, a real place —
/// so occupancy is a flag rather than a magic coordinate.
#[derive(Debug, Clone, Copy)]
pub struct Slot {
    taken: bool,
    run: Run,
}

impl Slot {
    /// A free cell. Bring an array of these — one per region you want remembered, hot or cold — to
    /// [`Store::new`].
    pub const EMPTY: Slot = Slot {
        taken: false,
        run: Run {
            key: RegionKey { x: 0, y: 0, z: 0 },
            start: 0,
            len: 0,
            hot: false,
            last_used: 0,
        },
    };
}

/// What an admission did. Every variant is observable, because "nothing happened" and "something was
/// evicted to make this happen" are different worlds to a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The region was already hot; only its timestamp moved.
    AlreadyHot,
    /// The region is hot now and no other region was demoted.
    Restored,
    /// The region is hot and `evicted` was demoted to make room. `evicted` stays in the table, cold.
    Evicted { evicted: RegionKey },
    /// The table could not hold the newcomer at all. With no cells this is the empty-table case; a full
    /// table is not `NoRoom` — it is an eviction, which is the point of having a directory.
    NoRoom,
}

/// A residency directory with a bounded hot set, over a caller-owned arena.
pub struct Store<'a> {
    slots: &'a mut [Slot],
    hot_capacity: usize,
    clock: u64,
}

impl<'a> Store<'a> {
    /// A table over `slots` cells, of which at most `hot_capacity` may be `M_active` at once. The cell
    /// count is the memory budget; `hot_capacity` is the visible-world budget.
    pub fn new(slots: &'a mut [Slot], hot_capacity: usize) -> Self {
        let mut store = Self {
            slots,
            hot_capacity,
            clock: 0,
        };
        store.reset();
        store
    }

    /// Forget every region — used when the arena beneath the table is recycled.
    pub fn reset(&mut self) {
        for slot in self.slots.iter_mut() {
            *slot = Slot::EMPTY;
        }
        self.clock = 0;
    }

    /// Advance the logical clock. The caller's tick loop decides what "a while ago" means; LRU ordering is
    /// only meaningful against this counter.
    pub fn tick(&mut self) {
        self.clock += 1;
    }

    /// Record where a region's edits live. Registering twice updates the range, which is what a commit
    /// that grew a region's edit list looks like. `false` means the *directory* is full — a different
    /// condition from being hot, and the reason this returns rather than evicting.
    pub fn register(&mut self, key: RegionKey, start: usize, len: usize) -> bool {
        let now = self.clock;
        if let Some(existing) = self.find_mut(&key) {
            existing.run.start = start;
            existing.run.len = len;
            existing.run.last_used = now;
            return true;
        }
        if let Some(free) = self.slots.iter_mut().find(|s| !s.taken) {
            free.taken = true;
            free.run = Run {
                key,
                start,
                len,
                hot: false,
                last_used: now,
            };
            return true;
        }
        false
    }

    /// Mark a region used this clock. An unknown region is ignored rather than invented: a table that
    /// grows entries as a side effect of a read cannot be reasoned about at commit time.
    pub fn use_region(&mut self, key: &RegionKey) {
        let now = self.clock;
        if let Some(slot) = self.find_mut(key) {
            slot.run.last_used = now;
        }
    }

    /// Bring a region into `M_active`, demoting the least-recently-used hot region if the hot budget is
    /// already spent. An unregistered region is admitted with an empty range: a region with no edits is
    /// normal, and it still occupies the camera.
    pub fn admit(&mut self, key: &RegionKey) -> Admission {
        let now = self.clock;
        let index = match self.find_mut(key) {
            Some(_) => self.index_of(key).expect("found a moment ago"),
            None => match self.slots.iter().position(|s| !s.taken) {
                Some(free) => {
                    self.slots[free].taken = true;
                    self.slots[free].run = Run {
                        key: *key,
                        start: 0,
                        len: 0,
                        hot: false,
                        last_used: now,
                    };
                    free
                }
                // Every cell holds a *remembered* region and none of them is this one. Nothing can be
                // evicted to fix this: the oldest cold region's arena range is what would be lost.
                None => return Admission::NoRoom,
            },
        };

        let was_hot = self.slots[index].run.hot;
        self.slots[index].run.hot = true;
        self.slots[index].run.last_used = now;

        let Some(evicted) = self.trim_hot_except(index) else {
            return if was_hot {
                Admission::AlreadyHot
            } else {
                Admission::Restored
            };
        };
        Admission::Evicted { evicted }
    }

    /// Re-enter an evicted region: make it hot and return the arena range its edits live at. `None` means
    /// this table never knew the region, which is the condition
    /// `LedgerError::TierRestoreIncomplete` exists to report — a restore that cannot find its subject must
    /// say so rather than handing back an empty region, which would look like "nobody edited here".
    pub fn re_enter(&mut self, key: &RegionKey) -> Option<(usize, usize)> {
        let range = self.edits_of(key)?;
        if let Admission::NoRoom = self.admit(key) {
            return None;
        }
        Some(range)
    }

    /// Demote hot regions past the budget, preferring to keep `except` hot, and return whoever was
    /// demoted. At most one region can be over budget per `admit`, so this demotes once or not at all.
    ///
    /// Ties break to the lower cell index, so two runs of one access sequence evict the same regions: an
    /// LRU that ties arbitrarily is a determinism hazard wearing an LRU costume. A hot budget of zero is
    /// the one case where the newcomer is the only candidate, and it bounces — `admit` then reports the
    /// caller's own key as evicted rather than silently leaving the budget overspent.
    fn trim_hot_except(&mut self, except: usize) -> Option<RegionKey> {
        if self.hot_count() <= self.hot_capacity {
            return None;
        }
        let victim = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.taken && slot.run.hot)
            .min_by_key(|(index, slot)| (usize::from(*index == except), slot.run.last_used, *index))
            .map(|(index, _)| index)?;
        self.slots[victim].run.hot = false;
        Some(self.slots[victim].run.key)
    }

    /// The hot region with the oldest `last_used`, as a cell index. Ties go to the lower index, as above.
    pub fn lru(&self) -> Option<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.taken && slot.run.hot)
            .min_by_key(|(index, slot)| (slot.run.last_used, *index))
            .map(|(index, _)| index)
    }

    pub fn is_hot(&self, key: &RegionKey) -> bool {
        self.find(key).is_some_and(|slot| slot.run.hot)
    }

    /// Where a region's edits are, hot or cold. Cold is not gone, and a caller that conflates them will not
    /// survive the P1.4 tests.
    pub fn edits_of(&self, key: &RegionKey) -> Option<(usize, usize)> {
        self.find(key).map(|slot| (slot.run.start, slot.run.len))
    }

    pub fn hot_count(&self) -> usize {
        self.slots.iter().filter(|s| s.taken && s.run.hot).count()
    }

    pub fn cold_count(&self) -> usize {
        self.slots.iter().filter(|s| s.taken && !s.run.hot).count()
    }

    pub fn hot_capacity(&self) -> usize {
        self.hot_capacity
    }

    /// Every tracked region, in cell order — a scan, not a hash, so the order is reproducible.
    pub fn snapshot(&self) -> impl Iterator<Item = Run> + '_ {
        self.slots.iter().filter(|s| s.taken).map(|s| s.run)
    }

    fn find(&self, key: &RegionKey) -> Option<&Slot> {
        self.slots.iter().find(|s| s.taken && s.run.key == *key)
    }

    fn find_mut(&mut self, key: &RegionKey) -> Option<&mut Slot> {
        self.slots.iter_mut().find(|s| s.taken && s.run.key == *key)
    }

    fn index_of(&self, key: &RegionKey) -> Option<usize> {
        self.slots.iter().position(|s| s.taken && s.run.key == *key)
    }
}
