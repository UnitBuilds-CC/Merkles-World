//! Gate C2 needs a value that two machines can disagree about, so this module supplies the
//! smallest thing that qualifies: an integer lattice hash of `(seed, position)`.
//!
//! Scope warning. SPEC-04 registers two hashing roles — BLAKE3-256 for ledger identity (ADR-028,
//! which belongs to docs/03 and is blocked by SPEC-09), and an integer spatial hash for terrain
//! continuity. **This is the second role, and only a harness for it:** the terrain lattice itself
//! is unspecified (OQ-27) — no doc yet says how base `f(pos, t)` stays coherent across region
//! boundaries. Nothing here is a terrain design; changing this function changes every C2 test
//! vector in the repo.
//!
//! Determinism contract (AGENTS.md §4): wrapping integer ops only. No float, no transcendental,
//! no allocator, no clock, so the result is bit-identical on x86-64, ARM64 and `wasm32`.

use contracts::coords::GlobalVoxelPos;

const GOLDEN: u64 = 0x9e37_79b9_7f4a_7c15;
const MIX_A: u64 = 0xbf58_476d_1ce4_e5b9;
const MIX_B: u64 = 0x94d0_49bb_1331_11eb;

/// Splitmix64's finalizer: cheap, avalanche-clean, and made only of operations Rust defines
/// identically on every target.
const fn mix(mut v: u64) -> u64 {
    v ^= v >> 30;
    v = v.wrapping_mul(MIX_A);
    v ^= v >> 27;
    v = v.wrapping_mul(MIX_B);
    v ^ (v >> 31)
}

/// `i64` voxel coordinate → `u64` bit pattern, biased so that the region boundaries at 0 are not
/// a fixed point of the mixing. The cast itself is defined bit-preserving by Rust.
const fn axis_bits(v: i64) -> u64 {
    (v as u64).wrapping_add(i64::MIN as u64)
}

/// The lattice hash: `(seed, pos) -> u64`, order-independent in the three axes only because each
/// axis is mixed with its own salt.
pub fn lattice_hash(seed: u64, pos: &GlobalVoxelPos) -> u64 {
    let mut h = mix(seed ^ GOLDEN);
    h = mix(h ^ axis_bits(pos.x).wrapping_mul(GOLDEN));
    h = mix(h ^ axis_bits(pos.y).wrapping_mul(GOLDEN.rotate_left(21)));
    h = mix(h ^ axis_bits(pos.z).wrapping_mul(GOLDEN.rotate_left(42)));
    h
}

/// 32-bit view of [`lattice_hash`], for content that only needs 4 billion states.
pub fn lattice_hash_u32(seed: u64, pos: &GlobalVoxelPos) -> u32 {
    (lattice_hash(seed, pos) >> 32) as u32
}
