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


tmp = tempfile.mkdtemp()
try:
    hist_dir = os.path.join(tmp, "history")
    os.makedirs(hist_dir)
    shutil.copy(APPEND, os.path.join(hist_dir, "append.py"))

    snap_dir = os.path.join(tmp, "results", "snapshots")
    os.makedirs(snap_dir)
    snapshot = {
        "schema_version": 1,
        "gateway": "bifrost",
        "build": "maximhq/bifrost:v1.6.4",
        "measured_at": "2026-07-27T04:01:53Z",
        "arch": "arm64",
        "matrix_version": 1,
        "served": True,
        "cells": {"openai": {"served": True}},
        "upstreams": {},
    }
    with open(os.path.join(snap_dir, "result_bifrost_2026-07-27T04-01-53Z.json"), "w") as f:
        json.dump(snapshot, f)

    proc = subprocess.run([sys.executable, os.path.join(hist_dir, "append.py")],
                           cwd=tmp, capture_output=True, text=True)

    hist_file = os.path.join(tmp, "results", "history", "bifrost.jsonl")
    check("a field run with only results/snapshots/*.json appends a history row",
          os.path.exists(hist_file) and "appended 1 record" in proc.stdout,
          f"stdout={proc.stdout!r} stderr={proc.stderr!r} hist_exists={os.path.exists(hist_file)}")
finally:
    shutil.rmtree(tmp, ignore_errors=True)


if _fail == 0:
    print("all history-append tests passed")
    sys.exit(0)
print("HISTORY-APPEND TESTS FAILED")
sys.exit(1)
