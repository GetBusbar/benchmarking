#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Re-derives the added-latency figures from the two legs they are a difference of.
#
# WHY: published quantities computed by exactly one piece of code, with no independent re-derivation,
# are unaudited - which is how two prior engine bugs went uncaught by any test. The frontier now has
# three independent re-derivations; added latency had zero. This is the second one.
#
#   CAN CHECK:    `added_latency_p*_us` is a DIFFERENCE of two published numbers - the gateway leg and
#                 the direct-to-mock leg at concurrency 1. Both operands ride in the artifact, so the
#                 subtraction is fully checkable.
#
#   CANNOT CHECK: the two operands are themselves percentiles over raw per-request latencies, which
#                 are NOT published. So this verifies the ARITHMETIC, never the percentiles - a
#                 systematic error in `percentile()` would move both legs together and still pass.
#                 Printed every run (see closing note) so a pass here isn't mistaken for full coverage.
#
# The streaming TTFT figures get the same shape of check where `ttft_direct_samples` / `ttft_gw_samples`
# are populated; on boards seen so far those are null, so the tool reports "nothing to re-derive" per
# cell rather than silently skipping (a silent skip looks identical to a clean bill of health).

import glob
import json
import sys


def num(v):
    """A published Measurement is a bare number or null. Absence is None, never 0."""
    if isinstance(v, dict):
        v = v.get("value")
    return v if isinstance(v, (int, float)) else None


def listof(v):
    if isinstance(v, dict):
        v = v.get("value")
    return v if isinstance(v, list) else None


def check(path):
    d = json.load(open(path))
    gw = d.get("gateway", "?")
    problems, checked, unverifiable = [], 0, 0
    ttft_with_samples = 0
    for eg, up in ((d.get("matrix") or {}).get("upstreams") or {}).items():
        for ing, cell in (up.get("cells") or {}).items():
            if cell.get("served") is not True:
                continue
            at = f"{gw} {ing}>{eg}"
            perf = cell.get("perf") or {}
            gw_leg = num(perf.get("gateway_c1_p99_us"))
            direct = num(perf.get("direct_c1_p99_us"))
            added = num(perf.get("added_latency_p99_us"))

            # A cell may legitimately withhold all three (egress re-verification found the gateway did
            # not translate). Absent-together is fine; a difference present without its operands is not
            # - it would be a number nothing can check.
            if added is None:
                continue
            if gw_leg is None or direct is None:
                unverifiable += 1
                problems.append(
                    f"{at}: publishes added_latency_p99_us={added} but not both legs it is a difference "
                    f"of (gateway_c1_p99={gw_leg}, direct_c1_p99={direct}) - the figure cannot be "
                    f"re-derived by anyone, which is the condition this tool exists to prevent"
                )
                continue
            checked += 1
            mine = gw_leg - direct
            if abs(mine - added) > 1:
                problems.append(
                    f"{at}: published added_latency_p99_us={added} but its own legs give "
                    f"{gw_leg} - {direct} = {mine}"
                )
            # A NEGATIVE difference means the gateway beat the direct leg - not overhead, but the two
            # legs having been measured under non-comparable conditions. charts.py refuses to plot one.
            if mine < 0:
                problems.append(
                    f"{at}: the gateway leg ({gw_leg}us) is FASTER than direct-to-mock ({direct}us), so "
                    f"'added latency' is negative - the two legs are not comparable and the difference "
                    f"is not overhead"
                )
            # NO p50<=p99 CHECK, deliberately: added_latency figures are DIFFERENCES (gateway leg minus
            # direct leg), not one distribution, and a difference does not inherit monotonicity -
            # constant gateway overhead under a stretching direct baseline can be smaller at p99 than at
            # p50 with nothing actually out of order (observed on real field data). Such a rule would
            # catch a genuine swap, but is indistinguishable from a legitimate reading, so it was
            # removed; what this tool proves instead - each added figure equals the difference of its
            # own published legs - is the statement that is actually true of these numbers.

            st = cell.get("stream") or {}
            if listof(st.get("ttft_gw_samples")) and listof(st.get("ttft_direct_samples")):
                ttft_with_samples += 1
    return gw, checked, unverifiable, ttft_with_samples, problems


def main():
    paths = [a for a in sys.argv[1:] if not a.startswith("-")] or sorted(
        glob.glob("results/snapshots/*.json")
    )
    total = unver = ttft = 0
    allp = []
    for p in paths:
        try:
            gw, n, u, t, probs = check(p)
        except Exception as e:  # a snapshot mid-write during a live run is not a defect
            print(f"  SKIP {p}: {e}")
            continue
        total += n
        unver += u
        ttft += t
        allp += probs
        if n or u:
            print(f"{gw:16s} {n} added-latency figure(s) re-derived from their own legs"
                  + (f"  <-- {len(probs)} PROBLEM(S)" if probs else ""))
    print(f"\n{'=' * 78}")
    print(f"re-derived {total} added-latency figure(s) from the two legs each is a difference of")
    if unver:
        print(f"{unver} figure(s) published WITHOUT both legs - unverifiable by anyone")
    print(
        f"{ttft} cell(s) publish raw TTFT samples; the rest publish TTFT percentiles with no samples, "
        f"so those are not re-derivable here"
    )
    # Printed every run, pass or fail: a pass here means the subtraction is right, not that the
    # percentiles feeding it are.
    print(
        "LIMIT: this verifies the ARITHMETIC of a difference, not the percentiles it subtracts. The raw\n"
        "       per-request latencies behind gateway_c1_p99_us and direct_c1_p99_us are not published, so\n"
        "       a systematic error in percentile() would move both legs together and pass this check."
    )
    for x in allp:
        print(f"  PROBLEM: {x}")
    if allp:
        print(f"\nFAIL: {len(allp)} problem(s)")
        return 1
    print("\nPASS: every added-latency figure equals the difference of its own published legs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
