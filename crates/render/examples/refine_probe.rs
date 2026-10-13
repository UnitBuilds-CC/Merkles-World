//! OQ-59's diagnostic: watch a streamed frame being refined, one frame at a time.
//!
//! `cargo run --release --example refine_probe -p render`
//!
//! The register says the defect was localised by reasoning twice and observed never, so this observes. Four
//! frames at a ceiling of four ray charges, printing the resume guard's own inputs and the two failure
//! signatures that look identical from outside: a *regression* (a pixel decided last frame is short of
//! again now, which means the sentinel init ran and the guard refused to carry) and a *repaint* (the depth
//! held but the colour changed, which is the symptom the reverted test actually hit).

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE, NODE_UNION,
};
use render::raster::{Budget, Camera, Frame, Scan};
use render::stream::{plan, render_bnb, render_budget_refined, Region};
use render::Tree;

fn chain(count: usize, spacing: f32, x: f32, nodes: &mut Vec<NodeRecord>, params: &mut Vec<f32>) {
    let root = nodes.is_empty();
    let flag = if root { FLAG_IS_ROOT } else { 0 };
    if count == 1 {
        nodes.push(NodeRecord([
            pack_kind_and_arity(NODE_SPHERE, 0),
            1,
            pack_first_and_flags(params.len() as u16, flag),
            pack_params_len(4),
        ]));
        params.extend_from_slice(&[0.8, x, 0.0, 0.0]);
        return;
    }
    nodes.push(NodeRecord([
        pack_kind_and_arity(NODE_UNION, 2),
        (2 * count - 1) as u32,
        pack_first_and_flags(0, flag),
        pack_params_len(0),
    ]));
    chain(1, spacing, x, nodes, params);
    chain(count - 1, spacing, x + spacing, nodes, params);
}

fn ridge(count: usize, spacing: f32) -> Tree {
    let mut nodes = Vec::new();
    let mut params = Vec::new();
    chain(
        count,
        spacing,
        -((count as f32 - 1.0) * spacing / 2.0),
        &mut nodes,
        &mut params,
    );
    Tree { nodes, params }
}

fn main() {
    let camera = Camera {
        origin: [0.0, 0.0, -6.0],
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: 32,
        height: 24,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 96,
    };
    let regions = [Region {
        cell: [0, 0, 0],
        tree: ridge(9, 8.0),
    }];
    let selected = plan(&camera, &regions);
    let rays = (camera.width * camera.height) as usize;
    let (full, full_stats) = render_bnb(&camera, &regions, &selected);
    let charge = u64::from(camera.max_steps) + 1;
    println!("// OQ-59: refinement watched, not reasoned about");
    println!(
        "// host {} / arch {}; rays {rays}, one ray's charge {charge}, whole frame's charge {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        full_stats.evaluations
    );
    println!();
    println!(" frame | charged | short of | decided | depth.len | pixels.len | frame wxh | regressions | repaints | depth==full for decided");
    let mut frame = Frame::new(camera.width, camera.height);
    let mut last_depth = vec![Frame::NOT_DRAWN_DEPTH; rays];
    let mut last_rgb = frame.pixels.clone();
    let mut finished_in = 0u32;
    for iteration in 1..=64 {
        let stats = render_budget_refined(
            &camera,
            &regions,
            &selected,
            Budget(charge),
            Scan::TileMajor,
            &mut frame,
        );
        let mut regressions = 0usize;
        let mut repaints = 0usize;
        let mut depth_matches = 0usize;
        let decided = usize::try_from(stats.rays - stats.not_drawn).unwrap_or(usize::MAX);
        for index in 0..rays {
            let was_decided = last_depth[index] != Frame::NOT_DRAWN_DEPTH;
            let is_decided = frame.depth[index] != Frame::NOT_DRAWN_DEPTH;
            if was_decided && !is_decided {
                regressions += 1;
            }
            if is_decided {
                if frame.depth[index] == full.depth[index] {
                    depth_matches += 1;
                }
                if was_decided
                    && frame.pixels[index * 3..index * 3 + 3] != last_rgb[index * 3..index * 3 + 3]
                {
                    repaints += 1;
                }
            }
        }
        println!(
            " {:>5} | {:>7} | {:>8} | {:>7} | {:>9} | {:>10} | {:>9} | {:>11} | {:>8} | {:>21}",
            iteration,
            stats.evaluations,
            stats.not_drawn,
            decided,
            frame.depth.len(),
            frame.pixels.len(),
            format!("{}x{}", frame.width, frame.height),
            regressions,
            repaints,
            format!("{depth_matches}/{decided}"),
        );
        if stats.not_drawn == 0 {
            finished_in = iteration;
            break;
        }
        last_depth = frame.depth.clone();
        last_rgb = frame.pixels.clone();
    }
    println!();
    println!(
        "// frames to finish at one ray charge per frame: {finished_in} (MEASURED, tile-major, {rays} rays, \
         whole frame costs {})",
        full_stats.evaluations
    );
    println!("// This is the column `stream_descent` still prints as DERIVED - the grid divided by the pixels");
    println!("// one ceiling decided. It is not the same number: skipped rays are free, so a frame settles far");
    println!("// more pixels than its charge divided by one ray's cost, and the derived figure understates");
    println!("// how fast a streamed frame converges. `stream_descent` should be switched to this measurement.");
    println!();
    println!(
        "// regressions > 0 means the resume guard refused to carry and the init wiped the frame;"
    );
    println!(
        "// repaints > 0 with depth==full is the shading path disagreeing between walkers; and"
    );
    println!("// depth!=full for decided pixels says the cut itself re-marched something it should not have.");
    println!(
        "// The whole-frame charge is printed above so the ceiling's unit is visible: a ceiling of"
    );
    println!("// four charges affords four rays, not four percent of the frame.");
}
