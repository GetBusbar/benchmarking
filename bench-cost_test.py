#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Covers bench-cost's snapshot-selection function: which snapshot a cost report describes is a
# silent failure mode (wrong gateway, wrong age, wrong engine all produce a plausible table).
# Pins newest-per-gateway selection, the gateway/engine-prefix filters, and that a malformed
# snapshot is skipped rather than sinking the report.
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

    # Point bench-audit's path anchor at the fixture; do NOT chdir. snapshot_paths() resolves
    # against the script's own directory so the audit runs from anywhere - a chdir here would not
    # redirect it, and this test would silently assert against whatever the real repo has checked
    # out. Same fix bench-audit_test.py's RED proof uses.
    audit_mod = sys.modules.get("bench_audit") or bc._audit
    old_here = audit_mod.HERE
    audit_mod.HERE = td
    old_cwd = os.getcwd()
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
        audit_mod.HERE = old_here
        os.chdir(old_cwd)

if _fail == 0:
    print("all bench-cost tests passed")
    sys.exit(0)
print("BENCH-COST TESTS FAILED")
sys.exit(1)
