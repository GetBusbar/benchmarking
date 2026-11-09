// SPDX-License-Identifier: Apache-2.0
// A DOM implementation at publication/request time; never shipped to the browser.
import { parseHTML } from "linkedom/worker";
import browserApp from "./app.js";

export const ORIGIN = "https://onthebench.ai";
export const esc = (value) => String(value ?? "").replace(/[&<>"']/g,
  c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
export const jsonScript = value => JSON.stringify(value).replace(/</g, "\\u003c");

// ---- HTML presentation of recorded values ------------------------------------------------------------
// These helpers change how a recorded value LOOKS on a human page and never what it is. The exact
// recorded value stays reachable on the element (title / datetime) and in `data-md`, which
// htmlToMarkdown emits verbatim, so the Markdown and text twins keep the engine's field names and values.
export const mdSpan = (html, raw) => `<span data-md="${esc(raw)}">${html}</span>`;
const PROTOCOL_NAMES = { openai: "OpenAI", "openai-responses": "OpenAI Responses", anthropic: "Anthropic", gemini: "Gemini", cohere: "Cohere", bedrock: "Bedrock" };
export const protocolName = p => PROTOCOL_NAMES[p] || String(p ?? "");
export const pairHtml = (ingress, egress) => mdSpan(`${esc(protocolName(ingress))} → ${esc(protocolName(egress))}`, `${ingress} → ${egress}`);
const METRIC_LABELS = {
  added_latency_p50_us: "Added latency p50 (µs)", added_latency_p99_us: "Added latency p99 (µs)",
  gateway_c1_p99_us: "Gateway p99 at concurrency 1 (µs)", direct_c1_p99_us: "Direct-to-mock p99 at concurrency 1 (µs)",
  cpu_us_per_request: "CPU per request (µs)", rps_per_cpu_second: "Requests per CPU-second",
  cost_window_conc: "Cost window concurrency", cost_core_utilisation: "Cost window core utilisation",
  cost_window_ok: "Cost window successful requests", cost_window_rps: "Cost window throughput (req/s)",
  cost_threads: "Cost window threads", cost_nonvol_ctxt_per_request: "Non-voluntary context switches per request",
  cost_majflt: "Cost window major page faults",
  added_ttft_p50_us: "Added time to first event p50 (µs)", added_ttft_p99_us: "Added time to first event p99 (µs)",
  added_gap_p50_us: "Added inter-frame gap p50 (µs)", added_gap_p99_us: "Added inter-frame gap p99 (µs)",
  streams_sustained: "Streams sustained", streams_sustained_fps: "Streams sustained (frames/s)",
  idle_rss_mib: "Idle RSS (MiB)", steady_state_rss_mib: "Steady-state RSS (MiB)", recovered_rss_mib: "Recovered RSS (MiB)",
  peak_rss_mib: "Peak RSS (MiB)", peak_rss_hwm_mib: "Peak RSS high-water mark (MiB)", time_to_plateau_s: "Time to plateau (s)",
  growth_rate_mib_per_min: "Memory growth (MiB/min)", load_s: "Memory load duration (s)", shape: "Memory shape under load",
  idle_shape: "Idle memory shape", idle_static: "Idle memory static", idle_growth_rate_mib_per_min: "Idle memory growth (MiB/min)",
  throughput_rps: "Throughput (req/s)",
};
const UNITS = [[/_mib_per_min$/, "MiB/min"], [/_us$/, "µs"], [/_ms$/, "ms"], [/_mib$/, "MiB"], [/_rps$/, "req/s"], [/_s$/, "s"]];
export function metricLabel(key) {
  const name = String(key ?? "");
  if (METRIC_LABELS[name]) return METRIC_LABELS[name];
  const unit = UNITS.find(([re]) => re.test(name));
  const words = (unit ? name.replace(unit[0], "") : name).replaceAll("_", " ");
  return words.charAt(0).toUpperCase() + words.slice(1) + (unit ? ` (${unit[1]})` : "");
}
export const metricLabelHtml = key => `<span data-md="${esc(key)}" title="${esc(key)}">${esc(metricLabel(key))}</span>`;
export const stateLabel = state => { const t = String(state ?? "").replaceAll("_", " "); return t.charAt(0).toUpperCase() + t.slice(1); };
const grouped = new Intl.NumberFormat("en-US");
export function formatNumber(v) {
  if (typeof v !== "number" || !Number.isFinite(v)) return String(v ?? "");
  if (Number.isInteger(v)) return grouped.format(v);
  const a = Math.abs(v);
  if (a < 0.001) return v.toPrecision(3);
  return new Intl.NumberFormat("en-US", { maximumFractionDigits: a >= 100 ? 1 : a >= 1 ? 2 : 3 }).format(v);
}
// A number keeps its exact recorded form on hover and in the Markdown twin.
export const numberHtml = (v, raw = v) => {
  const shown = formatNumber(v);
  return shown === String(raw) ? esc(shown) : `<span data-md="${esc(raw)}" title="${esc(v)}">${esc(shown)}</span>`;
};
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
// One human format everywhere ("9 Nov 2026, 05:55 UTC"); the exact ISO string stays in datetime/title.
export function formatTimestamp(iso) {
  if (typeof iso !== "string" || !iso) return null;
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return null;
  const day = `${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]} ${d.getUTCFullYear()}`;
  if (/^\d{4}-\d{2}-\d{2}$/.test(iso)) return day;
  const pad = n => String(n).padStart(2, "0");
  return `${day}, ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())} UTC`;
}
export const timeHtml = (iso, missing = "Not recorded") => {
  const shown = formatTimestamp(iso);
  return shown ? `<time datetime="${esc(iso)}" title="${esc(iso)}" data-md="${esc(iso)}">${esc(shown)}</time>` : esc(iso || missing);
};
// The engine's recorded prose uses ALL-CAPS lead-ins and Markdown code spans. Render them as sentence
// case and <code>; the words are unchanged and the verbatim text stays in data-md.
const ACRONYMS = new Set(["SSE", "CPU", "RSS", "TTFT", "HTTP", "HTTPS", "API", "ID", "OK", "TCP", "SHA", "JSON", "UTC", "MIB", "RPS", "IO", "OS", "LLM", "VMHWM", "P50", "P99", "AWS", "URL", "DNS", "TLS", "OOTB"]);
export function proseHtml(text) {
  const raw = String(text ?? "");
  const decap = segment => segment.replace(/\b[A-Z][A-Z0-9'’-]*(?:[ ,;:-]+[A-Z][A-Z0-9'’-]*)*\b/g, (run, offset, whole) => {
    const words = run.split(/([ ,;:-]+)/);
    const lower = w => /^[A-Z][A-Z0-9'’-]*$/.test(w) && !ACRONYMS.has(w.toUpperCase().replace(/[’']S$/, ""));
    if (!words.some(lower) || (words.length === 1 && run.length < 3)) return run;
    const sentenceStart = !whole.slice(0, offset).trim() || /[.!?:]\s*$/.test(whole.slice(0, offset));
    const lowered = words.map(w => lower(w) ? w.toLowerCase() : w).join("");
    return sentenceStart ? lowered.charAt(0).toUpperCase() + lowered.slice(1) : lowered;
  });
  const html = raw.split(/(`[^`]*`)/).map(part => part.startsWith("`") && part.endsWith("`") && part.length > 1
    ? `<code>${esc(part.slice(1, -1))}</code>` : esc(decap(part))).join("");
  return html === esc(raw) ? html : mdSpan(html, raw);
}

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
    // A bare WebPage record is already inside the site-identity graph; emitting it twice declares two pages.
    (structured?.["@type"] === "WebPage" ? "" : `<script data-publication type="application/ld+json">${jsonScript(structured)}</script>`) +
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
    // One row per run; the recorded definitions are listed once per distinct set rather than once per gateway.
    const names = new Map(publication.gateways.map(g => [g.id, g.name]));
    const sets = [...new Map(runs.map(m => [JSON.stringify(m.test.definitions || {}), m.test.definitions || {}])).values()];
    const definitionsHtml = defs => `<dl class="recorded-definitions">${Object.entries(defs).map(([key, value]) => `<dt><code>${esc(key)}</code></dt><dd>${proseHtml(value)}</dd>`).join("") || "<dd>Definitions not recorded; consult raw evidence.</dd>"}</dl>`;
    method.insertAdjacentHTML("afterbegin", `<section id="recorded-methodology"><h2>Methodology recorded with this collection</h2><p>Measured ${timeHtml(publication.measured_range.earliest)} through ${timeHtml(publication.measured_range.latest)}. Collection generated ${timeHtml(publication.generated_at)}. The essay below describes the design and its history; recorded definitions and pinned harness versions govern each numerical claim.</p><details><summary>Versioned definitions and instrument sources</summary><div class="table-scroll"><table><thead><tr><th>Gateway</th><th>Measured</th><th>Harness</th><th>Evidence</th></tr></thead><tbody>${runs.map(m => `<tr><td>${mdSpan(esc(names.get(m.gateway_id) || m.gateway_id), m.gateway_id)}</td><td>${timeHtml(m.measured_at)}</td><td>${m.test.harness_commit && /^[a-f0-9]{7,40}$/.test(m.test.harness_commit) ? `<a href="https://github.com/GetBusbar/benchmarking/tree/${esc(m.test.harness_commit)}/engine" title="${esc(m.test.harness_commit)}"><code>${esc(m.test.harness_commit.slice(0, 12))}</code></a>` : esc(m.test.harness_commit || "not recorded")}</td><td><a href="${esc(m.provenance.run_url)}">Immutable run</a></td></tr>`).join("")}</tbody></table></div>${sets.map((defs, i) => `<h3>Recorded metric definitions${sets.length > 1 ? ` (set ${i + 1} of ${sets.length}: ${runs.filter(m => JSON.stringify(m.test.definitions || {}) === JSON.stringify(defs)).map(m => esc(names.get(m.gateway_id) || m.gateway_id)).join(", ")})` : " (identical for every run above)"}</h3>${definitionsHtml(defs)}`).join("")}</details></section>`);
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
    // Home carries no performance table (index.html). This is the no-JavaScript and assistive equivalent
    // of the published data, laid out like the chart data table: visually hidden, shown under <noscript>.
    const names = new Map(publication.gateways.map(g => [g.id, g.name]));
    document.querySelector("main").insertAdjacentHTML("beforeend", `<section id="home-measurements" class="sr-only"><h2>Published OpenAI → OpenAI measurements</h2><p>Gateway overhead against a local mock. Independent runs; compare configurations before drawing ratios.</p><table><thead><tr><th>Gateway</th><th>Added p99 (µs)</th><th>State</th><th>Measured at</th></tr></thead><tbody>${rows.map(m => { const metric = m.metrics.added_latency_p99_us; return `<tr><td><a href="/gateways/${esc(m.gateway_id)}">${mdSpan(esc(names.get(m.gateway_id) || m.gateway_id), m.gateway_id)}</a></td><td>${metric?.value == null ? "n/a" : numberHtml(metric.value)}</td><td>${mdSpan(esc(stateLabel(metric?.state || m.status.state)), metric?.state || m.status.state)}</td><td>${timeHtml(m.measured_at, "")}</td></tr>`; }).join("")}</tbody></table></section>`);
  }
  document.querySelector("main").insertAdjacentHTML("afterbegin", `<h1 class="sr-only">${esc(title)}</h1><noscript><p class="muted">Published results are available below. <a href="/gateways/data">Browse all data and evidence</a>.</p><p class="muted"><strong>Comparison warning:</strong> Default rows use each gateway’s representative protocol cell, which can differ between gateways. This is not a like-for-like ranking. Select the same ingress and upstream protocols and check the <a href="/gateways/data">comparison evidence</a> before naming a winner.</p><style>canvas{display:none}#chart-canvas-data,#home-measurements{position:static;width:auto;height:auto;clip:auto;clip-path:none;overflow:visible;white-space:normal;margin:0}</style></noscript>`);
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
    // Presentation helpers keep the exact recorded value here (see mdSpan).
    if (n.hasAttribute("data-md")) return n.getAttribute("data-md");
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
    if (tag === "dt") return `\n\n**${content().trim()}**: `;
    if (tag === "dd") return `${content().trim()}\n`;
    return ["p", "div", "section", "article", "nav", "details", "summary"].includes(tag) ? `\n\n${content().trim()}\n\n` : content();
  }
  return `${document.querySelector("main h1") ? "" : "# " + document.title + "\n\n"}${visit(document.querySelector("main") || document.body).replace(/\n{3,}/g, "\n\n").trim()}\n`;
}
