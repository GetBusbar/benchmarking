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
// plus results/{perf,memory,stream,streamcpu,xlate,governed,matrix}/<gateway>.json, and emits
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

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = process.argv[2] || join(HERE, "..");
const OUT = process.argv[3] || HERE;

const SUITES = ["perf", "memory", "stream", "streamcpu", "xlate", "governed", "matrix"];

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
  // ---- OOTB config artifact -------------------------------------------------
  // Config transparency: the gateway ran from its as-shipped DEFAULT config (pointed at the mock) and
  // the exact config it used is captured to results/config/<key>.txt (see lib/harness.sh
  // harness_write_config + the perf suite's "ootb_config" pointer). Carry the raw text verbatim into
  // the bundle as g.ootb_config so app.js can render a per-gateway "Config" drawer. Prefer the pointer
  // the result JSON recorded (perf.ootb_config), else fall back to the conventional path; a gateway
  // with no artifact (the not-yet-wired ones) stays absent and the board renders "not published".
  const cfgPointer = (g.perf && typeof g.perf.ootb_config === "string") ? g.perf.ootb_config : `config/${key}.txt`;
  const cfgPath = join(ROOT, "results", cfgPointer);
  if (existsSync(cfgPath)) {
    try { g.ootb_config = readFileSync(cfgPath, "utf8"); } catch { /* unreadable → absent */ }
  }
  if (g.matrix) {
    normalizeMatrix(g.matrix);
    // CANONICAL RULE: the per-cell MATRIX sweep is the single source of truth for all
    // passthrough + translation perf; the standalone perf/xlate suites are FALLBACK ONLY
    // (a gateway with no matrix sweep). g.best_cell / g.translation_cell are the ONE
    // canonical record every surface reads (table, drawer, compare, charts.py); the
    // `source` tag ("matrix" | "perf-fallback" | "xlate-fallback") discloses provenance.
    //
    // Per-cell perf (matrix v2 + sweep): the gateway's BEST green cell by sustained RPS @20ms,
    // with its ingress -> egress path. The Passthrough tab ranks each gateway on this cell; the
    // matrix hover shows every other green cell's deviation from it.
    const bc = bestCell(g.matrix);
    if (bc) g.best_cell = { ...bc, source: "matrix", build: g.matrix.build ?? null, measured_at: g.matrix.measured_at ?? null };
    const tc = translationCell(g.matrix);
    if (tc) g.translation_cell = { ...tc, source: "matrix", build: g.matrix.build ?? null, measured_at: g.matrix.measured_at ?? null };
    // STREAMING projection (matrix is the single source): the streaming shown on the board is the
    // BEST DIAGONAL cell's streaming — the same (ingress==egress) passthrough cell the headline perf
    // is projected from, so the streaming numbers are read off the SAME cell as the RPS/latency
    // headline (one source of truth; check-consistency asserts headline streaming == this cell's).
    // g.streaming carries the diagonal cell's dialect + its full stream record.
    if (bc) {
      const cell = g.matrix.upstreams?.[bc.dialect]?.cells?.[bc.dialect];
      if (cell && cell.stream) {
        g.streaming = { dialect: bc.dialect, source: "matrix",
          build: g.matrix.build ?? null, measured_at: g.matrix.measured_at ?? null, ...cell.stream };
      }
    }
    // MEMORY projection (matrix is the single source): the one process-level RSS read the matrix run
    // takes (matrix.memory). A gateway whose matrix run served no memory (or ran with MATRIX_MEMORY=0)
    // leaves g.memory_read absent and the board falls back to the legacy memory suite below.
    if (g.matrix.memory && g.matrix.memory.served === true) {
      g.memory_read = { source: "matrix", build: g.matrix.build ?? null,
        measured_at: g.matrix.measured_at ?? null, ...g.matrix.memory };
    }
  }
  // LEGACY FALLBACK — old bundles only. Before the matrix folded streaming + memory in, they came from
  // the standalone stream/streamcpu/memory suites. Keep those as a fallback so an OLD bundle (matrix
  // with no per-cell stream / no matrix.memory) still renders. A fresh matrix bundle sets g.streaming /
  // g.memory_read above and these no-ops. Clearly a legacy path — the matrix is the primary source.
  if (!g.streaming && g.stream && g.stream.stream_served === true) {
    // The old stream suite measured the gateway's default passthrough; label it with that dialect so
    // the pill and numbers name the same path. streamcpu (if present) supplies the cpu-fps.
    const dia = passthroughDialect(g.matrix);
    g.streaming = {
      dialect: dia, source: "stream-fallback",
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
      build: g.stream.build ?? null, measured_at: g.stream.measured_at ?? null,
    };
  }
  if (!g.memory_read && g.memory && g.memory.served === true) {
    g.memory_read = { source: "memory-fallback", ...g.memory };
  }
  if (!g.best_cell && g.perf && g.perf.served === true && g.perf.added_latency_p99_us != null) {
    // No swept diagonal (e.g. bifrost mid-re-run), but the perf suite ran the gateway's default
    // passthrough. Synthesize best_cell from it, inferring the dialect from the served diagonal, so
    // the Tested-on pill and the numbers below it always name the SAME dialect (never a metric with
    // an n/a pill). The field re-run replaces this with a real swept cell. source:"perf-fallback"
    // makes the provenance visible on every surface (pill tooltip, drawer, charts).
    const dia = passthroughDialect(g.matrix);
    g.best_cell = {
      ingress: dia, egress: dia, dialect: dia, source: "perf-fallback",
      added_latency_p50_us: g.perf.added_latency_p50_us,
      added_latency_p99_us: g.perf.added_latency_p99_us,
      rps_sustained_20ms: g.perf.rps_sustained_20ms,
      rps_sustained_20ms_concurrency: g.perf.rps_sustained_20ms_concurrency ?? null,
      rps_max_proxy: g.perf.rps_max_proxy,
      rps_max_proxy_concurrency: g.perf.rps_max_proxy_concurrency ?? null,
      // The charted sweep arrays travel WITH the headline so the drawer curve and the headline are
      // read off the SAME record (best_cell): the marked peak on the curve IS rps_max_proxy /
      // rps_sustained_20ms. The perf suite emits the same array shape run_sweep produced.
      sweep_max_proxy: g.perf.sweep_max_proxy ?? null,
      sweep_sustained_20ms: g.perf.sweep_sustained_20ms ?? null,
      build: g.perf.build ?? null, measured_at: g.perf.measured_at ?? null,
    };
  }
  if (!g.translation_cell && g.xlate && g.xlate.xlate_served === true && g.xlate.xlate_added_latency_p99_us != null) {
    // No measured openai-in matrix translation cell, but the legacy xlate suite ran. Its direction
    // is the OPPOSITE of the matrix cell (anthropic in -> openai out), so it is normalized into the
    // same canonical shape WITH its real direction and an explicit source tag; every surface labels
    // the direction from these fields, so the two paths can never be confused.
    g.translation_cell = {
      ingress: "anthropic", egress: "openai", source: "xlate-fallback",
      added_latency_p50_us: g.xlate.xlate_added_latency_p50_us,
      added_latency_p99_us: g.xlate.xlate_added_latency_p99_us,
      rps_sustained_20ms: g.xlate.xlate_rps_sustained_20ms,
      build: g.xlate.build ?? null, measured_at: g.xlate.measured_at ?? null,
    };
  }
  // "Supports governance" is a DECLARED CAPABILITY, not a per-run measurement outcome. A gateway is
  // governance-capable if its manifest wires a governed launch - i.e. the governed note is anything
  // OTHER than "manifest defines no ..." (the string write_unserved emits when no gw_governed_launch
  // hook exists). So a capable gateway whose measurement failed on a given run (e.g. busbar's launch
  // hiccup) still reads as capable; only genuinely-unsupported gateways are excluded from the filter.
  const gnote = (g.governed && typeof g.governed.governed_note === "string") ? g.governed.governed_note : "";
  g.supports_governed = !!g.governed && !gnote.includes("manifest defines no");
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
    if (j.measured_at && (!latest || j.measured_at > latest)) latest = j.measured_at;
  }
}
const hardware = [...hwCounts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] || null;

// ---- staleness guard (R2) ---------------------------------------------------
// The bundle is regenerated from the raw results tree on every run, so generated_at must never
// precede the newest embedded measurement: if it does, a raw result is future-dated (clock skew
// on the rig) and a "fresh" bundle would look older than its data. Hard fail; never ship it.
const generatedAt = new Date().toISOString();
if (latest && generatedAt < latest) {
  throw new Error(`gen-data: generated_at ${generatedAt} predates the newest embedded measured_at ${latest}; ` +
    `a raw result is future-dated (rig clock skew?). Refusing to emit a bundle that would read stale.`);
}
// FRESHNESS GUARD (trust): a full field run puts every one of a gateway's suites on ONE box,
// sequentially. Most gateways cluster tightly (their whole run is < ~1h), but the MATRIX suite is
// the outlier: for a gateway that does real cross-protocol translation on every egress cell it runs
// ~2.3h (busbar), vs 17-45 min for passthrough gateways. So a LEGITIMATE single clean busbar box run
// spans ~2.6h (fast suites 18:32-18:48, matrix 21:08) - that is NOT a franken-mix. MAX_SPAN_H must
// therefore clear the slowest gateway's real single-run span, or busbar can never publish (it did
// exactly this: MAX_SPAN_H=2 hard-failed every clean busbar run because its matrix alone is > 2h).
// Set to 3h: still well under a genuine franken-mix (the busbar-streaming bug shipped a `stream`
// suite from a run HOURS/days older than the rest - a span far past 3h) and still backstopped by the
// LAG guard below, but no longer a false-positive on busbar's inherently long matrix.
// FOLLOW-UP: parallelize the matrix suite (harness task) so even busbar's run is < 1h, then this can
// drop back toward the fast-gateway cluster width; until then 3h is the honest floor.
// Newest suite timestamp per gateway, and the board-wide newest. A single field run launches all
// boxes together and they finish within a few hours of each other, so a gateway whose newest suite
// lags the board-wide newest by more than MAX_LAG_H did NOT refresh in the latest run (its box
// failed, or the rsync pull dropped and promote_guard kept old data): its whole row is stale,
// self-consistent but old. That is the SECOND way a refresh betrays trust (a whole stale row).
const MAX_SPAN_H = 3;   // a gateway's own suites must be from one box run; 3h clears busbar's matrix
const MAX_LAG_H = 3;    // no gateway may lag the board-wide newest measurement by more than this
// ABSOLUTE board-age floor (trust anchor). The two guards above are RELATIVE - span is row-internal,
// lag is row-vs-boardNewest - so a WHOLESALE-stale board (every box failed to refresh and
// promote_guard republished the SAME old timestamps for every gateway) sails through: each row's
// span is tiny and every row's lag against boardNewest is ~0. Nothing relative can see that the whole
// board is old. This absolute floor is the honesty backstop: if the newest measurement ANYWHERE on
// the board is older than MAX_BOARD_AGE_H, the board is stale as a whole and must NOT publish
// generated_at=now over week-old data. Board timestamps are the anchor; a stale board is a hard fail.
const MAX_BOARD_AGE_H = 48;
const newestOf = (g) => Math.max(...SUITES.map((s) => g[s] && g[s].measured_at).filter(Boolean).map((a) => Date.parse(a)).concat([0]));
const boardNewest = Math.max(...gateways.map(newestOf), 0);
if (boardNewest > 0) {
  const boardAgeH = (Date.parse(generatedAt) - boardNewest) / 3600000;
  if (boardAgeH > MAX_BOARD_AGE_H) {
    throw new Error(
      `gen-data: FRESHNESS FAILURE (stale board): the newest measurement anywhere on the board is ${boardAgeH.toFixed(1)}h old ` +
      `(> ${MAX_BOARD_AGE_H}h) - the WHOLE board is wholesale-stale (every box failed to refresh and old timestamps were republished). ` +
      `Refusing to publish generated_at=${generatedAt} over stale data. Re-run the field.`);
  }
}
for (const g of gateways) {
  const ats = SUITES.map((s) => g[s] && g[s].measured_at).filter(Boolean).map((a) => Date.parse(a));
  if (ats.length < 1) continue;
  const per = () => SUITES.filter((s) => g[s] && g[s].measured_at).map((s) => `${s}=${g[s].measured_at}`).join(", ");
  // The span (mixed-row) check needs >=2 suites; the lag (stale-row) check needs only ONE timestamp,
  // so it must ALSO apply to single-suite gateways - a box that failed partway (one suite on disk)
  // from a run days ago would otherwise ship stale to the board (audit R2-L3).
  if (ats.length >= 2) {
    const spanH = (Math.max(...ats) - Math.min(...ats)) / 3600000;
    if (spanH > MAX_SPAN_H) {
      throw new Error(
        `gen-data: FRESHNESS FAILURE (mixed row): ${g.key}'s suites span ${spanH.toFixed(1)}h (> ${MAX_SPAN_H}h), so its row mixes ` +
        `different runs (a stale suite survived a refresh). Re-run ${g.key} in one clean pass. Per-suite: ${per()}`);
    }
  }
  const lagH = (boardNewest - Math.max(...ats)) / 3600000;
  if (lagH > MAX_LAG_H) {
    throw new Error(
      `gen-data: FRESHNESS FAILURE (stale row): ${g.key}'s data lags the newest board measurement by ${lagH.toFixed(1)}h (> ${MAX_LAG_H}h) ` +
      `- it did NOT refresh in the latest run (box failed or the pull dropped and old data was kept). Re-run ${g.key}. Per-suite: ${per()}`);
  }
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
  latest_measured_at: latest,
  repo: "https://github.com/GetBusbar/benchmarking",
  gateways,
  charts,
};
writeFileSync(join(OUT, "data.json"), JSON.stringify(data, null, 1) + "\n");
console.log(`gen-data: ${gateways.length} gateways, ${charts.length} charts -> ${join(OUT, "data.json")}`);
