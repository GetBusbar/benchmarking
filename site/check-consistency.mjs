#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// check-consistency.mjs: build-time numeric-consistency guard for the results site.
//
// THE RULE: every (gateway, metric) that appears on more than one surface must resolve to ONE
// canonical source value. For passthrough perf that source is g.best_cell (matrix per-cell
// sweep, perf-suite fallback tagged source:"perf-fallback"); for translation it is
// g.translation_cell. The table (passCell), the drawer + compare modal (canonicalPerf /
// canonicalXlate via the LANES accessors), and charts.py (which reads best_cell /
// translation_cell straight from data.json) must all agree, or the build FAILS.
//
// Run standalone against an emitted bundle:
//   node site/check-consistency.mjs [site/data.json]
// or through the test suite (site/test.mjs runs it against a freshly generated bundle; the
// cf-pages deploy runs site/test.mjs, so an inconsistent bundle can never ship).
//
// Plausibility (R7): sustained@20ms > max-proxy on a cell is per-cell sweep noise between two
// independently measured ceilings. The guard WARNS (never fails, never edits data) so the
// inversion is visible in the build log while the measured values ship untouched.

import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const PASS_KEYS = ["added_latency_p99_us", "rps_sustained_20ms", "rps_max_proxy"];

/* What charts.py reads for a gateway's passthrough metric: best_cell verbatim (its overlay),
   else the raw perf suite when served (no canonical record = raw suite verdict stands).
   Mirrored here in JS so the guard covers the python reader without executing it. */
function chartsPassValue(g, key) {
  if (g.best_cell) return g.best_cell[key] ?? null;
  const j = g.perf;
  return j && j.served !== false && j[key] != null ? j[key] : null;
}

export function checkConsistency(data, app) {
  const errors = [];
  const warnings = [];
  for (const g of data.gateways || []) {
    // ---- passthrough: table (passCell) == drawer/compare (canonicalPerf) == charts (best_cell)
    const laneRec = app.canonicalPerf(g);
    for (const key of PASS_KEYS) {
      const table = app.passCell(g, key, String).v;
      const drawer = laneRec && laneRec.served !== false && laneRec[key] != null ? laneRec[key] : null;
      const charts = chartsPassValue(g, key);
      if (!(table === drawer && drawer === charts)) {
        errors.push(`${g.key}.${key}: table=${table} drawer/compare=${drawer} charts=${charts} (must be one canonical value)`);
      }
    }
    // ---- translation: drawer/compare (canonicalXlate) == canonical translation_cell (charts)
    const t = g.translation_cell;
    if (t) {
      const x = app.canonicalXlate(g);
      const pairs = [
        ["added_latency_p99_us", "xlate_added_latency_p99_us"],
        ["rps_sustained_20ms", "xlate_rps_sustained_20ms"],
      ];
      for (const [ck, lk] of pairs) {
        const canon = t[ck] ?? null;
        const lane = x && x.xlate_served !== false && x[lk] != null ? x[lk] : null;
        if (canon !== lane) {
          errors.push(`${g.key}.translation.${ck}: canonical=${canon} drawer/compare=${lane} (must be one canonical value)`);
        }
      }
    }
    // ---- plausibility (WARN only, R7): two independent measured ceilings may invert on noise
    const bc = g.best_cell;
    if (bc && bc.rps_sustained_20ms != null && bc.rps_max_proxy != null && bc.rps_sustained_20ms > bc.rps_max_proxy) {
      warnings.push(`${g.key}: sustained@20ms ${bc.rps_sustained_20ms} > max-proxy ${bc.rps_max_proxy} ` +
        `(independent per-cell sweep ceilings; noise, shipped unclamped)`);
    }
  }
  return { errors, warnings };
}

/* CLI: node site/check-consistency.mjs [data.json] */
const HERE = dirname(fileURLToPath(import.meta.url));
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const bundle = process.argv[2] || join(HERE, "data.json");
  const data = JSON.parse(readFileSync(bundle, "utf8"));
  const app = createRequire(import.meta.url)(join(HERE, "app.js"));
  const { errors, warnings } = checkConsistency(data, app);
  for (const w of warnings) console.warn(`check-consistency: WARNING: ${w}`);
  if (errors.length) {
    for (const e of errors) console.error(`check-consistency: FAIL: ${e}`);
    console.error(`check-consistency: ${errors.length} divergence(s); the build must not ship.`);
    process.exit(1);
  }
  console.log(`check-consistency: ${data.gateways.length} gateways consistent across table/drawer/compare/charts (${warnings.length} warning(s))`);
}
