//! The editor's state over MCP, so an agent can drive the same object the window draws from.
//!
//! §7 says a build log is not evidence, and `examples/render_png` proved that by writing the frames out —
//! but it is a command that has to be re-run with a different argument for every question, and it cannot
//! answer a question about state (`what is the snout's route?`, `did the edit land?`) at all. This module
//! is the same `EditorState` the egui panel edits, behind one function: `handle` takes a JSON-RPC line and
//! returns the reply, so the transport can be a pipe, a test, or a socket, and the protocol layer is
//! testable without a window, a device, or a human.
//!
//! The PNG dumps are verification artifacts and go to `temp_dir`, never into the repo (§6, §8).

use std::path::PathBuf;

use serde_json::{json, Value};

use render::raster::Frame;

use crate::gpu_renderer::{CpuRenderer, Stage};
use crate::params;
use crate::shape_path::{self, Step};
use crate::EditorState;
use assetc::Shape;
use entity::{Material, NoiseKind};

/// What `initialize` answers with. MCP clients refuse a server that speaks a version they do not know.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The six angles the viewport can be turned to, as `(name, yaw, pitch, distance)`.
///
/// Yaw 0 puts the camera on +Z, so a subject whose snout sits at negative Z faces the camera near `PI`.
/// Distances are set so the subject fills the frame: the camera's vertical span at `d` is `d` world units
/// with `half_height: 0.5`. `examples/render_png` and `render_view` read this one table, because two
/// angle lists that drift would give a reader a different picture than the dump they were told about.
pub const VIEWS: &[(&str, f32, f32, f32)] = &[
    ("front", core::f32::consts::PI, 0.12, 1.5),
    ("back", 0.0, 0.12, 1.5),
    ("side", core::f32::consts::FRAC_PI_2, 0.12, 1.5),
    ("three_quarter", -2.4, 0.30, 1.5),
    // The low view sat at 1.1, closer than the rest, and cropped the animal it exists to judge: the
    // edge of the frame cut through the shoulder, so a leg or a belly it was meant to show was not in
    // the picture. It shares the others' distance now — the pitch is what makes it low.
    ("low", -2.4, -0.30, 1.5),
    ("top", 0.0, 1.15, 1.6),
];

/// The dump directory, and the only place a rendered frame is written.
///
/// Named for this process, which is what keeps a stale dump out of a caller's hands: the frames from the
/// last run are in the last run's directory, so the path handed back here cannot be an older session's
/// picture of a state that no longer exists. Sharing one name between processes would make that a race
/// on who wrote last.
pub fn dump_dir() -> PathBuf {
    std::env::temp_dir().join(format!("merkle_editor_mcp_{}", std::process::id()))
}

/// Every tool the server claims. `tools/list` is built from this, and a test refuses a name here that
/// `dispatch` does not answer for — the way `outline` refuses a node the panel cannot reach.
pub const TOOL_NAMES: &[&str] = &[
    "editor_state",
    "list_materials",
    "set_material",
    "register_material",
    "load_entity",
    "set_transform",
    "select_part",
    "set_part",
    "add_part",
    "delete_part",
    "move_part",
    "graft_part",
    "render_view",
    "save_entity",
];

/// One JSON-RPC line in, its reply out. `None` for notifications, which must draw no response.
pub fn handle(state: &mut EditorState, line: &str) -> Option<String> {
    let request: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(e) => {
            return Some(error(
                Value::Null,
                -32700,
                &format!("the line is not JSON: {e}"),
            ))
        }
    };
    let id = request.get("id").cloned();
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return id.map(|id| error(id, -32600, "a request must name a method"));
    };
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

    match method {
        "initialize" => Some(result(
            id.unwrap_or(Value::Null),
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "merkle-editor",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )),
        "ping" => Some(result(id.unwrap_or(Value::Null), json!({}))),
        "tools/list" => Some(result(
            id.unwrap_or(Value::Null),
            json!({ "tools": tool_specs() }),
        )),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let outcome = match dispatch(state, name, &arguments) {
                Some(outcome) => outcome,
                None => {
                    return id.map(|id| {
                        error(
                            id,
                            -32602,
                            &format!(
                                "no tool named {name:?} — tools/list names the ones that exist"
                            ),
                        )
                    })
                }
            };
            let value = match outcome {
                Ok(value) => json!({
                    "content": [{ "type": "text", "text": value.to_string() }],
                    "isError": false,
                }),
                Err(message) => json!({
                    "content": [{ "type": "text", "text": message }],
                    "isError": true,
                }),
            };
            id.map(|id| result(id, value))
        }
        other if other.starts_with("notifications/") => None,
        other => Some(error(
            id.unwrap_or(Value::Null),
            -32601,
            &format!("this server has no method {other:?}"),
        )),
    }
}

fn dispatch(
    state: &mut EditorState,
    name: &str,
    arguments: &Value,
) -> Option<Result<Value, String>> {
    match name {
        "editor_state" => Some(Ok(state_json(state))),
        "list_materials" => Some(Ok(materials_json(state))),
        "set_material" => Some(set_material(state, arguments)),
        "register_material" => Some(register_material(state, arguments)),
        "load_entity" => Some(load(state, arguments)),
        "set_transform" => Some(set_transform(state, arguments)),
        "select_part" => Some(select_part(state, arguments)),
        "set_part" => Some(set_part(state, arguments)),
        "add_part" => Some(add_part(state, arguments)),
        "delete_part" => Some(delete_part(state, arguments)),
        "move_part" => Some(move_part(state, arguments)),
        "graft_part" => Some(graft_part(state, arguments)),
        "render_view" => Some(render_view(state, arguments)),
        "save_entity" => Some(save_entity(state, arguments)),
        _ => None,
    }
}

fn load(state: &mut EditorState, arguments: &Value) -> Result<Value, String> {
    let path = string_arg(arguments, "path")?;
    state.load_ron(&path)?;
    Ok(json!({ "loaded": path, "state": state_json(state) }))
}

fn save_entity(state: &EditorState, arguments: &Value) -> Result<Value, String> {
    // The path is required rather than defaulted to the file the state came from: that file is the
    // engine's own input, and a tool should not be able to rewrite it by omission.
    let path = string_arg(arguments, "path")?;
    state.export_ron(&path)?;
    Ok(json!({ "saved": path, "bytes": std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) }))
}

/// Apply the authored values a preview reads, refusing a whole call before any of it lands.
///
/// The tolerances are `assetc::compile_entity`'s, not invented here: a quaternion the compiler would
/// reject must be rejected at the panel and at this pipe alike, or an edit can look applied and then
/// fail to compile.
fn set_transform(state: &mut EditorState, arguments: &Value) -> Result<Value, String> {
    let position = match arguments.get("position") {
        Some(value) => Some([
            element(value, 0, "position")?,
            element(value, 1, "position")?,
            element(value, 2, "position")?,
        ]),
        None => None,
    };
    let rotation = match arguments.get("rotation") {
        Some(value) => Some([
            element(value, 0, "rotation")?,
            element(value, 1, "rotation")?,
            element(value, 2, "rotation")?,
            element(value, 3, "rotation")?,
        ]),
        None => None,
    };
    let scale = match arguments.get("scale") {
        Some(value) => Some(scalar(value, "scale")?),
        None => None,
    };
    let material_id = match arguments.get("material_id") {
        Some(value) => {
            let slot = scalar(value, "material_id")?;
            if slot != slot.trunc() || !(0.0..=255.0).contains(&slot) {
                return Err(format!(
                    "material_id must be a whole slot 0-255, got {slot}"
                ));
            }
            let slot = slot as u8;
            if !state.materials.is_occupied(slot) {
                // The viewport renders an empty slot as unlit white and prints a warning no caller of
                // this tool would see, so the assignment is refused rather than drawn.
                return Err(format!(
                    "material slot {slot} is empty; list_materials names the ones that are not"
                ));
            }
            Some(slot)
        }
        None => None,
    };

    // Checked in `compile_entity`'s order: a non-finite lane satisfies `(len_sq - 1.0).abs() > 1e-4` too,
    // and reporting that as a normalisation mistake would send the caller off to fix the wrong thing.
    if let Some(rotation) = rotation {
        if !rotation.iter().all(|lane| lane.is_finite()) {
            return Err(format!("rotation lanes must be finite, got {rotation:?}"));
        }
        let len_sq = rotation.iter().map(|x| x * x).sum::<f32>();
        if (len_sq - 1.0).abs() > 1e-4 {
            return Err(format!(
                "rotation must be a unit quaternion, this one has |q|² = {len_sq:.6}"
            ));
        }
    }
    if let Some(scale) = scale {
        if !scale.is_finite() {
            return Err(format!("scale must be finite, got {scale}"));
        }
        if scale <= 0.0 {
            return Err(format!("scale must be positive, got {scale}"));
        }
    }
    if let Some([x, y, z]) = position {
        if !x.is_finite() || !y.is_finite() || !z.is_finite() {
            return Err(format!("position must be finite, got [{x}, {y}, {z}]"));
        }
    }

    let mut changed = Vec::new();
    if let Some(position) = position {
        if state.entity_def.position != position {
            state.entity_def.position = position;
            changed.push("position");
        }
    }
    if let Some(rotation) = rotation {
        if state.entity_def.rotation != rotation {
            state.entity_def.rotation = rotation;
            changed.push("rotation");
        }
    }
    if let Some(scale) = scale {
        if state.entity_def.scale != scale {
            state.entity_def.scale = scale;
            changed.push("scale");
        }
    }
    if let Some(material_id) = material_id {
        if state.entity_def.material_id != material_id {
            state.entity_def.material_id = material_id;
            changed.push("material_id");
        }
    }
    if !changed.is_empty() {
        state.mark_dirty();
    }

    Ok(json!({
        "changed": changed,
        "preview_signature": state.preview_signature(),
        "state": state_json(state),
    }))
}

/// Write fields of an occupied material slot, and say which of them the renderer can see.
///
/// Two facts about this state are invisible to the caller, so both go into the reply rather than into a
/// description it may not have read. First, of the six authored fields only `base_color` reaches a pixel:
/// doc 04 § Materials, lighting, sky leaves the shading model to OQ-10, and the other five have no reader
/// to disagree with. Second, the registry is not part of the entity file — `save_entity` carries the
/// entity's `material_id`, not the fields of the slot it points at — so nothing written here survives a
/// reload. Both are measured by tests, one per fact, because a notice that only *describes* a claim is
/// worth as much as the claim.
///
/// The limits come from `params::MATERIAL_FIELDS`, which is the same table `editor_state`'s two lists are
/// derived from, so a refusal and a published list cannot describe different fields.
fn set_material(state: &mut EditorState, arguments: &Value) -> Result<Value, String> {
    let raw = number(arguments, "slot")?;
    if !raw.is_finite() || raw != raw.trunc() || !(0.0..=255.0).contains(&raw) {
        return Err(format!("\"slot\" must be a whole slot 0-255, got {raw}"));
    }
    let slot = raw as u8;
    let Some(current) = state.materials.get(slot).copied() else {
        let occupied: Vec<String> = (0..=255u8)
            .filter(|other| state.materials.is_occupied(*other))
            .map(|other| other.to_string())
            .collect();
        return Err(format!(
            "material slot {slot} is empty. `set_material` writes a slot that exists — filling an empty \
             one needs every field at once, which is what `register` does in code and not what a patch \
             means. Registered slots: {}",
            if occupied.is_empty() {
                "none".to_string()
            } else {
                occupied.join(", ")
            }
        ));
    };

    // A key that is neither `slot` nor a field of the table is a caller that believes it is writing
    // something. Silently dropping it is the exact drift the `params` table exists to prevent.
    let known: Vec<&str> = params::MATERIAL_FIELDS
        .iter()
        .map(|field| field.name)
        .collect();
    if let Some(map) = arguments.as_object() {
        for key in map.keys() {
            if key != "slot" && !known.contains(&key.as_str()) {
                return Err(format!(
                    "a material has no field {key:?}; it has {}",
                    known
                        .iter()
                        .map(|name| format!("{name:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }

    // A call with `slot` and no field is not a patch, and answering it as one would print the no-op
    // sentence about values that were never handed over. The list it refuses with is the list the call
    // needed.
    if !params::MATERIAL_FIELDS
        .iter()
        .any(|field| arguments.get(field.name).is_some())
    {
        return Err(format!(
            "no field to write: `set_material` takes `slot` plus at least one of {}",
            known
                .iter()
                .map(|name| format!("{name:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let mut next = current;
    let mut changed: Vec<&'static str> = Vec::new();
    for field in &params::MATERIAL_FIELDS {
        let Some(lanes) = material_field_lanes(arguments, field)? else {
            continue;
        };
        if params::read_material_field(&next, field.name).as_deref() != Some(lanes.as_slice()) {
            changed.push(field.name);
        }
        params::write_material_field(&mut next, field.name, &lanes)
            .expect("a field the table lists is a field the writer knows");
    }

    let before = if changed.is_empty() {
        current
    } else {
        // `current` was read out of this slot above and nothing between the two can empty a registry, so
        // the write lands: a `None` here would be a bug in `set`, not a caller's mistake.
        state
            .materials
            .set(slot, next)
            .expect("an occupied slot stays occupied")
    };

    let (read, _) = params::material_names_by_shading();
    let pixels: Vec<&str> = changed
        .iter()
        .copied()
        .filter(|name| read.contains(name))
        .collect();
    let no_pixels: Vec<&str> = changed
        .iter()
        .copied()
        .filter(|name| !read.contains(name))
        .collect();
    let mut notice = String::from(
        "the registry is not part of the entity file: save_entity writes the entity's `material_id`, not \
         these fields, so loading the file again restores the material this slot was registered with. \
         Assign the slot with `set_transform` if the choice is what you meant to keep.",
    );
    if changed.is_empty() {
        notice.push_str(
            " Every value you wrote is the one the slot already carried, so nothing moved.",
        );
    }
    if !no_pixels.is_empty() {
        notice.push_str(&format!(
            " Of what this call wrote, these have no reader: {}.{} Doc 04 § Materials, lighting, sky \
             leaves the shading model to OQ-10, so `base_color` is the only field a shader reads: the \
             names above change no pixel and no preview key while the registry holds them anyway.",
            quoted(&no_pixels),
            if pixels.is_empty() {
                String::new()
            } else {
                format!(" These do reach a pixel: {}.", quoted(&pixels))
            }
        ));
    }

    Ok(json!({
        "slot": slot,
        "before": material_json(&before),
        "material": material_json(&next),
        "changed": changed,
        "changed_a_pixel": pixels,
        "changed_no_pixel": no_pixels,
        "renderer_reads": read,
        "dirty": state.dirty,
        "preview_signature": state.preview_signature(),
        "notice": notice,
    }))
}

/// Fill the registry's next free slot with a material the caller defines in full.
///
/// The two writers are a patch and a definition, and the split is the registry's own: `set` refuses an empty
/// slot because a patch cannot mean one, and `register` is the only call that occupies a slot. So every field
/// the table lists has to arrive, and a call missing one is refused rather than completed from
/// `Material::default` — a lane the caller never chose is exactly the kind of value an author would read back
/// and not recognise, and the reply prints the whole material precisely so it can be checked against the call.
///
/// The slot is not the caller's to choose: `register` hands out the registry's next free one, which is what
/// the engine itself uses for its three presets, and the reply reports where the material landed so the
/// caller can `set_transform` the entity onto it.
fn register_material(state: &mut EditorState, arguments: &Value) -> Result<Value, String> {
    let known: Vec<&str> = params::MATERIAL_FIELDS
        .iter()
        .map(|field| field.name)
        .collect();
    if let Some(map) = arguments.as_object() {
        for key in map.keys() {
            if !known.contains(&key.as_str()) {
                return Err(format!(
                    "a material has no field {key:?}; it has {}",
                    quoted(&known)
                ));
            }
        }
    }

    let missing: Vec<&str> = known
        .iter()
        .copied()
        .filter(|name| arguments.get(*name).is_none())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "a registered material is a whole definition, and this call leaves out {}. `set_material` writes \
             one field of a slot that already exists; `register_material` takes all six at once, because a \
             field left off would stand as `Material::default`'s value and not as one the caller chose.",
            quoted(&missing)
        ));
    }

    let mut material = Material::default();
    for field in &params::MATERIAL_FIELDS {
        // Every field was required above, so `None` here would mean the table lists a field the call cannot
        // carry — a disagreement a test walks the table to catch.
        let lanes = material_field_lanes(arguments, field)?.ok_or_else(|| {
            format!(
                "\"{}\" is in the material table but was not read from this call",
                field.name
            )
        })?;
        params::write_material_field(&mut material, field.name, &lanes)
            .expect("a field the table lists is a field the writer knows");
    }

    let Some(slot) = state.materials.register(material) else {
        return Err(format!(
            "the registry is full: all {} slots hold a material, so there is no free slot to register into",
            state.materials.len()
        ));
    };

    let (read, _) = params::material_names_by_shading();
    Ok(json!({
        "slot": slot,
        "material": material_json(&material),
        "material_count": state.materials.len(),
        "entity_material_id": state.entity_def.material_id,
        "renderer_reads": read,
        "dirty": state.dirty,
        "preview_signature": state.preview_signature(),
        "notice": format!(
            "slot {slot} is registered and no pixel changed: the entity still points at slot {}. \
             `set_transform` with `material_id: {slot}` is the call that paints with it. The registry is not \
             part of the entity file — `save_entity` writes `material_id` and none of these fields — so a new \
             process starts from the three presets and would find slot {slot} empty.",
            state.entity_def.material_id
        ),
    }))
}

/// The lanes a call handed over for one material field, or `None` when it left the field out.
///
/// Both material writers read a call through this, so a spelled choice and a lane outside the table's range
/// are refused with the same words whether the value is a patch or part of a whole definition.
fn material_field_lanes(
    arguments: &Value,
    field: &params::MaterialField,
) -> Result<Option<Vec<f32>>, String> {
    let Some(value) = arguments.get(field.name) else {
        return Ok(None);
    };
    let lanes = match field.choices {
        // A choice is spelled, not numbered: `noise: 2` would be a call that works today and silently means
        // something else the next time the list is reordered.
        Some(choices) => {
            let name = value.as_str().ok_or_else(|| {
                format!(
                    "\"{}\" must be {}, got {value}",
                    field.name,
                    field.limit_text()
                )
            })?;
            let index = choices
                .iter()
                .position(|kind| *kind == name)
                .ok_or_else(|| {
                    format!(
                        "no noise kind {name:?}; {}",
                        choices
                            .iter()
                            .map(|kind| format!("{kind:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
            vec![index as f32]
        }
        None => material_lanes(value, field)?,
    };
    check_material_lanes(field, &lanes)?;
    Ok(Some(lanes))
}

/// The numbers a caller handed over for one numeric material field, as exactly the lanes the table says
/// it has. A bare number is one lane; an array is read for its length rather than truncated, so a
/// two-lane colour is refused instead of becoming a colour with a zero nobody authored.
fn material_lanes(value: &Value, field: &params::MaterialField) -> Result<Vec<f32>, String> {
    let lanes: Vec<f32> = match value {
        Value::Number(one) => {
            let lane = one
                .as_f64()
                .ok_or_else(|| format!("\"{}\" must be a number", field.name))?;
            vec![lane as f32]
        }
        Value::Array(items) => items
            .iter()
            .map(|item| {
                item.as_f64().map(|lane| lane as f32).ok_or_else(|| {
                    format!("\"{}\" must be numbers only, and {item} is not", field.name)
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
        other => {
            return Err(format!(
                "\"{}\" must be {} — it is {other}",
                field.name,
                field.limit_text()
            ))
        }
    };
    if lanes.len() != field.lanes as usize {
        return Err(format!(
            "\"{}\" takes {} lane{}; this call hands {}",
            field.name,
            field.lanes,
            if field.lanes == 1 { "" } else { "s" },
            lanes.len()
        ));
    }
    Ok(lanes)
}

/// Refuse a lane the field's own contract does not admit, quoting the limit as the table states it.
fn check_material_lanes(field: &params::MaterialField, lanes: &[f32]) -> Result<(), String> {
    for (index, lane) in lanes.iter().enumerate() {
        if !lane.is_finite() {
            return Err(format!(
                "\"{}\" lanes must be finite; lane {index} is {lane}",
                field.name
            ));
        }
        if field.whole && *lane != lane.trunc() {
            return Err(format!("\"{}\" is a whole choice, got {lane}", field.name));
        }
        let below = if field.lo_open {
            *lane <= field.lo
        } else {
            *lane < field.lo
        };
        let above = field.hi.is_some_and(|hi| *lane > hi);
        if below || above {
            return Err(format!(
                "\"{}\" must be {}; lane {index} is {lane}",
                field.name,
                field.limit_text()
            ));
        }
    }
    Ok(())
}

/// The route the panel prints for a node (`root.b.kept`) turned into the `Step` path it edits.
///
/// `root` may be spelled or omitted — `a.b` and `root.a.b` reach the same node — because the parts list
/// prints the long form while `assetc`'s refusals print the field names without it, and a caller reading
/// either should be able to paste what it got. A name that is not a child *at that node* is refused with
/// the names that are, the same way an unknown view names the six it knows.
fn route_of(shape: &Shape, route: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    let mut node = shape;
    let mut walked = "root".to_string();
    for (index, segment) in route
        .split('.')
        .filter(|segment| !segment.is_empty())
        .enumerate()
    {
        if index == 0 && segment == "root" {
            continue;
        }
        let kids = shape_path::children(node);
        let Some((_, step)) = kids.iter().find(|(name, _)| *name == segment) else {
            return Err(if kids.is_empty() {
                format!(
                    "{walked} is a {} and has no children — it is a leaf; its own fields are {}",
                    shape_path::kind(node),
                    parameter_names(node)
                )
            } else {
                format!(
                    "{walked} has no child {segment:?}; it names {}",
                    kids.iter()
                        .map(|(name, _)| format!("{name:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
        };
        let Some(child) = shape_path::resolve(node, &[*step]) else {
            // `children` and `resolve` are the same grammar read two ways. If they ever disagree that is
            // a defect in this module, not a caller's typo, and it says so rather than editing a parent.
            return Err(format!(
                "{walked}.{segment} names a child the tree cannot reach"
            ));
        };
        steps.push(*step);
        walked = format!("{walked}.{segment}");
        node = child;
    }
    Ok(steps)
}

/// A route a caller has to spell out. `move_part` acts on two nodes, so neither end can be the selection
/// the way `set_part`'s and `add_part`'s optional `route` is — the question "which one is the selected
/// part?" has no answer when the call moves one part into another's place.
fn named_route(arguments: &Value, key: &str) -> Result<String, String> {
    let route = string_arg(arguments, key)?;
    if route.trim().is_empty() {
        return Err(format!(
            "{key} is required; editor_state's parts list names the routes that exist, and \"root\" is \
             the whole entity"
        ));
    }
    Ok(route)
}

/// Whether `deepest` walks through `highest` on its way down from the root, so the node at `highest` holds
/// the node at `deepest`. Equal paths are not `holds` — one node does not hold itself.
fn holds(highest: &[Step], deepest: &[Step]) -> bool {
    highest.len() < deepest.len() && deepest.starts_with(highest)
}

/// The route a node's `Step` path is printed as, or `null` if the tree no longer holds it.
fn route_name(shape: &Shape, steps: &[Step]) -> Value {
    shape_path::outline(shape)
        .into_iter()
        .find(|entry| entry.path == steps)
        .map(|entry| Value::String(entry.name))
        .unwrap_or(Value::Null)
}

/// What one node can be asked for, in the words a caller has to answer with.
fn parameter_names(node: &Shape) -> String {
    params::field_text(shape_path::kind(node))
}

fn parameters_json(node: &Shape) -> Vec<Value> {
    params::params_of(node)
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "label": spec.label,
                "lanes": spec.lanes,
                "range": [spec.lo, spec.hi],
                "whole": spec.whole,
            })
        })
        .collect()
}

/// The node a call acts on: the route it passes, or the node the editor already holds selected when it
/// passes none. Shared by `set_part` and `add_part`, so clicking a row and then acting on it is one
/// gesture rather than two dialects of it.
fn route_steps(state: &EditorState, arguments: &Value) -> Result<Vec<Step>, String> {
    match arguments.get("route") {
        Some(value) => {
            let route = value
                .as_str()
                .ok_or_else(|| "route must be a string".to_string())?;
            route_of(&state.entity_def.shape, route)
        }
        None => Ok(state.selected.clone()),
    }
}

/// Point the editor at one node of the tree, which is what a click on a row of the panel's part list does.
///
/// Selecting is not an edit: nothing here marks the entity dirty or moves the preview key, so a caller can
/// ask where the snout is without leaving the file changed.
fn select_part(state: &mut EditorState, arguments: &Value) -> Result<Value, String> {
    let route = string_arg(arguments, "route")?;
    if route.trim().is_empty() {
        return Err("route is required; editor_state's parts list names them, and \"root\" is the whole entity".to_string());
    }
    let steps = route_of(&state.entity_def.shape, &route)?;
    state.select(steps);

    let snapshot = state_json(state);
    let selected = snapshot["selected"].clone();
    let node = state
        .selected_shape()
        .expect("route_of walked to this node, so the selection resolves");
    Ok(json!({
        "selected": selected,
        "kind": shape_path::kind(node),
        "summary": shape_path::summary(node),
        "parameters": parameters_json(node),
        "state": snapshot,
    }))
}

/// Write one authored parameter of one node — the field a slider in the panel would drag.
///
/// The route is optional: with none this writes the node the editor already holds selected, so
/// `select_part` then `set_part` is the same two gestures as clicking a row and dragging its slider. The
/// range refused here is the range the slider offers, because both read `crate::params` — an edit an agent
/// can make that the window cannot is exactly the disagreement gate E7 exists to catch.
fn set_part(state: &mut EditorState, arguments: &Value) -> Result<Value, String> {
    let name = string_arg(arguments, "parameter")?;
    let steps = route_steps(state, arguments)?;

    let node = shape_path::resolve(&state.entity_def.shape, &steps)
        .ok_or_else(|| "the selected node is no longer in the tree; pass a route".to_string())?;
    let spec = *params::params_of(node)
        .iter()
        .find(|spec| spec.name == name)
        .ok_or_else(|| {
            format!(
                "{} carries no parameter {name:?}; it takes {}",
                shape_path::kind(node),
                parameter_names(node)
            )
        })?;

    let given = arguments
        .get("value")
        .ok_or_else(|| "value is required".to_string())?;
    let lanes = lanes_of(given, &spec, "value")?;
    check_lanes(&spec, &lanes)?;

    let from = params::read_param(node, name.as_str()).expect("the spec came from this node");
    let kind_name = shape_path::kind(node);
    let moved = from[..spec.lanes as usize] != lanes[..spec.lanes as usize];
    if moved {
        let slot = shape_path::resolve_mut(&mut state.entity_def.shape, &steps)
            .expect("resolve() reached this node, so resolve_mut() does too");
        // A field the table publishes but the writer cannot set is a defect in `params`, and it refuses
        // here rather than taking the transport down with a panic.
        params::write_param(slot, name.as_str(), &lanes[..spec.lanes as usize]).ok_or_else(
            || format!("{name} is listed for a {kind_name} but this build cannot write it"),
        )?;
        state.mark_dirty();
    }

    let route = route_name(&state.entity_def.shape, &steps);
    let node =
        shape_path::resolve(&state.entity_def.shape, &steps).expect("the write kept the node");
    Ok(json!({
        "route": route,
        "parameter": name,
        "from": from[..spec.lanes as usize],
        "to": lanes[..spec.lanes as usize],
        "changed": moved,
        "summary": shape_path::summary(node),
        "preview_signature": state.preview_signature(),
        "state": state_json(state),
    }))
}

/// The combinators `add_part` can build by joining two nodes: the one the route names, and a leaf authored
/// on the call.
const COMBINATIONS: &[&str] = &["Union", "Subtract", "Intersect", "SmoothUnion"];

/// The wrappers `add_part` can build around one node. These do not join anything — the node the route
/// names becomes their single child — so they take no `shape` and no `keep`, and their own field (`axis`,
/// `period`) sits beside `combine` exactly as a `SmoothUnion`'s `k` does. A test walks this list against
/// the field each wrapper is built with, so a name here that the code cannot build is caught.
pub const WRAPPERS: &[&str] = &["Mirror", "Repeat"];

/// A list of names in the shape a refusal answers with: `"a", "b"`.
fn quoted(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The lanes a value carries for a field of this width: a number for a scalar, an array of exactly three
/// for a vector.
fn lanes_of(value: &Value, spec: &params::Param, key: &str) -> Result<[f32; 3], String> {
    if spec.lanes == 1 {
        return Ok([scalar(value, key)?, 0.0, 0.0]);
    }
    Ok([
        element(value, 0, key)?,
        element(value, 1, key)?,
        element(value, 2, key)?,
    ])
}

/// One written value, against the range the panel's slider offers. `set_part` and `add_part` read the same
/// check, so authoring a node from nothing cannot slip past a limit that editing one would refuse.
fn check_lanes(spec: &params::Param, lanes: &[f32; 3]) -> Result<(), String> {
    let name = spec.name;
    for lane in lanes.iter().take(spec.lanes as usize) {
        if !lane.is_finite() {
            return Err(format!("{name} must be finite, got {lanes:?}"));
        }
        if spec.whole && *lane != lane.trunc() {
            return Err(format!("{name} is a whole choice, got {lane}"));
        }
        if !(spec.lo..=spec.hi).contains(lane) {
            return Err(format!(
                "{name} must be inside {}, got {lane}",
                spec.range_text()
            ));
        }
    }
    Ok(())
}

/// The node `add_part` authors from nothing.
///
/// Every field the kind publishes has to be supplied: a defaulted radius would be a value the caller never
/// asked for, and the file the editor writes would read as though it had. A key that is neither `kind` nor
/// one of those fields is refused for the same reason — a typo that is quietly dropped authors a node
/// nobody described.
fn authored_leaf(given: &Value) -> Result<Shape, String> {
    let kind = string_arg(given, "kind")?;
    let mut node = params::blank(&kind).ok_or_else(|| {
        format!(
            "{kind:?} cannot be authored from nothing; the kinds add_part creates are {}, and a \
             combinator needs operands before it exists",
            quoted(params::AUTHORABLE)
        )
    })?;
    let table = params::params_of(&node);
    if let Some(fields) = given.as_object() {
        for key in fields.keys() {
            if key != "kind" && !table.iter().any(|spec| spec.name == key) {
                return Err(format!(
                    "{kind} takes no field {key:?}; it takes {}",
                    params::field_text(&kind)
                ));
            }
        }
    }
    for spec in table {
        let value = given.get(spec.name).ok_or_else(|| {
            format!(
                "{kind} takes {}; {:?} is missing",
                params::field_text(&kind),
                spec.name
            )
        })?;
        let lanes = lanes_of(value, spec, spec.name)?;
        check_lanes(spec, &lanes)?;
        // A field the table publishes but the writer cannot set is a defect in `params`, and it says so
        // here rather than letting a zero from `blank` into the tree.
        params::write_param(&mut node, spec.name, &lanes[..spec.lanes as usize]).ok_or_else(
            || {
                format!(
                    "{} is listed for a {kind} but this build cannot write it",
                    spec.name
                )
            },
        )?;
    }
    Ok(node)
}

/// A wrapper of the one node already at the route, with its own fields still at the build's defaults.
///
/// Like [`authored_leaf`], no default survives the call: every field the parameter table publishes for
/// this kind has to be supplied, and the loop in [`wrap_part`] writes each one into the node before it
/// joins the tree. `None` for a name [`WRAPPERS`] does not list.
fn blank_wrapper(kind: &str, child: Shape) -> Option<Shape> {
    match kind {
        "Mirror" => Some(Shape::Mirror {
            axis: 0,
            child: Box::new(child),
        }),
        "Repeat" => Some(Shape::Repeat {
            period: 1.0,
            child: Box::new(child),
        }),
        _ => None,
    }
}

/// Which slot of the new combinator keeps the node already in the tree. The names are the authored file's
/// — `a`/`b`, and `kept`/`cutter` for a `Subtract`, where the order is the difference between a hole and
/// an animal.
fn keep_slot(arguments: &Value, combine: &str) -> Result<(Step, &'static str), String> {
    let names = shape_path::child_names(combine);
    let (first_name, first_step) = *names.first().ok_or_else(|| {
        format!("{combine} has no slot to keep, so it is not a combinator add_part can build")
    })?;
    match arguments.get("keep") {
        None => Ok((first_step, first_name)),
        Some(value) => {
            let want = value.as_str().ok_or_else(|| {
                "keep must be a string naming the slot the existing node keeps".to_string()
            })?;
            names
                .iter()
                .find(|(name, _)| *name == want)
                .map(|(name, step)| (*step, *name))
                .ok_or_else(|| {
                    format!(
                        "{combine} has no slot {want:?}; it names {}",
                        quoted(&names.iter().map(|(name, _)| *name).collect::<Vec<_>>())
                    )
                })
        }
    }
}

/// A `SmoothUnion`'s blend width: required of the one combinator that has a parameter of its own, and
/// refused for the three that do not, so a `k` on a `Union` cannot sit there doing nothing.
fn blend_k(arguments: &Value, combine: &str) -> Result<f32, String> {
    let spec = *params::params_of_kind("SmoothUnion")
        .first()
        .expect("a SmoothUnion publishes k");
    match arguments.get("k") {
        Some(value) => {
            if combine != "SmoothUnion" {
                return Err(format!("{combine} carries no k; only a SmoothUnion blends"));
            }
            let lanes = lanes_of(value, &spec, "k")?;
            check_lanes(&spec, &lanes)?;
            Ok(lanes[0])
        }
        None if combine == "SmoothUnion" => Err(format!(
            "SmoothUnion needs k, the width it blends with; it takes {:?}",
            spec.range_text()
        )),
        None => Ok(0.0),
    }
}

/// The combinator `combine` names, holding `first` and `second` in that order.
///
/// Both structural calls reach this after deciding which node keeps which slot, so a slot's meaning is
/// written once: a `Subtract`'s `kept`/`cutter` is the difference between a hole and an animal, and a second
/// copy of this match is where that pair could get reversed.
fn joined(combine: &str, first: Shape, second: Shape, k: f32) -> Shape {
    match combine {
        "Union" => Shape::Union {
            a: Box::new(first),
            b: Box::new(second),
        },
        "Intersect" => Shape::Intersect {
            a: Box::new(first),
            b: Box::new(second),
        },
        "Subtract" => Shape::Subtract {
            kept: Box::new(first),
            cutter: Box::new(second),
        },
        _ => Shape::SmoothUnion {
            a: Box::new(first),
            b: Box::new(second),
            k,
        },
    }
}

/// The three axes, named the way an authored `centre` reads.
const AXES: [&str; 3] = ["x", "y", "z"];

/// One box, as an author reads it in a sentence: `x -0.5..=0.5 y 0..=0.2 z -1..=1`.
fn span_text(low: [f32; 3], high: [f32; 3]) -> String {
    AXES.iter()
        .zip(low.iter().zip(high.iter()))
        .map(|(axis, (lo, hi))| format!("{axis} {lo}..={hi}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the part a wrapper holds actually contributes to the drawn field.
///
/// Returns the part's authored box (`Value::Null` when no finite box states it) beside a sentence that is
/// present only when the wrapper **discards** part of the wrap. A wrapper is an isometry of the *query
/// point*, not of the child: a `Repeat` consults its child only at the point folded into
/// `[-period/2, period/2)` on every axis, and a `Mirror` only at `|p[axis]|`. So what survives a wrap is
/// the child's intersection with the wrapper's own domain — tiled or reflected — and not the child
/// wherever it happens to sit. When that intersection is empty the call has compiled, lowered, selected and
/// marked dirty exactly as if it had drawn something: only the frame knows. This is the half the frame
/// knows, said in the reply instead.
///
/// The box comes from `assetc::order::extent`, the same function the compiler's union sort keys are built
/// from, so the notice and the layout cannot drift into describing two different shapes. It overstates
/// where it must (`Subtract` reports its kept operand, a `Mirror` its child plus the reflection), which is
/// the safe direction: an overstated box warns about a part that in fact draws, and never blesses one that
/// would be dropped.
fn wrap_notice(wrapper: &Shape, held: &Shape) -> (Value, Option<String>) {
    let (low, high) = match assetc::order::extent(held) {
        // A `Plane` or a nested tiling has no finite box, so nothing here can be claimed about the cell.
        None => {
            return (
                Value::Null,
                Some(
                    "the part has no finite extent - a Plane, or a tiling inside it - so the pipe cannot \
                     say whether the wrapper's own domain reaches it. Look at the frame this draws before \
                     relying on it."
                        .to_string(),
                ),
            );
        }
        Some(box_at) => box_at,
    };
    let authored = span_text(low, high);
    let json = json!([[low[0], low[1], low[2]], [high[0], high[1], high[2]]]);
    let notice = match wrapper {
        Shape::Repeat { period, .. } => {
            let half = period * 0.5;
            // The cell is a product of three equal intervals, so one axis missing it is the whole part
            // missing it.
            let outside = (0..3).any(|axis| high[axis] < -half || low[axis] > half);
            let clipped = (0..3).any(|axis| low[axis] < -half || high[axis] > half);
            if outside {
                Some(format!(
                    "a Repeat consults its child only inside the cell {} about the origin, and the part it \
                     wraps is {authored}, none of which lies in it: this wrap compiles and draws nothing. \
                     Widen `period` until the cell reaches the part, or move the part in - the grammar has \
                     no translate node, so a sphere moves by its own `centre`.",
                    span_text([-half, -half, -half], [half, half, half])
                ))
            } else if clipped {
                Some(format!(
                    "a Repeat consults its child only inside the cell {} about the origin, and the part it \
                     wraps is {authored}, which reaches outside it: only the piece inside the cell is \
                     tiled, so the wrap silently drops the rest. Widen `period` or move the part in.",
                    span_text([-half, -half, -half], [half, half, half])
                ))
            } else {
                None
            }
        }
        Shape::Mirror { axis, .. } => {
            let axis = (*axis as usize).min(2);
            let plane = AXES[axis];
            if high[axis] < 0.0 {
                Some(format!(
                    "a Mirror consults its child only at {plane} >= 0 - it evaluates the field at \
                     |{plane}| - and the part it wraps is {authored}, wholly on the negative side: this \
                     wrap compiles and draws nothing. Move the part to {plane} >= 0, and the Mirror gives \
                     you the other side for free."
                ))
            } else if low[axis] < 0.0 {
                Some(format!(
                    "a Mirror consults its child only at {plane} >= 0, and the part it wraps is {authored}, \
                     which crosses the plane: only its {plane} >= 0 half is evaluated, and the reflection \
                     takes the place of the authored negative half rather than adding to it.",
                ))
            } else {
                None
            }
        }
        // Every other combinator is a plain set operation on the whole child: nothing it holds can be
        // discarded by where it sits.
        _ => None,
    };
    (json, notice)
}

/// Put a rebuilt tree into the state, answering the whole call with the compiler's own refusal if the
/// tree it would leave cannot compile.
///
/// The structural tools build their result on a clone and hand it here, so a refusal cannot leave the
/// editor holding half a change. `assetc::lower` is the pass the shipped `.bin` comes out of — the depth
/// cap, a non-unit axis, a NaN length — and an editor that wrote a tree the compiler refuses would hand
/// an author a file that cannot become an asset and say nothing until the boot fails.
fn commit(state: &mut EditorState, candidate: Shape, select: Vec<Step>) -> Result<usize, String> {
    if let Err(refusal) = assetc::lower(&candidate) {
        return Err(format!(
            "the tree this would leave cannot be compiled: {refusal}"
        ));
    }
    let before = shape_path::outline(&state.entity_def.shape).len();
    state.entity_def.shape = candidate;
    state.select(select);
    state.mark_dirty();
    Ok(before)
}

/// Grow the tree: wrap the node a route names in a new combinator, joining it to a freshly authored leaf.
///
/// There is no free slot in this grammar — a combinator is full the moment it exists — so *adding* means
/// wrapping, and that is also how a node reaches any position the routes can name, `root` included.
fn add_part(state: &mut EditorState, arguments: &Value) -> Result<Value, String> {
    let steps = route_steps(state, arguments)?;
    let combine = string_arg(arguments, "combine")?;
    if WRAPPERS.contains(&combine.as_str()) {
        return wrap_part(state, arguments, &combine, steps);
    }
    if !COMBINATIONS.contains(&combine.as_str()) {
        return Err(format!(
            "no combinator {combine:?}; add_part joins with {} and wraps with {}",
            quoted(COMBINATIONS),
            quoted(WRAPPERS)
        ));
    }
    let given = arguments.get("shape").ok_or_else(|| {
        format!(
            "{combine} needs the node to join it to: pass shape as {{\"kind\": \"Sphere\", ...}} — \
             the kinds it can author are {}",
            quoted(params::AUTHORABLE)
        )
    })?;
    let leaf = authored_leaf(given)?;
    let (keep, keep_name) = keep_slot(arguments, &combine)?;
    let k = blend_k(arguments, &combine)?;

    let mut candidate = state.entity_def.shape.clone();
    let slot = shape_path::resolve_mut(&mut candidate, &steps)
        .ok_or_else(|| "the selected node is no longer in the tree; pass a route".to_string())?;
    let existing = core::mem::replace(slot, leaf.clone());
    let (first, second) = if keep == Step::First {
        (existing, leaf)
    } else {
        (leaf, existing)
    };
    *slot = joined(&combine, first, second, k);

    let before = commit(state, candidate, steps.clone())?;
    let snapshot = state_json(state);
    let tree = &state.entity_def.shape;
    let at = route_name(tree, &steps);
    // The existing node stayed in the slot `keep` names and the new leaf took the other one, so both
    // reports read back out of the tree the call just left instead of being described from the arguments.
    let slots = shape_path::child_names(combine.as_str());
    let (other, other_name) = if keep == Step::First {
        (slots[1].1, slots[1].0)
    } else {
        (slots[0].1, slots[0].0)
    };
    let leaf_path = [steps.as_slice(), &[other]].concat();
    let kept_path = [steps.as_slice(), &[keep]].concat();
    let added =
        shape_path::resolve(tree, &leaf_path).expect("commit put the wrapper at this route");
    let kept = shape_path::resolve(tree, &kept_path)
        .expect("commit kept the node the route named in the slot it named");
    Ok(json!({
        "at": at,
        "into": combine,
        "kept": {
            "slot": keep_name,
            "route": route_name(tree, &kept_path),
            "kind": shape_path::kind(kept),
            "summary": shape_path::summary(kept),
        },
        "added": {
            "slot": other_name,
            "route": route_name(tree, &leaf_path),
            "kind": shape_path::kind(added),
            "summary": shape_path::summary(added),
        },
        "selected": at,
        "nodes_before": before,
        "node_count": snapshot["node_count"],
        "preview_signature": state.preview_signature(),
        "state": snapshot,
    }))
}

/// Grow the tree the one-child way: put the node a route names inside a `Mirror` or a `Repeat`.
///
/// The gesture is different enough from [`add_part`]'s to deserve its own function: there is no second
/// operand to author and no slot to choose, so a call that passes either is refused rather than ignored,
/// and the wrapper's own field is read from the same parameter table `set_part` writes — which is what
/// keeps `axis` a whole choice in 0..=2 here and on the panel.
///
/// A `Repeat` folds the *query point* into the cell `[-period/2, period/2)` on every axis before its
/// child is evaluated (`render::wrap`, transcribed identically in `interp.wgsl`), so the tiles are copies
/// of whatever the child has inside that cell — not copies of the child wherever it sits. A part authored
/// wholly outside the cell therefore renders as nothing, and [`wrap_notice`] is what says so in this
/// reply rather than only in the frame. This is the compiler's meaning, not this call's: `add_part` wraps,
/// and the wrap is visible in the field.
fn wrap_part(
    state: &mut EditorState,
    arguments: &Value,
    combine: &str,
    steps: Vec<Step>,
) -> Result<Value, String> {
    if arguments.get("shape").is_some() {
        return Err(format!(
            "{combine} wraps the node the route names instead of joining it to one, so it takes no \
             shape; to add a part beside it pass combine as {}",
            quoted(COMBINATIONS)
        ));
    }
    if arguments.get("keep").is_some() {
        return Err(format!(
            "{combine} holds one node — the one already at the route — so there is no slot to keep; \
             keep chooses which of two operands survives and only {} have two",
            quoted(COMBINATIONS)
        ));
    }
    // A `k` on a wrapper is the same mistake it is on a `Union`: a parameter that would sit in the file
    // doing nothing. `blend_k` refuses it in those words.
    blend_k(arguments, combine)?;

    let mut candidate = state.entity_def.shape.clone();
    let existing = shape_path::resolve(&candidate, &steps)
        .ok_or_else(|| "the selected node is no longer in the tree; pass a route".to_string())?;
    let mut wrapper =
        blank_wrapper(combine, existing.clone()).expect("WRAPPERS lists only what this builds");
    for spec in params::params_of(&wrapper) {
        let value = arguments.get(spec.name).ok_or_else(|| {
            format!(
                "{combine} takes {}; {:?} is missing",
                params::field_text(combine),
                spec.name
            )
        })?;
        let lanes = lanes_of(value, spec, spec.name)?;
        check_lanes(spec, &lanes)?;
        params::write_param(&mut wrapper, spec.name, &lanes[..spec.lanes as usize]).ok_or_else(
            || {
                format!(
                    "{} is listed for a {combine} but this build cannot write it",
                    spec.name
                )
            },
        )?;
    }
    *shape_path::resolve_mut(&mut candidate, &steps)
        .expect("resolve read the node at this route a moment ago") = wrapper;

    let before = commit(state, candidate, steps.clone())?;
    let snapshot = state_json(state);
    let tree = &state.entity_def.shape;
    let at = route_name(tree, &steps);
    let held_path = [steps.as_slice(), &[Step::Sole]].concat();
    // Both halves of the reply read back out of the tree the call just left: the wrapper at the route the
    // caller named, and the node it now holds one level deeper.
    let now = shape_path::resolve(tree, &steps).expect("commit put the wrapper at this route");
    let held = shape_path::resolve(tree, &held_path)
        .expect("a wrapper holds one node at its own child step");
    let (extent, notice) = wrap_notice(now, held);
    Ok(json!({
        "at": at,
        "into": combine,
        "wrapper": {
            "route": at,
            "kind": shape_path::kind(now),
            "summary": shape_path::summary(now),
        },
        "wrapped": {
            "slot": "child",
            "route": route_name(tree, &held_path),
            "kind": shape_path::kind(held),
            "summary": shape_path::summary(held),
            "extent": extent,
            "notice": notice,
        },
        "selected": at,
        "nodes_before": before,
        "node_count": snapshot["node_count"],
        "preview_signature": state.preview_signature(),
        "state": snapshot,
    }))
}

/// The route a node is printed at, as text. Every caller here walked to the node first, so the `null`
/// `route_name` answers for a path the tree does not hold cannot occur.
fn route_text(shape: &Shape, steps: &[Step]) -> String {
    route_name(shape, steps)
        .as_str()
        .map(str::to_string)
        .expect("a node the tree holds has a route")
}

/// One node lifted out of the tree, and the tree it left behind.
struct Lifted {
    /// The node that came out, exactly as it stood, subtree and all.
    removed: Shape,
    /// The tree without it: the holder is gone and the node that shared the holder — or the one a wrapper
    /// held — stands in the holder's place.
    remaining: Shape,
    /// The route the survivor took, which is where the holder stood.
    slot: Vec<Step>,
    /// The step, *before* the lift, at which the survivor sat under `slot`. A route that ran through it
    /// loses those two steps once the survivor is promoted; see [`after_collapse`].
    promoted: Step,
    /// The selection that still means something afterwards: the survivor's route, or the root when a
    /// combinator below it was flattened and every path below shifted.
    select: Vec<Step>,
}

/// Why the grammar cannot lift a node out of the tree at all.
enum Unliftable {
    /// The root is not a one-child wrapper: one operand of a union is not a shape, and a root leaf would
    /// leave the editor holding nothing.
    Root,
    /// The node is a one-child wrapper's only child, so lifting it would leave the wrapper holding nothing.
    OnlyChild {
        node: String,
        wrapper: &'static str,
        wrapper_route: String,
    },
}

/// How a lift treats a node that holds exactly one child.
enum Lift {
    /// Take the wrapper out and leave its child in its place. This is `delete_part`'s gesture: a one-child
    /// wrapper is removed by unwrapping it, at any depth.
    Unwrap,
    /// Take the wrapper out *with* its child, and collapse the combinator that held the wrapper. This is
    /// `graft_part`'s: a lifted subtree is the node and everything under it, so nothing may be left behind —
    /// unwrapping first would promote the child and then graft a second copy of it.
    Whole,
}

/// Take one node out of the tree, and the combinator that held it with it, answering the tree that is left
/// and the selection that still means something in it.
///
/// A two-operand combinator missing an operand is not a shape, so the surviving operand takes its parent's
/// place and the tree loses two nodes while keeping everything below the survivor. A one-child wrapper has
/// nothing to promote, so lifting its child is refused rather than answering with a `Mirror` of nothing.
/// `lift` decides what happens to a node that is itself a one-child wrapper: [`Lift::Unwrap`] takes it out
/// and leaves its child standing (a deletion), [`Lift::Whole`] takes the child with it (a move). The root is
/// its own holder, so it goes only under [`Lift::Unwrap`], where what it holds can stand alone: unwrapping a
/// root `Mirror` or `Repeat` makes its child the tree.
///
/// Both callers get the same structural answer: `delete_part` drops the removed node and `graft_part`
/// re-attaches it somewhere else. The wording belongs to the caller — "deleting it" and "lifting it out"
/// are different acts — so this answers *why* it refused rather than a sentence.
fn collapse_out(tree: &Shape, steps: &[Step], lift: Lift) -> Result<Lifted, Unliftable> {
    let removed = shape_path::resolve(tree, steps)
        .expect("the caller walked to this node")
        .clone();
    // Where the survivor goes, the step it came from, and where the selection lands.
    let (slot, promoted, select) =
        if matches!(lift, Lift::Unwrap) && shape_path::children(&removed).len() == 1 {
            // A one-child wrapper is lifted by *unwrapping* it: the node it held takes the wrapper's place, at
            // any depth. This is the exact inverse of `add_part`'s wrapper gesture, and it is not root-only
            // because a `Repeat` wrapped inside a `Mirror` was otherwise only ever removable together with the
            // `Mirror` over it. The route the wrapper held still names a node afterwards — the promoted child —
            // so the selection stays on it rather than retreating to `root`.
            (steps.to_vec(), Step::Sole, steps.to_vec())
        } else {
            match steps.split_last() {
                // A node below the root goes with the combinator that held it, and that combinator's other
                // operand takes its place.
                Some((&last, parent_path)) => {
                    let parent = shape_path::resolve(tree, parent_path)
                        .expect("route_of walked through here");
                    if shape_path::children(parent).len() != 2 {
                        return Err(Unliftable::OnlyChild {
                            node: route_text(tree, steps),
                            wrapper: shape_path::kind(parent),
                            wrapper_route: route_text(tree, parent_path),
                        });
                    }
                    let promoted = if last == Step::First {
                        Step::Second
                    } else {
                        Step::First
                    };
                    (parent_path.to_vec(), promoted, Vec::new())
                }
                // A root that is not a one-child wrapper cannot be lifted: one operand of a `Union` is not a
                // shape, and a root leaf would leave the editor holding nothing at all.
                None => return Err(Unliftable::Root),
            }
        };
    let survivor_path = [slot.as_slice(), &[promoted]].concat();
    let survivor = shape_path::resolve(tree, &survivor_path)
        .expect("the slot named here holds the node this lift promotes")
        .clone();
    let mut remaining = tree.clone();
    *shape_path::resolve_mut(&mut remaining, &slot)
        .expect("the survivor's slot is on the lifted node's own line") = survivor;
    Ok(Lifted {
        removed,
        remaining,
        slot,
        promoted,
        select,
    })
}

/// Where a route that ran through the lifted node's holder points once the survivor has taken its place.
///
/// The collapse removes two steps from that line — the holder's, and the step of the operand that was lifted
/// — so a route through the surviving operand keeps its tail and loses both, and a route that named the
/// survivor itself lands on the holder's route. `None` for a route that did not run through it, which the
/// caller leaves alone.
fn after_collapse(steps: &[Step], slot: &[Step], promoted: Step) -> Option<Vec<Step>> {
    let rest = steps.strip_prefix(slot)?;
    let (first, tail) = rest.split_first()?;
    (*first == promoted).then(|| [slot, tail].concat())
}

/// Take one node out of the tree, and the combinator that held it with it.
///
/// Deleting a `Subtract`'s `kept` leaves its `cutter` standing — that is what the caller asked for, and the
/// reply names the node that took the place. The structure itself is [`collapse_out`]'s; this is that answer
/// with the removed node dropped and said in the words of a deletion.
fn delete_part(state: &mut EditorState, arguments: &Value) -> Result<Value, String> {
    let route = string_arg(arguments, "route")?;
    if route.trim().is_empty() {
        return Err(
            "route is required; editor_state's parts list names the parts that can be deleted, and \
             \"root\" is the whole entity"
                .to_string(),
        );
    }
    let steps = route_of(&state.entity_def.shape, &route)?;
    let tree = &state.entity_def.shape;
    let gone = shape_path::resolve(tree, &steps).expect("route_of walked to this node");
    let (gone_kind, gone_summary, gone_route) = (
        shape_path::kind(gone),
        shape_path::summary(gone),
        route_name(tree, &steps),
    );
    let lifted = collapse_out(tree, &steps, Lift::Unwrap).map_err(|why| match why {
        Unliftable::Root => "\"root\" is the whole entity — deleting it leaves an editor holding nothing; \
             delete its parts until the tree is the one you want"
            .to_string(),
        Unliftable::OnlyChild {
            node,
            wrapper,
            wrapper_route,
        } => format!(
            "{node} is {wrapper}'s only child; deleting it would leave a {wrapper} holding nothing. Delete \
             the {wrapper} at \"{wrapper_route}\" instead — that takes the wrapper out and leaves this node \
             where it is — or set this node's parameter."
        ),
    })?;
    // The paths below a collapsed combinator have all shifted, so nothing the caller had selected can be
    // trusted to still mean what it meant and the selection goes back to the root; an unwrap leaves every
    // route outside the wrapper alone, so it lands on the promoted node.
    let before = commit(state, lifted.remaining, lifted.select)?;
    let snapshot = state_json(state);
    let now = shape_path::resolve(&state.entity_def.shape, &lifted.slot)
        .expect("the survivor took the slot this deletion named");
    Ok(json!({
        "deleted": { "route": gone_route, "kind": gone_kind, "summary": gone_summary },
        "its_parent": {
            "route": route_name(&state.entity_def.shape, &lifted.slot),
            "replaced_by": shape_path::kind(now),
            "summary": shape_path::summary(now),
        },
        "selected": snapshot["selected"],
        "nodes_before": before,
        "node_count": snapshot["node_count"],
        "preview_signature": state.preview_signature(),
        "state": snapshot,
    }))
}

/// Re-parent one node: lift the subtree a route names out of the tree, and graft it onto another node.
///
/// This is the gesture the full combinators otherwise cannot make. A grammar whose combinators are full the
/// moment they exist can exchange two operands ([`move_part`]) and grow ([`add_part`]), but nothing before
/// this call could put a finished subtree under a different parent: a part is authored twice, or authored
/// once and wrapped. `graft_part` lifts `from` exactly as [`delete_part`] would — the same collapse of the
/// combinator that held it — and joins the lifted subtree to `under` as one operand of a new `combine`, in
/// the slot `keep` does not name. The tree stays whole: the lift takes the holder and the subtree out, and the
/// join puts the subtree and one combinator back, so the node count is the same before and after.
///
/// The node `under` has to survive the lift, which is what the refusals are about. `from` may not hold
/// `under`, because lifting it would take the landing node with it. `under` may not be the combinator that
/// holds `from` — nor `root` when `from` is one of its operands — because the collapse replaces that very
/// node with its surviving operand, so the node the graft was asked to land on would not exist by the time
/// anything was grafted onto it. An ancestor of `from` that is not its holder does survive, and is the
/// useful shape of a lift-up: the part comes out of its branch and is joined beside the node that contained
/// it. A route through the operand the collapse promoted moves with it ([`after_collapse`]), so a target
/// inside that operand is followed rather than refused for a route the caller could not have known.
fn graft_part(state: &mut EditorState, arguments: &Value) -> Result<Value, String> {
    const KNOWN: [&str; 5] = ["from", "under", "combine", "keep", "k"];
    if let Some(map) = arguments.as_object() {
        for key in map.keys() {
            if !KNOWN.contains(&key.as_str()) {
                return Err(format!(
                    "graft_part has no argument {key:?}; it takes {}",
                    quoted(&KNOWN)
                ));
            }
        }
    }
    let from = named_route(arguments, "from")?;
    let under = named_route(arguments, "under")?;
    let combine = string_arg(arguments, "combine")?;
    if WRAPPERS.contains(&combine.as_str()) {
        return Err(format!(
            "{combine} wraps the one node it holds and joins nothing, so it cannot graft one node onto \
             another; graft_part joins with {}. To wrap a route, call add_part.",
            quoted(COMBINATIONS)
        ));
    }
    if !COMBINATIONS.contains(&combine.as_str()) {
        return Err(format!(
            "no combinator {combine:?}; graft_part joins with {}",
            quoted(COMBINATIONS)
        ));
    }
    let (keep, keep_name) = keep_slot(arguments, &combine)?;
    let k = blend_k(arguments, &combine)?;

    let tree = &state.entity_def.shape;
    let from_steps = route_of(tree, &from)?;
    let under_steps = route_of(tree, &under)?;

    if from_steps == under_steps {
        return Err(format!(
            "{from} and {under} are the same node — a graft lifts one node out and joins it to a \
             different one. To change this part's own field use set_part."
        ));
    }
    if from_steps.is_empty() {
        return Err(
            "\"root\" is the whole entity — lifting it out would leave nothing to graft it under; graft \
             one of its parts instead"
                .to_string(),
        );
    }
    if holds(&from_steps, &under_steps) {
        return Err(format!(
            "{from} holds {under}, so lifting it out would take {under} with it and leave nothing to graft \
             it under. Graft {from} under a node outside it."
        ));
    }
    if from_steps
        .split_last()
        .is_some_and(|(_, parent)| parent == under_steps.as_slice())
    {
        return Err(format!(
            "{from} sits directly under {under}, so lifting it out would replace {under} with its other \
             operand — the node the graft was asked to land on would not exist by then. Graft {from} under \
             a node that does not hold it."
        ));
    }

    let anchor_before = shape_path::resolve(tree, &under_steps)
        .expect("route_of walked to this node")
        .clone();
    let from_route = route_text(tree, &from_steps);
    let lifted = collapse_out(tree, &from_steps, Lift::Whole).map_err(|why| match why {
        Unliftable::Root => "\"root\" is the whole entity — lifting it out would leave nothing to graft \
             it under; graft one of its parts instead"
            .to_string(),
        Unliftable::OnlyChild {
            node,
            wrapper,
            wrapper_route,
        } => format!(
            "{node} is {wrapper}'s only child; lifting it out would leave a {wrapper} holding nothing. \
             Graft the {wrapper} at \"{wrapper_route}\" instead — that moves the wrapper and this node \
             together."
        ),
    })?;

    let under_now = after_collapse(&under_steps, &lifted.slot, lifted.promoted)
        .unwrap_or_else(|| under_steps.clone());
    // The anchor is read from the tree the lift left behind, not from the tree before it. When `under`
    // holds `from` the lift happened inside the anchor, so the earlier value still carries the subtree that
    // was just lifted out — joining that would put the lifted part back twice and lose the survivor.
    let anchor = shape_path::resolve(&lifted.remaining, &under_now);
    // The anchor must still be there. Its *contents* change when the lift happened inside it — that is the
    // lift-up this call allows — so the value is only compared when `under` does not hold `from`, where the
    // lift touched nothing on the anchor's line and the comparison is the guard on a route that moved.
    if anchor.is_none() || (!holds(&under_steps, &from_steps) && anchor != Some(&anchor_before)) {
        return Err(format!(
            "lifting {from} out moved the node {under} names in a way this call could not follow, so \
             nothing was grafted and the tree is as it was"
        ));
    }
    let anchor = anchor
        .expect("the check above refused a missing anchor")
        .clone();

    let (first, second) = if keep == Step::First {
        (anchor, lifted.removed)
    } else {
        (lifted.removed, anchor)
    };
    let mut candidate = lifted.remaining;
    *shape_path::resolve_mut(&mut candidate, &under_now)
        .expect("resolve read the node at this route a moment ago") =
        joined(&combine, first, second, k);

    let slots = shape_path::child_names(&combine);
    let (other, other_name) = if keep == Step::First {
        (slots[1].1, slots[1].0)
    } else {
        (slots[0].1, slots[0].0)
    };
    // The selection follows the node the caller lifted, which now sits in the slot `keep` did not name.
    let select = [under_now.as_slice(), &[other]].concat();
    let before = commit(state, candidate, select)?;

    let snapshot = state_json(state);
    let tree = &state.entity_def.shape;
    let at = route_name(tree, &under_now);
    let anchor_path = [under_now.as_slice(), &[keep]].concat();
    let grafted_path = [under_now.as_slice(), &[other]].concat();
    // Both halves read back out of the tree the call just left: a graft that wrote one operand and lost the
    // other would report the pair it was asked for from its arguments instead.
    let anchor_now = shape_path::resolve(tree, &anchor_path)
        .expect("commit put the new combinator at the route the anchor kept");
    let grafted_now = shape_path::resolve(tree, &grafted_path)
        .expect("commit put the lifted subtree in the slot it did not keep");
    Ok(json!({
        "at": at,
        "into": combine,
        "anchor": {
            "slot": keep_name,
            "called_with": under,
            "route": route_name(tree, &anchor_path),
            "kind": shape_path::kind(anchor_now),
            "summary": shape_path::summary(anchor_now),
            "children": shape_path::children(anchor_now).len(),
        },
        "grafted": {
            "slot": other_name,
            "was": from_route,
            "route": route_name(tree, &grafted_path),
            "kind": shape_path::kind(grafted_now),
            "summary": shape_path::summary(grafted_now),
            "children": shape_path::children(grafted_now).len(),
        },
        "selected": snapshot["selected"],
        "nodes_before": before,
        "node_count": snapshot["node_count"],
        "preview_signature": state.preview_signature(),
        "state": snapshot,
    }))
}

/// Move one node to another node's place, and that node into its: an exchange of two routes' contents.
///
/// There is no free slot in this grammar — a combinator is full the moment it exists — so a move that
/// *inserted* would have to answer what happens to the node it displaces, and the honest answers are only
/// "it is dropped" (a tool that silently deletes geometry) and "it goes where the moved node came from",
/// which is this call. Exchanging is total: both ends keep holding exactly one node, the tree keeps its
/// node count, and every route outside the two named still resolves to what it resolved to before.
///
/// The two ends may not sit on one line from the root. Both writes are applied to the same tree, and on an
/// ancestor pair the deeper write lands inside the very node the shallower one replaces, so one of the two
/// subtrees would be overwritten rather than moved and the count would come back different — a loss the
/// caller cannot see in the reply. `root` is an ancestor of every route, so it is never one end of an
/// exchange: the grammar has no hole to put the root's contents in. Two unrelated routes can still deepen
/// the tree, because a subtree that sat one step below the root can move several steps down, and that
/// refusal is the compiler's — [`commit`] runs `assetc::lower` before anything lands.
fn move_part(state: &mut EditorState, arguments: &Value) -> Result<Value, String> {
    let from = named_route(arguments, "from")?;
    let to = named_route(arguments, "to")?;
    let tree = &state.entity_def.shape;
    let from_steps = route_of(tree, &from)?;
    let to_steps = route_of(tree, &to)?;

    if from_steps == to_steps {
        return Err(format!(
            "{from} and {to} are the same node — an exchange needs two ends. To change this part's own \
             field use set_part."
        ));
    }
    let on_one_line = if holds(&from_steps, &to_steps) {
        Some((&from, &to))
    } else if holds(&to_steps, &from_steps) {
        Some((&to, &from))
    } else {
        // Neither end contains the other, so the two writes land in different nodes.
        None
    };
    if let Some((holder, held)) = on_one_line {
        return Err(format!(
            "{holder} holds {held}, so exchanging them would write into a node the other write \
             replaces: one of the two subtrees would be lost rather than moved. Exchange two routes \
             neither of which contains the other — and \"root\" contains every route, so it cannot be \
             one end."
        ));
    }

    let from_was = shape_path::resolve(tree, &from_steps)
        .expect("route_of walked to this node")
        .clone();
    let to_was = shape_path::resolve(tree, &to_steps)
        .expect("route_of walked to this node")
        .clone();
    if from_was == to_was {
        // Not a no-op worth reporting as success: the file would come back identical, and a caller who
        // meant to re-parent would read `dirty` and believe the tree moved.
        return Err(format!(
            "{from} and {to} both hold {} ({}) — exchanging equal contents moves nothing. Edit one of \
             them with set_part, or exchange it with a part that differs.",
            shape_path::kind(&from_was),
            shape_path::summary(&from_was),
        ));
    }

    let mut candidate = tree.clone();
    *shape_path::resolve_mut(&mut candidate, &from_steps)
        .expect("resolve read the node at this route a moment ago") = to_was.clone();
    *shape_path::resolve_mut(&mut candidate, &to_steps)
        .expect("resolve read the node at this route a moment ago") = from_was.clone();
    // The selection follows the node the caller moved, which is now at the route they named `to`.
    let before = commit(state, candidate, to_steps.clone())?;

    let snapshot = state_json(state);
    let tree = &state.entity_def.shape;
    // Both ends read back out of the tree the call just left: what the route held and what it holds now,
    // so a move that wrote only one side cannot report the pair it was asked for.
    let end = |spelled: &str, steps: &[Step], was: &Shape| {
        let now =
            shape_path::resolve(tree, steps).expect("an exchange keeps every route resolving");
        json!({
            "route": route_name(tree, steps),
            "called_with": spelled,
            "was": { "kind": shape_path::kind(was), "summary": shape_path::summary(was) },
            "now": { "kind": shape_path::kind(now), "summary": shape_path::summary(now) },
            "children": shape_path::children(now).len(),
        })
    };
    Ok(json!({
        "exchanged": [
            end(&from, &from_steps, &from_was),
            end(&to, &to_steps, &to_was),
        ],
        "selected": snapshot["selected"],
        "nodes_before": before,
        "node_count": snapshot["node_count"],
        "preview_signature": state.preview_signature(),
        "state": snapshot,
    }))
}

/// March the current state on the CPU reference and write the PNG where it can be looked at.
///
/// The filename carries `preview_signature`, so a reader can tell which state produced the frame they are
/// opening — and a second render of a changed state cannot be mistaken for the first.
fn render_view(state: &EditorState, arguments: &Value) -> Result<Value, String> {
    let width = bounded_size(arguments, "width", 160)?;
    let height = bounded_size(arguments, "height", 120)?;
    // Absent means the stage is on, and so does anything that is not a boolean: `stage: null` is a caller
    // that did not ask for a bare sky, not one that asked for it.
    let use_stage = arguments
        .get("stage")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let (name, yaw, pitch, distance) = match arguments.get("view") {
        // A view named by one of the six.
        Some(Value::String(name)) => {
            let (_, yaw, pitch, distance) = VIEWS
                .iter()
                .find(|(view, ..)| *view == name.as_str())
                .ok_or_else(|| {
                    format!(
                        "no view {:?}; the names are {}",
                        name.as_str(),
                        VIEWS
                            .iter()
                            .map(|(v, ..)| format!("{v:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
            (name.as_str(), *yaw, *pitch, *distance)
        }
        // Absent is the caller saying "I steer". A `view` of any other type — `null` included, exactly as
        // `route` and `keep` refuse it — *is* an offered name that failed to be one, and answering it by
        // rendering the yaw/pitch/distance camera instead would hand back a picture from an angle the caller
        // never asked for, under a filename naming that camera.
        None => (
            "custom",
            number(arguments, "yaw")?,
            number(arguments, "pitch")?,
            number(arguments, "distance")?,
        ),
        Some(offered) => {
            return Err(format!(
                "\"view\" must be one of {}; it is {offered}. Leave \"view\" off entirely to steer by \
                 `yaw`, `pitch` and `distance`.",
                VIEWS
                    .iter()
                    .map(|(v, ..)| format!("{v:?}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    };

    let def = &state.entity_def;
    let stage = if use_stage {
        Some(Stage::under(def))
    } else {
        None
    };
    let renderer = CpuRenderer::new(width, height);
    let frame = renderer.render_on_stage(def, &state.materials, yaw, pitch, distance, stage);

    let signature = state.preview_signature();
    let dir = dump_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // The name carries the frame, not just the state: two renders of one state at two sizes, or with the
    // stage on and off, are two different pictures, and a reader handed one path for both cannot tell which
    // frame it is holding. `preview_signature` alone also let two callers writing concurrently truncate each
    // other's file, which is how the collision was found.
    let path = dir.join(format!(
        "{name}-{width}x{height}-{}-{signature:016x}.png",
        if use_stage { "stage" } else { "bare" }
    ));
    image::save_buffer_with_format(
        &path,
        &frame.pixels,
        width,
        height,
        image::ColorType::Rgb8,
        image::ImageFormat::Png,
    )
    .map_err(|e| format!("cannot write {}: {e}", path.display()))?;

    let total = (width * height) as usize;
    // Two sentinels, not one. `NO_HIT` is a pixel that saw sky; `NOT_DRAWN_DEPTH` is a frame the march
    // refused to draw at all (a zero scale, or a non-unit quaternion that came in through a file load
    // rather than this tool). Counting only the first would report a blank frame as one covered
    // entirely by surface.
    let surface = frame
        .depth
        .iter()
        .filter(|t| **t != Frame::NO_HIT && **t != Frame::NOT_DRAWN_DEPTH)
        .count();
    let undrawn = frame
        .depth
        .iter()
        .filter(|t| **t == Frame::NOT_DRAWN_DEPTH)
        .count();
    Ok(json!({
        "path": path.to_string_lossy(),
        "view": name,
        "yaw": yaw,
        "pitch": pitch,
        "distance": distance,
        "width": width,
        "height": height,
        "stage": use_stage,
        "surface_pixels": surface,
        "undrawn_pixels": undrawn,
        "total_pixels": total,
        "preview_signature": signature,
    }))
}

/// The state as the panel sees it, plus the one thing the panel's prose has to say out loud: of the six
/// authored material fields, only `base_color` reaches a pixel.
fn state_json(state: &EditorState) -> Value {
    let def = &state.entity_def;
    // The two lists are the table's `shaded` column, not a second sentence about it: `set_material`
    // refuses with the same table, so a published claim and a refusal cannot name different fields.
    let (state_json_read, state_json_unread) = params::material_names_by_shading();
    let outline = shape_path::outline(&def.shape);
    let parts: Vec<Value> = outline
        .iter()
        .map(|entry| {
            json!({
                "route": entry.name,
                "kind": entry.kind,
                "summary": entry.summary,
                "depth": entry.depth,
                // What `set_part` will take for this node, with the range the panel's slider drags it in.
                "parameters": shape_path::resolve(&def.shape, &entry.path)
                    .map(parameters_json)
                    .unwrap_or_default(),
            })
        })
        .collect();
    // The selection is a path of `Step`s a caller has no name for, so report the route the panel's own
    // list gives that node. A path the tree no longer holds after a load reports null, the same way the
    // panel says the selection is gone rather than editing the root in its place.
    let selected = outline
        .iter()
        .find(|entry| entry.path == state.selected)
        .map(|entry| Value::String(entry.name.clone()))
        .unwrap_or(Value::Null);
    json!({
        "file_path": state.file_path,
        "dirty": state.dirty,
        "preview_signature": state.preview_signature(),
        "selected": selected,
        "transform": {
            "position": def.position,
            "rotation": def.rotation,
            "scale": def.scale,
            "material_id": def.material_id,
            "flags": def.flags,
            "quantity": def.quantity,
        },
        "material": state.materials.get(def.material_id).map(material_json),
        "material_slot_occupied": state.materials.is_occupied(def.material_id),
        "node_count": outline.len(),
        "parts": parts,
        "renderer_reads": state_json_read,
        "authored_but_not_shaded": state_json_unread,
    })
}

fn materials_json(state: &EditorState) -> Value {
    let slots: Vec<Value> = (0..=255u8)
        .filter(|slot| state.materials.is_occupied(*slot))
        .filter_map(|slot| {
            let material = state.materials.get(slot)?;
            let mut value = material_json(material);
            value["slot"] = json!(slot);
            Some(value)
        })
        .collect();
    json!({ "count": slots.len(), "materials": slots })
}

fn material_json(material: &Material) -> Value {
    json!({
        "base_color": material.base_color,
        "roughness": material.roughness,
        "emissive": material.emissive,
        "index_ior": material.index_ior,
        "noise": match material.noise {
            NoiseKind::Voronoi => "Voronoi",
            NoiseKind::Value => "Value",
            NoiseKind::Perlin => "Perlin",
            NoiseKind::None => "None",
        },
        "noise_frequency": material.noise_frequency,
    })
}

/// `set_material`'s published schema for one field, read off the table it also refuses with.
///
/// A schema written by hand beside a table written by hand is two statements of the same limit, and the
/// one that is wrong is the one the caller read first.
fn material_field_schema(field: &params::MaterialField) -> Value {
    if let Some(choices) = field.choices {
        return json!({
            "type": "string",
            "enum": choices.to_vec(),
        });
    }
    if field.lanes == 3 {
        return json!({
            "type": "array",
            "items": { "type": "number" },
            "minItems": field.lanes,
            "maxItems": field.lanes,
        });
    }
    let mut schema = serde_json::Map::new();
    schema.insert("type".to_string(), json!("number"));
    // `exclusiveMinimum` rather than a nudged `minimum`, so the schema says what the check does.
    if field.lo_open {
        schema.insert("exclusiveMinimum".to_string(), json!(field.lo));
    } else {
        schema.insert("minimum".to_string(), json!(field.lo));
    }
    if let Some(hi) = field.hi {
        schema.insert("maximum".to_string(), json!(hi));
    }
    Value::Object(schema)
}

fn string_arg(arguments: &Value, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{key} is required and must be a string"))
}

/// A number the caller already handed over, named by the key it came under.
fn scalar(value: &Value, key: &str) -> Result<f32, String> {
    value
        .as_f64()
        .map(|float| float as f32)
        .ok_or_else(|| format!("{key} must be a number"))
}

fn number(arguments: &Value, key: &str) -> Result<f32, String> {
    arguments
        .get(key)
        .ok_or_else(|| format!("{key} is required and must be a number"))
        .and_then(|value| scalar(value, key))
}

/// One lane of a `position` or `rotation` array. The length is checked per call, because a three-lane
/// `rotation` would otherwise be read as `w: 0` — a quaternion that is not unit and would be refused by
/// the compiler for a reason no caller could see.
fn element(value: &Value, index: usize, key: &str) -> Result<f32, String> {
    let lanes = value
        .as_array()
        .ok_or_else(|| format!("{key} must be an array of numbers"))?;
    let expected = if key == "rotation" { 4 } else { 3 };
    if index == 0 && lanes.len() != expected {
        return Err(format!(
            "{key} must be {expected} numbers, got {}",
            lanes.len()
        ));
    }
    lanes
        .get(index)
        .and_then(Value::as_f64)
        .map(|lane| lane as f32)
        .ok_or_else(|| format!("{key}[{index}] is missing or is not a number"))
}

/// A frame size the box can afford: this is a tool, and a 4096x4096 CPU march is a way to wedge it.
fn bounded_size(arguments: &Value, key: &str, fallback: u32) -> Result<u32, String> {
    match arguments.get(key) {
        Some(value) => {
            let raw = scalar(value, key)?;
            if raw != raw.trunc() || !(1.0..=768.0).contains(&raw) {
                return Err(format!("{key} must be a whole number 1-768, got {raw}"));
            }
            Ok(raw as u32)
        }
        None => Ok(fallback),
    }
}

fn result(id: Value, value: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": value }).to_string()
}

fn error(id: Value, code: i64, message: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }).to_string()
}

/// `tools/list`'s payload, written out beside the names so a new tool without a schema is caught.
pub fn tool_specs() -> Value {
    let mut material_properties = serde_json::Map::new();
    material_properties.insert(
        "slot".to_string(),
        json!({ "type": "integer", "minimum": 0, "maximum": 255 }),
    );
    // The six fields alone, for the call that has no slot to name: they are the same schemas the patch
    // publishes, because both writers read the same table and a caller comparing the two should see one
    // set of limits.
    let mut material_fields = serde_json::Map::new();
    for field in params::MATERIAL_FIELDS {
        let schema = material_field_schema(&field);
        material_properties.insert(field.name.to_string(), schema.clone());
        material_fields.insert(field.name.to_string(), schema);
    }
    let material_field_names: Vec<&str> = params::MATERIAL_FIELDS
        .iter()
        .map(|field| field.name)
        .collect();
    json!([
        {
            "name": "editor_state",
            "description": "The entity the editor holds: file, dirty flag, transform, material, and every node's route and kind.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "list_materials",
            "description": "Every occupied material slot with all six authored fields, including the five the renderer does not yet read.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "set_material",
            "description": "Patch one occupied material slot. `slot` is required and must be a slot `list_materials` reports as occupied — filling an empty one needs every field at once, which is `register` in code and not what a patch means. Each field is limited by the range `entity::Material` states for it, and a value outside it is refused quoting that range. Only `base_color` reaches a pixel: the reply's `changed_a_pixel` and `changed_no_pixel` say which of what you wrote the renderer can see, and the other five wait on OQ-10, the shading model doc 04 § Materials, lighting, sky leaves open. The registry is not part of the entity file, so `save_entity` carries none of what you write here and a reload restores the preset this slot was registered with; `before` is what you overwrote.",
            "inputSchema": {
                "type": "object",
                "properties": material_properties,
                "required": ["slot"],
                "additionalProperties": false
            }
        },
        {
            "name": "register_material",
            "description": "Define a whole material in the registry's next free slot. Every one of the six fields is required at once — a field left off would stand as `Material::default`'s value and not as one you chose — and each is limited by the range `entity::Material` states for it. The slot is the registry's to hand out, not the call's: the reply reports it, and `set_transform` with `material_id` is what points the entity at it. Registering paints nothing by itself, and the registry is not part of the entity file, so a new process starts from the three presets.",
            "inputSchema": {
                "type": "object",
                "properties": material_fields,
                "required": material_field_names,
                "additionalProperties": false
            }
        },
        {
            "name": "load_entity",
            "description": "Read a .ron entity definition into the editor. A broken file answers with the parser's own error.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }
        },
        {
            "name": "set_transform",
            "description": "Set position ([x,y,z]), rotation (unit quaternion), scale (>0) and material_id (an occupied slot). Everything is validated before anything is applied.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "position": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                    "rotation": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
                    "scale": { "type": "number" },
                    "material_id": { "type": "integer", "minimum": 0, "maximum": 255 }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "select_part",
            "description": "Point the editor at one node of the tree by the route editor_state prints for it (root, root.b, root.b.kept). Selecting is not editing: nothing is marked dirty. Answers the node's kind, its current summary and the parameters set_part accepts for it.",
            "inputSchema": {
                "type": "object",
                "properties": { "route": { "type": "string" } },
                "required": ["route"]
            }
        },
        {
            "name": "set_part",
            "description": "Write one authored parameter of the selected node, or of the node a route names: a Sphere's r or centre, a RoundedBox's half or r, a Plane's normal or offset, a SmoothUnion's k, a Repeat's period, a Mirror's axis (a whole 0, 1 or 2). A value outside the range the panel's slider offers is refused with that range, and a parameter the kind does not carry is refused with the ones it does.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "route": { "type": "string" },
                    "parameter": { "type": "string" },
                    "value": { "oneOf": [ { "type": "number" }, { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 } ] }
                },
                "required": ["parameter", "value"]
            }
        },
        {
            "name": "add_part",
            "description": "Grow the tree. With combine Union, Subtract, Intersect or SmoothUnion: wrap the node a route names (or the selected one) in that combinator, joining it to a freshly authored Sphere, RoundedBox or Plane. Every field of the new leaf is required, in the ranges set_part offers; k is required for a SmoothUnion and refused for the rest. keep names which slot the existing node stays in (a/b, or kept/cutter for a Subtract) and defaults to the first. With combine Mirror or Repeat: wrap that node in the one-child wrapper instead — no shape, no keep, and its own field (axis, period) required beside combine in the ranges set_part offers. A Repeat tiles the fold cell [-period/2, period/2) about the origin, so it repeats what lies inside that cell and renders a part authored wholly outside it as nothing; a Mirror folds about its own plane and doubles whatever sits off it. A wrapper that discards part of what it wraps is not an error — it is legal grammar that draws less than it holds — so the wrap lands and answers with wrapped.extent, the box it measured, and wrapped.notice, which names the cell or plane and what the wrap leaves of it; both are null when the wrapper's domain reaches the whole part. Either call refuses, leaving the tree untouched, if the tree it would leave cannot be compiled.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "route": { "type": "string" },
                    "combine": { "type": "string", "enum": ["Union", "Subtract", "Intersect", "SmoothUnion", "Mirror", "Repeat"] },
                    "keep": { "type": "string" },
                    "k": { "type": "number", "minimum": 0, "maximum": 1 },
                    "axis": { "type": "number", "minimum": 0, "maximum": 2 },
                    "period": { "type": "number", "minimum": 0.05, "maximum": 10.0 },
                    "shape": {
                        "type": "object",
                        "properties": {
                            "kind": { "type": "string", "enum": ["Sphere", "RoundedBox", "Plane"] },
                            "r": { "oneOf": [ { "type": "number" }, { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 } ] },
                            "centre": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                            "half": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                            "normal": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                            "offset": { "type": "number" }
                        },
                        "required": ["kind"]
                    }
                },
                "required": ["combine"]
            }
        },
        {
            "name": "delete_part",
            "description": "Take one node out of the tree by route. The two-operand combinator that held it goes too, and its surviving operand takes its place. A one-child wrapper's child cannot be deleted (that would leave a wrapper of nothing). Root cannot be deleted either, unless it is itself a one-child wrapper, in which case deleting it unwraps the tree and its child becomes the root. The selection returns to root, because every path below the collapse means something else now.",
            "inputSchema": {
                "type": "object",
                "properties": { "route": { "type": "string" } },
                "required": ["route"]
            }
        },
        {
            "name": "move_part",
            "description": "Exchange two nodes' places: the node at from ends up at to, and whatever held to ends up at from. There is no free slot in this grammar, so a move that only inserted would have to drop the node it displaces; an exchange keeps the node count and leaves every route outside the two ends exactly where it was. The two ends may not sit on one line from the root (one holding the other), because then one subtree would be overwritten instead of moved, and root holds every route so it is never one end. Equal contents at both ends move nothing and are refused. The selection follows the node that was moved, and a tree the compiler cannot lower — an exchange can deepen the tree — is refused before it lands.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": { "type": "string" },
                    "to": { "type": "string" }
                },
                "required": ["from", "to"],
                "additionalProperties": false
            }
        },
        {
            "name": "graft_part",
            "description": "Re-parent one node: lift the subtree from names out of the tree and join it to the node under names, inside a new Union, Subtract, Intersect or SmoothUnion. The lifted subtree takes the slot keep does not name, and the node under keeps the one it does (a/b, or kept/cutter; it defaults to the first, so a Subtract cuts the lifted part out of under). The lift collapses whichever combinator held from — the same collapse delete_part makes, except that a lifted wrapper leaves with its child instead of being unwrapped — and the graft puts the subtree and one combinator back in its place, so nothing is dropped and the node count is the same before and after. from may not hold under, and under may not be the combinator holding from (nor root when from is one of its operands), because the lift collapses that node; an ancestor of from that is not its holder survives, which is the lift-up shape. k is required for a SmoothUnion and refused for the rest. The selection follows the lifted subtree, and a tree the compiler cannot lower is refused before it lands.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": { "type": "string" },
                    "under": { "type": "string" },
                    "combine": { "type": "string", "enum": ["Union", "Subtract", "Intersect", "SmoothUnion"] },
                    "keep": { "type": "string" },
                    "k": { "type": "number", "minimum": 0, "maximum": 1 }
                },
                "required": ["from", "under", "combine"],
                "additionalProperties": false
            }
        },
        {
            "name": "render_view",
            "description": "March the current entity on the CPU reference and write a PNG to the temp dir. Pass view for one of the six named angles, or yaw/pitch/distance. Returns the path, surface_pixels (rays that hit), undrawn_pixels (a frame the march refused to draw), and the signature the filename carries.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "view": { "type": "string", "enum": VIEWS.iter().map(|(name, ..)| json!(*name)).collect::<Vec<Value>>() },
                    "yaw": { "type": "number" },
                    "pitch": { "type": "number" },
                    "distance": { "type": "number" },
                    "width": { "type": "integer", "minimum": 1, "maximum": 768 },
                    "height": { "type": "integer", "minimum": 1, "maximum": 768 },
                    "stage": { "type": "boolean" }
                }
            }
        },
        {
            "name": "save_entity",
            "description": "Write the entity to a .ron path. The path is required: nothing overwrites the file it was loaded from by default.",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }
        }
    ])
}
