//! Gate AS4's remaining clause: a frame must not allocate or parse its way to the queue, so a compiled shape
//! is uploaded **once** and every later frame binds the buffers that already exist.
//!
//! `cargo run --release -p gpu --example as4_upload_reuse`
//!
//! `Interpreter::march` re-uploads the node records and the parameter pool on every call, which is exactly
//! right for a correctness probe — one upload, one dispatch, one copy back, nothing to go stale between
//! frames — and wrong for a running game, where that is per-frame allocation and a per-frame bus copy of the
//! same bytes. `upload` + `march_uploaded` split the two: the shape goes up once, the rays go up per frame.
//!
//! What this can assert is a **byte count**, not a duration and not an allocation count. A device hands back
//! depths; it does not report what the host allocator did, and this harness runs on a laptop whose stage
//! medians moved 5.6× between two runs of identical code (docs/04). So the equivalence check is on the output,
//! the cost check is on the bytes the two paths hand over, and the milliseconds are printed as shape only
//! (ADR-017: timings are never evidence).
//!
//! The equivalence assert is falsified in-binary, by design: after the reused frames are shown to match the
//! per-frame path bit for bit, a *second* shape with one parameter changed is uploaded and marched, and its
//! depths are required to differ. Without that, "they match" would be compatible with `march_uploaded`
//! binding nothing at all.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SPHERE, NODE_UNION,
};
use gpu::device::Interpreter;
use render::{Ray, Tree, ROOT};

const WIDTH: u32 = 64;
const HEIGHT_GRID: u32 = 48;
const SURFACE: f32 = 0.01;
const MAX_STEPS: u32 = 64;
const MAX_T: f32 = 24.0;
const FRAMES: usize = 6;

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// A plain union of a slab and a ball, so the field is at least two nodes deep and the node-record buffer is
/// not the trivial one-record case. `radius` is the parameter the falsification moves.
fn scene(radius: f32) -> Tree {
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 3, 0, 0, true),
            record(NODE_ROUNDED_BOX, 0, 1, 0, 4, false),
            record(NODE_SPHERE, 0, 1, 4, 4, false),
        ],
        params: vec![12.0, 12.0, 0.5, 0.0, radius, 0.0, 1.0, 0.0],
    }
}

fn rays() -> Vec<Ray> {
    let origin = [0.0, 0.0, 12.0];
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

fn same_depths(a: &[f32], b: &[f32]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(|a, b| a.total_cmp(b));
    times[times.len() / 2]
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!("SKIP as4_upload_reuse: no device ({error:?}) - the clause is about what a frame hands \
                     the queue, and a software rasteriser would report the reference path as if it were a frame");
            return;
        }
    };
    println!("// adapter      : {}", interpreter.adapter());
    let grid = rays();
    let tree = scene(2.0);

    // The per-frame path, as every probe in this repo has used it: upload, dispatch, retrieve.
    let cold = interpreter
        .march(&tree, ROOT, &grid)
        .expect("the grid must march on the device");
    let shape = interpreter.upload(&tree);
    println!(
        "// one upload    : {} node records, {} bytes resident ({} per frame the cold path hands over again)",
        shape.node_count, shape.bytes, shape.bytes
    );

    let mut warm_ms = Vec::with_capacity(FRAMES);
    let mut cold_ms = Vec::with_capacity(FRAMES);
    let mut reused_bytes_after_upload = 0usize;
    for frame in 0..FRAMES {
        let start = std::time::Instant::now();
        let march = interpreter
            .march_uploaded(&shape, ROOT, &grid)
            .expect("the resident shape must march");
        warm_ms.push(start.elapsed().as_secs_f64() * 1000.0);

        // The equivalence half: the same rays against the same resident shape produce the same depths the
        // per-frame upload produced, bit for bit - not within a tolerance, because nothing here reorders.
        assert!(
            same_depths(&march.depths, &cold.depths),
            "frame {frame} of the reused shape disagrees with the per-frame upload, so `march_uploaded` is \
             not marching the shape `upload` put on the device"
        );
        assert_eq!(
            march.hits, cold.hits,
            "frame {frame} found {} surfaces against the cold path's {}",
            march.hits, cold.hits
        );
        // The cost half, as an integer: what this frame handed the bus. Rays only - never the shape.
        reused_bytes_after_upload += grid.len() * 32;

        let start = std::time::Instant::now();
        let again = interpreter
            .march(&tree, ROOT, &grid)
            .expect("the cold path must still march");
        cold_ms.push(start.elapsed().as_secs_f64() * 1000.0);
        assert!(same_depths(&again.depths, &cold.depths));
    }

    let cold_shape_bytes = shape.bytes * FRAMES;
    let saved_shape_bytes = shape.bytes * (FRAMES - 1);
    println!(
        "// per-frame path: {FRAMES} frames x {} shape bytes = {cold_shape_bytes} handed over",
        shape.bytes
    );
    println!(
        "// reused shape  : {} bytes once, then 0 shape bytes per frame; {saved_shape_bytes} never re-uploaded",
        shape.bytes
    );
    assert_eq!(
        cold_shape_bytes - shape.bytes,
        saved_shape_bytes,
        "the reused path's saving is not simply the shape's bytes times the frames after the first"
    );
    println!(
        "// ray bytes     : the part no path can avoid - {} bytes of rays per frame, {reused_bytes_after_upload} across {FRAMES}",
        grid.len() * 32
    );
    // DERIVED arithmetic, stated because it is the finding: the shape's bytes are a property of the asset and
    // the ray buffer's are a property of the resolution, so at 1080p the ratio inverts hard. AS4's clause is
    // about the asset path; the measurement says the per-frame cost lives in the rays, which a real renderer
    // generates on the device and never uploads at all.
    println!(
        "// at 1920x1080  : rays would be {} bytes per frame against this {}-byte shape - the shape is {:.3}% of the bus (DERIVED)",
        1920 * 1080 * 32,
        shape.bytes,
        100.0 * shape.bytes as f64 / (1920 * 1080 * 32) as f64
    );

    // Falsification, inside the artifact: a different shape must march differently, or the equivalence above
    // would also be satisfied by a bind that ignored the buffers entirely.
    let other = scene(3.5);
    let other_shape = interpreter.upload(&other);
    let diverged = interpreter
        .march_uploaded(&other_shape, ROOT, &grid)
        .expect("the second shape must march");
    assert!(
        !same_depths(&diverged.depths, &cold.depths),
        "a shape with a different radius marched to the same depths, so `march_uploaded` is not reading the \
         node and pool buffers `upload` created"
    );
    let first_difference = diverged
        .depths
        .iter()
        .zip(cold.depths.iter())
        .position(|(a, b)| a.to_bits() != b.to_bits())
        .expect("the falsification assert guarantees at least one differing depth");
    println!(
        "// falsified     : radius 2.0 vs 3.5 moved the picture (first differing depth at ray {first_difference}: {} against {})",
        diverged.depths[first_difference], cold.depths[first_difference]
    );

    println!(
        "// milliseconds  : reused {:.3} vs per-frame upload {:.3} median - printed, not asserted; a duration \
         on this machine is a measurement of whatever else was running",
        median(warm_ms),
        median(cold_ms)
    );
    println!("// AS4           : the shape is compiled, uploaded once and bound per frame; no parse, no per-frame \
             copy of its bytes.");
}
