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

const registerUnderTest = JSON.parse(readFileSync(at('docs/evidence.json'), 'utf8')).gate_p1_5_targets;
const agreedPristine = registerUnderTest.filter((row) => row.status === 'agrees').length;
const totalPristine = registerUnderTest.length;
// Row handles are derived from the register under test: this suite used to hardcode indices and the four old
// labels, so the day the target list was amended it crashed on index 2. A test that assumes the shape of the
// thing it guards is the same bug as a count that assumes no device has reported.
const rowAt = (j, i) => j.gate_p1_5_targets[i % j.gate_p1_5_targets.length];
const labelAt = (i) => registerUnderTest[i % registerUnderTest.length].target;
const countRight = `${agreedPristine} of ${totalPristine} named targets have reported`;
const countWrong = `${agreedPristine + 1} of ${totalPristine} named targets have reported`;

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
      Object.assign(rowAt(j, 0), {
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
      // Delete, do not assign: once both devices had reported, row 1 already said `agrees` with a digest, so
      // setting the status again planted nothing and the case passed by proving nothing.
      const noDigest = rowAt(j, 1);
      noDigest.status = "agrees";
      delete noDigest.boot_sha256;
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "an 'agrees' row with no commit",
    expects: "without the commit it ran",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      const current = j.claims.boot_output_sha256.value;
      // legs present and honest, so the only thing missing is the commit - the legs rule fires earlier.
      const noCommit = rowAt(j, 1);
      Object.assign(noCommit, { status: "agrees", boot_sha256: current, recorded: "2026-10-13", legs: "native,wasi" });
      delete noCommit.commit;
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "a 'diverges' row with no note",
    expects: "diverges with no note",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      rowAt(j, 3).status = "diverges";
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
      rowAt(j, 0).target = "Pentium-4";
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "a register row with no command",
    expects: "an unrun target has to say what would run it",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      delete rowAt(j, 1).command;
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "an 'agrees' row with no legs recorded",
    expects: "agrees with no legs recorded",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      Object.assign(rowAt(j, 0), {
        status: "agrees",
        boot_sha256: j.claims.boot_output_sha256.value,
        commit: "0".repeat(40),
        recorded: "2026-10-13",
      });
      delete rowAt(j, 0).legs;
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "a browser-named row agreeing without a browser leg",
    expects: "no browser leg",
    mutate() {
      const j2 = JSON.parse(PRISTINE.evidence);
      // One assignment sets the whole row: the rules are an else-if chain, so an earlier missing field would
      // otherwise mask the browser rule and this case would pass by proving nothing.
      Object.assign(j2.gate_p1_5_targets[1], {
        target: "M1-browser",
        status: "agrees",
        boot_sha256: j2.claims.boot_output_sha256.value,
        commit: "0".repeat(40),
        recorded: "2026-10-13",
        legs: "native,wasi",
      });
      write("docs/evidence.json", JSON.stringify(j2, null, 2));
    },
  },
  {
    name: "an 'agrees' row whose legs never include native",
    expects: "does not include native",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      Object.assign(rowAt(j, 1), {
        status: "agrees",
        boot_sha256: j.claims.boot_output_sha256.value,
        commit: "0".repeat(40),
        recorded: "2026-10-13",
        legs: "browser,codegen",
      });
      write("docs/evidence.json", JSON.stringify(j, null, 2));
    },
  },
  {
    name: "docs/11 stating a count the register does not hold",
    // Derived from the copy under test rather than hardcoded at 0 and 1: with the numbers fixed, these two
    // cases became silent no-ops the moment a device run landed and the register genuinely reported 1 of 4 -
    // the violation they plant would have been the truth, so the suite would have failed CI for nothing.
    expects: [`must state "${countRight}"`, "docs/11-roadmap.md"],
    mutate() {
      write("docs/11-roadmap.md", PRISTINE.roadmap.replace(countRight, countWrong));
    },
  },
  {
    name: "README stating a count the register does not hold",
    expects: [`must state "${countRight}"`, "README.md"],
    mutate() {
      write("README.md", PRISTINE.readme.replace(countRight, countWrong));
    },
  },
  {
    name: "a status value that is none of the three allowed",
    expects: "must be \"not run\", \"agrees\" or \"diverges\"",
    mutate() {
      const j = JSON.parse(PRISTINE.evidence);
      rowAt(j, 0).status = "probably fine";
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
