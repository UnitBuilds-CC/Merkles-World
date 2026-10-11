//! crates/memory — A3 Memory's slabs ([docs/03 § Slab allocation](../../docs/03-state-ledger.md)).
//!
//! A slab is an **append-only record buffer** for one region, since [ADR-029](../../docs/decisions.md)
//! took the slab out of the record's address: what a record carries is a region slot plus an
//! intra-region triple, so the buffer's entry count is bounded by its own size and not by a field
//! width. Two properties make it the hot path's only memory primitive, and both are preserved here
//! without an allocator and without `unsafe`:
//!
//! - **`O(1)` write.** One store of `DeltaRecord::as_u64()` past a bump pointer. No `malloc`, no GC,
//!   no OS memory-lock contention — which is what [AGENTS.md](../../AGENTS.md) §3 demands of the frame
//!   path.
//! - **`O(1)` mass reset.** At commit the bump pointer returns to 0 and the buffer is recycled,
//!   whether it held 10 records or 10,000,000. Freeing individual records is the thing this design
//!   must never have to do.
//!
//! **The buffer is the caller's.** `Slab` borrows a `&mut [u64]` rather than owning memory, because
//! who owns the arena — how many slabs, resident or pinned, tiered or evicted — is SPEC-06's open
//! question and gate P1.4's territory, not this type's. It also keeps this crate `no_std`.
//!
//! A deliberate divergence, logged rather than hidden: docs/03 § Slab allocation illustrates the
//! append as `core::ptr::write_unaligned`, while the workspace lints forbid `unsafe` in every crate
//! (`Cargo.toml` `[workspace.lints.rust]`). Both halves want the same guarantee — one store, no
//! allocator — so the safe form over a pre-allocated `&mut [u64]` is implemented, and the doc names
//! the disagreement. If a measurement ever shows the safe form costing the guarantee, that is spike
//! S2's evidence to bring, not a licence to quietly add `unsafe`.

#![no_std]

use contracts::delta::{DeltaRecord, RECORD_BYTES};

/// A region's append-only record buffer, borrowing storage the caller owns.
#[derive(Debug)]
pub struct Slab<'buffer> {
    records: &'buffer mut [u64],
    len: usize,
}

/// Why an append was refused: the buffer is at its own capacity.
///
/// Deliberately a crate-local type rather than a new `LedgerError` variant. A-09's band-3 taxonomy is
/// frozen-ish surface (docs/12: a contract change needs a migration note, a version bump and an ADR),
/// and the *number* it would carry — how many records a region may hold outstanding — is exactly what
/// [SPEC-06](../../docs/13-risks-open-questions.md) and spike S2 have not measured. Inventing a
/// variant now would bake an unmeasured limit into the shared error surface. Whoever freezes the
/// capacity maps this to band 3 with an ADR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlabFull {
    /// The buffer's capacity in records, which is `records.len()`.
    pub capacity: usize,
}

/// The two capacities docs/03 writes down **because nobody has measured which survives**: 64 KB holds
/// 8,192 records, 1 MB holds 131,072. These are budgets for a spike to compare, not a decision;
/// naming one of them `SLAB_CAPACITY` would be guessing in a type.
pub const SLAB_64KB_RECORDS: usize = (64 * 1024) / RECORD_BYTES;
/// See `SLAB_64KB_RECORDS`.
pub const SLAB_1MB_RECORDS: usize = (1024 * 1024) / RECORD_BYTES;

impl<'buffer> Slab<'buffer> {
    /// A slab over `records`, starting empty. Capacity is the buffer's length, in records.
    #[inline]
    pub const fn new(records: &'buffer mut [u64]) -> Self {
        Self { records, len: 0 }
    }

    #[inline]
    pub const fn capacity(&self) -> usize {
        self.records.len()
    }

    /// Records appended since the last `reset`.
    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub const fn is_full(&self) -> bool {
        self.len == self.records.len()
    }

    /// Bytes of records written so far — the size a streaming or hashing pass sees.
    #[inline]
    pub const fn bytes(&self) -> usize {
        self.len * RECORD_BYTES
    }

    /// One store past the bump pointer, returning the index it landed at.
    ///
    /// The value written is `DeltaRecord::as_u64()`, so the append and the addressing cannot drift
    /// apart: a slab is 8-byte-aligned records of one type and nothing else writes into it.
    #[inline]
    pub fn append(&mut self, record: DeltaRecord) -> Result<usize, SlabFull> {
        if self.len >= self.records.len() {
            return Err(SlabFull {
                capacity: self.records.len(),
            });
        }
        let index = self.len;
        self.records[index] = record.as_u64();
        self.len = index + 1;
        Ok(index)
    }

    /// The words appended, in append order. The slice is the interface to the rest of the engine:
    /// `crates/ledger` reads records back through `DeltaRecord::from_u64` and never reaches in here,
    /// which is what [AGENTS.md](../../AGENTS.md) §2's contract-only boundary means for two crates
    /// that both touch the same bytes.
    #[inline]
    pub fn records(&self) -> &[u64] {
        &self.records[..self.len]
    }

    /// The records as they really are: a re-decode of `records()`.
    #[inline]
    pub fn deltas(&self) -> impl Iterator<Item = DeltaRecord> + '_ {
        self.records().iter().copied().map(DeltaRecord::from_u64)
    }

    /// Bump-pointer reset: `O(1)` regardless of how much the region changed. The bytes themselves are
    /// left alone — they are garbage the next append overwrites, and zeroing them would be the
    /// per-record cost this design refuses to pay.
    #[inline]
    pub fn reset(&mut self) {
        self.len = 0;
    }
}
