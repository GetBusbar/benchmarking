/* AI Gateway Benchmarks results site. Vanilla JS, no dependencies.
   Reads data.json (emitted by gen-data.mjs) and renders the four views. */
"use strict";

/* Language chip colours: kept in sync with LANG_COLORS in charts.py. */
const LANG_COLORS = {
  Rust: "#c4602d",
  Go: "#00a0c6",
  Python: "#3b6ea5",
  Node: "#c59b2d",
  Other: "#6b7280",
};

const fmtInt = (v) => Math.round(v).toLocaleString("en-US");
const fmt1 = (v) => v.toLocaleString("en-US", { minimumFractionDigits: 1, maximumFractionDigits: 1 });

/* Column model. get(g) returns {v, text, na} where v is the sortable number (null = no
   value), text is the rendered cell, and na marks a muted "not measured / not served" cell.
   laneLabel(j, flag, err): if the suite file exists but the served flag is false, surface the
   suite's own explicit label instead of a number; if the file is absent, "not measured". */
function lane(g, suite, flag, errKey, pick) {
  const j = g[suite];
  if (!j) return { v: null, text: "not measured", na: true };
  if (j[flag] === false) return { v: null, text: j[errKey] || "not served", na: true };
  return pick(j);
}

const COLUMNS = [
  {
    id: "name", label: "Gateway", desc: false,
    get: (g) => ({ v: g.display.toLowerCase(), text: null, na: false }),
    render: (g) => {
      const a = g.repo
        ? `<a href="${g.repo}" target="_blank" rel="noopener">${esc(g.display)}</a>`
        : esc(g.display);
      return `<td class="name">${a}</td>`;
    },
  },
  {
    id: "lang", label: "Lang", desc: false,
    get: (g) => ({ v: g.lang, text: null, na: false }),
    render: (g) => {
      const c = LANG_COLORS[g.lang] || LANG_COLORS.Other;
      return `<td><span class="lang-chip" style="background:${c}">${esc(g.lang)}</span></td>`;
    },
  },
  {
    id: "rps20", label: "Sustained RPS @20ms", desc: true, title: "Sustained requests/sec with a 20 ms mock LLM latency (p99 < 1 s, <0.1% errors)",
    get: (g) => lane(g, "perf", "served", "serve_error",
      (j) => ({ v: j.rps_sustained_20ms, text: fmtInt(j.rps_sustained_20ms), na: false })),
  },
  {
    id: "rpsmax", label: "Max proxy RPS", desc: true, title: "Throughput ceiling against an instant mock (p99 < 1 s, <0.1% errors)",
    get: (g) => lane(g, "perf", "served", "serve_error",
      (j) => ({ v: j.rps_max_proxy, text: fmtInt(j.rps_max_proxy), na: false })),
  },
  {
    id: "lat", label: "Added latency p99 (µs)", desc: false, title: "Gateway p99 minus direct-to-mock p99 at concurrency 1",
    get: (g) => lane(g, "perf", "served", "serve_error",
      (j) => ({ v: j.added_latency_p99_us, text: fmtInt(j.added_latency_p99_us), na: false })),
  },
  {
    id: "memidle", label: "Mem idle (MiB)", desc: false, title: "Process RSS after launch, before load",
    get: (g) => lane(g, "memory", "served", "serve_error",
      (j) => ({ v: j.idle_rss_mib, text: fmt1(j.idle_rss_mib), na: false })),
  },
  {
    id: "mempeak", label: "Mem peak (MiB)", desc: false, title: "Peak process RSS under large-payload load",
    get: (g) => lane(g, "memory", "served", "serve_error",
      (j) => ({ v: j.peak_rss_mib, text: fmt1(j.peak_rss_mib), na: false })),
  },
  {
    id: "sttft", label: "Stream added TTFT p99 (µs)", desc: false, title: "Gateway first-content-frame time minus direct-to-mock TTFT",
    get: (g) => lane(g, "stream", "stream_served", "stream_error",
      (j) => ({ v: j.stream_added_ttft_p99_us, text: fmtInt(j.stream_added_ttft_p99_us), na: false })),
  },
  {
    id: "sgap", label: "Stream added per-token p99 (µs)", desc: false, title: "Gateway content-frame gap minus direct-to-mock gap",
    get: (g) => lane(g, "stream", "stream_served", "stream_error",
      (j) => ({ v: j.stream_added_gap_p99_us, text: fmtInt(j.stream_added_gap_p99_us), na: false })),
  },
  {
    id: "streams", label: "Streams sustained", desc: true, title: "Max concurrent SSE streams with >=99.9% frame delivery, no stalls, <0.1% errors",
    get: (g) => lane(g, "stream", "stream_served", "stream_error",
      (j) => ({ v: j.stream_sustained_streams, text: fmtInt(j.stream_sustained_streams), na: false })),
  },
  {
    id: "xlate", label: "Xlate sustained RPS", desc: true, title: "Sustained RPS @20ms on the Anthropic-in / OpenAI-out translation path",
    get: (g) => lane(g, "xlate", "xlate_served", "xlate_error",
      (j) => ({ v: j.xlate_rps_sustained_20ms, text: fmtInt(j.xlate_rps_sustained_20ms), na: false })),
  },
  {
    id: "gov", label: "Governed overhead", desc: true, title: "Sustained-RPS change with native key/limit governance active vs the plain launch",
    get: (g) => lane(g, "governed", "governed_served", "governed_note",
      (j) => ({
        v: j.governed_vs_plain_sustained_pct,
        text: `${j.governed_vs_plain_sustained_pct > 0 ? "+" : ""}${j.governed_vs_plain_sustained_pct.toFixed(1)}%`,
        na: false,
      })),
  },
];

const state = {
  data: null,
  sortCol: "rps20",
  sortDesc: true,
  langs: new Set(),
  needStream: false,
  needXlate: false,
  needGoverned: false,
};

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

/* ---- results table ---------------------------------------------------------- */
function renderTable() {
  const { data } = state;
  const thead = document.querySelector("#results-table thead");
  const tbody = document.querySelector("#results-table tbody");

  thead.innerHTML = "<tr>" + COLUMNS.map((c) => {
    const sorted = state.sortCol === c.id;
    const dir = sorted ? `<span class="dir">${state.sortDesc ? " ▾" : " ▴"}</span>` : "";
    return `<th data-col="${c.id}" class="${sorted ? "sorted" : ""}" title="${esc(c.title || "")}">${esc(c.label)}${dir}</th>`;
  }).join("") + "</tr>";

  let rows = data.gateways.filter((g) => {
    if (state.langs.size && !state.langs.has(g.lang)) return false;
    if (state.needStream && !(g.stream && g.stream.stream_served)) return false;
    if (state.needXlate && !(g.xlate && g.xlate.xlate_served)) return false;
    if (state.needGoverned && !(g.governed && g.governed.governed_served)) return false;
    return true;
  });

  const col = COLUMNS.find((c) => c.id === state.sortCol);
  rows = rows.slice().sort((a, b) => {
    const va = col.get(a).v, vb = col.get(b).v;
    if (va === null && vb === null) return a.display.localeCompare(b.display);
    if (va === null) return 1; /* missing values always sink to the bottom */
    if (vb === null) return -1;
    if (typeof va === "string") return state.sortDesc ? vb.localeCompare(va) : va.localeCompare(vb);
    return state.sortDesc ? vb - va : va - vb;
  });

  tbody.innerHTML = rows.map((g) =>
    "<tr>" + COLUMNS.map((c) => {
      if (c.render) return c.render(g);
      const cell = c.get(g);
      return cell.na ? `<td class="na">${esc(cell.text)}</td>` : `<td>${esc(cell.text)}</td>`;
    }).join("") + "</tr>"
  ).join("");

  thead.querySelectorAll("th").forEach((th) => {
    th.addEventListener("click", () => {
      const id = th.dataset.col;
      if (state.sortCol === id) state.sortDesc = !state.sortDesc;
      else {
        state.sortCol = id;
        state.sortDesc = !!COLUMNS.find((c) => c.id === id).desc;
      }
      renderTable();
    });
  });
}

function renderFilters() {
  const langs = [...new Set(state.data.gateways.map((g) => g.lang))].sort();
  const box = document.getElementById("lang-filters");
  box.innerHTML = langs.map((l) => `<button class="chip-filter" data-lang="${esc(l)}">${esc(l)}</button>`).join("");
  box.querySelectorAll("button").forEach((b) => {
    b.addEventListener("click", () => {
      const l = b.dataset.lang;
      if (state.langs.has(l)) { state.langs.delete(l); b.classList.remove("on"); b.style.background = ""; }
      else { state.langs.add(l); b.classList.add("on"); b.style.background = LANG_COLORS[l] || LANG_COLORS.Other; }
      renderTable();
    });
  });
  for (const [id, key] of [["f-stream", "needStream"], ["f-xlate", "needXlate"], ["f-governed", "needGoverned"]]) {
    document.getElementById(id).addEventListener("change", (e) => {
      state[key] = e.target.checked;
      renderTable();
    });
  }
}

/* ---- protocol matrix -------------------------------------------------------- */
const MATRIX_CELLS = ["openai", "openai-responses", "anthropic", "gemini", "cohere", "bedrock"];
const MATRIX_LABELS = {
  openai: "OpenAI", "openai-responses": "OpenAI Responses", anthropic: "Anthropic",
  gemini: "Gemini", cohere: "Cohere", bedrock: "Bedrock Converse",
};

function renderMatrix() {
  const withMatrix = state.data.gateways.filter((g) => g.matrix && g.matrix.cells);
  if (!withMatrix.length) {
    document.getElementById("matrix-empty").classList.remove("hidden");
    document.getElementById("matrix-grid").classList.add("hidden");
    return;
  }
  /* sorted by measurement: served-cell count desc, then name */
  const count = (g) => MATRIX_CELLS.filter((c) => g.matrix.cells[c] && g.matrix.cells[c].served === true).length;
  withMatrix.sort((a, b) => count(b) - count(a) || a.display.localeCompare(b.display));

  const grid = document.getElementById("matrix-grid");
  grid.innerHTML = `<div class="table-scroll matrix-table"><table><thead><tr><th>Gateway</th>${
    MATRIX_CELLS.map((c) => `<th>${esc(MATRIX_LABELS[c])}</th>`).join("")
  }</tr></thead><tbody>${
    withMatrix.map((g) => `<tr><td class="name">${
      g.repo ? `<a href="${g.repo}" target="_blank" rel="noopener">${esc(g.display)}</a>` : esc(g.display)
    }</td>${
      MATRIX_CELLS.map((c) => {
        const cell = g.matrix.cells[c];
        if (!cell) return `<td class="na">n/a</td>`;
        const cls = cell.served === true ? "served" : cell.served === "unprobed_auth" ? "unprobed" : "failed";
        const label = cell.served === true ? "served" : cell.served === "unprobed_auth" ? "unprobed (auth)" : "not served";
        return `<td><span class="cell ${cls}" data-gw="${esc(g.key)}" data-cell="${esc(c)}" title="${esc(g.display)} / ${esc(MATRIX_LABELS[c])}: ${label}. ${esc(cell.verdict_note || "")}"></span></td>`;
      }).join("")
    }</tr>`).join("")
  }</tbody></table></div>`;

  grid.querySelectorAll(".cell").forEach((el) => {
    el.addEventListener("click", () => {
      const g = state.data.gateways.find((x) => x.key === el.dataset.gw);
      const cell = g.matrix.cells[el.dataset.cell];
      const label = cell.served === true ? "served" : cell.served === "unprobed_auth" ? "unprobed (auth)" : "not served";
      const detail = document.getElementById("matrix-detail");
      detail.classList.remove("hidden");
      detail.innerHTML =
        `<h4>${esc(g.display)} / ${esc(MATRIX_LABELS[el.dataset.cell])}: ${label} (HTTP ${esc(cell.status || "?")}, ${esc(cell.path || "")})</h4>` +
        `<div>${esc(cell.verdict_note || "no verdict note")}</div>` +
        (cell.body_snippet ? `<pre>${esc(cell.body_snippet)}</pre>` : "");
      detail.scrollIntoView({ behavior: "smooth", block: "nearest" });
    });
  });
}

/* ---- charts gallery --------------------------------------------------------- */
const CHART_CAPTIONS = {
  added_latency: "Added latency vs direct-to-mock, p99 in microseconds, concurrency 1. Lower is better.",
  rps_sustained_20ms: "Sustained RPS with a 20 ms mock LLM latency (p99 under 1 s, error rate under 0.1 percent). Higher is better.",
  rps_max_proxy: "Max proxy RPS against an instant mock. Higher is better.",
  memory_rss: "Process RSS in MiB: idle after launch and peak under large-payload load. Lower is better.",
  cost_per_million: "Instance cost per million requests at the sustained rate. Lower is better.",
  rps_per_dollar: "Sustained RPS per dollar of hourly instance cost. Higher is better.",
  stream_added_ttft: "Streaming: added time-to-first-token vs direct-to-mock, p99. Lower is better.",
  stream_added_gap: "Streaming: added inter-frame (per-token) latency vs direct-to-mock, p99. Lower is better.",
  stream_sustained: "Streaming: max concurrent SSE streams sustained without frame loss or stalls. Higher is better.",
  xlate_added_latency: "Translation (Anthropic in, OpenAI upstream): added latency p99. Lower is better.",
  xlate_rps_sustained_20ms: "Translation path: sustained RPS at 20 ms LLM latency. Higher is better.",
  governed_throughput: "Sustained RPS with native governance active vs the plain launch. Closer bars mean cheaper governance.",
};
function chartCaption(file) {
  const base = file.replace(/^charts\//, "").replace(/\.png$/, "");
  const top5 = base.startsWith("top5_");
  const key = top5 ? base.slice(5) : base;
  const body = CHART_CAPTIONS[key] || key.replace(/_/g, " ");
  return (top5 ? "Top 5 field leaders. " : "All gateways. ") + body;
}

function renderCharts() {
  const gallery = document.getElementById("chart-gallery");
  const charts = state.data.charts || [];
  if (!charts.length) {
    gallery.innerHTML = `<p class="muted">No chart PNGs are committed yet.</p>`;
    return;
  }
  /* full-field charts first, then top5 variants */
  const ordered = charts.slice().sort((a, b) =>
    (a.file.includes("top5_") - b.file.includes("top5_")) || a.file.localeCompare(b.file));
  gallery.innerHTML = ordered.map((c) =>
    `<figure data-src="${esc(c.file)}"><img src="${esc(c.file)}" alt="${esc(chartCaption(c.file))}" loading="lazy"><figcaption>${esc(chartCaption(c.file))}</figcaption></figure>`
  ).join("");
  gallery.querySelectorAll("figure").forEach((f) => {
    f.addEventListener("click", () => {
      const box = document.createElement("div");
      box.className = "lightbox";
      box.innerHTML = `<img src="${esc(f.dataset.src)}" alt="">`;
      box.addEventListener("click", () => box.remove());
      document.body.appendChild(box);
    });
  });
}

/* ---- method links + footer -------------------------------------------------- */
function renderStatic() {
  const repo = state.data.repo || "https://github.com/GetBusbar/benchmarking";
  for (const suite of ["perf", "memory", "stream", "xlate", "governed", "matrix"]) {
    const a = document.getElementById(`lnk-${suite}`);
    if (a) a.href = `${repo}/blob/main/${suite}/run.sh`;
  }
  document.getElementById("repo-link").href = repo;
  const hw = document.getElementById("hw-stamp");
  const bits = [];
  if (state.data.hardware) bits.push(`Ran on: ${state.data.hardware}`);
  if (state.data.latest_measured_at) bits.push(`Latest measurement: ${state.data.latest_measured_at}`);
  bits.push(`Site data generated: ${state.data.generated_at || "unknown"}`);
  hw.textContent = bits.join(" · ");
}

/* ---- tabs ------------------------------------------------------------------- */
function initTabs() {
  const tabs = document.querySelectorAll(".tab");
  tabs.forEach((t) => t.addEventListener("click", () => {
    tabs.forEach((x) => x.classList.toggle("active", x === t));
    document.querySelectorAll(".view").forEach((v) =>
      v.classList.toggle("hidden", v.id !== `view-${t.dataset.view}`));
  }));
}

/* ---- boot ------------------------------------------------------------------- */
fetch("data.json")
  .then((r) => { if (!r.ok) throw new Error(`data.json: HTTP ${r.status}`); return r.json(); })
  .then((data) => {
    state.data = data;
    initTabs();
    renderFilters();
    renderTable();
    renderMatrix();
    renderCharts();
    renderStatic();
  })
  .catch((err) => {
    document.querySelector("main").innerHTML =
      `<p class="muted">Could not load data.json (${esc(err.message)}). Run <code>node site/gen-data.mjs</code> first.</p>`;
  });
