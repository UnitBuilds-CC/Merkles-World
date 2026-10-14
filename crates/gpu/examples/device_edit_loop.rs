//! Gate R2 on a device: the ledger fills with terrain edits while the frame is marched by the shader.
//!
//! `cargo run --release -p gpu --example device_edit_loop`
//!
//! `crates/engine`'s `r2_edit_loop` measured R2 on the CPU reference march and said so; this is the same loop
//! over the same slab, the same read path and the same placement law, with the rays handed to
//! `interp.wgsl`'s `march_rays` instead of `render::raster`. That makes it the first R2 number produced by a
//! renderer that keeps the ray loop on the GPU, which is the shape ADR-012 and docs/04 ask for — no field
//! evaluation crosses the bus per pixel, and the only bytes retrieved are the depths.
//!
//! Two asserted terms, both integers or both geometry, never a duration:
//!
//! 1. **P1.7's placement, on the device.** Whatever the slab holds, the pixel straight down the eye's axis
//!    must return the edited voxel's surface where doc 02's `[x, x+1)` span puts it.
//! 2. **The device's march against the reference's, ray for ray, at every occupancy** — the same
//!    hit/miss-set anchor `on_device_march` uses, because the hit/miss set is the sensitive term there, not
//!    the depth tolerance. Without it a dispatch the driver folded away would arrive here as a frame.
//!
//! The milliseconds are printed and asserted nowhere, for the reason the CPU loop learned on the same
//! machine: two runs of identical code moved a stage median from 3.44 ms to 8.69 ms on load alone. What the
//! occupancy comparison can assert on a device is `March.hits`, the count of rays that found surface.
//!
//! Timings are never hashed and never gate evidence (ADR-017; R4 informational). R2's 10-minute span is
//! DERIVED from ten edits a second, and the line below says so.

use contracts::coords::LocalOffset;
use contracts::delta::{voxel_at, DeltaRecord, RegionSlot};
use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::Interpreter;
use memory::{Slab, SLAB_64KB_RECORDS};
use render::{march, Ray, Tree, ROOT};

/// Slot 0, as the boot and the CPU loop use it.
const SLOT_ZERO: contracts::coords::RegionKey = contracts::coords::RegionKey { x: 0, y: 0, z: 0 };
const WIDTH: u32 = 64;
const HEIGHT_GRID: u32 = 48;
const EYE_HEIGHT: f32 = 8.0;
/// The device march's own law, the same pair `on_device_march` marches a 1080p grid under.
const SURFACE: f32 = 0.01;
const MAX_STEPS: u32 = 64;
const MAX_T: f32 = 24.0;
const FRAMES_PER_STAGE: usize = 11;
const WARM_UP_FRAMES: usize = 3;

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// doc 02's `[x, x+1)`: a voxel's centre is its corner plus a half per axis. Same one derivation the CPU
/// loop and the boot use, restated here because the two crates may not reach through each other's surface.
fn voxel_centre(position: &contracts::coords::GlobalVoxelPos) -> [f32; 3] {
    [
        position.x as f32 + 0.5,
        position.y as f32 + 0.5,
        position.z as f32 + 0.5,
    ]
}

/// Terrain, with the edited voxel unioned in as its inscribed ball: A-07 has no instance node (OQ-55), so a
/// true cube is not expressible and the substitution is stated rather than hidden.
fn terrain(centre: Option<[f32; 3]>) -> Tree {
    match centre {
        None => Tree {
            nodes: vec![record(NODE_ROUNDED_BOX, 0, 1, 0, 4, true)],
            params: vec![2048.0, 2048.0, 0.5, 0.0],
        },
        Some(centre) => Tree {
            nodes: vec![
                record(NODE_UNION, 2, 3, 0, 0, true),
                record(NODE_ROUNDED_BOX, 0, 1, 0, 4, false),
                record(NODE_SPHERE, 0, 1, 4, 4, false),
            ],
            params: vec![
                2048.0, 2048.0, 0.5, 0.0, 0.5, centre[0], centre[1], centre[2],
            ],
        },
    }
}

/// Straight down at the edited voxel from `EYE_HEIGHT` above it, as a ray grid: `up = +x`, `right = +y`,
/// `forward = -z`, the same basis the CPU `Camera` in the engine's loop carries.
fn rays(centre: [f32; 3]) -> Vec<Ray> {
    let origin = [centre[0], centre[1], centre[2] + EYE_HEIGHT];
    let aspect = WIDTH as f32 / HEIGHT_GRID as f32;
    let mut out = Vec::with_capacity((WIDTH * HEIGHT_GRID) as usize);
    for py in 0..HEIGHT_GRID {
        for px in 0..WIDTH {
            let u = (px as f32 + 0.5) / WIDTH as f32;
            let v = (py as f32 + 0.5) / HEIGHT_GRID as f32;
            let dx = (2.0 * u - 1.0) * 0.5 * aspect;
            let dy = (1.0 - 2.0 * v) * 0.5;
            let length = (dx * dx + dy * dy + 1.0).sqrt();
            out.push(Ray {
                origin,
                dir: [dx / length, dy / length, -1.0 / length],
                max_t: MAX_T,
                surface: SURFACE,
                max_steps: MAX_STEPS,
            });
        }
    }
    out
}

/// The pixel on the eye's axis: row and column centres, not `len / 2`.
fn centre_pixel() -> usize {
    (HEIGHT_GRID / 2 * WIDTH + WIDTH / 2) as usize
}

fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(|a, b| a.total_cmp(b));
    times[times.len() / 2]
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP device_edit_loop: no device ({error:?}) - R2 on a renderer that keeps the ray loop on \
                 the GPU cannot be measured by a software rasteriser, and a CPU adapter would report the \
                 reference path as if it were a frame"
            );
            return;
        }
    };
    println!("// adapter          : {}", interpreter.adapter());
    println!(
        "// R2 on a device   : slab fills to 1 / 100 / 1,000 / 6,000 records; {WIDTH}x{HEIGHT_GRID} grid marched on the GPU, {} steps, surface {SURFACE}",
        MAX_STEPS
    );
    println!(
        "//                  | occupancy | frames | hits | centre depth (placement says {:.3}) | ms/frame median | deltas() ms",
        EYE_HEIGHT - 0.5
    );

    // The scene with no edit at all, marched once at the first stage's viewpoint: the same grid's centre
    // depth is what the edit has to beat, since every ray in this frame already finds the terrain plane.
    let bare = interpreter
        .march(&terrain(None), ROOT, &rays([0.5, 0.5, 1.5]))
        .expect("the bare terrain must march");
    let bare_centre = bare.depths[centre_pixel()];

    let stages = [1usize, 100, 1_000, 6_000];
    let mut buffer = vec![0u64; 16 * SLAB_64KB_RECORDS];
    let mut slab = Slab::new(&mut buffer);
    let mut appended = 0usize;
    let mut device_hits = Vec::with_capacity(stages.len());

    for &stage in &stages {
        while appended < stage {
            slab.append(DeltaRecord::new(
                RegionSlot(0),
                LocalOffset {
                    x: (appended % 96) as u16,
                    y: (appended / 96) as u16,
                    z: 1,
                },
                0x00c0 | (appended % 256) as u16,
            ))
            .expect("the slab holds every edit this loop makes");
            appended += 1;
        }

        // The ledger's own read path, then doc 02's span: the same two steps the CPU loop performs.
        let read_back: Vec<DeltaRecord> = slab.deltas().collect();
        assert_eq!(read_back.len(), appended, "every edit must come back");
        let newest = read_back.last().expect("a stage has an edit");
        let centre = voxel_centre(&voxel_at(&SLOT_ZERO, &newest.intra_region()));
        let grid = rays(centre);
        let tree = terrain(Some(centre));

        let read_start = std::time::Instant::now();
        let again: Vec<DeltaRecord> = slab.deltas().collect();
        let read_ms = read_start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(again.len(), appended);

        let mut times = Vec::with_capacity(FRAMES_PER_STAGE);
        let mut last_hits = 0usize;
        let mut last_depth = 0.0f32;
        for frame_index in 0..(FRAMES_PER_STAGE + WARM_UP_FRAMES) {
            let start = std::time::Instant::now();
            let frame = interpreter
                .march(&tree, ROOT, &grid)
                .expect("the grid must march on the device");
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if frame_index >= WARM_UP_FRAMES {
                times.push(elapsed);
            }
            last_hits = frame.hits;
            last_depth = frame.depths[centre_pixel()];

            // 1. Placement, on the device, inside the loop.
            let depth = frame.depths[centre_pixel()];
            let expected = EYE_HEIGHT - 0.5;
            assert!(
                (depth - expected).abs() < 0.05,
                "at occupancy {appended} the device put the newest edit at {depth} where doc 02 says {expected}"
            );

            // 2. The device's march against the reference's, ray for ray, at this occupancy.
            let mut disagreements = 0usize;
            let mut worst = 0.0f32;
            for (ray, depth) in grid.iter().zip(frame.depths.iter()) {
                let hit = march(
                    &tree,
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
            assert_eq!(
                disagreements, 0,
                "at occupancy {appended} the device and the reference disagree on which rays find surface"
            );
            assert!(
                worst < 0.002,
                "at occupancy {appended} the two marchers disagree on depth by {worst}"
            );
        }
        // Every ray in this frame already finds the terrain plane, so "the edit is worth something" cannot be
        // a hit count here - it is a depth. The surface under the eye must be a whole unit nearer than the
        // same grid marched over the unedited terrain, which is the visible-edit claim restated on the device.
        assert!(
            last_depth < bare_centre - 0.5,
            "at occupancy {appended} the device's centre ray found surface at {last_depth} against the unedited terrain's {bare_centre}, so the accepted edit never arrived in the frame"
        );
        device_hits.push(last_hits);
        println!(
            "//                  | {:>9} | {:>6} | {:>4} | {:>33.4} | {:>15.3} | {:>11.3}",
            appended,
            FRAMES_PER_STAGE,
            last_hits,
            last_depth,
            median(times),
            read_ms,
        );
    }

    // The occupancy-independence term available on a device is the integer the march reports: the same
    // fixed-size shape must prove the same number of rays onto surface whether the slab holds one edit or
    // six thousand. If this ever fails, the frame has started to read the ledger, which is R2's prohibition.
    let first = device_hits[0];
    for (index, hits) in device_hits.iter().enumerate() {
        assert_eq!(
            *hits, first,
            "the device's frame found {hits} surfaces at {} records against {first} at {} records",
            stages[index], stages[0]
        );
    }
    println!(
        "// O(1) on a device : {first} rays found surface at every occupancy from {} to {} records, asserted equal",
        stages[0],
        stages[stages.len() - 1]
    );
    println!(
        "// what that column is : a GUARD, not the edit's signature - the terrain is 2048 units wide, so all {} \
         of this grid's rays find surface with or without an edit, and {first} against {first} only says the \
         frame never went empty. The edit's worth is the depth under the eye: {bare_centre:.4} units over \
         unedited terrain against the edited surface reported above, which is what the per-frame asserts check.",
        WIDTH * HEIGHT_GRID,
    );
    println!("// milliseconds     : printed, not asserted - this machine moved a stage median by 5.6x between");
    println!("//                    runs of identical code, and a duration measured on a shared laptop is a");
    println!("//                    measurement of whatever else was running (ADR-017: timings are never evidence).");
    println!(
        "// 10-minute span   : the loop ran to {} edits = 10.0 minutes at 10 edits/second; 36,000 frames",
        stages[stages.len() - 1]
    );
    println!("//                    at 60 fps is DERIVED from the flat hit count above, not run.");
    println!("// device loop complete. R2's edit loop now has a number from a renderer that keeps the march on the GPU.");
}
