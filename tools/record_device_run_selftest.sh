#!/usr/bin/env bash
# Does the device-run recorder actually refuse what it claims to refuse?
#
#   bash tools/record_device_run_selftest.sh
#
# tools/record_device_run.sh is the last step of gate P1.5's evidence chain: a human pastes what a machine
# printed and this command moves the register and the two prose sites together. A tool whose job is to refuse a
# bad record has to be shown refusing it - otherwise the gate's whole audit trail rests on the assumption that
# the refuse branch works. So each rejection is planted, and the acceptance is checked to have actually
# reached the files and to have left docs integrity passing.
#
# Everything happens in a throwaway copy of the repository with its own git history, so the restore path is
# exercised for real and the working tree is never touched.
#
# Exit status: 0 = every behaviour observed; 1 = at least one did not, which means the recorder is not the
# safety net it is described as.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root" || exit 2

work="$(mktemp -d "${TMPDIR:-/tmp}/merkles-recorder-XXXXXX")" || exit 2
trap 'rm -rf "$work"' EXIT

rsync -a --exclude 'target' --exclude '.git' "$repo_root/" "$work/" 2>/dev/null ||
  (cd "$repo_root" && tar cf - --exclude=./target --exclude=./.git .) | (cd "$work" && tar xf -) || exit 2

cd "$work" || exit 2
# Its own git history, because the recorder's failure path restores with `git checkout --`.
git init -q . || exit 2
git add -A >/dev/null 2>&1
git -c user.name=recorder-selftest -c user.email=recorder@invalid \
  -c commit.gpgsign=false commit -qm baseline >/dev/null 2>&1 || exit 2

# Use a commit that exists in this throwaway history: the recorder now refuses one that does not.
HEAD_SHA="$(git rev-parse HEAD)"
# Labels come from the register under test for the same reason the counts do: the target list is data now.
LABEL_A="$(node -e 'console.log(require("./docs/evidence.json").gate_p1_5_targets[0].target)')"
LABEL_B="$(node -e 'console.log(require("./docs/evidence.json").gate_p1_5_targets[1 % require("./docs/evidence.json").gate_p1_5_targets.length].target)')"

# The digest this copy publishes, and the count its prose currently claims.
published="$(node -e 'console.log(require("./docs/evidence.json").claims.boot_output_sha256.value)')"
claim="$(node -e 'const r=require("./docs/evidence.json").gate_p1_5_targets;console.log(r.filter(x=>x.status==="agrees").length+" of "+r.length+" named targets have reported")')"
status=0

record() {
  # $1 label, $2 json record, rest: extra flags
  local label="$1" json="$2"
  shift 2
  bash tools/record_device_run.sh --label "$label" --record "$json" "$@" >selftest.out 2>selftest.err
  echo $?
}

expect() {
  # $1 name, $2 want-pass(1/0), $3 exit code, $4 grep-for (may be empty)
  local name="$1" want="$2" got="$3" needle="${4:-}" ok=1
  [[ "$want" == "$got" ]] || ok=0
  if [[ -n "$needle" ]] && ! grep -qF "$needle" selftest.out selftest.err 2>/dev/null; then ok=0; fi
  if [[ $ok == 1 ]]; then
    echo "  ok: $name"
  else
    status=1
    echo "  NOT AS EXPECTED: $name (exit $got, wanted $want${needle:+, needle '$needle'})"
    sed 's/^/      /' selftest.err 2>/dev/null | head -3
  fi
}

good() {
  # $1 target label, $2 legs
  printf '{"target":"%s","status":"agrees","boot_sha256":"%s","commit":"%s","recorded":"2026-10-13","host":"testbox","os":"selftest","legs":"%s","command":"selftest"}' "$1" "$published" "$HEAD_SHA" "$2"
}

echo "// recorder self-test (register currently claims: $claim)"

# 1. a stale digest must be refused, and nothing may change
zero64="0000000000000000000000000000000000000000000000000000000000000000"
stale_record="$(good "$LABEL_A" native,wasi | sed "s/$published/$zero64/")"
out="$(record "$LABEL_A" "$stale_record" --apply)"
expect "an agrees record citing a digest other than the published one is refused" 1 "$out" "not the published"
if [[ "$(git status --porcelain -- docs/evidence.json docs/11-roadmap.md README.md | wc -l)" != 0 ]]; then
  status=1
  echo "  NOT AS EXPECTED: a refused record left files changed"
fi

# 2. the fourth target without a browser leg must be refused
out="$(record "$LABEL_A" "$(good "$LABEL_A" wasi,browser)" --apply)"
expect "a record whose legs never computed a root is refused" 1 "$out" "legs must include native"
if [[ "$(git status --porcelain -- docs/evidence.json docs/11-roadmap.md README.md | wc -l)" != 0 ]]; then
  status=1
  echo "  NOT AS EXPECTED: a refused record left files changed"
fi

# 3. a label that is not a register row must be refused
out="$(record Pentium-4 "$(good Pentium-4 native)" --apply)"
expect "an unknown target is refused rather than appended to the register" 1 "$out" "not a row of the register"

# 4. malformed JSON must be refused
out="$(record N2600 '{not json' --apply)"
expect "a malformed record is refused" 1 "$out" "not valid JSON"
# 4b. a commit that is not in this clone's history must be refused: a verdict pinned to an unreadable SHA
#     cannot be re-checked, and the device run is exactly the sort of thing a mistyped SHA would invalidate.
ghost_record="$(good "$LABEL_A" native,wasi | sed "s/$HEAD_SHA/0000000000000000000000000000000000000000/")"
out="$(record "$LABEL_A" "$ghost_record" --apply)"
expect "a record citing a commit absent from history is refused" 1 "$out" "not in this clone's history"

# 5. --apply with a valid record must land in all three files, and keep docs integrity passing
out="$(record "$LABEL_B" "$(good "$LABEL_B" native,wasi)" --apply)"
expect "a valid record is accepted with --apply" 0 "$out" "recorded"
node tools/check_docs.mjs >selftest.out 2>&1
expect "the register and both prose sites still satisfy docs integrity" 0 "$?" "no findings"
if ! grep -qF '"status": "agrees"' docs/evidence.json; then
  status=1
  echo "  NOT AS EXPECTED: the register row was not written"
fi
if ! grep -qF "2 of 2 named targets have reported" README.md; then
  status=1
  echo "  NOT AS EXPECTED: README's count did not move"
fi
if ! grep -qF "2 of 2 named targets have reported" docs/11-roadmap.md; then
  status=1
  echo "  NOT AS EXPECTED: docs/11's count did not move"
fi

# 6. a dry run must accept the validation and change nothing
git checkout -- . >/dev/null 2>&1
out="$(record "$LABEL_B" "$(good "$LABEL_B" native,wasi,browser)")"
expect "a dry run validates and reports it rolled back" 0 "$out" "dry run"
if [[ "$(git status --porcelain -- docs/evidence.json docs/11-roadmap.md README.md | wc -l)" != 0 ]]; then
  status=1
  echo "  NOT AS EXPECTED: a dry run left the tree dirty"
fi

if [[ $status == 0 ]]; then
  echo "RESULT: every refusal fires, accepted records land in all three files, and a dry run touches nothing."
else
  echo "FAILED: the recorder does not behave as documented; gate P1.5's evidence path has a soft spot." >&2
fi
exit $status
