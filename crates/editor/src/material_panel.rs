//! Material assignment panel.

use crate::EditorState;

/// Render material selection panel
pub fn material_panel(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.heading("Material Registry");
    ui.separator();

    ui.label("Available Materials:");

    // Show predefined materials
    let materials = [
        (0, "pig_skin", [1.0, 0.7, 0.8]), // Pink
        (1, "wood", [0.6, 0.4, 0.2]),     // Brown
        (2, "stone", [0.5, 0.5, 0.5]),    // Gray
    ];

    for (id, name, color) in &materials {
        let selected = state.entity_def.material_id == *id;
        let button = ui.selectable_label(selected, *name);

        if button.clicked() {
            state.entity_def.material_id = *id;
            state.mark_dirty();
        }

        // Show color swatch
        let rect = button.rect.translate(egui::vec2(100.0, 0.0));
        ui.painter().rect_filled(
            rect,
            2.0,
            egui::Color32::from_rgb(
                (color[0] * 255.0) as u8,
                (color[1] * 255.0) as u8,
                (color[2] * 255.0) as u8,
            ),
        );
    }

    ui.separator();
    ui.label(format!("Selected: ID {}", state.entity_def.material_id));
}
