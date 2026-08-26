#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Coverage siblings to bench-dashboard_test.py: the dashboard is the operator's only view of 13
# boxes billing by the hour, so every number it prints has a way to be wrong that costs money.
# Pinned here: the fail-CLOSED served-cell counter (the "10/8 served, ETA zero" regression), the
# phantom-gateway filter on fanout logs, the midnight-crossing elapsed clock, the phase-weighted
# early ETA, and the hms/phase-fraction helpers' documented boundaries.
#
#   python3 bench-dashboard_coverage_test.py
import importlib.util
import os
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location("bench_dashboard", os.path.join(HERE, "bench-dashboard.py"))
bd = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(bd)

_fail = 0


def check(name, got, want):
    global _fail
    if got == want:
        print(f"ok   - {name}")
    else:
        print(f"FAIL - {name}: got {got!r}, want {want!r}")
        _fail = 1


# ── hms: the one formatter every duration goes through ───────────────────────────────────────────
check("hms: None is '-' (no estimate is not zero minutes)", bd.hms(None), "-")
check("hms: negative is '-' (a clock running backwards is not an ETA)", bd.hms(-1), "-")
check("hms: zero is 0m00s, a real 'finishing now', never '-'", bd.hms(0), "0m00s")
check("hms: under an hour is m/s", bd.hms(3599), "59m59s")
check("hms: an hour turns h/m", bd.hms(3600), "1h00m")
check("hms: hours drop the seconds", bd.hms(7325), "2h02m")

# ── cell_fraction_done: the early-ETA prior ──────────────────────────────────────────────────────
check("phase fraction: unknown phase contributes nothing", bd.cell_fraction_done(None), 0.0)
check("phase fraction: an unrecognised phase name contributes nothing (fail-closed)",
      bd.cell_fraction_done("warp_drive"), 0.0)
check("phase fraction: the FIRST phase means the cell has just begun",
      bd.cell_fraction_done(bd.PHASE_ORDER[0]), 0.0)
# The fractions are cumulative in engine order and never reach past the whole cell.
prev = -1.0
monotone = True
for name in bd.PHASE_ORDER:
    f = bd.cell_fraction_done(name)
    if f < prev:
        monotone = False
    prev = f
check("phase fraction: cumulative along the engine's own phase order", monotone, True)
check("phase fraction: the last phase starts inside the cell, not past it",
      0.0 < bd.cell_fraction_done(bd.PHASE_ORDER[-1]) < 1.0, True)
check("phase fraction: the declared weights sum to ~1 (they are shares of one cell)",
      abs(sum(bd.PHASE_COST.values()) - 1.0) < 0.01, True)

# ── parse_progress: the fail-CLOSED served counter ───────────────────────────────────────────────
# Guards against counting any `[cell N/M] id: <verdict>` line as served unless the verdict is a
# known MEASURED one; an allowlist of cheap verdicts previously let same-prefix lines (e.g. cost
# breakdowns) double-count as served.
log = "\n".join([
    "[cell 1/36] openai>openai: served",
    "[cell 2/36] openai>anthropic: failed (HTTP 500 across the retry budget)",
    "[cell 3/36] openai>gemini: not_configurable",
    "[cell 4/36] openai>cohere: untestable",
    "[cell 5/36] openai>bedrock: unprobed_auth",
    "[cell 5/36] openai>bedrock: cost throughput=120s memory=200s",  # the regression line shape
    "[phase] openai>bedrock memory",
])
# `complete` flags when the log tail starts mid-grid, making served_done a floor rather than a
# total. Unpacked here so a tuple-shape change breaks this test loudly.
cells, total, served_done, phase, complete = bd.parse_progress(log)
check("parse_progress: latest cell position wins", (cells, total), (5, 36))
check("parse_progress: ONLY measured verdicts count as served (2 of 6 lines)", served_done, 2)
check("parse_progress: an unrecognised same-prefix line is ignored, never believed",
      served_done <= 5, True)
check("parse_progress: the latest phase is reported with its cell id", phase, "memory openai>bedrock")
# An empty tail reports complete=True: `complete` only qualifies a COUNT, and with no cell line
# there is no count to bound (cells is None; caller renders "-"). The fail-closed case is below —
# a tail that DID see cells but missed the start.
check("parse_progress: an empty tail has no position, no phase, and nothing to bound",
      bd.parse_progress(""), (None, None, 0, None, True))
check("parse_progress: a log carrying every cell position from 1 is complete", complete, True)
# A tail that starts mid-grid knows it can't count, so the dashboard can say 'at least N' rather
# than passing a floor off as a reading.
_, _, _, _, partial = bd.parse_progress("[cell 7/36] openai>openai: served")
check("parse_progress: a tail starting mid-grid reports itself incomplete", partial, False)

# ── local_state: what the orchestrator's log yields ──────────────────────────────────────────────
with tempfile.TemporaryDirectory() as td:
    p = os.path.join(td, "fanout-x.log")
    with open(p, "w") as f:
        f.write("[10:00:00] launching box\n"
                "[10:00:05] ip=1.2.3.4 up\n"
                "[10:07:00] running x now\n")
    st = bd.local_state(p)
    check("local_state: ip is read from the log", st["ip"], "1.2.3.4")
    check("local_state: start is the FIRST timestamp (wall clock the operator pays from)",
          st["start"], 10 * 3600)
    check("local_state: measuring starts at 'running', not at launch (build time is not cell time)",
          st["measuring"], 10 * 3600 + 7 * 60)
    check("local_state: no terminal verdict while the run is live", st["terminal"], None)

    with open(p, "a") as f:
        f.write("[12:00:00] DONE\n")
    check("local_state: DONE is terminal", bd.local_state(p)["terminal"], "DONE")
    with open(p, "a") as f:
        f.write("[12:01:00] INCOMPLETE after retries\n")
    check("local_state: a later INCOMPLETE overrides (the last word wins)",
          bd.local_state(p)["terminal"], "INCOMPLETE")

check("local_state: an unreadable log is an empty state, not a crash",
      bd.local_state("/nonexistent/nowhere.log"), {})

# ── fanout_logs: the phantom-gateway filter ──────────────────────────────────────────────────────
# An orchestration log (fanout-validate-*.log) is not a gateway, and used to render as a finished
# gateway that measured nothing. Only names with a gateways/<name>/definition.json may appear.
with tempfile.TemporaryDirectory() as td:
    os.makedirs(os.path.join(td, "gateways", "realgw"))
    with open(os.path.join(td, "gateways", "realgw", "definition.json"), "w") as f:
        f.write("{}")
    os.makedirs(os.path.join(td, "results"))
    for name in ["fanout-realgw.log", "fanout-validate-realgw.log", "fanout-field-abc123.log", "unrelated.txt"]:
        with open(os.path.join(td, "results", name), "w") as f:
            f.write("x")
    old = bd.HERE
    bd.HERE = td
    try:
        logs = bd.fanout_logs()
    finally:
        bd.HERE = old
    check("fanout_logs: a real gateway's log is discovered", "realgw" in logs, True)
    check("fanout_logs: orchestration logs never become phantom gateway rows",
          sorted(logs), ["realgw"])

# ── row_for: the assembled row, with the remote leg stubbed ──────────────────────────────────────
bd.remote_tail = lambda ip: ""
bd.declared_served = lambda gw: 8

with tempfile.TemporaryDirectory() as td:
    # Terminal rows: no SSH, no numbers, just the verdict.
    p = os.path.join(td, "done.log")
    with open(p, "w") as f:
        f.write("[09:00:00] ip=1.2.3.4\n[11:00:00] DONE\n")
    row = bd.row_for("gw", p, 12 * 3600)
    check("row_for: a DONE box is done and never bad", (row["done"], row["bad"]), (True, False))
    check("row_for: a terminal row carries no stale ETA", row["eta"], "-")

    p2 = os.path.join(td, "inc.log")
    with open(p2, "w") as f:
        f.write("[09:00:00] ip=1.2.3.4\nINCOMPLETE\n")
    row = bd.row_for("gw", p2, 12 * 3600)
    check("row_for: an INCOMPLETE box is bad and never done", (row["done"], row["bad"]), (False, True))

    # Midnight crossing: started 23:59:00, asked at 00:01:00 -> 2 minutes, not -23h58m.
    p3 = os.path.join(td, "mid.log")
    with open(p3, "w") as f:
        f.write("[23:59:00] ip=1.2.3.4 launched\n")
    row = bd.row_for("gw", p3, 60)
    check("row_for: elapsed survives midnight (2m, never negative)", row["elapsed"], "2m00s")

    # The early ETA: no cell has completed, but the phase prior gives the one in flight a fraction,
    # so an estimate exists exactly when the operator is deciding whether the run is healthy.
    bd.remote_tail = lambda ip: "[cell 1/36] openai>openai: probing\n[phase] openai>openai memory\n"
    p4 = os.path.join(td, "early.log")
    with open(p4, "w") as f:
        f.write("[10:00:00] ip=1.2.3.4\n[10:00:30] running gw\n")
    row = bd.row_for("gw", p4, 10 * 3600 + 10 * 60)
    check("row_for: an ETA exists before the first cell completes (phase-weighted prior)",
          row["eta"] not in ("-", "~"), True)
    check("row_for: served shows measured-vs-declared", row["served"], "0/8")

    # No progress signal at all: the honest '~', never a number.
    bd.remote_tail = lambda ip: ""
    row = bd.row_for("gw", p4, 10 * 3600 + 10 * 60)
    check("row_for: with nothing measured and no phase, the ETA is the honest '~'", row["eta"], "~")

if _fail == 0:
    print("all bench-dashboard coverage tests passed")
    sys.exit(0)
print("BENCH-DASHBOARD COVERAGE TESTS FAILED")
sys.exit(1)
