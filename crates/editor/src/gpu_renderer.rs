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
        // would be judged as if it were the authored shape. `compile_entity` is the contract's own
        // gate, so a non-unit quaternion or a zero scale is refused here too instead of being ignored.
        let compiled = match assetc::compile_entity(entity_def) {
            Ok(compiled) => compiled,
            Err(refusal) => {
                // `Refusal`'s own Display already opens with the place ("rotation is not a unit
                // quaternion…"), so appending `at {location}` printed the same word twice.
                eprintln!("✗ preview refused by assetc: {refusal}");
                return refused_frame(self.width, self.height);
            }
        };
        let place = Placement::from(entity_def);

        let start = std::time::Instant::now();
        let frame = render_frame(
            &compiled.asset.tree,
            &camera,
            stage,
            tint_of(entity_def, materials),
            &place,
        );
        let elapsed = start.elapsed();

        if elapsed.as_millis() > 100 {
            eprintln!(
                "⚠ CPU render took {}ms for {}x{} ({} nodes)",
                elapsed.as_millis(),
                self.width,
                self.height,
                compiled.asset.tree.nodes.len()
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

/// The authored transform from shape space to world, taken from an [`EntityDef`] the same way
/// `assetc::compile_entity` reads it.
///
/// A-07's grammar has no translate or scale node (OQ-55), so the lowered tree stays in shape space and
/// the viewport applies the placement to the ray instead of to the records: a rotation and a uniform
/// scale map a straight ray to a straight ray, and only its parameter stretches by `1 / scale`. That is
/// why marching happens in shape space and every value the frame reports comes back in world units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub position: [f32; 3],
    /// `[x, y, z, w]`, the order `assetc::EntityDef` stores it in.
    pub quat: [f32; 4],
    pub scale: f32,
}

impl Placement {
    /// The identity placement: shape space is world.
    pub const IDENTITY: Self = Self {
        position: [0.0; 3],
        quat: [0.0, 0.0, 0.0, 1.0],
        scale: 1.0,
    };

    pub fn from(entity_def: &EntityDef) -> Self {
        Self {
            position: entity_def.position,
            quat: entity_def.rotation,
            scale: entity_def.scale,
        }
    }

    /// This placement without its translation — the shape's own extent, which is what the stage
    /// measures against. See [`Stage::under`].
    fn contact(&self) -> Self {
        Self {
            position: [0.0; 3],
            ..*self
        }
    }

    /// A world point into shape space.
    fn to_shape(self, p: [f32; 3]) -> [f32; 3] {
        let shifted = [
            (p[0] - self.position[0]) / self.scale,
            (p[1] - self.position[1]) / self.scale,
            (p[2] - self.position[2]) / self.scale,
        ];
        rotate(conjugate(self.quat), shifted)
    }

    /// A shape point into world.
    fn to_world(self, q: [f32; 3]) -> [f32; 3] {
        let rotated = rotate(self.quat, q);
        [
            self.position[0] + self.scale * rotated[0],
            self.position[1] + self.scale * rotated[1],
            self.position[2] + self.scale * rotated[2],
        ]
    }

    /// A world direction into shape space. Rotation only: `scale` is carried by the parameter.
    fn dir_to_shape(&self, d: [f32; 3]) -> [f32; 3] {
        rotate(conjugate(self.quat), d)
    }

    /// A shape-space normal into world. A uniform scale leaves a normal's direction alone.
    fn normal_to_world(&self, n: [f32; 3]) -> [f32; 3] {
        rotate(self.quat, n)
    }
}

/// Rotate a vector by `[x, y, z, w]`: `v + w * t + q × t`, with `t = 2 (q × v)`.
fn rotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let axis = [q[0], q[1], q[2]];
    let t = {
        let c = cross(axis, v);
        [2.0 * c[0], 2.0 * c[1], 2.0 * c[2]]
    };
    let second = cross(axis, t);
    [
        v[0] + q[3] * t[0] + second[0],
        v[1] + q[3] * t[1] + second[1],
        v[2] + q[3] * t[2] + second[2],
    ]
}

fn conjugate(q: [f32; 4]) -> [f32; 4] {
    [-q[0], -q[1], -q[2], q[3]]
}

/// A surface found by [`march_placed`], in world units.
struct PlacedHit {
    t: f32,
    point: [f32; 3],
    normal: [f32; 3],
}

/// Trace one world ray against a placed tree.
///
/// The tolerance is converted too: the camera asks for a world-space `surface`, and a shape-space
/// epsilon of `surface / scale` is what delivers that after the point comes back out. Leaving it raw
/// would make a scaled-up entity sharper than the author asked for and a scaled-down one blurred.
fn march_placed(
    tree: &Tree,
    place: &Placement,
    origin: [f32; 3],
    dir: [f32; 3],
    max_t: f32,
    surface: f32,
    max_steps: u32,
) -> Option<PlacedHit> {
    let shape_surface = surface / place.scale;
    let hit = march(
        tree,
        ROOT,
        place.to_shape(origin),
        place.dir_to_shape(dir),
        max_t / place.scale,
        shape_surface,
        max_steps,
    )?;
    let normal_shape = normal(tree, ROOT, hit.point, shape_surface);
    Some(PlacedHit {
        t: hit.t * place.scale,
        point: place.to_world(hit.point),
        normal: place.normal_to_world(normal_shape),
    })
}

/// A ground plane to stand the entity on, so scale and contact are legible.
#[derive(Debug, Clone, Copy)]
pub struct Stage {
    /// Height of the plane.
    pub y: f32,
    /// Grid spacing in world units.
    pub spacing: f32,
    /// Beyond this radius from the disc's own centre the plane fades to sky, so it reads as a platform
    /// rather than an infinite floor.
    pub radius: f32,
    /// World `x, z` of the disc. The grid lines stay in world coordinates, so moving an entity slides
    /// it across the grid rather than re-centring the grid under its feet.
    pub centre: [f32; 2],
}

impl Stage {
    /// A floor at the lowest surface the entity actually reaches, so its feet meet the grid.
    ///
    /// Measured with the placement's rotation and scale but **not** its translation: if the floor
    /// followed `position.y` upward, the slider that lifts the animal would be invisible by
    /// construction. So the stage is the shape's own contact plane and `position` is what it is placed
    /// on — a raised entity floats over the grid, which is the honest reading of a placement value.
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
        let place = Placement::from(entity_def).contact();
        let y = assetc::lower(&entity_def.shape)
            .ok()
            .and_then(|asset| match render::cull::scene_enclosure(&asset.tree) {
                render::cull::Enclosure::Bounded { centre, radius } => {
                    lowest_surface(&asset.tree, &place, centre, radius)
                }
                render::cull::Enclosure::Unbounded => None,
            })
            .unwrap_or(-0.5);

        Stage {
            y,
            spacing: 0.25,
            radius: 1.6,
            centre: [entity_def.position[0], entity_def.position[2]],
        }
    }
}

/// The lowest world `y` reachable by casting one ray straight up per column of a grid over the
/// enclosure's footprint, and taking the nearest underside. `None` if no column hits, which is the
/// shape's own business — the caller falls back to the enclosure.
///
/// `centre_shape` and `radius_shape` are the enclosure in shape space, which is what
/// `render::cull::scene_enclosure` reports; a rotation and a uniform scale take that sphere to a
/// sphere, so the footprint needs no re-derivation.
fn lowest_surface(
    tree: &Tree,
    place: &Placement,
    centre_shape: [f32; 3],
    radius_shape: f32,
) -> Option<f32> {
    const COLUMNS: u32 = 24;
    let centre = place.to_world(centre_shape);
    let radius = place.scale * radius_shape;
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

            if let Some(hit) = march_placed(
                tree,
                place,
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
///
/// The rays start in world space and the tree lives in shape space, so [`Placement`] is what the
/// march needs — without it an authored `position`, `rotation` or `scale` changes the file and nothing
/// else.
fn render_frame(
    tree: &Tree,
    camera: &Camera,
    stage: Option<Stage>,
    tint: [f32; 3],
    place: &Placement,
) -> Frame {
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

            let hit = march_placed(
                tree,
                place,
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
                        (hit.t, tinted(shade_at(hit.normal, light), tint))
                    } else {
                        (ground.t, [ground.shade; 3])
                    }
                }
                (Some(hit), None) => (hit.t, tinted(shade_at(hit.normal, light), tint)),
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
    let dx = gx - stage.centre[0];
    let dz = gz - stage.centre[1];
    let reach = (dx * dx + dz * dz).sqrt();
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

    /// The authored `pig_skin` is the only thing in a frame whose channels differ — sky is
    /// `[30, 30, 35]`, the stage is grey — so `R != G` selects the subject out of the picture without
    /// depending on where it landed.
    fn entity_mask(frame: &Frame) -> Vec<bool> {
        frame.pixels.chunks(3).map(|rgb| rgb[0] != rgb[1]).collect()
    }

    fn entity_count(frame: &Frame) -> usize {
        entity_mask(frame).iter().filter(|on| **on).count()
    }

    /// `(mean row, mean column)` of the subject's pixels, in raster order (row grows downward).
    fn subject_centre(frame: &Frame) -> (f32, f32) {
        let mask = entity_mask(frame);
        let width = frame.width as usize;
        let (mut rows, mut cols, mut n) = (0.0f32, 0.0f32, 0.0f32);
        for (i, on) in mask.iter().enumerate() {
            if *on {
                rows += (i / width) as f32;
                cols += (i % width) as f32;
                n += 1.0;
            }
        }
        assert!(
            n > 0.0,
            "no subject pixel in the frame — the mask measured only sky and stage"
        );
        (rows / n, cols / n)
    }

    /// How far the subject reaches across the frame, in pixels.
    fn column_spread(frame: &Frame) -> usize {
        let mask = entity_mask(frame);
        let width = frame.width as usize;
        let cols: Vec<usize> = (0..mask.len())
            .filter(|i| mask[*i])
            .map(|i| i % width)
            .collect();
        assert!(!cols.is_empty(), "no subject pixel to measure");
        cols[cols.len() - 1] - cols[0]
    }

    #[test]
    fn the_placement_round_trips_and_pins_the_quaternion_convention() {
        // `[x, y, z, w]` is the order `assetc::EntityDef` stores it in, and a quarter turn about Y
        // takes +X to -Z. Read the tuple as `[w, x, y, z]` and every rotated entity faces a different
        // way from the one the file describes — which no amount of looking at a symmetric pig would
        // reveal.
        let quarter_turn_y = [
            0.0,
            core::f32::consts::FRAC_1_SQRT_2,
            0.0,
            core::f32::consts::FRAC_1_SQRT_2,
        ];
        let turned = rotate(quarter_turn_y, [1.0, 0.0, 0.0]);
        assert!(
            turned[0].abs() < 1e-5 && turned[1].abs() < 1e-5 && (turned[2] + 1.0).abs() < 1e-5,
            "a 90° turn about Y sent +X to {turned:?}, not -Z"
        );

        let place = Placement {
            position: [0.3, -0.2, 1.1],
            quat: quarter_turn_y,
            scale: 2.5,
        };
        for p in [
            [1.0, 2.0, 3.0],
            [-4.0, 0.5, 7.25],
            [0.0, 0.0, 0.0],
            [13.5, -9.25, 0.125],
        ] {
            let back = place.to_world(place.to_shape(p));
            for axis in 0..3 {
                assert!(
                    (back[axis] - p[axis]).abs() < 1e-4,
                    "{p:?} round-tripped to {back:?} — world→shape→world is not a round trip"
                );
            }
        }
        assert_eq!(
            Placement::IDENTITY.to_shape([2.5, -1.5, 0.75]),
            [2.5, -1.5, 0.75],
            "the default placement must not move anything"
        );
    }

    #[test]
    fn the_authored_scale_reaches_the_pixels() {
        let renderer = CpuRenderer::new(64, 64);
        let materials = MaterialRegistry::with_defaults();
        let mut big = sphere(0);
        big.scale = 2.0;

        let small = renderer.render(&sphere(0), &materials, 0.0, 0.0, 3.0);
        let large = renderer.render(&big, &materials, 0.0, 0.0, 3.0);
        assert!(
            entity_count(&large) > entity_count(&small),
            "doubling the scale changed the file and not the frame: {} vs {} subject pixels",
            entity_count(&small),
            entity_count(&large)
        );

        let centre = (32 * 64 + 32) as usize;
        assert!(
            large.depth[centre] < small.depth[centre],
            "a doubled sphere should meet the ray sooner: {} vs {}",
            small.depth[centre],
            large.depth[centre]
        );
    }

    #[test]
    fn the_authored_position_lifts_the_subject_off_its_own_contact_plane() {
        let renderer = CpuRenderer::new(64, 64);
        let materials = MaterialRegistry::with_defaults();
        let standing = sphere(0);
        let mut lifted = standing.clone();
        lifted.position = [0.0, 0.6, 0.0];

        // The stage is the shape's contact plane, so it must not ride upward with the entity: a floor
        // that follows `position.y` would make the slider invisible by construction.
        assert_eq!(
            Stage::under(&standing).y,
            Stage::under(&lifted).y,
            "the floor rose with the animal, so lifting it shows nothing"
        );
        // It does follow X/Z, or a moved entity would stand over a disc left behind at the origin.
        let mut shifted = standing.clone();
        shifted.position = [0.75, 0.0, -0.5];
        assert_eq!(Stage::under(&shifted).centre, [0.75, -0.5]);

        let stage = Stage::under(&standing);
        let still = renderer.render_on_stage(&standing, &materials, 0.0, 0.25, 3.0, Some(stage));
        let up = renderer.render_on_stage(&lifted, &materials, 0.0, 0.25, 3.0, Some(stage));
        let (row_still, _) = subject_centre(&still);
        let (row_up, _) = subject_centre(&up);
        // 0.6 of a 3-unit span across 64 rows is ~13 rows; the sign is the point, the margin is slack.
        assert!(
            row_up < row_still - 3.0,
            "lifting the sphere by 0.6 did not move it up the frame: row {row_still} → {row_up}"
        );
    }

    #[test]
    fn position_x_slides_and_rotation_turns_what_the_viewport_shows() {
        let renderer = CpuRenderer::new(64, 64);
        let materials = MaterialRegistry::with_defaults();

        // At yaw 0 the camera sits on +Z and its `right` axis is world +X, so a positive X placement
        // has to increase the subject's columns. Applied in the camera's own frame instead of the
        // world's, it would move them the other way — or not at all.
        let still = renderer.render(&sphere(0), &materials, 0.0, 0.0, 3.0);
        let mut moved = sphere(0);
        moved.position = [0.4, 0.0, 0.0];
        let shifted = renderer.render(&moved, &materials, 0.0, 0.0, 3.0);
        let (_, col_still) = subject_centre(&still);
        let (_, col_moved) = subject_centre(&shifted);
        assert!(
            col_moved > col_still + 3.0,
            "+0.4 in X slid the subject the wrong way: column {col_still} → {col_moved}"
        );

        // A bar of two spheres laid along X, then turned a quarter about Y: it points at the camera
        // rather than across it, so its spread collapses. A march that ignored `rotation` would leave
        // the wide bar exactly as wide.
        let bar = || assetc::Shape::Union {
            a: Box::new(assetc::Shape::Sphere {
                r: 0.15,
                centre: [0.4, 0.0, 0.0],
            }),
            b: Box::new(assetc::Shape::Sphere {
                r: 0.15,
                centre: [-0.4, 0.0, 0.0],
            }),
        };
        let mut across = sphere(0);
        across.shape = bar();
        let mut toward = across.clone();
        toward.rotation = [
            0.0,
            core::f32::consts::FRAC_1_SQRT_2,
            0.0,
            core::f32::consts::FRAC_1_SQRT_2,
        ];

        let wide = renderer.render(&across, &materials, 0.0, 0.0, 3.0);
        let narrow = renderer.render(&toward, &materials, 0.0, 0.0, 3.0);
        let (wide, narrow) = (column_spread(&wide), column_spread(&narrow));
        assert!(
            narrow * 3 <= wide,
            "a quarter turn did not shorten the bar's spread: {wide} px across, {narrow} px turned"
        );
    }

    #[test]
    fn an_impossible_placement_is_refused_rather_than_drawn() {
        let renderer = CpuRenderer::new(16, 16);
        let materials = MaterialRegistry::with_defaults();

        let mut collapsed = sphere(0);
        collapsed.scale = 0.0;
        let frame = renderer.render(&collapsed, &materials, 0.0, 0.0, 3.0);
        assert!(
            frame.depth.iter().all(|d| *d == Frame::NOT_DRAWN_DEPTH),
            "a zero scale was marched anyway"
        );

        // `compile_entity` is the contract's own gate, and the viewport goes through it: a
        // non-unit quaternion is refused rather than silently normalised or ignored.
        let mut wild = sphere(0);
        wild.rotation = [0.0, 2.0, 0.0, 1.0];
        let frame = renderer.render(&wild, &materials, 0.0, 0.0, 3.0);
        assert!(
            frame.depth.iter().all(|d| *d == Frame::NOT_DRAWN_DEPTH),
            "a quaternion of length {} was drawn instead of refused",
            (4.0f32 + 1.0).sqrt()
        );
    }
}
