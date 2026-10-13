#!/usr/bin/env bash
# Is the boot's output invariant to WebAssembly SIMD? Check, don't assume.
#
#   bash tools/wasm_simd_parity.sh
#
# Gate P1.5 is about one epoch root surviving four different machines, and one of the axes those machines
# genuinely differ on is WebAssembly SIMD: a browser or runtime built with `+simd128` compiles `blake3`'s
# compression and the SDF evaluator's float loops to different instructions than one without it. That is a
# real risk to a hashed value, and it is cheap to rule out, so this script does: build the engine natively,
# build it twice for `wasm32-wasip1` (SIMD on, SIMD off), run each module, and require the boot blocks to be
# byte-identical - every line from the header through `// boot complete`, hashed values and presentation
# numbers alike.
#
# Exit codes follow the repo's convention for these harnesses, because a missing dependency must never read
# as a pass:
#   0  all three agree, byte for byte
#   1  they disagree - which is gate-relevant, and means a hashed value depends on codegen
#   2  the harness could not run (no node, no wasm target, a build that failed)

set -euo pipefail

WORK="target/wasm-simd-parity"
mkdir -p "$WORK"
NATIVE="$WORK/native.txt"
SIMD_WASM="$WORK/engine_simd.wasm"
PLAIN_WASM="$WORK/engine_nosimd.wasm"
SIMD_LOG="$WORK/wasm_simd.txt"
PLAIN_LOG="$WORK/wasm_nosimd.txt"

command -v node >/dev/null 2>&1 || { echo "FAIL node is not installed; the WASM legs cannot be executed"; exit 2; }
if ! rustup target list --installed 2>/dev/null | grep -q '^wasm32-wasip1$'; then
  echo "SKIPPED wasm32-wasip1 is not installed (rustup target add wasm32-wasip1)"
  exit 2
fi

echo "// native boot, to compare against"
cargo run --release -q -p engine > "$NATIVE"

echo "// module with +simd128"
RUSTFLAGS="-Ctarget-feature=+simd128" cargo build -q --release --target wasm32-wasip1 -p engine
cp target/wasm32-wasip1/release/engine.wasm "$SIMD_WASM"

echo "// module with -simd128"
RUSTFLAGS="-Ctarget-feature=-simd128" cargo build -q --release --target wasm32-wasip1 -p engine
cp target/wasm32-wasip1/release/engine.wasm "$PLAIN_WASM"

# The parity runner prints the module's own stdout, so the native file is passed to it as the reference and
# the boot block is sliced out of the log below rather than trusted to whatever the runner echoed.
MLWS_NATIVE_OUTPUT="$NATIVE" node tools/wasm_parity.mjs "$SIMD_WASM" > "$SIMD_LOG" 2>&1
MLWS_NATIVE_OUTPUT="$NATIVE" node tools/wasm_parity.mjs "$PLAIN_WASM" > "$PLAIN_LOG" 2>&1

cat > "$WORK/compare.mjs" <<'JS'
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";

// Slice the boot block out of a log: from the native build's own first line through the closing marker.
// The WASM logs carry the parity runner's chatter, so anchoring on content rather than line numbers is the
// only robust way to compare like with like.
function bootBlock(path, firstLine) {
  const lines = readFileSync(path, "utf8").split(/\r?\n/);
  const start = lines.indexOf(firstLine);
  const end = lines.findIndex((l) => l.startsWith("// boot complete"));
  if (start === -1 || end === -1) return null;
  return lines.slice(start, end + 1).join("\n");
}

const [nativePath, ...others] = process.argv.slice(2);
const nativeLines = readFileSync(nativePath, "utf8").split(/\r?\n/);
const firstLine = nativeLines[0];
const native = bootBlock(nativePath, firstLine);
if (native === null) {
  console.error("FAIL the native output has no recognizable boot block");
  process.exit(2);
}
const sha = (s) => createHash("sha256").update(s).digest("hex");
const want = sha(native);
let ok = true;
console.log(`// native boot block: ${native.split("\n").length} lines, sha256 ${want.slice(0, 16)}...`);
for (const p of others) {
  const block = bootBlock(p, firstLine);
  if (block === null) {
    console.error(`FAIL ${p}: no boot block found - the module never finished its boot`);
    ok = false;
    continue;
  }
  const got = sha(block);
  const same = got === want;
  console.log(`// ${p}: ${block.split("\n").length} lines, sha256 ${got.slice(0, 16)}... ${same ? "equal to native" : "DIFFERS FROM NATIVE"}`);
  if (!same) {
    ok = false;
    const a = block.split("\n");
    const b = native.split("\n");
    for (let i = 0; i < Math.max(a.length, b.length); i += 1) {
      if (a[i] !== b[i]) console.error(`   line ${i + 1}:\n     wasm:   ${a[i]}\n     native: ${b[i]}`);
    }
  }
}
if (!ok) {
  console.error("RESULT: a hashed or printed value depends on WebAssembly SIMD codegen. Gate P1.5 cannot be");
  console.error("        met by machines that differ on that feature; treat this as a determinism defect.");
  process.exit(1);
}
console.log("RESULT: the boot output is invariant to WASM SIMD codegen, hashed lines included.");
JS

node "$WORK/compare.mjs" "$NATIVE" "$SIMD_LOG" "$PLAIN_LOG"
