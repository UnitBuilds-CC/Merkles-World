//! The raster probe: what the field looks like when a whole grid of rays asks the same question.
//!
//! `tests/scene.rs` measures single rays. This file measures coverage - the fraction of a frame that
//! finds surface, the depth the centre pixel reports, and whether the picture responds when the geometry
//! changes. Those are the properties a WGSL interpreter and a BVH will have to reproduce later, and they
//! are checkable today on a CPU.
//!
//! The expected numbers are computed here from the pinhole geometry, not pasted from a run: a sphere of
//! radius `r` at distance `d` subtends `tan(asin(r / d))` in image units at unit distance, and the camera
//! below maps that onto pixel offsets with a half-height of `0.5`. If the projection or the march drifts,
//! the measured disc stops agreeing with that expression.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SMOOTH_UNION, NODE_SPHERE,
};
use render::raster::{render, Camera, Frame};
use render::validate;
use render::Tree;

const SIDE: u32 = 64;
const DISTANCE: f32 = 6.0;
const STEP_BUDGET: u32 = 96;

/// A camera at `(0, 0, -6)` looking down +z at the origin, with the pinhole's tan-mapping and no trig.
fn camera() -> Camera {
    Camera {
        origin: [0.0, 0.0, -DISTANCE],
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: SIDE,
        height: SIDE,
        max_t: 20.0,
        surface: 1e-3,
        max_steps: STEP_BUDGET,
    }
}

fn sphere(radius: f32) -> Tree {
    let params = vec![radius, 0.0, 0.0, 0.0];
    Tree {
        nodes: vec![NodeRecord([
            pack_kind_and_arity(NODE_SPHERE, 0),
            1,
            pack_first_and_flags(0, FLAG_IS_ROOT),
            pack_params_len(params.len() as u16),
        ])],
        params,
    }
}

/// Two spheres blended and unioned with a third: the smallest tree that is a *scene* rather than a
/// primitive, so the probe is exercised on a walk with children.
fn scene() -> Tree {
    let left = NodeRecord([
        pack_kind_and_arity(NODE_SPHERE, 0),
        1,
        pack_first_and_flags(0, 0),
        pack_params_len(4),
    ]);
    let right = NodeRecord([
        pack_kind_and_arity(NODE_SPHERE, 0),
        1,
        pack_first_and_flags(4, 0),
        pack_params_len(4),
    ]);
    let blended = NodeRecord([
        pack_kind_and_arity(NODE_SMOOTH_UNION, 2),
        3,
        pack_first_and_flags(8, 0),
        pack_params_len(1),
    ]);
    let block = NodeRecord([
        pack_kind_and_arity(NODE_SPHERE, 0),
        1,
        pack_first_and_flags(9, 0),
        pack_params_len(4),
    ]);
    let whole = NodeRecord([
        pack_kind_and_arity(NODE_SMOOTH_UNION, 2),
        5,
        pack_first_and_flags(13, FLAG_IS_ROOT),
        pack_params_len(1),
    ]);
    let params = vec![
        0.8, -0.9, 0.0, 0.0, // left bead
        0.8, 0.9, 0.0, 0.0, // right bead
        0.4, // their blend
        1.1, 0.0, 0.0, 0.0, // the block it is blended with
        0.5, // and the outer blend
    ];
    let tree = Tree {
        nodes: vec![whole, block, blended, left, right],
        params,
    };
    // Order matters here: an invalid tree renders into a plausible picture, so the geometry tests below
    // are only evidence if the input is a legal A-07 tree first. That is not hypothetical - the first
    // draft of this helper stamped `FLAG_IS_ROOT` on a child as well as the root, and the validator
    // answered `RootCount(2)` instead of letting three tests march a tree that two implementations would
    // disagree about.
    validate(&tree).expect("the scene validates");
    tree
}

#[test]
fn a_sphere_covers_the_pixels_it_subtends() {
    let tree = sphere(1.0);
    let (frame, stats) = render(&tree, &camera());
    assert_eq!(stats.rays, SIDE * SIDE);
    assert_eq!(frame.hit_count(), stats.hits);

    // tan(asin(r / d)) is the projected radius in image units at unit distance; half_height maps it to
    // pixels. r = 1, d = 6 -> 0.16904 image units -> 0.16904 / (2 * 0.5) * 64 = 10.82 pixels.
    let radius = (1.0f32 / DISTANCE).asin().tan();
    let expected_px = radius / (2.0 * 0.5) * SIDE as f32;
    // The comparison is in `f64` so the tolerance measures the march's silhouette, not the rounding of a
    // 400-pixel area held in single precision.
    let expected_px = f64::from(expected_px);
    let expected_area = core::f64::consts::PI * expected_px * expected_px;
    let measured = f64::from(stats.hits);
    let error = (measured - expected_area).abs() / expected_area;
    assert!(
        error < 0.08,
        "a unit sphere at six units should cover about {expected_area:.0} of {} pixels, saw {measured:.0}",
        SIDE * SIDE
    );
}

#[test]
fn the_centre_pixel_reports_the_depth_the_geometry_promises() {
    let tree = sphere(1.0);
    let (frame, _) = render(&tree, &camera());
    let centre = SIDE / 2;
    let depth = frame.depth_at(centre, centre);
    assert!(
        (depth - (DISTANCE - 1.0)).abs() < 0.02,
        "the near side of a unit sphere at six units is at t = 5, saw {depth}"
    );
}

#[test]
fn a_ray_aimed_past_the_scene_finds_nothing() {
    // Small and far: the corner of a 64-pixel frame at half_height 0.5 looks off at roughly 45 degrees,
    // while a 0.2 sphere at six units projects to about two pixels of radius. So the axis rays find it
    // and the corner rays report absence - a marcher that never gives up would claim both.
    let tree = sphere(0.2);
    let (frame, stats) = render(&tree, &camera());
    assert_eq!(
        frame.depth_at(0, 0),
        Frame::NO_HIT,
        "the corner ray must not report surface it cannot see"
    );
    let centre = SIDE / 2;
    assert!(
        (frame.depth_at(centre, centre) - (DISTANCE - 0.2)).abs() < 0.02,
        "and the axis ray still finds the near side"
    );
    assert!(
        stats.hits > 0 && stats.hits < stats.rays / 4,
        "a 0.2 sphere is a dot, not the frame: {} hits of {} rays",
        stats.hits,
        stats.rays
    );
}

#[test]
fn changing_the_shape_changes_the_picture() {
    // The claim the whole exercise exists to make checkable: pixels respond to geometry. If a bigger
    // sphere did not cover more of the frame and get nearer, either the projection, the march, or the
    // tree indexing is quietly wrong - and a wrong picture is what that looks like in production.
    let (small, small_stats) = render(&sphere(1.0), &camera());
    let (large, large_stats) = render(&sphere(2.0), &camera());
    assert!(
        large_stats.hits > small_stats.hits,
        "a doubled radius must cover more: {} vs {}",
        large_stats.hits,
        small_stats.hits
    );
    let centre = SIDE / 2;
    assert!(
        large.depth_at(centre, centre) < small.depth_at(centre, centre),
        "and its surface is nearer: {} vs {}",
        large.depth_at(centre, centre),
        small.depth_at(centre, centre)
    );
    assert_ne!(
        small.pixels, large.pixels,
        "two different shapes must not render one image"
    );
}

#[test]
fn a_scene_of_several_nodes_renders_and_differs_from_its_parts() {
    let (frame, stats) = render(&scene(), &camera());
    assert!(stats.hits > 0, "the scene must occupy some of the frame");
    let (_bead, bead_stats) = render(&sphere(0.8), &camera());
    assert!(
        stats.hits > bead_stats.hits,
        "three blended beads cover more than one: {} vs {}",
        stats.hits,
        bead_stats.hits
    );
    assert_eq!(
        frame.pixels.len(),
        (stats.rays as usize) * 3,
        "three bytes per pixel, no more"
    );
    assert_eq!(frame.depth.len(), stats.rays as usize);
}

#[test]
fn the_probe_is_reproducible_pixel_for_pixel() {
    // Presentation output, nothing hashed - but a renderer that disagrees with itself frame to frame is
    // untestable, so this is worth pinning even though a differing root would not be a determinism bug.
    let tree = scene();
    let (a, a_stats) = render(&tree, &camera());
    let (b, b_stats) = render(&tree, &camera());
    assert_eq!(a, b, "two renders of one tree must be one image");
    assert_eq!(a_stats, b_stats);
}

#[test]
fn evaluations_are_charged_within_the_budget_they_declare() {
    let tree = scene();
    let (frame, stats) = render(&tree, &camera());
    let rays = u64::from(stats.rays);
    // Every ray evaluates at least once at its own origin, and no ray can spend more than the budget -
    // the exact inequality the compile-time budget pass (gate AS5) will one day enforce per tree.
    assert!(
        stats.evaluations >= rays && stats.evaluations <= rays * u64::from(STEP_BUDGET),
        "evaluations {} outside [{rays}, {}]",
        stats.evaluations,
        rays * u64::from(STEP_BUDGET)
    );
    assert_eq!(frame.width * frame.height, stats.rays);
}
