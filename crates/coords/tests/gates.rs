//! The Phase 0 gate harness for docs/02. Each test is named for the gate it *is*, so a CI line
//! like `test c1_region_local_round_trip ... ok` is the evidence the gate wants — no separate
//! reporting layer.

use contracts::coords::Axis;
use coords::{lattice::lattice_hash, GlobalVoxelPos, RegionKey, RENDER_RADIUS};

fn pos(x: i64, y: i64, z: i64) -> GlobalVoxelPos {
    GlobalVoxelPos { x, y, z }
}

/// Samples every axis value that could plausibly break floor semantics, then covers the whole
/// `[−2^34, 2^34]` span coarsely. An exhaustive sweep is gate C1's *spike* version; this is the
/// regression version that runs on every commit.
fn axis_samples() -> Vec<i64> {
    let mut v: Vec<i64> = Vec::new();
    // Every value in a window wide enough to cross several region boundaries in both directions.
    v.extend(-50_000i64..=50_000);
    // Boundaries: exact multiples of the region size, ±1, and the far ends of the tested span.
    let span: i64 = 1 << 34;
    for k in (-4096i64..=4096).chain([
        -4_194_304, -1_048_576, -4096, -1, 0, 1, 4096, 1_048_576, 4_194_304,
    ]) {
        let base = k.wrapping_mul(RENDER_RADIUS);
        for d in -2i64..=2 {
            for scale in [1i64, -1] {
                let candidate = scale.saturating_mul(base.saturating_add(d));
                if candidate.abs() <= span {
                    v.push(candidate);
                }
            }
        }
    }
    // A deterministic pseudo-random scatter across the span (LCG, no floats, no clock).
    let mut state = 0x1234_5678_9abc_def0u64;
    for _ in 0..200_000 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let spread = (state >> 11) as i64 % (span + 1);
        v.push(if state & 1 == 0 { spread } else { -spread });
    }
    v.push(-span);
    v.push(span);
    v
}

/// Gate C1: `region * 4096 + offset == pos` for every sampled axis value, negatives included.
#[test]
fn c1_region_local_round_trip() {
    let samples = axis_samples();
    let size = GlobalVoxelPos::REGION_SIZE;
    for &a in &samples {
        let p = pos(a, a, a);
        let r = p.region_key();
        let l = p.local_offset();
        assert_eq!(r.x * size + l.x as i64, a, "x = {a}");
        assert_eq!(r.y * size + l.y as i64, a, "y = {a}");
        assert_eq!(r.z * size + l.z as i64, a, "z = {a}");
        assert!(
            l.x < 4096 && l.y < 4096 && l.z < 4096,
            "offset escaped its 12-bit range at {a}: {l:?}"
        );
    }
}

/// Gate C1's floor requirement stated directly: region indexing must agree with `div_euclid`,
/// which is the other spelling docs/02 allows. `trunc` division is what breaks hashes across zero.
#[test]
fn c1_region_index_is_floor_not_trunc() {
    for &a in &axis_samples() {
        assert_eq!(
            a >> GlobalVoxelPos::REGION_SHIFT,
            a.div_euclid(size_const()),
            "x = {a}"
        );
    }
    // And the asymmetric failure mode itself, so a future edit cannot choose it by accident:
    // trunc division would fold -1 into region 0, straddling -4095..4095.
    assert_eq!(-1i64 / 4096, 0);
    assert_eq!((-1i64).div_euclid(4096), -1);
    assert_eq!(
        pos(-1, 0, 0).region_key().x,
        -1,
        "region_key must be floor-based"
    );
}

const fn size_const() -> i64 {
    GlobalVoxelPos::REGION_SIZE
}

/// SPEC-01: the width is `u16`, the valid range is 12 bits, and out-of-range inputs are **masked,
/// never clamped** — `-1` becomes `4095`, which is exactly what the C1 round trip needs.
#[test]
fn spec01_local_offset_masks_instead_of_clamping() {
    assert_eq!(pos(-1, -1, -1).local_offset().x, 4095);
    assert_eq!(pos(-1, -1, -1).region_key().x, -1);
    assert_eq!(pos(-4096, 0, 0).local_offset().x, 0);
    assert_eq!(pos(-4096, 0, 0).region_key().x, -1);
    assert_eq!(pos(4095, 0, 0).local_offset().x, 4095);
    assert_eq!(pos(4096, 0, 0).local_offset().x, 0);
    assert_eq!(pos(4096, 0, 0).region_key().x, 1);
}

/// Region → sector: 16³ regions per `SectorKey`, floor-based on both axes.
#[test]
fn sector_key_groups_sixteen_regions_per_axis() {
    assert_eq!(RegionKey { x: 15, y: 0, z: 0 }.sector().x, 0);
    assert_eq!(RegionKey { x: 16, y: 0, z: 0 }.sector().x, 1);
    assert_eq!(
        RegionKey {
            x: -1,
            y: -1,
            z: -1
        }
        .sector()
        .x,
        -1
    );
    assert_eq!(pos(16 * 4096, 0, 0).region_key().sector().x, 1);
}

/// Gate C2 (part 1): the lattice hash is a pure function — same `(seed, pos)`, same bits, and no
/// dependence on iteration order or allocation. Cross-target agreement is asserted against the
/// literal vectors in `c2_test_vectors_are_frozen`.
#[test]
fn c2_lattice_hash_is_pure_and_seed_sensitive() {
    let a = pos(50_000_000, 64, -12_000_000);
    let b = pos(50_000_000, 64, -12_000_000);
    assert_eq!(lattice_hash(0x5eed, &a), lattice_hash(0x5eed, &b));
    assert_ne!(lattice_hash(0x5eed, &a), lattice_hash(0x5ee0, &a));
    // Adjacent voxels must not share structure (avalanche), else the lattice is not a hash.
    let mut differing_bits = 0;
    for bit in 0..64 {
        let h0 = lattice_hash(0x5eed, &pos(1000, 1000, 1000));
        let h1 = lattice_hash(0x5eed, &pos(1001, 1000, 1000));
        if (h0 ^ h1) & (1 << bit) != 0 {
            differing_bits += 1;
        }
    }
    assert!(
        differing_bits >= 26,
        "weak avalanche: only {differing_bits}/64 bits flipped between neighbours"
    );
}

/// Gate C2 (part 2): the vectors. Generated on x86-64 (see `cargo run --example gate_vectors`);
/// ARM64 and wasm32 CI jobs must reproduce these exact bytes or the run is a determinism failure.
#[test]
fn c2_test_vectors_are_frozen() {
    // Generated on x86-64 (rustc 1.92.0) by `cargo run --example gate_vectors`.
    let vectors: [(u64, [i64; 3], u64); 6] = [
        (0, [0, 0, 0], 0xa465_9597_a9d8_aeca),
        (0, [50_000_000, 64, -12_000_000], 0x0efd_8daf_4bed_9faa),
        (1, [0, 0, 0], 0xff0d_a13f_3d3a_3f07),
        (0x5eed_1234_cafe_f00d, [-1, -2, -3], 0x33c9_0d2f_da54_9bc5),
        (
            0xffff_ffff_ffff_ffff,
            [1 << 33, -(1 << 33), 4095],
            0x0170_bd6a_6239_8861,
        ),
        (0x5eed, [50_000_000, 64, -12_000_000], 0x8e6d_a94e_370c_bc70),
    ];
    for (seed, [x, y, z], expected) in vectors {
        let got = lattice_hash(seed, &pos(x, y, z));
        assert_eq!(got, expected, "lattice_hash({seed:#x}, [{x}, {y}, {z}])");
    }
}

/// Gate C3: the Far Lands regression. At `x = 10^9` the camera-relative path must still resolve
/// single-voxel steps exactly — an integer subtraction then one `i64 → f32` cast.
#[test]
fn c3_no_jitter_at_1e9() {
    let camera = pos(1_000_000_000, 1_000_000_000, 1_000_000_000);
    for delta in 0..2_048i64 {
        let p = pos(camera.x + delta, camera.y - delta, camera.z + delta * 3);
        let local = p.to_local_camera_space(&camera);
        assert_eq!(local[0], delta as f32, "delta {delta} on x");
        assert_eq!(local[1], -(delta as f32), "delta {delta} on y");
        assert_eq!(local[2], (delta * 3) as f32, "delta {delta} on z");
    }
}

/// Exact `f32` spacing at a magnitude — the same definition `examples/camera_f32_demo.rs` prints,
/// so the gate and the demo cannot drift apart.
fn spacing(value: f32) -> f64 {
    let v = value.abs();
    f64::from(f32::from_bits(v.to_bits() + 1) - v)
}

/// Gate C4: camera-space error ≤ 1 f32 ulp at the declared radius. `f32` represents every integer
/// below 2^24 exactly, so inside `RENDER_RADIUS` the correct path has zero error; the bound is
/// stated in ulp so the gate survives a larger radius.
#[test]
fn c4_camera_error_within_one_ulp() {
    let camera = pos(1_000_000_000, 0, -1_000_000_000);
    let mut worst_ratio = 0.0f64;
    for delta in -RENDER_RADIUS..=RENDER_RADIUS {
        let p = pos(camera.x + delta, camera.y, camera.z);
        let local_f32 = p.to_local_camera_space(&camera)[0];
        let local = f64::from(local_f32);
        let exact = delta as f64;
        let error = (local - exact).abs();
        let ulp = spacing(local_f32.max(1.0));
        worst_ratio = worst_ratio.max(error / ulp);
        assert!(
            error <= ulp,
            "gate C4 exceeded at delta {delta}: error {error} > 1 ulp ({ulp})"
        );
    }
    assert_eq!(
        worst_ratio, 0.0,
        "integer deltas inside the radius must be exact"
    );
}

/// Gate C4's counterfactual, so the gate cannot be "fixed" by reordering the cast and the
/// subtraction. Casting globals to `f32` first is the failure docs/02 calls the most common way to
/// break this design; at `10^9` it is catastrophically wrong, not subtly wrong.
#[test]
fn c4_casting_globals_to_f32_first_is_the_far_lands_bug() {
    let camera = pos(1_000_000_000, 0, 0);
    let p = pos(camera.x + 1, 0, 0);
    let correct = p.to_local_camera_space(&camera)[0];
    let wrong = (p.x as f32) - (camera.x as f32);
    assert_eq!(correct, 1.0);
    assert!(
        (wrong - 1.0).abs() >= 1.0,
        "the float-first path was expected to miss the voxel by at least a whole step, got {wrong}"
    );
}

/// The slab key must stay 3-axis aware: `Axis` is part of identity, so an X-plane and a Y-plane in
/// the same region are different slabs. Pending SPEC-09 for what the slab actually contains.
#[test]
fn slab_key_identity_includes_axis_and_plane() {
    use contracts::coords::SlabKey;
    let region = pos(0, 0, 0).region_key();
    let x_plane = SlabKey {
        region,
        axis: Axis::X,
        plane: 7,
    };
    let y_plane = SlabKey {
        region,
        axis: Axis::Y,
        plane: 7,
    };
    assert_ne!(x_plane, y_plane);
    assert_ne!(
        x_plane,
        SlabKey {
            region,
            axis: Axis::X,
            plane: 8
        }
    );
}

/// Maintainability guard, not a design gate: docs/02 § The Rust contract mirrors this file, and a
/// doc that quietly diverges from the contract is the failure mode AGENTS.md §8 calls a broken
/// build. Comments may drift; code may not.
#[test]
fn contract_matches_spec_document() {
    let doc = include_str!("../../../docs/02-coordinate-system.md");
    let contract = include_str!("../../../contracts/coords.rs");
    let code_only = |text: &str| -> Vec<String> {
        text.lines()
            .map(str::trim_end)
            .map(str::to_owned)
            .filter(|l| !l.is_empty() && !l.starts_with("//"))
            .collect()
    };
    let block = doc
        .split("```rust")
        .find(|b| b.contains("pub struct GlobalVoxelPos"))
        .and_then(|b| b.split("```").next())
        .expect("docs/02 must keep a ```rust block holding the A-01 contract");
    let from_doc = code_only(block);
    let from_file = code_only(contract);
    assert_eq!(
        from_doc, from_file,
        "contracts/coords.rs and docs/02 § The Rust contract have diverged — fix whichever is wrong"
    );
}
