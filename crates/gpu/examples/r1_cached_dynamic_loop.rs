//! Does the temporal cache still show the frame the scene actually has, once something moves?
//!
//! `cargo run --release -p gpu --example r1_cached_dynamic_loop`
//!
//! The cache's published numbers were both taken with nothing moving: `r1_descent_arm`'s `reuse` row marches a second
//! frame after invalidating nothing, and `r1_temporal_benchmark` loops five marches over one bind group with no
//! invalidation anywhere. `r1_dynamic_benchmark` invalidates once and then marches three
//! times, so its median row was a fully-cached frame whose "1 % … 50 % dirty" labels described pixels the device
//! never saw (docs/04, "What that changes about the two dynamic benchmarks"). Nothing in the repo had asked the
//! question task #77 has to answer before any dynamic number means anything: **after a move and its invalidation,
//! is the cached frame the same picture an honest full march makes?**
//!
//! Per frame the loop moves something, then marches three ways and compares:
//!
//! * `stale` — the cache with nothing invalidated. Its disagreement count is the number of pixels the move really
//!   changed, and it is also the control on the mask: if the reuse branch were not gated on the dirty bit, this
//!   march would re-compute everything and come back equal to the reference, and the loop would be a pass earned by
//!   a shader that never caches;
//! * `repaired` — the cache after the policy under test dirties pixels. Every disagreement here is a **stale pixel
//!   on screen**: a pixel whose correct depth changed and whose dirty bit did not, or whose dirty bit did and whose
//!   re-march was swallowed anyway by the shader's own second cache layer (`use_cache`, which reuses a cached depth
//!   for a ray it never tested);
//! * `reference` — the shipped, unculled `march_rays_from_camera` on a separate frame buffer. It names no
//!   `@group(1)` and cannot consult a cache, and its agreement with the culled path was already proven bit for bit
//!   (docs/04's rows 1 and 2), so a difference here belongs to the temporal cache and to nothing else.
//!
//! Two policies run, because a real loop needs both: an entity moving under a still camera invalidates the union of
//! where it was and where it went; a camera moving invalidates the frame. Durations print and assert nothing
//! (ADR-017); the integers are the evidence. SKIP where there is no adapter, which is every CI runner but the macOS
//! leg's Apple Paravirtual device.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{Bind, CameraFrame, GpuCamera, Interpreter, March, Shape};
use render::{cull::scene_bounds, Tree, ROOT};
use std::time::Instant;

/// The no-surface sentinel: `device.rs` writes it into every ray the march leaves untouched.
const MISS: f32 = 1.0e37;

/// The moving entity's slide per frame, in world units, and the camera's pan per frame. Both are chosen so the
/// change is unambiguously larger than the march's surface epsilon - a move too small to change any depth would
/// make every assertion below vacuous.
const ENTITY_STEP: f32 = 0.12;
const CAMERA_STEP: f32 = 0.06;

fn scene(shift: f32) -> Tree {
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
        // The sphere's parameters are the moving entity: `shift` slides it along x. The rounded box is scenery, so
        // a policy that dirties the whole frame for this move is visibly more than the scene asked for.
        params: vec![
            0.5 + shift,
            0.0,
            0.0,
            0.0,
            2.0,
            0.1,
            2.0,
            0.0,
            0.0,
            -1.0,
            0.0,
        ],
    }
}

/// The sphere's world AABB, read from the same bound table whose rows the shader's arm licenses, so the region a
/// policy dirties and the geometry the march sees cannot drift apart inside the instrument.
fn entity_aabb(tree: &Tree) -> ([f32; 3], [f32; 3]) {
    let [cx, cy, cz, r] = scene_bounds(tree).encoded()[1];
    ([cx - r, cy - r, cz - r], [cx + r, cy + r, cz + r])
}

fn open_gpu() -> Result<Interpreter, String> {
    let instance = wgpu::Instance::default();
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());
    let chosen = adapters
        .iter()
        .filter(|a| a.get_info().device_type == wgpu::DeviceType::DiscreteGpu)
        .chain(
            adapters
                .iter()
                .filter(|a| a.get_info().device_type == wgpu::DeviceType::IntegratedGpu),
        )
        .find(|a| !a.get_info().name.contains("Microsoft"))
        .ok_or("no adapter that is not a software rasteriser")?;
    let info = chosen.get_info();
    println!("adapter: {} [{:?}]", info.name, info.device_type);
    Interpreter::open_with_adapter(chosen.clone()).map_err(|e| format!("{e:?}"))
}

fn setting(name: &str, fallback: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(fallback)
}

fn camera(width: u32, height: u32, pan: f32) -> GpuCamera {
    let v = [pan, -0.1, 1.0];
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    GpuCamera {
        origin: [0.0, 0.6, -4.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        forward: [v[0] / l, v[1] / l, v[2] / l],
        half_height: 0.5,
        max_t: 40.0,
        surface: 1.0e-3,
        max_steps: 64,
        root: ROOT,
        width,
        height,
    }
}

/// Pixels the device's own mask holds, rather than the pixels the caller meant to dirty. Bits past the frame's last
/// pixel live in the final word and are not pixels, so they are excluded rather than counted.
fn dirty_pixels(bind: &Bind, device: &wgpu::Device, queue: &wgpu::Queue, pixels: u32) -> usize {
    let words = bind
        .read_dirty_mask(device, queue)
        .expect("the cached frame's dirty mask must be readable");
    let whole = (pixels / 32) as usize;
    let counted: usize = words[..whole].iter().map(|w| w.count_ones() as usize).sum();
    let rest = pixels % 32;
    let tail = if rest == 0 {
        0
    } else {
        (words[whole] & ((1u32 << rest) - 1)).count_ones() as usize
    };
    counted + tail
}

/// Depths that differ, and how many of those flip a pixel between surface and sky. The split matters: a depth that
/// moved within one pixel's surface is a re-march's arithmetic, while a pixel that gained or lost its surface is
/// geometry appearing or disappearing on screen.
fn compare(cached: &March, reference: &March) -> (usize, usize) {
    let (mut bits, mut flips) = (0usize, 0usize);
    for (a, b) in cached.depths.iter().zip(reference.depths.iter()) {
        if a.to_bits() != b.to_bits() {
            bits += 1;
        }
        if (a >= &MISS) != (b >= &MISS) {
            flips += 1;
        }
    }
    (bits, flips)
}

/// One frame's three marches, printed as one row. `dirty` is the policy's own invalidation, run between the stale
/// march and the repaired one - the order is the point, since the stale march is what the mask would have produced.
/// `dirt` is what the mask held after that policy spoke, `stale` counts what the mask had been protecting, and
/// `repaired` counts what survived the policy: the only column that may be non-zero and still be called correct.
struct Row {
    frame: u32,
    dirt: usize,
    stale: usize,
    stale_flips: usize,
    repaired: usize,
    repaired_flips: usize,
    cached_hits: usize,
    reference_hits: usize,
    ms_cached: f64,
    ms_reference: f64,
}

/// The handles one loop needs, kept together because the three marches of a frame all read them and a
/// eleven-argument function is a sign the grouping belongs in a type.
struct Harness<'a> {
    interpreter: &'a Interpreter,
    variant: &'a gpu::device::CameraVariant,
    cached_bind: &'a Bind,
    plain_bind: &'a Bind,
    pixels: u32,
}

impl<'a> Harness<'a> {
    fn march_both(
        &self,
        frame: u32,
        marched: &CameraFrame,
        reference_frame: &CameraFrame,
        dirty: impl Fn(),
    ) -> Row {
        let (device, queue) = (self.interpreter.device(), self.interpreter.queue());
        let stale = self
            .interpreter
            .march_camera_variant(self.variant, self.cached_bind, marched)
            .expect("the un-invalidated march must complete");
        let start = Instant::now();
        let reference = self
            .interpreter
            .march_camera_frame(self.plain_bind, reference_frame)
            .expect("the reference march must complete");
        let ms_reference = start.elapsed().as_secs_f64() * 1000.0;
        dirty();
        let dirt = dirty_pixels(self.cached_bind, device, queue, self.pixels);
        let start = Instant::now();
        let repaired = self
            .interpreter
            .march_camera_variant(self.variant, self.cached_bind, marched)
            .expect("the repaired march must complete");
        let ms_cached = start.elapsed().as_secs_f64() * 1000.0;
        let (stale_bits, stale_flips) = compare(&stale, &reference);
        let (repaired_bits, repaired_flips) = compare(&repaired, &reference);
        Row {
            frame,
            dirt,
            stale: stale_bits,
            stale_flips,
            repaired: repaired_bits,
            repaired_flips,
            cached_hits: repaired.hits,
            reference_hits: reference.hits,
            ms_cached,
            ms_reference,
        }
    }
}

fn print_row(row: &Row) {
    println!(
        "  {:>4} | {:>9} | {:>14} | {:>15} | {:>8} | {:>8} | {:>7.2} / {:>7.2}",
        row.frame,
        row.dirt,
        format!("{} ({})", row.stale, row.stale_flips),
        format!("{} ({})", row.repaired, row.repaired_flips),
        row.cached_hits,
        row.reference_hits,
        row.ms_cached,
        row.ms_reference,
    );
}

fn main() {
    let interpreter = match open_gpu() {
        Ok(it) => it,
        Err(reason) => {
            println!("SKIP r1_cached_dynamic_loop: {reason}");
            return;
        }
    };
    let variant = interpreter
        .camera_cull_variant()
        .expect("the culled camera entry point exists to be compiled");
    let width = setting("CACHE_LOOP_WIDTH", 640);
    let height = setting("CACHE_LOOP_HEIGHT", 360);
    let frames = setting("CACHE_LOOP_FRAMES", 6);
    let pixels = width * height;
    let queue = interpreter.queue();

    let mut shape: Shape = interpreter.upload(&scene(0.0));
    let mut marched = interpreter.begin_camera_frame(width, height);
    let mut reference_frame = interpreter.begin_camera_frame(width, height);
    let start_cam = camera(width, height, 0.0);
    interpreter.write_camera(&mut marched, &start_cam);
    interpreter.write_camera(&mut reference_frame, &start_cam);
    let cached_bind = interpreter.bind_camera_variant(&variant, &shape, &marched);
    let plain_bind = interpreter.bind_camera(&shape, &reference_frame);
    let harness = Harness {
        interpreter: &interpreter,
        variant: &variant,
        cached_bind: &cached_bind,
        plain_bind: &plain_bind,
        pixels,
    };

    println!(
        "\ntemporal cache in a moving-scene loop - {}x{}, {} frames per policy",
        width, height, frames
    );

    cached_bind.invalidate_all(queue);
    let established = interpreter
        .march_camera_variant(&variant, &cached_bind, &marched)
        .expect("the establishing march must complete");
    let establish_reference = interpreter
        .march_camera_frame(&plain_bind, &reference_frame)
        .expect("the establishing reference march must complete");
    let (establish_bits, _) = compare(&established, &establish_reference);
    println!(
        "  frame  0 (both paths from empty): {} cached hits vs {} reference hits, {establish_bits} bit diffs",
        established.hits, establish_reference.hits
    );

    let mut failures: Vec<String> = Vec::new();
    if establish_bits != 0 {
        failures.push(format!(
            "with nothing cached, the culled and unculled paths disagree on {establish_bits} depths - the two \
             pipelines are not one scene, so no later row means anything"
        ));
    }

    // ---- policy 1: an entity moves under a still camera; dirty the union of its old and new footprints ----
    println!(
        "\nentity moves, camera still - stale march, then the entity's old+new regions, then both compared to the \
         uncacheable reference"
    );
    println!(" frame |   dirty px | stale diffs (flips) | repaired diffs (flips) | hits cached | hits ref | ms cached / ms ref");
    let mut prev = entity_aabb(&scene(0.0));
    let mut entity_stale = 0usize;
    let mut entity_worst = 0usize;
    for f in 1..=frames {
        let tree = scene(ENTITY_STEP * f as f32);
        interpreter
            .upload_into(&mut shape, &tree)
            .expect("the same-shaped tree slides inside the resident buffers");
        let next = entity_aabb(&tree);
        let cam = camera(width, height, 0.0);
        interpreter.write_camera(&mut marched, &cam);
        interpreter.write_camera(&mut reference_frame, &cam);
        // Where it *was* as well as where it goes: a pixel the entity vacated now shows whatever stands behind it,
        // so leaving the old footprint clean is a stale pixel by definition.
        let (old_min, old_max) = prev;
        let (new_min, new_max) = next;
        let row = harness.march_both(f, &marched, &reference_frame, || {
            let _ = cached_bind.invalidate_entity(queue, &cam, old_min, old_max);
            let _ = cached_bind.invalidate_entity(queue, &cam, new_min, new_max);
        });
        print_row(&row);
        entity_stale += row.stale;
        entity_worst = entity_worst.max(row.repaired);
        prev = next;
    }
    if entity_stale == 0 {
        failures.push(
            "no frame's entity slide changed a single depth - the loop moved nothing and every equality below is \
             an equality between two copies of one picture"
                .to_string(),
        );
    }

    // ---- policy 2: the camera moves; dirty the whole frame ----
    println!(
        "\ncamera pans, entity still - stale march, then invalidate_all, then both compared to the uncacheable \
         reference"
    );
    println!(" frame |   dirty px | stale diffs (flips) | repaired diffs (flips) | hits cached | hits ref | ms cached / ms ref");
    let mut camera_stale = 0usize;
    let mut camera_worst = 0usize;
    for f in 1..=frames {
        let cam = camera(width, height, CAMERA_STEP * f as f32);
        interpreter.write_camera(&mut marched, &cam);
        interpreter.write_camera(&mut reference_frame, &cam);
        let row = harness.march_both(f, &marched, &reference_frame, || {
            cached_bind.invalidate_all(queue);
        });
        print_row(&row);
        camera_stale += row.stale;
        camera_worst = camera_worst.max(row.repaired);
    }
    if camera_stale == 0 {
        failures.push(
            "no frame's camera pan changed a single depth - the instrument cannot see a stale pixel it never \
             moves"
                .to_string(),
        );
    }

    if entity_worst != 0 {
        failures.push(format!(
            "the entity policy left a stale pixel: the worst frame disagreed with the uncacheable reference on \
             {entity_worst} depths after its regions were invalidated"
        ));
    }
    if camera_worst != 0 {
        failures.push(format!(
            "the whole-frame policy left a stale pixel: the worst frame disagreed with the uncacheable reference \
             on {camera_worst} depths after invalidate_all"
        ));
    }

    println!(
        "\nsums - entity policy: {} stale pixels across {} frames, worst repaired {}; camera policy: {} stale, \
         worst repaired {}",
        entity_stale, frames, entity_worst, camera_stale, camera_worst
    );

    if failures.is_empty() {
        println!(
            "RESULT: every repaired frame equalled the uncacheable reference bit for bit, and every stale frame \
             did not - the dirty mask decides what the cache may reuse."
        );
        return;
    }
    for failure in &failures {
        eprintln!("FAIL: {failure}");
    }
    std::process::exit(1);
}
