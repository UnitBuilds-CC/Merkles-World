//! The frame work budget, tested as a *ceiling* and as a *prefix*.
//!
//! Both halves matter and they are different claims. A ceiling that overruns is a stall, and a cut that
//! changes what the drawn pixels look like is a renderer that quietly disagrees with itself frame to frame.
//! So every test here compares against the unbudgeted hierarchy render - the same field, the same march,
//! the same camera - and asserts either that the charge never exceeds the number asked for, or that every
//! pixel the budget did pay for is byte-identical to the full frame.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE, NODE_UNION,
};
use render::raster::{render_bnb, render_bnb_budget, Budget, Camera, Frame, Stats};
use render::{validate, Tree};

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// A hero sphere plus two neighbours far to the side: enough spread that the single scene bound is loose
/// and the bound hierarchy has work to do, which is the situation a ceiling is actually exercised in.
fn scene() -> Tree {
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 5, 0, 0, true),
            record(NODE_UNION, 2, 3, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 0, 4, false),
            record(NODE_SPHERE, 0, 1, 4, 4, false),
            record(NODE_SPHERE, 0, 1, 8, 4, false),
        ],
        params: vec![
            0.8, 0.0, 0.0, 0.0, //
            0.5, 6.0, 1.0, -1.0, //
            0.5, -7.0, -0.5, 2.0,
        ],
    }
}

fn camera() -> Camera {
    Camera {
        origin: [0.0, 0.6, -4.0],
        forward: [0.0, -0.1, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: 64,
        height: 48,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

fn full(tree: &Tree, camera: &Camera) -> (Frame, Stats) {
    render_bnb(tree, camera)
}

#[test]
fn an_unlimited_budget_is_the_hierarchy_render_byte_for_byte() {
    // The baseline for every other claim on this file: with the ceiling off, the budget path must be the
    // unbudgeted one - same bytes, same depths, nothing left undrawn. If this ever fails, the prefix tests
    // below are comparing a frame against a different frame.
    let tree = scene();
    validate(&tree).expect("the test scene must validate");
    let camera = camera();
    let (want, want_stats) = full(&tree, &camera);
    let mut frame = Frame::new(camera.width, camera.height);
    let got_stats = render_bnb_budget(&tree, &camera, Budget(u64::MAX), &mut frame);
    assert_eq!(
        frame.pixels, want.pixels,
        "an unlimited budget changed pixels"
    );
    assert_eq!(frame.depth, want.depth, "an unlimited budget changed depth");
    assert_eq!(got_stats.evaluations, want_stats.evaluations);
    assert_eq!(got_stats.hits, want_stats.hits);
    assert_eq!(
        got_stats.not_drawn, 0,
        "nothing may be left undrawn with no ceiling"
    );
}

#[test]
fn the_ceiling_holds_at_every_level_it_is_asked_to() {
    let tree = scene();
    let camera = camera();
    let (_, want) = full(&tree, &camera);
    let charge = u64::from(camera.max_steps) + 1;
    let mut frame = Frame::new(camera.width, camera.height);
    for budget in [
        0,
        1,
        charge - 1,
        charge,
        charge * 7,
        want.evaluations / 2,
        want.evaluations - 1,
        want.evaluations,
        want.evaluations + 1,
        u64::MAX,
    ] {
        let stats = render_bnb_budget(&tree, &camera, Budget(budget), &mut frame);
        assert!(
            stats.evaluations <= budget,
            "budget {budget} was overran by {}: charged {} evaluations",
            stats.evaluations - budget,
            stats.evaluations
        );
        assert_eq!(
            stats.rays,
            (camera.width * camera.height),
            "the grid size is not a function of the budget"
        );
        assert_eq!(
            stats.rays,
            stats.hits + stats.skipped + (stats.rays - stats.hits - stats.skipped),
            "the ray bookkeeping must close"
        );
        assert!(
            stats.hits + stats.skipped <= stats.rays - stats.not_drawn,
            "hits and skips can only come from pixels that were actually drawn"
        );
    }
}

#[test]
fn a_cut_budget_leaves_the_whole_frame_undrawn_and_costs_nothing() {
    let tree = scene();
    let camera = camera();
    let mut frame = Frame::new(camera.width, camera.height);
    let stats = render_bnb_budget(&tree, &camera, Budget(0), &mut frame);
    assert_eq!(
        stats.evaluations, 0,
        "a zero ceiling must not march one ray"
    );
    assert_eq!(stats.hits, 0);
    let rays = (camera.width * camera.height) as usize;
    assert_eq!(stats.not_drawn as usize, rays);
    assert!(
        frame.depth.iter().all(|&d| d == Frame::NOT_DRAWN_DEPTH),
        "every pixel must report that it was never drawn"
    );
    assert!(
        frame
            .pixels
            .chunks_exact(3)
            .all(|rgb| rgb == Frame::NOT_DRAWN_RGB),
        "the sentinel colour must be the only thing in the buffer"
    );
}

#[test]
fn what_a_budget_does_draw_is_exactly_what_the_full_frame_drew() {
    // The property the whole pass rests on: a ceiling chooses *how much*, never *what*. Every pixel the
    // budget paid for must equal the unbudgeted hierarchy render's pixel, byte for byte, and every pixel it
    // did not pay for must carry the undrawn sentinel rather than a plausible guess.
    let tree = scene();
    let camera = camera();
    let (want, want_stats) = full(&tree, &camera);
    let mut frame = Frame::new(camera.width, camera.height);
    let stats = render_bnb_budget(
        &tree,
        &camera,
        Budget(want_stats.evaluations / 2),
        &mut frame,
    );
    assert!(
        stats.not_drawn > 0,
        "half the work must actually cut something"
    );
    let mut drawn = 0usize;
    for index in 0..want.depth.len() {
        if frame.depth[index] == Frame::NOT_DRAWN_DEPTH {
            assert_eq!(
                &frame.pixels[index * 3..index * 3 + 3],
                &Frame::NOT_DRAWN_RGB,
                "pixel {index} has an undrawn depth but a drawn colour"
            );
            continue;
        }
        drawn += 1;
        assert_eq!(
            frame.depth[index], want.depth[index],
            "pixel {index} was drawn differently under a ceiling"
        );
        assert_eq!(
            &frame.pixels[index * 3..index * 3 + 3],
            &want.pixels[index * 3..index * 3 + 3],
            "pixel {index}'s colour moved under a ceiling"
        );
    }
    assert_eq!(
        drawn as u32 + stats.not_drawn,
        stats.rays,
        "the accounting must close: some pixels drawn, the rest undrawn, no third state"
    );
    assert!(
        stats.evaluations < want_stats.evaluations,
        "a halved ceiling must do strictly less work"
    );
}

#[test]
fn the_cut_falls_on_a_tile_boundary_so_the_drawn_set_is_a_prefix_in_tile_order() {
    // Tile-major order is what makes a partial frame comparable between machines: the cut is a whole number
    // of blocks, so the set of drawn pixels is determined by the budget alone and not by how the float
    // steps happened to land inside a row. Walking the tile order must therefore find drawn pixels, then
    // nothing but sentinels.
    let tree = scene();
    let camera = camera();
    let (_, want_stats) = full(&tree, &camera);
    let mut frame = Frame::new(camera.width, camera.height);
    let stats = render_bnb_budget(
        &tree,
        &camera,
        Budget(want_stats.evaluations * 3 / 4),
        &mut frame,
    );

    let mut seen_undrawn = false;
    for ty in 0..camera.height.div_ceil(8) {
        for tx in 0..camera.width.div_ceil(8) {
            for py in ty * 8..((ty + 1) * 8).min(camera.height) {
                for px in tx * 8..((tx + 1) * 8).min(camera.width) {
                    let at = (py * camera.width + px) as usize;
                    if frame.depth[at] == Frame::NOT_DRAWN_DEPTH {
                        seen_undrawn = true;
                    } else if seen_undrawn {
                        panic!(
                            "pixel ({px}, {py}) was drawn after an undrawn one in tile order - the cut \
                             is not a prefix"
                        );
                    }
                }
            }
        }
    }
    assert!(
        stats.not_drawn > 0,
        "three quarters of the work must cut something"
    );
}

#[test]
fn a_bigger_budget_never_draws_fewer_pixels() {
    let tree = scene();
    let camera = camera();
    let (_, want) = full(&tree, &camera);
    let mut frame = Frame::new(camera.width, camera.height);
    let mut previous = 0u32;
    for share in [0u64, 1, 2, 3, 4, 5, 6] {
        let budget = want.evaluations * share / 4;
        let stats = render_bnb_budget(&tree, &camera, Budget(budget), &mut frame);
        let drawn = stats.rays - stats.not_drawn;
        assert!(
            drawn >= previous,
            "budget {budget} drew {drawn} pixels, fewer than the {previous} a smaller budget drew"
        );
        previous = drawn;
    }
}

#[test]
fn two_runs_of_one_budget_agree_byte_for_byte() {
    // The determinism half, stated where it matters: a budget is a control, and a control whose result
    // varies between runs cannot be tuned. This is also the property CI checks across three architectures
    // when the boot prints a budgeted line.
    let tree = scene();
    let camera = camera();
    let (_, want) = full(&tree, &camera);
    let budget = Budget(want.evaluations * 2 / 5);
    let mut first = Frame::new(camera.width, camera.height);
    let first_stats = render_bnb_budget(&tree, &camera, budget, &mut first);
    let mut second = Frame::new(camera.width, camera.height);
    let second_stats = render_bnb_budget(&tree, &camera, budget, &mut second);
    assert_eq!(first.pixels, second.pixels);
    assert_eq!(first.depth, second.depth);
    assert_eq!(first_stats, second_stats);
}
