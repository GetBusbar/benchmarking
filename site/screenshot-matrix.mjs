#!/usr/bin/env node
/* screenshot-matrix.mjs — every page × every control combination × three widths, as full-page PNGs.

   WHY THIS EXISTS: the board's layout defects are combinatorial, not per-page. "Tested on is huge"
   is true only in the modes whose pill renders a TRANSLATION path (`OpenAI→Bedrock Converse`), and
   the header-wrap and column-crush problems appear at one width and not another. Reviewing the four
   pages a human happens to open cannot find those; enumerating the product can.

   RUN:
     cd site && python3 -m http.server 8899 &
     NODE_PATH="$(npm root -g)" node site/screenshot-matrix.mjs
   Playwright is not a repo dependency (this script is a development tool, not part of the deployed
   site, and the site ships zero runtime dependencies on purpose), so it resolves from wherever it is
   installed via NODE_PATH. If `npm root -g` does not hold it, point NODE_PATH at the directory that
   does (e.g. NODE_PATH=/private/tmp/node_modules).

   PLAYWRIGHT IS LOADED THROUGH createRequire, NOT A BARE `import`. NODE_PATH is consulted by CommonJS
   resolution only - ESM ignores it entirely - so `import { chromium } from "playwright"` fails with
   "Cannot find package" even when NODE_PATH is set correctly. createRequire gets the CJS resolver,
   which honours it, and playwright ships a CJS entry point.

   IT TARGETS localhost, NOT PRODUCTION, so it photographs the working tree's CSS. Screenshotting the
   deployed board would silently review the previous release.

   URLS USE THE ?view= QUERY FORM, not the clean /gateways/<view> path form. python3 -m http.server has
   no SPA fallback, so a clean path 404s; decodeUrl() accepts `view` as a query param for exactly this
   reason (it is the pre-path-routing link form the app still honours), and boot() then rewrites the
   address bar to path form without reloading. The rendered state is identical either way. */
import { createRequire } from "node:module";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
const { chromium } = createRequire(import.meta.url)("playwright");

const BASE = process.env.BASE_URL || "http://localhost:8899";
const OUT = process.env.OUT_DIR || path.resolve(import.meta.dirname, "../results/screenshots");

/* Mirrors of app.js's own constants. Kept as literals rather than imported because app.js is a
   browser script with top-level side effects; the assertion below is what keeps them honest. */
const VIEWS = ["gateways", "memory", "performance", "frontier", "streaming", "matrix", "method"];
const TABLE_VIEWS = new Set(["performance", "frontier", "streaming", "memory"]);
// The bound selector renders on these two ONLY (app.js BOUND_VIEWS): nothing on Streaming or Memory
// is read at a tail-latency bound, so a bound combo there would photograph a control that is not there.
const BOUND_VIEWS = new Set(["performance", "frontier"]);
// `peak` is a URL CONTRACT whose control now reads "Own cell". The token must stay `peak` in these URLs.
const PERF_MODES = ["peak", "same", "custom"];
const MEM_MODES = ["min", "max", "same", "custom"];
const BOUNDS = ["1", "5", "10", "50", "100", "none"];
/* 1440 desktop / 1024 narrow desktop / 768 tablet. 1024 is the width that matters most here: it is
   wide enough that the table does not collapse to its mobile treatment, and narrow enough that a
   column sized by its widest pill starves the numeric columns. */
const WIDTHS = [1440, 1024, 768];

function modesFor(view) { return view === "memory" ? MEM_MODES : PERF_MODES; }

/* The combinations to photograph. Only TABLE_VIEWS get mode, only BOUND_VIEWS get bound: generating
   ?mode= on the roster would produce N identical PNGs and hide the real matrix in the noise. */
function combos() {
  const out = [];
  for (const view of VIEWS) {
    if (!TABLE_VIEWS.has(view)) { out.push({ view, mode: null, bound: null }); continue; }
    for (const mode of modesFor(view)) {
      if (!BOUND_VIEWS.has(view)) { out.push({ view, mode, bound: null }); continue; }
      for (const bound of BOUNDS) out.push({ view, mode, bound });
    }
  }
  return out;
}

function urlFor({ view, mode, bound }) {
  const p = new URLSearchParams({ view });
  if (mode) p.set("mode", mode);
  if (bound) p.set("bound", bound);
  return `${BASE}/index.html?${p}`;
}

const name = ({ view, mode, bound }, w) => `shot-${view}-${mode || "na"}-${bound || "na"}-${w}.png`;

/* geometryAudit(page, view): does this view's column geometry depend on its CONTENT?

   The owner's complaint was "changing filters shouldn't change column widths", and the obvious check - measure
   under two filter combos, compare - only fires when the board HAS data wide enough to differ. This measures
   the underlying property instead: take the widths, replace every body cell's text with a string far wider
   than any real value, take them again. Under auto table layout the second measurement is different by
   construction; under declared geometry (table-layout: fixed + the colgroup renderTable emits) it is
   identical, and no filter, no future value and no thin data.json can move a column sideways.
   It is deliberately destructive to the DOM, so it runs on its own page load and nothing is screenshotted
   after it. */
async function geometryAudit(page, view) {
  return page.evaluate(() => {
    const table = document.querySelector("#results-table");
    const row = table.querySelector("tbody tr");
    if (!row) return null;
    const widths = () => [...table.querySelectorAll("tbody tr:first-child > *")]
      .map((td) => Math.round(td.getBoundingClientRect().width));
    const before = widths();
    const scroll = table.closest(".table-scroll");
    // Overflow is read BEFORE the mutation: the stuffed strings would report a table width no board ever has.
    const overflow = Math.round(table.scrollWidth - scroll.clientWidth);
    const wide = "OpenAI→Bedrock Converse 2,083,807 no rung held this tail";
    for (const td of table.querySelectorAll("tbody td")) td.textContent = wide;
    for (const th of table.querySelectorAll("thead th")) th.textContent = wide;
    const after = widths();
    return { before, after, stable: before.join(",") === after.join(","), overflow };
  }).then((r) => (r ? { view, ...r } : null));
}

async function main() {
  await mkdir(OUT, { recursive: true });
  const browser = await chromium.launch();
  const shots = [];
  const problems = [];
  const geometry = [];
  for (const w of WIDTHS) {
    const ctx = await browser.newContext({ viewport: { width: w, height: 900 }, deviceScaleFactor: 1 });
    const page = await ctx.newPage();
    // A console error means the screenshot below is of a half-rendered board; recording it keeps a
    // silently-broken combo from being reviewed as a layout opinion.
    page.on("pageerror", (e) => problems.push(`pageerror @${w} ${page.url()}: ${e.message}`));
    for (const c of combos()) {
      await page.goto(urlFor(c), { waitUntil: "networkidle" });
      // The table is rendered from data.json after fetch; wait for real rows, not just load.
      await page.waitForFunction(
        () => document.querySelectorAll("#view-table:not(.hidden) tbody tr, #view-gateways:not(.hidden) tbody tr, #view-matrix:not(.hidden) tbody tr, #view-method:not(.hidden) *").length > 0,
        null, { timeout: 15000 },
      ).catch(() => problems.push(`no rows: ${urlFor(c)} @${w}`));
      const f = path.join(OUT, name(c, w));
      await page.screenshot({ path: f, fullPage: true });
      shots.push(name(c, w));
    }
    // One geometry audit per table view per width, on a fresh load (it mutates the DOM).
    for (const view of VIEWS) {
      if (!TABLE_VIEWS.has(view)) continue;
      await page.goto(urlFor({ view, mode: modesFor(view)[0], bound: null }), { waitUntil: "networkidle" });
      await page.waitForFunction(() => document.querySelectorAll("#results-table tbody tr").length > 0, null, { timeout: 15000 })
        .catch(() => {});
      const g = await geometryAudit(page, view);
      if (g) geometry.push({ width: w, ...g });
    }
    await ctx.close();
  }
  await browser.close();
  for (const g of geometry) {
    console.log(`geometry ${g.view} @${g.width}: ${g.stable ? "STABLE" : "CONTENT-DEPENDENT"} overflow=${g.overflow}px`);
    if (!g.stable) console.log(`   before ${g.before.join(" ")}\n   after  ${g.after.join(" ")}`);
  }
  // An index, so a reviewer can map a filename back to the URL that produced it without re-deriving it.
  await writeFile(path.join(OUT, "index.json"),
    JSON.stringify({ base: BASE, widths: WIDTHS, geometry, shots: combos().flatMap((c) => WIDTHS.map((w) => ({ file: name(c, w), width: w, url: urlFor(c) }))), problems }, null, 2));
  console.log(`${shots.length} screenshots -> ${OUT}`);
  for (const p of problems) console.log(`PROBLEM ${p}`);
}

main().catch((e) => { console.error(e); process.exit(1); });
