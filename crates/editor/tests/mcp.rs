//! The editor's MCP surface: every tool answers, refusals refuse, and a rendered frame follows the state.
//!
//! These call `editor::mcp::handle` directly — the transport is a loop over stdin, and the last test here
//! runs that loop as a child process to prove the pipe carries the same answers the module does.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

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
