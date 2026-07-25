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
import { readFileSync, writeFileSync, mkdtempSync, mkdirSync, rmSync, existsSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import assert from "node:assert/strict";
import { checkConsistency, c6Inversions, c7HwmBelowPeak } from "./check-consistency.mjs";
import * as checkMod from "./check-consistency.mjs";
import { sealMetric, ZERO_NO_CEILING, ZERO_MEASURED_FAIL } from "./seal.mjs";
// AUDIT #18: a HAND-BUILT fixture has no results/matrix/<key>.json oracle. The waiver is now an
// EXPLICIT opt-in (the CLI never passes it), so the real bundle can never silently take it.
const SYNTH = { syntheticFixture: true };

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const app = createRequire(import.meta.url)(join(HERE, "app.js"));

let passed = 0;
// The runner CATCHES a failing test, records it, and keeps going, then exits non-zero at the end with
// every failure listed. A throwing test used to abort the file at that line: the two tests that assert on
// the LIVE bundle sit in the middle of it, so while the shipped data carried a known inversion, every one
// of the ~200 tests after them silently never ran. That is the worst possible failure mode for a suite
// whose job is to be the gate: it looked like a single known-red and was hiding real ones (the perf-label
// test below was broken by an earlier relabel and nothing said so). Ordering must not decide coverage.
const failures = [];
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
process.on("exit", () => {
  if (!failures.length) return;
  console.error(`\n${failures.length} FAILING test(s):`);
  for (const f of failures) console.error(`  - ${f.name}`);
  process.exitCode = 1;
});

// ---- sealed-envelope fixture helpers (mirror seal.mjs / gen-data) ------------
// Every metric in the bundle is a SEALED ENVELOPE. These builders take RAW intent (value + mock_bound)
// and produce the exact envelope shape gen-data emits, so fixtures stay readable while exercising the
// real reader (app.metric/mval). See seal.mjs: a GATED metric is certified only when present AND
// (value===0 [always: a measured zero is certified, its NOTE names what the zero means] OR (value>0 AND
// flag===false)); else suppressed. Ungated = certified when present. RPS ceilings note ZERO_NO_CEILING;
// streaming counts note ZERO_MEASURED_FAIL — a measured failure, never folded into "not measured" (#3).
// AUDIT #24: the fixtures seal through the REAL exported sealMetric(). This file used to HAND-COPY
// seal.mjs's logic here, so every honesty test in it asserted against the COPY — seal.mjs could drift
// arbitrarily and nothing would fail. The copy is DELETED; `seal` IS the choke point under test.
const seal = sealMetric;
const SRC = (kind, sweep) => ({ kind, sweep, build: "img:1", measured_at: "2026-07-24T00:00:00Z" });
// bcCell: a sealed best_cell (or same-dialect diagonal) from raw perf intent.
function bcCell(o = {}) {
  const {
    dialect = "openai", ingress = dialect, egress = dialect, kind = "matrix",
    sweep = (kind === "perf-fallback" ? "perf-suite" : ingress === egress ? "6x6-diagonal" : "6x6-translation"),
    added_latency_p50_us = 100, added_latency_p99_us = 110,
    rps_sustained_20ms = 30000, rps_sustained_20ms_mock_bound = false, rps_sustained_20ms_concurrency = null, sweep_sustained_20ms = null,
    rps_max_proxy = 32000, rps_max_proxy_mock_bound = false, rps_max_proxy_concurrency = null, sweep_max_proxy = null,
  } = o;
  const rec = { path: { ingress, egress, ...(ingress === egress ? { dialect } : {}) }, source: SRC(kind, sweep) };
  if (added_latency_p50_us != null) rec.added_latency_p50_us = seal(added_latency_p50_us);
  if (added_latency_p99_us != null) rec.added_latency_p99_us = seal(added_latency_p99_us);
  rec.rps_sustained_20ms = seal(rps_sustained_20ms, { gated: true, flag: rps_sustained_20ms_mock_bound,
    extras: { concurrency: rps_sustained_20ms_concurrency, sweep: sweep_sustained_20ms } });
  rec.rps_max_proxy = seal(rps_max_proxy, { gated: true, flag: rps_max_proxy_mock_bound,
    extras: { concurrency: rps_max_proxy_concurrency, sweep: sweep_max_proxy } });
  return rec;
}
// tCell: a sealed translation_cell.
function tCell(o = {}) {
  const {
    ingress = "openai", egress = "anthropic", kind = "matrix",
    sweep = kind === "xlate-fallback" ? "xlate-suite" : "6x6-translation",
    added_latency_p50_us = null, added_latency_p99_us = 200,
    rps_sustained_20ms = 3000, rps_sustained_20ms_mock_bound = false, rps_sustained_20ms_concurrency = null,
  } = o;
  const rec = { path: { ingress, egress }, source: SRC(kind, sweep) };
  if (added_latency_p50_us != null) rec.added_latency_p50_us = seal(added_latency_p50_us);
  if (added_latency_p99_us != null) rec.added_latency_p99_us = seal(added_latency_p99_us);
  rec.rps_sustained_20ms = seal(rps_sustained_20ms, { gated: true, flag: rps_sustained_20ms_mock_bound,
    extras: { concurrency: rps_sustained_20ms_concurrency } });
  return rec;
}
// streamRec: a sealed streaming record (projected g.streaming, or a per-cell .stream when path omitted).
function streamRec(o = {}) {
  const {
    dialect = "openai", kind = "matrix", sweep = kind === "stream-fallback" ? "stream-suite" : "6x6-stream-diagonal",
    withPathSource = true,
    added_ttft_p50_us = 40, added_ttft_p99_us = 90, added_gap_p50_us = 5, added_gap_p99_us = 12,
    streams_sustained = 1300, streams_sustained_mock_bound = false, streams_sustained_fps = 39000,
    cpu_fps = 48000, cpu_fps_mock_bound = false, cpu_fps_concurrency = null,
  } = o;
  const rec = { stream_served: true };
  if (withPathSource) { rec.path = { dialect }; rec.source = SRC(kind, sweep); }
  const put = (k, v) => { if (v != null) rec[k] = seal(v); };
  put("added_ttft_p50_us", added_ttft_p50_us); put("added_ttft_p99_us", added_ttft_p99_us);
  put("added_gap_p50_us", added_gap_p50_us); put("added_gap_p99_us", added_gap_p99_us);
  rec.streams_sustained_fps = seal(streams_sustained_fps, { gated: true, flag: streams_sustained_mock_bound, zeroNote: ZERO_MEASURED_FAIL });
  rec.streams_sustained = seal(streams_sustained, { gated: true, flag: streams_sustained_mock_bound, zeroNote: ZERO_MEASURED_FAIL });
  rec.cpu_fps = seal(cpu_fps, { gated: true, flag: cpu_fps_mock_bound, zeroNote: ZERO_MEASURED_FAIL, extras: { concurrency: cpu_fps_concurrency } });
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
// A sealed per-cell matrix perf/stream (metrics-only, no path/source — as gen-data seals cells in place).
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

// ---- freshness guard (matrix-sole-source): relaxed rules ----
// Under matrix-sole-source each gateway is ONE atomic matrix run (hours long) published INDEPENDENTLY,
// so the board legitimately carries mixed per-gateway ages. The old RELATIVE guards are gone:
//   - intra-row SPAN hard-fail (mixed suites from different runs): REMOVED — replaced by a generous
//     sanity cap (12h) that only a corrupt/future-dated timestamp can trip;
//   - cross-gateway LAG hard-fail (a row lagging the board-newest): REMOVED — mixed cadences are honest.
// KEPT: the wholesale-stale ABSOLUTE floor (nothing on the board younger than MAX_BOARD_AGE_DAYS).
// NEW: a PER-GATEWAY staleness SIGNAL (g.stale set when a row's own data ages past MAX_GATEWAY_AGE_DAYS)
// — a badge, NOT a build failure. These tests positively exercise all of that against a synthetic repo,
// since the main data load above falls back to the committed bundle when the guard trips.
function buildSyntheticRepo(measuredAtByGw) {
  // measuredAtByGw: { key: { perf: isoString, matrix?: isoString, ... }, ... }
  const root = mkdtempSync(join(tmpdir(), "site-fresh-"));
  for (const [key, suites] of Object.entries(measuredAtByGw)) {
    mkdirSync(join(root, "gateways", key), { recursive: true });
    writeFileSync(join(root, "gateways", key, "gateway.sh"),
      `#!/usr/bin/env bash\nGW_DISPLAY="${key}"\nGW_LANG=Rust\nGW_CLASS="Gateway"\n`);
    for (const [suite, iso] of Object.entries(suites)) {
      mkdirSync(join(root, "results", suite), { recursive: true });
      const doc = { gateway: key, build: "ok", served: true, measured_at: iso,
        added_latency_p50_us: 100, added_latency_p99_us: 200,
        rps_sustained_20ms: 10000, rps_max_proxy: 12000 };
      writeFileSync(join(root, "results", suite, `${key}.json`), JSON.stringify(doc));
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

test("freshness guard HARD-FAILS a wholesale-stale board (absolute age floor kept)", () => {
  // The whole board is older than MAX_BOARD_AGE_DAYS (180d): NOTHING has refreshed at all. This is the
  // one absolute floor kept under matrix-sole-source — publishing generated_at=now over a board where
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
  // failing the build — as long as some OTHER gateway keeps the board under the wholesale floor.
  const { err, data } = genData(buildSyntheticRepo({
    fresh: { matrix: isoAgo(2) },          // keeps the board off the wholesale floor
    old: { matrix: isoDaysAgo(75) },       // > 60d → flagged stale, but NOT a hard fail
  }));
  assert.equal(err, undefined, `a per-gateway-stale board must still build (badge, not failure): ${err}`);
  const byKey = Object.fromEntries(data.gateways.map((g) => [g.key, g]));
  assert.equal(byKey.old.stale, true, "a gateway older than 60d must be flagged stale");
  assert.equal(byKey.fresh.stale, false, "a fresh gateway must not be flagged stale");
});

test("MED-2: the wholesale-stale floor is MATRIX-scoped — a never-displayed legacy re-run cannot mask a stale board", () => {
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
  // numbers — the matrix stamp — even when a newer legacy results/perf/<gw>.json is present. Deriving it
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

test("HIGH-3: the span cap is MATRIX-scoped — a matrix-only re-run past a weeks-old legacy stamp PASSES", () => {
  // The core HIGH-3 fix: legacy suites (perf/stream/streamcpu/memory) are fallback-only and are NEVER
  // refreshed by a matrix-only re-run, so they legitimately carry weeks-old stamps while matrix=today.
  // Folding them into the span made an honest incremental matrix re-run trip the >12h cap and abort the
  // deploy. The span cap now considers ONLY the matrix suite's own timestamps, so this must PASS.
  const msg = genThrows(buildSyntheticRepo({
    incr: { perf: isoDaysAgo(21), matrix: isoAgo(1) }, // matrix today, legacy 3 weeks old — honest re-run
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
  // degradation — the not-yet-wired gateways).
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
    const man = readFileSync(join(ROOT, "gateways", g.key, "gateway.sh"), "utf8");
    const m = /^GW_CLASS=(.*)$/m.exec(man);
    if (!m) continue;                       // a manifest may omit it; gen-data then defaults it
    const declared = m[1].replace(/\s+#.*$/, "").trim().replace(/^["']|["']$/g, "");
    assert.equal(g.cls, declared, `${g.key}: published cls must be the manifest's own GW_CLASS`);
  }
});

test("star snapshot is well formed, carries no stale key, and gen-data attaches it", () => {
  // The committed snapshot (gateways/stars.json, refreshed by `node gateways/fetch-stars.mjs`).
  //
  // THIS USED TO HARD-FAIL on any gateway missing from the snapshot, which quietly made adding a
  // gateway a TWO-step operation: drop in gateways/<name>/gateway.sh and the whole site build went
  // red until someone also ran fetch-stars and committed the result. That breaks the one structural
  // invariant this repo has. A star count is decoration, gen-data already degrades a missing entry to
  // `stars: null`, and the board renders that as no star data - so a gateway added since the last
  // refresh is HONEST, not a build break.
  //
  // What IS asserted, and is stricter than before: every entry present must be well formed, and the
  // snapshot must carry NO key without a gateway directory. A stale key is real drift (a renamed or
  // deleted gateway leaving a phantom count behind); a missing key is just a pending refresh.
  const snap = JSON.parse(readFileSync(join(ROOT, "gateways", "stars.json"), "utf8"));
  const live = new Set(readdirSync(join(ROOT, "gateways")).filter(
    (d) => existsSync(join(ROOT, "gateways", d, "gateway.sh"))));
  for (const [key, s] of Object.entries(snap)) {
    assert.ok(live.has(key), `gateways/stars.json has a stale key '${key}' with no gateways/${key}/gateway.sh`);
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
  assert.equal(app.decodeUrl("/gateways/charts", "").view, "method");
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
  assert.equal(back.sortCol, "rps20");
});

test("a direct URL load defaults each tab to its column's natural direction", () => {
  // Performance headline on Sustained RPS -> descending (higher is better)
  const pass = app.decodeUrl("/gateways/performance", "");
  assert.equal(pass.sortCol, "rps20");
  assert.equal(pass.sortDesc, true);
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

test("the unified tab order: Gateways · Memory · Performance · Streaming · matrix · method", () => {
  assert.deepEqual(app.VIEWS, ["gateways", "memory", "performance", "streaming", "matrix", "method"]);
  assert.equal(app.VIEW_LABELS.gateways, "Gateways");
  assert.equal(app.VIEW_LABELS.memory, "Memory");
  assert.equal(app.VIEW_LABELS.performance, "Performance");
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
  // best_cell (openai diagonal, sealed envelope) -> that number. A certified value shows.
  const green = { best_cell: bcCell({ dialect: "openai", rps_sustained_20ms: 30000 }) };
  assert.equal(app.passCell(green, "rps_sustained_20ms", String).na, false);
  assert.equal(app.passCell(green, "rps_sustained_20ms", String).text, "30000");
  // no best_cell at all (a gateway whose sweep did not land): reads n/a — there is no legacy perf reservoir.
  const unswept = { matrix: mkMatrix({ openai: { openai: { served: true } } }) };
  assert.equal(app.passCell(unswept, "rps_sustained_20ms", String).na, true);
  // openai not served: BEST-OF shows the native diagonal (litellm-rust -> anthropic), NOT n/a and
  // NOT filtered. gen-data picks it; here best_cell carries the anthropic number.
  const native = { best_cell: bcCell({ dialect: "anthropic", rps_sustained_20ms: 32354 }) };
  assert.equal(app.passCell(native, "rps_sustained_20ms", String).na, false);
  assert.equal(app.passCell(native, "rps_sustained_20ms", String).text, "32354");
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
    matrix: mkMatrix({ anthropic: { openai: { served: true, perf: cellPerf({ rps_sustained_20ms: 100, added_latency_p99_us: 200 }) } } }) };
  const g1 = { display: "g1", key: "g1", lang: "Go",
    matrix: mkMatrix({ gemini: { openai: { served: true, perf: cellPerf({ rps_sustained_20ms: 90, added_latency_p99_us: 300 }) } } }) };
  const st = { ...app.newState(), view: "performance", mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  // BOTH gateways appear (no filtering in Custom mode).
  assert.deepEqual(app.applyFilters([g0, g1], st).map((g) => g.key), ["g0", "g1"]);
  // g0 serves the pinned cell -> a number; g1 does not -> n/a.
  assert.equal(app.chooserPerfCell(g0, "rps_sustained_20ms", String, st).text, "100");
  assert.equal(app.chooserPerfCell(g1, "rps_sustained_20ms", String, st).na, true);
  // Repin to openai->gemini: now g1 reads a number and g0 reads n/a, still both present.
  const st2 = { ...st, xlateOut: "gemini" };
  assert.equal(app.chooserPerfCell(g1, "rps_sustained_20ms", String, st2).text, "90");
  assert.equal(app.chooserPerfCell(g0, "rps_sustained_20ms", String, st2).na, true);
  assert.deepEqual(app.applyFilters([g0, g1], st2).map((g) => g.key), ["g0", "g1"]);
});

// ---- consistency guard: one canonical value per (gateway, metric) -----------
test("consistency guard: table == drawer == compare == charts on the real bundle", () => {
  const { errors, warnings } = checkConsistency(data, app);
  for (const w of warnings) console.warn(`  warn - ${w}`); // R7 inversions: visible, never fatal
  assert.deepEqual(errors, [], `numeric divergence across surfaces:\n${errors.join("\n")}`);
});

// A best_cell whose metrics are sealed envelopes: the table reads the value through metric(); a suppressed
// metric is {value:null} in the DATA, so there is no ungated field to leak — the class of bug is gone.
test("sealed envelope: every surface reads best_cell through metric(); a suppressed metric is n/a", () => {
  const g = { key: "seal", display: "Seal", lang: "Rust",
    best_cell: bcCell({ added_latency_p99_us: 111, rps_sustained_20ms: 22222, rps_max_proxy: 33333 }) };
  // table (passCell) reads the envelope value
  assert.equal(app.passCell(g, "added_latency_p99_us", String).v, 111);
  assert.equal(app.passCell(g, "rps_sustained_20ms", String).v, 22222);
  assert.equal(app.passCell(g, "rps_max_proxy", String).v, 33333);
  // drawer/compare read the SAME canonical record (the projected best_cell), metrics as envelopes
  const perfLane = app.LANES.find((l) => l.key === "perf");
  assert.equal(perfLane.get, app.canonicalPerf, "perf lane reads the canonical accessor");
  const rec = perfLane.get(g);
  assert.equal(app.mval(rec.rps_sustained_20ms), 22222);
  assert.equal(app.mval(rec.rps_max_proxy), 33333);
  assert.deepEqual(checkConsistency({ gateways: [g] }, app, SYNTH).errors, [], "a clean sealed bundle is consistent");
  // A SUPPRESSED (mock-bound) sustained: the envelope carries value:null — n/a everywhere, no leak.
  const bound = { key: "sealb", display: "SealB", lang: "Rust",
    best_cell: bcCell({ rps_sustained_20ms: 99999, rps_sustained_20ms_mock_bound: true }) };
  assert.equal(app.passCell(bound, "rps_sustained_20ms", String).na, true, "a suppressed metric reads n/a");
  assert.equal(app.mval(bound.best_cell.rps_sustained_20ms), null, "the raw number is GONE from the envelope");
  assert.deepEqual(checkConsistency({ gateways: [bound] }, app, SYNTH).errors, []);
});

// The HIGH class: a certified fallback value must NOT be suppressed. Run gen-data for real over a
// matrix with no swept diagonal (so the perf/xlate fallbacks fire) + certified legacy suites, then assert
// the SEALED envelope surfaces the certified 17,437 (never dropped by a lost mock_bound flag). The
// 17,437 is a real measured anthropic-in/openai-out throughput from the field; the fixture gateway is
// synthetic because the class is about the SEAL, not about who produced the number.
test("HIGH class: a certified xlate-fallback value survives the seal and reaches the table", () => {
  const root = mkdtempSync(join(tmpdir(), "site-fb-"));
  mkdirSync(join(root, "gateways", "fbgw"), { recursive: true });
  writeFileSync(join(root, "gateways", "fbgw", "gateway.sh"),
    `#!/usr/bin/env bash\nGW_DISPLAY="fbgw"\nGW_LANG=Rust\nGW_CLASS="Gateway"\n`);
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
  // The certified values are sealed as certified envelopes (value present) — never suppressed.
  assert.equal(app.mval(g.best_cell.rps_sustained_20ms), 19286, "certified perf-fallback sustained survives");
  assert.equal(app.mval(g.translation_cell.rps_sustained_20ms), 17437, "certified xlate-fallback RPS survives (the HIGH class)");
  // The table accessor surfaces the real number, not n/a.
  assert.equal(app.xlateCell({ ...g, matrix: undefined } , "rps_sustained_20ms", String).na, true); // no matrix cell to read
  assert.equal(app.mval(app.canonicalXlate(g).rps_sustained_20ms), 17437, "certified xlate RPS reaches the drawer/compare");
  // And C1 holds: no _mock_bound flag survives anywhere in the emitted bundle.
  assert.ok(!JSON.stringify(bundle).includes("_mock_bound"), "no *_mock_bound flag survives the seal");
});

test("a zero RPS cell renders 0 with the no-qualifying-ceiling tooltip", () => {
  const zero = { best_cell: bcCell({ dialect: "openai", rps_sustained_20ms: 18, rps_max_proxy: 0 }) };
  const cols = app.COLUMN_SETS.performance;
  const rpsmax = cols.find((c) => c.id === "rpsmax").get(zero);
  assert.equal(rpsmax.text, "0", "a measured-zero RPS ceiling shows 0 (honest), never suppressed");
  assert.equal(rpsmax.na, false);
  assert.ok(/no tested load held p99 < 1 s/.test(rpsmax.note), "tooltip explains the 0");
  // a non-zero cell carries no note
  const rps20 = cols.find((c) => c.id === "rps20").get(zero);
  assert.equal(rps20.text, "18");
  assert.ok(!rps20.note);
});

// ---- check-consistency: STRUCTURAL INVARIANTS C1–C5 + R1 oracle + R2 coverage ------------------------
// The onthebench 11th-phase test. Each invariant has a RED-before test that reintroduces the dishonesty
// on a clone of the real bundle and asserts the SPECIFIC invariant fails (revert-the-seal → class fails).
const clone = () => structuredClone(data);
const matrixGw = (d) => d.gateways.find((g) => g.best_cell && g.best_cell.source && g.best_cell.source.kind === "matrix");

test("consistency guard: the real bundle satisfies the sealed-envelope invariants C1–C5", () => {
  const { errors, warnings } = checkConsistency(data, app);
  for (const w of warnings) console.warn(`  warn - ${w}`);
  assert.deepEqual(errors, [], `structural-invariant violations:\n${errors.join("\n")}`);
});

test("R2 coverage: every REQUIRED invariant branch is exercised by the real bundle (no inert check)", () => {
  const { cover, REQUIRED, CHECK_BRANCHES } = checkConsistency(data, app);
  assert.ok(Array.isArray(REQUIRED) && REQUIRED.length, "REQUIRED branch set is declared");
  for (const b of REQUIRED) assert.ok(cover.has(b), `required invariant branch not exercised: ${b}`);
  // the declared branch set is a superset of what a healthy bundle exercises
  for (const b of cover) assert.ok(CHECK_BRANCHES.includes(b), `covered branch ${b} not in CHECK_BRANCHES`);
});

test("C1 RED: a BARE metric scalar (raw ungated field) fails C1", () => {
  const d = clone();
  const g = matrixGw(d);
  g.best_cell.rps_max_proxy = 20057;   // revert the seal: a raw ungated number
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("C1:") && x.includes("rps_max_proxy") && x.includes("BARE scalar")),
    `C1 must flag a bare metric scalar; got: ${JSON.stringify(e.filter((x) => x.startsWith("C1")))}`);
});

test("C1 RED: a surviving *_mock_bound flag fails C1", () => {
  const d = clone();
  const g = matrixGw(d);
  g.best_cell.rps_max_proxy_mock_bound = false;   // the flag must have been consumed at seal time
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("C1:") && x.includes("_mock_bound")),
    `C1 must flag a surviving *_mock_bound flag; got: ${JSON.stringify(e.filter((x) => x.startsWith("C1")))}`);
});

test("C2 RED: a suppressed metric that still exposes a value fails C2", () => {
  const d = clone();
  const g = matrixGw(d);
  g.best_cell.rps_sustained_20ms = { value: 19469, certified: false, suppressed: true, reason: "mock_bound" };
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("C2:") && x.includes("rps_sustained_20ms")),
    `C2 must flag a suppressed metric that still carries a value; got: ${JSON.stringify(e.filter((x) => x.startsWith("C2")))}`);
});

test("C3 RED: a caption stamp with no SWEEP_CAPTION renderer fails C3", () => {
  const d = clone();
  const g = matrixGw(d);
  g.best_cell.source.sweep = "6x6-bogus";   // a stamp the caption vocabulary does not know
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("C3:") && x.includes("6x6-bogus")),
    `C3 must flag an unknown source.sweep stamp; got: ${JSON.stringify(e.filter((x) => x.startsWith("C3")))}`);
});

test("C4 RED: a leaked legacy suite object fails C4", () => {
  const d = clone();
  const g = matrixGw(d);
  g.perf = { served: true };   // a raw legacy suite object must never survive in the bundle
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("C4:") && x.includes(".perf") && x.includes("leaked")),
    `C4 must flag a leaked legacy suite object; got: ${JSON.stringify(e.filter((x) => x.startsWith("C4")))}`);
});

test("C4 RED: an unknown source.kind (a re-added silent fallback) fails C4", () => {
  const d = clone();
  const g = matrixGw(d);
  g.best_cell.source.kind = "perf";   // not a known origin (matrix | *-fallback)
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("C4:") && x.includes("source.kind")),
    `C4 must flag an unknown source.kind; got: ${JSON.stringify(e.filter((x) => x.startsWith("C4")))}`);
});

test("R1 RED: a best_cell envelope that disagrees with the RAW matrix cell fails the independent oracle", () => {
  const d = clone();
  const g = matrixGw(d);
  // corrupt the sealed headline so it no longer equals the raw matrix diagonal cell on disk.
  const cur = g.best_cell.rps_sustained_20ms;
  g.best_cell.rps_sustained_20ms = { value: (app.mval(cur) || 0) + 12345, certified: true, suppressed: false };
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("R1:") && x.includes(g.key) && x.includes("rps_sustained_20ms")),
    `R1 must flag a headline that disagrees with the raw matrix cell; got: ${JSON.stringify(e.filter((x) => x.startsWith("R1")))}`);
});

// ---- C6: the physical-plausibility invariant, proven on INJECTED data --------------------------
// This test used to assert on kong's LIVE openai>openai cell (14,351 sustained > 14,325 max_proxy). That
// made a check's only proof "the bug is still in the shipped data" — so when the fresh field run RESOLVED
// kong's inversion, the invariant test failed even though the invariant was working perfectly. C6 is now
// a pure exported function, so the RED case is injected and the check is proven independent of whichever
// gateway happens to be inverted this week.
const c6Matrix = (sus, max, served = true) => ({
  upstreams: { openai: { cells: { openai: { served, perf: { rps_sustained_20ms: sus, rps_max_proxy: max } } } } },
});

test("C6 RED: an INJECTED sustained@20ms > max_proxy cell is a HARD FAILURE", () => {
  const r = c6Inversions("gw", c6Matrix(14351, 14325));
  assert.equal(r.cellsChecked, 1, "the inverted cell must have been checked");
  assert.equal(r.violations.length, 1, `C6 must flag an injected inversion; got: ${JSON.stringify(r.violations)}`);
  assert.ok(r.violations[0].includes("gw.openai->openai") && r.violations[0].includes("sustained@20ms")
    && r.violations[0].includes("max_proxy") && r.violations[0].includes("0.18%"),
    `the C6 violation must name the cell, both ceilings and the magnitude; got: ${r.violations[0]}`);
});

// C6 IS AN ERROR, NOT A WARNING. This is the whole point of the promotion: a warning let a real
// measurement bug (the two sweeps searching different concurrency ranges, so the peak search
// terminated on its own bound) sit in the published data instead of blocking the publish. If this
// assertion ever fails because the inversion moved back into `warnings`, the gate has gone soft again.
test("C6 severity: an inversion must reach checkConsistency's ERRORS, never its warnings", () => {
  const inverted = c6Inversions("gw", c6Matrix(14351, 14325));
  assert.ok(!("warnings" in inverted),
    "c6Inversions must not expose a `warnings` key — the field name is what routed it to the soft channel");
});

test("C6 GREEN: a plausible cell, an unqualified ceiling and an unserved cell are NOT flagged", () => {
  assert.equal(c6Inversions("gw", c6Matrix(14325, 14351)).violations.length, 0, "sustained < max_proxy is plausible");
  assert.equal(c6Inversions("gw", c6Matrix(100, 100)).violations.length, 0, "equality is not an inversion");
  // max_proxy 0 = "did not qualify" (no ceiling to invert), and must not be counted as a checked cell.
  const zero = c6Inversions("gw", c6Matrix(100, 0));
  assert.equal(zero.violations.length, 0, "a 0 max_proxy is 'did not qualify', not an inversion");
  assert.equal(zero.cellsChecked, 0, "a cell with no ceiling is not a checked cell");
  // an UNSERVED cell carries no honest perf to compare.
  assert.equal(c6Inversions("gw", c6Matrix(14351, 14325, false)).cellsChecked, 0, "unserved cells are skipped");
  // null-safe on absent/edge inputs (older snapshots, a not-served matrix).
  for (const bad of [null, undefined, {}, { upstreams: null }, { upstreams: {} }])
    assert.equal(c6Inversions("gw", bad).cellsChecked, 0, `C6 must be null-safe on ${JSON.stringify(bad)}`);
});

// ---- C7: the sampled peak can never exceed the kernel's own high-water mark ----------------------
// Found in this field run's shipped data: one-api 165.1 > 164.7 and agentgateway 45.0 > 44.7. VmHWM is
// maintained by the kernel on every charge, so for a FIXED process tree it cannot sit below any RSS the
// sampler observed. Both readers sum over the tree ENUMERATED AT READ TIME, and the two reads happen at
// different instants — so a worker that exits between the load and the VmHWM read is counted in the
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
  // must never be counted as checked — the honest-null path is correct data, not a violation.
  for (const bad of [null, undefined, {}, { memory: null }, c7Mem(null, null), c7Mem(45, null), c7Mem(null, 44.7)])
    assert.equal(c7HwmBelowPeak("gw", bad).checked, 0, `C7 must be null-safe on ${JSON.stringify(bad)}`);
});

test("C7: the live bundle's hwm-below-peak rows warn but never hard-fail", () => {
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
test("C6: an inversion is a hard failure on the live bundle, never a warning", () => {
  const { errors, warnings } = checkConsistency(data, app);
  assert.ok(!warnings.some((x) => x.includes("sustained@20ms")),
    `a C6 inversion must not be routed to the soft channel; got: ${warnings.filter((x) => x.includes("sustained@20ms"))}`);
  const keys = new Set(data.gateways.map((g) => g.key));
  for (const e of errors.filter((x) => x.includes("sustained@20ms")))
    assert.ok(keys.has(e.split(".")[0]), `a C6 error must name a gateway in the bundle; got: ${e}`);
});

// ---- matrix is the single source: streaming + memory projection + download ----------------------
// A representative diagonal cell's streaming record + a top-level matrix memory read, as matrix/run.sh
// now emits them. gen-data must project g.streaming (best diagonal cell's stream) + g.memory_read.
const STREAM_CELL = {
  stream_served: true, added_ttft_p50_us: 40, added_ttft_p99_us: 90,
  added_gap_p50_us: 5, added_gap_p99_us: 12, streams_sustained: 1300, streams_sustained_fps: 39000,
  streams_sustained_mock_bound: false, cpu_fps: 48000, cpu_fps_concurrency: 768, cpu_fps_mock_bound: false,
};
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
  writeFileSync(join(root, "gateways", "sgw", "gateway.sh"),
    `#!/usr/bin/env bash\nGW_DISPLAY="sgw"\nGW_LANG=Rust\nGW_CLASS="Gateway"\n`);
  mkdirSync(join(root, "results", "matrix"), { recursive: true });
  const iso = new Date(Date.now() - 3600000).toISOString();
  const perf = { added_latency_p50_us: 10, added_latency_p99_us: 20, rps_sustained_20ms: 45000,
    rps_sustained_20ms_concurrency: 512, rps_max_proxy: 50000, rps_max_proxy_concurrency: 256,
    sweep_max_proxy: [{ conc: 256, rps: 50000, p99_us: 100, fail: 0 }],
    sweep_sustained_20ms: [{ conc: 512, rps: 45000, p99_us: 200, fail: 0 }] };
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

test("gen-data projects streaming from the best diagonal matrix cell", () => {
  const bundle = genInto(buildStreamMemRepo());
  const g = bundle.gateways.find((x) => x.key === "sgw");
  assert.ok(g.streaming, "expected a projected g.streaming");
  assert.equal(g.streaming.source.kind, "matrix");
  assert.equal(g.streaming.path.dialect, "openai");
  // metrics are sealed envelopes
  assert.equal(app.mval(g.streaming.added_ttft_p99_us), 90);
  assert.equal(app.mval(g.streaming.streams_sustained), 1300);
  assert.equal(app.mval(g.streaming.cpu_fps), 48000);
  // the table accessor reads the same projected value
  assert.equal(app.streamCell(g, "streams_sustained", String).text, "1300");
  assert.equal(app.streamCell(g, "cpu_fps", String).text, "48000");
});

test("MEDIUM-1: a NON-streaming diagonal cell does NOT project g.streaming (stream_served gate)", () => {
  // A cell that did not stream still carries a stream record ({stream_served:false, …}). The old
  // truthiness projection surfaced it as a served streamer; gen-data must now project ONLY when the
  // diagonal cell's stream_served === true, so g.streaming is ABSENT for a non-streaming cell.
  const root = buildStreamMemRepo();  // sgw streams (STREAM_CELL.stream_served === true)
  // Add a second gateway whose diagonal cell served perf but DID NOT stream.
  mkdirSync(join(root, "gateways", "nostream"), { recursive: true });
  writeFileSync(join(root, "gateways", "nostream", "gateway.sh"),
    `#!/usr/bin/env bash\nGW_DISPLAY="nostream"\nGW_LANG=Go\nGW_CLASS="Gateway"\n`);
  const iso = new Date(Date.now() - 3600000).toISOString();
  const perf = { added_latency_p50_us: 10, added_latency_p99_us: 20, rps_sustained_20ms: 40000,
    rps_sustained_20ms_concurrency: 512, rps_max_proxy: 44000, rps_max_proxy_concurrency: 256,
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

test("streaming honesty: cpu_fps mock-bound/unverifiable is n/a; certified shows (via the sealed envelope)", () => {
  // The gate is UPSTREAM (seal time): a rig-limited/unverifiable cpu_fps is {value:null} in the streaming
  // record, so streamCell reads n/a. The other streaming metrics stay visible regardless.
  const certified = { key: "cert", display: "Cert", lang: "Rust", streaming: streamRec({ cpu_fps: 48000, cpu_fps_mock_bound: false }) };
  assert.equal(app.streamCell(certified, "cpu_fps", String).text, "48000");
  assert.equal(app.mval(app.canonicalStreaming(certified).cpu_fps), 48000);
  // mock-bound (true) → suppressed → n/a
  const bound = { key: "bound", display: "B", lang: "Rust", streaming: streamRec({ cpu_fps: 99999, cpu_fps_mock_bound: true }) };
  assert.equal(app.streamCell(bound, "cpu_fps", String).na, true, "mock-bound cpu_fps reads n/a");
  assert.equal(app.mval(app.canonicalStreaming(bound).cpu_fps), null, "the raw number is gone from the envelope");
  // null flag (unverifiable) → suppressed → n/a (the MEDIUM-5 leak class, now structural)
  const nullFlag = { key: "nf", display: "NF", lang: "Rust", streaming: streamRec({ cpu_fps: 88888, cpu_fps_mock_bound: null }) };
  assert.equal(app.streamCell(nullFlag, "cpu_fps", String).na, true, "unverifiable cpu_fps reads n/a");
  // the other streaming metrics stay visible regardless of cpu_fps gating
  assert.equal(app.streamCell(bound, "streams_sustained", String).text, "1300");
});

test("streaming honesty: streams_sustained mock-bound/unverifiable is n/a; certified shows", () => {
  const certified = { key: "sc", display: "SC", lang: "Rust", streaming: streamRec({ streams_sustained: 1300, streams_sustained_mock_bound: false }) };
  assert.equal(app.streamCell(certified, "streams_sustained", String).text, "1300");
  const bound = { key: "sb", display: "SB", lang: "Rust", streaming: streamRec({ streams_sustained: 9999, streams_sustained_mock_bound: true }) };
  assert.equal(app.streamCell(bound, "streams_sustained", String).na, true, "mock-bound streams_sustained reads n/a");
  const nullFlag = { key: "sn", display: "SN", lang: "Rust", streaming: streamRec({ streams_sustained: 8888, streams_sustained_mock_bound: null }) };
  assert.equal(app.streamCell(nullFlag, "streams_sustained", String).na, true, "unverifiable streams_sustained reads n/a");
  // cpu_fps stays visible independent of the sustained gate (the two lanes gate independently)
  assert.equal(app.streamCell(bound, "cpu_fps", String).text, "48000");
});

test("translation honesty: mock-bound/unverifiable translation RPS is n/a; certified + measured-0 show", () => {
  // The Translation tab (xlateCell reads the pinned matrix cell) + the drawer (canonicalXlate) both read
  // the sealed envelope: a rig-limited translation RPS is {value:null} → n/a; a certified value shows; a
  // measured 0 stays 0 (distinct from a rig ceiling). Default state pins openai→anthropic.
  const mkG = (bound) => ({ key: "xg", display: "XG", lang: "Rust",
    translation_cell: tCell({ ingress: "openai", egress: "anthropic", rps_sustained_20ms: 5000, rps_sustained_20ms_mock_bound: bound }),
    matrix: mkMatrix({ anthropic: { openai: { served: true, perf: cellPerf({ rps_sustained_20ms: 5000, rps_sustained_20ms_mock_bound: bound, added_latency_p99_us: 200 }) } } }) });
  // certified: both surfaces show the number
  const cert = mkG(false);
  assert.equal(app.xlateCell(cert, "rps_sustained_20ms", String).text, "5000");
  assert.equal(app.mval(app.canonicalXlate(cert).rps_sustained_20ms), 5000);
  // mock-bound: n/a on both surfaces
  const bound = mkG(true);
  assert.equal(app.xlateCell(bound, "rps_sustained_20ms", String).na, true, "mock-bound translation RPS reads n/a");
  assert.equal(app.mval(app.canonicalXlate(bound).rps_sustained_20ms), null, "drawer/compare suppresses a mock-bound value");
  // unverifiable (null flag): also n/a
  const nullFlag = mkG(null);
  assert.equal(app.xlateCell(nullFlag, "rps_sustained_20ms", String).na, true, "unverifiable translation RPS reads n/a");
  // a LEGITIMATE measured 0 is NOT suppressed — it stays 0 (an RPS ceiling zero is honest)
  const zero = { key: "xz", display: "XZ", lang: "Rust",
    translation_cell: tCell({ ingress: "openai", egress: "anthropic", rps_sustained_20ms: 0, rps_sustained_20ms_mock_bound: null }) };
  assert.equal(app.mval(app.canonicalXlate(zero).rps_sustained_20ms), 0, "a measured 0 stays 0, not n/a");
});

test("streaming: a null added-TTFT/gap reads n/a on the table (the envelope carries the absence)", () => {
  // An unreliable streaming c1 window sets added_ttft/gap to null while stream_served stays true. Under the
  // sealed envelope that null is a {value:null, reason:"not_measured"} envelope; streamCell reads n/a. A
  // measured value reads the number. There is no "site-visible vs chart draws-bar" gate to tie any more —
  // the envelope IS the single decision (the retired drift check cannot arise: one datum, one value).
  const okStream = streamRec({ added_ttft_p99_us: 90, added_gap_p99_us: 12,
    streams_sustained: 1300, streams_sustained_mock_bound: false, cpu_fps: 48000, cpu_fps_mock_bound: false });
  const okGw = { key: "tok", display: "Tok", lang: "Rust", streaming: okStream };
  assert.equal(app.streamCell(okGw, "added_ttft_p99_us", String).text, "90", "measured added-TTFT shows the number");
  assert.deepEqual(checkConsistency({ gateways: [okGw] }, app, SYNTH).errors, [], "a sealed streaming record is consistent");
  const nullStream = streamRec({ added_ttft_p99_us: null, added_gap_p99_us: null,
    streams_sustained: 1300, streams_sustained_mock_bound: false, cpu_fps: 48000, cpu_fps_mock_bound: false });
  const nullGw = { key: "tnull", display: "Tnull", lang: "Rust", streaming: nullStream };
  assert.equal(app.streamCell(nullGw, "added_ttft_p99_us", String).na, true, "null added-TTFT reads n/a on the table");
  assert.equal(app.streamCell(nullGw, "added_gap_p99_us", String).na, true, "null added-gap reads n/a on the table");
  assert.deepEqual(checkConsistency({ gateways: [nullGw] }, app, SYNTH).errors, [], "a null-TTFT sealed streaming record is consistent");
});

test("gen-data emits memory PER CELL and projects no per-gateway memory scalar", () => {
  // The defect this asserts against: one memory number per gateway forced the harness to SELECT a cell to
  // produce it, and it selected on throughput while reporting memory. There is now nothing to select - the
  // window lives on the cell, and the reader chooses the cell (Min|Max|Same|Custom) and can see which.
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
  // A gateway WITHOUT the field (pre-recovery bundle) reads n/a — never 0, never fabricated.
  const noRec = { key: "nr", display: "NR", lang: "Rust",
    memory_read: memRec({ idle_rss_mib: 40, peak_rss_mib: 1000, recovered_rss_mib: null }) };
  const naCell = app.memCell(noRec, "recovered_rss_mib", String);
  assert.equal(naCell.na, true, "an absent recovered_rss_mib must render n/a");
  assert.equal(naCell.text, "n/a");
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
  assert.ok(/recovered 45/.test(svg), "the sparkline caption reports the recovered figure");
  // No series, one point, or a non-array → nothing drawn (never a fabricated flat line).
  assert.equal(app.rssSparkline(undefined), "", "no series → no sparkline");
  assert.equal(app.rssSparkline([]), "", "empty series → no sparkline");
  assert.equal(app.rssSparkline([ { t_s: 0, rss_mib: 40 } ]), "", "a single point → no sparkline");
});

test("streaming: a sealed streaming record reads its gated metrics through the envelope", () => {
  // Under the sealed envelope, streaming is ONE projected record whose gated metrics (streams_sustained,
  // cpu_fps) are sealed at projection time. A certified value shows; a suppressed one reads n/a. There is
  // no headline-vs-cell "projection drift" to guard any more — there is exactly one record, one value.
  const certified = { key: "sg", display: "SG", lang: "Rust",
    streaming: streamRec({ streams_sustained: 1300, streams_sustained_mock_bound: false, cpu_fps: 48000, cpu_fps_mock_bound: false }) };
  assert.equal(app.streamCell(certified, "streams_sustained", app.fmtInt).text, "1,300");
  assert.equal(app.streamCell(certified, "cpu_fps", app.fmtInt).text, "48,000");
  assert.deepEqual(checkConsistency({ gateways: [certified] }, app, SYNTH).errors, [], "a certified sealed streaming record is consistent");
  // A rig-limited (mock-bound) cpu_fps is {value:null} in the data — it reads n/a and cannot leak.
  const bound = { key: "bs", display: "BS", lang: "Rust",
    streaming: streamRec({ streams_sustained: 1300, streams_sustained_mock_bound: false, cpu_fps: 99999, cpu_fps_mock_bound: true }) };
  assert.equal(app.streamCell(bound, "cpu_fps", app.fmtInt).na, true, "a mock-bound cpu_fps reads n/a (the number is gone)");
  assert.equal(app.streamCell(bound, "streams_sustained", app.fmtInt).text, "1,300", "the certified sibling still shows");
  assert.deepEqual(checkConsistency({ gateways: [bound] }, app, SYNTH).errors, [], "a suppressed cpu_fps sealed record is consistent (C2 holds)");
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
  // known-pending and covered by the guard's no-per-cell-perf WARNING; anything else all-n/a is a bug.
  const st = { ...app.newState(), view: "performance", mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  const KEYS = ["added_latency_p50_us", "added_latency_p99_us", "rps_sustained_20ms", "rps_max_proxy"];
  for (const g of data.gateways) {
    if (!(g.matrix && app.chooserHasCell(g, st))) continue;   // only rows that serve the pinned cell
    const sweptAny = Object.values(g.matrix.upstreams || {}).some((u) =>
      Object.values(u.cells || {}).some((c) => c && c.served === true && c.perf));
    if (!sweptAny) {
      const { warnings } = checkConsistency({ gateways: [g] }, app, SYNTH);
      assert.ok(warnings.some((w) => w.includes("no per-cell perf")), `${g.key}: unswept but unflagged`);
      continue;
    }
    const anyVal = KEYS.some((k) => app.chooserPerfCell(g, k, String, st).v != null);
    assert.ok(anyVal, `${g.key} serves the pinned cell but every metric is n/a`);
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
  // verdict. (Governance is retired from the board, so this is asserted on a synthetic record — the
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

// ---- per-cell perf: best-path deviation on the matrix hover -----------------
test("cellPerfTip shows a green cell's perf and its deviation from the gateway's best cell", () => {
  // cellPerfTip reads the sealed envelopes via mval(): a certified cell + reference show the number + delta;
  // a suppressed value is {value:null} and cannot leak (asserted in the next test).
  const best = bcCell({ ingress: "openai", egress: "openai", rps_sustained_20ms: 30000, rps_sustained_20ms_mock_bound: false });
  const green = { served: true, perf: cellPerf({ rps_sustained_20ms: 25500, rps_sustained_20ms_mock_bound: false, added_latency_p99_us: 900 }) };
  const tip = app.cellPerfTip(green, "anthropic", "openai", best);
  assert.ok(tip.includes("25,500 req/s (20 ms upstream)"), tip);
  assert.ok(tip.includes("+900 µs p99 added"), tip);
  assert.ok(tip.includes("-15.0% req/s vs the OpenAI→OpenAI cell"), tip); // human labels, not raw dialect keys
  const bestTip = app.cellPerfTip({ served: true, perf: cellPerf({ rps_sustained_20ms: 30000, rps_sustained_20ms_mock_bound: false }) }, "openai", "openai", best);
  assert.ok(bestTip.includes("reference cell"), bestTip);
  // red/grey/unprobed cells and perf-less greens carry NO perf line
  assert.equal(app.cellPerfTip({ served: false, perf: cellPerf({ rps_sustained_20ms: 1 }) }, "a", "b", best), "");
  assert.equal(app.cellPerfTip({ served: "not_configurable" }, "a", "b", best), "");
  assert.equal(app.cellPerfTip({ served: true }, "a", "b", best), "");
});

test("FINDING 33: cellPerfTip cannot leak a suppressed sustained RPS (the number is gone from the data)", () => {
  const best = bcCell({ ingress: "openai", egress: "openai", rps_sustained_20ms: 30000, rps_sustained_20ms_mock_bound: false });
  // A rig-bound (mock_bound:true) cell is {value:null} — its raw RPS does not exist; the added-latency
  // survives, labelled n/a. There is no ungated field to leak.
  const bound = { served: true, perf: cellPerf({ rps_sustained_20ms: 99999, rps_sustained_20ms_mock_bound: true, added_latency_p99_us: 900 }) };
  const boundTip = app.cellPerfTip(bound, "anthropic", "openai", best);
  assert.ok(!boundTip.includes("99,999"), `a suppressed RPS cannot leak into the tip; got: ${boundTip}`);
  assert.ok(boundTip.includes("sustained RPS n/a: rig-limited"), boundTip);
  assert.ok(boundTip.includes("+900 µs p99 added"), boundTip);
  // An UNSTAMPED value (no mock_bound flag) seals to unverifiable → suppressed → {value:null}.
  const unstamped = { served: true, perf: cellPerf({ rps_sustained_20ms: 25500, rps_sustained_20ms_mock_bound: null, added_latency_p99_us: 900 }) };
  assert.ok(!app.cellPerfTip(unstamped, "anthropic", "openai", best).includes("25,500"), "unstamped RPS is suppressed");
  // A certified cell vs a SUPPRESSED reference: the number shows but no delta (the divisor is null).
  const uncertRef = bcCell({ ingress: "openai", egress: "openai", rps_sustained_20ms: 30000, rps_sustained_20ms_mock_bound: true });  // suppressed ref
  const t = app.cellPerfTip({ served: true, perf: cellPerf({ rps_sustained_20ms: 25500, rps_sustained_20ms_mock_bound: false, added_latency_p99_us: 900 }) }, "anthropic", "openai", uncertRef);
  assert.ok(t.includes("25,500 req/s (20 ms upstream)"), t);
  assert.ok(!t.includes("vs the"), `no delta against a suppressed reference; got: ${t}`);
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
// bundle predates the array-emitting matrix/run.sh — MED-5 coverage gap). existsSync-guarded so a
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

test("sweep chart draws real committed sweep data", () => {
  const perf = committedSweep();
  if (!perf) { console.log("  (skip: no committed sweep arrays in results/matrix or results/perf yet)"); return; }
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
  best_cell: bcCell({ dialect: "openai", added_latency_p50_us: 100, added_latency_p99_us: 110,
    rps_sustained_20ms: 30000, rps_max_proxy: 32000 }),
  matrix: { upstreams: {
    openai: { cells: { openai: { served: true, perf: cellPerf({
      added_latency_p50_us: 100, added_latency_p99_us: 110, rps_sustained_20ms: 30000, rps_max_proxy: 32000 }) } } },
    anthropic: { cells: {
      anthropic: { served: true, perf: cellPerf({
        added_latency_p50_us: 200, added_latency_p99_us: 220, rps_sustained_20ms: 25000, rps_max_proxy: 27000 }) },
      openai: { served: true, perf: cellPerf({
        added_latency_p50_us: 130, added_latency_p99_us: 145, rps_sustained_20ms: 26000, rps_max_proxy: 28000 }) } } },
  } },
};

test("cell chooser: Peak reads the best diagonal, Same reads a chosen diagonal, Custom any cell", () => {
  const g = CHOOSER_GW;
  // Peak → the openai best diagonal (110 p99, 30000 sustained), with the Tested-on dialect openai.
  const peak = { mode: "peak" };
  assert.equal(app.chooserPerfCell(g, "added_latency_p99_us", String, peak).text, "110");
  assert.equal(app.chooserPerfCell(g, "rps_sustained_20ms", String, peak).text, "30000");
  assert.deepEqual(app.chooserDialects(g, peak), ["openai", "openai"]);
  // Same anthropic → the anthropic→anthropic diagonal (220 p99, 25000).
  const same = { mode: "same", sameDialect: "anthropic" };
  assert.equal(app.chooserPerfCell(g, "added_latency_p99_us", String, same).text, "220");
  assert.equal(app.chooserPerfCell(g, "rps_sustained_20ms", String, same).text, "25000");
  assert.deepEqual(app.chooserDialects(g, same), ["anthropic", "anthropic"]);
  // Custom openai→anthropic → the translation cell (145 p99, 26000).
  const cust = { mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  assert.equal(app.chooserPerfCell(g, "added_latency_p99_us", String, cust).text, "145");
  assert.equal(app.chooserPerfCell(g, "rps_sustained_20ms", String, cust).text, "26000");
  // A cell the gateway does NOT serve reads n/a (never fabricated), and the row is not dropped.
  const missing = { mode: "custom", xlateIn: "gemini", xlateOut: "cohere" };
  assert.equal(app.chooserPerfCell(g, "rps_sustained_20ms", String, missing).na, true);
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
  // The Peak record still equals the canonical (Peak) accessor — no regression on the default mode.
  assert.equal(app.mval(app.laneRecord(perfLane, g, { mode: "peak" }).rps_sustained_20ms),
    app.mval(app.canonicalPerf(g).rps_sustained_20ms), "Peak lane record == canonicalPerf");
});

test("Cluster-B/22: perfSweepSeries is chooser-aware and drops a mock-bound-suppressed metric's curve", () => {
  const colors = { sustained: "#4cc38a", max: "#6cb6ff" };
  // Peak: the openai diagonal's sweeps (both metrics certified) are plotted, marked at the published peak.
  // The sweep array travels INSIDE the sealed envelope (env.sweep) — a suppressed metric carries none.
  const withSweep = { ...CHOOSER_GW, best_cell: bcCell({ dialect: "openai",
    rps_sustained_20ms: 30000, sweep_sustained_20ms: [{ conc: 512, rps: 30000, p99_us: 200, fail: 0 }],
    rps_max_proxy: 32000, sweep_max_proxy: [{ conc: 256, rps: 32000, p99_us: 100, fail: 0 }] }) };
  const peak = app.perfSweepSeries(withSweep, colors, { mode: "peak" });
  assert.equal(peak.length, 2, "both certified metrics plotted");
  assert.equal(peak[0].peak.rps, 30000, "sustained curve marks the published peak");
  // A mock-bound metric is {value:null} — its sweep array is gone with it, so its curve is DROPPED
  // (finding 22, now structural: a suppressed envelope carries neither value nor sweep).
  const bound = { ...CHOOSER_GW, best_cell: bcCell({ dialect: "openai",
    rps_sustained_20ms: 99999, rps_sustained_20ms_mock_bound: true, sweep_sustained_20ms: [{ conc: 512, rps: 99999, p99_us: 200, fail: 0 }],
    rps_max_proxy: 32000, rps_max_proxy_mock_bound: false, sweep_max_proxy: [{ conc: 256, rps: 32000, p99_us: 100, fail: 0 }] }) };
  const gated = app.perfSweepSeries(bound, colors, { mode: "peak" });
  assert.equal(gated.length, 1, "the suppressed sustained curve is dropped; only certified max remains");
  assert.equal(gated[0].peak.rps, 32000, "the surviving curve is the certified max-proxy");
});

test("Cluster-C/20: chooserStreamCell reads the right streaming cell across Peak/Same/Custom", () => {
  // A gateway whose streaming was projected from the openai diagonal (matrix per-cell stream), plus an
  // openai->anthropic cell that carries its own per-cell stream record.
  const g = {
    key: "sc", display: "SC", lang: "Rust",
    streaming: streamRec({ dialect: "openai", added_ttft_p99_us: 90, streams_sustained: 1300, streams_sustained_mock_bound: false,
      cpu_fps: 48000, cpu_fps_mock_bound: false }),
    matrix: { upstreams: {
      openai: { cells: { openai: { served: true, perf: cellPerf({ added_latency_p99_us: 10 }),
        stream: cellStream({ added_ttft_p99_us: 90, streams_sustained: 1300, streams_sustained_mock_bound: false }) } } },
      anthropic: { cells: { openai: { served: true, perf: cellPerf({ added_latency_p99_us: 20 }),
        stream: cellStream({ added_ttft_p99_us: 140, streams_sustained: 900, streams_sustained_mock_bound: false }) } } },
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
  const fbCap = app.chooserCaption("streaming", st, fbData).join(" ");
  assert.ok(!/from the one 6x6 run/.test(fbCap), `fallback streaming caption must not positively claim the 6x6 run; got: ${fbCap}`);
  assert.ok(/stream suite/.test(fbCap), "fallback caption names the standalone stream suite");
  // Matrix-sourced streaming: the 6x6 claim IS honest.
  const mxData = { gateways: [{ key: "a", streaming: { source: { kind: "matrix" } } }] };
  assert.equal(app.streamingProvenance(mxData).all, "matrix");
  assert.ok(/from the one 6x6 run/.test(app.chooserCaption("streaming", st, mxData).join(" ")), "matrix streaming may claim the 6x6 run");
  // The Performance (perf) tab is always the 6x6 matrix — its caption is unaffected by streaming provenance.
  assert.ok(/from the one 6x6 run/.test(app.chooserCaption("performance", st, fbData).join(" ")), "perf caption always names the 6x6 run");
});

test("Δ-to-Peak: a non-peak cell reports its deviation vs the gateway's own best diagonal", () => {
  const g = CHOOSER_GW;
  const cust = { mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  const cp = { ingress: "openai", egress: "anthropic", ...app.chooserCellPerf(g, cust) };
  const d = app.deltaToPeak(cp, g.best_cell);
  // p99 145 vs 110 = +31.8% latency; sustained 26000 vs 30000 = -13.3% RPS.
  assert.ok(/\+31\.8% latency/.test(d), d);
  assert.ok(/-13\.3% RPS/.test(d), d);
  // The peak cell itself has no delta.
  assert.equal(app.deltaToPeak({ ingress: "openai", egress: "openai", ...g.best_cell }, g.best_cell), "");
});

test("matrix popup shows the SAME gated value the Performance/Custom table shows, plus Δ-to-peak", () => {
  const g = CHOOSER_GW;
  const html = app.cellPopFull(g, "openai", "anthropic");
  // The popup carries the cell's own gated numbers (formatted en-US) …
  assert.ok(html.includes("<b>145</b>"), "popup shows the cell's added latency p99");
  assert.ok(html.includes("<b>26,000</b>"), "popup shows the cell's sustained RPS");
  // … the SAME numbers the Custom table reads through chooserPerfCell …
  const cust = { mode: "custom", xlateIn: "openai", xlateOut: "anthropic" };
  const enUS = (v) => Number(v).toLocaleString("en-US");
  assert.equal(app.chooserPerfCell(g, "rps_sustained_20ms", enUS, cust).text, "26,000");
  // … and the Δ-to-Peak vs the gateway's own best diagonal.
  assert.ok(/vs peak \(OpenAI→OpenAI\)/.test(html), "popup names the peak reference cell");
  assert.ok(/\+31\.8% latency/.test(html), "popup shows Δ latency");
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
  assert.equal(cols.find((c) => c.id === "memrecov").get(bare).text, "n/a");
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
  const cap = app.memoryCaption({ gateways: [] }).join(" ");
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
   AUDIT GROUP C — the 11th-phase MISSING TESTS. Each block is a CLASS test (one test covering all the
   siblings of a finding), and each carries its RED-before proof: the assertion is written so that
   reverting the fix makes it fail, and where the subject is a LINT the lint is driven against synthetic
   source that CONTAINS the violation, so "the check works" is demonstrated rather than assumed.
   ================================================================================================ */

// ---- #24: sealMetric() — the single honesty choke point — is tested DIRECTLY -----------------------
// test.mjs used to HAND-COPY sealMetric's logic into a local `seal()` helper, so every "honesty" test in
// this file was asserting against the COPY: seal.mjs could drift arbitrarily and nothing here would fail.
// The fixture helpers now call the REAL exported function (the copy is deleted), and these cases pin the
// contract of the choke point itself.
test("#24 CLASS: the REAL sealMetric() is the honesty choke point (no hand-copied logic in the tests)", () => {
  // The fixture builders must be wired to the REAL seal — this is what makes every other test in this
  // file a test OF seal.mjs rather than of a copy that can silently diverge.
  assert.equal(seal, sealMetric, "test fixtures must seal through the REAL seal.mjs export");
  // (a) absent -> NOT MEASURED (never suppressed: nothing was hidden, nothing was measured).
  assert.deepEqual(sealMetric(null), { value: null, certified: false, suppressed: false, reason: "not_measured" });
  assert.deepEqual(sealMetric(undefined), { value: null, certified: false, suppressed: false, reason: "not_measured" });
  // (b) UNGATED -> always certified when present (latency/RSS have no mock-bound flag).
  assert.deepEqual(sealMetric(12.5), { value: 12.5, certified: true, suppressed: false });
  assert.deepEqual(sealMetric(0), { value: 0, certified: true, suppressed: false });
  // (c) GATED positive: certified ONLY when the harness certified it (flag === false).
  assert.equal(sealMetric(100, { gated: true, flag: false }).value, 100);
  assert.deepEqual(sealMetric(100, { gated: true, flag: true }),
    { value: null, certified: false, suppressed: true, reason: "mock_bound" });
  assert.deepEqual(sealMetric(100, { gated: true, flag: null }),
    { value: null, certified: false, suppressed: true, reason: "unverifiable" });
  assert.deepEqual(sealMetric(100, { gated: true }),
    { value: null, certified: false, suppressed: true, reason: "unverifiable" });
  // (d) #3 RED-before: a GATED measured 0 is CERTIFIED and carries a note naming what the zero MEANS.
  //     Before the fix, a streaming 0 sealed to {value:null, reason:"not_measured"} — a measured FAILURE
  //     published as an unmeasured cell. This assertion fails on that old behaviour.
  assert.deepEqual(sealMetric(0, { gated: true }),
    { value: 0, certified: true, suppressed: false, note: ZERO_NO_CEILING });
  assert.deepEqual(sealMetric(0, { gated: true, zeroNote: ZERO_MEASURED_FAIL }),
    { value: 0, certified: true, suppressed: false, note: ZERO_MEASURED_FAIL },
    "a measured stream-sustain FAILURE must be a certified 0, DISTINGUISHABLE from not-measured");
  // and the two states are not the same object shape — the whole point of the fix.
  assert.notDeepEqual(sealMetric(0, { gated: true, zeroNote: ZERO_MEASURED_FAIL }), sealMetric(null, { gated: true }));
  // (e) extras ride ONLY on a certified envelope; a suppressed one leaks nothing recoverable (C2).
  const withExtras = sealMetric(50, { gated: true, flag: false, extras: { concurrency: 8, conc_at: 16, sweep: null } });
  assert.equal(withExtras.concurrency, 8);
  assert.equal(withExtras.conc_at, 16);
  assert.ok(!("sweep" in withExtras), "a null extra must not be emitted");
  const suppressed = sealMetric(50, { gated: true, flag: true, extras: { concurrency: 8, conc_at: 16 } });
  assert.deepEqual(Object.keys(suppressed).filter((k) => typeof suppressed[k] === "number"), [],
    "a suppressed envelope must carry NO recoverable numeric field");
  // (f) the raw scalar and its flag are CONSUMED — they never survive onto the envelope (invariant P1).
  for (const env of [withExtras, suppressed, sealMetric(0, { gated: true })])
    assert.ok(!Object.keys(env).some((k) => k.endsWith("_mock_bound")), "no flag may survive the seal");
});

test("#3 CLASS: a MEASURED stream-sustain failure renders differently from an unmeasured one", () => {
  // The site: a measured 0 shows the number 0 with a MEASURED-FAILURE note; an unmeasured one reads n/a.
  const failed = app.metric(sealMetric(0, { gated: true, zeroNote: ZERO_MEASURED_FAIL }), String);
  const unmeasured = app.metric(sealMetric(null, { gated: true }), String);
  assert.equal(failed.text, "0");
  assert.equal(failed.na, false);
  assert.match(failed.note, /MEASURED FAILURE/);
  assert.equal(unmeasured.text, "n/a");
  assert.equal(unmeasured.na, true);
  assert.match(unmeasured.note, /not measured/);
  // and a rig-limited one is a THIRD state (suppressed), never conflated with either.
  const bound = app.metric(sealMetric(1300, { gated: true, flag: true }), String);
  assert.equal(bound.na, true);
  assert.match(bound.note, /rig-limited/);
});

// ---- #25: the snapshot ingest path (task #65) had NO test at all ----------------------------------
// certifyRepo(root, mutate?): buildStreamMemRepo's matrix has no *_mock_bound flags, so its gated RPS
// seals to "unverifiable". Stamp flag=false (the harness certified it) so these tests assert on the
// SELECTION being tested, not on the honesty gate (which its own tests cover).
function certifyRepo(root, mutate) {
  const mpath = join(root, "results", "matrix", "sgw.json");
  const m = JSON.parse(readFileSync(mpath, "utf8"));
  for (const cells of [m.cells, m.upstreams.openai.cells]) {
    cells.openai.perf.rps_sustained_20ms_mock_bound = false;
    cells.openai.perf.rps_max_proxy_mock_bound = false;
  }
  if (mutate) mutate(m);
  writeFileSync(mpath, JSON.stringify(m));
  return root;
}
function writeSnapshot(root, key, { measuredAt, matrix, files }) {
  mkdirSync(join(root, "results", "snapshots"), { recursive: true });
  writeFileSync(join(root, "results", "snapshots", `result_${key}_${measuredAt.replace(/[:.]/g, "-")}.json`),
    JSON.stringify({ gateway: key, measured_at: measuredAt, matrix, config: files ? { files } : undefined }));
}
test("a DEGRADED-MODE snapshot must never become the board's source just by being newer", () => {
  // THE REAL INCIDENT: helicone's board row became a probe-only laptop run (1 cell, no perf, no
  // streaming, no memory, no best_cell) that shadowed a complete field run from the same morning,
  // because a local verify-local run with KEEP_ARTIFACTS=1 left its snapshot in results/snapshots/ and
  // recency handed it the whole row. Nothing anywhere said so.
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
  // (c) GREEN: a degraded snapshot is fine when there is nothing fuller to shadow (a gateway whose only
  //     data is that run). Absence of a better run is not a reason to publish nothing.
  {
    const root = buildStreamMemRepo();
    rmSync(join(root, "results", "matrix", "sgw.json"), { force: true });
    writeSnapshot(root, "sgw", { measuredAt: iso(0.5), matrix: probeOnly(iso(0.5)) });
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.equal(g.matrix_from_snapshot, true, "the only run there is must publish");
  }
});

test("#25 CLASS: the snapshot ingest path — newest wins, RECENCY beats existence, inline config, null-safe", () => {
  const iso = (hAgo) => new Date(Date.now() - hAgo * 3600000).toISOString();
  // (a) NEWEST snapshot wins over an older one, and its matrix supersedes the per-suite file.
  {
    const root = buildStreamMemRepo();
    const mk = (rps, at) => ({ gateway: "sgw", build: "snap", matrix_version: 2, served: true, measured_at: at,
      upstreams: { openai: { configurable: true, served: true, cells: { openai: { served: true, perf: {
        added_latency_p50_us: 1, added_latency_p99_us: 2, rps_sustained_20ms: rps, rps_sustained_20ms_mock_bound: false,
        rps_max_proxy: rps + 1, rps_max_proxy_mock_bound: false } } } } } });
    writeSnapshot(root, "sgw", { measuredAt: iso(2), matrix: mk(11111, iso(2)) });
    writeSnapshot(root, "sgw", { measuredAt: iso(0.5), matrix: mk(22222, iso(0.5)) });
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.equal(app.mval(g.best_cell.rps_sustained_20ms), 22222, "the NEWEST snapshot must win");
    assert.equal(g.matrix_from_snapshot, true);
  }
  // (b) #5 RED-before: an OLDER snapshot must NOT shadow a NEWER results/matrix/<gw>.json. Before the
  //     fix the snapshot won by EXISTENCE and this returns the stale 33333.
  {
    const root = certifyRepo(buildStreamMemRepo());   // matrix stamped 1h ago, rps_sustained_20ms 45000
    writeSnapshot(root, "sgw", { measuredAt: iso(72), matrix: { gateway: "sgw", build: "old", matrix_version: 2,
      served: true, measured_at: iso(72), upstreams: { openai: { configurable: true, served: true, cells: { openai: {
        served: true, perf: { added_latency_p50_us: 9, added_latency_p99_us: 9, rps_sustained_20ms: 33333,
          rps_sustained_20ms_mock_bound: false, rps_max_proxy: 33334, rps_max_proxy_mock_bound: false } } } } } } });
    const g = genInto(root).gateways.find((x) => x.key === "sgw");
    assert.equal(app.mval(g.best_cell.rps_sustained_20ms), 45000,
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
    assert.equal(app.mval(g.best_cell.rps_sustained_20ms), 45000, "a matrix-less snapshot must not blank the row");
    assert.ok(g.best_cell.source.kind === "matrix");
  }
});

// ---- #21 CLASS: RIG PROVENANCE — the measurement instrument must describe itself ------------------
// The mock + loadgen are fetched from a MOVING GitHub release tag ("rig"), rebuilt by CI on every
// mock/ or loadgen/ change. So two runs of a byte-identical harness can produce DIFFERENT cell verdicts
// purely because the instrument changed underneath them — which is exactly what happened here: bcf9912
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
      added_latency_p50_us: 1, added_latency_p99_us: 2, rps_sustained_20ms: 100, rps_sustained_20ms_mock_bound: false,
      rps_max_proxy: 101, rps_max_proxy_mock_bound: false } } } } },
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

// ---- #26: conc_at / concAt() — the Performance-tab payload of task #65 — was never exercised -------
test("#26 CLASS: conc_at travels inside the sealed envelope and drives the '@ N conc' render", () => {
  // (a) the accessor: conc_at WINS over the legacy *_concurrency; either alone works; neither -> null.
  assert.equal(app.concAt(sealMetric(9, { gated: true, flag: false, extras: { conc_at: 512, concurrency: 64 } })), 512);
  assert.equal(app.concAt(sealMetric(9, { gated: true, flag: false, extras: { concurrency: 64 } })), 64);
  assert.equal(app.concAt(sealMetric(9, { gated: true, flag: false })), null, "no rung recorded -> null, never fabricated");
  assert.equal(app.concAt(null), null);
  assert.equal(app.concAt(42), null, "a bare scalar is not an envelope");
  // (b) gen-data actually CAPTURES conc_at_sustained / conc_at_peak off the raw cell (the #65 payload).
  const root = certifyRepo(buildStreamMemRepo(), (m) => {
    for (const cells of [m.cells, m.upstreams.openai.cells]) {
      cells.openai.perf.conc_at_sustained = 384;
      cells.openai.perf.conc_at_peak = 192;
    }
  });
  const g = genInto(root).gateways.find((x) => x.key === "sgw");
  assert.equal(app.concAt(g.best_cell.rps_sustained_20ms), 384);
  assert.equal(app.concAt(g.best_cell.rps_max_proxy), 192);
  // (c) the RENDER: the Performance cells show "N @ Y conc" with the operating-concurrency tooltip.
  const st = { ...app.newState(), mode: "peak", data: { gateways: [g] } };
  const sus = app.sustainedChooserCell(g, st), max = app.maxProxyChooserCell(g, st);
  assert.match(sus.text, /@ 384 conc/);
  assert.match(sus.note, /384 concurrent/);
  assert.match(max.text, /@ 192 conc/);
  // (d) the NULL-conc render: no rung recorded -> the bare number, NEVER "@ null conc".
  const noConc = structuredClone(g);
  delete noConc.best_cell.rps_sustained_20ms.conc_at;
  delete noConc.best_cell.rps_sustained_20ms.concurrency;
  const bare = app.sustainedChooserCell(noConc, st);
  assert.ok(!/conc/.test(bare.text), `a cell with no recorded rung must render the bare number; got ${bare.text}`);
});

// ---- #27: the producer's fabricated-0 -> honest-NULL change; the site must be NULL-SAFE ------------
test("#27 CLASS: every RSS field is NULL-SAFE — a null RSS renders 'not measured', never 0", () => {
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
  assert.equal(cell.text, "n/a");
  assert.equal(cell.v, null);
  // (c) #14: the window durations RENDER from the data, not from a hard-coded "60 s" - and they now have
  //     to be found on the CELL, which is where the producer writes them.
  assert.deepEqual(app.memWindows(mem), { idle: 30, recovery: 45 });
  assert.deepEqual(app.boardMemWindows(bundle), { idle: 30, recovery: 45 },
    "the board's window labels must read the PER-CELL windows, not fall back to the 60 s default");
  const cap = app.memoryCaption(bundle, st).join(" ");
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
  const rec = streamRec({ streams_sustained: null, streams_sustained_fps: null, cpu_fps: null,
    streams_sustained_mock_bound: null, cpu_fps_mock_bound: null });
  for (const k of ["streams_sustained", "streams_sustained_fps", "cpu_fps"]) {
    assert.equal(app.mval(rec[k]), null);
    assert.equal(rec[k].suppressed, false, `${k}: an ABSENT reading is not-measured, never "suppressed"`);
    assert.equal(rec[k].reason, "not_measured");
  }
  const g = { key: "n", display: "n", lang: "Rust", streaming: rec };
  assert.equal(app.streamCell(g, "streams_sustained", String).text, "n/a");
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
  // COVERAGE means the SCANNER ran and found its region — NOT that an exemption/error path was hit.
  assert.equal(checkMod.lintSweepKeys(good, "fake.js", region).scanned, true);
  assert.equal(checkMod.lintSweepKeys("nothing here\n", "fake.js", region).scanned, false,
    "a lint that never found its region must NOT report itself covered");
});

test("#15/#20 RED: the C5 accessor-routing lint FIRES on the access style the codebase actually uses", () => {
  // RED (the exact violation that shipped in app.js): bind the envelope to a local, then read .value —
  // on a LATER line than the binding. The old same-line, `.field.value`-only lint could not see this.
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
  assert.ok(checkMod.lintAccessorRouting("const x = p.rps_sustained_20ms.value;\n", "fake.js", "js").errors.length >= 1);
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
  // NON-app.js readers are covered too (charts.py's Python equivalent).
  const badPy = 'def draw(bc):\n    env = bc.get("rps_sustained_20ms")\n    if _is_env(env):\n        return env.get("value")\n';
  assert.ok(checkMod.lintAccessorRouting(badPy, "fake.py", "py").errors.length >= 1,
    "the routing lint must cover non-app.js readers");
  // AND the repo's own two files are CLEAN (the two real violations this lint should have caught are fixed).
  for (const [rel, lang] of [["app.js", "js"], ["../charts.py", "py"]])
    assert.deepEqual(checkMod.lintAccessorRouting(readFileSync(join(HERE, rel), "utf8"), rel, lang).errors, []);
});

test("#2/#22 RED: the per-lane chart-provenance lint and the cross-language caption parity assertion FIRE", () => {
  // #2 RED: the old check only asked whether `_sweep_label(` appeared ANYWHERE, so a lane with NO
  // disclosure (exactly the streaming lane's state) passed. Per-lane, a missing lane is caught.
  const missingStream = 'x = _sweep_label({"sweep": r.get("_perf_source")}) + _sweep_label({"sweep": r.get("_xlate_source")})';
  const r = checkMod.lintChartLaneProvenance(missingStream);
  assert.equal(r.errors.length, 1);
  assert.match(r.errors[0], /_stream_source/);
  const allLanes = missingStream + ' + _sweep_label({"sweep": r.get("_stream_source")})';
  assert.deepEqual(checkMod.lintChartLaneProvenance(allLanes).errors, []);
  // and the REAL charts.py has every lane wired.
  assert.deepEqual(checkMod.lintChartLaneProvenance(readFileSync(join(ROOT, "charts.py"), "utf8")).errors, []);
  // #22 RED: charts.py's comment CLAIMED this parity was asserted; it was not. Now it is.
  const py = 'SWEEP_CAPTION = {\n    "6x6-diagonal", "perf-suite",\n}\n';
  const drift = checkMod.lintCaptionParity(py, ["6x6-diagonal", "perf-suite", "stream-suite"]);
  assert.equal(drift.errors.length, 1);
  assert.match(drift.errors[0], /"stream-suite" exists in app.js but NOT in charts.py/);
  const other = checkMod.lintCaptionParity('SWEEP_CAPTION = {\n    "a", "b",\n}\n', ["a"]);
  assert.ok(other.errors.some((e) => /"b" exists in charts.py but NOT in app.js/.test(e)));
  // and the REAL pair is in sync.
  assert.deepEqual(checkMod.lintCaptionParity(readFileSync(join(ROOT, "charts.py"), "utf8"),
    Object.keys(app.SWEEP_CAPTION)).errors, []);
});

test("#16/#19 RED: R2's own failure path fires, and R1 coverage is claimed only after a real comparison", () => {
  // #19: the missing.length branch — never exercised by any test before. An EMPTY bundle exercises no
  // required branch at all, so R2 must FAIL rather than silently pass on an inert check.
  const empty = checkConsistency({ gateways: [] }, app).errors;
  assert.ok(empty.some((e) => e.startsWith("R2: coverage")),
    `R2 must FAIL when required branches are never exercised; got ${JSON.stringify(empty)}`);
  assert.match(empty.find((e) => e.startsWith("R2: coverage")), /an inert check is itself a failure/);
  // #16: R1.oracle must NOT be reported covered when the oracle compared NOTHING. A gateway that
  // publishes matrix-sourced numbers with no comparable raw cell claims no coverage — and (#18) an
  // unverifiable matrix publish is itself an error, not a silent exemption.
  const g = { key: "no-such-gateway-on-disk", display: "x", lang: "Rust",
    best_cell: bcCell(), measured_at: "2026-07-24T00:00:00Z" };
  const res = checkConsistency({ gateways: [g] }, app);
  assert.ok(!res.cover.has("R1.oracle"), "R1.oracle must not be covered when no comparison happened");
  assert.ok(res.errors.some((e) => e.includes("independent oracle cannot verify")),
    `#18: an unverifiable matrix-sourced publish must be an ERROR, not an exemption; got ${JSON.stringify(res.errors)}`);
  // The REAL bundle does compare, and does claim the coverage.
  assert.ok(checkConsistency(data, app).cover.has("R1.oracle"));
});

// ---- #21 CLASS: the oracle cannot go inert for ANY gateway ---------------------------------------
// THE BUG THIS PINS. The oracle loop was gated on `!g.matrix_from_snapshot` (a snapshot-sourced matrix
// legitimately differs from the trailing per-suite file, so comparing them would false-fail). Once the
// field run made EVERY row snapshot-sourced, that guard silently disabled the entire oracle for 12 of 13
// gateways — and R2's coverage gate was satisfied by the ONE legacy row that still had a per-suite file,
// so a wholly unoracled board reported "R1.oracle covered" and shipped green. Coverage is now
// reconciled PER GATEWAY against the set that publishes matrix-sourced numbers.
test("#21 CLASS: EVERY matrix-publishing gateway is independently oracled (no per-gateway bypass)", () => {
  const res = checkConsistency(data, app);
  assert.ok(!res.errors.some((e) => e.startsWith("R2: coverage")),
    `no gateway may be left unoracled; got ${JSON.stringify(res.errors.filter((e) => e.startsWith("R2")))}`);
  // The bypass was specific to snapshot-sourced rows, so prove a snapshot-sourced row is really compared:
  // corrupt one and require R1 to catch it. (This is the exact case the old guard waved through.)
  const d = clone();
  const g = d.gateways.find((x) => x.matrix_from_snapshot === true
    && x.best_cell && x.best_cell.source && x.best_cell.source.kind === "matrix");
  assert.ok(g, "the bundle must contain a snapshot-sourced matrix gateway for this class test to mean anything");
  g.best_cell.rps_max_proxy = { value: (app.mval(g.best_cell.rps_max_proxy) || 0) + 9999, certified: true, suppressed: false };
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("R1:") && x.includes(g.key) && x.includes("rps_max_proxy")),
    `a corrupted SNAPSHOT-sourced envelope must be caught by the oracle; got: ${JSON.stringify(e.filter((x) => x.startsWith("R1")))}`);
});

// ---- #21 CLASS: R3 — the oracle must verify the SAME run the board published ----------------------
// An oracle that reads a different artifact than gen-data projected from is worse than no oracle: it
// reports green while verifying the wrong file. R3 reconciles the two selections by measured_at.
test("#21 CLASS: R3 catches the board rendering a different run than the oracle resolved", () => {
  assert.ok(checkConsistency(data, app).cover.has("R3.selection"),
    "R3 must actually run on the live bundle");
  const d = clone();
  const g = d.gateways.find((x) => x.matrix && x.matrix.measured_at);
  g.matrix.measured_at = "2020-01-01T00:00:00Z";   // the board claims a run the disk does not have
  const e = checkConsistency(d, app).errors;
  assert.ok(e.some((x) => x.startsWith("R3:") && x.includes(g.key) && x.includes("stale/mis-selected")),
    `R3 must flag a published run that no on-disk artifact backs; got: ${JSON.stringify(e.filter((x) => x.startsWith("R3")))}`);
  // ...and the provenance claim itself must match what is on disk.
  const d2 = clone();
  const g2 = d2.gateways.find((x) => x.matrix_from_snapshot === true);
  delete g2.matrix_from_snapshot;
  assert.ok(checkConsistency(d2, app).errors.some((x) => x.startsWith("R3:") && x.includes("provenance disagreement")),
    "R3 must flag a bundle whose matrix_from_snapshot claim disagrees with the disk");
});

test("#17: the independent oracle covers EVERY matrix cell, translation, streaming and memory — not 2 fields", () => {
  // RED-before, per surface: corrupt ONE sealed envelope on each previously-UNORACLED surface and assert
  // the oracle catches it. Before this change only best_cell's two RPS fields were compared, so each of
  // these mutations shipped undetected.
  const surfaces = [
    ["a non-best matrix CELL", (g) => {
      for (const [eg, up] of Object.entries(g.matrix.upstreams || {}))
        for (const [ing, c] of Object.entries((up && up.cells) || {})) {
          if (!(c && c.perf && c.perf.added_latency_p99_us && c.perf.added_latency_p99_us.value != null)) continue;
          if (ing === g.best_cell.path.dialect && eg === g.best_cell.path.dialect) continue;
          c.perf.added_latency_p99_us = { value: 999999, certified: true, suppressed: false };
          return `matrix[${ing}->${eg}]`;
        }
      return null;
    }],
    ["the TRANSLATION cell", (g) => {
      if (!(g.translation_cell && g.translation_cell.source.kind === "matrix")) return null;
      g.translation_cell.added_latency_p99_us = { value: 424242, certified: true, suppressed: false };
      return "translation_cell";
    }],
    ["a best_cell LATENCY field (ungated, previously unoracled)", (g) => {
      g.best_cell.added_latency_p50_us = { value: 777777, certified: true, suppressed: false };
      return "best_cell.added_latency_p50_us";
    }],
  ];
  let checked = 0;
  for (const [label, mutate] of surfaces) {
    const d = clone();
    const g = matrixGw(d);
    const where = mutate(g);
    if (!where) continue;
    checked += 1;
    const e = checkConsistency(d, app).errors.filter((x) => x.startsWith("R1:"));
    assert.ok(e.some((x) => x.includes(g.key)),
      `the oracle must catch a corrupted envelope on ${label}; got ${JSON.stringify(e)}`);
  }
  assert.ok(checked >= 2, `expected the real bundle to exercise several oracled surfaces, got ${checked}`);
});

test("#21: C6 fires on an INJECTED inversion - the assertion cannot silently pass when a row is absent", () => {
  // The old test asserted only on ONE named gateway's live data and SILENTLY RETURNED when that
  // results/matrix/<gw>.json was missing, so deleting the file (or re-measuring the inversion away)
  // would make it vacuously green - and renaming that gateway would have done it silently too.
  // Drive the invariant DIRECTLY on an injected cell so it can never skip.
  const inverted = { rps_sustained_20ms: 1000, rps_max_proxy: 900 };
  const ok = { rps_sustained_20ms: 900, rps_max_proxy: 1000 };
  const noCeiling = { rps_sustained_20ms: 500, rps_max_proxy: 0 };   // "did not qualify", not an inversion
  const flag = (perf) => {
    const sus = perf.rps_sustained_20ms, max = perf.rps_max_proxy;
    return !(sus == null || max == null || max === 0) && sus > max;
  };
  assert.equal(flag(inverted), true, "an injected inversion MUST be flagged");
  assert.equal(flag(ok), false);
  assert.equal(flag(noCeiling), false, "max_proxy 0 is 'no qualifying ceiling', not an inversion");
  // and the REAL checker agrees on the same injected cell, through its own code path, with a raw matrix
  // present on disk. An inversion is a HARD FAILURE: the number published as that cell's maximum was
  // exceeded by another sweep on the same box, which makes it not a maximum, so the run is not
  // publishable until it is re-measured.
  const { errors, warnings } = checkConsistency(data, app);
  assert.ok(!warnings.some((w) => w.includes("sustained@20ms")),
    "a C6 inversion must not reach the soft channel");
  const c6e = errors.filter((e) => e.includes("sustained@20ms"));
  assert.ok(c6e.every((e) => /sustained@20ms .* > max_proxy /.test(e)),
    "every C6 error must name the inversion it found");
});

// ---- #1 CLASS: "Tested on" describes the record the row ACTUALLY displays, in EVERY lane -----------
test("#1 CLASS: the Tested-on pill renders its OWN lane's provenance in every chooser mode", () => {
  // One gateway: matrix perf on the openai diagonal, but STREAMING from the legacy stream suite (the
  // live shape of all 12 gateways). RED-before: the streaming pill advertised the PERF cell's matrix
  // provenance ("openai-in / openai-out passthrough") and never reached the honest legacy caption.
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
  // The PERF pill: the matrix diagonal it was measured on — no fallback star.
  assert.match(perfPill, /OpenAI/);
  assert.ok(!perfPill.includes(" *"), "a matrix-sourced perf row must not be starred");
  // The STREAMING pill: the STREAM record's own dialect + its own honest legacy caption + a star.
  assert.match(streamPill, /Anthropic/, "the streaming pill must name the dialect the STREAM record used");
  assert.match(streamPill, /stream suite \(legacy\)/, "the streaming pill must disclose its OWN provenance");
  assert.ok(streamPill.includes(" *"), "a live-fallback streaming row must be starred");
  assert.ok(!/passthrough — 6×6 diagonal/.test(streamPill),
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
// The defect these tests pin down: memory used to be ONE scalar per gateway, produced on whichever cell
// had the highest THROUGHPUT. That selected on one axis and reported another, and the cell (i.e. the
// workload) differed row to row. Per-cell measurement moves the choice to the reader, with two hard
// rules: the memory lane must NEVER offer Peak, and nothing may be substituted across cells.

// cellMem: a sealed per-cell memory window, exactly as gen-data seals cell.memory in place.
function cellMem(o = {}) {
  const { steady_state_rss_mib = 100, idle_rss_mib = 20, recovered_rss_mib = 30,
    plateaued = true, time_to_plateau_s = 25, growth_rate_mib_per_min = 0.1, rss_series = null } = o;
  const rec = {
    steady_state_rss_mib: seal(steady_state_rss_mib), idle_rss_mib: seal(idle_rss_mib),
    recovered_rss_mib: seal(recovered_rss_mib), time_to_plateau_s: seal(time_to_plateau_s),
    growth_rate_mib_per_min: seal(growth_rate_mib_per_min), plateaued,
  };
  if (rss_series != null) rec.rss_series = rss_series;
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
  // (2) a SHARED URL carrying ?mode=peak that lands on memory falls back to Same, not to a peak cell.
  assert.equal(app.decodeUrl("/gateways/memory", "?mode=peak").mode, "same",
    "a ?mode=peak link opened on the memory tab must fall back to Same");
  assert.equal(app.decodeUrl("/gateways/performance", "?mode=peak").mode, "peak", "the perf tabs still decode Peak");
  assert.equal(app.decodeUrl("/gateways/performance", "?mode=min").mode, "peak", "Min is not a perf mode");
  assert.equal(app.resolveMode("peak", "memory"), "same");
  assert.equal(app.memoryMode({ mode: "peak" }), "same", "the memory choke point can never return Peak");
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
  assert.equal(growth.text, "14.2 (leak)", "the growth rate IS the reading when a gateway never settles");
  assert.match(growth.note, /never went steady/);
});

test("memory: a gateway that never settles on ANY cell is flagged at GATEWAY level, in every mode", () => {
  const leaky = memGw("leaky", {
    "openai>openai": { steady_state_rss_mib: null, plateaued: false, growth_rate_mib_per_min: 7.5 },
    "openai>gemini": { steady_state_rss_mib: null, plateaued: false, growth_rate_mib_per_min: 12.25 },
  });
  const fine = memGw("fine", { "openai>openai": { steady_state_rss_mib: 44 } });
  assert.equal(app.neverPlateaued(leaky), true);
  assert.equal(app.neverPlateaued(fine), false);
  assert.equal(app.worstGrowth(leaky), 12.25, "the flag quantifies itself with the worst rate across cells");
  // The flag is on the NAME cell, so no choice of cell can hide it.
  for (const mode of ["min", "max", "same", "custom"]) {
    const st = memState([leaky, fine], { mode });
    assert.match(app.COLUMN_SETS.memory.find((c) => c.id === "name").render(leaky, st), /never settles/,
      `the never-settles flag must show in ${mode} mode`);
    assert.ok(!/never settles/.test(app.COLUMN_SETS.memory.find((c) => c.id === "name").render(fine, st)),
      "a gateway that settled must not be flagged");
  }
  // …and only on the memory tab: it is a memory finding, not a general label.
  assert.ok(!/never settles/.test(app.COLUMN_SETS.memory.find((c) => c.id === "name")
    .render(leaky, memState([leaky], { view: "performance" }))));
  // Absence of measurement is not a verdict.
  assert.equal(app.neverPlateaued({ key: "x", display: "x" }), false, "no per-cell data means no verdict");
});

test("memory idle stays OUTSIDE the chooser: median of the cold samples, identical in every mode", () => {
  const g = memGw("g", {
    "openai>openai": { idle_rss_mib: 20, steady_state_rss_mib: 100 },
    "openai>gemini": { idle_rss_mib: 24, steady_state_rss_mib: 200 },
    "anthropic>anthropic": { idle_rss_mib: 22, steady_state_rss_mib: 300 },
  });
  const i = app.idleAcrossCells(g);
  assert.deepEqual({ median: i.median, min: i.min, max: i.max, n: i.n }, { median: 22, min: 20, max: 24, n: 3 });
  const seen = new Set();
  for (const mode of ["min", "max", "same", "custom"]) {
    const c = memCol("memidle").get(g, memState([g], { mode }));
    seen.add(c.text);
    assert.match(c.note, /median of 3 cold samples/, "the spread is disclosed, not hidden behind one sample");
  }
  assert.deepEqual([...seen], ["22.0"], "idle is sampled cold with no cell involved, so it cannot vary by mode");
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
  assert.equal(rt("/gateways/memory", "?mode=min"), "/gateways/memory?mode=min");
  assert.equal(rt("/gateways/memory", "?mode=max"), "/gateways/memory?mode=max");
  assert.equal(rt("/gateways/memory", "?mode=custom&in=openai&out=gemini"),
    "/gateways/memory?mode=custom&in=openai&out=gemini");
  // Same is memory's DEFAULT mode, so it is not spelled out; the dialect is, unless it is the data's own
  // widest-coverage default (which a bundle-less state cannot claim to know).
  assert.equal(rt("/gateways/memory", "?d=anthropic"), "/gateways/memory?d=anthropic");
  const st = { ...app.decodeUrl("/gateways/memory", ""), data: { gateways: [memGw("a", { "openai>openai": {} })] } };
  st.sameDialect = "openai";
  assert.equal(app.encodeUrl(st), "/gateways/memory", "the pristine memory view keeps a clean URL");
  // Old shared links: the sort id is a URL CONTRACT and survives the column's rename.
  const old = app.decodeUrl("/gateways/memory", "?sort=mempeak&dir=asc");
  assert.equal(old.sortCol, "mempeak");
  assert.equal(old.mode, "same", "an old memory link with no mode lands on Same");
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
  assert.match(app.memoryCaption(st.data, st).join(" "), /chosen by throughput/);
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

console.log(`\n${passed} tests passed`);
