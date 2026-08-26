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
    // Home-page CTA card copy; homeCardsHtml prefixes the live entrant count when the
    // category's data bundle is loaded ("13 self-hostable AI gateways, ...").
    card: "Self-hostable AI gateways, measured for overhead, throughput, streaming, and protocol translation.",
    data: "/data.json",
  },
};
const DEFAULT_CATEGORY = "gateways";
/* The site root (/) is the HOME landing page: the level ABOVE the categories.
   It is not a category tab; the category nav and view tabs render only inside a
   category. Encoded as a pseudo-view so state/URL plumbing stays one codepath. */
const HOME_VIEW = "home";
// Each perf tab ranks an internally coherent path so a single sort is honest:
//   passthrough = openai->openai only (no translation); translation = openai-in -> best non-openai
//   egress; streaming = SSE passthrough (its own stall-gated ceiling).
// Tab order: neutral roster (`gateways`, alphabetical, no perf numbers) first, rankings second,
// matrix + method last; `charts` folds into method.
// `performance` is one cell-chooser-driven tab (Peak | Same | Custom picks the cell of the 6x6 run).
// `frontier` is its own tab rather than more Performance columns: "what shape is this gateway" is a
// different question from "who is fastest at my bound", and less scrolling wins.
const VIEWS = ["gateways", "memory", "performance", "frontier", "streaming", "matrix", "charts", "method"];
const VIEW_LABELS = { gateways: "Gateways", memory: "Memory", performance: "Performance", frontier: "Frontier", streaming: "Streaming", matrix: "Protocol matrix", charts: "Charts", method: "Method" };
// The default (bare /gateways) view: the roster overview.
const DEFAULT_VIEW = "gateways";
// The tabs whose columns read a PERF/STREAM cell of the one 6x6 run (Peak | Same | Custom).
const PERF_VIEWS = new Set(["performance", "frontier", "streaming", "charts"]);
// The views that render the shared results table (#view-table).
const TABLE_VIEWS = new Set(["performance", "frontier", "streaming", "memory"]);
// The views the CELL CHOOSER drives. Memory chooses its cell like every other lane, with its OWN
// mode set (below).
const CHOOSER_VIEWS = new Set(["performance", "frontier", "streaming", "memory", "charts"]);
// The views whose numbers are READ AT A TAIL-LATENCY BOUND, so the bound selector belongs on them. It is
// not a global control: nothing on Streaming or Memory is read at a bound, and a control that changed
// nothing would imply those numbers had a bound too.
const BOUND_VIEWS = new Set(["performance", "frontier", "charts"]);
// Maps retired view names onto current tabs so old shared links keep resolving. `translation`
// aliases to `performance` (its ?xin/?xout still decode into the Custom in/out below).
const VIEW_ALIASES = { results: "performance", peak: "performance", matched: "performance", passthrough: "performance", translation: "performance" };
// Each perf tab's default (headline) sort column; a clean URL omits the sort when it equals this.
// Streaming defaults to added TTFT, not streams-sustained: the sustained count saturates at the
// harness cap and ties break by name, floating a slow-TTFT gateway above a fast one. Added TTFT
// doesn't saturate.
// "f10" is `boundColId(DEFAULT_BOUND_MS)` written out (this object initialises before that constant
// exists); site/test.mjs asserts the two agree.
const VIEW_SORT = { performance: "rps", frontier: "f10", streaming: "sttft", memory: "mempeak" };
/* Retired sort ids, remapped so old shared permalinks still land on a ranking. `rps20`/`rpsmax`
   named scalar metrics that no longer exist; both meant "rank by throughput", which the frontier
   reading at the selected bound now is. */
const SORT_ALIASES = { rps20: "rps", rpsmax: "rps", cpufps: "streamfps" };
/* Cell-chooser modes shared by Performance + Streaming (which cell(s) of the 6x6 run to show):
     peak   — each gateway on its own representative same-dialect diagonal (best_cell). Default.
     same   — one picked dialect's diagonal (X->X) for every gateway.
     custom — any ingress->egress cell (incl. translation) for every gateway.

   `peak` is a URL contract, not a description: `?mode=peak` is in shared links so the token stays,
   but the UI label is "Own cell" (MODE_LABELS). Per gen-data.mjs `bestCell`, peak picks the openai
   diagonal when served, else the lowest-added-latency-p99 diagonal - it never reads throughput, so
   it is not a maximum of anything and the tail-latency bound can't change the pick (kong's diagonals
   span 3,903-22,891 req/s at the same bound, so "highest throughput" would be wrong by ~6x). */
const CHOOSER_MODES = new Set(["peak", "same", "custom"]);
/* Memory lane's own mode set: Min | Max | Same | Custom. No `peak`: best_cell ranks on latency, so
   using it for memory would select on one axis and report another. Min/Max select on memory and
   report memory. Candidate sets differ per gateway (min-of-26 vs min-of-1), so the row also shows
   the cell count. */
const MEM_CHOOSER_MODES = new Set(["min", "max", "same", "custom"]);
// The modes a view offers, and the mode it lands on when none is pinned.
// Memory defaults to Min, not Same: Same picks one shared dialect cell, so a gateway not serving it
// drops out of the default view (e.g. one-api, which declares only openai). Min uses each gateway's
// own lowest cell so nobody drops out, and the row states the candidate-set size.
function modesFor(view) { return view === "memory" ? MEM_CHOOSER_MODES : CHOOSER_MODES; }
function defaultMode(view) { return view === "memory" ? "min" : "peak"; }
/* Which chooser family a view belongs to. Perf lanes offer Peak/Same/Custom; memory offers
   Min/Max/Same/Custom - they overlap on Same/Custom only, so one carried-across `mode` can't serve both. */
function modeFamily(view) { return view === "memory" ? "memory" : "perf"; }
/* Coerce a mode onto a view that offers it (e.g. a shared ?mode=peak link landing on Memory falls
   back to Same rather than rendering a throughput-selected memory number). */
function resolveMode(mode, view) { return modesFor(view).has(mode) ? mode : defaultMode(view); }
/* The mode and family memo after navigating fromView->toView. Each family remembers its own mode
   (stashed on the way out) so a tab flip is lossless in both directions - coercing without memoing
   would make Performance(Custom) -> Memory -> Performance land on Min instead of the original Custom.
   A same-family arrival (incl. re-render of the current view) must NOT consult the memo, since memo is
   pre-seeded and would otherwise silently overwrite the mode decodeUrl just parsed from the URL.
   Pure/exported so the round trip is assertable without a browser. */
function modeOnArrival(fromView, toView, mode, memo) {
  const leaving = modeFamily(fromView), arriving = modeFamily(toView);
  if (leaving === arriving) return { mode: resolveMode(mode, toView), memo };
  return { mode: resolveMode(memo[arriving] ?? mode, toView), memo: { ...memo, [leaving]: mode } };
}
// Choke point for the memory lane's mode: guarantees a stale/hand-built state with mode:"peak" still
// reads as Same rather than a peak-selected memory number.
function memoryMode(st = state) { return MEM_CHOOSER_MODES.has(st.mode) ? st.mode : defaultMode("memory"); }
/* Segmented control's copy, one entry per mode across both mode sets. `peak` reads "Own cell" (see
   CHOOSER_MODES); Min/Max keep their names since they are real extrema, unlike peak. */
const MODE_LABELS = { peak: "Own cell", min: "Min", max: "Max", same: "Same", custom: "Custom" };
const MODE_TIPS = {
  peak: "Each gateway on its own representative same-dialect diagonal: its OpenAI passthrough where it serves one, otherwise its lowest-added-latency diagonal. Not the highest-throughput cell - switching the tail-latency bound cannot change which cell this picks.",
  min: "Each gateway's LOWEST steady-state cell (selected on memory, reported as memory)",
  max: "Each gateway's HIGHEST steady-state cell (selected on memory, reported as memory)",
  same: "One chosen dialect's identity cell for every gateway",
  custom: "Any ingress→egress cell for every gateway",
};

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
/* Rate is the one metric that can legitimately be below 1. `Math.round` would send 0.25 req/s to "0",
   which this board reads as "measured, carried nothing" - so keep two decimals below 1/s, matching
   GenStats::rps's own split, and a whole number at or above it. */
const fmtRate = (v) => (v > 0 && v < 1 ? v.toFixed(2) : fmtInt(v));
// Added-latency deltas are shown raw (no noise-floor smoothing): the paced stream suite's per-frame
// value is noise-dominated and can flip sign run-to-run, so smoothing here would just hide that.
const fmtAdded = fmtInt;
const fmt1 = (v) => v.toLocaleString("en-US", { minimumFractionDigits: 1, maximumFractionDigits: 1 });
/* Three significant figures because the quantity can be tiny (e.g. 0.008 MiB): fmt1 would round it
   to "0.0", a false "nothing happened". maximumSignificantDigits keeps small values legible without
   over-precision on large ones. */
const fmt2 = (v) => v.toLocaleString("en-US", { maximumSignificantDigits: 3 });
// Streaming latency cells: the column is µs (headers say so), but several gateways land in the
// hundreds of ms where a bare "596,693" invites misreading. Annotate any value >= 1 ms with its
// ms equivalent ("596,693 (596.7 ms)"); the charts' auto-ms relabel tells the same story.
const fmtUsMs = (v) => (v >= 1000 ? `${fmtInt(v)} (${fmt1(v / 1000)} ms)` : fmtAdded(v));
const fmtPct = (v) => `${v > 0 ? "+" : ""}${v.toFixed(1)}%`;

/* Footer timestamps: clean UTC date/time plus a coarse relative age (hours/days only, deliberately
   imprecise), computed client-side so it stays fresh without a rebuild. Pure; covered by site/test.mjs. */
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

/* Per-gateway freshness badge. Each gateway is measured + published independently, so mixed per-row
   ages are expected, not a bug. Shows the row's own measured_at plus, when gen-data set g.stale (data
   aged past MAX_GATEWAY_AGE_DAYS), a greyed "stale" pill. Returns "" with no measurement.
   Pure; covered by site/test.mjs. */
function measuredBadge(g, now = Date.now()) {
  if (!g || !g.measured_at) return "";
  const age = fmtAge(g.measured_at, now);
  const rel = age ? `measured ${age}` : "measured";
  const stalePill = g.stale
    ? ` <span class="stale-pill" title="This gateway's data has aged past the freshness threshold; re-run it to refresh.">stale</span>`
    : "";
  return `<span class="measured-at${g.stale ? " stale" : ""}" title="${esc(stampWithAge(g.measured_at, now))}">${esc(rel)}</span>${stalePill}${engineBadge(g)}`;
}

/* Read defensively: called from measuredBadge(), which site/test.mjs drives under Node, where there
   is no window and no live state. */
function boardBenchmarkVersion() {
  try {
    return (typeof state !== "undefined" && state.data && state.data.benchmark_version) || null;
  } catch {
    return null;
  }
}

/* Which harness measured this row: a row on an older engine isn't necessarily wrong but isn't
   comparable to the rest of the board. Current rows show the sha quietly; an older one is flagged.
   Returns "" when neither the row nor the board carries an engine (nothing to compare).
   Pure; covered by site/test.mjs. */
function engineBadge(g, boardEngine = boardBenchmarkVersion()) {
  const e = g && g.engine;
  if (!e) return "";
  if (!e.sha) {
    return boardEngine
      ? ` <span class="engine-pill old" title="This row carries no benchmark version at all: it predates the stamp, so which harness measured it cannot be established. The board is on ${esc(boardEngine.slice(0, 7))}.">engine unknown</span>`
      : "";
  }
  const cls = e.current ? "engine-pill" : "engine-pill old";
  const title = e.current
    ? `Measured by benchmark ${e.sha}, which is the version the rest of the board was measured on.`
    : `Measured by benchmark ${e.sha}, but the board is on ${boardEngine ? boardEngine.slice(0, 7) : "a newer version"}. Numbers from two different harnesses are not directly comparable - re-run this gateway to refresh it.`;
  return ` <span class="${cls}" title="${esc(title)}">${esc(e.short)}</span>`;
}

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

/* Gateway name, linked to its repo when present. Single function (was duplicated at 4 call sites,
   each a separate escaping question) so one test covers every href the board emits. gen-data
   validates the URL scheme on the way in; this is the second line of defence. */
function gwLink(g) {
  const name = esc((g && g.display) ?? "");
  return g && g.repo ? `<a href="${esc(g.repo)}" target="_blank" rel="noopener">${name}</a>` : name;
}

/* The benchmarking repo where config corrections are filed. */
const BENCH_REPO = "https://github.com/GetBusbar/benchmarking";

/* Per-gateway deep link to a pre-filled GitHub issue (config-correction template) so anyone can
   propose a fix to a gateway's published config. GitHub issue Forms map ?<field-id>=<value> onto
   form fields, so `gateway=<display>` fills the template's "gateway" input. */
function configCorrectionUrl(g) {
  const label = g.display || g.key;
  const p = new URLSearchParams({
    template: "config-correction.yml",
    title: `Config correction: ${label}`,
    labels: "config-correction",
    gateway: label,
  });
  return `${BENCH_REPO}/issues/new?${p.toString()}`;
}

// Absolute rig paths (bench box filesystem) are harness noise, not evidence; scrub before display.
const RIG_PATH_RE = /(?:file:\/\/)?\/(?:home|root)\/[^\s'"):,]+/g;
function stripRigPaths(s) {
  return String(s || "").replace(RIG_PATH_RE, "<rig path>");
}

/* Short cell label for a served-flag that is a token, not a boolean (e.g. `stream_served` can be
   true/false/"not_probed"/etc - record.rs). Renders the token's meaning; raw tokens like
   `search_exhausted` must never leak to the public board as-is. Unknown tokens render "not available"
   rather than guess. */
const STATUS_LABEL = {
  not_measured: "not measured", not_probed: "not measured", not_served: "not served",
  untestable: "not testable", rig_limited: "rig-limited", search_exhausted: "search exhausted",
  harness_error: "harness error", below_resolution: "below resolution",
};
/* Compact label for a lane that was not served. Suite diagnostic notes are long and must never be
   dumped as metric values; the cell shows a short badge, the full (scrubbed) note rides the tooltip,
   and the drawer shows the first line plus a folded Evidence block. */
function naText(j, flag, errKey) {
  if (!j) return { text: "not measured", note: "" };
  const note = stripRigPaths(j[errKey] || j.serve_error || "");
  let text = "not served";
  // A refusal and a lane that never ran are different findings and must not share a label: the
  // served flag isn't always boolean (StreamServed can be a status token - record.rs), and a
  // non-true value must not collapse into "did not stream", which asserts a measured refusal.
  const status = j[flag];
  if (typeof status === "string")
    return { text: STATUS_LABEL[status] || "not available", note: note || METRIC_NOTES[status] || "" };
  // A lane the gateway never claimed is "not declared", never a failure - same rule as the matrix.
  if (j.xlate_declared === false) text = "not declared";
  else if (j.xlate_passthrough === true || note.startsWith("UNTRANSLATED passthrough")) text = "n/a (passthrough)";
  // "manifest defines no <hook>": this harness never implemented the probe for this gateway.
  // That's "not tested", not "not supported" - never assert a capability verdict we didn't exercise.
  else if (note.includes("manifest defines no")) text = "not tested";
  // Boot/build failure is our environment failing to start the gateway, not a refusal: must read
  // as "did not run", never as a capability verdict. Same rule as the protocol matrix.
  else if (String(j.last_http_status || "") === "000" || /failed to boot|no such file|not listening|never became ready|build failed/i.test(note)) text = "did not run";
  else if (flag === "stream_served") text = "did not stream";
  return { text, note };
}

/* If the suite file exists but the served flag is false, surface a compact label (full note in
   .note); if the file is absent, "not measured". */
/* Only `true` counts as served; `false` and status tokens do not. A flag the record doesn't carry
   at all is treated as served, since legacy per-suite records predate the flag. */
function laneServed(j, flag) {
  if (!j) return false;
  const v = j[flag];
  return v == null ? true : v === true;
}
function lane(g, suite, flag, errKey, pick) {
  const j = g[suite];
  if (!laneServed(j, flag)) {
    const na = naText(j, flag, errKey);
    return { v: null, text: na.text, note: na.note, na: true };
  }
  return pick(j);
}

/* ---- the data-honesty reader (Design E §2.3) --------------------------------
   Every metric in data.json is a sealed envelope ({value, certified, suppressed, reason?, note?, ...})
   emitted by gen-data.mjs. The honesty gate lives upstream at seal time; a suppressed metric has
   value:null and the raw number is gone from the bundle, so the reader has no gate logic to bypass.
   This is the one accessor every surface reads a metric through.
     metric(env)        -> { v, text, na, note, source, env } ; v is null (na:true) when not shown.
   `fmt` formats the value; a suppressed/absent metric reads "n/a". */
function isEnvelope(x) { return x != null && typeof x === "object" && typeof x.certified === "boolean"; }
// The envelope's machine token -> the sentence a reader sees. A measured failure (certified 0) and
// not measured (null) are different states and must read differently.
const METRIC_NOTES = {
  // Only the streaming counts can carry this zero; it names the stream delivery gate specifically
  // (seal.mjs emits it only for streams_sustained / streams_sustained_fps).
  no_qualifying_ceiling: "served, but no tested concurrency held the stream delivery gate (every expected frame delivered, no stall, under 0.1% of streams erroring), so there is no qualifying ceiling to publish",
  measured_failure: "MEASURED FAILURE: the gateway was offered the load and sustained none of it (a real 0, not an unmeasured cell)",
  // No `mock_bound`/`unverifiable`: those were suppression reasons (measurements near the rig ceiling
  // hidden as null). The engine now publishes the number with its ceiling/fraction instead.
  not_measured: "not measured: no reading exists for this cell",
  // Engine's own absence reasons (measurement.rs Absent). below_resolution is a display state in
  // metric(), not a hole.
  below_resolution: "below measurement resolution: the comparison ran and the gateway's overhead was too small for this rig to detect (the best result this test can express)",
  rig_limited: "not shown: rig-limited, the harness's own ceiling bounded this number, so it is not a gateway reading",
  untestable: "not testable: the rig cannot pose this question for this dialect (a rig limit, not a gateway fault)",
  // Now emitted only by the streaming ceiling (top rung still passing is our choice, not the
  // gateway's answer). The throughput lane no longer reaches this - a frontier reading in that state
  // publishes as a floor ("≥ 19,000") instead.
  search_exhausted: "not shown: the search ran off the end of its range still improving, so any number would be a lower bound, not a ceiling",
  harness_error: "not shown: the harness itself failed here; this says nothing about the gateway",
  not_served: "the gateway does not serve this pairing",
};
function noteText(tok) { return (tok && METRIC_NOTES[tok]) || tok || ""; }
/* Comparison-against-our-own-rig sentence, from the fraction and ceiling the envelope carries (not
   re-derived here). Ceiling is named when known. Above 100% is rendered as-is: a gateway adding its
   own SSE framing can legitimately carry more events/sec than the mock's own layout. */
function headroomText(frac, ceiling) {
  const pct = frac >= 0.1 ? (frac * 100).toFixed(0) : (frac * 100).toFixed(1);
  const of = Number.isFinite(ceiling) ? ` (${fmtInt(ceiling)})` : "";
  return `${pct}% of this rig's own ceiling${of} at the same concurrency`;
}
// Short on-cell form of a measured zero's meaning (full sentence stays on the tooltip). A plain zero
// with no note renders bare.
const ZERO_WHY = {
  no_qualifying_ceiling: "no load held the gate",
  measured_failure: "measured failure",
};
/* The one `<td>` writer for every plain (non-render) column. A measured zero says what it means on
   the cell (ZERO_WHY short form), not only in the hover tooltip. */
function metricTd(cell, sc = "") {
  if (cell.na)
    return `<td class="na${cell.failed ? " failcell" : ""}${sc}" title="${esc(cell.note || "")}">${esc(cell.text)}</td>`;
  // `cell.why` is an explicit short on-cell reason for states an envelope note token can't carry:
  // e.g. a frontier reading where no rung held the bound is a measured 0, and "0" alone reads as "no data".
  const zeroWhy = cell.why || (cell.v === 0 && cell.env && ZERO_WHY[cell.env.note]);
  return `<td class="${sc.trim()}"${cell.note ? ` title="${esc(cell.note)}"` : ""}>${esc(cell.text)}${
    zeroWhy ? `<span class="zero-why">${esc(zeroWhy)}</span>` : ""}</td>`;
}
function metric(env, fmt = fmtInt) {
  if (!isEnvelope(env) || env.value == null) {
    // Below resolution is not a hole: the comparison ran and the difference was at or under what
    // the rig can resolve. Displays as "≈0", ranks as 0 (equal-best), engine's prose in the tooltip.
    if (env && env.reason === "below_resolution")
      return { v: 0, text: "≈0", na: false, note: env.detail || noteText(env.reason), env };
    // A measured failure reads as one, in red, with its counts - never as the same n/a an untested
    // cell gets. Only when the detail blames the gateway's own leg, though: the engine emits the
    // same sentence shape for our reference rig's direct-to-mock leg failing, which is not the
    // gateway's fault and must not be painted red. A rig-side failure renders as a plain n/a.
    const detail = env && env.detail;
    const okFail = detail && /the gateway leg.*?(\d+) ok, (\d+) fail/.exec(detail);
    if (okFail && Number(okFail[1]) === 0 && Number(okFail[2]) > 0)
      return { v: null, text: `failed · 0/${fmtInt(Number(okFail[2]))}`, na: true, failed: true, note: detail, env };
    if (detail && /no stream frame arrived from the gateway/.test(detail))
      return { v: null, text: "failed · 0 frames", na: true, failed: true, note: detail, env };
    // "n/a" means not applicable (the gateway doesn't serve this pairing), not "harness declined to
    // publish a result". `not_served`/`untestable` are capability limits (n/a is right); `not_measured`
    // means the harness ran and got a result but withheld it - the two must not share a label.
    const reason = env && env.reason;
    // When our own rig is why (a direct-to-mock leg failure, or a rung found but unconfirmed),
    // the cell must say so rather than charge the absence to the gateway.
    const rigSide = !!(detail && /(direct-to-mock leg|from the mock directly|did not hold on re-measurement)/.test(detail));
    const text = rigSide ? "unconfirmed"
      : reason === "not_measured" ? "not measured"
      : reason === "harness_error" ? "rig fault"
      : "n/a";
    return { v: null, text, na: true, rigSide, note: detail || noteText(reason), env: env || null };
  }
  // A certified number can carry more than one note worth saying (zero's meaning, paced-match signal,
  // and on legacy rows a provenance stamp when the number came from a different run than its record -
  // e.g. cpu_fps from the streamcpu suite under a stream-suite record). Each renders only if present.
  const notes = [];
  if (env.note) notes.push(noteText(env.note));
  // How close this came to our own rig's ceiling, stated rather than acted on (replaces a past
  // suppression of near-ceiling numbers). Shown as a percentage: 99% = kept pace, 20% = own limit.
  if (Number.isFinite(env.headroom)) notes.push(headroomText(env.headroom, env.rig_ceiling));
  if (env.source && (env.source.build || env.source.measured_at))
    notes.push(`from a separate run than the rest of this record: build ${env.source.build || "?"}, measured ${env.source.measured_at || "?"}`);
  return { v: env.value, text: fmt(env.value), na: false, note: notes.join(" · "), env };
}
// mval: the bare displayable value of an envelope (null when suppressed/absent). For arithmetic
// (deltas, best-of ranking) where only the number matters. Never returns a suppressed number.
// A below-resolution absence ranks as 0, the same value metric() displays it as: the comparison ran
// and found nothing the rig could weigh, which is equal-best, not missing.
/* mval() for a metric whose value is a code, not a magnitude. mval maps a below_resolution absence
   to 0, which is honest for a magnitude but not for a code (0 already means something, e.g. shape 0
   = oscillating), so this keeps that case null instead. */
function mcode(env) {
  if (isEnvelope(env) && env.reason === "below_resolution") return null;
  return mval(env);
}

function mval(env) {
  if (!isEnvelope(env)) return null;
  if (env.value != null) return env.value;
  return env.reason === "below_resolution" ? 0 : null;
}

/* ---- the latency-throughput frontier ----------------------------------------
   Replaces two retired throughput scalars (`rps_sustained_20ms`, `rps_max_proxy`) that collapsed the
   same concurrency sweep to one number at a fixed latency ceiling. Each perf record now carries
   `frontier`: one sealed reading per declared tail-latency bound (1, 5, 10, 50, 100 ms) plus one
   unbounded reading, ascending, from seal.mjs `sealFrontier`.

   The headline finding is the SHAPE of the curve, not any one point - two gateways with similar rates
   at one bound can diverge sharply at another. So every throughput surface shows the shape beside the
   number (frontierSpark + gain factor), and the bound a number was read at is always named and
   switchable. */
// Declared bounds, ms. Mirrors seal.mjs FRONTIER_BOUNDS_MS/DEFAULT_BOUND_MS (itself mirroring the
// engine's frontier::P99_BOUNDS_US); app.js can't import that module in-browser, so it's duplicated
// and site/test.mjs asserts the two agree.
const FRONTIER_BOUNDS_MS = [1, 5, 10, 50, 100];
// Which bound the board opens on - a view, not a verdict (seal.mjs says why 10ms). Every bound is
// published on every cell and switchable.
const DEFAULT_BOUND_MS = 10;
// Readings in published order: each declared bound, then unbounded (`null` = no latency bound, zero
// failures only). `null` is a first-class choice, not a missing value.
const BOUND_CHOICES = [...FRONTIER_BOUNDS_MS, null];
// A bound's short name, for a control or a column header.
function boundLabel(ms) { return ms == null ? "no bound" : `${ms} ms`; }
/* Standard phrasing: "18,995 req/s while 99% of requests finished under 10 ms" - not "rps at 10 ms",
   which reads as a category error. Every caption/tooltip/header renders this clause. */
function boundClause(ms) {
  return ms == null
    ? "under no latency bound at all, having failed no request it accepted"
    : `while 99% of requests finished under ${ms} ms`;
}
// Column header for a reading at `ms`. Names the bound explicitly, always (a past board's captions
// stated a bar the engine didn't actually enforce). Standalone form for Performance's single ranked
// column and the compare panel.
function boundColLabel(ms) { return ms == null ? "Req/s · no bound" : `Req/s · 99% under ${ms} ms`; }
/* Spanning group header over Frontier's six per-bound columns (which show just "1 ms" etc beneath).
   Must carry the same "99% of requests" qualifier as boundClause(), since it's the only place that
   appears on this table. */
const BOUND_GROUP_LABEL = "Req/s · 99% of requests under:";
// Frontier tab's per-bound column id. Stable, so a shared ?sort=f10 keeps resolving.
function boundColId(ms) { return ms == null ? "fnone" : `f${ms}`; }
// Read defensively: these helpers are called from renderers the node suite drives with a hand-built
// state, and from column labels that take no arguments.
/* The concurrency the board is ranked at, or null for each gateway's own peak.
   Exists because showing each gateway at its own peak concurrency renders misleadingly (e.g.
   "77,248 @ 128 conc" beside "44,475 @ 32 conc" looks like 4x the concurrency was handed to one
   entrant, even though it's an honest per-gateway measurement). Picking one rung and reporting what
   every gateway carried there lets a stable ratio across the ladder prove the difference is real.
   A gateway that never drove the chosen rung reads n/a rather than being interpolated. */
function selectedConc(st = (typeof state !== "undefined" ? state : null)) {
  if (!st || st.conc == null) return null;
  return Number.isFinite(st.conc) ? st.conc : null;
}
// Rung readings for the chosen cell, or [] for a record measured before rungs were published.
function rungsOf(rec) {
  return rec && Array.isArray(rec.rungs) ? rec.rungs : [];
}
// Sealed reading at one concurrency, or null when this gateway never drove that rung.
function rungAt(rec, conc) {
  return rungsOf(rec).find((r) => r.conc === conc) || null;
}
// Every concurrency any gateway on this board drove, ascending. Derived from the run so a board
// swept over a different ladder offers that ladder rather than one hardcoded here.
function concChoices(data = (typeof state !== "undefined" ? state.data : null)) {
  const seen = new Set();
  for (const g of (data && data.gateways) || [])
    for (const rec of [g.best_cell, g.translation_cell])
      for (const r of rungsOf(rec)) if (Number.isFinite(r.conc)) seen.add(r.conc);
  return [...seen].sort((a, b) => a - b);
}
function selectedBound(st = (typeof state !== "undefined" ? state : null)) {
  if (!st || !("bound" in st)) return DEFAULT_BOUND_MS;
  return st.bound === null || FRONTIER_BOUNDS_MS.includes(st.bound) ? st.bound : DEFAULT_BOUND_MS;
}
// Sub-millisecond tails are real and common (e.g. 584 µs), so they keep their own unit rather than
// rounding to "0.6 ms".
function fmtTail(us) { return us < 1000 ? `${fmtInt(us)} µs` : `${fmt1(us / 1000)} ms`; }
// The record's readings, or [] - the normal shape for a snapshot measured before frontier existed.
function frontierOf(rec) { return rec && Array.isArray(rec.frontier) ? rec.frontier : []; }
/* The reading taken at one bound. Mirrors seal.mjs's `frontierAt` and charts.py's `_frontier_at`, so
   every surface reads the same reading for the same bound. `null` selects the unbounded reading -
   a bound value, never "unset". */
function frontierAt(frontier, boundMs) {
  if (!Array.isArray(frontier)) return null;
  return frontier.find((r) => (boundMs == null ? r.bound_ms == null : r.bound_ms === boundMs)) || null;
}
// A cell with no frontier shows no throughput - never a 0, never a blank that reads as one.
const NO_FRONTIER_NOTE = "no frontier in this record: it was measured before the throughput frontier " +
  "existed, or this cell has not been re-measured. There is no throughput to show - which is not the same " +
  "as a throughput of zero.";
/* The whole reading, in words, with its evidence (engine publishes both `concurrency`, where the
   winning rate was observed, and `first_disqualified_conc`, the next rung that stopped qualifying -
   frontier.rs). `p99_us` is the observed tail, never the bound - they can differ a lot. */
function readingSentence(rd, v) {
  // fmtRate, not fmtInt: rounding would put "0 req/s" one hover away from a cell reading 0.25.
  const bits = [`${fmtRate(v)} req/s ${boundClause(rd.bound_ms)}.`];
  if (rd.concurrency != null) bits.push(`Observed with ${fmtInt(rd.concurrency)} concurrent requests in flight.`);
  if (rd.p99_us != null) bits.push(`The tail it actually produced there was ${fmtTail(rd.p99_us)}.`);
  if (rd.lower_bound === true)
    // A floor, not a ceiling: the sweep ran out of ladder while this rung still qualified, so the
    // rate is real but maximality isn't established.
    bits.push("The sweep ran out of ladder with this concurrency still qualifying, so this is a FLOOR (≥), " +
      "not a maximum: the gateway carries at least this much and we did not look higher.");
  else if (rd.first_disqualified_conc != null)
    bits.push(`The next concurrency probed above it (${fmtInt(rd.first_disqualified_conc)}) stopped qualifying, which is what establishes this as the boundary.`);
  return bits.join(" ");
}
/* The {v, text, na, note} cell shape every table column and popup uses, for one record at one bound.
   Rate is read through metric() so an absent reading surfaces the engine's own reason (frontier.rs
   `absence_for`) instead of a flattened hole. A `lower_bound` reading renders "≥ 19,000". */
function frontierCell(rec, boundMs) {
  const f = frontierOf(rec);
  if (!f.length) return { v: null, text: "no frontier", na: true, note: NO_FRONTIER_NOTE };
  const rd = frontierAt(f, boundMs);
  if (!rd)
    return { v: null, text: "n/a", na: true,
      note: `this record publishes no reading at ${boundLabel(boundMs)}` };
  const c = metric(rd.rps, fmtRate);
  if (c.na) return c;   // the engine's absence reason, rendered by the one accessor
  /* "Measured, and it cannot do this" is not "not measured", and the two must never look alike.
     `below_resolution` means rungs served cleanly but none held this bound (frontier.rs `absence_for`).
     A dash or "n/a" would read as neutral "no data" for what is a damning measurement, flattering the
     slowest gateways. So it ranks as 0 (metric() already does that) and says why on the cell: "0" with
     "no rung held this tail", distinct from "no frontier" (never measured at all).
     Detected via mcode(), not the envelope's raw `.value` (invariant C5: display rules live in
     accessors) - mcode returns null for below-resolution while mval coerces it to 0. */
  if (mcode(rd.rps) == null) return { ...c, text: "0", why: "no rung held this tail", reading: rd };
  const floor = rd.lower_bound === true;
  return { ...c, text: floor ? `≥ ${c.text}` : c.text, note: readingSentence(rd, c.v), reading: rd, floor };
}
/* The shape as one number: what fraction of its full (unbounded) rate the cell still carried at the
   tightest tail-latency bound it holds at all. Replaced a raw gain factor ("×1.0 from 1 ms"), which
   was ambiguous (×1.0 read like an unfilled default) and required assembling meaning from three
   scattered pieces. A percentage of full rate is self-explanatory (more is better).

   Denominator is always the unbounded reading (never the 100ms one), since the frontier is monotone
   and the unbounded reading is the true maximum; if it's absent, no percentage is published rather
   than rebasing against a bound that isn't the max.

   Numerator is the tightest bound with a reading, and the cell names that bound: two gateways can
   both read "×1.0"/99% while one holds it at 1ms and the other only at 50ms - very different findings
   that a bare factor would conflate. */
function frontierHeld(frontier) {
  const f = Array.isArray(frontier) ? frontier : [];
  const tight = f.find((r) => r.bound_ms != null && mval(r.rps) > 0);
  const loose = frontierAt(f, null);
  const held = tight ? mval(tight.rps) : null, full = loose ? mval(loose.rps) : null;
  if (held == null || full == null || !(held > 0) || !(full > 0)) return null;
  return { frac: held / full, boundMs: tight.bound_ms, held, full, lowerBound: loose.lower_bound === true };
}
/* The fraction as a whole percent, never 100 unless the curve is exactly flat: rounding 99.6% up to
   100% would claim the gateway loses nothing to a tight tail when it does. Floors at 99 unless
   held === full exactly. Whole percent because field differences are tens of points, not tenths. */
function heldPct(frac) { return frac >= 1 ? 100 : Math.min(99, Math.round(frac * 100)); }
/* One ranking number: (bound-of-origin, then share of full rate), bigger = better, so tightest-tail
   holders sort to the top. Origin dominates and is inverted+scaled by 2 so each origin group owns a
   disjoint interval - a 99% share at 50ms must never outrank a lower share held at 1ms. */
function heldSortKey(originIndex, frac) { return (HELD_NOTHING_INDEX - originIndex) * 2 + frac; }
/* Origin index for a cell that held nothing under any published bound: one past the loosest bound, so
   heldSortKey puts it at the bottom. Must not be `v: null` (rowComparator sinks nulls regardless of
   sort direction), since holding nothing everywhere is the worst curve, not a missing measurement. */
const HELD_NOTHING_INDEX = FRONTIER_BOUNDS_MS.length;
/* Reference paragraph for the column, rendered below the table (see captionText): states which
   reading is the denominator and why the named bound matters, which a cell can't say in six words. */
const HELD_REFERENCE = `"N% of its full rate at B" in the last column is what the cell still carried at B, the TIGHTEST tail-latency bound it holds any rate at, as a share of its full rate with no latency bound at all. 99% at 1 ms is the good shape: the gateway gives up almost nothing even when you demand a 1 ms tail. A low percentage means it needs a loose tail to go fast. The bound matters as much as the percentage - 99% at 50 ms is not a flat gateway, it is a gateway that holds no rate at all under 50 ms - so the column sorts by that bound first and the percentage second, and a cell that held nothing under any published bound says so in words instead of showing 0%.`;
/* The curve, drawn as a sparkline rather than six numbers: the finding is a slope, which the eye
   reads in one pass and a row of digits does not.
   Y scale is shared across the board (opts.min/opts.max, from boardFrontierScale), not per-row -
   per-row auto-scaling would make every curve equally tall regardless of magnitude.
   Y is logarithmic so equal slopes are equal ratios (the finding), and so the field's wide spread
   (some gateways orders of magnitude apart) doesn't collapse the slower rows onto the baseline.
   A bound the gateway served but could not hold is drawn ON the floor, not skipped: that's a
   measurement (below_resolution), distinct from a bound never measured at all (a gap).
   X is the bound's index, not its value, since the bounds (1,5,10,50,100ms) are unevenly spaced and
   most movement happens in the first three. */
function frontierSpark(frontier, opts = {}) {
  const f = Array.isArray(frontier) ? frontier : [];
  /* One point per published reading position, so a missing bound leaves a gap rather than shifting
     the curve left. Three states kept apart: a rate -> point at its level; a measured 0 -> point on
     the floor (`onFloor`, held nothing under this tail); no reading -> nothing, a genuine gap. */
  const pts = BOUND_CHOICES.map((b, i) => {
    const rd = frontierAt(f, b);
    if (!rd) return null;
    const v = mval(rd.rps);
    if (v == null) return null;
    return { i, b, v, onFloor: !(v > 0), floor: rd.lower_bound === true, p99: rd.p99_us };
  }).filter(Boolean);
  if (!pts.length) return "";
  const W = 108, H = 26, PAD = 3;
  const rates = pts.filter((p) => p.v > 0).map((p) => p.v);
  // The shared domain, widened if this row happens to exceed it (it cannot, but a caller may pass none).
  const hi = Math.max(Number.isFinite(opts.max) ? opts.max : 0, ...rates, 1);
  const loSeed = Number.isFinite(opts.min) && opts.min > 0 ? opts.min : Math.min(...rates, hi);
  // Headroom below the field's floor so the slowest row's own points sit above the baseline - the
  // baseline is reserved for "held nothing under this tail", a different statement.
  const lo = Math.max(Math.min(loSeed, ...rates) / 2, 1);
  const l0 = Math.log10(lo), l1 = Math.log10(Math.max(hi, lo * 2));
  const x = (i) => PAD + (i / (BOUND_CHOICES.length - 1)) * (W - 2 * PAD);
  const y = (v) => (v > 0
    ? PAD + (1 - (Math.log10(Math.max(v, lo)) - l0) / (l1 - l0)) * (H - 2 * PAD - 3)
    : H - PAD);
  const path = pts.map((p, i) => `${i ? "L" : "M"}${x(p.i).toFixed(1)},${y(p.v).toFixed(1)}`).join("");
  // The selected bound is marked on the curve, so the ranked column and the shape visibly relate.
  const selIdx = BOUND_CHOICES.indexOf(opts.boundMs === undefined ? null : opts.boundMs);
  const rule = selIdx >= 0
    ? `<line x1="${x(selIdx).toFixed(1)}" y1="${PAD}" x2="${x(selIdx).toFixed(1)}" y2="${H - PAD}" ` +
      `stroke="currentColor" stroke-opacity="0.35" stroke-width="1" stroke-dasharray="2 2"/>`
    : "";
  /* Three markers, three claims: filled dot = established ceiling; open dot = a floor (sweep ran out
     of ladder while still qualifying); floor tick = served but held nothing under this tail. */
  const dots = pts.map((p) => {
    const cx = x(p.i).toFixed(1), cy = y(p.v).toFixed(1);
    const title = `<title>${esc(`${boundLabel(p.b)}: ${p.onFloor ? "no rung held this tail" : `${fmtRate(p.v)} req/s${p.floor ? " or more" : ""}`}`)}</title>`;
    if (p.onFloor)
      return `<line x1="${cx}" y1="${(H - PAD - 3).toFixed(1)}" x2="${cx}" y2="${H - PAD}" stroke="currentColor" stroke-width="1.2" stroke-opacity="0.55">${title}</line>`;
    return p.floor
      ? `<circle cx="${cx}" cy="${cy}" r="2.4" fill="none" stroke="currentColor" stroke-width="1.2">${title}</circle>`
      : `<circle cx="${cx}" cy="${cy}" r="1.9" fill="currentColor">${title}</circle>`;
  }).join("");
  const aria = pts.map((p) => `${boundLabel(p.b)}: ${p.onFloor ? "no rung held this tail" : `${fmtRate(p.v)}${p.floor ? " or more" : ""}`}`).join("; ");
  return `<svg class="frontier-spark" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" role="img" ` +
    `aria-label="throughput across the published tail-latency bounds, log scale - ${esc(aria)}">` +
    `<line x1="${PAD}" y1="${H - PAD}" x2="${W - PAD}" y2="${H - PAD}" stroke="currentColor" stroke-opacity="0.15" stroke-width="1"/>` +
    rule +
    `<path d="${path}" fill="none" stroke="currentColor" stroke-width="1.4"/>` + dots + `</svg>`;
}
/* Shared log domain for every sparkline on the board. Computed over the whole bundle, not filtered
   rows, so the scale doesn't move as a reader types in search. Zero rates excluded from the floor
   (drawn on the baseline instead). */
const FRONTIER_SCALE_CACHE = new WeakMap();
function boardFrontierScale(data = (typeof state !== "undefined" ? state.data : null)) {
  if (!data || !Array.isArray(data.gateways)) return { min: null, max: null };
  if (FRONTIER_SCALE_CACHE.has(data)) return FRONTIER_SCALE_CACHE.get(data);
  let max = 0, min = Infinity;
  const scan = (rec) => {
    for (const rd of frontierOf(rec)) {
      const v = mval(rd.rps);
      if (v == null || !(v > 0)) continue;
      if (v > max) max = v;
      if (v < min) min = v;
    }
  };
  for (const g of data.gateways) {
    scan(g.best_cell); scan(g.translation_cell);
    const ups = (g.matrix && g.matrix.upstreams) || {};
    for (const eg of Object.keys(ups)) {
      const cells = (ups[eg] && ups[eg].cells) || {};
      for (const ing of Object.keys(cells)) scan(cells[ing] && cells[ing].perf);
    }
  }
  const out = max > 0 ? { min, max } : { min: null, max: null };
  FRONTIER_SCALE_CACHE.set(data, out);
  return out;
}

/* ---- the engine's own metric definitions, on the surface that shows the number --
   `data.definitions` is a metric-key -> prose map generated from the engine's constants
   (engine/src/suite.rs `metric_definitions`). Surfaced inline (each table/drawer lane carries a fold)
   rather than filed elsewhere, since a definition a reader has to leave the page for is unread.
   Selected by key prefix, never an enumerated list, so a new engine definition appears with no
   change here. */
const DEFINITION_PREFIXES = { performance: ["perf."], frontier: ["perf.frontier"], streaming: ["stream."], memory: ["memory"] };
// Lane -> the prefixes whose definitions belong in that drawer/compare lane.
const LANE_DEFINITION_PREFIXES = { perf: ["perf."], xlate: ["perf."], stream: ["stream."], memory: ["memory"] };
// The reader-facing name for a definition key. An unknown key still renders, under its own key, because a
// definition the engine has published and this table has not learned about is still worth showing.
const DEFINITION_LABELS = {
  "perf.frontier": "Throughput at a tail-latency bound",
  "perf.added_latency": "Added latency",
  "stream.streams_sustained": "Streams sustained",
  "stream.added_ttft_and_gap": "Added time-to-first-token and inter-frame gap",
  memory: "Memory",
};
// definitionsFor(prefixes, data): [[key, prose]] for every published definition under those prefixes.
// An empty result is the normal shape for a bundle generated before the engine published definitions.
function definitionsFor(prefixes, data = (typeof state !== "undefined" ? state.data : null)) {
  const defs = data && data.definitions;
  if (!defs || typeof defs !== "object") return [];
  return Object.keys(defs).sort()
    .filter((k) => prefixes.some((p) => k === p || k.startsWith(p)))
    .filter((k) => typeof defs[k] === "string" && defs[k].trim())
    .map((k) => [k, defs[k]]);
}
// Collapsed "What these numbers mean" block. Rendered as prose exactly as the engine wrote it -
// rewording here would be a second source of truth. Returns "" when the bundle carries none.
function definitionsFold(prefixes, data = (typeof state !== "undefined" ? state.data : null)) {
  const entries = definitionsFor(prefixes, data);
  if (!entries.length) return "";
  return `<details class="metric-defs"><summary>What these numbers mean, from the engine's own constants</summary>` +
    entries.map(([k, prose]) =>
      `<div class="metric-def"><b>${esc(DEFINITION_LABELS[k] || k)}</b> <code class="muted">${esc(k)}</code>` +
      `<p>${esc(prose)}</p></div>`).join("") +
    `</details>`;
}

/* ---- provenance-driven captions (Design E §3.2) -----------------------------
   Every caption/label naming where a datum came from is rendered from the cell's `source.sweep`
   stamp through this one table - no caption string literal may hard-code a source token ("6x6",
   "matrix", "sweep", "suite"); check-consistency's C3 lint enforces this. Keyed by source.sweep;
   receives the cell's path. */
const SWEEP_CAPTION = {
  "6x6-diagonal":        (p) => `${laneDialect(p && p.dialect)} passthrough — 6×6 diagonal cell`,
  "6x6-translation":     (p) => `${laneDialect(p && p.ingress)} in → ${laneDialect(p && p.egress)} out — 6×6 translation cell`,
  // Legacy single memory window: one fixed-duration load on the gateway's throughput-peak cell.
  "6x6-memory-window":   ()  => `post-6×6 memory window (identical fixed load on the peak cell, fresh cold-restarted process)`,
  // Per-cell memory windows: one cold-started process per cell, load run until RSS is steady.
  "6x6-memory-diagonal": (p) => `${laneDialect(p && p.dialect)} passthrough - 6×6 memory window (cold start, load run to plateau)`,
  "6x6-memory-translation": (p) => `${laneDialect(p && p.ingress)} in → ${laneDialect(p && p.egress)} out - 6×6 memory window (cold start, load run to plateau)`,
  "6x6-stream-diagonal": (p) => `${laneDialect(p && p.dialect)} SSE stream — 6×6 diagonal cell`,
  "6x6-stream-translation": (p) => `${laneDialect(p && p.ingress)} in → ${laneDialect(p && p.egress)} out SSE stream — 6×6 translation cell`,
  "perf-suite":          (p) => `${laneDialect(p && p.dialect)} passthrough — perf suite (no 6×6 cell for this gateway yet)`,
  "xlate-suite":         (p) => `${laneDialect(p && p.ingress)} in → ${laneDialect(p && p.egress)} out — translation suite (no 6×6 cell for this gateway yet)`,
  "stream-suite":        (p) => `${laneDialect(p && p.dialect)} SSE stream — stream suite (legacy)`,
};
// Provenance label for a projected cell, from its own source.sweep stamp. Throws if the stamp is
// absent/unknown (C3 asserts every displayed cell's stamp is in the table).
function caption(cell) {
  const sweep = cell && cell.source && cell.source.sweep;
  const render = SWEEP_CAPTION[sweep];
  if (!render) throw new Error(`caption: no SWEEP_CAPTION for source.sweep=${JSON.stringify(sweep)}`);
  return render(cell.path || {});
}

/* The single passthrough perf record every surface reads (table, drawer, compare; charts.py reads
   the same best_cell). gen-data emits g.best_cell from the matrix sweep, or synthesizes it from the
   perf suite when no swept diagonal exists. Legacy bundles with no best_cell fall back to the raw
   perf suite object. */
// Thin wrappers returning the projected cell (or null) so all lane accessors + check-consistency
// read the one canonical record. No gate logic here: the gate is upstream, at seal time.
function canonicalPerf(g) { return g.best_cell || null; }
function canonicalXlate(g) { return g.translation_cell || null; }
function canonicalStreaming(g) { return g.streaming || null; }
/* The single memory record, projected solely from the matrix's post-6x6 memory window
   (g.memory_read, source:"matrix"): a fixed identical load on this gateway's peak cell, measured on
   a fresh cold-restarted process. No synthetic-suite fallback. */
function canonicalMemory(g) {
  const m = g.memory_read;
  if (m) return { served: true, ...m };
  return null;
}

// Pretty "ingress → egress" for a memory record's load_cell ("ingress>egress").
function memLoadCellLabel(lc) {
  if (typeof lc !== "string" || !lc.includes(">")) return lc || "?";
  const [ing, eg] = lc.split(">");
  const L = (d) => (MATRIX_LABELS[d] || d || "?");
  return `${L(ing)} → ${L(eg)}`;
}
/* AUDIT #14: memory windows are tunable in the harness (idle_window_s / recovery_window_s), so
   labels must render from the data rather than hardcode "60 s". memWindows(m) reads one record;
   boardMemWindows() reads the board's records for column headers, falling back to 60s default only
   when nothing states otherwise. */
const MEM_WINDOW_DEFAULT = 60;
function memWindows(m) {
  const idle = m && Number.isFinite(Number(m.idle_window_s)) ? Number(m.idle_window_s) : MEM_WINDOW_DEFAULT;
  const rec = m && Number.isFinite(Number(m.recovery_window_s)) ? Number(m.recovery_window_s) : MEM_WINDOW_DEFAULT;
  // Steadiness window (how long RSS held still before the plateau was believed) rides in the load
  // recipe. Null when the record predates it.
  const lr = m && m.load_recipe;
  const steady = lr && Number.isFinite(Number(lr.plateau_window_s)) ? Number(lr.plateau_window_s) : null;
  return { idle, recovery: rec, steady };
}
function boardMemWindows(data = (typeof state !== "undefined" ? state.data : null)) {
  // Per-cell first: reading only the legacy per-gateway record would silently fall back to the 60s
  // default on every per-cell bundle. Legacy bundles still answer through g.memory_read.
  const gws = (data && data.gateways) || [];
  for (const g of gws) {
    for (const c of memoryCells(g)) {
      if (c.mem.idle_window_s != null || c.mem.recovery_window_s != null) return memWindows(c.mem);
    }
  }
  const recs = gws.map((g) => g.memory_read).filter(Boolean);
  const rec = recs.find((m) => m.idle_window_s != null || m.recovery_window_s != null) || null;
  return memWindows(rec);
}
const memWindowLabel = (s) => `${fmtInt(s)} s`;
// Fixed-load basis + peak cell, for the "Tested on" cell tooltip.
function memLoadRecipeTip(m) {
  const r = m && m.load_recipe;
  const w = memWindows(m);
  const basis = r ? `identical fixed load: ${fmtInt(r.concurrency)} concurrent, ${fmtInt(r.payload_bytes)} B payload, ${fmtInt(r.duration_s)} s` : "identical fixed load for every gateway";
  return `peak cell ${memLoadCellLabel(m && m.load_cell)} — ${basis}, on a fresh cold-restarted process ` +
    `(${memWindowLabel(w.idle)} idle → load → ${memWindowLabel(w.recovery)} recovery)${memDisclosure(m)}`;
}
/* Honesty disclosures (uncertified basis, payload mismatch, failed load - each a reason a peak RSS
   came back null) ride inside memory.protocol as text after the leading recipe sentence; surface them
   wherever the memory record is attributed. */
function memDisclosure(m) {
  const p = m && typeof m.protocol === "string" ? m.protocol : "";
  const parts = p.split(";").slice(1).map((x) => x.trim()).filter(Boolean);
  return parts.length ? ` — DISCLOSED: ${parts.join("; ")}` : "";
}

/* Canonical memory record made self-describing for the one "Tested on" renderer (colTested): pins
   load_cell onto .path so there's no second source of truth. Keeps its own source stamp (audit #1:
   describe the record shown, never the perf chooser's cell). Null -> no pill. */
function memoryTestedRecord(g) {
  const m = canonicalMemory(g);
  const lc = m && m.load_cell;
  if (!m || typeof lc !== "string" || !lc.includes(">")) return null;
  const [ingress, egress] = lc.split(">");
  return { ...m, path: { ingress, egress, ...(ingress === egress ? { dialect: ingress } : {}) } };
}

/* ---- per-cell memory --------------------------------------------------------
   Memory is measured as a cold-started, plateau-terminated window on every served cell. Which cell to
   show is a display choice the reader makes (Min | Max | Same | Custom), not one the harness picks by
   throughput and hides.
   Everything below is null-safe: bundles predating per-cell measurement carry none of these fields. */
// The memory window a served cell carries, or null. Same lookup shape as xlateMatrixCell.
function perCellMemory(g, ingress, egress) {
  const up = g.matrix && g.matrix.upstreams && g.matrix.upstreams[egress];
  const cell = up && up.cells && up.cells[ingress];
  return (cell && cell.served === true && cell.memory && typeof cell.memory === "object") ? cell.memory : null;
}
// Every served cell this gateway has a memory window for, in stable (egress, ingress) order so
// Min/Max tie-breaks are deterministic rather than object-key-order dependent.
function memoryCells(g) {
  const out = [];
  for (const egress of MATRIX_CELLS) for (const ingress of MATRIX_CELLS) {
    const mem = perCellMemory(g, ingress, egress);
    if (mem) out.push({ ingress, egress, mem });
  }
  return out;
}
/* Does this bundle carry per-cell memory at all? Switch is per bundle, never per gateway: a gateway
   missing per-cell data in a per-cell bundle reads n/a rather than substituting its old peak-cell
   number behind a memory-selected label. Memoised - every row of every memory column asks this. */
const PER_CELL_MEM_CACHE = new WeakMap();
function hasPerCellMemory(data = (typeof state !== "undefined" ? state.data : null)) {
  if (!data || typeof data !== "object") return false;
  if (PER_CELL_MEM_CACHE.has(data)) return PER_CELL_MEM_CACHE.get(data);
  const yes = (data.gateways || []).some((g) => memoryCells(g).length > 0);
  PER_CELL_MEM_CACHE.set(data, yes);
  return yes;
}
/* Does any gateway on this board carry a measured cost-per-request? Cost columns appear only when the
   board can answer them, rather than showing n/a on every row for a board measured before the
   capture existed - a column nothing on the page can answer is noise, not disclosure. */
const COST_CACHE = new WeakMap();
/* Concurrency every gateway's CPU-per-request was measured at. Peak req/s and CPU-per-request come
   from different, fixed concurrencies (the frontier's choice vs one held identical for every
   gateway) - multiplying them together would produce an impossible number (e.g. cores > box has).
   Read from the data, not hardcoded, since the engine's window concurrency can change. */
function costWindowConc(data = (typeof state !== "undefined" ? state.data : null)) {
  for (const g of (data && data.gateways) || []) {
    for (const cell of [g.best_cell, g.translation_cell]) {
      const v = mval(cell && cell.cost_window_conc);
      if (Number.isFinite(v)) return v;
    }
  }
  return null;
}
function hasCost(data = (typeof state !== "undefined" ? state.data : null)) {
  if (!data || typeof data !== "object") return false;
  if (COST_CACHE.has(data)) return COST_CACHE.get(data);
  const yes = (data.gateways || []).some((g) => {
    const cells = [g.best_cell, g.translation_cell].filter(Boolean);
    // Through mval(), not a raw .value read - C5 requires this since a raw read bypasses the
    // below_resolution rule (null value but "measured, too small to weigh" is still a measurement).
    return cells.some((c) => mval(c.cpu_us_per_request) != null);
  });
  COST_CACHE.set(data, yes);
  return yes;
}
/* Smallest difference this rig can actually resolve, derived from the board rather than chosen.
   Every box runs the same qualification, and `box_qualify.drift_pct` is how far it landed from the
   shared baseline; the spread between luckiest and unluckiest box is what the rig can't tell apart.
   Derived, not hardcoded, so the figure tracks the actual fleet. Null with fewer than two boxes
   reporting drift (no spread to observe). */
const RESOLUTION_CACHE = new WeakMap();
function rigResolutionPct(data = (typeof state !== "undefined" ? state.data : null)) {
  if (!data || typeof data !== "object") return null;
  if (RESOLUTION_CACHE.has(data)) return RESOLUTION_CACHE.get(data);
  const drifts = (data.gateways || [])
    .map((g) => g && g.rig && g.rig.box_qualify && g.rig.box_qualify.drift_pct)
    .filter((v) => typeof v === "number" && Number.isFinite(v));
  const out = drifts.length >= 2 ? Math.max(...drifts) - Math.min(...drifts) : null;
  RESOLUTION_CACHE.set(data, out);
  return out;
}

/* ---- the Charts tab -----------------------------------------------------------------------------
   Replaces 25 static PNGs from a python script (nine metrics x2 views + three frontier views), which
   were a second surface republishing the same numbers and could silently drift out of sync with the
   data. A tab that re-draws at read time (bound, cell, metric all live) avoids that.
   One registry since the metrics are structurally identical: number per gateway, direction, formatter. */
const CHART_METRICS = [
  { id: "cpu", label: "CPU per request", unit: "µs", log: true, desc: false,
    note: "Microseconds of gateway CPU per completed request, measured at one concurrency held identical for every gateway. Lower is better.",
    get: (g, st) => mval((chooserCellPerf(g, st) || {}).cpu_us_per_request) },
  { id: "rpsdollar", label: "Requests per $/hr", unit: "req/s per $/hr", log: true, desc: true,
    note: "Requests per second per dollar of hourly instance cost, at the selected bound. Higher is better.",
    get: (g, st) => mval((chooserCellPerf(g, st) || {}).rps_per_dollar) },
  { id: "permillion", label: "Cost per million requests", unit: "USD", log: true, desc: false,
    note: "Instance cost to serve a million requests at the selected bound. Lower is better.",
    // Cost is $/hr ÷ throughput, so below_resolution (no weighable rate) makes cost undefined, not
    // zero - mval's below_resolution->0 coercion would render $0.0000, falsely the cheapest gateway.
    get: (g, st) => { const e = (chooserCellPerf(g, st) || {}).cost_per_million_usd;
      return (isEnvelope(e) && e.reason === "below_resolution") ? null : mval(e); } },
  { id: "lat", label: "Added latency (p99)", unit: "µs", log: true, desc: false,
    note: "Gateway p99 minus direct-to-mock p99 at concurrency 1. Lower is better.",
    get: (g, st) => mval((chooserCellPerf(g, st) || {}).added_latency_p99_us) },
  { id: "rps", label: "Throughput at the selected bound", unit: "req/s", log: false, desc: true,
    note: "The most requests/sec the chosen cell carried while 99% of requests finished under the selected bound. Higher is better.",
    get: (g, st) => { const r = frontierAt(frontierOf(chooserCellPerf(g, st)), selectedBound(st)); return r ? mval(r.rps) : null; } },
  { id: "rss", label: "Peak memory", unit: "MiB", log: false, desc: false,
    note: "Highest resident memory observed while the fixed load ran on the chosen cell. Lower is better.",
    get: (g, st) => mval((memoryFor(g, st) || {}).peak_rss_mib) },
];

/* Log scale isn't a preference here, it's the only readable axis for metrics with huge spreads (e.g.
   cost per request can span 2000x+, collapsing a linear axis to one pixel). Bounded-spread metrics
   (throughput, memory) stay linear since log would flatten real, readable differences. */
function chartRows(metric, gateways, st) {
  const rows = [];
  for (const g of gateways || []) {
    const v = metric.get(g, st);
    if (typeof v === "number" && Number.isFinite(v)) rows.push({ key: g.key, name: g.name || g.key, v, g });
  }
  rows.sort((a, b) => (metric.desc ? b.v - a.v : a.v - b.v));
  return rows;
}

/* Whether a cell's core utilisation may be read as a saturation verdict. The cost window runs at one
   concurrency, identical for every gateway; a gateway whose peak needs far more concurrency is barely
   loaded there, so its idle cores say nothing about saturation at peak. Returns the utilisation with
   the ratio that qualifies it; `verdict` is null when the window didn't get close enough to peak. */
const SATURATION_NEEDS_FRAC_OF_PEAK = 0.75;
function costSaturation(cell) {
  const util = mval((cell || {}).cost_core_utilisation);
  const wrps = mval((cell || {}).cost_window_rps);
  const peak = peakFrontierRps(cell);
  if (util == null || wrps == null || !peak) return null;
  const frac = wrps / peak;
  if (frac < SATURATION_NEEDS_FRAC_OF_PEAK) {
    return { util, frac, verdict: null,
      why: `measured at ${Math.round(frac * 100)}% of this cell's peak rate, too far below it to say whether the gateway saturates at its peak` };
  }
  return { util, frac,
    verdict: util >= 0.85 ? "cpu-bound" : "headroom",
    why: util >= 0.85
      ? `at ${Math.round(frac * 100)}% of its peak rate it was using ${(util * 100).toFixed(0)}% of the cores it was given - this peak is its own CPU wall`
      : `at ${Math.round(frac * 100)}% of its peak rate it was using only ${(util * 100).toFixed(0)}% of the cores it was given - something other than CPU is holding it` };
}

/* The highest rate any frontier reading on this cell carried, for the ratio above. */
function peakFrontierRps(cell) {
  const fr = (cell || {}).frontier;
  if (!Array.isArray(fr)) return null;
  let best = null;
  for (const r of fr) {
    const v = mval(r && r.rps);
    if (v != null && (best == null || v > best)) best = v;
  }
  return best;
}

/* Keys of rows the sorted column cannot separate from the row directly above them: when two adjacent
   values are closer than the rig's own resolution, their order records which box they landed on, not
   a finding. Only the sorted column is checked; nothing is marked when resolution is unknown. */
function tiedRuns(rows, col, st, pct) {
  const out = new Set();
  if (pct == null || !col || col.render || typeof col.get !== "function") return out;
  for (let i = 1; i < rows.length; i++) {
    const a = col.get(rows[i - 1], st), b = col.get(rows[i], st);
    if (a && b && indistinguishable(a.v, b.v, pct)) out.add(rows[i].key);
  }
  return out;
}

// Are two published values closer than the rig can resolve? Relative to the larger value so the
// comparison means the same thing at 19 rps and at 49,000.
function indistinguishable(a, b, pct) {
  if (typeof pct !== "number" || !(pct > 0)) return false;
  if (typeof a !== "number" || typeof b !== "number") return false;
  const hi = Math.max(Math.abs(a), Math.abs(b));
  if (hi === 0) return true;                       // two measured zeros are the same measurement
  return (Math.abs(a - b) / hi) * 100 < pct;
}

// The data bundle a (possibly synthetic) state refers to; falls back to the live state's.
function stateData(st) {
  return (st && st.data) || (typeof state !== "undefined" ? state.data : null);
}
/* Identity cell the most gateways serve: default for memory's Same mode. Derived from the data, never
   hardcoded, so no gateway/protocol is special-cased. Ties break alphabetically. Null if nothing served. */
function widestDialect(data = (typeof state !== "undefined" ? state.data : null)) {
  const gws = (data && data.gateways) || [];
  if (!gws.length) return null;
  let best = null, bestN = 0;
  for (const d of MATRIX_CELLS) {
    const n = gws.filter((g) => servesXlatePair(g, d, d)).length;
    if (n > bestN || (n === bestN && n > 0 && best && d.localeCompare(best) < 0)) { best = d; bestN = n; }
  }
  return best;
}
/* Steady-state RSS of one cell's window, or null when it never plateaued. This is the value Min/Max
   select on, matching what the column reports. A never-plateaued cell isn't a min/max candidate. */
function memSteady(mem) { return mval(mem && mem.steady_state_rss_mib); }
/* Per-cell memory record the memory lane shows, stamped through the same choke point (stampChosen)
   every other lane uses:
     min / max  -> this gateway's lowest / highest steady-state RSS across cells it serves
     same       -> the chosen dialect's identity cell
     custom     -> the chosen ingress->egress cell
   Never peak. Returns null rather than a substituted cell when nothing qualifies. */
function chosenMemory(g, st = state) {
  const cells = memoryCells(g);
  if (!cells.length) return null;
  const mode = memoryMode(st);
  let pick = null;
  if (mode === "same" || mode === "custom") {
    const [ingress, egress] = mode === "same"
      ? [st.sameDialect, st.sameDialect] : [st.xlateIn, st.xlateOut];
    pick = cells.find((c) => c.ingress === ingress && c.egress === egress) || null;
  } else {
    const scored = cells.filter((c) => memSteady(c.mem) != null);
    for (const c of scored) {
      if (!pick) { pick = c; continue; }
      const better = mode === "min" ? memSteady(c.mem) < memSteady(pick.mem) : memSteady(c.mem) > memSteady(pick.mem);
      if (better) pick = c;
    }
  }
  if (!pick) return null;
  // Candidate count travels on the record so min-of-26 vs min-of-1 (different-sized searches) is disclosed.
  return { served: true, ...stampChosen(pick.mem, g, pick.ingress, pick.egress, "memory-"),
    mem_candidates: cells.filter((c) => memSteady(c.mem) != null).length, mem_cells: cells.length };
}
// The memory record every memory column reads. Per-cell bundle -> chosen cell; legacy bundle ->
// single post-6x6 window unchanged.
function memoryFor(g, st = state) {
  return hasPerCellMemory(stateData(st)) ? chosenMemory(g, st) : canonicalMemory(g);
}
// Idle is sampled cold, before the first request, so it stays outside the chooser - valid in every
// mode. With one sample per cell, publish the median plus spread rather than picking one cell.
function idleAcrossCells(g) {
  const vals = memoryCells(g).map((c) => mval(c.mem.idle_rss_mib)).filter((v) => v != null).sort((a, b) => a - b);
  if (!vals.length) return null;
  const mid = Math.floor(vals.length / 2);
  return { median: vals.length % 2 ? vals[mid] : (vals[mid - 1] + vals[mid]) / 2,
    min: vals[0], max: vals[vals.length - 1], n: vals.length };
}
// This gateway's RSS never went steady on any cell it serves - flagged at gateway level rather than
// per-cell. False when there's no per-cell data to judge.
function neverPlateaued(g) {
  // Tri-state: `plateaued: null` is a withheld verdict (harness never got a clean measurement), not
  // a negative one. `null !== true` would turn every withheld verdict into "never settles" - on
  // macOS, where RSS is unreadable, that would flag every gateway. Only judged cells may vote.
  const judged = memoryCells(g).filter((c) => c.mem.plateaued != null);
  return judged.length > 0 && judged.every((c) => c.mem.plateaued === false);
}
/* How a window failed to settle - "climbing", "swinging", "releasing" - or "" when settled/unpublished.
   "Never settles" conflates unbounded climb with a GC-driven swing around a level; the engine
   separates them and this only reads its verdict, never re-derives one. */
/* Whether to show a verdict word beside the growth rate. Off: the rate's sign already says
   everything, and a printed verdict is a way to be wrong about someone else's product (this
   happened - cells labelled "(leak)" while memory was falling). The shape is still measured and
   spelled out in the drawer/tooltip; this only gates the table cell's word. */
const SHOW_GROWTH_VERDICT = false;
function memShape(rec) {
  // mcode, not mval: 0 is a real shape code here, so an absence must not decay into "it swung".
  const c = mcode(rec && (rec.shape ?? rec.memory_shape));
  return c === 1 ? "climbing" : c === 0 ? "swinging" : c === -1 ? "releasing" : "";
}
// This gateway is climbing on at least one cell (the red-pill distinction: only accused when
// something is actually growing, not merely oscillating).
function memGrowing(g) {
  return memoryCells(g).some((c) => memShape(c.mem) === "climbing");
}
// At least one unsettled cell told us its shape, so shape-aware wording is available (distinguishes
// an all-oscillating gateway from one on a board too old to carry shapes).
function memShaped(g) {
  return memoryCells(g).some((c) => c.mem.plateaued === false && memShape(c.mem) !== "");
}
// How many of this gateway's measured cells had their verdict withheld (affects pill wording).
function memoryUnjudged(g) { return memoryCells(g).filter((c) => c.mem.plateaued == null).length; }
// Highest growth rate across this gateway's cells, so the gateway-level flag can quantify itself.
function worstGrowth(g) {
  const vals = memoryCells(g).map((c) => mval(c.mem.growth_rate_mib_per_min)).filter((v) => v != null);
  return vals.length ? Math.max(...vals) : null;
}
// "Tested on" tooltip for a per-cell record: did it settle, how long, and what it was doing if not.
// Legacy record's tooltip (memLoadRecipeTip) stays separate for legacy rows.
function memCellTip(rec) {
  const bits = [];
  const r = rec && rec.load_recipe;
  bits.push(r ? `identical fixed load: ${fmtInt(r.concurrency)} concurrent, ${fmtInt(r.payload_bytes)} B payload, run until RSS is steady`
    : "identical fixed load for every gateway, run until RSS is steady");
  bits.push("cold-started for this cell (idle sampled before the first request)");
  if (rec && rec.plateaued === true) {
    // time_to_plateau_s is when RSS went flat, not when steadiness was confirmed. 0 is a real answer
    // (already steady when load began), so say that rather than "settled after 0 s".
    const t = Number(mval(rec.time_to_plateau_s));
    const w = memWindows(rec).steady;
    const conf = w ? ` (steady for the ${memWindowLabel(w)} that followed)` : "";
    bits.push(!Number.isFinite(t) ? "settled"
      : t <= 0 ? `steady from the moment the load started${conf}`
      : `settled after ${fmtInt(t)} s${conf}`);
  } else if (rec && rec.plateaued === false) {
    const gr = mval(rec.growth_rate_mib_per_min);
    const sh = memShape(rec);
    // Same number, different meaning: under a climb it's a leak rate, under a swing it's just the
    // sampling instant's velocity, not called a leak. No verdict wording - the rate and sign are the
    // finding; every reading still prints.
    const what = sh === "swinging"
      ? "no steady state, and did not grow: RSS swung around a level it kept returning to"
      : sh === "releasing"
      ? "no steady state: still RELEASING memory when the cap was reached, not growing"
      : "no steady state: RSS was still rising when the cap was reached";
    bits.push(gr != null && sh !== "swinging" && sh !== "releasing"
      ? `no steady state: ${fmt1(gr)} MiB/min under load when the cap was reached`
      : gr != null && sh === "swinging"
      ? `${what} (moving ${fmt1(gr)} MiB/min at the close, which is the swing, not a leak)`
      : what);
  }
  // Mode-neutral: in Min/Max it's the search size, in Same/Custom it's still useful context.
  if (rec && rec.mem_candidates != null)
    bits.push(`${fmtInt(rec.mem_candidates)} of this gateway's ${fmtInt(rec.mem_cells)} measured cells reached a steady state`);
  return `${bits.join("; ")}${memDisclosure(rec)}`;
}

/* Passthrough tab reads only the canonical record (g.best_cell). When it exists it is THE record: a
   missing field reads n/a, never patched from another source. Only a legacy bundle with no best_cell
   falls back to its perf suite. */
function passCell(g, key, fmt) {
  // Sealed envelope through metric(); no gate here, a suppressed value is already {value:null,...}.
  return g.best_cell ? metric(g.best_cell[key], fmt) : { v: null, text: "n/a", na: true };
}

// Streaming tab reads only g.streaming. A gateway that did not stream reads n/a.
function streamCell(g, key, fmt) {
  const s = canonicalStreaming(g);
  return s ? metric(s[key], fmt) : { v: null, text: "n/a", na: true };
}
/* Memory columns read the record memoryFor() chose (per-cell window, or legacy single post-6x6
   window). No record for the chosen cell -> n/a; never substituted from another cell. */
function memCell(g, key, fmt, st = state) {
  const m = memoryFor(g, st);
  return m ? metric(m[key], fmt) : { v: null, text: "n/a", na: true };
}

/* Perf object for a gateway's ingress->egress translation cell, straight from the matrix
   (upstreams[egress].cells[ingress]). Returns cell.perf when served+measured, else null. The
   Translation tab pins both ends so every row is the identical translation. */
function xlateMatrixCell(g, ingress, egress) {
  const up = g.matrix && g.matrix.upstreams && g.matrix.upstreams[egress];
  const cell = up && up.cells && up.cells[ingress];
  return (cell && cell.served === true && cell.perf) ? cell.perf : null;
}
/* Does the gateway serve the pinned translation pair at all (green cell), measured or not? Drives the
   Translation tab's row set: only gateways that serve this exact ingress->egress path appear. */
function servesXlatePair(g, ingress, egress) {
  const up = g.matrix && g.matrix.upstreams && g.matrix.upstreams[egress];
  const cell = up && up.cells && up.cells[ingress];
  return !!(cell && cell.served === true);
}
/* Column reader for the Translation tab: the pinned-pair cell's metric, n/a when the pair is served
   but unmeasured (perf sweep did not land). */
function xlateCell(g, key, fmt) {
  const perf = xlateMatrixCell(g, state.xlateIn, state.xlateOut);
  // The cell's metric is a sealed envelope; metric() reads it (n/a when suppressed/absent). No gate here.
  return perf ? metric(perf[key], fmt) : { v: null, text: "n/a", na: true };
}

/* ---- unified cell chooser (Performance + Streaming) --------------------------
   The board runs the 6x6 matrix once; Performance and Streaming are picks of that one run. Chooser
   state (st.mode + sameDialect/xlateIn/xlateOut) selects which cell each row reads:
     peak   -> the gateway's own best diagonal (best_cell); streaming = projected diagonal g.streaming
     same D -> the D->D diagonal cell for every gateway
     custom -> the xlateIn->xlateOut cell (any pair, incl. translation)
   Every mode reads the same per-cell records under the same honesty rules; a cell a gateway lacks
   reads n/a - never 0, never fabricated. */
// Perf object (sealed envelopes) for the chosen cell of gateway g, or null if unserved/unmeasured.
// Mode = selection only, never gating. Peak reads best_cell; Same/Custom read the matrix cell's
// sealed .perf (caller stamps dialects on).
function chooserCellPerf(g, st = state) {
  if (st.mode === "peak") return g.best_cell || null;
  const [ingress, egress] = chooserDialects(g, st);
  if (ingress == null) return null;
  const perf = xlateMatrixCell(g, ingress, egress);
  return perf ? stampChosen(perf, g, ingress, egress, "") : null;
}
/* Choke point that makes every chosen record self-describing. A raw matrix cell's sealed .perf/.stream
   carries no path/source, so this stamps it once and every surface renders provenance via caption(). */
// `lane` is the sweep-key infix: "" (perf), "stream-", "memory-".
function stampChosen(rec, g, ingress, egress, lane = "") {
  const same = ingress === egress;
  const path = { ingress, egress, ...(same ? { dialect: ingress } : {}) };
  // Composed from the cell's own shape, never a caption literal - SWEEP_CAPTION stays the single home
  // of the key vocabulary (C3); caption() throws if this names a key the table doesn't render.
  const sweep = `6x6-${lane}${same ? "diagonal" : "translation"}`;
  return { path, source: { kind: "matrix", sweep,
    build: (g.matrix && g.matrix.build) || null,
    measured_at: (g.matrix && g.matrix.measured_at) || null }, ...rec };
}
// The (ingress, egress) dialects the chosen cell is measured on, for the pill/labels + popup.
function chooserDialects(g, st = state) {
  if (st.mode === "peak") { const d = g.best_cell ? g.best_cell.path.dialect : null; return d ? [d, d] : [null, null]; }
  // Memory's Min/Max name a cell only through the memory chooser's own pick, not the stale
  // xlateIn/xlateOut pair the Custom arm below would otherwise silently fall back to.
  if (st.mode === "min" || st.mode === "max") {
    const m = chosenMemory(g, st);
    const p = m && m.path;
    return p ? [p.ingress ?? p.dialect ?? null, p.egress ?? p.dialect ?? null] : [null, null];
  }
  return st.mode === "same" ? [st.sameDialect, st.sameDialect] : [st.xlateIn, st.xlateOut];
}
// Perf-metric cell for the chosen cell, via metric(). No gate here.
function chooserPerfCell(g, key, fmt, st = state) {
  const p = chooserCellPerf(g, st);
  return p ? metric(p[key], fmt) : { v: null, text: "n/a", na: true };
}
// The chosen cell's streaming record. Per-cell streaming is only measured on the diagonal today:
//   peak   -> g.streaming (best diagonal's streaming)
//   same D -> g.streaming only when its diagonal IS D, else n/a
//   custom -> the cell's own sealed .stream if the matrix carries one, else n/a
function chooserCellStream(g, st = state) {
  if (st.mode === "peak") return canonicalStreaming(g);
  const [ingress, egress] = chooserDialects(g, st);
  /* Same reads the cell, not the projected headline: `canonicalStreaming(g)` is one record (the
     diagonal the headline was projected from), so asking for any other diagonal used to return null
     even when the matrix had a fully measured cell for it. Same and Custom differ only in that Same
     names one dialect for both ends, so both read the matrix the same way. */
  if (st.mode === "same") {
    const upSame = g.matrix && g.matrix.upstreams && g.matrix.upstreams[ingress];
    const cellSame = upSame && upSame.cells && upSame.cells[ingress];
    const rawSame = cellSame && cellSame.served === true && cellSame.stream
      && cellSame.stream.stream_served === true ? cellSame.stream : null;
    return rawSame ? stampChosen({ stream_served: true, ...rawSame }, g, ingress, ingress, "stream-") : null;
  }
  // custom: a per-cell stream record if the matrix carries one for this exact pair (else n/a). Already
  // sealed in-place by gen-data, so no re-gating needed.
  const up = g.matrix && g.matrix.upstreams && g.matrix.upstreams[egress];
  const cell = up && up.cells && up.cells[ingress];
  const raw = cell && cell.served === true && cell.stream && cell.stream.stream_served === true ? cell.stream : null;
  // Stamped through the one choke point, so a per-cell translation stream is captioned as such,
  // never relabelled a single-dialect passthrough (audit #1/#6).
  return raw ? stampChosen({ stream_served: true, ...raw }, g, ingress, egress, "stream-") : null;
}
// A streaming-metric cell for the chosen cell (n/a when the cell has no streaming here or lacks the field).
// Peak delegates to streamCell; Same/Custom read the per-cell stream record's sealed envelope via metric().
function chooserStreamCell(g, key, fmt, st = state) {
  if (st.mode === "peak") return streamCell(g, key, fmt);
  const s = chooserCellStream(g, st);
  return s ? metric(s[key], fmt) : { v: null, text: "n/a", na: true };
}
// Does gateway g have a chosen cell to show at all (a served, measured cell)? Drives whether a row
// appears / how the pill renders. Peak: any best_cell. Same/Custom: the exact cell is served.
function chooserHasCell(g, st = state) {
  if (st.mode === "peak") return !!chooserCellPerf(g, st);
  const [ingress, egress] = chooserDialects(g, st);
  return servesXlatePair(g, ingress, egress);
}
// The {ingress, egress} a perf record was measured on. best_cell carries it under .path; a raw matrix
// .perf carries none (caller pins ingress/egress onto the record before comparing).
function cellPath(rec) {
  if (!rec) return {};
  return rec.path || { ingress: rec.ingress, egress: rec.egress };
}
/* Delta for a chosen cell vs the gateway's own representative diagonal (best_cell): "+18% latency,
   -9% req/s". `deltaToPeak` is a legacy name - best_cell is not a peak (it ranks on added latency,
   never throughput), so this delta can legitimately come out positive on req/s.
   Returns "" for the reference cell itself or when either number is missing.
   Throughput is compared at one named bound (caller states which) - comparing readings at different
   bounds would be a percentage between two different questions. */
function deltaToPeak(cellPerf, best, boundMs = selectedBound()) {
  if (!cellPerf || !best) return "";
  const cp = cellPath(cellPerf), bp = cellPath(best);
  if (bp.ingress === cp.ingress && bp.egress === cp.egress) return "";   // same cell as the reference
  const bits = [];
  const cLat = mval(cellPerf.added_latency_p99_us), bLat = mval(best.added_latency_p99_us);
  if (cLat != null && bLat != null && bLat > 0)
    bits.push(`${fmtPct((cLat / bLat - 1) * 100)} latency`);
  const cRd = frontierAt(frontierOf(cellPerf), boundMs), bRd = frontierAt(frontierOf(best), boundMs);
  const cRps = cRd ? mval(cRd.rps) : null, bRps = bRd ? mval(bRd.rps) : null;
  if (cRps != null && bRps != null && bRps > 0)
    bits.push(`${fmtPct((cRps / bRps - 1) * 100)} req/s at ${boundLabel(boundMs)}`);
  return bits.join(", ");
}

/* Gateway-level plateau verdict, next to the name on the memory tab. Quantified where possible (worst
   growth rate) so it's a measurement, not an accusation. Empty when settled or no per-cell data. */
function neverPlateauedPill(g) {
  if (!neverPlateaued(g)) return "";
  const gr = worstGrowth(g);
  const rate = gr != null ? `, still growing at up to ${fmt1(gr)} MiB/min` : "";
  // Say what was actually judged (narrower claim if some cells weren't measured).
  const un = memoryUnjudged(g);
  const scope = un > 0
    ? `on any cell we could measure it on (${fmtInt(un)} further cell${un === 1 ? "" : "s"} were not measured)`
    : "on any cell this gateway serves";
  // Only accused when something is climbing; oscillating-only gets neutral styling (a real finding,
  // not a leak).
  const growing = memGrowing(g);
  const cleared = !growing && memShaped(g);
  const cls = growing || !memShaped(g) ? "noplateau-pill" : "noplateau-pill neutral";
  // States the measurement (no steady-state level to publish), not a verdict on the gateway.
  const label = cleared ? "no steady state (no growth)" : "no steady state";
  const why = cleared
    ? `RSS never went steady ${scope}, but it never grew either: it swung around a level it kept returning to, which is memory being reclaimed rather than leaked. No steady-state number is published for it because there is no single level to publish, not because it is climbing.`
    : `RSS never went steady ${scope}${rate}. Its memory under load is bounded by how long we ran the load, not by the gateway, so no steady-state number is published for it.`;
  return ` <span class="${cls}" title="${esc(why)}">${label}</span>`;
}

/* ---- column model ----------------------------------------------------------- */
/* get(g) returns {v, text, na}: v is the sortable value (null = none), text the cell text, na marks a
   muted "not measured / not served" cell. sortable:false columns take no part in sorting. Columns are
   grouped into per-tab sets (COLUMN_SETS); shared leading columns (select/name) are reused across all.
   Implementation language is not a perf column - it lives on the Gateways overview roster instead. */
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
  render: (g, st = state) => {
    const a = gwLink(g);
    // No per-row date: every gateway shares the board-wide "last benchmarked" timestamp.
    // No pill beside the name: a red plateau-verdict tag here would read as a verdict on the whole
    // gateway rather than one measurement window; the Growth column and per-cell tooltip carry that
    // finding instead.
    return `<td class="name">${a}</td>`;
  },
};
// "Tested on" column, present in every mode. Reads the chosen cell's path so it always names the
// exact cell the row's numbers were measured on. Provenance disclosure renders from the chosen cell's
// source stamp via caption(), never a hard-coded string.
// colTested(lane) binds to its own lane's record so a Streaming row can never advertise the Perf
// cell's provenance, and paints no pill without a record.
// Memory joins the same choke point: per-cell bundle -> chosen cell's window (already stamped);
// legacy bundle -> single post-6x6 window with load_cell as the path.
const LANE_RECORD = {
  perf: (g, st) => chooserCellPerf(g, st),
  stream: (g, st) => chooserCellStream(g, st),
  memory: (g, st = state) => (hasPerCellMemory(stateData(st)) ? chosenMemory(g, st) : memoryTestedRecord(g)),
};
// The header tooltip per lane (what the column means on THIS tab).
const LANE_TESTED_TITLE = {
  memory: () => (hasPerCellMemory()
    ? "The cell this row's memory numbers were measured on. Min/Max: this gateway's own lowest/highest steady-state cell, with the size of the search beside it. Same/Custom: the chosen cell, identical on every row."
    : "The peak cell this gateway's memory window actually ran on (its highest-throughput served cell). The fixed load recipe is identical for every gateway; only the cell differs."),
};
// A lane may append its own extra disclosure after the record's caption on the pill tooltip. Memory
// carries the load basis, window outcome, and honesty disclosures (memory.protocol).
const LANE_TESTED_NOTE = { memory: (rec) => (rec && rec.load_cell ? memLoadRecipeTip(rec) : memCellTip(rec)) };
// A lane may also append a plain-text suffix after the pill: Min/Max need the size of the candidate
// set visible without opening anything (min-of-26 vs min-of-1 are different searches).
const LANE_TESTED_SUFFIX = {
  memory: (rec, st = state) => {
    const mode = memoryMode(st);
    if ((mode !== "min" && mode !== "max") || !rec || rec.mem_candidates == null) return "";
    return `of ${fmtInt(rec.mem_candidates)} served`;
  },
};
// Lanes that take no part in sorting (memory's cell is attribution, not ranking).
const LANE_TESTED_NOSORT = new Set(["memory"]);
/* Does this lane record put at least one number (or below-resolution ≈0) on the row? All-or-nothing
   contract: either the cell is measured and all data is reported, or it's untested and empty. A
   record whose every envelope is empty must not advertise a measurement it doesn't have. */
// Envelope keys no surface displays as a column or drawer metric - excluded so a record whose only
// value is one of these (invisible to the reader) doesn't falsely pass the all-or-nothing test.
const UNDISPLAYED_ENVELOPE_KEYS = new Set([
  "time_to_plateau_s", "direct_c1_p99_us", "gateway_c1_p99_us",
  "gateway_c1_samples", "direct_c1_samples", "peak_rss_hwm_mib",
]);
function recordShowsValues(rec) {
  if (!rec || typeof rec !== "object") return false;
  // A frontier reading is a displayed value but not an envelope on the record (it's an array of
  // readings). Without this clause a cell with published throughput but no added-latency would fail
  // the all-or-nothing test and wrongly show n/a.
  if (frontierOf(rec).some((rd) => !metric(rd.rps).na)) return true;
  return Object.entries(rec).some(([k, v]) =>
    !UNDISPLAYED_ENVELOPE_KEYS.has(k) && isEnvelope(v) && !metric(v).na);
}
function colTested(lane) {
  const pick = LANE_RECORD[lane];
  const note = LANE_TESTED_NOTE[lane];
  const suffix = LANE_TESTED_SUFFIX[lane];
  return {
    id: "tested", label: "Tested on", desc: false,
    ...(LANE_TESTED_NOSORT.has(lane) ? { sortable: false } : {}),
    title: LANE_TESTED_TITLE[lane] ||
      `The cell these ${lane === "stream" ? "streaming " : ""}numbers were measured on, with the provenance of the record actually shown. Peak: each gateway's own peak cell. Same: the chosen dialect. Custom: the chosen ingress→egress cell.`,
    get: (g, st = state) => {
      const rec = pick(g, st);
      const shown = rec && recordShowsValues(rec);
      const p = shown ? cellPath(rec) : null;
      const ing = p && (p.ingress ?? p.dialect);
      return { v: shown && ing ? ing : "", text: null, na: !(shown && ing) };
    },
    render: (g, st = state) => {
      const rec = pick(g, st);
      // No record -> no pill; a record with no displayable value is the same emptiness in disguise.
      if (!rec || !recordShowsValues(rec)) return `<td class="tested"><span class="muted">n/a</span></td>`;
      const p = cellPath(rec);
      const ing = p.ingress ?? p.dialect, eg = p.egress ?? p.dialect;
      if (ing == null) return `<td class="tested"><span class="muted">n/a</span></td>`;
      // Passthrough (in==out) shows the single dialect; a translation cell shows in→out.
      const label = ing === eg ? (MATRIX_LABELS[ing] || ing) : `${MATRIX_LABELS[ing] || ing}→${MATRIX_LABELS[eg] || eg}`;
      // Provenance from this record's own stamp. A live-fallback record (legacy suite, not matrix) is
      // starred so the disclosure is visible without hovering.
      const fb = !!(rec.source && rec.source.kind !== "matrix");
      const base = rec.source ? caption(rec) : `measured on the ${ing}-in / ${eg}-out cell`;
      const title = note ? `${base} — ${note(rec)}` : base;
      const suf = suffix ? suffix(rec, st) : "";
      return `<td class="tested"><span class="tested-pill" title="${esc(title)}">${esc(label)}${fb ? " *" : ""}</span>${
        suf ? `<span class="tested-of muted" title="The size of the set this extremum was selected from. A minimum over 26 cells and a minimum over 1 are not the same search.">${esc(suf)}</span>` : ""}</td>`;
    },
  };
}
const COL_TESTED = colTested("perf");
const COL_TESTED_STREAM = colTested("stream");
const COL_TESTED_MEMORY = colTested("memory");
// concAt(env): the concurrency rung a throughput envelope held its ceiling at (conc_at_* from the snapshot,
// falling back to the legacy *_concurrency). Null-safe — never fabricated (renders n/a when absent).
function concAt(env) {
  if (!isEnvelope(env)) return null;
  return env.conc_at ?? env.concurrency ?? null;
}
// Replaces two retired throughput cells (rps_sustained_20ms / rps_max_proxy: one sweep collapsed
// twice by two algorithms that could contradict each other) with one reader over the frontier at a
// named bound. Chosen cell's reading at the reader's selected bound; concurrency rides inline
// ("18,995 @ 8 conc"), full reading sentence on the tooltip.
function frontierChooserCell(g, st = state, boundMs = selectedBound(st)) {
  const p = chooserCellPerf(g, st);
  if (!p) return { v: null, text: "n/a", na: true };
  // Pinned to one rung, when the reader asks for it: every row reports what it carried at the same
  // concurrency. No "@ N conc" suffix - it's in the column header once, same for every row.
  const conc = selectedConc(st);
  if (conc != null) {
    const r = rungAt(p, conc);
    if (!r) {
      // Not interpolated: a gateway that never drove this rung has no reading there.
      return { v: null, text: "n/a", na: true,
               note: `This gateway did not drive c=${fmtInt(conc)}; the rungs it did drive are on its curve.` };
    }
    const v = mval(r.rps);
    if (v == null)
      return { v: null, text: "n/a", na: true, note: naText(r.rps) };
    return { v, text: fmtRate(v), na: false,
             note: `Observed at c=${fmtInt(conc)}, the same rung every row on this table reports. `
                 + `The tail it produced there was ${fmtTail(r.p99_us)}.`
                 + (r.clean_windows < r.windows
                     ? ` ${r.clean_windows} of ${r.windows} windows at this rung carried no failures; the rate is their median.`
                     : ` Median of ${r.windows} windows.`) };
  }
  const cell = frontierCell(p, boundMs);
  const rd = cell.reading;
  if (cell.na || !rd || rd.concurrency == null) return cell;
  return { ...cell, text: `${cell.text} @ ${fmtInt(rd.concurrency)} conc` };
}
// Chosen cell's reading at one named bound, for frontier's per-bound columns. No inline concurrency
// (six "@ N conc" would be noise) - the tooltip carries the rest of the evidence.
function frontierBoundCell(g, boundMs, st = state) {
  const p = chooserCellPerf(g, st);
  if (!p) return { v: null, text: "n/a", na: true };
  return frontierCell(p, boundMs);
}
// The unbounded reading's rate (the "full rate" denominator), through the same accessor as every
// other rate so the shape column and "no bound" column can never disagree.
function frontierFullRate(frontier) {
  const rd = frontierAt(Array.isArray(frontier) ? frontier : [], null);
  return rd ? mval(rd.rps) : null;
}
/* The shape column: sparkline plus, in words, what share of full rate the cell still carried at the
   tightest tail it holds. Sortable by that share via heldSortKey (bound-of-origin first, then share -
   a share at 1ms and one at 50ms aren't the same quantity). */
function frontierShapeCell(g, st = state) {
  const p = chooserCellPerf(g, st);
  const f = frontierOf(p);
  if (!f.length) return { v: null, text: "n/a", na: true, note: p ? NO_FRONTIER_NOTE : "" };
  const h = frontierHeld(f);
  if (!h) {
    const full = frontierFullRate(f);
    const anyBounded = f.some((r) => r.bound_ms != null);
    // No unbounded reading means no denominator to be a share of; promoting the 100ms reading into
    // that role would silently rebase this row against a different quantity than every other row.
    if (full == null || !(full > 0) || !anyBounded) {
      return { v: null, text: "n/a", na: false, frontier: f,
        note: "No share of full rate can be stated: this cell has no unbounded reading for the bounded ones to " +
          "be a share of. The curve beside this is what was measured." };
    }
    // Held nothing anywhere is still a curve, and the most damning shape on the board: the sparkline
    // still draws (ticks on the floor, one point at the right). Only the percentage is withheld -
    // a share of a rate never reached under any bound isn't a number. Rendered in words, not a dash
    // (a bare "—" reads as missing data, which this is not: the gateway served cleanly).
    const loosest = FRONTIER_BOUNDS_MS[FRONTIER_BOUNDS_MS.length - 1];
    return { v: heldSortKey(HELD_NOTHING_INDEX, 0), text: `served nothing under ${boundLabel(loosest)}`,
      zero: true, na: false, frontier: f,
      note: `No share of full rate can be stated: this cell carried no measurable throughput under ANY ` +
        `published bound, so the only reading it has is the unbounded one. That is a measurement, not a gap - ` +
        `the gateway served cleanly and no concurrency it was offered kept 99% of requests under even ` +
        `${boundLabel(loosest)}, the loosest bound on the board. The curve beside this is the whole finding - ` +
        `a tick on the floor at every bound it could not hold.` };
  }
  const floorNote = h.lowerBound
    ? " The unbounded reading is itself a floor (the sweep ran out of ladder), so the real full rate may be higher and this share correspondingly lower."
    : "";
  const originIndex = FRONTIER_BOUNDS_MS.indexOf(h.boundMs);
  // The bound is part of the number, so it's part of the text on every row (including 1ms rows) - a
  // reader must never have to know the default bound to interpret "99%".
  const tighter = originIndex > 0
    ? ` It holds no rate at all under ${boundLabel(FRONTIER_BOUNDS_MS[originIndex - 1])}, which is why this share is read at ${boundLabel(h.boundMs)} rather than at the tightest bound the board publishes.`
    : "";
  return { v: heldSortKey(originIndex, h.frac), na: false, frontier: f,
    text: `${heldPct(h.frac)}% of its full rate at ${boundLabel(h.boundMs)}`,
    note: `${fmtRate(h.held)} req/s ${boundClause(h.boundMs)}, against ${fmtRate(h.full)} req/s ` +
      `${boundClause(null)}: ${heldPct(h.frac)}% of its full rate. A gateway near 100% at ` +
      `${boundLabel(FRONTIER_BOUNDS_MS[0])} gives up almost nothing when you demand a tight tail; a low share ` +
      `means it needs a loose tail to go fast.${tighter}${floorNote}` };
}
/* Shape column's <td>, shared by both tabs that carry it (was a copy-paste pair; a fix to one and not
   the other would disagree about the same cell). The held-nothing row gets the same muted treatment as
   "no rung held this tail", keyed off `zero` since that row's whole content IS the statement. */
function frontierShapeTd(g, st = state) {
  const c = frontierShapeCell(g, st);
  if (c.na) return `<td class="shape na" title="${esc(c.note || "")}">${esc(c.text)}</td>`;
  const label = c.zero ? `<span class="reading-none">${esc(c.text)}</span>` : esc(c.text);
  return `<td class="shape${c.zero ? " reading-zero" : ""}" title="${esc(c.note)}">` +
    `${frontierSpark(c.frontier, { ...boardFrontierScale(stateData(st)), boundMs: selectedBound(st) })}` +
    `<span class="shape-gain">${label}</span></td>`;
}
const COLUMN_SETS = {
  // Performance (Peak | Same | Custom): per-cell latency + throughput from the one 6x6 run. Columns are
  // identical in every mode; the chooser only changes which cell each row reads. Tested-on is present
  // in every mode, rendering a pill only when the row's lane has a record (audit #1/#13).
  performance: [
    COL_SEL, COL_NAME, COL_TESTED,
    { id: "lat50", label: "Added latency p50 (µs)", desc: false, title: "Gateway p50 minus direct-to-mock p50 at concurrency 1 on the chosen cell",
      get: (g) => chooserPerfCell(g, "added_latency_p50_us", fmtAdded) },
    { id: "lat", label: "Added latency p99 (µs)", desc: false, title: "Gateway p99 minus direct-to-mock p99 at concurrency 1 on the chosen cell",
      get: (g) => chooserPerfCell(g, "added_latency_p99_us", fmtAdded) },
    // Ranked throughput column at the reader-selected bound. Label names the bound (boundColLabel)
    // and re-renders on selection change, so no header can imply a bound it didn't use.
    // `costOnly` keeps cost columns off a board that can't answer them (see hasCost). Throughput
    // stops describing the gateway once it saturates its cores; cost per request has no such ceiling.
    // Requests-per-CPU-second is not a second column: it's exactly 1,000,000/cpu_us_per_request, the
    // same measurement inverted, so printing both would look like corroboration of a tautology. It
    // lives on the Charts tab instead, answering a different question.
    { id: "cpu", label: () => { const c = costWindowConc();
        return c == null ? "CPU per request (\u00b5s)" : `CPU per request (\u00b5s @ c=${c})`; },
      desc: false, costOnly: true,
      title: () => `Microseconds of gateway CPU - user plus system, summed across its whole process tree - spent per completed request, ` +
        `measured at a fixed concurrency held identical for every gateway (published beside it as the cost window). ` +
        `Unlike peak throughput this does not stop separating gateways once they saturate their cores. ` +
        `A window with any failure publishes no cost: CPU divided by only the successes would describe the failures, not the work.`,
      get: (g) => chooserPerfCell(g, "cpu_us_per_request", fmt2) },
    // The header carries the concurrency pin once for the whole column, so a screenshot of the table
    // is self-explanatory without repeating "@ 128 conc" on every row.
    { id: "rps", label: () => (selectedConc() != null
        ? `Req/s @ ${fmtInt(selectedConc())} conc · same rung, every row`
        : boundColLabel(selectedBound())), desc: true,
      title: () => `The most requests/sec the chosen cell carried ${boundClause(selectedBound())} and it failed no request it accepted, with the concurrency it was observed at. ` +
        `One of ${BOUND_CHOICES.length} readings of the SAME concurrency sweep published on every cell - switch the bound above to re-rank the board. ` +
        `A "≥" is a floor: the sweep ran out of ladder while that concurrency was still qualifying. Hover a cell for the tail it actually produced and the concurrency that stopped qualifying above it.`,
      get: (g, st = state) => frontierChooserCell(g, st) },
    // Shape beside the number: six figures don't communicate a slope, this does, and the share of full
    // rate makes it sortable. Header states the quantity in words rather than a bare ratio, which
    // needed a legend to be readable.
    { id: "shape", label: "Rate held at its tightest bound", desc: true,
      title: () => `The whole frontier as one line: throughput at ${BOUND_CHOICES.map(boundLabel).join(", ")}, left to right, on a scale shared by every row. ` +
        `Flat means the gateway holds its rate even under a tight tail; a line climbing from the floor means it needs a loose tail to go fast. ` +
        `Log scale, so equal slopes are equal RATIOS - which is what the shape means - and the slowest gateway on the board is still visible. ` +
        `The dotted rule marks the bound the ranked column is reading; an open dot marks a reading that is a floor rather than a ceiling; a tick on the baseline means the gateway served but NO concurrency held that tail (a measured nothing, not a missing measurement). ` +
        `"N% OF ITS FULL RATE AT B" is what the cell still carried at B, the tightest published bound it holds any rate at, as a share of its rate with no latency bound at all. Sorting groups the column by that bound first, because 99% at 1 ms and 99% at 50 ms are opposite findings.`,
      get: (g, st = state) => frontierShapeCell(g, st),
      render: (g, st = state) => frontierShapeTd(g, st) },
  ],
  /* Frontier: the whole curve, one row per gateway, all six readings side by side. Its own tab rather
     than more Performance columns, since less scrolling wins. Performance answers "who is fastest at
     my bound"; this answers "what shape is each gateway". Every column is a real published reading,
     none derived; the selected bound's column is marked. */
  frontier: [
    COL_SEL, COL_NAME, COL_TESTED,
    ...BOUND_CHOICES.map((b) => ({
      id: boundColId(b), label: boundLabel(b), group: BOUND_GROUP_LABEL, desc: true,
      // Six headers share one spanning group (BOUND_GROUP_LABEL) and keep only their own bound, rather
      // than repeating "Req/s · 99% under N ms" six times; full sentence still lives on each tooltip.
      // Observed tail rides under the number since "4 ms under 100 ms" and "99 ms under it" differ.
      title: `The most requests/sec the chosen cell carried ${boundClause(b)} and it failed no request it accepted. ` +
        `Under each number is the tail it ACTUALLY produced there, which is never the bound. "≥" marks a floor.`,
      get: (g, st = state) => frontierBoundCell(g, b, st),
      render: (g, st = state) => {
        const c = frontierBoundCell(g, b, st);
        const sel = selectedBound(st) === b || (b == null && selectedBound(st) == null) ? " bound-col" : "";
        if (c.na) return `<td class="na${sel}" title="${esc(c.note || "")}">${esc(c.text)}</td>`;
        const rd = c.reading;
        // Sub-line: the tail actually produced for a real reading, or the "why" for a measured nothing -
        // bare cells there would read as missing measurements rather than the damning finding they are.
        const sub = c.why
          ? `<span class="reading-none">${esc(c.why)}</span>`
          : (rd && rd.p99_us != null ? `<span class="reading-tail">tail ${esc(fmtTail(rd.p99_us))}</span>` : "");
        return `<td class="reading${c.why ? " reading-zero" : ""}${sel}" title="${esc(c.note)}">${esc(c.text)}${sub}</td>`;
      },
    })),
    { id: "shape", label: "Rate held at its tightest bound", desc: true,
      title: "The six readings as one line, on a scale shared by every row, and beside it what the cell still carried at the TIGHTEST bound it holds any rate at, as a share of its full rate with no latency bound at all. 99% at 1 ms is the good shape (it gives up almost nothing at the tightest tail we publish); 99% at 50 ms is a gateway that holds no rate at all under 50 ms. Sorting groups by that bound first for exactly that reason.",
      get: (g, st = state) => frontierShapeCell(g, st),
      render: (g, st = state) => frontierShapeTd(g, st) },
  ],
  // Streaming (Peak | Same | Custom): per-cell SSE columns from the same run. Per-cell streaming is
  // measured on the diagonal today, so Same/Custom read n/a where the matrix carries no cell.
  streaming: [
    COL_SEL, COL_NAME, COL_TESTED_STREAM,
    { id: "sttft50", label: "Added TTFT p50 (µs)", desc: false, title: "Added time-to-first-token p50: the extra wait before the stream's first token, gateway minus direct-to-mock, at concurrency 1, on the chosen cell. Lower is better.",
      get: (g) => chooserStreamCell(g, "added_ttft_p50_us", fmtUsMs) },
    { id: "sttft", label: "Added TTFT p99 (µs)", desc: false, title: "Added time-to-first-token p99 on the chosen cell. Lower is better.",
      get: (g) => chooserStreamCell(g, "added_ttft_p99_us", fmtUsMs) },
    { id: "sgap50", label: "Added gap p50 (µs)", desc: false, title: "The extra pause the gateway adds between streamed tokens, p50, on the chosen cell. Lower is better.",
      get: (g) => chooserStreamCell(g, "added_gap_p50_us", fmtUsMs) },
    { id: "sgap", label: "Added gap p99 (µs)", desc: false, title: "The extra pause the gateway adds between streamed tokens, p99, on the chosen cell. Lower is better.",
      get: (g) => chooserStreamCell(g, "added_gap_p99_us", fmtUsMs) },
    { id: "streams", label: "Streams sustained", desc: true, title: "Max concurrent SSE streams sustained (bisected true concurrency) with EVERY expected frame delivered, no stalls, <0.1% errors, on the chosen cell",
      get: (g) => chooserStreamCell(g, "streams_sustained", fmtInt) },
    // `cpufps`/`cpu_fps` is retired: it counted frames/sec without a delivery gate, so a gateway
    // dropping frames could post a higher rate than one delivering all of them. `streams_sustained_fps`
    // is the honest replacement, measured where every frame arrived.
    { id: "streamfps", label: "Streams sustained (frames/s)", desc: true, title: "The frame rate the sustained-streams ceiling held, on the chosen cell: the throughput behind the stream count, measured where every expected frame was delivered. Higher is better.",
      get: (g) => chooserStreamCell(g, "streams_sustained_fps", fmtInt) },
  ],
  // Memory (cell-chooser driven, own Min | Max | Same | Custom modes): idle/steady-state/growth/recovered
  // RSS plus the cell the window ran on and the RSS curve. perCellOnly columns are dropped on bundles
  // predating per-cell measurement.
  // Steady-state column keeps id "mempeak" on purpose (URL contract for shared permalinks); only the
  // label changed.
  memory: [
    COL_SEL, COL_NAME,
    // Same pill renderer every other tab uses, bound to the memory lane.
    COL_TESTED_MEMORY,
    /* Header names the scope of its median: with per-cell data, this column is the median across the
       gateway's cold samples (one per cell), while the curve on the same row belongs to the selected
       cell - they can differ, so the row label says "all cells" rather than just "median". */
    { id: "memidle", label: () => (hasPerCellMemory() ? "Idle RSS (MiB, all cells)" : "Idle RSS (MiB)"), desc: false,
      title: () => (hasPerCellMemory()
        ? "Cold idle process RSS, before the first request is served. Sampled once per cell with no cell-specific work involved, so this is the median across those cold samples (hover for the spread) and it is valid in every mode. Lower is better."
        : `Cold idle process RSS: median over a ${memWindowLabel(boardMemWindows().idle)} window on a fresh cold-restarted process, before any load. Lower is better.`),
      get: (g, st = state) => {
        if (!hasPerCellMemory(stateData(st))) return memCell(g, "idle_rss_mib", fmt1, st);
        // All-or-nothing row: a row whose chosen cell has no displayable value is fully empty - idle
        // must not survive as one lone number on an otherwise-n/a row.
        if (!recordShowsValues(memoryFor(g, st))) return { v: null, text: "n/a", na: true };
        const i = idleAcrossCells(g);
        if (!i) return { v: null, text: "n/a", na: true };
        return { v: i.median, text: fmt1(i.median), na: false,
          note: `median of ${fmtInt(i.n)} cold samples, one per served cell; spread ${fmt1(i.min)} to ${fmt1(i.max)} MiB` };
      } },
    { id: "mempeak", label: () => (hasPerCellMemory() ? "Steady-state RSS (MiB)" : "Peak RSS (MiB)"), desc: false,
      title: () => (hasPerCellMemory()
        ? "Resident memory once it stopped climbing, under a fixed load on the chosen cell that runs until RSS is steady rather than for a fixed time. A gateway that never went steady has no steady state and reads n/a: its growth rate is the reading. Lower is better."
        : "Max process RSS observed while the identical fixed load runs on this gateway's peak cell. Same load recipe for every gateway. Lower is better."),
      get: (g, st = state) => (hasPerCellMemory(stateData(st))
        ? memCell(g, "steady_state_rss_mib", fmt1, st) : memCell(g, "peak_rss_mib", fmt1, st)) },
    // Growth: ~0 once settled, the leak rate when it never did. Turns "did not plateau" from a missing
    // value into the headline finding, so it's its own column rather than a footnote.
    { id: "memgrowth", label: "Growth (MiB/min)", desc: false, perCellOnly: true,
      title: "How fast RSS was still rising over the final window on the chosen cell. Around zero once the gateway has settled. If no steady state was reached, this rate under this load IS the reading, and no steady-state number exists to report instead. Lower is better.",
      get: (g, st = state) => {
        const m = memoryFor(g, st);
        const c = memCell(g, "growth_rate_mib_per_min", fmt1, st);
        if (c.na || !m) return c;
        if (m.plateaued === false && !SHOW_GROWTH_VERDICT) {
          // The note explains the number in the tooltip rather than stamping a verdict on the cell.
          const gr0 = mval(m.growth_rate_mib_per_min);
          return { ...c, note: gr0 != null && gr0 > 0
            ? "This cell never went steady: the load stopped at the cap with RSS still climbing at this rate, so there is no steady state to report and a longer load would have produced a larger number."
            : gr0 != null && gr0 < 0
            ? "This cell never went steady, but the rate is negative - it was releasing memory when the load hit the cap."
            : "This cell never went steady; neither a shape verdict nor a non-zero rate establishes which way RSS was moving." };
        }
        if (m.plateaued === false) {
          /* Suffix follows the shape, not just "did not settle": a negative rate (memory releasing)
             must never be labelled "(leak)". `memShape` reads the engine's own verdict; a record with
             no shape gets neutral wording rather than a guess. */
          const sh = memShape(m);
          if (sh === "releasing")
            return { ...c, text: `${c.text} (releasing)`, note: "This cell never went steady, but it was RELEASING memory when the load hit the cap - the rate is negative. That is the opposite of a leak: a longer load would not have produced a larger number." };
          if (sh === "swinging")
            return { ...c, text: `${c.text} (swing)`, note: "This cell never went steady, but it did not grow either: RSS swung around a level it kept returning to. This rate is how fast the window happened to be moving when it closed - a fact about the sampling instant, not a leak." };
          if (sh === "climbing")
            return { ...c, text: `${c.text} (leak)`, note: "This cell never went steady: the load stopped at the cap with RSS still climbing at this rate, so there is no steady state to report and a longer load would have produced a larger number." };
          // No shape published: fall back to the sign of the rate itself as the reading.
          const gr = mval(m.growth_rate_mib_per_min);
          if (gr != null && gr > 0)
            return { ...c, text: `${c.text} (leak)`, note: "This cell never went steady: the load stopped at the cap with RSS still climbing at this rate, so there is no steady state to report and a longer load would have produced a larger number." };
          if (gr != null && gr < 0)
            return { ...c, text: `${c.text} (releasing)`, note: "This cell never went steady, but the rate is negative - it was RELEASING memory when the load hit the cap, which is the opposite of a leak." };
          return { ...c, text: `${c.text} (no steady state)`, note: "This cell never went steady, and neither a shape verdict nor a non-zero rate establishes which way RSS was moving." };
        }
        return { ...c, note: c.note || "Settled: RSS had stopped climbing when the load was terminated." };
      } },
    { id: "memrecov", label: () => `Recovered @${memWindowLabel(boardMemWindows().recovery)} (MiB)`, desc: false,
      title: () => `Process RSS at the end of the ${memWindowLabel(boardMemWindows().recovery)} recovery window after the fixed load stops — does the gateway release memory? Lower is better.`,
      get: (g, st = state) => memCell(g, "recovered_rss_mib", fmt1, st) },
    { id: "memcurve", label: "RSS curve", desc: false, sortable: false,
      title: () => (hasPerCellMemory()
        ? "RSS across one process lifecycle on the chosen cell: cold idle → load run to steady state → recovery."
        : `RSS across the memory window on one process lifecycle: ${memWindowLabel(boardMemWindows().idle)} cold idle → fixed load on the peak cell → ${memWindowLabel(boardMemWindows().recovery)} recovery`),
      // All-or-nothing like every other memory column: rss_series isn't a sealed metric, so without
      // this guard an empty record could still show a live sparkline.
      get: (g, st = state) => {
        const m = memoryFor(g, st);
        const shows = recordShowsValues(m) && Array.isArray(m.rss_series) && m.rss_series.length >= 2;
        return { v: null, text: "", na: !shows };
      },
      /* One compact ~34px curve replacing six stacked block elements (~350px row). A <button>, not a
         div: content only in a `title` is invisible to touch/keyboard users, so this needs to be
         focusable with an aria-label, and Enter/Space bubbles to the row handler that opens the drawer. */
      render: (g, st = state) => {
        const m = memoryFor(g, st);
        const spark = m && recordShowsValues(m) ? rssCurves(m, { compact: true }) : "";
        if (!spark) return `<td class="memcurve na">n/a</td>`;
        const summary = memCurveSummary(m);
        return `<td class="memcurve"><button type="button" class="rss-life-btn" title="${esc(summary)}" ` +
          `aria-label="${esc(summary)}">${spark}</button></td>`;
      } },
  ],
  // Governance is RETIRED under matrix-sole-source: no tab, no column (busbar-only, non-default suite).
};
// A column/metric label or title, plain string or a function rendering from live data (audit #14).
function txt(x) { return typeof x === "function" ? String(x() ?? "") : String(x ?? ""); }
// Columns for a view; perf tabs use COLUMN_SETS. perCellOnly columns are dropped when the data can't
// fill them (a growth column reading n/a on every row would be noise).
function columnsFor(view, data = (typeof state !== "undefined" ? state.data : null)) {
  let cols = COLUMN_SETS[view] || COLUMN_SETS.performance;
  if (!hasPerCellMemory(data)) cols = cols.filter((c) => !c.perCellOnly);
  if (!hasCost(data)) cols = cols.filter((c) => !c.costOnly);
  return cols;
}
/* The roster's row order for one column and one direction. The name tiebreak is always ascending
   regardless of sort direction, so toggling descending reverses the ranking without also reversing
   ties' alphabetical order. Missing values always sink to the bottom in both directions. */
// Tiebreak column: the second-best measurement, not the alphabet - a dense tie (e.g. three gateways
// at a measured zero) should not fall straight to alphabetical order, which would read as a ranking
// it isn't. All tiebreak columns here are latencies (lower better), so ties sort ascending regardless
// of the primary column's direction.
const VIEW_TIEBREAK = {
  performance: "lat50",
  // Frontier has no latency column and its own columns are all rates, so ties there fall to the name.
  streaming: "sttft50",
  memory: "memidle",
};
function rowComparator(col, desc, tiebreak) {
  return (a, b) => {
    const va = col.get(a).v, vb = col.get(b).v;
    const byName = a.display.localeCompare(b.display);
    // Tiebreak measurement, used only when the primary column ties; never the sorted column itself.
    const byTie = () => {
      if (!tiebreak || tiebreak.id === col.id) return byName;
      const ta = tiebreak.get(a).v, tb = tiebreak.get(b).v;
      if (ta === null || tb === null || typeof ta === "string" || typeof tb === "string") return byName;
      return ta === tb ? byName : ta - tb;
    };
    if (va === null && vb === null) return byTie();
    if (va === null) return 1;
    if (vb === null) return -1;
    const cmp = typeof va === "string" ? va.localeCompare(vb) : va - vb;
    if (cmp === 0) return byTie();
    return desc ? -cmp : cmp;
  };
}
/* Every column id across all tabs - used to validate a sort id coming from a shared URL. */
const ALL_COLUMN_IDS = new Set(Object.values(COLUMN_SETS).flat().map((c) => c.id));

/* Metric groups per lane: drives the drawer and the compare table.
   best: "min"/"max" picks the neutral best-value highlight by measurement.
   `get` (optional) returns the CANONICAL record for the lane instead of the raw suite file
   (g[key]); the perf and xlate lanes read the same canonical objects the table reads, so the
   drawer/compare can never show a different number than the table (the R1/R3 rule).
   `pathNote` (optional) returns a one-line disclosure of WHICH path the record measured. */
const laneDialect = (d) => (MATRIX_LABELS[d] || d || "?");
const LANES = [
  // pathNote renders from the record's source.sweep stamp via caption(), no hard-coded string. The
  // memory lane appends its cell attribution (load_cell) after.
  {
    key: "perf", label: "Latency & throughput", flag: "served", err: "serve_error",
    get: canonicalPerf,
    pathNote: (j) => j && j.source ? caption(j) : "",
    metrics: [
      { k: "added_latency_p50_us", label: "Added latency p50 (µs)", best: "min", fmt: fmtAdded },
      { k: "added_latency_p99_us", label: "Added latency p99 (µs)", best: "min", fmt: fmtAdded },
      // The two legs the added-latency figure above is the difference of (gateway minus direct-to-mock
      // at c=1), so a reader can check the arithmetic. direct_c1_p99_us is best:null since it's a
      // shared baseline (evidence, not a contest - see bestIndex).
      { k: "gateway_c1_p99_us", label: "Gateway p99 @ c=1 (µs)", best: "min", fmt: fmtUsMs },
      { k: "direct_c1_p99_us", label: "Direct-to-mock p99 @ c=1 (µs)", best: null, fmt: fmtUsMs },
      // Every reading, one row each, replacing two retired scalars that collapsed the same sweep
      // twice. All six listed since the drawer/compare are where a reader checks the evidence.
      // `cell` (not `k`+`fmt`) since a reading is an envelope plus its own evidence.
      ...BOUND_CHOICES.map((b) => ({
        // A stable key per reading, mirroring the engine's absence keys (frontier.10ms.rps).
        k: `frontier.${b == null ? "unbounded" : `${b}ms`}`,
        label: boundColLabel(b), best: "max", cell: (rec) => frontierCell(rec, b),
      })),
    ],
    // Same sparkline the table shows, at the same shared scale.
    extra: (j) => frontierBlock(j),
    // Compare panel: one row of curves across the gateways being compared.
    cmpExtra: (j) => frontierBlock(j, { compact: true }),
  },
  {
    key: "memory", label: "Memory", flag: "served", err: "serve_error",
    get: canonicalMemory,
    pathNote: (j) => {
      const base = j && j.source ? caption(j) : "";
      // Legacy record names its peak-cell basis; per-cell record names window outcome + search size.
      // Both end with the producer's honesty disclosures (memory.protocol), surfaced, never silent.
      const note = j && j.load_cell
        ? `${base}, identical fixed load on ${memLoadCellLabel(j.load_cell)} (this gateway's peak cell)${memDisclosure(j)}`
        : (j && j.plateaued != null ? `${base}, ${memCellTip(j)}` : `${base}${memDisclosure(j)}`);
      return note;
    },
    // Renders only when rss_series exists (>=2 points); otherwise extra() returns "".
    extra: (j) => rssCurves(j),
    metrics: [
      { k: "idle_rss_mib", label: "Idle RSS (MiB)", best: "min", fmt: fmt1 },
      // Both shapes listed since both are published: per-cell record carries steady state (or none)
      // and a growth rate; legacy record carries the peak of a fixed-duration load.
      { k: "steady_state_rss_mib", label: "Steady-state RSS (MiB)", best: "min", fmt: fmt1 },
      { k: "growth_rate_mib_per_min", label: "Growth (MiB/min)", best: "min", fmt: fmt1 },
      { k: "peak_rss_mib", label: "Peak RSS (MiB)", best: "min", fmt: fmt1 },
      // Kernel's own high-water mark: the independent check on the sampled peak above (C7 warns when
      // hwm sits below peak).
      { k: "peak_rss_hwm_mib", label: "Peak RSS high-water (MiB)", best: "min", fmt: fmt1 },
      // RSS after the recovery window. Absent on pre-recovery bundles -> reads n/a.
      { k: "recovered_rss_mib", label: () => `Recovered @${memWindowLabel(boardMemWindows().recovery)} (MiB)`, best: "min", fmt: fmt1 },
    ],
  },
  {
    key: "stream", label: "Streaming", flag: "stream_served", err: "stream_error",
    get: canonicalStreaming,
    pathNote: (j) => j && j.source ? caption(j) : "",
    metrics: [
      { k: "added_ttft_p99_us", label: "Added TTFT p99 (µs)", best: "min", fmt: fmtUsMs },
      { k: "added_gap_p99_us", label: "Added per-token p99 (µs)", best: "min", fmt: fmtUsMs },
      { k: "streams_sustained", label: "Streams sustained", best: "max", fmt: fmtInt },
      // Sealed under the same mock-bound flag as the count above (audit #11); without this row two
      // gateways holding the same stream count at very different frame rates would read as identical.
      { k: "streams_sustained_fps", label: "Streams sustained (frames/s)", best: "max", fmt: fmtInt },
      // `cpu_fps` retired: counted relay frames/sec without the delivery gate, a loss rate with a
      // numerator. The row above is the honest replacement.
    ],
  },
  {
    key: "xlate", label: "Translation", flag: "xlate_served", err: "xlate_error",
    get: canonicalXlate,
    pathNote: (j) => j && j.source ? caption(j) : "",
    metrics: [
      { k: "added_latency_p50_us", label: "Added latency p50 (µs)", best: "min", fmt: fmtInt },
      { k: "added_latency_p99_us", label: "Added latency p99 (µs)", best: "min", fmt: fmtInt },
      // One reading here, at the reader-selected bound, since "what does translating cost" is only
      // answerable by comparing against passthrough at the same bound; the full curve is drawn below.
      { k: "frontier.selected", label: () => boundColLabel(selectedBound()), best: "max",
        cell: (rec) => frontierCell(rec, selectedBound()) },
    ],
    extra: (j) => frontierBlock(j),
    cmpExtra: (j) => frontierBlock(j, { compact: true }),
  },
];
/* Curve as a block for the drawer/compare panel: sparkline plus share of full rate in words. Returns
   "" for a record with no frontier, so a legacy row has no curve rather than an empty captioned frame. */
function frontierBlock(rec, opts = {}) {
  const f = frontierOf(rec);
  if (!f.length) return "";
  const spark = frontierSpark(f, { ...boardFrontierScale(), boundMs: selectedBound() });
  if (!spark) return "";
  const h = frontierHeld(f);
  const words = h
    ? `${heldPct(h.frac)}% of its full rate at ${boundLabel(h.boundMs)} (${fmtRate(h.held)} of ${fmtRate(h.full)} req/s)`
    : "one reading only: no share of full rate to state";
  return `<div class="frontier-block${opts.compact ? " compact" : ""}">${spark}` +
    `<div class="stamp muted">${esc(words)}</div></div>`;
}

/* The record a drawer/compare lane shows, chooser-aware so it agrees with the table in every mode.
   Perf + streaming are cell-chooser driven (PERF_VIEWS); memory + xlate are not (they read l.get).
   The returned perf record carries the chosen cell's source/dialect/ingress/egress so pathNote names
   the same path as the table pill. */
function laneRecord(l, g, st = state) {
  if (l.key === "perf") {
    const p = chooserCellPerf(g, st);
    if (!p) return null;
    // Metrics are already sealed envelopes and the record already self-describing (stampChosen), so
    // nothing to gate or re-stamp here (audit #1/#6).
    return { served: true, ...p };
  }
  if (l.key === "stream") return chooserCellStream(g, st);
  // Memory is chooser-driven too: canonicalMemory would show a different cell than the table.
  if (l.key === "memory") return memoryFor(g, st);
  return l.get ? l.get(g) : g[l.key];
}
/* Sweep-curve series for the chosen cell, used by drawer + compare so the plotted curve matches the
   table + headline. One curve, not two (sustained/max-proxy used to be drawn separately despite being
   the same sweep read twice). Marker is the reading at the selected bound. Returns [] if absent. */
function perfSweepSeries(g, colors, st = state) {
  const p = chooserCellPerf(g, st);
  if (!p || !Array.isArray(p.sweep) || !p.sweep.length) return [];
  const bound = selectedBound(st);
  const rd = frontierAt(frontierOf(p), bound);
  // C5: through mval(), never a bare `.value` deref.
  const v = rd ? mval(rd.rps) : null;
  return [{
    label: colors.sweepLabel || `req/s across the sweep · marked at ${boundLabel(bound)}`,
    color: colors.sustained || colors.max,
    sweep: p.sweep,
    peak: v != null && rd.concurrency != null ? { rps: v, conc: rd.concurrency } : null,
  }];
}
/* Age of the record this lane shows, from its own source.measured_at (audit #8) - not the row badge's
   matrix age, since a lane can project from a never-refreshed legacy suite. */
function laneAgeNote(j, now = Date.now()) {
  const at = j && j.source && j.source.measured_at;
  const age = at ? fmtAge(at, now) : "";
  return age ? ` · measured ${age}` : "";
}
// pathNote for a chooser-driven lane, always through caption(j); the mode is appended only as a UI
// hint, never as provenance.
function lanePathNote(l, j, st = state) {
  const base = l.pathNote ? l.pathNote(j) : "";
  if (!base) return "";
  const MODE_HINT = { same: "Same-dialect", custom: "chosen", min: "lowest steady-state", max: "highest steady-state" };
  const mode = l.key === "memory" ? memoryMode(st) : st.mode;
  const hint = (l.key === "perf" || l.key === "stream" || l.key === "memory") && MODE_HINT[mode]
    ? ` (the ${MODE_HINT[mode]} cell the table shows)` : "";
  return `${base}${hint}${laneAgeNote(j)}`;
}

/* ---- state + URL codec ------------------------------------------------------ */
function newState() {
  return {
    data: null,
    category: DEFAULT_CATEGORY,
    view: DEFAULT_VIEW,
    q: "",
    sortCol: "rps",
    sortDesc: true,
    /* The tail-latency bound the board is showing. `null` is a real choice (unbounded reading), so
       the absent state is DEFAULT_BOUND_MS, never null. A view, not a verdict: every bound is
       published on every cell, and switching it re-ranks the board. */
    bound: DEFAULT_BOUND_MS,
    // Mode each chooser family was last left on, so crossing tabs restores the reader's own choice.
    // Never encoded into the URL: a link carries one mode, for the view it names.
    modeMemo: { perf: "peak", memory: "min" },
    needStream: false,
    needXlate: false,
    // Cell chooser (Performance + Streaming): which cell(s) of the one 6x6 run to show.
    //   mode "peak"   -> each gateway's own best diagonal (best_cell); no dialect params
    //   mode "same"   -> sameDialect's diagonal (X->X) for every gateway
    //   mode "custom" -> xlateIn->xlateOut cell (any pair, incl. translation) for every gateway
    //   mode "min"/"max" -> memory only: this gateway's lowest/highest steady-state cell
    mode: "peak",
    sameDialect: "openai",
    // Was the Same dialect pinned by the URL? Memory's Same default is the widest-coverage dialect,
    // computed at boot, and a pinned ?d= must survive that seeding.
    sameDialectPinned: false,
    // Custom mode: the pinned ingress->egress pair the whole table is projected on. Both ends fixed
    // so every row is the identical cell; a gateway not serving this exact cell reads n/a.
    xlateIn: "openai",
    xlateOut: "anthropic",
    cmp: [],        /* gateway keys selected for compare, max 3 */
    cmpOpen: false, /* compare panel visible */
    drawer: null,   /* gateway key open in the drawer */
  };
}
const state = newState();

// Capability filter toggles. Governance is retired: not a filter, column, or drawer section.
const CAPS = [["needStream", "stream"], ["needXlate", "xlate"]];

/* Serialize the shareable parts of state into a clean path URL:
   /<category>/<view>?<params>. The default view (the roster overview) omits the
   view segment and default params are omitted, so the pristine view keeps a clean
   URL (/gateways = the category root). Returns path + query, e.g.
   /gateways/matrix?sort=mempeak&dir=asc. */
function encodeUrl(st) {
  // Home is the bare site root: no category segment, no params.
  if (st.view === HOME_VIEW) return "/";
  const p = new URLSearchParams();
  if (st.q) p.set("q", st.q);
  const caps = CAPS.filter(([k]) => st[k]).map(([, name]) => name);
  if (caps.length) p.set("cap", caps.join("|"));
  // Bound is in the URL so a shared link reproduces the reading it was shared at. Encoded only on
  // BOUND_VIEWS; `none` spells out the unbounded reading rather than an empty value.
  if (BOUND_VIEWS.has(st.view) && selectedBound(st) !== DEFAULT_BOUND_MS)
    p.set("bound", selectedBound(st) == null ? "none" : String(selectedBound(st)));
  // Clean URL omits the sort when it equals the tab's default column + direction. On frontier the
  // default column follows the selected bound.
  const defSort = st.view === "frontier" ? boundColId(selectedBound(st)) : (VIEW_SORT[st.view] || "rps");
  const defCol = columnsFor(st.view).find((c) => c.id === defSort);
  const defDesc = defCol ? defCol.desc !== false : true;
  if (st.sortCol !== defSort || st.sortDesc !== defDesc) {
    p.set("sort", st.sortCol);
    p.set("dir", st.sortDesc ? "desc" : "asc");
  }
  if (st.cmp.length) p.set("cmp", st.cmp.join("|"));
  if (st.cmpOpen) p.set("cv", "1");
  if (st.drawer) p.set("gw", st.drawer);
  // Cell chooser encoding (Performance, Streaming, Memory). Clean URL omits the view's default mode;
  // Same carries the dialect, Custom the pinned pair, so a link reproduces exactly the cell(s) shown.
  // Pinned rung travels in the link too, so "same-concurrency comparison" links reproduce exactly.
  if (st.conc != null) p.set("conc", String(st.conc));
  if (CHOOSER_VIEWS.has(st.view)) {
    const mode = st.view === "memory" ? memoryMode(st) : st.mode;
    if (mode !== defaultMode(st.view)) p.set("mode", mode);
    if (mode === "same") {
      // Memory's Same default is the widest-coverage dialect, derived from the run.
      const isDefault = st.view === "memory" && st.sameDialect === widestDialect(st.data);
      if (!isDefault) p.set("d", st.sameDialect);
    } else if (mode === "custom") { p.set("in", st.xlateIn); p.set("out", st.xlateOut); }
  }
  const cat = CATEGORIES[st.category] ? st.category : DEFAULT_CATEGORY;
  const path = st.view && st.view !== DEFAULT_VIEW ? `/${cat}/${st.view}` : `/${cat}`;
  const qs = p.toString();
  return qs ? `${path}?${qs}` : path;
}

/* Parse a path + query (+ optional legacy #hash) back into state. Bare root/unknown segment is the
   home landing page; known category segment enters it, unknown views fall back to the default.
   Legacy hash params (#view=matrix&sort=...) win over the query so old shared URLs keep resolving;
   boot() then rewrites them to path form. */
function decodeUrl(pathname, search, hash) {
  const st = newState();
  const segs = String(pathname || "/").split("/").filter(Boolean);
  // Resolve a raw view token, honoring legacy aliases so old shared/deep links keep landing.
  const resolveView = (v) => (VIEWS.includes(v) ? v : VIEW_ALIASES[v] || null);
  let i = 0;
  if (segs[i] && CATEGORIES[segs[i]]) {
    st.category = segs[i++];
    if (segs[i] && resolveView(segs[i])) st.view = resolveView(segs[i]);
  } else {
    // No/unknown category segment: the home landing page. A legacy hash view= still pulls state
    // back into the default category.
    st.view = HOME_VIEW;
  }
  const legacy = String(hash || "").replace(/^#/, "");
  const p = new URLSearchParams(legacy.includes("=") ? legacy : String(search || "").replace(/^\?/, ""));
  const list = (k) => (p.get(k) || "").split("|").filter(Boolean);
  if (p.get("view") && resolveView(p.get("view"))) st.view = resolveView(p.get("view")); /* legacy hash form */
  st.q = p.get("q") || "";
  // Retired class/language chip filters: a stale ?cls=/?lang= is silently ignored.
  for (const cap of list("cap")) {
    const hit = CAPS.find(([, name]) => name === cap);
    if (hit) st[hit[0]] = true;
  }
  // Bound before sort: `?sort=f50` and `?bound=50` are the same intent expressed two ways. A bound
  // the board doesn't publish is ignored rather than honoured, falling back to the named default.
  const rawBound = p.get("bound");
  if (rawBound === "none") st.bound = null;
  else if (rawBound != null && FRONTIER_BOUNDS_MS.includes(Number(rawBound))) st.bound = Number(rawBound);
  // Accept any real, sortable column id from any tab; renderTable snaps back to the tab default if it
  // doesn't belong. Retired sort ids map onto the column carrying that ranking now.
  const rawSort = SORT_ALIASES[p.get("sort")] || p.get("sort");
  if (rawSort && ALL_COLUMN_IDS.has(rawSort) && rawSort !== "sel") {
    st.sortCol = rawSort;
    st.sortDesc = p.get("dir") !== "asc";
  } else {
    // No sort param: default to this view's headline column and its own natural direction (not the
    // global default, which would sort ascending-better metrics worst-first).
    st.sortCol = st.view === "frontier" ? boundColId(st.bound) : (VIEW_SORT[st.view] || "rps");
    const dc = columnsFor(st.view).find((c) => c.id === st.sortCol);
    st.sortDesc = dc ? dc.desc !== false : true;
  }
  st.cmp = list("cmp").slice(0, 3);
  st.cmpOpen = p.get("cv") === "1" && st.cmp.length >= 2;
  st.drawer = p.get("gw") || null;
  // Cell chooser decoding. Clean params (mode/d/in/out) plus legacy translation params (xin/xout,
  // retired Matched tab) - a legacy link lands in Custom mode on the pinned pair.
  const rawConc = Number(p.get("conc"));
  // Only a rung the run actually drove is accepted; otherwise falls back to the peak view.
  st.conc = Number.isFinite(rawConc) && rawConc > 0 ? rawConc : null;
  const mode = p.get("mode");
  if (CHOOSER_MODES.has(mode) || MEM_CHOOSER_MODES.has(mode)) st.mode = mode;
  if (MATRIX_CELLS.includes(p.get("d"))) { st.sameDialect = p.get("d"); st.sameDialectPinned = true; }
  const cin = p.get("in") || p.get("xin");
  const cout = p.get("out") || p.get("xout");
  if (MATRIX_CELLS.includes(cin)) st.xlateIn = cin;
  if (MATRIX_CELLS.includes(cout)) st.xlateOut = cout;
  // A legacy Matched link (xin/xout, no explicit mode) means the pinned-pair Custom view.
  if (!CHOOSER_MODES.has(mode) && (p.get("xin") || p.get("xout"))) st.mode = "custom";
  // Coerce onto the view that received it: a ?mode=peak link on the memory tab must not render a
  // throughput-selected memory number, so it lands on Same instead.
  st.mode = resolveMode(st.mode, st.view);
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

const SITE_TITLE = "On the Bench · AI tool benchmarks";
/* Document title for a view. Pure, so testable without a document. View leads (not category) since
   it's the part that differs between open tabs, and "${category} ${view}" used to truncate to the
   shared word in a browser tab strip. */
function pageTitle(st = state) {
  if (st.view === HOME_VIEW) return SITE_TITLE;
  const cat = CATEGORIES[st.category] || CATEGORIES[DEFAULT_CATEGORY];
  const view = st.view !== DEFAULT_VIEW ? `${VIEW_LABELS[st.view] || st.view} · ` : "";
  return `${view}${cat.label} · ${SITE_TITLE}`;
}
function updateTitle() {
  if (NODE) return;
  document.title = pageTitle(state);
}

/* ---- filtering (pure) ------------------------------------------------------- */
function applyFilters(gateways, st) {
  const q = st.q.trim().toLowerCase();
  return gateways.filter((g) => {
    if (q && !g.display.toLowerCase().includes(q) && !g.key.toLowerCase().includes(q)) return false;
    if (st.needStream && !canonicalStreaming(g)) return false;
    if (st.needXlate && !hasTranslation(g)) return false;
    // Performance/Streaming deliberately unfiltered across every chooser mode: every gateway appears,
    // and one that doesn't serve the chosen cell reads n/a rather than disappearing.
    return true;
  });
}
// A gateway "translates" if it has a measured openai-in translation cell, or (legacy) served the
// xlate suite. Drives the translation tab's implicit filter and the capability toggle.
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
  // Below 1, Math.round would label every gridline "0" for a sub-1 domain (e.g. ~0.25 req/s
  // rungs). Two significant figures separates 0.25 from 0.04 without inventing digits.
  if (v > 0 && v < 1) return String(+v.toPrecision(2));
  return String(Math.round(v));
}

/* Draw at the display's real resolution, present at CSS size (avoids soft/upscaled text on
   high-DPI displays). Returns CSS dimensions; callers must use those for geometry since
   ctx.scale(dpr,dpr) puts the drawing coordinate system in CSS pixels. CSS size is stashed on the
   element for later hit-testing. Idempotent: a canvas already scaled must not be scaled again. */
function hidpi(canvas, ctx) {
  // `dataset` and `setTransform` are DOM/2D-context features a headless test stub need not provide,
  // and this must degrade to "draw at the size you were given" rather than throw - the geometry it
  // returns has to be right in node, where the only thing under test is the drawing logic.
  const ds = canvas.dataset || {};
  const dpr = (typeof window !== "undefined" && window.devicePixelRatio) || 1;
  const cssW = Number(ds.cssw) || canvas.width;
  const cssH = Number(ds.cssh) || canvas.height;
  if (canvas.dataset) {
    canvas.dataset.cssw = String(cssW);
    canvas.dataset.cssh = String(cssH);
  }
  const want = Math.round(cssW * dpr);
  if (dpr !== 1 && canvas.width !== want) {
    canvas.width = want;
    canvas.height = Math.round(cssH * dpr);
  }
  if (typeof ctx.setTransform === "function") ctx.setTransform(1, 0, 0, 1, 0, 0);
  if (dpr !== 1 && typeof ctx.scale === "function") ctx.scale(dpr, dpr);
  return { W: cssW, H: cssH };
}

function drawSweep(canvas, series, opts = {}) {
  const ctx = canvas.getContext && canvas.getContext("2d");
  if (!ctx) return null;
  const drawable = series.filter((s) => s.points && s.points.length);
  const pts = drawable.flatMap((s) => s.points);
  const { W, H } = hidpi(canvas, ctx);
  ctx.clearRect(0, 0, W, H);
  // padB carries the x tick labels, the axis title AND the legend row. The legend used to be drawn
  // top-right INSIDE the plot, which is exactly where a saturating throughput curve peaks - so it
  // collided with the peak label it was sitting on top of - and its box was a hard-coded 118px, so
  // any label wider than that ran off the right edge and was clipped. Below the axis it cannot
  // collide with data at any label length.
  const legendRows = opts.legend !== false && series.filter((s) => s && s.points && s.points.length).length > 1 ? 1 : 0;
  const padL = 58, padR = 14, padT = 16, padB = 34 + legendRows * 16;
  const fg = opts.fg || "#9aa4b2", grid = opts.grid || "rgba(154,164,178,.18)";
  if (!pts.length) {
    ctx.fillStyle = fg;
    ctx.font = "12px Inter, sans-serif";
    ctx.fillText("no sweep data", padL, H / 2);
    return null;
  }
  // x-axis domain: honor a shared concurrency domain (opts.xDomain) so stacked charts align on the
  // SAME x-axis; else fall back to this chart's own probed concurrencies.
  const lx = (opts.xDomain ? opts.xDomain : pts.map((p) => p.x)).map((v) => Math.log10(v));
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
  // Spaced in pixels, not index: on a log axis, adjacent concurrencies can land ~4px apart, so a
  // tick whose label would overlap the last one drawn is dropped instead.
  let lastX = -Infinity;
  xs.filter((_, i) => i % stride === 0 || i === xs.length - 1).forEach((v) => {
    ctx.strokeStyle = grid;
    ctx.beginPath(); ctx.moveTo(X(v), padT); ctx.lineTo(X(v), H - padB); ctx.stroke();
    const x = X(v);
    const half = ((ctx.measureText ? ctx.measureText(fmtTick(v)).width : 14) / 2) + 6;
    if (x - lastX < half * 2) return;
    lastX = x;
    ctx.fillStyle = fg;
    ctx.fillText(fmtTick(v), x, H - padB + 5);
  });
  /* axes */
  ctx.strokeStyle = fg;
  ctx.beginPath(); ctx.moveTo(padL, padT); ctx.lineTo(padL, H - padB); ctx.lineTo(W - padR, H - padB); ctx.stroke();
  /* axis labels */
  ctx.fillStyle = fg;
  ctx.textAlign = "center";
  ctx.fillText(opts.xLabel || "concurrency (log)", padL + (W - padL - padR) / 2, H - padB + 20);
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
  // Published-peak markers: labeled dot at each series' peak, sitting on the curve since the
  // headline is max() over these points.
  ctx.font = "11px Inter, sans-serif"; ctx.textAlign = "left"; ctx.textBaseline = "bottom";
  const placed = [];
  for (const s of drawable) {
    if (!s.mark) continue;
    const px = X(s.mark.x), py = Y(s.mark.y);
    ctx.strokeStyle = s.color; ctx.fillStyle = s.color; ctx.lineWidth = 1.6;
    ctx.beginPath(); ctx.arc(px, py, 4.6, 0, Math.PI * 2); ctx.stroke();
    ctx.beginPath(); ctx.arc(px, py, 2.0, 0, Math.PI * 2); ctx.fill();
    // keep the label inside the plot: flip left of the dot near the right edge
    const label = s.mark.label;
    const wide = (ctx.measureText ? ctx.measureText(label).width : label.length * 6) + 10;
    const lx0 = px + 7 + wide > W - padR ? px - 7 - wide : px + 7;
    // Two peaks near the same place is the normal case; a label that would overlap one already
    // placed drops below the dot instead.
    let ly = py - 6;
    if (placed.some((q) => Math.abs(q.y - ly) < 13 && lx0 < q.x1 && lx0 + wide > q.x0)) ly = py + 16;
    placed.push({ y: ly, x0: lx0, x1: lx0 + wide });
    ctx.fillText(label, lx0, ly);
  }
  /* legend, BELOW the plot: one horizontal row, centred, sized from the text it actually contains */
  if (legendRows && drawable.length > 1) {
    ctx.textAlign = "left"; ctx.textBaseline = "middle";
    const measure = (t) => (ctx.measureText ? ctx.measureText(t).width : t.length * 6);
    const SWATCH = 14, GAP = 6, ITEM_GAP = 18;
    const total = drawable.reduce((a, s) => a + SWATCH + GAP + measure(s.label) + ITEM_GAP, -ITEM_GAP);
    let lx = Math.max(padL, padL + (W - padL - padR - total) / 2);
    const ly = H - 6;
    for (const s of drawable) {
      ctx.fillStyle = s.color;
      ctx.fillRect(lx, ly - 1, SWATCH, 3);
      ctx.fillStyle = fg;
      ctx.fillText(s.label, lx + SWATCH + GAP, ly);
      lx += SWATCH + GAP + measure(s.label) + ITEM_GAP;
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
    // Through the CSS size, never the backing store: hidpi() enlarged the latter by the device pixel
    // ratio, so canvas.width/r.width would misplace the readout on a retina display.
    const cssW = Number(canvas.dataset.cssw) || canvas.width;
    const cssH = Number(canvas.dataset.cssh) || canvas.height;
    const mx = (ev.clientX - r.left) * (cssW / r.width);
    const my = (ev.clientY - r.top) * (cssH / r.height);
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
    // fmtY, not fmtInt: must match the peak marker's formatter (fmtRate for rate, fmtInt for p99) or
    // the same point reads two different numbers between hover and label.
    const fmtY = opts.fmtY || fmtInt;
    ctx.fillText(`${best.s.label}  conc ${fmtInt(best.p.x)}: ${fmtY(best.p.y)} ${opts.unit || ""}`, geo.padL + 6, 2);
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
  // Published peak on the RPS curve, at (peak.conc, peak.rps) - by construction one of the probed
  // sweep points.
  const rps = usable.map((s) => ({ label: s.label, color: s.color,
    points: s.sweep.map((p) => ({ x: p.conc, y: p.rps })),
    mark: s.peak && s.peak.rps > 0 && s.peak.conc != null
      ? { x: s.peak.conc, y: s.peak.rps, label: `${fmtRate(s.peak.rps)} @ c=${fmtInt(s.peak.conc)}` } : null }));
  const p99 = usable.map((s) => ({ label: s.label, color: s.color, points: s.sweep.map((p) => ({ x: p.conc, y: p.p99_us })) }));
  // Both charts share one concurrency domain so they stack and align vertically.
  const allX = [...rps, ...p99].flatMap((s) => s.points.map((p) => p.x));
  const xDomain = allX.length ? [Math.min(...allX), Math.max(...allX)] : null;
  const o1 = { yLabel: "RPS", unit: "rps", fmtY: fmtRate, xDomain, ...theme };
  const o2 = { yLabel: "p99 (µs)", unit: "µs p99", fmtY: fmtInt, xDomain, ...theme };
  drawSweep(c1, rps, o1); attachSweepHover(c1, rps, o1);
  drawSweep(c2, p99, o2); attachSweepHover(c2, p99, o2);
}

function chartTheme() {
  if (NODE) return {};
  const cs = getComputedStyle(document.documentElement);
  return {
    fg: cs.getPropertyValue("--fg-dim").trim() || "#9aa4b2",
    // Data labels are not axis chrome: `--fg-dim` is for gridlines/ticks; content gets `--fg`.
    ink: cs.getPropertyValue("--fg").trim() || "#e6edf3",
    grid: cs.getPropertyValue("--grid").trim() || "rgba(154,164,178,.18)",
  };
}

// Theme switcher: persist the choice, flip data-theme on <html>, and re-render so canvas charts
// re-read the palette. Initial data-theme is set by the inline <head> script before first paint.
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
// Per-tab caption states in one line which path the tab's numbers are. Short, one-idea-per-line.
// Streaming may be matrix-sourced or stream-fallback (standalone suite); the caption must not claim
// "the one 6x6 run" when it's actually fallback.
function streamingProvenance(data) {
  const kinds = (data && data.gateways || []).map((g) => g.streaming && g.streaming.source && g.streaming.source.kind).filter(Boolean);
  if (!kinds.length) return { all: null };
  const allMatrix = kinds.every((k) => k === "matrix");
  const allFallback = kinds.every((k) => k !== "matrix");
  return { all: allMatrix ? "matrix" : allFallback ? "fallback" : "mixed" };
}
// Lead line for a chooser caption: latency+throughput is always the 6x6 matrix; streaming depends on
// provenance (matrix/fallback/mixed).
// Age of the newest record this lane shows across the board (audit #8). "" if no gateway stamps it.
function laneAgeSummary(data, lane, now = Date.now()) {
  const stamps = ((data && data.gateways) || [])
    .map((g) => g.lane_measured_at && g.lane_measured_at[lane]).filter(Boolean)
    .map((a) => Date.parse(a)).filter(Number.isFinite);
  if (!stamps.length) return "";
  const age = fmtAge(new Date(Math.max(...stamps)).toISOString(), now);
  return age ? `, measured ${age}` : "";
}
function chooserLead(view, data) {
  // Frontier's readings ARE the latency-bounded throughput, so no separate latency column to promise.
  if (view === "frontier") return "Per-cell throughput from the one 6x6 run; the cell chooser picks which cell every row reads.";
  if (view !== "streaming") return "Per-cell latency + throughput from the one 6x6 run.";
  const prov = streamingProvenance(data).all;
  if (prov === "matrix") return "Per-cell streaming from the one 6x6 run.";
  if (prov === "mixed") return "Streaming: some gateways from the 6x6 run, some from the standalone stream suite (per-row provenance in the drawer).";
  // Age by the stream suite's own stamp, not the matrix's, since this tab isn't matrix-sourced.
  return `Streaming from the standalone stream suite, not the 6x6 matrix${laneAgeSummary(data, "stream")}; each row's pill names the passthrough it ran on.`;
}
/* A tab's prose split as `{ lead, notes }`: lead is 1-2 plain sentences above the table saying what
   it shows; notes (how to read it, what a marker means) render below as reference material, beside
   the engine's definitions. Nothing deleted, only moved - notes carry findings, not decoration.
   `captionText(c)` flattens both back into one string for tests that don't care which half says it. */
function captionText(c) { return [...c.lead, ...c.notes].join(" "); }
function chooserCaption(view, st, data) {
  const lead = chooserLead(view, data);
  // HELD_REFERENCE belongs only to the tab that renders the shape column.
  const extra = view === "performance" ? [HELD_REFERENCE] : [];
  if (st.mode === "peak")
    return { lead: [lead,
      // Not "best"/"peak": bestCell is representative, not a maximum. See CHOOSER_MODES.
      "Each gateway on its own representative same-dialect diagonal; the pill shows which dialect."],
      notes: [...extra, "The cell is this gateway's OpenAI passthrough where it serves one, otherwise its lowest-added-latency diagonal. It is not its highest-throughput cell: the chooser never reads a throughput number, so changing the tail-latency bound cannot change which cell a row shows.",
        "Everyone appears. Pick Same for one shared dialect, or Custom for any ingress→egress cell."] };
  if (st.mode === "same") {
    const d = MATRIX_LABELS[st.sameDialect] || st.sameDialect;
    return { lead: [lead, `Every gateway on the ${d}→${d} diagonal (pure forwarding, no translation).`],
      notes: [...extra, "A gateway that does not serve this dialect reads n/a and sinks to the bottom."] };
  }
  const inL = MATRIX_LABELS[st.xlateIn] || st.xlateIn, outL = MATRIX_LABELS[st.xlateOut] || st.xlateOut;
  return st.xlateIn === st.xlateOut
    ? { lead: [lead, `Every gateway on the ${inL}→${outL} cell: same dialect, so this is passthrough (no translation).`],
        notes: [...extra, "A gateway that does not serve this cell reads n/a."] }
    : { lead: [lead, `Every gateway on the ${inL}→${outL} cell: client speaks ${inL}, upstream speaks ${outL}, the gateway translates both ways.`],
        notes: [...extra, "Every row is the identical cell, so it is apples-to-apples; a gateway that does not serve it reads n/a."] };
}
// AUDIT #14: window durations render from the data, never hard-coded.
function memoryCaption(data = state.data, st = state) {
  const w = boardMemWindows(data);
  const I = memWindowLabel(w.idle), R = memWindowLabel(w.recovery);
  if (!hasPerCellMemory(data)) {
    return {
      lead: [`An identical fixed load on each gateway's PEAK cell, measured on a fresh cold-restarted process (${I} idle → load → ${R} recovery).`],
      notes: [
        `Same load recipe for every gateway, so it is apples-to-apples; only the cell differs (shown under Tested on).`,
        `Idle: cold-start RSS (median, no load). Peak: max RSS under the fixed load. Recovered @${R}: RSS ${R} after the load stops: does it release?`,
        "This run measured one cell per gateway, chosen by throughput, so there is no cell to choose between here. Lower is better on every column; a gateway with no served cell reads n/a.",
      ] };
  }
  const mode = memoryMode(st);
  const d = MATRIX_LABELS[st.sameDialect] || st.sameDialect;
  const inL = MATRIX_LABELS[st.xlateIn] || st.xlateIn, outL = MATRIX_LABELS[st.xlateOut] || st.xlateOut;
  const pick = {
    min: "Each gateway on its OWN lowest steady-state cell. Selected on memory and reported as memory, so it is a real minimum - but of a set whose size differs per gateway, which the row states next to the cell.",
    max: "Each gateway on its OWN highest steady-state cell. A real maximum, over a candidate set whose size differs per gateway (stated next to the cell). Min flatters a broad gateway, Max penalises it; both are offered so neither reads as the answer.",
    same: `Every gateway on the ${d}→${d} identity cell: the same work on every row, so this is the like-for-like comparison.`,
    custom: st.xlateIn === st.xlateOut
      ? `Every gateway on the ${inL}→${outL} cell: same dialect, so this is passthrough.`
      : `Every gateway on the ${inL}→${outL} cell: client speaks ${inL}, upstream speaks ${outL}, the gateway translates both ways.`,
  }[mode];
  const flagged = ((data && data.gateways) || []).filter(neverPlateaued);
  // States the count and where to read the rate, not a verdict.
  const never = flagged.length
    ? ` ${fmtInt(flagged.length)} gateway${flagged.length === 1 ? "" : "s"} reached no steady state on any cell, so no steady-state number is published for ${flagged.length === 1 ? "it" : "them"} and the Growth column carries the rate instead: their memory under load is bounded by how long the load ran, not by the gateway.`
    : "";
  return {
    lead: [
      "Every cell gets its own cold-started process and its own load, run until RSS stops climbing rather than for a fixed time.",
      pick,
    ],
    notes: [
      "Nothing is averaged across cells; the chooser picks which cell each row shows.",
      // Window lengths are board facts stated once here rather than repeated per row.
      `Every RSS curve is one process's whole lifetime, left to right: ${memWindowLabel(w.idle)} at rest before the first request, then the load run to steady state, then ${R} of recovery after it stops. Those windows are the same for every gateway. The break in the middle of each curve is the time axis changing scale between them - the at-rest window is far shorter than the load run, and is drawn wider than its duration earns so its shape stays legible. Hover a curve for its figures, or click the row for the two windows full size and separated.`,
      `Idle is sampled cold, before the first request, so no cell is involved and it is valid in every mode - which is why the Idle column is the median across ALL of a gateway's cells while its curve is the chosen cell's own window; the two can differ. Growth is around zero once a gateway has settled, and is the rate RSS was still moving at when no steady state was reached. Recovered @${R}: RSS ${R} after the load stops, which on a gateway still releasing is not the last figure its curve reaches.${never}`,
      "Lower is better on every column. A gateway that does not serve the chosen cell reads n/a and sinks to the bottom; nothing is substituted from another cell.",
    ] };
}
// Frontier tab's prose: states the finding the tab exists for (two gateways with similar headline
// rates can be different machines). Two sentences on top; rest is reference material below (captionText).
function frontierCaption(st = state, data = state.data) {
  const sel = selectedBound(st);
  return {
    lead: [
      `How much throughput each gateway holds as you tighten the tail-latency budget: one concurrency sweep per cell, read at ${BOUND_CHOICES.map(boundLabel).join(", ")}.`,
      `Read across a row, not down a column: a flat row holds its rate even under a tight tail, and a row that climbs needs a loose tail to go fast.`,
    ],
    notes: [
      `Each reading is the most requests/sec that cell carried while 99% of requests finished under that bound and it failed no request it accepted. Published as a single number, a flat row and a climbing row look comparable, and they are not the same machine.`,
      `The ${boundLabel(sel)} column is the one the Performance tab ranks and is marked here; every other column is published on every cell too, so nothing is chosen for you. "≥" marks a reading whose sweep ran out of ladder while still qualifying: a floor, not a maximum. "tail" under a number is the tail that reading ACTUALLY produced, never the bound.`,
      `A "0 · no rung held this tail" is a MEASUREMENT: the gateway served cleanly and no concurrency it was offered kept 99% of requests under that bound. It is not missing data - a cell with no measurement at all reads "no frontier" instead - and the difference matters most on the slowest rows, where five of the six columns can be that finding.`,
      HELD_REFERENCE,
      chooserLead("frontier", data),
    ] };
}
// The tab's `{ lead, notes }` from whichever caption function owns the view. One dispatch point.
function captionFor(view, st = state, data = state.data) {
  return view === "memory" ? memoryCaption(data, st)
    : view === "frontier" ? frontierCaption(st, data)
    : chooserCaption(view, st, data);
}
// Lead above the table, everything else below it as a collapsed fold beside the engine's definitions.
function updateTableCaption(view) {
  const el = document.getElementById("table-caption");
  if (!el) return;
  const c = captionFor(view, state, state.data);
  el.innerHTML = c.lead.map((l) => esc(l)).join("<br>");
  const defs = document.getElementById("table-defs");
  // Unknown view contributes no prefixes and renders nothing rather than everything.
  if (defs) defs.innerHTML = notesFold(c.notes) + definitionsFold(DEFINITION_PREFIXES[view] || [], state.data);
}
// "How to read this table" fold. Returns "" for a view with nothing to say.
function notesFold(notes) {
  const lines = (notes || []).filter((n) => typeof n === "string" && n.trim());
  if (!lines.length) return "";
  return `<details class="metric-defs table-notes"><summary>How to read this table</summary>` +
    lines.map((l) => `<p>${esc(l)}</p>`).join("") + `</details>`;
}
/* Memory tab has no chart block: tables are tables, every chart lives on the Charts tab. Used to
   append two static PNGs that couldn't follow the cell selector above them (stale images implying
   they described the current selection). One live place for charts avoids that. */

/* ---- DECLARED COLUMN GEOMETRY -------------------------------------------------
   Column widths were auto-layout (widest rendered cell), so switching filters/modes re-measured and
   reflowed every column even though the measurement itself hadn't changed. Widths are declared here
   instead, with `table-layout: fixed` (style.css) using them directly; excess width distributes
   proportionally so the table still fills its container regardless of content.
   One table, not one per column definition, since total width has to fit a narrow desktop as a whole.
   `tested` is capped narrow (an annotation, not a reading); readings never shrink. */
const COL_W_DEFAULT = "7rem";
const COL_WIDTHS = {
  sel: "2.4rem",   // the compare checkbox; matches the sticky rule in style.css
  name: "9.5rem",  // the gateway name, sticky beside it
  tested: "5rem",  // AN ANNOTATION, capped: its longest pill truncates (with the full value on hover)
  // performance
  lat50: "7rem", lat: "7rem", rps: "9.5rem", shape: "7.5rem",
  // frontier: six reading columns. 6rem holds "44,363" and lets the sub-line under it wrap.
  f1: "6rem", f5: "6rem", f10: "6rem", f50: "6rem", f100: "6rem", fnone: "6rem",
  // streaming
  sttft50: "7rem", sttft: "7rem", sgap50: "7rem", sgap: "7rem", streams: "6.5rem", streamfps: "6.5rem",
  // memory. memcurve matches td.memcurve's own 180px: the lifecycle SVG is a fixed width and the column has
  // no reason to be wider than the picture in it.
  memidle: "7.5rem", mempeak: "7.5rem", memgrowth: "7.5rem", memrecov: "7.5rem", memcurve: "11.25rem",
};
function colWidth(c) { return COL_WIDTHS[c.id] || COL_W_DEFAULT; }
/* colgroupHtml(cols): the <colgroup> children for a column set. A <col> per column, in order - the mapping is
   positional, so a missing one would silently shift every width onto the wrong column, which is why the test
   asserts the count rather than the contents. */
function colgroupHtml(cols) {
  return cols.map((c) => `<col style="width:${colWidth(c)}">`).join("");
}
/* Table head, one row normally and two when any column declares a `group` (avoids repeating the same
   shared clause across Frontier's six reading columns). A group-less column spans both rows via
   rowspan=2; consecutive columns sharing a group string collapse into one colspan cell. The group
   cell is not sortable - sort affordance stays on each column's own header in the second row. */
function theadHtml(cols, st = state) {
  const th = (c) => {
    const sorted = st.sortCol === c.id;
    const dir = sorted ? `<span class="dir">${st.sortDesc ? " ▾" : " ▴"}</span>` : "";
    // AUDIT #14: label/title may be a function so wording depending on a tunable harness window
    // renders from the data instead of hard-coding the default.
    return `<th data-col="${c.id}" class="${sorted ? "sorted" : ""}${c.sortable === false ? " nosort" : ""}" ` +
      `${c.group ? "" : `rowspan="2" `}title="${esc(txt(c.title))}">${esc(txt(c.label))}${dir}</th>`;
  };
  if (!cols.some((c) => c.group)) {
    // No groups: one row, no rowspan (else a phantom second row pushes body rows down).
    return "<tr>" + cols.map((c) => th(c).replace(' rowspan="2"', "")).join("") + "</tr>";
  }
  let top = "", sub = "";
  for (let i = 0; i < cols.length; i += 1) {
    const c = cols[i];
    if (!c.group) { top += th(c); continue; }
    let j = i;
    while (j + 1 < cols.length && cols[j + 1].group === c.group) j += 1;
    top += `<th class="colgroup nosort" colspan="${j - i + 1}" scope="colgroup">${esc(txt(c.group))}</th>`;
    for (let k = i; k <= j; k += 1) sub += th(cols[k]);
    i = j;
  }
  return `<tr>${top}</tr><tr class="subhead">${sub}</tr>`;
}
function renderTable() {
  const { data } = state;
  const table = document.querySelector("#results-table");
  const thead = table.querySelector("thead");
  const tbody = table.querySelector("tbody");

  // Which tab's columns to render. matrix/method have no table, so fall back to performance
  // (the section is hidden anyway) and never mutate the sort while off a table tab.
  const view = TABLE_VIEWS.has(state.view) ? state.view : "performance";
  const cols = columnsFor(view);
  // The Tested-on column is IDENTICAL in every mode (Peak / Same / Custom): the column set never changes
  // between modes, only WHICH cell each row reads. It renders from the chosen cell's own provenance stamp
  // (chooserDialects + source), so Own cell names each gateway's own representative dialect (varies per row), Same names
  // the chosen dialect on every row, and Custom names the chosen ingress→egress — one column, one renderer.
  // Snap the sort onto this tab if the current column does not belong to it (e.g. after switching
  // tabs, or a cross-tab sort id arrived from a shared URL).
  if (TABLE_VIEWS.has(state.view) && !cols.some((c) => c.id === state.sortCol && c.sortable !== false)) {
    state.sortCol = VIEW_SORT[view] || cols[cols.length - 1].id;
    const dc = cols.find((c) => c.id === state.sortCol);
    state.sortDesc = dc ? dc.desc !== false : true;
  }
  updateTableCaption(view);

  thead.innerHTML = theadHtml(cols, state);
  // Declared geometry, re-stated when the column set changes. <colgroup> must be the table's first
  // child so it's created once and refilled. Without it, table-layout:fixed divides width equally.
  const cg = table.querySelector("colgroup") ||
    table.insertBefore(document.createElement("colgroup"), table.firstChild);
  cg.innerHTML = colgroupHtml(cols);

  let rows = applyFilters(data.gateways, state);
  const count = document.getElementById("row-count");
  if (count) count.textContent = `${rows.length} of ${data.gateways.length}`;

  const col = cols.find((c) => c.id === state.sortCol) || cols.find((c) => c.id === VIEW_SORT[view]) || cols[3];
  const tiebreak = cols.find((c) => c.id === VIEW_TIEBREAK[view]);
  rows = rows.slice().sort(rowComparator(col, state.sortDesc, tiebreak));

  // Rows the sorted column cannot actually separate (closer than rig resolution): their order
  // records which box they landed on, not a finding. Only the sorted column is considered.
  const tiedWithPrev = tiedRuns(rows, col, state, rigResolutionPct(state.data));
  tbody.innerHTML = rows.map((g) =>
    `<tr data-gw="${esc(g.key)}"${tiedWithPrev.has(g.key) ? ' class="tied-above" title="Within the rig\u2019s own measurement resolution of the row above - this ordering is not a finding"' : ""}>` + cols.map((c) => {
      const sc = c.id === state.sortCol ? " sorted-col" : "";
      if (c.render) {
        // render columns emit their own <td>; tint the sorted one by injecting the class.
        return sc ? c.render(g, state).replace("<td", `<td class="sorted-col"`).replace('class="sorted-col" class="', 'class="sorted-col ') : c.render(g, state);
      }
      return metricTd(c.get(g), sc);
    }).join("") + "</tr>"
  ).join("");
  // Empty-state line: filters that clear the table must never render as a bare header over nothing.
  // Rows are unfiltered by the chosen cell (a gateway not serving the pinned pair reads n/a instead
  // of disappearing), so this can only happen via search/capability filters.
  if (!rows.length) {
    tbody.innerHTML = `<tr><td colspan="${cols.length}" class="na">No gateways match the current filters.</td></tr>`;
  }

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

/* ---- filter bar -------------------------------------------------------------
   Deliberately compact: search only. Class/language chip rows were retired; a stale ?cls=/?lang=
   URL param is ignored. */

// Wire the persistent inputs exactly once (renderFilters may re-run on hashchange).
function initFilterControls() {
  const search = document.getElementById("search");
  search.addEventListener("input", () => { state.q = search.value; renderTable(); syncUrl(false); });
  // Capability toggles are now implicit per tab; DOM checkboxes retired, state/URL param kept for back-compat.
  for (const [key, name] of CAPS) {
    const el = document.getElementById(`f-${name}`);
    if (el) el.addEventListener("change", () => { state[key] = el.checked; renderTable(); syncUrl(true); });
  }
  // Cell chooser dialect dropdowns. Changing mode or dialect re-projects every row.
  const opts = MATRIX_CELLS.map((d) => `<option value="${esc(d)}">${esc(MATRIX_LABELS[d] || d)}</option>`).join("");
  const same = document.getElementById("same-dialect");
  const cin = document.getElementById("cell-in");
  const cout = document.getElementById("cell-out");
  if (same) same.innerHTML = opts;
  if (cin) cin.innerHTML = opts;
  if (cout) cout.innerHTML = opts;
  // Mode buttons are re-rendered per view (different tabs offer different mode sets), so one
  // delegated listener replaces per-button ones that would go stale after a re-render.
  const seg = document.getElementById("mode-seg");
  if (seg) seg.addEventListener("click", (ev) => {
    const btn = ev.target.closest(".seg-btn");
    if (!btn || !modesFor(state.view).has(btn.dataset.mode)) return;
    state.mode = btn.dataset.mode;
    renderFilters(); renderTable(); syncUrl(true);
  });
  // Same delegation reason as the mode buttons.
  const bseg = document.getElementById("bound-seg");
  if (bseg) bseg.addEventListener("click", (ev) => {
    const btn = ev.target.closest(".seg-btn");
    if (!btn) return;
    selectBound(btn.dataset.bound === "none" ? null : Number(btn.dataset.bound));
  });
  // Re-renders filters too since the note under it states which claim ("own peak" vs "same rung") is on screen.
  const csel = document.getElementById("conc-select");
  if (csel) csel.addEventListener("change", () => {
    const v = Number(csel.value);
    state.conc = csel.value === "" || !Number.isFinite(v) ? null : v;
    renderFilters();
    renderTable();
    syncUrl(true);
  });
  const onSame = () => { state.sameDialect = same.value; renderTable(); syncUrl(true); };
  const onCustom = () => { state.xlateIn = cin.value; state.xlateOut = cout.value; renderTable(); syncUrl(true); };
  if (same) same.addEventListener("change", onSame);
  if (cin) cin.addEventListener("change", onCustom);
  if (cout) cout.addEventListener("change", onCustom);
}

/* The reader picked a tail-latency bound. Re-ranking in front of the reader is the point: the
   frontier's claim is that a gateway's position depends on the tail you accept. Frontier's ranking
   is per-bound column, so the sort follows the selection unless the reader sorted by something else. */
function selectBound(ms) {
  const prev = selectedBound(state);
  state.bound = ms;
  if (state.view === "frontier" && state.sortCol === boundColId(prev)) state.sortCol = boundColId(ms);
  // State change above is the whole decision and testable on its own; DOM calls below are skipped
  // under node so the suite can drive the selector without a document.
  if (NODE) return;
  renderFilters(); renderTable(); syncUrl(true);
}
/* Paint bound buttons from the one published list, mark selection, state in words what the selected
   column means (via boundClause(), same function every header/tooltip uses).
   Says "the cell the chooser picked", never "the most each gateway carried" - the reading is the top
   qualifying rung of one cell's sweep, not a maximum across a gateway's cells. */
function renderBoundChooser() {
  const seg = document.getElementById("bound-seg");
  if (!seg) return;
  const sel = selectedBound(state);
  seg.innerHTML = BOUND_CHOICES.map((b) =>
    `<button type="button" class="seg-btn${(b == null ? sel == null : b === sel) ? " active" : ""}" ` +
    `data-bound="${b == null ? "none" : b}" role="tab" ` +
    `title="${esc(`Rank the board on the req/s each gateway's chosen cell carried ${boundClause(b)}`)}">${esc(boundLabel(b))}</button>`).join("");
  const note = document.getElementById("bound-note");
  if (note) note.textContent = `showing the req/s each gateway's chosen cell carried ${boundClause(sel)}`;
}
/* Concurrency control. "Best" (each gateway's own peak) is the default so the control doesn't quietly
   answer a question the reader hasn't asked. Options are the rungs the run actually drove. Hidden
   entirely when the board carries no rungs. */
function renderConcChooser() {
  const wrap = document.getElementById("conc-chooser");
  const sel = document.getElementById("conc-select");
  if (!wrap || !sel) return;
  const choices = concChoices(state.data);
  const offer = CHOOSER_VIEWS.has(state.view) && state.view !== "memory" && choices.length > 0;
  wrap.classList.toggle("hidden", !offer);
  if (!offer) return;
  const cur = selectedConc(state);
  sel.innerHTML = [
    `<option value=""${cur == null ? " selected" : ""}>Best (each gateway's peak)</option>`,
    ...choices.map((c) =>
      `<option value="${c}"${c === cur ? " selected" : ""}>${esc(fmtInt(c))} concurrent</option>`),
  ].join("");
  const note = document.getElementById("conc-note");
  if (note)
    note.textContent = cur == null
      ? "each gateway at the concurrency where its own throughput peaked"
      : `every gateway at the same rung - ${fmtInt(cur)} concurrent requests in flight`;
}
function renderFilters() {
  document.getElementById("search").value = state.q;
  renderBoundChooser();
  renderConcChooser();
  for (const [, name] of CAPS) { const el = document.getElementById(`f-${name}`); if (el) el.checked = state[CAPS.find(([, n]) => n === name)[0]]; }
  // Cell chooser: paint buttons this view offers, mark active, show only the dropdown(s) the mode needs.
  // `peak` simply isn't rendered on the memory tab.
  const seg = document.getElementById("mode-seg");
  const mode = state.view === "memory" ? memoryMode(state) : state.mode;
  if (seg) seg.innerHTML = [...modesFor(state.view)].map((m) =>
    `<button type="button" class="seg-btn${m === mode ? " active" : ""}" data-mode="${esc(m)}" role="tab" title="${esc(MODE_TIPS[m] || "")}">${esc(MODE_LABELS[m] || m)}</button>`).join("");
  const sameWrap = document.getElementById("chooser-same");
  const customWrap = document.getElementById("chooser-custom");
  if (sameWrap) sameWrap.classList.toggle("hidden", mode !== "same");
  if (customWrap) customWrap.classList.toggle("hidden", mode !== "custom");
  const same = document.getElementById("same-dialect"); if (same) same.value = state.sameDialect;
  const cin = document.getElementById("cell-in"); if (cin) cin.value = state.xlateIn;
  const cout = document.getElementById("cell-out"); if (cout) cout.value = state.xlateOut;
}

/* ---- per-gateway drawer ----------------------------------------------------- */
const MATRIX_CELLS = ["openai", "openai-responses", "anthropic", "gemini", "cohere", "bedrock"];
const MATRIX_LABELS = {
  openai: "OpenAI", "openai-responses": "OpenAI Responses", anthropic: "Anthropic",
  gemini: "Gemini", cohere: "Cohere", bedrock: "Bedrock Converse",
};
/* Same-dialect cell for each protocol (openai>openai, etc), what the drawer's Protocol matrix shows.
   Returns null when the gateway carries no per-cell data, keeping "not measured" distinguishable from
   "measured, not served". Falls back to the legacy flat `cells` map for older boards; an empty object
   counts as nothing found. */
function matrixDiagonal(g) {
  const m = g && g.matrix;
  if (!m) return null;
  const out = {};
  const ups = m.upstreams || {};
  for (const d of MATRIX_CELLS) {
    const cell = (ups[d] && ups[d].cells && ups[d].cells[d]) || (m.cells && m.cells[d]);
    if (cell) out[d] = cell;
  }
  return Object.keys(out).length ? out : null;
}

/* A non-green cell is one of several different things, and a neutral board must not conflate them:
     served:"not_verified"     - harness couldn't fairly test it (never a red fail)
     served:"untestable"       - gateway pins the real cloud host, our mock is unreachable (rig limit)
     served:"not_configured"   - probe-first: probed, round trip wasn't a correct translation (grey)
     served:false (wrong_answer) - legacy red: gateway answered wrongly
   The prose-note heuristic below is only a fallback for results predating these machine-readable fields. */
const isHarnessGap = (cell) => {
  if (cell.served === "not_verified") return true;
  if (cell.reason) return false; // reason present and not not_verified: the verdict is explicit
  const note = (cell.verdict_note || "").toLowerCase();
  return cell.status === "000" || (note.includes("never served") && note.includes("warm-up"));
};
const cellState = (cell) =>
  cell.served === true ? ["served", "served"]
    : cell.served === "unprobed_auth" ? ["unprobed", "unprobed (auth)"]
      // Probe-first (matrix v3): every cell probed; a failed probe is "not configured", grey never red.
      : cell.served === "not_configured" ? ["notconf", "not configured"]
        // Legacy (pre-probe-first): grey by the drafted capability grid, not by a probe.
        : cell.served === "not_configurable" ? ["notconf", "not declared"]
          : cell.served === "untestable" ? ["untestable", "untestable (mock limit)"]
            // Real pairing, gateway declined the attempt - a genuine defect, so red not grey.
            : cell.served === "failed" ? ["failed", "not served"]
              : isHarnessGap(cell) ? ["unverified", "not verified"]
              : ["failed", "not served"];

function laneStamp(j) {
  const bits = [];
  // build/measured_at travel inside j.source on a projected cell; g.matrix carries them top level.
  // Prefer the stamp, fall back to top-level.
  const build = (j.source && j.source.build) ?? j.build;
  const at = (j.source && j.source.measured_at) ?? j.measured_at;
  if (build) bits.push(build);
  if (at) bits.push(at);
  return bits.length ? `<div class="stamp muted">${esc(bits.join(" · "))}</div>` : "";
}

/* Smallest band the idle sparkline's y axis will ever cover, as a fraction of the published idle
   figure - avoids drawing sampling noise as an event when the series barely moves. 2% is calibrated
   against the field: windows that genuinely didn't move draw flat; the few that did (a late step, an
   early ramp) fill their frame. The stamp always states the MiB regardless. */
const IDLE_AXIS_MIN_SPAN = 0.02;
/* What the idle window did, as a phrase describing its shape. Replaces "X MiB over N s at rest"
   wording, which misread as drift accumulating over the window when it was really one late/early
   step - the words must say where the movement was, not just how big.
   Geometry, not a verdict: it reads the plotted series, doesn't judge leaking vs healthy (the engine
   owns that via idle_shape). Never "X over N s" - that pairing reads as a rate. */
function idleShapeNote(pts, span, floorSpan) {
  const n = pts.length;
  // Below the axis floor is the expected case and renders flat. RSS is sampled in whole pages, so
  // "flat to within X" states the resolution rather than claiming an impossible zero.
  if (!(span > 0)) return "no movement at all";
  if (span < floorSpan) return `flat to within ${fmt2(span)} MiB`;
  // Where the movement sits in time is the distinction, not the size of one biggest single step
  // (which mis-scores a multi-sample step as gradual). So: total variation, then the shortest
  // contiguous run of samples accounting for most of it.
  const d = [];
  let tv = 0;
  for (let i = 1; i < n; i += 1) { const v = Math.abs(pts[i].rss_mib - pts[i - 1].rss_mib); d.push(v); tv += v; }
  if (!(tv > 0)) return `flat to within ${fmt2(span)} MiB`;
  const want = tv * 0.8;
  let a = 0, b = d.length - 1, lo = 0, run = 0;
  for (let hi = 0; hi < d.length; hi += 1) {
    run += d[hi];
    while (run - d[lo] >= want) { run -= d[lo]; lo += 1; }
    if (run >= want && (hi - lo) < (b - a)) { a = lo; b = hi; }
  }
  const t0 = pts[a].t_s, t1 = pts[b + 1].t_s, tEnd = pts[n - 1].t_s, tSpan = (tEnd - pts[0].t_s) || 1;
  const net = pts[b + 1].rss_mib - pts[a].rss_mib;
  const dir = net > 0 ? "up" : "down";
  // Extent of the concentrated run (its own min to max), not the net across its endpoints, since the
  // run's boundaries can land mid-climb.
  const runVals = pts.slice(a, b + 2).map((q) => q.rss_mib);
  const mag = fmt2(Math.max(...runVals) - Math.min(...runVals) || span);
  // "then held" is a claim about the rest of the window, made only when the rest earns it.
  const restVar = d.reduce((acc, v, i) => (i < a || i > b ? acc + v : acc), 0);
  const held = restVar < floorSpan ? ", then held" : "";
  // 20% of the window is the line between a step and drift.
  if ((t1 - t0) / tSpan <= 0.2) {
    if (t0 <= tSpan * 0.15) return `moved ${mag} MiB ${dir} inside the first ${fmtInt(Math.max(1, t1))} s${held}`;
    if (t1 >= tSpan * 0.7) {
      // "0 s from the end" is what a sub-second tail rounds to, and it reads as a missing number.
      const tail = Math.round(tEnd - t0);
      return `flat, then stepped ${mag} MiB ${dir} ${tail >= 1 ? `${fmtInt(tail)} s from the end` : "right at the end"}`;
    }
    return `flat, then stepped ${mag} MiB ${dir} ${fmtInt(t0)} s in`;
  }
  return `moved ${fmt2(span)} MiB gradually across the window`;
}
/* End of the load caption, naming the window each figure belongs to. Fixes a collision where the
   caption's last-sample figure and the "Recovered @30s" column figure could legitimately differ (a
   gateway still releasing after the recovery mark) and read as inconsistent data. States both points
   in time order when they differ, collapses to one figure when they agree. */
function recoveryTail(lastMib, opts = {}) {
  const at = opts.recoveredAt, w = opts.recoveryWindowS;
  const end = fmt1(lastMib);
  if (at == null || w == null) return `${end} MiB at the last sample`;
  const marked = fmt1(at);
  if (marked === end) return `${marked} MiB at the ${memWindowLabel(w)} recovery mark, and still there at the end`;
  return `${marked} MiB at the ${memWindowLabel(w)} recovery mark, ${at > lastMib ? "still falling" : "risen again"} to ${end} MiB by the last sample`;
}
/* Did it give the memory back - drawn, not asserted, so "released nothing" and "released most of it"
   don't render with identical emphasis. No new metric: a tick between two already-plotted levels (peak
   and final sample) at the x of the final sample. Nothing released, nothing drawn. Title states the fall
   and what it is a fall out of, both from figures the row already
   shows. Returns "" on an at-rest window: nothing is released before any load. */
function releaseMark(g) {
  if (!g) return "";
  const drop = g.peak - g.end;
  // Below 0.05 MiB is under what fmt1 can print, so a mark there would be a line with no legible cause.
  if (!(drop > 0.05)) return "";
  const gained = typeof g.idle === "number" && g.idle > 0 ? g.peak - g.idle : null;
  const of = gained && gained > 0 ? ` of the ${fmt1(gained)} MiB it gained` : "";
  return `<line class="rss-release" x1="${g.x.toFixed(1)}" y1="${g.yPeak.toFixed(1)}" x2="${g.x.toFixed(1)}" y2="${g.yEnd.toFixed(1)}" ` +
    `stroke="currentColor" stroke-width="3" stroke-opacity="0.35" stroke-linecap="round">` +
    `<title>released ${fmt1(drop)} MiB${of} between its peak and the last sample</title></line>`;
}
/* Compact inline-SVG recovery curve (idle -> peak -> recovery) from a memory record's rss_series.
   Returns "" when absent or <2 points - never a fabricated flat line. Y-axis runs idle to peak; a
   dot marks the last (recovered) sample. */
// loadEndS: the second load stopped and recovery began (record's `load_s`), drawn as a dotted rule
// so the curve shows which part is under load vs at rest.
// `opts` carries what the load panel needs to avoid colliding with the Recovered column: the
// scalar that column publishes, and the window it was read at (see recoveryTail).
function rssSparkline(series, loadEndS = null, idleMib = null, kind = "load", opts = {}) {
  if (!Array.isArray(series) || series.length < 2) return "";
  const pts = series
    .filter((p) => p && typeof p.t_s === "number" && typeof p.rss_mib === "number")
    .sort((a, b) => a.t_s - b.t_s);
  if (pts.length < 2) return "";
  const W = 260, H = 56, PAD = 3;
  const ts = pts.map((p) => p.t_s), ys = pts.map((p) => p.rss_mib);
  const t0 = ts[0], t1 = ts[ts.length - 1], tspan = (t1 - t0) || 1;
  // Axis runs 0 -> at least twice idle, always far enough to show the whole curve. Two failure modes
  // to avoid: auto-scaling to each curve's own range exaggerates small noise into a cliff; a hard cap
  // at twice idle clips a genuine climb into a flat line pinned to the ceiling. So twice idle is a
  // floor on the axis, never a ceiling - nothing is ever clipped.
  const dataMin = Math.min(...ys), dataMax = Math.max(...ys);
  const anchored = typeof idleMib === "number" && idleMib > 0;
  const idleWin = kind === "idle";
  /* Idle window gets its own axis, scaled to its own range with a floor on the span. The load axis's
     0->2x-idle frame answers no question for idle series (nearly flat, would all look identical), but
     a bare auto-scale would exaggerate one-page RSS jitter into a false cliff. So the span floors at
     IDLE_AXIS_MIN_SPAN of the idle figure: below it, movement draws to scale inside a stable frame;
     above it, the frame grows to fit, never clipping. */
  const ymin = idleWin ? dataMin : anchored ? 0 : dataMin;
  const ymax = idleWin ? dataMax : anchored ? Math.max(idleMib * 2, dataMax) : dataMax;
  const floorSpan = idleWin && anchored ? idleMib * IDLE_AXIS_MIN_SPAN : 0;
  // Centre the floored frame on the data so a flat line sits mid-panel rather than pinned to an edge.
  const pad = Math.max(0, (floorSpan - (ymax - ymin)) / 2);
  const yspan = (ymax + pad - (ymin - pad)) || 1;
  const x = (t) => PAD + ((t - t0) / tspan) * (W - 2 * PAD);
  const y = (v) => PAD + (1 - Math.min(Math.max((v - (ymin - pad)) / yspan, 0), 1)) * (H - 2 * PAD);
  const path = pts.map((p, i) => `${i ? "L" : "M"}${x(p.t_s).toFixed(1)},${y(p.rss_mib).toFixed(1)}`).join("");
  const last = pts[pts.length - 1];
  // Only draw the mark when it falls INSIDE the plotted span. A load_s at or past the last sample
  // means the recovery window produced no readings, and a rule on the axis edge would claim a
  // boundary the curve cannot show.
  const marks = (typeof loadEndS === "number" && loadEndS > t0 && loadEndS < t1)
    ? `<line x1="${x(loadEndS).toFixed(1)}" y1="${PAD}" x2="${x(loadEndS).toFixed(1)}" y2="${H - PAD}" ` +
      `stroke="currentColor" stroke-opacity="0.45" stroke-width="1" stroke-dasharray="2 2">` +
      `<title>load stopped at ${fmtInt(loadEndS)} s; everything right of this line is the gateway at rest</title></line>`
    : "";
  const restNote = marks ? `, load stopped at ${fmtInt(loadEndS)} s` : "";
  // Idle stamp differs from the load stamp ("peak X -> recovered Y"): the idle window is at rest,
  // sampled before load, so nothing was recovered and there's no peak-under-load. States the median
  // and the span the samples spanned, in MiB, since a floored axis hides the magnitude visually.
  const span = dataMax - dataMin;
  // A range whose ends round to the same figure isn't a range: say "held" instead of a fake span.
  const flatToTenth = fmt1(dataMin) === fmt1(dataMax);
  // "this cell: median X", never a bare "median X": the Idle RSS column beside this is the median
  // across the gateway's cells, while this is the selected cell's own window - they can differ.
  // Parenthetical describes shape, not a rate (idleShapeNote) - pairing magnitude with window length
  // reads as drift, which is not always what happened.
  const stamp = idleWin
    ? `${anchored ? `this cell: median ${fmt1(idleMib)} MiB · ` : ""}` +
      (flatToTenth ? `held ${fmt1(dataMax)} MiB` : `spanned ${fmt1(dataMin)}–${fmt1(dataMax)} MiB`) +
      ` at rest: ${idleShapeNote(pts, span, floorSpan || dataMax * IDLE_AXIS_MIN_SPAN)}`
    : `peak ${fmt1(dataMax)} → ${recoveryTail(last.rss_mib, opts)}`;
  const aria = idleWin
    ? `RSS at rest over ${fmtInt(tspan)} s, ${fmt1(dataMin)} to ${fmt1(dataMax)} MiB, published median ${anchored ? fmt1(idleMib) : "unknown"} MiB`
    : `RSS curve from zero, idle ${anchored ? fmt1(idleMib) : "unknown"} MiB, peak ${fmt1(dataMax)} MiB over ${fmtInt(tspan)} s${restNote}`;
  return `<div class="rss-spark"><svg viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" role="img" ` +
    `aria-label="${esc(aria)}">` +
    // Zero baseline, load axis only: it starts at 0. The idle axis is a narrow band above zero, so a
    // bottom-edge rule there would claim a zero the frame doesn't contain.
    (idleWin ? "" : `<polyline points="${x(t0).toFixed(1)},${(H - PAD).toFixed(1)} ${x(t1).toFixed(1)},${(H - PAD).toFixed(1)}" ` +
      `fill="none" stroke="currentColor" stroke-opacity="0.15" stroke-width="1"/>`) +
    // Idle level, drawn so "how far above idle" is measurable rather than inferred. On the idle panel
    // this rule is the published median itself, so the curve and the column visibly agree.
    (anchored
      ? `<line x1="${PAD}" y1="${y(idleMib).toFixed(1)}" x2="${(W - PAD).toFixed(1)}" y2="${y(idleMib).toFixed(1)}" ` +
        `stroke="currentColor" stroke-opacity="0.5" stroke-width="1" stroke-dasharray="2 2">` +
        `<title>${idleWin ? `median ${fmt1(idleMib)} MiB - the published idle figure for this cell` : `idle ${fmt1(idleMib)} MiB`}</title></line>`
      : "") +
    marks +
    `<path d="${path}" fill="none" stroke="currentColor" stroke-width="1.5"/>` +
    releaseMark(idleWin ? null : { x: x(last.t_s), yPeak: y(dataMax), yEnd: y(last.rss_mib), peak: dataMax, end: last.rss_mib, idle: idleMib }) +
    `<circle cx="${x(last.t_s).toFixed(1)}" cy="${y(last.rss_mib).toFixed(1)}" r="2.5" fill="currentColor"/>` +
    `</svg>` +
    `<div class="stamp muted">${esc(stamp)}</div></div>`;
}

/* Memory window as two curves: idle cost, then load cost. The two panels don't share an axis (a fix,
   not a regression) - a shared 0->2x-idle axis reads well for the load curve but flattens every idle
   curve into the same line, since idle series barely move. The idle panel frames its own floored span
   (IDLE_AXIS_MIN_SPAN); both stamp their own MiB so the panels stay comparable despite differing axes.
   Returns just the load curve when there's no idle series (bundles predating the idle window). */
/* Inline row is one lifecycle, one line. Fraction of width given to the at-rest segment. A split
   exists because idle and load+recovery are wildly different durations (~59s vs ~360s); a true shared
   time axis would make the at-rest window's whole finding invisible in under 1% of the width. So
   segments get widths their durations don't earn, and a real axis-break glyph marks the discontinuity
   rather than silently smoothing over it. */
const LIFECYCLE_IDLE_FRAC = 0.3;
/* Whole process lifetime as one inline sparkline for the table row. One shared y axis across both
   segments (the load axis, 0 -> at least 2x idle, never clipping) so the reader sees how far above
   rest work pushed the process. Trade: a small at-rest step becomes illegible inline - that's what
   the drawer's separated, floored-axis panels are for. Inline answers "what shape is this life";
   the drawer answers "what did each window do". */
function rssLifecycle(mem, opts = {}) {
  const clean = (s) => (Array.isArray(s) ? s.filter((p) => p && typeof p.t_s === "number" && typeof p.rss_mib === "number")
    .sort((a, b) => a.t_s - b.t_s) : []);
  const rest = clean(mem.idle_rss_series), load = clean(mem.rss_series);
  if (load.length < 2) return "";
  const idle = mval(mem.idle_rss_mib);
  const W = opts.w || 168, H = opts.h || 34, PAD = 3, GAP = 7;
  const all = [...rest, ...load].map((p) => p.rss_mib);
  const dataMax = Math.max(...all);
  const anchored = typeof idle === "number" && idle > 0;
  // The SAME axis rule as the load panel: twice idle is a floor, never a ceiling, so nothing is ever clipped.
  const ymax = anchored ? Math.max(idle * 2, dataMax) : dataMax;
  const y = (v) => PAD + (1 - Math.min(Math.max(v / (ymax || 1), 0), 1)) * (H - 2 * PAD);
  // Segment bands. With no at-rest series the load curve takes the whole width and there is no break to draw.
  const twoSeg = rest.length >= 2;
  const inner = W - 2 * PAD - (twoSeg ? GAP : 0);
  const wRest = twoSeg ? inner * LIFECYCLE_IDLE_FRAC : 0;
  const xRest0 = PAD, xLoad0 = PAD + wRest + (twoSeg ? GAP : 0), wLoad = inner - wRest;
  const band = (pts, x0, w) => {
    const t0 = pts[0].t_s, span = (pts[pts.length - 1].t_s - t0) || 1;
    return pts.map((p, i) => `${i ? "L" : "M"}${(x0 + ((p.t_s - t0) / span) * w).toFixed(1)},${y(p.rss_mib).toFixed(1)}`).join("");
  };
  const paths = (twoSeg ? `<path d="${band(rest, xRest0, wRest)}" fill="none" stroke="currentColor" stroke-width="1.4"/>` : "") +
    `<path d="${band(load, xLoad0, wLoad)}" fill="none" stroke="currentColor" stroke-width="1.4"/>`;
  // The axis break, drawn: two slashes in the gap, so the discontinuity is visible.
  const bx = PAD + wRest + GAP / 2;
  const brk = twoSeg
    ? `<g class="rss-break" stroke="currentColor" stroke-opacity="0.5" stroke-width="1">` +
      `<line x1="${(bx - 2).toFixed(1)}" y1="${H - PAD}" x2="${(bx + 0.5).toFixed(1)}" y2="${PAD}"/>` +
      `<line x1="${(bx + 0.5).toFixed(1)}" y1="${H - PAD}" x2="${(bx + 3).toFixed(1)}" y2="${PAD}"/>` +
      `<title>the time axis breaks here: left is the ${memWindowLabel(memWindows(mem).idle)} at-rest window, right is the load and recovery run. They are drawn at different time scales so the short window stays legible; they are not continuous.</title></g>`
    : "";
  const lastLoad = load[load.length - 1];
  const idleRule = anchored
    ? `<line x1="${PAD}" y1="${y(idle).toFixed(1)}" x2="${(W - PAD).toFixed(1)}" y2="${y(idle).toFixed(1)}" ` +
      `stroke="currentColor" stroke-opacity="0.4" stroke-width="1" stroke-dasharray="2 2"/>`
    : "";
  return `<svg class="rss-life" viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" aria-hidden="true" focusable="false">` +
    `<polyline points="${PAD},${(H - PAD).toFixed(1)} ${(W - PAD).toFixed(1)},${(H - PAD).toFixed(1)}" fill="none" stroke="currentColor" stroke-opacity="0.15" stroke-width="1"/>` +
    idleRule + brk + paths +
    releaseMark({ x: xLoad0 + wLoad, yPeak: y(Math.max(...load.map((p) => p.rss_mib))), yEnd: y(lastLoad.rss_mib),
      peak: Math.max(...load.map((p) => p.rss_mib)), end: lastLoad.rss_mib, idle }) +
    `<circle cx="${(xLoad0 + wLoad).toFixed(1)}" cy="${y(lastLoad.rss_mib).toFixed(1)}" r="2.2" fill="currentColor"/>` +
    `</svg>`;
}
/* Every figure the row's captions used to show, as one sentence. This is the accessible name of the
   row's curve control (reachability rule: nothing may become keyboard/screen-reader unreachable when
   a caption folds). Also the hover text; the drawer shows the same facts in full. */
function memCurveSummary(mem) {
  if (!mem || typeof mem !== "object") return "";
  const w = memWindows(mem), bits = [];
  const rest = Array.isArray(mem.idle_rss_series) ? mem.idle_rss_series.filter((p) => p && typeof p.rss_mib === "number") : [];
  const idle = mval(mem.idle_rss_mib);
  if (rest.length >= 2) {
    const vs = rest.map((p) => p.rss_mib), lo = Math.min(...vs), hi = Math.max(...vs);
    const pts = rest.slice().sort((a, b) => a.t_s - b.t_s);
    // A record can carry the series and no idle scalar; the median clause is simply not composed then
    // (fmt1(null) throws, and a fabricated 0 would be worse).
    const med = idle != null ? `this cell: median ${fmt1(idle)} MiB, ` : "";
    bits.push(`At rest (${memWindowLabel(w.idle)}, before any request): ${med}` +
      (fmt1(lo) === fmt1(hi) ? `held ${fmt1(hi)} MiB` : `spanned ${fmt1(lo)}–${fmt1(hi)} MiB`) +
      `, ${idleShapeNote(pts, hi - lo, (idle || hi) * IDLE_AXIS_MIN_SPAN)}.`);
  } else if (idle != null) bits.push(`At rest: this cell: median ${fmt1(idle)} MiB.`);
  const load = Array.isArray(mem.rss_series) ? mem.rss_series.filter((p) => p && typeof p.rss_mib === "number") : [];
  if (load.length >= 2) {
    const peak = Math.max(...load.map((p) => p.rss_mib));
    const last = load[load.length - 1].rss_mib;
    bits.push(`Under load: peak ${fmt1(peak)} MiB → ${recoveryTail(last, { recoveredAt: mval(mem.recovered_rss_mib), recoveryWindowS: w.recovery })}.`);
    const drop = peak - last, gained = idle != null ? peak - idle : null;
    if (drop > 0.05 && gained > 0) bits.push(`Released ${fmt1(drop)} MiB of the ${fmt1(gained)} MiB it gained.`);
    else if (gained > 0) bits.push(`Released none of the ${fmt1(gained)} MiB it gained.`);
  }
  bits.push("Click the row for the full-size separated windows.");
  return bits.join(" ");
}
function rssCurves(mem, opts = {}) {
  if (!mem || typeof mem !== "object") return "";
  // Compact is the table row: lifecycle curve alone, no prose labels. Everything the old ~350px
  // caption stack said now lives in memCurveSummary, the control's accessible name, and the drawer.
  if (opts.compact) return rssLifecycle(mem, opts);
  const idle = mval(mem.idle_rss_mib);
  // Load panel needs the Recovered column's scalar and window so its caption names the same window.
  const load = rssSparkline(mem.rss_series, mval(mem.load_s), idle, "load", {
    recoveredAt: mval(mem.recovered_rss_mib), recoveryWindowS: memWindows(mem).recovery,
  });
  const idleSeries = mem.idle_rss_series;
  if (!Array.isArray(idleSeries) || idleSeries.length < 2) return load;
  // The idle window has no load boundary to mark. kind:"idle" is passed explicitly rather than
  // inferred from `loadEndS == null`, since a load window with an absent load_s also passes null there.
  const idleCurve = rssSparkline(idleSeries, null, idle, "idle");
  if (!idleCurve) return load;
  const verdict = idleStatic(mem);
  return `<div class="rss-pair">` +
    `<div class="rss-half"><div class="rss-label muted">at rest, before any load${verdict ? ` · ${esc(verdict)}` : ""}</div>${idleCurve}</div>` +
    `<div class="rss-half"><div class="rss-label muted">load → recovery</div>${load}</div>` +
    `</div>`;
}

// What the idle window did, as a phrase. Renders the engine's own verdict; never re-derives it.
function idleStatic(mem) {
  const st = mval(mem.memory_idle_static ?? mem.idle_static);
  if (st == null) return "";
  if (st === 1) return "steady";
  const rate = mval(mem.memory_idle_growth_rate_mib_per_min ?? mem.idle_growth_rate_mib_per_min);
  // A swinging idle window is genuinely uninteresting (nothing asked of the gateway); "growing" would
  // be wrong there. mcode since 0 is a real code, not an unmeasured magnitude.
  const sh = mcode(mem.idle_shape ?? mem.memory_idle_shape);
  if (sh === 0) return "swinging, not growing";
  if (sh === -1) return "releasing";
  return rate != null ? `growing ${fmt1(rate)} MiB/min` : "growing";
}

// The whole drawer for one gateway, as a string. `st` is threaded through so a test can drive the
// drawer in a chosen mode without mutating live state.
function drawerHtml(g, st = state) {
  const langC = LANG_COLORS[g.lang] || LANG_COLORS.Other;
  // Same per-gateway freshness signal the table row shows.
  const badge = measuredBadge(g);
  let h = `<header class="drawer-head">
    <h3>${gwLink(g)}</h3>
    <div class="chips"><span class="cls-chip">${esc(g.cls || "Gateway")}</span>
    <span class="lang-chip" style="background:${langC}">${esc(g.lang)}</span></div>
    ${badge ? `<div class="drawer-measured">${badge}</div>` : ""}
  </header>`;

  // AUDIT #7: hardware stamp of the displayed basis (the matrix), not a deleted legacy suite object.
  const hw = gatewayHardware(g), arch = gatewayArch(g);
  if (hw) h += `<p class="stamp muted">${esc(hw)}${arch ? ` (${esc(arch)})` : ""}</p>`;

  for (const l of LANES) {
    // Chooser-aware: perf + streaming read the same chosen cell the table shows, so drawer/table/
    // compare agree in every mode; memory + xlate read their canonical accessor.
    const j = laneRecord(l, g, st);
    h += `<section class="drawer-lane"><h4>${esc(l.label)}</h4>`;
    if (!j) h += `<p class="muted">not measured</p>`;
    else if (!laneServed(j, l.flag)) {
      // A multi-line diagnostic must not dump raw lines into the drawer: first line as the verdict,
      // rest folded into a collapsed Evidence block, rig paths scrubbed.
      // Fallback headline comes from naText, not the literal "not served" - a never-probed lane
      // would otherwise announce a refusal it never observed.
      const note = stripRigPaths(j[l.err] || "") || naText(j, l.flag, l.err).text;
      const nl = note.indexOf("\n");
      const head = nl >= 0 ? note.slice(0, nl) : note;
      const rest = nl >= 0 ? note.slice(nl + 1).trim() : "";
      h += `<p class="muted">${esc(head)}</p>`;
      if (rest) h += `<details class="evidence-fold"><summary>Evidence</summary><pre>${esc(rest)}</pre></details>`;
      h += laneStamp(j);
    }
    else {
      const pn = lanePathNote(l, j, st);
      if (pn) h += `<p class="lane-note muted">${esc(pn)}</p>`;
      // Each metric is a sealed envelope; metric() reads it (suppressed metrics filtered by na).
      // A measured failure stays a row (red, with counts) rather than vanishing with genuinely-absent
      // metrics - the drawer must tell the same story the table does.
      // `m.cell` is a metric whose value is a reading plus evidence (the frontier); everything else is
      // a plain envelope read through metric().
      h += `<dl>` + l.metrics.map((m) => ({ m, c: m.cell ? m.cell(j, st) : metric(j[m.k], m.fmt) })).filter((x) => !x.c.na || x.c.failed).map(({ m, c }) => {
        if (c.failed)
          return `<div><dt>${esc(txt(m.label))}</dt><dd class="failtext" title="${esc(c.note || "")}">${esc(c.text)}</dd></div>`;
        const conc = c.env && c.env.concurrency;
        const cc = conc != null && c.v > 0 ? ` (@ c=${fmtInt(conc)})` : "";
        const zeroWhy = c.v === 0 && c.env && ZERO_WHY[c.env.note];
        return `<div><dt>${esc(txt(m.label))}</dt><dd${c.note ? ` title="${esc(c.note)}"` : ""}>${esc(c.text + cc)}${
          zeroWhy ? ` <span class="muted">(${esc(zeroWhy)})</span>` : ""}</dd></div>`;
      // Engine's definition of this lane's metrics, right under the numbers it defines.
      }).join("") + `</dl>` + (l.extra ? l.extra(j) : "") +
        definitionsFold(LANE_DEFINITION_PREFIXES[l.key] || [], stateData(st)) + `${laneStamp(j)}`;
    }
    h += `</section>`;
  }

  /* protocol matrix row with evidence */
  h += `<section class="drawer-lane"><h4>Protocol matrix</h4>`;
  // Presence is not content: `g.matrix.cells` is a legacy flat map that's always `{}` now (truthy but
  // empty), so reads go one level down into `upstreams[egress].cells[ingress]` instead.
  const diag = matrixDiagonal(g);
  if (!diag) h += `<p class="muted">not measured</p>`;
  else {
    h += `<ul class="matrix-list">` + MATRIX_CELLS.map((c) => {
      const cell = diag[c];
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

  // The sweep the whole frontier is read from, plotted once. The six published readings are maxima
  // over subsets of these same probed points.
  h += `<section class="drawer-lane"><h4>The concurrency sweep</h4>` +
    `<p class="lane-note muted">One sweep per cell, and every published throughput reading is a maximum over some subset of these same rungs: every point is a real probe, nothing decides when to stop looking. The marked dot is the reading at the bound the board is currently showing (${esc(boundLabel(selectedBound(st)))}), at the concurrency it was observed at.</p>` +
    `<div id="drawer-sweeps" class="sweeps"></div></section>`;

  // OOTB config artifact: exact as-shipped default config (pointed at the mock). "Suggest a
  // correction" opens a pre-filled GitHub issue.
  h += `<section class="drawer-lane"><h4>Config</h4>`;
  if (typeof g.ootb_config === "string" && g.ootb_config.trim()) {
    h += `<p class="lane-note muted">As-shipped default, pointed at the mock — reproduce with: fresh install + this config.</p>` +
      `<div class="config-block">` +
      `<button type="button" class="config-copy" data-config-copy title="Copy config">Copy</button>` +
      `<pre class="config-pre">${esc(g.ootb_config.replace(/\n+$/, ""))}</pre>` +
      `</div>` +
      `<p class="config-correct muted">Best-effort OOTB config. Spot something off? ` +
      `<a href="${esc(configCorrectionUrl(g))}" target="_blank" rel="noopener">Suggest a correction</a>.</p>`;
  } else {
    h += `<p class="muted">not published</p>`;
  }
  h += `</section>`;
  // Downloadable artifact is the gateway's complete record from data.json. Client-side blob, no server.
  h += `<section class="drawer-lane"><h4>Results</h4>` +
    `<p class="lane-note muted">The gateway's complete record — the full 6×6 matrix (per-cell perf + streaming), the memory read, the OOTB config, and the build stamp.</p>` +
    `<button type="button" class="results-download" data-results-download title="Download this gateway's full results as JSON">Download results (JSON)</button>` +
    `</section>`;
  return h;
}

// The gateway's complete record from data.json, as pretty JSON for the client-side download.
function gatewayResultsJson(g) {
  return JSON.stringify(g, null, 2);
}

function openDrawer(key, push = false) {
  const g = state.data.gateways.find((x) => x.key === key);
  if (!g) return;
  state.drawer = key;
  document.getElementById("drawer-body").innerHTML = drawerHtml(g);
  document.getElementById("drawer").classList.remove("hidden");
  document.getElementById("backdrop").classList.remove("hidden");
  // Copy-to-clipboard for the OOTB config block (copies the raw published text, not the escaped HTML).
  const copyBtn = document.querySelector("#drawer-body [data-config-copy]");
  if (copyBtn && typeof g.ootb_config === "string") {
    copyBtn.addEventListener("click", () => {
      const done = () => { copyBtn.textContent = "Copied"; setTimeout(() => { copyBtn.textContent = "Copy"; }, 1500); };
      if (navigator.clipboard && navigator.clipboard.writeText) navigator.clipboard.writeText(g.ootb_config).then(done, () => {});
    });
  }
  // Download the gateway's complete record as <gateway>-results.json (client-side blob, no server).
  const dlBtn = document.querySelector("#drawer-body [data-results-download]");
  if (dlBtn) {
    dlBtn.addEventListener("click", () => {
      const blob = new Blob([gatewayResultsJson(g)], { type: "application/json" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url; a.download = `${g.key}-results.json`;
      document.body.appendChild(a); a.click(); a.remove();
      URL.revokeObjectURL(url);
      dlBtn.textContent = "Downloaded"; setTimeout(() => { dlBtn.textContent = "Download results (JSON)"; }, 1500);
    });
  }
  const box = document.getElementById("drawer-sweeps");
  // Chooser-aware + gated: reads the same chosen cell the table reads, and a suppressed metric
  // draws no curve.
  const series = perfSweepSeries(g, { sustained: "#4cc38a", max: "#6cb6ff" });
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

/* Indices of the best value in a compare row - every index holding it, not just the first, so a tie
   (e.g. two gateways both below resolution) doesn't draw a false distinction. Empty Set if no contest. */
function bestIndex(vals, best) {
  // best == null means the row is evidence, not a contest (e.g. direct_c1_p99_us is a rig property,
  // not a gateway property) - crowning a winner would invent a ranking out of shared baseline noise.
  const none = new Set();
  if (best == null) return none;
  const present = vals.filter((v) => v != null);
  /* only highlight when there is an actual contest */
  if (present.length < 2) return none;
  const win = best === "min" ? Math.min(...present) : Math.max(...present);
  const winners = new Set();
  vals.forEach((v, i) => { if (v != null && v === win) winners.add(i); });
  return winners;
}

/* The entire compare panel as a string, given the gateways being compared. Split out of renderCompare
   (which touches the DOM on its first line) so it's testable without a DOM. Everything that decides
   anything lives here as a pure function of (gateways, state); DOM wiring stays in renderCompare. */
function compareBodyHtml(gws, st = state) {
  let h = `<div class="table-scroll"><table class="cmp-table"><thead><tr><th></th>` + gws.map((g, i) =>
    `<th><span class="dot" style="background:${CMP_COLORS[i]}"></span>${esc(g.display)}</th>`).join("") + `</tr></thead><tbody>`;
  h += `<tr><td class="metric">Class</td>${gws.map((g) => `<td>${esc(g.cls || "Gateway")}</td>`).join("")}</tr>`;
  h += `<tr><td class="metric">Language</td>${gws.map((g) => `<td>${esc(g.lang)}</td>`).join("")}</tr>`;
  h += `<tr><td class="metric">Build</td>${gws.map((g) => {
    // AUDIT #7: the build of the DISPLAYED basis (matrix / projected record), not a deleted legacy suite.
    const full = gatewayBuild(g) || "?";
    /* image digests and long refs stay in the tooltip; the cell stays compact */
    let short = full.replace(/\s*\(@sha256:[0-9a-f]+\)/, "");
    if (short.length > 40) short = short.slice(0, 37) + "...";
    return `<td title="${esc(full)}">${esc(short)}</td>`;
  }).join("")}</tr>`;

  for (const l of LANES) {
    // Chooser-aware: perf + streaming read the same chosen cell the table shows, so compare can
    // never disagree with the table. Skip the lane only when no gateway measured it at all.
    const recs = gws.map((g) => laneRecord(l, g, st));
    if (recs.every((j) => !j)) continue;
    h += `<tr class="lane-row"><td colspan="${gws.length + 1}">${esc(l.label)}</td></tr>`;
    if (l.pathNote) {
      // One disclosure row per canonical lane: which path each gateway's numbers measured.
      h += `<tr><td class="metric">Measured path</td>` + recs.map((j) =>
        laneServed(j, l.flag)
          ? `<td class="muted lane-note">${esc(lanePathNote(l, j, st))}</td>`
          : `<td class="na"></td>`).join("") + `</tr>`;
    }
    for (const m of l.metrics) {
      // Each metric read through the same metric() accessor the table uses (mval() alone would
      // collapse states the table renders apart). Ranking still uses mval().
      // `m.cell` is a metric that is a reading plus evidence; everything else is a plain envelope.
      const cells = recs.map((j) => (laneServed(j, l.flag) ? (m.cell ? m.cell(j, st) : metric(j[m.k], m.fmt)) : null));
      const bi = bestIndex(cells.map((c) => (c ? c.v : null)), m.best);   // a Set: every tied best
      h += `<tr><td class="metric">${esc(txt(m.label))}</td>` + cells.map((c, i) => {
        if (!c || c.na) {
          if (c && c.failed) return `<td class="na failcell" title="${esc(c.note || "")}">${esc(c.text)}</td>`;
          if (c) return `<td class="na" title="${esc(c.note || "")}">${esc(c.text)}</td>`;
          const na = naText(recs[i], l.flag, l.err);
          return `<td class="na" title="${esc(na.note)}">${esc(na.text)}</td>`;
        }
        const zeroWhy = c.v === 0 && c.env && ZERO_WHY[c.env.note];
        return `<td class="${bi.has(i) ? "best" : ""}"${c.note ? ` title="${esc(c.note)}"` : ""}>${esc(c.text)}${
          zeroWhy ? `<span class="zero-why">${esc(zeroWhy)}</span>` : ""}</td>`;
      }).join("") + `</tr>`;
    }
    // Curves side by side: same three gateways' readings, but as a scale a reader compares at a glance.
    if (l.cmpExtra) {
      const blocks = recs.map((j) => (laneServed(j, l.flag) ? l.cmpExtra(j, st) : ""));
      if (blocks.some(Boolean))
        h += `<tr><td class="metric">Curve across bounds</td>` +
          blocks.map((b) => (b ? `<td class="shape">${b}</td>` : `<td class="na"></td>`)).join("") + `</tr>`;
    }
  }
  h += `</tbody></table></div>`;
  h += `<p class="fineprint">Best value per row is highlighted, decided by the measurement (lower latency and memory, higher throughput). The sweep below each gateway is the ONE concurrency sweep its cell was measured on - every published throughput reading is a maximum over some subset of these same rungs - and the marked dot is the reading at the bound the board is currently showing, at the concurrency it was observed at.</p>`;
  h += `<div id="cmp-sweeps" class="sweeps"></div>`;
  return h;
}

function renderCompare() {
  const gws = state.cmp.map((k) => state.data.gateways.find((g) => g.key === k)).filter(Boolean);
  if (gws.length < 2) return;
  state.cmpOpen = true;
  const panel = document.getElementById("compare-panel");
  panel.classList.remove("hidden");
  document.getElementById("compare-body").innerHTML = compareBodyHtml(gws, state);

  // Chooser + bound aware, through perfSweepSeries, the one place that decides what a perf curve is.
  const series = gws.map((g, i) => ({ ...perfSweepSeries(g, { sustained: CMP_COLORS[i] })[0], label: g.display, color: CMP_COLORS[i] }));
  renderSweepCharts(document.getElementById("cmp-sweeps"), series, chartTheme());
}
function closeCompare(sync = true) {
  state.cmpOpen = false;
  document.getElementById("compare-panel").classList.add("hidden");
  if (sync) syncUrl(true);
}

/* ---- protocol matrix view --------------------------------------------------- */
/* One 6x6 grid per gateway, rows = ingress dialect, cols = upstream (egress) dialect. Cell states:
   pass/fail/not configurable/unprobed_auth/n/a (unmeasured in v1). Diagonal needs no translation.
   gen-data normalizes v1 results into the same upstreams shape. */
function matrixCell(g, egress, ingress) {
  const up = g.matrix && g.matrix.upstreams && g.matrix.upstreams[egress];
  return up && up.cells ? up.cells[ingress] : null;
}
// Tooltip text for a cell. A grey (not_configurable) cell shows the cited capability-limit reason,
// never a bare "we didn't test it". Green/red show the verdict label + note.
function matrixCellTip(cell) {
  const [, label] = cellState(cell);
  if (cell.served === "not_configured")
    // Probe-first grey: probed and failed - show probe evidence, never graded red.
    return `not configured: the capability probe on this ingress/upstream pairing did not complete a correct translation round trip${cell.probe_note ? " - " + cell.probe_note : cell.verdict_note ? " - " + cell.verdict_note : ""}`;
  if (cell.served === "not_configurable")
    // Grid is drafted by busbar from project docs, not confirmed by the project's own maintainers.
    return `not tested (this cell is not in the capability grid we drafted from the project's docs; the maintainers have not confirmed their own grid yet)${cell.verdict_note ? ": " + cell.verdict_note : ""}`;
  if (cell.served === "untestable")
    return `untestable on this rig: the gateway supports this pair in production but pins the real cloud host (no upstream base-URL override), so the test mock is unreachable - a harness limit, not gateway incapability${cell.verdict_note ? ": " + cell.verdict_note : ""}`;
  if (cell.served === "failed")
    // Real pairing; gateway reached and declined this attempt.
    return `failed: the gateway answered HTTP ${cell.status || "?"}${cell.body_snippet ? " - " + cell.body_snippet : ""}`;
  if (cell.served !== true && cell.served !== "unprobed_auth" && isHarnessGap(cell))
    return `not verified: the harness could not get this gateway serving under this upstream config${cell.verdict_note ? " (" + cell.verdict_note + ")" : ""}`;
  return `${label}. ${cell.verdict_note || ""}`;
}
/* Per-cell perf line for a green cell's tooltip/detail: sustained RPS + added latency p99, and delta
   vs the gateway's reference cell (named, never called "best"). Grey/red/unprobed return "".
   Dead on the live UI (cellPopFull used instead) but still exported. Reads every metric through
   mval() so an absent figure surfaces its own reason. */
function cellPerfTip(cell, ingress, egress, best, boundMs = selectedBound()) {
  const p = cell && cell.served === true ? cell.perf : null;
  const rd = p ? frontierAt(frontierOf(p), boundMs) : null;
  const rps = rd ? mval(rd.rps) : null;
  const lat = p ? mval(p.added_latency_p99_us) : null;
  // A record with no reading at all at this bound has nothing to say about throughput here.
  if (!p || !rd) return "";
  // An absent reading at this bound: show the certified added-latency alone rather than a bare "".
  if (rps == null) {
    // The record's own reason, not a guessed one (via METRIC_NOTES, consistent with the rest of the file).
    const why = rd.rps && rd.rps.reason ? noteText(rd.rps.reason) : "not measured";
    return lat != null ? `+${fmtInt(lat)} µs p99 added (no reading at ${boundLabel(boundMs)}: ${why})` : "";
  }
  const bpRd = frontierAt(frontierOf(best), boundMs);
  const bp = cellPath(best), bRps = bpRd ? mval(bpRd.rps) : null;
  // The bound is named and "≥" travels: a floor must never be stated as a ceiling.
  let s = `${rd.lower_bound === true ? "≥ " : ""}${fmtRate(rps)} req/s ${boundClause(boundMs)}`;
  if (lat != null) s += `, +${fmtInt(lat)} µs p99 added`;
  if (bRps != null && bRps > 0) {
    if (bp.ingress === ingress && bp.egress === egress) s += " - reference cell (ranks the table)";
    // Human dialect labels (MATRIX_LABELS), never the raw dialect keys, in the hover popup.
    else s += ` - ${fmtPct((rps / bRps - 1) * 100)} req/s vs the ${MATRIX_LABELS[bp.ingress] || bp.ingress}→${MATRIX_LABELS[bp.egress] || bp.egress} cell`;
  }
  return s;
}

/* Rich matrix-cell popup, the visual face of Custom mode: hovering a cell shows the same gated
   per-cell numbers the Performance/Streaming tables show, plus delta-to-own-cell and capability
   verdict/evidence. Returns "" for an unmeasured egress column a v1 result never probed. */
function cellPopFull(g, ingress, egress) {
  const cell = matrixCell(g, egress, ingress);
  if (!cell) return "";
  const [, label] = cellState(cell);
  const head = `<h4>${esc(g.display)}: ${esc(MATRIX_LABELS[ingress])} in / ${esc(MATRIX_LABELS[egress])} upstream — ${esc(label)}${
    cell.status ? ` (HTTP ${esc(cell.status)})` : ""}</h4>`;
  // Read the SAME gated values the tables read, by pinning a synthetic Custom-mode state on this cell.
  const st = { mode: "custom", xlateIn: ingress, xlateOut: egress };
  const rows = [];
  // A measured failure is a row, not a filtered-out absence.
  const pushRow = (lbl, c) => {
    if (!c.na) rows.push(`<div><span>${lbl}</span><b>${esc(c.text)}</b></div>`);
    else if (c.failed) rows.push(`<div><span>${lbl}</span><b class="failtext" title="${esc(c.note || "")}">${esc(c.text)}</b></div>`);
  };
  const perfRow = (key, fmt, lbl) => pushRow(lbl, chooserPerfCell(g, key, fmt, st));
  perfRow("added_latency_p50_us", fmtAdded, "Added latency p50");
  perfRow("added_latency_p99_us", fmtAdded, "Added latency p99");
  // Throughput at the board's selected bound, through the same reader the Performance table uses.
  // Always rendered, even with no reading at that bound: silently omitting it would leave a reader
  // unable to tell "cannot do it at 5ms" from "popup doesn't show throughput" - opposite conclusions.
  {
    const lbl = esc(boundColLabel(selectedBound(state)));
    const c = frontierChooserCell(g, st, selectedBound(state));
    if (!c.na) pushRow(lbl, c);
    else rows.push(`<div><span>${lbl}</span><b class="muted" title="${esc(c.note || "")}">${esc(c.text)}</b></div>`);
  }
  const streamRow = (key, fmt, lbl) => pushRow(lbl, chooserStreamCell(g, key, fmt, st));
  streamRow("added_ttft_p99_us", fmtUsMs, "Added TTFT p99");
  streamRow("streams_sustained", fmtInt, "Streams sustained");
  const perfBlock = rows.length
    ? `<div class="pop-metrics">${rows.join("")}</div>`
    // A served cell with no per-cell perf (unswept), or a non-green cell: honest "not measured".
    : (cell.served === true ? `<div class="pop-perf muted">served, not measured on this cell</div>` : "");
  // Delta: this cell vs the gateway's own representative diagonal (best_cell). "" for that cell itself.
  const cellPerf = chooserCellPerf(g, st);
  // A rate tells a reader whether this cell is fast; the shape tells them whether it's fast because
  // the tail was allowed to grow.
  const shapeBlock = cellPerf ? frontierBlock(cellPerf, { compact: true }) : "";
  const cellPerfLabeled = cellPerf ? { ingress, egress, ...cellPerf } : null;
  const delta = deltaToPeak(cellPerfLabeled, g.best_cell);
  const bp = g.best_cell ? g.best_cell.path : null;
  // "vs its own cell", not "vs peak": best_cell isn't necessarily this gateway's fastest, and calling
  // it "peak" would make a negative delta read as impossible rather than ordinary.
  const deltaBlock = delta
    ? `<div class="pop-delta">vs its own cell (${esc(MATRIX_LABELS[bp.ingress] || bp.ingress)}→${esc(MATRIX_LABELS[bp.egress] || bp.egress)}): ${esc(delta)}</div>`
    : (cellPerf && bp && bp.ingress === ingress && bp.egress === egress
      ? `<div class="pop-delta muted">this IS the cell that ranks the Performance tab</div>` : "");
  const verdict = cell.verdict_note ? `<div class="pop-note">${esc(cell.verdict_note)}</div>` : "";
  // Egress fairness guard: the mock answers all six dialects by path, so a gateway forwarding the
  // ingress request verbatim would still score 200 as a translation it never performed.
  // egress_reverified checks it actually re-shaped the request. Only stated off-diagonal (same-dialect
  // passthrough has nothing to translate).
  const rv = cellPerf && cellPerf.egress_reverified;
  const reverify = (ingress !== egress && cellPerf && cellPerf.egress_reverified != null)
    ? `<div class="pop-note ${rv ? "" : "warn"}">${rv
      ? "egress re-verified: the request reaching the mock was in the egress dialect, not the ingress one"
      : "egress NOT re-verified: the mock saw the ingress shape, so this cell may be a verbatim proxy rather than a translation"}${
      cellPerf.reverify_note ? ` - ${esc(cellPerf.reverify_note)}` : ""}</div>`
    : "";
  const cta = cell.served === true ? `<div class="pop-cta muted">click → Performance (Custom, this cell)</div>` : "";
  return head + perfBlock + shapeBlock + deltaBlock + verdict + reverify + cta;
}
// Did this gateway produce a protocol matrix at all?
function hasMatrixGrid(g) { return !!(g && g.matrix && (g.matrix.upstreams || g.matrix.cells)); }
// Why a gateway has no matrix. Falls back to a plain statement rather than inventing a cause.
function matrixFailureReason(g) {
  const first = [g && g.matrix && g.matrix.error, g && g.matrix_error, g && g.serve_error]
    .find((x) => typeof x === "string" && x.trim());
  const why = first ? stripRigPaths(first).split("\n")[0] : "the run produced no protocol matrix for this gateway";
  return `no matrix result: ${why}`;
}
/* Rows the protocol grid renders: every gateway, matrix or not. No matrix -> all-n/a row with its
   failure reason, never a silent absence. Sorted by pass count then name. Pure; covered by site/test.mjs. */
function matrixRoster(gateways, tally) {
  return (gateways || []).slice().sort((a, b) =>
    (hasMatrixGrid(b) ? tally(b).pass : -1) - (hasMatrixGrid(a) ? tally(a).pass : -1) ||
    a.display.localeCompare(b.display));
}
function renderMatrix() {
  const gateways = state.data.gateways || [];
  // Empty state is for a board with no matrix data at all; a single gateway without a matrix renders
  // as an all-n/a row instead.
  if (!gateways.some(hasMatrixGrid)) {
    document.getElementById("matrix-empty").classList.remove("hidden");
    document.getElementById("matrix-grid").classList.add("hidden");
    return;
  }
  // Per-gateway tallies over the full grid; sorted by pass count desc, then name.
  const tally = (g) => {
    const t = { pass: 0, fail: 0, notconf: 0, unprobed: 0, unverified: 0, untestable: 0 };
    for (const e of MATRIX_CELLS) for (const c of MATRIX_CELLS) {
      const cell = matrixCell(g, e, c);
      if (!cell) continue;
      if (cell.served === true) t.pass++;
      else if (cell.served === "not_configured" || cell.served === "not_configurable") t.notconf++;
      else if (cell.served === "unprobed_auth") t.unprobed++;
      else if (cell.served === "untestable") t.untestable++;
      else if (isHarnessGap(cell)) t.unverified++;
      else t.fail++;
    }
    return t;
  };
  const rows = matrixRoster(gateways, tally);

  const grid = document.getElementById("matrix-grid");
  grid.innerHTML = rows.map((g) => {
    const t = tally(g);
    const missing = !hasMatrixGrid(g);
    const bits = missing
      ? [`<b class="pass-count">0</b>/36 pass`, `<span class="matrix-nores">${esc(matrixFailureReason(g))}</span>`]
      : [`<b class="pass-count">${t.pass}</b>/36 pass`];
    if (t.fail) bits.push(`${t.fail} fail`);
    if (t.notconf) bits.push(`${t.notconf} not configured`);
    if (t.untestable) bits.push(`${t.untestable} untestable (mock limit)`);
    if (t.unverified) bits.push(`${t.unverified} not verified`);
    if (t.unprobed) bits.push(`${t.unprobed} unprobed (auth)`);
    return `<section class="matrix-gw">
      <header class="matrix-gw-head"><h3>${gwLink(g)}</h3><span class="muted">${bits.join(" · ")}</span></header>
      <div class="table-scroll matrix-table"><table>
        <thead><tr><th class="axis">ingress &#8595; \\ upstream &#8594;</th>${
          MATRIX_CELLS.map((e) => `<th>${esc(MATRIX_LABELS[e])}</th>`).join("")
        }</tr></thead><tbody>${
        MATRIX_CELLS.map((c) => `<tr><td class="name">${esc(MATRIX_LABELS[c])}</td>${
          MATRIX_CELLS.map((e) => {
            const cell = matrixCell(g, e, c);
            // Two different absences, said differently: no matrix at all carries its failure reason.
            if (!cell) return `<td class="na" title="${esc(missing ? matrixFailureReason(g)
              : "not measured (v1 result: this upstream dialect was not probed)")}">n/a</td>`;
            const [cls] = cellState(cell);
            const diag = e === c ? " diag" : "";
            // No native `title`: the richer hover popup carries verdict + perf; a title would double up.
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
  const showPop = (el) => {
    const g = state.data.gateways.find((x) => x.key === el.dataset.gw);
    // el.dataset.cell is the INGRESS (row), el.dataset.egress the upstream (column).
    const html = g && cellPopFull(g, el.dataset.cell, el.dataset.egress);
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
    // Click a served cell -> jump to Performance tab in Custom mode with this in->out pinned.
    el.addEventListener("click", () => {
      const g = state.data.gateways.find((x) => x.key === el.dataset.gw);
      const cell = g && matrixCell(g, el.dataset.egress, el.dataset.cell);
      if (!cell || cell.served !== true) return;
      state.view = "performance"; state.mode = "custom";
      state.xlateIn = el.dataset.cell; state.xlateOut = el.dataset.egress;
      state.sortCol = VIEW_SORT.performance; state.sortDesc = true;
      showView("performance"); renderFilters(); renderTable(); syncUrl(true);
      window.scrollTo({ top: 0, behavior: "smooth" });
    });
  });
}

/* ---- charts gallery --------------------------------------------------------- */
/* Charts tab: horizontal ranked bars from the live board, replacing 25 static PNGs. Metric is a
   control; bound and cell come from the selectors every other tab uses.
   A gateway with no value is listed, not dropped - an absent number usually means a refusal a reader
   needs to see, so it's named under the chart with its reason. */
function renderCharts() {
  const panel = document.getElementById("chart-panel");
  const controls = document.getElementById("chart-controls");
  const note = document.getElementById("chart-note");
  if (!panel || !controls) return;
  const metric = CHART_METRICS.find((m) => m.id === state.chartMetric) || CHART_METRICS[0];

  controls.innerHTML =
    `<label class="chart-metric">Metric
       <select id="chart-metric-select">${CHART_METRICS.map((m) =>
         `<option value="${esc(m.id)}"${m.id === metric.id ? " selected" : ""}>${esc(m.label)}</option>`).join("")}
       </select>
     </label>`;
  const sel = document.getElementById("chart-metric-select");
  if (sel) sel.addEventListener("change", (e) => { state.chartMetric = e.target.value; renderCharts(); });

  const roster = applyFilters(state.data.gateways || [], state);
  const rows = chartRows(metric, roster, state);
  const missing = roster.filter((g) => !rows.some((r) => r.key === g.key));

  if (!rows.length) {
    panel.innerHTML = `<p class="muted">No gateway on this board carries ${esc(metric.label)} yet.</p>`;
    note.textContent = "";
    return;
  }
  panel.innerHTML = `<canvas id="chart-canvas" width="900" height="${Math.max(160, rows.length * 30 + 60)}"></canvas>`;
  drawRankedBars(document.getElementById("chart-canvas"), rows, metric, chartTheme());

  note.innerHTML = esc(metric.note) +
    (metric.log ? " Drawn on a LOG scale: this metric spans orders of magnitude, and a linear axis would collapse most of the field into the width of a line." : "") +
    (missing.length
      ? `<br>Not shown (no value on the chosen cell): ${missing.map((g) => esc(g.name || g.key)).join(", ")}.`
      : "");
}

/* One bar per gateway, longest-first by the metric's own direction. Log scale where the metric says
   so (wide-spread metrics collapse to a pixel on a linear axis); gridlines fall on decades, labelled
   in the metric's own units. */
function drawRankedBars(canvas, rows, metric, theme = {}) {
  const ctx = canvas && canvas.getContext && canvas.getContext("2d");
  if (!ctx) return null;
  const { W, H } = hidpi(canvas, ctx);
  const padL = 150, padR = 90, padT = 18, padB = 26;
  ctx.clearRect(0, 0, W, H);
  const fg = theme.fg || "#9aa4b2", grid = theme.grid || "rgba(154,164,178,.18)";
  const ink = theme.ink || fg;
  const vals = rows.map((r) => r.v);
  const hi = Math.max(...vals);
  // A log axis cannot plot 0 or a negative, and both are real states here (a measured zero rate).
  // Those rows draw at the axis floor with their value still labelled, rather than being dropped.
  const lo = metric.log ? Math.min(...vals.filter((v) => v > 0)) : 0;
  const X = (v) => {
    if (!metric.log) return padL + (v / (hi || 1)) * (W - padL - padR);
    if (!(v > 0)) return padL;
    const a = Math.log10(lo || 1), b = Math.log10(hi || 1);
    return padL + (b > a ? (Math.log10(v) - a) / (b - a) : 1) * (W - padL - padR);
  };
  const rowH = (H - padT - padB) / rows.length;

  ctx.font = "11px Inter, sans-serif";
  ctx.textBaseline = "middle";
  if (metric.log && hi > 0) {
    ctx.textAlign = "center";
    for (let d = Math.floor(Math.log10(lo || 1)); d <= Math.ceil(Math.log10(hi)); d++) {
      const x = X(Math.pow(10, d));
      if (x < padL || x > W - padR) continue;
      ctx.strokeStyle = grid;
      ctx.beginPath(); ctx.moveTo(x, padT); ctx.lineTo(x, H - padB); ctx.stroke();
      ctx.fillStyle = fg;
      ctx.fillText(fmtChartTick(Math.pow(10, d), metric.unit), x, H - padB + 10);
    }
  }
  rows.forEach((r, i) => {
    const y = padT + i * rowH + rowH / 2;
    const w = Math.max(2, X(r.v) - padL);
    // The SAME language palette every other surface uses. Colour is a neutral property here:
    // never rank, never brand, so it cannot be read as favouring an entrant.
    ctx.fillStyle = LANG_COLORS[r.g.lang] || LANG_COLORS.Other;
    ctx.fillRect(padL, y - rowH * 0.32, w, rowH * 0.64);
    ctx.fillStyle = ink;
    ctx.textAlign = "right";
    ctx.fillText(r.name, padL - 8, y);
    ctx.textAlign = "left";
    ctx.fillText(fmtChartValue(r.v, metric.unit), padL + w + 6, y);
  });
  return { X, padL };
}

/* A decade label in the metric's own units: "100 µs" reads, "0.0001 s" does not. */
function fmtChartTick(v, unit) {
  if (unit === "µs") return v >= 1e6 ? `${v / 1e6} s` : v >= 1000 ? `${v / 1000} ms` : `${v} µs`;
  if (unit === "USD") return v >= 1 ? `$${v}` : `$${v.toFixed(String(v).length)}`;
  return fmtTick(v);
}
function fmtChartValue(v, unit) {
  if (unit === "µs") return v >= 1000 ? `${fmt1(v / 1000)} ms` : `${fmtInt(v)} µs`;
  if (unit === "USD") return v < 0.01 ? `$${v.toFixed(4)}` : `$${fmt2(v)}`;
  if (unit === "MiB") return `${fmt1(v)} MiB`;
  return fmtInt(v);
}

/* ---- method links + footer -------------------------------------------------- */
function renderStatic() {
  const repo = state.data.repo || "https://github.com/GetBusbar/benchmarking";
  // Method links point at what actually produces the numbers: the orchestrator that drives a run, and
  // the engine that measures.
  const PRODUCERS = { matrix: "run-on-ec2.sh", memory: "engine/src/metric.rs" };
  for (const [id, path] of Object.entries(PRODUCERS)) {
    const a = document.getElementById(`lnk-${id}`);
    if (a) a.href = `${repo}/blob/main/${path}`;
  }
  document.getElementById("repo-link").href = repo;
  const hw = document.getElementById("hw-stamp");
  const bits = [];
  if (state.data.hardware) bits.push(`Ran on: ${state.data.hardware}`);
  if (state.data.latest_measured_at) bits.push(`Latest measurement: ${stampWithAge(state.data.latest_measured_at)}`);
  /* Which benchmark produced this board, so the per-row sha has something to compare against.
     Distinguishes two different facts: a row on a different sha ("measured on an older version") vs
     a row with no sha at all ("not yet measured on it") - conflating them once produced a false
     "stale results" claim for rows that actually had no results yet. Each clause appears only when
     its set is non-empty. Extracted as a pure function so the sentence is testable. */
  const bv = benchmarkVersionStamp(state.data);
  if (bv) bits.push(bv);
  bits.push(`Site data generated: ${state.data.generated_at ? stampWithAge(state.data.generated_at) : "unknown"}`);
  const rig = rigStamp();
  if (rig) bits.push(rig);
  hw.textContent = bits.join(" · ");
}

function benchmarkVersionStamp(data) {
  if (!data || !data.benchmark_version) return "";
  const gws = data.gateways || [];
  const off = gws.filter((g) => g.engine && !g.engine.current);
  const older = off.filter((g) => g.engine.sha).length;
  const unmeasured = off.length - older;
  const short = String(data.benchmark_version).slice(0, 7);
  const clauses = [];
  if (older) clauses.push(`${older} row${older === 1 ? "" : "s"} measured on an older version`);
  if (unmeasured) clauses.push(`${unmeasured} not yet measured on it`);
  return `Benchmark version: ${short}${clauses.length ? ` (${clauses.join(", ")})` : ""}`;
}

/* Which measurement instrument produced the board's numbers. The mock+loadgen come from a moving
   GitHub release tag, so an identical harness can produce different verdicts if rebuilt in between.
   Returns "" when no gateway records a digest. Shows a count when rows disagree. */
function rigStamp() {
  const digests = new Map();   // short digest -> how many gateways used it
  for (const g of (state.data.gateways || [])) {
    const sha = g.rig && g.rig.mock && g.rig.mock.sha256;
    if (typeof sha !== "string" || sha.length < 12) continue;
    const short = sha.slice(0, 12);
    digests.set(short, (digests.get(short) || 0) + 1);
  }
  if (!digests.size) return "";
  if (digests.size === 1) return `Rig (mock): ${[...digests.keys()][0]}`;
  return `Rig (mock): ${digests.size} DIFFERENT builds across rows — ` +
    [...digests.entries()].map(([d, n]) => `${d} (${n})`).join(", ");
}

/* ---- gateways overview: the neutral roster ----------------------------------
   Landing view is a roster, not a ranking: every gateway alphabetical, with language, a star
   snapshot, and self-description. No perf numbers, no winner highlighting. */
// Roster sort state: sortable by any column, defaulting to name A-Z. `name` tiebreaks every column.
let rosterSort = { col: "name", dir: "asc" };
// Per-column sort key. null/n/a sorts last regardless of direction.
const ROSTER_KEY = {
  name: (g) => g.display.toLowerCase(),
  lang: (g) => (g.lang || "").toLowerCase(),
  // Sorts on the same token the cell renders (versionToken).
  version: (g) => { const v = versionToken(g); return v ? v.toLowerCase() : null; },
  lastrun: (g) => { const d = gatewayLastRun(g); return d ? d.getTime() : null; }, // newer = larger ms
  age: (g) => (g.first_commit ? new Date(g.first_commit).getTime() : null), // older = smaller ms
  stars: (g) => (g.stars == null ? null : g.stars),
  // First-party rows sort last in either direction, so outside contributors cluster at the top.
  contrib: (g) => { const cs = g.contributed_by || []; return cs.length ? cs[0].handle.toLowerCase() : null; },
};
const rosterRows = (gateways) => {
  const key = ROSTER_KEY[rosterSort.col] || ROSTER_KEY.name;
  const dir = rosterSort.dir === "desc" ? -1 : 1;
  const cmp = (a, b) => {
    const ka = key(a), kb = key(b);
    // Null always sinks to the bottom, independent of dir.
    if (ka == null && kb == null) return a.display.toLowerCase().localeCompare(b.display.toLowerCase());
    if (ka == null) return 1;
    if (kb == null) return -1;
    let r = typeof ka === "number" ? ka - kb : String(ka).localeCompare(String(kb));
    if (r === 0) r = a.display.toLowerCase().localeCompare(b.display.toLowerCase()); // stable by name
    return r * dir;
  };
  return gateways.slice().sort(cmp);
};
/* Star counts render compact: 12345 -> "12.3k", below 1000 the full int. Null (no
   snapshot entry) stays null; the cell renders it muted. */
const fmtStars = (v) => (v == null ? null : v >= 1000 ? `${(v / 1000).toFixed(1)}k` : String(Math.round(v)));
// Contributors cell: `@handle` chips linking to GitHub profiles (url pre-validated https-only in
// gen-data). First-party entrant has no contributor and renders a muted dash.
function contribCell(g) {
  const cs = g.contributed_by || [];
  if (!cs.length) return `<span class="muted" title="first-party entrant">&mdash;</span>`;
  return cs.map((c) => {
    const chip = `@${esc(c.handle)}`;
    const title = esc(`${c.name || c.handle} contributed this entrant`);
    return c.url
      ? `<a class="contrib-link" href="${c.url}" target="_blank" rel="noopener noreferrer" title="${title}">${chip}</a>`
      : `<span class="contrib-link" title="${title}">${chip}</span>`;
  }).join(" ");
}
// Project age from first-commit date, one floored unit ("11+ years", "7+ months"). Null renders muted.
const fmtProjectAge = (firstCommit) => {
  if (!firstCommit) return null;
  const days = Math.max(0, (Date.now() - new Date(firstCommit).getTime()) / 86400e3);
  // Floored so the "+" is honest: 11.7 years reads "11+ years".
  if (days >= 365) return `${Math.floor(days / 365)}+ year${days >= 730 ? "s" : ""}`;
  if (days >= 30.44) return `${Math.floor(days / 30.44)}+ month${days >= 61 ? "s" : ""}`;
  if (days >= 7) return `${Math.floor(days / 7)}+ week${days >= 14 ? "s" : ""}`;
  return `${Math.max(1, Math.floor(days))} days`;
};

// gatewayBuild reads the stamp of what is shown: the matrix, falling back to a projected record's
// source.build. `g[l.key]` is a raw suite object the emit step deletes, not a valid source here.
const displayedRecords = (g) => [g.best_cell, g.translation_cell, g.streaming, g.memory_read].filter(Boolean);
/* Build stamp of what actually ran, null for a gateway with no run. Split from gatewayBuild (which
   falls back to the manifest pin) because the Version column needs to distinguish "measured" from
   "pinned, not yet measured" rather than caption an unmeasured row as though its pin were a launch
   stamp. */
const measuredBuild = (g) => {
  if (g && g.matrix && g.matrix.build) return g.matrix.build;
  const rec = displayedRecords(g || {}).find((r) => r.source && r.source.build);
  return rec ? rec.source.build : null;
};
const gatewayBuild = (g) => {
  const measured = measuredBuild(g);
  if (measured) return measured;
  // Not-yet-measured is not not-known: a gateway awaiting its first run still has g.version, the
  // manifest pin, so the field always lists what it runs even before "last benchmarked" is filled in.
  return (g && g.version) || null;
};
// Hardware the displayed numbers were measured on: the matrix stamp (sole source).
const gatewayHardware = (g) => (g && g.matrix && g.matrix.hardware) || null;
const gatewayArch = (g) => (g && g.matrix && g.matrix.arch) || null;
/* How the gateway was run: official Docker image vs native/source binary, inferred from the build
   stamp. Real context - base image, fd limits, and startup differ between the two. Null if unstamped. */
const runMode = (g) => {
  const b = gatewayBuild(g); if (!b) return null;
  return (/@sha256:/.test(b) || /[\w.\-]+\/[\w.\-]+:[\w.\-]+/.test(b)) ? "docker" : "binary";
};
// Compact monochrome run-mode marks; tooltip carries the words. docker = whale; binary = shell prompt.
const RUNMODE_ICON = {
  docker: '<svg class="rm-ico" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M4 10h3v3H4zm4 0h3v3H8zm4 0h3v3h-3zM8 6h3v3H8zm4 0h3v3h-3z"/><path d="M23 12.3c-.6-.4-1.8-.6-2.8-.4-.1-.9-.7-1.8-1.6-2.4l-.5-.3-.3.5c-.4.7-.6 1.6-.1 2.4-.3.2-1 .4-1.7.4H2c-.2 1.4.1 2.9.9 4.1C4 18.9 6.6 20 10 20c6.9 0 12-3.2 14.3-9 .9.1 2.2 0 2.7-1.4-1.6-.9-3.7-.6-4-.3z" transform="translate(-2 0)"/></svg>',
  binary: '<svg class="rm-ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="2.5" y="4.5" width="19" height="15" rx="2"/><path d="M6.5 9.5l3 2.5-3 2.5M13 15h4.5"/></svg>',
};
const runModeCell = (g) => {
  const m = runMode(g); if (!m) return "";
  const label = m === "docker" ? "Measured running its official Docker image" : "Measured as a native / source-built binary";
  return `<span class="runmode ${m}" title="${label}" aria-label="${label}">${RUNMODE_ICON[m]}</span>`;
};
/* Version token a build stamp identifies, or null when it identifies none (e.g. a compiler output
   path or binary name for a source build). Split from fmtBuild so the caller can fall back to the
   manifest pin (versionToken) instead of dressing a path up as a release. */
function parseBuildVersion(full) {
  const head = String(full).split(" (")[0].trim();
  const first = head.split(/\s+/)[0];
  const colon = first.lastIndexOf(":");
  if (colon > 0 && !first.slice(colon + 1).includes("/")) return first.slice(colon + 1);
  if (first.includes("==")) return first.split("==").pop();
  if (first.includes("@")) {
    const ref = first.split("@").pop();
    // A bare commit sha keeps an "@" marker; a version-looking ref (npm "pkg@1.15.2") does not.
    return /^[0-9a-f]{7,40}$/.test(ref) ? "@" + ref.slice(0, 7) : ref;
  }
  const tail = head.match(/\s(v?\d[\w.\-]*)$/);
  if (tail) return tail[1];
  return null;
}
// Short form of a build stamp for display; when no version parses, shows the truncated stamp. The
// Version column doesn't use this - it goes through versionToken.
const fmtBuild = (full) => {
  const v = parseBuildVersion(full);
  if (v != null) return v;
  const head = String(full).split(" (")[0].trim();
  return head.length > 24 ? head.slice(0, 21) + "..." : head;
};
/* What version of this gateway was measured, or null. Prefers the build stamp of what actually ran;
   falls back to g.version (the manifest pin, rendered as "@sha" for a bare commit). Never falls
   through to the build stamp's raw text - a filesystem path under "Version" would be a false claim. */
function versionToken(g) {
  const build = measuredBuild(g);
  const parsed = build ? parseBuildVersion(build) : null;
  if (parsed != null) return parsed;
  const pin = g && g.version ? String(g.version).trim() : "";
  if (!pin) return null;
  return /^[0-9a-f]{7,40}$/i.test(pin) ? "@" + pin.slice(0, 7) : pin;
}
// Where the Version cell's token came from, as its tooltip: three distinct states (parsed from
// launch stamp; stamp names no version so this is the pin; pinned but not yet measured).
function versionBasis(g) {
  const build = measuredBuild(g);
  if (build && parseBuildVersion(build) != null) return `Measured running: ${build}`;
  if (build) return `Launched as: ${build} - that stamp names no version (it is a source build), so this is the commit pinned in the gateway's manifest, which is what the harness built.`;
  if (versionToken(g)) return "The version pinned in the gateway's manifest, which is what the harness builds. This gateway has not been measured on the current benchmark yet, so there is no launch stamp to read instead.";
  return "Nothing published for this gateway names a version: neither a build stamp of what ran nor a pin in its manifest.";
}

/* When that gateway was last benchmarked, for the roster cell + sort. Prefers g.measured_at (same
   per-row freshness basis as the "measured Nd ago" badge), so a standalone legacy re-run can't date
   the row fresher than the matrix numbers actually shown. Falls back to the newest lane-suite
   timestamp only when there's no matrix stamp. */
function gatewayLastRun(g) {
  if (g && g.measured_at) { const ms = new Date(g.measured_at).getTime(); if (ms > 0) return new Date(ms); }
  let newest = 0;
  for (const l of LANES) {
    const t = g[l.key] && g[l.key].measured_at;
    if (t) { const ms = new Date(t).getTime(); if (ms > newest) newest = ms; }
  }
  return newest ? new Date(newest) : null;
}
// Newest `measured_at` across every gateway's suites: when the field was last benchmarked.
function lastBenchmarkRun(gateways) {
  let newest = null;
  for (const g of gateways) {
    const d = gatewayLastRun(g);
    if (d && (!newest || d > newest)) newest = d;
  }
  return newest;
}
// Per-gateway last-benchmarked date for the roster cell: plain UTC date; full timestamp in tooltip.
const fmtLastRun = (d) => (d ? d.toISOString().slice(0, 10) : null);

function renderGateways() {
  const tbody = document.querySelector("#gateways-table tbody");
  if (!tbody || !state.data) return;
  // Sort-indicator + click wiring on the header (once): each <th data-sort="key"> becomes clickable.
  const thead = document.querySelector("#gateways-table thead");
  if (thead && !thead.dataset.wired) {
    thead.dataset.wired = "1";
    thead.querySelectorAll("th[data-sort]").forEach((th) => {
      th.classList.add("sortable");
      th.addEventListener("click", () => {
        const col = th.dataset.sort;
        if (rosterSort.col === col) rosterSort.dir = rosterSort.dir === "asc" ? "desc" : "asc";
        else rosterSort = { col, dir: "asc" };
        renderGateways();
      });
    });
  }
  if (thead) {
    thead.querySelectorAll("th[data-sort]").forEach((th) => {
      const active = th.dataset.sort === rosterSort.col;
      th.setAttribute("aria-sort", active ? (rosterSort.dir === "asc" ? "ascending" : "descending") : "none");
      th.dataset.dir = active ? rosterSort.dir : "";
    });
  }
  const rows = rosterRows(state.data.gateways);
  tbody.innerHTML = rows.map((g) => {
    const c = LANG_COLORS[g.lang] || LANG_COLORS.Other;
    const name = gwLink(g);
    const stars = fmtStars(g.stars);
    const version = versionToken(g);
    const age = fmtProjectAge(g.first_commit);
    const lastRun = gatewayLastRun(g);
    const lastRunTxt = fmtLastRun(lastRun);
    return `<tr data-gw="${esc(g.key)}" class="rowlink">
      <td class="name">${name}</td>
      <td><span class="lang-chip" style="background:${c}">${esc(g.lang)}</span></td>
      <td class="build">${version
        ? `<span title="${esc(versionBasis(g))}">${esc(version)}</span>`
        : `<span class="muted" title="${esc(versionBasis(g))}">no version published</span>`}</td>
      <td class="lastrun">${lastRunTxt ? `${runModeCell(g)}<span title="last benchmarked ${esc(lastRun.toISOString().slice(0, 16).replace("T", " "))} UTC">${esc(lastRunTxt)}</span>` : `<span class="muted">n/a</span>`}</td>
      <td class="age">${age ? `<span title="first commit ${esc(g.first_commit)}">${esc(age)}</span>` : `<span class="muted">n/a</span>`}</td>
      <td class="stars">${stars != null ? esc(stars) : `<span class="muted">n/a</span>`}</td>
      <td class="contrib">${contribCell(g)}</td>
    </tr>`;
  }).join("");
  // Row click opens the per-gateway drawer; a click on the repo link opens the repo instead.
  tbody.querySelectorAll("tr[data-gw]").forEach((tr) => {
    tr.addEventListener("click", (ev) => {
      if (ev.target.closest("a")) return;
      openDrawer(tr.dataset.gw, true);
    });
  });
  // "as of" disclosure for the star snapshot: the newest snapshot date in the bundle.
  const asOf = rows.map((g) => g.stars_as_of).filter(Boolean).sort().pop();
  const note = document.getElementById("stars-asof");
  if (note) note.textContent = asOf ? `Star counts are a GitHub snapshot as of ${asOf}, refreshed with the data, not live.` : "";
  // When the field was last benchmarked (newest measured_at across all gateways), UTC.
  const run = lastBenchmarkRun(rows);
  const runNote = document.getElementById("lastrun");
  if (runNote) {
    runNote.textContent = run
      ? `Benchmarks last run ${run.toISOString().slice(0, 16).replace("T", " ")} UTC.`
      : "";
  }
}

/* ---- home landing page ------------------------------------------------------
   Site root is a designed landing page: hero, pitch, neutrality line, and one CTA card per
   category. Pure HTML builder exported for the node smoke test. */
function homeCardsHtml(data) {
  // Live entrant count for the category whose bundle is loaded (gateways today).
  const counts = { gateways: data && Array.isArray(data.gateways) ? data.gateways.length : null };
  const cards = Object.values(CATEGORIES).map((c) => {
    const n = counts[c.id];
    const body = c.card || "";
    const desc = n != null ? `${n} ${body.charAt(0).toLowerCase()}${body.slice(1)}` : body;
    return `<a class="home-card" data-nav href="/${esc(c.id)}">` +
      `<h3>${esc(c.label)}</h3><p>${esc(desc)}</p>` +
      `<span class="card-cta">See the results &rarr;</span></a>`;
  });
  // Muted placeholder: signals the grid grows, promises nothing it cannot keep.
  cards.push(`<div class="home-card soon"><h3>Models</h3><p>Coming soon.</p></div>`);
  return cards.join("");
}

/* SPA navigation to any internal path (home cards, brand link, method link). */
function navigateTo(path) {
  applyState(decodeUrl(path, "", ""));
  syncUrl(true);
  ensureData().then(renderAll);
}
function wireNav(el) {
  el.addEventListener("click", (ev) => {
    if (ev.metaKey || ev.ctrlKey || ev.shiftKey) return; /* let new-tab clicks through */
    ev.preventDefault();
    navigateTo(el.getAttribute("href"));
  });
}

function renderHome() {
  const grid = document.getElementById("home-cards");
  if (!grid) return;
  grid.innerHTML = homeCardsHtml(state.data);
  grid.querySelectorAll("[data-nav]").forEach(wireNav);
}
/* Static home links (repo, method) + the header brand link (wordmark -> home):
   wired exactly once at boot. */
function initHomeLinks() {
  const repo = (state.data && state.data.repo) || "https://github.com/GetBusbar/benchmarking";
  const a = document.getElementById("home-repo");
  if (a) a.href = repo;
  document.querySelectorAll(".home-links [data-nav]").forEach(wireNav);
  const brand = document.getElementById("brand-link");
  if (brand) wireNav(brand);
}

/* ---- category nav + view tabs ----------------------------------------------- */
function viewPath(category, view) {
  return view && view !== DEFAULT_VIEW ? `/${category}/${view}` : `/${category}`;
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
  // Outgoing view must be read before state.view moves: reading it after made `leaving` always equal
  // the arriving view, so the mode memo below never stashed anything and silently discarded the mode
  // the URL had just decoded.
  const leaving = modeFamily(state.view);
  state.view = view;
  // Memory's data-derived Same default is seeded on arrival at memory, not globally at boot, so other
  // tabs keep the dialect default they declare (see seedMemorySameDialect).
  seedMemorySameDialect();
  const nx = modeOnArrival(leaving, view, state.mode, state.modeMemo);
  state.mode = nx.mode;
  state.modeMemo = nx.memo;
  // Category header/tab bar belong to the category view only; a body class hides them on home.
  document.body.classList.toggle("home", view === HOME_VIEW);
  // Performance/Streaming/Memory share one table container; matrix/method have their own.
  const containerId = TABLE_VIEWS.has(view) ? "view-table" : `view-${view}`;
  document.querySelectorAll(".tab").forEach((x) => {
    x.classList.toggle("active", x.dataset.view === view);
    x.setAttribute("href", viewPath(state.category, x.dataset.view));
  });
  document.querySelectorAll(".view").forEach((v) => v.classList.toggle("hidden", v.id !== containerId));
  // Cell chooser appears on memory only once the bundle carries per-cell windows (else all four
  // modes would show the same number).
  const chooser = document.getElementById("cell-chooser");
  if (chooser) chooser.classList.toggle("hidden",
    !CHOOSER_VIEWS.has(view) || (view === "memory" && !hasPerCellMemory(state.data)));
  // Bound selector belongs only to tabs whose numbers are read at a bound.
  const bound = document.getElementById("bound-chooser");
  if (bound) bound.classList.toggle("hidden", !BOUND_VIEWS.has(view));
  // Switching between table tabs changes columns/caption/filtering, so re-render the table.
  if (TABLE_VIEWS.has(view) && state.data) { renderFilters(); renderTable(); }
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
    needStream: st.needStream, needXlate: st.needXlate,
    // A missing/invalid decoded ?bound= leaves the default in place, never undefined.
    bound: st.bound === null || FRONTIER_BOUNDS_MS.includes(st.bound) ? st.bound : DEFAULT_BOUND_MS,
    mode: st.mode, sameDialect: st.sameDialect, sameDialectPinned: st.sameDialectPinned,
    xlateIn: st.xlateIn, xlateOut: st.xlateOut,
    cmp: st.cmp, cmpOpen: st.cmpOpen, drawer: st.drawer,
  });
}

/* Seed the Same dialect from the data (the identity cell the most gateways serve) for the memory tab
   specifically, not the whole state at boot - otherwise a deep link into performance/streaming (which
   share this dialect field) would have its dialect silently rewritten before rendering. A ?d= in the
   URL wins. Seeded on arrival at memory. */
function seedMemorySameDialect() {
  if (state.view !== "memory" || state.sameDialectPinned || !state.data) return;
  const w = widestDialect(state.data);
  if (w) state.sameDialect = w;
}

/* Drop selections that reference gateways no longer in data.json (removed
   entrants linger in shared URLs); a shrunken compare set must not leave the
   panel open on a partial table. */
function sanitizeState() {
  const gws = state.data.gateways;
  seedMemorySameDialect();
  state.cmp = state.cmp.filter((k) => gws.some((g) => g.key === k));
  if (state.cmp.length < 2) state.cmpOpen = false;
  if (state.drawer && !gws.some((g) => g.key === state.drawer)) state.drawer = null;
}

function renderAll() {
  renderCatNav();
  showView(state.view);
  renderHome();
  renderGateways();
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
  // Title the page from the URL before the fetch: state is fully decoded above, so the title is
  // knowable immediately rather than waiting on data.json (and staying generic on the failure path).
  updateTitle();
  ensureData()
    .then(() => {
      syncUrl(false); /* normalize: legacy #hash URLs -> clean path form */
      initTabs();
      initFilterControls();
      initThemeToggle();
      initHomeLinks();
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
      // View still titles itself on the failure path.
      updateTitle();
      document.querySelector("main").innerHTML =
        `<p class="muted">Could not load site data (${esc(err.message)}). Run <code>node site/gen-data.mjs</code> first.</p>`;
    });
}

if (NODE) {
  // Exports for the node smoke test (site/test.mjs).
  module.exports = {
    newState, encodeUrl, decodeUrl, viewPath, applyFilters,
    fmtStamp, fmtAge, stampWithAge, measuredBadge, engineBadge,
    drawSweep, niceStep, fmtTick, COLUMN_SETS, columnsFor, PERF_VIEWS, TABLE_VIEWS, VIEW_SORT, LANES, naText, stripRigPaths,
    cellState, matrixCellTip, cellPerfTip, passCell, xlateCell, streamCell, memCell, rssSparkline, hasTranslation, CATEGORIES, DEFAULT_CATEGORY, VIEWS,
    CHOOSER_MODES, MODE_LABELS, MODE_TIPS, chooserCellPerf, chooserDialects, chooserPerfCell, chooserCellStream, chooserStreamCell, chooserHasCell, deltaToPeak, cellPopFull,
    // memory cell chooser (Min | Max | Same | Custom, never Peak) + the matrix roster hole-closer.
    MEM_CHOOSER_MODES, CHOOSER_VIEWS, modesFor, defaultMode, resolveMode, modeFamily, modeOnArrival, memoryMode,
    perCellMemory, memoryCells, hasPerCellMemory, widestDialect, chosenMemory, memoryFor,
    idleAcrossCells, neverPlateaued, worstGrowth, memCellTip, neverPlateauedPill,
    idleStatic, memShape, memGrowing, memShaped,
    hasMatrixGrid, matrixFailureReason, matrixRoster, hasCost, costWindowConc, SHOW_GROWTH_VERDICT, selectedConc, concChoices, rungAt, rigResolutionPct, indistinguishable, tiedRuns, costSaturation, CHART_METRICS, chartRows,
    laneRecord, lanePathNote, perfSweepSeries, concAt,
    // Frontier: constants (mirrored from seal.mjs), the readers every surface goes through, and the
    // two renderers that make the curve's shape legible. Exported so the shape (the headline finding)
    // stays test-reachable.
    FRONTIER_BOUNDS_MS, DEFAULT_BOUND_MS, BOUND_CHOICES, BOUND_VIEWS, SORT_ALIASES,
    boundLabel, boundClause, boundColLabel, boundColId, selectedBound, fmtTail,
    frontierOf, frontierAt, frontierCell, frontierHeld, frontierFullRate, heldPct, frontierSpark, frontierBlock, boardFrontierScale,
    frontierChooserCell, frontierBoundCell, frontierShapeCell, frontierShapeTd, frontierCaption, selectBound,
    // #4: the gain factor's sort key and reference prose - exported so a test can distinguish
    // findings that used to render identically.
    heldSortKey, HELD_NOTHING_INDEX, HELD_REFERENCE, BOUND_GROUP_LABEL, theadHtml, colgroupHtml, colWidth, COL_WIDTHS,
    // #1: the lead/notes split, its flattener, and the dispatch every renderer goes through.
    captionText, captionFor, notesFold,
    // #6: version column's two halves - parseBuildVersion returns null for a stamp naming no version.
    parseBuildVersion, versionToken, versionBasis, measuredBuild, ROSTER_KEY,
    // #7: per-view document title, pure so checkable without a document.
    pageTitle, SITE_TITLE,
    // #5: idle axis floor, so the "flat must render flat" guard asserts against the real constant.
    IDLE_AXIS_MIN_SPAN, rssCurves, fmt2,
    // Memory tab's labelling + row-height helpers.
    idleShapeNote, recoveryTail, releaseMark, rssLifecycle, memCurveSummary, LIFECYCLE_IDLE_FRAC,
    definitionsFor, definitionsFold, DEFINITION_PREFIXES, LANE_DEFINITION_PREFIXES, METRIC_NOTES,
    colTested, gatewayBuild, gatewayHardware, runMode, laneAgeSummary,
    chooserCaption, chooserLead, streamingProvenance,
    memoryCaption, memWindows, boardMemWindows, memLoadCellLabel, memLoadRecipeTip, memDisclosure,
    canonicalPerf, canonicalXlate, canonicalStreaming, canonicalMemory, metric, mval, isEnvelope, caption, SWEEP_CAPTION, gatewayResultsJson, DEFAULT_VIEW, VIEW_LABELS, rosterRows, fmtStars,
    configCorrectionUrl, BENCH_REPO, fmtInt, fmtAdded,
    HOME_VIEW, homeCardsHtml,
    metricTd,
    // Pure functions the suite drives directly, each previously unreachable inside a renderer.
    rowComparator, VIEW_TIEBREAK, matrixDiagonal, bestIndex, laneServed, seedMemorySameDialect,
    // audit #21: rig-provenance footer stamp + the live state it reads.
    rigStamp, benchmarkVersionStamp, state,
    // Surfaces previously unreachable from a DOM-free suite: the drawer, the compare panel body, and
    // the one place a gateway's repo URL reaches an href.
    drawerHtml, compareBodyHtml, gwLink, recordShowsValues,
  };
} else {
  boot();
}
