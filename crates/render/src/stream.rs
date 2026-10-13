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

use crate::cull::{ray_may_hit, scene_bounds, Enclosure, SceneBounds};
use crate::raster::{normal_to_rgb, ray_for, sky, unit, Camera, Frame, Stats};
use crate::{normal, sdf_eval, sdf_eval_bnb, Tree, ROOT};
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

    /// The camera's position expressed in this region's own coordinates, which is the only far-away
    /// arithmetic in this module that is exact: for two `f32` of comparable magnitude the difference is
    /// representable, so IEEE-754 returns it without rounding (Sterbenz). Subtracting afterwards does not
    /// work, because the point being subtracted from has already been rounded to the cell's `f32` ulp -
    /// 64 world units at 10^9 - and `examples/deep_zoom.rs` measured what that costs: 1,280 of 4,096
    /// pixels of a snout deleted from the frame.
    #[inline]
    pub fn relative_origin(&self, camera_origin: [f32; 3]) -> [f32; 3] {
        let origin = self.origin();
        [
            camera_origin[0] - origin[0],
            camera_origin[1] - origin[1],
            camera_origin[2] - origin[2],
        ]
    }

    /// This region's enclosure expressed with the eye at the origin, which is the form `plan` and `render`
    /// need. The tree's own centre is a small number, so it moves by way of the exact relative origin
    /// (`centre - relative_origin` places it in camera space) rather than by way of the cell's absolute
    /// position, which would round the shape's centre to the cell's ulp before the camera ever saw it.
    /// A scene with no bound stays unbounded: streaming may not invent one, for the same reason `cull`
    /// refuses to.
    pub fn enclosure_relative(&self, camera_origin: [f32; 3]) -> Enclosure {
        let relative = self.relative_origin(camera_origin);
        match crate::cull::scene_enclosure(&self.tree) {
            Enclosure::Bounded { centre, radius } => Enclosure::Bounded {
                centre: [
                    centre[0] - relative[0],
                    centre[1] - relative[1],
                    centre[2] - relative[2],
                ],
                radius,
            },
            Enclosure::Unbounded => Enclosure::Unbounded,
        }
    }

    /// The field at a point already expressed in this region's coordinates. Getting there by way of a world
    /// point is the mistake `relative_origin` exists to prevent, so this takes the local point and nothing
    /// else.
    #[inline]
    pub fn field_at_local(&self, local: [f32; 3]) -> f32 {
        sdf_eval(&self.tree, ROOT, local)
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
        let Enclosure::Bounded { centre, radius } = region.enclosure_relative(camera.origin) else {
            // No bound means no proof of irrelevance: it is in the frame, always.
            selected.push(index);
            continue;
        };
        // Already camera-relative, so the centre's length is the distance from the eye - and it stayed
        // exact, because the cell's origin cancelled before any of these numbers grew.
        let length = crate::length(centre);
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
    march_regions(camera, regions, selected, None)
}

/// The same frame with ADR-024's descent running *inside* each region: one bound table per selected region,
/// so a ray that cannot reach any child of that region's tree is rejected before the region is marched, and
/// a march step may skip a subtree whose own enclosure cannot hold the surface.
///
/// The region-level bound was already doing the coarse half of this job, which is why the number to look at
/// is node visits rather than evaluations: what a per-region test cannot see is the geometry *within* a
/// cell, and that is the terrain-scale case OQ-56 pointed at. The tables are built once per frame, not once
/// per ray, which is the whole cost this path adds.
pub fn render_bnb(camera: &Camera, regions: &[Region], selected: &[usize]) -> (Frame, Stats) {
    let tables: Vec<SceneBounds> = selected
        .iter()
        .map(|&index| scene_bounds(&regions[index].tree))
        .collect();
    march_regions(camera, regions, selected, Some(&tables))
}

/// [`render_bnb`] with the caller's own tables, which is the form an instrument needs: pass
/// [`crate::cull::counting_bounds`] per region and the identical frame reports the node visits the pruned
/// walk avoided, so the saving is a measured difference instead of an argument. The tables must be one per
/// entry of `selected`, in the same order.
pub fn render_with_tables(
    camera: &Camera,
    regions: &[Region],
    selected: &[usize],
    tables: &[SceneBounds],
) -> (Frame, Stats) {
    assert_eq!(
        tables.len(),
        selected.len(),
        "one bound table per selected region, in the same order"
    );
    march_regions(camera, regions, selected, Some(tables))
}

fn march_regions(
    camera: &Camera,
    regions: &[Region],
    selected: &[usize],
    tables: Option<&[SceneBounds]>,
) -> (Frame, Stats) {
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
    // Both are camera-relative: the eye is the origin of the arithmetic (docs/02), so no number in the
    // bounds below is a world coordinate, and none of them has to survive a subtraction afterwards.
    let bounds: Vec<Enclosure> = selected
        .iter()
        .map(|&index| {
            regions[index]
                .enclosure_relative(camera.origin)
                .grown(camera.surface)
        })
        .collect();
    // The eye, in each region's own coordinates. Exact by the same argument as `relative_origin`, and the
    // reason the march can accumulate `t` here at all: at a far cell the alternative - build the world point,
    // subtract the cell afterwards - rounds the sample to the cell's ulp (64 units at 10^9) and deletes
    // surface, which `examples/deep_zoom.rs` measured at 1,280 of 4,096 pixels.
    let eyes: Vec<[f32; 3]> = selected
        .iter()
        .map(|&index| regions[index].relative_origin(camera.origin))
        .collect();

    for py in 0..camera.height {
        for px in 0..camera.width {
            let dir = ray_for(camera, px, py, forward, right, up);
            let mut any = false;
            for (slot, enclosure) in bounds.iter().enumerate() {
                if !ray_may_hit(*enclosure, [0.0; 3], dir, camera.max_t) {
                    continue;
                }
                // The descent is asked in this region's own coordinates, from this region's own eye, for the
                // same reason the samples are: a bound table is a statement about the tree's frame, not the
                // world's. A counting table declines to answer (`prunes()` is false), so the baseline walk
                // asks the region test and nothing deeper - which is the difference being measured.
                if let Some(scene) = tables {
                    let scene = &scene[slot];
                    if scene.prunes()
                        && !scene.may_reach_subtree(
                            &regions[selected[slot]].tree,
                            ROOT as usize,
                            eyes[slot],
                            dir,
                            camera.surface,
                        )
                    {
                        continue;
                    }
                }
                any = true;
                break;
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
            // The hit point stays in the coordinates of whichever region owned it, because that is where
            // its gradient has to be tapped and where it is still precise.
            let mut hit_local = [0.0f32; 3];
            // Which region owns the surface, so the gradient is tapped in that region's coordinates and
            // against that region's tree. Guessing "the first one" would shade a neighbouring region's
            // surface with this one's field - a plausible normal, and wrong.
            let mut hit_region = usize::MAX;
            let mut hit_steps = 0u32;
            while t < camera.max_t && steps < camera.max_steps {
                let mut distance = f32::MAX;
                let mut owner = 0usize;
                let mut owner_local = [0.0f32; 3];
                for (slot, &index) in selected.iter().enumerate() {
                    let eye = eyes[slot];
                    let local = [
                        eye[0] + dir[0] * t,
                        eye[1] + dir[1] * t,
                        eye[2] + dir[2] * t,
                    ];
                    let field = match tables {
                        // One evaluator with an optional table, same rule as `raster`: two implementations of
                        // A-07 that agree today are two programs tomorrow. A counting table answers the same
                        // field as no table at all and merely declines every licence, so the two walks this
                        // function can run differ only in what they decline to enter.
                        Some(scene) => {
                            sdf_eval_bnb(&regions[index].tree, &scene[slot], ROOT, local)
                        }
                        None => regions[index].field_at_local(local),
                    };
                    if field < distance {
                        distance = field;
                        owner = slot;
                        owner_local = local;
                    }
                }
                if distance.abs() < camera.surface {
                    hit_t = t;
                    hit_local = owner_local;
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
            let n = normal(&region.tree, ROOT, hit_local, camera.surface * 16.0);
            push(&mut frame.pixels, &normal_to_rgb(n));
            frame.depth.push(hit_t);
        }
    }

    // Node visits are the only column a per-region bound cannot already explain, and they belong to the
    // tables rather than the walk: sum them once, per region, at the end.
    let visits: u64 = match tables {
        Some(scene) => scene.iter().map(SceneBounds::visits).sum(),
        None => 0,
    };

    (
        frame,
        Stats {
            rays: rays as u32,
            hits,
            evaluations,
            skipped,
            visits,
            not_drawn: 0,
        },
    )
}

fn push(pixels: &mut Vec<u8>, rgb: &[u8; 3]) {
    pixels.extend_from_slice(rgb);
}
