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
use crate::shape_path;
use crate::EditorState;
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
    ("low", -2.4, -0.30, 1.1),
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
    "load_entity",
    "set_transform",
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
        "load_entity" => Some(load(state, arguments)),
        "set_transform" => Some(set_transform(state, arguments)),
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

    let (name, yaw, pitch, distance) = match arguments.get("view").and_then(Value::as_str) {
        Some(name) => {
            let (_, yaw, pitch, distance) = VIEWS
                .iter()
                .find(|(view, ..)| *view == name)
                .ok_or_else(|| {
                    format!(
                        "no view {name:?}; the names are {}",
                        VIEWS
                            .iter()
                            .map(|(v, ..)| format!("{v:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
            (name, *yaw, *pitch, *distance)
        }
        None => (
            "custom",
            number(arguments, "yaw")?,
            number(arguments, "pitch")?,
            number(arguments, "distance")?,
        ),
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
    let path = dir.join(format!("{name}-{signature:016x}.png"));
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
    let outline = shape_path::outline(&def.shape);
    let parts: Vec<Value> = outline
        .iter()
        .map(|entry| {
            json!({
                "route": entry.name,
                "kind": entry.kind,
                "summary": entry.summary,
                "depth": entry.depth,
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
        "renderer_reads": ["base_color"],
        "authored_but_not_shaded": ["roughness", "emissive", "index_ior", "noise", "noise_frequency"],
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
