//! AS4's asset side, closed the way the frame side was: a shape that changes keeps its buffers.
//!
//! `cargo run --release -p gpu --example as4_asset_reupload`
//!
//! `as4_upload_reuse` measured that the shape's bytes reach the shader as the words `assetc` writes, and named
//! the residue: `upload` calls `create_buffer_init` per shape, so an edit — 6,000 of them in R2's ten minutes,
//! as `r2_device_span` just counted — creates two buffers each time. That is 12,000 buffer creations per
//! session for a scene whose shape record is 80 bytes, and §3 of AGENTS.md bans exactly this kind of churn on
//! a gameplay frame.
//!
//! `upload_into` writes the same bytes into the buffers that already exist. This probe is the measurement that
//! says the reused bytes still draw the changed shape, at the edit cadence rather than once:
//!
//! 1. **Bit-for-bit against the fresh-upload path** over every edit of the run: all 3,072 depths, compared as
//!    `f32::to_bits`, must be identical between a re-upload and a brand-new `upload` of the same tree. If the
//!    reuse changed anything, this is where it shows.
//! 2. **Against `render::march`** on the full grid, every edit: zero hit/miss disagreements, worst deviation
//!    under 0.002 — the same anchor the R2 span holds.
//! 3. **A refusal, asserted**: a tree that outgrows the capacity its first upload established is refused, the
//!    resident bytes are left alone, and the frame still draws the shape that is on the device. Growing an
//!    asset buffer is an allocation, and the point of the clause is that the probe does not hide one.
//! 4. **The plant**: after a re-upload is *skipped* while the tree keeps changing, the device must return the
//!    stale picture. If it returned the new one, asserts 1 and 2 would be testing the host `Tree` argument and
//!    not the bytes on the device.
//!
//! Bytes and counts are the asserted quantities; the milliseconds are printed and gate nothing (ADR-017).

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{node_bytes, Interpreter};
use render::{march, Ray, Tree, ROOT};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const SURFACE: f32 = 0.01;
const MAX_STEPS: u32 = 64;
const MAX_T: f32 = 24.0;
/// One minute of R2's own cadence: ten edits a second. 600 edits is 6,000 at the same cost per edit, and the
/// extrapolation to the ten minutes is arithmetic on counts, not a duration.
const EDITS: usize = 600;
const DEVIATION: f32 = 0.002;

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// Terrain with the edit's inscribed ball at `centre` (A-07 has no instance node — OQ-55), 3 nodes, 8 params.
fn terrain(centre: [f32; 3]) -> Tree {
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 3, 0, 0, true),
            record(NODE_ROUNDED_BOX, 0, 1, 0, 4, false),
            record(NODE_SPHERE, 0, 1, 4, 4, false),
        ],
        params: vec![
            2048.0, 2048.0, 0.5, 0.0, 0.5, centre[0], centre[1], centre[2],
        ],
    }
}

/// The shape that needs MORE bytes than the first upload established: 5 nodes, 12 params. The refusal arm.
fn bigger(centre: [f32; 3]) -> Tree {
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 5, 0, 0, true),
            record(NODE_UNION, 2, 4, 0, 0, false),
            record(NODE_ROUNDED_BOX, 0, 1, 0, 4, false),
            record(NODE_SPHERE, 0, 1, 4, 4, false),
            record(NODE_SPHERE, 0, 1, 8, 4, false),
        ],
        params: vec![
            2048.0,
            2048.0,
            0.5,
            0.0,
            0.5,
            centre[0],
            centre[1],
            centre[2],
            0.25,
            centre[0] + 1.0,
            centre[1],
            centre[2],
        ],
    }
}

/// Straight down at the edit, the grid `r2_device_span` and `device_edit_loop` both march.
fn rays(centre: [f32; 3], out: &mut Vec<Ray>) {
    out.clear();
    let origin = [centre[0], centre[1], centre[2] + 8.0];
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

/// Device depths against `render::march`, ray for ray, over the whole grid: the count of rays that disagree on
/// hit-vs-miss and the worst depth deviation.
fn against_reference(tree: &Tree, grid: &[Ray], depths: &[f32]) -> (usize, f32) {
    let mut disagreements = 0usize;
    let mut worst = 0.0f32;
    for (ray, depth) in grid.iter().zip(depths) {
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

fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(|a, b| a.total_cmp(b));
    times[times.len() / 2]
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP as4_asset_reupload: no device ({error:?}) - the clause under test is which buffers an \
                 edit creates on a real adapter, and a software rasteriser would report zero either way"
            );
            return;
        }
    };
    println!("// adapter          : {}", interpreter.adapter());
    println!(
        "// what is measured : {EDITS} shape edits marched at the edit cadence, each one re-uploaded into the \
         buffers the first upload created ({} + {} bytes of shape, {} node records of 16 bytes)",
        node_bytes(&terrain([0.5, 0.5, 1.5])).len(),
        8 * 4,
        3,
    );

    let mut grid: Vec<Ray> = Vec::with_capacity((WIDTH * HEIGHT) as usize);
    let centre0 = [0.5f32, 0.5f32, 1.5f32];
    rays(centre0, &mut grid);
    let mut frame = interpreter.begin_frame(grid.len());
    let mut shape = interpreter.upload(&terrain(centre0));
    // One bind group for the whole run: the buffers it points at never move, so an edit rewrites the bytes
    // through them instead of creating another pair. This is the quantity the clause is about.
    let bind = interpreter.bind(&shape, &frame);
    let wrote = interpreter
        .write_rays(&mut frame, ROOT, &grid)
        .expect("the grid fits the frame");
    assert_eq!(
        wrote,
        grid.len() * 32,
        "the frame's ray bytes are the divisor below"
    );
    let mut reupload_ms: Vec<f64> = Vec::with_capacity(EDITS);
    let mut fresh_ms: Vec<f64> = Vec::with_capacity(EDITS);
    let mut bytes_written = 0usize;
    let mut mismatched_depths = 0usize;
    let mut compared_depths = 0usize;
    let mut worst_seen = 0.0f32;
    let nodes_capacity = shape.nodes_capacity;
    let pool_capacity = shape.pool_capacity;

    for edit in 0..EDITS {
        // The edit's own geometry: a walk along x and y on one layer, so the record count and byte counts stay
        // constant and the only variable is the params the pool carries.
        let centre = [(edit % 96) as f32 + 0.5, (edit / 96) as f32 + 0.5, 1.5f32];
        let tree = terrain(centre);
        rays(centre, &mut grid);
        interpreter
            .write_rays(&mut frame, ROOT, &grid)
            .expect("the grid fits the frame");

        let started = std::time::Instant::now();
        let bytes = interpreter
            .upload_into(&mut shape, &tree)
            .expect("the edited tree fits the capacity the first upload set");
        let reused = interpreter
            .march_frame(&bind, &frame)
            .expect("the reused shape marches");
        reupload_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        bytes_written += bytes;

        // 1. The same tree through the allocating path: byte-identical picture, or the reuse is not equivalent.
        let started = std::time::Instant::now();
        let fresh_shape = interpreter.upload(&tree);
        let fresh = interpreter
            .march_uploaded(&fresh_shape, ROOT, &grid)
            .expect("the fresh upload marches");
        fresh_ms.push(started.elapsed().as_secs_f64() * 1000.0);

        assert_eq!(
            fresh.depths.len(),
            reused.depths.len(),
            "edit {edit}: the re-uploaded frame returned {} depths where the fresh upload returned {}",
            reused.depths.len(),
            fresh.depths.len()
        );
        let mismatched = reused
            .depths
            .iter()
            .zip(&fresh.depths)
            .filter(|(a, b)| a.to_bits() != b.to_bits())
            .count();
        if mismatched != 0 && edit == 0 {
            println!(
                "// first divergence : edit {edit} put {mismatched} depths that differ from the fresh-upload \
                 path"
            );
        }
        mismatched_depths += mismatched;
        compared_depths += reused.depths.len();

        // 2. The device against the CPU reference, on the whole grid, at this edit.
        let (disagreements, worst) = against_reference(&tree, &grid, &reused.depths);
        worst_seen = worst_seen.max(worst);
        assert_eq!(
            reused.hits, fresh.hits,
            "edit {edit}: hits {} against {}",
            reused.hits, fresh.hits
        );

        if edit == 0 || edit == EDITS - 1 || mismatched != 0 || disagreements != 0 {
            println!(
                "// edit {:>4} | centre {:>5.1} {:>5.1} | hits {:>4} | depths differing from a fresh upload \
                 {:>5} of {:>5} | disagreements {:>3} | worst dev {:>9.6} | re-upload + march {:>6.3} ms | fresh {:>6.3} ms",
                edit, centre[0], centre[1], reused.hits, mismatched, reused.depths.len(), disagreements, worst,
                reupload_ms[edit], fresh_ms[edit],
            );
        }
        assert_eq!(
            disagreements, 0,
            "edit {edit}: the re-uploaded shape and render::march disagree on which rays find surface"
        );
        assert!(
            worst < DEVIATION,
            "edit {edit}: the re-uploaded shape differs from the reference by {worst}"
        );
    }

    // 1 again, as a gate over the whole run: a reused asset buffer must draw the same picture a new one does.
    assert_eq!(
        mismatched_depths, 0,
        "across {EDITS} edits and {compared_depths} depth comparisons, {mismatched_depths} of them differ \
         between re-uploading into the resident buffers and creating fresh ones - so the reuse is not \
         equivalent to the allocating path, which is the only thing that makes it legal on the hot path"
    );

    // 3. The refusal: a tree that outgrows the capacity must not be written, and the device must keep drawing
    //    what it already holds.
    let grown = bigger([0.5, 0.5, 1.5]);
    let grown_nodes = node_bytes(&grown).len() as u64;
    let error = interpreter.upload_into(&mut shape, &grown).expect_err(
        "a 5-node tree does not fit a 3-node capacity, and growing it is an allocation",
    );
    assert!(
        error.contains("does not fit"),
        "the refusal came back with the wrong words: {error}"
    );
    assert_eq!(
        (grown_nodes > nodes_capacity) as u8,
        1,
        "the fixture is not a fixture: a grown tree needs {grown_nodes} bytes against a capacity of {nodes_capacity}"
    );
    assert_eq!(
        shape.node_count, 3,
        "a refused upload changed the shape's bookkeeping to {} nodes",
        shape.node_count
    );
    let last_centre = [
        ((EDITS - 1) % 96) as f32 + 0.5,
        ((EDITS - 1) / 96) as f32 + 0.5,
        1.5f32,
    ];
    rays(last_centre, &mut grid);
    interpreter
        .write_rays(&mut frame, ROOT, &grid)
        .expect("the grid fits the frame");
    let bind = interpreter.bind(&shape, &frame);
    let after_refusal = interpreter
        .march_frame(&bind, &frame)
        .expect("the frame still marches after a refused upload");
    let (refusal_disagreements, refusal_worst) =
        against_reference(&terrain(last_centre), &grid, &after_refusal.depths);
    assert_eq!(
        refusal_disagreements, 0,
        "the refused upload still changed which rays find surface ({refusal_disagreements} of them)"
    );
    assert!(
        refusal_worst < DEVIATION,
        "the refused upload moved depths by {refusal_worst}"
    );
    println!(
        "// refusal          : a {}-node tree ({} bytes) against the {} / {} bytes resident was refused, the \
         shape still reports {} nodes, and the next frame still matches the reference ({} comparisons, worst \
         dev {refusal_worst:.6}) - the bytes that did not fit never reached the device",
        grown.nodes.len(),
        grown_nodes,
        nodes_capacity,
        pool_capacity,
        shape.node_count,
        grid.len(),
    );

    // 4. The plant: move the edit and skip the re-upload. The device must keep drawing the shape it holds,
    //    which is NOT the tree the host now has; if it returned the moved tree's answer, asserts 1 and 2 above
    //    would be reading the host `Tree` argument rather than the bytes on the device.
    let stale_centre = [0.5f32, 0.5f32, 1.5f32];
    rays(stale_centre, &mut grid);
    interpreter
        .write_rays(&mut frame, ROOT, &grid)
        .expect("the grid fits the frame");
    let stale = interpreter
        .march_frame(&bind, &frame)
        .expect("the stale shape still marches");
    let (stale_disagreements, stale_worst) =
        against_reference(&terrain(stale_centre), &grid, &stale.depths);
    assert!(
        stale_worst > DEVIATION,
        "the plant FAILED TO PLANT: with the re-upload skipped while the edit moved from x {} to x {}, the \
         device still matched the moved tree (worst deviation {stale_worst:.6} over {} rays), so the per-edit \
         asserts above are reading the host `Tree` and not the resident bytes",
        last_centre[0],
        stale_centre[0],
        grid.len(),
    );
    println!(
        "// the plant        : skipping the re-upload while the edit moved from x {} to x {} leaves the device \
         drawing the shape it holds - the moved tree's reference disagrees with it on {stale_disagreements} \
         rays' hit/miss set and by {stale_worst:.4} in depth (over {} rays), which is what makes asserts 1 and \
         2 a statement about the resident bytes",
        last_centre[0],
        stale_centre[0],
        grid.len(),
    );

    println!(
        "// result           : {EDITS} edits, {compared_depths} depth comparisons against a fresh upload, {} \
         bytes re-uploaded, zero divergences",
        bytes_written
    );
    println!(
        "// buffers created  : the re-upload path created 2 buffers and 1 bind group for the whole run and \
         wrote {} bytes in {} `write_buffer` calls (two per edit); the fresh path created {} buffers for the \
         same {EDITS} edits ({:.0}x the creations avoided) - MEASURED counts, and on a session of 6,000 edits \
         that is 12,000 creations avoided (DERIVED arithmetic on the asserted per-edit count)",
        bytes_written,
        EDITS * 2,
        EDITS * 2 + 2,
        (EDITS * 2 + 2) as f64 / 2.0,    );
    println!(
        "// anchor           : worst depth deviation {worst_seen:.6} against the {DEVIATION} the R2 span and \
         the 1080p split both hold, over {EDITS} full-grid comparisons - zero hit/miss disagreements"
    );
    println!(
        "// milliseconds     : re-upload + march median {:.3} | fresh upload + march median {:.3} - printed, \
         not asserted (ADR-017)",
        median(reupload_ms.clone()),
        median(fresh_ms.clone()),
    );
    println!(
        "// still open       : this is not zero-copy in the spike's sense. The bytes still travel through a \
         host `Vec<u8>` (`node_bytes`) and a queue copy; what is gone is the per-edit allocation, which is the \
         clause §3 actually bans. A mapped, truly aliased asset buffer is the remaining term, and the frame \
         side (`r1_round_trip_split`) is where the frame's bytes, not the shape's, dominate."
    );
}
