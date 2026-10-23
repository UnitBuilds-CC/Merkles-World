//! The editor's MCP surface: every tool answers, refusals refuse, and a rendered frame follows the state.
//!
//! These call `editor::mcp::handle` directly — the transport is a loop over stdin, and the last test here
//! runs that loop as a child process to prove the pipe carries the same answers the module does.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use assetc::Shape;
use editor::mcp::{self, TOOL_NAMES};
use editor::EditorState;
use serde_json::{json, Value};

fn pig_ron() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/entities/pig.ron")
        .canonicalize()
        .expect("the shipped pig is what an author actually loads")
}

/// One `tools/call` in, its tool payload out. Panics with the raw reply if the shape is not a tool result,
/// so a broken envelope is reported rather than swallowed by an `unwrap_or_default`.
fn call(state: &mut EditorState, name: &str, arguments: Value) -> Value {
    let line = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
    .to_string();
    let reply: Value = serde_json::from_str(&ask(state, &line)).expect("a reply is one JSON line");
    let content = reply["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("no tool content in {reply}"));
    let text = content[0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text part in {reply}"));
    // Checked before the parse: a refusal's text is prose, and claiming "the tool's text is not JSON"
    // would hide the message the tool actually gave.
    if reply["result"]["isError"] == json!(true) {
        panic!("expected a result, the tool refused: {text}");
    }
    let payload: Value = serde_json::from_str(text)
        .unwrap_or_else(|_| panic!("the tool's text is not JSON: {text}"));
    payload
}

/// The same call when a refusal *is* the expectation: returns the message and whether it refused.
fn try_call(state: &mut EditorState, name: &str, arguments: Value) -> (String, bool) {
    let line = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
    .to_string();
    let reply: Value = serde_json::from_str(&ask(state, &line)).expect("one JSON line back");
    let content = reply["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("no tool content in {reply}"));
    (
        content[0]["text"].as_str().unwrap_or("").to_string(),
        reply["result"]["isError"] == json!(true),
    )
}

fn ask(state: &mut EditorState, line: &str) -> String {
    mcp::handle(state, line).expect("a request with an id draws a reply")
}

/// A fresh state, so a test cannot pass on another test's edit.
fn fresh() -> EditorState {
    EditorState::new()
}

#[test]
fn the_handshake_answers_and_a_notification_says_nothing() {
    let mut state = fresh();
    let init: Value = serde_json::from_str(&ask(
        &mut state,
        &json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}).to_string(),
    ))
    .expect("initialize is JSON");
    assert_eq!(
        init["result"]["protocolVersion"],
        json!(mcp::PROTOCOL_VERSION)
    );
    assert_eq!(init["result"]["serverInfo"]["name"], json!("merkle-editor"));

    let silence = mcp::handle(
        &mut state,
        &json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string(),
    );
    assert!(
        silence.is_none(),
        "a notification has no id, so a reply to it is a message no client asked for"
    );
}

#[test]
fn every_claimed_tool_has_a_schema_and_answers() {
    let mut state = fresh();
    let listing: Value = serde_json::from_str(&ask(
        &mut state,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}).to_string(),
    ))
    .expect("tools/list is JSON");
    let tools = listing["result"]["tools"]
        .as_array()
        .expect("tools is an array");

    for name in TOOL_NAMES {
        let spec = tools
            .iter()
            .find(|tool| tool["name"] == json!(*name))
            .unwrap_or_else(|| {
                panic!("tools/list does not describe {name}, so a client cannot call it")
            });
        assert!(
            spec["inputSchema"].is_object(),
            "{name} has no inputSchema, so nothing validates its arguments"
        );
    }
    assert_eq!(
        tools.len(),
        TOOL_NAMES.len(),
        "tools/list carries {} entries while TOOL_NAMES claims {} - one of them is undocumented or unclaimed",
        tools.len(),
        TOOL_NAMES.len()
    );

    for name in TOOL_NAMES {
        let line = json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": name, "arguments": {} },
        })
        .to_string();
        let reply: Value = serde_json::from_str(&ask(&mut state, &line)).expect("a reply");
        assert!(
            reply["result"]["content"].is_array(),
            "{name} returned {reply} instead of tool content - a name in TOOL_NAMES with no arm in \
             dispatch is a tool a client is told about and cannot use"
        );
    }
}

#[test]
fn a_non_json_line_and_an_unknown_method_are_protocol_errors_not_tool_failures() {
    let mut state = fresh();
    let junk: Value = serde_json::from_str(&ask(&mut state, "initialize, please"))
        .expect("an error is still JSON");
    assert_eq!(junk["error"]["code"], json!(-32700));

    let unknown: Value = serde_json::from_str(&ask(
        &mut state,
        &json!({"jsonrpc":"2.0","id":9,"method":"tools/run"}).to_string(),
    ))
    .expect("an error is still JSON");
    assert_eq!(unknown["error"]["code"], json!(-32601));

    let missing_tool: Value = serde_json::from_str(&ask(
        &mut state,
        &json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"teleport"}})
            .to_string(),
    ))
    .expect("an error is still JSON");
    assert_eq!(missing_tool["error"]["code"], json!(-32602));
}

#[test]
fn loading_the_shipped_pig_reports_its_tree_and_names_the_fields_no_shader_reads() {
    let mut state = fresh();
    let loaded = call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let state_block = &loaded["state"];
    assert_eq!(state_block["file_path"], json!(pig_ron().to_string_lossy()));
    assert_eq!(state_block["dirty"], json!(false));

    let nodes = state_block["node_count"].as_u64().expect("a node count");
    assert!(
        nodes > 30,
        "the re-authored pig is a 30-plus node animal; a tree this small means the outline walked \
         something else: {nodes}"
    );
    let parts = state_block["parts"].as_array().expect("parts is a list");
    assert_eq!(
        parts.iter().filter(|p| p["route"] == json!("root")).count(),
        1,
        "the outline must name the root exactly once, or a caller has no route to start from"
    );
    // A fresh selection is empty, and an empty path resolves to the root — which is the node the shape
    // sliders would write. Reporting null here would hide where the next edit lands.
    assert_eq!(state_block["selected"], json!("root"));

    // The honest part: the panel lets an author set six fields and the renderer reads one.
    assert_eq!(state_block["renderer_reads"], json!(["base_color"]));
    assert_eq!(
        state_block["authored_but_not_shaded"]
            .as_array()
            .expect("a list")
            .len(),
        5
    );
    let material = &state_block["material"];
    for field in [
        "base_color",
        "roughness",
        "emissive",
        "index_ior",
        "noise",
        "noise_frequency",
    ] {
        assert!(
            material.get(field).is_some(),
            "the material block dropped {field}, so the caller cannot see what it is not being shown"
        );
    }
}

#[test]
fn a_broken_ron_answers_with_the_parser_error_and_leaves_the_state_untouched() {
    let mut state = fresh();
    let before = call(&mut state, "editor_state", json!({}));
    let junk = temp_path("broken_entity.ron");
    std::fs::write(&junk, "EntityDef( this is not a shape )").expect("the fixture is writable");

    let (message, refused) = try_call(
        &mut state,
        "load_entity",
        json!({ "path": junk.to_string_lossy() }),
    );
    let _ = std::fs::remove_file(&junk);
    assert!(
        refused,
        "a file that cannot parse must not read as a load: {message}"
    );
    assert!(
        message.contains("Failed to parse") && message.contains(&junk.display().to_string()),
        "the refusal should be the parser's own words about the file it was given, got: {message}"
    );

    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        after["node_count"], before["node_count"],
        "the refused load replaced the tree anyway - a half-loaded entity is the picture the file does \
         not describe"
    );
    assert_eq!(after["file_path"], before["file_path"]);
}

#[test]
fn a_refused_edit_moves_nothing_and_a_real_edit_moves_the_preview_key() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));

    // One call, one bad clause: the legal `position` must not land on its own.
    let (message, refused) = try_call(
        &mut state,
        "set_transform",
        json!({ "position": [4.0, 0.0, 0.0], "scale": 0.0 }),
    );
    assert!(
        refused,
        "a scale of zero compiled is not a preview: {message}"
    );
    let after_refusal = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        after_refusal["transform"]["position"], before["transform"]["position"],
        "the refusal kept the position half of the call - partial application is how an author gets a \
         file that says something else"
    );
    assert_eq!(after_refusal["dirty"], json!(false));

    let (message, refused) = try_call(
        &mut state,
        "set_transform",
        json!({ "rotation": [0.0, 0.0, 0.0, 0.5] }),
    );
    assert!(
        refused,
        "a non-unit quaternion is what compile_entity refuses: {message}"
    );
    assert!(
        message.contains("|q|"),
        "the refusal should say which clause and by how much, got: {message}"
    );

    let (message, refused) = try_call(&mut state, "set_transform", json!({ "material_id": 7 }));
    assert!(
        refused,
        "slot 7 is empty, and the viewport would draw it white: {message}"
    );
    assert!(
        message.contains("empty"),
        "name the empty slot rather than a general failure, got: {message}"
    );

    let edited = call(
        &mut state,
        "set_transform",
        json!({ "position": [4.0, 0.0, 0.0], "scale": 1.5 }),
    );
    assert_eq!(edited["changed"], json!(["position", "scale"]));
    assert_eq!(edited["state"]["dirty"], json!(true));
    assert_ne!(
        edited["preview_signature"], before["preview_signature"],
        "a real edit left the preview key alone, so the viewport would keep showing the old frame"
    );

    let again = call(&mut state, "set_transform", json!({ "scale": 1.5 }));
    assert_eq!(
        again["changed"],
        json!([]),
        "re-setting a value that was already there reported a change"
    );
}

#[test]
fn render_view_writes_a_png_and_a_changed_state_renders_a_different_one() {
    // No cleanup here on purpose: the dump dir is scoped to this process, and every test in this file
    // shares it, so a `remove_dir_all` at the end of one could delete a frame another is still reading.
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );

    let small = json!({ "view": "front", "width": 64, "height": 48 });
    let first = call(&mut state, "render_view", small.clone());
    let path = PathBuf::from(first["path"].as_str().expect("a path back"));
    let bytes =
        std::fs::read(&path).unwrap_or_else(|e| panic!("{} was not written: {e}", path.display()));
    assert_eq!(
        &bytes[0..4],
        &[0x89, b'P', b'N', b'G'],
        "the dump is not a PNG, so nothing a reader opens it with will agree about what it shows"
    );

    let surface = first["surface_pixels"].as_u64().expect("a count");
    let total = first["total_pixels"].as_u64().expect("a count");
    assert_eq!(total, json!(64 * 48).as_u64().expect("64x48"));
    assert!(
        surface > 0 && surface < total,
        "the front view of a pig standing on a stage covers part of the frame, not {} of {}",
        surface,
        total
    );

    call(&mut state, "set_transform", json!({ "scale": 2.0 }));
    let second = call(&mut state, "render_view", small);
    let second_path = PathBuf::from(second["path"].as_str().expect("a path back"));
    assert_ne!(
        second_path, path,
        "two different states wrote one filename, so the reader cannot tell which frame they are holding"
    );
    assert_ne!(
        second["surface_pixels"], first["surface_pixels"],
        "doubling the entity's scale left the hit count alone - this march does not read the state it \
         claims to render"
    );
}

#[test]
fn a_frame_the_march_refuses_to_draw_says_so_instead_of_reading_as_all_surface() {
    // Reached only through a file: `set_transform` refuses a non-unit quaternion, but `load_ron` takes
    // the file as it is and the CPU march then draws nothing at all. A hit count that treated the
    // not-drawn sentinel as a surface would report that blank frame as one covered entirely by pig.
    let mut state = fresh();
    let wild = temp_path("wild_rotation.ron");
    std::fs::write(
        &wild,
        "EntityDef(\n    shape: Sphere(r: 1.0, centre: (0.0, 0.0, 0.0)),\n    position: (0.0, 0.0, 0.0),\n    rotation: (0.0, 2.0, 0.0, 1.0),\n    scale: 1.0,\n    material_id: 0,\n    flags: 3,\n    quantity: 0,\n)",
    )
    .expect("the fixture is writable");

    call(
        &mut state,
        "load_entity",
        json!({ "path": wild.to_string_lossy() }),
    );
    let rendered = call(
        &mut state,
        "render_view",
        json!({ "view": "front", "width": 32, "height": 24 }),
    );
    let _ = std::fs::remove_file(&wild);

    assert_eq!(
        rendered["undrawn_pixels"],
        json!(32 * 24),
        "the whole frame should be reported as undrawn, not quietly counted as surface"
    );
    assert_eq!(rendered["surface_pixels"], json!(0));
}

#[test]
fn an_unknown_view_names_the_six_it_knows_and_a_oversized_frame_is_refused() {
    let mut state = fresh();
    let (message, refused) = try_call(&mut state, "render_view", json!({ "view": "profile" }));
    assert!(refused, "{message}");
    for name in mcp::VIEWS.iter().map(|(v, ..)| *v) {
        assert!(
            message.contains(name),
            "the refusal should teach the caller the names that work, and {} is missing from: {message}",
            name
        );
    }

    let (message, refused) = try_call(
        &mut state,
        "render_view",
        json!({ "view": "front", "width": 4096 }),
    );
    assert!(
        refused,
        "a 4096-wide CPU march is a way to wedge the tool: {message}"
    );
}

#[test]
fn a_view_that_is_not_a_string_is_refused_rather_than_read_as_no_view_at_all() {
    let mut state = fresh();
    // The hazard is not a missing argument. Beside a complete custom camera, a `view` the tool cannot name
    // used to fall into the custom arm and answer with a picture from an angle nobody asked for, filed under
    // that camera's own name — which is the disagreement this pipe refuses everywhere else.
    let camera = json!({
        "yaw": core::f32::consts::PI, "pitch": 0.12, "distance": 6.0,
        "width": 32, "height": 24,
    });
    for offered in [
        json!(0),
        json!(true),
        json!(["front"]),
        json!({ "name": "front" }),
        // `null` is offered-and-failed rather than absent — the same reading `route` and `keep` give it, and
        // the schema publishes `"type": "string"`.
        Value::Null,
    ] {
        let mut arguments = camera.as_object().expect("a camera object").clone();
        arguments.insert("view".to_string(), offered.clone());
        let (message, refused) = try_call(&mut state, "render_view", Value::Object(arguments));
        assert!(
            refused,
            "a render_view handed {offered} as its view answered by rendering: {message}"
        );
        for name in mcp::VIEWS.iter().map(|(v, ..)| *v) {
            assert!(
                message.contains(name),
                "the refusal should name the views that do work, and {name} is missing from: {message}"
            );
        }
        assert!(
            message.contains("Leave \"view\" off entirely"),
            "the refusal does not say how to get the custom camera it is describing: {message}"
        );
    }

    // Absence is the only spelling that steers, and the reply says which arm it took rather than leaving the
    // caller to infer it.
    let steered = call(&mut state, "render_view", camera.clone());
    assert_eq!(steered["view"], json!("custom"));
    assert_eq!(steered["undrawn_pixels"], json!(0));
    assert!(
        steered["path"]
            .as_str()
            .unwrap_or_default()
            .contains("custom-"),
        "the file should be named for the camera it came from: {}",
        steered["path"]
    );
}

#[test]
fn save_entity_writes_where_it_is_told_and_refuses_to_guess() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let (message, refused) = try_call(&mut state, "save_entity", json!({}));
    assert!(
        refused,
        "a default path would rewrite the file the engine loads: {message}"
    );

    let out = temp_path("saved_entity.ron");
    let saved = call(
        &mut state,
        "save_entity",
        json!({ "path": out.to_string_lossy() }),
    );
    assert!(saved["bytes"].as_u64().expect("a size") > 100);
    let round_trip = try_call(
        &mut fresh(),
        "load_entity",
        json!({ "path": out.to_string_lossy() }),
    );
    let _ = std::fs::remove_file(&out);
    assert!(
        !round_trip.1,
        "the file this wrote cannot be loaded back: {}",
        round_trip.0
    );
}

#[test]
fn a_route_from_the_parts_list_selects_that_node_and_edits_nothing() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let parts = before["parts"].as_array().expect("parts is a list");

    // From the caller's own vantage point: the deepest part that carries a parameter is the row an author
    // would click, and for the pig it sits well below the root's blend factor.
    let picked = parts
        .iter()
        .filter(|part| {
            !part["parameters"]
                .as_array()
                .expect("every part publishes its parameter list")
                .is_empty()
        })
        .max_by_key(|part| part["depth"].as_u64().expect("a depth"))
        .expect("the pig has parts a caller can edit");
    let route = picked["route"].as_str().expect("a route").to_string();
    assert!(
        route.contains('.'),
        "the deepest editable part should not be the root: {route}"
    );

    let selected = call(&mut state, "select_part", json!({ "route": route }));
    assert_eq!(selected["selected"], json!(route));
    assert_eq!(selected["kind"], picked["kind"]);
    assert_eq!(selected["summary"], picked["summary"]);
    assert_eq!(
        selected["parameters"], picked["parameters"],
        "select_part and editor_state describe one node's fields two ways, so a caller cannot tell which \
         list set_part will accept"
    );

    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(after["selected"], json!(route));
    assert_eq!(
        after["dirty"],
        json!(false),
        "selecting is not an edit - a caller that only asked where the snout is must not leave the file \
         changed and the next save overwrite it"
    );
    assert_eq!(
        after["preview_signature"], before["preview_signature"],
        "a pure selection moved the preview key, so the viewport would throw away a frame it never edited"
    );

    // The short spelling reaches the same node: `assetc`'s refusals name fields without the leading
    // `root.`, and a caller holding one of those should be able to paste what it got.
    let short = route.strip_prefix("root.").expect("a dotted route");
    let again = call(&mut state, "select_part", json!({ "route": short }));
    assert_eq!(
        again["selected"],
        json!(route),
        "{short} and {route} reached different nodes"
    );
}

#[test]
fn a_refused_route_names_what_exists_and_leaves_the_selection_where_it_was() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    // Pin the selection somewhere known, so a refusal that moved it is visible.
    call(&mut state, "select_part", json!({ "route": "root.a" }));
    let before = call(&mut state, "editor_state", json!({}));
    let parts = before["parts"].as_array().expect("parts").clone();

    let (message, refused) = try_call(&mut state, "select_part", json!({ "route": "root.q" }));
    assert!(
        refused,
        "root has no child q, and this read as a selection: {message}"
    );
    assert!(
        message.contains("\"a\"") && message.contains("\"b\""),
        "the refusal should name the children the node actually has, got: {message}"
    );

    // A leaf, asked for anything at all: the answer is that it is a leaf, plus the fields it does carry.
    let leaf = parts
        .iter()
        .find(|part| part["kind"] == json!("Sphere"))
        .expect("the pig is made of spheres");
    let leaf_route = leaf["route"].as_str().expect("a route");
    let (message, refused) = try_call(
        &mut state,
        "select_part",
        json!({ "route": format!("{leaf_route}.kept") }),
    );
    assert!(
        refused,
        "{leaf_route} is a sphere - it has no `kept`: {message}"
    );
    assert!(
        message.contains("no children")
            && message.contains("\"r\"")
            && message.contains("\"centre\""),
        "a route off a leaf should say it is a leaf and name the fields it does have, got: {message}"
    );

    // An empty route is not "the root": the root is spelled, or a caller silently edits the whole entity.
    let (message, refused) = try_call(&mut state, "select_part", json!({ "route": "  " }));
    assert!(
        refused,
        "an empty route selected the root anyway: {message}"
    );
    assert!(
        message.contains("parts list"),
        "the refusal should point at the list that names the routes, got: {message}"
    );

    let (message, refused) = try_call(&mut state, "select_part", json!({}));
    assert!(refused, "route is required, and this answered: {message}");

    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        after["selected"], before["selected"],
        "a refused select_part moved the selection, so the next set_part lands on a part the caller never \
         asked about"
    );
    assert_eq!(after["preview_signature"], before["preview_signature"]);
    assert_eq!(after["dirty"], json!(false));
}

#[test]
fn set_part_writes_one_field_of_one_node_and_the_saved_file_reads_it_back() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let parts = before["parts"].as_array().expect("parts").clone();

    // A node below the root's own operands: an edit here cannot be the transform in disguise.
    let sphere = parts
        .iter()
        .find(|part| part["kind"] == json!("Sphere") && part["depth"].as_u64() == Some(2))
        .or_else(|| parts.iter().find(|part| part["kind"] == json!("Sphere")))
        .expect("the pig has a sphere to edit");
    let route = sphere["route"].as_str().expect("a route").to_string();
    let radius = sphere["parameters"]
        .as_array()
        .expect("a sphere publishes its fields")
        .iter()
        .find(|param| param["name"] == json!("r"))
        .expect("a sphere publishes r")
        .clone();
    let range = radius["range"]
        .as_array()
        .expect("a published range")
        .clone();
    let (lo, hi) = (
        range[0].as_f64().expect("lo"),
        range[1].as_f64().expect("hi"),
    );

    let mut wrote = call(
        &mut state,
        "set_part",
        json!({ "route": route, "parameter": "r", "value": hi }),
    );
    if wrote["changed"] == json!(false) {
        wrote = call(
            &mut state,
            "set_part",
            json!({ "route": route, "parameter": "r", "value": lo }),
        );
    }
    assert_eq!(
        wrote["changed"],
        json!(true),
        "neither endpoint of the published range moved {route}.r, so the write never landed"
    );
    assert_eq!(wrote["parameter"], json!("r"));
    assert_ne!(
        wrote["from"], wrote["to"],
        "changed without a move: {:?} -> {:?}",
        wrote["from"], wrote["to"]
    );
    assert_eq!(wrote["state"]["dirty"], json!(true));
    assert_ne!(
        wrote["preview_signature"], before["preview_signature"],
        "a part edit left the preview key alone, so the viewport keeps showing the frame from before it"
    );
    assert_eq!(
        wrote["state"]["transform"], before["transform"],
        "editing {route}.r moved the entity's transform"
    );
    let edited_part = wrote["state"]["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .find(|part| part["route"] == json!(route))
        .expect("the edited node is still in the outline")
        .clone();
    assert_eq!(edited_part["summary"], wrote["summary"]);

    // The claim a save has to keep: the field this wrote is in the file, and loading that file puts it
    // back where the route says it belongs.
    let out = temp_path("increment2_round_trip.ron");
    call(
        &mut state,
        "save_entity",
        json!({ "path": out.to_string_lossy() }),
    );
    let mut reloaded = fresh();
    let loaded = call(
        &mut reloaded,
        "load_entity",
        json!({ "path": out.to_string_lossy() }),
    );
    let _ = std::fs::remove_file(&out);
    let same_route = loaded["state"]["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .find(|part| part["route"] == json!(route))
        .unwrap_or_else(|| {
            panic!("the saved file has no {route} - the tree changed shape on the round trip")
        });
    assert_eq!(
        same_route["summary"], wrote["summary"],
        "the radius this wrote did not survive the file it saved: wrote {:?}, read back {:?}",
        wrote["summary"], same_route["summary"]
    );
}

#[test]
fn a_parameter_the_node_does_not_carry_is_refused_with_the_ones_it_does() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let parts = before["parts"].as_array().expect("parts").clone();

    // A combinator has no field of its own, and the refusal has to send the caller back to the routes.
    let union = parts
        .iter()
        .find(|part| part["kind"] == json!("Union"))
        .expect("the pig unions its parts");
    assert!(
        union["parameters"].as_array().expect("a list").is_empty(),
        "a Union publishes {}, so it is not operand-only",
        union["parameters"]
    );
    let (message, refused) = try_call(
        &mut state,
        "set_part",
        json!({ "route": union["route"], "parameter": "r", "value": 1.0 }),
    );
    assert!(refused, "a Union took a radius: {message}");
    assert!(
        message.contains("reached by route"),
        "the refusal should point at the operands instead of a radius, got: {message}"
    );

    // A node that does carry fields, asked for one belonging to a different kind.
    let sphere = parts
        .iter()
        .find(|part| part["kind"] == json!("Sphere"))
        .expect("the pig has a sphere");
    let (message, refused) = try_call(
        &mut state,
        "set_part",
        json!({ "route": sphere["route"], "parameter": "period", "value": 0.5 }),
    );
    assert!(
        refused,
        "a sphere has no period - that is a Repeat's field: {message}"
    );
    assert!(
        message.contains("Sphere")
            && message.contains("\"period\"")
            && message.contains("\"r\"")
            && message.contains("\"centre\""),
        "the refusal should name the node's kind, the field asked for, and the two it takes, got: {message}"
    );

    let (message, refused) = try_call(
        &mut state,
        "set_part",
        json!({ "route": sphere["route"], "parameter": "r" }),
    );
    assert!(refused, "a call with no value wrote something: {message}");
    assert!(
        message.contains("value is required"),
        "say what is missing rather than a general failure, got: {message}"
    );

    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        after["parts"], before["parts"],
        "a refused set_part left a field moved somewhere in the tree"
    );
    assert_eq!(after["dirty"], json!(false));
}

#[test]
fn a_value_the_slider_would_not_reach_is_refused_quoting_the_range_it_offers() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let parts = before["parts"].as_array().expect("parts").clone();
    let sphere = parts
        .iter()
        .find(|part| part["kind"] == json!("Sphere"))
        .expect("the pig has a sphere");
    let route = sphere["route"].as_str().expect("a route").to_string();

    // Just outside the top and the bottom of the published range.
    for value in [5.5, -0.5] {
        let (message, refused) = try_call(
            &mut state,
            "set_part",
            json!({ "route": route, "parameter": "r", "value": value }),
        );
        assert!(
            refused,
            "r {value} is outside the 0.01..=5 the panel drags, and this took it: {message}"
        );
        assert!(
            message.contains("0.01..=5"),
            "the refusal should quote the same limits the published parameter carries, got: {message}"
        );
    }
    let after_refusal = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        after_refusal["parts"], before["parts"],
        "a refused r moved a node anyway - the range check ran after the write"
    );
    assert_eq!(after_refusal["dirty"], json!(false));

    // A three-lane field asked for the wrong number of lanes.
    let (message, refused) = try_call(
        &mut state,
        "set_part",
        json!({ "route": route, "parameter": "centre", "value": [1.0, 2.0] }),
    );
    assert!(
        refused,
        "a two-lane centre would leave the third lane authored-but-invisible: {message}"
    );
    assert!(
        message.contains("3 numbers"),
        "say how many lanes the field takes, got: {message}"
    );
    let (message, refused) = try_call(
        &mut state,
        "set_part",
        json!({ "route": route, "parameter": "centre", "value": 1.0 }),
    );
    assert!(refused, "a number cannot be a centre: {message}");
    assert!(
        message.contains("array"),
        "the refusal should name the shape it wanted, got: {message}"
    );

    // `Mirror.axis` is the file's integer, so 1.5 must be refused rather than rounded to a different animal.
    let mirror = parts
        .iter()
        .find(|part| part["kind"] == json!("Mirror"))
        .expect("the pig mirrors its legs");
    let mirror_route = mirror["route"].as_str().expect("a route").to_string();
    let (message, refused) = try_call(
        &mut state,
        "set_part",
        json!({ "route": mirror_route, "parameter": "axis", "value": 1.5 }),
    );
    assert!(
        refused,
        "a half axis is not a choice of x, y or z: {message}"
    );
    assert!(
        message.contains("whole choice"),
        "name the integrality, not the range, got: {message}"
    );

    let (message, refused) = try_call(
        &mut state,
        "set_part",
        json!({ "route": mirror_route, "parameter": "axis", "value": 3 }),
    );
    assert!(
        refused,
        "axis 3 is off the end of the three axes: {message}"
    );
    assert!(
        message.contains("0..=2"),
        "the refusal should quote the axis range, got: {message}"
    );

    // The endpoint the range includes is reachable - `..=` that refused its own bound would make a field
    // uneditable at its limit.
    let at_limit = call(
        &mut state,
        "set_part",
        json!({ "route": mirror_route, "parameter": "axis", "value": 2 }),
    );
    assert_eq!(at_limit["to"], json!([2.0]));
    assert_eq!(
        at_limit["changed"],
        json!(true),
        "the pig's mirror was already on axis z, so this proves nothing about the endpoint"
    );

    let after = call(&mut state, "editor_state", json!({}));
    assert_ne!(
        after["parts"], before["parts"],
        "the accepted endpoint never reached the state"
    );
}

#[test]
fn every_kind_writes_the_parameters_it_publishes_and_refuses_the_ones_it_does_not() {
    // The nine authored kinds, each with the fields its own `.ron` spelling gives them and the limits the
    // panel's sliders stop at. These are written down here rather than read back out of `params`, because
    // the point is to catch the table and the file drifting apart - and because a field a kind publishes
    // but cannot write only shows up when something actually tries to write it.
    // One authored field as (name, lo, hi, lanes), and one kind's whole row.
    type Field = (&'static str, f32, f32, u8);
    type Case = (
        &'static str,
        Shape,
        &'static [&'static str],
        &'static [Field],
    );
    let leaf = || {
        Box::new(Shape::Sphere {
            r: 0.5,
            centre: [0.0, 0.0, 0.0],
        })
    };
    let cases: Vec<Case> = vec![
        (
            "Sphere",
            Shape::Sphere {
                r: 1.0,
                centre: [0.25, 0.25, 0.25],
            },
            &[],
            &[("r", 0.01, 5.0, 1), ("centre", -5.0, 5.0, 3)],
        ),
        (
            "RoundedBox",
            Shape::RoundedBox {
                half: [0.5, 0.5, 0.5],
                r: 0.2,
            },
            &[],
            &[("half", 0.01, 5.0, 3), ("r", 0.0, 2.0, 1)],
        ),
        (
            "Plane",
            Shape::Plane {
                normal: [0.25, 0.5, 0.25],
                offset: 0.0,
            },
            &[],
            &[("normal", -1.0, 1.0, 3), ("offset", -10.0, 10.0, 1)],
        ),
        (
            "Union",
            Shape::Union {
                a: leaf(),
                b: leaf(),
            },
            &["a", "b"],
            &[],
        ),
        (
            "SmoothUnion",
            Shape::SmoothUnion {
                a: leaf(),
                b: leaf(),
                k: 0.4,
            },
            &["a", "b"],
            &[("k", 0.0, 1.0, 1)],
        ),
        (
            "Subtract",
            Shape::Subtract {
                kept: leaf(),
                cutter: leaf(),
            },
            &["kept", "cutter"],
            &[],
        ),
        (
            "Intersect",
            Shape::Intersect {
                a: leaf(),
                b: leaf(),
            },
            &["a", "b"],
            &[],
        ),
        (
            "Repeat",
            Shape::Repeat {
                period: 1.0,
                child: leaf(),
            },
            &["child"],
            &[("period", 0.05, 10.0, 1)],
        ),
        (
            "Mirror",
            Shape::Mirror {
                axis: 1,
                child: leaf(),
            },
            &["child"],
            &[("axis", 0.0, 2.0, 1)],
        ),
    ];
    let value_for = |lanes: u8, whole: bool, lane: f32| -> Value {
        let lane = if whole { lane.round() } else { lane };
        if lanes == 1 {
            json!(lane)
        } else {
            json!([lane, lane, lane])
        }
    };

    let mut params_checked = 0usize;
    let mut child_routes_checked = 0usize;
    for (kind, shape, kids, expected) in cases {
        let mut state = fresh();
        state.entity_def.shape = shape;
        let snapshot = call(&mut state, "editor_state", json!({}));
        let part = &snapshot["parts"].as_array().expect("parts")[0];
        assert_eq!(part["route"], json!("root"));
        assert_eq!(part["kind"], json!(kind));

        let published = part["parameters"].as_array().expect("a list").clone();
        assert_eq!(
            published.len(),
            expected.len(),
            "{kind} publishes {} fields while this test expects {expected:?}: {published:?}",
            published.len()
        );
        params_checked += published.len();

        for (index, got) in published.iter().enumerate() {
            let (name, lo, hi, lanes) = expected[index];
            assert_eq!(got["name"], json!(name), "{kind} publishes {got:?}");
            assert_eq!(
                got["lanes"],
                json!(lanes),
                "{kind}.{name} is written with {} lanes by the panel and {} here",
                lanes,
                got["lanes"]
            );
            assert_eq!(
                got["range"],
                json!([lo, hi]),
                "{kind}.{name} offers a range the slider does not stop at: {got:?}"
            );
            let whole = got["whole"]
                .as_bool()
                .unwrap_or_else(|| panic!("{kind}.{name} publishes no whole flag: {got:?}"));
            assert_eq!(whole, kind == "Mirror", "{kind}.{name} is whole: {got:?}");
            let range_text = format!("{lo}..={hi}");

            // Both included endpoints, written for real.
            for endpoint in [lo, hi] {
                let written = call(
                    &mut state,
                    "set_part",
                    json!({
                        "route": "root",
                        "parameter": name,
                        "value": value_for(lanes, whole, endpoint),
                    }),
                );
                assert_eq!(
                    written["changed"],
                    json!(true),
                    "{kind}.{name} did not move when handed its own {range_text} endpoint {endpoint}"
                );
                assert_eq!(
                    written["state"]["dirty"],
                    json!(true),
                    "{kind}.{name} = {endpoint} landed and the file still reads clean"
                );
            }

            // Re-writing the value the node now holds must report no change. That is the only way to see a
            // field the table publishes but the writer silently skips: it would answer `changed: true` to
            // every write and never once keep one.
            let settled = call(
                &mut state,
                "set_part",
                json!({
                    "route": "root",
                    "parameter": name,
                    "value": value_for(lanes, whole, hi),
                }),
            );
            assert_eq!(
                settled["changed"],
                json!(false),
                "{kind}.{name} did not keep the value it was last handed: wrote {:?}, read back {:?}",
                settled["to"],
                settled["from"]
            );

            // Just outside each end: refused, quoting the range, and moving nothing.
            let current = call(&mut state, "editor_state", json!({}));
            for outside in [lo - 1.0, hi + 1.0] {
                let (message, refused) = try_call(
                    &mut state,
                    "set_part",
                    json!({
                        "route": "root",
                        "parameter": name,
                        "value": value_for(lanes, whole, outside),
                    }),
                );
                assert!(
                    refused,
                    "{kind}.{name} took {outside}, outside its own {range_text}: {message}"
                );
                assert!(
                    message.contains(&range_text),
                    "the refusal for {kind}.{name} should quote {range_text}, got: {message}"
                );
            }
            let still = call(&mut state, "editor_state", json!({}));
            assert_eq!(
                still["parts"], current["parts"],
                "a refused {kind}.{name} left the tree changed"
            );

            if whole {
                let (message, refused) = try_call(
                    &mut state,
                    "set_part",
                    json!({ "route": "root", "parameter": name, "value": 0.5 }),
                );
                assert!(
                    refused,
                    "{kind}.{name} is the file's integer and 0.5 was rounded: {message}"
                );
                assert!(message.contains("whole choice"), "{message}");
            }

            // A field from another kind of the same tree is not this one's. `period` belongs to Repeat
            // alone and `r` to the two primitives that round a corner, so each is foreign to the other's
            // holders.
            let other = if name == "period" { "r" } else { "period" };
            let (message, refused) = try_call(
                &mut state,
                "set_part",
                json!({ "route": "root", "parameter": other, "value": 1.0 }),
            );
            assert!(
                refused,
                "{kind} accepted a {other:?}, which is not one of {expected:?}: {message}"
            );
        }

        // A combinator has no field of its own, and the refusal sends the caller back to the routes.
        if expected.is_empty() {
            let (message, refused) = try_call(
                &mut state,
                "set_part",
                json!({ "route": "root", "parameter": "r", "value": 1.0 }),
            );
            assert!(
                refused && message.contains("reached by route"),
                "{kind} is a combinator, and an `r` request answered: {message}"
            );
        }

        // The route grammar, per kind: the children the file names, and nothing else.
        if kids.is_empty() {
            let (message, refused) =
                try_call(&mut state, "select_part", json!({ "route": "root.a" }));
            assert!(
                refused && message.contains("no children"),
                "{kind} is a leaf and a route into it read as a selection: {message}"
            );
        } else {
            for kid in kids {
                let selected = call(
                    &mut state,
                    "select_part",
                    json!({ "route": format!("root.{kid}") }),
                );
                assert_eq!(selected["selected"], json!(format!("root.{kid}")));
                child_routes_checked += 1;
            }
            let (message, refused) =
                try_call(&mut state, "select_part", json!({ "route": "root.q" }));
            assert!(refused, "{kind} has no child q: {message}");
            for kid in kids {
                assert!(
                    message.contains(&format!("{kid:?}")),
                    "the refusal for {kind} should name {kid:?}, got: {message}"
                );
            }
        }
    }

    // The coverage claim, as a number: nine kinds, nine authored fields, and the ten named child routes
    // the six wrapper kinds open. A table that grows a field, or a kind this loop skips, moves one of these.
    assert_eq!(
        params_checked, 9,
        "the nine authored kinds carry nine authored fields between them; {params_checked} were tested, \
         so this list and `params` no longer describe the same grammar"
    );
    assert_eq!(
        child_routes_checked, 10,
        "the six wrapper kinds expose ten named child routes (two each for a, b, kept and cutter, and one \
         child each on Repeat and Mirror)"
    );
}

#[test]
fn the_transport_answers_the_same_over_a_pipe() {
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_editor_mcp"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("the server binary is built for this test");

    let lines = [
        json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"editor_state","arguments":{}}}),
    ]
    .map(|value| value.to_string())
    .join("\n");

    let mut stdin = child.stdin.take().expect("a pipe to the server");
    let writer = std::thread::spawn(move || {
        stdin
            .write_all(lines.as_bytes())
            .expect("the write reaches the server");
        stdin.write_all(b"\n").expect("the last line is terminated");
        drop(stdin);
    });

    let mut stdout = child.stdout.take().expect("a pipe back");
    let reader = std::thread::spawn(move || {
        let mut seen = String::new();
        let mut lines = BufReader::new(&mut stdout).lines();
        while let Some(Ok(line)) = lines.next() {
            seen.push_str(&line);
            seen.push('\n');
        }
        seen
    });

    writer.join().expect("the writer finished");
    let transcript = reader.join().expect("the reader drained the pipe");
    child.wait().expect("the server exits when stdin closes");

    let replies: Vec<Value> = transcript
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    assert_eq!(
        replies.len(),
        3,
        "three requests and one notification should be three replies, got: {transcript}"
    );
    assert_eq!(replies[0]["id"], json!(0));
    assert_eq!(
        replies[1]["result"]["tools"]
            .as_array()
            .expect("a tool list")
            .len(),
        TOOL_NAMES.len()
    );
    assert_eq!(replies[2]["id"], json!(2));
    assert!(
        replies[2]["result"]["content"].is_array(),
        "the state did not come back through the pipe: {transcript}"
    );
}

/// A path in the temp dir, unique per name so tests in one process cannot read each other's fixture.
fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("merkle_editor_mcp_tests_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("the fixture dir is writable");
    dir.join(name)
}

/// Every part of a parts list as one comparable string, so a test can say "nothing else in the tree
/// moved" without naming the thirty-odd nodes the pig happens to hold.
fn fingerprints(parts: &Value) -> Vec<String> {
    let field = |value: &Value| {
        value
            .as_str()
            .unwrap_or_else(|| panic!("a part field is not a string: {value}"))
            .to_string()
    };
    parts
        .as_array()
        .expect("a parts array")
        .iter()
        .map(|part| {
            format!(
                "{}|{}|{}",
                field(&part["route"]),
                field(&part["kind"]),
                field(&part["summary"])
            )
        })
        .collect()
}

/// The kinds the authored grammar ends in, and `add_part` can author from nothing.
const LEAF_KINDS: &[&str] = &["Sphere", "RoundedBox", "Plane"];

fn route_of(part: &Value) -> &str {
    part["route"].as_str().expect("every part prints a route")
}

/// The route of a leaf whose parent holds two leaves, so deleting it collapses the parent without
/// renaming any subtree: the pig's own `SmoothUnion` pairs are exactly that.
fn sibling_leaf_pair(parts: &[Value]) -> (String, String, String) {
    let is_leaf = |part: &Value| {
        part["kind"]
            .as_str()
            .is_some_and(|kind| LEAF_KINDS.contains(&kind))
    };
    for part in parts {
        let route = route_of(part);
        if !route.contains('.') || !is_leaf(part) {
            continue;
        }
        let parent = route.rsplit_once('.').expect("a dotted route").0;
        let mut others: Vec<&Value> = parts
            .iter()
            // A sibling sits exactly one step below the parent; a deeper descendant of it does not.
            .filter(|other| {
                route_of(other) != route
                    && route_of(other)
                        .rsplit_once('.')
                        .is_some_and(|(her_parent, _)| her_parent == parent)
            })
            .filter(|other| is_leaf(other))
            .collect();
        if others.len() == 1 {
            let sibling = others.pop().expect("one sibling");
            return (
                route.to_string(),
                parent.to_string(),
                sibling["route"]
                    .as_str()
                    .expect("a sibling route")
                    .to_string(),
            );
        }
    }
    panic!(
        "no leaf in the tree has a leaf beside it, so collapsing a parent is not being exercised"
    );
}

#[test]
fn add_part_wraps_the_node_a_route_names_and_the_saved_file_reads_the_new_tree() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let route = before["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .find(|part| part["kind"] == json!("Sphere") && part["depth"].as_u64() == Some(2))
        .expect("the pig has a sphere two levels down")["route"]
        .as_str()
        .expect("a route")
        .to_string();

    let added = call(
        &mut state,
        "add_part",
        json!({
            "route": route,
            "combine": "Union",
            "shape": { "kind": "Sphere", "r": 0.4, "centre": [0.9, -0.2, 0.0] },
        }),
    );
    let leaf_route = format!("{route}.b");
    assert_eq!(
        added["at"],
        json!(route),
        "the wrapper is not where the route was"
    );
    assert_eq!(added["into"], json!("Union"));
    assert_eq!(
        added["kept"]["slot"],
        json!("a"),
        "with no keep passed, the node already in the tree should hold the first slot"
    );
    assert_eq!(added["added"]["route"], json!(leaf_route));
    assert_eq!(added["added"]["kind"], json!("Sphere"));
    assert_eq!(
        added["added"]["summary"],
        json!("r 0.40 at [0.90, -0.20, 0.00]")
    );
    assert_eq!(
        added["node_count"].as_u64().expect("a count"),
        before["node_count"].as_u64().expect("a count") + 2,
        "an add is one new leaf and one new combinator over the node it wrapped"
    );
    assert_eq!(
        added["selected"],
        json!(route),
        "the editor should be left pointing at the combinator just built, so a k is one call away"
    );
    assert_eq!(added["state"]["dirty"], json!(true));
    assert_ne!(
        added["preview_signature"], before["preview_signature"],
        "a new node left the preview key alone, so the viewport keeps showing the frame from before it"
    );

    let parts = added["state"]["parts"].as_array().expect("parts");
    let wrapper = parts
        .iter()
        .find(|part| part["route"] == json!(route))
        .expect("the wrapper the answer names is not in the parts list");
    assert_eq!(wrapper["kind"], json!("Union"));
    assert!(
        wrapper["parameters"]
            .as_array()
            .expect("a Union publishes its (empty) fields")
            .is_empty(),
        "a Union claims a parameter of its own, which is a set_part call away from nowhere"
    );
    assert!(parts.iter().any(|part| part["route"] == json!(leaf_route)));

    // And the file an author compiles from reads back as the tree the tool said it built.
    let path = temp_path("add_part_round_trip.ron");
    call(
        &mut state,
        "save_entity",
        json!({ "path": path.to_string_lossy() }),
    );
    let mut reloaded = fresh();
    call(
        &mut reloaded,
        "load_entity",
        json!({ "path": path.to_string_lossy() }),
    );
    let again = call(&mut reloaded, "editor_state", json!({}));
    assert_eq!(again["node_count"], added["node_count"]);
    assert_eq!(
        fingerprints(&again["parts"]),
        fingerprints(&added["state"]["parts"]),
        "the saved file reads back as a different tree than the one add_part reported"
    );
}

#[test]
fn keep_names_which_slot_the_existing_node_stays_in_and_a_subtract_cares_which_it_is() {
    // No route at all: the add lands on the node the editor holds, which on a fresh state is the root.
    let mut state = fresh();
    let before = call(&mut state, "editor_state", json!({}));
    let root_line = before["parts"]
        .as_array()
        .expect("a fresh state lists its root")
        .iter()
        .find(|part| part["route"] == json!("root"))
        .expect("the root is listed")
        .clone();
    let added = call(
        &mut state,
        "add_part",
        json!({
            "combine": "Subtract",
            "keep": "cutter",
            "shape": { "kind": "RoundedBox", "half": [1.5, 1.5, 1.5], "r": 0.0 },
        }),
    );
    assert_eq!(added["at"], json!("root"));
    assert_eq!(added["kept"]["slot"], json!("cutter"));
    assert_eq!(added["kept"]["route"], json!("root.cutter"));
    assert_eq!(
        added["kept"]["kind"], root_line["kind"],
        "the kept report names a different node than the one the route list held"
    );
    assert_eq!(
        added["kept"]["summary"], root_line["summary"],
        "the node that was the whole entity should be reported unchanged, in the slot `keep` names"
    );
    assert_eq!(added["added"]["slot"], json!("kept"));
    assert_eq!(added["added"]["route"], json!("root.kept"));
    assert_eq!(added["added"]["kind"], json!("RoundedBox"));

    let (message, refused) = try_call(
        &mut state,
        "add_part",
        json!({
            "combine": "Subtract",
            "keep": "hole",
            "shape": { "kind": "RoundedBox", "half": [1.0, 1.0, 1.0], "r": 0.0 },
        }),
    );
    assert!(
        refused,
        "a slot a Subtract does not have was accepted: {message}"
    );
    assert!(
        message.contains("\"kept\"") && message.contains("\"cutter\""),
        "the refusal for keep:\"hole\" does not name the two slots that exist: {message}"
    );
    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        fingerprints(&after["parts"]),
        fingerprints(&added["state"]["parts"]),
        "a refused add changed the tree"
    );
}

#[test]
fn a_leaf_authored_from_nothing_is_refused_until_it_is_complete_and_in_range() {
    let mut state = fresh();
    let before = call(&mut state, "editor_state", json!({}));
    let sphere = json!({ "kind": "Sphere", "r": 0.5, "centre": [0.0, 0.0, 0.0] });

    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "no leaf to add",
            json!({ "combine": "Union" }),
            "needs the node",
        ),
        (
            "a kind that needs operands first",
            json!({ "combine": "Union", "shape": { "kind": "Union" } }),
            "cannot be authored from nothing",
        ),
        (
            "a field the kind takes but the caller omitted",
            json!({ "combine": "Union", "shape": { "kind": "Sphere", "r": 0.5 } }),
            "\"centre\" is missing",
        ),
        (
            "a field nobody asked for",
            json!({ "combine": "Union", "shape": { "kind": "Sphere", "r": 0.5, "centre": [0.0, 0.0, 0.0], "radious": 0.5 } }),
            "radious",
        ),
        (
            "a radius past the range the slider offers",
            json!({ "combine": "Union", "shape": { "kind": "Sphere", "r": 9.0, "centre": [0.0, 0.0, 0.0] } }),
            "0.01..=5",
        ),
        (
            "a vector with two lanes",
            json!({ "combine": "Union", "shape": { "kind": "Sphere", "r": 0.5, "centre": [0.0, 0.0] } }),
            "3 numbers",
        ),
        (
            "a blend with no blend width",
            json!({ "combine": "SmoothUnion", "shape": sphere }),
            "needs k",
        ),
        (
            "a blend width on a combinator that has none",
            json!({ "combine": "Union", "k": 0.4, "shape": sphere }),
            "carries no k",
        ),
        (
            "a combinator this grammar does not have",
            json!({ "combine": "Translate", "shape": sphere }),
            "add_part joins with",
        ),
    ];
    for (what, arguments, expected) in cases {
        let (message, refused) = try_call(&mut state, "add_part", arguments);
        assert!(refused, "{what} was accepted: {message}");
        assert!(
            message.contains(expected),
            "the refusal for {what} says {message:?}, which does not mention {expected:?}"
        );
    }

    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(after["node_count"], before["node_count"]);
    assert_eq!(
        after["preview_signature"], before["preview_signature"],
        "nine refused adds moved the preview key"
    );
    assert_eq!(after["dirty"], json!(false));
}

#[test]
fn a_tree_the_compiler_would_refuse_is_refused_before_it_lands() {
    let mut state = fresh();
    let mut grown = 0;
    let mut refusal = String::new();
    for _ in 0..64 {
        let before = call(&mut state, "editor_state", json!({}));
        // Not a `Union`: `assetc::lower` flattens a union chain and rebuilds it balanced before it
        // measures depth, so nesting one cannot reach the cap however deep the file goes. A `Subtract`'s
        // operand order is its meaning, so what the author wrote is what the compiler counts.
        let (message, refused) = try_call(
            &mut state,
            "add_part",
            json!({
                "route": "root",
                "combine": "Subtract",
                "keep": "kept",
                "shape": { "kind": "Sphere", "r": 0.05, "centre": [5.0, 5.0, 5.0] },
            }),
        );
        if !refused {
            grown += 1;
            continue;
        }
        refusal = message;
        let after = call(&mut state, "editor_state", json!({}));
        assert_eq!(
            fingerprints(&after["parts"]),
            fingerprints(&before["parts"]),
            "the add that hit the depth cap still rewrote the tree"
        );
        assert_eq!(after["preview_signature"], before["preview_signature"]);
        break;
    }
    assert!(
        grown > 1,
        "the tree never grew, so this test cannot see the cap: {refusal}"
    );
    assert!(
        !refusal.is_empty(),
        "wrapping the root {grown} times never hit the grammar's depth cap, so the compiler's own \
         refusal is not being exercised"
    );
    assert!(
        refusal.contains("cannot be compiled"),
        "the refusal does not say the tree cannot be compiled: {refusal}"
    );
    assert!(
        refusal.contains("levels deep") && refusal.contains("cap"),
        "the refusal does not quote the compiler's depth words: {refusal}"
    );
}

#[test]
fn delete_part_takes_the_node_and_the_combinator_that_held_it() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let parts = before["parts"].as_array().expect("parts").clone();
    let (victim, parent, sibling) = sibling_leaf_pair(&parts);
    let listed = |route: &str| -> Value {
        parts
            .iter()
            .find(|part| part["route"] == json!(route))
            .unwrap_or_else(|| panic!("{route} is not in the parts list the pair came from"))
            .clone()
    };
    let (victim_line, sibling_line) = (listed(&victim), listed(&sibling));

    let deleted = call(&mut state, "delete_part", json!({ "route": victim }));
    assert_eq!(deleted["deleted"]["route"], json!(victim));
    assert_eq!(deleted["deleted"]["kind"], victim_line["kind"]);
    assert_eq!(deleted["deleted"]["summary"], victim_line["summary"]);
    assert_eq!(
        deleted["node_count"].as_u64().expect("a count"),
        before["node_count"].as_u64().expect("a count") - 2,
        "deleting a leaf takes the leaf and the combinator that held it, and nothing else"
    );
    assert_eq!(deleted["its_parent"]["route"], json!(parent));
    assert_eq!(deleted["its_parent"]["replaced_by"], sibling_line["kind"]);
    assert_eq!(
        deleted["its_parent"]["summary"], sibling_line["summary"],
        "the operand beside the deleted one should have taken its parent's place unchanged"
    );
    assert_eq!(
        deleted["selected"],
        json!("root"),
        "every path below a collapse means something else now, so the selection has to go back"
    );
    assert_eq!(deleted["state"]["dirty"], json!(true));
    assert_ne!(
        deleted["preview_signature"], before["preview_signature"],
        "a node was removed and the preview key did not move"
    );

    // Everything above and around the collapse is still the node it was, route for route.
    let mut kept_before = fingerprints(&before["parts"]);
    kept_before.retain(|line| {
        let route = line.split('|').next().expect("the route prefix");
        route != victim && route != parent && route != sibling
    });
    let mut kept_after = fingerprints(&deleted["state"]["parts"]);
    kept_after.retain(|line| line.split('|').next().expect("the route prefix") != parent);
    kept_before.sort();
    kept_after.sort();
    assert_eq!(
        kept_after, kept_before,
        "deleting {victim} moved parts of the tree the call never named"
    );

    let path = temp_path("delete_part_round_trip.ron");
    call(
        &mut state,
        "save_entity",
        json!({ "path": path.to_string_lossy() }),
    );
    let mut reloaded = fresh();
    call(
        &mut reloaded,
        "load_entity",
        json!({ "path": path.to_string_lossy() }),
    );
    let again = call(&mut reloaded, "editor_state", json!({}));
    assert_eq!(again["node_count"], deleted["node_count"]);
    assert_eq!(
        fingerprints(&again["parts"]),
        fingerprints(&deleted["state"]["parts"]),
        "the saved file reads back as a different tree than the one delete_part reported"
    );
}

#[test]
fn a_node_that_cannot_be_deleted_says_why_and_leaves_the_tree_alone() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let mirror_child = before["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .find(|part| part["kind"] == json!("Mirror"))
        .map(|part| format!("{}.child", part["route"].as_str().expect("a route")))
        .expect("the shipped pig is symmetric, so a Mirror is in the list");

    let cases: Vec<(&str, Value, &str)> = vec![
        ("root", json!({ "route": "root" }), "the whole entity"),
        ("no route", json!({}), "route is required"),
        (
            "a blank route",
            json!({ "route": "  " }),
            "route is required",
        ),
        (
            "a child the tree does not have",
            json!({ "route": "root.nope" }),
            "has no child",
        ),
        (
            "a one-child wrapper's only child",
            json!({ "route": mirror_child }),
            "only child",
        ),
    ];
    for (what, arguments, expected) in cases {
        let (message, refused) = try_call(&mut state, "delete_part", arguments);
        assert!(refused, "{what} was deleted: {message}");
        assert!(
            message.contains(expected),
            "the refusal for {what} says {message:?}, which does not mention {expected:?}"
        );
    }
    let refusal = try_call(&mut state, "delete_part", json!({ "route": mirror_child })).0;
    assert!(
        refusal.contains("Mirror"),
        "the refusal names the wrapper's child but not the wrapper: {refusal}"
    );

    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        fingerprints(&after["parts"]),
        fingerprints(&before["parts"]),
        "five refused deletes changed the tree"
    );
    assert_eq!(after["preview_signature"], before["preview_signature"]);
    assert_eq!(after["dirty"], json!(false));
    assert_eq!(after["selected"], before["selected"]);
}

#[test]
fn a_part_added_is_in_the_frame_render_view_draws() {
    let mut state = fresh();
    // Two instrument choices, both so the count means what it says. The named views sit close enough to
    // the default sphere that its silhouette is the whole frame, and the stage is an infinite plane that
    // covers the rest, so either would report "all surface" before the add changed a pixel — hence a bare
    // sky and the front view's angles pulled back to a distance that leaves the frame mostly sky.
    let view = json!({
        "yaw": core::f32::consts::PI, "pitch": 0.12, "distance": 6.0,
        "width": 96, "height": 72, "stage": false,
    });
    let before = call(&mut state, "render_view", view.clone());
    let added = call(
        &mut state,
        "add_part",
        json!({
            "combine": "Union",
            "shape": { "kind": "Sphere", "r": 1.2, "centre": [0.0, 0.0, 0.0] },
        }),
    );
    let after = call(&mut state, "render_view", view);

    assert_eq!(
        after["preview_signature"], added["preview_signature"],
        "the frame rendered is not the state the add left behind"
    );
    assert_ne!(
        after["path"], before["path"],
        "two different states wrote the same PNG, so a reader cannot tell which frame they are looking at"
    );
    assert_eq!(
        after["undrawn_pixels"],
        json!(0),
        "the march refused to draw the frame the add produced"
    );
    let (before_surface, total) = (
        before["surface_pixels"].as_u64().expect("a count"),
        before["total_pixels"].as_u64().expect("a count"),
    );
    assert!(
        before_surface < total,
        "the frame before the add was already all surface, so this instrument cannot show a gain"
    );
    assert!(
        after["surface_pixels"].as_u64().expect("a count") > before_surface,
        "a sphere of r 1.2 joined the tree and the picture did not gain a pixel: {} -> {}",
        before_surface,
        after["surface_pixels"],
    );
}

/// The one-child gesture: the node keeps its route and gains a wrapper above it.
#[test]
fn add_part_wraps_a_node_in_a_one_child_wrapper_and_reports_both_halves() {
    let mut state = fresh();
    let before = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        before["node_count"],
        json!(1),
        "a fresh editor holds one sphere, so a wrapper is the only thing this can add"
    );
    let sphere_line = before["parts"][0].clone();
    assert_eq!(sphere_line["route"], json!("root"));

    let wrapped = call(
        &mut state,
        "add_part",
        json!({ "combine": "Mirror", "axis": 1 }),
    );
    assert_eq!(wrapped["into"], json!("Mirror"));
    assert_eq!(wrapped["at"], json!("root"));
    assert_eq!(wrapped["selected"], json!("root"));
    assert_eq!(wrapped["wrapper"]["route"], json!("root"));
    assert_eq!(wrapped["wrapper"]["kind"], json!("Mirror"));
    assert_eq!(
        wrapped["wrapper"]["summary"],
        json!("axis 1"),
        "the reply reports a Mirror that does not carry the axis it was handed"
    );
    assert_eq!(wrapped["wrapped"]["slot"], json!("child"));
    assert_eq!(wrapped["wrapped"]["route"], json!("root.child"));
    assert_eq!(wrapped["wrapped"]["kind"], sphere_line["kind"]);
    assert_eq!(wrapped["wrapped"]["summary"], sphere_line["summary"]);
    assert_eq!(
        wrapped["node_count"],
        json!(2),
        "a wrapper adds one node; two is what joining a leaf costs"
    );
    assert_eq!(wrapped["state"]["dirty"], json!(true));
    assert_ne!(
        wrapped["preview_signature"], before["preview_signature"],
        "a node joined the tree and the preview key did not move"
    );

    // The parts list is where a caller reads the new route from, so it has to carry it.
    let routes: Vec<&str> = wrapped["state"]["parts"]
        .as_array()
        .expect("parts")
        .iter()
        .map(|part| part["route"].as_str().expect("a route"))
        .collect();
    assert!(
        routes.contains(&"root.child"),
        "the outline never printed the route the reply names: {routes:?}"
    );

    // A route through the wrapper reaches the node inside it, and that node is still editable.
    let selected = call(&mut state, "select_part", json!({ "route": "root.child" }));
    assert_eq!(selected["kind"], json!("Sphere"));
    let edited = call(
        &mut state,
        "set_part",
        json!({ "route": "root.child", "parameter": "r", "value": 0.5 }),
    );
    assert_eq!(edited["changed"], json!(true));
    assert_eq!(edited["state"]["node_count"], json!(2));

    // And a slot a Mirror does not have is refused with the one it does.
    let (message, refused) = try_call(&mut state, "select_part", json!({ "route": "root.a" }));
    assert!(refused, "a Mirror was given an `a` operand: {message}");
    assert!(
        message.contains("\"child\""),
        "the refusal for root.a never names the slot a Mirror does have: {message}"
    );

    let path = temp_path("wrap_mirror_round_trip.ron");
    call(
        &mut state,
        "save_entity",
        json!({ "path": path.to_string_lossy() }),
    );
    let mut reloaded = fresh();
    call(
        &mut reloaded,
        "load_entity",
        json!({ "path": path.to_string_lossy() }),
    );
    let again = call(&mut reloaded, "editor_state", json!({}));
    assert_eq!(again["node_count"], json!(2));
    assert_eq!(
        fingerprints(&again["parts"]),
        fingerprints(&edited["state"]["parts"]),
        "the saved file reads back as a different tree than the wrapped one the pipe reported"
    );
}

/// A wrapper's own field comes from the same published table `set_part` writes, and the two arguments that
/// only mean something to a two-operand combinator are refused rather than dropped.
#[test]
fn a_wrapper_takes_its_own_field_from_the_same_table_and_refuses_the_two_operand_arguments() {
    let mut state = fresh();
    let before = call(&mut state, "editor_state", json!({}));
    let sphere = json!({ "kind": "Sphere", "r": 0.4, "centre": [0.0, 0.0, 0.0] });
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "a Mirror with no axis",
            json!({ "combine": "Mirror" }),
            "Mirror takes \"axis\"",
        ),
        (
            "a Repeat with no period",
            json!({ "combine": "Repeat" }),
            "Repeat takes \"period\"",
        ),
        (
            "an axis past the panel's last choice",
            json!({ "combine": "Mirror", "axis": 3 }),
            "0..=2",
        ),
        (
            "an axis that is not a whole choice",
            json!({ "combine": "Mirror", "axis": 1.5 }),
            "whole choice",
        ),
        (
            "an axis spelled as text",
            json!({ "combine": "Mirror", "axis": "x" }),
            "axis must be a number",
        ),
        (
            "a period under the smallest the table offers",
            json!({ "combine": "Repeat", "period": 0.01 }),
            "0.05..=10",
        ),
        (
            "a wrapper handed an operand",
            json!({ "combine": "Mirror", "axis": 0, "shape": sphere }),
            "takes no shape",
        ),
        (
            "a wrapper asked which slot to keep",
            json!({ "combine": "Repeat", "period": 1.0, "keep": "a" }),
            "no slot to keep",
        ),
        (
            "a blend width on a wrapper",
            json!({ "combine": "Mirror", "axis": 2, "k": 0.3 }),
            "carries no k",
        ),
    ];
    for (what, arguments, expected) in cases {
        let (message, refused) = try_call(&mut state, "add_part", arguments);
        assert!(refused, "{what} was wrapped: {message}");
        assert!(
            message.contains(expected),
            "the refusal for {what} says {message:?}, which does not mention {expected:?}"
        );
    }

    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        fingerprints(&after["parts"]),
        fingerprints(&before["parts"]),
        "nine refused wraps changed the tree"
    );
    assert_eq!(after["preview_signature"], before["preview_signature"]);
    assert_eq!(after["dirty"], json!(false));
}

/// A `Repeat` folds the *query point* into its cell, so what tiles is the child's intersection with that
/// cell — not the child. A part authored away from the origin therefore survives the wrap in the tree, in
/// the compiled bytes and in `node_count`, while contributing no pixel at all. `wrap_part` reports the box
/// it measured and says which of the three cases it is in, and the cell it quotes is the one the `period`
/// this call was handed actually opens.
#[test]
fn a_repeat_that_drops_the_part_it_wraps_outside_its_cell_says_so() {
    // One part, three cells: wholly outside, straddling, wholly inside. The box is the same all three
    // times, so what moves is only the notice - and the numbers in it.
    for (period, expected) in [
        (1.0f32, Some("none of which lies in it")),
        (4.0, Some("only the piece inside the cell is tiled")),
        (5.0, None),
    ] {
        let mut state = fresh();
        call(
            &mut state,
            "set_part",
            json!({ "parameter": "r", "value": 0.25 }),
        );
        call(
            &mut state,
            "set_part",
            json!({ "parameter": "centre", "value": [2.0, 0.0, 0.0] }),
        );
        let wrapped = call(
            &mut state,
            "add_part",
            json!({ "combine": "Repeat", "period": period }),
        );
        // The part's own box, derived from the two fields just set rather than typed here beside them.
        assert_eq!(
            wrapped["wrapped"]["extent"],
            json!([[1.75, -0.25, -0.25], [2.25, 0.25, 0.25]]),
            "the reply reports a box that is not the sphere this call wrapped"
        );
        let half = period * 0.5;
        let cell = format!("x {}..={}", -half, half);
        match expected {
            Some(phrase) => {
                let notice = wrapped["wrapped"]["notice"].as_str().unwrap_or_else(|| {
                    panic!("a {period} cell drops part of the part and says nothing")
                });
                assert!(
                    notice.contains(phrase),
                    "the notice for a {period} cell says {notice:?}, which does not mention {phrase:?}"
                );
                assert!(
                    notice.contains(&cell) && notice.contains("1.75..=2.25"),
                    "the notice for a {period} cell quotes neither the cell it opens ({cell}) nor the \
                     box it dropped: {notice:?}"
                );
            }
            None => assert_eq!(
                wrapped["wrapped"]["notice"],
                json!(null),
                "a cell wide enough to hold the whole part still warned: {}",
                wrapped["wrapped"]["notice"]
            ),
        }
    }

    // A part with no finite box is neither clean nor dropped: the pipe says it cannot tell.
    let mut state = fresh();
    call(
        &mut state,
        "add_part",
        json!({
            "combine": "Union",
            "shape": { "kind": "Plane", "normal": [0.0, 1.0, 0.0], "offset": 0.0 },
        }),
    );
    let wrapped = call(
        &mut state,
        "add_part",
        json!({ "combine": "Repeat", "period": 1.0, "route": "root.b" }),
    );
    assert_eq!(wrapped["wrapped"]["kind"], json!("Plane"));
    assert_eq!(
        wrapped["wrapped"]["extent"],
        json!(null),
        "a half-space was given a finite box"
    );
    assert!(
        wrapped["wrapped"]["notice"]
            .as_str()
            .unwrap_or_default()
            .contains("no finite extent"),
        "the wrap of a plane reported neither a box nor that it cannot state one: {}",
        wrapped["wrapped"]["notice"]
    );
}

/// The same hazard on the other wrapper: a `Mirror` evaluates its child at `|p[axis]|`, so a part sitting
/// wholly on the negative side of the plane is consulted nowhere and draws nothing, and a part crossing the
/// plane loses its authored negative half to the reflection. Both name the axis the call was handed.
#[test]
fn a_mirror_that_throws_its_child_to_the_wrong_side_of_its_plane_says_so() {
    let cases = [
        (0u8, [2.0, 0.0, 0.0], None),
        (0, [-2.0, 0.0, 0.0], Some("wholly on the negative side")),
        (1, [0.0, -2.0, 0.0], Some("wholly on the negative side")),
        (2, [0.0, 0.0, -2.0], Some("wholly on the negative side")),
    ];
    for (axis, centre, expected) in cases {
        let mut state = fresh();
        call(
            &mut state,
            "set_part",
            json!({ "parameter": "centre", "value": centre }),
        );
        let wrapped = call(
            &mut state,
            "add_part",
            json!({ "combine": "Mirror", "axis": axis }),
        );
        let plane = ["x", "y", "z"][axis as usize];
        let notice = wrapped["wrapped"]["notice"].clone();
        match expected {
            Some(phrase) => {
                let text = notice.as_str().unwrap_or_else(|| {
                    panic!("a part at {centre:?} mirrored about {plane} says nothing")
                });
                assert!(
                    text.contains(phrase) && text.contains(&format!("{plane} >= 0")),
                    "the notice for a {plane} mirror of {centre:?} says {text:?}: it names neither \
                     {phrase:?} nor the side a Mirror consults"
                );
            }
            None => assert_eq!(
                notice,
                json!(null),
                "a part already on the side a {plane} mirror reads still warned: {notice}"
            ),
        }
    }

    // A sphere straddling the plane: the authored half below it is not mirrored in addition, it is
    // replaced by the reflection of the half above.
    let mut state = fresh();
    call(
        &mut state,
        "set_part",
        json!({ "parameter": "r", "value": 2.5 }),
    );
    call(
        &mut state,
        "set_part",
        json!({ "parameter": "centre", "value": [-1.0, 0.0, 0.0] }),
    );
    let wrapped = call(
        &mut state,
        "add_part",
        json!({ "combine": "Mirror", "axis": 0 }),
    );
    assert_eq!(
        wrapped["wrapped"]["extent"],
        json!([[-3.5, -2.5, -2.5], [1.5, 2.5, 2.5]])
    );
    assert!(
        wrapped["wrapped"]["notice"]
            .as_str()
            .unwrap_or_default()
            .contains("crosses the plane"),
        "a Mirror of a part that straddles its plane did not say the authored far half is replaced: {}",
        wrapped["wrapped"]["notice"]
    );
}

/// The notice is a claim about pixels, so it is checked against the march rather than only against the
/// arithmetic: the same part, one call apart, with and without the wrapper that discards it. `stage: false`
/// because the preview floor is geometry the notice is not talking about.
#[test]
fn what_a_wrapper_discards_is_exactly_what_the_marched_frame_loses() {
    let frame = json!({ "view": "front", "width": 64, "height": 48, "stage": false });
    let mut state = fresh();
    call(
        &mut state,
        "set_part",
        json!({ "parameter": "r", "value": 0.4 }),
    );
    call(
        &mut state,
        "set_part",
        json!({ "parameter": "centre", "value": [0.0, -0.6, 0.0] }),
    );
    let plain = call(&mut state, "render_view", frame.clone());
    assert!(
        plain["surface_pixels"].as_u64().expect("a count") > 0,
        "the part draws nothing even before it is wrapped, so this compares a wrap with nothing at all: \
         {plain}"
    );

    let wrapped = call(
        &mut state,
        "add_part",
        json!({ "combine": "Mirror", "axis": 1 }),
    );
    assert!(
        wrapped["wrapped"]["notice"]
            .as_str()
            .unwrap_or_default()
            .contains("draws nothing"),
        "the notice for a part below the mirror's plane says: {}",
        wrapped["wrapped"]["notice"]
    );
    let mirrored = call(&mut state, "render_view", frame.clone());
    assert_eq!(
        mirrored["surface_pixels"],
        json!(0),
        "the notice promised this wrap draws nothing and the march found {} pixels of it",
        mirrored["surface_pixels"]
    );

    // The same part on the side a Mirror reads: no notice, and the frame is back.
    let mut state = fresh();
    call(
        &mut state,
        "set_part",
        json!({ "parameter": "r", "value": 0.4 }),
    );
    call(
        &mut state,
        "set_part",
        json!({ "parameter": "centre", "value": [0.0, 0.6, 0.0] }),
    );
    let wrapped = call(
        &mut state,
        "add_part",
        json!({ "combine": "Mirror", "axis": 1 }),
    );
    assert_eq!(wrapped["wrapped"]["notice"], json!(null));
    let mirrored = call(&mut state, "render_view", frame);
    assert!(
        mirrored["surface_pixels"].as_u64().expect("a count") > 0,
        "a wrap the notice called clean drew nothing, so the two halves of this pair say opposite things \
         about the same tree: {mirrored}"
    );
}

/// Every wrapper the server advertises is built here, with its field named and its value written out in
/// this test rather than read back from `params` — so a kind added to the list without a row here, or one
/// that stopped carrying the field this call needs, fails instead of going unexercised.
#[test]
fn every_wrapper_the_pipe_advertises_it_builds_with_the_field_it_was_given() {
    let builds: &[(&str, &str, f64, &str)] = &[
        ("Mirror", "axis", 2.0, "axis 2"),
        ("Repeat", "period", 0.75, "period 0.75"),
    ];
    for name in mcp::WRAPPERS {
        assert!(
            builds.iter().any(|(kind, ..)| kind == name),
            "{name} is in WRAPPERS but has no row here, so nothing proves the pipe can build it",
        );
    }

    let mut state = fresh();
    for (index, (kind, field, value, expect)) in builds.iter().enumerate() {
        let mut arguments = json!({ "combine": kind, "route": "root" });
        arguments
            .as_object_mut()
            .expect("an object")
            .insert((*field).to_string(), json!(value));
        let wrapped = call(&mut state, "add_part", arguments);
        assert_eq!(wrapped["wrapper"]["kind"], json!(kind));
        assert_eq!(
            wrapped["wrapper"]["summary"],
            json!(expect),
            "{kind} was built without the {field} it was handed"
        );
        assert_eq!(wrapped["wrapped"]["route"], json!("root.child"));
        assert_eq!(
            wrapped["node_count"],
            json!(2 + index as u64),
            "each wrapper adds exactly one node, nested above the last"
        );
    }
    // The nesting is the point of the loop: a `Repeat` over a `Mirror` is a tree the compiler still takes.
    let final_state = call(&mut state, "editor_state", json!({}));
    assert_eq!(final_state["node_count"], json!(builds.len() as u64 + 1));
    assert_eq!(final_state["dirty"], json!(true));
}

/// Deleting a root that holds one node unwraps it; a root that holds two, or nothing, still refuses.
#[test]
fn deleting_a_root_wrapper_unwraps_the_tree_and_a_root_that_holds_two_refuses() {
    let mut state = fresh();
    let sphere = call(&mut state, "editor_state", json!({}));

    let (message, refused) = try_call(&mut state, "delete_part", json!({ "route": "root" }));
    assert!(refused, "a leaf root was deleted: {message}");
    assert!(
        message.contains("the whole entity"),
        "the refusal for a leaf root says {message:?}"
    );

    let wrapped = call(
        &mut state,
        "add_part",
        json!({ "combine": "Mirror", "axis": 0 }),
    );
    let deleted = call(&mut state, "delete_part", json!({ "route": "root" }));
    assert_eq!(deleted["deleted"]["route"], json!("root"));
    assert_eq!(deleted["deleted"]["kind"], json!("Mirror"));
    assert_eq!(
        deleted["deleted"]["summary"], wrapped["wrapper"]["summary"],
        "the reply reports deleting a wrapper it did not remove"
    );
    assert_eq!(
        deleted["its_parent"]["route"],
        json!("root"),
        "a root is its own holder, so the unwrap is reported at root"
    );
    assert_eq!(deleted["its_parent"]["replaced_by"], json!("Sphere"));
    assert_eq!(
        deleted["its_parent"]["summary"], wrapped["wrapped"]["summary"],
        "the node the wrapper held came back changed"
    );
    assert_eq!(deleted["node_count"], json!(1));
    assert_eq!(deleted["selected"], json!("root"));
    assert_eq!(
        deleted["preview_signature"],
        sphere["preview_signature"],
        "wrapping the root and deleting the wrapper is the identity, and the tree did not come back the \
         tree that went in"
    );
    assert_eq!(
        fingerprints(&deleted["state"]["parts"]),
        fingerprints(&sphere["parts"])
    );

    // A root with two operands cannot be deleted, because one operand of a `Union` is not a shape.
    call(
        &mut state,
        "add_part",
        json!({
            "combine": "Union",
            "shape": { "kind": "Sphere", "r": 0.5, "centre": [2.0, 0.0, 0.0] },
        }),
    );
    let (message, refused) = try_call(&mut state, "delete_part", json!({ "route": "root" }));
    assert!(refused, "a two-operand root was deleted: {message}");
    assert!(
        message.contains("the whole entity"),
        "the refusal for a Union root says {message:?}, which does not explain why one operand is not a \
         shape"
    );
}

/// The wrap has to reach the marcher, not just the tree: a mirror of an off-plane part is a wider animal.
#[test]
fn a_mirror_of_an_off_centre_part_doubles_what_the_frame_sees() {
    let mut state = fresh();
    // The default sphere sits on every mirror plane, so a wrap of it is the same silhouette however the
    // axis is chosen. Moving it off X first is what makes the frame a test rather than a tautology.
    call(
        &mut state,
        "set_part",
        json!({ "route": "root", "parameter": "centre", "value": [0.6, 0.0, 0.0] }),
    );
    let view = json!({
        "yaw": core::f32::consts::PI, "pitch": 0.12, "distance": 6.0,
        "width": 96, "height": 72, "stage": false,
    });
    let before = call(&mut state, "render_view", view.clone());
    let wrapped = call(
        &mut state,
        "add_part",
        json!({ "combine": "Mirror", "axis": 0 }),
    );
    let after = call(&mut state, "render_view", view);

    assert_eq!(
        after["preview_signature"], wrapped["preview_signature"],
        "the frame rendered is not the state the wrap left behind"
    );
    assert_ne!(after["path"], before["path"]);
    assert_eq!(
        after["undrawn_pixels"],
        json!(0),
        "the march refused to draw the mirrored frame"
    );
    let (one_limb, mirrored, total) = (
        before["surface_pixels"].as_u64().expect("a count"),
        after["surface_pixels"].as_u64().expect("a count"),
        before["total_pixels"].as_u64().expect("a count"),
    );
    assert!(
        one_limb < total,
        "the frame before the wrap was already all surface, so this instrument cannot show a gain"
    );
    assert!(
        mirrored > one_limb,
        "mirroring a sphere at x = 0.6 about X added its other half and the picture gained nothing: \
         {one_limb} -> {mirrored}"
    );
}

/// What a `Repeat` actually tiles, measured in the frame rather than in the tree.
///
/// The compiler folds the *query point* into `[-period/2, period/2)` on every axis before it evaluates the
/// child, so the copies are of whatever the child has inside that cell — not of the child wherever it
/// sits. A part authored wholly outside the cell therefore renders as nothing, which is a result an agent
/// can easily mistake for a failed wrap. The two halves below are what the pipe's own tests can say about
/// it: the blank frame proves the fold is live, and the frame that comes back after the cell widens proves
/// the blank was the cell's doing and not the wrapper's.
#[test]
fn a_repeat_tiles_its_fold_cell_so_a_part_authored_outside_it_renders_as_nothing() {
    let mut state = fresh();
    // The default sphere is r 1.0 at the origin, which every cell contains. Moving it to x = 1.5 puts its
    // whole span, 0.5 to 2.5, outside a period-0.4 cell.
    call(
        &mut state,
        "set_part",
        json!({ "route": "root", "parameter": "centre", "value": [1.5, 0.0, 0.0] }),
    );
    let view = json!({
        "yaw": core::f32::consts::PI, "pitch": 0.12, "distance": 6.0,
        "width": 96, "height": 72, "stage": false,
    });
    let alone = call(&mut state, "render_view", view.clone());
    assert!(
        alone["surface_pixels"].as_u64().expect("a count") > 0,
        "the sphere was already invisible before the wrap, so a blank frame afterwards would prove nothing"
    );

    let wrapped = call(
        &mut state,
        "add_part",
        json!({ "combine": "Repeat", "period": 0.4 }),
    );
    assert_eq!(wrapped["node_count"], json!(2));
    let tiled = call(&mut state, "render_view", view.clone());
    assert_eq!(
        tiled["preview_signature"], wrapped["preview_signature"],
        "the frame rendered is not the state the wrap left behind"
    );
    assert_eq!(
        tiled["undrawn_pixels"],
        json!(0),
        "the march refused to draw the tiled frame, which reads the same as an empty one"
    );
    assert_eq!(
        tiled["surface_pixels"],
        json!(0),
        "a period-0.4 cell spans -0.2 to 0.2 and the sphere spans 0.5 to 2.5, so the tiling has nothing \
         to copy and the frame should be sky: {} pixels found surface anyway",
        tiled["surface_pixels"]
    );

    // Widen the cell to hold the part and the same tree renders it, tiled. This is the check that the blank
    // frame above was the fold cell and not a wrapper the marcher ignores.
    let widened = call(
        &mut state,
        "set_part",
        json!({ "route": "root", "parameter": "period", "value": 4.0 }),
    );
    let retiled = call(&mut state, "render_view", view);
    assert_eq!(
        retiled["preview_signature"], widened["preview_signature"],
        "the frame rendered is not the state the period edit left behind"
    );
    assert_ne!(retiled["path"], tiled["path"]);
    assert!(
        retiled["surface_pixels"].as_u64().expect("a count") > 0,
        "a period-4 cell spans -2 to 2 and holds the sphere, so the tiling should draw it"
    );
}

/// A non-finite position has no JSON spelling, so `set_transform` cannot be handed one — but a
/// hand-authored `.ron` can, and `load_entity` takes the file as it is so an author can open a broken
/// one and fix it. The gate behind that door is `compile_entity`, which the viewport also routes its
/// march through, and until this unit it checked `rotation` and `scale` and not `position`.
#[test]
fn a_loaded_non_finite_position_is_refused_by_the_compiler_instead_of_marching_into_an_empty_scene()
{
    let broken = temp_path("non_finite_position.ron");
    std::fs::write(
        &broken,
        "EntityDef(\n    shape: Sphere(r: 1.0, centre: (0.0, 0.0, 0.0)),\n    position: (inf, 0.0, \
         0.0),\n    rotation: (0.0, 0.0, 0.0, 1.0),\n    scale: 1.0,\n)\n",
    )
    .expect("the fixture is writable");

    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": broken.to_string_lossy() }),
    );
    assert_eq!(
        state.entity_def.position,
        [f32::INFINITY, 0.0, 0.0],
        "load took the file as it is, or this test is not about the gate behind it"
    );

    let refusal = assetc::compile_entity(&state.entity_def)
        .expect_err("an infinite lane is not a distance an entity can be placed at");
    assert_eq!(refusal.error, assetc::LowerError::NonFiniteParameter);
    assert_eq!(
        refusal.location(),
        "position",
        "the refusal has to name the field an author can edit: {refusal}"
    );

    // The same gate feeds the preview, and the refusal is the point: an infinity placed through the
    // old clause marched to a frame of sky, which an author reads as "nothing in the scene today"
    // rather than "your file is broken". An undrawn frame says which.
    let frame = call(
        &mut state,
        "render_view",
        json!({ "view": "front", "width": 32, "height": 24 }),
    );
    assert_eq!(
        frame["undrawn_pixels"],
        json!(32 * 24),
        "the placement was marched instead of refused"
    );
    assert_eq!(frame["surface_pixels"], json!(0));

    // Repaired through a legal call, the file round-trips and the identical frame draws.
    let saved = temp_path("position_repaired.ron");
    call(
        &mut state,
        "set_transform",
        json!({ "position": [0.75, 0.0, 0.0] }),
    );
    call(
        &mut state,
        "save_entity",
        json!({ "path": saved.to_string_lossy() }),
    );
    let mut reread = fresh();
    call(
        &mut reread,
        "load_entity",
        json!({ "path": saved.to_string_lossy() }),
    );
    assetc::compile_entity(&reread.entity_def).expect("the repaired file compiles");
    let placed = call(
        &mut reread,
        "render_view",
        json!({ "view": "front", "width": 32, "height": 24 }),
    );
    assert_eq!(placed["undrawn_pixels"], json!(0));
    assert!(
        placed["surface_pixels"].as_u64().expect("a count") > 0,
        "the repaired file placed the subject and the frame still holds nothing"
    );

    let _ = std::fs::remove_file(broken);
    let _ = std::fs::remove_file(saved);
}

/// What one part holds, without the route: the route is the slot, and an exchange moves contents between
/// slots rather than moving the slots.
fn content_of(part: &Value) -> String {
    format!(
        "{}|{}",
        part["kind"].as_str().expect("a kind"),
        part["summary"].as_str().expect("a summary")
    )
}

fn part_at<'p>(parts: &'p [Value], route: &str) -> &'p Value {
    parts
        .iter()
        .find(|part| route_of(part) == route)
        .unwrap_or_else(|| panic!("the parts list does not hold {route}"))
}

/// The contents sitting below one route, sorted so a test can compare two subtrees without depending on
/// the order the outline walks them in.
fn contents_under(parts: &[Value], route: &str) -> Vec<String> {
    let prefix = format!("{route}.");
    let mut lines: Vec<String> = parts
        .iter()
        .filter(|part| route_of(part).starts_with(&prefix))
        .map(content_of)
        .collect();
    lines.sort();
    lines
}

/// A pair of leaf siblings holding different things, so an exchange of them is visible. Two identical
/// spheres would make every assertion below pass on a tool that moved nothing at all.
fn differing_sibling_leaf_pair(parts: &[Value]) -> (String, String) {
    let is_leaf = |part: &Value| {
        part["kind"]
            .as_str()
            .is_some_and(|kind| LEAF_KINDS.contains(&kind))
    };
    for part in parts {
        let route = route_of(part);
        if !route.contains('.') || !is_leaf(part) {
            continue;
        }
        let parent = route.rsplit_once('.').expect("a dotted route").0;
        let siblings = parts.iter().filter(|other| {
            let her = route_of(other);
            her != route
                && is_leaf(other)
                && her
                    .rsplit_once('.')
                    .is_some_and(|(his_parent, _)| his_parent == parent)
        });
        for sibling in siblings {
            if content_of(part) != content_of(sibling) {
                return (route.to_string(), route_of(sibling).to_string());
            }
        }
    }
    panic!(
        "no two leaf siblings in the tree differ, so exchanging a pair of them is not being run"
    );
}

#[test]
fn an_exchange_moves_both_ends_and_leaves_every_other_route_exactly_where_it_was() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let was_parts = before["parts"].as_array().expect("a list").clone();
    let (from, to) = differing_sibling_leaf_pair(&was_parts);

    let moved = call(&mut state, "move_part", json!({ "from": from, "to": to }));
    let now_parts = moved["state"]["parts"].as_array().expect("a list").clone();
    let ends = moved["exchanged"].as_array().expect("two ends");
    assert_eq!(ends.len(), 2, "an exchange reports both of its ends");

    // Each end says what it held and what it holds now, and the two answers are each other's.
    assert_eq!(ends[0]["called_with"], json!(from));
    assert_eq!(ends[0]["route"], json!(from));
    assert_eq!(ends[1]["called_with"], json!(to));
    for (end, route, other) in [(&ends[0], &from, &to), (&ends[1], &to, &from)] {
        assert_eq!(
            end["was"]["kind"],
            part_at(&was_parts, route)["kind"],
            "the reply says {route} held something else",
        );
        assert_eq!(end["was"]["summary"], part_at(&was_parts, route)["summary"]);
        assert_eq!(
            end["now"]["kind"],
            part_at(&was_parts, other)["kind"],
            "{route} did not take what {other} held",
        );
        assert_eq!(end["now"]["summary"], part_at(&was_parts, other)["summary"]);
        assert_eq!(end["children"], json!(0), "two leaves were exchanged");
    }
    assert_eq!(
        content_of(part_at(&now_parts, &from)),
        content_of(part_at(&was_parts, &to))
    );
    assert_eq!(
        content_of(part_at(&now_parts, &to)),
        content_of(part_at(&was_parts, &from))
    );

    // The selection followed the node the caller moved, which is now at `to`.
    assert_eq!(moved["selected"], json!(to));
    // Total, not destructive: the tree keeps its count.
    assert_eq!(moved["nodes_before"], before["node_count"]);
    assert_eq!(
        moved["node_count"], before["node_count"],
        "an exchange added or lost a node"
    );
    assert_eq!(
        contents_under(&now_parts, &to),
        contents_under(&was_parts, &from),
        "the moved subtree did not travel intact"
    );

    // Every route that is not one of the two ends, or below one of them, reads exactly as it did.
    let outside = |line: &String| {
        let route = line.split('|').next().expect("a route");
        route != from
            && route != to
            && !route.starts_with(&format!("{from}."))
            && !route.starts_with(&format!("{to}."))
    };
    let kept: Vec<String> = fingerprints(&before["parts"])
        .into_iter()
        .filter(outside)
        .collect();
    let still: Vec<String> = fingerprints(&moved["state"]["parts"])
        .into_iter()
        .filter(outside)
        .collect();
    assert_eq!(
        kept, still,
        "an exchange of {from} and {to} rewrote a route it was never given"
    );

    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        after["dirty"],
        json!(true),
        "a real move left the file clean"
    );
    assert_ne!(
        after["preview_signature"], before["preview_signature"],
        "the moved tree is the tree the state came with"
    );
}

#[test]
fn an_exchange_reads_back_out_of_the_file_it_saves() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let (from, to) = differing_sibling_leaf_pair(before["parts"].as_array().expect("a list"));
    let moved = call(&mut state, "move_part", json!({ "from": from, "to": to }));

    let path = temp_path("exchanged_pig.ron");
    call(
        &mut state,
        "save_entity",
        json!({ "path": path.to_string_lossy() }),
    );
    let mut reloaded = fresh();
    call(
        &mut reloaded,
        "load_entity",
        json!({ "path": path.to_string_lossy() }),
    );
    let again = call(&mut reloaded, "editor_state", json!({}));
    assert_eq!(again["dirty"], json!(false));
    assert_eq!(again["node_count"], moved["node_count"]);
    assert_eq!(
        fingerprints(&again["parts"]),
        fingerprints(&moved["state"]["parts"]),
        "the file reads back as a different tree than the exchange reported"
    );
    // And it is the animal the compiler accepts, not just the one the editor is holding.
    assetc::compile_entity(&reloaded.entity_def)
        .expect("the exchanged pig does not compile into an asset");

    let _ = std::fs::remove_file(path);
}

#[test]
fn the_same_pair_exchanged_twice_puts_the_tree_back() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let (from, to) = differing_sibling_leaf_pair(before["parts"].as_array().expect("a list"));

    let once = call(&mut state, "move_part", json!({ "from": from, "to": to }));
    assert_ne!(
        once["preview_signature"], before["preview_signature"],
        "the first exchange did not move anything, so the second cannot undo it"
    );
    let back = call(&mut state, "move_part", json!({ "from": to, "to": from }));

    assert_eq!(
        fingerprints(&back["state"]["parts"]),
        fingerprints(&before["parts"]),
        "exchanging {from} with {to} and then {to} with {from} left the tree changed"
    );
    assert_eq!(back["preview_signature"], before["preview_signature"]);
    assert_eq!(back["node_count"], before["node_count"]);
    assert_eq!(back["selected"], json!(from));
}

#[test]
fn every_kind_the_grammar_has_travels_across_an_exchange() {
    // The child counts are written here rather than read back from `shape_path::child_names`, so a kind
    // that stopped carrying its operands into the exchange moves a number instead of passing silently.
    let leaf_pair = |kind: &str| {
        json!({
            "route": "root", "combine": "Union", "keep": "a",
            "shape": match kind {
                "Sphere" => json!({ "kind": "Sphere", "r": 0.3, "centre": [0.0, 0.0, 0.0] }),
                "RoundedBox" => json!({ "kind": "RoundedBox", "half": [0.2, 0.2, 0.2], "r": 0.05 }),
                _ => json!({ "kind": "Plane", "normal": [0.0, 1.0, 0.0], "offset": 0.0 }),
            },
        })
    };
    let small = json!({ "kind": "Sphere", "r": 0.15, "centre": [0.4, 0.0, 0.0] });
    let builds: Vec<(&str, usize, Value, Option<Value>)> = vec![
        ("Sphere", 0, leaf_pair("Sphere"), None),
        ("RoundedBox", 0, leaf_pair("RoundedBox"), None),
        ("Plane", 0, leaf_pair("Plane"), None),
        (
            "Union",
            2,
            leaf_pair("Sphere"),
            Some(json!({ "route": "root.b", "combine": "Union", "keep": "a", "shape": small })),
        ),
        (
            "SmoothUnion",
            2,
            leaf_pair("Sphere"),
            Some(
                json!({ "route": "root.b", "combine": "SmoothUnion", "keep": "a", "k": 0.12, "shape": small }),
            ),
        ),
        (
            "Subtract",
            2,
            leaf_pair("Sphere"),
            Some(
                json!({ "route": "root.b", "combine": "Subtract", "keep": "kept", "shape": small }),
            ),
        ),
        (
            "Intersect",
            2,
            leaf_pair("Sphere"),
            Some(json!({ "route": "root.b", "combine": "Intersect", "keep": "a", "shape": small })),
        ),
        (
            "Mirror",
            1,
            leaf_pair("Sphere"),
            Some(json!({ "route": "root.b", "combine": "Mirror", "axis": 1 })),
        ),
        (
            "Repeat",
            1,
            leaf_pair("Sphere"),
            Some(json!({ "route": "root.b", "combine": "Repeat", "period": 0.7 })),
        ),
    ];

    let mut carried = 0;
    for (kind, children, seed, wrap) in builds {
        let mut state = fresh();
        call(&mut state, "add_part", seed);
        if let Some(second) = wrap {
            call(&mut state, "add_part", second);
        }
        let before = call(&mut state, "editor_state", json!({}));
        let was_parts = before["parts"].as_array().expect("a list").clone();
        assert_eq!(
            part_at(&was_parts, "root.b")["kind"],
            json!(kind),
            "the seed did not leave a {kind} at root.b, so {kind} is not being exchanged"
        );

        let moved = call(
            &mut state,
            "move_part",
            json!({ "from": "root.b", "to": "root.a" }),
        );
        let now_parts = moved["state"]["parts"].as_array().expect("a list").clone();
        // The moved node is at `to`, which the reply lists second; the first end is the slot it came from.
        assert_eq!(moved["exchanged"][0]["was"]["kind"], json!(kind));
        assert_eq!(moved["exchanged"][1]["now"]["kind"], json!(kind));
        assert_eq!(
            moved["exchanged"][1]["children"],
            json!(children),
            "the reply reads {kind}'s children off the wrong end"
        );
        assert_eq!(
            contents_under(&now_parts, "root.a"),
            contents_under(&was_parts, "root.b"),
            "a {kind} lost or gained a node on the way across"
        );
        assert_eq!(part_at(&now_parts, "root.a")["kind"], json!(kind));
        // The other end came the other way: the sphere the editor started with.
        assert_eq!(part_at(&now_parts, "root.b")["kind"], json!("Sphere"));
        assert_eq!(
            part_at(&now_parts, "root.b")["summary"],
            part_at(&was_parts, "root.a")["summary"]
        );
        assert_eq!(
            moved["node_count"], before["node_count"],
            "exchanging a {kind} moved the count"
        );
        carried += 1;
    }
    assert_eq!(
        carried, 9,
        "the authored grammar has nine kinds and this walked {carried}"
    );
}

#[test]
fn an_exchange_refuses_the_pair_that_sits_on_one_line_from_the_root() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let deep = before["parts"]
        .as_array()
        .expect("a list")
        .iter()
        .map(route_of)
        .find(|route| route.matches('.').count() >= 2)
        .expect(
            "the shipped pig is deeper than two levels, so a node holding another is in the list",
        )
        .to_string();
    let holder = deep.rsplit_once('.').expect("a dotted route").0.to_string();

    let cases: Vec<(&str, Value, bool)> = vec![
        (
            "a node and the slot that holds it",
            json!({ "from": holder, "to": deep }),
            false,
        ),
        (
            "the same pair, the other way round",
            json!({ "from": deep, "to": holder }),
            false,
        ),
        (
            "root and a part below it",
            json!({ "from": "root", "to": deep }),
            true,
        ),
        (
            "a part and root",
            json!({ "from": deep, "to": "root" }),
            true,
        ),
    ];
    for (what, arguments, mentions_root) in cases {
        let (message, refused) = try_call(&mut state, "move_part", arguments);
        assert!(refused, "{what} was exchanged: {message}");
        assert!(
            message.contains("holds"),
            "the refusal for {what} does not name the end that holds the other: {message}"
        );
        if mentions_root {
            assert!(
                message.contains("contains every route"),
                "the refusal for {what} does not say why root cannot be one end: {message}"
            );
        }
    }

    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        fingerprints(&after["parts"]),
        fingerprints(&before["parts"]),
        "four refused exchanges changed the tree"
    );
    assert_eq!(after["preview_signature"], before["preview_signature"]);
    assert_eq!(after["dirty"], json!(false));
    assert_eq!(after["selected"], before["selected"]);
}

#[test]
fn a_move_refuses_the_call_it_cannot_answer_and_equal_contents_move_nothing() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let before = call(&mut state, "editor_state", json!({}));
    let was_parts = before["parts"].as_array().expect("a list").clone();
    let leaf = was_parts
        .iter()
        .find(|part| {
            part["kind"]
                .as_str()
                .is_some_and(|kind| LEAF_KINDS.contains(&kind))
        })
        .map(|part| route_of(part).to_string())
        .expect("the pig ends in leaves");

    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "the same node at both ends",
            json!({ "from": "root.a", "to": "a" }),
            "the same node",
        ),
        (
            "no from",
            json!({ "to": "root.a" }),
            "from is required and must be a string",
        ),
        (
            "no to",
            json!({ "from": "root.a" }),
            "to is required and must be a string",
        ),
        (
            "a blank end",
            json!({ "from": "  ", "to": "root.a" }),
            "from is required",
        ),
        (
            "an end that is not a route",
            json!({ "from": 3, "to": "root.a" }),
            "must be a string",
        ),
        (
            "a slot the parent does not have",
            json!({ "from": "root.nope", "to": "root.a" }),
            "has no child",
        ),
        (
            "a step into a leaf",
            json!({ "from": format!("{leaf}.a"), "to": "root.a" }),
            "has no children",
        ),
    ];
    for (what, arguments, expected) in cases {
        let (message, refused) = try_call(&mut state, "move_part", arguments);
        assert!(refused, "{what} was accepted: {message}");
        assert!(
            message.contains(expected),
            "the refusal for {what} says {message:?}, which does not mention {expected:?}"
        );
    }
    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        fingerprints(&after["parts"]),
        fingerprints(&before["parts"]),
        "seven refused calls moved a node anyway"
    );
    assert_eq!(after["dirty"], json!(false));
    assert_eq!(after["selected"], before["selected"]);

    // Equal contents at the two ends: the file would come back identical, so the call says so instead of
    // reporting a move and leaving `dirty` set.
    let mut twin = fresh();
    call(
        &mut twin,
        "add_part",
        json!({
            "route": "root", "combine": "Union", "keep": "a",
            // The same sphere the fresh editor already holds, authored again beside it.
            "shape": { "kind": "Sphere", "r": 1.0, "centre": [0.0, 0.0, 0.0] },
        }),
    );
    let seed = call(&mut twin, "editor_state", json!({}));
    let seed_parts = seed["parts"].as_array().expect("a list").clone();
    assert_eq!(
        content_of(part_at(&seed_parts, "root.a")),
        content_of(part_at(&seed_parts, "root.b")),
        "the fixture was meant to hold the same content at both ends"
    );
    let (message, refused) = try_call(
        &mut twin,
        "move_part",
        json!({ "from": "root.a", "to": "root.b" }),
    );
    assert!(refused, "two identical halves were exchanged: {message}");
    assert!(
        message.contains("moves nothing"),
        "the refusal does not say the exchange would move nothing: {message}"
    );
    assert!(
        message.contains(
            &part_at(&seed_parts, "root.a")["summary"]
                .as_str()
                .unwrap()
                .to_string()
        ),
        "the refusal does not quote the content it refused: {message}"
    );
    let clean = call(&mut twin, "editor_state", json!({}));
    assert_eq!(
        clean["dirty"], seed["dirty"],
        "a refused exchange changed the dirty flag the add had already set"
    );
    assert_eq!(clean["preview_signature"], seed["preview_signature"]);
}

#[test]
fn an_exchange_the_compiler_cannot_lower_is_refused_before_it_lands() {
    // A tree whose `kept` side can be grown to the grammar's cap, with a second branch two levels deep to
    // exchange the tall one into. The cap itself is never written here: the loop stops when the compiler
    // says so, the same way the add test finds it.
    let seed = temp_path("cap_seed.ron");
    std::fs::write(
        &seed,
        "EntityDef(
    shape: Subtract(
        kept: Sphere(r: 0.4, centre: (0.0, 0.0, 0.0)),
        cutter: Subtract(
            kept: Sphere(r: 0.25, centre: (1.0, 0.0, 0.0)),
            cutter: Sphere(r: 0.25, centre: (-1.0, 0.0, 0.0)),
        ),
    ),
)
",
    )
    .expect("the fixture is writable");

    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": seed.to_string_lossy() }),
    );

    let mut grown = 0;
    let mut last_refusal = String::new();
    for _ in 0..64 {
        let (message, refused) = try_call(
            &mut state,
            "add_part",
            json!({
                "route": "root.kept",
                "combine": "Subtract",
                "keep": "kept",
                "shape": { "kind": "Sphere", "r": 0.05, "centre": [5.0, 5.0, 5.0] },
            }),
        );
        if !refused {
            grown += 1;
            continue;
        }
        last_refusal = message;
        break;
    }
    assert!(
        grown > 1,
        "the kept chain never grew, so this test cannot see the cap: {last_refusal}"
    );
    assert!(
        last_refusal.contains("levels deep") && last_refusal.contains("cap"),
        "the growth stopped for a reason other than the depth cap: {last_refusal}"
    );

    // The tree now sits exactly at the cap, and the exchange that would push the tall chain one step
    // deeper is the move a caller would never see coming: same node count, same parameters, no free slot.
    let at_cap = call(&mut state, "editor_state", json!({}));
    assetc::compile_entity(&state.entity_def)
        .expect("the grown tree is meant to be exactly at the cap");
    let (message, refused) = try_call(
        &mut state,
        "move_part",
        json!({ "from": "root.kept", "to": "root.cutter.kept" }),
    );
    assert!(
        refused,
        "a {grown}-deep chain moved one step further down and the compiler said nothing: {message}"
    );
    assert!(
        message.contains("cannot be compiled"),
        "the refusal is not the compiler's: {message}"
    );
    assert!(
        message.contains("levels deep") && message.contains("cap"),
        "the refusal does not quote the compiler's depth words: {message}"
    );

    let after = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        fingerprints(&after["parts"]),
        fingerprints(&at_cap["parts"]),
        "the exchange the compiler refused rewrote the tree anyway"
    );
    assert_eq!(after["preview_signature"], at_cap["preview_signature"]);
    assert_eq!(after["selected"], at_cap["selected"]);

    let _ = std::fs::remove_file(seed);
}

#[test]
fn exchanging_a_subs_two_halves_turns_a_bite_into_a_crescent() {
    // Not a commutative parent: exchanging a `Union`'s operands leaves the solid exactly as it was, so a
    // frame test on one would pass on a tool that moved nothing. `kept` and `cutter` are the pair where
    // the slot is the meaning — the difference between a hole and an animal.
    let mut state = fresh();
    let view = json!({ "view": "front", "width": 96, "height": 72, "stage": false });

    let bitten = call(
        &mut state,
        "add_part",
        json!({
            "route": "root", "combine": "Subtract", "keep": "kept",
            "shape": { "kind": "Sphere", "r": 0.6, "centre": [0.0, 0.6, 0.0] },
        }),
    );
    let before_frame = call(&mut state, "render_view", view.clone());
    assert_eq!(
        before_frame["preview_signature"],
        bitten["preview_signature"]
    );

    let moved = call(
        &mut state,
        "move_part",
        json!({ "from": "root.kept", "to": "root.cutter" }),
    );
    let after_frame = call(&mut state, "render_view", view);

    assert_eq!(
        after_frame["preview_signature"], moved["preview_signature"],
        "the frame rendered is not the state the exchange left behind"
    );
    assert_ne!(
        after_frame["path"], before_frame["path"],
        "two different states wrote the same PNG, so a reader cannot tell which frame they are looking at"
    );
    assert_eq!(
        after_frame["undrawn_pixels"],
        json!(0),
        "the march refused to draw the frame the exchange produced"
    );
    assert_eq!(before_frame["undrawn_pixels"], json!(0));

    let bitten_surface = before_frame["surface_pixels"].as_u64().expect("a count");
    let crescent = after_frame["surface_pixels"].as_u64().expect("a count");
    assert!(
        bitten_surface > 0,
        "the ball with a bite in it drew nothing, so this instrument cannot show a loss"
    );
    assert!(
        crescent > 0,
        "the cap that sticks out past the ball was marched away entirely"
    );
    assert!(
        crescent < bitten_surface,
        "keeping the small sphere and cutting with the big one left the picture no smaller: {} -> {}",
        bitten_surface,
        crescent,
    );
}

#[test]
fn deleting_a_nested_wrapper_unwraps_it_and_carries_the_subtree_it_held() {
    let mut state = fresh();
    call(
        &mut state,
        "set_part",
        json!({ "route": "root", "parameter": "centre", "value": [0.4, 0.0, 0.0] }),
    );
    // What the wrapper holds is a subtree rather than a leaf, so this measures whether the unwrap carries
    // all of it up one level or only the node immediately inside the wrapper.
    call(
        &mut state,
        "add_part",
        json!({ "route": "root", "combine": "Union", "keep": "a",
                "shape": { "kind": "Sphere", "r": 0.2, "centre": [0.9, 0.0, 0.0] } }),
    );
    call(
        &mut state,
        "add_part",
        json!({ "route": "root", "combine": "Repeat", "period": 1.2 }),
    );
    let wrapped = call(
        &mut state,
        "add_part",
        json!({ "route": "root", "combine": "Mirror", "axis": 0 }),
    );
    let before_parts = wrapped["state"]["parts"]
        .as_array()
        .expect("a list")
        .clone();
    assert_eq!(part_at(&before_parts, "root")["kind"], json!("Mirror"));
    assert_eq!(
        part_at(&before_parts, "root.child")["kind"],
        json!("Repeat")
    );
    assert_eq!(
        part_at(&before_parts, "root.child.child")["kind"],
        json!("Union")
    );
    assert_eq!(
        before_parts.len(),
        5,
        "the fixture is not the tree the unwrap is meant to act on: {before_parts:?}"
    );
    // The wrapper's subtree, node for node, counted from the node it holds.
    let mut held = vec![content_of(part_at(&before_parts, "root.child.child"))];
    held.extend(contents_under(&before_parts, "root.child.child"));
    held.sort();

    let deleted = call(&mut state, "delete_part", json!({ "route": "root.child" }));
    assert_eq!(deleted["deleted"]["kind"], json!("Repeat"));
    assert_eq!(
        deleted["node_count"].as_u64().expect("a count") + 1,
        deleted["nodes_before"].as_u64().expect("a count"),
        "unwrapping removes the wrapper and nothing else: {}",
        deleted["node_count"]
    );
    let after_parts = deleted["state"]["parts"]
        .as_array()
        .expect("a list")
        .clone();
    // The wrapper's own route now names the node it held — the survivor moved up one level, and the route
    // it moved up to is the one the caller deleted.
    assert_eq!(deleted["its_parent"]["route"], json!("root.child"));
    assert_eq!(deleted["its_parent"]["replaced_by"], json!("Union"));
    let mut carried = vec![content_of(part_at(&after_parts, "root.child"))];
    carried.extend(contents_under(&after_parts, "root.child"));
    carried.sort();
    assert_eq!(
        carried, held,
        "the unwrap promoted the node inside the wrapper and left the rest of that node's subtree behind"
    );
    assert_eq!(part_at(&after_parts, "root")["kind"], json!("Mirror"));
    assert_eq!(
        deleted["selected"],
        json!("root.child"),
        "the selection should follow the node that was promoted, not retreat from a route that still means it"
    );
    // Nothing outside the wrapper survives by luck: the Mirror kept its axis and the tree is one node
    // shorter by exactly the wrapper.
    assert_eq!(
        part_at(&after_parts, "root")["summary"],
        part_at(&before_parts, "root")["summary"],
        "the wrapper over the one that came out moved"
    );
    assert!(!after_parts
        .iter()
        .any(|part| route_of(part).starts_with("root.child.child.child")));
    assert!(deleted["state"]["dirty"].as_bool().expect("a flag"));
    assert_ne!(
        deleted["preview_signature"], wrapped["preview_signature"],
        "a node was removed and the preview key did not move"
    );

    let saved = temp_path("incr7_unwrap.ron");
    call(
        &mut state,
        "save_entity",
        json!({ "path": saved.to_string_lossy() }),
    );
    let mut reloaded = fresh();
    call(
        &mut reloaded,
        "load_entity",
        json!({ "path": saved.to_string_lossy() }),
    );
    let read = call(&mut reloaded, "editor_state", json!({}));
    assert_eq!(
        fingerprints(&read["parts"]),
        fingerprints(&deleted["state"]["parts"]),
        "the file reads back as a different tree than the unwrap reported"
    );
    assert_eq!(read["dirty"], json!(false));
    // And it is a tree the compiler accepts, not only one the editor is holding.
    assetc::compile_entity(&reloaded.entity_def)
        .expect("the tree an unwrap left cannot compile into an asset");
    let _ = std::fs::remove_file(saved);
}

#[test]
fn wrapping_and_unwrapping_the_same_route_is_the_identity_at_any_depth() {
    let mut state = fresh();
    call(
        &mut state,
        "set_part",
        json!({ "route": "root", "parameter": "centre", "value": [0.4, 0.0, 0.0] }),
    );
    let mirrored = call(
        &mut state,
        "add_part",
        json!({ "route": "root", "combine": "Mirror", "axis": 1 }),
    );
    // The nested case: wrap the node *inside* the Mirror, at a route that is not the root.
    let nested = call(
        &mut state,
        "add_part",
        json!({ "route": "root.child", "combine": "Repeat", "period": 0.9 }),
    );
    assert_eq!(
        nested["node_count"].as_u64().expect("a count"),
        mirrored["node_count"].as_u64().expect("a count") + 1,
        "the nested wrap did not add its one node: {}",
        nested["state"]
    );

    let undone = call(&mut state, "delete_part", json!({ "route": "root.child" }));
    assert_eq!(
        undone["preview_signature"], mirrored["preview_signature"],
        "wrapping a route and deleting the wrapper left a different entity than the one the wrap started from"
    );
    assert_eq!(
        fingerprints(&undone["state"]["parts"]),
        fingerprints(&mirrored["state"]["parts"]),
        "the unwrap changed a node it should not have touched"
    );
    assert_eq!(
        undone["node_count"], mirrored["node_count"],
        "the node count did not come back"
    );

    // The promoted node is still the editor's own part: reachable, selectable, editable through its route.
    call(
        &mut state,
        "set_part",
        json!({ "route": "root.child", "parameter": "r", "value": 0.35 }),
    );
    let now = call(&mut state, "editor_state", json!({}));
    let now_parts = now["parts"].as_array().expect("a list").clone();
    assert_eq!(part_at(&now_parts, "root.child")["kind"], json!("Sphere"));
    assert!(
        part_at(&now_parts, "root.child")["summary"]
            .as_str()
            .expect("a summary")
            .starts_with("r 0.35"),
        "the radius edit did not land on the node the unwrap promoted: {}",
        part_at(&now_parts, "root.child")["summary"]
    );
}

#[test]
fn a_wrapper_under_a_wrapper_comes_out_one_at_a_time() {
    let mut state = fresh();
    call(
        &mut state,
        "set_part",
        json!({ "route": "root", "parameter": "centre", "value": [0.4, 0.0, 0.0] }),
    );
    call(
        &mut state,
        "add_part",
        json!({ "route": "root", "combine": "Repeat", "period": 1.1 }),
    );
    let stacked = call(
        &mut state,
        "add_part",
        json!({ "route": "root", "combine": "Mirror", "axis": 2 }),
    );
    let stacked_parts = stacked["state"]["parts"]
        .as_array()
        .expect("a list")
        .clone();
    assert_eq!(stacked["node_count"], json!(3));
    let sphere = content_of(part_at(&stacked_parts, "root.child.child"));

    let inner = call(&mut state, "delete_part", json!({ "route": "root.child" }));
    assert_eq!(inner["node_count"], json!(2));
    let middle_parts = inner["state"]["parts"].as_array().expect("a list").clone();
    assert_eq!(part_at(&middle_parts, "root")["kind"], json!("Mirror"));
    assert_eq!(content_of(part_at(&middle_parts, "root.child")), sphere);

    // And the root wrapper the same rule already allowed: one at a time, until the tree is the leaf.
    let outer = call(&mut state, "delete_part", json!({ "route": "root" }));
    assert_eq!(outer["node_count"], json!(1));
    let last_parts = outer["state"]["parts"].as_array().expect("a list").clone();
    assert_eq!(last_parts.len(), 1, "a leaf should be the whole list");
    assert_eq!(content_of(&last_parts[0]), sphere);
    assert_eq!(outer["selected"], json!("root"));

    // The leaf that came out of two unwraps is the entity, so it cannot itself be deleted.
    let (message, refused) = try_call(&mut state, "delete_part", json!({ "route": "root" }));
    assert!(refused, "the last node was deleted: {message}");
    assert!(
        message.contains("the whole entity"),
        "deleting a root leaf refused without saying why: {message}"
    );

    // The other nesting order, because the rule is about the node being deleted and not about which
    // wrapper it is: a `Mirror` inside a `Repeat` has to come out the same way.
    let mut other = fresh();
    call(
        &mut other,
        "set_part",
        json!({ "route": "root", "parameter": "centre", "value": [0.4, 0.0, 0.0] }),
    );
    call(
        &mut other,
        "add_part",
        json!({ "route": "root", "combine": "Mirror", "axis": 1 }),
    );
    let swapped = call(
        &mut other,
        "add_part",
        json!({ "route": "root", "combine": "Repeat", "period": 1.3 }),
    );
    let before_swap = swapped["state"]["parts"]
        .as_array()
        .expect("a list")
        .clone();
    assert_eq!(part_at(&before_swap, "root")["kind"], json!("Repeat"));
    assert_eq!(part_at(&before_swap, "root.child")["kind"], json!("Mirror"));
    let inner = call(&mut other, "delete_part", json!({ "route": "root.child" }));
    assert_eq!(inner["deleted"]["kind"], json!("Mirror"));
    assert_eq!(inner["node_count"], json!(2));
    let after_swap = inner["state"]["parts"].as_array().expect("a list").clone();
    assert_eq!(part_at(&after_swap, "root")["kind"], json!("Repeat"));
    assert_eq!(
        content_of(part_at(&after_swap, "root.child")),
        content_of(part_at(&before_swap, "root.child.child")),
        "the sphere that came out of a nested Mirror is not the one that went in"
    );
    assert_eq!(inner["selected"], json!("root.child"));
    assetc::compile_entity(&other.entity_def).expect("the unwrapped Repeat cannot compile");
}

#[test]
fn the_refusal_for_a_wrappers_child_names_the_call_that_actually_works() {
    let mut state = fresh();
    call(
        &mut state,
        "set_part",
        json!({ "route": "root", "parameter": "centre", "value": [0.4, 0.0, 0.0] }),
    );
    let wrapped = call(
        &mut state,
        "add_part",
        json!({ "route": "root", "combine": "Mirror", "axis": 0 }),
    );
    let before_parts = wrapped["state"]["parts"]
        .as_array()
        .expect("a list")
        .clone();
    let sphere = content_of(part_at(&before_parts, "root.child"));

    let (message, refused) = try_call(&mut state, "delete_part", json!({ "route": "root.child" }));
    assert!(refused, "a Mirror's only child was deleted: {message}");
    assert!(
        message.contains("only child"),
        "the refusal does not say what would be left holding nothing: {message}"
    );
    assert!(
        message.contains("Mirror") && message.contains("\"root\""),
        "the refusal does not name the wrapper and the route that would take it out: {message}"
    );
    let untouched = call(&mut state, "editor_state", json!({}));
    assert_eq!(
        fingerprints(&untouched["parts"]),
        fingerprints(&wrapped["state"]["parts"]),
        "a refused delete changed the tree"
    );

    // Follow the advice. A refusal an author cannot act on is a dead end, not a refusal.
    let unwrapped = call(&mut state, "delete_part", json!({ "route": "root" }));
    assert_eq!(unwrapped["node_count"], json!(1));
    let after_parts = unwrapped["state"]["parts"]
        .as_array()
        .expect("a list")
        .clone();
    assert_eq!(content_of(part_at(&after_parts, "root")), sphere);
}

#[test]
fn a_nested_wrappers_refusal_names_the_wrapper_where_it_actually_is() {
    let mut state = fresh();
    call(
        &mut state,
        "add_part",
        json!({ "route": "root", "combine": "Mirror", "axis": 0 }),
    );
    let twice = call(
        &mut state,
        "add_part",
        json!({ "route": "root.child", "combine": "Repeat", "period": 1.2 }),
    );
    // Mirror { Repeat { Sphere } }: the sphere the author is trying to delete sits two levels down, and the
    // wrapper that would refuse it is `root.child`, not `root`.
    let before_parts = twice["state"]["parts"].as_array().expect("a list").clone();
    assert_eq!(part_at(&before_parts, "root")["kind"], json!("Mirror"));
    assert_eq!(
        part_at(&before_parts, "root.child")["kind"],
        json!("Repeat")
    );
    let sphere = content_of(part_at(&before_parts, "root.child.child"));

    let (message, refused) = try_call(
        &mut state,
        "delete_part",
        json!({ "route": "root.child.child" }),
    );
    assert!(refused, "a Repeat's only child was deleted: {message}");
    assert!(
        message.contains("only child") && message.contains("Repeat"),
        "the refusal does not say which wrapper would be left holding nothing: {message}"
    );
    assert!(
        message.contains("\"root.child\""),
        "the refusal does not name the route the wrapper actually holds, so the call it suggests is \
         one the caller cannot make: {message}"
    );
    assert!(
        !message.contains("at \"root\" "),
        "the refusal sends the caller to the root, which is the Mirror over the whole thing and not the \
         wrapper in the way: {message}"
    );

    // Follow the advice: the named route is the call that works, and it leaves the sphere exactly where it
    // stands inside the Mirror.
    let unwrapped = call(&mut state, "delete_part", json!({ "route": "root.child" }));
    assert_eq!(unwrapped["deleted"]["kind"], json!("Repeat"));
    assert_eq!(unwrapped["node_count"], json!(2));
    let after_parts = unwrapped["state"]["parts"]
        .as_array()
        .expect("a list")
        .clone();
    assert_eq!(part_at(&after_parts, "root")["kind"], json!("Mirror"));
    assert_eq!(content_of(part_at(&after_parts, "root.child")), sphere);
    assert_eq!(unwrapped["selected"], json!("root.child"));
    assetc::compile_entity(&state.entity_def)
        .expect("the tree the refusal sent the caller to compiles");
}

#[test]
fn unwrapping_a_repeat_out_of_a_mirror_loses_the_tiles_it_drew() {
    let mut state = fresh();
    // The part has to sit wholly inside the fold cell, because a `Repeat` clips its child to the cell and
    // then tiles it: an r-1.0 sphere in a period-1.6 cell draws *less* than the sphere alone, which is the
    // trap the first draft of this test fell into when it measured 72 tiled against 709 unwrapped. A 0.15
    // sphere at x = 0.3 spans 0.15 to 0.45 and is entirely inside -0.6 to 0.6, so every copy is a whole one.
    call(
        &mut state,
        "set_part",
        json!({ "route": "root", "parameter": "r", "value": 0.15 }),
    );
    call(
        &mut state,
        "set_part",
        json!({ "route": "root", "parameter": "centre", "value": [0.3, 0.0, 0.0] }),
    );
    call(
        &mut state,
        "add_part",
        json!({ "route": "root", "combine": "Repeat", "period": 1.2 }),
    );
    call(
        &mut state,
        "add_part",
        json!({ "route": "root", "combine": "Mirror", "axis": 0 }),
    );
    let view = json!({
        "yaw": core::f32::consts::PI, "pitch": 0.12, "distance": 6.0,
        "width": 96, "height": 72, "stage": false,
    });
    let tiled = call(&mut state, "render_view", view.clone());
    let tiled_surface = tiled["surface_pixels"].as_u64().expect("a count");
    assert!(
        tiled_surface > 0 && tiled_surface < tiled["total_pixels"].as_u64().expect("a count"),
        "the tiled frame is blank or saturated, so a smaller frame after the unwrap would prove nothing: \
         {tiled_surface} of {}",
        tiled["total_pixels"]
    );
    assert_eq!(tiled["undrawn_pixels"], json!(0));

    let undone = call(&mut state, "delete_part", json!({ "route": "root.child" }));
    let alone = call(&mut state, "render_view", view);
    assert_eq!(
        alone["preview_signature"], undone["preview_signature"],
        "the frame rendered is not the state the unwrap left behind"
    );
    assert_ne!(alone["path"], tiled["path"]);
    assert_eq!(
        alone["undrawn_pixels"],
        json!(0),
        "the march refused to draw the unwrapped frame, which reads the same as an empty one"
    );
    let alone_surface = alone["surface_pixels"].as_u64().expect("a count");
    assert!(
        alone_surface > 0,
        "the mirror of one sphere should still draw a sphere, and drew {alone_surface} pixels"
    );
    assert!(
        alone_surface < tiled_surface,
        "the copy the Repeat was drawing survived the unwrap: {} -> {}",
        tiled_surface,
        alone_surface,
    );
}

/// A dumped frame decoded back to pixels. The alternative is a test that trusts the counter printed by
/// the same call that wrote the file, which is a number agreeing with itself.
fn decoded_rgb(reply: &Value) -> image::RgbImage {
    let path = PathBuf::from(
        reply["path"]
            .as_str()
            .unwrap_or_else(|| panic!("no path in {reply}")),
    );
    image::open(&path)
        .unwrap_or_else(|e| panic!("{} is not a readable PNG: {e}", path.display()))
        .to_rgb8()
}

/// The bytes of a dumped frame, for a row that has to say two pictures are the one picture.
fn read_png(reply: &Value) -> Vec<u8> {
    let path = PathBuf::from(
        reply["path"]
            .as_str()
            .unwrap_or_else(|| panic!("no path in {reply}")),
    );
    std::fs::read(&path).unwrap_or_else(|e| panic!("{} was not written: {e}", path.display()))
}

/// How many pixels of a decoded frame are exactly one colour.
fn count_colour(frame: &image::RgbImage, colour: [u8; 3]) -> usize {
    frame
        .as_raw()
        .as_chunks::<3>()
        .0
        .iter()
        .filter(|pixel| **pixel == colour)
        .count()
}

/// A number out of a reply.
fn number_at(value: &Value, label: &str) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("{label} is not a number: {value}"))
}

/// A material lane, checked against the `f32` the registry holds. An `f32` printed through `f64` spells
/// `0.7` as `0.699999988079071`, so a `json!` equality on a colour would refuse a correct reply.
fn near(got: f64, want: f32, label: &str) {
    assert!(
        (got - want as f64).abs() < 1e-5,
        "{label} reads {got}, expected {want}"
    );
}

/// The first candidate a slot does not already carry. The candidates are exactly representable `f32`s, so
/// the reply's copy of the number compares to the one that was sent without a tolerance, and reading the
/// current value is what stops a row passing on a write of the value that was already there.
fn a_different_number(current: &Value, candidates: &[f32]) -> f64 {
    let now = number_at(current, "a material field");
    let chosen = *candidates
        .iter()
        .find(|want| (now - **want as f64).abs() > 1e-6)
        .unwrap_or_else(|| panic!("the slot carries every candidate in {candidates:?}: {now}"));
    chosen as f64
}

#[test]
fn one_state_rendered_at_two_sizes_or_stages_writes_two_files() {
    // The name used to carry the state alone, so these three calls — one state, three pictures — all wrote
    // `front-<signature>.png`. The collision was found by two tests in this file rendering that path at
    // once and one of them decoding a half-written file, which is the same thing a client would hit.
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let bare = call(
        &mut state,
        "render_view",
        json!({ "view": "front", "width": 32, "height": 24, "stage": false }),
    );
    let staged = call(
        &mut state,
        "render_view",
        json!({ "view": "front", "width": 32, "height": 24 }),
    );
    let larger = call(
        &mut state,
        "render_view",
        json!({ "view": "front", "width": 48, "height": 36, "stage": false }),
    );
    assert_eq!(
        bare["preview_signature"], staged["preview_signature"],
        "three renders of one state must agree on the key, or the paths below differ for the wrong reason"
    );
    for other in [&staged, &larger] {
        assert_ne!(
            other["path"], bare["path"],
            "two different pictures were written to one path, so a reader cannot tell which frame it holds"
        );
    }
    // And each file holds the frame its own call asked for, which is what a reader opens it to see.
    assert_eq!(decoded_rgb(&staged).as_raw().len() / 3, 32 * 24);
    assert_eq!(decoded_rgb(&larger).as_raw().len() / 3, 48 * 36);
}

#[test]
fn set_material_paints_base_color_into_exactly_the_pixels_the_march_reports() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    // No stage: the floor is drawn without the tint, so every pixel of the frame is surface or the literal
    // sky, and the two counts have to add up to the whole image.
    let view = json!({ "view": "front", "width": 64, "height": 48, "stage": false });
    let pink = call(&mut state, "render_view", view.clone());
    assert_eq!(
        count_colour(&decoded_rgb(&pink), [0, 0, 0]),
        0,
        "the pig's registered skin already draws black somewhere, so a black count would not pick out the \
         tinted pixels"
    );

    let written = call(
        &mut state,
        "set_material",
        json!({ "slot": 0, "base_color": [0.0, 0.0, 0.0] }),
    );
    assert_eq!(written["slot"], json!(0));
    assert_eq!(written["changed"], json!(["base_color"]));
    assert_eq!(written["changed_a_pixel"], json!(["base_color"]));
    assert_eq!(written["changed_no_pixel"], json!([]));
    assert_eq!(
        written["dirty"],
        json!(false),
        "the registry is not in the entity file, so a recolour cannot be something a save carries"
    );
    // What was overwritten comes back as the preset's own numbers, because `before` is only a way to undo
    // a mistake if it is the value that was really there.
    let before = written["before"]["base_color"]
        .as_array()
        .expect("the material the slot held");
    for (lane, want) in before.iter().zip([1.0f32, 0.7, 0.75]) {
        near(
            number_at(lane, "the registered skin colour"),
            want,
            "the slot's red/green/blue before the write",
        );
    }
    assert_eq!(
        written["material"]["base_color"],
        json!([0.0, 0.0, 0.0]),
        "the reply shows what the slot holds, and it is not what was asked for"
    );

    let black = call(&mut state, "render_view", view);
    assert_ne!(
        black["preview_signature"], pink["preview_signature"],
        "a recolour left the preview key alone, so the viewport would keep showing the pink pig"
    );
    assert_ne!(black["path"], pink["path"]);
    assert_eq!(
        black["surface_pixels"],
        pink["surface_pixels"],
        "a colour changed which pixels are surface: the two frames marched the same tree from the same \
         camera, so only the tint may differ"
    );

    let surface = black["surface_pixels"].as_u64().expect("a count") as usize;
    let total = black["total_pixels"].as_u64().expect("a count") as usize;
    assert!(
        surface > 0 && surface < total,
        "the front view of a pig covers {} of {} pixels, so neither count proves anything",
        surface,
        total
    );
    let frame = decoded_rgb(&black);
    assert_eq!(
        frame.as_raw().len() / 3,
        total,
        "the file is not the size the reply said it marched"
    );
    // The invariant: a black tint reaches exactly the pixels the march reports as surface, and the rest of
    // the frame is the sky this renderer writes for a miss. The two counts partition the file.
    assert_eq!(
        count_colour(&frame, [0, 0, 0]),
        surface,
        "the black pixels of the recoloured frame are not the surface the march reported"
    );
    assert_eq!(
        count_colour(&frame, [30, 30, 35]),
        total - surface,
        "the untinted pixels are not the sky, which means the tint leaked past the entity or the entity \
         does not cover what the depth says it does"
    );
}

#[test]
fn the_five_material_fields_no_shader_reads_move_the_registry_and_neither_pixel_nor_key() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let view = json!({ "view": "side", "width": 64, "height": 48, "stage": false });
    let baseline = call(&mut state, "render_view", view.clone());
    let key = baseline["preview_signature"].clone();
    assert!(
        baseline["surface_pixels"].as_u64().expect("a count") > 0,
        "an empty frame would make the byte comparison below true for any edit at all"
    );

    let current = call(&mut state, "editor_state", json!({}))
        .as_object()
        .expect("a state")
        .get("material")
        .cloned()
        .expect("the state carries the assigned material");
    let roughness = a_different_number(&current["roughness"], &[0.25, 0.75]);
    let emissive = a_different_number(&current["emissive"], &[0.25, 0.5]);
    let index_ior = a_different_number(&current["index_ior"], &[2.0, 3.0]);
    let frequency = a_different_number(&current["noise_frequency"], &[4.0, 8.0]);
    let now_noise = current["noise"]
        .as_str()
        .expect("the registry spells its noise kind")
        .to_string();
    let noise = ["Voronoi", "Value", "Perlin", "None"]
        .into_iter()
        .find(|kind| *kind != now_noise)
        .expect("four kinds, so one of them differs from the slot's own");

    let cases: [(&str, Value); 5] = [
        ("roughness", json!(roughness)),
        ("emissive", json!(emissive)),
        ("index_ior", json!(index_ior)),
        ("noise", json!(noise)),
        ("noise_frequency", json!(frequency)),
    ];
    for (name, value) in cases {
        let mut arguments = json!({ "slot": 0 });
        arguments
            .as_object_mut()
            .expect("a call is an object")
            .insert(name.to_string(), value.clone());
        let written = call(&mut state, "set_material", arguments);
        assert_eq!(
            written["changed"],
            json!([name]),
            "the write of {name} as {value} reported no change, so this row measured nothing"
        );
        assert_eq!(
            written["changed_a_pixel"],
            json!([]),
            "{name} claimed a reader it does not have"
        );
        assert_eq!(written["changed_no_pixel"], json!([name]));
        assert_eq!(
            written["material"][name], value,
            "the registry does not hold the {name} this call wrote"
        );
        assert_eq!(
            written["preview_signature"],
            key,
            "the preview key moved for {name}, which no shader reads: an author would pay a march to \
             redraw the same picture"
        );
        assert_eq!(written["dirty"], json!(false));
        let notice = written["notice"].as_str().expect("a notice");
        assert!(
            notice.contains(name),
            "the notice did not name {name} among the fields with no reader: {notice}"
        );
        assert!(
            !notice.contains("Every value you wrote"),
            "a real write answered with the no-op sentence: {notice}"
        );
    }

    // Five writes, one picture: the frame is the bytes it was before any of them.
    let after = call(&mut state, "render_view", view.clone());
    assert_eq!(after["preview_signature"], key);
    assert_eq!(after["surface_pixels"], baseline["surface_pixels"]);
    assert_eq!(
        read_png(&after),
        read_png(&baseline),
        "the five fields no shader reads changed the dumped picture"
    );

    // The control that makes the equality above mean something: the same instrument does move when the one
    // tinted field is written, so identical bytes say the other fields reached nothing.
    call(
        &mut state,
        "set_material",
        json!({ "slot": 0, "base_color": [0.0, 0.0, 1.0] }),
    );
    let painted = call(&mut state, "render_view", view);
    assert_ne!(
        read_png(&painted),
        read_png(&baseline),
        "no write of any material field changes the picture, which means this row was never measuring \
         pixels"
    );
}

#[test]
fn set_material_refuses_by_naming_what_it_will_take_and_writes_nothing() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let registry = call(&mut state, "list_materials", json!({}));
    let key = call(&mut state, "editor_state", json!({}))["preview_signature"]
        .as_u64()
        .expect("a key");

    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "a slot nobody registered",
            json!({ "slot": 7, "roughness": 0.5 }),
            vec!["slot 7 is empty", "0, 1, 2"],
        ),
        (
            "a field a material does not have",
            json!({ "slot": 0, "colour": [1.0, 0.0, 0.0] }),
            vec![
                "no field \"colour\"",
                "\"base_color\"",
                "\"roughness\"",
                "\"emissive\"",
                "\"index_ior\"",
                "\"noise\"",
                "\"noise_frequency\"",
            ],
        ),
        (
            "a slot never handed over",
            json!({ "roughness": 0.5 }),
            vec!["slot is required and must be a number"],
        ),
        (
            "a slot that is not a number",
            json!({ "slot": "0", "roughness": 0.5 }),
            vec!["slot must be a number"],
        ),
        (
            "a fractional slot",
            json!({ "slot": 0.5, "roughness": 0.5 }),
            vec!["whole slot 0-255"],
        ),
        (
            "a slot past the registry",
            json!({ "slot": 256, "roughness": 0.5 }),
            vec!["whole slot 0-255"],
        ),
        (
            "a colour with two lanes",
            json!({ "slot": 0, "base_color": [1.0, 0.0] }),
            vec!["takes 3 lanes", "hands 2"],
        ),
        (
            "a colour that is not numbers",
            json!({ "slot": 0, "base_color": [1.0, "pink", 0.0] }),
            vec!["must be numbers only"],
        ),
        (
            "a roughness past its contract",
            json!({ "slot": 0, "roughness": 1.5 }),
            vec!["\"roughness\" must be >= 0 and <= 1", "lane 0 is 1.5"],
        ),
        (
            "a negative colour lane",
            json!({ "slot": 0, "base_color": [-0.5, 0.0, 0.0] }),
            vec!["\"base_color\" must be >= 0 and <= 1", "lane 0 is -0.5"],
        ),
        (
            "a frequency of zero",
            json!({ "slot": 0, "noise_frequency": 0.0 }),
            vec!["\"noise_frequency\" must be > 0", "lane 0 is 0"],
        ),
        (
            "an index of refraction below air",
            json!({ "slot": 0, "index_ior": 0.5 }),
            vec!["\"index_ior\" must be >= 1"],
        ),
        (
            "a noise kind spelled as a number",
            json!({ "slot": 0, "noise": 2 }),
            vec![
                "\"noise\" must be one of \"Voronoi\", \"Value\", \"Perlin\", \"None\"",
                "got 2",
            ],
        ),
        (
            "a noise kind that is not one of them",
            json!({ "slot": 0, "noise": "perlin" }),
            vec!["no noise kind \"perlin\"", "\"Perlin\""],
        ),
        (
            "a call with slot and nothing to write",
            json!({ "slot": 0 }),
            vec!["no field to write", "\"base_color\""],
        ),
    ];

    for (label, arguments, needles) in cases {
        let (message, refused) = try_call(&mut state, "set_material", arguments);
        assert!(refused, "{label} was accepted: {message}");
        for needle in needles {
            assert!(
                message.contains(needle),
                "{label} refused without saying {needle:?}: {message}"
            );
        }
        // A refusal that half-wrote a preset would leave an author with a material they never asked for,
        // and the only way to see it is to read the registry back.
        assert_eq!(
            call(&mut state, "list_materials", json!({})),
            registry,
            "{label} changed a material it refused"
        );
        let state_block = call(&mut state, "editor_state", json!({}));
        assert_eq!(
            state_block["dirty"],
            json!(false),
            "{label} marked the entity dirty"
        );
        assert_eq!(
            state_block["preview_signature"].as_u64().expect("a key"),
            key,
            "{label} moved the preview key while refusing"
        );
    }

    // And the call that writes what is already there is a no-op that says so, rather than a change. The
    // value is read out of the slot first, so the call cannot pass on writing a number that differs.
    let already = number_at(
        &call(&mut state, "editor_state", json!({}))["material"]["roughness"],
        "the slot's own roughness",
    );
    let carried = call(
        &mut state,
        "set_material",
        json!({ "slot": 0, "roughness": already }),
    );
    assert_eq!(carried["changed"], json!([]));
    assert!(
        carried["notice"]
            .as_str()
            .expect("a notice")
            .contains("Every value you wrote"),
        "a no-op answered without saying nothing moved: {}",
        carried["notice"]
    );
    assert_eq!(
        call(&mut state, "list_materials", json!({})),
        registry,
        "the no-op wrote anyway"
    );
}

#[test]
fn a_recolour_is_session_only_while_the_slot_choice_is_the_half_that_persists() {
    let mut state = fresh();
    call(
        &mut state,
        "load_entity",
        json!({ "path": pig_ron().to_string_lossy() }),
    );
    let assigned = call(&mut state, "editor_state", json!({}))["transform"]["material_id"]
        .as_u64()
        .expect("the pig's slot");

    call(
        &mut state,
        "set_material",
        json!({ "slot": 0, "base_color": [0.0, 0.0, 0.0] }),
    );
    assert_eq!(
        call(&mut state, "editor_state", json!({}))["dirty"],
        json!(false),
        "a recolour marked the entity dirty, so a save would be offered for a change the file cannot hold"
    );

    let out = temp_path("recoloured_entity.ron");
    let saved = call(
        &mut state,
        "save_entity",
        json!({ "path": out.to_string_lossy() }),
    );
    assert!(saved["bytes"].as_u64().expect("a size") > 100);

    let reloaded = call(
        &mut fresh(),
        "load_entity",
        json!({ "path": out.to_string_lossy() }),
    );
    assert_eq!(
        reloaded["state"]["transform"]["material_id"].as_u64(),
        Some(assigned),
        "the assignment is in the entity file, and this file lost it"
    );
    let skin = reloaded["state"]["material"]["base_color"]
        .as_array()
        .expect("a colour");
    for (lane, want) in skin.iter().zip([1.0f32, 0.7, 0.75]) {
        near(
            number_at(lane, "the reloaded skin colour"),
            want,
            "loading the file must restore the preset this slot was registered with",
        );
    }
    assert_eq!(
        reloaded["state"]["material_slot_occupied"],
        json!(true),
        "the reloaded state has nothing in the slot the entity points at"
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
fn set_materials_published_schema_is_the_table_the_refusals_quote() {
    let mut state = fresh();
    let listing: Value = serde_json::from_str(&ask(
        &mut state,
        &json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}).to_string(),
    ))
    .expect("tools/list is JSON");
    let spec = listing["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .find(|tool| tool["name"] == json!("set_material"))
        .expect("set_material is listed")
        .clone();

    let schema = &spec["inputSchema"];
    assert_eq!(schema["required"], json!(["slot"]));
    assert_eq!(schema["additionalProperties"], json!(false));
    let properties = schema["properties"].as_object().expect("properties");
    let mut keys: Vec<&str> = properties.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "base_color",
            "emissive",
            "index_ior",
            "noise",
            "noise_frequency",
            "roughness",
            "slot"
        ],
        "the published fields are not the six a material carries plus a slot"
    );

    // Written as literals rather than read back out of `params`, so a table that changes an limit has to be
    // changed here too — which is the point: the schema and the refusal are one statement, and this row is
    // the third.
    assert_eq!(
        properties["base_color"],
        json!({"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3})
    );
    assert_eq!(
        properties["roughness"],
        json!({"type":"number","minimum":0.0,"maximum":1.0})
    );
    assert_eq!(
        properties["emissive"],
        json!({"type":"number","minimum":0.0}),
        "the contract states no ceiling for emissive, so the schema may not invent one"
    );
    assert_eq!(
        properties["index_ior"],
        json!({"type":"number","minimum":1.0}),
        "1.0 is air: an ior below it has no material to mean"
    );
    assert_eq!(
        properties["noise"],
        json!({"type":"string","enum":["Voronoi","Value","Perlin","None"]}),
        "the choices are spelled in `entity::NoiseKind`'s declaration order, which is what makes them \
         indices a caller never has to guess"
    );
    assert_eq!(
        properties["noise_frequency"],
        json!({"type":"number","exclusiveMinimum":0.0}),
        "a frequency of zero is a pattern that never varies, so the schema must not admit it"
    );

    // The same column the refusals are built from: exactly one field reaches a pixel, and the list of the
    // others is in declaration order.
    let state_block = call(&mut state, "editor_state", json!({}));
    assert_eq!(state_block["renderer_reads"], json!(["base_color"]));
    assert_eq!(
        state_block["authored_but_not_shaded"],
        json!([
            "roughness",
            "emissive",
            "index_ior",
            "noise",
            "noise_frequency"
        ])
    );
}
