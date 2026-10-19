//! Inspector panel for editing entity parameters.

use core::ops::RangeInclusive;

use crate::shape_path;

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

        // The tree, so a part can be picked rather than the root being edited blind. Open by default:
        // which part the sliders below belong to is not something to hide behind a click.
        egui::CollapsingHeader::new("Parts")
            .default_open(true)
            .show(ui, |ui| parts_list(ui, state));

        // Parameters of whichever node is selected.
        egui::CollapsingHeader::new("Selected part")
            .default_open(true)
            .show(ui, |ui| selected_widgets(ui, state));

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

/// The tree as a list, each row labelled with the route the authored file gives it.
fn parts_list(ui: &mut egui::Ui, state: &mut EditorState) {
    let entries = shape_path::outline(&state.entity_def.shape);
    let mut selected = state.selected.clone();
    ui.label(format!("{} nodes", entries.len()));
    for entry in entries {
        ui.horizontal(|ui| {
            // Indentation by a monospace spacer rather than `Ui::indent`, which lays out relative to
            // the row's own height and drifts once the rows carry different text.
            if entry.depth > 0 {
                ui.label(
                    egui::RichText::new("    ".repeat(entry.depth))
                        .monospace()
                        .weak(),
                );
            }
            let mut label = format!("{} [{}]", entry.name, entry.kind);
            if !entry.summary.is_empty() {
                label.push_str(&format!(" · {}", entry.summary));
            }
            let response = ui.selectable_value(&mut selected, entry.path.clone(), label);
            if response.changed() {
                ui.ctx().request_repaint();
            }
        });
    }
    state.selected = selected;
}

fn selected_widgets(ui: &mut egui::Ui, state: &mut EditorState) {
    let Some(node) = state.selected_shape() else {
        // Reachable: the tree was rebuilt under the selection (a load, or an author replacing the root
        // variant). Naming that is the whole point — the alternative is sliders that quietly edit the
        // root while the author believes they are moving the ear.
        ui.label("The selected node is no longer in the tree.");
        if ui.button("Select the root").clicked() {
            state.selected = Vec::new();
            ui.ctx().request_repaint();
        }
        return;
    };
    let route = if state.selected.is_empty() {
        "root".to_string()
    } else {
        shape_path::outline(&state.entity_def.shape)
            .into_iter()
            .find(|e| e.path == state.selected)
            .map(|e| e.name)
            .unwrap_or_else(|| "?".to_string())
    };
    let kind = node.clone();
    ui.label(format!("{route} — {}", shape_path::kind(&kind)));

    let mut edited = kind;
    if edit_params(ui, &mut edited) {
        if let Some(slot) = state.selected_shape_mut() {
            *slot = edited;
        }
        state.mark_dirty();
    }
}

/// Every parameter the node itself carries, and `true` if any of them moved. The node is a clone that
/// gets written back whole, so a combinator keeps its children and no field-by-field write-back can
/// miss one.
fn edit_params(ui: &mut egui::Ui, shape: &mut Shape) -> bool {
    let mut changed = false;
    match shape {
        Shape::Sphere { r, centre } => {
            changed |= slider(ui, r, 0.01..=5.0, "radius");
            changed |= vec3(ui, centre, -5.0..=5.0, "centre");
        }
        Shape::RoundedBox { half, r } => {
            changed |= vec3(ui, half, 0.01..=5.0, "half extents");
            changed |= slider(ui, r, 0.0..=2.0, "corner radius");
        }
        Shape::Plane { normal, offset } => {
            changed |= vec3(ui, normal, -1.0..=1.0, "normal");
            changed |= slider(ui, offset, -10.0..=10.0, "offset");
        }
        Shape::SmoothUnion { k, .. } => {
            changed |= slider(ui, k, 0.0..=1.0, "blend k");
        }
        Shape::Repeat { period, .. } => {
            changed |= slider(ui, period, 0.05..=10.0, "period");
        }
        Shape::Mirror { axis, .. } => {
            let mut a = *axis as f32;
            if ui
                .add(egui::Slider::new(&mut a, 0.0..=2.0).text("axis (x y z)"))
                .changed()
            {
                *axis = a.round() as u8;
                changed = true;
            }
        }
        Shape::Union { .. } | Shape::Subtract { .. } | Shape::Intersect { .. } => {
            ui.label("No parameters of its own — pick one of its children.");
        }
    }
    changed
}

fn slider(ui: &mut egui::Ui, value: &mut f32, range: RangeInclusive<f32>, text: &str) -> bool {
    ui.add(egui::Slider::new(value, range).text(text)).changed()
}

fn vec3(ui: &mut egui::Ui, v: &mut [f32; 3], range: RangeInclusive<f32>, label: &str) -> bool {
    ui.label(label);
    let mut changed = false;
    for (axis, value) in ["X", "Y", "Z"].into_iter().zip(v.iter_mut()) {
        changed |= slider(ui, value, range.clone(), axis);
    }
    changed
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One headless egui pass over the panel, and every string it painted.
    ///
    /// The panel is the part of the editor a test over the state types cannot reach: a route can
    /// resolve perfectly and still never be listed, and a widget can mark the entity dirty just by
    /// being looked at. egui lays out and paints without a device, so the pass itself is what is
    /// under test here.
    fn painted(state: &mut EditorState) -> Vec<String> {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 900.0),
            )),
            ..Default::default()
        };
        let out = ctx.run(raw, |ctx| {
            egui::SidePanel::right("inspector")
                .default_width(300.0)
                .show(ctx, |ui| inspector_panel(ui, state));
        });
        out.shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    fn pig_state() -> EditorState {
        let mut state = EditorState::new();
        state.entity_def = ron::from_str(include_str!("../../../assets/entities/pig.ron"))
            .expect("pig.ron does not parse");
        state
    }

    #[test]
    fn every_part_of_the_shipped_pig_is_listed_where_the_author_can_see_it() {
        let mut state = pig_state();
        let routes: Vec<String> = shape_path::outline(&state.entity_def.shape)
            .iter()
            .map(|e| format!("{} [{}]", e.name, e.kind))
            .collect();
        assert!(
            routes.iter().any(|r| r.matches('.').count() >= 3),
            "pig.ron is not deep enough for this to be a test of reaching a part"
        );

        let labels = painted(&mut state);
        for route in &routes {
            assert!(
                labels.iter().any(|l| l.starts_with(route)),
                "the tree holds a node at {route} that the panel never painted"
            );
        }
    }

    #[test]
    fn looking_at_the_panel_does_not_edit_the_animal() {
        let mut state = pig_state();
        // Start from a part rather than the root: an assertion written against an already-empty route
        // cannot see a panel that resets the selection to the root.
        let deep = shape_path::outline(&state.entity_def.shape)
            .into_iter()
            .find(|e| e.depth >= 2)
            .map(|e| e.path)
            .expect("pig.ron has no node two levels down");
        state.select(deep.clone());
        let signature = state.preview_signature();

        let labels = painted(&mut state);
        assert!(!labels.is_empty(), "the panel painted nothing at all");
        assert_eq!(
            state.preview_signature(),
            signature,
            "painting the panel moved the preview key, so looking at the animal costs a march"
        );
        assert_eq!(
            state.selected, deep,
            "painting the panel moved the selection off the part the author had picked"
        );
        assert!(
            !state.dirty,
            "painting the panel marked the entity modified"
        );
    }

    /// Each variant's own parameters, named the way `edit_params` writes them.
    #[test]
    fn each_variant_offers_its_own_parameters() {
        let leaf = || {
            Box::new(Shape::Sphere {
                r: 0.4,
                centre: [0.0, 0.0, 0.0],
            })
        };
        let cases = [
            (
                Shape::Sphere {
                    r: 0.4,
                    centre: [0.0, 0.0, 0.0],
                },
                "radius",
            ),
            (
                Shape::RoundedBox {
                    half: [0.4, 0.4, 0.4],
                    r: 0.1,
                },
                "half extents",
            ),
            (
                Shape::Plane {
                    normal: [0.0, 1.0, 0.0],
                    offset: 0.0,
                },
                "offset",
            ),
            (
                Shape::SmoothUnion {
                    a: leaf(),
                    b: leaf(),
                    k: 0.1,
                },
                "blend k",
            ),
            (
                Shape::Repeat {
                    period: 2.0,
                    child: leaf(),
                },
                "period",
            ),
            (
                Shape::Mirror {
                    axis: 1,
                    child: leaf(),
                },
                "axis (x y z)",
            ),
        ];
        for (shape, expected) in cases {
            let mut state = EditorState::new();
            state.entity_def.shape = shape;
            let labels = painted(&mut state);
            assert!(
                labels.iter().any(|l| l.contains(expected)),
                "the panel did not offer {expected} for {}",
                shape_path::kind(&state.entity_def.shape)
            );
            assert!(
                !state.dirty,
                "offering {}'s sliders marked the entity modified",
                shape_path::kind(&state.entity_def.shape)
            );
        }

        // The three combinators carry nothing of their own, and say so rather than showing widgets
        // that would edit a value that is not there.
        for (shape, name) in [
            (
                Shape::Union {
                    a: leaf(),
                    b: leaf(),
                },
                "Union",
            ),
            (
                Shape::Subtract {
                    kept: leaf(),
                    cutter: leaf(),
                },
                "Subtract",
            ),
            (
                Shape::Intersect {
                    a: leaf(),
                    b: leaf(),
                },
                "Intersect",
            ),
        ] {
            let mut state = EditorState::new();
            state.entity_def.shape = shape;
            let labels = painted(&mut state);
            assert!(
                labels
                    .iter()
                    .any(|l| l.starts_with("No parameters of its own")),
                "{name} was offered sliders for parameters it does not have, or nothing at all"
            );
        }
    }
}
