#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// check-consistency.mjs: STRUCTURAL INVARIANTS on the sealed-envelope bundle + accessors.
//
// Under the sealed envelope (Design E) every metric is EITHER a certified number OR an explicit
// {value:null, suppressed:true}; the raw scalar and its _mock_bound flag are consumed at seal time
// and never re-emitted. This file checks invariants on the contract itself:
//
//   C1  No raw ungated metric field exists in the bundle; no `*_mock_bound` key survives anywhere.
//   C2  A suppressed metric exposes no recoverable value (value === null, no shadow numeric field).
//   C3  Every displayed caption derives from a source.sweep stamp present in the data; no hard-coded
//       source-token literal in a per-datum caption renderer (the lint).
//   C4  Single projection path: every projected cell's source.kind is a known origin and its
//       source.sweep is a valid caption key; no legacy suite object (g.perf/stream/streamcpu/xlate)
//       leaks into the bundle.
//   C5  Every sealed-metric read (app.js) routes through metric()/mval() - never a raw `.value` /
//       `.get("value")` deref outside the accessors (the taint-based accessor-routing lint).
//
// Each lint is a PURE EXPORTED FUNCTION so it can be driven against synthetic source that CONTAINS
// the violation - a lint with no RED-before proof is indistinguishable from one that cannot fire.
//
// Rigor rules (Design F Part 1): the expected side of any cross-representation assertion is
// re-derived INDEPENDENTLY from the RAW matrix cell on disk (results/matrix/<gw>.json), never via
// the accessor under test (R1). A COVERAGE assertion (R2) fails if any invariant branch is never
// exercised - an inert check is itself a failure. Each invariant has a RED-before test in test.mjs.
//
// Run standalone against an emitted bundle:
//   node site/check-consistency.mjs [site/data.json]

import { readFileSync, existsSync, readdirSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");

// Metric-field vocabulary imported from seal.mjs (the same list gen-data seals from) so this can't lag
// the producer; a shape rule for *_rss_mib covers new producer fields automatically.
import { displayedValue, isMetricField, zeroNoteFor } from "./seal.mjs";
// The origins a projected cell's source.kind may honestly carry: the single end-state "matrix" path plus
// the LIVE deferred fallbacks (kept until the field run; sealed honestly, never mislabelled as matrix).
const SOURCE_KINDS = new Set(["matrix", "perf-fallback", "xlate-fallback", "stream-fallback"]);

function isEnvelope(x) { return x != null && typeof x === "object" && typeof x.certified === "boolean"; }

// ---- reading a RAW artifact of either shape -------------------------------------------------------
// upstreamsOf(m): egress -> {cells} grid, from either raw artifact shape. v2 carries it under
// `upstreams`; v1 carries its single measured egress row as a top-level `cells`, named by
// `upstream_shape`. Reading only `m.upstreams` here silently zeroed the oracle for v1 artifacts,
// turning an honest legacy republish into a hard publish failure via the coverage gate.
export function upstreamsOf(m) {
  if (!m || typeof m !== "object") return {};
  if (m.upstreams && typeof m.upstreams === "object") return m.upstreams;
  if (m.cells && typeof m.cells === "object") return { [m.upstream_shape || "openai"]: { cells: m.cells } };
  return {};
}
// rawCellAt(m, ingress, egress): the raw cell at one coordinate, in either artifact shape, or null.
export function rawCellAt(m, ingress, egress) {
  const up = upstreamsOf(m)[egress];
  return (up && up.cells && up.cells[ingress]) || null;
}

// ---- C7: peak_rss_mib <= peak_rss_hwm_mib (a second physical-plausibility invariant) ------------
// VmHWM is the KERNEL's own high-water mark, so it cannot be lower than any RSS observed for the same
// process tree. Both readers sum over the tree ENUMERATED AT READ TIME (lib/harness.sh
// _proc_tree_field_mib): the sampled peak sums VmRSS over the tree alive DURING the load, while VmHWM
// is summed AFTER it, so a worker that exits in between is counted in the peak but absent from the HWM
// sum - sum(VmHWM) can legitimately fall below the sampled peak on a multi-process gateway. That's a
// transient-worker artefact, not fabricated data, so this WARNS rather than hard-failing; numbers are
// left exactly as measured.
export function c7HwmBelowPeak(gwKey, rawMatrix) {
  const warnings = [];
  let checked = 0;
  if (!rawMatrix || typeof rawMatrix !== "object") return { warnings, checked };
  const one = (label, mem) => {
    if (!mem || mem.served !== true) return;
    const peak = mem.peak_rss_mib, hwm = mem.peak_rss_hwm_mib;
    if (typeof peak !== "number" || typeof hwm !== "number" || peak <= 0 || hwm <= 0) return;
    checked += 1;
    if (peak > hwm)
      warnings.push(`${gwKey}.${label}: sampled peak_rss ${peak} MiB > kernel peak_rss_hwm ${hwm} MiB ` +
        `(a ${((peak / hwm - 1) * 100).toFixed(2)}% overshoot - VmHWM cannot be below an observed RSS for a FIXED ` +
        `process tree, so a child process counted in the sampled peak had exited before the VmHWM sum was taken; ` +
        `a transient-worker artefact of summing the tree at two different instants, not a fabricated value)`);
  };
  // Walked PER CELL: memory lives per-cell now, and per-cell memory is itself what makes a gateway a
  // matrix publisher, so reading only the top-level block would starve the required C7.hwm coverage
  // token on an all-new-shape field run.
  const ups = rawMatrix.upstreams && typeof rawMatrix.upstreams === "object" ? rawMatrix.upstreams : null;
  if (ups) {
    for (const [egress, up] of Object.entries(ups))
      for (const [ingress, cell] of Object.entries((up && up.cells) || {}))
        one(`${ingress}->${egress}.memory`, cell && cell.memory);
  } else {
    // v1-shape artifact: the top-level `cells` IS the one measured egress row. Only walked when there is
    // no upstreams grid, because v2 shares those cell objects with upstreams and would double-count.
    for (const [ingress, cell] of Object.entries(rawMatrix.cells || {}))
      one(`${ingress}.memory`, cell && cell.memory);
  }
  one("memory", rawMatrix.memory);   // legacy pre-redesign top-level block, still checked where present
  return { warnings, checked };
}

// The gross-inversion ceiling and C6_GROSS_PCT constant that used to live here are retired: the two
// fields they capped are deleted, and the frontier's ordering makes that inversion unrepresentable.
// bench-audit.py used to parse C6_GROSS_PCT to catch drift between copies; once the constant governed
// nothing, that check was pure decoration and went too. C6 below checks frontier ordering/disclosure.

// sweepSpreadPct(sweep, winner): the rung-to-rung scatter of a sweep, as a percentage of the winning
// rps - the cell's OWN measured noise. Null when there are fewer than two rungs: a sweep that probed
// once has not measured its own variability, and the caller must not treat an inversion as excusable.
function sweepSpreadPct(sweep, winner) {
  if (!Array.isArray(sweep) || sweep.length < 2 || !(winner > 0)) return null;
  const vals = sweep.map((r) => (r && typeof r.rps === "number" ? r.rps : null)).filter((v) => v != null);
  if (vals.length < 2) return null;
  return ((Math.max(...vals) - Math.min(...vals)) / winner) * 100;
}
// peakRanOutOfLadder(sweep, winnerConc): did the peak sweep WIN at the highest concurrency it probed?
// Then it never observed a fall-off, so it never established a ceiling - a hard error at any magnitude.
function peakRanOutOfLadder(sweep, winnerConc) {
  if (!Array.isArray(sweep) || sweep.length < 2 || winnerConc == null) return false;
  const concs = sweep.map((r) => (r && typeof r.conc === "number" ? r.conc : null)).filter((v) => v != null);
  if (concs.length < 2) return false;
  return Number(winnerConc) === Math.max(...concs);
}
// C6: THE FRONTIER MUST NOT INVERT, AND A READING MUST NOT CLAIM MORE THAN IT PROVED.
//
// C6 used to compare `rps_sustained_20ms` against `rps_max_proxy` and fail when "sustained" exceeded
// "maximum" - which happened because the two numbers came from different algorithms over one set of
// windows, and the bisection reached rungs the plateau search had already quit before.
//
// Both scalars are gone; the frontier makes that inversion UNREPRESENTABLE (a reading is a maximum
// over the rungs qualifying at its bound, and relaxing a bound only adds rungs, so a looser reading
// cannot be smaller). This checks it anyway - an invariant nothing verifies is an invariant nobody
// notices breaking.
export function c6Inversions(gwKey, rawMatrix) {
  const violations = [];
  const warnings = [];
  let cellsChecked = 0;
  for (const [egress, up] of Object.entries(upstreamsOf(rawMatrix))) {
    for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
      const perf = cell && cell.served === true && cell.perf;
      if (!perf || !Array.isArray(perf.frontier) || !perf.frontier.length) continue;
      cellsChecked += 1;
      const at = `${gwKey}.${ingress}->${egress}`;

      // (1) Bounds ascend, unbounded reading last - the sequence must read as the tradeoff curve it is.
      const bounds = perf.frontier.map((r) => (r.p99_bound_us == null ? Infinity : r.p99_bound_us));
      for (let i = 1; i < bounds.length; i += 1) {
        if (!(bounds[i] > bounds[i - 1])) {
          violations.push(`${at}: the frontier's bounds are not ascending (${bounds.join(", ")}) - the ` +
            `sequence must read as a curve, with the unbounded reading last`);
          break;
        }
      }

      // (2) Monotonicity: relaxing the bound can only widen the rung set a maximum is taken over, so a
      // looser reading cannot be smaller.
      let prev = null;
      let prevAt = null;
      for (const r of perf.frontier) {
        const v = typeof r.rps === "number" ? r.rps : null;
        if (v == null) continue;
        const label = r.p99_bound_us == null ? "unbounded" : `${r.p99_bound_us / 1000}ms`;
        if (prev != null && v < prev) {
          violations.push(`${at}: the frontier inverts - ${label} reads ${v} but the tighter ${prevAt} ` +
            `reads ${prev}. Relaxing a tail bound can only ADD rungs to the set the maximum is taken ` +
            `over, so a looser reading cannot be smaller: this cannot happen unless the readings came ` +
            `from different rungs than they claim`);
        }
        prev = v;
        prevAt = label;
      }

      // (3) A reading must not claim a ceiling it did not establish. `lower_bound` is true exactly when
      // the winning rung is the highest one probed - we stopped because the range ended, not because
      // the gateway did. The retired check (`peakRanOutOfLadder`) treated this as a violation; it is a
      // fact that must be DISCLOSED, so what's checked is that the disclosure agrees with the rungs.
      const topProbed = Math.max(
        0,
        ...(Array.isArray(perf.sweep_max_proxy) ? perf.sweep_max_proxy : []).map((p) => Number(p.conc) || 0),
      );
      for (const r of perf.frontier) {
        if (typeof r.rps !== "number" || typeof r.concurrency !== "number" || !topProbed) continue;
        const label = r.p99_bound_us == null ? "unbounded" : `${r.p99_bound_us / 1000}ms`;
        const shouldBeLower = r.concurrency >= topProbed;
        if (shouldBeLower !== (r.lower_bound === true)) {
          violations.push(`${at}: the ${label} reading won at c=${r.concurrency} of ${topProbed} probed ` +
            `but reports lower_bound=${r.lower_bound === true} - a rate is a floor rather than a ceiling ` +
            `exactly when nothing above it was looked at, and the artifact must say which it is`);
        }
      }
    }
  }
  return { violations, warnings, cellsChecked };
}

// ---- C9 WAS HERE, AND IT WAS WRONG ----------------------------------------------------------------
// It failed the build whenever a published p99 sat below its own p50 - true of two percentiles of one
// distribution, false of every pair this board actually publishes, because all of them are
// DIFFERENCES (added_gap_p50 = P50(gateway gaps) - P50(mock gaps), etc). If the mock's own jitter
// grows faster into the tail, the difference legitimately shrinks; plano's p50=15us/p99=11us was
// flagged as broken when it wasn't.
//
// The real defect C9 was written for was different: added_ttft_p50 came from a SINGLE stream while
// added_ttft_p99 came from a hundred samples - two populations under one name, indistinguishable from
// legitimate shrinkage at the data level. That guard is structural now, in metric.rs, asserted by
// `the_ttft_percentiles_come_from_one_sample_set_so_p99_can_never_sit_below_p50`.
//
// Left as a comment rather than deleted silently: a guard removed without its reasoning is
// indistinguishable from one lost in a refactor.

// ---- C8: ONE ENGINE PER BOARD ---------------------------------------------------------------------
// The board's claim is that every gateway was measured by the same instrument, so only the gateway
// should differ between columns. An instrument change (e.g. a mock rebuild altering which cells are
// judged served) can otherwise look indistinguishable from simultaneous regressions across the board.
//
// The engine commit is therefore data, and disagreement is a publish failure. Three ways to fail:
//   - two published gateways carry different engine commits (a mixed board)
//   - a gateway carries `dirty: true` (a modified tree does not identify what actually ran)
//   - a gateway carries no engine stamp at all while another does (silent pre-stamp data mixed in)
// A board where NO gateway is stamped is left alone (pre-stamp data); the moment one stamped run
// lands, all of them must be. Commits the repo ATTESTS as the same instrument (with built-artifact
// evidence, see site/instrument-equivalence.json) count as equal. Returns commit -> instrument id; an
// unattested commit maps to itself, degrading to plain commit-equality.
export function instrumentOf(fileText) {
  const map = new Map();
  let doc;
  try { doc = JSON.parse(fileText); } catch { return map; }
  for (const inst of (doc && doc.instruments) || []) {
    if (!inst || !inst.id || !Array.isArray(inst.commits)) continue;
    // An entry with no artifact evidence is INERT, not trusted: identical binaries are what admits an
    // entry, and a rule nothing enforces is a comment.
    const ev = inst.evidence && inst.evidence.otb_release_sha256;
    const hashes = ev ? Object.values(ev) : [];
    const proven = hashes.length >= inst.commits.length && new Set(hashes).size === 1;
    if (!proven) continue;
    for (const c of inst.commits) map.set(c, inst.id);
  }
  return map;
}

export function engineAgreement(gwKeys, resolve = (k) => newestSnapshotOnDisk(k), opts = {}) {
  const errors = [];
  const seen = new Map();   // instrument id -> [gwKey]
  const commitsOf = new Map();  // instrument id -> Set(commit)
  const unstamped = [];
  let checked = 0;
  const equiv = opts.equivalence !== undefined
    ? opts.equivalence
    : instrumentOf(readFileSync(join(ROOT, "site", "instrument-equivalence.json"), "utf8"));
  for (const k of gwKeys) {
    const found = resolve(k);
    const eng = found && found.snap && found.snap.rig && found.snap.rig.engine;
    if (!eng || !eng.commit) { unstamped.push(k); continue; }
    checked += 1;
    if (eng.dirty === true)
      errors.push(`C8: ${k} was measured by a DIRTY harness tree (engine.commit=${eng.commit.slice(0, 12)} with uncommitted edits) - the commit does not identify what ran, so this run is not reproducible and must be re-measured on a clean tree`);
    // The instrument, not the commit: two commits can build the same binaries measured the same way,
    // and failing that costs a whole field re-run to change nothing.
    const inst = equiv.get(eng.commit) || eng.commit;
    if (!seen.has(inst)) { seen.set(inst, []); commitsOf.set(inst, new Set()); }
    seen.get(inst).push(k);
    commitsOf.get(inst).add(eng.commit);
  }
  if (checked > 0 && unstamped.length > 0)
    errors.push(`C8: ${unstamped.length} gateway(s) carry no engine stamp (${unstamped.join(", ")}) while ${checked} do - the board would be mixing pre-stamp data with stamped data and cannot show they came from the same harness; re-measure the unstamped gateways`);
  if (seen.size > 1) {
    const groups = [...seen.entries()].map(([c, ks]) => `${c.slice(0, 12)}: ${ks.join(", ")}`).join(" | ");
    const msg = `C8: the board mixes ${seen.size} harness engines (${groups}) - columns measured by different instruments are not comparable, so a defect fixed between those commits applies to only part of the field; re-run the lagging gateways on the newest engine`;
    // The override, for the rare case the board must ship over C8's objection: deliberately awkward,
    // takes the reason as its value (so it can't be set by accident), and doesn't silence anything -
    // the mix is still reported/counted and the override rides along in the result, so the caller
    // publishes the fact that a human overrode a publish guard.
    const why = (opts.override !== undefined ? opts.override : process.env.OTB_ALLOW_MIXED_ENGINES) || "";
    if (why.trim().length >= 12) {
      return { errors, checked, commits: [...seen.keys()], overridden: { check: "C8.mix", reason: why.trim(), detail: msg } };
    }
    if (why.trim().length > 0)
      errors.push(`C8: OTB_ALLOW_MIXED_ENGINES was set to ${JSON.stringify(why)}, which is not a reason - the override takes the justification as its value (12 characters or more) so that what gets published records WHY a publish guard was overridden`);
    errors.push(msg);
  }
  return { errors, checked, commits: [...seen.keys()] };
}

// The raw matrix on disk - the INDEPENDENT oracle (Design F R1). Never read through the accessor.
//
// rawMatrixFor() resolves the same artifact gen-data resolved, but via its own independent
// re-derivation of the selection rule (newest snapshot by measured_at, over the per-suite file when
// at least as new), never by importing gen-data. Must cover every gateway including snapshot-sourced
// ones, or the oracle goes blind once every row is snapshot-sourced. R3 then asserts this independent
// resolution AGREES with the bundle's own stamp, catching a selection bug rather than mirroring it.
const SNAP_DIR = join(ROOT, "results", "snapshots");

function readJsonOrNull(p) {
  if (!existsSync(p)) return null;
  try { return JSON.parse(readFileSync(p, "utf8")); } catch { return null; }
}

// The newest snapshot for a gateway, by its own measured_at. Returns {snap, file} or null.
export function newestSnapshotOnDisk(gwKey, dir = SNAP_DIR) {
  if (!existsSync(dir)) return null;
  let best = null, bestFile = null, bestMs = -1;
  for (const f of readdirSync(dir)) {
    if (!f.startsWith(`result_${gwKey}_`) || !f.endsWith(".json")) continue;
    const snap = readJsonOrNull(join(dir, f));
    if (!snap) continue;
    const ms = snap.measured_at ? Date.parse(snap.measured_at) : 0;
    if (ms > bestMs) { bestMs = ms; best = snap; bestFile = f; }
  }
  return best ? { snap: best, file: bestFile } : null;
}

// rawMatrixFor(gwKey) -> { matrix, origin, file } | null. `origin` is "snapshot" | "suite".
function rawMatrixFor(gwKey) {
  const suite = readJsonOrNull(join(ROOT, "results", "matrix", `${gwKey}.json`));
  const found = newestSnapshotOnDisk(gwKey);
  const snapMs = found && found.snap.matrix && found.snap.measured_at ? Date.parse(found.snap.measured_at) : NaN;
  const suiteMs = suite && suite.measured_at ? Date.parse(suite.measured_at) : NaN;
  const s = Number.isFinite(snapMs) ? snapMs : -1;
  const d = Number.isFinite(suiteMs) ? suiteMs : -1;
  if (found && found.snap.matrix && (!suite || s >= d))
    return { matrix: found.snap.matrix, origin: "snapshot", file: found.file };
  if (suite) return { matrix: suite, origin: "suite", file: `results/matrix/${gwKey}.json` };
  return null;
}

function rawMatrix(gwKey) {
  const r = rawMatrixFor(gwKey);
  return r ? r.matrix : null;
}

// hasCellMemory(m): does this matrix carry a per-cell memory window on any served cell? Memory projects
// no per-gateway record, so "is this row publishing memory?" can only be asked of the cells themselves.
// Exported so test.mjs's BOARD_HAS_DATA reuses this predicate verbatim - the guard and its harness must
// agree on what "populated" means, or one can declare a board the other considers empty.
export function hasCellMemory(m) {
  if (!m || typeof m !== "object") return false;
  for (const cells of [m.cells, ...Object.values(m.upstreams || {}).map((u) => u && u.cells)]) {
    if (!cells || typeof cells !== "object") continue;
    for (const cell of Object.values(cells)) {
      if (cell && cell.served === true && cell.memory && typeof cell.memory === "object") return true;
    }
  }
  return false;
}

// ---- the caption-literal lint (C3) + accessor-routing lint (C5) --------------
// A source token in a per-datum caption literal is the bug class (memory mislabelled "6x6"). The lint
// scans the caption-RENDERING regions of app.js - SWEEP_CAPTION is the ONE allowed home for source
// tokens. Tab-level methodology prose is explicitly out of scope; C3 targets
// caption(cell)/pathNote/pill/annot renderers.

function readSrc(rel) {
  const p = join(HERE, rel);
  return existsSync(p) ? readFileSync(p, "utf8") : "";
}

// ---- the lints, as PURE EXPORTED FUNCTIONS (audit #20) -----------------------------------------
// Each lint used to be an inline loop over the repo's own source, so it could only ever be observed in
// its GREEN state - no RED-before test could prove it FIRES on the bug it claims to catch (C5's
// couldn't fire at all, see below). Extracted as pure source-text -> findings functions, they're
// unit-testable against synthetic source that CONTAINS the violation.

// (1) SWEEP-KEY LEAK (C3a): the internal sweep keys are caption VOCABULARY and live ONLY in the
// caption table / the seal. A key appearing as a user-facing string literal anywhere else is caption
// drift. THE TOKEN IN ANY QUOTE, AND AN EXEMPTION NARROW ENOUGH TO MEAN SOMETHING.
//
// Matches the token in ANY quote style (not just double-quoted), so a single-quoted literal, a
// template literal or an f-string carrying the same token can't walk past the lint. The exemption
// covers only the one thing it was written for: the token appearing as the VALUE of a `sweep` key
// (provenance DATA assignment) - checked against the text immediately preceding that token, not
// against the whole line, so a caption renderer's own reference to "source" doesn't blind it.
export const SWEEP_KEY_TOKENS = ["6x6-diagonal", "6x6-translation", "6x6-memory-window",
  "6x6-memory-diagonal", "6x6-memory-translation", "6x6-stream-diagonal", "6x6-stream-translation",
  "perf-suite", "xlate-suite", "stream-suite"];
export const SWEEP_KEY_RE = new RegExp(`(["'\`])(?:${SWEEP_KEY_TOKENS.join("|")})\\1`);
export function lintSweepKeys(src, name, allowRegion) {
  const errors = [];
  let inAllowed = false, sawRegion = false;
  const scan = new RegExp(SWEEP_KEY_RE.source, "g");
  src.split("\n").forEach((line, i) => {
    if (allowRegion.enter.test(line)) { inAllowed = true; sawRegion = true; }
    if (inAllowed) { if (allowRegion.exit.test(line)) inAllowed = false; return; }
    const code = line.replace(/\/\/.*$/, "").replace(/#.*$/, "");
    for (const m of code.matchAll(scan)) {
      // rec.source = {…sweep: "6x6-diagonal"…} is legitimate PROVENANCE DATA assignment, not a
      // caption - judged by what sits immediately before THIS token.
      if (/\bsweep["'`]?\s*[:=]\s*$/.test(code.slice(0, m.index))) continue;
      errors.push(`C3: ${name}:${i + 1} a sweep-key token leaked into a caption literal (keys live only in SWEEP_CAPTION): ${line.trim().slice(0, 80)}`);
      break;
    }
  });
  // COVERAGE means "the scanner actually parsed this file and FOUND its allowed region" - proof the
  // lint is live. The old tag was set by the EXEMPTION path too, so a lint that never scanned anything
  // still reported itself covered (audit #20).
  return { errors, scanned: sawRegion };
}

// (2) ACCESSOR ROUTING (C5): every read of a sealed metric's number must go through metric()/mval().
// The codebase typically binds the envelope to a local first (`const env = p[key]`) and reads
// `env.value` on a later line, so this lint is TAINT-BASED and whole-file rather than same-line: any
// identifier handed to an envelope predicate/accessor (isEnvelope/metric/mval, or _is_env/mval in
// Python) IS an envelope, and reading its raw `.value` / `.get("value")` outside the accessor
// definitions themselves bypasses the reader.
const JS_ACCESSOR_DEFS = /^\s*(?:export\s+)?function\s+(?:metric|mval|isEnvelope)\b|^\s*(?:export\s+)?const\s+(?:metric|mval|isEnvelope)\s*=/;
const PY_ACCESSOR_DEFS = /^\s*def\s+(?:mval|mvalid|menote|_is_env)\b/;
// THE FIELD-NAME LINT POLICES THE WHOLE SEALED VOCABULARY, NOT FOUR NAMES OF IT.
//
// The direct-form scan used to iterate GATED_FIELDS (four throughput metrics) and nothing else, so
// ~16 other sealed fields (latency, ttft, gap, growth, plateau, RSS) could be dereferenced straight to
// their raw number and still report green. A lint that knows a fifth of its own vocabulary is the
// inert-check failure in miniature: it fires often enough to look alive while the bug class walks past.
//
// So the direct form is DISCOVERED rather than enumerated: each raw-deref spelling captures the FIELD
// NAME it dereferences, and seal.mjs's own `isMetricField` (the same predicate gen-data seals by and
// C1 asserts by) decides whether that name is sealed. No second list to keep in step - a field added
// to seal.mjs's vocabulary is policed here the moment it's added, and the RSS family (discovered by
// regex, never enumerated anywhere) is covered for the first time.
const ID = "[A-Za-z_$][\\w$]*";
// Each entry captures group 1 = the dereferenced field name.
const JS_FIELD_DEREFS = [
  new RegExp(`\\.(${ID})\\s*\\.value\\b`, "g"),                                        // .field.value
  new RegExp(`\\.(${ID})\\s*\\[\\s*["'\`]value["'\`]\\s*\\]`, "g"),                     // .field["value"]
  new RegExp(`\\[\\s*["'\`](${ID})["'\`]\\s*\\]\\s*\\.value\\b`, "g"),                  // ["field"].value
  new RegExp(`\\[\\s*["'\`](${ID})["'\`]\\s*\\]\\s*\\[\\s*["'\`]value["'\`]\\s*\\]`, "g"), // ["field"]["value"]
  new RegExp(`\\{[^}]*\\bvalue\\b[^}]*\\}\\s*=\\s*[^;]*\\.(${ID})\\b`, "g"),            // const { value } = x.field
];
const PY_FIELD_DEREFS = [
  new RegExp(`\\.(${ID})\\b[^\\n]*?\\.get\\(\\s*["']value["']`, "g"),
  new RegExp(`\\.(${ID})\\b[^\\n]*?\\[\\s*["']value["']\\s*\\]`, "g"),
  new RegExp(`\\[\\s*["'](${ID})["']\\s*\\][^\\n]*?\\[\\s*["']value["']\\s*\\]`, "g"),
  new RegExp(`\\[\\s*["'](${ID})["']\\s*\\][^\\n]*?\\.get\\(\\s*["']value["']`, "g"),
];
// A SEALED FIELD BOUND TO A LOCAL IS AN ENVELOPE, whether or not that local ever meets an accessor:
//   const rec = g.best_cell.added_latency_p50_us; return rec.value;
// routes around both passes below (the binding line carries no `.value`, and `rec` was never handed
// to an accessor, so the taint set never learned it was an envelope). The field's OWN NAME is
// sufficient evidence of the type, so a binding read off a sealed field taints its target exactly as
// an accessor call does.
const JS_BINDING = new RegExp(`(?:const|let|var)\\s+(${ID})\\s*=\\s*([^;\\n]*)`, "g");
const PY_BINDING = new RegExp(`^\\s*(${ID})\\s*=\\s*([^\\n]*)`, "gm");
const JS_FIELD_IN_EXPR = new RegExp(`\\.(${ID})\\b|\\[\\s*["'\`](${ID})["'\`]\\s*\\]`, "g");
const PY_FIELD_IN_EXPR = new RegExp(`\\.get\\(\\s*["'](${ID})["']|\\[\\s*["'](${ID})["']\\s*\\]|\\.(${ID})\\b`, "g");
export function lintAccessorRouting(src, name, lang = "js") {
  const errors = [];
  const lines = src.split("\n");
  const isPy = lang === "py";
  const accessorDef = isPy ? PY_ACCESSOR_DEFS : JS_ACCESSOR_DEFS;
  const predicate = isPy ? /\b(?:_is_env|mval|mvalid|menote)\(\s*([A-Za-z_$][\w$]*)\s*[),]/g
    : /\b(?:isEnvelope|metric|mval)\(\s*([A-Za-z_$][\w$]*)\s*[),]/g;
  // EVERY SPELLING OF "read the raw number off the envelope", not just the dotted one - `env.value`,
  // `env["value"]`, `env[key].value`, `const { value } = env` all bypass the accessor in a few
  // keystrokes and would otherwise report green, and the bracket form is the one a reader reaches for
  // FIRST when the field name is a variable.
  const valueReads = (v) => isPy
    ? [new RegExp(`\\b${v}\\.get\\(\\s*["']value["']`),
      new RegExp(`\\b${v}\\s*\\[\\s*["']value["']\\s*\\]`),
      new RegExp(`\\b${v}\\s*\\[[^\\]]*\\]\\s*\\.get\\(\\s*["']value["']`),
      new RegExp(`\\b${v}\\s*\\[[^\\]]*\\]\\s*\\[\\s*["']value["']\\s*\\]`)]
    : [new RegExp(`\\b${v}\\.value\\b`),
      new RegExp(`\\b${v}\\s*\\[\\s*["'\`]value["'\`]\\s*\\]`),
      new RegExp(`\\b${v}\\s*\\[[^\\]]*\\]\\s*\\.value\\b`),
      new RegExp(`\\{[^}]*\\bvalue\\b[^}]*\\}\\s*=\\s*${v}\\b`)];
  // PASS 1 (whole file, so a deref on a LATER line than the binding is still caught): every identifier
  // ever handed to an envelope accessor/predicate is an envelope-typed local.
  const tainted = new Set();
  for (const m of src.matchAll(predicate)) tainted.add(m[1]);
  // PASS 1b: ...and so is every identifier bound DIRECTLY OFF A SEALED FIELD (see JS_BINDING above) -
  // the field's own name is the type evidence, no accessor call needed.
  {
    const bind = isPy ? PY_BINDING : JS_BINDING;
    const fieldIn = isPy ? PY_FIELD_IN_EXPR : JS_FIELD_IN_EXPR;
    bind.lastIndex = 0;
    for (const m of src.matchAll(bind)) {
      const rhs = m[2] || "";
      fieldIn.lastIndex = 0;
      for (const f of rhs.matchAll(fieldIn)) {
        const nm = f.slice(1).find((x) => x != null);
        if (nm && isMetricField(nm)) { tainted.add(m[1]); break; }
      }
    }
  }
  // PASS 2: mark the line ranges that ARE the accessor definitions - the one place `.value` is legal.
  const inAccessor = new Array(lines.length).fill(false);
  for (let i = 0; i < lines.length; i++) {
    if (!accessorDef.test(lines[i])) continue;
    const indent = (lines[i].match(/^\s*/) || [""])[0].length;
    for (let j = i; j < lines.length; j++) {
      inAccessor[j] = true;
      if (j > i && lines[j].trim() && (lines[j].match(/^\s*/) || [""])[0].length <= indent &&
        !/^\s*[})\]]/.test(lines[j])) break;
      if (j > i && new RegExp(`^\\s{0,${indent}}[}]`).test(lines[j])) break;
    }
  }
  lines.forEach((line, i) => {
    if (inAccessor[i]) return;
    const code = isPy ? line.replace(/#.*$/, "") : line.replace(/\/\/.*$/, "");
    // (a) the direct form: a metric FIELD dereferenced straight to its raw number, in any spelling.
    // The field name is DISCOVERED from the deref and judged by seal.mjs's isMetricField, covering the
    // whole sealed vocabulary rather than the four GATED_FIELDS it used to know about.
    for (const re of (isPy ? PY_FIELD_DEREFS : JS_FIELD_DEREFS)) {
      re.lastIndex = 0;
      for (const m of code.matchAll(re)) {
        const f = m[1];
        if (!f || !isMetricField(f)) continue;
        errors.push(`C5: ${name}:${i + 1} reads .${f}'s raw value directly (must route through metric()/mval()): ${line.trim().slice(0, 80)}`);
      }
    }
    // (b) the form the codebase ACTUALLY uses: an envelope bound to a local, then dereferenced.
    for (const v of tainted) {
      if (valueReads(v).some((re) => re.test(code)))
        errors.push(`C5: ${name}:${i + 1} reads the raw value off envelope-typed \`${v}\` (must route through metric()/mval()): ${line.trim().slice(0, 80)}`);
    }
  });
  return { errors, scanned: lines.length > 1, tainted };
}

// (2b) COVERAGE WIRING (R2): every declared invariant branch has a LIVE call site tagging it.
//
// R2's runtime coverage check asks "was every required branch exercised by this bundle" - which has
// no honest answer mid-run (a branch can be silent for lack of input yet), so that finding is
// downgraded to a warning while the board is filling. That meant on a partial board the ERROR arm was
// unreachable: commenting out the one line tagging R1.oracle left both check-consistency.mjs and
// test.mjs exiting 0 - the independent oracle could be disabled entirely and the gate stayed green.
//
// "No input yet" and "not wired up" are different facts; only the first depends on board completeness.
// The second is a property of the SOURCE - a branch no surviving call site tags can never be exercised
// by any bundle - so it's checked statically against this file's own text, unconditionally.
//
// A commented-out call site is not a call site (a leading `//` is how a check gets switched off
// "temporarily"). The reverse is checked too: a call site tagging an undeclared branch is a token that
// can never be required (this is how C8.engine was tagged-but-undeclared, hard-failing every stamped
// board).
export function lintCoverageWiring(src, branches) {
  const errors = [];
  const live = new Set();
  src.split("\n").forEach((line) => {
    for (const m of line.matchAll(/covered\(\s*["'`]([^"'`]+)["'`]\s*\)/g)) {
      if (line.slice(0, m.index).includes("//")) continue;   // commented out = switched off, not wired
      live.add(m[1]);
    }
  });
  for (const b of branches)
    if (!live.has(b))
      errors.push(`R2: wiring - declared invariant branch "${b}" has NO live call site tagging it in check-consistency.mjs ` +
        `(the branch cannot be exercised by ANY bundle, so its silence is not "no input yet" - the check is off)`);
  for (const b of live)
    if (!branches.includes(b))
      errors.push(`R2: wiring - branch "${b}" is tagged in check-consistency.mjs but is not declared in CHECK_BRANCHES ` +
        `(an undeclared token can never be REQUIRED of anything, so the branch it names is unguarded)`);
  return { errors, live };
}

// C3b AND C3c ARE GONE WITH THE PIPELINE THEY POLICED.
//
// C3b asserted each of charts.py's three lanes fed its own source stamp to `_sweep_label`; C3c
// compared charts.py's SWEEP_CAPTION key set to app.js's, so two caption vocabularies couldn't drift
// apart. Both existed because the board had a second renderer, in another language. It no longer does
// - the Charts tab draws from the bundle in the browser, so there's one renderer, one vocabulary,
// nothing to sync. A guard parsing a deleted file can't fail honestly.

// ---- the INDEPENDENT ORACLE (Design F R1) --------------------------------------------------------
// Re-derive what a metric MUST publish from the RAW artifact, through a path DISJOINT from
// metric()/seal.mjs.
//
// The oracle no longer re-derives a gate (there is no gate: a present number is always published, see
// seal.mjs). What it re-derives is the two facts published alongside the number - the rig ceiling the
// measurement was taken against, and the fraction of it reached - since anything published must be
// verified, or a headroom fraction could attach to the wrong metric and report green.
//
// WHICH FIELDS' FACTS A METRIC CARRIES: almost always its own, except `streams_sustained_fps`, which
// is produced by the same bisect as `streams_sustained` and so inherits that count's facts (gen-data
// seals it that way, see AUDIT #11 there). Looking up a name that's never written yields `undefined`
// and demands no headroom for a rate correctly annotated - the shape of the bug that blocked deploy
// for two days under the retired flag's name.
const STREAM_FACT_OWNER = { streams_sustained_fps: "streams_sustained" };
export function comparisonFactsFor(raw, field) {
  const owner = STREAM_FACT_OWNER[field] || field;
  // The stream metrics' ceiling is DERIVED from the mock's pacing and named `*_mock_ceiling`; the
  // throughput metrics' is MEASURED and named `*_rig_ceiling`. One of the two is present per field.
  const ceiling = raw[`${owner}_mock_ceiling`] ?? raw[`${owner}_rig_ceiling`] ?? null;
  return { headroom: raw[`${owner}_headroom`] ?? null, ceiling };
}

// WHAT THE BOARD SHOULD SHOW for one raw value, resolved by the same function the seal uses.
//
// This used to restate the rule and implemented only the capacity branch, so every paced field with
// mock_bound=true had the seal correctly publish while the oracle demanded null - 25 mismatches
// blocked the deploy for two days. An independent oracle is worth having because it re-derives the
// answer from the RAW artifact, not because it owns a second copy of the display rule: a second copy
// doesn't catch drift, it IS the drift.
// How many gateways the repo DECLARES, read from the manifests rather than from the bundle: the
// bundle only knows who has published, which is the very thing being judged.
export function declaredGatewayCount() {
  try {
    const dir = join(ROOT, "gateways");
    if (!existsSync(dir)) return 0;
    return readdirSync(dir).filter((d) => {
      try { return statSync(join(dir, d)).isDirectory() && existsSync(join(dir, d, "definition.json")); }
      catch { return false; }
    }).length;
  } catch { return 0; }
}

export function oracleExpected(raw, absentReason = null) {
  return displayedValue(raw, { absentReason });
}

// WHAT THE WHOLE ENVELOPE MUST SAY, not only what number it must show.
//
// Comparing only `.v` left everything the envelope says ABOUT its number unverified - a reason
// flattened back to "not_measured", a zero-note swapped so a measured streaming failure published as
// a missing RPS ceiling, a lost `detail` (the evidence a reader is shown) - all of which preserve the
// number and so all verified green. The reason IS data, so the oracle re-derives it like the value:
//   value    - through displayedValue, the one display rule.
//   reason   - the engine's absence token for a hole.
//   note     - a certified 0's meaning, from seal.mjs's zeroNoteFor (the one place that map lives).
//   detail   - the engine's prose for the absence, which must survive the seal.
//   headroom - the fraction of the rig's own ceiling the measurement reached, and
//   ceiling  - the ceiling that fraction is of. Both null when the engine had no usable reference.
//
// The `reason: "mock_bound"|"unverifiable"` branch is gone with the suppression that produced it.
// Nothing here can return a null value for a raw number that is present - the property C2 asserts
// from the other direction.
export function oracleEnvelope(raw, { absent = null, zeroNote = null, headroom = null, ceiling = null } = {}) {
  const absentReason = absent && absent.reason ? absent.reason : null;
  const v = displayedValue(raw, { absentReason });
  const facts = {
    headroom: Number.isFinite(headroom) ? headroom : null,
    ceiling: Number.isFinite(ceiling) ? ceiling : null,
  };
  const none = { v, reason: null, note: null, detail: null, ...facts };
  if (raw == null)
    return { ...none, reason: absentReason || "not_measured", detail: (absent && absent.detail) || null,
      // An absent metric publishes no comparison: seal.mjs attaches facts through `withExtras`, which
      // the absent branch returns before reaching.
      headroom: null, ceiling: null };
  if (Number(raw) === 0) return { ...none, v: 0, note: zeroNote };
  return none;
}

// ---- R4: the SELECTION itself, re-derived --------------------------------------------------------
// The oracle above verifies the numbers at the coordinates the BUNDLE CLAIMS, leaving the choice of
// coordinates unchecked - a projection that picked the wrong cell publishes correct values under the
// wrong name, and every value comparison agrees with it.
//
// These re-derive WHICH cell each projected record must be, from the raw artifact and the PUBLISHED
// rule (gen-data.mjs bestCell/translationCell) - a second implementation of a ranking, which is what
// deferred it, but the alternative verifies the arithmetic of an answer without ever asking whether
// it's the answer to the right question. Written against the rule as stated in gen-data's own
// comments, so a deliberate rule change fails here loudly and must be updated in both places.
// oracleAddedP99Rank(cell): the rank a candidate sorts by. A measured p99 ranks as itself; a
// below-resolution absence ranks 0 (the best reading the rig can express, not a hole); anything else
// sorts last.
export function oracleAddedP99Rank(cell) {
  const perf = cell && cell.perf;
  if (perf && perf.added_latency_p99_us != null) return perf.added_latency_p99_us;
  const a = absentEntryOf(cell && cell.absences, "perf", "added_latency_p99_us");
  return a && a.reason === "below_resolution" ? 0 : Infinity;
}
// The diagonal best_cell must be projected from: the canonical openai diagonal when it is served, else
// the served diagonal with the lowest p99 rank (first wins a tie, as the reduce does).
export function oracleBestDialect(m) {
  const diag = [];
  for (const [egress, up] of Object.entries(upstreamsOf(m))) {
    const cell = up && up.cells && up.cells[egress];
    if (cell && cell.served === true && cell.perf) diag.push({ dialect: egress, cell });
  }
  if (!diag.length) return null;
  const openai = diag.find((d) => d.dialect === "openai");
  const win = openai || diag.reduce((a, b) => (oracleAddedP99Rank(b.cell) < oracleAddedP99Rank(a.cell) ? b : a));
  return win.dialect;
}
// The translation cell that must be projected: the FAIR tier (openai ingress, identical input side on
// every gateway) when the matrix measured any, else any served cross-dialect cell it did measure;
// lowest p99 rank wins, first on a tie.
export function oracleTranslationPath(m) {
  const fair = [], any = [];
  for (const [egress, up] of Object.entries(upstreamsOf(m))) {
    for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
      if (ingress === egress) continue;
      if (!(cell && cell.served === true && cell.perf)) continue;
      if (oracleAddedP99Rank(cell) === Infinity) continue;
      const cand = { ingress, egress, cell };
      if (ingress === "openai") fair.push(cand);
      any.push(cand);
    }
  }
  const cands = fair.length ? fair : any;
  if (!cands.length) return null;
  const w = cands.reduce((a, b) => (oracleAddedP99Rank(b.cell) < oracleAddedP99Rank(a.cell) ? b : a));
  return { ingress: w.ingress, egress: w.egress };
}

// The engine's absence reason for one raw field, read from the raw cell's sibling `absences` map
// (block-prefixed keys: "perf.added_latency_p50_us"). The oracle needs it because a below_resolution
// absence DISPLAYS as 0 (see displayedValue), and an oracle blind to the reason would demand null for
// the very value the seal correctly publishes.
export function absentEntryOf(absences, prefix, k) {
  if (!absences || typeof absences !== "object") return null;
  return absences[`${prefix}.${k}`] || absences[k] || null;
}
export function absentReasonFor(absences, prefix, k) {
  const e = absentEntryOf(absences, prefix, k);
  return e && e.reason ? e.reason : null;
}

// opts.syntheticFixture: a HAND-BUILT fixture with no on-disk oracle (an invariant unit-test), so the
// "matrix-sourced publish must be oracle-verifiable" requirement is waived. AUDIT #18: replaces the
// old silent hatch (no results/matrix anywhere => oracle layer not required), which a REAL bundle
// could fall into. The waiver is now an explicit caller opt-in the CLI never passes.
export function checkConsistency(data, app, opts = {}) {
  const { syntheticFixture = false } = opts;
  const errors = [];
  const warnings = [];
  const cover = new Set();
  const covered = (tag) => cover.add(tag);
  // AUDIT #16: how many independent-oracle comparisons ACTUALLY ran. Coverage is claimed from this
  // counter, never from merely reaching the branch.
  let oracleCompared = 0;
  // AUDIT #21: coverage is PER-GATEWAY, not a single global counter - the old `oracleCompared > 0` gate
  // was satisfied by one oracled row, so twelve unoracled gateways still reported R1.oracle covered.
  // Reconciled at the end: every gateway publishing a matrix-sourced number must appear in oracledKeys.
  const matrixPublishers = new Set();
  const oracledKeys = new Set();
  // Gateways where a cell or block couldn't be compared at all. Subtracted before reconciliation below,
  // so "this gateway was oracled" means "all of it was" rather than "some of it was".
  const unverified = new Set();

  // ---- C1 + C2: envelope integrity across the WHOLE bundle -------------------
  // Walk every object. (C1) a *_mock_bound key must not survive; a gated metric field must be an
  // envelope, never a bare number. (C2) a suppressed envelope has value:null and no other numeric field.
  //
  // `timings_s` IS A COST RECORD, NOT A METRIC BLOCK: it maps each metric GROUP to seconds spent on
  // this cell, so it legitimately carries keys named `cpu_fps`/`streams_sustained` whose values are
  // DURATIONS. C1 matches on field NAME, so it misread those as unsealed metrics and blocked the
  // deploy the first time a real snapshot carried them. A duration is not a gateway measurement and
  // must not be sealed; the subtree is skipped whole, like rss_series/load_recipe.
  const walk = (node, path, inTimings = false) => {
    if (Array.isArray(node)) { node.forEach((v, i) => walk(v, `${path}[${i}]`, inTimings)); return; }
    if (node == null || typeof node !== "object") return;
    if (inTimings) return;
    for (const [k, v] of Object.entries(node)) {
      if (k === "timings_s") { covered("C1.timings"); continue; }
      /* `absences` IS A REASON MAP, NOT A METRIC BLOCK, skipped whole for the same reason as
         `timings_s`: its keys are metric names but its values are {reason, detail} records, never
         envelopes.

         It surfaced when memory absences appeared on refuted cells - `isMetricField` is false for
         "perf.added_latency_p50_us" and true for "memory.peak_rss_mib" (only memory registers
         prefixed names), so C1 was inspecting a third of this map and ignoring the rest.

         The map's own contract (every published null carries a reason) is checked by the absence
         invariants and bench-audit.py, which is where it belongs. */
      if (k === "absences") { covered("C1.absences"); continue; }
      if (k.endsWith("_mock_bound")) {
        errors.push(`C1: ${path}.${k} - a raw *_mock_bound flag survives in the bundle (must be consumed at seal time)`);
        covered("C1.mock_bound");
      }
      if (isMetricField(k)) {
        covered("C1.field");
        // EVERY MALFORMED SHAPE, not just a bare number. This used to check
        // `!isEnvelope(v) && typeof v === "number"`, which only caught a raw numeric leak - a metric
        // field carrying a string, boolean, array, or a HALF-BUILT envelope like `{value: 20057}` with
        // `certified` dropped matched neither branch and sailed through undetected.
      if (!isEnvelope(v) && v !== null && v !== undefined) {
          // The message names WHAT was found, because the fix for a bare number and the fix for a
          // half-built envelope are different, and `${v}` renders an object as "[object Object]".
          const shape = Array.isArray(v) ? "an array"
            : typeof v === "object" ? `a partial envelope missing \`certified\`: ${JSON.stringify(v)}`
            : `a bare ${typeof v}`;
          errors.push(`C1: ${path}.${k} is ${shape}, not a sealed envelope (a raw ungated metric field survives)`);
        } else if (isEnvelope(v)) {
          if (v.suppressed === true) {
            covered("C2.suppressed");
            if (v.value !== null) errors.push(`C2: ${path}.${k} is suppressed but value=${v.value} (a suppressed metric must expose no value)`);
            for (const [sk, sv] of Object.entries(v))
              if (sk !== "value" && typeof sv === "number")
                errors.push(`C2: ${path}.${k} is suppressed but carries a recoverable numeric field ${sk}=${sv}`);
          } else if (v.value != null) {
            covered("C1.certified");
          }
        }
      }
      walk(v, `${path}.${k}`);
    }
  };
  walk({ gateways: data.gateways || [] }, "data");

  // ---- C4: single path + no legacy leak --------------------------------------
  // ---- C9: a row suppressed for engine mismatch must be suppressed COMPLETELY -------------------
  //
  // OTB_SINGLE_ENGINE keeps the board single-instrument by blanking rows an older harness measured,
  // rather than mixing them in or overriding C8. That's honest only if "blank" means blank: a row
  // keeping one stale measurement while the rest went n/a would put another instrument's number on the
  // board with nothing marking it - worse than the mix C8 refuses, which at least declares itself.
  //
  // So suppression is VERIFIED here, not trusted: the list is the bundle's own published claim, and an
  // exemption a bundle can grant itself is not a check. A key on the list must carry no measurement
  // and must say what it's waiting for.
  const suppressedKeys = new Set(
    Array.isArray(data.suppressed_for_engine) ? data.suppressed_for_engine : []);
  for (const g of data.gateways || []) {
    if (suppressedKeys.has(g.key)) {
      covered("C9.suppressed");
      const leaked = ["matrix", "best_cell", "translation_cell", "streaming", "memory",
                      "snapshot_file", "rig", "measured_at", "lane_measured_at"]
        .filter((k) => g[k] != null);
      if (leaked.length)
        errors.push(`C9: ${g.key} is listed in suppressed_for_engine (n/a pending re-measurement) but still publishes ${leaked.join(", ")} - a partly-suppressed row puts another engine's number on the board with nothing marking it`);
      if (!g.awaiting_engine)
        errors.push(`C9: ${g.key} is suppressed but carries no awaiting_engine - a blank row must say whether it is blank because nothing was measured or because the board moved to a harness it has not been re-measured on`);
      if (g.engine && g.engine.current === true)
        errors.push(`C9: ${g.key} is suppressed as not-current but its own engine stamp claims current - suppression and the stamp disagree about the same fact`);
      // Nothing below applies: the row publishes no numbers, so there is nothing to oracle, no
      // provenance to agree about, and no envelope to seal.
      continue;
    }
    // No raw legacy suite object may survive in the emitted bundle (they are the stale reservoir).
    for (const suite of ["perf", "stream", "streamcpu", "xlate"]) {
      if (g[suite] != null) { errors.push(`C4: ${g.key}.${suite} - a raw legacy suite object leaked into the bundle`); covered("C4.leak"); }
    }
    // Memory is NOT in this list: it projects no per-gateway record any more (it is measured per cell and
    // read per cell), so it has no top-level source stamp to check. Its per-cell windows are stamped at
    // render time by app.js stampChosen, and C3's caption vocabulary covers those keys.
    for (const [name, cell] of [["best_cell", g.best_cell], ["translation_cell", g.translation_cell], ["streaming", g.streaming]]) {
      if (!cell) continue;
      covered("C4.cell");
      const src = cell.source;
      if (!src || !SOURCE_KINDS.has(src.kind))
        errors.push(`C4: ${g.key}.${name}.source.kind=${JSON.stringify(src && src.kind)} is not a known origin (${[...SOURCE_KINDS].join("|")})`);
      // C3 (data side): the caption stamp must be renderable through the ONE caption table.
      if (!src || !app.SWEEP_CAPTION[src.sweep])
        errors.push(`C3: ${g.key}.${name}.source.sweep=${JSON.stringify(src && src.sweep)} has no SWEEP_CAPTION renderer`);
      else covered("C3.stamp");
    }

    // ---- C6: sustained@20ms <= max_proxy on EVERY served cell (physical-plausibility invariant) ------
    // max_proxy is the unconstrained ceiling; sustained-under-SLO can't exceed it. Derived from the RAW
    // matrix cell (R1), never via the accessor. The two are swept in separate phases with separate
    // noise bands that legitimately overlap by a margin scaling with 1/throughput (sub-1% on a fast
    // gateway, up to ~8% on a ~500-rps one), so C6 WARNS on inversion rather than hard-failing - a hard
    // assert would false-fail every honest run on sub-noise inversions. max_proxy=0 means "did not
    // qualify", not an inversion, and is skipped. c6Inversions is a PURE EXPORTED FUNCTION so its
    // RED-before proof injects a synthetic inversion instead of depending on shipped data.
    const c6 = c6Inversions(g.key, rawMatrix(g.key));
    if (c6.cellsChecked > 0) covered("C6.cell");
    errors.push(...c6.violations);
    warnings.push(...(c6.warnings || []));
    const c7 = c7HwmBelowPeak(g.key, rawMatrix(g.key));
    if (c7.checked > 0) covered("C7.hwm");
    warnings.push(...c7.warnings);
    // ---- R1 independent oracle -------------------------------------------------------------------
    // Coverage is claimed only when a comparison ACTUALLY HAPPENED (oracleCompared increments in cmp()
    // below), never from merely reaching this branch. Re-derives EVERY sealed matrix cell (all perf +
    // stream metrics), the translation cell, the streaming record and the memory block, resolved from
    // the artifact the bundle actually projected from.
    const resolved = rawMatrixFor(g.key);
    const m = resolved ? resolved.matrix : null;
    // A gateway that publishes matrix-sourced numbers MUST be oracle-checkable: with no raw artifact on
    // disk at all, the oracle layer would silently become "not required" and an unverifiable publish
    // would pass. A row publishing ONLY per-cell memory off its matrix is just as matrix-sourced as one
    // publishing a best_cell, and must be just as oracle-checkable.
    const matrixSourced = [g.best_cell, g.translation_cell, g.streaming]
      .some((r) => r && r.source && r.source.kind === "matrix") || hasCellMemory(g.matrix);
    if (matrixSourced) matrixPublishers.add(g.key);
    if (matrixSourced && !m && !syntheticFixture)
      errors.push(`R2: ${g.key} publishes matrix-sourced numbers but no raw matrix artifact (snapshot or results/matrix/${g.key}.json) is on disk - the independent oracle cannot verify a single one of them (an unverifiable publish is a failure, not an exemption)`);
    // ---- R3: the oracle must be reading the SAME run the bundle published --------------------------
    // Compares the two measured_at stamps: a mismatch means the selection rules diverged (e.g. an
    // older snapshot shadowing a newer run), which would otherwise verify the wrong file and report
    // green. Also asserts the bundle's own "from snapshot" claim matches disk.
    if (m && !syntheticFixture) {
      const shownAt = g.matrix && g.matrix.measured_at ? Date.parse(g.matrix.measured_at) : NaN;
      const rawAt = m.measured_at ? Date.parse(m.measured_at) : NaN;
      covered("R3.selection");
      if (Number.isFinite(shownAt) && Number.isFinite(rawAt) && shownAt !== rawAt)
        errors.push(`R3: ${g.key}: the bundle published matrix measured_at=${g.matrix.measured_at} but the newest raw artifact on disk (${resolved.origin}: ${resolved.file}) is measured_at=${m.measured_at} - the board is rendering from a stale/mis-selected run`);
      const claimsSnapshot = g.matrix_from_snapshot === true;
      if (claimsSnapshot !== (resolved.origin === "snapshot"))
        errors.push(`R3: ${g.key}: the bundle claims matrix_from_snapshot=${claimsSnapshot} but the independently-resolved newest artifact is a ${resolved.origin} (${resolved.file}) - provenance disagreement`);
    }
    if (m) {
      // WHAT THE BUNDLE SHOWS for one sealed metric, as the same tuple oracleEnvelope re-derives: the
      // displayed number AND everything the envelope says about it. The number still comes through
      // app.metric(), the reader every surface uses; the rest is read off the envelope directly,
      // because it IS the envelope's own testimony and that is what is being verified.
      const shownOf = (env) => ({
        v: app.metric(env).v,
        reason: env && env.reason != null ? env.reason : null,
        note: env && env.note != null ? env.note : null,
        detail: env && env.detail != null ? env.detail : null,
        headroom: env && Number.isFinite(env.headroom) ? env.headroom : null,
        ceiling: env && Number.isFinite(env.rig_ceiling) ? env.rig_ceiling : null,
      });
      const say = (e) => `value=${e.v} reason=${e.reason} note=${e.note} headroom=${e.headroom} ceiling=${e.ceiling}` +
        (e.detail != null ? ` detail=${JSON.stringify(e.detail)}` : "");
      const cmp = (label, shown, expected) => {
        oracleCompared += 1;
        oracledKeys.add(g.key);
        for (const f of ["v", "reason", "note", "detail", "headroom", "ceiling"]) {
          if (shown[f] === expected[f]) continue;
          errors.push(`R1: ${g.key}.${label}: the RAW matrix on disk implies [${say(expected)}] but the sealed ` +
            `envelope carries [${say(shown)}] - they disagree on \`${f}\` (independent-oracle mismatch)`);
          return;
        }
      };
      // The oracle's own view of one raw field, with the comparison's facts resolved from the raw
      // artifact rather than read back off the bundle being judged.
      const expectOf = (rawSub, absences, prefix, k) => oracleEnvelope(rawSub[k], {
        ...comparisonFactsFor(rawSub, k),
        absent: absentEntryOf(absences, prefix, k),
        zeroNote: zeroNoteFor(k),
      });
      const cellAt = (ingress, egress) => rawCellAt(m, ingress, egress);
      // (a) EVERY sealed matrix cell (36 of them), perf + stream, gated + ungated.
      for (const [egress, up] of Object.entries((g.matrix && g.matrix.upstreams) || {})) {
        for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
          const at = `matrix[${ingress}->${egress}]`;
          const rawCell = cellAt(ingress, egress);
          // A CELL THE ORACLE CANNOT FIND IS NOT A CELL THE ORACLE CHECKED. Skipping in silence is how
          // a gateway whose cells the oracle never located still counted as an oracled gateway.
          if (!rawCell) {
            errors.push(`R1: ${g.key}.${at}: the bundle publishes this cell but the raw artifact on disk carries no ` +
              `cell at those coordinates - not one of its numbers can be verified`);
            unverified.add(g.key);
            continue;
          }
          // cell.memory joins perf/stream here: since memory became a PER-CELL record it is displayed
          // straight off these cells, so leaving it out would mean the board's whole memory lane
          // published unoracled numbers. Its fields are ungated (RSS + growth rate + time to plateau).
          for (const [prefix, sealedSub, rawSub] of [["perf", cell && cell.perf, rawCell.perf],
            ["stream", cell && cell.stream, rawCell.stream], ["memory", cell && cell.memory, rawCell.memory]]) {
            if (!sealedSub && !rawSub) continue;
            // A WHOLE BLOCK MISSING ON ONE SIDE IS THE LOUDEST THING IN THIS FILE, not the quietest.
            // `!sealedSub || !rawSub -> continue` used to skip a gateway's entire perf/stream/memory
            // lane silently while it still earned oracle credit from whichever block DID compare.
            if (!sealedSub || !rawSub) {
              errors.push(`R1: ${g.key}.${at}.${prefix}: the block exists in the ${sealedSub ? "published bundle" : "raw artifact"} ` +
                `but not in the ${sealedSub ? "raw artifact" : "published bundle"} - every metric in it went unverified`);
              unverified.add(g.key);
              continue;
            }
            for (const k of Object.keys(sealedSub)) {
              if (!isMetricField(k)) continue;
              cmp(`${at}.${k}`, shownOf(sealedSub[k]), expectOf(rawSub, rawCell.absences, prefix, k));
            }
            // AND EVERY FRONTIER READING'S OWN ABSENCE REASON, which nothing above reaches.
            //
            // `isMetricField` recognises scalar keys; the frontier is a list of readings, each with its
            // own sealed rate whose absence reason lives under a BOUND-KEYED name
            // (`perf.frontier.10ms.rps`), so this loop walked past it. The bug this missed:
            // `sealFrontier` looked up the bare key instead of the prefixed one, defaulting every absent
            // reading to `not_measured` with no detail - flattening "measured, and it cannot do this"
            // (e.g. one-api's rungs never got below 34ms) into indistinguishable-from-nothing-measured,
            // which flatters the gateway.
            if (prefix === "perf" && Array.isArray(sealedSub.frontier)) {
              for (const r of sealedSub.frontier) {
                const at2 = r.bound_ms == null ? "unbounded" : `${r.bound_ms}ms`;
                const key = `frontier.${at2}.rps`;
                const rawAbs = absentEntryOf(rawCell.absences, "perf", key);
                const shownReason = r.rps && r.rps.reason != null ? r.rps.reason : null;
                const wantReason = rawAbs && rawAbs.reason ? rawAbs.reason : null;
                // A published rate needs no reason; an absent one must carry the artifact's own.
                if (r.rps && r.rps.value == null && wantReason && shownReason !== wantReason) {
                  errors.push(`R1: ${g.key}.${at}.frontier.${at2}: the raw artifact records this absence as ` +
                    `\`${wantReason}\` but the sealed envelope carries \`${shownReason}\` - an absence that ` +
                    `loses its reason cannot be told from a hole nobody measured, and for a tail bound ` +
                    `that difference is the whole finding`);
                }
                const shownDetail = r.rps && r.rps.detail != null ? r.rps.detail : null;
                const wantDetail = rawAbs && rawAbs.detail ? rawAbs.detail : null;
                if (r.rps && r.rps.value == null && wantDetail && shownDetail !== wantDetail) {
                  errors.push(`R1: ${g.key}.${at}.frontier.${at2}: the absence's own evidence was dropped in ` +
                    `projection (artifact says ${JSON.stringify(wantDetail.slice(0, 60))})`);
                }
              }
            }
          }
        }
      }
      // (b) the PROJECTED records: best_cell, translation_cell, streaming. (Memory projects none.)
      for (const [name, rec, rawCellSel, prefix] of [
        ["best_cell", g.best_cell, (() => { const p = g.best_cell && g.best_cell.path; return (p && cellAt(p.dialect, p.dialect)) || null; })(), "perf"],
        ["translation_cell", g.translation_cell, (() => { const p = g.translation_cell && g.translation_cell.path; return (p && cellAt(p.ingress, p.egress)) || null; })(), "perf"],
        ["streaming", g.streaming, (() => { const p = g.streaming && g.streaming.path; return (p && p.dialect && cellAt(p.dialect, p.dialect)) || null; })(), "stream"],
      ]) {
        const raw = rawCellSel && rawCellSel[prefix];
        if (!rec || !raw || !rec.source || rec.source.kind !== "matrix") continue;
        for (const k of Object.keys(rec)) {
          if (!isMetricField(k)) continue;
          cmp(`${name}.${k}`, shownOf(rec[k]), expectOf(raw, rawCellSel.absences, prefix, k));
        }
      }
      // (c) R4: the SELECTION, re-derived from the raw artifact rather than read off the bundle. Only
      // for matrix-sourced records - a fallback record is projected from a legacy suite file by a
      // different rule, and holding it to the matrix's ranking would fail an honest legacy row.
      const bestDialect = g.best_cell && g.best_cell.path && g.best_cell.path.dialect;
      if (g.best_cell && g.best_cell.source && g.best_cell.source.kind === "matrix") {
        covered("R4.selection");
        const want = oracleBestDialect(m);
        if (want && bestDialect !== want)
          errors.push(`R4: ${g.key}.best_cell is published as the ${bestDialect} diagonal, but re-deriving the ` +
            `selection from the raw artifact picks ${want} - the values under that name may all verify while the ` +
            `board still names the wrong cell as this gateway's best`);
      }
      if (g.translation_cell && g.translation_cell.source && g.translation_cell.source.kind === "matrix") {
        covered("R4.selection");
        const want = oracleTranslationPath(m);
        const p = g.translation_cell.path || {};
        if (want && (p.ingress !== want.ingress || p.egress !== want.egress))
          errors.push(`R4: ${g.key}.translation_cell is published as ${p.ingress}->${p.egress}, but re-deriving the ` +
            `selection from the raw artifact picks ${want.ingress}->${want.egress} (the fair openai-ingress tier ` +
            `first, then lowest added-latency p99) - correct numbers under the wrong path`);
      }
      if (g.streaming && g.streaming.source && g.streaming.source.kind === "matrix") {
        covered("R4.selection");
        // Streaming projects from THE SAME diagonal best_cell was projected from (gen-data: one source
        // of truth). A streaming record naming a different dialect is two headline lanes describing
        // two different cells under one gateway's name.
        const sd = g.streaming.path && g.streaming.path.dialect;
        if (bestDialect && sd !== bestDialect)
          errors.push(`R4: ${g.key}.streaming is published on the ${sd} diagonal but best_cell is the ${bestDialect} ` +
            `diagonal - the two headline lanes name different cells, and streaming is projected from the best cell`);
      }
      if (oracleCompared > 0) covered("R1.oracle");
    }
  }

  // ---- C3 lint: per-datum provenance captions ROUTE through the one renderer -------------------------
  // The bug class is a caption that hard-codes a source token for a datum it does not describe (memory
  // mislabelled "6x6"). Since every per-datum provenance label renders through caption(cell) /
  // _sweep_label(source) keyed by source.sweep, the lint's job is to (a) forbid the internal sweep-KEY
  // tokens (6x6-diagonal, …) from appearing as a user-facing string literal outside SWEEP_CAPTION/seal
  // (a key leaking into prose is the drift), and (b) assert the LANES pathNotes route through caption().
  const appSrc = readSrc("app.js");
  // (a) the SWEEP-KEY tokens must not appear as a bare string literal in a caption renderer. They live
  // ONLY in SWEEP_CAPTION (app.js) and seal.mjs (data). charts.py was the second renderer and is gone.
  for (const [src, name, region] of [
    [appSrc, "app.js", { enter: /const SWEEP_CAPTION\s*=/, exit: /^\};\s*$/m }],
  ]) {
    const r = lintSweepKeys(src, name, region);
    errors.push(...r.errors);
    if (r.scanned) covered("C3.lint");   // the SCANNER ran and found its region - not the exemption path
  }
  // (b) the LANES pathNotes + COL_TESTED provenance + charts annot must route through the vocabulary.
  if (!/pathNote:\s*\(j\)\s*=>\s*j && j\.source \? caption\(j\)/.test(appSrc.replace(/\s+/g, " ")))
    errors.push(`C3: app.js LANES pathNotes must route provenance through caption(j) (found a pathNote not using caption())`);
  else covered("C3.route");

  // ---- C5 lint: every sealed-metric read routes through metric()/mval() ----
  // Taint-based and whole-file. ONE RENDERER, so one file to lint - charts.py was the second and is
  // deleted.
  for (const [src, name, lang] of [[appSrc, "app.js", "js"]]) {
    const r = lintAccessorRouting(src, name, lang);
    errors.push(...r.errors);
    if (r.scanned) covered("C5.lint");
  }
  if (/\bmetric\(|\bmval\(/.test(appSrc)) covered("C5.route");

  // A gateway with an unverifiable cell or block is not an oracled gateway (see `unverified`).
  for (const k of unverified) oracledKeys.delete(k);

  // ---- R2 coverage: every declared invariant branch must be exercised --------
  const CHECK_BRANCHES = [
    "C1.field", "C1.certified", "C1.mock_bound", "C2.suppressed",
    "C3.stamp", "C3.lint", "C3.route", "C4.cell", "C4.leak", "C6.cell", "R1.oracle",
    "R3.selection", "R4.selection", "C7.hwm", "C5.route", "C5.lint", "C8.engine", "C1.timings", "C1.absences",
    // Not REQUIRED: a board where every row is on the current engine suppresses nothing, and that is
    // the normal, healthy state. Declared so the branch is guarded when it does fire.
    "C9.suppressed",
  ];
  // WIRING FIRST, COVERAGE SECOND. Whether each declared branch still has a call site is a question
  // about this file, answerable without a single gateway, and never downgraded by board fullness (see
  // lintCoverageWiring) - this is what makes "switch a check off" a failing state even on a partial
  // board, where the coverage gate below can only warn.
  errors.push(...lintCoverageWiring(readSrc("check-consistency.mjs"), CHECK_BRANCHES).errors);
  // C8.engine fires but was never DECLARED here, so R2's "every covered branch is declared" assertion
  // hard-failed on any bundle with engine stamps while passing on fixtures that skip C8 entirely.
  // Declaring it is not a relaxation - it stays OUT of REQUIRED because eng.checked is 0 on a
  // legitimately all-unstamped board, and C8 already errors on the dishonest case (a mix of stamped
  // and unstamped rows).
  // R4.selection is declared but not REQUIRED for the same reason: a gateway can be a matrix publisher
  // on per-cell memory alone, with no projected selection to re-derive.
  // C1.mock_bound / C2.suppressed / C4.leak are ERROR-only branches (their absence is the GOOD state),
  // so they're not required of a healthy bundle. C3.lint and C5.lint are tagged when the SCANNER ran,
  // not when it found a violation, so an inert (never-scanning) lint is itself a coverage failure.
  // REQUIRED_ALWAYS = branches required of ANY bundle, including a pure-fallback or synthetic one; a
  // bundle publishing matrix numbers owes four more (see REQUIRED below). C3.parity is gone: it
  // compared charts.py's caption vocabulary to app.js's, and there's only one renderer now.
  const REQUIRED_ALWAYS = ["C1.field", "C1.certified", "C3.stamp", "C3.route",
    "C3.lint", "C5.lint", "C4.cell", "C5.route"];
  // The oracle branches are required whenever the bundle publishes ANY matrix-sourced number; a gateway
  // that publishes matrix numbers with no on-disk matrix is already an error above (its numbers are
  // unverifiable). A bundle with NO matrix-sourced cells at all (a pure-fallback or synthetic fixture)
  // legitimately has nothing to oracle, and only that exempts these branches.
  const publishesMatrix = matrixPublishers.size > 0;
  // PER-GATEWAY oracle reconciliation: every gateway publishing a matrix-sourced number must have been
  // independently oracled, by name, not merely "at least one comparison happened anywhere".
  if (publishesMatrix && !syntheticFixture) {
    const unoracled = [...matrixPublishers].filter((k) => !oracledKeys.has(k)).sort();
    if (unoracled.length)
      errors.push(`R2: coverage - ${unoracled.length} gateway(s) publish matrix-sourced numbers that the independent oracle never verified: ${unoracled.join(", ")} ` +
        `(a per-gateway bypass is exactly the inert-check failure R2 exists to catch)`);
  }
  // C8: one engine per board. Same guard as the oracle - a hand-built fixture has no snapshots on
  // disk to stamp, so there is nothing to agree about.
  if (publishesMatrix && !syntheticFixture) {
    const eng = engineAgreement([...matrixPublishers].sort());
    if (eng.checked > 0) covered("C8.engine");
    errors.push(...eng.errors);
  }
  const REQUIRED = publishesMatrix && !syntheticFixture
    ? [...REQUIRED_ALWAYS, "C6.cell", "C7.hwm", "R1.oracle", "R3.selection"] : REQUIRED_ALWAYS;
  const missing = REQUIRED.filter((b) => !cover.has(b));
  if (missing.length) {
    // A BRANCH WITH NO INPUT IS NOT AN INERT CHECK, on a COMPLETE board where every branch has
    // something to bite on - silence there really does mean the check went dead.
    //
    // It does not hold mid-run: a 14-gateway field run publishes each result as it lands, so between
    // the first and last gateway the board is legitimately partial and branches like C3.stamp/C4.cell
    // simply have nothing to bite on yet. Treating that as inert made the first gateway of a run
    // unpublishable and froze the whole site behind the slowest box. So the gate keeps its teeth only
    // where a board carries every gateway the repo declares, and warns while the board is filling.
    //
    // A bundle with NO ROWS AT ALL is different: "partial" used to mean `publishers < declared`, which
    // is also true of zero gateways - so the one input where every branch is unexercised took the
    // gentle (warning) arm instead of erroring, and the empty-bundle test had to be marked skipped to
    // stay green.
    //
    // `rowsOnBoard` (is this a board at all) and `matrixPublishers` (has it finished filling) are
    // checked separately so a zero-row bundle still errors while a fresh, unbenchmarked checkout still
    // warns. Whether a check was switched off no longer depends on any of this - that's caught
    // statically and unconditionally by lintCoverageWiring.
    const declared = declaredGatewayCount();
    const rowsOnBoard = (data.gateways || []).length;
    const partial = declared > 0 && rowsOnBoard > 0 && matrixPublishers.size < declared;
    const msg = `R2: coverage - required invariant branch(es) never exercised by this bundle: ${missing.join(", ")}`;
    if (partial) {
      warnings.push(`${msg} - the board carries ${matrixPublishers.size} of ${declared} declared gateways, ` +
        `so these branches have no input yet rather than having gone inert; they are required again once the board is complete`);
    } else {
      errors.push(`${msg} (an inert check is itself a failure)`);
    }
  }

  return { errors, warnings, cover, CHECK_BRANCHES, REQUIRED, REQUIRED_ALWAYS };
}

/* CLI: node site/check-consistency.mjs [data.json] */
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const bundle = process.argv[2] || join(HERE, "data.json");
  const data = JSON.parse(readFileSync(bundle, "utf8"));
  const app = createRequire(import.meta.url)(join(HERE, "app.js"));
  const { errors, warnings } = checkConsistency(data, app);
  for (const w of warnings) console.warn(`check-consistency: WARNING: ${w}`);
  if (errors.length) {
    for (const e of errors) console.error(`check-consistency: FAIL: ${e}`);
    console.error(`check-consistency: ${errors.length} structural-invariant violation(s); the build must not ship.`);
    process.exit(1);
  }
  console.log(`check-consistency: ${data.gateways.length} gateways - sealed-envelope invariants C1–C5 hold (${warnings.length} warning(s))`);
}
