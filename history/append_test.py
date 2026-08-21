#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Regression guard for history/append.py — the append-only history collector must actually find AND
# PRESERVE the measurements a field run produces. The engine now writes only results/snapshots/<gw>.json
# (and the timestamped result_<gw>_<measured_at>.json alongside it); it no longer writes the retired
# per-suite directories that SUITES still scans. History is append-only, so a run that appends nothing —
# or appends an EMPTY row — can never be backfilled.
#
# THIS TEST USED TO PASS WHILE THE COLLECTOR SILENTLY WROTE HOLLOW ROWS. Its fixture put
# served/matrix_version/cells/upstreams at the TOP LEVEL (agreeing with a bug in append.py) instead of
# nested under data["matrix"] as the real schema does (engine/src/record.rs: Matrix), and it only
# asserted file-exists + a stdout substring — never the written record's field VALUES. So both the
# fixture and the buggy code were wrong together, and every real matrix run recorded a row carrying no
# served status, no cells, no diagonal perf/stream/memory. This now uses the real nested shape and
# asserts the parsed record's contents, plus the dedup contract on a second run.
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


tmp = tempfile.mkdtemp()
try:
    hist_dir = os.path.join(tmp, "history")
    os.makedirs(hist_dir)
    shutil.copy(APPEND, os.path.join(hist_dir, "append.py"))

    snap_dir = os.path.join(tmp, "results", "snapshots")
    os.makedirs(snap_dir)
    # THE REAL SCHEMA: measurements nest under data["matrix"] and data["memory"], not the top level.
    snapshot = {
        "schema_version": 2,
        "gateway": "bifrost",
        "build": "maximhq/bifrost:v1.6.4",     # top-level, preserved as-is
        "measured_at": "2026-07-27T04:01:53Z",
        "arch": "arm64",
        "hardware": "m7g.2xlarge",
        "memory": {"served": True, "idle_rss_mib": 100.0, "peak_rss_mib": 210.0},  # top-level RSS window
        "matrix": {
            "matrix_version": 3,
            "served": True,
            "egress_configured": ["openai"],
            "sweep_ttft_ms": 20,
            "p99_ceiling_ms": 10,
            "sweep_rung_selection": "majority",
            "cells": {"openai": {"served": True}},
            "upstreams": {
                "openai": {
                    "cells": {
                        "openai": {   # the served DIAGONAL cell _matrix_extra flattens
                            "served": True,
                            "perf": {"rps_max_proxy": 40000, "added_latency_p99_us": 105,
                                     "added_latency_p50_us": None},   # a NULL lane: must stay null
                            "stream": {"stream_served": True, "streams_sustained": 128,
                                       "added_ttft_p99_us": 3000},
                            "memory": {"served": True, "peak_rss_mib": 210.0,
                                       "growth_rate_mib_per_min": None},  # NULL lane: must stay null
                        }
                    }
                }
            },
        },
    }
    with open(os.path.join(snap_dir, "result_bifrost_2026-07-27T04-01-53Z.json"), "w") as f:
        json.dump(snapshot, f)

    proc = subprocess.run([sys.executable, os.path.join(hist_dir, "append.py")],
                          cwd=tmp, capture_output=True, text=True)

    hist_file = os.path.join(tmp, "results", "history", "bifrost.jsonl")
    check("a field run with only results/snapshots/*.json appends a history row",
          os.path.exists(hist_file) and "appended 1 record" in proc.stdout,
          f"stdout={proc.stdout!r} stderr={proc.stderr!r} hist_exists={os.path.exists(hist_file)}")
    check("a clean run exits 0", proc.returncode == 0, f"rc={proc.returncode} stderr={proc.stderr!r}")

    # ── assert the written record's CONTENTS, not just that a line exists ──────────────────────────
    lines = [l for l in open(hist_file).read().splitlines() if l.strip()] if os.path.exists(hist_file) else []
    check("exactly one history line was written", len(lines) == 1, f"lines={lines}")
    rec = json.loads(lines[0]) if lines else {}

    check("record: suite is matrix", rec.get("suite") == "matrix", f"rec={rec}")
    check("record: measured_at carried", rec.get("measured_at") == "2026-07-27T04:01:53Z", f"rec={rec}")
    check("record: top-level build preserved", rec.get("build") == "maximhq/bifrost:v1.6.4", f"rec={rec}")
    # The nesting fix (u11-correctness-1): these come from data['matrix'], not the top level.
    check("record: matrix.served is preserved (read from data['matrix'])", rec.get("served") is True, f"rec={rec}")
    check("record: matrix.matrix_version preserved", rec.get("matrix_version") == 3, f"rec={rec}")
    check("record: matrix.sweep_rung_selection preserved", rec.get("sweep_rung_selection") == "majority", f"rec={rec}")
    check("record: the per-egress served cells map is preserved",
          rec.get("cells") == {"openai": True}, f"cells={rec.get('cells')}")
    # _matrix_extra body (u11-tests-3): diagonal perf/stream/memory, with nulls preserved.
    check("record: diagonal_perf carries the served cell's throughput",
          (rec.get("diagonal_perf") or {}).get("openai", {}).get("rps_max_proxy") == 40000, f"rec={rec}")
    check("record: a NULL perf lane is preserved as null, not dropped or zeroed",
          "added_latency_p50_us" in (rec.get("diagonal_perf") or {}).get("openai", {})
          and rec["diagonal_perf"]["openai"]["added_latency_p50_us"] is None, f"rec={rec}")
    check("record: diagonal_stream carries the streaming numbers",
          (rec.get("diagonal_stream") or {}).get("openai", {}).get("streams_sustained") == 128, f"rec={rec}")
    check("record: diagonal_memory carries the per-cell RSS",
          (rec.get("diagonal_memory") or {}).get("openai", {}).get("peak_rss_mib") == 210.0, f"rec={rec}")
    check("record: a NULL memory lane is preserved as null",
          "growth_rate_mib_per_min" in (rec.get("diagonal_memory") or {}).get("openai", {})
          and rec["diagonal_memory"]["openai"]["growth_rate_mib_per_min"] is None, f"rec={rec}")
    check("record: the top-level memory window is preserved",
          (rec.get("memory") or {}).get("idle_rss_mib") == 100.0, f"rec={rec}")

    # ── the append-only dedup contract (u11-tests-2): a second run adds nothing ────────────────────
    proc2 = subprocess.run([sys.executable, os.path.join(hist_dir, "append.py")],
                           cwd=tmp, capture_output=True, text=True)
    lines2 = [l for l in open(hist_file).read().splitlines() if l.strip()]
    check("a second run over the same tree appends 0 records (dedup by suite+measured_at)",
          "appended 0 record" in proc2.stdout, f"stdout={proc2.stdout!r}")
    check("the history file gained no new lines on the second run", len(lines2) == 1, f"lines2={lines2}")

    # ── a parseable result missing measured_at is a DROPPED row, distinct from a dedup no-op ───────
    nomeas_dir = tempfile.mkdtemp()
    try:
        os.makedirs(os.path.join(nomeas_dir, "history"))
        shutil.copy(APPEND, os.path.join(nomeas_dir, "history", "append.py"))
        sd = os.path.join(nomeas_dir, "results", "snapshots")
        os.makedirs(sd)
        with open(os.path.join(sd, "result_x_nomeas.json"), "w") as f:
            json.dump({"gateway": "x", "matrix": {"served": True}}, f)  # NO measured_at
        p = subprocess.run([sys.executable, os.path.join(nomeas_dir, "history", "append.py")],
                           cwd=nomeas_dir, capture_output=True, text=True)
        check("a measured_at-less result warns and exits 1 (not a silent drop)",
              p.returncode == 1 and "NO measured_at" in p.stderr,
              f"rc={p.returncode} stderr={p.stderr!r}")
    finally:
        shutil.rmtree(nomeas_dir, ignore_errors=True)

    # ── a garbage existing history line is surfaced, not silently swallowed into a possible dup ────
    corrupt_dir = tempfile.mkdtemp()
    try:
        os.makedirs(os.path.join(corrupt_dir, "history"))
        shutil.copy(APPEND, os.path.join(corrupt_dir, "history", "append.py"))
        hd = os.path.join(corrupt_dir, "results", "history")
        os.makedirs(hd)
        with open(os.path.join(hd, "bifrost.jsonl"), "w") as f:
            f.write('{"suite":"matrix","measured_at":"2026-07-01T00:00:00Z"}\n')
            f.write('{"suite":"matrix","measured_at": TRUNCA')  # truncated/garbage line
        sd = os.path.join(corrupt_dir, "results", "snapshots")
        os.makedirs(sd)
        with open(os.path.join(sd, "result_bifrost_2026-08-01T00-00-00Z.json"), "w") as f:
            json.dump({"gateway": "bifrost", "measured_at": "2026-08-01T00:00:00Z",
                       "matrix": {"served": True}}, f)
        p = subprocess.run([sys.executable, os.path.join(corrupt_dir, "history", "append.py")],
                           cwd=corrupt_dir, capture_output=True, text=True)
        check("a garbage existing history line warns and exits 1 (not a silent pass)",
              p.returncode == 1 and "not valid JSON" in p.stderr,
              f"rc={p.returncode} stderr={p.stderr!r}")
    finally:
        shutil.rmtree(corrupt_dir, ignore_errors=True)
finally:
    shutil.rmtree(tmp, ignore_errors=True)


if _fail == 0:
    print("all history-append tests passed")
    sys.exit(0)
print("HISTORY-APPEND TESTS FAILED")
sys.exit(1)
