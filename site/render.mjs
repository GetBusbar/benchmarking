// SPDX-License-Identifier: Apache-2.0
// A DOM implementation at publication/request time; never shipped to the browser.
import { parseHTML } from "linkedom/worker";
import browserApp from "./app.js";

export const ORIGIN = "https://onthebench.ai";
export const esc = (value) => String(value ?? "").replace(/[&<>"']/g,
  c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
export const jsonScript = value => JSON.stringify(value).replace(/</g, "\\u003c");

function dom(html) {
  const { document, HTMLSelectElement } = parseHTML(html);
  // linkedom exposes the standard select getter but not its setter.
  Object.defineProperty(HTMLSelectElement.prototype, "value", {
    configurable: true,
    get() { return [...this.options].find(o => o.hasAttribute("selected"))?.value ?? this.options[0]?.value ?? ""; },
    set(value) { for (const option of this.options) option.toggleAttribute("selected", option.value === String(value)); },
  });
  return document;
}

export function shellViews(template) { return [...dom(template).querySelectorAll("main > .view")].map(n => n.outerHTML); }

export function metadata(document, path, title, description, structured, markdown) {
  document.title = title;
  for (const [selector, attr, value] of [
    ['link[rel="canonical"]', "href", ORIGIN + path],
    ['meta[name="description"]', "content", description],
    ['meta[property="og:title"]', "content", title],
    ['meta[property="og:description"]', "content", description],
    ['meta[property="og:url"]', "content", ORIGIN + path],
    ['meta[name="twitter:title"]', "content", title],
    ['meta[name="twitter:description"]', "content", description],
  ]) document.querySelector(selector)?.setAttribute(attr, value);
  document.querySelectorAll('[data-publication]').forEach(n => n.remove());
  document.head.insertAdjacentHTML("beforeend",
    `<link data-publication rel="alternate" type="text/plain" href="${esc(ORIGIN + markdown.replace(/\.md(?=\?|$)/, ".txt"))}">` +
    `<link data-publication rel="alternate" type="text/markdown" href="${esc(ORIGIN + markdown)}">` +
    `<link data-publication rel="alternate" type="application/json" href="${ORIGIN}/api/v1/summary">` +
    `<link data-publication rel="service-desc" type="application/json" href="${ORIGIN}/openapi.json"><link data-publication rel="describedby" type="text/plain" href="${ORIGIN}/llms.txt"><link data-publication rel="sitemap" type="application/xml" href="${ORIGIN}/sitemap.xml"><link data-publication rel="describedby" type="text/plain" href="${ORIGIN}/robots.txt"><link data-publication rel="help" href="${ORIGIN}/gateways/data"><meta data-publication property="og:image" content="${ORIGIN}/social-card.png">` +
    `<script data-publication type="application/ld+json">${jsonScript(structured)}</script>` +
    `<script data-publication type="application/ld+json">${jsonScript(siteIdentity(path, title))}</script>`);
}

// Who publishes the site, what the site is, and where this page sits in it. Kept in its own block so
// the page's primary record (a Dataset, DataCatalog or WebPage) stays the first JSON-LD block.
export function siteIdentity(path, fullTitle) {
  const title = fullTitle.replace(/ \| On the Bench$/, "");
  const org = {
    "@type": "Organization", "@id": ORIGIN + "/#organization", name: "Busbar", url: "https://getbusbar.com",
    logo: { "@type": "ImageObject", url: ORIGIN + "/favicon.svg" }, image: ORIGIN + "/social-card.png",
    description: "The Busbar team operates On the Bench and also enters its own gateway in the benchmarks.",
    sameAs: ["https://github.com/GetBusbar"],
  };
  const site = { "@type": "WebSite", "@id": ORIGIN + "/#website", name: "On the Bench", url: ORIGIN + "/",
    description: "Reproducible AI gateway measurements.", publisher: { "@id": org["@id"] }, inLanguage: "en" };
  const parts = path.split("/").filter(Boolean);
  const crumbs = [{ name: "On the Bench", url: ORIGIN + "/" },
    ...parts.map((p, i) => ({ name: i === parts.length - 1 ? title : p.charAt(0).toUpperCase() + p.slice(1), url: ORIGIN + "/" + parts.slice(0, i + 1).join("/") }))];
  const breadcrumb = { "@type": "BreadcrumbList", "@id": ORIGIN + path + "#breadcrumb",
    itemListElement: crumbs.map((c, i) => ({ "@type": "ListItem", position: i + 1, name: c.name, item: c.url })) };
  const page = { "@type": "WebPage", "@id": ORIGIN + path + "#webpage", url: ORIGIN + path, name: title,
    isPartOf: { "@id": site["@id"] }, publisher: { "@id": org["@id"] }, breadcrumb: { "@id": breadcrumb["@id"] }, inLanguage: "en" };
  return { "@context": "https://schema.org", "@graph": [org, site, page, breadcrumb] };
}

export function renderDashboard(template, data, path, publication) {
  const document = dom(template);
  const app = browserApp.createApp(document);
  const state = app.renderForPublication(data, path);
  const url = new URL(path, ORIGIN);
  if (data.active_field?.status === 'under_review') {
    document.querySelector("main")?.insertAdjacentHTML("afterbegin", `<aside class="refresh-status" role="status"><strong>Provisional field — under review.</strong> ${esc(data.active_field.notice || 'Measurements are under review.')} <a href="/gateways/runs">Retained run evidence</a>.</aside>`);
  } else if (data.active_field && data.gateways.some(g => !g.matrix)) {
    const completed = data.gateways.filter(g => g.matrix).length;
    document.querySelector("main")?.insertAdjacentHTML("afterbegin", `<aside class="refresh-status" role="status"><strong>Benchmark refresh: ${completed} of ${data.gateways.length} results published.</strong> Unfinished entrants show n/a. Results appear after validation. <a href="/gateways/runs">Previous runs remain in History</a>.</aside>`);
  }
  // Keep the exact visual layout. Ordinary assistive links make every row an
  // independently retrievable entity without changing the existing repo links.
  for (const row of document.querySelectorAll("tr[data-gw]")) {
    const cell = row.querySelector("td.name") || row.querySelector("td");
    cell?.insertAdjacentHTML("beforeend", `<a class="sr-only" href="/gateways/${encodeURIComponent(row.dataset.gw)}">${esc(row.dataset.gw)} benchmark evidence</a>`);
  }
  const empty = document.getElementById("matrix-empty");
  if (empty?.classList.contains("hidden")) empty.textContent = "";
  else if (empty && data.active_field?.status === 'under_review') empty.textContent = "This provisional field was halted for measurement-engine corrections.";
  else if (empty && data.active_field) empty.textContent = "Refresh in progress. Protocol measurements will appear as validated runs finish.";
  document.querySelectorAll("[data-wired]").forEach(n => n.removeAttribute("data-wired"));
  // A crawler must be able to read a protocol verdict without interpreting color
  // or relying on an aria attribute that its text extractor might discard.
  for (const cell of document.querySelectorAll(".cell[data-gw]")) {
    const gateway = data.gateways.find(g => g.key === cell.dataset.gw);
    const record = gateway?.matrix?.upstreams?.[cell.dataset.egress]?.cells?.[cell.dataset.cell];
    const label = cell.getAttribute("aria-label") || `${cell.dataset.cell} → ${cell.dataset.egress}: ${app.cellState(record || {})[1]}`;
    cell.innerHTML = `<span class="sr-only">${esc(label)}</span>`;
  }
  // Older production bundles predate the canvas's accessible data table. The
  // shared chart selector supplies it without deriving a second set of values.
  if (!document.getElementById("chart-canvas-data")) {
    const metric = app.CHART_METRICS.find(m => m.id === state.chartMetric) || app.CHART_METRICS[0];
    const rows = app.chartRows(metric, app.applyFilters(data.gateways, state), state);
    document.getElementById("chart-panel")?.insertAdjacentHTML("beforeend", `<table class="sr-only" id="chart-canvas-data"><caption>${esc(metric.label)}</caption><thead><tr><th>Gateway</th><th>Value</th><th>Unit</th></tr></thead><tbody>${rows.map(r => `<tr><td>${esc(r.name)}</td><td>${esc(r.v)}</td><td>${esc(metric.unit)}</td></tr>`).join("")}</tbody></table>`);
  }
  const method = document.querySelector("#view-method article");
  if (method && publication) {
    const runs = [...new Map(publication.measurements.filter(m => m.run_id).map(m => [m.run_id, m])).values()];
    method.insertAdjacentHTML("afterbegin", `<section id="recorded-methodology"><h2>Methodology recorded with this collection</h2><p>Measured ${esc(publication.measured_range.earliest)} through ${esc(publication.measured_range.latest)}. Collection generated ${esc(publication.generated_at)}. The essay below describes the design and its history; recorded definitions and pinned harness versions govern each numerical claim.</p><details><summary>Versioned definitions and instrument sources</summary>${runs.map(m => `<h3>${esc(m.gateway_id)} — ${esc(m.measured_at)}</h3><p><a href="${esc(m.provenance.run_url)}">Immutable run</a> · Harness ${esc(m.test.harness_commit || "not recorded")}${m.test.harness_commit && /^[a-f0-9]{7,40}$/.test(m.test.harness_commit) ? ` · <a href="https://github.com/GetBusbar/benchmarking/tree/${esc(m.test.harness_commit)}/engine">Pinned source</a>` : ""}</p><dl>${Object.entries(m.test.definitions || {}).map(([key,value]) => `<dt>${esc(key)}</dt><dd>${esc(value)}</dd>`).join("") || "<dd>Definitions not recorded; consult raw evidence.</dd>"}</dl>`).join("")}</details></section>`);
  }
  const title = state.view === "gateways"
    ? "AI Gateway Benchmarks: Latency, Throughput & Protocol Support | On the Bench"
    : app.pageTitle(state);
  const markdown = url.pathname === "/" ? "/index.md" : `${url.pathname === "/gateways" ? "/gateways/index" : url.pathname}.md${url.search}`;
  metadata(document, url.pathname, title,
    `${title}. ${state.view === "memory" ? "Per-cell idle, loaded and recovery memory with recorded conditions." : state.view === "charts" ? "Charts with accessible data tables for the selected metric." : state.view === "method" ? "Versioned instrument definitions, provenance and comparison limitations." : state.view === "matrix" ? "Per-protocol support states and probe evidence." : "Protocol-selected gateway measurements against a local mock; not model inference latency."}`,
    datasetMetadata(publication, url.pathname), markdown);
  document.head.insertAdjacentHTML("beforeend", '<style data-publication>.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}</style>');
  document.querySelectorAll("canvas").forEach(n => { n.setAttribute("role", "img"); n.setAttribute("aria-label", "Benchmark chart; equivalent values in the associated data table"); n.setAttribute("aria-describedby", "chart-canvas-data"); });
  document.documentElement.setAttribute("data-prerendered", "true");
  document.documentElement.setAttribute("data-thin-publication", "true");
  // Keep only the selected view in crawler HTML. The browser restores the small
  // unrendered view templates before initializing its existing controls.
  document.querySelectorAll("main > .view.hidden").forEach(n => n.remove());
  document.querySelectorAll("select option").forEach(n => { if (!n.hasAttribute("selected")) n.remove(); });
  if (url.pathname === "/") {
    const rows = publication.gateways.map(g => publication.measurements.find(m => m.gateway_id === g.id && m.test.ingress_protocol === "openai" && m.test.upstream_protocol === "openai") || { gateway_id:g.id, metrics:{}, status:{state:"not_measured"}, measured_at:null });
    document.querySelector("main").insertAdjacentHTML("beforeend", `<section id="home-measurements"><h2>Published OpenAI → OpenAI measurements</h2><p>Gateway overhead against a local mock. Independent runs; compare configurations before drawing ratios.</p><table><thead><tr><th>Gateway</th><th>Added p99 (µs)</th><th>State</th><th>Measured at</th></tr></thead><tbody>${rows.map(m => `<tr><td><a href="/gateways/${esc(m.gateway_id)}">${esc(m.gateway_id)}</a></td><td>${esc(m.metrics.added_latency_p99_us?.value ?? "n/a")}</td><td>${esc(m.metrics.added_latency_p99_us?.state || m.status.state)}</td><td>${esc(m.measured_at)}</td></tr>`).join("")}</tbody></table></section>`);
  }
  document.querySelector("main").insertAdjacentHTML("afterbegin", `<h1 class="sr-only">${esc(title)}</h1><noscript><p class="muted">Published results are available below. <a href="/gateways/data">Browse all data and evidence</a>.</p><p class="muted"><strong>Comparison warning:</strong> Default rows use each gateway’s representative protocol cell, which can differ between gateways. This is not a like-for-like ranking. Select the same ingress and upstream protocols and check the <a href="/gateways/data">comparison evidence</a> before naming a winner.</p><style>canvas{display:none}#chart-canvas-data{position:static;width:auto;height:auto;clip:auto;clip-path:none;overflow:visible;white-space:normal}</style></noscript>`);
  document.querySelector("main").insertAdjacentHTML("beforeend",
    resourceLinks());
  return document.toString();
}

export function datasetMetadata(publication, path = "/gateways") {
  return {
    "@context": "https://schema.org", "@type": "Dataset",
    "@id": publication.immutable_url || ORIGIN + "/api/v1/benchmarks/" + publication.dataset_id,
    name: "On the Bench AI Gateway Benchmarks", url: ORIGIN + path,
    description: "Gateway measurements against a local mock upstream. Independent runs; compare only matched test conditions. Operated by the Busbar team, also a benchmark participant.",
    license: publication.license, creator: { "@type": "Organization", name: "Busbar team", "@id": ORIGIN + "/#organization" }, measurementTechnique: ORIGIN + "/gateways/method", isAccessibleForFree: true,
    ...(publication.measured_range?.latest ? { temporalCoverage: `${publication.measured_range.earliest}/${publication.measured_range.latest}` } : {}),
    distribution: [{ "@type": "DataDownload", encodingFormat: "application/json", contentUrl: ORIGIN + "/api/v1/benchmarks/latest" },
      { "@type": "DataDownload", encodingFormat: "text/csv", contentUrl: ORIGIN + "/gateways/data.csv?compact=1" }],
  };
}


function resourceLinks() {
  return `<div class="publication-resources"><nav class="data-links" aria-label="Benchmark data"><a href="/gateways/data">Data and API documentation</a><a href="/api/v1">API discovery</a><a href="/api/v1/summary">Compact results</a><a href="/llms.txt">Agent guide</a><a href="/gateways/runs">Historical measurements</a></nav><details class="crawler-links"><summary>Crawler files</summary><nav aria-label="Crawler discovery"><a href="${ORIGIN}/robots.txt">robots.txt</a> · <a href="${ORIGIN}/sitemap.xml">sitemap.xml</a> · <a href="${ORIGIN}/llms.txt">llms.txt</a></nav></details></div>`;
}

export function renderArticle(template, { path, title, body, markdown, structured }) {
  const document = dom(template);
  document.body.classList.remove("home");
  document.getElementById("catnav").innerHTML = '<span class="catnav-label">Benchmarking</span><a class="cat active" href="/gateways">Gateways</a>';
  document.querySelector("main").innerHTML = `<article class="publication-article">${body}</article>${resourceLinks()}`;
  document.querySelectorAll('script[src="/app.js"]').forEach(n => n.setAttribute("src", "/article.js"));
  document.querySelectorAll(".tab").forEach(n => n.classList.remove("active"));
  for (const id of ["backdrop", "drawer", "compare-panel", "compare-bar"]) document.getElementById(id)?.remove();
  document.getElementById("repo-link")?.setAttribute("href", "https://github.com/GetBusbar/benchmarking");
  metadata(document, path, `${title} | On the Bench`, title,
    structured || { "@context": "https://schema.org", "@type": "WebPage", name: title, url: ORIGIN + path }, markdown || `${path}.md`);
  return document.toString();
}

// Explicit Markdown is a representation of the same document, including tables.
// Hidden interactive views are excluded; accessible chart/measurement text stays.
export function htmlToMarkdown(html) {
  const document = dom(html);
  document.querySelectorAll("script,style,header.site-header,footer,.view.hidden,#drawer,#compare-panel,button,input,select,canvas").forEach(n => n.remove());
  function visit(n) {
    if (n.nodeType === 3) return n.textContent.replace(/\s+/g, " ");
    if (n.nodeType !== 1) return "";
    const content = () => [...n.childNodes].map(visit).join("");
    const tag = n.localName;
    if (tag === "svg") return " " + (n.getAttribute("aria-label") || [...n.querySelectorAll("title")].map(t => t.textContent).join("; ")) + " ";
    if (tag === "a") return n.getAttribute("href") ? ` [${content().trim()}](${new URL(n.getAttribute("href"), ORIGIN).href}) ` : content();
    if (/^h[1-6]$/.test(tag)) return `\n\n${"#".repeat(Number(tag[1]))} ${content().trim()}\n\n`;
    if (tag === "table") {
      const rows = [...n.querySelectorAll("tr")].map(row => [...row.querySelectorAll("th,td")].map(cell => visit(cell).trim().replace(/\|/g, "\\|").replace(/\n/g, " ")));
      if (!rows.length) return "";
      const width = Math.max(...rows.map(r => r.length));
      const line = row => `| ${Array.from({ length: width }, (_, i) => row[i] || "").join(" | ")} |`;
      return `\n\n${n.querySelector("caption")?.textContent || ""}\n${line(rows[0])}\n${line(Array(width).fill("---"))}\n${rows.slice(1).map(line).join("\n")}\n\n`;
    }
    if (tag === "pre") return `\n\n\`\`\`\n${n.textContent}\n\`\`\`\n\n`;
    if (tag === "code") return `\`${content()}\``;
    if (tag === "br") return "\n";
    if (tag === "li") return `\n- ${content().trim()}`;
    return ["p", "div", "section", "article", "nav", "details", "summary"].includes(tag) ? `\n\n${content().trim()}\n\n` : content();
  }
  return `${document.querySelector("main h1") ? "" : "# " + document.title + "\n\n"}${visit(document.querySelector("main") || document.body).replace(/\n{3,}/g, "\n\n").trim()}\n`;
}
