// Serve the wasip1 engine to a browser and let the browser print it back.
//
// Why this is a thing at all. Gate P1.5's fourth target has been reported as "three architectures plus a
// WASM engine, still not the four devices", and docs/11's own note says the M1-browser row "still needs a
// browser". A browser has no WASI, so the assumed blocker was the export-lint decision - the ban on
// hand-written `#[no_mangle]`. It is not. A WASI *command* module already exports `_start`, written by
// the Rust runtime and not by this repository, so a page can supply the 12 host functions the module
// imports, call `_start`, and read what came out. No export, no unsafe, no exception to AGENTS.md §5.
//
// What this measures and what it does not: the page runs the same module through the same shim that
// `tools/shim_parity.mjs` already proved reproduces a native boot byte for byte under V8. So the browser's
// answer is about the browser's WebAssembly implementation - its engine, on the machine hosting this
// server - and NOT about spike S3's named devices. An M1 Safari result needs a Mac with Safari; a
// Chromium-on-Windows result is a fourth *kind* of target, and the docs must say which one they are
// quoting.
//
// Usage:
//   cargo build --release --target wasm32-wasip1 -p engine
//   node tools/browser_parity_server.mjs target/wasm32-wasip1/release/engine.wasm
// then open the printed http://127.0.0.1:<port>/ URL in a browser, or drive it from a harness.
//
// localhost-only, no filesystem outside the two files it serves, and it exits when the first run is
// fetched - this is a probe, not a service.

import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { fileURLToPath } from "node:url";

const wasmPath = process.argv[2] ?? "target/wasm32-wasip1/release/engine.wasm";
const shimPath = fileURLToPath(new URL("./wasi_shim.mjs", import.meta.url));

const [wasm, shimSource] = await Promise.all([
  readFile(wasmPath),
  readFile(shimPath, "utf8"),
]);

const page = `<!doctype html>
<html lang="en">
  <head><meta charset="utf-8"><title>Merkles World - WASI command in a browser</title></head>
  <body style="font: 13px/1.5 ui-monospace, monospace; margin: 2rem;">
    <p id="status">running the wasip1 engine inside this browser engine&hellip;</p>
    <pre id="out"></pre>
    <script type="module">
      const status = document.getElementById("status");
      const out = document.getElementById("out");
      const { buildWasiShim, importObject } = await import("/wasi_shim.mjs");
      try {
        const response = await fetch("/engine.wasm");
        const bytes = await response.arrayBuffer();
        // compile then instantiate, in that order, because that is the shape tools/shim_parity.mjs uses:
        // WebAssembly.instantiate(buffer, imports) resolves to { module, instance }, not an instance, and
        // two hosts that disagree about which object they hold are not running the same comparison.
        let instance = null;
        const shim = buildWasiShim(() => instance.exports.memory);
        const compiled = await WebAssembly.compile(bytes);
        instance = await WebAssembly.instantiate(compiled, importObject(shim));
        let exit = "returned normally";
        try {
          instance.exports._start();
        } catch (error) {
          if (!String(error && error.message).startsWith("proc_exit(")) throw error;
          exit = error.message;
        }
        const text = shim.text();
        out.textContent = text;
        window.__mlws = {
          ok: text.includes("boot complete"),
          exit,
          lines: text.trim().split("\\n").length,
          userAgent: navigator.userAgent,
          text,
        };
        status.textContent = "done - " + window.__mlws.lines + " lines, " + exit;
      } catch (error) {
        window.__mlws = { ok: false, error: String(error) };
        status.textContent = "failed: " + String(error);
        out.textContent = String(error && error.stack);
      }
    </script>
  </body>
</html>`;

const server = createServer((request, response) => {
  const url = new URL(request.url, "http://127.0.0.1");
  response.setHeader("Access-Control-Allow-Origin", "*");
  if (url.pathname === "/wasi_shim.mjs") {
    // The same file on disk the Node control run imports, served as an ES module rather than inlined into
    // the page - so "the two hosts ran the same shim" is a fact about a file, not about a copy.
    response.setHeader("Content-Type", "text/javascript; charset=utf-8");
    response.end(shimSource);
    return;
  }
  if (url.pathname === "/engine.wasm") {
    response.setHeader("Content-Type", "application/wasm");
    response.end(wasm);
    return;
  }
  if (url.pathname === "/") {
    response.setHeader("Content-Type", "text/html; charset=utf-8");
    response.end(page);
    return;
  }
  response.statusCode = 404;
  response.end("not found");
});

server.listen(0, "127.0.0.1", () => {
  const { port } = server.address();
  console.log(`serving ${wasmPath} (${wasm.length} bytes) on http://127.0.0.1:${port}/`);
  console.log("open that URL in a browser, or read window.__mlws from a harness.");
});
