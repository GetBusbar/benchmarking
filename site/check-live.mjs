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
for (const userAgent of ["ChatGPT-User/1.0", "OAI-SearchBot/1.0", "ClaudeBot/1.0", "PerplexityBot/1.0", "Python-urllib/3.14"]) {
  await check(`public retrieval with ${userAgent}`, async () => {
    const r = await get("/llms.txt", userAgent);
    assert.match(r.headers.get("Content-Type"), /text\/plain/);
    assert.match(await r.text(), /# On the Bench/);
  });
}
if (failures.length) {
  console.error(`Deployment checks failed: ${failures.join("; ")}. The deployment may already be live; inspect HTTP/CDN behavior before declaring the release complete.`);
  process.exitCode = 1;
}
