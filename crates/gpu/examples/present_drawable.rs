//! The picture the presentation pass *drew*, read back off the device and compared to the reference march.
//!
//! `cargo run --release -p gpu --example present_drawable`
//!
//! `demo_window` verifies its frame ray for ray: `read_camera_depths` hands the host the depth buffer and every
//! pixel is compared against `render::march` through the same camera law. That is a strong check, and it has a
//! hole in it. The depth buffer's row order is pinned by it, but the *drawable*'s is chosen by `present.wgsl`:
//! the fragment stage decides which buffer row lands on which screen row, and a flip there paints an upside-down
//! world while the depth buffer it sampled stays perfectly correct. Every assertion on trunk would pass. Until
//! this example, the only witness for that class of mistake was a screenshot somebody looked at.
//!
//! The instrument is the same pass with a different target: `Present::read_drawable` draws the full-screen
//! triangle into an `Rgba8Unorm` texture this call owns — copyable, and linear, so a shader's `0.02` is the host's
//! `5` — and maps the bytes. Six asserts, over integers and exact bytes only (ADR-017: no duration on this
//! path is gated, and none is printed as a budget):
//!
//! 1. **Coverage**: no pixel still holds the witness pass's own clear colour, so the fragment stage ran for
//!    every pixel centre in the drawable.
//! 2. **Membership agrees**: the drawable's non-sky pixel count equals the reference march's hit count.
//! 3. **Every miss is exactly the sky triple**: a pixel the reference finds nothing in is byte-for-byte the
//!    shader's own background, which is what makes 2 a statement about the surface set rather than about noise.
//! 4. **The landmark box agrees**: the topmost, bottommost, leftmost and rightmost rows and columns containing a
//!    surface are the same on both sides. This is the check the depth read-back cannot make: a single flip in the
//!    fragment stage moves the drawable's topmost surface row to the reference's bottom.
//! 5. **The picture is not vacuous**: the reference must find surfaces, or 2 to 4 are an empty-window pass.
//! 6. **The ramp is the ramp**: each hit pixel sits within one 8-bit step of the distance the reference marched,
//!    so the drawable is not merely the right shape in the right orientation but the right *picture*.
//!
//! 1 exists because 2 to 4 turned out not to imply it. The first version of this witness found the demo's
//! frame missing an eighth of itself (a `2u` clip corner covers only `x + y <= 0`, half the square) and
//! reported it, then a `3u` fix made every assert above pass while 384 of 3072 pixel centres were still
//! never rasterized — because the boot scene's surfaces span rows 6..=35 and columns 12..=51, and the missing
//! region is the far corner. An empty patch of a sky-coloured clear is not distinguishable from sky, so
//! coverage had to be made visible on its own terms: clear the witness pass to a colour the shader cannot
//! emit, and count what is left.

use assetc::watch::{Outcome, Watch};
use gpu::device::{GpuCamera, Interpreter};
use render::{march, Tree, ROOT};

/// `width * 4` has to be a multiple of `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT` (256) or the copy needs padded
/// rows the host would have to skip; 64 is the smallest such width with a grid big enough to have a shape in it.
const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
/// The demo's camera law, the one `demo_window` presents and `r1_specialized_frame` times.
const HALF_HEIGHT: f32 = 0.5;
const MAX_T: f32 = 40.0;
const SURFACE: f32 = 1.0e-3;
const MAX_STEPS: u32 = 96;
/// `present.wgsl`'s own colours, and the only reason a byte-exact claim about them can be made here: a
/// non-sRGB 8-bit target stores `round(value * 255)`, so these three triples are integers on both sides.
const SKY: [u8; 3] = [5, 8, 13];
/// `read_drawable`'s own background, chosen because the shader cannot emit it (its red channel tops out at
/// 235 and its green never falls below 56). A pixel still holding this triple was never visited by the
/// fragment stage, which is the one failure mode a sky-coloured clear hides.
const UNVISITED: [u8; 3] = [255, 0, 255];
const NEAR: [f32; 3] = [0.92, 0.93, 0.95];
const FAR: [f32; 3] = [0.18, 0.22, 0.30];

fn boot_scene() -> Tree {
    let input = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../assetc/tests/fixtures/boot_scene.ron");
    let input = std::fs::canonicalize(&input)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", input.display()));
    let output = std::env::temp_dir().join(format!(
        "merkles_present_drawable_{}.bin",
        std::process::id()
    ));
    let mut watch = Watch::default();
    let outcome = watch.observe(
        input.to_str().expect("the fixture path is utf-8"),
        output.to_str().expect("the temp path is utf-8"),
    );
    if let Outcome::Failed(reason) = outcome {
        panic!("{} must compile: {reason}", input.display());
    }
    let bytes = std::fs::read(&output).expect("the compiler renamed its asset into place");
    let loaded = assetc::reader::from_bytes(&bytes)
        .unwrap_or_else(|error| panic!("{} must load: {error:?}", output.display()));
    let tree = Tree {
        nodes: loaded.nodes,
        params: loaded.params,
    };
    render::validate(&tree).expect("the demo scene must pass the validator the march requires");
    let _ = std::fs::remove_file(&output);
    tree
}

fn camera(width: u32, height: u32) -> GpuCamera {
    let v = [0.0f32, -0.1, 1.0];
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    GpuCamera {
        origin: [0.0f32, 0.6, -4.0],
        right: [1.0, 0.0, 0.0],
        up: [0.0, 1.0, 0.0],
        forward: [v[0] / l, v[1] / l, v[2] / l],
        half_height: HALF_HEIGHT,
        max_t: MAX_T,
        surface: SURFACE,
        max_steps: MAX_STEPS,
        root: ROOT,
        width,
        height,
    }
}

/// The pinhole `march_rays_from_camera` derives, restated host side: `v` grows downward from the top, and
/// `ndc_y` is `(1 - 2v) * half_height`, so buffer row 0 is the top of the picture.
fn direction(px: u32, py: u32, width: u32, height: u32, cam: &GpuCamera) -> [f32; 3] {
    let aspect = width as f32 / height as f32;
    let u = (px as f32 + 0.5) / width as f32;
    let v = (py as f32 + 0.5) / height as f32;
    let ndc_x = (2.0 * u - 1.0) * HALF_HEIGHT * aspect;
    let ndc_y = (1.0 - 2.0 * v) * HALF_HEIGHT;
    let span = [
        cam.forward[0] + cam.right[0] * ndc_x + cam.up[0] * ndc_y,
        cam.forward[1] + cam.right[1] * ndc_x + cam.up[1] * ndc_y,
        cam.forward[2] + cam.right[2] * ndc_x + cam.up[2] * ndc_y,
    ];
    let l = (span[0] * span[0] + span[1] * span[1] + span[2] * span[2]).sqrt();
    [span[0] / l, span[1] / l, span[2] / l]
}

fn main() {
    let interpreter = match Interpreter::open_hardware() {
        Ok(interpreter) => interpreter,
        Err(error) => {
            println!(
                "SKIP present_drawable: no device ({error:?}) - this is the render stage's own read-back, and \
                 a software rasteriser has no drawable to map"
            );
            return;
        }
    };
    let tree = boot_scene();
    let shape = interpreter.upload(&tree);
    let cam = camera(WIDTH, HEIGHT);

    let mut frame = interpreter.begin_camera_frame(WIDTH, HEIGHT);
    interpreter.write_camera(&mut frame, &cam);
    let bind = interpreter.bind_camera(&shape, &frame);
    let read = interpreter
        .march_camera_frame(&bind, &frame)
        .expect("the device must march the frame this witness draws");
    // The pass is begun with the *witness's* format, not a surface's: the pipeline's colour target has to match
    // the texture `read_drawable` creates, and that texture is the only way to get the drawable's bytes back.
    let present = interpreter.begin_presentation(
        &frame,
        wgpu::TextureFormat::Rgba8Unorm,
        WIDTH,
        HEIGHT,
        MAX_T,
    );
    let (drawable, stride) = present
        .read_drawable(WIDTH, HEIGHT)
        .expect("the render pass must draw into a texture the host can copy");

    // The reference, walked once, in the same order the buffer stores.
    let mut expected = vec![None; (WIDTH * HEIGHT) as usize];
    for py in 0..HEIGHT {
        for px in 0..WIDTH {
            let dir = direction(px, py, WIDTH, HEIGHT, &cam);
            expected[py as usize * WIDTH as usize + px as usize] =
                march(&tree, ROOT, cam.origin, dir, MAX_T, SURFACE, MAX_STEPS).map(|hit| hit.t);
        }
    }
    let reference_hits = expected.iter().filter(|t| t.is_some()).count();
    // 5. A sky-only grid would make every landmark claim below true of a blank window.
    assert!(
        reference_hits > 0,
        "the {WIDTH}x{HEIGHT} grid found 0 surfaces, so this witness would pass on an empty image"
    );

    let sky = |px: u32, py: u32| {
        let at = py as usize * stride as usize + px as usize * 4;
        drawable[at] == SKY[0] && drawable[at + 1] == SKY[1] && drawable[at + 2] == SKY[2]
    };
    let mut unvisited = 0usize;
    let mut shown = 0usize;
    let mut wrong_sky = 0usize;
    let mut off_ramp = 0usize;
    let mut top = HEIGHT;
    let mut bottom = 0u32;
    let mut left = WIDTH;
    let mut right = 0u32;
    for py in 0..HEIGHT {
        for px in 0..WIDTH {
            let at = py as usize * stride as usize + px as usize * 4;
            if drawable[at] == UNVISITED[0]
                && drawable[at + 1] == UNVISITED[1]
                && drawable[at + 2] == UNVISITED[2]
            {
                // 1. A centre still holding the pass's own background is one the fragment stage never saw, so
                // no claim below about sky, membership or the ramp can describe it. Counted separately.
                unvisited += 1;
                continue;
            }
            let hit = expected[py as usize * WIDTH as usize + px as usize];
            match (hit, sky(px, py)) {
                (None, false) => wrong_sky += 1,
                (None, true) => {}
                (Some(_), false) => {
                    shown += 1;
                    top = top.min(py);
                    bottom = bottom.max(py);
                    left = left.min(px);
                    right = right.max(px);
                }
                (Some(_), true) => {}
            }
            if let Some(t) = hit {
                // 6. The ramp, one 8-bit step wide: the quantisation is `round(v * 255)` on both sides, so a
                // difference of 1 is the format and a difference of 2 is the shader.
                let k = (t / MAX_T).clamp(0.0, 1.0);
                for channel in 0..3 {
                    let wanted = (NEAR[channel] + (FAR[channel] - NEAR[channel]) * k) * 255.0;
                    let wanted = wanted.round().min(255.0) as i32;
                    let got = drawable[at + channel] as i32;
                    if (got - wanted).abs() > 1 {
                        off_ramp += 1;
                    }
                }
            }
        }
    }
    // The same landmarks, measured out of the reference set rather than written down: nothing here is a
    // remembered number, and a flip or a transpose moves them apart.
    let mut ref_top = HEIGHT;
    let mut ref_bottom = 0u32;
    let mut ref_left = WIDTH;
    let mut ref_right = 0u32;
    for py in 0..HEIGHT {
        for px in 0..WIDTH {
            if expected[py as usize * WIDTH as usize + px as usize].is_some() {
                ref_top = ref_top.min(py);
                ref_bottom = ref_bottom.max(py);
                ref_left = ref_left.min(px);
                ref_right = ref_right.max(px);
            }
        }
    }

    // 1.
    assert_eq!(
        unvisited,
        0,
        "{unvisited} of {} pixel centres still hold the witness pass's own background, so the fragment stage \
         never ran for them: the full-screen triangle does not cover the viewport",
        WIDTH as usize * HEIGHT as usize,
    );
    // 2 and 3.
    assert_eq!(
        shown, reference_hits,
        "the drawable shows {shown} surface pixels where the reference march finds {reference_hits} over the \
         same {WIDTH}x{HEIGHT} grid"
    );
    assert_eq!(
        wrong_sky, 0,
        "{wrong_sky} pixels the reference calls sky are not the shader's own background colour"
    );
    // 4.
    assert_eq!(
        (top, bottom, left, right),
        (ref_top, ref_bottom, ref_left, ref_right),
        "the drawable's surfaces span rows {top}..={bottom} and columns {left}..={right} while the reference \
         march spans rows {ref_top}..={ref_bottom} and columns {ref_left}..={ref_right} - the buffer is right \
         and the presentation pass is drawing it in the wrong direction"
    );
    // 5.
    assert_eq!(
        off_ramp, 0,
        "{off_ramp} channel values sit more than one 8-bit step from the ramp the reference distance predicts"
    );

    println!(
        "// drawable grid    : {WIDTH}x{HEIGHT}, {} bytes per row, Rgba8Unorm, the same pass the window draws",
        stride
    );
    println!(
        "// coverage         : {unvisited} of {} pixel centres left unrasterized",
        WIDTH as usize * HEIGHT as usize
    );
    println!(
        "// drawn from     : the compute stage's own depth buffer, sampled in place by the fragment stage and \
         copied back once for this witness ({} depths, {} surfaces)",
        read.depths.len(),
        read.hits
    );
    println!(
        "// agrees with    : the reference march on {reference_hits} surface pixels, and on the box they span: \
         rows {ref_top}..={ref_bottom}, columns {ref_left}..={ref_right}"
    );
    println!(
        "// what this lands : the drawable, not just the buffer it samples: orientation and colour are checked \
         as bytes, which is the one thing `demo_window`'s depth verification cannot see"
    );
}
