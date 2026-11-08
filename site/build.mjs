#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// One checked input bundle, one deployable directory. Never upload source/tests.
import { readFileSync, writeFileSync, mkdirSync, readdirSync, copyFileSync, existsSync, rmSync } from "node:fs";
import { resolve, join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";
import { renderDashboard, renderArticle, htmlToMarkdown, esc, datasetMetadata, shellViews } from "./render.mjs";
import { projectMeasurements, measurementsCsv, compareMeasurements, DISCLOSURE, POLICY, ORIGIN, freshness, compactSummary, compactRecords } from "./public-data.mjs";
import { measurementTable, gatewayArticle, comparisonArticle, dataArticle, schemaIndex, measurementSchema, openapi, disclosureHtml } from "./evidence.mjs";

import { semanticGuide, retrievalGuide, compactCsv, DATA_LICENSE, gatewayFamily } from "./retrieval.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
function git(root, args) { return execFileSync("git", ["-C", root, ...args], { maxBuffer: 256 * 1024 * 1024 }); }

export function build({ root = resolve(HERE, ".."), out = join(HERE, "dist"), generate = true, history = true, publishedAt = null } = {}) {
  root = resolve(root); out = resolve(out);
  if (out === root || out === join(root, "site")) throw new Error("Build output must be a separate artifact directory");
  if (existsSync(join(out, ".onthebench-build"))) rmSync(out, { recursive: true });
  else if (existsSync(out) && readdirSync(out).length) throw new Error("Use an empty output directory; source and previous unrelated files are never overwritten");
  mkdirSync(out, { recursive: true });
  // OUT is explicit so local checks can keep all generated files under task tmp.
  if (generate) {
    execFileSync(process.execPath, [join(root, "site/gen-data.mjs"), root, out], { stdio: "inherit", env: { ...process.env, OTB_SINGLE_ENGINE: "1" } });
    execFileSync(process.execPath, [join(root, "site/check-consistency.mjs"), join(out, "data.json")], { stdio: "inherit" });
  }
  // gen-data replaces its output directory, so record ownership afterwards.
  writeFileSync(join(out, ".onthebench-build"), "generated artifact\n");
  const data = JSON.parse(readFileSync(join(out, "data.json"), "utf8"));
  // gen-data still supports the old Pages shell. This artifact has real routes
  // and must never inherit its soft-200 catch-all.
  rmSync(join(out, "_redirects"), { force: true });
  const template = readFileSync(join(root, "site/index.html"), "utf8");
  const routes = [];
  const write = (path, body) => { const dest = join(out, path.replace(/^\//, "")); mkdirSync(dirname(dest), { recursive: true }); writeFileSync(dest, body); };
  const json = (path, value) => write(path, JSON.stringify(value) + "\n");
  const page = (path, html) => {
    const file = path === "/" ? "/index.html" : `${path}.html`;
    write(file, html);
    const md = path === "/" ? "/index.md" : path === "/gateways" ? "/gateways/index.md" : `${path}.md`;
    let markdown = htmlToMarkdown(html);
    if (["/gateways/performance", "/gateways/memory", "/gateways/frontier", "/gateways/streaming", "/gateways/charts"].includes(path)) {
      const view = path.split("/").at(-1);
      const records = publication.measurements.filter(m => m.test.ingress_protocol === "openai" && m.test.upstream_protocol === "openai");
      const body = records.map(m => {
        const metrics = Object.fromEntries(Object.entries(m.metrics).filter(([key]) => view === "memory" ? /rss|memory/.test(key) : view === "streaming" ? /ttft|stream/.test(key) : /latency/.test(key)));
        return `<h2>${esc(m.gateway_id)}</h2><p>Build: ${esc(m.gateway_build)}. Measured: ${esc(m.measured_at)}. Published: ${esc(publication.published_at ?? "not recorded")}. ${esc(freshness(m.measured_at, publication.generated_at).age)}.</p>${measurementTable([{ ...m, metrics, frontier: view === "frontier" || view === "performance" ? m.frontier : [] }])}<p><a href="${esc(m.provenance.run_url)}">Run evidence and recorded definitions</a> · <a href="${esc(m.provenance.raw_result_url)}">Raw record</a></p>`;
      }).join("");
      markdown = htmlToMarkdown(`<html><body><main><h1>${esc(view)} — OpenAI → OpenAI</h1><p>Independent historical runs against a local mock. Compare only compatible configurations. Use the API for other protocol pairs.</p>${body}<p><a href="${ORIGIN}/gateways/method">Methodology</a> · <a href="${ORIGIN}/api/v1/definitions">Metric definitions</a></p></main></body></html>`);
    }
    write(md, markdown);
    write(md.replace(/\.md$/, ".txt"), markdown);
    routes.push({ path, file, markdown: md });
  };
  for (const name of ["app.js", "style.css", "favicon.svg", "article.js", "social-card.png"]) copyFileSync(join(root, "site", name), join(out, name));
  // Private build inputs are fetched only through the ASSETS binding. The Worker
  // denies public /_publication requests.
  write("/_publication/template.html", template);
  json("/ui-shell.json", shellViews(template));
  const archives = [], byFile = new Map(), byHash = new Map();
  function archive(file, bytes, current = false) {
    const snapshot = JSON.parse(bytes.toString("utf8"));
    if (!snapshot.gateway || !snapshot.matrix) return;
    if (!/^[a-z0-9][a-z0-9-]*$/.test(snapshot.gateway)) throw new Error(`Unsafe gateway ID in ${file}`);
    const sha256 = hash(bytes), id = `run-${snapshot.gateway}-${sha256}`;
    let item = byHash.get(sha256);
    if (!item) {
      item = { id, sha256, gateway_id: snapshot.gateway, measured_at: snapshot.measured_at ?? null, build: snapshot.build ?? null };
      const compressed = gzipSync(bytes, { level: 9 });
      write(`/api/v1/runs/${id}/raw.json.gz`, compressed);
      const summary = { schema_version: "1.0", ...item, raw_result_url: `${ORIGIN}/api/v1/runs/${id}/raw.json`, source_url: `${ORIGIN}/gateways/runs/${id}`, disclosure: DISCLOSURE };
      json(`/api/v1/benchmarks/${id}.json`, summary);
      page(`/gateways/runs/${id}`, renderArticle(template, {
        path: `/gateways/runs/${id}`, title: `${snapshot.gateway} — ${snapshot.measured_at || "undated run"}`,
        structured: { "@context": "https://schema.org", "@type": "Dataset", license: DATA_LICENSE, name: `${snapshot.gateway} benchmark run`, url: summary.source_url, distribution: { "@type": "DataDownload", encodingFormat: "application/json", contentUrl: summary.raw_result_url } },
        body: `<h1>${esc(snapshot.gateway)} benchmark run</h1>${disclosureHtml}<p>Measured ${esc(snapshot.measured_at)}. Recorded build: ${esc(snapshot.build)}.</p><p>Hardware: ${esc(snapshot.hardware)}. Architecture: ${esc(snapshot.arch)}.</p><p>SHA-256 of exact raw JSON bytes: <code>${sha256}</code>.</p><p><a href="${summary.raw_result_url}">Full immutable raw result, all protocol cells and samples</a> · <a href="/api/v1/benchmarks/${id}">Run metadata JSON</a> · <a href="/gateways/${snapshot.gateway}">Current gateway evidence</a></p><h2>Recorded metric definitions</h2><dl>${Object.entries(snapshot.definitions || {}).map(([k,v]) => `<dt>${esc(k)}</dt><dd>${esc(v)}</dd>`).join("")}</dl><h2>Instrument provenance</h2><pre>${esc(JSON.stringify(snapshot.rig ?? null, null, 2))}</pre>`,
      }));
      archives.push(item); byHash.set(sha256, item);
    }
    if (current) byFile.set(basename(file), { ...item, snapshot });
  }
  const dir = join(root, "results/snapshots");
  if (existsSync(dir)) for (const file of readdirSync(dir).filter(f => f.endsWith(".json")).sort()) archive(file, readFileSync(join(dir, file)), true);
  // Preserve earlier bytes even when a timestamped file was corrected or removed.
  // CI fetches full history. A source archive without .git can still build its
  // supplied snapshots, but cannot pretend to have a historical inventory.
  if (history) {
    const shallow = git(root, ["rev-parse", "--is-shallow-repository"]).toString().trim();
    if (shallow === "true") throw new Error("Fetch full Git history before publishing immutable runs");
    const objects = git(root, ["rev-list", "--objects", "HEAD", "--", "results/snapshots"]).toString().split("\n");
    for (const line of objects) {
      const [oid, ...parts] = line.split(" "); const path = parts.join(" ");
      if (/^results\/snapshots\/[^/]+\.json$/.test(path)) archive(path, git(root, ["cat-file", "blob", oid]));
    }
  }
  archives.sort((a,b) => (b.measured_at || "").localeCompare(a.measured_at || "") || a.id.localeCompare(b.id));
  let measurements = data.gateways.flatMap(g => projectMeasurements(g, byFile));
  const dates = measurements.map(m => m.measured_at).filter(Boolean).sort();
  const evidence = { schema_version: "1.0", collection_kind: "independent_gateway_runs", measurements, disclosure: DISCLOSURE, comparison_policy: POLICY };
  const datasetId = `dataset-${hash(JSON.stringify(evidence))}`;
  measurements = measurements.map(m => ({ ...m, ...freshness(m.measured_at, data.generated_at, publishedAt) }));
  const publication = {
    ...evidence, measurements, dataset_id: datasetId, generated_at: data.generated_at, published_at: publishedAt,
    measured_range: { earliest: dates[0] || null, latest: dates.at(-1) || null },
    metadata_as_of: data.gateways.map(g => g.stars_as_of).filter(Boolean).sort().at(-1) || null,
    license: DATA_LICENSE, publication_time_scope: "Release timestamp recorded when publishing begins; not the first historical publication of these measurements.", archive_history_included: history,
    source_url: `${ORIGIN}/gateways`, immutable_url: `${ORIGIN}/api/v1/benchmarks/${datasetId}`,
    gateways: data.gateways.map(g => ({ id: g.key, name: g.display, url: `${ORIGIN}/gateways/${g.key}`, json_url: `${ORIGIN}/api/v1/gateways/${g.key}`, current_metadata: { stars: g.stars ?? null, stars_as_of: g.stars_as_of ?? null, first_commit: g.first_commit ?? null } })),
    metrics: [...new Set(["throughput_rps", ...measurements.flatMap(m => Object.keys(m.metrics))])].sort(),
  };
  json(`/api/v1/benchmarks/${datasetId}.json`, { ...evidence, dataset_id: datasetId });
  json("/api/v1/benchmarks/latest.json", publication);
  json("/_publication/records.json", publication);
  const definitionCatalog = { schema_version: "1.0", dataset_id: datasetId, definitions: compactRecords(measurements).definitions };
  json("/api/v1/definitions.json", definitionCatalog);
  json(`/api/v1/definitions/${datasetId}.json`, definitionCatalog);
  json("/_publication/runs.json", archives.map(r => ({ ...r, gateway_family: gatewayFamily(r.gateway_id), raw_result_url: `${ORIGIN}/api/v1/runs/${r.id}/raw.json`, source_url: `${ORIGIN}/gateways/runs/${r.id}` })));
  json("/api/v1/summary.json", compactSummary(publication));
  json("/api/v1/gateways.json", { schema_version: "1.0", gateways: publication.gateways, disclosure: DISCLOSURE });
  json("/api/v1/index.json", { schema_version: "1.0", retrieval: retrievalGuide(Buffer.byteLength(JSON.stringify(publication) + "\n")), links: { runs: `${ORIGIN}/api/v1/runs`, definitions: `${ORIGIN}/api/v1/definitions`, summary: `${ORIGIN}/api/v1/summary`, gateways: `${ORIGIN}/api/v1/gateways`, latest: publication.source_url.replace("/gateways", "/api/v1/benchmarks/latest"), measurements: `${ORIGIN}/api/v1/measurements`, comparisons: `${ORIGIN}/api/v1/compare`, methodology: `${ORIGIN}/api/v1/methodology`, schema: `${ORIGIN}/api/v1/schema`, openapi: `${ORIGIN}/openapi.json` }, comparison_policy: POLICY, disclosure: DISCLOSURE });
  json("/api/v1/schema/index.json", schemaIndex());
  json("/api/v1/schema/measurement.json", measurementSchema());
  json("/openapi.json", openapi());
  for (const path of ["/", "/gateways", ...["memory", "performance", "frontier", "streaming", "matrix", "charts", "method"].map(v => `/gateways/${v}`)]) page(path, renderDashboard(template, data, path, publication));
  json("/api/v1/methodology.json", { schema_version: "1.0", url: `${ORIGIN}/gateways/method`, markdown: readFileSync(join(out, "gateways/method.md"), "utf8"), definitions: data.definitions ?? {}, comparison_policy: POLICY, disclosure: DISCLOSURE });
  for (const g of data.gateways) {
    const records = measurements.filter(m => m.gateway_id === g.key);
    const histories = archives.filter(r => gatewayFamily(r.gateway_id) === gatewayFamily(g.key));
    page(`/gateways/${g.key}/evidence`, renderArticle(template, { path: `/gateways/${g.key}/evidence`, title: `${g.display} complete benchmark evidence`, body: gatewayArticle(g, records, histories, data.gateways, { full: true }) }));
    json(`/api/v1/gateways/${g.key}.json`, { schema_version: "1.0", gateway: g, current_metadata: publication.gateways.find(item => item.id === g.key).current_metadata, metadata_scope: "gateway.stars and stars_as_of are legacy aliases of current_metadata, a dated repository snapshot, not stars at measurement time. Run statistics were not recorded.", measurements: records, history: histories, generated_at: data.generated_at, disclosure: DISCLOSURE });
    page(`/gateways/${g.key}`, renderArticle(template, { path: `/gateways/${g.key}`, title: `${g.display} benchmark measurements`, structured: { "@context": "https://schema.org", "@type": "Dataset", license: DATA_LICENSE, name: `${g.display} gateway measurements`, url: `${ORIGIN}/gateways/${g.key}`, creator: { "@type": "Organization", name: DISCLOSURE.operator }, about: { "@type": "SoftwareApplication", name: g.display, url: g.repo }, distribution: { "@type": "DataDownload", encodingFormat: "application/json", contentUrl: `${ORIGIN}/api/v1/gateways/${g.key}` } }, body: gatewayArticle(g, records, histories, data.gateways) }));
  }
  // Common comparison URLs cost only a static read, and are discoverable in the
  // sitemap. Both orders are useful: the ratio explicitly names its denominator.
  for (const a of data.gateways) for (const b of data.gateways) if (a.key !== b.key) {
    const comparison = compareMeasurements(measurements, { a: a.key, b: b.key });
    const pair = `${a.key}-vs-${b.key}`;
    json(`/api/v1/comparisons/${pair}.json`, comparison);
    page(`/gateways/compare/${pair}`, renderArticle(template, { path: `/gateways/compare/${pair}`, title: `${a.display} vs ${b.display}`, body: comparisonArticle(comparison) }));
  }
  // Retired entrants retain independently addressable history and evidence.
  for (const key of new Set(archives.map(r => r.gateway_id))) if (!data.gateways.some(g => g.key === key)) {
    const histories = archives.filter(r => r.gateway_id === key);
    page(`/gateways/${key}`, renderArticle(template, { path: `/gateways/${key}`, title: `${key} historical benchmarks`, body: `<h1>${esc(key)} historical benchmarks</h1>${disclosureHtml}<p>This gateway is not on the current board.</p><ul>${histories.map(r => `<li><a href="/gateways/runs/${r.id}">${esc(r.measured_at)} — ${esc(r.build)}</a></li>`).join("")}</ul>` }));
    json(`/api/v1/gateways/${key}.json`, { schema_version: "1.0", gateway: { key, current: false }, measurements: [], history: histories, disclosure: DISCLOSURE });
  }
  page("/gateways/data", renderArticle(template, { path: "/gateways/data", title: "Benchmark data and API", structured: { "@context": "https://schema.org", "@type": "DataCatalog", license: DATA_LICENSE, name: "On the Bench published gateway evidence", url: `${ORIGIN}/gateways/data`, creator: { "@type": "Organization", name: DISCLOSURE.operator }, dataset: publication.gateways.map(g => ({ "@type": "Dataset", license: DATA_LICENSE, name: g.name + " measurements", url: g.url, distribution: { "@type": "DataDownload", encodingFormat: "application/json", contentUrl: g.json_url } })) }, body: dataArticle(publication) }));
  page("/gateways/runs", renderArticle(template, { path: "/gateways/runs", title: "Historical benchmark runs", body: `<h1>Historical benchmark runs</h1>${disclosureHtml}<p><a href="/api/v1/runs">Paginated run catalog</a> · <a href="/api/v1/runs?family=busbar">All Busbar versions</a>. Gateway IDs identify tested variants; gateway_family groups their history.</p><ul>${archives.slice(0, 50).map(r => `<li><a href="/gateways/runs/${r.id}">${esc(r.gateway_id)} — ${esc(r.measured_at)} — ${esc(r.build)}</a></li>`).join("")}</ul>` }));
  write("/gateways/data.csv", compactCsv(measurements, measurements, datasetId));
  const index = `# On the Bench\n\n> Reproducible AI gateway measurements. Operated by the Busbar team, also an entrant.\n\nPublication generated ${data.generated_at}; newest measurement ${dates.at(-1) || "not recorded"}. Independent runs, not one simultaneous field experiment.\n\n## Resources\n\n- [Gateway benchmarks](${ORIGIN}/gateways)\n- [Markdown results](${ORIGIN}/gateways/performance.md)\n- [Data and API documentation](${ORIGIN}/gateways/data)\n- [Dataset reuse license](${ORIGIN}/gateways/license)\n- [Compact summary — start here](${ORIGIN}/api/v1/summary)\n- [Gateway catalog](${ORIGIN}/api/v1/gateways)\n- [API discovery](${ORIGIN}/api/v1)\n- [Latest dataset — large; prefer summary or paginated measurements](${ORIGIN}/api/v1/benchmarks/latest)\n- [Methodology](${ORIGIN}/gateways/method.md)\n- [Historical runs](${ORIGIN}/gateways/runs)\n- [OpenAPI](${ORIGIN}/openapi.json)\n\n## Gateways\n\n${publication.gateways.map(g => `- [${g.name}](${g.url}.md)`).join("\n")}\n\n## Retrieval alternatives\n\nMarkdown pages also have text/plain .txt twins. Use the HTML URL without .md if your tool rejects text/markdown.\n\n- [Compact CSV](${ORIGIN}/gateways/data.csv)\n- [Schema index](${ORIGIN}/api/v1/schema)\n\n## Example workflow\n\nCompare Busbar and Bifrost: read the gateway catalog, then [compare OpenAI to OpenAI p99 added latency](${ORIGIN}/api/v1/compare?a=busbar&b=bifrost&ingress=openai&egress=openai&metric=added_latency_p99_us). Inspect comparability.status before reporting a ratio; read measured_at, definitions, and provenance.run_url. Cite both runs and methodology with the numbers and caveats.\n\n## Agent guidance\n\n- The dashboard uses representative cells that can differ between gateways; do not rank those rows as like-for-like.\n- Choose explicit ingress and egress (upstream) protocols in /api/v1/measurements and /api/v1/compare.\n- Measurement pages are capped at 1 MiB of UTF-8 JSON. Always follow next until null, even when fewer than limit rows are returned. Use gateway, ingress, egress and metric filters to narrow retrieval.\n- Inspect comparability.status: only comparable supports a ratio. Missing evidence is not proof of comparability.\n- Distinguish measured_at from generated_at and published_at (unknown when null). Age is relative to age_as_of, not retrieval time. current_metadata contains dated repository statistics, not run-time statistics.\n- Never treat null, unavailable, or below-resolution values as zero.\n- Cite provenance.run_url with gateway_build, measured_at, the protocol pair, metric definition, and methodology. Verify raw_result_url against raw_sha256 when checking evidence.\n- [Measurement JSON Schema](${ORIGIN}/api/v1/schema/measurement.json)\n\n## Interpretation\n\n${POLICY.description}\n`;
  write("/llms.txt", index);
  write("/llms-full.txt", semanticGuide(publication));
  const licenseText = readFileSync(join(root, "DATA-LICENSE.md"), "utf8");
  page("/gateways/license", renderArticle(template, { path: "/gateways/license", title: "Dataset reuse and attribution", body: `<h1>Dataset reuse and attribution</h1>${licenseText.split(/\n\n/).filter(block => !block.startsWith("# ")).map(block => `<p>${esc(block).replace(/\*\*(.*?)\*\*/g, "<strong>$1</strong>")}</p>`).join("")}<p><a href="${DATA_LICENSE}">CC BY 4.0</a> · <a href="${DATA_LICENSE}legalcode">Legal terms</a></p>` }));
  write("/DATA-LICENSE.md", licenseText);
  write("/robots.txt", `User-agent: *\nAllow: /\nDisallow: /_publication/\nSitemap: ${ORIGIN}/sitemap.xml\n`);
  // No lastmod is preferable to an invented timestamp on every metadata refresh.
  write("/sitemap.xml", `<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${routes.map(r => `<url><loc>${ORIGIN}${esc(r.path)}</loc></url>`).join("")}</urlset>\n`);
  write("/404.html", renderArticle(template, { path: "/404", title: "Page not found", body: '<h1>Page not found</h1><p><a href="/gateways">Browse published gateway benchmarks</a>.</p>' }));
  json("/_publication/routes.json", routes);
  console.log(`publish: ${data.gateways.length} gateways, ${measurements.length} cells, ${archives.length} immutable runs, ${routes.length} HTML pages -> ${out}`);
  return { publication, routes, archives };
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  build({ root: process.argv[2] || resolve(HERE, ".."), out: process.argv[3] || join(HERE, "dist"), history: !process.argv.includes("--no-history") });
}
