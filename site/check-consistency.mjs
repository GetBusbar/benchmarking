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

import { readFileSync, existsSync, readdirSync, statSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");

// The metric-field vocabulary is IMPORTED from seal.mjs, the SAME list gen-data seals from, so a local
// whitelist here can never lag the producer: one shared list plus a shape rule (any *_rss_mib) means a
// new producer field is checked the day it appears.
import { displayedValue, isMetricField, zeroNoteFor } from "./seal.mjs";
// The origins a projected cell's source.kind may honestly carry: the single end-state "matrix" path plus
// the LIVE deferred fallbacks (kept until the field run; sealed honestly, never mislabelled as matrix).
const SOURCE_KINDS = new Set(["matrix", "perf-fallback", "xlate-fallback", "stream-fallback"]);

function isEnvelope(x) { return x != null && typeof x === "object" && typeof x.certified === "boolean"; }

// ---- reading a RAW artifact of either shape -------------------------------------------------------
// upstreamsOf(m): the egress -> {cells} grid of a raw artifact, in ONE shape. A v2 artifact carries it
// under `upstreams`; a v1 artifact carries its single measured egress row as a top-level `cells`, named
// by `upstream_shape` (the same normalization gen-data applies before projecting). Reading only
// `m.upstreams` here meant a v1 artifact produced ZERO oracle comparisons and ZERO re-derived
// selections, and the per-gateway coverage gate then converted that silence into a hard publish
// failure - so an honest legacy republish was blocked by the oracle's inability to READ the artifact
// rather than by anything wrong with the data it contained.
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
// THIS LINE IS PARSED BY bench-audit.py. The same bar exists there as a twin constant, and its
// board-level `check_c6_bar_agrees_with_the_site` reads THIS declaration out of this file and fails when
// the two disagree - so tuning the ceiling here alone does not fork the invariant quietly, it fails the
// python gate until both sides move together. The check also fails when it cannot find the constant at
// all, because going blind is not a pass: keep the exact `export const C6_GROSS_PCT = <number>;` shape,
// on one line, or the cross-check has nothing to read.
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
  // Either artifact shape (upstreamsOf): a v1 artifact's cells are just as physically implausible when
  // they invert, and skipping them made C6 silently inert on exactly the artifacts nobody re-measures.
  for (const [egress, up] of Object.entries(upstreamsOf(rawMatrix))) {
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

// ---- C9 WAS HERE, AND IT WAS WRONG ----------------------------------------------------------------
// It failed the build on any published p99 below its own p50. That rule is true of two percentiles OF
// ONE DISTRIBUTION and false of every pair this board actually publishes, because all of them are
// DIFFERENCES:
//
//   added_gap_p50 = P50(gateway gaps) - P50(mock gaps)
//   added_gap_p99 = P99(gateway gaps) - P99(mock gaps)
//
// Two independent differences. If the mock's own jitter grows faster into the tail than the
// gateway's, the difference shrinks, and p99 < p50 is the honest result. plano published
// p50=15us p99=11us against a 20ms pacing interval and this guard called it broken.
//
// The defect C9 was written for was real but different: added_ttft_p50 came from a SINGLE stream
// while added_ttft_p99 came from a hundred samples - two populations wearing one name. No data-level
// check can tell that apart from legitimate shrinkage, because both look identical in the artifact.
// The guard for it is structural and lives where it can work: both percentiles are computed from the
// same sample set in metric.rs, asserted by
// `the_ttft_percentiles_come_from_one_sample_set_so_p99_can_never_sit_below_p50`.
//
// Left as a comment rather than deleted silently: a guard removed without its reasoning is
// indistinguishable from one lost in a refactor.

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
// Commits the repo ATTESTS are the same instrument, with built-artifact evidence. See
// site/instrument-equivalence.json for the rule an entry has to meet. Returns commit -> instrument
// id; a commit nobody attested maps to itself, so an unreadable or absent file degrades to the
// original commit-equality behaviour rather than to a board that publishes anything.
export function instrumentOf(fileText) {
  const map = new Map();
  let doc;
  try { doc = JSON.parse(fileText); } catch { return map; }
  for (const inst of (doc && doc.instruments) || []) {
    if (!inst || !inst.id || !Array.isArray(inst.commits)) continue;
    // An entry with no artifact evidence is INERT, not trusted: the file's own rule is that identical
    // binaries are what admits an entry, and a rule nothing enforces is a comment.
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
    // The instrument, not the commit. Two commits that build the same binaries measured the same way,
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
    // THE OVERRIDE, for the rare case where the board must ship over C8's objection. It is deliberately
    // awkward: it takes the reason as its value, it cannot be set by accident, and it does not silence
    // anything. The mix is still reported, still counted, and rides along in the returned result so the
    // caller publishes the fact that a human overrode a publish guard. An override nobody can see after
    // the fact is just a disabled check.
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
// THE TOKEN IN ANY QUOTE, AND AN EXEMPTION NARROW ENOUGH TO MEAN SOMETHING.
//
// Two holes, both in the direction of not firing. (1) Only DOUBLE-quoted tokens were scanned, so a
// single-quoted literal, a template literal or a Python f-string carrying the same token walked past a
// lint whose whole job is to catch that token in prose. (2) The exemption skipped any line matching
// `\bsource\b` or `sweep:` ANYWHERE on it - and a caption renderer is a function that is about a
// datum's source, so the lines most likely to contain the word "source" are precisely the lines this
// lint exists to police. The exemption now covers the one thing it was written for: the token appearing
// as the VALUE of a `sweep` key (provenance DATA assignment), which is checked against the text
// immediately preceding that token rather than against the line as a whole.
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
      // rec.source = {…sweep: "6x6-diagonal"…} / {"sweep": '6x6-diagonal'} is legitimate PROVENANCE
      // DATA assignment, not a caption - judged by what sits immediately before THIS token.
      if (/\bsweep["'`]?\s*[:=]\s*$/.test(code.slice(0, m.index))) continue;
      errors.push(`C3: ${name}:${i + 1} a sweep-key token leaked into a caption literal (keys live only in SWEEP_CAPTION): ${line.trim().slice(0, 80)}`);
      break;
    }
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
// THE FIELD-NAME LINT POLICES THE WHOLE SEALED VOCABULARY, NOT FOUR NAMES OF IT.
//
// The direct-form scan below used to iterate GATED_FIELDS - the four throughput metrics - and nothing
// else, so it covered 4 of the ~20 fields seal.mjs actually seals. Every latency, ttft, gap, growth,
// plateau and RSS envelope could be dereferenced straight to its raw number and the lint reported
// green: `g.best_cell.added_latency_p99_us.value`, `g.best_cell["added_ttft_p99_us"]["value"]` and
// `g.matrix...peak_rss_mib.value` all passed a check whose whole purpose is to forbid exactly that.
// A lint that knows a fifth of its own vocabulary is the inert-check failure in miniature: it fires
// often enough to look alive while the bug class walks past it.
//
// So the direct form is DISCOVERED rather than enumerated. Each raw-deref spelling captures the FIELD
// NAME it dereferences, and seal.mjs's own `isMetricField` decides whether that name is a sealed
// metric - the same predicate gen-data seals by and C1 asserts by. There is no second list to keep in
// step: a field added to seal.mjs's vocabulary is policed here the moment it is added, and the RSS
// family (discovered by regex, never enumerated anywhere) is covered for the first time.
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
// A SEALED FIELD BOUND TO A LOCAL IS AN ENVELOPE, whether or not that local ever meets an accessor.
//
//   const rec = g.best_cell.added_latency_p50_us;
//   return rec.value;
//
// routed around BOTH passes below: the binding line carries no `.value`, and `rec` was never handed to
// metric()/mval()/isEnvelope(), so the taint set never learned it was an envelope. Two lines, no lint,
// no error - the exact shape a reader writes when the deref is inconvenient on one line. The field's
// OWN NAME is sufficient evidence of the type, so a binding read off a sealed field taints its target
// exactly as an accessor call does.
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
  // EVERY SPELLING OF "READ THE RAW NUMBER OFF THE ENVELOPE", not just the dotted one. A lint that
  // catches `env.value` and misses `env["value"]`, `env[key].value` and `const { value } = env` is a
  // lint that a reader routes around by accident, in three keystrokes, and then reports itself green -
  // and the bracket form is the one a reader reaches for FIRST when the field name is a variable.
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
  // PASS 1b: ...and so is every identifier bound DIRECTLY OFF A SEALED FIELD (see JS_BINDING above).
  // The name of the field is the type evidence; no accessor call is needed to know what `rec` is in
  // `const rec = g.best_cell.added_latency_p50_us`.
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
    // The field name is DISCOVERED from the deref and judged by seal.mjs's isMetricField, so this arm
    // covers the whole sealed vocabulary (gated, latency, ttft/gap, memory, and the RSS family that no
    // list anywhere enumerates) instead of the four GATED_FIELDS it used to know about.
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
// THE ANTI-INERT GATE WAS ITSELF INERT, AND ON A PARTIAL BOARD IT COULD NOT BE ANYTHING ELSE.
//
// R2's runtime coverage check asks "was every required branch exercised by this bundle". Mid-run that
// question has no honest answer - a branch can be silent because its input has not landed yet - so the
// finding is downgraded to a warning while the board is still filling (see the long note at the R2
// gate below, which is right about the tension it describes). The cost was that on a 5-of-14 board the
// ERROR arm was unreachable, and therefore so was every consequence of switching a check OFF: comment
// out the one line that tags R1.oracle and both `node check-consistency.mjs` and `node test.mjs` still
// exited 0. The independent oracle could be disabled entirely and the publish gate stayed green.
//
// "THIS BRANCH HAD NO INPUT" AND "THIS BRANCH IS NOT WIRED UP" ARE DIFFERENT FACTS, and only the first
// one depends on how full the board is. The second is a property of the SOURCE: a branch that no
// surviving call site tags can never be exercised by any bundle, complete or not, and that is a defect
// today rather than a maybe-tomorrow. So it is checked statically, against this file's own text, and
// it is an ERROR unconditionally - board completeness has nothing to say about whether a line exists.
//
// A commented-out call site is not a call site: a leading `//` on the line is precisely how a check
// gets switched off "temporarily", so any tag whose line has a comment marker before it is ignored.
// The reverse direction is checked too: a call site tagging a branch nobody declared is a token that
// can never be required, which is the same hole approached from the other side (it is how C8.engine
// was tagged-but-undeclared, hard-failing every stamped board).
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
// Re-derive what a metric MUST publish from the RAW artifact, through a path DISJOINT from
// metric()/seal.mjs.
//
// THE ORACLE NO LONGER RE-DERIVES A GATE, because there is no longer a gate: a present number is always
// published (see seal.mjs). What it re-derives now is the two FACTS that ride with the number - the rig
// ceiling the measurement was taken against and the fraction of it reached - because those are published
// and anything published must be verified. An oracle that checked only the value would let a headroom
// fraction attach to the wrong metric, or drift from the ceiling it claims to be a fraction of, and
// report green.
//
// WHICH FIELDS' FACTS A METRIC CARRIES. Almost always its own, with one deliberate exception:
// `streams_sustained_fps` is the rate produced by the SAME bisect that produced `streams_sustained`, so
// it carries no comparison of its own and inherits the count's (gen-data.mjs seals it that way for
// exactly this reason, see AUDIT #11 there). Looking for a name that is never written yields `undefined`
// and the oracle would demand no headroom for a rate the bundle correctly annotates - the shape of the
// bug that blocked the deploy for two days on the retired flag's name.
const STREAM_FACT_OWNER = { streams_sustained_fps: "streams_sustained" };
export function comparisonFactsFor(raw, field) {
  const owner = STREAM_FACT_OWNER[field] || field;
  // The stream metrics' ceiling is DERIVED from the mock's pacing and named `*_mock_ceiling`; the
  // throughput metrics' is MEASURED and named `*_rig_ceiling`. One of the two is present per field.
  const ceiling = raw[`${owner}_mock_ceiling`] ?? raw[`${owner}_rig_ceiling`] ?? null;
  return { headroom: raw[`${owner}_headroom`] ?? null, ceiling };
}

// WHAT THE BOARD SHOULD SHOW FOR ONE RAW VALUE, resolved by the same function the seal uses.
//
// This used to restate the rule. It implemented the capacity branch only, so every PACED field whose
// mock-bound flag was true had the seal publish (correctly - matching a paced target is the gateway
// keeping up, not a mock ceiling) while the oracle demanded null. Twenty-five of those mismatches
// blocked the deploy on every commit for two days, and the board went stale while the numbers it
// should have been showing sat in the repo.
//
// An independent oracle is worth having because it re-derives the answer from the RAW artifact
// instead of trusting the bundle. That independence is about the DATA PATH, not about owning a
// second copy of the display rule: a second copy does not catch drift, it IS the drift.
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
// The oracle compared `.v` and nothing else, which left everything the envelope says ABOUT its number
// unverified: a reason flattened back to "not_measured" (the exact regression the absence-carrying seal
// was written to end), a zero-note swapped so a measured streaming failure published as a missing RPS
// ceiling, a lost `detail` that is the evidence a reader is shown - every one of them preserves the
// number and so every one of them verified green. The reason WAS measured; it is data, and data the
// board renders, so the oracle re-derives it exactly as it re-derives the value.
//   value      - through displayedValue, the one display rule.
//   reason     - the engine's absence token for a hole.
//   note       - a certified 0's meaning, from seal.mjs's zeroNoteFor: the one place that map lives.
//   detail     - the engine's prose for the absence, which must survive the seal.
//   headroom   - the fraction of the rig's own ceiling the measurement reached, and
//   ceiling    - the ceiling that fraction is of. Both null when the engine had no usable reference,
//                which costs the facts and NOT the value.
//
// The `reason: "mock_bound"|"unverifiable"` branch is gone with the suppression that produced it. Nothing
// here can return a null value for a raw number that is present, which is the property C2 asserts from
// the other direction.
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
      // An absent metric publishes no comparison: seal.mjs attaches the facts through `withExtras`,
      // which the absent branch returns before reaching.
      headroom: null, ceiling: null };
  if (Number(raw) === 0) return { ...none, v: 0, note: zeroNote };
  return none;
}

// ---- R4: the SELECTION itself, re-derived --------------------------------------------------------
// The oracle above verifies the numbers at the coordinates the BUNDLE CLAIMS. That left the choice of
// coordinates entirely unchecked: a projection that picked the wrong cell publishes correct values
// under the wrong name, and every value comparison agrees with it. So the board could show one
// gateway's second-best diagonal as its headline, or a translation cell the fair tier should have
// outranked, and the independent oracle would report green on every number.
//
// These re-derive WHICH cell each projected record must be, from the raw artifact and the PUBLISHED
// rule (gen-data.mjs bestCell/translationCell). That is a second implementation of a ranking, which is
// what deferred it - but the alternative is a check that verifies the arithmetic of an answer without
// ever asking whether it is the answer to the right question. It is written against the rule as stated
// in gen-data's own comments, so a deliberate rule change fails here loudly and is updated in both
// places, which is the point.
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
  // Gateways where a cell or a whole block could not be compared at all. Oracle credit is per gateway,
  // so a row that lost a block in silence used to keep the credit its OTHER blocks earned; these are
  // subtracted before the reconciliation below, which is what makes "this gateway was oracled" mean
  // "all of it was" rather than "some of it was".
  const unverified = new Set();

  // ---- C1 + C2: envelope integrity across the WHOLE bundle -------------------
  // Walk every object. (C1) a *_mock_bound key must not survive; a gated metric field must be an envelope,
  // never a bare number. (C2) a suppressed envelope has value:null and no other numeric field on it.
  // `timings_s` IS A COST RECORD, NOT A METRIC BLOCK, and its keys collide with metric names.
  //
  // It maps each metric GROUP to the seconds that group spent on this cell, so it legitimately
  // carries keys named `cpu_fps` and `streams_sustained` whose values are DURATIONS. C1 matches on
  // the field NAME, so it read those seconds as unsealed metrics and blocked the deploy the first
  // time a real snapshot carried them - the engine only started serializing timings_s today, so the
  // collision could not appear until a box published. A duration is not a measurement of the
  // gateway and must not be sealed; the subtree is skipped whole, the way rss_series and
  // load_recipe already are by not being metric fields at all.
  const walk = (node, path, inTimings = false) => {
    if (Array.isArray(node)) { node.forEach((v, i) => walk(v, `${path}[${i}]`, inTimings)); return; }
    if (node == null || typeof node !== "object") return;
    if (inTimings) return;
    for (const [k, v] of Object.entries(node)) {
      if (k === "timings_s") { covered("C1.timings"); continue; }
      if (k.endsWith("_mock_bound")) {
        errors.push(`C1: ${path}.${k} - a raw *_mock_bound flag survives in the bundle (must be consumed at seal time)`);
        covered("C1.mock_bound");
      }
      if (isMetricField(k)) {
        covered("C1.field");
        // EVERY MALFORMED SHAPE, not just a bare number.
      //
      // This read `!isEnvelope(v) && typeof v === "number"`, so the ONLY thing it could catch was a raw
      // numeric leak. A metric field carrying a bare string, a boolean, an array, or - the dangerous
      // one - a HALF-BUILT envelope like `{value: 20057}` with `certified` dropped or renamed, matched
      // neither branch and sailed through. `isEnvelope` requires `typeof x.certified === "boolean"`, so
      // an under-built envelope is not an envelope, and it was not a number either, so nothing fired.
      // C1's own claim is "no raw ungated metric field exists in the bundle"; for those shapes it was
      // not being checked at all, and this file is the deploy gate.
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
            // `!sealedSub || !rawSub -> continue` skipped a gateway's entire perf, stream or memory
            // lane without a word, while the gateway still earned its per-gateway oracle credit from
            // whichever block did compare - so a bundle that lost a block wholesale, or invented one
            // the artifact never carried, verified green and reported itself covered.
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
      // (c) R4: the SELECTION, re-derived from the raw artifact rather than read off the bundle.
      // Only for matrix-sourced records: a fallback record is projected from a legacy suite file by a
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

  // A gateway with an unverifiable cell or block is not an oracled gateway (see `unverified`).
  for (const k of unverified) oracledKeys.delete(k);

  // ---- R2 coverage: every declared invariant branch must be exercised --------
  const CHECK_BRANCHES = [
    "C1.field", "C1.certified", "C1.mock_bound", "C2.suppressed",
    "C3.stamp", "C3.lint", "C3.route", "C3.parity", "C4.cell", "C4.leak", "C6.cell", "R1.oracle",
    "R3.selection", "R4.selection", "C7.hwm", "C5.route", "C5.lint", "C8.engine", "C1.timings",
  ];
  // WIRING FIRST, COVERAGE SECOND. Whether each declared branch still HAS a call site is a question
  // about this file, answerable without a single gateway, and it is never downgraded by how full the
  // board is - see lintCoverageWiring. This is what makes "switch a check off" a failing state on a
  // 5-of-14 board, where the coverage gate below can only warn.
  errors.push(...lintCoverageWiring(readSrc("check-consistency.mjs"), CHECK_BRANCHES).errors);
  // C8.engine was tagged by the branch below but never DECLARED here, so R2's own
  // "every covered branch is a declared branch" assertion (test.mjs) hard-failed on any bundle whose
  // gateways carry engine stamps - i.e. on every post-stamp real board, while passing on the synthetic
  // fixtures that skip C8 entirely. Declaring it is not a relaxation: the branch already runs and its
  // errors already fire. It stays OUT of REQUIRED because eng.checked is 0 on a legitimately
  // all-unstamped (pre-stamp) board, and C8 already errors on the dishonest case - a MIX of stamped
  // and unstamped rows - so requiring coverage here would fail the one board C8 deliberately permits.
  // R4.selection is declared but not REQUIRED for the same reason: a gateway can be a matrix publisher
  // on its per-cell memory alone (no best_cell, no translation_cell, no streaming), and that row has no
  // projected selection to re-derive. It is required of nothing and fires on everything that has one.
  // C1.mock_bound / C2.suppressed / C4.leak are ERROR-only branches: they fire only on a violation, so
  // they are NOT required to be covered by a healthy bundle (their absence is the GOOD state). REQUIRED =
  // the branches a healthy bundle with projected cells MUST exercise. C3.lint and C5.lint are tagged when
  // the SCANNER ran, not when it found a violation or took an exemption, so an inert (never-scanning)
  // lint is itself a coverage failure.
  // REQUIRED_ALWAYS = the branches required of ANY bundle, including a pure-fallback or synthetic one.
  // It is not the whole requirement: a bundle that publishes matrix numbers owes four more (see
  // REQUIRED below). The two used to be a constant and an unexported local respectively, and only the
  // constant was returned - so test.mjs, which iterates the returned REQUIRED directly, asserted
  // coverage of nine branches and knew nothing about C6.cell, C7.hwm, R1.oracle or R3.selection. The
  // oracle's own coverage token was outside the set the test walked, which is why deleting the line
  // that tags it failed nothing. The returned REQUIRED is now what this bundle is ACTUALLY required to
  // cover, and the unconditional floor is returned beside it under its own name.
  const REQUIRED_ALWAYS = ["C1.field", "C1.certified", "C3.stamp", "C3.route", "C3.parity",
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
  const REQUIRED = publishesMatrix && !syntheticFixture
    ? [...REQUIRED_ALWAYS, "C6.cell", "C7.hwm", "R1.oracle", "R3.selection"] : REQUIRED_ALWAYS;
  const missing = REQUIRED.filter((b) => !cover.has(b));
  if (missing.length) {
    // A BRANCH WITH NO INPUT IS NOT AN INERT CHECK.
    //
    // This gate exists because a check that stops firing is worse than no check: it converts "nobody
    // looked" into "it passed". That reasoning holds for a COMPLETE board, where every branch has
    // something to bite on and silence really does mean the check went dead.
    //
    // It does not hold mid-run. A 14-gateway field is measured over hours, one box per gateway, and
    // the board publishes each result as it lands - so between the first gateway and the last it is
    // legitimately partial, and branches like C3.stamp and C4.cell simply have no cell of the right
    // shape yet. Treating that as an inert check made the FIRST gateway of a run unpublishable and
    // kept the whole site frozen behind the slowest box, which is the opposite of publishing as
    // results arrive. The check has not gone quiet; it has nothing to say yet.
    //
    // So the gate keeps its teeth exactly where they mean something - a board carrying every gateway
    // the repo declares - and reports the same gap as a warning while the board is still filling.
    //
    // A BUNDLE WITH NO ROWS AT ALL IS NOT A BOARD THAT IS STILL FILLING - IT IS A BROKEN PROJECTION.
    //
    // "Partial" was `publishers < declared`, which is also true of a bundle carrying NO GATEWAYS
    // WHATSOEVER - so the one input on which every single required branch is unexercised took the
    // gentle arm and produced no error at all. That is precisely the failure path R2 exists for, and it
    // was the one input that could not reach it. The suite's own test of it
    // (`checkConsistency({gateways: []})` must error) had to be marked skipped to stay green, which is
    // how a skip came to conceal a failing assertion.
    //
    // The mid-run argument is about a board that HAS ROWS: fourteen gateways are on it, some have
    // published and the rest are still on their boxes, and the branches their cells would exercise have
    // no input yet. A bundle with zero rows is not that. The repo declares gateways, so a projection
    // that emitted none of them lost every row it was given - nothing about board timing explains that,
    // and there is nothing to publish either way, so the gate keeps its teeth.
    //
    // NOTE the deliberately different quantity on each side: rows ON THE BOARD decides whether this is
    // a board at all, and matrix PUBLISHERS decides whether it has finished filling. A fresh checkout
    // (fourteen rows, none benchmarked yet - gen-data emits exactly that, see its "empty is not
    // undatable" note) is a real board at the start of its life and warns; it does not block a deploy.
    // And "a check was switched off" no longer depends on any of this: that is caught statically and
    // unconditionally by lintCoverageWiring, which is the whole point of separating the two facts.
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
