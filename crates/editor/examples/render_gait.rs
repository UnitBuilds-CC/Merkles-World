//! Pose the pig: one compiled tree, walked by rewriting parameters, dumped as a stride strip.
//!
//! This is the instrument for doc 07 § Animation's claim that "a pig is a function; there is no skeleton to
//! bind". `entity::rig` turns a gait phase into sphere-centre writes, so what a frame *costs* is unchanged
//! by the pose: the tree marched here has the same record count at every phase, and each row prints that
//! count so the claim is visible rather than asserted. The alternative doc 07's analysis names — one tree
//! walk per placed part — would print a different number per frame.
//!
//! The strip goes to `temp_dir` like `render_png`'s six views: a verification artifact, never content (§6,
//! §8). Judging a gait from stills is honest only because the phases are enumerated — eight frames of one
//! stride show the lift, the contact and the extension, which is the evidence a video would give minus the
//! animation between them.

use std::path::PathBuf;

use assetc::EntityDef;
use editor::gpu_renderer::{CpuRenderer, Stage};
use entity::rig::{self, Gait, LegSpec};
use entity::MaterialRegistry;
use image::{ColorType, ImageFormat};
use render::raster::Frame;

/// The pig's authored limb columns, in shape space, read off `assets/entities/pig.ron`.
///
/// The front pair is at `z = -0.16` and the back pair at `z = 0.28`, both authored at `x = 0.14` and
/// mirrored about X into four. A hip sits above the topmost sphere of the bone it turns — the file's belly
/// is at `y ~ 0.10` and the columns reach `y = 0.135`, so `0.20` puts the hinge inside flesh that hides it.
const FRONT: LegSpec = LegSpec {
    z: -0.16,
    tolerance: 1e-3,
    hip_y: 0.20,
    knee_y: 0.0,
};
const BACK: LegSpec = LegSpec {
    z: 0.28,
    tolerance: 1e-3,
    hip_y: 0.20,
    knee_y: 0.0,
};

/// One stride, in radians of hip and knee.
///
/// `swing = 0.28` is about 16 degrees and `flex = 0.40` about 23: enough for a hoof to leave the grid
/// mid-stride and for the knee to read as a joint rather than as a leaning post, small enough that the
/// smooth union at the shoulder never parts. Doc 07's advice to keep warp amplitudes modest was a warning
/// about march steps, and a parameter patch adds none — but a limb that pulls out of its own socket is a
/// shape failure all the same, so the amplitude stays modest for the reason the file gives, now about form
/// rather than cost.
const GAIT: Gait = Gait {
    omega: 1.0,
    swing: 0.28,
    flex: 0.40,
};

/// Eight frames, evenly spaced over one stride — that is, at `2 pi / 8` of phase each.
///
/// The first draft of this list named six phases and produced three distinct silhouettes, because six
/// samples of a sine cluster at its peak: `sin(1.05)` and `sin(2.1)` are the same height to three digits, so
/// the strip showed the same forward-leaning pig twice and called it a gait. Sampling the *cycle* evenly is
/// what makes the frames a stride: the hip then takes the values `0, .707, 1, .707, 0, -.707, -1, -.707`
/// times the amplitude, and the frame at maximum reach is the one a shoulder gap would show up in.
const FRAMES: usize = 8;

/// The frame taken obliquely: maximum forward reach on the front leg, where a hip that pulled out of the
/// flesh would show as a gap rather than as a shadow.
const OBLIQUE_FRAME: usize = 2;

fn main() {
    let source_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/entities/pig.ron".to_string());
    let source = std::fs::read_to_string(&source_path)
        .unwrap_or_else(|e| panic!("cannot read {source_path}: {e}"));
    let def: EntityDef =
        ron::from_str(&source).unwrap_or_else(|e| panic!("cannot parse {source_path}: {e}"));

    let out_dir = out_dir();
    std::fs::create_dir_all(&out_dir).expect("gait dir");
    // Stale dumps are the classic false pass, and a strip is worse than a single view: a reader comparing
    // eight frames cannot tell one from last run's unless every name in the set is regenerated.
    if let Ok(entries) = std::fs::read_dir(&out_dir) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }

    let compiled = assetc::compile_entity(&def)
        .unwrap_or_else(|refusal| panic!("{source_path} does not lower: {refusal}"));
    let rest = compiled.asset.tree.clone();
    let front = rig::select_leg(&rest, &FRONT).expect("the front leg is in the tree");
    let back = rig::select_leg(&rest, &BACK).expect("the back leg is in the tree");
    println!(
        "rig: {} records ride the hips, {} ride the knees, of {} nodes total",
        front.hip.rides.len() + back.hip.rides.len(),
        front.knee.rides.len() + back.knee.rides.len(),
        rest.nodes.len(),
    );
    println!(
        "     front hip {:?} / knee {:?}\n     back  hip {:?} / knee {:?}",
        front.hip.rides, front.knee.rides, back.hip.rides, back.knee.rides
    );

    let (width, height) = (480u32, 360u32);
    let renderer = CpuRenderer::new(width, height);
    let materials = MaterialRegistry::with_defaults();
    // The floor is the rest pose's lowest surface, so a hoof that lifts leaves it: contact and flight are
    // both in the picture, which is the whole reason to render a phase rather than one stance.
    let stage = Stage::under(&def);
    let rest_support = front.support_y(&rest).min(back.support_y(&rest));

    for frame_index in 0..FRAMES {
        let phase = frame_index as f32 * (core::f32::consts::TAU / FRAMES as f32);
        let mut tree = rest.clone();
        // The back pair is half a cycle out of phase with the front. Left and right cannot be: the authored
        // column is `Mirror`d about X, and an X-parallel hinge moves nothing in X, so the two sides of one
        // authored leg are the same records and swing as a pair — a pace, not a trot. See doc 07.
        let (front_hip, front_knee) = GAIT.angles(phase);
        let (back_hip, back_knee) = GAIT.angles(phase + core::f32::consts::PI);
        front.pose(&mut tree, front_hip, front_knee);
        back.pose(&mut tree, back_hip, back_knee);

        // A pose is a legal edit or it is nothing: a rewrite that pushed a parameter out of the pool would
        // fail here, and not as an unexplained artefact in the pixels.
        render::validate(&tree)
            .unwrap_or_else(|e| panic!("frame {frame_index} broke the grammar: {e:?}"));

        // Keep the lowest hoof on the floor by moving the body, not the foot. The first strip had no line
        // like this, and the animal floated at every non-stance frame — the artifact that turns a rigid
        // swing into a hover. Doc 07 hands the pose parameters to the logic module and the vertical is one
        // of them; the limb's own support height is what says how far to take it.
        let support = front.support_y(&tree).min(back.support_y(&tree));
        let mut stance = def.clone();
        stance.position[1] += def.scale * (rest_support - support);
        let bob = stance.position[1] - def.position[1];

        dump(
            &renderer,
            &tree,
            &stance,
            &materials,
            Some(stage),
            &Shot {
                view: "side",
                frame_index,
                yaw: core::f32::consts::FRAC_PI_2,
                pitch: 0.12,
                hip: front_hip,
                knee: front_knee,
                bob,
            },
        );
        if frame_index == OBLIQUE_FRAME {
            dump(
                &renderer,
                &tree,
                &stance,
                &materials,
                Some(stage),
                &Shot {
                    view: "three_quarter",
                    frame_index,
                    yaw: -2.4,
                    pitch: 0.30,
                    hip: front_hip,
                    knee: front_knee,
                    bob,
                },
            );
        }
    }
    println!("strip: {}", out_dir.display());
}

/// One frame of the strip: the angle it is taken from, the limb angles it is supposed to show, and how far
/// the body came down to keep a hoof on the grid.
struct Shot {
    view: &'static str,
    frame_index: usize,
    yaw: f32,
    pitch: f32,
    hip: f32,
    knee: f32,
    bob: f32,
}

fn dump(
    renderer: &CpuRenderer,
    tree: &render::Tree,
    def: &EntityDef,
    materials: &MaterialRegistry,
    stage: Option<Stage>,
    shot: &Shot,
) {
    let (width, height) = (480u32, 360u32);
    let started = std::time::Instant::now();
    let frame = renderer.render_pose(tree, def, materials, shot.yaw, shot.pitch, 1.5, stage);
    let took = started.elapsed().as_millis();
    let surface = frame.depth.iter().filter(|t| **t != Frame::NO_HIT).count();
    let path = out_dir().join(format!("gait_{}_{}.png", shot.view, shot.frame_index));
    image::save_buffer_with_format(
        &path,
        &frame.pixels,
        width,
        height,
        ColorType::Rgb8,
        ImageFormat::Png,
    )
    .unwrap_or_else(|e| panic!("cannot write {}: {e}", path.display()));
    println!(
        "{} frame {}: hip {:+.3} rad, knee {:+.3}, body {:+.4} -> {} ({}x{}, {surface} of {} pixels hit, {} nodes, {took}ms)",
        shot.view,
        shot.frame_index,
        shot.hip,
        shot.knee,
        shot.bob,
        path.display(),
        width,
        height,
        width * height,
        tree.nodes.len(),
    );
}

fn out_dir() -> PathBuf {
    std::env::temp_dir().join("merkle_editor_gait")
}
