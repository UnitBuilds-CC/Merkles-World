//! OQ-50's decision packet: what each voxel-span convention costs, measured rather than argued.
//!
//! `cargo run --release --example voxel_span_oq50 -p render`
//!
//! docs/02 fixes a voxel's *size* (one unit) and gate C1 fixes the region arithmetic, but not where the unit
//! sits on the integer: either it is the half-open span `[i, i+1)` (the low corner on the index) or it is
//! centred, `[i-0.5, i+0.5)`. Nothing in this workspace converts a voxel to world space yet, so neither
//! answer has been taken for granted by code - which is why the question is open and why an instrument is the
//! useful contribution rather than a guess: this prints what each convention implies, and it names no winner.
//!
//! Two halves. First, the arithmetic consequence against the contract that is already frozen: `region()` is
//! `i >> 12`, and its own comment says region 0 is `[0, 4096)`. Does a single voxel's cube stay inside the
//! region its index names? Second, the visual consequence: march one voxel's inscribed ball placed each way
//! and count how different the pictures actually are, close up and across a chunk.

use contracts::coords::GlobalVoxelPos;
use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_SPHERE,
};
use render::raster::{render_bnb, Camera, Frame};
use render::Tree;

const REGION: i64 = GlobalVoxelPos::REGION_SIZE;

fn record(kind: u16, arity: u16, subtree: u32, first: u16, len: u16, root: bool) -> NodeRecord {
    NodeRecord([
        pack_kind_and_arity(kind, arity),
        subtree,
        pack_first_and_flags(first, if root { FLAG_IS_ROOT } else { 0 }),
        pack_params_len(len),
    ])
}

/// A voxel's inscribed ball, radius 0.5, placed by the convention under test.
fn voxel_ball(centre: [f64; 3]) -> Tree {
    Tree {
        nodes: vec![record(NODE_SPHERE, 0, 1, 0, 4, true)],
        params: vec![0.5, centre[0] as f32, centre[1] as f32, centre[2] as f32],
    }
}

/// The regions a closed interval `[lo, hi]` touches, using the contract's own floor semantics.
fn regions_touched(lo: f64, hi: f64) -> Vec<i64> {
    let first = lo.floor().div_euclid(REGION as f64) as i64;
    let last = hi.floor().div_euclid(REGION as f64) as i64;
    (first..=last).collect()
}

fn camera_at(distance: f32) -> Camera {
    Camera {
        origin: [0.5 - distance, 0.5, 0.5],
        forward: [1.0, 0.0, 0.0],
        right: [0.0, 1.0, 0.0],
        up: [0.0, 0.0, 1.0],
        half_height: 0.5,
        width: 64,
        height: 48,
        max_t: distance * 3.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

fn main() {
    println!("// OQ-50: which world span does a voxel coordinate name?");
    println!("// convention A: [i, i+1)   (low corner on the index)");
    println!("// convention B: [i-0.5, i+0.5) (cube centred on the index)");
    println!();

    // ---- the arithmetic half, against the region() the contract already froze -------------------------
    // Every multiple of REGION is where the two answers part company: a centred cube there has its low half
    // in the region below the one its own index maps to.
    let mut probes: Vec<i64> = Vec::new();
    for k in -3i64..=3 {
        probes.push(k * REGION);
        probes.push(k * REGION - 1);
        probes.push(k * REGION + 1);
    }
    probes.extend([0, 37, -12, 4095, 4096, -4096, -4097]);

    let mut straddle_a = 0usize;
    let mut straddle_b = 0usize;
    let mut disagree_a = 0usize;
    let mut disagree_b = 0usize;
    for i in &probes {
        let pos = GlobalVoxelPos { x: *i, y: 0, z: 0 };
        let named = pos.region_key().x;
        // Convention A: the cube occupies [i, i+1). Its top edge belongs to the next region by floor rules,
        // so ask about the half-open interior instead: any point strictly inside [i, i+1) floors to i.
        let a_regions = regions_touched(*i as f64 + 1e-9, *i as f64 + 1.0 - 1e-9);
        // Convention B: the cube occupies [i-0.5, i+0.5).
        let b_regions = regions_touched(*i as f64 - 0.5 + 1e-9, *i as f64 + 0.5 - 1e-9);
        if a_regions.len() > 1 {
            straddle_a += 1;
        }
        if b_regions.len() > 1 {
            straddle_b += 1;
        }
        if a_regions.first() != Some(&named) {
            disagree_a += 1;
        }
        if b_regions.first() != Some(&named) {
            disagree_b += 1;
        }
    }
    println!(
        "// over {} probe indices (every region boundary in +-3 regions):",
        probes.len()
    );
    println!(
        "//   A [i,i+1)     : {} voxels touch more than one region, {} disagree with region_key()'s own answer",
        straddle_a, disagree_a
    );
    println!(
        "//   B [i-.5,i+.5) : {} voxels touch more than one region, {} disagree with region_key()'s own answer",
        straddle_b, disagree_b
    );
    println!(
        "// gate C1's comment says region 0 is [0, 4096) - a half-open span, which is A's shape."
    );
    println!();

    // ---- the visual half: can a player tell, at two distances? ---------------------------------------
    // One voxel well inside a region, and one sitting exactly on a boundary, because that is where the two
    // conventions differ in kind rather than in position.
    for (label, voxel) in [
        ("interior (37,0,0)", [37i64, 0, 0]),
        ("boundary (4096,0,0)", [REGION, 0, 0]),
    ] {
        let low = [
            voxel[0] as f64 + 0.5,
            voxel[1] as f64 + 0.5,
            voxel[2] as f64 + 0.5,
        ];
        let mid = [voxel[0] as f64, voxel[1] as f64, voxel[2] as f64];
        println!(
            "// voxel {label} - the two placements are {} world units apart",
            0.5
        );
        for distance in [2.0f32, 32.0, 128.0] {
            // Aim the camera at the voxel whichever way it sits, so the difference is the geometry and not
            // the framing: the eye moves with the candidate centre.
            let mut frames = Vec::new();
            for centre in [low, mid] {
                let camera = Camera {
                    origin: [
                        centre[0] as f32 - distance,
                        centre[1] as f32,
                        centre[2] as f32,
                    ],
                    ..camera_at(distance)
                };
                frames.push(render_bnb(&voxel_ball(centre), &camera));
            }
            let (a, _) = &frames[0];
            let (b, sb) = &frames[1];
            let rays = a.depth.len();
            let pixels_differ = (0..rays).filter(|k| a.depth[*k] != b.depth[*k]).count();
            let hits_a = a.depth.iter().filter(|d| **d != Frame::NO_HIT).count();
            let worst = (0..rays)
                .filter(|k| a.depth[*k] != Frame::NO_HIT && b.depth[*k] != Frame::NO_HIT)
                .map(|k| (a.depth[k] - b.depth[k]).abs())
                .fold(0.0f32, f32::max);
            println!(
                "//   at {distance:>3} units: {} of {rays} pixels change depth, surface present {} vs {}, worst shift {worst:.3} units (the B frame charges {} evaluations)",
                pixels_differ,
                hits_a,
                b.depth.iter().filter(|d| **d != Frame::NO_HIT).count(),
                sb.evaluations
            );
        }
        println!();
    }

    println!("// What this settles and what it does not: the arithmetic half is a contradiction test against a");
    println!("// frozen contract, and only one convention passes it. The visual half is about whether the");
    println!("// difference is *seen* - it is not a claim that either placement is correct, and no code here");
    println!("// chooses one. docs/13's OQ-50 row carries the ruling, which needs the Architect because the");
    println!("// answer also fixes how physics (doc 07) and any future base terrain read the same coordinate.");
}
