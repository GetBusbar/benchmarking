#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# RE-DERIVE THE ADDED-LATENCY FIGURES FROM THE TWO LEGS THEY ARE A DIFFERENCE OF.
#
# WHY THIS EXISTS. Two engine bugs were found in one night, and BOTH were found the same way: a second
# implementation of one rule disagreeing with the first. The frontier's tie-break surfaced because
# Python's `max` and Rust's `max_by` break ties at opposite ends. The NaN-sort corruption surfaced
# because I went looking for the same class afterwards. Neither was caught by a test.
#
# That makes the discovery mechanism luck plus a habit, not coverage - and it means ANY published
# quantity computed by exactly one piece of code, with no independent re-derivation, is unaudited in
# precisely the way those two were. The frontier is now re-derived three independent ways. Added
# latency was re-derived zero ways. This is the second way.
#
# WHAT CAN AND CANNOT BE CHECKED, stated plainly, because the limit is the point:
#
#   CAN:    `added_latency_p*_us` is a DIFFERENCE of two published numbers - the gateway leg and the
#           direct-to-mock leg at concurrency 1. Both operands ride in the artifact, so the subtraction
#           is fully checkable, and a published difference that disagrees with its own operands is a
#           defect no other tool here would notice.
#
#   CANNOT: the two operands are themselves percentiles over raw per-request latencies, and those
#           samples are NOT published. So this verifies the ARITHMETIC, never the percentiles. If
#           `percentile()` were wrong, both legs would be wrong consistently and this would pass. That
#           is a real limit and it is reported rather than papered over - see the closing note, which
#           prints it every run so nobody mistakes a pass here for a fully audited figure.
#
# The same shape of check applies to the streaming TTFT figures, which DO declare
# `ttft_direct_samples` / `ttft_gw_samples`. On the boards seen so far those fields are null, so there
# is nothing to re-derive from; the tool says so per cell rather than silently skipping, because a
# checker that quietly finds nothing looks exactly like a clean bill of health.

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


def check(d):
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

            # A cell may legitimately withhold all three (egress re-verification proved the gateway did
            # not translate, so every number there describes a different wire). Absent-together is fine;
            # a difference present without its operands is not - it would be a number nothing can check.
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
            # A NEGATIVE difference would mean the gateway beat the direct leg, which is not a
            # measurement of overhead - it is the two legs having been taken under conditions that are
            # not comparable. charts.py refuses to plot one; nothing checked the artifact for it.
            if mine < 0:
                problems.append(
                    f"{at}: the gateway leg ({gw_leg}us) is FASTER than direct-to-mock ({direct}us), so "
                    f"'added latency' is negative - the two legs are not comparable and the difference "
                    f"is not overhead"
                )
            # NO p50<=p99 CHECK, AND THE ONE REMOVED FROM HERE WAS UNSOUND FOR THE SAME REASON AS ITS
            # TWIN IN audit-every-metric.py - which is the point worth recording, because the two were
            # written in one sitting and the second was missed when the first was found.
            #
            # It read "p50 must not exceed p99 from the same distribution. Cheap, and it catches a
            # swap." Both added_latency figures are DIFFERENCES (gateway leg minus direct leg), not one
            # distribution, and a difference does not inherit monotonicity: constant gateway overhead
            # under a stretching direct baseline is smaller at p99 than at p50 with nothing out of
            # order. The sibling rule fired on agentgateway in the 2026-07-30 field run (added_gap
            # p50=4us vs p99=3us); this one had simply not been unlucky yet.
            #
            # Catching a SWAP was a fair motive, but it cannot be done from the differences alone, and
            # the swap it would catch is indistinguishable from a legitimate reading. What this tool
            # does prove - that each added figure equals the difference of its own published legs - is
            # the statement that is actually true of these numbers, and it stays.

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
            d = json.load(open(p))
        except Exception as e:  # a snapshot mid-write during a live run is not a defect
            print(f"  SKIP {p}: {e}")
            continue
        # An exception INSIDE the re-derivation is a real failure, not a mid-write race - let it surface.
        gw, n, u, t, probs = check(d)
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
    # PRINTED EVERY RUN, PASS OR FAIL. A pass here means the subtraction is right - not that the
    # percentiles feeding it are. Saying so on every run is the difference between a known limit and a
    # false sense of coverage.
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
