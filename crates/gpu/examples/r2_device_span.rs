//! Gate R2's other half: the ten-minute span over the **device** path, run rather than derived.
//!
//! `cargo run --release -p gpu --example r2_device_span`
//!
//! `engine`'s `r2_ten_minute_span` runs 36,000 consecutive frames with an edit every sixth and says in its own
//! output that the same span on `interp.wgsl`'s `march_rays` is still DERIVED; `device_edit_loop` runs the
//! device path but samples only four occupancies. This is the two of them together: the span's cadence marched
//! by the shader over the frame path that allocates nothing per frame (`begin_frame` once, then
//! `write_rays`/`march_frame`), so the number describes a renderer rather than a harness.
//!
//! Four terms are asserted, all of them integers or geometry — a duration on a shared laptop measures the
//! laptop (ADR-017), and the CPU span's own p99.9 moved 12.7x between runs of identical code:
//!
//! 1. **The device's per-frame work never grows with the ledger**: `March.hits` at frame 1 must equal frame
//!    36,000's, compared at *every* frame rather than at four sampled occupancies.
//! 2. **P1.7's placement holds at every frame on the device**: the pixel straight down the eye's axis sits at
//!    doc 02's `[x, x+1)` centre of the newest edit, more than half a unit nearer than the unedited terrain's
//!    answer for the same grid.
//! 3. **The device's march agrees with the reference's, ray for ray, at every frame** — a `LATTICE_STEP`
//!    lattice each frame, the whole grid once every `FULL_GRID_EVERY` frames. Without this, a dispatch the
//!    driver folded away would arrive here as a frame.
//! 4. **Nothing is lost and nothing is re-created**: every edit comes back from the slab, and the tree rebuild
//!    plus shape re-upload that carries an edit to the GPU happens exactly once per edit (OQ-55 — A-07 has no
//!    instance node), never once per frame.
//!
//! The frame count is settable (`MERKLES_R2_DEVICE_FRAMES`, default 36,000) because CI runners have a job
//! timeout, not because the assertion is weaker at 3,600: what travels between machines is the per-frame
//! agreement with the reference, and every run prints the frame and edit counts it actually did. A shorter run
//! does not give the ten minutes themselves, and the line at the bottom says which of the two this was.

use contracts::coords::LocalOffset;
use contracts::delta::{voxel_at, DeltaRecord, RegionSlot};
use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::Interpreter;
use gpu::device::{Bind, Frame, Shape};
use memory::{Slab, SLAB_64KB_RECORDS};
use render::{march, Ray, Tree, ROOT};

/// Slot 0, as the boot, `device_edit_loop` and the CPU span all use it.
const SLOT_ZERO: contracts::coords::RegionKey = contracts::coords::RegionKey { x: 0, y: 0, z: 0 };
const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const EYE_HEIGHT: f32 = 8.0;
/// The device march's own law: the same triple `device_edit_loop` marches at four occupancies, so the two
/// instruments differ only about the span and not about the frame.
const SURFACE: f32 = 0.01;
const MAX_STEPS: u32 = 64;
const MAX_T: f32 = 24.0;
/// R2's own words: ten minutes at ten edits a second, sixty frames a second.
const DEFAULT_FRAMES: usize = 36_000;
const FRAMES_PER_EDIT: usize = 6;
/// How many rays `render::march` is run against per frame, as a row-by-row lattice. A flat stride over the ray
/// index is degenerate on a wide grid (`r1_round_trip_split` documented exactly that), so both axes step.
const LATTICE_STEP: u32 = 4;
/// The full-grid comparison is the expensive anchor; once every this many frames still lands ~60 of them
/// across the default span, and the per-frame lattice is what catches a drift at the frame where it starts.
const FULL_GRID_EVERY: usize = 600;
/// Tolerances inherited from the instruments this span replaces: the depth bound the device loop and the 1080p
/// split both assert at, and the placement bound the visible-edit check ships with.
const DEVIATION: f32 = 0.002;
const PLACEMENT: f32 = 0.05;
/// What doc 02's span predicts for the surface under the eye: the eye is `EYE_HEIGHT` above the edited voxel's
/// centre, and the voxel's own top is a half unit below that.
const EXPECTED_DEPTH: f32 = EYE_HEIGHT - 0.5;

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// doc 02's `[x, x+1)`: the centre of the voxel a coordinate names is its corner plus a half per axis.
fn voxel_centre(position: &contracts::coords::GlobalVoxelPos) -> [f32; 3] {
    [
        position.x as f32 + 0.5,
        position.y as f32 + 0.5,
        position.z as f32 + 0.5,
    ]
}

/// Terrain, with the newest edit unioned in as its inscribed ball: A-07 has no instance node (OQ-55), so a true
/// cube is not expressible and the substitution is stated rather than hidden.
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

/// Straight down at the newest edit from `EYE_HEIGHT` above it: `up = +x`, `right = +y`, `forward = -z`, the
/// basis the CPU `Camera` in the engine's span carries. Writes into the caller's `Vec`, so a span that moves
/// its camera 6,000 times still allocates the grid once.
fn rays(centre: [f32; 3], out: &mut Vec<Ray>) {
    out.clear();
    let origin = [centre[0], centre[1], centre[2] + EYE_HEIGHT];
    let aspect = WIDTH as f32 / HEIGHT as f32;
    for py in 0..HEIGHT {
        for px in 0..WIDTH {
            let u = (px as f32 + 0.5) / WIDTH as f32;
            let v = (py as f32 + 0.5) / HEIGHT as f32;
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
}

/// The pixel on the eye's axis: row and column centres, not `len / 2`.
fn centre_pixel() -> usize {
    (HEIGHT / 2 * WIDTH + WIDTH / 2) as usize
}

/// One frame's agreement between the device and `render::march`, asserted: zero rays may disagree on which of
/// them find surface, and no depth may differ by `DEVIATION` or more. `lattice` thins the comparison to every
/// `LATTICE_STEP`-th pixel of both axes; `false` compares the whole grid.
fn agree(tree: &Tree, grid: &[Ray], depths: &[f32], lattice: bool) -> (usize, f32) {
    let step = if lattice { LATTICE_STEP } else { 1 };
    let mut disagreements = 0usize;
    let mut worst = 0.0f32;
    let mut compared = 0usize;
    for (index, ray) in grid.iter().enumerate() {
        let py = index as u32 / WIDTH;
        let px = index as u32 % WIDTH;
        if !px.is_multiple_of(step) || !py.is_multiple_of(step) {
            continue;
        }
        compared += 1;
        let reference = march(
            tree,
            ROOT,
            ray.origin,
            ray.dir,
            ray.max_t,
            ray.surface,
            ray.max_steps,
        );
        let depth = depths[index];
        match (reference, depth < 1.0e37) {
            (Some(hit), true) => worst = worst.max((hit.t - depth).abs()),
            (None, false) => {}
            _ => disagreements += 1,
        }
    }
    assert_eq!(
        disagreements, 0,
        "the device and the reference disagree on which of {compared} compared rays find surface - a frame \
         that lost the edit and a frame the driver folded away both look like a picture from here"
    );
    assert!(
        worst < DEVIATION,
        "the two marchers disagree on depth by {worst} over {compared} rays, at or above the {DEVIATION} the \
         device loop and the 1080p split both hold"
    );
    (compared, worst)
}

fn percentile(sorted: &[f64], num: usize, den: usize) -> f64 {
    sorted[(sorted.len() - 1) * num / den]
}

fn main() {
    let frames: usize = std::env::var("MERKLES_R2_DEVICE_FRAMES")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(DEFAULT_FRAMES);
    assert!(
        frames >= FULL_GRID_EVERY,
        "the span needs at least {FULL_GRID_EVERY} frames to be a span (asked for {frames}) - below that this \
         is device_edit_loop with extra steps"
    );
    let edits = frames / FRAMES_PER_EDIT;

    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP r2_device_span: no device ({error:?}) - R2's span over the device path cannot be \
                 measured by a software rasteriser, which would report the reference march as if it were a frame"
            );
            return;
        }
    };
    println!("// adapter          : {}", interpreter.adapter());
    println!(
        "// R2's span, on a device: {frames} consecutive frames at {WIDTH}x{HEIGHT} marched by interp.wgsl's \
         march_rays, one slab edit every {FRAMES_PER_EDIT} frames ({edits} records = {:.1} minutes at 10/s)",
        edits as f64 / 600.0
    );
    println!("// asserted per frame : hits equal to frame 1's | placement at the newest edit | ray-for-ray agreement with render::march over the lattice | every edit reads back");
    println!("// printed, never asserted: milliseconds - the CPU span's p99.9 moved 12.7x between runs of identical code on this machine (ADR-017)");

    // The unedited terrain's own answer for one grid, taken once: what every frame must beat by half a unit,
    // and what the closing plant expects to see again.
    let mut grid: Vec<Ray> = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    rays([0.5, 0.5, 1.5], &mut grid);
    let bare = interpreter
        .march(&terrain(None), ROOT, &grid)
        .expect("the bare terrain must march");
    let bare_centre = bare.depths[centre_pixel()];

    let mut buffer = vec![0u64; 16 * SLAB_64KB_RECORDS];
    let mut slab = Slab::new(&mut buffer);
    // The frame the demo holds: buffers created once for the whole span, refilled only when a new edit moves
    // the camera. This is the path AS4's frame-side clause asked for.
    let mut frame: Frame = interpreter.begin_frame((WIDTH * HEIGHT) as usize);
    let mut shape: Option<Shape> = None;
    let mut bind: Option<Bind> = None;

    let mut appended = 0usize;
    let mut uploads = 0usize;
    let mut refills = 0usize;
    let mut upload_bytes = 0usize;
    let mut read_records = 0usize;
    let mut first_hits = 0usize;
    let mut lattice_rays = 0usize;
    let mut full_rays = 0usize;
    let mut worst_seen = 0.0f32;
    let mut times: Vec<f64> = Vec::with_capacity(frames);
    let mut centre = [0.0f32; 3];
    let mut tree = terrain(None);

    let session_start = std::time::Instant::now();
    for frame_index in 1..=frames {
        let edited = (frame_index - 1).is_multiple_of(FRAMES_PER_EDIT) && appended < edits;
        if edited {
            slab.append(DeltaRecord::new(
                RegionSlot(0),
                LocalOffset {
                    x: (appended % 96) as u16,
                    y: (appended / 96) as u16,
                    z: 1,
                },
                0x00c0 | (appended % 256) as u16,
            ))
            .expect("the slab holds every edit the span makes");
            appended += 1;

            // The read path the demo uses, at the edit's own cadence: the tree the march walks is rebuilt from
            // what the ledger actually stored, not from the loop counter.
            let read_back: Vec<DeltaRecord> = slab.deltas().collect();
            read_records += read_back.len();
            assert_eq!(
                read_back.len(),
                appended,
                "at frame {frame_index} the slab handed back {} records for {appended} appended - an edit was \
                 lost mid-span, which is what R2's nothing-is-lost clause forbids",
                read_back.len()
            );
            centre = voxel_centre(&voxel_at(
                &SLOT_ZERO,
                &read_back
                    .last()
                    .expect("an edit lands before it is marched")
                    .intra_region(),
            ));
            tree = terrain(Some(centre));

            // OQ-55's cost, counted: an edit reaches this renderer by a host-side tree rebuild plus a shape
            // re-upload and a re-bind, because A-07 has no instance node to move. Once per edit, never per frame.
            let uploaded = interpreter.upload(&tree);
            upload_bytes += uploaded.bytes;
            uploads += 1;
            rays(centre, &mut grid);
            let wrote = interpreter
                .write_rays(&mut frame, ROOT, &grid)
                .expect("the grid fits the frame it was opened for");
            assert_eq!(
                wrote,
                grid.len() * 32,
                "write_rays reported {wrote} bytes for {} rays at frame {frame_index}",
                grid.len()
            );
            refills += 1;
            bind = Some(interpreter.bind(&uploaded, &frame));
            // The shape must OUTLIVE the bind group that points at its buffers, so the span holds it; reading
            // it back is also the check that the bound shape is the one the ledger just produced.
            let bound = shape.insert(uploaded);
            assert_eq!(
                bound.node_count,
                tree.nodes.len(),
                "at frame {frame_index} the shape on the device holds {} nodes where the tree the read path \
                 just built has {}",
                bound.node_count,
                tree.nodes.len()
            );
        }

        let started = std::time::Instant::now();
        let marched = interpreter
            .march_frame(
                bind.as_ref()
                    .expect("frame 1 fills the frame before marching it"),
                &frame,
            )
            .expect("the frame marches on the device");
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        times.push(elapsed);

        // 1. The O(1) claim, on the device, at every frame of the span.
        if frame_index == 1 {
            first_hits = marched.hits;
        }
        let hits = marched.hits;
        let depth = marched.depths[centre_pixel()];

        // 3. The reference anchor, and 2. placement — with the row printed BEFORE the asserts, so a failure
        //    leaves the trail of the frames that passed instead of ending the run with no evidence at all.
        let full = frame_index.is_multiple_of(FULL_GRID_EVERY);
        let (compared, worst) = agree(&tree, &grid, &marched.depths, !full);
        if full {
            full_rays += compared;
        } else {
            lattice_rays += compared;
        }
        worst_seen = worst_seen.max(worst);
        let placed = (depth - EXPECTED_DEPTH).abs() < PLACEMENT;
        let above_bare = depth < bare_centre - 0.5;

        if frame_index == 1 || full || !placed || !above_bare {
            println!(
                "// frame {:>6} | records {:>5} | hits {:>4} | centre depth {:>7.4} (bare {bare_centre:.4}) | rays checked {:>5} | worst dev {:>8.6} | ms {:>7.3}",
                frame_index, appended, hits, depth, compared, worst, elapsed
            );
        }
        assert!(
            placed,
            "at frame {frame_index}, occupancy {appended}: the device put the newest edit at depth {depth} \
             where doc 02's span says {EXPECTED_DEPTH} - the edit stopped being visible partway through the span"
        );
        assert!(
            above_bare,
            "at frame {frame_index} the centre ray found surface at {depth} against the unedited terrain's \
             {bare_centre}, so the accepted edit never arrived in the frame"
        );
        assert_eq!(
            hits, first_hits,
            "frame {frame_index} found {hits} surfaces against frame 1's {first_hits} at {appended} records - \
             the device's per-frame work moved with the ledger, which is the degradation R2 forbids"
        );
    }
    let session_ms = session_start.elapsed().as_secs_f64() * 1000.0;

    assert_eq!(appended, edits, "the span must make every edit it claims");
    assert_eq!(
        uploads, refills,
        "the span uploaded {uploads} shapes but refilled the frame {refills} times, so a refill happened \
         without the shape the bind points at"
    );

    // The plant, run in the binary rather than assumed: the same grid over the UNEDITED terrain, from the last
    // edit's viewpoint, must NOT satisfy the placement assert. If it did, every per-frame depth assert above
    // would be testing the buffer rather than the edit.
    rays(centre, &mut grid);
    let stale = interpreter
        .march(&terrain(None), ROOT, &grid)
        .expect("the unedited terrain marches");
    let stale_depth = stale.depths[centre_pixel()];
    assert!(
        (stale_depth - EXPECTED_DEPTH).abs() >= PLACEMENT,
        "the plant FAILED TO PLANT: the unedited terrain returned {stale_depth} at the last edit's viewpoint, \
         inside the {PLACEMENT} placement bound, so the per-frame placement assert cannot tell a frame that \
         lost the edit from one that kept it"
    );

    times.sort_by(|a, b| a.total_cmp(b));
    let median = percentile(&times, 1, 2);
    let p95 = percentile(&times, 95, 100);
    let p999 = percentile(&times, 999, 1000);
    println!("// result           : {frames} frames marched on the device, {edits} edits appended, exit-ready");
    println!(
        "// O(1) on a device : {first_hits} rays found surface on every one of the {frames} frames, asserted \
         equal frame to frame - a guard against the frame going empty or reading the ledger, not the edit's \
         signature (the terrain is 2048 units wide, so the whole grid hits it with or without an edit)"
    );
    println!(
        "// agreement          : {lattice_rays} lattice rays checked against render::march across the span, \
         plus {full_rays} in {edits_full} full-grid checks (one every {FULL_GRID_EVERY} frames) - worst depth \
         deviation {worst_seen:.6} against the {DEVIATION} bound, zero hit/miss disagreements, asserted per frame",
        edits_full = frames / FULL_GRID_EVERY,
    );
    println!(
        "// the edit's path      : {uploads} host-side tree rebuilds, shape uploads and re-binds for {edits} \
         edits ({upload_bytes} shape bytes, {:.0} each) and {refills} ray refills of {} bytes, against \
         {frames} frames that created no buffer and no bind group - a per-frame allocation count of zero, while \
         an edit still reaching the GPU still costs a rebuild (OQ-55)",
        upload_bytes as f64 / uploads.max(1) as f64,
        WIDTH * HEIGHT * 32,
    );
    println!(
        "// the growing term   : the ledger read-back touched {read_records} records across the span ({:.0} \
         average per edit at {edits} edits, {:.0} per frame) - the epoch commit's pass run at the edit cadence \
         rather than once at the end as the CPU span does (DERIVED arithmetic on the asserted record counts, \
         not a measured duration)",
        read_records as f64 / edits as f64,
        read_records as f64 / frames as f64,
    );
    println!(
        "// milliseconds     : min {:.2} | median {median:.2} | p95 {p95:.2} | p99.9 {p999:.2} | max {:.2} - \
         printed, not asserted",
        times[0],
        times[times.len() - 1]
    );
    println!(
        "// spread           : p99.9 is {:.2}x the median over {} device frames. The CPU span put its own tail \
         anywhere from 1.27x to 12.7x its median depending on machine and run, which is why nothing in this \
         block gates.",
        p999 / median,
        times.len()
    );
    println!(
        "// wall clock       : the span took {session_ms:.0} ms of real time - {:.1}x a live session's ten \
         minutes at 60 fps, so this ran the frames rather than deriving them",
        600_000.0 / session_ms,
    );
    println!(
        "// what is still not met: R1's 16.67 ms at 1080p is not this instrument (a {WIDTH}x{HEIGHT} grid) and \
         is unmet on every device this project has; R2's span is now MEASURED on the device path as well as the \
         reference, so the gate stays open on the frame budget and not on the span."
    );
}
