//! A-07's WGSL master interpreter, validated by the same front end `wgpu` would use - with no device
//! involved, which is the finding: this check runs on all three CI architectures, including the Linux
//! runner that reported no adapters at all.
//!
//! The teeth are the second test. A validator that always returns `Ok` would look identical to this one
//! if nobody fed it a shader that has to fail, so a broken module goes through the same path and must be
//! refused, with the reason in hand.

use gpu::naga_validate::{declared_bindings, validate, ValidationError, INTERP};

#[test]
fn the_master_interpreter_validates_as_wgsl() {
    validate(INTERP).unwrap_or_else(|error| panic!("interp.wgsl was refused:\n{}", error.0));
}

#[test]
fn a_shader_that_should_not_compile_is_refused() {
    // Reading past the declared array length is a type error in WGSL, and calling an undefined
    // function is a resolution error. Either one is enough to prove the validator is running.
    let broken = "
struct Node { a: u32, b: u32, c: u32, d: u32 }
@group(0) @binding(0) var<storage, read> nodes: array<Node>;
@compute @workgroup_size(64)
fn evaluate_points() {
    let bad: f32 = nodes[0].a;
    let also_bad = not_a_real_function(bad);
}
";
    let result = validate(broken);
    match result {
        Ok(()) => panic!("a broken shader validated, so the check above proves nothing"),
        Err(ValidationError(message)) => assert!(
            !message.is_empty(),
            "refused, but with no explanation to put in a log"
        ),
    }
}

#[test]
fn the_bindings_the_cpu_side_would_upload_are_the_ones_declared() {
    // Five storage buffers: nodes, pool, queries, header, distances. The order is the bind group's
    // order, and a silent insertion between two of them would have the shader read the wrong buffer -
    // the class of mistake that produces a plausible picture rather than an error.
    assert_eq!(declared_bindings(INTERP), vec![0, 1, 2, 3, 4]);
}

#[test]
fn the_interpreter_covers_the_kinds_the_demo_scene_needs() {
    // The engine's boot scene is a union of a rounded box and a smooth union of two spheres, so the
    // shader has to know about those four things before a device comparison means anything.
    for kind in [
        "NODE_SPHERE",
        "NODE_ROUNDED_BOX",
        "NODE_PLANE",
        "NODE_UNION",
        "NODE_SMOOTH_UNION",
    ] {
        assert!(
            INTERP.contains(&format!("const {kind}:")),
            "{kind} is missing from the interpreter"
        );
    }
    assert!(
        INTERP.contains("fn eval_from"),
        "the stack machine that replaces WGSL's missing recursion is the point of the file"
    );
}

#[test]
fn the_blend_formula_matches_the_reference_rather_than_the_literature() {
    // The device check in examples/agreement.rs is the real gate, but it can only run where a device
    // exists - and the Linux runner has none. A wrong blend radius formula is visible in the source, so
    // the regression that actually happened gets a text guard too: the first draft used the cubic smin
    // (h^3, k/6) that circulates in the SDF literature, while crates/render::smin is quadratic (h^2,
    // k/4). Two f32 evaluators of "the same" smooth union differed by 0.042 world units, about 4% of a
    // voxel, on a point inside the blend region.
    assert!(
        INTERP.contains("h * h * k * 0.25"),
        "the shader must use the reference quadratic blend, not another valid-looking one"
    );
    assert!(
        !INTERP.contains("1.0 / 6.0"),
        "the cubic blend is the wrong implementation of this contract"
    );
    assert!(
        INTERP.contains("k <= 0.0"),
        "a zero blend radius has to collapse to the plain min on both sides"
    );
}
