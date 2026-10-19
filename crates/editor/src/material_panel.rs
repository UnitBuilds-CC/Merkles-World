//! Material assignment panel.

use crate::EditorState;

/// Display names for the slots [`entity::MaterialRegistry::with_defaults`] fills. The registry
/// stores no names — `Material` is a render value, not a catalogue entry — so only the labels live
/// here. The colours do not: they are read back out of the registry, which is the same object the
/// viewport shades with, so a swatch cannot promise a colour the render disagrees with.
const NAMED_SLOTS: [(u8, &str); 3] = [(0, "pig_skin"), (1, "wood"), (2, "stone")];

/// Render material selection panel
pub fn material_panel(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.heading("Material Registry");
    ui.separator();

    ui.label("Available Materials:");

    for (id, name) in NAMED_SLOTS {
        let Some(material) = state.materials.get(id) else {
            continue;
        };
        let color = material.base_color;

        let selected = state.entity_def.material_id == id;
        let button = ui.selectable_label(selected, name);

        if button.clicked() {
            state.entity_def.material_id = id;
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
    let current = state.entity_def.material_id;
    let status = match state.materials.get(current) {
        Some(_) => format!("Selected: ID {current}"),
        None => format!("Selected: ID {current} (empty slot — nothing to shade with)"),
    };
    ui.label(status);
}
