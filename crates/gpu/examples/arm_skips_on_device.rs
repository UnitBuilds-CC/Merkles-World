//! The bound-skip arm, **counted on the device** instead of priced on the CPU.
//!
//! `cargo run --release -p gpu --example arm_skips_on_device`
//!
//! docs/04's culling layer landed on the device in `669cad1`: at a plain `NODE_UNION`'s unwind, if the
//! second child's enclosure cannot contain the point, its subtree is never descended. What landed with it
//! was a *CPU* number for the same idea — 42,240 node entries avoided over the demo scene's 3,072 rays —
//! explicitly labelled an upper bound, because the device arm asks only plain unions and leaves the bound
//! unclamped, so it skips a strict subset of what the CPU layer may.
//!
//! This example removes that caveat by making the device report its own tally. `march_rays_counted` calls
//! `march_one` exactly as `march_rays` does and appends one word per ray: the number of union second
//! children that ray's walks refused to evaluate. The claims are integers.
//!
//! 1. **The tally is the arm's, not the walk's.** A single sphere has no second child at all, so its
//!    per-ray counts must be zero; a `NODE_SMOOTH_UNION` must be zero because the device refuses that kind
//!    (OQ-54: `smin` returns less than `min` by up to `k / 4`, so a lower bound on one child bounds
//!    neither). If the counter were measuring descents or steps, both would read non-zero.
//! 2. **Counting does not change the picture.** The counted march's depths must equal the ordinary
//!    `march_uploaded` depths *bit for bit* — the stride-2 layout is the same dispatch, not a second
//!    renderer — and both must match `render::march` on the hit/miss set, with the worst depth deviation
//!    under the anchor every device example in this repository already uses.
//! 3. **The subset holds as numbers.** For the plain union, the device's total must not exceed the CPU
//!    layer's avoided entries over the same rays and the same tree, since that layer is the superset by
//!    construction. A device count above it means the two instruments are not counting the same thing, and
//!    every doc line that compares them would be arithmetic on two different quantities.
//!
//! What this is NOT: not an R1 frame-budget number — a 32x24 fan is a shape of a grid, not 1080p, and the
//! doubled read-back makes this path *more* expensive per frame than the march it instruments — and not a
//! hashed value. Depths and skip counts are presentation-side, so nothing here feeds a root (AGENTS.md §4).

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SMOOTH_UNION, NODE_SPHERE, NODE_UNION,
};
use gpu::device::Interpreter;
use render::cull::{counting_bounds, scene_bounds};
use render::{march, march_bnb, Ray, Tree, ROOT};

/// The fan's grid. Small on purpose: this measures a count, not a frame.
const WIDTH: u32 = 32;
const HEIGHT: u32 = 24;
const MAX_T: f32 = 40.0;
const SURFACE: f32 = 1e-3;
const MAX_STEPS: u32 = 96;
/// The anchor every device example here uses against the reference march.
const DEVIATION: f32 = 0.002;

fn leaf(kind: u16, params_first: u16, params: usize) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, 0),
        1,
        pack_first_and_flags(params_first, 0),
        pack_params_len(params as u16),
    ])
}

/// A root that is itself a leaf: the tree the arm cannot fire on, because there is no second child.
fn root_leaf(kind: u16, params: usize) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, 0),
        1,
        pack_first_and_flags(0, FLAG_IS_ROOT),
        pack_params_len(params as u16),
    ])
}

fn branch(kind: u16, subtree_len: u32, params_first: u16, params: usize) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, 2),
        subtree_len,
        pack_first_and_flags(params_first, FLAG_IS_ROOT),
        pack_params_len(params as u16),
    ])
}

/// Two spheres under a plain union — the node the device arm is licensed at.
fn union_two(separation: f32, radius: f32) -> Tree {
    Tree {
        nodes: vec![
            branch(NODE_UNION, 3, 0, 0),
            leaf(NODE_SPHERE, 0, 4),
            leaf(NODE_SPHERE, 4, 4),
        ],
        params: vec![radius, -separation, 0.0, 0.0, radius, separation, 0.0, 0.0],
    }
}

/// The same pair blended, which the device must refuse and the CPU layer must not.
fn smooth_union_two(k: f32, separation: f32) -> Tree {
    Tree {
        nodes: vec![
            branch(NODE_SMOOTH_UNION, 3, 0, 1),
            leaf(NODE_SPHERE, 1, 4),
            leaf(NODE_SPHERE, 5, 4),
        ],
        params: vec![
            k, //
            1.0,
            -separation,
            0.0,
            0.0, //
            1.0,
            separation,
            0.0,
            0.0,
        ],
    }
}

fn one_sphere() -> Tree {
    Tree {
        nodes: vec![root_leaf(NODE_SPHERE, 4)],
        params: vec![1.0, 0.0, 0.0, 0.0],
    }
}

/// A pinhole fan with no trig: direction is `normalize(x, y, 1)` for `x` in [-2, 2] and `y` in [-1.5,
/// 1.5], so the rays that miss the scene entirely — the ones a bound early-out exists for — are a large
/// share of the grid rather than an edge case.
fn fan_rays() -> Vec<Ray> {
    let mut rays = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    for py in 0..HEIGHT {
        let v = (py as f32 + 0.5) / HEIGHT as f32;
        for px in 0..WIDTH {
            let u = (px as f32 + 0.5) / WIDTH as f32;
            rays.push(Ray {
                origin: [0.0, 0.0, -4.0],
                dir: normalize([2.0 * u - 1.0, 3.0 * v - 1.5, 1.0]),
                max_t: MAX_T,
                surface: SURFACE,
                max_steps: MAX_STEPS,
            });
        }
    }
    rays
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length == 0.0 {
        return [0.0, 0.0, 0.0];
    }
    [v[0] / length, v[1] / length, v[2] / length]
}

/// What the CPU layer refuses to descend, for the same rays and the same tree: the licensed table against
/// a baseline that is offered every bound and skips nothing. `counting_bounds` is the instrument
/// `boot_scene_on_device` prices the demo scene with, so the two numbers are one convention, not two.
fn cpu_side(tree: &Tree, rays: &[Ray]) -> (u64, u64, usize) {
    let licensed = scene_bounds(tree);
    let baseline = counting_bounds(tree);
    let mut hits = 0usize;
    for ray in rays {
        if march_bnb(tree, &licensed, ROOT, *ray).is_some() {
            hits += 1;
        }
        let _ = march_bnb(tree, &baseline, ROOT, *ray);
    }
    let (entered, unpruned) = (licensed.visits(), baseline.visits());
    (unpruned - entered, entered, hits)
}

fn main() {
    let rays = fan_rays();
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(reason) => {
            println!(
                "SKIP arm_skips_on_device: no device ({reason:?}) - the number under test is what a \
                 device's own walk refused, and a software rasteriser would report the reference path as \
                 if it were a frame"
            );
            return;
        }
    };
    println!("adapter          : {}", interpreter.adapter());
    println!(
        "grid             : {WIDTH}x{HEIGHT} = {} rays, max_t {MAX_T}, surface {SURFACE}, steps \
         {MAX_STEPS}",
        rays.len()
    );

    let report = |name: &str, tree: &Tree| {
        let shape = interpreter.upload(tree);
        let counted = interpreter
            .march_counted(&shape, ROOT, &rays)
            .unwrap_or_else(|error| panic!("{name}: counted march failed: {error}"));
        let plain = interpreter
            .march_uploaded(&shape, ROOT, &rays)
            .unwrap_or_else(|error| panic!("{name}: march failed: {error}"));

        assert_eq!(
            counted.march.depths.len(),
            rays.len(),
            "{name}: the counted layout must hand back one depth per ray, whatever the tally does"
        );
        assert_eq!(
            counted.skips.len(),
            rays.len(),
            "{name}: a tally word with no ray to attach it to"
        );

        let layout_flips = counted
            .march
            .depths
            .iter()
            .zip(&plain.depths)
            .filter(|(counted, plain)| counted.to_bits() != plain.to_bits())
            .count();
        assert_eq!(
            layout_flips, 0,
            "{name}: doubling the output stride moved {layout_flips} rays between surface and sky, so the \
             counted path is a second renderer rather than the same march with a tally appended"
        );

        let mut set_flips = 0usize;
        let mut worst = 0.0f32;
        for (index, ray) in rays.iter().enumerate() {
            let reference = march(
                tree,
                ROOT,
                ray.origin,
                ray.dir,
                ray.max_t,
                ray.surface,
                ray.max_steps,
            );
            let depth = counted.march.depths[index];
            let on_device = depth < MAX_T;
            if on_device != reference.is_some() {
                set_flips += 1;
            }
            if let (Some(hit), true) = (reference, on_device) {
                worst = worst.max((depth - hit.t).abs());
            }
        }
        assert_eq!(
            set_flips,
            0,
            "{name}: the device's march disagrees with `render::march` on {set_flips} of {} rays' \
             hit/miss set",
            rays.len()
        );
        assert!(
            worst < DEVIATION,
            "{name}: worst depth deviation {worst:.6} against the {DEVIATION} every device example here \
             holds"
        );

        let total: u64 = counted.skips.iter().map(|count| u64::from(*count)).sum();
        let firing = counted.skips.iter().filter(|count| **count > 0).count();
        let peak = counted.skips.iter().max().copied().unwrap_or(0);
        let (avoided, entered, hits) = cpu_side(tree, &rays);
        println!(
            "// {name:<16} : device refused {total} union second children, on {firing} of {} rays (peak \
             {peak} on one ray); {} surfaces on the device and {hits} on the CPU reference, worst depth \
             deviation {worst:.6}, {set_flips} set disagreements, {layout_flips} words moved by the stride",
            rays.len(),
            counted.march.hits,
        );
        println!(
            "//   cpu layer      : {avoided} node entries avoided of {entered} entered with the table, same \
             rays and same tree (the CPU arm also takes smooth unions and clamps its bound, so it is the \
             superset the device count must sit at or below)"
        );
        (total, firing, avoided)
    };

    // 1. No second child anywhere in the tree: any non-zero tally would mean the counter is the walk's.
    let (sphere_total, sphere_firing, sphere_avoided) = report("sphere", &one_sphere());
    assert_eq!(
        sphere_total, 0,
        "a single sphere fired the union arm {sphere_total} times across {} rays - that tree has no \
         second child, so the tally is not the arm's"
    , rays.len());
    assert_eq!(sphere_firing, 0, "see the total");
    assert_eq!(
        sphere_avoided, 0,
        "the CPU layer claims it avoided {sphere_avoided} entries on a one-leaf tree, which contradicts \
         the device's zero for the same rays"
    );

    // 2. The licensed case: the arm must actually fire, or the layer landed as decoration.
    let (union_total, union_firing, union_avoided) = report("union", &union_two(1.2, 1.0));
    assert!(
        union_total > 0,
        "the arm never fired on a plain union of two separated spheres over {} rays: either the bound \
         table is not reaching the shader, or the stride-2 entry point is not the walk the frame path runs"
    , rays.len());
    assert!(
        union_firing > 0,
        "total {union_total} but not one ray carried a skip"
    );
    assert!(
        union_total <= union_avoided,
        "the device counted {union_total} refusals while the CPU layer avoided {union_avoided} entries on \
         the same rays and the same tree. The CPU arm is a superset by construction - plain unions plus \
         smooth ones, bound clamped instead of left unclamped - so a device count above it means the two \
         instruments count different things and docs/04's comparison of them is meaningless"
    );

    // 3. The refused kind, measured from both sides.
    let (smooth_total, _, smooth_avoided) = report("smooth union", &smooth_union_two(0.5, 1.2));
    assert_eq!(
        smooth_total, 0,
        "the device arm fired {smooth_total} times on a SMOOTH union, which is the case OQ-54 exists to \
         refuse: `smin` returns less than `min` by up to k/4, so a lower bound on the second child is not \
         a lower bound on the pair"
    );
    assert!(
        smooth_avoided > 0,
        "the CPU layer avoided nothing on the blend either, so this run does not distinguish the two \
         arms and claim 3 is measuring nothing"
    );
    println!(
        "// subset claim     : on the blend the CPU avoided {smooth_avoided} entries where the device \
         counted {smooth_total} - the k-aware arm stays on the CPU, and the device's stricter refusal is \
         now a measured pair of numbers rather than a comment."
    );

    // 4. Directional, not claimed: the same law on a scene whose second child is small and far off-axis,
    //    where more of the fan should be provably outside its enclosure. Printed so a future reader can
    //    see the tally tracks the geometry, and deliberately not asserted - no bound on the ratio between
    //    two scenes follows from the arm's soundness argument.
    let far_tree = union_two(6.0, 0.25);
    let (far_total, _, _) = report("union far", &far_tree);

    // 5. The arm **priced on the device**, against a control that cannot fire. `upload_bounds_refused` keeps
    //    the pool layout (four words per node), the node records and the walk byte-identical and makes every
    //    radius `+inf`, so `length(p - centre) - inf >= left` never answers yes: the only thing that differs
    //    between the two shapes is what the bound test returns. What is asserted is that the control really
    //    refuses (its own tally reads zero where the armed shape's does not) and that neither march computes a
    //    different picture (bit-identical depths on every repeat). The milliseconds are printed and are not
    //    evidence (ADR-017) — they are what lets docs/04 say what the arm saved instead of what it could have.
    const REPEATS: usize = 30;
    let price = |name: &str, tree: &Tree, armed_total: u64| {
        let armed = interpreter.upload(tree);
        let control = interpreter.upload_bounds_refused(tree);
        let control_counted = interpreter
            .march_counted(&control, ROOT, &rays)
            .unwrap_or_else(|error| panic!("{name}: control counted march failed: {error}"));
        let control_total: u64 = control_counted
            .skips
            .iter()
            .map(|count| u64::from(*count))
            .sum();
        assert_eq!(
            control_total, 0,
            "{name}: the +inf control still refused {control_total} second children while the armed shape \
             refused {armed_total} - the control is not a control, and the two timings would be one walk \
             measured twice"
        );

        let mut armed_ms = Vec::with_capacity(REPEATS);
        let mut control_ms = Vec::with_capacity(REPEATS);
        for repeat in 0..REPEATS {
            let a = interpreter
                .march_uploaded(&armed, ROOT, &rays)
                .unwrap_or_else(|error| panic!("{name}: armed march {repeat} failed: {error}"));
            let c = interpreter
                .march_uploaded(&control, ROOT, &rays)
                .unwrap_or_else(|error| panic!("{name}: control march {repeat} failed: {error}"));
            let flips = a
                .depths
                .iter()
                .zip(&c.depths)
                .filter(|(a, c)| a.to_bits() != c.to_bits())
                .count();
            assert_eq!(
                flips,
                0,
                "{name}: repeat {repeat} of the armless control moved {flips} rays' depths, so the bound \
                 test is changing the picture rather than the work - which is exactly what an enclosure too \
                 small would do, and why this pair is asserted before it is timed"
            );
            armed_ms.push(a.ms_frame_with_read);
            control_ms.push(c.ms_frame_with_read);
        }
        let median = |values: &mut Vec<f64>| {
            values.sort_by(|a, b| a.total_cmp(b));
            values[values.len() / 2]
        };
        let (a, c) = (median(&mut armed_ms), median(&mut control_ms));
        println!(
            "// {name:<16} priced : median of {REPEATS} marches of {} rays - armed {a:.3} ms, control \
             {c:.3} ms, the arm's share of the caller's wait {:.1}%; depths bit-identical on every repeat, \
             and the control tallied 0 refusals against the armed shape's {armed_total}",
            rays.len(),
            if c > 0.0 { (c - a) * 100.0 / c } else { 0.0 },
        );
        (a, c)
    };
    let (union_armed, union_control) = price("union", &union_two(1.2, 1.0), union_total);
    // The blend is the control's own falsifier: the arm never fires on it, so its two timings are one walk
    // measured twice and any difference is the instrument's noise floor, not the arm's saving.
    let (smooth_armed, smooth_control) =
        price("smooth union", &smooth_union_two(0.5, 1.2), smooth_total);
    let (far_armed, far_control) = price("union far", &far_tree, far_total);

    println!(
        "// noise floor      : on the blend, where the armed tally is {smooth_total}, the two medians differ \
         by {:.3} ms of {:.3} - that gap is the instrument's own spread, so an arm saving below it is not a \
         saving anyone could claim from these numbers.",
        (smooth_armed - smooth_control).abs(),
        smooth_control,
    );
    println!(
        "result           : the device's bound-skip arm counted its own refusals - {union_total} over {} \
         rays on the plain union, 0 on the leaf, 0 on the blend - each on depths identical to the march \
         that tallies nothing; and against an unlicensed-bound control it is {union_armed:.3} ms against \
         {union_control:.3} on the union, {far_armed:.3} against {far_control:.3} on the far union \
         (informational, ADR-017)."
    , rays.len());
}
