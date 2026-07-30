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

/* A gateway whose openai->anthropic cell carries one field per story, so every surface can be read off the
   SAME record. Metrics seal through the REAL sealMetric, exactly as gen-data does.

   THE FOUR STORIES CHANGED, BECAUSE ONE OF THEM STOPPED EXISTING. They were: a measured zero, a
   below-resolution absence, a measured failure, and a RIG-LIMITED SUPPRESSION - a sealed envelope
   withholding a number it had, because our own rig might have bounded it. Suppression is gone: the
   measurement was correct in every one of those cells and only its INTERPRETATION was open, so the number
   is published with the fraction of the rig's ceiling it reached and the reader draws the conclusion.
   The fourth story is now the LOWER BOUND, which the frontier introduced: a rate whose sweep ran out of
   ladder while that concurrency was still qualifying. It is real, it is not a maximum, and a surface that
   renders it as a ceiling is making a claim the data does not support - the same class of error the
   suppression was, in the opposite direction. So the four remain four, and each is still one substitution
   away from being indistinguishable from its neighbour, which is why every surface is checked. */
const IN = "openai";
const OUT = "anthropic";
// The frontier as it arrives on a projected record: a sealed rate per reading, with the reading's own
// evidence beside it. Written out rather than built by a helper so each story is visible at its bound.
const STORY_FRONTIER = [
  // story A: SERVED, BUT NO RUNG HELD THIS TAIL. `below_resolution` on a throughput reading - a measured
  // nothing, not a missing measurement. On the field data this is five of six columns for the slowest
  // gateways, so it is the state most likely to be misread as "no data".
  { bound_ms: 1, concurrency: null, p99_us: null, first_disqualified_conc: null, lower_bound: false,
    rps: sealMetric(null, { absent: { reason: "below_resolution",
      detail: "every cleanly-served rung had a tail latency at or above 1ms, so this gateway carried no measurable throughput under that bound" } }) },
  // an ordinary reading, so the stories above and below have something to be distinct FROM.
  { bound_ms: 10, concurrency: 64, p99_us: 4200, first_disqualified_conc: 128, lower_bound: false,
    rps: sealMetric(20000) },
  // story D: A FLOOR, not a ceiling.
  { bound_ms: null, concurrency: 1024, p99_us: 40000, first_disqualified_conc: null, lower_bound: true,
    rps: sealMetric(22000) },
];
const STORY_CELL_PERF = {
  frontier: STORY_FRONTIER,
  // story B: a below-resolution absence on a LATENCY difference - the best result the comparison can
  // express. Note it is the SAME engine reason as story A and must render differently, because the
  // sentence differs: "smaller than we can measure" for a difference, "held nothing" for a rate.
  added_latency_p99_us: sealMetric(null, {
    absent: { reason: "below_resolution", detail: "the gateway leg (20073us) came in under the direct leg (21070us)" },
  }),
  // story C: a measured failure - digits prove the window ran; red says the gateway failed it.
  added_latency_p50_us: sealMetric(null, {
    absent: { reason: "not_measured", detail: "the gateway leg at c=1 was not clean: 0 ok, 14201 fail" },
  }),
  // A number taken NEAR THE RIG'S OWN CEILING, published with the comparison's facts on it. This is what
  // replaced the suppression, and it is here so the inverted property has something to hold.
  gateway_c1_p99_us: sealMetric(99999, { headroom: 0.996, ceiling: 100400 }),
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
const CUSTOM = { ...app.newState(), mode: "custom", xlateIn: IN, xlateOut: OUT };
const perfLane = app.LANES.find((l) => l.key === "perf");
const laneRow = (bound) => perfLane.metrics.find((m) => m.k === `frontier.${bound}`);

test("a MEASURED NOTHING is a 0 that says why, on every surface, and is never a missing measurement", () => {
  const table = app.frontierBoundCell(STORY_GW, 1, CUSTOM);
  assert.equal(table.na, false, "the gateway served and the sweep ran: this is a result, not a hole");
  assert.equal(table.text, "0", "the table shows the digit");
  assert.equal(table.v, 0, "it ranks as 0 - last on a higher-is-better sort, which is the honest place");
  assert.equal(table.why, "no rung held this tail", "and the cell says WHY, not just '0'");
  assert.match(app.metricTd(table), /no rung held this tail/, "on the cell itself, not only on hover");
  assert.match(table.note, /no measurable throughput under that bound/, "with the engine's own prose on the tooltip");

  const rec = app.laneRecord(perfLane, STORY_GW, CUSTOM);
  assert.ok(rec, "the drawer/compare lane reads the same cell");
  assert.equal(laneRow("1ms").cell(rec).v, 0, "drawer/compare rank the same 0");
  assert.equal(laneRow("1ms").cell(rec).why, "no rung held this tail", "and tell the same story");

  const pop = app.cellPopFull(STORY_GW, IN, OUT);
  assert.match(pop, /frontier-spark/, "the popup carries the cell's curve, where that 0 is a tick on the floor");
});

test("a BELOW-RESOLUTION absence reads as approximately-zero on every surface, never a bare n/a", () => {
  const table = app.chooserPerfCell(STORY_GW, "added_latency_p99_us", String, CUSTOM);
  assert.equal(table.na, false, "below-resolution is a display state, not a hole");
  assert.equal(table.text, "≈0", "the table reads approximately zero");
  assert.equal(table.v, 0, "it ranks as 0, equal-best on a lower-is-better sort");
  assert.match(table.note, /came in under the direct leg/, "the engine's evidence is the tooltip");
  // THE SAME REASON, A DIFFERENT SENTENCE. Story A above is `below_resolution` too, and it must NOT read
  // "≈0": "approximately zero" is the right words for a difference too small to weigh and the wrong words
  // for a gateway that held nothing under a tail.
  assert.notEqual(app.frontierBoundCell(STORY_GW, 1, CUSTOM).text, table.text,
    "one absence reason, two findings: they must not render identically");

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
  const untested = app.chooserPerfCell(STORY_GW, "added_latency_p50_us", String, { ...CUSTOM, xlateIn: "gemini", xlateOut: "cohere" });
  assert.equal(untested.text, "n/a");
  assert.ok(!untested.failed, "an untested cell must never wear the failure red");
});

/* THE INVERTED STORY. This test used to be "a RIG-LIMITED suppression is n/a on every surface and the raw
   number is unrecoverable from any of them": a measurement at or above a chosen fraction of our own rig's
   ceiling had its value replaced with null, and the test asserted that no surface could recover it.
   That was withholding a correct measurement. The engine reached the verdict by comparing the observation
   against a rig reference and applying a chosen fraction, and it fired hardest on the gateways that did
   best - a gateway keeping pace with the paced mock to within 0.7% published nothing at all. So the
   opposite property is the one that must hold now, and it is asserted in both directions: the number is
   PUBLISHED on every surface with the comparison's own facts attached, and the suppression vocabulary
   cannot come back (invariant C2 asserts the same thing from the bundle side). */
test("a NEAR-CEILING measurement is published on every surface, and suppression cannot come back", () => {
  const table = app.chooserPerfCell(STORY_GW, "gateway_c1_p99_us", String, CUSTOM);
  assert.equal(table.na, false, "a number near the rig's own ceiling is not a hole");
  assert.equal(table.v, 99999, "the table shows the number that was measured");
  assert.match(table.note, /of this rig's own ceiling/, "with the fraction of that ceiling it reached");
  assert.match(table.note, /100,400/, "and the ceiling it is a fraction of, so the claim is checkable");

  const rec = app.laneRecord(perfLane, STORY_GW, CUSTOM);
  assert.equal(app.mval(rec.gateway_c1_p99_us), 99999, "drawer/compare publish the same number");
  assert.equal(rec.gateway_c1_p99_us.headroom, 0.996, "the fraction travels with the record");
  // AND NO SURFACE WITHHOLDS IT. Every sealed envelope on the record is unsuppressed, which is the
  // structural half: there is no `suppressed: true` shape left for a value to hide behind.
  for (const [k, v] of Object.entries(rec))
    if (app.isEnvelope(v)) assert.equal(v.suppressed, false, `${k} must not be suppressed`);
  for (const r of rec.frontier) assert.equal(r.rps.suppressed, false, "nor may any frontier reading's rate be");
});

test("a LOWER BOUND is a floor on every surface, never rendered as a ceiling", () => {
  const table = app.frontierBoundCell(STORY_GW, null, CUSTOM);
  assert.equal(table.v, 22000, "the rate is real and is published");
  assert.equal(table.text, "≥ 22,000", "and it is a FLOOR: the sweep ran out of ladder, so this is not a maximum");
  assert.match(table.note, /FLOOR/, "the tooltip says what the glyph means");
  assert.match(table.note, /did not look higher/, "and why it is not a maximum");

  const rec = app.laneRecord(perfLane, STORY_GW, CUSTOM);
  assert.equal(laneRow("unbounded").cell(rec).text, "≥ 22,000", "the drawer/compare row agrees, glyph and all");

  // The ordinary reading beside it carries NO floor glyph, or the mark would mean nothing.
  assert.equal(app.frontierBoundCell(STORY_GW, 10, CUSTOM).text, "20,000");
  // And the curve marks it apart: an open dot for a floor, a filled dot for an established ceiling.
  const spark = app.frontierSpark(STORY_FRONTIER, { min: 20000, max: 22000, boundMs: 10 });
  assert.match(spark, /fill="none"/, "the floor reading is drawn open, not as a proven peak");
  assert.match(spark, /or more/, "and says so to a screen reader");
});

test("an UNTESTED cell is empty on every surface: no row, no record, no popup, no fabricated value", () => {
  const missing = { ...CUSTOM, xlateIn: "gemini", xlateOut: "cohere" };
  assert.equal(app.chooserHasCell(STORY_GW, missing), false, "the chooser knows there is no cell");
  assert.equal(app.frontierChooserCell(STORY_GW, missing).na, true, "the table reads n/a");
  assert.equal(app.laneRecord(perfLane, STORY_GW, missing), null, "the drawer/compare lane has no record");
  assert.equal(app.cellPopFull(STORY_GW, "gemini", "cohere"), "", "the popup renders nothing for a cell that does not exist");
});

test("the popup reads the SAME chosen-cell values the table reads, at the SAME bound", () => {
  // The agreement stated directly: every non-n/a perf value the table would show for this cell appears
  // verbatim in the popup, and nothing else does.
  const pop = app.cellPopFull(STORY_GW, IN, OUT);
  for (const [key, label] of [
    ["added_latency_p99_us", "Added latency p99"],
    ["added_latency_p50_us", "Added latency p50"],
  ]) {
    assert.ok(pop.includes(label), `the popup shows ${label}`);
    assert.ok(app.chooserPerfCell(STORY_GW, key, String, CUSTOM), `${key} is read by the same accessor`);
  }
  /* THE THROUGHPUT ROW IS LABELLED WITH ITS BOUND, AND FOLLOWS THE SELECTOR. This replaces the old
     "the popup shows Sustained @20ms" check, whose column is deleted - and it is a strictly stronger
     property, because a bound selector creates a way for two surfaces to disagree that a fixed column
     could not: the popup could keep showing 10 ms after the reader switched to 1 ms. Both read
     selectedBound() through the same accessors, so both move together or neither does. */
  const prev = app.state.bound;
  try {
    for (const bound of app.BOUND_CHOICES) {
      app.state.bound = bound;
      const h = app.cellPopFull(STORY_GW, IN, OUT);
      assert.ok(h.includes(app.boundColLabel(bound)), `the popup names the ${app.boundLabel(bound)} bound it read`);
      const cell = app.frontierChooserCell(STORY_GW, { ...CUSTOM, bound });
      if (!cell.na)
        assert.ok(h.includes(cell.text), `the popup carries the table's own text at ${app.boundLabel(bound)} (${cell.text})`);
    }
  } finally { app.state.bound = prev; }
  // THE AGREEMENT IS "RENDERS CONTENT", NOT "IS NOT n/a" - the two came apart deliberately.
  //
  // `na` means "there is no number here", and two different states share it. A never-measured value is
  // absent and shows nothing on either surface. A MEASURED FAILURE is also `na` (0 ok, N fail: there is no
  // latency to publish) but it is a result, and both surfaces render it in red with its counts. The popup
  // used to drop it with the genuine absences, which is how a cell whose every metric was measured and
  // failed printed "served, not measured on this cell" - a false claim about a cell we measured thoroughly.
  const failed = app.chooserPerfCell(STORY_GW, "added_latency_p50_us", String, CUSTOM);
  assert.equal(failed.na, true, "a measured failure has no number");
  assert.equal(failed.failed, true, "but it IS a result, not an absence");
  assert.ok(pop.includes(failed.text), `the popup carries the same counts the table shows (${failed.text})`);
  assert.ok(/failtext/.test(pop), "and marks it as a failure rather than printing it like a measurement");
});

test("a measured stream-sustain failure stays distinct from not-measured through metric() on both counts", () => {
  // The streaming shape of the same discipline, driven through the same seal the site uses.
  const failed = app.metric(sealMetric(0, { zeroNote: ZERO_MEASURED_FAIL }), String);
  const unmeasured = app.metric(sealMetric(null), String);
  assert.equal(failed.na, false);
  assert.equal(failed.v, 0);
  assert.equal(unmeasured.na, true);
  assert.equal(unmeasured.v, null);
  assert.notEqual(failed.text, unmeasured.text, "the two states must never render identically");
});

// ---- family 2b: ...and "every surface" now includes the two that had no test at all ---------------
//
// The family above reads the drawer and compare lanes through `laneRecord`, which is the RECORD those
// surfaces consume - not the markup they emit. That was not a shortcut, it was the only thing reachable:
// drawerHtml() was called by no test in either file, and renderCompare() reached for
// document.getElementById on its first useful line in a suite with no DOM. So "one story on every
// surface" was in fact asserted on every surface except the two a reader actually opens.
//
// With the compare panel's row-building extracted (compareBodyHtml) and the drawer threaded through its
// state, both render as pure functions of (gateways, state). The same four stories are now asserted on
// the markup itself, which is where a divergence would be visible to a reader and nowhere else.
const OTHER_GW = {
  key: "othergw", display: "Other GW", lang: "Go",
  matrix: { upstreams: { [OUT]: { cells: { [IN]: { served: true, status: 200, perf: {
    frontier: [
      { bound_ms: 1, concurrency: 32, p99_us: 400, first_disqualified_conc: 64, lower_bound: false, rps: sealMetric(1100) },
      { bound_ms: 10, concurrency: 64, p99_us: 4000, first_disqualified_conc: 128, lower_bound: false, rps: sealMetric(1200) },
      { bound_ms: null, concurrency: 128, p99_us: 40000, first_disqualified_conc: null, lower_bound: false, rps: sealMetric(1500) },
    ],
    added_latency_p99_us: sealMetric(9000),
    added_latency_p50_us: sealMetric(4000),
  } } } } } },
};
const STORY_STATE = { ...app.newState(), view: "performance", mode: "custom", xlateIn: IN, xlateOut: OUT,
  data: { gateways: [STORY_GW, OTHER_GW] }, cmp: [STORY_GW.key, OTHER_GW.key] };

test("the DRAWER markup tells all four stories - a measured nothing, below-resolution, a failure, a floor", () => {
  const h = app.drawerHtml(STORY_GW, STORY_STATE);
  // A: the measured nothing is a digit with its reason beside it, never a hole and never a blank.
  assert.match(h, /99% under 1 ms/, "the drawer names the reading's own bound");
  assert.match(h, /no rung held this tail/, "and says what its 0 means");
  // B: below-resolution is the ≈0 result, not an absence.
  assert.match(h, /≈0/, "below-resolution reads as approximately zero in the drawer");
  // C: the measured failure keeps its counts and its red class - the clause that survives the na filter.
  assert.match(h, /failed · 0\/14,201/, "the measured failure keeps its counts in the drawer");
  assert.match(h, /class="failtext"/, "and is marked as a failure");
  // D: the floor is a floor. A bare 22,000 here would publish our own range as the gateway's answer.
  assert.match(h, /≥ 22,000/, "a lower-bound reading is rendered as a floor in the drawer");
  // ...and the near-ceiling number is PUBLISHED, which is what replaced the suppression this asserted.
  assert.match(h, /99,999/, "a measurement near the rig's ceiling is published, not withheld");
  // (the prose is HTML-escaped into the title attribute, so the match avoids its apostrophe)
  assert.match(h, /own ceiling/, "with the fraction of that ceiling it reached");
});

test("the COMPARE markup tells all four stories, and the curves sit side by side", () => {
  const h = app.compareBodyHtml([STORY_GW, OTHER_GW], STORY_STATE);
  assert.match(h, /Story GW/); assert.match(h, /Other GW/);
  assert.match(h, /≈0/, "below-resolution reads as approximately zero in compare too");
  assert.match(h, /failed · 0\/14,201/, "the measured failure carries its counts into compare");
  assert.match(h, /class="na failcell"/, "marked as a failure, not folded in with the untested cells");
  assert.match(h, /≥ 22,000/, "and a floor stays a floor when set beside another gateway's ceiling");
  assert.match(h, /no rung held this tail/, "the measured nothing keeps its reason here as well");
  // EVERY BOUND IS A ROW, so three gateways' whole curves are comparable as digits...
  for (const bound of app.BOUND_CHOICES) assert.ok(h.includes(app.boundColLabel(bound)), `${app.boundLabel(bound)} has a row`);
  // ...and as shapes, which is the comparison a single scalar made impossible.
  assert.match(h, /Curve across bounds/, "the curves get a row of their own");
  assert.equal((h.match(/frontier-spark/g) || []).length, 2, "one curve per gateway being compared");
  assert.match(h, /class="best"/, "and a winner is called where there is a contest");
});
