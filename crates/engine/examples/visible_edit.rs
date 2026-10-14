//! The edit-to-geometry step: an edit that the ledger accepted must arrive in the frame, and by how much it
//! arrives must be a number rather than a hope.
//!
//! `cargo run --release --example visible_edit -p engine`
//!
//! Gate P1.7's claim is "no invisible state" — an edit is committed, re-read, and *seen*. The first two thirds
//! were demonstrated long ago (the boot commits, roots, and reads back); the third waited on OQ-50, which is
//! ruled: doc 02 says a coordinate names `[x, x+1)` and the centre is therefore `+0.5`. Nothing in the
//! workspace chose that before the ruling — `contracts::delta::voxel_at` is pure index arithmetic, and no code
//! turned a voxel into a world position at all — so this is the first consumer, and it derives the centre from
//! doc 02's row rather than restating an offset it invented.
//!
//! What it prints is deliberately two distances, because that is where the honest answer lives. A voxel is one
//! world unit: close, the edit is unmistakable; across a chunk it is sub-pixel, and OQ-56's angular arithmetic
//! says the frame *should* not change. A demo that only ever showed the close case would be proving the easy
//! half, so both are reported, and the assertions are stated as the weaker true ones: the edit changes pixels
//! near, and does not fabricate a surface far.

use contracts::coords::LocalOffset;
use contracts::delta::{voxel_at, RegionSlot};
use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use memory::{Slab, SLAB_64KB_RECORDS};
use render::raster::{render_bnb, Camera, Frame};
use render::Tree;

/// Region slot 0, same as the boot's: the ledger's own first-touched key.
const SLOT_ZERO: contracts::coords::RegionKey = contracts::coords::RegionKey { x: 0, y: 0, z: 0 };

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// doc 02, "Regions and addressing": a coordinate names `[x, x+1) x [y, y+1) x [z, z+1)`, so the centre of the
/// voxel it denotes is the corner plus a half on every axis. This is the *only* place in the workspace that
/// turns a voxel coordinate into a world position, and it cites the row instead of paraphrasing arithmetic.
fn voxel_centre(position: &contracts::coords::GlobalVoxelPos) -> [f32; 3] {
    [
        position.x as f32 + 0.5,
        position.y as f32 + 0.5,
        position.z as f32 + 0.5,
    ]
}

/// The terrain the edit lands in: one wide slab centred on the origin with its top surface at z = +0.5, so a
/// voxel at z = 1 has its centre half a unit above it and the edit is unambiguous in the frame. A-07's box
/// takes no centre parameter, which is the same limitation the note on `terrain_with_edit` reports.
fn terrain() -> Tree {
    Tree {
        nodes: vec![record(NODE_ROUNDED_BOX, 0, 1, 0, 4, true)],
        params: vec![2048.0, 2048.0, 0.5, 0.0],
    }
}

/// The same terrain with the edited voxel unioned in.
///
/// A-07 has no translate node (that is OQ-55, still open), and `NODE_ROUNDED_BOX` carries no centre — so the
/// voxel is drawn as its inscribed ball, radius 0.5 at the derived centre. That substitution is stated rather
/// than hidden: the *placement* is what is being tested here, and a true cube would need the instance node
/// OQ-55 asks for.
fn terrain_with_edit(centre: [f32; 3]) -> Tree {
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 3, 0, 0, true),
            record(NODE_ROUNDED_BOX, 0, 1, 0, 4, false),
            record(NODE_SPHERE, 0, 1, 4, 4, false),
        ],
        params: vec![
            2048.0, 2048.0, 0.5, 0.0, 0.5, centre[0], centre[1], centre[2],
        ],
    }
}

/// Straight down at the edited voxel, from `height` above it.
fn camera_at(centre: [f32; 3], height: f32) -> Camera {
    Camera {
        origin: [centre[0], centre[1], centre[2] + height],
        forward: [0.0, 0.0, -1.0],
        right: [0.0, 1.0, 0.0],
        up: [1.0, 0.0, 0.0],
        half_height: 0.5,
        width: 64,
        height: 48,
        max_t: height * 3.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

/// Pixels whose depth moved by more than the march's own tolerance, and the two centre-pixel depths that
/// decide the claim. Sub-tolerance differences are reported separately because a bigger root enclosure changes
/// the descent's step sequence across the whole frame without moving any surface a player could see.
fn changed_pixels(without: &Frame, with: &Frame, tolerance: f32) -> (usize, usize, f32, f32) {
    let rays = without.depth.len();
    let mut beyond = 0usize;
    let mut jitter = 0usize;
    for index in 0..rays {
        let a = without.depth[index];
        let b = with.depth[index];
        if a == b {
            continue;
        }
        let delta = if a == Frame::NO_HIT || b == Frame::NO_HIT {
            f32::INFINITY
        } else {
            (a - b).abs()
        };
        if delta > tolerance {
            beyond += 1;
        } else {
            jitter += 1;
        }
    }
    // The pixel straight down the camera's axis: row and column centres, not `len / 2`, which is the first
    // column of the middle row and lands on an oblique ray.
    let centre_pixel = (with.height / 2) * with.width + with.width / 2;
    (
        beyond,
        jitter,
        without.depth[centre_pixel as usize],
        with.depth[centre_pixel as usize],
    )
}

fn main() {
    // ---- 1. an edit, through the same path the boot uses ----------------------------------------------
    let edited = LocalOffset { x: 3, y: 5, z: 1 };
    let mut buffer = vec![0u64; SLAB_64KB_RECORDS];
    let mut slab = Slab::new(&mut buffer);
    slab.append(contracts::delta::DeltaRecord::new(
        RegionSlot(0),
        edited,
        0x00c0,
    ))
    .expect("one record fits a 64 KB slab");
    let read_back: Vec<_> = slab.deltas().collect();
    assert_eq!(
        read_back.len(),
        1,
        "the slab must hand back the edit it took"
    );

    let position = voxel_at(&SLOT_ZERO, &read_back[0].intra_region());
    let centre = voxel_centre(&position);
    println!("// visible edit (P1.7): committed -> read back -> placed -> marched");
    println!(
        "//   edit         : slot 0 at local ({}, {}, {}), payload 0x{:04x}",
        edited.x,
        edited.y,
        edited.z,
        read_back[0].payload()
    );
    println!(
        "//   voxel        : global ({}, {}, {})",
        position.x, position.y, position.z
    );
    println!(
        "//   centre       : ({}, {}, {}) - doc 02's `[x, x+1)`, so corner + 0.5, derived in one place",
        centre[0], centre[1], centre[2]
    );

    // The convention, asserted rather than assumed: the half-open span puts this voxel's centre half a unit
    // off the integer on every axis. The rejected alternative (a cube centred on the coordinate) would place
    // geometry a full half-unit away per axis, which is what the ruling was about.
    assert_eq!(
        centre,
        [
            position.x as f32 + 0.5,
            position.y as f32 + 0.5,
            position.z as f32 + 0.5
        ],
        "the centre must come from doc 02's span, not from an invented offset"
    );

    // ---- 2. it must change the frame, at the distance where one world unit is visible ------------------
    let height = 8.0f32;
    let camera = camera_at(centre, height);
    let tolerance = camera.surface * 100.0;
    let without = render_bnb(&terrain(), &camera);
    let with = render_bnb(&terrain_with_edit(centre), &camera);
    let (beyond, jitter, depth_before, depth_after) =
        changed_pixels(&without.0, &with.0, tolerance);
    // The terrain's top is z = +0.5 and the ball's top is z = +2.0, both directly under the eye, so the two
    // centre depths are arithmetic the frame has to agree with: this is the placement check, and it is exact.
    let expect_before = height + 1.0;
    let expect_after = height - 0.5;
    println!(
        "//   at {height} units : {beyond} pixels moved beyond the march tolerance ({jitter} sub-tolerance), centre ray {:.3} -> {:.3} units where doc 02's placement says {:.3} -> {:.3}",
        depth_before, depth_after, expect_before, expect_after
    );
    assert!(
        beyond > 0,
        "an edit the ledger accepted moved no surface in the frame - the invisible state P1.7 forbids"
    );
    assert!(
        (depth_before - expect_before).abs() < 0.05 && (depth_after - expect_after).abs() < 0.05,
        "the frame put the surface where something other than doc 02's span says it should be"
    );

    // ---- 3. and it must NOT look like a bug at range, where one unit is sub-pixel ----------------------
    let far = 512.0f32;
    let without_far = render_bnb(&terrain(), &camera_at(centre, far));
    let with_far = render_bnb(&terrain_with_edit(centre), &camera_at(centre, far));
    let (beyond_far, jitter_far, _, _) = changed_pixels(&without_far.0, &with_far.0, tolerance);
    println!(
        "//   at {far} units : {beyond_far} pixels moved beyond tolerance ({jitter_far} sub-tolerance) - one world unit is {:.3} of this {}-pixel frame, so an edit at chunk range is meant to be invisible, and a frame that lit up would be the bug",
        1.0 / (far * 2.0) * 64.0,
        without_far.0.width
    );
    assert!(
        beyond_far <= 8,
        "a one-voxel edit resolvable from {far} units away would contradict OQ-56's angular-size measurement"
    );
    println!("//   not claimed    : this is placement and visibility, not a root - nothing here is hashed, so no");
    println!("//                    P1.5 or R4 claim attaches (ADR-017), and the cube would need OQ-55's instance");
    println!("//                    node. A-07's box has no centre, so the edit is drawn as its inscribed ball.");
    println!("// boot complete. An accepted edit arrives in the frame at the distance where a voxel is resolvable.");
}
