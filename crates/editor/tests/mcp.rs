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
