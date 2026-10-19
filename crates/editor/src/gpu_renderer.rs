//! CPU renderer for the editor viewport preview.
//!
//! The editor is a tool (§3's hot-path bans do not apply to it), so the CPU raster is an acceptable
//! instrument for judging a shape until the GPU pass can run inside eframe — `gpu::Interpreter` owns
//! its own device today, and eframe already owns one.

use assetc::EntityDef;
use entity::MaterialRegistry;
use render::{
    march, normal,
    raster::{Camera, Frame},
    Tree, ROOT,
};

/// CPU renderer that converts entity shapes to pixel previews.
pub struct CpuRenderer {
    width: u32,
    height: u32,
}

impl CpuRenderer {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Render the current entity shape to pixels from an orbit camera, with no ground.
    pub fn render(
        &self,
        entity_def: &EntityDef,
        materials: &MaterialRegistry,
        camera_yaw: f32,
        camera_pitch: f32,
        camera_distance: f32,
    ) -> Frame {
        self.render_on_stage(
            entity_def,
            materials,
            camera_yaw,
            camera_pitch,
            camera_distance,
            None,
        )
    }

    /// As [`Self::render`], composited over a ground plane at `stage.y`.
    ///
    /// The stage is marched, not drawn on top: a grid painted over the image cannot be occluded by the
    /// entity, so it would read as a decal rather than as a floor the pig stands on.
    pub fn render_on_stage(
        &self,
        entity_def: &EntityDef,
        materials: &MaterialRegistry,
        camera_yaw: f32,
        camera_pitch: f32,
        camera_distance: f32,
        stage: Option<Stage>,
    ) -> Frame {
        let camera = create_orbit_camera(
            camera_yaw,
            camera_pitch,
            camera_distance,
            self.width,
            self.height,
        );

        // A refused lowering draws nothing rather than a guess: an invented sphere in the viewport
        // would be judged as if it were the authored shape.
        let tree = match assetc::lower(&entity_def.shape) {
            Ok(asset) => asset.tree,
            Err(refusal) => {
                eprintln!(
                    "✗ preview refused by assetc: {refusal} at {}",
                    refusal.location()
                );
                return refused_frame(self.width, self.height);
            }
        };

        let start = std::time::Instant::now();
        let frame = render_frame(&tree, &camera, stage, tint_of(entity_def, materials));
        let elapsed = start.elapsed();

        if elapsed.as_millis() > 100 {
            eprintln!(
                "⚠ CPU render took {}ms for {}x{} ({} nodes)",
                elapsed.as_millis(),
                self.width,
                self.height,
                tree.nodes.len()
            );
        }

        frame
    }

    /// Resize the renderer.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
    }
}

/// A ground plane to stand the entity on, so scale and contact are legible.
#[derive(Debug, Clone, Copy)]
pub struct Stage {
    /// Height of the plane.
    pub y: f32,
    /// Grid spacing in world units.
    pub spacing: f32,
    /// Beyond this radius from the world origin the plane fades to sky, so it reads as a platform
    /// rather than an infinite floor.
    pub radius: f32,
}

impl Stage {
    /// A floor at the lowest surface the entity actually reaches, so its feet meet the grid.
    ///
    /// Two shortcuts were tried and are wrong. The enclosure (`render::cull::scene_enclosure`) bounds a
    /// subtree by a *sphere*, and for the pig that sphere's bottom is ~0.2 below the hooves, which reads
    /// as the animal floating over its own shadow. Computing the bottom from the authored parameters
    /// instead can't answer for `Mirror`/`Repeat` (the leaf values don't speak for the transformed
    /// child), and chaining each `SmoothUnion`'s `k` into the minimum overshoots by every level the foot
    /// passes through on the way to the root — where the other operand is far enough away that `smin`
    /// contributes nothing at all.
    ///
    /// So: measure. A column traced upward from under the enclosure finds the underside of whatever solid
    /// the tree actually defines, transformed nodes and all. The tree has already been lowered once for
    /// the frame; this runs only when the shape changes.
    pub fn under(entity_def: &EntityDef) -> Stage {
        let y = assetc::lower(&entity_def.shape)
            .ok()
            .and_then(|asset| match render::cull::scene_enclosure(&asset.tree) {
                render::cull::Enclosure::Bounded { centre, radius } => {
                    lowest_surface(&asset.tree, centre, radius)
                }
                render::cull::Enclosure::Unbounded => None,
            })
            .unwrap_or(-0.5);

        Stage {
            y,
            spacing: 0.25,
            radius: 1.6,
        }
    }
}

/// The lowest `y` reachable by casting one ray straight up per column of a grid over the enclosure's
/// footprint, and taking the nearest underside. `None` if no column hits, which is the shape's own
/// business — the caller falls back to the enclosure.
fn lowest_surface(tree: &Tree, centre: [f32; 3], radius: f32) -> Option<f32> {
    const COLUMNS: u32 = 24;
    // Start clear of the enclosure, so the trace begins outside a solid that is entirely within it.
    let floor = centre[1] - radius - 1.0;
    let span = 2.0 * radius;

    let mut best = None::<f32>;
    for i in 0..COLUMNS {
        let fx = i as f32 / (COLUMNS - 1) as f32;
        let x = centre[0] - radius + fx * span;
        for j in 0..COLUMNS {
            let fz = j as f32 / (COLUMNS - 1) as f32;
            let z = centre[2] - radius + fz * span;
            // Only columns inside the footprint; the corners of the square can't hold surface anyway.
            let dx = x - centre[0];
            let dz = z - centre[2];
            if dx * dx + dz * dz > radius * radius {
                continue;
            }

            if let Some(hit) = march(
                tree,
                ROOT,
                [x, floor, z],
                [0.0, 1.0, 0.0],
                2.0 + span,
                1e-3,
                96,
            ) {
                if best.is_none_or(|low| hit.point[1] < low) {
                    best = Some(hit.point[1]);
                }
            }
        }
    }
    best
}

/// A frame that says out loud that nothing was marched.
fn refused_frame(width: u32, height: u32) -> Frame {
    let mut frame = Frame::new(width, height);
    for _ in 0..width * height {
        frame.pixels.extend_from_slice(&Frame::NOT_DRAWN_RGB);
        frame.depth.push(Frame::NOT_DRAWN_DEPTH);
    }
    frame
}

/// Create an orbit camera looking at the origin.
fn create_orbit_camera(yaw: f32, pitch: f32, distance: f32, width: u32, height: u32) -> Camera {
    let position = [
        distance * pitch.cos() * yaw.sin(),
        distance * pitch.sin(),
        distance * pitch.cos() * yaw.cos(),
    ];

    let forward = normalize([-position[0], -position[1], -position[2]]);
    let right = normalize(cross(forward, [0.0, 1.0, 0.0]));
    let up = normalize(cross(right, forward));

    Camera {
        origin: position,
        forward,
        right,
        up,
        half_height: 0.5,
        width,
        height,
        max_t: 40.0,
        surface: 1e-3,
        max_steps: 96,
    }
}

/// The key light, carried with the camera and offset up and to one side. A fixed world light left the
/// face — which points away from it — at ambient, so the front view read as a black silhouette.
fn key_light(camera: &Camera) -> [f32; 3] {
    normalize([
        -camera.forward[0] + camera.up[0] * 0.75 - camera.right[0] * 0.45,
        -camera.forward[1] + camera.up[1] * 0.75 - camera.right[1] * 0.45,
        -camera.forward[2] + camera.up[2] * 0.75 - camera.right[2] * 0.45,
    ])
}

/// The entity's material as a per-channel multiplier on the shaded pixel.
///
/// Only `base_color` is read. Doc 04's materials are functions of the surface point, and the noise
/// term is not evaluated here, so the viewport shows the flat base colour and nothing about the
/// pattern — the swatch and the render agree, and neither yet speaks for the authored `noise`.
fn tint_of(entity_def: &EntityDef, materials: &MaterialRegistry) -> [f32; 3] {
    match materials.get(entity_def.material_id) {
        Some(m) => m.base_color,
        None => {
            eprintln!(
                "⚠ material slot {} is empty; rendering unlit white — assign a registered material",
                entity_def.material_id
            );
            [1.0, 1.0, 1.0]
        }
    }
}

/// Render a tree with the given camera, optionally over a [`Stage`].
fn render_frame(tree: &Tree, camera: &Camera, stage: Option<Stage>, tint: [f32; 3]) -> Frame {
    let mut frame = Frame::new(camera.width, camera.height);
    let light = key_light(camera);
    let aspect = camera.width as f32 / camera.height as f32;

    for y in 0..camera.height {
        for x in 0..camera.width {
            let px = (x as f32 / camera.width as f32 - 0.5) * aspect * camera.half_height * 2.0;
            let py = (0.5 - y as f32 / camera.height as f32) * camera.half_height * 2.0;

            let dir = normalize([
                camera.forward[0] + camera.right[0] * px + camera.up[0] * py,
                camera.forward[1] + camera.right[1] * px + camera.up[1] * py,
                camera.forward[2] + camera.right[2] * px + camera.up[2] * py,
            ]);

            let hit = march(
                tree,
                ROOT,
                camera.origin,
                dir,
                camera.max_t,
                camera.surface,
                camera.max_steps,
            );
            let ground = stage.and_then(|stage| stage_hit(camera, &dir, stage));

            // Whichever of the two is nearer owns the pixel. Only the entity carries a material:
            // the stage is the editor's measuring grid, not a thing in the world.
            let (t, rgb) = match (hit.as_ref(), ground.as_ref()) {
                (Some(hit), Some(ground)) => {
                    if hit.t <= ground.t {
                        let n = normal(tree, ROOT, hit.point, camera.surface);
                        (hit.t, tinted(shade_at(n, light), tint))
                    } else {
                        (ground.t, [ground.shade; 3])
                    }
                }
                (Some(hit), None) => {
                    let n = normal(tree, ROOT, hit.point, camera.surface);
                    (hit.t, tinted(shade_at(n, light), tint))
                }
                (None, Some(ground)) => (ground.t, [ground.shade; 3]),
                (None, None) => {
                    frame.pixels.extend_from_slice(&[30, 30, 35]);
                    frame.depth.push(Frame::NO_HIT);
                    continue;
                }
            };

            frame.pixels.extend_from_slice(&rgb);
            frame.depth.push(t);
        }
    }

    frame
}

/// Apply a material's base colour to a lambert shade.
fn tinted(shade: u8, tint: [f32; 3]) -> [u8; 3] {
    [
        (shade as f32 * tint[0]) as u8,
        (shade as f32 * tint[1]) as u8,
        (shade as f32 * tint[2]) as u8,
    ]
}

/// A ground pixel: how far along the ray, and how bright.
struct Ground {
    t: f32,
    shade: u8,
}

/// Intersect a ray with the stage plane and shade its grid.
fn stage_hit(camera: &Camera, dir: &[f32; 3], stage: Stage) -> Option<Ground> {
    if dir[1] >= -1e-5 {
        return None; // parallel, or looking up from the plane
    }
    let t = (stage.y - camera.origin[1]) / dir[1];
    if t <= 0.0 || t > camera.max_t {
        return None;
    }
    let gx = camera.origin[0] + dir[0] * t;
    let gz = camera.origin[2] + dir[2] * t;
    let reach = (gx * gx + gz * gz).sqrt();
    if reach > stage.radius {
        return None;
    }

    let line = |v: f32| {
        let f = (v / stage.spacing).abs() % 1.0;
        f.min(1.0 - f) * stage.spacing < 0.012
    };
    // Fade to sky at the rim so the platform has an edge without an outline.
    let fade = (1.0 - reach / stage.radius).clamp(0.0, 1.0);
    let base = if line(gx) || line(gz) { 96.0 } else { 52.0 };
    Some(Ground {
        t,
        shade: (base * (0.25 + 0.75 * fade)) as u8,
    })
}

fn shade_at(n: [f32; 3], light: [f32; 3]) -> u8 {
    let diffuse = n[0] * light[0] + n[1] * light[1] + n[2] * light[2];
    // Ambient floor plus a weak counter term keeps the unlit side readable as a form instead
    // of as a hole.
    let shade = (0.20 + 0.80 * diffuse.max(0.0) + 0.22 * (-diffuse).max(0.0)).min(1.0) * 235.0;
    shade as u8
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 0.0001 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere(material_id: u8) -> EntityDef {
        EntityDef {
            shape: assetc::Shape::Sphere {
                r: 1.0,
                centre: [0.0, 0.0, 0.0],
            },
            position: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: 1.0,
            material_id,
            flags: 0,
            quantity: 0,
        }
    }

    /// The centre pixel of a 16x16 frame, as (R, G, B).
    fn centre_pixel(frame: &Frame) -> (u8, u8, u8) {
        let i = ((8 * 16) + 8) * 3;
        (frame.pixels[i], frame.pixels[i + 1], frame.pixels[i + 2])
    }

    #[test]
    fn assigned_material_reaches_the_pixel_and_the_stage_does_not() {
        let renderer = CpuRenderer::new(16, 16);
        let materials = MaterialRegistry::with_defaults();

        // Slot 0 is pig_skin [1.0, 0.7, 0.75], slot 2 is stone [0.5, 0.5, 0.5].
        let pig = centre_pixel(&renderer.render(&sphere(0), &materials, 0.0, 0.0, 3.0));
        let rock = centre_pixel(&renderer.render(&sphere(2), &materials, 0.0, 0.0, 3.0));

        assert!(pig.0 > 0 && pig.1 > 0, "nothing was shaded: {pig:?}");
        assert!(
            pig.0 > pig.1,
            "the viewport is grayscale again — base_color never reached the pixel: {pig:?}"
        );
        assert_eq!(rock.0, rock.1, "stone's channels must stay equal: {rock:?}");
        assert!(
            pig.0 > rock.0,
            "pig_skin (1.0) and stone (0.5) rendered the same red: {pig:?} vs {rock:?}"
        );
    }

    #[test]
    fn an_empty_slot_renders_instead_of_refusing_the_frame() {
        let renderer = CpuRenderer::new(16, 16);
        let materials = MaterialRegistry::with_defaults();
        let hit = centre_pixel(&renderer.render(&sphere(200), &materials, 0.0, 0.0, 3.0));
        assert!(hit.0 > 0, "an unassigned material lost the shape: {hit:?}");
    }
}
