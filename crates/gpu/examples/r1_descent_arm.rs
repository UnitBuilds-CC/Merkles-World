//! R1's ray-level descent cull, armed on the device and priced at 1080p. This is task #69: the root bound
//! measured worthless (zero rays refused on the product content) so the arm to build is the per-node descent,
//! which refuses 851,956 of 2,073,600 rays at grown-by-surface margin (`r1_cull_price`). The arm reads the pool
//! tail's four words per node, walks the tree in WGSL with a local list of capacity 8, and writes UNSUPPORTED for
//! every ray whose segment never approaches any subtree enclosure within `surface`.
//!
//! `cargo run --release -p gpu --example r1_descent_arm`
//!
//! Durations assert nothing (ADR-017); integers are asserted.

use assetc;
use gpu::device::{GpuCamera, Interpreter};
use render::{march, Tree, Vec3, ROOT};

const PRODUCT_HALF_HEIGHT: f32 = 0.5;
const PRODUCT_MAX_T: f32 = 40.0;
const PRODUCT_SURFACE: f32 = 1.0e-3;
const PRODUCT_STEPS: u32 = 96;
const DEVIATION: f32 = 0.002;
const ANCHOR_W: u32 = 64;
const ANCHOR_H: u32 = 48;
const R1_W: u32 = 1920;
const R1_H: u32 = 1080;
const BUDGET_MS: f64 = 1000.0 / 60.0;
const MISS: f32 = 1.0e37;
const ROUNDS: usize = 7;
/// The CPU-priced upper bound on how many surfaces a bare-radius lie may eat at 1080p (r1_cull_price).
const BARE_BOUND: usize = 1_104;

fn product_origin() -> Vec3 {
    [0.0f32, 0.6, -4.0]
}

fn product_forward() -> [f32; 3] {
    let v = [0.0f32, -0.1, 1.0];
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}

fn product_ray(px: u32, py: u32, width: u32, height: u32, max_steps: u32) -> render::Ray {
    let aspect = width as f32 / height as f32;
    let u = (px as f32 + 0.5) / width as f32;
    let v = (py as f32 + 0.5) / height as f32;
    let ndc_x = (2.0 * u - 1.0) * PRODUCT_HALF_HEIGHT * aspect;
    let ndc_y = (1.0 - 2.0 * v) * PRODUCT_HALF_HEIGHT;
    let forward = product_forward();
    let span = [forward[0] + ndc_x, forward[1] + ndc_y, forward[2]];
    let len = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
    render::Ray {
        origin: product_origin(),
        dir: if len == 0.0 {
            [0.0, 0.0, 0.0]
        } else {
            [span[0] / len, span[1] / len, span[2] / len]
        },
        max_t: PRODUCT_MAX_T,
        surface: PRODUCT_SURFACE,
        max_steps,
    }
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

fn product_scene() -> Tree {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../assetc/tests/fixtures/boot_scene.ron");
    let input = std::fs::canonicalize(&raw)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", raw.display()));
    let output =
        std::env::temp_dir().join(format!("merkles_descent_arm_{}.bin", std::process::id()));
    let mut watch = assetc::watch::Watch::default();
    match watch.observe(
        input.to_str().expect("the fixture path is utf-8"),
        output.to_str().expect("the temp path is utf-8"),
    ) {
        assetc::watch::Outcome::Rebuilt { .. } => {}
        other => panic!("a first observation of a fresh file must compile, got {other:?}"),
    }
    let bytes = std::fs::read(&output).expect("the compiler renamed its asset into place");
    let loaded = assetc::reader::from_bytes(&bytes)
        .unwrap_or_else(|error| panic!("{} must load: {error:?}", output.display()));
    let _ = std::fs::remove_file(&output);
    let tree = Tree {
        nodes: loaded.nodes,
        params: loaded.params,
    };
    render::validate(&tree).expect("the demo scene must pass the validator");
    tree
}

fn bit_differences(a: &[f32], b: &[f32]) -> usize {
    a.iter()
        .zip(b.iter())
        .filter(|(l, r)| l.to_bits() != r.to_bits())
        .count()
}

fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
    v[v.len() / 2]
}

fn spread(values: &[f64]) -> f64 {
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    max - min
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(i) => i,
        Err(e) => {
            println!(
                "SKIP r1_descent_arm: no device ({e:?}) - the descent arm is dispatch plus copy plus map on a GPU"
            );
            return;
        }
    };
    let product = product_scene();
    let variant = interpreter
        .camera_cull_variant()
        .expect("interp.wgsl must carry the culled entry point exactly once");

    println!("// adapter    : {}", interpreter.adapter());
    println!("// budget     : gate R1 names {BUDGET_MS:.2} ms for one 1080p frame at 60 Hz on this device");
    println!("// asserted   : the descent arm keeps the picture identical (0 bit-differing depths), agrees with render::march, and lies only when the bound does");
    println!("// printed    : paired medians and budget ratios - durations assert nothing (ADR-017)");
    println!();

    // ---- Anchor grid: soundness, reference agreement, licensing neutrality ---------------------------
    let shape_licensed = interpreter.upload(&product);
    let shape_refused = interpreter.upload_bounds_refused(&product);
    let mut frame = interpreter.begin_camera_frame(ANCHOR_W, ANCHOR_H);
    interpreter.write_camera(&mut frame, &product_camera(ANCHOR_W, ANCHOR_H, PRODUCT_STEPS));

    let shipped_bind = interpreter.bind_camera(&shape_licensed, &frame);
    let culled_bind = interpreter.bind_camera_variant(&variant, &shape_licensed, &frame);

    let shipped = interpreter
        .march_camera_frame(&shipped_bind, &frame)
        .expect("shipped anchor must march");
    let culled = interpreter
        .march_camera_variant(&variant, &culled_bind, &frame)
        .expect("culled anchor must march");

    let diffs = bit_differences(&shipped.depths, &culled.depths);
    println!(
        "// anchor       : {ANCHOR_W}x{ANCHOR_H} - shipped {}, culled {}, {diffs} bit-differing depths",
        shipped.hits, culled.hits
    );
    assert_eq!(
        diffs, 0,
        "{diffs} depths differ between shipped and culled at the anchor, so the arm changes the picture"
    );
    assert_eq!(
        shipped.hits, culled.hits,
        "shipped found {} surfaces where culled found {} at the anchor - the arm eats geometry before the bound lies",
        shipped.hits, culled.hits
    );
    assert!(shipped.hits > 0, "the anchor grid finds no surfaces, so there is nothing to compare");

    // Reference agreement: culled vs render::march
    let mut set_diffs = 0usize;
    let mut worst = 0.0f32;
    let mut ref_hits = 0usize;
    let mut idx = 0usize;
    for py in 0..ANCHOR_H {
        for px in 0..ANCHOR_W {
            let ray = product_ray(px, py, ANCHOR_W, ANCHOR_H, PRODUCT_STEPS);
            let hit = march(
                &product, ROOT, ray.origin, ray.dir, ray.max_t, ray.surface, ray.max_steps,
            );
            if hit.is_some() {
                ref_hits += 1;
            }
            let dev = culled.depths[idx];
            if (hit.is_some()) != (dev < MISS) {
                set_diffs += 1;
            } else if let Some(h) = hit {
                worst = worst.max((h.t - dev).abs());
            }
            idx += 1;
        }
    }
    println!(
        "// reference    : {ref_hits} surfaces, {set_diffs} set disagreements, worst deviation {worst:.6}",
    );
    assert_eq!(set_diffs, 0, "{set_diffs} pixels disagree on hit-vs-miss with render::march");
    assert!(worst < DEVIATION, "worst deviation {worst:.6} exceeds {DEVIATION}");

    // Licensing neutrality: unlicensed shape (+inf bounds) → both arms produce the honest picture
    let unlic_shipped = interpreter.bind_camera(&shape_refused, &frame);
    let unlic_culled = interpreter.bind_camera_variant(&variant, &shape_refused, &frame);
    let ul_shipped = interpreter
        .march_camera_frame(&unlic_shipped, &frame)
        .expect("unlicensed shipped must march");
    let ul_culled = interpreter
        .march_camera_variant(&variant, &unlic_culled, &frame)
        .expect("unlicensed culled must march");
    let ul_diffs = bit_differences(&ul_shipped.depths, &ul_culled.depths);
    println!(
        "// unlicensed   : shipped {}, culled {}, {ul_diffs} bit diffs - +inf bounds are neutral",
        ul_shipped.hits, ul_culled.hits
    );
    assert_eq!(ul_diffs, 0, "{ul_diffs} bit diffs on an unlicensed shape - the arm is not inert");
    assert_eq!(
        ul_shipped.hits, shipped.hits,
        "unlicensed shipped found {} where licensed shipped found {} - skipping is not picture-neutral",
        ul_shipped.hits, shipped.hits
    );

    // Falsifier at anchor: shrunk by 100 → all radii negative → descent refuses everything → culled hits == 0
    let shape_shrunk100 = interpreter.upload_bounds_shrunk(&product, 100.0);
    let shrunk100_bind = interpreter.bind_camera_variant(&variant, &shape_shrunk100, &frame);
    let s100 = interpreter
        .march_camera_variant(&variant, &shrunk100_bind, &frame)
        .expect("shrunk-100 culled must march");
    println!(
        "// shrunk 100   : culled {} hits (expected 0 - the arm is in control)",
        s100.hits
    );
    assert_eq!(s100.hits, 0, "shrunk by 100 still found {} surfaces - the descent did not refuse every ray", s100.hits);

    // ---- 1080p legs: paired, order-alternating, under catch_unwind -----------------------------------
    let (w, h) = (R1_W, R1_H);
    let pixels = w as usize * h as usize;
    println!("// grid         : {w}x{h} = {pixels} pixels per frame, R1's own resolution");

    let outcome = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| {
        let mut rows = Vec::new();

        // Timed legs at cap 96 and cap 8
        for &(cap, expected_label) in &[(PRODUCT_STEPS, "cap 96"), (8u32, "cap 8")] {
            let mut big = interpreter.begin_camera_frame(w, h);
            interpreter.write_camera(&mut big, &product_camera(w, h, cap));
            let sh_bind = interpreter.bind_camera(&shape_licensed, &big);
            let cu_bind = interpreter.bind_camera_variant(&variant, &shape_licensed, &big);

            let mut sh_ms = Vec::with_capacity(ROUNDS);
            let mut cu_ms = Vec::with_capacity(ROUNDS);
            let mut sh_last = None;
            let mut cu_last = None;
            for round in 0..ROUNDS {
                if round % 2 == 0 {
                    sh_ms.push(
                        interpreter
                            .march_camera_frame(&sh_bind, &big)
                            .expect("shipped must frame")
                            .ms_frame_with_read,
                    );
                    let m = interpreter
                        .march_camera_variant(&variant, &cu_bind, &big)
                        .expect("culled must frame");
                    cu_ms.push(m.ms_frame_with_read);
                    cu_last = Some(m);
                } else {
                    let m = interpreter
                        .march_camera_variant(&variant, &cu_bind, &big)
                        .expect("culled must frame");
                    cu_ms.push(m.ms_frame_with_read);
                    cu_last = Some(m);
                    sh_ms.push(
                        interpreter
                            .march_camera_frame(&sh_bind, &big)
                            .expect("shipped must frame")
                            .ms_frame_with_read,
                    );
                }
                // Keep last depths for the diff check without holding 8 MB per round
                let m = interpreter
                    .march_camera_frame(&sh_bind, &big)
                    .expect("shipped re-march for depths");
                sh_last = Some(m);
            }
            let sh_march = sh_last.expect("shipped marched");
            let cu_march = cu_last.expect("culled marched");
            let d = bit_differences(&sh_march.depths, &cu_march.depths);

            println!(
                "// {expected_label:<8} : shipped {} ms (spread {:.2}), culled {} ms (spread {:.2}), ratio {:.3} x budget, {d} bit diffs",
                median(&sh_ms), spread(&sh_ms), median(&cu_ms), spread(&cu_ms), median(&cu_ms) / BUDGET_MS
            );
            assert_eq!(d, 0, "{d} bit-differing depths at {expected_label} - the arm changes the picture at 1080p");
            assert_eq!(
                sh_march.hits, cu_march.hits,
                "at {expected_label} shipped found {} surfaces where culled found {} - the arm eats geometry",
                sh_march.hits, cu_march.hits
            );

            rows.push((cap, sh_march.hits, cu_march.hits, median(&sh_ms), median(&cu_ms)));
        }

        // Falsifiers at cap 96
        let cap = PRODUCT_STEPS;
        let mut big = interpreter.begin_camera_frame(w, h);
        interpreter.write_camera(&mut big, &product_camera(w, h, cap));

        // Unlicensed: both arms must agree (the arm is inert when bounds are +inf)
        let ul_sh = interpreter.bind_camera(&shape_refused, &big);
        let ul_cu = interpreter.bind_camera_variant(&variant, &shape_refused, &big);
        let ul_sh_m = interpreter
            .march_camera_frame(&ul_sh, &big)
            .expect("unlicensed shipped 1080p must march");
        let ul_cu_m = interpreter
            .march_camera_variant(&variant, &ul_cu, &big)
            .expect("unlicensed culled 1080p must march");
        let ul_d = bit_differences(&ul_sh_m.depths, &ul_cu_m.depths);
        println!(
            "// 1080p unlic  : shipped {}, culled {}, {ul_d} bit diffs - +inf is neutral at full res too",
            ul_sh_m.hits, ul_cu_m.hits
        );
        assert_eq!(ul_d, 0, "{ul_d} bit diffs on unlicensed 1080p");
        assert_eq!(
            ul_sh_m.hits, ul_cu_m.hits,
            "unlicensed 1080p: shipped {} vs culled {} - licensing spelling broken",
            ul_sh_m.hits, ul_cu_m.hits
        );

        // Shrunk by 1.0: culled must lose strictly more than unculled on the same shape
        let sh_shrunk1 = interpreter.upload_bounds_shrunk(&product, 1.0);
        let cu_shrunk1 = interpreter.bind_camera_variant(&variant, &sh_shrunk1, &big);
        let sh_unculled_same = interpreter.bind_camera(&sh_shrunk1, &big);
        let sh_unculled_m = interpreter
            .march_camera_frame(&sh_unculled_same, &big)
            .expect("unculled shrunk-1 must march");
        let cu_shrunk1_m = interpreter
            .march_camera_variant(&variant, &cu_shrunk1, &big)
            .expect("culled shrunk-1 must march");
        let loss_unculled = ul_sh_m.hits.saturating_sub(sh_unculled_m.hits);
        let loss_culled = ul_sh_m.hits.saturating_sub(cu_shrunk1_m.hits);
        println!(
            "// shrunk 1.0   : unculled lost {loss_unculled}, culled lost {loss_culled} (culled differential {})",
            loss_culled.saturating_sub(loss_unculled)
        );
        assert!(
            loss_culled > loss_unculled,
            "culled lost {loss_culled} where unculled lost {loss_unculled} on the same shrunk shape - the descent \
             arm did not fire or the bound is not lying enough"
        );

        // Bare radius (shrunk by surface): culled loss bounded by the CPU price
        let sh_bare = interpreter.upload_bounds_shrunk(&product, PRODUCT_SURFACE);
        let cu_bare = interpreter.bind_camera_variant(&variant, &sh_bare, &big);
        let cu_bare_m = interpreter
            .march_camera_variant(&variant, &cu_bare, &big)
            .expect("culled bare must march");
        let loss_bare = ul_sh_m.hits.saturating_sub(cu_bare_m.hits);
        println!(
            "// bare radius  : culled lost {loss_bare} surfaces (CPU-priced bound {BARE_BOUND})",
        );
        assert!(
            loss_bare <= BARE_BOUND,
            "bare-radius culled lost {loss_bare} surfaces, exceeding the CPU-priced bound of {BARE_BOUND}"
        );

        // Shrunk by 100: culled hits must be exactly 0
        let sh_s100 = interpreter.upload_bounds_shrunk(&product, 100.0);
        let cu_s100 = interpreter.bind_camera_variant(&variant, &sh_s100, &big);
        let cu_s100_m = interpreter
            .march_camera_variant(&variant, &cu_s100, &big)
            .expect("culled shrunk-100 must march");
        println!(
            "// shrunk 100   : culled {} hits at 1080p (expected 0)",
            cu_s100_m.hits
        );
        assert_eq!(
            cu_s100_m.hits, 0,
            "shrunk by 100 still found {} surfaces at 1080p - the descent refused nothing",
            cu_s100_m.hits
        );

        rows
    }));

    match outcome {
        Ok(_) => {}
        Err(payload) => {
            let text = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "a panic with no message".to_string());
            if !(text.contains("device is lost") || text.contains("Validation")) {
                println!("// 1080p: UNEXPECTED PANIC, not a lost device, so it is a failure: {text}");
                std::process::exit(101);
            }
            println!("// 1080p: SKIPPED, the device was lost mid-leg: {text}");
            println!("//          (the anchor rows above are the asserted ones; no duration here is R1 evidence)");
        }
    }
}
