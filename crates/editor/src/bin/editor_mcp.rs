//! The editor's MCP transport: a line of JSON-RPC in, its reply out, over a pipe.
//!
//! `cargo run -p editor --bin editor_mcp`
//!
//! All the behaviour is in `editor::mcp`, which the tests call directly; this is the loop an MCP client
//! is pointed at. Nothing here can render without being asked, and every reply is flushed before the next
//! line is read, because a client that is waiting for a response and gets a buffered one is debugging a
//! transport rather than an editor.

use std::io::{self, BufRead, Write};

fn main() {
    // Frames go to a directory named for this process (`editor::mcp::dump_dir`), so the path a caller is
    // handed can only be a picture this session marched.
    let mut state = editor::EditorState::new();

    let stdin = io::stdin();
    let mut out = io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = editor::mcp::handle(&mut state, &line) {
            if writeln!(out, "{reply}").is_err() {
                break;
            }
            if out.flush().is_err() {
                break;
            }
        }
    }
}
