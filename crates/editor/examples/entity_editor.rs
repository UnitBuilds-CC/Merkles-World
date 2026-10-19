//! Entity Editor - Full GUI window with eframe (winit + egui + wgpu).
//!
//! Usage: `cargo run -p editor --example entity_editor`
//!
//! Features:
//! - Native window with Dear ImGui UI
//! - 3D viewport with orbit camera (mouse drag to rotate, scroll to zoom)
//! - Inspector panel for editing entity parameters in real-time
//! - Material assignment panel
//! - File menu for load/save/export
//! - RON import/export with dirty tracking

use editor::{EditorState, export::export_menu, inspector::inspector_panel, material_panel::material_panel, viewport::{ViewportCamera, viewport_panel}};
use eframe::{App, CreationContext};

struct EditorApp {
    state: EditorState,
    camera: ViewportCamera,
}

impl EditorApp {
    fn new(_cc: &CreationContext<'_>) -> Self {
        let mut state = EditorState::new();

        // Try to load pig entity if it exists
        if std::path::Path::new("assets/entities/pig.ron").exists() {
            match state.load_ron("assets/entities/pig.ron") {
                Ok(()) => println!("Loaded pig.ron"),
                Err(e) => eprintln!("Warning: {}", e),
            }
        }

        println!("Entity Editor initialized");
        println!("Controls:");
        println!("  - Drag in viewport to orbit camera");
        println!("  - Scroll to zoom");
        println!("  - Use inspector panel to edit parameters");
        println!("  - File menu to load/save entities");

        Self {
            state,
            camera: ViewportCamera::new(),
        }
    }
}

impl App for EditorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Update window title based on dirty state
        ctx.send_viewport_cmd(egui::viewport::ViewportCommand::Title(
            if self.state.dirty {
                "Entity Editor - Phase 2 (modified)".to_string()
            } else {
                "Entity Editor - Phase 2".to_string()
            },
        ));

        // Menu bar
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("File", |ui| {
                    export_menu(ui, &mut self.state);
                });
                ui.separator();
                ui.label("Phase 2 Entity Framework");
            });
        });

        // Inspector panel (right side)
        egui::SidePanel::right("inspector")
            .default_width(300.0)
            .show(ctx, |ui| {
                inspector_panel(ui, &mut self.state);
                
                ui.separator();
                ui.heading("Materials");
                material_panel(ui, &mut self.state);
            });

        // Central viewport
        egui::CentralPanel::default().show(ctx, |ui| {
            let response = viewport_panel(ui, &mut self.camera);

            // Handle mouse wheel for zoom when hovering viewport
            if response.hovered() {
                let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll_delta != 0.0 {
                    self.camera.handle_zoom(scroll_delta * 0.01);
                }
            }
        });

        // Request next frame
        ctx.request_repaint();
    }
}

fn main() {
    env_logger::init();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_title("Entity Editor - Phase 2"),
        ..Default::default()
    };

    println!("Starting Entity Editor...");
    
    eframe::run_native(
        "Entity Editor - Phase 2",
        native_options,
        Box::new(|cc| Ok(Box::new(EditorApp::new(cc)))),
    )
    .unwrap();
}
