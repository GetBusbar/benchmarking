// SPDX-License-Identifier: Apache-2.0
import { renderDashboard, renderArticle, htmlToMarkdown, ORIGIN } from "./render.mjs";
import { compareMeasurements, measurementsCsv, PROTOCOLS } from "./public-data.mjs";
import { comparisonArticle } from "./evidence.mjs";

const caches = new WeakMap();
function inputs(env) {
  if (!caches.has(env.ASSETS)) caches.set(env.ASSETS, new Map());
  const cache = caches.get(env.ASSETS);
  return (path, json = true) => {
    if (!cache.has(path)) cache.set(path, env.ASSETS.fetch(new Request(ORIGIN + path)).then(r => {
      if (!r.ok) throw new Error(`Missing publication artifact: ${path}`);
      return json ? r.json() : r.text();
    }).catch(e => { cache.delete(path); throw e; }));
    return cache.get(path);
  };
}
function error(status, message) { return new Response(JSON.stringify({ error: message, status }) + "\n", { status, headers: { "Content-Type": "application/json; charset=utf-8" } }); }
function json(value) { return new Response(JSON.stringify(value) + "\n", { headers: { "Content-Type": "application/json; charset=utf-8" } }); }
function text(value, type) { return new Response(value, { headers: { "Content-Type": `${type}; charset=utf-8` } }); }
const dashboard = new Set(["/", "/gateways", ...["memory", "performance", "frontier", "streaming", "matrix", "charts", "method"].map(v => `/gateways/${v}`)]);
function parameters(url, allowed) {
  for (const key of url.searchParams.keys()) {
    if (!allowed.includes(key)) throw new RangeError(`Unknown parameter: ${key}`);
    if (url.searchParams.getAll(key).length !== 1) throw new RangeError(`Repeated parameter: ${key}`);
  }
  return Object.fromEntries(url.searchParams);
}
function filterQuery(url, publication, allowed) {
  const q = parameters(url, allowed);
  if (Object.values(q).some(v => v === "")) throw new RangeError("Query values must not be empty");
  for (const key of ["gateway", "a", "b"]) if (q[key] && !publication.gateways.some(g => g.id === q[key])) throw new URIError(`Unknown gateway: ${q[key]}`);
  for (const key of ["ingress", "egress"]) if (q[key] && !PROTOCOLS.includes(q[key])) throw new RangeError(`Unknown protocol: ${q[key]}`);
  if (q.metric && q.metric !== "throughput_rps" && !publication.metrics.includes(q.metric)) throw new RangeError(`Unknown metric: ${q.metric}`);
  if (q.bound && (q.metric !== "throughput_rps" || !["1", "5", "10", "50", "100", "none"].includes(q.bound))) throw new RangeError("bound requires throughput_rps and a published bound: 1, 5, 10, 50, 100, none");
  return q;
}
function select(measurements, q) {
  return measurements.filter(m => (!q.gateway || m.gateway_id === q.gateway) && (!q.ingress || m.test.ingress_protocol === q.ingress) && (!q.egress || m.test.upstream_protocol === q.egress))
    .map(m => q.metric ? { ...m, metrics: m.metrics[q.metric] ? { [q.metric]: m.metrics[q.metric] } : {}, frontier: q.metric === "throughput_rps" ? m.frontier : [] } : m);
}
function integer(value, fallback, min, max) {
  if (value === undefined) return fallback;
  if (!/^\d+$/.test(value) || Number(value) < min || Number(value) > max) throw new RangeError(`Expected integer from ${min} through ${max}`);
  return Number(value);
}
async function route(request, env) {
  const url = new URL(request.url), path = decodeURIComponent(url.pathname);
  if (url.hostname === "www.onthebench.ai") { url.hostname = "onthebench.ai"; return Response.redirect(url.href, 308); }
  if (/%2f|%5c|%2e/i.test(path) || path.includes("\\") || path.startsWith("/_publication")) return error(404, "Not found");
  if (request.method === "OPTIONS") return new Response(null, { status: 204 });
  if (!["GET", "HEAD"].includes(request.method)) return new Response(null, { status: 405, headers: { Allow: "GET, HEAD, OPTIONS" } });
  if (path.length > 500 || url.search.length > 2000) return error(400, "Request URL too long");
  if (path.length > 1 && path.endsWith("/")) { url.pathname = path.slice(0, -1); return Response.redirect(url.href, 308); }
  if (path === "/gateways/methodology" || path === "/gateways/methodology.md") { url.pathname = path.replace("methodology", "method"); return Response.redirect(url.href, 308); }
  const input = inputs(env);
  const getPublication = () => input("/_publication/records.json");
  if (path === "/api/v1/measurements" || path === "/gateways/data.csv") {
    const publication = await getPublication();
    const q = filterQuery(url, publication, path.endsWith(".csv") ? ["gateway", "ingress", "egress", "metric"] : ["gateway", "ingress", "egress", "metric", "limit", "offset"]);
    const rows = select(publication.measurements, q);
    if (path.endsWith(".csv")) return text(measurementsCsv(rows), "text/csv");
    const limit = integer(q.limit, 50, 1, 100), offset = integer(q.offset, 0, 0, Number.MAX_SAFE_INTEGER);
    const next = new URL(url); next.searchParams.set("offset", String(offset + limit)); next.searchParams.set("limit", String(limit));
    return json({ schema_version: "1.0", dataset_id: publication.dataset_id, total: rows.length, offset, limit, next: offset + limit < rows.length ? next.href : null, measurements: rows.slice(offset, offset + limit) });
  }
  const pair = path.match(/^\/gateways\/compare\/([a-z0-9-]+)-vs-([a-z0-9-]+?)(\.md)?$/);
  if (path === "/api/v1/compare" && [...url.searchParams.keys()].every(k => k === "a" || k === "b")) {
    const q = parameters(url, ["a", "b"]);
    if (/^[a-z0-9-]+$/.test(q.a || "") && /^[a-z0-9-]+$/.test(q.b || "") && q.a !== q.b) {
      const staticPair = await env.ASSETS.fetch(new Request(`${ORIGIN}/api/v1/comparisons/${q.a}-vs-${q.b}.json`));
      if (staticPair.ok) return staticPair;
    }
  }
  if (path === "/api/v1/compare" || (pair && url.search)) {
    const publication = await getPublication();
    if (pair) { url.searchParams.set("a", pair[1]); url.searchParams.set("b", pair[2]); }
    const q = filterQuery(url, publication, ["a", "b", "ingress", "egress", "metric", "bound"]);
    if (!q.a || !q.b || q.a === q.b) throw new RangeError("Provide two distinct gateway IDs as a and b");
    const comparison = compareMeasurements(publication.measurements, q);
    if (!pair) return json(comparison);
    const canonical = path.replace(/\.md$/, "");
    const html = renderArticle(await input("/_publication/template.html", false), { path: canonical, title: `${q.a} vs ${q.b}`, body: comparisonArticle(comparison), markdown: canonical + ".md" + new URL(request.url).search });
    return text(pair[3] ? htmlToMarkdown(html) : html, pair[3] ? "text/markdown" : "text/html");
  }
  const dashboardPath = path === "/index.md" ? "/" : path === "/gateways/index.md" ? "/gateways" : path.replace(/\.md$/, "");
  // Server-render every shareable dashboard query using the very same renderer.
  // A crawler never sees default numbers under a URL naming a different cell.
  if (dashboard.has(dashboardPath) && url.search) {
    const [template, data, publication] = await Promise.all([input("/_publication/template.html", false), input("/data.json"), getPublication()]);
    const html = renderDashboard(template, data, dashboardPath + url.search, publication);
    return text(path.endsWith(".md") ? htmlToMarkdown(html) : html, path.endsWith(".md") ? "text/markdown" : "text/html");
  }
  const raw = path.match(/^\/api\/v1\/runs\/(run-[a-z0-9-]+-[a-f0-9]{64})\/raw\.json$/);
  if (raw) {
    const response = await env.ASSETS.fetch(new Request(ORIGIN + path + ".gz"));
    if (!response.ok) return error(404, "Unknown run");
    return new Response(response.body, { encodeBody: "manual", headers: { "Content-Type": "application/json; charset=utf-8", "Content-Encoding": "gzip", "Cache-Control": "public, max-age=31536000, immutable", ETag: `"${raw[1].slice(-64)}"` } });
  }
  let asset = path;
  if (path === "/api/v1") asset = "/api/v1/index.json";
  else if (path === "/api/v1/schema") asset = "/api/v1/schema/index.json";
  else if (/^\/api\/v1\/(gateways|methodology|benchmarks\/latest|gateways\/[a-z0-9-]+|benchmarks\/(?:dataset-[a-f0-9]{64}|run-[a-z0-9-]+-[a-f0-9]{64}))$/.test(path)) asset += ".json";
  else if (path === "/") asset = "/index.html";
  else if (!path.split("/").at(-1).includes(".")) asset += ".html";
  const response = await env.ASSETS.fetch(new Request(ORIGIN + asset));
  if (!response.ok) {
    const dataset = path.match(/^\/api\/v1\/benchmarks\/(dataset-[a-f0-9]{64})(?:\.json)?$/);
    if (dataset && env.PUBLICATIONS) {
      const stored = await env.PUBLICATIONS.get(`datasets/${dataset[1]}`);
      if (stored != null) return new Response(stored, { headers: { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "public, max-age=31536000, immutable" } });
    }
    return error(404, "Not found");
  }
  const headers = new Headers(response.headers);
  if (path.endsWith(".md")) headers.set("Content-Type", "text/markdown; charset=utf-8");
  if (path.endsWith(".txt")) headers.set("Content-Type", "text/plain; charset=utf-8");
  if (/^\/api\/v1\/benchmarks\/(?:dataset-|run-)/.test(path)) headers.set("Cache-Control", "public, max-age=31536000, immutable");
  return new Response(response.body, { status: response.status, headers });
}
export default {
  async fetch(request, env) {
    let response;
    try { response = await route(request, env); }
    catch (e) {
      if (e instanceof RangeError) response = error(400, e.message);
      else if (e instanceof URIError) response = error(404, e.message);
      else { console.error(e); response = error(500, "Publication could not be read"); }
    }
    const headers = new Headers(response.headers);
    headers.set("Access-Control-Allow-Origin", "*");
    headers.set("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
    headers.set("Access-Control-Allow-Headers", "Accept, If-None-Match, If-Modified-Since");
    headers.set("Access-Control-Expose-Headers", "ETag, Last-Modified, Link");
    headers.set("X-Content-Type-Options", "nosniff");
    headers.set("Link", `<${ORIGIN}/llms.txt>; rel="describedby"; type="text/plain", <${ORIGIN}/api/v1>; rel="service-desc"; type="application/json"`);
    if (!headers.has("Cache-Control")) headers.set("Cache-Control", response.ok ? "public, max-age=0, must-revalidate" : "no-store");
    // Static assets already have ETags. Small generated responses receive one as
    // well, so repeated API reads need not download an unchanged result.
    if (response.ok && !headers.has("ETag") && response.status !== 204) {
      const body = await response.arrayBuffer();
      const digest = await crypto.subtle.digest("SHA-256", body);
      headers.set("ETag", '"' + [...new Uint8Array(digest)].map(b => b.toString(16).padStart(2, "0")).join("") + '"');
      response = new Response(body, { status: response.status, headers });
    }
    if (response.ok && request.headers.get("If-None-Match")?.split(/,\s*/).some(tag => tag === headers.get("ETag") || tag === "*")) return new Response(null, { status: 304, headers });
    // Preserve already encoded asset streams, especially immutable raw JSON.
    // Workers' default automatic encoding would gzip our gzip bytes again.
    return new Response(request.method === "HEAD" || response.status === 204 ? null : response.body, { status: response.status, headers, encodeBody: "manual" });
  },
};
