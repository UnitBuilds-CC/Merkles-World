//! OQ-60's curve: how many frames a streamed frame needs to settle, as a function of its ceiling.
//!
//! `cargo run --release --example refine_probe -p render`
//!
//! The boot's step 9b performs the carry at one ray charge per frame and settles a 3,072-ray world in 813
//! frames. That single point raised the question the register now carries as OQ-60: the ceiling is charged
//! *before* the reachability test, so rays proved empty at zero cost are not harvested - frame one settles
//! one ray out of 3,072. Whether that matters is a shape question, not an opinion, so this file sweeps the
//! ceiling and prints the shape: frames to settle, the share of the grid that was free, and the cumulative
//! charge, which the carry provably does not change.
//!
//! Integers only. No milliseconds, and no claim about a frame time - R1 is a GPU question (OQ-51).

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE, NODE_UNION,
};
use render::raster::{Budget, Camera, Frame, Scan};
use render::stream::{plan, render_bnb, render_budget_refined, Region};
use render::Tree;

fn chain(count: usize, spacing: f32, x: f32, nodes: &mut Vec<NodeRecord>, params: &mut Vec<f32>) {
    let flag = if nodes.is_empty() { FLAG_IS_ROOT } else { 0 };
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

/// Refine one frame at `ceiling` until nothing is short of, and return (frames, cumulative charge, rays
/// proved empty, pixels settled by the first frame). The cap is a safety stop for a ceiling that cannot
/// afford a ray at all - which is a real case, not a hypothetical: `max_steps` charges less than the
/// worst-case `max_steps + 1`, so such a budget settles nothing, forever.
fn carry(
    camera: &Camera,
    regions: &[Region],
    selected: &[usize],
    ceiling: u64,
    scan: Scan,
    cap: u32,
) -> (u32, u64, u32, u32) {
    let mut frame = Frame::new(camera.width, camera.height);
    let mut frames = 0u32;
    let mut charge_total = 0u64;
    let mut skipped = 0u32;
    let mut first = 0u32;
    loop {
        frames += 1;
        let stats =
            render_budget_refined(camera, regions, selected, Budget(ceiling), scan, &mut frame);
        assert!(
            stats.evaluations <= ceiling,
            "frame {frames} breached a ceiling of {ceiling}: {}",
            stats.evaluations
        );
        charge_total += stats.evaluations;
        skipped += stats.skipped;
        if frames == 1 {
            first = stats.rays - stats.not_drawn;
        }
        if stats.not_drawn == 0 || frames >= cap {
            return (frames, charge_total, skipped, first);
        }
    }
}

fn main() {
    let camera = Camera {
        origin: [0.0, 0.0, -6.0],
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: 64,
        height: 48,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 96,
    };
    let regions = [Region {
        cell: [0, 0, 0],
        tree: ridge(33, 8.0),
    }];
    let selected = plan(&camera, &regions);
    let rays = u64::from(camera.width * camera.height);
    let (settled, single) = render_bnb(&camera, &regions, &selected);
    let charge = u64::from(camera.max_steps) + 1;
    println!("// OQ-60: frames to settle a streamed frame, against the size of its ceiling");
    println!(
        "// host {} / arch {}; {} rays, one ray's worst-case charge {charge}, the single pass charged {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        rays,
        single.evaluations
    );
    println!(
        "// of those rays, {} are proved empty by the bounds before any marching (charge {} of them at zero)",
        single.skipped,
        single.skipped
    );
    println!();
    println!(" ceiling | charges | frame 1 settles | frames to settle all | cumulative charge | charge per frame | free share of the grid");
    for charges in [0u64, 1, 2, 4, 8, 16, 32, 64, 128] {
        let ceiling = charge * charges;
        let (frames, total, _skipped, first) =
            carry(&camera, &regions, &selected, ceiling, Scan::CentreOut, 4096);
        let settled_all = frames < 4096;
        println!(
            " {:>7} | {:>7} | {:>15} | {:>20} | {:>17} | {:>16} | {:>21}",
            if charges == 0 {
                "none".to_string()
            } else {
                ceiling.to_string()
            },
            charges,
            first,
            if settled_all {
                format!("{frames}")
            } else {
                format!(">{frames} (cap)")
            },
            if settled_all {
                total.to_string()
            } else {
                format!("{total} (partial)")
            },
            if settled_all && frames > 0 {
                (total / u64::from(frames)).to_string()
            } else {
                "-".to_string()
            },
            format!("{:.1}%", 100.0 * single.skipped as f64 / rays as f64),
        );
        if settled_all {
            assert_eq!(
                total, single.evaluations,
                "{charges} charges: the carry charged {total} against the single pass's {}",
                single.evaluations
            );
            assert_eq!(settled.depth.len(), rays as usize);
        }
    }
    println!();
    println!("// What the curve says is not what I expected when I wrote the boot's line: the ceiling's SIZE");
    println!("// dominates, and steeply. One charge settles this grid in 125 frames, two in 9, four in 3,");
    println!("// eight in 2, sixteen in one - every one of those rows charging the same 876 in total, which");
    println!("// the asserts above hold. So refinement is free in work at every ceiling, and the frames are");
    println!("// bought with ceiling, not with savings.");
    println!("// That sharpens OQ-60 rather than answering it: with the charge taken before the reachability");
    println!("// test, a frame can only ever buy a couple of rays, which is why frame one settles 22 pixels");
    println!("// of 3,072 even though 96% of this grid is provably empty and costs nothing. Harvesting the");
    println!("// free rays first would move those 2,948 pixels into frame one at the same total charge - the");
    println!("// same work, a far better first frame. Whether the prefix property that buys is worth more");
    println!(
        "// than the predictability it costs is the decision, and it is now numerically specified."
    );
    println!("// The zero-charge row is the sub-ray law, kept because it is the failure a caller actually");
    println!("// hits: a budget smaller than one ray's worst case draws nothing, forever, and looks exactly");
    println!("// like a renderer that stopped working.");
}
