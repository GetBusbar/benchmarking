#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
"""Render benchmark charts from results/ — pretty, and pluggable.

Nothing is hard-coded: every number is read from results/<suite>/<gateway>.json (written by the
runners). Bars are colored by MEASUREMENT — a neutral highlight goes to whichever gateway measured
best on the metric, so if busbar loses, busbar isn't highlighted. The highlight is deliberately not a
brand color.

Add a chart = append one `Chart(...)` to CHARTS below. Add a gateway = it shows up automatically
once it has a result file (label/order from GATEWAYS). Run after the benchmark:

    python3 charts.py
"""
from __future__ import annotations

import json
import os
import re
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

# When this render happened (UTC). Stamped into every report page + chart footer so a re-run always
# refreshes and re-commits ALL readmes and ALL images, even when the underlying numbers didn't change.
_NOW = datetime.now(timezone.utc)
RENDER_TS = _NOW.strftime("%Y-%m-%d %H:%M UTC")
# Cache-buster appended to every chart <img> URL in the report. GitHub proxies README images through
# its camo cache keyed on the full URL — a stable path serves a STALE png long after the table (plain
# markdown) has updated. A per-render query string changes the URL each time, so the image refreshes
# in lockstep with the numbers. (Costs nothing; the file on disk is unchanged.)
CACHE_BUSTER = _NOW.strftime("%Y%m%d%H%M")
# Absolute base for chart <img>s in the report. Must be the raw.githubusercontent host so GitHub
# camo-proxies the images (a relative repo path is NOT proxied, so its ?v= is ignored and the picture
# goes stale while the table updates). Override IMG_BASE for a fork; defaults to this repo's main.
IMG_BASE = os.environ.get(
    "IMG_BASE", "https://raw.githubusercontent.com/GetBusbar/benchmarking/main/results"
)

# matplotlib is imported lazily (in render) so the report pages can be generated with plain JSON even
# where matplotlib isn't installed. plt is filled in by _mpl().
plt = None

ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results"
SITE_DATA = ROOT / "site" / "data.json"


# ── canonical numbers (single source of truth) ────────────────────────────────────────────────────
# CANONICAL RULE: the matrix per-cell sweep is the single source of truth for all passthrough +
# translation perf; the standalone perf/xlate suites are FALLBACK ONLY. site/gen-data.mjs applies
# that rule once and emits the result as best_cell / translation_cell (with a `source` provenance
# tag) in site/data.json, the SAME bundle the site table reads. charts.py reads those canonical
# records instead of re-deriving numbers from results/perf + results/xlate, so a chart can never
# show a different value (or a different #1) than the table. Streaming (stream/streamcpu) and memory
# are ALSO projected from the matrix now: the standalone stream/streamcpu/memory suites were RETIRED
# (run-all.sh runs ONLY the matrix), so gen-data.mjs projects the matrix's best-diagonal streaming
# into g.streaming and its one process RSS read into g.memory_read. charts.py reads those projected
# fields (via _load_projected → canonicalStreaming/canonicalMemory mirrors) so the streaming/memory
# PNGs match the site's in-browser streaming/memory charts.
# ORDERING: run `node site/gen-data.mjs` BEFORE charts.py (CI: gen-data → charts.py → gen-data,
# the second pass copying the fresh PNGs into site/charts/).
def _canonical() -> dict:
    if not SITE_DATA.exists():
        raise SystemExit(
            "charts.py: site/data.json not found - run `node site/gen-data.mjs` first.\n"
            "  Charts read the canonical per-gateway passthrough/translation numbers from that\n"
            "  bundle (matrix per-cell sweep, perf/xlate-suite fallback) so every surface -\n"
            "  table, drawer, compare, charts - shows the same value."
        )
    data = json.loads(SITE_DATA.read_text(encoding="utf-8"))
    return {g["key"]: g for g in data.get("gateways", [])}


CANON = _canonical()
_PERF_FIELDS = ("added_latency_p50_us", "added_latency_p99_us", "rps_sustained_20ms", "rps_max_proxy")


def _read_result(p: Path) -> dict:
    """Load one results JSON, failing LOUDLY with the offending path.

    A single malformed result file used to crash the whole chart/report pipeline with a
    cryptic json.decoder.JSONDecodeError that named no file. Name the file, and the byte/line
    of the parse error, so the bad result is obvious instead of blocking every gateway's render.
    """
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError) as e:
        raise SystemExit(
            f"charts.py: invalid result file {p.relative_to(ROOT)}: {e}\n"
            f"  fix or remove that file and re-run; the suite's json_escape should never emit invalid JSON."
        )

# ── house style ──────────────────────────────────────────────────────────────────────────────────
BRAND = "#2f6fed"   # winner highlight — a NEUTRAL blue, deliberately not a brand color, so a
                    # highlighted bar can never be misread as "the sponsor won."
BRAND_DK = "#1e5bd8"
SLATE = "#3a3f4b"   # everyone else's primary bar
MUTE = "#9aa2b2"    # secondary/idle bars — mid grey so idle RSS stays readable, not near-invisible
MUTE_TXT = "#2b3140"  # idle-bar value labels: near-ink for clear contrast on white (kept smaller/lighter
                      # weight than the peak label so the hierarchy still reads)
INK = "#1c2430"     # titles
GRAY = "#8a90a0"    # captions
GRID = "#eef0f3"


def _mpl():
    """Import matplotlib on demand; set the house font once. Returns pyplot or None if unavailable."""
    global plt
    if plt is not None:
        return plt
    try:
        import matplotlib
        matplotlib.use("Agg")
        import matplotlib.font_manager as fm
        import matplotlib.pyplot as _plt
    except ImportError:
        return None
    # Inter is BUNDLED in the repo (assets/fonts/) and registered here, so the charts render
    # identically on any machine — a dev laptop, CI, whatever — regardless of what fonts the OS has.
    # (CI runners have neither Inter nor a "medium" weight, so relying on system fonts silently fell
    # back to DejaVu and dropped the medium weight. Registering our own TTFs removes that dependency.)
    fonts_dir = ROOT / "assets" / "fonts"
    have_inter = False
    for ttf in sorted(fonts_dir.glob("Inter-*.ttf")):
        fm.fontManager.addfont(str(ttf))
        have_inter = True
    if have_inter:
        _plt.rcParams["font.family"] = "Inter"
    else:  # no bundled fonts (shouldn't happen in-repo) — fall back to something always present
        for _f in ("Helvetica Neue", "Arial", "DejaVu Sans"):
            if any(_f.lower() in f.name.lower() for f in fm.fontManager.ttflist):
                _plt.rcParams["font.family"] = _f
                break
    _plt.rcParams.update({"axes.edgecolor": "#d7dae0", "svg.fonttype": "none"})
    plt = _plt
    return plt

# ── the field is discovered from the manifests, nothing is hard-coded here ─────────────────────────
# Each gateway is fully defined by its own dir: gateways/<key>/gateway.sh declares GW_DISPLAY (label),
# GW_LANG (color bucket), and GW_REPO (linked from the name in the report table). Add a dir → it shows
# up in the charts/tables/run-lists; delete it → it's gone everywhere. A gateway only appears in a
# chart once it also has a result file this run. Alphabetical by key — deliberately NOT busbar-first;
# order here is only load order, every chart + table sorts by the MEASURED value.
def _manifest_meta():
    out = {}
    for man in sorted((ROOT / "gateways").glob("*/gateway.sh")):
        key = man.parent.name
        txt = man.read_text()

        def grab(var, default=""):
            m = re.search(rf'^{var}=(.*)$', txt, re.M)
            if not m:
                return default
            v = m.group(1).split("#", 1)[0].strip()          # drop trailing comment
            return v.strip('"').strip("'").strip()

        out[key] = {
            "display": grab("GW_DISPLAY", key),
            "lang": grab("GW_LANG", "Other"),
            "repo": grab("GW_REPO"),
        }
    return out

_META = _manifest_meta()
GATEWAYS = {k: v["display"] for k, v in _META.items()}   # key → display label
LANGS = {k: v["lang"] for k, v in _META.items()}         # key → language bucket
REPOS = {k: v["repo"] for k, v in _META.items()}         # key → GitHub URL (may be "")


def _linked(key: str) -> str:
    """Gateway display name for the report table, linked to its GitHub repo when the manifest gives one."""
    name = GATEWAYS.get(key, key)
    repo = REPOS.get(key)
    return f"[{name}]({repo})" if repo else name

# Bars are colored by the gateway's IMPLEMENTATION LANGUAGE — informative (you can see the Rust/Go/
# Python clustering) and neutral (no "winner" highlight for the sponsor; the best is already the top
# bar since rows are sorted). A gateway that didn't serve is drawn grey regardless.
# Five buckets: Rust / Go / Python / Node / Other (Lua/OpenResty, Envoy/C++, … fold into Other).
LANG_ORDER = ["Rust", "Go", "Python", "Node", "Other"]
LANG_COLORS = {
    "Rust": "#c4602d",     # orange
    "Go": "#00a0c6",       # cyan
    "Python": "#3b6ea5",   # blue
    "Node": "#c59b2d",     # amber
    "Other": "#6b7280",    # grey
}
LANG_DEFAULT = "#6b7280"


@dataclass(frozen=True)
class Series:
    field: str            # json key
    legend: str           # legend label
    kind: str = "rank"    # "rank" → green-to-winner/slate-to-rest; or a hex color for a fixed tint


@dataclass(frozen=True)
class Chart:
    name: str             # output png stem
    suite: str            # results/<suite>/*.json
    title: str
    subtitle: str
    unit: str
    series: list          # list[Series]; the FIRST series decides the winner + sort order
    log: bool = False
    higher_better: bool = False   # RPS: bigger wins (green to the max, sort desc)
    money: bool = False           # format bar labels + axis as USD ($0.0015)
    # ── suite-specific behavior (streaming / translation / governance lanes) ──────────────────────
    served_field: str = "served"  # json key that decides "did this gateway do the thing at all"
    not_served_text: str = "✕ did not serve"   # label + legend entry when served_field is false
    not_measured_text: str = ""                 # label for a null (unmeasured) primary metric when null_not_served (falls back to not_served_text)
    zero_text: str = "0  ·  no load held p99 < 1 s"  # served, but the metric came out 0
    clamp_negatives: bool = False  # clamp sub-noise negatives to 0 (footnoted) — never a negative bar
    zero_ok: bool = False          # a clamped/true 0 is a GOOD result (sorts to the winning end)
    # MEDIUM-R3-3: on a zero_ok chart a MEASURED sub-noise <=0 is the winning end, but a NULL primary
    # metric is UNMEASURED (e.g. an unreliable streaming c1 window sets added_ttft/gap to null while
    # stream_served stays true). float(null or 0) would coerce it to a served 0 that ranks #1 as a bold
    # "0 perfect streamer" while the table shows n/a — a single-source divergence. When set, a null
    # primary value is treated as NOT-served on this chart (no bar, out of top-N, "not measured" label),
    # matching how the site table renders the null.
    null_not_served: bool = False
    auto_ms: bool = False          # µs metric: relabel the whole chart in ms once the max is >= 1 ms
    annot: object = None           # optional fn(row) -> str appended after the primary bar label


# Dialect display labels — the SAME branded casing the site uses (MATRIX_LABELS in site/app.js), so a
# PNG bar reads "OpenAI → Anthropic" exactly like the in-browser Translation surfaces, never the raw
# lowercase key ("openai → anthropic"). Unknown dialects fall through to their raw key (audit LOW:
# "anthropic" capitalization consistency).
_DIALECT_LABELS = {
    "openai": "OpenAI", "openai-responses": "OpenAI Responses", "anthropic": "Anthropic",
    "gemini": "Gemini", "cohere": "Cohere", "bedrock": "Bedrock Converse",
}


def _dialect(d):
    return _DIALECT_LABELS.get(d, d) if d else d


# Per-bar provenance note on the canonical passthrough charts: name the dialect when it is not
# the common openai diagonal, and disclose a perf-suite fallback (no matrix per-cell sweep yet).
def _perf_annot(r):
    if r.get("_perf_source") == "perf-fallback":
        return "perf-suite default path"
    d = r.get("_dialect")
    return f"on {_dialect(d)}" if d and d != "openai" else None


CHARTS = [
    # ── the headline: what the system can DO ──────────────────────────────────────────────────────
    # The three passthrough charts read the CANONICAL best_cell numbers (matrix per-cell sweep,
    # via site/data.json), the same record the site's Passthrough table ranks.
    Chart(
        name="added_latency",
        suite="perf",
        title="Added latency - what the gateway costs you",
        subtitle="p99 the gateway adds on top of the upstream, concurrency 1, best same-dialect passthrough (lower is better)",
        unit="µs",
        series=[Series("added_latency_p99_us", "p99 added latency", "rank")],
        log=True,
        annot=_perf_annot,
    ),
    Chart(
        name="rps_max_proxy",
        suite="perf",
        title="Max proxy throughput - raw forwarding speed",
        subtitle="highest sustained req/s with p99 < 1s, <0.1% errors, instant upstream, best same-dialect passthrough (higher is better)",
        unit="requests / sec",
        series=[Series("rps_max_proxy", "max proxy RPS", "rank")],
        higher_better=True,
        # MED-3: gate the bar on the mock-bound honesty flag (rps_max_proxy_valid = >0 AND NOT
        # mock-bound), mirroring the streaming lane (stream_sustained_valid / streamcpu_valid). A
        # rig-limited (mock-bound) throughput must not draw a full bar or rank #1 — it renders "not
        # proven" instead. The site (canonicalPerf) + check-consistency assert the identical rule.
        served_field="rps_max_proxy_valid",
        not_served_text="✕ not measured (rig-limited / needs field run)",
        annot=_perf_annot,
    ),
    Chart(
        name="rps_sustained_20ms",
        suite="perf",
        title="Sustained throughput under 20 ms LLM latency",
        subtitle="req/s held with p99 < 1s + <0.1% errors under a realistic 20 ms model delay, best same-dialect passthrough (higher is better)",
        unit="requests / sec",
        series=[Series("rps_sustained_20ms", "sustained RPS @20ms", "rank")],
        higher_better=True,
        # MED-3: same mock-bound gate as max-proxy above (rps_sustained_20ms_valid).
        served_field="rps_sustained_20ms_valid",
        not_served_text="✕ not measured (rig-limited / needs field run)",
        annot=_perf_annot,
    ),
    # ── supporting: memory (matters at scale) ─────────────────────────────────────────────────────
    Chart(
        name="memory_rss",
        suite="memory",
        title="Gateway RAM under sustained load",
        subtitle="idle vs peak RAM (resident memory) - same box, same mock, same load",
        unit="MiB RAM",
        series=[
            Series("peak_rss_mib", "peak RAM (under load)", "rank"),
            Series("idle_rss_mib", "idle RAM (before load)", MUTE),
        ],
        log=True,
    ),
    # ── cost framing (AIGatewayBench's $/vCPU lens) ───────────────────────────────────────────────
    Chart(
        name="rps_per_dollar",
        suite="perf",
        title="Throughput per dollar",
        subtitle="sustained req/s @20ms per $/hr of the pinned 4-core (m7g.xlarge) slice (higher is better)",
        unit="sustained RPS per $/hr",
        series=[Series("rps_per_dollar", "RPS per $/hr", "rank")],
        higher_better=True,
        # MED-3: the cost lanes derive from the sustained@20ms ceiling, so a rig-limited (mock-bound)
        # sustained number must not draw a cost bar or rank #1 either — gate on the same validity flag.
        served_field="rps_sustained_20ms_valid",
        not_served_text="✕ not measured (rig-limited / needs field run)",
    ),
    Chart(
        name="cost_per_million",
        suite="perf",
        title="Cost per million requests",
        subtitle="$ to serve 1M sustained requests on the pinned 4-core slice (lower is better)",
        unit="$ / 1M requests",
        series=[Series("cost_per_million_usd", "cost / 1M", "rank")],
        money=True,
        # MED-3: derived from the sustained@20ms ceiling — gate on the same mock-bound validity flag.
        served_field="rps_sustained_20ms_valid",
        not_served_text="✕ not measured (rig-limited / needs field run)",
    ),
    # ── streaming: what the gateway costs an SSE stream ───────────────────────────────────────────
    Chart(
        name="stream_added_ttft",
        suite="stream",
        title="Streaming time-to-first-token overhead",
        subtitle="p99 TTFT the gateway adds on top of the mock's paced SSE stream, concurrency 1 (lower is better)",
        unit="µs",
        series=[Series("stream_added_ttft_p99_us", "p99 added TTFT", "rank")],
        log=True,
        served_field="stream_served",
        not_served_text="✕ no SSE streaming",
        not_measured_text="✕ TTFT not measured (unreliable c1 window)",
        clamp_negatives=True,
        zero_ok=True,
        null_not_served=True,
        auto_ms=True,
    ),
    Chart(
        name="stream_added_gap",
        suite="stream",
        title="Streaming per-token overhead",
        subtitle="p99 the gateway adds to each inter-token gap vs direct-to-mock, concurrency 1 (lower is better)",
        unit="µs",
        series=[Series("stream_added_gap_p99_us", "p99 added inter-token gap", "rank")],
        log=True,
        served_field="stream_served",
        not_served_text="✕ no SSE streaming",
        not_measured_text="✕ inter-token gap not measured (unreliable c1 window)",
        clamp_negatives=True,
        zero_ok=True,
        null_not_served=True,
        auto_ms=True,
    ),
    Chart(
        name="stream_sustained",
        suite="stream",
        title="Concurrent SSE streams sustained",
        subtitle="max concurrent streams with 99.9% of frames delivered, no stalls, <0.1% errors (higher is better)",
        unit="concurrent streams",
        series=[Series("stream_sustained_streams", "sustained streams", "rank")],
        higher_better=True,
        # served_field is stream_sustained_valid (streamed AND not mock-bound), mirroring streamcpu_fps
        # below (MEDIUM-R2-2): a rig-limited sustained count is not a valid gateway-vs-ceiling reading, so
        # it renders "not proven" rather than a clean bar. A mock-bound / unverifiable count never draws a
        # full bar or ranks in the top-N — the same discipline the cpu-fps lane already applies.
        served_field="stream_sustained_valid",
        not_served_text="✕ not measured (rig-limited / needs field run)",
        zero_text="0  ·  no stream load qualified",
        annot=lambda r: (lambda f: f"{f:,.0f} frames/s" if f > 0 else None)(
            float(r.get("stream_sustained_fps") or 0)),
    ),
    # ── streaming (CPU-bound): sustained relay throughput under an unpaced firehose ────────────────
    Chart(
        name="streamcpu_fps",
        suite="streamcpu",
        title="Streaming relay throughput (CPU-bound)",
        subtitle="sustained SSE content-frames/sec relayed under an unpaced firehose, gateway pinned (higher is better)",
        unit="frames / sec",
        series=[Series("streamcpu_frames_per_sec", "sustained frames/sec", "rank")],
        higher_better=True,
        # served_field is streamcpu_valid (streamed AND not mock-bound): a mock-bound result is not a
        # valid gateway-vs-ceiling comparison, so it renders as "not proven" rather than a clean bar.
        # On an UNPINNED box every result is mock-bound; only the EC2 field run (real core pinning)
        # yields streamcpu_valid=true, so unproven laptop numbers are never surfaced as a comparison.
        served_field="streamcpu_valid",
        not_served_text="✕ not measured (needs pinned field run)",
        zero_text="0  ·  no stream load qualified",
        annot=lambda r: (lambda f: f"{f:,.0f}/core" if f > 0 else None)(
            float(r.get("streamcpu_fps_per_core") or 0)),
    ),
    # ── translation: the CANONICAL translation cell (matrix per-cell sweep) ───────────────────────
    # Same record the site's Translation surfaces read: OpenAI ingress translated to the gateway's
    # measured egress (named per bar). A gateway with no matrix translation cell falls back to the
    # legacy xlate suite (Anthropic in -> OpenAI out) and the bar says so; direction is never mixed
    # silently across surfaces.
    Chart(
        name="xlate_rps_sustained_20ms",
        suite="xlate",
        title="Cross-protocol translation: throughput",
        subtitle="sustained req/s on each gateway's canonical translation path (direction on the bar), p99 < 1s, <0.1% errors, 20 ms model delay (higher is better)",
        unit="requests / sec",
        series=[Series("xlate_rps_sustained_20ms", "translated RPS @20ms", "rank")],
        higher_better=True,
        # MED-3 (mirrored onto translation): gate on the mock-bound honesty flag
        # (xlate_rps_sustained_20ms_valid = present && >0 && NOT mock-bound), exactly like the
        # passthrough RPS charts (rps_sustained_20ms_valid). A rig-limited translation throughput must
        # not draw a full bar or rank #1 — it renders "not measured (rig-limited)" instead. The site
        # (canonicalXlate / xlateCell) + check-consistency assert the identical rule. A gateway that
        # cannot translate at all has no xlate row (xlate_served absent) and is off the chart entirely.
        served_field="xlate_rps_sustained_20ms_valid",
        not_served_text="✕ not measured (rig-limited / needs field run)",
        annot=lambda r: (f"{_dialect(r.get('_xlate_ingress'))} → {_dialect(r.get('_xlate_egress'))}"
                         + (" (xlate suite)" if r.get("_xlate_source") == "xlate-fallback" else ""))
                        if r.get("_xlate_ingress") else None,
    ),
    Chart(
        name="xlate_added_latency",
        suite="xlate",
        title="Cross-protocol translation: added latency",
        subtitle="p99 added on each gateway's canonical translation path (direction on the bar) vs the egress shape straight to the mock, concurrency 1 (lower is better)",
        unit="µs",
        series=[Series("xlate_added_latency_p99_us", "p99 added latency (translated)", "rank")],
        log=True,
        served_field="xlate_served",
        not_served_text="✕ cannot translate",
        clamp_negatives=True,
        zero_ok=True,
        auto_ms=True,
        annot=lambda r: (f"{_dialect(r.get('_xlate_ingress'))} → {_dialect(r.get('_xlate_egress'))}"
                         + (" (xlate suite)" if r.get("_xlate_source") == "xlate-fallback" else ""))
                        if r.get("_xlate_ingress") else None,
    ),
    # Governance is intentionally NOT charted on the neutral board: the governed suite is a
    # non-default, busbar-only launch (only busbar's manifest wires it), so a comparison would
    # spotlight busbar and read "not tested" for the rest. Governance overhead belongs on the
    # advocacy site. The governed suite still runs and its data is kept for that use.
]


# Cost model: the gateway is pinned to 4 cores = an m7g.xlarge (the class AIGatewayBench costs on).
# us-east-1 on-demand ≈ $0.1632/hr for that slice. Derived per-gateway from the SUSTAINED @20ms ceiling
# (the realistic in-flight capacity), so a gateway that can't sustain load has no cost basis (renders
# as "did not sustain"). Override with GATEWAY_HOURLY_USD.
GATEWAY_HOURLY_USD = 0.1632


# ── projected lanes: streaming / memory now come from the matrix via site/data.json ───────────────
# The harness was consolidated (run-all.sh runs ONLY the matrix; the standalone stream/streamcpu/
# memory suites are RETIRED). gen-data.mjs projects the matrix's best-diagonal streaming into
# g.streaming and its one process RSS read into g.memory_read — the SAME canonical records the site
# reads via canonicalStreaming()/canonicalMemory(). These loaders mirror those two functions so the
# PNGs and the in-browser charts show identical numbers. A gateway with no projected record is simply
# absent from the chart (rendered "not measured"), exactly as the board renders it.
_PROJECTED_SUITES = ("stream", "streamcpu", "memory")


def _proj_streaming(key: str) -> dict | None:
    """canonicalStreaming(g) mirror → a row carrying the chart's legacy stream_*/streamcpu_* keys.

    g.streaming (source:"matrix" or a legacy stream-fallback) carries the matrix-native field names
    (added_ttft_p99_us, added_gap_p99_us, streams_sustained, cpu_fps, …). A present record means the
    gateway streamed, so stream_served is true (matching canonicalStreaming's `stream_served: true`)."""
    s = (CANON.get(key) or {}).get("streaming")
    if not s:
        return None
    # streamcpu validity: the cpu-fps relay number is a valid gateway-vs-ceiling comparison only when
    # it was actually measured (cpu_fps present + positive) AND was NOT mock-bound (an unpinned box is
    # mock-bound → not proven). Mirrors the retired suite's streamcpu_valid = streamed && !mock_bound.
    cpu = s.get("cpu_fps")
    # MEDIUM-5: require the mock-bound flag to be EXPLICITLY False. `cpu_fps_mock_bound=null` means the
    # harness could NOT certify the number (the ceiling probe read 0), so the number is UNVERIFIABLE —
    # not proven. The old `not s.get(...)` treated null as "not mock-bound" (Python `not None` is True),
    # leaking an unverifiable number through as a proven gateway-vs-ceiling comparison. Only a value the
    # harness certified as NOT mock-bound (explicit False) is valid; null/True both suppress the bar
    # (mirrors app.js cpuFpsCertified, which the site + check-consistency use for the identical rule).
    cpu_valid = cpu is not None and float(cpu or 0) > 0 and s.get("cpu_fps_mock_bound") is False
    # MEDIUM-R2-2: streams_sustained gets the SAME mock-bound gate as cpu-fps (above). A sustained count
    # whose bisect saturated near the paced-mock ceiling (streams_sustained_mock_bound=true) is rig-
    # limited, not gateway-limited; a null flag (reference ceiling unread) is unverifiable. Both must
    # suppress the bar so a mock bottleneck never draws a full sustained bar / ranks top-N — exactly the
    # asymmetry the cpu-fps lane already closes. Mirrors app.js sustainedCertified (site + check-
    # consistency use the identical rule); only an explicit False survives.
    sust = s.get("streams_sustained")
    sust_valid = sust is not None and float(sust or 0) > 0 and s.get("streams_sustained_mock_bound") is False
    return {
        # MEDIUM-1(b): carry the cell's ACTUAL stream_served through, never hardcode True. gen-data now
        # only projects g.streaming for a cell that actually streamed (stream_served === true), but a
        # legacy stream-fallback record could still carry stream_served=false; hardcoding True drew a
        # did-not-stream cell as a served streamer. Default True only when the key is absent (a matrix
        # projection with no explicit flag is, by construction, a streamed cell).
        "stream_served": s.get("stream_served", True),
        "stream_added_ttft_p99_us": s.get("added_ttft_p99_us"),
        "stream_added_gap_p99_us": s.get("added_gap_p99_us"),
        "stream_sustained_streams": s.get("streams_sustained"),
        "stream_sustained_fps": s.get("streams_sustained_fps"),
        "stream_sustained_valid": sust_valid,
        "streamcpu_frames_per_sec": cpu,
        # NIT: the matrix never emits cpu_fps_per_core, so this is ALWAYS null today. Kept null-safe (the
        # per-core chart tolerates a null via `float(r.get(...) or 0)`); emitting a real value is a
        # matrix/run.sh change (out of scope here). Left in place so the column reappears automatically
        # once the harness does emit it, rather than silently dropping the plumbing.
        "streamcpu_fps_per_core": s.get("cpu_fps_per_core"),
        "streamcpu_valid": cpu_valid,
    }


def _proj_memory(key: str) -> dict | None:
    """canonicalMemory(g) mirror → a row carrying peak_rss_mib / idle_rss_mib. g.memory_read
    (source:"matrix" or a legacy memory-fallback) is the matrix run's one process-level RSS read."""
    m = (CANON.get(key) or {}).get("memory_read")
    if not m:
        return None
    return {
        "served": True,
        "idle_rss_mib": m.get("idle_rss_mib"),
        "peak_rss_mib": m.get("peak_rss_mib"),
    }


def _load_projected(suite: str) -> list[dict]:
    """Rows for a projected lane (streaming / streamcpu / memory), built from CANON, not results/."""
    rows = []
    for key, label in GATEWAYS.items():
        obj = _proj_memory(key) if suite == "memory" else _proj_streaming(key)
        if obj is None:
            continue
        obj["_key"], obj["_label"] = key, label
        rows.append(obj)
    return rows


def _perf_derived(obj: dict) -> None:
    """Derive the cost lanes from the canonical sustained ceiling (so the cost charts match the table)."""
    sust = float(obj.get("rps_sustained_20ms") or 0)
    # sustained req/s you get per $/hr, and $ per 1M sustained requests. 0 when it can't sustain.
    obj["rps_per_dollar"] = (sust / GATEWAY_HOURLY_USD) if sust > 0 else 0
    obj["cost_per_million_usd"] = (GATEWAY_HOURLY_USD / (sust * 3600) * 1e6) if sust > 0 else 0


def _proj_perf(key: str) -> dict | None:
    """HIGH-1: the passthrough perf chart row, projected from the CANONICAL best_cell (matrix per-cell
    sweep / perf-fallback via site/data.json) — NOT the RETIRED results/perf/<key>.json. Mirrors the
    site's canonicalPerf: a gateway with a best_cell is a served passthrough row; without one it is
    absent from the chart, exactly as the site table ranks it. Reading the retired disk file made the
    first matrix-only gateway (no results/perf file) silently vanish from every passthrough PNG while
    the site table still showed its best_cell — the single-source violation this closes."""
    g = CANON.get(key) or {}
    bc = g.get("best_cell")
    if not bc:
        return None
    obj: dict = {}
    for f in _PERF_FIELDS:
        if bc.get(f) is not None:
            obj[f] = bc[f]
    obj["served"] = True  # best_cell only exists for a served path
    obj["_dialect"] = bc.get("dialect")
    obj["_perf_source"] = bc.get("source")
    # MED-3: carry the mock-bound honesty flags through so the report's rps_cell (⚠) AND the RPS
    # charts' validity gate see them — a rig-limited (mock-bound) throughput must not draw a full bar
    # or rank #1. Also carry build/measured_at (report row provenance) + hardware (from the memory
    # read, since best_cell carries no hardware stamp) so the report header + rows keep their context.
    obj["rps_max_proxy_mock_bound"] = bc.get("rps_max_proxy_mock_bound")
    obj["rps_sustained_20ms_mock_bound"] = bc.get("rps_sustained_20ms_mock_bound")
    # MED-3: per-metric VALIDITY (served_field) for the RPS charts. A POSITIVE throughput that is
    # mock-bound (rig-limited) or unverifiable (mock_bound !== False) is NOT a valid gateway-vs-ceiling
    # reading — it is suppressed (draws no bar, never ranks top-N, shows "not measured (rig-limited)"),
    # mirroring app.js perfRpsSuppressed and the check-consistency RPS visibility assertion. A legitimate
    # measured 0 (served but no tested load held p99 < 1 s) is NOT suppressed: it stays "served" so the
    # chart shows its zero_text ("0 · no load held"), distinct from a rig-ceiling number. So a row is
    # valid (served) unless it is a positive value the harness could not certify as gateway-limited.
    for _m in ("rps_max_proxy", "rps_sustained_20ms"):
        _v = obj.get(_m)
        _suppressed = (_v is not None and float(_v or 0) > 0
                       and bc.get(f"{_m}_mock_bound") is not False)
        obj[f"{_m}_valid"] = (_v is not None and not _suppressed)
    obj["build"] = bc.get("build")
    obj["measured_at"] = bc.get("measured_at")
    mem = g.get("memory_read") or {}
    if mem.get("hardware"):
        obj["hardware"] = mem["hardware"]
    if mem.get("concurrency") is not None:
        obj["concurrency"] = mem["concurrency"]
    if mem.get("payload_bytes") is not None:
        obj["payload_bytes"] = mem["payload_bytes"]
    _perf_derived(obj)
    return obj


def _proj_xlate(key: str) -> dict | None:
    """HIGH-1: the translation chart row, projected from the CANONICAL translation_cell (matrix per-cell
    sweep / xlate-fallback via site/data.json) — NOT the RETIRED results/xlate/<key>.json. Mirrors the
    site's canonicalXlate; a gateway with no translation_cell is absent from the translation charts."""
    tc = (CANON.get(key) or {}).get("translation_cell")
    if not tc:
        return None
    obj: dict = {"xlate_served": True, "xlate_passthrough": False}
    if tc.get("added_latency_p50_us") is not None:
        obj["xlate_added_latency_p50_us"] = tc["added_latency_p50_us"]
    if tc.get("added_latency_p99_us") is not None:
        obj["xlate_added_latency_p99_us"] = tc["added_latency_p99_us"]
    if tc.get("rps_sustained_20ms") is not None:
        obj["xlate_rps_sustained_20ms"] = tc["rps_sustained_20ms"]
    # MED-3 (mirrored onto the translation lane): a rig-limited (mock-bound) translation RPS is NOT a
    # valid gateway-vs-ceiling reading — it must not draw a full bar or rank #1 on the translation chart,
    # exactly as a mock-bound passthrough RPS is suppressed via rps_sustained_20ms_valid. Carry the
    # honesty flag through and emit xlate_rps_sustained_20ms_valid (present && >0 && mock_bound is False).
    # A legitimate measured 0 stays served (chart shows its zero_text), distinct from a rig-ceiling number.
    obj["rps_sustained_20ms_mock_bound"] = tc.get("rps_sustained_20ms_mock_bound")
    _v = obj.get("xlate_rps_sustained_20ms")
    _suppressed = (_v is not None and float(_v or 0) > 0
                   and tc.get("rps_sustained_20ms_mock_bound") is not False)
    obj["xlate_rps_sustained_20ms_valid"] = (_v is not None and not _suppressed)
    obj["_xlate_ingress"] = tc.get("ingress")
    obj["_xlate_egress"] = tc.get("egress")
    obj["_xlate_source"] = tc.get("source")
    return obj


def _load(suite: str) -> list[dict]:
    if suite in _PROJECTED_SUITES:
        return _load_projected(suite)
    # HIGH-1: perf + xlate are projected from CANON (best_cell / translation_cell), NOT read from the
    # RETIRED results/perf|xlate/<key>.json by disk-presence. Enumerate every gateway with a canonical
    # record so a matrix-only gateway (no legacy suite file) appears on the PNG + report exactly as it
    # appears on the site table — one source of truth. A gateway with no canonical record is absent.
    rows = []
    for key, label in GATEWAYS.items():
        obj = _proj_perf(key) if suite == "perf" else _proj_xlate(key) if suite == "xlate" else None
        if obj is None:
            # Any other (non-projected) suite still reads its own results/<suite>/<key>.json.
            if suite in ("perf", "xlate"):
                continue
            p = RESULTS / suite / f"{key}.json"
            if not p.exists():
                continue
            obj = _read_result(p)
        obj["_key"], obj["_label"] = key, label
        rows.append(obj)
    return rows


def _fmt(v: float) -> str:
    if v >= 1000:
        return f"{v/1000:.1f}k" if v < 100000 else f"{v/1000:.0f}k"
    if v <= 0:
        return "0"
    return f"{v:.0f}" if v >= 10 else f"{v:.1f}"


def _topn_keys(chart: Chart, n: int = 5) -> set:
    """The top-N gateway keys for THIS chart, ranked by ITS OWN primary metric, among ONLY the rows
    that have a VALID value for that metric (audit HIGH). A gateway that did not serve the chart's
    metric — did-not-stream, cannot-translate, streamcpu-not-proven — is never eligible for the
    ranking, so it can never appear in a top-N chart it has no valid number for. Each chart therefore
    ranks its own top-N (a latency top-5 no longer leaks a 'cannot translate' gateway into the
    translation top-5)."""
    rows = _load(chart.suite)
    field = chart.series[0].field

    def _served(r) -> bool:
        if not bool(r.get(chart.served_field, True)):
            return False
        # MEDIUM-R3-3: a null primary metric on a null_not_served chart is UNMEASURED, not a served 0.
        if chart.null_not_served and r.get(field) is None:
            return False
        return True

    def _val(r) -> float:
        return float(r.get(field, 0) or 0)

    # Eligible = a valid served measurement. A served 0 counts on a zero_ok chart (sub-noise overhead
    # is the winning end); elsewhere a non-positive metric is not a real value and is not ranked.
    eligible = [r for r in rows if _served(r) and (_val(r) > 0 or chart.zero_ok)]
    eligible.sort(key=lambda r: (-_val(r) if chart.higher_better else _val(r)))
    return {r["_key"] for r in eligible[:n]}


def render(chart: Chart, only_keys=None, out_stem: str | None = None) -> None:
    if _mpl() is None:
        return  # no matplotlib — reports still generate from JSON
    rows = _load(chart.suite)
    if only_keys is not None:  # subset (e.g. top-5): draw just these gateways, to its own PNG
        rows = [r for r in rows if r["_key"] in only_keys]
    if not rows:
        print(f"skip {chart.name}: no results/{chart.suite}/*.json yet")
        return
    primary = chart.series[0].field

    # MEDIUM-R3-3: capture whether the PRIMARY metric is null BEFORE any clamp/auto-ms mutation coerces
    # None→0.0 below, so a null_not_served chart can still tell "unmeasured (null)" from "measured 0".
    if chart.null_not_served:
        for r in rows:
            r["_primary_null"] = r.get(primary) is None

    def _served(r) -> bool:
        if not bool(r.get(chart.served_field, True)):
            return False
        if chart.null_not_served and r.get("_primary_null"):
            return False
        return True

    def _val(r, field=primary) -> float:
        return float(r.get(field, 0) or 0)

    # Suite-specific preprocessing on a working COPY of the rows (never mutate the loaded dicts):
    # clamp sub-noise negatives to 0 (footnoted — never a negative bar), and relabel a µs chart in ms
    # once the biggest value crosses 1 ms so the numbers stay readable.
    unit = chart.unit
    clamped = False
    if chart.clamp_negatives or chart.auto_ms:
        rows = [dict(r) for r in rows]
        fields = [s.field for s in chart.series]
        if chart.clamp_negatives:
            for r in rows:
                for f in fields:
                    if float(r.get(f, 0) or 0) < 0:
                        clamped = True
                        r[f] = 0.0
        if chart.auto_ms and unit == "µs":
            if max((_val(r) for r in rows), default=0.0) >= 1000:
                unit = "ms"
                for r in rows:
                    for f in fields:
                        r[f] = float(r.get(f, 0) or 0) / 1000.0

    # Winner is decided ONLY among gateways that actually served — a gateway that failed under
    # load (or never came up) never colors green, even if a concurrency-1 number looks good.
    served_vals = [_val(r) for r in rows if _served(r) and _val(r) > 0]
    best = (max(served_vals) if chart.higher_better else min(served_vals)) if served_vals else None

    # Sort winners to the top. Broken gateways (did-not-serve, or a non-positive/zero metric) sink
    # to the bottom regardless of metric direction, so a failure never lands at the "best" end —
    # except on a zero_ok chart, where a served 0 is sub-noise overhead, i.e. the winning end.
    def _sortkey(r):
        ok = _served(r) and (_val(r) > 0 or chart.zero_ok)
        if not ok:
            return (1, 0.0)
        return (0, -_val(r) if chart.higher_better else _val(r))
    rows.sort(key=_sortkey)

    # A positive floor for the log axis: negative/zero bars can't be drawn on a log scale, so their
    # labels get anchored here instead of vanishing off-canvas. xmax spans EVERY series so a longer
    # secondary bar (e.g. plain RPS behind governed RPS) never runs off the right edge.
    xmax = max((float(r.get(s.field, 0) or 0) for r in rows for s in chart.series), default=1.0) or 1.0
    pos = [_val(r) for r in rows if _val(r) > 0]
    floor_x = min(pos) if pos else 1.0

    n = len(rows)
    ns = len(chart.series)
    fig, ax = plt.subplots(figsize=(11.5, 0.92 * n + 1.9))
    fig.patch.set_facecolor("white")
    ax.set_facecolor("white")
    group_h = 0.74
    bar_h = group_h / ns
    y0 = list(range(n))

    def _numlab(v: float) -> str:
        # Money → "$0.0015". Time (µs) → the FULL number with commas ("7,807"), never "7.8k" — for
        # latency the exact microseconds read clearest. Everything else → compact ("44k").
        if chart.money:
            return "$0" if v <= 0 else f"${v:,.4g}"
        if unit == "µs":
            return f"{int(round(v)):,}"
        if unit == "ms":  # auto-relabeled µs chart — one decimal keeps 1.2 ms vs 12.0 ms readable
            return f"{v:,.1f}"
        if unit == "concurrent streams":  # a discrete count — "1,024", never "1.0k"
            return f"{int(round(v)):,}"
        return _fmt(v)

    for si, s in enumerate(chart.series):
        offset = group_h / 2 - bar_h / 2 - si * bar_h
        vals = [float(r.get(s.field, 0) or 0) for r in rows]
        rank = s.kind == "rank"
        if rank:
            # colored by implementation language (served); did-not-serve is drawn grey. No winner
            # highlight — the best is already the top bar (rows are sorted).
            colors = [LANG_COLORS.get(LANGS.get(r["_key"], ""), LANG_DEFAULT) if _served(r) else MUTE
                      for r in rows]
        else:
            colors = [s.kind] * n
        # VALIDITY GATE (audit HIGH): a bar is drawn ONLY for a row that is a valid served
        # measurement on THIS chart's metric — the served_field (streamcpu → streamcpu_valid,
        # xlate → xlate_served, streaming → stream_served, …). An invalid/unmeasured row draws
        # ZERO (no visual bar) so the bar matches its "not measured"/"cannot translate" label
        # instead of a full bar off a raw value. On a log axis a bar also can't start at 0, and
        # a negative/zero value can't be drawn at all.
        draw = [v if (_served(r) and (not chart.log or v > 0)) else 0
                for r, v in zip(rows, vals)]
        bars = ax.barh([y + offset for y in y0], draw, height=bar_h * 0.92,
                       color=colors, zorder=3, label=s.legend)
        for r, bar, v in zip(rows, bars, vals):
            served = _served(r)
            # Anchor at the bar's end; when the bar is absent (≤0), pin to the axis floor on a log
            # scale, else to the left edge — so every "0"/"did not serve" note lines up on the left.
            anchor = bar.get_width() if bar.get_width() > 0 else (floor_x if chart.log else 0.0)
            tx = anchor * 1.06 if chart.log else anchor + xmax * 0.012
            cy = bar.get_y() + bar.get_height() / 2
            if rank:
                if served and v > 0:
                    txt, col, weight = _numlab(v), INK, "bold"
                    if chart.annot:  # extra per-bar context (frames/s, governed-vs-plain %)
                        extra = chart.annot(r)
                        if extra:
                            txt = f"{txt}  ·  {extra}"
                elif served and chart.zero_ok:  # sub-noise overhead — a 0 here is the winning end
                    txt, col, weight = "0", INK, "bold"
                elif served:  # came up, but the metric came out 0 (see chart.zero_text for why)
                    txt, col, weight = chart.zero_text, "#c2410c", "bold"
                elif v > 0:   # a number exists, but the gateway failed the suite's serve gate
                    nst = chart.not_served_text
                    if nst == "✕ did not serve":  # the perf suites' historical phrasing
                        nst += " under load"
                    txt, col, weight = f"{_numlab(v)}   {nst}", "#c2410c", "bold"
                elif chart.null_not_served and r.get("_primary_null"):
                    # MEDIUM-R3-3: streamed, but the primary metric is null (unmeasured) — say so, do
                    # NOT draw it as a served 0. Matches the site table's n/a for the same null.
                    txt, col, weight = (chart.not_measured_text or chart.not_served_text), "#c2410c", "bold"
                else:
                    txt, col, weight = chart.not_served_text, "#c2410c", "bold"
                ax.text(tx, cy, txt, va="center", ha="left", fontsize=9.5,
                        fontweight=weight, color=col, zorder=4)
            elif v > 0:  # secondary series (e.g. idle RSS): readable label, skip empty bars
                ax.text(tx, cy, _numlab(v), va="center", ha="left", fontsize=9,
                        fontweight="normal", color=MUTE_TXT, zorder=4)

    ax.set_yticks(y0)
    ax.set_yticklabels([r["_label"] for r in rows], fontsize=11.5, color=INK, fontweight="medium")
    ax.invert_yaxis()
    ax.tick_params(left=False)
    for sp in ("top", "right", "left"):
        ax.spines[sp].set_visible(False)
    ax.spines["bottom"].set_color("#d7dae0")
    if chart.log:
        ax.set_xscale("log")
    ax.xaxis.grid(True, color=GRID, zorder=0)
    ax.set_axisbelow(True)
    # Human tick labels: comma-separated integers on BOTH axes — "1,000 / 10,000" on the log µs/MiB
    # axes (not 10³/10⁴), "10,000 / 20,000" on the linear RPS axis (not 10000). Minor log ticks stay
    # unlabeled so the decade labels don't get crowded.
    from matplotlib.ticker import FuncFormatter, NullFormatter
    if chart.money:
        ax.xaxis.set_major_formatter(FuncFormatter(lambda v, _pos: f"${v:,.4g}" if v > 0 else "$0"))
    elif unit == "ms":  # sub-integer ticks are real on a relabeled ms axis (0.5, 1, 2, …)
        ax.xaxis.set_major_formatter(FuncFormatter(lambda v, _pos: f"{v:,.3g}" if v > 0 else "0"))
    else:
        ax.xaxis.set_major_formatter(FuncFormatter(lambda v, _pos: f"{int(round(v)):,}" if v > 0 else "0"))
    if chart.log:
        ax.xaxis.set_minor_formatter(NullFormatter())
    better = "higher is better" if chart.higher_better else "lower is better"
    ax.set_xlabel(f"{unit}   ·   {better}" + ("   (log scale)" if chart.log else ""),
                  fontsize=9, color=GRAY)
    ax.set_xlim(right=xmax * (2.9 if chart.log else 1.28))

    # Title + subtitle stacked above the axes with real vertical separation (no overlap). Both anchored
    # in POINTS above the axes top (not axes-fraction) so the gap is identical on every chart regardless
    # of its height: subtitle 10 pt up, title 40 pt up → a fixed ~30 pt gap. (Axes-fraction spacing
    # collided once the taller Inter metrics replaced DejaVu — the reported title/subtitle cramping.)
    ax.set_title(chart.title, fontsize=15, fontweight="bold", color=INK, loc="left", pad=40)
    ax.annotate(chart.subtitle, xy=(0, 1), xycoords="axes fraction", xytext=(0, 10),
                textcoords="offset points", fontsize=10.5, color=GRAY, va="bottom", ha="left")

    # Language legend (swatch per language present) + a note for the secondary series (e.g. idle RAM).
    from matplotlib.patches import Patch
    present = [l for l in LANG_ORDER if any(_served(r) and LANGS.get(r["_key"]) == l for r in rows)]
    handles = [Patch(facecolor=LANG_COLORS[l], label=l) for l in present]
    if any(not _served(r) for r in rows):
        handles.append(Patch(facecolor=MUTE, label=chart.not_served_text.lstrip("✕ ").strip()))
    if ns > 1:  # a muted secondary series (idle RAM) — label it too
        handles.append(Patch(facecolor=MUTE, label=chart.series[1].legend))
    if handles:
        # FIXED placement (audit LOW): "best" would drift onto the title/subtitle or the first bar
        # (the streamcpu "colored by language" overlap). Pin to the lower-right corner with padding so
        # the swatch box always lands in the right-edge headroom below the shortest (bottom) bars,
        # clear of the title/subtitle up top and never on top of a bar.
        ax.legend(handles=handles, loc="lower right", fontsize=8.5, frameon=False,
                  ncols=min(len(handles), 6), title="colored by language",
                  borderaxespad=0.8)

    meta = rows[0]
    bits = []
    if "hardware" in meta:
        bits.append(str(meta["hardware"]))
    if "concurrency" in meta and "payload_bytes" in meta:
        bits.append(f"{meta['concurrency']}× {int(meta['payload_bytes'])//1000}KB sustained")
    if clamped:  # a gateway measured faster than direct-to-mock, i.e. inside measurement noise
        bits.append("sub-noise negative differences are shown as 0")
    bits.append("bars colored by implementation language")
    fig.text(0.008, 0.012, "  ·  ".join(bits) + f"     github.com/GetBusbar/benchmarking - regenerated {RENDER_TS} from raw results",
             fontsize=7.3, color=GRAY)

    fig.tight_layout(rect=(0, 0.05, 1, 0.93))
    out = RESULTS / f"{out_stem or chart.name}.png"
    fig.savefig(out, dpi=300, bbox_inches="tight", facecolor="white")
    plt.close(fig)
    print(f"wrote {out}")


def _suite_map(suite: str) -> dict:
    """key → the lane row for every gateway that HAS one. For xlate the row is the CANONICAL
    translation_cell projection (HIGH-1 / NIT-1) — NOT the RETIRED results/xlate/<key>.json — so the
    README translation table enumerates the same matrix-projected gateways the PNGs do. Any other
    suite still reads its own results/<suite>/<key>.json by disk-presence."""
    if suite == "xlate":
        return {k: r for k in GATEWAYS if (r := _proj_xlate(k)) is not None}
    d = RESULTS / suite
    out = {}
    for key in GATEWAYS:
        p = d / f"{key}.json"
        if p.exists():
            out[key] = _read_result(p)
    return out


def _merge() -> dict:
    """One dict per gateway for the README leaderboard: the CANONICAL passthrough perf (best_cell,
    HIGH-1 / NIT-1) merged with the matrix-projected memory read — enumerated from CANON, NOT from
    the RETIRED results/perf/<key>.json by disk-presence. A matrix-only gateway (no legacy perf file)
    therefore appears in the report leaderboard exactly as it appears on the site table."""
    gws: dict = {}
    for key in GATEWAYS:
        perf = _proj_perf(key)
        mem = _proj_memory(key)
        if perf is None and mem is None:
            continue
        obj: dict = {}
        if perf is not None:
            obj.update(perf)
        if mem is not None:
            obj.update(mem)
        gws[key] = obj
    return gws


def _report_md(rows: list, title: str, charts: list, pending: tuple = (), chart_prefix: str = "") -> str:
    """A self-contained result page: machine, table (ranked), charts, provenance."""
    hw = next((r.get("hardware") for _, r in rows if r.get("hardware")), "unknown")
    when = next((r.get("measured_at") for _, r in rows if r.get("measured_at")), "")
    lines = [f"# {title}", ""]
    lines.append(f"**Ran on:** {hw}  ·  {when}")
    lines.append("")
    lines.append("Every number below is regenerated from the raw `results/*.json` - re-run "
                 "`run-all.sh` and this page updates. Passthrough and translation figures are the "
                 "canonical per-gateway records (matrix per-cell sweep, perf/xlate-suite fallback) "
                 "from `site/data.json`, the same values the site table ranks. Chart bars are "
                 "**colored by implementation "
                 "language** (Rust / Go / Python / Node / Other). **Rows are sorted by added latency "
                 "(p99), lowest first.**")
    lines.append("")
    lines.append("| Gateway | Added latency (p99) | Sustained RPS @20ms | Max proxy RPS | Idle RAM | Peak RAM | Built |")
    lines.append("|---|--:|--:|--:|--:|--:|---|")
    mock_bound_seen = False
    zero_load_seen = False
    dnf_seen = False
    fail_notes = []  # (gateway, serve_error) for every ❌ row — the receipt behind "did not serve"

    def rps_cell(val, bound, served):
        # ✕ = never served under load; 0 = served but no tested load held p99<1s + <0.1% errors.
        if served is False:
            return "✕"
        if not val:
            return "0"
        cell = f"{int(val):,}"
        if bound:  # ceiling within 10% of the mock's own — a floor, not a limit
            cell += " ⚠"
        return cell

    for key, r in rows:
        lat = r.get("added_latency_p99_us")
        idle = r.get("idle_rss_mib")
        peak = r.get("peak_rss_mib")
        served = r.get("served", None)
        proxy = rps_cell(r.get("rps_max_proxy"), r.get("rps_max_proxy_mock_bound"), served)
        llm = rps_cell(r.get("rps_sustained_20ms"), r.get("rps_sustained_20ms_mock_bound"), served)
        if r.get("rps_max_proxy_mock_bound") or r.get("rps_sustained_20ms_mock_bound"):
            mock_bound_seen = True
        if served is not False and (not r.get("rps_max_proxy") or not r.get("rps_sustained_20ms")):
            zero_load_seen = True
        # Latency cell: a did-not-serve gateway may still have a concurrency-1 number — flag it † so it
        # is never mistaken for a clean win.
        lat_cell = "-"
        if lat is not None:
            lat_cell = f"{lat:,} µs" + (" †" if served is False else "")
            if served is False:
                dnf_seen = True
        rss = lambda v: f"{v:.0f} MiB" if v is not None else "-"
        if served is False and r.get("serve_error"):
            fail_notes.append((GATEWAYS[key], str(r.get("serve_error"))))
        lines.append(
            f"| {_linked(key)} "
            f"| {lat_cell} "
            f"| {llm} "
            f"| {proxy} "
            f"| {rss(idle)} "
            f"| {rss(peak)} "
            f"| `{(r.get('build') or '').strip()[:46]}` |"
        )
    # Gateways we intend to measure but haven't yet — shown so the field is transparent, never hidden.
    for key in pending:
        lines.append(
            f"| {_linked(key)} | ⏳ *pending* | - | - | - | - | *pending measurement* |"
        )
    lines.append("")
    if pending:
        names = ", ".join(GATEWAYS[k] for k in pending)
        lines.append(f"⏳ **Pending measurement** (a manifest exists; not yet run on the rig): {names}. "
                     "These land here as their runs complete - nothing is hidden.")
        lines.append("")
    lines.append("Two throughput numbers: **max proxy RPS** (instant upstream - raw forwarding speed) "
                 "and **sustained RPS @20ms** (AIGatewayBench's metric - concurrent in-flight capacity "
                 "under realistic LLM latency).")
    legend = []
    zero_or_x = any(True for _, r in rows if r.get("served") is False) or zero_load_seen
    if zero_or_x:
        legend.append("**✕** = did not serve under load (0 successful req/s).")
        legend.append("**0** = came up, but no tested concurrency held p99 < 1 s with <0.1% errors.")
    if dnf_seen:
        legend.append("**†** = a concurrency-1 latency exists, but the gateway failed under load: "
                      "not a clean result.")
    if mock_bound_seen:
        legend.append("**⚠** = ceiling within 10% of the mock's own: treat as a **floor**, not a limit.")
    if pending:
        legend.append("**⏳** = a manifest exists but it hasn't been run on the rig yet.")
    if legend:
        lines.append(" &nbsp; ".join(legend))
        lines.append("")
    # The receipt: WHY each gateway that didn't serve failed — captured status + its own logs, so the
    # claim is evidence, not an assertion.
    if fail_notes:
        lines.append("**Why the ✕ gateways did not serve** (captured live, verbatim from the run):")
        lines.append("")
        for name, err in fail_notes:
            err = err.replace("|", "\\|").strip()
            if len(err) > 300:
                err = err[:300] + "…"
            lines.append(f"- **{name}** - {err}")
        lines.append("")
    # ── the lane suites: streaming / translation / governance ────────────────────────────────────
    # Their own table, built from results/{stream,xlate,governed}/<gateway>.json. A suite that
    # hasn't been run yet simply contributes empty cells; the whole section disappears when none
    # of the three has any result. "cannot" cells ARE the story: a gateway that answers 200 but
    # never frames, or cannot take an Anthropic request, is recorded, not hidden.
    # NIT (charts.py:916): the streaming column must read the SAME source the streaming PNGs use — the
    # matrix projection (g.streaming via _proj_streaming), NOT the RETIRED results/stream/*.json suite.
    # Reading the legacy suite here put weeks-old stale numbers (or ✕ rows for a gateway that no longer
    # has a legacy file) in the README table while the PNGs showed the fresh matrix projection. Build the
    # stream map from _proj_streaming so the table and the charts agree. xlate/governed unchanged
    # (translation is already the canonical matrix cell via _overlay_xlate; governed is retired/absent).
    stream_m = {k: r for k in GATEWAYS if (r := _proj_streaming(k)) is not None}
    # NIT-R3-N2: drop the retired `governed` read. The matrix never produces results/governed/*.json, so
    # governed_m was normally empty; but a stale results/governed/<gw>.json left on a box tree could inject
    # a governance-only gateway as an all-n/a stream row (governance columns aren't rendered here anyway).
    xlate_m = _suite_map("xlate")
    row_keys = [k for k, _ in rows]
    lane_keys = [k for k in row_keys if k in stream_m or k in xlate_m]
    if lane_keys:
        lines.append("## Streaming and translation")
        lines.append("")
        lines.append("Same box, same mock, one gateway at a time. Streaming figures are the overhead "
                     "the gateway adds on top of the mock's paced SSE stream; translation is the "
                     "gateway's canonical translation path (matrix per-cell sweep: OpenAI client in, "
                     "the gateway's measured egress out; direction named per row). A gateway with no "
                     "matrix translation cell falls back to the legacy xlate suite (Anthropic in, "
                     "OpenAI out), marked as such. The conversion is the work being measured.")
        lines.append("")
        lines.append("| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS @20ms |")
        lines.append("|---|--:|--:|--:|--:|")

        def us_cell(r, field):
            v = r.get(field)
            if v is None:
                return "n/a"
            v = max(float(v), 0.0)  # sub-noise negatives read as 0, matching the charts
            return f"{v/1000:,.1f} ms" if v >= 1000 else f"{int(round(v)):,} µs"

        for key in lane_keys:
            s, x = stream_m.get(key), xlate_m.get(key)
            if s is None:
                ttft = gap = streams = "n/a"
            elif not s.get("stream_served"):
                ttft = gap = streams = "✕ no SSE streaming"
            else:
                ttft = us_cell(s, "stream_added_ttft_p99_us")
                gap = us_cell(s, "stream_added_gap_p99_us")
                # MEDIUM-R3-5: gate the sustained count on stream_sustained_valid (streamed AND not
                # mock-bound), matching the stream_sustained PNG (served_field=stream_sustained_valid)
                # and the site drawer. Reading stream_sustained_streams raw would print a concrete count
                # (e.g. "256") for a gateway whose bisect saturated near the paced-mock ceiling — a
                # rig-limited number the chart renders "not measured (rig-limited)" — two published
                # surfaces diverging from the same record.
                if not s.get("stream_sustained_valid"):
                    streams = "✕ not measured (rig-limited)"
                else:
                    streams = f"{int(s.get('stream_sustained_streams') or 0):,}"
                    fps = float(s.get("stream_sustained_fps") or 0)
                    if fps > 0:
                        streams += f" ({fps:,.0f} fps)"
            if x is None:
                xl = "n/a"
            elif x.get("xlate_passthrough"):
                # Returned the upstream body untranslated: a wrong answer to an Anthropic client,
                # distinct from an honest refusal - name it so the two are not conflated.
                xl = "✕ untranslated passthrough"
            elif not x.get("xlate_served"):
                xl = "✕ cannot translate"
            else:
                xl = f"{int(x.get('xlate_rps_sustained_20ms') or 0):,}"
                if x.get("_xlate_ingress"):  # canonical direction, named so no two surfaces mix paths
                    xl += f" ({x['_xlate_ingress']} → {x['_xlate_egress']})"
            lines.append(f"| {_linked(key)} | {ttft} | {gap} | {streams} | {xl} |")
        lines.append("")
        lines.append("**✕** cells are measured refusals, not gaps: the gateway was offered the load "
                     "and could not do the thing (buffered instead of streaming, rejected the "
                     "Anthropic shape, or has no native key/limit governance). **n/a** = that suite "
                     "hasn't been run for this gateway yet.")
        lines.append("")
    for c in charts:
        png = f"{chart_prefix}{c}"  # top5 report points at its own top5_*.png set
        if (RESULTS / f"{png}.png").exists():
            # ABSOLUTE raw URL, not a relative path. GitHub only routes EXTERNAL image URLs through its
            # camo proxy (which honors the ?v= cache-buster); relative same-repo paths are served by a
            # CDN that ignores the query string, so a relative ?v= never actually busts and the stale png
            # keeps showing (the exact symptom: table updates, image doesn't). An absolute raw URL is
            # camo'd → ?v= creates a new cache key each render → the image refreshes with the numbers.
            lines.append(f"![{c}]({IMG_BASE}/{png}.png?v={CACHE_BUSTER})")
            lines.append("")
    lines.append("---")
    lines.append("Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS "
                 "ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors; RSS idle = after "
                 "first 200, peak = under sustained load. Same box, same mock, same load, one gateway "
                 "at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.")
    lines.append("")
    lines.append(f"<sub>Page + charts regenerated **{RENDER_TS}** from the raw `results/*.json`.</sub>")
    return "\n".join(lines) + "\n"


def _ranked() -> list:
    """Ranked by ADDED LATENCY p99, ascending — the table's first column, the headline overhead, and
    lower-is-better, so the table reads intuitively top-down. Served gateways with a real latency sort
    first; a gateway that didn't serve (no clean latency) sinks to the bottom."""
    gws = _merge()
    def key(kv):
        d = kv[1]
        lat = d.get("added_latency_p99_us")
        if d.get("served", True) and lat is not None:
            return (0, lat)
        return (1, float("inf"))
    return sorted(gws.items(), key=key)


def write_reports() -> None:
    ranked = _ranked()
    if not ranked:
        return
    gws = dict(ranked)
    # Known gateways with a manifest but no result yet → listed as "pending measurement" on the all page.
    pending = tuple(k for k in GATEWAYS if k not in gws)
    charts = [c.name for c in CHARTS]
    (RESULTS / "reports" / "all").mkdir(parents=True, exist_ok=True)
    (RESULTS / "reports" / "top5").mkdir(parents=True, exist_ok=True)
    (RESULTS / "reports" / "all" / "README.md").write_text(
        _report_md(ranked, "All gateways — full field", charts, pending=pending))
    # top5 report points at its own top5_*.png charts (rendered in main). The TABLE below is the 5
    # lowest-added-latency gateways; each CHART shows the top 5 by ITS OWN metric among gateways with a
    # valid value for that metric (audit HIGH) — a gateway that cannot do a thing is never ranked into
    # that thing's chart, so a "cannot translate" gateway never appears on the translation top-5.
    (RESULTS / "reports" / "top5" / "README.md").write_text(
        _report_md(ranked[:5], "Top 5 gateways (table: lowest added latency; each chart: top 5 by its own metric)",
                   charts, chart_prefix="top5_"))
    print(f"wrote results/reports/all + top5 ({len(ranked)} gateways)")


def main() -> None:
    RESULTS.mkdir(exist_ok=True)
    any_done = False
    for c in CHARTS:
        render(c)                                       # full field → <name>.png
        # top-5 by THIS chart's OWN metric among rows with a valid value for it (audit HIGH), never a
        # single latency top-5 reused across every metric (which leaked invalid rows into a ranking).
        top5 = _topn_keys(c, 5)
        if top5:
            render(c, only_keys=top5, out_stem=f"top5_{c.name}")   # top-5 only → top5_<name>.png
        any_done = any_done or (RESULTS / f"{c.name}.png").exists()
    write_reports()
    if not any_done:
        print("no charts drawn — run the benchmark first (run-all.sh)")


if __name__ == "__main__":
    main()
