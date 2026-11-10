// SPDX-License-Identifier: Apache-2.0
import { renderDashboard, renderArticle, htmlToMarkdown, ORIGIN } from "./render.mjs";
import { compareMeasurements, measurementsCsv, compactSummary, PROTOCOLS } from "./public-data.mjs";
import { historyPoints, historyArticle, historySummary, historicalComparison, fieldRankings, expandHistorical } from "./history.mjs";
import { comparisonArticle } from "./evidence.mjs";

import { measurementPage, compactCsv, gatewayFamily } from "./retrieval.mjs";

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
// WITHDRAWN MEANS GONE FROM EVERY SURFACE. Retained releases, fields and datasets live in the
// PUBLICATIONS store from before a withdrawal, so anything read from there that names a withdrawn
// run's sha256 is refused (single resources) or dropped (lists), exactly like /api/v1/runs/<id>.
async function withdrawnShas(env) {
  const response = await env.ASSETS.fetch(new Request(`${ORIGIN}/_publication/withdrawn-runs.json`));
  return response.ok ? Object.keys(await response.json()) : [];
}
const namesWithdrawn = (text, shas) => typeof text === "string" && shas.some(sha => text.includes(sha));
async function storedFieldWithdrawn(env, id, shas) {
  if (!shas.length) return false;
  return namesWithdrawn(await env.PUBLICATIONS?.get(`fields/${id}`), shas) || namesWithdrawn(await env.PUBLICATIONS?.get(`field-evidence/${id}`), shas);
}
const WITHDRAWN = () => error(410, "This publication includes withdrawn runs and is unavailable.");
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
  // Plain http and www both land on the one canonical https origin.
  if (url.hostname.endsWith("onthebench.ai") && (url.protocol === "http:" || url.hostname === "www.onthebench.ai")) { url.protocol = "https:"; url.hostname = "onthebench.ai"; return Response.redirect(url.href, 308); }
  if (/%2f|%5c|%2e/i.test(path) || path.includes("\\") || path.startsWith("/_publication")) return error(404, "Not found");
  if (request.method === "OPTIONS") return new Response(null, { status: 204 });
  if (!["GET", "HEAD"].includes(request.method)) return new Response(null, { status: 405, headers: { Allow: "GET, HEAD, OPTIONS" } });
  if (path.length > 500 || url.search.length > 2000) return error(400, "Request URL too long");
  if (path.length > 1 && path.endsWith("/")) { url.pathname = path.slice(0, -1); return Response.redirect(url.href, 308); }
  if (path === "/gateways/methodology" || path === "/gateways/methodology.md") { url.pathname = path.replace("methodology", "method"); return Response.redirect(url.href, 308); }
  if (path === "/security.txt") { url.pathname = "/.well-known/security.txt"; return Response.redirect(url.href, 301); }
  if (path.endsWith(".txt") && !["/robots.txt", "/humans.txt", "/.well-known/security.txt"].includes(path) && !path.startsWith("/llms")) {
    const alternate = new URL(url); alternate.pathname = path.replace(/\.txt$/, ".md");
    const response = await route(new Request(alternate, request), env);
    const headers = new Headers(response.headers); headers.set("Content-Type", "text/plain; charset=utf-8");
    return new Response(response.body, { status: response.status, headers });
  }
  const requestedRun = path.match(/(?:^|\/)(run-[a-z0-9-]+-([a-f0-9]{64}))(?:[/.]|$)/);
  if (requestedRun) {
    const response = await env.ASSETS.fetch(new Request(`${ORIGIN}/_publication/withdrawn-runs.json`));
    const withdrawn = response.ok ? await response.json() : {};
    if (withdrawn[requestedRun[2]]) return error(410, "This run was withdrawn and its measurements are unavailable.");
  }
  const input = inputs(env);
  const getPublication = () => input("/_publication/records.json");
  const historyMatch = path.match(/^\/gateways\/([a-z0-9-]+)\/history(\.md)?$/);
  const runMeasurements = path.match(/^\/api\/v1\/runs\/(run-[a-z0-9-]+-[a-f0-9]{64})\/measurements$/);
  if (runMeasurements) {
    const q = parameters(url, ['limit','offset']);
    const catalog = await input('/_publication/history/catalog.json');
    if (!catalog.runs.some(r => r.id === runMeasurements[1])) throw new URIError('Unknown run');
    const records = expandHistorical(await input(`/_publication/history/runs/${runMeasurements[1]}.json`));
    return json(measurementPage(records, { datasetId: catalog.catalog_id, limit: integer(q.limit,20,1,100), offset: integer(q.offset,0,0,Number.MAX_SAFE_INTEGER), url, compact: true }));
  }
  if (path === '/api/v1/history' || historyMatch) {
    const q = parameters(url, ['family','ingress','egress','metric','bound','from','to','limit','offset','catalog','a','b']);
    if (historyMatch) q.family = historyMatch[1];
    q.ingress ||= 'openai'; q.egress ||= 'openai'; q.metric ||= 'added_latency_p99_us';
    if (!PROTOCOLS.includes(q.ingress) || !PROTOCOLS.includes(q.egress)) throw new RangeError('Unknown protocol');
    let catalog = await input('/_publication/history/catalog.json');
    const archivedRevision = q.catalog && q.catalog !== catalog.catalog_id;
    if (archivedRevision && /^catalog-[a-f0-9]{64}$/.test(q.catalog)) {
      const stored = await env.PUBLICATIONS?.get(`history/catalogs/${q.catalog}`);
      if (stored) {
        // A catalog retained from before a withdrawal still lists the withdrawn runs; drop them here too.
        const shas = await withdrawnShas(env);
        catalog = JSON.parse(stored);
        catalog = { ...catalog, runs: catalog.runs.filter(r => !shas.some(sha => String(r.id).endsWith(sha))) };
      }
    }
    if (q.catalog && q.catalog !== catalog.catalog_id) return error(409, 'Catalog revision changed. Restart traversal; records from different revisions are never silently mixed.');
    if (!catalog.runs.some(r => r.gateway_family === q.family)) throw new URIError('Unknown gateway family');
    for (const key of ['from','to']) if (q[key] && (!/^\d{4}-\d{2}-\d{2}$/.test(q[key]) || !Number.isFinite(Date.parse(q[key])))) throw new RangeError('Date must be YYYY-MM-DD');
    async function archivedShard(key,empty) {
      if (catalog.shards && !catalog.shards[key]) return empty;
      const storageKey=catalog.shards?.[key] || `history/catalogs/${catalog.catalog_id}/${key}`;
      const value=await env.PUBLICATIONS.get(storageKey);
      if (value == null && catalog.shards) throw new Error('Retained history shard is unavailable');
      return value == null ? empty : JSON.parse(value);
    }
    const shard=`families/${q.family}/${q.ingress}--${q.egress}`;
    const records = expandHistorical(archivedRevision
      ? await archivedShard(shard,{measurements:[],definitions:{}})
      : await input(`/_publication/history/${shard}.json`));
    const metrics = [...new Set(['added_latency_p50_us','added_latency_p99_us','added_ttft_p99_us','throughput_rps', ...records.flatMap(m=>Object.keys(m.metrics))])].sort();
    if (!metrics.includes(q.metric)) throw new RangeError('Unknown historical metric');
    if (q.bound && !['1','5','10','50','100','none'].includes(q.bound)) throw new RangeError('Unknown throughput bound');
    if (q.from && q.to && q.from > q.to) throw new RangeError('From date must precede through date');
    const runs = catalog.runs.filter(r=>r.gateway_family === q.family && (!q.from || (r.measured_at && r.measured_at >= q.from)) && (!q.to || (r.measured_at && r.measured_at.slice(0,10) <= q.to)));
    const limit = integer(q.limit,20,1,100), offset = integer(q.offset,0,0,Number.MAX_SAFE_INTEGER);
    const readings = new Map(historyPoints(records,q).map(p=>[p.run_id,p]));
    const points = runs.slice(offset,offset+limit).map(r=>({ ...(readings.get(r.id) || { run_id:r.id,gateway_id:r.gateway_id,gateway_build:r.build,measured_at:r.measured_at,engine:r.engine,value:null,state:r.evidence_available ? 'not_measured' : 'evidence_unavailable',provenance:{run_url:r.source_url} }), disposition:r.disposition }));
    const placementKey=`placements/${q.family}/${q.ingress}--${q.egress}`;
    const placements=archivedRevision ? await archivedShard(placementKey,[]) : await input(`/_publication/history/${placementKey}.json`);
    for(const point of points) point.placements=placements.filter(p=>p.run_id===point.run_id && p.metric===q.metric && (q.metric!=='throughput_rps' || p.bound===(q.bound || '10')));
    const definitions = {};
    for (const p of points) { if (p.definition) { let id=Object.keys(definitions).find(k=>definitions[k]===p.definition); if (!id) {id=`definition-${Object.keys(definitions).length+1}`;definitions[id]=p.definition;} p.definition_id=id;} delete p.definition; }
    const next = new URL(url); next.searchParams.set('offset',String(offset+points.length)); next.searchParams.set('catalog',catalog.catalog_id);
    // The then → now summary always reads the representative OpenAI → OpenAI cell, whatever pair the table shows.
    const representative = q.ingress === 'openai' && q.egress === 'openai';
    const summaryShard = `families/${q.family}/openai--openai`, summaryPlacementKey = `placements/${q.family}/openai--openai`;
    const summaryRecords = representative ? records : expandHistorical(archivedRevision ? await archivedShard(summaryShard,{measurements:[],definitions:{}}) : await input(`/_publication/history/${summaryShard}.json`));
    const summaryPlacements = representative ? placements : archivedRevision ? await archivedShard(summaryPlacementKey,[]) : await input(`/_publication/history/${summaryPlacementKey}.json`);
    const summary = historySummary(summaryRecords, catalog.runs.filter(r=>r.gateway_family === q.family), summaryPlacements);
    const page = { schema_version:'1.0',catalog_id:catalog.catalog_id,projector:catalog.projector,total:runs.length,limit,offset,returned:points.length,next:offset+points.length<runs.length?next.href:null,metrics,definitions,summary,points };
    let comparisons = [];
    if (q.a || q.b) {
      const pick = id=>records.find(m=>m.run_id===id && m.test.ingress_protocol===q.ingress && m.test.upstream_protocol===q.egress);
      const left=pick(q.a),right=pick(q.b);
      if (!left || !right || q.a===q.b) throw new RangeError('Select two distinct runs containing this protocol pair');
      comparisons=historicalComparison(left,right);
    }
    if (!historyMatch) return json({...page,comparisons});
    const canonical=path.replace(/\.md$/,'');
    const display=(await getPublication()).gateways?.find(g=>g.id===q.family)?.name||q.family;
    const html=renderArticle(await input('/_publication/template.html',false),{path:canonical,title:`${display} benchmark history`,body:historyArticle(q.family,page,q,comparisons,display)});
    return text(historyMatch[2]?htmlToMarkdown(html):html,historyMatch[2]?'text/markdown':'text/html');
  }
  const fieldRanks = path.match(/^\/api\/v1\/fields\/(field-[a-f0-9]{64})\/rankings$/);
  if (fieldRanks) {
    const q=parameters(url,['ingress','egress','metric','bound']);
    q.ingress ||= 'openai'; q.egress ||= 'openai'; q.metric ||= 'added_latency_p99_us';
    if (!PROTOCOLS.includes(q.ingress) || !PROTOCOLS.includes(q.egress)) throw new RangeError('Unknown protocol');
    const fields=await input('/_publication/history/fields.json');
    const collection=fields.some(f=>f.field_id===fieldRanks[1]) ? await input(`/_publication/history/fields/${fieldRanks[1]}.json`) : JSON.parse(await env.PUBLICATIONS?.get(`field-evidence/${fieldRanks[1]}`) || 'null');
    if(!collection) throw new URIError('Unknown field');
    if (!fields.some(f=>f.field_id===fieldRanks[1]) && namesWithdrawn(JSON.stringify(collection), await withdrawnShas(env))) return WITHDRAWN();
    return json({schema_version:'1.0',field_id:fieldRanks[1],...fieldRankings(collection,q)});
  }
  if (path === '/api/v1/releases' && env.PUBLICATIONS) {
    const q=parameters(url,['limit','cursor']);
    const limit=integer(q.limit,20,1,100);
    const list=await env.PUBLICATIONS.list({prefix:'releases/',limit,...(q.cursor?{cursor:q.cursor}:{})});
    const shas=await withdrawnShas(env);
    const raw=(await Promise.all(list.keys.map(k=>env.PUBLICATIONS.get(k.name)))).filter(Boolean);
    const kept=[];
    for (const text of raw) {
      if (namesWithdrawn(text, shas)) continue;
      const ids=[...new Set(text.match(/field-[a-f0-9]{64}/g)||[])], datasets=[...new Set(text.match(/dataset-[a-f0-9]{64}/g)||[])];
      let hidden=false;
      for (const id of ids) if (await storedFieldWithdrawn(env,id,shas)) { hidden=true; break; }
      for (const d of datasets) if (!hidden && namesWithdrawn(await env.PUBLICATIONS.get(`datasets/${d}`), shas)) hidden=true;
      if (!hidden) kept.push(JSON.parse(text));
    }
    const releases=kept;
    const next=new URL(url);if(list.cursor)next.searchParams.set('cursor',list.cursor);
    return json({schema_version:'1.0',limit,next:list.list_complete?null:next.href,releases});
  }
  if (path === '/api/v1/fields') {
    const q=parameters(url,['limit','offset','cursor']);
    if (env.PUBLICATIONS) {
      if(q.offset) throw new RangeError('Follow cursor pagination for retained fields');
      const limit=integer(q.limit,20,1,100);
      const list=await env.PUBLICATIONS.list({prefix:'field-index/',limit,...(q.cursor?{cursor:q.cursor}:{})});
      const shas=await withdrawnShas(env);
      const all=(await Promise.all(list.keys.map(k=>env.PUBLICATIONS.get(k.name)))).filter(Boolean).map(JSON.parse);
      const fields=[];
      for (const f of all) if (!(await storedFieldWithdrawn(env,f.field_id,shas))) fields.push(f);
      const next=new URL(url);if(list.cursor)next.searchParams.set('cursor',list.cursor);
      return json({schema_version:'1.0',total:null,limit,offset:0,next:list.list_complete?null:next.href,fields});
    }
    const rows=await input('/_publication/history/fields.json');
    const limit=integer(q.limit,20,1,100),offset=integer(q.offset,0,0,Number.MAX_SAFE_INTEGER);
    const next=new URL(url);next.searchParams.set('offset',String(offset+limit));
    return json({schema_version:'1.0',total:rows.length,limit,offset,next:offset+limit<rows.length?next.href:null,fields:rows.slice(offset,offset+limit)});
  }

  if (path === "/api/v1/summary" && url.search) {
    const publication = await getPublication();
    return json(compactSummary(publication, filterQuery(url, publication, ["gateway", "ingress", "egress"])));
  }
  if (path === "/api/v1/measurements" || path === "/gateways/data.csv") {
    const publication = await getPublication();
    const q = filterQuery(url, publication, path.endsWith(".csv") ? ["gateway", "ingress", "egress", "metric", "compact"] : ["gateway", "ingress", "egress", "metric", "limit", "offset", "compact"]);
    if (q.compact && !["0", "1"].includes(q.compact)) throw new RangeError("compact must be 0 or 1");
    const rows = select(publication.measurements, q);
    if (path.endsWith(".csv")) return text(q.compact !== "0" ? compactCsv(rows, publication.measurements, publication.dataset_id) : measurementsCsv(rows), "text/csv");
    const limit = integer(q.limit, 50, 1, 100), offset = integer(q.offset, 0, 0, Number.MAX_SAFE_INTEGER);
    const page = measurementPage(rows, { datasetId: publication.dataset_id, generatedAt: publication.generated_at, publishedAt: publication.published_at, limit, offset, url, compact: q.compact !== "0" });
    if (!page) return error(413, "One measurement exceeds the response budget. Filter by gateway, ingress, egress and metric, or use /api/v1/summary and its raw evidence links.");
    return json(page);
  }
  if (path === "/api/v1/runs") {
    const q = parameters(url, ["gateway", "family", "limit", "offset"]);
    if (Object.values(q).some(v => v === "")) throw new RangeError("Query values must not be empty");
    const catalog = await input("/_publication/runs.json");
    if (q.gateway && !catalog.some(r => r.gateway_id === q.gateway)) throw new URIError("Unknown historical gateway");
    if (q.family && !catalog.some(r => r.gateway_family === q.family)) throw new URIError("Unknown gateway family");
    const rows = catalog.filter(r => (!q.gateway || r.gateway_id === q.gateway) && (!q.family || r.gateway_family === q.family));
    const limit = integer(q.limit, 20, 1, 100), offset = integer(q.offset, 0, 0, Number.MAX_SAFE_INTEGER);
    const next = new URL(url); next.searchParams.set("offset", String(offset + limit)); next.searchParams.set("limit", String(limit));
    return json({ schema_version: "1.0", total: rows.length, limit, offset, next: offset + limit < rows.length ? next.href : null, runs: rows.slice(offset, offset + limit) });
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
    const names = Object.fromEntries((publication.gateways || []).map(g => [g.id, g.name]));
    const html = renderArticle(await input("/_publication/template.html", false), { path: canonical, title: `${names[q.a] || q.a} vs ${names[q.b] || q.b}`, body: comparisonArticle(comparison, names), markdown: canonical + ".md" + new URL(request.url).search });
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
    const body = response.ok ? response.body : await env.PUBLICATIONS?.get(`raw/${raw[1]}`, 'arrayBuffer');
    if (!body) return error(404, "Unknown run");
    return new Response(body, { encodeBody: "manual", headers: { "Content-Type": "application/json; charset=utf-8", "Content-Encoding": "gzip", "Cache-Control": "public, max-age=31536000, immutable", ETag: `"${raw[1].slice(-64)}"` } });
  }
  const definitionsId = path.match(/^\/api\/v1\/definitions\/(dataset-[a-f0-9]{64})(?:\.json)?$/);
  if (definitionsId) {
    const response = await env.ASSETS.fetch(new Request(`${ORIGIN}/api/v1/definitions/${definitionsId[1]}.json`));
    const stored = response.ok ? await response.text() : await env.PUBLICATIONS?.get(`definitions/${definitionsId[1]}`);
    return stored == null ? error(404, "Unknown definition catalog") : new Response(stored, { headers: { "Content-Type": "application/json; charset=utf-8", "Cache-Control": "public, max-age=31536000, immutable" } });
  }
  let asset = path;
  if (path === "/api/v1") asset = "/api/v1/index.json";
  else if (path === "/api/v1/schema") asset = "/api/v1/schema/index.json";
  else if (/^\/api\/v1\/(?:fields\/field-[a-f0-9]{64}|releases)$/.test(path)) asset += ".json";
  else if (/^\/api\/v1\/(definitions|summary|gateways|methodology|benchmarks\/latest|gateways\/[a-z0-9-]+|benchmarks\/(?:dataset-[a-f0-9]{64}|run-[a-z0-9-]+-[a-f0-9]{64}))$/.test(path)) asset += ".json";
  else if (path === "/") asset = "/index.html";
  else if (!path.split("/").at(-1).includes(".")) asset += ".html";
  const response = await env.ASSETS.fetch(new Request(ORIGIN + asset));
  if (!response.ok) {
    const field=path.match(/^\/api\/v1\/fields\/(field-[a-f0-9]{64})(?:\.json)?$/);
    if(field && env.PUBLICATIONS) {
      const stored=await env.PUBLICATIONS.get(`fields/${field[1]}`);
      if(stored && namesWithdrawn(stored, await withdrawnShas(env))) return WITHDRAWN();
      if(stored) return new Response(stored,{headers:{'Content-Type':'application/json; charset=utf-8','Cache-Control':'public, max-age=31536000, immutable'}});
    }
    const dataset = path.match(/^\/api\/v1\/benchmarks\/(dataset-[a-f0-9]{64})(?:\.json)?$/);
    if (dataset && env.PUBLICATIONS) {
      const stored = await env.PUBLICATIONS.get(`datasets/${dataset[1]}`);
      if (stored != null && namesWithdrawn(stored, await withdrawnShas(env))) return WITHDRAWN();
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
    // A person who mistypes a page gets the site's 404 page; API clients keep the JSON error.
    const reqPath = new URL(request.url).pathname;
    if (response.status === 404 && !reqPath.startsWith("/api/") && (request.headers.get("Accept") || "").includes("text/html")) {
      const page = await env.ASSETS.fetch(new Request(`${ORIGIN}/404.html`));
      if (page.ok) response = new Response(page.body, { status: 404, headers: { "Content-Type": "text/html; charset=utf-8" } });
    }
    const headers = new Headers(response.headers);
    headers.set("Access-Control-Allow-Origin", "*");
    headers.set("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
    headers.set("Access-Control-Allow-Headers", "Accept, If-None-Match, If-Modified-Since");
    headers.set("Access-Control-Expose-Headers", "ETag, Last-Modified, Link");
    headers.set("X-Content-Type-Options", "nosniff");
    if (new URL(request.url).hostname === "onthebench.ai") headers.set("Strict-Transport-Security", "max-age=31536000");
    if (headers.get("Content-Type") === "text/html") headers.set("Content-Type", "text/html; charset=utf-8");
    headers.set("Link", `<${ORIGIN}/api/v1/summary>; rel="alternate"; type="application/json", <${ORIGIN}/llms.txt>; rel="describedby"; type="text/plain", <${ORIGIN}/api/v1>; rel="service-desc"; type="application/json"`);
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
