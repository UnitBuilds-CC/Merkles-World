#!/usr/bin/env bash
# Does the boot still print the bytes the docs publish?
#
#   bash tools/check_boot_digest.sh
#
# The lesson this encodes: a citation edit that landed inside the boot's own printed line changed the
# whole-output digest, and every local check stayed green - no hashed vector had moved, and the citing files
# still agreed with the stale manifest value, because a claim checked only against other claims is not checked.
# CI's browser leg, which hashes a real run, caught it. This is that check moved to before the push.
#
# Exit status: 0 = measured bytes equal the published digest; 1 = they differ, so the manifest and the
# citations must move together; 2 = nothing was measured (no boot output, a truncated boot, no sha tool, or an
# unreadable manifest), which is never a pass.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root" || exit 2

EVIDENCE="${MLWS_EVIDENCE:-docs/evidence.json}"
EMPTY_SHA="e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"

if [[ ! -f "$EVIDENCE" ]]; then
  echo "cannot read the manifest at $EVIDENCE" >&2
  exit 2
fi
published="$(node -e "console.log(JSON.parse(require('fs').readFileSync('$EVIDENCE','utf8')).claims.boot_output_sha256.value)" 2>/dev/null)"
if [[ -z "$published" ]]; then
  echo "no boot_output_sha256 claim in $EVIDENCE, so there is nothing to compare against" >&2
  exit 2
fi

output="$(cargo run --release --quiet -p engine 2>/dev/null)"
if [[ -z "$output" ]]; then
  echo "the boot produced no output - a release build failure reads the same as a passing run if this is not checked" >&2
  exit 2
fi
if ! printf '%s\n' "$output" | grep -q '^// boot complete'; then
  echo "the boot never reached its closing marker: the run is truncated, and a truncated file has a hash too" >&2
  exit 2
fi

measured="$(printf '%s\n' "$output" | sha256sum 2>/dev/null | cut -d' ' -f1)"
[[ -z "$measured" ]] && measured="$(printf '%s\n' "$output" | shasum -a 256 2>/dev/null | cut -d' ' -f1)"
if [[ -z "$measured" ]]; then
  echo "neither sha256sum nor shasum is available, so the digest cannot be measured" >&2
  exit 2
fi
if [[ "$measured" == "$EMPTY_SHA" ]]; then
  echo "the boot output hashed to the empty-input digest - the pipeline fed nothing" >&2
  exit 2
fi

lines="$(printf '%s\n' "$output" | grep -c '' )"
echo "// measured: $measured ($lines lines of boot output)"
echo "// published in $EVIDENCE: $published"
if [[ "$measured" != "$published" ]]; then
  echo "MISMATCH: the boot's printed bytes moved without the manifest moving with them, or the manifest is" >&2
  echo "ahead of this checkout. Either repoint docs/evidence.json plus every file that cites it in one commit," >&2
  echo "or find what changed the output: no hashed vector has to move for this to happen - a printed word does." >&2
  exit 1
fi
echo "RESULT: the published whole-output digest is what this machine measured."
exit 0
