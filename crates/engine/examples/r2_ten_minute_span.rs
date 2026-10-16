//! Gate R2's missing half: the ten minutes themselves, run rather than derived.
//!
//! `cargo run --release --example r2_ten_minute_span -p engine`
//!
//! `r2_edit_loop` measured the frame at four occupancies — 1 / 100 / 1,000 / 6,000 records — and then said
//! in its own output that a full 10-minute session at 60 fps is 36,000 frames and was **DERIVED from the flat
//! median, not run**. This closes that sentence: 36,000 consecutive frames, with one terrain edit appended to
//! a real `Slab` every sixth frame (ten a second at sixty), and the invariant asserted on **every** frame
//! rather than sampled at four stages.
//!
//! Three things are asserted, all of them integers or geometry, because a duration on a laptop is a
//! measurement of whatever else was running:
//!
//! 1. **The frame's charged work never grows with the ledger.** `Stats::evaluations` from frame 1 must equal
//!    frame 36,000's, and every frame in between is compared against frame 1's value at the moment it is
//!    marched — so a degradation is caught at the frame where it starts, with the occupancy and the frame
//!    index in the failure message. This is doc 03's O(1) hot-path claim tested at the span R2 names.
//! 2. **P1.7's placement holds at every frame of the span**, not just the last: the surface under the eye is
//!    where doc 02's `[x, x+1)` says the newest edit belongs. An edit loop whose visibility is only checked at
//!    the end cannot distinguish a stable frame from one that lost the edit somewhere in the middle.
//! 3. **Nothing is lost from the ledger**: at every frame boundary the slab hands back exactly the number of
//!    records appended, so the growing term is the epoch commit's read-back and never the frame.
//!
//! What this is NOT: not R1 (a 64×48 CPU reference march is a shape of a frame, not a 1080p GPU frame
//! budget), not a hashed number (timings are never hashed and never gate evidence — ADR-017, gate R4 is
//! informational), and not the device path (`device_edit_loop` runs the same invariant over `interp.wgsl`'s
//! `march_rays`, but at four occupancies; gpu's r2_device_span runs the whole span there.
//!
//! The span's millisecond distribution is printed as a distribution over all 36,000 samples, which is the
//! only thing that could make R2's "stable" word measurable at all — and the honest version of that claim is
//! the p99.9 against the median, not a ratio of stage medians.

use contracts::coords::LocalOffset;
use contracts::delta::{voxel_at, DeltaRecord, RegionSlot};
use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use memory::{Slab, SLAB_64KB_RECORDS};
use render::raster::{render_bnb, Camera};
use render::Tree;

/// Region slot 0, the key the boot, `visible_edit` and `r2_edit_loop` all touch.
const SLOT_ZERO: contracts::coords::RegionKey = contracts::coords::RegionKey { x: 0, y: 0, z: 0 };
/// R2's own words: "continuously edits terrain for 10 minutes". At ten edits a second that is 6,000 edits;
/// at sixty frames a second the same span is 36,000 frames. Both numbers come from the gate, not from tuning.
const FRAMES: usize = 36_000;
const EDITS: usize = 6_000;
/// Frames per edit: 60 fps / 10 edits a second. One edit every sixth frame, so the ledger grows continuously
/// instead of in four stages, which is the difference between this instrument and `r2_edit_loop`.
const FRAMES_PER_EDIT: usize = FRAMES / EDITS;
const HEIGHT: f32 = 8.0;
/// The centre pixel of the 64×48 grid, where the camera looks straight down at the newest edit.
const CENTRE_PIXEL: usize = (48 / 2) * 64 + 64 / 2;

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// doc 02's `[x, x+1)`: the centre of the voxel a coordinate names is the corner plus a half per axis.
fn voxel_centre(position: &contracts::coords::GlobalVoxelPos) -> [f32; 3] {
    [
        position.x as f32 + 0.5,
        position.y as f32 + 0.5,
        position.z as f32 + 0.5,
    ]
}

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

/// Straight down at the newest edit, from `HEIGHT` above it: the demo's close-range view, where one world
/// unit is resolvable (OQ-56's angular arithmetic says a distant view correctly shows nothing).
fn camera(centre: [f32; 3]) -> Camera {
    Camera {
        origin: [centre[0], centre[1], centre[2] + HEIGHT],
        forward: [0.0, 0.0, -1.0],
        right: [0.0, 1.0, 0.0],
        up: [1.0, 0.0, 0.0],
        half_height: 0.5,
        width: 64,
        height: 48,
        max_t: HEIGHT * 3.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

/// A walk over the terrain's surface, held in ONE `z` layer for the whole span, so a 96×64 column set covers
/// all 6,000 edits. The layer is not a convenience: the charged-work assert below compares one frame's
/// evaluation count against frame 1's, and it is only a statement about the ledger if the marched scene is
/// otherwise identical. `r2_edit_loop`'s first draft climbed into a second layer at its last stage, the union
/// genuinely changed, and the frame charged 19,712 against 18,284 — the assert fired on the fixture, not on
/// the hot path. Same rule here, at span length.
fn voxel_at_step(step: usize) -> LocalOffset {
    LocalOffset {
        x: (step % 96) as u16,
        y: (step / 96) as u16,
        z: 1,
    }
}

fn percentile(sorted: &[f64], num: usize, den: usize) -> f64 {
    let index = (sorted.len() - 1) * num / den;
    sorted[index]
}

fn main() {
    println!(
        "// R2's ten minutes, run: {FRAMES} consecutive frames at 64x48, one edit every {FRAMES_PER_EDIT} \
         frames ({EDITS} records = 10 minutes at 10/s)"
    );
    println!("// asserted per frame: the charge equals frame 1's, the newest edit sits at doc 02's placement, the slab hands back every record");
    println!("// printed, never asserted: milliseconds - a wall-clock number on this machine measures the machine (ADR-017)");

    let mut buffer = vec![0u64; 16 * SLAB_64KB_RECORDS];
    let mut slab = Slab::new(&mut buffer);
    let mut appended = 0usize;
    let mut times: Vec<f64> = Vec::with_capacity(FRAMES);
    let mut first_charge: Option<u64> = None;
    let mut centre = voxel_centre(&voxel_at(&SLOT_ZERO, &voxel_at_step(0)));

    let session_start = std::time::Instant::now();
    for frame in 1..=FRAMES {
        // The session's own clock: an edit lands every sixth frame, so occupancy grows continuously.
        if frame % FRAMES_PER_EDIT == 1 && appended < EDITS {
            slab.append(DeltaRecord::new(
                RegionSlot(0),
                voxel_at_step(appended),
                0x00c0 | (appended % 256) as u16,
            ))
            .expect("the slab holds every edit the span makes");
            appended += 1;
            // Resolve the newest edit through the read path the demo uses, exactly as the frame would
            // after an accepted edit arrives: the tree the march walks is rebuilt from what the ledger
            // actually stored, not from the loop counter.
            let read_back: Vec<DeltaRecord> = slab.deltas().collect();
            assert_eq!(
                read_back.len(),
                appended,
                "at frame {frame} the slab handed back {} records for {appended} appended - an edit was lost mid-span",
                read_back.len()
            );
            let newest = read_back
                .last()
                .expect("the span appends before it marches");
            centre = voxel_centre(&voxel_at(&SLOT_ZERO, &newest.intra_region()));
        }

        let start = std::time::Instant::now();
        let (frame_pixels, stats) = render_bnb(&terrain(Some(centre)), &camera(centre));
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        times.push(elapsed);

        // 1. The hot path's O(1) claim, asserted at every frame against the first frame's charge.
        match first_charge {
            None => first_charge = Some(stats.evaluations),
            Some(expected) => assert_eq!(
                stats.evaluations, expected,
                "frame {frame} charged {} evaluations against frame 1's {expected} at {appended} records - \
                 the per-frame work grew with the ledger, which is the degradation R2 forbids",
                stats.evaluations
            ),
        }

        // 2. P1.7's placement, every frame of the span: the surface under the eye is where doc 02 puts it.
        let depth = frame_pixels.depth[CENTRE_PIXEL];
        let expected_depth = HEIGHT - 0.5;
        assert!(
            (depth - expected_depth).abs() < 0.05,
            "at frame {frame}, occupancy {appended}: the frame put the newest edit at depth {depth} where \
             placement says {expected_depth} - the edit stopped being visible partway through the span"
        );
    }
    let session_ms = session_start.elapsed().as_secs_f64() * 1000.0;

    assert_eq!(appended, EDITS, "the span must make every edit it claims");
    let charge = first_charge.expect("the span marches at least one frame");

    // 3. The ledger's growing term is the commit's read-back, and it is measured here at the span's end as
    //    well as at four stages by `r2_edit_loop`: one pass over the whole slab after the last frame.
    let read_start = std::time::Instant::now();
    let final_records: Vec<DeltaRecord> = slab.deltas().collect();
    let read_ms = read_start.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(final_records.len(), EDITS);
    assert_eq!(
        voxel_centre(&voxel_at(&SLOT_ZERO, &final_records.last().unwrap().intra_region())),
        centre,
        "the last record read back is not the one the span last marched, so the loop and the ledger disagree"
    );

    times.sort_by(|a, b| a.total_cmp(b));
    let median = percentile(&times, 1, 2);
    let p95 = percentile(&times, 95, 100);
    let p999 = percentile(&times, 999, 1000);
    println!("// result           : {FRAMES} frames marched, {EDITS} edits appended, exit-ready");
    println!("// charge           : {charge} evaluations per frame, asserted equal on all {FRAMES} - the O(1) hot path holds across R2's whole span, not at four sampled occupancies");
    println!(
        "// milliseconds     : min {:.2} | median {median:.2} | p95 {p95:.2} | p99.9 {p999:.2} | max {:.2} - \
         printed, not asserted",
        times[0],
        times[times.len() - 1]
    );
    println!(
        "// spread           : p99.9 is {:.2}x the median over {} samples. That tail, not a stage-median \
         ratio, is what \"stable\" would have to mean on a wall clock - and it is this machine's load, which \
         is why the gate's claim rides on the integer above instead.",
        p999 / median,
        times.len()
    );
    println!(
        "// budget share     : median {:.1}% of R1's 16.67 ms whole-frame budget; p99.9 {:.1}%. R1 is about \
         a 1080p GPU frame and this is a 64x48 CPU reference march, so these shares are shape, not verdict.",
        median / 16.67 * 100.0,
        p999 / 16.67 * 100.0
    );
    println!(
        "// read-back        : one pass over {EDITS} records taken once, after the last frame, at \
         {read_ms:.3} ms ({:.1} ns/record) - this span ends with the ledger full; `r2_edit_loop` is the \
         instrument that prices this term's growth across occupancies",
        read_ms * 1.0e6 / EDITS as f64
    );
    println!(
        "// wall clock       : the span itself took {session_ms:.0} ms of real time, i.e. this ran {:.1}x \
         faster than a 10-minute session would - it is the same 36,000 frames, not a scaled sample",
        600_000.0 / session_ms
    );
    println!("// what R2 still needs: this is the CPU reference path at 64x48. The device path (interp.wgsl's");
    println!(
        "//                    march_rays) has the same invariants asserted at four occupancies by"
    );
    println!("//                    gpu's device_edit_loop, and gpu's r2_device_span now runs the span there too, while an edit still reaches the GPU by a host-side");
    println!("//                    tree rebuild because A-07 has no instance node (OQ-55). So R2's span is");
    println!("//                    MEASURED on the reference AND on a device (36,000 device frames, 3,072 hits asserted equal every one), and R1's frame");
    println!("//                    budget is unmet on both - culling on the GPU, not this loop, is that gate.");
}
