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

/* What charts.py reads for a gateway's passthrough metric. This must mirror charts.py:_overlay_perf
   at the FIELD level (audit R5-#5): _overlay_perf overwrites obj[f] only when best_cell[f] is not null,
   so a best_cell that is present but carries a NULL field falls THROUGH to the raw perf-suite value in
   the PNG - it does not force null. The prior mirror returned null whenever best_cell existed, so a
   present-but-null best_cell field beside a non-null perf fallback let the guard see table===drawer===
   charts===null and pass the bundle "consistent" while the chart actually drew the perf number. Mirror
   the overlay: use best_cell[key] when non-null, else fall through to the raw perf value exactly as
   _overlay_perf does. */
function chartsPassValue(g, key) {
  if (g.best_cell && g.best_cell[key] != null) return g.best_cell[key];
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
    // ---- streaming: the SAME single-source rule as passthrough. The streaming headline shown on the
    // board (table via streamCell, drawer/compare via canonicalStreaming) MUST be the streaming of the
    // BEST DIAGONAL matrix cell it is projected from — i.e. the same (ingress==egress) passthrough cell
    // the perf headline is projected from. If the projected streaming diverges from that cell's own
    // stream record, the two were read off different sources and this FAILS. Only asserted for a
    // matrix-sourced projection with a matrix present (a legacy stream-fallback carries no matrix cell
    // to compare against and is covered by the coverage guard instead).
    const s = g.streaming;
    if (s && s.source === "matrix" && g.matrix && g.matrix.upstreams) {
      const cell = g.matrix.upstreams[s.dialect]?.cells?.[s.dialect];
      const cellStream = cell && cell.stream;
      if (!cellStream) {
        errors.push(`${g.key}.streaming: projected from the ${s.dialect} diagonal but that matrix cell has no stream record`);
      } else {
        // table == drawer/compare: both read canonicalStreaming, so compare the table accessor against
        // the diagonal cell's own numbers to prove the projection did not drift from its source.
        const skeys = ["added_ttft_p99_us", "added_gap_p99_us", "streams_sustained", "cpu_fps"];
        for (const key of skeys) {
          const table = app.streamCell(g, key, String).v;   // n/a -> undefined via .v
          let headline = s[key] ?? null;
          let cellVal = cellStream[key] ?? null;
          // MEDIUM-6: cpu_fps is GATED — the site (streamCell/canonicalStreaming) shows it only when the
          // number is certified (present + positive + cpu_fps_mock_bound === false), exactly the rule
          // charts.py uses for streamcpu_valid (which suppresses the bar otherwise). So for cpu_fps the
          // canonical comparison target is the GATED value on BOTH the projected headline and the
          // diagonal cell: an uncertified/mock-bound cpu_fps reads n/a on every surface, matching the
          // chart's suppressed bar. Comparing the raw cell value here would (wrongly) demand the site
          // render a number the chart deliberately hides.
          if (key === "cpu_fps") {
            headline = app.cpuFpsCertified(s) ? headline : null;
            cellVal = app.cpuFpsCertified(cellStream) ? cellVal : null;
          }
          const tableVal = table == null ? null : table;
          if (!(tableVal === headline && headline === cellVal)) {
            errors.push(`${g.key}.streaming.${key}: table=${tableVal} headline=${headline} diagonal-cell=${cellVal} ` +
              `(the streaming headline must be the best diagonal cell's streaming it is projected from)`);
          }
        }
        // MEDIUM-6 (explicit visibility tie): the site's cpu-fps VISIBILITY must equal the chart's
        // streamcpu_valid rule so the two can never silently diverge again. streamcpu_valid (charts.py
        // _proj_streaming) = cpu present + positive + NOT mock-bound; the site shows cpu_fps iff
        // cpuFpsCertified. Assert the two booleans are identical for this projected streaming record.
        const siteShowsCpu = app.streamCell(g, "cpu_fps", String).v != null;
        const chartCpuValid = app.cpuFpsCertified(s);   // mirrors charts.py streamcpu_valid on this record
        if (siteShowsCpu !== chartCpuValid) {
          errors.push(`${g.key}.streaming.cpu_fps: site-visible=${siteShowsCpu} but chart streamcpu_valid=${chartCpuValid} ` +
            `(the drawer/table must show cpu-fps exactly when the chart draws its bar — same mock-bound rule)`);
        }
      }
    }
    // ---- ONE SOURCE OF TRUTH (sweep chart vs headline): the published headline MUST be a point on
    // its OWN charted sweep. rps_max_proxy/_concurrency must equal the max-rps point of the charted
    // sweep_max_proxy array (same value AND concurrency), and likewise sustained@20ms vs
    // sweep_sustained_20ms. If the headline is measured by one code path and the curve by another,
    // the max of the curve won't match the headline and this FAILS - exactly the two-sources bug.
    // Only asserted when the canonical record carries the sweep array (a regenerated bundle); a legacy
    // bundle with no array is silently skipped (the coverage/other guards cover its provenance).
    const canon = app.canonicalPerf(g);
    if (canon && canon.served !== false) {
      const pairs = [
        ["rps_max_proxy", "rps_max_proxy_concurrency", "sweep_max_proxy"],
        ["rps_sustained_20ms", "rps_sustained_20ms_concurrency", "sweep_sustained_20ms"],
      ];
      for (const [rk, ck, ak] of pairs) {
        const arr = canon[ak];
        if (!Array.isArray(arr) || !arr.length) continue; // legacy / no charted array: skip
        const head = canon[rk];
        if (head == null) continue;
        // The max-rps point of the charted array is what the curve peaks at. The headline must BE it.
        const peak = arr.reduce((a, b) => (b.rps > a.rps ? b : a));
        if (peak.rps !== head) {
          errors.push(`${g.key}.${rk}: headline=${head} but charted ${ak} peaks at ${peak.rps} ` +
            `(@ conc=${peak.conc}); the published number must be a point on its own sweep curve`);
        } else if (canon[ck] != null && peak.conc !== canon[ck]) {
          errors.push(`${g.key}.${rk}: headline concurrency=${canon[ck]} but the charted peak of ${ak} ` +
            `is at conc=${peak.conc}; the marked operating concurrency must match the headline`);
        }
      }
    }
    // ---- plausibility (WARN only, R7): two independent measured ceilings may invert on noise.
    // DISTINCT case (H4): max-proxy === 0 means the ceiling run found NO tested load that held
    // p99 < 1 s at <0.1% errors, i.e. that run did not qualify at all. That is NOT sweep noise
    // and must never be filed under the small-inversion story (arch's 18-vs-0 is this case).
    const bc = g.best_cell;
    if (bc && bc.rps_max_proxy === 0) {
      warnings.push(`${g.key}: max-proxy run did not qualify (rps_max_proxy=0: no tested load held ` +
        `p99 < 1 s at <0.1% errors); not noise, shipped as measured`);
    } else if (bc && bc.rps_sustained_20ms != null && bc.rps_max_proxy != null && bc.rps_sustained_20ms > bc.rps_max_proxy) {
      warnings.push(`${g.key}: sustained@20ms ${bc.rps_sustained_20ms} > max-proxy ${bc.rps_max_proxy} ` +
        `(independent per-cell sweep ceilings; noise, shipped unclamped)`);
    }
    // ---- coverage (WARN, M7): a served (green) matrix cell with NO per-cell perf renders as an
    // all-n/a row on any tab that reads that exact cell (the bifrost translation-row case). The
    // cell is honest (served, unmeasured), but an unmeasured green cell should be loud in the
    // build log so a half-finished sweep never ships silently.
    if (g.matrix && g.matrix.upstreams) {
      const unswept = [];
      for (const [egress, up] of Object.entries(g.matrix.upstreams)) {
        for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
          if (cell && cell.served === true && !cell.perf) unswept.push(`${ingress}->${egress}`);
        }
      }
      if (unswept.length) {
        warnings.push(`${g.key}: ${unswept.length} served matrix cell(s) with no per-cell perf ` +
          `(${unswept.join(", ")}); these render all-n/a on the tabs that read them`);
      }
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
