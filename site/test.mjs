#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// test.mjs: node smoke test for the results site. No dependencies, no browser:
// app.js exports its pure logic (filtering, hash codec, sweep chart) when run
// under node, and the canvas is exercised through a recording 2d-context stub.
//
//   node site/test.mjs
//
// Covers: gen-data emits GW_CLASS for every gateway; search/class/lang/capability
// filtering; URL-hash state round-trip; the sweep chart component drawing real
// committed sweep data through the stub canvas.

import { execFileSync } from "node:child_process";
import { readFileSync, mkdtempSync, rmSync } from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import assert from "node:assert/strict";

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

// ---- "supports governance" filters on DECLARED capability, not run outcome ----
test("governance filter keys on capability (supports_governed), not measurement success", () => {
  const st = app.newState();
  st.needGoverned = true;
  const shown = app.applyFilters(data.gateways, st);
  // Every gateway the filter keeps must be capability-flagged (supports_governed), regardless of
  // whether a given run served — a capable gateway whose measurement failed must NOT be excluded.
  assert.ok(shown.every((g) => g.supports_governed));
  // A gateway whose governed note says the manifest defines no governed launch is genuinely
  // unsupported and must be excluded; a capable one whose note is a launch failure must be kept.
  const capableButFailed = data.gateways.find(
    (g) => g.governed && g.governed.governed_served === false
      && !/manifest defines no/.test(g.governed.governed_note || ""));
  const genuinelyUnsupported = data.gateways.find(
    (g) => g.governed && /manifest defines no/.test(g.governed.governed_note || ""));
  if (capableButFailed) assert.ok(shown.includes(capableButFailed), "capable-but-failed gateway must still show");
  if (genuinelyUnsupported) assert.ok(!shown.includes(genuinelyUnsupported), "genuinely-unsupported gateway must be filtered out");
});

// ---- URL hash state round-trip ----------------------------------------------
test("hash state round-trips", () => {
  const st = app.newState();
  st.view = "results";
  st.q = "bus bar & co";
  st.classes = new Set(["Control plane", "LLM gateway"]);
  st.langs = new Set(["Rust"]);
  st.needXlate = true;
  st.sortCol = "lat";
  st.sortDesc = false;
  st.cmp = ["busbar", "bifrost"];
  st.cmpOpen = true;
  st.drawer = "busbar";
  const hash = app.encodeState(st);
  const back = app.decodeState(`#${hash}`);
  for (const k of ["view", "q", "sortCol", "sortDesc", "needStream", "needXlate", "needGoverned", "cmpOpen", "drawer"]) {
    assert.deepEqual(back[k], st[k], `field ${k}`);
  }
  assert.deepEqual([...back.classes].sort(), [...st.classes].sort());
  assert.deepEqual([...back.langs].sort(), [...st.langs].sort());
  assert.deepEqual(back.cmp, st.cmp);
});

test("default state encodes to an empty hash and decodes back to defaults", () => {
  assert.equal(app.encodeState(app.newState()), "");
  const back = app.decodeState("");
  const def = app.newState();
  assert.equal(back.sortCol, def.sortCol);
  assert.equal(back.sortDesc, def.sortDesc);
  assert.equal(back.drawer, null);
  assert.deepEqual(back.cmp, []);
});

test("decode rejects a bogus sort column", () => {
  const back = app.decodeState("#sort=evil&dir=asc");
  assert.equal(back.sortCol, "rps20");
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
  const pass = data.gateways.find((g) => g.xlate && g.xlate.xlate_passthrough === true);
  assert.ok(pass, "expected at least one passthrough gateway in the field");
  assert.equal(app.naText(pass.xlate, "xlate_served", "xlate_error").text, "n/a (passthrough)");
  const unsupported = data.gateways.find((g) =>
    g.governed && g.governed.governed_served === false && /manifest defines no/.test(g.governed.governed_note || ""));
  assert.ok(unsupported, "expected a gateway without native governance");
  assert.equal(app.naText(unsupported.governed, "governed_served", "governed_note").text, "not supported");
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

console.log(`\n${passed} tests passed`);
