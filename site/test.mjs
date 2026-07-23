#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// test.mjs: node smoke test for the results site. No dependencies, no browser:
// app.js exports its pure logic (filtering, URL codec, sweep chart) when run
// under node, and the canvas is exercised through a recording 2d-context stub.
//
//   node site/test.mjs
//
// Covers: gen-data emits GW_CLASS for every gateway; search/class/lang/capability
// filtering; path-URL state round-trip (/<category>/<view>?<params>) including
// legacy-hash decoding; the sweep chart component drawing real committed sweep
// data through the stub canvas.

import { execFileSync } from "node:child_process";
import { readFileSync, mkdtempSync, rmSync } from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import assert from "node:assert/strict";
import { checkConsistency } from "./check-consistency.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const app = createRequire(import.meta.url)(join(HERE, "app.js"));

let passed = 0;
function test(name, fn) {
  fn();
  passed += 1;
  console.log(`ok - ${name}`);
}

// ---- gen-data: run it for real into a temp dir ------------------------------
const out = mkdtempSync(join(tmpdir(), "site-test-"));
execFileSync(process.execPath, [join(HERE, "gen-data.mjs"), ROOT, out], { stdio: "pipe" });
const data = JSON.parse(readFileSync(join(out, "data.json"), "utf8"));
rmSync(out, { recursive: true, force: true });

test("gen-data emits gateways with a class for every entry", () => {
  assert.ok(data.gateways.length >= 10, `expected a full field, got ${data.gateways.length}`);
  for (const g of data.gateways) {
    assert.ok(typeof g.cls === "string" && g.cls.length > 0, `${g.key} has no cls`);
  }
  const busbar = data.gateways.find((g) => g.key === "busbar");
  assert.equal(busbar.cls, "Control plane");
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

test("class filter matches the manifest self-description", () => {
  const st = app.newState();
  st.classes = new Set(["Control plane"]);
  const rows = app.applyFilters(data.gateways, st);
  assert.deepEqual(rows.map((g) => g.key), ["busbar"]);
});

test("language filter and capability toggles combine", () => {
  const st = app.newState();
  st.langs = new Set(["Rust"]);
  const rust = app.applyFilters(data.gateways, st);
  assert.ok(rust.length > 0 && rust.every((g) => g.lang === "Rust"));
  // stream results are not committed yet: the capability toggle must degrade to
  // an empty (not crashing) result set, never a throw.
  st.needStream = true;
  const streaming = app.applyFilters(data.gateways, st);
  assert.ok(streaming.every((g) => g.stream && g.stream.stream_served));
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
  st.classes = new Set(["Control plane", "LLM gateway"]);
  st.langs = new Set(["Rust"]);
  st.needXlate = true;
  st.sortCol = "lat";
  st.sortDesc = false;
  st.cmp = ["busbar", "bifrost"];
  st.cmpOpen = true;
  st.drawer = "busbar";
  const url = app.encodeUrl(st);
  assert.ok(url.startsWith("/gateways/matrix?"), `path carries category+view: ${url}`);
  const back = app.decodeUrl(...parts(url));
  for (const k of ["category", "view", "q", "sortCol", "sortDesc", "needStream", "needXlate", "cmpOpen", "drawer"]) {
    assert.deepEqual(back[k], st[k], `field ${k}`);
  }
  assert.deepEqual([...back.classes].sort(), [...st.classes].sort());
  assert.deepEqual([...back.langs].sort(), [...st.langs].sort());
  assert.deepEqual(back.cmp, st.cmp);
});

test("default state encodes to /gateways and decodes back to defaults", () => {
  assert.equal(app.encodeUrl(app.newState()), "/gateways");
  const back = app.decodeUrl("/gateways", "");
  const def = app.newState();
  assert.equal(back.category, "gateways");
  assert.equal(back.view, "passthrough");
  assert.equal(back.sortCol, def.sortCol);
  assert.equal(back.sortDesc, def.sortDesc);
  assert.equal(back.drawer, null);
  assert.deepEqual(back.cmp, []);
});

test("root, unknown paths and unknown views normalize to gateways passthrough", () => {
  assert.equal(app.decodeUrl("/", "").category, "gateways");
  assert.equal(app.decodeUrl("/", "").view, "passthrough");
  assert.equal(app.decodeUrl("/index.html", "").view, "passthrough");
  assert.equal(app.decodeUrl("/no-such-category/matrix", "").category, "gateways");
  assert.equal(app.decodeUrl("/gateways/no-such-view", "").view, "passthrough");
  // legacy view aliases still resolve onto the new tabs
  assert.equal(app.decodeUrl("/gateways/results", "").view, "passthrough");
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
  assert.deepEqual([...st.langs], ["Rust"]);
  // and re-encoding a legacy state yields the clean path form
  assert.ok(app.encodeUrl(st).startsWith("/gateways/matrix?"));
});

test("decode rejects a bogus sort column", () => {
  const back = app.decodeUrl("/gateways", "?sort=evil&dir=asc");
  assert.equal(back.sortCol, "rps20");
});

test("a direct URL load defaults each tab to its column's natural direction", () => {
  // Passthrough / Translation headline on Sustained RPS -> descending (higher is better)
  const pass = app.decodeUrl("/gateways", "");
  assert.equal(pass.sortCol, "rps20");
  assert.equal(pass.sortDesc, true);
  const xlate = app.decodeUrl("/gateways/translation", "");
  assert.equal(xlate.sortCol, "xlrps");
  assert.equal(xlate.sortDesc, true);
  // Streaming headline on added TTFT -> ASCENDING (lower is better); the hard-refresh bug
  // was this defaulting to descending and floating the worst gateway to the top.
  const stream = app.decodeUrl("/gateways/streaming", "");
  assert.equal(stream.sortCol, "sttft");
  assert.equal(stream.sortDesc, false);
});

// ---- three-tab split: honest passthrough / translation sourcing ---------------
const mkMatrix = (cells) => ({ upstreams: Object.fromEntries(
  Object.entries(cells).map(([eg, ing]) => [eg, { cells: Object.fromEntries(
    Object.entries(ing).map(([i, c]) => [i, c])) }])) });

test("Passthrough is BEST-OF: every gateway shows on its best diagonal, none filtered", () => {
  // best_cell (openai diagonal) -> that number
  const green = { best_cell: { rps_sustained_20ms: 30000, dialect: "openai" } };
  assert.equal(app.passCell(green, "rps_sustained_20ms", String).na, false);
  // no swept diagonal -> fall back to the perf suite so the row is never blank
  const unswept = { perf: { served: true, rps_sustained_20ms: 5541 },
    matrix: mkMatrix({ openai: { openai: { served: true } } }) };
  assert.equal(app.passCell(unswept, "rps_sustained_20ms", String).text, "5541");
  // openai not served: BEST-OF shows the native diagonal (litellm-rust -> anthropic), NOT n/a and
  // NOT filtered. gen-data picks it; here best_cell carries the anthropic number.
  const native = { best_cell: { rps_sustained_20ms: 32354, dialect: "anthropic" } };
  assert.equal(app.passCell(native, "rps_sustained_20ms", String).na, false);
  assert.equal(app.passCell(native, "rps_sustained_20ms", String).text, "32354");
  // and Passthrough does NOT filter: a gateway with only a native diagonal still appears
  const st = app.newState(); // view passthrough
  const rows = app.applyFilters([{ display: "x", key: "x", lang: "Rust", ...native }], st);
  assert.equal(rows.length, 1);
});

test("Translation tab lists only gateways serving the pinned in->out pair", () => {
  // g0 serves openai->anthropic (the default pair), g1 serves only openai->gemini.
  const g0 = { display: "g0", key: "g0", lang: "Rust",
    matrix: mkMatrix({ anthropic: { openai: { served: true, perf: { rps_sustained_20ms: 100, added_latency_p99_us: 200 } } } }) };
  const g1 = { display: "g1", key: "g1", lang: "Go",
    matrix: mkMatrix({ gemini: { openai: { served: true, perf: { rps_sustained_20ms: 90, added_latency_p99_us: 300 } } } }) };
  const st = app.newState();
  st.view = "translation"; // default pair openai -> anthropic
  assert.deepEqual(app.applyFilters([g0, g1], st).map((g) => g.key), ["g0"]);
  // repin to openai -> gemini and the row set follows the pair
  st.xlateOut = "gemini";
  assert.deepEqual(app.applyFilters([g0, g1], st).map((g) => g.key), ["g1"]);
  // the cell reader returns the pinned pair's perf
  st.xlateOut = "anthropic";
  assert.equal(app.xlateCell(g0, "rps_sustained_20ms", String).text, "100");
  assert.equal(app.xlateCell(g0, "rps_sustained_20ms", String).na, false);
});

// ---- consistency guard: one canonical value per (gateway, metric) -----------
test("consistency guard: table == drawer == compare == charts on the real bundle", () => {
  const { errors, warnings } = checkConsistency(data, app);
  for (const w of warnings) console.warn(`  warn - ${w}`); // R7 inversions: visible, never fatal
  assert.deepEqual(errors, [], `numeric divergence across surfaces:\n${errors.join("\n")}`);
});

test("divergent best_cell vs perf suite: every surface resolves to best_cell", () => {
  // A gateway whose matrix sweep and perf suite DISAGREE (the exact bug class this guard
  // exists for): the table, the drawer/compare lane accessor, and the charts read must all
  // return the best_cell (canonical) value, never the perf-suite scalar.
  const g = {
    key: "diverge", display: "Diverge", lang: "Rust",
    best_cell: { ingress: "openai", egress: "openai", dialect: "openai", source: "matrix",
      added_latency_p50_us: 100, added_latency_p99_us: 111,
      rps_sustained_20ms: 22222, rps_max_proxy: 33333 },
    perf: { served: true, added_latency_p50_us: 900, added_latency_p99_us: 999,
      rps_sustained_20ms: 11111, rps_max_proxy: 22221 },
  };
  // table
  assert.equal(app.passCell(g, "added_latency_p99_us", String).v, 111);
  assert.equal(app.passCell(g, "rps_sustained_20ms", String).v, 22222);
  assert.equal(app.passCell(g, "rps_max_proxy", String).v, 33333);
  // drawer + compare read the SAME accessor (wired as the perf lane's `get`)
  const perfLane = app.LANES.find((l) => l.key === "perf");
  assert.equal(perfLane.get, app.canonicalPerf, "perf lane must read the canonical accessor");
  const rec = perfLane.get(g);
  assert.equal(rec.added_latency_p99_us, 111);
  assert.equal(rec.rps_sustained_20ms, 22222);
  assert.equal(rec.rps_max_proxy, 33333);
  // and the guard agrees this gateway is consistent (all surfaces on best_cell)
  assert.deepEqual(checkConsistency({ gateways: [g] }, app).errors, []);
  // sanity: if a surface DID read the perf scalar, the guard would fail. Simulate by
  // stripping best_cell from the charts-side view only: not constructible through the real
  // accessors, so instead assert the guard catches a poisoned canonical record.
  const poisoned = { ...g, best_cell: { ...g.best_cell, rps_sustained_20ms: null } };
  // table/drawer show n/a for the null field while charts read null too: still consistent
  assert.deepEqual(checkConsistency({ gateways: [poisoned] }, app).errors, []);
  assert.equal(app.passCell(poisoned, "rps_sustained_20ms", String).v, null, "best_cell is THE record; no silent perf patch");
});

test("divergent translation_cell vs xlate suite: drawer/compare read the matrix cell", () => {
  const g = {
    key: "xdiv", display: "XDiv", lang: "Go",
    translation_cell: { ingress: "openai", egress: "anthropic", source: "matrix",
      added_latency_p50_us: 10, added_latency_p99_us: 20, rps_sustained_20ms: 3000 },
    xlate: { xlate_served: true, xlate_added_latency_p99_us: 9999, xlate_rps_sustained_20ms: 1 },
  };
  const lane = app.LANES.find((l) => l.key === "xlate");
  assert.equal(lane.get, app.canonicalXlate, "xlate lane must read the canonical accessor");
  const rec = lane.get(g);
  assert.equal(rec.xlate_added_latency_p99_us, 20);
  assert.equal(rec.xlate_rps_sustained_20ms, 3000);
  assert.ok(lane.pathNote(rec).includes("OpenAI in -> Anthropic out"), "direction disclosed");
  assert.deepEqual(checkConsistency({ gateways: [g] }, app).errors, []);
});

test("guard warns (never fails) on a sustained > max-proxy inversion", () => {
  const g = { key: "inv", display: "Inv", lang: "Rust",
    best_cell: { dialect: "openai", source: "matrix",
      added_latency_p99_us: 100, rps_sustained_20ms: 12879, rps_max_proxy: 12700 } };
  const { errors, warnings } = checkConsistency({ gateways: [g] }, app);
  assert.deepEqual(errors, []);
  assert.equal(warnings.length, 1);
  assert.ok(warnings[0].includes("noise"));
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

// ---- compact not-served labels (compare + results cells) --------------------
test("naText keeps long diagnostic notes out of cell values", () => {
  assert.deepEqual(app.naText(null, "xlate_served", "xlate_error"), { text: "not measured", note: "" });
  for (const g of data.gateways) {
    for (const l of app.LANES) {
      const j = g[l.key];
      if (!j || j[l.flag] !== false) continue;
      const na = app.naText(j, l.flag, l.err);
      assert.ok(na.text.length <= 24, `${g.key}/${l.key}: label too long: ${na.text}`);
      assert.equal(na.note, j[l.err] || "", `${g.key}/${l.key}: full note preserved`);
    }
  }
  // Data-dependent: the field may or may not currently contain an untranslated-passthrough
  // gateway (it comes and goes with re-runs). Assert the label only when one exists; always
  // assert the mapping itself on a synthetic record so the rule stays covered.
  const pass = data.gateways.find((g) => g.xlate && g.xlate.xlate_passthrough === true);
  if (pass) assert.equal(app.naText(pass.xlate, "xlate_served", "xlate_error").text, "n/a (passthrough)");
  assert.equal(app.naText({ xlate_served: false, xlate_passthrough: true }, "xlate_served", "xlate_error").text, "n/a (passthrough)");
  const unsupported = data.gateways.find((g) =>
    g.governed && g.governed.governed_served === false && /manifest defines no/.test(g.governed.governed_note || ""));
  assert.ok(unsupported, "expected a gateway without native governance");
  // "manifest defines no <hook>" = the harness never probed it: "not tested", never a capability verdict
  assert.equal(app.naText(unsupported.governed, "governed_served", "governed_note").text, "not tested");
});

// ---- per-cell perf: best-path deviation on the matrix hover -----------------
test("cellPerfTip shows a green cell's perf and its deviation from the gateway's best cell", () => {
  const best = { ingress: "openai", egress: "openai", rps_sustained_20ms: 30000 };
  const green = { served: true, perf: { rps_sustained_20ms: 25500, added_latency_p99_us: 900 } };
  const tip = app.cellPerfTip(green, "anthropic", "openai", best);
  assert.ok(tip.includes("25,500 req/s @20ms"), tip);
  assert.ok(tip.includes("+900 µs p99 added"), tip);
  assert.ok(tip.includes("-15.0% req/s vs the openai→openai cell"), tip);
  const bestTip = app.cellPerfTip({ served: true, perf: best }, "openai", "openai", best);
  assert.ok(bestTip.includes("reference cell"), bestTip);
  // red/grey/unprobed cells and perf-less greens carry NO perf line
  assert.equal(app.cellPerfTip({ served: false, perf: { rps_sustained_20ms: 1 } }, "a", "b", best), "");
  assert.equal(app.cellPerfTip({ served: "not_configurable" }, "a", "b", best), "");
  assert.equal(app.cellPerfTip({ served: true }, "a", "b", best), "");
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

test("sweep chart draws real committed sweep data", () => {
  const perf = JSON.parse(readFileSync(join(ROOT, "results", "perf", "busbar.json"), "utf8"));
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
  const reason = "Kong 3.8 ai-proxy accepts only OpenAI-canonical ingress and emits no OpenAI-Responses route_type";
  const tip = app.matrixCellTip({ served: "not_configurable", verdict_note: reason });
  // HONEST wording: grey = not in the grid WE drafted, not a claim the maintainer declined it.
  assert.ok(tip.includes("not in the capability grid we drafted"), "reads as our omission, not the gateway's declared incapability");
  assert.ok(tip.includes(reason), "carries the cited capability-limit reason");
  // no reason present: still honest, never a bare "untested"
  const bare = app.matrixCellTip({ served: "not_configurable" });
  assert.ok(bare.includes("not in the capability grid we drafted"));
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

console.log(`\n${passed} tests passed`);
