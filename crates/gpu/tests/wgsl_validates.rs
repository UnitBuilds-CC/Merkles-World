//! A-07's WGSL master interpreter, validated by the same front end `wgpu` would use - with no device
//! involved, which is the finding: this check runs on all three CI architectures, including the Linux
//! runner that reported no adapters at all.
//!
//! The teeth are the `a_shader_that_should_not_compile_is_refused` test. A validator that always returns
//! `Ok` would look identical to this one if nobody fed it a shader that has to fail, so a broken module goes
//! through the same path and must be refused, with the reason in hand.

use gpu::naga_validate::{declared_bindings, validate, ValidationError, INTERP};

/// The presentation half, handed to naga exactly as `wgpu` would hand it. This is the only place in the
/// repository that compiles `present.wgsl` on a machine with no adapter, which matters because the windowed
/// example is the only thing that ever compiles it at all — and every CI runner either has no device or
/// prints SKIP. A first draft of that shader named a constant by a name it no longer had and failed at
/// `create_shader_module` on a laptop while trunk stayed green.
const PRESENT: &str = include_str!("../src/present.wgsl");

#[test]
fn the_master_interpreter_validates_as_wgsl() {
    validate(INTERP).unwrap_or_else(|error| panic!("interp.wgsl was refused:\n{}", error.0));
}

#[test]
fn the_presentation_shader_validates_as_wgsl() {
    validate(PRESENT).unwrap_or_else(|error| panic!("present.wgsl was refused:\n{}", error.0));
}

#[test]
fn the_presenting_pass_has_the_two_bindings_it_declares() {
    // Two, not four: the render pass binds the extent block and the compute stage's own `traced` buffer, and
    // it is a *separate* bind group from the march's, so it is not competing for `interp.wgsl`'s four
    // storage slots. The order is the bind group's order — a silent insertion would have the fragment shader
    // read the uniform as an array of f32 and paint a plausible, wrong picture.
    assert_eq!(declared_bindings(PRESENT), vec![0, 1]);
    assert!(
        PRESENT.contains("@group(0) @binding(0) var<uniform> extent: Extent"),
        "the extent is a uniform, not a fifth storage buffer"
    );
    assert!(
        PRESENT.contains("@group(0) @binding(1) var<storage, read> depths: array<f32>"),
        "the fragment stage samples the march's own depth buffer, in place"
    );
}

#[test]
fn the_sky_test_is_the_declared_constant_and_not_a_literal() {
    // The no-surface threshold has one spelling in the shader. If a second `1.0e37`-looking literal appeared
    // in the fragment body, a pixel the host counts as a hit could be painted as sky, and the windowed
    // example's own verify step reads depths through the *host* rule — so the two would disagree in exactly
    // the direction that shows a hole in the picture rather than an error.
    assert!(
        PRESENT.contains("if (t >= MISS)"),
        "the shader must compare against the declared MISS constant"
    );
    let literals = PRESENT
        .lines()
        .filter(|line| !line.trim().starts_with("//"))
        .filter(|line| line.contains("1.0e"))
        .count();
    assert_eq!(
        literals, 1,
        "the miss threshold appears {literals} times outside comments; only the `const MISS` line may carry it",
    );
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

#[test]
fn the_full_screen_triangle_reaches_the_viewports_far_corner() {
    // The two off-screen corners are what make three vertices enough. The triangle through (-1,-1), (c-1,-1)
    // and (-1,c-1) is the region `x + y <= c - 2`, and the viewport square's far corner (1,1) is on
    // `x + y = 2`, so `2u` (clip 1.0, the original) leaves half of every frame unrasterized and `3u` (clip 2.0)
    // still leaves an eighth - 1536 and 384 of the 3072 centres at 64x48. Four is the smallest value that
    // reaches (1,1), which is the classic (-1,-1) (3,-1) (-1,3).
    //
    // The invisible part is why this needs a guard at all: an unrasterized pixel keeps the pass's clear colour,
    // and the window path clears to the shader's own sky, so the frame looks right, the screenshot looks right,
    // and the depth buffer `demo_window` verifies ray for ray is entirely correct. `examples/present_drawable.rs`
    // found the bug by clearing its witness pass to magenta and counting what was left; it then passed on the
    // `3u` fix, because the boot scene's surfaces (rows 6..=35, columns 12..=51) never reach the corner `3u`
    // still misses. This is the same rule with no device, because two of the three CI runners have no adapter
    // and the example prints SKIP there.
    let at = PRESENT
        .find("fn vs_full_screen")
        .expect("the presentation pass needs a full-screen triangle; vs_full_screen is gone");
    let body = &PRESENT[at..];
    let body = &body[..body.find('}').expect("vs_full_screen is unterminated")];
    assert!(
        body.contains("select(0u, 4u, index == 1u)") && body.contains("select(0u, 4u, index == 2u)"),
        "the full-screen triangle must reach clip 3.0 on both off-screen corners, or it leaves the viewport's \
         far corner undrawn: {body}"
    );
}

#[test]
fn every_stack_push_is_bounded_before_the_store() {
    // `eval_from` bounds the *read* of `stack[sp]` (`sp >= header.stack_depth || sp >= DEPTH`) and then, to
    // descend, stores `stack[sp + 1]`. The read bound does not cover that store: at `sp == DEPTH - 1` the walk
    // indexes one past `array<Frame, DEPTH>`, and WGSL leaves an out-of-bounds index undefined rather than
    // diagnosing it, so the only defence is a bound checked before the write. docs/04 records the hazard.
    //
    // The guard is a count plus an adjacency test, because both halves fail on a real mistake: a third push
    // site with no bound moves the count, and deleting one bound leaves a store whose nearest guard is the
    // read's, 40 lines up.
    let lines: Vec<&str> = INTERP.lines().collect();
    let guard = "sp + 1u >= DEPTH || sp + 1u >= header.stack_depth";
    let mut stores = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.contains("stack[sp] = Frame(") {
            let before = lines[..=i]
                .iter()
                .rposition(|l| l.contains(guard))
                .unwrap_or_else(|| {
                    panic!(
                        "line {} stores a frame with no bound above it: {line}",
                        i + 1
                    )
                });
            assert!(
                i - before <= 12,
                "the nearest push bound is {} lines above the store on line {}, so this store is not the one \
                 it guards: {line}",
                i - before,
                i + 1
            );
            stores.push(i);
        }
    }
    assert_eq!(
        stores.len(),
        2,
        "eval_from has exactly two push sites - the descent and the unwind's second child - and the count \
         moved to {}, which means a stack store exists that this guard cannot see",
        stores.len()
    );
}

#[test]
fn the_temporal_cache_has_four_storage_buffers_in_group_one() {
    // The culled camera entry point uses bind group 1 for temporal cache storage buffers. This keeps the
    // downlevel 4-storage-buffer limit intact for group 0 while adding desktop-only cache support.
    let cache_bindings = vec![
        "@group(1) @binding(0) var<storage, read_write> cached_positions",
        "@group(1) @binding(1) var<storage, read_write> cached_depths",
        "@group(1) @binding(2) var<storage, read_write> cached_normals",
        "@group(1) @binding(3) var<storage, read_write> cached_metadata",
    ];
    for binding in cache_bindings {
        assert!(
            INTERP.contains(binding),
            "temporal cache missing {binding:?} in interp.wgsl"
        );
    }
}

#[test]
fn the_culled_camera_entry_point_checks_cache_before_marching() {
    // The culled variant must check the temporal cache and dirty mask before performing a full march.
    assert!(
        INTERP.contains("march_rays_from_camera_culled"),
        "culled camera entry point is missing"
    );
    assert!(
        INTERP.contains("use_cache"),
        "culled entry point doesn't declare use_cache variable"
    );
    assert!(
        INTERP.contains("cached_depths[index]"),
        "culled entry point doesn't read cached depths"
    );
    assert!(
        INTERP.contains("dirty_mask"),
        "culled entry point doesn't check dirty mask for delta rendering"
    );
    assert!(
        INTERP.contains("is_dirty"),
        "culled entry point doesn't declare is_dirty variable for delta rendering"
    );
}
