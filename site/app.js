/* On the Bench: AI tool benchmark results site. Vanilla JS, no dependencies.
   The benchmarked thing is a CATEGORY (today: gateways). Each category reads its
   data bundle (emitted by gen-data.mjs) and renders the views: results table with
   search/filters, per-gateway drawer, compare mode, protocol matrix, charts, method.
   State round-trips through clean path URLs: /<category>/<view>?<params> via the
   History API, so every view is permalinkable. Pure logic (filtering, URL codec,
   sweep chart) is exported for the node smoke test in site/test.mjs. */
"use strict";

const NODE = typeof window === "undefined";

/* ---- category model ---------------------------------------------------------
   A category is the class of AI tool being benchmarked. Each entry declares its
   id (the first URL path segment), nav label, page tagline, and data source.
   EXTENSION SEAM: to add a category (e.g. models), add an entry here with its own
   data bundle (convention: data/<category>.json, emitted by that category's
   generator) and teach gen-data.mjs to emit it. Routing, nav, and permalinks pick
   it up automatically; the per-category views/columns stay category-specific. */
const CATEGORIES = {
  gateways: {
    id: "gateways",
    label: "Gateways",
    tagline: "Reproducible gateway overhead measurement on neutral hardware. Same box, same mock upstream, same load, same CPU pin for every gateway; every number regenerates from committed JSON.",
    data: "/data.json",
  },
};
const DEFAULT_CATEGORY = "gateways";
// The three perf tabs each rank an INTERNALLY COHERENT path so a single sort is honest:
//   passthrough = openai->openai only (every gateway on the identical dialect, no translation)
//   translation = openai-in -> best non-openai egress (fixed fair ingress, egress varies)
//   streaming   = SSE passthrough (its own stall-gated ceiling)
// matrix + method round out the five. `charts` folds into method; `results` was the old blended tab.
const VIEWS = ["passthrough", "translation", "streaming", "matrix", "method"];
const VIEW_LABELS = { passthrough: "Passthrough", translation: "Translation", streaming: "Streaming", matrix: "Protocol matrix", method: "Method" };
const PERF_VIEWS = new Set(["passthrough", "translation", "streaming"]);
// Old shared URLs pointed at results/charts; map them onto the new tabs so links keep resolving.
const VIEW_ALIASES = { results: "passthrough", charts: "method" };
// Each perf tab's default (and honest headline) sort column; a clean URL omits the sort when it
// equals this, and switching tabs snaps to it unless the URL pins another.
const VIEW_SORT = { passthrough: "rps20", translation: "xlrps", streaming: "streams" };

/* Language chip colours: kept in sync with LANG_COLORS in charts.py. */
const LANG_COLORS = {
  Rust: "#c4602d",
  Go: "#00a0c6",
  Python: "#3b6ea5",
  Node: "#c59b2d",
  Other: "#6b7280",
};
/* Distinct series colours for compare overlays (max 3 gateways). */
const CMP_COLORS = ["#4cc38a", "#6cb6ff", "#e5a54b"];

const fmtInt = (v) => Math.round(v).toLocaleString("en-US");
// Added-latency deltas are shown raw (no noise-floor smoothing). On the paced stream
// suite the per-frame value is noise-dominated and can flip sign run-to-run; the honest
// per-frame number comes from the CPU-bound stream suite, not from massaging this one.
const fmtAdded = fmtInt;
const fmt1 = (v) => v.toLocaleString("en-US", { minimumFractionDigits: 1, maximumFractionDigits: 1 });
const fmtPct = (v) => `${v > 0 ? "+" : ""}${v.toFixed(1)}%`;

/* Footer timestamps: a clean UTC date/time plus a COARSE relative age (hours or
   days only, deliberately imprecise). Age is computed client-side against now,
   so it stays fresh without a rebuild. Pure; covered by site/test.mjs. */
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
function fmtStamp(iso) {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return String(iso || "unknown");
  const d = new Date(t);
  const pad = (n) => String(n).padStart(2, "0");
  return `${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}, ${d.getUTCFullYear()} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())} UTC`;
}
function fmtAge(iso, now = Date.now()) {
  const t = Date.parse(iso);
  if (!Number.isFinite(t) || now < t) return "";
  const hours = Math.floor((now - t) / 3600000);
  if (hours < 1) return "just now";
  if (hours < 48) return `${hours} hour${hours === 1 ? "" : "s"} ago`;
  return `${Math.floor(hours / 24)} days ago`;
}
function stampWithAge(iso, now = Date.now()) {
  const age = fmtAge(iso, now);
  return age ? `${fmtStamp(iso)} (${age})` : fmtStamp(iso);
}

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

/* naText: compact honest label for a lane that was not served. The suites emit
   long diagnostic notes (passthrough evidence, launch errors); those must never
   be dumped as metric values or they blow the table layout wide open. The cell
   shows a short badge and the full note travels in the title tooltip; the
   drawer still shows the note verbatim as evidence. */
function naText(j, flag, errKey) {
  if (!j) return { text: "not measured", note: "" };
  const note = j[errKey] || j.serve_error || "";
  let text = "not served";
  if (j.xlate_passthrough === true || note.startsWith("UNTRANSLATED passthrough")) text = "n/a (passthrough)";
  // "manifest defines no <hook>" means THIS harness did not implement that suite's probe for this
  // gateway (e.g. the governed suite is only wired for gateways whose manifest defines the hook).
  // That is "not tested", NOT "not supported": we must never assert a capability verdict about a
  // gateway we did not actually exercise (several here have native governance we simply did not probe).
  else if (note.includes("manifest defines no")) text = "not tested";
  // A boot/build failure is OUR environment failing to start the gateway, not the gateway refusing
  // a probe: it must read as "did not run", never as a capability verdict against the gateway. Same
  // honesty rule as the protocol matrix (status 000 / "failed to boot" / never became ready).
  else if (String(j.last_http_status || "") === "000" || /failed to boot|no such file|not listening|never became ready|build failed/i.test(note)) text = "did not run";
  return { text, note };
}

/* laneVal: if the suite file exists but the served flag is false, surface a
   compact label (full note in .note); if the file is absent, "not measured". */
function lane(g, suite, flag, errKey, pick) {
  const j = g[suite];
  if (!j || j[flag] === false) {
    const na = naText(j, flag, errKey);
    return { v: null, text: na.text, note: na.note, na: true };
  }
  return pick(j);
}

/* passCell: the Passthrough tab reads ONLY the openai->openai diagonal (g.best_cell, which gen-data
   now restricts to that single dialect) so every row ranks on identical work. A gateway that has a
   matrix but no green openai diagonal genuinely does not serve openai passthrough in our grid, so it
   reads n/a rather than borrowing a different-dialect number under an OpenAI-passthrough header.
   Only a gateway with NO matrix at all (legacy single-path run) falls back to its perf suite. */
function passCell(g, key, fmt) {
  if (g.best_cell && g.best_cell[key] != null)
    return { v: g.best_cell[key], text: fmt(g.best_cell[key]), na: false };
  // No swept openai diagonal. Fall back to the perf suite ONLY when the gateway genuinely serves
  // openai passthrough (matrix openai->openai is green but its per-cell sweep has not landed, e.g.
  // bifrost mid-re-run), or has no matrix at all. A gateway whose openai->openai is not_configurable
  // (e.g. litellm-rust) does NOT serve openai passthrough, so it reads n/a rather than borrowing its
  // native-dialect perf number under an OpenAI-passthrough header.
  if (servesOpenaiPassthrough(g))
    return lane(g, "perf", "served", "serve_error", (j) =>
      j[key] != null ? { v: j[key], text: fmt(j[key]), na: false } : { v: null, text: "n/a", na: true });
  return { v: null, text: "n/a", na: true };
}
/* Does this gateway serve the openai->openai passthrough at all? True when it has no matrix (legacy
   single-path run, perf suite IS its passthrough) or its matrix openai->openai cell is green. */
function servesOpenaiPassthrough(g) {
  if (!g.matrix || !g.matrix.upstreams) return true;
  const up = g.matrix.upstreams.openai;
  const cell = up && up.cells && up.cells.openai;
  return !!(cell && cell.served === true);
}

/* xlateCell: the Translation tab reads the openai-in -> best non-openai egress cell (g.translation_cell,
   chosen by gen-data on lowest added latency, fixed fair ingress). Absent a matrix, fall back to the
   legacy xlate suite (anthropic-in / openai-out). Keys are cell.perf keys (added_latency_p99_us, ...). */
function xlateCell(g, key, fmt) {
  if (g.translation_cell && g.translation_cell[key] != null)
    return { v: g.translation_cell[key], text: fmt(g.translation_cell[key]), na: false };
  if (!g.matrix || !g.matrix.upstreams) {
    const legacy = { added_latency_p99_us: "xlate_added_latency_p99_us", rps_sustained_20ms: "xlate_rps_sustained_20ms" }[key] || key;
    return lane(g, "xlate", "xlate_served", "xlate_error", (j) =>
      j[legacy] != null ? { v: j[legacy], text: fmt(j[legacy]), na: false } : { v: null, text: "n/a", na: true });
  }
  return { v: null, text: "n/a", na: true };
}

/* ---- column model ----------------------------------------------------------- */
/* get(g) returns {v, text, na}: v is the sortable value (null = none), text the cell
   text, na marks a muted "not measured / not served" cell. sortable:false columns
   (the compare checkbox) take no part in sorting. Columns are grouped into per-tab sets
   (COLUMN_SETS) so each perf tab ranks one coherent path; the shared leading columns
   (select / name / lang) are reused across all three. */
const COL_SEL = {
  id: "sel", label: "", sortable: false,
  get: () => ({ v: null, text: "", na: false }),
  render: (g, st) => {
    const on = st.cmp.includes(g.key);
    const full = !on && st.cmp.length >= 3;
    return `<td class="sel"><input type="checkbox" data-cmp="${esc(g.key)}" ${on ? "checked" : ""} ${full ? "disabled" : ""} title="Select for compare (max 3)"></td>`;
  },
};
const COL_NAME = {
  id: "name", label: "Gateway", desc: false,
  get: (g) => ({ v: g.display.toLowerCase(), text: null, na: false }),
  render: (g) => {
    const a = g.repo
      ? `<a href="${g.repo}" target="_blank" rel="noopener">${esc(g.display)}</a>`
      : esc(g.display);
    return `<td class="name">${a}</td>`;
  },
};
const COL_LANG = {
  id: "lang", label: "Lang", desc: false,
  get: (g) => ({ v: g.lang, text: null, na: false }),
  render: (g) => {
    const c = LANG_COLORS[g.lang] || LANG_COLORS.Other;
    return `<td><span class="lang-chip" style="background:${c}">${esc(g.lang)}</span></td>`;
  },
};

const COLUMN_SETS = {
  // Passthrough: openai->openai only. No "Tested on" pill (every row is openai, so it would be a
  // column of identical chips); a caption above the table states the dialect once.
  passthrough: [
    COL_SEL, COL_NAME, COL_LANG,
    { id: "lat", label: "Added latency p99 (µs)", desc: false, title: "Gateway p99 minus direct-to-mock p99 at concurrency 1 on the OpenAI-in / OpenAI-out passthrough - pure forwarding, no translation",
      get: (g) => passCell(g, "added_latency_p99_us", fmtAdded) },
    { id: "rps20", label: "Sustained RPS @20ms", desc: true, title: "Sustained requests/sec with a 20 ms mock LLM latency (p99 < 1 s, <0.1% errors) on the OpenAI passthrough",
      get: (g) => passCell(g, "rps_sustained_20ms", fmtInt) },
    { id: "rpsmax", label: "Max proxy RPS", desc: true, title: "Throughput ceiling against an instant mock (p99 < 1 s, <0.1% errors) on the OpenAI passthrough",
      get: (g) => passCell(g, "rps_max_proxy", fmtInt) },
    { id: "memidle", label: "Mem idle (MiB)", desc: false, title: "Process RSS after launch, before load",
      get: (g) => lane(g, "memory", "served", "serve_error", (j) => ({ v: j.idle_rss_mib, text: fmt1(j.idle_rss_mib), na: false })) },
    { id: "mempeak", label: "Mem peak (MiB)", desc: false, title: "Peak process RSS under large-payload load",
      get: (g) => lane(g, "memory", "served", "serve_error", (j) => ({ v: j.peak_rss_mib, text: fmt1(j.peak_rss_mib), na: false })) },
  ],
  // Translation: openai-in -> best non-openai egress. A path pill shows which egress this row's
  // number is; only gateways that actually translate appear (view-implicit filter).
  translation: [
    COL_SEL, COL_NAME, COL_LANG,
    { id: "xlpath", label: "Path", desc: false, title: "OpenAI ingress translated to this egress dialect - the fastest translation path this gateway serves (chosen by lowest added latency)",
      get: (g) => ({ v: g.translation_cell ? g.translation_cell.egress : "", text: null, na: !g.translation_cell }),
      render: (g) => g.translation_cell
        ? `<td class="tested"><span class="tested-pill" title="OpenAI ingress translated to ${esc(g.translation_cell.egress)} upstream">openai&nbsp;&rarr;&nbsp;${esc(g.translation_cell.egress)}</span></td>`
        : `<td class="tested"><span class="muted">n/a</span></td>` },
    { id: "xllat", label: "Added latency p99 (µs)", desc: false, title: "Gateway p99 minus direct-to-mock p99 on the OpenAI-in translation path",
      get: (g) => xlateCell(g, "added_latency_p99_us", fmtAdded) },
    { id: "xlrps", label: "Sustained RPS @20ms", desc: true, title: "Sustained RPS @20ms on the OpenAI-in translation path (p99 < 1 s, <0.1% errors)",
      get: (g) => xlateCell(g, "rps_sustained_20ms", fmtInt) },
  ],
  // Streaming: SSE passthrough, its own stall-gated ceiling.
  streaming: [
    COL_SEL, COL_NAME, COL_LANG,
    { id: "sttft", label: "Stream added TTFT p99 (µs)", desc: false, title: "Gateway first-content-frame time minus direct-to-mock TTFT",
      get: (g) => lane(g, "stream", "stream_served", "stream_error", (j) => ({ v: j.stream_added_ttft_p99_us, text: fmtAdded(j.stream_added_ttft_p99_us), na: false })) },
    { id: "sgap", label: "Stream added per-token p99 (µs)", desc: false, title: "Gateway content-frame gap minus direct-to-mock gap",
      get: (g) => lane(g, "stream", "stream_served", "stream_error", (j) => ({ v: j.stream_added_gap_p99_us, text: fmtAdded(j.stream_added_gap_p99_us), na: false })) },
    { id: "streams", label: "Streams sustained", desc: true, title: "Max concurrent SSE streams with >=99.9% frame delivery, no stalls, <0.1% errors",
      get: (g) => lane(g, "stream", "stream_served", "stream_error", (j) => ({ v: j.stream_sustained_streams, text: fmtInt(j.stream_sustained_streams), na: false })) },
  ],
  // Governance is intentionally NO tab. onthebench measures every gateway at its default, out-of-the-box
  // config; the governed suite runs a non-default governance-enabled launch that only busbar's manifest
  // wires, so a comparative tab would spotlight busbar and read "not tested" for the rest.
};
/* The set of columns for a view; perf tabs use COLUMN_SETS, everything else has no table. */
function columnsFor(view) { return COLUMN_SETS[view] || COLUMN_SETS.passthrough; }
/* Every column id across all tabs - used to validate a sort id coming from a shared URL. */
const ALL_COLUMN_IDS = new Set(Object.values(COLUMN_SETS).flat().map((c) => c.id));

/* Metric groups per lane: drives the drawer and the compare table.
   best: "min"/"max" picks the neutral best-value highlight by measurement. */
const LANES = [
  {
    key: "perf", label: "Latency & throughput", flag: "served", err: "serve_error",
    metrics: [
      { k: "added_latency_p50_us", label: "Added latency p50 (µs)", best: "min", fmt: fmtAdded },
      { k: "added_latency_p99_us", label: "Added latency p99 (µs)", best: "min", fmt: fmtAdded },
      { k: "rps_max_proxy", label: "Max proxy RPS", best: "max", fmt: fmtInt },
      { k: "rps_sustained_20ms", label: "Sustained RPS @20ms", best: "max", fmt: fmtInt },
    ],
  },
  {
    key: "memory", label: "Memory", flag: "served", err: "serve_error",
    metrics: [
      { k: "idle_rss_mib", label: "Idle RSS (MiB)", best: "min", fmt: fmt1 },
      { k: "peak_rss_mib", label: "Peak RSS (MiB)", best: "min", fmt: fmt1 },
    ],
  },
  {
    key: "stream", label: "Streaming", flag: "stream_served", err: "stream_error",
    metrics: [
      { k: "stream_added_ttft_p99_us", label: "Added TTFT p99 (µs)", best: "min", fmt: fmtAdded },
      { k: "stream_added_gap_p99_us", label: "Added per-token p99 (µs)", best: "min", fmt: fmtAdded },
      { k: "stream_sustained_streams", label: "Streams sustained", best: "max", fmt: fmtInt },
    ],
  },
  {
    key: "xlate", label: "Translation", flag: "xlate_served", err: "xlate_error",
    metrics: [
      { k: "xlate_added_latency_p99_us", label: "Added latency p99 (µs)", best: "min", fmt: fmtInt },
      { k: "xlate_rps_sustained_20ms", label: "Sustained RPS @20ms", best: "max", fmt: fmtInt },
    ],
  },
];

/* ---- state + URL codec ------------------------------------------------------ */
function newState() {
  return {
    data: null,
    category: DEFAULT_CATEGORY,
    view: "passthrough",
    q: "",
    sortCol: "rps20",
    sortDesc: true,
    langs: new Set(),
    classes: new Set(),
    needStream: false,
    needXlate: false,
    cmp: [],        /* gateway keys selected for compare, max 3 */
    cmpOpen: false, /* compare panel visible */
    drawer: null,   /* gateway key open in the drawer */
  };
}
const state = newState();

/* Capability filter toggles. Governance is deliberately NOT here: it is only
   measured for one gateway, so a field-wide filter on it would mislead; the
   governed data still shows per-gateway in the table column and drawer. */
const CAPS = [["needStream", "stream"], ["needXlate", "xlate"]];

/* Serialize the shareable parts of state into a clean path URL:
   /<category>/<view>?<params>. The default view (results) omits the view segment
   and default params are omitted, so the pristine view keeps a clean URL
   (/gateways). Returns path + query, e.g. /gateways/matrix?sort=mempeak&dir=asc. */
function encodeUrl(st) {
  const p = new URLSearchParams();
  if (st.q) p.set("q", st.q);
  if (st.classes.size) p.set("cls", [...st.classes].sort().join("|"));
  if (st.langs.size) p.set("lang", [...st.langs].sort().join("|"));
  const caps = CAPS.filter(([k]) => st[k]).map(([, name]) => name);
  if (caps.length) p.set("cap", caps.join("|"));
  // Each perf tab's clean URL omits the sort when it equals that tab's default column + direction.
  const defSort = VIEW_SORT[st.view] || "rps20";
  const defCol = columnsFor(st.view).find((c) => c.id === defSort);
  const defDesc = defCol ? defCol.desc !== false : true;
  if (st.sortCol !== defSort || st.sortDesc !== defDesc) {
    p.set("sort", st.sortCol);
    p.set("dir", st.sortDesc ? "desc" : "asc");
  }
  if (st.cmp.length) p.set("cmp", st.cmp.join("|"));
  if (st.cmpOpen) p.set("cv", "1");
  if (st.drawer) p.set("gw", st.drawer);
  const cat = CATEGORIES[st.category] ? st.category : DEFAULT_CATEGORY;
  const path = st.view && st.view !== "passthrough" ? `/${cat}/${st.view}` : `/${cat}`;
  const qs = p.toString();
  return qs ? `${path}?${qs}` : path;
}

/* Parse a path + query (+ optional legacy #hash) back into state.
   Unknown categories and views fall back to the defaults, so / normalizes to
   /gateways (results). Pre-path-routing links carried everything in the hash
   (#view=matrix&sort=...); when the hash holds params, it wins over the query so
   old shared URLs keep resolving, and boot() then rewrites them to path form. */
function decodeUrl(pathname, search, hash) {
  const st = newState();
  const segs = String(pathname || "/").split("/").filter(Boolean);
  // Resolve a raw view token to a real view, honoring legacy aliases (results->passthrough,
  // charts->method) so old shared/deep links keep landing on a live tab.
  const resolveView = (v) => (VIEWS.includes(v) ? v : VIEW_ALIASES[v] || null);
  let i = 0;
  if (segs[i] && CATEGORIES[segs[i]]) st.category = segs[i++];
  if (segs[i] && resolveView(segs[i])) st.view = resolveView(segs[i]);
  const legacy = String(hash || "").replace(/^#/, "");
  const p = new URLSearchParams(legacy.includes("=") ? legacy : String(search || "").replace(/^\?/, ""));
  const list = (k) => (p.get(k) || "").split("|").filter(Boolean);
  if (p.get("view") && resolveView(p.get("view"))) st.view = resolveView(p.get("view")); /* legacy hash form */
  st.q = p.get("q") || "";
  st.classes = new Set(list("cls"));
  st.langs = new Set(list("lang"));
  for (const cap of list("cap")) {
    const hit = CAPS.find(([, name]) => name === cap);
    if (hit) st[hit[0]] = true;
  }
  // Accept any real, sortable column id from any tab; renderTable snaps it back to the tab's
  // default if it does not belong to the resolved view.
  if (p.get("sort") && ALL_COLUMN_IDS.has(p.get("sort")) && p.get("sort") !== "sel") {
    st.sortCol = p.get("sort");
    st.sortDesc = p.get("dir") !== "asc";
  } else {
    st.sortCol = VIEW_SORT[st.view] || "rps20";
  }
  st.cmp = list("cmp").slice(0, 3);
  st.cmpOpen = p.get("cv") === "1" && st.cmp.length >= 2;
  st.drawer = p.get("gw") || null;
  return st;
}

/* Push a history entry for navigation-shaped interactions (tabs, sorts, filter
   clicks, compare, drawer) so back/forward walks through them; replace for
   continuous input (search typing) so it never spams history. */
function syncUrl(push = false) {
  if (NODE) return;
  const url = encodeUrl(state);
  const cur = location.pathname + location.search;
  try {
    if (url !== cur || location.hash) {
      if (push) history.pushState(null, "", url);
      else history.replaceState(null, "", url);
    }
  } catch (e) { /* file:// or sandboxed: the URL bar goes stale but the app still works */ }
  updateTitle();
}

function updateTitle() {
  if (NODE) return;
  const cat = CATEGORIES[state.category] || CATEGORIES[DEFAULT_CATEGORY];
  const view = state.view !== "passthrough" ? ` ${VIEW_LABELS[state.view] || state.view}` : "";
  document.title = `${cat.label}${view} · On the Bench · AI tool benchmarks`;
}

/* ---- filtering (pure) ------------------------------------------------------- */
function applyFilters(gateways, st) {
  const q = st.q.trim().toLowerCase();
  return gateways.filter((g) => {
    if (q && !g.display.toLowerCase().includes(q) && !g.key.toLowerCase().includes(q)) return false;
    if (st.classes.size && !st.classes.has(g.cls || "Gateway")) return false;
    if (st.langs.size && !st.langs.has(g.lang)) return false;
    if (st.needStream && !(g.stream && g.stream.stream_served)) return false;
    if (st.needXlate && !hasTranslation(g)) return false;
    // View-implicit filter: the Translation tab lists only gateways that actually translate, the
    // Streaming tab only ones that serve SSE. A pure proxy that never translates simply is not in
    // the translation ranking (rather than sitting there as a row of n/a).
    if (st.view === "translation" && !hasTranslation(g)) return false;
    if (st.view === "streaming" && !(g.stream && g.stream.stream_served)) return false;
    return true;
  });
}
/* A gateway "translates" if it has a measured openai-in translation cell, or (legacy, no matrix) it
   served the xlate suite. Drives both the translation tab's implicit filter and the capability toggle. */
function hasTranslation(g) {
  return !!(g.translation_cell || (g.xlate && g.xlate.xlate_served));
}

/* ---- sweep chart: dependency-free canvas line chart -------------------------
   series: [{label, color, points: [{x, y}]}], x is concurrency (log scale),
   y linear. Returns the geometry (for tests and the hover handler) or null when
   there is nothing to draw. */
function niceStep(raw) {
  const pow = Math.pow(10, Math.floor(Math.log10(raw)));
  const m = raw / pow;
  return (m <= 1 ? 1 : m <= 2 ? 2 : m <= 5 ? 5 : 10) * pow;
}
function fmtTick(v) {
  if (v >= 1e6) return `${+(v / 1e6).toFixed(1)}M`;
  if (v >= 1e3) return `${+(v / 1e3).toFixed(1)}k`;
  return String(Math.round(v));
}

function drawSweep(canvas, series, opts = {}) {
  const ctx = canvas.getContext && canvas.getContext("2d");
  if (!ctx) return null;
  const drawable = series.filter((s) => s.points && s.points.length);
  const pts = drawable.flatMap((s) => s.points);
  const W = canvas.width, H = canvas.height;
  ctx.clearRect(0, 0, W, H);
  const padL = 58, padR = 14, padT = 16, padB = 34;
  const fg = opts.fg || "#9aa4b2", grid = opts.grid || "rgba(154,164,178,.18)";
  if (!pts.length) {
    ctx.fillStyle = fg;
    ctx.font = "12px Inter, sans-serif";
    ctx.fillText("no sweep data", padL, H / 2);
    return null;
  }
  const lx = pts.map((p) => Math.log10(p.x));
  let x0 = Math.min(...lx), x1 = Math.max(...lx);
  if (x0 === x1) { x0 -= 0.3; x1 += 0.3; }
  let yMax = Math.max(...pts.map((p) => p.y)) * 1.06;
  if (!(yMax > 0)) yMax = 1;
  const X = (v) => padL + ((Math.log10(v) - x0) / (x1 - x0)) * (W - padL - padR);
  const Y = (v) => H - padB - (v / yMax) * (H - padT - padB);

  ctx.font = "11px Inter, sans-serif";
  ctx.lineWidth = 1;

  /* y grid + ticks */
  const step = niceStep(yMax / 4);
  ctx.textAlign = "right"; ctx.textBaseline = "middle";
  for (let v = 0; v <= yMax; v += step) {
    ctx.strokeStyle = grid;
    ctx.beginPath(); ctx.moveTo(padL, Y(v)); ctx.lineTo(W - padR, Y(v)); ctx.stroke();
    ctx.fillStyle = fg;
    ctx.fillText(fmtTick(v), padL - 6, Y(v));
  }
  /* x ticks: up to 7 of the distinct measured concurrencies */
  const xs = [...new Set(pts.map((p) => p.x))].sort((a, b) => a - b);
  const stride = Math.ceil(xs.length / 7);
  ctx.textAlign = "center"; ctx.textBaseline = "top";
  xs.filter((_, i) => i % stride === 0 || i === xs.length - 1).forEach((v) => {
    ctx.strokeStyle = grid;
    ctx.beginPath(); ctx.moveTo(X(v), padT); ctx.lineTo(X(v), H - padB); ctx.stroke();
    ctx.fillStyle = fg;
    ctx.fillText(fmtTick(v), X(v), H - padB + 5);
  });
  /* axes */
  ctx.strokeStyle = fg;
  ctx.beginPath(); ctx.moveTo(padL, padT); ctx.lineTo(padL, H - padB); ctx.lineTo(W - padR, H - padB); ctx.stroke();
  /* axis labels */
  ctx.fillStyle = fg;
  ctx.textAlign = "center";
  ctx.fillText(opts.xLabel || "concurrency (log)", padL + (W - padL - padR) / 2, H - 14);
  ctx.save();
  ctx.translate(12, padT + (H - padT - padB) / 2); ctx.rotate(-Math.PI / 2);
  ctx.fillText(opts.yLabel || "", 0, 0);
  ctx.restore();

  /* series */
  for (const s of drawable) {
    const sp = s.points.slice().sort((a, b) => a.x - b.x);
    ctx.strokeStyle = s.color; ctx.fillStyle = s.color; ctx.lineWidth = 1.6;
    ctx.beginPath();
    sp.forEach((p, i) => { if (i === 0) ctx.moveTo(X(p.x), Y(p.y)); else ctx.lineTo(X(p.x), Y(p.y)); });
    ctx.stroke();
    for (const p of sp) { ctx.beginPath(); ctx.arc(X(p.x), Y(p.y), 2.4, 0, Math.PI * 2); ctx.fill(); }
  }
  /* legend, top-right */
  if (opts.legend !== false && drawable.length > 1) {
    ctx.textAlign = "left"; ctx.textBaseline = "middle";
    let ly = padT + 4;
    for (const s of drawable) {
      ctx.fillStyle = s.color;
      ctx.fillRect(W - padR - 118, ly - 3, 14, 3);
      ctx.fillStyle = fg;
      ctx.fillText(s.label, W - padR - 100, ly - 1);
      ly += 15;
    }
  }
  return { X, Y, series: drawable, padL, padR, padT, padB, W, H };
}

/* Cheap hover readout: nearest point across all series by pixel distance. */
function attachSweepHover(canvas, series, opts) {
  if (!canvas.addEventListener) return;
  const redraw = () => drawSweep(canvas, series, opts);
  canvas.addEventListener("mousemove", (ev) => {
    const geo = redraw();
    if (!geo) return;
    const r = canvas.getBoundingClientRect();
    const mx = (ev.clientX - r.left) * (canvas.width / r.width);
    const my = (ev.clientY - r.top) * (canvas.height / r.height);
    let best = null;
    for (const s of geo.series) for (const p of s.points) {
      const d = Math.hypot(geo.X(p.x) - mx, geo.Y(p.y) - my);
      if (!best || d < best.d) best = { d, p, s };
    }
    if (!best || best.d > 40) return;
    const ctx = canvas.getContext("2d");
    ctx.strokeStyle = best.s.color;
    ctx.beginPath(); ctx.arc(geo.X(best.p.x), geo.Y(best.p.y), 4.2, 0, Math.PI * 2); ctx.stroke();
    ctx.font = "11px Inter, sans-serif"; ctx.textAlign = "left"; ctx.textBaseline = "top";
    ctx.fillStyle = opts.fg || "#e6edf3";
    ctx.fillText(`${best.s.label}  conc ${fmtInt(best.p.x)}: ${fmtInt(best.p.y)} ${opts.unit || ""}`, geo.padL + 6, 2);
  });
  canvas.addEventListener("mouseleave", redraw);
}

/* Render both sweep charts (rps and p99 vs concurrency) into a container.
   series come as [{label, color, sweep: [{conc,rps,p99_us}]}]. */
function renderSweepCharts(container, sweepSeries, theme) {
  const usable = sweepSeries.filter((s) => s.sweep && s.sweep.length);
  if (!usable.length) {
    container.innerHTML = `<p class="muted">No sweep data recorded.</p>`;
    return;
  }
  container.innerHTML =
    `<figure class="sweep"><figcaption>RPS vs concurrency</figcaption><canvas width="520" height="230"></canvas></figure>` +
    `<figure class="sweep"><figcaption>p99 latency vs concurrency (µs)</figcaption><canvas width="520" height="230"></canvas></figure>`;
  const [c1, c2] = container.querySelectorAll("canvas");
  const rps = usable.map((s) => ({ label: s.label, color: s.color, points: s.sweep.map((p) => ({ x: p.conc, y: p.rps })) }));
  const p99 = usable.map((s) => ({ label: s.label, color: s.color, points: s.sweep.map((p) => ({ x: p.conc, y: p.p99_us })) }));
  const o1 = { yLabel: "RPS", unit: "rps", ...theme };
  const o2 = { yLabel: "p99 (µs)", unit: "µs p99", ...theme };
  drawSweep(c1, rps, o1); attachSweepHover(c1, rps, o1);
  drawSweep(c2, p99, o2); attachSweepHover(c2, p99, o2);
}

function chartTheme() {
  if (NODE) return {};
  const cs = getComputedStyle(document.documentElement);
  return {
    fg: cs.getPropertyValue("--fg-dim").trim() || "#9aa4b2",
    grid: cs.getPropertyValue("--grid").trim() || "rgba(154,164,178,.18)",
  };
}

/* Theme switcher: persist the choice, flip data-theme on <html>, and re-render
   so the canvas charts re-read the palette via chartTheme(). The initial
   data-theme is set by the inline <head> script before first paint. */
function initThemeToggle() {
  const btn = document.getElementById("theme-toggle");
  if (!btn) return;
  btn.addEventListener("click", () => {
    const next = document.documentElement.getAttribute("data-theme") === "light" ? "dark" : "light";
    document.documentElement.setAttribute("data-theme", next);
    try { localStorage.setItem("theme", next); } catch (e) { /* private mode: ignore */ }
    renderAll();
  });
}

/* ---- results table ---------------------------------------------------------- */
/* Per-tab caption: states in one line exactly which path this tab's numbers are, so a reader never
   has to guess what the ranking compares. No em dashes (house style). */
const TABLE_CAPTIONS = {
  passthrough: "OpenAI ingress to OpenAI upstream: pure passthrough, no translation. Every gateway runs the identical dialect, so the ranking is apples-to-apples. A gateway that does not serve OpenAI ingress reads n/a.",
  translation: "OpenAI ingress translated to another dialect. Each gateway is shown on the fastest translation path it serves (the Path pill). Only gateways that actually translate appear here.",
  streaming: "Server-sent-event passthrough. Streams sustained is each gateway's stall-free concurrent-stream ceiling; added TTFT and per-token are gateway minus direct-to-mock.",
};
function updateTableCaption(view) {
  const el = document.getElementById("table-caption");
  if (el) el.textContent = TABLE_CAPTIONS[view] || TABLE_CAPTIONS.passthrough;
}
function renderTable() {
  const { data } = state;
  const thead = document.querySelector("#results-table thead");
  const tbody = document.querySelector("#results-table tbody");

  // Which tab's columns to render. matrix/method have no table, so fall back to passthrough
  // (the section is hidden anyway) and never mutate the sort while off a perf tab.
  const view = PERF_VIEWS.has(state.view) ? state.view : "passthrough";
  const cols = columnsFor(view);
  // Snap the sort onto this tab if the current column does not belong to it (e.g. after switching
  // tabs, or a cross-tab sort id arrived from a shared URL).
  if (PERF_VIEWS.has(state.view) && !cols.some((c) => c.id === state.sortCol && c.sortable !== false)) {
    state.sortCol = VIEW_SORT[view] || "rps20";
    const dc = cols.find((c) => c.id === state.sortCol);
    state.sortDesc = dc ? dc.desc !== false : true;
  }
  updateTableCaption(view);

  thead.innerHTML = "<tr>" + cols.map((c) => {
    const sorted = state.sortCol === c.id;
    const dir = sorted ? `<span class="dir">${state.sortDesc ? " ▾" : " ▴"}</span>` : "";
    return `<th data-col="${c.id}" class="${sorted ? "sorted" : ""}${c.sortable === false ? " nosort" : ""}" title="${esc(c.title || "")}">${esc(c.label)}${dir}</th>`;
  }).join("") + "</tr>";

  let rows = applyFilters(data.gateways, state);
  const count = document.getElementById("row-count");
  if (count) count.textContent = `${rows.length} of ${data.gateways.length}`;

  const col = cols.find((c) => c.id === state.sortCol) || cols.find((c) => c.id === VIEW_SORT[view]) || cols[3];
  rows = rows.slice().sort((a, b) => {
    const va = col.get(a).v, vb = col.get(b).v;
    if (va === null && vb === null) return a.display.localeCompare(b.display);
    if (va === null) return 1; /* missing values always sink to the bottom */
    if (vb === null) return -1;
    if (typeof va === "string") return state.sortDesc ? vb.localeCompare(va) : va.localeCompare(vb);
    return state.sortDesc ? vb - va : va - vb;
  });

  tbody.innerHTML = rows.map((g) =>
    `<tr data-gw="${esc(g.key)}">` + cols.map((c) => {
      const sc = c.id === state.sortCol ? " sorted-col" : "";
      if (c.render) {
        // render columns emit their own <td>; tint the sorted one by injecting the class.
        return sc ? c.render(g, state).replace("<td", `<td class="sorted-col"`).replace('class="sorted-col" class="', 'class="sorted-col ') : c.render(g, state);
      }
      const cell = c.get(g);
      return cell.na
        ? `<td class="na${sc}" title="${esc(cell.note || "")}">${esc(cell.text)}</td>`
        : `<td class="${sc.trim()}">${esc(cell.text)}</td>`;
    }).join("") + "</tr>"
  ).join("");

  thead.querySelectorAll("th").forEach((th) => {
    th.addEventListener("click", () => {
      const id = th.dataset.col;
      const c = cols.find((x) => x.id === id);
      if (!c || c.sortable === false) return;
      if (state.sortCol === id) state.sortDesc = !state.sortDesc;
      else { state.sortCol = id; state.sortDesc = !!c.desc; }
      renderTable(); syncUrl(true);
    });
  });
  tbody.querySelectorAll("input[data-cmp]").forEach((cb) => {
    cb.addEventListener("change", () => toggleCompare(cb.dataset.cmp));
    cb.addEventListener("click", (ev) => ev.stopPropagation());
  });
  tbody.querySelectorAll("tr").forEach((tr) => {
    tr.addEventListener("click", (ev) => {
      if (ev.target.closest("a, input")) return;
      openDrawer(tr.dataset.gw, true);
    });
  });
}

/* ---- filter bar ------------------------------------------------------------- */
function chipGroup(boxId, values, set, colorFor) {
  const box = document.getElementById(boxId);
  box.innerHTML = values.map((v) => `<button class="chip-filter" data-v="${esc(v)}">${esc(v)}</button>`).join("");
  box.querySelectorAll("button").forEach((b) => {
    const apply = () => {
      const on = set.has(b.dataset.v);
      b.classList.toggle("on", on);
      b.style.background = on ? (colorFor ? colorFor(b.dataset.v) : "var(--grey)") : "";
    };
    apply();
    b.addEventListener("click", () => {
      if (set.has(b.dataset.v)) set.delete(b.dataset.v); else set.add(b.dataset.v);
      apply(); renderTable(); syncUrl(true);
    });
  });
}

/* Wire the persistent inputs exactly once (renderFilters may re-run on hashchange). */
function initFilterControls() {
  const search = document.getElementById("search");
  search.addEventListener("input", () => { state.q = search.value; renderTable(); syncUrl(false); });
  // The capability toggles are now implicit per tab (Translation/Streaming self-filter), so the DOM
  // checkboxes were retired; the state fields + URL param survive for back-compat. Wire only if present.
  for (const [key, name] of CAPS) {
    const el = document.getElementById(`f-${name}`);
    if (el) el.addEventListener("change", () => { state[key] = el.checked; renderTable(); syncUrl(true); });
  }
}

function renderFilters() {
  const gws = state.data.gateways;
  chipGroup("class-filters", [...new Set(gws.map((g) => g.cls || "Gateway"))].sort(), state.classes, null);
  chipGroup("lang-filters", [...new Set(gws.map((g) => g.lang))].sort(), state.langs,
    (l) => LANG_COLORS[l] || LANG_COLORS.Other);
  document.getElementById("search").value = state.q;
  for (const [, name] of CAPS) { const el = document.getElementById(`f-${name}`); if (el) el.checked = state[CAPS.find(([, n]) => n === name)[0]]; }
}

/* ---- per-gateway drawer ----------------------------------------------------- */
const MATRIX_CELLS = ["openai", "openai-responses", "anthropic", "gemini", "cohere", "bedrock"];
const MATRIX_LABELS = {
  openai: "OpenAI", "openai-responses": "OpenAI Responses", anthropic: "Anthropic",
  gemini: "Gemini", cohere: "Cohere", bedrock: "Bedrock Converse",
};
/* A served===false cell is one of two very different things, and a neutral board must not
   conflate them. If the gateway never served the warm-up under this egress config (or never
   answered at all, status 000), the harness could not get it into a testable state: that is
   our config, not the gateway's translation, so it renders "not verified", never a red fail.
   Only a gateway that actually served and returned a wrong or untranslated body is red. */
const isHarnessGap = (cell) => {
  const note = (cell.verdict_note || "").toLowerCase();
  return cell.status === "000" || (note.includes("never served") && note.includes("warm-up"));
};
const cellState = (cell) =>
  cell.served === true ? ["served", "served"]
    : cell.served === "unprobed_auth" ? ["unprobed", "unprobed (auth)"]
      : cell.served === "not_configurable" ? ["notconf", "not declared"]
        : isHarnessGap(cell) ? ["unverified", "not verified"]
          : ["failed", "not served"];

function laneStamp(j) {
  const bits = [];
  if (j.build) bits.push(j.build);
  if (j.measured_at) bits.push(j.measured_at);
  return bits.length ? `<div class="stamp muted">${esc(bits.join(" · "))}</div>` : "";
}

function drawerHtml(g) {
  const langC = LANG_COLORS[g.lang] || LANG_COLORS.Other;
  let h = `<header class="drawer-head">
    <h3>${g.repo ? `<a href="${g.repo}" target="_blank" rel="noopener">${esc(g.display)}</a>` : esc(g.display)}</h3>
    <div class="chips"><span class="cls-chip">${esc(g.cls || "Gateway")}</span>
    <span class="lang-chip" style="background:${langC}">${esc(g.lang)}</span></div>
  </header>`;

  const hw = LANES.map((l) => g[l.key]).find((j) => j && j.hardware);
  if (hw) h += `<p class="stamp muted">${esc(hw.hardware)}${hw.arch ? ` (${esc(hw.arch)})` : ""}</p>`;

  for (const l of LANES) {
    const j = g[l.key];
    h += `<section class="drawer-lane"><h4>${esc(l.label)}</h4>`;
    if (!j) h += `<p class="muted">not measured</p>`;
    else if (j[l.flag] === false) h += `<p class="muted">${esc(j[l.err] || "not served")}</p>${laneStamp(j)}`;
    else {
      h += `<dl>` + l.metrics.filter((m) => j[m.k] != null).map((m) =>
        `<div><dt>${esc(m.label)}</dt><dd>${esc(m.fmt(j[m.k]))}</dd></div>`).join("") + `</dl>${laneStamp(j)}`;
    }
    h += `</section>`;
  }

  /* protocol matrix row with evidence */
  h += `<section class="drawer-lane"><h4>Protocol matrix</h4>`;
  if (!(g.matrix && g.matrix.cells)) h += `<p class="muted">not measured</p>`;
  else {
    h += `<ul class="matrix-list">` + MATRIX_CELLS.map((c) => {
      const cell = g.matrix.cells[c];
      if (!cell) return `<li><span class="cell na"></span> ${esc(MATRIX_LABELS[c])}: <span class="muted">n/a</span></li>`;
      const [cls, label] = cellState(cell);
      return `<li><span class="cell ${cls}"></span> <b>${esc(MATRIX_LABELS[c])}</b>: ${label}` +
        ` <span class="muted">(HTTP ${esc(cell.status || "?")}, ${esc(cell.path || "")})</span>` +
        (cell.verdict_note ? `<div class="muted evidence">${esc(cell.verdict_note)}</div>` : "") +
        (cell.served !== true && cell.body_snippet ? `<pre>${esc(cell.body_snippet)}</pre>` : "") +
        `</li>`;
    }).join("") + `</ul>${laneStamp(g.matrix)}`;
  }
  h += `</section>`;

  h += `<section class="drawer-lane"><h4>Throughput sweeps</h4><div id="drawer-sweeps" class="sweeps"></div></section>`;
  return h;
}

function openDrawer(key, push = false) {
  const g = state.data.gateways.find((x) => x.key === key);
  if (!g) return;
  state.drawer = key;
  document.getElementById("drawer-body").innerHTML = drawerHtml(g);
  document.getElementById("drawer").classList.remove("hidden");
  document.getElementById("backdrop").classList.remove("hidden");
  const box = document.getElementById("drawer-sweeps");
  const series = [];
  if (g.perf && g.perf.served !== false) {
    series.push({ label: "sustained @20ms", color: "#4cc38a", sweep: g.perf.sweep_sustained_20ms });
    series.push({ label: "max proxy", color: "#6cb6ff", sweep: g.perf.sweep_max_proxy });
  }
  renderSweepCharts(box, series, chartTheme());
  syncUrl(push);
}
function closeDrawer() {
  state.drawer = null;
  document.getElementById("drawer").classList.add("hidden");
  document.getElementById("backdrop").classList.add("hidden");
  syncUrl(true);
}

/* ---- compare mode ----------------------------------------------------------- */
function toggleCompare(key) {
  const i = state.cmp.indexOf(key);
  if (i >= 0) state.cmp.splice(i, 1);
  else if (state.cmp.length < 3) state.cmp.push(key);
  if (state.cmp.length < 2) state.cmpOpen = false;
  renderTable(); renderCompareBar();
  if (state.cmpOpen) renderCompare(); else closeCompare(false);
  syncUrl(true);
}

function renderCompareBar() {
  const bar = document.getElementById("compare-bar");
  if (!state.cmp.length) { bar.classList.add("hidden"); return; }
  bar.classList.remove("hidden");
  const names = state.cmp.map((k) => {
    const g = state.data.gateways.find((x) => x.key === k);
    return g ? g.display : k;
  });
  bar.innerHTML = `<span>Compare: <b>${names.map(esc).join(", ")}</b> <span class="muted">(${state.cmp.length}/3)</span></span>
    <span class="bar-actions">
      <button id="cmp-open" ${state.cmp.length < 2 ? "disabled" : ""}>Compare</button>
      <button id="cmp-clear" class="ghost">Clear</button>
    </span>`;
  document.getElementById("cmp-open").addEventListener("click", () => { state.cmpOpen = true; renderCompare(); syncUrl(true); });
  document.getElementById("cmp-clear").addEventListener("click", () => {
    state.cmp = []; state.cmpOpen = false;
    renderTable(); renderCompareBar(); closeCompare(false); syncUrl(true);
  });
}

function bestIndex(vals, best) {
  let bi = -1;
  vals.forEach((v, i) => {
    if (v == null) return;
    if (bi < 0 || (best === "min" ? v < vals[bi] : v > vals[bi])) bi = i;
  });
  /* only highlight when there is an actual contest */
  return vals.filter((v) => v != null).length >= 2 ? bi : -1;
}

function renderCompare() {
  const gws = state.cmp.map((k) => state.data.gateways.find((g) => g.key === k)).filter(Boolean);
  if (gws.length < 2) return;
  state.cmpOpen = true;
  const panel = document.getElementById("compare-panel");
  panel.classList.remove("hidden");

  let h = `<div class="table-scroll"><table class="cmp-table"><thead><tr><th></th>` + gws.map((g, i) =>
    `<th><span class="dot" style="background:${CMP_COLORS[i]}"></span>${esc(g.display)}</th>`).join("") + `</tr></thead><tbody>`;
  h += `<tr><td class="metric">Class</td>${gws.map((g) => `<td>${esc(g.cls || "Gateway")}</td>`).join("")}</tr>`;
  h += `<tr><td class="metric">Language</td>${gws.map((g) => `<td>${esc(g.lang)}</td>`).join("")}</tr>`;
  h += `<tr><td class="metric">Build</td>${gws.map((g) => {
    const j = LANES.map((l) => g[l.key]).find((x) => x && x.build);
    const full = j ? j.build : "?";
    /* image digests and long refs stay in the tooltip; the cell stays compact */
    let short = full.replace(/\s*\(@sha256:[0-9a-f]+\)/, "");
    if (short.length > 40) short = short.slice(0, 37) + "...";
    return `<td title="${esc(full)}">${esc(short)}</td>`;
  }).join("")}</tr>`;

  for (const l of LANES) {
    /* skip the whole lane only when no gateway measured it at all; an all
       not-served lane still renders rows so the header is never left bare */
    if (gws.every((g) => !g[l.key])) continue;
    h += `<tr class="lane-row"><td colspan="${gws.length + 1}">${esc(l.label)}</td></tr>`;
    for (const m of l.metrics) {
      const vals = gws.map((g) => {
        const j = g[l.key];
        return j && j[l.flag] !== false && j[m.k] != null ? j[m.k] : null;
      });
      const bi = bestIndex(vals, m.best);
      h += `<tr><td class="metric">${esc(m.label)}</td>` + vals.map((v, i) => {
        if (v == null) {
          const j = gws[i][l.key];
          const na = j && j[l.flag] !== false ? { text: "n/a", note: "" } : naText(j, l.flag, l.err);
          return `<td class="na" title="${esc(na.note)}">${esc(na.text)}</td>`;
        }
        return `<td class="${i === bi ? "best" : ""}">${esc(m.fmt(v))}</td>`;
      }).join("") + `</tr>`;
    }
  }
  h += `</tbody></table></div>`;
  h += `<p class="fineprint">Best value per row is highlighted, decided by the measurement (lower latency and memory, higher throughput). Sweep overlays below use the sustained @20ms sweep.</p>`;
  h += `<div id="cmp-sweeps" class="sweeps"></div>`;
  document.getElementById("compare-body").innerHTML = h;

  const series = gws.map((g, i) => ({
    label: g.display, color: CMP_COLORS[i],
    sweep: g.perf && g.perf.served !== false ? g.perf.sweep_sustained_20ms : null,
  }));
  renderSweepCharts(document.getElementById("cmp-sweeps"), series, chartTheme());
}
function closeCompare(sync = true) {
  state.cmpOpen = false;
  document.getElementById("compare-panel").classList.add("hidden");
  if (sync) syncUrl(true);
}

/* ---- protocol matrix view --------------------------------------------------- */
/* v2: one 6x6 grid per gateway, rows = ingress dialect, cols = upstream (egress) dialect.
   Cell states: pass (green), fail (red), not configurable (neutral: the manifest defines no egress
   config for that dialect), unprobed_auth (grey), and n/a for egress columns a v1-era result never
   measured. The diagonal needs no translation; a faithful passthrough passes there by design and
   its verdict note says so. gen-data normalizes v1 results into the same upstreams shape. */
function matrixCell(g, egress, ingress) {
  const up = g.matrix.upstreams && g.matrix.upstreams[egress];
  return up && up.cells ? up.cells[ingress] : null;
}
/* Tooltip text for a cell. A grey (not_configurable) cell is the gateway's OWN declared
   incapability, so it shows the cited capability-limit reason (verdict_note) - never a bare
   "we didn't test it". Green/red show the verdict label + note as before. */
function matrixCellTip(cell) {
  const [, label] = cellState(cell);
  if (cell.served === "not_configurable")
    // HONEST wording: the capability grid is authored by the busbar team from each project's docs
    // as a stand-in until that project's maintainers confirm their own grid. So a grey cell is "not
    // in the grid we drafted / not tested", NOT a claim the gateway's own maintainer declined it.
    return `not tested (this cell is not in the capability grid we drafted from the project's docs; the maintainers have not confirmed their own grid yet)${cell.verdict_note ? ": " + cell.verdict_note : ""}`;
  if (cell.served !== true && cell.served !== "unprobed_auth" && isHarnessGap(cell))
    return `not verified: the harness could not get this gateway serving under this upstream config${cell.verdict_note ? " (" + cell.verdict_note + ")" : ""}`;
  return `${label}. ${cell.verdict_note || ""}`;
}
/* Per-cell perf line for a GREEN cell's tooltip/detail: this path's sustained RPS + added latency
   p99, and its deviation from THIS gateway's best cell (cell.rps / best.rps - 1, signed %). The
   best cell itself reads "best path". Grey/red/unprobed cells carry no perf and return "". */
function cellPerfTip(cell, ingress, egress, best) {
  const p = cell && cell.served === true ? cell.perf : null;
  if (!p || p.rps_sustained_20ms == null) return "";
  let s = `${fmtInt(p.rps_sustained_20ms)} req/s @20ms`;
  if (p.added_latency_p99_us != null) s += `, +${fmtInt(p.added_latency_p99_us)} µs p99 added`;
  if (best && best.rps_sustained_20ms > 0) {
    if (best.ingress === ingress && best.egress === egress) s += " - best path";
    else s += ` - ${fmtPct((p.rps_sustained_20ms / best.rps_sustained_20ms - 1) * 100)} vs best (${best.ingress}→${best.egress})`;
  }
  return s;
}
function renderMatrix() {
  const withMatrix = state.data.gateways.filter((g) => g.matrix && (g.matrix.upstreams || g.matrix.cells));
  if (!withMatrix.length) {
    document.getElementById("matrix-empty").classList.remove("hidden");
    document.getElementById("matrix-grid").classList.add("hidden");
    return;
  }
  /* per-gateway tallies over the full grid; sorted by measurement: pass count desc, then name */
  const tally = (g) => {
    const t = { pass: 0, fail: 0, notconf: 0, unprobed: 0, unverified: 0 };
    for (const e of MATRIX_CELLS) for (const c of MATRIX_CELLS) {
      const cell = matrixCell(g, e, c);
      if (!cell) continue;
      if (cell.served === true) t.pass++;
      else if (cell.served === "not_configurable") t.notconf++;
      else if (cell.served === "unprobed_auth") t.unprobed++;
      else if (isHarnessGap(cell)) t.unverified++;
      else t.fail++;
    }
    return t;
  };
  withMatrix.sort((a, b) => tally(b).pass - tally(a).pass || a.display.localeCompare(b.display));

  const grid = document.getElementById("matrix-grid");
  grid.innerHTML = withMatrix.map((g) => {
    const t = tally(g);
    const bits = [`<b class="pass-count">${t.pass}</b>/36 pass`];
    if (t.fail) bits.push(`${t.fail} fail`);
    if (t.notconf) bits.push(`${t.notconf} not declared`);
    if (t.unverified) bits.push(`${t.unverified} not verified`);
    if (t.unprobed) bits.push(`${t.unprobed} unprobed (auth)`);
    return `<section class="matrix-gw">
      <header class="matrix-gw-head"><h3>${
        g.repo ? `<a href="${g.repo}" target="_blank" rel="noopener">${esc(g.display)}</a>` : esc(g.display)
      }</h3><span class="muted">${bits.join(" · ")}</span></header>
      <div class="table-scroll matrix-table"><table>
        <thead><tr><th class="axis">ingress &#8595; \\ upstream &#8594;</th>${
          MATRIX_CELLS.map((e) => `<th>${esc(MATRIX_LABELS[e])}</th>`).join("")
        }</tr></thead><tbody>${
        MATRIX_CELLS.map((c) => `<tr><td class="name">${esc(MATRIX_LABELS[c])}</td>${
          MATRIX_CELLS.map((e) => {
            const cell = matrixCell(g, e, c);
            if (!cell) return `<td class="na" title="not measured (v1 result: this upstream dialect was not probed)">n/a</td>`;
            const [cls] = cellState(cell);
            const diag = e === c ? " diag" : "";
            // No native `title` here: the richer hover popup (cellPopHtml/showPop) carries the
            // verdict + perf, and a native title on top of it would double up.
            return `<td><span class="cell ${cls}${diag}" data-gw="${esc(g.key)}" data-egress="${esc(e)}" data-cell="${esc(c)}"></span></td>`;
          }).join("")
        }</tr>`).join("")
      }</tbody></table></div>
    </section>`;
  }).join("");

  // Floating hover popup: a single reused element that follows the hovered cell. Richer than the
  // native title tooltip (perf line + verdict + body), appears on hover, no click needed.
  let pop = document.getElementById("matrix-pop");
  if (!pop) {
    pop = document.createElement("div");
    pop.id = "matrix-pop";
    pop.className = "matrix-pop hidden";
    document.body.appendChild(pop);
  }
  const cellPopHtml = (g, ing, eg) => {
    const cell = matrixCell(g, eg, ing);
    if (!cell) return "";
    const [, label] = cellState(cell);
    const perf = cellPerfTip(cell, ing, eg, g.best_cell);
    return `<h4>${esc(g.display)}: ${esc(MATRIX_LABELS[ing])} in / ${esc(MATRIX_LABELS[eg])} upstream - ${esc(label)}${
      cell.status ? ` (HTTP ${esc(cell.status)})` : ""
    }</h4>` +
      (perf ? `<div class="pop-perf">${esc(perf)}</div>` : "") +
      (cell.verdict_note ? `<div class="pop-note">${esc(cell.verdict_note)}</div>` : "");
  };
  const showPop = (el) => {
    const g = state.data.gateways.find((x) => x.key === el.dataset.gw);
    const html = g && cellPopHtml(g, el.dataset.cell, el.dataset.egress);
    if (!html) return;
    pop.innerHTML = html;
    pop.classList.remove("hidden");
    const r = el.getBoundingClientRect();
    // position above the cell, clamped to the viewport
    const pr = pop.getBoundingClientRect();
    let left = r.left + window.scrollX + r.width / 2 - pr.width / 2;
    left = Math.max(8 + window.scrollX, Math.min(left, window.scrollX + document.documentElement.clientWidth - pr.width - 8));
    let top = r.top + window.scrollY - pr.height - 8;
    if (top < window.scrollY + 8) top = r.bottom + window.scrollY + 8;   // flip below if no room above
    pop.style.left = `${left}px`;
    pop.style.top = `${top}px`;
  };
  grid.querySelectorAll(".cell").forEach((el) => {
    el.addEventListener("mouseenter", () => showPop(el));
    el.addEventListener("mouseleave", () => pop.classList.add("hidden"));
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
};
function chartCaption(file) {
  const base = file.replace(/^charts\//, "").replace(/\?.*$/, "").replace(/\.png$/, "");
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
  /* Root-absolute src: the page URL may be a deep path (/gateways/charts), so a
     relative charts/ path would resolve under the route, not the site root. */
  gallery.innerHTML = ordered.map((c) =>
    `<figure data-src="/${esc(c.file)}"><img src="/${esc(c.file)}" alt="${esc(chartCaption(c.file))}" loading="lazy"><figcaption>${esc(chartCaption(c.file))}</figcaption></figure>`
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
  for (const suite of ["perf", "memory", "stream", "xlate", "matrix"]) {
    const a = document.getElementById(`lnk-${suite}`);
    if (a) a.href = `${repo}/blob/main/${suite}/run.sh`;
  }
  document.getElementById("repo-link").href = repo;
  const hw = document.getElementById("hw-stamp");
  const bits = [];
  if (state.data.hardware) bits.push(`Ran on: ${state.data.hardware}`);
  if (state.data.latest_measured_at) bits.push(`Latest measurement: ${stampWithAge(state.data.latest_measured_at)}`);
  bits.push(`Site data generated: ${state.data.generated_at ? stampWithAge(state.data.generated_at) : "unknown"}`);
  hw.textContent = bits.join(" · ");
}

/* ---- category nav + view tabs ----------------------------------------------- */
function viewPath(category, view) {
  return view && view !== "passthrough" ? `/${category}/${view}` : `/${category}`;
}

/* The category row above the tabs. One category today; new CATEGORIES entries
   appear here automatically. The links are real anchors (open-in-new-tab works)
   with the click intercepted into a pushState navigation. */
function renderCatNav() {
  const nav = document.getElementById("catnav");
  if (!nav) return;
  nav.innerHTML = `<span class="catnav-label">Benchmarking</span>` +
    Object.values(CATEGORIES).map((c) =>
      `<a class="cat${c.id === state.category ? " active" : ""}" data-cat="${esc(c.id)}" href="/${esc(c.id)}">${esc(c.label)}</a>`
    ).join("");
  nav.querySelectorAll("a.cat").forEach((a) => a.addEventListener("click", (ev) => {
    if (ev.metaKey || ev.ctrlKey || ev.shiftKey) return; /* let new-tab clicks through */
    ev.preventDefault();
    const fresh = newState();
    fresh.category = a.dataset.cat;
    applyState(fresh);
    syncUrl(true);
    ensureData().then(renderAll);
  }));
  const tagline = document.getElementById("tagline");
  const cat = CATEGORIES[state.category] || CATEGORIES[DEFAULT_CATEGORY];
  if (tagline) tagline.textContent = cat.tagline;
}

function showView(view) {
  state.view = view;
  // The three perf tabs share one table container (#view-table); matrix/method have their own.
  const containerId = PERF_VIEWS.has(view) ? "view-table" : `view-${view}`;
  document.querySelectorAll(".tab").forEach((x) => {
    x.classList.toggle("active", x.dataset.view === view);
    x.setAttribute("href", viewPath(state.category, x.dataset.view));
  });
  document.querySelectorAll(".view").forEach((v) => v.classList.toggle("hidden", v.id !== containerId));
  // Switching between perf tabs changes columns/caption/filtering, so re-render the table.
  if (PERF_VIEWS.has(view) && state.data) renderTable();
  updateTitle();
}
function initTabs() {
  document.querySelectorAll(".tab").forEach((t) => t.addEventListener("click", (ev) => {
    if (ev.metaKey || ev.ctrlKey || ev.shiftKey) return;
    ev.preventDefault();
    showView(t.dataset.view); syncUrl(true);
  }));
}

/* ---- boot ------------------------------------------------------------------- */
function applyState(st) {
  Object.assign(state, {
    category: st.category, view: st.view, q: st.q, sortCol: st.sortCol, sortDesc: st.sortDesc,
    langs: st.langs, classes: st.classes,
    needStream: st.needStream, needXlate: st.needXlate,
    cmp: st.cmp, cmpOpen: st.cmpOpen, drawer: st.drawer,
  });
}

/* Drop selections that reference gateways no longer in data.json (removed
   entrants linger in shared URLs); a shrunken compare set must not leave the
   panel open on a partial table. */
function sanitizeState() {
  const gws = state.data.gateways;
  state.cmp = state.cmp.filter((k) => gws.some((g) => g.key === k));
  if (state.cmp.length < 2) state.cmpOpen = false;
  if (state.drawer && !gws.some((g) => g.key === state.drawer)) state.drawer = null;
}

function renderAll() {
  renderCatNav();
  showView(state.view);
  renderFilters();
  renderTable();
  renderCompareBar();
  renderMatrix();
  renderCharts();
  renderStatic();
  if (state.drawer) openDrawer(state.drawer);
  if (state.cmpOpen && state.cmp.length >= 2) renderCompare();
}

/* Fetch the current category's data bundle if it is not the one already loaded.
   With one category this runs once at boot; the seam is what a future second
   category navigates through. */
let loadedCategory = null;
function ensureData() {
  const cat = CATEGORIES[state.category] || CATEGORIES[DEFAULT_CATEGORY];
  if (loadedCategory === cat.id && state.data) return Promise.resolve(state.data);
  return fetch(cat.data)
    .then((r) => { if (!r.ok) throw new Error(`${cat.data}: HTTP ${r.status}`); return r.json(); })
    .then((data) => {
      state.data = data;
      loadedCategory = cat.id;
      sanitizeState();
      return data;
    });
}

function boot() {
  applyState(decodeUrl(location.pathname, location.search, location.hash));
  ensureData()
    .then(() => {
      syncUrl(false); /* normalize: / -> /gateways, legacy #hash -> path form */
      initTabs();
      initFilterControls();
      initThemeToggle();
      renderAll();

      document.getElementById("backdrop").addEventListener("click", closeDrawer);
      document.getElementById("drawer-close").addEventListener("click", closeDrawer);
      document.getElementById("compare-close").addEventListener("click", () => closeCompare());
      document.addEventListener("keydown", (ev) => {
        if (ev.key !== "Escape") return;
        if (state.drawer) closeDrawer();
        else if (state.cmpOpen) closeCompare();
      });
      window.addEventListener("popstate", () => {
        applyState(decodeUrl(location.pathname, location.search, location.hash));
        ensureData().then(() => {
          sanitizeState();
          if (!state.drawer) { document.getElementById("drawer").classList.add("hidden"); document.getElementById("backdrop").classList.add("hidden"); }
          if (!state.cmpOpen) document.getElementById("compare-panel").classList.add("hidden");
          renderAll();
        });
      });
    })
    .catch((err) => {
      document.querySelector("main").innerHTML =
        `<p class="muted">Could not load site data (${esc(err.message)}). Run <code>node site/gen-data.mjs</code> first.</p>`;
    });
}

if (NODE) {
  /* Exports for the node smoke test (site/test.mjs). */
  module.exports = {
    newState, encodeUrl, decodeUrl, viewPath, applyFilters,
    fmtStamp, fmtAge, stampWithAge,
    drawSweep, niceStep, fmtTick, COLUMN_SETS, columnsFor, PERF_VIEWS, VIEW_SORT, LANES, naText,
    cellState, matrixCellTip, cellPerfTip, passCell, xlateCell, hasTranslation, CATEGORIES, DEFAULT_CATEGORY, VIEWS,
  };
} else {
  boot();
}
