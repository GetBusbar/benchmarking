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
import { sealMetric, makeSource, SWEEP, UNGATED_LAT_FIELDS, UNGATED_STREAM_FIELDS, RSS_FIELD_RE, isMetricField, ZERO_MEASURED_FAIL } from "./seal.mjs";

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

// GitHub star snapshot for the Gateways overview: a COMMITTED build-time file
// (gateways/stars.json, refreshed by `node gateways/fetch-stars.mjs`), never a live
// API call, so the bundle stays reproducible and CI needs no network. Absent file or
// absent key degrades to null; the site renders those muted.
const starsSnap = readJson(join(gatewaysDir, "stars.json")) || {};

const gateways = gatewayKeys.map((key) => {
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
  const snap = newestSnapshot(key);
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
      const degraded = snapshotDegradedMode(snap.matrix);
      const diskFull = g.matrix && !snapshotDegradedMode(g.matrix);
      if (degraded && diskFull) {
        throw new Error(
          `gen-data: REFUSING to publish ${key} from a DEGRADED-MODE snapshot. ${snap.__file} ` +
          `(measured_at ${snap.measured_at}) ran with ${degraded} - the phases were switched OFF, so it is a ` +
          `local smoke run, not a measurement - yet it is NEWER than results/matrix/${key}.json ` +
          `(measured_at ${g.matrix.measured_at}), which ran them all. Publishing it would replace a complete ` +
          `run with a probe-only one and the board would show it as this gateway's result.\n` +
          `  Fix: delete or move that snapshot out of results/snapshots/ (a local verify-local run with ` +
          `KEEP_ARTIFACTS=1 leaves it behind; without KEEP_ARTIFACTS the teardown's git clean removes it).`);
      }
      g.matrix = snap.matrix;                                      // matrix from the snapshot (sole source)
      g.matrix_from_snapshot = true;
      g.snapshot_file = snap.__file ?? null;                       // which archived run the board renders
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
    // Per-cell perf (matrix v2 + sweep): the gateway's BEST green diagonal by sustained RPS @20ms.
    const bc = bestCell(g.matrix);
    if (bc) g.best_cell = sealPerfCell(bc, { ingress: bc.dialect, egress: bc.dialect, dialect: bc.dialect },
      makeSource("matrix", SWEEP.DIAGONAL, build, at));
    // The gateway's TRANSLATION cell (openai in -> best non-openai egress).
    const tc = translationCell(g.matrix);
    if (tc) g.translation_cell = sealPerfCell(tc, { ingress: tc.ingress, egress: tc.egress },
      makeSource("matrix", SWEEP.TRANSLATION, build, at));
    // STREAMING projection (matrix single source): the BEST DIAGONAL cell's streaming - the SAME
    // (ingress==egress) cell the headline perf is projected from (one source of truth). Only when the
    // diagonal ACTUALLY STREAMED (stream_served===true); a non-streaming cell leaves g.streaming absent.
    if (bc) {
      const cell = g.matrix.upstreams?.[bc.dialect]?.cells?.[bc.dialect];
      if (cell && cell.stream && cell.stream.stream_served === true) {
        g.streaming = sealStreaming(cell.stream, bc.dialect, makeSource("matrix", SWEEP.STREAM_DIAGONAL, build, at));
      }
    }
    // MEMORY: NOT projected. Memory is measured per cell (its own cold-started, plateau-terminated
    // window on EVERY served cell) and stays per cell all the way to the reader - the board's memory lane
    // chooses a cell through the same chooser every other lane uses (Min | Max | Same | Custom) and says
    // which. A per-gateway scalar cannot exist without the harness selecting a cell silently, which is
    // exactly the defect the per-cell design removes. The windows are sealed in place below.
    // SEAL every matrix cell in-place (AFTER selection/projection, which read raw). The matrix popup +
    // Protocol view read cell.perf / cell.stream directly, so those must be envelopes too - otherwise a
    // raw ungated scalar (and its _mock_bound flag) survives in the bundle (invariant C1).
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
    // Legacy xlate suite (anthropic in -> openai out - the OPPOSITE direction of the matrix cell).
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
});

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
function sealPerfCell(perf, path, source) {
  return { path: { ...path }, source, ...sealPerfCellPerf(perf) };
}
// sealStreamRecord: a raw stream record -> {<sealed metrics>} (no path/source). TTFT/gap are UNGATED;
// streams_sustained + cpu_fps are GATED on their mock-bound flags. Used for the canonical g.streaming AND
// for sealing every matrix cell's own .stream in-place (so the popup reads envelopes).
function sealStreamRecord(s) {
  const rec = {};
  for (const k of UNGATED_STREAM_FIELDS) if (s[k] != null) rec[k] = sealMetric(s[k], {});
  // AUDIT #11: streams_sustained_fps is the SAME bisect's rate - it inherits that bisect's mock-bound
  // honesty flag. Sealing it UNGATED beside a GATED streams_sustained let the rig-bound rate publish
  // while the count it came from was suppressed. Gate it on the same flag.
  rec.streams_sustained_fps = sealMetric(s.streams_sustained_fps, {
    gated: true, paced: true, flag: s.streams_sustained_mock_bound, zeroNote: ZERO_MEASURED_FAIL });
  // AUDIT #3: streaming counts - a 0 is a MEASURED FAILURE (offered stream load, sustained none), NOT
  // "not measured". Only a null (absent field) is not-measured. The note names which, and every surface
  // renders the two apart.
  rec.streams_sustained = sealMetric(s.streams_sustained, {
    gated: true, paced: true, flag: s.streams_sustained_mock_bound, zeroNote: ZERO_MEASURED_FAIL });
  rec.cpu_fps = sealMetric(s.cpu_fps, { gated: true, paced: true, flag: s.cpu_fps_mock_bound, zeroNote: ZERO_MEASURED_FAIL,
    extras: { concurrency: s.cpu_fps_concurrency ?? null } });
  return rec;
}
function sealStreaming(s, dialect, source) {
  return { path: { dialect }, source, stream_served: true, ...sealStreamRecord(s) };
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
      if (cell.perf) cell.perf = sealPerfCellPerf(cell.perf);
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
          ...sealStreamRecord(cell.stream),
        };
      }
      // PER-CELL MEMORY: its own cold-started, plateau-terminated window per cell. The memory tab reads
      // these directly off the matrix cell (Min/Max/Same/Custom), so every published number on them must
      // be an envelope like every other metric, sealed BY DISCOVERY (any RSS field) plus the non-RSS
      // memory metrics the vocabulary names (growth rate, time to plateau).
      if (cell.memory && typeof cell.memory === "object") {
        for (const k of Object.keys(cell.memory))
          if (isMetricField(k)) cell.memory[k] = sealMetric(cell.memory[k], {});
      }
    }
  }
  // A LEGACY top-level memory block (pre-per-cell results, no longer read by anything) still travels in the bundle (embedded
  // in g.matrix + the snapshot); seal its RSS scalars so no bare ungated field survives.
  // Sealed BY DISCOVERY (audit #11): every `*_rss_mib` key present, not a 3-key whitelist that the
  // producer already outgrew (peak_rss_hwm_mib / post_load_rss_mib were shipping as BARE scalars).
  if (m.memory && typeof m.memory === "object") {
    for (const k of Object.keys(m.memory)) if (RSS_FIELD_RE.test(k)) m.memory[k] = sealMetric(m.memory[k], {});
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
      if (!(cell && cell.served === true && cell.perf && cell.perf.added_latency_p99_us != null)) continue;
      const rec = { ingress, egress, ...cell.perf };
      if (ingress === "openai" && egress !== "openai") fair.push(rec);
      any.push(rec);
    }
  }
  const cands = fair.length ? fair : any;
  if (!cands.length) return null;
  return cands.reduce((a, b) => (b.added_latency_p99_us < a.added_latency_p99_us ? b : a));
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
if (boardNewest <= 0 && gateways.length > 0) {
  throw new Error(
    `gen-data: FRESHNESS FAILURE (undatable board): ${gateways.length} gateway(s) but not one carries a resolvable ` +
    `displayed measured_at, so the board's age cannot be established at all. Refusing to publish ` +
    `generated_at=${generatedAt} over data that cannot be dated.`);
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
  charts,
};
// C1: strip the raw legacy suite objects from the EMITTED bundle. They were projection INPUTS (their
// values are now sealed into best_cell/translation_cell/streaming), and they carry raw scalars + their
// _mock_bound flags - a reservoir no surface reads any more (charts.py projects from the canonical
// records; app.js reads envelopes). Removing them from the artifact is what makes "no ungated metric
// field exists in the bundle" true. g.matrix stays (its cells are sealed in-place; its top-level
// build/measured_at/p99_ceiling_ms/sweep_dur drive freshness + the sweep-integrity oracle).
for (const g of gateways) {
  for (const suite of ["perf", "stream", "streamcpu", "xlate"]) delete g[suite];
}
writeFileSync(join(OUT, "data.json"), JSON.stringify(data, null, 1) + "\n");
console.log(`gen-data: ${gateways.length} gateways, ${charts.length} charts -> ${join(OUT, "data.json")}`);
