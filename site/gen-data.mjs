#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// gen-data.mjs: build the static data bundle for the results site. No dependencies.
//
// The site (onthebench.ai) is a category-based benchmark platform; this script emits the
// data bundle for the GATEWAYS category, today served as site/data.json. CATEGORY SEAM:
// when a second category lands (e.g. models), give each category its own bundle under
// site/data/<category>.json (a per-category generator or a section here), and register it
// in CATEGORIES in app.js; the emitted `category` field names which bundle this is.
//
// Scans gateways/*/gateway.sh (the self-describing manifests: GW_DISPLAY, GW_LANG, GW_REPO)
// plus results/{perf,memory,stream,streamcpu,xlate,matrix}/<gateway>.json, and emits
// site/data.json. Also copies the generated chart PNGs (results/*.png) into site/charts/
// and the bundled Inter fonts (assets/fonts) into site/fonts/ so the site/ directory is a
// self-contained Pages artifact, and writes 404.html (a copy of the app shell) so hosts
// without _redirects support (GitHub Pages) still deep-link into /gateways/<view> paths.
//
//   node site/gen-data.mjs [repoRoot] [outDir]
//
// Defaults: repoRoot = the directory above this script, outDir = this script's directory.
// Absent suites, absent gateways and absent charts are all handled cleanly: the site reads
// whatever this emits and renders "not measured" for the gaps.

import { readdirSync, readFileSync, statSync, existsSync, mkdirSync, copyFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { sealMetric, makeSource, SWEEP } from "./seal.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = process.argv[2] || join(HERE, "..");
const OUT = process.argv[3] || HERE;

// GOVERNANCE RETIRED (matrix-sole-source): governance is not measured on the board — the governed
// suite was busbar-only and is retired. `governed/run.sh` stays on disk (unused) but the suite is
// no longer scanned into the bundle and no governed column/derivation is emitted. See app.js.
// NOTE: "memory" is intentionally NOT scanned. The retired standalone memory suite wrote synthetic
// burst numbers (conc=1500, 150KB payload, 120s) that mislabelled as 6x6 provenance; memory now comes
// SOLELY from the matrix's post-6x6 peak-cell window (g.matrix.memory, projected below). No fallback.
const SUITES = ["perf", "stream", "streamcpu", "xlate", "matrix"];
// The ungated (non-honesty-gated) latency-shaped metrics on a perf cell: always certified when present.
const UNGATED_LAT = ["added_latency_p50_us", "added_latency_p99_us", "gateway_c1_p99_us", "direct_c1_p99_us"];
// The RSS metric fields on a memory block (ungated — no mock-bound flag).
const MEM_RSS = ["idle_rss_mib", "peak_rss_mib", "recovered_rss_mib"];

// ---- gateway manifests ------------------------------------------------------
function parseManifest(text) {
  // Values are either quoted ("LiteLLM · Python") or a bare word (Rust); a trailing
  // shell comment may follow either form.
  const get = (name) => {
    const m = text.match(new RegExp(`^${name}=(?:"([^"]*)"|(\\S+))`, "m"));
    return m ? (m[1] ?? m[2]) : null;
  };
  // GW_CLASS is each project's OWN self-description (its README/site tagline: "control
  // plane", "LLM gateway", "API gateway", ...), never our editorial classification.
  // Missing/unknown falls back to the neutral "Gateway".
  return { display: get("GW_DISPLAY"), lang: get("GW_LANG"), repo: get("GW_REPO"), cls: get("GW_CLASS") };
}

const gatewaysDir = join(ROOT, "gateways");
const gatewayKeys = existsSync(gatewaysDir)
  ? readdirSync(gatewaysDir).filter((d) => {
      try {
        return statSync(join(gatewaysDir, d)).isDirectory() && existsSync(join(gatewaysDir, d, "gateway.sh"));
      } catch { return false; }
    }).sort()
  : [];

function readJson(path) {
  try { return JSON.parse(readFileSync(path, "utf8")); } catch { return null; }
}

// newestSnapshot(key): the newest results/snapshots/result_<key>_<ts>.json by its own measured_at (an
// on-disk archive of every run; the newest wins). Returns the parsed snapshot or null (no snapshot yet).
const SNAP_DIR = join(ROOT, "results", "snapshots");
function newestSnapshot(key) {
  if (!existsSync(SNAP_DIR)) return null;
  let best = null, bestMs = -1;
  for (const f of readdirSync(SNAP_DIR)) {
    if (!f.startsWith(`result_${key}_`) || !f.endsWith(".json")) continue;
    const snap = readJson(join(SNAP_DIR, f));
    if (!snap) continue;
    const ms = snap.measured_at ? Date.parse(snap.measured_at) : 0;
    if (ms > bestMs) { bestMs = ms; best = snap; }
  }
  return best;
}

// GitHub star snapshot for the Gateways overview: a COMMITTED build-time file
// (gateways/stars.json, refreshed by `node gateways/fetch-stars.mjs`), never a live
// API call, so the bundle stays reproducible and CI needs no network. Absent file or
// absent key degrades to null; the site renders those muted.
const starsSnap = readJson(join(gatewaysDir, "stars.json")) || {};

const gateways = gatewayKeys.map((key) => {
  const meta = parseManifest(readFileSync(join(gatewaysDir, key, "gateway.sh"), "utf8"));
  const g = {
    key,
    display: meta.display || key,
    lang: meta.lang || "Other",
    cls: meta.cls || "Gateway",
    // Only accept an https:// repo URL. app.js interpolates g.repo RAW into href="${g.repo}" at four
    // render sites (display is esc()'d, href is not), so a manifest GW_REPO like
    // `x" onfocus=alert(...) autofocus="` or a `javascript:` scheme would inject on the public board.
    // Validating the scheme/format here (reject to null otherwise) closes that sink (audit R2-L2).
    repo: (typeof meta.repo === "string" && /^https:\/\/[^\s"'<>]+$/.test(meta.repo)) ? meta.repo : null,
    stars: starsSnap[key]?.stars ?? null,
    stars_as_of: starsSnap[key]?.as_of ?? null,
    // Project age context: the repo's FIRST-commit date (not created_at, which resets on
    // renames). Rendered as a simple relative age — 43k stars over 10 years and 100 over 3
    // weeks are different statements.
    first_commit: starsSnap[key]?.first_commit ?? null,
  };
  for (const suite of SUITES) {
    const j = readJson(join(ROOT, "results", suite, `${key}.json`));
    if (j) g[suite] = j;
  }
  // ---- snapshot artifact (task #65): the SINGLE self-describing per-gateway run ------------------
  // Prefer the NEWEST snapshot (results/snapshots/result_<key>_<measured_at>.json) as the source of the
  // matrix + config: config lives INSIDE the same file as the numbers it produced, killing the config-
  // drift class. A gateway with no snapshot yet keeps the per-suite read path above (transition; null-
  // safe). The snapshot's matrix carries the SAME sealed-envelope-producing cells, so projection is
  // unchanged. Its inline config.files replace the config/<gw>.txt sidecar read.
  const snap = newestSnapshot(key);
  if (snap) {
    if (snap.matrix) g.matrix = snap.matrix;                       // matrix from the snapshot (sole source)
    const files = snap.config && snap.config.files;
    if (files && typeof files === "object") {
      const parts = Object.entries(files).map(([name, body]) =>
        Object.keys(files).length > 1 ? `# ${name}\n${body}` : String(body));
      if (parts.length) g.ootb_config = parts.join("\n\n");        // inline config render (no sidecar)
    }
  }
  // ---- OOTB config artifact (sidecar fallback, no snapshot) ----------------------------------------
  // Only when the snapshot did not supply config: the gateway ran from its as-shipped DEFAULT config and
  // the exact config it used is captured to results/config/<key>.txt (lib/harness.sh harness_write_config).
  // A gateway with no artifact stays absent and the board renders "not published".
  if (g.ootb_config == null) {
    const cfgPointer = (g.perf && typeof g.perf.ootb_config === "string") ? g.perf.ootb_config : `config/${key}.txt`;
    const cfgPath = join(ROOT, "results", cfgPointer);
    if (existsSync(cfgPath)) {
      try { g.ootb_config = readFileSync(cfgPath, "utf8"); } catch { /* unreadable → absent */ }
    }
  }
  if (g.matrix) {
    normalizeMatrix(g.matrix);
    // CANONICAL RULE: the per-cell MATRIX sweep is the single source of truth for all passthrough +
    // translation perf; the standalone perf/xlate suites are a LIVE deferred FALLBACK (a gateway with
    // no matrix sweep for that path). g.best_cell / g.translation_cell are the ONE canonical record
    // every surface reads (table, drawer, compare, charts.py). Every metric is SEALED here into an
    // envelope (seal.mjs): the raw scalar + its _mock_bound flag are CONSUMED, never re-emitted — a
    // render site has no ungated field to leak. The cell's `source` stamp discloses provenance and
    // drives every caption (no hard-coded source string can drift). See seal.mjs / Design E.
    const build = g.matrix.build ?? null, at = g.matrix.measured_at ?? null;
    // Per-cell perf (matrix v2 + sweep): the gateway's BEST green diagonal by sustained RPS @20ms.
    const bc = bestCell(g.matrix);
    if (bc) g.best_cell = sealPerfCell(bc, { ingress: bc.dialect, egress: bc.dialect, dialect: bc.dialect },
      makeSource("matrix", SWEEP.DIAGONAL, build, at));
    // The gateway's TRANSLATION cell (openai in -> best non-openai egress).
    const tc = translationCell(g.matrix);
    if (tc) g.translation_cell = sealPerfCell(tc, { ingress: tc.ingress, egress: tc.egress },
      makeSource("matrix", SWEEP.TRANSLATION, build, at));
    // STREAMING projection (matrix single source): the BEST DIAGONAL cell's streaming — the SAME
    // (ingress==egress) cell the headline perf is projected from (one source of truth). Only when the
    // diagonal ACTUALLY STREAMED (stream_served===true); a non-streaming cell leaves g.streaming absent.
    if (bc) {
      const cell = g.matrix.upstreams?.[bc.dialect]?.cells?.[bc.dialect];
      if (cell && cell.stream && cell.stream.stream_served === true) {
        g.streaming = sealStreaming(cell.stream, bc.dialect, makeSource("matrix", SWEEP.STREAM_DIAGONAL, build, at));
      }
    }
    // MEMORY projection (matrix SOLE source): the post-6x6 memory window (matrix.memory) — a fixed
    // identical load on THIS gateway's peak cell (load_cell), on a fresh cold-restarted process. RSS is
    // UNGATED (no mock-bound flag), so its envelopes are certified-or-not-measured; load_cell/load_recipe
    // /rss_series travel verbatim. A window that did not serve leaves g.memory_read absent (renders n/a).
    if (g.matrix.memory && g.matrix.memory.served === true) {
      g.memory_read = sealMemory(g.matrix.memory, makeSource("matrix", SWEEP.MEMORY, build, at));
    }
    // SEAL every matrix cell in-place (AFTER selection/projection, which read raw). The matrix popup +
    // Protocol view read cell.perf / cell.stream directly, so those must be envelopes too — otherwise a
    // raw ungated scalar (and its _mock_bound flag) survives in the bundle (invariant C1).
    sealMatrixCellsInPlace(g.matrix);
  }
  // LIVE DEFERRED FALLBACKS (stay until the field run folds them into the matrix; DO NOT break them).
  // Each is sealed with its OWN honest `source` stamp (stream-suite / perf-suite / xlate-suite), so the
  // envelope is correct NOW and captions tell the truth about provenance. There is NO memory fallback —
  // the retired synthetic burst suite mislabelled as 6x6 provenance and is neither scanned nor read.
  if (!g.streaming && g.stream && g.stream.stream_served === true) {
    const dia = passthroughDialect(g.matrix);
    g.streaming = sealStreaming({
      added_ttft_p50_us: g.stream.stream_added_ttft_p50_us,
      added_ttft_p99_us: g.stream.stream_added_ttft_p99_us,
      added_gap_p50_us: g.stream.stream_added_gap_p50_us,
      added_gap_p99_us: g.stream.stream_added_gap_p99_us,
      streams_sustained: g.stream.stream_sustained_streams,
      streams_sustained_fps: g.stream.stream_sustained_fps,
      streams_sustained_mock_bound: g.stream.stream_mock_bound ?? null,
      cpu_fps: g.streamcpu ? g.streamcpu.streamcpu_frames_per_sec : null,
      cpu_fps_concurrency: g.streamcpu ? g.streamcpu.streamcpu_concurrency : null,
      cpu_fps_mock_bound: g.streamcpu ? g.streamcpu.streamcpu_mock_bound : null,
    }, dia, makeSource("stream-fallback", SWEEP.STREAM_SUITE, g.stream.build ?? null, g.stream.measured_at ?? null));
  }
  if (!g.best_cell && g.perf && g.perf.served === true && g.perf.added_latency_p99_us != null) {
    // No swept diagonal, but the perf suite ran the gateway's default passthrough. Seal it into the same
    // canonical shape with source:"perf-fallback" so provenance is visible on every surface.
    const dia = passthroughDialect(g.matrix);
    g.best_cell = sealPerfCell({
      added_latency_p50_us: g.perf.added_latency_p50_us,
      added_latency_p99_us: g.perf.added_latency_p99_us,
      rps_sustained_20ms: g.perf.rps_sustained_20ms,
      rps_sustained_20ms_concurrency: g.perf.rps_sustained_20ms_concurrency ?? null,
      rps_sustained_20ms_mock_bound: g.perf.rps_sustained_20ms_mock_bound ?? null,
      rps_max_proxy: g.perf.rps_max_proxy,
      rps_max_proxy_concurrency: g.perf.rps_max_proxy_concurrency ?? null,
      rps_max_proxy_mock_bound: g.perf.rps_max_proxy_mock_bound ?? null,
      sweep_max_proxy: g.perf.sweep_max_proxy ?? null,
      sweep_sustained_20ms: g.perf.sweep_sustained_20ms ?? null,
    }, { ingress: dia, egress: dia, dialect: dia },
      makeSource("perf-fallback", SWEEP.PERF_SUITE, g.perf.build ?? null, g.perf.measured_at ?? null));
  }
  if (!g.translation_cell && g.xlate && g.xlate.xlate_served === true && g.xlate.xlate_added_latency_p99_us != null) {
    // Legacy xlate suite (anthropic in -> openai out — the OPPOSITE direction of the matrix cell).
    // Sealed with its real direction + source:"xlate-fallback" so the two paths can never be confused.
    g.translation_cell = sealPerfCell({
      added_latency_p50_us: g.xlate.xlate_added_latency_p50_us,
      added_latency_p99_us: g.xlate.xlate_added_latency_p99_us,
      rps_sustained_20ms: g.xlate.xlate_rps_sustained_20ms,
      rps_sustained_20ms_concurrency: g.xlate.xlate_rps_sustained_20ms_concurrency ?? null,
      rps_sustained_20ms_mock_bound: g.xlate.xlate_rps_sustained_20ms_mock_bound ?? null,
    }, { ingress: "anthropic", egress: "openai" },
      makeSource("xlate-fallback", SWEEP.XLATE_SUITE, g.xlate.build ?? null, g.xlate.measured_at ?? null));
  }
  // GOVERNANCE RETIRED: no `supports_governed` derivation. Under matrix-sole-source governance is not
  // a board metric (the governed suite was busbar-only and is retired), so the board neither emits a
  // governed column nor a supports_governed capability flag.
  //
  // PER-GATEWAY freshness stamp: each gateway carries its OWN newest measurement so the board can show
  // an independent "measured Nd ago" per row and flag a row that has aged past MAX_GATEWAY_AGE_DAYS.
  // Different gateways legitimately have different measured_at (busbar today, kong 3 weeks ago) — that
  // is honest on a living board where any one gateway can be re-run alone. The staleness flag drives a
  // per-row badge in app.js; it is NOT a build failure (see the freshness guard below).
  // LOW-R3-3: the badge stamp must reflect the age of the DISPLAYED numbers, which are projected from
  // g.matrix ONLY (best_cell / streaming / memory_read). Deriving it from the MAX across all suites let a
  // newer legacy results/perf/<gw>.json (reachable via an ad-hoc SUITES=perf re-run) drive a "measured 5d
  // ago" badge while the shown matrix numbers were 90d old — the badge overstating freshness. Prefer the
  // matrix stamp; fall back to the newest-across-suites only when there is no matrix (a legacy-only row
  // whose numbers age by that stamp anyway). The staleness flag below is re-derived on the same basis.
  // LOW-R3-3 / MED-1: the per-row badge stamp ages the DISPLAYED (matrix-preferring) numbers — the SAME
  // shared basis the board-level footer + wholesale-stale floor now use (displayedMeasuredMs). Hoisted
  // function declaration, so it is callable here even though it is defined below.
  const gAtMs = displayedMeasuredMs(g);
  g.measured_at = gAtMs > 0 ? new Date(gAtMs).toISOString() : null;
  return g;
});

// Matrix v1 results carry one upstream shape (fixed openai) as top-level `cells`; v2 carries the
// full 6x6 under `upstreams.<egress>.cells` plus the same top-level compat row. Normalize v1 into
// the v2 shape so the site renders exactly one structure: the one measured egress column becomes
// `upstreams`, and the columns v1 never probed stay absent (the site renders them "not measured",
// which is the honest reading of a v1 run: unmeasured, not "not configurable").
// The gateway's BEST passthrough cell for the Passthrough tab (BEST-OF): its same-dialect diagonal
// (ingress === egress, pure forwarding, no translation), chosen deterministically as the canonical
// `openai` diagonal when served (every gateway on the identical fair workload), else the gateway's
// fastest NATIVE diagonal by lowest added latency (e.g. litellm-rust -> anthropic). BEST-OF, not
// strict-openai, so EVERY gateway appears on its best passthrough; filtering a competitor out reads
// as hiding it. `dialect` (== ingress == egress) is the label the tab's "Tested on" pill shows.
// ---- envelope sealers (Design E §2): raw cell -> sealed, envelope-carrying record ----------
// The projected record carries `path` (ingress/egress/dialect), `source` (the provenance stamp), and one
// SEALED envelope per metric under its own field name. The raw scalar + its _mock_bound flag are consumed
// here and never re-emitted, so no ungated field survives for a render site to leak (invariant P1).
// A throughput metric: its sealed envelope folds in the concurrency + charted sweep array + the NEW
// conc_at_* rung so the headline, its operating concurrency, and its curve all travel as one datum.
function sealThroughput(perf, key, concAtKey) {
  return sealMetric(perf[key], {
    gated: true, flag: perf[`${key}_mock_bound`],
    extras: {
      concurrency: perf[`${key}_concurrency`] ?? null,
      conc_at: perf[concAtKey] ?? null,          // NEW (snapshot #65): the rung peak/sustained held at
      sweep: perf[`sweep_${key === "rps_sustained_20ms" ? "sustained_20ms" : "max_proxy"}`] ?? null,
    },
  });
}
// sealPerfCellPerf: a raw perf object -> {<sealed metrics>} (no path/source; the caller stamps those).
// Used BOTH for the canonical best_cell/translation_cell AND to seal every matrix cell in-place, so the
// matrix popup reads envelopes, never raw scalars (invariant C1: no ungated field survives in the bundle).
function sealPerfCellPerf(perf) {
  const rec = {};
  for (const k of UNGATED_LAT) if (perf[k] != null) rec[k] = sealMetric(perf[k], {});
  rec.rps_sustained_20ms = sealThroughput(perf, "rps_sustained_20ms", "conc_at_sustained");
  rec.rps_max_proxy = sealThroughput(perf, "rps_max_proxy", "conc_at_peak");
  if (perf.egress_reverified != null) rec.egress_reverified = perf.egress_reverified;
  return rec;
}
// sealPerfCell: a matrix/fallback perf object -> the canonical {path, source, <sealed metrics>} record.
function sealPerfCell(perf, path, source) {
  return { path: { ...path }, source, ...sealPerfCellPerf(perf) };
}
// sealStreamRecord: a raw stream record -> {<sealed metrics>} (no path/source). TTFT/gap are UNGATED;
// streams_sustained + cpu_fps are GATED on their mock-bound flags. Used for the canonical g.streaming AND
// for sealing every matrix cell's own .stream in-place (so the popup reads envelopes).
function sealStreamRecord(s) {
  const rec = {};
  for (const k of ["added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us", "added_gap_p99_us", "streams_sustained_fps"])
    if (s[k] != null) rec[k] = sealMetric(s[k], {});
  // Streaming counts: a 0 is "not measured" (n/a), never an honest measured-zero (unlike an RPS ceiling).
  rec.streams_sustained = sealMetric(s.streams_sustained, { gated: true, flag: s.streams_sustained_mock_bound, zeroMeasured: false });
  rec.cpu_fps = sealMetric(s.cpu_fps, { gated: true, flag: s.cpu_fps_mock_bound, zeroMeasured: false,
    extras: { concurrency: s.cpu_fps_concurrency ?? null } });
  return rec;
}
function sealStreaming(s, dialect, source) {
  return { path: { dialect }, source, stream_served: true, ...sealStreamRecord(s) };
}
// sealMemory: the post-6x6 memory window -> canonical record. RSS metrics are UNGATED (no mock-bound
// flag); load_cell / load_recipe / rss_series travel verbatim (the fair-load basis + recovery curve).
function sealMemory(mem, source) {
  const rec = { source, served: true };
  for (const k of ["idle_rss_mib", "peak_rss_mib", "recovered_rss_mib"])
    rec[k] = sealMetric(mem[k], {});
  for (const k of ["load_cell", "load_recipe", "rss_series", "protocol"])
    if (mem[k] != null) rec[k] = mem[k];
  return rec;
}
// sealMatrixCellsInPlace: replace every served cell's raw perf/stream AND the top-level memory block's raw
// RSS with SEALED envelopes, so the matrix popup + Protocol view + the embedded/snapshot matrix carry
// envelopes, never raw scalars — NO ungated metric field survives anywhere in the bundle (invariant C1).
// Non-metric fields (served/status/path/verdict_note/load_cell/rss_series/…) are untouched.
function sealMatrixCellsInPlace(m) {
  const seen = new Set();   // v1 shares m.cells with upstreams[shape].cells (same refs) — seal once.
  const cellGroups = [m.cells, ...Object.values(m.upstreams || {}).map((u) => u && u.cells)];
  for (const cells of cellGroups) {
    if (!cells || typeof cells !== "object") continue;
    for (const cell of Object.values(cells)) {
      if (!cell || typeof cell !== "object" || seen.has(cell)) continue;
      seen.add(cell);
      if (cell.perf) cell.perf = sealPerfCellPerf(cell.perf);
      if (cell.stream && cell.stream.stream_served === true) {
        cell.stream = { stream_served: true, ...sealStreamRecord(cell.stream) };
      }
    }
  }
  // The raw memory block (the source for the g.memory_read projection) also travels in the bundle (embedded
  // in g.matrix + the snapshot); seal its RSS scalars so no bare ungated field survives.
  if (m.memory && typeof m.memory === "object") {
    for (const k of MEM_RSS) if (k in m.memory) m.memory[k] = sealMetric(m.memory[k], {});
  }
}

function bestCell(m) {
  if (!m.upstreams) return null;
  const diag = [];
  for (const [egress, up] of Object.entries(m.upstreams)) {
    const cell = up && up.cells && up.cells[egress];        // ingress === egress
    if (cell && cell.served === true && cell.perf && cell.perf.added_latency_p99_us != null)
      diag.push({ ingress: egress, egress, dialect: egress, ...cell.perf });
  }
  if (!diag.length) return null;
  const openai = diag.find((d) => d.dialect === "openai");
  if (openai) return openai;
  return diag.reduce((a, b) => (b.added_latency_p99_us < a.added_latency_p99_us ? b : a));
}

// The gateway's TRANSLATION cell for the Translation tab: openai INGRESS (fixed fair input) translated
// to its best non-openai EGRESS. "Best" = LOWEST added latency p99 (a proxy's quality is its overhead;
// RPS is capacity-bound and noisier). Fixing ingress to openai keeps the input side identical across
// gateways; the egress varies and is shown as the row's path pill. Returns {ingress:"openai", egress,
// ...perf} or null when the gateway serves no openai-in translation path.
function translationCell(m) {
  if (!m.upstreams) return null;
  const cands = [];
  for (const [egress, up] of Object.entries(m.upstreams)) {
    if (egress === "openai") continue;                      // openai->openai is passthrough, not translation
    const cell = up && up.cells && up.cells.openai;         // openai ingress -> this egress
    if (cell && cell.served === true && cell.perf && cell.perf.added_latency_p99_us != null)
      cands.push({ ingress: "openai", egress, ...cell.perf });
  }
  if (!cands.length) return null;
  return cands.reduce((a, b) => (b.added_latency_p99_us < a.added_latency_p99_us ? b : a));
}

// The dialect a perf-suite fallback was measured on: the gateway's default passthrough. Prefer the
// openai diagonal when green (the common default), else the first served diagonal, else openai.
function passthroughDialect(m) {
  if (!m || !m.upstreams) return "openai";
  const oa = m.upstreams.openai;
  if (oa && oa.cells && oa.cells.openai && oa.cells.openai.served === true) return "openai";
  for (const [egress, up] of Object.entries(m.upstreams)) {
    const cell = up && up.cells && up.cells[egress];
    if (cell && cell.served === true) return egress;
  }
  return "openai";
}

function normalizeMatrix(m) {
  if (m.upstreams || !m.cells) return;
  const shape = m.upstream_shape || "openai";
  m.matrix_version = 1;
  m.upstreams = { [shape]: { configurable: true, served: m.served !== false, cells: m.cells } };
}

// ---- hardware stamp (most common perf/memory "hardware" string) -------------
const hwCounts = new Map();
let latest = null;
for (const g of gateways) {
  for (const suite of SUITES) {
    const j = g[suite];
    if (!j) continue;
    if (j.hardware) hwCounts.set(j.hardware, (hwCounts.get(j.hardware) || 0) + 1);
    // NIT-R2-3: pick the newest instant by PARSED epoch ms, not a lexicographic ISO-string compare.
    // A mixed-precision compare mis-orders sibling stamps ('...06Z' sorts above '...06.5Z' because
    // 'Z' > '.'), which could under-select the true-newest instant the future-date hard-fail (:313)
    // then relies on. Match the Date.parse comparison used everywhere else (:300-302/313/340/376/392).
    if (j.measured_at && (!latest || Date.parse(j.measured_at) > Date.parse(latest))) latest = j.measured_at;
  }
}
const hardware = [...hwCounts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] || null;
// MED-1: the board footer "Latest measurement: Nd ago" must reflect the DISPLAYED numbers (matrix-
// preferring), NOT the max across all suites (which folds a never-displayed legacy re-run and overstates
// freshness). `latest` above still folds all suites for the future-date corruption hard-fail (:324) —
// a future-dated legacy stamp is still corruption — but the PUBLISHED footer uses the displayed basis.
const latestDisplayedMs = Math.max(...gateways.map(displayedMeasuredMs), 0);
const latestDisplayed = latestDisplayedMs > 0 ? new Date(latestDisplayedMs).toISOString() : null;

// The newest embedded measurement across a gateway's own suites, in epoch ms (0 when it has none).
function newestMeasuredMs(g) {
  return Math.max(...SUITES.map((s) => g[s] && g[s].measured_at).filter(Boolean).map((a) => Date.parse(a)).concat([0]));
}

// MED-1 / MED-2: the stamp that actually drives what the board DISPLAYS, in epoch ms (0 when none).
// EVERY displayed number now projects from g.matrix ONLY (best_cell / streaming / memory_read), so the
// board-level freshness footer AND the wholesale-stale hard-fail must age those displayed numbers — the
// matrix stamp when present, falling back to the newest-across-suites only for a legacy-only row (whose
// numbers legitimately age by that stamp). This is the SAME basis the per-gateway ageBasisMs (:415) uses.
// Folding retired legacy suite timestamps in (newestMeasuredMs) let a never-displayed ad-hoc SUITES=perf
// re-run make a wholesale-stale matrix board look fresh (MED-1) or slip past the 180-day floor (MED-2).
function displayedMeasuredMs(g) {
  const matrixAt = g.matrix && g.matrix.measured_at ? Date.parse(g.matrix.measured_at) : 0;
  return matrixAt > 0 ? matrixAt : newestMeasuredMs(g);
}

// ---- freshness guard (matrix-sole-source) -----------------------------------
// The bundle is regenerated from the raw results tree on every run, so generated_at must never
// precede the newest embedded measurement: if it does, a raw result is future-dated (clock skew
// on the rig) and a "fresh" bundle would look older than its data. Hard fail; never ship it.
const generatedAt = new Date().toISOString();
// NIT: compare as parsed epoch ms, not raw ISO strings. A lexicographic `generatedAt < latest`
// is only correct when both are the SAME ISO precision/zone; a fractional-second vs whole-second
// mismatch can mis-order two instants that are microseconds apart (this is what forced
// verify-local.sh's `sleep 2` workaround — Date.parse comparison lets it drop that; not touched here).
if (latest && Date.parse(generatedAt) < Date.parse(latest)) {
  throw new Error(`gen-data: generated_at ${generatedAt} predates the newest embedded measured_at ${latest}; ` +
    `a raw result is future-dated (rig clock skew?). Refusing to emit a bundle that would read stale.`);
}

// WHAT CHANGED (matrix-sole-source). A gateway's ENTIRE benchmark is now ONE atomic matrix run that
// legitimately takes HOURS (busbar ~5h), and gateways are published INDEPENDENTLY (busbar can be
// re-run and pushed alone, kong's row stays from 3 weeks ago). The two RELATIVE guards the old model
// used are therefore both WRONG now and are REMOVED:
//   - The intra-row SPAN hard-fail (MAX_SPAN_H ~3h) assumed a row mixed several short suites from
//     different runs. Under one atomic matrix run there are no "mixed suites" to catch, and a single
//     legitimate run's timestamps span hours — so the span check false-fails every real run. Replaced
//     by a GENEROUS sanity cap (MAX_ROW_SPAN_SANITY_H) far above any real run, purely to catch a
//     clearly-corrupt/future-dated timestamp within a row; a real run never approaches it.
//   - The cross-gateway LAG hard-fail (MAX_LAG_H) assumed one field run updated every box together, so
//     a lagging row meant a failed refresh. On a living board with per-gateway cadences, different
//     measured_at is HONEST and EXPECTED — updating just busbar must not make every other gateway a
//     hard-fail. REMOVED entirely.
// KEPT: the wholesale-stale ABSOLUTE floor (soft anchor) — if the newest measurement ANYWHERE on the
// board is older than MAX_BOARD_AGE_DAYS, the WHOLE board is stale (nothing refreshed at all) and the
// bundle must not publish generated_at=now over it.
// NEW: a PER-GATEWAY absolute age SIGNAL. A gateway whose own newest measurement is older than
// MAX_GATEWAY_AGE_DAYS gets a per-row `stale` flag (drives the app.js badge) — NOT a build failure.
// This makes independent update cadences visible without blocking per-gateway updates.
const MAX_ROW_SPAN_SANITY_H = 12;  // sanity-only: one atomic matrix run is hours; >12h means a corrupt/skewed stamp
const MAX_GATEWAY_AGE_DAYS = 60;   // per-gateway staleness SIGNAL (badge), never a build failure
const MAX_BOARD_AGE_DAYS = 180;    // wholesale-stale floor (soft anchor): the whole board older than this = hard fail
// MED-2: base the wholesale-stale floor on the DISPLAYED (matrix-preferring) stamps, not the max across
// all suites. Otherwise a single untouched results/perf/<gw>.json re-run yesterday makes boardNewest =
// yesterday while every DISPLAYED matrix number is 179d old — the 180-day hard-fail (the one absolute
// guard the rewrite KEPT) never fires and a wholesale-stale matrix board publishes generated_at=now.
// The floor now ages exactly what the board shows (same basis as MED-1's footer + the per-row badge).
const boardNewest = Math.max(...gateways.map(displayedMeasuredMs), 0);
if (boardNewest > 0) {
  const boardAgeDays = (Date.parse(generatedAt) - boardNewest) / 86400000;
  if (boardAgeDays > MAX_BOARD_AGE_DAYS) {
    throw new Error(
      `gen-data: FRESHNESS FAILURE (stale board): the newest DISPLAYED measurement anywhere on the board is ${boardAgeDays.toFixed(1)}d old ` +
      `(> ${MAX_BOARD_AGE_DAYS}d) — the WHOLE board is wholesale-stale (nothing displayed has refreshed at all). ` +
      `Refusing to publish generated_at=${generatedAt} over stale data. Re-run the field.`);
  }
}
const nowMs = Date.parse(generatedAt);
for (const g of gateways) {
  g.stale = false;
  // PER-GATEWAY future-date sanity assert (HIGH-3 sibling / NIT): a single gateway's own measured_at
  // must never be in the FUTURE. The board-wide floor above only checks the max; a lone clock-skewed
  // future stamp on one gateway would slip past it and render as a NEGATIVE "measured Nd ago" badge.
  // Skip any future suite stamp so a skewed row can never post a negative age (matrix run is atomic;
  // one bad stamp is corruption, not a legitimate run).
  let sawFuture = false;
  for (const s of SUITES) {
    const at = g[s] && g[s].measured_at;
    if (at && Date.parse(at) > nowMs) {
      sawFuture = true;
      console.warn(`gen-data: WARNING: ${g.key}.${s}.measured_at ${at} is in the FUTURE (> generated_at ${generatedAt}); ` +
        `clock skew on the rig. Skipping this stamp for the freshness/age computation so the badge never reads negative.`);
    }
  }
  // SANITY-ONLY span cap (HIGH-3): the span the cap bounds is ONE atomic matrix run. Restrict the span
  // computation to the MATRIX suite's measured_at ONLY. The retired legacy suites (perf/stream/streamcpu/
  // memory) are fallback-only and are NEVER refreshed by a matrix-only re-run, so they carry weeks-old
  // stamps; folding them into the span made an honest matrix-only re-run (matrix=today, legacy=weeks ago)
  // trip the >12h cap and abort the deploy — defeating incremental publish. The matrix is the single
  // source; only its own timestamps define the run this cap sanity-checks.
  const matrixAt = g.matrix && g.matrix.measured_at && Date.parse(g.matrix.measured_at) <= nowMs
    ? Date.parse(g.matrix.measured_at) : null;
  // The staleness SIGNAL below still considers every suite's newest (non-future) stamp, so a gateway
  // whose ONLY data is a legacy suite still ages correctly; the SPAN cap is what is matrix-scoped.
  const ats = SUITES.map((s) => g[s] && g[s].measured_at).filter(Boolean)
    .map((a) => Date.parse(a)).filter((ms) => ms <= nowMs);
  // NIT-R2-4 / NIT-R5-4 (comment accuracy): actually PERFORM the skip the warn loop promises. g.measured_at
  // was assigned from the future-INCLUSIVE newestMeasuredMs (:227), so a lone skewed-future stamp would
  // otherwise drive a NEGATIVE "measured Nd ago" badge; re-derive the badge stamp from the non-future ats
  // only. HONEST SCOPE: this is DEFENSE-IN-DEPTH, not a live guard. ANY future stamp (matrix OR legacy —
  // the loop at :306 folds every suite into `latest`) already trips the board-wide future-date hard-fail
  // at :343, which THROWS before this line is reached, so no bad badge can ship today. This re-derivation
  // only becomes reachable if that :343 hard-fail is ever weakened/removed; it is retained as a belt-and-
  // suspenders backstop for the per-row badge, NOT because a live path reaches it.
  if (sawFuture) g.measured_at = ats.length ? new Date(Math.max(...ats)).toISOString() : null;
  // LOW-2: a served matrix row whose DISPLAYED numbers project from g.matrix (best_cell / streaming /
  // memory_read / translation_cell, source:"matrix") but that carries NO valid (non-future) matrix
  // measured_at is CORRUPT: run.sh:1145 ALWAYS writes measured_at via `date -u`, so a null/absent matrix
  // stamp is only reachable via truncation / hand-edit / producer bug. Left unguarded such a row bypasses
  // EVERY freshness guard — best_cell still projects and ranks, `latest` (:306) skips it so the future-
  // date hard-fail never sees it, displayedMeasuredMs returns 0 so it is exempt from the 180d floor, and
  // ats.length<1 hits the `continue` below BEFORE g.stale is set, so it publishes FRESH with no badge. A
  // stamp-less served matrix row must never publish clean: flag it stale (drives the app.js badge) and
  // warn, consistent with treating a null matrix stamp as corruption rather than a legitimate reading.
  const matrixProjected = (g.best_cell || g.translation_cell || g.streaming || g.memory_read) &&
    [g.best_cell, g.translation_cell, g.streaming, g.memory_read].some((r) => r && r.source && r.source.kind === "matrix");
  if (g.matrix && matrixProjected && matrixAt == null) {
    console.warn(`gen-data: WARNING: ${g.key} projects displayed numbers from a served matrix but its ` +
      `matrix.measured_at is missing/invalid (=${g.matrix.measured_at}) — run.sh always stamps a matrix, ` +
      `so this is corruption/hand-edit. Flagging the row STALE so it never publishes fresh without a badge.`);
    g.stale = true;
  }
  if (ats.length < 1) continue;
  // NIT-R2-2 / NIT-R5-2: INERT PLACEHOLDER — dead code today, NOT a live safeguard. A matrix row carries
  // at most ONE matrix stamp, so matrixSpanAts is length 0 or 1 and the `>= 2` gate below NEVER fires;
  // it performs no cross-timestamp check on any current data. The live corruption/freshness guards are
  // the board-wide future-date hard-fail (:343) and the 180-day wholesale-stale floor (:376) — NOT this
  // block. It is retained (null-safe, matrix-scoped) purely so the span check reactivates automatically
  // should a future matrix result ever embed multiple internal timestamps; until then it is a no-op.
  const matrixSpanAts = matrixAt != null ? [matrixAt] : [];
  if (matrixSpanAts.length >= 2) {
    const spanH = (Math.max(...matrixSpanAts) - Math.min(...matrixSpanAts)) / 3600000;
    if (spanH > MAX_ROW_SPAN_SANITY_H) {
      throw new Error(
        `gen-data: FRESHNESS FAILURE (corrupt row): ${g.key}'s MATRIX timestamps span ${spanH.toFixed(1)}h (> ${MAX_ROW_SPAN_SANITY_H}h sanity cap) — ` +
        `a corrupt or future-dated timestamp (one atomic matrix run is hours, never this). matrix.measured_at=${g.matrix.measured_at}`);
    }
  }
  // PER-GATEWAY staleness SIGNAL (not a failure): flag a row whose own data has aged past the
  // threshold so app.js can show a "stale" badge. A living board with mixed cadences is fine.
  // LOW-R3-3: age the DISPLAYED numbers — matrix.measured_at when present (best_cell/streaming/memory
  // all project from it), so a stale matrix is flagged even if a newer legacy suite stamp is around;
  // fall back to the newest non-future suite stamp only for a legacy-only row (no matrix).
  const ageBasisMs = matrixAt != null ? matrixAt : Math.max(...ats);
  const ageDays = (nowMs - ageBasisMs) / 86400000;
  g.stale = ageDays > MAX_GATEWAY_AGE_DAYS;
}

// ---- charts: copy results/*.png into site/charts/ ---------------------------
const resultsDir = join(ROOT, "results");
const chartFiles = existsSync(resultsDir)
  // Governance is not a neutral-board metric (the governed suite is a non-default, busbar-only
  // launch), so its chart is excluded from the public gallery even if the PNG is present.
  ? readdirSync(resultsDir).filter((f) => f.endsWith(".png") && !f.includes("governed")).sort()
  : [];
mkdirSync(join(OUT, "charts"), { recursive: true });
const charts = [];
for (const f of chartFiles) {
  const bytes = readFileSync(join(resultsDir, f));
  writeFileSync(join(OUT, "charts", f), bytes);
  // Content-hash cache-buster: the filename is stable across runs, so a browser would
  // serve a stale cached PNG when the chart content changes. Append a short hash of the
  // bytes so the query changes only when the image actually does.
  const v = createHash("sha1").update(bytes).digest("hex").slice(0, 8);
  charts.push({ file: `charts/${f}?v=${v}` });
}

// ---- fonts: copy the repo's bundled Inter faces -----------------------------
const fontsDir = join(ROOT, "assets", "fonts");
if (existsSync(fontsDir)) {
  mkdirSync(join(OUT, "fonts"), { recursive: true });
  for (const f of readdirSync(fontsDir)) copyFileSync(join(fontsDir, f), join(OUT, "fonts", f));
}

// ---- SPA fallback for deep links (/gateways/matrix, ...) --------------------
// The host is Cloudflare Pages, which reads site/_redirects (committed) for the
// /* -> /index.html 200 rewrite so every deep link resolves with a 200 status.
// We deliberately DO NOT emit a 404.html: on CF Pages a 404.html SHADOWS the
// _redirects rewrite (CF serves the 404.html with a 404 status instead of the
// 200-rewrite), which is exactly the deep-link-404 bug. Verified on a preview:
// with 404.html present every /gateways/* is 404; with it removed the same paths
// are 200. GitHub Pages is retired (pages.yml dormant), so the 404.html fallback
// it needed is no longer relevant.
const redirects = join(HERE, "_redirects");
if (existsSync(redirects) && OUT !== HERE) copyFileSync(redirects, join(OUT, "_redirects"));

// ---- emit -------------------------------------------------------------------
const data = {
  category: "gateways", // which category bundle this is (see CATEGORIES in app.js)
  generated_at: generatedAt,
  hardware,
  // MED-1: the DISPLAYED-number freshness stamp (matrix-preferring), not the max across all suites.
  latest_measured_at: latestDisplayed,
  repo: "https://github.com/GetBusbar/benchmarking",
  gateways,
  charts,
};
// C1: strip the raw legacy suite objects from the EMITTED bundle. They were projection INPUTS (their
// values are now sealed into best_cell/translation_cell/streaming), and they carry raw scalars + their
// _mock_bound flags — a reservoir no surface reads any more (charts.py projects from the canonical
// records; app.js reads envelopes). Removing them from the artifact is what makes "no ungated metric
// field exists in the bundle" true. g.matrix stays (its cells are sealed in-place; its top-level
// build/measured_at/p99_ceiling_ms/sweep_dur drive freshness + the sweep-integrity oracle).
for (const g of gateways) {
  for (const suite of ["perf", "stream", "streamcpu", "xlate"]) delete g[suite];
}
writeFileSync(join(OUT, "data.json"), JSON.stringify(data, null, 1) + "\n");
console.log(`gen-data: ${gateways.length} gateways, ${charts.length} charts -> ${join(OUT, "data.json")}`);
