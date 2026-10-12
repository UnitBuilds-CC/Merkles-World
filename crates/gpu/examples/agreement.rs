//! Run A-07's WGSL master interpreter on a device, one tree per implemented kind, and subtract the CPU
//! reference.
//!
//! `cargo run --release -p gpu --example agreement`
//!
//! Why per kind rather than one pretty scene: a single union-of-three exercises three arms and hides
//! ten. The contract's claim is that *any* tree the grammar allows computes the same field twice, so the
//! check walks each implemented arm on its own tree — five leaves, four binaries, four space-folding
//! unaries — and reports a deviation per arm. One number for one scene is a demo; thirteen numbers is a
//! gate.
//!
//! Exit codes, because the honest outcomes are three:
//!   0 - every kind agrees within tolerance, **or** no adapter exists (the Linux CI runner), printed as a
//!       skip and never as a pass;
//!   1 - an adapter that would not open a device, or any kind outside tolerance.
//!
//! What agreement is not: nothing here is hashed, so this is not gate P1.5 or R4 evidence (ADR-017), and
//! a software adapter says nothing about gate R1's frame budget. `Noise` has no arm on either side by
//! design — it needs docs/02's lattice hash and OQ-45's payload meaning — so thirteen is the whole
//! implemented set, not fifteen.

use contracts::sdf::{
    pack_first_and_flags, pack_kind_and_arity, pack_params_len, NodeRecord, FLAG_IS_ROOT,
    NODE_BEND, NODE_CYLINDER, NODE_INTERSECT, NODE_MIRROR, NODE_PLANE, NODE_REPEAT,
    NODE_ROUNDED_BOX, NODE_SMOOTH_UNION, NODE_SPHERE, NODE_SUBTRACT, NODE_TORUS, NODE_TWIST,
    NODE_UNION,
};
use gpu::device::{Interpreter, OpenError};
use render::{validate, Tree, Vec3};

/// Pre-order by construction: a parent is pushed before its children, so `params_first` lands where
/// `assetc` would put it and the tree is validated before either evaluator sees it.
#[derive(Default)]
struct Builder {
    nodes: Vec<NodeRecord>,
    params: Vec<f32>,
}

impl Builder {
    fn push(&mut self, kind: u16, arity: u16, subtree: u32, params: &[f32], root: bool) {
        let first = self.params.len() as u16;
        self.params.extend_from_slice(params);
        let flags = if root { FLAG_IS_ROOT } else { 0 };
        self.nodes.push(NodeRecord([
            pack_kind_and_arity(kind, arity),
            subtree,
            pack_first_and_flags(first, flags),
            pack_params_len(params.len() as u16),
        ]));
    }

    fn leaf(&mut self, kind: u16, params: &[f32], root: bool) {
        self.push(kind, 0, 1, params, root);
    }

    fn binary(&mut self, kind: u16, params: &[f32], left: &[f32], right: &[f32], root: bool) {
        self.push(kind, 2, 3, params, root);
        self.leaf(NODE_SPHERE, left, false);
        self.leaf(NODE_SPHERE, right, false);
    }

    fn unary(&mut self, kind: u16, params: &[f32], child: &[f32], root: bool) {
        self.push(kind, 1, 2, params, root);
        self.leaf(NODE_SPHERE, child, false);
    }

    fn finish(self, name: &str) -> Tree {
        let tree = Tree {
            nodes: self.nodes,
            params: self.params,
        };
        validate(&tree).unwrap_or_else(|error| {
            panic!("{name} must validate before it is compared: {error:?}")
        });
        tree
    }
}

/// A sphere at `x`, so the kinds differ in where their surface is and not only in what they are.
fn bead(x: f32) -> Vec<f32> {
    vec![0.6, x, 0.0, 0.0]
}

/// One tree per implemented kind, named as docs/04 names them.
fn kinds() -> Vec<(&'static str, Tree)> {
    let mut out: Vec<(&'static str, Tree)> = Vec::new();

    let mut b = Builder::default();
    b.leaf(NODE_SPHERE, &[0.7, 0.0, 0.0, 0.0], true);
    out.push(("sphere", b.finish("sphere")));

    let mut b = Builder::default();
    b.leaf(NODE_ROUNDED_BOX, &[0.5, 0.4, 0.3, 0.12], true);
    out.push(("rounded box", b.finish("rounded box")));

    let mut b = Builder::default();
    b.leaf(NODE_CYLINDER, &[0.45, 0.8, 1.0], true);
    out.push(("cylinder", b.finish("cylinder")));

    let mut b = Builder::default();
    b.leaf(NODE_TORUS, &[0.8, 0.25], true);
    out.push(("torus", b.finish("torus")));

    let mut b = Builder::default();
    b.leaf(NODE_PLANE, &[0.0, 1.0, 0.0, 0.35], true);
    out.push(("plane", b.finish("plane")));

    let mut b = Builder::default();
    b.binary(NODE_UNION, &[], &bead(-0.8), &bead(0.8), true);
    out.push(("union", b.finish("union")));

    let mut b = Builder::default();
    b.binary(NODE_SMOOTH_UNION, &[0.5], &bead(-0.8), &bead(0.8), true);
    out.push(("smooth union", b.finish("smooth union")));

    let mut b = Builder::default();
    b.binary(NODE_SUBTRACT, &[], &bead(0.0), &bead(0.55), true);
    out.push(("subtract", b.finish("subtract")));

    let mut b = Builder::default();
    b.binary(NODE_INTERSECT, &[], &bead(0.0), &bead(0.5), true);
    out.push(("intersect", b.finish("intersect")));

    let mut b = Builder::default();
    b.unary(NODE_REPEAT, &[1.6], &bead(0.0), true);
    out.push(("repeat", b.finish("repeat")));

    let mut b = Builder::default();
    b.unary(NODE_MIRROR, &[0.0], &bead(0.6), true);
    out.push(("mirror", b.finish("mirror")));

    let mut b = Builder::default();
    b.unary(NODE_TWIST, &[0.7, 1.0], &bead(0.0), true);
    out.push(("twist", b.finish("twist")));

    let mut b = Builder::default();
    b.unary(NODE_BEND, &[0.45, 0.0], &bead(0.0), true);
    out.push(("bend", b.finish("bend")));

    out
}

/// A fixed grid: no RNG, no clock, no viewport. A deviation reported here describes points another
/// machine would sample too.
fn grid() -> Vec<Vec3> {
    let mut out = Vec::new();
    for i in -4..=4 {
        for j in -4..=4 {
            for k in -4..=4 {
                out.push([i as f32 * 0.42, j as f32 * 0.37, k as f32 * 0.44]);
            }
        }
    }
    out
}

/// How far apart two `f32` evaluators may be before this stops counting as agreement. Loose on purpose:
/// it is a wrong-shape detector, not a precision claim - and `twist` is the one arm where a GPU's `sin`
/// and `cos` may legitimately differ from libm's, which is a shading nuance precisely because nothing on
/// this path is hashed.
const TOLERANCE: f32 = 1e-4;

fn main() {
    let points = grid();
    println!(
        "// A-07 agreement, per kind: WGSL master interpreter vs crates/render, {} points each",
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

    println!(" adapter          : {}", interpreter.adapter());
    println!(" kind             | max deviation | gpu vs cpu at worst | sentinels excluded");

    let mut failures: Vec<String> = Vec::new();
    let mut widest = (String::new(), 0.0f32);
    let mut count = 0usize;

    for (name, tree) in kinds() {
        count += 1;
        match interpreter.compare_with_reference(&tree, &points) {
            Ok(report) => {
                println!(
                    " {:>14} | {:>13.6} | {:.6} vs {:.6} | {}",
                    name,
                    report.max_abs_deviation,
                    report.gpu_at_worst,
                    report.cpu_at_worst,
                    report.sentinels
                );
                if report.max_abs_deviation > widest.1 {
                    widest = (name.to_string(), report.max_abs_deviation);
                }
                if report.max_abs_deviation > TOLERANCE {
                    failures.push(format!(
                        "{name}: {:.6} units at {:?} (gpu {:.6}, cpu {:.6})",
                        report.max_abs_deviation,
                        report.worst_point,
                        report.gpu_at_worst,
                        report.cpu_at_worst
                    ));
                }
            }
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }

    println!(
        " widest          : {} at {:.6} world units, over {count} kinds",
        widest.0, widest.1
    );
    if failures.is_empty() {
        println!(
            " agreement        : all {count} kinds within {TOLERANCE} units (presentation only; not gate P1.5, R4 or R1 evidence)"
        );
        return;
    }
    eprintln!(
        "FAIL: {} kind(s) diverged from the reference",
        failures.len()
    );
    for failure in failures {
        eprintln!("  {failure}");
    }
    std::process::exit(1);
}
