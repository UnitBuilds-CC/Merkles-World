#!/usr/bin/env bash
# The trunk checks, in one command, with the toolchain stated.
#
#   bash tools/local_gate.sh
#
# Why: `rust-toolchain.toml` asks for `channel = "stable"`, which is not a version. CI resolves whatever
# stable its runner image carries - 1.99 as of 2026-10-13 - while this workstation resolves 1.92. Twice in one
# day a commit was green locally and red on all three gates jobs because a *newer clippy* had a lint the local
# one has never heard of (`chunks_exact` with a constant size, and earlier a `chunks_exact_mut`). That is not
# an agent being careless; it is no defined procedure for "what trunk will check". This script is that
# procedure, and it prints the versions so a disagreement is visible before it becomes a red trunk rather than
# a mystery afterwards.
#
# It runs the cheap-to-expensive order CI uses and stops at the first failure, because a step that ran after a
# failed one tells you nothing.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root" || exit 2

fail=0
step() {
  local name="$1"
  shift
  echo "--- $name"
  if "$@"; then
    echo "    ok: $name"
  else
    local status=$?
    echo "    FAILED ($status): $name" >&2
    fail=1
  fi
}

echo "toolchain: $(rustc -vV 2>/dev/null | sed -n 's/^release: /rustc /p')"
echo "clippy:    $(cargo clippy --version 2>/dev/null || echo 'not installed')"
echo "cargo:     $(cargo -V 2>/dev/null)"
# The number CI last resolved is not knowable offline, so it is asked for explicitly: an agent who reads a red
# trunk here can compare the two lines instead of assuming their machine is right.
ci_line="$(gh run list --limit 1 --json databaseId -q '.[0].databaseId' 2>/dev/null)"
if [[ -n "${ci_line:-}" ]] && command -v gh >/dev/null 2>&1; then
  ci_clippy="$(gh run view "$ci_line" --log 2>/dev/null | grep -m1 -oE 'clippy [0-9]+\.[0-9]+\.[0-9]+' | head -1)"
  echo "CI (most recent run): ${ci_clippy:-could not read the clippy line from the CI log}"
fi

if [[ $fail -eq 0 ]]; then
  step "format" cargo fmt --all -- --check
  step "lints" cargo clippy --all-targets -- -D warnings
  step "tests" cargo test --workspace
  step "docs integrity" node tools/check_docs.mjs
  step "docs checker self-test" node tools/check_docs_selftest.mjs
  step "AS2 gate logic" bash tools/check_as2_gate.sh
fi

echo
if [[ $fail -ne 0 ]]; then
  echo "RESULT: trunk would reject this tree. Fix the named step; do not push and find out from the runner."
  exit 1
fi
echo "RESULT: every trunk check above passed locally. Note the clippy versions: if CI's is newer than yours,"
echo "        its lint set is a superset of what you just ran, and that gap has red-trunked this repo twice."
exit 0
