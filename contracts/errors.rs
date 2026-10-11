//! A-09 — error taxonomy and determinism violation codes. **DRAFT**, in the specific sense that
//! the split is deliberate: the codes and their bands are final for this draft (they are what CI
//! routes on), while every payload width named in a `// pending` note below waits on the artifact
//! that owns it — A-02/A-08 for module and wire identities, A-05 for event tags, A-07 for source
//! spans. Coordinates are no longer on that list: A-01 (`coords.rs`) froze on 2026-10-11, so the
//! plain-integer payloads here adopt its newtypes as a scheduled migration (AGENTS.md §2), not as
//! an unresolved dependency.
//!
//! Why this file exists: docs/12-agent-operations.md routes a *classified* failure back to the
//! owning agent with a fixed prompt. Classification must therefore be a machine-comparable
//! integer, not a string: every class in this taxonomy owns exactly one stable `u16` code.
//!
//! Rules this contract keeps:
//! - **Integer codes only.** A code is what peers and CI logs compare, so it is a literal
//!   per variant — never a string hash, never derived from declaration order (inserting a
//!   variant must not renumber its neighbours).
//! - **Banded.** Each subsystem owns one 512-code band, listed in [`bands`]. Band = owning
//!   crate in docs/12's layout (coords, ledger, memory, render, sim, circuits, physics, host,
//!   asset, net); band arithmetic is how CI picks the agent without a lookup table.
//! - **Determinism is its own class.** AGENTS.md §4 and ADR-017 put a hard boundary around
//!   values a root depends on, so a violation of it is not a subsystem bug: it is routed
//!   differently (docs/11's Bitwise Determinism Suite, and AGENTS.md §9 escalation).
//! - **Hot path clean (AGENTS.md §3).** Nothing here allocates, formats, or parses. Every
//!   [`EngineError::code`], [`DeterminismViolation::code`], [`bands`] helper and
//!   [`RootCompare::differs`] is a `const fn` of integer math. The
//!   `Display` and `Error` impls are cold-path (they run once per failure, after it).
//! - **Core-only, zero dependencies.** No `std::` item is named, so the same file compiles as a
//!   standalone lib and inside the `#![no_std]` contracts crate: no clock, no filesystem, no
//!   process, no random-seed map. Verified on `wasm32-unknown-unknown`.
//!
//! Coordinate newtypes are *not* defined here: `GlobalVoxelPos`, `RegionKey`, `SlabKey`,
//! `SectorKey`, `LocalOffset` belong to `contracts/coords.rs` (artifact A-01). Until that file
//! freezes, payloads are plain `u16`/`u32` integers, opaque `[u8; 32]` digests, and the two
//! payload structs below — never a coordinate tuple.
//! A-09 adopts the A-01 newtypes in one migration when coords.rs freezes (AGENTS.md §2:
//! migration note + version bump + ADR entry); that migration renames payload field *types*,
//! never a code, so no CI log or peer comparison breaks.
//!
//! Adding, removing, or re-bandinging a class is a contract change: it needs an ADR and the
//! System Architect's approval (docs/12 merge rule 2, ADR-025). This enum is deliberately
//! **not** `#[non_exhaustive]`: a new class must fail to compile in every router, so no error
//! can silently fall through to the generic retry path.

/// Band geometry for every code in this taxonomy (docs/12 routing key).
///
/// A code is `band * SIZE + offset`. Band 0 is unassigned; bands 1..=11 are assigned below;
/// 12..=127 are free for future subsystems (`u16` holds 127 full bands of 512).
pub mod bands {
    /// Code points owned by one subsystem; no variant may emit outside its own band.
    pub const SIZE: u16 = 512;
    /// Whole of band 0: an unrecognised code from a newer build lands here and is never emitted.
    pub const UNKNOWN: u16 = 0;
    /// Offset 0 inside every band: "this subsystem, class unknown to this build" — reserved, so
    /// a code equal to a band base is never emitted by a variant.
    pub const OFFSET_UNKNOWN: u16 = 0;
    /// Cross-cutting determinism class (AGENTS.md §4, ADR-017); routed ahead of subsystem bands.
    pub const DETERMINISM: u16 = 1;
    /// `crates/coords` — doc 02, gates C1–C4.
    pub const COORDS: u16 = 2;
    /// `crates/ledger` — doc 03, gates P1.1–P1.6.
    pub const LEDGER: u16 = 3;
    /// `crates/memory` — doc 03 slabs/tiering, ADR-008/009.
    pub const MEMORY: u16 = 4;
    /// `crates/render` — doc 04, gates R1–R5.
    pub const RENDER: u16 = 5;
    /// `crates/sim` — doc 05, gates SM1–SM6.
    pub const SIM: u16 = 6;
    /// `crates/circuits` — doc 06, gates E1–E6.
    pub const CIRCUITS: u16 = 7;
    /// `crates/physics` — doc 07, gates PH1/AU1/AN1/DE1.
    pub const PHYSICS: u16 = 8;
    /// `crates/host` — doc 08, gates W1–W5.
    pub const HOST: u16 = 9;
    /// `crates/assetc` — doc 09, gates AS1–AS5; tool-side, never frame-side.
    pub const ASSET: u16 = 10;
    /// `crates/net` — doc 10, gates M1–M7; reserved now even though multiplayer is deferred
    /// (ADR-004) so the Phase 1.5 build cannot renumber anything.
    pub const NET: u16 = 11;
    /// Lowest unassigned band; take bands from here upward, never below an assigned one.
    pub const RESERVED_FIRST: u16 = 12;
    /// Highest band a `u16` can carry at this band size.
    pub const LAST: u16 = 127;

    /// First code of a band; overflow is a bug in band assignment, not a runtime condition.
    pub const fn base(band: u16) -> u16 {
        band * SIZE
    }

    /// The band a code belongs to — the routing decision docs/12's CI loop makes.
    pub const fn band_of(code: u16) -> u16 {
        code / SIZE
    }

    /// The offset of a code inside its band.
    pub const fn offset_of(code: u16) -> u16 {
        code % SIZE
    }
}

// Band assignment is a compile-time fact, not a test-time one. These assertions are why the
// taxonomy needs no runtime guard: taking a band past `LAST`, or pushing `RESERVED_FIRST` down
// onto an assigned band, fails the build rather than surfacing as a code collision later.
const _: () = assert!(bands::RESERVED_FIRST > bands::NET);
const _: () = assert!(((bands::LAST as u32 + 1) * bands::SIZE as u32) - 1 == u16::MAX as u32);

/// Width of every root this contract compares: BLAKE3-256, 32 bytes (ADR-028, doc 03 § Digest).
pub const ROOT_BYTES: usize = 32;

/// The evidence pair a root comparison carries: two opaque 32-byte digests (ADR-023, ADR-028).
///
/// Deliberately root-bytes-only — no `RegionKey`, no slab addressing — because which region
/// disagreed is the outcome of the bisection walk, not an input to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootCompare {
    /// Digest computed locally.
    pub local: [u8; ROOT_BYTES],
    /// Digest presented by the peer, the replay, or the second build target.
    pub remote: [u8; ROOT_BYTES],
}

impl RootCompare {
    /// True when the digests differ; fixed-width integer byte compare, no allocation (AGENTS.md §3).
    pub const fn differs(&self) -> bool {
        let mut i = 0usize;
        let mut found = false;
        while i < ROOT_BYTES {
            if self.local[i] != self.remote[i] {
                found = true;
            }
            i += 1;
        }
        found
    }
}

/// Location in an authoring source file, as doc 09 requires of `assetc` errors ("source file +
/// line + column"), because those messages are read by agents as well as humans.
///
// pending OQ-30 / A-07: the `.fbs` schema and span encoding are undecided; if spans are ever
// emitted as packed offsets rather than (line, column), these fields change shape — the codes do not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
    /// 1-based line in the authoring file.
    pub line: u32,
    /// 1-based column in the authoring file.
    pub column: u32,
    /// Byte offset from the start of the buffer that was parsed.
    pub byte_offset: u32,
}

/// The class AGENTS.md §4 and ADR-017 create: a value that some root depends on failed to be
/// bitwise reproducible, or something that must not feed a root did.
///
/// Each variant is distinguishable because docs/12's CI loop routes it to a different owner:
/// a live peer mismatch goes to the net/ledger repair path, a suite divergence goes back to the
/// agent that owns the math, and a lint-caught float or transcendental never reaches a root at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeterminismViolation {
    /// A peer's epoch root differs from ours (gate P1.6, gate M1, ADR-023).
    // pending SPEC-08: which epoch is "the" epoch under quorum-or-timer is undecided, so this
    // carries digests only and no epoch index; A-08/doc 10 decides whether an epoch id joins it.
    EpochRootMismatch {
        /// The two 32-byte roots that disagree (ADR-028).
        roots: RootCompare,
    },
    /// Bisection localised the disagreement to one region branch (ADR-023, doc 10 § Verification).
    // the widths are no longer the blocker: ADR-029 (locked 2026-10-11) fixes them and frozen A-01
    // supplies the branch key. What is still missing is A-03's epoch region-slot table and
    // SPEC-06's slab geometry, so no addressing payload is committed here yet.
    RegionRootMismatch {
        /// The two subtree roots that disagree at the located branch.
        roots: RootCompare,
    },
    /// The same seed and transaction sequence replayed to different roots (gate SM3, doc 11 snapshot replay).
    SameSeedReplayDivergence {
        /// The two roots produced by the two runs.
        roots: RootCompare,
    },
    /// Two Bitwise Determinism Suite targets produced different bytes for identical input (gate P1.5, spike S3, ADR-017).
    CrossTargetDivergence {
        /// The two target digests, compared without assuming which is canonical.
        roots: RootCompare,
    },
    /// A mod's result differs across toolchains or targets on the same WIT surface (gate W1, ADR-013).
    WasmModDivergence {
        /// Instance id of the offending module.
        // pending A-08 / OQ-11: `ModuleID` width is not contracted anywhere yet; this `u32` is a
        // placeholder payload and adopts whatever A-02/A-08 freeze.
        module_id: u32,
    },
    /// A float value was found inside something a root hashes (AGENTS.md §4, ADR-017 decision 1).
    FloatInHashedValue,
    /// A transcendental result (`sin`/`cos`/`exp`/`pow`/`atan`) fed a root or epoch hash (AGENTS.md §4).
    TranscendentalInHashedValue,
    /// An adapter-dependent value — shader or GPU-derived — reached a hashed path (SPEC-02 locked via ADR-017).
    AdapterValueInRoot,
    /// Identical source serialized to different bytes, so asset hashes cannot be epoch inputs (gate AS2, OQ-30 item 4).
    NonDeterministicSerialization,
    /// Two machines disagree on a material id, which would fork every root built from it (doc 03 § Determinism boundary).
    MaterialIdDivergence,
    /// A Merkle node was hashed without its domain-separated prefix (ADR-028 constraints, SPEC-04).
    DomainSeparatorMissing,
    /// Lint finding: a mod read wall-clock or system time instead of `tick`/`current-epoch` (doc 08 determinism rule 1).
    WallClockReadInLogic {
        /// Instance id of the offending module (placeholder width, pending A-08 / OQ-11).
        module_id: u32,
    },
    /// Lint finding: a mod used unseeded randomness instead of a host RNG stream (doc 08 determinism rule 2).
    UnseededRandomInLogic {
        /// Instance id of the offending module (placeholder width, pending A-08 / OQ-11).
        module_id: u32,
    },
    /// Lint finding: an `on-*` handler produced different output for identical input (doc 08 determinism rule 4).
    AmbientStateInHandler {
        /// Instance id of the offending module (placeholder width, pending A-08 / OQ-11).
        module_id: u32,
    },
}

impl DeterminismViolation {
    /// Stable code for this violation: band 1 plus a literal offset, never a string hash.
    pub const fn code(&self) -> u16 {
        let base = bands::base(bands::DETERMINISM);
        match self {
            Self::EpochRootMismatch { .. } => base + 1,
            Self::RegionRootMismatch { .. } => base + 2,
            Self::SameSeedReplayDivergence { .. } => base + 3,
            Self::CrossTargetDivergence { .. } => base + 4,
            Self::WasmModDivergence { .. } => base + 5,
            Self::FloatInHashedValue => base + 6,
            Self::TranscendentalInHashedValue => base + 7,
            Self::AdapterValueInRoot => base + 8,
            Self::NonDeterministicSerialization => base + 9,
            Self::MaterialIdDivergence => base + 10,
            Self::DomainSeparatorMissing => base + 11,
            Self::WallClockReadInLogic { .. } => base + 12,
            Self::UnseededRandomInLogic { .. } => base + 13,
            Self::AmbientStateInHandler { .. } => base + 14,
        }
    }
}

/// `crates/coords` — HDCS violations (doc 02, gates C1–C4, ADR-016).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordsError {
    /// `region * 4096 + offset != pos` for some sampled coordinate (gate C1).
    RegionRoundTripFailed,
    /// A value outside the 12-bit `LocalOffset` range reached a masked field (SPEC-01, doc 02: masked, never clamped).
    LocalOffsetOutOfRange,
    /// Region resolution truncated toward zero instead of flooring, so keys differ across zero (doc 02 rounding rule, gate C2).
    RegionDivisionNotFloor,
    /// Precision jitter observed at a far coordinate, i.e. the Far Lands regression (gate C3).
    FarCoordPrecisionJitter,
    /// Camera-space error exceeded the declared ulp budget at the declared radius (gate C4 — radius and epsilon are declared together).
    CameraUlpBudgetExceeded,
    /// A Morton code was computed on raw voxel coordinates, which doc 02 forbids (tree shape must be peer-agreable).
    VoxelLevelMortonForbidden,
    /// A slab plane index fell outside its region's plane set (SPEC-06 canonical `(region_key, axis, plane_index)`).
    // SPEC-06 still decides what a plane set contains, so the valid `plane` range is not pinned
    // here. A-01 froze on 2026-10-11, so carrying its `SlabKey` as this variant's payload is now a
    // scheduled migration (AGENTS.md §2) rather than a blocked one.
    PlaneIndexOutOfRange,
    /// An `AabbI64` arrived with `min > max` on some axis (doc 02: integer bounds, never f32).
    AabbInverted,
}

impl CoordsError {
    /// Stable code for this class: band 2 plus a literal offset.
    pub const fn code(&self) -> u16 {
        let base = bands::base(bands::COORDS);
        match self {
            Self::RegionRoundTripFailed => base + 1,
            Self::LocalOffsetOutOfRange => base + 2,
            Self::RegionDivisionNotFloor => base + 3,
            Self::FarCoordPrecisionJitter => base + 4,
            Self::CameraUlpBudgetExceeded => base + 5,
            Self::VoxelLevelMortonForbidden => base + 6,
            Self::PlaneIndexOutOfRange => base + 7,
            Self::AabbInverted => base + 8,
        }
    }
}

/// `crates/ledger` — epochs, roots, mempool, event log, compaction (doc 03, gates P1.1–P1.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerError {
    /// A packed delta did not decode back to the record it was encoded from (gate P1.1).
    // the layout closes: ADR-029 (locked 2026-10-11) fixes the record as 16 region-slot / 36
    // intra-region / 12 payload. The tuple `(RegionKey, [u16; 3], value)` is not carried here yet
    // only because A-03 is unwritten — it is unblocked, not undecided.
    DeltaRecordRoundTripFailed,
    /// The per-epoch distinct-region count crossed the region-slot ceiling (doc 03: a hard, checked limit).
    // decided by ADR-029: the slot field stays 16 bits, so the ceiling is 65,536 regions per epoch
    // record stream. Past it the stream partitions per region (or micro-commits early) rather than
    // widening the field.
    EpochRegionSlotsExhausted,
    /// A requested epoch is outside the retained window and no snapshot anchors it (doc 03: the ledger is a bounded window).
    EpochWindowUnderrun,
    /// The signed state snapshot named as a replay anchor is not present (doc 03 § replay is `O(k)` from a snapshot).
    // pending OQ-23 / OQ-07: what physically persists to disk and the save layout are unspecified,
    // so no path, file id, or record handle is committed into this variant.
    SnapshotAnchorMissing,
    /// Two transactions entered the pool out of Lamport order (doc 03 commit pipeline step 2, doc 10 consistency rule 2).
    LamportOrderViolation,
    /// Compaction changed the state it was only meant to re-encode (doc 03 § Compaction, gate SM3).
    CompactionNotIdempotent {
        /// Root before and after the bake, compared as opaque digests.
        roots: RootCompare,
    },
    /// An event tag arrived whose kind is not in the registry (doc 03 event log, doc 05 event signals).
    // pending OQ-08 / A-05: the tag vocabulary, its field names and its width are undecided; this
    // `u16` is the smallest non-committal carrier and will adopt A-05's registry type.
    EventTagUnregistered {
        /// Registry index of the unrecognised tag kind.
        kind: u16,
    },
    /// Re-entering a modified region did not restore every edit it had outstanding (gate P1.4).
    // ADR-029 (locked 2026-10-11) addresses a record inside the region, not inside a slab, so the
    // open question here is capacity — how many records a region can hold outstanding, which is
    // SPEC-06's and spike S2's to measure. Frozen A-01 supplies the region identity; A-03 supplies
    // the record, and until it exists no region or record index is committed into this variant.
    TierRestoreIncomplete,
}

impl LedgerError {
    /// Stable code for this class: band 3 plus a literal offset.
    pub const fn code(&self) -> u16 {
        let base = bands::base(bands::LEDGER);
        match self {
            Self::DeltaRecordRoundTripFailed => base + 1,
            Self::EpochRegionSlotsExhausted => base + 2,
            Self::EpochWindowUnderrun => base + 3,
            Self::SnapshotAnchorMissing => base + 4,
            Self::LamportOrderViolation => base + 5,
            Self::CompactionNotIdempotent { .. } => base + 6,
            Self::EventTagUnregistered { .. } => base + 7,
            Self::TierRestoreIncomplete => base + 8,
        }
    }
}

/// `crates/memory` — slabs, bump pointers, tiering, GPU binding (doc 03, ADR-008/009).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    /// No slab could be reserved for a region's working set (doc 03: slab size and count are budgets, not guesses).
    // ADR-029 removed the addressing conflict this note used to record, so the 64 KB / 1 MB figures
    // are now purely a capacity question for spike S2. No byte count is committed into this variant
    // until a measurement picks one.
    SlabPoolExhausted,
    /// An LRU eviction targeted a slab still bound to the GPU (doc 03 `M_active`, ADR-009).
    PinnedSlabEviction,
    /// A dirty slab set was not contiguous, so it could not go up in one DMA or hash in one pass (doc 03, gate P1.3).
    DirtyRangeNotContiguous,
    /// The `M_cold` write-ahead log filled before its background micro-checkpoint could run (doc 03 § Tiering).
    // pending OQ-23 / OQ-24: the disk tier's contents and the compaction cadence beyond sleep are
    // undecided, so this carries no file, offset, or threshold value.
    ColdLogOverflow,
    /// A zero-copy read or write landed off the alignment its FlatBuffer or slab layout requires (ADR-015, gate AS4).
    UnalignedZeroCopyAccess,
    /// A slab was recycled while a frame still had it bound (doc 04 § VRAM: ring-recycled across epochs, never mid-frame).
    SlabRecycleWhileBound,
}

impl MemoryError {
    /// Stable code for this class: band 4 plus a literal offset.
    pub const fn code(&self) -> u16 {
        let base = bands::base(bands::MEMORY);
        match self {
            Self::SlabPoolExhausted => base + 1,
            Self::PinnedSlabEviction => base + 2,
            Self::DirtyRangeNotContiguous => base + 3,
            Self::ColdLogOverflow => base + 4,
            Self::UnalignedZeroCopyAccess => base + 5,
            Self::SlabRecycleWhileBound => base + 6,
        }
    }
}

/// `crates/render` — WGSL master interpreter, BVH, frame budget (doc 04, gates R1–R5, ADR-011/012/024).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderError {
    /// A shader failed to compile, reported by the S6 headless harness (docs/12 loop, ADR-003).
    ShaderCompileFailed,
    /// The WebGPU device rejected a pipeline or binding layout (ADR-003, doc 04 § VRAM).
    PipelineValidationFailed,
    /// A march exceeded its step cap or `MIN_STEP` floor (doc 04 § Culling; caps are provisional until spike S1).
    MarchStepBudgetExceeded,
    /// BVH descent visited more candidates than the budget allows (ADR-024, gate R1).
    BvhVisitsExceeded,
    /// A frame exceeded the committed budget (gates R1/R2 — the five budget numbers are ours, and unmeasured).
    FrameBudgetMissed,
    /// A shader output was wired into a buffer the simulation reads — the structural form of SPEC-02 (ADR-017, gate DE1).
    ///
    /// Distinguished from [`DeterminismViolation::AdapterValueInRoot`]: this is the wiring finding
    /// caught before any root is computed; that one is the digest consequence after one got through.
    GpuOutputFeedsState,
    /// Frame hashes differ across `wgpu` backends (gate R4) — **informational only**, never a state gate (ADR-017, docs/11).
    AdapterFrameHashDrift,
}

impl RenderError {
    /// Stable code for this class: band 5 plus a literal offset.
    pub const fn code(&self) -> u16 {
        let base = bands::base(bands::RENDER);
        match self {
            Self::ShaderCompileFailed => base + 1,
            Self::PipelineValidationFailed => base + 2,
            Self::MarchStepBudgetExceeded => base + 3,
            Self::BvhVisitsExceeded => base + 4,
            Self::FrameBudgetMissed => base + 5,
            Self::GpuOutputFeedsState => base + 6,
            Self::AdapterFrameHashDrift => base + 7,
        }
    }
}

/// `crates/sim` — macro fields, rule library, jump methods, Δt catch-up (doc 05, gates SM1–SM6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimError {
    /// A macro rule was registered with no jump method, so it can only offer endpoints it evaluated (ADR-005, gate SM2).
    // pending OQ-09: which rule families admit jump evaluation is a survey that has not been done,
    // so this variant names the absence and does not classify the family.
    JumpMethodMissing,
    /// An iterative rule was asked to advance past its published cap, where it is not claimed exact (SPEC-05 proposal, doc 05).
    IterativeCapExceeded,
    /// A field has no declared clock, so Δt means nothing for it (OQ-42 — ticks, seconds, or days).
    FieldClockUndeclared,
    /// One-ring propagation did not converge on the effect the event log recorded (gate SM1, spike S7).
    // pending OQ-01: the propagation mechanism and its radius are the project's central unknown;
    // no radius or region count is committed into this payload.
    PropagationRingUnconverged,
    /// Region-edge cells disagreed with their cross-region neighbours (gate SM6, doc 02 integration matrix).
    RegionEdgeWrapDisagreement,
    /// An epoch names a rule-set version this build cannot evaluate (doc 05 § Determinism across versions).
    RuleSetVersionUnavailable,
    /// A field was re-anchored without an ADR, teleporting existing state (gate SM4, doc 05: re-anchoring is a content decision).
    FieldReanchorUnauthorized,
    /// A commit touched more regions than its budget allows (gate SM5 — a cost-model hypothesis, not a measurement).
    EpochCommitOverBudget,
}

impl SimError {
    /// Stable code for this class: band 6 plus a literal offset.
    pub const fn code(&self) -> u16 {
        let base = bands::base(bands::SIM);
        match self {
            Self::JumpMethodMissing => base + 1,
            Self::IterativeCapExceeded => base + 2,
            Self::FieldClockUndeclared => base + 3,
            Self::PropagationRingUnconverged => base + 4,
            Self::RegionEdgeWrapDisagreement => base + 5,
            Self::RuleSetVersionUnavailable => base + 6,
            Self::FieldReanchorUnauthorized => base + 7,
            Self::EpochCommitOverBudget => base + 8,
        }
    }
}

/// `crates/circuits` — graph compiler, topology solver, stateful nodes (doc 06, gates E1–E6, ADR-010).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitError {
    /// A feedback cycle was compiled with no stateful node to cut it at, where a topo sort is undefined (OQ-02).
    // pending OQ-02: the `LedgerStateCell` direction is a proposal without an ADR, so no node or
    // component identity is committed into this payload.
    CyclicGraphWithoutStatefulCut,
    /// Cutting the cycle at stateful nodes did not reach a fixed point (doc 06 proposed direction, OQ-02).
    // pending OQ-02: iteration caps belong to the eventual stateful-node ADR.
    SccFixedPointUnreached,
    /// Sequential state was written outside an epoch boundary, breaking the clocked design (doc 06 consequences, OQ-02).
    // pending OQ-02: gate E5/E6 exist only after OQ-02; this offset is reserved for that mechanism.
    StateCellWriteOutsideEpoch,
    /// Observing a running circuit changed its committed state (gate E6 — the hard invariant doc 06 names).
    ObservationAlteredOutput,
    /// Recompiling a touched component exceeded its budget (OQ-22, spike S10).
    // pending SPEC-07: whether invalidation is whole-component or dirty-subgraph is open, so the
    // unit that overshot is deliberately not named in the payload.
    RecompileOverBudget,
    /// A circuit edge carried an absolute coordinate instead of a relative offset (doc 02 integration matrix).
    AbsoluteEdgeCoordinate,
    /// An analog quantity left the fixed-point range the design commits to (doc 06 § Determinism: 0–15 signal strength, never float).
    AnalogSignalOutOfRange,
}

impl CircuitError {
    /// Stable code for this class: band 7 plus a literal offset.
    pub const fn code(&self) -> u16 {
        let base = bands::base(bands::CIRCUITS);
        match self {
            Self::CyclicGraphWithoutStatefulCut => base + 1,
            Self::SccFixedPointUnreached => base + 2,
            Self::StateCellWriteOutsideEpoch => base + 3,
            Self::ObservationAlteredOutput => base + 4,
            Self::RecompileOverBudget => base + 5,
            Self::AbsoluteEdgeCoordinate => base + 6,
            Self::AnalogSignalOutOfRange => base + 7,
        }
    }
}

/// `crates/physics` — SDF gradient collision, audio tracing, animation warps (doc 07, gates PH1/AU1/AN1/DE1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicsError {
    /// The central-difference gradient collapsed to a degenerate normal (doc 07, ADR-018).
    ///
    /// Presentation-grade and never hashed (AGENTS.md §4), so this is a quality failure, not a desync.
    GradientDegenerate,
    /// The push-out iteration did not converge to `d >= 0` (doc 07 resolution loop).
    PushoutNotConverged,
    /// Per-entity gradient queries exceeded the frame's contact budget (gate PH2, OQ-16; PH2's target is invented).
    ContactQueryOverBudget,
    /// Voice count exceeded the audio budget for SDF occlusion tracing (gate AU1, OQ-25; OQ-40 leaves the engine unchosen).
    AudioVoiceCapReached,
    /// A domain-deformation warp cost more march steps than the budget tolerates (gate AN1, doc 07 consequences).
    WarpMarchOverBudget,
    /// A subsystem asked for sub-voxel coordinate precision, which HDCS does not supply (OQ-15, AGENTS.md §9: escalate, do not special-case a second coordinate system).
    SubVoxelPrecisionRequested,
}

impl PhysicsError {
    /// Stable code for this class: band 8 plus a literal offset.
    pub const fn code(&self) -> u16 {
        let base = bands::base(bands::PHYSICS);
        match self {
            Self::GradientDegenerate => base + 1,
            Self::PushoutNotConverged => base + 2,
            Self::ContactQueryOverBudget => base + 3,
            Self::AudioVoiceCapReached => base + 4,
            Self::WarpMarchOverBudget => base + 5,
            Self::SubVoxelPrecisionRequested => base + 6,
        }
    }
}

/// `crates/host` — WIT bindings, sandbox, fuel metering, hot reload (doc 08, gates W1–W5, ADR-013).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostError {
    /// A module exhausted its fuel and was interrupted (gate W3, OQ-20).
    // pending OQ-20: the budget itself is undecided, so no fuel figure is carried — only which
    // instance blew it, which is what docs/12's routing needs.
    FuelExhausted {
        /// Instance id of the metered module (placeholder width, pending A-08 / OQ-11).
        module_id: u32,
    },
    /// A module trapped; it was unloaded and its linear memory reset with the game still running (gate W2).
    ModTrapUnloaded {
        /// Instance id of the trapped module (placeholder width, pending A-08 / OQ-11).
        module_id: u32,
    },
    /// A module crossed its linear-memory cap (doc 08 execution limits, OQ-20).
    ModuleMemoryCapExceeded {
        /// Instance id of the offending module (placeholder width, pending A-08 / OQ-11).
        module_id: u32,
    },
    /// A query returned more bytes than the host quota allows (doc 08 permissions, OQ-34 — including who tunes a legitimate cap).
    QueryQuotaExceeded {
        /// Instance id of the offending module (placeholder width, pending A-08 / OQ-11).
        module_id: u32,
    },
    /// A module's WIT surface does not match the host's (ADR-013, artifact A-02).
    // pending A-02: `contracts/ledger-api.wit` v1 is not frozen, and the version encoding shown in
    // doc 08 (`game:modding@0.1.0`) is illustrative; these two `u16`s adopt whatever A-02 defines.
    WitAbiMismatch {
        /// Version the host exports.
        expected: u16,
        /// Version the module declares.
        found: u16,
    },
    /// A module used a permission its manifest did not declare (doc 08 permissions).
    // pending OQ-34: the permission name space and its ids are undecided, so `permission_id` is a
    // registry index placeholder, not a contracted namespace.
    PermissionDenied {
        /// Instance id of the offending module (placeholder width, pending A-08 / OQ-11).
        module_id: u32,
        /// Registry index of the denied permission.
        permission_id: u16,
    },
    /// A module's declared contract version is below the engine's minimum (doc 08 version compat, OQ-33).
    // pending OQ-33: the compatibility window is unspecified; only the fact of rejection is coded.
    ContractVersionUnsupported {
        /// Instance id of the unsupported module (placeholder width, pending A-08 / OQ-11).
        module_id: u32,
    },
    /// A module's bytes do not match the hash recorded for it, i.e. an unsigned or swapped binary (doc 08 signing, ADR-023's detection story).
    // pending OQ-11: signing, distribution and update channels are open, so no signature or
    // key material is committed — the content digest alone is enough to route this.
    UnsignedModuleSwap {
        /// Content digest of the module that failed the check (ADR-028 width).
        module_hash: [u8; ROOT_BYTES],
    },
}

impl HostError {
    /// Stable code for this class: band 9 plus a literal offset.
    pub const fn code(&self) -> u16 {
        let base = bands::base(bands::HOST);
        match self {
            Self::FuelExhausted { .. } => base + 1,
            Self::ModTrapUnloaded { .. } => base + 2,
            Self::ModuleMemoryCapExceeded { .. } => base + 3,
            Self::QueryQuotaExceeded { .. } => base + 4,
            Self::WitAbiMismatch { .. } => base + 5,
            Self::PermissionDenied { .. } => base + 6,
            Self::ContractVersionUnsupported { .. } => base + 7,
            Self::UnsignedModuleSwap { .. } => base + 8,
        }
    }
}

/// `crates/assetc` — parse, schema validation, lowering, packing (doc 09, gates AS1–AS5, ADR-014/015).
///
/// The whole band is tool-side: `assetc` is not on the frame path (AGENTS.md §3), so these codes
/// exist to give an agent a clean failure to fix, which is why they carry spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetError {
    /// RON did not parse (AGENTS.md §5 canonical authoring format, gate AS3).
    RonParseFailed {
        /// Where in the source it failed (doc 09 § Error reporting).
        span: SourceSpan,
    },
    /// JSON did not parse on the accepted-alias path (doc 09 § Formats).
    JsonAliasParseFailed {
        /// Where in the source it failed.
        span: SourceSpan,
    },
    /// Source parsed but violated the schema (gate AS3: fail the build with a span-accurate message).
    SchemaViolation {
        /// Where in the source the violated path was written.
        span: SourceSpan,
    },
    /// A `.bin` or `.mpack` declares a schema version outside the accepted window (OQ-30 items 1–2).
    // pending OQ-30: the versioning window and migration strategy are open; these two `u16`s only
    // report the endpoints the loader was built with, they do not choose a policy.
    SchemaVersionUnsupported {
        /// Version the container declares.
        found: u16,
        /// Oldest version this loader accepts.
        minimum: u16,
    },
    /// A runtime container's header does not match its expected layout (doc 09 § Formats, ADR-015).
    ContainerHeaderMismatch,
    /// An SDF tree was rejected by the compile-time budget pass (gate AS5 — distinct from
    /// [`RenderError::MarchStepBudgetExceeded`], which is a runtime march overshoot).
    CompileBudgetRejected,
    /// A compiled container could not be uploaded zero-copy (gates AS1/AS4, ADR-015).
    ZeroCopyUploadFailed,
    /// Two contents claimed the same stable asset id (doc 09 § Schema item 3, OQ-30).
    // pending OQ-30: stable id assignment and namespacing are open; no id scheme is implied by the
    // variant's existence, only that a collision was detected.
    StableIdCollision,
    /// Something asked the runtime to parse text, which doc 09's rule and docs/01 invariant 4 forbid (ADR-014).
    RuntimeParseAttempt,
}

impl AssetError {
    /// Stable code for this class: band 10 plus a literal offset.
    pub const fn code(&self) -> u16 {
        let base = bands::base(bands::ASSET);
        match self {
            Self::RonParseFailed { .. } => base + 1,
            Self::JsonAliasParseFailed { .. } => base + 2,
            Self::SchemaViolation { .. } => base + 3,
            Self::SchemaVersionUnsupported { .. } => base + 4,
            Self::ContainerHeaderMismatch => base + 5,
            Self::CompileBudgetRejected => base + 6,
            Self::ZeroCopyUploadFailed => base + 7,
            Self::StableIdCollision => base + 8,
            Self::RuntimeParseAttempt => base + 9,
        }
    }
}

/// `crates/net` — `ActionTx` broadcast, ordering, ownership, repair (doc 10, gates M1–M7, ADR-021/022/023).
///
/// Reserved now even though multiplayer is deferred to Phase 1.5 (ADR-004), so that phase cannot
/// renumber anything. Root disagreement itself is **not** here: it is
/// [`DeterminismViolation::EpochRootMismatch`], and only the repair machinery's failures are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// An `ActionTx` envelope did not decode (artifact A-08, doc 10 § The shift).
    // the delta payload's widths are decided (ADR-029: 16 slot / 36 intra-region / 12 payload); what
    // is missing is A-08's envelope layout itself, which that sign-off unblocked. Until A-08 is
    // written no field index or byte offset is committed.
    ActionTxMalformed,
    /// A transaction's signature did not verify against its claimed proposer (ADR-021, OQ-11).
    // pending OQ-11 / OQ-14: signature scheme, sizes, and what a divergent root cost a proposer are
    // all open; this codes the fact of failure only, and carries no key material.
    SignatureInvalid,
    /// A sequence id arrived behind one already applied, breaking causal order (doc 10 consistency rule 2, ADR-027).
    HlcMonotonicityBroken,
    /// A client assumed state read at its `HLC_max` could still be rolled back (doc 10 staleness bound).
    StaleReadAfterHlcBound,
    /// A peer sent a slab mutation for a region it does not own (doc 10 single-writer-per-region).
    // pending OQ-18: whether single-writer-per-region ownership survives real RTT is unproven. The
    // region-slot field is settled at 16 bits (ADR-029) and the region identity is frozen A-01's, so
    // this variant could carry both — it does not yet, because the ownership model is what is open.
    RegionOwnershipDenied,
    /// The bisection walk could not localise the disagreeing branch (ADR-023, gate P1.6).
    // SPEC-09 no longer holds this open: ADR-029 fixes the slot field and frozen A-01 supplies the
    // region index. A located-branch payload waits on A-03's epoch region-slot table, which is now
    // unblocked and simply unwritten.
    BisectionRepairExhausted,
    /// Concurrent operations on one field had no defined merge rule (gate M5/M6, OQ-35).
    // pending OQ-35: merge rules cover aggregate fields, not arbitrary mod state; the variant is
    // the placeholder for whichever class of state that turns out to be.
    MergeRuleUndefined,
    /// A watch-list grew past what the host can afford (doc 10 § Macro causality on the wire, OQ-36).
    WatchListCapacityExceeded,
}

impl NetError {
    /// Stable code for this class: band 11 plus a literal offset.
    pub const fn code(&self) -> u16 {
        let base = bands::base(bands::NET);
        match self {
            Self::ActionTxMalformed => base + 1,
            Self::SignatureInvalid => base + 2,
            Self::HlcMonotonicityBroken => base + 3,
            Self::StaleReadAfterHlcBound => base + 4,
            Self::RegionOwnershipDenied => base + 5,
            Self::BisectionRepairExhausted => base + 6,
            Self::MergeRuleUndefined => base + 7,
            Self::WatchListCapacityExceeded => base + 8,
        }
    }
}

/// Every engine-side failure class, grouped by the owning crate in docs/12's layout.
///
/// This is the type a subsystem returns. Its [`code`](Self::code) is the classification docs/12's
/// CI loop feeds back to the owning agent, and [`bands::band_of`] is what maps that code to the
/// crate — no string matching anywhere in the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineError {
    /// A determinism violation, escalated ahead of any subsystem retry (AGENTS.md §4 and §9, ADR-017).
    Determinism(DeterminismViolation),
    /// HDCS violation (doc 02, `crates/coords`).
    Coords(CoordsError),
    /// Ledger violation (doc 03, `crates/ledger`).
    Ledger(LedgerError),
    /// Slab, tiering, or binding violation (doc 03, `crates/memory`).
    Memory(MemoryError),
    /// Render or budget violation (doc 04, `crates/render`).
    Render(RenderError),
    /// Macro simulation violation (doc 05, `crates/sim`).
    Sim(SimError),
    /// Circuit graph violation (doc 06, `crates/circuits`).
    Circuits(CircuitError),
    /// Physics, audio, or animation violation (doc 07, `crates/physics`).
    Physics(PhysicsError),
    /// WASM host or sandbox violation (doc 08, `crates/host`).
    Host(HostError),
    /// Asset compilation or container violation (doc 09, `crates/assetc`).
    Asset(AssetError),
    /// Wire, ordering, or repair violation (doc 10, `crates/net`).
    Net(NetError),
}

impl EngineError {
    /// Stable code for the wrapped class; a determinism violation keeps its band-1 code (AGENTS.md §4).
    pub const fn code(&self) -> u16 {
        match self {
            Self::Determinism(v) => v.code(),
            Self::Coords(e) => e.code(),
            Self::Ledger(e) => e.code(),
            Self::Memory(e) => e.code(),
            Self::Render(e) => e.code(),
            Self::Sim(e) => e.code(),
            Self::Circuits(e) => e.code(),
            Self::Physics(e) => e.code(),
            Self::Host(e) => e.code(),
            Self::Asset(e) => e.code(),
            Self::Net(e) => e.code(),
        }
    }

    /// True when this error must escalate instead of entering the three-attempt retry budget (AGENTS.md §9, docs/12).
    pub const fn is_determinism_class(&self) -> bool {
        matches!(self, Self::Determinism(_))
    }
}

macro_rules! subsystem_from {
    ($($sub:ty => $variant:ident),* $(,)?) => {
        $(
            impl From<$sub> for EngineError {
                /// Wrap a subsystem class in the single error type the CI loop routes on (docs/12).
                fn from(e: $sub) -> Self {
                    Self::$variant(e)
                }
            }
        )*
    };
}

subsystem_from! {
    DeterminismViolation => Determinism,
    CoordsError => Coords,
    LedgerError => Ledger,
    MemoryError => Memory,
    RenderError => Render,
    SimError => Sim,
    CircuitError => Circuits,
    PhysicsError => Physics,
    HostError => Host,
    AssetError => Asset,
    NetError => Net,
}

impl core::fmt::Display for DeterminismViolation {
    /// Cold path only: formats once per failure, after it, never inside a frame (AGENTS.md §3).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "determinism#{:05}", self.code())
    }
}

impl core::fmt::Display for EngineError {
    /// Cold path only: emits the band tag plus the stable integer code; the meaning of a code
    /// lives in this file's rustdoc, not in a formatted string (docs/12, AGENTS.md §3).
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Determinism(v) => core::fmt::Display::fmt(v, f),
            Self::Coords(_) => write!(f, "coords#{:05}", self.code()),
            Self::Ledger(_) => write!(f, "ledger#{:05}", self.code()),
            Self::Memory(_) => write!(f, "memory#{:05}", self.code()),
            Self::Render(_) => write!(f, "render#{:05}", self.code()),
            Self::Sim(_) => write!(f, "sim#{:05}", self.code()),
            Self::Circuits(_) => write!(f, "circuits#{:05}", self.code()),
            Self::Physics(_) => write!(f, "physics#{:05}", self.code()),
            Self::Host(_) => write!(f, "host#{:05}", self.code()),
            Self::Asset(_) => write!(f, "asset#{:05}", self.code()),
            Self::Net(_) => write!(f, "net#{:05}", self.code()),
        }
    }
}

impl core::error::Error for DeterminismViolation {}

impl core::error::Error for EngineError {
    /// Cold path only; a wrapped determinism violation is reported as the source, subsystem classes
    /// are not (`From` already folded them into this type, and only the escalation class is named).
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Determinism(v) => Some(v),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    // Test-only scaffolding. It allocates nothing either, because `contracts/` is compiled
    // `#![no_std]` by the workspace crate (docs/12, AGENTS.md §3): no `Vec`, no `String`, no
    // `to_string()` — fixed-size arrays on the stack and a byte-counting `core::fmt::Write` sink.
    use super::*;

    const ROOTS_SAME: RootCompare = RootCompare {
        local: [7; ROOT_BYTES],
        remote: [7; ROOT_BYTES],
    };
    const ROOTS_DIFFER: RootCompare = RootCompare {
        local: [0; ROOT_BYTES],
        remote: [1; ROOT_BYTES],
    };
    const SPAN: SourceSpan = SourceSpan {
        line: 1,
        column: 1,
        byte_offset: 0,
    };
    const ID: u32 = 0;

    /// Number of classes the taxonomy can emit; the table below must cover exactly every one.
    const CLASS_COUNT: usize = 89;

    /// Every emittable class, paired with the band it must land in.
    ///
    /// Built in a `const` from the variants themselves, so the test enumerates the contract
    /// rather than a hand-typed copy of it: adding a variant without adding a row here changes
    /// `CLASS_COUNT` and fails the first assertion.
    const PAIRS: [(u16, u16); CLASS_COUNT] = [
        // band 1 — DeterminismViolation (AGENTS.md §4, ADR-017)
        (
            DeterminismViolation::EpochRootMismatch {
                roots: ROOTS_DIFFER,
            }
            .code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::RegionRootMismatch {
                roots: ROOTS_DIFFER,
            }
            .code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::SameSeedReplayDivergence {
                roots: ROOTS_DIFFER,
            }
            .code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::CrossTargetDivergence {
                roots: ROOTS_DIFFER,
            }
            .code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::WasmModDivergence { module_id: ID }.code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::FloatInHashedValue.code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::TranscendentalInHashedValue.code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::AdapterValueInRoot.code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::NonDeterministicSerialization.code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::MaterialIdDivergence.code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::DomainSeparatorMissing.code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::WallClockReadInLogic { module_id: ID }.code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::UnseededRandomInLogic { module_id: ID }.code(),
            bands::DETERMINISM,
        ),
        (
            DeterminismViolation::AmbientStateInHandler { module_id: ID }.code(),
            bands::DETERMINISM,
        ),
        // band 2 — CoordsError (doc 02, gates C1–C4)
        (CoordsError::RegionRoundTripFailed.code(), bands::COORDS),
        (CoordsError::LocalOffsetOutOfRange.code(), bands::COORDS),
        (CoordsError::RegionDivisionNotFloor.code(), bands::COORDS),
        (CoordsError::FarCoordPrecisionJitter.code(), bands::COORDS),
        (CoordsError::CameraUlpBudgetExceeded.code(), bands::COORDS),
        (CoordsError::VoxelLevelMortonForbidden.code(), bands::COORDS),
        (CoordsError::PlaneIndexOutOfRange.code(), bands::COORDS),
        (CoordsError::AabbInverted.code(), bands::COORDS),
        // band 3 — LedgerError (doc 03, gates P1.1–P1.6)
        (
            LedgerError::DeltaRecordRoundTripFailed.code(),
            bands::LEDGER,
        ),
        (LedgerError::EpochRegionSlotsExhausted.code(), bands::LEDGER),
        (LedgerError::EpochWindowUnderrun.code(), bands::LEDGER),
        (LedgerError::SnapshotAnchorMissing.code(), bands::LEDGER),
        (LedgerError::LamportOrderViolation.code(), bands::LEDGER),
        (
            LedgerError::CompactionNotIdempotent { roots: ROOTS_SAME }.code(),
            bands::LEDGER,
        ),
        (
            LedgerError::EventTagUnregistered { kind: 0 }.code(),
            bands::LEDGER,
        ),
        (LedgerError::TierRestoreIncomplete.code(), bands::LEDGER),
        // band 4 — MemoryError (doc 03, ADR-008/009)
        (MemoryError::SlabPoolExhausted.code(), bands::MEMORY),
        (MemoryError::PinnedSlabEviction.code(), bands::MEMORY),
        (MemoryError::DirtyRangeNotContiguous.code(), bands::MEMORY),
        (MemoryError::ColdLogOverflow.code(), bands::MEMORY),
        (MemoryError::UnalignedZeroCopyAccess.code(), bands::MEMORY),
        (MemoryError::SlabRecycleWhileBound.code(), bands::MEMORY),
        // band 5 — RenderError (doc 04, gates R1–R5)
        (RenderError::ShaderCompileFailed.code(), bands::RENDER),
        (RenderError::PipelineValidationFailed.code(), bands::RENDER),
        (RenderError::MarchStepBudgetExceeded.code(), bands::RENDER),
        (RenderError::BvhVisitsExceeded.code(), bands::RENDER),
        (RenderError::FrameBudgetMissed.code(), bands::RENDER),
        (RenderError::GpuOutputFeedsState.code(), bands::RENDER),
        (RenderError::AdapterFrameHashDrift.code(), bands::RENDER),
        // band 6 — SimError (doc 05, gates SM1–SM6)
        (SimError::JumpMethodMissing.code(), bands::SIM),
        (SimError::IterativeCapExceeded.code(), bands::SIM),
        (SimError::FieldClockUndeclared.code(), bands::SIM),
        (SimError::PropagationRingUnconverged.code(), bands::SIM),
        (SimError::RegionEdgeWrapDisagreement.code(), bands::SIM),
        (SimError::RuleSetVersionUnavailable.code(), bands::SIM),
        (SimError::FieldReanchorUnauthorized.code(), bands::SIM),
        (SimError::EpochCommitOverBudget.code(), bands::SIM),
        // band 7 — CircuitError (doc 06, gates E1–E6)
        (
            CircuitError::CyclicGraphWithoutStatefulCut.code(),
            bands::CIRCUITS,
        ),
        (CircuitError::SccFixedPointUnreached.code(), bands::CIRCUITS),
        (
            CircuitError::StateCellWriteOutsideEpoch.code(),
            bands::CIRCUITS,
        ),
        (
            CircuitError::ObservationAlteredOutput.code(),
            bands::CIRCUITS,
        ),
        (CircuitError::RecompileOverBudget.code(), bands::CIRCUITS),
        (CircuitError::AbsoluteEdgeCoordinate.code(), bands::CIRCUITS),
        (CircuitError::AnalogSignalOutOfRange.code(), bands::CIRCUITS),
        // band 8 — PhysicsError (doc 07, gates PH1/AU1/AN1/DE1)
        (PhysicsError::GradientDegenerate.code(), bands::PHYSICS),
        (PhysicsError::PushoutNotConverged.code(), bands::PHYSICS),
        (PhysicsError::ContactQueryOverBudget.code(), bands::PHYSICS),
        (PhysicsError::AudioVoiceCapReached.code(), bands::PHYSICS),
        (PhysicsError::WarpMarchOverBudget.code(), bands::PHYSICS),
        (
            PhysicsError::SubVoxelPrecisionRequested.code(),
            bands::PHYSICS,
        ),
        // band 9 — HostError (doc 08, gates W1–W5)
        (
            HostError::FuelExhausted { module_id: ID }.code(),
            bands::HOST,
        ),
        (
            HostError::ModTrapUnloaded { module_id: ID }.code(),
            bands::HOST,
        ),
        (
            HostError::ModuleMemoryCapExceeded { module_id: ID }.code(),
            bands::HOST,
        ),
        (
            HostError::QueryQuotaExceeded { module_id: ID }.code(),
            bands::HOST,
        ),
        (
            HostError::WitAbiMismatch {
                expected: 1,
                found: 2,
            }
            .code(),
            bands::HOST,
        ),
        (
            HostError::PermissionDenied {
                module_id: ID,
                permission_id: 0,
            }
            .code(),
            bands::HOST,
        ),
        (
            HostError::ContractVersionUnsupported { module_id: ID }.code(),
            bands::HOST,
        ),
        (
            HostError::UnsignedModuleSwap {
                module_hash: [0; ROOT_BYTES],
            }
            .code(),
            bands::HOST,
        ),
        // band 10 — AssetError (doc 09, gates AS1–AS5)
        (
            AssetError::RonParseFailed { span: SPAN }.code(),
            bands::ASSET,
        ),
        (
            AssetError::JsonAliasParseFailed { span: SPAN }.code(),
            bands::ASSET,
        ),
        (
            AssetError::SchemaViolation { span: SPAN }.code(),
            bands::ASSET,
        ),
        (
            AssetError::SchemaVersionUnsupported {
                found: 1,
                minimum: 2,
            }
            .code(),
            bands::ASSET,
        ),
        (AssetError::ContainerHeaderMismatch.code(), bands::ASSET),
        (AssetError::CompileBudgetRejected.code(), bands::ASSET),
        (AssetError::ZeroCopyUploadFailed.code(), bands::ASSET),
        (AssetError::StableIdCollision.code(), bands::ASSET),
        (AssetError::RuntimeParseAttempt.code(), bands::ASSET),
        // band 11 — NetError (doc 10, gates M1–M7)
        (NetError::ActionTxMalformed.code(), bands::NET),
        (NetError::SignatureInvalid.code(), bands::NET),
        (NetError::HlcMonotonicityBroken.code(), bands::NET),
        (NetError::StaleReadAfterHlcBound.code(), bands::NET),
        (NetError::RegionOwnershipDenied.code(), bands::NET),
        (NetError::BisectionRepairExhausted.code(), bands::NET),
        (NetError::MergeRuleUndefined.code(), bands::NET),
        (NetError::WatchListCapacityExceeded.code(), bands::NET),
    ];

    #[test]
    fn every_code_is_unique_across_all_subsystems() {
        assert_eq!(PAIRS.len(), CLASS_COUNT);
        for (i, (code, band)) in PAIRS.iter().enumerate() {
            for (other_code, other_band) in &PAIRS[i + 1..] {
                assert_ne!(
                    *code,
                    *other_code,
                    "collision: band {band} offset {} and band {other_band} offset {}",
                    bands::offset_of(*code),
                    bands::offset_of(*other_code),
                );
            }
        }
    }

    #[test]
    fn every_code_lands_in_its_declared_band_and_never_on_a_reserved_offset() {
        for (code, band) in PAIRS {
            assert_eq!(
                bands::band_of(code),
                band,
                "code {} escaped band {}",
                code,
                band
            );
            assert_ne!(
                code,
                bands::UNKNOWN,
                "no variant may emit the unclassified code"
            );
            assert_ne!(
                bands::offset_of(code),
                bands::OFFSET_UNKNOWN,
                "code {} sits on reserved offset 0 of band {}",
                code,
                band,
            );
        }
    }

    #[test]
    fn codes_are_stable_vectors_not_declaration_order() {
        // Pinned values: adding a variant must not move these, because every code is a literal.
        assert_eq!(
            DeterminismViolation::EpochRootMismatch {
                roots: ROOTS_DIFFER
            }
            .code(),
            513
        );
        assert_eq!(DeterminismViolation::FloatInHashedValue.code(), 518);
        assert_eq!(
            DeterminismViolation::AmbientStateInHandler { module_id: 9 }.code(),
            526
        );
        assert_eq!(CoordsError::RegionRoundTripFailed.code(), 1025);
        assert_eq!(CoordsError::AabbInverted.code(), 1032);
        assert_eq!(LedgerError::LamportOrderViolation.code(), 1541);
        assert_eq!(LedgerError::EventTagUnregistered { kind: 3 }.code(), 1543);
        assert_eq!(MemoryError::SlabRecycleWhileBound.code(), 2054);
        assert_eq!(RenderError::AdapterFrameHashDrift.code(), 2567);
        assert_eq!(SimError::EpochCommitOverBudget.code(), 3080);
        assert_eq!(CircuitError::AnalogSignalOutOfRange.code(), 3591);
        assert_eq!(PhysicsError::SubVoxelPrecisionRequested.code(), 4102);
        assert_eq!(
            HostError::WitAbiMismatch {
                expected: 1,
                found: 2
            }
            .code(),
            4613
        );
        assert_eq!(AssetError::RuntimeParseAttempt.code(), 5129);
        assert_eq!(NetError::WatchListCapacityExceeded.code(), 5640);
        // A payload never changes a class, and the band arithmetic round-trips.
        assert_eq!(
            DeterminismViolation::AmbientStateInHandler { module_id: 0 }.code(),
            DeterminismViolation::AmbientStateInHandler {
                module_id: u32::MAX
            }
            .code()
        );
        assert_eq!(bands::base(bands::NET), 5632);
        assert_eq!(bands::offset_of(5640), 8);
        assert_eq!(bands::LAST * bands::SIZE, 65024);
        assert!(!ROOTS_SAME.differs());
        assert!(ROOTS_DIFFER.differs());
    }

    #[test]
    fn determinism_class_is_routable_and_display_is_stable() {
        let v = DeterminismViolation::MaterialIdDivergence;
        let e: EngineError = v.into();
        assert!(e.is_determinism_class());
        assert_eq!(e.code(), v.code());
        assert_eq!(bands::band_of(e.code()), bands::DETERMINISM);
        assert!(core::error::Error::source(&e).is_some());

        let sub: EngineError = LedgerError::LamportOrderViolation.into();
        assert!(!sub.is_determinism_class());
        assert_eq!(bands::band_of(sub.code()), bands::LEDGER);
        assert!(core::error::Error::source(&sub).is_none());

        assert_text(&e, b"determinism#00522");
        assert_text(&sub, b"ledger#01541");
        assert_text(
            &EngineError::from(CircuitError::ObservationAlteredOutput),
            b"circuits#03588",
        );
    }

    /// Render through the cold-path `Display` impl into a stack buffer and compare, so `alloc`
    /// is not needed to test it (`contracts/` is `#![no_std]`).
    fn assert_text(value: &dyn core::fmt::Display, expected: &[u8]) {
        let mut probe = Probe::new();
        core::fmt::write(&mut probe, format_args!("{}", value)).unwrap();
        assert_eq!(probe.len, expected.len(), "display length drifted");
        assert_eq!(&probe.bytes[..probe.len], expected, "display text drifted");
    }

    /// A byte-counting `core::fmt::Write` sink that allocates nothing.
    struct Probe {
        bytes: [u8; 32],
        len: usize,
    }

    impl Probe {
        const fn new() -> Self {
            Self {
                bytes: [0; 32],
                len: 0,
            }
        }
    }

    impl core::fmt::Write for Probe {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for b in s.as_bytes() {
                if self.len < self.bytes.len() {
                    self.bytes[self.len] = *b;
                }
                self.len += 1;
            }
            Ok(())
        }
    }
}
