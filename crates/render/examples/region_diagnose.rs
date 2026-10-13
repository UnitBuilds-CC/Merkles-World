//! Reproduce the demo's multi-region frame failure exactly, then vary one factor at a time.
//!
//! `cargo run --release --example region_diagnose -p render`
//!
//! OQ-57 was written with a mechanism, the mechanism failed a controlled test, and what is left is the
//! observation: the demo's configuration lost a far cell's surface and a cleaner stand-in does not. This
//! walks the difference out, one factor per row, so the cause is a named factor rather than another story.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SMOOTH_UNION, NODE_SPHERE, NODE_UNION,
};
use render::raster::Camera;
use render::stream::{plan, render, Region};
use render::Tree;

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// The hero exactly as the boot assembles it after `assetc`'s rebalance: a plain union of a rounded box
/// with a smooth-union pair, plus the three spread spheres the demo added.
fn hero_tree() -> Tree {
    Tree {
        nodes: vec![
            record(NODE_UNION, 2, 11, 0, 0, true),
            record(NODE_UNION, 2, 5, 0, 0, false),
            record(NODE_ROUNDED_BOX, 0, 1, 0, 4, false),
            record(NODE_SMOOTH_UNION, 2, 3, 4, 1, false),
            record(NODE_SPHERE, 0, 1, 5, 4, false),
            record(NODE_SPHERE, 0, 1, 9, 4, false),
            record(NODE_UNION, 2, 5, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 13, 4, false),
            record(NODE_UNION, 2, 3, 0, 0, false),
            record(NODE_SPHERE, 0, 1, 17, 4, false),
            record(NODE_SPHERE, 0, 1, 21, 4, false),
        ],
        params: vec![
            0.35, 0.35, 0.9, 0.12, //
            0.5, 0.7, -0.9, 0.0, 0.0, 0.7, 0.9, 0.0, 0.0, //
            0.8, 0.0, 0.0, 0.0, //
            0.5, -6.0, 0.0, 1.5, //
            0.5, 6.2, -0.4, -1.0, //
            0.5, 1.0, 3.2, 7.5,
        ],
    }
}

fn sphere(r: f32) -> Tree {
    Tree {
        nodes: vec![record(NODE_SPHERE, 0, 1, 0, 4, true)],
        params: vec![r, 0.0, 0.0, 0.0],
    }
}

fn slab() -> Tree {
    Tree {
        nodes: vec![record(NODE_ROUNDED_BOX, 0, 1, 0, 4, true)],
        params: vec![900.0, 120.0, 900.0, 200.0],
    }
}

fn camera(width: u32, height: u32, steps: u32) -> Camera {
    Camera {
        origin: [0.0, 0.0, 0.0],
        forward: [1.0, 0.0, 0.0],
        right: [0.0, 1.0, 0.0],
        up: [0.0, 0.0, 1.0],
        half_height: 0.5,
        width,
        height,
        max_t: 16_384.0,
        surface: 1e-2,
        max_steps: steps,
    }
}

fn cells(hero: Tree, ridge: Tree) -> Vec<Region> {
    vec![
        Region {
            cell: [1, 0, 0],
            tree: hero,
        },
        Region {
            cell: [2, 0, 0],
            tree: ridge,
        },
        Region {
            cell: [3, 0, 0],
            tree: sphere(500.0),
        },
    ]
}

fn compare(label: &str, cam: &Camera, regions: &[Region]) {
    let planned = plan(cam, regions);
    let (all_frame, all_stats) = render(cam, regions, &planned);
    let neighbours = &regions[1..];
    let terrain_plan = plan(cam, neighbours);
    let (terrain_frame, terrain_stats) = render(cam, neighbours, &terrain_plan);
    let first_diff = (0..all_frame.depth.len()).find(|&i| {
        all_frame.depth[i] != terrain_frame.depth[i]
            || all_frame.pixels[i * 3..i * 3 + 3] != terrain_frame.pixels[i * 3..i * 3 + 3]
    });
    let verdict = match first_diff {
        None => "identical".to_string(),
        Some(index) => format!(
            "differs at pixel {index}: all {} vs terrain {}",
            all_frame.depth[index], terrain_frame.depth[index]
        ),
    };
    println!(
        " {label:<38} | planned {:>2} | hits {:>4}/{:<4} | evals {:>9} vs {:>9} | skipped {:>4} vs {:>4} | {verdict}",
        planned.len(),
        all_stats.hits,
        all_stats.rays,
        all_stats.evaluations,
        terrain_stats.evaluations,
        all_stats.skipped,
        terrain_stats.skipped,
    );
}

fn main() {
    println!("// OQ-57 diagnosis - the demo configuration, then one factor changed per row");
    println!("// each row compares 'all three cells' with 'the two terrain cells': the hero prop should be");
    println!("// invisible at this distance, so any difference is the bug being looked for.");
    println!();
    let demo = cells(hero_tree(), slab());
    let prop_hero = cells(sphere(1.6), slab());
    let sphere_ridge = cells(hero_tree(), sphere(700.0));
    let both_simple = cells(sphere(1.6), sphere(700.0));
    for (label, cam, regions) in [
        (
            "demo as shipped (64x48, 512 steps)",
            camera(64, 48, 512),
            &demo[..],
        ),
        ("demo, 96 steps", camera(64, 48, 96), &demo[..]),
        (
            "hero swapped for a 1.6 sphere",
            camera(64, 48, 512),
            &prop_hero[..],
        ),
        (
            "ridge swapped for a 700 sphere",
            camera(64, 48, 512),
            &sphere_ridge[..],
        ),
        (
            "both neighbours are spheres",
            camera(64, 48, 512),
            &both_simple[..],
        ),
        (
            "grid 32x24, hero as shipped",
            camera(32, 24, 512),
            &demo[..],
        ),
    ] {
        compare(label, &cam, regions);
    }
    println!();
    println!(
        "// Read the verdict column: rows that differ are configurations where adding a cell the"
    );
    println!(
        "// camera cannot resolve changed the frame. The first row that stops differing while the"
    );
    println!("// others still do names the factor.");
}
