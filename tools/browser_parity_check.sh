#!/usr/bin/env bash
# Gate P1.5's browser clause, run on the machine that the clause names rather than on a runner.
#
#   bash tools/browser_parity_check.sh [path/to/engine.wasm]
#
# CI already drives both engines (the `browser-execute` and `browser-jsc` jobs), so nothing here is a new
# claim - what is missing until now is that a person holding an Apple-silicon Mac had no command. The gate
# says "an M1 browser", and the device harness stopped at the native and WASI legs, which are both not a
# browser.
#
# What it checks: that a browser engine, hosting the same WASI command module through the same hand-written
# shim, prints the boot this machine's native build printed. The comparison is against THIS machine's own
# output, so the verdict does not depend on the published digest having been updated in step with trunk.
#
# Exit status: 0 = every engine that could run agreed; 1 = some engine's run disagrees, which is a gate
# finding; 2 = the harness could not run at all (no node, no module, no WASI target), which is never a pass.
#
# A skipped engine is reported as skipped and named in the summary line. That distinction is the point of
# the script: a leg that quietly did not run is how a gate gets closed by an empty comparison, and the
# project has caught itself doing exactly that three times in this session.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root" || exit 2

WASM="${1:-target/wasm32-wasip1/release/engine.wasm}"
PORT="${MLWS_PORT:-8137}"
BASE="http://127.0.0.1:${PORT}"
REF="${MLWS_NATIVE_OUTPUT:-target/s3_native.txt}"
EMPTY_SHA="e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"

sha_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

if ! command -v node >/dev/null 2>&1; then
  echo "BROWSER LEG: cannot run - node is not on PATH (the parity server is a node script)"
  echo "SUMMARY: browser legs: NOT RUN (no node)"
  exit 2
fi
if [[ ! -f "$WASM" ]]; then
  echo "BROWSER LEG: cannot run - no module at $WASM"
  echo "  build it first: cargo build --release -p engine --target wasm32-wasip1"
  echo "SUMMARY: browser legs: NOT RUN (no module)"
  exit 2
fi
if [[ ! -f "$REF" ]]; then
  echo "BROWSER LEG: cannot run - no native reference at $REF"
  echo "  the reference is this machine's own boot output; write it with"
  echo "  'cargo run --release -p engine > $REF' or set MLWS_NATIVE_OUTPUT"
  echo "SUMMARY: browser legs: NOT RUN (no reference)"
  exit 2
fi
# "Two runs agree" is not "the same complete boot": a native build that died halfway would leave every leg
# agreeing on truncated output, and a truncated file still has a hash.
if ! grep -q '^// boot complete' "$REF"; then
  echo "BROWSER LEG: cannot run - the native reference in $REF never reached its closing marker"
  echo "SUMMARY: browser legs: NOT RUN (reference did not boot)"
  exit 2
fi

expected="$(sha_of "$REF")"
if [[ -z "$expected" || "$expected" == "$EMPTY_SHA" ]]; then
  echo "BROWSER LEG: cannot run - no sha256 tool, or the reference hashes to the empty-input digest"
  echo "SUMMARY: browser legs: NOT RUN (no usable reference hash)"
  exit 2
fi
echo "// browser leg: reference $REF sha256 $expected ($(wc -l < "$REF") lines)"

# The module is rebuilt by default, because the first run of this leg on this machine compared a fresh
# native boot against a module left over from an earlier commit: the disagreement was real arithmetic on
# stale bytes, and it looked exactly like the P1.5 finding the leg exists to catch. A harness must not be
# able to run old code and report it as a device disagreeing.
if [[ "${MLWS_SKIP_BUILD:-0}" != 1 ]] && command -v cargo >/dev/null 2>&1; then
  if ! cargo build --release --quiet -p engine --target wasm32-wasip1; then
    echo "BROWSER LEG: cannot run - the wasm32-wasip1 build failed (target installed? rustup target add wasm32-wasip1)"
    echo "SUMMARY: browser legs: NOT RUN (module build failed)"
    exit 2
  fi
fi
module_sha="$(sha_of "$WASM")"
echo "// browser leg: module $WASM sha256 ${module_sha:-?}, $(wc -c < "$WASM") bytes"

ran=0
fail=0
verdicts=""

# --- Safari / JavaScriptCore: the non-V8 engine, and the one gate P1.5's fourth target is about ------
if [[ "$(uname -s)" == "Darwin" ]] && command -v safaridriver >/dev/null 2>&1; then
  echo
  echo "// Safari / JavaScriptCore"
  MLWS_NATIVE_OUTPUT="$REF" MLWS_PORT="$PORT" bash tools/safari_parity.sh "$WASM"
  status=$?
  if [[ $status -eq 0 ]]; then
    ran=1
    verdicts="${verdicts}safari=OK "
  else
    ran=1
    fail=1
    verdicts="${verdicts}safari=DIVERGES "
  fi
else
  if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "// Safari / JavaScriptCore: SKIPPED (not macOS - JavaScriptCore ships as Safari)"
    verdicts="${verdicts}safari=SKIPPED(not-macos) "
  else
    echo "// Safari / JavaScriptCore: SKIPPED (safaridriver not installed)"
    verdicts="${verdicts}safari=SKIPPED(no-driver) "
  fi
fi

# --- Chromium-family: V8 in a browser rather than V8 in node ---------------------------------------
browser=""
for candidate in \
  "google-chrome" "google-chrome-stable" "chromium" "chromium-browser" \
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  "/c/Program Files/Google/Chrome/Application/chrome.exe" \
  "/c/Program Files (x86)/Google/Chrome/Application/chrome.exe" \
  "/c/Program Files (x86)/Microsoft/Edge/Application/msedge.exe" \
  "/c/Program Files/Microsoft/Edge/Application/msedge.exe"; do
  if [[ -x "$candidate" ]] || command -v "$candidate" >/dev/null 2>&1; then
    browser="$candidate"
    break
  fi
done
if [[ -z "$browser" ]]; then
  echo "// Chromium-family: SKIPPED (no chrome/chromium/edge binary found)"
  echo "//   install any Chromium-family browser, or set nothing - the Safari leg above is the one the"
  echo "//   gate names, and a Chromium result on the M1 would be V8 again."
  verdicts="${verdicts}chromium=SKIPPED(no-binary) "
else
  echo
  echo "// Chromium-family: $browser"
  # The page POSTs its digest to the harness's own server and this polls for it, because a DOM dump on a
  # loaded machine can be captured before the fetch and compile have finished. A page that reports cannot
  # be mistaken for a page that is merely slow.
  node tools/browser_parity_server.mjs "$WASM" --port "$PORT" >target/browser-leg-server.log 2>&1 &
  server=$!
  trap 'kill "$server" 2>/dev/null || true' EXIT
  up=0
  for _ in $(seq 1 40); do
    if curl -sS --max-time 2 "$BASE/" -o /dev/null 2>/dev/null; then up=1; break; fi
    if ! kill -0 "$server" 2>/dev/null; then
      echo "// Chromium-family: could not run - the parity server exited"
      tail -5 target/browser-leg-server.log 2>/dev/null | sed 's/^/  /'
      verdicts="${verdicts}chromium=NOT-RUN(server) "
      break
    fi
    sleep 0.5
  done
  if [[ $up -eq 1 ]]; then
    page=""
    for attempt in 1 2 3 4 5 6; do
      "$browser" --headless=new --disable-gpu --no-sandbox --disable-dev-shm-usage \
        --no-first-run --virtual-time-budget=20000 --dump-dom "$BASE/" \
        > target/browser-leg-dom.html 2> target/browser-leg.log || true
      page=$(curl -sS --max-time 5 "$BASE/report" 2>/dev/null || true)
      if [[ -n "$page" ]]; then
        echo "//   reported on attempt $attempt ($(wc -c < target/browser-leg-dom.html) byte DOM)"
        break
      fi
      sleep 2
    done
    if [[ -z "$page" ]]; then
      echo "// Chromium-family: the browser never reported a digest - NOT a pass"
      tail -10 target/browser-leg.log 2>/dev/null | sed 's/^/  /'
      fail=1
      verdicts="${verdicts}chromium=FAILED(no-report) "
    elif [[ "$page" == "$EMPTY_SHA" ]]; then
      echo "// Chromium-family: reported the empty-input digest - the page printed nothing"
      fail=1
      verdicts="${verdicts}chromium=FAILED(empty) "
    elif [[ "$page" == "$expected" ]]; then
      echo "//   browser sha256 $page"
      echo "// Chromium-family agrees with this machine's native boot"
      ran=1
      verdicts="${verdicts}chromium=OK "
    else
      echo "//   native sha256  $expected"
      echo "//   browser sha256 $page"
      echo "// CHROMIUM LEG DISAGREES - same module, browser host. That is a P1.5 finding."
      ran=1
      fail=1
      verdicts="${verdicts}chromium=DIVERGES "
    fi
  fi
fi

echo
if [[ $ran -eq 0 && $fail -eq 0 ]]; then
  echo "SUMMARY: browser legs: ${verdicts:-none} - NO browser engine could be driven here, so this run"
  echo "         proves nothing about gate P1.5's browser clause."
  echo "RESULT: skipped. On macOS enable Safari's Develop > Allow Remote Automation and re-run."
  exit 2
fi
echo "SUMMARY: browser legs: ${verdicts}"
if [[ $fail -eq 1 ]]; then
  echo "RESULT: a browser leg disagrees with this machine's native boot - record it against gate P1.5."
  exit 1
fi
echo "RESULT: every browser engine that could run reproduced this machine's native boot output."
exit 0
