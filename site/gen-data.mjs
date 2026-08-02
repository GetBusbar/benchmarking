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
// Scans gateways/*/definition.json (the manifest the engine runs from: display, lang, class, repo)
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
import { snapshotCellCoords, isStrictSubset, layerScopedMatrix } from "./snapshots.mjs";
import { sealMetric, sealFrontier, makeSource, SWEEP, UNGATED_LAT_FIELDS, UNGATED_COST_FIELDS, DEFAULT_BOUND_MS, frontierAt, UNGATED_STREAM_FIELDS, isMetricField, zeroNoteFor } from "./seal.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = process.argv[2] || join(HERE, "..");
const OUT = process.argv[3] || HERE;

// GOVERNANCE RETIRED (matrix-sole-source): governance is not measured on the board - the governed
// suite was busbar-only and is retired. `governed/run.sh` stays on disk (unused) but the suite is
// no longer scanned into the bundle and no governed column/derivation is emitted. See app.js.
// NOTE: "memory" is intentionally NOT scanned. The retired standalone memory suite wrote synthetic
// burst numbers (conc=1500, 150KB payload, 120s) that mislabelled as 6x6 provenance; memory now comes
// SOLELY from the matrix's PER-CELL windows (matrix.upstreams[egress].cells[ingress].memory, sealed
// below). No fallback, and NO per-gateway memory scalar: there is no cell to project one from that the
// harness would not have had to SELECT, which is the defect per-cell measurement exists to remove.
const SUITES = ["perf", "stream", "streamcpu", "xlate", "matrix"];
// The ungated (non-honesty-gated) latency-shaped metrics on a perf cell: always certified when present.
// Imported from seal.mjs - the ONE vocabulary check-consistency also imports, so the two can never lag
// each other. RSS fields are sealed BY DISCOVERY (RSS_FIELD_RE), never from a whitelist (audit #11).
const UNGATED_LAT = UNGATED_LAT_FIELDS;

/* THE PRICE, AS A NUMBER RATHER THAN A PHRASE.
   us-east-1 on-demand for the 4-core slice the gateway under test is pinned to. It is disclosed here
   and rendered in every caption that shows a dollar figure, so a reader on different pricing can
   rescale rather than having to trust the word "cheap". Override with GATEWAY_HOURLY_USD.

   Moved here from charts.py when the static chart pipeline was retired: the derivation is the board's,
   not a chart's, and leaving it in a script that only ran to draw PNGs meant deleting the PNGs would
   have silently deleted the metric. */
const GATEWAY_HOURLY_USD = Number(process.env.GATEWAY_HOURLY_USD || 0.1632);

/* costLanes(cell): req/s per $/hr and $ per 1M requests, from the frontier reading AT THE DEFAULT
   BOUND - so the dollar figures and the throughput column describe the SAME operating point.

   Cost is a rate divided by a price, so it inherits whatever qualification the rate carried. The
   bound is therefore named on every surface that shows the result; a dollar figure must never imply
   a tail it was not computed at.

   `cost_per_million` is ABSENT at rate 0, not 0. At zero the quotient is undefined, and 0 is the
   CHEAPEST value on a lower-is-better axis - so a gateway that held nothing under the bound would
   render as free, the best possible result, while ranking last. `rps_per_dollar` keeps its 0 because
   zero requests per dollar genuinely is zero. 0 is a number; n/a is not. */
function costLanes(cell) {
  const r = frontierAt((cell || {}).frontier, DEFAULT_BOUND_MS);
  /* AN ABSENT RATE IS NOT A ZERO RATE, AND BOTH LANES USED TO SAY IT WAS.
     `rate` collapsed "no measurement" into the same 0 as "measured zero", and that 0 went through
     `sealMetric` with no absence - emerging CERTIFIED. On the 2026-07-31 board one-api, plano and
     tensorzero each carried `rps.reason = "below_resolution"` at the 10ms bound and published
     `rps_per_dollar: {value: 0, certified: true}` beside it: the board asserting, as a measurement,
     that three competitors deliver zero requests per dollar, and ranking them last on a
     higher-is-better axis for it. Neither lane is in seal.mjs's vocabulary, so `isMetricField` is
     false for them and check-consistency never compared either against the raw artifact - which is
     why it shipped.

     The paragraph above is still right about a MEASURED zero: 0 requests per dollar genuinely is 0,
     and n/a is not. The defect was never the zero, it was treating absence as one.

     Both lanes now carry the RATE's own reason and detail. `cost_per_million_usd` used to stamp a
     hardcoded `not_measured` over it, flattening "measured, and it could not hold the bound" into
     "nothing was measured here" - the exact reason-flattening seal.mjs exists to prevent, and the
     reading that flatters the gateway. */
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
// The version a manifest pins, as a short human string. Image tag for a container gateway, short
// commit for one built from source. `null` when the manifest pins neither, which is a real state and
// renders as nothing rather than as a guess.
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

// A gateway built from source pins its ref in its own build.sh rather than in the manifest's launch
// block, because there is no image to name. Read it from there so those three rows carry a version
// too: `COMMIT="${SOME_COMMIT:-<sha>}"`, optionally followed by a `# tag vX.Y.Z` the pin came from,
// which is the more useful thing to show when it exists.
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
  // ONE manifest per gateway, and it is the one the engine runs from. This used to scrape
  // GW_DISPLAY/GW_LANG/GW_CLASS/GW_REPO out of a shell file that the Rust engine had already
  // stopped reading, so the board's labels came from a different file than the measurements - and
  // a gateway carrying only a definition.json was invisible to every shell-side caller.
  //
  // `cls` is each project's OWN self-description (its README/site tagline: "control plane", "LLM
  // gateway", "API gateway", ...), never our editorial classification. Missing/unknown falls back
  // to the neutral "Gateway".
  let d = {};
  try { d = JSON.parse(text); } catch { return { display: null, lang: null, repo: null, cls: null, version: null }; }
  return {
    display: d.display ?? null,
    lang: d.lang ?? null,
    repo: d.repo ?? null,
    cls: d.class ?? null,
    // THE VERSION WE PIN, WHICH IS KNOWN WITHOUT HAVING MEASURED ANYTHING.
    //
    // The engine stamps the version it ACTUALLY built into every run (`build`), and that stays the
    // authority for a row that has numbers. But it only exists once a run has happened, so a gateway
    // awaiting its first measurement rendered as a name and nothing else - no version, no upstream,
    // an empty row that reads as "we know nothing about this" when the manifest names the exact
    // image. This is the declared pin, so /gateways can always say WHAT would be measured even when
    // "last benchmarked" is honestly n/a.
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
// ran everything. Reads the producer's OWN mode flags (matrix/run.sh emits cell_perf_sweep / cell_stream /
// cell_memory), so it describes how the run was CONFIGURED, never how it turned out. A flag that is absent
// (an older result predating it) is treated as ON: only an explicit `false` is a switched-off phase.
function snapshotDegradedMode(m) {
  if (!m || typeof m !== "object") return "";
  const off = ["cell_perf_sweep", "cell_stream", "cell_memory"].filter((k) => m[k] === false);
  return off.length ? off.join("=false, ") + "=false" : "";
}

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
  /* SUBSET OF THE BASE, NOT OF THE WIDEST. Subset-ness only means anything relative to the run being
     layered ONTO: a scoped re-run is a subset of the GRID, not necessarily of some historical
     snapshot. Testing against `widest` misclassified any re-run that touched a coordinate the widest
     run lacked - add a 7th dialect, re-run `OTB_DIALECTS=openai,vertex`, and its 4 cells contain
     `vertex|openai`, which the old 36-cell run never had. Not a subset of `widest`, so it landed in
     `fulls`, became the base by recency, and 33 measured cells vanished from the board. That is the
     exact failure this module was written to prevent, arriving through the door next to the one it
     was watching. */
  const scopedNewer = snaps.filter((s) => isStrictSubset(s.__coords, base.__coords) && s.__ms >= base.__ms);
  if (!scopedNewer.length) return base;
  const out = structuredClone(base);
  out.__file = base.__file;
  let m = base.matrix;
  for (const sc of scopedNewer) {
    /* AND EACH SCOPED RUN IS SCREENED ON ITS OWN FLAGS. The degraded-mode refusal below reads
       `snap.matrix`, which by then is the LAYERED result carrying the BASE run's mode flags -
       `layerScopedMatrix` copies cells and nothing else. So every scoped snapshot that layered in was
       exempt from the guard, and a `verify-local` smoke run with `cell_perf_sweep: false` could
       replace the headline cell of a clean field run while the guard read the base's `true` and threw
       nothing. Screening here, per run, is the only place the scoped run's own flags still exist. */
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

// GitHub star snapshot for the Gateways overview: a COMMITTED build-time file
// (gateways/stars.json, refreshed by `node gateways/fetch-stars.mjs`), never a live
// API call, so the bundle stays reproducible and CI needs no network. Absent file or
// absent key degrades to null; the site renders those muted.
const starsSnap = readJson(join(gatewaysDir, "stars.json")) || {};

// snapshotDefinitionsByKey: gateway key -> the metric-definitions map off the SAME snapshot that
// became that gateway's g.matrix (see the capture below). Kept OUT of the gateway object itself -
// the per-gateway records are the public bundle shape and definitions are a board-level projection
// (data.definitions), not a per-row field - so this stays a private side table, never emitted raw.
const snapshotDefinitionsByKey = new Map();
const allGateways = gatewayKeys.map((key) => {
  const meta = parseManifest(readFileSync(join(gatewaysDir, key, "definition.json"), "utf8"));
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
    // The pinned version, present whether or not this gateway has ever been measured.
    version: meta.version ?? builtFromSourceVersion(key),
    stars: starsSnap[key]?.stars ?? null,
    stars_as_of: starsSnap[key]?.as_of ?? null,
    // Project age context: the repo's FIRST-commit date (not created_at, which resets on
    // renames). Rendered as a simple relative age - 43k stars over 10 years and 100 over 3
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
  const snap = resolvedSnapshot(key);
  if (snap) {
    // RECENCY, not existence (audit #5). An older snapshot must NEVER shadow a newer results/matrix/<gw>.json
    // (a matrix-only re-run writes the per-suite file; the snapshot archive can trail it). Compare the two
    // stamps and take the NEWER; a snapshot with no stamp loses to any stamped matrix, and wins only when
    // there is no per-suite matrix at all.
    const snapAt = snap.matrix && snap.measured_at ? Date.parse(snap.measured_at) : NaN;
    const diskAt = g.matrix && g.matrix.measured_at ? Date.parse(g.matrix.measured_at) : NaN;
    const snapMs = Number.isFinite(snapAt) ? snapAt : -1;
    const diskMs = Number.isFinite(diskAt) ? diskAt : -1;
    if (snap.matrix && (!g.matrix || snapMs >= diskMs)) {
      // A DEGRADED-MODE RUN MUST NOT BECOME THE BOARD'S SOURCE JUST BY BEING NEWER. The producer records
      // which phases it was told to run (cell_perf_sweep / cell_stream / cell_memory); a local smoke run
      // turns them off to finish in minutes and its snapshot lands in the SAME results/snapshots/
      // directory as a field run, so recency alone would let a probe-only run silently outrank and
      // shadow a complete one. This is NOT the "fewer served cells" case (a re-run that finds less IS
      // the new truth); it is the case where the producer was TOLD NOT TO MEASURE, so refusing it is a
      // statement about the run's MODE, never about its numbers.
      // SHADOWING WAS NEVER THE DEFECT; BECOMING THE BOARD'S SOURCE IS. The guard only fired when a
      // degraded snapshot was newer than a COMPLETE results/matrix/<gw>.json, so the case with nothing
      // to shadow - a gateway whose ONLY artifact is a local smoke run - walked straight through and
      // published as that gateway's board result, which is the exact outcome the guard's own message
      // describes as unacceptable. A run that was TOLD NOT TO MEASURE is not a measurement whether or
      // not a real one exists beside it; the presence of a fuller file changes the remedy, not the rule.
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
    // CANONICAL RULE: the per-cell MATRIX sweep is the single source of truth for all passthrough +
    // translation perf; the standalone perf/xlate suites are a LIVE deferred FALLBACK (a gateway with
    // no matrix sweep for that path). g.best_cell / g.translation_cell are the ONE canonical record
    // every surface reads (table, drawer, compare, charts.py). Every metric is SEALED here into an
    // envelope (seal.mjs): the raw scalar + its _mock_bound flag are CONSUMED, never re-emitted - a
    // render site has no ungated field to leak. The cell's `source` stamp discloses provenance and
    // drives every caption (no hard-coded source string can drift). See seal.mjs / Design E.
    const build = g.matrix.build ?? null, at = g.matrix.measured_at ?? null;
    // Per-cell perf (matrix v2 + sweep): the gateway's OPENAI diagonal when it serves one, else the
    // lowest-added-p99 diagonal it does serve. NOT "best by sustained RPS @20ms", which this comment
    // claimed and `bestCell` has never done - it takes the openai cell unconditionally and only ranks
    // when there is none.
    //
    // And that is the right rule, which is why the CODE stayed and the labels changed: comparing every
    // gateway on the SAME cell is fairer than comparing each on whichever cell flatters it most. But
    // three chart subtitles said "best same-dialect passthrough", and for apisix, agentgateway and
    // plano the openai diagonal is not their best one (apisix's openai-responses cell sustains 5.7%
    // more), so the word "best" was false on the board even though the number beside it was right.
    const bc = bestCell(g.matrix);
    if (bc) g.best_cell = sealPerfCell(bc, { ingress: bc.dialect, egress: bc.dialect, dialect: bc.dialect },
      makeSource("matrix", SWEEP.DIAGONAL, build, at), bc.absences);
    // THE DOLLAR LANES RIDE ON THE CANONICAL CELL, derived from its frontier reading at the default
    // bound - the same cell and the same operating point the throughput column ranks, so the two can
    // never describe different runs of the same gateway.
    if (g.best_cell) Object.assign(g.best_cell, costLanes(g.best_cell));
    // The gateway's TRANSLATION cell (openai in -> best non-openai egress).
    const tc = translationCell(g.matrix);
    if (tc) g.translation_cell = sealPerfCell(tc, { ingress: tc.ingress, egress: tc.egress },
      makeSource("matrix", SWEEP.TRANSLATION, build, at), tc.absences);
    // STREAMING projection (matrix single source): the BEST DIAGONAL cell's streaming - the SAME
    // (ingress==egress) cell the headline perf is projected from (one source of truth). Only when the
    // diagonal ACTUALLY STREAMED (stream_served===true); a non-streaming cell leaves g.streaming absent.
    if (bc) {
      const cell = g.matrix.upstreams?.[bc.dialect]?.cells?.[bc.dialect];
      if (cell && cell.stream && cell.stream.stream_served === true) {
        g.streaming = sealStreaming(cell.stream, bc.dialect,
          makeSource("matrix", SWEEP.STREAM_DIAGONAL, build, at), cell.absences);
      }
    }
    // MEMORY: NOT projected. Memory is measured per cell (its own cold-started, plateau-terminated
    // window on EVERY served cell) and stays per cell all the way to the reader - the board's memory lane
    // chooses a cell through the same chooser every other lane uses (Min | Max | Same | Custom) and says
    // which. A per-gateway scalar cannot exist without the harness selecting a cell silently, which is
    // exactly the defect the per-cell design removes. The windows are sealed in place below.
    // SEAL every matrix cell in-place (AFTER selection/projection, which read raw). The matrix popup +
    // Protocol view read cell.perf / cell.stream directly, so those must be envelopes too - otherwise a
    // raw ungated scalar survives in the bundle (invariant C1).
    sealMatrixCellsInPlace(g.matrix);
  }
  // LIVE DEFERRED FALLBACKS (stay until the field run folds them into the matrix; DO NOT break them).
  // Each is sealed with its OWN honest `source` stamp (stream-suite / perf-suite / xlate-suite), so the
  // envelope is correct NOW and captions tell the truth about provenance. There is NO memory fallback -
  // the retired synthetic burst suite mislabelled as 6x6 provenance and is neither scanned nor read.
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
      // The legacy stream suite predates both the flag and these fields, so there is no ceiling to
      // state and no fraction - which now costs the headroom and not the number.
      streams_sustained_mock_ceiling: g.stream.stream_mock_ceiling ?? null,
      streams_sustained_headroom: g.stream.stream_headroom ?? null,

    }, dia, makeSource("stream-fallback", SWEEP.STREAM_SUITE, g.stream.build ?? null, g.stream.measured_at ?? null),
    null,
    // cpu_fps CAME FROM THE OTHER SUITE, SO IT CARRIES THE OTHER SUITE'S STAMP. The record's own stamp
    // is the stream suite's build + measured_at; cpu_fps is produced by the SEPARATE streamcpu suite,
    // which runs on its own cadence, so stamping it with the record's provenance dated a number to a
    // run that never produced it. It is only carried when the two genuinely disagree - on a row where
    // both suites ran together the record's stamp already tells the truth, and repeating it on the
    // envelope would be bundle bloat for no added disclosure.
    cpuFpsSourceFor(g));
  }
  if (!g.best_cell && g.perf && g.perf.served === true && g.perf.added_latency_p99_us != null) {
    // No swept diagonal, but the perf suite ran the gateway's default passthrough. Seal it into the same
    // canonical shape with source:"perf-fallback" so provenance is visible on every surface.
    const dia = passthroughDialect(g.matrix);
    g.best_cell = sealPerfCell({
      added_latency_p50_us: g.perf.added_latency_p50_us,
      added_latency_p99_us: g.perf.added_latency_p99_us,
      // NO FRONTIER FROM THE LEGACY PERF SUITE. It measured a single throughput scalar under one
      // chosen latency ceiling and recorded no per-rung tail latencies, so there is nothing to read a
      // frontier off. Publishing its old number in one bound's column would put a differently-defined
      // measurement in a column that names a definition it does not meet - which is the exact class of
      // mislabelling this whole change removes. The cell keeps its latency figures, which ARE comparable,
      // and shows no throughput until a matrix run replaces it.
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
      // Same as the perf fallback above: the legacy xlate suite has no per-rung tails to read a
      // frontier from, so it publishes none rather than mislabelling its scalar as one bound's reading.
      frontier: [],
    }, { ingress: "anthropic", egress: "openai" },
      makeSource("xlate-fallback", SWEEP.XLATE_SUITE, g.xlate.build ?? null, g.xlate.measured_at ?? null));
  }
  // GOVERNANCE RETIRED: no `supports_governed` derivation. Under matrix-sole-source governance is not
  // a board metric (the governed suite was busbar-only and is retired), so the board neither emits a
  // governed column nor a supports_governed capability flag.
  //
  // PER-GATEWAY freshness stamp: each gateway carries its OWN newest measurement so the board can show
  // an independent "measured Nd ago" per row and flag a row that has aged past MAX_GATEWAY_AGE_DAYS.
  // measured_at legitimately differs per gateway (one today, another 3 weeks ago), and that
  // is honest on a living board where any one gateway can be re-run alone. The staleness flag drives a
  // per-row badge in app.js; it is NOT a build failure (see the freshness guard below).
  // LOW-R3-3: the badge stamp must reflect the age of the DISPLAYED numbers, which are projected from
  // g.matrix ONLY (best_cell / streaming / the per-cell memory windows). Deriving it from the MAX across all suites let a
  // newer legacy results/perf/<gw>.json (reachable via an ad-hoc SUITES=perf re-run) drive a "measured 5d
  // ago" badge while the shown matrix numbers were 90d old - the badge overstating freshness. Prefer the
  // matrix stamp; fall back to the newest-across-suites only when there is no matrix (a legacy-only row
  // whose numbers age by that stamp anyway). The staleness flag below is re-derived on the same basis.
  // LOW-R3-3 / MED-1: the per-row badge stamp ages the DISPLAYED (matrix-preferring) numbers - the SAME
  // shared basis the board-level footer + wholesale-stale floor now use (displayedMeasuredMs). Hoisted
  // function declaration, so it is callable here even though it is defined below.
  const gAtMs = displayedMeasuredMs(g);
  g.measured_at = gAtMs > 0 ? new Date(gAtMs).toISOString() : null;
  // AUDIT #8: a PER-LANE freshness stamp. The row badge ages the matrix, but a whole displayed TAB can
  // project from a never-refreshed legacy suite (every streaming column today comes from the stream
  // suite), so ageing it by the matrix stamp OVERSTATES that tab's freshness. Each lane therefore
  // carries the measured_at of THE RECORD IT ACTUALLY SHOWS; app.js renders the age from this.
  g.lane_measured_at = {};
  for (const [lane, rec] of [["perf", g.best_cell], ["xlate", g.translation_cell],
    ["stream", g.streaming]]) {
    const at = rec && rec.source && rec.source.measured_at;
    if (at) g.lane_measured_at[lane] = at;
  }
  // The memory lane projects no per-gateway record, so it has no `source` to age by. It reads the matrix
  // cells directly, so it ages by the matrix that produced them - and only when a served cell actually
  // carries a window, so a matrix with no memory data leaves the lane unstamped rather than claiming a
  // freshness for numbers that are not there.
  if (matrixHasCellMemory(g.matrix) && g.matrix.measured_at) g.lane_measured_at.memory = g.matrix.measured_at;
  // ---- RIG PROVENANCE: WHICH measurement instrument produced this row -------------------------------
  // The mock + loadgen come from a MOVING GitHub release tag, so an identical harness can produce
  // DIFFERENT cell verdicts across runs purely because the instrument was rebuilt between them.
  // Surfacing it here makes an instrument change legible at a glance on any cross-run comparison.
  // NULL-SAFE: a snapshot with no rig block renders "not recorded", never a fabricated digest.
  g.rig = (g.matrix && g.matrix.rig) || null;
  return g;
})
/* A GATEWAY NOBODY HAS MEASURED IS NOT A ROW.
   Every gateway with a definition became a row, measured or not, which was harmless while the only
   definitions were the fourteen on the board. Adding busbar-150 changed that: its manifest has to be
   pushed for the boxes to fetch it, so the moment it existed the public board grew a row reading
   "Busbar 1.5.0" with no matrix, no best_cell and no snapshot behind it.

   An empty row is not a disclosure, it is an implication - a reader sees a gateway that apparently
   has nothing to show, when the truth is nobody has run it yet. The board's rule is that an absence
   carries a reason; a gateway with no measurement at all has no absence to explain, because there is
   no cell to be absent.

   A gateway REJOINS the board the moment a snapshot lands for it. Nothing here decides what may be
   published - it decides what has been measured. */
  ;
const wasMeasured = (g) => Boolean(g.matrix || g.snapshot_file || g.perf || g.stream || g.memory);
// ...BUT AN ALL-EMPTY BOARD KEEPS ITS DECLARED ROWS. The two situations are not the same shape.
// An unmeasured row misleads by COMPARISON - it only reads as "this gateway has nothing to show"
// because it sits beside rows that do. A board where nothing at all has been measured draws no
// comparison; it is the honest empty bundle a fresh checkout produces, with n/a on every row, and
// the site suite itself depends on gen-data emitting it rather than throwing (a clean clone commits
// no snapshots, so this file would otherwise die at its own gen-data call before its first
// assertion - a suite that gates nothing while reading as an unrelated failure).
const gateways = allGateways.some(wasMeasured)
  ? allGateways.filter((g) => {
      if (wasMeasured(g)) return true;
      console.log(`gen-data: skipping ${g.key} - no snapshot, no suite result, nothing measured yet`);
      return false;
    })
  : allGateways;

// Matrix v1 results carry one upstream shape (fixed openai) as top-level `cells`; v2 carries the
// full 6x6 under `upstreams.<egress>.cells` plus the same top-level compat row. Normalize v1 into
// the v2 shape so the site renders exactly one structure: the one measured egress column becomes
// `upstreams`, and the columns v1 never probed stay absent (the site renders them "not measured",
// which is the honest reading of a v1 run: unmeasured, not "not configurable").
// The gateway's BEST passthrough cell for the Passthrough tab (BEST-OF): its same-dialect diagonal
// (ingress === egress, pure forwarding, no translation), chosen deterministically as the canonical
// `openai` diagonal when served (every gateway on the identical fair workload), else the gateway's
// fastest NATIVE diagonal by lowest added latency (e.g. one gateway -> anthropic). BEST-OF, not
// strict-openai, so EVERY gateway appears on its best passthrough; filtering a competitor out reads
// as hiding it. `dialect` (== ingress == egress) is the label the tab's "Tested on" pill shows.
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
  // WHAT THE CELL COST. Sealed exactly like the latency fields, and for the same reason: an absent
  // cost must arrive carrying WHY it is absent. Every snapshot taken before the capture existed has
  // none of these, and "not measured" is the only honest rendering - a 0 would make a gateway that
  // was never measured look infinitely efficient.
  for (const k of UNGATED_COST_FIELDS)
    rec[k] = sealMetric(perf[k], { absent: absentEntryFor(absences, "perf", k) });
  // THE THROUGHPUT ANSWER: one reading per declared tail-latency bound, off one sweep.
  //
  // This replaces `sealThroughput` and the two scalars it produced (`rps_sustained_20ms` and
  // `rps_max_proxy`), which were the same sweep collapsed twice by a chosen ceiling - and which could
  // invert against each other, because two different algorithms summarised one set of windows. See
  // seal.mjs and the engine's `frontier.rs`.
  rec.frontier = sealFrontier(perf.frontier, absences);
  // The rungs every reading was taken from, so a reader can re-derive the whole frontier rather than
  // taking it on trust. It rides as a plain field: it is evidence, not a metric.
  if (Array.isArray(perf.sweep_max_proxy) && perf.sweep_max_proxy.length)
    rec.sweep = perf.sweep_max_proxy;
  if (perf.egress_reverified != null) rec.egress_reverified = perf.egress_reverified;
  // The verdict without its evidence is an assertion. egress_reverified is the fairness guard's boolean
  // (did this gateway actually TRANSLATE to the egress dialect, or just proxy the ingress request
  // verbatim - which the mock, answering all six dialects by path, would otherwise score as a
  // translation capability it does not have). reverify_note is the reason string behind that boolean, and
  // it was dying here while the flag travelled on. A FALSE reverify is an accusation against a gateway;
  // publishing it with no stated basis is exactly what this codebase refuses to do elsewhere.
  // Neither is a sealed metric - they are provenance about a metric, so they ride as plain fields.
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
  // streams_sustained_fps and streams_sustained are the SAME bisect's rate and count, so they carry the
  // same comparison: one derived mock ceiling, one fraction of it. (They used to share a mock-bound FLAG
  // for the same reason - audit #11 was that sealing the rate ungated beside a gated count let the rate
  // publish while the count it came from was suppressed. Neither is suppressed now; what they share is
  // the fact, not a gate.)
  //
  // THE CEILING HERE IS DERIVED, NOT MEASURED - `run::mock_frame_ceiling_fps`, from the mock's own
  // declared frame count and pacing interval. So a gateway at ~1.0 of it forwarded every frame as it
  // arrived, which is the best outcome a proxy of a paced upstream can have. That case used to publish
  // nothing: 13 cells lost this metric on the 2026-07-28 board.
  //
  // The zero-note comes from seal.mjs's zeroNoteFor, the ONE place that mapping lives, so the note the
  // independent oracle expects and the note the seal writes cannot be different notes.
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
  // NO cpu_fps. Retired: of the 16 cells that published both it and `streams_sustained_fps` (the rate
  // at the PROVEN delivery boundary), 4 had it INVERTED below that boundary, 5 were redundant within 1%,
  // and 7 were measured at a concurrency where the delivery gate did not hold - a frame rate recorded
  // while dropping frames. See the engine's `run.rs` for the per-gateway numbers.
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
      // THE CASE THIS GUARD MISSED: stream_served is `true`/`false`/a status string ("not_measured",
      // "untestable", "not_probed" - StreamServed's real shape, record.rs), and in real field data it
      // is almost NEVER the literal boolean true - a non-streaming or untestable cell still carries a
      // raw stream object with its own *_mock_bound siblings. Gating the seal on `=== true` left every
      // other case's raw engine object (and its unconsumed _mock_bound flags) in the bundle untouched
      // (invariant C1). Seal whenever a stream object exists at all; sealStreamRecord/sealMetric are
      // already null-safe, and downstream readers (app.js) still gate display on stream_served === true
      // themselves, so preserving the real value here changes nothing they show.
      if (cell.stream && typeof cell.stream === "object") {
        cell.stream = {
          stream_served: cell.stream.stream_served,
          stream_error: cell.stream.stream_error ?? null,
          // THE EVIDENCE FOR THE STATUS, WHICH THE REBUILD WAS DROPPING. `stream_served` is a token
          // (`true`, any Absent token as a string, "not_probed"; `false` is legacy parse-only),
          // `reason` is the MACHINE token for why it came out that way (the same Absent vocabulary the
          // envelopes carry) and `stream_error` is the PROSE behind it; `stream_c1_note` is the c=1
          // leg's own note. Rebuilding the object from a fixed key list silently deleted the reason and
          // the c1 note - seven reasons and five c1 notes in the recovered 2026-07-29 snapshots - so the
          // bundle published a refusal with its explanation removed. A cell whose gap figures measured
          // now publishes stream_served:true WITH a reason token for the TTFT leg, which makes carrying
          // the pair load-bearing rather than decorative. None of the three is a sealed metric (they are
          // prose/vocabulary about the cell, not numbers), so they ride as plain fields; app.js renders
          // the prose and maps the token through its own note vocabulary, never printing it verbatim.
          reason: cell.stream.reason ?? null,
          stream_c1_note: cell.stream.stream_c1_note ?? null,
          ...sealStreamRecord(cell.stream, cell.absences),
        };
      }
      // PER-CELL MEMORY: its own cold-started, plateau-terminated window per cell. The memory tab reads
      // these directly off the matrix cell (Min/Max/Same/Custom), so every published number on them must
      // be an envelope like every other metric, sealed BY DISCOVERY (any RSS field) plus the non-RSS
      // memory metrics the vocabulary names (growth rate, time to plateau).
      if (cell.memory && typeof cell.memory === "object") {
        for (const k of Object.keys(cell.memory))
          if (isMetricField(k))
            cell.memory[k] = sealMetric(cell.memory[k], { absent: absentEntryFor(cell.absences, "memory", k) });
      }
    }
  }
  // A LEGACY top-level memory block (pre-per-cell results, no longer read by anything) still travels in the bundle (embedded
  // in g.matrix + the snapshot); seal its metrics so no bare ungated field survives.
  // Sealed BY DISCOVERY (audit #11): every `*_rss_mib` key present, not a 3-key whitelist that the
  // producer already outgrew (peak_rss_hwm_mib / post_load_rss_mib were shipping as BARE scalars).
  // THE SAME VOCABULARY AND THE SAME ABSENCES AS A PER-CELL WINDOW, because it is the same kind of
  // block. Testing only RSS_FIELD_RE here re-created the narrower-whitelist bug one level up: the
  // non-RSS memory metrics (growth rate, time to plateau) shipped as bare scalars off a legacy block,
  // and passing `{}` for the options threw away the engine's absence reason, so a legacy
  // below-resolution or untestable hole flattened to "not_measured" on reseal.
  if (m.memory && typeof m.memory === "object") {
    for (const k of Object.keys(m.memory))
      if (isMetricField(k))
        m.memory[k] = sealMetric(m.memory[k], { absent: absentEntryFor(m.absences, "memory", k) });
  }
}

// addedP99Rank(rec): the rank a candidate cell's added-latency p99 sorts by (lower is better).
// A measured number ranks as itself. A null whose absences entry says "below_resolution" ranks as 0:
// the comparison RAN and the difference was at or under what the rig can resolve, which is the
// engine's BEST outcome, not a hole - Infinity here turned a win into a last-place sort (and, in
// translationCell, let the legacy xlate fallback shadow a measured matrix cell). Any other null
// (not measured, suppressed, ...) still sorts last. `rec` is a candidate record carrying the raw
// perf fields plus the cell's `absences` map, so absentEntryFor resolves the block-prefixed key.
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
    // ADDED LATENCY RANKS THIS CHOICE; IT DOES NOT QUALIFY THE CELL.
    //
    // It used to be a precondition, and that quietly deleted whole rows. A gateway with a measured
    // throughput curve but no c=1 latency reading produced NO best_cell, so its entire Peak row read
    // n/a - while Same mode, which reads the cell directly, showed the same numbers fine. One-API
    // published 40 rps @ c16 and Plano 85 @ c512 on the 2026-07-29 board and both vanished from Peak,
    // which is the board disagreeing with itself about a cell it measured.
    //
    // A cell qualifies by having been SERVED and carrying perf. Latency then orders the candidates,
    // and a cell without one sorts last rather than being struck from the list.
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
// RPS is capacity-bound and noisier). Fixing ingress to openai keeps the input side identical across
// gateways; the egress varies and is shown as the row's path pill. Returns {ingress:"openai", egress,
// ...perf} or null when the gateway serves no openai-in translation path.
// The MATRIX WINS whenever it measured ANY translation cell, not only an openai-ingress one: a gateway
// whose matrix measured translation in the other direction (anthropic in -> openai out) must not fall
// through to the legacy xlate suite's stale number just because it lacks the fair-tier direction.
// Selection is two tiers: the FAIR tier first (openai ingress, identical input side across gateways),
// and only when the matrix has none of those, ANY served cross-dialect cell it did measure. The legacy
// fallback fires only when the matrix genuinely has NO translation cell at all.
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
// freshness). `latest` above still folds all suites for the future-date corruption hard-fail (:324) -
// a future-dated legacy stamp is still corruption - but the PUBLISHED footer uses the displayed basis.
const latestDisplayedMs = Math.max(...gateways.map(displayedMeasuredMs), 0);
const latestDisplayed = latestDisplayedMs > 0 ? new Date(latestDisplayedMs).toISOString() : null;

// The newest embedded measurement across a gateway's own suites, in epoch ms (0 when it has none).
function newestMeasuredMs(g) {
  return Math.max(...SUITES.map((s) => g[s] && g[s].measured_at).filter(Boolean).map((a) => Date.parse(a)).concat([0]));
}

// MED-1 / MED-2: the stamp that actually drives what the board DISPLAYS, in epoch ms (0 when none).
// EVERY displayed number now comes from g.matrix ONLY (best_cell / streaming / the per-cell memory windows), so the
// board-level freshness footer AND the wholesale-stale hard-fail must age those displayed numbers - the
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
// Compare as parsed epoch ms, not raw ISO strings: a lexicographic `generatedAt < latest` is only
// correct when both are the SAME ISO precision/zone, and a fractional-second vs whole-second mismatch
// can mis-order two instants that are microseconds apart.
if (latest && Date.parse(generatedAt) < Date.parse(latest)) {
  throw new Error(`gen-data: generated_at ${generatedAt} predates the newest embedded measured_at ${latest}; ` +
    `a raw result is future-dated (rig clock skew?). Refusing to emit a bundle that would read stale.`);
}

// FRESHNESS MODEL: a gateway's ENTIRE benchmark is ONE atomic matrix run that legitimately takes HOURS
// (busbar ~5h), and gateways publish INDEPENDENTLY (one gateway's row can be hours old while another's
// is weeks old). There is no intra-row span hard-fail beyond a generous sanity cap
// (MAX_ROW_SPAN_SANITY_H, far above any real run) that exists only to catch a corrupt/future-dated
// timestamp, and no cross-gateway lag check: on a living board with per-gateway cadences, differing
// measured_at values across gateways are honest and expected.
// The wholesale-stale ABSOLUTE floor (soft anchor): if the newest measurement ANYWHERE on the board is
// older than MAX_BOARD_AGE_DAYS, the WHOLE board is stale and the bundle must not publish
// generated_at=now over it.
// A PER-GATEWAY absolute age SIGNAL: a gateway whose own newest measurement is older than
// MAX_GATEWAY_AGE_DAYS gets a per-row `stale` flag (drives the app.js badge), not a build failure.
const MAX_ROW_SPAN_SANITY_H = 12;  // sanity-only: one atomic matrix run is hours; >12h means a corrupt/skewed stamp
const MAX_GATEWAY_AGE_DAYS = 60;   // per-gateway staleness SIGNAL (badge), never a build failure
const MAX_BOARD_AGE_DAYS = 180;    // wholesale-stale floor (soft anchor): the whole board older than this = hard fail
// MED-2: base the wholesale-stale floor on the DISPLAYED (matrix-preferring) stamps, not the max across
// all suites. Otherwise a single untouched results/perf/<gw>.json re-run yesterday makes boardNewest =
// yesterday while every DISPLAYED matrix number is 179d old - the 180-day hard-fail (the one absolute
// guard the rewrite KEPT) never fires and a wholesale-stale matrix board publishes generated_at=now.
// The floor now ages exactly what the board shows (same basis as MED-1's footer + the per-row badge).
const boardNewest = Math.max(...gateways.map(displayedMeasuredMs), 0);
// A BOARD WHOSE AGE CANNOT BE ESTABLISHED IS NOT A FRESH BOARD.
//
// This guard only ran `if (boardNewest > 0)`, so the one situation it could not judge - no gateway
// carrying a resolvable displayed stamp - was the one it let straight through. That is the same
// shape as every other guard that turned out to be doing nothing today: the retry budget nothing
// called, the box qualification that always seeded, the history appender scanning directories the
// engine stopped writing. Each was silent rather than wrong, which is why none of them were noticed.
//
// Publishing generated_at=now over a board we cannot date is exactly what the 180-day floor exists
// to refuse, so an unresolvable board is a hard failure with its own reason rather than a pass.
//
// BUT: "UNDATABLE" AND "EMPTY" ARE NOT THE SAME BOARD, AND ONLY ONE OF THEM IS A DEFECT.
//
// The guard as first written asked only "is boardNewest 0", which is true of BOTH a board carrying
// numbers nobody can date AND a board carrying nothing at all. Those are opposite situations. A board
// that publishes measurements with no resolvable stamp is genuinely undatable and must not ship - that
// is the case this guard was written for. A board where NO gateway has been benchmarked yet is not
// ambiguous in the slightest: nothing has been measured, there is nothing to date, and the honest
// bundle is one that says so on every row. app.js and the site suite already treat that as a first-
// class state and say so at length (BOARD_HAS_DATA, testWithData, testWithMatrixDonor).
//
// Conflating the two cost more than a bad error message. A clean checkout has an empty
// results/snapshots/ - no artifacts are committed - so gen-data THREW on every fresh clone, which
// meant `node site/test.mjs` died at its own line ~167 (it runs gen-data for real into a temp dir)
// before reaching a single assertion: zero ok lines, zero FAIL lines, exit non-zero for a reason that
// had nothing to do with the code under test. Its documented fallback to a committed site/data.json
// cannot rescue it either, because that file is gitignored. So the whole site suite gated nothing in
// CI, and the deploy workflows failed three steps before they ever reached a site test.
//
// The distinction is made on EVIDENCE OF MEASUREMENT, not on gateway count: a gateway is "measured" if
// it carries any suite artifact or any projected record at all. If even one does and the board still
// cannot be dated, the original hard failure stands, exactly as strict as before.
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
  // trip the >12h cap and abort the deploy - defeating incremental publish. The matrix is the single
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
  // only. HONEST SCOPE: this is DEFENSE-IN-DEPTH, not a live guard. ANY future stamp (matrix OR legacy -
  // the loop at :306 folds every suite into `latest`) already trips the board-wide future-date hard-fail
  // at :343, which THROWS before this line is reached, so no bad badge can ship today. This re-derivation
  // only becomes reachable if that :343 hard-fail is ever weakened/removed; it is retained as a belt-and-
  // suspenders backstop for the per-row badge, NOT because a live path reaches it.
  if (sawFuture) g.measured_at = ats.length ? new Date(Math.max(...ats)).toISOString() : null;
  // LOW-2: a served matrix row whose DISPLAYED numbers project from g.matrix (best_cell / streaming /
  // translation_cell, source:"matrix", or a per-cell memory window) but that carries NO valid (non-future) matrix
  // measured_at is CORRUPT: run.sh:1145 ALWAYS writes measured_at via `date -u`, so a null/absent matrix
  // stamp is only reachable via truncation / hand-edit / producer bug. Left unguarded such a row bypasses
  // EVERY freshness guard - best_cell still projects and ranks, `latest` (:306) skips it so the future-
  // date hard-fail never sees it, displayedMeasuredMs returns 0 so it is exempt from the 180d floor, and
  // ats.length<1 hits the `continue` below BEFORE g.stale is set, so it publishes FRESH with no badge. A
  // stamp-less served matrix row must never publish clean: flag it stale (drives the app.js badge) and
  // warn, consistent with treating a null matrix stamp as corruption rather than a legitimate reading.
  // Memory joins this test through the matrix cells it is read from, not through a projected record:
  // a matrix carrying per-cell windows displays numbers just as much as one carrying a best_cell, so a
  // stamp-less matrix must be caught either way.
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
  // NIT-R2-2 / NIT-R5-2: INERT PLACEHOLDER - dead code today, NOT a live safeguard. A matrix row carries
  // at most ONE matrix stamp, so matrixSpanAts is length 0 or 1 and the `>= 2` gate below NEVER fires;
  // it performs no cross-timestamp check on any current data. The live corruption/freshness guards are
  // the board-wide future-date hard-fail (:343) and the 180-day wholesale-stale floor (:376) - NOT this
  // block. It is retained (null-safe, matrix-scoped) purely so the span check reactivates automatically
  // should a future matrix result ever embed multiple internal timestamps; until then it is a no-op.
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
  // threshold so app.js can show a "stale" badge. A living board with mixed cadences is fine.
  // LOW-R3-3: age the DISPLAYED numbers - matrix.measured_at when present (best_cell/streaming/memory
  // all project from it), so a stale matrix is flagged even if a newer legacy suite stamp is around;
  // fall back to the newest non-future suite stamp only for a legacy-only row (no matrix).
  const ageBasisMs = matrixAt != null ? matrixAt : Math.max(...ats);
  const ageDays = (nowMs - ageBasisMs) / 86400000;
  g.stale = ageDays > MAX_GATEWAY_AGE_DAYS;
}

// ---- NO CHART PNGs ----------------------------------------------------------
// This copied results/*.png into site/charts/ and listed them in `data.json.charts` so the board
// could render 25 pre-drawn images. The Charts tab draws from the board itself now, at the bound
// and cell the reader selected, so there is no PNG to ship and no list to keep in sync. The key is
// REMOVED rather than emitted empty: `charts: []` would tell a future reader the board has charts
// and found none this run, which is a different and untrue statement.
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
// THE BOARD'S OWN ENGINE, and each row's, surfaced for the reader.
//
// The engine commit already travelled into every row inside `rig.engine.commit`; nothing rendered
// it, so "which harness measured this" was knowable only by opening the JSON. A row measured by an
// older engine is not necessarily wrong, but it is not comparable to the rest, and a reader deciding
// whether to trust a comparison needs to see that without being told.
//
// The board's version is the engine of the most recently measured row, which is the one a re-run
// moves forward. A row whose engine differs from it is marked, and the site renders that in red.
const engineOf = (g) => (g && g.rig && g.rig.engine && g.rig.engine.commit) || null;
const newestRow = gateways
  .filter((g) => displayedMeasuredMs(g) > 0)
  .sort((a, b) => displayedMeasuredMs(b) - displayedMeasuredMs(a))[0];
const boardEngine = engineOf(newestRow);
for (const g of gateways) {
  const sha = engineOf(g);
  g.engine = sha
    ? { sha, short: sha.slice(0, 7), current: boardEngine == null || sha === boardEngine }
    // A row with no engine stamp predates the stamp entirely; saying so is better than implying it
    // matches, and better than omitting the field so the render site has to guess.
    : { sha: null, short: null, current: false };
}

// METRIC DEFINITIONS FOR THE BOARD (task: project data.definitions): the engine's own prose for what
// each metric is, generated from its own constants (engine/src/suite.rs metric_definitions) so the
// definition displayed can never drift from the enforcement it describes - see the p99<1s-vs-20ms
// history this exists to foreclose. Sourced ONLY from rows measured by boardEngine: the board's numbers
// are boardEngine's numbers (per-row `current` above), and a definitions map from a DIFFERENT engine
// may describe different constants than the ones that produced what is on screen - publishing it would
// be the exact mislabelling class this field exists to prevent. A board with no boardEngine (nothing
// measured) or where none of boardEngine's rows carry a snapshot new enough to have the field yet
// publishes NO definitions rather than guess: absent must mean absent (old snapshots predate the
// field), and there is no honest substitute for "the current engine hasn't told us yet".
let definitions = null;
if (boardEngine != null) {
  for (const [key, defs] of snapshotDefinitionsByKey) {
    if (engineOf(gateways.find((g) => g.key === key)) !== boardEngine) continue;
    if (definitions == null) { definitions = { ...defs }; continue; }
    // TWO ROWS ON THE SAME ENGINE COMMIT DISAGREEING ABOUT WHAT A METRIC MEANS IS NOT A DATA
    // CONDITION TO SMOOTH OVER - the definitions are generated from the engine BINARY's own
    // constants, so two runs of the identical commit must produce byte-identical prose. Divergence
    // here means the definitions were hand-edited, generated by a dirty tree stamped as the clean
    // commit, or some other corruption - silently picking one would publish a definition that may
    // not describe the metric the reader is looking at, which is the defect this field exists to
    // remove. Refuse loudly, matching how this file treats every other "cannot publish honestly"
    // state (e.g. the degraded-snapshot and mismatched-config-pointer throws above).
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
  // WHICH HARNESS MEASURED THE BOARD, as a thing a reader can see rather than a thing a build guard
  // knows privately. C8 refuses to publish a board whose columns were measured by different engines,
  // which is the right call for a ranking - but it made the engine invisible when it passed, so a
  // reader had no way to tell WHICH harness produced the numbers or whether a row was measured by an
  // older one. The board states its own engine, and every row states the engine that measured it.
  benchmark_version: boardEngine,
  repo: "https://github.com/GetBusbar/benchmarking",
  gateways,
};
// Omit the key entirely rather than publish `{}` when no boardEngine row has it yet: app.js reads
// `data.definitions` null-safely, and an empty object is indistinguishable from "every metric here
// has no definition", which is not the honest reading of "the engine hasn't told us yet".
if (definitions != null) data.definitions = definitions;
// C1: strip the raw legacy suite objects from the EMITTED bundle. They were projection INPUTS (their
// values are now sealed into best_cell/translation_cell/streaming), and they carry raw scalars + their
// _mock_bound flags - a reservoir no surface reads any more (charts.py projects from the canonical
// records; app.js reads envelopes). Removing them from the artifact is what makes "no ungated metric
// field exists in the bundle" true. g.matrix stays (its cells are sealed in-place; its top-level
// build/measured_at/p99_ceiling_ms/sweep_dur drive freshness + the sweep-integrity oracle).
for (const g of gateways) {
  for (const suite of ["perf", "stream", "streamcpu", "xlate"]) delete g[suite];
}
/* THINNING THE RSS SERIES FOR THE WIRE.
   The bundle reached 14.1 MB, of which 5.2 MB was 156,506 raw RSS samples, and the browser paid to
   decompress and parse every one of them before the first row appeared. They feed one thing: a
   sparkline about fifty pixels wide. Nothing re-derives a value from them - every memory verdict
   (steady state, growth rate, time to plateau, shape) is decided in the engine from the FULL series
   and published as its own field, and the full series stays in the snapshot artifacts, which are what
   an auditor reads. This is a drawing resolution, not a measurement.

   Decimation is MIN/MAX PRESERVING per bucket, not "every Nth sample". Taking every Nth is how a
   thinned curve loses the spike that was the whole finding: a gateway that touched 900 MiB for three
   seconds would draw flat if those three seconds fell between strides. Keeping both extremes of every
   bucket, in the order they occurred, means the drawn envelope still touches every high and low the
   full series reached. The first and last samples are always kept so the window's own boundaries -
   which the load/recovery rule reads - never move. */
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
