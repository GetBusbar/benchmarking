#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// One checked input bundle, one deployable directory. Never upload source/tests.
import { readFileSync, writeFileSync, mkdirSync, readdirSync, copyFileSync, existsSync, rmSync } from "node:fs";
import { resolve, join, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { gzipSync } from "node:zlib";
import { renderDashboard, renderArticle, htmlToMarkdown, esc, datasetMetadata } from "./render.mjs";
import { projectMeasurements, measurementsCsv, compareMeasurements, DISCLOSURE, POLICY, ORIGIN } from "./public-data.mjs";
import { gatewayArticle, comparisonArticle, dataArticle, schemaIndex, measurementSchema, openapi, disclosureHtml } from "./evidence.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
function git(root, args) { return execFileSync("git", ["-C", root, ...args], { maxBuffer: 256 * 1024 * 1024 }); }

export function build({ root = resolve(HERE, ".."), out = join(HERE, "dist"), generate = true, history = true } = {}) {
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
    write(md, htmlToMarkdown(html));
    routes.push({ path, file, markdown: md });
  };
  for (const name of ["app.js", "style.css", "favicon.svg", "article.js"]) copyFileSync(join(root, "site", name), join(out, name));
  // Private build inputs are fetched only through the ASSETS binding. The Worker
  // denies public /_publication requests.
  write("/_publication/template.html", template);
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
        structured: { "@context": "https://schema.org", "@type": "Dataset", name: `${snapshot.gateway} benchmark run`, url: summary.source_url, distribution: { "@type": "DataDownload", encodingFormat: "application/json", contentUrl: summary.raw_result_url } },
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
  const measurements = data.gateways.flatMap(g => projectMeasurements(g, byFile));
  const dates = measurements.map(m => m.measured_at).filter(Boolean).sort();
  const evidence = { schema_version: "1.0", collection_kind: "independent_gateway_runs", measurements, disclosure: DISCLOSURE, comparison_policy: POLICY };
  const datasetId = `dataset-${hash(JSON.stringify(evidence))}`;
  const publication = {
    ...evidence, dataset_id: datasetId, generated_at: data.generated_at, published_at: null,
    measured_range: { earliest: dates[0] || null, latest: dates.at(-1) || null },
    metadata_as_of: data.gateways.map(g => g.stars_as_of).filter(Boolean).sort().at(-1) || null,
    license: null, archive_history_included: history,
    source_url: `${ORIGIN}/gateways`, immutable_url: `${ORIGIN}/api/v1/benchmarks/${datasetId}`,
    gateways: data.gateways.map(g => ({ id: g.key, name: g.display, url: `${ORIGIN}/gateways/${g.key}`, json_url: `${ORIGIN}/api/v1/gateways/${g.key}` })),
    metrics: [...new Set(["throughput_rps", ...measurements.flatMap(m => Object.keys(m.metrics))])].sort(),
  };
  json(`/api/v1/benchmarks/${datasetId}.json`, { ...evidence, dataset_id: datasetId });
  json("/api/v1/benchmarks/latest.json", publication);
  json("/_publication/records.json", publication);
  json("/api/v1/gateways.json", { schema_version: "1.0", gateways: publication.gateways, disclosure: DISCLOSURE });
  json("/api/v1/index.json", { schema_version: "1.0", links: { gateways: `${ORIGIN}/api/v1/gateways`, latest: publication.source_url.replace("/gateways", "/api/v1/benchmarks/latest"), measurements: `${ORIGIN}/api/v1/measurements`, comparisons: `${ORIGIN}/api/v1/compare`, methodology: `${ORIGIN}/api/v1/methodology`, schema: `${ORIGIN}/api/v1/schema`, openapi: `${ORIGIN}/openapi.json` }, comparison_policy: POLICY, disclosure: DISCLOSURE });
  json("/api/v1/schema/index.json", schemaIndex());
  json("/api/v1/schema/measurement.json", measurementSchema());
  json("/openapi.json", openapi());
  for (const path of ["/", "/gateways", ...["memory", "performance", "frontier", "streaming", "matrix", "charts", "method"].map(v => `/gateways/${v}`)]) page(path, renderDashboard(template, data, path, publication));
  json("/api/v1/methodology.json", { schema_version: "1.0", url: `${ORIGIN}/gateways/method`, markdown: readFileSync(join(out, "gateways/method.md"), "utf8"), definitions: data.definitions ?? {}, comparison_policy: POLICY, disclosure: DISCLOSURE });
  for (const g of data.gateways) {
    const records = measurements.filter(m => m.gateway_id === g.key);
    const histories = archives.filter(r => r.gateway_id === g.key);
    json(`/api/v1/gateways/${g.key}.json`, { schema_version: "1.0", gateway: g, measurements: records, history: histories, generated_at: data.generated_at, disclosure: DISCLOSURE });
    page(`/gateways/${g.key}`, renderArticle(template, { path: `/gateways/${g.key}`, title: `${g.display} benchmark measurements`, body: gatewayArticle(g, records, histories, data.gateways) }));
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
  page("/gateways/data", renderArticle(template, { path: "/gateways/data", title: "Benchmark data and API", body: dataArticle(publication) }));
  page("/gateways/runs", renderArticle(template, { path: "/gateways/runs", title: "Historical benchmark runs", body: `<h1>Historical benchmark runs</h1>${disclosureHtml}<ul>${archives.map(r => `<li><a href="/gateways/runs/${r.id}">${esc(r.gateway_id)} — ${esc(r.measured_at)} — ${esc(r.build)}</a></li>`).join("")}</ul>` }));
  write("/gateways/data.csv", measurementsCsv(measurements));
  const index = `# On the Bench\n\nReproducible AI gateway measurements. Operated by the Busbar team, also an entrant.\n\nPublication generated ${data.generated_at}; newest measurement ${dates.at(-1) || "not recorded"}. Independent runs, not one simultaneous field experiment.\n\n## Resources\n\n- [Gateway benchmarks](${ORIGIN}/gateways)\n- [Markdown results](${ORIGIN}/gateways/performance.md)\n- [Data and API documentation](${ORIGIN}/gateways/data)\n- [Latest dataset](${ORIGIN}/api/v1/benchmarks/latest)\n- [Methodology](${ORIGIN}/gateways/method.md)\n- [Historical runs](${ORIGIN}/gateways/runs)\n- [OpenAPI](${ORIGIN}/openapi.json)\n\n## Gateways\n\n${publication.gateways.map(g => `- [${g.name}](${g.url}.md)`).join("\n")}\n\n## Agent guidance\n\n- The dashboard uses representative cells that can differ between gateways; do not rank those rows as like-for-like.\n- Choose explicit ingress and egress (upstream) protocols in /api/v1/measurements and /api/v1/compare.\n- Inspect comparability.status: only comparable supports a ratio. Missing evidence is not proof of comparability.\n- Never treat null, unavailable, or below-resolution values as zero.\n- Cite provenance.run_url with gateway_build, measured_at, the protocol pair, metric definition, and methodology. Verify raw_result_url against raw_sha256 when checking evidence.\n- [Measurement JSON Schema](${ORIGIN}/api/v1/schema/measurement.json)\n\n## Interpretation\n\n${POLICY.description}\n`;
  write("/llms.txt", index);
  write("/llms-full.txt", index + "\n" + readFileSync(join(out, "gateways/performance.md"), "utf8") + "\n" + readFileSync(join(out, "gateways/method.md"), "utf8"));
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
