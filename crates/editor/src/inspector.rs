//! Inspector panel for editing entity parameters.

use crate::EditorState;
use assetc::Shape;

/// Render the inspector panel with editable parameters
pub fn inspector_panel(ui: &mut egui::Ui, state: &mut EditorState) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("Entity Properties");
        ui.separator();

        // Transform section
        ui.collapsing("Transform", |ui| {
            transform_widgets(ui, state);
        });

        // Shape section
        ui.collapsing("Shape", |ui| {
            shape_widgets(ui, state);
        });

        // Material section
        ui.collapsing("Material", |ui| {
            material_widgets(ui, state);
        });

        // State section
        ui.collapsing("State", |ui| {
            state_widgets(ui, state);
        });
    });
}

fn transform_widgets(ui: &mut egui::Ui, state: &mut EditorState) {
    let mut changed = false;

    ui.label("Position");
    let mut pos = state.entity_def.position;
    if ui
        .add(egui::Slider::new(&mut pos[0], -10.0..=10.0).text("X"))
        .changed()
    {
        changed = true;
    }
    if ui
        .add(egui::Slider::new(&mut pos[1], -10.0..=10.0).text("Y"))
        .changed()
    {
        changed = true;
    }
    if ui
        .add(egui::Slider::new(&mut pos[2], -10.0..=10.0).text("Z"))
        .changed()
    {
        changed = true;
    }
    if changed {
        state.entity_def.position = pos;
    }

    ui.label("Scale");
    let mut scale = state.entity_def.scale;
    if ui.add(egui::Slider::new(&mut scale, 0.1..=5.0)).changed() {
        state.entity_def.scale = scale;
        changed = true;
    }

    if changed {
        state.mark_dirty();
    }
}

fn shape_widgets(ui: &mut egui::Ui, state: &mut EditorState) {
    let mut changed = false;

    // Clone shape to work with it without holding a mutable borrow
    let shape_clone = state.entity_def.shape.clone();

    match shape_clone {
        Shape::Sphere { r, centre } => {
            ui.label("Sphere Parameters");

            ui.label("Radius");
            let mut radius = r;
            if ui.add(egui::Slider::new(&mut radius, 0.01..=5.0)).changed() {
                if let Shape::Sphere { r, .. } = &mut state.entity_def.shape {
                    *r = radius;
                }
                changed = true;
            }

            ui.label("Centre");
            let mut c = centre;
            if ui
                .add(egui::Slider::new(&mut c[0], -5.0..=5.0).text("X"))
                .changed()
            {
                if let Shape::Sphere { centre, .. } = &mut state.entity_def.shape {
                    *centre = c;
                }
                changed = true;
            }
            if ui
                .add(egui::Slider::new(&mut c[1], -5.0..=5.0).text("Y"))
                .changed()
            {
                if let Shape::Sphere { centre, .. } = &mut state.entity_def.shape {
                    *centre = c;
                }
                changed = true;
            }
            if ui
                .add(egui::Slider::new(&mut c[2], -5.0..=5.0).text("Z"))
                .changed()
            {
                if let Shape::Sphere { centre, .. } = &mut state.entity_def.shape {
                    *centre = c;
                }
                changed = true;
            }
        }
        Shape::RoundedBox { half, r } => {
            ui.label("Rounded Box Parameters");

            ui.label("Half Extents");
            let mut h = half;
            if ui
                .add(egui::Slider::new(&mut h[0], 0.01..=5.0).text("X"))
                .changed()
            {
                if let Shape::RoundedBox { half, .. } = &mut state.entity_def.shape {
                    *half = h;
                }
                changed = true;
            }
            if ui
                .add(egui::Slider::new(&mut h[1], 0.01..=5.0).text("Y"))
                .changed()
            {
                if let Shape::RoundedBox { half, .. } = &mut state.entity_def.shape {
                    *half = h;
                }
                changed = true;
            }
            if ui
                .add(egui::Slider::new(&mut h[2], 0.01..=5.0).text("Z"))
                .changed()
            {
                if let Shape::RoundedBox { half, .. } = &mut state.entity_def.shape {
                    *half = h;
                }
                changed = true;
            }

            ui.label("Corner Radius");
            let mut radius = r;
            if ui.add(egui::Slider::new(&mut radius, 0.0..=2.0)).changed() {
                if let Shape::RoundedBox { r, .. } = &mut state.entity_def.shape {
                    *r = radius;
                }
                changed = true;
            }
        }
        Shape::SmoothUnion { k, .. } => {
            ui.label("Smooth Union Parameters");
            ui.label("Blend factor k");
            let mut blend = k;
            if ui.add(egui::Slider::new(&mut blend, 0.0..=1.0)).changed() {
                if let Shape::SmoothUnion { k, .. } = &mut state.entity_def.shape {
                    *k = blend;
                }
                changed = true;
            }
        }
        Shape::Union { .. } => {
            ui.label("Union (no additional parameters)");
        }
        _ => {
            ui.label("Shape type not fully supported in editor yet");
        }
    }

    if changed {
        state.mark_dirty();
    }
}

fn material_widgets(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.label("Material ID");
    let mut mat_id = state.entity_def.material_id;
    if ui.add(egui::Slider::new(&mut mat_id, 0..=255)).changed() {
        state.entity_def.material_id = mat_id;
        state.mark_dirty();
    }
    ui.label("0 = pig_skin, 1 = wood, 2 = stone");
}

fn state_widgets(ui: &mut egui::Ui, state: &mut EditorState) {
    ui.label("Flags");
    let mut alive = state.entity_def.flags & 0x01 != 0;
    let mut active = state.entity_def.flags & 0x02 != 0;

    if ui.checkbox(&mut alive, "Alive").changed() {
        if alive {
            state.entity_def.flags |= 0x01;
        } else {
            state.entity_def.flags &= !0x01;
        }
        state.mark_dirty();
    }

    if ui.checkbox(&mut active, "Active").changed() {
        if active {
            state.entity_def.flags |= 0x02;
        } else {
            state.entity_def.flags &= !0x02;
        }
        state.mark_dirty();
    }

    ui.label("Quantity");
    let mut qty = state.entity_def.quantity;
    if ui.add(egui::Slider::new(&mut qty, 0..=3)).changed() {
        state.entity_def.quantity = qty;
        state.mark_dirty();
    }
}
