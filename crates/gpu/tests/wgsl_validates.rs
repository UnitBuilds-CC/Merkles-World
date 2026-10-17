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
    // Six declared bindings, and only four of them are storage buffers: nodes, pool, queries and distances
    // are the stage's whole storage budget under `Limits::downlevel_defaults()`, which is why the header
    // arrived as a uniform at 3 and the camera block arrived as a uniform at 5 rather than as a fifth
    // storage buffer. The order is the bind group's order, and a silent insertion between two of them would
    // have the shader read the wrong buffer - the class of mistake that produces a plausible picture rather
    // than an error.
    assert_eq!(declared_bindings(INTERP), vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn camera_bytes_is_the_uniform_layout() {
    // `write_camera` fills a 96-byte block by row offset and `interp.wgsl` reads it by field name, so the
    // two files share a layout that neither checks against a schema. The rows are the ones a uniform block
    // forces - each `vec3` is aligned to 16, so four direction rows, then geometry, then the grid - and a
    // reordering on either side silently reinterprets every camera ever written.
    //
    // This is a text guard rather than a device one on purpose: the Linux and Windows CI runners have no
    // adapter, so the example that would catch a fused read-write disagreement (`r1_device_rays`) prints
    // SKIP there, while the field order it depends on is checkable without a GPU.
    let start = INTERP
        .find("struct CameraUniform {")
        .expect("the camera entry point reads a CameraUniform; the struct is gone");
    let body = &INTERP[start..];
    let body = &body[..body.find('}').expect("CameraUniform is unterminated")];
    let mut last = 0usize;
    for field in [
        "origin: vec3<f32>",
        "right: vec3<f32>",
        "up: vec3<f32>",
        "forward: vec3<f32>",
        "geom: vec4<f32>",
        "grid: vec4<f32>",
    ] {
        let at = body
            .find(field)
            .unwrap_or_else(|| panic!("{field} is not a field of CameraUniform: {body}"));
        assert!(
            at > last,
            "{field} is out of order in CameraUniform, and the host writes it by row offset"
        );
        last = at;
    }
    // The six rows are 16 bytes each: four direction rows at 0/16/32/48, geometry at 64, grid at 80, for the
    // 96-byte block `write_camera` asserts it wrote. `vec3` aligns to 16 in a uniform block, which is what
    // makes the row pitch 16 rather than 12 and what the third word of the first row is `root` for.
    assert!(
        INTERP.contains("@group(0) @binding(5) var<uniform> cam: CameraUniform"),
        "the camera has to be a uniform at binding 5; a fifth storage buffer is a validation error downlevel"
    );
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
