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
  for (const path of ["/", "/gateways", "/gateways/performance", "/gateways/matrix", `/gateways/${first}`]) {
    const r = await call(path); assert.equal(r.status, 200, path);
    const html = await r.text(); const doc = parseHTML(html).document;
    assert.ok(doc.querySelectorAll("tbody tr").length > 0 || (data.active_field && /Refresh in progress|not measured|Unfinished entrants/.test(html)), `${path} has rows or explicit pending state`);
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
  const paged = await r.json(); assert.equal(paged.measurements.length, Math.min(1, pub.measurements.filter(m => m.gateway_id === first).length));
  if (pub.measurements.filter(m => m.gateway_id === first).length > 1) assert.ok(paged.next);
  assert.equal((await call(`/api/v1/measurements?gateway=${first}&limit=1`, { headers: { "If-None-Match": r.headers.get("ETag") } })).status, 304);
  assert.equal((await call("/api/v1", { method: "POST" })).status, 405);
  const www = await worker.fetch(new Request("https://www.onthebench.ai/gateways?mode=same"), env);
  assert.equal(www.status, 308);
  assert.equal(www.headers.get("Location"), "https://onthebench.ai/gateways?mode=same");
  if (pub.measurements.length) {
  const rawUrl = pub.measurements.find(m => m.provenance.raw_result_url).provenance.raw_result_url;
  const raw = await worker.fetch(new Request(rawUrl), env);
  const bytes = gunzipSync(Buffer.from(await raw.arrayBuffer()));
  const expected = pub.measurements.find(m => m.provenance.raw_result_url === rawUrl).provenance.raw_sha256;
  assert.equal(createHash("sha256").update(bytes).digest("hex"), expected);
  assert.ok(JSON.parse(bytes).matrix);
  }
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

test("agent contract: bounded retrieval, schemas, freshness and provenance", { skip: !artifact }, async () => {
  const { default: Ajv } = await import("ajv/dist/2020.js");
  const env = { ASSETS: fileAssets(artifact) };
  const call = path => worker.fetch(new Request("https://onthebench.ai" + path), env);
  const spec = await (await call("/openapi.json")).json();
  const ajv = new Ajv({ strict: false, allErrors: true });
  ajv.addSchema({ $id: "otb", components: spec.components });
  function validate(name, value) {
    const check = ajv.compile({ $ref: `otb#/components/schemas/${name}` });
    assert.ok(check(value), `${name}: ${JSON.stringify(check.errors)?.slice(0,2500)}`);
  }
  const pub = await (await call("/api/v1/benchmarks/latest")).json(); validate("LatestCollection", pub);
  const first = pub.gateways[0].id, second = pub.gateways[1].id;
  for (const [path, schema] of [["/api/v1", "Discovery"], ["/api/v1/gateways", "Catalog"], [`/api/v1/gateways/${first}`, "GatewayEvidence"], ["/api/v1/methodology", "Methodology"], ["/api/v1/schema", "SchemaIndex"], ["/api/v1/measurements?limit=2", "MeasurementPage"], ["/api/v1/measurements?limit=2&compact=1", "MeasurementPage"], ["/api/v1/definitions", "Definitions"], ["/api/v1/runs?limit=2", "RunPage"], [`/api/v1/benchmarks/${pub.dataset_id}`, "BenchmarkResource"], [`/api/v1/benchmarks/${(pub.measurements.find(m => m.run_id)?.run_id || (await (await call("/api/v1/runs?limit=1")).json()).runs[0].id)}`, "BenchmarkResource"]]) validate(schema, await (await call(path)).json());
  const body = await (await call("/api/v1/summary")).text();
  assert.ok(Buffer.byteLength(body) < 64 * 1024, "summary fits a small retrieval context");
  const summary = JSON.parse(body); validate("Summary", summary);
  assert.equal(summary.results.length, pub.gateways.length);
  for (const row of summary.results) {
    const source = pub.measurements.find(m => m.gateway_id === row.gateway_id && m.test.ingress_protocol === "openai" && m.test.upstream_protocol === "openai");
    if (source) {
      assert.deepEqual(row.provenance, source.provenance);
      assert.equal(row.measured_at, source.measured_at);
      if (source.metrics.added_latency_p99_us) { const metric = row.metrics.added_latency_p99_us; assert.equal(metric.value, source.metrics.added_latency_p99_us.value); assert.equal(metric.state, source.metrics.added_latency_p99_us.state); assert.equal(summary.definitions[metric.definition_id] ?? null, source.metrics.added_latency_p99_us.definition); }
    }
    assert.equal(row.age_as_of, pub.generated_at);
    assert.equal(row.published_at, pub.published_at, "release date agrees with collection metadata");
    const gateway = await (await call(`/api/v1/gateways/${row.gateway_id}`)).json();
    assert.equal(row.current_metadata.stars, gateway.gateway.stars);
    assert.equal(row.current_metadata.stars_as_of, gateway.gateway.stars_as_of);
  }
  for (const suffix of ["", "&ingress=openai&egress=openai", "&metric=throughput_rps&bound=none"]) {
    const c = await (await call(`/api/v1/compare?a=${first}&b=${second}${suffix}`)).json(); validate("Comparison", c);
    for (const side of [c.results.gateway_a,c.results.gateway_b].filter(Boolean)) {
      assert.deepEqual(side, pub.measurements.find(m => m.measurement_id === side.measurement_id));
      assert.equal(side.age_as_of, pub.generated_at);
    }
    if (c.comparability.status !== "comparable") assert.equal(c.results.gateway_b_over_a, null);
  }
  const filtered = await (await call(`/api/v1/summary?gateway=${first}&ingress=anthropic&egress=openai`)).json();
  validate("Summary", filtered); assert.equal(filtered.results.length, 1);
  for (const query of ["gateway=", "ingress=invalid", "offset=1", "gateway=x&gateway=y"]) {
    const r = await call(`/api/v1/summary?${query}`); assert.equal(r.status, 400); validate("Error", await r.json());
  }
  assert.equal((await call("/api/v1/summary?gateway=missing-gateway")).status, 404);
  assert.match(await (await call("/robots.txt")).text(), /User-agent: \*\nAllow: \//);
  const sitemap = await (await call("/sitemap.xml")).text();
  for (const path of ["/gateways", "/gateways/data", "/gateways/method", `/gateways/${first}`]) assert.ok(sitemap.includes(`<loc>https://onthebench.ai${path}</loc>`));
  for (const path of ["/", "/gateways/method", "/gateways/data", `/gateways/${first}`]) {
    const doc = parseHTML(await (await call(path)).text()).document;
    assert.ok(doc.querySelector('link[rel="canonical"]'));
    const metadata = JSON.parse(doc.querySelector('script[type="application/ld+json"][data-publication]').textContent);
    assert.ok(["Dataset", "DataCatalog"].includes(metadata["@type"]));
    assert.ok(!JSON.stringify(metadata).includes("aggregateRating"));
  }
});

test("OpenAPI generates a usable typed client with nullable metrics", async () => {
  const { default: openapiTS, astToString } = await import("openapi-typescript");
  const { openapi } = await import("./api-contract.mjs");
  const { mkdtempSync, writeFileSync, rmSync, mkdirSync } = await import("node:fs");
  const { join } = await import("node:path");
  const { execFileSync } = await import("node:child_process");
  const base = process.env.OTB_BUILD_ROOT || (process.platform === "darwin" ? "/Users/matthew/Developer/tmp" : process.env.TMPDIR || "/tmp");
  mkdirSync(base, { recursive: true });
  const dir = mkdtempSync(join(base, "otb-typed-client-"));
  try {
    writeFileSync(join(dir, "api.d.ts"), astToString(await openapiTS(openapi())));
    writeFileSync(join(dir, "client.ts"), `import type { paths } from "./api";
      type Summary = paths["/api/v1/summary"]["get"]["responses"][200]["content"]["application/json"];
      async function readSummary(): Promise<Summary> { return (await fetch("https://onthebench.ai/api/v1/summary")).json(); }
      async function consume() { const s = await readSummary(); for (const row of s.results) {
        const value: number | null = row.metrics.added_latency_p99_us.value;
        const run: string | null = row.provenance.run_url;
        const measured: string | null = row.measured_at;
        const age: string = row.age;
        // @ts-expect-error Null must not silently become a quantitative value.
        const unsafe: number = row.metrics.added_latency_p99_us.value;
      } }
      type Query = paths["/api/v1/compare"]["get"]["parameters"]["query"];
      const query: Query = { a: "busbar", b: "litellm-python", ingress: "openai" };
      // @ts-expect-error Comparisons require both gateway IDs.
      const missing: Query = { a: "busbar" };
    `);
    execFileSync(process.execPath, [new URL("./node_modules/typescript/bin/tsc", import.meta.url).pathname, "--strict", "--noEmit", "--skipLibCheck", "--lib", "es2022,dom", join(dir, "client.ts")], { stdio: "pipe" });
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("byte-aware pagination preserves every record and UTF-8 boundaries", async () => {
  const { measurementPage } = await import("./retrieval.mjs");
  const rows = Array.from({ length: 17 }, (_, i) => ({ id: i, note: "🧪".repeat(240) }));
  let url = new URL("https://onthebench.ai/api/v1/measurements?gateway=busbar&limit=10"), seen = [];
  do {
    const page = measurementPage(rows, { datasetId: "dataset-test", offset: Number(url.searchParams.get("offset") || 0), limit: 10, url }, 2400);
    assert.ok(Buffer.byteLength(JSON.stringify(page) + "\n") <= 2400);
    assert.ok(page.returned > 0 && page.returned < 10);
    seen.push(...page.measurements.map(r => r.id));
    if (!page.next) break;
    url = new URL(page.next); assert.equal(url.searchParams.get("gateway"), "busbar");
  } while (seen.length < 100);
  assert.deepEqual(seen, rows.map(r => r.id));
  assert.equal(measurementPage([{ huge: "x".repeat(3000) }], { datasetId: "x", offset: 0, limit: 1, url }, 2400), null);
});

test("thin HTML, semantic guide, compact exports and run discovery", { skip: !artifact }, async () => {
  const env = { ASSETS: fileAssets(artifact) };
  const call = path => worker.fetch(new Request("https://onthebench.ai" + path), env);
  const pub = await (await call("/api/v1/benchmarks/latest")).json();
  const first = pub.gateways[0].id;
  const full = await (await call(`/api/v1/measurements?gateway=${first}&compact=0&limit=2`)).json();
  assert.equal(full.applied_filters.gateway, first); assert.equal(full.limit, 2); assert.ok(full.measurements.every(m => m.gateway_id === first));
  const compactResponse = await call(`/api/v1/measurements?gateway=${first}&compact=1&limit=2`);
  const compact = await compactResponse.json();
  assert.equal(compact.measurements.length, full.measurements.length);
  for (let i = 0; i < full.measurements.length; i++) for (const [key, metric] of Object.entries(full.measurements[i].metrics)) {
    const c = compact.measurements[i].metrics[key];
    assert.equal(c.value, metric.value); assert.equal(c.state, metric.state);
    assert.equal(compact.definitions[c.definition_id] ?? null, metric.definition);
  }
  const guide = await (await call("/llms-full.txt")).text();
  assert.ok(Buffer.byteLength(guide) < 64 * 1024);
  assert.equal((guide.match(/^# /gm) || []).length, 1);
  assert.doesNotMatch(guide, /\d+ of \d+|<select|coming soon|32,768 concurrent/);
  const csv = await (await call(`/gateways/data.csv?gateway=${first}&compact=1`)).text();
  assert.match(csv, /definition_id/); if (full.measurements.length) assert.match(csv, /\/api\/v1\/definitions/);
  const dictionaryResponse = await call(`/api/v1/definitions/${pub.dataset_id}`);
  assert.equal(dictionaryResponse.status, 200); assert.match(dictionaryResponse.headers.get("Cache-Control"), /immutable/);
  if (full.measurements.length) assert.ok(csv.length < (await (await call(`/gateways/data.csv?gateway=${first}&compact=0`)).text()).length);
  const runs = await (await call("/api/v1/runs?family=busbar&limit=2")).json();
  assert.ok(runs.runs.every(r => r.gateway_family === "busbar")); assert.equal(runs.limit, 2); assert.ok(runs.next);
  assert.equal((await call("/api/v1/runs?family=nonexistent")).status, 404);
  for (const path of ["/", "/gateways", "/gateways/performance", "/gateways/memory", "/gateways/charts"]) {
    const html = await (await call(path)).text(), doc = parseHTML(html).document;
    assert.equal(doc.querySelectorAll("h1").length, 1, path);
    const discovery = doc.querySelector('nav[aria-label="Crawler discovery"]');
    for (const file of ["robots.txt", "sitemap.xml", "llms.txt"]) {
      const url = `https://onthebench.ai/${file}`;
      assert.equal(discovery.querySelector(`a[href="${url}"]`)?.textContent, file);
    }
    assert.equal(doc.querySelectorAll("main > .view.hidden").length, 0, path);
    assert.ok(!doc.querySelector("#view-method"), path);
    assert.ok(!doc.querySelector("#matrix-grid"), path);
    assert.ok(doc.querySelector('meta[property="og:image"]'));
    for (const href of ["/gateways/data", "/api/v1", "/llms.txt"]) assert.ok(doc.querySelector(`main a[href="${href}"]`));
  }
  assert.equal(pub.license, "https://creativecommons.org/licenses/by/4.0/");
  assert.match(await (await call("/gateways/license")).text(), /CC BY 4.0/);
});

test("browser restores omitted view templates before initializing navigation", { skip: !artifact }, async () => {
  const { default: appModule } = await import("./app.js");
  const env = { ASSETS: fileAssets(artifact) };
  const html = await (await worker.fetch(new Request("https://onthebench.ai/gateways/performance"), env)).text();
  const doc = parseHTML(html).document;
  const before = doc.getElementById("view-table").innerHTML;
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async path => env.ASSETS.fetch(new Request("https://onthebench.ai" + path));
  try { await appModule.createApp(doc).restorePublicationViews(); }
  finally { globalThis.fetch = originalFetch; }
  assert.ok(doc.getElementById("view-method")); assert.ok(doc.getElementById("matrix-grid"));
  assert.equal(doc.getElementById("view-table").innerHTML, before);
});

test("UI and API state vocabulary agree without rewriting raw evidence", async () => {
  const { default: app } = await import("./app.js");
  const { canonicalCellState } = await import("./states.mjs");
  for (const served of [true, false, "failed", "not_configurable", "not_configured", "untestable", "unprobed_auth", "not_verified", "not_probed", "not_measured", "future_token"]) {
    assert.equal(app.cellState({ served })[1].replaceAll(" ", "_"), canonicalCellState({ served }));
  }
});

 test("Markdown separates inline evidence and SVG bounds without duplicate headings", () => {
 const md = htmlToMarkdown('<html><head><title>Title</title></head><body><main><h1>Title</h1><p><a href="/a">A</a><a href="/b">B</a></p><svg><title>1 ms: 20 req/s</title><title>5 ms: 30 req/s</title></svg></main></body></html>');
 assert.equal((md.match(/^# /gm) || []).length, 1);
 assert.match(md, /\) +\[/); assert.match(md, /req\/s; 5 ms/);
 });

test("default exports are compact and plain-text alternatives retain evidence", { skip: !artifact }, async () => {
 const env = { ASSETS: fileAssets(artifact) }; const call = p => worker.fetch(new Request("https://onthebench.ai" + p), env);
 const page = await (await call("/api/v1/measurements?gateway=busbar&ingress=openai&egress=openai&limit=2")).json();
 assert.ok(page.definitions); assert.ok(page.measurements.every(m => m.gateway_id === "busbar"));
 const txt = await call("/gateways/index.txt"); assert.equal(txt.status, 200); assert.match(txt.headers.get("Content-Type"), /text\/plain/);
 assert.equal(await txt.text(), await (await call("/gateways/index.md")).text());
 const csv = await (await call("/gateways/data.csv?gateway=busbar")).text(); assert.match(csv.split("\n")[0], /definition_id/); assert.doesNotMatch(csv.split("\n")[0], /"definition",/);
});

test("gateway entry pages are bounded and explicitly lead to full evidence", { skip: !artifact }, async () => {
 const call = path => worker.fetch(new Request("https://onthebench.ai" + path), { ASSETS: fileAssets(artifact) });
 const html = await (await call("/gateways/busbar")).text();
 assert.ok(Buffer.byteLength(html) < 20000); assert.match(html, /OpenAI → OpenAI/); assert.match(html, /busbar\/evidence/);
 const page = await (await call("/api/v1/measurements?gateway=busbar&limit=1")).json();
 const summary = await (await call("/api/v1/summary")).json();
 assert.equal(page.dataset_id, summary.dataset_id); assert.equal(page.generated_at, summary.generated_at); assert.equal(page.published_at, summary.published_at);
 assert.equal((await call("/gateways/busbar/evidence")).status, 200);
});


test('container version display uses the tag rather than the image digest', async () => {
  const { default: app } = await import('./app.js');
  const digest='a'.repeat(64);
  assert.equal(app.parseBuildVersion(`kong:3.9.4@sha256:${digest}`),'3.9.4');
  assert.equal(app.parseBuildVersion(`registry.example:5000/kong:3.9.4@sha256:${digest}`),'3.9.4');
  assert.equal(app.parseBuildVersion(`registry.example:5000/kong@sha256:${digest}`),'sha256:'+digest.slice(0,12));
  assert.equal(app.parseBuildVersion('apache/apisix:3.17.0-debian'),'3.17.0-debian');
  assert.equal(app.parseBuildVersion('target/release/ai-gateway'),null);
});

test('completed null metrics sort ahead of pending rows in both directions', async () => {
  const { default: app } = await import('./app.js');
  const rows = [{display:'agentgateway',v:null}, {display:'One-API',v:null,measured_at:'2026-11-08T20:15:58Z'}, {display:'Kong',v:19461,measured_at:'2026-11-08T20:15:58Z'}];
  for (const desc of [true,false]) assert.deepEqual(rows.slice().sort(app.rowComparator({get:g=>({v:g.v})},desc)).map(g=>g.display),['Kong','One-API','agentgateway']);
});

test('table trends are compact position changes and respect the neutral band', async () => {
 const {default:app}=await import('./app.js');
 const trend={state:'improved',label:'Improved',change_percent:12,previous_measured_at:'2026-08-01',rank_change:{before:8,after:1,change:7}};
 const render=t=>app.metricTd({v:100,text:'100',historyTrend:t});
 assert.match(render(trend),/↑ \+7/);
 assert.match(render({...trend,state:'flat',change_percent:1}),/trend-flat[^>]*>—/);
 assert.doesNotMatch(render({...trend,rank_change:null}),/metric-trend/);
 assert.doesNotMatch(render(trend),/<a /);
});

 test("refuted upstream verification cannot appear as a served matrix pairing", async () => {
  const { default: app } = await import("./app.js");
  const { canonicalCellState } = await import("./states.mjs");
  const cell = {served:true,perf:{egress_reverified:false,reverify_note:"Observed a different upstream dialect"}};
  assert.deepEqual(app.cellState(cell),["failed","not served"]);
  assert.equal(canonicalCellState(cell),"not_served");
  assert.equal(cell.served,true); // Original raw observation remains available.
  for (const checked of [true,null,undefined]) {
    const valid = {served:true,perf:{egress_reverified:checked}};
    assert.equal(canonicalCellState(valid),"served");
    assert.equal(app.cellState(valid)[0],"served");
  }
 });
