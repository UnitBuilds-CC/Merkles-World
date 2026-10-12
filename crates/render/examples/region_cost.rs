//! What a world of regions costs per frame, planned and unplanned.
//!
//! `cargo run --release --example region_cost -p render`
//!
//! docs/11's renderer row lists "region streaming" as a deliverable, and the honest question it has to
//! answer is not "can we draw a big world" - the raster already draws whatever it is given - but **"does a
//! frame cost the world, or does it cost the neighbourhood?"**. This measures that, and it also corrects the
//! naive expectation about where the saving is.
//!
//! A-07 has no translation node, so N regions are N trees and the frame is the minimum of N fields
//! (`stream` exists for that reason and OQ-55 asks whether the grammar should grow an instance kind). Each
//! region carries its own bound, so a ray that cannot reach a region is rejected before it is evaluated -
//! which means the *march* is already bounded per region. What planning removes is the **test**: `regions x
//! rays` bound checks become `planned x rays`. In a seven-cell world that is a factor of seven on the part
//! of the cost that scales with the size of the world rather than with what is on screen.
//!
//! Counts only. No milliseconds, and no claim about a frame time: gate R1 is a GPU statement and there is
//! still no GPU here.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE,
};
use render::raster::Camera;
use render::stream::{plan, render, Region, REGION_UNITS};
use render::Tree;

fn sphere(radius: f32, centre: [f32; 3]) -> Tree {
    Tree {
        nodes: vec![NodeRecord([
            pack_kind_and_arity(NODE_SPHERE, 0),
            1,
            pack_first_and_flags(0, FLAG_IS_ROOT),
            pack_params_len(4),
        ])],
        params: vec![radius, centre[0], centre[1], centre[2]],
    }
}

fn camera(origin: [f32; 3], max_t: f32) -> Camera {
    Camera {
        origin,
        forward: [0.0, 0.0, 1.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        half_height: 0.5,
        width: 64,
        height: 48,
        max_t,
        surface: 1e-3,
        max_steps: 96,
    }
}

fn main() {
    println!(
        "// region streaming cost - a cell is {:.0} units, so a seven-cell world is 28,672 units across",
        REGION_UNITS
    );
    println!(
        "// host {} / arch {}; integers only, no milliseconds",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!();
    println!(" regions in world | range | planned | bound checks | charged evals | rays skipped | hits | frame changed");
    let camera = camera([0.0, 0.0, -6.0], 40.0);
    let rays = i64::from(camera.width * camera.height);
    for world in [1usize, 3, 7, 21, 64] {
        let regions: Vec<Region> = (0..world as i64)
            .map(|index| Region {
                // Cells strung along z, the camera standing in the middle of them.
                cell: [0, 0, index - world as i64 / 2],
                tree: sphere(0.9, [0.0, 0.0, 0.0]),
            })
            .collect();
        let all: Vec<usize> = (0..regions.len()).collect();
        let selected = plan(&camera, &regions);
        let (planned_frame, planned) = render(&camera, &regions, &selected);
        let (all_frame, unplanned) = render(&camera, &regions, &all);
        let changed =
            planned_frame.pixels != all_frame.pixels || planned_frame.depth != all_frame.depth;
        println!(
            " {:>14} | {:>5} | {:>7} | {:>12} | {:>13} | {:>12} | {:>4} | {}",
            world,
            camera.max_t,
            selected.len(),
            rays * world as i64,
            planned.evaluations,
            planned.skipped,
            planned.hits,
            if changed { "YES - bug" } else { "no" },
        );
        assert_eq!(
            planned.evaluations, unplanned.evaluations,
            "planning changed the march work"
        );
        assert!(!changed, "planning dropped geometry the frame could see");
        assert!(
            (selected.len() as i64) <= world as i64,
            "the plan cannot select more regions than exist"
        );
    }
    println!();
    println!("// What the table says:");
    println!("// - `frame changed` is `no` at every size, which is the property streaming has to have: a");
    println!("//   region the plan dropped contributed nothing any ray could reach. The same assertion runs");
    println!("//   in tests/stream.rs against the single-tree renderer, so it is not this example grading");
    println!("//   its own implementation.");
    println!("// - `charged evals` is FLAT as the world grows, and that is the part that was assumed rather");
    println!("//   than known: each region already carries a bound, so a distant region's rays are rejected");
    println!(
        "//   before any field is evaluated. Streaming did not buy that - the enclosure work did."
    );
    println!(
        "// - `bound checks` is where a big world actually costs something, and it is the column"
    );
    println!("//   planning shrinks: one test per region per ray, so seven times the regions is seven times");
    println!("//   the checks even though the march is unchanged. That is the arithmetic a real streaming");
    println!("//   layer exists to avoid, and it grows with the size of the world rather than the screen.");
    println!("// - unchanged by this probe: A-07 still cannot express a translated subtree, so a region is a");
    println!("//   separate tree and the frame is a minimum of fields (OQ-55). And nothing above is a frame");
    println!("//   time; gate R1 still waits on a GPU.");
}
