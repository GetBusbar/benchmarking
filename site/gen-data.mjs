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
    // Per-cell perf (matrix v2 + sweep): the gateway's BEST green cell by sustained RPS @20ms,
    // with its ingress -> egress path. The Performance tab ranks each gateway on this cell; the
    // matrix hover shows every other green cell's deviation from it. Absent (older results
    // without per-cell sweeps) the site falls back to the perf suite's single-path number.
    const bc = bestCell(g.matrix);
    if (bc) g.best_cell = bc;
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
// Best green cell across the full grid: max rps_sustained_20ms among served===true cells that
// carry a per-cell perf object. Returns { ingress, egress, ...perf } or null.
function bestCell(m) {
  if (!m.upstreams) return null;
  let best = null;
  for (const [egress, up] of Object.entries(m.upstreams)) {
    for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
      if (cell.served !== true || !cell.perf || cell.perf.rps_sustained_20ms == null) continue;
      if (!best || cell.perf.rps_sustained_20ms > best.rps_sustained_20ms)
        best = { ingress, egress, ...cell.perf };
    }
  }
  return best;
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
// Cloudflare Pages reads site/_redirects (committed) for the /* -> /index.html 200
// rewrite; GitHub Pages has no rewrite support but serves 404.html for unknown
// paths, so a copy of the app shell there makes the same deep links render.
const shell = join(HERE, "index.html");
if (existsSync(shell)) copyFileSync(shell, join(OUT, "404.html"));
const redirects = join(HERE, "_redirects");
if (existsSync(redirects) && OUT !== HERE) copyFileSync(redirects, join(OUT, "_redirects"));

// ---- emit -------------------------------------------------------------------
const data = {
  category: "gateways", // which category bundle this is (see CATEGORIES in app.js)
  generated_at: new Date().toISOString(),
  hardware,
  latest_measured_at: latest,
  repo: "https://github.com/GetBusbar/benchmarking",
  gateways,
  charts,
};
writeFileSync(join(OUT, "data.json"), JSON.stringify(data, null, 1) + "\n");
console.log(`gen-data: ${gateways.length} gateways, ${charts.length} charts -> ${join(OUT, "data.json")}`);
