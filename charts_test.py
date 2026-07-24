#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Regression guard for charts.py's VALIDITY-GATE + TOP-N RANKING — the core matrix-sole-source rewrite
# artifact that decides which gateways draw a bar and rank on the public PNGs (audit MEDIUM-R3-6).
#
# Before this test, bench-tests.yml exercised sweep_peak / stream_measure / promote_guard / test.mjs but
# NOTHING ran the real Python rules. check-consistency asserts site==chart by calling the JS
# re-implementations (app.cpuFpsCertified / app.sustainedCertified), NOT charts.py itself — so a
# regression on the Python side (cpu_valid -> `>= 0`, _served treating "untestable" as valid, inverting
# the top-N sort, or the M3 null->0 coercion) would keep every test green while the PNGs drew a bar for
# an invalid/mock-bound/null row the table hides — the exact single-source divergence the guard prevents.
#
# This drives the ACTUAL charts.py functions: _proj_streaming (cpu_valid / sust_valid), _topn_keys
# (eligibility + ranking direction), _served (via the eligibility it gates), and the MEDIUM-R3-3
# null_not_served rule. matplotlib is NOT required (render() is not exercised). charts.py reads its
# canonical numbers from site/data.json at import, so we write a minimal one, import, then monkeypatch
# charts.CANON / charts.GATEWAYS with fixtures for the assertions.
#
# Run: python3 charts_test.py
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
_DATA = os.path.join(HERE, "site", "data.json")

_created_data = False
if not os.path.exists(_DATA):
    os.makedirs(os.path.dirname(_DATA), exist_ok=True)
    with open(_DATA, "w") as f:
        json.dump({"gateways": []}, f)  # minimal valid canonical bundle so _canonical() imports
    _created_data = True

sys.path.insert(0, HERE)
try:
    import charts  # noqa: E402
finally:
    if _created_data:
        os.remove(_DATA)

_fail = 0


def check(name, got, want):
    global _fail
    if got == want:
        print(f"ok   - {name}")
    else:
        print(f"FAIL - {name}: got {got!r}, want {want!r}")
        _fail = 1


def chart_by_name(name):
    for c in charts.CHARTS:
        if c.name == name:
            return c
    raise AssertionError(f"no chart named {name}")


# ── fixtures: a canonical bundle keyed like CANON (key -> record with a `streaming` sub-record) ───────
def _canon(streaming_by_key):
    charts.CANON = {k: {"streaming": s} for k, s in streaming_by_key.items()}
    charts.GATEWAYS = {k: k for k in streaming_by_key}


BASE = dict(stream_served=True, added_ttft_p99_us=90, added_gap_p99_us=12,
            streams_sustained=1300, streams_sustained_fps=40000, streams_sustained_mock_bound=False,
            cpu_fps=48000, cpu_fps_mock_bound=False)


def stream(**over):
    d = dict(BASE)
    d.update(over)
    return d


# ── _proj_streaming: the streamcpu / sustained validity gates ────────────────────────────────────────
_canon({"g": stream()})
row = charts._proj_streaming("g")
check("_proj_streaming: certified cpu_fps (present, >0, mock_bound False) -> streamcpu_valid True", row["streamcpu_valid"], True)
check("_proj_streaming: certified sustained (present, >0, mock_bound False) -> stream_sustained_valid True", row["stream_sustained_valid"], True)

# cpu_fps mock-bound True -> NOT valid (a rig-limited number is not a gateway-vs-ceiling comparison).
_canon({"g": stream(cpu_fps_mock_bound=True)})
check("_proj_streaming: cpu_fps mock_bound True -> streamcpu_valid False", charts._proj_streaming("g")["streamcpu_valid"], False)

# cpu_fps mock-bound NULL (ceiling probe read 0, unverifiable) -> NOT valid. Guards the MEDIUM-5 leak:
# `not None` is True in Python, so a naive gate would have leaked this through as proven.
_canon({"g": stream(cpu_fps_mock_bound=None)})
check("_proj_streaming: cpu_fps mock_bound None (unverifiable) -> streamcpu_valid False", charts._proj_streaming("g")["streamcpu_valid"], False)

# cpu_fps == 0 (no valid measurement) -> NOT valid even with mock_bound False.
_canon({"g": stream(cpu_fps=0)})
check("_proj_streaming: cpu_fps 0 -> streamcpu_valid False", charts._proj_streaming("g")["streamcpu_valid"], False)

# sustained mock-bound True / None -> NOT valid (symmetric with cpu-fps, MEDIUM-R2-2 + M4 upstream).
_canon({"g": stream(streams_sustained_mock_bound=True)})
check("_proj_streaming: sustained mock_bound True -> stream_sustained_valid False", charts._proj_streaming("g")["stream_sustained_valid"], False)
_canon({"g": stream(streams_sustained_mock_bound=None)})
check("_proj_streaming: sustained mock_bound None (unverifiable) -> stream_sustained_valid False", charts._proj_streaming("g")["stream_sustained_valid"], False)


# ── _topn_keys: only VALID served rows are eligible; ranking direction is correct ────────────────────
# streamcpu_fps chart: served_field=streamcpu_valid. Only the certified gateway is eligible + ranked.
_canon({
    "good": stream(cpu_fps=48000, cpu_fps_mock_bound=False),
    "bound": stream(cpu_fps=99999, cpu_fps_mock_bound=True),   # higher raw fps but MOCK-BOUND -> excluded
    "zero": stream(cpu_fps=0, cpu_fps_mock_bound=False),        # no measurement -> excluded
})
topn = charts._topn_keys(chart_by_name("streamcpu_fps"))
check("_topn_keys: a mock-bound cpu_fps is NOT eligible even with a higher raw value", "bound" in topn, False)
check("_topn_keys: a zero cpu_fps is NOT eligible", "zero" in topn, False)
check("_topn_keys: the certified cpu_fps IS eligible", "good" in topn, True)

# stream_sustained chart: higher-is-better. The valid gateway with the higher count ranks first.
_canon({
    "hi": stream(streams_sustained=2000, streams_sustained_mock_bound=False),
    "lo": stream(streams_sustained=500, streams_sustained_mock_bound=False),
    "rig": stream(streams_sustained=9999, streams_sustained_mock_bound=True),  # mock-bound -> excluded
})
chart = chart_by_name("stream_sustained")
topn = charts._topn_keys(chart, n=1)
check("_topn_keys: higher-is-better top-1 is the highest VALID sustained count", topn, {"hi"})
topn2 = charts._topn_keys(chart, n=5)
check("_topn_keys: a mock-bound sustained count is out of the top-N despite the highest raw value", "rig" in topn2, False)


# ── MEDIUM-R3-3: a NULL added-TTFT/gap is UNMEASURED, never a served 0 that ranks first ──────────────
# The stream_added_ttft chart is zero_ok + null_not_served, ascending (lower-is-better). A gateway with a
# real measured 5 µs must rank; a gateway with a NULL TTFT must NOT be eligible (it would coerce to a
# served 0 and rank #1 on the ascending sort — the M3 bug).
_canon({
    "measured": stream(added_ttft_p99_us=5),      # a real, low, WINNING value
    "nullttft": stream(added_ttft_p99_us=None),   # unreliable c1 window: unmeasured
})
ttft_chart = chart_by_name("stream_added_ttft")
check("stream_added_ttft chart is null_not_served", ttft_chart.null_not_served, True)
topn = charts._topn_keys(ttft_chart)
check("_topn_keys: a null added-TTFT is NOT eligible (never a served 0 at the winning end)", "nullttft" in topn, False)
check("_topn_keys: a measured added-TTFT IS eligible", "measured" in topn, True)

# A genuine MEASURED 0 (sub-noise) on a zero_ok chart IS still eligible — the fix must not reject real 0s.
_canon({"z": stream(added_ttft_p99_us=0)})
check("_topn_keys: a MEASURED 0 added-TTFT is still eligible on a zero_ok chart (only null is suppressed)",
      "z" in charts._topn_keys(ttft_chart), True)

# The added-gap chart carries the same flag.
_canon({"n": stream(added_gap_p99_us=None), "m": stream(added_gap_p99_us=7)})
gap_chart = chart_by_name("stream_added_gap")
check("stream_added_gap chart is null_not_served", gap_chart.null_not_served, True)
topn = charts._topn_keys(gap_chart)
check("_topn_keys: a null added-gap is NOT eligible", "n" in topn, False)
check("_topn_keys: a measured added-gap IS eligible", "m" in topn, True)


# ── HIGH-1: perf + xlate charts project from the CANONICAL best_cell / translation_cell, NOT the ────
# RETIRED results/perf|xlate/<key>.json by disk-presence. A matrix-only gateway (best_cell present, NO
# results/perf file on disk) MUST appear as a chart row + report row, exactly as the site table ranks it.
def _canon_perf(perf_by_key):
    """key -> full canonical gateway record (best_cell / translation_cell / memory_read)."""
    charts.CANON = dict(perf_by_key)
    charts.GATEWAYS = {k: k for k in perf_by_key}


BC = dict(added_latency_p99_us=120, added_latency_p50_us=40,
          rps_max_proxy=44000, rps_max_proxy_mock_bound=False,
          rps_sustained_20ms=22000, rps_sustained_20ms_mock_bound=False,
          dialect="openai", source="matrix", build="img:1", measured_at="2026-07-24T00:00:00Z")

# A gateway with ONLY a best_cell (no results/perf/<key>.json on disk at all).
_canon_perf({"matrixonly": {"best_cell": dict(BC)}})
perf_rows = charts._load("perf")
perf_keys = {r["_key"] for r in perf_rows}
check("HIGH-1: a matrix-only gateway (best_cell, no results/perf file) appears as a perf chart row",
      "matrixonly" in perf_keys, True)
check("HIGH-1: the projected perf row carries the canonical sustained RPS (from best_cell, not disk)",
      next(r["rps_sustained_20ms"] for r in perf_rows if r["_key"] == "matrixonly"), 22000)

# A gateway with NO best_cell is absent from the perf charts (never served) — no retired-file read.
_canon_perf({"noserve": {}})
check("HIGH-1: a gateway with no best_cell is absent from the perf charts", charts._load("perf"), [])

# _merge (README leaderboard) enumerates CANON, so the matrix-only gateway appears in the report too.
_canon_perf({"matrixonly": {"best_cell": dict(BC), "memory_read": {"peak_rss_mib": 90, "idle_rss_mib": 30}}})
merged = charts._merge()
check("HIGH-1: _merge (report leaderboard) includes a matrix-only gateway (best_cell, no disk perf)",
      "matrixonly" in merged, True)
check("HIGH-1: the merged report row carries the canonical sustained RPS", merged["matrixonly"]["rps_sustained_20ms"], 22000)

# xlate charts project from translation_cell, not results/xlate/<key>.json.
_canon_perf({"xl": {"translation_cell": dict(ingress="openai", egress="anthropic", source="matrix",
                                             added_latency_p99_us=200, rps_sustained_20ms=15000)}})
xrows = charts._load("xlate")
check("HIGH-1: a matrix-only gateway with a translation_cell appears as an xlate chart row",
      {r["_key"] for r in xrows}, {"xl"})
check("HIGH-1: _suite_map('xlate') enumerates translation_cell (report translation table)",
      "xl" in charts._suite_map("xlate"), True)

# HIGH-1 (consistency): EVERY gateway with a best_cell appears as a perf chart row, and vice-versa
# (chart-row presence <=> best_cell presence). This is the assertion the audit asks for, enforced here
# by construction of the projection.
_canon_perf({"a": {"best_cell": dict(BC)}, "b": {"best_cell": dict(BC)}, "c": {}})
bc_keys = {k for k, g in charts.CANON.items() if g.get("best_cell")}
row_keys = {r["_key"] for r in charts._load("perf")}
check("HIGH-1: chart-row presence == best_cell presence (every best_cell is a row and vice-versa)",
      row_keys, bc_keys)


# ── MED-3: the passthrough RPS charts gate the bar on the mock-bound honesty flag (rps_*_valid) ──────
# A mock-bound (rig-limited) throughput must NOT draw a full bar or rank #1 — mirroring the streaming lane.
_canon_perf({
    "clean": {"best_cell": dict(BC, rps_sustained_20ms=20000, rps_sustained_20ms_mock_bound=False)},
    "bound": {"best_cell": dict(BC, rps_sustained_20ms=99999, rps_sustained_20ms_mock_bound=True)},   # higher raw, rig-limited
    "unver": {"best_cell": dict(BC, rps_sustained_20ms=88888, rps_sustained_20ms_mock_bound=None)},   # unverifiable
})
# the projected rows carry the validity flags
prows = {r["_key"]: r for r in charts._load("perf")}
check("MED-3: a NOT-mock-bound sustained RPS is valid", prows["clean"]["rps_sustained_20ms_valid"], True)
check("MED-3: a mock-bound sustained RPS is NOT valid", prows["bound"]["rps_sustained_20ms_valid"], False)
check("MED-3: an unverifiable (null mock_bound) sustained RPS is NOT valid", prows["unver"]["rps_sustained_20ms_valid"], False)
sust_topn = charts._topn_keys(chart_by_name("rps_sustained_20ms"), n=5)
check("MED-3: a mock-bound sustained RPS is out of the top-N despite the highest raw value", "bound" in sust_topn, False)
check("MED-3: an unverifiable sustained RPS is out of the top-N", "unver" in sust_topn, False)
check("MED-3: the clean sustained RPS IS ranked", "clean" in sust_topn, True)
# max-proxy chart carries the same gate
_canon_perf({
    "clean": {"best_cell": dict(BC, rps_max_proxy=40000, rps_max_proxy_mock_bound=False)},
    "bound": {"best_cell": dict(BC, rps_max_proxy=99999, rps_max_proxy_mock_bound=True)},
})
proxy_topn = charts._topn_keys(chart_by_name("rps_max_proxy"), n=5)
check("MED-3: a mock-bound max-proxy RPS is out of the top-N", "bound" in proxy_topn, False)
check("MED-3: the clean max-proxy RPS IS ranked", "clean" in proxy_topn, True)


if _fail == 0:
    print("all charts.py validity-gate tests passed")
    sys.exit(0)
print("CHARTS.PY TESTS FAILED")
sys.exit(1)
