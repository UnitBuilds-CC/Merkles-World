//! Render an entity definition to PNGs offscreen so the picture can actually be looked at.
//!
//! §7's point is that a build log is not evidence, and until now the editor preview had no
//! instrument that could be inspected without a human watching a window: the viewport's own pixels
//! were only ever described by the text printed around them. This example writes the same frames the
//! viewport draws into the temp dir, one per camera angle.
//!
//! The dumps are a verification artifact, not content — they go to `temp_dir`, never into the repo
//! (§6 and §8).

use std::path::PathBuf;

use assetc::EntityDef;
use editor::gpu_renderer::{CpuRenderer, Stage};
use entity::MaterialRegistry;
use image::{ColorType, ImageFormat};
use render::raster::Frame;

/// The angles live in `editor::mcp::VIEWS` so the six views this example dumps and the six a caller of
/// `render_view` asks for by name are one table — two lists would drift, and a reader given `front` would
/// not know which `front` they were being shown.
use editor::mcp::VIEWS;

fn main() {
    let source_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/entities/pig.ron".to_string());
    // A view name as the second argument while tuning one angle; the whole set costs ~45s in a debug
    // build, which is long enough to change what gets tried.
    let only = std::env::args().nth(2);

    let source = std::fs::read_to_string(&source_path)
        .unwrap_or_else(|e| panic!("cannot read {source_path}: {e}"));

    let def: EntityDef =
        ron::from_str(&source).unwrap_or_else(|e| panic!("cannot parse {source_path}: {e}"));

    let out_dir = out_dir();
    std::fs::create_dir_all(&out_dir).expect("preview dir");

    // Stale dumps are the classic false pass: a filtered run must not leave last run's frames where a
    // reader will pick them up.
    if let Ok(entries) = std::fs::read_dir(&out_dir) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }

    let (width, height) = (480u32, 360u32);
    let renderer = CpuRenderer::new(width, height);
    let materials = MaterialRegistry::with_defaults();
    // The floor is marched to the lowest surface the tree actually reaches, so the hooves meet the
    // grid rather than the animal floating over it. The grid is 0.25 apart, which is also the scale
    // check: the body should cover about three cells.
    let stage = Stage::under(&def);

    for (name, yaw, pitch, distance) in VIEWS {
        if let Some(only) = &only {
            if only != name {
                continue;
            }
        }
        let started = std::time::Instant::now();
        let frame =
            renderer.render_on_stage(&def, &materials, *yaw, *pitch, *distance, Some(stage));
        let took = started.elapsed().as_millis();

        let surface = frame.depth.iter().filter(|t| **t != Frame::NO_HIT).count();
        let path = out_dir.join(format!("pig_{name}.png"));
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
            "{name}: yaw={yaw:.2} pitch={pitch:.2} dist={distance:.1} -> {} ({}x{}, {surface} of {} pixels hit entity or stage, {took}ms)",
            path.display(),
            width,
            height,
            width * height,
        );
    }
}

fn out_dir() -> PathBuf {
    std::env::temp_dir().join("merkle_editor_preview")
}
