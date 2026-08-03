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
// A `frontier` TAB rather than six more columns on Performance, or a taller Performance page: the whole
// curve is a different question ("what shape is this gateway") from the ranking ("who is fastest at my
// bound"), and the layout rule here is that less scrolling wins even at the cost of another tab.
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
// Maps retired view names onto the current tabs so old shared links keep resolving. `translation`
// aliases to `performance` (its ?xin/?xout still decode into the Custom in/out below).
// `charts: "method"` WAS HERE and is gone: it pointed a retired /charts tab at Method, and Charts is a
// real tab again. `resolveView` checks VIEWS first, so the alias was already shadowed - leaving it
// would have been a dead entry claiming a redirect that no longer happens. An old /gateways/charts
// link now lands on the Charts tab, which is what the URL says.
const VIEW_ALIASES = { results: "performance", peak: "performance", matched: "performance", passthrough: "performance", translation: "performance" };
// Each perf tab's default (and honest headline) sort column; a clean URL omits the sort when it
// equals this, and switching tabs snaps to it unless the URL pins another.
// Streaming defaults to added TTFT (asc), NOT streams-sustained: the sustained count saturates at the
// harness cap (1024 in the current field data) so it ties several gateways and breaks ties by name,
// floating a slow-TTFT gateway above a fast one at the same count. Added TTFT is the streaming-overhead
// discriminator that a user actually feels first and it does not saturate.
// The frontier tab lands on the DEFAULT BOUND's column, and follows the reader's selection from there
// (renderTable moves the sort with the bound, so switching re-ranks the board in front of them).
// "f10" is `boundColId(DEFAULT_BOUND_MS)` written out, because this object is initialised before that
// constant exists; site/test.mjs asserts the two agree, so the literal cannot outlive the default.
const VIEW_SORT = { performance: "rps", frontier: "f10", streaming: "sttft", memory: "mempeak" };
/* RETIRED SORT IDS, remapped so a shared permalink still lands on a ranking. `?sort=rps20` and
   `?sort=rpsmax` are in every link ever shared to the Performance tab and in the charts' deep links; the
   columns they name are gone with the two scalar metrics, and an unrecognised sort id silently falls back to
   the tab default, which is fine - but both of those links MEANT "rank by throughput", and the frontier
   reading at the selected bound is that ranking. Mapping them says so instead of quietly dropping them. */
const SORT_ALIASES = { rps20: "rps", rpsmax: "rps", cpufps: "streamfps" };
/* The cell-chooser modes shared by Performance + Streaming: which cell(s) of the ONE 6x6 run to show.
     peak   — each gateway on its OWN REPRESENTATIVE same-dialect diagonal (best_cell). Default. Per-row pill.
     same   — ONE picked dialect's diagonal (X→X) for every gateway. No pill (the dialect is in the control).
     custom — any ingress→egress cell (incl. translation) for every gateway. No pill.

   THE `peak` KEY IS A URL CONTRACT, NOT A DESCRIPTION, and it is deliberately no longer what the control
   SAYS. `?mode=peak` is in every link ever shared to this board (and VIEW_ALIASES maps a retired /peak tab
   onto it), so the token stays; the label a reader sees is "Own cell" (MODE_LABELS), because "Peak" asserted
   a maximum that the selection does not compute.
   WHAT IT ACTUALLY SELECTS, from gen-data.mjs `bestCell`: the openai→openai diagonal unconditionally when
   the gateway serves one, otherwise the diagonal with the LOWEST added-latency p99. It never reads a
   throughput number, so it is not a maximum of anything, and switching the tail-latency bound cannot change
   which cell it picks. The board caught this claiming "the most req/s each gateway carried": kong's four
   diagonals span 3,903 → 22,891 req/s at the same bound, so "the most" was wrong by ~6x on that one row.
   The chooser is a REPRESENTATIVE-CELL chooser and every surface now says so. */
const CHOOSER_MODES = new Set(["peak", "same", "custom"]);
/* The MEMORY lane's own mode set: Min | Max | Same | Custom.
   There is deliberately no `peak` here. best_cell prefers the openai diagonal and otherwise ranks on
   LATENCY; using it for memory would select on one axis and report another - the exact defect per-cell
   measurement exists to remove - and it would arrive dressed as a UI control, so a reader could not see
   it. Min/Max ARE offered because they select on memory and report memory: real minima and maxima of the
   quantity in the column.
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
/* THE PERF LANES DEFAULT TO SAME, NOT OWN CELL, so the first thing a reader sees is like-for-like.
   `peak` puts every gateway on its OWN representative diagonal, which means the default view could sit
   busbar's openai>openai (82,328 req/s) directly beside litellm-rust's anthropic>anthropic (44,475) and
   invite the ratio between them. Those are different cells: on the SAME cell the gap is 1.70x, not
   1.85x, and the "Tested on" pill said so all along in a column most readers never read.
   A ranking is a comparison, and a comparison of two different measurements is not one. Own cell stays
   one click away (it is the honest view of what each gateway is BEST at, and the only mode that shows
   every gateway at once); it is simply not the one we hand somebody first. */
function defaultMode(view) { return view === "memory" ? "min" : "same"; }
/* The dialect a Same-mode view opens on before any data has loaded. widestDialect() answers this from
   the run and is always preferred; this is only the pre-data fallback, and it is the value newState()
   seeds, so "is this dialect the default" has one answer both the encoder and the state agree on. */
const DEFAULT_SAME_DIALECT = "openai";
function seededSameDialect(data) { return widestDialect(data) || DEFAULT_SAME_DIALECT; }
/* Which chooser family a view belongs to. The perf lanes offer Peak/Same/Custom; memory offers
   Min/Max/Same/Custom. They overlap on Same/Custom but not on the mode most readers want, which is
   why a single carried-across `mode` cannot serve both. */
function modeFamily(view) { return view === "memory" ? "memory" : "perf"; }
/* resolveMode: coerce a mode onto a view that offers it. This is what a SHARED URL hits: a link carrying
   ?mode=peak that lands on the memory tab must NOT render a throughput-selected memory number, so it falls
   back to Same; the reverse (?mode=min on Performance) falls back to Peak rather than reading nothing. */
function resolveMode(mode, view) { return modesFor(view).has(mode) ? mode : defaultMode(view); }
/* modeOnArrival(fromView, toView, mode, memo): the mode and the family memo after navigating fromView->toView.

   EACH FAMILY REMEMBERS ITS OWN MODE, because coercing across them is LOSSY. The tabs do not offer the same
   modes: Peak is meaningless (and dishonest) on memory, Min/Max are meaningless on the perf lanes. Coercing on
   arrival keeps the control and the numbers agreeing, but it also OVERWRITES the mode - Performance(Custom) ->
   Memory coerces to Min, and coming back to Performance kept Min's coercion instead of the Custom the reader
   had chosen. Stashing the OUTGOING family's mode before coercing makes a tab flip lossless in both directions.

   A SAME-FAMILY ARRIVAL MUST NOT CONSULT THE MEMO AT ALL, and that is the half that was broken: the memo is
   pre-seeded (newState gives it perf:"peak", memory:"min"), so `memo[arriving] ?? mode` never falls through to
   `mode`, and reading it on every arrival - including the first render of a deep link, and every re-render of
   the view you are already on - overwrote the mode decodeUrl had just parsed out of the URL. Frontier ->
   Performance is one family; so is a plain re-render; neither is a place where a remembered mode can be more
   authoritative than the current one.

   Pure, and exported, because showView is DOM-bound and this decision is not: the round trip has to be
   assertable without a browser. */
function modeOnArrival(fromView, toView, mode, memo) {
  const leaving = modeFamily(fromView), arriving = modeFamily(toView);
  if (leaving === arriving) return { mode: resolveMode(mode, toView), memo };
  return { mode: resolveMode(memo[arriving] ?? mode, toView), memo: { ...memo, [leaving]: mode } };
}
/* memoryMode: THE choke point for the memory lane's mode. Every memory reader routes through it, so even a
   state hand-built with mode:"peak" (a stale in-memory state, a test, a future caller) cannot produce a
   peak-selected memory number - it reads Same instead. */
function memoryMode(st = state) { return MEM_CHOOSER_MODES.has(st.mode) ? st.mode : defaultMode("memory"); }
/* The segmented control's copy, one entry per mode across both mode sets.
   `peak` READS "Own cell", not "Peak": see CHOOSER_MODES. Min/Max keep their names because they really
   are extrema of the quantity their column reports; this one is not an extremum of anything. */
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
/* A RATE, WHICH IS THE ONE METRIC THAT CAN LEGITIMATELY BE BELOW 1.
   `Math.round` sends 0.25 req/s to "0", and "0" on this board means "measured, and it carried nothing"
   - so a gateway serving one request every four seconds would be reported as serving none. The engine
   was changed to stop making exactly that statement (GenStats::rps publishes a fraction below 1/s and a
   whole number at or above it); rounding here re-made it at the last step, which is how a fix reaches
   the artifact and dies at the renderer. Same split as the engine's, so the two cannot disagree. */
const fmtRate = (v) => (v > 0 && v < 1 ? v.toFixed(2) : fmtInt(v));
// Added-latency deltas are shown raw (no noise-floor smoothing). On the paced stream
// suite the per-frame value is noise-dominated and can flip sign run-to-run; the honest
// per-frame number comes from the CPU-bound stream suite, not from massaging this one.
const fmtAdded = fmtInt;
const fmt1 = (v) => v.toLocaleString("en-US", { minimumFractionDigits: 1, maximumFractionDigits: 1 });
/* Three significant figures for a quantity whose whole point is that it can be TINY. The idle window's
   span is the case: litellm-rust moves 0.008 MiB (one 8 KiB page) and bifrost moves 64.7, and fmt1 rounds
   the first to "0.0" - a flat zero, which is precisely the "nothing happened" claim the span exists to
   distinguish from. maximumSignificantDigits keeps 0.00781 legible without giving 64.7 five decimals. */
const fmt2 = (v) => v.toLocaleString("en-US", { maximumSignificantDigits: 3 });
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
  /* THE ZERO THAT ONLY THE STREAMING COUNTS CAN CARRY, and it names THEIR gate. It used to read "served,
     but no tested load held p99 < 1 s at <0.1% errors", which by the end was wrong twice over: no gate ever
     enforced 1 s (the retired throughput gate used 20 ms, and the frontier that replaced it names its bound
     on every surface), and the frontier grants no error tolerance at all - a rung qualifies only if it
     failed nothing it accepted. The two retired throughput columns were the only surfaces that showed this
     note; seal.mjs now emits it for `streams_sustained` / `streams_sustained_fps` alone, so it states the
     STREAM delivery gate, which is the one this token is now ever true about. */
  no_qualifying_ceiling: "served, but no tested concurrency held the stream delivery gate (every expected frame delivered, no stall, under 0.1% of streams erroring), so there is no qualifying ceiling to publish",
  measured_failure: "MEASURED FAILURE: the gateway was offered the load and sustained none of it (a real 0, not an unmeasured cell)",
  // NO `mock_bound` / `unverifiable`. Those were the two suppression reasons: a measurement at or above
  // 90% of the rig's own ceiling was replaced with null and rendered as "not shown". The measurement was
  // correct in every one of those cells - only what it MEANT was open - so the engine now publishes the
  // number along with the ceiling it was measured against and the fraction of it reached, and a reader
  // draws the conclusion these sentences used to draw for them. No producer can emit either token.
  not_measured: "not measured: no reading exists for this cell",
  // The engine's own absence reasons (measurement.rs Absent), carried through the seal since the
  // reason-flattening fix. below_resolution is handled in metric() as a display state, not a hole.
  below_resolution: "below measurement resolution: the comparison ran and the gateway's overhead was too small for this rig to detect (the best result this test can express)",
  rig_limited: "not shown: rig-limited, the harness's own ceiling bounded this number, so it is not a gateway reading",
  untestable: "not testable: the rig cannot pose this question for this dialect (a rig limit, not a gateway fault)",
  // Still emitted, and now ONLY by the streaming ceiling: a stream-concurrency range whose top rung still
  // passed publishes an absence, because the top of the range is our choice and not the gateway's answer.
  // THE THROUGHPUT LANE NO LONGER REACHES THIS. It used to discard a real measured rate for the same
  // condition; a frontier reading in that state is published and labelled a FLOOR ("≥ 19,000"), which tells
  // the reader strictly more than the null did.
  search_exhausted: "not shown: the search ran off the end of its range still improving, so any number would be a lower bound, not a ceiling",
  harness_error: "not shown: the harness itself failed here; this says nothing about the gateway",
  not_served: "the gateway does not serve this pairing",
};
function noteText(tok) { return (tok && METRIC_NOTES[tok]) || tok || ""; }
/* The comparison-against-our-own-rig sentence, from the fraction and the ceiling it is a fraction of.
   Both come off the envelope; neither is re-derived here, so this cannot disagree with the engine.
   The ceiling is named when it is known, because a bare percentage of an unstated quantity is not a
   fact a reader can check. Above 100% is rendered as it is: two separately-timed legs scatter, and a
   gateway that adds its own SSE framing legitimately carries more events per second than the mock's own
   layout would - hiding that would hide the reader's best clue that the reference is the soft number. */
function headroomText(frac, ceiling) {
  const pct = frac >= 0.1 ? (frac * 100).toFixed(0) : (frac * 100).toFixed(1);
  const of = Number.isFinite(ceiling) ? ` (${fmtInt(ceiling)})` : "";
  return `${pct}% of this rig's own ceiling${of} at the same concurrency`;
}
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
  // `cell.why` is an EXPLICIT short on-cell reason, for a state whose meaning is not carried by an envelope
  // note token: a frontier reading where no rung held the bound is a measured 0 whose reason lives on the
  // READING, not on the envelope, and "0" alone would read as "no data" for the gateways it matters most on.
  const zeroWhy = cell.why || (cell.v === 0 && cell.env && ZERO_WHY[cell.env.note]);
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
    /* "n/a" MEANS NOT APPLICABLE - the gateway does not serve this pairing - AND NOTHING ELSE.
       It was the fall-through for every remaining absence, which put two opposite findings under one
       label. `not_served` and `untestable` are capability limits: the question does not apply to this
       gateway, and n/a is exactly right. `not_measured` is the opposite - the harness ran, got a
       result, and refused to publish it. busbar's streaming is the case that shows the cost:
       "the bisection proved c=4093, but it did not hold on re-measurement" is a gateway that
       demonstrably streams, rendered identically to one that cannot stream at all.
       The reasons are already distinct in the data; only the label collapsed them. */
    const reason = env && env.reason;
    /* AND WHEN OUR OWN RIG IS WHY, THE ROW MUST SAY SO. The engine emits the same absence shape when
       the DIRECT-TO-MOCK leg failed, or when its own search found a rung and could not confirm it -
       neither of which is a fact about the gateway. Rendering those in the gateway's column with no
       attribution charges the rig to the subject, which is the one thing this board exists not to do.
       The detail already names the culprit; this reads it. */
    const rigSide = !!(detail && /(direct-to-mock leg|from the mock directly|did not hold on re-measurement)/.test(detail));
    const text = rigSide ? "unconfirmed"
      : reason === "not_measured" ? "not measured"
      : reason === "harness_error" ? "rig fault"
      : "n/a";
    return { v: null, text, na: true, rigSide, note: detail || noteText(reason), env: env || null };
  }
  // A CERTIFIED NUMBER CAN CARRY MORE THAN ONE THING WORTH SAYING, so the note is composed rather than
  // being whichever single token the envelope happened to have: the zero's meaning, the paced-match
  // signal, and (on the legacy fallback rows) a provenance stamp OF ITS OWN when this number came out
  // of a different run than the record around it - cpu_fps is measured by the streamcpu suite while its
  // record is stamped by the stream suite, and dating it to the wrong run is a claim, not a formatting
  // detail. Each is rendered only when the envelope actually carries it.
  const notes = [];
  if (env.note) notes.push(noteText(env.note));
  // HOW CLOSE THIS CAME TO OUR OWN RIG'S CEILING, stated rather than acted on.
  //
  // This is what replaced the suppression, and it has to be VISIBLE or the trade was not made: a number
  // near the rig's limit used to be deleted, and deleting it at least told the reader something. Saying
  // "43297 frames/sec, 83% of the mock's own 52013 ceiling" tells them strictly more and costs them
  // nothing. A `paced_match: true` boolean rode here before, which could not distinguish 0.993 from 0.20.
  //
  // Rendered as a percentage because that is how the fraction reads: 99% says "kept pace with a paced
  // upstream, the best outcome available", 20% says "plainly the gateway's own limit".
  if (Number.isFinite(env.headroom)) notes.push(headroomText(env.headroom, env.rig_ceiling));
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

/* ---- the latency-throughput frontier ----------------------------------------
   WHAT THIS REPLACED, AND WHY THE UI IS SHAPED THE WAY IT IS. The board used to publish two throughput
   scalars per cell, `rps_sustained_20ms` and `rps_max_proxy`. Both were the SAME concurrency sweep
   collapsed to one number by a chosen latency ceiling (engine/src/frontier.rs states the case), and one
   of the two columns even captioned a qualifying bar of "p99 < 1 s" that the engine never enforced.
   They are deleted. Each perf record now carries `frontier`: one sealed reading per declared tail-latency
   bound (1, 5, 10, 50, 100 ms) plus one unbounded reading, ascending, from seal.mjs `sealFrontier`.

   THE HEADLINE FINDING IS THE SHAPE OF THE CURVE, not any one of its six points. On the 2026-07-29 board
   agentgateway carried 23,630 req/s under a 1 ms tail and gained 7% by dropping the bound entirely, while
   apisix went 10,697 → 19,339 by letting its tail out from 1 ms to 5 ms and apisix anthropic>anthropic
   nearly tripled across the range. Published as one number those three read as comparable machines. They
   are not, and the difference was in the data the whole time. So every surface that shows a throughput
   number here shows the SHAPE beside it (frontierSpark + the gain factor), and the bound the number was
   read at is NAMED on the column and switchable by the reader. */
// THE DECLARED BOUNDS, in milliseconds. A MIRROR of seal.mjs FRONTIER_BOUNDS_MS / DEFAULT_BOUND_MS, which
// mirrors the engine's `frontier::P99_BOUNDS_US` - app.js is loaded as a plain <script> in the browser and
// cannot import the module, so the list is duplicated and the duplication is CHECKED: site/test.mjs asserts
// these two constants equal seal.mjs's, so a bound added on one side cannot quietly shorten the other's table.
const FRONTIER_BOUNDS_MS = [1, 5, 10, 50, 100];
// WHICH BOUND THE BOARD OPENS ON. A VIEW, NOT A VERDICT (seal.mjs says why 10 ms): it decides which column
// opens first, never which measurement exists. Every bound is published on every cell and the reader can
// switch, which re-ranks the board in front of them.
const DEFAULT_BOUND_MS = 10;
// The readings in published order: each declared bound, then the unbounded one (`null` = no latency bound,
// zero failures only). `null` is a first-class choice here, not a missing value.
const BOUND_CHOICES = [...FRONTIER_BOUNDS_MS, null];
// A bound's short name, for a control or a column header.
function boundLabel(ms) { return ms == null ? "no bound" : `${ms} ms`; }
/* THE PHRASING WAS SETTLED WITH THE OWNER: "18,995 req/s while 99% of requests finished under 10 ms."
   Not "rps at 10 ms", which reads as a category error - the 10 ms is not a rate and the reading is not a
   latency. Every caption, tooltip and column header renders the clause from here, so no surface can state
   the bound in a way that implies the gateway was held to a target it was not. */
function boundClause(ms) {
  return ms == null
    ? "under no latency bound at all, having failed no request it accepted"
    : `while 99% of requests finished under ${ms} ms`;
}
// The column header for a reading at `ms`. Names the bound it is showing, always: the retired board's
// captions claimed "p99 < 1 s" while the engine enforced 20 ms - a bar 96% of rungs pass against 57% for
// the real one - so readers reasoned about a test that never ran.
// STILL THE STANDALONE FORM, used by the Performance tab's single ranked column and by the compare panel,
// where there is nothing beside it to share a group header with.
function boundColLabel(ms) { return ms == null ? "Req/s · no bound" : `Req/s · 99% under ${ms} ms`; }
/* The SPANNING group header over the Frontier tab's six per-bound columns, which say only "1 ms", "5 ms",
   ... "no bound" beneath it. The shared clause is stated once here instead of six times; it must carry the
   same "99% of requests" qualifier boundClause() does, because the group header is now the only place the
   99% appears on that table. */
const BOUND_GROUP_LABEL = "Req/s · 99% of requests under:";
// The frontier tab's per-bound column id. Stable, so a shared ?sort=f10 keeps resolving.
function boundColId(ms) { return ms == null ? "fnone" : `f${ms}`; }
// The reader's currently-selected bound, read defensively: these helpers are called from renderers the
// node suite drives with a hand-built state, and from column labels that take no arguments.
function selectedBound(st = (typeof state !== "undefined" ? state : null)) {
  if (!st || !("bound" in st)) return DEFAULT_BOUND_MS;
  return st.bound === null || FRONTIER_BOUNDS_MS.includes(st.bound) ? st.bound : DEFAULT_BOUND_MS;
}
// A TAIL LATENCY as a reader reads it. Sub-millisecond tails are real and common (agentgateway's 1 ms
// column sits at 584 µs), so they keep their own unit rather than rounding to "0.6 ms".
function fmtTail(us) { return us < 1000 ? `${fmtInt(us)} µs` : `${fmt1(us / 1000)} ms`; }
// frontierOf(rec): the record's readings, or [] - which is what EVERY snapshot measured before the
// frontier existed carries, so an empty array is the normal shape of an old record and not an error.
function frontierOf(rec) { return rec && Array.isArray(rec.frontier) ? rec.frontier : []; }
/* frontierAt(frontier, boundMs): the reading taken at one bound. Mirrors seal.mjs's accessor of the same
   name (and charts.py's `_frontier_at`), so the table, the drawer, the compare panel and the charts all
   read the SAME reading for the same bound and cannot disagree about which column they are showing.
   `null` selects the unbounded reading; it is a bound value, never "unset". */
function frontierAt(frontier, boundMs) {
  if (!Array.isArray(frontier)) return null;
  return frontier.find((r) => (boundMs == null ? r.bound_ms == null : r.bound_ms === boundMs)) || null;
}
// A cell with no frontier at all shows NO THROUGHPUT - never a 0, and never a blank that reads as one
// (requirement: a legacy row and a gateway not yet re-measured are the same state, and both are absence).
const NO_FRONTIER_NOTE = "no frontier in this record: it was measured before the throughput frontier " +
  "existed, or this cell has not been re-measured. There is no throughput to show - which is not the same " +
  "as a throughput of zero.";
/* readingSentence(rd, v): the whole reading, in words, with its own evidence attached. Both halves of the
   proof travel because the engine publishes both (frontier.rs: `concurrency` is where the winning rate was
   observed, `first_disqualified_conc` is the lowest concurrency above it that stopped qualifying).
   `p99_us` IS THE OBSERVED TAIL, NEVER THE BOUND. 4 ms under a 100 ms bound and 99 ms under it are very
   different findings, and a tooltip that echoed the bound back would restate the question as the answer. */
function readingSentence(rd, v) {
  /* fmtRate, not fmtInt: this sentence explains the cell beside it, and rounding here put
     "0 req/s" one hover away from a cell reading 0.25 - the same false statement the engine and the
     cell renderer were both changed to stop making. */
  const bits = [`${fmtRate(v)} req/s ${boundClause(rd.bound_ms)}.`];
  if (rd.concurrency != null) bits.push(`Observed with ${fmtInt(rd.concurrency)} concurrent requests in flight.`);
  if (rd.p99_us != null) bits.push(`The tail it actually produced there was ${fmtTail(rd.p99_us)}.`);
  if (rd.lower_bound === true)
    // A FLOOR, NOT A CEILING. The sweep ran out of ladder while this rung was still qualifying, so the
    // rate is real and maximality is not established. The retired search published null for this state,
    // discarding a measured rate for failing to prove something the reader never asked it to prove.
    bits.push("The sweep ran out of ladder with this concurrency still qualifying, so this is a FLOOR (≥), " +
      "not a maximum: the gateway carries at least this much and we did not look higher.");
  else if (rd.first_disqualified_conc != null)
    bits.push(`The next concurrency probed above it (${fmtInt(rd.first_disqualified_conc)}) stopped qualifying, which is what establishes this as the boundary.`);
  return bits.join(" ");
}
/* frontierCell(rec, boundMs): the {v, text, na, note} cell shape every table column and popup uses, for
   one record at one bound. The RATE is read through metric() like every other published number, so an
   absent reading surfaces the engine's own reason (frontier.rs `absence_for` distinguishes "nothing served
   cleanly anywhere" from "nothing held THIS bound") instead of a flattened hole.
   A `lower_bound` reading renders "≥ 19,000": the floor is in the glyph, not only in the tooltip. */
function frontierCell(rec, boundMs) {
  const f = frontierOf(rec);
  if (!f.length) return { v: null, text: "no frontier", na: true, note: NO_FRONTIER_NOTE };
  const rd = frontierAt(f, boundMs);
  if (!rd)
    return { v: null, text: "n/a", na: true,
      note: `this record publishes no reading at ${boundLabel(boundMs)}` };
  const c = metric(rd.rps, fmtRate);
  if (c.na) return c;   // the engine's absence reason, rendered by the one accessor
  /* "MEASURED, AND IT CANNOT DO THIS" IS NOT "NOT MEASURED", and the two must never look alike.
     `below_resolution` is how the engine says "rungs served cleanly, but NONE held THIS bound, so the
     gateway carried no measurable throughput under it" (frontier.rs `absence_for`). On the field data that
     is the majority of some rows: plano's tail is ~890 ms at c=8, so five of its six columns are this state
     and only the unbounded reading carries a rate. A dash or an "n/a" there would read as "no data", which
     is a NEUTRAL impression of a DAMNING measurement - and it would flatter the slowest gateways on the
     board, which is the worst direction for an error like this to run.
     So it ranks as the 0 it is (metric() already does that, which is why it is not `na`), and it SAYS what
     it is on the cell: "0" with "no rung held this tail" underneath, distinct at a glance from the "no
     frontier" of a record that was never measured at all. The engine's own prose stays on the tooltip.
     No reading sentence is composed on top: "0 req/s while 99% of requests finished under 1 ms" describes a
     measurement that did not happen, and it would talk over the reason that did.
     Detected through mcode(), not by reading the envelope's raw `.value` (invariant C5 forbids that here,
     and rightly: the display rule must live in the accessors). mcode is exactly the accessor that returns
     null for a below-resolution absence while mval coerces it to 0. */
  if (mcode(rd.rps) == null) return { ...c, text: "0", why: "no rung held this tail", reading: rd };
  const floor = rd.lower_bound === true;
  return { ...c, text: floor ? `≥ ${c.text}` : c.text, note: readingSentence(rd, c.v), reading: rd, floor };
}
/* frontierHeld(frontier): THE SHAPE, AS ONE NUMBER — what FRACTION of its full rate the cell still carried
   at the tightest tail-latency bound it holds at all.

   IT REPLACED A GAIN FACTOR ("×1.0 from 1 ms"), AND THE REASON IS THE OWNER READING HIS OWN COLUMN:
   "its just not clear what this means, even I know and i cant figure it out". A gain factor made the reader
   assemble one sentence out of three scattered pieces - the multiplier (of WHAT?), "from 1 ms" (to what?),
   and the missing half of that ("to no bound") stranded up in the column header. Worse, ×1.0 was the BEST
   possible result and read like an unfilled default. A percentage of full rate is the same measurement with
   none of that: the direction is obvious without a legend (more is better), and the number is a share of a
   quantity the reader can see in the columns to the left.

   THE DENOMINATOR IS THE UNBOUNDED READING, never the 100 ms one and never a max across bounds. The frontier
   is monotone by construction (a looser tail can only admit more concurrency), so the unbounded reading IS
   the maximum and "full rate" is well defined. If it is somehow absent while a bounded reading exists we
   publish NO percentage rather than promote the 100 ms reading into a denominator it is not - that would
   silently rebase one row against a different quantity from every other row while looking identical to them.

   THE NUMERATOR IS THE TIGHTEST BOUND THAT HAS A READING, and the cell NAMES that bound, because for most of
   the field it is not 1 ms. This is the fact a bare factor destroyed: on the 2026-07-30 data litellm-rust and
   tensorzero both read "×1.0" - the same six characters - while the first runs at full rate with a 0.56 ms
   tail and the second cannot serve one request under 10 ms. "99% of its full rate at 1 ms" and "99% of its
   full rate at 50 ms" are still one form, so they are still comparable, but they can no longer be mistaken
   for the same finding. */
function frontierHeld(frontier) {
  const f = Array.isArray(frontier) ? frontier : [];
  const tight = f.find((r) => r.bound_ms != null && mval(r.rps) > 0);
  const loose = frontierAt(f, null);
  const held = tight ? mval(tight.rps) : null, full = loose ? mval(loose.rps) : null;
  if (held == null || full == null || !(held > 0) || !(full > 0)) return null;
  return { frac: held / full, boundMs: tight.bound_ms, held, full, lowerBound: loose.lower_bound === true };
}
/* heldPct(frac): the fraction as a whole percent, WHICH NEVER READS 100% UNLESS THE CURVE IS EXACTLY FLAT.
   Rounding 99.6% up to "100% of its full rate" would assert the gateway loses nothing at all to a tight tail
   when its own two readings say it loses something, and that assertion is the exact class of overclaim this
   column exists to remove. So anything short of equality floors at 99, and only held === full - two readings
   that are the same number - is allowed to print 100.
   Whole percent rather than a decimal because the discriminating differences in the field are tens of points
   (31%, 56%, 66%, 93%), and a tenth of a percent off a single concurrency sweep is not a difference we
   measured. */
function heldPct(frac) { return frac >= 1 ? 100 : Math.min(99, Math.round(frac * 100)); }
/* heldSortKey(originIndex, frac): ONE number ranking the column by (bound-of-origin, then share of full rate),
   with BIGGER = BETTER, so the column's descending default puts the gateways that hold their rate at the
   tightest tail on top - where the column's own question ("what does it still carry when you demand a tight
   tail") points.
   Origin dominates, and it has to: 99% at 50 ms and 99% at 1 ms are answers to different questions, and
   ranking them on the share alone would file a gateway that serves nothing under 10 ms beside one running at
   full rate at 0.56 ms. Origin is INVERTED (a tighter bound scores higher) and scaled by 2, so each origin
   group owns a disjoint interval of width 2 while `frac` spans [0, 1] - no share, not even an exactly flat
   1.0, can leak into the neighbouring group. */
function heldSortKey(originIndex, frac) { return (HELD_NOTHING_INDEX - originIndex) * 2 + frac; }
/* The origin index a cell that held NOTHING under any published bound sorts at: one past the loosest bound,
   which through heldSortKey's inversion makes it the bottom of the ranking.
   plano carried nothing under ANY declared bound and 19 req/s unbounded, so there is no share of full rate to
   state - and that is the most extreme shape on the board, not a missing measurement. It must not carry
   `v: null`, which rowComparator sinks to the bottom regardless of direction: the single worst curve in the
   field would then sort as though it had not been measured. */
const HELD_NOTHING_INDEX = FRONTIER_BOUNDS_MS.length;
/* The reference paragraph for the column, rendered BELOW the table (see captionText). It states the two
   things six words on a cell cannot: which reading is the denominator, and why the named bound is part of the
   number rather than a footnote. */
const HELD_REFERENCE = `"N% of its full rate at B" in the last column is what the cell still carried at B, the TIGHTEST tail-latency bound it holds any rate at, as a share of its full rate with no latency bound at all. 99% at 1 ms is the good shape: the gateway gives up almost nothing even when you demand a 1 ms tail. A low percentage means it needs a loose tail to go fast. The bound matters as much as the percentage - 99% at 50 ms is not a flat gateway, it is a gateway that holds no rate at all under 50 ms - so the column sorts by that bound first and the percentage second, and a cell that held nothing under any published bound says so in words instead of showing 0%.`;
/* frontierSpark(frontier, opts): the curve, drawn.
   WHY A SPARKLINE AND NOT SIX NUMBERS. The finding is a SLOPE - "flat" vs "nearly doubles by 5 ms" - and a
   slope is something the eye reads in one pass and a row of digits is not. Six numbers per row across
   fourteen rows is 84 figures a reader has to hold in their head to notice that two gateways with similar
   headline rates are not the same machine at all. The line makes that difference pre-attentive; the gain
   factor beside it carries the same fact in text for sorting and for a screen reader.
   THE Y SCALE IS SHARED ACROSS THE BOARD (opts.min/opts.max, from boardFrontierScale), NOT per-row. Per-row
   auto-scaling would draw every gateway's curve full-height, so a 7% climb and a 170% climb would look
   identical - the exact defect the RSS sparkline was fixed for.
   AND IT IS LOGARITHMIC, which is what makes the shape the thing the eye reads. On a log axis equal SLOPES
   are equal RATIOS, and the ratio IS the finding: ×1.07 (holds its rate under a tight tail) and ×2.7 (needs
   a loose tail to go fast) are two visibly different slopes whatever the gateway's absolute level. A shared
   LINEAR axis cannot do that at this field's spread - the 2026-07-30 run has litellm-rust at 44,363 req/s
   and plano at 19 - because plano's entire curve would collapse onto the baseline and the row would read as
   broken rather than as slow. The charts moved to a log y axis for the same reason.
   A BOUND THE GATEWAY SERVED BUT COULD NOT HOLD IS DRAWN ON THE FLOOR, not skipped. That state is
   `below_resolution` - "no rung held this tail" - and it is a MEASUREMENT, so a gap there (which is what a
   never-measured bound gets) would turn plano's five damning columns into five shrugs. On the floor, with an
   open tick, the row reads as what it is: flat on the bottom until the bound is dropped entirely.
   x IS THE BOUND'S INDEX, not its value: the bounds are ticks decided by where the field's p99 population
   separates (1, 5, 10, 50, 100), so spacing them linearly by value would crush the first three columns -
   where nearly all of the movement is - into the left 10% of the line. */
function frontierSpark(frontier, opts = {}) {
  const f = Array.isArray(frontier) ? frontier : [];
  /* One point per PUBLISHED reading position, so a missing bound leaves a gap rather than shifting the curve
     left and mis-stating which bound a point belongs to. Three states, deliberately kept apart:
       a rate           -> a point at its level
       a measured 0     -> a point ON THE FLOOR (`onFloor`), because the gateway held nothing under this tail
       no reading / no rate for another reason -> nothing, a genuine gap */
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
  // A DECADE OF HEADROOM BELOW THE FIELD'S FLOOR, so the slowest row's own points sit above the baseline
  // rather than on it - the baseline is reserved for "held nothing under this tail", which is a different
  // statement and must not be confused with "slowest on the board".
  const lo = Math.max(Math.min(loSeed, ...rates) / 2, 1);
  const l0 = Math.log10(lo), l1 = Math.log10(Math.max(hi, lo * 2));
  const x = (i) => PAD + (i / (BOUND_CHOICES.length - 1)) * (W - 2 * PAD);
  const y = (v) => (v > 0
    ? PAD + (1 - (Math.log10(Math.max(v, lo)) - l0) / (l1 - l0)) * (H - 2 * PAD - 3)
    : H - PAD);
  const path = pts.map((p, i) => `${i ? "L" : "M"}${x(p.i).toFixed(1)},${y(p.v).toFixed(1)}`).join("");
  // The SELECTED bound is marked on the curve, so the reader can see which point of the shape the ranked
  // column is reading. A number in a column and a shape beside it that do not say how they relate is two
  // facts a reader has to join up themselves.
  const selIdx = BOUND_CHOICES.indexOf(opts.boundMs === undefined ? null : opts.boundMs);
  const rule = selIdx >= 0
    ? `<line x1="${x(selIdx).toFixed(1)}" y1="${PAD}" x2="${x(selIdx).toFixed(1)}" y2="${H - PAD}" ` +
      `stroke="currentColor" stroke-opacity="0.35" stroke-width="1" stroke-dasharray="2 2"/>`
    : "";
  /* THREE MARKERS, THREE CLAIMS:
       filled dot  - an established ceiling at that bound
       open dot    - a FLOOR: the sweep ran out of ladder with that concurrency still qualifying, so a
                     filled dot identical to a proven ceiling would state something the sweep did not
       floor tick  - served, but nothing held this tail (a measured nothing, not a missing measurement) */
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
/* boardFrontierScale(data): the SHARED log domain for every sparkline on the board - the lowest and highest
   rate any row publishes at any bound. Computed over the whole bundle rather than the filtered rows, so the
   scale does not move when a reader types in the search box: a curve that changes shape because a different
   gateway was filtered out is a curve that cannot be trusted. Zero rates are excluded from the floor, since
   "held nothing under this tail" is drawn on the baseline rather than placed on the scale. */
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
   `data.definitions` is a metric-key -> prose map GENERATED FROM THE ENGINE'S CONSTANTS
   (engine/src/suite.rs `metric_definitions`), stating for each metric what quantity it is, which
   observations counted, and how the measurement knew to stop.
   IT IS SURFACED RATHER THAN FILED because the failure it exists to prevent is a reader reasoning
   carefully about a test that never ran: every published surface described the retired throughput gate as
   "p99 < 1 s" while the engine enforced 20 ms. A definition a reader has to leave the page to find is a
   definition nobody reads, so each table and each drawer lane carries a fold with the definitions for the
   metrics IT shows, verbatim from the engine.
   SELECTED BY KEY PREFIX, never by an enumerated list: a definition the engine adds under `perf.` appears
   on the Performance surfaces with no change here, which is the only way this cannot go stale. */
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
/* definitionsFold(prefixes, data): the collapsed "What these numbers mean" block. Collapsed by default so
   it costs no vertical space until asked for (the board's whole layout rule is that less scrolling wins),
   and rendered as prose exactly as the engine wrote it - reworded here it would be a second source of
   truth, which is the defect the generated map exists to close. Returns "" when the bundle carries none. */
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
/* hasCost(data): does ANY gateway on this board carry a measured cost-per-request?
   The cost columns follow the per-cell-memory precedent above and appear only when the board can
   answer them. This is not hiding an absence: every OTHER column shows "not measured" per row,
   because a row that lacks one metric still has the rest. A cost column on a board measured before
   the capture existed would be n/a on EVERY row - a column that asks a question nothing on the page
   can answer, which is noise rather than disclosure. It lights up by itself on the first board that
   carries the field. */
const COST_CACHE = new WeakMap();
/* costWindowConc(data): the concurrency every gateway's CPU-per-request was measured at.
   THE NUMBER THAT STOPS THE COST FIGURE BEING MULTIPLIED BY THE PEAK RATE. The board publishes a peak
   req/s at the concurrency the frontier chose (busbar: 82,328 at c=64) and a CPU-per-request at a
   DIFFERENT, fixed concurrency held identical for every entrant (c=8). Both are labelled in the
   artifact - `cost_window_conc` has been in every snapshot - but nothing rendered it, so the page
   showed "53.6 µs" beside "82,328 req/s" with no sign they came from different windows. Multiply them
   and you get 4.41 cores on a 4-core box: an impossible number assembled from two correct ones, which
   is the most damaging way for a real measurement to be read.
   Read from the data rather than hardcoded, because the engine's COST_WINDOW_CONCURRENCY can change
   and a stale literal here would mislabel every row. Null when the board carries no cost. */
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
    // THROUGH mval(), not a raw .value read. C5 exists because a raw read bypasses the
    // below_resolution rule (an envelope whose value is null but whose reason means "measured, too
    // small to weigh" IS a measurement), and the lint caught this line the first time it was
    // written the other way.
    return cells.some((c) => mval(c.cpu_us_per_request) != null);
  });
  COST_CACHE.set(data, yes);
  return yes;
}
/* rigResolutionPct(data): the smallest DIFFERENCE this rig can actually resolve, derived from the
   board itself rather than chosen.

   Every box runs the SAME qualification before it measures anything, and `box_qualify.drift_pct` is
   how far that box landed from the shared baseline. Those boxes are identical by construction - same
   instance type, same image, same mock - so the spread between the luckiest and unluckiest box is a
   direct measurement of what the rig cannot tell apart. Two gateways closer than that did not
   demonstrate a difference; they demonstrated which box they happened to land on.

   DERIVED, NOT PICKED. A hard-coded "1%" or "2%" would be a rule nobody measured deciding which
   published comparisons count - and this project's whole position is that no undeclared rule may
   author a number. The figure moves with the fleet: a tighter fleet resolves finer, and a noisier one
   honestly admits it resolves less.

   Null when fewer than two boxes report a drift: with one box there is no spread to observe, and
   inventing a floor from a single sample would be the magic number this exists to avoid. */
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

   WHAT THIS REPLACED. The board used to ship 25 static PNGs drawn by a python script: nine metrics,
   each rendered twice (full field and "top 5"), plus three frontier views. They were a SECOND SURFACE
   publishing the same numbers, and on 2026-07-31 the chart toolchain silently failed to regenerate and
   shipped one run's images beside another run's data.

   A picture cannot answer a question asked after it was drawn, and that is not a small complaint here:
   half those files existed because "top 5" had to be decided at render time, by one metric, before the
   reader arrived. A tab that re-draws answers it at read time instead - change the bound, change the
   cell, change the metric, and every chart follows.

   ONE REGISTRY, because the metrics are structurally identical: a number per gateway, a direction, a
   formatter. A chart per metric would be nine near-copies drifting apart. */
const CHART_METRICS = [
  { id: "cpu", label: "CPU per request", unit: "µs", log: true, desc: false,
    note: "Microseconds of gateway CPU per completed request, measured at one concurrency held identical for every gateway. Lower is better.",
    get: (g, st) => mval((chooserCellPerf(g, st) || {}).cpu_us_per_request) },
  { id: "rpsdollar", label: "Requests per $/hr", unit: "req/s per $/hr", log: true, desc: true,
    note: "Requests per second per dollar of hourly instance cost, at the selected bound. Higher is better.",
    get: (g, st) => mval((chooserCellPerf(g, st) || {}).rps_per_dollar) },
  { id: "permillion", label: "Cost per million requests", unit: "USD", log: true, desc: false,
    note: "Instance cost to serve a million requests at the selected bound. Lower is better.",
    get: (g, st) => mval((chooserCellPerf(g, st) || {}).cost_per_million_usd) },
  { id: "lat", label: "Added latency (p99)", unit: "µs", log: true, desc: false,
    note: "Gateway p99 minus direct-to-mock p99 at concurrency 1. Lower is better.",
    get: (g, st) => mval((chooserCellPerf(g, st) || {}).added_latency_p99_us) },
  { id: "rps", label: "Throughput at the selected bound", unit: "req/s", log: false, desc: true,
    note: "The most requests/sec the chosen cell carried while 99% of requests finished under the selected bound. Higher is better.",
    get: (g, st) => { const r = frontierAt(frontierOf(chooserCellPerf(g, st)), selectedBound(st)); return r ? mval(r.rps) : null; } },
  { id: "rss", label: "Peak memory", unit: "MiB", log: false, desc: false,
    note: "Highest resident memory observed while the fixed load ran on the chosen cell. Lower is better.",
    get: (g, st) => mval((chooserCellMemory(g, st) || {}).peak_rss_mib) },
];

/* LOG SCALE IS NOT A PREFERENCE ON SOME OF THESE, IT IS THE ONLY HONEST AXIS.
   Cost per request spans 89 µs to 199,333 µs on the current board - 2,247x. On a linear axis twelve
   gateways collapse into a single pixel beside the slowest one, which renders the comparison the chart
   exists to make unreadable. Metrics whose spread is bounded (throughput, memory) stay linear, because
   a log axis there would flatten differences that are real and readable. */
function chartRows(metric, gateways, st) {
  const rows = [];
  for (const g of gateways || []) {
    const v = metric.get(g, st);
    if (typeof v === "number" && Number.isFinite(v)) rows.push({ key: g.key, name: g.name || g.key, v, g });
  }
  rows.sort((a, b) => (metric.desc ? b.v - a.v : a.v - b.v));
  return rows;
}

/* costSaturation(cell): whether a cell's core utilisation may be read as a saturation verdict.

   THE NUMBER ALONE IS MISLEADING AND I MISREAD IT MYSELF. The cost window runs at ONE concurrency,
   held identical for every gateway, which is what makes cost per request comparable. But a gateway
   whose peak needs far more concurrency is barely loaded there - tensorzero's window carried 200 rps
   against a 13,303 peak, 2% of it - so its idle cores say nothing whatever about whether it
   saturates at its peak. I wrote "tensorzero is not CPU-bound" from exactly that figure.

   one-api is the case that IS a verdict: 95% of its peak at that same concurrency and still using
   2.6% of its cores. Same utilisation shape, opposite meaning, and only the ratio distinguishes them.

   So this returns the utilisation WITH the ratio that qualifies it, and `verdict` is null whenever
   the window did not get close enough to the peak for the question to be answerable. A caller cannot
   render the number without also holding the reason it may or may not be believed. */
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

/* tiedRuns(rows, col, state, pct): the keys of rows the SORTED column cannot separate from the row
   directly above them.

   A table sorted by a column implies the order carries information. When two adjacent values are
   closer than the rig's own resolution, the order between them records which box they landed on -
   not a finding. Marking that boundary is the difference between publishing a measurement and
   publishing a coin toss with a decimal point.

   ONLY THE SORTED COLUMN, because a tie on some other column is not what the reader is being shown a
   ranking of. And nothing is marked when the resolution is unknown (a single box, so no spread to
   observe): asserting a tie needs a figure, and we do not invent one. */
function tiedRuns(rows, col, st, pct) {
  const out = new Set();
  if (pct == null || !col || col.render || typeof col.get !== "function") return out;
  for (let i = 1; i < rows.length; i++) {
    const a = col.get(rows[i - 1], st), b = col.get(rows[i], st);
    if (a && b && indistinguishable(a.v, b.v, pct)) out.add(rows[i].key);
  }
  return out;
}

/* indistinguishable(a, b, pct): are two published values closer than the rig can resolve?
   Relative to the LARGER value, so the comparison means the same thing at 19 rps and at 49,000. */
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
    // NO VERDICT WORDING. The rate, its sign and its units ARE the finding; "NEVER SETTLED" in caps on
    // top of them was the board passing judgement on a gateway instead of publishing a measurement.
    // Nothing is dropped - every reading these branches carried still prints.
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

/* DELETED: `ZERO_RPS_NOTE` / `withZeroNote(cell)`, which annotated a throughput 0 with "served, but no
   tested load held p99 < 1 s at <0.1% errors". Its only two callers were the two retired scalar columns,
   and the sentence was doubly wrong by the end: no gate ever enforced 1 s (the engine used 20 ms), and the
   frontier does not grant a 0.1% error tolerance at all - a rung qualifies only if it failed nothing it
   accepted. A frontier reading that found no qualifying rung is an ABSENCE carrying the engine's own reason
   (frontier.rs `absence_for` separates "nothing served cleanly anywhere" from "nothing held THIS bound"),
   so there is no 0 left here for a note to explain. The equivalent annotation for the streaming zeros it
   never covered lives where it always did, in METRIC_NOTES. */

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
  /* SAME READS THE CELL, NOT THE PROJECTED HEADLINE. `canonicalStreaming(g)` is ONE record - the
     diagonal the headline was projected from - so asking for any OTHER diagonal returned null even when
     the matrix carried a fully measured cell for it. On the 2026-07-31 board that hid real streaming
     numbers for 9 of the 14 gateways under "Same -> Anthropic": agentgateway had 468us TTFT and 4,356
     sustained streams, busbar had 264us, litellm-rust 217us, and every one of them rendered n/a beside
     the five gateways that genuinely cannot serve the pairing. Two opposite findings, one label.

     Same and Custom differ only in that Same names one dialect for both ends, so they read the matrix
     the same way. The diagonal-only rule below was never a measurement fact - it was the projection's
     shape leaking into the chooser. */
  if (st.mode === "same") {
    const upSame = g.matrix && g.matrix.upstreams && g.matrix.upstreams[ingress];
    const cellSame = upSame && upSame.cells && upSame.cells[ingress];
    const rawSame = cellSame && cellSame.served === true && cellSame.stream
      && cellSame.stream.stream_served === true ? cellSame.stream : null;
    return rawSame ? stampChosen({ stream_served: true, ...rawSame }, g, ingress, ingress, "stream-") : null;
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
/* The delta for a chosen cell vs the gateway's OWN REPRESENTATIVE diagonal (best_cell): "+18% latency,
   -9% req/s". The NAME `deltaToPeak` is legacy and the reference is not a peak - best_cell prefers the
   openai diagonal and otherwise ranks on added latency, never on throughput (gen-data.mjs `bestCell`), so
   this delta can and does come out POSITIVE on req/s and that is not an anomaly. Every label it feeds says
   "its own cell" rather than "peak" for exactly that reason.
   Returns "" for the reference cell itself, or when either reference number is missing. Honest by construction:
   mval() returns null for an absent envelope, so a hole never enters the delta.
   THE THROUGHPUT HALF IS COMPARED AT ONE NAMED BOUND, and the caller states which. It used to compare
   `rps_sustained_20ms`, one collapsed reading against another; comparing two frontier readings taken at
   DIFFERENT bounds would be a percentage between two different questions, so both sides read the same bound
   and a cell with no reading there contributes nothing rather than a number built out of two. */
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
  // The label states the measurement (there is no steady-state level to publish), not a verdict on the
  // gateway: "never settles" read as the board calling a gateway out, and the rate below carries the
  // finding on its own.
  const label = cleared ? "no steady state (no growth)" : "no steady state";
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
    // NO PILL BESIDE THE NAME. The plateau verdict used to ride here on the memory tab, and a red
    // NEVER SETTLES tag on a gateway's name reads as a verdict on the GATEWAY rather than on one
    // window of one measurement - which is a much larger claim than the metric supports. The finding
    // is not hidden: the Growth column carries the rate, and the per-cell tooltip says what each
    // window did and, when it did not settle, whether it climbed or merely swung.
    return `<td class="name">${a}</td>`;
  },
};
// The "Tested on" column: present in EVERY mode (identical column set across Own cell/Same/Custom). It reads
// the CHOSEN cell's path (chooserDialects) so it always names the exact cell the row's numbers were
// measured on — Own cell: each gateway's own representative dialect (varies per row); Same: the chosen dialect on every
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
  // A FRONTIER READING IS A DISPLAYED VALUE, and it is not an envelope ON the record - the record carries an
  // ARRAY of readings, each with its own sealed rate. Without this clause a cell whose throughput published
  // and whose added-latency did not (the whole point of per-metric absences) would fail the all-or-nothing
  // test and paint "n/a" over a row that shows six real rates: the plano rule inverted into hiding a
  // measurement rather than advertising a missing one.
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
/* THE TWO RETIRED THROUGHPUT CELLS. `sustainedChooserCell` and `maxProxyChooserCell` read
   `rps_sustained_20ms` and `rps_max_proxy`, which no producer emits: they were one sweep collapsed to one
   number by a chosen ceiling, twice, by two algorithms that contradicted each other in the field (a
   "maximum" of 16,232 against the same cell's sustained 16,610). Their replacement is one reader over the
   frontier, at a bound the reader NAMES - and it shows the concurrency inline exactly as they did, because
   "18,995 @ 8 conc" was the useful half of what they published. */
// frontierChooserCell: the chosen cell's frontier reading at the reader's selected bound, as a table cell.
// The concurrency rides inline ("18,995 @ 8 conc") and the full reading sentence - the observed tail, the
// boundary proof, or the floor disclosure - rides on the tooltip.
function frontierChooserCell(g, st = state, boundMs = selectedBound(st)) {
  const p = chooserCellPerf(g, st);
  if (!p) return { v: null, text: "n/a", na: true };
  const cell = frontierCell(p, boundMs);
  const rd = cell.reading;
  if (cell.na || !rd || rd.concurrency == null) return cell;
  return { ...cell, text: `${cell.text} @ ${fmtInt(rd.concurrency)} conc` };
}
// frontierBoundCell: the chosen cell's reading at ONE NAMED bound, for the frontier tab's per-bound
// columns. No inline concurrency (six columns of "@ N conc" is noise); the number and its observed tail are
// what a reader compares across the row, and the tooltip carries the rest of the evidence.
function frontierBoundCell(g, boundMs, st = state) {
  const p = chooserCellPerf(g, st);
  if (!p) return { v: null, text: "n/a", na: true };
  return frontierCell(p, boundMs);
}
/* frontierFullRate(frontier): the UNBOUNDED reading's rate, i.e. the denominator that "full rate" means.
   Named, and read through the same accessor as every other rate, so the share stated in the shape column and
   the number rendered in the "no bound" column can never come from two different readings. */
function frontierFullRate(frontier) {
  const rd = frontierAt(Array.isArray(frontier) ? frontier : [], null);
  return rd ? mval(rd.rps) : null;
}
/* frontierShapeCell: the SHAPE column - the sparkline plus, in words, what share of its full rate the cell
   still carried at the tightest tail it holds.
   Sortable BY THAT SHARE, deliberately: "which gateways need a loose tail to go fast" is a question about the
   field that no single-bound ranking can answer, and it is the question the frontier exists to expose. `v` is
   heldSortKey, which ranks bound-of-origin first and the share within it, because a share read at 1 ms and one
   read at 50 ms are not one quantity (see heldSortKey); the render puts the curve beside it. */
function frontierShapeCell(g, st = state) {
  const p = chooserCellPerf(g, st);
  const f = frontierOf(p);
  if (!f.length) return { v: null, text: "n/a", na: true, note: p ? NO_FRONTIER_NOTE : "" };
  const h = frontierHeld(f);
  if (!h) {
    const full = frontierFullRate(f);
    const anyBounded = f.some((r) => r.bound_ms != null);
    /* THE DENOMINATOR IS MISSING. Structurally it cannot be - every cell publishes a reading at every declared
       bound plus the unbounded one - but a share needs a whole to be a share OF, and promoting the 100 ms
       reading into that role would rebase this one row against a different quantity from every other row on
       the board while looking identical to them. So: no percentage. Not 100%, and not a 0. */
    if (full == null || !(full > 0) || !anyBounded) {
      return { v: null, text: "n/a", na: false, frontier: f,
        note: "No share of full rate can be stated: this cell has no unbounded reading for the bounded ones to " +
          "be a share of. The curve beside this is what was measured." };
    }
    /* HELD NOTHING, ANYWHERE - and that is still a curve, and it is the most damning shape on the board:
       plano carried nothing under ANY declared bound and 19 req/s unbounded. The sparkline still draws (five
       ticks on the floor and one point at the right, which reads as "it cannot meet any bound we publish");
       what is withheld is only the percentage, because a share of a rate the gateway never reached under any
       bound is not a number. Rendering the whole cell n/a here would delete the finding for exactly the
       gateways it is about, and a "0%" would claim a share at a bound where no rung qualified at all.
       IT SAYS SO IN WORDS RATHER THAN DRAWING A DASH. The owner read the bare "—" as missing data, which is
       the one thing it is not: plano SERVED, cleanly, and no concurrency it was offered kept 99% of requests
       under even the loosest bound we publish. A dash is the neutral rendering of a damning measurement. */
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
  /* THE BOUND IS PART OF THE NUMBER, so it is part of the text - on EVERY row, including the 1 ms rows. A
     reader must never have to know which bound is the default to know whether "99%" means "full rate at the
     tightest tail we publish" or "full rate, but only once you allow 50 ms". */
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
/* frontierShapeTd(g, st): the shape column's <td>, for BOTH tabs that carry it.
   ONE renderer, because the two used to be a copy-paste pair and the Frontier tab is where this column is
   read most: any fix applied to one and not the other would ship a board that disagrees with itself about the
   same cell.
   The held-nothing row gets the MEASURED-ZERO treatment the reading columns use - muted cell, the statement
   itself in the same ink a "no rung held this tail" carries - so a reader never has to hover to learn that
   the row without a percentage is the strongest finding in the column. It is keyed off `zero`, not off the
   presence of a second line, because that row's whole content IS the statement: a mark plus a why underneath
   would be the same sentence twice. */
function frontierShapeTd(g, st = state) {
  const c = frontierShapeCell(g, st);
  if (c.na) return `<td class="shape na" title="${esc(c.note || "")}">${esc(c.text)}</td>`;
  const label = c.zero ? `<span class="reading-none">${esc(c.text)}</span>` : esc(c.text);
  return `<td class="shape${c.zero ? " reading-zero" : ""}" title="${esc(c.note)}">` +
    `${frontierSpark(c.frontier, { ...boardFrontierScale(stateData(st)), boundMs: selectedBound(st) })}` +
    `<span class="shape-gain">${label}</span></td>`;
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
    /* THE RANKED THROUGHPUT COLUMN, at the bound the reader selected. Its LABEL names that bound
       (boundColLabel) and re-renders when the selection changes, so no header can imply a bound it did not
       use - which is exactly what the two columns this replaces did ("Sustained RPS (20 ms upstream)", with
       a tooltip asserting a p99 < 1 s bar the engine never enforced, over a metric read at 20 ms).
       Switching the bound re-reads this column and re-sorts the table in front of the reader. */
    /* WHAT A REQUEST COST, beside what it delivered. `costOnly` keeps these off a board that cannot
       answer them (see hasCost); they light up on the first run carrying the capture.
       WHY THIS COLUMN EXISTS AT ALL: the throughput column stops describing the GATEWAY the moment
       one saturates its pinned cores - past that it describes the box, and two gateways at the wall
       read the same however different they are. Cost per request has no such ceiling: at saturation
       both deliver the same rps by definition, and the one doing less work per request still reads
       lower. Ascending, because less CPU per request is better. */
    /* REQUESTS PER CPU-SECOND IS NOT A SECOND COLUMN HERE, because it is not a second number.
       1 CPU-second IS a million microseconds, so `rps_per_cpu_second` is exactly
       1,000,000 / cpu_us_per_request - the same measurement inverted. Printed side by side they read
       as corroboration and are nothing of the kind: 88.7 us/req and 11,273 req/CPU-s multiply to
       1,000,000 by construction, which is the same tautology that once made a cost "cross-check"
       pass while proving nothing. It lives on the Charts tab, where it answers a different question
       (how much traffic per unit of CPU you are buying) rather than restating this one. */
    { id: "cpu", label: () => { const c = costWindowConc();
        return c == null ? "CPU per request (\u00b5s)" : `CPU per request (\u00b5s @ c=${c})`; },
      desc: false, costOnly: true,
      title: () => `Microseconds of gateway CPU - user plus system, summed across its whole process tree - spent per completed request, ` +
        `measured at a fixed concurrency held identical for every gateway (published beside it as the cost window). ` +
        `Unlike peak throughput this does not stop separating gateways once they saturate their cores. ` +
        `A window with any failure publishes no cost: CPU divided by only the successes would describe the failures, not the work.`,
      get: (g) => chooserPerfCell(g, "cpu_us_per_request", fmt2) },
    { id: "rps", label: () => boundColLabel(selectedBound()), desc: true,
      title: () => `The most requests/sec the chosen cell carried ${boundClause(selectedBound())} and it failed no request it accepted, with the concurrency it was observed at. ` +
        `One of ${BOUND_CHOICES.length} readings of the SAME concurrency sweep published on every cell - switch the bound above to re-rank the board. ` +
        `A "≥" is a floor: the sweep ran out of ladder while that concurrency was still qualifying. Hover a cell for the tail it actually produced and the concurrency that stopped qualifying above it.`,
      get: (g, st = state) => frontierChooserCell(g, st) },
    /* THE SHAPE, beside the number. A row of six figures does not communicate a slope; this does, and the
       share of full rate makes it sortable and readable aloud. See frontierSpark on the shared y scale. */
    /* THE HEADER STATES THE QUANTITY IN WORDS. It read "Curve across bounds" over a column of bare "×1.3"s
       ("i dont know what 1.3x or whatever means"), then named a gain factor - and the owner still could not
       read it: "its just not clear what this means, even I know and i cant figure it out". A ratio needed a
       legend; a share of a rate the reader can see in the columns to the left does not. */
    { id: "shape", label: "Rate held at its tightest bound", desc: true,
      title: () => `The whole frontier as one line: throughput at ${BOUND_CHOICES.map(boundLabel).join(", ")}, left to right, on a scale shared by every row. ` +
        `Flat means the gateway holds its rate even under a tight tail; a line climbing from the floor means it needs a loose tail to go fast. ` +
        `Log scale, so equal slopes are equal RATIOS - which is what the shape means - and the slowest gateway on the board is still visible. ` +
        `The dotted rule marks the bound the ranked column is reading; an open dot marks a reading that is a floor rather than a ceiling; a tick on the baseline means the gateway served but NO concurrency held that tail (a measured nothing, not a missing measurement). ` +
        `"N% OF ITS FULL RATE AT B" is what the cell still carried at B, the tightest published bound it holds any rate at, as a share of its rate with no latency bound at all. Sorting groups the column by that bound first, because 99% at 1 ms and 99% at 50 ms are opposite findings.`,
      get: (g, st = state) => frontierShapeCell(g, st),
      render: (g, st = state) => frontierShapeTd(g, st) },
  ],
  /* FRONTIER: THE WHOLE CURVE, ONE ROW PER GATEWAY, ALL SIX READINGS SIDE BY SIDE.
     Its own tab rather than more columns on Performance, and rather than a longer page: the owner's rule is
     that less scrolling wins and more tabs are the acceptable cost. Performance answers "who is fastest at
     MY bound"; this answers "what shape is each gateway", which is the finding the two retired scalars
     averaged away. Fourteen rows by eight columns fits one screen, so a reader compares shapes without
     scrolling and without opening anything.
     Every column is a real published reading - none is derived - and the selected bound's column is marked
     so the ranked column on Performance is locatable here. */
  frontier: [
    COL_SEL, COL_NAME, COL_TESTED,
    ...BOUND_CHOICES.map((b) => ({
      id: boundColId(b), label: boundLabel(b), group: BOUND_GROUP_LABEL, desc: true,
      /* THE SIX HEADERS SHARE ONE SPANNING GROUP AND KEEP ONLY THEIR OWN BOUND.
         They each read "Req/s · 99% under N ms" - the same five words six times across the widest table on
         the board, in the owner's words "make Req/s 99% a header that spans all columns vs repeating?". The
         group header (BOUND_GROUP_LABEL) says the shared part once and each sub-header says only what differs.
         The full sentence has NOT been dropped: it is still on every cell's own tooltip below, which is where
         boundClause() guarantees the wording, so the reason boundColLabel exists (a header may never imply a
         bound it did not use) is served by the group + sub-header pair rather than by repetition. */
      // The observed tail rides UNDER the number (frontierBoundCell) because "4 ms under a 100 ms bound" and
      // "99 ms under it" are different findings and a column of rates alone cannot tell them apart.
      title: `The most requests/sec the chosen cell carried ${boundClause(b)} and it failed no request it accepted. ` +
        `Under each number is the tail it ACTUALLY produced there, which is never the bound. "≥" marks a floor.`,
      get: (g, st = state) => frontierBoundCell(g, b, st),
      render: (g, st = state) => {
        const c = frontierBoundCell(g, b, st);
        const sel = selectedBound(st) === b || (b == null && selectedBound(st) == null) ? " bound-col" : "";
        if (c.na) return `<td class="na${sel}" title="${esc(c.note || "")}">${esc(c.text)}</td>`;
        const rd = c.reading;
        /* THE SUB-LINE UNDER THE NUMBER carries whichever of the two things this cell has to say. For a real
           reading it is the tail the gateway ACTUALLY produced (4 ms under a 100 ms bound is not the same
           finding as 99 ms). For a measured nothing it is WHY - "no rung held this tail" - because five
           bare cells in a row would read as five missing measurements on exactly the gateways where the
           measurement is the most damning thing on the board. */
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
    // THE `cpufps` COLUMN IS GONE with the `cpu_fps` metric the producer retired: it counted frames/sec
    // under an unpaced firehose WITHOUT the delivery gate, so a gateway dropping frames could post a higher
    // relay rate than one delivering all of them - a loss rate with a numerator, not a throughput. The
    // frame rate a reader can act on survives as `streams_sustained_fps`, measured at a concurrency where
    // every frame arrived, and it is in the drawer's streaming lane.
    { id: "streamfps", label: "Streams sustained (frames/s)", desc: true, title: "The frame rate the sustained-streams ceiling held, on the chosen cell: the throughput behind the stream count, measured where every expected frame was delivered. Higher is better.",
      get: (g) => chooserStreamCell(g, "streams_sustained_fps", fmtInt) },
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
    /* THE HEADER NAMES THE SCOPE OF ITS MEDIAN. With per-cell data this column is the median ACROSS the
       gateway's cold samples, one per served cell (idle is sampled before any request, so no cell is involved
       in it); the curve on the same row belongs to the SELECTED CELL. The two differ on six of the live
       board's eleven measured rows - apisix 177.9 against 178.1, bifrost 244.3 against 222.6 - and with both
       labelled only "median" a reader comparing them concludes one is wrong. The tooltip always said this;
       the ROW did not, and a disclosure you must hover to find does not stop a reader trusting the wrong
       number. The legacy single-window shape has one cell, makes no such claim, and keeps the plain label. */
    { id: "memidle", label: () => (hasPerCellMemory() ? "Idle RSS (MiB, all cells)" : "Idle RSS (MiB)"), desc: false,
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
      title: "How fast RSS was still rising over the final window on the chosen cell. Around zero once the gateway has settled. If no steady state was reached, this rate under this load IS the reading, and no steady-state number exists to report instead. Lower is better.",
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
      /* ONE COMPACT LIFECYCLE CURVE, INSIDE A REAL FOCUSABLE CONTROL.
         The cell used to stack six block elements (label, sparkline, caption, label, sparkline, caption) and
         the row stood ~350px tall. It is now one ~34px curve, and everything the captions carried lives on
         this control's accessible name (memCurveSummary) plus the drawer.
         IT IS A <button> AND NOT A DIV BECAUSE OF THE REACHABILITY RULE: content that exists only in a
         `title` is invisible to touch and to keyboard users, which is deletion for some readers. A button is
         focusable, announces its aria-label, and its Enter/Space activation bubbles to the row handler that
         opens the drawer - so the full detail is reachable without a pointer. */
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
/* txt(x): a column/metric label or title, which may be a plain string OR a function rendering it from
   the live data (used where the wording depends on a tunable harness setting — audit #14). */
function txt(x) { return typeof x === "function" ? String(x() ?? "") : String(x ?? ""); }
/* The set of columns for a view; perf tabs use COLUMN_SETS, everything else has no table. A column marked
   perCellOnly exists only where the data can fill it: the published board still carries bundles measured
   before per-cell memory, and a growth column that reads n/a on all thirteen rows would be noise. */
function columnsFor(view, data = (typeof state !== "undefined" ? state.data : null)) {
  let cols = COLUMN_SETS[view] || COLUMN_SETS.performance;
  if (!hasPerCellMemory(data)) cols = cols.filter((c) => !c.perCellOnly);
  if (!hasCost(data)) cols = cols.filter((c) => !c.costOnly);
  return cols;
}
/* rowComparator(col, desc): the roster's row order for one column and one direction.
   THE NAME TIEBREAK IS NOT PART OF WHAT THE READER ASKED TO REVERSE. Toggling a column to descending
   reverses the RANKING; it does not mean "and also reverse the alphabet". The direction used to be
   applied to the whole comparison, so every group of equal-valued rows flipped its name order too - and
   on a column with dense ties (two gateways both below resolution, a lane where several rows read the
   same round number) the table visibly reshuffled rows whose values had not changed at all. Direction
   decides the value comparison; the name tiebreak is always ascending, so a tie sits still.
   Missing values always sink to the bottom, in both directions: an absent reading is not a low score. */
/* TIEBREAK: the second-best measurement, not the alphabet.
   Three gateways sustained a MEASURED ZERO (no load held the gate), which is a real result and a real
   three-way tie. Falling straight to display order put them in alphabetical order, which reads as a
   ranking it is not - a reader scanning the bottom of a sorted column sees One-API above Plano above
   TensorZero and infers something the data never said. When the sorted column cannot separate rows,
   the next-most-relevant MEASUREMENT should, and the alphabet is only the last resort once the numbers
   genuinely run out. Lower is better for every tiebreak column named here (they are all latencies), so
   the tiebreak always sorts ascending regardless of the primary column's direction: it is not part of
   the sort the reader asked for, it is what to do when that sort has nothing left to say. */
const VIEW_TIEBREAK = {
  performance: "lat50",
  // The frontier tab has no latency column to fall back on, and its own columns are all rates (higher is
  // better), so the ascending-tiebreak rule above does not fit any of them. Ties there fall to the name,
  // which is honest: two gateways with the identical reading at the sorted bound genuinely tie on it, and
  // the curve column beside them is where the difference shows.
  streaming: "sttft50",
  memory: "memidle",
};
function rowComparator(col, desc, tiebreak) {
  return (a, b) => {
    const va = col.get(a).v, vb = col.get(b).v;
    const byName = a.display.localeCompare(b.display);
    // The tie-breaking measurement, used only when the primary column ties. Never the sorted column
    // itself: comparing a column with itself always returns 0 and would just re-add the alphabet.
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
      /* EVERY READING, ONE ROW EACH, in published order - replacing `rps_max_proxy` and
         `rps_sustained_20ms`, which were the same sweep collapsed twice and which contradicted each other
         in the field. All six are listed rather than only the selected one BECAUSE THE DRAWER AND THE
         COMPARE PANEL ARE WHERE A READER GOES FOR THE EVIDENCE: the shape across bounds is the finding, and
         in compare it puts three gateways' whole curves side by side as numbers, which is the one place
         digits beat the sparkline. `cell` (not `k`+`fmt`) because a reading is not a bare envelope on the
         record - it is an envelope plus its own evidence, and frontierCell is what renders that pair. */
      ...BOUND_CHOICES.map((b) => ({
        // A stable key per reading, mirroring the engine's absence keys (frontier.10ms.rps): the tests and
        // any future per-metric lookup need to name a row, and its bound is the only thing that identifies it.
        k: `frontier.${b == null ? "unbounded" : `${b}ms`}`,
        label: boundColLabel(b), best: "max", cell: (rec) => frontierCell(rec, b),
      })),
    ],
    // The curve itself, under the numbers: the same sparkline the table shows, at the same shared scale, so
    // the drawer's six rows arrive with the shape they describe rather than leaving the reader to plot it.
    extra: (j) => frontierBlock(j),
    // And in the compare panel, one row of curves across the gateways being compared - three shapes beside
    // each other is the comparison the two retired scalars made impossible.
    cmpExtra: (j) => frontierBlock(j, { compact: true }),
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
      // DELETED: `cpu_fps` ("CPU-bound fps (peak)"). The producer retired the metric because it counted
      // relay frames/sec WITHOUT the delivery gate, so a gateway dropping frames could out-score one
      // delivering all of them - a loss rate with a numerator. The frame rate a reader can act on is the
      // row above, measured at a concurrency where every expected frame arrived.
    ],
  },
  {
    key: "xlate", label: "Translation", flag: "xlate_served", err: "xlate_error",
    get: canonicalXlate,
    pathNote: (j) => j && j.source ? caption(j) : "",
    metrics: [
      { k: "added_latency_p50_us", label: "Added latency p50 (µs)", best: "min", fmt: fmtInt },
      { k: "added_latency_p99_us", label: "Added latency p99 (µs)", best: "min", fmt: fmtInt },
      /* ONE READING HERE, at the bound the reader selected, rather than the perf lane's six. The translation
         cell carries its own frontier off its own sweep, and the question this lane answers is "what does
         translating cost", which is only answerable by comparing against the passthrough lane AT THE SAME
         BOUND - so the row is labelled with that bound and moves with the selector, and the whole curve is
         drawn underneath (extra) for anyone who wants the shape. */
      { k: "frontier.selected", label: () => boundColLabel(selectedBound()), best: "max",
        cell: (rec) => frontierCell(rec, selectedBound()) },
    ],
    extra: (j) => frontierBlock(j),
    cmpExtra: (j) => frontierBlock(j, { compact: true }),
  },
];
/* frontierBlock(rec, opts): the curve as a block, for the drawer and the compare panel. The sparkline plus
   the share of full rate in words - the two forms of the same fact, because the picture is what makes the shape
   legible at a glance and the sentence is what a reader can quote, sort by, or hear read aloud.
   Returns "" for a record with no frontier, so a legacy row simply has no curve rather than an empty frame
   captioned as if a measurement were behind it. */
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
   the drawer + compare so the plotted curve reads the SAME cell the table + headline do.
   ONE CURVE, NOT TWO. It used to draw the sustained and max-proxy sweeps as separate series, which was
   always a fiction: they were ONE sweep read twice, so the two curves were the same points with two
   different markers on them, and the pair could contradict each other (a "maximum" below the sustained
   figure). The cell now publishes that one sweep once (`rec.sweep`, every rung with its own concurrency,
   rate, tail and failure count) and the marker is the reading AT THE SELECTED BOUND - so the dot a reader
   sees on the curve is the number the ranked column shows, at the concurrency it was observed at.
   Returns [] when the chosen cell is absent or carries no rungs. */
function perfSweepSeries(g, colors, st = state) {
  const p = chooserCellPerf(g, st);
  if (!p || !Array.isArray(p.sweep) || !p.sweep.length) return [];
  const bound = selectedBound(st);
  const rd = frontierAt(frontierOf(p), bound);
  // C5: the displayable number comes through the ONE accessor (mval), never a bare `.value` deref. An
  // absent reading marks nothing rather than marking a rung the reading does not claim.
  const v = rd ? mval(rd.rps) : null;
  return [{
    label: colors.sweepLabel || `req/s across the sweep · marked at ${boundLabel(bound)}`,
    color: colors.sustained || colors.max,
    sweep: p.sweep,
    peak: v != null && rd.concurrency != null ? { rps: v, conc: rd.concurrency } : null,
  }];
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
    sortCol: "rps",
    sortDesc: true,
    /* THE TAIL-LATENCY BOUND THE BOARD IS SHOWING. `null` is a real choice (the unbounded reading), so the
       absent state is DEFAULT_BOUND_MS and never null.
       IT IS A VIEW, NOT A VERDICT. The constant it replaces, the engine's `SUSTAINED_P99_CEILING_US`,
       decided which measurements existed and no surface said so; this decides which column opens first,
       every bound is published on every cell, and switching it re-ranks the board in front of the reader. */
    bound: DEFAULT_BOUND_MS,
    // The mode each chooser family was last left on, so crossing tabs restores the reader's own
    // choice rather than the coercion the other family forced. Never encoded into the URL: a link
    // carries ONE mode, for the view it names.
    modeMemo: { perf: "same", memory: "min" },
    needStream: false,
    needXlate: false,
    // CELL CHOOSER (Performance + Streaming): which cell(s) of the ONE 6x6 run to show.
    //   mode "peak"   → each gateway's own best diagonal (best_cell); no dialect params.
    //   mode "same"   → sameDialect's diagonal (X→X) for every gateway.
    //   mode "custom" → xlateIn→xlateOut cell (any pair, incl. translation) for every gateway.
    //   mode "min"/"max" → MEMORY ONLY: this gateway's lowest / highest steady-state cell.
    mode: "same",
    sameDialect: DEFAULT_SAME_DIALECT,
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
  /* THE BOUND IS IN THE URL, so a shared link reproduces the reading it was shared at. Encoded only on the
     tabs whose numbers ARE read at a bound (BOUND_VIEWS): a ?bound= on the memory tab would claim the memory
     figures had one. `none` is the unbounded reading, spelled out rather than encoded as an empty value, so
     the link says which of the six readings it means. The default is omitted, keeping the pristine URL clean. */
  if (BOUND_VIEWS.has(st.view) && selectedBound(st) !== DEFAULT_BOUND_MS)
    p.set("bound", selectedBound(st) == null ? "none" : String(selectedBound(st)));
  // Each perf tab's clean URL omits the sort when it equals that tab's default column + direction.
  // On the frontier tab the default column FOLLOWS the selected bound (the tab ranks at the bound the
  // reader chose), so the link stays clean when the sort is simply that - and carries ?sort= when the
  // reader ranked by something else, e.g. the curve.
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
      // Any view whose DEFAULT mode is Same seeds its dialect from the run (widestDialect), so a
      // pristine URL for that view stays clean. This was memory-only when memory was the only such
      // view; the perf lanes now default to Same too, and a default that encoded ?d= would mean the
      // "default state round-trips to a bare /gateways" contract no longer held.
      const isDefault = defaultMode(st.view) === "same" && st.sameDialect === seededSameDialect(st.data);
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
  /* THE BOUND, before the sort: a `?sort=f50` link and a `?bound=50` link are the same intent expressed
     two ways, and the sort default below is derived from the bound on the frontier tab.
     A bound the board does not publish is IGNORED rather than honoured - an old or hand-edited ?bound=20
     (the retired gate's ceiling, and the one value a reader is most likely to try) must not render a column
     labelled with a bound no reading was taken at. It falls back to the default, which is named on screen. */
  const rawBound = p.get("bound");
  if (rawBound === "none") st.bound = null;
  else if (rawBound != null && FRONTIER_BOUNDS_MS.includes(Number(rawBound))) st.bound = Number(rawBound);
  // Accept any real, sortable column id from any tab; renderTable snaps it back to the tab's
  // default if it does not belong to the resolved view. A retired throughput sort id (?sort=rps20 /
  // ?sort=rpsmax, in every Performance link ever shared) maps onto the column that carries that ranking now.
  const rawSort = SORT_ALIASES[p.get("sort")] || p.get("sort");
  if (rawSort && ALL_COLUMN_IDS.has(rawSort) && rawSort !== "sel") {
    st.sortCol = rawSort;
    st.sortDesc = p.get("dir") !== "asc";
  } else {
    // No sort param: default to this view's headline column AND its natural direction. Leaving
    // sortDesc at the global default would sort added-latency defaults (sttft) descending, i.e.
    // worst-first. Derive the direction from the column's own `desc` flag.
    st.sortCol = st.view === "frontier" ? boundColId(st.bound) : (VIEW_SORT[st.view] || "rps");
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
  // NO ?mode= MEANS THE VIEW'S OWN DEFAULT, stated rather than inherited. This carried whatever the
  // fresh state happened to hold, which was harmless only while that value (`peak`) was a mode memory
  // does not offer, so memory fell through to Min. The perf default is now `same`, which memory DOES
  // offer, so a bare /gateways/memory started decoding as Same - memory silently losing the Min
  // default it declares because another view changed its own. A view's default is the view's to state.
  if (CHOOSER_MODES.has(mode) || MEM_CHOOSER_MODES.has(mode)) st.mode = mode;
  else st.mode = defaultMode(st.view);
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

const SITE_TITLE = "On the Bench · AI tool benchmarks";
/* pageTitle(st): the document title for a view. Pure, so it is testable without a document.
   THE VIEW LEADS. It used to compose "${category} ${view}", which put the constant first and produced
   "Gateways Frontier" - two nouns with no separator, and every tab in a browser strip truncating to the word
   they all share. The view is the only part that differs between two open tabs or two shared links, so it
   goes first, separated, and the category and site names follow as context. */
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
  // BELOW 1 THE ROUNDING ATE THE WHOLE AXIS. niceStep happily produces a sub-1 step for a sub-1
  // domain, so a sweep of ~0.25 req/s rungs draws gridlines at 0, 0.1 and 0.2 - and Math.round
  // labelled all three "0", a chart whose entire vertical scale reads zero for a gateway that was
  // measurably serving. Two significant figures separates 0.25 from 0.04 without inventing digits.
  if (v > 0 && v < 1) return String(+v.toPrecision(2));
  return String(Math.round(v));
}

/* hidpi(canvas, ctx): draw at the display's real resolution, present at CSS size.

   A canvas whose backing store matches its CSS size is UPSCALED by the browser on any display with a
   device pixel ratio above 1, so every glyph is interpolated and the text reads soft. Sizing the
   backing store to css x dpr and scaling the context once puts one canvas pixel on one device pixel.

   RETURNS THE CSS DIMENSIONS, and callers must use those for all geometry: after `ctx.scale(dpr,dpr)`
   the drawing coordinate system is CSS pixels, so reading `canvas.width` back would be off by the
   ratio. The CSS size is stashed on the element because hit-testing runs later, from a different
   function, long after the backing store was resized - that is exactly where this goes wrong quietly,
   giving a chart that looks right and whose hover lands in the wrong place.

   Idempotent: re-entered on every redraw, and a canvas already scaled must not be scaled again. */
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
  // SPACED IN PIXELS, NOT IN INDEX. Taking every Nth distinct concurrency says nothing about where
  // they land: on a log axis 12 and 14 are ~4px apart, so both labels drew on top of each other and
  // the axis read "1214". A tick whose label cannot be placed clear of the last one drawn is dropped
  // - the gridline is what locates the point, and an unreadable number is worse than one less tick.
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
  /* published-peak markers: a distinct labeled dot at each series' peak (its headline value at its
     operating concurrency). It sits ON the curve because the headline is max() over these points. */
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
    // TWO PEAKS AT THE SAME PLACE ARE THE NORMAL CASE, not the edge case: sustained and max-proxy
    // usually peak within a rung of each other, so both labels drew on the same line and neither was
    // readable. A label that would overlap one already placed drops below the dot instead.
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
    // THROUGH THE CSS SIZE, never the backing store: hidpi() enlarged the latter by the device pixel
    // ratio, so `canvas.width / r.width` would scale every hit by that ratio and put the readout on
    // the wrong point on a retina display.
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
    // fmtY, NOT fmtInt: this readout shares its series with the peak marker seven lines below, and
    // that marker was already fixed to fmtRate. Hovering the same point printed "0 rps" while the
    // label beside it printed "0.25" - one chart, two claims about one number. The p99 chart passes
    // fmtInt because a sub-1 MICROSECOND tail is not a thing; the rate chart passes fmtRate.
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
  // Mark the PUBLISHED peak on the RPS curve: a labeled dot at (peak.conc, peak.rps). By construction
  // that point is one of the probed sweep points (the headline is max() over this same array), so the
  // marker lands ON the curve and names the operating concurrency.
  const rps = usable.map((s) => ({ label: s.label, color: s.color,
    points: s.sweep.map((p) => ({ x: p.conc, y: p.rps })),
    mark: s.peak && s.peak.rps > 0 && s.peak.conc != null
      ? { x: s.peak.conc, y: s.peak.rps, label: `${fmtRate(s.peak.rps)} @ c=${fmtInt(s.peak.conc)}` } : null }));
  const p99 = usable.map((s) => ({ label: s.label, color: s.color, points: s.sweep.map((p) => ({ x: p.conc, y: p.p99_us })) }));
  // SAME x-axis: both charts share ONE concurrency domain (min..max across BOTH series) so they stack
  // and align vertically. Compute it from every probed concurrency on either chart.
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
    // THE DATA LABELS ARE NOT AXIS CHROME. `--fg-dim` is the muted grey for gridlines and tick marks;
    // using it for the gateway names and their values renders the actual content at chrome contrast,
    // which reads as blurry even when the pixels are sharp. Those get `--fg`, the body text colour.
    ink: cs.getPropertyValue("--fg").trim() || "#e6edf3",
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
  // The frontier tab shows no latency column (its readings ARE the latency-bounded throughput), so its lead
  // must not promise one - the cell chooser it describes is the same one, though.
  if (view === "frontier") return "Per-cell throughput from the one 6x6 run; the cell chooser picks which cell every row reads.";
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
/* A TAB'S PROSE, SPLIT BY WHERE IT BELONGS ON THE PAGE: `{ lead, notes }`.
   THE OWNER'S INSTRUCTION, VERBATIM: "1-2 sentence english, definitions go below data table like
   references." The Frontier tab was six paragraphs deep before the first number - a reader who came for the
   board had to read an essay to reach it, and a reader who wanted a definition had already skipped the
   essay. So `lead` is one or two plain sentences saying WHAT THIS TAB SHOWS and nothing else, rendered
   above the table; `notes` is everything else - how to read it, what a marker means, what a measured 0 is -
   rendered BELOW the table as reference material, beside the engine's own definitions.
   NOTHING IS DELETED, ONLY MOVED. The notes carry findings, not decoration: the "0 · no rung held this
   tail" distinction is the difference between a damning measurement and a shrug, and dropping it to shorten
   the page would flatter exactly the slowest gateways on the board. Footnote position is right for it -
   the reader who wants it goes looking, and the reader who wants numbers hits them immediately.
   `captionText(c)` flattens the two halves back into one string, for a test that asserts a claim is made
   SOMEWHERE on the tab without pinning which half says it. */
function captionText(c) { return [...c.lead, ...c.notes].join(" "); }
function chooserCaption(view, st, data) {
  const lead = chooserLead(view, data);
  // HELD_REFERENCE belongs to whichever tab actually renders the shape column. Performance does; Streaming
  // has no frontier column at all, and explaining a number that is not on the page is noise.
  const extra = view === "performance" ? [HELD_REFERENCE] : [];
  if (st.mode === "peak")
    return { lead: [lead,
      // NOT "best" and NOT "peak": bestCell prefers the openai diagonal and otherwise ranks on added
      // latency, so this is a representative cell, not a maximum. See CHOOSER_MODES.
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
// AUDIT #14: the window durations render from the data (idle_window_s / recovery_window_s), never
// hard-coded — the harness makes them tunable and the caption must describe the run that happened.
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
  // States the count and where to read the rate. It used to say "never settled ... (flagged by name)":
  // a verdict, and a pointer to a name-column pill that no longer exists.
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
      /* THE WINDOW LENGTHS ARE BOARD FACTS, STATED ONCE HERE. Every row's curve used to print "(360 s)" and
         "over 59 s at rest" - one property of the harness, identical on all fourteen rows, repeated fourteen
         times inside the cell whose height was the complaint. The RSS curve column is the same lifecycle for
         every gateway; only the shape differs, so only the shape is per-row. */
      `Every RSS curve is one process's whole lifetime, left to right: ${memWindowLabel(w.idle)} at rest before the first request, then the load run to steady state, then ${R} of recovery after it stops. Those windows are the same for every gateway. The break in the middle of each curve is the time axis changing scale between them - the at-rest window is far shorter than the load run, and is drawn wider than its duration earns so its shape stays legible. Hover a curve for its figures, or click the row for the two windows full size and separated.`,
      `Idle is sampled cold, before the first request, so no cell is involved and it is valid in every mode - which is why the Idle column is the median across ALL of a gateway's cells while its curve is the chosen cell's own window; the two can differ. Growth is around zero once a gateway has settled, and is the rate RSS was still moving at when no steady state was reached. Recovered @${R}: RSS ${R} after the load stops, which on a gateway still releasing is not the last figure its curve reaches.${never}`,
      "Lower is better on every column. A gateway that does not serve the chosen cell reads n/a and sinks to the bottom; nothing is substituted from another cell.",
    ] };
}
/* frontierCaption(st, data): the Frontier tab's prose. It states the finding the tab exists for - that two
   gateways with similar headline rates can be completely different machines - and names the evidence on the
   row, because a table of six rates with no explanation of what varies across them is the same "six numbers"
   that fails the reader.
   IT USED TO BE SIX PARAGRAPHS ABOVE THE FIRST NUMBER. Every one of them was load-bearing and every one of
   them was in the wrong place: the reader who came to compare fourteen gateways had to scroll an essay to
   reach the table. Two sentences stay on top; the rest is reference material below it (see captionText). */
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
/* captionFor(view, st, data): the tab's `{ lead, notes }`, from whichever caption function owns the view.
   ONE dispatch point, so the renderer and any test read the same split. */
function captionFor(view, st = state, data = state.data) {
  return view === "memory" ? memoryCaption(data, st)
    : view === "frontier" ? frontierCaption(st, data)
    : chooserCaption(view, st, data);
}
/* updateTableCaption(view): the lead ABOVE the table, everything else BELOW it.
   See the `{ lead, notes }` note on captionText for why. The notes render as a collapsed fold beside the
   engine's definitions rather than as loose paragraphs: below the table they cost nothing until asked for,
   which is what lets them stay complete instead of being trimmed for length. */
function updateTableCaption(view) {
  const el = document.getElementById("table-caption");
  if (!el) return;
  const c = captionFor(view, state, state.data);
  el.innerHTML = c.lead.map((l) => esc(l)).join("<br>");
  const defs = document.getElementById("table-defs");
  // The engine's own definitions for the metrics THIS tab shows. An unknown view contributes no prefixes and
  // renders nothing rather than everything.
  if (defs) defs.innerHTML = notesFold(c.notes) + definitionsFold(DEFINITION_PREFIXES[view] || [], state.data);
}
/* notesFold(notes): the "How to read this table" reference block. Collapsed, one line of cost until opened.
   Returns "" for a view with nothing to say rather than an empty fold with a summary and no body. */
function notesFold(notes) {
  const lines = (notes || []).filter((n) => typeof n === "string" && n.trim());
  if (!lines.length) return "";
  return `<details class="metric-defs table-notes"><summary>How to read this table</summary>` +
    lines.map((l) => `<p>${esc(l)}</p>`).join("") + `</details>`;
}
/* THE MEMORY TAB HAS NO CHART BLOCK, and that is the shape of the whole board now: tables are tables,
   and every chart lives on the Charts tab.

   It used to append two static PNGs under the memory table. They could not follow the cell selector
   sitting directly above them - a reader switching from Min to Max cell watched the table change and
   the pictures stay put, which is worse than having no pictures: it implies the images describe the
   selection when they describe whatever was rendered hours earlier. One place for charts, all of them
   live, is both simpler to read and impossible to get out of sync. */

/* ---- DECLARED COLUMN GEOMETRY -------------------------------------------------
   THE OWNER: "changing filters shouldn't change column widths, just an annoyance." Measured across filter
   combos on the live board, every table view drifted at every width. The frontier tab at 1440, first body
   row, per column:
       mode=peak   bound=10   36 165  90 118 118 118 118 118  93 165
       mode=same   bound=10   36 165  84 119 119 119 119 119  93 167
       mode=custom bound=10   36 165 144 125 125 125  80  80  87 173
   Nothing about the measurement changed - only which cell each row reads - and the whole grid re-solved.

   THE CAUSE IS AUTO TABLE LAYOUT: a column is as wide as its widest RENDERED cell, so a filter that swaps
   `20,119` for `20,389`, or a passthrough pill for `OpenAI→Bedrock Converse`, or a number for `no rung held
   this tail`, re-measures every column. And the widest thing a column CAN hold is usually not in the combo
   on screen - `Added latency p99` holds 2,083,807 for one-api and 232,065 for plano - so sizing from what is
   rendered cannot be stable even in principle.

   SO THE WIDTH IS A PROPERTY OF THE COLUMN, declared here, and `table-layout: fixed` (style.css) makes the
   browser use it instead of consulting a cell. Excess width is then distributed proportionally over the
   declared widths, which is also content-independent, so the table still fills its container and no filter
   can move a column sideways.

   IT IS ONE TABLE, NOT ONE PER COLUMN DEFINITION, deliberately: geometry is a property of the table as a
   whole (the frontier's ten columns have to add up to something that fits a narrow desktop), and spreading
   the numbers across forty column definitions would make that sum impossible to see or to check.
   THE TWO OVER-WIDE COLUMNS ARE THE ONES THAT GIVE, which is the owner's other complaint - "column widths -
   Tested on is huge (cos of 1 large openai > cohere)" - seen from the other side: `tested` is an annotation
   and is narrower than any column of readings, and the curve columns are sized to their own SVG. The numbers
   are never what shrinks. */
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
/* theadHtml(cols, st): the table head, ONE row normally and TWO when any column declares a `group`.
   WHY A SPANNING HEADER AT ALL: the Frontier tab's six reading columns each read "Req/s · 99% under N ms" -
   thirty of the same words in one header strip, which is the owner's "make Req/s 99% a header that spans all
   columns vs repeating?". The shared clause is stated once and each sub-header carries only its own bound.
   A column with NO group spans both rows (rowspan=2) so it stays vertically aligned with the grouped pair;
   consecutive columns sharing the same group string collapse into one colspan cell. The group cell is
   deliberately NOT sortable and carries no data-col: the sort affordance, the `sorted` class and the
   direction arrow all stay on the column's OWN header in the second row, so switching the bound still marks
   and re-ranks the column the reader clicked. */
function theadHtml(cols, st = state) {
  const th = (c) => {
    const sorted = st.sortCol === c.id;
    const dir = sorted ? `<span class="dir">${st.sortDesc ? " ▾" : " ▴"}</span>` : "";
    // AUDIT #14: label/title may be a FUNCTION so a column whose wording depends on a tunable harness
    // window (the memory windows) renders from the data instead of hard-coding the default.
    return `<th data-col="${c.id}" class="${sorted ? "sorted" : ""}${c.sortable === false ? " nosort" : ""}" ` +
      `${c.group ? "" : `rowspan="2" `}title="${esc(txt(c.title))}">${esc(txt(c.label))}${dir}</th>`;
  };
  if (!cols.some((c) => c.group)) {
    // No groups on this tab: one row, and no rowspan (a rowspan of 2 over a one-row head would reserve a
    // phantom second row and push every body row down by one header's height).
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
  /* THE DECLARED GEOMETRY, re-stated whenever the column set changes. A <colgroup> has to be a child of the
     <table> and the first one at that, so it cannot ride along in theadHtml's string; it is created once and
     refilled, rather than replaced, so nothing else on the table is disturbed. Without it `table-layout:
     fixed` would divide the width equally between the columns - the frontier's checkbox as wide as its
     gateway names - so the CSS rule and this element are one mechanism and neither works alone. */
  const cg = table.querySelector("colgroup") ||
    table.insertBefore(document.createElement("colgroup"), table.firstChild);
  cg.innerHTML = colgroupHtml(cols);

  let rows = applyFilters(data.gateways, state);
  const count = document.getElementById("row-count");
  if (count) count.textContent = `${rows.length} of ${data.gateways.length}`;

  const col = cols.find((c) => c.id === state.sortCol) || cols.find((c) => c.id === VIEW_SORT[view]) || cols[3];
  const tiebreak = cols.find((c) => c.id === VIEW_TIEBREAK[view]);
  rows = rows.slice().sort(rowComparator(col, state.sortDesc, tiebreak));

  /* WHICH ROWS THE SORTED COLUMN CANNOT ACTUALLY SEPARATE.
     The table ranks by one column, and a rank implies the order means something. When two adjacent
     values are closer than the rig can resolve (rigResolutionPct, derived from how far identical
     boxes drift on the same qualification), the order between them is which box they landed on -
     not a finding. Marking them is the difference between publishing a measurement and publishing
     a coin toss with a decimal point.
     Only the SORTED column is considered: a tie on some other column is not what the reader is
     being shown a ranking of. */
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
  // THE BOUND SELECTOR. One delegated listener, for the same reason the mode buttons have one: the buttons
  // are re-rendered per view, so per-button listeners would bind to elements that no longer exist.
  const bseg = document.getElementById("bound-seg");
  if (bseg) bseg.addEventListener("click", (ev) => {
    const btn = ev.target.closest(".seg-btn");
    if (!btn) return;
    selectBound(btn.dataset.bound === "none" ? null : Number(btn.dataset.bound));
  });
  const onSame = () => { state.sameDialect = same.value; renderTable(); syncUrl(true); };
  const onCustom = () => { state.xlateIn = cin.value; state.xlateOut = cout.value; renderTable(); syncUrl(true); };
  if (same) same.addEventListener("change", onSame);
  if (cin) cin.addEventListener("change", onCustom);
  if (cout) cout.addEventListener("change", onCustom);
}

/* selectBound(ms): the reader picked a tail-latency bound.
   RE-RANKING IN FRONT OF THE READER IS THE POINT, not a side effect: the whole claim of the frontier is
   that a gateway's position depends on the tail you are willing to accept, and a control that changed only
   the numbers in place would leave that claim unmade. The Performance tab's ranked column re-reads the new
   bound on its own; the frontier tab's ranking is per-bound COLUMN, so the sort follows the selection -
   unless the reader had deliberately sorted by something else (the curve, a name, a latency), in which case
   their choice is left alone. */
function selectBound(ms) {
  const prev = selectedBound(state);
  state.bound = ms;
  if (state.view === "frontier" && state.sortCol === boundColId(prev)) state.sortCol = boundColId(ms);
  // The STATE change above is the whole decision and is testable on its own; the three calls below are the
  // DOM half, skipped under node exactly as syncUrl skips its history write, so the suite can drive the
  // selector without a document. A guard here is the difference between "the re-rank is covered" and "the
  // one behaviour the bound selector exists for is covered by nothing".
  if (NODE) return;
  renderFilters(); renderTable(); syncUrl(true);
}
/* renderBoundChooser(): paint the bound buttons from the ONE published list, mark the selection, and state
   in words what the selected column means. The sentence is rendered from boundClause(), the same function
   every column header and tooltip uses, so the control and the columns cannot describe the bound differently.

   IT SAYS "THE CELL THE CHOOSER PICKED", NEVER "THE MOST EACH GATEWAY CARRIED". It used to read "showing the
   most req/s each gateway carried while 99% of requests finished under 10 ms", which asserts a maximum ACROSS
   a gateway's cells that nothing here computes: the reading is the top qualifying rung of ONE cell's sweep,
   and which cell that is comes from the chooser (bestCell, which ranks on latency and never reads a
   throughput number). kong's four diagonals span 3,903 → 22,891 req/s at the same bound, so on that row
   alone the old wording overstated by ~6x. Per-cell is the true scope and the only scope any bound can
   change. */
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
function renderFilters() {
  document.getElementById("search").value = state.q;
  renderBoundChooser();
  for (const [, name] of CAPS) { const el = document.getElementById(`f-${name}`); if (el) el.checked = state[CAPS.find(([, n]) => n === name)[0]]; }
  // Cell chooser: paint the buttons THIS view offers, mark the active one, and show only the dropdown(s)
  // that mode needs (Same → one dialect; Custom → in→out pair; Own cell/Min/Max → none). The `peak` mode is
  // simply not rendered on the memory tab: the control cannot offer a selection the metric is not allowed
  // to make.
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
/* matrixDiagonal(g): the same-dialect cell for each protocol - openai>openai, anthropic>anthropic -
   which is what the drawer's six-row Protocol matrix has always been showing. Returns null when the
   gateway carries no per-cell data at all, so "not measured" stays distinguishable from "measured and
   this pairing was not served".

   Reads `upstreams[egress].cells[ingress]`, with the legacy flat `cells` map as a fallback so older
   boards keep rendering. An EMPTY object counts as nothing found, not as a matrix full of holes:
   treating a truthy `{}` as data is exactly what put six "n/a" rows under every gateway. */
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

/* IDLE_AXIS_MIN_SPAN: the SMALLEST band the idle sparkline's y axis will ever cover, as a fraction of the
   published idle figure. It is the idle panel's equivalent of the load panel's "2x idle is a floor, not a
   ceiling" rule, and it exists for the same reason: an axis that shrinks to fit whatever the series did will
   draw sampling noise as an event.
   2% is calibrated against the field, not chosen for roundness. Below it sit every window that genuinely did
   not move - litellm-rust 0.008 MiB on 252 (0.003%), tensorzero 0.07 on 48.7 (0.14%), kong 0.61 on 379
   (0.16%), plano 0.5 on 625 (0.08%) - all of which now draw as the flat lines they are. Above it sit the two
   windows that did something: apisix steps down 6.6 MiB on 178 (3.7%) late in its window, and every bifrost
   cell ramps 65-102 MiB (up to 74%) through its first fifth and then goes flat. Those fill their frames, and
   the stamp states the MiB either way so no magnitude has to be inferred from the picture. */
const IDLE_AXIS_MIN_SPAN = 0.02;
/* idleShapeNote(pts, span, floorSpan): WHAT THE IDLE WINDOW DID, as a phrase describing its SHAPE.
   THE WORDING THIS REPLACES CAUSED A REAL MISREADING. The caption said "(6.59 MiB over 59 s at rest)", and
   "over 59 s" is the window LENGTH, not a duration over which anything moved - so it read as 6.59 MiB of
   drift accumulating across the minute, and the person auditing the board reported apisix as an idle drifter
   on the strength of it. apisix does no such thing: it holds 178.078 for 127 of its 130 samples, steps DOWN
   6.594 MiB at 98% through, and holds. One late release.
   A span cannot distinguish these and neither can a floored axis, so the words have to.
   THIS IS GEOMETRY, NOT A VERDICT. It says where the movement was, how big, and which direction - all read
   off the plotted series. It does not judge whether the gateway is leaking or healthy: the engine owns that
   (`memory_idle_static` / `idle_shape`) and idleStatic() renders the engine's own word whenever a bundle
   carries one. This board publishes none, which is exactly why the shape of the line is all a reader has.
   NEVER "X over N s": no phrase here may pair a magnitude with the window length, because that pairing is
   what reads as a rate. */
function idleShapeNote(pts, span, floorSpan) {
  const n = pts.length;
  // BELOW THE AXIS FLOOR IS THE EXPECTED CASE (nine of the board's eleven measured windows) and it renders
  // flat, so it says flat. RSS is sampled in whole pages, so a static process still reports a page or two of
  // jitter; "flat to within X" states the resolution rather than claiming an impossible zero. Sharing the
  // axis floor is deliberate: the words and the picture must agree about what counts as movement.
  if (!(span > 0)) return "no movement at all";
  if (span < floorSpan) return `flat to within ${fmt2(span)} MiB`;
  /* WHERE THE MOVEMENT SITS IN TIME IS THE WHOLE DISTINCTION, and it is not "how big is the biggest single
     step". Keying on one sample-to-sample delta got bifrost wrong: its climb is eight consecutive samples of
     ~10 MiB each, none of which dominates the span, so it was described as gradual when it in fact completes
     inside the first 2 s of a 59 s window and then holds flat.
     So: total variation, then the SHORTEST contiguous run of samples accounting for most of it. A step is
     movement packed into a small slice of the window whatever its sample count; drift is movement spread
     across the window. */
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
  // The EXTENT of the concentrated run (its own min to max), not the net across its endpoints: the run's
  // boundaries can land mid-climb, and bifrost's then read "71.2 MiB up" beside a stated span of 151.0-252.5.
  const runVals = pts.slice(a, b + 2).map((q) => q.rss_mib);
  const mag = fmt2(Math.max(...runVals) - Math.min(...runVals) || span);
  /* "THEN HELD" IS A CLAIM ABOUT THE REST OF THE WINDOW, made only when the rest of the window earns it.
     bifrost climbs to 252.5 in its first 2 s then falls back ~30 MiB to 222.6 before going flat, so "settled
     up, then held" would paper over a second movement larger than most gateways' entire span. */
  const restVar = d.reduce((acc, v, i) => (i < a || i > b ? acc + v : acc), 0);
  const held = restVar < floorSpan ? ", then held" : "";
  /* 20% OF THE WINDOW is the line between a step and drift. apisix packs its move into one sample at 98%
     through; bifrost into ~3% at the start. No gateway on the current board is a genuine drifter, which is
     precisely why the retired wording - which described every one of them as though it were - misled. */
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
/* recoveryTail(lastMib, opts): the END of the load caption, naming the WINDOW each figure belongs to.
   THE COLLISION THIS REMOVES. The caption said "peak 144.4 → recovered 129.6 MiB (365 s)" while the
   `Recovered @30 s` column on the same row said 139.1. Both are right: one-api kept releasing after the 30 s
   recovery mark, so the scalar the engine publishes AT that mark and the last sample of a 365 s observation
   are two readings of one falling curve. Two figures under the single word "recovered" reads as an
   inconsistency in the data rather than a difference of when it was read.
   So "recovered" belongs to the column that names its window, and this states BOTH points in time order when
   they differ - which turns the discrepancy into the finding it actually is, "it was still falling at 30 s" -
   and collapses to one figure when they agree, which is most rows. */
function recoveryTail(lastMib, opts = {}) {
  const at = opts.recoveredAt, w = opts.recoveryWindowS;
  const end = fmt1(lastMib);
  if (at == null || w == null) return `${end} MiB at the last sample`;
  const marked = fmt1(at);
  if (marked === end) return `${marked} MiB at the ${memWindowLabel(w)} recovery mark, and still there at the end`;
  return `${marked} MiB at the ${memWindowLabel(w)} recovery mark, ${at > lastMib ? "still falling" : "risen again"} to ${end} MiB by the last sample`;
}
/* releaseMark(g): DID IT GIVE THE MEMORY BACK - drawn, not asserted.
   "Don't let 'releases nothing' and 'releases most of it' render with identical emphasis if a cheap visual
   distinction is available." TensorZero peaks at 65.8 and ends at 65.8, releasing none of the ~19 MiB it
   gained; bifrost peaks at 870.0 and comes back to 580.3. Both rendered as a line, a dot and two grey
   numbers.
   NO NEW METRIC AND NO VERDICT: a tick between two levels ALREADY PLOTTED - the peak and the final sample -
   at the x of that final sample. Released nothing, nothing to draw. Released a lot, a tall mark, in
   proportion. Its title states the fall and what it is a fall out of, both from figures the row already
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
// `opts` carries the two facts the LOAD panel needs in order to stop colliding with the Recovered column:
// the scalar that column publishes, and the window it was read at (see recoveryTail).
function rssSparkline(series, loadEndS = null, idleMib = null, kind = "load", opts = {}) {
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
  // gateway on the board that reached no steady state, and publishes a 51 MiB/min growth rate instead
  // of one, was the one whose curve showed no growth at all. Hiding the finding the row exists to
  // report is not a scale, it is a lie with a caption.
  //
  // So twice idle is a FLOOR on the axis, not a ceiling: a gateway that stays near idle still gets a
  // stable, honest frame instead of magnifying its own noise, and a gateway that climbs gets a frame
  // big enough to show the climb. Nothing is ever clipped.
  const dataMin = Math.min(...ys), dataMax = Math.max(...ys);
  const anchored = typeof idleMib === "number" && idleMib > 0;
  const idleWin = kind === "idle";
  /* THE IDLE WINDOW GETS ITS OWN AXIS, SCALED TO ITS OWN RANGE WITH A FLOOR ON THE SPAN.
     The load axis above runs 0 -> 2x idle, and on an IDLE window that frame answers no question: every
     idle series is a nearly-horizontal line at the idle level, so all 26 of them rendered as the same flat
     line halfway up the panel whatever they did. That hid the one real finding in the window - every
     bifrost cell keeps allocating for ~12 s AFTER it is ready to serve (openai>openai: 152.3 -> 217.0 MiB,
     +43%, then dead flat for the remaining 48 s), and no other gateway on the board does it.
     BUT A BARE AUTO-SCALE WOULD BE WORSE, AND IT IS THE SAME BUG THE LOAD AXIS ALREADY FIXED. RSS is
     sampled in whole pages, so a genuinely static process still reports a jitter of one or two pages:
     litellm-rust moves 0.008 MiB - a single 8 KiB page - across 123 samples of a 252 MiB process.
     Auto-scaled to its own range, that one page becomes a full-height cliff, and the panel would claim a
     memory event where the truth is "this process did not move". FLAT IS THE EXPECTED CASE HERE (20 of 26
     windows) and it must render flat.
     So the span is FLOORED at IDLE_AXIS_MIN_SPAN of the published idle figure: below that, movement is
     drawn to scale inside a stable frame and reads as the flat line it is; above it, the frame grows to the
     data and nothing is ever clipped. The exact magnitude never has to be read off the picture anyway - the
     stamp states the span in MiB. */
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
  /* THE STAMP UNDER THE CURVE, AND WHY THE IDLE WINDOW NEEDED ITS OWN.
     Both panels used to render "peak X → recovered Y MiB (N s)". On the idle panel every word of that is
     wrong: the idle window is the process AT REST, sampled BEFORE any load, so nothing has been recovered
     from anything and its highest sample is not a peak under load. The idle stamp states what the window
     actually establishes - the published median, and the band the samples spanned - and the SPAN IS STATED
     IN MiB because that is the number the reader needs and the one a floored axis deliberately does not let
     them read off the picture: 0.008 MiB (litellm-rust, one page of jitter) and 64.7 MiB (bifrost, still
     allocating after it was ready to serve) are the same shape at a glance and nothing alike. */
  const span = dataMax - dataMin;
  // A range whose two ends round to the same figure is not a range - "spanned 252.3-252.3 MiB" reads as a
  // formatting fault where the fact is that the process never moved a tenth of a MiB. It says "held" instead,
  // and the exact movement still travels in the parenthesis (litellm-rust: 0.00781 MiB, one 8 KiB page).
  const flatToTenth = fmt1(dataMin) === fmt1(dataMax);
  /* "THIS CELL: MEDIAN X", NEVER A BARE "MEDIAN X". The Idle RSS column beside this is the median ACROSS the
     gateway's cells (idle is sampled cold before any request, so no cell is involved in it - which is what
     that column's own tooltip says); this is the SELECTED CELL's own window. Both are correct and they differ
     on six of the live board's eleven measured rows: apisix 177.9 against 178.1, bifrost 244.3 against 222.6,
     a 21.7 MiB gap. Two figures under one unqualified word "median" reads as one of them being wrong, so each
     names its scope - the column in its header, this in its caption.
     THE PARENTHETICAL DESCRIBES SHAPE, NOT A RATE (idleShapeNote): "(6.59 MiB over 59 s at rest)" paired a
     magnitude with the window length and so read as gradual drift, which is not what apisix did. */
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
    // The ZERO baseline. Load axis only: it starts at 0, so the bottom of the panel IS zero. The idle axis
    // is a window onto a narrow band a long way above zero, and a rule along its bottom edge would claim a
    // zero the frame does not contain.
    (idleWin ? "" : `<polyline points="${x(t0).toFixed(1)},${(H - PAD).toFixed(1)} ${x(t1).toFixed(1)},${(H - PAD).toFixed(1)}" ` +
      `fill="none" stroke="currentColor" stroke-opacity="0.15" stroke-width="1"/>`) +
    /* The idle level, drawn so "how far above idle" is a thing the eye can measure rather than infer.
       ON THE IDLE PANEL THIS RULE IS THE PUBLISHED NUMBER ITSELF: idle_rss_mib is the MEDIAN of this very
       window (correct in all 26 cells - bifrost's median lands on its settled plateau, apisix's on the value
       it holds), so drawing it here is what makes the sparkline and the scalar in the column beside it
       visibly agree instead of merely coexisting. */
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

/* rssCurves(mem): the memory window as TWO curves - what the process cost doing nothing, then what work
   cost it.
   Idle used to be a single number in a column, so a gateway that grew while completely idle looked
   identical to one that sat still, and the load curve's baseline was a value the reader had to take
   on trust.
   THE TWO PANELS DO NOT SHARE AN AXIS, AND THAT IS THE FIX, NOT A REGRESSION. They used to: both ran
   0 -> 2x idle, which reads well for the load curve (the question is "how far above idle did work push
   it") and answers nothing at all for the idle curve, where every series in the field is a nearly flat
   line at the idle level and all 26 of them therefore drew the same flat line halfway up the panel. The
   idle panel now frames its own window with a floored span (IDLE_AXIS_MIN_SPAN), so a static process still
   draws flat and bifrost's 12-second post-ready ramp is visible; both stamps state their own MiB, which is
   what keeps the panels comparable now that their axes are not.
   Returns just the load curve when there is no idle series, which is every bundle measured before
   the idle window existed. */
/* THE INLINE ROW IS ONE LIFECYCLE, ONE LINE. Fraction of the width given to the at-rest segment.
   WHY THERE IS A SPLIT AT ALL: idle and load+recovery are not two experiments, they are one process's
   lifetime in time order - cold start, at rest, load applied, load removed, recovery. The row rendered them
   as two stacked panels only because the engine hands over two arrays. But the two windows are wildly
   different lengths: ~59 s at rest against ~360 s under load. On a true shared time axis the at-rest window
   would occupy 14% of the width, and bifrost's entire finding lives in the first 2 SECONDS of it - under 1%
   of a 420 s axis, i.e. invisible.
   SO THE SEGMENTS ARE GIVEN WIDTHS THEIR DURATIONS DO NOT EARN, AND THE BREAK IS SHOWN. Silently rescaling
   two different time scales into one smooth line would draw a continuous axis that is not continuous - a
   picture asserting when things happened, wrongly. Instead there is a real gap with an axis-break glyph in
   it (the conventional double-slash), the two halves are drawn as two separate paths so no line crosses the
   discontinuity, and the hover text names both window lengths. An honest discontinuity beats a dishonest
   smooth line. */
const LIFECYCLE_IDLE_FRAC = 0.3;
/* rssLifecycle(mem, opts): the whole process lifetime as ONE inline sparkline, for the table row.
   ONE SHARED Y AXIS across both segments - that part must not be broken, because the entire point of putting
   them on one line is that the reader can see how far above its resting level work pushed the process. It is
   the load axis (0 → at least twice idle, never clipping), so the at-rest segment sits low and flat, which is
   the truth: idle is a small fraction of peak.
   THE TRADE THIS MAKES, STATED: on a shared axis a 6.59 MiB step at rest is ~2% of the frame and is not
   legible inline. That is what the hover text and the drawer are for - the drawer keeps the separated panels,
   where the at-rest window has its own floored axis (IDLE_AXIS_MIN_SPAN) and its movement is legible. Inline
   answers "what shape is this process's life"; the drawer answers "what did each window do". */
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
  // THE AXIS BREAK, drawn: two slashes in the gap, so the discontinuity is a thing the reader can see rather
  // than an assumption they have to avoid making.
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
/* memCurveSummary(mem): EVERY figure the row's captions used to show, as one sentence.
   THE REACHABILITY RULE: nothing may become unreachable when a caption folds. This string is the accessible
   name of the row's curve control, so it is read by a screen reader and reachable by keyboard focus, not by
   hover alone - a value that lives only in a tooltip is deleted for touch and keyboard users. It is also the
   hover text, and the drawer shows the same facts laid out in full. Each figure names its own scope and its
   own window, because that is the whole point of the memory tab's two labelling fixes. */
function memCurveSummary(mem) {
  if (!mem || typeof mem !== "object") return "";
  const w = memWindows(mem), bits = [];
  const rest = Array.isArray(mem.idle_rss_series) ? mem.idle_rss_series.filter((p) => p && typeof p.rss_mib === "number") : [];
  const idle = mval(mem.idle_rss_mib);
  if (rest.length >= 2) {
    const vs = rest.map((p) => p.rss_mib), lo = Math.min(...vs), hi = Math.max(...vs);
    const pts = rest.slice().sort((a, b) => a.t_s - b.t_s);
    // A record can carry the SERIES and no idle SCALAR (the envelope is absent for its own published reason).
    // fmt1(null) throws, and a fabricated 0 would be worse than the omission, so the median clause is simply
    // not composed - the span and the shape still are, because those come from the series itself.
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
  /* COMPACT IS THE TABLE ROW: the lifecycle curve alone, no prose labels and no caption lines. A memory row
     was ~350px tall - six block elements stacked in one cell - so three gateways filled a screen and a
     fourteen-row comparison table stopped being comparable ("massive rows are not professional"). Everything
     that was in those captions is in memCurveSummary, on the control's accessible name, and in the drawer. */
  if (opts.compact) return rssLifecycle(mem, opts);
  const idle = mval(mem.idle_rss_mib);
  // The load panel needs the scalar the Recovered column publishes, and the window it was read at, so its
  // caption can name that same window instead of colliding with it. Both come off this very record.
  const load = rssSparkline(mem.rss_series, mval(mem.load_s), idle, "load", {
    recoveredAt: mval(mem.recovered_rss_mib), recoveryWindowS: memWindows(mem).recovery,
  });
  const idleSeries = mem.idle_rss_series;
  if (!Array.isArray(idleSeries) || idleSeries.length < 2) return load;
  // The idle window carries no load boundary to mark, so no dotted rule: the whole window IS idle.
  // kind:"idle" is what switches the axis, the stamp and the aria-label onto the at-rest vocabulary. It is
  // passed explicitly rather than inferred from `loadEndS == null`, because a LOAD window whose load_s is
  // absent also passes null there - and that row would then have been captioned as if it were at rest.
  const idleCurve = rssSparkline(idleSeries, null, idle, "idle");
  if (!idleCurve) return load;
  const verdict = idleStatic(mem);
  return `<div class="rss-pair">` +
    `<div class="rss-half"><div class="rss-label muted">at rest, before any load${verdict ? ` · ${esc(verdict)}` : ""}</div>${idleCurve}</div>` +
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
      // `m.cell` is a metric whose value is a reading PLUS its evidence (the frontier), so the record does
      // not carry a plain envelope under `m.k` and the cell renderer is the metric's own. Everything else is
      // an envelope read through metric(), exactly as before.
      h += `<dl>` + l.metrics.map((m) => ({ m, c: m.cell ? m.cell(j, st) : metric(j[m.k], m.fmt) })).filter((x) => !x.c.na || x.c.failed).map(({ m, c }) => {
        if (c.failed)
          return `<div><dt>${esc(txt(m.label))}</dt><dd class="failtext" title="${esc(c.note || "")}">${esc(c.text)}</dd></div>`;
        const conc = c.env && c.env.concurrency;
        const cc = conc != null && c.v > 0 ? ` (@ c=${fmtInt(conc)})` : "";
        const zeroWhy = c.v === 0 && c.env && ZERO_WHY[c.env.note];
        return `<div><dt>${esc(txt(m.label))}</dt><dd${c.note ? ` title="${esc(c.note)}"` : ""}>${esc(c.text + cc)}${
          zeroWhy ? ` <span class="muted">(${esc(zeroWhy)})</span>` : ""}</dd></div>`;
      // The engine's definition of this lane's metrics, right under the numbers it defines: a reader who
      // asks "what does 'under 10 ms' actually mean" gets the answer without leaving the drawer.
      }).join("") + `</dl>` + (l.extra ? l.extra(j) : "") +
        definitionsFold(LANE_DEFINITION_PREFIXES[l.key] || [], stateData(st)) + `${laneStamp(j)}`;
    }
    h += `</section>`;
  }

  /* protocol matrix row with evidence */
  h += `<section class="drawer-lane"><h4>Protocol matrix</h4>`;
  // PRESENCE IS NOT CONTENT. This read `g.matrix.cells`, a legacy FLAT map that the per-cell artifact
  // no longer fills - it is now always `{}`, which is truthy, so it sailed past the guard and every
  // one of the six rows rendered "n/a" on every gateway on the board. The measurements were there the
  // whole time, one level down in `upstreams[egress].cells[ingress]`.
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

  // THE SWEEP THE WHOLE FRONTIER IS READ FROM, plotted once. The caption no longer says "the search sweeps
  // then bisects to the peak": nothing searches for a shape any more - every rung probed is a rung
  // considered, and the six published readings are maxima over subsets of these same points.
  h += `<section class="drawer-lane"><h4>The concurrency sweep</h4>` +
    `<p class="lane-note muted">One sweep per cell, and every published throughput reading is a maximum over some subset of these same rungs: every point is a real probe, nothing decides when to stop looking. The marked dot is the reading at the bound the board is currently showing (${esc(boundLabel(selectedBound(st)))}), at the concurrency it was observed at.</p>` +
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
      // `m.cell` is a metric that is a reading plus its own evidence (a frontier reading), whose renderer
      // travels with the metric; everything else is a plain envelope on the record.
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
    /* THE CURVES, SIDE BY SIDE. The rows above give three gateways' six readings as digits, which is
       precise and slow to read; this row is the same three curves on the same shared scale, which is the
       comparison a reader makes in one glance - and it is the comparison the two retired scalars made
       impossible, since a single number cannot show that one of these machines needs a loose tail and
       another does not. */
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

  // CHOOSER-AWARE + BOUND-AWARE: each gateway's CHOSEN cell, its ONE sweep, marked at the SAME bound the
  // rest of the panel is showing - through perfSweepSeries, the one place that decides what a perf curve is,
  // so the drawer and the compare panel cannot plot a different thing from the same record.
  const series = gws.map((g, i) => ({ ...perfSweepSeries(g, { sustained: CMP_COLORS[i] })[0], label: g.display, color: CMP_COLORS[i] }));
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
   Dead on the live UI (cellPopFull is used instead) but still EXPORTED. It reads every metric through
   mval(), so an ABSENT figure surfaces as its own reason rather than as a bare number - there is no
   ungated field here for a render site to leak. (It used to be the suppression this guarded against; a
   measurement near the rig's ceiling is now published, and only a genuine absence has nothing to show.) */
function cellPerfTip(cell, ingress, egress, best, boundMs = selectedBound()) {
  const p = cell && cell.served === true ? cell.perf : null;
  const rd = p ? frontierAt(frontierOf(p), boundMs) : null;
  const rps = rd ? mval(rd.rps) : null;
  const lat = p ? mval(p.added_latency_p99_us) : null;
  // A record with no reading AT ALL at this bound has nothing to say about throughput here. It used to test
  // `isEnvelope(p.rps_sustained_20ms)`, i.e. "does this record carry the field"; the equivalent question now
  // is whether the frontier has a reading at the bound being displayed.
  if (!p || !rd) return "";
  // An ABSENT reading at this bound: show the certified added-latency alone rather than a bare "".
  if (rps == null) {
    // THE RECORD'S OWN REASON, NOT A GUESSED ONE. This read "sustained RPS n/a: rig-limited" for EVERY
    // absent sustained figure - not_measured, harness_error and below_resolution alike - so a hole this
    // rig caused and a hole nobody has measured told the reader the same untrue thing. Everywhere else in
    // this file an absence reason travels through METRIC_NOTES; this one sentence was hand-written.
    const why = rd.rps && rd.rps.reason ? noteText(rd.rps.reason) : "not measured";
    return lat != null ? `+${fmtInt(lat)} µs p99 added (no reading at ${boundLabel(boundMs)}: ${why})` : "";
  }
  const bpRd = frontierAt(frontierOf(best), boundMs);
  const bp = cellPath(best), bRps = bpRd ? mval(bpRd.rps) : null;
  // The bound is NAMED, and the "≥" travels: a floor stated as a ceiling is the one thing this popup must
  // never do (the sweep ran out of ladder; the rate is real and maximality is not established).
  let s = `${rd.lower_bound === true ? "≥ " : ""}${fmtRate(rps)} req/s ${boundClause(boundMs)}`;
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
  // THROUGHPUT AT THE BOUND THE BOARD IS SHOWING, labelled with that bound, through the same reader the
  // Performance table uses - so the popup and the table can never disagree about which reading they show.
  // It replaces "Sustained @20ms" and "Max proxy RPS": one sweep collapsed twice, and the first label named
  // a bound the qualifying gate did not use.
  /* AND IT IS ALWAYS RENDERED, even when this cell has no reading at that bound. Every other row here is
     dropped when it has nothing to say, which is right for a metric that is simply absent - but the bound is
     the READER'S CHOICE, and a popup that silently omits the throughput row leaves them unable to tell
     "this cell cannot do it at 5 ms" from "the popup does not show throughput". Those are opposite
     conclusions, and on the slowest gateways the first one is most of the row. */
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
  // The delta: this cell vs the gateway's own REPRESENTATIVE diagonal (best_cell). "" for that cell itself.
  const cellPerf = chooserCellPerf(g, st);
  // THE CELL'S CURVE, in the popup that is the visual face of Custom mode. A single rate tells a reader
  // whether this cell is fast; the shape tells them whether it is fast because the tail was allowed to grow,
  // which is the difference this matrix is most often used to look for.
  const shapeBlock = cellPerf ? frontierBlock(cellPerf, { compact: true }) : "";
  const cellPerfLabeled = cellPerf ? { ingress, egress, ...cellPerf } : null;
  const delta = deltaToPeak(cellPerfLabeled, g.best_cell);
  const bp = g.best_cell ? g.best_cell.path : null;
  // "vs its own cell", NOT "vs peak": the reference is best_cell, the representative diagonal the chooser
  // picks (openai where served, else lowest added latency). Calling it the peak told a reader the other
  // cell was this gateway's fastest, which the selection never established - and a NEGATIVE delta against
  // a cell wrongly labelled "peak" reads as impossible rather than as ordinary.
  const deltaBlock = delta
    ? `<div class="pop-delta">vs its own cell (${esc(MATRIX_LABELS[bp.ingress] || bp.ingress)}→${esc(MATRIX_LABELS[bp.egress] || bp.egress)}): ${esc(delta)}</div>`
    : (cellPerf && bp && bp.ingress === ingress && bp.egress === egress
      ? `<div class="pop-delta muted">this IS the cell that ranks the Performance tab</div>` : "");
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
  return head + perfBlock + shapeBlock + deltaBlock + verdict + reverify + cta;
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
/* renderCharts(): the Charts tab - horizontal ranked bars, drawn from the live board.

   REPLACED 25 STATIC PNGs. Half of those existed only because "top 5" had to be decided at render
   time, by one metric, before the reader arrived; the rest froze one bound and one cell into an
   image. Here the metric is a control, the bound and cell come from the selectors every other tab
   honours, and "top 5" is a row count rather than a decision baked into a filename.

   A GATEWAY WITH NO VALUE IS LISTED, NOT DROPPED. A chart that silently omits the rows it cannot
   draw reports a smaller, tidier field than the one that was measured - and on this board an absent
   number usually means a refusal (the cell was not served, the window had failures) that a reader
   needs to see. They are named under the chart with their reason. */
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

/* drawRankedBars: one bar per gateway, longest-first by the metric's own direction.

   LOG WHERE THE METRIC SAYS SO. Cost per request spans 2,247x on the current board; on a linear axis
   twelve of fourteen gateways are a single pixel wide beside the slowest, which destroys the very
   comparison the chart exists to make. Gridlines fall on decades and are labelled in the metric's own
   units, so the scale is readable as "ten times" rather than as an unlabelled curve. */
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
  /* WHICH BENCHMARK PRODUCED THIS BOARD. Stated once here so the per-row sha has something to be
     compared against: a red row means "not this one", and a reader should not have to infer what
     "this one" is. Also counts the rows that are not on it, because one red pill in a long table is
     easy to miss and the count is the thing that decides whether the board is safely comparable.

     "NOT ON THIS ENGINE" IS TWO DIFFERENT FACTS AND THE FOOTER MUST NOT MERGE THEM.
     It counted `!g.engine.current` and called every one of them "measured on an older version". That
     predicate is true for both of:
       - a row stamped with a DIFFERENT sha: it has numbers, and they are not comparable with the rest;
       - a row stamped with NO sha (`engine.sha === null`): it has no numbers at all yet.
     On the 2026-07-30 board the second set was seven gateways mid-run and the first was empty, so the
     footer told a reader we were showing stale results for half the field when we were showing none. That
     is a false statement about the board's own trustworthiness, in the one line a reader consults to decide
     whether to trust it. The null stamp is deliberate on the producer's side (a row with no stamp must not
     imply it matches the board's engine); the defect was reading it as a version.
     Each clause appears only when its set is non-empty, so a clean board still reads as one bare version.
     Extracted as a pure function for the same reason rigStamp is: the whole defect was in the SENTENCE, and a
     sentence composed inside a DOM renderer is a sentence no test can read. */
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
  // Sorts on the SAME token the cell renders (versionToken), so a row whose column says "no version
  // published" sorts as the null it is rather than on a build path the column never showed.
  version: (g) => { const v = versionToken(g); return v ? v.toLowerCase() : null; },
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
/* measuredBuild(g): the build stamp of WHAT ACTUALLY RAN, and nothing else - null for a gateway with no run.
   Split out of gatewayBuild because gatewayBuild deliberately falls back to the manifest pin, and a caller
   that needs to tell those two apart cannot. The Version column is exactly that caller: it has to say
   "measured running apache/apisix:3.17.0-debian" for one row and "pinned in the manifest, not yet measured"
   for another, and with the fallback folded in it captioned every unmeasured row as though its pin were a
   launch stamp ("Launched as: v0.5.0"). */
const measuredBuild = (g) => {
  if (g && g.matrix && g.matrix.build) return g.matrix.build;
  const rec = displayedRecords(g || {}).find((r) => r.source && r.source.build);
  return rec ? rec.source.build : null;
};
const gatewayBuild = (g) => {
  const measured = measuredBuild(g);
  if (measured) return measured;
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
/* parseBuildVersion(full): the VERSION TOKEN a build stamp identifies, or NULL when it identifies none.
   "ghcr.io/x/y:v1.3.1" -> "v1.3.1"; "somepkg==1.93.0" -> "1.93.0"; "repo@9649b27..." -> "@9649b27";
   "somegateway 1.4.1" -> "1.4.1".
   THE NULL RETURN IS THE POINT, AND IT IS WHY THIS IS SPLIT OUT OF fmtBuild. A build stamp is whatever the
   engine recorded for how the gateway was launched, and for a source build that is a PATH, not a version:
   the live board showed Helicone's Version as "target/release/ai-gat…" and LiteLLM · Rust's as
   "litellm-ai-gateway". Those are a compiler output path and a binary name. fmtBuild's four recognisers
   (":tag", "==ver", "@ref", trailing " vN") all declined them, and fmtBuild's fallback then printed the raw
   head anyway - so the column asserted a version it had explicitly failed to find. Returning null lets the
   caller fall back to the manifest's own pin (versionToken) instead of dressing a path up as a release. */
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
/* fmtBuild(full): the short form of a build stamp for display. Unchanged behaviour - when no version can be
   parsed it still shows the (truncated) stamp - because there are places where the stamp itself is the
   honest thing to show. The VERSION COLUMN is not one of them; it goes through versionToken. */
const fmtBuild = (full) => {
  const v = parseBuildVersion(full);
  if (v != null) return v;
  const head = String(full).split(" (")[0].trim();
  return head.length > 24 ? head.slice(0, 21) + "..." : head;
};
/* versionToken(g): WHAT VERSION OF THIS GATEWAY WAS MEASURED, or null when nothing published says.
   Two sources, in this order and for this reason:
     1. the build stamp of what actually ran, when it names a version (an image tag, a pinned package, a
        commit ref) - that is the strongest evidence, because it is what the process reported;
     2. otherwise `g.version`, the manifest's own pin. For a source build that is the commit the harness
        checked out, which IS the version - Helicone pins 9649b27 and LiteLLM · Rust 6980723 - and rendering
        it as "@9649b27" marks it as a commit rather than letting a bare hex string read as a release name.
   Null when neither exists, and the cell then says so in words. It must never fall through to the build
   stamp's raw text: a filesystem path in a column headed "Version" is a false claim, not a partial one. */
function versionToken(g) {
  const build = measuredBuild(g);
  const parsed = build ? parseBuildVersion(build) : null;
  if (parsed != null) return parsed;
  const pin = g && g.version ? String(g.version).trim() : "";
  if (!pin) return null;
  return /^[0-9a-f]{7,40}$/i.test(pin) ? "@" + pin.slice(0, 7) : pin;
}
/* versionBasis(g): WHERE the Version cell's token came from, as the cell's tooltip.
   Three distinct answers, and they are three different epistemic states a reader is entitled to tell apart:
   the full launch stamp when the token was parsed out of it; "the stamp names no version, here is the pin"
   when the gateway ran from a source build (Helicone's stamp is `target/release/ai-gateway`, a compiler
   output path); and "pinned, not yet measured" for a gateway still awaiting its first run. */
function versionBasis(g) {
  const build = measuredBuild(g);
  if (build && parseBuildVersion(build) != null) return `Measured running: ${build}`;
  if (build) return `Launched as: ${build} - that stamp names no version (it is a source build), so this is the commit pinned in the gateway's manifest, which is what the harness built.`;
  if (versionToken(g)) return "The version pinned in the gateway's manifest, which is what the harness builds. This gateway has not been measured on the current benchmark yet, so there is no launch stamp to read instead.";
  return "Nothing published for this gateway names a version: neither a build stamp of what ran nor a pin in its manifest.";
}

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
  /* THE OUTGOING VIEW IS READ BEFORE state.view MOVES, and that ordering is the whole correctness of the
     mode memo below. It used to read `modeFamily(state.view)` one line AFTER `state.view = view`, so
     `leaving` was always the view being ARRIVED at: the two families always compared equal, the stash branch
     was unreachable, and the memo was frozen at its newState defaults forever. What shipped was not a lossy
     memo - it was `state.mode = resolveMode(modeMemo[family])` on every render, which silently threw away
     the mode the URL had just decoded. `/gateways/performance?mode=same&d=openai` rendered Own cell WITH
     "?mode=same" still in the address bar, i.e. a shared link showed different cells from the ones its own
     URL named. */
  const leaving = modeFamily(state.view);
  state.view = view;
  // Memory's data-derived Same default is seeded on ARRIVAL at memory, not once globally at boot, so
  // the other tabs keep the dialect default they declare (see seedMemorySameDialect).
  seedMemorySameDialect();
  const nx = modeOnArrival(leaving, view, state.mode, state.modeMemo);
  state.mode = nx.mode;
  state.modeMemo = nx.memo;
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
  // The bound selector belongs only to the tabs whose numbers are READ AT a bound. Offering it over the
  // streaming or memory columns would imply those figures had a tail-latency bound too, which is the exact
  // class of claim - a surface implying a bound it did not use - that the frontier exists to end.
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
    // The bound travels with the rest of the shareable state; a decoded ?bound= that is missing leaves the
    // default in place (newState), never undefined - selectedBound() would coerce it back anyway, but a
    // state field that means "unbounded" when absent is exactly the confusion this board is removing.
    bound: st.bound === null || FRONTIER_BOUNDS_MS.includes(st.bound) ? st.bound : DEFAULT_BOUND_MS,
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
  // EVERY CHOOSER VIEW, not memory alone. This was memory-only because the perf lanes defaulted to Own
  // cell, where no dialect is selected and seeding one would silently rewrite a deep link's ?d=. Those
  // lanes now default to Same, so the dialect IS their selection, and opening them on a hardcoded
  // `openai` rather than the dialect the run says most of the field can be compared on would be the
  // same editorial choice widestDialect exists to avoid.
  //
  // The invariant that actually protects deep links is unchanged and is the one below: a pinned ?d=
  // always wins. Keying on the view name never protected anything a pin did not already protect.
  if (!CHOOSER_VIEWS.has(state.view) || state.sameDialectPinned || !state.data) return;
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
  /* TITLE THE PAGE FROM THE URL, BEFORE THE FETCH.
     The title was only ever written from inside showView, which runs in renderAll, which runs after
     data.json resolves - three quarters of a megabyte. Until then every deep link showed index.html's
     static <title>, so a tab opened in the background, a link preview, and anything that read the document
     before the fetch landed all reported the generic site title for a specific view; on the failure path
     (the .catch below) it stayed generic forever. The state is fully decoded one line above this, so the
     title is knowable immediately and there is no reason to make it wait on a network round trip. */
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
      // The view still titles itself on the failure path: the reader's tab should say which view they asked
      // for even when its numbers could not be loaded.
      updateTitle();
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
    CHOOSER_MODES, MODE_LABELS, MODE_TIPS, chooserCellPerf, chooserDialects, chooserPerfCell, chooserCellStream, chooserStreamCell, chooserHasCell, deltaToPeak, cellPopFull,
    // memory cell chooser (Min | Max | Same | Custom, never Peak) + the matrix roster hole-closer.
    MEM_CHOOSER_MODES, CHOOSER_VIEWS, modesFor, defaultMode, resolveMode, modeFamily, modeOnArrival, memoryMode,
    perCellMemory, memoryCells, hasPerCellMemory, widestDialect, chosenMemory, memoryFor,
    idleAcrossCells, neverPlateaued, worstGrowth, memCellTip, neverPlateauedPill,
    idleStatic, memShape, memGrowing, memShaped,
    hasMatrixGrid, matrixFailureReason, matrixRoster, hasCost, costWindowConc, rigResolutionPct, indistinguishable, tiedRuns, costSaturation, CHART_METRICS, chartRows,
    laneRecord, lanePathNote, perfSweepSeries, concAt,
    // THE FRONTIER: the constants (mirrored from seal.mjs and checked against it), the readers every
    // surface goes through, and the two renderers that make the curve's SHAPE legible. Exported because the
    // shape is the headline finding and a renderer no test can reach is a renderer that can be deleted
    // without anything going red - which is how the retired scalars' captions came to describe a test that
    // never ran. `sustainedChooserCell` / `maxProxyChooserCell` are GONE with the two metrics they read.
    FRONTIER_BOUNDS_MS, DEFAULT_BOUND_MS, BOUND_CHOICES, BOUND_VIEWS, SORT_ALIASES,
    boundLabel, boundClause, boundColLabel, boundColId, selectedBound, fmtTail,
    frontierOf, frontierAt, frontierCell, frontierHeld, frontierFullRate, heldPct, frontierSpark, frontierBlock, boardFrontierScale,
    frontierChooserCell, frontierBoundCell, frontierShapeCell, frontierShapeTd, frontierCaption, selectBound,
    // #4: the ×N gain factor's sort key and the reference prose that says what the ratio is OF. Exported
    // because "×1.0 from 1 ms" and "×1.0 from 50 ms" are opposite findings that used to render identically,
    // and a guard no test can reach is a guard that can be deleted without anything going red.
    heldSortKey, HELD_NOTHING_INDEX, HELD_REFERENCE, BOUND_GROUP_LABEL, theadHtml, colgroupHtml, colWidth, COL_WIDTHS,
    // #1: the lead/notes split, its flattener, and the dispatch every renderer goes through.
    captionText, captionFor, notesFold,
    // #6: the version column's two halves. parseBuildVersion returns NULL for a build stamp that names no
    // version (a source build's binary path), which is the whole fix; versionToken is what the cell renders.
    parseBuildVersion, versionToken, versionBasis, measuredBuild, ROSTER_KEY,
    // #7: the per-view document title, pure so it is checkable without a document.
    pageTitle, SITE_TITLE,
    // #5: the idle axis floor, so the "flat must render flat" guard can assert against the real constant.
    IDLE_AXIS_MIN_SPAN, rssCurves, fmt2,
    /* THE MEMORY TAB'S LABELLING + ROW-HEIGHT WORK. Exported because every one of these was a case of a
       CORRECT number placed so as to look wrong, and a guard that cannot reach the composed sentence cannot
       stop the wording sliding back: idleShapeNote (a span is not a rate), recoveryTail (two windows, not two
       facts), releaseMark (released nothing vs released most, drawn), rssLifecycle (one process, one line,
       with the axis break SHOWN) and memCurveSummary (the reachability guarantee - every folded figure, on a
       focusable control's accessible name). */
    idleShapeNote, recoveryTail, releaseMark, rssLifecycle, memCurveSummary, LIFECYCLE_IDLE_FRAC,
    definitionsFor, definitionsFold, DEFINITION_PREFIXES, LANE_DEFINITION_PREFIXES, METRIC_NOTES,
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
    rowComparator, VIEW_TIEBREAK, matrixDiagonal, bestIndex, laneServed, seedMemorySameDialect,
    // audit #21: the rig-provenance footer stamp + the live state it reads, so the class test can drive it.
    rigStamp, benchmarkVersionStamp, state,
    // THE SURFACES THAT WERE UNREACHABLE FROM A DOM-FREE SUITE, and were therefore covered by nothing:
    // the drawer (drawerHtml was called by no test at all - deleting the clause that keeps a MEASURED
    // FAILURE visible in it broke no test), the compare panel's whole body (extracted from renderCompare
    // for exactly this reason), and the one place a gateway's repo URL reaches an href.
    drawerHtml, compareBodyHtml, gwLink, recordShowsValues,
  };
} else {
  boot();
}
