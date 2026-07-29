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
// The three perf tabs each rank an INTERNALLY COHERENT path so a single sort is honest:
//   passthrough = openai->openai only (every gateway on the identical dialect, no translation)
//   translation = openai-in -> best non-openai egress (fixed fair ingress, egress varies)
//   streaming   = SSE passthrough (its own stall-gated ceiling)
// The board leads with a NEUTRAL ROSTER (the `gateways` overview: who is on the bench, in
// alphabetical order, no perf numbers) and the rankings come second; matrix + method round it
// out. `charts` folds into method.
// TAB BAR (matrix-sole-source): Gateways · Memory · Performance · Streaming · Protocol matrix · Method.
// `performance` is ONE cell-chooser-driven tab (Peak | Same | Custom picks which cell of the ONE 6x6
// run to show).
const VIEWS = ["gateways", "memory", "performance", "streaming", "matrix", "method"];
const VIEW_LABELS = { gateways: "Gateways", memory: "Memory", performance: "Performance", streaming: "Streaming", matrix: "Protocol matrix", method: "Method" };
// The default (bare /gateways) view: the roster overview.
const DEFAULT_VIEW = "gateways";
// The tabs whose columns read a PERF/STREAM cell of the one 6x6 run (Peak | Same | Custom).
const PERF_VIEWS = new Set(["performance", "streaming"]);
// The views that render the shared results table (#view-table).
const TABLE_VIEWS = new Set(["performance", "streaming", "memory"]);
// The views the CELL CHOOSER drives. Memory chooses its cell like every other lane, with its OWN
// mode set (below).
const CHOOSER_VIEWS = new Set(["performance", "streaming", "memory"]);
// Maps retired view names onto the current tabs so old shared links keep resolving. `translation`
// aliases to `performance` (its ?xin/?xout still decode into the Custom in/out below).
const VIEW_ALIASES = { results: "performance", charts: "method", peak: "performance", matched: "performance", passthrough: "performance", translation: "performance" };
// Each perf tab's default (and honest headline) sort column; a clean URL omits the sort when it
// equals this, and switching tabs snaps to it unless the URL pins another.
// Streaming defaults to added TTFT (asc), NOT streams-sustained: the sustained count saturates at the
// harness cap (1024 in the current field data) so it ties several gateways and breaks ties by name,
// floating a slow-TTFT gateway above a fast one at the same count. Added TTFT is the streaming-overhead
// discriminator that a user actually feels first and it does not saturate.
const VIEW_SORT = { performance: "rps20", streaming: "sttft", memory: "mempeak" };
// The cell-chooser modes shared by Performance + Streaming: which cell(s) of the ONE 6x6 run to show.
//   peak   — each gateway on its OWN best same-dialect diagonal (best_cell). Default. Shows a per-row pill.
//   same   — ONE picked dialect's diagonal (X→X) for every gateway. No pill (the dialect is in the control).
//   custom — any ingress→egress cell (incl. translation) for every gateway. No pill.
const CHOOSER_MODES = new Set(["peak", "same", "custom"]);
/* The MEMORY lane's own mode set: Min | Max | Same | Custom.
   There is deliberately NO Peak. Peak reads best_cell, which the harness selects by THROUGHPUT; using it
   for memory would select on one axis and report another - the exact defect per-cell measurement exists to
   remove - and it would arrive dressed as a UI control, so a reader could not see it. Min/Max ARE offered
   because they select on memory and report memory: real minima and maxima of the quantity in the column.
   Their candidate sets still differ per gateway (min-of-26 vs min-of-1), which is why the row shows the
   cell count, and why the two are offered together (Min flatters breadth, Max penalises it). */
const MEM_CHOOSER_MODES = new Set(["min", "max", "same", "custom"]);
// The modes a view offers, and the mode it lands on when none is pinned.
//
// MEMORY DEFAULTS TO MIN, NOT SAME. Same is the like-for-like comparison and it is the right tool
// when you want one shared cell, but it makes the DEFAULT view depend on which dialect happens to be
// the widest on a part-published board: a gateway that does not serve the chosen cell reads n/a and
// drops out, correctly and by design, so a reader arriving at the tab can see a row missing for a
// gateway that measured perfectly well on a cell it does declare. one-api declares exactly one cell
// (openai) and vanished from a board whose widest dialect was anthropic. Min shows every gateway on
// its own lowest steady-state cell, so nobody drops out of the default view, and the row states the
// size of the set the minimum came from so the comparison discloses its own basis.
function modesFor(view) { return view === "memory" ? MEM_CHOOSER_MODES : CHOOSER_MODES; }
function defaultMode(view) { return view === "memory" ? "min" : "peak"; }
/* Which chooser family a view belongs to. The perf lanes offer Peak/Same/Custom; memory offers
   Min/Max/Same/Custom. They overlap on Same/Custom but not on the mode most readers want, which is
   why a single carried-across `mode` cannot serve both. */
function modeFamily(view) { return view === "memory" ? "memory" : "perf"; }
/* resolveMode: coerce a mode onto a view that offers it. This is what a SHARED URL hits: a link carrying
   ?mode=peak that lands on the memory tab must NOT render a throughput-selected memory number, so it falls
   back to Same; the reverse (?mode=min on Performance) falls back to Peak rather than reading nothing. */
function resolveMode(mode, view) { return modesFor(view).has(mode) ? mode : defaultMode(view); }
/* memoryMode: THE choke point for the memory lane's mode. Every memory reader routes through it, so even a
   state hand-built with mode:"peak" (a stale in-memory state, a test, a future caller) cannot produce a
   peak-selected memory number - it reads Same instead. */
function memoryMode(st = state) { return MEM_CHOOSER_MODES.has(st.mode) ? st.mode : defaultMode("memory"); }
// The segmented control's copy, one entry per mode across both mode sets.
const MODE_LABELS = { peak: "Peak", min: "Min", max: "Max", same: "Same", custom: "Custom" };
const MODE_TIPS = {
  peak: "Each gateway on its own best same-dialect diagonal",
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
// Added-latency deltas are shown raw (no noise-floor smoothing). On the paced stream
// suite the per-frame value is noise-dominated and can flip sign run-to-run; the honest
// per-frame number comes from the CPU-bound stream suite, not from massaging this one.
const fmtAdded = fmtInt;
const fmt1 = (v) => v.toLocaleString("en-US", { minimumFractionDigits: 1, maximumFractionDigits: 1 });
// Streaming latency cells: the column is µs (headers say so), but several gateways land in the
// hundreds of ms where a bare "596,693" invites misreading. Annotate any value >= 1 ms with its
// ms equivalent ("596,693 (596.7 ms)"); the charts' auto-ms relabel tells the same story.
const fmtUsMs = (v) => (v >= 1000 ? `${fmtInt(v)} (${fmt1(v / 1000)} ms)` : fmtAdded(v));
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

/* Per-gateway freshness badge. Under matrix-sole-source each gateway is measured + published
   INDEPENDENTLY, so the board legitimately carries mixed per-gateway ages (one row measured today,
   another 3 weeks ago) - that is honest, not a bug. We surface each row's OWN measured_at ("measured 3d ago",
   full stamp in the tooltip) and, when gen-data set g.stale (its data aged past MAX_GATEWAY_AGE_DAYS),
   a greyed "stale" pill. Returns "" when the gateway has no measurement at all (renders nothing).
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

/* The board's own benchmark version, read defensively: this helper is called from measuredBadge(),
   which site/test.mjs drives under Node where there is no window and no live state. */
function boardBenchmarkVersion() {
  try {
    return (typeof state !== "undefined" && state.data && state.data.benchmark_version) || null;
  } catch {
    return null;
  }
}

/* WHICH HARNESS MEASURED THIS ROW.
   The engine commit already travelled into every row and nothing rendered it, so "which version of
   the benchmark produced this number" was answerable only by opening the JSON. A row measured by an
   older engine is not necessarily wrong, but it is not comparable to the rest of the board, and a
   reader deciding whether to trust a side-by-side needs to see that rather than be told.
   Current rows show the sha quietly; an older one is red and says what it should have been.
   Returns "" when the row carries no engine at all AND the board has none either - there is nothing
   to compare, so a badge would be noise. Pure; covered by site/test.mjs. */
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

/* gwLink(g): the gateway's name, linked to its repo when it has one and plain text when it does not.
   ONE PLACE, because there were FOUR and no test built a gateway with a hostile repo at any of them.
   The roster, the perf/streaming/memory table's name column, the drawer head and the protocol-matrix
   section each wrote out the same anchor by hand, so `g.repo` reached an href attribute at four
   independent sites and "is it escaped here" was four separate questions with four separate answers.
   gen-data validates the scheme on the way in (an https:// URL or null - see its `repo` field), which
   is the primary defence; this is the second one, and the reason it is a function is so that a single
   test can cover every href the board emits and a fifth site cannot open quietly. */
function gwLink(g) {
  const name = esc((g && g.display) ?? "");
  return g && g.repo ? `<a href="${esc(g.repo)}" target="_blank" rel="noopener">${name}</a>` : name;
}

/* The benchmarking repo where config corrections are filed. */
const BENCH_REPO = "https://github.com/GetBusbar/benchmarking";

/* configCorrectionUrl: a per-gateway deep link to a PRE-FILLED GitHub issue in the benchmarking repo,
   so anyone (not just maintainers) can propose a fix to a gateway's published OOTB config. Uses the
   config-correction issue-form template (?template=config-correction.yml) and pre-sets the title +
   the gateway field, encoding every param. GitHub issue Forms map ?<field-id>=<value> onto the form's
   fields, so `gateway=<display>` lands in the template's "gateway" input. Everything is
   encodeURIComponent'd, so a display name with spaces/specials can't break the URL. */
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

/* Absolute rig paths (the bench box's own filesystem: /home/ubuntu/.npm/..., file:///home/...)
   are harness noise inside captured diagnostics, not evidence a reader needs; leaking them into
   tooltips reads as sloppy. Scrub them to a neutral placeholder wherever a note is surfaced. */
const RIG_PATH_RE = /(?:file:\/\/)?\/(?:home|root)\/[^\s'"):,]+/g;
function stripRigPaths(s) {
  return String(s || "").replace(RIG_PATH_RE, "<rig path>");
}

/* STATUS_LABEL: the SHORT cell label for a served-flag that is a TOKEN rather than a boolean.
   `stream_served` publishes `true`, any of the engine's Absent tokens as a string, "not_probed", and
   `false` only as a legacy parse-only value (record.rs). Those tokens are machine vocabulary: the badge
   renders the token's MEANING and the full sentence rides on the tooltip (METRIC_NOTES, the same
   vocabulary the envelope reasons render through), because a raw `search_exhausted` on a public board is
   a leak of our own field names, not a disclosure. A token this table does not know renders as "not
   available" - which claims nothing - rather than as a guess at which of "never ran" or "refused" it is. */
const STATUS_LABEL = {
  not_measured: "not measured", not_probed: "not measured", not_served: "not served",
  untestable: "not testable", rig_limited: "rig-limited", search_exhausted: "search exhausted",
  harness_error: "harness error", below_resolution: "below resolution",
};
/* naText: compact honest label for a lane that was not served. The suites emit
   long diagnostic notes (passthrough evidence, launch errors); those must never
   be dumped as metric values or they blow the table layout wide open. The cell
   shows a short badge and the full note (rig paths scrubbed) travels in the
   title tooltip; the drawer shows the first line plus a folded Evidence block. */
function naText(j, flag, errKey) {
  if (!j) return { text: "not measured", note: "" };
  const note = stripRigPaths(j[errKey] || j.serve_error || "");
  let text = "not served";
  // A REFUSAL AND A LANE THAT NEVER RAN ARE DIFFERENT FINDINGS AND MUST NOT SHARE A LABEL.
  //
  // The served flag is not a boolean on every lane: StreamServed is `true`, `false`, or a STATUS TOKEN
  // ("not_measured", "not_probed", "untestable" - record.rs), and the recovered 2026-07-29 snapshots
  // carry seven cells of exactly that. Every non-true value used to fall through to "did not stream",
  // which asserts a MEASURED refusal - the gateway was offered stream load and framed none - about
  // cells the harness never offered anything to. That is the compare table making an accusation out of
  // a gap in its own coverage, and the two read identically on a screenshot.
  const status = j[flag];
  if (typeof status === "string")
    return { text: STATUS_LABEL[status] || "not available", note: note || METRIC_NOTES[status] || "" };
  // A lane the gateway never CLAIMED (manifest declares the capability 0, with a cited note) is
  // "not declared", never a failure - same rule as the matrix capability grid.
  if (j.xlate_declared === false) text = "not declared";
  else if (j.xlate_passthrough === true || note.startsWith("UNTRANSLATED passthrough")) text = "n/a (passthrough)";
  // "manifest defines no <hook>" means THIS harness did not implement that suite's probe for this
  // gateway (e.g. the governed suite is only wired for gateways whose manifest defines the hook).
  // That is "not tested", NOT "not supported": we must never assert a capability verdict about a
  // gateway we did not actually exercise (several here have native governance we simply did not probe).
  else if (note.includes("manifest defines no")) text = "not tested";
  // A boot/build failure is OUR environment failing to start the gateway, not the gateway refusing
  // a probe: it must read as "did not run", never as a capability verdict against the gateway. Same
  // honesty rule as the protocol matrix (status 000 / "failed to boot" / never became ready).
  else if (String(j.last_http_status || "") === "000" || /failed to boot|no such file|not listening|never became ready|build failed/i.test(note)) text = "did not run";
  // A MEASURED streaming refusal (answered, but never framed SSE): "did not stream", with the
  // evidence in the note/tooltip. Same wording family as the stream charts' "no SSE streaming".
  else if (flag === "stream_served") text = "did not stream";
  return { text, note };
}

/* laneVal: if the suite file exists but the served flag is false, surface a
   compact label (full note in .note); if the file is absent, "not measured". */
/* laneServed(j, flag): did this lane actually serve? Only `true` is served. `false` and every STATUS
   TOKEN the producer can put there (StreamServed's "not_measured" / "not_probed" / "untestable") are
   not, and each keeps its own wording through naText - reading "anything that is not literally false"
   as served is what let an unprobed cell render as a measured streaming refusal. A flag the record does
   not carry at all is treated as served: the legacy per-suite records predate the flag, and demoting
   them would turn every one of their published numbers into an n/a. */
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

/* ---- THE data-honesty reader (Design E §2.3) --------------------------------
   Every metric in data.json is a SEALED ENVELOPE ({value, certified, suppressed, reason?, note?, …})
   emitted by gen-data.mjs (see seal.mjs). The honesty gate lives UPSTREAM, at seal time; a suppressed
   metric has value:null and the raw number is GONE from the bundle. So the reader has NO gate logic:
   it cannot return an ungated value because there is none. This is the ONE accessor every surface reads
   a metric through.
     metric(env)        -> { v, text, na, note, source, env } ; v is null (na:true) when not shown.
   `fmt` formats the value; a suppressed/absent metric reads "n/a". */
function isEnvelope(x) { return x != null && typeof x === "object" && typeof x.certified === "boolean"; }
/* The envelope's machine token -> the sentence a reader sees. A MEASURED FAILURE (the gateway was
   offered the load and sustained none: a certified 0) and NOT MEASURED (no reading at all: null) are
   different states and must read differently. */
const METRIC_NOTES = {
  no_qualifying_ceiling: "served, but no tested load held p99 < 1 s at <0.1% errors (no qualifying throughput ceiling)",
  measured_failure: "MEASURED FAILURE: the gateway was offered the load and sustained none of it (a real 0, not an unmeasured cell)",
  mock_bound: "not shown: rig-limited — the harness's own ceiling bounded this number, so it is not a gateway reading",
  unverifiable: "not shown: this number could not be certified against the harness's own ceiling",
  not_measured: "not measured: no reading exists for this cell",
  // The engine's own absence reasons (measurement.rs Absent), carried through the seal since the
  // reason-flattening fix. below_resolution is handled in metric() as a display state, not a hole.
  below_resolution: "below measurement resolution: the comparison ran and the gateway's overhead was too small for this rig to detect (the best result this test can express)",
  rig_limited: "not shown: rig-limited, the harness's own ceiling bounded this number, so it is not a gateway reading",
  untestable: "not testable: the rig cannot pose this question for this dialect (a rig limit, not a gateway fault)",
  search_exhausted: "not shown: the search ran off the end of its range still improving, so any number would be a lower bound, not a ceiling",
  harness_error: "not shown: the harness itself failed here; this says nothing about the gateway",
  not_served: "the gateway does not serve this pairing",
  // NOT an absence and NOT a suppression: a PUBLISHED number that also carries what the comparison
  // against the mock found. The mock paces its stream deltas at a fixed interval, so its frames/sec is
  // a target it was told to produce; a gateway that lands on it kept up, which is the best outcome the
  // test can express - but it is a different statement from a number proven to sit below the rig's own
  // ceiling, and the bundle used to publish the two identically (seal.mjs PACED_MATCH).
  paced_match: "matched the harness's paced upstream: the gateway kept up with the rate the mock was told to produce (not a proven-unbounded reading)",
};
function noteText(tok) { return (tok && METRIC_NOTES[tok]) || tok || ""; }
/* The SHORT on-cell form of a measured zero's meaning, rendered under the number in the table so the
   state is visible without hovering (the full METRIC_NOTES sentence stays on the tooltip). Keyed by
   the envelope's own note token; a plain zero with no note (a genuine 0.0 reading, e.g. memory
   growth) renders bare, exactly as before. */
const ZERO_WHY = {
  no_qualifying_ceiling: "no load held the gate",
  measured_failure: "measured failure",
};
/* metricTd(cell, sc): the ONE `<td>` writer for every plain (non-render) column. A MEASURED ZERO says
   what it means ON the cell, not only in a hover tooltip: "0" beside a real maximum reads as "this
   gateway does nothing", when the envelope's own note says "no tested load held the qualifying gates"
   or "offered the load, sustained none". The ZERO_WHY short form rides under the number; the full
   sentence stays on the tooltip. A zero with no note renders bare, exactly as before. */
function metricTd(cell, sc = "") {
  if (cell.na)
    return `<td class="na${cell.failed ? " failcell" : ""}${sc}" title="${esc(cell.note || "")}">${esc(cell.text)}</td>`;
  const zeroWhy = cell.v === 0 && cell.env && ZERO_WHY[cell.env.note];
  return `<td class="${sc.trim()}"${cell.note ? ` title="${esc(cell.note)}"` : ""}>${esc(cell.text)}${
    zeroWhy ? `<span class="zero-why">${esc(zeroWhy)}</span>` : ""}</td>`;
}
function metric(env, fmt = fmtInt) {
  if (!isEnvelope(env) || env.value == null) {
    // BELOW RESOLUTION IS NOT A HOLE. The comparison ran; the difference was at or under what the
    // rig can resolve, which is the best outcome the test can express. It displays as "≈0", ranks
    // as 0 (equal-best on every lower-is-better sort), and carries the engine's own prose as the
    // tooltip. Rendering it as n/a turned a win into a blank, which is how APISIX published an
    // added-gap p99 with no p50 - impossible for one distribution, so the table looked broken.
    if (env && env.reason === "below_resolution")
      return { v: 0, text: "≈0", na: false, note: env.detail || noteText(env.reason), env };
    // A MEASURED FAILURE READS AS ONE, in red, with its counts - never as the same n/a an untested
    // cell gets. The engine's detail carries the evidence ("0 ok, 14201 fail" - one-api's c=1 leg
    // after the restart bug; "no stream frame arrived"), and the cell shows the digits so a
    // screenshot proves the measurement ran and the gateway failed it.
    // ONLY when the detail blames THE GATEWAY'S OWN LEG. The engine emits the identical sentence
    // shape for the direct-to-mock leg ("the direct-to-mock leg at c=1 was not clean: 0 ok, N
    // fail"), which is OUR reference rig failing, not the gateway - painting that red would accuse
    // the gateway of a failure it never had. A rig-side total failure renders as a plain n/a with
    // the full detail on the tooltip.
    const detail = env && env.detail;
    const okFail = detail && /the gateway leg.*?(\d+) ok, (\d+) fail/.exec(detail);
    if (okFail && Number(okFail[1]) === 0 && Number(okFail[2]) > 0)
      return { v: null, text: `failed · 0/${fmtInt(Number(okFail[2]))}`, na: true, failed: true, note: detail, env };
    if (detail && /no stream frame arrived from the gateway/.test(detail))
      return { v: null, text: "failed · 0 frames", na: true, failed: true, note: detail, env };
    return { v: null, text: "n/a", na: true, note: detail || noteText(env && env.reason), env: env || null };
  }
  // A CERTIFIED NUMBER CAN CARRY MORE THAN ONE THING WORTH SAYING, so the note is composed rather than
  // being whichever single token the envelope happened to have: the zero's meaning, the paced-match
  // signal, and (on the legacy fallback rows) a provenance stamp OF ITS OWN when this number came out
  // of a different run than the record around it - cpu_fps is measured by the streamcpu suite while its
  // record is stamped by the stream suite, and dating it to the wrong run is a claim, not a formatting
  // detail. Each is rendered only when the envelope actually carries it.
  const notes = [];
  if (env.note) notes.push(noteText(env.note));
  if (env.paced_match === true) notes.push(noteText("paced_match"));
  if (env.source && (env.source.build || env.source.measured_at))
    notes.push(`from a separate run than the rest of this record: build ${env.source.build || "?"}, measured ${env.source.measured_at || "?"}`);
  return { v: env.value, text: fmt(env.value), na: false, note: notes.join(" · "), env };
}
// mval: the bare displayable value of an envelope (null when suppressed/absent). For arithmetic
// (deltas, best-of ranking) where only the number matters. Never returns a suppressed number.
// A below-resolution absence ranks as 0, the same value metric() displays it as: the comparison ran
// and found nothing the rig could weigh, which is equal-best, not missing.
/* mcode(env): mval() for a metric whose value is a CODE rather than a magnitude.
   mval maps a `below_resolution` absence to 0, which is the honest reading of "smaller than we can
   measure" for a magnitude. For a code, 0 is a real value with a meaning of its own (shape 0 =
   oscillating), so that coercion would turn an unmeasured field into a positive claim. Everything else
   about the read is identical, so this defers to mval rather than reaching into the envelope itself. */
function mcode(env) {
  if (isEnvelope(env) && env.reason === "below_resolution") return null;
  return mval(env);
}

function mval(env) {
  if (!isEnvelope(env)) return null;
  if (env.value != null) return env.value;
  return env.reason === "below_resolution" ? 0 : null;
}

/* ---- provenance-driven captions (Design E §3.2) -----------------------------
   EVERY caption/label that names where a datum came from is rendered FROM the cell's `source.sweep`
   stamp through this ONE table — no caption string literal may hard-code a source token ("6x6",
   "matrix", "sweep", "suite"); the lint in check-consistency (C3) enforces that. A label cannot claim
   "6×6" unless the datum's stamp is a 6x6-* sweep. Keyed by source.sweep; receives the cell's path. */
const SWEEP_CAPTION = {
  "6x6-diagonal":        (p) => `${laneDialect(p && p.dialect)} passthrough — 6×6 diagonal cell`,
  "6x6-translation":     (p) => `${laneDialect(p && p.ingress)} in → ${laneDialect(p && p.egress)} out — 6×6 translation cell`,
  // The LEGACY single memory window: one fixed-duration load on the gateway's throughput-peak cell. It
  // still renders (older bundles carry it) and its caption still says peak cell, because that IS what that
  // record is - the honest label is how a reader tells it apart from the per-cell windows below.
  "6x6-memory-window":   ()  => `post-6×6 memory window (identical fixed load on the peak cell, fresh cold-restarted process)`,
  // The PER-CELL memory windows: one cold-started process per cell, load run until RSS is steady (or the
  // cap is hit). The cell is the workload, so the caption names it exactly like every other lane's does.
  "6x6-memory-diagonal": (p) => `${laneDialect(p && p.dialect)} passthrough - 6×6 memory window (cold start, load run to plateau)`,
  "6x6-memory-translation": (p) => `${laneDialect(p && p.ingress)} in → ${laneDialect(p && p.egress)} out - 6×6 memory window (cold start, load run to plateau)`,
  "6x6-stream-diagonal": (p) => `${laneDialect(p && p.dialect)} SSE stream — 6×6 diagonal cell`,
  "6x6-stream-translation": (p) => `${laneDialect(p && p.ingress)} in → ${laneDialect(p && p.egress)} out SSE stream — 6×6 translation cell`,
  "perf-suite":          (p) => `${laneDialect(p && p.dialect)} passthrough — perf suite (no 6×6 cell for this gateway yet)`,
  "xlate-suite":         (p) => `${laneDialect(p && p.ingress)} in → ${laneDialect(p && p.egress)} out — translation suite (no 6×6 cell for this gateway yet)`,
  "stream-suite":        (p) => `${laneDialect(p && p.dialect)} SSE stream — stream suite (legacy)`,
};
// caption(cell): the provenance label for a projected cell, rendered from its own source.sweep stamp.
// Throws if the stamp is absent/unknown (C3 asserts every displayed cell's stamp is in the table).
function caption(cell) {
  const sweep = cell && cell.source && cell.source.sweep;
  const render = SWEEP_CAPTION[sweep];
  if (!render) throw new Error(`caption: no SWEEP_CAPTION for source.sweep=${JSON.stringify(sweep)}`);
  return render(cell.path || {});
}

/* canonicalPerf: THE single passthrough perf record every surface reads (table, drawer,
   compare; charts.py reads the same best_cell from data.json). gen-data emits g.best_cell
   from the matrix per-cell sweep, or synthesizes it from the perf suite when no swept
   diagonal exists (source:"perf-fallback"). Only a legacy bundle with no best_cell at all
   falls back to the raw perf suite object (whose field names match). */
// canonicalPerf / canonicalXlate / canonicalStreaming: under the sealed envelope there is nothing left
// to "canonicalize" — the projected cell (g.best_cell / g.translation_cell / g.streaming) already carries
// one honest envelope per metric; a suppressed metric is {value:null,…} in the data itself. These thin
// wrappers just return that cell (or null), so the LANES accessors + check-consistency read the ONE
// canonical record. No gate logic here: the gate is upstream, at seal time (Design E §2).
function canonicalPerf(g) { return g.best_cell || null; }
function canonicalXlate(g) { return g.translation_cell || null; }
function canonicalStreaming(g) { return g.streaming || null; }
/* canonicalMemory: THE single memory record. gen-data projects it SOLELY from the matrix's dedicated
   post-6x6 memory window (g.memory_read, source:"matrix"): a fixed identical load on this gateway's
   peak cell (load_cell), measured on a fresh cold-restarted process. There is no synthetic-suite
   fallback. Returns a record with served + idle/peak/recovered + load_cell/load_recipe, or null. */
function canonicalMemory(g) {
  const m = g.memory_read;
  if (m) return { served: true, ...m };
  return null;
}

/* memLoadCellLabel: pretty "ingress → egress" for a memory record's load_cell ("ingress>egress"). */
function memLoadCellLabel(lc) {
  if (typeof lc !== "string" || !lc.includes(">")) return lc || "?";
  const [ing, eg] = lc.split(">");
  const L = (d) => (MATRIX_LABELS[d] || d || "?");
  return `${L(ing)} → ${L(eg)}`;
}
/* AUDIT #14: the memory windows are TUNABLE in the harness (MEM_IDLE_S / MEM_SETTLE_S, emitted as
   idle_window_s / recovery_window_s on the memory block). Every window label therefore RENDERS from the
   data — hard-coding "60 s" published the default as a fact even when the run used another duration.
   memWindows(m) reads one record; boardMemWindows() reads the board's records for the column headers /
   captions (which are not per-row), falling back to the 60 s default only when nothing states otherwise. */
const MEM_WINDOW_DEFAULT = 60;
function memWindows(m) {
  const idle = m && Number.isFinite(Number(m.idle_window_s)) ? Number(m.idle_window_s) : MEM_WINDOW_DEFAULT;
  const rec = m && Number.isFinite(Number(m.recovery_window_s)) ? Number(m.recovery_window_s) : MEM_WINDOW_DEFAULT;
  // The STEADINESS window (how long the RSS had to hold still before the plateau was believed) rides in
  // the load recipe, not beside the other two. Null when the record predates it: a caption then states
  // the settling time without claiming a confirmation length it does not know.
  const lr = m && m.load_recipe;
  const steady = lr && Number.isFinite(Number(lr.plateau_window_s)) ? Number(lr.plateau_window_s) : null;
  return { idle, recovery: rec, steady };
}
function boardMemWindows(data = (typeof state !== "undefined" ? state.data : null)) {
  // PER-CELL FIRST. The windows ride on the per-cell records; reading only the legacy per-gateway record
  // here would silently fall back to the 60 s DEFAULT on every per-cell bundle, republishing a hard-coded
  // duration as a fact about a run that may not have used it. Legacy bundles still answer through
  // g.memory_read.
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
/* memLoadRecipeTip: the fixed-load basis + peak cell, for the "Tested on" cell tooltip. */
function memLoadRecipeTip(m) {
  const r = m && m.load_recipe;
  const w = memWindows(m);
  const basis = r ? `identical fixed load: ${fmtInt(r.concurrency)} concurrent, ${fmtInt(r.payload_bytes)} B payload, ${fmtInt(r.duration_s)} s` : "identical fixed load for every gateway";
  return `peak cell ${memLoadCellLabel(m && m.load_cell)} — ${basis}, on a fresh cold-restarted process ` +
    `(${memWindowLabel(w.idle)} idle → load → ${memWindowLabel(w.recovery)} recovery)${memDisclosure(m)}`;
}
/* memDisclosure(m): the producer rides its HONESTY DISCLOSURES inside memory.protocol as text — an
   uncertified peak-cell basis, a payload mismatch, a failed fixed load (each of which is why a peak RSS
   came back NULL). Carrying that string in the bundle without ever rendering it would hide the reason a
   column reads n/a, so it is SURFACED wherever the memory record is attributed. Everything after the
   protocol's leading recipe sentence is a disclosure clause. */
function memDisclosure(m) {
  const p = m && typeof m.protocol === "string" ? m.protocol : "";
  const parts = p.split(";").slice(1).map((x) => x.trim()).filter(Boolean);
  return parts.length ? ` — DISCLOSED: ${parts.join("; ")}` : "";
}

/* memoryTestedRecord(g): the canonical memory record, made SELF-DESCRIBING for the ONE "Tested on"
   renderer (colTested). The memory window's cell rides on the record as load_cell ("ingress>egress")
   rather than as a matrix cell's .path, so this pins THAT SAME cell onto .path — no second source of
   truth, no bespoke memory pill. The record keeps its own source stamp (6x6-memory-window), so the pill's
   provenance is the MEMORY window's, never the perf chooser's cell (audit #1: describe the record shown).
   Null when the gateway has no memory record or no load_cell → the shared renderer paints NO pill. */
function memoryTestedRecord(g) {
  const m = canonicalMemory(g);
  const lc = m && m.load_cell;
  if (!m || typeof lc !== "string" || !lc.includes(">")) return null;
  const [ingress, egress] = lc.split(">");
  return { ...m, path: { ingress, egress, ...(ingress === egress ? { dialect: ingress } : {}) } };
}

/* ---- per-cell memory --------------------------------------------------------
   Memory is measured as a cold-started, plateau-terminated window on EVERY served cell. WHICH cell to
   show is a display choice the reader makes and can see (Min | Max | Same | Custom), never one the
   harness picks by throughput and hides.

   Everything below is null-safe by construction: the published bundles that predate per-cell measurement
   carry none of these fields, and the board must degrade to that shape rather than blank the tab. */
// perCellMemory: the memory window a served cell carries, or null. Same lookup shape as xlateMatrixCell.
function perCellMemory(g, ingress, egress) {
  const up = g.matrix && g.matrix.upstreams && g.matrix.upstreams[egress];
  const cell = up && up.cells && up.cells[ingress];
  return (cell && cell.served === true && cell.memory && typeof cell.memory === "object") ? cell.memory : null;
}
// memoryCells(g): every served cell this gateway has a memory window for, in a stable (egress, ingress)
// order so Min/Max tie-breaks are deterministic rather than object-key-order dependent.
function memoryCells(g) {
  const out = [];
  for (const egress of MATRIX_CELLS) for (const ingress of MATRIX_CELLS) {
    const mem = perCellMemory(g, ingress, egress);
    if (mem) out.push({ ingress, egress, mem });
  }
  return out;
}
/* hasPerCellMemory(data): does this BUNDLE carry per-cell memory at all? The board runs the memory lane in
   one of two shapes and says which: per-cell (chooser + steady state + growth) or LEGACY (the single
   post-6x6 window, no chooser). The switch is per BUNDLE, never per gateway: a gateway missing per-cell
   data in a per-cell bundle reads n/a, because substituting its old peak-cell number into a named mode
   would put a throughput-selected reading behind a memory-selected label. Memoised on the data object -
   every row of every memory column asks this question. */
const PER_CELL_MEM_CACHE = new WeakMap();
function hasPerCellMemory(data = (typeof state !== "undefined" ? state.data : null)) {
  if (!data || typeof data !== "object") return false;
  if (PER_CELL_MEM_CACHE.has(data)) return PER_CELL_MEM_CACHE.get(data);
  const yes = (data.gateways || []).some((g) => memoryCells(g).length > 0);
  PER_CELL_MEM_CACHE.set(data, yes);
  return yes;
}
// The data bundle a (possibly synthetic) state refers to; falls back to the live state's.
function stateData(st) {
  return (st && st.data) || (typeof state !== "undefined" ? state.data : null);
}
/* widestDialect(data): the identity cell the MOST gateways serve: the default for memory's Same mode.
   Derived from the data, never named: no gateway or protocol may be special-cased, and "the dialect most
   of the field can actually be compared on" is a property of the run, not an editorial choice. Ties break
   alphabetically so the answer is deterministic. Null when nothing is served (no data yet). */
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
/* memSteady(mem): the steady-state RSS of one cell's window, or null when it never plateaued. This is the
   value Min/Max SELECT on, so it is the same quantity the column REPORTS - the rule the old peak-cell
   memory number broke. A cell that never plateaued has no steady state to be the min or max of, so it is
   not a candidate (its growth rate is the finding, and it is reported as such). */
function memSteady(mem) { return mval(mem && mem.steady_state_rss_mib); }
/* chosenMemory(g, st): the per-cell memory record the memory lane shows, stamped through the SAME choke
   point every other lane's chosen record goes through (stampChosen), so the pill, the drawer and the
   tooltip all render its provenance from one caption table.
     min / max  → this gateway's lowest / highest steady-state RSS across the cells it serves
     same       → the chosen dialect's identity cell
     custom     → the chosen ingress→egress cell
   Never peak: memoryMode() cannot return it. Returns null when the gateway has no window for the chosen
   cell (Same/Custom) or no plateaued cell at all (Min/Max) - n/a, never a substituted cell. */
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
  // The candidate count travels ON the record: min-of-26 and min-of-1 are different-sized searches and the
  // row has to be able to say so (the bias Min/Max carry is disclosed, not designed away).
  return { served: true, ...stampChosen(pick.mem, g, pick.ingress, pick.egress, "memory-"),
    mem_candidates: cells.filter((c) => memSteady(c.mem) != null).length, mem_cells: cells.length };
}
/* memoryFor(g, st): THE memory record every memory column reads. Per-cell bundle → the chosen cell;
   legacy bundle → the single post-6x6 window, unchanged (that is what keeps the published board working
   until the field re-runs; the caption says which shape is on screen). */
function memoryFor(g, st = state) {
  return hasPerCellMemory(stateData(st)) ? chosenMemory(g, st) : canonicalMemory(g);
}
/* idleAcrossCells(g): idle is sampled COLD, before the first request, so no cell is involved in it and it
   stays OUTSIDE the chooser - valid for every gateway in every mode. With one sample per cell we publish
   the median plus the spread rather than picking a cell's sample to stand for the rest. */
function idleAcrossCells(g) {
  const vals = memoryCells(g).map((c) => mval(c.mem.idle_rss_mib)).filter((v) => v != null).sort((a, b) => a - b);
  if (!vals.length) return null;
  const mid = Math.floor(vals.length / 2);
  return { median: vals.length % 2 ? vals[mid] : (vals[mid - 1] + vals[mid]) / 2,
    min: vals[0], max: vals[vals.length - 1], n: vals.length };
}
/* neverPlateaued(g): this gateway's RSS never went steady on ANY cell it serves. That is the strongest
   statement the memory metric makes - the published number for such a gateway would describe when we
   stopped, not the gateway - so it is flagged at GATEWAY level (next to the name, in every mode) rather
   than being something a reader has to find by selecting the right cell. False when there is no per-cell
   data to judge: absence of measurement is not a verdict. */
function neverPlateaued(g) {
  // TRI-STATE, because the producer is deliberately tri-state. `plateaued: null` is a WITHHELD verdict,
  // not a negative one: the harness writes it when the cold-restarted process never opened its port, when
  // the fixed load stopped delivering, and when the trailing window held fewer than the four RSS samples
  // the steadiness test needs. `null !== true` quietly turned every one of those into "never settles" -
  // a named, permanent accusation on the board about a gateway the rig never actually watched. On macOS,
  // where no RSS is readable at all, that meant EVERY gateway on EVERY locally generated board. Only
  // cells that were genuinely judged may vote, and at least one must have been.
  const judged = memoryCells(g).filter((c) => c.mem.plateaued != null);
  return judged.length > 0 && judged.every((c) => c.mem.plateaued === false);
}
/* memShape(rec): HOW a window failed to settle - "climbing", "swinging", "releasing" - or "" when it
   settled, when the producer did not publish a shape, or when the record predates the field.

   "Never settles" describes two different gateways. One climbs without bound. The other swings around
   a level it keeps returning to, which is a garbage collector working, not a leak. Rendered under one
   phrase - NEVER SETTLES, in red, beside a rate the column calls a leak rate - the second gateway is
   accused of the first one's defect. The engine now separates them; everything below reads its verdict
   and never re-derives one from the series. */
function memShape(rec) {
  // mcode, not mval: 0 is a real shape code here, so an absence must not decay into "it swung".
  const c = mcode(rec && (rec.shape ?? rec.memory_shape));
  return c === 1 ? "climbing" : c === 0 ? "swinging" : c === -1 ? "releasing" : "";
}
/* memGrowing(g): this gateway is CLIMBING on at least one cell. The distinction the red pill turns on:
   an unsettled gateway is only accused when something is actually growing. A gateway whose every
   unsettled cell merely oscillates is reported, in neutral type, as what it is. Records with no shape
   published (older boards, withheld verdicts) do not vote either way - the pill falls back to the
   unshaped wording rather than guessing. */
function memGrowing(g) {
  return memoryCells(g).some((c) => memShape(c.mem) === "climbing");
}
/* memShaped(g): at least one unsettled cell told us its shape, so the shape-aware wording is available.
   Without this an all-oscillating gateway and a gateway on a board too old to carry shapes would render
   identically, and only one of them has actually been cleared. */
function memShaped(g) {
  return memoryCells(g).some((c) => c.mem.plateaued === false && memShape(c.mem) !== "");
}
// memoryUnjudged(g): how many of this gateway's measured cells had their verdict WITHHELD. The pill's
// wording depends on it: "on any cell this gateway serves" overclaims when some cells were never judged.
function memoryUnjudged(g) { return memoryCells(g).filter((c) => c.mem.plateaued == null).length; }
/* worstGrowth(g): the highest growth rate across this gateway's cells. When a cell hit the plateau cap
   this IS its leak rate, so the gateway-level flag can quantify itself instead of just asserting. */
function worstGrowth(g) {
  const vals = memoryCells(g).map((c) => mval(c.mem.growth_rate_mib_per_min)).filter((v) => v != null);
  return vals.length ? Math.max(...vals) : null;
}
/* memCellTip(rec): the "Tested on" tooltip for a PER-CELL record. The legacy record's tooltip
   (memLoadRecipeTip) describes the peak-cell window and stays for legacy rows; this one describes what a
   per-cell window actually did: did it settle, how long it took, and what it was still doing if not. */
function memCellTip(rec) {
  const bits = [];
  const r = rec && rec.load_recipe;
  bits.push(r ? `identical fixed load: ${fmtInt(r.concurrency)} concurrent, ${fmtInt(r.payload_bytes)} B payload, run until RSS is steady`
    : "identical fixed load for every gateway, run until RSS is steady");
  bits.push("cold-started for this cell (idle sampled before the first request)");
  if (rec && rec.plateaued === true) {
    // time_to_plateau_s is WHEN THE RSS WENT FLAT, not when the steadiness test finished confirming it
    // (the producer reports the trailing window's START for exactly that reason). 0 is a real answer:
    // the working set was already steady when the load began, so say that rather than "settled after 0 s".
    const t = Number(mval(rec.time_to_plateau_s));
    const w = memWindows(rec).steady;
    const conf = w ? ` (steady for the ${memWindowLabel(w)} that followed)` : "";
    bits.push(!Number.isFinite(t) ? "settled"
      : t <= 0 ? `steady from the moment the load started${conf}`
      : `settled after ${fmtInt(t)} s${conf}`);
  } else if (rec && rec.plateaued === false) {
    const gr = mval(rec.growth_rate_mib_per_min);
    const sh = memShape(rec);
    // The rate is the same number in all three cases; what it MEANS is not. Under a climb it is a leak
    // rate. Under a swing it is how fast the window happened to be moving when it closed, which is a
    // fact about the sampling instant and not about the gateway - so it is not called a leak there.
    const what = sh === "swinging"
      ? "NEVER SETTLED, but did not grow: RSS swung around a level it kept returning to"
      : sh === "releasing"
      ? "NEVER SETTLED: still RELEASING memory when the cap was reached, not growing"
      : "NEVER SETTLED: still growing when the cap was reached";
    bits.push(gr != null && sh !== "swinging" && sh !== "releasing"
      ? `NEVER SETTLED: still growing at ${fmt1(gr)} MiB/min when the cap was reached`
      : gr != null && sh === "swinging"
      ? `${what} (moving ${fmt1(gr)} MiB/min at the close, which is the swing, not a leak)`
      : what);
  }
  // Stated mode-neutrally: in Min/Max it is the size of the search, and in Same/Custom it is still the
  // context a reader needs for the row above it.
  if (rec && rec.mem_candidates != null)
    bits.push(`${fmtInt(rec.mem_candidates)} of this gateway's ${fmtInt(rec.mem_cells)} measured cells reached a steady state`);
  return `${bits.join("; ")}${memDisclosure(rec)}`;
}

/* passCell: the Passthrough tab reads ONLY the canonical record (g.best_cell). When best_cell
   exists it is THE record: a field it lacks reads n/a, never silently patched from a different
   source (that is exactly the numeric divergence this rule exists to kill). Only a gateway with
   NO best_cell at all (legacy bundle) falls back to its perf suite. */
function passCell(g, key, fmt) {
  // The chosen record's metric is a SEALED envelope; metric() reads it (n/a when suppressed/absent).
  // There is no gate here — a suppressed value is {value:null,…} in the data, so it CANNOT leak.
  return g.best_cell ? metric(g.best_cell[key], fmt) : { v: null, text: "n/a", na: true };
}

/* streamCell: the Streaming tab reads ONLY the canonical streaming record (g.streaming). Each metric is
   a sealed envelope; metric() reads it. A gateway that did not stream (no g.streaming) reads n/a. */
function streamCell(g, key, fmt) {
  const s = canonicalStreaming(g);
  return s ? metric(s[key], fmt) : { v: null, text: "n/a", na: true };
}
/* memCell: the memory columns read the record memoryFor() chose: the per-cell window for the chosen mode,
   or (legacy bundle) the single post-6x6 window. Each metric is a sealed envelope; metric() reads it. A
   gateway with no record for the chosen cell reads n/a; nothing is ever substituted from another cell. */
function memCell(g, key, fmt, st = state) {
  const m = memoryFor(g, st);
  return m ? metric(m[key], fmt) : { v: null, text: "n/a", na: true };
}

/* A throughput cell of 0 is a real, honest measurement, not a broken benchmark: the gateway
   served, but NO tested load level passed the qualifying gates (p99 < 1 s at <0.1% errors), so
   that run has no qualifying throughput ceiling. Distinct from sweep noise (see the caption and
   the check-consistency guard, which flags max=0 separately from a small inversion). The cell
   still shows "0" and this note travels in its title tooltip. */
const ZERO_RPS_NOTE = METRIC_NOTES.no_qualifying_ceiling;
function withZeroNote(cell) {
  return !cell.na && cell.v === 0 ? { ...cell, note: cell.note || ZERO_RPS_NOTE } : cell;
}

/* xlateMatrixCell: the perf object for a gateway's ingress->egress translation cell, straight from the
   matrix (upstreams[egress].cells[ingress]). Returns cell.perf when that exact pair is served and
   measured, else null. The Translation tab pins BOTH ends (state.xlateIn/xlateOut) so every row is the
   identical translation and the ranking is apples-to-apples. */
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
   The board runs the 6x6 matrix ONCE; Performance and Streaming are PICKS of that one run. The chooser
   state (st.mode + sameDialect/xlateIn/xlateOut) selects WHICH cell each gateway's row reads:
     peak   → the gateway's own best diagonal (best_cell); streaming = the projected diagonal g.streaming.
     same D → the D→D diagonal cell (every gateway on the identical dialect).
     custom → the xlateIn→xlateOut cell (any pair, incl. translation).
   Every mode reads the SAME per-cell records the matrix carries, gated by the SAME mock-bound honesty
   rules, so a value shows identically on the table, the matrix popup, and the drawer. A cell a gateway
   lacks (unserved / unmeasured / a metric the record lacks) reads n/a — never 0, never fabricated. */
// chooserCellPerf: the PERF object (metrics as sealed envelopes) for the currently-chosen cell of gateway
// g, or null when that cell is unserved/unmeasured. Chooser mode = cell SELECTION only, never gating (the
// gate is upstream at seal time). Peak reads best_cell (metrics + path/source at top); Same/Custom read
// the matrix cell's sealed .perf (metrics at top, no path/source — the caller stamps dialects).
function chooserCellPerf(g, st = state) {
  if (st.mode === "peak") return g.best_cell || null;
  const [ingress, egress] = chooserDialects(g, st);
  if (ingress == null) return null;
  const perf = xlateMatrixCell(g, ingress, egress);
  return perf ? stampChosen(perf, g, ingress, egress, "") : null;
}
/* stampChosen: THE choke point that makes EVERY chosen record self-describing. A raw matrix cell's sealed
   .perf/.stream carries no path/source (the CELL's coordinates are implicit in where it was looked up), so
   this stamps it ONCE, and every surface renders provenance through caption() from the same stamp
   (Design E §3.2). */
// `lane` is the sweep-key infix for the lane the record belongs to: "" (perf), "stream-", "memory-".
function stampChosen(rec, g, ingress, egress, lane = "") {
  const same = ingress === egress;
  const path = { ingress, egress, ...(same ? { dialect: ingress } : {}) };
  // The sweep KEY is COMPOSED from the cell's own shape (matrix lane + diagonal/translation), never
  // written as a caption literal — SWEEP_CAPTION stays the single home of the key vocabulary (C3), and
  // caption() throws loudly if this composition ever names a key the table does not render.
  const sweep = `6x6-${lane}${same ? "diagonal" : "translation"}`;
  return { path, source: { kind: "matrix", sweep,
    build: (g.matrix && g.matrix.build) || null,
    measured_at: (g.matrix && g.matrix.measured_at) || null }, ...rec };
}
// The (ingress, egress) dialects the chosen cell is measured on — used for the pill/labels + the popup.
function chooserDialects(g, st = state) {
  if (st.mode === "peak") { const d = g.best_cell ? g.best_cell.path.dialect : null; return d ? [d, d] : [null, null]; }
  // MEMORY'S MIN/MAX NAME A CELL ONLY THROUGH THE MEMORY CHOOSER'S OWN PICK. Before this branch they
  // fell into the Custom arm below and every other lane (drawer perf/stream, compare, the sweep
  // charts) silently rendered the STALE xlateIn/xlateOut pair - a cell the user never chose - while
  // lanePathNote captioned it as "the lowest steady-state cell the table shows".
  if (st.mode === "min" || st.mode === "max") {
    const m = chosenMemory(g, st);
    const p = m && m.path;
    return p ? [p.ingress ?? p.dialect ?? null, p.egress ?? p.dialect ?? null] : [null, null];
  }
  return st.mode === "same" ? [st.sameDialect, st.sameDialect] : [st.xlateIn, st.xlateOut];
}
// A perf-metric cell for the chosen cell. Reads the sealed envelope through metric(); a suppressed/absent
// value reads n/a. No gate here — the envelope carries the honesty decision (Design E §2.3).
function chooserPerfCell(g, key, fmt, st = state) {
  const p = chooserCellPerf(g, st);
  return p ? metric(p[key], fmt) : { v: null, text: "n/a", na: true };
}
// The chosen cell's STREAMING record (metrics as sealed envelopes). Per-cell streaming is only measured on
// the diagonal today (gen-data projects it to g.streaming), so:
//   peak     → g.streaming (the best diagonal's streaming).
//   same D   → g.streaming ONLY when the diagonal it was projected from IS D (else n/a: not measured here).
//   custom   → the cell's own sealed .stream when the matrix carries one (future per-cell streaming), else n/a.
function chooserCellStream(g, st = state) {
  if (st.mode === "peak") return canonicalStreaming(g);
  const [ingress, egress] = chooserDialects(g, st);
  if (st.mode === "same") {
    const cs = canonicalStreaming(g);
    return cs && cs.path && cs.path.dialect === ingress ? cs : null;   // only the diagonal it was measured on
  }
  // custom: a per-cell stream record if the matrix carries one for this exact pair (else n/a). The cell's
  // .stream is ALREADY sealed in-place by gen-data, so no re-gating is needed — envelopes carry the truth.
  const up = g.matrix && g.matrix.upstreams && g.matrix.upstreams[egress];
  const cell = up && up.cells && up.cells[ingress];
  const raw = cell && cell.served === true && cell.stream && cell.stream.stream_served === true ? cell.stream : null;
  // Stamped through the ONE choke point, so a per-cell TRANSLATION stream is captioned as a translation
  // stream, not relabelled a single-dialect passthrough (audit #1/#6).
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
// cellPath(rec): the {ingress, egress} a perf record was measured on. best_cell carries it under .path;
// a raw matrix .perf carries none (the caller pins ingress/egress onto the record before comparing).
function cellPath(rec) {
  if (!rec) return {};
  return rec.path || { ingress: rec.ingress, egress: rec.egress };
}
// Δ-to-Peak for a chosen cell vs the gateway's OWN best diagonal (best_cell): "+18% latency, -9% RPS".
// Returns "" for the peak cell itself, or when either reference number is missing. Honest by construction:
// mval() returns null for a suppressed/absent envelope, so a rig-bound RPS never enters the delta — there
// is no separate mock-bound flag to check, because the envelope already dropped the number.
function deltaToPeak(cellPerf, best) {
  if (!cellPerf || !best) return "";
  const cp = cellPath(cellPerf), bp = cellPath(best);
  if (bp.ingress === cp.ingress && bp.egress === cp.egress) return "";   // same cell as the reference
  const bits = [];
  const cLat = mval(cellPerf.added_latency_p99_us), bLat = mval(best.added_latency_p99_us);
  if (cLat != null && bLat != null && bLat > 0)
    bits.push(`${fmtPct((cLat / bLat - 1) * 100)} latency`);
  const cRps = mval(cellPerf.rps_sustained_20ms), bRps = mval(best.rps_sustained_20ms);
  if (cRps != null && bRps != null && bRps > 0)
    bits.push(`${fmtPct((cRps / bRps - 1) * 100)} RPS`);
  return bits.join(", ");
}

/* neverPlateauedPill(g): the gateway-level plateau verdict, rendered next to the name on the memory tab.
   Quantified where it can be: the worst growth rate across the gateway's cells says HOW fast, so the flag
   is a measurement rather than an accusation. Empty for every gateway that settled somewhere, and empty
   when there is no per-cell data to judge. */
function neverPlateauedPill(g) {
  if (!neverPlateaued(g)) return "";
  const gr = worstGrowth(g);
  const rate = gr != null ? `, still growing at up to ${fmt1(gr)} MiB/min` : "";
  // Say what was actually judged. A gateway can be flagged on the cells we could measure while others
  // were withheld, and the claim has to be the narrower one in that case.
  const un = memoryUnjudged(g);
  const scope = un > 0
    ? `on any cell we could measure it on (${fmtInt(un)} further cell${un === 1 ? "" : "s"} were not measured)`
    : "on any cell this gateway serves";
  // A gateway is only ACCUSED when something is climbing. One that never settled but only ever
  // oscillated gets the same information in neutral type: it is a real finding (no steady-state number
  // can be published for it) but it is not a leak, and the red pill said it was.
  const growing = memGrowing(g);
  const cleared = !growing && memShaped(g);
  const cls = growing || !memShaped(g) ? "noplateau-pill" : "noplateau-pill neutral";
  const label = cleared ? "never settles (no growth)" : "never settles";
  const why = cleared
    ? `RSS never went steady ${scope}, but it never grew either: it swung around a level it kept returning to, which is memory being reclaimed rather than leaked. No steady-state number is published for it because there is no single level to publish, not because it is climbing.`
    : `RSS never went steady ${scope}${rate}. Its memory under load is bounded by how long we ran the load, not by the gateway, so no steady-state number is published for it.`;
  return ` <span class="${cls}" title="${esc(why)}">${label}</span>`;
}

/* ---- column model ----------------------------------------------------------- */
/* get(g) returns {v, text, na}: v is the sortable value (null = none), text the cell
   text, na marks a muted "not measured / not served" cell. sortable:false columns
   (the compare checkbox) take no part in sorting. Columns are grouped into per-tab sets
   (COLUMN_SETS) so each perf tab ranks one coherent path; the shared leading columns
   (select / name) are reused across all three. Implementation language is NOT a perf
   column: the perf tabs are pure measurement (the "Tested on" pill stays, a measurement
   fact); language lives on the Gateways overview roster. */
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
    // No per-row date: the board is one atomic run (matrix-sole-source = one source of truth), so
    // every gateway shares a single timestamp — the board-wide "last benchmarked" (roster tab + home)
    // IS the freshness, and a per-row date is pure redundant bloat. Just the name.
    // The ONE exception is the memory tab's gateway-level plateau verdict: "RSS never went steady on ANY
    // cell" is a property of the gateway, not of whichever cell the chooser is on, and it is the strongest
    // statement this metric makes. Burying it in a cell would mean a reader has to select the right cell to
    // discover it, so it rides next to the name and is visible in every mode.
    return `<td class="name">${a}${st && st.view === "memory" ? neverPlateauedPill(g) : ""}</td>`;
  },
};
// The "Tested on" column: present in EVERY mode (identical column set across Peak/Same/Custom). It reads
// the CHOSEN cell's path (chooserDialects) so it always names the exact cell the row's numbers were
// measured on — Peak: each gateway's own peak dialect (varies per row); Same: the chosen dialect on every
// row; Custom: the chosen ingress→egress. The provenance disclosure (tooltip / fallback star) renders FROM
// the chosen cell's source stamp via caption() (Design E §3.2), never a hard-coded source string.
// "Tested on" must describe THE RECORD THE ROW ACTUALLY DISPLAYS. colTested(lane) binds the column to
// its own LANE's record, renders provenance through the ONE caption() path, and paints NO pill without
// a record, so a Streaming row can never advertise the Perf cell's provenance and a null record can
// never paint a pill.
// MEMORY joins the same choke point. In a per-cell bundle its lane record is the CHOSEN cell's window
// (Min/Max/Same/Custom), already stamped by stampChosen; in a legacy bundle it is the single post-6x6
// window with its own load_cell pinned as the path. Either way the pill names the cell the MEMORY
// measurement actually ran on, rendered by the SAME code as every other tab.
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
// A lane may append its OWN extra disclosure after the record's caption on the pill tooltip. Memory
// carries the load basis, what the window did (settled or still climbing) and the producer's honesty
// disclosures (memory.protocol). Dropping them when the memory column moved onto the shared pill would
// hide WHY a memory column reads n/a.
const LANE_TESTED_NOTE = { memory: (rec) => (rec && rec.load_cell ? memLoadRecipeTip(rec) : memCellTip(rec)) };
/* A lane may also append a plain-text SUFFIX after the pill. Min/Max are per-gateway extrema, and an
   extremum means nothing without the size of the set it came from: min-of-26 and min-of-1 are different
   searches, and the reader has to be able to see that without opening anything. */
const LANE_TESTED_SUFFIX = {
  memory: (rec, st = state) => {
    const mode = memoryMode(st);
    if ((mode !== "min" && mode !== "max") || !rec || rec.mem_candidates == null) return "";
    return `of ${fmtInt(rec.mem_candidates)} served`;
  },
};
// Lanes that take no part in sorting (memory's cell is an attribution, not a ranking, as before).
const LANE_TESTED_NOSORT = new Set(["memory"]);
/* recordShowsValues(rec): does this lane record put at least ONE number (or below-resolution ≈0) on
   the row? The pill's contract is all-or-nothing, in the owner's words: "either this cell is measured,
   and all data must be reported, or this cell wasn't tested and empty is expected. Not a combo." A
   record whose every envelope is empty (plano's memory cells on the 2026-07-28 board: an OpenAI pill
   over four n/a columns) must not advertise a measurement it does not have. */
// Envelope keys NO surface displays as a column or drawer metric. They must not satisfy the
// all-or-nothing test: a record whose only value is the harness's own direct-leg reading, or a
// plateau timing no column renders, would otherwise paint a pill (and keep idle alive) over a row
// whose every VISIBLE cell reads n/a - the plano shape back through a side door.
const UNDISPLAYED_ENVELOPE_KEYS = new Set([
  "time_to_plateau_s", "direct_c1_p99_us", "gateway_c1_p99_us",
  "gateway_c1_samples", "direct_c1_samples", "peak_rss_hwm_mib",
]);
function recordShowsValues(rec) {
  if (!rec || typeof rec !== "object") return false;
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
      // NO record → NO pill, and a record with NO displayable value is the same emptiness wearing a
      // costume. A row whose every column reads n/a must not advertise a measurement.
      if (!rec || !recordShowsValues(rec)) return `<td class="tested"><span class="muted">n/a</span></td>`;
      const p = cellPath(rec);
      const ing = p.ingress ?? p.dialect, eg = p.egress ?? p.dialect;
      if (ing == null) return `<td class="tested"><span class="muted">n/a</span></td>`;
      // The pill label: a passthrough (in==out) shows the single dialect; a translation cell shows in→out.
      const label = ing === eg ? (MATRIX_LABELS[ing] || ing) : `${MATRIX_LABELS[ing] || ing}→${MATRIX_LABELS[eg] || eg}`;
      // Provenance from THIS record's own stamp, through the ONE caption table. A live-fallback record
      // (a legacy suite, not the matrix) is starred so the disclosure is visible without hovering.
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
// sustainedChooserCell: the sustained@20ms cell for the chosen cell, carrying the winning-concurrency
// tooltip when the chosen cell records it (the concurrency travels INSIDE the sealed envelope). The
// concurrency is ALSO shown inline on the cell text ("N @ Y conc") — snapshot #65's Performance-tab ask.
function sustainedChooserCell(g, st = state) {
  const cell = withZeroNote(chooserPerfCell(g, "rps_sustained_20ms", fmtInt, st));
  const p = chooserCellPerf(g, st);
  const cc = p ? concAt(p.rps_sustained_20ms) : null;
  if (!cell.na && cell.v > 0 && cc != null)
    return { ...cell, text: `${cell.text} @ ${fmtInt(cc)} conc`,
      note: `Peaked at ${fmtInt(cell.v)} req/s with ${fmtInt(cc)} concurrent requests in flight - the load level that maximised sustained throughput under 20 ms LLM latency (higher concurrency added latency without more throughput).` };
  return cell;
}
// maxProxyChooserCell: the max-proxy cell for the chosen cell, showing its own peak concurrency inline
// ("N @ Y conc") next to the number — the sibling of sustainedChooserCell for the peak throughput.
function maxProxyChooserCell(g, st = state) {
  const cell = withZeroNote(chooserPerfCell(g, "rps_max_proxy", fmtInt, st));
  const p = chooserCellPerf(g, st);
  const cc = p ? concAt(p.rps_max_proxy) : null;
  if (!cell.na && cell.v > 0 && cc != null)
    return { ...cell, text: `${cell.text} @ ${fmtInt(cc)} conc`,
      note: `Peaked at ${fmtInt(cell.v)} req/s with ${fmtInt(cc)} concurrent requests in flight - the throughput ceiling against an instant mock.` };
  return cell;
}
const COLUMN_SETS = {
  // PERFORMANCE (Peak | Same | Custom): per-cell latency + throughput from the ONE 6x6 run. The columns
  // are IDENTICAL in every mode; the chooser only changes WHICH cell each row reads. The Tested-on column
  // is present in EVERY mode (renderTable does not drop it); it renders a pill only when the row's lane
  // actually has a record, and names that record's own provenance (audit #1/#13).
  performance: [
    COL_SEL, COL_NAME, COL_TESTED,
    { id: "lat50", label: "Added latency p50 (µs)", desc: false, title: "Gateway p50 minus direct-to-mock p50 at concurrency 1 on the chosen cell",
      get: (g) => chooserPerfCell(g, "added_latency_p50_us", fmtAdded) },
    { id: "lat", label: "Added latency p99 (µs)", desc: false, title: "Gateway p99 minus direct-to-mock p99 at concurrency 1 on the chosen cell",
      get: (g) => chooserPerfCell(g, "added_latency_p99_us", fmtAdded) },
    { id: "rps20", label: "Sustained RPS (20 ms upstream)", desc: true, title: "Sustained requests/sec on the chosen cell while the mock upstream holds every response for 20 ms, standing in for a real model's time to first token. The 20 ms is the UPSTREAM's delay, not a latency target the gateway is held to: the qualifying bar is p99 under 1 s with fewer than 0.1% errors. Hover a cell for the concurrency it peaked at.",
      get: (g, st = state) => sustainedChooserCell(g, st) },
    { id: "rpsmax", label: "Max proxy RPS", desc: true, title: "Throughput ceiling against an instant mock (p99 < 1 s, <0.1% errors) on the chosen cell. Shows the concurrency it peaked at.",
      get: (g, st = state) => maxProxyChooserCell(g, st) },
  ],
  // STREAMING (Peak | Same | Custom): per-cell SSE columns from the SAME run. Per-cell streaming is
  // measured on the diagonal today, so Same reads it only on the gateway's own measured diagonal and
  // Custom reads a cell's own stream when the matrix carries one — else n/a (honest, never fabricated).
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
    { id: "cpufps", label: "CPU-bound fps", desc: true, title: "Streaming relay throughput under an unpaced firehose (CPU-bound): sustained content frames/sec on the chosen cell. Higher is better.",
      get: (g) => chooserStreamCell(g, "cpu_fps", fmtInt) },
  ],
  // MEMORY (one row per gateway, cell-chooser driven with its OWN Min | Max | Same | Custom modes):
  // idle / steady-state / growth / recovered RSS (best = min), the cell the chosen window ran on, plus the
  // RSS curve. Reads the record memoryFor() chose; a gateway with no window for that cell reads n/a and
  // nothing is substituted for it. Columns marked perCellOnly are dropped on a bundle that predates
  // per-cell measurement (columnsFor), because a column of pure n/a is noise, not disclosure.
  //
  // The steady-state column keeps the id "mempeak" ON PURPOSE. The id is a URL contract (?sort=mempeak is
  // in every shared memory permalink and in the charts' deep links); renaming it with the semantics would
  // silently drop the sort out of every one of those links. The LABEL is what a reader sees, and it changed.
  memory: [
    COL_SEL, COL_NAME,
    // Tested on: the SAME pill renderer every other tab uses (colTested), bound to the MEMORY lane so it
    // names the cell this row's memory numbers came from, not the perf cell.
    COL_TESTED_MEMORY,
    { id: "memidle", label: "Idle RSS (MiB)", desc: false,
      title: () => (hasPerCellMemory()
        ? "Cold idle process RSS, before the first request is served. Sampled once per cell with no cell-specific work involved, so this is the median across those cold samples (hover for the spread) and it is valid in every mode. Lower is better."
        : `Cold idle process RSS: median over a ${memWindowLabel(boardMemWindows().idle)} window on a fresh cold-restarted process, before any load. Lower is better.`),
      get: (g, st = state) => {
        if (!hasPerCellMemory(stateData(st))) return memCell(g, "idle_rss_mib", fmt1, st);
        // THE ROW IS ALL-OR-NOTHING (owner's rule): a row whose chosen cell was not tested, or whose
        // chosen record puts no number on the row, is fully empty - idle, however cell-independent
        // its sampling, must not survive as one lone number on an otherwise-empty row advertising a
        // measurement the cell does not have. Pick a mode that serves the cell (Min/Max always do)
        // and idle appears with the rest.
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
    // GROWTH: ~0 once a gateway has settled, and the LEAK RATE when it never did. This is the most
    // informative thing the metric produces - it turns "did not plateau" from a missing value into the
    // headline finding - so it is a column of its own rather than a footnote on the steady-state n/a.
    { id: "memgrowth", label: "Growth (MiB/min)", desc: false, perCellOnly: true,
      title: "How fast RSS was still rising over the final window on the chosen cell. Around zero once the gateway has settled. If it never settled, this IS its leak rate under this load, and no steady-state number exists to report instead. Lower is better.",
      get: (g, st = state) => {
        const m = memoryFor(g, st);
        const c = memCell(g, "growth_rate_mib_per_min", fmt1, st);
        if (c.na || !m) return c;
        if (m.plateaued === false)
          return { ...c, text: `${c.text} (leak)`, note: "This cell never went steady: the load stopped at the cap with RSS still climbing at this rate, so there is no steady state to report and a longer load would have produced a larger number." };
        return { ...c, note: c.note || "Settled: RSS had stopped climbing when the load was terminated." };
      } },
    { id: "memrecov", label: () => `Recovered @${memWindowLabel(boardMemWindows().recovery)} (MiB)`, desc: false,
      title: () => `Process RSS at the end of the ${memWindowLabel(boardMemWindows().recovery)} recovery window after the fixed load stops — does the gateway release memory? Lower is better.`,
      get: (g, st = state) => memCell(g, "recovered_rss_mib", fmt1, st) },
    { id: "memcurve", label: "RSS curve", desc: false, sortable: false,
      title: () => (hasPerCellMemory()
        ? "RSS across one process lifecycle on the chosen cell: cold idle → load run to steady state → recovery."
        : `RSS across the memory window on one process lifecycle: ${memWindowLabel(boardMemWindows().idle)} cold idle → fixed load on the peak cell → ${memWindowLabel(boardMemWindows().recovery)} recovery`),
      // ALL-OR-NOTHING, like every other memory column: a record whose every envelope is empty must
      // not keep a live sparkline as the one surviving cell on an otherwise-n/a row - the raw
      // rss_series is not a sealed metric, so without this guard it outlived the rule.
      get: (g, st = state) => {
        const m = memoryFor(g, st);
        const shows = recordShowsValues(m) && Array.isArray(m.rss_series) && m.rss_series.length >= 2;
        return { v: null, text: "", na: !shows };
      },
      render: (g, st = state) => {
        const m = memoryFor(g, st);
        const spark = m && recordShowsValues(m) ? rssCurves(m) : "";
        return spark ? `<td class="memcurve">${spark}</td>` : `<td class="memcurve na">n/a</td>`;
      } },
  ],
  // Governance is RETIRED under matrix-sole-source: no tab, no column (busbar-only, non-default suite).
};
/* txt(x): a column/metric label or title, which may be a plain string OR a function rendering it from
   the live data (used where the wording depends on a tunable harness setting — audit #14). */
function txt(x) { return typeof x === "function" ? String(x() ?? "") : String(x ?? ""); }
/* The set of columns for a view; perf tabs use COLUMN_SETS, everything else has no table. A column marked
   perCellOnly exists only where the data can fill it: the published board still carries bundles measured
   before per-cell memory, and a growth column that reads n/a on all thirteen rows would be noise. */
function columnsFor(view, data = (typeof state !== "undefined" ? state.data : null)) {
  const cols = COLUMN_SETS[view] || COLUMN_SETS.performance;
  return hasPerCellMemory(data) ? cols : cols.filter((c) => !c.perCellOnly);
}
/* rowComparator(col, desc): the roster's row order for one column and one direction.
   THE NAME TIEBREAK IS NOT PART OF WHAT THE READER ASKED TO REVERSE. Toggling a column to descending
   reverses the RANKING; it does not mean "and also reverse the alphabet". The direction used to be
   applied to the whole comparison, so every group of equal-valued rows flipped its name order too - and
   on a column with dense ties (two gateways both below resolution, a lane where several rows read the
   same round number) the table visibly reshuffled rows whose values had not changed at all. Direction
   decides the value comparison; the name tiebreak is always ascending, so a tie sits still.
   Missing values always sink to the bottom, in both directions: an absent reading is not a low score. */
function rowComparator(col, desc) {
  return (a, b) => {
    const va = col.get(a).v, vb = col.get(b).v;
    const byName = a.display.localeCompare(b.display);
    if (va === null && vb === null) return byName;
    if (va === null) return 1;
    if (vb === null) return -1;
    const cmp = typeof va === "string" ? va.localeCompare(vb) : va - vb;
    if (cmp === 0) return byName;
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
  // pathNote renders FROM the record's source.sweep stamp via caption() (Design E §3.2) — no hard-coded
  // source string. The memory lane appends its cell attribution (load_cell) after the stamp caption.
  {
    key: "perf", label: "Latency & throughput", flag: "served", err: "serve_error",
    get: canonicalPerf,
    pathNote: (j) => j && j.source ? caption(j) : "",
    metrics: [
      { k: "added_latency_p50_us", label: "Added latency p50 (µs)", best: "min", fmt: fmtAdded },
      { k: "added_latency_p99_us", label: "Added latency p99 (µs)", best: "min", fmt: fmtAdded },
      // The two LEGS the added-latency figure above is the difference of. Added latency is a SUBTRACTION
      // (gateway leg minus direct-to-mock leg at concurrency 1), and until now the board published the
      // result while hiding both operands - a reader could not check the arithmetic, or see that a tiny
      // "added" number came from two large legs that nearly cancelled. Both are sealed into best_cell by
      // seal.mjs's UNGATED_LAT_FIELDS and were reaching the bundle unrendered.
      // direct_c1_p99_us carries best:null deliberately: it is the harness's own leg against the mock, the
      // same baseline for every row, so it is evidence rather than a contest (see bestIndex).
      { k: "gateway_c1_p99_us", label: "Gateway p99 @ c=1 (µs)", best: "min", fmt: fmtUsMs },
      { k: "direct_c1_p99_us", label: "Direct-to-mock p99 @ c=1 (µs)", best: null, fmt: fmtUsMs },
      // The operating concurrency travels INSIDE the sealed envelope (env.concurrency); the drawer shows
      // it as "(@ c=Y)" so the headline surfaces the load level its marked sweep peak sat at.
      { k: "rps_max_proxy", label: "Max proxy RPS", best: "max", fmt: fmtInt },
      { k: "rps_sustained_20ms", label: "Sustained RPS (20 ms upstream)", best: "max", fmt: fmtInt },
    ],
  },
  {
    key: "memory", label: "Memory", flag: "served", err: "serve_error",
    get: canonicalMemory,
    pathNote: (j) => {
      const base = j && j.source ? caption(j) : "";
      // A LEGACY record names its peak-cell basis; a per-cell record names what its window did (settled, or
      // still climbing at the cap) and how big the Min/Max search was. Both end with the producer's own
      // honesty disclosures (memory.protocol), which are surfaced, never carried silently.
      const note = j && j.load_cell
        ? `${base}, identical fixed load on ${memLoadCellLabel(j.load_cell)} (this gateway's peak cell)${memDisclosure(j)}`
        : (j && j.plateaued != null ? `${base}, ${memCellTip(j)}` : `${base}${memDisclosure(j)}`);
      return note;
    },
    // The RSS curve (idle→load→recovery, one process lifecycle). Renders ONLY when rss_series
    // exists (≥2 points); a bundle without a series → extra() returns "" and the drawer shows just the numbers.
    extra: (j) => rssCurves(j),
    metrics: [
      { k: "idle_rss_mib", label: "Idle RSS (MiB)", best: "min", fmt: fmt1 },
      // Both shapes are listed because both shapes are published: a per-cell record carries a steady state
      // (or none, when it never settled) and a growth rate; a legacy record carries the peak of a
      // fixed-duration load. The drawer drops whichever the record does not have, so no row is invented.
      { k: "steady_state_rss_mib", label: "Steady-state RSS (MiB)", best: "min", fmt: fmt1 },
      { k: "growth_rate_mib_per_min", label: "Growth (MiB/min)", best: "min", fmt: fmt1 },
      { k: "peak_rss_mib", label: "Peak RSS (MiB)", best: "min", fmt: fmt1 },
      // The kernel's own high-water mark, sealed and carried but unrendered. It is the independent check on
      // the sampled peak above: C7 already WARNS when hwm sits below peak (a sampler that outran the
      // kernel's accounting), and a reader could not see the pair the guard was comparing. Showing both
      // makes that disclosure legible instead of build-log-only.
      { k: "peak_rss_hwm_mib", label: "Peak RSS high-water (MiB)", best: "min", fmt: fmt1 },
      // Recovery: RSS 60 s after the load ends. Lower = released more of the peak (best: min). Absent on
      // pre-recovery bundles → the drawer/compare read n/a, exactly like any other lane field it lacks.
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
      // The RATE the sustained-streams bisect held, sealed under the SAME mock-bound flag as the count
      // above (audit #11) and carried into the bundle by sealStreamRecord - but never rendered, so the
      // board published a stream COUNT with no throughput behind it. Two gateways holding the same number
      // of streams at very different frame rates read as identical without this row.
      { k: "streams_sustained_fps", label: "Streams sustained (frames/s)", best: "max", fmt: fmtInt },
      { k: "cpu_fps", label: "CPU-bound fps (peak)", best: "max", fmt: fmtInt },
    ],
  },
  {
    key: "xlate", label: "Translation", flag: "xlate_served", err: "xlate_error",
    get: canonicalXlate,
    pathNote: (j) => j && j.source ? caption(j) : "",
    metrics: [
      { k: "added_latency_p50_us", label: "Added latency p50 (µs)", best: "min", fmt: fmtInt },
      { k: "added_latency_p99_us", label: "Added latency p99 (µs)", best: "min", fmt: fmtInt },
      { k: "rps_sustained_20ms", label: "Sustained RPS (20 ms upstream)", best: "max", fmt: fmtInt },
    ],
  },
];

/* laneRecord(l, g, st): the record a drawer/compare lane shows, CHOOSER-AWARE so it agrees with the
   TABLE in every mode. The perf + streaming lanes are cell-chooser driven (PERF_VIEWS): Peak reads the
   best diagonal, Same reads the D→D cell, Custom reads the in→out cell, exactly what the table columns
   render. The memory + xlate lanes are NOT chooser-driven (one matrix memory read; Translation is its
   own openai-in cell), so they read l.get. The returned perf record is GATED identically to canonicalPerf
   (suppressed RPS → null) and carries the chosen cell's source/dialect/ingress/egress so the pathNote
   names the SAME path the table pill does. */
function laneRecord(l, g, st = state) {
  if (l.key === "perf") {
    const p = chooserCellPerf(g, st);
    if (!p) return null;
    // The metrics are ALREADY sealed envelopes; nothing to gate here (the gate is upstream). The chosen
    // record is ALREADY self-describing (path + source) — chooserCellPerf stamps a raw matrix cell through
    // stampChosen, the ONE choke point — so there is no local provenance re-invention here (audit #1/#6).
    return { served: true, ...p };
  }
  if (l.key === "stream") return chooserCellStream(g, st);
  // Memory is chooser-driven too: canonicalMemory would show the drawer one cell while the table shows
  // another, so this reads the same chosen cell the table does.
  if (l.key === "memory") return memoryFor(g, st);
  return l.get ? l.get(g) : g[l.key];
}
/* perfSweepSeries(g, colors, st): the sweep-curve series for the CHOSEN cell (Peak/Same/Custom), used by
   the drawer + compare so the plotted curve reads the SAME cell the table + headline do. MOCK-BOUND GATE
   (finding 22): a metric whose headline is suppressed (rig-bound / unverifiable — mock_bound !== false)
   reads n/a on every honest surface, so its curve is DROPPED here too — a rig-bound sweep must not reveal
   on the curve a number the gate hides on the headline. Returns [] when the chosen cell is absent. */
function perfSweepSeries(g, colors, st = state) {
  const p = chooserCellPerf(g, st);
  if (!p) return [];
  const out = [];
  const add = (key, label, color) => {
    const env = p[key];
    // C5: the displayable number comes through the ONE accessor (mval), never a bare `.value` deref.
    // A suppressed headline is {value:null,…}: no number, so no curve (finding 22, now structural — the
    // sweep array is INSIDE the envelope and a suppressed envelope carries neither value nor sweep).
    const v = mval(env);
    if (v == null || !(env.sweep && env.sweep.length)) return;
    out.push({ label, color, sweep: env.sweep, peak: { rps: v, conc: concAt(env) } });
  };
  add("rps_sustained_20ms", colors.sustainedLabel || "sustained (20 ms upstream)", colors.sustained);
  add("rps_max_proxy", colors.maxLabel || "max proxy", colors.max);
  return out;
}
/* laneAgeNote(j): the age of the RECORD THIS LANE SHOWS, from its own source.measured_at (audit #8).
   The row badge ages the matrix, but a lane can project from a never-refreshed legacy suite (every
   streaming column today comes from the stream suite) — ageing that lane by the matrix stamp overstates
   its freshness. Empty when the record carries no stamp. */
function laneAgeNote(j, now = Date.now()) {
  const at = j && j.source && j.source.measured_at;
  const age = at ? fmtAge(at, now) : "";
  return age ? ` · measured ${age}` : "";
}
/* pathNote for a chooser-driven lane: ALWAYS routed through the lane's own pathNote, i.e. through
   caption(j), keyed by the record's source.sweep. The chosen record is stamped at the choke point
   (stampChosen) so caption() names the exact cell; the mode is appended as a UI hint only, never as
   provenance. */
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
    sortCol: "rps20",
    sortDesc: true,
    // The mode each chooser family was last left on, so crossing tabs restores the reader's own
    // choice rather than the coercion the other family forced. Never encoded into the URL: a link
    // carries ONE mode, for the view it names.
    modeMemo: { perf: "peak", memory: "min" },
    needStream: false,
    needXlate: false,
    // CELL CHOOSER (Performance + Streaming): which cell(s) of the ONE 6x6 run to show.
    //   mode "peak"   → each gateway's own best diagonal (best_cell); no dialect params.
    //   mode "same"   → sameDialect's diagonal (X→X) for every gateway.
    //   mode "custom" → xlateIn→xlateOut cell (any pair, incl. translation) for every gateway.
    //   mode "min"/"max" → MEMORY ONLY: this gateway's lowest / highest steady-state cell.
    mode: "peak",
    sameDialect: "openai",
    /* Was the Same dialect pinned by the URL? Memory's Same default is the WIDEST-COVERAGE dialect,
       computed from the data at boot, and a pinned ?d= must survive that seeding. */
    sameDialectPinned: false,
    // Custom mode: the pinned ingress->egress pair the whole table is projected on. Both ends are fixed
    // so every row is the identical cell (apples-to-apples); in==out is that dialect's passthrough, and a
    // gateway that does not serve this exact cell reads n/a (Performance) / is absent — kept honest.
    xlateIn: "openai",
    xlateOut: "anthropic",
    cmp: [],        /* gateway keys selected for compare, max 3 */
    cmpOpen: false, /* compare panel visible */
    drawer: null,   /* gateway key open in the drawer */
  };
}
const state = newState();

/* Capability filter toggles. Governance is RETIRED (matrix-sole-source): it is neither a filter,
   a column, nor a drawer section - the governed suite ran for a single entrant and is not a board metric. */
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
  // CELL CHOOSER encoding (Performance, Streaming, Memory). A clean URL omits the view's DEFAULT mode
  // (Peak on the perf lanes, Same on memory); Same carries the picked dialect (?mode=same&d=openai),
  // Custom the pinned pair (?mode=custom&in=anthropic&out=openai), so a link reproduces exactly the
  // cell(s) the view shows. Memory encodes its own modes (?mode=min / ?mode=max) and never Peak.
  if (CHOOSER_VIEWS.has(st.view)) {
    const mode = st.view === "memory" ? memoryMode(st) : st.mode;
    if (mode !== defaultMode(st.view)) p.set("mode", mode);
    if (mode === "same") {
      // Memory's Same default is the widest-coverage dialect, derived from the run rather than named, so
      // the pristine memory URL stays clean when the dialect IS that default.
      const isDefault = st.view === "memory" && st.sameDialect === widestDialect(st.data);
      if (!isDefault) p.set("d", st.sameDialect);
    } else if (mode === "custom") { p.set("in", st.xlateIn); p.set("out", st.xlateOut); }
  }
  const cat = CATEGORIES[st.category] ? st.category : DEFAULT_CATEGORY;
  const path = st.view && st.view !== DEFAULT_VIEW ? `/${cat}/${st.view}` : `/${cat}`;
  const qs = p.toString();
  return qs ? `${path}?${qs}` : path;
}

/* Parse a path + query (+ optional legacy #hash) back into state.
   The bare root (and any unknown first segment) is the HOME landing page; a
   known category segment enters that category, with unknown views falling back
   to its default (the roster). Pre-path-routing links carried everything in the hash
   (#view=matrix&sort=...); when the hash holds params, it wins over the query so
   old shared URLs keep resolving, and boot() then rewrites them to path form. */
function decodeUrl(pathname, search, hash) {
  const st = newState();
  const segs = String(pathname || "/").split("/").filter(Boolean);
  // Resolve a raw view token to a real view, honoring legacy aliases (results->passthrough,
  // charts->method) so old shared/deep links keep landing on a live tab.
  const resolveView = (v) => (VIEWS.includes(v) ? v : VIEW_ALIASES[v] || null);
  let i = 0;
  if (segs[i] && CATEGORIES[segs[i]]) {
    st.category = segs[i++];
    if (segs[i] && resolveView(segs[i])) st.view = resolveView(segs[i]);
  } else {
    // No (or an unknown) category segment: the site root, i.e. the HOME landing
    // page above the category nav. A legacy hash carrying view= (below) still
    // pulls the state back into the default category so old links keep landing.
    st.view = HOME_VIEW;
  }
  const legacy = String(hash || "").replace(/^#/, "");
  const p = new URLSearchParams(legacy.includes("=") ? legacy : String(search || "").replace(/^\?/, ""));
  const list = (k) => (p.get(k) || "").split("|").filter(Boolean);
  if (p.get("view") && resolveView(p.get("view"))) st.view = resolveView(p.get("view")); /* legacy hash form */
  st.q = p.get("q") || "";
  // Retired class/language chip filters: a stale ?cls= / ?lang= in an old shared
  // URL is IGNORED (never an error, never an invisible filter with no UI to clear).
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
    // No sort param: default to this view's headline column AND its natural direction. Leaving
    // sortDesc at the global default would sort added-latency defaults (sttft) descending, i.e.
    // worst-first. Derive the direction from the column's own `desc` flag.
    st.sortCol = VIEW_SORT[st.view] || "rps20";
    const dc = columnsFor(st.view).find((c) => c.id === st.sortCol);
    st.sortDesc = dc ? dc.desc !== false : true;
  }
  st.cmp = list("cmp").slice(0, 3);
  st.cmpOpen = p.get("cv") === "1" && st.cmp.length >= 2;
  st.drawer = p.get("gw") || null;
  // CELL CHOOSER decoding. New clean params (mode/d/in/out) plus the legacy translation params
  // (xin/xout, from the retired Matched tab) — a legacy ?xin/?xout link lands in Custom mode on the
  // pinned pair, exactly the cell the old Matched tab showed.
  const mode = p.get("mode");
  if (CHOOSER_MODES.has(mode) || MEM_CHOOSER_MODES.has(mode)) st.mode = mode;
  if (MATRIX_CELLS.includes(p.get("d"))) { st.sameDialect = p.get("d"); st.sameDialectPinned = true; }
  const cin = p.get("in") || p.get("xin");
  const cout = p.get("out") || p.get("xout");
  if (MATRIX_CELLS.includes(cin)) st.xlateIn = cin;
  if (MATRIX_CELLS.includes(cout)) st.xlateOut = cout;
  // A legacy Matched link (xin/xout with no explicit mode) means the pinned-pair Custom view.
  if (!CHOOSER_MODES.has(mode) && (p.get("xin") || p.get("xout"))) st.mode = "custom";
  /* Coerce the mode onto the view that received it. This is the shared-link case that matters: a
     ?mode=peak link opened on the memory tab must NOT render a throughput-selected memory number, because
     selecting on throughput and reporting memory is the defect per-cell measurement exists to remove. It
     lands on Same instead. (And ?mode=min on a perf tab lands on Peak rather than reading nothing.) */
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

function updateTitle() {
  if (NODE) return;
  if (state.view === HOME_VIEW) { document.title = "On the Bench · AI tool benchmarks"; return; }
  const cat = CATEGORIES[state.category] || CATEGORIES[DEFAULT_CATEGORY];
  const view = state.view !== DEFAULT_VIEW ? ` ${VIEW_LABELS[state.view] || state.view}` : "";
  document.title = `${cat.label}${view} · On the Bench · AI tool benchmarks`;
}

/* ---- filtering (pure) ------------------------------------------------------- */
function applyFilters(gateways, st) {
  const q = st.q.trim().toLowerCase();
  return gateways.filter((g) => {
    if (q && !g.display.toLowerCase().includes(q) && !g.key.toLowerCase().includes(q)) return false;
    if (st.needStream && !canonicalStreaming(g)) return false;
    if (st.needXlate && !hasTranslation(g)) return false;
    // Performance/Streaming are DELIBERATELY unfiltered across every chooser mode: every gateway appears
    // (fairness beats strict same-path — filtering a competitor out reads as hiding it), and a gateway
    // that does not serve the chosen Same/Custom cell simply reads n/a on that row (null metrics sort
    // last), never disappearing. Same principle for a measured streaming refusal (stream_served:false):
    // a muted "n/a" row sunk to the bottom, matching the stream charts' "no SSE streaming" bars.
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
  /* published-peak markers: a distinct labeled dot at each series' peak (its headline value at its
     operating concurrency). It sits ON the curve because the headline is max() over these points. */
  ctx.font = "11px Inter, sans-serif"; ctx.textAlign = "left"; ctx.textBaseline = "bottom";
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
    ctx.fillText(label, lx0, py - 6);
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
  // Mark the PUBLISHED peak on the RPS curve: a labeled dot at (peak.conc, peak.rps). By construction
  // that point is one of the probed sweep points (the headline is max() over this same array), so the
  // marker lands ON the curve and names the operating concurrency.
  const rps = usable.map((s) => ({ label: s.label, color: s.color,
    points: s.sweep.map((p) => ({ x: p.conc, y: p.rps })),
    mark: s.peak && s.peak.rps > 0 && s.peak.conc != null
      ? { x: s.peak.conc, y: s.peak.rps, label: `${fmtInt(s.peak.rps)} @ c=${fmtInt(s.peak.conc)}` } : null }));
  const p99 = usable.map((s) => ({ label: s.label, color: s.color, points: s.sweep.map((p) => ({ x: p.conc, y: p.p99_us })) }));
  // SAME x-axis: both charts share ONE concurrency domain (min..max across BOTH series) so they stack
  // and align vertically. Compute it from every probed concurrency on either chart.
  const allX = [...rps, ...p99].flatMap((s) => s.points.map((p) => p.x));
  const xDomain = allX.length ? [Math.min(...allX), Math.max(...allX)] : null;
  const o1 = { yLabel: "RPS", unit: "rps", xDomain, ...theme };
  const o2 = { yLabel: "p99 (µs)", unit: "µs p99", xDomain, ...theme };
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
// Short, one-idea-per-line captions (rendered on their own lines). Keep each line terse and concrete.
// Per-mode caption for the cell-chooser tabs: says in one line exactly which cell(s) of the ONE 6x6 run
// the table is showing, so a reader never has to guess what the numbers compare.
// The streaming numbers may be matrix-sourced (projected from the 6x6 diagonal cell) OR stream-fallback
// (a standalone stream suite, not the matrix). The caption must NOT hard-claim "the one 6x6 run" for
// streaming when the data is actually fallback (finding 12). Summarize the actual provenance across the
// gateways so the lead line is honest: "the one 6x6 run" only when streaming really came from the matrix.
function streamingProvenance(data) {
  const kinds = (data && data.gateways || []).map((g) => g.streaming && g.streaming.source && g.streaming.source.kind).filter(Boolean);
  if (!kinds.length) return { all: null };
  const allMatrix = kinds.every((k) => k === "matrix");
  const allFallback = kinds.every((k) => k !== "matrix");
  return { all: allMatrix ? "matrix" : allFallback ? "fallback" : "mixed" };
}
// The lead line for a chooser caption. For latency+throughput it is always the 6x6 matrix. For streaming
// it depends on provenance: matrix → "the one 6x6 run"; fallback → the standalone stream suite (honest);
// mixed → say so rather than over-claim.
/* laneAgeSummary(data, lane): the age of the NEWEST record this lane actually shows across the board,
   from g.lane_measured_at (audit #8). "" when no gateway stamps that lane. */
function laneAgeSummary(data, lane, now = Date.now()) {
  const stamps = ((data && data.gateways) || [])
    .map((g) => g.lane_measured_at && g.lane_measured_at[lane]).filter(Boolean)
    .map((a) => Date.parse(a)).filter(Number.isFinite);
  if (!stamps.length) return "";
  const age = fmtAge(new Date(Math.max(...stamps)).toISOString(), now);
  return age ? `, measured ${age}` : "";
}
function chooserLead(view, data) {
  if (view !== "streaming") return "Per-cell latency + throughput from the one 6x6 run.";
  const prov = streamingProvenance(data).all;
  if (prov === "matrix") return "Per-cell streaming from the one 6x6 run.";
  if (prov === "mixed") return "Streaming: some gateways from the 6x6 run, some from the standalone stream suite (per-row provenance in the drawer).";
  // fallback (or no data yet): the streaming figures come from the standalone stream suite, not the matrix.
  // Age this tab by the STREAM SUITE's own stamp: the row badge ages the matrix, which this tab does not
  // show, so quoting the matrix age here would overstate the freshness of every number on it.
  // laneAgeSummary contributes ", measured 23 hours ago", so the clause it attaches to must not already
  // end in "measured", or the two collide into "measured, measured 23 hours ago".
  return `Streaming from the standalone stream suite, not the 6x6 matrix${laneAgeSummary(data, "stream")}; each row's pill names the passthrough it ran on.`;
}
function chooserCaption(view, st, data) {
  const lead = chooserLead(view, data);
  if (st.mode === "peak")
    return [lead,
      "Each gateway on its OWN best same-dialect diagonal (best-of); the pill shows which dialect.",
      "Everyone appears. Pick Same for one shared dialect, or Custom for any ingress→egress cell."];
  if (st.mode === "same") {
    const d = MATRIX_LABELS[st.sameDialect] || st.sameDialect;
    return [lead,
      `Every gateway on the ${d}→${d} diagonal (pure forwarding, no translation).`,
      "A gateway that does not serve this dialect reads n/a and sinks to the bottom."];
  }
  const inL = MATRIX_LABELS[st.xlateIn] || st.xlateIn, outL = MATRIX_LABELS[st.xlateOut] || st.xlateOut;
  return st.xlateIn === st.xlateOut
    ? [lead,
       `Every gateway on the ${inL}→${outL} cell: same dialect, so this is passthrough (no translation).`,
       "A gateway that does not serve this cell reads n/a."]
    : [lead,
       `Every gateway on the ${inL}→${outL} cell: client speaks ${inL}, upstream speaks ${outL}, the gateway translates both ways.`,
       "Every row is the identical cell, so it is apples-to-apples; a gateway that does not serve it reads n/a."];
}
// AUDIT #14: the window durations render from the data (idle_window_s / recovery_window_s), never
// hard-coded — the harness makes them tunable and the caption must describe the run that happened.
function memoryCaption(data = state.data, st = state) {
  const w = boardMemWindows(data);
  const I = memWindowLabel(w.idle), R = memWindowLabel(w.recovery);
  if (!hasPerCellMemory(data)) {
    return [
      `An identical fixed load on each gateway's PEAK cell, measured on a fresh cold-restarted process (${I} idle → load → ${R} recovery). Same load recipe for every gateway, so it is apples-to-apples; only the cell differs (shown under Tested on).`,
      `Idle: cold-start RSS (median, no load). Peak: max RSS under the fixed load. Recovered @${R}: RSS ${R} after the load stops: does it release?`,
      "This run measured one cell per gateway, chosen by throughput, so there is no cell to choose between here. Lower is better on every column; a gateway with no served cell reads n/a.",
    ];
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
  const never = flagged.length
    ? ` ${fmtInt(flagged.length)} gateway${flagged.length === 1 ? "" : "s"} never settled on any cell (flagged by name): their memory under load is bounded by how long the load ran, not by the gateway.`
    : "";
  return [
    "Every cell gets its own cold-started process and its own load, run until RSS stops climbing rather than for a fixed time. Nothing is averaged across cells; the chooser picks which cell each row shows.",
    pick,
    `Idle is sampled cold, before the first request, so no cell is involved and it is valid in every mode. Growth is around zero once a gateway has settled and is its leak rate when it never did. Recovered @${R}: RSS after the load stops: does it release?${never}`,
    "Lower is better on every column. A gateway that does not serve the chosen cell reads n/a and sinks to the bottom; nothing is substituted from another cell.",
  ];
}
function updateTableCaption(view) {
  const el = document.getElementById("table-caption");
  if (!el) return;
  const lines = view === "memory" ? memoryCaption(state.data, state) : chooserCaption(view, state, state.data);
  el.innerHTML = lines.map((l) => esc(l)).join("<br>");
}
/* Memory tab: show the memory-recovery + memory-rss charts (charts.py PNGs) under the per-gateway table.
   Hidden on Performance/Streaming. Same lightbox behaviour as the main gallery. Absent PNGs → hidden. */
function renderMemoryCharts(view) {
  const box = document.getElementById("memory-charts");
  if (!box) return;
  const show = view === "memory";
  box.classList.toggle("hidden", !show);
  if (!show) return;
  const gallery = document.getElementById("memory-chart-gallery");
  const charts = (state.data.charts || []).filter((c) => /memory/i.test(c.file));
  if (!charts.length) { box.classList.add("hidden"); return; }
  const ordered = charts.slice().sort((a, b) =>
    (a.file.includes("top5_") - b.file.includes("top5_")) || a.file.localeCompare(b.file));
  gallery.innerHTML = ordered.map((c) =>
    `<figure data-src="/${esc(c.file)}"><img src="/${esc(c.file)}" alt="${esc(chartCaption(c.file))}" loading="lazy"><figcaption>${esc(chartCaption(c.file))}</figcaption></figure>`
  ).join("");
  gallery.querySelectorAll("figure").forEach((f) => f.addEventListener("click", () => {
    const lb = document.createElement("div");
    lb.className = "lightbox";
    lb.innerHTML = `<img src="${esc(f.dataset.src)}" alt="">`;
    lb.addEventListener("click", () => lb.remove());
    document.body.appendChild(lb);
  }));
}
function renderTable() {
  const { data } = state;
  const thead = document.querySelector("#results-table thead");
  const tbody = document.querySelector("#results-table tbody");

  // Which tab's columns to render. matrix/method have no table, so fall back to performance
  // (the section is hidden anyway) and never mutate the sort while off a table tab.
  const view = TABLE_VIEWS.has(state.view) ? state.view : "performance";
  const cols = columnsFor(view);
  // The Tested-on column is IDENTICAL in every mode (Peak / Same / Custom): the column set never changes
  // between modes, only WHICH cell each row reads. It renders from the chosen cell's own provenance stamp
  // (chooserDialects + source), so Peak names each gateway's own peak dialect (varies per row), Same names
  // the chosen dialect on every row, and Custom names the chosen ingress→egress — one column, one renderer.
  // Snap the sort onto this tab if the current column does not belong to it (e.g. after switching
  // tabs, or a cross-tab sort id arrived from a shared URL).
  if (TABLE_VIEWS.has(state.view) && !cols.some((c) => c.id === state.sortCol && c.sortable !== false)) {
    state.sortCol = VIEW_SORT[view] || cols[cols.length - 1].id;
    const dc = cols.find((c) => c.id === state.sortCol);
    state.sortDesc = dc ? dc.desc !== false : true;
  }
  updateTableCaption(view);
  renderMemoryCharts(view);

  thead.innerHTML = "<tr>" + cols.map((c) => {
    const sorted = state.sortCol === c.id;
    const dir = sorted ? `<span class="dir">${state.sortDesc ? " ▾" : " ▴"}</span>` : "";
    // AUDIT #14: label/title may be a FUNCTION so a column whose wording depends on a tunable harness
    // window (the memory windows) renders from the data instead of hard-coding the default.
    return `<th data-col="${c.id}" class="${sorted ? "sorted" : ""}${c.sortable === false ? " nosort" : ""}" title="${esc(txt(c.title))}">${esc(txt(c.label))}${dir}</th>`;
  }).join("") + "</tr>";

  let rows = applyFilters(data.gateways, state);
  const count = document.getElementById("row-count");
  if (count) count.textContent = `${rows.length} of ${data.gateways.length}`;

  const col = cols.find((c) => c.id === state.sortCol) || cols.find((c) => c.id === VIEW_SORT[view]) || cols[3];
  rows = rows.slice().sort(rowComparator(col, state.sortDesc));

  tbody.innerHTML = rows.map((g) =>
    `<tr data-gw="${esc(g.key)}">` + cols.map((c) => {
      const sc = c.id === state.sortCol ? " sorted-col" : "";
      if (c.render) {
        // render columns emit their own <td>; tint the sorted one by injecting the class.
        return sc ? c.render(g, state).replace("<td", `<td class="sorted-col"`).replace('class="sorted-col" class="', 'class="sorted-col ') : c.render(g, state);
      }
      return metricTd(c.get(g), sc);
    }).join("") + "</tr>"
  ).join("");
  // Empty-state line: filters that clear the table must never render as a bare header over nothing.
  // There is no "no gateway serves this pair" case to branch on. `view` is coerced to a TABLE_VIEWS
  // member at the top of this function, and "translation" is not one - it was retired when the pinned
  // pair became a chooser MODE rather than a tab, so that arm had been unreachable ever since. A branch
  // that cannot be taken is not a safety net; it is a claim about the UI that stopped being true, and
  // the rows are unfiltered by the chosen cell anyway: a gateway that does not serve the pinned pair
  // stays on the board reading n/a, deliberately, so the table can never be emptied by the chooser.
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
   Deliberately compact: search only. The class/language chip rows were retired
   (they burned vertical space above a 13-row table, and the roster tab already
   shows language + class); a stale ?cls= / ?lang= URL param is ignored. */

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
  // Cell chooser: the Peak | Same | Custom segmented control + its dialect dropdowns. Changing the mode
  // or a dialect re-projects every row onto the newly-chosen cell and re-encodes the URL.
  const opts = MATRIX_CELLS.map((d) => `<option value="${esc(d)}">${esc(MATRIX_LABELS[d] || d)}</option>`).join("");
  const same = document.getElementById("same-dialect");
  const cin = document.getElementById("cell-in");
  const cout = document.getElementById("cell-out");
  if (same) same.innerHTML = opts;
  if (cin) cin.innerHTML = opts;
  if (cout) cout.innerHTML = opts;
  // The mode buttons are RENDERED PER VIEW (renderFilters), because the tabs offer different mode sets:
  // the memory lane must never be able to paint a Peak button. One delegated listener therefore replaces
  // the per-button ones, which would have been bound to buttons that no longer exist after a re-render.
  const seg = document.getElementById("mode-seg");
  if (seg) seg.addEventListener("click", (ev) => {
    const btn = ev.target.closest(".seg-btn");
    if (!btn || !modesFor(state.view).has(btn.dataset.mode)) return;
    state.mode = btn.dataset.mode;
    renderFilters(); renderTable(); syncUrl(true);
  });
  const onSame = () => { state.sameDialect = same.value; renderTable(); syncUrl(true); };
  const onCustom = () => { state.xlateIn = cin.value; state.xlateOut = cout.value; renderTable(); syncUrl(true); };
  if (same) same.addEventListener("change", onSame);
  if (cin) cin.addEventListener("change", onCustom);
  if (cout) cout.addEventListener("change", onCustom);
}

function renderFilters() {
  document.getElementById("search").value = state.q;
  for (const [, name] of CAPS) { const el = document.getElementById(`f-${name}`); if (el) el.checked = state[CAPS.find(([, n]) => n === name)[0]]; }
  // Cell chooser: paint the buttons THIS view offers, mark the active one, and show only the dropdown(s)
  // that mode needs (Same → one dialect; Custom → in→out pair; Peak/Min/Max → none). Peak is simply not
  // rendered on the memory tab: the control cannot offer a selection the metric is not allowed to make.
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
/* A non-green cell is one of several very different things, and a neutral board must not
   conflate them. The harness now says which MACHINE-READABLY:
     served:"not_verified" (+ reason harness_boot_failure/suite_ceiling/mock_norecord) - the
       harness could not get the gateway into a fairly-testable state: never a red fail;
     served:"untestable" (+ reason no_base_url_override) - the gateway supports this pair in
       production but pins the real cloud host, so our mock is unreachable: a limit of this rig,
       not gateway incapability;
     served:"not_configured" (+ reason probe_failed, probe_note evidence) - PROBE-FIRST (matrix v3):
       the cell was probed and the round trip was not a correct translation; renders grey with the
       probe evidence, NEVER a red;
     served:false (+ reason wrong_answer) - LEGACY (pre-probe-first) red: the gateway served a
       declared cell and answered wrongly. New results never emit it; old ones still render.
   The prose-note heuristic below survives ONLY as a fallback for results that predate the
   machine-readable served/reason fields. */
const isHarnessGap = (cell) => {
  if (cell.served === "not_verified") return true;
  if (cell.reason) return false; // reason present and not not_verified: the verdict is explicit
  const note = (cell.verdict_note || "").toLowerCase();
  return cell.status === "000" || (note.includes("never served") && note.includes("warm-up"));
};
const cellState = (cell) =>
  cell.served === true ? ["served", "served"]
    : cell.served === "unprobed_auth" ? ["unprobed", "unprobed (auth)"]
      // PROBE-FIRST (matrix v3): every cell is probed; a failed probe is "not configured" with the
      // probe's evidence (probe_note) - it renders like the old declaration-grey, never as a red.
      : cell.served === "not_configured" ? ["notconf", "not configured"]
        // legacy results (pre-probe-first): grey by the drafted capability grid, not by a probe
        : cell.served === "not_configurable" ? ["notconf", "not declared"]
          : cell.served === "untestable" ? ["untestable", "untestable (mock limit)"]
            // The pairing is real (not a 404/501-shaped absence) but the gateway declined this
            // attempt - a genuine defect to disclose, not a capability boundary, so it is RED, not
            // grey, unlike every case above it.
            : cell.served === "failed" ? ["failed", "not served"]
              : isHarnessGap(cell) ? ["unverified", "not verified"]
              : ["failed", "not served"];

function laneStamp(j) {
  const bits = [];
  // build/measured_at travel INSIDE the provenance stamp (j.source) on a projected cell; g.matrix still
  // carries them at top level. Prefer the stamp, fall back to top-level.
  const build = (j.source && j.source.build) ?? j.build;
  const at = (j.source && j.source.measured_at) ?? j.measured_at;
  if (build) bits.push(build);
  if (at) bits.push(at);
  return bits.length ? `<div class="stamp muted">${esc(bits.join(" · "))}</div>` : "";
}

/* rssSparkline: a compact inline-SVG recovery curve (idle → peak → recovery) built from a memory
   record's rss_series [{t_s,rss_mib},…]. Returns "" when the series is absent or has < 2 points, so a
   pre-recovery bundle (no series) draws NOTHING — never a fabricated flat line or a zero baseline. The
   y-axis runs from IDLE to the series' own peak, so the height of the line is the growth under load
   and a flat gateway draws a flat line; a dot marks the last (recovered) sample. Same self-contained
   inline-SVG style as the matrix legend/cell swatches. */
// loadEndS: the second the load stopped and the recovery window began, from the record's own
// `load_s`. Drawn as a dotted rule so the curve says WHICH part is under load and which is the
// gateway with nothing asked of it - the whole point of the recovered figure beside it is that the
// reader can see whether the line came down after that mark, and until now nothing on the chart
// said where the mark was.
function rssSparkline(series, loadEndS = null, idleMib = null) {
  if (!Array.isArray(series) || series.length < 2) return "";
  const pts = series
    .filter((p) => p && typeof p.t_s === "number" && typeof p.rss_mib === "number")
    .sort((a, b) => a.t_s - b.t_s);
  if (pts.length < 2) return "";
  const W = 260, H = 56, PAD = 3;
  const ts = pts.map((p) => p.t_s), ys = pts.map((p) => p.rss_mib);
  const t0 = ts[0], t1 = ts[ts.length - 1], tspan = (t1 - t0) || 1;
  // THE AXIS RUNS 0 -> AT LEAST TWICE IDLE, AND ALWAYS FAR ENOUGH TO SHOW THE WHOLE CURVE.
  //
  // Two failure modes, and the fix has to avoid both.
  //
  // Auto-scaling to each curve's own range made every curve fill the height whatever it did: 1.2% of
  // growth (litellm-rust) drew the same cliff as 801% (kong). It exaggerated hardest for the gateways
  // that behaved best, and made two curves impossible to compare.
  //
  // But a HARD cap at twice idle is worse, and the board caught it: every one of bifrost's six cells
  // spends 100% of its samples above 2x idle (idle 153, rising through 525 and 665 to a peak of 875,
  // then falling back to 605). Clipping drew that as a flat line pinned to the ceiling - so the one
  // gateway on the board that never settles, and is labelled NEVER SETTLES with a 51 MiB/min leak
  // beside it, was the one whose curve showed no growth at all. Hiding the finding the row exists to
  // report is not a scale, it is a lie with a caption.
  //
  // So twice idle is a FLOOR on the axis, not a ceiling: a gateway that stays near idle still gets a
  // stable, honest frame instead of magnifying its own noise, and a gateway that climbs gets a frame
  // big enough to show the climb. Nothing is ever clipped.
  const dataMin = Math.min(...ys), dataMax = Math.max(...ys);
  const anchored = typeof idleMib === "number" && idleMib > 0;
  const ymin = anchored ? 0 : dataMin;
  const ymax = anchored ? Math.max(idleMib * 2, dataMax) : dataMax;
  const yspan = (ymax - ymin) || 1;
  const x = (t) => PAD + ((t - t0) / tspan) * (W - 2 * PAD);
  const y = (v) => PAD + (1 - Math.min(Math.max((v - ymin) / yspan, 0), 1)) * (H - 2 * PAD);
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
  return `<div class="rss-spark"><svg viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" role="img" ` +
    `aria-label="RSS curve from zero, idle ${anchored ? fmt1(idleMib) : "unknown"} MiB, peak ${fmt1(dataMax)} MiB over ${fmtInt(tspan)} s${restNote}">` +
    `<polyline points="${x(t0).toFixed(1)},${(H - PAD).toFixed(1)} ${x(t1).toFixed(1)},${(H - PAD).toFixed(1)}" ` +
    `fill="none" stroke="currentColor" stroke-opacity="0.15" stroke-width="1"/>` +
    // The idle level, drawn so "how far above idle" is a thing the eye can measure rather than infer.
    (anchored
      ? `<line x1="${PAD}" y1="${y(idleMib).toFixed(1)}" x2="${(W - PAD).toFixed(1)}" y2="${y(idleMib).toFixed(1)}" ` +
        `stroke="currentColor" stroke-opacity="0.5" stroke-width="1" stroke-dasharray="2 2">` +
        `<title>idle ${fmt1(idleMib)} MiB</title></line>`
      : "") +
    marks +
    `<path d="${path}" fill="none" stroke="currentColor" stroke-width="1.5"/>` +
    `<circle cx="${x(last.t_s).toFixed(1)}" cy="${y(last.rss_mib).toFixed(1)}" r="2.5" fill="currentColor"/>` +
    `</svg>` +
    `<div class="stamp muted">peak ${fmt1(dataMax)} → recovered ${fmt1(last.rss_mib)} MiB (${fmtInt(tspan)} s)</div></div>`;
}

/* rssCurves(mem): the memory window as TWO curves on ONE scale - what the process cost doing
   nothing, then what work cost it.
   Idle used to be a single number in a column, so a gateway that grew while completely idle looked
   identical to one that sat still, and the load curve's baseline was a value the reader had to take
   on trust. Both curves share the 0 -> 2x idle axis (see rssSparkline), so the idle line sits at the
   halfway mark in both and the two are directly comparable: a flat idle curve under a rising load
   curve is a healthy gateway, and a rising idle curve is a leak with nothing asked of it.
   Returns just the load curve when there is no idle series, which is every bundle measured before
   the idle window existed. */
function rssCurves(mem) {
  if (!mem || typeof mem !== "object") return "";
  const idle = mval(mem.idle_rss_mib);
  const load = rssSparkline(mem.rss_series, mval(mem.load_s), idle);
  const idleSeries = mem.idle_rss_series;
  if (!Array.isArray(idleSeries) || idleSeries.length < 2) return load;
  // The idle window carries no load boundary to mark, so no dotted rule: the whole window IS idle.
  const idleCurve = rssSparkline(idleSeries, null, idle);
  if (!idleCurve) return load;
  const verdict = idleStatic(mem);
  return `<div class="rss-pair">` +
    `<div class="rss-half"><div class="rss-label muted">idle${verdict ? ` · ${esc(verdict)}` : ""}</div>${idleCurve}</div>` +
    `<div class="rss-half"><div class="rss-label muted">load → recovery</div>${load}</div>` +
    `</div>`;
}

/* idleStatic(mem): what the idle window itself did, as a phrase. The engine judges it with the same
   plateau test the load window uses and publishes the verdict plus its rate, so this only has to
   render what was decided - it never re-derives the verdict from the series. */
function idleStatic(mem) {
  const st = mval(mem.memory_idle_static ?? mem.idle_static);
  if (st == null) return "";
  if (st === 1) return "steady";
  const rate = mval(mem.memory_idle_growth_rate_mib_per_min ?? mem.idle_growth_rate_mib_per_min);
  // An idle window that swings is the one place a wave is genuinely uninteresting - nothing is being
  // asked of the gateway - so saying "growing" there is simply wrong, not merely harsh.
  // mcode for the same reason as memShape: 0 is a real code, not an unmeasured magnitude.
  const sh = mcode(mem.idle_shape ?? mem.memory_idle_shape);
  if (sh === 0) return "swinging, not growing";
  if (sh === -1) return "releasing";
  return rate != null ? `growing ${fmt1(rate)} MiB/min` : "growing";
}

/* drawerHtml(g, st): the whole drawer for one gateway, as a string.
   `st` is threaded through (it defaulted to the module-level state) so a test can drive the drawer in a
   chosen mode without mutating live state - the same shape every other renderer here already has. This
   surface had NO test of any kind: the clause that keeps a MEASURED FAILURE visible among the metrics
   could be deleted and the suite stayed green, which would have silently removed a gateway's worst
   result from the one place a reader goes for evidence. */
function drawerHtml(g, st = state) {
  const langC = LANG_COLORS[g.lang] || LANG_COLORS.Other;
  // The gateway's OWN freshness stamp in the drawer head: measured_at + a stale badge when flagged,
  // the same per-gateway signal the table row shows (independent update cadences, made honest).
  const badge = measuredBadge(g);
  let h = `<header class="drawer-head">
    <h3>${gwLink(g)}</h3>
    <div class="chips"><span class="cls-chip">${esc(g.cls || "Gateway")}</span>
    <span class="lang-chip" style="background:${langC}">${esc(g.lang)}</span></div>
    ${badge ? `<div class="drawer-measured">${badge}</div>` : ""}
  </header>`;

  // AUDIT #7: the hardware stamp of the DISPLAYED basis (the matrix), not a deleted legacy suite object.
  const hw = gatewayHardware(g), arch = gatewayArch(g);
  if (hw) h += `<p class="stamp muted">${esc(hw)}${arch ? ` (${esc(arch)})` : ""}</p>`;

  for (const l of LANES) {
    // CHOOSER-AWARE: the perf + streaming lanes read the SAME chosen cell the table shows in the
    // current Peak/Same/Custom mode (laneRecord), so drawer/table/compare agree in every mode; the
    // memory + xlate lanes are not chooser-driven and read their canonical accessor. The raw suite
    // object is only the legacy fallback inside the accessor itself.
    const j = laneRecord(l, g, st);
    h += `<section class="drawer-lane"><h4>${esc(l.label)}</h4>`;
    if (!j) h += `<p class="muted">not measured</p>`;
    else if (!laneServed(j, l.flag)) {
      // A multi-line diagnostic (e.g. a captured stack trace) must not dump ~25 raw lines into
      // the drawer: show the first line as the verdict, fold the rest into a collapsed Evidence
      // block, and scrub absolute rig paths (harness noise, not evidence).
      // The fallback headline comes from naText, not from the literal "not served": with no error note
      // to show, a lane the harness never probed would otherwise announce a refusal it never observed.
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
      // Each metric is a sealed envelope; metric() reads it (a suppressed metric is absent, reads nothing
      // here since we filter na). The operating concurrency travels INSIDE the envelope (env.concurrency).
      // A MEASURED FAILURE stays a row (red, with its counts) rather than vanishing with the
      // genuinely-absent metrics, and a measured zero carries its short reason - the drawer must
      // tell the same story the table does, or the two surfaces disagree about the same envelope.
      h += `<dl>` + l.metrics.map((m) => ({ m, c: metric(j[m.k], m.fmt) })).filter((x) => !x.c.na || x.c.failed).map(({ m, c }) => {
        if (c.failed)
          return `<div><dt>${esc(txt(m.label))}</dt><dd class="failtext" title="${esc(c.note || "")}">${esc(c.text)}</dd></div>`;
        const conc = c.env && c.env.concurrency;
        const cc = conc != null && c.v > 0 ? ` (@ c=${fmtInt(conc)})` : "";
        const zeroWhy = c.v === 0 && c.env && ZERO_WHY[c.env.note];
        return `<div><dt>${esc(txt(m.label))}</dt><dd${c.note ? ` title="${esc(c.note)}"` : ""}>${esc(c.text + cc)}${
          zeroWhy ? ` <span class="muted">(${esc(zeroWhy)})</span>` : ""}</dd></div>`;
      }).join("") + `</dl>` + (l.extra ? l.extra(j) : "") + `${laneStamp(j)}`;
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

  h += `<section class="drawer-lane"><h4>Throughput sweeps</h4>` +
    `<p class="lane-note muted">Every point is a real probe; the search sweeps then bisects to the peak; the marked dot is the published number at its operating concurrency. The headline numbers above are that same marked peak.</p>` +
    `<div id="drawer-sweeps" class="sweeps"></div></section>`;

  /* OOTB config artifact: the exact as-shipped default config this gateway ran from (pointed at the
     mock). Monospace, scrollable, copy-friendly. Absent (not-yet-wired gateway) → "not published".
     A per-gateway "Suggest a correction" link opens a pre-filled GitHub issue so anyone — not just
     maintainers — can propose a fix; the published config is a best-effort OOTB attempt. */
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
  // ── Download results ──────────────────────────────────────────────────────────────────────────
  // The downloadable per-gateway artifact IS the matrix result: its full 6x6 cell matrix (with the
  // per-cell perf + streaming), the one memory read, the OOTB config, and the build/version stamp —
  // the gateway's COMPLETE record from data.json. Client-side blob, no server (see openDrawer's
  // [data-results-download] handler). Styled like the config Copy button.
  h += `<section class="drawer-lane"><h4>Results</h4>` +
    `<p class="lane-note muted">The gateway's complete record — the full 6×6 matrix (per-cell perf + streaming), the memory read, the OOTB config, and the build stamp.</p>` +
    `<button type="button" class="results-download" data-results-download title="Download this gateway's full results as JSON">Download results (JSON)</button>` +
    `</section>`;
  return h;
}

/* The per-gateway results artifact: the gateway's COMPLETE record from data.json (matrix 6x6 cells +
   memory + OOTB config + build/version). Returned as pretty JSON for the client-side download. */
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
  // CHOOSER-AWARE + GATED: the drawer curve reads the SAME chosen cell the headline rows + table read
  // (perfSweepSeries honors the Peak/Same/Custom mode), so the marked peak on the curve IS the published
  // rps_max_proxy / rps_sustained_20ms at its operating concurrency for THIS mode's cell — and a
  // mock-bound-suppressed metric draws no curve (finding 22), never revealing a number the gate hides.
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

/* bestIndex(vals, best): the indices of the best value in a compare row - EVERY index holding it, not
   the first one to reach it. A tie is a tie: two gateways both below resolution on a metric both rank
   as 0, which is the same reading, and highlighting only the leftmost of them draws a distinction the
   measurement explicitly says it cannot make. Returns a Set (empty when there is no contest to call). */
function bestIndex(vals, best) {
  // best == null means the row is EVIDENCE, not a contest: direct_c1_p99_us is the harness's own
  // direct-to-mock leg, a property of the rig rather than of any gateway, so crowning a "winner" on it
  // would invent a ranking out of measurement noise on a baseline every row shares. Without this the
  // `best === "min" ? ... : ...` ternary would silently fall through to max and highlight one anyway.
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

/* compareBodyHtml(gws, st): the ENTIRE compare panel as a string, given the gateways being compared.
   IT IS SPLIT OUT SO IT CAN BE TESTED AT ALL. renderCompare reaches for document.getElementById on its
   first useful line, the suite has no DOM, and so the compare table - the surface whose whole job is to
   put three gateways' numbers side by side and call a winner - was structurally unreachable by every
   test in the suite. Not under-tested: untestable. That is how a lane comes to disagree with the table
   without anything going red, and it is exactly the divergence the "table == drawer == compare" tests
   exist to forbid on the other two surfaces.
   The DOM half stays in renderCompare (find the panel, set innerHTML, draw the canvases); everything
   that DECIDES anything - which record each lane reads, which cell wins, how a failure or a suppressed
   metric renders - lives here, as a pure function of (gateways, state). */
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
    /* CHOOSER-AWARE: perf + streaming read the SAME chosen cell the table shows in the current
       Peak/Same/Custom mode (laneRecord); memory + xlate read their canonical accessor. So compare
       can never disagree with the table in any mode. Skip the whole lane only when no gateway measured
       it at all; an all not-served lane still renders rows so the header is never left bare. */
    const recs = gws.map((g) => laneRecord(l, g, st));
    if (recs.every((j) => !j)) continue;
    h += `<tr class="lane-row"><td colspan="${gws.length + 1}">${esc(l.label)}</td></tr>`;
    if (l.pathNote) {
      /* one disclosure row per canonical lane: WHICH path each gateway's numbers measured */
      h += `<tr><td class="metric">Measured path</td>` + recs.map((j) =>
        laneServed(j, l.flag)
          ? `<td class="muted lane-note">${esc(lanePathNote(l, j, st))}</td>`
          : `<td class="na"></td>`).join("") + `</tr>`;
    }
    for (const m of l.metrics) {
      // Each metric is a sealed envelope, read through the SAME metric() accessor the table uses -
      // routing this surface through mval() alone collapsed the states the table renders apart: a
      // measured failure showed as a bare n/a with no evidence, below-resolution's ≈0 showed as a
      // plain 0, and a suppressed metric's reason vanished. Ranking still uses mval() (failed and
      // absent rank as nothing; below-resolution ranks as 0).
      const cells = recs.map((j) => (laneServed(j, l.flag) ? metric(j[m.k], m.fmt) : null));
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
  }
  h += `</tbody></table></div>`;
  h += `<p class="fineprint">Best value per row is highlighted, decided by the measurement (lower latency and memory, higher throughput). Sweep overlays below use the sustained-throughput sweep (20 ms upstream delay) read off the SAME canonical record as the headline rows; every point is a real probe and the marked dot is the published number at its operating concurrency.</p>`;
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

  const series = gws.map((g, i) => {
    // CHOOSER-AWARE: the CHOSEN cell's sustained@20ms sweep (Peak/Same/Custom), the SAME cell the headline
    // rows read, so the marked peak is the published sustained@20ms at its operating concurrency. A
    // suppressed metric is {value:null,…} and its sweep array lives INSIDE that envelope — so a rig-bound
    // curve cannot surface a number the headline hides (finding 22, now structural).
    const p = chooserCellPerf(g);
    const env = p && p.rps_sustained_20ms;
    // C5: read the number through mval(), never a bare `.value` deref (audit #15).
    const v = mval(env);
    return {
      label: g.display, color: CMP_COLORS[i],
      sweep: v != null ? (env.sweep ?? null) : null,
      peak: v != null ? { rps: v, conc: concAt(env) } : null,
    };
  });
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
  const up = g.matrix && g.matrix.upstreams && g.matrix.upstreams[egress];
  return up && up.cells ? up.cells[ingress] : null;
}
/* Tooltip text for a cell. A grey (not_configurable) cell is the gateway's OWN declared
   incapability, so it shows the cited capability-limit reason (verdict_note) - never a bare
   "we didn't test it". Green/red show the verdict label + note as before. */
function matrixCellTip(cell) {
  const [, label] = cellState(cell);
  if (cell.served === "not_configured")
    // PROBE-FIRST grey: this cell WAS probed and the probe failed - show the probe's own evidence
    // (probe_note), falling back to the verdict prose. Honest wording: not configured/supported on
    // this pairing, never "the gateway failed" - no cell is graded red under probe-first.
    return `not configured: the capability probe on this ingress/upstream pairing did not complete a correct translation round trip${cell.probe_note ? " - " + cell.probe_note : cell.verdict_note ? " - " + cell.verdict_note : ""}`;
  if (cell.served === "not_configurable")
    // HONEST wording: the capability grid is authored by the busbar team from each project's docs
    // as a stand-in until that project's maintainers confirm their own grid. So a grey cell is "not
    // in the grid we drafted / not tested", NOT a claim the gateway's own maintainer declined it.
    return `not tested (this cell is not in the capability grid we drafted from the project's docs; the maintainers have not confirmed their own grid yet)${cell.verdict_note ? ": " + cell.verdict_note : ""}`;
  if (cell.served === "untestable")
    return `untestable on this rig: the gateway supports this pair in production but pins the real cloud host (no upstream base-URL override), so the test mock is unreachable - a harness limit, not gateway incapability${cell.verdict_note ? ": " + cell.verdict_note : ""}`;
  if (cell.served === "failed")
    // The pairing is real (the status was not a 404/501-shaped absence); the gateway reached and
    // declined this specific attempt. Surface the actual evidence, not just the verdict label.
    return `failed: the gateway answered HTTP ${cell.status || "?"}${cell.body_snippet ? " - " + cell.body_snippet : ""}`;
  if (cell.served !== true && cell.served !== "unprobed_auth" && isHarnessGap(cell))
    return `not verified: the harness could not get this gateway serving under this upstream config${cell.verdict_note ? " (" + cell.verdict_note + ")" : ""}`;
  return `${label}. ${cell.verdict_note || ""}`;
}
/* Per-cell perf line for a GREEN cell's tooltip/detail: this path's sustained RPS + added latency
   p99, and its RPS delta vs THIS gateway's REFERENCE cell (the one the Passthrough tab ranks; not
   necessarily the fastest, so it is named, never called "best"). Grey/red/unprobed cells carry no
   perf and return "".
   Dead on the live UI (cellPopFull is used instead) but still EXPORTED. Under the sealed envelope it
   CANNOT leak a rig-bound number: it reads the metric through mval(), which returns null for a
   suppressed envelope, so there is no ungated field to surface. */
function cellPerfTip(cell, ingress, egress, best) {
  const p = cell && cell.served === true ? cell.perf : null;
  const rps = p ? mval(p.rps_sustained_20ms) : null;
  const lat = p ? mval(p.added_latency_p99_us) : null;
  if (!p || !isEnvelope(p.rps_sustained_20ms)) return "";
  // Suppressed sustained RPS: {value:null}. Show the certified added-latency alone rather than a bare "".
  if (rps == null) {
    return lat != null ? `+${fmtInt(lat)} µs p99 added (sustained RPS n/a: rig-limited)` : "";
  }
  const bp = cellPath(best), bRps = mval(best && best.rps_sustained_20ms);
  let s = `${fmtInt(rps)} req/s (20 ms upstream)`;
  if (lat != null) s += `, +${fmtInt(lat)} µs p99 added`;
  if (bRps != null && bRps > 0) {
    if (bp.ingress === ingress && bp.egress === egress) s += " - reference cell (ranks the table)";
    // Human dialect labels (MATRIX_LABELS), never the raw dialect keys, in the hover popup.
    else s += ` - ${fmtPct((rps / bRps - 1) * 100)} req/s vs the ${MATRIX_LABELS[bp.ingress] || bp.ingress}→${MATRIX_LABELS[bp.egress] || bp.egress} cell`;
  }
  return s;
}

/* cellPopFull: the RICH matrix-cell popup. THE visual face of Custom: hovering a cell shows the SAME
   gated per-cell numbers the Performance/Streaming tables show for that exact ingress→egress cell (read
   through the SAME chooserPerfCell/chooserStreamCell accessors, so the popup and the table can never
   diverge), PLUS Δ-to-Peak (this cell vs the gateway's own best diagonal), PLUS the capability
   verdict/evidence. A rig-limited/absent cell reads "not measured", never a number.
   Returns an HTML string (or "" for an unmeasured egress column a v1 result never probed). */
function cellPopFull(g, ingress, egress) {
  const cell = matrixCell(g, egress, ingress);
  if (!cell) return "";
  const [, label] = cellState(cell);
  const head = `<h4>${esc(g.display)}: ${esc(MATRIX_LABELS[ingress])} in / ${esc(MATRIX_LABELS[egress])} upstream — ${esc(label)}${
    cell.status ? ` (HTTP ${esc(cell.status)})` : ""}</h4>`;
  // Read the SAME gated values the tables read, by pinning a synthetic Custom-mode state on this cell.
  const st = { mode: "custom", xlateIn: ingress, xlateOut: egress };
  const rows = [];
  // A MEASURED FAILURE IS A ROW, not a filtered-out absence: dropping it left a popup claiming
  // "served, not measured on this cell" over a cell whose every metric was measured and failed.
  const pushRow = (lbl, c) => {
    if (!c.na) rows.push(`<div><span>${lbl}</span><b>${esc(c.text)}</b></div>`);
    else if (c.failed) rows.push(`<div><span>${lbl}</span><b class="failtext" title="${esc(c.note || "")}">${esc(c.text)}</b></div>`);
  };
  const perfRow = (key, fmt, lbl) => pushRow(lbl, chooserPerfCell(g, key, fmt, st));
  perfRow("added_latency_p50_us", fmtAdded, "Added latency p50");
  perfRow("added_latency_p99_us", fmtAdded, "Added latency p99");
  perfRow("rps_sustained_20ms", fmtInt, "Sustained @20ms");
  perfRow("rps_max_proxy", fmtInt, "Max proxy RPS");
  const streamRow = (key, fmt, lbl) => pushRow(lbl, chooserStreamCell(g, key, fmt, st));
  streamRow("added_ttft_p99_us", fmtUsMs, "Added TTFT p99");
  streamRow("streams_sustained", fmtInt, "Streams sustained");
  const perfBlock = rows.length
    ? `<div class="pop-metrics">${rows.join("")}</div>`
    // A served cell with no per-cell perf (unswept), or a non-green cell: honest "not measured".
    : (cell.served === true ? `<div class="pop-perf muted">served, not measured on this cell</div>` : "");
  // Δ-to-Peak: this cell vs the gateway's own best diagonal (best_cell). "" for the peak cell itself.
  const cellPerf = chooserCellPerf(g, st);
  const cellPerfLabeled = cellPerf ? { ingress, egress, ...cellPerf } : null;
  const delta = deltaToPeak(cellPerfLabeled, g.best_cell);
  const bp = g.best_cell ? g.best_cell.path : null;
  const deltaBlock = delta
    ? `<div class="pop-delta">vs peak (${esc(MATRIX_LABELS[bp.ingress] || bp.ingress)}→${esc(MATRIX_LABELS[bp.egress] || bp.egress)}): ${esc(delta)}</div>`
    : (cellPerf && bp && bp.ingress === ingress && bp.egress === egress
      ? `<div class="pop-delta muted">this IS the peak cell (ranks the Performance tab)</div>` : "");
  const verdict = cell.verdict_note ? `<div class="pop-note">${esc(cell.verdict_note)}</div>` : "";
  // The egress fairness guard, surfaced where the translation claim is actually inspected. The mock answers
  // all six dialects by path, so a gateway that forwarded the ingress request VERBATIM would still get a
  // 200 and score as a translation it never performed. egress_reverified is the check that it really
  // re-shaped the request into the egress dialect. This is a CAPABILITY verdict, not a perf metric, so it
  // renders as prose next to verdict_note rather than as a lane row - and it is only stated for OFF-DIAGONAL
  // cells, since a same-dialect passthrough has nothing to translate and "verbatim" is the correct behaviour
  // there. reverify_note carries the basis, and an unverified cell says so rather than staying silent.
  const rv = cellPerf && cellPerf.egress_reverified;
  const reverify = (ingress !== egress && cellPerf && cellPerf.egress_reverified != null)
    ? `<div class="pop-note ${rv ? "" : "warn"}">${rv
      ? "egress re-verified: the request reaching the mock was in the egress dialect, not the ingress one"
      : "egress NOT re-verified: the mock saw the ingress shape, so this cell may be a verbatim proxy rather than a translation"}${
      cellPerf.reverify_note ? ` - ${esc(cellPerf.reverify_note)}` : ""}</div>`
    : "";
  const cta = cell.served === true ? `<div class="pop-cta muted">click → Performance (Custom, this cell)</div>` : "";
  return head + perfBlock + deltaBlock + verdict + reverify + cta;
}
/* hasMatrixGrid(g): did this gateway produce a protocol matrix at all? */
function hasMatrixGrid(g) { return !!(g && g.matrix && (g.matrix.upstreams || g.matrix.cells)); }
/* matrixFailureReason(g): WHY a gateway has no matrix, from whatever the producer recorded. Falls back to
   a plain statement rather than inventing a cause. */
function matrixFailureReason(g) {
  const first = [g && g.matrix && g.matrix.error, g && g.matrix_error, g && g.serve_error]
    .find((x) => typeof x === "string" && x.trim());
  const why = first ? stripRigPaths(first).split("\n")[0] : "the run produced no protocol matrix for this gateway";
  return `no matrix result: ${why}`;
}
/* matrixRoster(gateways): the rows the protocol grid renders: EVERY gateway, always, matrix or not. A
   gateway with no matrix renders as an all-n/a row carrying its failure reason, never as a silent
   absence: total failure has to provoke the same question a row of grey does. Sorted by pass count (a
   matrix-less gateway has none, so it sorts last), then by name. Pure; covered by site/test.mjs. */
function matrixRoster(gateways, tally) {
  return (gateways || []).slice().sort((a, b) =>
    (hasMatrixGrid(b) ? tally(b).pass : -1) - (hasMatrixGrid(a) ? tally(a).pass : -1) ||
    a.display.localeCompare(b.display));
}
function renderMatrix() {
  const gateways = state.data.gateways || [];
  // The empty state is for a board with NO matrix data at all (no results committed yet); it is not a
  // per-gateway filter. One gateway without a matrix renders as an all-n/a row, not as a disappearance.
  if (!gateways.some(hasMatrixGrid)) {
    document.getElementById("matrix-empty").classList.remove("hidden");
    document.getElementById("matrix-grid").classList.add("hidden");
    return;
  }
  /* per-gateway tallies over the full grid; sorted by measurement: pass count desc, then name */
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
            // Two different absences, said differently: a gateway with no matrix AT ALL carries its
            // failure reason on every cell, so the row reads as a failure rather than as an old result.
            if (!cell) return `<td class="na" title="${esc(missing ? matrixFailureReason(g)
              : "not measured (v1 result: this upstream dialect was not probed)")}">n/a</td>`;
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
    // Click a SERVED cell → jump to the Performance tab in Custom mode with this in→out pinned. The matrix
    // is the visual face of Custom: the popup shows the cell's numbers, the click opens the full row for it.
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
const CHART_CAPTIONS = {
  added_latency: "Added latency vs direct-to-mock, p99 in microseconds, concurrency 1, on each gateway's best same-dialect passthrough (the same canonical record the table ranks). Lower is better.",
  rps_sustained_20ms: "Sustained RPS with a 20 ms mock LLM latency (p99 under 1 s, error rate under 0.1 percent), best same-dialect passthrough. Higher is better.",
  rps_max_proxy: "Max proxy RPS against an instant mock, best same-dialect passthrough. Higher is better.",
  memory_rss: "Process RSS in MiB: cold idle vs the steady state reached under an identical fixed load, on the SAME cell for every gateway (a process cold-started for that cell, the load run until the RSS is steady). A gateway whose RSS never went steady has no steady state and is drawn not-measured, with its growth rate on the bar. Lower is better.",
  memory_recovery: "RSS 60 s after the fixed load stops (recovered) vs the steady state under load: does the gateway release the memory it took? Lower recovery is better.",
  cost_per_million: "Instance cost per million requests at the canonical sustained rate. Lower is better.",
  rps_per_dollar: "Canonical sustained RPS per dollar of hourly instance cost. Higher is better.",
  stream_added_ttft: "Streaming: added time-to-first-token vs direct-to-mock, p99. Lower is better.",
  stream_added_gap: "Streaming: added inter-frame (per-token) latency vs direct-to-mock, p99. Lower is better.",
  stream_sustained: "Streaming: max concurrent SSE streams sustained without frame loss or stalls. Higher is better.",
  streamcpu_fps: "Streaming relay throughput under an unpaced firehose (CPU-bound): sustained content frames/sec. Higher is better.",
  xlate_added_latency: "Translation on each gateway's canonical path (direction named on the bar; matrix per-cell sweep): added latency p99. Lower is better.",
  xlate_rps_sustained_20ms: "Translation on each gateway's canonical path (direction named on the bar): sustained RPS at 20 ms LLM latency. Higher is better.",
};
function chartCaption(file) {
  const base = file.replace(/^charts\//, "").replace(/\?.*$/, "").replace(/\.png$/, "");
  const top5 = base.startsWith("top5_");
  const key = top5 ? base.slice(5) : base;
  const body = CHART_CAPTIONS[key] || key.replace(/_/g, " ");
  // The top5 subset is selected ONCE, by lowest added latency, and the SAME five gateways are
  // drawn on every top5 chart (charts.py _ranked()[:5]). Said explicitly so a reader is never
  // surprised that a top5 RPS chart can omit the true #4 by RPS: the cut is by latency, not
  // re-computed per metric.
  return (top5 ? "Top 5 by lowest added latency, the same five on every chart. " : "All gateways. ") + body;
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
  // WHICH BENCHMARK PRODUCED THIS BOARD. Stated once here so the per-row sha has something to be
  // compared against: a red row means "not this one", and a reader should not have to infer what
  // "this one" is. Also counts the rows that are behind it, because one red pill in a long table is
  // easy to miss and the count is the thing that decides whether the board is safely comparable.
  if (state.data.benchmark_version) {
    const behind = (state.data.gateways || []).filter((g) => g.engine && !g.engine.current).length;
    const short = String(state.data.benchmark_version).slice(0, 7);
    bits.push(behind
      ? `Benchmark version: ${short} (${behind} row${behind === 1 ? "" : "s"} measured on an older version)`
      : `Benchmark version: ${short}`);
  }
  bits.push(`Site data generated: ${state.data.generated_at ? stampWithAge(state.data.generated_at) : "unknown"}`);
  const rig = rigStamp();
  if (rig) bits.push(rig);
  hw.textContent = bits.join(" · ");
}

/* rigStamp(): WHICH measurement instrument produced the board's numbers.
   The mock + loadgen come from a MOVING GitHub release tag, so an identical harness can produce
   different cell VERDICTS across runs purely because the instrument was rebuilt in between. Each
   snapshot carries the mock/ugen sha256 that produced it; this surfaces the short digest so an
   instrument change is legible. Returns "" when no gateway records one: never a fabricated identity,
   and never the word "unknown" dressed up as a version. When rows DISAGREE the count is shown, because a
   board built from two different instruments is exactly the condition worth seeing. */
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
   The landing view is a ROSTER, not a ranking: every gateway in alphabetical
   order (display name, case-insensitive), with its language, a committed star
   snapshot, and its OWN self-description (g.cls). No perf numbers, no winner
   highlighting; the other tabs measure how they perform. Every entry gets the
   exact same row treatment, the operator's own included. */
/* Roster sort state: the overview is sortable by any column, DEFAULTING to name A→Z (the neutral
   ordering — no metric, no ranking). Clicking a header sorts by it; clicking the active header
   flips direction. `name` is the tiebreaker for every column so ties are stable and alphabetical. */
let rosterSort = { col: "name", dir: "asc" };
/* Per-column sort key: a comparable value (string or number) for gateway `g`. `null`/`n/a` sorts
   LAST regardless of direction (a missing value is never "best"). */
const ROSTER_KEY = {
  name: (g) => g.display.toLowerCase(),
  lang: (g) => (g.lang || "").toLowerCase(),
  version: (g) => { const b = gatewayBuild(g); return b ? fmtBuild(b).toLowerCase() : null; },
  lastrun: (g) => { const d = gatewayLastRun(g); return d ? d.getTime() : null; }, // newer = larger ms
  age: (g) => (g.first_commit ? new Date(g.first_commit).getTime() : null), // older = smaller ms
  stars: (g) => (g.stars == null ? null : g.stars),
  cls: (g) => (g.cls || "Gateway").toLowerCase(),
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
/* Project age from the repo's first-commit date, in ONE simple floored unit: "11+ years",
   "7+ months", "3+ weeks". Context for the star counts and scores - a decade-old project and a
   three-week-old one earn them differently. Null (no snapshot) renders muted. */
const fmtProjectAge = (firstCommit) => {
  if (!firstCommit) return null;
  const days = Math.max(0, (Date.now() - new Date(firstCommit).getTime()) / 86400e3);
  // Floored, so the "+" is honest: 11.7 years reads "11+ years".
  if (days >= 365) return `${Math.floor(days / 365)}+ year${days >= 730 ? "s" : ""}`;
  if (days >= 30.44) return `${Math.floor(days / 30.44)}+ month${days >= 61 ? "s" : ""}`;
  if (days >= 7) return `${Math.floor(days / 7)}+ week${days >= 14 ? "s" : ""}`;
  return `${Math.max(1, Math.floor(days))} days`;
};

/* gatewayBuild reads the stamp of what is SHOWN: the matrix (the sole source of every projected number),
   falling back to a projected record's own source.build. `g[l.key]` (g.perf / g.stream / g.xlate) is a
   raw suite object the emit step DELETES from the bundle, not a valid source here. */
const displayedRecords = (g) => [g.best_cell, g.translation_cell, g.streaming, g.memory_read].filter(Boolean);
const gatewayBuild = (g) => {
  if (g && g.matrix && g.matrix.build) return g.matrix.build;
  const rec = displayedRecords(g || {}).find((r) => r.source && r.source.build);
  if (rec) return rec.source.build;
  // NOT-YET-MEASURED IS NOT NOT-KNOWN. Above this line the build is the stamp of what was actually
  // run, which is the right authority for a row carrying numbers and stays first. But a gateway
  // awaiting its first run has no stamp at all, and the row rendered a bare "n/a" in every column -
  // a page that reads as "we know nothing about this project" when the manifest pins the exact
  // version we would measure. `g.version` is that pin, so the field is always listed with what it
  // runs; "last benchmarked" stays honestly n/a, because that part really is unknown.
  return (g && g.version) || null;
};
/* The hardware the DISPLAYED numbers were measured on: the matrix stamp (sole source). */
const gatewayHardware = (g) => (g && g.matrix && g.matrix.hardware) || null;
const gatewayArch = (g) => (g && g.matrix && g.matrix.arch) || null;
/* HOW the gateway was run for the benchmark: its official Docker image vs a native/source binary.
   Inferred from the build stamp - an image ref (registry/repo:tag or an @sha256 digest) is docker;
   a bare version/commit ("...@9649b27 (source build)") is a native binary. This is real context, not
   decoration: a containerised gateway and a native one differ in base image, fd limits, and startup,
   so the reader deserves to see which each number was measured under. Null when no build is stamped. */
const runMode = (g) => {
  const b = gatewayBuild(g); if (!b) return null;
  return (/@sha256:/.test(b) || /[\w.\-]+\/[\w.\-]+:[\w.\-]+/.test(b)) ? "docker" : "binary";
};
/* Compact monochrome run-mode marks (currentColor, so they sit muted beside the date); the tooltip
   carries the words. docker = container/whale; binary = a terminal with a shell prompt. */
const RUNMODE_ICON = {
  docker: '<svg class="rm-ico" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M4 10h3v3H4zm4 0h3v3H8zm4 0h3v3h-3zM8 6h3v3H8zm4 0h3v3h-3z"/><path d="M23 12.3c-.6-.4-1.8-.6-2.8-.4-.1-.9-.7-1.8-1.6-2.4l-.5-.3-.3.5c-.4.7-.6 1.6-.1 2.4-.3.2-1 .4-1.7.4H2c-.2 1.4.1 2.9.9 4.1C4 18.9 6.6 20 10 20c6.9 0 12-3.2 14.3-9 .9.1 2.2 0 2.7-1.4-1.6-.9-3.7-.6-4-.3z" transform="translate(-2 0)"/></svg>',
  binary: '<svg class="rm-ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="2.5" y="4.5" width="19" height="15" rx="2"/><path d="M6.5 9.5l3 2.5-3 2.5M13 15h4.5"/></svg>',
};
const runModeCell = (g) => {
  const m = runMode(g); if (!m) return "";
  const label = m === "docker" ? "Measured running its official Docker image" : "Measured as a native / source-built binary";
  return `<span class="runmode ${m}" title="${label}" aria-label="${label}">${RUNMODE_ICON[m]}</span>`;
};
/* The VERSION token alone for the table cell - the tag, package version, or short commit;
   the full build string (image path, digest, annotations) stays in the tooltip.
   "ghcr.io/x/y:v1.3.1" -> "v1.3.1"; "somepkg==1.93.0" -> "1.93.0"; "repo@9649b27..." -> "@9649b27";
   "somegateway 1.4.1" -> "1.4.1". Anything unparsable falls back to a truncated string. */
const fmtBuild = (full) => {
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
  return head.length > 24 ? head.slice(0, 21) + "..." : head;
};

/* WHEN that gateway was last benchmarked, for the roster "last benchmarked" cell + sort. Prefers
   g.measured_at, the matrix-preferring stamp gen-data emits (displayedMeasuredMs) and the SAME per-row
   freshness basis the "measured Nd ago" badge + the freshness guard use, so a standalone legacy suite
   re-run cannot date the row fresher than the matrix numbers the board actually shows. Falls back to the
   newest timestamp across lane suites only when there is no matrix stamp (a legacy-only row aged by
   that stamp). */
function gatewayLastRun(g) {
  if (g && g.measured_at) { const ms = new Date(g.measured_at).getTime(); if (ms > 0) return new Date(ms); }
  let newest = 0;
  for (const l of LANES) {
    const t = g[l.key] && g[l.key].measured_at;
    if (t) { const ms = new Date(t).getTime(); if (ms > newest) newest = ms; }
  }
  return newest ? new Date(newest) : null;
}
/* The newest `measured_at` across every gateway's suites: WHEN the field was last benchmarked.
   Honest label for the board — a single clock for "how fresh is this data". Null if none stamped. */
function lastBenchmarkRun(gateways) {
  let newest = null;
  for (const g of gateways) {
    const d = gatewayLastRun(g);
    if (d && (!newest || d > newest)) newest = d;
  }
  return newest;
}
/* Per-gateway last-benchmarked date for the roster cell: a plain UTC date (YYYY-MM-DD); the full
   timestamp rides the tooltip. Null renders muted. */
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
    const build = gatewayBuild(g);
    const age = fmtProjectAge(g.first_commit);
    const lastRun = gatewayLastRun(g);
    const lastRunTxt = fmtLastRun(lastRun);
    return `<tr data-gw="${esc(g.key)}" class="rowlink">
      <td class="name">${name}</td>
      <td><span class="lang-chip" style="background:${c}">${esc(g.lang)}</span></td>
      <td class="build">${build ? `<span title="${esc(build)}">${esc(fmtBuild(build))}</span>` : `<span class="muted">n/a</span>`}</td>
      <td class="lastrun">${lastRunTxt ? `${runModeCell(g)}<span title="last benchmarked ${esc(lastRun.toISOString().slice(0, 16).replace("T", " "))} UTC">${esc(lastRunTxt)}</span>` : `<span class="muted">n/a</span>`}</td>
      <td class="age">${age ? `<span title="first commit ${esc(g.first_commit)}">${esc(age)}</span>` : `<span class="muted">n/a</span>`}</td>
      <td class="stars">${stars != null ? esc(stars) : `<span class="muted">n/a</span>`}</td>
      <td class="cls">${esc(g.cls || "Gateway")}</td>
    </tr>`;
  }).join("");
  // Row click opens the per-gateway drawer (same as the perf tabs) — /gateways rows are clickable too.
  // A click on the repo link (<a>) opens the repo, not the drawer.
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
   The site root (/) is a designed landing page, not a data dump: hero, pitch,
   neutrality line, and one CTA card per CATEGORY (the extension seam: a new
   category entry gets its card automatically). Pure HTML builder exported for
   the node smoke test. */
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
  state.view = view;
  // Memory's data-derived Same default is seeded on ARRIVAL at memory, not once globally at boot, so
  // the other tabs keep the dialect default they declare (see seedMemorySameDialect).
  seedMemorySameDialect();
  // EACH FAMILY REMEMBERS ITS OWN MODE, because coercing across them is LOSSY.
  //
  // The tabs do not offer the same modes: Peak is meaningless (and dishonest) on memory, Min/Max are
  // meaningless on the perf lanes. Coercing on arrival keeps the control and the numbers agreeing,
  // but it also OVERWRITES the mode - so Performance(Peak) -> Memory coerced to Min, and coming back
  // to Performance kept Min's coercion instead of the Peak the reader had chosen. It destroyed a
  // selection in both directions: a Min/Max choice died the same way on a round trip through a perf
  // tab. Stashing the outgoing family's mode before coercing makes a tab flip lossless, while a
  // shared URL still coerces exactly as before (decodeUrl, unchanged).
  const arriving = modeFamily(view);
  const leaving = modeFamily(state.view);
  if (leaving !== arriving) state.modeMemo[leaving] = state.mode;
  state.mode = resolveMode(state.modeMemo[arriving] ?? state.mode, view);
  // Home is the root above the category nav: the header's category row, tab bar
  // and category tagline belong to the category view only, so a body class hides
  // them (style.css) while the home hero carries the brand treatment instead.
  document.body.classList.toggle("home", view === HOME_VIEW);
  // Performance/Streaming/Memory share one table container (#view-table); matrix/method
  // have their own; home renders #view-home.
  const containerId = TABLE_VIEWS.has(view) ? "view-table" : `view-${view}`;
  document.querySelectorAll(".tab").forEach((x) => {
    x.classList.toggle("active", x.dataset.view === view);
    x.setAttribute("href", viewPath(state.category, x.dataset.view));
  });
  document.querySelectorAll(".view").forEach((v) => v.classList.toggle("hidden", v.id !== containerId));
  // The cell chooser belongs to every chooser-driven tab. On MEMORY it appears only once the bundle
  // carries per-cell windows: offering Min | Max | Same | Custom over a run that measured one cell per
  // gateway would be four controls that all show the same number.
  const chooser = document.getElementById("cell-chooser");
  if (chooser) chooser.classList.toggle("hidden",
    !CHOOSER_VIEWS.has(view) || (view === "memory" && !hasPerCellMemory(state.data)));
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
    mode: st.mode, sameDialect: st.sameDialect, sameDialectPinned: st.sameDialectPinned,
    xlateIn: st.xlateIn, xlateOut: st.xlateOut,
    cmp: st.cmp, cmpOpen: st.cmpOpen, drawer: st.drawer,
  });
}

/* seedMemorySameDialect(): seed the Same dialect from the DATA - the identity cell the most gateways
   serve - FOR THE TAB THAT ASKS FOR IT. Memory's Same mode defaults to it (only Same/Custom are
   like-for-like, so the default has to be the cell the widest slice of the field can actually be
   compared on), and it is computed, never named: no protocol is special-cased anywhere in this engine.
   A ?d= in the URL wins.

   IT IS A MEMORY-TAB DEFAULT, SO IT IS SEEDED FOR THE MEMORY TAB. Seeding it for the whole state at
   boot meant a deep link into performance or streaming - tabs that share this one dialect field and
   whose own default is the declared one, which is exactly why syncUrl only omits ?d= on memory - had
   its dialect silently rewritten from the data before it rendered a single row. The seed now happens on
   arrival at memory (and at boot when memory IS the arrival tab), so a tab gets the default it declares
   rather than another tab's. */
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
      document.querySelector("main").innerHTML =
        `<p class="muted">Could not load site data (${esc(err.message)}). Run <code>node site/gen-data.mjs</code> first.</p>`;
    });
}

if (NODE) {
  /* Exports for the node smoke test (site/test.mjs). */
  module.exports = {
    newState, encodeUrl, decodeUrl, viewPath, applyFilters,
    fmtStamp, fmtAge, stampWithAge, measuredBadge, engineBadge,
    drawSweep, niceStep, fmtTick, COLUMN_SETS, columnsFor, PERF_VIEWS, TABLE_VIEWS, VIEW_SORT, LANES, naText, stripRigPaths,
    cellState, matrixCellTip, cellPerfTip, passCell, xlateCell, streamCell, memCell, rssSparkline, hasTranslation, CATEGORIES, DEFAULT_CATEGORY, VIEWS,
    CHOOSER_MODES, chooserCellPerf, chooserDialects, chooserPerfCell, chooserCellStream, chooserStreamCell, chooserHasCell, deltaToPeak, cellPopFull,
    // memory cell chooser (Min | Max | Same | Custom, never Peak) + the matrix roster hole-closer.
    MEM_CHOOSER_MODES, CHOOSER_VIEWS, modesFor, defaultMode, resolveMode, memoryMode,
    perCellMemory, memoryCells, hasPerCellMemory, widestDialect, chosenMemory, memoryFor,
    idleAcrossCells, neverPlateaued, worstGrowth, memCellTip, neverPlateauedPill,
    idleStatic, memShape, memGrowing, memShaped,
    hasMatrixGrid, matrixFailureReason, matrixRoster,
    laneRecord, lanePathNote, perfSweepSeries, concAt, sustainedChooserCell, maxProxyChooserCell,
    colTested, gatewayBuild, gatewayHardware, runMode, laneAgeSummary,
    chooserCaption, chooserLead, streamingProvenance,
    memoryCaption, memWindows, boardMemWindows, memLoadCellLabel, memLoadRecipeTip, memDisclosure,
    canonicalPerf, canonicalXlate, canonicalStreaming, canonicalMemory, metric, mval, isEnvelope, caption, SWEEP_CAPTION, gatewayResultsJson, DEFAULT_VIEW, VIEW_LABELS, rosterRows, fmtStars,
    configCorrectionUrl, BENCH_REPO, fmtInt, fmtAdded,
    HOME_VIEW, homeCardsHtml,
    metricTd,
    // The roster's row order, the compare row's tied-best set, the lane served predicate and the
    // memory-tab dialect seed: pure functions the suite drives directly, because each of them was a
    // defect that no DOM-free test could reach while it lived inside a renderer.
    rowComparator, bestIndex, laneServed, seedMemorySameDialect,
    // audit #21: the rig-provenance footer stamp + the live state it reads, so the class test can drive it.
    rigStamp, state,
    // THE SURFACES THAT WERE UNREACHABLE FROM A DOM-FREE SUITE, and were therefore covered by nothing:
    // the drawer (drawerHtml was called by no test at all - deleting the clause that keeps a MEASURED
    // FAILURE visible in it broke no test), the compare panel's whole body (extracted from renderCompare
    // for exactly this reason), and the one place a gateway's repo URL reaches an href.
    drawerHtml, compareBodyHtml, gwLink, recordShowsValues,
  };
} else {
  boot();
}
