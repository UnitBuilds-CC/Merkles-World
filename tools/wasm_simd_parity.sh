#!/usr/bin/env bash
# Is the boot's output invariant to WebAssembly SIMD? Check, don't assume.
#
#   bash tools/wasm_simd_parity.sh                      # full three-way check
#   bash tools/wasm_simd_parity.sh --native host.txt --only +simd128
#
# Gate P1.5 is about one epoch root surviving four different machines, and one axis those machines genuinely
# differ on is WebAssembly SIMD: a module compiled with `+simd128` turns blake3's compression and the
# evaluator's float loops into different instructions than one without it. That is a real risk to a hashed
# value, and it is cheap to rule out, so this script does: build the engine natively, build it for
# `wasm32-wasip1` with each feature setting, run each module, and require the boot blocks to be
# byte-identical - every line from the header through `// boot complete`, hashed values and presentation
# numbers alike.
#
# The two flags exist because of where this runs. The full check belongs in the single-runner `wasm` job; the
# three-architecture `wasm-execute` matrix already proves the default module equals that runner's native
# build, so per OS only one extra module is worth paying for - if `+simd128` equals the same native output,
# SIMD-invariance follows without rebuilding both variants on every runner.
#
# Exit codes follow the repo's convention for these harnesses, because a missing dependency must never read
# as a pass:
#   0  everything compared, all equal, byte for byte
#   1  a disagreement - gate-relevant, and it means a printed or hashed value depends on codegen
#   2  the harness could not run (no node, no wasm target, a failed build, an unusable reference file)

set -euo pipefail

REFERENCE=""
VARIANTS="+simd128 -simd128"
while [ $# -gt 0 ]; do
  case "$1" in
    --native) REFERENCE="${2:-}"; shift 2 ;;
    --only)  VARIANTS="${2:-}"; shift 2 ;;
    *) echo "usage: $0 [--native FILE] [--only +simd128|-simd128]"; exit 2 ;;
  esac
done

for V in $VARIANTS; do
  case "$V" in +simd128|-simd128) ;; *) echo "FAIL --only wants +simd128 or -simd128, got '$V'"; exit 2 ;; esac
done

command -v node >/dev/null 2>&1 || { echo "FAIL node is not installed; the WASM legs cannot be executed"; exit 2; }
if ! rustup target list --installed 2>/dev/null | grep -q '^wasm32-wasip1$'; then
  echo "SKIPPED wasm32-wasip1 is not installed (rustup target add wasm32-wasip1)"
  exit 2
fi

WORK="target/wasm-simd-parity"
mkdir -p "$WORK"
if [ -n "$REFERENCE" ]; then
  # Trust but check: a reference that never booted would make every comparison vacuously true or unfalsifiable.
  grep -q '^// boot complete' "$REFERENCE" || { echo "FAIL reference '$REFERENCE' has no boot block"; exit 2; }
  cp "$REFERENCE" "$WORK/native.txt"
  echo "// reference: $REFERENCE (no native build run)"
else
  echo "// native boot, to compare against"
  cargo run --release -q -p engine > "$WORK/native.txt"
fi

LOGS=""
for V in $VARIANTS; do
  # Tagged by name, not by stripping the sign: `tr -d '+-'` collapses +simd128 and -simd128 onto the same
  # file, which made the full check compare one module against itself and quietly pass. The tags must differ
  # or the second build overwrites the first and the comparison is vacuous.
  case "$V" in +simd128) TAG="simd_on" ;; -simd128) TAG="simd_off" ;; *) TAG="$V" ;; esac
  echo "// module with $V"
  RUSTFLAGS="-Ctarget-feature=$V" cargo build -q --release --target wasm32-wasip1 -p engine
  cp target/wasm32-wasip1/release/engine.wasm "$WORK/engine_$TAG.wasm"
  # The parity runner prints the module's own stdout; the boot block is sliced out below rather than trusting
  # whatever the runner echoed about its own comparison.
  MLWS_NATIVE_OUTPUT="$WORK/native.txt" node tools/wasm_parity.mjs "$WORK/engine_$TAG.wasm" > "$WORK/wasm_$TAG.txt" 2>&1
  LOGS="$LOGS $WORK/wasm_$TAG.txt"
done

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
  console.error("FAIL the reference output has no recognizable boot block");
  process.exit(2);
}
const sha = (s) => createHash("sha256").update(s).digest("hex");
const want = sha(native);
let ok = true;
console.log(`// reference boot block: ${native.split("\n").length} lines, sha256 ${want.slice(0, 16)}...`);
for (const p of others) {
  const block = bootBlock(p, firstLine);
  if (block === null) {
    console.error(`FAIL ${p}: no boot block found - the module never finished its boot`);
    ok = false;
    continue;
  }
  const got = sha(block);
  const same = got === want;
  console.log(
    `// ${p}: ${block.split("\n").length} lines, sha256 ${got.slice(0, 16)}... ${same ? "equal to reference" : "DIFFERS FROM REFERENCE"}`,
  );
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

node "$WORK/compare.mjs" "$WORK/native.txt" $LOGS
