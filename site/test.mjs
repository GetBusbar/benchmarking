#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// test.mjs: node smoke test for the results site. No dependencies, no browser:
// app.js exports its pure logic (filtering, URL codec, sweep chart) when run
// under node, and the canvas is exercised through a recording 2d-context stub.
//
//   node site/test.mjs
//
// Covers: gen-data emits GW_CLASS for every gateway; search/capability filtering
// (the class/lang chip rows are retired; stale params must be ignored); path-URL
// state round-trip (/<category>/<view>?<params>) including legacy-hash decoding
// and the HOME landing page at the site root; the sweep chart component drawing
// real committed sweep data through the stub canvas.

import { execFileSync } from "node:child_process";
import { snapshotCellCoords, isStrictSubset, layerScopedMatrix } from "./snapshots.mjs";
import { readFileSync, writeFileSync, mkdtempSync, mkdirSync, rmSync, existsSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import assert from "node:assert/strict";
import { checkConsistency, c6Inversions, c7HwmBelowPeak, hasCellMemory } from "./check-consistency.mjs";
import * as checkMod from "./check-consistency.mjs";
import { sealMetric, displayedValue, THROUGHPUT_FIELDS, isMetricField, zeroNoteFor, ZERO_NO_CEILING, ZERO_MEASURED_FAIL,
  FRONTIER_BOUNDS_MS, DEFAULT_BOUND_MS, sealFrontier } from "./seal.mjs";
import { oracleExpected } from "./check-consistency.mjs";
// A HAND-BUILT fixture has no results/matrix/<key>.json oracle, so it needs an EXPLICIT opt-in
// (the CLI never passes it) to waive the oracle-verifiable requirement.
const SYNTH = { syntheticFixture: true };

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const app = createRequire(import.meta.url)(join(HERE, "app.js"));

let passed = 0;
// The runner CATCHES a failing test, records it, and keeps going, then exits non-zero at the end with
// every failure listed: a throwing test aborting the file at that line would silently skip every test
// after it, which is the worst failure mode for a suite whose job is to be the gate. Ordering must not
// decide coverage.
const failures = [];
// A SKIP IS NOT A PASS AND MUST NOT BE PRINTED AS ONE.
//
// The gated families below (empty board, no matrix donor, board still filling) returned early from
// INSIDE the test body, so the runner saw a function that did not throw and printed `ok - <name>` for
// a check that never executed a single assertion. On an empty board that is most of this suite reading
// green, per-test, with only a file-level warn banner to say otherwise - and the whole point of a suite
// that gates a publish is that its per-test output can be read as evidence. A skip now says skip, is
// counted apart from passes, and is summarised at the end.
//
// EXIT SEMANTICS ARE UNCHANGED: a skip is not a failure, so it does not set a non-zero exit. The suite
// still exits non-zero if and only if something actually failed.
const skipped = [];
function test(name, fn) {
  try {
    fn();
    passed += 1;
    console.log(`ok - ${name}`);
  } catch (e) {
    failures.push({ name, e });
    console.error(`FAIL - ${name}\n      ${(e && e.message ? String(e.message) : String(e)).split("\n").join("\n      ")}`);
  }
}
function skip(name, why) {
  skipped.push({ name, why });
  console.log(`skip - ${name}  # ${why}`);
}
process.on("exit", () => {
  if (skipped.length) {
    console.warn(`\n${skipped.length} SKIPPED test(s) (not run, not passed):`);
    for (const s of skipped) console.warn(`  - ${s.name}  # ${s.why}`);
  }
  if (!failures.length) return;
  console.error(`\n${failures.length} FAILING test(s):`);
  for (const f of failures) console.error(`  - ${f.name}`);
  process.exitCode = 1;
});

// ---- sealed-envelope fixture helpers (mirror seal.mjs / gen-data) ------------
// Every metric in the bundle is a SEALED ENVELOPE. These builders take RAW intent (the value, plus the
// two facts the engine publishes about the comparison it was taken in) and produce the exact envelope
// shape gen-data emits, so fixtures stay readable while exercising the real reader (app.metric/mval).
//
// The raw intent USED to be (value + `*_mock_bound`), the engine's verdict that our own rig had set the
// limit, and a GATED metric was certified only when that verdict was `false`; a positive value with a
// `true` or `null` flag was published as {value:null, suppressed:true}. That is gone: a present number is
// always published, and the builders now take `*_headroom` (the fraction of the rig's own ceiling the
// measurement reached) and `*_rig_ceiling` / `*_mock_ceiling` (the ceiling that fraction is of), which
// ride ON the certified envelope for the reader to weigh. See seal.mjs.
//
// A measured 0 is still certified, and its NOTE names which zero it is: RPS ceilings note
// ZERO_NO_CEILING, streaming counts note ZERO_MEASURED_FAIL - a measured failure, never folded into
// "not measured" (#3). The fixtures seal through the REAL exported sealMetric(), so `seal` IS the choke
// point under test: a fixture builder here can never drift from what seal.mjs actually does.
const seal = sealMetric;
const SRC = (kind, sweep) => ({ kind, sweep, build: "img:1", measured_at: "2026-07-24T00:00:00Z" });
/* ---- the frontier, as a fixture --------------------------------------------------------------------
   THE RETIRED THROUGHPUT INTENTS ARE GONE from these builders: `rps_sustained_20ms`, `rps_max_proxy`,
   their headroom/ceiling/concurrency siblings and their two sweep arrays. No producer emits any of them
   (engine/src/frontier.rs replaced the pair with one sweep read at each declared bound), so a fixture that
   still offered them would let a test assert on a shape the board can never receive - which is exactly how
   the retired scalars' captions came to describe a test that never ran.
   The intent is now the CURVE, because the curve is the finding:
     frontier: 30000                          - flat: the same rate at every published bound
     frontier: {1: 7015, 5: 15438, none: ...} - a real shape; a slot left out is a bound with NO qualifying
                                                rung, which the engine omits from the array entirely
     frontier: null                           - no frontier at all (a record measured before it existed)
   A slot whose value is null is a reading whose RATE is absent, carrying the engine's own reason. */
const FRONTIER_SLOTS = [1, 5, 10, 50, 100, "none"];
function fxFrontier(spec = 30000, o = {}) {
  if (spec == null) return [];
  const flat = typeof spec === "number";
  const slots = flat ? FRONTIER_SLOTS : FRONTIER_SLOTS.filter((k) => Object.prototype.hasOwnProperty.call(spec, k));
  return slots.map((k) => {
    const v = flat ? spec : spec[k];
    const isLower = o.lowerBound === true || (Array.isArray(o.lowerBound) && o.lowerBound.includes(k));
    return {
      bound_ms: k === "none" ? null : k,
      // Sealed through the REAL sealMetric, so a fixture reading can never carry an envelope shape the
      // producer would not emit. An absent rate carries the engine's own absence reason.
      rps: seal(v, { absent: v == null ? (o.absent || { reason: "below_resolution", detail: "every cleanly-served rung had a tail latency at or above this bound" }) : null }),
      concurrency: v == null ? null : (o.conc ?? 512),
      // THE OBSERVED TAIL, not the bound: 40% of it, so a fixture never accidentally asserts that a reading
      // sat exactly on its own bound (which would not have qualified - the comparison is p99 < bound).
      p99_us: v == null ? null : (o.p99_us ?? (k === "none" ? 40_000 : k * 400)),
      first_disqualified_conc: v == null || isLower ? null : (o.firstDisq ?? 1024),
      lower_bound: isLower,
    };
  });
}
// bcCell: a sealed best_cell (or same-dialect diagonal) from raw perf intent.
function bcCell(o = {}) {
  const {
    dialect = "openai", ingress = dialect, egress = dialect, kind = "matrix",
    sweep = (kind === "perf-fallback" ? "perf-suite" : ingress === egress ? "6x6-diagonal" : "6x6-translation"),
    added_latency_p50_us = 100, added_latency_p99_us = 110,
    frontier = 30000, frontierOpts = {}, sweepRungs = null,
  } = o;
  const rec = { path: { ingress, egress, ...(ingress === egress ? { dialect } : {}) }, source: SRC(kind, sweep) };
  if (added_latency_p50_us != null) rec.added_latency_p50_us = seal(added_latency_p50_us);
  if (added_latency_p99_us != null) rec.added_latency_p99_us = seal(added_latency_p99_us);
  rec.frontier = fxFrontier(frontier, frontierOpts);
  // The rungs every reading was taken from, as gen-data carries them: evidence, not a metric, so it is a
  // plain array on the record rather than an envelope.
  if (sweepRungs) rec.sweep = sweepRungs;
  return rec;
}
// tCell: a sealed translation_cell.
function tCell(o = {}) {
  const {
    ingress = "openai", egress = "anthropic", kind = "matrix",
    sweep = kind === "xlate-fallback" ? "xlate-suite" : "6x6-translation",
    added_latency_p50_us = null, added_latency_p99_us = 200,
    frontier = 3000, frontierOpts = {},
  } = o;
  const rec = { path: { ingress, egress }, source: SRC(kind, sweep) };
  if (added_latency_p50_us != null) rec.added_latency_p50_us = seal(added_latency_p50_us);
  if (added_latency_p99_us != null) rec.added_latency_p99_us = seal(added_latency_p99_us);
  rec.frontier = fxFrontier(frontier, frontierOpts);
  return rec;
}
// streamRec: a sealed streaming record (projected g.streaming, or a per-cell .stream when path omitted).
function streamRec(o = {}) {
  const {
    dialect = "openai", kind = "matrix", sweep = kind === "stream-fallback" ? "stream-suite" : "6x6-stream-diagonal",
    withPathSource = true,
    added_ttft_p50_us = 40, added_ttft_p99_us = 90, added_gap_p50_us = 5, added_gap_p99_us = 12,
    streams_sustained = 1300, streams_sustained_headroom = null, streams_sustained_mock_ceiling = null,
    streams_sustained_fps = 39000,
    cpu_fps = 48000, cpu_fps_headroom = null, cpu_fps_mock_ceiling = null, cpu_fps_concurrency = null,
  } = o;
  const rec = { stream_served: true };
  if (withPathSource) { rec.path = { dialect }; rec.source = SRC(kind, sweep); }
  const put = (k, v) => { if (v != null) rec[k] = seal(v); };
  put("added_ttft_p50_us", added_ttft_p50_us); put("added_ttft_p99_us", added_ttft_p99_us);
  put("added_gap_p50_us", added_gap_p50_us); put("added_gap_p99_us", added_gap_p99_us);
  rec.streams_sustained_fps = seal(streams_sustained_fps, {
    headroom: streams_sustained_headroom, ceiling: streams_sustained_mock_ceiling, zeroNote: ZERO_MEASURED_FAIL });
  rec.streams_sustained = seal(streams_sustained, {
    headroom: streams_sustained_headroom, ceiling: streams_sustained_mock_ceiling, zeroNote: ZERO_MEASURED_FAIL });
  rec.cpu_fps = seal(cpu_fps, {
    headroom: cpu_fps_headroom, ceiling: cpu_fps_mock_ceiling, zeroNote: ZERO_MEASURED_FAIL,
    extras: { concurrency: cpu_fps_concurrency } });
  return rec;
}
// memRec: a sealed memory_read record.
function memRec(o = {}) {
  const { idle_rss_mib = 40, peak_rss_mib = 900, recovered_rss_mib = null, load_cell = null, load_recipe = null, rss_series = null } = o;
  const rec = { source: SRC("matrix", "6x6-memory-window"), served: true,
    idle_rss_mib: seal(idle_rss_mib), peak_rss_mib: seal(peak_rss_mib), recovered_rss_mib: seal(recovered_rss_mib) };
  if (load_cell != null) rec.load_cell = load_cell;
  if (load_recipe != null) rec.load_recipe = load_recipe;
  if (rss_series != null) rec.rss_series = rss_series;
  return rec;
}
// A sealed per-cell matrix perf/stream (metrics-only, no path/source - as gen-data seals cells in place).
function cellPerf(o = {}) {
  const b = bcCell(o);
  const { path, source, ...rest } = b;
  return rest;   // { added_latency_*, rps_* } as envelopes
}
function cellStream(o = {}) { return streamRec({ ...o, withPathSource: false }); }

// ---- gen-data: run it for real into a temp dir ------------------------------
// Mid-refresh, the freshness guard hard-fails gen-data ON PURPOSE (a partial field re-run
// is exactly what it exists to block). That guard protects the PUBLISHED bundle; it must
// not also block testing the app logic. So: run gen-data for real when the raw results are
// coherent, and fall back to the committed site/data.json (the last bundle the guard
// accepted) when the guard trips. Any OTHER gen-data failure still fails the suite.
const out = mkdtempSync(join(tmpdir(), "site-test-"));
let data;
try {
  execFileSync(process.execPath, [join(HERE, "gen-data.mjs"), ROOT, out], { stdio: "pipe" });
  data = JSON.parse(readFileSync(join(out, "data.json"), "utf8"));
} catch (e) {
  const msg = String(e.stderr || e.message || "");
  if (!msg.includes("FRESHNESS FAILURE")) throw e;
  // site/data.json is GITIGNORED, so this fallback only exists on a machine that has generated one.
  // In CI it does not exist, and a bare readFileSync would replace the real, explanatory gen-data
  // failure with an ENOENT about a file nobody was looking for. Re-throw the original instead: the
  // reason the bundle could not be built is the finding, not the missing fallback.
  if (!existsSync(join(HERE, "data.json"))) {
    console.error("gen-data failed AND there is no committed site/data.json to fall back to; the original failure follows.");
    throw e;
  }
  console.warn("warn - raw results are mid-refresh (freshness guard tripped); testing against the committed site/data.json");
  data = JSON.parse(readFileSync(join(HERE, "data.json"), "utf8"));
} finally {
  rmSync(out, { recursive: true, force: true });
}

// ---- an EMPTY board is a legitimate state, not a broken one ------------------
//
// The consistency guard is deliberately anti-inert: it fails if its own invariant branches were never
// exercised, because a check that cannot fire is worse than no check at all. That would make "no
// results" an impossible state to publish, even though clearing every result to start a board over is a
// legitimate one.
//
// A board with no measurements has nothing to be inconsistent ABOUT. Every surface reads n/a, which is
// the honest rendering, and gen-data already produces it (13 gateways, no best_cell). So the real-bundle
// family below is vacuous here and says so out loud, while staying exactly as strict the moment a single
// gateway carries data. The n/a rendering itself is asserted unconditionally, so "empty" cannot become a
// hole that hides a broken board.
// "Carries data" must mean PUBLISHES A NUMBER, which is exactly check-consistency's own `matrixSourced`
// predicate (a projected best_cell / translation_cell / streaming record, or per-cell memory). The old
// `g.best_cell || g.matrix` also accepted a bare `g.matrix` OBJECT, which a gateway that failed to serve
// still carries (matrix.served=false, zero cells, no projected record). That mismatch is what made a board
// of only-failed-to-serve rows report BOARD_HAS_DATA=true and then fail 14 ways: R2 demanded the REQUIRED
// branches (C1.field, C1.certified, C4.cell) that only a published cell can exercise, and every RED
// self-test below TypeError'd on an undefined donor row. A gateway that served nothing published nothing
// and has nothing to be inconsistent about - the same reasoning the comment above already states ("gen-data
// already produces it (13 gateways, no best_cell)"). This narrows the predicate to match that stated intent;
// it does not weaken any assertion, because every branch it now skips is one the bundle cannot exercise.
const BOARD_HAS_DATA = (data.gateways || []).some((g) => g &&
  ([g.best_cell, g.translation_cell, g.streaming].some((r) => r && r.source) || hasCellMemory(g.matrix)));
if (!BOARD_HAS_DATA) {
  console.warn(`warn - the board carries no measurements (${(data.gateways || []).length} gateways, all n/a):`);
  console.warn("       the real-bundle consistency checks are vacuous and are reported as skipped.");
}
// Register a test that only runs against a populated board. On an empty one it is recorded as skipped
// rather than silently dropped, so the count never quietly shrinks.
const testWithData = (name, fn) =>
  (BOARD_HAS_DATA ? test(name, fn)
    : skip(name, "the board carries no measurements: no gateway publishes a number to be inconsistent about"));

// THE RED SELF-TESTS NEED A STRICTLY NARROWER THING THAN BOARD_HAS_DATA.
//
// Each of them reverts one seal on a clone of the real bundle and asserts the invariant catches it,
// so each needs a donor row publishing a MATRIX-SOURCED best_cell. `BOARD_HAS_DATA` is satisfied by
// any sourced record at all - streaming, translation, or per-cell memory - which is the right
// predicate for the checks that consume those. The two can therefore disagree, and the comment on
// `matrixGw` below already anticipated that they might.
//
// They did. Mid-run, the first gateway to land was one-api: it publishes streaming and memory but no
// matrix-sourced best_cell, so BOARD_HAS_DATA said "populated", every RED test ran, and all seven
// hard-failed on a precondition their own message correctly described as "board is dataless, not
// dishonest". A run publishes each gateway as it finishes, so a board with one thin row is a normal
// state for hours, not a defect - and it froze the entire site behind it.
//
// Skipping is only honest while the board is still filling. Once it carries every gateway the repo
// declares, a missing donor means something really did stop publishing, and these must run.
const BOARD_HAS_MATRIX_DONOR = (data.gateways || []).some(
  (g) => g && g.best_cell && g.best_cell.source && g.best_cell.source.kind === "matrix");
// A DONOR ROW IS NOT THE SAME THING AS A DONOR WITH SEVERAL SURFACES, and the oracle-surface test
// needs the second. It corrupts one envelope on each of: a NON-best matrix cell, the translation
// cell, and a best_cell latency field - so a gateway that declares exactly ONE cell offers only the
// third, and the test fails on a board that is behaving perfectly. That is not hypothetical: one-api
// declares a single cell and was the first gateway to publish in the 2026-07-29 run, so the test went
// red on a one-row board while the row itself was correct. The gate now counts what the test actually
// consumes, so the two cannot disagree about when it is runnable.
const donorSurfaces = (g) => {
  if (!(g && g.best_cell && g.best_cell.source && g.best_cell.source.kind === "matrix")) return 0;
  let n = 1;                                    // the best_cell latency field is always available
  const d = g.best_cell.path && g.best_cell.path.dialect;
  for (const [eg, up] of Object.entries((g.matrix && g.matrix.upstreams) || {}))
    for (const [ing, c] of Object.entries((up && up.cells) || {})) {
      if (ing === d && eg === d) continue;      // the best cell itself is not a SECOND surface
      const v = c && c.perf && c.perf.added_latency_p99_us;
      if (v && v.value != null) { n += 1; break; }
    }
  if (g.translation_cell && g.translation_cell.source && g.translation_cell.source.kind === "matrix") n += 1;
  return n;
};
const BOARD_HAS_MULTI_SURFACE_DONOR = (data.gateways || []).some((g) => donorSurfaces(g) >= 2);
const DECLARED_GATEWAYS = (data.gateways || []).length;
const isPublishing = (g) => !!(g && [g.best_cell, g.translation_cell, g.streaming].some((r) => r && r.source));
const PUBLISHING_GATEWAYS = (data.gateways || []).filter(isPublishing).length;
// "COMPLETE" MUST BE A STATE THE BOARD CAN ACTUALLY REACH.
//
// This was `publishing >= declared`, i.e. all fourteen. That is not "the run has finished", it is "the
// run has finished AND every gateway succeeded" - so one gateway that can never publish (its matrix
// ran and served nothing; a gateway the mock cannot drive; a retired row still declared) pinned
// completeness at false FOREVER, and every test gated on it became permanently unrunnable while
// reporting itself as a temporary skip. A gate that can only ever be closed is the same defect as a
// check that can only ever pass, wearing the opposite sign.
//
// A gateway is STILL PENDING only when its result has not landed yet. One whose matrix DID land and
// served nothing has published everything it is ever going to: that is a finished, measured row (the
// board renders it as a failure to serve), not a box we are waiting on. Completeness is therefore
// "nothing is still pending", which a real field run reaches even when some gateways fail.
const matrixServedSomething = (m) => !!(m && m.upstreams && Object.values(m.upstreams).some(
  (up) => Object.values((up && up.cells) || {}).some((c) => c && c.served === true)));
const PENDING_GATEWAYS = (data.gateways || []).filter(
  (g) => g && !isPublishing(g) && !(g.matrix && !matrixServedSomething(g.matrix))).length;
const BOARD_IS_COMPLETE = DECLARED_GATEWAYS > 0 && PENDING_GATEWAYS === 0;
if (!BOARD_HAS_MATRIX_DONOR && !BOARD_IS_COMPLETE) {
  console.warn(`warn - no matrix-sourced best_cell donor yet (${PUBLISHING_GATEWAYS}/${DECLARED_GATEWAYS} gateways publishing):`);
  console.warn("       the RED self-tests have nothing to revert and are reported as skipped until the board fills.");
}
const testWithMatrixDonor = (name, fn) =>
  ((BOARD_HAS_MATRIX_DONOR || BOARD_IS_COMPLETE) ? test(name, fn)
    : skip(name, `no matrix-sourced best_cell to revert yet (${PUBLISHING_GATEWAYS}/${DECLARED_GATEWAYS} gateways publishing)`));
// For the tests that need a donor carrying MORE THAN ONE oracled surface. See donorSurfaces.
const testWithMultiSurfaceDonor = (name, fn) =>
  (BOARD_HAS_MULTI_SURFACE_DONOR ? test(name, fn)
    : skip(name, `no gateway publishing 2+ oracled surfaces yet (${PUBLISHING_GATEWAYS}/${DECLARED_GATEWAYS} gateways publishing)`));

// THERE IS NO "ONLY WHEN THE WHOLE FIELD HAS LANDED" GATE ANY MORE, and it is worth saying why it
// went rather than letting it reappear.
//
// It existed for two tests described as claims about the whole FIELD: that R2's own failure path
// fires, and that the oracle reaches several distinct surfaces. Neither claim actually needed a full
// board. The R2 failure-path assertions are entirely SYNTHETIC (`checkConsistency({gateways: []})`) and
// consume no board data at all, so gating them on board completeness was wrong on its own terms; the
// oracle-surface test needs ONE matrix donor row, which is what testWithMatrixDonor already means.
//
// And the gate was not merely redundant, it was concealing a real regression: forced open, the R2 test
// FAILED, because an empty bundle took the partial-board arm of R2's coverage gate and produced a
// warning where the test (correctly) demanded an error. The skip read as "waiting for the board to
// fill" and meant "this assertion does not hold". A skip that hides a red test is worse than the red
// test, because it is quiet. Both tests now run on today's board; the invariant they broke on is fixed
// in check-consistency.mjs (see the "a board with nothing on it is not a board that is still filling"
// note there).

// ---- freshness guard (matrix-sole-source): relaxed rules ----
// Under matrix-sole-source each gateway is ONE atomic matrix run (hours long) published INDEPENDENTLY,
// so the board legitimately carries mixed per-gateway ages. The old RELATIVE guards are gone:
//   - intra-row SPAN hard-fail (mixed suites from different runs): REMOVED - replaced by a generous
//     sanity cap (12h) that only a corrupt/future-dated timestamp can trip;
//   - cross-gateway LAG hard-fail (a row lagging the board-newest): REMOVED - mixed cadences are honest.
// KEPT: the wholesale-stale ABSOLUTE floor (nothing on the board younger than MAX_BOARD_AGE_DAYS).
// NEW: a PER-GATEWAY staleness SIGNAL (g.stale set when a row's own data ages past MAX_GATEWAY_AGE_DAYS)
// - a badge, NOT a build failure. These tests positively exercise all of that against a synthetic repo,
// since the main data load above falls back to the committed bundle when the guard trips.
function buildSyntheticRepo(measuredAtByGw) {
  // measuredAtByGw: { key: { matrix: isoString, ... }, ... }
  //
  // THE FIXTURE HAS TO BE THE SHAPE THE ENGINE ACTUALLY WRITES. This built
  // `gateways/<key>/gateway.sh` and `results/<suite>/<key>.json` - a manifest format that no longer
  // exists and a per-suite results layout the engine stopped writing when it moved to
  // `results/snapshots/`. gen-data discovers neither, so every test built on this fixture was
  // asserting against an empty board and failing for that reason rather than for the guard it names.
  // Same migration debt the history appender carried: code left pointing at directories nothing
  // produces any more, going quietly red instead of guarding anything.
  const root = mkdtempSync(join(tmpdir(), "site-fresh-"));
  for (const [key, suites] of Object.entries(measuredAtByGw)) {
    mkdirSync(join(root, "gateways", key), { recursive: true });
    writeFileSync(
      join(root, "gateways", key, "definition.json"),
      JSON.stringify({
        name: key,
        display: key,
        lang: "Rust",
        class: "Gateway",
        model: "m",
        port: 1,
        path: "/v1/chat/completions",
        auth: "dummy",
        egress: ["openai"],
        matrix: ["100000", "000000", "000000", "000000", "000000", "000000"],
      }),
    );
    mkdirSync(join(root, "results", "snapshots"), { recursive: true });
    for (const [suite, iso] of Object.entries(suites)) {
      const doc = {
        gateway: key,
        build: "ok",
        served: true,
        measured_at: iso,
        arch: "arm64",
        added_latency_p50_us: 100,
        added_latency_p99_us: 200,
        rps_sustained_20ms: 10000,
        rps_max_proxy: 12000,
      };
      // The matrix suite is what the board displays, so it lands as a snapshot exactly as the engine
      // writes one. Any other suite key stays in its legacy per-suite file, which is the point of the
      // tests that assert a never-displayed legacy stamp cannot mask a stale board.
      if (suite === "matrix") {
        doc.matrix = {
          gateway: key,
          served: true,
          // The board ages what it DISPLAYS, and displayedMeasuredMs() reads the stamp off the
          // matrix rather than the envelope, so a fixture without it makes every freshness guard
          // silently inapplicable.
          measured_at: iso,
          upstreams: {
            openai: {
              configurable: true,
              served: true,
              cells: {
                openai: {
                  served: true,
                  perf: {
                    added_latency_p50_us: 100,
                    added_latency_p99_us: 200,
                    rps_sustained_20ms: 10000,
                    rps_max_proxy: 12000,
                  },
                },
              },
            },
          },
        };
        const stamp = String(iso).replace(/[:.]/g, "-");
        writeFileSync(join(root, "results", "snapshots", `result_${key}_${stamp}.json`), JSON.stringify(doc));
      } else {
        mkdirSync(join(root, "results", suite), { recursive: true });
        writeFileSync(join(root, "results", suite, `${key}.json`), JSON.stringify(doc));
      }
    }
  }
  return root;
}
function genThrows(root) {
  try {
    const outDir = mkdtempSync(join(tmpdir(), "site-fresh-out-"));
    execFileSync(process.execPath, [join(HERE, "gen-data.mjs"), root, outDir], { stdio: "pipe" });
    rmSync(outDir, { recursive: true, force: true });
    return null; // did not throw
  } catch (e) {
    return String(e.stderr || e.message || "");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}
// Like genThrows but returns the emitted data.json on success (so tests can assert per-gateway
// staleness flags / measured_at). Returns { err } on failure, { data } on success.
function genData(root) {
  const outDir = mkdtempSync(join(tmpdir(), "site-fresh-out-"));
  try {
    execFileSync(process.execPath, [join(HERE, "gen-data.mjs"), root, outDir], { stdio: "pipe" });
    return { data: JSON.parse(readFileSync(join(outDir, "data.json"), "utf8")) };
  } catch (e) {
    return { err: String(e.stderr || e.message || "") };
  } finally {
    rmSync(outDir, { recursive: true, force: true });
    rmSync(root, { recursive: true, force: true });
  }
}
const isoAgo = (h) => new Date(Date.now() - h * 3600000).toISOString();
const isoDaysAgo = (d) => new Date(Date.now() - d * 86400000).toISOString();

// THE SEAL AND ITS ORACLE MUST BE THE SAME RULE, over every input either can see.
//
// check-consistency.mjs re-derives what the board should show, straight from the raw artifact, as an
// independent check on the bundle. That independence is about the DATA PATH. It used to extend to the
// RULE as well - the oracle carried its own copy of the display condition - and the copy implemented
// only the capacity branch. When paced metrics arrived, the seal learned that matching a paced target
// is the gateway keeping up rather than a mock ceiling, and the oracle did not. Every cell whose
// mock-bound flag was true then had the seal publish and the oracle demand null: 25 mismatches, which
// hard-failed the deploy on every commit for two days while the board served a stale build.
//
// A second copy of a rule does not catch drift. It is the drift. Both call `displayedValue`, and this
// walks the whole input space to hold them there.
//
// The space USED to be (raw, flag, gated, paced) - the flag being the engine's retired verdict that our
// own rig had set the limit, which suppressed the value. There is no flag and no gate now, so the space
// is the raw value and the absence reason, and the property is stronger than agreement: a present number
// must SHOW.
test("seal and oracle agree on every (raw, absentReason) the board can produce", () => {
  const RAWS = [null, 0, 1, 1234.5, -1];
  const REASONS = [null, "not_measured", "below_resolution", "rig_limited", "harness_error"];
  const mismatches = [];
  for (const raw of RAWS) {
    for (const reason of REASONS) {
      const absent = reason ? { reason } : null;
      const oracle = oracleExpected(raw, reason);
      const sealed = sealMetric(raw, { absent });
      // COMPARE LIKE WITH LIKE - the two sides must answer the SAME question.
      //
      // `oracleExpected` is `displayedValue`, which answers "what does the board SHOW". For a
      // `below_resolution` absence that is 0: the comparison ran, the difference came out at or under
      // what the rig can resolve, and that is the best reading the test can express. The SEAL never
      // writes that 0 - it publishes {value: null, reason: "below_resolution"} and the DISPLAY layer
      // (app.metric) turns the reason into the ≈0 state. Reading `sealed.value` here compared a
      // seal-layer field against a display-layer answer and reported a disagreement where the two
      // agree completely. The envelope goes through the reader every surface uses instead.
      const shown = app.metric(sealed).v;
      if (shown !== oracle)
        mismatches.push(`raw=${raw} reason=${reason}: seal shows ${shown}, oracle expects ${oracle}`);
      // AND NOTHING SUPPRESSES. The retired shape is unreachable by construction; this holds it there.
      if (sealed.suppressed)
        mismatches.push(`raw=${raw} reason=${reason}: the seal produced a SUPPRESSED envelope, which no longer exists`);
      if (raw != null && sealed.value !== Number(raw))
        mismatches.push(`raw=${raw} reason=${reason}: a present measurement was not published as itself`);
    }
  }
  assert(mismatches.length === 0, `seal/oracle disagree:\n  ${mismatches.join("\n  ")}`);
  // AND THE BELOW-RESOLUTION 0 BELONGS TO THE DISPLAY LAYER, not to the seal. Pinned separately so the
  // loop above cannot be read as a claim that the seal invents a zero for a field nothing was measured
  // on: the envelope carries the absence and its reason, and the reader derives the ≈0 from the reason.
  const belowRes = sealMetric(null, { absent: { reason: "below_resolution" } });
  assert.equal(belowRes.value, null, "the seal publishes the absence, never a fabricated 0");
  assert.equal(belowRes.reason, "below_resolution", "with the engine's own reason, which is what carries the display");
  assert.equal(app.metric(belowRes).v, 0, "the ≈0 is the display reader's, derived from that reason");
});

// THE FACTS RIDE WITH THE NUMBER, AND ONLY WITH A NUMBER.
//
// `headroom` and `rig_ceiling` are what replaced the suppression: a reader gets the fraction of our own
// rig's ceiling the measurement reached and decides for themselves. They attach through `withExtras`, so
// they reach a certified 0 as well as a certified positive - a 0 beside a real ceiling is the claim that
// most demands its evidence - and they must never appear on an absence, which has no comparison.
test("headroom and its ceiling ride on certified envelopes and never on an absence", () => {
  const opts = { headroom: 0.83, ceiling: 52013 };
  for (const raw of [1, 1234.5, 0]) {
    const env = sealMetric(raw, opts);
    assert.equal(env.headroom, 0.83, `raw=${raw} lost its headroom`);
    assert.equal(env.rig_ceiling, 52013, `raw=${raw} lost the ceiling its headroom is a fraction of`);
    assert.equal(env.certified, true);
  }
  const absent = sealMetric(null, { ...opts, absent: { reason: "harness_error" } });
  assert.equal(absent.headroom, undefined, "an absence has no comparison to state a fraction of");
  assert.equal(absent.rig_ceiling, undefined);
  // No usable reference costs the FACTS and not the value - it used to cost both.
  const noRef = sealMetric(43297, { headroom: null, ceiling: null });
  assert.equal(noRef.value, 43297);
  assert.equal(noRef.certified, true);
  assert.equal(noRef.headroom, undefined);
});

// The throughput vocabulary is a list of KEYS THAT MUST BE ENVELOPES, not a list of values that must
// pass a gate - that is the whole of the rename from GATED_FIELDS. A field in it must therefore be one
// `isMetricField` recognises, or the seal and the C1 walk disagree about what has to be sealed.
test("every throughput field is part of the sealed-metric vocabulary", () => {
  for (const f of THROUGHPUT_FIELDS) {
    assert(isMetricField(f), `${f} is a throughput metric but isMetricField does not recognise it`);
  }
});

test("freshness guard HARD-FAILS a wholesale-stale board (absolute age floor kept)", () => {
  // The whole board is older than MAX_BOARD_AGE_DAYS (180d): NOTHING has refreshed at all. This is the
  // one absolute floor kept under matrix-sole-source - publishing generated_at=now over a board where
  // the newest measurement anywhere is >180d old is dishonest. Hard fail.
  const stale = isoDaysAgo(200);
  const msg = genThrows(buildSyntheticRepo({
    alpha: { matrix: stale },
    bravo: { matrix: stale },
  }));
  assert.ok(msg, "expected gen-data to THROW on a wholesale-stale board, but it succeeded");
  assert.ok(/FRESHNESS FAILURE \(stale board\)/.test(msg), `expected the stale-board failure, got: ${msg}`);
});

test("freshness guard PASSES a board with MIXED per-gateway ages (independent cadences are honest)", () => {
  // The core relaxation: one gateway measured today, another 3 weeks ago. Under the OLD lag guard the
  // 3-week-old row would hard-fail (it "lags the board-newest"); now that is honest and expected on a
  // living board where any one gateway is re-run alone. Board must PASS. The fixture keys are
  // synthetic on purpose: the guard is a function of AGE, not of which gateway is which.
  const { err, data } = genData(buildSyntheticRepo({
    alpha: { matrix: isoAgo(1) },         // today
    bravo: { matrix: isoDaysAgo(21) },    // 3 weeks ago - would have hard-failed the old lag guard
  }));
  assert.equal(err, undefined, `expected a mixed-age board to PASS gen-data, but it threw: ${err}`);
  // Neither is stale (both < 60d), and each carries its OWN measured_at.
  const byKey = Object.fromEntries(data.gateways.map((g) => [g.key, g]));
  assert.equal(byKey.alpha.stale, false, "the fresh row (today) must not be flagged stale");
  assert.equal(byKey.bravo.stale, false, "the 3-week-old row must not be flagged stale (< 60d)");
  assert.ok(byKey.alpha.measured_at && byKey.bravo.measured_at, "each gateway carries its OWN measured_at");
  assert.notEqual(byKey.alpha.measured_at, byKey.bravo.measured_at, "per-gateway measured_at survives independently");
});

test("freshness guard does NOT hard-fail a legitimate hours-long single matrix run (span check relaxed)", () => {
  // One atomic matrix run legitimately spans HOURS (a full 6x6 takes ~5h): timestamps 5h apart within
  // a row are a real run, not a franken-mix. The old MAX_SPAN_H=3 hard-failed exactly this; now only a
  // >12h sanity cap can trip. Board must PASS.
  const msg = genThrows(buildSyntheticRepo({
    alpha: { perf: isoAgo(6), matrix: isoAgo(1) }, // 5h span - a real long matrix run
  }));
  assert.equal(msg, null, `expected an hours-long single run to PASS, but it threw: ${msg}`);
});

test("freshness guard sets the PER-GATEWAY stale flag past MAX_GATEWAY_AGE_DAYS (badge, not a failure)", () => {
  // A gateway whose own data has aged past 60d gets g.stale=true (drives the app.js badge) WITHOUT
  // failing the build - as long as some OTHER gateway keeps the board under the wholesale floor.
  const { err, data } = genData(buildSyntheticRepo({
    fresh: { matrix: isoAgo(2) },          // keeps the board off the wholesale floor
    old: { matrix: isoDaysAgo(75) },       // > 60d → flagged stale, but NOT a hard fail
  }));
  assert.equal(err, undefined, `a per-gateway-stale board must still build (badge, not failure): ${err}`);
  const byKey = Object.fromEntries(data.gateways.map((g) => [g.key, g]));
  assert.equal(byKey.old.stale, true, "a gateway older than 60d must be flagged stale");
  assert.equal(byKey.fresh.stale, false, "a fresh gateway must not be flagged stale");
});

test("MED-2: the wholesale-stale floor is MATRIX-scoped - a never-displayed legacy re-run cannot mask a stale board", () => {
  // Every DISPLAYED number is 200d old (matrix ancient) but one untouched results/perf/<gw>.json was
  // re-run 'yesterday'. Folding the retired suite made boardNewest=yesterday, so the 180d hard-fail
  // (the KEPT last line of defense) never fired and a wholesale-stale matrix board published. The floor
  // now ages the DISPLAYED (matrix-preferring) stamps, so the legacy re-run does NOT save it: hard fail.
  const msg = genThrows(buildSyntheticRepo({
    alpha: { matrix: isoDaysAgo(200), perf: isoAgo(24) },  // displayed=200d old; legacy=1d (never shown)
    bravo: { matrix: isoDaysAgo(200) },
  }));
  assert.ok(msg, "expected gen-data to THROW: a never-displayed legacy stamp must not mask a stale matrix board");
  assert.ok(/FRESHNESS FAILURE \(stale board\)/.test(msg), `expected the stale-board failure, got: ${msg}`);
});

test("MED-1: latest_measured_at reflects the DISPLAYED (matrix) stamp, not a newer never-displayed legacy re-run", () => {
  // All matrix data is 90d old; one gateway had an ad-hoc SUITES=perf re-run 1h ago. The board footer
  // 'Latest measurement' must age the DISPLAYED numbers (90d), NOT claim '1h ago' off a legacy stamp
  // that is never shown. latest_measured_at must equal the newest MATRIX stamp, not the perf stamp.
  const matrixNewer = isoDaysAgo(90), legacyFresh = isoAgo(1);
  const { err, data } = genData(buildSyntheticRepo({
    alpha: { matrix: matrixNewer, perf: legacyFresh },  // legacy fresher than the displayed matrix
    bravo: { matrix: isoDaysAgo(120) },
  }));
  assert.equal(err, undefined, `expected a 90d board to PASS (< 180d floor): ${err}`);
  assert.equal(data.latest_measured_at, matrixNewer,
    `latest_measured_at must be the newest DISPLAYED matrix stamp (${matrixNewer}), not the never-displayed legacy perf stamp (${legacyFresh}); got ${data.latest_measured_at}`);
});

test("NIT-5: the per-gateway badge stamp (ageBasisMs) prefers the MATRIX stamp over a newer legacy suite", () => {
  // LOW-R3-3 regression guard: g.measured_at (the per-row 'measured Nd ago' badge) must age the DISPLAYED
  // numbers - the matrix stamp - even when a newer legacy results/perf/<gw>.json is present. Deriving it
  // from the max-across-suites would drive a 'measured 1h ago' badge over 90d-old shown numbers.
  const matrixStamp = isoDaysAgo(90), legacyFresh = isoAgo(1);
  const { err, data } = genData(buildSyntheticRepo({
    alpha: { matrix: matrixStamp, perf: legacyFresh },
    bravo: { matrix: isoAgo(2) },  // keeps the board under the wholesale floor
  }));
  assert.equal(err, undefined, `board must build: ${err}`);
  const alpha = data.gateways.find((g) => g.key === "alpha");
  assert.equal(alpha.measured_at, matrixStamp,
    `the badge stamp must be the matrix stamp (${matrixStamp}), not the newer legacy perf stamp (${legacyFresh}); got ${alpha.measured_at}`);
  assert.equal(alpha.stale, true, "alpha's displayed numbers are 90d old (> 60d) → stale badge, matrix-aged");
});

test("HIGH-3: the span cap is MATRIX-scoped - a matrix-only re-run past a weeks-old legacy stamp PASSES", () => {
  // The core HIGH-3 fix: legacy suites (perf/stream/streamcpu/memory) are fallback-only and are NEVER
  // refreshed by a matrix-only re-run, so they legitimately carry weeks-old stamps while matrix=today.
  // Folding them into the span made an honest incremental matrix re-run trip the >12h cap and abort the
  // deploy. The span cap now considers ONLY the matrix suite's own timestamps, so this must PASS.
  const msg = genThrows(buildSyntheticRepo({
    incr: { perf: isoDaysAgo(21), matrix: isoAgo(1) }, // matrix today, legacy 3 weeks old - honest re-run
  }));
  assert.equal(msg, null, `expected a matrix-only re-run past a weeks-old legacy stamp to PASS, but it threw: ${msg}`);
});

test("HIGH-3: a per-gateway FUTURE measured_at is warned and never posts a negative age badge", () => {
  // NIT (future-date, per-gateway): the board-wide floor only checks the max stamp; a lone clock-skewed
  // FUTURE stamp on one gateway would slip past and render a negative "measured Nd ago" badge. gen-data
  // must skip the future stamp from the age computation (and the board floor throw catches a future
  // matrix stamp outright). Here a legacy suite is future-dated: the board floor throws on it first
  // (generated_at would predate the newest embedded measured_at), which is the honest hard-fail.
  const msg = genThrows(buildSyntheticRepo({
    fresh: { matrix: isoAgo(2) },
    skewed: { matrix: isoAgo(1), perf: isoAgo(-48) }, // perf 2 days in the FUTURE (rig clock skew)
  }));
  assert.ok(msg, "expected a future-dated stamp to be caught");
  assert.ok(/predates the newest embedded measured_at|future-dated/.test(msg),
    `expected the future-date hard-fail, got: ${msg}`);
});

test("OOTB config artifact round-trips into data.json (results/config/<gw>.txt → g.ootb_config)", () => {
  // Config transparency: a gateway whose run captured results/config/<key>.txt must carry that exact
  // text into the bundle as g.ootb_config (app.js renders it in the Config drawer). Build a fresh,
  // coherent synthetic repo (so the freshness guard passes), drop a config sidecar for ONE gateway,
  // reference it via that gateway's perf.ootb_config pointer, run gen-data for real, and assert the
  // artifact appears verbatim on that gateway and is ABSENT on the gateway with no sidecar (graceful
  // degradation - the not-yet-wired gateways).
  const t = isoAgo(1);
  const root = buildSyntheticRepo({
    withcfg: { perf: t, matrix: isoAgo(2) },
    nocfg: { perf: isoAgo(0.5), matrix: isoAgo(1.5) },
  });
  const CFG = "PORT=8080\nOPENAI_BASE_URL=http://127.0.0.1:8000/v1\nOPENAI_API_KEY=dummy\nSTORAGE_TYPE=sqlite\n";
  mkdirSync(join(root, "results", "config"), { recursive: true });
  writeFileSync(join(root, "results", "config", "withcfg.txt"), CFG);
  // Record the pointer in the perf result, exactly as the perf suite does.
  const perfPath = join(root, "results", "perf", "withcfg.json");
  const perf = JSON.parse(readFileSync(perfPath, "utf8"));
  perf.ootb_config = "config/withcfg.txt";
  writeFileSync(perfPath, JSON.stringify(perf));
  const outDir = mkdtempSync(join(tmpdir(), "site-cfg-out-"));
  try {
    execFileSync(process.execPath, [join(HERE, "gen-data.mjs"), root, outDir], { stdio: "pipe" });
    const d = JSON.parse(readFileSync(join(outDir, "data.json"), "utf8"));
    const withCfg = d.gateways.find((g) => g.key === "withcfg");
    const noCfg = d.gateways.find((g) => g.key === "nocfg");
    assert.ok(withCfg, "expected the withcfg gateway in the bundle");
    assert.equal(withCfg.ootb_config, CFG, "OOTB config artifact must round-trip verbatim into g.ootb_config");
    assert.ok(!("ootb_config" in noCfg) || noCfg.ootb_config == null,
      "a gateway with no config sidecar must have no ootb_config (graceful degradation)");
  } finally {
    rmSync(outDir, { recursive: true, force: true });
    rmSync(root, { recursive: true, force: true });
  }
});

test("config-correction deep link is a per-gateway, template-referencing, fully-encoded GitHub URL", () => {
  const url = app.configCorrectionUrl({ key: "synthgw", display: "SynthGW" });
  assert.ok(url.startsWith(app.BENCH_REPO + "/issues/new?"), `must target the benchmarking repo new-issue endpoint, got ${url}`);
  const u = new URL(url);
  assert.equal(u.searchParams.get("template"), "config-correction.yml", "must reference the issue-form template");
  assert.equal(u.searchParams.get("title"), "Config correction: SynthGW", "title must be pre-set per gateway (decoded)");
  assert.equal(u.searchParams.get("gateway"), "SynthGW", "gateway field must be injected per gateway");
  // A display name with spaces/specials must be encoded, never break the URL.
  const tricky = app.configCorrectionUrl({ key: "x", display: 'A & B "gw"' });
  assert.doesNotThrow(() => new URL(tricky), "special chars in the display name must stay URL-safe");
  assert.equal(new URL(tricky).searchParams.get("gateway"), 'A & B "gw"');
  // The referenced template must actually exist in the repo.
  assert.ok(
    readFileSync(join(ROOT, ".github", "ISSUE_TEMPLATE", "config-correction.yml"), "utf8").includes("id: gateway"),
    "the config-correction issue template the deep link references must exist and define the gateway field");
});

test("gen-data emits gateways with a class for every entry", () => {
  assert.ok(data.gateways.length >= 10, `expected a full field, got ${data.gateways.length}`);
  for (const g of data.gateways) {
    assert.ok(typeof g.cls === "string" && g.cls.length > 0, `${g.key} has no cls`);
  }
  // And the class is not invented by the board: it is each project's OWN self-description, read
  // straight out of that gateway's manifest. Assert that for EVERY gateway rather than spot-checking
  // one by name, so the assertion holds for a gateway dropped in tomorrow.
  for (const g of data.gateways) {
    // definition.json is the manifest the engine runs from; gateway.sh has not existed since the
    // field moved off shell manifests, so reading it made this assertion throw ENOENT rather than
    // check anything.
    const man = JSON.parse(readFileSync(join(ROOT, "gateways", g.key, "definition.json"), "utf8"));
    if (!man.class) continue;               // a manifest may omit it; gen-data then defaults it
    assert.equal(g.cls, man.class, `${g.key}: published cls must be the manifest's own class`);
  }
});

test("star snapshot is well formed, carries no stale key, and gen-data attaches it", () => {
  // The committed snapshot (gateways/stars.json, refreshed by `node gateways/fetch-stars.mjs`).
  //
  // A gateway missing from the stars snapshot must NOT hard-fail the build: a star count is decoration,
  // gen-data already degrades a missing entry to `stars: null`, and the board renders that as no star
  // data, so a gateway added since the last refresh is honest, not a build break.
  //
  // What IS asserted: every entry present must be well formed, and the snapshot must carry NO key
  // without a gateway directory. A stale key is real drift (a renamed or deleted gateway leaving a
  // phantom count behind); a missing key is just a pending refresh.
  const snap = JSON.parse(readFileSync(join(ROOT, "gateways", "stars.json"), "utf8"));
  const live = new Set(readdirSync(join(ROOT, "gateways")).filter(
    (d) => existsSync(join(ROOT, "gateways", d, "definition.json"))));
  for (const [key, s] of Object.entries(snap)) {
    assert.ok(live.has(key), `gateways/stars.json has a stale key '${key}' with no gateways/${key}/definition.json`);
    assert.ok(Number.isInteger(s.stars) && s.stars >= 0, `${key} stars not an integer`);
    assert.ok(/^\d{4}-\d{2}-\d{2}$/.test(s.as_of), `${key} as_of not YYYY-MM-DD`);
  }
  const pending = data.gateways.filter((g) => !snap[g.key]).map((g) => g.key);
  if (pending.length) console.log(`  (note: no star snapshot yet for ${pending.join(", ")} - run: node gateways/fetch-stars.mjs)`);
  // A bundle emitted by the CURRENT gen-data carries the attached fields. The committed
  // fallback bundle (mid-refresh) may predate them; assert only when present.
  for (const g of data.gateways) {
    if ("stars" in g && snap[g.key]) {
      assert.equal(g.stars, snap[g.key].stars, `${g.key} bundle stars != snapshot`);
      assert.equal(g.stars_as_of, snap[g.key].as_of, `${g.key} bundle as_of != snapshot`);
    }
    // No snapshot entry -> the bundle must say so honestly, never fabricate a count.
    if ("stars" in g && !snap[g.key]) {
      assert.equal(g.stars, null, `${g.key} has no snapshot entry, so its published star count must be null`);
    }
  }
});

// ---- filtering --------------------------------------------------------------
test("search filters rows by name", () => {
  const st = app.newState();
  st.q = "lite";
  const rows = app.applyFilters(data.gateways, st);
  assert.ok(rows.length >= 1 && rows.length < data.gateways.length);
  assert.ok(rows.every((g) => (g.display + g.key).toLowerCase().includes("lite")));
  st.q = "no-such-gateway-xyz";
  assert.equal(app.applyFilters(data.gateways, st).length, 0);
});

test("capability toggle filters without crashing on missing suites", () => {
  const st = app.newState();
  st.needStream = true;
  const streaming = app.applyFilters(data.gateways, st);
  // The stream capability filter keeps only gateways with a projected g.streaming (canonicalStreaming).
  assert.ok(streaming.every((g) => g.streaming && g.streaming.stream_served));
});

test("the class/lang filter chip rows are gone; stale URL params are ignored", () => {
  // The chip rows were removed from the perf-tab controls (the roster tab already
  // shows language and class); the shell must not carry their containers.
  const shell = readFileSync(join(HERE, "index.html"), "utf8");
  assert.ok(!shell.includes("class-filters"), "index.html still has #class-filters");
  assert.ok(!shell.includes("lang-filters"), "index.html still has #lang-filters");
  // A stale ?cls= / ?lang= from an old shared URL decodes without error and
  // without filtering (no invisible filter with no UI to clear); the rest of the
  // params on the same URL still apply.
  const st = app.decodeUrl("/gateways/performance", "?cls=Control%20plane&lang=Rust&q=bus");
  assert.equal(st.view, "performance");
  assert.equal(st.q, "bus");
  assert.ok(!("classes" in st) && !("langs" in st), "retired filter state fields are gone");
  st.q = "";
  assert.equal(app.applyFilters(data.gateways, st).length, data.gateways.length, "stale params filter nothing");
  // and encoding never re-emits them
  assert.ok(!app.encodeUrl(st).includes("cls="));
  assert.ok(!app.encodeUrl(st).includes("lang="));
});

// ---- path-URL state round-trip ----------------------------------------------
const parts = (url) => {
  const u = new URL(url, "https://onthebench.ai");
  return [u.pathname, u.search];
};

test("url state round-trips through /<category>/<view>?<params>", () => {
  const st = app.newState();
  st.view = "matrix";
  st.q = "bus bar & co";
  st.needXlate = true;
  st.sortCol = "lat";
  st.sortDesc = false;
  st.cmp = ["alpha", "bravo"];
  st.cmpOpen = true;
  st.drawer = "alpha";
  const url = app.encodeUrl(st);
  assert.ok(url.startsWith("/gateways/matrix?"), `path carries category+view: ${url}`);
  const back = app.decodeUrl(...parts(url));
  for (const k of ["category", "view", "q", "sortCol", "sortDesc", "needStream", "needXlate", "cmpOpen", "drawer"]) {
    assert.deepEqual(back[k], st[k], `field ${k}`);
  }
  assert.deepEqual(back.cmp, st.cmp);
});

test("default state encodes to /gateways and decodes back to defaults", () => {
  // The category root is the OVERVIEW (the neutral roster), not a ranking tab.
  assert.equal(app.DEFAULT_VIEW, "gateways");
  assert.equal(app.newState().view, "gateways");
  assert.equal(app.encodeUrl(app.newState()), "/gateways");
  const back = app.decodeUrl("/gateways", "");
  const def = app.newState();
  assert.equal(back.category, "gateways");
  assert.equal(back.view, "gateways");
  assert.equal(back.sortCol, def.sortCol);
  assert.equal(back.sortDesc, def.sortDesc);
  assert.equal(back.drawer, null);
  assert.deepEqual(back.cmp, []);
  // Performance is a real tab at its own path now, no longer the landing view.
  assert.equal(app.decodeUrl("/gateways/performance", "").view, "performance");
  const st = app.newState();
  st.view = "performance";
  assert.equal(app.encodeUrl(st), "/gateways/performance");
});

test("the site root is the HOME landing page, above the category nav", () => {
  // / decodes to home (not a category tab) and a home state encodes back to /.
  assert.equal(app.HOME_VIEW, "home");
  const home = app.decodeUrl("/", "");
  assert.equal(home.view, "home");
  const st = app.newState();
  st.view = app.HOME_VIEW;
  assert.equal(app.encodeUrl(st), "/");
  // /gateways is the category, defaulting to the roster overview.
  const cat = app.decodeUrl("/gateways", "");
  assert.equal(cat.category, "gateways");
  assert.equal(cat.view, "gateways");
  // home is NOT one of the category's view tabs
  assert.ok(!app.VIEWS.includes("home"));
  assert.ok(!app.PERF_VIEWS.has("home"));
});

test("home renders one CTA card per category plus the coming-soon placeholder", () => {
  const html = app.homeCardsHtml(data);
  assert.ok(html.includes(`href="/gateways"`), "gateways card links to the category");
  assert.ok(html.includes(`${data.gateways.length} self-hostable AI gateways`), "card carries the live entrant count");
  assert.ok(/overhead, throughput, streaming, and protocol translation/.test(html));
  assert.ok(html.includes("Coming soon"), "muted future-category placeholder");
  // no data yet: the card still renders, just without a count
  assert.ok(app.homeCardsHtml(null).includes("Self-hostable AI gateways"));
  assert.ok(!app.homeCardsHtml(null).includes("null "));
  // no em dashes in rendered strings (house style)
  assert.ok(!html.includes("\u2014"), "no em dashes in home cards");
});

test("unknown paths land on home; unknown views land on the category overview", () => {
  assert.equal(app.decodeUrl("/index.html", "").view, "home");
  assert.equal(app.decodeUrl("/no-such-category/matrix", "").view, "home");
  assert.equal(app.decodeUrl("/gateways/no-such-view", "").view, "gateways");
  assert.equal(app.decodeUrl("/gateways/no-such-view", "").category, "gateways");
  // legacy view aliases still resolve onto live tabs (the old Peak/Matched/passthrough/translation
  // tabs all fold into Performance; results/charts keep their old targets)
  assert.equal(app.decodeUrl("/gateways/results", "").view, "performance");
  assert.equal(app.decodeUrl("/gateways/peak", "").view, "performance");
  assert.equal(app.decodeUrl("/gateways/matched", "").view, "performance");
  assert.equal(app.decodeUrl("/gateways/passthrough", "").view, "performance");
  assert.equal(app.decodeUrl("/gateways/translation", "").view, "performance");
  // `charts` used to be an example of an UNKNOWN view here. It is a real tab now, so the fixture had
  // to change or the test would assert that a live tab redirects away from itself.
  assert.equal(app.decodeUrl("/gateways/charts", "").view, "charts");
  assert.equal(app.decodeUrl("/gateways/no-such-tab", "").view, "gateways");
  // the documented deep link shape
  const st = app.decodeUrl("/gateways/matrix", "?sort=mempeak&dir=asc");
  assert.equal(st.view, "matrix");
  assert.equal(st.sortCol, "mempeak");
  assert.equal(st.sortDesc, false);
});

test("legacy hash URLs (#view=...&sort=...) still decode", () => {
  const st = app.decodeUrl("/", "", "#view=matrix&sort=mempeak&dir=asc&lang=Rust");
  assert.equal(st.category, "gateways");
  assert.equal(st.view, "matrix");
  assert.equal(st.sortCol, "mempeak");
  assert.equal(st.sortDesc, false);
  // and re-encoding a legacy state yields the clean path form
  assert.ok(app.encodeUrl(st).startsWith("/gateways/matrix?"));
});

test("decode rejects a bogus sort column", () => {
  const back = app.decodeUrl("/gateways", "?sort=evil&dir=asc");
  assert.equal(back.sortCol, "rps");
});

// A RETIRED SORT ID STILL LANDS ON A RANKING. `?sort=rps20` / `?sort=rpsmax` are in every Performance link
// ever shared and in the charts' deep links; the two columns they name are gone with the two scalar metrics
// they read, and both links MEANT "rank by throughput" - which is now the frontier reading at the selected
// bound. Falling through to the tab default would land in the same place by accident; the alias says so.
test("a retired throughput sort id decodes onto the column that carries that ranking now", () => {
  for (const old of ["rps20", "rpsmax"]) {
    const st = app.decodeUrl("/gateways/performance", `?sort=${old}&dir=desc`);
    assert.equal(st.sortCol, "rps", `?sort=${old} must rank by the frontier reading`);
    assert.equal(st.sortDesc, true);
  }
  // And the retired streaming id lands on the surviving frame-rate column, not on nothing.
  assert.equal(app.decodeUrl("/gateways/streaming", "?sort=cpufps").sortCol, "streamfps");
});

test("a direct URL load defaults each tab to its column's natural direction", () => {
  // Performance headline on the frontier reading at the selected bound -> descending (higher is better)
  const pass = app.decodeUrl("/gateways/performance", "");
  assert.equal(pass.sortCol, "rps");
  assert.equal(pass.sortDesc, true);
  // The Frontier tab ranks at the DEFAULT BOUND's own column, so a reader arriving with no params is
  // ranked at the bound the caption names rather than at whichever column happens to be listed first.
  const front = app.decodeUrl("/gateways/frontier", "");
  assert.equal(front.sortCol, app.boundColId(app.DEFAULT_BOUND_MS));
  assert.equal(front.sortDesc, true);
  assert.equal(front.bound, app.DEFAULT_BOUND_MS);
  // Memory headline on Peak RSS -> ascending (lower is better)
  const mem = app.decodeUrl("/gateways/memory", "");
  assert.equal(mem.sortCol, "mempeak");
  assert.equal(mem.sortDesc, false);
  // Streaming headline on added TTFT -> ASCENDING (lower is better); the hard-refresh bug
  // was this defaulting to descending and floating the worst gateway to the top.
  const stream = app.decodeUrl("/gateways/streaming", "");
  assert.equal(stream.sortCol, "sttft");
  assert.equal(stream.sortDesc, false);
});

// ---- gateways overview: the neutral roster ----------------------------------
test("gateways overview lists EVERY gateway alphabetically, none seated first or held back", () => {
  const rows = app.rosterRows(data.gateways);
  assert.equal(rows.length, data.gateways.length, "no gateway filtered out of the roster");
  const names = rows.map((g) => g.display.toLowerCase());
  assert.deepEqual(names, names.slice().sort(), "roster is alphabetical, case-insensitive");
  // SET EQUALITY, not a spot-check on one name: every key in the data appears exactly once in the
  // roster, so no entry (the operator's own included) can ever be special-cased in or out of it.
  assert.deepEqual(rows.map((g) => g.key).slice().sort(), data.gateways.map((g) => g.key).slice().sort(),
    "the roster is exactly the field, with no entry added, dropped or duplicated");
  // the roster is a VIEW of the data, never a mutation of it
  assert.notEqual(rows, data.gateways);
});

test("star counts format compactly and degrade to null", () => {
  assert.equal(app.fmtStars(614), "614");
  assert.equal(app.fmtStars(12345), "12.3k");
  assert.equal(app.fmtStars(54500), "54.5k");
  assert.equal(app.fmtStars(0), "0");
  assert.equal(app.fmtStars(null), null);
  assert.equal(app.fmtStars(undefined), null);
});

test("the unified tab order: Gateways · Memory · Performance · Frontier · Streaming · matrix · Charts · method", () => {
  // FRONTIER SITS BESIDE PERFORMANCE, not at the end: it is the same measurement read every published way,
  // and a reader who has just looked at a ranking at one bound is one tab away from the whole curve. The
  // order is asserted because it is the reading order of the board, not an implementation detail.
  //
  // CHARTS SITS AFTER EVERY DATA TAB, next to Method. It replaced 25 static PNGs, and the reading
  // order is "the numbers, then the same numbers as a picture" - a gallery a reader meets before
  // knowing what is in it teaches nothing.
  assert.deepEqual(app.VIEWS, ["gateways", "memory", "performance", "frontier", "streaming", "matrix", "charts", "method"]);
  assert.equal(app.VIEW_LABELS.gateways, "Gateways");
  assert.equal(app.VIEW_LABELS.memory, "Memory");
  assert.equal(app.VIEW_LABELS.performance, "Performance");
  assert.equal(app.VIEW_LABELS.frontier, "Frontier");
  // the overview is a roster section, not a ranked perf table
  assert.ok(!app.PERF_VIEWS.has("gateways"));
  assert.ok(!(app.VIEW_SORT && "gateways" in app.VIEW_SORT));
  // Memory is a table view (its own per-gateway columns) but NOT cell-chooser driven.
  assert.ok(app.TABLE_VIEWS.has("memory") && !app.PERF_VIEWS.has("memory"));
  assert.ok(app.PERF_VIEWS.has("performance") && app.PERF_VIEWS.has("streaming"));
  // the perf tabs are pure measurement: no implementation-language column anywhere.
  // Language lives only on the Gateways overview roster.
  for (const [view, cols] of Object.entries(app.COLUMN_SETS)) {
    assert.ok(!cols.some((c) => c.id === "lang"), `${view} still carries a lang column`);
  }
  // the measurement-fact pill (Tested on) stays on Performance (shown in Peak mode)
  assert.ok(app.COLUMN_SETS.performance.some((c) => c.id === "tested"));
});

// ---- three-tab split: honest passthrough / translation sourcing ---------------
const mkMatrix = (cells) => ({ upstreams: Object.fromEntries(
  Object.entries(cells).map(([eg, ing]) => [eg, { cells: Object.fromEntries(
    Object.entries(ing).map(([i, c]) => [i, c])) }])) });

test("Passthrough is BEST-OF: every gateway shows on its best diagonal, none filtered", () => {
  // best_cell (openai diagonal) -> that reading. THE THROUGHPUT IS READ THROUGH THE FRONTIER now, at a
  // named bound: `passCell` takes a metric FIELD and the frontier is an array of readings, not a field, so
  // the sibling accessor is frontierCell. The latency half of this record is still a plain envelope and is
  // still read by passCell, which is the point of keeping both here.
  const green = { best_cell: bcCell({ dialect: "openai", frontier: 30000 }) };
  assert.equal(app.frontierCell(green.best_cell, 10).na, false);
  assert.equal(app.frontierCell(green.best_cell, 10).text, "30,000");
  assert.equal(app.passCell(green, "added_latency_p99_us", String).text, "110");
  // no best_cell at all (a gateway whose sweep did not land): reads n/a - there is no legacy perf reservoir.
  const unswept = { matrix: mkMatrix({ openai: { openai: { served: true } } }) };
  assert.equal(app.passCell(unswept, "added_latency_p99_us", String).na, true);
  assert.equal(app.frontierChooserCell(unswept, { ...app.newState(), mode: "peak" }).na, true);
  // openai not served: BEST-OF shows the native diagonal (one gateway -> anthropic), NOT n/a and
  // NOT filtered. gen-data picks it; here best_cell carries the anthropic number.
  const native = { best_cell: bcCell({ dialect: "anthropic", frontier: 32354 }) };
  assert.equal(app.frontierCell(native.best_cell, 10).na, false);
  assert.equal(app.frontierCell(native.best_cell, 10).text, "32,354");
  // and Passthrough does NOT filter: a gateway with only a native diagonal still appears
  const st = app.newState(); // view passthrough
  const rows = app.applyFilters([{ display: "x", key: "x", lang: "Rust", ...native }], st);
  assert.equal(rows.length, 1);
});

test("Streaming tab keeps measured streaming refusals as visible rows", () => {
  // Principle 3: filtering a competitor out reads as hiding it. A streaming gateway (projected
  // g.streaming) stays; a measured refusal (no projected streaming) still appears as a muted row, and
  // naText labels a stream_served:false record "did not stream" with the evidence.
  const st = app.newState();
  st.view = "streaming";
  const streams = { display: "s", key: "s", lang: "Go", streaming: streamRec({ added_ttft_p99_us: 1 }) };
  const refused = { display: "r", key: "r", lang: "Node" };  // no projected streaming (did not stream)
  const rows = app.applyFilters([streams, refused], st);
  assert.deepEqual(rows.map((g) => g.key).sort(), ["r", "s"], "refusal row is not filtered out");
  // naText still maps a raw-shaped stream_served:false record to the "did not stream" label + evidence.
  const na = app.naText({ stream_served: false, stream_error: "no SSE frames on stream:true" }, "stream_served", "stream_error");
  assert.equal(na.text, "did not stream");
  assert.equal(na.note, "no SSE frames on stream:true");
});

test("Performance Custom shows EVERY gateway (unfiltered); a gateway lacking the pinned cell reads n/a", () => {
  // Unlike the old Matched tab, Performance Custom NEVER filters a competitor out: every gateway
  // appears, and one that does not serve the pinned in->out cell simply reads n/a on that row.
  // g0 serves openai->anthropic, g1 serves only openai->gemini. Cell perf is SEALED in place.
  const g0 = { display: "g0", key: "g0", lang: "Rust",
    matrix: mkMatrix({ anthropic: { openai: { served: true, perf: cellPerf({ frontier: 100, added_latency_p99_us: 200 }) } } }) };
  const g1 = { display: "g1", key: "g1", lang: "Go",
    matrix: mkMatrix({ gemini: { openai: { served: true, perf: cellPerf({ frontier: 90, added_latency_p99_us: 300 }) } } }) };
  const st = { ...app.newState(), view: "performance", mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  // BOTH gateways appear (no filtering in Custom mode).
  assert.deepEqual(app.applyFilters([g0, g1], st).map((g) => g.key), ["g0", "g1"]);
  // g0 serves the pinned cell -> a number; g1 does not -> n/a. Read through the frontier accessor the
  // Performance column uses, at the state's own selected bound.
  assert.equal(app.frontierChooserCell(g0, st).text, "100 @ 512 conc");
  assert.equal(app.frontierChooserCell(g1, st).na, true);
  // Repin to openai->gemini: now g1 reads a number and g0 reads n/a, still both present.
  const st2 = { ...st, xlateOut: "gemini" };
  assert.equal(app.frontierChooserCell(g1, st2).text, "90 @ 512 conc");
  assert.equal(app.frontierChooserCell(g0, st2).na, true);
  assert.deepEqual(app.applyFilters([g0, g1], st2).map((g) => g.key), ["g0", "g1"]);
});

// ---- consistency guard: one canonical value per (gateway, metric) -----------
testWithData("consistency guard: table == drawer == compare == charts on the real bundle", () => {
  const { errors, warnings } = checkConsistency(data, app);
  for (const w of warnings) console.warn(`  warn - ${w}`); // R7 inversions: visible, never fatal
  assert.deepEqual(errors, [], `numeric divergence across surfaces:\n${errors.join("\n")}`);
});

// A best_cell whose metrics are sealed envelopes: every surface reads the value through metric(), so no
// render site holds a raw scalar of its own (invariant P1).
//
// The second half of this test used to assert the SUPPRESSION: a `rps_sustained_20ms_mock_bound: true`
// cell published {value:null} and read n/a on every surface, "so there is no ungated field to leak". The
// leak class it guarded is real and still guarded (the raw scalar is consumed at seal time), but the
// price was deleting the measurement itself - a number the harness took correctly, withheld because our
// own rig might have bounded it. It now publishes, with the fraction of that ceiling it reached riding
// alongside so a reader can weigh it. So the second half holds the OPPOSITE property: the near-ceiling
// number reaches every surface, its `headroom` and `rig_ceiling` reach it too, and the bundle is still
// structurally clean (C1/C2) with them on board.
test("sealed envelope: every surface reads best_cell through metric(); a frontier reading is one too", () => {
  const g = { key: "seal", display: "Seal", lang: "Rust",
    best_cell: bcCell({ added_latency_p99_us: 111, frontier: { 1: 22222, 10: 30000, none: 33333 } }) };
  // table (passCell) reads the envelope value
  assert.equal(app.passCell(g, "added_latency_p99_us", String).v, 111);
  // AND THE THROUGHPUT READINGS ARE ENVELOPES TOO, one per bound, read through the same metric()
  // accessor - which is the property that survived the two scalars this test used to check. The rate is
  // sealed; the concurrency, the observed tail and the boundary proof ride beside it as plain evidence.
  assert.equal(app.frontierCell(g.best_cell, 1).v, 22222);
  assert.equal(app.frontierCell(g.best_cell, 10).v, 30000);
  assert.equal(app.frontierCell(g.best_cell, null).v, 33333);
  assert.ok(app.isEnvelope(app.frontierAt(g.best_cell.frontier, 10).rps), "each reading's rate is a sealed envelope");
  // A bound the record has no reading at is an ABSENCE with a reason, never a zero and never a blank.
  assert.equal(app.frontierCell(g.best_cell, 50).na, true);
  assert.match(app.frontierCell(g.best_cell, 50).note, /no reading at 50 ms/);
  // drawer/compare read the SAME canonical record (the projected best_cell)
  const perfLane = app.LANES.find((l) => l.key === "perf");
  assert.equal(perfLane.get, app.canonicalPerf, "perf lane reads the canonical accessor");
  const rec = perfLane.get(g);
  const laneRow = (bound) => perfLane.metrics.find((m) => m.k === `frontier.${bound}`);
  assert.equal(laneRow("10ms").cell(rec).v, 30000, "the drawer row reads the same reading the table does");
  assert.equal(laneRow("unbounded").cell(rec).v, 33333);
  assert.deepEqual(checkConsistency({ gateways: [g] }, app, SYNTH).errors, [], "a clean sealed bundle is consistent");
  // A LATENCY metric near the rig's own ceiling is still PUBLISHED with the comparison's own facts on it
  // (the headroom/ceiling pair that replaced the suppression). The throughput lane no longer carries
  // headroom - a frontier reading is a maximum over qualifying rungs, not a comparison against a rig
  // reference - so the property is asserted where it still exists.
  const bound = { key: "sealb", display: "SealB", lang: "Rust",
    best_cell: { ...bcCell({ frontier: 24999 }),
      added_latency_p99_us: seal(24999, { headroom: 0.97, ceiling: 25700 }) } };
  assert.equal(app.passCell(bound, "added_latency_p99_us", String).na, false, "a near-ceiling metric is not n/a");
  assert.equal(app.passCell(bound, "added_latency_p99_us", String).v, 24999, "the table shows the number that was measured");
  assert.equal(app.mval(bound.best_cell.added_latency_p99_us), 24999, "and the drawer/compare read the same one");
  assert.equal(bound.best_cell.added_latency_p99_us.headroom, 0.97, "the fraction of the rig ceiling reached travels with it");
  assert.equal(bound.best_cell.added_latency_p99_us.rig_ceiling, 25700, "as does the ceiling it is a fraction of");
  assert.deepEqual(checkConsistency({ gateways: [bound] }, app, SYNTH).errors, [],
    "the facts on the envelope are structurally clean: C1 accepts them, C2 finds no suppression");
});

// The HIGH class: a certified fallback value must NOT be suppressed. Run gen-data for real over a
// matrix with no swept diagonal (so the perf/xlate fallbacks fire) + certified legacy suites, then assert
// the SEALED envelope surfaces the certified 17,437 (never dropped by a lost mock_bound flag). The
// 17,437 is a real measured anthropic-in/openai-out throughput from the field; the fixture gateway is
// synthetic because the class is about the SEAL, not about who produced the number.
test("HIGH class: a certified xlate-fallback value survives the seal and reaches the table", () => {
  const root = mkdtempSync(join(tmpdir(), "site-fb-"));
  mkdirSync(join(root, "gateways", "fbgw"), { recursive: true });
  writeFileSync(join(root, "gateways", "fbgw", "definition.json"),
    JSON.stringify({ name: "fbgw", display: "fbgw", lang: "Rust", class: "Gateway", model: "m", port: 1,
      path: "/v1/chat/completions", auth: "dummy", egress: ["openai"],
      matrix: ["100000", "000000", "000000", "000000", "000000", "000000"] }));
  const iso = new Date(Date.now() - 3600000).toISOString();
  mkdirSync(join(root, "results", "matrix"), { recursive: true });
  // matrix served but its diagonal has no perf → bestCell()/translationCell() null → the fallbacks fire.
  writeFileSync(join(root, "results", "matrix", "fbgw.json"), JSON.stringify({
    gateway: "fbgw", build: "ok", matrix_version: 2, served: true, measured_at: iso,
    upstreams: { openai: { configurable: true, served: true, cells: { openai: { served: true } } } },
    cells: { openai: { served: true } },
  }));
  mkdirSync(join(root, "results", "perf"), { recursive: true });
  writeFileSync(join(root, "results", "perf", "fbgw.json"), JSON.stringify({
    gateway: "fbgw", build: "ok", served: true, measured_at: iso,
    added_latency_p50_us: 10, added_latency_p99_us: 20,
    rps_sustained_20ms: 19286, rps_sustained_20ms_concurrency: 576, rps_sustained_20ms_mock_bound: false,
    rps_max_proxy: 19721, rps_max_proxy_concurrency: 96, rps_max_proxy_mock_bound: false,
  }));
  mkdirSync(join(root, "results", "xlate"), { recursive: true });
  writeFileSync(join(root, "results", "xlate", "fbgw.json"), JSON.stringify({
    gateway: "fbgw", build: "ok", xlate_served: true, measured_at: iso,
    xlate_added_latency_p50_us: 15, xlate_added_latency_p99_us: 30,
    xlate_rps_sustained_20ms: 17437, xlate_rps_sustained_20ms_concurrency: 1024,
    xlate_rps_sustained_20ms_mock_bound: false,
  }));
  const outDir = mkdtempSync(join(tmpdir(), "site-fb-out-"));
  let bundle;
  try {
    execFileSync(process.execPath, [join(HERE, "gen-data.mjs"), root, outDir], { stdio: "pipe" });
    bundle = JSON.parse(readFileSync(join(outDir, "data.json"), "utf8"));
  } finally {
    rmSync(outDir, { recursive: true, force: true });
    rmSync(root, { recursive: true, force: true });
  }
  const g = bundle.gateways.find((x) => x.key === "fbgw");
  assert.ok(g, "fbgw present");
  // Provenance is honestly stamped as the fallback (source.kind), NOT mislabelled matrix.
  assert.equal(g.best_cell.source.kind, "perf-fallback", "best_cell stamped perf-fallback");
  assert.equal(g.translation_cell.source.kind, "xlate-fallback", "translation_cell stamped xlate-fallback");
  // The certified LATENCY values are sealed as certified envelopes (value present) - never suppressed.
  assert.equal(app.mval(g.best_cell.added_latency_p99_us), 20, "certified perf-fallback latency survives");
  assert.equal(app.mval(g.translation_cell.added_latency_p99_us), 30, "certified xlate-fallback latency survives (the HIGH class)");
  /* AND THE THROUGHPUT PUBLISHES NOTHING AT ALL, on purpose. A legacy suite record carries one scalar taken
     under one chosen ceiling; the frontier is six readings off a sweep the suite never recorded, so there is
     nothing to project. gen-data emits `frontier: []` rather than dressing the old 19,286 as one bound's
     reading - which would be the retired defect exactly, a number published under a bound it was not
     measured at. THE UI'S JOB HERE IS TO SHOW NO THROUGHPUT, never a zero and never a blank that reads as
     one, and that is what is asserted. */
  assert.deepEqual(g.best_cell.frontier, [], "a perf-suite fallback projects NO frontier");
  assert.deepEqual(g.translation_cell.frontier, [], "an xlate-suite fallback projects NO frontier");
  const cell = app.frontierCell(g.best_cell, app.DEFAULT_BOUND_MS);
  assert.equal(cell.na, true, "no frontier reads as an absence");
  assert.equal(cell.v, null, "and carries no value at all - not a 0");
  assert.match(cell.text, /no frontier/, "the cell says so on its face, not only in a tooltip");
  assert.match(cell.note, /not the same as a throughput of zero/);
  // The retired scalars are GONE from the bundle: a fallback row cannot smuggle them back in.
  assert.ok(!JSON.stringify(bundle).includes("rps_sustained_20ms"), "no rps_sustained_20ms survives anywhere");
  assert.ok(!JSON.stringify(bundle).includes("rps_max_proxy"), "no rps_max_proxy survives anywhere");
  // And C1 holds: no _mock_bound flag survives anywhere in the emitted bundle.
  assert.ok(!JSON.stringify(bundle).includes("_mock_bound"), "no *_mock_bound flag survives the seal");
});

/* WAS: "a zero RPS cell renders 0 with the no-qualifying-ceiling tooltip". That test asserted that a
   measured `rps_max_proxy` of 0 rendered as an honest "0" annotated "no tested load held p99 < 1 s at
   <0.1% errors". BOTH HALVES ARE GONE: the metric is deleted, and the sentence was wrong twice over (no
   gate enforced 1 s, and the frontier grants no error tolerance at all). There is no throughput zero left
   to render - a bound no rung qualified at is an ABSENCE carrying the engine's own reason.
   So the property is inverted into the guard that the zero cannot come back, and the two absences the
   engine distinguishes are asserted to render apart, which is the finding that replaced it. */
test("a throughput hole is an ABSENCE with the engine's reason, never a zero", () => {
  const st = { ...app.newState(), mode: "peak" };
  /* THE ENGINE DISTINGUISHES TWO ABSENCES and the board must render them apart (frontier.rs
     `absence_for`: "the two cases are genuinely different and the old code published one token for both").
     (a) NOTHING SERVED CLEANLY ANYWHERE - a fact about the GATEWAY, and a genuine hole: no value at all. */
  const nothing = { best_cell: bcCell({ dialect: "openai", frontier: { 1: null, 10: 18, none: 20 },
    frontierOpts: { absent: { reason: "not_measured", detail: "no concurrency in this sweep served every request it accepted, across 9 rung(s) probed" } } }) };
  const at1 = app.frontierCell(nothing.best_cell, 1);
  assert.equal(at1.na, true, "nothing served cleanly is an absence");
  assert.equal(at1.v, null, "with NO value - a 0 here would claim a measurement of zero throughput");
  assert.notEqual(at1.text, "0");
  assert.match(at1.note, /served every request it accepted/, "the engine's own reason travels to the cell");
  /* (b) SERVED CLEANLY, BUT NOTHING HELD THIS BOUND - a fact about the BOUND, and the engine states it as
     below_resolution with the prose "carried no measurable throughput under that bound". That IS a measured
     zero for this bound, so it displays as the one accessor displays below_resolution ("≈0", ranking 0) -
     the reader sees a gateway that carried nothing under a tight tail, which is the finding, not a hole. */
  const tooSlow = { best_cell: bcCell({ dialect: "openai", frontier: { 1: null, 10: 18, none: 20 } }) };
  const slow1 = app.frontierCell(tooSlow.best_cell, 1);
  assert.equal(slow1.v, 0, "no rung held this bound ranks as zero throughput under it");
  assert.equal(slow1.text, "0");
  // AND IT SAYS SO ON THE CELL. On the field data plano is this state at five of six bounds; a bare cell
  // there would read as five missing measurements instead of the one damning finding it is.
  assert.equal(slow1.why, "no rung held this tail");
  assert.match(app.metricTd(slow1), /no rung held this tail/, "the reason renders in the td, not only on hover");
  assert.match(slow1.note, /tail latency at or above/, "and the engine's own prose is the tooltip");
  const hole = tooSlow;
  // (b) A REAL READING still reads as one, at the bound it was taken at, with its concurrency inline.
  const cols = app.COLUMN_SETS.performance;
  const rps = cols.find((c) => c.id === "rps").get(hole, st);
  assert.equal(rps.text, "18 @ 512 conc");
  assert.match(rps.note, /while 99% of requests finished under 10 ms/);
  // (c) AND THE RETIRED VOCABULARY IS UNREACHABLE: no surface can name a bound the run did not enforce.
  assert.ok(!Object.values(app.METRIC_NOTES).some((s) => /p99 < 1 s|<0\.1% errors/.test(s)),
    "no metric note may assert the retired throughput gate's fabricated bar");
  for (const tok of ["mock_bound", "unverifiable", "paced_match"])
    assert.ok(!(tok in app.METRIC_NOTES), `${tok} is retired vocabulary and must not be renderable`);
});

// ---- check-consistency: STRUCTURAL INVARIANTS C1–C5 + R1 oracle + R2 coverage ------------------------
// The onthebench 11th-phase test. Each invariant has a RED-before test that reintroduces the dishonesty
// on a clone of the real bundle and asserts the SPECIFIC invariant fails (revert-the-seal → class fails).
const clone = () => structuredClone(data);
// The RED self-tests below revert one seal on a clone of the REAL bundle, so they need a donor row that
// actually publishes a matrix-sourced best_cell. When none exists they used to hand back `undefined` and
// die on `g.best_cell` with a bare TypeError - which reads as "the guard is broken" when the truth is
// "this board published nothing to revert". Fail with the precondition named instead, so a genuine
// regression stays loud and a dataless board is diagnosable at a glance. (BOARD_HAS_DATA gates these
// tests off entirely in that case; this is the belt-and-braces message if the two ever disagree.)
const matrixGw = (d) => {
  const g = d.gateways.find((x) => x.best_cell && x.best_cell.source && x.best_cell.source.kind === "matrix");
  assert.ok(g, "RED self-test precondition: no gateway in the bundle publishes a matrix-sourced best_cell, " +
    "so there is no seal to revert (board is dataless, not dishonest)");
  return g;
};
// THE DONOR MUST BE THE ONE THE TEST CAN ACTUALLY USE, not merely the first matrix row. The
// oracle-surface test corrupts one envelope on each of three surfaces, so it needs the row that
// carries the most of them - picking the first matrix publisher hands it a one-cell gateway like
// one-api and the test fails on a board that is behaving perfectly. Falls back to the first matrix
// row so its own precondition assert still speaks when the board has nothing at all.
const richestMatrixGw = (d) => {
  const ranked = (d.gateways || [])
    .map((g) => [donorSurfaces(g), g])
    .filter(([n]) => n > 0)
    .sort((a, b) => b[0] - a[0]);
  return ranked.length ? ranked[0][1] : matrixGw(d);
};

testWithData("consistency guard: the real bundle satisfies the sealed-envelope invariants C1–C5", () => {
  const { errors, warnings } = checkConsistency(data, app);
  for (const w of warnings) console.warn(`  warn - ${w}`);
  assert.deepEqual(errors, [], `structural-invariant violations:\n${errors.join("\n")}`);
});

testWithMatrixDonor("R2 coverage: every REQUIRED invariant branch is exercised by the real bundle (no inert check)", () => {
  const { cover, REQUIRED, CHECK_BRANCHES } = checkConsistency(data, app);
  assert.ok(Array.isArray(REQUIRED) && REQUIRED.length, "REQUIRED branch set is declared");
  for (const b of REQUIRED) assert.ok(cover.has(b), `required invariant branch not exercised: ${b}`);
  // the declared branch set is a superset of what a healthy bundle exercises
  for (const b of cover) assert.ok(CHECK_BRANCHES.includes(b), `covered branch ${b} not in CHECK_BRANCHES`);
  // THE SET THIS TEST WALKS MUST BE THE SET THE BUNDLE OWES.
  //
  // checkConsistency used to return the unconditional nine-branch floor under the name REQUIRED while
  // enforcing a wider set internally, so this loop - the only place anything iterates REQUIRED - had
  // never heard of R1.oracle, C6.cell, C7.hwm or R3.selection. Deleting the line that tags the
  // independent oracle's coverage therefore failed nothing here. A matrix-publishing bundle owes all
  // four, so assert they are IN the set as well as covered by it; a future narrowing of REQUIRED can
  // no longer quietly narrow what this test checks.
  for (const b of ["R1.oracle", "C6.cell", "C7.hwm", "R3.selection"])
    assert.ok(REQUIRED.includes(b),
      `a matrix-publishing bundle must be REQUIRED to exercise ${b}; REQUIRED = ${JSON.stringify(REQUIRED)}`);
});

// ---- THE ANTI-INERT GATE MUST NOT ITSELF BE INERT (round-2 audit) --------------------------------
// R2's coverage finding is downgraded to a warning while the board is still filling, for a good
// reason (a branch whose input has not landed is not a branch that went dead). On today's 5-of-14
// board that made the ERROR arm unreachable, and with it every consequence of switching a check OFF:
// commenting out the single line that tags R1.oracle left BOTH `node check-consistency.mjs` and
// `node test.mjs` exiting 0. The independent oracle could be disabled outright and the publish gate
// stayed green. Wiring is a fact about the SOURCE, not about the board, so it is checked statically
// and never downgraded.
test("R2 WIRING: a switched-off check is caught regardless of how full the board is", () => {
  const branches = ["A.one", "B.two"];
  const wired = 'if (x) covered("A.one");\nif (y) covered("B.two");\n';
  assert.deepEqual(checkMod.lintCoverageWiring(wired, branches).errors, [],
    "a fully wired source has nothing to report");

  // (1) DELETED call site.
  const deleted = 'if (x) covered("A.one");\n';
  const eDel = checkMod.lintCoverageWiring(deleted, branches).errors;
  assert.equal(eDel.length, 1);
  assert.match(eDel[0], /B\.two.*NO live call site/);

  // (2) COMMENTED-OUT call site - the way a check is really switched off, and the way the round-2
  // audit proved the oracle could be disabled. A `//` before the tag means the tag is not there.
  const commented = 'if (x) covered("A.one");\n// if (y) covered("B.two");\n';
  const eCom = checkMod.lintCoverageWiring(commented, branches).errors;
  assert.equal(eCom.length, 1, `a commented-out tag is not a call site; got ${JSON.stringify(eCom)}`);
  assert.match(eCom[0], /B\.two/);
  // ...but a trailing comment AFTER a live tag is still a live tag.
  assert.deepEqual(checkMod.lintCoverageWiring(
    'covered("A.one");\ncovered("B.two");   // the scanner ran\n', branches).errors, []);

  // (3) The other direction: a tag nobody declared can never be REQUIRED of anything.
  const undeclared = checkMod.lintCoverageWiring(wired + 'covered("C.three");\n', branches).errors;
  assert.equal(undeclared.length, 1);
  assert.match(undeclared[0], /C\.three.*not declared in CHECK_BRANCHES/);

  // (4) And the REAL file is fully wired: every branch it declares still has a live tag.
  const real = readFileSync(join(HERE, "check-consistency.mjs"), "utf8");
  const { CHECK_BRANCHES } = checkConsistency(data, app);
  assert.deepEqual(checkMod.lintCoverageWiring(real, CHECK_BRANCHES).errors, [],
    "check-consistency.mjs must tag every branch it declares, and declare every branch it tags");

  // (5) THE PROOF THAT THIS REACHES THE PUBLISH GATE: switching a tag off in a copy of the real source
  // is an ERROR out of checkConsistency itself, on TODAY'S PARTIAL BOARD - not a warning, not
  // conditional on the board being complete. This is the assertion the round-2 audit found missing.
  const off = real.replace(/^(\s*)(if \(oracleCompared > 0\) covered\("R1\.oracle"\);)/m, "$1// $2");
  assert.notEqual(off, real, "the R1.oracle tag must be findable, or this proof is vacuous");
  const e = checkMod.lintCoverageWiring(off, CHECK_BRANCHES).errors;
  assert.ok(e.some((x) => x.includes("R1.oracle") && x.includes("the check is off")),
    `switching the oracle's coverage tag off must be an error; got ${JSON.stringify(e)}`);
});

// ---- R2's ERROR arm is reachable on an EMPTY bundle ----------------------------------------------
// The partial-board downgrade is for a board that is PUBLISHING and still filling. A bundle with zero
// publishers is not filling, it is empty, and every required branch being unexercised is precisely the
// inert-check failure R2 exists to report. Treating zero-of-fourteen as "partial" made the failure
// path unreachable and forced the suite to skip its own test of it.
test("R2: an EMPTY bundle takes the ERROR arm, not the still-filling warning", () => {
  const { errors, warnings } = checkConsistency({ gateways: [] }, app);
  const err = errors.find((e) => e.startsWith("R2: coverage"));
  assert.ok(err, `an empty bundle exercises nothing and must ERROR; got ${JSON.stringify(errors)}`);
  assert.match(err, /an inert check is itself a failure/);
  assert.ok(!warnings.some((w) => w.startsWith("R2: coverage")),
    "an empty bundle must not ALSO be excused as a board that is still filling");
});

testWithMatrixDonor("C1 RED: EVERY malformed metric shape fails C1, not just a bare number", () => {
  // C1's walk used to read `!isEnvelope(v) && typeof v === "number"`, so a raw numeric leak was the
  // ONLY thing it could catch. A bare string, a boolean, an array, or - the dangerous one - a
  // half-built envelope like {value: 20057} with `certified` dropped matched neither branch and sailed
  // through the deploy gate. `isEnvelope` demands a boolean `certified`, so a partial envelope is not
  // an envelope, and it is not a number either, so nothing fired. This test only ever drove the number.
  for (const [what, bad] of [
    ["a bare number", 20057],
    ["a bare string", "n/a"],
    ["a boolean", false],
    ["an array", [20057]],
    ["a partial envelope with no certified flag", { value: 20057 }],
  ]) {
    const d = clone();
    const g = matrixGw(d);
    // INJECTED ON A SURVIVING ENVELOPE. It used to be `rps_max_proxy`, which no producer emits; the
    // invariant is about the SHAPE of any sealed metric, so any metric field proves it.
    g.best_cell.added_latency_p99_us = bad;
    const e = checkConsistency(d, app).errors;
    assert.ok(
      e.some((x) => x.startsWith("C1:") && x.includes("added_latency_p99_us")),
      `C1 must flag ${what}; got: ${JSON.stringify(e.filter((x) => x.startsWith("C1")))}`
    );
  }
  // And the shape that must still PASS: a real sealed envelope. A rule that rejects everything is as
  // useless as one that rejects nothing.
  const ok = clone();
  const e = checkConsistency(ok, app).errors;
  assert.ok(!e.some((x) => x.startsWith("C1:")), `a clean bundle must raise no C1: ${JSON.stringify(e)}`);
});

testWithMatrixDonor("C1 RED: a surviving *_mock_bound flag fails C1", () => {
  const d = clone();
  const g = matrixGw(d);
  g.best_cell.added_latency_p99_us_mock_bound = false;   // the flag must have been consumed at seal time
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("C1:") && x.includes("_mock_bound")),
    `C1 must flag a surviving *_mock_bound flag; got: ${JSON.stringify(e.filter((x) => x.startsWith("C1")))}`);
});

testWithMatrixDonor("C2 RED: a suppressed metric that still exposes a value fails C2", () => {
  const d = clone();
  const g = matrixGw(d);
  // Injected on a surviving metric field, and with the retired suppression reason it used to carry: the
  // vocabulary is dead, and C2 is the guard that it stays dead whatever field it is smuggled in on.
  g.best_cell.added_latency_p99_us = { value: 19469, certified: false, suppressed: true, reason: "mock_bound" };
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("C2:") && x.includes("added_latency_p99_us")),
    `C2 must flag a suppressed metric that still carries a value; got: ${JSON.stringify(e.filter((x) => x.startsWith("C2")))}`);
});

testWithMatrixDonor("C3 RED: a caption stamp with no SWEEP_CAPTION renderer fails C3", () => {
  const d = clone();
  const g = matrixGw(d);
  g.best_cell.source.sweep = "6x6-bogus";   // a stamp the caption vocabulary does not know
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("C3:") && x.includes("6x6-bogus")),
    `C3 must flag an unknown source.sweep stamp; got: ${JSON.stringify(e.filter((x) => x.startsWith("C3")))}`);
});

testWithMatrixDonor("C4 RED: a leaked legacy suite object fails C4", () => {
  const d = clone();
  const g = matrixGw(d);
  g.perf = { served: true };   // a raw legacy suite object must never survive in the bundle
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("C4:") && x.includes(".perf") && x.includes("leaked")),
    `C4 must flag a leaked legacy suite object; got: ${JSON.stringify(e.filter((x) => x.startsWith("C4")))}`);
});

testWithMatrixDonor("C4 RED: an unknown source.kind (a re-added silent fallback) fails C4", () => {
  const d = clone();
  const g = matrixGw(d);
  g.best_cell.source.kind = "perf";   // not a known origin (matrix | *-fallback)
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("C4:") && x.includes("source.kind")),
    `C4 must flag an unknown source.kind; got: ${JSON.stringify(e.filter((x) => x.startsWith("C4")))}`);
});

testWithMatrixDonor("R1 RED: a best_cell envelope that disagrees with the RAW matrix cell fails the independent oracle", () => {
  const d = clone();
  const g = matrixGw(d);
  // corrupt the sealed headline so it no longer equals the raw matrix diagonal cell on disk.
  const cur = g.best_cell.added_latency_p99_us;
  g.best_cell.added_latency_p99_us = { value: (app.mval(cur) || 0) + 12345, certified: true, suppressed: false };
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("R1:") && x.includes(g.key) && x.includes("added_latency_p99_us")),
    `R1 must flag a headline that disagrees with the raw matrix cell; got: ${JSON.stringify(e.filter((x) => x.startsWith("R1")))}`);
});

/* ---- C6: THE FRONTIER'S ORDERING AND ITS DISCLOSURE, proven on INJECTED data --------------------
   WHAT THIS FAMILY USED TO TEST, AND WHY NONE OF IT SURVIVES AS WRITTEN. C6 compared `rps_sustained_20ms`
   against `rps_max_proxy` and failed the build when the "sustained" figure exceeded the "maximum" - with a
   whole severity band around it (the cell's own sweep scatter excused a small inversion, a gross-percentage
   ceiling capped what noise could excuse, a plateau median explained an inversion of up to half the
   scatter, and a peak that won at its top rung was flagged separately). Six tests pinned those edges.
   Both metrics are DELETED. The inversion they policed is now unrepresentable: a reading is a maximum over
   the rungs qualifying at its bound, relaxing a bound only ADDS rungs, so a looser reading cannot be
   smaller - which is why the elaborate band has nothing left to be a band around. The scatter machinery is
   gone with it, and so are the tests that pinned its edges: there is no chosen tolerance left to test.
   What C6 checks now is the same INVARIANT from the other side, and it is checked precisely because it is
   structural - an invariant nothing verifies is an invariant nobody notices breaking. The three claims:
   the bounds ascend with the unbounded reading last; the readings do not invert; and `lower_bound` agrees
   with the rungs actually probed. The third is the direct descendant of the retired "won at its top rung"
   test - the state is identical, but it is no longer a violation: a rate whose sweep ran out of ladder is
   published and DISCLOSED as a floor, and what must hold is that the disclosure is truthful. */
// A raw matrix carrying one cell whose frontier is the readings given, plus the sweep rungs they were read
// from (which is what `lower_bound` is checked against).
const c6Matrix = (readings, opts = {}) => ({
  upstreams: { openai: { cells: { openai: { served: opts.served !== false,
    perf: { frontier: readings, sweep_max_proxy: opts.sweep ?? [{ conc: 64, rps: 100 }, { conc: 128, rps: 90 }] } } } } },
});
// One raw reading, in the engine's own units (bounds in microseconds, `null` = the unbounded reading).
const rd = (boundMs, rps, o = {}) => ({
  p99_bound_us: boundMs == null ? null : boundMs * 1000,
  rps, concurrency: o.conc ?? 64, p99_us: o.p99_us ?? (boundMs == null ? 40_000 : boundMs * 400),
  first_disqualified_conc: o.firstDisq ?? 128, lower_bound: o.lowerBound === true,
});

test("C6 RED: a frontier that INVERTS is a hard failure - a looser bound cannot carry less", () => {
  // 5 ms reads lower than 1 ms. Relaxing the bound only adds rungs to the set the maximum is taken over,
  // so this cannot arise from the rungs it claims: it is a producer defect, not scatter.
  const r = c6Inversions("gw", c6Matrix([rd(1, 14351), rd(5, 14325), rd(null, 20000)]));
  assert.equal(r.cellsChecked, 1, "the cell must have been checked");
  assert.equal(r.violations.length, 1, `C6 must flag an inverted frontier; got: ${JSON.stringify(r.violations)}`);
  assert.ok(r.violations[0].includes("gw.openai->openai") && r.violations[0].includes("5ms")
    && r.violations[0].includes("14325") && r.violations[0].includes("14351"),
    `the violation must name the cell, the bound and both rates; got: ${r.violations[0]}`);
});

test("C6 RED: the bounds must ASCEND, with the unbounded reading last", () => {
  // Published out of order, the sequence stops reading as the tradeoff curve it is, and a reader checking
  // monotonicity by eye is checking the wrong order.
  const shuffled = c6Inversions("gw", c6Matrix([rd(5, 100), rd(1, 90), rd(null, 200)]));
  assert.ok(shuffled.violations.some((v) => /bounds are not ascending/.test(v)),
    `C6 must flag a non-ascending sequence; got: ${JSON.stringify(shuffled.violations)}`);
  // The unbounded reading in the MIDDLE is the same defect: it is the loosest reading there is.
  const misplaced = c6Inversions("gw", c6Matrix([rd(1, 90), rd(null, 200), rd(5, 100)]));
  assert.ok(misplaced.violations.some((v) => /bounds are not ascending/.test(v)),
    `the unbounded reading must be last; got: ${JSON.stringify(misplaced.violations)}`);
});

/* C6 RED: THE FLOOR DISCLOSURE MUST BE TRUTHFUL, in both directions.
   This is what became of "a peak sweep that WON at its top rung is an error": the state is real and
   common, it is no longer an error, and the rate is published either way - so what must hold is that the
   artifact says WHICH it is. A reading that won at the top of the ladder and does not say so publishes our
   own range as the gateway's answer; one that says so when the curve turned over inside the range
   understates a peak it did establish. */
test("C6 RED: lower_bound must agree with the rungs actually probed, in both directions", () => {
  const sweep = [{ conc: 64, rps: 100 }, { conc: 256, rps: 120 }];
  // Won at c=256, the top rung probed, but claims a ceiling.
  const claimed = c6Inversions("gw", c6Matrix([rd(10, 120, { conc: 256 })], { sweep }));
  assert.ok(claimed.violations.some((v) => /lower_bound=false/.test(v) && /c=256 of 256 probed/.test(v)),
    `a reading at the top of the ladder must disclose it; got: ${JSON.stringify(claimed.violations)}`);
  // Won at c=64 with a rung probed above it, but claims to be only a floor.
  const understated = c6Inversions("gw", c6Matrix([rd(10, 100, { conc: 64, lowerBound: true })], { sweep }));
  assert.ok(understated.violations.some((v) => /lower_bound=true/.test(v)),
    `a peak established inside the range must not be published as a floor; got: ${JSON.stringify(understated.violations)}`);
  // And the truthful pair raises nothing.
  assert.equal(c6Inversions("gw", c6Matrix([rd(10, 120, { conc: 256, lowerBound: true })], { sweep })).violations.length, 0);
  assert.equal(c6Inversions("gw", c6Matrix([rd(10, 100, { conc: 64 })], { sweep })).violations.length, 0);
});

test("C6 GREEN: a monotone frontier, an absent reading and an unserved cell are NOT flagged", () => {
  // The ordinary shape: equal readings across a range the gateway holds flat, then a gain when the tail is
  // let out. Equality is not an inversion.
  const ok = c6Inversions("gw", c6Matrix([rd(1, 14325), rd(5, 14325), rd(10, 14351), rd(null, 20000)]));
  assert.equal(ok.violations.length, 0, `a monotone frontier is clean; got: ${JSON.stringify(ok.violations)}`);
  // A reading whose RATE is absent (no rung held that bound) is skipped by the ordering check rather than
  // read as a zero that would invert against everything after it.
  const withHole = c6Inversions("gw", c6Matrix([rd(1, null), rd(5, 14325), rd(null, 20000)]));
  assert.equal(withHole.violations.length, 0, `an absent reading is not an inversion; got: ${JSON.stringify(withHole.violations)}`);
  // A cell with NO frontier at all is not a checked cell: there is nothing to order.
  const none = c6Inversions("gw", c6Matrix([]));
  assert.equal(none.cellsChecked, 0, "a cell with no frontier is not a checked cell");
  // an UNSERVED cell carries no honest perf to compare.
  assert.equal(c6Inversions("gw", c6Matrix([rd(1, 100)], { served: false })).cellsChecked, 0, "unserved cells are skipped");
  // null-safe on absent/edge inputs (older snapshots, a not-served matrix).
  for (const bad of [null, undefined, {}, { upstreams: null }, { upstreams: {} }])
    assert.equal(c6Inversions("gw", bad).cellsChecked, 0, `C6 must be null-safe on ${JSON.stringify(bad)}`);
});

// ---- C7: the sampled peak can never exceed the kernel's own high-water mark ----------------------
// Found in this run's shipped data: one gateway at 165.1 > 164.7, another at 45.0 > 44.7. VmHWM is
// maintained by the kernel on every charge, so for a FIXED process tree it cannot sit below any RSS the
// sampler observed. Both readers sum over the tree ENUMERATED AT READ TIME, and the two reads happen at
// different instants - so a worker that exits between the load and the VmHWM read is counted in the
// sampled peak and missing from the HWM sum. A real transient-child artefact on multi-process gateways,
// not a fabricated number: it WARNS so the next run can attribute it, and no measured value is rewritten.
const c7Mem = (peak, hwm, served = true) => ({ memory: { served, peak_rss_mib: peak, peak_rss_hwm_mib: hwm } });

test("C7 RED: a sampled peak above the kernel HWM is flagged as a warning", () => {
  const r = c7HwmBelowPeak("gw", c7Mem(165.1, 164.7));
  assert.equal(r.checked, 1);
  assert.equal(r.warnings.length, 1, `C7 must flag peak > hwm; got ${JSON.stringify(r.warnings)}`);
  assert.ok(r.warnings[0].includes("gw.memory") && r.warnings[0].includes("0.24%")
    && r.warnings[0].includes("transient-worker artefact"),
    `the C7 warning must name the gateway, the overshoot and the mechanism; got: ${r.warnings[0]}`);
});

test("C7 GREEN: a plausible pair, equality, an unserved window and nulls are NOT flagged", () => {
  assert.equal(c7HwmBelowPeak("gw", c7Mem(208.3, 212.8)).warnings.length, 0, "hwm above peak is correct");
  assert.equal(c7HwmBelowPeak("gw", c7Mem(263.0, 263.0)).warnings.length, 0, "equality is not an overshoot");
  assert.equal(c7HwmBelowPeak("gw", c7Mem(45, 44.7, false)).checked, 0, "an unserved window is not checked");
  // NULL-SAFE: an honest-null memory window (a gateway that served no cell) must never be flagged, and
  // must never be counted as checked - the honest-null path is correct data, not a violation.
  for (const bad of [null, undefined, {}, { memory: null }, c7Mem(null, null), c7Mem(45, null), c7Mem(null, 44.7)])
    assert.equal(c7HwmBelowPeak("gw", bad).checked, 0, `C7 must be null-safe on ${JSON.stringify(bad)}`);
});

test("C7 RED: memory lives PER CELL now, and a new-shape matrix must still be checked", () => {
  // THE SHAPE THE PRODUCER ACTUALLY WRITES. matrix/run.sh emits no top-level `memory` key at all: the
  // window is folded into each served cell. C7 read only the top-level block, so on every artifact the
  // current producer writes it checked NOTHING - and "C7.hwm" is a REQUIRED coverage token once a bundle
  // publishes matrix numbers, which a per-cell memory row is itself enough to make true. An all-new-shape
  // field run would therefore satisfy the requirement and starve the token in the same breath, turning 13
  // freshly measured gateways into a hard publish failure. This fixture is that field run in miniature.
  const perCell = (peak, hwm, served = true) => ({
    upstreams: { openai: { cells: {
      openai: { served: true, memory: { served, peak_rss_mib: peak, peak_rss_hwm_mib: hwm } },
      anthropic: { served: true, memory: { served, peak_rss_mib: 100, peak_rss_hwm_mib: 120 } },
    } } },
  });
  const r = c7HwmBelowPeak("gw", perCell(165.1, 164.7));
  assert.equal(r.checked, 2, "EVERY served cell's window is checked, not just one");
  assert.equal(r.warnings.length, 1, `C7 must flag the inverted cell; got ${JSON.stringify(r.warnings)}`);
  assert.match(r.warnings[0], /gw\.openai->openai\.memory/, "the warning must name the CELL, not just the gateway");
  assert.match(r.warnings[0], /0\.24%/);
  // A new-shape matrix with NO top-level block must still produce coverage: that is the whole failure.
  assert.ok(c7HwmBelowPeak("gw", perCell(100, 120)).checked > 0,
    "a per-cell-only matrix must COVER C7, or the publish gate starves on fresh data");
  // v2 shares its cell objects with the top-level compat `cells` row; they must not be counted twice.
  const v2 = perCell(100, 120);
  v2.cells = v2.upstreams.openai.cells;
  assert.equal(c7HwmBelowPeak("gw", v2).checked, 2, "a v2 compat row must not double-count its cells");
  // and a LEGACY top-level block is still checked, so pre-redesign artifacts do not go dark.
  assert.equal(c7HwmBelowPeak("gw", { memory: { served: true, peak_rss_mib: 165.1, peak_rss_hwm_mib: 164.7 } }).checked,
    1, "the legacy top-level block must keep being checked");
});
testWithData("C7: the live bundle's hwm-below-peak rows warn but never hard-fail", () => {
  const { errors, warnings, cover } = checkConsistency(data, app);
  assert.ok(cover.has("C7.hwm"), "C7 must actually run on the live bundle");
  assert.ok(!errors.some((x) => x.includes("peak_rss_hwm")), "C7 must never hard-fail an honest publish");
  for (const w of warnings.filter((x) => x.includes("peak_rss_hwm")))
    assert.match(w, /sampled peak_rss [\d.]+ MiB > kernel peak_rss_hwm [\d.]+ MiB/);
});

// C6 IS A HARD FAILURE. It was a warning on the theory that two independently-swept ceilings overlap
// on noise. That theory was wrong and it hid a real defect: the two sweeps searched different
// concurrency ranges, so the peak search terminated on its own bound rather than on the gateway. Both
// sweeps now share one SWEEP constant, so an inversion has no benign mechanism left. Whatever
// inversions the live bundle still carries must therefore reach ERRORS, and must be well formed.
test("C6 on the live bundle: every inversion is adjudicated against its own cell's scatter, and says which", () => {
  const { errors, warnings } = checkConsistency(data, app);
  const keys = new Set(data.gateways.map((g) => g.key));
  const c6 = [...errors, ...warnings].filter((x) => x.includes("sustained@20ms"));
  for (const m of c6) {
    assert.ok(keys.has(m.split(".")[0]), `a C6 message must name a gateway in the bundle; got: ${m}`);
    // NO SILENT TOLERANCE: whichever channel it lands in, the message must carry the magnitude AND the
    // basis for the verdict, so a reader can check the judgement instead of trusting it.
    assert.match(m, /% inversion/, `a C6 message must state the magnitude; got: ${m}`);
    assert.match(m, /scatter|too few rungs|ceiling on excusable noise/,
      `a C6 message must state the basis it was judged against; got: ${m}`);
  }
  // A C6 message routed to the soft channel must be there BECAUSE it fell inside the band, never for
  // any other reason.
  for (const w of warnings.filter((x) => x.includes("sustained@20ms")))
    assert.match(w, /within this cell's own max-proxy sweep scatter/,
      `only a within-band inversion may warn; got: ${w}`);
});

// ---- matrix is the single source: streaming + memory projection + download ----------------------
// A representative diagonal cell's streaming record + a top-level matrix memory read, as matrix/run.sh
// now emits them. gen-data must project g.streaming (best diagonal cell's stream) + g.memory_read.
const STREAM_CELL = {
  stream_served: true, added_ttft_p50_us: 40, added_ttft_p99_us: 90,
  added_gap_p50_us: 5, added_gap_p99_us: 12, streams_sustained: 1300, streams_sustained_fps: 39000,
  // NO `cpu_fps` / `cpu_fps_concurrency`: the producer retired the metric (it counted relay frames/sec
  // without the delivery gate, so dropping frames could raise the score). A fixture that still offered it
  // would be a raw artifact shape the engine cannot emit.
  streams_sustained_mock_bound: false,
};
/* rawFrontier(spec): the frontier as the ENGINE writes it into a raw artifact - bounds in MICROSECONDS
   under `p99_bound_us`, rates as bare numbers - which is the shape gen-data's sealFrontier consumes. The
   sealed, bound_ms-in-milliseconds shape is what fxFrontier above builds, and the two are deliberately kept
   apart: a fixture that fed the SEALED shape into gen-data would test the seal against itself. */
function rawFrontier(spec) {
  return FRONTIER_SLOTS.filter((k) => Object.prototype.hasOwnProperty.call(spec, k)).map((k) => ({
    p99_bound_us: k === "none" ? null : k * 1000,
    rps: spec[k],
    concurrency: 512,
    p99_us: k === "none" ? 40_000 : k * 400,
    first_disqualified_conc: 1024,
    lower_bound: false,
  }));
}
// CELL_MEM: one served cell's own memory window, as matrix/run.sh emits it (RAW scalars - gen-data seals
// them in place). This is the ONLY shape memory ships in now: per cell, cold-started, plateau-terminated.
const CELL_MEM = { served: true, protocol: "per-cell, own cold-started process", serve_error: "",
  load_recipe: { concurrency: 64, payload_bytes: 4096 },
  idle_rss_mib: 120.5, steady_state_rss_mib: 890.2, recovered_rss_mib: 130.0,
  peak_rss_mib: 892.0, peak_rss_hwm_mib: 910, plateaued: true, time_to_plateau_s: 95,
  growth_rate_mib_per_min: 0.2, load_s: 155, idle_window_s: 60, recovery_window_s: 60,
  rss_series: [ { t_s: 0, rss_mib: 120.5 }, { t_s: 60, rss_mib: 890.2 }, { t_s: 180, rss_mib: 130.0 } ] };

function buildStreamMemRepo() {
  const root = mkdtempSync(join(tmpdir(), "site-strm-"));
  mkdirSync(join(root, "gateways", "sgw"), { recursive: true });
  writeFileSync(join(root, "gateways", "sgw", "definition.json"),
    JSON.stringify({ name: "sgw", display: "sgw", lang: "Rust", class: "Gateway", model: "m", port: 1,
      path: "/v1/chat/completions", auth: "dummy", egress: ["openai"],
      matrix: ["100000", "000000", "000000", "000000", "000000", "000000"] }));
  mkdirSync(join(root, "results", "matrix"), { recursive: true });
  const iso = new Date(Date.now() - 3600000).toISOString();
  // The RAW cell shape the engine emits: one frontier off one sweep. `sweep_max_proxy` keeps its producer
  // name (it is the rung array gen-data carries onto the record as `sweep`); the two retired scalars and the
  // second sweep array are gone with the metrics that needed them.
  const perf = { added_latency_p50_us: 10, added_latency_p99_us: 20,
    frontier: rawFrontier({ 1: 40000, 10: 45000, none: 50000 }),
    sweep_max_proxy: [{ conc: 256, rps: 50000, p99_us: 100, fail: 0 }] };
  const cellMem = { ...CELL_MEM };   // ONE object, shared by both views, so a mutator reaches the cell
  const matrix = {
    gateway: "sgw", build: "ok", matrix_version: 2, served: true, measured_at: iso,
    memory: { served: true, idle_rss_mib: 120.5, peak_rss_mib: 890.2, peak_rss_hwm_mib: 910, post_load_rss_mib: 300,
      recovered_rss_mib: 130.0, rss_series: [ { t_s: 0, rss_mib: 120.5 }, { t_s: 60, rss_mib: 890.2 }, { t_s: 180, rss_mib: 130.0 } ] },
    upstreams: { openai: { configurable: true, served: true, cells: {
      openai: { served: true, perf, stream: { ...STREAM_CELL }, memory: cellMem } } } },
    cells: { openai: { served: true, perf, stream: { ...STREAM_CELL }, memory: cellMem } },
  };
  writeFileSync(join(root, "results", "matrix", "sgw.json"), JSON.stringify(matrix));
  return root;
}
function genInto(root) {
  const outDir = mkdtempSync(join(tmpdir(), "site-strm-out-"));
  try {
    execFileSync(process.execPath, [join(HERE, "gen-data.mjs"), root, outDir], { stdio: "pipe" });
    return JSON.parse(readFileSync(join(outDir, "data.json"), "utf8"));
  } finally {
    rmSync(outDir, { recursive: true, force: true });
    rmSync(root, { recursive: true, force: true });
  }
}

// translationCell()'s FAIR tier (openai ingress, so every gateway's translation number shares the same
// input side) has a second tier that fires only when the matrix has NO openai-ingress cross-dialect
// cell at all: ANY served cross-dialect cell it did measure (see the two-tier selection in gen-data.mjs).
// Nothing else drives the matrix into the shape that makes the "any" tier the ONLY candidate, so a
// regression that deleted the `any` array entirely (leaving only `fair`) would keep every other test green.
test("translationCell falls through to the ANY tier when no openai-ingress cell exists", () => {
  const root = buildStreamMemRepo();
  const mpath = join(root, "results", "matrix", "sgw.json");
  const m = JSON.parse(readFileSync(mpath, "utf8"));
  // Only a gemini-in/openai-out cell: cross-dialect, but its ingress is NOT openai, so it can only
  // ever be picked by the "any" tier, never the "fair" one.
  m.upstreams.openai.cells.gemini = {
    served: true,
    perf: { added_latency_p50_us: 40, added_latency_p99_us: 90, frontier: rawFrontier({ 10: 12000, none: 13000 }) },
  };
  writeFileSync(mpath, JSON.stringify(m));
  const g = genInto(root).gateways.find((x) => x.key === "sgw");
  assert.ok(g.translation_cell, "the any-tier candidate must still produce a translation cell");
  assert.equal(g.translation_cell.path.ingress, "gemini");
  assert.equal(g.translation_cell.path.egress, "openai");
  assert.equal(app.mval(g.translation_cell.added_latency_p99_us), 90);
});

// The FAIR tier must win even when an "any"-tier candidate would look better by the tie-break metric
// (lowest p99): tier comes first, the metric only breaks ties WITHIN a tier. Otherwise gateways would
// not be compared on the same input side after all, which is the whole reason the FAIR tier exists.
test("translationCell prefers the FAIR (openai-ingress) tier even when an ANY candidate has lower latency", () => {
  const root = buildStreamMemRepo();
  const mpath = join(root, "results", "matrix", "sgw.json");
  const m = JSON.parse(readFileSync(mpath, "utf8"));
  // Fair candidate: openai in -> anthropic out (m.upstreams is keyed by EGRESS, cells by INGRESS), higher latency.
  m.upstreams.anthropic = { configurable: true, served: true, cells: { openai: {
    served: true,
    perf: { added_latency_p50_us: 50, added_latency_p99_us: 200, frontier: rawFrontier({ 10: 9000, none: 9500 }) },
  } } };
  // Any-only candidate: gemini in -> openai out, LOWER latency, but not eligible for the fair tier.
  m.upstreams.openai.cells.gemini = {
    served: true,
    perf: { added_latency_p50_us: 10, added_latency_p99_us: 20, frontier: rawFrontier({ 10: 12000, none: 13000 }) },
  };
  writeFileSync(mpath, JSON.stringify(m));
  const g = genInto(root).gateways.find((x) => x.key === "sgw");
  assert.equal(g.translation_cell.path.ingress, "openai", "the fair tier must win on tier, not on latency");
  assert.equal(g.translation_cell.path.egress, "anthropic");
  assert.equal(app.mval(g.translation_cell.added_latency_p99_us), 200);
});

// A BELOW-RESOLUTION added latency is the engine's BEST outcome (the difference was at or under what
// the rig can resolve), so it must RANK as 0 in bestCell's lowest-p99 choice, never as the Infinity a
// plain missing number sorts as. Non-openai diagonals only: bestCell prefers the openai diagonal
// deterministically, which would mask the ranking under test.
test("bestCell ranks a below-resolution p99 as 0, beating a measured diagonal", () => {
  const root = buildStreamMemRepo();
  const mpath = join(root, "results", "matrix", "sgw.json");
  const m = JSON.parse(readFileSync(mpath, "utf8"));
  delete m.cells;
  m.upstreams = {
    // The winner: served, p99 null, and the engine's absences entry says WHY - below the rig's resolution.
    anthropic: { configurable: true, served: true, cells: { anthropic: { served: true,
      perf: { added_latency_p50_us: null, added_latency_p99_us: null, frontier: rawFrontier({ 10: 30000 }) },
      absences: {
        "perf.added_latency_p50_us": { reason: "below_resolution", detail: "difference at or under the rig's resolution" },
        "perf.added_latency_p99_us": { reason: "below_resolution", detail: "difference at or under the rig's resolution" },
      } } } },
    // The measured competitor: a real 350us p99, which rank 0 must beat.
    gemini: { configurable: true, served: true, cells: { gemini: { served: true,
      perf: { added_latency_p50_us: 200, added_latency_p99_us: 350, frontier: rawFrontier({ 10: 45000 }) } } } },
  };
  writeFileSync(mpath, JSON.stringify(m));
  const g = genInto(root).gateways.find((x) => x.key === "sgw");
  assert.ok(g.best_cell, "the below-resolution diagonal must still produce a best_cell");
  assert.equal(g.best_cell.path.dialect, "anthropic", "rank 0 (below resolution) must beat a measured 350");
  assert.equal(g.best_cell.added_latency_p99_us.reason, "below_resolution", "the envelope carries the engine's reason");
  assert.equal(app.mval(g.best_cell.added_latency_p99_us), 0, "the site ranks/renders it as 0");
});

// The same win must QUALIFY a translation cell: a served cross-dialect cell whose p99 is below
// resolution is a measured matrix result, and it must be selected - not silently dropped so the
// legacy xlate suite's stale number publishes over the matrix (matrix-sole-source).
test("translationCell selects a below-resolution matrix cell instead of falling back to legacy xlate", () => {
  const root = buildStreamMemRepo();
  const mpath = join(root, "results", "matrix", "sgw.json");
  const m = JSON.parse(readFileSync(mpath, "utf8"));
  // openai in -> anthropic out, p99 below the rig's resolution (the best a translation cell can read).
  m.upstreams.anthropic = { configurable: true, served: true, cells: { openai: { served: true,
    perf: { added_latency_p50_us: null, added_latency_p99_us: null, frontier: rawFrontier({ 10: 8000 }) },
    absences: {
      "perf.added_latency_p50_us": { reason: "below_resolution", detail: "difference at or under the rig's resolution" },
      "perf.added_latency_p99_us": { reason: "below_resolution", detail: "difference at or under the rig's resolution" },
    } } } };
  writeFileSync(mpath, JSON.stringify(m));
  // A live legacy xlate result that the fallback would publish if the matrix cell were dropped.
  mkdirSync(join(root, "results", "xlate"), { recursive: true });
  writeFileSync(join(root, "results", "xlate", "sgw.json"), JSON.stringify({
    xlate_served: true, build: "ok", measured_at: new Date(Date.now() - 7200000).toISOString(),
    xlate_added_latency_p50_us: 900, xlate_added_latency_p99_us: 1800, xlate_rps_sustained_20ms: 5000 }));
  const g = genInto(root).gateways.find((x) => x.key === "sgw");
  assert.ok(g.translation_cell, "the below-resolution cell must still produce a translation cell");
  assert.equal(g.translation_cell.source.kind, "matrix", "the matrix wins; the legacy fallback must not fire");
  assert.equal(g.translation_cell.path.ingress, "openai");
  assert.equal(g.translation_cell.path.egress, "anthropic");
  assert.equal(g.translation_cell.added_latency_p99_us.reason, "below_resolution");
  assert.equal(app.mval(g.translation_cell.added_latency_p99_us), 0);
});

test("gen-data projects streaming from the best diagonal matrix cell", () => {
  const bundle = genInto(buildStreamMemRepo());
  const g = bundle.gateways.find((x) => x.key === "sgw");
  assert.ok(g.streaming, "expected a projected g.streaming");
  assert.equal(g.streaming.source.kind, "matrix");
  assert.equal(g.streaming.path.dialect, "openai");
  // metrics are sealed envelopes
  assert.equal(app.mval(g.streaming.added_ttft_p99_us), 90);
  assert.equal(app.mval(g.streaming.streams_sustained), 1300);
  // `cpu_fps` IS RETIRED (it counted relay frames/sec without the delivery gate, so dropping frames could
  // raise the score). The frame rate a reader can act on is the one measured where every expected frame
  // arrived, which is what is asserted in its place.
  assert.equal(g.streaming.cpu_fps, undefined, "the retired metric cannot come back through the projection");
  assert.equal(app.mval(g.streaming.streams_sustained_fps), 39000);
  // the table accessor reads the same projected value
  assert.equal(app.streamCell(g, "streams_sustained", String).text, "1300");
  assert.equal(app.streamCell(g, "streams_sustained_fps", String).text, "39000");
});

test("MEDIUM-1: a NON-streaming diagonal cell does NOT project g.streaming (stream_served gate)", () => {
  // A cell that did not stream still carries a stream record ({stream_served:false, …}). The old
  // truthiness projection surfaced it as a served streamer; gen-data must now project ONLY when the
  // diagonal cell's stream_served === true, so g.streaming is ABSENT for a non-streaming cell.
  const root = buildStreamMemRepo();  // sgw streams (STREAM_CELL.stream_served === true)
  // Add a second gateway whose diagonal cell served perf but DID NOT stream.
  mkdirSync(join(root, "gateways", "nostream"), { recursive: true });
  writeFileSync(join(root, "gateways", "nostream", "definition.json"),
    JSON.stringify({ name: "nostream", display: "nostream", lang: "Go", class: "Gateway", model: "m", port: 1,
      path: "/v1/chat/completions", auth: "dummy", egress: ["openai"],
      matrix: ["100000", "000000", "000000", "000000", "000000", "000000"] }));
  const iso = new Date(Date.now() - 3600000).toISOString();
  const perf = { added_latency_p50_us: 10, added_latency_p99_us: 20,
    frontier: rawFrontier({ 1: 35000, 10: 40000, none: 44000 }),
    sweep_max_proxy: [{ conc: 256, rps: 44000, p99_us: 100, fail: 0 }],
    sweep_sustained_20ms: [{ conc: 512, rps: 40000, p99_us: 200, fail: 0 }] };
  const nonStreamCell = { served: true, perf, stream: { stream_served: false, stream_error: "buffered, no SSE frames" } };
  const matrix = { gateway: "nostream", build: "ok", matrix_version: 2, served: true, measured_at: iso,
    upstreams: { openai: { configurable: true, served: true, cells: { openai: nonStreamCell } } },
    cells: { openai: nonStreamCell } };
  writeFileSync(join(root, "results", "matrix", "nostream.json"), JSON.stringify(matrix));
  const bundle = genInto(root);
  const streamer = bundle.gateways.find((x) => x.key === "sgw");
  const quiet = bundle.gateways.find((x) => x.key === "nostream");
  assert.ok(streamer.streaming, "a streaming cell still projects g.streaming");
  assert.ok(!quiet.streaming, "a non-streaming cell must NOT project g.streaming (stream_served gate)");
  // the table accessor renders n/a, not a fabricated streaming number
  assert.equal(app.streamCell(quiet, "streams_sustained", String).na, true);
});

// THE STREAMING SUPPRESSION IS GONE, AND THESE TWO HOLD IT GONE.
//
// This pair used to assert the inverse: a cpu_fps or streams_sustained whose engine `*_mock_bound` flag
// read `true` (our own rig set the limit) or `null` (no usable reference) was replaced with {value:null}
// at seal time, so every surface read n/a. That deleted correct measurements, and it deleted them from
// the gateways doing best: the mock paces its stream deltas, so its frames/sec is a TARGET rate, and a
// relay forwarding every frame as it arrives lands within a percent of it - the best possible outcome,
// suppressed for looking like a ceiling. 24 of 69 cells on the 2026-07-28 board published nothing.
//
// The number is now always published, and the two facts the retired verdict was derived from ride with
// it: `headroom` (the fraction of the ceiling reached) and `rig_ceiling` (the ceiling it is a fraction
// of - for the stream metrics DERIVED from the mock's declared pacing). So the property under test is
// the opposite one, and it is stronger: no near-ceiling reading and no missing reference can cost the
// value, and where the engine supplied the facts they travel with it.
test("streaming honesty: a near-ceiling cpu_fps is PUBLISHED with its headroom, never suppressed", () => {
  const certified = { key: "cert", display: "Cert", lang: "Rust",
    streaming: streamRec({ cpu_fps: 48000, cpu_fps_headroom: 0.62, cpu_fps_mock_ceiling: 77419 }) };
  assert.equal(app.streamCell(certified, "cpu_fps", String).text, "48000");
  assert.equal(app.mval(app.canonicalStreaming(certified).cpu_fps), 48000);
  // 0.993 of the mock's own paced ceiling - the reading the retired flag suppressed hardest, because
  // keeping pace with a paced upstream is exactly what a relay doing its job looks like.
  const atCeiling = { key: "bound", display: "B", lang: "Rust",
    streaming: streamRec({ cpu_fps: 51649, cpu_fps_headroom: 0.993, cpu_fps_mock_ceiling: 52013 }) };
  assert.equal(app.streamCell(atCeiling, "cpu_fps", String).na, false, "a near-ceiling cpu_fps is NOT n/a");
  assert.equal(app.streamCell(atCeiling, "cpu_fps", String).text, "51649", "the table shows the frames/sec that were measured");
  const env = app.canonicalStreaming(atCeiling).cpu_fps;
  assert.equal(app.mval(env), 51649, "the number is IN the envelope, where every surface reads it");
  assert.equal(env.headroom, 0.993, "the fraction of the rig's own ceiling reached rides with the number");
  assert.equal(env.rig_ceiling, 52013, "and the ceiling it is a fraction of, so the fraction is checkable");
  // NO USABLE REFERENCE (the retired `null` flag - the MEDIUM-5 leak class) costs the FACTS, not the value.
  const noRef = { key: "nf", display: "NF", lang: "Rust",
    streaming: streamRec({ cpu_fps: 88888, cpu_fps_headroom: null, cpu_fps_mock_ceiling: null }) };
  assert.equal(app.streamCell(noRef, "cpu_fps", String).text, "88888", "an unreferenced cpu_fps still publishes");
  assert.equal(app.canonicalStreaming(noRef).cpu_fps.headroom, undefined, "claiming no fraction it cannot state");
  // the sibling streaming metrics are unaffected either way
  assert.equal(app.streamCell(atCeiling, "streams_sustained", String).text, "1300");
});

test("streaming honesty: a near-ceiling streams_sustained is PUBLISHED with its headroom, never suppressed", () => {
  const certified = { key: "sc", display: "SC", lang: "Rust",
    streaming: streamRec({ streams_sustained: 1300, streams_sustained_headroom: 0.41, streams_sustained_mock_ceiling: 3170 }) };
  assert.equal(app.streamCell(certified, "streams_sustained", String).text, "1300");
  const atCeiling = { key: "sb", display: "SB", lang: "Rust",
    streaming: streamRec({ streams_sustained: 9999, streams_sustained_headroom: 0.998, streams_sustained_mock_ceiling: 10019 }) };
  assert.equal(app.streamCell(atCeiling, "streams_sustained", String).text, "9999", "a near-ceiling stream count publishes");
  const env = app.canonicalStreaming(atCeiling).streams_sustained;
  assert.equal(env.headroom, 0.998, "with the fraction of the paced ceiling it reached");
  assert.equal(env.rig_ceiling, 10019);
  // streams_sustained_fps carries no comparison of its own: it is the rate out of the SAME bisect, so it
  // inherits the count's facts (gen-data seals it that way, and the oracle looks them up the same way).
  const fps = app.canonicalStreaming(atCeiling).streams_sustained_fps;
  assert.equal(fps.headroom, 0.998, "the rate inherits the count's comparison, never an invented one");
  const noRef = { key: "sn", display: "SN", lang: "Rust",
    streaming: streamRec({ streams_sustained: 8888, streams_sustained_headroom: null, streams_sustained_mock_ceiling: null }) };
  assert.equal(app.streamCell(noRef, "streams_sustained", String).text, "8888", "no usable reference still publishes the count");
  // cpu_fps is measured by its own suite and carries its own facts, independent of the sustained lane
  assert.equal(app.streamCell(atCeiling, "cpu_fps", String).text, "48000");
});

/* THE TRANSLATION LANE READS ITS OWN FRONTIER, AT THE SAME BOUND AS THE PASSTHROUGH LANE.
   This used to assert that a translation `rps_sustained_20ms` near the rig's ceiling was PUBLISHED rather
   than suppressed, on both the Translation surface (xlateCell, reading the pinned matrix cell) and the
   drawer (canonicalXlate). The metric is gone; the property that matters survives and is the one asserted
   here, because it is what makes the translation cost readable at all: the translation cell carries its OWN
   frontier off its own sweep, and the drawer row for it is labelled with - and moves with - the SAME bound
   the passthrough lane is showing. Comparing a translated rate read at one bound against a passthrough rate
   read at another would be a percentage between two different questions.
   The `headroom`/`rig_ceiling` half moved with the metric: a frontier reading is a maximum over qualifying
   rungs, not a comparison against a rig reference, so there is no fraction to carry. SITE-07 asserts that
   pair where it still exists. Default state pins openai→anthropic. */
test("translation honesty: the translation cell's own frontier is read at the SAME bound as passthrough", () => {
  const g = { key: "xg", display: "XG", lang: "Rust",
    best_cell: bcCell({ dialect: "openai", frontier: { 1: 9000, 10: 20000, none: 21000 } }),
    translation_cell: tCell({ ingress: "openai", egress: "anthropic", frontier: { 1: 2000, 10: 5000, none: 6000 } }),
    matrix: mkMatrix({ anthropic: { openai: { served: true,
      perf: cellPerf({ frontier: { 1: 2000, 10: 5000, none: 6000 }, added_latency_p99_us: 200 }) } } }) };
  const xlateLane = app.LANES.find((l) => l.key === "xlate");
  const row = xlateLane.metrics.find((m) => m.k === "frontier.selected");
  const rec = app.canonicalXlate(g);
  // At the board's default bound the label names it and the row reads that bound's reading.
  assert.equal(typeof row.label, "function", "the label is rendered from the selected bound, not fixed");
  assert.equal(row.label(), app.boundColLabel(app.DEFAULT_BOUND_MS));
  assert.equal(row.cell(rec).v, 5000, "the translation row reads the selected bound");
  assert.equal(app.frontierCell(g.best_cell, app.DEFAULT_BOUND_MS).v, 20000, "and the passthrough lane the same bound");
  // MOVE THE BOUND: both lanes move together, so the ratio between them is always one question.
  const prev = app.state.bound;
  try {
    app.state.bound = 1;
    assert.equal(row.label(), app.boundColLabel(1));
    assert.equal(row.cell(rec).v, 2000, "the translation row follows the selector");
    assert.equal(app.frontierCell(g.best_cell, app.selectedBound(app.state)).v, 9000);
  } finally { app.state.bound = prev; }
  // The pinned-cell surface (the Translation table's own accessor) reads the SAME cell's frontier.
  assert.equal(app.frontierCell(app.canonicalXlate(g), app.DEFAULT_BOUND_MS).v, 5000);
  // A record with no frontier at all publishes NO throughput here either - never a zero.
  const none = { key: "xz", display: "XZ", lang: "Rust",
    translation_cell: tCell({ ingress: "openai", egress: "anthropic", frontier: null }) };
  const cell = row.cell(app.canonicalXlate(none));
  assert.equal(cell.na, true);
  assert.equal(cell.v, null, "no frontier is not a throughput of zero");
});

test("streaming: a null added-TTFT/gap reads n/a on the table (the envelope carries the absence)", () => {
  // An unreliable streaming c1 window sets added_ttft/gap to null while stream_served stays true. Under the
  // sealed envelope that null is a {value:null, reason:"not_measured"} envelope; streamCell reads n/a. A
  // measured value reads the number. There is no "site-visible vs chart draws-bar" gate to tie any more -
  // the envelope IS the single decision (the retired drift check cannot arise: one datum, one value).
  const okStream = streamRec({ added_ttft_p99_us: 90, added_gap_p99_us: 12,
    streams_sustained: 1300, cpu_fps: 48000 });
  const okGw = { key: "tok", display: "Tok", lang: "Rust", streaming: okStream };
  assert.equal(app.streamCell(okGw, "added_ttft_p99_us", String).text, "90", "measured added-TTFT shows the number");
  assert.deepEqual(checkConsistency({ gateways: [okGw] }, app, SYNTH).errors, [], "a sealed streaming record is consistent");
  const nullStream = streamRec({ added_ttft_p99_us: null, added_gap_p99_us: null,
    streams_sustained: 1300, cpu_fps: 48000 });
  const nullGw = { key: "tnull", display: "Tnull", lang: "Rust", streaming: nullStream };
  assert.equal(app.streamCell(nullGw, "added_ttft_p99_us", String).na, true, "null added-TTFT reads n/a on the table");
  assert.equal(app.streamCell(nullGw, "added_gap_p99_us", String).na, true, "null added-gap reads n/a on the table");
  assert.deepEqual(checkConsistency({ gateways: [nullGw] }, app, SYNTH).errors, [], "a null-TTFT sealed streaming record is consistent");
});

test("gen-data emits memory PER CELL and projects no per-gateway memory scalar", () => {
  // Memory is per-cell, not a per-gateway scalar the harness has to select: the window lives on the
  // cell, and the reader chooses the cell (Min|Max|Same|Custom) and can see which.
  const bundle = genInto(buildStreamMemRepo());
  const g = bundle.gateways.find((x) => x.key === "sgw");
  assert.equal(g.memory_read, undefined, "NO per-gateway memory scalar may be projected");
  const cell = g.matrix.upstreams.openai.cells.openai;
  assert.ok(cell.memory, "the served cell carries its own memory window");
  assert.equal(app.mval(cell.memory.idle_rss_mib), 120.5);
  assert.equal(app.mval(cell.memory.steady_state_rss_mib), 890.2);
  assert.equal(app.mval(cell.memory.recovered_rss_mib), 130.0);
  // The growth rate is a SEALED metric, not a bare scalar: it is published whether or not the cell
  // plateaued (any threshold admits a leak slower than itself), so it must be an envelope like the rest.
  assert.ok(app.isEnvelope(cell.memory.growth_rate_mib_per_min), "growth_rate_mib_per_min is sealed");
  assert.ok(app.isEnvelope(cell.memory.time_to_plateau_s), "time_to_plateau_s is sealed");
  assert.equal(cell.memory.plateaued, true, "the plateau VERDICT is a raw bool, not a metric envelope");
  assert.ok(Array.isArray(cell.memory.rss_series) && cell.memory.rss_series.length === 3,
    "the rss_series travels verbatim on the cell");
  // The board reads that cell: Same mode on the widest dialect lands on it and reports the steady state.
  const st = { data: bundle, mode: "same", sameDialect: "openai", view: "memory" };
  assert.equal(app.hasPerCellMemory(bundle), true, "the bundle is a per-cell memory bundle");
  assert.equal(app.memCell(g, "steady_state_rss_mib", String, st).text, "890.2");
  // The memory lane still ages: with no projected record to carry a source stamp, it ages by the matrix
  // that produced the windows.
  assert.equal(g.lane_measured_at.memory, g.matrix.measured_at, "the memory lane ages by its matrix");
});

test("memory recovery column: present shows the value, absent renders muted n/a (never a fabricated 0)", () => {
  // A gateway WITH the recovery field shows it.
  const withRec = { key: "wr", display: "WR", lang: "Rust",
    memory_read: memRec({ idle_rss_mib: 40, peak_rss_mib: 1000, recovered_rss_mib: 45 }) };
  const cell = app.memCell(withRec, "recovered_rss_mib", String);
  assert.equal(cell.na, false, "a measured recovered_rss_mib must not read n/a");
  assert.equal(cell.text, "45");
  // A gateway WITHOUT the field (pre-recovery bundle) reads n/a - never 0, never fabricated.
  const noRec = { key: "nr", display: "NR", lang: "Rust",
    memory_read: memRec({ idle_rss_mib: 40, peak_rss_mib: 1000, recovered_rss_mib: null }) };
  const naCell = app.memCell(noRec, "recovered_rss_mib", String);
  assert.equal(naCell.na, true, "an absent recovered_rss_mib must render n/a");
  assert.equal(naCell.text, "not measured");
  assert.equal(naCell.v, null, "an absent recovered_rss_mib carries a null value, never 0");
  // The Memory tab carries the Recovered column, gated best = min (lower recovery releases more).
  const col = app.COLUMN_SETS.memory.find((c) => c.id === "memrecov");
  assert.ok(col, "the Recovered @60s column exists on the Memory tab");
  assert.ok(/release memory/.test(col.title), "the column tooltip explains the recovery signal");
  const rec = app.LANES.find((l) => l.key === "memory").metrics.find((m) => m.k === "recovered_rss_mib");
  assert.ok(rec && rec.best === "min", "the memory lane ranks recovered_rss_mib best = min");
});

test("recovery sparkline: renders only when rss_series exists (≥2 points), never fabricated", () => {
  // With a series → an inline SVG recovery curve.
  const svg = app.rssSparkline([ { t_s: 0, rss_mib: 40 }, { t_s: 60, rss_mib: 1000 }, { t_s: 180, rss_mib: 45 } ]);
  assert.ok(/<svg/.test(svg) && /<path /.test(svg), "a series yields an inline-SVG path");
  // The caption reports the final figure and names WHICH point it is. It no longer says "recovered" for a
  // bare call: that word belongs to the Recovered @N s column, which reads a different point of the same
  // falling curve, and two figures under one word read as an inconsistency (one-api: 139.1 vs 129.6).
  assert.ok(/45\.0 MiB at the last sample/.test(svg), `the caption reports the final figure and names the point: ${svg}`);
  // Given the column's own scalar and window, it names that window too, so the two are comparable on sight.
  const withMark = app.rssSparkline([ { t_s: 0, rss_mib: 40 }, { t_s: 60, rss_mib: 1000 }, { t_s: 180, rss_mib: 45 } ], null, 40, "load", { recoveredAt: 60, recoveryWindowS: 30 });
  assert.match(withMark, /60\.0 MiB at the 30 s recovery mark, still falling to 45\.0 MiB by the last sample/);
  // No series, one point, or a non-array → nothing drawn (never a fabricated flat line).
  assert.equal(app.rssSparkline(undefined), "", "no series → no sparkline");
  assert.equal(app.rssSparkline([]), "", "empty series → no sparkline");
  assert.equal(app.rssSparkline([ { t_s: 0, rss_mib: 40 } ]), "", "a single point → no sparkline");
});

// Under the sealed envelope, streaming is ONE projected record whose throughput metrics
// (streams_sustained, cpu_fps) are sealed at projection time; every surface reads them through the
// envelope, so there is no headline-vs-cell "projection drift" to guard - one record, one value.
//
// The second half used to prove that a `cpu_fps_mock_bound: true` reading was {value:null} and "cannot
// leak". What it actually proved was that the reading was deleted: 99,999 frames/sec measured, nothing
// published, because our own rig might have been the limiter. It now publishes with its headroom, and
// what this half holds is that the envelope stays STRUCTURALLY clean while carrying those facts - C1
// accepts `headroom`/`rig_ceiling` as envelope fields, and C2's "no suppression anywhere" still holds,
// which is the guard that the retired shape has not come back.
test("streaming: a sealed streaming record publishes its throughput metrics, facts and all", () => {
  const certified = { key: "sg", display: "SG", lang: "Rust",
    streaming: streamRec({ streams_sustained: 1300, cpu_fps: 48000 }) };
  assert.equal(app.streamCell(certified, "streams_sustained", app.fmtInt).text, "1,300");
  assert.equal(app.streamCell(certified, "cpu_fps", app.fmtInt).text, "48,000");
  assert.deepEqual(checkConsistency({ gateways: [certified] }, app, SYNTH).errors, [], "a certified sealed streaming record is consistent");
  // A cpu_fps that came within 0.4% of the mock's paced ceiling: published, annotated, and consistent.
  const atCeiling = { key: "bs", display: "BS", lang: "Rust",
    streaming: streamRec({ streams_sustained: 1300, cpu_fps: 99999, cpu_fps_headroom: 0.996, cpu_fps_mock_ceiling: 100400 }) };
  assert.equal(app.streamCell(atCeiling, "cpu_fps", app.fmtInt).text, "99,999", "the near-ceiling reading IS the cell");
  assert.equal(app.streamCell(atCeiling, "streams_sustained", app.fmtInt).text, "1,300", "the sibling is unaffected");
  assert.equal(atCeiling.streaming.cpu_fps.headroom, 0.996, "the reader is given the fraction, not a verdict");
  assert.deepEqual(checkConsistency({ gateways: [atCeiling] }, app, SYNTH).errors, [],
    "an annotated record is structurally clean: C1 accepts the facts, C2 finds no suppression to reject");
});

test("download: gatewayResultsJson is the gateway's complete record as parseable JSON", () => {
  const g = { key: "dgw", display: "DGW", lang: "Rust",
    matrix: { upstreams: { openai: { cells: { openai: { served: true } } } }, memory: { served: true, idle_rss_mib: 100 } },
    ootb_config: "port: 8080\n", best_cell: { dialect: "openai", rps_max_proxy: 50000 } };
  const json = app.gatewayResultsJson(g);
  const round = JSON.parse(json);   // must be valid JSON
  assert.equal(round.key, "dgw");
  assert.ok(round.matrix && round.matrix.upstreams, "the download carries the full matrix (6x6 cells)");
  assert.ok(round.matrix.memory, "the download carries the memory read");
  assert.equal(round.ootb_config, "port: 8080\n", "the download carries the OOTB config");
  // the download filename convention is <gateway>-results.json (asserted at the call site by using g.key)
  assert.equal(`${g.key}-results.json`, "dgw-results.json");
});

test("Performance Custom (openai->anthropic) has no silent all-n/a served row", () => {
  // In Custom mode on a pinned pair, any gateway that SERVES that cell should have per-cell perf,
  // or its row is all n/a. A gateway whose per-cell sweep never ran AT ALL (mid re-run) is
  // known-pending: app.js reads that HONESTLY as "served, not measured on this cell" (see the
  // perfBlock fallback in cellPopFull) rather than fabricating a value; anything else all-n/a is a bug.
  const st = { ...app.newState(), view: "performance", mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  const KEYS = ["added_latency_p50_us", "added_latency_p99_us", "rps_sustained_20ms", "rps_max_proxy"];
  let checked = 0;
  for (const g of data.gateways) {
    if (!(g.matrix && app.chooserHasCell(g, st))) continue;   // only rows that serve the pinned cell
    checked++;
    const sweptAny = Object.values(g.matrix.upstreams || {}).some((u) =>
      Object.values(u.cells || {}).some((c) => c && c.served === true && c.perf));
    if (!sweptAny) {
      const anyVal = KEYS.some((k) => app.chooserPerfCell(g, k, String, st).v != null);
      assert.ok(!anyVal, `${g.key}: unswept cell must read n/a on every metric, not a fabricated value`);
      continue;
    }
    const anyVal = KEYS.some((k) => app.chooserPerfCell(g, k, String, st).v != null);
    assert.ok(anyVal, `${g.key} serves the pinned cell but every metric is n/a`);
  }
  // Whether the CURRENT live bundle happens to have a gateway serving openai->anthropic is a fact
  // about the field, not something this test can guarantee - so a live-bundle miss only logs. The
  // guard logic itself (the two branches above) is guaranteed to run at least once by the synthetic,
  // fixture-driven test right after this one, so the assertions are never silently skipped entirely.
  if (checked === 0) console.log("  (note: no gateway in the live bundle currently serves openai->anthropic)");
});

// The live-bundle test above is best-effort (it depends on what the field has actually measured); this
// is the deterministic guarantee that the SAME guard logic (served + no-perf -> warning, served +
// perf -> a real value) actually executes, regardless of what is or is not currently committed.
test("Performance Custom (openai->anthropic): the served/swept-vs-unswept guard actually fires", () => {
  const st = { ...app.newState(), view: "performance", mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  const KEYS = ["added_latency_p50_us", "added_latency_p99_us", "rps_sustained_20ms", "rps_max_proxy"];
  // (a) served AND swept: a real per-cell value must be readable, not n/a.
  {
    const root = buildStreamMemRepo();
    const mpath = join(root, "results", "matrix", "sgw.json");
    const m = JSON.parse(readFileSync(mpath, "utf8"));
    m.upstreams.anthropic = { configurable: true, served: true, cells: { openai: {
      served: true,
      perf: { added_latency_p50_us: 12, added_latency_p99_us: 30, frontier: rawFrontier({ 10: 8000, none: 8500 }) },
    } } };
    writeFileSync(mpath, JSON.stringify(m));
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.ok(app.chooserHasCell(g, st), "the pinned cell must be recognised as served");
    const anyVal = KEYS.some((k) => app.chooserPerfCell(g, k, String, st).v != null);
    assert.ok(anyVal, "a served, swept cell must surface a real value, not n/a");
  }
  // (b) served but never swept: every metric must read n/a HONESTLY (app.js's cellPopFull falls back
  // to "served, not measured on this cell" rather than fabricating a value from a different cell).
  {
    const root = buildStreamMemRepo();
    const mpath = join(root, "results", "matrix", "sgw.json");
    const m = JSON.parse(readFileSync(mpath, "utf8"));
    m.upstreams.anthropic = { configurable: true, served: true, cells: { openai: { served: true } } };
    // No perf anywhere in the whole matrix removes the "swept" evidence entirely.
    delete m.upstreams.openai.cells.openai.perf;
    writeFileSync(mpath, JSON.stringify(m));
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.ok(app.chooserHasCell(g, st), "still served, just never measured");
    const anyVal = KEYS.some((k) => app.chooserPerfCell(g, k, String, st).v != null);
    assert.ok(!anyVal, "an unswept served cell must read n/a on every metric, never a fabricated value");
  }
});

// ---- footer timestamps: clean UTC stamp + coarse relative age ----------------
test("footer timestamps format cleanly with a coarse age", () => {
  const iso = "2026-07-22T17:52:46.101Z";
  assert.equal(app.fmtStamp(iso), "Jul 22, 2026 17:52 UTC");
  const t = Date.parse(iso);
  const H = 3600000;
  assert.equal(app.fmtAge(iso, t + 10 * 60000), "just now");            // < 1 hour
  assert.equal(app.fmtAge(iso, t + 1 * H + 1), "1 hour ago");           // hours, coarse
  assert.equal(app.fmtAge(iso, t + 47.5 * H), "47 hours ago");          // still hours at 47
  assert.equal(app.fmtAge(iso, t + 48 * H), "2 days ago");              // days from 48 hours
  assert.equal(app.fmtAge(iso, t + 10 * 24 * H + 5 * H), "10 days ago"); // whole days only
  assert.equal(app.stampWithAge(iso, t + 3 * H), "Jul 22, 2026 17:52 UTC (3 hours ago)");
  // garbage in: fall back to the raw string, no age
  assert.equal(app.fmtStamp("not-a-date"), "not-a-date");
  assert.equal(app.fmtAge("not-a-date"), "");
});

test("measuredBadge shows a gateway's own measured_at + a stale pill only when flagged", () => {
  const iso = "2026-07-22T17:52:46.101Z";
  const t = Date.parse(iso);
  const H = 3600000;
  // Fresh, not flagged: relative age, full stamp in the title, NO stale pill.
  const fresh = app.measuredBadge({ measured_at: iso, stale: false }, t + 3 * H);
  assert.ok(/measured 3 hours ago/.test(fresh), `expected the relative age, got: ${fresh}`);
  assert.ok(/Jul 22, 2026 17:52 UTC \(3 hours ago\)/.test(fresh), "full stamp travels in the title");
  assert.ok(!/stale-pill/.test(fresh), "a fresh gateway shows no stale pill");
  // Flagged stale: the greyed pill appears.
  const stale = app.measuredBadge({ measured_at: iso, stale: true }, t + 70 * 24 * H);
  assert.ok(/class="stale-pill"/.test(stale), `a stale gateway shows the stale pill, got: ${stale}`);
  // No measurement at all → renders nothing (graceful).
  assert.equal(app.measuredBadge({ measured_at: null, stale: false }), "");
  assert.equal(app.measuredBadge(null), "");
});

// ---- compact not-served labels (compare + results cells) --------------------
test("naText keeps long diagnostic notes out of cell values", () => {
  assert.deepEqual(app.naText(null, "xlate_served", "xlate_error"), { text: "not measured", note: "" });
  for (const g of data.gateways) {
    for (const l of app.LANES) {
      const j = g[l.key];
      if (!j || j[l.flag] !== false) continue;
      const na = app.naText(j, l.flag, l.err);
      assert.ok(na.text.length <= 24, `${g.key}/${l.key}: label too long: ${na.text}`);
      assert.equal(na.note, app.stripRigPaths(j[l.err] || ""), `${g.key}/${l.key}: full note preserved (rig paths scrubbed)`);
      assert.ok(!/\/home\//.test(na.note), `${g.key}/${l.key}: tooltip leaks a rig path`);
    }
  }
  // Data-dependent: the field may or may not currently contain an untranslated-passthrough
  // gateway (it comes and goes with re-runs). Assert the label only when one exists; always
  // assert the mapping itself on a synthetic record so the rule stays covered.
  const pass = data.gateways.find((g) => g.xlate && g.xlate.xlate_passthrough === true);
  if (pass) assert.equal(app.naText(pass.xlate, "xlate_served", "xlate_error").text, "n/a (passthrough)");
  assert.equal(app.naText({ xlate_served: false, xlate_passthrough: true }, "xlate_served", "xlate_error").text, "n/a (passthrough)");
  // "manifest defines no <hook>" = the harness never probed that lane: "not tested", never a capability
  // verdict. (Governance is retired from the board, so this is asserted on a synthetic record - the
  // naText rule itself still applies to any suite whose note carries that string.)
  assert.equal(
    app.naText({ served: false, serve_error: "manifest defines no gw_governed_launch hook" }, "served", "serve_error").text,
    "not tested");
});

test("streaming latency cells annotate >=1ms values with their ms equivalent", () => {
  const cols = app.COLUMN_SETS.streaming;
  const sttft = cols.find((c) => c.id === "sttft");
  const big = { streaming: streamRec({ added_ttft_p99_us: 596693 }) };
  assert.equal(sttft.get(big).text, "596,693 (596.7 ms)");
  const small = { streaming: streamRec({ added_ttft_p99_us: 397 }) };
  assert.equal(sttft.get(small).text, "397");
});

test("stripRigPaths scrubs absolute bench-box paths from diagnostic notes", () => {
  const note = "boom at file:///home/ubuntu/.npm/_npx/abc/node_modules/x/y.js:2:434559\n" +
    "    at dispatch (/home/ubuntu/.npm/_npx/abc/node_modules/hono/dist/compose.js:22:17)";
  const out = app.stripRigPaths(note);
  assert.ok(!out.includes("/home/"), out);
  assert.ok(out.includes("<rig path>"));
  // and naText tooltips get the scrubbed note
  const na = app.naText({ stream_served: false, stream_error: note }, "stream_served", "stream_error");
  assert.ok(!na.note.includes("/home/"));
});

test("NOISE: the rig's resolution is DERIVED from box qualification, never a chosen constant", () => {
  // Every box runs the same qualification before measuring, and identical boxes still land apart.
  // That spread IS what the rig cannot resolve - a measurement, not a policy. A hard-coded 1% or 2%
  // would be an undeclared rule deciding which published comparisons count.
  const board = (drifts) => ({
    gateways: drifts.map((d, i) => ({ key: `g${i}`, rig: { box_qualify: { drift_pct: d } } })),
  });
  // The real 2026-07-30 field: 13 identical boxes spanning -6.22%..+2.05%.
  assert.ok(Math.abs(app.rigResolutionPct(board([-6.22, -1.04, 1.86, 2.05, -0.38])) - 8.27) < 1e-9);

  // ONE box has no spread to observe. Inventing a floor from a single sample is exactly the magic
  // number this exists to avoid, so it reports that it cannot say.
  assert.equal(app.rigResolutionPct(board([1.5])), null);
  assert.equal(app.rigResolutionPct({ gateways: [] }), null);

  // A gateway whose qualification is absent contributes nothing rather than counting as 0 drift -
  // a missing measurement is not a perfectly-calibrated box.
  const mixed = { gateways: [{ key: "a", rig: { box_qualify: { drift_pct: -3 } } }, { key: "b" }, { key: "c", rig: { box_qualify: { drift_pct: 2 } } }] };
  assert.equal(app.rigResolutionPct(mixed), 5);
});

test("CHARTS: the tab draws from the live board, ranks by the metric's own direction, and drops nobody silently", () => {
  // The registry is what replaced 25 PNGs. Every entry must be drawable: a label, a direction, a
  // getter. A metric that cannot say which way is better cannot be ranked.
  assert.ok(app.CHART_METRICS.length >= 4);
  for (const m of app.CHART_METRICS) {
    assert.ok(m.id && m.label && typeof m.get === "function", `chart metric ${m.id} is not drawable`);
    assert.equal(typeof m.desc, "boolean", `${m.id} must declare which direction is better`);
  }

  // COST IS ON A LOG AXIS AND THAT IS NOT A PREFERENCE. The real board spans 89us to 199,333us -
  // 2,247x - and on a linear axis twelve of fourteen gateways are a single pixel beside the slowest.
  const cpu = app.CHART_METRICS.find((m) => m.id === "cpu");
  assert.equal(cpu.log, true, "cost per request must be logarithmic");
  assert.equal(cpu.desc, false, "less CPU per request is better");

  // Ranking uses the metric's direction, not a fixed order.
  const mk = (key, v) => ({ key, name: key, lang: "Rust", best_cell: { ...bcCell({}), cpu_us_per_request: seal(v) } });
  const st = { data: null, mode: "peak", bound: 10 };
  const rows = app.chartRows(cpu, [mk("slow", 199333), mk("fast", 89), mk("mid", 273)], st);
  assert.deepEqual(rows.map((r) => r.key), ["fast", "mid", "slow"], "lower-is-better sorts ascending");

  // A gateway with no value is EXCLUDED FROM THE BARS but is not lost - renderCharts names it under
  // the chart. A chart that silently omits rows reports a tidier field than the one measured, and on
  // this board an absent number usually means a refusal a reader needs to see.
  const withHole = app.chartRows(cpu, [mk("has", 100), { key: "none", name: "none", lang: "Go", best_cell: bcCell({}) }], st);
  assert.deepEqual(withHole.map((r) => r.key), ["has"]);
});

test("SATURATION: a utilisation figure cannot be read as a verdict unless the window reached the peak", () => {
  // The real numbers from 2026-07-31. Same utilisation SHAPE, opposite meaning, and only the ratio
  // to the cell's own peak separates them - which is the mistake I made in prose before catching it.
  const cell = (util, wrps, peak) => ({
    cost_core_utilisation: seal(util),
    cost_window_rps: seal(wrps),
    frontier: [{ p99_bound_us: null, rps: seal(peak), concurrency: seal(8), p99_us: seal(1000), lower_bound: false }],
  });

  // tensorzero: 2.1% of cores, but the window carried 200 rps against a 13,303 peak - 2% of it.
  // Idle cores there say NOTHING about saturation at peak.
  const tz = app.costSaturation(cell(0.021, 200, 13303));
  assert.equal(tz.verdict, null, "too far below peak to be a verdict");
  assert.match(tz.why, /too far below it/);

  // one-api: the SAME 2-3% utilisation, but at 95% of its peak - so it genuinely is not CPU-bound.
  const oa = app.costSaturation(cell(0.026, 35, 37));
  assert.equal(oa.verdict, "headroom");
  assert.match(oa.why, /something other than CPU/);

  // litellm-rust: 96% of cores at 94% of its peak - the peak IS its own wall.
  const lr = app.costSaturation(cell(0.961, 43527, 46187));
  assert.equal(lr.verdict, "cpu-bound");
  assert.match(lr.why, /own CPU wall/);

  // Absent inputs yield no claim at all, rather than a claim built on a missing number.
  assert.equal(app.costSaturation({}), null);
  assert.equal(app.costSaturation(cell(0.5, 100, 0)), null);
});

test("NOISE: the table marks the boundary where its own ranking stops meaning anything", () => {
  // The 2026-07-30 fleet resolved 8.27%. These four rows are a ranking at the top and a coin toss in
  // the middle, and the reader cannot tell which without being told.
  const col = { id: "rps", get: (g) => ({ v: g.v }) };
  const rows = [{ key: "a", v: 48394 }, { key: "b", v: 46031 }, { key: "c", v: 25101 }, { key: "d", v: 24500 }];
  const tied = app.tiedRuns(rows, col, {}, 8.27);
  assert.ok(tied.has("b"), "46,031 is 4.9% from 48,394 - inside the rig's resolution, so not a ranking");
  assert.ok(!tied.has("c"), "25,101 is far below 46,031 - a real finding, must NOT be marked");
  assert.ok(tied.has("d"), "24,500 is 2.4% from 25,101 - also inside it");
  assert.ok(!tied.has("a"), "the first row has nothing above it to tie with");

  // NOTHING is marked when the resolution is unknown. Asserting a tie needs a figure, and a board
  // with one box has no spread to derive one from - so it must not claim ties either way.
  assert.equal(app.tiedRuns(rows, col, {}, null).size, 0);

  // A column that renders its own cell is not a ranking this can reason about.
  assert.equal(app.tiedRuns(rows, { id: "x", render: () => "<td/>" }, {}, 8.27).size, 0);
});

test("NOISE: two values closer than the rig can resolve are NOT a ranking", () => {
  // busbar 46,031 vs litellm-rust 48,394 is 4.9% apart, and the 2026-07-30 fleet could only resolve
  // 8.27%. Presenting that as a ranking claims a difference the rig never demonstrated.
  assert.equal(app.indistinguishable(46031, 48394, 8.27), true);
  // A gap wider than the rig's own spread IS a finding.
  assert.equal(app.indistinguishable(25101, 48394, 8.27), false);
  // RELATIVE TO THE LARGER VALUE, so the rule means the same thing at the bottom of the board as at
  // the top. 19 vs 20 is 5% apart and 49,000 vs 50,000 is 2% - both inside a rig that resolves 8.27%,
  // and both correctly tied. The absolute gaps (1 and 1,000) differ by three orders of magnitude,
  // which is exactly why an absolute threshold could not serve this board.
  assert.equal(app.indistinguishable(19, 20, 8.27), true);
  assert.equal(app.indistinguishable(49000, 50000, 8.27), true);
  // And a small-value pair that IS resolvable stays resolvable: 19 vs 25 is 24% apart.
  assert.equal(app.indistinguishable(19, 25, 8.27), false);
  // Two MEASURED zeros are the same measurement, not an undefined ratio.
  assert.equal(app.indistinguishable(0, 0, 8.27), true);
  // With no resolution known (a single box), nothing may be declared indistinguishable: that would
  // be asserting a tie on the strength of a figure we just said we could not derive.
  assert.equal(app.indistinguishable(100, 101, null), false);
});

test("COST: the cost columns appear only on a board that can answer them, and read through the envelope", () => {
  // ADDITIVE, NOT A REPLACEMENT. The existing columns are untouched; these follow the per-cell-memory
  // precedent and appear only when the board carries the field.
  //
  // WHY GATE AT ALL, when every other absence renders per row: a row missing ONE metric still has the
  // rest, so "not measured" there is disclosure. A cost column on a board measured before the capture
  // existed is n/a on EVERY row - a column asking a question nothing on the page can answer, which is
  // noise. It lights up by itself on the first board carrying the field.
  const noCost = { gateways: [{ key: "a", best_cell: bcCell({}) }] };
  assert.equal(app.hasCost(noCost), false);
  const ids = app.columnsFor("performance", noCost).map((c) => c.id);
  assert.ok(!ids.includes("cpu"), `no cost column on a board without cost: ${ids.join(",")}`);
  assert.ok(ids.includes("rps"), "the throughput column is untouched");

  // A board that DOES carry cost shows them, and reads the value through the sealed envelope.
  const withCost = {
    gateways: [{
      key: "a",
      best_cell: { ...bcCell({}), cpu_us_per_request: seal(37.5), rps_per_cpu_second: seal(26666) },
    }],
  };
  assert.equal(app.hasCost(withCost), true);
  const cols = app.columnsFor("performance", withCost);
  const cpu = cols.find((c) => c.id === "cpu");
  assert.ok(cpu, "the cost column appears once the board can answer it");
  assert.equal(cpu.desc, false, "less CPU per request is better, so it sorts ascending");
  const st = { data: withCost, mode: "peak", bound: 10 };
  assert.equal(cpu.get(withCost.gateways[0], st).v, 37.5);
  // AND ITS RECIPROCAL IS NOT BESIDE IT. 1 CPU-second is a million microseconds, so
  // rps_per_cpu_second is 1,000,000 / cpu_us_per_request - the same measurement inverted. Two columns
  // that multiply to a constant read as corroboration while carrying one number between them. It
  // lives on the Charts tab instead, where it asks a different question.
  assert.equal(cols.find((c) => c.id === "rpscpu"), undefined,
    "requests-per-CPU-second must not sit beside CPU-per-request: they are one number, inverted");
});

test("COST: an absent cost renders 'not measured', never a 0 that would look infinitely efficient", () => {
  // A gateway measured before the capture existed carries a null envelope. Rendering that as 0 would
  // make the LEAST-measured gateway look like the cheapest one on the board.
  const board = {
    gateways: [
      { key: "measured", best_cell: { ...bcCell({}), cpu_us_per_request: seal(40) } },
      { key: "not-yet", best_cell: { ...bcCell({}), cpu_us_per_request: seal(null, { reason: "not_measured" }) } },
    ],
  };
  const cpu = app.columnsFor("performance", board).find((c) => c.id === "cpu");
  const st = { data: board, mode: "peak", bound: 10 };
  const absent = cpu.get(board.gateways[1], st);
  assert.equal(absent.v, null, "an unmeasured cost has no value");
  assert.ok(!/^0/.test(absent.text), `must not render as a zero: ${absent.text}`);
});

test("FINDING 39: a sub-1/s rate survives every axis tick and every hover sentence", () => {
  // THE RATE HAS NOW LEAKED AT SIXTEEN BOUNDARIES ACROSS FIVE AUDIT ROUNDS, always the same shape:
  // one renderer is taught the fractional rate and a sibling reading the SAME number is missed. These
  // pin the two that round 5 found, so the next miss is a red test rather than a published "0".

  // fmtTick is the y-axis label formatter for the RPS sweep chart. niceStep correctly produces a
  // sub-1 step for a sub-1 domain, so the gridlines were being drawn - and then every one of them
  // was labelled "0", an axis whose entire scale read zero for a gateway that was measurably serving.
  assert.equal(app.fmtTick(0.25), "0.25");
  assert.equal(app.fmtTick(0.1), "0.1");
  assert.equal(app.fmtTick(0.04), "0.04");
  // 0 is still "0" - it is a MEASURED zero, and the whole point is that it stays distinguishable
  // from a truncated fraction rather than both rendering alike.
  assert.equal(app.fmtTick(0), "0");
  // and nothing about the integer domain moved: these are what the published charts already show.
  assert.equal(app.fmtTick(1), "1");
  assert.equal(app.fmtTick(44382), "44.4k");

  // cellPerfTip composes a "req/s" SENTENCE, and it was the last literal fmtInt() on a rate in app.js.
  // Its integer form asserted the gateway carried nothing, from a reading that says otherwise - while
  // frontierCell, the live equivalent reading the same envelope, printed "0.25".
  const best = bcCell({ ingress: "openai", egress: "openai", frontier: 30000 });
  const slow = { served: true, perf: cellPerf({ frontier: 0.25 }) };
  const tip = app.cellPerfTip(slow, "anthropic", "openai", best, 10);
  assert.ok(tip.includes("0.25 req/s"), tip);
  assert.ok(!/\b0 req\/s/.test(tip), `a measured 0.25 must never print as "0 req/s": ${tip}`);
});

// ---- per-cell perf: best-path deviation on the matrix hover -----------------
test("cellPerfTip shows a green cell's perf and its deviation from the gateway's best cell", () => {
  // cellPerfTip reads the sealed envelopes via mval(): a certified cell + reference show the number + delta;
  // an envelope with no value cannot become a number on hover (asserted in the next test).
  // THE RATE IS NAMED WITH ITS BOUND - "while 99% of requests finished under 10 ms" - and both sides of the
  // delta are read at the SAME bound, or the percentage would be between two different questions.
  const best = bcCell({ ingress: "openai", egress: "openai", frontier: 30000 });
  const green = { served: true, perf: cellPerf({ frontier: 25500, added_latency_p99_us: 900 }) };
  const tip = app.cellPerfTip(green, "anthropic", "openai", best, 10);
  assert.ok(tip.includes("25,500 req/s while 99% of requests finished under 10 ms"), tip);
  assert.ok(tip.includes("+900 µs p99 added"), tip);
  assert.ok(tip.includes("-15.0% req/s vs the OpenAI→OpenAI cell"), tip); // human labels, not raw dialect keys
  const bestTip = app.cellPerfTip({ served: true, perf: cellPerf({ frontier: 30000 }) }, "openai", "openai", best, 10);
  assert.ok(bestTip.includes("reference cell"), bestTip);
  // A FLOOR SAYS SO ON THE HOVER TOO: the sweep ran out of ladder with that concurrency still qualifying,
  // so the rate is real and is not a maximum. Rendering it as a bare number would state a ceiling.
  const floorTip = app.cellPerfTip(
    { served: true, perf: cellPerf({ frontier: 25500, frontierOpts: { lowerBound: true }, added_latency_p99_us: 900 }) },
    "anthropic", "openai", best, 10);
  assert.ok(floorTip.includes("≥ 25,500 req/s"), floorTip);
  // red/grey/unprobed cells and perf-less greens carry NO perf line
  assert.equal(app.cellPerfTip({ served: false, perf: cellPerf({ frontier: 1 }) }, "a", "b", best, 10), "");
  assert.equal(app.cellPerfTip({ served: "not_configurable" }, "a", "b", best, 10), "");
  assert.equal(app.cellPerfTip({ served: true }, "a", "b", best, 10), "");
  // A cell with NO frontier at all has nothing to say about throughput and says nothing.
  assert.equal(app.cellPerfTip({ served: true, perf: cellPerf({ frontier: null }) }, "a", "b", best, 10), "");
});

// FINDING 33 was that the matrix hover tip read the RAW cell scalar, so a number no other surface would
// show still appeared on hover. The fix was structural - the tip reads through mval(), so it can only
// ever render what the envelope carries - and that is what survives here.
//
// What does NOT survive is the state the test used to demonstrate it with: a `mock_bound: true` cell
// whose 99,999 req/s had been replaced with {value:null}, so "the number is gone from the data" was the
// assertion. The number is no longer gone, because deleting a correct measurement is not a way to
// qualify it. So the tip's two live properties are pinned against the state that DOES still produce an
// empty envelope - a metric the harness never measured on that cell:
//   - an absent RPS cannot become a number on hover, and the certified added-latency beside it survives;
//   - a delta needs BOTH sides, so an absent reference yields the number and no percentage.
// And the inverted half: a near-ceiling RPS now renders in the tip like any other measurement.
test("FINDING 33: cellPerfTip renders only what the envelope carries - an absent RPS cannot become a number", () => {
  const best = bcCell({ ingress: "openai", egress: "openai", frontier: 30000 });
  // A cell whose reading at this bound has no rate: {value:null} with the engine's reason. The
  // added-latency survives, and the hover says which bound has no reading rather than implying a rate.
  const unmeasured = { served: true, perf: cellPerf({ frontier: { 10: null }, added_latency_p99_us: 900,
    frontierOpts: { absent: { reason: "not_measured", detail: "no reading" } } }) };
  const tip = app.cellPerfTip(unmeasured, "anthropic", "openai", best, 10);
  assert.ok(!tip.includes("req/s"), `an absent reading must not produce a rate on hover; got: ${tip}`);
  assert.ok(tip.includes("no reading at 10 ms"), tip);
  assert.ok(tip.includes("+900 µs p99 added"), tip);
  // A certified cell against an ABSENT reference: the number shows, but no delta - the divisor is null.
  const noRef = bcCell({ ingress: "openai", egress: "openai", frontier: { 10: null },
    frontierOpts: { absent: { reason: "not_measured", detail: "no reading" } } });
  const t = app.cellPerfTip({ served: true, perf: cellPerf({ frontier: 25500, added_latency_p99_us: 900 }) }, "anthropic", "openai", noRef, 10);
  assert.ok(t.includes("25,500 req/s while 99% of requests finished under 10 ms"), t);
  assert.ok(!t.includes("vs the"), `no delta against a reference with no number; got: ${t}`);
  /* AND THE BOUND IS THE ONE THE CALLER ASKED FOR, on both sides. This is the failure mode a bound selector
     introduces and the retired scalars could not have: a surface still showing the previous bound's number
     after the reader switched. Reading the same pair at 1 ms must produce the 1 ms figures and the 1 ms
     delta - a shape where the two bounds disagree is exactly the divergence this family of tests forbids. */
  const shaped = { served: true, perf: cellPerf({ frontier: { 1: 5000, 10: 25500, none: 26000 }, added_latency_p99_us: 900 }) };
  const shapedBest = bcCell({ ingress: "openai", egress: "openai", frontier: { 1: 10000, 10: 30000, none: 31000 } });
  const at1 = app.cellPerfTip(shaped, "anthropic", "openai", shapedBest, 1);
  assert.ok(at1.includes("5,000 req/s while 99% of requests finished under 1 ms"), at1);
  assert.ok(at1.includes("-50.0% req/s vs the OpenAI→OpenAI cell"), `the delta is read at the SAME bound; got: ${at1}`);
  const at10 = app.cellPerfTip(shaped, "anthropic", "openai", shapedBest, 10);
  assert.ok(at10.includes("25,500 req/s while 99% of requests finished under 10 ms"), at10);
  assert.ok(at10.includes("-15.0% req/s vs the OpenAI→OpenAI cell"), at10);
});

// ---- sweep chart on a stub canvas with real committed data ------------------
function stubCanvas() {
  const calls = { lineTo: 0, fillText: 0, stroke: 0, arc: 0 };
  const ctx = new Proxy({}, {
    get(t, prop) {
      if (prop === "measureText") return () => ({ width: 10 });
      return (...a) => { if (prop in calls) calls[prop] += 1; };
    },
    set() { return true; },
  });
  return { width: 520, height: 230, getContext: () => ctx, calls };
}

// NIT-4: prefer the CANONICAL matrix best_cell sweep arrays (the single source the drawer actually
// charts) over the RETIRED results/perf/<gw>.json. Scan every gateway's matrix diagonal for a cell that
// carries the sweep arrays; fall back to the perf suite only if no matrix arrays exist yet (the shipped
// bundle predates the array-emitting matrix/run.sh - MED-5 coverage gap). existsSync-guarded so a
// cleaned results/perf/ (retirement) never ENOENT-crashes this test.
function committedSweep() {
  const mdir = join(ROOT, "results", "matrix");
  if (existsSync(mdir)) {
    for (const f of readdirSync(mdir).filter((x) => x.endsWith(".json"))) {
      let m; try { m = JSON.parse(readFileSync(join(mdir, f), "utf8")); } catch { continue; }
      for (const up of Object.values(m.upstreams || {})) {
        for (const c of Object.values((up && up.cells) || {})) {
          const p = c && c.perf;
          if (p && Array.isArray(p.sweep_sustained_20ms) && p.sweep_sustained_20ms.length > 3
              && Array.isArray(p.sweep_max_proxy)) return p;
        }
      }
    }
  }
  // Legacy fallback: whatever results/perf/*.json happens to be on disk. DISCOVERED, never a named
  // file - naming one made this helper silently return null the day that gateway was renamed or removed.
  const pdir = join(ROOT, "results", "perf");
  if (existsSync(pdir)) {
    for (const f of readdirSync(pdir).filter((x) => x.endsWith(".json")).sort()) {
      let p; try { p = JSON.parse(readFileSync(join(pdir, f), "utf8")); } catch { continue; }
      if (Array.isArray(p.sweep_sustained_20ms) && p.sweep_sustained_20ms.length > 3
          && Array.isArray(p.sweep_max_proxy)) return p;
    }
  }
  return null;
}

// A synthetic stand-in for `committedSweep()`, same shape (conc/rps rungs, doubling concurrency),
// used ONLY when no committed sweep data exists on disk (a clean checkout, an empty board, CI before
// any field run has ever published), so the test below always exercises `drawSweep` rather than passing
// by skipping on a bare board.
function syntheticSweep() {
  const rung = (conc, rps) => ({ conc, rps, p99_us: 9_000, fail: 0 });
  return {
    sweep_sustained_20ms: [rung(8, 4200), rung(16, 8100), rung(32, 15600), rung(64, 29800), rung(128, 41200)],
    sweep_max_proxy: [rung(8, 4300), rung(16, 8400), rung(32, 16100), rung(64, 31000), rung(128, 45995)],
  };
}

test("sweep chart draws real committed sweep data", () => {
  const perf = committedSweep() ?? syntheticSweep();
  assert.ok(Array.isArray(perf.sweep_sustained_20ms) && perf.sweep_sustained_20ms.length > 3);
  const canvas = stubCanvas();
  const series = [
    { label: "sustained @20ms", color: "#4cc38a", points: perf.sweep_sustained_20ms.map((p) => ({ x: p.conc, y: p.rps })) },
    { label: "max proxy", color: "#6cb6ff", points: perf.sweep_max_proxy.map((p) => ({ x: p.conc, y: p.rps })) },
  ];
  const geo = app.drawSweep(canvas, series, { yLabel: "RPS" });
  assert.ok(geo, "expected geometry back");
  assert.equal(geo.series.length, 2);
  assert.ok(canvas.calls.lineTo > series[0].points.length, "polyline segments drawn");
  assert.ok(canvas.calls.fillText > 4, "axis labels and ticks drawn");
  // log-x: pixel spacing between 8 and 32 equals spacing between 32 and 128
  const d1 = geo.X(32) - geo.X(8), d2 = geo.X(128) - geo.X(32);
  assert.ok(Math.abs(d1 - d2) < 1e-6, "x axis is logarithmic");
});

test("sweep chart marks the published peak and honors a shared x-domain", () => {
  const canvas = stubCanvas();
  const series = [{
    label: "max proxy", color: "#6cb6ff",
    points: [{ x: 32, y: 45061 }, { x: 52, y: 45995 }, { x: 256, y: 39747 }],
    mark: { x: 52, y: 45995, label: "45,995 @ c=52" },
  }];
  // shared x-domain wider than this series' own probed range: X() must span the shared domain, so
  // the two stacked charts (RPS + p99) align on ONE concurrency axis.
  const geo = app.drawSweep(canvas, series, { yLabel: "RPS", xDomain: [8, 2048] });
  assert.ok(geo, "geometry back");
  // the shared domain sets the axis extremes (log scale): X(8) is the left edge, X(2048) the right
  assert.ok(geo.X(2048) - geo.X(8) > geo.X(256) - geo.X(32), "axis spans the shared domain, not just the points");
  // a peak marker was drawn (extra arc strokes + a label beyond the plain point dots + ticks)
  assert.ok(canvas.calls.arc >= series[0].points.length + 2, "peak marker ring + dot drawn on top of the point dots");
  assert.ok(canvas.calls.fillText > 4, "peak label + axis text drawn");
});

test("sweep chart degrades cleanly with no data", () => {
  const canvas = stubCanvas();
  assert.equal(app.drawSweep(canvas, [{ label: "empty", color: "#fff", points: [] }], {}), null);
});

// ---- protocol matrix: cell states + grey-cell cited tooltip -----------------
test("matrix cell states map served to the three visible states", () => {
  assert.equal(app.cellState({ served: true })[0], "served");
  assert.equal(app.cellState({ served: false })[0], "failed");
  assert.equal(app.cellState({ served: "not_configurable" })[0], "notconf");
  // the grey label reads as a declaration, not our omission
  assert.equal(app.cellState({ served: "not_configurable" })[1], "not declared");
});

test("machine-readable served states map to distinct honest cell states", () => {
  // not_verified is a harness gap, never a red
  assert.equal(app.cellState({ served: "not_verified", reason: "harness_boot_failure" })[0], "unverified");
  // untestable is a rig limit (real cloud host pinned), its own state, never a red
  assert.equal(app.cellState({ served: "untestable", reason: "no_base_url_override" })[0], "untestable");
  assert.equal(app.cellState({ served: "untestable" })[1], "untestable (mock limit)");
  assert.ok(app.matrixCellTip({ served: "untestable" }).includes("untestable on this rig"));
  // served:false with an explicit reason (wrong_answer) is the ONLY red: not a harness gap
  assert.equal(app.cellState({ served: false, reason: "wrong_answer", status: "200" })[0], "failed");
  // a lane the gateway never declared reads "not declared", never a failure
  assert.equal(app.naText({ xlate_declared: false, xlate_served: false }, "xlate_served", "xlate_error").text, "not declared");
});

test("a grey (not_configurable) cell tooltip shows the gateway's cited reason", () => {
  // Shape of a real cited capability limit (they are written by each manifest, about its own project).
  const reason = "this gateway accepts only OpenAI-canonical ingress and emits no OpenAI-Responses route_type";
  const tip = app.matrixCellTip({ served: "not_configurable", verdict_note: reason });
  // HONEST wording: grey = not in the grid WE drafted, not a claim the maintainer declined it.
  assert.ok(tip.includes("not in the capability grid we drafted"), "reads as our omission, not the gateway's declared incapability");
  assert.ok(tip.includes(reason), "carries the cited capability-limit reason");
  // no reason present: still honest, never a bare "untested"
  const bare = app.matrixCellTip({ served: "not_configurable" });
  assert.ok(bare.includes("not in the capability grid we drafted"));
});

test("probe-first: a not_configured cell renders grey with the probe evidence, never a red", () => {
  // state class: same visual bucket as the legacy declaration-grey, its own honest label
  assert.equal(app.cellState({ served: "not_configured", reason: "probe_failed" })[0], "notconf");
  assert.equal(app.cellState({ served: "not_configured", reason: "probe_failed" })[1], "not configured");
  // tooltip leads with "not configured" and carries the probe's own evidence (probe_note)
  const ev = "probe failed: HTTP 404; upstream request landed on the openai endpoint, not the gemini endpoint";
  const tip = app.matrixCellTip({ served: "not_configured", reason: "probe_failed", probe_note: ev });
  assert.ok(tip.startsWith("not configured"), "leads with the state, not a failure verdict");
  assert.ok(tip.includes(ev), "carries the probe evidence");
  // no probe_note (defensive): fall back to the verdict prose, still never a bare grey
  const tip2 = app.matrixCellTip({ served: "not_configured", verdict_note: "HTTP 404 on POST /v2/chat" });
  assert.ok(tip2.includes("HTTP 404 on POST /v2/chat"));
  // and it is NEVER counted or shown as a failure
  assert.notEqual(app.cellState({ served: "not_configured" })[0], "failed");
});

test("gen-data preserves the per-cell verdict_note reason for grey cells", () => {
  const withGrey = data.gateways.find((g) =>
    g.matrix && g.matrix.upstreams &&
    Object.values(g.matrix.upstreams).some((u) =>
      u.cells && Object.values(u.cells).some((c) => c.served === "not_configurable" && c.verdict_note)));
  // Once field results with declared-0 cells land, the cited reason must survive gen-data. If no
  // committed matrix result carries one yet, skip rather than fail (vacuous pre-field-run).
  if (withGrey) {
    const cell = Object.values(withGrey.matrix.upstreams)
      .flatMap((u) => Object.values(u.cells || {}))
      .find((c) => c.served === "not_configurable" && c.verdict_note);
    assert.ok(typeof cell.verdict_note === "string" && cell.verdict_note.length > 0);
  }
});

// ---- unified cell chooser: the three modes pick the right cell -----------------
// A gateway serving three cells with distinct numbers: its openai diagonal (best_cell), a slower
// anthropic diagonal, and an openai->anthropic translation cell.
const CHOOSER_GW = {
  key: "cg", display: "CG", lang: "Rust",
  // Each cell carries its own frontier, and the three have DIFFERENT SHAPES on purpose: the openai
  // diagonal is nearly flat (30,000 at 1 ms), the anthropic diagonal needs a loose tail to reach its own
  // ceiling (12,000 at 1 ms, 25,000 unbounded), the translation cell sits between them. A chooser that
  // read the wrong cell used to show a wrong number; it can now also show a wrong SHAPE, which is the
  // thing a reader is being asked to compare.
  best_cell: bcCell({ dialect: "openai", added_latency_p50_us: 100, added_latency_p99_us: 110,
    frontier: { 1: 29000, 10: 30000, none: 32000 } }),
  matrix: { upstreams: {
    openai: { cells: { openai: { served: true, perf: cellPerf({
      added_latency_p50_us: 100, added_latency_p99_us: 110, frontier: { 1: 29000, 10: 30000, none: 32000 } }) } } },
    anthropic: { cells: {
      anthropic: { served: true, perf: cellPerf({
        added_latency_p50_us: 200, added_latency_p99_us: 220, frontier: { 1: 12000, 10: 25000, none: 27000 } }) },
      openai: { served: true, perf: cellPerf({
        added_latency_p50_us: 130, added_latency_p99_us: 145, frontier: { 1: 20000, 10: 26000, none: 28000 } }) } } },
  } },
};

test("cell chooser: a shared ?mode= link renders the mode it names, and a tab flip is lossless", () => {
  /* FOUND BY SCREENSHOTTING EVERY VIEW x EVERY MODE and comparing the control to the URL that produced it:
     /gateways/performance?mode=same&d=openai rendered OWN CELL, with "?mode=same&d=openai" still in the
     address bar. Every ?mode= link ever shared to a perf tab showed the wrong cells under a URL that named
     the right ones, and on memory every ?mode= link showed Min. decodeUrl was never at fault - it parses the
     mode correctly - showView threw it away one line later.

     THE MECHANISM. showView set `state.view = view` and THEN computed `modeFamily(state.view)` as the view
     being LEFT, so leaving and arriving always compared equal, the stash branch was unreachable, and what
     actually ran on every render was `state.mode = resolveMode(modeMemo[family])`. The memo is pre-seeded
     (perf:"peak", memory:"min"), so `?? state.mode` never fell through and the seed won every time.

     Asserted on modeOnArrival, the pure decision showView now delegates to, because showView is DOM-bound and
     this has to be provable without a browser. The source-order guard below is what stops the two lines from
     being swapped back. */
  const seed = { perf: "peak", memory: "min" };

  // A SAME-FAMILY ARRIVAL KEEPS THE MODE IT WAS GIVEN. This is the deep-link case: boot decodes ?mode=same
  // into state and the first render must not overwrite it - and neither must the second, or a re-render.
  for (const m of ["peak", "same", "custom"])
    assert.equal(app.modeOnArrival("performance", "performance", m, seed).mode, m,
      `a re-render of the view you are on must not replace ?mode=${m} with the memo's seed`);
  // Including across the perf lanes, which are ONE family: Frontier -> Performance is not a family change.
  assert.equal(app.modeOnArrival("frontier", "performance", "custom", seed).mode, "custom");
  assert.equal(app.modeOnArrival("streaming", "frontier", "same", seed).mode, "same");
  // And on memory, whose seed is "min": ?mode=max must survive its own first render.
  for (const m of ["min", "max", "same", "custom"])
    assert.equal(app.modeOnArrival("memory", "memory", m, seed).mode, m, `?mode=${m} must survive on memory`);

  /* A CROSS-FAMILY FLIP STILL COERCES, AND IS STILL LOSSLESS - the behaviour the memo exists for, which was
     dead code until now. Custom on Performance -> Memory coerces to the memory family's remembered mode, and
     coming back restores Custom rather than the coercion memory forced. */
  const toMem = app.modeOnArrival("performance", "memory", "custom", seed);
  assert.equal(toMem.mode, "min", "memory renders its own family's mode, never a perf-selected one");
  assert.equal(toMem.memo.perf, "custom", "and the outgoing family's choice is stashed, not discarded");
  const back = app.modeOnArrival("memory", "performance", toMem.mode, toMem.memo);
  assert.equal(back.mode, "custom", "so the round trip restores the reader's own choice");
  assert.equal(back.memo.memory, "min", "and stashes memory's in turn, so the next flip is lossless too");
  // A mode the arriving view cannot offer is still coerced, never rendered as a mode it does not have.
  assert.equal(app.modeOnArrival("performance", "memory", "peak", { perf: "peak" }).mode, "min",
    "peak selects on throughput, which is exactly what a memory number must not be selected by");

  /* THE SOURCE-ORDER GUARD. The defect was one line in the wrong order, and it is invisible in behaviour
     until you compare a rendered control to the URL beside it, so it is pinned here in the shape it broke. */
  const src = readFileSync(join(HERE, "app.js"), "utf8");
  // Matched on STATEMENT LINES, not on substrings: the comment above the fix quotes `state.view = view`
  // verbatim to explain the defect, and a substring search would find the prose before the code.
  const lines = src.slice(src.indexOf("function showView(view) {")).split("\n");
  const at = (re) => lines.findIndex((l) => re.test(l));
  const iLeaving = at(/^\s*const leaving = modeFamily\(state\.view\);\s*$/);
  const iAssign = at(/^\s*state\.view = view;\s*$/);
  assert.ok(iLeaving >= 0 && iAssign >= 0, "showView still captures the outgoing view and assigns the new one");
  assert.ok(iLeaving < iAssign,
    "showView must read the OUTGOING view before state.view moves, or `leaving` is the view being arrived at");
});

test("cell chooser: Peak reads the best diagonal, Same reads a chosen diagonal, Custom any cell", () => {
  const g = CHOOSER_GW;
  // Peak → the openai best diagonal (110 p99, 30000 sustained), with the Tested-on dialect openai.
  const peak = { ...app.newState(), mode: "peak" };
  assert.equal(app.chooserPerfCell(g, "added_latency_p99_us", String, peak).text, "110");
  assert.equal(app.frontierChooserCell(g, peak).v, 30000);
  assert.deepEqual(app.chooserDialects(g, peak), ["openai", "openai"]);
  // Same anthropic → the anthropic→anthropic diagonal (220 p99, 25,000 at the default bound).
  const same = { ...app.newState(), mode: "same", sameDialect: "anthropic" };
  assert.equal(app.chooserPerfCell(g, "added_latency_p99_us", String, same).text, "220");
  assert.equal(app.frontierChooserCell(g, same).v, 25000);
  assert.deepEqual(app.chooserDialects(g, same), ["anthropic", "anthropic"]);
  // Custom openai→anthropic → the translation cell (145 p99, 26,000).
  const cust = { ...app.newState(), mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  assert.equal(app.chooserPerfCell(g, "added_latency_p99_us", String, cust).text, "145");
  assert.equal(app.frontierChooserCell(g, cust).v, 26000);
  // AND THE CHOSEN CELL'S SHAPE TRAVELS WITH IT. The three cells have different curves; the shape column
  // must read the chosen one, or the row shows one cell's rate beside another cell's slope.
  assert.equal(app.frontierShapeCell(g, peak).text, `${app.heldPct(29000 / 32000)}% of its full rate at ${app.boundLabel(1)}`);
  assert.equal(app.frontierShapeCell(g, same).text, `${app.heldPct(12000 / 27000)}% of its full rate at ${app.boundLabel(1)}`);
  // Not the same share, because they are not the same cell: 91% against 44%.
  assert.notEqual(app.frontierShapeCell(g, peak).text, app.frontierShapeCell(g, same).text);
  // A cell the gateway does NOT serve reads n/a (never fabricated), and the row is not dropped.
  const missing = { ...app.newState(), mode: "custom", xlateIn: "gemini", xlateOut: "cohere" };
  assert.equal(app.frontierChooserCell(g, missing).na, true);
  assert.equal(app.frontierShapeCell(g, missing).na, true);
  assert.equal(app.chooserHasCell(g, missing), false);
});

test("Cluster-B: drawer/compare (laneRecord) read the SAME chosen cell as the table in every mode", () => {
  const g = CHOOSER_GW;
  const perfLane = app.LANES.find((l) => l.key === "perf");
  // In each mode the drawer/compare perf lane record must match the table column value (chooserPerfCell).
  for (const st of [
    { mode: "peak" },
    { mode: "same", sameDialect: "anthropic" },
    { mode: "custom", xlateIn: "openai", xlateOut: "anthropic" },
  ]) {
    const rec = app.laneRecord(perfLane, g, st);
    assert.ok(rec, `lane record present in ${st.mode}`);
    for (const k of ["added_latency_p99_us", "rps_sustained_20ms", "rps_max_proxy"]) {
      const tableV = app.chooserPerfCell(g, k, String, st).v;
      // The lane record's metric is a sealed envelope; mval() reads its displayable value.
      assert.equal(app.mval(rec[k]), tableV, `${st.mode}: drawer/compare ${k} == table`);
    }
  }
  // The Peak record still equals the canonical (Peak) accessor - no regression on the default mode.
  assert.equal(app.mval(app.laneRecord(perfLane, g, { mode: "peak" }).rps_sustained_20ms),
    app.mval(app.canonicalPerf(g).rps_sustained_20ms), "Peak lane record == canonicalPerf");
});

// Finding 22 was that the drawer chart plotted a curve for a metric the table showed no number for, so
// the chart contradicted the cell beside it. The fix was structural: the sweep array travels INSIDE the
// sealed envelope (env.sweep), so a metric with no published value has no curve to plot either.
//
// The state the test used to demonstrate that with was a suppression - a `mock_bound: true` sustained
// whose 99,999 and whose sweep were both discarded at seal time. A near-ceiling number is now published,
// and its curve is published WITH it: that curve is the evidence for exactly the reading a reader most
// needs to weigh, and dropping it was the second half of the same mistake. The invariant survives
// against the state that genuinely publishes nothing - a metric that was never measured on this cell.
/* THE DRAWER/COMPARE CURVE IS ONE SWEEP, MARKED AT THE SELECTED BOUND.
   This test used to assert that perfSweepSeries plotted TWO curves (the sustained sweep and the max-proxy
   sweep) and dropped either one whose headline was absent. Both halves are gone with the two metrics: they
   were ONE sweep read twice, so the "two curves" were the same rungs drawn twice with two markers, and the
   pair could disagree with itself. The cell now publishes that sweep ONCE (`rec.sweep`) and every reading is
   a maximum over a subset of it, so the properties worth guarding are the ones asserted here:
     - one series, off the cell's own rungs, chooser-aware (it must be the CHOSEN cell's sweep);
     - the marker is the reading AT THE SELECTED BOUND, at the concurrency that reading names - so the dot a
       reader sees is the number the ranked column shows, and moving the bound moves the dot;
     - a cell with no rungs plots nothing rather than an empty frame captioned as a measurement. */
test("Cluster-B/22: perfSweepSeries plots the ONE sweep, chooser-aware, marked at the selected bound", () => {
  const colors = { sustained: "#4cc38a", max: "#6cb6ff" };
  const rungs = [
    { conc: 8, rps: 20000, p99_us: 900, fail: 0 },
    { conc: 64, rps: 30000, p99_us: 4000, fail: 0 },
    { conc: 512, rps: 32000, p99_us: 40000, fail: 0 },
  ];
  const withSweep = { ...CHOOSER_GW, best_cell: bcCell({ dialect: "openai", sweepRungs: rungs,
    frontier: { 1: 20000, 10: 30000, none: 32000 },
    frontierOpts: { conc: 64 } }) };
  const peak = app.perfSweepSeries(withSweep, colors, { ...app.newState(), mode: "peak" });
  assert.equal(peak.length, 1, "ONE sweep, not one per collapsed reading");
  assert.equal(peak[0].sweep.length, 3, "every probed rung is on the curve");
  assert.equal(peak[0].peak.rps, 30000, "marked at the reading for the selected bound");
  assert.equal(peak[0].peak.conc, 64, "at the concurrency that reading names");
  assert.match(peak[0].label, /10 ms/, "and the label says which bound the mark is");
  // MOVE THE BOUND: the same rungs, a different mark. A curve whose marker did not follow the selector
  // would show the reader a dot that is not the number in the column beside it.
  const at1 = app.perfSweepSeries(withSweep, colors, { ...app.newState(), mode: "peak", bound: 1 });
  assert.equal(at1[0].peak.rps, 20000);
  assert.match(at1[0].label, /1 ms/);
  // A reading that is absent at the selected bound marks nothing - the curve is still the evidence, but
  // nothing on it may be labelled as a published number that does not exist.
  const noReading = { ...CHOOSER_GW, best_cell: bcCell({ dialect: "openai", sweepRungs: rungs,
    frontier: { none: 32000 } }) };
  const unmarked = app.perfSweepSeries(noReading, colors, { ...app.newState(), mode: "peak" });
  assert.equal(unmarked.length, 1);
  assert.equal(unmarked[0].peak, null, "no reading at this bound, so no marker");
  // NO RUNGS: nothing is plotted at all (a legacy record, or a cell whose sweep never landed).
  const noRungs = { ...CHOOSER_GW, best_cell: bcCell({ dialect: "openai", frontier: 30000 }) };
  assert.deepEqual(app.perfSweepSeries(noRungs, colors, { ...app.newState(), mode: "peak" }), []);
});

test("Cluster-C/20: chooserStreamCell reads the right streaming cell across Peak/Same/Custom", () => {
  // A gateway whose streaming was projected from the openai diagonal (matrix per-cell stream), plus an
  // openai->anthropic cell that carries its own per-cell stream record.
  const g = {
    key: "sc", display: "SC", lang: "Rust",
    streaming: streamRec({ dialect: "openai", added_ttft_p99_us: 90, streams_sustained: 1300, cpu_fps: 48000 }),
    matrix: { upstreams: {
      openai: { cells: { openai: { served: true, perf: cellPerf({ added_latency_p99_us: 10 }),
        stream: cellStream({ added_ttft_p99_us: 90, streams_sustained: 1300 }) } } },
      anthropic: { cells: { openai: { served: true, perf: cellPerf({ added_latency_p99_us: 20 }),
        stream: cellStream({ added_ttft_p99_us: 140, streams_sustained: 900 }) } } },
    } },
  };
  // Peak → the projected diagonal streaming (90 TTFT, 1300 streams).
  const peak = { mode: "peak" };
  assert.equal(app.chooserStreamCell(g, "added_ttft_p99_us", String, peak).text, "90");
  assert.equal(app.chooserStreamCell(g, "streams_sustained", String, peak).text, "1300");
  // Same openai → the same diagonal it was measured on (still 90).
  const sameOa = { mode: "same", sameDialect: "openai" };
  assert.equal(app.chooserStreamCell(g, "added_ttft_p99_us", String, sameOa).text, "90");
  // Same anthropic → the diagonal was measured on openai, NOT anthropic → n/a (never fabricated).
  const sameAn = { mode: "same", sameDialect: "anthropic" };
  assert.equal(app.chooserStreamCell(g, "added_ttft_p99_us", String, sameAn).na, true);
  // Custom openai->anthropic → that cell's OWN per-cell stream record (140 TTFT, 900 streams).
  const cust = { mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  assert.equal(app.chooserStreamCell(g, "added_ttft_p99_us", String, cust).text, "140");
  assert.equal(app.chooserStreamCell(g, "streams_sustained", String, cust).text, "900");
  // A cell with no per-cell stream reads n/a.
  const missing = { mode: "custom", xlateIn: "gemini", xlateOut: "cohere" };
  assert.equal(app.chooserStreamCell(g, "added_ttft_p99_us", String, missing).na, true);
});

test("Cluster-C/12: the streaming caption is CONDITIONAL on provenance (no hard 6x6 claim on fallback)", () => {
  const st = { ...app.newState(), mode: "peak" };
  // All-fallback streaming (today's real data): the caption must NOT claim the 6x6 run for streaming.
  const fbData = { gateways: [{ key: "a", streaming: { source: { kind: "stream-fallback" } } },
    { key: "b", streaming: { source: { kind: "stream-fallback" } } }] };
  assert.equal(app.streamingProvenance(fbData).all, "fallback");
  const fbCap = app.captionText(app.chooserCaption("streaming", st, fbData));
  assert.ok(!/from the one 6x6 run/.test(fbCap), `fallback streaming caption must not positively claim the 6x6 run; got: ${fbCap}`);
  assert.ok(/stream suite/.test(fbCap), "fallback caption names the standalone stream suite");
  // Matrix-sourced streaming: the 6x6 claim IS honest.
  const mxData = { gateways: [{ key: "a", streaming: { source: { kind: "matrix" } } }] };
  assert.equal(app.streamingProvenance(mxData).all, "matrix");
  assert.ok(/from the one 6x6 run/.test(app.captionText(app.chooserCaption("streaming", st, mxData))), "matrix streaming may claim the 6x6 run");
  // The Performance (perf) tab is always the 6x6 matrix - its caption is unaffected by streaming provenance.
  assert.ok(/from the one 6x6 run/.test(app.captionText(app.chooserCaption("performance", st, fbData))), "perf caption always names the 6x6 run");
});

test("Δ-to-Peak: a non-peak cell reports its deviation vs the gateway's own best diagonal", () => {
  const g = CHOOSER_GW;
  const cust = { ...app.newState(), mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  const cp = { ingress: "openai", egress: "anthropic", ...app.chooserCellPerf(g, cust) };
  const d = app.deltaToPeak(cp, g.best_cell, 10);
  // p99 145 vs 110 = +31.8% latency; 26,000 vs 30,000 at the 10 ms bound = -13.3% req/s.
  assert.ok(/\+31\.8% latency/.test(d), d);
  // THE THROUGHPUT HALF NAMES ITS BOUND. Both sides are read at the same one, and the label says which:
  // a bare "-13.3% RPS" over a board with six published readings is a percentage between two unstated
  // questions, which is the ambiguity the two retired scalars shipped with.
  assert.ok(/-13\.3% req\/s at 10 ms/.test(d), d);
  // At a DIFFERENT bound the same pair of cells deviates differently, because the two cells have different
  // shapes - which is the finding, and it is invisible if the delta is computed at one hidden bound.
  const at1 = app.deltaToPeak(cp, g.best_cell, 1);
  assert.ok(/-31\.0% req\/s at 1 ms/.test(at1), at1);
  // The peak cell itself has no delta.
  assert.equal(app.deltaToPeak({ ingress: "openai", egress: "openai", ...g.best_cell }, g.best_cell, 10), "");
});

testWithData("matrix popup shows the SAME chosen-cell values the Performance/Custom table shows, at the SAME bound", () => {
  const g = CHOOSER_GW;
  const cust = { ...app.newState(), mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  const html = app.cellPopFull(g, "openai", "anthropic");
  // The popup carries the cell's own numbers (formatted en-US) …
  assert.ok(html.includes("<b>145</b>"), "popup shows the cell's added latency p99");
  assert.ok(html.includes("<b>26,000 @ 512 conc</b>"), `popup shows the cell's reading at the shown bound; got ${html}`);
  // … LABELLED WITH THE BOUND IT WAS READ AT, and the same one the table's ranked column is showing.
  assert.ok(html.includes(app.boundColLabel(app.selectedBound(app.state))), "the popup names the bound it read");
  // … the SAME number the Custom table reads through the same accessor …
  assert.equal(app.frontierChooserCell(g, cust).text, "26,000 @ 512 conc");
  /* … AND IT FOLLOWS THE SELECTOR. A bound selector adds a way for two surfaces to disagree that the
     retired scalars could not: the popup could keep showing 10 ms after the reader switched to 1 ms. Both
     surfaces read selectedBound() through the same accessors, so switching moves both. */
  const prev = app.state.bound;
  try {
    app.state.bound = 1;
    const html1 = app.cellPopFull(g, "openai", "anthropic");
    assert.ok(html1.includes("<b>20,000 @ 512 conc</b>"), `the popup follows the selected bound; got ${html1}`);
    assert.ok(html1.includes(app.boundColLabel(1)), "and relabels itself with the bound it is now reading");
    assert.equal(app.frontierChooserCell(g, { ...cust, bound: 1 }).text, "20,000 @ 512 conc",
      "the table reads the same reading at the same bound");
  } finally { app.state.bound = prev; }
  // … and the Δ-to-Peak vs the gateway's own best diagonal.
  // "vs its own cell", NOT "vs peak": best_cell prefers the openai diagonal and otherwise ranks on latency,
  // so it is a representative cell and a positive req/s delta against it is ordinary, not impossible.
  assert.ok(/vs its own cell \(OpenAI→OpenAI\)/.test(html), "popup names the reference cell without calling it a peak");
  assert.ok(!/vs peak/.test(html), "and never calls it the peak");
  assert.ok(/\+31\.8% latency/.test(html), "popup shows Δ latency");
  // THE SHAPE IS ON THE POPUP TOO: a rate alone cannot say whether this cell is fast because the tail was
  // allowed to grow, which is what the matrix is most often opened to find out.
  assert.ok(/frontier-spark/.test(html), "the popup carries the cell's own curve");
  // The consistency guard proves popup == table per cell on the whole bundle (no divergence ships).
  const { errors } = checkConsistency(data, app);
  assert.deepEqual(errors, [], `popup/table divergence: ${JSON.stringify(errors)}`);
});

test("Memory tab renders idle/peak/recovered/sparkline, n/a when a field is absent", () => {
  const cols = app.COLUMN_SETS.memory;
  const ids = cols.map((c) => c.id);
  for (const id of ["memidle", "mempeak", "memrecov", "memcurve"]) assert.ok(ids.includes(id), `memory tab has ${id}`);
  // A full record: every column shows its value, the curve renders an SVG.
  const full = { key: "m", display: "M", lang: "Rust", memory_read: memRec({
    idle_rss_mib: 40, peak_rss_mib: 900, recovered_rss_mib: 55,
    rss_series: [{ t_s: 0, rss_mib: 40 }, { t_s: 60, rss_mib: 900 }, { t_s: 180, rss_mib: 55 }] }) };
  assert.equal(cols.find((c) => c.id === "memidle").get(full).text, "40.0");
  assert.equal(cols.find((c) => c.id === "mempeak").get(full).text, "900.0");
  assert.equal(cols.find((c) => c.id === "memrecov").get(full).text, "55.0");
  const curve = cols.find((c) => c.id === "memcurve");
  assert.equal(curve.get(full).na, false, "a series enables the curve column");
  assert.ok(/<svg/.test(curve.render(full)), "the curve column renders an inline SVG sparkline");
  // An absent-field gateway: n/a everywhere, no fabricated 0, the curve cell reads n/a (no line).
  const bare = { key: "b", display: "B", lang: "Rust", memory_read: memRec({ idle_rss_mib: 40, peak_rss_mib: null, recovered_rss_mib: null }) };
  assert.equal(cols.find((c) => c.id === "memrecov").get(bare).na, true);
  assert.equal(cols.find((c) => c.id === "memrecov").get(bare).text, "not measured");
  assert.equal(curve.get(bare).na, true, "no series → the curve column reads n/a");
  assert.ok(/n\/a/.test(curve.render(bare)) && !/<svg/.test(curve.render(bare)), "no series → n/a, never a fabricated line");
  // A gateway with no memory record at all → n/a, never a crash.
  const none = { key: "n", display: "N", lang: "Rust" };
  assert.equal(cols.find((c) => c.id === "memidle").get(none).na, true);
});

test("Memory tab attributes each gateway's peak cell (load_cell) and states the fixed-load basis", () => {
  const cols = app.COLUMN_SETS.memory;
  const memcell = cols.find((c) => c.id === "tested");
  assert.ok(memcell, "the memory tab has a Tested-on (load_cell) column");
  // A gateway measured on its anthropic>anthropic peak cell shows that cell, prettified.
  const g = { key: "m", display: "M", lang: "Rust", memory_read: memRec({
    load_cell: "anthropic>anthropic", load_recipe: { concurrency: 64, payload_bytes: 4096, duration_s: 120 },
    idle_rss_mib: 40, peak_rss_mib: 900, recovered_rss_mib: 55 }) };
  const cell = memcell.render(g);
  assert.ok(/Anthropic/.test(cell), "the Tested-on cell names the peak cell's dialect");
  assert.ok(/64|4,?096|120/.test(cell), "the Tested-on tooltip states the fixed-load recipe (the fair-load basis)");
  // A gateway with no load_cell (no served cell) reads n/a, never a fabricated cell.
  const bare = { key: "b", display: "B", lang: "Rust", memory_read: memRec({ idle_rss_mib: 40, peak_rss_mib: null, recovered_rss_mib: null }) };
  assert.equal(memcell.get(bare).na, true);
  assert.ok(!memcell.render(bare).includes("tested-pill"), "no load_cell -> no pill");
  // The caption states the fixed identical load + cold-restart, NOT a "6x6 drives memory" claim.
  const cap = app.captionText(app.memoryCaption({ gateways: [] }));
  assert.ok(/identical fixed load/i.test(cap) && /cold-?restart/i.test(cap), "caption states the fair fixed-load basis + cold restart");
  assert.ok(!/6x6 (sweep )?drives/i.test(cap), "caption drops any 6x6-drives-memory wording");
});

test("URL round-trips the chooser mode + selection (peak / same / custom)", () => {
  const rt = (path, search) => {
    const st = app.decodeUrl(path, search);
    const url = app.encodeUrl(st);
    const u = new URL(url, "https://onthebench.ai");
    return { st, back: app.decodeUrl(u.pathname, u.search), url };
  };
  // Peak is the clean default: no mode param.
  const peak = rt("/gateways/performance", "");
  assert.equal(peak.st.mode, "peak");
  assert.ok(!peak.url.includes("mode="), `peak is clean: ${peak.url}`);
  // Same carries the dialect.
  const same = rt("/gateways/performance", "?mode=same&d=anthropic");
  assert.equal(same.st.mode, "same");
  assert.equal(same.st.sameDialect, "anthropic");
  assert.ok(same.url.includes("mode=same") && same.url.includes("d=anthropic"), same.url);
  assert.equal(same.back.mode, "same");
  assert.equal(same.back.sameDialect, "anthropic");
  // Custom carries the in→out pair.
  const cust = rt("/gateways/performance", "?mode=custom&in=anthropic&out=openai");
  assert.equal(cust.st.mode, "custom");
  assert.equal(cust.st.xlateIn, "anthropic");
  assert.equal(cust.st.xlateOut, "openai");
  assert.ok(cust.url.includes("mode=custom") && cust.url.includes("in=anthropic") && cust.url.includes("out=openai"), cust.url);
  assert.equal(cust.back.xlateIn, "anthropic");
  assert.equal(cust.back.xlateOut, "openai");
  // A legacy Matched link (xin/xout, no mode) lands in Custom on that pinned pair.
  const legacy = app.decodeUrl("/gateways/translation", "?xin=anthropic&xout=openai");
  assert.equal(legacy.view, "performance");
  assert.equal(legacy.mode, "custom");
  assert.equal(legacy.xlateIn, "anthropic");
  assert.equal(legacy.xlateOut, "openai");
});

/* ================================================================================================
   AUDIT GROUP C - the 11th-phase MISSING TESTS. Each block is a CLASS test (one test covering all the
   siblings of a finding), and each carries its RED-before proof: the assertion is written so that
   reverting the fix makes it fail, and where the subject is a LINT the lint is driven against synthetic
   source that CONTAINS the violation, so "the check works" is demonstrated rather than assumed.
   ================================================================================================ */

// ---- sealMetric(), the single honesty choke point, is tested DIRECTLY -----------------------
// The fixture helpers call the REAL exported function, so every "honesty" test in this file is a test OF
// seal.mjs rather than of a copy that could silently diverge; these cases pin the contract itself.
test("#24 CLASS: the REAL sealMetric() is the honesty choke point (no hand-copied logic in the tests)", () => {
  // The fixture builders must be wired to the REAL seal - this is what makes every other test in this
  // file a test OF seal.mjs rather than of a copy that can silently diverge.
  assert.equal(seal, sealMetric, "test fixtures must seal through the REAL seal.mjs export");
  // (a) absent -> NOT MEASURED (never suppressed: nothing was hidden, nothing was measured).
  assert.deepEqual(sealMetric(null), { value: null, certified: false, suppressed: false, reason: "not_measured" });
  assert.deepEqual(sealMetric(undefined), { value: null, certified: false, suppressed: false, reason: "not_measured" });
  // (b) PRESENT -> always certified. There is no second condition any more: this used to be the "ungated"
  //     case (latency/RSS, which had no mock-bound flag) and it is now the whole rule. A present ZERO is
  //     certified too, and case (d) below pins its shape, because a 0 additionally carries a note.
  assert.deepEqual(sealMetric(12.5), { value: 12.5, certified: true, suppressed: false });
  // (c) A PRESENT NUMBER IS PUBLISHED, whatever the comparison says about it.
  //
  //     This case used to be the gate: `sealMetric(100, {gated: true, flag: true})` returned
  //     {value: null, certified: false, suppressed: true, reason: "mock_bound"}, and a `null` flag (no
  //     usable reference) returned the same with reason "unverifiable". Both threw away a measurement the
  //     harness had taken correctly, on the strength of a comparison against our own rig - and hardest on
  //     the gateways that came closest to keeping up. Nothing about the number was in doubt; only how to
  //     weigh it, and the answer to that is to publish the weighing, not to delete the number.
  //
  //     So the facts ride ON the certified envelope, and the seal reaches no conclusion:
  assert.deepEqual(sealMetric(100, { headroom: 0.83, ceiling: 120 }),
    { value: 100, certified: true, suppressed: false, headroom: 0.83, rig_ceiling: 120 },
    "a near-ceiling number publishes, with the fraction and the ceiling a reader needs to weigh it");
  assert.deepEqual(sealMetric(100, { headroom: 0.999, ceiling: 100.1 }),
    { value: 100, certified: true, suppressed: false, headroom: 0.999, rig_ceiling: 100.1 },
    "even AT the ceiling: 0.999 is a fact about the comparison, never a reason to withhold the number");
  assert.deepEqual(sealMetric(100), { value: 100, certified: true, suppressed: false },
    "and no usable reference costs the FACTS, not the value - it used to cost both");
  // (d) a measured 0 is CERTIFIED and carries a note naming what the zero MEANS: never folded into
  //     {value:null, reason:"not_measured"}, which would publish a measured FAILURE as an unmeasured cell.
  //
  //     THE NOTE IS PER-FIELD, NOT A DEFAULT. This case asserted that a bare `sealMetric(0, {})` came
  //     back annotated ZERO_NO_CEILING, because the note used to be the default argument and the caller
  //     only reached it under a `gated` flag. With the gate gone the default became live for every field,
  //     and 37 `growth_rate_mib_per_min` zeros and 6 `added_gap_p50_us` zeros shipped claiming "served,
  //     but no tested load held p99 < 1 s at <0.1% errors" - a sentence about a throughput ceiling,
  //     rendered on a memory growth rate. `zeroNoteFor` now answers null outside the two families that
  //     have such a meaning, so an unannotated zero stays a bare 0.
  assert.deepEqual(sealMetric(0, {}),
    { value: 0, certified: true, suppressed: false },
    "a zero on a field with no zero-note vocabulary must not borrow a throughput sentence");
  assert.equal(zeroNoteFor("growth_rate_mib_per_min"), null,
    "and the ONE place that mapping lives must be the thing that says so");
  /* THE FIELD THAT STILL CARRIES THIS NOTE IS A STREAMING ONE. It used to be `rps_max_proxy`: a throughput
     ceiling whose zero meant "served, but no tested load held the qualifying gates". That metric is
     deleted, and a frontier reading has no zero of that kind at all - a bound no rung qualified at is an
     ABSENCE with the engine's own reason, not a certified 0. `streams_sustained` is the surviving member of
     the family (seal.mjs THROUGHPUT_FIELDS), so it is what proves the per-field mapping still works. */
  assert.deepEqual(sealMetric(0, { zeroNote: zeroNoteFor("streams_sustained") }),
    { value: 0, certified: true, suppressed: false, note: ZERO_MEASURED_FAIL },
    "a surviving throughput field's zero carries its own note, and says which zero it is");
  assert.equal(zeroNoteFor("rps_max_proxy"), null,
    "and the RETIRED throughput fields have no note left to borrow: nothing may emit them");
  assert.deepEqual(sealMetric(0, { zeroNote: ZERO_MEASURED_FAIL }),
    { value: 0, certified: true, suppressed: false, note: ZERO_MEASURED_FAIL },
    "a measured stream-sustain FAILURE must be a certified 0, DISTINGUISHABLE from not-measured");
  // and the two states are not the same object shape.
  assert.notDeepEqual(sealMetric(0, { zeroNote: ZERO_MEASURED_FAIL }), sealMetric(null));
  // (e) extras ride on a certified envelope; an ABSENCE - the only remaining no-value shape - carries no
  //     recoverable number at all, which is what C2 asserts from the bundle side.
  const withExtras = sealMetric(50, { extras: { concurrency: 8, conc_at: 16, sweep: null } });
  assert.equal(withExtras.concurrency, 8);
  assert.equal(withExtras.conc_at, 16);
  assert.ok(!("sweep" in withExtras), "a null extra must not be emitted");
  const absent = sealMetric(null, { extras: { concurrency: 8, conc_at: 16 }, headroom: 0.5, ceiling: 100 });
  assert.deepEqual(Object.keys(absent).filter((k) => typeof absent[k] === "number"), [],
    "an absent envelope must carry NO recoverable numeric field - not an extra, not a ceiling");
  // (f) the raw scalar and its engine siblings are CONSUMED - the retired `*_mock_bound` verdict cannot
  //     reappear under any name, and the facts that replaced it are published under their OWN names
  //     (`headroom`, `rig_ceiling`), never as the raw `<field>_headroom` key C1 refuses.
  for (const env of [withExtras, absent, sealMetric(0, {}), sealMetric(100, { headroom: 0.83, ceiling: 120 })])
    for (const k of Object.keys(env))
      assert.ok(!/_(mock_bound|headroom|rig_ceiling|mock_ceiling)$/.test(k),
        `no raw engine sibling may survive the seal; found ${k}`);
});

test("#3 CLASS: a MEASURED stream-sustain failure renders differently from an unmeasured one", () => {
  // The site: a measured 0 shows the number 0 with a MEASURED-FAILURE note; an unmeasured one reads n/a.
  // Publishing "the gateway was offered stream load and sustained none of it" as "not measured" would
  // flatter the gateway, so null - an absent field - is the ONLY not-measured state.
  const failed = app.metric(sealMetric(0, { zeroNote: ZERO_MEASURED_FAIL }), String);
  const unmeasured = app.metric(sealMetric(null), String);
  assert.equal(failed.text, "0");
  assert.equal(failed.na, false);
  assert.match(failed.note, /MEASURED FAILURE/);
  assert.equal(unmeasured.text, "not measured");
  assert.equal(unmeasured.na, true);
  assert.match(unmeasured.note, /not measured/);
  // AND THERE IS NO THIRD STATE. This used to assert that a rig-limited 1300 was one - a suppressed
  // envelope reading n/a with a "rig-limited" note, "never conflated with either" of the two above. It
  // was a third state only because the seal invented it: the harness measured 1300 streams and the board
  // showed a blank. A near-ceiling reading is now simply a measurement, so it renders as its number and
  // the only two no-number states left are the honest ones - a measured 0 and an absence.
  const atCeiling = app.metric(sealMetric(1300, { headroom: 0.98, ceiling: 1326 }), String);
  assert.equal(atCeiling.na, false, "a near-ceiling stream count is a number, not a state");
  assert.equal(atCeiling.text, "1300");
  assert.equal(atCeiling.env.headroom, 0.98, "how close it came travels with it, for the reader to weigh");
});

test("a measured FAILURE renders red with its counts, never as the n/a an untested cell gets", () => {
  // one-api's c=1 leg after the restart bug: 0 ok, 14201 fail. The owner's rule: a failure is marked
  // in the cell - digits prove the measurement ran, red says the gateway failed it.
  const env = sealMetric(null, { absent: { reason: "not_measured",
    detail: "the gateway leg at c=1 was not clean: 0 ok, 14201 fail" } });
  const cell = app.metric(env, String);
  assert.equal(cell.text, "failed · 0/14,201", "the counts are the cell text");
  assert.equal(cell.failed, true);
  assert.match(app.metricTd(cell), /class="na failcell"/, "the td carries the red failure class");
  assert.match(app.metricTd(cell), /title="the gateway leg at c=1 was not clean/, "full evidence on hover");
  // The streaming shape of the same fact.
  const frames = app.metric(sealMetric(null, { absent: { reason: "not_measured",
    detail: "no stream frame arrived from the gateway, so there is nothing to difference" } }), String);
  assert.equal(frames.text, "failed · 0 frames");
  assert.equal(frames.failed, true);
  // A leg that was merely NOISY (some ok, some fail) is not a total failure and stays n/a.
  const noisy = app.metric(sealMetric(null, { absent: { reason: "not_measured",
    detail: "the gateway leg at c=1 was not clean: 497 ok, 3 fail" } }), String);
  assert.equal(noisy.text, "not measured");
  assert.ok(!noisy.failed);
  // The engine emits the IDENTICAL sentence for the DIRECT-TO-MOCK leg - that is OUR reference rig
  // failing, not the gateway. It must NOT get the red gateway-blaming render: plain n/a, with the
  // full detail on the tooltip so the rig-side cause is still visible.
  const directDetail = "the direct-to-mock leg at c=1 was not clean: 0 ok, 8123 fail";
  const direct = app.metric(sealMetric(null, { absent: { reason: "not_measured",
    detail: directDetail } }), String);
  // A RIG-SIDE FAILURE IS NOT THE GATEWAY'S. This rendered as a plain "n/a" in the gateway's own
  // column, which charges our reference leg to the subject. It now reads "unconfirmed".
  assert.equal(direct.text, "unconfirmed", "a rig-side failure must not read as a gateway hole");
  assert.equal(direct.rigSide, true);
  assert.ok(!direct.failed, "the failed flag blames the gateway; the direct leg must never carry it");
  assert.equal(direct.note, directDetail, "the full detail stays on the tooltip");
  // The streaming shape of the same distinction: no frame from the mock directly is a rig problem.
  const directFrames = app.metric(sealMetric(null, { absent: { reason: "not_measured",
    detail: "no stream frame arrived from the mock directly, so there is nothing to difference" } }), String);
  assert.equal(directFrames.text, "unconfirmed");
  assert.ok(!directFrames.failed);
});

test("a measured zero's meaning is VISIBLE on the table cell, not only in a hover tooltip", () => {
  // The 2026-07-28 board rendered rps_sustained_20ms=0 as a bare "0" beside a real maximum, which
  // reads as "this gateway does nothing". The td writer now prints the short reason under the number.
  const zeroCeiling = app.metric(sealMetric(0, { zeroNote: ZERO_NO_CEILING }), String);
  assert.match(app.metricTd(zeroCeiling), /class=""/);
  assert.match(app.metricTd(zeroCeiling), /<span class="zero-why">no load held the gate<\/span>/);
  const zeroFail = app.metric(sealMetric(0, { zeroNote: ZERO_MEASURED_FAIL }), String);
  assert.match(app.metricTd(zeroFail), /<span class="zero-why">measured failure<\/span>/);
  // A plain zero on a field with NO zero-note vocabulary (memory growth 0.0, an added gap of 0) renders
  // bare, exactly as before. ZERO_WHY is keyed by the envelope's own note token, and the two tokens mean
  // "no tested load held the qualifying gates" (RPS ceilings) and "offered stream load, sustained none"
  // (streaming counts) - neither is a true statement about a memory growth rate, so an untagged zero must
  // arrive at the td with no note for the writer to render. seal.mjs's own header states the same scope.
  const plainZero = app.metric(sealMetric(0, {}), String);
  assert.ok(!app.metricTd(plainZero).includes("zero-why"), "an unannotated zero stays a bare 0");
  // And n/a cells are untouched by the writer refactor.
  assert.match(app.metricTd(app.metric(sealMetric(null, {}), String)), /class="na"/);
});

test("#2b CLASS: a below-resolution difference renders as ≈0, ranks as 0, and is never a bare n/a", () => {
  // The engine publishes a difference that came out at or under the rig's resolution as
  // {reason:"below_resolution", detail:"..."} in the cell's absences map. That is the BEST result the
  // comparison can express; rendering it as n/a turned a win into a hole (APISIX published an
  // added-gap p99 with no p50 - impossible for one distribution - on the 2026-07-28 board).
  const absent = { reason: "below_resolution", detail: "the gateway's own inter-frame gap at this percentile (20073us) came in under the mock's (21070us)" };
  const env = sealMetric(null, { absent });
  assert.equal(env.value, null, "the envelope still carries NO number - 0 was not measured");
  assert.equal(env.reason, "below_resolution", "the engine's reason survives the seal");
  assert.match(env.detail, /came in under the mock's/, "the engine's prose survives the seal");
  const cell = app.metric(env, String);
  assert.equal(cell.na, false, "below-resolution is a display state, not a hole");
  assert.equal(cell.text, "≈0", "it reads as approximately zero, never as n/a");
  assert.equal(cell.v, 0, "it ranks as 0 - equal-best on every lower-is-better sort");
  assert.match(cell.note, /came in under the mock's/, "the tooltip carries the engine's own evidence");
  // mval agrees with metric(): the compare table and deltas rank it as 0, not as missing.
  assert.equal(app.mval(env), 0);
  // The oracle derives the same display from the RAW artifact, or R1 would block every deploy. Its
  // signature is (raw, absentReason): the flag and the gated/paced pair it also used to take were the
  // retired suppression's inputs and are gone from both sides of the rule.
  assert.equal(oracleExpected(null, "below_resolution"), 0);
  assert.equal(oracleExpected(null, "not_measured"), null,
    "every OTHER absence still displays as nothing");
  // And every other engine absence reason survives the seal instead of flattening to not_measured.
  for (const reason of ["rig_limited", "untestable", "search_exhausted", "harness_error"]) {
    const e = sealMetric(null, { absent: { reason, detail: "why" } });
    assert.equal(e.reason, reason, `the seal must carry ${reason}, not flatten it`);
    const c = app.metric(e, String);
    assert.equal(c.na, true, `${reason} still reads n/a`);
    assert.equal(c.note, "why", "the engine's detail is the tooltip when present");
  }
  // Without an absences entry the seal behaves exactly as before (bit-for-bit envelope shape).
  assert.deepEqual(sealMetric(null, {}), { value: null, certified: false, suppressed: false, reason: "not_measured" });
});

// ---- #25: the snapshot ingest path (task #65) had NO test at all ----------------------------------
// certifyRepo(root, mutate?): apply an optional edit to buildStreamMemRepo's matrix artifact on the way
// in, so the tests below assert on the SELECTION they exist to test.
//
// It used to also stamp `*_mock_bound: false` into every cell, because without the flag a gated RPS
// sealed to "unverifiable" and published nothing - so a test about which snapshot wins would have failed
// on the honesty gate instead. There is no gate to satisfy now (a present number is published, annotated
// with the comparison's facts), so the stamping is gone and the helper does only what its name says.
function certifyRepo(root, mutate) {
  const mpath = join(root, "results", "matrix", "sgw.json");
  const m = JSON.parse(readFileSync(mpath, "utf8"));
  if (mutate) mutate(m);
  writeFileSync(mpath, JSON.stringify(m));
  return root;
}
function writeSnapshot(root, key, { measuredAt, matrix, files }) {
  mkdirSync(join(root, "results", "snapshots"), { recursive: true });
  writeFileSync(join(root, "results", "snapshots", `result_${key}_${measuredAt.replace(/[:.]/g, "-")}.json`),
    JSON.stringify({ gateway: key, measured_at: measuredAt, matrix, config: files ? { files } : undefined }));
}
test("the ootb_config pointer is UNTRUSTED: a path that escapes results/ is refused, never read", () => {
  // The pointer arrives inside a producer-written results JSON and its CONTENTS are published verbatim
  // onto a public page. A `..` in it therefore reads an arbitrary local file straight onto the board.
  // Nothing malicious is required for this to bite - a producer bug writing an absolute or relative
  // path would exfiltrate a file rather than fail - so the shape the harness actually writes
  // (config/<key>.txt) is allowlisted and anything else is refused LOUDLY, not silently ignored.
  for (const bad of ["../../lib/harness.sh", "/etc/passwd", "config/../../README.md",
    "config/sub/dir.txt", "notconfig/x.txt", "config/x.yaml"]) {
    const root = buildStreamMemRepo();
    const mpath = join(root, "results", "matrix", "sgw.json");
    const m = JSON.parse(readFileSync(mpath, "utf8"));
    m.ootb_config = bad;
    writeFileSync(mpath, JSON.stringify(m));
    assert.throws(() => genInto(root), (e) => {
      assert.match(e.message, /is not a results-relative config artifact/);
      return true;
    }, `a pointer of ${JSON.stringify(bad)} must be refused`);
  }
  // The legitimate shape still reads.
  {
    const root = buildStreamMemRepo();
    const mpath = join(root, "results", "matrix", "sgw.json");
    const m = JSON.parse(readFileSync(mpath, "utf8"));
    m.ootb_config = "config/sgw.txt";
    writeFileSync(mpath, JSON.stringify(m));
    mkdirSync(join(root, "results", "config"), { recursive: true });
    writeFileSync(join(root, "results", "config", "sgw.txt"), "port: 8080\n");
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.equal(g.ootb_config, "port: 8080\n", "the allowlisted pointer must still be read");
  }
});

test("a DEGRADED-MODE snapshot must never become the board's source just by being newer", () => {
  // A local verify-local run with KEEP_ARTIFACTS=1 leaves its snapshot in results/snapshots/; without
  // this guard, recency alone would let that probe-only snapshot (1 cell, no perf, no streaming, no
  // memory, no best_cell) silently shadow a complete field run.
  const iso = (hAgo) => new Date(Date.now() - hAgo * 3600000).toISOString();
  const probeOnly = (at) => ({ gateway: "sgw", build: "local", matrix_version: 2, served: true,
    measured_at: at, cell_perf_sweep: false, cell_stream: false, cell_memory: false,
    upstreams: { openai: { configurable: true, served: true, cells: { openai: { served: true } } } } });
  // (a) RED: the degraded snapshot is NEWER than a full run on disk. Refuse, loudly, naming the file.
  {
    const root = buildStreamMemRepo();     // its results/matrix/sgw.json is a FULL run (all phases on)
    writeSnapshot(root, "sgw", { measuredAt: iso(0.5), matrix: probeOnly(iso(0.5)) });
    assert.throws(() => genInto(root), (e) => {
      assert.match(e.message, /DEGRADED-MODE snapshot/);
      assert.match(e.message, /cell_perf_sweep=false/, "the message must name WHICH phases were off");
      assert.match(e.message, /results\/snapshots/, "the message must say where to remove it from");
      return true;
    }, "a probe-only snapshot must never silently replace a complete run");
  }
  // (b) GREEN: a newer FULL run supersedes normally, even when it found FEWER served cells. A re-run
  //     that finds less IS the new truth; this guard is about the run's MODE, never about its numbers.
  {
    const root = buildStreamMemRepo();
    const fullButEmptier = { gateway: "sgw", build: "field", matrix_version: 2, served: true,
      measured_at: iso(0.5), cell_perf_sweep: true, cell_stream: true, cell_memory: true,
      upstreams: { openai: { configurable: true, served: true, cells: { openai: { served: false } } } } };
    writeSnapshot(root, "sgw", { measuredAt: iso(0.5), matrix: fullButEmptier });
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.equal(g.matrix_from_snapshot, true, "a newer FULL run must still win");
    assert.equal(g.best_cell, undefined, "and its honest zero-served result must publish as such");
  }
  // (c) RED: a LONE degraded snapshot - nothing on disk for it to shadow - must be refused too.
  //     This case used to publish, on the reasoning that "absence of a better run is not a reason to
  //     publish nothing". But the board does not publish "nothing" for a gateway with no artifact; it
  //     publishes n/a, which is the true statement. What it published instead was a probe-only run
  //     under that gateway's name, as a board RESULT, with no marker of any kind - so the one gateway
  //     the reader has no other information about is the one shown a smoke run. Shadowing was never the
  //     defect; becoming the board's source is, and a lone smoke snapshot does that most cheaply.
  {
    const root = buildStreamMemRepo();
    rmSync(join(root, "results", "matrix", "sgw.json"), { force: true });
    writeSnapshot(root, "sgw", { measuredAt: iso(0.5), matrix: probeOnly(iso(0.5)) });
    assert.throws(() => genInto(root), (e) => {
      assert.match(e.message, /DEGRADED-MODE snapshot/);
      assert.match(e.message, /ONLY matrix artifact/, "the message must say nothing was shadowed, so the fix differs");
      return true;
    }, "a smoke run must not become a gateway's board result just because it is the only file there");
  }
});

test("#25 CLASS: the snapshot ingest path - newest wins, RECENCY beats existence, inline config, null-safe", () => {
  const iso = (hAgo) => new Date(Date.now() - hAgo * 3600000).toISOString();
  // (a) NEWEST snapshot wins over an older one, and its matrix supersedes the per-suite file.
  {
    const root = buildStreamMemRepo();
    const mk = (rps, at) => ({ gateway: "sgw", build: "snap", matrix_version: 2, served: true, measured_at: at,
      upstreams: { openai: { configurable: true, served: true, cells: { openai: { served: true, perf: {
        added_latency_p50_us: 1, added_latency_p99_us: 2,
        frontier: rawFrontier({ 10: rps, none: rps + 1 }) } } } } } });
    writeSnapshot(root, "sgw", { measuredAt: iso(2), matrix: mk(11111, iso(2)) });
    writeSnapshot(root, "sgw", { measuredAt: iso(0.5), matrix: mk(22222, iso(0.5)) });
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.equal(app.frontierCell(g.best_cell, 10).v, 22222, "the NEWEST snapshot must win");
    assert.equal(g.matrix_from_snapshot, true);
  }
  // (b) #5 RED-before: an OLDER snapshot must NOT shadow a NEWER results/matrix/<gw>.json. Before the
  //     fix the snapshot won by EXISTENCE and this returns the stale 33333.
  {
    const root = certifyRepo(buildStreamMemRepo());   // matrix stamped 1h ago, rps_sustained_20ms 45000
    writeSnapshot(root, "sgw", { measuredAt: iso(72), matrix: { gateway: "sgw", build: "old", matrix_version: 2,
      served: true, measured_at: iso(72), upstreams: { openai: { configurable: true, served: true, cells: { openai: {
        served: true, perf: { added_latency_p50_us: 9, added_latency_p99_us: 9,
          frontier: rawFrontier({ 10: 33333, none: 33334 }) } } } } } } });
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.equal(app.frontierCell(g.best_cell, 10).v, 45000,
      "a 3-day-old snapshot must NOT shadow the newer matrix on disk (recency, not existence)");
    assert.ok(!g.matrix_from_snapshot);
  }
  // (c) inline config.files replaces the config/<gw>.txt sidecar read.
  {
    const root = buildStreamMemRepo();
    writeSnapshot(root, "sgw", { measuredAt: iso(0.1), matrix: null, files: { "sgw.yaml": "listen: 8080\n" } });
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.match(g.ootb_config, /listen: 8080/);
  }
  // (d) NULL-SAFE: a snapshot with no matrix / no config / a corrupt sibling degrades to the disk path.
  {
    const root = certifyRepo(buildStreamMemRepo());
    writeSnapshot(root, "sgw", { measuredAt: iso(0.1), matrix: null });
    mkdirSync(join(root, "results", "snapshots"), { recursive: true });
    writeFileSync(join(root, "results", "snapshots", "result_sgw_corrupt.json"), "{not json");
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.equal(app.frontierCell(g.best_cell, 10).v, 45000, "a matrix-less snapshot must not blank the row");
    assert.ok(g.best_cell.source.kind === "matrix");
  }
});

// ---- #21 CLASS: RIG PROVENANCE - the measurement instrument must describe itself ------------------
// The mock is fetched from a MOVING GitHub release tag ("rig"), rebuilt by CI on every mock/ change.
// So two runs of a byte-identical harness can produce DIFFERENT cell verdicts
// purely because the instrument changed underneath them - which is exactly what happened here: bcf9912
// tightened the mock's request_shape_ok so bedrock/cohere began rejecting a raw OpenAI body forwarded
// verbatim, the release assets were rebuilt 2026-07-24T19:03Z, and served-cell counts fell board-wide
// for a reason that had nothing to do with any gateway. Nothing in either run's output recorded which
// binaries produced it, so establishing that took a long investigation. The snapshot now carries a
// mock+ugen sha256 (the authoritative identity) plus the release asset updated_at, and gen-data
// projects it, so a future cross-run comparison can tell at a glance whether the instrument moved.
test("#21 CLASS: rig provenance travels from the snapshot into the bundle, and is NULL-SAFE without it", () => {
  const iso = (hAgo) => new Date(Date.now() - hAgo * 3600000).toISOString();
  const RIG = {
    arch: "arm64", release_url: "https://example.invalid/releases/download/rig",
    mock: { origin: "release", sha256: "a".repeat(64), asset_updated_at: "2026-07-24T19:03:00Z" },
    ugen: { origin: "cached", sha256: "b".repeat(64), asset_updated_at: "2026-07-24T19:03:00Z" },
  };
  const mkMatrix = (at, rig) => ({
    gateway: "sgw", build: "snap", matrix_version: 2, served: true, measured_at: at, rig,
    upstreams: { openai: { configurable: true, served: true, cells: { openai: { served: true, perf: {
      added_latency_p50_us: 1, added_latency_p99_us: 2,
      frontier: rawFrontier({ 10: 100, none: 101 }) } } } } },
  });
  // (a) present: the block reaches the bundle intact, so the instrument is recoverable from the board.
  {
    const root = buildStreamMemRepo();
    writeSnapshot(root, "sgw", { measuredAt: iso(0.5), matrix: mkMatrix(iso(0.5), RIG) });
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.deepEqual(g.rig, RIG, "the rig block must travel verbatim into the bundle");
    assert.equal(g.rig.mock.sha256.length, 64, "the mock digest is the authoritative instrument identity");
  }
  // (b) ABSENT (every snapshot written before this existed): null, never a fabricated digest and never
  //     a crash. This is the null-safety requirement for older snapshots.
  {
    const root = buildStreamMemRepo();
    writeSnapshot(root, "sgw", { measuredAt: iso(0.5), matrix: mkMatrix(iso(0.5), undefined) });
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.equal(g.rig, null, "a pre-rig snapshot must project null, not a placeholder");
  }
  // (c) a PARTIAL block (no network for the asset stamp, or a locally-built binary) still travels: an
  //     unknown field is null, and the sha256 alone is enough to prove the instrument changed.
  {
    const root = buildStreamMemRepo();
    const partial = { arch: "arm64", release_url: null,
      mock: { origin: "source-build", sha256: "c".repeat(64), asset_updated_at: null },
      ugen: { origin: "source-build", sha256: "d".repeat(64), asset_updated_at: null } };
    writeSnapshot(root, "sgw", { measuredAt: iso(0.5), matrix: mkMatrix(iso(0.5), partial) });
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.equal(g.rig.mock.asset_updated_at, null, "an unfetchable asset stamp is null, never invented");
    assert.equal(g.rig.mock.origin, "source-build", "the ORIGIN must disclose a non-release binary");
  }
  // (d) the LIVE bundle: every gateway carries the key (null until the next field run writes one), so
  //     the board never has to guess whether the field is missing or the value is.
  for (const g of data.gateways)
    assert.ok("rig" in g, `${g.key} must carry a rig key (null-safe) so provenance is explicit`);
});

test("#21 CLASS: the footer rig stamp shows one digest, flags DISAGREEING rows, and stays silent without one", () => {
  const withRig = (shas) => ({ gateways: shas.map((sha, i) => ({ key: `g${i}`, rig: sha ? { mock: { sha256: sha } } : null })) });
  const run = (d) => { const prev = app.state.data; app.state.data = d; try { return app.rigStamp(); } finally { app.state.data = prev; } };
  // no gateway records an instrument -> NOTHING is claimed (never "unknown" dressed up as a version).
  assert.equal(run(withRig([null, null])), "", "an unrecorded instrument must render nothing at all");
  assert.equal(run({ gateways: [] }), "");
  // a short/garbage digest is not an identity and must not be shown as one.
  assert.equal(run(withRig(["abc"])), "", "a truncated digest is not an instrument identity");
  // one instrument across the board -> the short digest.
  assert.equal(run(withRig(["a".repeat(64), "a".repeat(64)])), `Rig (mock): ${"a".repeat(12)}`);
  // THE CASE THAT MATTERS: rows measured by DIFFERENT instruments must say so loudly, because that is
  // precisely the condition (a mid-week rig rebuild) that made this run's verdicts incomparable.
  const mixed = run(withRig(["a".repeat(64), "b".repeat(64), "b".repeat(64)]));
  assert.match(mixed, /2 DIFFERENT builds across rows/);
  assert.ok(mixed.includes("aaaaaaaaaaaa (1)") && mixed.includes("bbbbbbbbbbbb (2)"),
    `the mixed stamp must attribute each instrument to its row count; got: ${mixed}`);
});

/* ---- #26: THE OPERATING CONCURRENCY - the Performance-tab payload of task #65 --------------------
   WHERE IT LIVES CHANGED. It used to ride INSIDE the sealed throughput envelope (`conc_at` / `concurrency`
   on `rps_sustained_20ms`, captured by gen-data from the raw cell's `conc_at_sustained`), and `concAt()`
   read it out. Those metrics are deleted. The concurrency is now a FIELD ON THE READING - each frontier
   reading names the concurrency its winning rate was observed at - which is a better home for it: the
   number belongs to the reading, and each of the six readings has its own.
   `concAt()` survives for the metrics that still carry it inside their envelope (the stream ceiling), so it
   is still exercised here; the render half is asserted against the frontier reading that drives it now. */
test("#26 CLASS: the operating concurrency travels with the reading and drives the '@ N conc' render", () => {
  // (a) the accessor, on an envelope that still carries a rung inside it: conc_at WINS over the legacy
  // *_concurrency; either alone works; neither -> null.
  assert.equal(app.concAt(sealMetric(9, { extras: { conc_at: 512, concurrency: 64 } })), 512);
  assert.equal(app.concAt(sealMetric(9, { extras: { concurrency: 64 } })), 64);
  assert.equal(app.concAt(sealMetric(9)), null, "no rung recorded -> null, never fabricated");
  assert.equal(app.concAt(null), null);
  assert.equal(app.concAt(42), null, "a bare scalar is not an envelope");
  // (b) gen-data carries each READING's own concurrency through the seal, off the raw frontier.
  const root = certifyRepo(buildStreamMemRepo(), (m) => {
    for (const cells of [m.cells, m.upstreams.openai.cells]) {
      cells.openai.perf.frontier = [
        { p99_bound_us: 1000, rps: 40000, concurrency: 192, p99_us: 400, first_disqualified_conc: 384, lower_bound: false },
        { p99_bound_us: 10000, rps: 45000, concurrency: 384, p99_us: 4000, first_disqualified_conc: 768, lower_bound: false },
        { p99_bound_us: null, rps: 50000, concurrency: 768, p99_us: 40000, first_disqualified_conc: null, lower_bound: false },
      ];
    }
  });
  const g = genInto(root).gateways.find((x) => x.key === "sgw");
  assert.equal(app.frontierAt(g.best_cell.frontier, 10).concurrency, 384);
  assert.equal(app.frontierAt(g.best_cell.frontier, 1).concurrency, 192);
  // (c) the RENDER: the Performance cell shows "N @ Y conc" with the reading's own evidence on the tooltip,
  // and it is the SELECTED bound's concurrency - a different bound is a different rung.
  const st = { ...app.newState(), mode: "peak", data: { gateways: [g] } };
  const at10 = app.frontierChooserCell(g, st);
  assert.match(at10.text, /@ 384 conc/);
  assert.match(at10.note, /Observed with 384 concurrent requests/);
  const at1 = app.frontierChooserCell(g, { ...st, bound: 1 });
  assert.match(at1.text, /@ 192 conc/, "the concurrency follows the bound, because the rung does");
  // (d) the NULL-conc render: no concurrency recorded -> the bare number, NEVER "@ null conc".
  const noConc = structuredClone(g);
  for (const r of noConc.best_cell.frontier) r.concurrency = null;
  const bare = app.frontierChooserCell(noConc, st);
  assert.ok(!/conc/.test(bare.text), `a reading with no recorded rung must render the bare number; got ${bare.text}`);
});

// ---- #27: the producer's fabricated-0 -> honest-NULL change; the site must be NULL-SAFE ------------
test("#27 CLASS: every RSS field is NULL-SAFE - a null RSS renders 'not measured', never 0", () => {
  const root = certifyRepo(buildStreamMemRepo(), (m) => {
  // The producer emits NULL (never a fabricated 0) for an RSS it could not obtain: a failed fixed load or
  // a payload mismatch nulls the steady state + peak/hwm, and the disclosure rides in memory.protocol as
  // text. On this path the plateau VERDICT is withheld as null too: `false` would assert that we watched
  // this gateway fail to settle, when what happened is that we could not watch it at all.
  const mem = m.upstreams.openai.cells.openai.memory;
  mem.steady_state_rss_mib = null;
  mem.peak_rss_mib = null;
  mem.peak_rss_hwm_mib = null;
  mem.plateaued = null;
  mem.time_to_plateau_s = null;
  mem.protocol = "per-cell, own cold-started process: 30s COLD idle -> fixed load run to plateau -> 45s recovery; " +
    "steady state/peak/hwm withheld: declared load_recipe.payload_bytes=4096 but only 512B were actually delivered";
  mem.idle_window_s = 30;
  mem.recovery_window_s = 45;
  });
  const g = genInto(root).gateways.find((x) => x.key === "sgw");
  const mem = g.matrix.upstreams.openai.cells.openai.memory;
  // (a) SEALED, not bare: a null RSS is an explicit not-measured envelope, and the NEW producer fields
  //     (peak_rss_hwm_mib / growth rate / time to plateau) are sealed BY DISCOVERY plus the named memory
  //     vocabulary - no whitelist to lag the producer (#11).
  for (const k of ["idle_rss_mib", "steady_state_rss_mib", "recovered_rss_mib", "peak_rss_mib",
    "peak_rss_hwm_mib", "growth_rate_mib_per_min", "time_to_plateau_s"])
    assert.ok(app.isEnvelope(mem[k]), `${k} must be a sealed envelope, not a bare scalar`);
  assert.equal(app.mval(mem.steady_state_rss_mib), null);
  assert.equal(mem.steady_state_rss_mib.reason, "not_measured");
  assert.equal(app.mval(mem.idle_rss_mib), 120.5);
  assert.equal(mem.plateaued, null, "an unmeasurable window WITHHOLDS the verdict, it does not assert false");
  // (b) the RENDER suppresses it: n/a, never a 0 bar or a 0 cell.
  const bundle = { gateways: [g] };
  const st = { data: bundle, mode: "same", sameDialect: "openai", view: "memory" };
  const cell = app.memCell(g, "steady_state_rss_mib", String, st);
  assert.equal(cell.na, true);
  assert.equal(cell.text, "not measured");
  assert.equal(cell.v, null);
  // (c) #14: the window durations RENDER from the data, not from a hard-coded "60 s" - and they now have
  //     to be found on the CELL, which is where the producer writes them.
  // `steady` is the STEADINESS window (how long the RSS had to hold still before the plateau was
  // believed). It rides in load_recipe.plateau_window_s; this fixture predates it, so it reads null and
  // the caption states the settling time without claiming a confirmation length it does not know.
  assert.deepEqual(app.memWindows(mem), { idle: 30, recovery: 45, steady: null });
  assert.deepEqual(app.boardMemWindows(bundle), { idle: 30, recovery: 45, steady: null },
    "the board's window labels must read the PER-CELL windows, not fall back to the 60 s default");
  const cap = app.captionText(app.memoryCaption(bundle, st));
  assert.ok(cap.includes("45 s"), `memory caption must render the run's own windows; got: ${cap}`);
  assert.ok(!/60 s/.test(cap), "the caption must not hard-code the default window");
  // (d) the DISCLOSURE the producer rides in memory.protocol must reach the board, not be silently carried.
  assert.match(mem.protocol, /withheld/);
  assert.match(app.memCellTip(app.chosenMemory(g, st)), /withheld/,
    "the memory protocol disclosure must be SURFACED in the Tested-on tooltip, not silently carried");
  // (e) the C1/C2 invariants still hold on a null-RSS bundle (no bare scalar, nothing recoverable).
  assert.deepEqual(checkConsistency(bundle, app, SYNTH).errors, []);
});

test("#27: a fallback stream record with NULL counts seals to not-measured, never a fabricated 0", () => {
  // The producer can now abort before any rung: streams_sustained / _fps / cpu_fps are all nullable.
  const rec = streamRec({ streams_sustained: null, streams_sustained_fps: null, cpu_fps: null });
  for (const k of ["streams_sustained", "streams_sustained_fps", "cpu_fps"]) {
    assert.equal(app.mval(rec[k]), null);
    assert.equal(rec[k].suppressed, false, `${k}: an ABSENT reading is not-measured, never "suppressed"`);
    assert.equal(rec[k].reason, "not_measured");
  }
  const g = { key: "n", display: "n", lang: "Rust", streaming: rec };
  assert.equal(app.streamCell(g, "streams_sustained", String).text, "not measured");
});

/* ---- AUDIT GROUP B: the lints + the coverage oracle now have RED-BEFORE proofs ------------------- */

test("#20 RED: the C3 sweep-key lint FIRES on a leaked key (and its coverage tag means 'the scanner ran')", () => {
  const region = { enter: /const SWEEP_CAPTION\s*=/, exit: /^\};\s*$/m };
  // RED: a sweep KEY used as a user-facing caption literal outside the caption table.
  const bad = `const SWEEP_CAPTION = {\n  "6x6-diagonal": () => "x",\n};\nconst note = "measured on the 6x6-diagonal";\nconst t = label + "6x6-diagonal";\n`;
  const r = checkMod.lintSweepKeys(bad, "fake.js", region);
  assert.equal(r.errors.length, 1, `the lint must FIRE on a leaked caption literal; got ${JSON.stringify(r.errors)}`);
  assert.match(r.errors[0], /sweep-key token leaked/);
  // GREEN: the same key inside the caption table, and as provenance DATA, are both legitimate.
  const good = `const SWEEP_CAPTION = {\n  "6x6-diagonal": () => "x",\n};\nrec.source = { kind: "matrix", sweep: "6x6-diagonal" };\n`;
  assert.deepEqual(checkMod.lintSweepKeys(good, "fake.js", region).errors, []);
  // COVERAGE means the SCANNER ran and found its region - NOT that an exemption/error path was hit.
  assert.equal(checkMod.lintSweepKeys(good, "fake.js", region).scanned, true);
  assert.equal(checkMod.lintSweepKeys("nothing here\n", "fake.js", region).scanned, false,
    "a lint that never found its region must NOT report itself covered");
});

test("#15/#20 RED: the C5 accessor-routing lint FIRES on the access style the codebase actually uses", () => {
  // RED: bind the envelope to a local, then read .value on a LATER line than the binding, which a
  // same-line, `.field.value`-only lint would not catch.
  const bad = [
    "function draw(p, key) {",
    "  const env = p[key];",
    "  if (!isEnvelope(env)) return;",
    "  out.push({ rps: env.value });",
    "}",
  ].join("\n");
  const r = checkMod.lintAccessorRouting(bad, "fake.js", "js");
  assert.equal(r.errors.length, 1, `the lint must FIRE on a bound-then-deref read; got ${JSON.stringify(r.errors)}`);
  assert.match(r.errors[0], /envelope-typed `env`/);
  // RED (the direct form the old lint DID cover) still fires.
  // The sample field must be one seal.mjs still calls a metric: the lint DISCOVERS the vocabulary through
  // `isMetricField` rather than enumerating it, so a retired field name is (correctly) not policed.
  assert.ok(checkMod.lintAccessorRouting("const x = p.added_latency_p99_us.value;\n", "fake.js", "js").errors.length >= 1);
  // GREEN: routed through the accessor.
  const good = [
    "function draw(p, key) {",
    "  const env = p[key];",
    "  const v = mval(env);",
    "  if (v == null) return;",
    "  out.push({ rps: v });",
    "}",
  ].join("\n");
  assert.deepEqual(checkMod.lintAccessorRouting(good, "fake.js", "js").errors, []);
  // GREEN: the accessor's OWN body is the one legal place to read .value.
  assert.deepEqual(checkMod.lintAccessorRouting(
    "function metric(env, fmt) {\n  if (!isEnvelope(env) || env.value == null) return null;\n  return fmt(env.value);\n}\n",
    "fake.js", "js").errors, []);
  // The lint stays LANGUAGE-GENERAL even though app.js is the only reader today: a second reader in
  // another language is exactly what charts.py was, and the rule that caught it should survive it.
  const badPy = 'def draw(bc):\n    env = bc.get("added_latency_p99_us")\n    if _is_env(env):\n        return env.get("value")\n';
  assert.ok(checkMod.lintAccessorRouting(badPy, "fake.py", "py").errors.length >= 1,
    "the routing lint must cover non-app.js readers");
  // AND the repo's own reader is CLEAN.
  assert.deepEqual(checkMod.lintAccessorRouting(readFileSync(join(HERE, "app.js"), "utf8"), "app.js", "js").errors, []);
});

/* #2/#22 RETIRED WITH THE PIPELINE THEY GUARDED.
   These asserted that charts.py disclosed provenance per lane, and that its caption vocabulary matched
   app.js's. Both existed because the board had a SECOND renderer in another language publishing the
   same numbers. There is one renderer now, so there is no second vocabulary to drift and no lane to
   leave undisclosed - and a test that reads a deleted file fails for the wrong reason. */
test("#21: C6 fires on an INJECTED frontier inversion - the assertion cannot silently pass when a row is absent", () => {
  /* Drive the invariant DIRECTLY on an injected cell rather than a named gateway's live data, so the
     assertion cannot skip vacuously when that gateway's file is missing or its inversion resolves.
     WHAT IS INJECTED CHANGED WITH THE METRIC. It used to be a `sustained@20ms > max_proxy` pair, re-derived
     by a local copy of the flag so the test proved the rule independently of the checker. The pair is gone
     and the property is now internal to ONE curve: a looser bound cannot read lower than a tighter one. The
     local re-derivation stays, for the same reason it existed - the test must be able to disagree with the
     checker rather than only echo it. */
  const rate = (r) => (typeof r.rps === "number" ? r.rps : null);
  const inverts = (readings) => {
    let prev = null;
    for (const r of readings) {
      const v = rate(r);
      if (v == null) continue;
      if (prev != null && v < prev) return true;
      prev = v;
    }
    return false;
  };
  const inverted = [rd(1, 1000), rd(5, 900), rd(null, 1200)];
  const ok = [rd(1, 900), rd(5, 1000), rd(null, 1200)];
  const withHole = [rd(1, null), rd(5, 500), rd(null, 1200)];
  assert.equal(inverts(inverted), true, "an injected inversion MUST be flagged");
  assert.equal(inverts(ok), false);
  assert.equal(inverts(withHole), false, "an absent reading is a hole, not an inversion");
  // and the REAL checker agrees on the same injected cell, through its own code path.
  assert.equal(c6Inversions("gw", c6Matrix(inverted)).violations.length, 1, "the checker must block an inverted frontier");
  assert.equal(c6Inversions("gw", c6Matrix(ok)).violations.length, 0);
  const { errors, warnings } = checkConsistency(data, app);
  const c6all = [...errors, ...warnings].filter((e) => e.includes("the frontier inverts"));
  assert.ok(c6all.every((e) => /reads \d+ but the tighter/.test(e)),
    "every C6 inversion message must name both rates it found");
});

// ---- #1 CLASS: "Tested on" describes the record the row ACTUALLY displays, in EVERY lane -----------
test("#1 CLASS: the Tested-on pill renders its OWN lane's provenance in every chooser mode", () => {
  // One gateway: matrix perf on the openai diagonal, but STREAMING from the legacy stream suite. The
  // streaming pill must disclose its OWN lane's provenance, never the perf cell's matrix provenance.
  const g = {
    key: "tw", display: "TW", lang: "Rust",
    best_cell: bcCell({ dialect: "openai" }),
    streaming: streamRec({ dialect: "anthropic", kind: "stream-fallback" }),
    matrix: mkMatrix({ openai: { openai: { served: true, perf: {} } } }),
  };
  const cols = app.COLUMN_SETS;
  const testedIn = (set) => set.find((c) => c.id === "tested");
  const st = { ...app.newState(), mode: "peak", data: { gateways: [g] } };
  const perfPill = testedIn(cols.performance).render(g, st);
  const streamPill = testedIn(cols.streaming).render(g, st);
  // The PERF pill: the matrix diagonal it was measured on - no fallback star.
  assert.match(perfPill, /OpenAI/);
  assert.ok(!perfPill.includes(" *"), "a matrix-sourced perf row must not be starred");
  // The STREAMING pill: the STREAM record's own dialect + its own honest legacy caption + a star.
  assert.match(streamPill, /Anthropic/, "the streaming pill must name the dialect the STREAM record used");
  assert.match(streamPill, /stream suite \(legacy\)/, "the streaming pill must disclose its OWN provenance");
  assert.ok(streamPill.includes(" *"), "a live-fallback streaming row must be starred");
  assert.ok(!/passthrough - 6×6 diagonal/.test(streamPill),
    "the streaming pill must NOT advertise the perf cell's matrix provenance");
  // NO RECORD -> NO PILL. In Same/Custom on a dialect the streaming record was not measured on, every
  // streaming column reads n/a, so the row must not advertise a measurement at all.
  const same = { ...st, mode: "same", sameDialect: "openai" };
  const noRec = testedIn(cols.streaming).render(g, same);
  assert.match(noRec, /n\/a/, "no streaming record for this cell -> no pill");
  assert.ok(!noRec.includes("tested-pill"), "a pill must never be painted without a record");
  assert.equal(testedIn(cols.streaming).get(g, same).na, true, "and the column sorts as not-measured");
});

test("#1 CLASS: the MEMORY tab's Tested-on cell is the SAME pill, showing the memory lane's own load_cell", () => {
  // A gateway whose PERF peak cell (openai) differs from the cell its MEMORY window ran on (anthropic).
  const g = {
    key: "mm", display: "MM", lang: "Rust",
    best_cell: bcCell({ dialect: "openai" }),
    memory_read: memRec({ load_cell: "anthropic>anthropic",
      load_recipe: { concurrency: 64, payload_bytes: 4096, duration_s: 120 },
      idle_rss_mib: 40, peak_rss_mib: 900, recovered_rss_mib: 55 }),
  };
  const st = { ...app.newState(), mode: "peak", data: { gateways: [g] } };
  const memTested = app.COLUMN_SETS.memory.find((c) => c.id === "tested");
  const perfTested = app.COLUMN_SETS.performance.find((c) => c.id === "tested");
  assert.ok(memTested, "the memory tab carries the shared Tested-on column");
  // ONE renderer, not N copies: the memory column is produced by the same colTested factory.
  assert.equal(memTested.render.toString(), perfTested.render.toString(),
    "the memory tab must reuse the SHARED tested-on renderer, not a bespoke plain-text cell");
  const pill = memTested.render(g, st);
  // Same PILL markup as every other tab.
  assert.ok(pill.includes("tested-pill"), "the memory tested-on cell renders as a pill");
  assert.match(pill, /<td class="tested"/, "and uses the shared tested-on cell class");
  // PROVENANCE HONESTY: the pill names the cell the MEMORY window ran on (load_cell), not the perf cell.
  const label = (pill.match(/<span class="tested-pill"[^>]*>([^<]*)<\/span>/) || [])[1];
  assert.equal(label, "Anthropic",
    "the memory pill's chip is the memory window's own load_cell, a single dialect for a passthrough cell");
  assert.ok(!/OpenAI/.test(pill), "the memory pill must NOT advertise the perf cell");
  // The memory record is matrix-sourced → no fallback star; the tooltip keeps the memory window's own
  // caption plus the fixed-load basis (the disclosure the bespoke cell used to carry).
  assert.ok(!pill.includes(" *"), "a matrix-sourced memory row must not be starred");
  assert.match(pill, /memory window/, "the tooltip renders the MEMORY record's own provenance stamp");
  assert.match(pill, /identical fixed load/, "the tooltip keeps the fair fixed-load basis");
  // The perf tab is unchanged by all this.
  assert.match(perfTested.render(g, st), /OpenAI/, "the perf pill still names the perf cell");
});

// ---- PER-CELL MEMORY: the memory lane joins the cell chooser (Min | Max | Same | Custom) ----------
// Memory is measured per cell, and the reader picks the cell, with two hard rules: the memory lane must
// NEVER offer Peak (selecting on throughput while reporting memory would mix the two axes), and nothing
// may be substituted across cells.

// cellMem: a sealed per-cell memory window, exactly as gen-data seals cell.memory in place.
function cellMem(o = {}) {
  const { steady_state_rss_mib = 100, idle_rss_mib = 20, recovered_rss_mib = 30,
    plateaued = true, time_to_plateau_s = 25, growth_rate_mib_per_min = 0.1, rss_series = null,
    // shape: undefined by default ON PURPOSE, so every fixture that does not opt in exercises the
    // no-shape-published path a pre-shape board takes. Opting in is how a test says "I mean a wave".
    shape = undefined } = o;
  const rec = {
    steady_state_rss_mib: seal(steady_state_rss_mib), idle_rss_mib: seal(idle_rss_mib),
    recovered_rss_mib: seal(recovered_rss_mib), time_to_plateau_s: seal(time_to_plateau_s),
    growth_rate_mib_per_min: seal(growth_rate_mib_per_min), plateaued,
  };
  if (rss_series != null) rec.rss_series = rss_series;
  if (shape !== undefined) rec.shape = seal(shape);
  return rec;
}
/* memGw: a gateway whose matrix carries a per-cell memory window on each listed cell.
   cells: { "ingress>egress": <cellMem opts | null> } : null = served, no memory window. */
function memGw(key, cells, extra = {}) {
  const upstreams = {};
  for (const [pair, mem] of Object.entries(cells)) {
    const [ingress, egress] = pair.split(">");
    upstreams[egress] = upstreams[egress] || { cells: {} };
    upstreams[egress].cells[ingress] = { served: true, perf: cellPerf({ ingress, egress }),
      ...(mem ? { memory: cellMem(mem) } : {}) };
  }
  return { key, display: key, lang: "Rust", matrix: { upstreams, measured_at: "2026-07-25T00:00:00Z" }, ...extra };
}
const memState = (gws, over = {}) => ({ ...app.newState(), view: "memory", data: { gateways: gws }, ...over });
const memCol = (id) => app.COLUMN_SETS.memory.find((c) => c.id === id);

test("memory chooser RED: Peak is not offered, not decodable, and cannot select a memory number", () => {
  // (1) the mode set itself. Peak reads best_cell, which is chosen by THROUGHPUT.
  assert.ok(!app.MEM_CHOOSER_MODES.has("peak"), "the memory lane must not offer Peak");
  assert.ok(!app.modesFor("memory").has("peak"), "modesFor(memory) must not contain Peak");
  assert.ok(app.modesFor("performance").has("peak"), "the perf lanes keep Peak (select on throughput, report throughput)");
  assert.deepEqual([...app.MEM_CHOOSER_MODES], ["min", "max", "same", "custom"]);
  // (2) a SHARED URL carrying ?mode=peak that lands on memory falls back to memory's own default,
  // not to a peak cell. That default is MIN: it shows every gateway on its own lowest steady-state
  // cell, so nobody drops out of the view a reader arrives at. Same is a like-for-like comparison
  // and a gateway that does not serve the chosen dialect correctly reads n/a there - honest, but the
  // wrong thing to land on by default, because one-api declares a single cell and vanished entirely
  // from a board whose widest dialect was anthropic.
  assert.equal(app.decodeUrl("/gateways/memory", "?mode=peak").mode, "min",
    "a ?mode=peak link opened on the memory tab must fall back to memory's default, Min");
  assert.equal(app.decodeUrl("/gateways/performance", "?mode=peak").mode, "peak", "the perf tabs still decode Peak");
  assert.equal(app.decodeUrl("/gateways/performance", "?mode=min").mode, "peak", "Min is not a perf mode");
  assert.equal(app.resolveMode("peak", "memory"), "min");
  assert.equal(app.memoryMode({ mode: "peak" }), "min", "the memory choke point can never return Peak");
  // (3) BEHAVIOURAL: a gateway whose throughput-peak cell is memory-heavy and whose identity cell is
  // light. Forcing mode:"peak" must NOT surface the heavy peak-cell number.
  const g = memGw("g", { "openai>openai": { steady_state_rss_mib: 50 }, "openai>gemini": { steady_state_rss_mib: 900 } });
  g.best_cell = bcCell({ dialect: "gemini" });   // the throughput-peak cell is the 900 MiB one
  const st = memState([g], { mode: "peak", sameDialect: "openai" });
  assert.equal(memCol("mempeak").get(g, st).text, "50.0",
    "with mode forced to peak the memory column must read the SAME-dialect cell, never the throughput-peak cell");
});

test("memory Min/Max select on memory and report memory, and disclose the size of the search", () => {
  const g = memGw("broad", {
    "openai>openai": { steady_state_rss_mib: 120 },
    "openai>gemini": { steady_state_rss_mib: 61.5 },
    "anthropic>anthropic": { steady_state_rss_mib: 300 },
  });
  const min = memState([g], { mode: "min" }), max = memState([g], { mode: "max" });
  assert.equal(memCol("mempeak").get(g, min).text, "61.5", "Min is the lowest steady-state cell");
  assert.equal(memCol("mempeak").get(g, max).text, "300.0", "Max is the highest steady-state cell");
  // The chosen record names the cell it came from: the extremum is attributable, not anonymous.
  assert.deepEqual(app.chosenMemory(g, min).path, { ingress: "openai", egress: "gemini" });
  assert.equal(app.chosenMemory(g, max).path.dialect, "anthropic");
  // The candidate count rides next to the cell: min-of-3 and min-of-1 are different-sized searches.
  const ofText = (html) => (html.match(/<span class="tested-of[^>]*>([^<]*)<\/span>/) || [])[1] || "";
  const pill = memCol("tested").render(g, min);
  assert.equal(ofText(pill), "of 3 served", "a Min/Max row must state how many cells the extremum was chosen from");
  assert.match(pill, /OpenAI→Gemini/, "and which cell it landed on");
  const narrow = memGw("narrow", { "anthropic>anthropic": { steady_state_rss_mib: 70 } });
  assert.equal(ofText(memCol("tested").render(narrow, memState([narrow], { mode: "min" }))), "of 1 served");
  // Same/Custom are like-for-like: one named cell on every row, so there is no candidate set to disclose.
  assert.equal(ofText(memCol("tested").render(g, memState([g], { mode: "same", sameDialect: "openai" }))), "",
    "Same names one cell for every row; there is no search size to state");
});

test("memory Min/Max: a cell that never went steady is not a candidate (no steady state to be an extremum of)", () => {
  const g = memGw("g", {
    "openai>openai": { steady_state_rss_mib: 200 },
    "openai>gemini": { steady_state_rss_mib: null, plateaued: false, growth_rate_mib_per_min: 14.2 },
  });
  const min = memState([g], { mode: "min" });
  assert.equal(memCol("mempeak").get(g, min).text, "200.0", "the non-plateauing cell cannot be the minimum");
  assert.equal(app.chosenMemory(g, min).mem_candidates, 1, "only cells with a steady state are candidates");
  assert.equal(app.chosenMemory(g, min).mem_cells, 2, "…out of every served cell that has a window");
  // In Custom on that exact cell, the steady state reads n/a and the GROWTH carries the finding.
  const cust = memState([g], { mode: "custom", xlateIn: "openai", xlateOut: "gemini" });
  assert.equal(memCol("mempeak").get(g, cust).na, true, "no steady state was reached, so none is published");
  const growth = memCol("memgrowth").get(g, cust);
  assert.equal(growth.text, "14.2 (leak)", "the growth rate IS the reading when no steady state was reached");
  assert.match(growth.note, /never went steady/);
});

test("memory: a gateway that reaches no steady state on ANY cell is flagged at GATEWAY level, in every mode", () => {
  const leaky = memGw("leaky", {
    "openai>openai": { steady_state_rss_mib: null, plateaued: false, growth_rate_mib_per_min: 7.5 },
    "openai>gemini": { steady_state_rss_mib: null, plateaued: false, growth_rate_mib_per_min: 12.25 },
  });
  const fine = memGw("fine", { "openai>openai": { steady_state_rss_mib: 44 } });
  assert.equal(app.neverPlateaued(leaky), true);
  assert.equal(app.neverPlateaued(fine), false);
  assert.equal(app.worstGrowth(leaky), 12.25, "the flag quantifies itself with the worst rate across cells");
  // THE NAME CELL CARRIES NO PILL, IN ANY MODE. A red tag on a gateway's NAME reads as a verdict on
  // the gateway, when what was measured is one window of one metric - a much larger claim than the
  // data supports, and permanent-looking next to the name. The verdict itself is unchanged and still
  // computed (the assertions above); what changed is that it no longer brands the row. The finding
  // reaches the reader through the Growth column and the per-cell tooltip instead.
  for (const mode of ["min", "max", "same", "custom"]) {
    const st = memState([leaky, fine], { mode });
    for (const g of [leaky, fine]) {
      assert.ok(!/never settles|no steady state/i.test(app.COLUMN_SETS.memory.find((c) => c.id === "name").render(g, st)),
        `the name cell must carry no plateau pill in ${mode} mode`);
    }
  }
  // Absence of measurement is not a verdict.
  assert.equal(app.neverPlateaued({ key: "x", display: "x" }), false, "no per-cell data means no verdict");
});

test("memory: a WITHHELD plateau verdict is not a negative one - a rig failure is never a gateway defect", () => {
  // The producer is deliberately TRI-STATE. plateaued:null is written when the cold-restarted process
  // never opened its port, when the fixed load stopped delivering, and when the trailing window held
  // fewer than the four samples the steadiness test needs. `null !== true` collapsed every one of those
  // into "never settles": a permanent, named accusation on the public board about a gateway the rig never
  // watched. On macOS, where no RSS is readable at all, that was EVERY gateway on EVERY local board.
  const unmeasured = memGw("unmeasured", {
    "openai>openai": { steady_state_rss_mib: null, plateaued: null, growth_rate_mib_per_min: null },
    "openai>gemini": { steady_state_rss_mib: null, plateaued: null, growth_rate_mib_per_min: null },
  });
  assert.equal(app.neverPlateaued(unmeasured), false,
    "a gateway whose every verdict was WITHHELD must not be labelled as having reached no steady state");
  const st = memState([unmeasured], { mode: "same" });
  assert.ok(!/never settles|no steady state/i.test(app.COLUMN_SETS.memory.find((c) => c.id === "name").render(unmeasured, st)),
    "and it must not be painted with the pill either");
  assert.ok(!app.captionText(app.memoryCaption({ gateways: [unmeasured] }, st)).match(/no steady state on any cell/),
    "nor counted in the caption's tally of gateways with no steady state");
  // MIXED: one cell judged and failing, one withheld. The gateway IS flagged - we watched it fail
  // somewhere - but the claim narrows to what was actually measured.
  const mixed = memGw("mixed", {
    "openai>openai": { steady_state_rss_mib: null, plateaued: false, growth_rate_mib_per_min: 9 },
    "openai>gemini": { steady_state_rss_mib: null, plateaued: null, growth_rate_mib_per_min: null },
  });
  assert.equal(app.neverPlateaued(mixed), true, "a cell we DID judge, and it reached no steady state, is a finding");
  const pill = app.neverPlateauedPill(mixed);
  assert.match(pill, /no steady state/);
  assert.match(pill, /cell we could measure it on/, "the claim must narrow to the cells actually judged");
  assert.match(pill, /1 further cell/, "and say how many were not measured");
  // A gateway with every cell judged keeps the unqualified claim.
  const leakyAll = memGw("leakyall", {
    "openai>openai": { steady_state_rss_mib: null, plateaued: false, growth_rate_mib_per_min: 7.5 },
  });
  assert.match(app.neverPlateauedPill(leakyAll), /on any cell this gateway serves/);
});

test("drawer protocol matrix: a truthy-but-EMPTY cells map is not data", () => {
  // The drawer read `g.matrix.cells`, a legacy FLAT map the per-cell artifact stopped filling. It is
  // now always `{}` - which is truthy - so it passed the `if (!g.matrix.cells)` guard and then every
  // lookup missed, rendering "n/a" on all six protocol rows of every gateway on the board. The
  // measurements were present the entire time, one level down under upstreams.
  const cell = (served) => ({ served, status: 200, path: "/v1/chat/completions" });
  const modern = { matrix: { cells: {}, upstreams: {
    openai: { cells: { openai: cell(true) } },
    anthropic: { cells: { anthropic: cell(false) } },
  } } };
  const diag = app.matrixDiagonal(modern);
  assert.ok(diag, "an empty legacy map must not hide a populated upstreams tree");
  assert.equal(diag.openai.served, true);
  assert.equal(diag.anthropic.served, false, "a NOT-SERVED cell is data too, and must survive");
  assert.ok(!("gemini" in diag), "a pairing with no cell stays absent rather than being invented");

  // The legacy shape still renders, so older boards do not go blank.
  assert.equal(app.matrixDiagonal({ matrix: { cells: { openai: cell(true) } } }).openai.served, true);

  // And the two genuinely empty cases stay "not measured" rather than six rows of n/a.
  assert.equal(app.matrixDiagonal({ matrix: { cells: {}, upstreams: {} } }), null);
  assert.equal(app.matrixDiagonal({}), null);

  // Against the REAL bundle: every gateway resolves its diagonal, or this fix is theatre.
  for (const g of data.gateways) {
    if (!(g.matrix && g.matrix.upstreams)) continue;
    assert.ok(app.matrixDiagonal(g), `${g.key} publishes a matrix but its drawer diagonal is empty`);
  }
});

test("sort: a MEASURED TIE breaks on the next measurement, not on the alphabet", () => {
  // Three gateways sustained a measured ZERO ("no load held the gate") - a real result and a real
  // three-way tie. The comparator fell straight to display order, so the bottom of the column read
  // One-API, Plano, TensorZero: alphabetical, presented in a ranked table, which a reader scanning a
  // sorted column takes as a ranking. Nothing in the data said that.
  // Driven through the comparator's OWN contract - a column is anything with {id, get} - so this
  // pins the ordering rule itself rather than a particular view's cell-selection machinery. The
  // separate test below is what holds the real column sets to naming ids that exist.
  const col = { id: "rps20", get: (g) => ({ v: g.rps }) };
  const tie = { id: "lat50", get: (g) => ({ v: g.lat }) };
  const perfGw = (display, rps, lat) => ({ display, rps, lat });

  // All three tie at 0 on the sorted column; their latencies differ and are deliberately in the
  // OPPOSITE order to their names, so name-order and latency-order cannot be confused for each other.
  const gws = [
    perfGw("aaa", 0, 900),
    perfGw("mmm", 0, 100),
    perfGw("zzz", 0, 500),
  ];
  const sorted = gws.slice().sort(app.rowComparator(col, true, tie)).map((g) => g.display);
  assert.deepEqual(sorted, ["mmm", "zzz", "aaa"],
    "tied rows order by the tiebreak measurement, lowest latency first");

  // The tiebreak sorts ASCENDING even when the primary column is descending: it is not part of the
  // sort the reader asked for, it is what to do once that sort has nothing left to say.
  const asc = gws.slice().sort(app.rowComparator(col, false, tie)).map((g) => g.display);
  assert.deepEqual(asc, ["mmm", "zzz", "aaa"], "the tiebreak direction does not flip with the header");

  // Rows that do NOT tie are untouched by any of this.
  const spread = [perfGw("slow", 10, 1), perfGw("fast", 900, 999)];
  assert.deepEqual(
    spread.slice().sort(app.rowComparator(col, true, tie)).map((g) => g.display),
    ["fast", "slow"],
    "a real difference in the sorted column still decides the order outright"
  );

  // Falling back to the alphabet is still correct when the tiebreak ALSO ties - it is the last
  // resort, not something the tiebreak removed.
  const both = [perfGw("zed", 0, 7), perfGw("abe", 0, 7)];
  assert.deepEqual(
    both.slice().sort(app.rowComparator(col, true, tie)).map((g) => g.display),
    ["abe", "zed"],
    "when the numbers genuinely run out, display order is the honest last resort"
  );
});

test("sort: every declared tiebreak names a column that actually exists in its own view", () => {
  // A tiebreak pointing at a renamed or deleted column does not throw - `cols.find` returns
  // undefined and the comparator quietly reverts to the alphabet. That is the failure this whole
  // change was made to remove, and it would come back silently on the next column rename.
  for (const [view, id] of Object.entries(app.VIEW_TIEBREAK)) {
    const set = app.COLUMN_SETS[view];
    assert.ok(set, `VIEW_TIEBREAK names view ${view}, which has no column set`);
    assert.ok(set.some((c) => c.id === id),
      `VIEW_TIEBREAK.${view} = ${id}, which is not a column in that view - the tiebreak is dead config`);
  }
});

test("memory: a WAVE is not a leak, and the board must stop calling it one", () => {
  // "Never settles" described two different gateways under one red pill. One climbs without bound; the
  // other swings around a level it keeps returning to - a garbage collector doing its job. Both fail the
  // steadiness test, so both were rendered NEVER SETTLES in red beside a number the column labels a leak
  // rate. The second gateway was being accused of the first one's defect, on a public board, by name.
  const climbing = memGw("climbing", {
    "openai>openai": { steady_state_rss_mib: null, plateaued: false, growth_rate_mib_per_min: 51, shape: 1 },
  });
  const swinging = memGw("swinging", {
    "openai>openai": { steady_state_rss_mib: null, plateaued: false, growth_rate_mib_per_min: 51, shape: 0 },
  });

  // Both are unsettled - that verdict does not change, and neither one gets a steady-state number.
  assert.equal(app.neverPlateaued(climbing), true);
  assert.equal(app.neverPlateaued(swinging), true);

  const cp = app.neverPlateauedPill(climbing), sp = app.neverPlateauedPill(swinging);
  assert.ok(!/neutral/.test(cp), "unbounded growth is the defect this metric exists to catch: keep it red");
  assert.match(cp, /still growing at up to 51/, "and keep quantifying it");
  assert.match(sp, /neutral/, "a wave must not be painted in the leak colour");
  assert.match(sp, /no growth/, "and the label must say so without the reader opening a tooltip");
  assert.match(sp, /never grew either/, "the tooltip explains what it saw instead of asserting a defect");
  assert.ok(!/still growing/.test(sp), "and nothing about a wave may be described as growth");

  // THE FALLBACK. A board generated before shapes existed carries no shape at all, and an unshaped
  // gateway must NOT be quietly cleared - "no evidence of growth" and "evidence of no growth" are
  // different claims, and only the second earns the neutral pill.
  const unshaped = memGw("unshaped", {
    "openai>openai": { steady_state_rss_mib: null, plateaued: false, growth_rate_mib_per_min: 51 },
  });
  assert.ok(!/neutral/.test(app.neverPlateauedPill(unshaped)),
    "a board too old to carry shapes has not cleared anyone");

  // The per-cell tooltip carries the same distinction, because the drawer is where a reader goes to
  // check the pill rather than take it on faith.
  const tipC = app.memCellTip({ plateaued: false, growth_rate_mib_per_min: seal(51), shape: seal(1) });
  const tipS = app.memCellTip({ plateaued: false, growth_rate_mib_per_min: seal(51), shape: seal(0) });
  const tipF = app.memCellTip({ plateaued: false, growth_rate_mib_per_min: seal(-51), shape: seal(-1) });
  assert.match(tipC, /51\.0 MiB\/min under load/);
  assert.match(tipS, /did not grow/);
  assert.match(tipS, /the swing, not a leak/, "the same rate means something different under a swing");
  assert.match(tipF, /RELEASING/, "a window still handing memory back is the OPPOSITE of a leak");
  assert.ok(!/still growing/.test(tipF), "and it must never be described as growing");
});

test("memory idle: a swinging idle window is not reported as growing", () => {
  // Idle is the one window where a wave is genuinely uninteresting - nothing is being asked of the
  // gateway - so "growing 3.0 MiB/min" there is not merely harsh, it is wrong.
  assert.equal(app.idleStatic({ idle_static: seal(0), idle_growth_rate_mib_per_min: seal(3), idle_shape: seal(0) }),
    "swinging, not growing");
  assert.equal(app.idleStatic({ idle_static: seal(0), idle_growth_rate_mib_per_min: seal(-3), idle_shape: seal(-1) }),
    "releasing");
  assert.equal(app.idleStatic({ idle_static: seal(0), idle_growth_rate_mib_per_min: seal(3), idle_shape: seal(1) }),
    "growing 3.0 MiB/min", "a climbing idle window is a real finding and keeps its rate");
  assert.equal(app.idleStatic({ idle_static: seal(1) }), "steady");
});

test("memory idle: one cold-sample median wherever the row shows, and GONE when the row is empty", () => {
  const g = memGw("g", {
    "openai>openai": { idle_rss_mib: 20, steady_state_rss_mib: 100 },
    "openai>gemini": { idle_rss_mib: 24, steady_state_rss_mib: 200 },
    "anthropic>anthropic": { idle_rss_mib: 22, steady_state_rss_mib: 300 },
  });
  const i = app.idleAcrossCells(g);
  assert.deepEqual({ median: i.median, min: i.min, max: i.max, n: i.n }, { median: 22, min: 20, max: 24, n: 3 });
  // Wherever the chosen cell displays, idle is the SAME cross-cell cold median: sampled before the
  // first request, no cell involved, so it cannot vary by which served cell is chosen.
  const seen = new Set();
  for (const mode of ["min", "max", "same"]) {
    const c = memCol("memidle").get(g, memState([g], { mode }));
    seen.add(c.text);
    assert.match(c.note, /median of 3 cold samples/, "the spread is disclosed, not hidden behind one sample");
  }
  assert.deepEqual([...seen], ["22.0"], "idle is one number wherever it appears");
  // THE ROW IS ALL-OR-NOTHING (the owner's rule): a chosen cell the gateway does not serve renders a
  // FULLY empty row. Idle - measured, real, cell-independent - must not survive as one lone number on
  // an otherwise-empty row: that combo reads as a measured cell with holes, which is the exact shape
  // the 2026-07-28 board shipped for litellm-rust (idle 251.8, everything else n/a, no pill).
  const empty = memCol("memidle").get(g, memState([g], { mode: "custom", xlateIn: "gemini", xlateOut: "cohere" }));
  assert.equal(empty.na, true, "an untested chosen cell empties the WHOLE row, idle included");
});

test("memory Same defaults to the WIDEST-COVERAGE dialect, computed from the data (no protocol is named)", () => {
  // A field where the identity cell most gateways serve is NOT the one the old code hard-coded.
  const gws = [
    memGw("a", { "cohere>cohere": {}, "openai>openai": {} }),
    memGw("b", { "cohere>cohere": {} }),
    memGw("c", { "cohere>cohere": {} }),
  ];
  assert.equal(app.widestDialect({ gateways: gws }), "cohere",
    "the default is the identity cell the most gateways serve, derived from the run");
  // Ties break deterministically, and an empty board yields no answer rather than a guess.
  assert.equal(app.widestDialect({ gateways: [] }), null);
  assert.equal(app.widestDialect(null), null);
  // The app source must not name a dialect as the memory default (the gateway-isolation rule).
  const src = readFileSync(join(HERE, "app.js"), "utf8");
  assert.ok(!/widestDialect[\s\S]{0,400}return "openai"/.test(src), "no protocol may be hard-coded as the default");
});

test("memory URL codec: the new modes round-trip, and old memory links keep working", () => {
  const rt = (path, qs) => app.encodeUrl({ ...app.decodeUrl(path, qs), data: null });
  assert.equal(rt("/gateways/memory", "?mode=max"), "/gateways/memory?mode=max");
  assert.equal(rt("/gateways/memory", "?mode=custom&in=openai&out=gemini"),
    "/gateways/memory?mode=custom&in=openai&out=gemini");
  // MIN is memory's default, so it is the mode that goes UNSPELLED - a clean memory URL means Min,
  // and every other mode has to name itself. A dialect rides along whenever one is pinned, because
  // Same and Custom are one click away for the like-for-like comparison.
  assert.equal(rt("/gateways/memory", "?mode=min"), "/gateways/memory");
  assert.equal(rt("/gateways/memory", "?mode=same&d=anthropic"), "/gateways/memory?mode=same&d=anthropic");
  const st = { ...app.decodeUrl("/gateways/memory", ""), data: { gateways: [memGw("a", { "openai>openai": {} })] } };
  st.sameDialect = "openai";
  assert.equal(app.encodeUrl(st), "/gateways/memory", "the pristine memory view keeps a clean URL");
  // Old shared links: the sort id is a URL CONTRACT and survives the column's rename.
  const old = app.decodeUrl("/gateways/memory", "?sort=mempeak&dir=asc");
  assert.equal(old.sortCol, "mempeak");
  assert.equal(old.mode, "min", "an old memory link with no mode lands on memory's default, Min");
  // The perf lanes' encoding is untouched.
  assert.equal(rt("/gateways/performance", "?mode=same&d=openai"), "/gateways/performance?mode=same&d=openai");
  assert.equal(rt("/gateways/performance", ""), "/gateways/performance");
});

test("memory degrades to the LEGACY single-window shape when the bundle has no per-cell data", () => {
  assert.equal(app.hasPerCellMemory(data), app.hasPerCellMemory(data), "the live bundle answers deterministically");
  const legacy = { key: "l", display: "L", lang: "Rust",
    memory_read: memRec({ idle_rss_mib: 40, peak_rss_mib: 900, recovered_rss_mib: 55, load_cell: "anthropic>anthropic" }) };
  const st = memState([legacy]);
  assert.equal(app.hasPerCellMemory(st.data), false);
  // The old columns still read the old record, in every mode (the chooser has nothing to choose from).
  for (const mode of ["min", "max", "same", "custom"]) {
    assert.equal(memCol("mempeak").get(legacy, { ...st, mode }).text, "900.0");
    assert.equal(memCol("memidle").get(legacy, { ...st, mode }).text, "40.0");
  }
  // The per-cell-only columns are not rendered at all: a column of pure n/a is noise, not disclosure.
  const cols = app.columnsFor("memory", st.data).map((c) => c.id);
  assert.ok(!cols.includes("memgrowth"), "no growth column on a run that predates plateau termination");
  assert.deepEqual(cols, ["sel", "name", "tested", "memidle", "mempeak", "memrecov", "memcurve"]);
  // …and it IS rendered once the data can fill it.
  const perCell = { gateways: [memGw("g", { "openai>openai": {} })] };
  assert.ok(app.columnsFor("memory", perCell).map((c) => c.id).includes("memgrowth"));
  // The legacy row keeps its own honest caption (it really was measured on a throughput-peak cell).
  assert.match(memCol("tested").render(legacy, st), /peak cell/);
  assert.match(app.captionText(app.memoryCaption(st.data, st)), /chosen by throughput/);
});

test("memory: a record with NO displayable value paints NO pill (all-or-nothing, the plano shape)", () => {
  // plano on the 2026-07-28 board: a served openai>openai cell whose memory window produced nothing -
  // every envelope null. The pill advertised a measurement over four n/a columns. The pill's contract
  // is content, not existence: a row is fully measured or fully empty, never a combo.
  const nothing = { steady_state_rss_mib: null, idle_rss_mib: null, recovered_rss_mib: null,
    time_to_plateau_s: null, growth_rate_mib_per_min: null, plateaued: null };
  const g = memGw("g", { "openai>openai": nothing });
  const st = memState([g], { mode: "same", sameDialect: "openai" });
  assert.ok(app.chosenMemory(g, st), "the record EXISTS - that is exactly why existence cannot gate the pill");
  assert.equal(memCol("tested").render(g, st).includes("tested-pill"), false,
    "a record with no displayable value must not paint a pill");
  assert.equal(memCol("tested").get(g, st).na, true);
  assert.equal(memCol("memidle").get(g, st).na, true, "no lone idle number on an otherwise-empty row");
  // And the same gateway with one real value paints the pill again.
  const ok = memGw("ok", { "openai>openai": { steady_state_rss_mib: 55 } });
  const st2 = memState([ok], { mode: "same", sameDialect: "openai" });
  assert.ok(memCol("tested").render(ok, st2).includes("tested-pill"), "one displayable value restores the pill");
});

// ---- the plano shape through the SIDE DOOR: a hidden envelope key satisfying the pill -------------
// recordShowsValues asks "does this record put at least one number on the row", and it used to ask it
// of EVERY envelope on the record - including the ones no column and no drawer entry ever renders.
// The harness's own direct-to-mock leg, the kernel HWM sibling of peak RSS, the plateau timing: each
// is a real sealed envelope with a real value sitting on a record whose every VISIBLE cell is n/a. Any
// one of them was enough to paint a "Tested on" pill over four n/a columns and to keep idle alive
// beside it - the exact plano regression the all-or-nothing rule was written to stop, arriving through
// a key the reader cannot see. UNDISPLAYED_ENVELOPE_KEYS is the fix; emptying it reopens the door.
test("all-or-nothing: an UNDISPLAYED envelope key cannot satisfy the pill (the plano shape, side door)", () => {
  // A memory window whose every DISPLAYED metric is absent, carrying one hidden envelope with a value.
  const hiddenOnly = (extra) => ({
    steady_state_rss_mib: seal(null), idle_rss_mib: seal(null), recovered_rss_mib: seal(null),
    growth_rate_mib_per_min: seal(null), plateaued: null, ...extra,
  });
  const memGwRaw = (key, mem) => ({ key, display: key, lang: "Rust",
    matrix: { measured_at: "2026-07-25T00:00:00Z",
      upstreams: { openai: { cells: { openai: { served: true, perf: cellPerf({}), memory: mem } } } } } });

  for (const [label, extra] of [
    ["time_to_plateau_s", { time_to_plateau_s: seal(25) }],
    ["peak_rss_hwm_mib", { peak_rss_hwm_mib: seal(212.1) }],
    ["both", { time_to_plateau_s: seal(25), peak_rss_hwm_mib: seal(212.1) }],
  ]) {
    const g = memGwRaw("g", hiddenOnly(extra));
    const st = memState([g], { mode: "same", sameDialect: "openai" });
    assert.ok(app.chosenMemory(g, st), `${label}: the record exists (existence is not the question)`);
    assert.equal(memCol("tested").render(g, st).includes("tested-pill"), false,
      `${label}: a value on a key NO column renders must not advertise a measurement`);
    assert.equal(memCol("tested").get(g, st).na, true, `${label}: and it must not sort as measured either`);
    assert.equal(memCol("memidle").get(g, st).na, true,
      `${label}: idle must not survive as the one number on an otherwise-empty row`);
  }
  // The same door on the PERF lane: direct_c1_p99_us is the harness's own direct leg, evidence about
  // the rig rather than a column about the gateway.
  const perfTested = app.COLUMN_SETS.performance.find((c) => c.id === "tested");
  const bare = { key: "p", display: "p", lang: "Rust",
    best_cell: { path: { ingress: "openai", egress: "openai", dialect: "openai" },
      source: SRC("matrix", "6x6-diagonal"),
      added_latency_p50_us: seal(null), added_latency_p99_us: seal(null),
      rps_sustained_20ms: seal(null), rps_max_proxy: seal(null),
      direct_c1_p99_us: seal(9100) } };
  const pst = { ...app.newState(), view: "performance", mode: "peak", data: { gateways: [bare] } };
  assert.equal(perfTested.render(bare, pst).includes("tested-pill"), false,
    "the harness's own direct leg is not a measurement OF this gateway and must not paint its pill");
  // ...and a real displayed number restores the pill on both lanes, so this is a content rule and not
  // a blanket suppression.
  bare.best_cell.added_latency_p99_us = seal(110);
  assert.ok(perfTested.render(bare, pst).includes("tested-pill"),
    "one VISIBLE value restores the pill - the rule is about what the reader can see, not about hiding");
});

// ---- the RSS sparkline is bound by the same all-or-nothing rule -----------------------------------
// rss_series is a raw array, not a sealed envelope, so nothing in the metric machinery constrains it:
// without an explicit guard a live sparkline outlived the rule and survived as the ONE occupied cell
// on a row of n/a, which is precisely the "measured, but only a bit" state the owner's rule forbids.
// The column has TWO code paths and only the renderer emits markup - the get() guard governs the CSS
// class and the sort value, so removing the guard from render() alone leaves the line on the page.
test("memory: the RSS sparkline obeys all-or-nothing too (the get() guard does not govern the markup)", () => {
  const series = [0, 30, 60, 90, 120].map((t, i) => ({ t_s: t, rss_mib: 100 + i * 5 }));
  const empties = { steady_state_rss_mib: seal(null), idle_rss_mib: seal(null), recovered_rss_mib: seal(null),
    growth_rate_mib_per_min: seal(null), plateaued: null };
  const gwWith = (mem) => ({ key: "g", display: "g", lang: "Rust",
    matrix: { measured_at: "2026-07-25T00:00:00Z",
      upstreams: { openai: { cells: { openai: { served: true, perf: cellPerf({}), memory: mem } } } } } });

  // (1) every displayed metric absent, but a perfectly good curve recorded.
  const g = gwWith({ ...empties, rss_series: series });
  const st = memState([g], { mode: "same", sameDialect: "openai" });
  const curve = memCol("memcurve");
  const html = curve.render(g, st);
  assert.ok(!/<svg/.test(html),
    `an all-n/a row must not keep a live sparkline as its one occupied cell; got: ${html.slice(0, 120)}`);
  assert.match(html, /n\/a/, "the cell reads n/a like every other cell on the row");
  assert.equal(curve.get(g, st).na, true, "and the column agrees about it");

  // (2) the same curve on a row that DID measure something still draws, so this is not a blanket kill.
  const ok = gwWith({ ...empties, steady_state_rss_mib: seal(120), rss_series: series });
  const st2 = memState([ok], { mode: "same", sameDialect: "openai" });
  assert.ok(/<svg/.test(curve.render(ok, st2)), "a measured row still gets its curve");
  assert.equal(curve.get(ok, st2).na, false);
});

test("memory: an unserved chosen cell reads n/a and nothing is substituted from another cell", () => {
  const g = memGw("g", { "anthropic>anthropic": { steady_state_rss_mib: 77 } });
  const st = memState([g], { mode: "same", sameDialect: "openai" });
  assert.equal(memCol("mempeak").get(g, st).na, true, "the gateway does not serve the chosen cell");
  assert.equal(memCol("memrecov").get(g, st).na, true);
  assert.equal(memCol("tested").render(g, st).includes("tested-pill"), false, "no record, no pill");
  // The row still exists: filtering a competitor out reads as hiding it.
  assert.deepEqual(app.applyFilters([g], st).map((x) => x.key), ["g"]);
  // A gateway with per-cell data missing entirely in a per-cell bundle is n/a, never patched from the
  // legacy peak-cell window it may still carry.
  const mixed = { key: "m", display: "M", lang: "Rust", memory_read: memRec({ peak_rss_mib: 999 }) };
  const st2 = memState([g, mixed], { mode: "same", sameDialect: "anthropic" });
  assert.equal(memCol("mempeak").get(mixed, st2).na, true,
    "a throughput-selected legacy number must never appear behind a memory-selected label");
});

test("memory drawer/compare read the SAME chosen cell as the table (no lane divergence)", () => {
  const g = memGw("g", { "openai>openai": { steady_state_rss_mib: 120 }, "openai>gemini": { steady_state_rss_mib: 61.5 } });
  const lane = app.LANES.find((l) => l.key === "memory");
  for (const [mode, expect] of [["min", 61.5], ["max", 120], ["same", 120]]) {
    const st = memState([g], { mode, sameDialect: "openai" });
    const j = app.laneRecord(lane, g, st);
    assert.equal(app.mval(j.steady_state_rss_mib), expect, `${mode}: the drawer reads the table's cell`);
    assert.equal(app.metric(j.steady_state_rss_mib).v, memCol("mempeak").get(g, st).v);
    // Provenance routes through the ONE caption table, and names the cell, not a hard-coded sentence.
    assert.match(app.lanePathNote(lane, j, st), /memory window/);
  }
});

// ---- Min/Max name a cell through the MEMORY chooser, and every other lane must hear it ------------
// chooserDialects answers "which (ingress, egress) is the chosen cell" for every non-memory surface:
// the drawer's perf and streaming lanes, the compare table and the sweep charts all read it. Its modes
// used to be peak / same / else-custom, and Min and Max are neither - so they fell into the CUSTOM arm
// and returned whatever stale (xlateIn, xlateOut) pair the user last had selected on another tab. The
// memory column then correctly showed the min cell while every other lane showed a cell the reader had
// not chosen, captioned by lanePathNote as "the lowest steady-state cell the table shows". Two
// different cells, one label, no disagreement visible anywhere.
//
// The existing chooser tests cover `peak` and `same` only, which is why this shipped.
test("chooser: Min/Max resolve to the MEMORY chooser's own cell, never the stale Custom pair", () => {
  const g = memGw("g", {
    "openai>openai": { steady_state_rss_mib: 50 },       // the MIN cell
    "anthropic>anthropic": { steady_state_rss_mib: 900 }, // the MAX cell
    "gemini>bedrock": null,                               // served, no memory: the stale Custom pair
  });
  // The reader's last Custom selection points at a cell that is REAL and WRONG - so a leak shows up as
  // another gateway's numbers under a memory caption, not as a convenient n/a.
  const stale = { xlateIn: "gemini", xlateOut: "bedrock", sameDialect: "openai" };

  for (const [mode, ing, eg] of [["min", "openai", "openai"], ["max", "anthropic", "anthropic"]]) {
    const st = memState([g], { mode, ...stale });
    assert.deepEqual(app.chooserDialects(g, st), [ing, eg],
      `${mode} must name the cell the memory chooser picked, not the stale Custom pair`);
    // ...and the surfaces that read it agree. The perf record is stamped by the ONE choke point, so its
    // path IS the claim every lane renders: drawer, compare and the sweep charts all read this record.
    const p = app.chooserCellPerf(g, st);
    assert.ok(p, `${mode}: the chosen cell must resolve to a real perf record`);
    assert.equal(p.path.ingress, ing, `${mode}: the drawer/compare/sweep lanes read the memory cell`);
    assert.equal(p.path.egress, eg);
    assert.equal(app.chooserPerfCell(g, "added_latency_p99_us", (v) => String(v), st).na, false);
  }
  // Custom itself is untouched: it still means exactly what the reader selected.
  assert.deepEqual(app.chooserDialects(g, memState([g], { mode: "custom", ...stale })), ["gemini", "bedrock"]);
  // And Min/Max on a gateway with NO memory anywhere name no cell at all rather than falling back to
  // a pair the reader never chose.
  const noMem = memGw("n", { "gemini>bedrock": null });
  assert.deepEqual(app.chooserDialects(noMem, memState([noMem], { mode: "min", ...stale })), [null, null],
    "no memory window means no chosen cell - not the stale pair wearing a memory caption");
});

test("gen-data SEALS the per-cell memory window: no published memory number ships as a bare scalar", () => {
  // The producer's per-cell block, exactly as the design says it publishes it. Everything the board
  // renders off it must come back as an envelope, including growth rate and time-to-plateau, which are
  // NOT rss-shaped and so cannot be discovered by the RSS pattern (the bug that shipped peak_rss_hwm_mib
  // unsealed). plateaued is a boolean verdict, not a metric, and stays a bare bool.
  const root = buildStreamMemRepo();
  const raw = JSON.parse(readFileSync(join(root, "results", "matrix", "sgw.json"), "utf8"));
  const cellMemory = { steady_state_rss_mib: 119.7, idle_rss_mib: 7.1, recovered_rss_mib: 40.2,
    plateaued: false, time_to_plateau_s: null, growth_rate_mib_per_min: 6.4,
    rss_series: [{ t_s: 0, rss_mib: 7.1 }, { t_s: 60, rss_mib: 119.7 }] };
  raw.upstreams.openai.cells.openai.memory = { ...cellMemory };
  raw.cells.openai.memory = raw.upstreams.openai.cells.openai.memory;
  writeFileSync(join(root, "results", "matrix", "sgw.json"), JSON.stringify(raw));
  const bundle = genInto(root);
  const g = bundle.gateways.find((x) => x.key === "sgw");
  const mem = app.perCellMemory(g, "openai", "openai");
  assert.ok(mem, "the per-cell window must survive into the bundle where the memory tab reads it");
  for (const k of ["steady_state_rss_mib", "idle_rss_mib", "recovered_rss_mib", "growth_rate_mib_per_min", "time_to_plateau_s"])
    assert.ok(app.isEnvelope(mem[k]), `${k} must be a sealed envelope, not a bare scalar`);
  assert.equal(mem.plateaued, false, "the plateau verdict is a bool, not a metric");
  assert.equal(app.mval(mem.growth_rate_mib_per_min), 6.4);
  assert.equal(app.mval(mem.time_to_plateau_s), null, "never plateaued: no time-to-plateau exists");
  // C1 (no bare metric field anywhere in the bundle) must hold with the new fields present.
  const { errors } = checkConsistency(bundle, app, SYNTH);
  assert.deepEqual(errors.filter((e) => e.startsWith("C1") || e.startsWith("C2")), []);
  // And the board reads it: this gateway never settled, so it is flagged and its growth is the finding.
  const st = { ...app.newState(), view: "memory", mode: "min", data: bundle };
  assert.equal(app.neverPlateaued(g), true);
  assert.equal(app.hasPerCellMemory(bundle), true);
  assert.equal(app.COLUMN_SETS.memory.find((c) => c.id === "memgrowth").get(g, st).text, "6.4 (leak)");
});

// ---- the matrix grid shows EVERY gateway, including one that produced no matrix -----------------
test("protocol grid: a matrix-less gateway renders an all-n/a row with its reason, never disappears", () => {
  const tally = () => ({ pass: 0, fail: 0, notconf: 0, unprobed: 0, unverified: 0, untestable: 0 });
  const withM = memGw("has-matrix", { "openai>openai": {} });
  const withoutM = { key: "no-matrix", display: "no-matrix", lang: "Go", matrix: null,
    serve_error: "container exited before the first probe\n  at /home/ec2-user/bench/lib/harness.sh:12" };
  const roster = app.matrixRoster([withM, withoutM], tally);
  assert.deepEqual(roster.map((g) => g.key), ["has-matrix", "no-matrix"],
    "a gateway with no matrix must still be a row, sorted last, never filtered out");
  assert.equal(app.hasMatrixGrid(withM), true);
  assert.equal(app.hasMatrixGrid(withoutM), false);
  assert.equal(app.hasMatrixGrid({}), false);
  // The reason travels with the row: total failure must read as a row of n/a, not as absence.
  const why = app.matrixFailureReason(withoutM);
  assert.match(why, /^no matrix result: container exited before the first probe$/,
    "the failure reason is surfaced, first line only, with rig paths scrubbed");
  assert.match(app.matrixFailureReason({ key: "q", display: "q" }), /no matrix result: the run produced no protocol matrix/,
    "with nothing recorded we state that, rather than inventing a cause");
  // RED-before: the old filter dropped exactly this row.
  const oldBehaviour = [withM, withoutM].filter((g) => g.matrix && (g.matrix.upstreams || g.matrix.cells));
  assert.equal(oldBehaviour.length, 1, "…which is the disappearance this test exists to prevent");
});

/* ---- LEDGER 2026-07-29: the deferred SITE-* findings, each with the RED it now fails ------------- */

// SITE-01. The oracle compared `.v` and nothing else, so everything the envelope SAYS about its number
// was unverified: a reason flattened back to "not_measured", a zero-note swapped between "no qualifying
// ceiling" and "measured failure", a lost detail. Each preserves the number, so each verified green.
// The `paced_match` arm this test used to carry is gone with the suppression it belonged to: it was a
// boolean restatement of the retired `*_mock_bound` verdict ("this number matched the paced upstream"),
// which was all a verdict could express. The engine now publishes the ceiling and the fraction of it
// reached, so the same information arrives as `headroom` + `rig_ceiling` - numbers a reader can weigh,
// where 0.993 and 0.20 were both `paced_match: undefined` before. They are PUBLISHED fields, so the
// oracle must re-derive them for the same reason it re-derives the reason and the note: anything the
// envelope says about its number is data the board renders, and an unverified fact can drift.
test("SITE-01: the oracle re-derives the whole envelope - reason, note, detail, headroom, ceiling", () => {
  const oe = checkMod.oracleEnvelope;
  // an absence carries the ENGINE's reason and its prose, not a flattened token
  assert.deepEqual(oe(null, { absent: { reason: "below_resolution", detail: "too small to weigh" } }),
    { v: 0, reason: "below_resolution", note: null, detail: "too small to weigh", headroom: null, ceiling: null });
  assert.deepEqual(oe(null, {}),
    { v: null, reason: "not_measured", note: null, detail: null, headroom: null, ceiling: null });
  // an absence publishes NO comparison, even when the raw artifact carried one: there is no measurement
  // for a fraction to be a fraction of, and the seal's absent branch returns before the facts attach.
  assert.deepEqual(oe(null, { headroom: 0.83, ceiling: 120, absent: { reason: "harness_error" } }),
    { v: null, reason: "harness_error", note: null, detail: null, headroom: null, ceiling: null });
  // a certified zero carries the note that names WHICH zero it is
  assert.equal(oe(0, { zeroNote: ZERO_MEASURED_FAIL }).note, ZERO_MEASURED_FAIL);
  assert.equal(oe(0, { zeroNote: ZERO_NO_CEILING }).note, ZERO_NO_CEILING);
  // and the comparison's facts, which a certified 0 carries as much as a certified maximum does
  assert.deepEqual(oe(0, { zeroNote: ZERO_MEASURED_FAIL, headroom: 0, ceiling: 52013 }),
    { v: 0, reason: null, note: ZERO_MEASURED_FAIL, detail: null, headroom: 0, ceiling: 52013 });
  assert.deepEqual(oe(9, { headroom: 0.993, ceiling: 9.06 }),
    { v: 9, reason: null, note: null, detail: null, headroom: 0.993, ceiling: 9.06 });
  // a present number NEVER resolves to a null value: the branch that could is gone, which is the
  // property C2 asserts from the bundle side (no envelope in a published bundle is suppressed).
  for (const h of [null, 0, 0.5, 1, 1.4]) assert.equal(oe(9, { headroom: h, ceiling: 10 }).v, 9);
  // THE SEAL AND THE ORACLE MUST AGREE ON ALL OF IT, not only on the number.
  for (const raw of [null, 0, 7]) {
    for (const headroom of [null, 0, 0.5, 0.993, 1]) {
      for (const ceiling of [null, 52013]) {
        const s = sealMetric(raw, { headroom, ceiling, zeroNote: ZERO_MEASURED_FAIL });
        const o = oe(raw, { headroom, ceiling, zeroNote: ZERO_MEASURED_FAIL });
        const at = `raw=${raw} headroom=${headroom} ceiling=${ceiling}`;
        assert.equal(s.reason ?? null, o.reason, `reason for ${at}`);
        assert.equal(s.note ?? null, o.note, `note for ${at}`);
        assert.equal(s.headroom ?? null, o.headroom, `headroom for ${at}`);
        assert.equal(s.rig_ceiling ?? null, o.ceiling, `ceiling for ${at}`);
      }
    }
  }
});

testWithMatrixDonor("SITE-01 RED: a mangled reason / note / headroom on a CORRECT number fails the oracle", () => {
  // Mangled on a surviving sealed envelope. It used to be `rps_sustained_20ms`, which no producer emits;
  // the oracle's claim is about the SHAPE of a published envelope, so any sealed metric on the record proves
  // it. (A frontier reading's rate is an envelope too and is oracled through the same walk.)
  const at = (d) => matrixGw(d).best_cell.added_latency_p99_us;
  // THE CONTROL, so each mangle below is proved to be what fires the oracle rather than something else on
  // the board being the real cause. An untouched clone owes NO R1 finding on this envelope at all; if this
  // line ever fails, every assertion under it is passing for the wrong reason.
  assert.deepEqual(checkConsistency(clone(), app).errors.filter((x) => x.startsWith("R1:")), [],
    "an unmangled bundle must produce no independent-oracle finding, or the RED cases below prove nothing");
  // (a) a note the raw data does not imply: the number is untouched, so a value-only oracle sees nothing.
  {
    const d = clone(); at(d).note = ZERO_MEASURED_FAIL;
    const e = checkConsistency(d, app).errors;
    assert.ok(e.some((x) => x.startsWith("R1:") && x.includes("`note`")),
      `the oracle must catch a note the raw artifact does not imply; got ${JSON.stringify(e.slice(0, 3))}`);
  }
  // (b) a reason bolted onto a certified envelope.
  {
    const d = clone(); at(d).reason = "not_measured";
    const e = checkConsistency(d, app).errors;
    assert.ok(e.some((x) => x.startsWith("R1:") && x.includes("`reason`")),
      `the oracle must catch an invented reason; got ${JSON.stringify(e.slice(0, 3))}`);
  }
  // (c) A HEADROOM THE RAW ARTIFACT DOES NOT IMPLY. This case used to mangle `paced_match`, the boolean
  //     the retired verdict was carried as. The published fact is now a fraction, which is strictly more
  //     mangleable than a boolean was - it can attach to the wrong metric, or drift away from the ceiling
  //     it claims to be a fraction of - and every one of those preserves the number, so a value-only
  //     oracle reports green on all of them. Both halves of the fact are mangled here, separately.
  {
    const d = clone(); at(d).headroom = 0.42;
    const e = checkConsistency(d, app).errors;
    assert.ok(e.some((x) => x.startsWith("R1:") && x.includes("`headroom`")),
      `the oracle must catch a headroom the raw artifact does not imply; got ${JSON.stringify(e.slice(0, 3))}`);
  }
  {
    const d = clone(); at(d).rig_ceiling = 1;
    const e = checkConsistency(d, app).errors;
    assert.ok(e.some((x) => x.startsWith("R1:") && x.includes("`ceiling`")),
      `the oracle must catch a ceiling the raw artifact does not imply; got ${JSON.stringify(e.slice(0, 3))}`);
  }
});

// SITE-02. The oracle verified the numbers at the coordinates the BUNDLE CLAIMED and never asked whether
// those were the right coordinates, so a wrong best/translation cell published correct values under the
// wrong name and every comparison agreed with it.
test("SITE-02: the SELECTION is re-derived from the raw artifact, by the published rule", () => {
  const cell = (p99, served = true) => ({ served, perf: { added_latency_p99_us: p99, rps_sustained_20ms: 1 } });
  // the canonical openai diagonal wins whenever it is served, whatever the others read
  const m1 = { upstreams: { openai: { cells: { openai: cell(900) } }, anthropic: { cells: { anthropic: cell(10) } } } };
  assert.equal(checkMod.oracleBestDialect(m1), "openai");
  // with no openai diagonal, the lowest added-latency p99 wins
  const m2 = { upstreams: { gemini: { cells: { gemini: cell(900) } }, anthropic: { cells: { anthropic: cell(10) } } } };
  assert.equal(checkMod.oracleBestDialect(m2), "anthropic");
  // a BELOW-RESOLUTION p99 is the best reading the rig can express, so it ranks 0 and wins - not last
  const m3 = { upstreams: {
    gemini: { cells: { gemini: cell(10) } },
    anthropic: { cells: { anthropic: { served: true, perf: { rps_sustained_20ms: 1 },
      absences: { "perf.added_latency_p99_us": { reason: "below_resolution" } } } } } } };
  assert.equal(checkMod.oracleBestDialect(m3), "anthropic");
  // translation: the FAIR (openai-ingress) tier outranks a faster any-tier candidate
  const m4 = { upstreams: {
    anthropic: { cells: { openai: cell(500), gemini: cell(5) } } } };
  assert.deepEqual(checkMod.oracleTranslationPath(m4), { ingress: "openai", egress: "anthropic" });
  // and the any tier is used only when the matrix measured no openai-ingress cell at all
  const m5 = { upstreams: { anthropic: { cells: { gemini: cell(5), bedrock: cell(50) } } } };
  assert.deepEqual(checkMod.oracleTranslationPath(m5), { ingress: "gemini", egress: "anthropic" });
});

testWithMatrixDonor("SITE-02 RED: a projected record naming the WRONG cell fails R4", () => {
  {
    const d = clone(); matrixGw(d).best_cell.path.dialect = "not-a-dialect";
    const e = checkConsistency(d, app).errors;
    assert.ok(e.some((x) => x.startsWith("R4:") && x.includes("best_cell")),
      `R4 must re-derive the best-cell selection; got ${JSON.stringify(e.slice(0, 3))}`);
  }
  {
    const d = clone();
    const g = d.gateways.find((x) => x.translation_cell && x.translation_cell.source.kind === "matrix");
    if (g) {
      g.translation_cell.path.egress = "not-a-dialect";
      const e = checkConsistency(d, app).errors;
      assert.ok(e.some((x) => x.startsWith("R4:") && x.includes("translation_cell")),
        `R4 must re-derive the translation selection; got ${JSON.stringify(e.slice(0, 3))}`);
    }
  }
  {
    const d = clone();
    const g = d.gateways.find((x) => x.streaming && x.streaming.source.kind === "matrix" && x.best_cell);
    if (g) {
      g.streaming.path.dialect = "not-a-dialect";
      const e = checkConsistency(d, app).errors;
      assert.ok(e.some((x) => x.startsWith("R4:") && x.includes("streaming")),
        `R4 must catch streaming projected off a different cell than best_cell; got ${JSON.stringify(e.slice(0, 3))}`);
    }
  }
});

// SITE-03. A whole perf/stream/memory block missing on one side was skipped in silence while the gateway
// still earned its per-gateway oracle credit from whichever block did compare.
testWithData("SITE-03 RED: a whole block the oracle cannot compare fails, and costs the gateway its oracle credit", () => {
  const d = clone();
  let hit = null;
  outer:
  for (const g of d.gateways) {
    for (const up of Object.values((g.matrix && g.matrix.upstreams) || {})) {
      for (const [ingress, cell] of Object.entries((up && up.cells) || {})) {
        if (cell && cell.stream) { delete cell.stream; hit = { key: g.key, ingress }; break outer; }
      }
    }
  }
  assert.ok(hit, "precondition: the board publishes at least one sealed stream block to drop");
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("R1:") && x.includes(hit.key) && x.includes("went unverified")),
    `a dropped block must be reported, not skipped; got ${JSON.stringify(e.slice(0, 3))}`);
  assert.ok(e.some((x) => x.startsWith("R2:") && x.includes("never verified") && x.includes(hit.key)),
    `and the gateway must lose its oracle credit rather than stay 'oracled'; got ${JSON.stringify(e.slice(0, 5))}`);
});

// SITE-04. rawCellAt read only m.upstreams, so a v1-shape artifact produced ZERO comparisons - and the
// per-gateway coverage gate then turned that silence into a hard failure on an honest legacy publish.
test("SITE-04: a v1-shape raw artifact is read by the oracle, not skipped into a publish failure", () => {
  const v1 = { upstream_shape: "anthropic", cells: { openai: { served: true, perf: { added_latency_p99_us: 7 } } } };
  assert.deepEqual(Object.keys(checkMod.upstreamsOf(v1)), ["anthropic"]);
  assert.equal(checkMod.rawCellAt(v1, "openai", "anthropic").perf.added_latency_p99_us, 7);
  assert.equal(checkMod.rawCellAt(v1, "openai", "openai"), null, "the shape names the ONE measured egress");
  // a v1 artifact with no upstream_shape is the openai row, exactly as gen-data normalizes it
  assert.ok(checkMod.rawCellAt({ cells: { openai: { served: true } } }, "openai", "openai"));
  // v2 is untouched, and a matrix with neither shape yields nothing rather than throwing
  assert.equal(checkMod.rawCellAt({ upstreams: { openai: { cells: { gemini: { served: true } } } } }, "gemini", "openai").served, true);
  assert.deepEqual(checkMod.upstreamsOf({}), {});
  // and C6 now bites on a v1 cell: the invariant is about the ordering of one curve, not about the
  // artifact's version. An inverted frontier on a v1-shaped cell must still block.
  const inv = checkMod.c6Inversions("gw", { upstream_shape: "openai", cells: { openai: { served: true,
    perf: { frontier: [rd(1, 200), rd(5, 100)], sweep_max_proxy: [{ conc: 64, rps: 200 }, { conc: 128, rps: 90 }] } } } });
  assert.equal(inv.cellsChecked, 1);
  assert.ok(inv.violations.some((v) => /the frontier inverts/.test(v)), JSON.stringify(inv.violations));
});

// SITE-05. The C3 caption lint scanned DOUBLE-quoted tokens only, and exempted any line containing the
// word "source" - which is every caption renderer, since a caption renderer is a function about where a
// datum came from. Both holes point the same way: not firing.
test("SITE-05: the C3 lint sees every quoting style, and its exemption is a sweep ASSIGNMENT only", () => {
  const region = { enter: /const SWEEP_CAPTION\s*=/, exit: /^\};\s*$/m };
  const head = 'const SWEEP_CAPTION = {\n  "6x6-diagonal": () => "x",\n};\n';
  const fires = (line) => checkMod.lintSweepKeys(head + line + "\n", "fake.js", region).errors;
  // every quoting style the two languages can leak a token in
  assert.equal(fires(`const note = '6x6-diagonal';`).length, 1, "single quotes must be scanned");
  assert.equal(fires('const note = `6x6-diagonal`;').length, 1, "template literals must be scanned");
  assert.equal(fires('const note = "6x6-diagonal";').length, 1, "double quotes, as before");
  // A CAPTION RENDERER IS EXACTLY WHERE THIS BUG CLASS LIVES, and it mentions "source" by nature.
  assert.equal(fires('function sourceLabel(x) { return "6x6-diagonal"; }').length, 1,
    "a line mentioning `source` must NOT be exempt: caption renderers are where the leak happens");
  assert.equal(fires('const t = sweepy + "6x6-diagonal";').length, 1,
    "a word merely containing `sweep` is not a sweep assignment");
  // the one legitimate use stays legitimate: the token AS the value of a sweep key (provenance data)
  assert.deepEqual(fires('rec.source = { kind: "matrix", sweep: "6x6-diagonal" };'), []);
  assert.deepEqual(fires(`rec.source = { kind: 'matrix', sweep: '6x6-diagonal' };`), []);
  assert.deepEqual(fires('lbl = _sweep_label({"sweep": "6x6-diagonal"})'), []);
  // and the repo's own scanned file stays clean under the stricter lint. The python fixtures above
  // stay: the lint is language-general, and a second renderer is exactly what it was written for.
  const appSrc = readFileSync(join(HERE, "app.js"), "utf8");
  assert.deepEqual(checkMod.lintSweepKeys(appSrc, "app.js", region).errors, []);
});

// SITE-06. The C5 routing lint knew one spelling of "read the raw number off the envelope". A reader
// routes around the other three by accident, in three keystrokes, and the lint reports itself green.
test("SITE-06: the C5 lint catches bracket, bracket-chain and destructuring reads, not just `.value`", () => {
  const bad = (body) => checkMod.lintAccessorRouting(
    `function draw(p, key) {\n  const env = p[key];\n  if (!isEnvelope(env)) return;\n  ${body}\n}\n`, "fake.js", "js").errors;
  assert.equal(bad('out.push(env["value"]);').length, 1, 'env["value"] must fire');
  assert.equal(bad("out.push(env['value']);").length, 1, "env['value'] must fire");
  assert.equal(bad("out.push(env[key].value);").length, 1, "a bracket-then-.value chain must fire");
  assert.equal(bad("const { value } = env;").length, 1, "a destructured read must fire");
  assert.equal(bad("const { value: v } = env; out.push(v);").length, 1, "a renamed destructured read must fire");
  assert.deepEqual(bad("out.push(mval(env));"), [], "the routed read stays clean");
  // the direct field form, in the same spellings
  const direct = (line) => checkMod.lintAccessorRouting(line + "\n", "fake.js", "js").errors;
  assert.ok(direct('const x = p.added_latency_p99_us["value"];').length >= 1);
  assert.ok(direct('const x = p["added_latency_p99_us"].value;').length >= 1);
  assert.ok(direct("const { value } = p.added_latency_p50_us;").length >= 1);
  // python's bracket spelling of the same read
  const py = 'def draw(bc):\n    env = bc.get("added_latency_p99_us")\n    if _is_env(env):\n        return env["value"]\n';
  assert.ok(checkMod.lintAccessorRouting(py, "fake.py", "py").errors.length >= 1);
  // and the repo's own reader is still clean under the wider lint
  assert.deepEqual(checkMod.lintAccessorRouting(readFileSync(join(HERE, "app.js"), "utf8"), "app.js", "js").errors, []);
});

// ---- the C5 lint polices the WHOLE sealed vocabulary, not four names of it (round-2 audit) --------
// The direct-form arm iterated GATED_FIELDS - four throughput metrics - so it covered 4 of the ~20
// fields seal.mjs seals. Every latency, ttft, gap, growth, plateau and RSS envelope could be
// dereferenced straight to its raw number with the lint reporting green, which is the bug class the
// lint exists for, walking past the lint. The three forms below were each PROVEN green before this
// change. The field name is now discovered from the deref and judged by seal.mjs's own isMetricField,
// so the vocabulary cannot drift from the thing that defines it.
test("C5 lint: EVERY sealed metric field is policed, not just the four gated ones", () => {
  const errs = (src) => checkMod.lintAccessorRouting(src, "fake.js", "js").errors;
  const fires = (src, why) => assert.ok(errs(src).length >= 1, `${why}\n  source: ${src.trim()}\n  got: []`);

  // The three forms the audit proved green:
  fires("const x = g.best_cell.added_latency_p99_us.value;\n",
    "an UNGATED latency field read straight to its raw number must fire");
  fires('const x = g.best_cell["added_ttft_p99_us"]["value"];\n',
    "the fully-bracketed spelling of a streaming field must fire");
  fires("function f(g) {\n  const rec = g.best_cell.added_latency_p50_us;\n  return rec.value;\n}\n",
    "a sealed field bound to a local and dereferenced on a LATER line must fire - the field's own name " +
    "is the type evidence, no accessor call needed");

  // ...and the rest of the vocabulary, one representative per family, including the RSS metrics that
  // no list anywhere enumerates (seal.mjs discovers them by pattern, so a whitelist could never have
  // covered them).
  fires("const x = m.peak_rss_mib.value;\n", "an RSS metric is a sealed envelope like any other");
  fires("const x = m.peak_rss_hwm_mib.value;\n", "the qualified RSS variants are sealed too");
  fires("const x = m.growth_rate_mib_per_min.value;\n", "the memory growth rate is a published number");
  fires("const x = m.time_to_plateau_s.value;\n", "so is the plateau timing");
  fires("const x = s.added_gap_p99_us.value;\n", "the streaming gap metrics are sealed");
  fires("const x = c.streams_sustained_fps.value;\n", "the paced streaming rate is sealed");
  fires("const { value } = m.recovered_rss_mib;\n", "the destructured spelling, on a non-gated field");

  // A NON-metric field is not an envelope and must NOT fire: the lint has to stay usable.
  assert.deepEqual(errs("const x = cell.status.value;\n"), [], "a plain field is not a sealed metric");
  assert.deepEqual(errs("const x = g.matrix.rss_series.value;\n"), [],
    "rss_series is a raw array, not an RSS-in-MiB envelope");
  assert.deepEqual(errs("const x = mval(g.best_cell.added_latency_p99_us);\n"), [],
    "the routed read stays clean on every field, gated or not");
  assert.deepEqual(errs("const rec = memCell(g, \"growth_rate_mib_per_min\", fmt1, st);\nreturn rec.value;\n"), [],
    "a field NAME passed as a string argument does not make the result an envelope");
});

// SITE-07. The finding was that a published number said NOTHING about the comparison it came out of: the
// paced branch's comment promised "the flag stays on the envelope as the signal it always was" and
// nothing of the kind survived, so a gateway that merely matched the mock's paced target and one proven
// far below its ceiling were indistinguishable in the bundle. That is a real difference between two
// published numbers, and it was being dropped on the floor.
//
// The `paced_match: true` this test used to assert is gone with the verdict it restated - and the finding
// it was the fix for is BETTER served now, which is why the test survives rather than being deleted. The
// engine publishes the ceiling and the fraction of it reached, so instead of one boolean covering every
// near-match, the envelope carries the number itself: 0.993 and 0.20 were both `paced_match: undefined`
// before. What must hold is unchanged in shape - the signal reaches the envelope in a form C1 accepts
// (named fields, never the raw `*_mock_bound` / `*_mock_ceiling` keys C1 refuses), it distinguishes the
// two cases in the bundle, and C2 finds nothing suppressed.
test("SITE-07: a near-ceiling publish carries the comparison's facts, in a form C1 accepts", () => {
  const matched = sealMetric(39000, { headroom: 0.993, ceiling: 39275, zeroNote: ZERO_MEASURED_FAIL });
  assert.equal(matched.value, 39000, "matching a paced target publishes the number");
  assert.equal(matched.headroom, 0.993, "and says how close it came, on the envelope");
  assert.equal(matched.rig_ceiling, 39275, "against a stated ceiling, so the fraction is checkable");
  const proven = sealMetric(39000, { headroom: 0.2, ceiling: 195000, zeroNote: ZERO_MEASURED_FAIL });
  assert.equal(proven.headroom, 0.2, "a proven-unbound number states its own, much smaller, fraction");
  assert.notEqual(JSON.stringify(matched), JSON.stringify(proven),
    "the two must be distinguishable in the bundle; that is the whole finding");
  // NOT a re-emitted raw sibling: the engine's own key names are consumed, which is what C1 forbids
  // anywhere in a published bundle.
  for (const k of Object.keys(matched))
    assert.ok(!/_(mock_bound|headroom|mock_ceiling|rig_ceiling)$/.test(k), `raw engine key ${k} survived the seal`);
  // No usable reference costs the facts and NOT the value - the case that used to suppress outright.
  const unreferenced = sealMetric(39000, { headroom: null, ceiling: null });
  assert.equal(unreferenced.value, 39000);
  assert.equal(unreferenced.suppressed, false);
  assert.equal(unreferenced.headroom, undefined, "an unstated fraction is absent, never invented");
  // C1/C2 accept the envelope in a real bundle: it is certified, and the facts are not a hidden second
  // number for a render site to reach past the reader with.
  const d = { gateways: [{ key: "g", streaming: { path: { dialect: "openai" },
    source: { kind: "matrix", sweep: "6x6-stream-diagonal", build: "b", measured_at: "2026-07-24T00:00:00Z" },
    stream_served: true, streams_sustained: matched } }] };
  assert.deepEqual(checkConsistency(d, app, SYNTH).errors, []);
});

// SITE-08. The gated measured-zero returned before the extras were attached, so a certified 0 lost its
// concurrency and its sweep - the curve that is the evidence FOR the zero, and the reading whose
// evidence a reader most needs, since a 0 beside a real maximum is the claim that most demands it.
test("SITE-08: a certified MEASURED ZERO keeps its concurrency and its sweep evidence", () => {
  const sweep = [{ conc: 64, rps: 0, p99_us: 900000, fail: 12 }, { conc: 128, rps: 0, p99_us: 900000, fail: 40 }];
  const z = sealMetric(0, { zeroNote: ZERO_NO_CEILING, extras: { concurrency: 64, conc_at: 64, sweep },
    headroom: 0, ceiling: 25700 });
  assert.equal(z.value, 0);
  assert.equal(z.certified, true);
  assert.equal(z.note, ZERO_NO_CEILING, "the note still names WHICH zero this is");
  assert.equal(z.concurrency, 64, "a certified zero was measured AT a concurrency");
  assert.deepEqual(z.sweep, sweep, "and its sweep is the evidence the zero rests on");
  // The comparison's facts attach through the SAME path as the extras, for the same reason: a 0 beside a
  // real ceiling is the claim that most demands its evidence, so "0 out of a rig ceiling of 25,700"
  // reaches the reader rather than a bare 0.
  assert.equal(z.headroom, 0, "a measured zero states its headroom too - zero of the ceiling is a reading");
  assert.equal(z.rig_ceiling, 25700);
  // AN ABSENCE - now the only no-value envelope - carries none of it: no extra, no fact, nothing
  // recoverable (C2). This half used to make that point about a SUPPRESSED envelope, the shape that no
  // longer exists; the property is the same one and the absence is what still has it.
  const s = sealMetric(null, { extras: { concurrency: 64, sweep }, headroom: 0.9, ceiling: 25700 });
  assert.equal(s.value, null);
  assert.equal(s.suppressed, false, "an absence is not a suppression: nothing was withheld, nothing was measured");
  assert.equal(s.concurrency, undefined);
  assert.equal(s.sweep, undefined);
  assert.equal(s.headroom, undefined, "and no fraction of a ceiling nothing was measured against");
  assert.equal(s.rig_ceiling, undefined);
});

// SITE-09. The legacy top-level memory reseal tested RSS_FIELD_RE only and passed no absent option: the
// narrower-whitelist bug (peak_rss_hwm_mib shipping bare) re-created one level up, plus a flattened
// absence reason on reseal.
test("SITE-09: the LEGACY top-level memory block seals by the same vocabulary and carries its absences", () => {
  const root = buildStreamMemRepo();
  const mpath = join(root, "results", "matrix", "sgw.json");
  const m = JSON.parse(readFileSync(mpath, "utf8"));
  m.memory.growth_rate_mib_per_min = 1.5;      // a metric that is NOT an RSS field
  m.memory.time_to_plateau_s = null;           // measured, and absent for a stated reason
  m.absences = { "memory.time_to_plateau_s": { reason: "below_resolution", detail: "settled inside one sample" } };
  writeFileSync(mpath, JSON.stringify(m));
  const g = genInto(root).gateways.find((x) => x.key === "sgw");
  const mem = g.matrix.memory;
  assert.equal(app.isEnvelope(mem.growth_rate_mib_per_min), true,
    "a non-RSS memory metric must not ship as a bare scalar off a legacy block");
  assert.equal(app.mval(mem.growth_rate_mib_per_min), 1.5);
  assert.equal(mem.time_to_plateau_s.reason, "below_resolution",
    "the engine's own reason must survive the reseal, not flatten to not_measured");
  assert.equal(mem.time_to_plateau_s.detail, "settled inside one sample");
  assert.equal(app.isEnvelope(mem.peak_rss_hwm_mib), true, "RSS discovery still applies");
});

// SITE-10. sealMatrixCellsInPlace rebuilt each cell's stream object from a fixed key list, which deleted
// the engine's `reason` prose and its c=1 note: the bundle published a refusal with its explanation
// removed. Seven reasons and five c1 notes in the recovered 2026-07-29 snapshots.
test("SITE-10: the in-place stream reseal carries the WHY (reason + c1 note), not only the status", () => {
  const root = buildStreamMemRepo();
  const mpath = join(root, "results", "matrix", "sgw.json");
  const m = JSON.parse(readFileSync(mpath, "utf8"));
  // The engine's shape: `reason` is the machine TOKEN (the Absent vocabulary), `stream_error` the prose.
  for (const cells of [m.cells, m.upstreams.openai.cells]) {
    cells.openai.stream = { ...cells.openai.stream, stream_served: "untestable",
      reason: "untestable",
      stream_error: "the rig cannot pose an SSE request in this dialect",
      stream_c1_note: "the c=1 leg answered, but never framed an event" };
  }
  writeFileSync(mpath, JSON.stringify(m));
  const g = genInto(root).gateways.find((x) => x.key === "sgw");
  const s = g.matrix.upstreams.openai.cells.openai.stream;
  assert.equal(s.stream_served, "untestable");
  assert.equal(s.reason, "untestable", "the machine token must survive the seal");
  assert.equal(s.stream_error, "the rig cannot pose an SSE request in this dialect",
    "and so must the prose behind it: a status with its explanation removed is an assertion");
  assert.equal(s.stream_c1_note, "the c=1 leg answered, but never framed an event");
  assert.equal(app.isEnvelope(s.streams_sustained), true, "and the metrics are still sealed");
  // the reader shows the PROSE, never the raw token
  const na = app.naText(s, "stream_served", "stream_error");
  assert.equal(na.text, "not testable");
  assert.equal(na.note, "the rig cannot pose an SSE request in this dialect");
});

/* SITE-12 IS DELETED, AND NOTHING SURVIVES IT.
   It asserted that on a legacy stream-suite fallback row, `cpu_fps` carried the provenance of the SEPARATE
   streamcpu suite that produced it (its own build and measured_at) rather than being dated to the stream
   suite's run, plus the reader disclosing that split with a "from a separate run" note.
   `cpu_fps` IS RETIRED. It counted relay frames/sec under an unpaced firehose WITHOUT the delivery gate, so
   a gateway dropping frames could post a higher rate than one delivering every frame - a loss rate with a
   numerator. No producer emits it and no surface renders it, so there is no metric left whose provenance
   could be mis-stamped.
   The per-envelope-stamp MACHINERY it exercised is not orphaned: `metric()` still composes the "from a
   separate run than the rest of this record" note from any envelope carrying its own `source`, and the
   surviving streaming metrics all come from one run today. If a future metric is ever again projected from a
   different suite than its record, this test is the shape to restore - against that metric. */

// SITE-13. StreamServed is `true`, `false`, or a STATUS TOKEN ("not_measured", "not_probed",
// "untestable"). Every non-true value fell through to "did not stream", which asserts a MEASURED refusal
// about cells the harness never offered anything to: two identical-looking n/a cells, different stories.
test("SITE-13: a lane that never ran does not read as a lane that refused", () => {
  const na = (status) => app.naText({ stream_served: status }, "stream_served", "stream_error").text;
  assert.equal(na(false), "did not stream", "a MEASURED refusal keeps its wording");
  assert.equal(na("not_measured"), "not measured", "never offered any stream load is not a refusal");
  assert.equal(na("not_probed"), "not measured");
  assert.equal(na("untestable"), "not testable", "a rig limit is not a gateway verdict");
  assert.equal(na("rig_limited"), "rig-limited");
  assert.equal(na("harness_error"), "harness error", "our own failure is not a gateway verdict either");
  assert.equal(na("search_exhausted"), "search exhausted");
  // an unknown token claims NOTHING rather than guessing between "never ran" and "refused", and it is
  // never printed verbatim: the machine vocabulary is ours, not the reader's.
  assert.equal(na("some_future_token"), "not available");
  assert.equal(app.naText({ stream_served: "some_future_token" }, "stream_served", "stream_error").note, "");
  // a known token with no prose still gets the vocabulary's full sentence on the tooltip
  assert.match(app.naText({ stream_served: "untestable" }, "stream_served", "stream_error").note, /rig cannot pose/i);
  assert.equal(app.naText(null, "stream_served", "stream_error").text, "not measured");
  // the predicate the render sites gate on agrees: only `true` is served
  assert.equal(app.laneServed({ stream_served: true }, "stream_served"), true);
  assert.equal(app.laneServed({ stream_served: "not_measured" }, "stream_served"), false,
    "a status token must not read as served, or the metric row renders numbers that do not exist");
  assert.equal(app.laneServed({ stream_served: false }, "stream_served"), false);
  assert.equal(app.laneServed({}, "stream_served"), true, "a record predating the flag stays served");
  assert.equal(app.laneServed(null, "stream_served"), false);
});

// SITE-14. The roster's desc toggle reversed the WHOLE comparison, so tied rows also reversed their name
// order: toggling a column with dense ties reshuffled rows whose values had not changed.
test("SITE-14: descending reverses the ranking, never the name tiebreak", () => {
  const col = { get: (g) => ({ v: g.v }) };
  const rows = [{ display: "charlie", v: 1 }, { display: "alpha", v: 1 }, { display: "bravo", v: 2 }];
  const names = (desc) => rows.slice().sort(app.rowComparator(col, desc)).map((r) => r.display);
  assert.deepEqual(names(false), ["alpha", "charlie", "bravo"]);
  assert.deepEqual(names(true), ["bravo", "alpha", "charlie"],
    "the tied pair keeps its alphabetical order in both directions");
  // missing values sink to the bottom in BOTH directions: an absent reading is not a low score
  const withNull = [{ display: "zulu", v: null }, { display: "alpha", v: 5 }];
  assert.deepEqual(withNull.slice().sort(app.rowComparator(col, true)).map((r) => r.display), ["alpha", "zulu"]);
  assert.deepEqual(withNull.slice().sort(app.rowComparator(col, false)).map((r) => r.display), ["alpha", "zulu"]);
  // and a string column sorts by name in the direction asked, ties impossible
  const scol = { get: (g) => ({ v: g.display }) };
  assert.deepEqual(rows.slice().sort(app.rowComparator(scol, true)).map((r) => r.display), ["charlie", "bravo", "alpha"]);
});

// SITE-16. bestIndex highlighted the FIRST of several tied bests, so two gateways both below resolution
// on a metric (both ranking 0, the same reading) showed one winner - a distinction the measurement says
// it cannot make.
test("SITE-16: every tied best is highlighted, because a tie is a tie", () => {
  const idx = (vals, best) => [...app.bestIndex(vals, best)].sort((a, b) => a - b);
  assert.deepEqual(idx([0, 0, 12], "min"), [0, 1], "two below-resolution zeros are equal-best");
  assert.deepEqual(idx([5, 9, 9], "max"), [1, 2]);
  assert.deepEqual(idx([5, null, 3], "min"), [2]);
  assert.deepEqual(idx([7, null], "min"), [], "one value is not a contest");
  assert.deepEqual(idx([1, 2], null), [], "an EVIDENCE row (best:null) crowns nobody");
});

// The empty-state's `view === "translation"` arm could not be taken: `view` is coerced to a TABLE_VIEWS
// member, and translation stopped being a tab when the pinned pair became a chooser MODE. A branch that
// cannot fire is not a safety net, it is a claim about the UI that stopped being true.
test("the roster's empty state has no unreachable translation arm", () => {
  assert.equal(app.TABLE_VIEWS.has("translation"), false);
  assert.equal(/view === "translation"/.test(readFileSync(join(HERE, "app.js"), "utf8")), false,
    "a branch keyed on a view the table cannot be in is dead code, not a fallback");
});

// TOOL-01. A gated test returned early from inside its own body, so the runner saw a function that did
// not throw and printed `ok - <name>` for a check that never asserted anything: on an empty board most
// of this suite read green, per test, with only a file-level warn to say otherwise.
test("TOOL-01: a skipped test is recorded as a skip, not counted as a pass, and does not fail the run", () => {
  const passedBefore = passed, skippedBefore = skipped.length, failedBefore = failures.length;
  skip("SELF-TEST probe (not a real check)", "proving a skip is visible and counted apart");
  assert.equal(skipped.length, skippedBefore + 1, "the skip is recorded");
  assert.equal(skipped[skipped.length - 1].why, "proving a skip is visible and counted apart",
    "with the reason it was skipped for");
  assert.equal(passed, passedBefore, "and is NOT counted as a pass");
  assert.equal(failures.length, failedBefore, "and is NOT a failure: exit semantics are unchanged");
  skipped.pop();   // leave the run's own tally honest
});

// SITE-15. sanitizeState seeded the data-derived Same dialect for the WHOLE state at boot, so a deep
// link into performance or streaming - tabs whose own default is the declared one, which is exactly why
// syncUrl omits ?d= on memory only - had its dialect rewritten from the data before it rendered a row.
test("SITE-15: the data-derived Same dialect is seeded for the MEMORY tab, not for every arrival", () => {
  const st = app.state;
  const saved = { view: st.view, d: st.sameDialect, pinned: st.sameDialectPinned, data: st.data };
  const gw = (key) => ({ key, display: key, lang: "Rust",
    matrix: { upstreams: { anthropic: { cells: { anthropic: { served: true } } } } } });
  try {
    st.data = { gateways: [gw("a"), gw("b")] };
    assert.equal(app.widestDialect(st.data), "anthropic", "fixture: the field's widest cell is anthropic");
    st.sameDialectPinned = false;
    st.sameDialect = "openai";
    st.view = "performance";
    app.seedMemorySameDialect();
    assert.equal(st.sameDialect, "openai", "a non-memory arrival keeps the dialect default it declares");
    st.view = "streaming";
    app.seedMemorySameDialect();
    assert.equal(st.sameDialect, "openai");
    st.view = "memory";
    app.seedMemorySameDialect();
    assert.equal(st.sameDialect, "anthropic", "and memory gets the data-derived default it asks for");
    st.sameDialect = "openai";
    st.sameDialectPinned = true;
    app.seedMemorySameDialect();
    assert.equal(st.sameDialect, "openai", "a ?d= in the URL still wins on memory");
  } finally {
    st.view = saved.view; st.sameDialect = saved.d; st.sameDialectPinned = saved.pinned; st.data = saved.data;
  }
});

console.log(`\n${passed} tests passed${skipped.length ? `, ${skipped.length} skipped (see the list below - a skip is not a pass)` : ""}`);

// ---- C8: THE BOARD IS GROUPED BY INSTRUMENT, NOT BY COMMIT SHA -----------------------------------
//
// C8's claim is that one instrument measured every column. The commit sha is a PROXY for that, and it
// is loose in exactly one direction: a commit can change while the instrument does not. A gateway's
// own config file, a test, a workflow - none of them can alter how a DIFFERENT gateway was measured,
// yet each moves the sha and each used to cost a full field re-run to satisfy a check that would have
// learned nothing. The proxy is never loose the other way, so attesting equivalence can only ever
// excuse a difference the built binaries prove is not there.
test("C8: commits the repo attests are one instrument, with artifact evidence, do not read as a mixed board", () => {
  const snapOf = (commit) => ({ snap: { rig: { engine: { commit, dirty: false } } } });
  const A = "a".repeat(40), B = "b".repeat(40);
  const resolve = (k) => snapOf(k === "plano" ? B : A);
  const keys = ["bifrost", "one-api", "plano"];

  // RED-before: with no attestation, two commits are two instruments and the board is refused. This
  // is the behaviour being preserved for every case that is NOT attested, so it is asserted first.
  const bare = checkMod.engineAgreement(keys, resolve, { equivalence: new Map() });
  assert.equal(bare.errors.length, 1, "an unattested mix must still fail");
  assert.match(bare.errors[0], /mixes 2 harness engines/);

  // GREEN-after: the same board, with the two commits attested as one instrument, publishes.
  const attested = checkMod.instrumentOf(JSON.stringify({
    instruments: [{ id: "otb-1fc78d7c", commits: [A, B],
      evidence: { otb_release_sha256: { [A]: "1fc78d7c", [B]: "1fc78d7c" } } }],
  }));
  const ok = checkMod.engineAgreement(keys, resolve, { equivalence: attested });
  assert.deepEqual(ok.errors, [], "commits proven to build the same binary are one instrument");
  assert.equal(ok.checked, 3);

  // An attestation whose evidence does NOT show identical binaries is inert. The file's rule is that
  // identical bytes admit an entry; a rule nothing enforces is a comment, and this is the case where
  // a real instrument change would otherwise be waved through by a hand-written claim.
  const unproven = checkMod.instrumentOf(JSON.stringify({
    instruments: [{ id: "wishful", commits: [A, B],
      evidence: { otb_release_sha256: { [A]: "1111", [B]: "2222" } } }],
  }));
  assert.equal(unproven.size, 0, "differing binaries are differing instruments, whatever the file claims");
  assert.equal(checkMod.engineAgreement(keys, resolve, { equivalence: unproven }).errors.length, 1);

  // No evidence block at all is equally inert.
  assert.equal(checkMod.instrumentOf(JSON.stringify({
    instruments: [{ id: "bare", commits: [A, B] }] })).size, 0);
  // Unparseable or absent file degrades to commit-equality, never to "everything agrees".
  assert.equal(checkMod.instrumentOf("{ not json").size, 0);
});

// ---- C8: the override is loud, reasoned, and published ------------------------------------------
//
// A publish guard sometimes has to be overridden. The failure mode of an override is that it is
// silent: a boolean env var flips, the board ships, and six months later nobody can say which numbers
// were published over an objection. So the override takes the REASON as its value and hands it back to
// the caller to publish. An override nobody can see after the fact is a disabled check.
test("C8: the mixed-board override demands a reason and returns it for publication", () => {
  const snapOf = (commit) => ({ snap: { rig: { engine: { commit, dirty: false } } } });
  const resolve = (k) => snapOf(k === "plano" ? "b".repeat(40) : "a".repeat(40));
  const keys = ["bifrost", "plano"];
  const run = (override) => checkMod.engineAgreement(keys, resolve, { equivalence: new Map(), override });

  assert.equal(run(undefined).errors.length, 1, "no override: the mix is refused");

  // A truthy-but-meaningless value is NOT an override. This is the "1"/"true" habit the check exists
  // to refuse, and it fails with a message saying what the field is for.
  const bogus = run("1");
  assert.equal(bogus.overridden, undefined, "a flag is not a justification");
  assert.match(bogus.errors.join("\n"), /not a reason/);
  assert.match(bogus.errors.join("\n"), /mixes 2 harness engines/, "and the original objection still stands");

  // A real reason publishes, and the objection travels WITH the result rather than vanishing.
  const forced = run("plano re-ran on a gateway-config-only commit; binaries verified identical");
  assert.deepEqual(forced.errors, []);
  assert.equal(forced.overridden.check, "C8.mix");
  assert.match(forced.overridden.reason, /binaries verified identical/);
  assert.match(forced.overridden.detail, /mixes 2 harness engines/,
    "what was overridden is recorded, not just that something was");
});

// ---- C9: n/a beats a mixed board, and "blank" must mean blank -----------------------------------
//
// OTB_SINGLE_ENGINE is the third answer to C8, between re-measuring the whole field and overriding
// the guard: show what the current engine measured, blank what it has not reached. That is only
// honest if suppression is TOTAL. A row that went n/a everywhere except one surviving number would
// put an older instrument's reading on the board with nothing marking it - strictly worse than the
// mix C8 refuses, because the mix at least declares itself. So the bundle's own suppression claim is
// verified rather than trusted: an exemption a bundle grants itself is not a check.
test("C9: a row claiming suppression must publish NOTHING, and say what it waits for", () => {
  const base = () => ({
    suppressed_for_engine: ["kong"],
    gateways: [{
      key: "kong", display: "Kong", awaiting_engine: "80030c2",
      engine: { sha: "a".repeat(40), short: "aaaaaaa", current: false },
      matrix: null, best_cell: null, rig: null, snapshot_file: null,
    }],
  });
  const c9 = (data) => checkMod.checkConsistency(data, {}, { syntheticFixture: true })
    .errors.filter((e) => e.startsWith("C9:"));

  assert.deepEqual(c9(base()), [], "a fully-blank suppressed row is fine");

  // ONE surviving measurement is the whole failure mode. Each of these is a separate door into it.
  for (const field of ["matrix", "best_cell", "translation_cell", "streaming", "snapshot_file", "rig"]) {
    const d = base();
    d.gateways[0][field] = { source: { kind: "matrix" } };
    const errs = c9(d);
    assert.equal(errs.length, 1, `a suppressed row keeping ${field} must be refused`);
    assert.match(errs[0], new RegExp(field), "and the message must name what leaked");
  }

  // A blank row owes the reader the DIFFERENCE between "measured nothing" and "not re-measured yet".
  const noWhy = base();
  delete noWhy.gateways[0].awaiting_engine;
  assert.match(c9(noWhy).join("\n"), /carries no awaiting_engine/);

  // Suppression and the row's own engine stamp cannot disagree about the same fact.
  const disagrees = base();
  disagrees.gateways[0].engine.current = true;
  assert.match(c9(disagrees).join("\n"), /suppression and the stamp disagree/i);

  // AND THE GUARD MUST NOT FIRE ON A BOARD THAT SUPPRESSES NOTHING - the normal, healthy state.
  const clean = base();
  clean.suppressed_for_engine = [];
  clean.gateways[0].matrix = { served: true };
  assert.deepEqual(c9(clean), [], "an unsuppressed row is C9's business not at all");
});

// ---- the shipped attestation is itself valid ----------------------------------------------------
// The file in the repo is data the publish path trusts, so it is tested like code: it must parse, and
// every entry in it must meet the evidence rule it states for itself.
test("C8: the repo's own instrument-equivalence.json meets the evidence rule it declares", () => {
  const raw = readFileSync(join(ROOT, "site", "instrument-equivalence.json"), "utf8");
  const doc = JSON.parse(raw);
  const admitted = checkMod.instrumentOf(raw);
  for (const inst of doc.instruments) {
    const hashes = Object.values((inst.evidence || {}).otb_release_sha256 || {});
    assert.equal(hashes.length, inst.commits.length,
      `${inst.id}: every attested commit needs a built-artifact hash, not a subset`);
    assert.equal(new Set(hashes).size, 1, `${inst.id}: the hashes must actually be identical`);
    assert.ok(inst.reason && inst.reason.length > 20, `${inst.id}: an entry states why it exists`);
    for (const c of inst.commits) {
      assert.match(c, /^[0-9a-f]{40}$/, `${inst.id}: commits are full shas, so they cannot be ambiguous`);
      assert.equal(admitted.get(c), inst.id, `${inst.id}: ${c.slice(0, 12)} must be admitted`);
    }
  }
});
// ---- a board whose age cannot be established is not a fresh board -------------------------------
// The 180-day floor ran only `if (boardNewest > 0)`, so the one case it could not judge was the one
// it waved through. That is the shape of every guard that turned out to be inert today: the retry
// budget nothing called, the box qualification that always seeded, the history appender scanning
// directories nothing writes. Silent, not wrong, which is why none were noticed.
// A gateway manifest with no results beside it: the shape both halves below start from.
function undatableRoot(withUnstampedMatrix) {
  const root = mkdtempSync(join(tmpdir(), "site-undatable-"));
  mkdirSync(join(root, "gateways", "alpha"), { recursive: true });
  writeFileSync(join(root, "gateways", "alpha", "definition.json"), JSON.stringify({
    name: "alpha", display: "alpha", lang: "Rust", class: "Gateway", model: "m", port: 1,
    path: "/v1/chat/completions", auth: "dummy", egress: ["openai"],
    matrix: ["100000", "000000", "000000", "000000", "000000", "000000"],
  }));
  mkdirSync(join(root, "results", "snapshots"), { recursive: true });
  if (withUnstampedMatrix) {
    // A matrix that SERVED - real cells, real numbers - and carries no measured_at anywhere. This is
    // the board the guard was written for: it publishes, and nobody can say how old what it publishes
    // is. Note the deliberate absence of `measured_at` at every level.
    mkdirSync(join(root, "results", "matrix"), { recursive: true });
    writeFileSync(join(root, "results", "matrix", "alpha.json"), JSON.stringify({
      served: true,
      upstreams: { openai: { cells: { openai: { served: true, perf: {
        added_latency_p50_us: 100, added_latency_p99_us: 200,
        frontier: rawFrontier({ 10: 1000, none: 1200 }),
      } } } } },
    }));
  }
  return root;
}
test("freshness guard REFUSES a board it cannot date, rather than passing it", () => {
  // A gateway that PUBLISHES NUMBERS with no resolvable displayed stamp anywhere. Its row will show
  // measurements the board cannot age, which is exactly what publishing generated_at=now over would
  // misrepresent, so it is still a hard failure and its strictness is unchanged.
  const msg = genThrows(undatableRoot(true));
  assert.ok(msg, "expected gen-data to THROW on a board that publishes numbers it cannot date, but it succeeded");
  assert.match(msg, /FRESHNESS FAILURE \(undatable board\)/, `expected the undatable-board failure, got: ${msg}`);
});
// ---- ...but an EMPTY board is a legitimate state, and the guard must not eat it -------------------
//
// "Undatable" and "empty" both make boardNewest 0, and the guard originally asked only that question -
// so it hard-failed a board where NOTHING had been benchmarked, which is not ambiguous at all: there
// is nothing to date, and the honest bundle says n/a on every row (the whole BOARD_HAS_DATA family at
// the top of this file exists to describe exactly that state).
//
// The cost was not cosmetic. A clean checkout commits no artifacts, so results/snapshots/ is empty and
// gen-data threw on every fresh clone - which meant THIS FILE died at its own gen-data call, above,
// before its first assertion: zero ok lines, zero FAIL lines, and a site suite that gated nothing in
// CI while reading as "the job failed for an unrelated reason". The committed-data.json fallback could
// not help because site/data.json is gitignored. This test is the guard on the guard.
test("freshness guard PUBLISHES a board with nothing measured on it (empty is not undatable)", () => {
  const { data, err } = genData(undatableRoot(false));
  assert.ok(!err, `a board with no measurements at all must publish an honest empty bundle, not throw: ${err}`);
  assert.equal(data.gateways.length, 1, "the declared gateway is still on the board, reading n/a");
  assert.ok(!data.gateways[0].best_cell, "with nothing measured there is no projected record to show");
  assert.ok(data.generated_at, "the bundle still stamps when it was generated");
});
// ---- a gateway NOBODY HAS MEASURED is not a row, once anything else has been -----------------------
//
// busbar-150's manifest had to be committed for the benchmark boxes to fetch it, and the moment it
// existed the PUBLIC board grew a row reading "Busbar 1.5.0" with no matrix, no best_cell and no
// snapshot behind it - beside fourteen rows carrying full 6x6 grids. That is not a disclosure, it is
// an implication: the reader sees a gateway that apparently has nothing to show, when the truth is
// that nobody has run it yet. An absence on this board carries a reason; a gateway with no
// measurement at all has no absence to explain, because there is no cell to be absent.
//
// The rule is scoped to boards that HAVE data - the test above pins the all-empty board keeping its
// declared rows, and these two must not be collapsed into one rule in either direction.
test("a declared-but-unmeasured gateway is off a board that has data (n/a beside real rows implies)", () => {
  const root = mkdtempSync(join(tmpdir(), "site-unmeasured-"));
  for (const name of ["alpha", "ghost"]) {
    mkdirSync(join(root, "gateways", name), { recursive: true });
    writeFileSync(join(root, "gateways", name, "definition.json"), JSON.stringify({
      name, display: name, lang: "Rust", class: "Gateway", model: "m", port: 1,
      path: "/v1/chat/completions", auth: "dummy", egress: ["openai"],
      matrix: ["100000", "000000", "000000", "000000", "000000", "000000"],
    }));
  }
  mkdirSync(join(root, "results", "snapshots"), { recursive: true });
  // alpha measured; ghost declared and never run.
  mkdirSync(join(root, "results", "matrix"), { recursive: true });
  writeFileSync(join(root, "results", "matrix", "alpha.json"), JSON.stringify({
    served: true, measured_at: "2026-08-02T00:00:00Z",
    upstreams: { openai: { cells: { openai: { served: true, measured_at: "2026-08-02T00:00:00Z", perf: {
      added_latency_p50_us: 100, added_latency_p99_us: 200,
      frontier: rawFrontier({ 10: 1000, none: 1200 }),
    } } } } },
  }));
  const { data, err } = genData(root);
  assert.ok(!err, `expected an honest board, got: ${err}`);
  const keys = data.gateways.map((g) => g.key || g.name);
  assert.ok(keys.includes("alpha"), "the measured gateway must be on the board");
  assert.ok(!keys.includes("ghost"),
    `a gateway nobody has measured must not be a row beside measured ones, got: ${JSON.stringify(keys)}`);
});
// ---- the rig's ceiling is a FACT ABOUT THE COMPARISON, never a reason to withhold ------------------
// This test's title used to be "matching a PACED upstream publishes the value; matching a CAPACITY still
// suppresses", and it pinned exactly that split: a stream metric at the mock's paced rate published,
// while a throughput metric at the rig's capacity was replaced with {value:null, reason:"mock_bound"}, on
// the reasoning that publishing it "would rank the rig rather than the gateway".
//
// The split was the wrong shape of answer. Both halves are the same situation - a measurement taken
// against equipment that has a ceiling of its own - and in both the number is correct; what differs is
// only how much weight it will bear. Suppressing the throughput half deleted correct measurements and
// deleted the most of them from the gateways that performed best, while the reader was given no way to
// tell a withheld number from one that was never taken. There is no split now: a present number is always
// published, with the ceiling it was taken against and the fraction of it reached, and the reader does
// the weighing the seal used to do on their behalf and without telling them.
test("seal: a measurement at the rig's own ceiling is PUBLISHED, with the ceiling and the fraction reached", () => {
  // Stream metrics: the mock paces deltas, so its frames/sec is a TARGET rate. Reaching it is the gateway
  // keeping up - 24 of 69 cells were deleted for exactly this in the 2026-07-28 run.
  const paced = sealMetric(12275, { headroom: 0.997, ceiling: 12312 });
  assert.equal(paced.value, 12275, "a gateway that kept pace must publish its rate");
  assert.equal(paced.certified, true);
  assert.equal(paced.suppressed, false);
  assert.equal(paced.headroom, 0.997);
  assert.equal(paced.rig_ceiling, 12312);

  // Throughput metrics: the SAME rule, which is the whole change. A saturating load really can hit the
  // rig's capacity rather than the gateway's, and that is what the 0.997 says - it is stated, not acted on.
  const capacity = sealMetric(12275, { headroom: 0.997, ceiling: 12312 });
  assert.equal(capacity.value, 12275, "a near-ceiling throughput number is published, not withheld");
  assert.equal(capacity.suppressed, false, "the SUPPRESSED envelope shape no longer exists at all");
  assert.equal(capacity.reason, undefined, "and a certified number carries no absence reason");
  assert.deepEqual(capacity, paced, "there is no longer a paced path and a capacity path - there is one rule");

  // AN UNMEASURABLE REFERENCE used to suppress on both paths ("certifying a number on no evidence is what
  // the gate exists to prevent"). It was the number that had the evidence; the missing evidence was for
  // the INTERPRETATION, so it is the interpretation that goes missing - the fraction is simply not stated.
  const unreferenced = sealMetric(12275, { headroom: null, ceiling: null });
  assert.equal(unreferenced.value, 12275, "an unusable reference costs the fraction, not the measurement");
  assert.equal(unreferenced.certified, true);
  assert.equal(unreferenced.headroom, undefined);
  assert.equal(unreferenced.rig_ceiling, undefined);

  // A clean, comfortably-under-the-ceiling reading is certified exactly as it always was.
  assert.equal(sealMetric(500, { headroom: 0.02, ceiling: 25000 }).value, 500);
  assert.equal(sealMetric(500).value, 500);
});
// ---- the benchmark version is visible, and an older one is loud -------------------------------
// The engine commit travelled into every row from the start and nothing rendered it, so "which
// version of the benchmark produced this number" was answerable only by opening the JSON. A row
// measured by an older harness is not necessarily wrong, but it is not comparable to the rest, and
// that has to be visible rather than known only to the build guard that refuses mixed boards.
test("benchmark version: current rows are quiet, older rows are marked red with what they should be", () => {
  const board = "8f2af5ddc8980fce326ef140e4f75de36e8cfc72";
  const current = { key: "a", measured_at: new Date().toISOString(),
    engine: { sha: board, short: "8f2af5d", current: true } };
  const behind = { key: "b", measured_at: new Date().toISOString(),
    engine: { sha: "dd26a545026be44a0c38589242859138df05b2eb", short: "dd26a54", current: false } };

  const cur = app.engineBadge(current, board);
  assert.match(cur, /8f2af5d/, "a current row still shows which harness measured it");
  assert.ok(!/engine-pill old/.test(cur), "and is not flagged");

  const old = app.engineBadge(behind, board);
  assert.match(old, /engine-pill old/, "a row measured by an older harness must be marked");
  assert.match(old, /dd26a54/, "showing the version it WAS measured on");
  assert.match(old, /8f2af5d/, "and the version the board is on, so the reader can see the gap");
  assert.match(old, /not directly comparable/, "and why that matters");

  // A row with no stamp at all predates the engine stamp entirely. Saying so beats implying it
  // matches, and beats omitting the badge so the reader assumes it does.
  const unstamped = app.engineBadge({ key: "c", engine: { sha: null, short: null, current: false } }, board);
  assert.match(unstamped, /engine unknown/);
  assert.match(unstamped, /engine-pill old/);

  // Nothing to compare against: no board version and no row version renders nothing rather than an
  // empty pill.
  assert.equal(app.engineBadge({ key: "d", engine: { sha: null, short: null, current: false } }, null), "");
  assert.equal(app.engineBadge({ key: "e" }, board), "", "a row with no engine field at all renders nothing");
});


// ================================================================================================
// THE THREE SURFACES NOTHING WAS WATCHING (round-2 audit)
//
// Each of these was verified to be CORRECT in the source and to produce ZERO test failures when
// deliberately broken. A fix nothing can go red for is a fix that lasts until the next refactor.
// ================================================================================================

// ---- the drawer ---------------------------------------------------------------------------------
// drawerHtml() was called by NO test in this suite. It is the surface a reader opens to see the
// evidence behind a row - every lane, every metric, the provenance stamp and the failure notes - and
// nothing asserted a single character of it. Concretely: the drawer's metric list filters out absent
// metrics with `.filter((x) => !x.c.na || x.c.failed)`, and a MEASURED FAILURE is `na: true,
// failed: true` - so deleting `|| x.c.failed` deletes a gateway's worst measured result from the one
// place a reader goes looking for it, and no test noticed.
const FAIL_DETAIL = { reason: "not_measured", detail: "the gateway leg at c=1 was not clean: 0 ok, 14201 fail" };
const failEnv = () => sealMetric(null, { absent: FAIL_DETAIL });
/* The drawer fixture carries FOUR DISTINCT STATES on one record, because the drawer's job is to render
   them apart and each pair of them is one substitution away from being indistinguishable:
     a measured FAILURE   - the 1 ms reading's rate: the harness ran and the gateway failed everything
     a below-RESOLUTION   - the p50 added latency: the comparison ran and the answer was under the floor
     a real reading       - the 10 ms reading
     a LOWER BOUND        - the unbounded reading: a floor, not a ceiling
   The fourth replaces the retired SUPPRESSION state (a sealed envelope withholding a number it had). It
   is genuinely new, it is the state the frontier introduced, and rendering it as a ceiling would be the
   same class of error the suppression was: a surface making a claim the measurement does not support. */
const drawerGw = () => ({
  key: "dgw", display: "Drawer GW", lang: "Rust", cls: "AI proxy",
  repo: "https://github.com/example/dgw",
  measured_at: "2026-07-25T00:00:00Z",
  best_cell: bcCell({ dialect: "openai", frontier: { 1: null, 10: 20000, none: 22000 },
    frontierOpts: { absent: FAIL_DETAIL, lowerBound: ["none"] } }),
});
/* A column's label/title may be a FUNCTION (a header whose wording depends on the selected bound or on a
   tunable harness window). txtOf resolves either form, so a guard can scan every column's real copy. */
const txtOf = (v) => (typeof v === "function" ? String(v()) : String(v ?? ""));
const drawerState = (g, over = {}) => ({ ...app.newState(), view: "performance", mode: "peak",
  data: { gateways: [g] }, ...over });

test("drawer: the four states render APART - failure, below-resolution, a reading, and a floor", () => {
  const g = drawerGw();
  const h = app.drawerHtml(g, drawerState(g));
  // (1) A MEASURED FAILURE keeps its counts and its red class, under the label of the reading that failed.
  assert.match(h, /failed · 0\/14,201/,
    "a measured failure must appear in the drawer with its counts - it is a result, not an absence");
  assert.match(h, /class="failtext"/, "and it is marked as a failure, not rendered as an ordinary value");
  assert.match(h, /99% under 1 ms/, "under the label of the reading that failed, which names its bound");
  assert.match(h, /the gateway leg at c=1 was not clean/, "with the engine's own evidence on the tooltip");
  // (2) A real reading renders as its number, with the concurrency it was observed at.
  assert.match(h, /20,000/, "the measured reading is the number that was measured");
  // (3) A LOWER BOUND renders as a FLOOR. A bare "22,000" beside the others would state a maximum the
  // sweep never established - it ran out of ladder with that concurrency still qualifying.
  assert.match(h, /≥ 22,000/, "a reading whose sweep ran out of ladder is a floor, and says so");
  assert.match(h, /FLOOR/, "and the tooltip explains what the glyph means");
  // (4) THE CURVE, under the numbers: the shape is the finding, so the drawer draws it rather than
  // leaving the reader to plot six rows in their head.
  assert.match(h, /frontier-spark/, "the drawer carries the cell's own curve");
  // The distinction the failure clause exists to preserve: a genuinely ABSENT reading is still filtered
  // out, so this is not "show everything" - it is "a failure is not an absence".
  const absent = drawerGw();
  app.frontierAt(absent.best_cell.frontier, 1).rps = sealMetric(null);
  const h2 = app.drawerHtml(absent, drawerState(absent));
  assert.ok(!/failed · /.test(h2), "a never-measured reading is omitted; only a measured failure earns a row");
  assert.ok(!/99% under 1 ms/.test(h2), "and its label goes with it");
});

test("drawer: the surface renders - name, class, lanes, values and provenance", () => {
  const g = drawerGw();
  const h = app.drawerHtml(g, drawerState(g));
  assert.match(h, /<a href="https:\/\/github\.com\/example\/dgw"/, "the head links the gateway's repo");
  assert.match(h, /Drawer GW/);
  assert.match(h, /AI proxy/, "the class chip");
  assert.match(h, /Rust/, "the language chip");
  for (const lane of app.LANES)
    assert.ok(h.includes(lane.label.replace(/&/g, "&amp;")), `the ${lane.key} lane has a section`);
  assert.match(h, /Added latency p99/, "a measured lane lists its metrics");
  assert.match(h, /6x6|diagonal|matrix/i, "and discloses where the numbers came from");
  // A lane with no record says so, rather than rendering an empty section.
  assert.match(h, /not measured/, "the lanes this gateway never ran read 'not measured'");
});

// ---- the compare table --------------------------------------------------------------------------
// renderCompare() reached for document.getElementById on its first useful line and the suite has no
// DOM, so the entire compare surface was structurally untestable: the one place three gateways' numbers
// sit side by side and a winner is declared, covered by nothing. The row-building half is now
// compareBodyHtml(gws, st), a pure function, and this is the regression the audit named: routing those
// cells through mval() instead of metric() collapses states the table renders apart - a measured
// failure becomes a bare n/a with no evidence, below-resolution's "≈0" becomes a plain 0, and an absence
// loses the engine's reason for it.
test("compare: the table renders through metric(), so a bare-mval read cannot collapse the states", () => {
  const failing = { key: "a", display: "Alpha", lang: "Rust", cls: "AI proxy",
    best_cell: { ...bcCell({ dialect: "openai", added_latency_p99_us: 110,
      // A measured FAILURE on one reading, and a FLOOR on another: two states the compare table must
      // render apart from each other and from an ordinary number.
      frontier: { 1: null, 10: 20000, none: 22000 },
      frontierOpts: { absent: FAIL_DETAIL, lowerBound: ["none"] } }),
      // below-resolution: the comparison RAN and the difference was under what the rig can weigh.
      added_latency_p50_us: sealMetric(null, { absent: { reason: "below_resolution",
        detail: "the difference was at or below what the rig can resolve" } }) } };
  const bound = { key: "b", display: "Beta", lang: "Go", cls: "AI proxy",
    best_cell: { ...bcCell({ dialect: "openai", added_latency_p99_us: 500 }),
      // A metric the ENGINE reports as rig-limited: an absence with a stated reason, and the reason is
      // the disclosure. This used to be `sealMetric(32000, {gated: true, flag: true})` - a SUPPRESSED
      // envelope, the seal hiding a number it had. That shape is gone (the 32,000 would now publish), and
      // with it went the only fixture in this test that produced a reason to render.
      //
      // `rig_limited` IS A DECLARED TOKEN WITH NO CURRENT PRODUCER, and this comment used to call it a
      // "live engine absence token", which is not true: `Absent::RigLimited` is constructed nowhere in
      // the engine outside test modules (checked across measurement.rs, suite.rs, run.rs, record.rs).
      // It stays as the fixture because what is under test is the SITE's behaviour - a no-number cell
      // must disclose WHY rather than vanishing into the same n/a an unmeasured cell gets - and that
      // property holds for any reason token the engine may emit. What must not stand is a test comment
      // asserting a fact about the engine that stopped being true.
      gateway_c1_p99_us: sealMetric(null, { absent: { reason: "rig_limited" } }) } };
  const st = { ...app.newState(), view: "performance", mode: "peak",
    data: { gateways: [failing, bound] }, cmp: ["a", "b"] };
  const h = app.compareBodyHtml([failing, bound], st);

  assert.match(h, /Alpha/); assert.match(h, /Beta/);
  // (1) A MEASURED FAILURE keeps its counts and its red class. Under a bare mval() read this cell is
  // `null` and renders as an indistinguishable n/a.
  assert.match(h, /failed · 0\/14,201/, "a measured failure shows its counts in the compare table");
  assert.match(h, /class="na failcell"/, "and is marked red, not folded in with the untested cells");
  // (2) BELOW-RESOLUTION reads ≈0, which is a RESULT (equal-best), not a hole and not a plain 0.
  assert.match(h, /≈0/, "below-resolution renders as ≈0 - the best answer the comparison can express");
  // (3) A metric with NO NUMBER carries its reason on the tooltip rather than vanishing silently. The
  // assertion is anchored on the engine reason's own prose, not on the word "mock": every perf column
  // title in this table says "direct-to-mock", so a loose /mock/i matched the table's furniture and
  // passed no matter what the cell rendered.
  // (The prose is HTML-escaped into the title attribute, so the match avoids its apostrophe.)
  assert.match(h, /own ceiling bounded this number/,
    "a cell with no number discloses WHY, in the reason's own words");
  // (4) The contest is still called, on the metric that both gateways measured.
  assert.match(h, /class="best"/, "the winning cell per row is highlighted");
  // (5) ...and the winner is the one the measurement picks (lower added latency).
  const p99Row = h.split("<tr>").find((r) => r.includes("Added latency p99"));
  assert.ok(p99Row, "the p99 row exists");
  // split("<td") yields [prefix, metric-label cell, Alpha's cell, Beta's cell].
  const bestCol = p99Row.split("<td").findIndex((c) => c.includes('class="best"'));
  assert.equal(bestCol, 2, `lower added latency wins the row (Alpha); got column ${bestCol}`);
});

// ================================================================================================
// THE FRONTIER SURFACES: the bound is named, switchable, and re-ranks the board
//
// The retired board's failure was not a wrong number, it was a caption: every surface described the
// throughput gate as "p99 < 1 s" while the engine enforced 20 ms - a bar 96% of the 1632 recorded rungs
// pass, against 57% for the real one - so a reader who reasoned carefully about our numbers reasoned about
// a test we never ran. Everything below is a guard on the property that replaced it: the bound the board is
// showing is DECLARED, the reader can change it, and changing it changes the ranking in front of them.
// ================================================================================================

test("FRONTIER: the board's bound vocabulary MIRRORS seal.mjs, which mirrors the engine", () => {
  // app.js is loaded as a plain <script> and cannot import the module, so the list is duplicated. A second
  // source of truth is only acceptable while something checks it: a bound added to the engine and to
  // seal.mjs must not leave the board rendering a table one column short, silently.
  assert.deepEqual(app.FRONTIER_BOUNDS_MS, FRONTIER_BOUNDS_MS,
    "the board's declared bounds must equal seal.mjs's, which mirror frontier::P99_BOUNDS_US");
  assert.equal(app.DEFAULT_BOUND_MS, DEFAULT_BOUND_MS, "and so must the bound the board opens on");
  // The published order: every declared bound ascending, then the UNBOUNDED reading, which is a real
  // choice (`null`) and not an unset value.
  assert.deepEqual(app.BOUND_CHOICES, [...FRONTIER_BOUNDS_MS, null]);
  for (const w of app.FRONTIER_BOUNDS_MS.slice(1).map((b, i) => [app.FRONTIER_BOUNDS_MS[i], b]))
    assert.ok(w[0] < w[1], `bounds must ascend: ${app.FRONTIER_BOUNDS_MS}`);
  // The frontier tab's default sort is the DEFAULT BOUND's own column, written out as a literal because
  // VIEW_SORT is initialised before the constant exists. This is the check that keeps them agreeing.
  assert.equal(app.VIEW_SORT.frontier, app.boundColId(app.DEFAULT_BOUND_MS));
});

test("FRONTIER: every surface NAMES the bound it is showing, and none can imply one it did not use", () => {
  // THE PHRASING WAS SETTLED WITH THE OWNER: "18,995 req/s while 99% of requests finished under 10 ms".
  // Not "rps at 10 ms", which reads as a category error - the bound is not a rate.
  for (const b of app.FRONTIER_BOUNDS_MS) {
    assert.equal(app.boundClause(b), `while 99% of requests finished under ${b} ms`);
    assert.match(app.boundColLabel(b), new RegExp(`99% under ${b} ms`));
  }
  // The unbounded reading makes NO latency claim, and says so rather than borrowing the clause.
  assert.match(app.boundClause(null), /no latency bound at all/);
  assert.match(app.boundClause(null), /failed no request it accepted/);
  assert.ok(!/99%/.test(app.boundColLabel(null)), "the unbounded column must not imply a percentile bound");
  // NO SURFACE MAY STATE A BOUND AS A BARE NUMBER-AND-UNIT: the column header, the tooltip, the caption
  // and the popup all render their clause from boundClause(), so there is one sentence to be wrong.
  const col = app.COLUMN_SETS.performance.find((c) => c.id === "rps");
  const prev = app.state.bound;
  try {
    for (const b of app.BOUND_CHOICES) {
      app.state.bound = b;
      assert.equal(String(col.label()), app.boundColLabel(b), `the header renames itself for ${app.boundLabel(b)}`);
      assert.ok(String(col.title()).includes(app.boundClause(b)), "and the tooltip states the same clause");
      assert.ok(app.captionText(app.frontierCaption(app.state, null)).includes(app.boundLabel(b)),
        "and the Frontier caption names the column it has marked");
    }
  } finally { app.state.bound = prev; }
});

test("FRONTIER: the bound is in the URL, is a fixed point, and a bound the board never used is refused", () => {
  // A shared link reproduces the reading it was shared at, on the tabs whose numbers ARE read at a bound.
  for (const view of ["performance", "frontier"]) {
    for (const b of app.BOUND_CHOICES) {
      const st = { ...app.newState(), view, bound: b, sortCol: app.VIEW_SORT[view] };
      if (view === "frontier") st.sortCol = app.boundColId(b);
      const url = app.encodeUrl(st);
      const back = app.decodeUrl(...(() => { const u = new URL(url, "https://x.invalid"); return [u.pathname, u.search]; })());
      assert.equal(back.bound, b, `${url}: the bound survives the round trip`);
      assert.equal(app.encodeUrl(back), url, `${url}: and the URL is a fixed point`);
    }
  }
  // The DEFAULT is omitted, so the pristine link stays clean...
  assert.ok(!app.encodeUrl({ ...app.newState(), view: "performance" }).includes("bound="));
  // ...and the unbounded reading is spelled out rather than encoded as an empty value.
  assert.match(app.encodeUrl({ ...app.newState(), view: "performance", bound: null }), /bound=none/);
  // A BOUND THE BOARD DOES NOT PUBLISH IS IGNORED, not honoured. 20 ms is the retired gate's ceiling and
  // the value a reader is most likely to try by hand; rendering a column labelled "99% under 20 ms" over
  // readings taken at 10 ms would be the original defect, re-created by a URL parameter.
  assert.equal(app.decodeUrl("/gateways/performance", "?bound=20").bound, app.DEFAULT_BOUND_MS);
  assert.equal(app.decodeUrl("/gateways/performance", "?bound=abc").bound, app.DEFAULT_BOUND_MS);
  assert.equal(app.decodeUrl("/gateways/performance", "?bound=").bound, app.DEFAULT_BOUND_MS);
  // The selector is offered ONLY where something is read at a bound: a control over the memory columns
  // would imply those numbers had a tail-latency bound too.
  // Charts joins them: its throughput and dollar metrics are READ AT A BOUND, and a chart that ignored
  // the selector while sitting beside tables that honour it would be showing a different question.
  assert.deepEqual([...app.BOUND_VIEWS].sort(), ["charts", "frontier", "performance"]);
  for (const v of ["streaming", "memory"]) assert.ok(!app.BOUND_VIEWS.has(v), `${v} is not read at a bound`);
  assert.ok(!app.encodeUrl({ ...app.newState(), view: "memory", bound: 1 }).includes("bound="),
    "a memory link must not carry a bound it does not use");
});

test("FRONTIER: switching the bound RE-RANKS the board, in front of the reader", () => {
  /* THE LOAD-BEARING BEHAVIOUR. The whole claim of the frontier is that a gateway's position depends on the
     tail you are willing to accept; a selector that changed only the digits in place would leave that claim
     unmade. These two gateways are the field's two shapes, from the 2026-07-29 board: agentgateway holds
     23,630 under a 1 ms tail and gains 7% unbounded; apisix carries 10,697 at 1 ms and nearly doubles by
     5 ms. Ranked at 1 ms the flat one wins; ranked unbounded the steep one does. */
  const flat = { key: "flat", display: "Flat GW", lang: "Rust",
    best_cell: bcCell({ dialect: "openai", frontier: { 1: 23630, 5: 24712, 10: 25158, none: 25290 } }) };
  const steep = { key: "steep", display: "Steep GW", lang: "Go",
    best_cell: bcCell({ dialect: "openai", frontier: { 1: 10697, 5: 19339, 10: 20352, none: 26000 } }) };
  const rows = [flat, steep];
  const rank = (bound) => {
    const st = { ...app.newState(), view: "performance", mode: "peak", bound, data: { gateways: rows } };
    const col = app.COLUMN_SETS.performance.find((c) => c.id === "rps");
    return rows.slice().sort(app.rowComparator({ ...col, get: (g) => col.get(g, st) }, true, null)).map((g) => g.key);
  };
  assert.deepEqual(rank(1), ["flat", "steep"], "at a 1 ms tail the gateway that holds its rate wins");
  assert.deepEqual(rank(null), ["steep", "flat"], "with no bound at all the ranking INVERTS");
  // ...and the two are not the same machine, which is what the shape column says in one number.
  const heldOf = (g, bound) => app.frontierShapeCell(g, { ...app.newState(), mode: "peak", bound });
  // THE BOUND IS PART OF THE TEXT, always, including on the 1 ms rows: a reader must never have to know
  // which bound is the default to know whether "93%" means "at the tightest tail we publish" or "at 50 ms".
  assert.equal(heldOf(flat, 10).text, "93% of its full rate at 1 ms", "flat: it keeps nearly all of it under a 1 ms tail");
  assert.equal(heldOf(steep, 10).text, "41% of its full rate at 1 ms", "steep: it needs a loose tail to go fast");
  // BIGGER IS BETTER now that the column states a share rather than a gain, so the column's descending
  // default puts the gateway that holds its rate on top - the direction the header's own words point.
  assert.ok(heldOf(flat, 10).v > heldOf(steep, 10).v, "so the shape column separates them at any bound");
  // The share is bound-INDEPENDENT (it is a property of the whole curve), which is why it is a stable
  // second ranking rather than a third reading of the selected bound.
  assert.equal(heldOf(steep, 1).text, heldOf(steep, null).text);
  // AND SELECTING A BOUND MOVES THE FRONTIER TAB'S SORT ONTO THAT BOUND'S COLUMN, so the re-rank is
  // visible rather than merely available.
  const st = { ...app.newState(), view: "frontier", bound: 10, sortCol: app.boundColId(10) };
  Object.assign(app.state, st, { data: { gateways: rows } });
  try {
    app.selectBound(1);
    assert.equal(app.state.sortCol, app.boundColId(1), "the ranking follows the reader's selection");
    // ...unless they had deliberately ranked by something else, which is their choice to keep.
    app.state.sortCol = "shape";
    app.selectBound(50);
    assert.equal(app.state.sortCol, "shape", "a deliberate sort is not overwritten by a bound change");
  } finally { Object.assign(app.state, app.newState()); }
});

test("FRONTIER: the tab publishes every reading as its own column, marked at the selected bound", () => {
  const cols = app.COLUMN_SETS.frontier;
  // ONE COLUMN PER PUBLISHED READING, in the engine's own order, each labelled with its own bound.
  for (const b of app.BOUND_CHOICES) {
    const c = cols.find((x) => x.id === app.boundColId(b));
    assert.ok(c, `the frontier tab has a column for ${app.boundLabel(b)}`);
    /* THE SHARED WORDS ARE STATED ONCE, IN A SPANNING GROUP HEADER, and each sub-header carries only what
       differs. Six headers each reading "Req/s · 99% under N ms" was the same five words six times across
       the widest table on the board. The 99% qualifier has NOT been dropped - it moved into the group
       header, which is now the only place on that table it appears, so this asserts both halves. */
    assert.equal(String(c.label), app.boundLabel(b), "the sub-header carries only its own bound");
    assert.equal(c.group, app.BOUND_GROUP_LABEL, "and shares the one spanning group header");
    assert.ok(String(c.title).includes(app.boundClause(b)), "and states its own clause, not a shared one");
  }
  assert.match(app.BOUND_GROUP_LABEL, /Req\/s/, "the group header names the quantity");
  assert.match(app.BOUND_GROUP_LABEL, /99% of requests under/, "and carries the 99% qualifier the sub-headers no longer repeat");
  assert.ok(cols.some((c) => c.id === "shape"), "plus the curve, so the shape is legible without reading six numbers");
  // A row with a SHAPE: the six cells are the six readings, and each carries the tail it ACTUALLY produced.
  const g = { key: "s", display: "S", lang: "Rust",
    best_cell: bcCell({ dialect: "openai", frontier: { 1: 7015, 5: 15438, 10: 18943, 50: 19284, none: 19284 },
      frontierOpts: { conc: 64 } }) };
  const st = { ...app.newState(), view: "frontier", mode: "peak", data: { gateways: [g] } };
  assert.equal(cols.find((c) => c.id === "f1").get(g, st).v, 7015);
  assert.equal(cols.find((c) => c.id === "f5").get(g, st).v, 15438);
  assert.equal(cols.find((c) => c.id === "fnone").get(g, st).v, 19284);
  // 100 ms has no reading (the engine omits a bound no rung qualified at): n/a, with the bound named.
  const at100 = cols.find((c) => c.id === "f100").get(g, st);
  assert.equal(at100.na, true);
  assert.match(at100.note, /no reading at 100 ms/);
  // The OBSERVED TAIL rides under the number, because 4 ms under a 100 ms bound and 99 ms under it are
  // different findings and a column of rates alone cannot tell them apart.
  const td = cols.find((c) => c.id === "f10").render(g, st);
  assert.match(td, /tail 4\.0 ms/, `the observed tail is on the cell; got ${td}`);
  assert.ok(!/10 ms<\/span>/.test(td), "and it is the tail, never the bound echoed back");
  // THE SELECTED BOUND'S COLUMN IS MARKED, so the number the Performance tab ranks is locatable here.
  assert.match(cols.find((c) => c.id === "f10").render(g, { ...st, bound: 10 }), /bound-col/);
  assert.ok(!/bound-col/.test(cols.find((c) => c.id === "f1").render(g, { ...st, bound: 10 })));
});

test("FRONTIER: the curve is drawn on a SHARED log scale, with three distinguishable markers", () => {
  /* THE SHAPE HAS TO BE LEGIBLE WITHOUT READING NUMBERS, which is what the sparkline is for, and the scale
     is what makes it honest. Shared, so two rows are comparable; logarithmic, so equal slopes are equal
     ratios (the ratio IS the finding) and the slowest gateway on the board is still visible - the field
     spans litellm-rust at 44,363 req/s and plano at 19, which no shared linear axis can show at once. */
  const board = { gateways: [
    { key: "fast", best_cell: bcCell({ dialect: "openai", frontier: { 1: 40000, none: 44363 } }) },
    { key: "slow", best_cell: bcCell({ dialect: "openai", frontier: { none: 19 } }) },
  ] };
  const scale = app.boardFrontierScale(board);
  assert.deepEqual(scale, { min: 19, max: 44363 }, "the domain spans the whole board, not one row");
  // A SLOW ROW IS STILL A SHAPE, not an empty frame with one dot: its curve is drawn on the same scale and
  // its points are placed, so "slow" reads as slow rather than as broken.
  const slow = app.frontierSpark(board.gateways[1].best_cell.frontier, { ...scale, boundMs: 10 });
  assert.match(slow, /<path d="M/, "the slow row still draws a path");
  assert.match(slow, /log scale/, "and says which scale it is on, for a screen reader");
  // THE THREE MARKERS, THREE CLAIMS. A ceiling, a floor, and a bound the gateway served but could not hold.
  const three = [
    { bound_ms: 1, rps: sealMetric(null, { absent: { reason: "below_resolution", detail: "none held it" } }),
      concurrency: null, p99_us: null, first_disqualified_conc: null, lower_bound: false },
    { bound_ms: 10, rps: sealMetric(20000), concurrency: 64, p99_us: 4000, first_disqualified_conc: 128, lower_bound: false },
    { bound_ms: null, rps: sealMetric(22000), concurrency: 1024, p99_us: 40000, first_disqualified_conc: null, lower_bound: true },
  ];
  const svg = app.frontierSpark(three, { min: 19, max: 44363, boundMs: 10 });
  assert.match(svg, /no rung held this tail/, "a bound nothing held is drawn ON THE FLOOR, and titled");
  assert.match(svg, /r="1\.9" fill="currentColor"/, "an established ceiling is a filled dot");
  assert.match(svg, /r="2\.4" fill="none"/, "a floor is an OPEN dot - it is not a proven peak");
  assert.match(svg, /stroke-dasharray="2 2"/, "and the selected bound is ruled, so the ranked number is locatable");
  // A record with no frontier draws NOTHING - never an empty frame that reads as a measurement.
  assert.equal(app.frontierSpark([], { ...scale }), "");
  assert.equal(app.frontierSpark(null, { ...scale }), "");
  /* THE SLOWEST SHAPE ON THE BOARD MUST STILL BE A SHAPE. plano's real curve: nothing under any declared
     bound (its tail is ~890 ms at c=8) and 19 req/s unbounded. There is no share of full rate to state - a
     share of a rate it never reached under any bound is not a number - but the CURVE is the whole finding, so
     the cell keeps it and withholds only the percentage. Rendering the cell as n/a here would delete the
     finding for exactly the gateways it is about, and "no data" is a neutral impression of a damning
     measurement. */
  const floorOnly = bcCell({ dialect: "openai", frontier: { 1: null, 5: null, 10: null, 50: null, 100: null, none: 19 } });
  const g = { key: "slow", display: "Slow", lang: "Go", best_cell: floorOnly };
  const cell = app.frontierShapeCell(g, { ...app.newState(), mode: "peak" });
  assert.equal(cell.na, false, "a curve with no share to state is still a curve");
  /* AND IT SAYS SO IN WORDS. A bare "—" was what shipped, and the owner read it as missing data - which is
     the one thing it is not: this gateway SERVED and no concurrency it was offered held any published tail.
     A dash is the neutral rendering of a damning measurement, and a "0%" would be worse: it would claim a
     share at a bound where no rung qualified at all. The cell states the finding as a sentence, in the same
     measured-zero ink a "no rung held this tail" carries in the reading columns. */
  assert.equal(cell.text, "served nothing under 100 ms", "the percentage - not the curve - is what is withheld, and the cell says why in words");
  assert.ok(!/\b0\s*%/.test(cell.text), "and never as 0%, which would claim a measured share it does not have");
  assert.equal(cell.zero, true, "carrying the measured-zero styling, not an absence styling");
  assert.ok(cell.v != null, "a curve that holds nothing at any bound is the most extreme shape on the board, not a null that sinks to the bottom");
  assert.match(cell.note, /no measurable throughput under ANY published bound/);
  const td = app.COLUMN_SETS.performance.find((c) => c.id === "shape").render(g, { ...app.newState(), mode: "peak", data: { gateways: [g] } });
  assert.match(td, /frontier-spark/, "the sparkline still renders for the slowest row on the board");
  assert.equal((td.match(/<title>[^<]*no rung held this tail<\/title>/g) || []).length, 5,
    "five ticks on the floor - one per bound the gateway could not hold - and one real point");
});

test("FRONTIER: an absent reading keeps its OWN reason, which needs the block-prefixed absences key", () => {
  /* THIS PINS A FIX THAT SHIPPED WITHOUT A TEST, AND THE BUG IT FIXED WAS SILENT.

     `sealFrontier` looks its absences up under `perf.frontier.<bound>.rps` - the block-prefixed key the
     engine actually writes - and once looked them up under the bare `frontier.<bound>.rps`. Nothing threw:
     every lookup simply missed, so all 36 absent frontier readings degraded to a flat `not_measured` with
     no detail, and the board stopped being able to say WHY a gateway carried nothing at a bound. "No rung
     held this tail" and "we did not measure this" render identically once the reason is gone, which is
     invariant 1 - a measured absence and an unmeasured one must never look alike.

     It was found by a chart agent noticing the reasons had vanished, not by any check. A refactor that
     renamed the prefix, or "simplified" the lookup back to the bare key, would restore the silence. */
  const readings = [
    { p99_bound_us: 1000, rps: null, concurrency: null, p99_us: null, first_disqualified_conc: null, lower_bound: false },
    { p99_bound_us: null, rps: 19, concurrency: 8, p99_us: 889100, first_disqualified_conc: null, lower_bound: false },
  ];
  const absences = {
    "perf.frontier.1ms.rps": {
      reason: "below_resolution",
      detail: "every cleanly-served rung had a tail latency at or above 1ms, so this gateway carried no measurable throughput under that bound",
    },
  };
  const sealed = sealFrontier(readings, absences);
  const tight = sealed.find((r) => r.bound_ms === 1);
  assert.equal(tight.rps.value, null, "an absent reading has no value");
  assert.equal(tight.rps.reason, "below_resolution",
    "the reading must carry the engine's OWN reason - a bare `not_measured` here means the prefixed key was missed");
  assert.match(tight.rps.detail || "", /carried no measurable throughput/,
    "the detail is the half a reader acts on; losing it is the silent form of this bug");

  // AND THE BARE KEY MUST NOT BE THE ONLY THING THAT WORKS: an absences map written the old way is still
  // honoured (the `|| absences[key]` fallback), so this test cannot pass merely because both spellings
  // were collapsed into one.
  const bareOnly = sealFrontier(readings, { "frontier.1ms.rps": { reason: "below_resolution", detail: "legacy spelling" } });
  assert.equal(bareOnly.find((r) => r.bound_ms === 1).rps.reason, "below_resolution",
    "the legacy unprefixed key is still read, so older bundles keep their reasons");

  // A reading that IS measured must be untouched by any of this.
  const unbounded = sealed.find((r) => r.bound_ms === null);
  assert.equal(unbounded.rps.value, 19, "a measured reading keeps its number");
});

test("FRONTIER: every metric's definition is reachable from the surface that shows it", () => {
  /* THE DEFINITIONS ARE GENERATED FROM THE ENGINE'S CONSTANTS (suite.rs metric_definitions) and surfaced
     where the number is, because the failure they exist to prevent is a reader reasoning carefully about a
     test that never ran. A definition filed on another page is a definition nobody reads. */
  const defs = {
    "perf.frontier": "THROUGHPUT AT A TAIL LATENCY YOU ACCEPT. For each declared bound...",
    "perf.added_latency": "WHAT THE GATEWAY ADDS, at concurrency 1...",
    "stream.streams_sustained": "THE MOST CONCURRENT SSE STREAMS THIS CELL CARRIES CLEANLY...",
    memory: "See matrix.memory.protocol...",
  };
  const data = { gateways: [], definitions: defs };
  // SELECTED BY PREFIX, never by an enumerated list: a definition the engine adds under `perf.` reaches the
  // Performance surfaces with no change here, which is the only way this cannot go stale.
  assert.deepEqual(app.definitionsFor(app.DEFINITION_PREFIXES.performance, data).map((e) => e[0]),
    ["perf.added_latency", "perf.frontier"]);
  assert.deepEqual(app.definitionsFor(app.DEFINITION_PREFIXES.streaming, data).map((e) => e[0]), ["stream.streams_sustained"]);
  assert.deepEqual(app.definitionsFor(app.DEFINITION_PREFIXES.memory, data).map((e) => e[0]), ["memory"]);
  const unknown = { gateways: [], definitions: { "perf.something_new": "a metric this table has not learned about" } };
  assert.equal(app.definitionsFor(app.DEFINITION_PREFIXES.performance, unknown).length, 1,
    "a definition the engine publishes and this table does not know still surfaces");
  // The fold carries the engine's prose VERBATIM - reworded here it would be a second source of truth.
  const html = app.definitionsFold(app.DEFINITION_PREFIXES.performance, data);
  assert.match(html, /<details class="metric-defs">/);
  assert.match(html, /THROUGHPUT AT A TAIL LATENCY YOU ACCEPT/);
  assert.match(html, /WHAT THE GATEWAY ADDS/);
  // A bundle generated before the engine published definitions renders nothing, not an empty fold.
  assert.equal(app.definitionsFold(app.DEFINITION_PREFIXES.performance, { gateways: [] }), "");
  assert.equal(app.definitionsFold(app.DEFINITION_PREFIXES.performance, null), "");
  // And the drawer reaches it from the lane that shows the numbers.
  const g = { key: "d", display: "D", lang: "Rust", best_cell: bcCell({ dialect: "openai" }) };
  const h = app.drawerHtml(g, { ...app.newState(), view: "performance", mode: "peak", data: { gateways: [g], definitions: defs } });
  assert.match(h, /THROUGHPUT AT A TAIL LATENCY YOU ACCEPT/, "the drawer's perf lane carries its own definition");
});

test("FRONTIER: the retired throughput vocabulary is unreachable from every board surface", () => {
  // The producer cannot emit these any more; this is the guard that no RENDERER can either. A caption or a
  // note that still names them would describe a measurement the board no longer takes - which is precisely
  // the class of error the frontier was built to end.
  const raw = readFileSync(join(HERE, "app.js"), "utf8");
  // COMMENTS ARE WHERE THE HISTORY IS KEPT DELIBERATELY - every deletion in this file names what it removed
  // and why - so they are stripped and the scan is of STRING LITERALS ONLY: the text a reader can actually
  // see on the page.
  const src = raw.replace(/\/\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
  const literals = src.match(/"[^"\n]*"|'[^'\n]*'|`[^`]*`/g) || [];
  for (const tok of ["rps_sustained_20ms", "rps_max_proxy", "cpu_fps", "mock_bound", "unverifiable", "paced_match"]) {
    const hit = literals.filter((l) => l.includes(tok) && !/DELETED|retired|RETIRED/.test(l));
    assert.deepEqual(hit, [], `${tok} must not appear in any rendered string; got ${JSON.stringify(hit)}`);
  }
  // And the two columns that read them are gone from every column set, by id and by label.
  const ids = Object.values(app.COLUMN_SETS).flat().map((c) => c.id);
  for (const id of ["rps20", "rpsmax", "cpufps"]) assert.ok(!ids.includes(id), `the ${id} column is retired`);
  const labels = Object.values(app.COLUMN_SETS).flat().map((c) => String(typeof c.label === "function" ? c.label() : c.label));
  assert.ok(!labels.some((l) => /20 ms upstream|Max proxy|CPU-bound/.test(l)),
    `no column may still be labelled with a retired metric; got ${JSON.stringify(labels)}`);
  // The tooltip that was wrong in both halves - "The 20 ms is the UPSTREAM's delay, not a latency target
  // the gateway is held to" - went with the column. Nothing may say it again.
  assert.ok(!/not a latency target the gateway is held to/.test(src));
  assert.ok(!/p99 under 1 s|p99 < 1 s/.test(src), "and no surface may assert the gate's fabricated bar");
  // The same scan over the two files a reader also sees, for the same reason.
  for (const f of ["index.html", "style.css"]) {
    const t = readFileSync(join(HERE, f), "utf8");
    for (const tok of ["rps_sustained_20ms", "rps_max_proxy", "cpu_fps", "Sustained RPS", "Max proxy"])
      assert.ok(!t.includes(tok), `${f} must not name the retired ${tok}`);
  }
});

// ---- the repo URL reaches an href at four sites, and nothing built a hostile one -----------------
// gen-data validates `repo` to an https:// URL or null on the way in, which is the primary defence -
// but app.js interpolated `g.repo` into an href at FOUR independent render sites, so "is it escaped
// here" was four separate questions, and no test anywhere constructed a gateway with a hostile repo to
// ask any of them. All four now route through gwLink(), and this covers the helper plus the fact that
// there is exactly one place left that can get it wrong.
test("every repo href escapes, and all FOUR render sites route through the one helper", () => {
  const hostile = { display: 'Ev"il <script>', repo: 'https://example.com/a" onmouseover="alert(1)' };
  const a = app.gwLink(hostile);
  // The attack is escaping the href's own quote to start a new attribute. An escaped `&quot;` cannot,
  // so the test is for an UNESCAPED quote followed by an attribute, not for the substring itself.
  assert.ok(!/ onmouseover="/.test(a), `the attribute must not break out of the href: ${a}`);
  assert.equal((a.match(/"/g) || []).length, 6, `only the six attribute delimiters may be real quotes: ${a}`);
  assert.match(a, /&quot;/, "the quote is escaped, not passed through");
  assert.ok(!/<script>/.test(a), "the display name is escaped too");
  assert.match(a, /&lt;script&gt;/);
  assert.match(a, /rel="noopener"/, "and the link keeps its opener protection");
  // No repo: plain escaped text, never a bare or empty anchor.
  const plain = app.gwLink({ display: 'x"<y>' });
  assert.ok(!/<a /.test(plain), "no repo means no link at all");
  assert.equal(plain, "x&quot;&lt;y&gt;");
  assert.equal(app.gwLink({ display: null }), "", "a gateway with no display name renders nothing, not 'null'");

  // Behaviourally, at the two sites a DOM-free suite can reach.
  const g = { key: "h", display: hostile.display, lang: "Rust", cls: "AI proxy", repo: hostile.repo,
    best_cell: bcCell({ dialect: "openai" }) };
  const st = drawerState(g);
  const nameCell = app.COLUMN_SETS.performance.find((c) => c.id === "name").render(g, st);
  assert.ok(!/ onmouseover="/.test(nameCell), `the table's name column must escape: ${nameCell}`);
  assert.ok(!/ onmouseover="/.test(app.drawerHtml(g, st)), "and so must the drawer head");

  // ...and structurally at all four, including the two that only exist inside a DOM renderer
  // (renderMatrix's per-gateway header, renderGateways' roster row). ONE site may build this anchor.
  const src = readFileSync(join(HERE, "app.js"), "utf8");
  const rawHrefs = src.match(/href="\$\{esc\(g\.repo\)\}"/g) || [];
  assert.equal(rawHrefs.length, 1,
    `exactly one place may interpolate a repo into an href (gwLink); found ${rawHrefs.length}`);
  // Four call sites, no more and no fewer (comments and the definition line are excluded, so this
  // counts real callers): the table name column, the drawer head, the protocol-matrix header and the
  // roster row. A fifth hand-rolled anchor would fail the raw-href count above; a MISSING one would
  // fail here, which is the direction a refactor breaks it in.
  const callSites = src.split("\n").filter((ln) => /\bgwLink\(g\)/.test(ln) &&
    !/^\s*(\/\/|\*|\/\*)/.test(ln) && !/^\s*function\b/.test(ln));
  assert.equal(callSites.length, 4, `expected four gwLink call sites, got ${callSites.length}`);
  for (const fn of ["renderMatrix", "renderGateways"]) {
    const body = src.slice(src.indexOf(`function ${fn}(`));
    assert.match(body.slice(0, body.indexOf("\n}\n") + 3), /gwLink\(g\)/,
      `${fn} must render the gateway name through gwLink`);
  }
});

/* ============================================================================================
   THE 2026-07-30 REVIEW: the owner's own findings on the live board, each with a guard.
   Every one of these is an OVERCLAIM CLASS - a surface asserting more than the measurement
   establishes - which is the class this whole project exists to eliminate, so each gets a test that
   fails if the wording or the ranking slides back.
   ============================================================================================ */

test("#2 OVERCLAIM: no surface claims a MAXIMUM ACROSS CELLS, because nothing computes one", () => {
  /* WHAT WENT WRONG. The bound chooser's note read "showing the most req/s each gateway carried while 99% of
     requests finished under 10 ms". That asserts a maximum over a GATEWAY - over all of its cells - and the
     selection does no such thing: gen-data.mjs `bestCell` takes the openai diagonal unconditionally and
     otherwise ranks on added-latency p99, so it never reads a throughput number at all. On the live board
     kong's four diagonals spanned 3,903 to 22,891 req/s at one bound, making "the most" wrong by ~6x on that
     one row, and no choice of bound could have changed which cell was picked. */
  const board = { gateways: [] };
  const st = { ...app.newState(), view: "performance", mode: "peak", bound: 10, data: board };

  // THE LABEL. "Peak" named a maximum the chooser does not compute; the key stays (it is a URL contract).
  assert.ok(app.CHOOSER_MODES.has("peak"), "the ?mode=peak URL token is unchanged - every shared link uses it");
  assert.notEqual(app.MODE_LABELS.peak, "Peak", "but the CONTROL must not call it a peak");
  assert.match(app.MODE_LABELS.peak, /own cell/i, "it is a representative-cell chooser, and says so");
  assert.match(app.MODE_TIPS.peak, /lowest-added-latency|added.latency/i,
    "and the tooltip names the rule that actually selects the cell");
  assert.match(app.MODE_TIPS.peak, /cannot change which cell/i,
    "including the consequence: the bound cannot move this selection");

  /* THE PHRASE ITSELF, hunted across every string the board can put in front of a reader. This is a
     CLASS guard, not a spot fix: it fails on any future surface that revives the wording, which is how the
     retired "p99 < 1 s" caption survived for as long as it did. "each gateway carried" is the exact shape of
     the claim - a rate attributed to the GATEWAY rather than to the one cell it was measured on. */
  const surfaces = [
    app.captionText(app.chooserCaption("performance", st, board)),
    app.captionText(app.chooserCaption("streaming", st, board)),
    app.captionText(app.frontierCaption(st, board)),
    ...app.COLUMN_SETS.performance.map((c) => `${txtOf(c.label)} ${txtOf(c.title)}`),
    ...app.COLUMN_SETS.frontier.map((c) => `${txtOf(c.label)} ${txtOf(c.title)}`),
    ...app.BOUND_CHOICES.map((b) => app.boundColLabel(b) + " " + app.boundClause(b)),
    app.HELD_REFERENCE, app.BOUND_GROUP_LABEL,
  ];
  for (const s of surfaces)
    assert.ok(!/most\s+req(uests)?\/?s?e?c?[^.]*each gateway carried/i.test(s),
      `a surface attributes a rate to the GATEWAY rather than to the cell it was measured on: ${s}`);
  // A reading IS the top qualifying rung of ONE cell's sweep, so "the most ... the chosen cell carried" is
  // exact and must stay - the fix was the scope of the claim, not the strength of it.
  const rps = app.COLUMN_SETS.performance.find((c) => c.id === "rps");
  assert.match(txtOf(rps.title), /the chosen cell carried/, "the per-CELL maximum is real and still claimed");

  // AND THE DELTA'S REFERENCE CELL IS NOT CALLED A PEAK EITHER: a positive req/s delta against it is
  // ordinary (it was chosen on latency), and "peak" made that read as impossible.
  const src = readFileSync(join(HERE, "app.js"), "utf8");
  assert.ok(!/vs peak \(/.test(src), "no surface labels the reference cell 'vs peak'");
});

test("#4 OVERCLAIM: the shape column states a SHARE OF FULL RATE and names the bound it was read at", () => {
  /* WHY IT IS A PERCENTAGE AND NOT A GAIN FACTOR. The column shipped twice as a ratio - first as a bare
     "×1.3" ("i dont know what 1.3x or whatever means"), then as "×1.0 from 1 ms" - and the owner still could
     not read his own column: "its just not clear what this means, even I know and i cant figure it out". A
     factor makes the reader assemble one sentence out of three scattered pieces: the multiplier (of WHAT?),
     "from 1 ms" (to what?), and the missing half of that stranded in the column header. And ×1.0 was the BEST
     possible result while reading like an unfilled default.
     THE TRAP IT STILL HAS TO SURVIVE, FROM THE LIVE BOARD:
       litellm-rust  43,876 of 44,363 at 1 ms   (full rate at a 0.56 ms tail)
       tensorzero    11,875 of 11,936 at 50 ms  (holds NOTHING under 10 ms)
     Both round to 99%. Rendered without the bound they would be the same four characters, telling a reader
     those two gateways have the same curve - the exact opposite of the truth and the one claim this metric
     exists to make. one-api is the same trap the other way: 78% at 50 ms looks better-behaved than kong's 56%
     at 1 ms, while one-api serves nothing at all under 50 ms. So the bound is ON the cell, and `v` groups the
     column by that bound before ranking on the share. */
  const stFor = (g) => ({ ...app.newState(), mode: "peak", data: { gateways: [g] } });
  const gw = (key, frontier) => ({ key, display: key, lang: "Rust", best_cell: bcCell({ dialect: "openai", frontier }) });

  const tight = gw("tight", { 1: 43876, 5: 44363, 10: 44363, 50: 44363, 100: 44363, none: 44363 });
  const loose = gw("loose", { 1: null, 5: null, 10: null, 50: 11875, 100: 11936, none: 11936 });
  const tc = app.frontierShapeCell(tight, stFor(tight));
  const lc = app.frontierShapeCell(loose, stFor(loose));
  assert.equal(tc.text, "99% of its full rate at 1 ms", "the tightest-tail gateway names the bound it was read at");
  assert.equal(lc.text, "99% of its full rate at 50 ms", "and so does the gateway that holds nothing tighter than 50 ms");
  assert.notEqual(tc.text, lc.text, "THE TWO MUST NOT RENDER IDENTICALLY: they are opposite findings");
  assert.match(lc.note, /no rate at all under 10 ms/,
    "and the tooltip states the tighter bound it could not serve, which is the finding");
  /* NO VERDICT ON THE CELL. A preview rendered tensorzero as "99% - but only at 50 ms"; this board publishes
     facts, not editorial judgements about whether 50 ms is bad, and the bound already carries that plainly.
     Every row takes the identical form and the reader draws the conclusion. */
  assert.ok(!/but only/i.test(lc.text), `the cell states the reading, not a verdict on it: ${lc.text}`);

  /* THE DENOMINATOR IS THE UNBOUNDED READING, not the 100 ms one and not a max across bounds. `loose` is the
     discriminating case: 11,875 at 50 ms, 11,936 at BOTH 100 ms and unbounded, so the percentage alone cannot
     tell which was used. The tooltip names both rates, and frontierFullRate names the reading itself. */
  assert.match(lc.note, /11,875 req\/s .*against 11,936 req\/s/, "the tooltip names numerator and denominator");
  assert.equal(app.frontierFullRate(app.frontierOf(app.chooserCellPerf(loose, stFor(loose)))), 11936,
    "and 'full rate' is the UNBOUNDED reading, read through one named accessor");

  /* NEVER 100% UNLESS THE TWO READINGS ARE THE SAME NUMBER. Rounding 99.6% up to "100% of its full rate"
     asserts the gateway loses nothing at all to a tight tail when its own readings say otherwise - the exact
     class of overclaim this column exists to remove. */
  assert.equal(app.heldPct(0.996), 99, "99.6% floors at 99: it is not AT its full rate");
  assert.equal(app.heldPct(0.9999), 99, "and so does anything short of equality, however close");
  assert.equal(app.heldPct(1), 100, "only two identical readings - an exactly flat curve - print 100");

  /* THE RANKING. Ranking on the bare share would sort 99%-at-50ms beside 99%-at-1ms as though they measured
     one quantity, and would file the gateway that cannot serve under 10 ms beside the one running at full rate
     at 0.56 ms. Bound-of-origin dominates; the share orders within it. Bigger is better now that the column
     states a share rather than a gain, so the descending default lands the good shapes on top. */
  assert.ok(tc.v > lc.v, "a share read at a looser bound ranks below one read at a tighter bound, whatever its size");
  const steepFromTight = gw("steep", { 1: 10697, 5: 19339, 10: 20352, 50: 26000, 100: 26000, none: 26000 });
  const sc = app.frontierShapeCell(steepFromTight, stFor(steepFromTight));
  assert.ok(sc.v < tc.v, "within one origin the smaller share ranks worse");
  assert.ok(sc.v > lc.v, "but ANY share from a tighter origin outranks one from a looser - they are not one quantity");
  // The key gives each origin group a disjoint interval, so no share - not even an exactly flat 1.0 - can leak.
  assert.ok(app.heldSortKey(1, 1) < app.heldSortKey(0, 0),
    "a perfectly flat curve read at 5 ms still sorts below the worst share read at 1 ms");
  assert.equal(app.heldSortKey(0, 1), app.HELD_NOTHING_INDEX * 2 + 1,
    "100% at the tightest bound is the ceiling of the ranking - the good shape");

  /* THE HELD-NOTHING CASE, which is what the owner actually saw: plano carried nothing under ANY published
     bound and 19 req/s unbounded, so there is no share to state. It rendered a bare "—", which reads as
     missing data when the truth is a measurement: it SERVED, cleanly, and no concurrency it was offered held
     even the loosest tail on the board. A dash is the neutral rendering of a damning finding, and it flattered
     the slowest row. A "0%" would be worse still - it would claim a share at a bound where no rung qualified. */
  const none = gw("none", { 1: null, 5: null, 10: null, 50: null, 100: null, none: 19 });
  const nc = app.frontierShapeCell(none, stFor(none));
  assert.equal(nc.na, false, "the curve is still a curve");
  assert.ok(!/^[—–-]+$/.test(nc.text), `a bare dash reads as missing data, which this is not: ${nc.text}`);
  assert.ok(!/%/.test(nc.text), `and it is not dressed as a percentage: ${nc.text}`);
  assert.equal(nc.text, "served nothing under 100 ms",
    "the cell states the finding in words, naming the loosest bound it failed to hold");
  assert.match(nc.note, /not a gap/i, "the tooltip insists it is a measurement");
  assert.ok(nc.v < lc.v, "and it ranks below every bound: it is the most extreme shape on the board, not a null");
  assert.notEqual(nc.v, null, "and it is not a null, which rowComparator would sink regardless of direction");
  // Rendered, it carries the measured-zero treatment the reading columns use.
  const td = app.frontierShapeTd(none, stFor(none));
  assert.match(td, /reading-zero/, "the markup marks it as a measured nothing, not an absence");
  assert.match(td, /reading-none/, "in the same ink a 'no rung held this tail' carries");
  assert.match(td, /frontier-spark/, "with the curve, which is the whole finding");

  /* AND THE MISSING-DENOMINATOR CASE: a bounded reading with no unbounded one. Structurally impossible today
     (every cell publishes every declared bound plus the unbounded reading), and the point is that if it ever
     happens we publish NO percentage rather than promoting the 100 ms reading into a denominator it is not -
     that would rebase one row against a different quantity while looking identical to every other row. */
  const noFull = gw("nofull", { 1: 500, 5: 600, 10: 700, 50: 800, 100: 900 });
  const nf = app.frontierShapeCell(noFull, stFor(noFull));
  assert.ok(!/%/.test(nf.text), `no unbounded reading means no share, not a share of the 100 ms reading: ${nf.text}`);
  assert.equal(nf.v, null, "and it does not rank on a quantity it could not compute");

  // A cell with NO frontier at all is a different state and still reads n/a - that one really is absence.
  const bare = { key: "b", display: "b", lang: "Go", best_cell: bcCell({ dialect: "openai", frontier: null }) };
  assert.equal(app.frontierShapeCell(bare, stFor(bare)).na, true,
    "no frontier at all stays n/a, distinct from 'served nothing'");

  /* THE COLUMN NAMES ITS OWN QUANTITY. Asserted on the REQUIREMENT, not on one vocabulary: the header has been
     through "Curve across bounds" (the owner: "i dont know what 1.3x or whatever means"), then a named gain
     factor, and the wording may move again. What must hold is that the header is not a bare "Curve" with an
     unexplained number under it, that it names no ratio the reader has to decode, and that the tooltip says
     what the figure is measured AGAINST - the tightest bound the cell holds any rate at, in whatever words. */
  for (const set of ["performance", "frontier"]) {
    const col = app.COLUMN_SETS[set].find((c) => c.id === "shape");
    const label = txtOf(col.label);
    assert.ok(!/^Curve( across bounds)?$/i.test(label),
      `the ${set} tab's shape header must name its quantity, not just the picture: ${label}`);
    assert.ok(!/×|gain/i.test(label),
      `and must not name a ratio the reader had to decode: ${label}`);
    // Case-insensitive: the tooltip SHOUTS the phrase for emphasis, and the claim is the wording, not the case.
    assert.match(txtOf(col.title), /tightest published bound|tightest bound/i,
      `and the ${set} tooltip must say what the figure is read against`);
    assert.match(txtOf(col.title), /share of its (full )?rate with no latency bound|share of its full rate/,
      `and what it is a share OF: ${set}`);
  }
  const ref = app.captionFor("performance", { ...app.newState(), view: "performance", mode: "peak", data: { gateways: [] } },
    { gateways: [] }).notes.join(" ");
  assert.match(ref, /TIGHTEST tail-latency bound it holds any rate at/i,
    "the reference block below the table explains the basis");
  assert.match(ref, /50 ms/, "and names the loose-origin trap it exists to prevent");
});

test("#1 PROSE: one or two sentences above the table, everything else below it as reference", () => {
  /* THE OWNER'S INSTRUCTION, VERBATIM: "way too much text on each tab", "1-2 sentence english, definitions go
     below data table like references". The Frontier tab had SIX paragraphs before its first number. Nothing
     was deleted - the notes carry findings, and the "0 · no rung held this tail" distinction in particular is
     the difference between a damning measurement and a shrug - so this asserts BOTH halves: the lead is short,
     and every relocated claim is still reachable on the tab. */
  const board = { gateways: [], definitions: {} };
  const st = { ...app.newState(), view: "frontier", mode: "peak", bound: 10, data: board };
  for (const view of ["performance", "streaming", "frontier", "memory"]) {
    const c = app.captionFor(view, { ...st, view }, board);
    assert.ok(Array.isArray(c.lead) && Array.isArray(c.notes), `${view} splits into lead + notes`);
    assert.ok(c.lead.length >= 1 && c.lead.length <= 2, `${view} leads with 1-2 sentences, got ${c.lead.length}`);
    // A "sentence" that is a paragraph defeats the point, so the lead is length-capped too.
    const chars = c.lead.join(" ").length;
    assert.ok(chars <= 320, `${view}'s lead is ${chars} chars; the owner's limit is a sentence or two`);
  }
  // THE FRONTIER'S RELOCATED CLAIMS ARE ALL STILL THERE, below the table.
  const fc = app.frontierCaption(st, board);
  const notes = fc.notes.join(" ");
  assert.match(notes, /no rung held this tail/, "the measured-zero distinction survives the move");
  assert.match(notes, /It is not missing data/, "including the sentence that makes it a finding rather than a gap");
  assert.match(notes, /"≥" marks a reading whose sweep ran out of ladder/, "and the floor marker's meaning");
  assert.match(notes, /never the bound/, "and the observed-tail-is-not-the-bound rule");
  assert.ok(fc.notes.includes(app.HELD_REFERENCE), "and the shape column's reference the owner asked for");
  assert.ok(!/no rung held this tail/.test(fc.lead.join(" ")), "none of which is above the table any more");
  // The Performance tab carries the shape column, so it carries the reference; Streaming has no such column.
  assert.ok(app.chooserCaption("performance", st, board).notes.includes(app.HELD_REFERENCE));
  assert.ok(!app.chooserCaption("streaming", st, board).notes.includes(app.HELD_REFERENCE),
    "a tab must not explain a figure it does not render");
  // The notes render as a collapsed fold, so relocating them costs no vertical space...
  const fold = app.notesFold(fc.notes);
  assert.match(fold, /<details/, "the reference block is collapsed by default");
  assert.match(fold, /How to read this table/, "and says what it is");
  assert.equal(app.notesFold([]), "", "a view with nothing to say renders no empty fold");
  // ...and it lives BELOW the table in the markup, which is the whole instruction.
  const html = readFileSync(join(HERE, "index.html"), "utf8");
  assert.ok(html.indexOf(`id="results-table"`) < html.indexOf(`id="table-defs"`),
    "the reference block must come AFTER the table in the document, like references");
  assert.ok(html.indexOf(`id="table-caption"`) < html.indexOf(`id="results-table"`),
    "and the lead before it");
  // captionText flattens both halves, so a claim can be asserted without pinning which half carries it.
  assert.equal(app.captionText(fc), [...fc.lead, ...fc.notes].join(" "));
});

test("#3 HEADERS: the six per-bound columns share ONE spanning group instead of repeating five words", () => {
  /* "make Req/s 99% a header that spans all columns vs repeating?" - the owner. Six headers each read
     "Req/s · 99% under N ms": the same five words six times across the widest table on the board. */
  const cols = app.COLUMN_SETS.frontier;
  const bound = cols.filter((c) => c.group === app.BOUND_GROUP_LABEL);
  assert.equal(bound.length, app.BOUND_CHOICES.length, "every published reading is under the one group");
  for (const c of bound)
    assert.ok(!/Req\/s|99%/.test(txtOf(c.label)), `a sub-header still repeats the shared words: ${txtOf(c.label)}`);
  const st = { ...app.newState(), view: "frontier", bound: 10, sortCol: app.boundColId(10), sortDesc: true };
  const head = app.theadHtml(cols, st);
  // TWO ROWS: the group over the six, and the six under it.
  assert.equal((head.match(/<tr/g) || []).length, 2, "a grouped head is two rows");
  assert.equal((head.match(/class="colgroup nosort" colspan="6"/g) || []).length, 1,
    "one group cell spanning exactly the six reading columns");
  // THE UNGROUPED COLUMNS SPAN BOTH ROWS, so the body stays aligned under them.
  const ungrouped = cols.filter((c) => !c.group).length;
  assert.equal((head.match(/rowspan="2"/g) || []).length, ungrouped,
    "every ungrouped column spans both header rows");
  /* THE SELECTED BOUND'S SORT AFFORDANCE SURVIVES THE SPLIT. The group cell must NOT be sortable (it is not
     a column), and the marker + direction arrow must stay on the reader's own column in the second row -
     otherwise switching the bound would re-rank the board with nothing on screen saying so. */
  assert.ok(!/<th class="colgroup nosort"[^>]*data-col=/.test(head), "the group header is not a sortable column");
  const sub = head.slice(head.indexOf('class="subhead"'));
  assert.match(sub, new RegExp(`data-col="${app.boundColId(10)}" class="sorted"`), "the selected column is marked");
  assert.match(sub, /▾/, "and carries the direction arrow");
  // A tab with no groups is still ONE row, and gains no phantom rowspan.
  const flat = app.theadHtml(app.COLUMN_SETS.streaming, { ...app.newState(), view: "streaming" });
  assert.equal((flat.match(/<tr/g) || []).length, 1, "an ungrouped tab keeps a single header row");
  assert.ok(!/rowspan/.test(flat), "with no rowspan, which would reserve a header row that does not exist");
});

test("#5 IDLE CURVE: it is not a recovery curve, and a flat window renders flat", () => {
  /* TWO DEFECTS IN ONE TEMPLATE. The idle and load sparklines shared a caption - "peak X → recovered Y MiB" -
     and on the idle window every word of that is wrong: the window is the process AT REST, sampled BEFORE any
     load, so nothing has been recovered from anything.
     And it could not show its own finding. Framed on the load axis (0 → 2x idle) every idle series in the
     field is a nearly-flat line at the idle level, so all 26 of them drew the same picture - hiding that
     every bifrost cell jumps ~100 MiB while idle and then holds, which no other gateway does. */
  const at = (n, f) => Array.from({ length: n }, (_, i) => ({ t_s: i * 0.5, rss_mib: f(i, n) }));
  // litellm-rust's real shape: 252.2578 → 252.2656, one 8 KiB page of movement across the whole window.
  const flatSeries = at(120, (i) => 252.2578125 + (i > 60 ? 0.0078125 : 0));
  // bifrost openai>openai: 152.3 climbing to 237.3 inside the first seconds, then dead flat.
  const rampSeries = at(120, (i) => (i < 8 ? 152.3 + (i / 8) * 85 : 237.3));

  const flat = app.rssSparkline(flatSeries, null, 252.265625, "idle");
  const ramp = app.rssSparkline(rampSeries, null, 217.0, "idle");
  const load = app.rssSparkline(rampSeries, 60, 217.0);

  // 1. THE WORD "RECOVERED" IS A LOAD-WINDOW WORD and must not appear on an idle panel, nor "peak".
  for (const [name, svg] of [["flat", flat], ["ramp", ramp]]) {
    assert.ok(!/recovered/i.test(svg), `${name}: nothing is recovered in a window taken before any load`);
    assert.ok(!/\bpeak\b/i.test(svg), `${name}: the highest sample at rest is not a peak under load`);
    assert.match(svg, /at rest/, `${name}: it says what the window is`);
  }
  // The LOAD panel keeps the recovery vocabulary - correct there - but attached to the window it read at.
  const loadMarked = app.rssSparkline(rampSeries, 60, 217.0, "load", { recoveredAt: 237.3, recoveryWindowS: 30 });
  assert.match(loadMarked, /peak .* → .* recovery mark/, "the LOAD panel names the recovery mark it read at");
  assert.match(load, /peak .* → .* at the last sample/, "and without that scalar it names the point it does have");

  /* 2. A FLAT WINDOW RENDERS FLAT. This is the trap a bare auto-scale walks into, and it is the same bug the
     load axis was already fixed for: RSS is sampled in whole pages, so a static process still reports one or
     two pages of jitter, and scaled to its own range that one page becomes a full-height cliff - a panel
     claiming a memory event where the truth is "this process did not move". The span is floored at
     IDLE_AXIS_MIN_SPAN of the published idle figure. */
  const ys = (svg) => [...svg.matchAll(/[ML]([\d.]+),([\d.]+)/g)].map((m) => Number(m[2]));
  const height = (svg) => Math.max(...ys(svg)) - Math.min(...ys(svg));
  assert.ok(height(flat) < 2, `0.008 MiB on a 252 MiB process must draw flat, drew ${height(flat).toFixed(1)}px`);
  assert.ok(height(ramp) > 25, `an 85 MiB climb must fill the frame, drew ${height(ramp).toFixed(1)}px`);
  assert.ok(app.IDLE_AXIS_MIN_SPAN > 0 && app.IDLE_AXIS_MIN_SPAN < 1, "the floor is a fraction of idle");
  // The floor is what does it: 0.008/252 is far below it, 85/217 far above.
  assert.ok(0.0078125 / 252.265625 < app.IDLE_AXIS_MIN_SPAN, "the flat case sits under the floor by construction");
  assert.ok(85 / 217 > app.IDLE_AXIS_MIN_SPAN, "and the ramp above it");

  /* 3. THE MAGNITUDE IS STATED IN NUMBERS, so it never has to be read off a floored axis - which is what
     keeps 0.008 MiB and 85 MiB distinguishable however they are drawn. fmt1 would round the first to "0.0",
     a flat zero, which is exactly the "nothing happened" claim the span exists to distinguish from. */
  assert.match(flat, /0\.00781 MiB/, "the exact movement is published, not rounded away to 0.0");
  assert.match(ramp, /spanned 152\.3–237\.3 MiB/, "and the climb states its band");
  const stampOfSvg = (svg) => svg.match(/class="stamp muted">([^<]*)</)[1];
  assert.notEqual(stampOfSvg(flat), stampOfSvg(ramp),
    "so the two windows are distinguishable in text whatever the axis does");
  assert.ok(!/MiB over \d+ s/.test(flat) && !/MiB over \d+ s/.test(ramp),
    "and neither states a magnitude 'over' the window length, which reads as a rate (see MEMORY #4)");
  assert.equal(app.fmt2(0.0078125), "0.00781");
  // A range whose ends round to one figure is not a range; it says "held", because "252.3–252.3" reads as a bug.
  assert.match(flat, /held 252\.3 MiB/, "a window that never moved a tenth of a MiB says it held its level");
  assert.match(ramp, /spanned 152\.3–237\.3 MiB/, "a window that moved states the band");

  /* 4. THE MEDIAN IS ANNOTATED, because idle_rss_mib IS the median of this very window: drawing it is what
     makes the sparkline and the scalar in the column beside it visibly agree instead of merely coexisting. */
  assert.match(ramp, /median 217\.0 MiB/, "the published figure is named in the stamp");
  assert.match(ramp, /<title>median 217\.0 MiB - the published idle figure/, "and ruled on the chart");
  assert.ok(!/<title>median/.test(load), "the load panel's dashed rule is still the idle BASELINE, not a median");
  // The idle axis does not reach zero, so no rule may be drawn along its bottom edge claiming one.
  assert.ok(!/<polyline/.test(ramp), "no zero baseline on an axis that does not contain zero");
  assert.match(load, /<polyline/, "the load axis starts at 0, so its baseline IS zero and stays drawn");

  // 5. AND THE PANEL SAYS WHICH WINDOW IT IS, in the pair the drawer renders.
  const pair = app.rssCurves({ idle_rss_mib: seal(217.0), idle_rss_series: rampSeries,
    rss_series: rampSeries, load_s: seal(60) });
  assert.match(pair, /at rest, before any load/, "the idle half names itself");
  assert.match(pair, /load → recovery/, "and the load half keeps its own name");
});

test("#6 VERSION: a build path is not a version, and the column never dresses one up as one", () => {
  /* THE LIVE BOARD showed Helicone's Version as "target/release/ai-gat…" and LiteLLM · Rust's as
     "litellm-ai-gateway". Those are a compiler output path and a binary name. fmtBuild's four recognisers all
     declined them and its fallback then printed the raw stamp anyway, so the column asserted a version it had
     explicitly failed to find. */
  assert.equal(app.parseBuildVersion("target/release/ai-gateway"), null, "a build path names no version");
  assert.equal(app.parseBuildVersion("litellm-ai-gateway"), null, "nor does a bare binary name");
  assert.equal(app.parseBuildVersion("apache/apisix:3.17.0-debian"), "3.17.0-debian", "an image tag does");
  assert.equal(app.parseBuildVersion("somepkg==1.93.0"), "1.93.0");
  assert.equal(app.parseBuildVersion("repo@9649b27abcdef"), "@9649b27");
  assert.equal(app.parseBuildVersion("somegateway 1.4.1"), "1.4.1");

  // A SOURCE BUILD falls back to the manifest pin, which for these two IS the version (the commit built),
  // marked with "@" so a bare hex string cannot read as a release name.
  const helicone = { key: "helicone", display: "Helicone", lang: "Rust", version: "9649b27",
    matrix: { build: "target/release/ai-gateway" } };
  assert.equal(app.versionToken(helicone), "@9649b27");
  assert.ok(!/target|release/.test(app.versionToken(helicone)), "and no part of the path survives into the cell");
  // The path is not thrown away - it is provenance for HOW the thing was launched, so it rides the tooltip.
  assert.match(app.versionBasis(helicone), /Launched as: target\/release\/ai-gateway/);
  assert.match(app.versionBasis(helicone), /names no version/, "which the tooltip says outright");

  // An image tag is the strongest evidence and wins: it is what the process actually reported.
  const kong = { key: "kong", display: "Kong", lang: "Lua", version: "3.9.3", matrix: { build: "kong:3.9.3" } };
  assert.equal(app.versionToken(kong), "3.9.3");
  assert.match(app.versionBasis(kong), /Measured running: kong:3\.9\.3/);

  /* AN UNMEASURED GATEWAY IS NOT A SOURCE BUILD. gatewayBuild deliberately falls back to the manifest pin, so
     a caller that cannot tell the two apart captions a never-run row as though its pin were a launch stamp
     ("Launched as: v0.5.0"). measuredBuild is the stamp of what RAN, and only that. */
  const unrun = { key: "aisix", display: "AISIX", lang: "Go", version: "v0.5.0" };
  assert.equal(app.measuredBuild(unrun), null, "nothing ran, so there is no launch stamp");
  assert.equal(app.versionToken(unrun), "v0.5.0", "but the manifest pin is what we would measure, so it shows");
  assert.ok(!/Launched as/.test(app.versionBasis(unrun)), "and it is never described as a launch stamp");
  assert.match(app.versionBasis(unrun), /has not been measured/, "the tooltip says which state this is");

  // NEITHER SOURCE: the cell says so in words rather than showing a path or an empty dash.
  const nothing = { key: "x", display: "X", lang: "Go" };
  assert.equal(app.versionToken(nothing), null);
  assert.match(app.versionBasis(nothing), /Nothing published/);
  // And the sort key is the token the cell RENDERS, so a "no version published" row sorts as the null it is.
  assert.equal(app.ROSTER_KEY.version(nothing), null);
  assert.equal(app.ROSTER_KEY.version(helicone), "@9649b27");
});

test("#7 TITLE: every view titles itself, from the URL, without waiting on the data fetch", () => {
  /* Playwright saw the generic site title on views that should have named themselves. The cause was ordering:
     the title was only ever written from showView, which runs inside renderAll, which runs after data.json
     resolves - three quarters of a megabyte. Until then every deep link reported index.html's static <title>,
     and on the fetch-failure path it stayed generic forever. */
  const src = readFileSync(join(HERE, "app.js"), "utf8");
  const boot = src.slice(src.indexOf("function boot()"), src.indexOf("if (NODE) {"));
  const decode = boot.indexOf("applyState(decodeUrl");
  assert.ok(boot.slice(0, boot.indexOf("ensureData()")).includes("updateTitle()"),
    "boot must title the page BEFORE ensureData(), from state it has already decoded");
  assert.ok(boot.indexOf("updateTitle()") > decode, "and after the URL is decoded, so it titles the right view");
  assert.ok(boot.slice(boot.indexOf(".catch(")).includes("updateTitle()"),
    "and the failure path still names the view the reader asked for");

  // EVERY VIEW GETS ITS OWN TITLE, and they are all distinct - a browser tab strip is the use case.
  const titles = new Map();
  for (const view of app.VIEWS) {
    const t = app.pageTitle({ ...app.newState(), category: app.DEFAULT_CATEGORY, view });
    assert.ok(t.endsWith(app.SITE_TITLE), `${view} keeps the site name as context`);
    assert.ok(!titles.has(t), `${view} must not share a title with ${titles.get(t)}`);
    titles.set(t, view);
  }
  // THE VIEW LEADS, because it is the only part that differs between two open tabs or two shared links. The
  // old form was "${category} ${view}" - two nouns, no separator ("Gateways Frontier"), truncating in a tab
  // strip to the one word every view shares.
  assert.ok(app.pageTitle({ ...app.newState(), view: "frontier" }).startsWith(`${app.VIEW_LABELS.frontier} · `),
    "the view's own name comes first, separated");
  // The default view is the category itself, so it does not repeat its own label.
  const def = app.pageTitle({ ...app.newState(), view: app.DEFAULT_VIEW });
  assert.equal(def, `${app.CATEGORIES[app.DEFAULT_CATEGORY].label} · ${app.SITE_TITLE}`);
  // Home is the level above the categories and titles itself as the site.
  assert.equal(app.pageTitle({ ...app.newState(), view: app.HOME_VIEW }), app.SITE_TITLE);
});

test("#8 FOOTER: 'measured on an older version' and 'not yet measured' are two facts, counted apart", () => {
  /* WHAT SHIPPED. The footer read "Benchmark version: 4c45e0b (7 rows measured on an older version)". There
     were ZERO rows on an older engine. The seven were gateways carrying `engine: {sha: null}` - never
     measured on this benchmark at all, several of them mid-run. The count was `!engine.current`, which is
     true for both:
       a DIFFERENT sha  -> the row has numbers, and they are not comparable with the rest;
       NO sha at all    -> the row has no numbers.
     So the board asserted "we are showing you stale results for half the field" when the truth was "we have
     none for it yet" - a false statement about the board's own trustworthiness, in the one line a reader
     consults to decide whether to trust it. The null stamp is deliberate upstream (a row with no stamp must
     not imply it matches the board's engine); the defect was reading absence as a version. */
  const cur = (sha) => ({ engine: { sha, short: sha.slice(0, 7), current: true } });
  const old = (sha) => ({ engine: { sha, short: sha.slice(0, 7), current: false } });
  const none = () => ({ engine: { sha: null, short: null, current: false } });
  const stamp = (gateways) => app.benchmarkVersionStamp({ benchmark_version: "4c45e0b4fa92", gateways });

  // THE LIVE SHAPE: seven current, seven never measured, none behind. It must not mention an older version.
  const live = stamp([...Array(7)].map(() => cur("4c45e0b4fa92")).concat([...Array(7)].map(none)));
  assert.ok(!/older version/.test(live), `no row is behind, so nothing may say one is: ${live}`);
  assert.match(live, /7 not yet measured on it/, "and the seven unmeasured rows are counted as what they are");

  // A GENUINELY BEHIND ROW still gets the original sentence - that fact is real and worth flagging.
  const behind = stamp([cur("4c45e0b4fa92"), old("beefcafe1234")]);
  assert.match(behind, /1 row measured on an older version/, "singular, and named as an older version");
  assert.ok(!/not yet measured/.test(behind), "and a clause for an empty set is not rendered");

  // BOTH NON-EMPTY: both are stated, because they are different facts about different rows.
  const mixed = stamp([cur("4c45e0b4fa92"), old("beefcafe1234"), old("d00dfeed5678"), none(), none(), none()]);
  assert.match(mixed, /2 rows measured on an older version/);
  assert.match(mixed, /3 not yet measured on it/);

  // A CLEAN BOARD reads as one bare version, with no parenthesis at all.
  assert.equal(stamp([cur("4c45e0b4fa92"), cur("4c45e0b4fa92")]), "Benchmark version: 4c45e0b");
  // And a bundle with no version stamped contributes nothing rather than the word "unknown".
  assert.equal(app.benchmarkVersionStamp({ gateways: [none()] }), "");
  assert.equal(app.benchmarkVersionStamp(null), "");
});

/* ============================================================================================
   THE MEMORY TAB REVIEW: correct numbers placed so as to look contradictory.
   Not one of these is a data bug. Every figure checked out. The defect in each case is a LABEL that
   does not name the scope, the window, or the shape it belongs to - so a reader comparing two correct
   numbers on one row concludes one of them is wrong.
   ============================================================================================ */

// A synthetic memory record, from raw intent. Sealed through the real sealMetric like every other fixture.
function memWin(o = {}) {
  const { idle = 178.1, idleSeries = null, series = null, steady = null, recovered = null,
    peak = null, growth = 0, plateaued = true, cell = "openai" } = o;
  const rec = { path: { ingress: cell, egress: cell, dialect: cell }, source: SRC("matrix", "6x6-memory-diagonal"),
    idle_window_s: 60, recovery_window_s: 30, plateaued };
  rec.idle_rss_mib = seal(idle);
  if (steady != null) rec.steady_state_rss_mib = seal(steady);
  if (peak != null) rec.peak_rss_mib = seal(peak);
  if (recovered != null) rec.recovered_rss_mib = seal(recovered);
  if (growth != null) rec.growth_rate_mib_per_min = seal(growth);
  if (idleSeries) rec.idle_rss_series = idleSeries;
  if (series) rec.rss_series = series;
  return rec;
}
// A series of `n` samples over `secs`, from a function of the sample index.
const seriesOf = (n, secs, f) => Array.from({ length: n }, (_, i) => ({ t_s: Math.round((i / (n - 1)) * secs), rss_mib: f(i, n) }));

test("MEMORY #4: the idle caption describes the SHAPE, never an implied rate across the window", () => {
  /* WHAT SHIPPED, AND WHY IT MISLED THE PERSON WHO WROTE THE AUDIT. The caption read
     "median 178.1 MiB · spanned 171.5–178.1 MiB (6.59 MiB over 59 s at rest)". "6.59 MiB over 59 s" reads as
     6.59 MiB of drift accumulating across the window. apisix does nothing of the kind: it sits at 178.1 for
     127 of its 130 samples and then steps DOWN 6.594 MiB at 98% through and holds - one late release, visible
     in the sparkline as a cliff at the right edge. "over 59 s" was the window LENGTH, not a duration over
     which anything moved, and the template applied it to all four real shapes on the board identically. */
  const at = (v) => () => v;
  // The four real shapes, from the board's own series (verified against data.json).
  const flat = seriesOf(123, 59, (i) => 42.8203125 + (i > 60 ? 0.0859375 : 0));          // helicone
  const lateStep = seriesOf(130, 59, (i) => (i < 127 ? 178.078125 : 171.484375));         // apisix
  const earlyStep = seriesOf(130, 59, (i) => (i < 5 ? 151.0 : 252.5));                    // bifrost
  const gradual = seriesOf(120, 59, (i) => 200 + (i / 119) * 20);                          // none on the board yet
  const sp = (series, idle) => app.rssSparkline(series, null, idle, "idle");

  // 1. NO SURFACE MAY STATE A SPAN "OVER" THE WINDOW LENGTH. That phrasing is the defect itself.
  for (const [name, svg] of [["flat", sp(flat, 42.91)], ["late", sp(lateStep, 178.08)],
    ["early", sp(earlyStep, 222.6)], ["gradual", sp(gradual, 210)]]) {
    assert.ok(!/MiB over \d+ s/.test(svg),
      `${name}: "X MiB over N s" reads as a rate across the window, which is not what a span is: ${svg.match(/class="stamp[^>]*>([^<]*)/)?.[1]}`);
  }

  // 2. THE FOUR SHAPES MUST BE DISTINGUISHABLE IN WORDS, because the picture alone cannot separate a late
  //    step from gradual drift once the axis is floored, and the span alone cannot either.
  const stampOf = (svg) => svg.match(/class="stamp muted">([^<]*)</)[1];
  const [sFlat, sLate, sEarly, sGrad] = [sp(flat, 42.91), sp(lateStep, 178.08), sp(earlyStep, 222.6), sp(gradual, 210)].map(stampOf);
  assert.equal(new Set([sFlat, sLate, sEarly, sGrad]).size, 4, "four shapes, four different sentences");

  // 3. AND EACH NAMES ITS OWN SHAPE, with direction and position - the facts that distinguish them.
  assert.match(sFlat, /flat/i, `a window inside the noise floor is flat: ${sFlat}`);
  assert.match(sLate, /step/i, `a single late move is a step: ${sLate}`);
  assert.match(sLate, /down/i, "and its direction is stated - apisix RELEASED, it did not drift up");
  assert.match(sLate, /end/i, "and its position: near the end of the window");
  assert.match(sEarly, /first/i, `an early move is placed at the start: ${sEarly}`);
  assert.ok(!/down/i.test(sEarly), "bifrost's step is UPWARD and must not read as a release");
  assert.match(sGrad, /gradual/i, `only a genuinely spread-out span is gradual: ${sGrad}`);
  // The late step and the early step must not be confusable, which is the pair that fooled the auditor.
  assert.ok(!/first/i.test(sLate), "a late step is not an early one");
  assert.ok(!/end/i.test(sEarly), "and an early step is not a late one");
});

test("MEMORY #2: two medians on one row are labelled by SCOPE, so neither reads as wrong", () => {
  /* THE ROW THAT MISLED. apisix: the `Idle RSS (MiB)` column reads 177.9 and the sparkline caption six
     inches to its right reads "median 178.1 MiB". Both correct. The column is the median ACROSS CELLS (idle
     is sampled cold before any request, so no cell is involved - which is exactly what the column's own
     tooltip says); the sparkline is the SELECTED CELL's own window. The tooltip explained it; the row did
     not, and a reader comparing the two concludes one is broken. It is six rows on the live board, not one,
     and bifrost's pair differs by 21.7 MiB (244.3 vs 222.6). */
  const cellMedian = 178.078125;
  const series = seriesOf(130, 59, (i) => (i < 127 ? cellMedian : 171.484375));
  const svg = app.rssSparkline(series, null, cellMedian, "idle");
  // THE CAPTION SAYS WHOSE MEDIAN IT IS. Not "median X" - "this cell's median X".
  assert.match(svg, /this cell: median/i, `the sparkline's median must name its scope: ${svg.match(/class="stamp muted">([^<]*)</)[1]}`);
  const idleStamp = svg.match(/class="stamp muted">([^<]*)</)[1];
  // EXACTLY ONE "median" on the caption, and it is the scoped one: an unqualified second occurrence would
  // re-create the ambiguity the scope was added to remove.
  assert.equal((idleStamp.match(/median/g) || []).length, 1, `one median, scoped: ${idleStamp}`);
  assert.match(idleStamp, /this cell: median/, "and the scope is attached to it, not stated elsewhere");
  assert.ok(!/&#3\d;/.test(idleStamp), "and the caption carries no HTML entity - it is read, not parsed");
  // AND THE COLUMN SAYS WHOSE ITS OWN IS, in the header, without hovering.
  const st = { ...app.newState(), view: "memory", mode: "min" };
  const g = { key: "a", display: "A", lang: "Lua", matrix: { upstreams: {
    openai: { cells: { openai: { served: true, memory: memWin({ idle: cellMedian, idleSeries: series, series, steady: 200, peak: 208, recovered: 207.8 }) } } },
    anthropic: { cells: { anthropic: { served: true, memory: memWin({ idle: 176.828125, cell: "anthropic", series, steady: 201, peak: 209, recovered: 208 }) } } },
  } } };
  Object.assign(app.state, { data: { gateways: [g] } });
  try {
    const col = app.COLUMN_SETS.memory.find((c) => c.id === "memidle");
    assert.match(txtOf(col.label), /all cells/i,
      `the idle column's header must state that it is the across-cell median: ${txtOf(col.label)}`);
    // The number itself is untouched: still the median across the gateway's cold samples.
    const cell = col.get(g, { ...st, data: { gateways: [g] } });
    assert.equal(cell.v, app.idleAcrossCells(g).median, "the COLUMN's value is unchanged - this is a labelling fix");
    assert.match(cell.note, /one per served cell/, "and its tooltip still discloses the basis");
  } finally { Object.assign(app.state, app.newState()); }
});

test("MEMORY #3: two 'recovered' figures on one row are separated by the WINDOW each belongs to", () => {
  /* One-API: the column `Recovered @30 s` reads 139.1 and the caption reads "recovered 129.6 MiB (365 s)".
     Both true - it kept releasing after the 30 s mark - but two numbers under one word reads as an
     inconsistency. The column already names its window; the caption named none, so the difference looked
     like a difference of fact rather than of when it was read. */
  const series = seriesOf(200, 365, (i, n) => (i < n * 0.9 ? 144.4 : 129.6));
  const svg = app.rssSparkline(series, null, 82.3, "load", { recoveredAt: 139.140625, recoveryWindowS: 30 });
  const stamp = svg.match(/class="stamp muted">([^<]*)</)[1];
  // BOTH POINTS, IN ORDER, EACH WITH ITS OWN WINDOW: the difference becomes legible as a timeline.
  assert.match(stamp, /139\.1/, `the column's own figure appears, so the row cannot look self-contradictory: ${stamp}`);
  assert.match(stamp, /129\.6/, "and so does the end of the observation");
  assert.match(stamp, /30 s/, "the recovery mark is named");
  assert.ok(!/recovered 129\.6/.test(stamp), "the last sample is no longer called 'recovered' - that word is the column's");
  // WHEN THE TWO AGREE (apisix: 207.8 at the mark and at the end) it collapses to one figure, not a
  // pointless restatement of the same number twice.
  const flatTail = seriesOf(200, 360, (i, n) => (i < n * 0.85 ? 178.1 + (i / (n * 0.85)) * 37 : 207.8203125));
  const same = app.rssSparkline(flatTail, null, 178.1, "load", { recoveredAt: 207.8203125, recoveryWindowS: 30 });
  const s2 = same.match(/class="stamp muted">([^<]*)</)[1];
  assert.equal((s2.match(/207\.8/g) || []).length, 1, `one figure when there is only one: ${s2}`);
  // With no recovered scalar published at all, it still says what the last sample is without claiming a window.
  const bare = app.rssSparkline(series, null, 82.3, "load");
  assert.match(bare.match(/class="stamp muted">([^<]*)</)[1], /129\.6/);
});

test("MEMORY #5: releasing nothing and releasing most of it do not render with identical emphasis", () => {
  /* From the board: TensorZero peaks at 65.8 and recovers to 65.8 - it releases NOTHING of the ~19 MiB it
     gained. Bifrost peaks at 870.0 and comes back to 580.3. Those two curves ended up with the same visual
     weight: a line, a dot, and two numbers in the same grey. No new metric and no verdict is invented here -
     the drop is drawn from the two levels already plotted, so a curve that gave nothing back has no mark and
     one that gave a lot back has a tall one. */
  const nothing = seriesOf(200, 362, (i, n) => (i < n / 2 ? 46.7 + (i / (n / 2)) * 19.1 : 65.8));
  const lots = seriesOf(200, 360, (i, n) => (i < n * 0.8 ? 222.6 + (i / (n * 0.8)) * 647 : 580.3));
  const a = app.rssSparkline(nothing, null, 46.7, "load", { recoveredAt: 65.796875, recoveryWindowS: 30 });
  const b = app.rssSparkline(lots, null, 222.6, "load", { recoveredAt: 580.3, recoveryWindowS: 30 });
  assert.ok(!/class="rss-release"/.test(a), "a gateway that released nothing gets no release mark - there is nothing to draw");
  assert.match(b, /class="rss-release"/, "a gateway that released a lot gets one");
  assert.match(b, /<title>released [\d.,]+ MiB of the [\d.,]+ MiB it gained/,
    "titled from the levels already on the chart, stating both, inventing neither");
  // It is drawn from the plotted geometry, so its height tracks the fall rather than being a fixed badge.
  const h = (svg) => { const m = svg.match(/class="rss-release" x1="[\d.]+" y1="([\d.]+)" x2="[\d.]+" y2="([\d.]+)"/); return m ? Math.abs(+m[2] - +m[1]) : 0; };
  assert.ok(h(b) > 5, `a 290 MiB fall must be a visible mark, got ${h(b)}px`);
  assert.equal(h(a), 0, "and no fall must be no mark");
  // The IDLE panel never carries one: nothing has been released in a window taken before any load.
  assert.ok(!/rss-release/.test(app.rssSparkline(lots, null, 222.6, "idle")));
});

test("MEMORY #1: the table is not sized by its two widest columns, and no header breaks mid-word", () => {
  /* Matthew's report: `Recovered @30 s` rendered as "Recovere / d @30 s". `Tested on` was sized by its
     longest pill (OpenAI→Bedrock Converse) and `RSS curve` took roughly half the table, so the four numeric
     columns - the content - were crushed into a narrow band in the middle (70/73/93/73 px against 185 and
     446). Fixed by constraining the two offenders, not by shrinking the numbers. */
  const css = readFileSync(join(HERE, "style.css"), "utf8");
  /* THE MID-WORD BREAK. `overflow-wrap: anywhere` on the header is what did it: squeezed to 73px, the header
     was allowed to break inside "Recovered". A header may wrap between words and must never split one. */
  const headRule = css.match(/#results-table thead th \{[^}]*\}/)[0];
  assert.ok(!/overflow-wrap:\s*anywhere/.test(headRule),
    `a header that may break anywhere breaks inside words: ${headRule}`);
  assert.match(headRule, /word-break:\s*keep-all|overflow-wrap:\s*normal/,
    "the header must be pinned to wrapping at spaces only");
  // THE CURVE COLUMN IS CAPPED. It grew to 446px because the stamp under the sparkline is one long nowrap
  // line; the svg itself is a fixed 260px and needs no more than that.
  const curve = css.match(/td\.memcurve \{[^}]*\}/)[0];
  assert.match(curve, /max-width:/, `the curve column must not expand past the sparkline: ${curve}`);
  assert.match(css, /\.rss-spark \.stamp \{[^}]*white-space:\s*normal/,
    "and its caption must wrap rather than widening the column to fit one line");
  // THE PILL IS CONSTRAINED, with the full value still reachable - it already carries a tooltip.
  assert.match(css, /#results-table td\.tested \{[^}]*max-width:/,
    "the Tested-on column is capped so its longest pill cannot size the table");
  assert.match(css, /\.tested-pill \{[^}]*(text-overflow:\s*ellipsis|overflow-wrap)/,
    "and the pill truncates or wraps inside that cap instead of overflowing it");
});

test("MEMORY #0: the row is ONE lifecycle curve, short, with the axis break SHOWN", () => {
  /* THE OWNER: "while i get why, massive rows are not professional". A memory row was ~350px tall - six block
     elements stacked in one cell (label, sparkline, caption, label, sparkline, caption) - so three gateways
     filled a screen and a fourteen-row comparison table stopped being comparable.
     THE RESTRUCTURE: idle and load+recovery are not two experiments, they are ONE process's lifetime in time
     order. The row draws it as one line. Four of the six stacked elements go away, and the remaining figures
     move to the control's accessible name and to the drawer - moved, not deleted. */
  const idleSeries = seriesOf(120, 59, () => 46.7);
  const series = seriesOf(200, 360, (i, n) => (i < n * 0.8 ? 46.7 + (i / (n * 0.8)) * 19.1 : 55.0));
  const mem = memWin({ idle: 46.7, idleSeries, series, steady: 65.8, peak: 65.8, recovered: 55.0 });

  const compact = app.rssCurves(mem, { compact: true });
  const full = app.rssCurves(mem);

  // ONE curve inline, not two panels; the stacked pair survives only in the drawer.
  assert.equal((compact.match(/<svg/g) || []).length, 1, "one inline sparkline, not two");
  assert.ok(!/rss-pair|rss-half|rss-label/.test(compact), "and none of the stacked wrappers or prose labels");
  assert.equal((full.match(/<svg/g) || []).length, 2, "the drawer keeps the two separated windows");
  assert.match(full, /rss-pair/, "in the stacked pair");
  // NO CAPTION LINE IN THE ROW - that was two of the six stacked elements.
  assert.ok(!/class="stamp muted"/.test(compact), "no caption line in the table cell");
  assert.match(full, /class="stamp muted"/, "the drawer still shows the captions");
  // SHORT. The inline curve must be in a frontier row's league, not three times it.
  const box = compact.match(/viewBox="0 0 (\d+) (\d+)"/);
  const [w, h] = [Number(box[1]), Number(box[2])];
  assert.ok(h <= 40, `the inline curve must be short, got ${h}px`);
  assert.ok(w <= 200, `and narrow enough to leave the numeric columns room, got ${w}px`);

  /* THE AXIS BREAK IS DRAWN, AND THAT IS THE HONESTY REQUIREMENT. The two windows are wildly different
     lengths (~59 s at rest against ~360 s under load). On a true shared time axis the at-rest window would be
     14% of the width and bifrost's whole finding - it allocates ~100 MiB in its first 2 SECONDS - would be
     under 1% of it, invisible. So the at-rest segment is given more width than its duration earns, and
     BECAUSE it is, the discontinuity has to be visible: silently smoothing two time scales into one line
     would be a picture asserting when things happened, wrongly. */
  assert.match(compact, /class="rss-break"/, "the time-axis discontinuity is drawn, not assumed away");
  assert.match(compact, /<title>[^<]*not continuous[^<]*<\/title>/, "and says so in words on hover");
  // Two separate paths, so no drawn line crosses the break.
  assert.equal((compact.match(/<path d="M/g) || []).length, 2, "the two segments are two paths, never one");
  assert.ok(app.LIFECYCLE_IDLE_FRAC > 0 && app.LIFECYCLE_IDLE_FRAC < 1, "the split is a declared fraction");
  // With no at-rest series there is no break to draw and no gap to explain.
  const loadOnly = app.rssCurves(memWin({ idle: 46.7, series, peak: 65.8, recovered: 55.0 }), { compact: true });
  assert.ok(!/rss-break/.test(loadOnly), "a single-window record draws no break it does not have");
  assert.equal((loadOnly.match(/<path d="M/g) || []).length, 1, "and one path");

  /* NOTHING BECAME UNREACHABLE. Every figure the captions carried is on the control's ACCESSIBLE NAME, not
     in a hover-only tooltip: hover-only content is invisible to touch and to keyboard users, which is
     deletion for some readers. */
  const g = { key: "t", display: "T", lang: "Rust", matrix: { upstreams: {
    openai: { cells: { openai: { served: true, memory: mem } } } } } };
  const st = { ...app.newState(), view: "memory", mode: "min", data: { gateways: [g] } };
  Object.assign(app.state, { data: { gateways: [g] } });
  try {
    const td = app.COLUMN_SETS.memory.find((c) => c.id === "memcurve").render(g, st);
    assert.match(td, /<button type="button"[^>]*aria-label="/, "the curve sits in a focusable control with a name");
    const name = td.match(/aria-label="([^"]*)"/)[1];
    assert.match(name, /this cell: median 46\.7 MiB/, "the scoped median is reachable by keyboard");
    assert.match(name, /recovery mark/, "and the windowed recovery figure");
    assert.match(name, /At rest \(60 s, before any request\)/, "and which window is which");
    assert.match(name, /Under load: peak/, "and the other window, named");
    assert.match(name, /Released 10\.\d MiB of the 19\.\d MiB it gained/, "and the release finding, quantified");
    assert.match(name, /Click the row/, "and where the full detail is");
    // The shape phrasing survived the restructure, and the rate-implying form did not come back with it.
    assert.match(name, /no movement at all/, "the shape note travels into the fold");
    assert.ok(!/MiB over \d+ s/.test(name), "and never as a magnitude paired with the window length");
    /* "RELEASED NONE" IS SAID OUT LOUD. TensorZero peaks at 65.8 and ends at 65.8, giving back none of the
       ~19 MiB it gained - the most interesting thing on its row - and silence there reads as an absent
       measurement rather than as the finding it is. */
    const held = memWin({ idle: 46.7, idleSeries, steady: 65.8, peak: 65.8, recovered: 65.796875,
      series: seriesOf(200, 362, (i, n) => (i < n / 2 ? 46.7 + (i / (n / 2)) * 19.1 : 65.796875)) });
    assert.match(app.memCurveSummary(held), /Released none of the 19\.1 MiB it gained/,
      "a gateway that released nothing says so, rather than saying nothing");
  } finally { Object.assign(app.state, app.newState()); }

  /* BOARD-LEVEL FACTS ARE STATED ONCE, not fourteen times inside the cell whose height was the complaint. */
  assert.ok(!/\(\d{2,3} s\)/.test(compact), "no per-row window length in the curve");
  const cap = app.captionText(app.memoryCaption({ gateways: [g] }, st));
  assert.match(cap, /same for every gateway/, "the caption owns the window lengths");
  assert.match(cap, /time axis changing scale/, "and explains the break once, for the whole table");
});

test("UI: the two filter axes are ONE labelled control block, not two ragged rows of chips", () => {
  /* THE OWNER, LOOKING AT THE LIVE FRONTIER TAB: "filter formatting is ugly, align them", "generally needs
     some ui help". Two stacked control rows:
         Tail-latency bound  [1 ms][5 ms][10 ms][50 ms][100 ms][no bound]  showing the req/s ...
         [Own cell][Same][Custom]
     Row 1 had a text label that indented its chips; row 2's chips started flush at the far left, so the two
     read as ragged. The trailing sentence on row 1 ran to a width nothing else on the page shared.
     THE FIX IS NOT AN INDENT ON ROW 2. These are TWO DIFFERENT AXES - which tail bound, and which cell -
     and the second one was never named at all, which is why it looked like loose chips rather than a control.
     Both axes get a label in a shared max-content gutter, so one left edge falls out of the layout rather
     than being hand-tuned; the sentence drops to its own line inside the controls column, where it starts at
     the same left edge as the chips and is capped to a readable measure instead of the page width. */
  const html = readFileSync(join(HERE, "index.html"), "utf8");
  const css = readFileSync(join(HERE, "style.css"), "utf8");

  // ONE BLOCK, containing both axes, so their alignment is a property of the layout and not a coincidence.
  assert.match(html, /<div class="filters" id="filters">/, "the two axes live in one control block");
  // Bounded from the block's own start: index.html has an earlier .table-scroll (the roster table), so an
  // unanchored indexOf for the terminator finds it and slices backwards to nothing.
  const from = html.indexOf('id="filters"');
  const block = html.slice(from, html.indexOf('<div class="table-scroll">', from));
  assert.ok(block.includes('id="bound-chooser"'), "the bound axis is inside it");
  assert.ok(block.includes('id="cell-chooser"'), "and so is the cell axis");

  // BOTH AXES ARE NAMED. The cell chooser's label is the half that did not exist.
  assert.match(block, /class="flabel"[^>]*id="bound-legend">Tail-latency bound</, "the bound axis names itself");
  assert.match(block, /class="flabel"[^>]*id="mode-legend">/, "and so does the cell axis, which never used to");
  assert.equal((block.match(/class="flabel"/g) || []).length, 2, "exactly two axes, exactly two labels");
  // The seg keeps its accessible name pointed at the visible label rather than duplicating it in an attribute.
  assert.match(block, /id="mode-seg"[^>]*aria-labelledby="mode-legend"/, "the mode seg is labelled by the visible label");

  /* THE SHARED LEFT EDGE COMES FROM A GRID, not from a hand-picked indent: a max-content label column is
     exactly as wide as the longer of the two labels, so neither row can drift when the wording changes. */
  const grid = css.match(/\.filters \{[^}]*\}/);
  assert.ok(grid, "the block has a layout rule");
  assert.match(grid[0], /display:\s*grid/, `the two axes are laid out as a grid: ${grid[0]}`);
  assert.match(grid[0], /grid-template-columns:\s*max-content/, "with a label gutter sized to its content");
  // The groups are subgrid-transparent, so hiding one axis removes its whole row and shifts nothing sideways.
  assert.match(css, /\.filters > \.control-group \{[^}]*display:\s*contents/,
    "each axis is display:contents so its label and chips are items of the ONE grid");

  // THE SENTENCE IS ON ITS OWN LINE, ALIGNED WITH THE CHIPS, and capped to a measure.
  const note = css.match(/\.filters \.fnote \{[^}]*\}/);
  assert.ok(note, "the explanatory sentence has its own rule");
  assert.match(note[0], /grid-column:\s*2/, "it starts at the chips' left edge, not the label's");
  assert.match(note[0], /max-width:/, "and is capped rather than running to whatever width the page happens to be");

  // CONSISTENT CHIP GEOMETRY across both axes: "1 ms" and "100 ms" must not be two different-sized chips.
  assert.match(css, /#bound-seg \.seg-btn \{[^}]*min-width:/,
    "the bound chips share a minimum width, so the row is not ragged inside itself either");
});

test("UI: column geometry is FIXED, so changing a filter never moves a column sideways", () => {
  /* THE OWNER: "changing filters shouldn't change column widths, just an annoyance." Measured on the live
     board with getBoundingClientRect across filter combos, every table view drifted at every width. The
     frontier tab at 1440, first body row, per column:
         mode=peak   bound=10   36 165  90 118 118 118 118 118  93 165
         mode=same   bound=10   36 165  84 119 119 119 119 119  93 167
         mode=custom bound=10   36 165 144 125 125 125  80  80  87 173
     Nothing about the measurement changed - only which cell each row reads - and the whole grid re-solved.

     THE CAUSE IS AUTO TABLE LAYOUT: widths are derived from the cells that happen to be rendered, so a
     filter that swaps `20,119` for `20,389`, or a passthrough pill for `OpenAI→Bedrock Converse`, or a
     number for `no rung held this tail`, re-measures every column. The widest content a column CAN hold is
     usually not in the current combo, so sizing from what is on screen can only ever be unstable.

     THE FIX IS TO DECLARE THE GEOMETRY: table-layout: fixed plus a colgroup built from the column set, so a
     column's width is a property of the COLUMN and not of the rows currently passing through it. Excess width
     is then distributed proportionally over the declared widths, which is also content-independent, so the
     table still fills its container without any column consulting a cell. */
  const css = readFileSync(join(HERE, "style.css"), "utf8");
  const rule = css.match(/#results-table \{[^}]*\}/);
  assert.ok(rule, "#results-table has its own rule");
  assert.match(rule[0], /table-layout:\s*fixed/,
    `auto layout sizes columns from the rendered cells, which is exactly what makes a filter move them: ${rule[0]}`);

  // THE WIDTHS ARE DECLARED PER COLUMN, in one place, and every column in every table view has one.
  for (const view of ["performance", "frontier", "streaming", "memory"]) {
    /* BOTH the declared set and the set actually RENDERED. columnsFor() drops a column the current bundle
       cannot fill (memory sheds `memgrowth` without per-cell windows), and the <col>-to-column mapping is
       POSITIONAL: a colgroup built from the superset while the body renders the subset would put every width
       on the wrong column - stable geometry, wrong geometry. */
    for (const set of [app.COLUMN_SETS[view], app.columnsFor(view)])
      assert.equal((app.colgroupHtml(set).match(/<col /g) || []).length, set.length,
        `${view}: the colgroup is built from the columns being rendered, one <col> each`);
    const cols = app.COLUMN_SETS[view];
    const cg = app.colgroupHtml(cols);
    assert.equal((cg.match(/<col /g) || []).length, cols.length,
      `${view}: one <col> per column, or the declared widths land on the wrong columns`);
    assert.ok(!/width:\s*(undefined|null|auto|)\s*[;"]/.test(cg), `${view}: every column declares a real width: ${cg}`);
    for (const c of cols)
      assert.ok(app.colWidth(c) && /rem|px|%/.test(app.colWidth(c)), `${view}.${c.id} declares a width`);
  }
  /* THE TWO OVER-WIDE COLUMNS ARE THE ONES THAT GIVE. The owner's other complaint - "column widths - Tested
     on is huge (cos of 1 large openai > cohere)" - is the same defect seen from the other side: the column
     was sized by its longest pill in one mode and starved the numbers in every mode. Declared geometry fixes
     both, and the declaration has to keep the identity/annotation columns narrower than the numeric ones or
     it has bought stability at the cost the owner objected to. */
  const px = (w) => (w.endsWith("rem") ? parseFloat(w) * 16 : parseFloat(w));
  const w = (view, id) => px(app.colWidth(app.COLUMN_SETS[view].find((c) => c.id === id)));
  assert.ok(w("frontier", "tested") < w("frontier", "f10"),
    "Tested on is an annotation and must not be wider than a column of readings");
  assert.ok(w("memory", "tested") < w("memory", "mempeak") * 1.6,
    "and on memory it must not be sized by OpenAI→Bedrock Converse either");

  /* AND THE FRONTIER'S TEN COLUMNS STILL FIT A NARROW DESKTOP. At 1024 the curve column was cut off by the
     scroll edge - the sparkline sliced in half, the "no rung held any bound" note clipped mid-word - because
     the table was 96px wider than its container. The declared geometry has to be narrower than that, and the
     width it gives back comes from Tested on and the curve, never from the numbers. */
  const sum = app.COLUMN_SETS.frontier.reduce((t, c) => t + px(app.colWidth(c)), 0);
  assert.ok(sum <= 984, `the frontier's declared widths must fit a 1024 viewport's table area, got ${sum}px`);

  // The sub-lines under a reading WRAP. They are the only nowrap content wide enough to force a numeric
  // column past its declared width, and a number that wraps reads as two numbers while a reason does not.
  for (const cls of ["reading-none", "reading-tail", "zero-why"])
    assert.match(css, new RegExp(`\\.${cls} \\{[^}]*white-space:\\s*normal`),
      `.${cls} must wrap inside its declared column rather than widening it`);
});

/* SCOPED RE-RUN LAYERING. A 4-cell re-run must not delete 32 measured cells, and a cell it DID
   measure must win and carry its own provenance. Both directions matter: refuse to layer and the
   re-run is pointless; layer too eagerly and a genuine re-run that found less is silently discarded. */
test("a scoped re-run layers over the full run instead of replacing it", () => {
  const cell = (served, tag) => ({ served, stream: { stream_served: true, tag } });
  const mk = (coords) => {
    const m = { upstreams: {}, cells: {} };
    for (const [eg, ing, tag] of coords) {
      (m.upstreams[eg] ??= { cells: {} }).cells[ing] = cell(true, tag);
      m.cells[ing] = m.upstreams[eg].cells[ing];
    }
    return m;
  };
  const full = mk([["openai", "openai", "old"], ["openai", "anthropic", "old"],
                   ["gemini", "openai", "old"], ["gemini", "cohere", "old"]]);
  const scoped = mk([["openai", "openai", "new"]]);

  const fullC = snapshotCellCoords(full), scopedC = snapshotCellCoords(scoped);
  assert.equal(fullC.size, 4);
  assert.ok(isStrictSubset(scopedC, fullC), "1 of 4 cells is a strict subset and must be treated as scoped");
  assert.ok(!isStrictSubset(fullC, fullC), "an identical cell set is NOT a subset - a full re-run replaces");
  assert.ok(!isStrictSubset(new Set(["x|y"]), fullC), "a set with a foreign coord is not a subset");

  const merged = layerScopedMatrix(full, scoped, { build: "beef", measured_at: "2026-08-01T00:00:00Z", __file: "s.json" });
  assert.equal(snapshotCellCoords(merged).size, 4, "layering must not drop the 3 cells it never looked at");
  assert.equal(merged.upstreams.openai.cells.openai.stream.tag, "new", "the re-measured cell must win");
  assert.equal(merged.upstreams.gemini.cells.cohere.stream.tag, "old", "and the untouched ones must survive");

  // Provenance: the layered cell says which run produced it; the untouched ones make no such claim.
  assert.equal(merged.upstreams.openai.cells.openai.__run.build, "beef",
    "a spliced cell must carry the run that measured it, or the board dates it by its neighbours");
  assert.equal(merged.upstreams.gemini.cells.cohere.__run, undefined,
    "a cell that was not re-measured must not be stamped with a run that never touched it");
  assert.deepEqual(merged.__layered.cells, ["openai>openai"], "the matrix records exactly what was layered");

  // The v1-compat top-level row must not disagree with the grid it mirrors.
  assert.equal(merged.cells.openai.stream.tag, "new", "the v1 row must follow the layered grid");

  // The input must not be mutated - gen-data layers repeatedly over a shared base.
  assert.equal(full.upstreams.openai.cells.openai.stream.tag, "old", "layering must be pure");
});

/* A DERIVED LANE MAY NOT BE MORE CERTAIN THAN WHAT IT IS DERIVED FROM.
   The dollar lanes are computed from the frontier rate at the priced bound. When that rate is
   absent, `rate` used to collapse to 0 and `rps_per_dollar` published {value: 0, certified: true} -
   the board asserting as a MEASUREMENT that a gateway delivers zero requests per dollar, and
   ranking it last on a higher-is-better axis for it. It shipped on one-api, plano and tensorzero
   because neither lane is in seal.mjs's vocabulary, so `isMetricField` is false for them and
   check-consistency never compared either against the raw artifact. Nothing was watching. */
test("the dollar lanes are never more certain than the rate they are derived from", () => {
  let checkedAbsent = 0, checkedMeasured = 0;
  for (const g of data.gateways) {
    const bc = g.best_cell;
    if (!bc || !bc.frontier) continue;
    const reading = bc.frontier.find((f) => f.bound_ms === bc.priced_at_bound_ms);
    if (!reading || !reading.rps) continue;
    const rate = reading.rps;
    for (const lane of ["rps_per_dollar", "cost_per_million_usd"]) {
      const env = bc[lane];
      if (!env) continue;
      if (rate.value == null) {
        checkedAbsent++;
        assert.equal(env.value, null,
          `${g.key}.${lane} publishes ${env.value} while its rate is absent (${rate.reason})`);
        assert.equal(env.certified, false, `${g.key}.${lane} certifies a value it never measured`);
        // And it must carry the RATE's reason, not a flattened stand-in: "measured, and it could
        // not hold the bound" is a different finding from "nothing was measured here", and the
        // second is the one that flatters the gateway.
        assert.equal(env.reason, rate.reason,
          `${g.key}.${lane} reports ${env.reason} where the rate says ${rate.reason} - a flattened reason`);
      } else if (rate.value > 0 && lane === "rps_per_dollar") {
        checkedMeasured++;
        assert.equal(env.certified, true, `${g.key}.${lane} must certify a rate that WAS measured`);
        assert.ok(env.value > 0, `${g.key}.${lane} must be positive for a positive rate`);
      }
    }
  }
  assert.ok(checkedAbsent > 0, "this board must contain an absent-rate cell or the guard is untested");
  assert.ok(checkedMeasured > 0, "and a measured one, or it only proves the absent half");
});

/* A NON-NUMBER MUST NOT BE CERTIFIED. Number("n/a") is NaN, NaN === 0 is false, so a non-numeric
   raw fell to sealMetric's certified branch and JSON.stringify turned it into null - publishing
   {value: null, certified: true}, a bare null wearing the certified badge, in a shape none of the
   three documented envelope forms allow. */
test("sealMetric refuses to certify anything that is not a finite number", () => {
  for (const bad of ["n/a", "", NaN, Infinity, -Infinity, {}, [1, 2]]) {
    const env = sealMetric(bad);
    assert.equal(env.certified, false, `sealMetric certified ${JSON.stringify(bad)}`);
    assert.equal(env.value, null, `sealMetric published a value for ${JSON.stringify(bad)}`);
    assert.ok(env.reason, `an absence must carry a reason, ${JSON.stringify(bad)} carried none`);
  }
  // And the real cases are untouched: a measured zero is still certified, as is any finite number.
  assert.equal(sealMetric(0).certified, true, "a measured zero is a real number");
  assert.equal(sealMetric(0).value, 0);
  assert.equal(sealMetric(12.5).value, 12.5);
  // A numeric string is still a number - the producer's shape, not a fabrication.
  assert.equal(sealMetric("42").value, 42);
});

/* A v1-SHAPED SNAPSHOT IS NOT AN EMPTY ONE. snapshotCellCoords only walked `upstreams`, so a matrix
   carrying its cells under the bare v1 `cells` row counted ZERO coords - and an empty set is a
   strict subset of everything, so such a snapshot could never be the base and layered nothing
   either. The newest run would vanish from the board with no warning, while normalizeMatrix and
   app.js both still treat that row as real measured cells. */
test("a v1-shaped matrix contributes coords instead of reading as empty", () => {
  const v1 = { cells: { openai: { served: true }, anthropic: { served: true } } };
  const coords = snapshotCellCoords(v1);
  assert.equal(coords.size, 2, "the v1 row's cells must count");
  // And it must not read as a subset of a v2 run, which would let a v2 run silently swallow it.
  const v2 = snapshotCellCoords({ upstreams: { openai: { cells: { openai: {}, anthropic: {} } } } });
  assert.ok(!isStrictSubset(coords, v2), "v1 and v2 coords are not alignable cell-for-cell");
  // A v2 matrix that also carries an empty compat row is unaffected - the live shape.
  const both = snapshotCellCoords({ upstreams: { openai: { cells: { openai: {} } } }, cells: {} });
  assert.equal(both.size, 1);
});

/* __layered IS THE SUMMARY A READER LOOKS AT, and it was rebuilt from scratch on every pass - so
   with two scoped re-runs it disclosed only the most recent, and the earlier run's cells read as if
   they came from the base. The per-cell __run stamps survived, which bounded the damage. */
test("layering two scoped runs discloses both, not just the last", () => {
  const cell = (tag) => ({ served: true, stream: { stream_served: true, tag } });
  const mk = (coords) => {
    const m = { upstreams: {}, cells: {} };
    for (const [eg, ing, tag] of coords) (m.upstreams[eg] ??= { cells: {} }).cells[ing] = cell(tag);
    return m;
  };
  const base = mk([["openai", "openai", "old"], ["openai", "anthropic", "old"], ["gemini", "cohere", "old"]]);
  const runA = mk([["openai", "openai", "A"]]);
  const runB = mk([["openai", "anthropic", "B"]]);

  let m = layerScopedMatrix(base, runA, { build: "aaa", measured_at: "2026-08-01T01:00:00Z", __file: "a.json" });
  m = layerScopedMatrix(m, runB, { build: "bbb", measured_at: "2026-08-01T02:00:00Z", __file: "b.json" });

  assert.equal(m.upstreams.openai.cells.openai.stream.tag, "A", "run A's cell must survive run B");
  assert.equal(m.upstreams.openai.cells.anthropic.stream.tag, "B");
  assert.equal(m.upstreams.gemini.cells.cohere.stream.tag, "old", "untouched cells keep the base");

  const runs = m.__layered.runs;
  assert.equal(runs.length, 2, `both scoped runs must be disclosed, got ${JSON.stringify(runs)}`);
  assert.deepEqual(runs.map((r) => r.from.file), ["a.json", "b.json"]);
  assert.deepEqual(runs[0].cells, ["openai>openai"]);
  assert.deepEqual(runs[1].cells, ["anthropic>openai"]);
});
