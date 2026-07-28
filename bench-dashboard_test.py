#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Regression guard for bench-dashboard.py's row_for()/eta computation. `eta` of exactly 0.0 (every
# declared cell already served, so `left == 0`) is falsy in Python, and the render expression
# `hms(eta) if eta else (...)` treats that the same as "no estimate yet", printing '-' on a run that
# is actually finishing instead of `hms(0)` == '0m00s'.
#
# Run: python3 bench-dashboard_test.py
import importlib.util
import os
import sys

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


# All declared cells (2) are already served -> left == 0 -> eta == 0.0.
bd.local_state = lambda path: {"ip": "1.2.3.4", "start": 0, "terminal": None}
bd.remote_tail = lambda ip: ""
bd.declared_served = lambda gw: 2
bd.parse_progress = lambda text: (2, 2, 2, "done")

row = bd.row_for("fakegw", "/tmp/fake", 100)
check("eta of exactly 0.0 (all cells served) renders as an estimate, not '-'", row["eta"], "0m00s")

if _fail == 0:
    print("all bench-dashboard tests passed")
    sys.exit(0)
print("BENCH-DASHBOARD TESTS FAILED")
sys.exit(1)
