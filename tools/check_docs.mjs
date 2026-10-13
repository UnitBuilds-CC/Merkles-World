// tools/check_docs.mjs — docs integrity, as a CI gate.
//
// Why this exists: the documentation *is* the specification (AGENTS.md §1: "The docs are the
// spec"), and this repository is public, so a broken link, a reference to an OQ that was never
// registered, or a table row that GitHub refuses to render is a product defect, not a typo. Each
// check below fired at least once during the pre-Phase-0 audit and found a real problem.
//
// Design rule: no judgement calls. Every check here is mechanical, and a green run means
// nothing is being quietly tolerated — so nothing in this script prints a warning. Either a
// finding is a failure, or the check does not exist. The spec↔contract mirror is deliberately not
// duplicated here; `cargo test -p coords --test gates` owns that rule, so the two can never
// disagree about what "in sync" means.
//
// Run: node tools/check_docs.mjs   (Node 20+, no dependencies)

import { readFileSync, readdirSync, statSync } from "node:fs";
import { extname, join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function markdownFiles(dir, out = []) {
  for (const entry of readdirSync(dir)) {
    if (entry === ".git" || entry === "node_modules" || entry === "target" || entry === ".github") continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) markdownFiles(full, out);
    else if (entry.endsWith(".md")) out.push(full);
  }
  return out;
}

const files = markdownFiles(repoRoot);
const text = new Map(files.map((f) => [f, readFileSync(f, "utf8")]));
const rel = (f) => f.slice(repoRoot.length + 1).replaceAll("\\", "/");
const findings = [];
const fail = (f, line, msg) => findings.push(`${rel(f)}:${line}  ${msg}`);

// ---------- 1. identifiers: every reference resolves to a definition ----------
// Home docs own the ids. A reference anywhere else is only a pointer.
const HOME = {
  OQ: "docs/13-risks-open-questions.md",
  SPEC: "docs/13-risks-open-questions.md",
  ADR: "docs/decisions.md",
  A: "docs/11-roadmap.md",
  S: "docs/11-roadmap.md",
};
const defined = { OQ: new Set(), SPEC: new Set(), ADR: new Set(), A: new Set(), S: new Set() };
// A definition is an id opening a heading, a bold span, or a table row — the three ways the docs
// register one; anything else is only a pointer. Two kinds share docs/13 and two share docs/11, so
// each match is bucketed by the id's own prefix and its home file decides whether that counts as a
// definition here. Spike ids are bare (`S1`), the rest are hyphenated.
for (const [f, t] of text) {
  const path = rel(f);
  for (const m of t.matchAll(
    /^(?:#{2,6}\s.*?\b|\*\*|\|\s*\**)(OQ-\d+|SPEC-\d+|ADR-\d+|A-\d+|S\d+)\b/gm
  )) {
    const kind = /^(OQ|SPEC|ADR|A)-/.test(m[1]) ? /^(OQ|SPEC|ADR|A)-/.exec(m[1])[1] : "S";
    if (HOME[kind] === path) defined[kind].add(m[1]);
  }
}

for (const [f, t] of text) {
  t.split(/\r?\n/).forEach((line, i) => {
    for (const m of line.matchAll(/\b(OQ|SPEC|ADR)-(\d+)\b/g)) {
      const id = `${m[1]}-${m[2]}`;
      if (!defined[m[1]].has(id)) fail(f, i + 1, `${id} referenced but never defined in ${HOME[m[1]]}`);
    }
    // Artifact ids (A-01…A-09) are hyphenated; spike ids are bare `S<n>`, which is why the spike
    // check is scoped to the word "spike" and to docs/11's own table.
    for (const m of line.matchAll(/\bA-(\d+)\b/g)) {
      const id = `A-${m[1]}`;
      if (!defined.A.has(id)) fail(f, i + 1, `${id} referenced but not in the artifact table (${HOME.A})`);
    }
    for (const m of line.matchAll(/\bspikes?\s+(S\d+)\b/gi)) {
      if (!defined.S.has(m[1])) fail(f, i + 1, `${m[1]} referenced as a spike but not in ${HOME.S}`);
    }
  });
}

// ---------- 2. local links and GitHub heading anchors ----------
// GitHub's slug: lowercase, markdown emphasis/backticks/colons removed, punctuation dropped,
// every remaining space becomes a hyphen. Two adjacent spaces therefore yield `--`, and an em
// dash vanishes rather than becoming `-`. Table rows are never anchors.
function slug(heading) {
  const body = heading.replace(/^#+\s+/, "").replaceAll(/[*`]/g, "").replaceAll(":", "");
  return body.trim().toLowerCase().replaceAll(/[^\w\s-]/g, "").replaceAll(/\s/g, "-");
}

const anchors = new Map();
for (const [f, t] of text) {
  const set = new Set();
  let fenced = false;
  for (const line of t.split(/\r?\n/)) {
    if (/^\s*```/.test(line)) fenced = !fenced;
    else if (!fenced && /^#{1,6}\s/.test(line)) set.add(slug(line));
  }
  anchors.set(f, set);
}

let linksChecked = 0;
for (const [f, t] of text) {
  let fenced = false;
  t.split(/\r?\n/).forEach((line, i) => {
    if (/^\s*```/.test(line)) fenced = !fenced;
    if (fenced) return;
    for (const m of line.matchAll(/\[[^\]]*\]\(([^)\s]+)\)/g)) {
      const href = m[1];
      if (/^(https?:|mailto:|#)/.test(href)) continue;
      linksChecked++;
      const [pathPart, frag] = href.split("#");
      let target = resolve(dirname(f), pathPart);
      if (!statSync(target, { throwIfNoEntry: false })?.isFile() && !extname(target)) target += ".md";
      if (!statSync(target, { throwIfNoEntry: false })?.isFile()) {
        fail(f, i + 1, `broken link: ${href}`);
        continue;
      }
      if (frag && anchors.has(target) && !anchors.get(target).has(frag))
        fail(f, i + 1, `anchor missing in ${rel(target)}: ${href}`);
    }
  });
}

// ---------- 3. markdown tables ----------
for (const [f, t] of text) {
  let fenced = false;
  let table = null;
  const flush = () => {
    if (table) {
      for (const row of table.rows)
        if (row.cells !== table.cells)
          fail(f, row.line, `table row has ${row.cells} cells, header has ${table.cells}`);
    }
    table = null;
  };
  t.split(/\r?\n/).forEach((line, i) => {
    if (/^\s*```/.test(line)) { fenced = !fenced; flush(); return; }
    if (fenced) return;
    if (!/^\s*\|.*\|\s*$/.test(line)) { flush(); return; }
    // `\|` is an escaped pipe inside a cell (docs/13 writes packed bit layouts that way), so it
    // must not count as a separator.
    const cells = line.trim().replace(/^\||\|$/g, "").split(/(?<!\\)\|/).length;
    if (/^\s*\|[\s:|-]+\|\s*$/.test(line)) return;
    if (!table) table = { cells, rows: [] };
    else table.rows.push({ line: i + 1, cells });
  });
  flush();
}

// ---------- 4. fenced code blocks ----------
// A fence that is never closed swallows the rest of the document, and an unbalanced brace inside
// a block means the snippet was cut mid-edit. Both are silent in preview and loud in review.
const PAIRS = { "{": "}", "(": ")", "[": "]" };
for (const [f, t] of text) {
  const lines = t.split(/\r?\n/);
  let lang = null;
  let body = [];
  let start = 0;
  let fences = 0;
  for (let i = 0; i < lines.length; i++) {
    const fence = /^\s*```(\w*)/.exec(lines[i]);
    if (!fence) {
      if (lang !== null) body.push(lines[i]);
      continue;
    }
    fences++;
    if (lang === null) { lang = fence[1] || "plain"; body = []; start = i + 1; continue; }
    let stack = [];
    let quote = null;
    let lineComment = false;
    const code = body.join("\n");
    for (let j = 0; j < code.length; j++) {
      const c = code[j];
      const next = code[j + 1];
      if (lineComment) { if (c === "\n") lineComment = false; continue; }
      if (quote) { if (c === "\\") j++; else if (c === quote) quote = null; continue; }
      if (c === '"') { quote = c; continue; }
      if (c === "'") {
        // Rust spells lifetimes (`&'a mut [T]`) and char literals with the same tick. Opening a
        // quote on every tick makes an odd number of lifetimes swallow the braces around them, so a
        // verbatim contract mirror reports a phantom imbalance. Only a well-formed char literal —
        // one non-space character, a `\` escape, or a `\u{..}` form between the ticks — opens one.
        const close = code.indexOf("'", j + 1);
        const inner = close === -1 ? "" : code.slice(j + 1, close);
        const isChar = /^.$/.test(inner) && !/\s/.test(inner)
          || /^\\.$/.test(inner)
          || /^\\u\{[0-9a-fA-F]+\}$/.test(inner);
        if (isChar) j = close;
        continue;
      }
      if (c === "/" && next === "/") { lineComment = true; continue; }
      if (c === "/" && next === "*") { j += 2; while (j < code.length && !(code[j] === "*" && code[j + 1] === "/")) j++; continue; }
      if (PAIRS[c]) stack.push(c);
      else if (c === "}" || c === ")" || c === "]") {
        const open = stack.pop();
        if (open && PAIRS[open] !== c) { fail(f, start, `unbalanced '${c}' in ${lang} block (innermost was '${open}')`); break; }
      }
    }
    if (stack.length) fail(f, start, `unclosed ${lang} block: leftover ${stack.join("")}`);
    lang = null;
  }
  if (fences % 2) fail(f, 1, `odd number of code fences: ${fences}`);
}

// ---------- 5. publication hygiene ----------
// What must never reach a public repo: another machine's filesystem, and unfinished markers.
// The project's commit identity (UnitBuilds <ian@unitbuilds.com>, AGENTS.md §8) is deliberate and
// is not a personal path, so it is not in this list. Neither is any operator username literal:
// this file is published, so it catches *home paths* generically instead of naming an account.
// The one exempt account is `runner`, the GitHub-hosted user: docs quote Actions log lines, and
// `/Users/runner/work/…` is a rented VM's path, not anybody's machine. Every other username in
// those positions is still a finding.
const FORBIDDEN = [
  [/\b[A-Za-z]:\\Users\\/g, "a Windows user-home path"],
  [/\/c\/Users\//g, "a Git-Bash user-home path"],
  // `(?<!\/c)` keeps the Git-Bash form above from being reported twice, once as macOS. One leaked
  // line must produce one finding, or the report lies about how many problems there are.
  [/(?<!\/c)\/Users\/(?!runner\/)[A-Za-z0-9._-]+\//g, "a macOS user-home path"],
  [/\/home\/(?!runner\/)[A-Za-z0-9._-]+\/(?!\/)/g, "a Linux user-home path"],
  [/<<(<<+)|>>(>>+)/g, "a merge-conflict marker"],
  [/\b(TODO|TBD|FIXME|lorem ipsum)\b/g, "an unfinished marker"],
];
for (const [f, t] of text) {
  t.split(/\r?\n/).forEach((line, i) => {
    for (const [pattern, why] of FORBIDDEN)
      if (pattern.test(line)) fail(f, i + 1, `${why}: ${line.trim().slice(0, 90)}`);
  });
}

// ---------- 6. every design doc carries its status line ----------
// docs/00…14 open with **Status:** (and what they depend on), so an agent can tell a frozen
// contract from a proposal before it codes against one.
for (const [f, t] of text) {
  if (!/^docs\/\d\d-/.test(rel(f))) continue;
  if (!t.includes("**Status:**")) fail(f, 1, "numbered design doc has no **Status:** line");
}

// ---------- 7. published evidence cannot drift from the docs that cite it ----------
// docs/evidence.json holds the numbers the documentation publishes as *measured* — a whole-output hash, an
// asset digest — and this makes each one a rule rather than a habit. The motivation is concrete: the boot's
// output digest was quoted in five places and went stale the moment the raster line grew, because nothing
// checked the prose against the artifact. CI carries the other half (it fails when the measured value stops
// matching the manifest), so a green run means the published number is the measured one.
//
// Mechanical only, in keeping with the rule at the top of this file: a claim's value must be a full 64-hex
// digest, each file listed as citing it must contain that digest's prefix, and **a line that cites a
// superseded digest must also cite the current digest on that same line**. There is deliberately no list of
// acceptable "history" words: a vocabulary rule is prose contorted to satisfy a checker, and one that any
// sentence could satisfy by including a magic word is not a check. Requiring the current value beside the
// old one is the property actually wanted - nobody reads a stale number without meeting the live one.
let evidence = null;
try {
  evidence = JSON.parse(readFileSync(join(repoRoot, "docs", "evidence.json"), "utf8"));
} catch (error) {
  fail(join(repoRoot, "docs", "evidence.json"), 1, `unreadable: ${error.message}`);
}
if (evidence) {
  const claims = evidence.claims ?? {};
  for (const [name, claim] of Object.entries(claims)) {
    const kind = claim?.kind ?? "digest";
    if (typeof claim?.value !== "string") {
      fail(join(repoRoot, "docs", "evidence.json"), 1, `${name}: value must be a string`);
      continue;
    }
    if (kind === "digest" && !/^[0-9a-f]{64}$/.test(claim.value)) {
      fail(join(repoRoot, "docs", "evidence.json"), 1, `${name}: a digest claim's value must be a 64-character lowercase hex digest`);
      continue;
    }
    if (kind === "phrase" && claim.value.length < 6) {
      fail(join(repoRoot, "docs", "evidence.json"), 1, `${name}: a phrase claim must be long enough to be unambiguous (got "${claim.value}")`);
      continue;
    }
    if (!Array.isArray(claim.cited_in) || claim.cited_in.length === 0) {
      fail(join(repoRoot, "docs", "evidence.json"), 1, `${name}: cited_in must list at least one doc`);
      continue;
    }
    const prefix = kind === "digest" ? claim.value.slice(0, 16) : claim.value;
    for (const doc of claim.cited_in) {
      const entry = [...text.entries()].find(([path]) => path === join(repoRoot, doc));
      if (!entry) {
        fail(join(repoRoot, "docs", "evidence.json"), 1, `${name}: cited_in names ${doc}, which is not a markdown file in this repo`);
        continue;
      }
      if (!entry[1].includes(prefix)) {
        fail(entry[0], 1, `${name}: ${doc} is declared as citing ${prefix}… but does not contain it — the citation moved, changed or was dropped`);
      }
    }
    if (kind !== "digest") continue;
    for (const past of claim.superseded ?? []) {
      if (typeof past?.value !== "string" || !/^[0-9a-f]{64}$/.test(past.value)) {
        fail(join(repoRoot, "docs", "evidence.json"), 1, `${name}: a superseded value is not a 64-character hex digest`);
        continue;
      }
      const stale = past.value.slice(0, 16);
      // Per *file*, not per line. The line rule was written to stop a doc dropping the live number while
      // quoting a dead one, and it did - five times over, by appending "(current digest: …)" to every line
      // that narrates history, until the prose was unreadable and the next supersession would have produced
      // thirty-odd findings that anyone would be tempted to switch off. A file that cites a recorded value
      // at all must (a) contain the live value somewhere in itself and (b) be declared in cited_in, which is
      // the same protection at the granularity prose actually has: history is told once per file, not once
      // per sentence. CI's half is untouched - it fails when the measured bytes differ from the manifest.
      for (const [file, body] of text) {
        if (!body.includes(stale)) continue;
        if (!body.includes(prefix)) {
          fail(file, 1, `${name}: this file cites the superseded digest ${stale}… but nowhere contains the current ${prefix}…`);
        }
        const relative = file.slice(repoRoot.length + 1).split("\\").join("/");
        if (!claim.cited_in.includes(relative)) {
          fail(file, 1, `${name}: this file cites a recorded evidence value but is not listed in the claim's cited_in (${claim.cited_in.join(", ")})`);
        }
      }
    }
  }
}

// ---------- report ----------
console.log(`docs check: ${files.length} files, ${linksChecked} local links, ` +
  `${defined.OQ.size} OQ + ${defined.SPEC.size} SPEC + ${defined.ADR.size} ADR + ` +
  `${defined.A.size} artifacts + ${defined.S.size} spikes defined`);
if (findings.length) {
  for (const line of findings) console.log(`  ${line}`);
  console.error(`\nFAILED: ${findings.length} finding(s)`);
  process.exit(1);
}
console.log("OK: no findings");
