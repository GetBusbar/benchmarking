#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
"""Render benchmark charts from results/ — pretty, and pluggable.

Nothing is hard-coded: every number is read from results/<suite>/<gateway>.json (written by the
runners). Bars are colored by MEASUREMENT — green goes to whichever gateway measured best on the
metric, so if busbar loses, busbar isn't green.

Add a chart = append one `Chart(...)` to CHARTS below. Add a gateway = it shows up automatically
once it has a result file (label/order from GATEWAYS). Run after the benchmark:

    python3 charts.py
"""
from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

# matplotlib is imported lazily (in render) so the report pages can be generated with plain JSON even
# where matplotlib isn't installed. plt is filled in by _mpl().
plt = None

ROOT = Path(__file__).resolve().parent
RESULTS = ROOT / "results"

# ── house style ──────────────────────────────────────────────────────────────────────────────────
BRAND = "#00b34a"   # busbar green — the "won this metric" color
BRAND_DK = "#059142"
SLATE = "#3a3f4b"   # everyone else's primary bar
MUTE = "#cdd0d7"    # secondary/idle bars
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
    for _f in ("Inter", "Helvetica Neue", "Arial", "DejaVu Sans"):
        if any(_f.lower() in f.name.lower() for f in fm.fontManager.ttflist):
            _plt.rcParams["font.family"] = _f
            break
    _plt.rcParams.update({"axes.edgecolor": "#d7dae0", "svg.fonttype": "none"})
    plt = _plt
    return plt

# display order + labels. A gateway appears in a chart only if it has a result file this run.
GATEWAYS = {
    "busbar": "Busbar",
    "litellm-rust": "LiteLLM · Rust",
    "bifrost": "Bifrost",
    "portkey": "Portkey",
    "litellm-python": "LiteLLM · Python",
    "kong": "Kong",
    "helicone": "Helicone",
    "gomodel": "GoModel",
    "one-api": "One-API",
    "gptrouter": "GPTRouter",
    "arch": "Arch",
    "envoy-ai": "Envoy AI Gateway",
}


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


CHARTS = [
    # ── the headline: what the system can DO ──────────────────────────────────────────────────────
    Chart(
        name="added_latency",
        suite="perf",
        title="Added latency — what the gateway costs you",
        subtitle="p99 the gateway adds on top of the upstream, concurrency 1 (lower is better)",
        unit="µs",
        series=[Series("added_latency_p99_us", "p99 added latency", "rank")],
        log=True,
    ),
    Chart(
        name="rps_max_proxy",
        suite="perf",
        title="Max proxy throughput — raw forwarding speed",
        subtitle="highest sustained req/s with p99 < 1s, zero errors, instant upstream (higher is better)",
        unit="requests / sec",
        series=[Series("rps_max_proxy", "max proxy RPS", "rank")],
        higher_better=True,
    ),
    Chart(
        name="rps_sustained_20ms",
        suite="perf",
        title="Sustained throughput under 20 ms LLM latency",
        subtitle="AIGatewayBench's metric: req/s held with p99 < 1s + zero errors, 20 ms upstream (higher is better)",
        unit="requests / sec",
        series=[Series("rps_sustained_20ms", "sustained RPS @20ms", "rank")],
        higher_better=True,
    ),
    # ── supporting: memory (matters at scale) ─────────────────────────────────────────────────────
    Chart(
        name="memory_rss",
        suite="memory",
        title="Gateway memory under sustained load",
        subtitle="idle vs peak resident memory — same box, same mock, same load",
        unit="MiB",
        series=[
            Series("peak_rss_mib", "peak RSS (under load)", "rank"),
            Series("idle_rss_mib", "idle RSS (before load)", MUTE),
        ],
        log=True,
    ),
]


def _load(suite: str) -> list[dict]:
    d = RESULTS / suite
    rows = []
    for key, label in GATEWAYS.items():
        p = d / f"{key}.json"
        if not p.exists():
            continue
        obj = json.loads(p.read_text())
        obj["_key"], obj["_label"] = key, label
        rows.append(obj)
    return rows


def _fmt(v: float) -> str:
    if v >= 1000:
        return f"{v/1000:.1f}k" if v < 100000 else f"{v/1000:.0f}k"
    if v <= 0:
        return "0"
    return f"{v:.0f}" if v >= 10 else f"{v:.1f}"


def render(chart: Chart) -> None:
    if _mpl() is None:
        return  # no matplotlib — reports still generate from JSON
    rows = _load(chart.suite)
    if not rows:
        print(f"skip {chart.name}: no results/{chart.suite}/*.json yet")
        return
    primary = chart.series[0].field

    def _served(r) -> bool:
        return bool(r.get("served", True))

    def _val(r, field=primary) -> float:
        return float(r.get(field, 0) or 0)

    # Winner is decided ONLY among gateways that actually served — a gateway that failed under
    # load (or never came up) never colors green, even if a concurrency-1 number looks good.
    served_vals = [_val(r) for r in rows if _served(r) and _val(r) > 0]
    best = (max(served_vals) if chart.higher_better else min(served_vals)) if served_vals else None

    # Sort winners to the top. Broken gateways (did-not-serve, or a non-positive/zero metric) sink
    # to the bottom regardless of metric direction, so a failure never lands at the "best" end.
    def _sortkey(r):
        ok = _served(r) and _val(r) > 0
        if not ok:
            return (1, 0.0)
        return (0, -_val(r) if chart.higher_better else _val(r))
    rows.sort(key=_sortkey)

    # A positive floor for the log axis: negative/zero bars can't be drawn on a log scale, so their
    # labels get anchored here instead of vanishing off-canvas.
    xmax = max((_val(r) for r in rows), default=1.0) or 1.0
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

    for si, s in enumerate(chart.series):
        offset = group_h / 2 - bar_h / 2 - si * bar_h
        vals = [float(r.get(s.field, 0) or 0) for r in rows]
        rank = s.kind == "rank"
        if rank:
            # green = served winner; slate = served; muted = did-not-serve (bar is context only).
            colors = [BRAND if (best is not None and _served(r) and v == best)
                      else (SLATE if _served(r) else MUTE)
                      for r, v in zip(rows, vals)]
        else:
            colors = [s.kind] * n
        # On a log axis a bar can't start at 0, and a negative/zero value can't be drawn at all.
        draw = [v if (not chart.log or v > 0) else 0 for v in vals]
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
                    txt, col, weight = _fmt(v), INK, "bold"
                elif served:  # came up, but no tested load held p99 < 1 s with zero errors
                    txt, col, weight = "0  ·  no load held p99 < 1 s", "#c2410c", "bold"
                elif v > 0:   # a concurrency-1 number exists, but it collapsed under load
                    txt, col, weight = f"{_fmt(v)}   ✕ did not serve under load", "#c2410c", "bold"
                else:
                    txt, col, weight = "✕ did not serve", "#c2410c", "bold"
                ax.text(tx, cy, txt, va="center", ha="left", fontsize=9.5,
                        fontweight=weight, color=col, zorder=4)
            elif v > 0:  # secondary series (e.g. idle RSS): quiet label, skip empty bars
                ax.text(tx, cy, _fmt(v), va="center", ha="left", fontsize=8,
                        fontweight="normal", color=GRAY, zorder=4)

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
    better = "higher is better" if chart.higher_better else "lower is better"
    ax.set_xlabel(f"{chart.unit}   ·   {better}" + ("   (log scale)" if chart.log else ""),
                  fontsize=9, color=GRAY)
    ax.set_xlim(right=xmax * (2.9 if chart.log else 1.28))

    # Title + subtitle stacked above the axes with real vertical separation (no overlap). pad lifts
    # the title clear of the plot; the subtitle sits just below it, still above the top gridline.
    ax.set_title(chart.title, fontsize=15, fontweight="bold", color=INK, loc="left", pad=38)
    ax.text(0, 1.035, chart.subtitle, transform=ax.transAxes, fontsize=10.5, color=GRAY, va="bottom")

    # legend (only multi-series charts need it)
    if ns > 1:
        ax.legend(loc="lower right", fontsize=9, frameon=False, ncols=ns)

    meta = rows[0]
    bits = []
    if "hardware" in meta:
        bits.append(str(meta["hardware"]))
    if "concurrency" in meta and "payload_bytes" in meta:
        bits.append(f"{meta['concurrency']}× {int(meta['payload_bytes'])//1000}KB sustained")
    bits.append("green = measured best")
    fig.text(0.008, 0.012, "  ·  ".join(bits) + "     getbusbar.com/bench — every number regenerates from raw results",
             fontsize=7.3, color=GRAY)

    fig.tight_layout(rect=(0, 0.05, 1, 0.93))
    out = RESULTS / f"{chart.name}.png"
    fig.savefig(out, dpi=200, bbox_inches="tight", facecolor="white")
    plt.close(fig)
    print(f"wrote {out}")


def _merge() -> dict:
    """One dict per gateway, merging its perf/ + memory/ result files."""
    gws: dict = {}
    for suite in ("perf", "memory"):
        d = RESULTS / suite
        for key in GATEWAYS:
            p = d / f"{key}.json"
            if p.exists():
                gws.setdefault(key, {}).update(json.loads(p.read_text()))
    return gws


def _report_md(rows: list, title: str, charts: list, pending: tuple = ()) -> str:
    """A self-contained result page: machine, table (ranked), charts, provenance."""
    hw = next((r.get("hardware") for _, r in rows if r.get("hardware")), "unknown")
    when = next((r.get("measured_at") for _, r in rows if r.get("measured_at")), "")
    lines = [f"# {title}", ""]
    lines.append(f"**Ran on:** {hw}  ·  {when}")
    lines.append("")
    lines.append("Every number below is regenerated from the raw `results/*.json` — re-run "
                 "`run-all.sh` and this page updates. Green in the charts = measured best.")
    lines.append("")
    lines.append("| Gateway | Added latency (p99) | Max proxy RPS | Sustained RPS @20ms | Idle RSS | Peak RSS | Built |")
    lines.append("|---|--:|--:|--:|--:|--:|---|")
    mock_bound_seen = False
    zero_load_seen = False
    dnf_seen = False
    fail_notes = []  # (gateway, serve_error) for every ❌ row — the receipt behind "did not serve"

    def rps_cell(val, bound, served):
        # ✕ = never served under load; 0 = served but no tested load held p99<1s + zero errors.
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
        lat_cell = "—"
        if lat is not None:
            lat_cell = f"{lat} µs" + (" †" if served is False else "")
            if served is False:
                dnf_seen = True
        rss = lambda v: f"{v:.0f} MiB" if v is not None else "—"
        if served is False and r.get("serve_error"):
            fail_notes.append((GATEWAYS[key], str(r.get("serve_error"))))
        lines.append(
            f"| {GATEWAYS[key]} "
            f"| {lat_cell} "
            f"| {proxy} "
            f"| {llm} "
            f"| {rss(idle)} "
            f"| {rss(peak)} "
            f"| `{(r.get('build') or '').strip()[:38]}` |"
        )
    # Gateways we intend to measure but haven't yet — shown so the field is transparent, never hidden.
    for key in pending:
        lines.append(
            f"| {GATEWAYS[key]} | ⏳ *pending* | — | — | — | — | *pending measurement* |"
        )
    lines.append("")
    if pending:
        names = ", ".join(GATEWAYS[k] for k in pending)
        lines.append(f"⏳ **Pending measurement** (a manifest exists; not yet run on the rig): {names}. "
                     "These land here as their runs complete — nothing is hidden.")
        lines.append("")
    lines.append("Two throughput numbers: **max proxy RPS** (instant upstream — raw forwarding speed) "
                 "and **sustained RPS @20ms** (AIGatewayBench's metric — concurrent in-flight capacity "
                 "under realistic LLM latency).")
    legend = []
    zero_or_x = any(True for _, r in rows if r.get("served") is False) or zero_load_seen
    if zero_or_x:
        legend.append("**✕** = did not serve under load (0 successful req/s).")
        legend.append("**0** = came up, but no tested concurrency held p99 < 1 s with zero errors.")
    if dnf_seen:
        legend.append("**†** = a concurrency-1 latency exists, but the gateway failed under load — "
                      "not a clean result.")
    if mock_bound_seen:
        legend.append("**⚠** = ceiling within 10% of the mock's own — treat as a **floor**, not a limit.")
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
            lines.append(f"- **{name}** — {err}")
        lines.append("")
    for c in charts:
        if (RESULTS / f"{c}.png").exists():
            lines.append(f"![{c}](../../{c}.png)")
            lines.append("")
    lines.append("---")
    lines.append("Method: added latency = gateway p99 − direct-to-mock p99 at concurrency 1; RPS "
                 "ceiling = highest sustained req/s with p99 < 1 s and zero errors; RSS idle = after "
                 "first 200, peak = under sustained load. Same box, same mock, same load, one gateway "
                 "at a time. Source refs pinned in `gateways/versions.env`; the built commit is in each row.")
    return "\n".join(lines) + "\n"


def write_reports() -> None:
    gws = _merge()
    if not gws:
        return
    ranked = sorted(gws.items(), key=lambda kv: kv[1].get("rps_sustained_20ms", 0) or kv[1].get("rps_max_proxy", 0), reverse=True)
    # Known gateways with a manifest but no result yet → listed as "pending measurement" on the all page.
    pending = tuple(k for k in GATEWAYS if k not in gws)
    charts = [c.name for c in CHARTS]
    (RESULTS / "reports" / "all").mkdir(parents=True, exist_ok=True)
    (RESULTS / "reports" / "top5").mkdir(parents=True, exist_ok=True)
    (RESULTS / "reports" / "all" / "README.md").write_text(
        _report_md(ranked, "All gateways — full field", charts, pending=pending))
    (RESULTS / "reports" / "top5" / "README.md").write_text(
        _report_md(ranked[:5], "Top 5 gateways (by throughput ceiling)", charts))
    print(f"wrote results/reports/all + top5 ({len(ranked)} gateways)")


def main() -> None:
    RESULTS.mkdir(exist_ok=True)
    any_done = False
    for c in CHARTS:
        render(c)
        any_done = any_done or (RESULTS / f"{c.name}.png").exists()
    write_reports()
    if not any_done:
        print("no charts drawn — run the benchmark first (run-all.sh)")


if __name__ == "__main__":
    main()
