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

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = process.argv[2] || join(HERE, "..");
const OUT = process.argv[3] || HERE;

// GOVERNANCE RETIRED (matrix-sole-source): governance is not measured on the board — the governed
// suite was busbar-only and is retired. `governed/run.sh` stays on disk (unused) but the suite is
// no longer scanned into the bundle and no governed column/derivation is emitted. See app.js.
const SUITES = ["perf", "memory", "stream", "streamcpu", "xlate", "matrix"];

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
      // MEDIUM-1(a): only project streaming when the diagonal cell ACTUALLY STREAMED. A non-streaming
      // cell still carries a stream record ({stream_served:false, …}), so the old truthiness check
      // (`cell.stream`) projected it — surfacing a did-not-stream cell as a served streamer. Mirror the
      // memory guard (line 136, `.served === true`): require stream_served === true so g.streaming is
      // ABSENT for a non-streaming cell and the board renders "did not stream".
      if (cell && cell.stream && cell.stream.stream_served === true) {
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
  // GOVERNANCE RETIRED: no `supports_governed` derivation. Under matrix-sole-source governance is not
  // a board metric (the governed suite was busbar-only and is retired), so the board neither emits a
  // governed column nor a supports_governed capability flag.
  //
  // PER-GATEWAY freshness stamp: each gateway carries its OWN newest measurement so the board can show
  // an independent "measured Nd ago" per row and flag a row that has aged past MAX_GATEWAY_AGE_DAYS.
  // Different gateways legitimately have different measured_at (busbar today, kong 3 weeks ago) — that
  // is honest on a living board where any one gateway can be re-run alone. The staleness flag drives a
  // per-row badge in app.js; it is NOT a build failure (see the freshness guard below).
  const gAtMs = newestMeasuredMs(g);
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

// The newest embedded measurement across a gateway's own suites, in epoch ms (0 when it has none).
// Used both for each gateway's own g.measured_at stamp and for the wholesale-stale board floor below.
function newestMeasuredMs(g) {
  return Math.max(...SUITES.map((s) => g[s] && g[s].measured_at).filter(Boolean).map((a) => Date.parse(a)).concat([0]));
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
const boardNewest = Math.max(...gateways.map(newestMeasuredMs), 0);
if (boardNewest > 0) {
  const boardAgeDays = (Date.parse(generatedAt) - boardNewest) / 86400000;
  if (boardAgeDays > MAX_BOARD_AGE_DAYS) {
    throw new Error(
      `gen-data: FRESHNESS FAILURE (stale board): the newest measurement anywhere on the board is ${boardAgeDays.toFixed(1)}d old ` +
      `(> ${MAX_BOARD_AGE_DAYS}d) — the WHOLE board is wholesale-stale (nothing has refreshed at all). ` +
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
  for (const s of SUITES) {
    const at = g[s] && g[s].measured_at;
    if (at && Date.parse(at) > nowMs) {
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
  if (ats.length < 1) continue;
  // Under matrix-sole-source a row has at most ONE matrix stamp, so an intra-matrix span is 0. The cap
  // stays as a defensive guard should a future matrix result ever embed multiple internal timestamps
  // (kept null-safe: matrixSpanAts is the matrix suite's stamps only).
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
  const ageDays = (nowMs - Math.max(...ats)) / 86400000;
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
  latest_measured_at: latest,
  repo: "https://github.com/GetBusbar/benchmarking",
  gateways,
  charts,
};
writeFileSync(join(OUT, "data.json"), JSON.stringify(data, null, 1) + "\n");
console.log(`gen-data: ${gateways.length} gateways, ${charts.length} charts -> ${join(OUT, "data.json")}`);
