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
import pathlib
import re
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
    # the envelope-carrying record _proj_streaming reads. `over` names the raw values; the seal turns them
    # into the correct envelope, exactly as gen-data does.
    #
    # `cpu_fps` IS NOT IN THE BASE RECORD ANY MORE, because the producer no longer emits it: across the 16
    # cells that published both it and `streams_sustained_fps`, 4 were INVERTED below the proven delivery
    # boundary, 5 were redundant within 1%, and 7 were measured at a concurrency where the delivery gate did
    # not hold. It is still ACCEPTED as an override, and there is a guard below that a stale bundle still
    # carrying it projects no streamcpu_* row keys - the retired metric's absence being the property under
    # test, rather than the metric.
    def stream(**over):
        raw = dict(added_ttft_p99_us=90, added_gap_p99_us=12,
                   streams_sustained=1300, streams_sustained_fps=40000, streams_sustained_mock_bound=False)
        raw.update(over)
        rec = {"stream_served": True, "path": {"dialect": "openai"}, "source": _SRC,
               "added_ttft_p99_us": _seal(raw["added_ttft_p99_us"]),
               "added_gap_p99_us": _seal(raw["added_gap_p99_us"]),
               "streams_sustained_fps": _seal(raw["streams_sustained_fps"], gated=True, flag=raw["streams_sustained_mock_bound"], zero_note="measured_failure"),
               "streams_sustained": _seal(raw["streams_sustained"], gated=True, flag=raw["streams_sustained_mock_bound"], zero_note="measured_failure")}
        if "cpu_fps" in raw:   # a STALE bundle: sealed, present, and required to be ignored
            rec["cpu_fps"] = _seal(raw["cpu_fps"], gated=True, flag=raw.get("cpu_fps_mock_bound", False),
                                   zero_note="measured_failure")
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
    check("_proj_streaming: certified sustained (present, >0) -> stream_sustained_valid True", row["stream_sustained_valid"], True)

    # ── THE cpu_fps GATES ARE INVERTED, NOT DELETED ──────────────────────────────────────────────────
    # This block used to assert four things about `streamcpu_valid` (certified -> True, mock_bound True ->
    # False, mock_bound None -> False, measured 0 -> a real reading). All four described a metric that is
    # retired at the producer, so the assertions cannot be kept as they were. Deleting them outright would
    # leave nothing pinning the removal: a projection that started re-emitting `streamcpu_*` keys - or a
    # revert of the deletion - would break no test. So the PROPERTY UNDER TEST IS NOW THE ABSENCE, driven by
    # a fixture that deliberately still carries the retired field, which is exactly the shape of every
    # snapshot on disk that predates the removal.
    check("cpu_fps is not in the base stream fixture (the producer no longer emits it)",
          "cpu_fps" in stream(), False)
    _canon({"g": stream(cpu_fps=48000)})          # a STALE bundle, still carrying a certified cpu_fps
    _stale = charts._proj_streaming("g")
    check("RETIRED: a stale bundle's cpu_fps projects NO streamcpu_frames_per_sec row key",
          "streamcpu_frames_per_sec" in _stale, False)
    check("RETIRED: a stale bundle's cpu_fps projects NO streamcpu_valid gate",
          "streamcpu_valid" in _stale, False)
    check("RETIRED: no streamcpu_* row key survives the projection at all",
          [k for k in _stale if k.startswith("streamcpu")], [])
    check("RETIRED: a stale cpu_fps does not smuggle in a _cpu_fps_reason either",
          [k for k in _stale if "cpu_fps" in k], [])
    # ...and the metric that SURVIVED the cull is still projected. `streams_sustained_fps` is the
    # delivery-proven frame rate - measured at a concurrency where every expected frame arrived - which is
    # why it is the one that stayed. A cull that also lost it would be a regression, not a simplification.
    check("SURVIVED: streams_sustained_fps is still projected (the delivery-proven frame rate)",
          _stale["stream_sustained_fps"], 40000)
    check("RETIRED: no chart reads the streamcpu lane any more",
          [c.name for c in charts.CHARTS if c.suite == "streamcpu"], [])
    check("RETIRED: 'streamcpu' is not a projected suite",
          "streamcpu" in charts._PROJECTED_SUITES, False)

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
# This block ranked `streamcpu_fps`, whose chart is deleted with its metric. The ELIGIBILITY RULE it was
# testing is still live and is now carried by the frontier ranked bar (served_field=rps_at_bound_valid), so
# the check moves rather than dies - see the frontier block further down for the by-name coverage. What is
# pinned here is that the retired chart is gone from CHARTS at all, which is what `chart_by_name` raising
# would otherwise have reported as an error in the test rather than as the intended state.
with isolated('_topn_keys: only VALID served rows are eligible; ranking direction ...'):
    for _gone in ("streamcpu_fps", "rps_max_proxy", "rps_sustained_20ms", "xlate_rps_sustained_20ms"):
        check(f"RETIRED: no chart named {_gone} remains in CHARTS",
              any(c.name == _gone for c in charts.CHARTS), False)

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


    # fr(): a SEALED FRONTIER - one reading per declared tail-latency bound, ascending, unbounded last, in
    # the shape seal.mjs's sealFrontier emits. This replaces the two scalar-throughput fixtures
    # (`rps_max_proxy` / `rps_sustained_20ms` with their mock_bound flags), which described metrics the
    # producer retired: they were one concurrency sweep collapsed twice by different algorithms, and the pair
    # could invert against each other in the field.
    #
    # Every state a surface has to render is expressible, because each one is rendered DIFFERENTLY and the
    # difference is the whole contract:
    #   rates={b: v}   a reading whose rate is present (v may be 0 - a measured "nothing held this bound")
    #   a bound absent from `rates` but not in `omit` -> a reading whose rate is ABSENT, carrying `reason`
    #   omit=(b,)      NO reading at all for that bound (a record that never published it)
    #   lower_bound=   the sweep ran out of ladder: the rate is a FLOOR, and must render as one
    def fr(rates=None, omit=(), lower_bound=(), reason="not_measured",
           conc=64, p99_us=9000, first_disq=128):
        if rates is None:      # a plain monotone frontier: tight tails cost throughput
            rates = {1: 9000, 5: 17000, 10: 22000, 50: 25000, 100: 25500, None: 26000}
        out = []
        for b in charts.FRONTIER_BOUNDS_MS + [None]:
            if b in omit:
                continue
            v = rates.get(b) if b in rates else None
            out.append({
                "bound_ms": b,
                "rps": (_seal(v) if v is not None
                        else {"value": None, "certified": False, "suppressed": False, "reason": reason}),
                "concurrency": conc if v is not None else None,
                "p99_us": p99_us if v is not None else None,
                "first_disqualified_conc": None if b in lower_bound else first_disq,
                "lower_bound": b in lower_bound,
            })
        return out


    # A concurrency sweep, the plain (UNSEALED) evidence every frontier reading is derived from. Plain on
    # purpose: gen-data publishes it unsealed so a reader can re-derive the readings, and _proj_sweep must
    # read it without mval() - which would reject a bare scalar.
    def sweep(rungs=((1, 4000, 250, 0), (2, 8000, 320, 0), (4, 16000, 570, 0), (8, 22000, 9000, 0))):
        return [{"conc": c, "rps": r, "p99_us": p, "fail": f} for c, r, p, f in rungs]


    def bc(added_latency_p99_us=120, added_latency_p50_us=40, frontier=None, dialect="openai",
           rungs=None, direct_c1_p99_us=32):
        """A SEALED best_cell fixture: raw intent -> the envelope shape gen-data emits."""
        rec = {
            "path": {"ingress": dialect, "egress": dialect, "dialect": dialect}, "source": _BC_SRC,
            "added_latency_p50_us": _seal(added_latency_p50_us),
            "added_latency_p99_us": _seal(added_latency_p99_us),
            "frontier": fr() if frontier is None else frontier,
            "sweep": sweep() if rungs is None else sweep(rungs),
            # The only measured basis for the climb's zero-overhead reference line.
            "direct_c1_p99_us": _seal(direct_c1_p99_us),
            "gateway_c1_p99_us": _seal(direct_c1_p99_us + added_latency_p99_us
                                       if isinstance(added_latency_p99_us, (int, float)) else None),
        }
        return rec


    def tc(ingress="openai", egress="anthropic", added_latency_p99_us=200,
           added_latency_p50_us=None, frontier=None):
        """A SEALED translation_cell fixture. It carries its OWN frontier, off its own sweep."""
        rec = {"path": {"ingress": ingress, "egress": egress},
               "source": {"kind": "matrix", "sweep": "6x6-translation", "build": "img:1", "measured_at": "2026-07-24T00:00:00Z"},
               "added_latency_p99_us": _seal(added_latency_p99_us),
               "frontier": (fr({1: 6000, 5: 12000, 10: 15000, 50: 16000, 100: 16200, None: 16500})
                            if frontier is None else frontier)}
        if added_latency_p50_us is not None:
            rec["added_latency_p50_us"] = _seal(added_latency_p50_us)
        return rec


    # A gateway with ONLY a best_cell (no results/perf/<key>.json on disk at all).
    _canon_perf({"matrixonly": {"best_cell": bc()}})
    perf_rows = charts._load("perf")
    perf_keys = {r["_key"] for r in perf_rows}
    check("HIGH-1: a matrix-only gateway (best_cell, no results/perf file) appears as a perf chart row",
          "matrixonly" in perf_keys, True)
    # The projected row carries the FRONTIER READING AT THE BOARD'S DEFAULT BOUND, which is what replaced
    # the canonical sustained scalar this used to read. Same assertion - the number comes from best_cell and
    # not from a retired file on disk - against the metric that now exists.
    check("HIGH-1: the projected perf row carries the frontier reading at the default bound (from best_cell, not disk)",
          next(r["rps_at_bound"] for r in perf_rows if r["_key"] == "matrixonly"), 22000)
    check("HIGH-1: ...and the whole frontier travels with it, so the shape chart can be drawn",
          len(next(r["_frontier"] for r in perf_rows if r["_key"] == "matrixonly")),
          len(charts.FRONTIER_BOUNDS_MS) + 1)
    check("HIGH-1: ...and so does the unsealed sweep the readings are derived from (the climb)",
          len(next(r["_sweep"] for r in perf_rows if r["_key"] == "matrixonly")), 4)

    # A gateway with NO best_cell is absent from the perf charts (never served) - no retired-file read.
    _canon_perf({"noserve": {}})
    check("HIGH-1: a gateway with no best_cell is absent from the perf charts", charts._load("perf"), [])

    # _merge (README leaderboard) enumerates CANON, so the matrix-only gateway appears in the report too.
    _canon_perf({"matrixonly": {"best_cell": bc(),
                                **_mem(idle_rss_mib=30, steady_state_rss_mib=90)}})
    merged = charts._merge()
    check("HIGH-1: _merge (report leaderboard) includes a matrix-only gateway (best_cell, no disk perf)",
          "matrixonly" in merged, True)
    check("HIGH-1: the merged report row carries the frontier reading at the default bound",
          merged["matrixonly"]["rps_at_bound"], 22000)

    # xlate charts project from translation_cell, not results/xlate/<key>.json.
    _canon_perf({"xl": {"translation_cell": tc()}})
    xrows = charts._load("xlate")
    check("HIGH-1: a matrix-only gateway with a translation_cell appears as an xlate chart row",
          {r["_key"] for r in xrows}, {"xl"})
    check("HIGH-1: _suite_map('xlate') enumerates translation_cell (report translation table)",
          "xl" in charts._suite_map("xlate"), True)

    # The README translation TABLE must render an ABSENT translated throughput as an absence, never as a
    # number, and must name the RECORD'S OWN reason for it.
    #
    # This block was FINDING 24, about a mock-bound (rig-limited) value leaking its raw number into the
    # table. The suppression layer it tested is retired - a measurement near the rig's ceiling is now
    # published with the fraction of that ceiling it reached, and no producer can set the flag - so
    # "rig-limited" is no longer a state this column can be in. What survives, and is what the finding was
    # really about, is that an absent reading must not print a number and must not be captioned with a cause
    # the record does not give. `search_exhausted` is used here because it is the reason the field artifacts
    # actually carried while the old code printed "(rig-limited)" over it.
    _canon_perf({
        "xcert": {"best_cell": bc(),
                  "translation_cell": tc(frontier=fr({1: 6000, 5: 12000, 10: 15000, None: 16500}))},
        "xgone": {"best_cell": bc(),
                  "translation_cell": tc(frontier=fr({50: 99999, None: 99999}, reason="search_exhausted"))},
    })
    md = charts._report_md(list(charts._merge().items()), "t", [])
    check("a PRESENT translated frontier reading prints its value in the README table", "15,000" in md, True)
    check("an ABSENT translated reading does NOT leak a number from another bound into its cell",
          "99,999" in md, False)
    check("an ABSENT translated reading is captioned with the RECORD'S OWN reason, not a blanket cause",
          "still climbing when the range ran out" in md, True)
    check("...and no surface invents 'rig-limited' for a reason the record never gave",
          "rig-limited" in md, False)

    # HIGH-1 (consistency): EVERY gateway with a best_cell appears as a perf chart row, and vice-versa
    # (chart-row presence <=> best_cell presence). This is the assertion the audit asks for, enforced here
    # by construction of the projection.
    _canon_perf({"a": {"best_cell": bc()}, "b": {"best_cell": bc()}, "c": {}})
    bc_keys = {k for k, g in charts.CANON.items() if g.get("best_cell")}
    row_keys = {r["_key"] for r in charts._load("perf")}
    check("HIGH-1: chart-row presence == best_cell presence (every best_cell is a row and vice-versa)",
          row_keys, bc_keys)


# ── the throughput bar is gated on THIS BOUND's own reading carrying a rate ───────────────────────────
#
# This was MED-3, "the passthrough RPS charts gate the bar on the mock-bound honesty flag", over the two
# retired scalars and the retired suppression layer. Both halves of that are gone: the scalars are replaced
# by the frontier, and a measurement near the rig's ceiling is now published with the fraction it reached
# rather than withheld, so there is no mock-bound flag left to gate on.
#
# The RULE the section existed to protect is still live and still worth a red test: a bar may be drawn, and
# a gateway may be ranked, ONLY on a reading that actually carries a rate at the bound the chart names.
# What changed is that validity is now per-READING rather than per-metric, which makes a new mistake
# possible and worth pinning: a gateway with a huge rate at a LOOSER bound must not rank on the chart for a
# TIGHTER one. That is the frontier's whole point, and a naive "does this record have any throughput"
# gate would get it backwards.
with isolated('the throughput bar is gated on THIS BOUND own reading carrying a rate'):
    _B = charts.DEFAULT_BOUND_MS
    _canon_perf({
        # holds the default bound: eligible, and its rate is the one at THAT bound (not its best)
        "clean": {"best_cell": bc(frontier=fr({1: 5000, 5: 15000, _B: 20000, None: 40000}))},
        # a far higher rate, but only once the bound is relaxed past the one this chart names
        "looser": {"best_cell": bc(frontier=fr({50: 99999, 100: 99999, None: 99999}))},
        # a reading exists at this bound but its rate is absent, with the engine's own reason
        "absent": {"best_cell": bc(frontier=fr({None: 88888}, reason="search_exhausted"))},
        # the sweep ran and NO rung held this bound while failing nothing: a measured 0, which is a number
        "zero":   {"best_cell": bc(frontier=fr({_B: 0, None: 777}))},
        # no frontier at all (every snapshot predating the frontier)
        "norec":  {"best_cell": bc(frontier=[])},
    })
    prows = {r["_key"]: r for r in charts._load("perf")}
    check("a reading carrying a rate at the named bound is valid", prows["clean"]["rps_at_bound_valid"], True)
    check("...and the row carries the rate AT THAT BOUND, not the gateway's best",
          prows["clean"]["rps_at_bound"], 20000)
    check("a rate only at a LOOSER bound is NOT valid at this one", prows["looser"]["rps_at_bound_valid"], False)
    check("an absent rate is NOT valid", prows["absent"]["rps_at_bound_valid"], False)
    check("...and the record's OWN reason travels for the caption, under the name _absent_cause looks up",
          prows["absent"]["_rps_at_bound_reason"], "search_exhausted")
    check("a MEASURED 0 at this bound IS valid (it is a number, not an absence)",
          prows["zero"]["rps_at_bound_valid"], True)
    check("a record with NO frontier is not valid", prows["norec"]["rps_at_bound_valid"], False)
    check("...and asserts NO reason, because the record gives none",
          "_rps_at_bound_reason" in prows["norec"], False)

    topn = charts._topn_keys(chart_by_name("frontier_rps_at_bound"), n=5)
    check("a gateway whose rate is only at a looser bound is out of the top-N despite the highest raw value",
          "looser" in topn, False)
    check("an absent-rate gateway is out of the top-N", "absent" in topn, False)
    check("a record with no frontier is out of the top-N", "norec" in topn, False)
    check("the gateway that held the named bound IS ranked", "clean" in topn, True)
    # A measured 0 is a real reading but not a positive one, so it is ranked only where a 0 is the winning
    # end - which a higher-is-better throughput chart is not (zero_ok is False on it).
    check("a measured 0 is not ranked on a higher-is-better chart (zero_ok is False)", "zero" in topn, False)

    # THE BOUND IS IN THE TITLE, rendered from the constant, on every chart that shows one bound. This is the
    # non-negotiable the retired board broke: it captioned numbers "p99 < 1 s" while the engine enforced
    # 20 ms - a bar 96% of all 1632 recorded rungs pass, against 57% for the real one.
    for _n in ("frontier_rps_at_bound", "xlate_frontier_rps_at_bound"):
        check(f"{_n}: names its bound in the title", f"{_B:g} ms" in chart_by_name(_n).title, True)
    check("no chart caption claims the 1 s bar the engine never enforced",
          [c.name for c in charts.CHARTS
           if "1 s" in str(c.title) or (isinstance(c.subtitle, str) and "1 s" in c.subtitle)
           or (isinstance(c.subtitle, str) and "< 1s" in c.subtitle)], [])
    check("...and neither does the Chart default zero_text (it names no bound at all)",
          "1 s" in charts.Chart.zero_text, False)

    # THE FLOOR RENDERS AS A FLOOR. A reading the sweep never found a ceiling for is a lower bound, and the
    # bar must say so on the NUMBER (numlab_prefix), not only in the prose beside it.
    _canon_perf({"floor": {"best_cell": bc(frontier=fr({_B: 12000, None: 12000}, lower_bound=(_B, None)))}})
    _fl = {r["_key"]: r for r in charts._load("perf")}["floor"]
    check("a lower_bound reading is flagged on the row", _fl["_rps_at_bound_lower_bound"], True)
    check("...and the bar's number is prefixed with the floor marker",
          chart_by_name("frontier_rps_at_bound").numlab_prefix(_fl), "≥ ")
    check("...while a bounded reading gets no prefix",
          chart_by_name("frontier_rps_at_bound").numlab_prefix(prows["clean"]), "")
    _md_fl = charts._report_md(list(charts._merge().items()), "t", [])
    check("...and the report table prints it as a floor too, never as a bare ceiling",
          "≥ 12,000" in _md_fl, True)


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
# because the honest answer there is not a number at all. Its growth rate is the finding, and the bar
# publishes that rate instead of substituting a peak (which would report when the load stopped).
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
    check("memory_rss: a bar with no steady state publishes the growth rate, signed and united",
          "+42.5 MiB/min under load" in (charts._mem_annot(mrows["leaks"]) or ""), True)
    # THE RATE IS THE FINDING, AND ONLY THE RATE. A verdict layered on top of it ("never settled") reads
    # as the board calling a gateway out rather than reporting a measurement, so no chart string may
    # carry that wording again.
    check("memory_rss: the bar carries no verdict wording, only the measurement",
          "settl" in (charts._mem_annot(mrows["leaks"]) or "").lower(), False)
    # A window still RELEASING memory at the cap has a NEGATIVE rate; "+" hard-coded in front of it
    # printed "+-3.2", and a reader who trusted the sign would have read a leak off a gateway giving
    # memory back.
    charts.CANON["releases"] = _mem(idle_rss_mib=20, steady_state_rss_mib=None, plateaued=False,
                                    growth_rate_mib_per_min=-3.2)
    charts.GATEWAYS = {k: k for k in charts.CANON}
    _rel = {r["_key"]: r for r in charts._load("memory")}["releases"]
    check("memory_rss: a negative rate keeps its own sign", "-3.2 MiB/min under load" in (charts._mem_annot(_rel) or ""), True)
    del charts.CANON["releases"]
    charts.GATEWAYS = {k: k for k in charts.CANON}
    mrows = {r["_key"]: r for r in charts._load("memory")}
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
    # A NON-difference chart, which used to be rps_max_proxy (deleted with its metric). Any throughput chart
    # serves: the point is that this gate has no opinion outside difference metrics, because a negative RSS
    # or a negative rate is a different bug entirely and not this function's to name.
    check("the refusal has no opinion about a non-difference chart",
          charts._negative_diff_violations(chart_by_name("frontier_rps_at_bound"),
                                           [{"_key": "g", "rps_at_bound": -1}]), [])
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
    # THE CELL, NOT THE PAGE. This asserted `"0 µs" not in _md` over the whole document, which is a
    # substring test that any measured latency ending in a zero satisfies - "250 µs" contains "0 µs". It
    # passed only because no other µs figure happened to be on the page; the climb table's "p99 at lowest c"
    # column put one there and the assertion started failing on correct output. Anchored to the table
    # delimiters it tests what it was written to test: that no CELL is a flat "0 µs".
    check("the report's latency cell discloses a below_resolution reading, never a flat '0 µs'",
          ("≤ rig resolution" in _md, "| 0.0 µs " in _md, "| 0 µs " in _md), (True, False, False))
    _canon_perf({"p": {"best_cell": bc(added_latency_p99_us=120)}})
    _md2 = charts._report_md(charts._ranked(), "t", [])
    check("a real measured latency still prints as a number", "120 µs" in _md2, True)

# ── write_reports() must survive a non-UTF-8 locale (routine in a minimal CI container) ───────────────
#
# _report_md's body embeds literal non-ASCII glyphs (✕, ⚠, µs, ≤, ·, ...) into every report it writes.
# write_reports() saves both README.md files with Path.write_text() and no encoding=, so when the box's
# locale is C/POSIX with no UTF-8 coercion (PYTHONUTF8=0 PYTHONCOERCECLOCALE=0 LC_ALL=C LANG=C - routine
# in a minimal CI container, and NOT reproducible by monkeypatching locale.getpreferredencoding() from
# Python: CPython's TextIOWrapper resolves the "None" encoding at the C level, so only a real subprocess
# with that environment exercises it) that write raises UnicodeEncodeError and BOTH reports are never
# written - the exact regression this pins.
with isolated("write_reports() must survive a non-UTF-8 locale"):
    import subprocess
    import tempfile

    with tempfile.TemporaryDirectory() as _tmp:
        _row = {"hardware": "test-rig", "measured_at": "2026-07-24T00:00:00Z",
                "added_latency_p99_us": 120, "served": True}
        _script = (
            "import json, os, sys; sys.path.insert(0, %r)\n"
            "_created = False\n"
            "if not os.path.exists(_DATA := os.path.join(%r, 'site', 'data.json')):\n"
            "    os.makedirs(os.path.dirname(_DATA), exist_ok=True)\n"
            "    with open(_DATA, 'w') as _f: json.dump({'gateways': []}, _f)\n"
            "    _created = True\n"
            "import charts, pathlib\n"
            "if _created: os.remove(_DATA)\n"
            "charts.RESULTS = pathlib.Path(%r) / 'results'\n"
            "charts.GATEWAYS = {'p': 'p'}\n"
            "charts._ranked = lambda: [('p', %r)]\n"
            "charts.write_reports()\n"
        ) % (HERE, HERE, _tmp, _row)
        _env = dict(os.environ, PYTHONUTF8="0", PYTHONCOERCECLOCALE="0", LC_ALL="C", LANG="C")
        _proc = subprocess.run([sys.executable, "-c", _script], env=_env,
                                capture_output=True, text=True)
        _all_readme = os.path.join(_tmp, "results", "reports", "all", "README.md")
        _top5_readme = os.path.join(_tmp, "results", "reports", "top5", "README.md")
        check("write_reports() writes both README.md files under an ASCII-only (LC_ALL=C) locale",
              (_proc.returncode == 0, os.path.exists(_all_readme), os.path.exists(_top5_readme)),
              (True, True, True))

# ── every read_text(/write_text( call in charts.py must pin an explicit encoding ──────────────────────
#
# The locale case above pins the two write sites and the manifest read site by hand, but nothing stops
# the next read_text/write_text call site from being added without encoding= and silently reopening the
# same class of bug. Scoped to charts.py only - this is not a repo-wide lint.
with isolated("every read_text(/write_text( call in charts.py carries an explicit encoding="):
    _src = (pathlib.Path(HERE) / "charts.py").read_text(encoding="utf-8")
    _bad = []
    for _m in re.finditer(r"\.(read_text|write_text)\(", _src):
        _start = _m.end()
        _depth = 1
        _i = _start
        while _depth > 0 and _i < len(_src):
            if _src[_i] == "(":
                _depth += 1
            elif _src[_i] == ")":
                _depth -= 1
            _i += 1
        _call_args = _src[_start:_i - 1]
        if "encoding=" not in _call_args:
            _line_no = _src.count("\n", 0, _m.start()) + 1
            _bad.append(f"{_m.group(1)}() at line {_line_no}")
    check("every read_text(/write_text( call in charts.py carries an explicit encoding=", _bad, [])


if _fail == 0:
    print("all charts.py validity-gate tests passed")
    sys.exit(0)
print("CHARTS.PY TESTS FAILED")
sys.exit(1)
