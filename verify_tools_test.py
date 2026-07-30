#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# RED FIXTURES FOR THE TWO AUDITORS THAT HAD NEITHER A TEST NOR A PLACE IN THE CI GATE.
#
# `verify-latency.py` and `audit-every-metric.py` were written in one sitting during a live run and
# shipped with no test file and no CI invocation. That is the same shape as the defects they exist to
# catch: an oracle nobody has shown can fail is an oracle nobody should believe. A typo'd field name
# that always resolves to `None` would leave `checked` at 0 and print PASS on every board forever, and
# nothing would notice.
#
# So each check gets a fixture that violates exactly it, and the test asserts the tool FAILS. The
# accept side is covered too, because a checker that rejects everything is equally useless.
#
# Run: python3 verify_tools_test.py

import copy
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))

# A minimal cell that violates nothing: the baseline every red fixture is a single mutation away from.
CLEAN_CELL = {
    "served": True,
    "perf": {
        "added_latency_p50_us": 95,
        "added_latency_p99_us": 105,
        "gateway_c1_p99_us": 136,
        "direct_c1_p99_us": 31,
        "frontier": [
            {"p99_bound_us": 1000, "rps": 100.0, "concurrency": 8, "p99_us": 900,
             "first_disqualified_conc": 16, "lower_bound": False},
            {"p99_bound_us": None, "rps": 120.0, "concurrency": 16, "p99_us": 1500,
             "first_disqualified_conc": None, "lower_bound": False},
        ],
        "sweep_max_proxy": [
            {"conc": 8, "ok": 100, "rps": 100.0, "p99_us": 900, "fail": 0},
            {"conc": 16, "ok": 120, "rps": 120.0, "p99_us": 1500, "fail": 0},
            {"conc": 32, "ok": 110, "rps": 110.0, "p99_us": 2500, "fail": 0},
        ],
    },
    "memory": {
        "idle_rss_mib": 100.0, "steady_state_rss_mib": 200.0, "peak_rss_mib": 210.0,
        "peak_rss_hwm_mib": 210.0, "recovered_rss_mib": 205.0,
        "growth_rate_mib_per_min": 0.1, "load_s": 360, "idle_window_s": 59,
        "recovery_window_s": 30,
    },
    "stream": {"stream_served": False},
    "absences": {},
}


def snapshot(cell):
    return {
        "schema_version": 2, "gateway": "fixture", "measured_at": "2026-07-30T00:00:00Z",
        "rig": {"engine": {"commit": "0" * 12, "dirty": False}},
        "matrix": {"upstreams": {"openai": {"cells": {"openai": cell}}}},
    }


def run(tool, cell):
    """Run one auditor over a one-cell board and return (exit_code, output)."""
    d = tempfile.mkdtemp()
    try:
        os.makedirs(os.path.join(d, "results", "snapshots"))
        with open(os.path.join(d, "results", "snapshots", "result_fixture_2026-07-30T00-00-00Z.json"), "w") as f:
            json.dump(snapshot(cell), f)
        shutil.copy(os.path.join(HERE, tool), d)
        # verify-frontier.py parses the engine for its declared bounds; not needed by these two.
        p = subprocess.run([sys.executable, tool], cwd=d, capture_output=True, text=True)
        return p.returncode, p.stdout + p.stderr
    finally:
        shutil.rmtree(d, ignore_errors=True)


def mutate(**changes):
    c = copy.deepcopy(CLEAN_CELL)
    for path, val in changes.items():
        block, _, field = path.partition("__")
        c[block][field] = val
    return c


FAILURES = []


def expect(name, code, out, want_fail, needle=""):
    ok = (code != 0) if want_fail else (code == 0)
    if ok and needle and needle not in out:
        ok = False
        why = f"expected the message to mention {needle!r}"
    else:
        why = f"exit={code}, wanted {'non-zero' if want_fail else 'zero'}"
    print(f"  {'ok  ' if ok else 'FAIL'} - {name}")
    if not ok:
        FAILURES.append(f"{name}: {why}\n{out[:400]}")


def main():
    print("verify-latency.py:")
    # ACCEPT: a clean cell whose added latency really is the difference of its two legs.
    c, o = run("verify-latency.py", CLEAN_CELL)
    expect("a cell whose legs subtract correctly passes", c, o, False)

    # RED: the published difference disagrees with its own operands. This is the check's whole point,
    # and a typo'd field name would silently skip it while still printing PASS.
    c, o = run("verify-latency.py", mutate(perf__added_latency_p99_us=999))
    expect("a difference that disagrees with its legs FAILS", c, o, True, "re-derived")

    # RED: a difference published with no legs to check it against is unverifiable by anyone.
    bad = copy.deepcopy(CLEAN_CELL); del bad["perf"]["gateway_c1_p99_us"]
    c, o = run("verify-latency.py", bad)
    expect("a difference published without its legs FAILS", c, o, True)

    # RED: p50 above p99 from one distribution is impossible.
    c, o = run("verify-latency.py", mutate(perf__added_latency_p50_us=500))
    expect("a p50 above its own p99 FAILS", c, o, True)

    print("audit-every-metric.py:")
    c, o = run("audit-every-metric.py", CLEAN_CELL)
    expect("a clean cell passes", c, o, False)

    # RED: a quantity that cannot be negative, published negative.
    c, o = run("audit-every-metric.py", mutate(memory__idle_rss_mib=-5.0))
    expect("a negative RSS FAILS", c, o, True, "negative")

    # RED: a peak below the steady state it peaked from.
    c, o = run("audit-every-metric.py", mutate(memory__peak_rss_mib=50.0))
    expect("a peak below its own steady state FAILS", c, o, True)

    # RED: recovered above peak - it cannot recover to above its own maximum.
    c, o = run("audit-every-metric.py", mutate(memory__recovered_rss_mib=9999.0))
    expect("a recovered value above the peak FAILS", c, o, True)

    # THE SILENT-PASS GUARD: the tool must actually iterate cells, not print PASS over an empty walk.
    # This is the failure mode that would make every other assertion here vacuous.
    c, o = run("audit-every-metric.py", CLEAN_CELL)
    expect("it reports the cell count it actually examined", c, o, False, "1 served cells")

    print()
    if FAILURES:
        for f in FAILURES:
            print("FAILURE: " + f)
        print(f"\n{len(FAILURES)} failure(s)")
        return 1
    print("PASS: both auditors reject each violation they exist to catch, and accept a clean cell.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
