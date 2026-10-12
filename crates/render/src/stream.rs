//! Region streaming: a world of scenes, marched as one field, with the plan deciding who is in it.
//!
//! Why this exists as a separate module rather than as one bigger [`Tree`]: **A-07's grammar has no
//! translation node.** The kind list runs sphere, rounded box, cylinder, torus, plane, the five
//! combinators, `repeat`, `mirror`, `twist`, `bend`, `noise`, `material` - and `repeat` tiles at a fixed
//! period while `mirror` reflects about zero, so neither can say "this subtree, over *there*". Two authored
//! shapes in different regions therefore cannot be joined into one tree at all today, which is a contract
//! question (only the Architect adds a kind, AGENTS.md §2) and is filed as
//! [OQ-55](../../../docs/13-risks-open-questions.md). What this module does instead is the thing a running
//! engine would do anyway: keep one tree per region, and march the **minimum** of their fields.
//!
//! That is legal for the reason docs/04 gives for `min` combinators generally: the minimum of a set of
//! 1-Lipschitz fields is 1-Lipschitz, so sphere tracing the composite cannot step through a surface, and
//! the nearest-hit image a union would produce is exactly what comes out. `tests/stream.rs` pins that
//! against the single-tree renderer rather than trusting the argument.
//!
//! The plan is where streaming earns its keep. A region whose translated enclosure cannot be reached by the
//! camera's ray range contributes nothing to any pixel, so it is not selected and not evaluated - which is
//! the difference between "cost grows with the world" and "cost grows with what is nearby". The measurement
//! is `examples/region_cost.rs`.
//!
//! Floats throughout, and none of it is hashed: a region cell is integer arithmetic (gate C1's
//! `region * 4096 + offset`), and the only place it becomes an `f32` is here, on the way to a pixel
//! (docs/02's float discipline, AGENTS.md §4).

use crate::cull::{ray_may_hit, Enclosure};
use crate::raster::{normal_to_rgb, ray_for, sky, unit, Camera, Frame, Stats};
use crate::{normal, sdf_eval, Tree, ROOT};
use contracts::coords::GlobalVoxelPos;

/// One region's side length in voxels, and so in world units: gate C1's `region * 4096 + offset`, quoted
/// from the contract rather than restated as a second number.
pub const REGION_UNITS: f32 = GlobalVoxelPos::REGION_SIZE as f32;

/// A compiled shape placed at a region cell. `cell` is the region index triple - integers, floor-divided,
/// the form docs/02 pins - and the placement is `cell * REGION_UNITS` in world space.
#[derive(Clone)]
pub struct Region {
    pub cell: [i64; 3],
    pub tree: Tree,
}

impl Region {
    /// The world-space origin of this region's cell.
    #[inline]
    pub fn origin(&self) -> [f32; 3] {
        [
            self.cell[0] as f32 * REGION_UNITS,
            self.cell[1] as f32 * REGION_UNITS,
            self.cell[2] as f32 * REGION_UNITS,
        ]
    }

    /// This region's enclosure moved into world space. A scene with no bound stays unbounded: streaming may
    /// not invent one, for the same reason `cull` refuses to.
    pub fn enclosure(&self) -> Enclosure {
        let origin = self.origin();
        match crate::cull::scene_enclosure(&self.tree) {
            Enclosure::Bounded { centre, radius } => Enclosure::Bounded {
                centre: [
                    centre[0] + origin[0],
                    centre[1] + origin[1],
                    centre[2] + origin[2],
                ],
                radius,
            },
            Enclosure::Unbounded => Enclosure::Unbounded,
        }
    }

    /// The field at a world point, in this region's own coordinates.
    #[inline]
    pub fn field_at(&self, world: [f32; 3]) -> f32 {
        let origin = self.origin();
        sdf_eval(
            &self.tree,
            ROOT,
            [
                world[0] - origin[0],
                world[1] - origin[1],
                world[2] - origin[2],
            ],
        )
    }
}

/// The regions a frame may need: those whose enclosure can be reached from `camera.origin` within
/// `camera.max_t`.
///
/// Deliberately weaker than "visible". A sphere test against the camera's range cannot prove a region is
/// off-screen - the frustum is a wedge, the range is a ball - so a region can be selected and still cost
/// nothing, because every ray in it is rejected by the per-ray test below. The asymmetry is the safe
/// direction: over-selecting costs evaluations, under-selecting deletes geometry.
pub fn plan(camera: &Camera, regions: &[Region]) -> Vec<usize> {
    let mut selected = Vec::new();
    for (index, region) in regions.iter().enumerate() {
        let Enclosure::Bounded { centre, radius } = region.enclosure() else {
            // No bound means no proof of irrelevance: it is in the frame, always.
            selected.push(index);
            continue;
        };
        let to_centre = [
            centre[0] - camera.origin[0],
            centre[1] - camera.origin[1],
            centre[2] - camera.origin[2],
        ];
        // `length - radius <= max_t` is "some point of this sphere is within the ray range". The
        // normalisation below is not part of the test; it keeps `length` from being read as a direction.
        let length = crate::length(to_centre);
        if length - radius <= camera.max_t + camera.surface {
            selected.push(index);
        }
    }
    selected
}

/// March the selected regions as one field: nearest surface wins, exactly as a union would make it win, and
/// with the same per-ray rejection `render_culled` performs on one scene.
///
/// The march loop is this module's own, and that is the honest cost of a grammar that cannot express a
/// translated tree - so the equivalence is tested rather than asserted: `tests/stream.rs` renders a single
/// region at cell `[0, 0, 0]` through here and through [`crate::raster::render_bnb`] and requires one
/// image, and renders a near region with a far one present and absent and requires no pixel to move.
pub fn render(camera: &Camera, regions: &[Region], selected: &[usize]) -> (Frame, Stats) {
    let forward = unit(camera.forward);
    let right = unit(camera.right);
    let up = unit(camera.up);
    let rays = (camera.width * camera.height) as usize;
    let mut frame = Frame::new(camera.width, camera.height);
    frame.prepare(camera.width, camera.height);

    let mut hits = 0u32;
    let mut evaluations = 0u64;
    let mut skipped = 0u32;

    // Pre-compute what each selected region needs, once, rather than per ray.
    // Grown by the marcher's hit tolerance, exactly as `raster` grows the enclosures it tests: a ray that
    // grazes a region - passing within `surface` of its sphere but outside it - is a hit as far as the march
    // is concerned, and a skip that disagrees with the marcher is a missing pixel rather than a saving.
    let bounds: Vec<Enclosure> = selected
        .iter()
        .map(|&index| regions[index].enclosure().grown(camera.surface))
        .collect();

    for py in 0..camera.height {
        for px in 0..camera.width {
            let dir = ray_for(camera, px, py, forward, right, up);
            let mut any = false;
            for enclosure in &bounds {
                if ray_may_hit(*enclosure, camera.origin, dir, camera.max_t) {
                    any = true;
                    break;
                }
            }
            if !any {
                skipped += 1;
                push(&mut frame.pixels, &sky(dir));
                frame.depth.push(Frame::NO_HIT);
                continue;
            }
            let mut t = 0.0f32;
            let mut steps = 0u32;
            let mut hit_t = Frame::NO_HIT;
            let mut hit_point = camera.origin;
            // Which region owns the surface, so the gradient is tapped in that region's coordinates and
            // against that region's tree. Guessing "the first one" would shade a neighbouring region's
            // surface with this one's field - a plausible normal, and wrong.
            let mut hit_region = usize::MAX;
            let mut hit_steps = 0u32;
            while t < camera.max_t && steps < camera.max_steps {
                let point = [
                    camera.origin[0] + dir[0] * t,
                    camera.origin[1] + dir[1] * t,
                    camera.origin[2] + dir[2] * t,
                ];
                let mut distance = f32::MAX;
                let mut owner = 0usize;
                for (slot, &index) in selected.iter().enumerate() {
                    let field = regions[index].field_at(point);
                    if field < distance {
                        distance = field;
                        owner = slot;
                    }
                }
                if distance.abs() < camera.surface {
                    hit_t = t;
                    hit_point = point;
                    hit_region = owner;
                    hit_steps = steps;
                    break;
                }
                t += distance.max(camera.surface);
                steps += 1;
            }
            if hit_t == Frame::NO_HIT {
                // The same accounting `raster` uses, so a one-region frame and a tree frame report
                // comparable numbers: a miss is charged the whole step budget, which is an upper bound.
                evaluations += u64::from(camera.max_steps);
                push(&mut frame.pixels, &sky(dir));
                frame.depth.push(Frame::NO_HIT);
                continue;
            }
            hits += 1;
            evaluations += u64::from(hit_steps) + 1;
            let region = &regions[selected[hit_region]];
            let n = normal(
                &region.tree,
                ROOT,
                local_of(region, hit_point),
                camera.surface * 16.0,
            );
            push(&mut frame.pixels, &normal_to_rgb(n));
            frame.depth.push(hit_t);
        }
    }

    (
        frame,
        Stats {
            rays: rays as u32,
            hits,
            evaluations,
            skipped,
            visits: 0,
            not_drawn: 0,
        },
    )
}

/// The world point expressed in the region that owns the hit, so the gradient is tapped in that region's
/// own coordinates.
#[inline]
fn local_of(region: &Region, world: [f32; 3]) -> [f32; 3] {
    let origin = region.origin();
    [
        world[0] - origin[0],
        world[1] - origin[1],
        world[2] - origin[2],
    ]
}

fn push(pixels: &mut Vec<u8>, rgb: &[u8; 3]) {
    pixels.extend_from_slice(rgb);
}
