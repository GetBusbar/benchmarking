#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// test-coverage.mjs: coverage-sweep siblings to site/test.mjs, importing the same modules.
//
//   node site/test-coverage.mjs
//
// Two families, both class invariants rather than spot checks:
//
//   1. THE URL CODEC AS A FIXED POINT, enumerated: for EVERY category, view, chooser mode, dialect
//      selection and sortable column, encode(decode(encode(state))) === encode(state). test.mjs pins
//      several specific links; this holds the whole space, so a new tab or mode cannot ship with a
//      link that silently drops part of its state.
//
//   2. ONE ENVELOPE, ONE STORY ON EVERY SURFACE: the same sealed metric must tell the same story on
//      the table cell, the drawer/compare lane record, the matrix popup and the rank value - for a
//      measured zero, a below-resolution absence, a measured failure, a rig-limited suppression and
//      an untested cell. A surface that disagrees is publishing a second truth.

import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import assert from "node:assert/strict";
import { sealMetric, ZERO_NO_CEILING, ZERO_MEASURED_FAIL } from "./seal.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const app = createRequire(import.meta.url)(join(HERE, "app.js"));

let passed = 0;
// Same runner contract as test.mjs: a failing test is recorded and the run continues, then the
// process exits non-zero listing every failure. Ordering must not decide coverage.
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
  if (!failures.length) {
    console.log(`\n${passed} passing`);
    return;
  }
  console.error(`\n${failures.length} FAILING test(s):`);
  for (const f of failures) console.error(`  - ${f.name}`);
  process.exitCode = 1;
});

const parts = (url) => {
  const u = new URL(url, "https://onthebench.ai");
  return [u.pathname, u.search];
};
const roundTrip = (st) => {
  const url = app.encodeUrl(st);
  const back = app.decodeUrl(...parts(url));
  return { url, back, again: app.encodeUrl(back) };
};
const DIALECTS = ["openai", "openai-responses", "anthropic", "gemini", "cohere", "bedrock"];

// ---- family 1: the URL codec, enumerated ------------------------------------------------------

test("every (category, view) encodes to a URL that decodes back to itself, as a fixed point", () => {
  for (const category of Object.keys(app.CATEGORIES)) {
    for (const view of app.VIEWS) {
      const st = app.newState();
      st.category = category;
      st.view = view;
      const { url, back, again } = roundTrip(st);
      assert.equal(back.category, category, `${url}: category survives`);
      assert.equal(back.view, view, `${url}: view survives`);
      assert.equal(again, url, `${url}: encode(decode(url)) must be the identical URL`);
    }
  }
  // And home, the one view outside the category space.
  const home = app.newState();
  home.view = app.HOME_VIEW;
  const { url, back, again } = roundTrip(home);
  assert.equal(url, "/");
  assert.equal(back.view, app.HOME_VIEW);
  assert.equal(again, "/");
});

test("every sortable column of every table view round-trips in both directions", () => {
  for (const view of app.TABLE_VIEWS) {
    const cols = app.columnsFor(view).filter((c) => c.id && c.id !== "sel");
    assert.ok(cols.length >= 3, `${view} declares sortable columns`);
    for (const col of cols) {
      for (const desc of [true, false]) {
        const st = app.newState();
        st.view = view;
        st.sortCol = col.id;
        st.sortDesc = desc;
        const { url, back, again } = roundTrip(st);
        assert.equal(back.sortCol, col.id, `${view}/${col.id} dir=${desc}: column survives ${url}`);
        assert.equal(back.sortDesc, desc, `${view}/${col.id} dir=${desc}: direction survives ${url}`);
        assert.equal(again, url, `${view}/${col.id}: fixed point`);
      }
    }
  }
});

test("every chooser mode of every chooser view round-trips with its selection intact", () => {
  for (const view of app.CHOOSER_VIEWS) {
    for (const mode of app.modesFor(view)) {
      const st = app.newState();
      st.view = view;
      st.mode = mode;
      if (mode === "same") {
        st.sameDialect = "anthropic";
        st.sameDialectPinned = true;
      }
      if (mode === "custom") {
        st.xlateIn = "gemini";
        st.xlateOut = "cohere";
      }
      const { url, back, again } = roundTrip(st);
      assert.equal(back.view, view, url);
      assert.equal(back.mode, mode, `${view}: mode ${mode} must survive its own view's URL: ${url}`);
      if (mode === "same") assert.equal(back.sameDialect, "anthropic", `${view}/same: dialect survives ${url}`);
      if (mode === "custom") {
        assert.equal(back.xlateIn, "gemini", `${view}/custom: ingress survives ${url}`);
        assert.equal(back.xlateOut, "cohere", `${view}/custom: egress survives ${url}`);
      }
      assert.equal(again, url, `${view}/${mode}: fixed point`);
    }
  }
});

test("every dialect and every ingress->egress pair survives the codec (no dialect is special-cased)", () => {
  for (const d of DIALECTS) {
    const st = app.newState();
    st.view = "performance";
    st.mode = "same";
    st.sameDialect = d;
    const { back } = roundTrip(st);
    assert.equal(back.sameDialect, d, `same-dialect ${d} must survive`);
    assert.equal(back.mode, "same");
  }
  for (const cin of DIALECTS) {
    for (const cout of DIALECTS) {
      const st = app.newState();
      st.view = "performance";
      st.mode = "custom";
      st.xlateIn = cin;
      st.xlateOut = cout;
      const { back, url, again } = roundTrip(st);
      assert.equal(back.xlateIn, cin, `${cin}->${cout}: ingress survives ${url}`);
      assert.equal(back.xlateOut, cout, `${cin}->${cout}: egress survives ${url}`);
      assert.equal(again, url, `${cin}->${cout}: fixed point`);
    }
  }
  // A dialect the matrix does not know is refused, never smuggled into state.
  const bogus = app.decodeUrl("/gateways/performance", "?mode=same&d=grpc-teapot");
  assert.equal(bogus.sameDialect, app.newState().sameDialect, "an unknown dialect falls back to the default");
});

test("a mode is coerced onto the view that receives it, in EVERY illegal (view, mode) pairing", () => {
  // The shared-link case: a perf-mode link opened on memory (and vice versa) must land on that
  // view's own default, never render a selection the view does not offer.
  for (const view of app.CHOOSER_VIEWS) {
    const offered = app.modesFor(view);
    const all = new Set([...app.CHOOSER_MODES, ...app.MEM_CHOOSER_MODES]);
    for (const mode of all) {
      const st = app.decodeUrl(`/gateways/${view}`, `?mode=${mode}`);
      if (offered.has(mode)) {
        assert.equal(st.mode, mode, `${view} offers ${mode} and must keep it`);
      } else {
        assert.equal(
          st.mode, app.defaultMode(view),
          `${view} does not offer ${mode}; it must fall back to ${app.defaultMode(view)}, got ${st.mode}`,
        );
      }
    }
  }
});

test("compare/drawer/search/caps state survives alongside the chooser on one URL", () => {
  const st = app.newState();
  st.view = "streaming";
  st.mode = "same";
  st.sameDialect = "gemini";
  st.sameDialectPinned = true;
  st.q = "kong & friends";
  st.needStream = true;
  st.cmp = ["alpha", "bravo", "charlie"];
  st.cmpOpen = true;
  st.drawer = "bravo";
  const { url, back, again } = roundTrip(st);
  assert.equal(back.q, st.q, url);
  assert.equal(back.needStream, true, url);
  assert.deepEqual(back.cmp, st.cmp, url);
  assert.equal(back.cmpOpen, true, url);
  assert.equal(back.drawer, "bravo", url);
  assert.equal(back.mode, "same", url);
  assert.equal(back.sameDialect, "gemini", url);
  assert.equal(again, url, "the fully-loaded state is still a fixed point");
});

// ---- family 2: one envelope, one story on every surface ---------------------------------------

// A gateway whose openai->anthropic cell carries one field per story, so every surface can be read
// off the SAME record. Metrics seal through the REAL sealMetric, exactly as gen-data does.
const IN = "openai";
const OUT = "anthropic";
const STORY_CELL_PERF = {
  // story A: a measured zero with its ceiling note - a real result, not a hole.
  rps_sustained_20ms: sealMetric(0, { gated: true }),
  // story B: a below-resolution absence - the best result the comparison can express.
  added_latency_p99_us: sealMetric(null, {
    absent: { reason: "below_resolution", detail: "the gateway leg (20073us) came in under the direct leg (21070us)" },
  }),
  // story C: a measured failure - digits prove the window ran; red says the gateway failed it.
  added_latency_p50_us: sealMetric(null, {
    absent: { reason: "not_measured", detail: "the gateway leg at c=1 was not clean: 0 ok, 14201 fail" },
  }),
  // story D: a rig-limited suppression - the number is gone, only the reason remains.
  rps_max_proxy: sealMetric(99999, { gated: true, flag: true }),
};
const STORY_GW = {
  key: "storygw",
  display: "Story GW",
  lang: "Rust",
  matrix: {
    upstreams: {
      [OUT]: { cells: { [IN]: { served: true, status: 200, perf: STORY_CELL_PERF } } },
    },
  },
};
const CUSTOM = { mode: "custom", xlateIn: IN, xlateOut: OUT };
const perfLane = app.LANES.find((l) => l.key === "perf");

test("a measured ZERO is the number 0 on every surface, with its meaning visible, never n/a", () => {
  const table = app.chooserPerfCell(STORY_GW, "rps_sustained_20ms", String, CUSTOM);
  assert.equal(table.na, false, "a measured zero is not a hole");
  assert.equal(table.text, "0", "the table shows the digit");
  assert.equal(table.v, 0, "it ranks as 0");
  assert.match(app.metricTd(table), /zero-why/, "the cell says what the zero means, on the cell");

  const rec = app.laneRecord(perfLane, STORY_GW, CUSTOM);
  assert.ok(rec, "the drawer/compare lane reads the same cell");
  assert.equal(app.mval(rec.rps_sustained_20ms), 0, "drawer/compare rank the same 0");
  assert.equal(rec.rps_sustained_20ms.note, ZERO_NO_CEILING, "the note travels with the record");

  const pop = app.cellPopFull(STORY_GW, IN, OUT);
  assert.match(pop, /Sustained @20ms/, "the popup shows the measured-zero row");
  assert.match(pop, /<b>0<\/b>/, "the popup shows the same 0 the table shows");
});

test("a BELOW-RESOLUTION absence reads as approximately-zero on every surface, never a bare n/a", () => {
  const table = app.chooserPerfCell(STORY_GW, "added_latency_p99_us", String, CUSTOM);
  assert.equal(table.na, false, "below-resolution is a display state, not a hole");
  assert.equal(table.text, "≈0", "the table reads approximately zero");
  assert.equal(table.v, 0, "it ranks as 0, equal-best on a lower-is-better sort");
  assert.match(table.note, /came in under the direct leg/, "the engine's evidence is the tooltip");

  const rec = app.laneRecord(perfLane, STORY_GW, CUSTOM);
  assert.equal(app.mval(rec.added_latency_p99_us), 0, "drawer/compare rank the same 0");
  assert.equal(rec.added_latency_p99_us.reason, "below_resolution", "the reason survives on the record");

  const pop = app.cellPopFull(STORY_GW, IN, OUT);
  assert.match(pop, /Added latency p99/, "the popup shows the row - a win too small to weigh is not hidden");
});

test("a MEASURED FAILURE yields a number on NO surface, but carries its counts on every one that shows it", () => {
  const table = app.chooserPerfCell(STORY_GW, "added_latency_p50_us", String, CUSTOM);
  assert.equal(table.failed, true, "the table knows this is a measured failure");
  assert.equal(table.text, "failed · 0/14,201", "the counts are the cell text");
  assert.equal(table.v, null, "a failure never ranks as a number");
  assert.match(app.metricTd(table), /failcell/, "the td carries the red failure class");

  const rec = app.laneRecord(perfLane, STORY_GW, CUSTOM);
  assert.equal(app.mval(rec.added_latency_p50_us), null, "drawer/compare agree: no number");

  // The failure is DISTINCT from an untested cell on every surface: an untested cell has no counts.
  const untested = app.chooserPerfCell(STORY_GW, "added_latency_p50_us", String, { mode: "custom", xlateIn: "gemini", xlateOut: "cohere" });
  assert.equal(untested.text, "n/a");
  assert.ok(!untested.failed, "an untested cell must never wear the failure red");
});

test("a RIG-LIMITED suppression is n/a on every surface and the raw number is unrecoverable from any of them", () => {
  const table = app.chooserPerfCell(STORY_GW, "rps_max_proxy", String, CUSTOM);
  assert.equal(table.na, true, "a rig-bound number is suppressed");
  assert.equal(table.v, null);
  assert.match(table.note, /rig-limited|mock/i, "the note names the rig, not the gateway");

  const rec = app.laneRecord(perfLane, STORY_GW, CUSTOM);
  assert.equal(app.mval(rec.rps_max_proxy), null, "drawer/compare agree: suppressed");
  assert.ok(
    !JSON.stringify(rec).includes("99999"),
    "the suppressed number must not be recoverable from the drawer/compare record",
  );

  const pop = app.cellPopFull(STORY_GW, IN, OUT);
  assert.ok(!pop.includes("Max proxy RPS"), "the popup omits the suppressed row rather than showing a number");
  assert.ok(!pop.includes("99999"), "the suppressed number must not leak into the popup HTML");
});

test("an UNTESTED cell is empty on every surface: no row, no record, no popup, no fabricated value", () => {
  const missing = { mode: "custom", xlateIn: "gemini", xlateOut: "cohere" };
  assert.equal(app.chooserHasCell(STORY_GW, missing), false, "the chooser knows there is no cell");
  assert.equal(app.chooserPerfCell(STORY_GW, "rps_sustained_20ms", String, missing).na, true, "the table reads n/a");
  assert.equal(app.laneRecord(perfLane, STORY_GW, missing), null, "the drawer/compare lane has no record");
  assert.equal(app.cellPopFull(STORY_GW, "gemini", "cohere"), "", "the popup renders nothing for a cell that does not exist");
});

test("the popup reads the SAME chosen-cell values the table reads, for the same coordinates", () => {
  // The agreement stated directly: every non-n/a perf value the table would show for this cell
  // appears verbatim in the popup, and nothing else does.
  const pop = app.cellPopFull(STORY_GW, IN, OUT);
  for (const [key, label] of [
    ["rps_sustained_20ms", "Sustained @20ms"],
    ["added_latency_p99_us", "Added latency p99"],
  ]) {
    const cell = app.chooserPerfCell(STORY_GW, key, String, CUSTOM);
    assert.equal(cell.na, false, `${key} is displayable`);
    assert.ok(pop.includes(label), `the popup shows ${label}`);
  }
  // THE AGREEMENT IS "RENDERS CONTENT", NOT "IS NOT n/a" - the two came apart deliberately.
  //
  // `na` means "there is no number here", and two different states share it. A rig-limited value is
  // absent and shows nothing on either surface. A MEASURED FAILURE is also `na` (0 ok, N fail: there
  // is no latency to publish) but it is a result, and both surfaces render it in red with its
  // counts. The popup used to drop it with the genuine absences, which is how a cell whose every
  // metric was measured and failed printed "served, not measured on this cell" - a false claim about
  // a cell we measured thoroughly.
  //
  // So the invariant is that the two surfaces agree about which rows carry content, and a failed
  // row carries content on both.
  const rigLimited = app.chooserPerfCell(STORY_GW, "rps_max_proxy", String, CUSTOM);
  assert.equal(rigLimited.na, true, "the rig-limited field is absent");
  assert.ok(!rigLimited.failed, "and it is an absence, not a measured failure");
  assert.ok(!pop.includes("Max proxy RPS"), "so the popup omits it, exactly as the table shows nothing");

  const failed = app.chooserPerfCell(STORY_GW, "added_latency_p50_us", String, CUSTOM);
  assert.equal(failed.na, true, "a measured failure has no number");
  assert.equal(failed.failed, true, "but it IS a result, not an absence");
  assert.ok(pop.includes("Added latency p50"), "so the popup shows it, exactly as the table does");
  assert.ok(pop.includes(failed.text), `the popup carries the same counts the table shows (${failed.text})`);
  assert.ok(/failtext/.test(pop), "and marks it as a failure rather than printing it like a measurement");
});

test("a measured stream-sustain failure stays distinct from not-measured through metric() on both counts", () => {
  // The streaming shape of the same discipline, driven through the same seal the site uses.
  const failed = app.metric(sealMetric(0, { gated: true, zeroNote: ZERO_MEASURED_FAIL }), String);
  const unmeasured = app.metric(sealMetric(null, { gated: true }), String);
  assert.equal(failed.na, false);
  assert.equal(failed.v, 0);
  assert.equal(unmeasured.na, true);
  assert.equal(unmeasured.v, null);
  assert.notEqual(failed.text, unmeasured.text, "the two states must never render identically");
});
