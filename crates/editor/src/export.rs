//! Export functionality for saving edited entities.

use crate::EditorState;

/// Handle export UI in the top menu bar
pub fn export_menu(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.menu_button("File", |ui| {
        if ui.button("New Entity").clicked() {
            *state = EditorState::new();
            ui.close_menu();
        }

        if ui.button("Load RON...").clicked() {
            // In a full implementation, this would open a file dialog
            // For now, load the default pig entity
            match state.load_ron("assets/entities/pig.ron") {
                Ok(()) => println!("Loaded pig.ron"),
                Err(e) => eprintln!("Failed to load: {}", e),
            }
            ui.close_menu();
        }

        ui.separator();

        if ui.button("Save").clicked() {
            if let Some(path) = &state.file_path {
                match state.export_ron(path) {
                    Ok(()) => {
                        state.dirty = false;
                        println!("Saved to {}", path);
                    }
                    Err(e) => eprintln!("Failed to save: {}", e),
                }
            } else {
                // Save as new file
                let path = "assets/entities/edited_entity.ron";
                match state.export_ron(path) {
                    Ok(()) => {
                        state.file_path = Some(path.to_string());
                        state.dirty = false;
                        println!("Saved to {}", path);
                    }
                    Err(e) => eprintln!("Failed to save: {}", e),
                }
            }
            ui.close_menu();
        }

        if ui.button("Save As...").clicked() {
            let path = "assets/entities/exported_entity.ron";
            match state.export_ron(path) {
                Ok(()) => {
                    state.file_path = Some(path.to_string());
                    state.dirty = false;
                    println!("Exported to {}", path);
                }
                Err(e) => eprintln!("Failed to export: {}", e),
            }
            ui.close_menu();
        }
    });

    // Show dirty indicator
    if state.dirty {
        ui.label("(modified)");
    }
}
