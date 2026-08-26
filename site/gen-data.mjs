#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// gen-data.mjs: build the static data bundle for the results site. No dependencies.
//
// The site (onthebench.ai) is category-based; this emits the GATEWAYS category bundle
// (site/data.json). CATEGORY SEAM: a second category (e.g. models) should get its own bundle under
// site/data/<category>.json, registered in CATEGORIES in app.js - the emitted `category` field names
// which bundle this is.
//
// Scans gateways/*/definition.json (display/lang/class/repo) plus
// results/{perf,memory,stream,streamcpu,xlate,matrix}/<gateway>.json, and emits site/data.json. Also
// copies results/*.png into site/charts/ and assets/fonts into site/fonts/ so site/ is a
// self-contained Pages artifact, and writes 404.html so hosts without _redirects support (GitHub
// Pages) still deep-link into /gateways/<view> paths.
//
//   node site/gen-data.mjs [repoRoot] [outDir]
//
// Defaults: repoRoot = the directory above this script, outDir = this script's directory.
// Absent suites, gateways and charts are handled cleanly: the site renders "not measured" for gaps.

import { readdirSync, readFileSync, statSync, existsSync, mkdirSync, copyFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { snapshotCellCoords, isStrictSubset, layerScopedMatrix } from "./snapshots.mjs";
import { instrumentOf } from "./check-consistency.mjs";
import { sealMetric, sealFrontier, sealRungs, makeSource, SWEEP, UNGATED_LAT_FIELDS, UNGATED_COST_FIELDS, DEFAULT_BOUND_MS, frontierAt, UNGATED_STREAM_FIELDS, isMetricField, zeroNoteFor } from "./seal.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = process.argv[2] || join(HERE, "..");
const OUT = process.argv[3] || HERE;

// GOVERNANCE RETIRED (matrix-sole-source): the governed suite was busbar-only and is retired, so
// governance is not scanned or measured on the board (`governed/run.sh` stays on disk, unused). See app.js.
// "memory" is intentionally NOT scanned: the retired standalone memory suite wrote synthetic burst
// numbers mislabelled as 6x6 provenance. Memory now comes SOLELY from the matrix's PER-CELL windows
// (matrix.upstreams[egress].cells[ingress].memory, sealed below) - no fallback, no per-gateway scalar,
// since there is no single cell to project one from without the harness silently SELECTING one.
const SUITES = ["perf", "stream", "streamcpu", "xlate", "matrix"];
// The ungated (non-honesty-gated) latency-shaped metrics on a perf cell: always certified when present.
// Imported from seal.mjs - the ONE vocabulary check-consistency also imports, so the two can never lag
// each other. RSS fields are sealed BY DISCOVERY (RSS_FIELD_RE), never from a whitelist (audit #11).
const UNGATED_LAT = UNGATED_LAT_FIELDS;

// us-east-1 on-demand price for the 4-core slice the gateway under test is pinned to, disclosed here and
// rendered in every dollar caption so a reader on different pricing can rescale. Override with
// GATEWAY_HOURLY_USD. Moved here from the retired charts.py pipeline: the derivation is the board's, not
// a chart's.
const GATEWAY_HOURLY_USD = Number(process.env.GATEWAY_HOURLY_USD || 0.1632);

/* costLanes(cell): req/s per $/hr and $ per 1M requests, from the frontier reading AT THE DEFAULT BOUND,
   so the dollar figures and the throughput column describe the SAME operating point. Cost inherits
   whatever qualification the rate carried, so the bound is named on every surface that shows it.

   `cost_per_million` is ABSENT at rate 0, not 0: at zero the quotient is undefined, and 0 would be the
   CHEAPEST (best) value on a lower-is-better axis, making "held nothing" look free. `rps_per_dollar`
   keeps its 0 because zero requests per dollar genuinely is zero - 0 is a number, n/a is not. */
function costLanes(cell) {
  const r = frontierAt((cell || {}).frontier, DEFAULT_BOUND_MS);
  /* AN ABSENT RATE IS NOT A ZERO RATE. `rate` used to collapse "no measurement" into the same 0 as
     "measured zero" and pass it through `sealMetric` with no absence reason, emerging CERTIFIED - on
     the 2026-07-31 board three gateways with `rps.reason = "below_resolution"` published
     `rps_per_dollar: {value: 0, certified: true}`, asserting as measurement that they deliver zero
     requests per dollar. Neither lane was in seal.mjs's vocabulary, so check-consistency never caught
     it. Both lanes now carry the RATE's own reason/detail instead of a hardcoded `not_measured`. */
  const rateEnv = r && r.rps;
  const rate = rateEnv && typeof rateEnv.value === "number" ? rateEnv.value : null;
  const rateAbsent = rate == null
    ? {
        reason: (rateEnv && rateEnv.reason) || "not_measured",
        detail: (rateEnv && rateEnv.detail)
          || `no reading resolved at the ${DEFAULT_BOUND_MS} ms bound, so this lane is undefined rather than zero`,
      }
    : null;
  return {
    rps_per_dollar: sealMetric(rate == null ? null : rate / GATEWAY_HOURLY_USD, { absent: rateAbsent }),
    cost_per_million_usd: sealMetric(
      rate > 0 ? (GATEWAY_HOURLY_USD / (rate * 3600)) * 1e6 : null,
      {
        absent: rateAbsent
          || (rate === 0
            ? { reason: "not_applicable", detail: "the gateway carried zero throughput at this bound, so cost per request is undefined rather than infinite" }
            : null),
      },
    ),
    priced_at_bound_ms: DEFAULT_BOUND_MS,
    gateway_hourly_usd: GATEWAY_HOURLY_USD,
  };
}

// ---- gateway manifests ------------------------------------------------------
// The version a manifest pins, as a short human string: image tag for a container gateway, short
// commit for one built from source. `null` when the manifest pins neither (renders as nothing, not a guess).
function declaredVersion(d) {
  const img = d?.launch?.image;
  if (typeof img === "string" && img.includes(":")) {
    const tag = img.slice(img.lastIndexOf(":") + 1);
    if (tag && tag !== "latest") return tag;
  }
  const commit = d?.launch?.commit;
  if (typeof commit === "string" && commit.length >= 7) return commit.slice(0, 7);
  return null;
}

// A gateway built from source pins its ref in its own build.sh (`COMMIT="${SOME_COMMIT:-<sha>}"`,
// optionally with a `# tag vX.Y.Z`), since there's no image to name a version off of.
function builtFromSourceVersion(key) {
  const path = join(gatewaysDir, key, "build.sh");
  if (!existsSync(path)) return null;
  let text = "";
  try { text = readFileSync(path, "utf8"); } catch { return null; }
  const line = text.split("\n").find((l) => /^\s*COMMIT=/.test(l));
  if (!line) return null;
  const tag = line.match(/#\s*tag\s+(\S+)/);
  if (tag) return tag[1];
  const sha = line.match(/:-([0-9a-f]{7,40})\}/i);
  return sha ? sha[1].slice(0, 7) : null;
}

function parseManifest(text) {
  // ONE manifest per gateway - the one the engine runs from (definition.json), not a shell file the
  // engine no longer reads. `cls` is each project's OWN self-description (its README/tagline), never
  // our editorial classification; missing/unknown falls back to the neutral "Gateway".
  let d = {};
  try { d = JSON.parse(text); } catch { return { display: null, lang: null, repo: null, cls: null, version: null }; }
  return {
    display: d.display ?? null,
    lang: d.lang ?? null,
    repo: d.repo ?? null,
    cls: d.class ?? null,
    // The declared pin, known without having measured anything - so a gateway awaiting its first run
    // still shows WHAT would be measured rather than an empty row. A row WITH numbers is instead
    // authoritatively versioned by `build`, the version the engine actually built for that run.
    version: declaredVersion(d),
  };
}

const gatewaysDir = join(ROOT, "gateways");
const gatewayKeys = existsSync(gatewaysDir)
  ? readdirSync(gatewaysDir).filter((d) => {
      try {
        return statSync(join(gatewaysDir, d)).isDirectory() && existsSync(join(gatewaysDir, d, "definition.json"));
      } catch { return false; }
    }).sort()
  : [];

// snapshotDegradedMode(m): the phases this run was told NOT to measure, as a human string, or "" when it
// ran everything. Reads the producer's own mode flags (cell_perf_sweep / cell_stream / cell_memory) - how
// the run was CONFIGURED, never how it turned out. An absent flag (predates it) is treated as ON.
function snapshotDegradedMode(m) {
  if (!m || typeof m !== "object") return "";
  const off = ["cell_perf_sweep", "cell_stream", "cell_memory"].filter((k) => m[k] === false);
  return off.length ? off.join("=false, ") + "=false" : "";
}

function readJson(path) {
  try { return JSON.parse(readFileSync(path, "utf8")); } catch { return null; }
}

// newestSnapshot(key): the newest results/snapshots/result_<key>_<ts>.json by its own measured_at.
// Returns the parsed snapshot or null (no snapshot yet).
const SNAP_DIR = join(ROOT, "results", "snapshots");
function newestSnapshot(key) {
  if (!existsSync(SNAP_DIR)) return null;
  let best = null, bestMs = -1;
  for (const f of readdirSync(SNAP_DIR)) {
    if (!f.startsWith(`result_${key}_`) || !f.endsWith(".json")) continue;
    const snap = readJson(join(SNAP_DIR, f));
    if (!snap) continue;
    const ms = snap.measured_at ? Date.parse(snap.measured_at) : 0;
    if (ms > bestMs) { bestMs = ms; best = snap; best.__file = f; }
  }
  return best;
}

/* The snapshot the board should render for `key`: the newest FULL run, with every strictly-scoped
   newer run layered over it oldest-first (so the newest measurement of any cell wins). */
function resolvedSnapshot(key) {
  if (!existsSync(SNAP_DIR)) return null;
  const snaps = [];
  for (const f of readdirSync(SNAP_DIR)) {
    if (!f.startsWith(`result_${key}_`) || !f.endsWith(".json")) continue;
    const snap = readJson(join(SNAP_DIR, f));
    if (!snap) continue;
    snap.__file = f;
    snap.__ms = snap.measured_at ? Date.parse(snap.measured_at) : 0;
    snap.__coords = snapshotCellCoords(snap.matrix);
    snaps.push(snap);
  }
  if (!snaps.length) return null;
  snaps.sort((a, b) => a.__ms - b.__ms);
  const widest = snaps.reduce((m, s) => (s.__coords.size > m.__coords.size ? s : m), snaps[0]);
  // The base is the newest run that is NOT a strict subset of the widest - i.e. a full run.
  const fulls = snaps.filter((s) => !isStrictSubset(s.__coords, widest.__coords));
  const base = fulls[fulls.length - 1];
  // Subset of the BASE, not of the widest: subset-ness only means anything relative to the run being
  // layered onto. Testing against `widest` misclassified a re-run touching a coordinate the widest run
  // lacked (e.g. a new dialect) as a "full" run by exclusion, which became the base by recency and
  // silently deleted every other measured cell.
  const scopedNewer = snaps.filter((s) => isStrictSubset(s.__coords, base.__coords) && s.__ms >= base.__ms);
  if (!scopedNewer.length) return base;
  const out = structuredClone(base);
  out.__file = base.__file;
  let m = base.matrix;
  for (const sc of scopedNewer) {
    // Screen each scoped run on ITS OWN mode flags, not the layered result's (which carries the base
    // run's flags after `layerScopedMatrix` copies in only cells) - otherwise a degraded smoke run
    // could replace a clean field run's cells while the guard read the base's flags and stayed silent.
    const degraded = snapshotDegradedMode(sc.matrix);
    if (degraded) {
      throw new Error(
        `gen-data: REFUSING to layer a DEGRADED-MODE re-run into ${key}. ${sc.__file} ` +
        `(measured_at ${sc.measured_at}) ran with ${degraded} - those phases were switched OFF, so it ` +
        `is a local smoke run, not a measurement, and its cells would replace measured ones.\n` +
        `  Fix: delete or move that snapshot out of results/snapshots/.`);
    }
    m = layerScopedMatrix(m, sc.matrix, sc);
  }
  out.matrix = m;
  out.__layered_from = scopedNewer.map((s) => s.__file);
  return out;
}

// GitHub star snapshot for the Gateways overview: a COMMITTED build-time file (gateways/stars.json,
// refreshed by `node gateways/fetch-stars.mjs`), never a live API call, so the bundle stays reproducible
// and CI needs no network. Absent file or key degrades to null; the site renders those muted.
const starsSnap = readJson(join(gatewaysDir, "stars.json")) || {};

// OUTSIDE CONTRIBUTORS, keyed by gateway. Kept in a SITE-ONLY file the engine never parses, since the
// engine is a frozen instrument (ENGINE_PIN) and adding a contributor field to definition.json would
// force a board-wide re-run for a purely editorial credit.
// Shape: { "<gatewayKey>": [ { "handle": "...", "name": "...", "url": "https://..." }, ... ] }.
const contributorsBook = readJson(join(HERE, "contributors.json")) || {};

// snapshotDefinitionsByKey: gateway key -> the metric-definitions map off the SAME snapshot that became
// that gateway's g.matrix (captured below). Kept out of the gateway object itself - definitions are a
// board-level projection (data.definitions), not a per-row field - as a private side table never
// emitted raw.
const snapshotDefinitionsByKey = new Map();
const allGateways = gatewayKeys.map((key) => {
  const meta = parseManifest(readFileSync(join(gatewaysDir, key, "definition.json"), "utf8"));
  const g = {
    key,
    display: meta.display || key,
    lang: meta.lang || "Other",
    cls: meta.cls || "Gateway",
    // Sanitised the same way g.repo is: app.js interpolates url RAW into an href, so only https://
    // profile URLs survive - a bad/missing url drops to null and renders as plain text.
    contributed_by: (Array.isArray(contributorsBook[key]) ? contributorsBook[key] : [])
      .filter((c) => c && typeof c.handle === "string" && c.handle.trim())
      .map((c) => ({
        handle: c.handle,
        name: (typeof c.name === "string" && c.name.trim()) ? c.name : c.handle,
        url: (typeof c.url === "string" && /^https:\/\/[^\s"'<>]+$/.test(c.url)) ? c.url : null,
      })),
    // Only accept an https:// repo URL: app.js interpolates g.repo RAW into href="${g.repo}" (unescaped),
    // so an unvalidated manifest field could inject a `javascript:` scheme onto the public board (audit R2-L2).
    repo: (typeof meta.repo === "string" && /^https:\/\/[^\s"'<>]+$/.test(meta.repo)) ? meta.repo : null,
    // The pinned version, present whether or not this gateway has ever been measured.
    version: meta.version ?? builtFromSourceVersion(key),
    stars: starsSnap[key]?.stars ?? null,
    stars_as_of: starsSnap[key]?.as_of ?? null,
    // The repo's FIRST-commit date, not created_at (which resets on renames): 43k stars over 10 years
    // and 100 over 3 weeks are different statements.
    first_commit: starsSnap[key]?.first_commit ?? null,
  };
  for (const suite of SUITES) {
    const j = readJson(join(ROOT, "results", suite, `${key}.json`));
    if (j) g[suite] = j;
  }
  // ---- snapshot artifact (task #65): the SINGLE self-describing per-gateway run ------------------
  // Prefer the NEWEST snapshot (results/snapshots/result_<key>_<measured_at>.json) as the source of the
  // matrix + config: config lives INSIDE the same file as the numbers it produced, killing the
  // config-drift class. A gateway with no snapshot yet keeps the per-suite read path above.
  const snap = resolvedSnapshot(key);
  if (snap) {
    // RECENCY, not existence (audit #5): an older snapshot must never shadow a newer
    // results/matrix/<gw>.json (a matrix-only re-run can leave the snapshot archive trailing it).
    const snapAt = snap.matrix && snap.measured_at ? Date.parse(snap.measured_at) : NaN;
    const diskAt = g.matrix && g.matrix.measured_at ? Date.parse(g.matrix.measured_at) : NaN;
    const snapMs = Number.isFinite(snapAt) ? snapAt : -1;
    const diskMs = Number.isFinite(diskAt) ? diskAt : -1;
    if (snap.matrix && (!g.matrix || snapMs >= diskMs)) {
      // A degraded-mode run (a local smoke run with cell_perf_sweep/cell_stream/cell_memory turned off
      // to finish in minutes) must never become the board's source just by being newer - it was TOLD
      // NOT TO MEASURE, so refusing it is a statement about the run's MODE, not its numbers. This
      // applies even when there's no fuller file to shadow (a gateway whose only artifact is a smoke
      // run): a run that wasn't told to measure still isn't a measurement.
      const degraded = snapshotDegradedMode(snap.matrix);
      if (degraded) {
        const diskFull = g.matrix && !snapshotDegradedMode(g.matrix);
        const shadows = diskFull
          ? `yet it is NEWER than results/matrix/${key}.json (measured_at ${g.matrix.measured_at}), which ran them ` +
            `all. Publishing it would replace a complete run with a probe-only one`
          : `and it is the ONLY matrix artifact this gateway has, so nothing else would be replaced - the board ` +
            `would simply carry a probe-only run`;
        throw new Error(
          `gen-data: REFUSING to publish ${key} from a DEGRADED-MODE snapshot. ${snap.__file} ` +
          `(measured_at ${snap.measured_at}) ran with ${degraded} - the phases were switched OFF, so it is a ` +
          `local smoke run, not a measurement - ${shadows} and the board would show it as this gateway's result.\n` +
          `  Fix: delete or move that snapshot out of results/snapshots/ (a local verify-local run with ` +
          `KEEP_ARTIFACTS=1 leaves it behind; without KEEP_ARTIFACTS the teardown's git clean removes it).`);
      }
      g.matrix = snap.matrix;                                      // matrix from the snapshot (sole source)
      g.matrix_from_snapshot = true;
      g.snapshot_file = snap.__file ?? null;                       // which archived run the board renders
      // METRIC DEFINITIONS (task: project data.definitions): carried on the SAME snapshot that just
      // became this row's matrix, never read independently of it. The definitions are generated from
      // the engine's own constants (engine/src/suite.rs metric_definitions), so a definitions map is
      // only honest paired with the numbers the SAME engine produced - reading it off a snapshot that
      // did NOT become g.matrix (the disk-matrix-newer branch above) would risk publishing prose from
      // one engine beside numbers from another, exactly the class of lie definitions exist to prevent.
      // Older snapshots predate the field entirely; absent here just means absent (handled below).
      if (snap.definitions && typeof snap.definitions === "object") snapshotDefinitionsByKey.set(key, snap.definitions);
    }
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
    // The pointer comes from the MATRIX (the sole source) when it carries one; the retired perf suite is
    // no longer consulted (audit #12 - g.perf is a fallback-only input that the emit step deletes).
    // THE POINTER IS PRODUCER-SUPPLIED, SO IT IS UNTRUSTED. It arrives inside a results JSON and its
    // contents are published verbatim onto a public page, so a `..` in it reads an arbitrary local file
    // straight onto the board - a gateway manifest, a harness script, or anything else readable by the
    // process that runs gen-data. Nothing malicious is needed for this to bite: a producer bug that
    // wrote an absolute or relative path would exfiltrate a file rather than fail. Allowlist the exact
    // shape the harness writes (lib/harness.sh harness_write_config emits config/<key>.txt) and refuse
    // anything else loudly, rather than silently falling back and hiding a producer that has gone wrong.
    const CFG_POINTER_RE = /^config\/[A-Za-z0-9._-]+\.txt$/;
    const rawPointer = (g.matrix && typeof g.matrix.ootb_config === "string") ? g.matrix.ootb_config : null;
    if (rawPointer && !CFG_POINTER_RE.test(rawPointer)) {
      throw new Error(`gen-data: ${key}.matrix.ootb_config = ${JSON.stringify(rawPointer)} is not a ` +
        `results-relative config artifact (expected config/<name>.txt). A pointer that escapes results/ ` +
        `would read an arbitrary local file onto the public board. Refusing to build the bundle.`);
    }
    const cfgPointer = rawPointer || `config/${key}.txt`;
    const cfgPath = join(ROOT, "results", cfgPointer);
    if (existsSync(cfgPath)) {
      try { g.ootb_config = readFileSync(cfgPath, "utf8"); } catch { /* unreadable → absent */ }
    }
  }
  if (g.matrix) {
    normalizeMatrix(g.matrix);
    // CANONICAL RULE: the per-cell MATRIX sweep is the single source of truth for passthrough +
    // translation perf; the standalone perf/xlate suites are a live deferred fallback. g.best_cell /
    // g.translation_cell are the ONE canonical record every surface reads. Every metric is SEALED here
    // (seal.mjs): the raw scalar is consumed, never re-emitted, and the cell's `source` stamp drives
    // every caption. See seal.mjs / Design E.
    const build = g.matrix.build ?? null, at = g.matrix.measured_at ?? null;
    // Per-cell perf (matrix v2 + sweep): the gateway's OPENAI diagonal when it serves one, else the
    // lowest-added-p99 diagonal it does serve - NOT "best by sustained RPS @20ms". Comparing every
    // gateway on the SAME cell is fairer than each on whichever cell flatters it most, even though for
    // some gateways their openai diagonal isn't their fastest one.
    const bc = bestCell(g.matrix);
    if (bc) g.best_cell = sealPerfCell(bc, { ingress: bc.dialect, egress: bc.dialect, dialect: bc.dialect },
      makeSource("matrix", SWEEP.DIAGONAL, build, at), bc.absences);
    // The dollar lanes ride on the canonical cell's frontier reading at the default bound - the same
    // operating point the throughput column ranks - so the two can never describe different runs.
    if (g.best_cell) Object.assign(g.best_cell, costLanes(g.best_cell));
    // The gateway's TRANSLATION cell (openai in -> best non-openai egress).
    const tc = translationCell(g.matrix);
    if (tc) g.translation_cell = sealPerfCell(tc, { ingress: tc.ingress, egress: tc.egress },
      makeSource("matrix", SWEEP.TRANSLATION, build, at), tc.absences);
    // STREAMING projection: the BEST DIAGONAL cell's streaming (same cell the headline perf projects
    // from), only when it actually streamed (stream_served===true); otherwise g.streaming stays absent.
    if (bc) {
      const cell = g.matrix.upstreams?.[bc.dialect]?.cells?.[bc.dialect];
      if (cell && cell.stream && cell.stream.stream_served === true) {
        g.streaming = sealStreaming(cell.stream, bc.dialect,
          makeSource("matrix", SWEEP.STREAM_DIAGONAL, build, at), cell.absences);
      }
    }
    // MEMORY: NOT projected to a per-gateway scalar. It stays per cell all the way to the reader; the
    // memory lane picks a cell through the same chooser (Min/Max/Same/Custom) every other lane uses,
    // rather than the harness silently selecting one. Windows are sealed in place below.
    // Seal every matrix cell in-place (AFTER selection/projection, which read raw): the matrix popup +
    // Protocol view read cell.perf/cell.stream directly, so those must be envelopes too (invariant C1).
    sealMatrixCellsInPlace(g.matrix);
  }
  // LIVE DEFERRED FALLBACKS (stay until the field run folds them into the matrix): each is sealed with
  // its own honest `source` stamp. No memory fallback - the retired synthetic burst suite mislabelled
  // as 6x6 provenance and is neither scanned nor read.
  if (!g.streaming && g.stream && g.stream.stream_served === true) {
    // Stamp the dialect the STREAM SUITE ACTUALLY USED (derived from the endpoint it probed), not the
    // matrix's passthrough diagonal: the two can differ, and stamping the diagonal would claim
    // provenance for a run that never happened. Unknown endpoint -> null (the caption says "?").
    const dia = streamSuiteDialect(g.stream);
    g.streaming = sealStreaming({
      added_ttft_p50_us: g.stream.stream_added_ttft_p50_us,
      added_ttft_p99_us: g.stream.stream_added_ttft_p99_us,
      added_gap_p50_us: g.stream.stream_added_gap_p50_us,
      added_gap_p99_us: g.stream.stream_added_gap_p99_us,
      streams_sustained: g.stream.stream_sustained_streams,
      streams_sustained_fps: g.stream.stream_sustained_fps,
      // The legacy stream suite predates both the flag and these fields, so no ceiling/fraction to state.
      streams_sustained_mock_ceiling: g.stream.stream_mock_ceiling ?? null,
      streams_sustained_headroom: g.stream.stream_headroom ?? null,

    }, dia, makeSource("stream-fallback", SWEEP.STREAM_SUITE, g.stream.build ?? null, g.stream.measured_at ?? null),
    null,
    // cpu_fps comes from the SEPARATE streamcpu suite (own cadence), so it needs its own stamp rather
    // than the record's stream-suite one - only carried when the two genuinely disagree.
    cpuFpsSourceFor(g));
  }
  if (!g.best_cell && g.perf && g.perf.served === true && g.perf.added_latency_p99_us != null) {
    // No swept diagonal, but the perf suite ran the gateway's default passthrough. Seal it into the same
    // canonical shape with source:"perf-fallback" so provenance is visible on every surface.
    const dia = passthroughDialect(g.matrix);
    g.best_cell = sealPerfCell({
      added_latency_p50_us: g.perf.added_latency_p50_us,
      added_latency_p99_us: g.perf.added_latency_p99_us,
      // The legacy perf suite recorded no per-rung tail latencies, so there's no frontier to read off -
      // publishing its scalar in a bound's column would mislabel a differently-defined measurement.
      // Latency figures stay (comparable); throughput shows none until a matrix run replaces it.
      frontier: [],
      sweep_max_proxy: g.perf.sweep_max_proxy ?? null,
      sweep_sustained_20ms: g.perf.sweep_sustained_20ms ?? null,
    }, { ingress: dia, egress: dia, dialect: dia },
      makeSource("perf-fallback", SWEEP.PERF_SUITE, g.perf.build ?? null, g.perf.measured_at ?? null));
  }
  if (!g.translation_cell && g.xlate && g.xlate.xlate_served === true && g.xlate.xlate_added_latency_p99_us != null) {
    // Legacy xlate suite (anthropic in -> openai out - the OPPOSITE direction of the matrix cell).
    // Sealed with its real direction + source:"xlate-fallback" so the two paths can never be confused.
    g.translation_cell = sealPerfCell({
      added_latency_p50_us: g.xlate.xlate_added_latency_p50_us,
      added_latency_p99_us: g.xlate.xlate_added_latency_p99_us,
      // Same as the perf fallback above: no per-rung tails, so no frontier is published.
      frontier: [],
    }, { ingress: "anthropic", egress: "openai" },
      makeSource("xlate-fallback", SWEEP.XLATE_SUITE, g.xlate.build ?? null, g.xlate.measured_at ?? null));
  }
  // GOVERNANCE RETIRED: no `supports_governed` derivation (the governed suite was busbar-only and is retired).
  //
  // PER-GATEWAY freshness stamp: each gateway carries its OWN newest measurement so the board can show
  // an independent "measured Nd ago" per row (honest on a living board where any gateway can be re-run
  // alone). LOW-R3-3: must age the DISPLAYED numbers (g.matrix-preferring via displayedMeasuredMs, a
  // hoisted fn below), not the max across all suites - otherwise a stale ad-hoc legacy re-run could
  // overstate freshness for numbers the board isn't even showing. Drives the app.js badge, not a build failure.
  const gAtMs = displayedMeasuredMs(g);
  g.measured_at = gAtMs > 0 ? new Date(gAtMs).toISOString() : null;
  // AUDIT #8: a PER-LANE freshness stamp, since a whole displayed TAB can project from a never-refreshed
  // legacy suite (ageing it by the matrix stamp would overstate that tab's freshness). Each lane carries
  // the measured_at of the record it actually shows; app.js renders the age from this.
  g.lane_measured_at = {};
  for (const [lane, rec] of [["perf", g.best_cell], ["xlate", g.translation_cell],
    ["stream", g.streaming]]) {
    const at = rec && rec.source && rec.source.measured_at;
    if (at) g.lane_measured_at[lane] = at;
  }
  // The memory lane has no per-gateway record/`source`, so it ages by the matrix that produced its
  // windows directly - only stamped when a served cell actually carries one.
  if (matrixHasCellMemory(g.matrix) && g.matrix.measured_at) g.lane_measured_at.memory = g.matrix.measured_at;
  // ---- RIG PROVENANCE: WHICH measurement instrument produced this row -------------------------------
  // The mock + loadgen come from a MOVING GitHub release tag, so an identical harness can produce
  // different cell verdicts across runs purely because the instrument was rebuilt between them.
  // Null-safe: a snapshot with no rig block renders "not recorded", never a fabricated digest.
  g.rig = (g.matrix && g.matrix.rig) || null;
  return g;
})
/* A GATEWAY NOBODY HAS MEASURED IS NOT A ROW. Every gateway with a definition used to become a row
   regardless, which read as an implication (nothing to show) rather than a disclosure (nobody has run
   it yet) for a freshly-added gateway. A gateway REJOINS the board the moment a snapshot lands. */
  ;
const wasMeasured = (g) => Boolean(g.matrix || g.snapshot_file || g.perf || g.stream || g.memory);
// ...BUT AN ALL-EMPTY BOARD KEEPS ITS DECLARED ROWS: an unmeasured row only misleads by comparison to
// rows that DO have data; a board where nothing has been measured is just the honest empty bundle a
// fresh checkout produces (no snapshots committed), and the site test suite depends on gen-data
// emitting that rather than throwing.
// POLICY (2026-08-26): a declared entrant appears the moment its definition lands, before its first
// measurement, with n/a on every lane - the roster shows what's ON the bench, not just what has
// finished running. wasMeasured is retained for the lane-freshness/audit paths that still need it.
void wasMeasured;
const gateways = allGateways;

// Matrix v1 carries one upstream shape (fixed openai) as top-level `cells`; v2 carries the full 6x6
// under `upstreams.<egress>.cells` plus the same top-level compat row. Normalize v1 into the v2 shape:
// the one measured egress column becomes `upstreams`, columns v1 never probed stay absent/"not measured".
// The gateway's BEST passthrough cell for the Passthrough tab: its same-dialect diagonal (ingress ===
// egress), the canonical `openai` diagonal when served, else the fastest NATIVE diagonal by lowest added
// latency. BEST-OF rather than strict-openai, so every gateway appears on its best passthrough.
// `dialect` (== ingress == egress) is the label the tab's "Tested on" pill shows.
// ---- envelope sealers (Design E §2): raw cell -> sealed, envelope-carrying record ----------
// The projected record carries `path` (ingress/egress/dialect), `source` (the provenance stamp), and one
// SEALED envelope per metric under its own field name. The raw scalar is consumed here and never
// re-emitted, so no ungated field survives for a render site to leak (invariant P1).
// absentEntryFor: the engine's `absences` entry for one field, tolerant of the block-prefixed key
// shape the cell publishes ("perf.added_latency_p50_us") and the bare one a projected record carries.
function absentEntryFor(absences, prefix, k) {
  if (!absences || typeof absences !== "object") return null;
  return absences[`${prefix}.${k}`] || absences[k] || null;
}
// sealPerfCellPerf: a raw perf object -> {<sealed metrics>} (no path/source; the caller stamps those).
// Used BOTH for the canonical best_cell/translation_cell AND to seal every matrix cell in-place, so the
// matrix popup reads envelopes, never raw scalars (invariant C1: no ungated field survives in the bundle).
// `absences` is the raw CELL's sibling reason map; an absent metric now emits an envelope CARRYING that
// reason instead of no key at all - a missing key was how "below rig resolution" rendered as a bare n/a.
function sealPerfCellPerf(perf, absences = null) {
  const rec = {};
  for (const k of UNGATED_LAT)
    rec[k] = sealMetric(perf[k], { absent: absentEntryFor(absences, "perf", k) });
  // What the cell COST. Sealed exactly like the latency fields, for the same reason: an absent cost
  // must carry WHY (a bare 0 would make a never-measured gateway look infinitely efficient).
  for (const k of UNGATED_COST_FIELDS)
    rec[k] = sealMetric(perf[k], { absent: absentEntryFor(absences, "perf", k) });
  // One throughput reading per declared tail-latency bound, off one sweep. Replaces `sealThroughput`
  // and its two scalars (`rps_sustained_20ms`/`rps_max_proxy`), which could invert against each other
  // since two different algorithms summarised the same windows. See seal.mjs / engine's `frontier.rs`.
  rec.frontier = sealFrontier(perf.frontier, absences);
  // The rungs every reading was taken from, so a reader can re-derive the frontier rather than trust it.
  // Plain field: evidence, not a metric.
  if (Array.isArray(perf.sweep_max_proxy) && perf.sweep_max_proxy.length) {
    rec.sweep = perf.sweep_max_proxy;
    // The same rungs as a SEALED reading, so the board can compare at one concurrency instead of each
    // gateway's own peak - `sweep` above is evidence, this is the metric the concurrency selector ranks on.
    rec.rungs = sealRungs(perf.sweep_max_proxy);
  }
  if (perf.egress_reverified != null) rec.egress_reverified = perf.egress_reverified;
  // egress_reverified: the fairness guard's boolean (did this gateway actually TRANSLATE to the egress
  // dialect, or just proxy the ingress request verbatim - which the mock would otherwise score as a
  // capability it doesn't have). reverify_note is the reason behind it; a FALSE reverify is an
  // accusation, so it always ships with its basis. Neither is a sealed metric - both are plain fields.
  if (perf.reverify_note != null) rec.reverify_note = perf.reverify_note;
  return rec;
}
// sealPerfCell: a matrix/fallback perf object -> the canonical {path, source, <sealed metrics>} record.
function sealPerfCell(perf, path, source, absences = null) {
  return { path: { ...path }, source, ...sealPerfCellPerf(perf, absences) };
}
// sealStreamRecord: a raw stream record -> {<sealed metrics>} (no path/source). Used for the canonical g.streaming AND
// for sealing every matrix cell's own .stream in-place (so the popup reads envelopes).
// `cpuSource`: the provenance stamp for cpu_fps ALONE, when that number came from a different run than
// the rest of the record (the legacy stream-suite fallback below reads it from the SEPARATE streamcpu
// suite). Null on every matrix path, where one cell produced the whole record.
function sealStreamRecord(s, absences = null, cpuSource = null) {
  const rec = {};
  const abs = (k) => absentEntryFor(absences, "stream", k);
  for (const k of UNGATED_STREAM_FIELDS)
    rec[k] = sealMetric(s[k], { absent: abs(k) });
  // streams_sustained_fps and streams_sustained are the SAME bisect's rate and count, so they share a
  // ceiling and headroom (audit #11: sealing the rate ungated beside a gated count let the rate publish
  // while its count was suppressed - neither is suppressed now).
  // The ceiling is DERIVED, not measured (`run::mock_frame_ceiling_fps`, from the mock's own declared
  // frame count/pacing) - a gateway at ~1.0 of it forwarded every frame as it arrived, the best outcome
  // a proxy of a paced upstream can have.
  // zeroNoteFor (seal.mjs) is the ONE place the zero-note mapping lives, so this and the independent
  // oracle can never disagree about the note.
  const streamCeiling = s.streams_sustained_mock_ceiling ?? null;
  const streamHeadroom = s.streams_sustained_headroom ?? null;
  rec.streams_sustained_fps = sealMetric(s.streams_sustained_fps, {
    headroom: streamHeadroom, ceiling: streamCeiling,
    zeroNote: zeroNoteFor("streams_sustained_fps"),
    absent: abs("streams_sustained_fps") });
  // AUDIT #3: streaming counts - a 0 is a MEASURED FAILURE (offered stream load, sustained none), NOT
  // "not measured". Only a null (absent field) is not-measured. The note names which, and every surface
  // renders the two apart.
  rec.streams_sustained = sealMetric(s.streams_sustained, {
    headroom: streamHeadroom, ceiling: streamCeiling,
    zeroNote: zeroNoteFor("streams_sustained"),
    absent: abs("streams_sustained") });
  // NO cpu_fps: retired. Of the 16 cells that published it alongside `streams_sustained_fps`, several
  // inverted below the delivery boundary or were measured while dropping frames. See engine's `run.rs`.
  void cpuSource;
  return rec;
}
function sealStreaming(s, dialect, source, absences = null, cpuSource = null) {
  return { path: { dialect }, source, stream_served: true, ...sealStreamRecord(s, absences, cpuSource) };
}
// cpuFpsSourceFor(g): the provenance stamp cpu_fps needs OF ITS OWN on the legacy stream-suite fallback
// row, or null when the record's own stamp already describes it. cpu_fps is measured by the streamcpu
// suite and the rest of the record by the stream suite; they are separate runs with separate build +
// measured_at, so the record's single stamp is the truth for one of them and a fabrication for the other
// whenever they differ. Hoisted, so the per-gateway pass above can call it.
function cpuFpsSourceFor(g) {
  if (!g.streamcpu || g.streamcpu.streamcpu_frames_per_sec == null) return null;
  const build = g.streamcpu.build ?? null, at = g.streamcpu.measured_at ?? null;
  const sameRun = build === (g.stream.build ?? null) && at === (g.stream.measured_at ?? null);
  return sameRun ? null : makeSource("stream-fallback", SWEEP.STREAM_SUITE, build, at);
}
// matrixHasCellMemory(m): does this matrix carry a per-cell memory window on any served cell? The memory
// lane's freshness stamp ages by the matrix that produced those windows, so the lane must know whether it
// has anything to age. Hoisted, so the per-gateway pass above can call it.
function matrixHasCellMemory(m) {
  if (!m || typeof m !== "object") return false;
  const cellGroups = [m.cells, ...Object.values(m.upstreams || {}).map((u) => u && u.cells)];
  for (const cells of cellGroups) {
    if (!cells || typeof cells !== "object") continue;
    for (const cell of Object.values(cells)) {
      if (cell && cell.served === true && cell.memory && typeof cell.memory === "object") return true;
    }
  }
  return false;
}
// sealMatrixCellsInPlace: replace every served cell's raw perf/stream AND the top-level memory block's raw
// RSS with SEALED envelopes, so the matrix popup + Protocol view + the embedded/snapshot matrix carry
// envelopes, never raw scalars - NO ungated metric field survives anywhere in the bundle (invariant C1).
// Non-metric fields (served/status/path/verdict_note/load_cell/rss_series/…) are untouched.
function sealMatrixCellsInPlace(m) {
  const seen = new Set();   // v1 shares m.cells with upstreams[shape].cells (same refs) - seal once.
  const cellGroups = [m.cells, ...Object.values(m.upstreams || {}).map((u) => u && u.cells)];
  for (const cells of cellGroups) {
    if (!cells || typeof cells !== "object") continue;
    for (const cell of Object.values(cells)) {
      if (!cell || typeof cell !== "object" || seen.has(cell)) continue;
      seen.add(cell);
      if (cell.perf) cell.perf = sealPerfCellPerf(cell.perf, cell.absences);
      // stream_served is `true`/`false`/a status string ("not_measured", "untestable", "not_probed" -
      // record.rs), almost NEVER the literal boolean true in real data - so gating the seal on `=== true`
      // left every other case's raw object (and unconsumed _mock_bound flags) untouched (invariant C1).
      // Seal whenever a stream object exists at all; app.js still gates display on stream_served === true
      // itself, so this changes nothing shown.
      if (cell.stream && typeof cell.stream === "object") {
        cell.stream = {
          stream_served: cell.stream.stream_served,
          stream_error: cell.stream.stream_error ?? null,
          // `reason` is the machine token for why stream_served came out that way (Absent vocabulary),
          // `stream_error` its prose, `stream_c1_note` the c=1 leg's own note. Rebuilding this object
          // from a fixed key list once silently dropped reason/stream_c1_note - a cell can publish
          // stream_served:true WITH a reason on the TTFT leg, so carrying all three is load-bearing.
          // None is a sealed metric (prose/vocabulary, not numbers); app.js maps the token, never prints
          // it verbatim.
          reason: cell.stream.reason ?? null,
          stream_c1_note: cell.stream.stream_c1_note ?? null,
          ...sealStreamRecord(cell.stream, cell.absences),
        };
      }
      // PER-CELL MEMORY: its own cold-started, plateau-terminated window. The memory tab reads these
      // directly off the matrix cell, sealed BY DISCOVERY (any RSS field) plus the non-RSS vocabulary
      // (growth rate, time to plateau).
      if (cell.memory && typeof cell.memory === "object") {
        for (const k of Object.keys(cell.memory))
          if (isMetricField(k))
            cell.memory[k] = sealMetric(cell.memory[k], { absent: absentEntryFor(cell.absences, "memory", k) });
      }
    }
  }
  // A LEGACY top-level memory block (pre-per-cell, no longer read by anything) still travels embedded in
  // g.matrix/the snapshot; seal its metrics so no bare ungated field survives. Same vocabulary and same
  // absences handling as a per-cell window (audit #11) - testing only RSS_FIELD_RE here once shipped the
  // non-RSS memory metrics as bare scalars, and passing `{}` for opts discarded absence reasons.
  if (m.memory && typeof m.memory === "object") {
    for (const k of Object.keys(m.memory))
      if (isMetricField(k))
        m.memory[k] = sealMetric(m.memory[k], { absent: absentEntryFor(m.absences, "memory", k) });
  }
}

// addedP99Rank(rec): the rank a candidate cell's added-latency p99 sorts by (lower is better). A
// measured number ranks as itself; a null whose absences entry says "below_resolution" ranks as 0 (the
// engine's BEST outcome, not a hole - Infinity there once turned a win into a last-place sort). Any
// other null still sorts last. `rec` carries the raw perf fields plus the cell's `absences` map.
function addedP99Rank(rec) {
  if (rec.added_latency_p99_us != null) return rec.added_latency_p99_us;
  const abs = absentEntryFor(rec.absences, "perf", "added_latency_p99_us");
  return abs && abs.reason === "below_resolution" ? 0 : Infinity;
}

function bestCell(m) {
  if (!m.upstreams) return null;
  const diag = [];
  for (const [egress, up] of Object.entries(m.upstreams)) {
    const cell = up && up.cells && up.cells[egress];        // ingress === egress
    // Added latency RANKS this choice; it does not QUALIFY the cell. Requiring a c=1 latency reading as
    // a precondition once dropped whole rows to n/a in Peak mode while Same mode (reading the cell
    // directly) showed the same numbers fine. A cell qualifies by being SERVED and carrying perf;
    // latency then orders the candidates, and a cell without one sorts last rather than being struck.
    if (cell && cell.served === true && cell.perf)
      diag.push({ ingress: egress, egress, dialect: egress, absences: cell.absences ?? null, ...cell.perf });
  }
  if (!diag.length) return null;
  const openai = diag.find((d) => d.dialect === "openai");
  if (openai) return openai;
  // Rank through addedP99Rank so a below-resolution p99 (the best possible reading) sorts first,
  // not last: a null there is the engine saying "too small to weigh", never "unknown".
  return diag.reduce((a, b) => (addedP99Rank(b) < addedP99Rank(a) ? b : a));
}

// The gateway's TRANSLATION cell for the Translation tab: openai INGRESS (fixed fair input) translated
// to its best non-openai EGRESS. "Best" = LOWEST added latency p99 (a proxy's quality is its overhead;
// RPS is capacity-bound and noisier). Returns {ingress:"openai", egress, ...perf} or null.
// Selection is two-tier: FAIR (openai ingress) first, else ANY served cross-dialect cell the matrix
// measured - so a matrix measuring translation only in the other direction still wins over the legacy
// xlate suite's stale number. The legacy fallback fires only when the matrix has NO translation cell at all.
function translationCell(m) {
  if (!m.upstreams) return null;
  const fair = [], any = [];
  for (const [egress, up] of Object.entries(m.upstreams)) {
    for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
      if (ingress === egress) continue;                     // same dialect is passthrough, not translation
      if (!(cell && cell.served === true && cell.perf)) continue;
      const rec = { ingress, egress, absences: cell.absences ?? null, ...cell.perf };
      // A cell qualifies when its p99 rank is real: a measured number OR a below-resolution reading
      // (rank 0, the best outcome the rig can express). Requiring a non-null scalar here dropped the
      // below-resolution win entirely and let the legacy xlate fallback publish over the matrix.
      if (addedP99Rank(rec) === Infinity) continue;
      if (ingress === "openai" && egress !== "openai") fair.push(rec);
      any.push(rec);
    }
  }
  const cands = fair.length ? fair : any;
  if (!cands.length) return null;
  return cands.reduce((a, b) => (addedP99Rank(b) < addedP99Rank(a) ? b : a));
}

// streamSuiteDialect(s): the dialect the LEGACY stream suite actually drove, read off the endpoint it
// probed (the suite records no dialect field). `/v1/messages` is the Anthropic shape; a
// `chat/completions` path is the OpenAI shape. Anything else → null: unknown provenance is rendered as
// unknown, never borrowed from a different run (audit #6).
function streamSuiteDialect(s) {
  const ep = s && typeof s.endpoint === "string" ? s.endpoint : "";
  if (/\/messages\b/.test(ep)) return "anthropic";
  if (/chat\/completions/.test(ep)) return "openai";
  if (/\/responses\b/.test(ep)) return "openai-responses";
  return null;
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
    // Compare parsed epoch ms, not ISO strings lexicographically: a mixed-precision compare mis-orders
    // sibling stamps ('...06Z' sorts above '...06.5Z' since 'Z' > '.'), which could under-select the
    // true-newest instant the future-date hard-fail below relies on.
    if (j.measured_at && (!latest || Date.parse(j.measured_at) > Date.parse(latest))) latest = j.measured_at;
  }
}
const hardware = [...hwCounts.entries()].sort((a, b) => b[1] - a[1])[0]?.[0] || null;
// MED-1: the board footer must reflect the DISPLAYED numbers (matrix-preferring), not the max across
// all suites (which would fold in a never-displayed legacy re-run and overstate freshness). `latest`
// above still folds all suites, for the future-date corruption hard-fail only.
const latestDisplayedMs = Math.max(...gateways.map(displayedMeasuredMs), 0);
const latestDisplayed = latestDisplayedMs > 0 ? new Date(latestDisplayedMs).toISOString() : null;

// The newest embedded measurement across a gateway's own suites, in epoch ms (0 when it has none).
function newestMeasuredMs(g) {
  return Math.max(...SUITES.map((s) => g[s] && g[s].measured_at).filter(Boolean).map((a) => Date.parse(a)).concat([0]));
}

// MED-1 / MED-2: the stamp that drives what the board DISPLAYS, in epoch ms (0 when none) - the matrix
// stamp when present, else the newest-across-suites for a legacy-only row. Folding in retired legacy
// timestamps unconditionally let a never-displayed ad-hoc re-run make a stale matrix board look fresh.
function displayedMeasuredMs(g) {
  const matrixAt = g.matrix && g.matrix.measured_at ? Date.parse(g.matrix.measured_at) : 0;
  return matrixAt > 0 ? matrixAt : newestMeasuredMs(g);
}

// ---- freshness guard (matrix-sole-source) -----------------------------------
// The bundle is regenerated from the raw results tree on every run, so generated_at must never
// precede the newest embedded measurement: if it does, a raw result is future-dated (clock skew
// on the rig) and a "fresh" bundle would look older than its data. Hard fail; never ship it.
const generatedAt = new Date().toISOString();
// Compare as parsed epoch ms, not raw ISO strings: a lexicographic `generatedAt < latest` is only
// correct when both are the SAME ISO precision/zone, and a fractional-second vs whole-second mismatch
// can mis-order two instants that are microseconds apart.
if (latest && Date.parse(generatedAt) < Date.parse(latest)) {
  throw new Error(`gen-data: generated_at ${generatedAt} predates the newest embedded measured_at ${latest}; ` +
    `a raw result is future-dated (rig clock skew?). Refusing to emit a bundle that would read stale.`);
}

// FRESHNESS MODEL: a gateway's ENTIRE benchmark is ONE atomic matrix run that legitimately takes hours,
// and gateways publish INDEPENDENTLY, so differing measured_at across rows is honest. No cross-gateway
// lag check; only a generous intra-row sanity cap to catch a corrupt/future-dated timestamp.
// The wholesale-stale floor: if the newest measurement ANYWHERE is older than MAX_BOARD_AGE_DAYS, the
// whole board is stale and must not publish generated_at=now. The per-gateway MAX_GATEWAY_AGE_DAYS is a
// `stale` badge signal only, never a build failure.
const MAX_ROW_SPAN_SANITY_H = 12;  // sanity-only: one atomic matrix run is hours; >12h means a corrupt/skewed stamp
const MAX_GATEWAY_AGE_DAYS = 60;   // per-gateway staleness SIGNAL (badge), never a build failure
const MAX_BOARD_AGE_DAYS = 180;    // wholesale-stale floor (soft anchor): the whole board older than this = hard fail
// MED-2: base the wholesale-stale floor on the DISPLAYED (matrix-preferring) stamps, not the max across
// all suites - otherwise an untouched legacy re-run could make a stale matrix board's `boardNewest` look
// fresh and slip past the 180-day floor. Same basis as MED-1's footer and the per-row badge.
const boardNewest = Math.max(...gateways.map(displayedMeasuredMs), 0);
// A board whose age cannot be established must not publish as fresh - but "undatable" and "empty" are
// different boards. A board carrying measurements with no resolvable stamp is genuinely undatable and
// must not ship. A board where NO gateway has been benchmarked yet is unambiguous: nothing to date, and
// the honest bundle says so on every row (app.js/site suite already treat this as first-class). Once,
// conflating the two meant a clean checkout (no committed snapshots) threw before the site test suite
// reached a single assertion, so CI gated nothing.
// The distinction is EVIDENCE OF MEASUREMENT: a gateway is "measured" if it carries any suite artifact
// or projected record. If even one does and the board still can't be dated, it's a hard failure.
const measuredGateways = gateways.filter((g) =>
  SUITES.some((s) => g[s] != null) || !!g.best_cell || !!g.translation_cell || !!g.streaming || !!g.memory_read);
if (boardNewest <= 0 && measuredGateways.length > 0) {
  throw new Error(
    `gen-data: FRESHNESS FAILURE (undatable board): ${measuredGateways.length} of ${gateways.length} gateway(s) carry ` +
    `measurements but not one carries a resolvable displayed measured_at, so the board's age cannot be established ` +
    `at all. Refusing to publish generated_at=${generatedAt} over data that cannot be dated.`);
}
if (boardNewest <= 0 && measuredGateways.length === 0 && gateways.length > 0) {
  console.warn(`gen-data: the board carries NO measurements at all (${gateways.length} declared gateway(s), none benchmarked). ` +
    `Emitting an honest empty board: every row reads n/a and there is no age to publish. This is a legitimate ` +
    `state (a fresh checkout has no artifacts committed), not an undatable one.`);
}
if (boardNewest > 0) {
  const boardAgeDays = (Date.parse(generatedAt) - boardNewest) / 86400000;
  if (boardAgeDays > MAX_BOARD_AGE_DAYS) {
    throw new Error(
      `gen-data: FRESHNESS FAILURE (stale board): the newest DISPLAYED measurement anywhere on the board is ${boardAgeDays.toFixed(1)}d old ` +
      `(> ${MAX_BOARD_AGE_DAYS}d) - the WHOLE board is wholesale-stale (nothing displayed has refreshed at all). ` +
      `Refusing to publish generated_at=${generatedAt} over stale data. Re-run the field.`);
  }
}
const nowMs = Date.parse(generatedAt);
for (const g of gateways) {
  g.stale = false;
  // Per-gateway future-date sanity check: the board-wide floor above only checks the max, so a lone
  // clock-skewed future stamp on one gateway could still render a negative "measured Nd ago" badge.
  // Skip any future suite stamp for this gateway's freshness computation.
  let sawFuture = false;
  for (const s of SUITES) {
    const at = g[s] && g[s].measured_at;
    if (at && Date.parse(at) > nowMs) {
      sawFuture = true;
      console.warn(`gen-data: WARNING: ${g.key}.${s}.measured_at ${at} is in the FUTURE (> generated_at ${generatedAt}); ` +
        `clock skew on the rig. Skipping this stamp for the freshness/age computation so the badge never reads negative.`);
    }
  }
  // Sanity-only span cap: restrict the span computation to the MATRIX suite's own measured_at, since
  // the retired legacy suites are fallback-only and never refreshed by a matrix-only re-run - folding
  // their weeks-old stamps in would trip the >12h cap on an honest matrix-only re-run.
  const matrixAt = g.matrix && g.matrix.measured_at && Date.parse(g.matrix.measured_at) <= nowMs
    ? Date.parse(g.matrix.measured_at) : null;
  // The staleness SIGNAL below still considers every suite's newest (non-future) stamp, so a gateway
  // whose ONLY data is a legacy suite still ages correctly; the SPAN cap is what is matrix-scoped.
  const ats = SUITES.map((s) => g[s] && g[s].measured_at).filter(Boolean)
    .map((a) => Date.parse(a)).filter((ms) => ms <= nowMs);
  // Re-derive the badge stamp from non-future ats when sawFuture, so a skewed stamp never posts a
  // negative age. Defense-in-depth: the board-wide future-date hard-fail above already throws first.
  if (sawFuture) g.measured_at = ats.length ? new Date(Math.max(...ats)).toISOString() : null;
  // A served matrix row whose DISPLAYED numbers project from g.matrix but carries no valid matrix
  // measured_at is CORRUPT (run.sh always stamps a matrix via `date -u`) - left unguarded it would
  // bypass every other freshness check and publish FRESH with no badge. Flag it stale instead.
  // Memory joins this test through the matrix cells it's read from (a matrix with per-cell windows
  // displays numbers just as much as one with a best_cell), not through a projected record.
  const matrixProjected = (g.best_cell || g.translation_cell || g.streaming || matrixHasCellMemory(g.matrix)) &&
    ([g.best_cell, g.translation_cell, g.streaming].some((r) => r && r.source && r.source.kind === "matrix")
      || matrixHasCellMemory(g.matrix));
  if (g.matrix && matrixProjected && matrixAt == null) {
    console.warn(`gen-data: WARNING: ${g.key} projects displayed numbers from a served matrix but its ` +
      `matrix.measured_at is missing/invalid (=${g.matrix.measured_at}) - run.sh always stamps a matrix, ` +
      `so this is corruption/hand-edit. Flagging the row STALE so it never publishes fresh without a badge.`);
    g.stale = true;
  }
  if (ats.length < 1) continue;
  // INERT PLACEHOLDER: a matrix row carries at most one matrix stamp, so this `>= 2` gate never fires
  // today. Retained (null-safe) so the span check reactivates automatically if a future matrix result
  // ever embeds multiple internal timestamps.
  const matrixSpanAts = matrixAt != null ? [matrixAt] : [];
  if (matrixSpanAts.length >= 2) {
    const spanH = (Math.max(...matrixSpanAts) - Math.min(...matrixSpanAts)) / 3600000;
    if (spanH > MAX_ROW_SPAN_SANITY_H) {
      throw new Error(
        `gen-data: FRESHNESS FAILURE (corrupt row): ${g.key}'s MATRIX timestamps span ${spanH.toFixed(1)}h (> ${MAX_ROW_SPAN_SANITY_H}h sanity cap) - ` +
        `a corrupt or future-dated timestamp (one atomic matrix run is hours, never this). matrix.measured_at=${g.matrix.measured_at}`);
    }
  }
  // PER-GATEWAY staleness SIGNAL (not a failure): flag a row whose own data has aged past the
  // threshold so app.js can show a "stale" badge. Age the DISPLAYED numbers - matrix.measured_at when
  // present, else the newest non-future suite stamp for a legacy-only row.
  const ageBasisMs = matrixAt != null ? matrixAt : Math.max(...ats);
  const ageDays = (nowMs - ageBasisMs) / 86400000;
  g.stale = ageDays > MAX_GATEWAY_AGE_DAYS;
}

// ---- NO CHART PNGs ----------------------------------------------------------
// The Charts tab draws from the board itself now (at the reader's chosen bound/cell), so there's no
// PNG to ship. The key is REMOVED rather than emitted empty: `charts: []` would falsely imply the
// board looked for charts and found none.
// ---- fonts: copy the repo's bundled Inter faces -----------------------------
const fontsDir = join(ROOT, "assets", "fonts");
if (existsSync(fontsDir)) {
  mkdirSync(join(OUT, "fonts"), { recursive: true });
  for (const f of readdirSync(fontsDir)) copyFileSync(join(fontsDir, f), join(OUT, "fonts", f));
}

// ---- SPA fallback for deep links (/gateways/matrix, ...) --------------------
// The host is Cloudflare Pages, which reads site/_redirects (committed) for the /* -> /index.html 200
// rewrite. We deliberately do NOT emit a 404.html: on CF Pages a 404.html SHADOWS the _redirects
// rewrite (serves 404.html with a 404 status instead of the 200-rewrite), breaking every deep link.
// GitHub Pages (which needed the 404.html fallback) is retired.
const redirects = join(HERE, "_redirects");
if (existsSync(redirects) && OUT !== HERE) copyFileSync(redirects, join(OUT, "_redirects"));

// ---- emit -------------------------------------------------------------------
// The board's own engine, and each row's, surfaced for the reader: a row measured by an older engine
// isn't necessarily wrong, but isn't comparable, and a reader deciding whether to trust a comparison
// needs to see that. The board's version is the engine of the most recently measured row; a row whose
// engine differs is marked (rendered in red).
//
// Compares by INSTRUMENT, not raw commit - the same resolution C8 uses (site/instrument-equivalence.json
// attests which commits build byte-identical binaries), so gen-data and check-consistency can't disagree
// about what "the same engine" means. Falls back to the raw sha for any commit the file doesn't attest
// (an unlisted commit is its own instrument).
const equivalence = (() => {
  const f = join(HERE, "instrument-equivalence.json");
  if (!existsSync(f)) return new Map();
  return instrumentOf(readFileSync(f, "utf8"));
})();
const engineOf = (g) => {
  const sha = (g && g.rig && g.rig.engine && g.rig.engine.commit) || null;
  return sha == null ? null : (equivalence.get(sha) || sha);
};
// The raw commit, for the per-row stamp a reader sees. The instrument decides COMPARABILITY; the sha
// is still what identifies the exact tree, and collapsing it here would hide which commit ran.
const engineShaOf = (g) => (g && g.rig && g.rig.engine && g.rig.engine.commit) || null;
const newestRow = gateways
  .filter((g) => displayedMeasuredMs(g) > 0)
  .sort((a, b) => displayedMeasuredMs(b) - displayedMeasuredMs(a))[0];
const boardEngine = engineOf(newestRow);
for (const g of gateways) {
  // `sha` is what ran; `inst` is what it is comparable TO. A row is current when its INSTRUMENT
  // matches the board's, so two commits proven to build the same binary both read as current while
  // the row still reports the exact commit that produced it.
  const sha = engineShaOf(g);
  const inst = engineOf(g);
  g.engine = sha
    ? { sha, short: sha.slice(0, 7), current: boardEngine == null || inst === boardEngine }
    // A row with no engine stamp predates the stamp entirely; saying so is better than implying it
    // matches, and better than omitting the field so the render site has to guess.
    : { sha: null, short: null, current: false };
}

// ---- OTB_SINGLE_ENGINE: n/a beats a mixed board ---------------------------------------------
// C8 refuses to publish a board whose columns were measured by different harnesses (a ranking across
// two instruments compares the instruments as much as the gateways). Rather than re-measuring the
// whole field or overriding the guard, this shows the gateways the CURRENT engine measured and n/a for
// the ones it hasn't reached yet - the board stays single-instrument by construction, and a partial
// re-run publishes as it lands instead of the whole field waiting on the slowest box.
//
// IDENTITY IS A WHITELIST, deliberately: blacklisting measurement fields would let every future field
// leak through suppression by default (stale numbers from another instrument, presented as current).
// A suppressed row keeps the same shape as a never-measured one, which the site already renders as n/a.
// Stripped fields are set to NULL, not deleted - `rig` etc. are null-safe-by-contract, and deleting them
// would turn "measured nothing" into "this key never existed", a different (and untested) claim.
const SINGLE_ENGINE = process.env.OTB_SINGLE_ENGINE === "1";
const suppressedForEngine = [];
if (SINGLE_ENGINE && boardEngine != null) {
  const IDENTITY = new Set([
    "key", "display", "lang", "cls", "repo", "version",
    "stars", "stars_as_of", "first_commit", "engine",
  ]);
  for (const g of gateways) {
    if (g.engine.current) continue;
    for (const k of Object.keys(g)) if (!IDENTITY.has(k)) g[k] = null;
    // WHY this row is blank, on the row itself. A reader who sees n/a is owed the difference between
    // "this gateway was measured and had nothing to show" and "this gateway has not been re-measured
    // on the harness that produced everything else on screen".
    g.awaiting_engine = boardEngine.slice(0, 7);
    suppressedForEngine.push(g.key);
  }
  if (suppressedForEngine.length)
    console.log(`gen-data: OTB_SINGLE_ENGINE - ${suppressedForEngine.length} row(s) show n/a pending ` +
      `re-measurement on ${boardEngine.slice(0, 7)}: ${suppressedForEngine.join(", ")}`);
}

// METRIC DEFINITIONS FOR THE BOARD (data.definitions): the engine's own prose for each metric,
// generated from its own constants (engine/src/suite.rs metric_definitions) so the definition can never
// drift from the enforcement it describes. Sourced ONLY from rows measured by boardEngine - a
// definitions map from a different engine could describe different constants than what's on screen.
// A board with no boardEngine, or where no boardEngine row has the field yet, publishes NO definitions
// rather than guess.
let definitions = null;
if (boardEngine != null) {
  for (const [key, defs] of snapshotDefinitionsByKey) {
    if (engineOf(gateways.find((g) => g.key === key)) !== boardEngine) continue;
    if (definitions == null) { definitions = { ...defs }; continue; }
    // Two rows on the SAME engine commit must produce byte-identical definitions (they're generated
    // from the binary's own constants) - divergence means corruption (hand-edited snapshot, dirty
    // build), so refuse loudly rather than silently picking one.
    for (const [k, v] of Object.entries(defs)) {
      if (definitions[k] !== undefined && definitions[k] !== v) {
        throw new Error(
          `gen-data: DEFINITIONS DISAGREE for metric "${k}" between two rows both stamped engine ` +
          `${boardEngine.slice(0, 7)} (gateway ${key} vs. an earlier row on this engine) - the same ` +
          `engine commit must generate identical definitions, so this is corruption (hand-edited ` +
          `snapshot, or a dirty build stamped as this commit), not a legitimate disagreement. Refusing ` +
          `to publish definitions the board's own numbers cannot be trusted to match.\n` +
          `  earlier: ${JSON.stringify(definitions[k])}\n  ${key}:   ${JSON.stringify(v)}`);
      }
    }
    definitions = { ...definitions, ...defs };
  }
}

const data = {
  category: "gateways", // which category bundle this is (see CATEGORIES in app.js)
  generated_at: generatedAt,
  hardware,
  // MED-1: the DISPLAYED-number freshness stamp (matrix-preferring), not the max across all suites.
  latest_measured_at: latestDisplayed,
  // Which harness measured the board, visible to the reader rather than known only to a build guard
  // (C8 passing made the engine invisible, so a reader couldn't tell which harness produced the numbers).
  benchmark_version: boardEngine,
  // Which rows are blank on purpose, so a later run can tell "nothing suppressed" (single-engine board)
  // apart from "the flag was off" (a mix would have been refused by C8).
  suppressed_for_engine: SINGLE_ENGINE ? suppressedForEngine.slice().sort() : null,
  repo: "https://github.com/GetBusbar/benchmarking",
  gateways,
};
// Omit the key entirely rather than publish `{}` when no boardEngine row has it yet: app.js reads
// `data.definitions` null-safely, and an empty object is indistinguishable from "every metric here
// has no definition", which is not the honest reading of "the engine hasn't told us yet".
if (definitions != null) data.definitions = definitions;
// C1: strip the raw legacy suite objects from the emitted bundle - they were projection INPUTS (now
// sealed into best_cell/translation_cell/streaming) and carry raw scalars + _mock_bound flags no
// surface reads any more. Removing them is what makes "no ungated metric field in the bundle" true.
// g.matrix stays (cells sealed in-place; top-level build/measured_at/etc drive freshness + the
// sweep-integrity oracle).
for (const g of gateways) {
  for (const suite of ["perf", "stream", "streamcpu", "xlate"]) delete g[suite];
}
/* THINNING THE RSS SERIES FOR THE WIRE. Raw RSS samples were 5.2 MB of a 14.1 MB bundle, feeding only a
   ~50px sparkline - every memory verdict is computed by the engine and published as its own field, and
   the full series stays in the snapshot artifacts for auditors. This is a drawing resolution, not a
   measurement.
   Decimation is MIN/MAX PRESERVING per bucket, not "every Nth sample" (which would draw a spike flat if
   it fell between strides). First/last samples are always kept so the window's own boundaries never move. */
const RSS_DRAW_POINTS = 240;
function thinSeries(series) {
  if (!Array.isArray(series) || series.length <= RSS_DRAW_POINTS) return series;
  const buckets = Math.floor(RSS_DRAW_POINTS / 2);
  const size = series.length / buckets;
  const val = (p) => (p && typeof p === "object" ? (p.rss_mib ?? p.mib ?? 0) : 0);
  const out = [series[0]];
  for (let b = 0; b < buckets; b++) {
    const lo = Math.floor(b * size), hi = Math.min(series.length, Math.floor((b + 1) * size));
    if (hi <= lo) continue;
    let min = series[lo], max = series[lo], minI = lo, maxI = lo;
    for (let i = lo + 1; i < hi; i++) {
      if (val(series[i]) < val(min)) { min = series[i]; minI = i; }
      if (val(series[i]) > val(max)) { max = series[i]; maxI = i; }
    }
    // In the order they actually occurred: a rise drawn as a fall is a different curve.
    for (const pt of (minI <= maxI ? [min, max] : [max, min])) {
      if (out[out.length - 1] !== pt) out.push(pt);
    }
  }
  const last = series[series.length - 1];
  if (out[out.length - 1] !== last) out.push(last);
  return out;
}
let thinnedFrom = 0, thinnedTo = 0;
(function thinAll(o) {
  if (Array.isArray(o)) { for (const x of o) thinAll(x); return; }
  if (!o || typeof o !== "object") return;
  for (const [k, v] of Object.entries(o)) {
    if ((k === "rss_series" || k === "idle_rss_series") && Array.isArray(v)) {
      const t = thinSeries(v);
      thinnedFrom += v.length; thinnedTo += t.length;
      o[k] = t;
    } else thinAll(v);
  }
})(data);

// Compact, not indented. The indentation was one space per level on a 14 MB document - megabytes of
// whitespace shipped to every reader for a file no human opens by hand.
writeFileSync(join(OUT, "data.json"), JSON.stringify(data) + "\n");
if (thinnedFrom) console.log(`gen-data: rss samples ${thinnedFrom} -> ${thinnedTo} for drawing (full series stay in the snapshots)`);
console.log(`gen-data: ${gateways.length} gateways -> ${join(OUT, "data.json")}`);
