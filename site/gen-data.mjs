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

const gateways = gatewayKeys.map((key) => {
  const meta = parseManifest(readFileSync(join(gatewaysDir, key, "gateway.sh"), "utf8"));
  const g = {
    key,
    display: meta.display || key,
    lang: meta.lang || "Other",
    cls: meta.cls || "Gateway",
    repo: meta.repo || null,
  };
  for (const suite of SUITES) {
    const j = readJson(join(ROOT, "results", suite, `${key}.json`));
    if (j) g[suite] = j;
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
      rps_max_proxy: g.perf.rps_max_proxy,
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
// Loud (non-fatal) warning when a gateway's suite lags the newest raw measurement by a lot: a
// field re-run that skipped a suite leaves mixed-age numbers on one row, which is worth seeing
// in the build log even though it is honest (each lane is stamped with its own measured_at).
const LAG_DAYS = 30;
for (const g of gateways) {
  for (const suite of SUITES) {
    const at = g[suite] && g[suite].measured_at;
    if (!at || !latest) continue;
    const lag = (Date.parse(latest) - Date.parse(at)) / 86400000;
    if (lag > LAG_DAYS) console.warn(
      `gen-data: WARNING: ${g.key}/${suite} measured_at ${at} lags the newest raw measurement (${latest}) by ${Math.round(lag)} days`);
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
