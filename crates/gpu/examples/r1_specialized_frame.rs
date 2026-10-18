//! R1's whole frame, timed in one leg, on an interpreter compiled with the stack capacity its scene actually
//! needs. This is the row the DEPTH ladder left as arithmetic.
//!
//! `cargo run --release -p gpu --example r1_specialized_frame`
//!
//! `r1_walk_cost_ladder` measured the march leg alone at two capacities and closed with the honest caveat that the
//! ~11.2 ms inside-budget frame it implied was **DERIVED from two legs timed minutes apart, not a measured frame**.
//! This file times one: `Interpreter::march_camera_variant` dispatches and copies and maps the depths back inside
//! a single `Instant`, so the number it returns is the interval a caller actually waits, which is exactly what gate
//! R1 spends.
//!
//! The capacity is not a constant typed into this file. `render::tree_depth` counts the frames the walk pushes for
//! the tree being marched — one per nested node from the root the walk starts at — and that count is what the
//! module is compiled with. Getting the derivation wrong in either direction is the failure this example exists to
//! make impossible, so both directions are asserted on the anchor grid: a capacity **below** the tree's depth must
//! find 0 surfaces (a scene that renders empty, not slow — which is the reason `const DEPTH = 2` is not a shippable
//! global), and a capacity **equal** to it must reproduce the shipped pipeline's depths with 0 differing bits. The
//! two-node demo tree and a three-node-nested one cover both cases, so the rule `capacity = tree_depth` is tested
//! rather than assumed.
//!
//! The timing is a paired, order-alternating comparison against the shipped capacity on the same frame, because
//! that is the statistic that survived its own re-pricing before: the ladder's first fixed-order run put the
//! shipped row 2× faster than its own recompiled twin, and the arms here differ only in which module the same
//! dispatch list uses. A 1070's clock ramp is then inside both arms rather than inside whichever ran first.
//!
//! Durations assert nothing (ADR-017) and the budget ratio is printed, not claimed: R1 names 60 Hz at 1080p on
//! buildbox's GTX 1070, so a row here says `x.xx ms = y.yy x budget` on its own instrument and nothing more.
//! `R1_GRID=WxH` moves the timed legs to another grid, labelling its rows as not R1's, exactly as
//! `r1_march_alone` does; unset reproduces the gate's own resolution.
//!
//! The file ends on the scene the demo actually shows, not on a fixture: `boot_scene_on_device`'s RON, compiled by
//! `assetc`'s own `Watch`, read back through the loader, and marched at the depth the *bytes* declare. That depth
//! and `render::tree_depth`'s count of the same records are asserted to be the same number from two independent
//! implementations - the compiler's walk over the grammar and the renderer's walk over the tree - and the picture
//! at that capacity is anchored on `render::march` with the boot's own camera, 96-step law and 1e-3 surface. The
//! three-node fixture above is the comparison the ladder's rows were written against; it is not R1's content, and a
//! saving measured on a depth-2 scene does not entitle anyone to quote it against a depth-4 one.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{GpuCamera, Interpreter, March};
use render::{march, tree_depth, Ray, Tree, Vec3, ROOT};

/// `r1_march_alone`'s and `r1_walk_cost_ladder`'s scene: `union(sphere, rounded_box)`, two frames deep.
fn demo_scene() -> Tree {
    let record = |kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool| {
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
            pack_params_len(len),
        ])
    };
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 3, 0, 0, true),
            record(NODE_SPHERE, 0, 1, 0, 4, false),
            record(NODE_ROUNDED_BOX, 0, 1, 4, 4, false),
        ],
        params: vec![0.5, 0.0, 0.0, 0.0, 2.0, 0.1, 2.0, 0.0, 0.0, -1.0, 0.0],
    }
}

/// The same field, one nesting level deeper: `union(union(sphere, sphere), rounded_box)`, three frames deep.
/// Built to give `tree_depth` something to be wrong about, and to be the scene a capacity of 2 must refuse.
fn nested_scene() -> Tree {
    let record = |kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool| {
        NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
            pack_params_len(len),
        ])
    };
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 5, 0, 0, true),
            record(NODE_UNION, 2, 3, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 0, 4, false),
            record(NODE_SPHERE, 0, 1, 4, 4, false),
            record(NODE_ROUNDED_BOX, 0, 1, 8, 4, false),
        ],
        params: vec![
            0.5, 0.0, 0.0, 0.0, -1.5, 0.0, 0.0, 0.0, 2.0, 0.1, 2.0, 0.0, 0.0, -1.0, 0.0, 0.0,
        ],
    }
}

const SURFACE: f32 = 0.01;
const MAX_T: f32 = 24.0;
const HALF_HEIGHT: f32 = 0.5;
const MAX_STEPS: u32 = 64;
const ANCHOR_W: u32 = 64;
const ANCHOR_H: u32 = 48;
const R1_W: u32 = 1920;
const R1_H: u32 = 1080;
/// R1's frame budget at 60 Hz, the only duration this file is allowed to compare itself with.
const BUDGET_MS: f64 = 1000.0 / 60.0;
const MISS: f32 = 1.0e37;
/// Paired rounds per timed row; the median of seven, with the arms alternating which one starts.
const ROUNDS: usize = 7;

fn grid() -> (u32, u32) {
    std::env::var("R1_GRID")
        .ok()
        .and_then(|text| {
            let (w, h) = text.trim().split_once(['x', 'X'])?;
            let w = w.trim().parse::<u32>().ok()?;
            let h = h.trim().parse::<u32>().ok()?;
            Some((w, h))
        })
        .filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((R1_W, R1_H))
}

fn camera(width: u32, height: u32, origin: Vec3, max_steps: u32) -> GpuCamera {
    GpuCamera {
        origin,
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        forward: [0.0, 0.0, 1.0],
        half_height: HALF_HEIGHT,
        max_t: MAX_T,
        surface: SURFACE,
        max_steps,
        root: ROOT,
        width,
        height,
    }
}

fn ray_at(px: u32, py: u32, width: u32, height: u32, origin: Vec3, max_steps: u32) -> Ray {
    let aspect = width as f32 / height as f32;
    let u = (px as f32 + 0.5) / width as f32;
    let v = (py as f32 + 0.5) / height as f32;
    let ndc_x = (2.0 * u - 1.0) * HALF_HEIGHT * aspect;
    let ndc_y = (1.0 - 2.0 * v) * HALF_HEIGHT;
    let span = [ndc_x, ndc_y, 1.0];
    let length = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
    Ray {
        origin,
        dir: if length == 0.0 {
            [0.0, 0.0, 0.0]
        } else {
            [span[0] / length, span[1] / length, span[2] / length]
        },
        max_t: MAX_T,
        surface: SURFACE,
        max_steps,
    }
}

fn bit_differences(a: &[f32], b: &[f32]) -> usize {
    a.iter()
        .zip(b.iter())
        .filter(|(left, right)| left.to_bits() != right.to_bits())
        .count()
}

fn median(values: &[f64]) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    values[values.len() / 2]
}

fn spread(values: &[f64]) -> f64 {
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    max - min
}

/// One whole frame, timed as one interval, at one capacity.
struct FrameRow {
    depth: u32,
    shipped: bool,
    cap: u32,
    ms: f64,
    spread_ms: f64,
    hits: usize,
}

impl FrameRow {
    fn budget(&self) -> f64 {
        self.ms / BUDGET_MS
    }
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP r1_specialized_frame: no device ({error:?}) - a whole frame is dispatch plus copy plus map \
                 on a GPU; a software rasteriser has no such interval to time"
            );
            return;
        }
    };
    let demo = demo_scene();
    let nested = nested_scene();
    let origin = [0.0f32, 0.0, -6.0];

    let demo_depth = tree_depth(&demo);
    let nested_depth = tree_depth(&nested);

    println!("// adapter    : {}", interpreter.adapter());
    println!("// budget     : gate R1 names {BUDGET_MS:.2} ms for one 1080p frame at 60 Hz on this device");
    println!("// derived    : tree_depth(demo) = {demo_depth}, tree_depth(nested) = {nested_depth} - computed from the records, never typed in");
    println!("// asserted   : the capacity a scene needs is exactly its depth (below it renders empty, at it reproduces the shipped depths bit for bit), and both arms of the timed legs agree on the picture");
    println!("// printed    : one-interval frame ms and the budget ratio - durations assert nothing (ADR-017)");
    println!();

    // ---- The derivation rule, both directions, on the anchor grid ---------------------------------
    //
    // `capacity == tree_depth` must be the exact rule, not a conservative margin, because the whole saving is the
    // absence of margin: a capacity of 32 for a two-frame tree is what the ladder priced at 15x the walk. So a
    // scene of depth D is marched by D and by D - 1, and the second one must be an empty picture.
    assert_eq!(
        demo_depth, 2,
        "the demo scene is union(sphere, rounded_box), which pushes two frames, and tree_depth says {demo_depth}; \
         the capacity the shipped pipeline is compiled with is derived from this number"
    );
    assert_eq!(
        nested_depth, 3,
        "union(union(sphere, sphere), rounded_box) pushes three frames, and tree_depth says {nested_depth} - a \
         count that is wrong here makes the refusal row below meaningless"
    );

    let mut refuse_checks = Vec::new();
    for (name, tree, depth) in [
        ("demo", &demo, demo_depth),
        ("nested", &nested, nested_depth),
    ] {
        let shape = interpreter.upload(tree);
        let mut frame = interpreter.begin_camera_frame(ANCHOR_W, ANCHOR_H);
        interpreter.write_camera(&mut frame, &camera(ANCHOR_W, ANCHOR_H, origin, MAX_STEPS));
        let shipped = interpreter
            .march_camera_frame(&interpreter.bind_camera(&shape, &frame), &frame)
            .expect("the shipped pipeline must march the anchor");

        // Reference picture, so "reproduces the shipped pipeline" is anchored on `render::march` and not on itself.
        let mut reference = Vec::with_capacity(shipped.depths.len());
        for py in 0..ANCHOR_H {
            for px in 0..ANCHOR_W {
                let ray = ray_at(px, py, ANCHOR_W, ANCHOR_H, origin, MAX_STEPS);
                reference.push(
                    match march(
                        tree,
                        ROOT,
                        ray.origin,
                        ray.dir,
                        ray.max_t,
                        ray.surface,
                        ray.max_steps,
                    ) {
                        Some(hit) => hit.t,
                        None => MISS,
                    },
                );
            }
        }
        let set_diffs = reference
            .iter()
            .zip(&shipped.depths)
            .filter(|(left, right)| (**left < MISS) != (**right < MISS))
            .count();

        let exact = interpreter
            .camera_variant(depth)
            .expect("a variant at the tree's own depth is a legal module");
        let exact_march = interpreter
            .march_camera_variant(
                &exact,
                &interpreter.bind_camera_variant(&exact, &shape, &frame),
                &frame,
            )
            .expect("the exact-capacity pipeline must march the anchor");
        let exact_diffs = bit_differences(&shipped.depths, &exact_march.depths);

        let short = interpreter
            .camera_variant(depth - 1)
            .expect("a variant one frame short is still a legal module");
        let short_march = interpreter
            .march_camera_variant(
                &short,
                &interpreter.bind_camera_variant(&short, &shape, &frame),
                &frame,
            )
            .expect("the short-capacity pipeline must march the anchor");

        refuse_checks.push(RefusalRow {
            name,
            depth,
            shipped_hits: shipped.hits,
            set_diffs,
            exact_hits: exact_march.hits,
            exact_diffs,
            short_hits: short_march.hits,
            pixels: shipped.depths.len(),
        });

        println!(
            "//   {name} scene   : depth {depth}, shipped finds {} of {} surfaces",
            shipped.hits,
            shipped.depths.len()
        );
        println!("//     capacity {depth} (exact)  : {} bit-differing depths vs shipped, {} surfaces, {} set diffs from render::march", exact_diffs, exact_march.hits, set_diffs);
        println!("//     capacity {} (one short): {} surfaces - the guard's sentinel, so a scene that does not fit renders empty", depth - 1, short_march.hits);
    }
    println!();

    for row in &refuse_checks {
        assert_eq!(
            row.set_diffs, 0,
            "the shipped pipeline disagrees with `render::march` on {} of {} {}-scene pixels, so this file's \
             reference picture is not the march the repo has tests for",
            row.set_diffs, row.pixels, row.name
        );
        assert_eq!(
            row.exact_diffs, 0,
            "a capacity of exactly the {} scene's depth ({}) changed {} depth bit patterns out of {}, so the \
             rule `capacity = tree_depth` is not the rule the walk follows and the per-asset saving is not \
             available",
            row.name, row.depth, row.exact_diffs, row.pixels
        );
        assert_eq!(
            row.exact_hits, row.shipped_hits,
            "the exact-capacity pipeline found {} surfaces where the shipped one found {} on the {} scene",
            row.exact_hits, row.shipped_hits, row.name
        );
        assert_eq!(
            row.short_hits, 0,
            "a capacity one frame short of the {} scene's depth still found {} surfaces, so the walk's guard did \
             not fire and `DEPTH` is decoration in the module the substitution built",
            row.name, row.short_hits
        );
        assert!(
            row.shipped_hits > 0,
            "the {} scene found no surfaces on the anchor grid, so there is nothing for a refusal to differ from",
            row.name
        );
    }

    // ---- One whole 1080p frame, both arms, paired and order-alternating -----------------------------
    let (w, h) = grid();
    let label = if (w, h) == (R1_W, R1_H) {
        "1080p".to_string()
    } else {
        format!("{w}x{h}")
    };
    let pixels = w as usize * h as usize;
    println!(
        "// grid         : {label:<11} = {pixels} pixels per frame{}",
        if label == "1080p" {
            ", R1's own resolution".to_string()
        } else {
            ", NOT R1's grid, so these rows are a frontier reading".to_string()
        }
    );

    let shape = interpreter.upload(&demo);
    let variant = interpreter
        .camera_variant(demo_depth)
        .expect("the demo scene's own capacity must compile");
    let caps = [1u32, 8, MAX_STEPS];

    // A loss at this grid is a panic from inside `Device::poll`, not an `Err`, so the legs run under
    // `catch_unwind`; only a payload naming a lost device prints a skip, and anything else exits 101.
    let outcome = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| {
        let mut rows: Vec<FrameRow> = Vec::new();
        let mut picture = Vec::new();
        for &cap in &caps {
            let mut big = interpreter.begin_camera_frame(w, h);
            interpreter.write_camera(&mut big, &camera(w, h, origin, cap));
            let shipped_bind = interpreter.bind_camera(&shape, &big);
            let variant_bind = interpreter.bind_camera_variant(&variant, &shape, &big);

            let mut shipped_ms = Vec::with_capacity(ROUNDS);
            let mut variant_ms = Vec::with_capacity(ROUNDS);
            let mut shipped_last: Option<March> = None;
            let mut variant_last: Option<March> = None;
            for round in 0..ROUNDS {
                if round % 2 == 0 {
                    shipped_ms.push(
                        interpreter
                            .march_camera_frame(&shipped_bind, &big)
                            .expect("the shipped arm must frame")
                            .ms_frame_with_read,
                    );
                    let march = interpreter
                        .march_camera_variant(&variant, &variant_bind, &big)
                        .expect("the specialized arm must frame");
                    variant_ms.push(march.ms_frame_with_read);
                    variant_last = Some(march);
                } else {
                    let march = interpreter
                        .march_camera_variant(&variant, &variant_bind, &big)
                        .expect("the specialized arm must frame");
                    variant_ms.push(march.ms_frame_with_read);
                    variant_last = Some(march);
                    shipped_ms.push(
                        interpreter
                            .march_camera_frame(&shipped_bind, &big)
                            .expect("the shipped arm must frame")
                            .ms_frame_with_read,
                    );
                }
                // Keep the last depths of each arm for the picture assert without holding 8 MB per round.
                let march = interpreter
                    .march_camera_frame(&shipped_bind, &big)
                    .expect("the shipped arm must re-march for its depths");
                shipped_last = Some(march);
            }
            let shipped_march = shipped_last.expect("the shipped arm marched");
            let variant_march = variant_last.expect("the specialized arm marched");
            let diffs = bit_differences(&shipped_march.depths, &variant_march.depths);
            picture.push((cap, shipped_march.hits, variant_march.hits, diffs, pixels));

            rows.push(FrameRow {
                depth: 32,
                shipped: true,
                cap,
                ms: median(&shipped_ms),
                spread_ms: spread(&shipped_ms),
                hits: shipped_march.hits,
            });
            rows.push(FrameRow {
                depth: demo_depth,
                shipped: false,
                cap,
                ms: median(&variant_ms),
                spread_ms: spread(&variant_ms),
                hits: variant_march.hits,
            });
        }
        (rows, picture)
    }));

    let (rows, picture) = match outcome {
        Ok(result) => result,
        Err(payload) => {
            let text = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|held| held.to_string()))
                .unwrap_or_else(|| "a panic with no message".to_string());
            if !(text.contains("device is lost") || text.contains("Validation")) {
                println!("// {label:<11}: UNEXPECTED PANIC, not a lost device, so it is a failure: {text}");
                std::process::exit(101);
            }
            println!("// {label:<11}: SKIPPED, the device was lost mid-leg: {text}");
            println!("//                (the derivation rows above are the asserted ones; no duration here is R1 evidence)");
            return;
        }
    };

    println!(
        "//   capacity |   cap |  hits | bit diffs | ms one interval | spread | x {BUDGET_MS:.2} ms budget"
    );
    for row in &rows {
        println!(
            "//   {:>8}{} | {:>5} | {:>5} | {:>9} | {:>15.3} | {:>6.3} | {:>5.2} x",
            row.depth,
            if row.shipped { "*" } else { " " },
            row.cap,
            row.hits,
            picture
                .iter()
                .find(|(cap, _, _, _, _)| *cap == row.cap)
                .map(|(_, _, _, diffs, _)| *diffs)
                .unwrap_or(usize::MAX),
            row.ms,
            row.spread_ms,
            row.budget(),
        );
    }
    println!("//   (* = the shipped capacity 32, timed in the same alternating sequence)");
    println!();

    for (cap, shipped_hits, variant_hits, diffs, count) in &picture {
        assert_eq!(
            *diffs, 0,
            "at cap {cap} the specialized pipeline's depths differ from the shipped frame's on {} of {} pixels, \
             so the timed rows below are two different pictures rather than one picture at two capacities",
            diffs,
            count
        );
        assert_eq!(
            shipped_hits, variant_hits,
            "at cap {cap} the arms found {} and {} surfaces",
            variant_hits, shipped_hits
        );
    }
    let frame = |depth: u32, cap: u32| -> f64 {
        rows.iter()
            .find(|row| row.depth == depth && row.cap == cap && row.shipped == (depth == 32))
            .map(|row| row.ms)
            .unwrap_or(f64::INFINITY)
    };
    for &cap in &caps {
        let shipped = frame(32, cap);
        let specialized = frame(demo_depth, cap);
        println!(
            "// cap {cap:>3}      : shipped {shipped:.3} ms vs capacity {demo_depth} {specialized:.3} ms - {:.1}% of \
             the frame, {specialized:.2} ms = {:.2} x budget",
            100.0 * (1.0 - specialized / shipped),
            specialized / BUDGET_MS
        );
        assert!(
            specialized <= shipped,
            "at cap {cap} the specialized frame took {specialized:.3} ms against the shipped {shipped:.3} ms, so \
             the capacity the ladder priced is not what a whole interval sees"
        );
    }
    assert_eq!(
        picture
            .iter()
            .find(|(cap, _, _, _, _)| *cap == MAX_STEPS)
            .map(|(_, hits, _, _, _)| *hits),
        Some(frame_at(&rows, MAX_STEPS, true)),
        "the cap-64 shipped frame's hits do not match its own row"
    );
    println!(
        "// reading      : the 64-step row is the one R1's 5.5x was measured against; both columns above are one \
         dispatch-plus-copy-plus-map interval, so the read-back is inside the number rather than added to it"
    );

    // ---- The product's own content --------------------------------------------------------------
    //
    // Everything above marches a three-node fixture, because that is the scene `r1_march_alone` and the ladder
    // used, and comparable rows are worth a lot. It is not the scene the demo shows. The demo's content is a RON
    // file that `assetc` compiles into bytes and the boot loads, and its declared depth is 4 - so the capacity
    // this lever actually ships with is the one *that* scene derives, and the frame is the one at the boot's own
    // camera and step cap. A saving measured on a depth-2 fixture does not entitle anyone to quote it against
    // the boot's content, so the content is compiled here by the shipped compiler object, marched at the depth
    // the asset declares, and timed as the same one interval.
    let (product, declared) = product_scene();
    let derived = tree_depth(&product);
    println!();
    println!(
        "// product scene  : the boot's compiled asset - {} node records, {} parameters, {} bytes of pool",
        product.nodes.len(),
        product.params.len(),
        product.params.len() * 4
    );
    println!(
        "//   declared depth {declared} (from the asset bytes) | counted by render::tree_depth {derived}"
    );
    assert_eq!(
        declared, derived,
        "the compiler writes a declared depth of {declared} for the boot's scene while the walk's own frame count \
         is {derived}, so either the envelope the boot reads is wrong or the capacity this file derives from the \
         records is"
    );

    let shape = interpreter.upload(&product);
    let mut frame = interpreter.begin_camera_frame(ANCHOR_W, ANCHOR_H);
    interpreter.write_camera(
        &mut frame,
        &product_camera(ANCHOR_W, ANCHOR_H, PRODUCT_STEPS),
    );
    let shipped_anchor = interpreter
        .march_camera_frame(&interpreter.bind_camera(&shape, &frame), &frame)
        .expect("the shipped pipeline must march the product anchor");
    let variant = interpreter
        .camera_variant(derived)
        .expect("the product's declared depth is a legal module capacity");
    let exact_anchor = interpreter
        .march_camera_variant(
            &variant,
            &interpreter.bind_camera_variant(&variant, &shape, &frame),
            &frame,
        )
        .expect("the product's own capacity must march the anchor");
    let exact_diffs = bit_differences(&shipped_anchor.depths, &exact_anchor.depths);

    let mut set_diffs = 0usize;
    let mut worst = 0.0f32;
    let mut reference_hits = 0usize;
    let mut index = 0usize;
    for py in 0..ANCHOR_H {
        for px in 0..ANCHOR_W {
            let ray = product_ray(px, py, ANCHOR_W, ANCHOR_H, PRODUCT_STEPS);
            let hit = march(
                &product,
                ROOT,
                ray.origin,
                ray.dir,
                ray.max_t,
                ray.surface,
                ray.max_steps,
            );
            if hit.is_some() {
                reference_hits += 1;
            }
            let device = exact_anchor.depths[index];
            if (hit.is_some()) != (device < MISS) {
                set_diffs += 1;
            } else if let Some(hit) = hit {
                worst = worst.max((hit.t - device).abs());
            }
            index += 1;
        }
    }
    println!(
        "// anchor grid    : {ANCHOR_W}x{ANCHOR_H} - reference {reference_hits} surfaces, shipped {}, capacity \
         {derived} {}, {exact_diffs} bit-differing depths, {set_diffs} set disagreements, worst deviation {worst:.6}",
        shipped_anchor.hits,
        exact_anchor.hits
    );
    assert_eq!(
        exact_diffs, 0,
        "{exact_diffs} depths differ in their bits between the shipped capacity and the product's declared one, so \
         the timed rows below are not the same picture"
    );
    assert_eq!(
        set_diffs, 0,
        "{set_diffs} pixels disagree on hit-vs-miss between the product's own capacity and `render::march`, so the \
         demo content is marched by a walk this repo has no test for"
    );
    assert!(
        worst < DEVIATION,
        "worst |device - reference| depth on the product scene is {worst}, above the {DEVIATION} every other \
         device example anchors on"
    );
    assert_eq!(
        exact_anchor.hits, shipped_anchor.hits,
        "the product at its declared capacity found {} surfaces and at capacity 32 found {}",
        exact_anchor.hits, shipped_anchor.hits
    );

    let (w, h) = grid();
    let product_caps = [8u32, PRODUCT_STEPS];
    let outcome = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| {
        let mut rows: Vec<FrameRow> = Vec::new();
        let mut picture: Vec<(u32, usize, usize, usize, usize)> = Vec::new();
        for &cap in &product_caps {
            let mut big = interpreter.begin_camera_frame(w, h);
            interpreter.write_camera(&mut big, &product_camera(w, h, cap));
            let shipped_bind = interpreter.bind_camera(&shape, &big);
            let variant_bind = interpreter.bind_camera_variant(&variant, &shape, &big);
            let mut shipped_ms = Vec::new();
            let mut variant_ms = Vec::new();
            for round in 0..ROUNDS {
                // Alternate which arm starts, so a ramping clock is inside both columns instead of one.
                if round % 2 == 0 {
                    shipped_ms.push(
                        interpreter
                            .march_camera_frame(&shipped_bind, &big)
                            .expect("shipped arm")
                            .ms_frame_with_read,
                    );
                    variant_ms.push(
                        interpreter
                            .march_camera_variant(&variant, &variant_bind, &big)
                            .expect("specialized arm")
                            .ms_frame_with_read,
                    );
                } else {
                    variant_ms.push(
                        interpreter
                            .march_camera_variant(&variant, &variant_bind, &big)
                            .expect("specialized arm")
                            .ms_frame_with_read,
                    );
                    shipped_ms.push(
                        interpreter
                            .march_camera_frame(&shipped_bind, &big)
                            .expect("shipped arm")
                            .ms_frame_with_read,
                    );
                }
            }
            let shipped_march = interpreter
                .march_camera_frame(&shipped_bind, &big)
                .expect("the shipped arm must re-march for its depths");
            let variant_march = interpreter
                .march_camera_variant(&variant, &variant_bind, &big)
                .expect("the specialized arm must re-march for its depths");
            let diffs = bit_differences(&shipped_march.depths, &variant_march.depths);
            picture.push((
                cap,
                shipped_march.hits,
                variant_march.hits,
                diffs,
                shipped_march.depths.len(),
            ));
            rows.push(FrameRow {
                depth: 32,
                shipped: true,
                cap,
                ms: median(&shipped_ms),
                spread_ms: spread(&shipped_ms),
                hits: shipped_march.hits,
            });
            rows.push(FrameRow {
                depth: derived,
                shipped: false,
                cap,
                ms: median(&variant_ms),
                spread_ms: spread(&variant_ms),
                hits: variant_march.hits,
            });
        }
        (rows, picture)
    }));

    let (product_rows, product_picture) = match outcome {
        Ok(result) => result,
        Err(payload) => {
            let text = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|held| held.to_string()))
                .unwrap_or_else(|| "a panic with no message".to_string());
            if !(text.contains("device is lost") || text.contains("Validation")) {
                println!("// product legs   : UNEXPECTED PANIC, not a lost device, so it is a failure: {text}");
                std::process::exit(101);
            }
            println!("// product legs   : SKIPPED, the device was lost mid-leg: {text}");
            return;
        }
    };

    println!(
        "// product grid   : {w}x{h} = {} pixels, the boot's camera, caps {:?}, medians of {ROUNDS} paired rounds",
        w as usize * h as usize,
        product_caps
    );
    println!(
        "//   capacity |   cap |  hits | bit diffs | ms one interval | spread | x 16.67 ms budget"
    );
    for row in &product_rows {
        println!(
            "//   {:>8}{} | {:>5} | {:>5} | {:>9} | {:>15.3} | {:>6.3} | {:>5.2} x",
            row.depth,
            if row.shipped { "*" } else { " " },
            row.cap,
            row.hits,
            product_picture
                .iter()
                .find(|(cap, _, _, _, _)| *cap == row.cap)
                .map(|(_, _, _, diffs, _)| *diffs)
                .unwrap_or(usize::MAX),
            row.ms,
            row.spread_ms,
            row.ms / BUDGET_MS,
        );
    }
    for (cap, shipped_hits, variant_hits, diffs, count) in &product_picture {
        assert_eq!(
            *diffs, 0,
            "at cap {cap} the product's declared capacity disagrees with the shipped module on {} of {} pixels",
            diffs, count
        );
        assert_eq!(
            shipped_hits, variant_hits,
            "at cap {cap} the arms found {} and {} surfaces on the demo content",
            variant_hits, shipped_hits
        );
    }
    for &cap in &product_caps {
        let shipped = product_rows
            .iter()
            .find(|row| row.cap == cap && row.shipped)
            .map(|row| row.ms)
            .unwrap_or(f64::INFINITY);
        let specialized = product_rows
            .iter()
            .find(|row| row.cap == cap && !row.shipped)
            .map(|row| row.ms)
            .unwrap_or(f64::INFINITY);
        assert!(
            specialized <= shipped,
            "at cap {cap} the demo content took {specialized:.3} ms at its declared capacity against {shipped:.3} \
             ms at 32, so the capacity ladder priced is not what the whole interval sees"
        );
        println!(
            "// product cap {cap:>3} : shipped {shipped:.3} ms vs depth {derived} {specialized:.3} ms - {:.1}% of \
             the frame, {specialized:.2} ms = {:.2} x budget",
            100.0 * (1.0 - specialized / shipped),
            specialized / BUDGET_MS
        );
    }
    println!(
        "// reading      : this is R1's row on the demo's own content and the boot's own law, so it is the number \
         the gate is about; the fixture rows above are the comparison the ladder's rows were written against"
    );
}

/// The boot's camera, transcribed from `boot_scene_on_device`'s `probe_rays`: aimed down a forward tilted
/// -0.1 in y from `[0.0, 0.6, -4.0]`, `max_t` 40, surface 1e-3, and 96 steps - a different law from the
/// fixture's on purpose, because the fixture's numbers do not entitle anyone to quote the demo content.
const PRODUCT_HALF_HEIGHT: f32 = 0.5;
const PRODUCT_MAX_T: f32 = 40.0;
const PRODUCT_SURFACE: f32 = 1.0e-3;
const PRODUCT_STEPS: u32 = 96;
/// The deviation every other device example in this repo anchors on.
const DEVIATION: f32 = 0.002;

fn product_origin() -> Vec3 {
    [0.0f32, 0.6, -4.0]
}

fn product_forward() -> [f32; 3] {
    let v = [0.0f32, -0.1, 1.0];
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}

fn product_camera(width: u32, height: u32, max_steps: u32) -> GpuCamera {
    GpuCamera {
        origin: product_origin(),
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        forward: product_forward(),
        half_height: PRODUCT_HALF_HEIGHT,
        max_t: PRODUCT_MAX_T,
        surface: PRODUCT_SURFACE,
        max_steps,
        root: ROOT,
        width,
        height,
    }
}

fn product_ray(px: u32, py: u32, width: u32, height: u32, max_steps: u32) -> Ray {
    let aspect = width as f32 / height as f32;
    let u = (px as f32 + 0.5) / width as f32;
    let v = (py as f32 + 0.5) / height as f32;
    let ndc_x = (2.0 * u - 1.0) * PRODUCT_HALF_HEIGHT * aspect;
    let ndc_y = (1.0 - 2.0 * v) * PRODUCT_HALF_HEIGHT;
    let forward = product_forward();
    let span = [forward[0] + ndc_x, forward[1] + ndc_y, forward[2]];
    let len = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
    Ray {
        origin: product_origin(),
        dir: [span[0] / len, span[1] / len, span[2] / len],
        max_t: PRODUCT_MAX_T,
        surface: PRODUCT_SURFACE,
        max_steps,
    }
}

/// The demo's content as the shipped pipeline sees it: the RON the boot's scene is authored in, compiled by
/// `assetc`'s own `Watch`, read back through the loader, and validated by the validator the march requires. The
/// depth returned is the one the compiler wrote into the bytes, which is what a shipped loader would specialize from.
fn product_scene() -> (Tree, u32) {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../assetc/tests/fixtures/boot_scene.ron");
    let input = std::fs::canonicalize(&raw)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", raw.display()));
    let output =
        std::env::temp_dir().join(format!("merkles_spec_product_{}.bin", std::process::id()));
    let mut watch = assetc::watch::Watch::default();
    let declared = match watch.observe(
        input.to_str().expect("the fixture path is utf-8"),
        output.to_str().expect("the temp path is utf-8"),
    ) {
        assetc::watch::Outcome::Rebuilt { depth, .. } => depth,
        other => panic!("a first observation of a fresh file must compile, got {other:?}"),
    };
    let bytes = std::fs::read(&output).expect("the compiler renamed its asset into place");
    let loaded = assetc::reader::from_bytes(&bytes)
        .unwrap_or_else(|error| panic!("{} must load: {error:?}", output.display()));
    let _ = std::fs::remove_file(&output);
    let tree = Tree {
        nodes: loaded.nodes,
        params: loaded.params,
    };
    render::validate(&tree).expect("the demo scene must pass the validator the march requires");
    (tree, declared)
}

fn frame_at(rows: &[FrameRow], cap: u32, shipped: bool) -> usize {
    rows.iter()
        .find(|row| row.cap == cap && row.shipped == shipped)
        .map(|row| row.hits)
        .unwrap_or(usize::MAX)
}

struct RefusalRow {
    name: &'static str,
    depth: u32,
    shipped_hits: usize,
    set_diffs: usize,
    exact_hits: usize,
    exact_diffs: usize,
    short_hits: usize,
    pixels: usize,
}
