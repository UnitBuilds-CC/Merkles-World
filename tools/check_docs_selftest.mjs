// Every rule docs/11's gate P1.5 register depends on is a rule about absence: a row that names no digest, a
// count nobody checked, a target that quietly disappeared. Those are precisely the rules that "pass" when the
// checker itself is broken, so the checker gets tested here - in a throwaway copy of the repository, against
// each violation planted one at a time. If the validator misses any of them, this exits non-zero, and CI's
// docs job goes red on a *checker* regression rather than leaving the register protected by hope.
//
//   node tools/check_docs_selftest.mjs
//
// It never touches the working tree: the repo is copied first, and each case rewrites files inside the copy.
import { cpSync, mkdtempSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, dirname, sep } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const sandbox = mkdtempSync(join(tmpdir(), "merkles-selftest-"));

cpSync(repoRoot, sandbox, {
  recursive: true,
  filter: (src) => {
    const rel = src.slice(repoRoot.length);
    return !rel.startsWith(`${sep}target`) && !rel.startsWith(`${sep}.git`) && !rel.startsWith(`${sep}node_modules`);
  },
});

const at = (p) => join(sandbox, p);
const read = (p) => readFileSync(at(p), "utf8");
const write = (p, body) => writeFileSync(at(p), body);

function validate() {
  const r = spawnSync("node", [at("tools/check_docs.mjs")], { cwd: sandbox, encoding: "utf8" });
  return { code: r.status ?? 1, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
}

const PRISTINE = {
  evidence: read("docs/evidence.json"),
  roadmap: read("docs/11-roadmap.md"),
  readme: read("README.md"),
};

function restore() {
  write("docs/evidence.json", PRISTINE.evidence);
  write("docs/11-roadmap.md", PRISTINE.roadmap);
  write("README.md", PRISTINE.readme);
}

// A rule is proven when a planted violation produces this message, and nothing else does.
const CASES = [
  {
    name: "an 'agrees' row citing a boot digest from an older commit",
    expects: "older code state",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      Object.assign(j.gate_p1_5_targets[0], {
        status: "agrees",
        boot_sha256: "7c71da7ca50774da36def063e24b4848ff345c9116cdea2221e3fba619bd50af",
        commit: "0".repeat(40),
        recorded: "2026-10-13",
      });
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "an 'agrees' row with no digest at all",
    expects: "no whole-output digest",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      j.gate_p1_5_targets[1].status = "agrees";
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "an 'agrees' row with no commit",
    expects: "without the commit it ran",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      const current = j.claims.boot_output_sha256.value;
      Object.assign(j.gate_p1_5_targets[2], { status: "agrees", boot_sha256: current, recorded: "2026-10-13" });
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "a 'diverges' row with no note",
    expects: "diverges with no note",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      j.gate_p1_5_targets[3].status = "diverges";
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "a register with a named target dropped",
    expects: "a register with a row missing",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      j.gate_p1_5_targets.pop();
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "the register naming hardware the roadmap does not",
    expects: "names a target docs/11 does not",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      j.gate_p1_5_targets[0].target = "Pentium-4";
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "a register row with no command",
    expects: "an unrun target has to say what would run it",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      delete j.gate_p1_5_targets[1].command;
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "docs/11 stating a count the register does not hold",
    expects: ['must state "0 of 4 named targets have reported"', "docs/11-roadmap.md"],
    mutate() {
      write("docs/11-roadmap.md", PRISTINE.roadmap.replace("0 of 4 named targets have reported", "1 of 4 named targets have reported"));
    },
  },
  {
    name: "README stating a count the register does not hold",
    expects: ['must state "0 of 4 named targets have reported"', "README.md"],
    mutate() {
      write("README.md", PRISTINE.readme.replace("0 of 4 named targets have reported", "1 of 4 named targets have reported"));
    },
  },
  {
    name: "a status value that is none of the three allowed",
    expects: "must be \"not run\", \"agrees\" or \"diverges\"",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      j.gate_p1_5_targets[0].status = "probably fine";
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
];

restore();
const baseline = validate();
console.log(`baseline (pristine copy): exit ${baseline.code}, ${baseline.code === 0 ? "no findings" : "findings present"}`);
if (baseline.code !== 0) {
  console.error("the pristine copy already fails, so a planted violation proves nothing - fix that first");
  console.error(baseline.out.split("\n").slice(0, 6).join("\n"));
  rmSync(sandbox, { recursive: true, force: true });
  process.exit(1);
}

let missed = 0;
for (const test of CASES) {
  restore();
  test.mutate();
  const res = validate();
  const wanted = Array.isArray(test.expects) ? test.expects : [test.expects];
  const caught = res.code !== 0 && wanted.every((needle) => res.out.includes(needle));
  if (!caught) {
    missed += 1;
    console.log(`  NOT CAUGHT: ${test.name}`);
    console.log(`    exit=${res.code}, expected findings containing: ${wanted.join(" + ")}`);
    console.log(`    got: ${res.out.trim().split("\n").slice(0, 3).join(" | ")}`);
  } else {
    console.log(`  caught: ${test.name}`);
  }
}

// And the inverse: a no-op edit must not fail, or the "NOT CAUGHT" results above would be meaningless.
restore();
write("docs/evidence.json", PRISTINE.evidence);
const restored = validate();
if (restored.code !== 0) {
  missed += 1;
  console.log("  NOT CAUGHT: restoring the pristine files leaves findings behind");
} else {
  console.log("  caught: restoring the pristine files reports no findings");
}

rmSync(sandbox, { recursive: true, force: true });
console.log(missed ? `\nFAILED: ${missed} of ${CASES.length + 1} checks did not behave` : `\nOK: all ${CASES.length} planted violations are caught, and the pristine state is clean`);
process.exit(missed ? 1 : 0);
