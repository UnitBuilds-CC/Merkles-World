//! Behaviour and gate tests for artifact A-01. The types themselves live in
//! `contracts/coords.rs` — this crate adds what a contract file cannot carry (a hash function and
//! the gate C1–C4 harness) without letting the contract grow an implementation.
//!
//! Gate C4 is stated against a declared render radius, not an invented one:
//! `RENDER_RADIUS` is the value docs/02 measures its ulp budget at.

pub mod lattice;

pub use contracts::coords::{
    AabbI64, Axis, CameraAnchor, GlobalVoxelPos, LocalOffset, RegionKey, SectorKey, SlabKey,
};

/// Half-extent of the world the renderer is allowed to see, in voxels (docs/02 § Float
/// discipline). Everything C4 claims is measured at this radius.
pub const RENDER_RADIUS: i64 = 4096;
