#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// Post-deployment checks exercise the actual CDN, not just the local adaptor.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";

const origin = process.argv[2] || "https://onthebench.ai";
const failures = [];
async function check(name, action) {
  try { await action(); console.log(`PASS ${name}`); }
  catch (e) { failures.push(name); console.error(`FAIL ${name}: ${e.message}`); }
}
async function get(path, userAgent = "OnTheBench-Publication-Check/1.0") {
  const r = await fetch(new URL(path, origin), { headers: { "User-Agent": userAgent }, signal: AbortSignal.timeout(30000) });
  assert.equal(r.status, 200, `${path}: HTTP ${r.status}`);
  return r;
}
await check("benchmark facts in the HTML response", async () => {
  const html = await (await get("/gateways/performance")).text();
  assert.match(html, /<tbody>[\s\S]*?<tr[\s>]/);
  assert.ok(!html.includes("Protocol matrix results are coming soon"));
});
await check("Markdown results", async () => {
  const r = await get("/gateways/performance.md");
  assert.match(r.headers.get("Content-Type"), /text\/markdown/);
  assert.match(await r.text(), /\|.*Gateway.*\|/);
});
await check("raw download has its published SHA-256 after HTTP decoding", async () => {
  const catalog = await (await get("/api/v1/gateways")).json();
  const key = catalog.gateways[0].id;
  const page = await (await get(`/api/v1/measurements?gateway=${encodeURIComponent(key)}&limit=1`)).json();
  const record = page.measurements[0];
  const raw = await get(new URL(record.provenance.raw_result_url).pathname);
  // Native fetch decodes Content-Encoding. A double-compressed response fails.
  const bytes = Buffer.from(await raw.arrayBuffer());
  assert.equal(createHash("sha256").update(bytes).digest("hex"), record.provenance.raw_sha256);
  assert.ok(JSON.parse(bytes).matrix);
});
await check("unknown routes return 404", async () => {
  const r = await fetch(new URL("/api/v1/gateways/publication-check-missing", origin));
  assert.equal(r.status, 404);
});
for (const userAgent of ["GPTBot/1.3", "Googlebot/2.1", "ChatGPT-User/1.0", "OAI-SearchBot/1.0", "ClaudeBot/1.0", "Claude-User/1.0", "Claude-SearchBot/1.0", "Bingbot/2.0", "PerplexityBot/1.0", "Python-urllib/3.14"]) {
  await check(`public retrieval with ${userAgent}`, async () => {
    const robots = await get("/robots.txt", userAgent);
    assert.match(await robots.text(), /User-agent: \*\nAllow: \//);
    assert.equal(robots.headers.get("cf-mitigated"), null);
    const sitemap = await get("/sitemap.xml", userAgent);
    assert.match(await sitemap.text(), /<urlset[^>]*>[\s\S]*<loc>https:\/\/onthebench.ai\/gateways<\/loc>/);
    const r = await get("/llms.txt", userAgent);
    assert.match(r.headers.get("Content-Type"), /text\/plain/);
    assert.match(await r.text(), /# On the Bench/);
  });
}
await check("compact evidence, discovery and canonical pages", async () => {
  const response = await get("/api/v1/summary");
  const body = await response.text();
  assert.ok(Buffer.byteLength(body) < 64 * 1024);
  const summary = JSON.parse(body);
  assert.ok(summary.results.length);
  for (const row of summary.results) {
    assert.ok("measured_at" in row && "published_at" in row && "age" in row);
    assert.ok(row.provenance.methodology_url);
  }
  const sitemap = await (await get("/sitemap.xml")).text();
  for (const path of ["/gateways", "/gateways/data", "/gateways/method", "/gateways/runs"]) {
    assert.ok(sitemap.includes(`<loc>${origin}${path}</loc>`));
    const html = await (await get(path)).text();
    assert.ok(html.includes(`href="${origin}${path}"`));
  }
  const spec = await (await get("/openapi.json")).json();
  assert.equal(spec.paths["/api/v1/summary"].get.responses[200].content["application/json"].schema.$ref, "#/components/schemas/Summary");
});
if (failures.length) {
  console.error(`Deployment checks failed: ${failures.join("; ")}. The deployment may already be live; inspect HTTP/CDN behavior before declaring the release complete.`);
  process.exitCode = 1;
}
await check("query filters are preserved on measurements", async () => {
  const catalog = await (await get("/api/v1/gateways")).json();
  const gateway = catalog.gateways.find(g => g.id === "busbar")?.id || catalog.gateways[0].id;
  const r = await get(`/api/v1/measurements?gateway=${gateway}&ingress=openai&egress=openai&limit=2&compact=1`);
  assert.ok(new URL(r.url).searchParams.has("gateway"));
  const page = await r.json(); assert.equal(page.limit, 2);
  assert.ok(page.measurements.length > 0);
  assert.ok(page.measurements.every(m => m.gateway_id === gateway && m.test.ingress_protocol === "openai" && m.test.upstream_protocol === "openai"));
});
await check("root and tab HTML are thin, self-canonical and discoverable", async () => {
  for (const path of ["/", "/gateways", "/gateways/performance", "/gateways/memory", "/gateways/charts"]) {
    const html = await (await get(path)).text();
    assert.ok(html.includes(`href="${origin}${path}"`));
    assert.equal((html.match(/<h1[ >]/g) || []).length, 1);
    assert.ok(!html.includes('id="matrix-grid"'));
    assert.ok(!html.includes('id="view-method"'));
    for (const link of ["/llms.txt", "/api/v1", "/gateways/data"]) assert.ok(html.includes(`href="${link}"`));
    assert.ok(!html.includes("coming soon"));
    if (path === "/" || path === "/gateways") assert.match(html, /<tbody>[\s\S]*?<tr[\s>]/);
  }
});
await check("semantic guide, dataset license and recorded release time", async () => {
  const guide = await (await get("/llms-full.txt")).text();
  assert.ok(Buffer.byteLength(guide) < 64 * 1024);
  assert.equal((guide.match(/^# /gm) || []).length, 1);
  assert.doesNotMatch(guide, /\d+ of \d+|coming soon/);
  const summary = await (await get("/api/v1/summary")).json();
  assert.equal(summary.license, "https://creativecommons.org/licenses/by/4.0/");
  assert.ok(Number.isFinite(Date.parse(summary.published_at)));
  await get("/social-card.png"); await get("/gateways/license"); await get("/api/v1/runs?limit=2");
});
if (failures.length) process.exitCode = 1;
