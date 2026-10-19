//! Entity Editor - Full GUI window with eframe + CPU SDF rendering preview.
//!
//! Usage: `cargo run --release -p editor --example entity_editor`
//!
//! Features:
//! - Native window with the egui UI
//! - 3D viewport showing the marched entity over a ground grid, both from the orbit camera
//! - Orbit camera (mouse drag to rotate, scroll to zoom)
//! - Inspector panel for editing entity parameters in real-time
//! - Material assignment panel
//! - File menu for load/save/export
//! - RON import/export with dirty tracking
//!
//! Run it in release: the preview is a CPU march, and in a debug build the authored pig costs 2.5-4.6 s
//! per 256x192 frame here. The viewport renders only when the camera or the shape actually changed, and
//! at a quarter of the pixels while a drag is in progress.

use std::hash::{Hash, Hasher};

use editor::{
    export::export_menu,
    gpu_renderer::{CpuRenderer, Stage},
    inspector::inspector_panel,
    material_panel::material_panel,
    viewport::{viewport_panel, ViewportCamera},
};
use eframe::{App, CreationContext, Frame};

/// Pixels of the settled preview; a drag renders at half of each axis.
const FULL_WIDTH: u32 = 256;
const FULL_HEIGHT: u32 = 192;

/// What a rendered preview was made from. Any part of it that changes invalidates the image.
#[derive(PartialEq, Eq)]
struct PreviewKey {
    yaw: u32,
    pitch: u32,
    distance: u32,
    shape: u64,
    stage: bool,
    width: u32,
    height: u32,
}

struct EditorApp {
    state: editor::EditorState,
    camera: ViewportCamera,
    cpu_renderer: CpuRenderer,
    preview: Option<egui::TextureHandle>,
    key: Option<PreviewKey>,
    cost_ms: u128,
    show_stage: bool,
}

impl EditorApp {
    fn new(_cc: &CreationContext<'_>) -> Self {
        let mut state = editor::EditorState::new();

        // Try to load pig entity if it exists
        if std::path::Path::new("assets/entities/pig.ron").exists() {
            match state.load_ron("assets/entities/pig.ron") {
                Ok(()) => println!("Loaded pig.ron"),
                Err(e) => eprintln!("Warning: {}", e),
            }
        }

        let cpu_renderer = CpuRenderer::new(FULL_WIDTH, FULL_HEIGHT);

        println!("Entity Editor initialized");
        println!("Controls:");
        println!("  - Drag in viewport to orbit camera");
        println!("  - Scroll to zoom");
        println!("  - Use inspector panel to edit parameters");
        println!("  - File menu to load/save entities");

        Self {
            state,
            camera: ViewportCamera::new(),
            cpu_renderer,
            preview: None,
            key: None,
            cost_ms: 0,
            show_stage: true,
        }
    }

    fn key_for(&self, width: u32, height: u32) -> PreviewKey {
        PreviewKey {
            yaw: self.camera.yaw.to_bits(),
            pitch: self.camera.pitch.to_bits(),
            distance: self.camera.distance.to_bits(),
            shape: shape_signature(&self.state),
            stage: self.show_stage,
            width,
            height,
        }
    }
}

/// A digest of the authored shape, so the inspector moving a slider invalidates the preview. The
/// editor is a tool (§3's bans do not apply to it), and this runs at most once per UI frame against a
/// tree of a few dozen nodes.
fn shape_signature(state: &editor::EditorState) -> u64 {
    let text = ron::to_string(&state.entity_def.shape).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

impl App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut Frame) {
        ctx.send_viewport_cmd(egui::viewport::ViewportCommand::Title(
            if self.state.dirty {
                "Entity Editor - Phase 2 (modified)".to_string()
            } else {
                "Entity Editor - Phase 2".to_string()
            },
        ));

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    export_menu(ui, &mut self.state);
                });
                ui.separator();
                ui.label("Phase 2 Entity Framework");
            });
        });

        egui::SidePanel::right("inspector")
            .default_width(300.0)
            .show(ctx, |ui| {
                inspector_panel(ui, &mut self.state);

                ui.separator();
                ui.heading("Materials");
                material_panel(ui, &mut self.state);
            });

        // Allocated before the central panel, because panels take their space from what the previous
        // ones left: shown afterwards, this one found nothing left to give it and drew nowhere, which
        // is why the checkbox and everything positioned off the viewport's bottom edge vanished.
        egui::TopBottomPanel::bottom("viewport_controls").show(ctx, |ui| {
            let mut stage = self.show_stage;
            ui.horizontal(|ui| {
                ui.checkbox(&mut stage, "Stage grid");
                if stage != self.show_stage {
                    self.show_stage = stage;
                    ctx.request_repaint();
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let response = viewport_panel(ui, &mut self.camera);
            let rect = response.rect;

            if response.hovered() {
                let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll_delta != 0.0 {
                    self.camera.handle_zoom(scroll_delta * 0.01);
                }
            }

            let dragging = response.dragged();
            let (width, height) = if dragging {
                (FULL_WIDTH / 2, FULL_HEIGHT / 2)
            } else {
                (FULL_WIDTH, FULL_HEIGHT)
            };

            if rect.width() > 50.0 && rect.height() > 50.0 {
                let key = self.key_for(width, height);
                if self.key.as_ref() != Some(&key) {
                    self.cpu_renderer.resize(width, height);
                    let stage = self
                        .show_stage
                        .then(|| Stage::under(&self.state.entity_def));
                    let started = std::time::Instant::now();
                    let frame = self.cpu_renderer.render_on_stage(
                        &self.state.entity_def,
                        &self.state.materials,
                        self.camera.yaw,
                        self.camera.pitch,
                        self.camera.distance,
                        stage,
                    );
                    self.cost_ms = started.elapsed().as_millis();
                    let color_image = egui::ColorImage::from_rgb(
                        [frame.width as usize, frame.height as usize],
                        &frame.pixels,
                    );
                    self.preview = Some(ctx.load_texture(
                        "sdf_preview",
                        color_image,
                        egui::TextureOptions::LINEAR,
                    ));
                    self.key = Some(key);
                }

                let painter = ui.painter();
                if let Some(texture) = &self.preview {
                    painter.image(
                        texture.id(),
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }
                draw_gizmo(painter, rect, &self.camera);
                draw_compass(painter, rect, &self.camera);

                painter.text(
                    rect.left_top() + egui::vec2(10.0, 10.0),
                    egui::Align2::LEFT_TOP,
                    format!(
                        "{}x{} · {} ms · drag to orbit, scroll to zoom",
                        width, height, self.cost_ms
                    ),
                    egui::FontId::monospace(11.0),
                    egui::Color32::from_gray(180),
                );
            }
        });

        // Request next frame
        if ctx.input(|i| i.pointer.any_released()) {
            ctx.request_repaint();
        }
    }
}

fn main() {
    env_logger::init();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            // Points, so at 125% scaling this is 1250x750 physical. The 1280x720 this used to ask for is
            // 1600x900 physical, which does not fit a 1536x864 desktop: the window manager shrank the
            // window while egui kept laying out the requested rectangle, and everything anchored to the
            // bottom edge — the gizmo, the compass, the stage checkbox — was drawn outside it.
            .with_inner_size([1000.0, 600.0])
            .with_title("Entity Editor - Phase 2"),
        ..Default::default()
    };

    eframe::run_native(
        "Entity Editor - Phase 2",
        native_options,
        Box::new(|cc| Ok(Box::new(EditorApp::new(cc)))),
    )
    .unwrap();
}

/// The world axes, projected through the same basis the preview was marched with.
fn draw_gizmo(painter: &egui::Painter, rect: egui::Rect, camera: &ViewportCamera) {
    let origin = egui::pos2(rect.left() + 54.0, rect.bottom() - 54.0);
    let length = 40.0;

    for (axis, label, colour) in [
        ([1.0, 0.0, 0.0], "X", egui::Color32::from_rgb(220, 90, 90)),
        ([0.0, 1.0, 0.0], "Y", egui::Color32::from_rgb(120, 220, 120)),
        ([0.0, 0.0, 1.0], "Z", egui::Color32::from_rgb(110, 150, 240)),
    ] {
        let dir = camera.project_dir(axis) * length;
        let end = origin + dir;
        painter.line_segment([origin, end], (2.0, colour.linear_multiply(0.9)));
        painter.text(
            end + egui::vec2(6.0 * dir.x.signum(), 6.0 * dir.y.signum()),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::monospace(13.0),
            colour,
        );
    }
}

/// Where the camera is looking, in world terms: the compass needle is the view direction projected
/// into the horizontal plane, so it turns with the orbit rather than with the mouse.
fn draw_compass(painter: &egui::Painter, rect: egui::Rect, camera: &ViewportCamera) {
    let centre = egui::pos2(rect.right() - 54.0, rect.bottom() - 54.0);
    let radius = 34.0;

    // A black disc on a #1e1e23 viewport is not a disc, so the plate is darker than the background by
    // enough to read and the rim is bright.
    painter.circle_filled(
        centre,
        radius + 5.0,
        egui::Color32::from_rgba_premultiplied(8, 8, 10, 235),
    );
    painter.circle_stroke(centre, radius, (1.5, egui::Color32::from_gray(200)));

    // World -Z is the direction an entity faces, so the needle marks it.
    let needle = camera.project_dir([0.0, 0.0, -1.0]);
    let flat = needle.x.hypot(needle.y);
    if flat > 1e-3 {
        let unit = needle / flat;
        let tip = centre + unit * (radius - 6.0);
        painter.line_segment([centre, tip], (2.0, egui::Color32::from_rgb(240, 200, 90)));
        painter.text(
            tip + unit * 8.0,
            egui::Align2::CENTER_CENTER,
            "facing",
            egui::FontId::monospace(10.0),
            egui::Color32::from_rgb(240, 200, 90),
        );
    }

    painter.text(
        rect.right_top() + egui::vec2(-10.0, 10.0),
        egui::Align2::RIGHT_TOP,
        format!(
            "Yaw: {:.0}°\nPitch: {:.0}°\nDist: {:.1}",
            camera.yaw.to_degrees(),
            camera.pitch.to_degrees(),
            camera.distance
        ),
        egui::FontId::monospace(11.0),
        egui::Color32::WHITE,
    );
}
