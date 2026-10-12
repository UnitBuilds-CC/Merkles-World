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

use crate::cull::{ray_may_hit, Enclosure};
use crate::{march, march_bnb, normal, Tree, Vec3, ROOT};

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

    /// The depth written for a pixel a [`render_bnb_budget`] ceiling never reached. Deliberately not
    /// [`Frame::NO_HIT`]: "marched and found nothing" and "never marched" are different facts, and a frame
    /// that conflates them cannot be audited — the whole value of a budget pass is knowing which pixels it
    /// sacrificed. `f32::MIN` because no ray parameter can be that.
    pub const NOT_DRAWN_DEPTH: f32 = f32::MIN;

    /// The colour sent alongside [`Frame::NOT_DRAWN_DEPTH`]: three small distinct values, so a frame that
    /// forgot to draw is unmistakable in a dump and in a diff rather than a shade of black.
    pub const NOT_DRAWN_RGB: [u8; 3] = [1, 2, 3];

    /// An empty frame with room for `width * height` pixels.
    pub fn new(width: u32, height: u32) -> Frame {
        let rays = (width * height) as usize;
        Frame {
            width,
            height,
            pixels: Vec::with_capacity(rays * 3),
            depth: Vec::with_capacity(rays),
        }
    }

    /// Make this frame ready to be written as a `width x height` image, **reusing whatever memory it
    /// already holds**. Capacity only ever grows, so a frame rendered every frame at a stable resolution
    /// stops allocating after the first one - which is the part of doc 09's zero-copy promise that can be
    /// measured on a CPU. Shrinking is not done on purpose: releasing the buffer to save memory the
    /// renderer will immediately ask for again is the churn the hot-path ban is about.
    pub fn prepare(&mut self, width: u32, height: u32) {
        let rays = (width * height) as usize;
        self.width = width;
        self.height = height;
        self.pixels.clear();
        self.depth.clear();
        self.pixels.reserve_exact(rays * 3);
        self.depth.reserve_exact(rays);
    }

    /// Every depth reset to [`Frame::NO_HIT`] and no pixel counted as a hit.
    ///
    /// [`render_into`] and friends do this themselves; it is public because a caller that writes pixels by
    /// hand (a compositor, a test) needs the same guarantee the marcher gets.
    pub fn clear(&mut self) {
        self.pixels.clear();
        self.depth.clear();
    }

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
    /// Rays that [`render_culled`] proved could not reach the scene and therefore never evaluated. Zero
    /// from [`render`], which has no bound to consult: the field exists so the saving is stated next to
    /// the number it reduced rather than in prose.
    pub skipped: u32,
    /// Nodes the evaluator entered, tallied only when a bound table is in play: [`render`] reports 0,
    /// [`render_culled`] reports the **unpruned** walk (its table counts and declines to skip), and
    /// [`render_bnb`] reports the pruned one. That pairing is the point — `evaluations` above cannot see a
    /// per-node skip at all, because a skipped subtree returns the distance the walk would have returned
    /// anyway, so the ray's step count is identical either way.
    ///
    /// March-step field walks only. The four gradient taps per hit go through the public evaluator and are
    /// not counted, which is why the number is "nodes entered per frame" and not "cost of the frame".
    pub visits: u64,
    /// Pixels a [`render_bnb_budget`] ceiling never reached, left at [`Frame::NOT_DRAWN_DEPTH`]. Zero from
    /// every unbudgeted path, which is the point: `rays == drawn + not_drawn` is the check that a budget cut
    /// pixels rather than blurred the accounting.
    pub not_drawn: u32,
}

/// March every pixel with no culling. This is the reference: whatever a culled path produces has to
/// produce the same image, and only the evaluation count may differ.
pub fn render(tree: &Tree, camera: &Camera) -> (Frame, Stats) {
    let mut frame = Frame::new(camera.width, camera.height);
    let stats = render_into(tree, camera, &mut frame);
    (frame, stats)
}

/// [`render`] writing into a frame the caller keeps.
///
/// This is the form the hot path is supposed to use, and the reason doc 09's "no allocation churn"
/// promise is checkable at all: `examples/allocation_audit.rs` counts what each render allocates, and a
/// reused frame is where the number has to reach zero.
pub fn render_into(tree: &Tree, camera: &Camera, frame: &mut Frame) -> Stats {
    march_into(tree, camera, None, None, frame)
}

/// March every pixel against the scene's own [`crate::cull::Enclosure`], charging nothing for a ray that
/// provably cannot reach it.
///
/// The image is identical to [`render`]'s - pixels, depth and hit count - because a bounded enclosure is a
/// superset of the surface, and `tests/cull.rs` asserts that equality rather than trusting the arithmetic.
/// A scene containing an unbounded node (a plane, a tiling) yields `skipped = 0`, which is the refusal
/// working: no bound, no early-out, no invented box.
pub fn render_culled(tree: &Tree, camera: &Camera) -> (Frame, Stats) {
    let mut frame = Frame::new(camera.width, camera.height);
    let stats = render_culled_into(tree, camera, &mut frame);
    (frame, stats)
}

/// [`render_culled`] writing into a reused frame. Note that this path still builds a bound table per call,
/// which is an allocation - the audit reports that honestly rather than pretending the frame is the only
/// memory a frame costs.
pub fn render_culled_into(tree: &Tree, camera: &Camera, frame: &mut Frame) -> Stats {
    // The counting table, not the pruning one: this entry point is the ray-level early-out and the
    // unpruned baseline to compare `render_bnb` against.
    let bounds = crate::cull::counting_bounds(tree);
    march_into(
        tree,
        camera,
        Some(bounds.enclosure_of(crate::ROOT as usize)),
        Some(&bounds),
        frame,
    )
}

/// The whole culling story: the ray-level early-out of [`render_culled`] plus the per-step subtree skips
/// ADR-024 asks for, which is what keeps working when a scene spreads out and one bound stops proving
/// anything. See `examples/cull_probe.rs` for the measured difference between the two - it is large, and
/// it is the reason the ray-level version alone was not enough.
pub fn render_bnb(tree: &Tree, camera: &Camera) -> (Frame, Stats) {
    let mut frame = Frame::new(camera.width, camera.height);
    let stats = render_bnb_into(tree, camera, &mut frame);
    (frame, stats)
}

/// [`render_bnb`] writing into a reused frame.
pub fn render_bnb_into(tree: &Tree, camera: &Camera, frame: &mut Frame) -> Stats {
    let bounds = crate::cull::scene_bounds(tree);
    render_bnb_with(tree, camera, frame, &bounds)
}

/// [`render_bnb_into`] with the bound table supplied by the caller.
///
/// This exists for the hot path. A `SceneBounds` is two vectors sized by the node count, so building one
/// per frame is the allocation churn AGENTS.md §3 bans during gameplay frames even when the pixel buffers
/// are reused - and a scene's bounds only change when its asset does. A caller that hoists the table out
/// of its loop is the form doc 09's "no allocation churn" promise can honestly be made about, which is
/// what `examples/allocation_audit.rs` counts.
pub fn render_bnb_with(
    tree: &Tree,
    camera: &Camera,
    frame: &mut Frame,
    bounds: &crate::cull::SceneBounds,
) -> Stats {
    march_into(
        tree,
        camera,
        Some(bounds.enclosure_of(crate::ROOT as usize)),
        Some(bounds),
        frame,
    )
}

/// March every pixel of `camera`'s grid against `tree` and shade what comes back, into `frame`.
fn march_into(
    tree: &Tree,
    camera: &Camera,
    cull: Option<Enclosure>,
    bounds: Option<&crate::cull::SceneBounds>,
    frame: &mut Frame,
) -> Stats {
    let forward = unit(camera.forward);
    let right = unit(camera.right);
    let up = unit(camera.up);
    // The gradient taps need to be far enough apart that the four `f32` distances differ; gate R5's own
    // measurement is the reason this is a multiple of the surface epsilon rather than the epsilon itself.
    let normal_eps = camera.surface * 16.0;
    let rays = camera.width * camera.height;
    frame.prepare(camera.width, camera.height);
    let pixels = &mut frame.pixels;
    let depth = &mut frame.depth;
    let mut hits = 0u32;
    let mut evaluations = 0u64;
    let mut skipped = 0u32;

    for py in 0..camera.height {
        // Top row first: pixel (0, 0) is the upper-left, which is what a PPM expects.
        for px in 0..camera.width {
            let dir = ray_for(camera, px, py, forward, right, up);
            // Two ways to prove a ray is empty, and the table decides which is available. A pruning table
            // gets the hierarchy's answer, which *contains* the single-sphere test: the descent's first
            // question is the root's own enclosure, so every ray the root rejects is rejected here too, and
            // what the children add is the ray that passes between two spheres inside a loose cover - the
            // case cull_probe measured at 0.0% for a spread scene. A counting table gets the root test and
            // nothing else, so the baseline stays a baseline.
            let reachable = ray_is_reachable(tree, camera, cull, bounds, dir);
            if !reachable {
                skipped += 1;
                push_rgb(pixels, sky(dir));
                depth.push(Frame::NO_HIT);
                continue;
            }
            let hit = match bounds {
                None => march(
                    tree,
                    ROOT,
                    camera.origin,
                    dir,
                    camera.max_t,
                    camera.surface,
                    camera.max_steps,
                ),
                Some(scene) => march_bnb(
                    tree,
                    scene,
                    ROOT,
                    crate::Ray {
                        origin: camera.origin,
                        dir,
                        max_t: camera.max_t,
                        surface: camera.surface,
                        max_steps: camera.max_steps,
                    },
                ),
            };
            match hit {
                Some(hit) => {
                    hits += 1;
                    evaluations += u64::from(hit.steps) + 1;
                    let n = normal(tree, ROOT, hit.point, normal_eps);
                    push_rgb(pixels, normal_to_rgb(n));
                    depth.push(hit.t);
                }
                None => {
                    evaluations += u64::from(camera.max_steps);
                    push_rgb(pixels, sky(dir));
                    depth.push(Frame::NO_HIT);
                }
            }
        }
    }

    Stats {
        rays,
        hits,
        evaluations,
        skipped,
        visits: bounds.map_or(0, |scene| scene.visits()),
        not_drawn: 0,
    }
}

/// Whether one ray may be skipped outright, given the table and the cull the caller supplied.
///
/// Both rasterising paths ask this through one function on purpose. A budget pass that reached a different
/// verdict than the unbudgeted one would produce a frame whose *drawn* pixels disagree, and the prefix
/// property that makes a partial frame meaningful would then be false by construction rather than by
/// measurement. A pruning table gets the hierarchy's answer, which contains the single-sphere test - the
/// descent's first question is the root's own enclosure - and adds the children a cover sphere cannot judge.
/// Every enclosure consulted here is grown by the marcher's hit tolerance, because a ray passing within
/// `surface` of a sphere but outside it is a hit as far as the marcher is concerned; `cull.rs` carries the
/// argument for why that growth is not a courtesy.
fn ray_is_reachable(
    tree: &Tree,
    camera: &Camera,
    cull: Option<Enclosure>,
    bounds: Option<&crate::cull::SceneBounds>,
    dir: Vec3,
) -> bool {
    match bounds {
        Some(scene) if scene.prunes() => {
            let root = match cull {
                Some(enclosure) => enclosure,
                None => scene.enclosure_of(crate::ROOT as usize),
            };
            ray_may_hit(root.grown(camera.surface), camera.origin, dir, camera.max_t)
                && scene.may_reach_subtree(
                    tree,
                    crate::ROOT as usize,
                    camera.origin,
                    dir,
                    camera.surface,
                )
        }
        _ => match cull {
            Some(enclosure) => ray_may_hit(
                enclosure.grown(camera.surface),
                camera.origin,
                dir,
                camera.max_t,
            ),
            None => true,
        },
    }
}

/// The ray for one pixel, in the camera's own basis: the single place a pixel becomes a ray.
///
/// The raster loop and [`render_bnb_budget`] walk pixels in different orders - the second walks tiles, so a
/// ceiling cuts blocks rather than a ragged edge - and two copies of the pinhole arithmetic is precisely the
/// fork that would show up as "the budgeted frame disagrees with the full one", which would then be blamed
/// on the budget instead of on the duplicate.
fn ray_for(camera: &Camera, px: u32, py: u32, forward: Vec3, right: Vec3, up: Vec3) -> Vec3 {
    let aspect = camera.width as f32 / camera.height as f32;
    let u = (px as f32 + 0.5) / camera.width as f32;
    let v = (py as f32 + 0.5) / camera.height as f32;
    let ndc_x = (2.0 * u - 1.0) * camera.half_height * aspect;
    let ndc_y = (1.0 - 2.0 * v) * camera.half_height;
    unit([
        forward[0] + right[0] * ndc_x + up[0] * ndc_y,
        forward[1] + right[1] * ndc_x + up[1] * ndc_y,
        forward[2] + right[2] * ndc_x + up[2] * ndc_y,
    ])
}

/// The most a single ray can be charged under this file's accounting: `max_steps + 1`, because a hit costs
/// `steps + 1` and a miss costs the full step budget. It is also the unit a [`Budget`] is spent in, which is
/// what turns a ceiling into a bound: refusing to *start* a ray whose worst-case charge does not fit is
/// enough, and no measured quantity is needed mid-frame.
fn ray_charge(camera: &Camera) -> u64 {
    u64::from(camera.max_steps) + 1
}

/// A ceiling on the work one frame may do, in charged evaluations - the same unit
/// [`Stats::evaluations`] reports, so a budget and the number it produced are comparable with no
/// translation step. `usize::MAX` means "no ceiling".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget(pub u64);

/// The order a [`render_bnb_budget`] walk visits pixels in, which decides *where* a ceiling bites.
///
/// Not a cosmetic choice: `budget_probe` measures that at one quarter of the work the tile-major walk keeps
/// 0% of the scene's surface, because it starts in the top-left corner and that is sky, while centre-out
/// keeps the subject and spends the cut on the edges. Same ceiling, same bound, same march, different
/// frame - which is the sense in which ordering is policy here, and why it is an argument rather than an
/// internal detail of one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scan {
    /// Eight-by-eight blocks, top-left block first, rows within a block. The order that spends a ceiling on
    /// whatever happens to sit at the top left.
    TileMajor,
    /// Eight-by-eight blocks ordered by squared tile distance from the frame's centre, ties broken by row
    /// then column so the sequence is a total order and identical on every machine. Integer arithmetic on
    /// purpose: a float here would make the cut position depend on how a backend rounds a square root.
    CentreOut,
}

/// Every tile index in the requested order. `CentreOut` sorts on `(dy*dy + dx*dx, ty, tx)` in integers.
fn tile_order(camera: &Camera, scan: Scan) -> Vec<u32> {
    let tiles_x = camera.width.div_ceil(TILE);
    let tiles_y = camera.height.div_ceil(TILE);
    let mut tiles: Vec<u32> = (0..tiles_x * tiles_y).collect();
    if scan == Scan::CentreOut {
        // The centre doubled to keep half-tiles exact: twice_cx = tiles_x - 1.
        let twice_cx = tiles_x as i64 - 1;
        let twice_cy = tiles_y as i64 - 1;
        tiles.sort_unstable_by_key(|&tile| {
            let tx = (tile % tiles_x) as i64;
            let ty = (tile / tiles_x) as i64;
            let dx = 2 * tx - twice_cx;
            let dy = 2 * ty - twice_cy;
            (dx * dx + dy * dy, ty, tx)
        });
    }
    tiles
}

/// Pixels per tile side, for [`render_bnb_budget`]. Eight is a choice rather than a law - it is the
/// smallest block that still amortises a bound test - and `tests/budget.rs` pins the *behaviour* (whole
/// tiles cut, prefixes identical) instead of the number, so a different tile on a GPU is not a failure.
const TILE: u32 = 8;

/// Write one already-computed colour into a sized frame at `index`.
fn write_pixel(frame: &mut Frame, index: usize, rgb: &[u8; 3]) {
    frame.pixels[index * 3..index * 3 + 3].copy_from_slice(rgb);
}

/// Draw as much of `camera`'s grid as `budget` pays for, through the bound hierarchy, into a caller-owned
/// frame.
///
/// Three properties, each one tested in `tests/budget.rs`, and the third is what makes the first two mean
/// something:
///
/// 1. **The ceiling holds.** `Stats::evaluations <= budget.0`, always, and not by measuring: the check is
///    made against [`ray_charge`], the worst case a ray can cost, before the ray starts.
/// 2. **What is drawn is exactly what the unbudgeted frame draws.** Same bytes, same depths, same buffer
///    layout, because the ray math lives in [`ray_for`] and the march is the same [`crate::march_bnb`].
///    A budget decides *how much*, never *what*.
/// 3. **The order is deterministic and tile-major.** Blocks of eight, top-left first, so a cut removes
///    whole tiles and the sequence of prefixes is identical on every machine. That is what lets a partial
///    frame be compared across architectures at all - gate R4's hash is informational, and this is the
///    version of it that a budget pass can actually produce.
///
/// Pixels past the cut keep [`Frame::NOT_DRAWN_DEPTH`] and [`Frame::NOT_DRAWN_RGB`], and
/// `Stats::not_drawn` counts them: the shortfall is a reported number, not a black pixel someone has to
/// notice. The bound table is built per call here; the unbudgeted path has `render_bnb_with` for callers
/// that hoist it, and a budget pass is expected to want the ceiling more than the allocation.
pub fn render_bnb_budget(tree: &Tree, camera: &Camera, budget: Budget, frame: &mut Frame) -> Stats {
    render_bnb_budget_in(tree, camera, budget, Scan::TileMajor, frame)
}

/// The same ceiling with the visit order chosen by the caller, because [`Scan`] is what decides which
/// part of the frame the cut falls on. Every property documented on [`render_bnb_budget`] holds for
/// both orders; only the set of sacrificed pixels differs.
pub fn render_bnb_budget_in(
    tree: &Tree,
    camera: &Camera,
    budget: Budget,
    scan: Scan,
    frame: &mut Frame,
) -> Stats {
    let bounds = crate::cull::scene_bounds(tree);
    let enclosure = bounds.enclosure_of(crate::ROOT as usize);
    let forward = unit(camera.forward);
    let right = unit(camera.right);
    let up = unit(camera.up);
    let rays = (camera.width * camera.height) as usize;

    frame.width = camera.width;
    frame.height = camera.height;
    frame.pixels.clear();
    frame.pixels.resize(rays * 3, 0);
    for chunk in frame.pixels.chunks_exact_mut(3) {
        chunk.copy_from_slice(&Frame::NOT_DRAWN_RGB);
    }
    frame.depth.clear();
    frame.depth.resize(rays, Frame::NOT_DRAWN_DEPTH);

    let charge = ray_charge(camera);
    let mut spent = 0u64;
    let mut hits = 0u32;
    let mut skipped = 0u32;
    let mut decided = 0u32;
    let mut not_drawn = 0u32;

    let tiles_x = camera.width.div_ceil(TILE);
    'tiles: for tile in tile_order(camera, scan) {
        let tx = tile % tiles_x;
        let ty = tile / tiles_x;
        {
            for py in ty * TILE..((ty + 1) * TILE).min(camera.height) {
                for px in tx * TILE..((tx + 1) * TILE).min(camera.width) {
                    if spent + charge > budget.0 {
                        not_drawn = rays as u32 - decided;
                        break 'tiles;
                    }
                    let dir = ray_for(camera, px, py, forward, right, up);
                    let at = (py * camera.width + px) as usize;
                    if !ray_is_reachable(tree, camera, Some(enclosure), Some(&bounds), dir) {
                        write_pixel(frame, at, &sky(dir));
                        frame.depth[at] = Frame::NO_HIT;
                        skipped += 1;
                        decided += 1;
                        continue;
                    }
                    let hit = crate::march_bnb(
                        tree,
                        &bounds,
                        crate::ROOT,
                        crate::Ray {
                            origin: camera.origin,
                            dir,
                            max_t: camera.max_t,
                            surface: camera.surface,
                            max_steps: camera.max_steps,
                        },
                    );
                    match hit {
                        Some(hit) => {
                            let n =
                                crate::normal(tree, crate::ROOT, hit.point, camera.surface * 16.0);
                            write_pixel(frame, at, &normal_to_rgb(n));
                            frame.depth[at] = hit.t;
                            hits += 1;
                            spent += u64::from(hit.steps) + 1;
                            decided += 1;
                        }
                        None => {
                            write_pixel(frame, at, &sky(dir));
                            frame.depth[at] = Frame::NO_HIT;
                            spent += u64::from(camera.max_steps);
                            decided += 1;
                        }
                    }
                }
            }
        }
    }

    Stats {
        rays: rays as u32,
        hits,
        evaluations: spent,
        skipped,
        visits: bounds.visits(),
        not_drawn,
    }
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
