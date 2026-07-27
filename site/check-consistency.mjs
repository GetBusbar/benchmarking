#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// check-consistency.mjs: STRUCTURAL INVARIANTS on the sealed-envelope bundle + accessors.
//
// Under the sealed envelope (Design E) every metric is EITHER a certified number OR an explicit
// {value:null, suppressed:true}: the raw scalar + its _mock_bound flag are consumed at seal time and
// never re-emitted, so there is nothing for render surfaces to individually agree about. This file
// instead checks invariants on the contract itself, the onthebench 11th-phase ("where are tests
// missing?") test:
//
//   C1  No raw ungated metric field exists in the bundle (only certified-or-suppressed envelopes); NO
//       `*_mock_bound` key survives anywhere.
//   C2  A suppressed metric exposes no recoverable value (value === null, no shadow numeric field).
//   C3  Every displayed caption derives from a source.sweep stamp present in the data; app.js/charts.py
//       carry no hard-coded source-token literal in a per-datum caption renderer (the lint).
//   C4  Single projection path: every projected cell's source.kind is a known origin and its source.sweep
//       is a valid caption key; NO legacy suite object (g.perf/stream/streamcpu/xlate) leaks into the bundle.
//   C5  Every sealed-metric read (app.js AND charts.py) routes through metric()/mval() - never a raw
//       `.value` / `.get("value")` deref outside the accessors (the taint-based accessor-routing lint).
//
// Each lint is a PURE EXPORTED FUNCTION so it can be driven against synthetic source that CONTAINS the
// violation: a lint with no RED-before proof is indistinguishable from a lint that cannot fire, which is
// exactly what C5's predecessor was.
//
// Rigor rules (Design F Part 1): the expected side of any cross-representation assertion is re-derived
// INDEPENDENTLY from the RAW matrix cell on disk (results/matrix/<gw>.json), never via the accessor under
// test (R1). A COVERAGE assertion (R2) fails if any invariant branch is never exercised by the bundle -
// an inert check is itself a failure. Each invariant has a RED-before test in test.mjs (revert the seal on
// one surface -> the class test fails).
//
// Run standalone against an emitted bundle:
//   node site/check-consistency.mjs [site/data.json]

import { readFileSync, existsSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");

// The metric-field vocabulary is IMPORTED from seal.mjs, the SAME list gen-data seals from, so a local
// whitelist here can never lag the producer: one shared list plus a shape rule (any *_rss_mib) means a
// new producer field is checked the day it appears.
import { GATED_FIELDS, isMetricField } from "./seal.mjs";
// The origins a projected cell's source.kind may honestly carry: the single end-state "matrix" path plus
// the LIVE deferred fallbacks (kept until the field run; sealed honestly, never mislabelled as matrix).
const SOURCE_KINDS = new Set(["matrix", "perf-fallback", "xlate-fallback", "stream-fallback"]);

function isEnvelope(x) { return x != null && typeof x === "object" && typeof x.certified === "boolean"; }

// ---- C7: peak_rss_mib <= peak_rss_hwm_mib (a second physical-plausibility invariant) ------------
// VmHWM is the KERNEL's own high-water mark, updated on every charge, so it cannot be lower than any
// RSS the sampler ever observed for the same process tree. The shipped data violates it on two
// gateways (one gateway at 165.1 > 164.7, another at 45.0 > 44.7), which is physically impossible for a
// FIXED process tree - and that is the tell. Both readers sum over the tree ENUMERATED AT READ TIME
// (lib/harness.sh _proc_tree_field_mib): the sampled peak sums VmRSS over the tree alive DURING the
// load, while VmHWM is summed AFTER it. A worker that exits in between is counted in the peak and
// absent from the HWM sum, so sum(VmHWM) can legitimately come out BELOW the sampled peak on a
// multi-process gateway. It is a real artefact of transient children, not a fabricated number - so it
// WARNS (so the next run can attribute it) rather than hard-failing an otherwise honest publish. The
// numbers are left exactly as measured; nothing here rewrites data.
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
  // PER CELL, because that is where memory lives now. Reading only the top-level block made this check a
  // NO-OP on every artifact the current producer writes - and worse than a no-op: "C7.hwm" is a REQUIRED
  // coverage token whenever a bundle publishes matrix numbers, and a per-cell memory row is itself what
  // makes a gateway a matrix publisher, so an all-new-shape field run would satisfy the requirement and
  // starve the token at the same time, hard-failing the publish gate on 13 freshly measured gateways.
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

// ---- C6 as a pure function: sustained@20ms <= max_proxy on every served cell --------------------
// max_proxy is the UNCONSTRAINED throughput ceiling; sustained-under-SLO cannot EXCEED it. A
// max_proxy of 0 is "did not qualify" (no ceiling), not an inversion, and is skipped. The magnitude is
// stamped so a gross inversion is legible at a glance.
//
// Exported and pure (AUDIT #21) so its RED-before test can INJECT an inversion into a synthetic matrix
// instead of depending on a real gateway staying broken.
// C6 SEVERITY IS DECIDED BY THE MEASUREMENT'S OWN NOISE, NOT BY A FIXED RULE IN EITHER DIRECTION: a
// fixed hard-fail is wrong on a CPU-bound gateway, where the sustained and max-proxy sweeps measure THE
// SAME ceiling in two separate phases, so which one comes out higher is decided by run-to-run variation.
//
// The test is therefore not "is sustained > max_proxy" and not "is the gap under some chosen percent".
// It is: IS THE GAP LARGER THAN THIS CELL'S OWN MEASURED SPREAD? The peak sweep probes many rungs and
// their rps values scatter; that scatter IS this gateway-and-cell's measurement noise, measured on the
// same box in the same phase, for free. An inversion inside that band is a comparison the data cannot
// resolve. An inversion outside it is a number that the gateway's own repeated measurements say should
// not have happened, and that is a real finding.
//
// Two things bound how far this can be stretched:
//   1. C6_GROSS_PCT caps how much noise may ever be excused, so a degenerate two-rung sweep with a wild
//      spread cannot license an arbitrarily large inversion.
//   2. The bound-termination check below is an ERROR ON ITS OWN, at any magnitude. A peak sweep whose
//      WINNING rung is the highest rung it probed has not found a ceiling, it ran out of ladder, and
//      that is caught directly rather than being inferred from the inversion it happens to produce.
// A sub-band inversion is reported as a WARNING carrying its magnitude and the band it fell inside, so
// it stays visible in the build log and on the row instead of being silently tolerated.
export const C6_GROSS_PCT = 5;
// sweepSpreadPct(sweep, winner): the rung-to-rung scatter of a sweep, as a percentage of the winning
// rps. This is the cell's OWN measured noise: same box, same phase, same gateway, several samples of
// the same ceiling. Null when there is nothing to measure it from (fewer than two rungs), which is the
// honest answer - a sweep that probed once has not measured its own variability, and the caller must
// NOT then treat an inversion as excusable.
function sweepSpreadPct(sweep, winner) {
  if (!Array.isArray(sweep) || sweep.length < 2 || !(winner > 0)) return null;
  const vals = sweep.map((r) => (r && typeof r.rps === "number" ? r.rps : null)).filter((v) => v != null);
  if (vals.length < 2) return null;
  return ((Math.max(...vals) - Math.min(...vals)) / winner) * 100;
}
// peakRanOutOfLadder(sweep, winnerConc): did the peak sweep WIN at the highest concurrency it probed?
// Then it never observed a fall-off, so it never established a ceiling - the true peak may be past the
// end of the ladder. This is the defect the old warning masked, and it is a hard error at any
// magnitude, inversion or not.
function peakRanOutOfLadder(sweep, winnerConc) {
  if (!Array.isArray(sweep) || sweep.length < 2 || winnerConc == null) return false;
  const concs = sweep.map((r) => (r && typeof r.conc === "number" ? r.conc : null)).filter((v) => v != null);
  if (concs.length < 2) return false;
  return Number(winnerConc) === Math.max(...concs);
}
export function c6Inversions(gwKey, rawMatrix) {
  const violations = [];
  const warnings = [];
  let cellsChecked = 0;
  if (!rawMatrix || !rawMatrix.upstreams) return { violations, warnings, cellsChecked };
  for (const [egress, up] of Object.entries(rawMatrix.upstreams)) {
    for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
      const perf = cell && cell.served === true && cell.perf;
      if (!perf) continue;
      const sus = perf.rps_sustained_20ms, max = perf.rps_max_proxy;
      if (sus == null || max == null || max === 0) continue;
      cellsChecked += 1;
      const at = `${gwKey}.${ingress}->${egress}`;
      // (1) LADDER EXHAUSTION - an error on its own, whether or not this cell also inverted. A peak that
      // is the top rung probed is not a peak, it is where we stopped climbing.
      if (peakRanOutOfLadder(perf.sweep_max_proxy, perf.rps_max_proxy_concurrency)) {
        violations.push(`${at}: the max_proxy sweep WON at the highest concurrency it probed ` +
          `(${perf.rps_max_proxy_concurrency}), so it never observed a fall-off and never established a ` +
          `ceiling - the published maximum is where the ladder ended, not where the gateway did`);
      }
      if (!(sus > max)) continue;
      const pct = (sus / max - 1) * 100;
      const band = sweepSpreadPct(perf.sweep_max_proxy, max);
      const inBand = band != null && pct <= band && pct <= C6_GROSS_PCT;
      const detail = `sustained@20ms ${sus} > max_proxy ${max} (a ${pct.toFixed(2)}% inversion`;
      if (inBand) {
        // Inside the cell's own measured scatter: the two sweeps sampled one ceiling twice and the
        // difference is smaller than the difference between the peak sweep's own rungs. Visible, with
        // the band that excused it stated, so the judgement can be checked rather than trusted.
        warnings.push(`${at}: ${detail}, within this cell's own max-proxy sweep scatter of ` +
          `${band.toFixed(2)}% - the two phases sampled the same ceiling and the data cannot resolve which is higher)`);
      } else {
        const why = band == null
          ? "the max-proxy sweep has too few rungs to have measured its own variability, so nothing establishes this gap as noise"
          : pct > C6_GROSS_PCT
            ? `above the ${C6_GROSS_PCT}% ceiling on excusable noise (this cell's sweep scatter is ${band.toFixed(2)}%)`
            : `outside this cell's own max-proxy sweep scatter of ${band.toFixed(2)}%`;
        violations.push(`${at}: ${detail}, ${why}) - the number the board publishes as a MAXIMUM was ` +
          `exceeded by another measurement on the same box against the same mock, which makes it not a maximum`);
      }
    }
  }
  return { violations, warnings, cellsChecked };
}

// ---- C8: ONE ENGINE PER BOARD ---------------------------------------------------------------------
// The board's whole claim is that every gateway was measured by the same instrument, so the only thing
// that differs between two columns is the gateway. That claim is false the moment two snapshots were
// produced by different harness commits, and an instrument change (e.g. a mock rebuild that alters
// which cells are judged served) can otherwise look indistinguishable from simultaneous gateway
// regressions across the whole board.
//
// The engine commit is therefore data, and disagreement is a publish failure rather than something a
// human has to notice. Three ways to fail:
//   - two published gateways carry different engine commits (a mixed board)
//   - a gateway carries `dirty: true` (a modified tree does not identify what actually ran)
//   - a gateway carries no engine stamp at all, while another does (silently pre-stamp data mixed in)
// A board where NO gateway is stamped is left alone: that is entirely pre-stamp data, and failing it
// would only punish history it cannot fix. The moment one stamped run lands, all of them must be.
export function engineAgreement(gwKeys, resolve = (k) => newestSnapshotOnDisk(k)) {
  const errors = [];
  const seen = new Map();   // commit -> [gwKey]
  const unstamped = [];
  let checked = 0;
  for (const k of gwKeys) {
    const found = resolve(k);
    const eng = found && found.snap && found.snap.rig && found.snap.rig.engine;
    if (!eng || !eng.commit) { unstamped.push(k); continue; }
    checked += 1;
    if (eng.dirty === true)
      errors.push(`C8: ${k} was measured by a DIRTY harness tree (engine.commit=${eng.commit.slice(0, 12)} with uncommitted edits) - the commit does not identify what ran, so this run is not reproducible and must be re-measured on a clean tree`);
    if (!seen.has(eng.commit)) seen.set(eng.commit, []);
    seen.get(eng.commit).push(k);
  }
  if (checked > 0 && unstamped.length > 0)
    errors.push(`C8: ${unstamped.length} gateway(s) carry no engine stamp (${unstamped.join(", ")}) while ${checked} do - the board would be mixing pre-stamp data with stamped data and cannot show they came from the same harness; re-measure the unstamped gateways`);
  if (seen.size > 1) {
    const groups = [...seen.entries()].map(([c, ks]) => `${c.slice(0, 12)}: ${ks.join(", ")}`).join(" | ");
    errors.push(`C8: the board mixes ${seen.size} harness engines (${groups}) - columns measured by different instruments are not comparable, so a defect fixed between those commits applies to only part of the field; re-run the lagging gateways on the newest engine`);
  }
  return { errors, checked, commits: [...seen.keys()] };
}

// The raw matrix on disk - the INDEPENDENT oracle (Design F R1). Never read through the accessor.
//
// A snapshot IS a raw on-disk artifact, exactly as independent of seal.mjs/metric() as the per-suite
// file. rawMatrixFor() resolves the same artifact gen-data resolved, but by its OWN independent
// re-derivation of the selection rule (newest snapshot by measured_at, taken over the per-suite file
// when at least as new), never by importing gen-data. This must cover every gateway, including a
// snapshot-sourced one: excluding snapshot-sourced rows from the oracle would leave the whole board
// unverified once every row is snapshot-sourced. R3 below then asserts that this independent resolution
// AGREES with the stamp the bundle shipped, so a selection bug is caught rather than silently mirrored.
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
// Exported so test.mjs's BOARD_HAS_DATA can reuse this predicate verbatim instead of re-implementing it:
// the guard and its harness must agree on what "this row publishes something" means, or the harness can
// declare a board populated that the guard considers empty (and vice versa).
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
// A source token in a per-datum caption literal is the bug class (memory mislabelled "6x6"). The lints
// scan the caption-RENDERING regions of app.js/charts.py - the SWEEP_CAPTION table is the ONE allowed
// home for source tokens. Tab-level methodology prose (the chooser lead lines that describe the run
// design) is explicitly out of scope; the C3 lint targets caption(cell)/pathNote/pill/annot renderers.

function readSrc(rel) {
  const p = join(HERE, rel);
  return existsSync(p) ? readFileSync(p, "utf8") : "";
}

// ---- the lints, as PURE EXPORTED FUNCTIONS (audit #20) -----------------------------------------
// Each lint used to be an inline loop over the repo's own source, which meant it could only ever be
// observed in its GREEN state: there was no way to write a RED-before test proving the lint FIRES on the
// bug it claims to catch (and C5's could not fire at all - see below). Extracted as pure
// source-text -> findings functions, they are unit-testable against synthetic source that CONTAINS the
// violation, so "the lint works" is proven rather than assumed.

// (1) SWEEP-KEY LEAK (C3a): the internal sweep keys are caption VOCABULARY and live ONLY in the caption
// table / the seal. A key appearing as a user-facing string literal anywhere else is caption drift.
export const SWEEP_KEY_RE = /"(?:6x6-diagonal|6x6-translation|6x6-memory-window|6x6-memory-diagonal|6x6-memory-translation|6x6-stream-diagonal|6x6-stream-translation|perf-suite|xlate-suite|stream-suite)"/;
export function lintSweepKeys(src, name, allowRegion) {
  const errors = [];
  let inAllowed = false, sawRegion = false;
  src.split("\n").forEach((line, i) => {
    if (allowRegion.enter.test(line)) { inAllowed = true; sawRegion = true; }
    if (inAllowed) { if (allowRegion.exit.test(line)) inAllowed = false; return; }
    const code = line.replace(/\/\/.*$/, "").replace(/#.*$/, "");
    if (!SWEEP_KEY_RE.test(code)) return;
    // rec.source = {…sweep: "6x6-diagonal"…} is legitimate PROVENANCE DATA assignment, not a caption.
    if (/\bsource\b|\bsweep\b\s*:/.test(code)) return;
    errors.push(`C3: ${name}:${i + 1} a sweep-key token leaked into a caption literal (keys live only in SWEEP_CAPTION): ${line.trim().slice(0, 80)}`);
  });
  // COVERAGE means "the scanner actually parsed this file and FOUND its allowed region" - proof the lint
  // is live. The old tag was set by the EXEMPTION path (and by the error path), so a lint that never
  // scanned anything still reported itself covered (audit #20).
  return { errors, scanned: sawRegion };
}

// (2) ACCESSOR ROUTING (C5): every read of a sealed metric's number must go through metric()/mval(). The
// codebase typically binds the envelope to a local first (`const env = p[key]` /
// `const env = p && p.rps_sustained_20ms`) and reads `env.value` on a later line, so this lint is
// TAINT-BASED and whole-file rather than a same-line text match: any identifier handed to an envelope
// predicate or accessor (isEnvelope/metric/mval, or _is_env/mval in Python) IS an envelope, and reading
// its raw `.value` / `.get("value")` outside the accessor definitions themselves bypasses the reader.
const JS_ACCESSOR_DEFS = /^\s*(?:export\s+)?function\s+(?:metric|mval|isEnvelope)\b|^\s*(?:export\s+)?const\s+(?:metric|mval|isEnvelope)\s*=/;
const PY_ACCESSOR_DEFS = /^\s*def\s+(?:mval|mvalid|menote|_is_env)\b/;
export function lintAccessorRouting(src, name, lang = "js") {
  const errors = [];
  const lines = src.split("\n");
  const isPy = lang === "py";
  const accessorDef = isPy ? PY_ACCESSOR_DEFS : JS_ACCESSOR_DEFS;
  const predicate = isPy ? /\b(?:_is_env|mval|mvalid|menote)\(\s*([A-Za-z_$][\w$]*)\s*[),]/g
    : /\b(?:isEnvelope|metric|mval)\(\s*([A-Za-z_$][\w$]*)\s*[),]/g;
  const valueRead = (v) => isPy
    ? new RegExp(`\\b${v}\\.get\\(\\s*["']value["']`)
    : new RegExp(`\\b${v}\\.value\\b`);
  // PASS 1 (whole file, so a deref on a LATER line than the binding is still caught): every identifier
  // ever handed to an envelope accessor/predicate is an envelope-typed local.
  const tainted = new Set();
  for (const m of src.matchAll(predicate)) tainted.add(m[1]);
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
    // (a) the direct form: a metric FIELD dereferenced straight to its raw number.
    for (const f of GATED_FIELDS) {
      const re = isPy ? new RegExp(`\\.${f}\\b[^\\n]*\\.get\\(\\s*["']value["']`) : new RegExp(`\\.${f}\\.value\\b`);
      if (re.test(code))
        errors.push(`C5: ${name}:${i + 1} reads .${f}'s raw value directly (must route through metric()/mval()): ${line.trim().slice(0, 80)}`);
    }
    // (b) the form the codebase ACTUALLY uses: an envelope bound to a local, then dereferenced.
    for (const v of tainted) {
      if (valueRead(v).test(code))
        errors.push(`C5: ${name}:${i + 1} reads the raw value off envelope-typed \`${v}\` (must route through metric()/mval()): ${line.trim().slice(0, 80)}`);
    }
  });
  return { errors, scanned: lines.length > 1, tainted };
}

// (3) PER-LANE chart provenance (C3b): assert PER LANE, not merely that `_sweep_label(` appears
// somewhere in charts.py, so a lane can never ship PNGs with zero provenance disclosure while its
// siblings disclose theirs. Each lane's own `_<lane>_source` stamp must be fed to _sweep_label.
export const CHART_PROVENANCE_LANES = ["_perf_source", "_xlate_source", "_stream_source"];
export function lintChartLaneProvenance(chartsSrc) {
  const errors = [];
  const flat = chartsSrc.replace(/\s+/g, " ");
  for (const lane of CHART_PROVENANCE_LANES) {
    if (!new RegExp(`_sweep_label\\(\\s*\\{\\s*"sweep":\\s*r\\.get\\(\\s*"${lane}"`).test(flat))
      errors.push(`C3: charts.py lane ${lane} never reaches _sweep_label - that lane's PNGs publish numbers with NO provenance disclosure while its sibling lanes disclose theirs`);
  }
  return { errors, scanned: !!chartsSrc };
}

// (4) CROSS-LANGUAGE caption parity (C3c): parse charts.py's SWEEP_CAPTION key set and compare it to
// app.js's, so the two caption vocabularies can never silently drift (a key added on one side only
// would render on one surface and throw on the other).
export function lintCaptionParity(chartsSrc, jsCaptionKeys) {
  const errors = [];
  const m = chartsSrc.match(/SWEEP_CAPTION\s*=\s*\{([\s\S]*?)\}/);
  if (!m) return { errors: [`C3: charts.py has no SWEEP_CAPTION key set to compare with app.js (the caption vocabularies cannot be proven in sync)`], scanned: false };
  const pyKeys = new Set([...m[1].matchAll(/"([^"]+)"/g)].map((x) => x[1]));
  const js = new Set(jsCaptionKeys);
  for (const k of js) if (!pyKeys.has(k)) errors.push(`C3: SWEEP_CAPTION key "${k}" exists in app.js but NOT in charts.py (the caption vocabularies have drifted)`);
  for (const k of pyKeys) if (!js.has(k)) errors.push(`C3: SWEEP_CAPTION key "${k}" exists in charts.py but NOT in app.js (the caption vocabularies have drifted)`);
  return { errors, scanned: true };
}

// ---- the INDEPENDENT ORACLE (Design F R1) --------------------------------------------------------
// oracleExpected(raw, flag, gated): re-derive what a metric MUST display, from the RAW value + its own
// _mock_bound flag, through a path DISJOINT from metric()/seal.mjs. A gated metric is shown only when it
// is null-free and either a measured 0 (honest) or a positive value the harness certified (flag===false).
// WHICH FLAG GATES A FIELD. Almost always its own `<field>_mock_bound`, with one deliberate
// exception: `streams_sustained_fps` is the rate produced by the SAME bisect that produced
// `streams_sustained`, so it carries no flag of its own and inherits the count's (gen-data.mjs seals
// it that way for exactly this reason, see AUDIT #11 there). Looking for a flag that is never written
// yields `undefined`, which reads as "not proven unbound" and makes the oracle demand null for a rate
// the bundle correctly publishes - four gateways' streaming rates blocked the deploy on a name.
export function mockBoundFlagFor(raw, field) {
  const name = field === "streams_sustained_fps" ? "streams_sustained_mock_bound" : `${field}_mock_bound`;
  return raw[name];
}

export function oracleExpected(raw, flag, gated) {
  if (raw == null) return null;
  if (!gated) return raw;
  if (raw === 0) return 0;                     // measured zero: honest, always shown
  return (raw > 0 && flag === false) ? raw : null;   // suppressed -> n/a
}

// opts.syntheticFixture: this bundle is a HAND-BUILT fixture with no on-disk oracle (an invariant
// unit-test), so the "a matrix-sourced publish must be oracle-verifiable" requirement is waived. AUDIT
// #18: this replaces the old SILENT hatch (`no results/matrix on disk anywhere => the whole oracle layer
// is not required`), which the REAL bundle could fall into - an unverifiable publish would then pass. The
// waiver is now an EXPLICIT caller opt-in that the CLI never passes, so a real bundle can never take it.
export function checkConsistency(data, app, opts = {}) {
  const { syntheticFixture = false } = opts;
  const errors = [];
  const warnings = [];
  const cover = new Set();
  const covered = (tag) => cover.add(tag);
  // AUDIT #16: how many independent-oracle comparisons ACTUALLY ran. Coverage is claimed from this
  // counter, never from merely reaching the branch.
  let oracleCompared = 0;
  // AUDIT #21: coverage is now PER-GATEWAY, not a single global counter. The old `oracleCompared > 0`
  // gate was satisfied by ONE oracled row, so twelve completely unoracled gateways still reported the
  // R1.oracle branch as covered. These two sets are reconciled at the end: every gateway that publishes
  // a matrix-sourced number must appear in oracledKeys.
  const matrixPublishers = new Set();
  const oracledKeys = new Set();

  // ---- C1 + C2: envelope integrity across the WHOLE bundle -------------------
  // Walk every object. (C1) a *_mock_bound key must not survive; a gated metric field must be an envelope,
  // never a bare number. (C2) a suppressed envelope has value:null and no other numeric field on it.
  const walk = (node, path) => {
    if (Array.isArray(node)) { node.forEach((v, i) => walk(v, `${path}[${i}]`)); return; }
    if (node == null || typeof node !== "object") return;
    for (const [k, v] of Object.entries(node)) {
      if (k.endsWith("_mock_bound")) {
        errors.push(`C1: ${path}.${k} - a raw *_mock_bound flag survives in the bundle (must be consumed at seal time)`);
        covered("C1.mock_bound");
      }
      if (isMetricField(k)) {
        covered("C1.field");
        if (!isEnvelope(v) && typeof v === "number") {
          errors.push(`C1: ${path}.${k}=${v} is a BARE scalar, not a sealed envelope (a raw ungated metric field survives)`);
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
  for (const g of data.gateways || []) {
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

    // ---- C6: sustained@20ms <= max_proxy on EVERY served cell (a physical-plausibility invariant) ------
    // max_proxy is the UNCONSTRAINED throughput ceiling; sustained-under-SLO cannot EXCEED it. Derived
    // from the RAW matrix cell (Design F R1, the independent oracle), never via the accessor. sustained@
    // 20ms and max_proxy are swept in SEPARATE phases, each with its own noise band, so two independent
    // ceilings legitimately overlap by a margin that scales with 1/throughput (sub-1% on a fast gateway,
    // up to ~8% on a ~500-rps one); an inversion within that margin is measurement noise, not a real
    // "sustained beat the ceiling". C6 therefore FLAGS every inverted cell as a WARNING, visible in the
    // build log so the FIELD RUN re-measures the offender, but does NOT hard-fail: a hard assert would
    // false-fail every honest run on sub-measurement-noise, blocking all publishing. A max_proxy of 0 is
    // "did not qualify" (no ceiling), not an inversion, and is skipped. The magnitude is stamped so a GROSS
    // (implausible) inversion stands out in the log for a human to escalate at re-measure time.
    // C6 is a PURE EXPORTED FUNCTION (c6Inversions, below) so its RED-before proof injects a synthetic
    // inversion rather than depending on a particular gateway staying inverted in the shipped data.
    const c6 = c6Inversions(g.key, rawMatrix(g.key));
    if (c6.cellsChecked > 0) covered("C6.cell");
    errors.push(...c6.violations);
    warnings.push(...(c6.warnings || []));
    const c7 = c7HwmBelowPeak(g.key, rawMatrix(g.key));
    if (c7.checked > 0) covered("C7.hwm");
    warnings.push(...c7.warnings);
    // ---- R1 independent oracle -------------------------------------------------------------------
    // Coverage is claimed ONLY when a comparison ACTUALLY HAPPENED (oracleCompared increments in cmp()
    // below), never merely from reaching this branch. The oracle independently re-derives EVERY sealed
    // matrix cell (all perf + stream metrics), the translation cell, the streaming record and the memory
    // block, resolved from the artifact the bundle ACTUALLY projected from (snapshot or per-suite file),
    // re-derived here independently rather than trusting the bundle's own claim.
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
    // The oracle is only meaningful if its independently-resolved artifact is the one gen-data projected
    // from. Compare the two measured_at stamps: a mismatch means the selection rules diverged (e.g. an
    // older snapshot shadowing a newer run), which would otherwise make the oracle verify the wrong file
    // and report green. Also asserts the bundle's own "from snapshot" claim matches what is on disk.
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
      const cmp = (label, shown, expected) => {
        oracleCompared += 1;
        oracledKeys.add(g.key);
        if (shown !== expected)
          errors.push(`R1: ${g.key}.${label}: the RAW matrix on disk implies displayed=${expected} but the sealed envelope shows ${shown} (independent-oracle mismatch)`);
      };
      const rawCellAt = (ingress, egress) => {
        const up = m.upstreams && m.upstreams[egress];
        return (up && up.cells && up.cells[ingress]) || null;
      };
      // (a) EVERY sealed matrix cell (36 of them), perf + stream, gated + ungated.
      for (const [egress, up] of Object.entries((g.matrix && g.matrix.upstreams) || {})) {
        for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
          const rawCell = rawCellAt(ingress, egress);
          if (!rawCell) continue;
          // cell.memory joins perf/stream here: since memory became a PER-CELL record it is displayed
          // straight off these cells, so leaving it out would mean the board's whole memory lane
          // published unoracled numbers. Its fields are ungated (RSS + growth rate + time to plateau).
          for (const [sealedSub, rawSub] of [[cell && cell.perf, rawCell.perf], [cell && cell.stream, rawCell.stream],
            [cell && cell.memory, rawCell.memory]]) {
            if (!sealedSub || !rawSub) continue;
            for (const k of Object.keys(sealedSub)) {
              if (!isMetricField(k)) continue;
              cmp(`matrix[${ingress}->${egress}].${k}`, app.metric(sealedSub[k]).v,
                oracleExpected(rawSub[k], mockBoundFlagFor(rawSub, k), GATED_FIELDS.includes(k) || k === "streams_sustained_fps"));
            }
          }
        }
      }
      // (b) the PROJECTED records: best_cell, translation_cell, streaming. (Memory projects none.)
      for (const [name, rec, raw] of [
        ["best_cell", g.best_cell, (() => { const p = g.best_cell && g.best_cell.path; const c = p && rawCellAt(p.dialect, p.dialect); return c && c.perf; })()],
        ["translation_cell", g.translation_cell, (() => { const p = g.translation_cell && g.translation_cell.path; const c = p && rawCellAt(p.ingress, p.egress); return c && c.perf; })()],
        ["streaming", g.streaming, (() => { const p = g.streaming && g.streaming.path; const c = p && p.dialect && rawCellAt(p.dialect, p.dialect); return c && c.stream; })()],
      ]) {
        if (!rec || !raw || !rec.source || rec.source.kind !== "matrix") continue;
        for (const k of Object.keys(rec)) {
          if (!isMetricField(k)) continue;
          cmp(`${name}.${k}`, app.metric(rec[k]).v,
            oracleExpected(raw[k], mockBoundFlagFor(raw, k), GATED_FIELDS.includes(k) || k === "streams_sustained_fps"));
        }
      }
      if (oracleCompared > 0) covered("R1.oracle");
    }
  }

  // ---- C3 lint: per-datum provenance captions ROUTE through the one renderer -------------------------
  // The bug class is a caption that hard-codes a source token for a datum it does not describe (memory
  // mislabelled "6x6"). Since every per-datum provenance label now renders through caption(cell) /
  // _sweep_label(source) keyed by source.sweep, the lint's job is to (a) forbid the internal sweep-KEY
  // tokens (6x6-diagonal, …) from appearing as a user-facing string literal outside SWEEP_CAPTION/seal
  // (a key leaking into prose is the drift), and (b) assert the LANES pathNotes route through caption().
  const appSrc = readSrc("app.js");
  const chartsSrc = readSrc("../charts.py");
  // (a) the SWEEP-KEY tokens must not appear as a bare string literal in a caption renderer. They live
  // ONLY in SWEEP_CAPTION (app.js), the _sweep_label/SWEEP_CAPTION set (charts.py), and seal.mjs (data).
  for (const [src, name, region] of [
    [appSrc, "app.js", { enter: /const SWEEP_CAPTION\s*=/, exit: /^\};\s*$/m }],
    [chartsSrc, "charts.py", { enter: /SWEEP_CAPTION\s*=|def _sweep_label/, exit: /^def (?!_sweep_label)/ }],
  ]) {
    const r = lintSweepKeys(src, name, region);
    errors.push(...r.errors);
    if (r.scanned) covered("C3.lint");   // the SCANNER ran and found its region - not the exemption path
  }
  // (b) the LANES pathNotes + COL_TESTED provenance + charts annot must route through the vocabulary.
  if (!/pathNote:\s*\(j\)\s*=>\s*j && j\.source \? caption\(j\)/.test(appSrc.replace(/\s+/g, " ")))
    errors.push(`C3: app.js LANES pathNotes must route provenance through caption(j) (found a pathNote not using caption())`);
  else covered("C3.route");
  // AUDIT #2: PER-LANE, not "the string _sweep_label( appears somewhere in the file".
  {
    const r = lintChartLaneProvenance(chartsSrc);
    errors.push(...r.errors);
    if (r.scanned) covered("C3.route");
  }
  // AUDIT #22: the cross-language caption parity charts.py's comment claimed but nothing asserted.
  {
    const r = lintCaptionParity(chartsSrc, Object.keys(app.SWEEP_CAPTION || {}));
    errors.push(...r.errors);
    if (r.scanned) covered("C3.parity");
  }

  // ---- C5 lint: every sealed-metric read routes through metric()/mval() ----
  // Taint-based and whole-file, applied to both app.js and charts.py.
  for (const [src, name, lang] of [[appSrc, "app.js", "js"], [chartsSrc, "charts.py", "py"]]) {
    const r = lintAccessorRouting(src, name, lang);
    errors.push(...r.errors);
    if (r.scanned) covered("C5.lint");
  }
  if (/\bmetric\(|\bmval\(/.test(appSrc)) covered("C5.route");

  // ---- R2 coverage: every declared invariant branch must be exercised --------
  const CHECK_BRANCHES = [
    "C1.field", "C1.certified", "C1.mock_bound", "C2.suppressed",
    "C3.stamp", "C3.lint", "C3.route", "C3.parity", "C4.cell", "C4.leak", "C6.cell", "R1.oracle",
    "R3.selection", "C7.hwm", "C5.route", "C5.lint", "C8.engine",
  ];
  // C8.engine was tagged by the branch below but never DECLARED here, so R2's own
  // "every covered branch is a declared branch" assertion (test.mjs) hard-failed on any bundle whose
  // gateways carry engine stamps - i.e. on every post-stamp real board, while passing on the synthetic
  // fixtures that skip C8 entirely. Declaring it is not a relaxation: the branch already runs and its
  // errors already fire. It stays OUT of REQUIRED because eng.checked is 0 on a legitimately
  // all-unstamped (pre-stamp) board, and C8 already errors on the dishonest case - a MIX of stamped
  // and unstamped rows - so requiring coverage here would fail the one board C8 deliberately permits.
  // C1.mock_bound / C2.suppressed / C4.leak are ERROR-only branches: they fire only on a violation, so
  // they are NOT required to be covered by a healthy bundle (their absence is the GOOD state). REQUIRED =
  // the branches a healthy bundle with projected cells MUST exercise. C3.lint and C5.lint are tagged when
  // the SCANNER ran, not when it found a violation or took an exemption, so an inert (never-scanning)
  // lint is itself a coverage failure.
  const REQUIRED = ["C1.field", "C1.certified", "C3.stamp", "C3.route", "C3.parity",
    "C3.lint", "C5.lint", "C4.cell", "C5.route"];
  // The oracle branches are required whenever the bundle publishes ANY matrix-sourced number; a gateway
  // that publishes matrix numbers with no on-disk matrix is already an error above (its numbers are
  // unverifiable). A bundle with NO matrix-sourced cells at all (a pure-fallback or synthetic fixture)
  // legitimately has nothing to oracle, and only that exempts these branches.
  const publishesMatrix = matrixPublishers.size > 0;
  // PER-GATEWAY oracle reconciliation: every gateway that publishes a matrix-sourced number must have
  // been independently oracled, by name, not merely "at least one comparison happened anywhere".
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
  const requiredNow = publishesMatrix && !syntheticFixture
    ? [...REQUIRED, "C6.cell", "C7.hwm", "R1.oracle", "R3.selection"] : REQUIRED;
  const missing = requiredNow.filter((b) => !cover.has(b));
  if (missing.length)
    errors.push(`R2: coverage - required invariant branch(es) never exercised by this bundle: ${missing.join(", ")} ` +
      `(an inert check is itself a failure)`);

  return { errors, warnings, cover, CHECK_BRANCHES, REQUIRED };
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
