//! The first pixels: a CPU raster probe over the marched field.
//!
//! What this is for. docs/04's cost story - naive march versus a BVH, the 14 ms frame, gates R1 and R2 -
//! has been arithmetic on guesses, because nothing in this repository has ever turned a tree into an
//! image. This module does that on a CPU, so the renderer's numbers stop being per-ray anecdotes
//! (`scene_steps` measures one ray at a time) and become a picture with a measured hit fraction and step
//! charge across a grid of rays.
//!
//! What it is not, stated because docs/04 is emphatic about it and a demo must not quietly disagree with
//! its own gate table: **this is not R1.** Steps per ray on a CPU core are not nanoseconds per frame on
//! a GPU; there is no WGSL interpreter, no BVH and no culling here. And the colour is a normal-to-rgb map
//! rather than a material model - `material_eval` and lighting are OQ-10's open question, so inventing
//! shading here would answer a question the project has not answered. Treat the output as geometry
//! evidence: if a shape changes and the pixels do not, something between the field and the frame broke.
//!
//! Coordinates are plain `f32` world units and nothing here is hashed, which is the licence docs/02's
//! float discipline gives presentation values. The camera is a pinhole with an explicit image-plane
//! half-height rather than a field angle, so no trigonometry enters ray generation at all - not because
//! floats are forbidden in a renderer but because a square root is a smaller thing to be wrong about
//! than a tangent, and every number a ray depends on here is one the tests can recompute.

use crate::{march, normal, Tree, Vec3, ROOT};

/// Where the probe looks from. `forward`, `right` and `up` are normalised by [`render`], so a caller can
/// pass a convenient basis without orthonormalising it by hand - though a non-perpendicular basis still
/// produces a sheared image, and that is the caller's geometry rather than this code's.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub origin: Vec3,
    pub forward: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    /// Image half-height at unit distance. `0.5` across a square viewport is a 90-degree-ish vertical
    /// span without ever naming an angle.
    pub half_height: f32,
    pub width: u32,
    pub height: u32,
    /// March range in world units, the same `max_t` the single-ray helper takes.
    pub max_t: f32,
    /// Surface epsilon. An input rather than a constant for docs/04's reason: a fixed absolute epsilon is
    /// exactly what stops a marcher resolving detail as the camera pulls back.
    pub surface: f32,
    pub max_steps: u32,
}

/// A rendered probe: colour per pixel plus the distance each surface was found at.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Row-major, top row first, three bytes per pixel - the layout a PPM wants, so `examples` can hand
    /// the buffer straight to a file without a conversion step to get wrong.
    pub pixels: Vec<u8>,
    /// `t` along the ray, or [`Frame::NO_HIT`] where the ray spent its budget without meeting surface.
    pub depth: Vec<f32>,
}

impl Frame {
    /// The depth written for a pixel that found no surface.
    pub const NO_HIT: f32 = f32::MAX;

    #[inline]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let at = ((y * self.width + x) as usize) * 3;
        [self.pixels[at], self.pixels[at + 1], self.pixels[at + 2]]
    }

    #[inline]
    pub fn depth_at(&self, x: u32, y: u32) -> f32 {
        self.depth[(y * self.width + x) as usize]
    }

    /// Pixels that found a surface. The fraction of these is the thing a BVH is supposed to make cheap.
    pub fn hit_count(&self) -> u32 {
        self.depth.iter().filter(|&&t| t != Frame::NO_HIT).count() as u32
    }
}

/// What a render cost, in ray counts rather than milliseconds - timings belong to the example, because
/// the same numbers on a different machine are a different story.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub rays: u32,
    pub hits: u32,
    /// Distance-field evaluations. Exact for a hit (`steps` counts the iterations that missed, and the
    /// iteration that hits is one more). For a miss it charges the full step budget, which is an upper
    /// bound - `march` can also exit on range - and the bound is tightest precisely on the miss-heavy
    /// grids that culling exists to fix.
    pub evaluations: u64,
}

/// March every pixel of `camera`'s grid against `tree` and shade what comes back.
pub fn render(tree: &Tree, camera: &Camera) -> (Frame, Stats) {
    let forward = unit(camera.forward);
    let right = unit(camera.right);
    let up = unit(camera.up);
    let aspect = camera.width as f32 / camera.height as f32;
    // The gradient taps need to be far enough apart that the four `f32` distances differ; gate R5's own
    // measurement is the reason this is a multiple of the surface epsilon rather than the epsilon itself.
    let normal_eps = camera.surface * 16.0;
    let rays = camera.width * camera.height;
    let mut pixels = Vec::with_capacity((rays as usize) * 3);
    let mut depth = Vec::with_capacity(rays as usize);
    let mut hits = 0u32;
    let mut evaluations = 0u64;

    for py in 0..camera.height {
        // Top row first: pixel (0, 0) is the upper-left, which is what a PPM expects.
        let v = (py as f32 + 0.5) / camera.height as f32;
        let ndc_y = (1.0 - 2.0 * v) * camera.half_height;
        for px in 0..camera.width {
            let u = (px as f32 + 0.5) / camera.width as f32;
            let ndc_x = (2.0 * u - 1.0) * camera.half_height * aspect;
            let dir = unit([
                forward[0] + right[0] * ndc_x + up[0] * ndc_y,
                forward[1] + right[1] * ndc_x + up[1] * ndc_y,
                forward[2] + right[2] * ndc_x + up[2] * ndc_y,
            ]);
            match march(
                tree,
                ROOT,
                camera.origin,
                dir,
                camera.max_t,
                camera.surface,
                camera.max_steps,
            ) {
                Some(hit) => {
                    hits += 1;
                    evaluations += u64::from(hit.steps) + 1;
                    let n = normal(tree, ROOT, hit.point, normal_eps);
                    push_rgb(&mut pixels, normal_to_rgb(n));
                    depth.push(hit.t);
                }
                None => {
                    evaluations += u64::from(camera.max_steps);
                    push_rgb(&mut pixels, sky(dir));
                    depth.push(Frame::NO_HIT);
                }
            }
        }
    }

    (
        Frame {
            width: camera.width,
            height: camera.height,
            pixels,
            depth,
        },
        Stats {
            rays,
            hits,
            evaluations,
        },
    )
}

fn unit(v: Vec3) -> Vec3 {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length == 0.0 {
        return [0.0, 0.0, 0.0];
    }
    [v[0] / length, v[1] / length, v[2] / length]
}

fn push_rgb(out: &mut Vec<u8>, rgb: [u8; 3]) {
    out.push(rgb[0]);
    out.push(rgb[1]);
    out.push(rgb[2]);
}

/// Surface orientation as colour, and nothing more: the material half of A-07's interpreter signature is
/// OQ-10's unsettled question, so this maps each normal component from `[-1, 1]` into `0..=255` and stops.
fn normal_to_rgb(n: Vec3) -> [u8; 3] {
    [quantise(n[0]), quantise(n[1]), quantise(n[2])]
}

fn quantise(component: f32) -> u8 {
    // A unit normal's components are in [-1, 1] by construction; the clamp is for arithmetic that says
    // 1.0000001, not for a case the geometry can produce.
    ((component.clamp(-1.0, 1.0) + 1.0) * 127.5) as u8
}

/// The placeholder background: darker toward the bottom of the frame, a function of the ray direction
/// only, so a miss depends on nothing that could disagree with another machine.
fn sky(dir: Vec3) -> [u8; 3] {
    let upness = (dir[2] + 1.0) * 0.5;
    let band = (20.0 + 90.0 * upness) as u8;
    [band / 3, band / 2, band]
}
