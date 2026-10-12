//! Reusing a frame must not be a second renderer.
//!
//! `examples/allocation_audit.rs` reports the memory story; this file pins the behaviour that makes the
//! story worth anything. A `*_into` entry point that quietly produced a different picture - a stale depth
//! from a previous frame, a buffer that kept the old resolution's pixels - would be the worst kind of
//! hot-path optimisation, because the first symptom is a frame that looks fine. So every test here
//! compares a reused frame against a freshly allocated one, pixel for pixel and stat for stat.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE, NODE_UNION,
};
use render::cull::scene_bounds;
use render::raster::{
    render, render_bnb, render_bnb_into, render_bnb_with, render_culled, render_culled_into,
    render_into, Camera, Frame,
};
use render::Tree;

fn sphere(params_first: u16) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(NODE_SPHERE, 0),
        1,
        pack_first_and_flags(params_first, 0),
        pack_params_len(4),
    ])
}

fn union(subtree_len: u32, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(NODE_UNION, 2),
        subtree_len,
        pack_first_and_flags(0, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(0),
    ])
}

fn scene() -> Tree {
    let tree = Tree {
        nodes: vec![
            union(7, true),
            union(3, false),
            sphere(0),
            sphere(4),
            union(3, false),
            sphere(8),
            sphere(12),
        ],
        params: vec![
            0.4, -2.0, 0.0, 0.0, //
            0.4, -0.5, 0.0, 0.0, //
            0.4, 1.0, 0.0, 0.0, //
            0.4, 2.5, 0.0, 0.0,
        ],
    };
    render::validate(&tree).expect("the fixture scene is a valid A-07 tree");
    tree
}

fn camera(width: u32, height: u32) -> Camera {
    Camera {
        origin: [0.0, 0.0, -6.0],
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width,
        height,
        max_t: 20.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

#[test]
fn a_reused_frame_renders_the_same_picture_as_a_fresh_one() {
    let tree = scene();
    let camera = camera(64, 48);
    let (owning, owning_stats) = render(&tree, &camera);
    let mut reused = Frame::new(0, 0);
    let stats = render_into(&tree, &camera, &mut reused);

    assert_eq!(
        owning.pixels, reused.pixels,
        "reusing a frame changed pixels"
    );
    assert_eq!(
        owning.depth, reused.depth,
        "reusing a frame left stale depth"
    );
    assert_eq!(owning.hit_count(), reused.hit_count());
    assert!(
        owning.hit_count() > 0,
        "the fixture must actually hit something"
    );
    assert_eq!(owning_stats, stats);
    assert_eq!(reused.width, 64);
    assert_eq!(reused.height, 48);
}

#[test]
fn a_reused_frame_stops_growing_its_buffers() {
    // The measurable half of AS4: std guarantees a push that fits the capacity does not allocate, so
    // capacity that never moves across renders is the claim, not a guess about it.
    let tree = scene();
    let camera = camera(64, 48);
    let mut reused = Frame::new(camera.width, camera.height);
    let first = {
        render_into(&tree, &camera, &mut reused);
        (reused.pixels.capacity(), reused.depth.capacity())
    };
    for _ in 0..5 {
        render_into(&tree, &camera, &mut reused);
        assert_eq!(
            first,
            (reused.pixels.capacity(), reused.depth.capacity()),
            "a steady-resolution frame re-allocated its buffers"
        );
        assert_eq!(reused.pixels.len(), 64 * 48 * 3);
        assert_eq!(reused.depth.len(), 64 * 48);
    }
}

#[test]
fn a_shrinking_viewport_reuses_the_larger_buffers() {
    // `prepare` grows and never shrinks on purpose: releasing memory the next frame asks for again is the
    // churn the hot-path ban is about. The picture still has to be right at the new size.
    let tree = scene();
    let big = camera(64, 48);
    let small = camera(16, 12);
    let mut reused = Frame::new(big.width, big.height);
    render_into(&tree, &big, &mut reused);
    let grown = (reused.pixels.capacity(), reused.depth.capacity());

    render_into(&tree, &small, &mut reused);
    let (owning, _) = render(&tree, &small);
    assert_eq!(
        (reused.pixels.capacity(), reused.depth.capacity()),
        grown,
        "the smaller viewport freed buffers it will be asked to allocate again"
    );
    assert_eq!(reused.pixels, owning.pixels);
    assert_eq!(reused.depth, owning.depth);
    assert_eq!(reused.width, 16);
}

#[test]
fn every_culled_path_agrees_with_the_unculled_one_through_a_reused_frame() {
    let tree = scene();
    let camera = camera(48, 36);
    let (plain, _) = render(&tree, &camera);

    let mut reused = Frame::new(0, 0);
    render_culled_into(&tree, &camera, &mut reused);
    assert_eq!(
        plain.pixels, reused.pixels,
        "the ray early-out changed pixels"
    );
    assert_eq!(plain.depth, reused.depth, "the ray early-out changed depth");

    let (fresh, fresh_stats) = render_culled(&tree, &camera);
    let mut again = Frame::new(0, 0);
    let reused_stats = render_culled_into(&tree, &camera, &mut again);
    assert_eq!(
        fresh.pixels, again.pixels,
        "allocating and reusing diverged"
    );
    assert_eq!(
        fresh.depth, again.depth,
        "allocating and reusing diverged on depth"
    );
    assert_eq!(
        fresh_stats, reused_stats,
        "the reusing form reported different work"
    );

    let mut reused = Frame::new(0, 0);
    render_bnb_into(&tree, &camera, &mut reused);
    assert_eq!(
        plain.pixels, reused.pixels,
        "the per-node skips changed pixels"
    );
    assert_eq!(
        plain.depth, reused.depth,
        "the per-node skips changed depth"
    );
}

#[test]
fn a_hoisted_bound_table_is_the_same_render_as_a_fresh_one() {
    let tree = scene();
    let camera = camera(48, 36);
    let bounds = scene_bounds(&tree);
    let (fresh, fresh_stats) = render_bnb(&tree, &camera);
    let mut reused = Frame::new(camera.width, camera.height);
    let hoisted_stats = render_bnb_with(&tree, &camera, &mut reused, &bounds);
    assert_eq!(fresh.pixels, reused.pixels);
    assert_eq!(fresh.depth, reused.depth);
    assert_eq!(
        fresh_stats, hoisted_stats,
        "hoisting the table changed the report"
    );
}

#[test]
fn a_frame_can_be_rendered_twice_and_the_second_write_does_not_accumulate() {
    // The failure mode this exists to catch: a reused buffer that pushes without clearing would grow, and
    // a frame that grows every render is both a memory leak and a picture that slides.
    let tree = scene();
    let camera = camera(32, 24);
    let mut reused = Frame::new(camera.width, camera.height);
    render_into(&tree, &camera, &mut reused);
    let once = reused.pixels.clone();
    render_into(&tree, &camera, &mut reused);
    assert_eq!(
        reused.pixels.len(),
        once.len(),
        "pixels accumulated across renders"
    );
    assert_eq!(
        reused.pixels, once,
        "a second render into one frame changed the picture"
    );
}
