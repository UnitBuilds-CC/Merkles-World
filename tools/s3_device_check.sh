#!/usr/bin/env bash
# Spike S3's remaining half, made a single command on whatever machine you have.
#
#   ./tools/s3_device_check.sh
#   ./tools/s3_device_check.sh --codegen
#   ./tools/s3_device_check.sh --browser
#   ./tools/s3_device_check.sh --preflight
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

# `--codegen` adds one axis: does this machine's output change when the same source is compiled at a
# different x86 codegen level (AVX2 and FMA on versus off)? It is opt-in rather than default because it
# costs a second full release build, and the slowest machine on gate P1.5's list - the N2600 - is precisely
# where an unexpected five-minute rebuild makes the harness annoying instead of useful. The published
# vectors it checks against are the same ones, so the default path remains the gate and this is extra
# reach.
CODEGEN=0
BROWSER=0
PREFLIGHT=0
TARGET=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --codegen) CODEGEN=1 ;;
    --browser) BROWSER=1 ;;
    --preflight) PREFLIGHT=1 ;;
    --target)
      shift
      TARGET="${1:-}"
      [[ -z "$TARGET" ]] && { echo "--target needs one of the names in docs/evidence.json's gate_p1_5_targets" >&2; exit 2; }
      ;;
    -h|--help) echo "usage: ./tools/s3_device_check.sh [--codegen] [--browser] [--preflight] [--target <label>]"; exit 0 ;;
    *) echo "unknown option '$1' (only --codegen, --browser, --preflight, --target and --help are recognised)" >&2; exit 2 ;;
  esac
  shift
done

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
# The header lines are what docs/11 asks a device run to paste, so they should read cleanly: the previous
# form printed `rustc -vV` with every line re-prefixed, which duplicated the host line and emitted
# "rustc: binary: rustc". Version and host are the two that identify a machine.
if command -v rustc >/dev/null 2>&1; then
  echo "rustc: $(rustc --version 2>/dev/null)"
  echo "host:  $(rustc -vV 2>/dev/null | sed -n 's/^host: //p')"
else
  echo "rustc: not found"
fi
# The commit and the hasher's pinned version belong in the record. Gate P1.5's row asks which machine
# reproduced the vectors, and a machine without a code state is only half an answer: the same Celeron on a
# different commit is a different claim, and the blake3 line is what an ISA-portability question turns on.
if command -v git >/dev/null && git -C "$repo_root" rev-parse --git-dir >/dev/null 2>&1; then
  GIT_SHA="$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "commit: $GIT_SHA ($(git -C "$repo_root" describe --always --dirty 2>/dev/null || echo '?'))"
else
  GIT_SHA="unknown"
fi
BLAKE3_VERSION="$(awk '/^name = "blake3"$/{getline; gsub(/^version = |"/, "", $0); print; exit}' "$repo_root/Cargo.lock" 2>/dev/null || true)"
echo "blake3: ${BLAKE3_VERSION:-not found in Cargo.lock}"
echo "// expected (from docs/03-state-ledger.md)"
echo "//   EPOCH_0   $expected_epoch"
echo "//   REGIONS   $expected_regions"
echo "//   REGION_0  $expected_region0"
echo "//   SLOT_TABLE $expected_slots"
echo

# --- preflight: what CAN this machine check, before anything expensive starts ---------------------
# The N2600 is the slowest box on gate P1.5's list, and a release build is the expensive part of this
# command. Discovering afterwards that there is no node, or that the WASI target was never installed, spends
# the compile to learn the legs will be skipped anyway - so say it first, in two seconds, with the fix.
if (( PREFLIGHT )); then
  echo "// preflight (nothing built)"
  ok=1
  if command -v cargo >/dev/null 2>&1; then
    echo "//   cargo: $(cargo -V)  -> native leg possible"
  else
    echo "//   cargo: MISSING -> the native leg, which is the gate, cannot run at all. Install Rust."
    ok=0
  fi
  if command -v rustc >/dev/null 2>&1; then
    echo "//   rustc: $(rustc --version)"
    rustup target list --installed 2>/dev/null | grep -q wasm32-wasip1 \
      && echo "//   wasm32-wasip1: installed -> WASI leg possible" \
      || echo "//   wasm32-wasip1: MISSING -> install with 'rustup target add wasm32-wasip1'"
  fi
  command -v node >/dev/null 2>&1 \
    && echo "//   node: $(node --version) -> browser leg possible if a browser is present" \
    || echo "//   node: MISSING -> the browser leg and the WASI leg cannot run"
  found=""
  for candidate in "google-chrome" "chromium" "/c/Program Files/Google/Chrome/Application/chrome.exe" \
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"; do
    if [[ -x "$candidate" ]] || command -v "$candidate" >/dev/null 2>&1; then found="$candidate"; break; fi
  done
  if [[ -n "$found" ]]; then
    echo "//   browser binary: $found"
  else
    echo "//   browser binary: none of chrome/chromium found"
  fi
  if [[ "$(uname -s)" == "Darwin" ]]; then
    command -v safaridriver >/dev/null 2>&1 \
      && echo "//   safaridriver: present (needs Safari's Develop > Allow Remote Automation once)" \
      || echo "//   safaridriver: MISSING - JavaScriptCore, the engine M1-browser names, cannot be driven"
  fi
  (command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1) \
    && echo "//   sha256: available -> whole-output digest will be compared" \
    || echo "//   sha256: MISSING -> the output cannot be hashed, so the manifest comparison is lost"
  echo "//   vectors read from docs/03: EPOCH_0, REGIONS, SLOT_TABLE, REGION_0 (all four extracted above)"
  if (( ok )); then
    echo "RESULT: preflight found enough to run the gate's native leg here."
    exit 0
  fi
  exit 2
fi

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
      WASI_RAN=1
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

if (( CODEGEN )); then
  echo
  echo "// codegen axis (--codegen)"
  if [[ "$(uname -m)" != x86_64 ]]; then
    echo "//   SKIPPED: this axis is x86 codegen levels. On $(uname -m) the ISA spread is what the WASM"
    echo "//   leg above already covers, so nothing is being quietly left unchecked here."
  elif bash tools/cpu_codegen_parity.sh; then
    CODEGEN_RAN=1
    echo "//   this machine's output does not move across x86 codegen levels"
  else
    codegen_status=$?
    if [[ $codegen_status -eq 2 ]]; then
      echo "//   codegen axis could not run (status 2: toolchain or empty build). Not a gate result."
    else
      echo "//   CODEGEN AXIS DISAGREES: same source, different x86 ISA level. On a machine without AVX2"
      echo "//   and FMA (the N2600) versus one with them (the RTX 3060 box), that is a P1.5 finding."
      fail=1
    fi
  fi
fi

if (( BROWSER )); then
  echo
  # Gate P1.5's fourth named target is a *browser*, and until now the device harness had no leg for one:
  # the CI jobs drive both engines (`browser-execute` on Chromium-family, `browser-jsc` on Safari), so the
  # capability was proven and the missing thing was a command a person holding an M1 Mac could run. Opt-in
  # for the same reason as --codegen: it needs node, the WASI target and a browser binary, and a leg that
  # cannot run has to say so rather than being quietly absent from the record. The verdict is against THIS
  # machine's native boot, so it does not depend on the published digest being current.
  echo "// browser leg (--browser)"
  printf '%s\n' "$output" > target/s3_native.txt
  if MLWS_NATIVE_OUTPUT=target/s3_native.txt bash tools/browser_parity_check.sh; then
    BROWSER_RAN=1
    echo "//   browser leg recorded above agrees, or reports exactly which engines it skipped"
  else
    browser_status=$?
    if [[ $browser_status -eq 2 ]]; then
      echo "//   BROWSER LEG DID NOT RUN (status 2: no node, no module, no reference or no drivable"
      echo "//   engine). This is not a gate result and gate P1.5's browser clause stays open."
    else
      echo "//   BROWSER LEG DISAGREES with this machine's native build. That is a P1.5 finding."
      fail=1
    fi
  fi
fi

echo
# One line that carries everything docs/11's S3 row needs. The gate has already been told "the hardware
# failed" on a machine nobody could identify afterwards, so the identity travels with the verdict rather
# than living in whoever remembers to scroll up. Digests stay where they are printed above - this line
# deliberately restates no hash, so there is no second value to drift.
# --target turns this machine's run into one row of gate P1.5's device register. The register is data
# (docs/evidence.json's gate_p1_5_targets) and tools/check_docs.mjs refuses a row that claims agreement
# without the current boot digest, the commit and a date - so the record this prints is shaped to satisfy
# that check rather than to be retyped into prose. The label is checked against the register, because a
# typo here is a fifth target nobody owns.
print_record() {
  local verdict="$1"
  local known
  known="$(node -e 'const r=require("./docs/evidence.json").gate_p1_5_targets;console.log(r.map(e=>e.target).join("|"))' 2>/dev/null)"
  if [[ -z "$known" ]]; then
    echo "// --target $TARGET: the register could not be read from docs/evidence.json - no record printed" >&2
    return
  fi
  if [[ ! "|$known|" == *"|$TARGET|"* ]]; then
    echo "// --target '$TARGET' is not a row of gate P1.5's register. Known targets: ${known//|/, }" >&2
    # Exit, do not merely note it: this runs after the verdict line has already printed AGREES, so a mistyped
    # label would otherwise leave a run that produced no record reporting exit 0 - and the whole reason the
    # label is checked against the register is that a fifth target nobody owns must not be acceptible input.
    exit 2
  fi
  local legs="native"
  # Which legs ACTUALLY ran, tracked by each branch rather than inferred from the flags: a record that names
  # a leg nobody ran is the vacuous-green shape this project keeps tripping over, and an option flag whose
  # off value is 0 is still a non-empty string, so `${BROWSER:+,…}` would claim it everywhere.
  (( ${WASI_RAN:-0} )) && legs="$legs,wasi"
  (( ${BROWSER_RAN:-0} )) && legs="$legs,browser"
  (( ${CODEGEN_RAN:-0} )) && legs="$legs,codegen"
  echo "// register record for $TARGET - paste this object into docs/evidence.json's gate_p1_5_targets:"
  # Say it here rather than at paste time: the checker refuses an M1-browser row whose legs do not include a
  # browser, and the way that happens in practice is Safari's remote automation being switched off, which the
  # leg reports as a skip and this run would otherwise carry on past.
  if [[ "$TARGET" == "M1-browser" ]] && ! [[ ",$legs," == *",browser,"* ]]; then
    echo "//   WARNING: legs is \"$legs\" with no browser leg, and gate_p1_5_targets' M1-browser row is"
    echo "//   required by tools/check_docs.mjs to have driven one. This record will be refused: enable"
    echo "//   Safari's Develop > Allow Remote Automation and re-run, rather than pasting a row that names"
    echo "//   a target whose engine never ran."
  fi
  echo "//   the label is a claim about this machine, not a fact the harness can check, so the record"
  echo "//   carries the host and os beside the verdict and the register entry has to agree with them."
  printf '{"target":"%s","status":"%s","boot_sha256":"%s","commit":"%s","recorded":"%s","host":"%s","os":"%s","legs":"%s","command":"bash tools/s3_device_check.sh %s"}\n' \
    "$TARGET" "$verdict" "${digest:-none}" "${GIT_SHA:-unknown}" "$(date -u +%F)" \
    "$(hostname 2>/dev/null || uname -n | tr -d '"')" "$(uname -srm | tr -d '"')" "$legs" \
    "--target $TARGET$( ((CODEGEN)) && echo ' --codegen')$( ((BROWSER)) && echo ' --browser')"
}

SUMMARY="// SUMMARY: $(uname -srm) | commit $GIT_SHA | blake3 ${BLAKE3_VERSION:-?} | $(git -C "$repo_root" rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
if (( fail )); then
  echo "$SUMMARY | verdict: DIVERGES from the published vectors"
  echo "RESULT: this machine does NOT reproduce the published vectors. That is gate-relevant: record the"
  echo "        uname line, the rustc host triple and the digests above against spike S3 in docs/11."
  [[ -n "$TARGET" ]] && print_record diverges
  exit 1
fi

echo "$SUMMARY | verdict: AGREES"
echo "RESULT: this machine reproduces every published vector."
echo "        Paste the machine line and the digests into docs/11's S3 row - the gate needs the name of"
echo "        the hardware, not just the fact that something passed."
[[ -n "$TARGET" ]] && print_record agrees
exit 0
