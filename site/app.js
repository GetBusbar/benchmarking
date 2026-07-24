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
// out. `charts` folds into method; `results` was the old blended tab.
const VIEWS = ["gateways", "passthrough", "translation", "streaming", "matrix", "method"];
const VIEW_LABELS = { gateways: "Gateways", passthrough: "Passthrough", translation: "Translation", streaming: "Streaming", matrix: "Protocol matrix", method: "Method" };
// The default (bare /gateways) view: the roster overview. The old default, passthrough, stays a
// real tab at /gateways/passthrough.
const DEFAULT_VIEW = "gateways";
const PERF_VIEWS = new Set(["passthrough", "translation", "streaming"]);
// Old shared URLs pointed at results/charts; map them onto the new tabs so links keep resolving.
const VIEW_ALIASES = { results: "passthrough", charts: "method" };
// Each perf tab's default (and honest headline) sort column; a clean URL omits the sort when it
// equals this, and switching tabs snaps to it unless the URL pins another.
// Streaming defaults to added TTFT (asc), NOT streams-sustained: the sustained count saturates at the
// harness cap (1024 in the current field data) so it ties several gateways and breaks ties by name,
// floating a slow-TTFT gateway above a fast one at the same count. Added TTFT is the streaming-overhead
// discriminator that a user actually feels first and it does not saturate.
const VIEW_SORT = { passthrough: "rps20", translation: "xlrps", streaming: "sttft" };

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
   INDEPENDENTLY, so the board legitimately carries mixed per-gateway ages (busbar today, kong 3
   weeks ago) — that is honest, not a bug. We surface each row's OWN measured_at ("measured 3d ago",
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
  return `<span class="measured-at${g.stale ? " stale" : ""}" title="${esc(stampWithAge(g.measured_at, now))}">${esc(rel)}</span>${stalePill}`;
}

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
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

/* naText: compact honest label for a lane that was not served. The suites emit
   long diagnostic notes (passthrough evidence, launch errors); those must never
   be dumped as metric values or they blow the table layout wide open. The cell
   shows a short badge and the full note (rig paths scrubbed) travels in the
   title tooltip; the drawer shows the first line plus a folded Evidence block. */
function naText(j, flag, errKey) {
  if (!j) return { text: "not measured", note: "" };
  const note = stripRigPaths(j[errKey] || j.serve_error || "");
  let text = "not served";
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
function lane(g, suite, flag, errKey, pick) {
  const j = g[suite];
  if (!j || j[flag] === false) {
    const na = naText(j, flag, errKey);
    return { v: null, text: na.text, note: na.note, na: true };
  }
  return pick(j);
}

/* canonicalPerf: THE single passthrough perf record every surface reads (table, drawer,
   compare; charts.py reads the same best_cell from data.json). gen-data emits g.best_cell
   from the matrix per-cell sweep, or synthesizes it from the perf suite when no swept
   diagonal exists (source:"perf-fallback"). Only a legacy bundle with no best_cell at all
   falls back to the raw perf suite object (whose field names match). */
function canonicalPerf(g) {
  if (g.best_cell) return { served: true, ...g.best_cell };
  return g.perf || null;
}
/* canonicalXlate: the ONE Translation record for the drawer/compare: the SAME matrix cell
   the Translation surfaces use (openai in -> the gateway's measured egress), normalized by
   gen-data into g.translation_cell (source:"matrix" | "xlate-fallback", direction in
   ingress/egress). Metric keys are normalized onto the lane's xlate_* names. Legacy bundles
   without translation_cell fall back to the raw xlate suite object. */
function canonicalXlate(g) {
  const t = g.translation_cell;
  if (t) return {
    xlate_served: true, source: t.source, ingress: t.ingress, egress: t.egress,
    build: t.build, measured_at: t.measured_at,
    xlate_added_latency_p50_us: t.added_latency_p50_us,
    xlate_added_latency_p99_us: t.added_latency_p99_us,
    xlate_rps_sustained_20ms: t.rps_sustained_20ms,
  };
  return g.xlate || null;
}

/* canonicalStreaming: THE single streaming record every surface reads. gen-data projects it from the
   BEST DIAGONAL matrix cell's streaming (g.streaming, source:"matrix") — the SAME cell the passthrough
   headline is projected from, so streaming and RPS/latency are read off one cell (one source of truth;
   check-consistency asserts the headline streaming == this cell's). Returns a record with
   stream_served + normalized keys, or the legacy stream-fallback g.streaming, or null. */
// MEDIUM-6: the cpu-fps relay is a valid gateway-vs-ceiling comparison ONLY when the harness certified
// it — cpu_fps present + positive AND explicitly NOT mock-bound (cpu_fps_mock_bound === false). A null
// mock-bound flag means the ceiling probe read 0 and the number could NOT be certified (unverifiable),
// and a true flag means an unpinned box floored it. In BOTH cases charts.py suppresses the bar
// (streamcpu_valid=false); the drawer/compare "CPU-bound fps (peak)" metric MUST match that visibility
// or the site shows a number the chart does not (check-consistency asserts this can't diverge). We drop
// cpu_fps from the canonical record unless it is explicitly certified, so every site surface reads n/a
// exactly when the chart draws no bar. The raw value stays on g.streaming for provenance/download.
function cpuFpsCertified(s) {
  return s != null && s.cpu_fps != null && Number(s.cpu_fps) > 0 && s.cpu_fps_mock_bound === false;
}
// MEDIUM-R2-2: streams_sustained is gated EXACTLY like cpu_fps. A rig-limited sustained count
// (streams_sustained_mock_bound=true — the bisect saturated near the paced-mock ceiling) or an
// unverifiable one (mock-bound=null — the reference ceiling read 0) is NOT a gateway limit; it must
// not draw a full bar, rank in the top-N, or win the best:max compare. charts.py suppresses the bar
// via stream_sustained_valid (present + >0 + NOT mock-bound); the site must read n/a on every surface
// exactly when the chart draws no bar (check-consistency asserts they can never diverge). Only an
// explicitly-certified (mock_bound === false) count survives — the raw value stays on g.streaming for
// provenance/download, symmetric with cpuFpsCertified.
function sustainedCertified(s) {
  return s != null && s.streams_sustained != null && Number(s.streams_sustained) > 0 && s.streams_sustained_mock_bound === false;
}
function canonicalStreaming(g) {
  const s = g.streaming;
  if (!s) return null;
  const rec = { stream_served: true, ...s };
  if (!cpuFpsCertified(s)) rec.cpu_fps = null;   // uncertified/mock-bound → n/a on every site surface
  if (!sustainedCertified(s)) rec.streams_sustained = null;  // rig-limited/unverifiable → n/a, no bar
  return rec;
}
/* canonicalMemory: THE single memory record. gen-data projects it from the matrix's ONE process-level
   RSS read (g.memory_read, source:"matrix"), or the legacy memory suite (source:"memory-fallback").
   Returns a record with served + the idle/peak fields, or null. */
function canonicalMemory(g) {
  const m = g.memory_read;
  if (m) return { served: true, ...m };
  return null;
}

/* passCell: the Passthrough tab reads ONLY the canonical record (g.best_cell). When best_cell
   exists it is THE record: a field it lacks reads n/a, never silently patched from a different
   source (that is exactly the numeric divergence this rule exists to kill). Only a gateway with
   NO best_cell at all (legacy bundle) falls back to its perf suite. */
function passCell(g, key, fmt) {
  if (g.best_cell)
    return g.best_cell[key] != null
      ? { v: g.best_cell[key], text: fmt(g.best_cell[key]), na: false }
      : { v: null, text: "n/a", na: true };
  return lane(g, "perf", "served", "serve_error", (j) =>
    j[key] != null ? { v: j[key], text: fmt(j[key]), na: false } : { v: null, text: "n/a", na: true });
}

/* streamCell: the Streaming tab reads ONLY the canonical streaming record (g.streaming, projected by
   gen-data from the best diagonal matrix cell's stream, or a legacy stream-fallback). A field it lacks
   reads n/a; a gateway that did not stream (no g.streaming) shows the legacy stream suite's not-served
   evidence when present, else a plain n/a. Same one-source discipline as passCell. */
function streamCell(g, key, fmt) {
  const s = canonicalStreaming(g);
  if (s)
    return s[key] != null
      ? { v: s[key], text: fmt(s[key]), na: false }
      : { v: null, text: "n/a", na: true };
  // Legacy fallback (a bundle whose gen-data did not project g.streaming): the raw stream suite uses
  // stream_*-prefixed keys, so map the canonical key onto the suite key.
  const legacyKey = {
    added_ttft_p99_us: "stream_added_ttft_p99_us",
    added_gap_p99_us: "stream_added_gap_p99_us",
    streams_sustained: "stream_sustained_streams",
    cpu_fps: null,   // cpu-fps lived in the separate streamcpu suite; not on the stream lane
  }[key] ?? key;
  if (legacyKey == null) return { v: null, text: "n/a", na: true };
  return lane(g, "stream", "stream_served", "stream_error", (j) =>
    j[legacyKey] != null ? { v: j[legacyKey], text: fmt(j[legacyKey]), na: false } : { v: null, text: "n/a", na: true });
}
/* memCell: the memory columns read ONLY the canonical memory record (g.memory_read, the matrix's one
   process-level RSS read, or a legacy memory-fallback). Same discipline as passCell/streamCell. */
function memCell(g, key, fmt) {
  const m = canonicalMemory(g);
  if (m)
    return m[key] != null
      ? { v: m[key], text: fmt(m[key]), na: false }
      : { v: null, text: "n/a", na: true };
  return lane(g, "memory", "served", "serve_error", (j) =>
    j[key] != null ? { v: j[key], text: fmt(j[key]), na: false } : { v: null, text: "n/a", na: true });
}

/* A throughput cell of 0 is a real, honest measurement, not a broken benchmark: the gateway
   served, but NO tested load level passed the qualifying gates (p99 < 1 s at <0.1% errors), so
   that run has no qualifying throughput ceiling. Distinct from sweep noise (see the caption and
   the check-consistency guard, which flags max=0 separately from a small inversion). The cell
   still shows "0" and this note travels in its title tooltip. */
const ZERO_RPS_NOTE = "served, but no tested load held p99 < 1 s at <0.1% errors (no qualifying throughput ceiling)";
function withZeroNote(cell) {
  return !cell.na && cell.v === 0 ? { ...cell, note: ZERO_RPS_NOTE } : cell;
}

/* The sustained@20ms cell carries the WINNING CONCURRENCY in its tooltip. rps and concurrency are
   bound by Little's law (rps <= concurrency / mock_delay), so the concurrency IS the latency story
   behind the throughput - e.g. 31,288 req/s peaked at ~1,024 in flight = ~33 ms effective latency.
   It's the in-flight load level at which the gateway's throughput was highest while still holding the
   gate (p99 < 1 s, <0.1% errors). Falls back cleanly when concurrency wasn't recorded (legacy). */
function sustainedCell(g) {
  const cell = withZeroNote(passCell(g, "rps_sustained_20ms", fmtInt));
  const cc = g.best_cell ? g.best_cell.rps_sustained_20ms_concurrency : null;
  if (!cell.na && cell.v > 0 && cc != null)
    return { ...cell, note: `Peaked at ${fmtInt(cell.v)} req/s with ${fmtInt(cc)} concurrent requests in flight - the load level that maximised sustained throughput under 20 ms LLM latency (higher concurrency added latency without more throughput).` };
  return cell;
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
  if (perf && perf[key] != null) return { v: perf[key], text: fmt(perf[key]), na: false };
  return { v: null, text: "n/a", na: true };
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
  render: (g) => {
    const a = g.repo
      ? `<a href="${g.repo}" target="_blank" rel="noopener">${esc(g.display)}</a>`
      : esc(g.display);
    // Per-gateway freshness: the board-wide "last benchmarked" (roster tab + homepage) already covers
    // the normal one-run case, so a per-row date on every perf row is redundant bloat. Show the per-row
    // measured_at + stale pill ONLY when this gateway is OUT OF SYNC with the board (g.stale) — the
    // honest signal for an independent update cadence — and otherwise keep the row compact.
    const badge = g.stale ? measuredBadge(g) : "";
    return `<td class="name">${a}${badge ? `<div class="row-measured">${badge}</div>` : ""}</td>`;
  },
};
const COLUMN_SETS = {
  // Passthrough (BEST-OF): each gateway on its best same-dialect passthrough diagonal. The "Tested on"
  // pill discloses which dialect that is (openai for most; a native dialect where openai is not served,
  // e.g. litellm-rust -> anthropic), so every gateway appears and the dialect is never hidden.
  passthrough: [
    COL_SEL, COL_NAME,
    { id: "tested", label: "Tested on", desc: false,
      title: "The same-dialect passthrough these numbers were measured on (openai when served, else the gateway's fastest native dialect) - pure forwarding, no translation",
      get: (g) => ({ v: g.best_cell ? g.best_cell.dialect : "", text: null, na: !g.best_cell }),
      render: (g) => {
        if (!g.best_cell) return `<td class="tested"><span class="muted">n/a</span></td>`;
        const d = g.best_cell.dialect;
        // Provenance disclosure (R4): a perf-fallback row was NOT measured by the matrix
        // per-cell sweep like the rest of the field; the pill says so (asterisk + tooltip).
        const fb = g.best_cell.source === "perf-fallback";
        const title = fb
          ? `measured on the perf-suite default path (${esc(d)} passthrough; no matrix per-cell sweep for this gateway yet)`
          : `measured on ${esc(d)}-in / ${esc(d)}-out passthrough (matrix per-cell sweep)`;
        return `<td class="tested"><span class="tested-pill" title="${title}">${esc(MATRIX_LABELS[d] || d)}${fb ? " *" : ""}</span></td>`;
      } },
    { id: "lat", label: "Added latency p99 (µs)", desc: false, title: "Gateway p99 minus direct-to-mock p99 at concurrency 1 on the gateway's best same-dialect passthrough (the Tested-on dialect) - pure forwarding, no translation",
      get: (g) => passCell(g, "added_latency_p99_us", fmtAdded) },
    { id: "rps20", label: "Sustained RPS @20ms", desc: true, title: "Sustained requests/sec with a 20 ms mock LLM latency (p99 < 1 s, <0.1% errors) on the Tested-on dialect (see the pill). Hover a cell for the concurrency it peaked at.",
      get: (g) => sustainedCell(g) },
    { id: "rpsmax", label: "Max proxy RPS", desc: true, title: "Throughput ceiling against an instant mock (p99 < 1 s, <0.1% errors) on the Tested-on dialect (see the pill)",
      get: (g) => withZeroNote(passCell(g, "rps_max_proxy", fmtInt)) },
    { id: "memidle", label: "Mem idle (MiB)", desc: false, title: "Process RSS after launch, before load (the matrix run's one memory read)",
      get: (g) => memCell(g, "idle_rss_mib", fmt1) },
    { id: "mempeak", label: "Mem peak (MiB)", desc: false, title: "Peak process RSS under large-payload load (the matrix run's one memory read)",
      get: (g) => memCell(g, "peak_rss_mib", fmt1) },
  ],
  // Translation: the pinned ingress->egress pair (state.xlateIn/xlateOut) chosen by the two dropdowns.
  // Every row is the identical path, so no per-row pill. When in == out the pair IS a passthrough
  // (same dialect, no translation), so this tab doubles as the per-dialect passthrough explorer. Same
  // metric depth as Passthrough (added latency p50/p99, sustained RPS, max proxy RPS).
  translation: [
    COL_SEL, COL_NAME,
    { id: "xll50", label: "Added latency p50 (µs)", desc: false, title: "Gateway p50 minus direct-to-mock p50 at concurrency 1 on the selected path",
      get: (g) => xlateCell(g, "added_latency_p50_us", fmtAdded) },
    { id: "xllat", label: "Added latency p99 (µs)", desc: false, title: "Gateway p99 minus direct-to-mock p99 at concurrency 1 on the selected path",
      get: (g) => xlateCell(g, "added_latency_p99_us", fmtAdded) },
    { id: "xlrps", label: "Sustained RPS @20ms", desc: true, title: "Sustained RPS @20ms on the selected path (p99 < 1 s, <0.1% errors)",
      get: (g) => withZeroNote(xlateCell(g, "rps_sustained_20ms", fmtInt)) },
    { id: "xlmax", label: "Max proxy RPS", desc: true, title: "Throughput ceiling against an instant mock on the selected path (p99 < 1 s, <0.1% errors)",
      get: (g) => withZeroNote(xlateCell(g, "rps_max_proxy", fmtInt)) },
  ],
  // Streaming: SSE passthrough, its own stall-gated ceiling.
  streaming: [
    COL_SEL, COL_NAME,
    { id: "sttft", label: "Added wait for 1st token p99 (µs)", desc: false, title: "Time to first token (TTFT): the extra wait before the stream's first token, gateway minus direct-to-mock, at concurrency 1, on the gateway's best same-dialect passthrough cell. Lower is better.",
      get: (g) => streamCell(g, "added_ttft_p99_us", fmtUsMs) },
    { id: "sgap", label: "Added gap between tokens p99 (µs)", desc: false, title: "The extra pause the gateway adds between streamed tokens, gateway minus direct-to-mock, on the best same-dialect passthrough cell. Lower is better.",
      get: (g) => streamCell(g, "added_gap_p99_us", fmtUsMs) },
    { id: "streams", label: "Streams sustained", desc: true, title: "Max concurrent SSE streams sustained (bisected true concurrency) with >=99.9% frame delivery, no stalls, <0.1% errors, on the best same-dialect passthrough cell",
      get: (g) => streamCell(g, "streams_sustained", fmtInt) },
  ],
  // Governance is RETIRED under matrix-sole-source: it is no tab AND no column. onthebench measures
  // every gateway at its default, out-of-the-box config; the governed suite was a non-default,
  // busbar-only launch (only busbar's manifest wired it), so it is not a neutral-board metric and the
  // board neither ranks it nor shows a governed column/drawer section. governed/run.sh stays on disk
  // (unused); gen-data.mjs no longer scans it and emits no supports_governed flag.
};
/* The set of columns for a view; perf tabs use COLUMN_SETS, everything else has no table. */
function columnsFor(view) { return COLUMN_SETS[view] || COLUMN_SETS.passthrough; }
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
  {
    key: "perf", label: "Latency & throughput", flag: "served", err: "serve_error",
    get: canonicalPerf,
    pathNote: (j) => !j.source ? "measured on the perf-suite default path (legacy result)"
      : j.source === "perf-fallback"
        ? `measured on the perf-suite default path (${laneDialect(j.dialect)} passthrough; no matrix per-cell sweep for this gateway yet)`
        : `measured on the ${laneDialect(j.dialect)} passthrough (matrix per-cell sweep, the same record the table ranks)`,
    metrics: [
      { k: "added_latency_p50_us", label: "Added latency p50 (µs)", best: "min", fmt: fmtAdded },
      { k: "added_latency_p99_us", label: "Added latency p99 (µs)", best: "min", fmt: fmtAdded },
      // The operating concurrency (concKey) is carried by the SAME record; the drawer shows it as
      // "(@ c=Y)" so the headline surfaces the load level its marked sweep peak sat at.
      { k: "rps_max_proxy", label: "Max proxy RPS", best: "max", fmt: fmtInt, concKey: "rps_max_proxy_concurrency" },
      { k: "rps_sustained_20ms", label: "Sustained RPS @20ms", best: "max", fmt: fmtInt, concKey: "rps_sustained_20ms_concurrency" },
    ],
  },
  {
    key: "memory", label: "Memory", flag: "served", err: "serve_error",
    get: canonicalMemory,
    pathNote: (j) => j.source === "matrix"
      ? "the matrix run's one process-level RSS read (default config, sustained large-payload load)"
      : "the memory suite's RSS read (legacy result)",
    metrics: [
      { k: "idle_rss_mib", label: "Idle RSS (MiB)", best: "min", fmt: fmt1 },
      { k: "peak_rss_mib", label: "Peak RSS (MiB)", best: "min", fmt: fmt1 },
    ],
  },
  {
    key: "stream", label: "Streaming", flag: "stream_served", err: "stream_error",
    get: canonicalStreaming,
    pathNote: (j) => j.source === "matrix"
      ? `measured on the ${laneDialect(j.dialect)} passthrough (the same best diagonal cell the headline is projected from)`
      : "measured on the stream suite's default path (legacy result)",
    metrics: [
      { k: "added_ttft_p99_us", label: "Added TTFT p99 (µs)", best: "min", fmt: fmtUsMs },
      { k: "added_gap_p99_us", label: "Added per-token p99 (µs)", best: "min", fmt: fmtUsMs },
      { k: "streams_sustained", label: "Streams sustained", best: "max", fmt: fmtInt },
      { k: "cpu_fps", label: "CPU-bound fps (peak)", best: "max", fmt: fmtInt },
    ],
  },
  {
    key: "xlate", label: "Translation", flag: "xlate_served", err: "xlate_error",
    get: canonicalXlate,
    pathNote: (j) => !j.source ? "Anthropic in -> OpenAI out (legacy xlate suite)"
      : j.source === "xlate-fallback"
        ? `${laneDialect(j.ingress)} in -> ${laneDialect(j.egress)} out (xlate suite; no matrix translation cell for this gateway yet)`
        : `${laneDialect(j.ingress)} in -> ${laneDialect(j.egress)} out (matrix per-cell sweep, the same cell the Translation tab reads)`,
    metrics: [
      { k: "xlate_added_latency_p50_us", label: "Added latency p50 (µs)", best: "min", fmt: fmtInt },
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
    view: DEFAULT_VIEW,
    q: "",
    sortCol: "rps20",
    sortDesc: true,
    needStream: false,
    needXlate: false,
    // Translation tab: the pinned ingress->egress pair the whole table is ranked on. Both ends are
    // fixed so every row does the identical translation (apples-to-apples); a gateway that does not
    // serve this exact pair is absent. Default is the fullest-served pair.
    xlateIn: "openai",
    xlateOut: "anthropic",
    cmp: [],        /* gateway keys selected for compare, max 3 */
    cmpOpen: false, /* compare panel visible */
    drawer: null,   /* gateway key open in the drawer */
  };
}
const state = newState();

/* Capability filter toggles. Governance is RETIRED (matrix-sole-source): it is neither a filter,
   a column, nor a drawer section — the governed suite was busbar-only and is not a board metric. */
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
  // Carry a non-default translation pair so a shared link opens the same ranking.
  if (st.xlateIn !== "openai") p.set("xin", st.xlateIn);
  if (st.xlateOut !== "anthropic") p.set("xout", st.xlateOut);
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
  if (MATRIX_CELLS.includes(p.get("xin"))) st.xlateIn = p.get("xin");
  if (MATRIX_CELLS.includes(p.get("xout"))) st.xlateOut = p.get("xout");
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
    // View-implicit filter: Translation lists only gateways that serve the pinned pair (every row
    // must be the identical path or the ranking lies). Passthrough is DELIBERATELY unfiltered:
    // every gateway must appear on its best passthrough (fairness beats strict same-dialect -
    // filtering a competitor out reads as hiding it). Streaming follows the same principle: a
    // MEASURED streaming refusal (stream_served:false, e.g. Portkey's) is a result, not a gap, so
    // those gateways stay in the table as muted "did not stream" rows sunk to the bottom (null
    // metric values sort last), matching the stream charts' "no SSE streaming" bars.
    if (st.view === "translation" && !servesXlatePair(g, st.xlateIn, st.xlateOut)) return false;
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
const TABLE_CAPTIONS = {
  passthrough: [
    "Pure forwarding, no translation.",
    "Each gateway on its best same-dialect path; the pill shows which dialect.",
    "Everyone appears. For one strict dialect, pin the same in and out in Translation.",
    "Sustained @20ms and max proxy RPS are independently measured ceilings; a small inversion between them is sweep noise, not an error.",
    "A 0 is not noise: the gateway served, but no tested load held p99 < 1 s at <0.1% errors, so that run found no qualifying ceiling.",
  ],
  streaming: [
    "Streaming responses (server-sent events).",
    "Added columns: extra time the gateway adds, before the first token and between tokens. Lower is better.",
    "Streams sustained: concurrent streams held without stalling. Higher is better.",
    "A gateway that answered but never framed SSE shows as \"did not stream\" (evidence in its tooltip): measured, not hidden.",
  ],
};
function updateTableCaption(view) {
  const el = document.getElementById("table-caption");
  if (!el) return;
  let lines;
  if (view === "translation") {
    const inL = (MATRIX_LABELS[state.xlateIn] || state.xlateIn);
    const outL = (MATRIX_LABELS[state.xlateOut] || state.xlateOut);
    lines = state.xlateIn === state.xlateOut
      ? [`${inL} in, ${outL} out: same dialect, so this is passthrough (no translation).`,
         "Only gateways that serve this dialect appear."]
      : [`Client speaks ${inL}, upstream speaks ${outL}; the gateway translates both ways.`,
         "Only gateways that serve this exact pair appear.",
         "Every row is the identical path, so the ranking is apples-to-apples."];
  } else {
    lines = TABLE_CAPTIONS[view] || TABLE_CAPTIONS.passthrough;
  }
  el.innerHTML = lines.map((l) => esc(l)).join("<br>");
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
        : `<td class="${sc.trim()}"${cell.note ? ` title="${esc(cell.note)}"` : ""}>${esc(cell.text)}</td>`;
    }).join("") + "</tr>"
  ).join("");
  // Empty-state line: a pinned translation pair no gateway serves (or filters that clear the
  // table) must never render as a bare header over nothing.
  if (!rows.length) {
    tbody.innerHTML = `<tr><td colspan="${cols.length}" class="na">${
      view === "translation" ? "No gateway serves this pair on this rig." : "No gateways match the current filters."
    }</td></tr>`;
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
  // Translation ingress/egress pickers: populate the six dialects and re-rank on change.
  const xin = document.getElementById("xlate-in");
  const xout = document.getElementById("xlate-out");
  if (xin && xout) {
    const opts = MATRIX_CELLS.map((d) => `<option value="${esc(d)}">${esc(MATRIX_LABELS[d] || d)}</option>`).join("");
    xin.innerHTML = opts; xout.innerHTML = opts;
    const onPick = () => { state.xlateIn = xin.value; state.xlateOut = xout.value; renderTable(); syncUrl(true); };
    xin.addEventListener("change", onPick);
    xout.addEventListener("change", onPick);
  }
}

function renderFilters() {
  document.getElementById("search").value = state.q;
  for (const [, name] of CAPS) { const el = document.getElementById(`f-${name}`); if (el) el.checked = state[CAPS.find(([, n]) => n === name)[0]]; }
  const xin = document.getElementById("xlate-in"); if (xin) xin.value = state.xlateIn;
  const xout = document.getElementById("xlate-out"); if (xout) xout.value = state.xlateOut;
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
  // The gateway's OWN freshness stamp in the drawer head: measured_at + a stale badge when flagged,
  // the same per-gateway signal the table row shows (independent update cadences, made honest).
  const badge = measuredBadge(g);
  let h = `<header class="drawer-head">
    <h3>${g.repo ? `<a href="${g.repo}" target="_blank" rel="noopener">${esc(g.display)}</a>` : esc(g.display)}</h3>
    <div class="chips"><span class="cls-chip">${esc(g.cls || "Gateway")}</span>
    <span class="lang-chip" style="background:${langC}">${esc(g.lang)}</span></div>
    ${badge ? `<div class="drawer-measured">${badge}</div>` : ""}
  </header>`;

  const hw = LANES.map((l) => g[l.key]).find((j) => j && j.hardware);
  if (hw) h += `<p class="stamp muted">${esc(hw.hardware)}${hw.arch ? ` (${esc(hw.arch)})` : ""}</p>`;

  for (const l of LANES) {
    // Canonical lanes (perf, xlate) read the SAME record the table reads via l.get; the raw
    // suite object is only the legacy fallback inside the accessor itself.
    const j = l.get ? l.get(g) : g[l.key];
    h += `<section class="drawer-lane"><h4>${esc(l.label)}</h4>`;
    if (!j) h += `<p class="muted">not measured</p>`;
    else if (j[l.flag] === false) {
      // A multi-line diagnostic (e.g. a captured stack trace) must not dump ~25 raw lines into
      // the drawer: show the first line as the verdict, fold the rest into a collapsed Evidence
      // block, and scrub absolute rig paths (harness noise, not evidence).
      const note = stripRigPaths(j[l.err] || "not served");
      const nl = note.indexOf("\n");
      const head = nl >= 0 ? note.slice(0, nl) : note;
      const rest = nl >= 0 ? note.slice(nl + 1).trim() : "";
      h += `<p class="muted">${esc(head)}</p>`;
      if (rest) h += `<details class="evidence-fold"><summary>Evidence</summary><pre>${esc(rest)}</pre></details>`;
      h += laneStamp(j);
    }
    else {
      if (l.pathNote) h += `<p class="lane-note muted">${esc(l.pathNote(j))}</p>`;
      h += `<dl>` + l.metrics.filter((m) => j[m.k] != null).map((m) => {
        const cc = m.concKey && j[m.concKey] != null && j[m.k] > 0 ? ` (@ c=${fmtInt(j[m.concKey])})` : "";
        return `<div><dt>${esc(m.label)}</dt><dd>${esc(m.fmt(j[m.k]) + cc)}</dd></div>`;
      }).join("") + `</dl>${laneStamp(j)}`;
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
  // ONE source of truth: the drawer curve reads the SAME canonical record the headline rows read
  // (best_cell via canonicalPerf), so the marked peak on the curve IS the published rps_max_proxy /
  // rps_sustained_20ms at its operating concurrency - never a separate perf-suite run.
  const perf = canonicalPerf(g);
  const series = [];
  if (perf && perf.served !== false) {
    series.push({ label: "sustained @20ms", color: "#4cc38a", sweep: perf.sweep_sustained_20ms,
      peak: { rps: perf.rps_sustained_20ms, conc: perf.rps_sustained_20ms_concurrency } });
    series.push({ label: "max proxy", color: "#6cb6ff", sweep: perf.sweep_max_proxy,
      peak: { rps: perf.rps_max_proxy, conc: perf.rps_max_proxy_concurrency } });
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
    /* Canonical lanes read the SAME record the table reads (l.get), so compare can never
       disagree with the table. Skip the whole lane only when no gateway measured it at all;
       an all not-served lane still renders rows so the header is never left bare */
    const recs = gws.map((g) => (l.get ? l.get(g) : g[l.key]));
    if (recs.every((j) => !j)) continue;
    h += `<tr class="lane-row"><td colspan="${gws.length + 1}">${esc(l.label)}</td></tr>`;
    if (l.pathNote) {
      /* one disclosure row per canonical lane: WHICH path each gateway's numbers measured */
      h += `<tr><td class="metric">Measured path</td>` + recs.map((j) =>
        j && j[l.flag] !== false
          ? `<td class="muted lane-note">${esc(l.pathNote(j))}</td>`
          : `<td class="na"></td>`).join("") + `</tr>`;
    }
    for (const m of l.metrics) {
      const vals = recs.map((j) =>
        j && j[l.flag] !== false && j[m.k] != null ? j[m.k] : null);
      const bi = bestIndex(vals, m.best);
      h += `<tr><td class="metric">${esc(m.label)}</td>` + vals.map((v, i) => {
        if (v == null) {
          const j = recs[i];
          const na = j && j[l.flag] !== false ? { text: "n/a", note: "" } : naText(j, l.flag, l.err);
          return `<td class="na" title="${esc(na.note)}">${esc(na.text)}</td>`;
        }
        return `<td class="${i === bi ? "best" : ""}">${esc(m.fmt(v))}</td>`;
      }).join("") + `</tr>`;
    }
  }
  h += `</tbody></table></div>`;
  h += `<p class="fineprint">Best value per row is highlighted, decided by the measurement (lower latency and memory, higher throughput). Sweep overlays below use the sustained @20ms sweep read off the SAME canonical record as the headline rows; every point is a real probe and the marked dot is the published number at its operating concurrency.</p>`;
  h += `<div id="cmp-sweeps" class="sweeps"></div>`;
  document.getElementById("compare-body").innerHTML = h;

  const series = gws.map((g, i) => {
    // Same canonical record as the headline rows (best_cell via canonicalPerf), so the marked peak
    // is the published sustained@20ms at its operating concurrency - not a separate perf-suite run.
    const perf = canonicalPerf(g);
    return {
      label: g.display, color: CMP_COLORS[i],
      sweep: perf && perf.served !== false ? perf.sweep_sustained_20ms : null,
      peak: perf && perf.served !== false
        ? { rps: perf.rps_sustained_20ms, conc: perf.rps_sustained_20ms_concurrency } : null,
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
  const up = g.matrix.upstreams && g.matrix.upstreams[egress];
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
  if (cell.served !== true && cell.served !== "unprobed_auth" && isHarnessGap(cell))
    return `not verified: the harness could not get this gateway serving under this upstream config${cell.verdict_note ? " (" + cell.verdict_note + ")" : ""}`;
  return `${label}. ${cell.verdict_note || ""}`;
}
/* Per-cell perf line for a GREEN cell's tooltip/detail: this path's sustained RPS + added latency
   p99, and its RPS delta vs THIS gateway's REFERENCE cell (the one the Passthrough tab ranks; not
   necessarily the fastest, so it is named, never called "best"). Grey/red/unprobed cells carry no
   perf and return "". */
function cellPerfTip(cell, ingress, egress, best) {
  const p = cell && cell.served === true ? cell.perf : null;
  if (!p || p.rps_sustained_20ms == null) return "";
  let s = `${fmtInt(p.rps_sustained_20ms)} req/s @20ms`;
  if (p.added_latency_p99_us != null) s += `, +${fmtInt(p.added_latency_p99_us)} µs p99 added`;
  if (best && best.rps_sustained_20ms > 0) {
    if (best.ingress === ingress && best.egress === egress) s += " - reference cell (ranks the table)";
    // Human dialect labels (MATRIX_LABELS), never the raw dialect keys, in the hover popup.
    else s += ` - ${fmtPct((p.rps_sustained_20ms / best.rps_sustained_20ms - 1) * 100)} req/s vs the ${MATRIX_LABELS[best.ingress] || best.ingress}→${MATRIX_LABELS[best.egress] || best.egress} cell`;
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
  withMatrix.sort((a, b) => tally(b).pass - tally(a).pass || a.display.localeCompare(b.display));

  const grid = document.getElementById("matrix-grid");
  grid.innerHTML = withMatrix.map((g) => {
    const t = tally(g);
    const bits = [`<b class="pass-count">${t.pass}</b>/36 pass`];
    if (t.fail) bits.push(`${t.fail} fail`);
    if (t.notconf) bits.push(`${t.notconf} not configured`);
    if (t.untestable) bits.push(`${t.untestable} untestable (mock limit)`);
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
  added_latency: "Added latency vs direct-to-mock, p99 in microseconds, concurrency 1, on each gateway's best same-dialect passthrough (the same canonical record the table ranks). Lower is better.",
  rps_sustained_20ms: "Sustained RPS with a 20 ms mock LLM latency (p99 under 1 s, error rate under 0.1 percent), best same-dialect passthrough. Higher is better.",
  rps_max_proxy: "Max proxy RPS against an instant mock, best same-dialect passthrough. Higher is better.",
  memory_rss: "Process RSS in MiB: idle after launch and peak under large-payload load. Lower is better.",
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

/* ---- gateways overview: the neutral roster ----------------------------------
   The landing view is a ROSTER, not a ranking: every gateway in alphabetical
   order (display name, case-insensitive), with its language, a committed star
   snapshot, and its OWN self-description (g.cls). No perf numbers, no winner
   highlighting; the other tabs measure how they perform. busbar gets the exact
   same row treatment as everyone else. */
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

/* The gateway's measured BUILD string (version/tag as run): the first suite record carrying one -
   every suite of a gateway ran the same single-box build, so any lane's stamp is THE stamp. */
const gatewayBuild = (g) => {
  const j = LANES.map((l) => g[l.key]).find((x) => x && x.build);
  return j ? j.build : null;
};
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
   "ghcr.io/x/y:v1.3.1" -> "v1.3.1"; "litellm==1.93.0" -> "1.93.0"; "repo@9649b27..." -> "@9649b27";
   "busbar 1.4.1" -> "1.4.1". Anything unparsable falls back to a truncated string. */
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

/* The newest `measured_at` across ONE gateway's suites: WHEN that gateway was last benchmarked.
   Null if it carries no stamp on any lane. */
function gatewayLastRun(g) {
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
    const name = g.repo
      ? `<a href="${g.repo}" target="_blank" rel="noopener">${esc(g.display)}</a>`
      : esc(g.display);
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
  // Home is the root above the category nav: the header's category row, tab bar
  // and category tagline belong to the category view only, so a body class hides
  // them (style.css) while the home hero carries the brand treatment instead.
  document.body.classList.toggle("home", view === HOME_VIEW);
  // The three perf tabs share one table container (#view-table); matrix/method
  // have their own; home renders #view-home.
  const containerId = PERF_VIEWS.has(view) ? "view-table" : `view-${view}`;
  document.querySelectorAll(".tab").forEach((x) => {
    x.classList.toggle("active", x.dataset.view === view);
    x.setAttribute("href", viewPath(state.category, x.dataset.view));
  });
  document.querySelectorAll(".view").forEach((v) => v.classList.toggle("hidden", v.id !== containerId));
  // The translation ingress/egress pickers only make sense on the Translation tab.
  const picker = document.getElementById("xlate-picker");
  if (picker) picker.classList.toggle("hidden", view !== "translation");
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
    needStream: st.needStream, needXlate: st.needXlate,
    xlateIn: st.xlateIn, xlateOut: st.xlateOut,
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
    fmtStamp, fmtAge, stampWithAge, measuredBadge,
    drawSweep, niceStep, fmtTick, COLUMN_SETS, columnsFor, PERF_VIEWS, VIEW_SORT, LANES, naText, stripRigPaths,
    cellState, matrixCellTip, cellPerfTip, passCell, xlateCell, streamCell, memCell, hasTranslation, CATEGORIES, DEFAULT_CATEGORY, VIEWS,
    canonicalPerf, canonicalXlate, canonicalStreaming, canonicalMemory, cpuFpsCertified, sustainedCertified, gatewayResultsJson, DEFAULT_VIEW, VIEW_LABELS, rosterRows, fmtStars,
    configCorrectionUrl, BENCH_REPO,
    HOME_VIEW, homeCardsHtml,
  };
} else {
  boot();
}
