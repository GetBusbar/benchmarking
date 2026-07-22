#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// gen-data.mjs: build the static data bundle for the results site. No dependencies.
//
// Scans gateways/*/gateway.sh (the self-describing manifests: GW_DISPLAY, GW_LANG, GW_REPO)
// plus results/{perf,memory,stream,xlate,governed,matrix}/<gateway>.json, and emits
// site/data.json. Also copies the generated chart PNGs (results/*.png) into site/charts/
// and the bundled Inter fonts (assets/fonts) into site/fonts/ so the site/ directory is a
// self-contained Pages artifact.
//
//   node site/gen-data.mjs [repoRoot] [outDir]
//
// Defaults: repoRoot = the directory above this script, outDir = this script's directory.
// Absent suites, absent gateways and absent charts are all handled cleanly: the site reads
// whatever this emits and renders "not measured" for the gaps.

import { readdirSync, readFileSync, statSync, existsSync, mkdirSync, copyFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = process.argv[2] || join(HERE, "..");
const OUT = process.argv[3] || HERE;

const SUITES = ["perf", "memory", "stream", "xlate", "governed", "matrix"];

// ---- gateway manifests ------------------------------------------------------
function parseManifest(text) {
  // Values are either quoted ("LiteLLM · Python") or a bare word (Rust); a trailing
  // shell comment may follow either form.
  const get = (name) => {
    const m = text.match(new RegExp(`^${name}=(?:"([^"]*)"|(\\S+))`, "m"));
    return m ? (m[1] ?? m[2]) : null;
  };
  return { display: get("GW_DISPLAY"), lang: get("GW_LANG"), repo: get("GW_REPO") };
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
    repo: meta.repo || null,
  };
  for (const suite of SUITES) {
    const j = readJson(join(ROOT, "results", suite, `${key}.json`));
    if (j) g[suite] = j;
  }
  return g;
});

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
  ? readdirSync(resultsDir).filter((f) => f.endsWith(".png")).sort()
  : [];
mkdirSync(join(OUT, "charts"), { recursive: true });
const charts = [];
for (const f of chartFiles) {
  copyFileSync(join(resultsDir, f), join(OUT, "charts", f));
  charts.push({ file: `charts/${f}` });
}

// ---- fonts: copy the repo's bundled Inter faces -----------------------------
const fontsDir = join(ROOT, "assets", "fonts");
if (existsSync(fontsDir)) {
  mkdirSync(join(OUT, "fonts"), { recursive: true });
  for (const f of readdirSync(fontsDir)) copyFileSync(join(fontsDir, f), join(OUT, "fonts", f));
}

// ---- emit -------------------------------------------------------------------
const data = {
  generated_at: new Date().toISOString(),
  hardware,
  latest_measured_at: latest,
  repo: "https://github.com/GetBusbar/benchmarking",
  gateways,
  charts,
};
writeFileSync(join(OUT, "data.json"), JSON.stringify(data, null, 1) + "\n");
console.log(`gen-data: ${gateways.length} gateways, ${charts.length} charts -> ${join(OUT, "data.json")}`);
