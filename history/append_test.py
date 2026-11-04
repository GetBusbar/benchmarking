#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Regression guard for history/append.py — the append-only history collector must actually find the
# results a field run produces. The engine now writes only results/snapshots/<gw>.json (and the
# timestamped result_<gw>_<measured_at>.json alongside it); it no longer writes the retired per-suite
# directories (results/perf, results/memory, results/matrix, ...) that SUITES still scans. A field run
# should therefore append a row for a fresh snapshot — history is append-only, so a run that appends
# nothing can never be backfilled.
#
# Run: python3 history/append_test.py
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
APPEND = os.path.join(HERE, "append.py")

_fail = 0


def check(name, cond, detail=""):
    global _fail
    if cond:
        print(f"ok   - {name}")
    else:
        print(f"FAIL - {name}: {detail}")
        _fail = 1


def run_append(snapshots, history=None, named=None):
    """Run a copy of append.py over a temp tree holding `snapshots` ({filename: doc}) and, optionally,
    a pre-existing history file per gateway ({gw: text}). `named` (a list of snapshot filenames) is
    passed as arguments - the run-on-ec2.sh mode. Returns (proc, {gw: [rows]})."""
    tmp = tempfile.mkdtemp()
    try:
        hist_dir = os.path.join(tmp, "history")
        os.makedirs(hist_dir)
        shutil.copy(APPEND, os.path.join(hist_dir, "append.py"))
        snap_dir = os.path.join(tmp, "results", "snapshots")
        os.makedirs(snap_dir)
        os.makedirs(os.path.join(tmp, "results", "history"))
        for gw, text in (history or {}).items():
            with open(os.path.join(tmp, "results", "history", gw + ".jsonl"), "w") as f:
                f.write(text)
        for name, doc in snapshots.items():
            with open(os.path.join(snap_dir, name), "w") as f:
                json.dump(doc, f)
        args = [os.path.join(snap_dir, n) for n in (named or [])]
        proc = subprocess.run([sys.executable, "-I", "-B", os.path.join(hist_dir, "append.py")] + args,
                              cwd=tmp, capture_output=True, text=True)
        rows = {}
        hd = os.path.join(tmp, "results", "history")
        for fn in os.listdir(hd):
            with open(os.path.join(hd, fn)) as f:
                rows[fn[:-len(".jsonl")]] = [ln for ln in f.read().split("\n") if ln]
        return proc, rows
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


# THE SHAPE THE ENGINE ACTUALLY WRITES (record::ResultSnapshot): metadata at the top, the grid nested
# under `matrix`. The fixture this file used to carry put cells/upstreams/served at the TOP level - a
# layout no engine writes - and asserted only that SOME row was appended, so it stayed green while all
# 190 committed history rows recorded no measurement at all.
FRONTIER = [{"p99_bound_us": 10000, "rps": 4969.0, "concurrency": 8, "p99_us": 5333,
             "first_disqualified_conc": 16, "lower_bound": False}]
CELL = {
    "served": True,
    "perf": {"frontier": FRONTIER, "added_latency_p50_us": 110, "added_latency_p99_us": 240,
             "cpu_us_per_request": 61.5, "egress_reverified": True},
    "stream": {"stream_served": True, "added_ttft_p99_us": 400, "streams_sustained": 512,
               "streams_sustained_fps": 9001.0},
    "memory": {"served": True, "idle_rss_mib": 41.0, "peak_rss_mib": None, "plateaued": True},
}


def snapshot(gw="bifrost", measured_at="2026-07-27T04:01:53Z"):
    return {
        "schema_version": 1,
        "gateway": gw,
        "build": "maximhq/bifrost:v1.6.4",
        "measured_at": measured_at,
        "arch": "arm64",
        "matrix": {
            "gateway": gw, "measured_at": measured_at, "matrix_version": 1, "served": True,
            "cells": {"openai": {"served": True}, "broken": None},
            "upstreams": {"openai": {"served": True, "cells": {"openai": CELL}}},
        },
    }


proc, rows = run_append({"result_bifrost_2026-07-27T04-01-53Z.json": snapshot()})
check("a field run with only results/snapshots/*.json appends a history row",
      len(rows.get("bifrost", [])) == 1 and "appended 1 record" in proc.stdout and proc.returncode == 0,
      f"rc={proc.returncode} stdout={proc.stdout!r} stderr={proc.stderr!r} rows={rows!r}")
row = json.loads(rows["bifrost"][0]) if rows.get("bifrost") else {}
check("the row carries the diagonal cell's frontier (the board's throughput metric)",
      (row.get("diagonal_perf") or {}).get("openai", {}).get("frontier") == FRONTIER, f"row={row!r}")
check("...its added latency and cost", (row.get("diagonal_perf") or {}).get("openai", {}).get(
      "cpu_us_per_request") == 61.5, f"row={row!r}")
check("...its streaming capacity", (row.get("diagonal_stream") or {}).get("openai", {}).get(
      "streams_sustained") == 512, f"row={row!r}")
check("...and its memory, with a null kept as a null (never a 0)",
      (row.get("diagonal_memory") or {}).get("openai") == {"served": True, "idle_rss_mib": 41.0,
                                                          "peak_rss_mib": None, "plateaued": True},
      f"row={row!r}")
check("...and the matrix-level fields that live under snapshot['matrix']",
      (row.get("matrix_version"), row.get("served")) == (1, True), f"row={row!r}")

# A row that cannot be recorded is a LOST row in an append-only history: it must fail the run.
unnamed = snapshot()
del unnamed["gateway"]
proc, rows = run_append({"result_x_2026-07-27T04-01-53Z.json": unnamed})
check("a snapshot with no gateway is reported and exits non-zero",
      proc.returncode == 1 and "no `gateway` field" in proc.stderr, f"rc={proc.returncode} {proc.stderr!r}")
untimed = snapshot()
del untimed["measured_at"]
del untimed["matrix"]["measured_at"]
proc, rows = run_append({"result_bifrost_x.json": untimed})
check("a snapshot with no measured_at is reported and exits non-zero",
      proc.returncode == 1 and "NOT RECORDED" in proc.stderr and not rows.get("bifrost"),
      f"rc={proc.returncode} {proc.stderr!r} rows={rows!r}")
# A damaged history line hides which run it recorded, so appending could duplicate it.
proc, rows = run_append({"result_bifrost_2026-07-27T04-01-53Z.json": snapshot()},
                        history={"bifrost": '{"suite":"matrix","measured_at":"2026-07-27T04:01:53Z"\n'})
check("a damaged history file is refused, not appended to",
      proc.returncode == 1 and len(rows.get("bifrost", [])) == 1, f"rc={proc.returncode} rows={rows!r}")
# Already recorded: append-only, nothing rewritten, a clean exit.
proc, rows = run_append({"result_bifrost_2026-07-27T04-01-53Z.json": snapshot()},
                        history={"bifrost": '{"suite":"matrix","measured_at":"2026-07-27T04:01:53Z"}\n'})
check("an already-recorded run appends nothing and exits 0",
      proc.returncode == 0 and len(rows.get("bifrost", [])) == 1, f"rc={proc.returncode} rows={rows!r}")

# NAMED MODE (what run-on-ec2.sh passes): only the snapshots this run's clean boxes pulled. A
# harvested or held-back snapshot of another gateway sitting on disk must not become this run's history.
proc, rows = run_append({"result_bifrost_2026-07-27T04-01-53Z.json": snapshot(),
                         "result_kong_2026-07-27T05-00-00Z.json": snapshot("kong", "2026-07-27T05:00:00Z")},
                        named=["result_bifrost_2026-07-27T04-01-53Z.json"])
check("named mode appends ONLY the named snapshot",
      proc.returncode == 0 and len(rows.get("bifrost", [])) == 1 and not rows.get("kong"),
      f"rc={proc.returncode} rows={rows!r} {proc.stderr!r}")
proc, rows = run_append({}, named=["result_gone_2026-07-27T04-01-53Z.json"])
check("a named snapshot that is missing is a lost row (non-zero), not an empty run",
      proc.returncode == 1 and "SKIPPED" in proc.stderr, f"rc={proc.returncode} {proc.stderr!r}")

if _fail == 0:
    print("all history-append tests passed")
    sys.exit(0)
print("HISTORY-APPEND TESTS FAILED")
sys.exit(1)
