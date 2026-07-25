#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// check-consistency.mjs: STRUCTURAL INVARIANTS on the sealed-envelope bundle + accessors.
//
// This file used to assert that twelve render surfaces AGREE on a honesty gate they each re-implemented.
// Under the sealed envelope (Design E) there is nothing to agree ABOUT: every metric is EITHER a certified
// number OR an explicit {value:null, suppressed:true} — the raw scalar + its _mock_bound flag are consumed
// at seal time and never re-emitted. So the check is REPURPOSED into invariants on the contract itself —
// the onthebench 11th-phase ("where are tests missing?") test:
//
//   C1  No raw ungated metric field exists in the bundle (only certified-or-suppressed envelopes); NO
//       `*_mock_bound` key survives anywhere.
//   C2  A suppressed metric exposes no recoverable value (value === null, no shadow numeric field).
//   C3  Every displayed caption derives from a source.sweep stamp present in the data; app.js/charts.py
//       carry no hard-coded source-token literal in a per-datum caption renderer (the lint).
//   C4  Single projection path: every projected cell's source.kind is a known origin and its source.sweep
//       is a valid caption key; NO legacy suite object (g.perf/stream/streamcpu/xlate) leaks into the bundle.
//   C5  Every sealed-metric read (app.js AND charts.py) routes through metric()/mval() — never a raw
//       `.value` / `.get("value")` deref outside the accessors (the taint-based accessor-routing lint).
//
// Each lint is a PURE EXPORTED FUNCTION so it can be driven against synthetic source that CONTAINS the
// violation: a lint with no RED-before proof is indistinguishable from a lint that cannot fire, which is
// exactly what C5's predecessor was.
//
// Rigor rules (Design F Part 1): the expected side of any cross-representation assertion is re-derived
// INDEPENDENTLY from the RAW matrix cell on disk (results/matrix/<gw>.json), never via the accessor under
// test (R1). A COVERAGE assertion (R2) fails if any invariant branch is never exercised by the bundle —
// an inert check is itself a failure. Each invariant has a RED-before test in test.mjs (revert the seal on
// one surface → the class test fails).
//
// Run standalone against an emitted bundle:
//   node site/check-consistency.mjs [site/data.json]

import { readFileSync, existsSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");

// AUDIT #11: the metric-field vocabulary is IMPORTED from seal.mjs — the SAME list gen-data seals from.
// A local whitelist here had already lagged the producer (peak_rss_hwm_mib / post_load_rss_mib were
// shipping unsealed and C1 could not see them, because C1's whitelist did not know they existed). One
// shared list + a shape rule (any *_rss_mib) means a new producer field is checked the day it appears.
import { GATED_FIELDS, isMetricField } from "./seal.mjs";
// The origins a projected cell's source.kind may honestly carry: the single end-state "matrix" path plus
// the LIVE deferred fallbacks (kept until the field run; sealed honestly, never mislabelled as matrix).
const SOURCE_KINDS = new Set(["matrix", "perf-fallback", "xlate-fallback", "stream-fallback"]);

function isEnvelope(x) { return x != null && typeof x === "object" && typeof x.certified === "boolean"; }

// ---- C6 as a pure function: sustained@20ms <= max_proxy on every served cell --------------------
// max_proxy is the UNCONSTRAINED throughput ceiling; sustained-under-SLO cannot EXCEED it. Every
// inversion observed in shipped data has been a CROSS-PHASE measurement artefact: sustained@20ms and
// max_proxy are swept in SEPARATE phases with independent noise bands, so two ceilings legitimately
// overlap — the margin scales with 1/throughput (sub-1% on a 14k-rps gateway, a few % on a ~200-rps
// one). So C6 WARNS on every inverted cell (visible in the build log so the next field run re-measures
// the offender) but never hard-fails: a hard assert would false-fail every honest run on sub-noise
// overlap and block all publishing. A max_proxy of 0 is "did not qualify" (no ceiling), not an
// inversion, and is skipped. The magnitude is stamped so a GROSS inversion stands out for escalation.
//
// Exported and pure (AUDIT #21) so its RED-before test can INJECT an inversion into a synthetic matrix
// instead of depending on a real gateway staying broken.
export function c6Inversions(gwKey, rawMatrix) {
  const warnings = [];
  let cellsChecked = 0;
  if (!rawMatrix || !rawMatrix.upstreams) return { warnings, cellsChecked };
  for (const [egress, up] of Object.entries(rawMatrix.upstreams)) {
    for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
      const perf = cell && cell.served === true && cell.perf;
      if (!perf) continue;
      const sus = perf.rps_sustained_20ms, max = perf.rps_max_proxy;
      if (sus == null || max == null || max === 0) continue;
      cellsChecked += 1;
      if (sus > max)
        warnings.push(`${gwKey}.${ingress}->${egress}: sustained@20ms ${sus} > max_proxy ${max} ` +
          `(a ${((sus / max - 1) * 100).toFixed(2)}% inversion — two independently-swept ceilings overlapping on measurement noise; re-measure this cell)`);
    }
  }
  return { warnings, cellsChecked };
}

// The raw matrix on disk — the INDEPENDENT oracle (Design F R1). Never read through the accessor.
//
// AUDIT #21 (THE ORACLE WENT INERT). rawMatrix() used to read ONLY results/matrix/<gw>.json, and the
// oracle loop was additionally gated on `!g.matrix_from_snapshot` — because a snapshot-sourced matrix
// legitimately differs from the trailing per-suite file, comparing the two would false-fail. That was
// correct while snapshots were a transition path used by a minority of rows. It became a SILENT TOTAL
// BYPASS the moment the field run made EVERY gateway snapshot-sourced: the oracle compared nothing for
// 12 of 13 gateways, and R2's coverage gate was satisfied by the single legacy row that still had a
// per-suite matrix — so an entirely unoracled board reported "R1.oracle covered" and shipped green.
//
// The fix is to give the oracle the RIGHT on-disk artifact instead of switching it off. A snapshot IS a
// raw on-disk artifact — it is exactly as independent of seal.mjs/metric() as the per-suite file was.
// rawMatrixFor() resolves the same artifact gen-data resolved, but by its OWN independent re-derivation
// of the selection rule (newest snapshot by measured_at, taken over the per-suite file when at least as
// new), never by importing gen-data. R3 below then asserts that this independent resolution AGREES with
// the stamp the bundle shipped — so a selection bug is caught rather than silently mirrored.
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

// ---- the caption-literal lint (C3) + accessor-routing lint (C5) --------------
// A source token in a per-datum caption literal is the bug class (memory mislabelled "6x6"). The lints
// scan the caption-RENDERING regions of app.js/charts.py — the SWEEP_CAPTION table is the ONE allowed
// home for source tokens. Tab-level methodology prose (the chooser lead lines that describe the run
// design) is explicitly out of scope; the C3 lint targets caption(cell)/pathNote/pill/annot renderers.

function readSrc(rel) {
  const p = join(HERE, rel);
  return existsSync(p) ? readFileSync(p, "utf8") : "";
}

// ---- the lints, as PURE EXPORTED FUNCTIONS (audit #20) -----------------------------------------
// Each lint used to be an inline loop over the repo's own source, which meant it could only ever be
// observed in its GREEN state: there was no way to write a RED-before test proving the lint FIRES on the
// bug it claims to catch (and C5's could not fire at all — see below). Extracted as pure
// source-text -> findings functions, they are unit-testable against synthetic source that CONTAINS the
// violation, so "the lint works" is proven rather than assumed.

// (1) SWEEP-KEY LEAK (C3a): the internal sweep keys are caption VOCABULARY and live ONLY in the caption
// table / the seal. A key appearing as a user-facing string literal anywhere else is caption drift.
export const SWEEP_KEY_RE = /"(?:6x6-diagonal|6x6-translation|6x6-memory-window|6x6-stream-diagonal|6x6-stream-translation|perf-suite|xlate-suite|stream-suite)"/;
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
  // COVERAGE means "the scanner actually parsed this file and FOUND its allowed region" — proof the lint
  // is live. The old tag was set by the EXEMPTION path (and by the error path), so a lint that never
  // scanned anything still reported itself covered (audit #20).
  return { errors, scanned: sawRegion };
}

// (2) ACCESSOR ROUTING (C5): every read of a sealed metric's number must go through metric()/mval().
// AUDIT #15: the old lint looked for the literal text `.<gatedField>.value` on ONE line of app.js. The
// codebase does not read metrics that way — it binds the envelope to a local first
// (`const env = p[key]` / `const env = p && p.rps_sustained_20ms`) and then reads `env.value`, often on a
// LATER line — so the lint could not fire, and app.js contained two real violations it should have
// caught. This version is TAINT-BASED and whole-file: any identifier handed to an envelope predicate or
// accessor (isEnvelope/metric/mval, or _is_env/mval in Python) IS an envelope, and reading its raw
// `.value` / `.get("value")` outside the accessor definitions themselves bypasses the reader.
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
  // PASS 2: mark the line ranges that ARE the accessor definitions — the one place `.value` is legal.
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

// (3) PER-LANE chart provenance (C3b). AUDIT #2: the old check asserted only that `_sweep_label(`
// appears SOMEWHERE in charts.py — so the streaming lane could (and did) ship four PNGs with zero
// provenance disclosure while the perf + xlate lanes disclosed theirs, and the lint stayed green.
// Assert it PER LANE: each lane's own `_<lane>_source` stamp must be fed to _sweep_label.
export const CHART_PROVENANCE_LANES = ["_perf_source", "_xlate_source", "_stream_source"];
export function lintChartLaneProvenance(chartsSrc) {
  const errors = [];
  const flat = chartsSrc.replace(/\s+/g, " ");
  for (const lane of CHART_PROVENANCE_LANES) {
    if (!new RegExp(`_sweep_label\\(\\s*\\{\\s*"sweep":\\s*r\\.get\\(\\s*"${lane}"`).test(flat))
      errors.push(`C3: charts.py lane ${lane} never reaches _sweep_label — that lane's PNGs publish numbers with NO provenance disclosure while its sibling lanes disclose theirs`);
  }
  return { errors, scanned: !!chartsSrc };
}

// (4) CROSS-LANGUAGE caption parity (C3c). AUDIT #22: charts.py's comment CLAIMED check-consistency
// asserts its SWEEP_CAPTION keys match app.js's — it did not, so the two caption vocabularies could
// silently drift (and a new key added on one side only would render on one surface and throw on the
// other). Implemented: parse the Python key set and compare it to the JS table's keys.
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
export function oracleExpected(raw, flag, gated) {
  if (raw == null) return null;
  if (!gated) return raw;
  if (raw === 0) return 0;                     // measured zero: honest, always shown
  return (raw > 0 && flag === false) ? raw : null;   // suppressed -> n/a
}

// opts.syntheticFixture: this bundle is a HAND-BUILT fixture with no on-disk oracle (an invariant
// unit-test), so the "a matrix-sourced publish must be oracle-verifiable" requirement is waived. AUDIT
// #18: this replaces the old SILENT hatch (`no results/matrix on disk anywhere => the whole oracle layer
// is not required`), which the REAL bundle could fall into — an unverifiable publish would then pass. The
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
        errors.push(`C1: ${path}.${k} — a raw *_mock_bound flag survives in the bundle (must be consumed at seal time)`);
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
      if (g[suite] != null) { errors.push(`C4: ${g.key}.${suite} — a raw legacy suite object leaked into the bundle`); covered("C4.leak"); }
    }
    for (const [name, cell] of [["best_cell", g.best_cell], ["translation_cell", g.translation_cell], ["streaming", g.streaming], ["memory_read", g.memory_read]]) {
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
    // max_proxy is the UNCONSTRAINED throughput ceiling; sustained-under-SLO cannot EXCEED it. Derived from
    // the RAW matrix cell (Design F R1 — the independent oracle), never via the accessor. Empirically EVERY
    // inversion in the shipped data is a CROSS-PHASE measurement artefact: sustained@20ms and max_proxy are
    // swept in SEPARATE phases, each with its own noise band, so two independent ceilings legitimately
    // overlap — the margin scales with 1/throughput (sub-1% on fast gateways like kong 0.18%, up to ~8% on
    // a ~500-rps gateway like litellm-python). None is a real "sustained beat the ceiling". So C6 FLAGS
    // every inverted cell as a WARNING — visible in the build log so the FIELD RUN re-measures the offender
    // (kong's 14,351 vs 14,325 is the seed case) — but does NOT hard-fail the build: a hard assert would
    // false-fail every honest run on sub-measurement-noise, blocking all publishing. A max_proxy of 0 is
    // "did not qualify" (no ceiling), not an inversion, and is skipped. The magnitude is stamped so a GROSS
    // (implausible) inversion stands out in the log for a human to escalate at re-measure time.
    // AUDIT #21: C6 is a PURE EXPORTED FUNCTION (c6Inversions, below) so it has a RED-before proof that
    // does not depend on a particular gateway still being inverted in the shipped data. The old test
    // asserted on kong's LIVE 14,351>14,325 cell; the fresh field run resolved it, and a check whose only
    // proof is "the bug is still in the data" fails the day the data gets better.
    const c6 = c6Inversions(g.key, rawMatrix(g.key));
    if (c6.cellsChecked > 0) covered("C6.cell");
    warnings.push(...c6.warnings);
    // ---- R1 independent oracle -------------------------------------------------------------------
    // AUDIT #16: coverage is claimed ONLY when a comparison ACTUALLY HAPPENED. The tag used to fire
    // before the rawPerf guard, so a bundle whose oracle compared NOTHING (raw cell missing/renamed)
    // still reported "R1.oracle covered" and R2 passed on an oracle that had done no work.
    // AUDIT #17: the oracle no longer re-derives ONLY best_cell's two RPS fields (2 of 36 cells' worth
    // of numbers). It now independently re-derives EVERY sealed matrix cell (all perf + stream metrics),
    // the translation cell, the streaming record and the memory block — the surfaces that were unoracled.
    // AUDIT #21: resolve the artifact the bundle ACTUALLY projected from (snapshot or per-suite file),
    // re-derived here independently. The old `!g.matrix_from_snapshot` skip is GONE — it silently
    // disabled the whole oracle once every row became snapshot-sourced.
    const resolved = rawMatrixFor(g.key);
    const m = resolved ? resolved.matrix : null;
    // AUDIT #18: the escape hatch is CLOSED. A gateway that publishes matrix-sourced numbers MUST be
    // oracle-checkable: with no raw artifact on disk at all, the oracle layer would silently become
    // "not required" and an unverifiable publish would pass.
    const matrixSourced = [g.best_cell, g.translation_cell, g.streaming, g.memory_read]
      .some((r) => r && r.source && r.source.kind === "matrix");
    if (matrixSourced) matrixPublishers.add(g.key);
    if (matrixSourced && !m && !syntheticFixture)
      errors.push(`R2: ${g.key} publishes matrix-sourced numbers but no raw matrix artifact (snapshot or results/matrix/${g.key}.json) is on disk — the independent oracle cannot verify a single one of them (an unverifiable publish is a failure, not an exemption)`);
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
        errors.push(`R3: ${g.key}: the bundle published matrix measured_at=${g.matrix.measured_at} but the newest raw artifact on disk (${resolved.origin}: ${resolved.file}) is measured_at=${m.measured_at} — the board is rendering from a stale/mis-selected run`);
      const claimsSnapshot = g.matrix_from_snapshot === true;
      if (claimsSnapshot !== (resolved.origin === "snapshot"))
        errors.push(`R3: ${g.key}: the bundle claims matrix_from_snapshot=${claimsSnapshot} but the independently-resolved newest artifact is a ${resolved.origin} (${resolved.file}) — provenance disagreement`);
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
          for (const [sealedSub, rawSub] of [[cell && cell.perf, rawCell.perf], [cell && cell.stream, rawCell.stream]]) {
            if (!sealedSub || !rawSub) continue;
            for (const k of Object.keys(sealedSub)) {
              if (!isMetricField(k)) continue;
              cmp(`matrix[${ingress}->${egress}].${k}`, app.metric(sealedSub[k]).v,
                oracleExpected(rawSub[k], rawSub[`${k}_mock_bound`], GATED_FIELDS.includes(k) || k === "streams_sustained_fps"));
            }
          }
        }
      }
      // (b) the PROJECTED records: best_cell, translation_cell, streaming, memory_read.
      for (const [name, rec, raw] of [
        ["best_cell", g.best_cell, (() => { const p = g.best_cell && g.best_cell.path; const c = p && rawCellAt(p.dialect, p.dialect); return c && c.perf; })()],
        ["translation_cell", g.translation_cell, (() => { const p = g.translation_cell && g.translation_cell.path; const c = p && rawCellAt(p.ingress, p.egress); return c && c.perf; })()],
        ["streaming", g.streaming, (() => { const p = g.streaming && g.streaming.path; const c = p && p.dialect && rawCellAt(p.dialect, p.dialect); return c && c.stream; })()],
        ["memory_read", g.memory_read, m.memory],
      ]) {
        if (!rec || !raw || !rec.source || rec.source.kind !== "matrix") continue;
        for (const k of Object.keys(rec)) {
          if (!isMetricField(k)) continue;
          cmp(`${name}.${k}`, app.metric(rec[k]).v,
            oracleExpected(raw[k], raw[`${k}_mock_bound`], GATED_FIELDS.includes(k) || k === "streams_sustained_fps"));
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
    if (r.scanned) covered("C3.lint");   // the SCANNER ran and found its region — not the exemption path
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
  // AUDIT #15: taint-based and whole-file, and applied to charts.py too — the old version was same-line,
  // app.js-only, and matched an access style the codebase does not use, so it could not fire.
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
    "R3.selection", "C5.route", "C5.lint",
  ];
  // C1.mock_bound / C2.suppressed / C4.leak are ERROR-only branches: they fire only on a violation, so
  // they are NOT required to be covered by a healthy bundle (their absence is the GOOD state). REQUIRED =
  // the branches a healthy bundle with projected cells MUST exercise. AUDIT #20: C3.lint and C5.lint are
  // now coverable in the GOOD state too — they are tagged when the SCANNER ran, not when it found a
  // violation or took an exemption — so an inert (never-scanning) lint is now itself a coverage failure.
  const REQUIRED = ["C1.field", "C1.certified", "C3.stamp", "C3.route", "C3.parity",
    "C3.lint", "C5.lint", "C4.cell", "C5.route"];
  // AUDIT #18: the "no raw matrix on disk => the oracle is not required" escape hatch is CLOSED. The
  // oracle branches are required whenever the bundle publishes ANY matrix-sourced number; a gateway that
  // publishes matrix numbers with no on-disk matrix is already an error above (its numbers are
  // unverifiable). A bundle with NO matrix-sourced cells at all (a pure-fallback or synthetic fixture)
  // legitimately has nothing to oracle — that, and only that, exempts these branches.
  const publishesMatrix = matrixPublishers.size > 0;
  // AUDIT #21: the PER-GATEWAY oracle reconciliation. "R1.oracle covered" used to mean "at least one
  // comparison happened anywhere on the board" — which one legacy row could satisfy for all thirteen.
  // Every gateway that publishes a matrix-sourced number must have been independently oracled, by name.
  if (publishesMatrix && !syntheticFixture) {
    const unoracled = [...matrixPublishers].filter((k) => !oracledKeys.has(k)).sort();
    if (unoracled.length)
      errors.push(`R2: coverage — ${unoracled.length} gateway(s) publish matrix-sourced numbers that the independent oracle never verified: ${unoracled.join(", ")} ` +
        `(a per-gateway bypass is exactly the inert-check failure R2 exists to catch)`);
  }
  const requiredNow = publishesMatrix && !syntheticFixture
    ? [...REQUIRED, "C6.cell", "R1.oracle", "R3.selection"] : REQUIRED;
  const missing = requiredNow.filter((b) => !cover.has(b));
  if (missing.length)
    errors.push(`R2: coverage — required invariant branch(es) never exercised by this bundle: ${missing.join(", ")} ` +
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
  console.log(`check-consistency: ${data.gateways.length} gateways — sealed-envelope invariants C1–C5 hold (${warnings.length} warning(s))`);
}
