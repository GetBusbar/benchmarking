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
    `<link data-publication rel="alternate" type="text/markdown" href="${esc(ORIGIN + markdown)}">` +
    `<link data-publication rel="alternate" type="application/json" href="${ORIGIN}/api/v1/benchmarks/latest">` +
    `<script data-publication type="application/ld+json">${jsonScript(structured)}</script>`);
}

export function renderDashboard(template, data, path, publication) {
  const document = dom(template);
  const app = browserApp.createApp(document);
  const state = app.renderForPublication(data, path);
  const url = new URL(path, ORIGIN);
  // Keep the exact visual layout. Ordinary assistive links make every row an
  // independently retrievable entity without changing the existing repo links.
  for (const row of document.querySelectorAll("tr[data-gw]")) {
    const cell = row.querySelector("td.name") || row.querySelector("td");
    cell?.insertAdjacentHTML("beforeend", `<a class="sr-only" href="/gateways/${encodeURIComponent(row.dataset.gw)}">${esc(row.dataset.gw)} benchmark evidence</a>`);
  }
  const empty = document.getElementById("matrix-empty");
  if (empty?.classList.contains("hidden")) empty.textContent = "";
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
  const title = state.view === "gateways"
    ? "AI Gateway Benchmarks: Latency, Throughput & Protocol Support | On the Bench"
    : app.pageTitle(state);
  const markdown = url.pathname === "/" ? "/index.md" : `${url.pathname === "/gateways" ? "/gateways/index" : url.pathname}.md${url.search}`;
  metadata(document, url.pathname, title,
    `Reproducible AI gateway benchmarks. Measurement dates and protocol cells are disclosed; gateway overhead is not model response latency.`,
    datasetMetadata(publication, url.pathname), markdown);
  document.head.insertAdjacentHTML("beforeend", '<style data-publication>.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}</style>');
  document.documentElement.setAttribute("data-prerendered", "true");
  document.querySelector("main").insertAdjacentHTML("afterbegin", `<h1 class="sr-only">${esc(title)}</h1><noscript><p class="muted">Published results are available below. <a href="/gateways/data">Browse all data and evidence</a>.</p><p class="muted"><strong>Comparison warning:</strong> Default rows use each gateway’s representative protocol cell, which can differ between gateways. This is not a like-for-like ranking. Select the same ingress and upstream protocols and check the <a href="/gateways/data">comparison evidence</a> before naming a winner.</p><style>canvas{display:none}#chart-canvas-data{position:static;width:auto;height:auto;clip:auto;clip-path:none;overflow:visible;white-space:normal}</style></noscript>`);
  document.querySelector("main").insertAdjacentHTML("beforeend",
    `<nav class="sr-only" aria-label="Benchmark data"><a href="/gateways/data">Data and API documentation</a> <a href="/gateways/runs">Historical measurements</a></nav>`);
  return document.toString();
}

export function datasetMetadata(publication, path = "/gateways") {
  return {
    "@context": "https://schema.org", "@type": path === "/" ? "WebSite" : "Dataset",
    "@id": ORIGIN + path + (path === "/" ? "#website" : "#dataset"),
    name: "On the Bench AI Gateway Benchmarks", url: ORIGIN + path,
    description: "Gateway measurements against a local mock upstream. Independent runs; compare only matched test conditions. Operated by the Busbar team, also a benchmark participant.",
    creator: { "@type": "Organization", name: "Busbar team" }, isAccessibleForFree: true,
    ...(publication.measured_range?.latest ? { temporalCoverage: `${publication.measured_range.earliest}/${publication.measured_range.latest}` } : {}),
    distribution: [{ "@type": "DataDownload", encodingFormat: "application/json", contentUrl: ORIGIN + "/api/v1/benchmarks/latest" },
      { "@type": "DataDownload", encodingFormat: "text/csv", contentUrl: ORIGIN + "/gateways/data.csv" }],
  };
}

export function renderArticle(template, { path, title, body, markdown, structured }) {
  const document = dom(template);
  document.body.classList.remove("home");
  document.querySelector("main").innerHTML = `<article class="method">${body}</article>`;
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
    if (tag === "a") return n.getAttribute("href") ? `[${content().trim()}](${new URL(n.getAttribute("href"), ORIGIN).href})` : content();
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
  return `# ${document.title}\n\n${visit(document.querySelector("main") || document.body).replace(/\n{3,}/g, "\n\n").trim()}\n`;
}
