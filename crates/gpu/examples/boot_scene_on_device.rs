//! The pitch demo's scene, arriving as content and drawn by the shader.
//!
//! `cargo run --release -p gpu --example boot_scene_on_device`
//!
//! Every device number this repository has ever printed came from a tree the example itself assembled —
//! `on_device_march`, `device_edit_loop`, `r1_step_price` and `r2_device_span` all hand-build their nodes.
//! That is the right fixture for pricing a march and the wrong one for the demo's own clause, "a rendered
//! procedural scene", which docs/09 says arrives as **bytes an asset compiler emitted**. Meanwhile the boot
//! compiles its scene through `assetc` in memory and never lets a device see it, because `engine` does not
//! depend on `gpu` (wgpu in a `wasm32-wasip1` build would change the published boot output).
//!
//! This is that seam, closed at the only place it can be closed without touching the hashed boot:
//!
//! `crates/assetc/tests/fixtures/boot_scene.ron` → the shipped `assetc::watch::Watch` → **bytes on disk** →
//! `assetc::reader::from_bytes` → `render::Tree` → `upload` → `bind` → `write_rays` → `march_frame`.
//!
//! Four things are asserted, and all four are integers or geometry (ADR-017 — no duration gates anything):
//!
//! 1. **The content form is the boot's scene.** The compiler's own answer must be `Rebuilt { nodes: 11,
//!    depth: 4 }` and the pool must hold 25 parameters — the three numbers the boot prints for the shape it
//!    builds in code (`crates/engine/src/main.rs`, `scene source     : authored shape lowered by assetc (11
//!    nodes, depth 4), compiled to 348 bytes`). Two descriptions of one scene, checked to be one scene.
//! 2. **The device's march equals the reference's, ray for ray, over the boot's own camera law** — 64×48,
//!    `surface` 1e-3, 96 steps, max `t` 40, from `probe_camera()`. Zero hit/miss disagreements, and the
//!    worst depth deviation under 2e-3. This is the same anchor `on_device_march` uses, because the
//!    hit/miss set is the sensitive term, not the depth tolerance.
//! 3. **The reused-frame path equals the one-shot path, bit for bit** — `march_frame` against
//!    `march_uploaded` over all 3,072 `f32::to_bits`. AS4's buffer reuse must not be a second renderer.
//! 4. **The check can fail.** The same rays marched against a tampered pool (one parameter moved by 2.0)
//!    must *disagree* with the untouched reference, or the anchor above was measuring nothing.
//!
//! What this is NOT: not R1 (a 64×48 grid is a shape of a frame, not a 1080p budget), not a hashed number
//! (the picture is presentation — docs/02's float discipline; nothing here feeds a root), and not the boot
//! itself (the boot's 18 stdout lines and their digest are untouched; this example runs beside them).

use assetc::watch::{Outcome, Watch};
use gpu::device::Interpreter;
use render::{march, Ray, Tree, ROOT};

/// The boot's published scene, as three integers. If `scene_tree()` ever changes, this example is the
/// place that has to say so — the fixture and the in-code shape are one scene by assertion, not by habit.
const BOOT_NODES: usize = 11;
const BOOT_DEPTH: u32 = 4;
const BOOT_PARAMS: usize = 25;
const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
/// `probe_camera()`'s law, restated because `render::raster::ray_for` is `pub(crate)`: the pinhole
/// arithmetic is copied, not reached through another crate's private surface (AGENTS.md §2).
const HALF_HEIGHT: f32 = 0.5;
const MAX_T: f32 = 40.0;
const SURFACE: f32 = 1e-3;
const MAX_STEPS: u32 = 96;
/// The anchor every device example in this repository uses against the reference march.
const DEVIATION: f32 = 0.002;
/// A parameter the plant moves. Any single pool entry is geometric — a radius, a half-extent, a centre
/// axis or a blend `k` — so +2.0 on one of them cannot leave the surface where it was.
const TAMPER_DELTA: f32 = 2.0;

/// The fixture sits in `assetc`'s own test directory: it is the compiler's authoring input first and the
/// demo's content second, and gpu may not reach into engine's source tree. Canonicalised so the log line
/// names one path rather than `crates\gpu\../assetc/...`.
fn fixture() -> std::path::PathBuf {
    let raw = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../assetc/tests/fixtures/boot_scene.ron");
    std::fs::canonicalize(&raw)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", raw.display()))
}

/// The boot's 64×48 grid, aimed at the scene from `[0.0, 0.6, -4.0]` down its own axis — the same camera
/// step 6 of `cargo run -p engine` marches, so this is the demo's picture rather than a picture of it.
fn probe_rays() -> Vec<Ray> {
    let origin = [0.0f32, 0.6, -4.0];
    let forward = normalize3([0.0f32, -0.1, 1.0]);
    let right = [1.0f32, 0.0, 0.0];
    let up = [0.0f32, 1.0, 0.0];
    let aspect = WIDTH as f32 / HEIGHT as f32;
    let mut rays = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    for py in 0..HEIGHT {
        let v = (py as f32 + 0.5) / HEIGHT as f32;
        for px in 0..WIDTH {
            let u = (px as f32 + 0.5) / WIDTH as f32;
            let ndc_x = (2.0 * u - 1.0) * HALF_HEIGHT * aspect;
            let ndc_y = (1.0 - 2.0 * v) * HALF_HEIGHT;
            rays.push(Ray {
                origin,
                dir: normalize3([
                    forward[0] + right[0] * ndc_x + up[0] * ndc_y,
                    forward[1] + right[1] * ndc_x + up[1] * ndc_y,
                    forward[2] + right[2] * ndc_x + up[2] * ndc_y,
                ]),
                max_t: MAX_T,
                surface: SURFACE,
                max_steps: MAX_STEPS,
            });
        }
    }
    rays
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}

/// Ray-for-ray agreement between a set of device depths and the reference march. Returns the disagreement
/// count and the worst depth deviation, so the caller can assert either direction of it.
fn compare(tree: &Tree, rays: &[Ray], depths: &[f32]) -> (usize, f32) {
    let mut disagreements = 0usize;
    let mut worst = 0.0f32;
    for (ray, depth) in rays.iter().zip(depths.iter()) {
        let hit = march(
            tree,
            ROOT,
            ray.origin,
            ray.dir,
            ray.max_t,
            ray.surface,
            ray.max_steps,
        );
        match (hit, *depth < 1.0e37) {
            (Some(hit), true) => worst = worst.max((hit.t - *depth).abs()),
            (None, false) => {}
            _ => disagreements += 1,
        }
    }
    (disagreements, worst)
}

fn main() {
    let input = fixture();
    let output =
        std::env::temp_dir().join(format!("merkles_boot_scene_{}.bin", std::process::id()));
    // Windows canonicalisation prefixes a path with `\\?\`; the log should read like a path on every runner.
    println!(
        "// content file     : {}",
        input.display().to_string().trim_start_matches(r"\\?\")
    );

    // 1. Compile with the shipped compiler object, not with a re-implementation of its steps. Watch::observe
    //    reads the .ron, lowers it, self-checks the bytes it wrote and renames them into place — the whole
    //    docs/09 stage-one path, run by the crate that owns it.
    let mut watch = Watch::default();
    let outcome = watch.observe(
        input.to_str().expect("the fixture path is utf-8"),
        output.to_str().expect("the temp path is utf-8"),
    );
    let (nodes, depth) = match outcome {
        Outcome::Rebuilt { nodes, depth } => (nodes, depth),
        Outcome::Unchanged => panic!("a first observation of a fresh file cannot be unchanged"),
        Outcome::Failed(reason) => panic!("{} must compile: {reason}", input.display()),
    };
    assert_eq!(
        nodes, BOOT_NODES,
        "the content form compiled to {nodes} nodes where the boot's in-code shape publishes {BOOT_NODES}"
    );
    assert_eq!(
        depth, BOOT_DEPTH,
        "the content form compiled to depth {depth} where the boot publishes {BOOT_DEPTH}"
    );

    // The runtime side: bytes on disk, read by the loader the boot uses, validated by the gate it uses.
    let bytes = std::fs::read(&output).expect("the compiler renamed its asset into place");
    let loaded = assetc::reader::from_bytes(&bytes)
        .unwrap_or_else(|error| panic!("{} must load: {error:?}", output.display()));
    let tree = Tree {
        nodes: loaded.nodes,
        params: loaded.params,
    };
    render::validate(&tree).expect("the demo scene must pass the validator the march requires");
    assert_eq!(
        tree.params.len(),
        BOOT_PARAMS,
        "the pool holds {} parameters where the boot's shape needs {BOOT_PARAMS}",
        tree.params.len()
    );
    println!(
        "// compiled         : {} bytes, label {:?}, {} nodes, {} parameters, depth {depth} - the boot publishes 348 bytes / {BOOT_NODES} nodes / depth {BOOT_DEPTH} from its in-code shape",
        bytes.len(),
        loaded.label,
        tree.nodes.len(),
        tree.params.len(),
    );
    // The two byte counts differ by three, and the three are accounted for rather than noticed: the
    // compiler stamps the file name as the label and the boot stamps its own string, 17 characters against
    // this file's 14. If the header were anything other than a fixed 55 bytes over label + records + pool,
    // this identity would not hold and the two forms would not be one asset.
    const HEADER_BYTES: usize = 55;
    let tree_bytes = tree.nodes.len() * 16 + tree.params.len() * 4;
    assert_eq!(
        bytes.len(),
        tree_bytes + loaded.label.len() + HEADER_BYTES,
        "{} bytes is not the {tree_bytes}-byte tree plus a {:?} label plus a {HEADER_BYTES}-byte header, so the envelope differs from the boot's for a reason other than the label",
        bytes.len(),
        loaded.label,
    );
    assert_eq!(
        tree_bytes + HEADER_BYTES + "engine boot scene".len(),
        348,
        "the boot's published 348 bytes does not reconstruct from this tree, so the two forms of the demo \
         scene are not the same asset"
    );
    println!(
        "// byte counts     : {} bytes = {tree_bytes} tree + {} label + {HEADER_BYTES} header; the boot's 348 = the same tree + its 17-character label, so the only envelope difference is the stamped name",
        bytes.len(),
        loaded.label.len(),
    );

    // The arm's layer, priced on the demo scene's own rays — printed before the device opens, because it is
    // a CPU quantity and a GPU-less runner should still report it.
    //
    // `interp.wgsl` has no visit counter: its stage already binds the four storage buffers
    // `Limits::downlevel_defaults()` allows, so a counter would have to displace a real binding. That means
    // the one thing this seam cannot print is how often the device actually took a skip. What it *can* print
    // is the same question put to the evaluator the arm was transcribed from — `march_bnb` consults a bound
    // table and `SceneBounds::visits` counts the node entries it entered, while `counting_bounds` charges the
    // identical walk and refuses every skip, which is the baseline that makes the difference mean something.
    // No third implementation of A-07 is written here, deliberately: two walks of one grammar are the fork
    // this repository's whole interpreter suite exists to prevent.
    //
    // The device arm is a *subset* of the saving below (plain union only, bound unclamped, so it skips less
    // than the CPU may), and `rows licensed` is the part that applies to it directly: an infinite-radius row
    // is the table's own spelling of "nothing bounds this subtree", and the arm cannot fire on one.
    let rays = probe_rays();
    let licensed = render::cull::scene_bounds(&tree);
    let baseline = render::cull::counting_bounds(&tree);
    let table = licensed.encoded();
    let rows_licensed = table.iter().filter(|row| row[3].is_finite()).count();
    let mut flipped = 0usize;
    for ray in &rays {
        let pruned = render::march_bnb(&tree, &licensed, ROOT, *ray).is_some();
        let charged = render::march_bnb(&tree, &baseline, ROOT, *ray).is_some();
        if pruned != charged {
            flipped += 1;
        }
    }
    let (licensed_visits, baseline_visits) = (licensed.visits(), baseline.visits());
    assert_eq!(
        flipped, 0,
        "the bound table changed hit/miss status for {flipped} of {} rays, so an enclosure is too small: \
         skipping work that was load-bearing is the failure this arm exists to avoid",
        rays.len()
    );
    assert!(
        licensed_visits <= baseline_visits,
        "the pruned walk entered {licensed_visits} nodes and the refusal baseline entered {baseline_visits} — \
         a bound table that costs more entries than the walk it skips is a bug in the table"
    );
    println!(
        "// arm pricing     : {} table rows uploaded, {rows_licensed} with a finite radius and {} as +inf (an \
         +inf row bounds nothing, so the device arm cannot fire on it); over {rays_count} rays the walk entered \
         {licensed_visits} nodes with the table against {baseline_visits} with every skip refused — \
         {avoided} entries avoided, the pruned walk reaching the same hit/miss set on all of them \
         ({flipped} flipped). Upper bound on the device arm's saving, not a measurement of it: the arm asks \
         only plain union and leaves the bound unclamped, so it skips a subset of what the CPU may.",
        table.len(),
        table.len() - rows_licensed,
        avoided = baseline_visits - licensed_visits,
        rays_count = rays.len(),
    );

    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP boot_scene_on_device: no device ({error:?}) - the content-to-pixels seam cannot be \
                 closed by a software rasteriser, and a CPU adapter would report the reference path as if it \
                 were a frame"
            );
            println!("// compiled asset   : still written and read above, so the AS1-AS3 half of this ran on a GPU-less runner");
            return;
        }
    };
    println!("// adapter          : {}", interpreter.adapter());

    let shape = interpreter.upload(&tree);
    println!(
        "// uploaded         : {} node records, {} bytes resident, one allocation for the scene",
        shape.node_count, shape.bytes
    );

    // 2 + 3. The reused-frame path, then the one-shot path, both against the reference march.
    let mut frame = interpreter.begin_frame(rays.len());
    let wrote = interpreter
        .write_rays(&mut frame, ROOT, &rays)
        .expect("the grid must fit the frame");
    assert_eq!(
        wrote,
        rays.len() * 32,
        "the frame packed {wrote} bytes for {} rays, so a ray is not 32 bytes on this path",
        rays.len()
    );
    let bind = interpreter.bind(&shape, &frame);
    let marched = interpreter
        .march_frame(&bind, &frame)
        .expect("the demo scene must march on the device");
    let one_shot = interpreter
        .march_uploaded(&shape, ROOT, &rays)
        .expect("the same scene must march through the one-shot path too");

    assert_eq!(
        marched.max_steps, MAX_STEPS,
        "the device marched at {} steps under the boot's camera law of {MAX_STEPS} - the law was clamped, \
         so this is not the demo's frame",
        marched.max_steps
    );
    assert_eq!(
        marched.surface.to_bits(),
        SURFACE.to_bits(),
        "the device's surface epsilon {} is not the camera's {SURFACE}",
        marched.surface
    );

    let (disagreements, worst) = compare(&tree, &rays, &marched.depths);
    let (one_shot_disagreements, one_shot_worst) = compare(&tree, &rays, &one_shot.depths);
    let mut bit_differences = 0usize;
    for (a, b) in marched.depths.iter().zip(one_shot.depths.iter()) {
        if a.to_bits() != b.to_bits() {
            bit_differences += 1;
        }
    }
    println!(
        "// device vs CPU    : {} rays, {} surfaces found, {disagreements} hit/miss disagreements, worst depth deviation {worst:.6}",
        marched.rays, marched.hits
    );
    println!(
        "// one-shot vs CPU  : {one_shot_disagreements} disagreements, worst deviation {one_shot_worst:.6} ({} surfaces)",
        one_shot.hits
    );
    println!(
        "// reused vs one-shot: {bit_differences} of {} depths differ in any bit - the reused frame is the same renderer, not a second one",
        marched.depths.len()
    );

    assert_eq!(
        disagreements,
        0,
        "the device and the reference disagree on which of {} rays find the demo scene's surface",
        rays.len()
    );
    assert!(
        worst < DEVIATION,
        "the device's march of the compiled asset deviates from the reference by {worst}"
    );
    assert_eq!(
        one_shot_disagreements, 0,
        "the one-shot path disagrees with the reference on {} rays",
        one_shot_disagreements
    );
    assert!(
        one_shot_worst < DEVIATION,
        "the one-shot path deviates from the reference by {one_shot_worst}"
    );
    assert_eq!(
        bit_differences, 0,
        "the reused-frame path and the one-shot path disagree on {bit_differences} depths"
    );
    assert_eq!(
        one_shot.hits, marched.hits,
        "the two device paths found {} and {} surfaces for the same rays",
        one_shot.hits, marched.hits
    );

    // 4. The plant: the same rays against a scene whose pool has been moved, marched on the device. If the
    //    anchor above is sensitive, this must disagree; if it is measuring nothing, this arrives as a zero.
    let mut tampered = tree.clone();
    tampered.params[0] += TAMPER_DELTA;
    let tampered_shape = interpreter.upload(&tampered);
    let tampered_march = interpreter
        .march_uploaded(&tampered_shape, ROOT, &rays)
        .expect("the tampered scene must still march");
    let (planted, plant_deviation) = compare(&tree, &rays, &tampered_march.depths);
    assert!(
        planted > 0,
        "the plant FAILED TO PLANT: a pool entry moved by {TAMPER_DELTA} left all {} rays agreeing with the \
         untouched reference (worst deviation {plant_deviation}), so the anchor above proves nothing",
        rays.len()
    );
    println!(
        "// falsification    : moving pool[0] by {TAMPER_DELTA} flipped {planted} rays between surface and sky, while the rays that still agree stayed within {plant_deviation:.6} - the depth tolerance is blind to a scene that moved, the hit/miss set is not, which is why this repository anchors on the set"
    );

    let _ = std::fs::remove_file(&output);
    println!(
        "// what this lands   : the demo's scene reaches a device through the asset pipeline the docs promise, \
         and the picture the shader marches is the picture the reference march computes. R1's 16.67 ms at \
         1080p is untouched by it, and so is the boot's published output."
    );
}
