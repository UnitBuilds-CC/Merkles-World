#!/usr/bin/env bash
# Gate P1.5's fourth target family, attempted where it actually exists: Safari on Apple silicon.
#
# Why a script rather than workflow YAML. The claim is "these roots survive a second WebAssembly
# implementation", and the browser legs CI already runs are all Chromium - i.e. V8 three times, which
# docs/11 and OQ-41 say out loud. The one non-V8 engine this repository can reach without installing
# anything is JavaScriptCore, and it ships with macOS as Safari, drivable by the `safaridriver` that
# ships with it too. So the whole harness is: a WebDriver session, one URL, and the digest the page
# already POSTs to our own server.
#
# Deliberately no dependencies: curl, sed and the shell. The WebDriver protocol is JSON over HTTP, and
# adding a Node or Python client to read one `sessionId` would put a dependency in the hot path of the
# one check that is supposed to be about a browser engine.
#
# Usage (from a macOS runner):
#   cargo build --release --target wasm32-wasip1 -p engine
#   bash tools/safari_parity.sh target/wasm32-wasip1/release/engine.wasm
#
# Exit status: 0 and "SKIPPED <why>" when the platform or the driver cannot be used at all, 0 and
# "OK" when Safari reported the native digest, 1 for anything else - a digest that disagrees is a
# finding, and a missing driver is a fact about the runner, not about the code.

set -uo pipefail

WASM="${1:-target/wasm32-wasip1/release/engine.wasm}"
PORT="${MLWS_PORT:-8137}"
DRIVER_PORT="${MLWS_WEBDRIVER_PORT:-44451}"
BASE="http://127.0.0.1:${PORT}"
DRIVER="http://127.0.0.1:${DRIVER_PORT}"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "SKIPPED not macOS; JavaScriptCore is not reachable from $(uname -s)"
  exit 0
fi
if ! command -v safaridriver >/dev/null 2>&1; then
  echo "SKIPPED safaridriver is not installed on this runner"
  exit 0
fi
if [ ! -f "$WASM" ]; then
  echo "FAIL no module at $WASM (build it first: cargo build --release --target wasm32-wasip1 -p engine)"
  exit 1
fi

# Remote automation is off by default and needs one privileged toggle per machine. On CI images this
# usually succeeds; a refusal is reported rather than swallowed, because a silent skip is how a gate
# stops being a gate.
if ! safaridriver --enable 2>/dev/null && ! sudo -n safaridriver --enable 2>/dev/null; then
  echo "SKIPPED safaridriver --enable was refused (remote automation not permitted)"
  exit 0
fi

node tools/browser_parity_server.mjs "$WASM" --port "$PORT" >safari-server.log 2>&1 &
server=$!
safaridriver --port "$DRIVER_PORT" >safari-driver.log 2>&1 &
driver=$!
cleanup() {
  kill "$server" 2>/dev/null || true
  kill "$driver" 2>/dev/null || true
}
trap cleanup EXIT

# The server binds asynchronously, so wait for it rather than guessing a sleep - and if it died, say so
# with its own log, because the first run of this step failed trunk by curling a fixed two seconds after
# a `node` start and reporting "not answering" with no reason attached.
up=0
for _ in $(seq 1 40); do
  if curl -sS --max-time 2 "$BASE/" -o /dev/null 2>/dev/null; then
    up=1
    break
  fi
  if ! kill -0 "$server" 2>/dev/null; then
    echo "FAIL the parity server exited before answering"
    cat safari-server.log 2>/dev/null || true
    exit 1
  fi
  sleep 0.5
done
if [ "$up" != 1 ]; then
  echo "FAIL the parity server never answered on $BASE"
  cat safari-server.log 2>/dev/null || true
  exit 1
fi
echo "node: $(node --version 2>/dev/null || echo 'not found'); server up on $BASE"

session_json=$(curl -sS --max-time 60 -X POST -H 'Content-Type: application/json' \
  -d '{"capabilities":{"alwaysMatch":{"browserName":"safari"}}}' "$DRIVER/session" 2>&1)
session=$(printf '%s' "$session_json" | grep -o '"sessionId":"[^"]*"' | head -1 | cut -d'"' -f4)
if [ -z "$session" ]; then
  echo "SKIPPED no WebDriver session could be started (safaridriver on macOS is unavailable here)"
  printf '%s\n' "$session_json" | head -c 400
  echo
  tail -20 safari-driver.log 2>/dev/null || true
  exit 0
fi

curl -sS --max-time 120 -X POST -H 'Content-Type: application/json' \
  -d "{\"url\":\"${BASE}/\"}" "$DRIVER/session/$session/url" >/dev/null

# The page reports its own digest; polling it is what makes this a wait rather than a sample. A DOM
# scrape would ask Safari to be finished at an arbitrary moment, which is how the Chromium leg first
# failed trunk.
reported=""
for _ in $(seq 1 60); do
  reported=$(curl -sS --max-time 5 "$BASE/report" 2>/dev/null || true)
  if [ -n "$reported" ]; then
    break
  fi
  sleep 1
done

curl -sS --max-time 20 -X DELETE "$DRIVER/session/$session" >/dev/null 2>&1 || true

if [ -z "$reported" ]; then
  echo "FAIL Safari never reported a digest within 60s"
  tail -20 safari-driver.log 2>/dev/null || true
  exit 1
fi

expected=$( (sha256sum "$MLWS_NATIVE_OUTPUT" 2>/dev/null || shasum -a 256 "$MLWS_NATIVE_OUTPUT" 2>/dev/null) | cut -d' ' -f1)
echo "safari sha256 : $reported"
echo "native sha256 : ${expected:-not supplied}"
if [ -z "$expected" ]; then
  echo "FAIL MLWS_NATIVE_OUTPUT must point at the native boot's output for a comparison"
  exit 1
fi
if [ "$reported" != "$expected" ]; then
  echo "FAIL JavaScriptCore's run disagrees with the native boot"
  exit 1
fi
echo "OK Safari/JavaScriptCore produced the native digest"
