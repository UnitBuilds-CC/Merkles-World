#!/usr/bin/env bash
# Spike S3's remaining half, made a single command on whatever machine you have.
#
#   ./tools/s3_device_check.sh
#
# Gate P1.5 asks for byte-identical epoch roots on four *named* targets - N2600, RTX 3060, Linux-Mesa and
# an M1 browser - and CI's three runners are none of them. The blocker was never a missing test, it was
# that running the test on real hardware meant remembering which hashes to compare and how to reach the
# WASM leg. So this script does both, and reads the expected values out of docs/03 rather than embedding
# copies of them: the published table stays the one source of truth, and if the binary disagrees with it
# this exits non-zero whether or not anyone has updated the doc.
#
# Exit status: 0 = this machine agrees with the published vectors; 1 = it does not, which is a gate
# finding, not a script bug; 2 = the build or the harness could not run at all.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
doc="$repo_root/docs/03-state-ledger.md"
cd "$repo_root" || exit 2

if [[ ! -f "$doc" ]]; then
  echo "cannot find $doc - run this from a clone of the repository" >&2
  exit 2
fi

# --- the published vectors, lifted from doc 03's table so there is no second copy to drift -------
extract() {
  # $1 = the name in the first table column; prints the 64-hex digest from that row, or nothing.
  awk -F'|' -v name="$1" '
    $2 ~ ("^[ ]*`?" name "`?[ ]*$") {
      gsub(/[ `]/, "", $3)
      if ($3 ~ /^[0-9a-f]{64}$/) { print $3; exit }
    }
  ' "$doc"
}

expected_epoch=$(extract EPOCH_0)
expected_regions=$(extract REGIONS)
expected_region0=$(extract REGION_0)
expected_slots=$(extract SLOT_TABLE)

missing=()
[[ -z "$expected_epoch"   ]] && missing+=("EPOCH_0")
[[ -z "$expected_regions" ]] && missing+=("REGIONS")
[[ -z "$expected_region0" ]] && missing+=("REGION_0")
[[ -z "$expected_slots"   ]] && missing+=("SLOT_TABLE")
if (( ${#missing[@]} )); then
  echo "doc 03's vector table did not yield: ${missing[*]}" >&2
  echo "the row format is '| \`NAME\` | \`64-hex\` | ... |' - check before trusting a comparison" >&2
  exit 2
fi

echo "// machine"
uname -a || true
command -v rustc >/dev/null && rustc -vV | sed -n 's/^/rustc: /p; s/host: /host: /p' || echo "rustc: not found"
echo "// expected (from docs/03-state-ledger.md)"
echo "//   EPOCH_0   $expected_epoch"
echo "//   REGIONS   $expected_regions"
echo "//   REGION_0  $expected_region0"
echo "//   SLOT_TABLE $expected_slots"
echo

if ! command -v cargo >/dev/null; then
  echo "cargo is not on PATH; the native leg cannot be built" >&2
  exit 2
fi

echo "// building the engine (release)"
if ! cargo build --release --quiet -p engine; then
  echo "the release build failed on this machine - that is itself an S3 finding, report it" >&2
  exit 2
fi

output="$(cargo run --release --quiet -p engine 2>/dev/null)"
if [[ -z "$output" ]]; then
  echo "the boot produced no output" >&2
  exit 2
fi

echo "// native leg"
fail=0
check() {
  # $1 = label, $2 = expected digest, $3 = the substring of the boot output that carries it
  local found
  found="$(printf '%s\n' "$3" | grep -o '[0-9a-f]\{64\}' | head -1)"
  if [[ -z "$found" ]]; then
    echo "  $1: the boot printed no 64-hex digest to compare"
    fail=1
  elif [[ "$found" == "$2" ]]; then
    echo "  $1: agrees ($found)"
  else
    echo "  $1: MISMATCH - printed $found, doc 03 publishes $2"
    fail=1
  fi
}
check "EPOCH_0   " "$expected_epoch"   "$(printf '%s\n' "$output" | grep 'epoch root')"
check "REGIONS   " "$expected_regions" "$(printf '%s\n' "$output" | grep 'region list')"
check "SLOT_TABLE" "$expected_slots"   "$(printf '%s\n' "$output" | grep 'slot table')"
check "REGION_0  " "$expected_region0" "$(printf '%s\n' "$output" | grep 're-rooted to')"

# the whole output, hashed: two machines that agree on the digests but disagree on anything else
# printed would still be a determinism finding, so the artifact compared is the full boot output.
digest=""
if command -v sha256sum >/dev/null; then
  digest="$(printf '%s\n' "$output" | sha256sum | cut -d' ' -f1)"
elif command -v shasum >/dev/null; then
  digest="$(printf '%s\n' "$output" | shasum -a 256 | cut -d' ' -f1)"
fi
expected_output_digest="$(node -e 'const fs=require("fs");console.log(JSON.parse(fs.readFileSync("docs/evidence.json","utf8")).claims.boot_output_sha256.value)' 2>/dev/null || true)"
echo "// full output digest: ${digest:-sha256sum unavailable}"
if [[ -n "$digest" && -n "$expected_output_digest" ]]; then
  if [[ "$digest" == "$expected_output_digest" ]]; then
    echo "//   agrees with docs/evidence.json ($expected_output_digest)"
  else
    echo "//   differs from docs/evidence.json (${expected_output_digest}) - either this machine prints"
    echo "//   something the harness does not expect, or the manifest is stale for the current trunk"
    fail=1
  fi
fi

# --- optional WASM leg: the same binary under a WASM engine, if the toolchain is here -------------
echo
if command -v node >/dev/null && rustup target list --installed 2>/dev/null | grep -q wasm32-wasip1; then
  echo "// wasm leg (node + wasm32-wasip1 present)"
  native_txt=target/s3_native.txt
  wasm_txt=target/s3_wasm.txt
  printf '%s
' "$output" > "$native_txt"
  if cargo build --release --quiet -p engine --target wasm32-wasip1      && node --experimental-wasi-unstable-preview1 tools/wasm_parity.mjs           target/wasm32-wasip1/release/engine.wasm > "$wasm_txt" 2> target/s3_wasm.err; then
    if diff -u "$native_txt" "$wasm_txt"; then
      echo "//   wasm leg agrees with this machine's native build, line for line"
    else
      echo "//   WASM LEG DISAGREES - same binary, different engine. That is a P1.5 finding."
      fail=1
    fi
  else
    echo "//   wasm leg could not run (build, toolchain or shim). Not a gate result; the error is in"
    echo "//   target/s3_wasm.err"
    tail -3 target/s3_wasm.err 2>/dev/null | sed 's/^/     /'
    fail=1
  fi
else
  echo "// wasm leg: SKIPPED (needs node and the wasm32-wasip1 target)"
  echo "//   install with: rustup target add wasm32-wasip1"
fi

echo
if (( fail )); then
  echo "RESULT: this machine does NOT reproduce the published vectors. That is gate-relevant: record the"
  echo "        uname line, the rustc host triple and the digests above against spike S3 in docs/11."
  exit 1
fi
echo "RESULT: this machine reproduces every published vector."
echo "        Paste the machine line and the digests into docs/11's S3 row - the gate needs the name of"
echo "        the hardware, not just the fact that something passed."
exit 0
