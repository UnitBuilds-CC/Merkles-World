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

        // The transform, open by default. `ui.collapsing` runs its closure only while the section is
        // open, so a closed Transform meant the position, rotation and scale widgets — the ones an
        // author reaches for first, and the only place the quaternion is shown — did not exist at all
        // until the heading was clicked. Caught by a test that painted the panel and could not find
        // the readout it writes.
        egui::CollapsingHeader::new("Transform")
            .default_open(true)
            .show(ui, |ui| transform_widgets(ui, state));

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

    if rotation_widgets(ui, state) {
        changed = true;
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

/// The rotation as an axis and a turn in degrees, which is what an author picks up, written into the
/// `[x, y, z, w]` the file carries. `EntityDef.rotation`'s representation is OQ-63's to settle and this does not
/// settle it — the panel only composes the field that already exists. The quaternion comes out unit by
/// construction, which is what `gpu_renderer::compile_entity` requires of it: a non-unit one is refused into
/// `NOT_DRAWN`, so a widget that wrote a raw four-tuple could hand the viewport a shape it will not march.
///
/// Returns whether the entity changed.
fn rotation_widgets(ui: &mut egui::Ui, state: &mut EditorState) -> bool {
    let authored = state.entity_def.rotation;
    let (mut axis, mut degrees) = axis_angle(authored);
    let mut requested = false;

    ui.label("Rotation");
    if ui
        .add(egui::Slider::new(&mut degrees, -180.0..=180.0).text("turn"))
        .changed()
    {
        requested = true;
    }
    for (label, component) in ["X", "Y", "Z"].into_iter().zip(axis.iter_mut()) {
        if ui
            .add(egui::Slider::new(component, -1.0..=1.0).text(label))
            .changed()
        {
            requested = true;
        }
    }
    ui.horizontal(|ui| {
        for (name, preset) in [
            ("none", ([0.0, 1.0, 0.0], 0.0)),
            ("¼ Y", ([0.0, 1.0, 0.0], 90.0)),
            ("½ Y", ([0.0, 1.0, 0.0], 180.0)),
            ("¼ X", ([1.0, 0.0, 0.0], 90.0)),
            ("¼ Z", ([0.0, 0.0, 1.0], 90.0)),
        ] {
            if ui.button(name).clicked() {
                axis = preset.0;
                degrees = preset.1;
                requested = true;
            }
        }
    });

    let wrote = requested && apply_rotation(state, axis, degrees);
    if !wrote && requested {
        ui.label(
            egui::RichText::new("A zero-length axis carries no turn — pick one.")
                .color(egui::Color32::YELLOW),
        );
    }
    let current = state.entity_def.rotation;
    ui.label(
        egui::RichText::new(format!(
            "[{:.4}, {:.4}, {:.4}, {:.4}] as the file stores it",
            current[0], current[1], current[2], current[3]
        ))
        .monospace()
        .weak(),
    );
    if !is_unit(current) {
        ui.label(
            egui::RichText::new(
                "this quaternion is not unit, so the viewport refuses to march the entity until it is",
            )
            .color(egui::Color32::RED),
        );
    }
    wrote
}

/// Write `axis` turned by `degrees` into the entity's rotation. `false` means nothing was written, which is
/// the case a caller has to be able to tell — a panel that marked the file dirty while leaving the transform
/// alone would offer a save of the shape the author never made.
fn apply_rotation(state: &mut EditorState, axis: [f32; 3], degrees: f32) -> bool {
    let length = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    if length < 1.0e-6 {
        return false;
    }
    // Half the angle, because the vector part of a quaternion is the axis scaled by sin(θ/2). `sin` and `cos`
    // are legitimate here and nowhere the ledger reads: a rotation is a presentation float, never hashed
    // (docs/02's float discipline), and this is a tool (AGENTS.md §3).
    let half = degrees.to_radians() * 0.5;
    let scale = half.sin() / length;
    state.entity_def.rotation = [
        axis[0] * scale,
        axis[1] * scale,
        axis[2] * scale,
        half.cos(),
    ];
    true
}

/// The rotation as its own axis and angle in degrees. A turn of zero has no axis to report, so it is shown as
/// the panel's default rather than as three zeros the author would then have to fill in.
fn axis_angle(quat: [f32; 4]) -> ([f32; 3], f32) {
    let sin_half = (quat[0] * quat[0] + quat[1] * quat[1] + quat[2] * quat[2]).sqrt();
    let angle = 2.0 * sin_half.atan2(quat[3]);
    let axis = if sin_half > 1.0e-6 {
        [quat[0] / sin_half, quat[1] / sin_half, quat[2] / sin_half]
    } else {
        [0.0, 1.0, 0.0]
    };
    (axis, angle.to_degrees())
}

fn is_unit(quat: [f32; 4]) -> bool {
    let squared = quat[0] * quat[0] + quat[1] * quat[1] + quat[2] * quat[2] + quat[3] * quat[3];
    (squared - 1.0).abs() < 1.0e-3
}

/// The tree as a list, each row labelled with the route the authored file gives it.
fn parts_list(ui: &mut egui::Ui, state: &mut EditorState) {
    let entries = shape_path::outline(&state.entity_def.shape);
    let mut selected = state.selected.clone();
    ui.label(format!("{} nodes", entries.len()));
    // Routes past ~40 characters no longer fit the panel's default width, and a label that is silently
    // truncated is a part the author cannot find by reading. Wrapping turns a deep row into a tall one
    // instead, which the scroll area can show all of.
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
    for entry in entries {
        ui.horizontal(|ui| {
            // Indentation in pixels, not a spacer of spaces: at the fourth pass's depth of 14 the old
            // four-spaces-per-level label was 56 characters of blank text, which wrapped on its own and
            // left under 20 px for the route. `Ui::indent` is still wrong here, because it lays out
            // relative to the row's own height and drifts once the rows carry different text.
            ui.add_space(entry.depth as f32 * 6.0);
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
///
/// The rows come from `crate::params`, the same table the MCP pipe validates against, so a range cannot
/// be one thing in the window and another over the wire.
fn edit_params(ui: &mut egui::Ui, shape: &mut Shape) -> bool {
    let specs = crate::params::params_of(shape);
    if specs.is_empty() {
        ui.label("No parameters of its own — pick one of its children.");
        return false;
    }

    let mut changed = false;
    for spec in specs {
        let Some(mut lanes) = crate::params::read_param(shape, spec.name) else {
            continue;
        };
        let mut moved = false;
        if spec.lanes == 1 {
            let mut value = lanes[0];
            if spec.whole {
                // The axis is a field of the grammar, not a length: the drag rounds to the lane the
                // file will carry rather than storing 1.4 and letting the march guess.
                if ui
                    .add(egui::Slider::new(&mut value, spec.lo..=spec.hi).text(spec.label))
                    .changed()
                {
                    lanes[0] = value.round();
                    moved = true;
                }
            } else if slider(ui, &mut value, spec.lo..=spec.hi, spec.label) {
                lanes[0] = value;
                moved = true;
            }
        } else {
            ui.label(spec.label);
            for (lane, axis) in (0..spec.lanes as usize).zip(["X", "Y", "Z"]) {
                let mut value = lanes[lane];
                if slider(ui, &mut value, spec.lo..=spec.hi, axis) {
                    lanes[lane] = value;
                    moved = true;
                }
            }
        }
        if moved && crate::params::write_param(shape, spec.name, &lanes).is_some() {
            changed = true;
        }
    }
    changed
}

fn slider(ui: &mut egui::Ui, value: &mut f32, range: RangeInclusive<f32>, text: &str) -> bool {
    ui.add(egui::Slider::new(value, range).text(text)).changed()
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
    ///
    /// The rect is taller than any window this editor opens on, because egui culls the rows below the
    /// fold of a scroll area — a test that asked for all of the pig's 74 nodes has to give the probe
    /// room to paint them, and the fourth-pass ears pushed the list past 2048. What is at the fold in
    /// the real window is the author's to scroll to; what this probe can say is whether the panel
    /// produces the row at all.
    fn painted(state: &mut EditorState) -> Vec<String> {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 4096.0),
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

    /// The readout is the quaternion the file will carry, not the numbers in the sliders: the two differ
    /// whenever the authored rotation is not a whole turn about a primary axis, and an author who cannot
    /// see which one the viewport will get has no reason to trust the widgets above it.
    #[test]
    fn the_panel_shows_the_quaternion_the_file_will_carry() {
        let mut state = pig_state();
        let half_turn_sin = (core::f32::consts::FRAC_PI_4).sin();
        let authored = [0.0, half_turn_sin, 0.0, half_turn_sin];
        state.entity_def.rotation = authored;

        let expected = format!(
            "[{:.4}, {:.4}, {:.4}, {:.4}] as the file stores it",
            authored[0], authored[1], authored[2], authored[3]
        );
        let labels = painted(&mut state);
        assert!(
            labels.iter().any(|l| l.contains(&expected)),
            "the panel never printed the quaternion it holds; it painted {labels:?}"
        );
        // Reading the panel must not quieten a warning by fixing the value it is warning about.
        assert_eq!(state.entity_def.rotation, authored);
        assert!(!state.dirty);

        // The refusal `gpu_renderer::compile_entity` makes is said here, before the author saves.
        let mut warped = pig_state();
        warped.entity_def.rotation = [0.0, 2.0, 0.0, 1.0];
        let labels = painted(&mut warped);
        assert!(
            labels.iter().any(|l| l.contains("not unit")),
            "a quaternion the viewport refuses to march was painted without a warning"
        );
        assert_eq!(warped.entity_def.rotation, [0.0, 2.0, 0.0, 1.0]);

        // Identity, which every shipped entity starts on, carries no warning.
        let labels = painted(&mut pig_state());
        assert!(
            !labels.iter().any(|l| l.contains("not unit")),
            "the panel called the authored identity quaternion non-unit"
        );
    }

    /// The widget's whole job in one place: write a *unit* quaternion (the viewport refuses anything else),
    /// leave the entity alone when the axis points nowhere, and agree with itself on the round trip.
    #[test]
    fn writing_a_turn_produces_the_quaternion_the_viewport_accepts() {
        let mut state = pig_state();
        let before = state.preview_signature();

        assert!(apply_rotation(&mut state, [0.0, 1.0, 0.0], 90.0));
        let half = core::f32::consts::FRAC_PI_4;
        assert_eq!(
            state.entity_def.rotation,
            [0.0, half.sin(), 0.0, half.cos()]
        );
        assert!(
            is_unit(state.entity_def.rotation),
            "the widget wrote a quaternion the viewport would refuse: {:?}",
            state.entity_def.rotation
        );
        assert_ne!(
            state.preview_signature(),
            before,
            "a turn the viewport renders left the preview key alone, so the editor would keep showing the old frame"
        );

        // An unnormalised axis is the same direction, so it must come out the same quaternion.
        let mut long = pig_state();
        assert!(apply_rotation(&mut long, [0.0, 3.0, 0.0], 90.0));
        assert_eq!(long.entity_def.rotation, state.entity_def.rotation);

        // An axis with no length has no direction to turn about, and writing zero for w would be a
        // half-turn the author never asked for.
        let mut stateless = pig_state();
        stateless.entity_def.rotation = [0.0, 0.0, 0.0, 1.0];
        assert!(
            !apply_rotation(&mut stateless, [0.0, 0.0, 0.0], 90.0),
            "a directionless axis reported that it wrote something"
        );
        assert_eq!(stateless.entity_def.rotation, [0.0, 0.0, 0.0, 1.0]);

        // Round trip: read back what the panel would show for what it just wrote.
        for (axis, degrees) in [
            ([0.0, 1.0, 0.0], 90.0),
            ([1.0, 2.0, -1.0], 37.0),
            ([0.0, 0.0, 1.0], 180.0),
        ] {
            let mut state = pig_state();
            assert!(apply_rotation(&mut state, axis, degrees));
            let (read_axis, read_degrees) = axis_angle(state.entity_def.rotation);
            let length = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
            for (component, expected) in read_axis.iter().zip(axis) {
                assert!(
                    (component - expected / length).abs() < 1.0e-6,
                    "{}° about {:?} came back about {:?}",
                    degrees,
                    axis,
                    read_axis
                );
            }
            assert!(
                (read_degrees - degrees).abs() < 1.0e-3,
                "{}° about {:?} came back as {read_degrees}°",
                degrees,
                axis
            );
        }

        // Turning the other way is the same as turning about the opposite axis — the identity the two
        // sliders lean on when they show a negative angle.
        let mut clockwise = pig_state();
        assert!(apply_rotation(&mut clockwise, [1.0, 0.0, 0.0], -60.0));
        let mut flipped = pig_state();
        assert!(apply_rotation(&mut flipped, [-1.0, 0.0, 0.0], 60.0));
        assert_eq!(clockwise.entity_def.rotation, flipped.entity_def.rotation);
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
