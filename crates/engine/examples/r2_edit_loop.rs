//! Gate R2's first number: does the frame stay stable while a player keeps editing terrain?
//!
//! `cargo run --release --example r2_edit_loop -p engine`
//!
//! R2 is docs/04's "frame time stable while a player continuously edits terrain for 10 minutes", tied to
//! doc 03's tiering, and until now it had no measurement behind it at all - only the statement that a CPU
//! step count is not a frame time. This loop produces the shape of the claim on the CPU reference march:
//! the ledger fills with edits in stages, and at every stage the same frame is rendered repeatedly and its
//! per-frame cost reported as a distribution rather than a single lucky number.
//!
//! Two things are measured separately because they scale in opposite directions. The **frame** is the hot
//! path: doc 03's whole design says a region's compiled shape is a fixed cost, so the charged work per frame
//! must be identical as slab occupancy grows — and that is asserted as an integer, `Stats::evaluations`,
//! which repeats on every machine. The **read-back** (`slab.deltas()`) is the term that does grow with
//! occupancy, and it is what an epoch commit pays — S2 measured that law and this re-measures the
//! proportional part of it rather than citing it. Reporting only the flat half would be the easy proof.
//!
//! The milliseconds are printed and NOT asserted, because this machine proved the point during the loop's own
//! development: two runs of identical code put the 6,000-record stage's median at 3.44 ms and at 8.69 ms and
//! its read-back at 0.046 ms and 1.108 ms. A wall-clock ratio on a laptop is a measurement of whatever else
//! was running, and an informational CI step that can redden trunk on machine load is a worse instrument
//! than no step at all.
//!
//! The visibility invariant from P1.7 is asserted inside the loop, not once at the end: whatever the slab
//! holds, the frame must still put the newest edit's surface where doc 02's span says it belongs. That is
//! what makes this an edit loop rather than a benchmark of a static scene.
//!
//! Timings are never hashed and never gate evidence (ADR-017, R4 informational). What is claimed below is
//! MEASURED on this machine at these occupancies; the 10-minute span is DERIVED from ten edits a second,
//! and that line says so in the same sentence.

use contracts::coords::LocalOffset;
use contracts::delta::{voxel_at, DeltaRecord, RegionSlot};
use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use memory::{Slab, SLAB_64KB_RECORDS};
use render::raster::{render_bnb, Camera};

/// Region slot 0, the same key the boot and `visible_edit` touch.
const SLOT_ZERO: contracts::coords::RegionKey = contracts::coords::RegionKey { x: 0, y: 0, z: 0 };
/// The edit rate R2's ten minutes assumes: ten a second, so 6,000 edits is the whole gate span.
const EDITS_PER_SECOND: usize = 10;
/// Marches per stage that enter the distribution.
const FRAMES_PER_STAGE: usize = 21;
/// Marches per stage discarded before those, and the reason they are discarded: the first frames of a stage
/// pay the page faults of freshly-written slab pages and the allocator warming its frame buffers, and on a
/// quiet run those show up as a 3 ms median beside a 30 ms max. The stability assertion compares stage
/// medians, so leaving warm-up in the *first* stage's median biases that assertion toward passing — it is
/// easiest to satisfy exactly when the machine is noisiest at the start. Discarding a fixed count is the
/// weakest correction available: it uses no information about the times themselves.
const WARM_UP_FRAMES: usize = 3;
const HEIGHT: f32 = 8.0;

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
/// unit is resolvable (OQ-56's angular arithmetic says a distant view would correctly show nothing).
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

/// A walk over the terrain's surface, so every edit is a distinct voxel and the slab really fills: 96x64
/// columns, which is more than the 6,000 edits this loop makes, so every edit lands in the SAME `z` layer.
/// That is not a convenience. The charged-work assertion below compares one frame's evaluation count across
/// occupancies, and it is only about the ledger if the marched scene is otherwise identical — when the walk
/// was 64x64 and climbed to `z = 2` at the last stage, the edited voxel sat a full unit higher above the
/// plane, the union genuinely changed, and the frame charged 19,712 evaluations against 18,284. The assert
/// caught it, which is the falsification: it fires when the scene changes, and therefore says nothing at all
/// unless the scene is held still.
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

fn distribution(mut times: Vec<f64>) -> [f64; 4] {
    times.sort_by(|a, b| a.total_cmp(b));
    [
        times[0],
        percentile(&times, 1, 2),
        percentile(&times, 95, 100),
        times[times.len() - 1],
    ]
}

use render::Tree;

fn main() {
    let stages = [1usize, 100, 1_000, 6_000];
    let mut buffer = vec![0u64; 16 * SLAB_64KB_RECORDS];
    let mut slab = Slab::new(&mut buffer);
    let mut appended = 0usize;
    let mut medians = Vec::with_capacity(stages.len());
    let mut stage_charges: Vec<(usize, u64)> = Vec::with_capacity(stages.len());

    println!(
        "// R2 edit loop: frame time as the ledger fills (CPU reference march, 64x48, 96 steps)"
    );
    println!(
        "// warm-up        : {WARM_UP_FRAMES} marches per stage are marched and asserted, but not timed"
    );
    println!("// occupancy | frames | evaluations/frame | ms/frame min | median | p95 | max   | deltas() read-back ms | ns/record");
    for &stage in &stages {
        while appended < stage {
            slab.append(DeltaRecord::new(
                RegionSlot(0),
                voxel_at_step(appended),
                0x00c0 | (appended % 256) as u16,
            ))
            .expect("the slab holds every edit this loop makes");
            appended += 1;
        }

        // The newest edit, resolved through the same read path the demo uses.
        let read_back: Vec<DeltaRecord> = slab.deltas().collect();
        assert_eq!(
            read_back.len(),
            appended,
            "the slab must hand back every edit it took"
        );
        let newest = read_back.last().expect("a stage has at least one edit");
        let centre = voxel_centre(&voxel_at(&SLOT_ZERO, &newest.intra_region()));

        let read_start = std::time::Instant::now();
        let again: Vec<DeltaRecord> = slab.deltas().collect();
        let read_ms = read_start.elapsed().as_secs_f64() * 1000.0;
        assert_eq!(again.len(), appended);

        let mut times = Vec::with_capacity(FRAMES_PER_STAGE);
        let mut charged = Vec::with_capacity(FRAMES_PER_STAGE);
        for frame_index in 0..(FRAMES_PER_STAGE + WARM_UP_FRAMES) {
            let start = std::time::Instant::now();
            let (frame, stats) = render_bnb(&terrain(Some(centre)), &camera(centre));
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if frame_index >= WARM_UP_FRAMES {
                times.push(elapsed);
                charged.push(stats.evaluations);
            }

            // P1.7's invariant, inside the loop: the surface under the eye is where doc 02's span puts it.
            let centre_pixel = ((48 / 2) * 64 + 64 / 2) as usize;
            let expected = HEIGHT - 0.5;
            let depth = frame.depth[centre_pixel];
            assert!(
                (depth - expected).abs() < 0.05,
                "at occupancy {appended} the frame put the newest edit at {depth} where placement says {expected}"
            );
        }
        let charged_this_stage: u64 = charged.iter().sum::<u64>() / charged.len() as u64;
        assert_eq!(
            charged.iter().min(),
            charged.iter().max(),
            "the same frame charged different work at different marches of one occupancy"
        );
        stage_charges.push((appended, charged_this_stage));
        let [min, median, p95, max] = distribution(times);
        medians.push(median);
        println!(
            "// {:>9} | {:>6} | {:>17} | {:>12.2} | {:>6.2} | {:>5.2} | {:>5.2} | {:>21.3} | {:>9.1}",
            appended,
            FRAMES_PER_STAGE,
            charged_this_stage,
            min,
            median,
            p95,
            max,
            read_ms,
            read_ms * 1.0e6 / appended as f64
        );
    }

    // The instrument that can gate is the integer column, not the millisecond one. Doc 03's claim is that a
    // frame walks a region's compiled shape rather than scanning what the player edited, so the charged work
    // per frame must be the same at 6,000 records as at 1 — and it is a count the march reports, so it
    // repeats on every machine. The medians are printed to show the shape of R2's claim, not to test it: two
    // runs of this identical code put the 6,000-record median at 3.44 ms and at 8.69 ms, and its read-back at
    // 0.046 ms and 1.108 ms, which is machine load arriving through the whole stage, not the ledger.
    //
    // The count was falsified by coupling the march to the ledger: growing the stop tolerance by one part in
    // 1e7 per record moved the charge from 18,284 to 18,280 and this assert fired. The first plant tried was
    // inert and exited 0 — raising `max_steps` from 96 to 189 changed nothing, because every ray converges
    // long before the cap, so the cap is not a term the frame pays. A checker is only as live as the
    // violation it can express.
    let first_charge = stage_charges[0].1;
    for (occupancy, charge) in &stage_charges {
        assert_eq!(
            *charge, first_charge,
            "the frame charged {charge} evaluations at {occupancy} records against {first_charge} at {} - the hot path grew with the ledger, which is the degradation R2 forbids",
            stage_charges[0].0
        );
    }
    println!(
        "// O(1) hot path    : {} evaluations marched per frame at every occupancy from {} to {} records, asserted equal",
        first_charge,
        stage_charges[0].0,
        stage_charges[stage_charges.len() - 1].0
    );

    let (slowest, fastest) = medians.iter().fold((0.0f64, f64::INFINITY), |acc, &m| {
        (acc.0.max(m), acc.1.min(m))
    });
    println!(
        "// medians          : {fastest:.2} ms to {slowest:.2} ms across the stages, {:.2}x apart - INFORMATIONAL, and the spread is this machine's, not the ledger's",
        slowest / fastest
    );
    println!(
        "// budget share     : each stage's median against R1's 16.67 ms whole-frame budget: {}",
        medians
            .iter()
            .map(|m| format!("{:.1}%", m / 16.67 * 100.0))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let minutes = stages[stages.len() - 1] as f64 / EDITS_PER_SECOND as f64 / 60.0;
    println!("// 10-minute span   : the loop ran to {} edits = {minutes:.1} minutes at {EDITS_PER_SECOND} edits/second.", stages[stages.len() - 1]);
    println!("//                    Frame cost is MEASURED at these occupancies; a full 10-minute session at");
    println!("//                    60 fps is 36,000 frames and is DERIVED from the flat median above, not run.");
    println!("// what R2 still needs: this is the CPU reference march. R1's on-device march (interp.wgsl's");
    println!("//                    march_rays) does not read the ledger yet - an edit reaches the frame here");
    println!("//                    by rebuilding the tree, and A-07 has no instance node (OQ-55) to do it in");
    println!("//                    the grammar. R2 is therefore MEASURED on the reference path and UNMET on a");
    println!("//                    device until the edit loop drives the GPU frame.");
}
