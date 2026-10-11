//! Phase 0's camera-relative f32 demo — docs/11 says "this demo **is** gate C4's run", so it
//! prints the numbers docs/02 claims instead of asserting them quietly.
//!
//! Two paths are measured at the same world position, `x = 10^9`. The spacing each path can
//! resolve is the whole difference between a working engine and a Far Lands regression:
//!   * the contract's path — subtract the camera anchor in `i64`, cast once to `f32`;
//!   * the tempting path — cast the globals to `f32`, subtract afterwards.

use coords::{GlobalVoxelPos, RENDER_RADIUS};

/// Exact `f32` spacing at a magnitude: the gap to the next representable value. Unlike
/// `EPSILON * |x|` this never overstates the budget by up to 2x.
fn spacing(value: f32) -> f64 {
    let v = value.abs();
    let next = f32::from_bits(v.to_bits() + 1);
    f64::from(next - v)
}

fn main() {
    let camera = GlobalVoxelPos {
        x: 1_000_000_000,
        y: 1_000_000_000,
        z: 1_000_000_000,
    };
    println!("camera anchor (global i64): {camera:?}");
    println!("declared render radius: {RENDER_RADIUS} voxels (docs/02 gate C4)");

    for radius in [512i64, 2048, 4096] {
        let mut worst_error = 0.0f64;
        for step in 0..=radius {
            for delta in [step, -step] {
                let p = GlobalVoxelPos {
                    x: camera.x + delta,
                    y: camera.y + delta,
                    z: camera.z + delta,
                };
                for &got in &p.to_local_camera_space(&camera) {
                    let error = (f64::from(got) - delta as f64).abs();
                    if error > worst_error {
                        worst_error = error;
                    }
                    assert!(
                        error <= spacing(got.max(1.0)),
                        "gate C4 broken: delta {delta} -> {got} (error {error})"
                    );
                }
            }
        }
        let ulp = spacing(radius as f32);
        let ratio = worst_error / ulp;
        println!(
            "radius {radius:>5}:  1 ulp = {ulp:e}   max |f32 - exact| = {worst_error:e}   ratio = {ratio:.1} ulp"
        );
    }

    let neighbour = GlobalVoxelPos {
        x: camera.x + 1,
        y: camera.y,
        z: camera.z,
    };
    let correct = neighbour.to_local_camera_space(&camera)[0];
    let float_first = (neighbour.x as f32) - (camera.x as f32);
    println!("\none voxel step at x = 1e9:");
    println!("  integer-then-cast (the contract): {correct}");
    println!("  cast-then-subtract (the bug)    : {float_first}");
    println!(
        "  f32 spacing at 1e9 is {} voxels, so the second path cannot see a single block",
        spacing(camera.x as f32)
    );

    let mut collapsed = 0;
    for delta in 0..4096i64 {
        if (camera.x + delta) as f32 == (camera.x + delta + 1) as f32 {
            collapsed += 1;
        }
    }
    println!(
        "\nconsecutive global positions indistinguishable as f32: {collapsed}/4096 \
         (the reason region keys are integers)"
    );
}
