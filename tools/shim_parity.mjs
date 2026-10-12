// Run the wasip1 engine through the hand-written shim under bare V8 - no `node:wasi` anywhere - and
// print exactly what the native boot prints, so the two can be diffed.
//
// This is the control for the browser run. The claim being set up is "these roots survive a browser
// engine", and a claim about an engine is only worth what it is compared against: if the only browser
// result came from a shim nobody had checked, a disagreement could equally mean the shim was wrong. Here
// the same shim, same module and same argv must reproduce a native run byte for byte, and only then is a
// browser's answer about the browser.
//
// Usage:
//   cargo build --release --target wasm32-wasip1 -p engine
//   node tools/shim_parity.mjs target/wasm32-wasip1/release/engine.wasm
//
// Exit code is 0 when the module ran to completion, 1 when it did not.

import { readFile } from "node:fs/promises";
import { buildWasiShim, importObject } from "./wasi_shim.mjs";

const [wasmPath] = process.argv.slice(2);
if (!wasmPath) {
  console.error("usage: node tools/shim_parity.mjs <engine.wasm>");
  process.exit(2);
}

const bytes = await readFile(wasmPath);
const module = await WebAssembly.compile(bytes);

// The shim needs the memory, the memory belongs to the instance, and the imports are needed to make the
// instance - so the reference is filled in one step after.
let instance = null;
const shim = buildWasiShim(() => instance.exports.memory);
instance = await WebAssembly.instantiate(module, importObject(shim));

try {
  instance.exports._start();
} catch (error) {
  if (!String(error?.message).startsWith("proc_exit(")) throw error;
}

process.stdout.write(shim.text());
