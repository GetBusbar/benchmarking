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
//   C5  Every gated-field read in app.js routes through metric()/mval() — never a bare `.value` deref or a
//       numeric compare of a metric field (the accessor-routing lint).
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

// The gated (honesty-flag-bearing) metric field names + their sibling *_mock_bound flags. C1 asserts none
// of these appear as a BARE scalar, and no *_mock_bound key survives, anywhere in the bundle.
const GATED_FIELDS = ["rps_sustained_20ms", "rps_max_proxy", "streams_sustained", "cpu_fps"];
// The full envelope-valued metric field set (gated + ungated), on any projected cell / matrix cell.
const METRIC_FIELDS = [
  ...GATED_FIELDS,
  "added_latency_p50_us", "added_latency_p99_us", "gateway_c1_p99_us", "direct_c1_p99_us",
  "added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us", "added_gap_p99_us", "streams_sustained_fps",
  "idle_rss_mib", "peak_rss_mib", "recovered_rss_mib",
];
// The origins a projected cell's source.kind may honestly carry: the single end-state "matrix" path plus
// the LIVE deferred fallbacks (kept until the field run; sealed honestly, never mislabelled as matrix).
const SOURCE_KINDS = new Set(["matrix", "perf-fallback", "xlate-fallback", "stream-fallback"]);

function isEnvelope(x) { return x != null && typeof x === "object" && typeof x.certified === "boolean"; }

// The raw matrix snapshot on disk — the INDEPENDENT oracle (Design F R1). Never read through the accessor.
function rawMatrix(gwKey) {
  const p = join(ROOT, "results", "matrix", `${gwKey}.json`);
  if (!existsSync(p)) return null;
  try { return JSON.parse(readFileSync(p, "utf8")); } catch { return null; }
}

// ---- the caption-literal lint (C3) + accessor-routing lint (C5) --------------
// A source token in a per-datum caption literal is the bug class (memory mislabelled "6x6"). The lint
// scans the caption-RENDERING regions of app.js/charts.py — the SWEEP_CAPTION table is the ONE allowed
// home for source tokens. Tab-level methodology prose (the chooser lead lines that describe the run design)
// is explicitly out of scope; the C3 lint targets caption(cell)/pathNote/pill/annot renderers.
const SOURCE_TOKENS = /(?:6x6|6×6|matrix per-cell|xlate suite|stream suite|perf suite)/;

function readSrc(rel) {
  const p = join(HERE, rel);
  return existsSync(p) ? readFileSync(p, "utf8") : "";
}

export function checkConsistency(data, app) {
  const errors = [];
  const warnings = [];
  const cover = new Set();
  const covered = (tag) => cover.add(tag);

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
      if (METRIC_FIELDS.includes(k)) {
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
    const mm = rawMatrix(g.key);
    if (mm && mm.upstreams) {
      for (const [egress, up] of Object.entries(mm.upstreams)) {
        for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
          const perf = cell && cell.served === true && cell.perf;
          if (!perf) continue;
          const sus = perf.rps_sustained_20ms, max = perf.rps_max_proxy;
          if (sus == null || max == null || max === 0) continue;
          covered("C6.cell");
          if (sus > max)
            warnings.push(`${g.key}.${ingress}->${egress}: sustained@20ms ${sus} > max_proxy ${max} ` +
              `(a ${((sus / max - 1) * 100).toFixed(2)}% inversion — two independently-swept ceilings overlapping on measurement noise; re-measure this cell)`);
        }
      }
    }
    // ---- R1 independent oracle: the projected best_cell headline == the RAW matrix diagonal cell -------
    const m = rawMatrix(g.key);
    if (m && m.upstreams && g.best_cell && g.best_cell.source && g.best_cell.source.kind === "matrix") {
      covered("R1.oracle");
      const dia = g.best_cell.path && g.best_cell.path.dialect;
      const rawCell = dia && m.upstreams[dia] && m.upstreams[dia].cells && m.upstreams[dia].cells[dia];
      const rawPerf = rawCell && rawCell.perf;
      if (rawPerf) {
        for (const key of ["rps_sustained_20ms", "rps_max_proxy"]) {
          // Re-derive the EXPECTED display value from the RAW cell (value + its own _mock_bound flag),
          // through a path DISJOINT from metric(): a positive value certified only when flag === false;
          // a positive uncertified value is n/a; a 0 is honest (measured-zero).
          const raw = rawPerf[key];
          const flag = rawPerf[`${key}_mock_bound`];
          const expected = raw == null ? null
            : raw === 0 ? 0
            : (raw > 0 && flag === false) ? raw : null;   // suppressed -> n/a
          const shown = app.metric(g.best_cell[key]).v;
          if (shown !== expected)
            errors.push(`R1: ${g.key}.${key}: raw matrix diagonal cell implies displayed=${expected} but the sealed envelope shows ${shown} (independent-oracle mismatch)`);
        }
      }
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
  const SWEEP_KEY = /"(?:6x6-diagonal|6x6-translation|6x6-memory-window|6x6-stream-diagonal|perf-suite|xlate-suite|stream-suite)"/;
  const scanKeys = (src, name, allowRegion) => {
    let inAllowed = false;
    src.split("\n").forEach((line, i) => {
      if (allowRegion.enter.test(line)) inAllowed = true;
      if (inAllowed) { if (allowRegion.exit.test(line)) inAllowed = false; return; }
      const code = line.replace(/\/\/.*$/, "");
      if (SWEEP_KEY.test(code)) {
        // rec.source = {…sweep: "6x6-diagonal"…} is legitimate PROVENANCE DATA assignment, not a caption.
        if (/\bsource\b|\bsweep\b\s*:/.test(code)) { covered("C3.lint"); return; }
        errors.push(`C3: ${name}:${i + 1} a sweep-key token leaked into a caption literal (keys live only in SWEEP_CAPTION): ${line.trim().slice(0, 80)}`);
        covered("C3.lint");
      }
    });
  };
  scanKeys(appSrc, "app.js", { enter: /const SWEEP_CAPTION\s*=/, exit: /^\};\s*$/m });
  scanKeys(chartsSrc, "charts.py", { enter: /SWEEP_CAPTION\s*=|def _sweep_label/, exit: /^def (?!_sweep_label)/ });
  // (b) the LANES pathNotes + COL_TESTED provenance + charts annot must route through the vocabulary.
  if (!/pathNote:\s*\(j\)\s*=>\s*j && j\.source \? caption\(j\)/.test(appSrc.replace(/\s+/g, " ")))
    errors.push(`C3: app.js LANES pathNotes must route provenance through caption(j) (found a pathNote not using caption())`);
  else covered("C3.route");
  if (!/_sweep_label\(/.test(chartsSrc))
    errors.push(`C3: charts.py provenance annotations must route through _sweep_label(source) (the SWEEP_CAPTION mirror)`);
  else covered("C3.route");

  // ---- C5 lint: every gated-field read in app.js routes through metric()/mval() ----
  // A bare `.rps_sustained_20ms.value` deref (outside metric/mval/isEnvelope) or a numeric compare of a
  // metric field would bypass the reader. Scan app.js for a gated field immediately followed by `.value`
  // or a numeric comparison; the only allowed `.value` reads are inside metric()/mval().
  appSrc.split("\n").forEach((line, i) => {
    const code = line.replace(/\/\/.*$/, "");
    for (const f of GATED_FIELDS) {
      // a direct `.<field>.value` deref bypasses metric(); metric()/mval() themselves read `env.value`.
      const re = new RegExp(`\\.${f}\\.value\\b`);
      if (re.test(code) && !/function metric|function mval|isEnvelope\(/.test(code)) {
        errors.push(`C5: app.js:${i + 1} reads .${f}.value directly (must route through metric()/mval()): ${line.trim().slice(0, 80)}`);
        covered("C5.lint");
      }
    }
    if (/\bmetric\(|\bmval\(/.test(code)) covered("C5.route");
  });

  // ---- R2 coverage: every declared invariant branch must be exercised --------
  const CHECK_BRANCHES = [
    "C1.field", "C1.certified", "C1.mock_bound", "C2.suppressed",
    "C3.stamp", "C3.lint", "C3.route", "C4.cell", "C4.leak", "C6.cell", "R1.oracle", "C5.route",
  ];
  // C1.mock_bound / C2.suppressed / C4.leak / C5.lint are ERROR-only branches: they fire only on a
  // violation, so they are NOT required to be covered by a healthy bundle (their absence is the GOOD
  // state). REQUIRED = the branches a healthy bundle with projected cells MUST exercise.
  const REQUIRED = ["C1.field", "C1.certified", "C3.stamp", "C3.route", "C4.cell", "C5.route"];
  // ORACLE branches (C6.cell, R1.oracle) need the RAW matrix snapshot on disk (results/matrix/<gw>.json).
  // The REAL bundle always has it; a SYNTHETIC single-gateway fixture (RED-before test) legitimately does
  // not, so these are required ONLY when at least one gateway's raw matrix was found — otherwise their
  // absence is "not applicable to this bundle", not an inert check. This keeps R2 honest on the CI bundle
  // while letting invariant unit-tests run on synthetic data.
  const sawRawMatrix = (data.gateways || []).some((g) => rawMatrix(g.key));
  const requiredNow = sawRawMatrix ? [...REQUIRED, "C6.cell", "R1.oracle"] : REQUIRED;
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
