// gate P1.5's fourth target family: run the engine inside a WASM engine.
//
// Why this exists. docs/11 has said "WASM is compiled but never executed" ever since the export-lint
// discovery, and the fix was assumed to need `#[no_mangle]` or a `wit-bindgen` binding - both of which
// this workspace forbids. It does not. A WASI *command* module exports `_start` because the Rust runtime
// writes it, not because anything here wrote an unsafe attribute, so the ban is never reached: build for
// `wasm32-wasip1`, hand the module to a WASI host, and read what it printed.
//
// The caller diffs. This script lets the module's own stdout flow through, so the comparison is an
// ordinary `diff` against a host run - one fewer place for a capture bug to hide.
//
// What agreement is worth: the boot prints the epoch root, the region list and the slot table, which are
// values a peer must agree with, so identical output is a claim about hashed state - gate P1.5's shape.
// What it is not worth: the same lines also carry the marched `t` and the raster's evaluation counts,
// which are `f32` presentation values that match only because the march uses add, multiply and `sqrt`.
// They must never be quoted as gate evidence, and a GPU path may legitimately move them (ADR-017,
// gate R4 is informational by policy).
//
// And this is not spike S3's list of devices (N2600 / RTX 3060 / Linux-Mesa / M1-browser). It is V8's
// WebAssembly on whichever machine runs the script - a fourth *kind* of target, not the fourth device.
// The M1-browser row still needs a browser.
//
// Usage:
//   cargo build --target wasm32-wasip1 -p engine
//   node --experimental-wasi-unstable-preview1 tools/wasm_parity.mjs \
//       target/wasm32-wasip1/debug/engine.wasm

import { readFile } from "node:fs/promises";
import { WASI } from "node:wasi";

const [wasmPath] = process.argv.slice(2);
if (!wasmPath) {
  console.error("usage: node tools/wasm_parity.mjs <engine.wasm>");
  process.exit(2);
}

const bytes = await readFile(wasmPath);
const module = await WebAssembly.compile(bytes);
const wasi = new WASI({
  version: "preview1",
  // The boot writes nothing, so the sandbox gets nothing: no preopens, no inherited environment. A
  // program whose output is gate evidence should not be able to read its way into one.
  args: ["engine"],
  env: {},
  preopens: {},
  returnOnExit: true,
});

const imports = typeof wasi.getImportObject === "function" ? wasi.getImportObject() : wasi.wasiImport;
const instance = await WebAssembly.instantiate(module, imports);
// Node's WASI is still experimental, and its two entry points differ by version: `start` for a command
// module, `initialize` plus a manual `_start` for older builds.
if (typeof wasi.start === "function") {
  wasi.start(instance);
} else {
  wasi.initialize(instance);
  instance.exports._start();
}
