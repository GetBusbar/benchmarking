#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# bench-cost.py had no test at all. Its one selection function decides WHICH snapshot a cost report
# describes, and every wrong answer is silent: an older snapshot, another gateway's snapshot, or a
# snapshot from a different engine all produce a plausible table. Pinned here: newest-per-gateway
# selection, the gateway filter, the engine-prefix filter, and that a malformed snapshot is skipped
# rather than sinking the report.
#
#   python3 bench-cost_test.py
import importlib.util
import json
import os
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location("bench_cost", os.path.join(HERE, "bench-cost.py"))
bc = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(bc)

_fail = 0


def check(name, got, want):
    global _fail
    if got == want:
        print(f"ok   - {name}")
    else:
        print(f"FAIL - {name}: got {got!r}, want {want!r}")
        _fail = 1


def snap(gw, stamp, engine_sha):
    return (f"result_{gw}_{stamp}.json",
            {"gateway": gw, "measured_at": stamp, "rig": {"engine": {"commit": engine_sha}}})


with tempfile.TemporaryDirectory() as td:
    d = os.path.join(td, "results", "snapshots")
    os.makedirs(d)
    fixtures = [
        snap("alpha", "2026-07-01T00-00-00Z", "aaa1111"),
        snap("alpha", "2026-07-20T00-00-00Z", "bbb2222"),   # newer: must win for alpha
        snap("bravo", "2026-07-10T00-00-00Z", "aaa1111"),
    ]
    for fname, body in fixtures:
        with open(os.path.join(d, fname), "w") as f:
            json.dump(body, f)
    # A snapshot that does not parse must be SKIPPED, not sink the whole report.
    with open(os.path.join(d, "result_broken_2026-07-25.json"), "w") as f:
        f.write("{not json")
    # A snapshot with no gateway key describes nothing and is ignored.
    with open(os.path.join(d, "result_anon_2026-07-25.json"), "w") as f:
        json.dump({"measured_at": "x"}, f)

    old_cwd = os.getcwd()
    os.chdir(td)  # newest_snapshots globs relative to the working directory
    try:
        allsnaps = bc.newest_snapshots()
        check("every real gateway appears once", sorted(allsnaps), ["alpha", "bravo"])
        check("the NEWEST snapshot wins per gateway (older runs must not describe the current engine)",
              allsnaps["alpha"][1]["measured_at"], "2026-07-20T00-00-00Z")
        check("a malformed snapshot is skipped, never a crash and never a row",
              "broken" in allsnaps, False)

        only = bc.newest_snapshots(gw_filter="bravo")
        check("the gateway filter narrows to exactly that gateway", sorted(only), ["bravo"])

        pinned = bc.newest_snapshots(engine="aaa")
        check("the engine prefix filter keeps only matching engines", sorted(pinned), ["alpha", "bravo"])
        check("under an engine pin, the newest MATCHING snapshot wins (not the globally newest)",
              pinned["alpha"][1]["measured_at"], "2026-07-01T00-00-00Z")
        check("a non-matching engine prefix yields nothing rather than a fallback",
              bc.newest_snapshots(engine="zzz"), {})
    finally:
        os.chdir(old_cwd)

if _fail == 0:
    print("all bench-cost tests passed")
    sys.exit(0)
print("BENCH-COST TESTS FAILED")
sys.exit(1)
