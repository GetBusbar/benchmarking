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
  console.warn("warn - raw results are mid-refresh (freshness guard tripped); testing against the committed site/data.json");
  data = JSON.parse(readFileSync(join(HERE, "data.json"), "utf8"));
} finally {
  rmSync(out, { recursive: true, force: true });
}

test("gen-data emits gateways with a class for every entry", () => {
  assert.ok(data.gateways.length >= 10, `expected a full field, got ${data.gateways.length}`);
  for (const g of data.gateways) {
    assert.ok(typeof g.cls === "string" && g.cls.length > 0, `${g.key} has no cls`);
  }
  const busbar = data.gateways.find((g) => g.key === "busbar");
  assert.equal(busbar.cls, "Control plane");
});

test("star snapshot covers the field and gen-data attaches it", () => {
  // The committed snapshot (gateways/stars.json, refreshed by gateways/fetch-stars.mjs)
  // must cover every gateway with an integer count and an ISO date.
  const snap = JSON.parse(readFileSync(join(ROOT, "gateways", "stars.json"), "utf8"));
  for (const g of data.gateways) {
    const s = snap[g.key];
    assert.ok(s, `${g.key} missing from gateways/stars.json`);
    assert.ok(Number.isInteger(s.stars) && s.stars >= 0, `${g.key} stars not an integer`);
    assert.ok(/^\d{4}-\d{2}-\d{2}$/.test(s.as_of), `${g.key} as_of not YYYY-MM-DD`);
  }
  // A bundle emitted by the CURRENT gen-data carries the attached fields. The committed
  // fallback bundle (mid-refresh) may predate them; assert only when present.
  for (const g of data.gateways) {
    if ("stars" in g) {
      assert.equal(g.stars, snap[g.key].stars, `${g.key} bundle stars != snapshot`);
      assert.equal(g.stars_as_of, snap[g.key].as_of, `${g.key} bundle as_of != snapshot`);
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
  assert.ok(streaming.every((g) => g.stream && g.stream.stream_served));
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
  const st = app.decodeUrl("/gateways/passthrough", "?cls=Control%20plane&lang=Rust&q=bus");
  assert.equal(st.view, "passthrough");
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
  st.cmp = ["busbar", "bifrost"];
  st.cmpOpen = true;
  st.drawer = "busbar";
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
  // Passthrough is a real tab at its own path now, no longer the landing view.
  assert.equal(app.decodeUrl("/gateways/passthrough", "").view, "passthrough");
  const st = app.newState();
  st.view = "passthrough";
  assert.equal(app.encodeUrl(st), "/gateways/passthrough");
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
  // legacy view aliases still resolve onto live tabs (the old default stays reachable)
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
  // and re-encoding a legacy state yields the clean path form
  assert.ok(app.encodeUrl(st).startsWith("/gateways/matrix?"));
});

test("decode rejects a bogus sort column", () => {
  const back = app.decodeUrl("/gateways", "?sort=evil&dir=asc");
  assert.equal(back.sortCol, "rps20");
});

test("a direct URL load defaults each tab to its column's natural direction", () => {
  // Passthrough / Translation headline on Sustained RPS -> descending (higher is better)
  const pass = app.decodeUrl("/gateways/passthrough", "");
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

// ---- gateways overview: the neutral roster ----------------------------------
test("gateways overview lists every gateway alphabetically, busbar treated like the rest", () => {
  const rows = app.rosterRows(data.gateways);
  assert.equal(rows.length, data.gateways.length, "no gateway filtered out of the roster");
  const names = rows.map((g) => g.display.toLowerCase());
  assert.deepEqual(names, names.slice().sort(), "roster is alphabetical, case-insensitive");
  assert.ok(rows.some((g) => g.key === "busbar"), "busbar is a plain roster row like the others");
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

test("Gateways leads the tab order and is not a perf view", () => {
  assert.deepEqual(app.VIEWS, ["gateways", "passthrough", "translation", "streaming", "matrix", "method"]);
  assert.equal(app.VIEW_LABELS.gateways, "Gateways");
  // the overview is a roster section, not a ranked perf table
  assert.ok(!app.PERF_VIEWS.has("gateways"));
  assert.ok(!(app.VIEW_SORT && "gateways" in app.VIEW_SORT));
  // the perf tabs are pure measurement: no implementation-language column anywhere.
  // Language lives only on the Gateways overview roster.
  for (const [view, cols] of Object.entries(app.COLUMN_SETS)) {
    assert.ok(!cols.some((c) => c.id === "lang"), `${view} still carries a lang column`);
  }
  // the measurement-fact pill (Tested on) stays on Passthrough
  assert.ok(app.COLUMN_SETS.passthrough.some((c) => c.id === "tested"));
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

test("Streaming tab keeps measured streaming refusals as visible rows", () => {
  // Principle 3: filtering a competitor out reads as hiding it. A stream_served:false gateway
  // (Portkey's measured refusal) must stay in the Streaming row set; its null metrics sink it to
  // the bottom as a muted row, and naText labels it "did not stream" with the evidence.
  const st = app.newState();
  st.view = "streaming";
  const streams = { display: "s", key: "s", lang: "Go", stream: { stream_served: true, stream_added_ttft_p99_us: 1 } };
  const refused = { display: "r", key: "r", lang: "Node",
    stream: { stream_served: false, stream_error: "no SSE frames on stream:true" } };
  const rows = app.applyFilters([streams, refused], st);
  assert.deepEqual(rows.map((g) => g.key).sort(), ["r", "s"], "refusal row is not filtered out");
  const na = app.naText(refused.stream, "stream_served", "stream_error");
  assert.equal(na.text, "did not stream");
  assert.equal(na.note, "no SSE frames on stream:true");
  // the real field: any committed stream_served:false gateway gets the same label
  for (const g of data.gateways) {
    if (g.stream && g.stream.stream_served === false) {
      assert.equal(app.naText(g.stream, "stream_served", "stream_error").text, "did not stream", g.key);
    }
  }
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

test("guard treats max-proxy=0 as a DISTINCT did-not-qualify warning, never noise", () => {
  // arch's real shape: sustained 18, max 0. The ceiling run failed to qualify at every tested
  // load; filing that under "small inversion is sweep noise" would misdescribe a real failure.
  const g = { key: "zeromax", display: "ZeroMax", lang: "Rust",
    best_cell: { dialect: "openai", source: "matrix",
      added_latency_p99_us: 100, rps_sustained_20ms: 18, rps_max_proxy: 0 } };
  const { errors, warnings } = checkConsistency({ gateways: [g] }, app);
  assert.deepEqual(errors, []);
  assert.equal(warnings.length, 1);
  assert.ok(warnings[0].includes("did not qualify"), warnings[0]);
  assert.ok(warnings[0].includes("not noise"), warnings[0]);
});

test("a zero RPS cell renders 0 with the no-qualifying-ceiling tooltip", () => {
  const zero = { best_cell: { dialect: "openai", source: "matrix",
    rps_sustained_20ms: 18, rps_max_proxy: 0 } };
  const cols = app.COLUMN_SETS.passthrough;
  const rpsmax = cols.find((c) => c.id === "rpsmax").get(zero);
  assert.equal(rpsmax.text, "0");
  assert.equal(rpsmax.na, false);
  assert.ok(/no tested load held p99 < 1 s/.test(rpsmax.note), "tooltip explains the 0");
  // a non-zero cell carries no note
  const rps20 = cols.find((c) => c.id === "rps20").get(zero);
  assert.equal(rps20.text, "18");
  assert.ok(!rps20.note);
});

test("guard warns on a served matrix cell with no per-cell perf", () => {
  const g = { key: "unswept", display: "Unswept", lang: "Go",
    matrix: mkMatrix({ anthropic: { openai: { served: true } } }) };
  const { errors, warnings } = checkConsistency({ gateways: [g] }, app);
  assert.deepEqual(errors, []);
  assert.equal(warnings.length, 1);
  assert.ok(warnings[0].includes("no per-cell perf"), warnings[0]);
  assert.ok(warnings[0].includes("openai->anthropic"), warnings[0]);
});

test("default translation pair has no silent all-n/a served row", () => {
  // The Translation tab's default pair (openai -> anthropic): any gateway that serves it should
  // have per-cell perf, or its row is a table of n/a cells. A gateway whose per-cell sweep never
  // ran AT ALL (mid re-run, e.g. a best_cell synthesized as perf-fallback) is known-pending and
  // covered by the guard's no-per-cell-perf WARNING above; anything else all-n/a is a bug.
  const st = app.newState();
  st.view = "translation"; // pinned pair defaults to openai -> anthropic
  const KEYS = ["added_latency_p50_us", "added_latency_p99_us", "rps_sustained_20ms", "rps_max_proxy"];
  for (const g of app.applyFilters(data.gateways, st)) {
    const sweptAny = Object.values(g.matrix.upstreams || {}).some((u) =>
      Object.values(u.cells || {}).some((c) => c && c.served === true && c.perf));
    if (!sweptAny) {
      // no per-cell sweep for this gateway at all: must at least trip the coverage warning
      const { warnings } = checkConsistency({ gateways: [g] }, app);
      assert.ok(warnings.some((w) => w.includes("no per-cell perf")), `${g.key}: unswept but unflagged`);
      continue;
    }
    const anyVal = KEYS.some((k) => app.xlateCell(g, k, String).v != null);
    assert.ok(anyVal, `${g.key} serves the default translation pair but every metric is n/a`);
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
  const unsupported = data.gateways.find((g) =>
    g.governed && g.governed.governed_served === false && /manifest defines no/.test(g.governed.governed_note || ""));
  assert.ok(unsupported, "expected a gateway without native governance");
  // "manifest defines no <hook>" = the harness never probed it: "not tested", never a capability verdict
  assert.equal(app.naText(unsupported.governed, "governed_served", "governed_note").text, "not tested");
});

test("streaming latency cells annotate >=1ms values with their ms equivalent", () => {
  const cols = app.COLUMN_SETS.streaming;
  const sttft = cols.find((c) => c.id === "sttft");
  const big = { stream: { stream_served: true, stream_added_ttft_p99_us: 596693 } };
  assert.equal(sttft.get(big).text, "596,693 (596.7 ms)");
  const small = { stream: { stream_served: true, stream_added_ttft_p99_us: 397 } };
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
  const best = { ingress: "openai", egress: "openai", rps_sustained_20ms: 30000 };
  const green = { served: true, perf: { rps_sustained_20ms: 25500, added_latency_p99_us: 900 } };
  const tip = app.cellPerfTip(green, "anthropic", "openai", best);
  assert.ok(tip.includes("25,500 req/s @20ms"), tip);
  assert.ok(tip.includes("+900 µs p99 added"), tip);
  assert.ok(tip.includes("-15.0% req/s vs the OpenAI→OpenAI cell"), tip); // human labels, not raw dialect keys
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
