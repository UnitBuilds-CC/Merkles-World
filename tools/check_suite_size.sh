#!/usr/bin/env bash
# The published test count, measured from a run rather than agreed between documents.
#
#   bash tools/check_suite_size.sh [file-with-cargo-test-output]
#
# Why this exists: README, docs/11 and docs/evidence.json all state "194 tests in 30 suites", and the docs
# checker makes those three agree. Three files agreeing with each other is not evidence - this session has now
# been caught twice by a claim whose only verification was another claim, including one where CI's browser leg
# caught a digest no local check could see. So the number gets recomputed from what `cargo test` actually
# printed, and the manifest is compared against the measurement instead of against the prose.
#
# Exit status: 0 = the measured tally is the published one; 1 = the published phrase disagrees with the run;
# 2 = nothing was measured (no input, no tests ran, or the run did not pass), which is never a pass.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root" || exit 2
SRC="${1:-}"

if [[ -z "$SRC" ]]; then
  SRC="$(mktemp)"
  if ! cargo test --workspace >"$SRC" 2>&1; then
    echo "cargo test did not pass, so there is no honest tally to publish" >&2
    tail -5 "$SRC" | sed 's/^/  /'
    exit 2
  fi
fi

if [[ ! -s "$SRC" ]]; then
  echo "no test output at $SRC - an empty file has a hash, and an empty tally has a count of zero" >&2
  exit 2
fi
if grep -q "test result: FAILED" "$SRC"; then
  echo "the run being tallied contains failures; a count of the passing lines alone would understate and pass" >&2
  exit 2
fi

read -r total suites <<<"$(awk '
  /^test result: ok\./ {
    gsub(/;/, "", $0)
    n = $4
    total += n
    if (n > 0) suites++
  }
  END { printf "%d %d\n", total, suites }
' "$SRC")"

if [[ "${total:-0}" -eq 0 ]]; then
  echo "the tally came out as zero tests, which means the parser matched nothing - check the input format" >&2
  exit 2
fi

measured="$total tests in $suites suites"
published="$(node -e 'console.log(require("./docs/evidence.json").claims.suite_size.value)' 2>/dev/null)"

echo "// measured from $SRC: $measured"
if [[ -z "$published" ]]; then
  echo "docs/evidence.json's suite_size claim could not be read, so there is nothing to compare against" >&2
  exit 2
fi
echo "// published in docs/evidence.json: $published"
if [[ "$measured" != "$published" ]]; then
  echo "MISMATCH: the run measures $measured while the docs publish $published." >&2
  echo "Either a suite was added or removed without repointing README, docs/11 and the manifest together," >&2
  echo "or the count in the docs was never re-measured against a run." >&2
  exit 1
fi
echo "RESULT: the published test count is what this run measured - $measured"
