#!/usr/bin/env bash
# Record a device run into gate P1.5's register - with the bookkeeping checked rather than remembered.
#
#   bash tools/record_device_run.sh --label M1-browser --record '<the JSON line the device printed>'
#   ...add --apply to write. Without it the validation still runs and nothing is left behind.
#
# Why this exists: the four device runs belong to the user, and once they happen the register has to move with
# them - the row in docs/evidence.json, and the derived `N of 4 named targets have reported` sentence in docs/11
# and README, which docs integrity fails over when they disagree. Doing that across three files by hand is how
# a stale digest or a forgotten citation gets in, and this project has already been caught by exactly that class
# of drift. So the paste becomes one command, the checks run before anything is written, the validators run
# after, and a failing validator restores every file.
#
# Exit status: 0 = recorded (or would be, in a dry run) and consistent; 1 = refused, with the reason;
# 2 = could not run at all (bad arguments, unreadable register).
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root" || exit 2

LABEL=""
RECORD=""
APPLY=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --label) shift; LABEL="${1:-}" ;;
    --record) shift; RECORD="${1:-}" ;;
    --apply) APPLY=1 ;;
    -h | --help)
      echo "usage: $0 --label <target> --record '<json>' [--apply]"
      exit 0
      ;;
    *)
      echo "unknown option '$1'" >&2
      exit 2
      ;;
  esac
  shift
done

if [[ -z "$LABEL" || -z "$RECORD" ]]; then
  echo "both --label and --record are required (see --help)" >&2
  exit 2
fi

# ---- what the prose says today, captured before anything changes ---------------------------------------
before_count="$(node -e 'const r=require("./docs/evidence.json").gate_p1_5_targets;console.log(r.filter(x=>x.status==="agrees").length)' 2>/dev/null)"
total="$(node -e 'console.log(require("./docs/evidence.json").gate_p1_5_targets.length)' 2>/dev/null)"
if [[ -z "$before_count" || -z "$total" ]]; then
  echo "the register could not be read from docs/evidence.json" >&2
  exit 2
fi
phrase_old="$before_count of $total named targets have reported"

# ---- validate, then write the row -----------------------------------------------------------------------
verdict="$(node - "$LABEL" "$RECORD" <<'NODE'
const fs = require("fs");
const path = "docs/evidence.json";
const j = JSON.parse(fs.readFileSync(path, "utf8"));
const register = j.gate_p1_5_targets;
const { execFileSync } = require("child_process");
const commitExists = (sha) => {
  try {
    execFileSync("git", ["cat-file", "-e", sha + "^{commit}"], { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
};
const label = process.argv[2];
let record;
try {
  record = JSON.parse(process.argv[3]);
} catch (error) {
  console.log("REFUSE|the record is not valid JSON: " + error.message);
  process.exit(0);
}
const entry = register.find((row) => row.target === label);
if (!entry) {
  console.log("REFUSE|'" + label + "' is not a row of the register. Known: " + register.map((r) => r.target).join(", "));
  process.exit(0);
}
const fail = [];
if (record.target !== label) fail.push("the record's target is '" + record.target + "' but --label says " + label);
if (!["agrees", "diverges"].includes(record.status)) fail.push("status must be agrees or diverges");
const hex64 = /^[0-9a-f]{64}$/;
if (!hex64.test(record.boot_sha256 ?? "")) {
  fail.push("boot_sha256 is missing or not 64 hex characters");
} else if (record.status === "agrees" && record.boot_sha256 !== j.claims.boot_output_sha256.value) {
  fail.push(
    "boot_sha256 " + record.boot_sha256.slice(0, 16) + "... is not the published " +
      j.claims.boot_output_sha256.value.slice(0, 16) +
      "... - that run measured a different code state, so re-run it at trunk or repoint the manifest first",
  );
}
if (!/^[0-9a-f]{7,40}$/.test(record.commit ?? "")) fail.push("commit is missing or not a hex SHA");
else if (!commitExists(record.commit)) {
  fail.push("commit " + record.commit + " is not in this clone's history - fetch it, or record the run from the checkout it was built from; a verdict tied to an unreadable SHA cannot be re-checked");
}
if (!/^\d{4}-\d{2}-\d{2}$/.test(record.recorded ?? "")) fail.push("recorded must be a YYYY-MM-DD date");
const legs = String(record.legs ?? "").split(",");
if (!legs.includes("native")) fail.push("legs must include native, or no root was computed here");
if (label === "M1-browser" && !legs.includes("browser")) {
  fail.push("M1-browser needs a browser leg - Safari was skipped, so this is not the fourth target reporting");
}
if (record.status === "diverges" && !record.note) fail.push("a divergence needs a note describing it");
if (fail.length) {
  console.log("REFUSE|" + fail.join(" ; "));
  process.exit(0);
}
Object.assign(entry, {
  status: record.status,
  boot_sha256: record.boot_sha256,
  commit: record.commit,
  recorded: record.recorded,
  host: record.host ?? "(unknown)",
  os: record.os ?? "(unknown)",
  legs: record.legs,
});
if (record.note) entry.note = record.note;
fs.writeFileSync(path, JSON.stringify(j, null, 2) + "\n");
const agreed = register.filter((row) => row.status === "agrees").length;
console.log("OK|" + label + "|" + agreed + "|" + register.length);
NODE
)"

case "$verdict" in
  REFUSE\|*)
    echo "REFUSED: ${verdict#REFUSE|}" >&2
    echo "Nothing was written." >&2
    exit 1
    ;;
  OK\|*)
    # Read the WHOLE line, prefix included: the field order is tag|label|agreed|total, and stripping the
    # prefix first shifts every name one slot left - which is how this script once reported the total as the
    # count and let docs integrity catch it.
    IFS='|' read -r _tag rowlabel agreed total_after <<<"$verdict"
    ;;
  *)
    echo "the validator printed no verdict - treating that as a harness failure" >&2
    exit 2
    ;;
esac
echo "// $rowlabel recorded; the register now reports $agreed of $total_after targets"

if [[ "$agreed" != "$before_count" ]]; then
  phrase_new="$agreed of $total named targets have reported"
  for f in docs/11-roadmap.md README.md; do
    python - "$f" "$phrase_old" "$phrase_new" <<'PY'
import sys
path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
raw = open(path, encoding="utf-8", newline="").read()
if old not in raw:
    print(f"// {path}: the sentence '{old}' was not found, so it was left alone - check it by hand")
else:
    open(path, "w", encoding="utf-8", newline="").write(raw.replace(old, new, 1))
    print(f"// {path}: '{old}' -> '{new}'")
PY
  done
fi

# ---- the checkers have the last word, and a failure restores everything --------------------------------
# Only docs integrity is asked to agree here. The self-test is a test of the *checker*, and it plants
# violations into a copy of the tree - running it against a register this command has just legitimately
# modified conflates "the prose and the register disagree" with "the checker's fixtures assumed otherwise".
# Deliberately not target/record_check.log: on a fresh clone target/ does not exist, so the redirect would
# fail and the restore branch would run - a false alarm from the tool meant to record the gate.
if ! node tools/check_docs.mjs >"$(mktemp)" 2>&1; then
  echo "the validators did not pass after the write; restoring docs/evidence.json, docs/11 and README" >&2
  tail -5 target/record_check.log >&2
  git checkout -- docs/evidence.json docs/11-roadmap.md README.md
  exit 1
fi

if [[ $APPLY -eq 0 ]]; then
  echo "RESULT: the record validates and every checker agrees with it. Rolling back - this was a dry run; add --apply to keep it."
  git checkout -- docs/evidence.json docs/11-roadmap.md README.md
  exit 0
fi

echo "RESULT: recorded at $agreed of $total. Commit docs/evidence.json, docs/11-roadmap.md and README together - the register and the two prose sites are one change."
exit 0
