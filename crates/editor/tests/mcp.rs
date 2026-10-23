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
