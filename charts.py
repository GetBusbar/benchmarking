#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
"""Render benchmark charts from results/ - pretty, and pluggable.

Nothing is hard-coded: every number is read from results/<suite>/<gateway>.json (written by the
runners). Bars are colored by MEASUREMENT - a neutral highlight goes to whichever gateway measured
best on the metric, so the operator's own entry is highlighted only when it actually wins. The
highlight is deliberately not a brand color.

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
# its camo cache keyed on the full URL - a stable path serves a STALE png long after the table (plain
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
# ALSO come from the matrix now: the standalone stream/streamcpu/memory suites were RETIRED (run-all.sh
# runs ONLY the matrix), so gen-data.mjs projects the matrix's best-diagonal streaming into g.streaming,
# which charts.py reads via the _proj_streaming mirror. MEMORY has no projected record - it is per cell -
# so charts.py reads the per-cell windows directly (_proj_memory) on the SAME cell the site's Same mode
# defaults to, which is what keeps the memory PNGs and the in-browser memory table showing one number.
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


# ---- the sealed-envelope reader (Python mirror of app.js metric() / seal.mjs) ----------------------
# Every metric in data.json is a SEALED ENVELOPE ({value, certified, suppressed, reason?, note?, …});
# the honesty gate lives UPSTREAM at seal time, so a suppressed metric has value:null and the raw number
# is GONE. mval() is the displayable value (None when suppressed/absent); the chart's `*_valid` gates are
# now simply "value is not None" - there is no separate mock-bound flag to re-check, because the envelope
# already dropped the number. This replaces the per-metric `_mock_bound is not False` re-derivation.
def _is_env(x) -> bool:
    return isinstance(x, dict) and isinstance(x.get("certified"), bool)


def mval(env):
    """The bare displayable value of a sealed envelope, or None when suppressed/absent.

    A BARE SCALAR is REJECTED, not tolerated: tolerating one would let charts.py publish a raw ungated
    number if a producer field ever escaped the seal (the exact class C1 exists to prevent), and would
    silently disagree with app.js's metric(), which returns n/a for a non-envelope. Absent (None) is fine
    and reads "not measured"; anything else is a bug, loudly.

    A below_resolution absence displays as 0.0, the same value app.js's metric()/mval render it as: the
    difference ran and came out under what the rig can resolve, which is the winning end of every
    lower-is-better chart, not a hole."""
    if env is None:
        return None
    if not _is_env(env):
        raise SystemExit(
            f"charts.py: refusing to chart an UNSEALED metric value {env!r} (type {type(env).__name__}).\n"
            "  Every metric in site/data.json must be a sealed envelope ({value, certified, suppressed, …}).\n"
            "  A bare scalar means gen-data.mjs did not seal a producer field - fix the seal, not the reader."
        )
    if env.get("value") is None and env.get("reason") == "below_resolution":
        return 0.0
    return env.get("value")


def menote(env):
    """The envelope's note token (e.g. measured_failure / no_qualifying_ceiling), or None."""
    return env.get("note") if _is_env(env) else None


def mreason(env):
    """The envelope's absence-reason token (e.g. below_resolution / not_measured), or None.

    mval() collapses a below_resolution absence into a displayable 0.0, which is correct for the bar
    but loses WHY the value is 0 - and the chart label needs the why, so a sub-resolution 0 can be
    disclosed as such instead of reading like an exact measurement (see _zero_label)."""
    return env.get("reason") if _is_env(env) else None


def mvalid(env) -> bool:
    """A metric draws a bar iff its envelope carries a value (certified, incl. a measured 0), or is a
    below-resolution absence (which displays as 0, see mval)."""
    return _is_env(env) and (env.get("value") is not None or env.get("reason") == "below_resolution")


# ---- provenance-driven captions (Python mirror of app.js SWEEP_CAPTION) -----------------------------
# check-consistency asserts this set's keys match the JS SWEEP_CAPTION (lintCaptionParity) so the two
# caption vocabularies can never drift. Every source label a chart/README emits is keyed by source.sweep.
SWEEP_CAPTION = {
    "6x6-diagonal", "6x6-translation", "6x6-memory-window", "6x6-stream-diagonal",
    "6x6-stream-translation", "perf-suite", "xlate-suite", "stream-suite",
    # Per-cell memory windows (one cold-started process per cell, load run to plateau). The single
    # post-6x6 "6x6-memory-window" above is the LEGACY shape and stays for bundles that carry it.
    "6x6-memory-diagonal", "6x6-memory-translation",
}


def _sweep_label(source: dict | None) -> str:
    """A short provenance suffix for a datum, rendered FROM its source.sweep stamp (never hard-coded)."""
    sweep = (source or {}).get("sweep")
    return {
        "perf-suite": " (perf suite)",
        "xlate-suite": " (translation suite)",
        "stream-suite": " (stream suite)",
    }.get(sweep, "")


def _read_result(p: Path) -> dict:
    """Load one results JSON, failing LOUDLY with the offending path.

    Names the file, and the byte/line of the parse error, so a malformed result is obvious instead of a
    cryptic json.decoder.JSONDecodeError blocking every gateway's render.
    """
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError) as e:
        raise SystemExit(
            f"charts.py: invalid result file {p.relative_to(ROOT)}: {e}\n"
            f"  fix or remove that file and re-run; the suite's json_escape should never emit invalid JSON."
        )

# ── house style ──────────────────────────────────────────────────────────────────────────────────
BRAND = "#2f6fed"   # winner highlight - a NEUTRAL blue, deliberately not a brand color, so a
                    # highlighted bar can never be misread as "the sponsor won."
BRAND_DK = "#1e5bd8"
SLATE = "#3a3f4b"   # everyone else's primary bar
MUTE = "#9aa2b2"    # secondary/idle bars - mid grey so idle RSS stays readable, not near-invisible
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
    # identically on any machine - a dev laptop, CI, whatever - regardless of what fonts the OS has.
    # (CI runners have neither Inter nor a "medium" weight, so relying on system fonts silently fell
    # back to DejaVu and dropped the medium weight. Registering our own TTFs removes that dependency.)
    fonts_dir = ROOT / "assets" / "fonts"
    have_inter = False
    for ttf in sorted(fonts_dir.glob("Inter-*.ttf")):
        fm.fontManager.addfont(str(ttf))
        have_inter = True
    if have_inter:
        _plt.rcParams["font.family"] = "Inter"
    else:  # no bundled fonts (shouldn't happen in-repo) - fall back to something always present
        for _f in ("Helvetica Neue", "Arial", "DejaVu Sans"):
            if any(_f.lower() in f.name.lower() for f in fm.fontManager.ttflist):
                _plt.rcParams["font.family"] = _f
                break
    _plt.rcParams.update({"axes.edgecolor": "#d7dae0", "svg.fonttype": "none"})
    plt = _plt
    return plt

# ── the field is discovered from the manifests, nothing is hard-coded here ─────────────────────────
# Each gateway is fully defined by its own dir: gateways/<key>/definition.json declares display (label),
# lang (color bucket), and repo (linked from the name in the report table). Add a dir → it shows
# up in the charts/tables/run-lists; delete it → it's gone everywhere. A gateway only appears in a
# chart once it also has a result file this run. Alphabetical by key, so no entrant is seated first;
# order here is only load order, every chart + table sorts by the MEASURED value.
def _manifest_meta():
    out = {}
    for man in sorted((ROOT / "gateways").glob("*/definition.json")):
        key = man.parent.name
        try:
            d = json.loads(man.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            d = {}
        out[key] = {
            "display": d.get("display") or key,
            "lang": d.get("lang") or "Other",
            "repo": d.get("repo") or "",
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

# Bars are colored by the gateway's IMPLEMENTATION LANGUAGE - informative (you can see the Rust/Go/
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
    # WHICH METRIC THIS BAR IS, in one or two words, printed beside the bar's own number. Required on
    # every chart that draws more than one bar per gateway: colour there encodes implementation
    # LANGUAGE, so it cannot also say which of the two bars is idle and which is under load. A reader
    # looking at "46" above "42" with no tag has no way to tell the cold sample from the loaded one,
    # and guessing from the sizes is wrong as often as it is right (idle above steady-state is a real
    # result). Unused on a single-series chart, where there is nothing to confuse it with.
    tag: str = ""


@dataclass(frozen=True)
class Chart:
    name: str             # output png stem
    suite: str            # results/<suite>/*.json
    title: str
    subtitle: object          # str, or a callable(rows) -> str (renders from the data)
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
    # THE PRIMARY METRIC IS A DIFFERENCE (gateway leg minus direct-to-mock leg), which is the only
    # shape that could ever come out negative. This flag replaced `clamp_negatives` (ledger TOOL-05).
    #
    # `clamp_negatives=True` used to mean "silently rewrite a negative to 0 and footnote the chart".
    # It made sense when the engine published a raw subtraction: a gateway measuring a hair faster than
    # direct-to-mock is measurement noise, not a negative cost, and a bar pointing backwards would have
    # been a worse lie than a 0. The engine no longer does that. A difference that lands at or below
    # what the rig can resolve is published as a BelowResolution absence, which arrives here as a
    # sealed envelope that mval() renders 0.0 and _zero_label() captions "0 (≤ rig resolution)" - the
    # same 0, but one that says why it is 0 instead of a footnote at the bottom of the image saying
    # some unnamed bar somewhere was clamped.
    #
    # So the clamp had nothing left to clamp, and its footnote had not printed in any published run.
    # Deleting it outright would have been the quiet option: if a negative ever DID arrive again, the
    # bar would draw at 0 (the log-axis guard and `float(v or 0)` both floor it) and rank at the
    # winning end with a bare "0" label - a broken seal contract presented as the best result on the
    # board. The clamp is therefore not deleted but INVERTED: this flag marks the charts where a
    # negative is possible-in-principle, and render() REFUSES to draw one (see
    # `_negative_diff_violations`). What was a silent rewrite is now a gate that fails, which is the
    # only honest way to remove a rewrite nobody was watching.
    diff_metric: bool = False
    zero_ok: bool = False          # a sub-resolution/true 0 is a GOOD result (sorts to the winning end)
    # MEDIUM-R3-3: on a zero_ok chart a MEASURED sub-noise <=0 is the winning end, but a NULL primary
    # metric is UNMEASURED (e.g. an unreliable streaming c1 window sets added_ttft/gap to null while
    # stream_served stays true). float(null or 0) would coerce it to a served 0 that ranks #1 as a bold
    # "0 perfect streamer" while the table shows n/a - a single-source divergence. When set, a null
    # primary value is treated as NOT-served on this chart (no bar, out of top-N, "not measured" label),
    # matching how the site table renders the null.
    null_not_served: bool = False
    auto_ms: bool = False          # µs metric: relabel the whole chart in ms once the max is >= 1 ms
    annot: object = None           # optional fn(row) -> str appended after the primary bar label


# Dialect display labels - the SAME branded casing the site uses (MATRIX_LABELS in site/app.js), so a
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
    # Provenance suffix rendered FROM the sweep stamp (never a hard-coded source string).
    lbl = _sweep_label({"sweep": r.get("_perf_source")})
    if lbl:
        return lbl.strip(" ()")
    d = r.get("_dialect")
    return f"on {_dialect(d)}" if d and d != "openai" else None


# The STREAMING lane's provenance annotation: the same mechanism _perf_annot gives the passthrough
# charts and the xlate annots give the translation charts. Every streaming number is currently a LEGACY
# stream-suite reading (source "stream-suite"), so it must disclose that just like its sibling charts do.
def _stream_annot(r, extra=None):
    lbl = _sweep_label({"sweep": r.get("_stream_source")}).strip(" ()")
    bits = [b for b in (extra, lbl) if b]
    return "  ·  ".join(bits) if bits else None



# AUDIT #10/#14: the memory prose renders FROM the run's own record - the harness makes the windows and
# the fixed-load recipe tunable and emits them (idle_window_s / recovery_window_s / load_recipe /
# protocol), so no chart label may hard-code a duration or a payload. Falls back to the documented field
# default ONLY when nothing in the data states otherwise.
_MEM_WINDOW_DEFAULT_S = 60


def _mem_window(rows, key) -> str:
    for r in rows or []:
        v = r.get(key)
        if v is not None:
            return f"{float(v):g} s"
    return f"{_MEM_WINDOW_DEFAULT_S} s"


def _mem_protocol_line(rows) -> str:
    """The real protocol, in one line, from the data: cold idle -> identical fixed load on the SAME cell
    for every gateway, run until the RSS is steady -> recovery, on a process cold-started for this cell."""
    recipe = next((r.get("_mem_load_recipe") for r in (rows or []) if r.get("_mem_load_recipe")), None)
    load = "an identical fixed load"
    if isinstance(recipe, dict):
        c, pb, d = recipe.get("concurrency"), recipe.get("payload_bytes"), recipe.get("duration_s")
        bits = [f"c={c:g}" if isinstance(c, (int, float)) else None,
                f"{pb:,.0f} B payload" if isinstance(pb, (int, float)) else None,
                f"{d:g} s" if isinstance(d, (int, float)) else None]
        inner = ", ".join(b for b in bits if b)
        if inner:
            load = f"an identical fixed load ({inner})"
    cell = next((r.get("_mem_load_cell") for r in (rows or []) if r.get("_mem_load_cell")), None)
    where = f"on {cell} for every gateway" if cell else "on the same cell for every gateway"
    return (f"process cold-started for this cell: {_mem_window(rows, '_mem_idle_window_s')} cold idle -> "
            f"{load} {where}, run until the RSS is steady -> "
            f"{_mem_window(rows, '_mem_recovery_window_s')} recovery")



def _mem_annot(r):
    """Per-bar memory attribution: WHICH cell this gateway was loaded on, whether the RSS ever went
    steady there, plus any HONESTY DISCLOSURE the producer rode in memory.protocol (payload mismatch,
    failed load - each of which is why an RSS came back NULL). Carrying those without rendering them
    would hide the reason a bar reads "not measured", and hide that a bar has no steady state at all."""
    bits = []
    cell = r.get("_mem_load_cell")
    if r.get("_mem_unserved"):
        return f"does not serve {cell}" if cell else None
    if cell:
        bits.append(f"on {cell}")
    # A cell that never plateaued has NO steady state, so its bar is absent from the ranked series. Say
    # why, and quantify it: at the cap the growth rate is the whole finding.
    #
    # The RATE is the finding; the old wording ("never settled") stacked a verdict on top of it, which
    # reads as the board calling a gateway out rather than reporting what it measured. Signed, so a
    # window that was still RELEASING memory at the cap reads as the negative it is instead of a "+"
    # in front of a minus sign.
    if r.get("_mem_plateaued") is False:
        gr = r.get("_mem_growth_rate_mib_per_min")
        bits.append(f"{gr:+,.1f} MiB/min under load" if isinstance(gr, (int, float)) else "no steady-state RSS")
    proto = r.get("_mem_protocol") or ""
    clauses = [c.strip() for c in proto.split(";")[1:] if c.strip()]
    if clauses:
        bits.append("! " + clauses[0][:60])
    return "  ·  ".join(bits) if bits else None


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
        # Same MEDIUM-R3-3 guard as the translation lane. _proj_perf always reports served=True, so an
        # absent added-latency envelope fell through to the DEFAULT zero_text ("0 · no load held p99 < 1 s")
        # - a THROUGHPUT-failure sentence captioning an unmeasured LATENCY, which states a reason the
        # harness never established. It sank to the bottom rather than the top, so the ranking was not
        # corrupted, but the label was a fabricated explanation.
        not_measured_text="✕ added latency not measured",
        null_not_served=True,
        # A 0 on this chart is the WINNING end, not a failure: lower-is-better, and a below_resolution
        # absence (the difference ran and came out under what the rig can resolve) charts as 0 by
        # design (see mval). Without zero_ok that 0 fell through to the DEFAULT zero_text
        # ("0 · no load held p99 < 1 s") - a THROUGHPUT-failure sentence, in failure orange, captioning
        # a latency WIN. zero_ok renders it in ink at the winning end instead, and the label disclosed
        # by _zero_label reads "0 (≤ rig resolution)" when the envelope's reason says so.
        zero_ok=True,
        # The primary metric is `gateway p99 - direct p99`, so this is a difference chart and gets the
        # negative-difference refusal. It is worth noting it never carried the old `clamp_negatives`
        # while its three siblings (translation + the two streaming latency lanes) all did, which is
        # the flag's real legacy: three charts silently rewrote a negative and the fourth, measuring
        # the same shape of quantity, did not. A refusal that applies to the whole family is one rule
        # to reason about instead of four independent settings nobody was comparing.
        diff_metric=True,
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
        # rig-limited (mock-bound) throughput must not draw a full bar or rank #1 - it renders "not
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
        title="Gateway RAM under a fixed load",
        subtitle=lambda rows: "cold idle vs steady-state RAM, " + _mem_protocol_line(rows),
        annot=_mem_annot,
        unit="MiB RAM",
        series=[
            Series("steady_state_rss_mib", "steady-state RAM (under load)", "rank", tag="steady-state"),
            Series("idle_rss_mib", "idle RAM (cold, before load)", MUTE, tag="idle"),
        ],
        log=True,
        # NULL-SAFE (audit #7/#23): a gateway whose RSS never went steady on this cell has
        # steady_state_rss_mib None → drawn "not measured", never a fabricated served-0 bar and never a
        # peak substituted for a steady state (a peak would describe when the load stopped). The bar
        # carries its growth rate instead (_mem_annot). Same for a gateway that does not
        # serve the cell at all. The secondary (idle) label is likewise suppressed for such a row (see
        # render), so a not-measured gateway shows no idle number either.
        null_not_served=True,
        not_measured_text="✕ no steady state on this cell (or cell not served)",
    ),
    # ── supporting: memory RECOVERY (does it release?) ────────────────────────────────────────────
    # The protocol is the per-cell memory window: a process COLD-STARTED for this cell, a cold-idle
    # sampling window, then the IDENTICAL fixed load on the SAME
    # cell for every gateway run until the RSS is steady (or the cap), then a recovery window (durations +
    # recipe are harness-tunable and travel in the data - every label above renders from them). The level
    # under load is a weak signal on its own; the honest differentiator is whether memory is RELEASED
    # afterward. Rank by recovered_rss_mib (best = min), with the steady state shown muted as the
    # reference. null_not_served gates on the recovery field: a gateway with no recovery reading is drawn
    # "not measured", never a fabricated 0.
    Chart(
        name="memory_recovery",
        suite="memory",
        title="Does the gateway release memory after the load?",
        subtitle=lambda rows: ("recovered RSS at the end of the "
                               f"{_mem_window(rows, '_mem_recovery_window_s')} recovery window vs. its steady state - lower recovery is better"),
        annot=_mem_annot,
        unit="MiB RAM",
        series=[
            Series("recovered_rss_mib", "recovered RAM (60s after load)", "rank", tag="recovered"),
            Series("steady_state_rss_mib", "steady-state RAM (under load)", MUTE, tag="steady-state"),
        ],
        log=True,
        null_not_served=True,
        # THE TEXT MAY NOT NAME A CAUSE THIS CHART CANNOT ESTABLISH. It used to read "needs a
        # recovery-enabled field run", which is one specific explanation out of several and was the
        # wrong one every time a gateway simply did not serve the cell this chart draws: litellm-rust
        # published recovered_rss_mib 255.0 with a 60s recovery window on anthropic>anthropic, and the
        # chart told the reader its run had recovery switched off. The memory table beside it showed
        # the number. Same defect class as a Measurement carrying the wrong `Absent` reason, one layer
        # up in the renderer. Hedged the way the steady-state chart above already hedges.
        not_measured_text="✕ no recovery reading on this cell (or cell not served)",
    ),
    # ── cost framing (AIGatewayBench's $/vCPU lens) ───────────────────────────────────────────────
    Chart(
        name="rps_per_dollar",
        suite="perf",
        title="Throughput per dollar",
        subtitle="sustained req/s (20 ms upstream) per $/hr of the pinned 4-core (m7g.xlarge) slice (higher is better)",
        unit="sustained RPS per $/hr",
        series=[Series("rps_per_dollar", "RPS per $/hr", "rank")],
        higher_better=True,
        # MED-3: the cost lanes derive from the sustained@20ms ceiling, so a rig-limited (mock-bound)
        # sustained number must not draw a cost bar or rank #1 either - gate on the same validity flag.
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
        # MED-3: derived from the sustained@20ms ceiling - gate on the same mock-bound validity flag.
        served_field="rps_sustained_20ms_valid",
        not_served_text="✕ not measured (rig-limited / needs field run)",
        # THE DEFAULT zero_text IS THE THROUGHPUT CHART'S, and it opens with a literal "0" - which on a
        # dollar axis reads as a price of zero, the cheapest possible answer on a lower-is-better chart.
        # This chart's absent rows are gateways whose cost is UNDEFINED because they sustained nothing,
        # so the sentence says that and never shows a number.
        zero_text="no cost per request: no load held p99 < 1 s",
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
        diff_metric=True,
        zero_ok=True,
        null_not_served=True,
        auto_ms=True,
        annot=_stream_annot,   # disclose the streaming lane's provenance
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
        diff_metric=True,
        zero_ok=True,
        null_not_served=True,
        auto_ms=True,
        annot=_stream_annot,   # disclose the streaming lane's provenance
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
        # full bar or ranks in the top-N - the same discipline the cpu-fps lane already applies.
        served_field="stream_sustained_valid",
        not_served_text="✕ not measured (rig-limited / needs field run)",
        # AUDIT #3: a certified 0 is a MEASURED FAILURE (offered stream load, sustained none), and must
        # never read like the unmeasured/rig-limited state above. Name it as the failure it is.
        zero_text="0  ·  MEASURED: sustained no stall-free stream",
        annot=lambda r: _stream_annot(
            r, (lambda f: f"{f:,.0f} frames/s" if f > 0 else None)(float(r.get("stream_sustained_fps") or 0))),
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
        zero_text="0  ·  MEASURED: relayed no qualifying frames",
        annot=lambda r: _stream_annot(
            r, (lambda f: f"{f:,.0f}/core" if f > 0 else None)(float(r.get("streamcpu_fps_per_core") or 0))),
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
        # not draw a full bar or rank #1 - it renders "not measured (rig-limited)" instead. The site
        # (canonicalXlate / xlateCell) + check-consistency assert the identical rule. A gateway that
        # cannot translate at all has no xlate row (xlate_served absent) and is off the chart entirely.
        served_field="xlate_rps_sustained_20ms_valid",
        not_served_text="✕ not measured (rig-limited / needs field run)",
        annot=lambda r: (f"{_dialect(r.get('_xlate_ingress'))} → {_dialect(r.get('_xlate_egress'))}"
                         + _sweep_label({"sweep": r.get("_xlate_source")}))
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
        not_measured_text="✕ translated added latency not measured",
        diff_metric=True,
        zero_ok=True,
        # MEDIUM-R3-3, applied to the translation lane: without null_not_served, `float(r.get(f, 0) or 0)`
        # turns an ABSENT added-latency envelope into a measured 0.0. On a served row with zero_ok that
        # renders a bold "0" and, because lower-is-better, SORTS IT TO THE WINNING END - an unmeasured
        # gateway ranking #1 for adding no latency. The streaming latency lanes above already carry this
        # guard; this lane was missed. Nullness is snapshotted before the coercion, so absent now reads
        # not_measured_text and drops out of the ranking instead of winning it.
        null_not_served=True,
        auto_ms=True,
        annot=lambda r: (f"{_dialect(r.get('_xlate_ingress'))} → {_dialect(r.get('_xlate_egress'))}"
                         + _sweep_label({"sweep": r.get("_xlate_source")}))
                        if r.get("_xlate_ingress") else None,
    ),
    # Governance is intentionally NOT charted on the neutral board: the governed suite is a
    # non-default launch wired by a single manifest, so a comparison would spotlight that one
    # entrant and read "not tested" for the rest. Governance overhead belongs on the
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
# g.streaming - the SAME canonical record the site reads via canonicalStreaming(), so the PNG and the
# in-browser chart show identical numbers. MEMORY has no projected record at all: it is measured PER
# CELL (its own cold-started, plateau-terminated window on every served cell) and there is no
# per-gateway scalar to project, because producing one would mean SELECTING a cell. A chart still has
# to draw one bar per gateway, so it draws the SAME cell for every gateway (the site's Same mode: the
# identity cell most of the field serves), names that cell on the bar, and reads n/a for any gateway
# that does not serve it. Same rule as every other cross-gateway comparison on the board: rank within
# a condition. A gateway with no record for that cell is drawn "not measured", as the board renders it.
_PROJECTED_SUITES = ("stream", "streamcpu", "memory")


def _proj_streaming(key: str) -> dict | None:
    """canonicalStreaming(g) mirror → a row carrying the chart's legacy stream_*/streamcpu_* keys.

    g.streaming (source:"matrix" or a legacy stream-fallback) carries the matrix-native field names
    (added_ttft_p99_us, added_gap_p99_us, streams_sustained, cpu_fps, …). A present record means the
    gateway streamed, so stream_served is true (matching canonicalStreaming's `stream_served: true`)."""
    s = (CANON.get(key) or {}).get("streaming")
    if not s:
        return None
    # Every metric is a SEALED ENVELOPE: the mock-bound gate was applied at seal time, so a rig-limited /
    # unverifiable value is already {value:null,…}. Validity is simply "the envelope carries a value" -
    # there is no separate mock-bound flag to re-check (it was consumed). cpu_fps / streams_sustained are
    # gated (their envelope is null when suppressed); TTFT / gap are ungated latency-shaped envelopes.
    cpu = mval(s.get("cpu_fps"))
    sust = mval(s.get("streams_sustained"))
    row = {
        "stream_served": s.get("stream_served", True),
        "stream_added_ttft_p99_us": mval(s.get("added_ttft_p99_us")),
        "stream_added_gap_p99_us": mval(s.get("added_gap_p99_us")),
        "stream_sustained_streams": sust,
        "stream_sustained_fps": mval(s.get("streams_sustained_fps")),
        "stream_sustained_valid": sust is not None,
        # A measured 0 is a MEASURED FAILURE (the gateway sustained none of the offered stream load),
        # NOT "not measured". The note token carries which; the chart + README render them apart.
        "stream_sustained_note": menote(s.get("streams_sustained")),
        # The streaming lane's PROVENANCE stamp, so the four streaming PNGs disclose their legacy
        # stream-suite source the same way the sibling perf/xlate charts disclose theirs via _sweep_label.
        "_stream_source": (s.get("source") or {}).get("sweep"),
        "streamcpu_frames_per_sec": cpu,
        # cpu_fps_per_core is not emitted today (always null); kept null-safe so the column reappears
        # automatically once the harness emits it. It is not an envelope (plumbing placeholder).
        "streamcpu_fps_per_core": s.get("cpu_fps_per_core"),
        "streamcpu_valid": cpu is not None,
    }
    # Same below_resolution disclosure as _proj_perf: a sub-resolution TTFT/gap charts as 0 (mval)
    # and its label must say so (see _zero_label), not read like an exact measurement.
    for _row_f, _env_f in (("stream_added_ttft_p99_us", "added_ttft_p99_us"),
                           ("stream_added_gap_p99_us", "added_gap_p99_us")):
        if mreason(s.get(_env_f)) == "below_resolution":
            row[f"_{_row_f}_reason"] = "below_resolution"
    return row


def _cell_memory(key: str, ingress: str, egress: str) -> dict | None:
    """The per-cell memory window a gateway's served cell carries, or None. Mirrors app.js
    perCellMemory(): matrix.upstreams[egress].cells[ingress].memory on a cell that actually served."""
    up = ((CANON.get(key) or {}).get("matrix") or {}).get("upstreams") or {}
    cell = ((up.get(egress) or {}).get("cells") or {}).get(ingress)
    if not isinstance(cell, dict) or cell.get("served") is not True:
        return None
    mem = cell.get("memory")
    return mem if isinstance(mem, dict) else None


def _dialects() -> list:
    """Every egress dialect present in the bundle's matrices, sorted. Derived from the data, never a
    hard-coded protocol list: the harness decides what it measured, not this reader."""
    seen = set()
    for g in CANON.values():
        seen.update((((g or {}).get("matrix") or {}).get("upstreams") or {}).keys())
    return sorted(seen)


def _widest_dialect() -> str | None:
    """The identity cell the MOST gateways serve - the site's Same-mode default (app.js widestDialect).
    Derived from the data and tie-broken alphabetically, so no gateway or protocol is ever special-cased
    and the answer is deterministic. None when nothing is served (no data yet)."""
    best, best_n = None, 0
    for d in _dialects():
        n = sum(1 for key in CANON if _cell_memory(key, d, d) is not None)
        if n > best_n or (n == best_n and n > 0 and best and d < best):
            best, best_n = d, n
    return best


def _mem_cell() -> str | None:
    """The comparison cell, derived from CANON on every call. NOT memoised: the answer is a property of
    the bundle currently loaded, and a cached one would survive a CANON swap and quietly describe a
    different run than the bars are drawn from."""
    return _widest_dialect()


def _proj_memory(key: str) -> dict | None:
    """One chart row from THIS gateway's per-cell memory window on the shared comparison cell.

    There is no per-gateway memory record to mirror any more: memory is measured per cell, and the cell
    IS the workload, so a bar drawn from a different cell per gateway would compare different work. Every
    row therefore comes from the SAME cell (the identity cell most of the field serves), and a gateway
    that does not serve it has no row - it renders "not measured", never a substituted cell."""
    d = _mem_cell()
    if not d:
        return None
    g = CANON.get(key) or {}
    if not g.get("matrix"):
        return None          # never measured at all: no row to draw, and none to claim
    m = _cell_memory(key, d, d)
    # THE WINDOW'S OWN VERDICT, not just the cell's capability verdict. A cell can serve perfectly well
    # while its MEMORY window is disclosed as not-served: the producer sets memory.served=false when the
    # fixed load stopped delivering or the declared payload was not the delivered payload, and in that
    # state the load-phase numbers are withheld but idle_rss_mib / recovered_rss_mib / rss_series are
    # still emitted. Charting the idle bar from such a window would show a number the producer has
    # already said not to trust as a cold-idle reading (a relaunch race can leave it sampling the
    # PREVIOUS cell's post-load process). Treat the whole window as absent instead.
    if m and m.get("served") is False:
        m = None
    if not m:
        # MEASURED, but this gateway does not serve the comparison cell. It still gets a ROW, with every
        # number absent: dropping it would delete the single most important fact about a narrow gateway
        # (that it serves 1 of 36 cells) from the one chart where breadth shows. Same rule the board's
        # tables follow - every gateway always appears, unserved reads n/a, never a substituted cell.
        return {"_mem_load_cell": f"{d}>{d}", "_mem_unserved": True,
                "idle_rss_mib": None, "steady_state_rss_mib": None, "recovered_rss_mib": None}
    # RSS metrics are UNGATED sealed envelopes (no mock-bound flag); mval() reads them (None when absent).
    return {
        "served": True,
        # The run's OWN protocol string + window durations + fixed-load recipe travel with the row so
        # every memory label describes the run that happened, never a hard-coded default.
        "_mem_protocol": m.get("protocol"),
        "_mem_idle_window_s": m.get("idle_window_s"),
        "_mem_recovery_window_s": m.get("recovery_window_s"),
        "_mem_load_recipe": m.get("load_recipe"),
        # The cell is not a per-gateway choice any more, so it is the same string on every row. It still
        # travels per row, because the annotation that names it must come from the record it describes.
        "_mem_load_cell": f"{d}>{d}",
        # The plateau verdict rides with the row: a cell that never went steady has NO steady state, and
        # the growth rate is what it has instead. Both are rendered on the bar (_mem_annot).
        "_mem_plateaued": m.get("plateaued"),
        "_mem_growth_rate_mib_per_min": mval(m.get("growth_rate_mib_per_min")),
        "idle_rss_mib": mval(m.get("idle_rss_mib")),
        # The headline is the STEADY STATE, the same quantity the site's memory column ranks: the value
        # the RSS settled at, null when it never settled. Peak-under-load is not it - a peak is bounded by
        # how long the load ran, which is the dependence the plateau termination exists to remove.
        "steady_state_rss_mib": mval(m.get("steady_state_rss_mib")),
        # recovered_rss_mib is absent on pre-recovery bundles → None. The recovery chart gates on it
        # (null_not_served), so such a gateway is shown "not measured", never a fabricated 0 bar.
        "recovered_rss_mib": mval(m.get("recovered_rss_mib")),
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
    # NOT `else 0`. At sust == 0 this quotient is undefined, and 0 is the CHEAPEST value on a
    # lower-is-better chart - so the three gateways that held no load under the p99 gate rendered as
    # free, the best possible result, while ranking last. `rps_per_dollar` above keeps its 0 because
    # zero requests per dollar genuinely IS zero; cost per request of a gateway that served nothing is
    # an absence, and the board's rule is that 0 is a number and n/a is not.
    obj["cost_per_million_usd"] = (GATEWAY_HOURLY_USD / (sust * 3600) * 1e6) if sust > 0 else None


def _proj_perf(key: str) -> dict | None:
    """HIGH-1: the passthrough perf chart row, projected from the CANONICAL best_cell (matrix per-cell
    sweep / perf-fallback via site/data.json) - NOT the RETIRED results/perf/<key>.json. Mirrors the
    site's canonicalPerf: a gateway with a best_cell is a served passthrough row; without one it is
    absent from the chart, exactly as the site table ranks it. Reading the retired disk file made the
    first matrix-only gateway (no results/perf file) silently vanish from every passthrough PNG while
    the site table still showed its best_cell - the single-source violation this closes."""
    g = CANON.get(key) or {}
    bc = g.get("best_cell")
    if not bc:
        return None
    obj: dict = {}
    # Every metric is a SEALED ENVELOPE; mval() reads it (None when suppressed/absent). The gate lives
    # upstream at seal time, so a rig-limited RPS is already null here - there is no _mock_bound flag to
    # re-check (it was consumed). Validity (served_field) is simply "the envelope carries a value".
    for f in _PERF_FIELDS:
        v = mval(bc.get(f))
        if v is not None:
            obj[f] = v
        # A below_resolution absence charts as 0 (mval), but the WHY must ride with the row so the
        # chart can label its 0 "≤ rig resolution" instead of an exact reading (see _zero_label).
        if mreason(bc.get(f)) == "below_resolution":
            obj[f"_{f}_reason"] = "below_resolution"
    obj["served"] = True  # best_cell only exists for a served path
    src = bc.get("source") or {}
    path = bc.get("path") or {}
    obj["_dialect"] = path.get("dialect")
    obj["_perf_source"] = src.get("sweep")   # the provenance stamp key (drives _sweep_label / _perf_annot)
    for _m in ("rps_max_proxy", "rps_sustained_20ms"):
        env = bc.get(_m)
        obj[f"{_m}_valid"] = obj.get(_m) is not None
        # suppressed = the harness could not certify a positive value (rig-limited / unverifiable). The
        # envelope carries this explicitly; the README renders "not measured (rig-limited)" for it, distinct
        # from a measured 0 (value 0, honest) and from an unserved path (no best_cell at all).
        obj[f"{_m}_suppressed"] = bool(_is_env(env) and env.get("suppressed"))
    obj["build"] = src.get("build")
    obj["measured_at"] = src.get("measured_at")
    hw = (g.get("matrix") or {}).get("hardware")
    if hw:
        obj["hardware"] = hw
    _perf_derived(obj)
    return obj


def _proj_xlate(key: str) -> dict | None:
    """HIGH-1: the translation chart row, projected from the CANONICAL translation_cell (matrix per-cell
    sweep / xlate-fallback via site/data.json) - NOT the RETIRED results/xlate/<key>.json. Mirrors the
    site's canonicalXlate; a gateway with no translation_cell is absent from the translation charts."""
    tc = (CANON.get(key) or {}).get("translation_cell")
    if not tc:
        return None
    obj: dict = {"xlate_served": True, "xlate_passthrough": False}
    # Sealed envelopes; mval() reads them. The rps_sustained_20ms envelope is null when suppressed, so
    # the validity gate is simply "the envelope carries a value" (no _mock_bound flag to re-check).
    lat50 = mval(tc.get("added_latency_p50_us"))
    lat99 = mval(tc.get("added_latency_p99_us"))
    rps = mval(tc.get("rps_sustained_20ms"))
    if lat50 is not None:
        obj["xlate_added_latency_p50_us"] = lat50
    if lat99 is not None:
        obj["xlate_added_latency_p99_us"] = lat99
    if rps is not None:
        obj["xlate_rps_sustained_20ms"] = rps
    # Same below_resolution disclosure the passthrough row carries (see _proj_perf / _zero_label).
    if mreason(tc.get("added_latency_p99_us")) == "below_resolution":
        obj["_xlate_added_latency_p99_us_reason"] = "below_resolution"
    obj["xlate_rps_sustained_20ms_valid"] = rps is not None
    path = tc.get("path") or {}
    obj["_xlate_ingress"] = path.get("ingress")
    obj["_xlate_egress"] = path.get("egress")
    obj["_xlate_source"] = (tc.get("source") or {}).get("sweep")
    return obj


def _load(suite: str) -> list[dict]:
    if suite in _PROJECTED_SUITES:
        return _load_projected(suite)
    # HIGH-1: perf + xlate are projected from CANON (best_cell / translation_cell), NOT read from the
    # RETIRED results/perf|xlate/<key>.json by disk-presence. Enumerate every gateway with a canonical
    # record so a matrix-only gateway (no legacy suite file) appears on the PNG + report exactly as it
    # appears on the site table - one source of truth. A gateway with no canonical record is absent.
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


def _below_res(r: dict, field: str) -> bool:
    """Did this row's `field` come out as a below_resolution absence rather than a measured number?

    The projections (_proj_perf / _proj_xlate / _proj_streaming) stash the envelope's reason token
    beside the value as `_<field>_reason`, because mval() has already collapsed the absence into a
    displayable 0.0 and the WHY would otherwise be gone by the time anything renders it. One reader
    for it, so the PNG label and the README cell cannot disagree about the same 0 - which they did:
    the chart said "0 (≤ rig resolution)" and the README table printed a flat "0 µs" for the very
    same envelope, one of them describing a rig limit and the other an exact measurement."""
    return r.get(f"_{field}_reason") == "below_resolution"


def _zero_label(chart: Chart, r: dict) -> str:
    """The winning-end label a served 0 draws on a zero_ok chart.

    A below_resolution absence charts as 0 (see mval) and its projected row carries the reason
    (_<field>_reason, see _below_res); the label discloses that the rig could not resolve the
    difference, so the 0 is never mistaken for an exact reading. A plain measured 0 stays a bare
    "0"."""
    return "0 (≤ rig resolution)" if _below_res(r, chart.series[0].field) else "0"


def _negative_diff_violations(chart: Chart, rows: list) -> list:
    """Every negative value on a difference chart, named. Empty on every chart that is not one.

    THE REPLACEMENT FOR `clamp_negatives` (ledger TOOL-05). The clamp turned a negative into a 0 and
    footnoted the image; the engine stopped producing negatives (a sub-resolution difference is a
    BelowResolution absence now), so the clamp became configuration that described a behavior nobody
    could observe. Deleting it silently would have left a real gap, because the code that ran AFTER it
    still floors negatives by accident - `float(v or 0)` in _val, `v > 0` in the bar-draw gate, the
    log-axis positive filter - so a returning negative would have drawn a 0 bar, sorted to the WINNING
    end of a lower-is-better chart, and captioned itself a bare "0". A broken upstream contract would
    have published as the best result on the board.

    A negative here is not a data point to render politely. It means the gateway leg measured faster
    than the direct leg AND the engine did not classify that as below-resolution, which is a
    contradiction in the producer, not a fact about the gateway. The renderer's correct move is to
    refuse and name the row, so someone fixes the seal or the metric rather than the picture.

    Returned as a list rather than raised in place so a test can assert on it without driving
    matplotlib, and so the message can name every offending row instead of only the first.
    """
    if not chart.diff_metric:
        return []
    out = []
    for s in chart.series:
        for r in rows:
            v = r.get(s.field)
            if isinstance(v, (int, float)) and not isinstance(v, bool) and v < 0:
                out.append(
                    f"charts.py: refusing to draw {chart.name}: {r.get('_key', '?')} carries a "
                    f"NEGATIVE {s.field} ({v}).\n"
                    "  This chart's metric is a DIFFERENCE (gateway leg minus direct leg); the engine\n"
                    "  publishes a sub-resolution difference as a below_resolution absence, never as a\n"
                    "  negative number. A negative means that contract broke upstream - fix the metric\n"
                    "  or the seal. Drawing it would floor it to 0 and rank it FIRST on a\n"
                    "  lower-is-better chart, which is the opposite of what the number says."
                )
    return out


def _topn_keys(chart: Chart, n: int = 5) -> set:
    """The top-N gateway keys for THIS chart, ranked by ITS OWN primary metric, among ONLY the rows
    that have a VALID value for that metric (audit HIGH). A gateway that did not serve the chart's
    metric - did-not-stream, cannot-translate, streamcpu-not-proven - is never eligible for the
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
    rows = _load(chart.suite)
    if only_keys is not None:  # subset (e.g. top-5): draw just these gateways, to its own PNG
        rows = [r for r in rows if r["_key"] in only_keys]

    # THE NEGATIVE-DIFFERENCE REFUSAL (ledger TOOL-05), BEFORE the matplotlib bail and before any
    # coercion can hide one. On a difference chart the engine's contract is that a sub-resolution
    # difference is published as a BelowResolution absence, never as a negative number; a negative
    # arriving here means that contract broke somewhere upstream (engine, seal, or projection), and
    # every downstream step in this function would launder it into a 0 that ranks FIRST on a
    # lower-is-better chart. This is the gate that replaces the old silent clamp, so removing the
    # clamp cannot change a published chart without saying so.
    #
    # Above the `_mpl()` return deliberately: on a box with no matplotlib the PNGs are skipped but the
    # README tables and the top-5 rankings are still written from the same rows, so a gate that only
    # ran when a PNG was being drawn would be off on exactly the machines that publish text.
    for v in _negative_diff_violations(chart, rows):
        raise SystemExit(v)

    if _mpl() is None:
        return  # no matplotlib - reports still generate from JSON
    if not rows:
        print(f"skip {chart.name}: no results/{chart.suite}/*.json yet")
        return
    primary = chart.series[0].field

    # MEDIUM-R3-3: capture whether the PRIMARY metric is null BEFORE the auto-ms mutation coerces
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

    # _measured(r): did this row come up AT ALL, regardless of whether the PRIMARY metric resolved? A
    # null primary is not a null row. On the memory chart the primary is the steady state, which is null
    # for any gateway whose RSS never went steady - and that gateway's cold IDLE RSS was measured
    # perfectly well, before it served a single request. Gating the secondary label on _served() deleted
    # that measured number because a DIFFERENT field on the same record was null, which is
    # "unmeasurable means absent" applied to something that was measured. Under the per-cell design this
    # is not a corner case: on the last field run four of eleven gateways never settled within the load
    # window, so four idle bars would silently vanish from the one chart that shows idle.
    def _measured(r) -> bool:
        return bool(r.get(chart.served_field, True))

    def _val(r, field=primary) -> float:
        return float(r.get(field, 0) or 0)

    # Suite-specific preprocessing on a working COPY of the rows (never mutate the loaded dicts):
    # relabel a µs chart in ms once the biggest value crosses 1 ms so the numbers stay readable.
    unit = chart.unit
    if chart.auto_ms:
        rows = [dict(r) for r in rows]
        fields = [s.field for s in chart.series]
        if unit == "µs":
            if max((_val(r) for r in rows), default=0.0) >= 1000:
                unit = "ms"
                for r in rows:
                    for f in fields:
                        r[f] = float(r.get(f, 0) or 0) / 1000.0

    # Winner is decided ONLY among gateways that actually served - a gateway that failed under
    # load (or never came up) never colors green, even if a concurrency-1 number looks good.
    served_vals = [_val(r) for r in rows if _served(r) and _val(r) > 0]
    best = (max(served_vals) if chart.higher_better else min(served_vals)) if served_vals else None

    # Sort winners to the top. Broken gateways (did-not-serve, or a non-positive/zero metric) sink
    # to the bottom regardless of metric direction, so a failure never lands at the "best" end -
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
        # Money → "$0.0015". Time (µs) → the FULL number with commas ("7,807"), never "7.8k" - for
        # latency the exact microseconds read clearest. Everything else → compact ("44k").
        if chart.money:
            return "$0" if v <= 0 else f"${v:,.4g}"
        if unit == "µs":
            return f"{int(round(v)):,}"
        if unit == "ms":  # auto-relabeled µs chart - one decimal keeps 1.2 ms vs 12.0 ms readable
            return f"{v:,.1f}"
        if unit == "concurrent streams":  # a discrete count - "1,024", never "1.0k"
            return f"{int(round(v)):,}"
        return _fmt(v)

    # WHICH METRIC A BAR IS, printed on the bar itself. A chart with two bars per gateway had nothing
    # anywhere on the image tying either bar to its series: the legend's colours are LANGUAGES, and the
    # only clue was bar order, which a reader cannot recover from the picture. Tagging each number is
    # what survives cropping, greyscale printing and a legend read out of order.
    def _tagged(s: Series, v: float) -> str:
        return f"{_numlab(v)} {s.tag}" if ns > 1 and s.tag else _numlab(v)

    for si, s in enumerate(chart.series):
        offset = group_h / 2 - bar_h / 2 - si * bar_h
        vals = [float(r.get(s.field, 0) or 0) for r in rows]
        rank = s.kind == "rank"
        if rank:
            # colored by implementation language (served); did-not-serve is drawn grey. No winner
            # highlight - the best is already the top bar (rows are sorted).
            colors = [LANG_COLORS.get(LANGS.get(r["_key"], ""), LANG_DEFAULT) if _served(r) else MUTE
                      for r in rows]
        else:
            colors = [s.kind] * n
        # VALIDITY GATE (audit HIGH): a bar is drawn ONLY for a row that is a valid served
        # measurement on THIS chart's metric - the served_field (streamcpu → streamcpu_valid,
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
            # scale, else to the left edge - so every "0"/"did not serve" note lines up on the left.
            anchor = bar.get_width() if bar.get_width() > 0 else (floor_x if chart.log else 0.0)
            tx = anchor * 1.06 if chart.log else anchor + xmax * 0.012
            cy = bar.get_y() + bar.get_height() / 2
            if rank:
                if served and v > 0:
                    txt, col, weight = _tagged(s, v), INK, "bold"
                    if chart.annot:  # extra per-bar context (frames/s, governed-vs-plain %)
                        extra = chart.annot(r)
                        if extra:
                            txt = f"{txt}  ·  {extra}"
                elif served and chart.zero_ok:  # sub-noise overhead - a 0 here is the winning end,
                    # in ink, never the zero_text failure sentence; a below_resolution 0 says so.
                    txt, col, weight = _zero_label(chart, r), INK, "bold"
                elif served:  # came up, but the metric came out 0 (see chart.zero_text for why)
                    txt, col, weight = chart.zero_text, "#c2410c", "bold"
                elif v > 0:   # a number exists, but the gateway failed the suite's serve gate
                    nst = chart.not_served_text
                    if nst == "✕ did not serve":  # the perf suites' historical phrasing
                        nst += " under load"
                    txt, col, weight = f"{_numlab(v)}   {nst}", "#c2410c", "bold"
                elif chart.null_not_served and r.get("_primary_null"):
                    # MEDIUM-R3-3: streamed, but the primary metric is null (unmeasured) - say so, do
                    # NOT draw it as a served 0. Matches the site table's n/a for the same null.
                    txt, col, weight = (chart.not_measured_text or chart.not_served_text), "#c2410c", "bold"
                else:
                    txt, col, weight = chart.not_served_text, "#c2410c", "bold"
                ax.text(tx, cy, txt, va="center", ha="left", fontsize=9.5,
                        fontweight=weight, color=col, zorder=4)
            elif v > 0 and _measured(r):  # secondary series (e.g. idle RSS): readable label, skip empty bars.
                # GATED on the row having COME UP (audit #7/#23): a genuinely not-served row must not
                # show a secondary idle number beside a "did not serve" primary. But a row whose PRIMARY
                # is merely null still measured this series, and deleting a real measurement because a
                # neighbouring field is null is the opposite of the honesty rule it was written for.
                ax.text(tx, cy, _tagged(s, v), va="center", ha="left", fontsize=9,
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
    # Human tick labels: comma-separated integers on BOTH axes - "1,000 / 10,000" on the log µs/MiB
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
    # collided once the taller Inter metrics replaced DejaVu - the reported title/subtitle cramping.)
    ax.set_title(chart.title, fontsize=15, fontweight="bold", color=INK, loc="left", pad=40)
    # AUDIT #10/#14: subtitle may be a callable taking the chart's rows, so a chart whose wording depends
    # on a TUNABLE harness setting (the memory windows) describes the run that happened, not a default.
    _subtitle = chart.subtitle(rows) if callable(chart.subtitle) else chart.subtitle
    ax.annotate(_subtitle, xy=(0, 1), xycoords="axes fraction", xytext=(0, 10),
                textcoords="offset points", fontsize=10.5, color=GRAY, va="bottom", ha="left")

    # The legend band, in inches from the figure bottom. Hoisted out of the `if handles:` block so
    # the tight_layout reservation below is valid even on a chart with no legend at all.
    legend_in = 0.42
    # Language legend (swatch per language present) + a note for the secondary series (e.g. idle RAM).
    from matplotlib.patches import Patch
    present = [l for l in LANG_ORDER if any(_served(r) and LANGS.get(r["_key"]) == l for r in rows)]
    handles = [Patch(facecolor=LANG_COLORS[l], label=l) for l in present]
    if any(not _served(r) for r in rows):
        handles.append(Patch(facecolor=MUTE, label=chart.not_served_text.lstrip("✕ ").strip()))
    # THE SERIES KEY, on its own row above the language legend. It used to be one grey swatch appended
    # to the language legend, under the title "colored by language" - so the chart named the secondary
    # series inside a legend that said colour meant something else, and never named the PRIMARY series
    # at all. Colour cannot carry both meanings: it is spent on language. So the key identifies each
    # series by its POSITION in the group (which is what the reader can actually see) and repeats the
    # per-bar tag that _tagged() prints, and it is kept out of the language legend so neither key
    # borrows the other's title.
    skeys = []
    if ns > 1:
        # POSITION IS COUNTED THE WAY THE READER SEES IT, not the way the series are indexed. Within a
        # group the offset DECREASES with si while the y axis is INVERTED, so series 0 - the ranked,
        # language-coloured one - is drawn at the BOTTOM of its group. A key that called it the upper bar
        # would be worse than no key at all: it would confidently name the wrong bar.
        for si, s in reversed(list(enumerate(chart.series))):
            from_top = ns - si
            where = ("upper bar" if from_top == 1 else "lower bar" if from_top == ns
                     else f"bar {from_top} from the top")
            lab = f'{where}, tagged "{s.tag}": {s.legend}' if s.tag else f"{where}: {s.legend}"
            # The ranked series has no fixed colour to swatch (it is language-coloured, which the
            # legend below states), so its entry is text-only rather than a swatch that would claim
            # one language's colour for the whole series.
            skeys.append(Patch(facecolor="none", edgecolor="none", label=lab) if s.kind == "rank"
                         else Patch(facecolor=s.kind, label=lab))
    if handles or skeys:
        # The legend gets its OWN BAND BELOW THE AXES, never an overlay: no single corner is empty on
        # every chart (a higher-is-better chart's longest bar sits opposite a lower-is-better chart's),
        # so placing it inside the plot always collides with some chart's bars. Anchoring in FIGURE
        # coordinates below the axes is correct for any sort direction, any bar count and any series
        # count. The offset is computed from the figure height so the gap is a CONSTANT number of
        # inches rather than shrinking as charts get taller.
        fig_h = 0.92 * n + 1.9    # must match the figsize above
        if handles:
            fig.legend(handles=handles, loc="lower center", bbox_to_anchor=(0.5, legend_in / fig_h),
                       fontsize=8.5, frameon=False, ncols=min(len(handles), 6),
                       title="colored by language")
        if skeys:
            # A SECOND figure legend, anchored a constant 0.5 in above the language band (the same
            # inches-not-fractions rule the band itself uses, so the gap holds on a tall chart). One
            # row: these labels are sentences, and wrapping them into a column would read as a list of
            # unrelated notes rather than "top bar is this, bottom bar is that".
            fig.legend(handles=skeys, loc="lower center",
                       bbox_to_anchor=(0.5, (legend_in + 0.5) / fig_h),
                       fontsize=8.5, frameon=False, ncols=len(skeys),
                       title=f"{ns} bars per gateway", handlelength=1.4)

    meta = rows[0]
    bits = []
    if "hardware" in meta:
        bits.append(str(meta["hardware"]))
    if "concurrency" in meta and "payload_bytes" in meta:
        bits.append(f"{meta['concurrency']}× {int(meta['payload_bytes'])//1000}KB sustained")
    # The "sub-noise negative differences are shown as 0" footnote used to live here, driven by the
    # clamp. It went out with the clamp (ledger TOOL-05): it could only ever have printed on a chart
    # that had already rewritten a number, and there is no longer a rewrite to disclose. The
    # disclosure that replaced it is per-bar and precise - "0 (≤ rig resolution)" on the bar whose
    # difference the rig could not resolve - rather than one sentence at the bottom of the image
    # about some unnamed bar.
    bits.append("bars colored by implementation language")
    fig.text(0.008, 0.012, "  ·  ".join(bits) + f"     github.com/GetBusbar/benchmarking - regenerated {RENDER_TS} from raw results",
             fontsize=7.3, color=GRAY)

    # Reserve the band the out-of-axes legend now occupies (audit #21), in the SAME constant-inches
    # terms it is anchored in, so the axes never grow down into it on a tall chart. The series key sits
    # 0.5 in above the language band, so a multi-series chart must reserve that much more or the axes
    # grow down over the one label that says which bar is which.
    band_in = legend_in + 0.55 + (0.5 if ns > 1 else 0.0)
    fig.tight_layout(rect=(0, max(0.05, band_in / (0.92 * n + 1.9)), 1, 0.93))
    out = RESULTS / f"{out_stem or chart.name}.png"
    fig.savefig(out, dpi=300, bbox_inches="tight", facecolor="white")
    plt.close(fig)
    print(f"wrote {out}")


def _suite_map(suite: str) -> dict:
    """key → the lane row for every gateway that HAS one. For xlate the row is the CANONICAL
    translation_cell projection (HIGH-1 / NIT-1) - NOT the RETIRED results/xlate/<key>.json - so the
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
    HIGH-1 / NIT-1) merged with the matrix-projected memory read - enumerated from CANON, NOT from
    the RETIRED results/perf/<key>.json by disk-presence. A matrix-only gateway (no legacy perf file)
    therefore appears in the report leaderboard exactly as it appears on the site table."""
    gws: dict = {}
    for key in GATEWAYS:
        perf = _proj_perf(key)
        mem = _proj_memory(key)
        # The all-null placeholder row (measured, but does not serve the comparison cell) exists so the
        # memory CHART can show an n/a bar. It carries no numbers, so it must not by itself conjure a
        # leaderboard row for a gateway with nothing else to report.
        if mem is not None and mem.get("_mem_unserved"):
            mem = None
        if perf is None and mem is None:
            continue
        obj: dict = {}
        if perf is not None:
            obj.update(perf)
        if mem is not None:
            # `served` is the PERF verdict in this row's vocabulary (it drives the RPS cells' ✕). The
            # memory lane must not overwrite it in either direction: a gateway that served its
            # passthrough but has no window on the comparison cell would otherwise be reported as not
            # having served at all, and the reverse would claim a serve the perf sweep never saw.
            obj.update({k: v for k, v in mem.items() if k != "served"})
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
    lines.append("| Gateway | Added latency (p99) | Sustained RPS (20 ms upstream) | Max proxy RPS | Idle RAM | Steady-state RAM | Built |")
    lines.append("|---|--:|--:|--:|--:|--:|---|")
    mock_bound_seen = False
    zero_load_seen = False
    dnf_seen = False
    fail_notes = []  # (gateway, serve_error) for every ❌ row - the receipt behind "did not serve"

    def rps_cell(val, suppressed, served):
        # ✕ = never served under load; ⚠ rig-limited = the sealed envelope suppressed a positive value the
        # harness could not certify as gateway-limited; 0 = served but no tested load held p99<1s.
        if served is False:
            return "✕"
        # NEITHER SERVED NOR NOT-SERVED. `served` is None on a row assembled from a memory record with no
        # perf record beside it, and that fell through to `not val` and printed a bare "0" - a measured
        # zero for a gateway whose throughput was never measured at all. The three states this column
        # can be in are "served and this is the number", "served and nothing held the gate" (a real 0),
        # and "there is no perf record here"; only the middle one is a zero.
        if served is None:
            return "-"
        if suppressed:
            return "⚠ rig-limited"
        if not val:
            return "0"
        return f"{int(val):,}"

    def rss_cell(v):
        return f"{v:.0f} MiB" if v is not None else "-"

    for key, r in rows:
        lat = r.get("added_latency_p99_us")
        idle = r.get("idle_rss_mib")
        # The published RAM-under-load number is the STEADY STATE, the same quantity the site ranks. A
        # gateway that never settled has none, and the table reads "-" for it rather than substituting a
        # peak (which would report when the load stopped, not what the gateway holds).
        peak = r.get("steady_state_rss_mib")
        served = r.get("served", None)
        proxy = rps_cell(r.get("rps_max_proxy"), r.get("rps_max_proxy_suppressed"), served)
        llm = rps_cell(r.get("rps_sustained_20ms"), r.get("rps_sustained_20ms_suppressed"), served)
        if r.get("rps_max_proxy_suppressed") or r.get("rps_sustained_20ms_suppressed"):
            mock_bound_seen = True
        if served is not False and r.get("rps_max_proxy") == 0:
            zero_load_seen = True
        # Latency cell: a did-not-serve gateway may still have a concurrency-1 number - flag it † so it
        # is never mistaken for a clean win.
        lat_cell = "-"
        if lat is not None:
            # A below_resolution added latency charts as 0 and labels itself "0 (≤ rig resolution)" on
            # the PNG; printing "0 µs" here would state an exact measurement the rig never made, and
            # would read as a stronger claim than the chart drawn from the same envelope. The two
            # surfaces say the same thing (see _below_res).
            shown = "≤ rig resolution" if _below_res(r, "added_latency_p99_us") else f"{lat:,} µs"
            lat_cell = shown + (" †" if served is False else "")
            if served is False:
                dnf_seen = True
        if served is False and r.get("serve_error"):
            fail_notes.append((GATEWAYS[key], str(r.get("serve_error"))))
        lines.append(
            f"| {_linked(key)} "
            f"| {lat_cell} "
            f"| {llm} "
            f"| {proxy} "
            f"| {rss_cell(idle)} "
            f"| {rss_cell(peak)} "
            f"| `{(r.get('build') or '').strip()[:46]}` |"
        )
    # Gateways we intend to measure but haven't yet - shown so the field is transparent, never hidden.
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
                 "and **sustained RPS under a 20 ms upstream delay** (AIGatewayBench's metric - concurrent in-flight capacity "
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
        legend.append("**⚠ rig-limited** = a positive throughput the harness could not certify as "
                      "gateway-limited (rig / mock-bound); suppressed, not shown as a number.")
    if pending:
        legend.append("**⏳** = a manifest exists but it hasn't been run on the rig yet.")
    if legend:
        lines.append(" &nbsp; ".join(legend))
        lines.append("")
    # The receipt: WHY each gateway that didn't serve failed - captured status + its own logs, so the
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
    # The streaming column must read the SAME source the streaming PNGs use, the matrix projection
    # (g.streaming via _proj_streaming), never the RETIRED results/stream/*.json suite, so the table and
    # the charts agree. xlate is already the canonical matrix cell via _overlay_xlate; `governed` is
    # retired and deliberately not read here at all (a stale results/governed/*.json on disk must not
    # inject a governance-only gateway as an all-n/a stream row).
    stream_m = {k: r for k in GATEWAYS if (r := _proj_streaming(k)) is not None}
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
        lines.append("| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams | Translated RPS (20 ms upstream) |")
        lines.append("|---|--:|--:|--:|--:|")

        def us_cell(r, field):
            v = r.get(field)
            if v is None:
                return "n/a"
            # A below_resolution absence charts as 0 and the PNG labels it "0 (≤ rig resolution)"; the
            # table said "0 µs", which reads as an exact measurement of no overhead. Same envelope,
            # two published surfaces, two different claims about what the rig could see. Say the same
            # thing here (ledger TOOL-05 / the below-resolution work it belongs to).
            if _below_res(r, field):
                return "≤ rig resolution"
            v = float(v)
            # The `max(v, 0.0)` that used to sit here was the report's copy of the retired
            # clamp_negatives, and it laundered the same negative the charts now refuse to draw. There
            # is nothing left to clamp, and a negative difference is a broken producer contract, not a
            # cell to round up - so it is printed as measured and left visibly wrong.
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
                # (e.g. "256") for a gateway whose bisect saturated near the paced-mock ceiling - a
                # rig-limited number the chart renders "not measured (rig-limited)" - two published
                # surfaces diverging from the same record.
                if not s.get("stream_sustained_valid"):
                    streams = "✕ not measured (rig-limited)"
                elif int(s.get("stream_sustained_streams") or 0) == 0:
                    # AUDIT #3: a MEASURED FAILURE. The gateway was offered stream load and sustained
                    # none of it - publishing that as "not measured (rig-limited)" (the branch above)
                    # would flatter it by hiding a real, measured failure behind a rig excuse.
                    streams = "✕ 0 - MEASURED: sustained no stall-free stream"
                else:
                    streams = f"{int(s.get('stream_sustained_streams') or 0):,}"
                    fps = float(s.get("stream_sustained_fps") or 0)
                    if fps > 0:
                        streams += f" ({fps:,.0f} fps)"
                # AUDIT #2: disclose the streaming lane's provenance in the table, exactly as the PNGs do.
                streams += _sweep_label({"sweep": s.get("_stream_source")})
            if x is None:
                xl = "n/a"
            elif x.get("xlate_passthrough"):
                # Returned the upstream body untranslated: a wrong answer to an Anthropic client,
                # distinct from an honest refusal - name it so the two are not conflated.
                xl = "✕ untranslated passthrough"
            elif not x.get("xlate_served"):
                xl = "✕ cannot translate"
            elif not x.get("xlate_rps_sustained_20ms_valid"):
                # Gate the translation RPS on the mock-bound honesty flag (xlate_rps_sustained_20ms_valid),
                # exactly as the SSE-streams column above gates on stream_sustained_valid and the
                # translation PNG gates on served_field, so a rig-limited (mock-bound) number is never
                # printed as if it were a real reading. A legitimate measured 0 stays valid (shows "0").
                xl = "✕ not measured (rig-limited)"
                if x.get("_xlate_ingress"):
                    xl += f" ({x['_xlate_ingress']} → {x['_xlate_egress']})"
                # Render the legacy-xlate-suite fallback marker from the source stamp, like every PNG.
                xl += _sweep_label({"sweep": x.get("_xlate_source")})
            else:
                xl = f"{int(x.get('xlate_rps_sustained_20ms') or 0):,}"
                if x.get("_xlate_ingress"):  # canonical direction, named so no two surfaces mix paths
                    xl += f" ({x['_xlate_ingress']} → {x['_xlate_egress']})"
                xl += _sweep_label({"sweep": x.get("_xlate_source")})
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
                 "at a time. Each gateway's source ref is pinned in its own `gateways/<name>/definition.json`; "
                 "the built commit is in each row.")
    lines.append("")
    lines.append(f"<sub>Page + charts regenerated **{RENDER_TS}** from the raw `results/*.json`.</sub>")
    return "\n".join(lines) + "\n"


def _ranked() -> list:
    """Ranked by ADDED LATENCY p99, ascending - the table's first column, the headline overhead, and
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
        _report_md(ranked, "All gateways - full field", charts, pending=pending), encoding="utf-8")
    # top5 report points at its own top5_*.png charts (rendered in main). The TABLE below is the 5
    # lowest-added-latency gateways; each CHART shows the top 5 by ITS OWN metric among gateways with a
    # valid value for that metric (audit HIGH) - a gateway that cannot do a thing is never ranked into
    # that thing's chart, so a "cannot translate" gateway never appears on the translation top-5.
    (RESULTS / "reports" / "top5" / "README.md").write_text(
        _report_md(ranked[:5], "Top 5 gateways (table: lowest added latency; each chart: top 5 by its own metric)",
                   charts, chart_prefix="top5_"), encoding="utf-8")
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
        print("no charts drawn - run the benchmark first (run-all.sh)")


if __name__ == "__main__":
    main()
