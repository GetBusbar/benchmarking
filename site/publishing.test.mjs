// SPDX-License-Identifier: Apache-2.0
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { gunzipSync } from "node:zlib";
import { parseHTML } from "linkedom/worker";
import { metricRecord, compareMeasurements, projectMeasurements, measurementsCsv } from "./public-data.mjs";
import { renderDashboard, htmlToMarkdown } from "./render.mjs";
import { fileAssets } from "./serve.mjs";
import worker from "./worker.mjs";

function measured(id, overrides = {}) {
  return {
    gateway_id: id, measured_at: "2026-08-01T00:00:00Z", status: { state: "served" },
    test: { ingress_protocol: "openai", upstream_protocol: "openai", hardware: "same hardware and pinned cores", architecture: "arm64", harness_commit: "abc", harness_dirty: false, mock_sha256: "def", qualification: { outcome: "pass" } },
    metrics: { added_latency_p99_us: { value: id === "a" ? 10 : 20, state: "valid", definition: "p99 gateway minus mock at concurrency one", section: "perf" } },
    provenance: { raw_result_url: "https://onthebench.ai/raw" }, ...overrides,
  };
}
test("API never turns below-resolution or absent values into exact zeroes", () => {
  assert.equal(metricRecord({ value: 0, certified: false, reason: "below_resolution" }).value, null);
  assert.equal(metricRecord({ value: 0, certified: true }).value, 0);
  assert.equal(metricRecord({ value: 999, certified: true, suppressed: true }).value, null);
  assert.equal(metricRecord({ value: null, certified: false, reason: "untestable" }).state, "untestable");
});
test("ratios require positive measured denominators and affirmative comparable evidence", () => {
  const a = measured("a"), b = measured("b");
  assert.equal(compareMeasurements([a,b], { a: "a", b: "b" }).results.gateway_b_over_a, 2);
  for (const field of ["hardware", "architecture", "harness_commit", "mock_sha256"]) {
    const altered = structuredClone(b); altered.test[field] = "different";
    const c = compareMeasurements([a,altered], { a: "a", b: "b" });
    assert.equal(c.comparability.status, "incomparable"); assert.equal(c.results.gateway_b_over_a, null);
    a.test[field] = null; altered.test[field] = null;
    assert.equal(compareMeasurements([a,altered], { a: "a", b: "b" }).comparability.status, "insufficient_evidence");
    a.test[field] = measured("a").test[field];
  }
  for (const state of ["below_resolution", "not_measured", "untestable"]) {
    const bad = measured("a"); bad.metrics.added_latency_p99_us = { value: 0, state };
    assert.equal(compareMeasurements([bad,b], { a: "a", b: "b" }).results.gateway_b_over_a, null);
  }
  a.metrics.added_latency_p99_us.value = 0;
  assert.equal(compareMeasurements([a,b], { a: "a", b: "b" }).results.gateway_b_over_a, null);
});
test("scoped cells retain their own run evidence instead of borrowing the base run", () => {
  const base = { id: "old", snapshot: { rig: { engine: { commit: "old", dirty: false } } } };
  const scoped = { id: "new", sha256: "newhash", snapshot: { rig: { engine: { commit: "new", dirty: false } } } };
  const g = { key: "a", snapshot_file: "old.json", matrix: { measured_at: "old-date", upstreams: { openai: { cells: { openai: { served: true, __run: { file: "new.json", measured_at: "new-date", build: "new-build" } } } } } } };
  const [m] = projectMeasurements(g, new Map([["old.json", base], ["new.json", scoped]]));
  assert.equal(m.run_id, "new"); assert.equal(m.measured_at, "new-date"); assert.equal(m.test.harness_commit, "new");
});
test("throughput comparisons preserve bounds, concurrency, and lower-bound uncertainty", () => {
  const a = measured("a"), b = measured("b");
  for (const m of [a,b]) {
    m.test.definitions = { "perf.frontier": "maximum clean observed rate under the named tail bound" };
    m.frontier = [{ bound_ms: 10, concurrency: 32, lower_bound: false, rps: { value: m.gateway_id === "a" ? 100 : 200, state: "valid" } }];
  }
  const query = { a: "a", b: "b", metric: "throughput_rps", bound: "10" };
  assert.equal(compareMeasurements([a,b], query).results.gateway_b_over_a, 2);
  b.frontier[0].concurrency = 64;
  assert.equal(compareMeasurements([a,b], query).comparability.status, "incomparable");
  b.frontier[0].concurrency = 32; b.frontier[0].lower_bound = true;
  assert.equal(compareMeasurements([a,b], query).results.gateway_b_over_a, null);
  assert.equal(compareMeasurements([a,b], { ...query, bound: "5" }).results.gateway_b_over_a, null);
});
test("CSV escapes spreadsheet formulas and quotes, without replacing null with zero", () => {
  const m = measured("=evil"); m.frontier = []; m.metrics.added_latency_p99_us.value = null;
  const csv = measurementsCsv([m]);
  assert.match(csv, /"'=evil"/); assert.match(csv, /"added_latency_p99_us","","valid"/);
});

const artifact = process.env.OTB_TEST_ARTIFACT;
test("published HTTP interface works without JavaScript", { skip: !artifact }, async () => {
  const env = { ASSETS: fileAssets(artifact) };
  const call = (path, options) => worker.fetch(new Request("https://onthebench.ai" + path, options), env);
  const pub = await (await call("/api/v1/benchmarks/latest")).json();
  const data = JSON.parse(readFileSync(artifact + "/data.json"));
  const first = pub.gateways[0].id;
  for (const path of ["/gateways", "/gateways/performance", "/gateways/matrix", `/gateways/${first}`]) {
    const r = await call(path); assert.equal(r.status, 200, path);
    const html = await r.text(); const doc = parseHTML(html).document;
    assert.ok(doc.querySelectorAll("tbody tr").length > 0, `${path} has actual rows`);
    assert.ok(!html.includes("Protocol matrix results are coming soon"));
    assert.equal(doc.querySelector('link[rel="canonical"]').getAttribute("href"), "https://onthebench.ai" + path);
  }
  for (const [path, type] of [["/llms.txt","text/plain"],["/sitemap.xml","application/xml"],["/gateways/performance.md","text/markdown"],["/gateways/data.csv","text/csv"],["/openapi.json","application/json"]]) {
    const r = await call(path); assert.equal(r.status, 200); assert.ok(r.headers.get("Content-Type").includes(type));
  }
  for (const path of ["/does-not-exist", "/api/v1/gateways/does-not-exist", "/_publication/records.json", "/%5fpublication/template.html"]) assert.equal((await call(path)).status, 404, path);
  for (const query of ["limit=0", "offset=-1", "gateway=", "ingress=imaginary", "limit=1&limit=2", "garbage=x"]) assert.equal((await call("/api/v1/measurements?" + query)).status, 400, query);
  const r = await call(`/api/v1/measurements?gateway=${first}&limit=1`);
  assert.equal(r.headers.get("Access-Control-Allow-Origin"), "*");
  const paged = await r.json(); assert.equal(paged.measurements.length, 1); assert.ok(paged.next);
  assert.equal((await call(`/api/v1/measurements?gateway=${first}&limit=1`, { headers: { "If-None-Match": r.headers.get("ETag") } })).status, 304);
  assert.equal((await call("/api/v1", { method: "POST" })).status, 405);
  const www = await worker.fetch(new Request("https://www.onthebench.ai/gateways?mode=same"), env);
  assert.equal(www.status, 308);
  assert.equal(www.headers.get("Location"), "https://onthebench.ai/gateways?mode=same");
  const rawUrl = pub.measurements.find(m => m.provenance.raw_result_url).provenance.raw_result_url;
  const raw = await worker.fetch(new Request(rawUrl), env);
  const bytes = gunzipSync(Buffer.from(await raw.arrayBuffer()));
  const expected = pub.measurements.find(m => m.provenance.raw_result_url === rawUrl).provenance.raw_sha256;
  assert.equal(createHash("sha256").update(bytes).digest("hex"), expected);
  assert.ok(JSON.parse(bytes).matrix);
  // Both path and query select the actual rendered facts, not just a title.
  const md = await (await call("/gateways/performance.md?mode=same&d=anthropic")).text();
  assert.match(md, /Anthropic.*Anthropic/);
  assert.ok(!md.includes("Protocol support matrix (6x6)"));
  const pair = await call(`/gateways/compare/${first}-vs-${pub.gateways[1].id}.md`);
  assert.equal(pair.status, 200); assert.match(await pair.text(), /Comparability:/);
  const comparison = await (await call(`/api/v1/compare?a=${first}&b=${pub.gateways[1].id}`)).json();
  assert.equal(comparison.comparison.gateway_a, first);
  assert.ok((await (await call("/sitemap.xml")).text()).includes(`/gateways/compare/${first}-vs-${pub.gateways[1].id}`));
  const template = readFileSync(new URL("./index.html", import.meta.url), "utf8");
  const html = renderDashboard(template, data, "/gateways/performance", pub);
  const doc = parseHTML(html).document;
  assert.ok(doc.querySelectorAll("#results-table tbody tr").length === data.gateways.length);
  assert.ok(!doc.querySelector("[data-wired]"), "SSR must not prevent client event wiring");
  assert.match(htmlToMarkdown(html), /req\/s|Throughput|throughput/);
});
