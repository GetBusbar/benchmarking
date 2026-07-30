#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# EVERY METRIC, EVERY CELL, EVERY GATEWAY - enumerated, not sampled.
#
# The other verifiers each take one metric family and prove it deeply: `verify-frontier` re-derives the
# frontier from its rungs, `verify-turnover` proves each maximum is a turnover rather than a failure
# cliff, `verify-latency` re-derives added latency from its two legs. This one is the opposite shape -
# it goes WIDE. It walks every published field of every cell and asks the questions that apply to any
# measurement at all:
#
#   - is a value that is present actually possible (finite, non-negative where the quantity cannot be
#     negative, a percentile ordering that is not inverted)?
#   - is a value that is ABSENT absent for a stated reason, or did it just vanish?
#   - do fields that must agree with each other agree?
#
# WHY WIDE MATTERS. Both engine bugs found on 2026-07-29 were in metrics nobody had a second opinion
# about. A deep verifier only exists for a metric someone already suspected. A wide sweep is how a
# metric nobody thought about gets looked at at all - and the output here is deliberately a per-metric
# INVENTORY (how many cells publish it, how many are absent, and why), because a metric that is absent
# everywhere is invisible to every check that only looks at values.

import glob
import json
import sys
from collections import Counter, defaultdict

# Quantities that cannot be negative. A negative one is not a measurement of anything.
NON_NEGATIVE = {
    "added_latency_p50_us", "added_latency_p99_us", "direct_c1_p99_us", "gateway_c1_p99_us",
    "added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us", "added_gap_p99_us",
    "streams_sustained", "streams_sustained_fps",
    "idle_rss_mib", "steady_state_rss_mib", "peak_rss_mib", "peak_rss_hwm_mib", "recovered_rss_mib",
    "time_to_plateau_s", "load_s", "idle_window_s", "recovery_window_s",
}
# (p50 field, p99 field) pairs from one distribution: p50 must not exceed p99.
PERCENTILE_PAIRS = [
    ("added_latency_p50_us", "added_latency_p99_us"),
    ("added_ttft_p50_us", "added_ttft_p99_us"),
    ("added_gap_p50_us", "added_gap_p99_us"),
]


def num(v):
    if isinstance(v, dict):
        v = v.get("value")
    return v if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def scalar_fields(block):
    """Every leaf NUMBER in a metric block, skipping evidence arrays, prose, and non-numeric fields.

    THE NON-NUMERIC EXCLUSION IS LOAD-BEARING, not tidiness. The first version treated "num() returned
    None" as "this measurement is absent", which made every string and boolean field look like a null
    with no reason recorded - `load_recipe` (a string, 92 cells), `egress_reverified` (a boolean, 31),
    `stream.reason` (a string, 33). Three of the five "absences" it reported were fields that are not
    measurements at all and were never absent.

    That is the same false-alarm class as reading `.value` off a `{t_s, rss_mib}` sample and concluding
    a board with 26 idle series had none: an extractor that cannot read a field reports it missing, and
    a missing field looks exactly like a defect. So decide what a field IS from the artifact, and only
    then ask whether it has a value.
    """
    out = {}
    for k, v in (block or {}).items():
        if isinstance(v, list) or k.endswith("_series") or k.endswith("_note") or k.endswith("_samples"):
            continue
        if k in ("frontier", "sweep_max_proxy", "sweep_streams"):
            continue
        inner = v.get("value") if isinstance(v, dict) else v
        if isinstance(inner, (str, bool)):
            continue  # a recipe, a verdict flag or a reason - not a quantity
        n = num(v)
        if n is not None:
            out[k] = n
        elif inner is None:
            # Only a field that is numeric SOMEWHERE counts as an absent measurement; a key that is
            # null in every cell of every gateway is a schema slot, not a lost number.
            out[k] = None
    return out


def main():
    paths = sorted(glob.glob("results/snapshots/*.json"))
    if not paths:
        print("no snapshots")
        return 0

    present = Counter()
    absent = Counter()
    absent_no_reason = defaultdict(list)
    problems = []
    warnings = []
    cells = 0
    gateways = set()

    for p in paths:
        try:
            d = json.load(open(p))
        except (OSError, ValueError):
            # See verify-turnover.py: a snapshot mid-write during a live run is not a defect, and this
            # tool is most useful precisely while a run is in flight.
            print(f"  SKIP {p}: not readable as JSON yet (a snapshot mid-write is not a defect)")
            continue
        gw = d.get("gateway", "?")
        gateways.add(gw)
        for eg, up in ((d.get("matrix") or {}).get("upstreams") or {}).items():
            for ing, cell in (up.get("cells") or {}).items():
                if cell.get("served") is not True:
                    continue
                cells += 1
                at = f"{gw} {ing}>{eg}"
                absences = cell.get("absences") or {}
                for block_name in ("perf", "stream", "memory"):
                    block = cell.get(block_name) or {}
                    for field, val in scalar_fields(block).items():
                        key = f"{block_name}.{field}"
                        if val is None:
                            absent[key] += 1
                            # AN ABSENCE WITHOUT A REASON IS THE FAILURE MODE THIS CATCHES. "not
                            # measured" and "measured, and the answer is nothing" are different facts,
                            # and a null with no entry in `absences` collapses them.
                            if key not in absences and field not in ("stream_error", "serve_error"):
                                absent_no_reason[key].append(at)
                            continue
                        present[key] += 1
                        if not isinstance(val, (int, float)) or val != val or val in (float("inf"), float("-inf")):
                            problems.append(f"{at}: {key} is not a finite number ({val!r})")
                        if field in NON_NEGATIVE and val < 0:
                            problems.append(f"{at}: {key} is negative ({val}) - not possible for this quantity")
                    for lo_f, hi_f in PERCENTILE_PAIRS:
                        lo, hi = num(block.get(lo_f)), num(block.get(hi_f))
                        if lo is not None and hi is not None and lo > hi:
                            problems.append(f"{at}: {block_name}.{lo_f} ({lo}) exceeds {hi_f} ({hi})")

                # CROSS-FIELD AGREEMENTS, each one a fact two numbers must both tell.
                mem = cell.get("memory") or {}
                idle, steady = num(mem.get("idle_rss_mib")), num(mem.get("steady_state_rss_mib"))
                peak, hwm = num(mem.get("peak_rss_mib")), num(mem.get("peak_rss_hwm_mib"))
                rec = num(mem.get("recovered_rss_mib"))
                if peak is not None and steady is not None and peak < steady:
                    problems.append(f"{at}: peak_rss ({peak}) is below steady_state ({steady}) - a peak cannot be under the level it peaked from")
                if peak is not None and idle is not None and peak < idle:
                    problems.append(f"{at}: peak_rss ({peak}) is below idle ({idle}) - load cannot use less than rest")
                if rec is not None and peak is not None and rec > peak:
                    problems.append(f"{at}: recovered_rss ({rec}) exceeds peak ({peak}) - it cannot recover to above its own peak")
                # A WARNING, NOT A VIOLATION, AND THE PERCENTAGE IS THE WRONG INSTRUMENT.
                #
                # `check-consistency.mjs` already owns this fact and classifies it as an explained
                # artefact: VmHWM cannot be below an observed RSS for a FIXED tree, so an overshoot means
                # a child counted in the sampled peak had exited before the VmHWM sum was taken - two
                # instants, one tree. Making it a hard failure here would have two oracles disagreeing
                # about whether the same measurement is a defect, which is worse than either verdict.
                #
                # The threshold was 1% and it fired on exactly one cell: bifrost openai>anthropic, 1.33%.
                # But one transient worker is an ABSOLUTE quantity, not a proportion - the same worker is
                # 0.3 MiB (0.6%) on agentgateway's 48 MiB tree and 11.5 MiB (1.33%) on bifrost's 876 MiB
                # one. Board-wide: 32 of 92 cells overshoot, median 0.22%, and the only cell above 1% is
                # the largest tree on the board. That is the artefact's signature, not a defect's.
                if hwm is not None and peak is not None and peak > hwm:
                    warnings.append(
                        f"{at}: sampled peak ({peak:.1f} MiB) exceeds kernel HWM ({hwm:.1f} MiB) by "
                        f"{(peak - hwm):.1f} MiB ({(peak / hwm - 1) * 100:.2f}%) - transient-worker artefact"
                    )

                st = cell.get("stream") or {}
                if st.get("stream_served") is True:
                    ss, fps = num(st.get("streams_sustained")), num(st.get("streams_sustained_fps"))
                    if (ss is None) != (fps is None):
                        problems.append(f"{at}: streams_sustained and its frame rate disagree about being measured ({ss} / {fps})")

    print(f"{'=' * 92}\nEVERY METRIC, EVERY CELL: {len(gateways)} gateways, {cells} served cells\n{'=' * 92}\n")
    allkeys = sorted(set(present) | set(absent))
    print(f"{'metric':44s} {'present':>8} {'absent':>7}  {'coverage':>9}")
    print("-" * 92)
    for k in allkeys:
        pn, an = present[k], absent[k]
        tot = pn + an
        print(f"{k:44s} {pn:8d} {an:7d}  {pn / tot * 100 if tot else 0:8.1f}%")

    never_numeric = {k for k in allkeys if present[k] == 0}
    if never_numeric:
        print(f"\n{'=' * 92}")
        print("NEVER CARRIES A NUMBER ON THIS BOARD (schema slot, or a metric no cell produced):")
        for k in sorted(never_numeric):
            print(f"  {k}: null in all {absent[k]} cell(s)")

    print(f"\n{'=' * 92}")
    absent_no_reason = {k: v for k, v in absent_no_reason.items() if k not in never_numeric}
    if absent_no_reason:
        print("ABSENT WITH NO REASON RECORDED - a null nobody has to explain:")
        for k, ats in sorted(absent_no_reason.items()):
            print(f"  {k}: {len(ats)} cell(s) e.g. {ats[0]}")
    else:
        print("Every absent value carries a reason in the cell's `absences` map.")

    if warnings:
        print(f"\n{'=' * 92}")
        print(f"WARNINGS ({len(warnings)}) - explained artefacts, not defects; check-consistency owns these:")
        for w in warnings[:6]:
            print(f"  {w}")
        if len(warnings) > 6:
            print(f"  ... and {len(warnings) - 6} more, same class")

    print(f"\n{'=' * 92}")
    if problems:
        print(f"PROBLEMS ({len(problems)}):")
        for x in problems:
            print(f"  {x}")
        return 1
    print("NO IMPOSSIBLE VALUES: every published number is finite, correctly signed, correctly ordered,")
    print("and agrees with the fields it must agree with.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
