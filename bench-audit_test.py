#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# EVERY AUDIT CHECK MUST BE ABLE TO FAIL.
#
# This file exists because the first draft of `bench-audit.py` shipped a check that could not. Its
# regression guard for the exact defect that forced a full 14-gateway rerun - throughput windows
# published without the p99 they measured - read `cell["sweep_max_proxy"]` when the sweep lives at
# `cell["perf"]["sweep_max_proxy"]`. It found nothing, returned early, and reported PASS on data that
# violates it on all 64 cells. It was written, run against a real board, and it agreed with the board.
#
# That is the same species as `transient_budget()` called by nothing, `box_qualify` always seeding,
# and 27 site tests asserting against an empty board. An audit made of checks like that is worse than
# no audit, because it converts "nobody looked" into "it passed".
#
# So each check gets a cell it MUST reject and a cell it MUST accept. A check that cannot be made to
# fire is not protecting anything, and this file is what makes that a red test rather than a quiet
# green board.
#
#   python3 bench-audit_test.py
import importlib.util
import os
import sys

# Same loader the dashboard's own test uses: the module's filename is hyphenated, so it cannot be
# imported by name.
HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location("bench_audit", os.path.join(HERE, "bench-audit.py"))
audit = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(audit)


def cell(**over):
    """A cell that violates nothing, as the baseline every check is proven against."""
    c = {
        "served": True,
        "perf": {
            "rps_max_proxy": 10_000,
            "conc_at_peak": 64,
            "rps_sustained_20ms": 9_000,
            "rps_sustained_20ms_concurrency": 40,
            "sweep_max_proxy": [
                {"conc": 64, "rps": 10_000, "p99_us": 5_000, "fail": 0},
                {"conc": 128, "rps": 9_500, "p99_us": 30_000, "fail": 0},
            ],
        },
        "stream": {
            "added_ttft_p50_us": 100,
            "added_ttft_p99_us": 400,
            "streams_sustained": 128,
            "cpu_fps": 6_400,
        },
    }
    for k, v in over.items():
        section, _, field = k.partition("__")
        if field:
            c[section][field] = v
        else:
            c[section] = v
    return c


# Each entry: the check, a cell it must REJECT, and what the violation is about.
REJECTS = [
    (audit.check_sustained_not_above_peak, cell(perf__rps_sustained_20ms=11_000),
     "a sustained figure above the peak it shares a sweep with"),
    (audit.check_peak_came_from_its_own_sweep, cell(perf__rps_max_proxy=99_000),
     "a peak no window in its own sweep produced"),
    (audit.check_sweep_carries_its_latency,
     cell(perf__sweep_max_proxy=[{"conc": 64, "rps": 10_000, "p99_us": None, "fail": None}]),
     "throughput windows published without the p99 they measured"),
    (audit.check_ttft_percentiles_are_ordered, cell(stream__added_ttft_p99_us=50),
     "a p99 below the p50 from the same sample set"),
    (audit.check_rate_and_concurrency_travel_together, cell(perf__rps_sustained_20ms_concurrency=None),
     "a rate with no concurrency beside it"),
    # Past MAX_RPS_PER_CONNECTION, deliberately: the bar is loose on purpose (it catches a rate
    # divided by the wrong thing, not marginal optimism), so the fixture has to clear it rather than
    # the bar being lowered to meet the fixture.
    (audit.check_rate_is_physically_possible, cell(perf__conc_at_peak=1, perf__rps_max_proxy=50_000),
     "50000 rps on a single connection"),
    (audit.check_frames_have_a_stream_behind_them, cell(stream__streams_sustained=0),
     "frames per second over a population of zero"),
]


def main():
    failures = []

    for check, bad, what in REJECTS:
        got = list(check("t", bad))
        if not got:
            failures.append(f"{check.__name__} accepted {what} - it cannot fail, so it guards nothing")

    # And the other half: a check that rejects everything is equally useless, because a board that
    # can never pass gets the gate switched off.
    clean = cell()
    for check, _bad, _what in REJECTS:
        got = list(check("t", clean))
        if got:
            failures.append(f"{check.__name__} rejected a clean cell: {got}")

    # The per-gateway invariant is driven off real definitions rather than a fixture, because its
    # whole subject is what the repo actually declares.
    declared_and_untestable = list(audit.check_declaration_matches_what_we_measured("one-api"))
    bifrost_clean = list(audit.check_declaration_matches_what_we_measured("bifrost"))
    if bifrost_clean:
        failures.append(f"bifrost declares nothing it marks untestable, but the check fired: {bifrost_clean}")

    for f in failures:
        print(f"FAIL: {f}")
    if failures:
        return 1
    print(f"PASS: {len(REJECTS)} checks each reject their own violation and accept a clean cell.")
    if declared_and_untestable:
        print(f"note: one-api still declares {len(declared_and_untestable)} cell(s) it marks untestable")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
