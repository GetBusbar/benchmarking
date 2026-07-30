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
# show a different value (or a different #1) than the table. Streaming and memory
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
    # The bundle's own timestamp travels with it: when the staleness guard fires, the first thing the
    # operator needs is WHEN this snapshot of the board was taken, so the message can be acted on
    # without a second command to go find out.
    return {g["key"]: g for g in data.get("gateways", [])}, data.get("generated_at")


CANON, CANON_GENERATED_AT = _canonical()
# THE SCALAR-THROUGHPUT FIELDS ARE GONE FROM THIS LIST, and from the producer.
#
# It used to read (..., "rps_sustained_20ms", "rps_max_proxy"). Both came off the SAME concurrency sweep
# and each collapsed it to one number (engine/src/frontier.rs, module header): the gate that decided which
# rungs counted was a chosen 20 ms, the two algorithms reading one dataset produced an IMPOSSIBLE pair
# (aisix openai-responses>anthropic published a "maximum" of 16,232 BELOW its own sustained 16,610; bifrost
# openai-responses>openai-responses 5,113 below 5,174), and a scalar cannot express a tradeoff that is a
# curve. They are replaced by `frontier` - the same sweep, read at each declared tail-latency bound - which
# is a per-reading structure rather than a flat metric field, so it is projected by _proj_frontier below
# instead of by this loop.
_PERF_FIELDS = ("added_latency_p50_us", "added_latency_p99_us")


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


# WHOSE FAULT THE ABSENCE IS, ACCORDING TO THE RECORD - not according to the chart.
#
# Every `not_served_text` on the streaming and cost charts reads "rig-limited / needs field run", and
# that is asserted for EVERY absence on those charts. The artifacts disagree. On the 2026-07-29 board
# `stream_sustained` captioned five gateways that way while their own sealed details said:
#   helicone / litellm-rust: "the bisection proved c=1032, but that concurrency did not hold the stream
#                             gate on re-measurement" - the GATEWAY failing a re-measurement
#   bifrost / gomodel:       "... whether this was rig-bound is unknown"
#   apisix:                  "the rig reference ceiling was not measurable"
# So the chart handed two gateways an excuse the data does not support and asserted a cause for three
# more that the engine explicitly calls unknown. The rig-vs-gateway line is the one this whole project
# refuses to blur, and a blanket caption blurs it in both directions at once.
#
# The reasons below are the engine's own `Absent` tokens. `rig_limited` is the ONLY one that means our
# equipment bounded the number; everything else is either the gateway's answer or an open question, and
# is rendered as such.
_ABSENCE_CAUSE = {
    "rig_limited": "rig-limited",
    "untestable": "the rig cannot pose this",
    "search_exhausted": "still climbing when the range ran out",
    "harness_error": "harness fault",
    "not_served": "not served",
    "below_resolution": "below measurement resolution",
    "not_measured": "not measured",
}


def _absent_cause(chart, r):
    """The cause the RECORD gives for this row's absence, or None when it gives none.

    None rather than a fallback ON PURPOSE. This returned `chart.not_served_text` for an unrecognised
    reason, and because that is never falsy it made every chart's `not_measured_text` unreachable
    through the `or` chain at the call site - so an unmeasured metric on a cell that DID serve was
    captioned as a gateway that did not. `memory_rss` printed "did not serve" beside its own
    `+12.3 MiB/min under load` on the same bar; `stream_added_ttft` asserted "no SSE streaming" over a
    record whose `stream_served` is true. Returning None lets the caller reach the wording its author
    wrote for exactly this case."""
    field = chart.series[0].field if getattr(chart, "series", None) else None
    if not field:
        return None
    # The row is FLATTENED - envelopes became plain numbers upstream - so the reason comes from the
    # `_<field>_reason` the projection carries beside the value, not from the value itself.
    reason = r.get(f"_{field}_reason") or mreason(r.get(field))
    cause = _ABSENCE_CAUSE.get(reason or "")
    return f"\u2715 {cause}" if cause else None


def _absent_label(chart, r):
    """The absent-row caption: what the record says, else what the chart's author wrote."""
    return _absent_cause(chart, r) or chart.not_served_text


def mvalid(env) -> bool:
    """A metric draws a bar iff its envelope carries a value (certified, incl. a measured 0), or is a
    below-resolution absence (which displays as 0, see mval)."""
    return _is_env(env) and (env.get("value") is not None or env.get("reason") == "below_resolution")


# ---- the LATENCY-THROUGHPUT FRONTIER (Python mirror of seal.mjs FRONTIER_BOUNDS_MS / frontierAt) -----
# One measurement - the concurrency sweep - read at each declared tail-latency bound. A reading says:
# "the most req/s this gateway carried while 99% of requests finished under `bound_ms`, failing none it
# accepted." Every reading's rate is a sealed envelope, so mval()/mvalid() read it exactly like every
# other metric on the board; the rest of the reading (concurrency, the tail it actually came with, the
# concurrency above it that stopped qualifying, whether the sweep ever found a ceiling) is EVIDENCE about
# that rate and rides as plain fields.
#
# MIRRORS, THEREFORE CHECKED UPSTREAM RATHER THAN TRUSTED HERE: the bundle carries each reading's own
# `bound_ms`, and site/check-consistency compares seal.mjs's list against what the raw artifacts contain.
# This file reads bounds FROM the readings wherever it can and uses the list only to lay out an axis, so a
# gateway that publishes a bound not in this list still charts and a drift shows up as a violation there
# rather than as a silently short axis here.
FRONTIER_BOUNDS_MS = [1, 5, 10, 50, 100]
# WHICH BOUND THE RANKED BARS USE. A ranked bar chart has to pick one, and 10 ms is where the field
# population actually separates (of the 1632 rungs on the 2026-07-29 board 16% hold 1 ms, 47% hold 10 ms,
# 88% hold 100 ms). It is a VIEW, not a verdict - every bound is published on every cell, and the bound is
# rendered INTO the title from this constant (never typed into a caption) so no chart can imply a bound it
# did not use. That is the whole difference from `SUSTAINED_P99_CEILING_US`, which decided which
# measurements existed at all.
DEFAULT_BOUND_MS = 10


# HOW THE UNBOUNDED READING IS NAMED, everywhere: "no bound" / "none", never "∞ ms". It is not a very
# large bound, it is the absence of one - the question becomes "how much can it carry before it starts
# failing requests" and no latency claim is made at all - and an axis tick reading "∞" invites a reader to
# treat it as the far end of the same scale.
def _frontier_at(frontier, bound_ms):
    """frontierAt(frontier, boundMs) mirror: the reading taken at `bound_ms`, or None.

    ONE ACCESSOR, so the ranked bar, the shape panels and the report table cannot disagree about which
    reading a "@10 ms" label refers to. None (no reading for this bound) is a real state and the ONLY
    honest answer for a record whose frontier is empty - a pre-frontier snapshot has no throughput
    reading, which is not the same as a gateway that carried nothing."""
    if not isinstance(frontier, list):
        return None
    for r in frontier:
        if isinstance(r, dict) and r.get("bound_ms") == bound_ms:
            return r
    return None


def _proj_frontier(readings) -> list:
    """The sealed frontier → the flattened per-reading rows the charts draw from.

    Flattened for the same reason every other projection here flattens: the renderer works on plain
    numbers, and the WHY of an absence has to travel beside the value or it is gone by the time anything
    captions it (see _absent_cause). An empty/absent frontier projects to an EMPTY LIST, which every
    consumer below reads as "this record carries no throughput reading" - never as a zero."""
    out = []
    for r in readings or []:
        if not isinstance(r, dict):
            continue
        out.append({
            "bound_ms": r.get("bound_ms"),
            "rps": mval(r.get("rps")),
            # The engine's own absence token for THIS reading's rate, carried so a caption names the
            # record's reason instead of asserting one (_ABSENCE_CAUSE).
            "reason": mreason(r.get("rps")),
            "concurrency": r.get("concurrency"),
            # The tail the winning rung ACTUALLY produced - not the bound. A gateway holding 4 ms under a
            # 100 ms bound is not the same finding as one sitting at 99 ms, and publishing the bound here
            # would restate the question as though it were the answer.
            "p99_us": r.get("p99_us"),
            "first_disqualified_conc": r.get("first_disqualified_conc"),
            # A RATE THE SWEEP NEVER FOUND A CEILING FOR IS A FLOOR. The sweep ran out of ladder rather
            # than establishing a maximum, so rendering it as a ceiling would publish our own range as the
            # gateway's answer. Every surface below prefixes it "≥".
            "lower_bound": r.get("lower_bound") is True,
        })
    return out


def _frontier_row(obj: dict, frontier: list, field: str, bound_ms=DEFAULT_BOUND_MS) -> None:
    """Flatten the reading at `bound_ms` onto a chart row as `field` (+ its evidence, + its validity).

    THE ABSENCE-REASON KEY IS `_<field>_reason` because that is the ONE name `_absent_cause` looks a
    reason up under. Writing it under the envelope's own name instead is the bug that made `_ABSENCE_CAUSE`
    dead in production for the whole streaming lane (see _proj_streaming): the lookup never resolved and
    the blanket captions it was written to replace are what actually printed.

    NO REASON IS WRITTEN WHEN THERE IS NO READING. A record with an empty frontier gives no cause, so this
    asserts none and the chart falls through to the wording its author wrote for exactly that case. Mapping
    it to `not_measured` would put an engine token on a hole the engine never spoke about."""
    rd = _frontier_at(frontier, bound_ms)
    obj[f"_{field}_bound_ms"] = bound_ms
    if rd is None:
        obj[f"{field}_valid"] = False
        return
    if rd["rps"] is not None:
        obj[field] = rd["rps"]
    obj[f"{field}_valid"] = rd["rps"] is not None
    if rd["reason"]:
        obj[f"_{field}_reason"] = rd["reason"]
    obj[f"_{field}_conc"] = rd["concurrency"]
    obj[f"_{field}_p99_us"] = rd["p99_us"]
    obj[f"_{field}_first_disq"] = rd["first_disqualified_conc"]
    obj[f"_{field}_lower_bound"] = rd["lower_bound"]


# ---- the CLIMB: the sweep itself, rung by rung (rps vs CONCURRENCY) ---------------------------------
# A DIFFERENT AXIS FROM THE FRONTIER, and conflating them loses half the story. The frontier asks "how
# much throughput can I have if I insist on a tight tail" (x = the bound). The climb asks "where does it
# saturate, and what does the tail do while it gets there" (x = concurrency). Only the climb can show
# "started low, took forever to climb, peaked early", because only the climb has concurrency on an axis.
#
# THE RUNGS ARE EVIDENCE, NOT A METRIC, and they arrive as PLAIN objects: gen-data.mjs attaches
# `sweep_max_proxy` to the cell as `sweep` precisely so a reader can re-derive every frontier reading
# rather than taking it on trust ("It rides as a plain field: it is evidence, not a metric"). So mval() must
# never be pointed at a rung - it would reject the plain scalars as unsealed and refuse to chart the one
# thing that makes the sealed numbers checkable.
def _proj_sweep(sweep) -> list:
    """The cell's raw rungs → the plain rung rows the climb draws from. Empty when there is no sweep."""
    out = []
    for r in sweep or []:
        if not isinstance(r, dict):
            continue
        c, rps = r.get("conc"), r.get("rps")
        if not isinstance(c, (int, float)) or not isinstance(rps, (int, float)) or c <= 0:
            continue
        out.append({"conc": float(c), "rps": float(rps),
                    "p99_us": r.get("p99_us") if isinstance(r.get("p99_us"), (int, float)) else None,
                    "fail": float(r.get("fail") or 0)})
    return out


def _climb_points(rungs: list) -> list:
    """One point per CONCURRENCY: the median rate and median tail of the windows probed there.

    THE MEDIAN, NOT THE MAX, and the individual windows are drawn behind it (see render_frontier_climb).
    The frontier's readings take the MAX rate, because the question there is the most the gateway carried;
    the question here is what it does at a given concurrency, and a max over three windows would draw the
    luckiest window as the curve. Both are published, from the same rungs, and the chart says which is
    which - the alternative (one aggregate serving two different questions) is how a "maximum" ended up
    below a sustained figure read off the same windows.

    `any_fail` is per-concurrency because a single failed request is what ENDS a climb: a rung that failed
    something it accepted qualifies for no frontier reading at any bound, so the rate above that point is
    not throughput the gateway is entitled to."""
    by_c: dict = {}
    for r in rungs:
        by_c.setdefault(r["conc"], []).append(r)
    out = []
    for c in sorted(by_c):
        ws = by_c[c]
        rates = sorted(w["rps"] for w in ws)
        tails = sorted(w["p99_us"] for w in ws if w["p99_us"] is not None)

        def _med(xs):
            return None if not xs else (xs[len(xs) // 2] if len(xs) % 2
                                        else (xs[len(xs) // 2 - 1] + xs[len(xs) // 2]) / 2)
        out.append({"conc": c, "rps": _med(rates), "p99_us": _med(tails),
                    "any_fail": any(w["fail"] > 0 for w in ws), "windows": ws})
    return out


# WHAT FRACTION OF ITS PEAK COUNTS AS "SATURATED". Naming the concurrency where a gateway PEAKS is the
# wrong number for "peaked early": agentgateway's highest median rate lands at c=256, having been within
# 7% of it since c=8, so the peak's concurrency describes noise at the top of the ladder rather than where
# the climb ended. This is the one chosen constant in the climb, it is disclosed on the chart and in the
# table header, and it changes no ranking - nothing is ordered by it.
_SATURATION_FRAC = 0.95


def _climb_summary(pts: list) -> dict | None:
    """The climb as the handful of numbers the table publishes and the panel captions state.

    Every field is a direct read of the rungs, so each visual claim is re-derivable from the table beside
    it: the start, the peak, how much of the peak the start already was, where the climb effectively
    ended, and where (if anywhere) the gateway began failing requests it had accepted."""
    rated = [p for p in pts if p["rps"] is not None]
    if not rated:
        return None
    first, peak = rated[0], max(rated, key=lambda p: p["rps"])
    sat = next((p for p in rated if p["rps"] >= _SATURATION_FRAC * peak["rps"]), peak)
    fails = [p["conc"] for p in pts if p["any_fail"]]
    return {
        "c_first": first["conc"], "rps_first": first["rps"], "p99_first": first["p99_us"],
        "c_peak": peak["conc"], "rps_peak": peak["rps"], "p99_peak": peak["p99_us"],
        # HOW MUCH THE CLIMB ACTUALLY BOUGHT, as a multiple of where it started. This is "climbed a little"
        # vs "climbed a lot" as a number: agentgateway multiplies its c=1 rate 5.3x, one-api 1.4x - and
        # one-api paid for that 1.4x with a tail going from 37 ms to 3.4 s.
        "gain": peak["rps"] / first["rps"] if first["rps"] else None,
        "conc_gain": peak["conc"] / first["conc"] if first["conc"] else None,
        "c_sat": sat["conc"],
        "c_first_fail": min(fails) if fails else None,
        "c_top": max(p["conc"] for p in pts),
        # The tail at the top of the ladder, which is the other half of "what did it cost to get there".
        "p99_top": next((p["p99_us"] for p in reversed(pts) if p["p99_us"] is not None), None),
    }


def _ideal_rps(direct_p99_us, c):
    """The ZERO-OVERHEAD reference rate at concurrency `c`: Little's Law with the cell's OWN measured
    direct-to-mock service time. `rps = c / s`, s = the direct leg's p99.

    A REFERENCE, NOT A BOUND, and the file must never render it as one - see the caption built in
    render_frontier_climb, which states the percentile it came from and quantifies how far off the
    p99-based form is on the cells where both halves are published. None when the direct leg was not
    measured, because a reference line with no measured basis is exactly the invented qualifying bar this
    codebase spent its history deleting."""
    if not isinstance(direct_p99_us, (int, float)) or direct_p99_us <= 0:
        return None
    return c * 1e6 / float(direct_p99_us)


def _frontier_gain(frontier: list):
    """(tightest bound that has a reading, fractional gain from it to the UNBOUNDED reading), or None.

    THE ONE NUMBER THAT SAYS WHETHER A GATEWAY'S CURVE IS FLAT OR STEEP, which is the whole finding the
    two retired scalars destroyed. On the 2026-07-29 board agentgateway carried 23,630 rps at a 1 ms tail
    and gained only 7% by dropping the bound entirely; apisix needed 5 ms to nearly double and 10 ms to
    reach 19k. Published as one number those looked comparable - this ratio is what shows they are not the
    same machine. None when either end is missing, because a gain computed against a bound the gateway has
    no reading for would be a comparison against nothing."""
    lo = next((r for r in frontier or [] if r["bound_ms"] is not None and r["rps"]), None)
    hi = _frontier_at(frontier, None)
    if not lo or not hi or not hi.get("rps"):
        return None
    return lo["bound_ms"], (hi["rps"] / lo["rps"]) - 1.0


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
    # SERVED, BUT THE METRIC CAME OUT 0. The DEFAULT NAMES NO BOUND, and that is the point: this string
    # read "0 · no load held p99 < 1 s", a qualifying bar the engine NEVER enforced. The retired gate was
    # `SUSTAINED_P99_CEILING_US` = 20 ms; a 1 s bar would have passed 96% of all 1632 recorded rungs
    # against 57% for the real one, so the sentence described a test that never ran - and being the
    # DEFAULT it printed on charts measuring latency, memory growth and dollars, none of which have a
    # tail-latency bound to state. A shared default cannot know its chart's bound, so it must not claim
    # one; a chart whose 0 does mean "no rung held bound B" says so in its own zero_text, rendered from
    # the bound it actually used.
    zero_text: str = "0  ·  measured 0 on this metric"
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
    # A QUALIFIER ON THE NUMBER ITSELF: optional fn(row) -> str prefixed to the PRIMARY bar's label.
    #
    # Separate from `annot` because annot appends AFTER the number, and that is not where "this is a floor,
    # not a ceiling" can live: a reader scanning bars reads "23,630" and has already formed the claim
    # before the clause arrives. A frontier reading whose sweep ran out of ladder is a LOWER BOUND, so its
    # bar reads "≥ 23,630" and the sentence explaining why follows in the annot.
    numlab_prefix: object = None


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


# ── the frontier bar's own evidence, printed on the bar ───────────────────────────────────────────
# A frontier reading is a claim with both halves of its proof attached (engine/src/frontier.rs: the
# concurrency the winning rate was observed at, the tail it ACTUALLY came with, and the lowest
# concurrency above it that stopped qualifying). The retired scalars published a value and a
# concurrency, so a reader had no way to see whether the search had established a boundary or merely
# stopped - which is exactly how a "maximum" got published below a sustained figure read off the same
# rungs. Printing the evidence beside the number is what makes the bar checkable instead of asserted.
def _frontier_annot_for(field: str):
    def _annot(r):
        bits = []
        if r.get(f"_{field}_lower_bound"):
            # THE FLOOR, IN WORDS, beside the "≥" the number already carries (see numlab_prefix). The
            # sweep's top rung WAS the winner, so nothing in the record sits above it: we stopped because
            # our ladder ended, not because the gateway did.
            bits.append("floor - the sweep's top rung won, so no ceiling was established")
        c, p = r.get(f"_{field}_conc"), r.get(f"_{field}_p99_us")
        if isinstance(c, (int, float)):
            bits.append(f"at c={c:g}")
        if isinstance(p, (int, float)):
            # The tail it actually produced, NOT the bound - "held 0.6 ms under a 10 ms bound" and "sat at
            # 9.9 ms under a 10 ms bound" are different findings and the bound alone cannot tell them apart.
            bits.append(f"p99 {p/1000:,.3g} ms")
        d = r.get(f"_{field}_first_disq")
        if isinstance(d, (int, float)):
            bits.append(f"c={d:g} broke it")
        extra = _perf_annot(r) if field == "rps_at_bound" else _xlate_path_annot(r)
        if extra:
            bits.append(extra)
        return "  ·  ".join(bits) if bits else None
    return _annot


def _frontier_prefix_for(field: str):
    """"≥ " on a reading the sweep never found a ceiling for, "" otherwise. See Chart.numlab_prefix."""
    return lambda r: "≥ " if r.get(f"_{field}_lower_bound") else ""


def _xlate_path_annot(r):
    """The translation lane's direction + provenance suffix, shared by both translation charts."""
    if not r.get("_xlate_ingress"):
        return None
    return (f"{_dialect(r.get('_xlate_ingress'))} → {_dialect(r.get('_xlate_egress'))}"
            + _sweep_label({"sweep": r.get("_xlate_source")}))


# The STREAMING lane's provenance annotation: the same mechanism _perf_annot gives the passthrough
# charts and the xlate annots give the translation charts. Every streaming number is currently a LEGACY
# stream-suite reading (source "stream-suite"), so it must disclose that just like its sibling charts do.
def _stream_annot(r, extra=None):
    lbl = _sweep_label({"sweep": r.get("_stream_source")}).strip(" ()")
    # AND THE DIALECT, on the same rule `_perf_annot` uses: named whenever it is not the common openai
    # diagonal. Without it a stream measured on another protocol is ranked against openai readings with
    # nothing saying so, which is a comparison the chart cannot support.
    d = r.get("_stream_dialect")
    dial = f"on {_dialect(d)}" if d and d != "openai" else None
    bits = [b for b in (extra, dial, lbl) if b]
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


# Cost model: the gateway is pinned to 4 cores = an m7g.xlarge (the class AIGatewayBench costs on).
# us-east-1 on-demand ≈ $0.1632/hr for that slice. Override with GATEWAY_HOURLY_USD.
#
# A CHOSEN CONSTANT, SO IT IS DISCLOSED AS A NUMBER on both cost charts and in the report's method line -
# rendered from here, never typed into a caption. "Per dollar" is not a self-describing unit: a reader on
# reserved pricing, another region or another instance class needs the divisor to rescale, and a chart that
# hides it is asking to be believed rather than checked. DEFINED ABOVE `CHARTS` because those subtitles
# render it at import time.
#
# The rate it divides is the frontier reading at DEFAULT_BOUND_MS (see _perf_derived): a gateway with no
# reading at that bound has no cost basis and renders as an absence, never as free.
GATEWAY_HOURLY_USD = 0.1632


CHARTS = [
    # ── the headline: what the system can DO ──────────────────────────────────────────────────────
    # The three passthrough charts read the CANONICAL best_cell numbers (matrix per-cell sweep,
    # via site/data.json), the same record the site's Passthrough table ranks.
    Chart(
        name="added_latency",
        suite="perf",
        title="Added latency - what the gateway costs you",
        subtitle="p99 the gateway adds on top of the upstream, concurrency 1, same-dialect passthrough (OpenAI where served) (lower is better)",
        unit="µs",
        series=[Series("added_latency_p99_us", "p99 added latency", "rank")],
        log=True,
        # Same MEDIUM-R3-3 guard as the translation lane. _proj_perf always reports served=True, so an
        # absent added-latency envelope fell through to the DEFAULT zero_text - which then read "0 · no
        # load held p99 < 1 s", a THROUGHPUT-failure sentence captioning an unmeasured LATENCY, and one
        # naming a 1 s bar no gate in the engine ever enforced. It sank to the bottom rather than the top,
        # so the ranking was not corrupted, but the label was a fabricated explanation of a fabricated
        # test.
        not_measured_text="✕ added latency not measured",
        null_not_served=True,
        # A 0 on this chart is the WINNING end, not a failure: lower-is-better, and a below_resolution
        # absence (the difference ran and came out under what the rig can resolve) charts as 0 by
        # design (see mval). Without zero_ok that 0 fell through to the DEFAULT zero_text - then a
        # THROUGHPUT-failure sentence, in failure orange, captioning a latency WIN, and asserting a 1 s
        # qualifying bar into the bargain. zero_ok renders it in ink at the winning end instead, and the label disclosed
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
    # ── the frontier, RANKED AT ONE NAMED BOUND ───────────────────────────────────────────────────
    # DELETED HERE: `rps_max_proxy` ("Max proxy throughput - raw forwarding speed") and
    # `rps_sustained_20ms` ("Sustained throughput under 20 ms LLM latency"), plus their two report-table
    # columns. Both drew the same concurrency sweep collapsed to one number by a different algorithm, and
    # the pair was self-contradicting in the field: aisix openai-responses>anthropic published a
    # "maximum" of 16,232 against its own sustained 16,610, bifrost openai-responses>openai-responses
    # 5,113 against 5,174 - because the plateau search quit before rungs the bisection then reached. A
    # maximum another reading of the same windows beats is not a maximum, so there is nothing here for a
    # chart to rank. Both subtitles also stated a qualifying bar of "p99 < 1s"; the engine enforced 20 ms
    # (see the zero_text note on Chart). This chart replaces them: the SAME sweep, read at a bound the
    # title names, with the reading's own evidence on the bar.
    #
    # A RANKED BAR AT ONE BOUND IS HOW A READER COMPARES GATEWAYS, and the bound is rendered from
    # DEFAULT_BOUND_MS into the title so the picture cannot outlive the constant. It is deliberately NOT
    # the headline claim about a gateway - the shape across bounds is (see render_frontier_shape) - and a
    # reader who wants a different bound has every one of them published on every cell.
    Chart(
        name="frontier_rps_at_bound",
        suite="perf",
        title=f"Throughput at a {DEFAULT_BOUND_MS:g} ms tail-latency bound",
        subtitle=(f"the most req/s each gateway carried while 99% of requests finished under "
                  f"{DEFAULT_BOUND_MS:g} ms and it failed none it accepted; same-dialect passthrough "
                  f"(OpenAI where served). One of {len(FRONTIER_BOUNDS_MS) + 1} bounds published per cell "
                  f"- see the frontier-shape chart (higher is better)"),
        unit="requests / sec",
        series=[Series("rps_at_bound", f"req/s @ p99 < {DEFAULT_BOUND_MS:g} ms", "rank")],
        higher_better=True,
        # A bar is drawn iff THIS BOUND's reading carries a rate. A record with no frontier at all (every
        # snapshot predating the frontier) is not-measured, not zero: see _frontier_row.
        served_field="rps_at_bound_valid",
        not_served_text="✕ no frontier reading in this record",
        # THE 0 ON THIS CHART IS THE ONE PLACE A "no load held the bound" SENTENCE IS TRUE, and it names
        # the bound it means, from the constant the chart is built on.
        zero_text=f"0  ·  no rung held a {DEFAULT_BOUND_MS:g} ms tail while failing nothing",
        annot=_frontier_annot_for("rps_at_bound"),
        numlab_prefix=_frontier_prefix_for("rps_at_bound"),
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
    # THE COST LANES DERIVE FROM THE FRONTIER READING AT DEFAULT_BOUND_MS, AND SAY SO IN THEIR SUBTITLES.
    #
    # The scalar they used to divide (`rps_sustained_20ms`) is gone, and a cost per request is a rate
    # divided by a price - so it inherits whatever qualification the rate carried. It was inheriting a bar
    # the engine never enforced, which is the retired "p99 < 1 s" defect one derivation removed: a dollar
    # figure whose latency bound is invisible.
    #
    # WHY ONE NAMED BOUND RATHER THAN A COST FRONTIER. A cost frontier was the other option, and it would
    # be exactly the throughput frontier inverted: cost per million = price / (rps × 3600) × 1e6 is a
    # strictly decreasing function of the ONE input that varies, so a five-bound cost curve carries no
    # information the five-bound throughput curve does not already show, drawn in a unit that is harder to
    # reason about. What the reader needs from cost is the operating point they are being quoted, so the
    # honest move is to compute it at ONE bound and name that bound everywhere it shows - the SAME
    # DEFAULT_BOUND_MS the throughput bars use, so the two never describe different operating points.
    #
    # THE PRICE IS A CHOSEN CONSTANT AND IS DISCLOSED AS A NUMBER, not folded into the phrase "per dollar":
    # every caption renders GATEWAY_HOURLY_USD, so a reader on different pricing can rescale, and a chart
    # cannot outlive an edit to the constant.
    Chart(
        name="rps_per_dollar",
        suite="perf",
        title="Throughput per dollar",
        subtitle=(f"req/s at a {DEFAULT_BOUND_MS:g} ms p99 tail bound, per $/hr of the pinned 4-core "
                  f"(m7g.xlarge) slice at ${GATEWAY_HOURLY_USD:.4f}/hr (higher is better)"),
        unit="RPS per $/hr",
        series=[Series("rps_per_dollar", "RPS per $/hr", "rank")],
        higher_better=True,
        # Gate on the frontier reading's own validity: no reading at this bound, no cost bar. A cost
        # derived from a number that is not there is not a cheap gateway, it is not a measurement.
        served_field="rps_at_bound_valid",
        not_served_text="✕ no frontier reading in this record",
    ),
    Chart(
        name="cost_per_million",
        suite="perf",
        title="Cost per million requests",
        subtitle=(f"$ to serve 1M requests at a {DEFAULT_BOUND_MS:g} ms p99 tail bound on the pinned "
                  f"4-core (m7g.xlarge) slice at ${GATEWAY_HOURLY_USD:.4f}/hr (lower is better)"),
        unit="$ / 1M requests",
        series=[Series("cost_per_million_usd", "cost / 1M", "rank")],
        money=True,
        served_field="rps_at_bound_valid",
        not_served_text="✕ no frontier reading in this record",
        # THE DEFAULT zero_text OPENS WITH A LITERAL "0" - which on a dollar axis reads as a price of
        # zero, the cheapest possible answer on a lower-is-better chart. This chart's absent rows are
        # gateways whose cost is UNDEFINED because nothing they carried held the bound, so the sentence
        # says that, names the bound it means, and never shows a number.
        zero_text=f"no cost per request: no rung held a {DEFAULT_BOUND_MS:g} ms tail",
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
        # EVERY frame, not 99.9% of them. This read "99.9% of frames delivered", and the gate is
        # `STREAM_MIN_DELIVERY_RATIO = 1.0` (run.rs) - "a proxy that drops a frame has dropped a user's
        # token" - so the caption advertised a tolerance the engine does not grant. Same defect class as
        # the retired "p99 < 1 s": a published surface describing a looser test than the one that ran.
        # The stall bound and the <0.1% stream-error tolerance (STREAM_MAX_ERROR_RATIO = 0.001) are real.
        subtitle="max concurrent streams with EVERY expected content frame delivered, no stalled inter-frame gap, <0.1% of streams errored (higher is better)",
        unit="concurrent streams",
        series=[Series("stream_sustained_streams", "sustained streams", "rank")],
        higher_better=True,
        # served_field is stream_sustained_valid (streamed AND not mock-bound), mirroring streamcpu_fps
        # below (MEDIUM-R2-2): a rig-limited sustained count is not a valid gateway-vs-ceiling reading, so
        # it renders "not proven" rather than a clean bar. A mock-bound / unverifiable count never draws a
        # full bar or ranks in the top-N - the same discipline the cpu-fps lane already applies.
        served_field="stream_sustained_valid",
        not_served_text="✕ not measured - see this cell's own reason",
        # AUDIT #3: a certified 0 is a MEASURED FAILURE (offered stream load, sustained none), and must
        # never read like the unmeasured/rig-limited state above. Name it as the failure it is.
        zero_text="0  ·  MEASURED: sustained no stall-free stream",
        annot=lambda r: _stream_annot(
            r, (lambda f: f"{f:,.0f} frames/s" if f > 0 else None)(float(r.get("stream_sustained_fps") or 0))),
    ),
    # ── DELETED: `streamcpu_fps` ("Streaming relay throughput (CPU-bound)"), the chart of the producer's
    # `cpu_fps`, and its `cpu_fps_per_core` annotation. The metric is retired, and the reason is that it
    # DISAGREED WITH THE MEASUREMENT IT WAS SUPPOSED TO SUPPORT. Across the 16 cells that published both
    # `cpu_fps` and `streams_sustained_fps`:
    #   · 4 were INVERTED - the "CPU-bound" frame rate came out BELOW the frame rate already proven to be
    #     delivered whole, i.e. a ceiling under its own floor;
    #   · 5 were redundant, within 1% of the delivery-proven figure, so they added no reading;
    #   · 7 were measured at a concurrency where the DELIVERY GATE DID NOT HOLD - a frame rate recorded
    #     while the gateway was dropping frames, which is not a relay throughput, it is a loss rate with a
    #     numerator.
    # `streams_sustained_fps` survives and is already published on the stream_sustained bar above (the
    # "frames/s" annotation), so nothing a reader could act on was removed: the frame rate is still there,
    # measured at a concurrency where every frame arrived. There is no replacement chart, because a
    # second, weaker reading of the same quantity is what was wrong with it.
    # ── translation: the CANONICAL translation cell (matrix per-cell sweep) ───────────────────────
    # Same record the site's Translation surfaces read: OpenAI ingress translated to the gateway's
    # measured egress (named per bar). A gateway with no matrix translation cell falls back to the
    # legacy xlate suite (Anthropic in -> OpenAI out) and the bar says so; direction is never mixed
    # silently across surfaces.
    # The translation cell carries its OWN frontier, off its own sweep, so the translation throughput
    # chart is the same reading at the same bound as the passthrough one - which is the only way the two
    # bars are comparable at all. It used to chart `rps_sustained_20ms` off this cell and caption it
    # "p99 < 1s, <0.1% errors, 20 ms model delay": three claims, of which the first named a bound no gate
    # enforced and the second a tolerance the frontier does not grant (a rung qualifies only if it failed
    # NOTHING it accepted - frontier.rs, "there is no concurrency at which that is the gateway
    # succeeding"). The 20 ms model delay is real and stays.
    Chart(
        name="xlate_frontier_rps_at_bound",
        suite="xlate",
        title=f"Cross-protocol translation: throughput at a {DEFAULT_BOUND_MS:g} ms tail-latency bound",
        subtitle=(f"the most req/s each gateway carried on its canonical translation path (direction on "
                  f"the bar) while 99% of requests finished under {DEFAULT_BOUND_MS:g} ms and it failed "
                  f"none it accepted, under a 20 ms model delay (higher is better)"),
        unit="requests / sec",
        series=[Series("xlate_rps_at_bound", f"translated req/s @ p99 < {DEFAULT_BOUND_MS:g} ms", "rank")],
        higher_better=True,
        # Gate on this bound's reading carrying a rate, exactly as the passthrough chart does. A gateway
        # that cannot translate at all has no xlate row (no translation_cell) and is off the chart
        # entirely; a gateway that translates but has no reading at this bound says so.
        served_field="xlate_rps_at_bound_valid",
        not_served_text="✕ no frontier reading in this record",
        zero_text=f"0  ·  no rung held a {DEFAULT_BOUND_MS:g} ms tail while failing nothing",
        annot=_frontier_annot_for("xlate_rps_at_bound"),
        numlab_prefix=_frontier_prefix_for("xlate_rps_at_bound"),
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
        # The same direction+provenance annot the translation throughput chart uses, from one function, so
        # the two translation PNGs cannot come to disagree about which path a bar describes.
        annot=_xlate_path_annot,
    ),
    # Governance is intentionally NOT charted on the neutral board: the governed suite is a
    # non-default launch wired by a single manifest, so a comparison would spotlight that one
    # entrant and read "not tested" for the rest. Governance overhead belongs on the
    # advocacy site. The governed suite still runs and its data is kept for that use.
]


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
# "streamcpu" is no longer in this tuple: the only chart that read that lane was `streamcpu_fps`, deleted
# above with the `cpu_fps` metric it drew. The suite name would otherwise still route to _proj_streaming
# and quietly build rows for a lane nothing charts.
_PROJECTED_SUITES = ("stream", "memory")


def _proj_streaming(key: str) -> dict | None:
    """canonicalStreaming(g) mirror → a row carrying the chart's legacy stream_* keys.

    g.streaming (source:"matrix" or a legacy stream-fallback) carries the matrix-native field names
    (added_ttft_p99_us, added_gap_p99_us, streams_sustained, streams_sustained_fps, …). A present record means the
    gateway streamed, so stream_served is true (matching canonicalStreaming's `stream_served: true`)."""
    s = (CANON.get(key) or {}).get("streaming")
    if not s:
        return None
    # Every metric is a SEALED ENVELOPE: the mock-bound gate was applied at seal time, so a rig-limited /
    # unverifiable value is already {value:null,…}. Validity is simply "the envelope carries a value" -
    # there is no separate mock-bound flag to re-check (it was consumed). streams_sustained is a
    # throughput field; TTFT / gap are ungated latency-shaped envelopes.
    #
    # `cpu_fps` IS GONE from the producer and from this projection (with `cpu_fps_per_core` and the
    # `streamcpu_*` row keys it fed). See the deleted streamcpu_fps chart in CHARTS for the field
    # evidence: of the 16 cells that published both it and streams_sustained_fps, 4 were inverted below
    # the proven delivery boundary, 5 were within 1%, and 7 were measured where the delivery gate did not
    # hold. `streams_sustained_fps` below is the surviving frame rate, and it is the delivery-proven one.
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
        # WHICH DIALECT THE STREAM WAS MEASURED ON. The perf lane carries `_dialect` and `_perf_annot`
        # names it whenever it is not the common openai diagonal; the streaming lane carried nothing, so
        # a reading taken on a DIFFERENT protocol ranked silently against twelve openai readings. On the
        # 2026-07-29 board litellm-rust's streaming cell is anthropic and every other gateway's is
        # openai - and on top5_stream_added_ttft that unlabelled row is the WINNING bar.
        "_stream_dialect": (s.get("path") or {}).get("dialect"),
    }
    # Same below_resolution disclosure as _proj_perf: a sub-resolution TTFT/gap charts as 0 (mval)
    # and its label must say so (see _zero_label), not read like an exact measurement.
    for _row_f, _env_f in (("stream_added_ttft_p99_us", "added_ttft_p99_us"),
                           ("stream_added_gap_p99_us", "added_gap_p99_us")):
        if mreason(s.get(_env_f)) == "below_resolution":
            row[f"_{_row_f}_reason"] = "below_resolution"
    # EVERY ABSENCE REASON, not only below_resolution. The projection flattens envelopes into plain
    # numbers, and it used to keep the reason for exactly one token - so by the time a chart drew an
    # absent row, WHY it was absent had been discarded and the caption fell back to a hard-coded
    # string asserting "rig-limited" over absences the engine attributed to the gateway or called
    # explicitly unknown. See `_absent_label`.
    # KEYED BY THE ROW FIELD, NOT THE ENVELOPE FIELD. This wrote `_streams_sustained_reason` and
    # `_cpu_fps_reason` while `_absent_label` looks the reason up as `_<chart.series[0].field>_reason` -
    # and the row field name is `stream_sustained_streams`. So the
    # lookup never resolved for ANY chart, `_ABSENCE_CAUSE` was dead in production, and the blanket
    # captions it was written to replace are what actually printed. Two names for one fact is how a fix
    # ships and changes nothing.
    for _env_f, _row_f in (("streams_sustained", "stream_sustained_streams"),
                           ("streams_sustained_fps", "stream_sustained_fps"),
                           ("added_ttft_p99_us", "stream_added_ttft_p99_us"),
                           ("added_gap_p99_us", "stream_added_gap_p99_us")):
        _r = mreason(s.get(_env_f))
        if _r:
            row.setdefault(f"_{_row_f}_reason", _r)
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
    """Rows for a projected lane (streaming / memory), built from CANON, not results/."""
    rows = []
    for key, label in GATEWAYS.items():
        obj = _proj_memory(key) if suite == "memory" else _proj_streaming(key)
        if obj is None:
            continue
        obj["_key"], obj["_label"] = key, label
        rows.append(obj)
    return rows


def _perf_derived(obj: dict) -> None:
    """Derive the cost lanes from the FRONTIER READING AT DEFAULT_BOUND_MS (so the cost charts, the
    throughput bars and the report column all describe the same operating point).

    This divided `rps_sustained_20ms`, which is retired. Cost is a rate divided by a price, so it inherits
    the rate's qualification whether or not anything says so - and the caption said "p99 < 1 s", a bar no
    gate ever enforced. The bound is now an argument of the derivation and is named on every surface that
    shows the result (both cost subtitles, the report method line), so a dollar figure can never imply a
    tail it was not computed at. See the note above CHARTS for why this is one named bound rather than a
    cost frontier."""
    rate = float(obj.get("rps_at_bound") or 0)
    # req/s you get per $/hr at that bound, and $ per 1M such requests. 0 when nothing held the bound.
    obj["rps_per_dollar"] = (rate / GATEWAY_HOURLY_USD) if rate > 0 else 0
    # NOT `else 0`. At rate == 0 this quotient is undefined, and 0 is the CHEAPEST value on a
    # lower-is-better chart - so the gateways that held no load under the bound rendered as
    # free, the best possible result, while ranking last. `rps_per_dollar` above keeps its 0 because
    # zero requests per dollar genuinely IS zero; cost per request of a gateway that served nothing is
    # an absence, and the board's rule is that 0 is a number and n/a is not.
    obj["cost_per_million_usd"] = (GATEWAY_HOURLY_USD / (rate * 3600) * 1e6) if rate > 0 else None


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
    # THE FRONTIER, WHOLE, plus the one reading the ranked bars and the report column show. The whole
    # curve travels because the headline finding is its SHAPE (render_frontier_shape) and a row carrying
    # only the default bound's reading could not draw one. The `*_valid` flags the two retired scalars
    # published (and the constant-False `*_suppressed` compatibility shims beside them) went with them:
    # validity is now per READING, set by _frontier_row from that reading's own envelope.
    obj["_frontier"] = _proj_frontier(bc.get("frontier"))
    _frontier_row(obj, obj["_frontier"], "rps_at_bound", DEFAULT_BOUND_MS)
    # THE RUNGS THEMSELVES, for the climb (rps vs concurrency). Plain objects, not envelopes - they are the
    # evidence every frontier reading is derived from, which is why gen-data publishes them unsealed, and
    # why _proj_sweep reads them without mval(). Present on every snapshot on disk, including the ones
    # written before the frontier existed: the climb is drawable today, the frontier is not.
    obj["_sweep"] = _proj_sweep(bc.get("sweep"))
    # The direct-to-mock leg, which is the ONLY measured basis for a zero-overhead reference line. p99 is
    # the only percentile of it the bundle carries (there is no `direct_c1_p50_us`), so the reference can
    # only be drawn from p99 and the caption says exactly that.
    obj["_direct_c1_p99_us"] = mval(bc.get("direct_c1_p99_us"))
    obj["_gateway_c1_p99_us"] = mval(bc.get("gateway_c1_p99_us"))
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
    # Sealed envelopes; mval() reads them (None when absent).
    lat50 = mval(tc.get("added_latency_p50_us"))
    lat99 = mval(tc.get("added_latency_p99_us"))
    if lat50 is not None:
        obj["xlate_added_latency_p50_us"] = lat50
    if lat99 is not None:
        obj["xlate_added_latency_p99_us"] = lat99
    # Same below_resolution disclosure the passthrough row carries (see _proj_perf / _zero_label).
    if mreason(tc.get("added_latency_p99_us")) == "below_resolution":
        obj["_xlate_added_latency_p99_us_reason"] = "below_resolution"
    # THE TRANSLATION CELL'S OWN FRONTIER, off its own sweep, read at the SAME bound the passthrough bars
    # use - the only way a translated rate and a passthrough rate are comparable. This was
    # `mval(tc.get("rps_sustained_20ms"))` plus a hand-written `_xlate_rps_sustained_20ms_reason` /
    # `_xlate_rps_sustained_20ms_valid` pair; _frontier_row writes both, under the `_<field>_reason` name
    # `_absent_cause` actually looks up, so the record's own cause reaches the caption instead of a
    # blanket "(rig-limited)" over holes the engine recorded as search_exhausted or harness_error.
    obj["_xlate_frontier"] = _proj_frontier(tc.get("frontier"))
    _frontier_row(obj, obj["_xlate_frontier"], "xlate_rps_at_bound", DEFAULT_BOUND_MS)
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


def _us(v) -> str:
    """A microsecond figure in the largest unit that keeps it readable: 246 µs, 8.3 ms, 3.4 s.

    The climb's tail axis spans 244 µs to 3,396,667 µs on one shared scale, so a single unit cannot serve
    it: everything in µs makes the top unreadable, everything in ms makes the bottom read "0.2". One
    formatter, used by the axis, the panel captions and the climb table, so no two surfaces round the same
    tail differently."""
    if v is None:
        return "-"
    v = float(v)
    if v >= 1e6:
        return f"{v/1e6:,.3g} s"
    if v >= 1000:
        return f"{v/1000:,.3g} ms"
    return f"{v:,.0f} µs"


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
    metric - did-not-stream, cannot-translate, no-frontier-reading-at-this-bound - is never eligible for the
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
    # A TOP-N OF WINNERS THAT CANNOT DRAW IS A KNOWN, ACCEPTED CONSEQUENCE.
    #
    # On a `zero_ok` chart a sub-resolution 0 is the WINNING end and ranks first, which is right. On
    # `stream_added_gap` five gateways sit there, so all five top-5 slots go to rows whose value is a
    # below-resolution absence - and those render as text, not bars, so that PNG comes out with no bars
    # and no readable scale while the five gateways with a real measured overhead (5, 10, 24, 64, 85 us)
    # appear nowhere on it.
    #
    # A fallback to "the best rows that can draw" was tried and reverted: it breaks the tested invariant
    # that a below-resolution row is ELIGIBLE and ranks at the winning end
    # (charts_test.py asserts exactly that), and that invariant is worth more than the visual. Ranking
    # by drawability would mean a gateway with the best possible result loses its place to a slower one.
    # The measured values remain on the FULL chart, which lists every gateway and states the
    # sub-resolution rows as such.
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

    # A CHART WITH NO MEASUREMENT ON ITS OWN METRIC IS NOT DRAWN AT ALL.
    #
    # Rows exist as soon as the gateway has a canonical record, and the metric a given chart draws may not
    # be in that record: every snapshot on disk predates the frontier, so `frontier` is `[]` on all 14 and
    # `rps_at_bound` is absent everywhere. Without this the renderer produced a full-size, titled,
    # axis-labelled PNG of fourteen empty bars against an invented 0-to-1 scale - a picture that LOOKS like
    # a published comparison and would be committed and camo-cached as one. A missing metric is a missing
    # metric; the report page simply has no image for it (the `if png.exists()` gate downstream), and the
    # per-row "no frontier reading in this record" wording still shows on every surface built from text.
    #
    # Deliberately keyed on the PRIMARY series only, since that is what the chart ranks, and deliberately
    # generic rather than frontier-specific: the same trap is one field-rename away on any lane.
    if not any(bool(r.get(chart.served_field, True)) and r.get(primary) is not None for r in rows):
        print(f"skip {chart.name}: no row carries a {primary} measurement yet "
              f"(nothing to rank - not drawing an empty chart as if it were data)")
        return

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
    def _tagged(s: Series, v: float, r: dict | None = None) -> str:
        lab = f"{_numlab(v)} {s.tag}" if ns > 1 and s.tag else _numlab(v)
        # The PRIMARY series may qualify its own number (see Chart.numlab_prefix): a frontier reading the
        # sweep never found a ceiling for reads "≥ 23,630", because the digits alone would publish our
        # ladder's top rung as the gateway's maximum.
        if chart.numlab_prefix and r is not None and s is chart.series[0]:
            lab = f"{chart.numlab_prefix(r) or ''}{lab}"
        return lab

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
        # measurement on THIS chart's metric - the served_field (frontier → rps_at_bound_valid,
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
                    txt, col, weight = _tagged(s, v, r), INK, "bold"
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
                    #
                    # And say it with the RECORD's own reason where there is one: see `_absent_label`.
                    # This branch used to print a caption asserting "rig-limited" over absences the
                    # artifact attributed to the gateway or called explicitly unknown.
                    txt, col, weight = (
                        _absent_cause(chart, r) or chart.not_measured_text or chart.not_served_text,
                        "#c2410c",
                        "bold",
                    )
                else:
                    txt, col, weight = _absent_label(chart, r), "#c2410c", "bold"
                ax.text(tx, cy, txt, va="center", ha="left", fontsize=9.5,
                        fontweight=weight, color=col, zorder=4)
            elif v > 0 and _measured(r):  # secondary series (e.g. idle RSS): readable label, skip empty bars.
                # GATED on the row having COME UP (audit #7/#23): a genuinely not-served row must not
                # show a secondary idle number beside a "did not serve" primary. But a row whose PRIMARY
                # is merely null still measured this series, and deleting a real measurement because a
                # neighbouring field is null is the opposite of the honesty rule it was written for.
                ax.text(tx, cy, _tagged(s, v, r), va="center", ha="left", fontsize=9,
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


# ── THE HEADLINE: the SHAPE of each gateway's latency-throughput frontier ──────────────────────────
# The name of this PNG, so main() draws it and the report pages embed it without either side spelling
# the string twice. It is not a `Chart(...)`: Chart draws one horizontal bar group per gateway ranked by
# one number, and the finding here is a CURVE per gateway - the one thing a ranked bar cannot show.
FRONTIER_SHAPE_CHART = "frontier_shape"


# WHY SMALL MULTIPLES, ONE PANEL PER GATEWAY, AND NOT THE ALTERNATIVES.
#
# The finding is that two gateways with similar published throughput are not the same machine: on the
# 2026-07-29 board agentgateway carried 23,630 rps at a 1 ms tail and gained only 7% by dropping the bound
# entirely, while apisix needed 5 ms to nearly double and 10 ms to reach 19k. That is a statement about the
# SLOPE of each gateway's curve, and it has to be legible without reading a single number.
#
#   · Grouped bars per bound (14 gateways × 6 bounds = 84 bars) makes the reader reconstruct each curve out
#     of six bars that are not adjacent. The comparison is between SHAPES; bars encode magnitude.
#   · One line chart, 14 overlaid lines, is spaghetti at this field size, and the interesting gateways are
#     the flat ones - which is exactly what gets buried under the steep ones.
#   · Small multiples give each gateway its own curve, in reading order, and the eye compares silhouettes
#     across panels without decoding a legend. Flat-and-high (agentgateway) vs. steep-and-climbing (apisix)
#     is a difference in outline, visible at thumbnail size.
#
# ONE SHARED Y AXIS ACROSS ALL PANELS, which is the decision that makes or breaks it: per-panel autoscaling
# would give every gateway an identically dramatic climb and would say the opposite of the data.
#
# AND IT IS LOG, NOT LINEAR, WHICH IS A CORRECTNESS POINT AND NOT A VISIBILITY ONE. The claim this chart
# makes is a RATIO - agentgateway gains 7% from dropping the bound, apisix nearly doubles - and log is the
# only scale on which equal ratios have equal slopes. On a shared LINEAR axis the same +29% tilts steeply
# near the top of the range and is geometrically invisible near the bottom, so the picture would rank the
# finding by the gateway's magnitude rather than by its shape. It was linear first, and the first render
# with real data showed exactly that: one-api's frontier goes 28 → 36 req/s, a +29% tilt that IS its entire
# story, and against litellm-rust's 44,363 it drew as a dead-flat line on the bottom axis - 0.08% of the
# panel's scale, i.e. the shape chart hiding the shape.
#
# What log costs is the reading "panel height is throughput", which becomes "vertical POSITION is
# throughput" - still a valid cross-panel comparison, since the axis is shared and litellm-rust sits three
# decades above one-api. The fill under each curve goes with it: area to the bottom of a log axis is area to
# an arbitrary floor and means nothing, so there is nothing to fill.
#
# THE GAIN IS PRINTED TOO. The slope is the picture, but "+7%" vs "+96%" is the sentence a reader repeats,
# and a chart whose whole claim has to be measured off an axis by eye is one misreading from being quoted
# backwards.
def render_frontier_shape(out_stem: str | None = None) -> None:
    rows = _load("perf")
    if _mpl() is None:
        return  # no matplotlib - reports still generate from JSON
    bounds = FRONTIER_BOUNDS_MS + [None]     # ascending, unbounded LAST: the sequence IS the tradeoff
    # Bounds are taken from the READINGS as well as from the mirror, so a gateway publishing a bound this
    # file does not know about still charts (and the divergence is caught by check-consistency, which is
    # where a mirror drift belongs - not silently truncated to a shorter axis here).
    extra = sorted({r["bound_ms"] for row in rows for r in row.get("_frontier") or []
                    if r["bound_ms"] is not None and r["bound_ms"] not in FRONTIER_BOUNDS_MS})
    if extra:
        bounds = sorted(FRONTIER_BOUNDS_MS + extra) + [None]

    def _curve(row):
        """This gateway's rate at each bound, with None for a bound it has no reading for."""
        f = row.get("_frontier") or []
        return [(_frontier_at(f, b) or {}).get("rps") for b in bounds]

    curves = {r["_key"]: _curve(r) for r in rows}
    # NOTHING TO DRAW IS NOT AN EMPTY CHART. Every snapshot on disk predates the frontier, so every curve
    # is all-None; a titled grid of 14 blank panels would read as "we measured this and found nothing"
    # rather than "this has not been measured". A shape needs two points, so the chart exists only once
    # some gateway has two.
    if not any(sum(1 for v in c if v is not None) >= 2 for c in curves.values()):
        print(f"skip {FRONTIER_SHAPE_CHART}: no gateway has two frontier readings yet "
              f"(a shape needs two points - not drawing an empty chart as if it were data)")
        return

    n = len(rows)
    ncols = min(4, n) or 1
    nrows = (n + ncols - 1) // ncols
    fig_h = 2.35 * nrows + 1.7
    fig, axes = plt.subplots(nrows, ncols, figsize=(3.1 * ncols + 0.6, fig_h),
                             sharex=True, sharey=True)
    fig.patch.set_facecolor("white")
    axes = list(axes.flat) if hasattr(axes, "flat") else [axes]
    _vals = [v for c in curves.values() for v in c if v is not None and v > 0]
    # A LOG AXIS HAS NO ZERO, so the floor comes from the smallest rate actually read rather than from 0.
    # A measured 0 (the sweep ran and no rung held that bound) is therefore not plottable here - it is a
    # real result, and it is published as a number in the per-bound table and as its own labelled state on
    # the ranked bar. A chart cannot show it without inventing a position for it, so it does not try.
    ymin = min(_vals) * 0.6 if _vals else 1.0
    ymax = max(_vals) * 1.8 if _vals else 10.0
    xs = list(range(len(bounds)))
    xlabels = [("none" if b is None else f"{b:g}") for b in bounds]

    for i, (ax, r) in enumerate(zip(axes, rows)):
        ax.set_facecolor("white")
        col = LANG_COLORS.get(LANGS.get(r["_key"], ""), LANG_DEFAULT)
        c = curves[r["_key"]]
        ys = [v if v is not None else float("nan") for v in c]
        # nan breaks the line at a bound with no reading rather than interpolating across it: a segment
        # drawn through a missing reading would assert a rate between two bounds that was never read.
        ax.plot(xs, ys, color=col, lw=1.9, zorder=3, solid_capstyle="round")
        # No fill: on a log axis the area under a curve is area down to an arbitrary floor, so it encodes
        # nothing. The line and its markers carry the whole message.
        ax.set_yscale("log")
        f = r.get("_frontier") or []
        for x, b, v in zip(xs, bounds, c):
            if v is None:
                continue
            rd = _frontier_at(f, b) or {}
            if rd.get("lower_bound"):
                # A FLOOR IS DRAWN AS A FLOOR: an up-caret, not a dot, because the sweep ran out of ladder
                # and the true rate is somewhere at or above this point. A dot would claim a located value.
                ax.plot([x], [v], marker="^", ms=7, color=col, zorder=4,
                        markeredgecolor="white", markeredgewidth=0.8)
            else:
                ax.plot([x], [v], marker="o", ms=4.4, color=col, zorder=4,
                        markeredgecolor="white", markeredgewidth=0.8)
        ax.set_ylim(ymin, ymax)
        ax.set_xlim(-0.35, len(bounds) - 0.65)
        ax.set_xticks(xs)
        ax.set_xticklabels(xlabels, fontsize=8, color=GRAY)
        ax.yaxis.grid(True, color=GRID, zorder=0)
        ax.set_axisbelow(True)
        for sp in ("top", "right"):
            ax.spines[sp].set_visible(False)
        for sp in ("bottom", "left"):
            ax.spines[sp].set_color("#d7dae0")
        # sharex hides the tick labels on every panel that has another panel BELOW it - which on a partly
        # filled last row leaves the bottom-most panel of every short column with an unlabelled x axis, i.e.
        # a curve whose axis the reader has to find in another column. Force the labels on wherever nothing
        # is drawn underneath.
        ax.tick_params(labelsize=8, colors=GRAY, length=2,
                       labelbottom=(i + ncols >= n))
        # A PANEL WITH NO READING LOSES ITS AXES ENTIRELY, and that is not cosmetic. While a field run lands
        # gateway by gateway most panels have no curve, and drawn gridlines with a 0-to-N scale around empty
        # space read as a measured floor - the precise misreading the frontier exists to prevent. Stripped of
        # scale there is nothing to misread and the caption is the only content, which is correct: the
        # caption is the only thing the record supports.
        #
        # SUPPRESSED PER-AXES, NOT BY CLEARING THE TICK LOCATORS. `set_yticks([])` on a `sharey` grid is
        # shared state: it emptied the locator for EVERY panel, so the whole chart lost its rate axis the
        # moment any one gateway had no reading. tick_params + spines + grid are per-axes and leave the
        # shared scale intact for the panels that do have curves.
        if not any(v is not None for v in c):
            # which="both": a log axis draws MINOR ticks too, and tick_params defaults to major only - so
            # the suppressed panels kept a ladder of little dashes down their left edge, which is a scale.
            ax.tick_params(which="both", labelleft=False, labelbottom=False, length=0)
            ax.grid(False)
            for sp in ax.spines.values():
                sp.set_visible(False)
        ax.set_title(r["_label"], fontsize=10.5, fontweight="bold", color=INK, loc="left", pad=13)
        # THE PANEL'S OWN SENTENCE: the gain from its tightest measured bound to no bound at all, which is
        # the flat-vs-steep finding as one number. Named with the bound it starts from, because "+7%" is
        # only meaningful against a stated starting point.
        g = _frontier_gain(f)
        if g is not None:
            lo, gain = g
            sub = f"{gain:+.0%} from {lo:g} ms to no bound"
        elif any(v is not None for v in c):
            # Some readings, but not the pair the gain needs. Say which is missing rather than print a
            # dash: "no unbounded reading" is a different fact from "not measured".
            sub = "no 1-ms/unbounded pair to compare"
        else:
            # NO READING AT ALL, and the caption may not invent a cause. If the record gives one for this
            # gateway's default-bound rate, name it (the engine's own token); otherwise say only that the
            # record carries no reading.
            cause = _ABSENCE_CAUSE.get(r.get("_rps_at_bound_reason") or "")
            # Spelled out rather than marked with "✕": the bundled Inter faces have no MULTIPLICATION X
            # glyph, so that character renders as a tofu box in a PNG (matplotlib warns about it on the
            # existing charts). A panel whose only content is a caption cannot afford an unreadable one.
            sub = f"no reading - {cause}" if cause else "no frontier reading in this record"
        ax.annotate(sub, xy=(0, 1), xycoords="axes fraction", xytext=(0, 2),
                    textcoords="offset points", fontsize=8, color=GRAY, va="bottom", ha="left")
        from matplotlib.ticker import FuncFormatter
        ax.yaxis.set_major_formatter(FuncFormatter(lambda v, _p: _fmt(v) if v > 0 else "0"))
    for ax in axes[n:]:
        ax.set_visible(False)

    fig.suptitle("The latency-throughput frontier: how much each gateway carries, at each tail you accept",
                 fontsize=15, fontweight="bold", color=INK, x=0.008, ha="left", y=0.995)
    fig.text(0.008, 0.955,
             "one concurrency sweep per gateway, read at every declared p99 bound: the most req/s it "
             "carried while 99% of requests finished under that bound and it failed none it accepted. The "
             "same LOG y axis on every panel, so vertical position is throughput and TILT is the tradeoff - "
             "and equal tilts mean equal RATIOS, whatever the gateway's magnitude.",
             fontsize=9.5, color=GRAY, va="top", ha="left")
    # THE BAND BELOW THE PANELS, anchored in INCHES from the figure bottom rather than in figure fractions,
    # for the reason render() gives for its legend: the figure grows with the gateway count, so a fraction
    # that clears the legend on a 4-row grid overlaps it on a 2-row one. Four stacked items, bottom-up:
    # footer, language legend, the floor-marker note, the x-axis caption.
    def _at(inches):
        return inches / fig_h
    fig.text(0.5, _at(1.02), "p99 tail-latency bound the reading was taken under (ms; \"none\" = no latency "
             "bound, i.e. how much it carries before it fails requests)",
             fontsize=9, color=GRAY, ha="center")
    fig.text(0.5, _at(0.74), "▲ = a FLOOR, not a ceiling: the sweep's top rung won, so no maximum was "
             "established and the true rate is at or above the point drawn",
             fontsize=8.5, color=GRAY, ha="center")
    from matplotlib.patches import Patch
    # ONLY THE LANGUAGES ACTUALLY DRAWN. Keyed on every row, the legend advertised five colours on a board
    # where one gateway had a curve - a key to marks that are not on the page.
    drawn = {r["_key"] for r in rows if any(v is not None for v in curves[r["_key"]])}
    present = [l for l in LANG_ORDER if any(LANGS.get(k) == l for k in drawn)]
    if present:
        fig.legend(handles=[Patch(facecolor=LANG_COLORS[l], label=l) for l in present],
                   loc="lower center", bbox_to_anchor=(0.5, _at(0.20)), fontsize=8.5, frameon=False,
                   ncols=min(len(present), 6), title="colored by language")
    fig.text(0.008, _at(0.04), "req/s   ·   curves colored by implementation language     "
             f"github.com/GetBusbar/benchmarking - regenerated {RENDER_TS} from raw results",
             fontsize=7.3, color=GRAY)
    fig.tight_layout(rect=(0, _at(1.25), 1, 0.93))
    out = RESULTS / f"{out_stem or FRONTIER_SHAPE_CHART}.png"
    fig.savefig(out, dpi=300, bbox_inches="tight", facecolor="white")
    plt.close(fig)
    print(f"wrote {out}")


# ── THE CLIMB: rps vs CONCURRENCY, with the tail beside it ─────────────────────────────────────────
FRONTIER_CLIMB_CHART = "frontier_climb"


# WHY TWO LINES IN ONE PANEL RATHER THAN TWO PANELS PER GATEWAY.
#
# The finding this chart exists for is a RELATIONSHIP between two quantities: one-api's rate is flat from
# c=1 (29 → 42 req/s) while its tail goes from 37 ms to 3.4 s. Split across two stacked panels that is two
# separate shapes the reader has to hold in mind and align by eye; in one panel it is a single picture - a
# horizontal line and a rising one crossing the same space - and "it buys nothing with concurrency except
# latency" needs no caption. 28 panels would also halve the size of each.
#
# The cost is a twin y axis, which is a real hazard: two scales in one frame invites reading the crossing
# point as meaningful when it is an artifact of where each axis starts. It is mitigated the only way that
# works - the two series are encoded differently in EVERY channel (solid vs dashed, saturated colour vs
# grey, left axis vs right, both labelled with their unit), and the axis ranges are IDENTICAL on all
# panels, so a crossing means the same thing in every panel even if it means nothing on its own.
#
# BOTH AXES LOG. Concurrency because the ladder doubles (a linear axis would crush c=1..8, where the whole
# early-saturation story is). Rate because the field spans 29 req/s to 25,290 req/s - three decades - and a
# shared linear rate axis would flatten every gateway except the top two into the baseline. On log-log,
# PERFECT LINEAR SCALING IS A 45° LINE, which is what makes the reference line readable as an angle rather
# than a value, and a plateau reads as a bend to horizontal.
def render_frontier_climb(out_stem: str | None = None) -> None:
    rows = _load("perf")
    if _mpl() is None:
        return  # no matplotlib - reports still generate from JSON
    climbs = {r["_key"]: _climb_points(r.get("_sweep") or []) for r in rows}
    # A CLIMB NEEDS TWO RUNGS. One point is not a shape, and a grid of single dots captioned as a
    # throughput-vs-concurrency comparison would be a picture of nothing presented as a measurement.
    drawable = [r for r in rows if len([p for p in climbs[r["_key"]] if p["rps"] is not None]) >= 2]
    if not drawable:
        print(f"skip {FRONTIER_CLIMB_CHART}: no gateway has two probed concurrencies yet "
              f"(a climb needs two points - not drawing an empty chart as if it were data)")
        return

    n = len(rows)
    ncols = min(4, n) or 1
    nrows = (n + ncols - 1) // ncols
    fig_h = 2.55 * nrows + 2.25
    fig, axes = plt.subplots(nrows, ncols, figsize=(3.35 * ncols + 0.9, fig_h), sharex=True, sharey=True)
    fig.patch.set_facecolor("white")
    axes = list(axes.flat) if hasattr(axes, "flat") else [axes]

    allp = [p for r in rows for p in climbs[r["_key"]]]
    rates = [p["rps"] for p in allp if p["rps"]]
    tails = [p["p99_us"] for p in allp if p["p99_us"]]
    concs = [p["conc"] for p in allp]
    # The rate axis spans the DATA. The reference line starts ABOVE the whole field (the mock answers in
    # ~30 µs, so one connection's zero-overhead rate is ~31,000 req/s - already above the best gateway's
    # peak) and is left to run off the top rather than being given room, because giving it room would
    # compress every real curve into the bottom of its panel to display a line that is not a measurement.
    ymin, ymax = (min(rates) * 0.55, max(rates) * 1.9) if rates else (1, 10)
    tmin, tmax = (min(tails) * 0.55, max(tails) * 2.4) if tails else (1, 10)
    xmin, xmax = (min(concs) * 0.7, max(concs) * 1.45) if concs else (1, 2)
    TAIL = "#6b7280"

    for i, (ax, r) in enumerate(zip(axes, rows)):
        ax.set_facecolor("white")
        col = LANG_COLORS.get(LANGS.get(r["_key"], ""), LANG_DEFAULT)
        pts = climbs[r["_key"]]
        s = _climb_summary(pts)
        ax.set_xscale("log", base=2)
        ax.set_yscale("log")
        ax.set_xlim(xmin, xmax)
        ax.set_ylim(ymin, ymax)
        ax.xaxis.grid(True, color=GRID, zorder=0)
        ax.yaxis.grid(True, color=GRID, zorder=0)
        ax.set_axisbelow(True)
        for sp in ("top", "right"):
            ax.spines[sp].set_visible(False)
        for sp in ("bottom", "left"):
            ax.spines[sp].set_color("#d7dae0")

        ax2 = ax.twinx()          # the TAIL, on its own scale, identical on every panel
        ax2.set_yscale("log")
        ax2.set_ylim(tmin, tmax)
        for sp in ("top", "left"):
            ax2.spines[sp].set_visible(False)
        ax2.spines["right"].set_color("#e3e5ea")
        ax2.spines["bottom"].set_color("#d7dae0")

        if s is None:
            # NO CAUSE IS INVENTED. There is no sweep on this record and the record says nothing about why,
            # so the panel says only that. (`✕` is avoided in PNG text: the bundled Inter faces have no
            # MULTIPLICATION X glyph and it renders as a tofu box.) The axes come off with it, for the same
            # reason as on the shape chart: a log scale drawn around empty space invites a reader to place
            # the missing curve somewhere on it.
            ax.annotate("no concurrency sweep in this record", xy=(0.5, 0.5), xycoords="axes fraction",
                        fontsize=8.5, color=GRAY, ha="center", va="center")
            # Per-axes, never set_xticks/set_yticks: those locators are SHARED across a sharex/sharey grid,
            # so clearing them on one empty panel strips the axes off every panel that does have a curve.
            for _a in (ax, ax2):
                _a.tick_params(which="both", labelleft=False, labelright=False, labelbottom=False,
                               length=0)
                _a.grid(False)
                for sp in _a.spines.values():
                    sp.set_visible(False)
        else:
            # THE ZERO-OVERHEAD REFERENCE, clipped at the highest concurrency actually probed (honesty
            # constraint 2: the model knows nothing about the mock's own saturation or the box's cores, so
            # it must not be drawn past the range we looked at).
            dc = r.get("_direct_c1_p99_us")
            ideal = [(p["conc"], _ideal_rps(dc, p["conc"])) for p in pts]
            ideal = [(c, v) for c, v in ideal if v is not None]
            if ideal:
                ax.plot([c for c, _ in ideal], [v for _, v in ideal], color="#111827", lw=1.0,
                        ls=(0, (1, 2)), zorder=5, alpha=0.55)
            # Every WINDOW behind the median line, so the dispersion is visible rather than averaged away:
            # one-api's tail at c=2 spans 148 ms to 848 ms across three windows, which is itself a finding
            # about how repeatable the gateway is, and a lone median asserts a steadiness it did not show.
            for p in pts:
                for w in p["windows"]:
                    ax.plot([w["conc"]], [w["rps"]], marker="o", ms=2.2, color=col, alpha=0.33, zorder=3)
                    if w["p99_us"]:
                        ax2.plot([w["conc"]], [w["p99_us"]], marker="o", ms=2.0, color=TAIL,
                                 alpha=0.28, zorder=3)
            xs = [p["conc"] for p in pts if p["rps"] is not None]
            ys = [p["rps"] for p in pts if p["rps"] is not None]
            ax.plot(xs, ys, color=col, lw=2.0, zorder=6, solid_capstyle="round")
            tx = [p["conc"] for p in pts if p["p99_us"]]
            ty = [p["p99_us"] for p in pts if p["p99_us"]]
            if tx:
                ax2.plot(tx, ty, color=TAIL, lw=1.5, ls=(0, (4, 2)), zorder=5)
            # WHERE THE CLIMB ENDED, marked: a rung that failed a request it accepted qualifies for no
            # frontier reading at any bound, so rate measured at or above it is not throughput the gateway
            # is entitled to. Everything to the right of this rule is outside what the board will publish.
            if s["c_first_fail"] is not None:
                ax.axvline(s["c_first_fail"], color="#c2410c", lw=1.0, ls=(0, (2, 2)), zorder=4, alpha=0.8)
                ax.annotate(f"fails from c={s['c_first_fail']:g}", xy=(s["c_first_fail"], ymax),
                            xytext=(2, -8), textcoords="offset points", fontsize=7.2,
                            color="#c2410c", ha="left", va="top", zorder=7)
            # The saturation point - where the rate first reached _SATURATION_FRAC of its own peak - which
            # is the "peaked early" fact. The PEAK's concurrency is the wrong number for it: agentgateway's
            # highest median rate is at c=256 having been within 7% of it since c=8.
            if s["c_sat"] < s["c_top"]:
                ax.plot([s["c_sat"]], [next(p["rps"] for p in pts if p["conc"] == s["c_sat"])],
                        marker="|", ms=13, mew=1.8, color=INK, zorder=7)

        ax.set_title(r["_label"], fontsize=10.5, fontweight="bold", color=INK, loc="left", pad=21)
        if s is not None:
            # THE PANEL'S SENTENCE, and every number in it is a column of the climb table below the chart,
            # so the picture is re-derivable from the artifact beside it.
            g = f"{s['gain']:.1f}×" if s["gain"] else "?"
            cg = f"{s['conc_gain']:.0f}×" if s["conc_gain"] else "?"
            line1 = (f"{_fmt(s['rps_first'])} at c={s['c_first']:g} → {_fmt(s['rps_peak'])} peak "
                     f"({g} the rate for {cg} the concurrency)")
            line2 = (f"95% of peak by c={s['c_sat']:g}"
                     + (f"  ·  tail {_us(s['p99_first'])} → {_us(s['p99_top'])}"
                        if s["p99_first"] and s["p99_top"] else ""))
            ax.annotate(line1 + "\n" + line2, xy=(0, 1), xycoords="axes fraction", xytext=(0, 2),
                        textcoords="offset points", fontsize=7.6, color=GRAY, va="bottom", ha="left")

        from matplotlib.ticker import FuncFormatter, NullFormatter
        ax.xaxis.set_major_formatter(FuncFormatter(lambda v, _p: f"{v:,.0f}"))
        ax.yaxis.set_major_formatter(FuncFormatter(lambda v, _p: _fmt(v)))
        ax.yaxis.set_minor_formatter(NullFormatter())
        ax2.yaxis.set_major_formatter(FuncFormatter(lambda v, _p: _us(v)))
        ax2.yaxis.set_minor_formatter(NullFormatter())
        # Rate labels on the leftmost column, tail labels on the rightmost, x labels wherever nothing sits
        # below - the same rule the shape chart uses, for the same reason (sharex/sharey hide the labels on
        # the bottom-most panel of every short column on a partly filled last row).
        # AXIS LABELS IN NEUTRAL INK, NOT THE LANGUAGE COLOUR. Tinting them per gateway made the Node
        # panel's amber tick labels near-illegible on white and implied the SCALE differed per panel when
        # it is identical on all of them. Which series is which is carried by solid-vs-dashed and by the
        # left/right unit labels, which survive greyscale; a tick colour cannot carry it and should not try.
        ax.tick_params(labelsize=7.6, colors=INK, length=2, labelbottom=(i + ncols >= n),
                       labelleft=(i % ncols == 0))
        ax.tick_params(axis="x", colors=GRAY)
        ax2.tick_params(labelsize=7.6, colors=TAIL, length=2,
                        labelright=(i % ncols == ncols - 1 or i == n - 1))
    for ax in axes[n:]:
        ax.set_visible(False)

    fig.suptitle("The climb: what each gateway does as concurrency doubles",
                 fontsize=15, fontweight="bold", color=INK, x=0.008, ha="left", y=0.995)

    def _at(inches):
        return inches / fig_h
    fig.text(0.008, 1 - _at(0.52),
             "every rung of the same concurrency sweep the frontier readings are taken from. SOLID = req/s "
             "(left axis, coloured); DASHED GREY = p99 tail (right axis). Both axes log, and identical on "
             "every panel. Faint dots are the individual measurement windows behind each median.",
             fontsize=9.5, color=GRAY, va="top", ha="left")
    fig.text(0.5, _at(2.56), "concurrency (log₂ - the ladder doubles)", fontsize=9, color=GRAY,
             ha="center")
    # THE REFERENCE LINE'S CAPTION CARRIES BOTH HONESTY CONSTRAINTS, and the first one is QUANTIFIED off
    # the board rather than hedged in words: Little's Law is stated in MEAN latency, we publish
    # percentiles, and on the cells where both halves are published (gateway_c1_p99_us against the measured
    # c=1 rate) the p99-based form UNDER-predicts the measured rate by 17% on agentgateway and by 29x on
    # one-api. A reader who is told "conservative" cannot tell those apart; a reader who is told the range
    # knows the line is a direction and not a limit.
    # WRAPPED BY HAND, because `bbox_inches="tight"` grows the SAVED image to fit the widest artist on it -
    # so one long centred sentence stretched the canvas well past the panel grid and left the grid marooned
    # in the middle of a very wide picture. Explicit newlines keep the text block narrower than the panels.
    # No box-drawing characters anywhere: the bundled Inter faces have no BOX DRAWINGS LIGHT VERTICAL and it
    # renders as a tofu box, so the tick-style saturation marker is described in words.
    fig.text(0.5, _at(2.30),
             "DOTTED = the zero-overhead reference: Little's Law (rps = c / s) on this cell's own measured "
             "direct-to-mock p99. A REFERENCE, NOT A LIMIT -\n"
             "Little's Law is stated in MEAN latency and this is drawn from a PERCENTILE; where both halves "
             "are published, that form under-predicts the\n"
             "measured c=1 rate by 17% (agentgateway) to 29x (one-api), so a curve near or above it is not "
             "a defect.",
             fontsize=8.2, color=GRAY, ha="center", va="top", linespacing=1.55)
    fig.text(0.5, _at(1.50),
             "It knows nothing of the mock's own saturation or the box's four cores, so it is clipped at the "
             "highest concurrency actually probed, and it is\n"
             "expected to leave the top of every panel: the mock answers in ~30 us, so one connection's "
             "zero-overhead rate already exceeds the best\n"
             "gateway's peak. That gap is what the gateway costs.     .     The short vertical tick on a "
             f"rate curve marks where it first reached {_SATURATION_FRAC:.0%} of its own peak.",
             fontsize=8.2, color=GRAY, ha="center", va="top", linespacing=1.55)
    from matplotlib.patches import Patch
    # Only the languages of gateways that actually drew a curve - a legend to marks that are on the page.
    present = [l for l in LANG_ORDER if any(LANGS.get(r["_key"]) == l for r in drawable)]
    if present:
        fig.legend(handles=[Patch(facecolor=LANG_COLORS[l], label=l) for l in present],
                   loc="lower center", bbox_to_anchor=(0.5, _at(0.16)), fontsize=8.5, frameon=False,
                   ncols=min(len(present), 6), title="rate curve colored by language")
    fig.text(0.008, _at(0.03), "req/s and p99 per concurrency rung     "
             f"github.com/GetBusbar/benchmarking - regenerated {RENDER_TS} from raw results",
             fontsize=7.3, color=GRAY)
    # The header band is two lines of prose, so it needs ~0.85 in, not the 1.15 in that left a visible
    # empty stripe between the subtitle and the first row of panels.
    fig.tight_layout(rect=(0, _at(2.92), 1, 1 - _at(0.85)))
    out = RESULTS / f"{out_stem or FRONTIER_CLIMB_CHART}.png"
    fig.savefig(out, dpi=300, bbox_inches="tight", facecolor="white")
    plt.close(fig)
    print(f"wrote {out}")


# ── the READING KEY for the climb: an ILLUSTRATIVE diagram, not a measurement ──────────────────────
FRONTIER_SHAPES_KEY_CHART = "frontier_shapes_key"

# WHY A DIAGRAM IS WORTH THE RISK, AND HOW THE RISK IS CONTAINED.
#
# Fourteen real climb curves are a lot to read cold, and the vocabulary for them ("peaked early", "flat
# rate while the tail explodes") is not something a reader can be assumed to arrive with. Four drawn
# archetypes teach it in one glance and make the real panels legible.
#
# The risk is obvious and serious: a hand-drawn curve in the visual language of the measured charts, on a
# board whose entire premise is that every number is a measurement, is one crop away from being quoted as
# data. It is contained by making the diagram UNABLE to pass as a measurement rather than merely labelled
# as one: no axis numbers at all (nothing to misread as a rate or a concurrency), a tinted background no
# measured panel uses, "ILLUSTRATIVE" in the title, and a footer saying it in full. The curves are declared
# right here as plain shape definitions, so there is no data path into this function at all.
#
# The archetypes are named from the field, not invented: each caption cites the gateway on the current
# board that exhibits it, so a reader can go straight from the shape to a real panel.
_CLIMB_ARCHETYPES = [
    ("Ideal", [0.06, 0.22, 0.45, 0.72, 0.90, 0.94, 0.95, 0.95],
     [0.05, 0.06, 0.08, 0.12, 0.20, 0.34, 0.55, 0.80],
     "rate rises with concurrency, then plateaus cleanly;\ntail stays low until the plateau"),
    ("Peaks early", [0.30, 0.62, 0.80, 0.83, 0.84, 0.84, 0.84, 0.84],
     [0.06, 0.10, 0.18, 0.32, 0.50, 0.68, 0.82, 0.92],
     "most of its rate arrives in the first doublings;\nextra concurrency buys tail, not throughput"),
    ("Slow climb", [0.04, 0.07, 0.13, 0.24, 0.40, 0.58, 0.74, 0.86],
     [0.30, 0.36, 0.43, 0.50, 0.58, 0.66, 0.74, 0.82],
     "never reaches a plateau inside the range probed;\nthe peak is where we stopped asking"),
    ("Flat, tail explodes", [0.10, 0.115, 0.125, 0.13, 0.133, 0.135, 0.136, 0.136],
     [0.10, 0.30, 0.52, 0.70, 0.82, 0.90, 0.95, 0.98],
     "concurrency buys nothing but latency;\nthe rate line is already horizontal at c=1"),
]


def render_frontier_shapes_key(out_stem: str | None = None) -> None:
    if _mpl() is None:
        return
    fig, axes = plt.subplots(1, len(_CLIMB_ARCHETYPES), figsize=(3.05 * len(_CLIMB_ARCHETYPES), 3.9))
    fig.patch.set_facecolor("white")
    axes = list(axes.flat) if hasattr(axes, "flat") else [axes]
    for ax, (name, rate, tail, note) in zip(axes, _CLIMB_ARCHETYPES):
        # A TINTED PANEL, which no measured chart in this file uses: the background alone says "not data"
        # before a single label is read.
        ax.set_facecolor("#f7f4ee")
        xs = list(range(len(rate)))
        ax.plot(xs, rate, color=INK, lw=2.2, zorder=4, solid_capstyle="round")
        ax.plot(xs, tail, color="#9aa2b2", lw=1.6, ls=(0, (4, 2)), zorder=3)
        ax.set_xlim(-0.3, len(rate) - 0.7)
        ax.set_ylim(0, 1.06)
        # NO NUMBERS ON EITHER AXIS. There is nothing here to read as a rate or a concurrency, which is the
        # structural half of "this is not a measurement" - a labelled axis would invite exactly the misuse
        # the wording is trying to prevent.
        ax.set_xticks([])
        ax.set_yticks([])
        for sp in ax.spines.values():
            sp.set_visible(False)
        ax.set_title(name, fontsize=11, fontweight="bold", color=INK, loc="left", pad=8)
        ax.annotate(note, xy=(0, 0), xycoords="axes fraction", xytext=(0, -14),
                    textcoords="offset points", fontsize=8.2, color=GRAY, va="top", ha="left")
    fig.suptitle("ILLUSTRATIVE - how to read the climb charts (drawn shapes, not measurements)",
                 fontsize=13, fontweight="bold", color=INK, x=0.008, ha="left", y=0.99)
    # THE SERIES KEY ONCE, IN THE HEADER. Per-panel it sat on top of the curves it was describing, which is
    # the one thing a four-panel reading key cannot afford to do.
    fig.text(0.008, 0.905, "solid = req/s     ·     dashed = p99 tail     ·     concurrency increases "
             "left to right", fontsize=9, color=GRAY, va="top", ha="left")
    fig.text(0.008, 0.055,
             "These four curves are DRAWN BY HAND to name the shapes, and contain no data: the axes carry "
             "no numbers because there are none to carry. On the measured climb chart, concurrency runs "
             "left to right on a log₂ axis and both series have real units.",
             fontsize=8.2, color=GRAY, va="bottom", ha="left")
    fig.text(0.008, 0.012, "ILLUSTRATIVE DIAGRAM - NOT MEASURED DATA     "
             f"github.com/GetBusbar/benchmarking - regenerated {RENDER_TS}",
             fontsize=7.3, color=GRAY)
    fig.tight_layout(rect=(0, 0.22, 1, 0.84))
    out = RESULTS / f"{out_stem or FRONTIER_SHAPES_KEY_CHART}.png"
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


def _rate_cell(rd) -> str:
    """One frontier reading as a table cell: the rate, a floor as a floor, an absence with its own reason.

    Shared by the per-bound table and the default-bound column so the same reading cannot render two ways
    on one page - which is the whole failure mode the frontier replaced (two summaries of one sweep,
    disagreeing)."""
    if rd is None:
        return "n/a"
    if rd["rps"] is None:
        cause = _ABSENCE_CAUSE.get(rd["reason"] or "")
        return f"✕ {cause}" if cause else "✕ not measured"
    if not rd["rps"]:
        return "0"
    return ("≥ " if rd["lower_bound"] else "") + f"{int(rd['rps']):,}"


def _frontier_table(rows: list) -> list:
    """The frontier at EVERY published bound, one row per gateway - the artifact the shape chart is drawn
    from.

    The chart shows flatness; this shows the numbers whose ratio flatness IS. A reader who wants to rank at
    a bound the board does not lead with does it here, without taking the picture's word for the shape.
    """
    have = [(k, r) for k, r in rows if (r.get("_frontier") or [])]
    if not have:
        # NOT AN EMPTY TABLE. Fourteen rows of "n/a" across six columns is noise that looks like a result;
        # one sentence states the same fact, and it states it about the RECORDS rather than about the
        # gateways - no cause is asserted, because none is given.
        return ["**The frontier, bound by bound.** No gateway's record carries a frontier reading yet, so "
                "there is no per-bound table to publish. The concurrency sweep every reading is derived "
                "from is on disk and is tabled below.", ""]
    bounds = FRONTIER_BOUNDS_MS + [None]
    out = ["## The frontier: throughput at each tail you accept", "",
           "The most req/s each gateway carried while 99% of requests finished under the column's bound "
           "**and it failed none it accepted**. Reading left to right is the tradeoff: a row that barely "
           "changes gives you its full rate at a tight tail, and a row that climbs steeply is buying "
           "throughput with latency. The last column applies no latency bound at all, so it answers only "
           "\"how much before it starts failing requests\". Rates are non-decreasing left to right by "
           "construction - relaxing a bound can only add qualifying rungs, never remove one.", ""]
    head = " | ".join(f"p99 &lt; {b:g} ms" if b is not None else "no bound" for b in bounds)
    out.append(f"| Gateway | {head} | at {DEFAULT_BOUND_MS:g} ms: concurrency, observed tail |")
    out.append("|---|" + "--:|" * len(bounds) + "---|")
    for key, r in have:
        f = r["_frontier"]
        cells = " | ".join(_rate_cell(_frontier_at(f, b)) for b in bounds)
        # THE EVIDENCE FOR THE COLUMN THE BOARD LEADS WITH: the concurrency the winning rate was observed
        # at and the tail it ACTUALLY produced (not the bound - "held 0.6 ms under a 10 ms bound" and "sat
        # at 9.9 ms" are different findings), plus the concurrency above it that stopped qualifying, which
        # is the other half of the proof that this is the most it can do under this bound.
        rd = _frontier_at(f, DEFAULT_BOUND_MS)
        ev = "-"
        if rd and rd["rps"] is not None:
            bits = []
            if rd["concurrency"] is not None:
                bits.append(f"c={rd['concurrency']:g}")
            if rd["p99_us"] is not None:
                bits.append(f"p99 {_us(rd['p99_us'])}")
            if rd["first_disqualified_conc"] is not None:
                bits.append(f"c={rd['first_disqualified_conc']:g} broke it")
            if rd["lower_bound"]:
                bits.append("floor: no ceiling established")
            ev = ", ".join(bits) or "-"
        out.append(f"| {_linked(key)} | {cells} | {ev} |")
    out.append("")
    out.append("**≥** = the sweep's top rung won, so that rate is a **floor** and no ceiling was "
               "established. **0** = the sweep ran and no rung held that bound while failing nothing. "
               "**n/a** = the record carries no reading at that bound. A **✕** cell names the record's own "
               "reason for the absence.")
    out.append("")
    return out


def _climb_table(rows: list) -> list:
    """The climb as numbers: where each gateway starts, where it saturates, what the tail did, where it
    began failing. The artifact behind every sentence printed on a climb panel."""
    have = [(k, s) for k, r in rows if (s := _climb_summary(_climb_points(r.get("_sweep") or [])))]
    if not have:
        return ["**The climb.** No gateway's record carries a concurrency sweep, so there is no climb to "
                "table.", ""]
    out = ["## The climb: what each gateway does as concurrency doubles", "",
           "Every rung of the same sweep the frontier readings above are taken from, summarised. This is "
           "where \"started low, took forever to climb, peaked early\" is a number rather than an "
           "impression: **gain** is what the whole climb bought over the first rung, and **saturates** is "
           f"the first concurrency reaching {_SATURATION_FRAC:.0%} of the gateway's own peak - which is the "
           "honest \"peaked early\" figure, since a peak's own concurrency can sit far above where the "
           "climb effectively ended. Rate figures are the median of the windows probed at that "
           "concurrency; the chart draws every window behind the median.", ""]
    out.append("| Gateway | req/s at lowest c | peak req/s (at c) | gain (rate × / concurrency ×) "
               f"| saturates ({_SATURATION_FRAC:.0%} of peak) | p99 at lowest c → at top c "
               "| first c that failed a request | top c probed |")
    out.append("|---|--:|--:|--:|--:|--:|--:|--:|")
    for key, s in have:
        gain = (f"{s['gain']:.1f}× / {s['conc_gain']:.0f}×"
                if s["gain"] and s["conc_gain"] else "-")
        # A GATEWAY THAT NEVER FAILED A REQUEST SAYS SO, rather than printing a dash a reader has to guess
        # at: "none" is a measured result across the whole ladder, and it is the good one.
        fail = f"c={s['c_first_fail']:g}" if s["c_first_fail"] is not None else "none"
        out.append(
            f"| {_linked(key)} | {int(round(s['rps_first'])):,} at c={s['c_first']:g} "
            f"| {int(round(s['rps_peak'])):,} at c={s['c_peak']:g} "
            f"| {gain} "
            f"| c={s['c_sat']:g} "
            f"| {_us(s['p99_first'])} → {_us(s['p99_top'])} "
            f"| {fail} "
            f"| c={s['c_top']:g} |")
    out.append("")
    out.append("A rung that failed a request it had accepted qualifies for **no** frontier reading at any "
               "bound, so rate measured at or above the failing concurrency is not throughput the board "
               "will publish - the climb chart rules that region off. **none** in that column is a "
               "measured result across the whole ladder, not a missing one.")
    out.append("")
    return out


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
    # ONE THROUGHPUT COLUMN, AT A BOUND THE HEADER NAMES.
    #
    # DELETED: "Sustained RPS (20 ms upstream)" and "Max proxy RPS", the two scalar-throughput columns,
    # with their `rps_cell` renderer and the two-throughput-numbers paragraph that explained them. They
    # were two collapses of one sweep and they contradicted each other in the field (a "maximum" below its
    # own sustained figure - see the note in CHARTS). The header renders the bound from DEFAULT_BOUND_MS,
    # so this column cannot come to describe a bound it was not read at; the SHAPE across all six bounds is
    # in the frontier-shape chart, and the per-row "+n% unbounded" suffix below is its one-cell summary.
    lines.append(f"| Gateway | Added latency (p99) | req/s @ p99 &lt; {DEFAULT_BOUND_MS:g} ms, zero failures "
                 f"| Idle RAM | Steady-state RAM | Built |")
    lines.append("|---|--:|--:|--:|--:|---|")

    zero_load_seen = False
    dnf_seen = False
    no_reading_seen = False
    fail_notes = []  # (gateway, serve_error) for every ❌ row - the receipt behind "did not serve"

    def frontier_cell(r, served):
        """This row's frontier reading at DEFAULT_BOUND_MS, as a table cell.

        Five distinct states, and the point of the cell is that they stay distinct:
          ✕            - never served under load (the perf verdict, as the old column rendered it)
          -            - no perf record beside this row at all (a memory-only row); `served` is None
          n/a          - a record with NO frontier reading for this bound. NOT a zero: a pre-frontier
                         snapshot measured no throughput, which is not the same as carrying none.
          ✕ <cause>    - a reading exists and its rate is absent, captioned with the RECORD'S OWN reason
                         (_ABSENCE_CAUSE), never a blanket "rig-limited".
          0            - measured: no rung held this bound while failing nothing.
        """
        nonlocal no_reading_seen
        if served is False:
            return "✕"
        # NEITHER SERVED NOR NOT-SERVED. `served` is None on a row assembled from a memory record with no
        # perf record beside it, and the old renderer fell through to `not val` and printed a bare "0" - a
        # measured zero for a gateway whose throughput was never measured at all.
        if served is None:
            return "-"
        f = r.get("_frontier") or []
        rd = _frontier_at(f, DEFAULT_BOUND_MS)
        if rd is None:
            no_reading_seen = True
            return "n/a"
        # THE RATE ITSELF THROUGH THE SHARED RENDERER (_rate_cell): floors as floors, absences with the
        # record's own reason, a measured 0 as a number. Written twice it would be one edit away from the
        # leaderboard column and the per-bound table disagreeing about the same reading.
        cell = _rate_cell(rd)
        if rd["rps"] is None or not rd["rps"]:
            return cell
        # THE SHAPE, IN THE ONE CELL THE TABLE HAS FOR IT. Two gateways can print the same number here and
        # be nothing alike: on the 2026-07-29 board agentgateway gained 7% from dropping the bound entirely
        # and apisix nearly doubled between 1 ms and 5 ms. The suffix names where it starts from.
        g = _frontier_gain(f)
        if g is not None:
            lo, gain = g
            cell += f" <sub>({gain:+.0%} from {lo:g} ms to no bound)</sub>"
        return cell

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
        rps = frontier_cell(r, served)
        # A MEASURED 0 AT THIS BOUND, which is what the "0" legend entry below is about - read off the
        # reading itself rather than off a scalar that no longer exists. It means the sweep ran and no rung
        # held this bound while failing nothing; it is a number, and it is not an absence.
        _rd = _frontier_at(r.get("_frontier") or [], DEFAULT_BOUND_MS)
        if served is not False and _rd is not None and _rd["rps"] == 0:
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
            f"| {rps} "
            f"| {rss_cell(idle)} "
            f"| {rss_cell(peak)} "
            f"| `{(r.get('build') or '').strip()[:46]}` |"
        )
    # Gateways we intend to measure but haven't yet - shown so the field is transparent, never hidden.
    for key in pending:
        lines.append(
            f"| {_linked(key)} | ⏳ *pending* | - | - | - | *pending measurement* |"
        )
    lines.append("")
    if pending:
        names = ", ".join(GATEWAYS[k] for k in pending)
        lines.append(f"⏳ **Pending measurement** (a manifest exists; not yet run on the rig): {names}. "
                     "These land here as their runs complete - nothing is hidden.")
        lines.append("")
    # ONE THROUGHPUT NUMBER, AND WHAT IT IS. This paragraph read "Two throughput numbers: max proxy RPS
    # (instant upstream - raw forwarding speed) and sustained RPS under a 20 ms upstream delay"; both
    # metrics are retired (see the note in CHARTS). The replacement states the bound, states that the
    # bound is one of several published, and points at the chart where the tradeoff is visible - because
    # ONE number out of a curve is exactly what made the old columns misleading.
    lines.append(f"**Throughput is a curve, not a number.** The column above is one reading of each "
                 f"gateway's concurrency sweep: the most req/s it carried while 99% of requests finished "
                 f"under **{DEFAULT_BOUND_MS:g} ms** and it failed **none** it accepted. The same sweep is "
                 f"published at {len(FRONTIER_BOUNDS_MS)} tail-latency bounds "
                 f"({', '.join(f'{b:g} ms' for b in FRONTIER_BOUNDS_MS)}) plus with no bound at all, and "
                 f"the shape across them is the comparison that matters: a gateway already at its ceiling "
                 f"at 1 ms is a different machine from one that doubles when given 5 ms. See the "
                 f"frontier-shape chart. **≥** on a number means the sweep's top rung won, so that rate is "
                 f"a floor and no ceiling was established.")
    legend = []
    zero_or_x = any(True for _, r in rows if r.get("served") is False) or zero_load_seen
    if zero_or_x:
        legend.append("**✕** = did not serve under load (0 successful req/s).")
        # THE BOUND THIS 0 IS ABOUT, from the constant the column was read at. This read "no tested
        # concurrency held p99 < 1 s with <0.1% errors" - two claims, neither of them the test: the
        # retired gate was 20 ms (a 1 s bar passes 96% of all 1632 recorded rungs against 57% for the real
        # one), and the frontier grants no error tolerance at all - a rung qualifies only if the gateway
        # failed nothing it accepted.
        legend.append(f"**0** = came up, but no tested concurrency held p99 &lt; {DEFAULT_BOUND_MS:g} ms "
                      "while failing none of the requests it accepted.")
    if no_reading_seen:
        # NAMES NO CAUSE. The record carries no reading for this bound and gives no reason for it, so this
        # says only that - it does not guess at a rig limit, a gateway failure or a stale snapshot.
        legend.append("**n/a** = this gateway's record carries no frontier reading at that bound "
                      "(distinct from a measured 0, which is a number).")
    if dnf_seen:
        legend.append("**†** = a concurrency-1 latency exists, but the gateway failed under load: "
                      "not a clean result.")
    # NO rig-limited legend. Nothing renders that marker any more: a throughput the harness could not
    # certify as gateway-limited is published with the fraction of our own rig's ceiling it reached, which
    # is the fact the marker was standing in for, and it needs no legend entry because it is a number.
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
    # ── THE TWO CURVES, AS TABLES ─────────────────────────────────────────────────────────────────
    # "Table + visuals, both": the visuals make 14 gateways comparable at a glance and the tables are the
    # checkable artifact. The rule these two enforce is that EVERY claim a panel makes is a column here, so
    # a reader who distrusts a curve can re-derive it - and a reader quoting a shape can cite a number.
    lines.extend(_frontier_table(rows))
    lines.extend(_climb_table(rows))
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
        # The translated-throughput header names its bound from the same constant the passthrough column
        # uses, so the two are read at the same operating point and neither implies a bound it did not use.
        lines.append(f"| Gateway | Added TTFT (p99) | Added per-token (p99) | SSE streams "
                     f"| Translated req/s @ p99 &lt; {DEFAULT_BOUND_MS:g} ms, 20 ms model delay |")
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
                # Gate on the envelope carrying a value, matching the stream_sustained PNG
                # (served_field=stream_sustained_valid) and the site drawer, so two published surfaces
                # cannot diverge from the same record.
                #
                # AND NAME THE RECORD'S OWN CAUSE. This printed "not measured (rig-limited)" for EVERY
                # absence of this figure. `stream_sustained_valid` is `sust is not None`, true only when a
                # value exists - so search_exhausted ("still climbing when the range ran out"),
                # harness_error, and a not_measured whose own detail says "whether this was rig-bound is
                # unknown" all published as an assertion that our equipment was the limit. It is also now
                # wrong in every case it can fire: a streams figure near the mock's ceiling is published
                # with its headroom, so a null here can no longer BE rig-limited.
                if not s.get("stream_sustained_valid"):
                    _cause = _ABSENCE_CAUSE.get(s.get("_stream_sustained_streams_reason") or "")
                    streams = f"✕ {_cause}" if _cause else "✕ not measured"
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
            elif _frontier_at(x.get("_xlate_frontier") or [], DEFAULT_BOUND_MS) is None:
                # TRANSLATES, but its record carries no reading at this bound - and gives no reason, so
                # none is asserted. Distinct from "cannot translate" above and from a measured 0 below.
                # Spelled out in the cell rather than folded into the bare "n/a" the lane legend covers
                # ("that suite hasn't been run yet"), which is a different and here untrue explanation.
                xl = "n/a - no frontier reading at this bound"
            elif not x.get("xlate_rps_at_bound_valid"):
                # Gate on the reading carrying a rate, as the SSE-streams column above does and as the
                # translation PNG's served_field does. A legitimate measured 0 stays valid (shows "0").
                #
                # The cause comes from the record for the same reason as the streams column above: this
                # read "(rig-limited)" for every absence, and the validity flag is `rps is not None`,
                # which is false for every absence reason there is.
                _cause = _ABSENCE_CAUSE.get(x.get("_xlate_rps_at_bound_reason") or "")
                xl = f"✕ {_cause}" if _cause else "✕ not measured"
                if x.get("_xlate_ingress"):
                    xl += f" ({x['_xlate_ingress']} → {x['_xlate_egress']})"
                # Render the legacy-xlate-suite fallback marker from the source stamp, like every PNG.
                xl += _sweep_label({"sweep": x.get("_xlate_source")})
            else:
                # "≥" on a floor, as everywhere else: a rate the sweep found no ceiling for is not a
                # maximum, and this column must not be the one surface that implies it is.
                xl = (("≥ " if x.get("_xlate_rps_at_bound_lower_bound") else "")
                      + f"{int(x.get('xlate_rps_at_bound') or 0):,}")
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
    # THE METHOD LINE STATES THE BOUND EACH NUMBER WAS READ AT, AND THE PRICE THE COST FIGURES DIVIDE BY.
    # It read "RPS ceiling = highest sustained req/s with p99 < 1 s and <0.1% errors" - the last of the 1 s
    # claims, and the one a reader most likely takes as the definition of every throughput number on the
    # page. No gate in the engine ever enforced 1 s (the retired one was 20 ms), and the frontier grants no
    # error tolerance whatsoever, so both halves of that sentence described a test that never ran.
    lines.append(f"Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; a frontier "
                 f"reading = the highest req/s any probed concurrency carried while 99% of requests "
                 f"finished under the STATED bound and the gateway failed none it accepted (readings are "
                 f"published at {', '.join(f'{b:g}' for b in FRONTIER_BOUNDS_MS)} ms and with no bound; the "
                 f"columns above use {DEFAULT_BOUND_MS:g} ms, and every caption names the bound it used); "
                 f"cost figures divide that {DEFAULT_BOUND_MS:g} ms reading by ${GATEWAY_HOURLY_USD:.4f}/hr "
                 f"for the pinned 4-core (m7g.xlarge) slice; RSS idle = after "
                 "first 200, steady state = the level the RSS settled at under load. Same box, same mock, "
                 "same load, one gateway "
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
    # THE TWO CURVE CHARTS LEAD, IN READING ORDER, because they are the headline: the SHAPE of a gateway's
    # behaviour is the comparison the retired scalars destroyed, and a reader who sees the ranked
    # single-bound bar first has already formed the conclusion these exist to complicate. The illustrative
    # reading key sits immediately before the climb it teaches - a reader meets the vocabulary
    # ("peaks early", "flat while the tail explodes") before the 14 real curves, not after. None of the
    # three has a top-5 variant (they are grids of small multiples, and the key is not data at all), so the
    # top5 page's `if png.exists()` gate simply skips them.
    charts = [FRONTIER_SHAPE_CHART, FRONTIER_SHAPES_KEY_CHART, FRONTIER_CLIMB_CHART] + \
             [c.name for c in CHARTS]
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


def _assert_no_silent_drop() -> None:
    """FAIL LOUDLY IF A MEASURED GATEWAY IS ABOUT TO BE DRAWN OUT OF EXISTENCE.

    Every chart on this page is projected from CANON (`site/data.json`), never from the snapshots on
    disk. That is deliberate - it is what keeps the PNGs and the site table from disagreeing - but it
    makes staleness INVISIBLE: if data.json was generated before a snapshot landed, that gateway has no
    `best_cell`, `_proj_perf` returns None, `_merge` skips it, and every chart plus the report table
    renders without it AND WITHOUT COMPLAINING. The count in the closing line goes from 8 to 7 and
    nothing says which one left.

    That is not hypothetical. It happened here: bifrost harvested at 20:52, data.json was from 20:44,
    and a full `python3 charts.py` printed "(7 gateways)" over a board with eight - six served cells with
    a valid frontier silently absent from every PNG. The pipeline order in cf-pages.yml (gen-data →
    charts → gen-data) is what normally prevents it, which means the protection lives entirely in the
    sequencing of a YAML file and nothing checks that it held.

    So compare the two sources and refuse to draw on disagreement. A snapshot with served cells whose
    gateway has no best_cell in CANON means CANON is stale, and the fix is to re-run gen-data - never to
    publish the smaller board. Raising beats warning: a warning in CI scrolls past, and the artifact it
    would have let through looks complete.
    """
    served_on_disk = {}
    for p in sorted((RESULTS / "snapshots").glob("*.json")):
        try:
            d = json.loads(p.read_text())
        except (OSError, ValueError):
            continue  # a snapshot mid-write during a live run is not a staleness signal
        key = d.get("gateway")
        if not key:
            continue
        n = sum(
            1
            for up in ((d.get("matrix") or {}).get("upstreams") or {}).values()
            for cell in (up.get("cells") or {}).values()
            if cell.get("served") is True
        )
        if n:
            served_on_disk[key] = n
    missing = {
        k: n for k, n in served_on_disk.items() if not (CANON.get(k) or {}).get("best_cell")
    }
    if missing:
        detail = ", ".join(f"{k} ({n} served cell(s))" for k, n in sorted(missing.items()))
        raise SystemExit(
            f"REFUSING TO DRAW A STALE BOARD: {detail} {'has' if len(missing) == 1 else 'have'} "
            f"measured results on disk but no best_cell in site/data.json, so "
            f"{'it' if len(missing) == 1 else 'they'} would be silently absent from every chart and "
            f"from the report table.\n"
            f"site/data.json was generated at {CANON_GENERATED_AT or 'an unrecorded time'}.\n"
            f"Run `node site/gen-data.mjs` first, then re-run this."
        )


def main() -> None:
    RESULTS.mkdir(exist_ok=True)
    # Before any rendering: the board we are about to draw must contain everything we measured.
    _assert_no_silent_drop()
    any_done = False
    # THE TWO CURVES AND THE KEY THAT TEACHES ONE OF THEM, each drawn by its own renderer rather than by the
    # bar machinery (see each function for why a Chart cannot express it). Both data charts skip themselves
    # when there is nothing to draw - no frontier readings, or fewer than two probed concurrencies - so a
    # partial bundle produces no misleading PNG. The shapes key is a diagram and always renders, because it
    # depends on no measurement.
    render_frontier_shape()
    render_frontier_shapes_key()
    render_frontier_climb()
    for c in CHARTS:
        render(c)                                       # full field → <name>.png
        # top-5 by THIS chart's OWN metric among rows with a valid value for it (audit HIGH), never a
        # single latency top-5 reused across every metric (which leaked invalid rows into a ranking).
        top5 = _topn_keys(c, 5)
        if top5:
            render(c, only_keys=top5, out_stem=f"top5_{c.name}")   # top-5 only → top5_<name>.png
        any_done = any_done or (RESULTS / f"{c.name}.png").exists()
    any_done = any_done or any((RESULTS / f"{c}.png").exists()
                               for c in (FRONTIER_SHAPE_CHART, FRONTIER_CLIMB_CHART))
    write_reports()
    if not any_done:
        print("no charts drawn - run the benchmark first (run-all.sh)")


if __name__ == "__main__":
    main()
