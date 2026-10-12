//! Run A-07's WGSL master interpreter on a device and subtract the CPU reference.
//!
//! `cargo run --release -p gpu --example agreement`
//!
//! The scene is the one the engine boots with — a rounded box unioned with a smooth union of two
//! spheres, five nodes deep enough to exercise the walk and shallow enough to sit inside
//! `MAX_TREE_DEPTH`. Distances are sampled on a fixed grid: no RNG and no clock, so a deviation this
//! machine reports describes points another machine would test too.
//!
//! Exit codes, because the honest outcomes are three and not two:
//!   0 - agreement within tolerance, **or** no adapter at all (the x86-64 Linux CI runner), which is
//!       printed as a skip and never as a pass;
//!   1 - an adapter that would not open a device, or a deviation above tolerance.
//!
//! And what the number is not: nothing here is hashed, so this is not gate P1.5 or R4 evidence
//! (ADR-017), and running on a software adapter says nothing about gate R1's frame budget. It is
//! evidence that two implementations of one contract compute one shape the same way.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_ROUNDED_BOX, NODE_SMOOTH_UNION, NODE_SPHERE, NODE_UNION,
};
use gpu::device::{sample_grid, Interpreter, OpenError};
use render::{validate, Tree};

/// The engine's boot scene, packed the way `assetc` packs: pre-order, `subtree_len` derived from the
/// children, one root flag.
fn scene() -> Tree {
    let params = vec![
        0.35, 0.35, 0.9, 0.12, // rounded box: half-extents then fillet radius
        0.5,  // the blend radius of the inner smooth union
        0.7, -0.9, 0.0, 0.0, // left bead
        0.7, 0.9, 0.0, 0.0, // right bead
    ];
    let tree = Tree {
        nodes: vec![
            NodeRecord([
                pack_kind_and_arity(NODE_UNION, 2),
                5,
                pack_first_and_flags(0, FLAG_IS_ROOT),
                pack_params_len(0),
            ]),
            NodeRecord([
                pack_kind_and_arity(NODE_ROUNDED_BOX, 0),
                1,
                pack_first_and_flags(0, 0),
                pack_params_len(4),
            ]),
            NodeRecord([
                pack_kind_and_arity(NODE_SMOOTH_UNION, 2),
                3,
                pack_first_and_flags(4, 0),
                pack_params_len(1),
            ]),
            NodeRecord([
                pack_kind_and_arity(NODE_SPHERE, 0),
                1,
                pack_first_and_flags(5, 0),
                pack_params_len(4),
            ]),
            NodeRecord([
                pack_kind_and_arity(NODE_SPHERE, 0),
                1,
                pack_first_and_flags(9, 0),
                pack_params_len(4),
            ]),
        ],
        params,
    };
    validate(&tree).expect("the scene validates before either evaluator sees it");
    tree
}

/// How far apart two `f32` evaluators of the same formulas are allowed to be before this stops counting
/// as agreement. Generous on purpose: it is a *wrong shape* detector, not a precision claim, and a
/// tighter bound would be a test of `f32` rather than of the contract.
const TOLERANCE: f32 = 1e-4;

fn main() {
    let tree = scene();
    let points = sample_grid();
    println!(
        "// A-07 agreement: WGSL master interpreter vs crates/render, {} sample points, one tree",
        points.len()
    );

    let interpreter = match Interpreter::open() {
        Ok(interpreter) => interpreter,
        Err(OpenError::NoAdapter) => {
            println!(
                "SKIP: this machine enumerates no GPU adapters (expected on the Linux CI runner)"
            );
            return;
        }
        Err(OpenError::NoDevice) => {
            eprintln!("FAIL: an adapter was found but would not open a device");
            std::process::exit(1);
        }
    };

    match interpreter.compare_with_reference(&tree, &points) {
        Ok(report) => {
            println!("adapter          : {}", report.adapter);
            println!(
                "samples          : {} compared, {} excluded as sentinel results",
                report.samples, report.sentinels
            );
            println!(
                "max deviation    : {:.9} world units at {:?} (gpu {:.9} vs cpu {:.9})",
                report.max_abs_deviation,
                report.worst_point,
                report.gpu_at_worst,
                report.cpu_at_worst
            );
            if report.max_abs_deviation > TOLERANCE {
                eprintln!(
                    "FAIL: {:.3} exceeds the {TOLERANCE} tolerance - the two implementations do not compute one shape",
                    report.max_abs_deviation
                );
                std::process::exit(1);
            }
            println!(
                "agreement        : within {TOLERANCE} units on every compared point (presentation only; not gate P1.5, R4 or R1 evidence)"
            );
        }
        Err(error) => {
            eprintln!("FAIL: {error}");
            std::process::exit(1);
        }
    }
}
