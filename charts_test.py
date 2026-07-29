#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Regression guard for charts.py's VALIDITY-GATE + TOP-N RANKING, the artifact that decides which
# gateways draw a bar and rank on the public PNGs.
#
# check-consistency asserts site==chart by calling the JS re-implementations (app.cpuFpsCertified /
# app.sustainedCertified), NOT charts.py itself, so a regression on the Python side (cpu_valid -> `>= 0`,
# _served treating "untestable" as valid, an inverted top-N sort, or a null->0 coercion) could otherwise
# keep every other test green while the PNGs drew a bar for an invalid/mock-bound/null row the table
# hides. This test drives the ACTUAL charts.py functions: _proj_streaming (cpu_valid / sust_valid),
# _topn_keys (eligibility + ranking direction), _served (via the eligibility it gates), and the MEDIUM-R3-3
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


import contextlib


@contextlib.contextmanager
def isolated(section):
    """PER-SECTION EXCEPTION ISOLATION (round-2 audit finding). Every check below is written as
    `check(name, some_charts_function(...), want)`, which evaluates the call into charts.py BEFORE
    check() is ever entered - so a mutation that makes that function raise (rather than return a
    wrong value) previously killed this whole file at that line, silently skipping every later
    check with no FAIL printed for any of them. That is worse than any one check failing: an audit
    whose exit code and printed lines both look like "ran fine, nothing to see" while most of the
    suite never executed.

    Wrapping each section (the same "# ── ... ──" blocks the file is already organised into) in
    this context manager makes a raise INSIDE one section a recorded FAILURE for that section only,
    same as `site/test.mjs`'s runner documents for the same reason: "ordering must not decide
    coverage" (test.mjs:35-39). Every section after the one that raised still runs, and gets its own
    ok/FAIL lines.
    """
    global _fail
    try:
        yield
    except Exception as e:  # noqa: BLE001 - a check raising IS the finding, not a bug in the test
        _fail = 1
        print(f"FAIL - [{section}] raised {e!r} instead of completing - isolated, run continues")


def chart_by_name(name):
    for c in charts.CHARTS:
        if c.name == name:
            return c
    raise AssertionError(f"no chart named {name}")


# ── seal helper: mirror seal.mjs / gen-data so fixtures express RAW intent (value + mock_bound) and get
#    sealed into the SAME envelope shape the real bundle carries. A gated metric is certified only when
#    present + (0 [measured-zero] OR (>0 AND flag is False)); else suppressed (value:null). ─────────────
with isolated('seal helper: mirror seal.mjs / gen-data so fixtures express RAW int...'):
    def _seal(value, gated=False, flag=None, extras=None, zero_note="no_qualifying_ceiling"):
        if value is None:
            return {"value": None, "certified": False, "suppressed": False, "reason": "not_measured"}
        if gated:
            # A measured 0 is ALWAYS certified; its NOTE names what the zero means. Only null is
            # "not measured": folding a measured streaming 0 into not_measured would hide a real
            # measured failure behind an unmeasured cell.
            if value == 0:
                return {"value": 0, "certified": True, "suppressed": False, "note": zero_note}
            if not (value > 0 and flag is False):
                reason = "mock_bound" if flag is True else "unverifiable"
                return {"value": None, "certified": False, "suppressed": True, "reason": reason}
        env = {"value": value, "certified": True, "suppressed": False}
        if extras:
            for k, v in extras.items():
                if v is not None:
                    env[k] = v
        return env


    _SRC = {"kind": "matrix", "sweep": "6x6-stream-diagonal", "build": "x", "measured_at": "2026-01-01T00:00:00Z"}


# ── fixtures: a canonical bundle keyed like CANON (key -> record with a `streaming` sub-record) ───────
with isolated('fixtures: a canonical bundle keyed like CANON (key -> record with a...'):
    def _canon(streaming_by_key):
        charts.CANON = {k: {"streaming": s} for k, s in streaming_by_key.items()}
        charts.GATEWAYS = {k: k for k in streaming_by_key}


    # stream(**over): build a raw stream record from the base intent (value + mock_bound), then SEAL it into
    # the envelope-carrying record _proj_streaming reads. `over` names the raw values (e.g. cpu_fps_mock_bound=
    # True); the seal turns them into the correct envelope, exactly as gen-data does.
    def stream(**over):
        raw = dict(added_ttft_p99_us=90, added_gap_p99_us=12,
                   streams_sustained=1300, streams_sustained_fps=40000, streams_sustained_mock_bound=False,
                   cpu_fps=48000, cpu_fps_mock_bound=False)
        raw.update(over)
        rec = {"stream_served": True, "path": {"dialect": "openai"}, "source": _SRC,
               "added_ttft_p99_us": _seal(raw["added_ttft_p99_us"]),
               "added_gap_p99_us": _seal(raw["added_gap_p99_us"]),
               "streams_sustained_fps": _seal(raw["streams_sustained_fps"], gated=True, flag=raw["streams_sustained_mock_bound"], zero_note="measured_failure"),
               "streams_sustained": _seal(raw["streams_sustained"], gated=True, flag=raw["streams_sustained_mock_bound"], zero_note="measured_failure"),
               "cpu_fps": _seal(raw["cpu_fps"], gated=True, flag=raw["cpu_fps_mock_bound"], zero_note="measured_failure")}
        return rec


    # _mem(**over): a gateway record carrying ONE SEALED PER-CELL memory window, which is the only shape
    # memory ships in: there is no per-gateway memory record any more, because producing one would mean the
    # harness SELECTING a cell. Every *_rss_mib field is an envelope, never a bare scalar. RSS is UNGATED, so
    # a present value is certified and an absent one seals to not_measured. Null-safe by construction: the
    # producer emits NULL (not a fabricated 0) for an RSS it could not obtain, and every consumer must render
    # that as "not measured". `dialect` is the identity cell the window sits on (charts compare on ONE cell).
    _MEM_SEALED = ("idle_rss_mib", "steady_state_rss_mib", "recovered_rss_mib", "peak_rss_mib",
                   "peak_rss_hwm_mib", "growth_rate_mib_per_min", "time_to_plateau_s")


    def _mem(dialect="openai", served=True, **over):
        mem = {"served": True, "protocol": "per-cell, own cold-started process",
               "load_recipe": {"concurrency": 64, "payload_bytes": 4096},
               "idle_window_s": 60, "recovery_window_s": 60, "plateaued": over.pop("plateaued", True)}
        for k, v in over.items():
            mem[k] = _seal(v) if k in _MEM_SEALED else v
        return {"matrix": {"upstreams": {dialect: {"cells": {dialect: {"served": served, "memory": mem}}}}}}


# ── _proj_streaming: the streamcpu / sustained validity gates ────────────────────────────────────────
with isolated('_proj_streaming: the streamcpu / sustained validity gates'):
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

    # AUDIT #3: cpu_fps == 0 with mock_bound False is a MEASURED FAILURE (the gateway relayed no qualifying
    # frames), NOT "not measured". It stays a REAL reading (valid) and is rendered as the failure it is; only
    # a NULL is unmeasured. Folding the measured zero into "not measured (rig-limited)" flattered the gateway.
    _canon({"g": stream(cpu_fps=0)})
    _r0 = charts._proj_streaming("g")
    check("AUDIT #3: cpu_fps measured 0 is a REAL reading, not 'not measured'", _r0["streamcpu_valid"], True)
    check("AUDIT #3: cpu_fps measured 0 keeps its value (0, not None)", _r0["streamcpu_frames_per_sec"], 0)
    _canon({"g": stream(streams_sustained=0, streams_sustained_fps=0)})
    _s0 = charts._proj_streaming("g")
    check("AUDIT #3: a measured stream-sustain FAILURE is 0, distinguishable from unmeasured",
          (_s0["stream_sustained_streams"], _s0["stream_sustained_valid"], _s0["stream_sustained_note"]),
          (0, True, "measured_failure"))
    _canon({"g": stream(streams_sustained=None)})
    _sn = charts._proj_streaming("g")
    check("AUDIT #3: an UNMEASURED stream-sustain is None and not valid (the other state)",
          (_sn["stream_sustained_streams"], _sn["stream_sustained_valid"]), (None, False))
    # AUDIT #2: the streaming lane carries its PROVENANCE stamp so the PNGs can disclose it.
    _canon({"g": stream()})
    check("AUDIT #2: _proj_streaming carries the streaming lane's source sweep stamp",
          charts._proj_streaming("g")["_stream_source"], "6x6-stream-diagonal")
    check("AUDIT #2: _stream_annot discloses a legacy stream-suite provenance",
          charts._stream_annot({"_stream_source": "stream-suite"}), "stream suite")
    check("AUDIT #2: _stream_annot adds NO suffix for a matrix-sourced record",
          charts._stream_annot({"_stream_source": "6x6-stream-diagonal"}), None)
    # AUDIT #23: a BARE unsealed scalar is REJECTED, never silently charted.
    try:
        charts.mval(1234)
        check("AUDIT #23: mval REJECTS a bare unsealed scalar", "no error", "SystemExit")
    except SystemExit:
        check("AUDIT #23: mval REJECTS a bare unsealed scalar", "SystemExit", "SystemExit")
    check("AUDIT #23: mval still accepts absent (None) as not-measured", charts.mval(None), None)

    # sustained mock-bound True / None -> NOT valid (symmetric with cpu-fps, MEDIUM-R2-2 + M4 upstream).
    _canon({"g": stream(streams_sustained_mock_bound=True)})
    check("_proj_streaming: sustained mock_bound True -> stream_sustained_valid False", charts._proj_streaming("g")["stream_sustained_valid"], False)
    _canon({"g": stream(streams_sustained_mock_bound=None)})
    check("_proj_streaming: sustained mock_bound None (unverifiable) -> stream_sustained_valid False", charts._proj_streaming("g")["stream_sustained_valid"], False)


# ── _topn_keys: only VALID served rows are eligible; ranking direction is correct ────────────────────
# streamcpu_fps chart: served_field=streamcpu_valid. Only the certified gateway is eligible + ranked.
with isolated('_topn_keys: only VALID served rows are eligible; ranking direction ...'):
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
# served 0 and rank #1 on the ascending sort - the M3 bug).
with isolated('MEDIUM-R3-3: a NULL added-TTFT/gap is UNMEASURED, never a served 0 ...'):
    _canon({
        "measured": stream(added_ttft_p99_us=5),      # a real, low, WINNING value
        "nullttft": stream(added_ttft_p99_us=None),   # unreliable c1 window: unmeasured
    })
    ttft_chart = chart_by_name("stream_added_ttft")
    check("stream_added_ttft chart is null_not_served", ttft_chart.null_not_served, True)
    topn = charts._topn_keys(ttft_chart)
    check("_topn_keys: a null added-TTFT is NOT eligible (never a served 0 at the winning end)", "nullttft" in topn, False)
    check("_topn_keys: a measured added-TTFT IS eligible", "measured" in topn, True)

    # A genuine MEASURED 0 (sub-noise) on a zero_ok chart IS still eligible - the fix must not reject real 0s.
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
with isolated('HIGH-1: perf + xlate charts project from the CANONICAL best_cell / ...'):
    def _canon_perf(perf_by_key):
        """key -> full canonical gateway record (best_cell / translation_cell / memory_read)."""
        charts.CANON = dict(perf_by_key)
        charts.GATEWAYS = {k: k for k in perf_by_key}


    _BC_SRC = {"kind": "matrix", "sweep": "6x6-diagonal", "build": "img:1", "measured_at": "2026-07-24T00:00:00Z"}


    def bc(added_latency_p99_us=120, added_latency_p50_us=40,
           rps_max_proxy=44000, rps_max_proxy_mock_bound=False,
           rps_sustained_20ms=22000, rps_sustained_20ms_mock_bound=False, dialect="openai"):
        """A SEALED best_cell fixture: raw intent (value + mock_bound) -> the envelope shape gen-data emits."""
        return {
            "path": {"ingress": dialect, "egress": dialect, "dialect": dialect}, "source": _BC_SRC,
            "added_latency_p50_us": _seal(added_latency_p50_us),
            "added_latency_p99_us": _seal(added_latency_p99_us),
            "rps_max_proxy": _seal(rps_max_proxy, gated=True, flag=rps_max_proxy_mock_bound),
            "rps_sustained_20ms": _seal(rps_sustained_20ms, gated=True, flag=rps_sustained_20ms_mock_bound),
        }


    def tc(ingress="openai", egress="anthropic", added_latency_p99_us=200,
           added_latency_p50_us=None, rps_sustained_20ms=15000, rps_sustained_20ms_mock_bound=False):
        """A SEALED translation_cell fixture."""
        rec = {"path": {"ingress": ingress, "egress": egress},
               "source": {"kind": "matrix", "sweep": "6x6-translation", "build": "img:1", "measured_at": "2026-07-24T00:00:00Z"},
               "added_latency_p99_us": _seal(added_latency_p99_us),
               "rps_sustained_20ms": _seal(rps_sustained_20ms, gated=True, flag=rps_sustained_20ms_mock_bound)}
        if added_latency_p50_us is not None:
            rec["added_latency_p50_us"] = _seal(added_latency_p50_us)
        return rec


    # A gateway with ONLY a best_cell (no results/perf/<key>.json on disk at all).
    _canon_perf({"matrixonly": {"best_cell": bc()}})
    perf_rows = charts._load("perf")
    perf_keys = {r["_key"] for r in perf_rows}
    check("HIGH-1: a matrix-only gateway (best_cell, no results/perf file) appears as a perf chart row",
          "matrixonly" in perf_keys, True)
    check("HIGH-1: the projected perf row carries the canonical sustained RPS (from best_cell, not disk)",
          next(r["rps_sustained_20ms"] for r in perf_rows if r["_key"] == "matrixonly"), 22000)

    # A gateway with NO best_cell is absent from the perf charts (never served) - no retired-file read.
    _canon_perf({"noserve": {}})
    check("HIGH-1: a gateway with no best_cell is absent from the perf charts", charts._load("perf"), [])

    # _merge (README leaderboard) enumerates CANON, so the matrix-only gateway appears in the report too.
    _canon_perf({"matrixonly": {"best_cell": bc(),
                                **_mem(idle_rss_mib=30, steady_state_rss_mib=90)}})
    merged = charts._merge()
    check("HIGH-1: _merge (report leaderboard) includes a matrix-only gateway (best_cell, no disk perf)",
          "matrixonly" in merged, True)
    check("HIGH-1: the merged report row carries the canonical sustained RPS", merged["matrixonly"]["rps_sustained_20ms"], 22000)

    # xlate charts project from translation_cell, not results/xlate/<key>.json.
    _canon_perf({"xl": {"translation_cell": tc()}})
    xrows = charts._load("xlate")
    check("HIGH-1: a matrix-only gateway with a translation_cell appears as an xlate chart row",
          {r["_key"] for r in xrows}, {"xl"})
    check("HIGH-1: _suite_map('xlate') enumerates translation_cell (report translation table)",
          "xl" in charts._suite_map("xlate"), True)

    # The README translation TABLE must suppress a mock-bound (rig-limited) translation RPS, exactly as the
    # translation PNG (served_field=xlate_rps_sustained_20ms_valid) and the site do: printing the raw value
    # would publish a number the chart + site both hide.
    # Certified cell -> the number prints; mock-bound cell -> "not measured (rig-limited)", not the raw value.
    _canon_perf({
        "xcert": {"best_cell": bc(), "translation_cell": tc(rps_sustained_20ms=15000, rps_sustained_20ms_mock_bound=False)},
        "xbound": {"best_cell": bc(), "translation_cell": tc(rps_sustained_20ms=99999, rps_sustained_20ms_mock_bound=True)},
    })
    md = charts._report_md(list(charts._merge().items()), "t", [])
    check("FINDING 24: a CERTIFIED translation RPS prints its value in the README table", "15,000" in md, True)
    check("FINDING 24: a mock-bound translation RPS does NOT leak its raw value into the README table",
          "99,999" in md, False)
    check("FINDING 24: a mock-bound translation cell reads 'rig-limited' in the README table",
          "rig-limited" in md, True)

    # HIGH-1 (consistency): EVERY gateway with a best_cell appears as a perf chart row, and vice-versa
    # (chart-row presence <=> best_cell presence). This is the assertion the audit asks for, enforced here
    # by construction of the projection.
    _canon_perf({"a": {"best_cell": bc()}, "b": {"best_cell": bc()}, "c": {}})
    bc_keys = {k for k, g in charts.CANON.items() if g.get("best_cell")}
    row_keys = {r["_key"] for r in charts._load("perf")}
    check("HIGH-1: chart-row presence == best_cell presence (every best_cell is a row and vice-versa)",
          row_keys, bc_keys)


# ── MED-3: the passthrough RPS charts gate the bar on the mock-bound honesty flag (rps_*_valid) ──────
# A mock-bound (rig-limited) throughput must NOT draw a full bar or rank #1 - mirroring the streaming lane.
with isolated('MED-3: the passthrough RPS charts gate the bar on the mock-bound ho...'):
    _canon_perf({
        "clean": {"best_cell": bc(rps_sustained_20ms=20000, rps_sustained_20ms_mock_bound=False)},
        "bound": {"best_cell": bc(rps_sustained_20ms=99999, rps_sustained_20ms_mock_bound=True)},   # higher raw, rig-limited
        "unver": {"best_cell": bc(rps_sustained_20ms=88888, rps_sustained_20ms_mock_bound=None)},   # unverifiable
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
        "clean": {"best_cell": bc(rps_max_proxy=40000, rps_max_proxy_mock_bound=False)},
        "bound": {"best_cell": bc(rps_max_proxy=99999, rps_max_proxy_mock_bound=True)},
    })
    proxy_topn = charts._topn_keys(chart_by_name("rps_max_proxy"), n=5)
    check("MED-3: a mock-bound max-proxy RPS is out of the top-N", "bound" in proxy_topn, False)
    check("MED-3: the clean max-proxy RPS IS ranked", "clean" in proxy_topn, True)


# ── memory RECOVERY: recovered_rss_mib is null_not_served - a gateway measured BEFORE the recovery ────
# signal existed (recovered_rss_mib absent → projected None) must NOT draw a fabricated 0 bar or rank,
# while a gateway WITH a recovery number is ranked (best = min, lower recovery wins). _proj_memory reads
# the PER-CELL window on the shared comparison cell, so fixture the matrix cell directly.
with isolated('memory RECOVERY: recovered_rss_mib is null_not_served - a gateway m...'):
    rec_chart = chart_by_name("memory_recovery")
    check("memory_recovery chart is null_not_served (no fabricated 0 for a pre-recovery bundle)",
          rec_chart.null_not_served, True)
    # SEALED fixtures: a bare scalar in the bundle is a hard error (see mval()), so these fixtures state
    # what the real bundle actually carries rather than a raw number.
    charts.CANON = {
        "recovers": _mem(idle_rss_mib=40, steady_state_rss_mib=1000, recovered_rss_mib=45),
        "pinned":   _mem(idle_rss_mib=60, steady_state_rss_mib=900,  recovered_rss_mib=880),
        "oldbundle":_mem(idle_rss_mib=50, steady_state_rss_mib=800),  # pre-recovery: no field
    }
    charts.GATEWAYS = {k: k for k in charts.CANON}
    mrows = {r["_key"]: r for r in charts._load("memory")}
    check("_proj_memory carries recovered_rss_mib when present", mrows["recovers"]["recovered_rss_mib"], 45)
    check("_proj_memory carries None (not 0) when the recovery field is absent",
          mrows["oldbundle"]["recovered_rss_mib"], None)
    rec_topn = charts._topn_keys(rec_chart, n=5)
    check("memory_recovery: a gateway WITH a recovery number is ranked", "recovers" in rec_topn, True)
    check("memory_recovery: a gateway that RELEASES ranks over one that stays pinned (best = min)",
          list(charts._topn_keys(rec_chart, n=1))[0], "recovers")
    check("memory_recovery: a pre-recovery bundle (null recovered) is NOT eligible (never a fabricated 0)",
          "oldbundle" in rec_topn, False)

# ── memory RSS: steady_state_rss_mib is null_not_served. Two DISTINCT ways to have no number, and ─────
# neither may draw a fabricated served-0 bar (audit #7/#23): a gateway that does not serve the comparison
# cell at all, and one that served it but whose RSS NEVER WENT STEADY - the second is the interesting one,
# because the honest answer there is not a number at all. Its growth rate is the finding, and the bar says
# "never settled" instead of substituting a peak (which would report when the load stopped).
with isolated('memory RSS: steady_state_rss_mib is null_not_served. Two DISTINCT w...'):
    rss_chart = chart_by_name("memory_rss")
    check("memory_rss chart is null_not_served (no fabricated 0 for a gateway with no steady state)",
          rss_chart.null_not_served, True)
    charts.CANON = {
        "measured": _mem(idle_rss_mib=40, steady_state_rss_mib=900),
        "nocell":   _mem(served=False, idle_rss_mib=None, steady_state_rss_mib=None),  # cell not served
        "leaks":    _mem(idle_rss_mib=20, steady_state_rss_mib=None, plateaued=False,
                         growth_rate_mib_per_min=42.5),                                # served, never settled
    }
    charts.GATEWAYS = {k: k for k in charts.CANON}
    mrows = {r["_key"]: r for r in charts._load("memory")}
    rss_topn = charts._topn_keys(rss_chart, n=5)
    check("memory_rss: a gateway with a real steady state is ranked", "measured" in rss_topn, True)
    check("memory_rss: an unserved cell is NOT eligible (never a fabricated 0 bar)", "nocell" in rss_topn, False)
    check("memory_rss: a gateway that never settled is NOT eligible (no steady state to rank)",
          "leaks" in rss_topn, False)
    check("memory_rss: the never-settled bar says so, and quantifies it with the growth rate",
          "never settled: +42.5 MiB/min" in (charts._mem_annot(mrows["leaks"]) or ""), True)
    # ...and it KEEPS its measured cold-idle number. The idle sample is taken cold, before the gateway
    # serves a single request, so it is valid whether or not the RSS later went steady. Deleting it because
    # the steady state (a different field on the same record) is null would apply "unmeasurable means
    # absent" to something that WAS measured - and on the last field run four of eleven gateways never
    # settled, so four real idle bars would have silently vanished from the one chart that shows idle.
    check("memory_rss: a never-settled gateway KEEPS its measured idle value",
          mrows["leaks"]["idle_rss_mib"], 20)
    # ...but a window the PRODUCER disclosed as not-served contributes nothing at all, idle included. The
    # producer sets memory.served=false when the fixed load stopped delivering or the delivered payload was
    # not the declared one; in that state a relaunch race can leave the "cold" idle sample belonging to the
    # previous cell's post-load process, so the whole window is absent rather than partially charted.
    charts.CANON = {"disclosed": _mem(idle_rss_mib=33, steady_state_rss_mib=None)}
    charts.CANON["disclosed"]["matrix"]["upstreams"]["openai"]["cells"]["openai"]["memory"]["served"] = False
    charts.GATEWAYS = {k: k for k in charts.CANON}
    _drow = {r["_key"]: r for r in charts._load("memory")}["disclosed"]
    check("a window disclosed served=false still gets a ROW (every measured gateway appears)",
          _drow["_mem_unserved"], True)
    check("...but contributes NO numbers, its idle included", _drow["idle_rss_mib"], None)
    check("memory_rss: every row names the ONE cell every gateway was compared on",
          mrows["measured"]["_mem_load_cell"], "openai>openai")

    # The comparison cell is the identity cell the MOST gateways serve, derived from the data (never named).
    charts.CANON = {
        "a": _mem(dialect="anthropic", idle_rss_mib=10, steady_state_rss_mib=100),
        "b": _mem(dialect="anthropic", idle_rss_mib=10, steady_state_rss_mib=100),
        "c": _mem(dialect="openai", idle_rss_mib=10, steady_state_rss_mib=100),
    }
    charts.GATEWAYS = {k: k for k in charts.CANON}
    check("the memory comparison cell is the identity cell most of the field serves", charts._mem_cell(), "anthropic")
    # RULE: every MEASURED gateway appears; one that does not serve the comparison cell reads n/a. Dropping
    # its row would delete the most important fact about a narrow gateway from the chart where breadth shows.
    mrows = {r["_key"]: r for r in charts._load("memory")}
    check("a gateway that does not serve the comparison cell still gets a row", set(mrows), {"a", "b", "c"})
    check("that row carries NO numbers (n/a, never a substituted cell)", mrows["c"]["steady_state_rss_mib"], None)
    check("that row is not rankable", "c" in charts._topn_keys(chart_by_name("memory_rss"), n=5), False)
    check("that row says WHY it is empty", charts._mem_annot(mrows["c"]), "does not serve anthropic>anthropic")
    # A gateway with no matrix at all (never measured) has no row to draw and none to claim.
    charts.CANON = {"a": _mem(dialect="anthropic", idle_rss_mib=10, steady_state_rss_mib=100), "never": {}}
    charts.GATEWAYS = {k: k for k in charts.CANON}
    check("a gateway that was never measured has no memory row", {r["_key"] for r in charts._load("memory")}, {"a"})


# ── below_resolution on the added-latency charts: a sub-resolution 0 is a WIN, never a throughput ────
# failure. The sealed envelope {value:null, reason:"below_resolution", detail:...} charts as 0.0 (mval:
# the difference ran and came out under what the rig can resolve - the winning end of a lower-is-better
# chart). Before the fix, added_latency had no zero_ok, so that 0 fell into render()'s `elif served:`
# branch and was captioned with the DEFAULT zero_text "0  ·  no load held p99 < 1 s" - a THROUGHPUT-
# failure sentence, in failure orange, on a latency win - and _topn_keys excluded it from the ranking
# entirely (zero_ok False rejects a 0). Both assertions below fail against that behavior.
with isolated('below_resolution on the added-latency charts: a sub-resolution 0 is...'):
    _BR = {"value": None, "certified": True, "suppressed": False, "reason": "below_resolution",
           "detail": "p99 delta under rig resolution"}
    check("mreason reads the envelope's absence-reason token", charts.mreason(_BR), "below_resolution")
    check("mval renders a below_resolution absence as 0.0 (the winning end)", charts.mval(_BR), 0.0)

    lat_chart = chart_by_name("added_latency")
    check("added_latency chart is zero_ok (a 0 is the winning end, never the zero_text failure)",
          lat_chart.zero_ok, True)
    check("xlate_added_latency chart is zero_ok too", chart_by_name("xlate_added_latency").zero_ok, True)

    _canon_perf({
        "subres": {"best_cell": {**bc(), "added_latency_p99_us": _BR}},   # below rig resolution
        "slow":   {"best_cell": bc(added_latency_p99_us=120)},            # a real, higher reading
    })
    prows = {r["_key"]: r for r in charts._load("perf")}
    check("a below_resolution added-latency envelope charts as 0.0, not absent",
          prows["subres"]["added_latency_p99_us"], 0.0)
    check("...so it is NOT flagged unmeasured on the null_not_served chart (0.0 is not None)",
          prows["subres"]["added_latency_p99_us"] is None, False)
    check("...and it carries the reason for the label", prows["subres"]["_added_latency_p99_us_reason"],
          "below_resolution")
    check("...it is ELIGIBLE and ranks at the WINNING end of the lower-is-better chart",
          charts._topn_keys(lat_chart, n=1), {"subres"})
    _lbl = charts._zero_label(lat_chart, prows["subres"])
    check("...its bar label discloses sub-resolution, exactly", _lbl, "0 (≤ rig resolution)")
    check("...and NEVER the throughput-failure zero_text", "no load held p99" in _lbl, False)
    check("a plain measured 0 (no below_resolution reason) stays a bare '0'",
          charts._zero_label(lat_chart, prows["slow"]), "0")

    # The translation twin carries the same treatment.
    _canon_perf({"xsub": {"translation_cell": {**tc(), "added_latency_p99_us": _BR}}})
    _xrow = charts._load("xlate")[0]
    check("a below_resolution translated added-latency charts as 0.0", _xrow["xlate_added_latency_p99_us"], 0.0)
    check("...and its label discloses sub-resolution",
          charts._zero_label(chart_by_name("xlate_added_latency"), _xrow), "0 (≤ rig resolution)")
    check("...it is eligible on the (already zero_ok) translation chart",
          charts._topn_keys(chart_by_name("xlate_added_latency"), n=1), {"xsub"})

    # The streaming latency lanes (already zero_ok) get the same disclosure on their zero.
    _rec = stream()
    _rec["added_ttft_p99_us"] = _BR
    _canon({"g": _rec})
    _srow = charts._proj_streaming("g")
    check("a below_resolution added-TTFT charts as 0.0", _srow["stream_added_ttft_p99_us"], 0.0)
    check("...and its label discloses sub-resolution",
          charts._zero_label(chart_by_name("stream_added_ttft"), _srow), "0 (≤ rig resolution)")
    _canon({"z": stream(added_ttft_p99_us=0)})
    check("a MEASURED 0 added-TTFT still labels a bare '0' (only below_resolution gets the suffix)",
          charts._zero_label(chart_by_name("stream_added_ttft"), charts._proj_streaming("z")), "0")


# ── the DIFFERENCE-CHART FAMILY, as one rule instead of four settings (ledger TOOL-05) ───────────────
#
# `clamp_negatives` was set on three of the four charts whose primary metric is a difference and not on
# the fourth, and nothing anywhere said which charts were supposed to carry it. The flag that replaced
# it (`diff_metric`) is only worth having if the family is enumerated and each member is held to the
# same treatment, so this pins the membership AND the settings each member needs for a
# below_resolution 0 to render honestly.
with isolated('the DIFFERENCE-CHART FAMILY, as one rule instead of four settings (...'):
    DIFF_CHARTS = {c.name for c in charts.CHARTS if c.diff_metric}
    check("the difference-chart family is exactly the four charts whose metric is a subtraction",
          DIFF_CHARTS,
          {"added_latency", "xlate_added_latency", "stream_added_ttft", "stream_added_gap"})
    for _c in [c for c in charts.CHARTS if c.diff_metric]:
        # zero_ok: a below_resolution difference charts as 0, and without zero_ok that 0 falls into
        # render()'s `elif served:` branch and is captioned with zero_text - a THROUGHPUT-failure sentence
        # in failure orange on a latency WIN, and excluded from the ranking it should top.
        check(f"{_c.name}: a difference chart treats 0 as the winning end (zero_ok)", _c.zero_ok, True)
        # null_not_served: without it, `float(r.get(f, 0) or 0)` coerces an UNMEASURED null to 0.0, which
        # on a zero_ok chart ranks the unmeasured gateway #1 for adding no overhead.
        check(f"{_c.name}: an unmeasured null is not a served 0 (null_not_served)", _c.null_not_served, True)
        # lower-is-better: the whole reason a 0 is the winning end.
        check(f"{_c.name}: is lower-is-better", _c.higher_better, False)

    # Every member's projection must carry the `_<field>_reason` breadcrumb, or _zero_label has nothing to
    # read and the bar reverts to a bare "0" that reads as an exact measurement. Driven off DIFF_CHARTS so
    # a fifth difference chart added tomorrow is covered the day it is added.
    _BR_ROWS = {}
    _canon_perf({"p": {"best_cell": {**bc(), "added_latency_p99_us": _BR}},
                 "x": {"translation_cell": {**tc(), "added_latency_p99_us": _BR}}})
    _BR_ROWS["added_latency"] = charts._proj_perf("p")
    _BR_ROWS["xlate_added_latency"] = charts._proj_xlate("x")
    _st = stream(); _st["added_ttft_p99_us"] = _BR
    _canon({"s": _st})
    _BR_ROWS["stream_added_ttft"] = charts._proj_streaming("s")
    _sg = stream(); _sg["added_gap_p99_us"] = _BR
    _canon({"s": _sg})
    _BR_ROWS["stream_added_gap"] = charts._proj_streaming("s")
    check("every difference chart has a below_resolution fixture proving it end to end",
          set(_BR_ROWS), DIFF_CHARTS)
    for _c in [c for c in charts.CHARTS if c.diff_metric]:
        _f = _c.series[0].field
        _r = _BR_ROWS[_c.name]
        check(f"{_c.name}: a below_resolution envelope charts as 0.0, not absent", _r.get(_f), 0.0)
        check(f"{_c.name}: its projection carries the reason breadcrumb", charts._below_res(_r, _f), True)
        check(f"{_c.name}: and its bar label discloses the rig limit",
              charts._zero_label(_c, _r), "0 (≤ rig resolution)")
    # The stream_added_gap half of that was previously unproven: only the TTFT twin had a test, so a
    # projection that dropped the gap breadcrumb would have shipped a bare "0" on the per-token chart.
    _canon({"z": stream(added_gap_p99_us=0)})
    check("a MEASURED 0 added-gap still labels a bare '0' (only below_resolution gets the suffix)",
          charts._zero_label(chart_by_name("stream_added_gap"), charts._proj_streaming("z")), "0")

# ── the negative-difference REFUSAL, which replaced the silent clamp (ledger TOOL-05) ────────────────
#
# RED: a negative on a difference chart must be refused by name. Under the old clamp this same row was
# rewritten to 0.0, sorted to the WINNING end of a lower-is-better chart, labelled a bare "0", and
# disclosed only by one footnote at the bottom of the image that said some unnamed bar had been
# clamped. Deleting the clamp without this check would have kept every one of those consequences and
# dropped the footnote too.
with isolated('the negative-difference REFUSAL, which replaced the silent clamp (l...'):
    _neg = [{"_key": "gw", "added_latency_p99_us": -3.0}]
    _v = charts._negative_diff_violations(chart_by_name("added_latency"), _neg)
    check("a NEGATIVE difference is refused, not clamped to a winning 0", len(_v), 1)
    check("...and the refusal names the gateway", "gw" in _v[0], True)
    check("...and names the field", "added_latency_p99_us" in _v[0], True)
    for _c in [c for c in charts.CHARTS if c.diff_metric]:
        _row = [{"_key": "gw", _c.series[0].field: -1.0}]
        check(f"{_c.name}: refuses a negative difference",
              len(charts._negative_diff_violations(_c, _row)), 1)

    # ACCEPT: everything that is not a negative on a difference chart passes untouched - a measured 0, a
    # below_resolution 0, a real positive, an absent value, and a NON-difference chart's negative (which
    # this check has no opinion about, because a negative RSS or RPS is a different bug entirely).
    for _name, _row in (("a measured 0", {"_key": "g", "added_latency_p99_us": 0.0}),
                        ("a real positive", {"_key": "g", "added_latency_p99_us": 120}),
                        ("an absent value", {"_key": "g"}),
                        ("a below_resolution 0", _BR_ROWS["added_latency"])):
        check(f"the refusal accepts {_name}",
              charts._negative_diff_violations(chart_by_name("added_latency"), [_row]), [])
    check("the refusal has no opinion about a non-difference chart",
          charts._negative_diff_violations(chart_by_name("rps_max_proxy"),
                                           [{"_key": "g", "rps_max_proxy": -1}]), [])
    # `served=False` is a bool, and bool is a subclass of int in Python - a naive `v < 0` walk over row
    # fields would be fine, but a naive `isinstance(v, int)` guard that forgot bools would start reading
    # flags as measurements. Pin that bools are never mistaken for a negative reading.
    check("a False flag in a row field is not read as a measurement",
          charts._negative_diff_violations(chart_by_name("added_latency"),
                                           [{"_key": "g", "added_latency_p99_us": False}]), [])

# ── WIRING: the refusal must fire from render() itself, not just from a direct call to the helper ────
#
# Everything above calls _negative_diff_violations() directly, which proves the function is correct but
# NOT that render() actually consults it. render()'s only call site is two lines (`for v in
# _negative_diff_violations(...): raise SystemExit(v)`) that a future edit could delete, comment out, or
# reorder past the `_mpl()` bail without breaking a single assertion above - every one of them drives the
# helper, none of them drive render(). This check closes that gap by going through the real entry point:
# seed CANON with a negative added_latency_p99_us best_cell and call charts.render() itself.
#
# It also pins WHERE the gate sits: the docstring at charts.py:1075 claims the refusal runs "above the
# `_mpl()` return deliberately" so a box with no matplotlib still refuses instead of silently writing the
# README/report tables from the tainted row. This box may or may not have matplotlib; either way render()
# must raise before it could return early for that reason, so this check does not special-case _mpl() at
# all - a render() that reordered the gate below the bail would still be a real regression on THIS box
# whenever matplotlib is absent, and this test would falsely pass on a box that happens to have it. So we
# assert the raise happens regardless, which is only true if the gate really does sit above the bail.
with isolated('WIRING: the refusal must fire from render() itself, not just from a...'):
    _canon_perf({"gw": {"best_cell": bc(added_latency_p99_us=-3.0)}})
    try:
        charts.render(chart_by_name("added_latency"))
        check("render() refuses a negative difference row (WIRING, not just the helper)", "no SystemExit raised", "SystemExit")
    except SystemExit as _e:
        _msg = str(_e)
        check("render() refuses a negative difference row (WIRING, not just the helper)",
              ("gw" in _msg and "added_latency_p99_us" in _msg), True)

# ── the README table says the SAME thing about the same envelope as the PNG ───────────────────────────
#
# The chart labelled a below_resolution difference "0 (≤ rig resolution)" while the report table
# printed "0 µs" for the identical envelope - one describing a rig limit, the other an exact
# measurement of no overhead, from one number. Both surfaces are published; they must not disagree.
with isolated('the README table says the SAME thing about the same envelope as the...'):
    _canon_perf({"p": {"best_cell": {**bc(), "added_latency_p99_us": _BR}}})
    _md = charts._report_md(charts._ranked(), "t", [])
    check("the report's latency cell discloses a below_resolution reading, never a flat '0 µs'",
          ("≤ rig resolution" in _md, "0.0 µs" in _md, "0 µs" in _md), (True, False, False))
    _canon_perf({"p": {"best_cell": bc(added_latency_p99_us=120)}})
    _md2 = charts._report_md(charts._ranked(), "t", [])
    check("a real measured latency still prints as a number", "120 µs" in _md2, True)


if _fail == 0:
    print("all charts.py validity-gate tests passed")
    sys.exit(0)
print("CHARTS.PY TESTS FAILED")
sys.exit(1)
