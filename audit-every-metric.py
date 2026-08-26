#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# EVERY METRIC, EVERY CELL, EVERY GATEWAY - enumerated, not sampled.
#
# The other verifiers each prove one metric family deeply (verify-frontier, verify-turnover,
# verify-latency). This one goes WIDE instead: for every published field of every cell it checks
# whether a present value is possible (finite, correctly signed, correctly ordered), whether an
# absent value has a stated reason, and whether fields that must agree with each other do.
#
# The output is deliberately a per-metric INVENTORY (present/absent counts and why), because a
# metric that is absent everywhere is invisible to any check that only looks at values - and past
# engine bugs have hidden in exactly those metrics nobody had a deep verifier for.

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
    # COST fields: derived from two monotonic counters, so none can be negative. `procsample::cost`
    # already refuses the pid-reuse case that would go negative; a negative here means that refusal
    # was bypassed, so this is the second line of defence.
    "cpu_us_per_request", "rps_per_cpu_second", "cost_window_conc", "cost_threads",
    "cost_core_utilisation", "cost_window_ok", "cost_window_rps",
    "cost_nonvol_ctxt_per_request", "cost_majflt",
}
# THERE IS NO p50<=p99 CHECK HERE, AND THE ONE THAT USED TO BE WAS UNSOUND.
#
# Each `added_*` figure is a DIFFERENCE of two distributions' percentiles
# (e.g. added_gap_p50 = gateway_gap_p50 - direct_gap_p50), and a difference does not inherit
# monotonicity from its operands - (X50-Y50) <= (X99-Y99) is not guaranteed, so p50 can legitimately
# exceed p99 for an added_* figure with no engine defect involved. A prior version of this check
# flagged exactly that as a violation.
#
# A sound version would need the RAW legs, which the snapshot doesn't carry (stream publishes only
# added_*; perf publishes direct/gateway at p99 only). The mechanism is DELETED rather than left as
# an empty list, since a gate that can't fire reads like coverage and isn't. verify-latency.py checks
# the statement that's actually true of added_*: that it equals the difference of its own legs.


# NOT TUNING KNOBS. `run-on-ec2.sh` pins the gateway to CORES=0-3, so 4 is the utilisation
# denominator - must track that pin or this compares against the wrong core count. Tolerance is
# loose (3x) because disagreement is expected to be one-sided (utilisation window > load window, so
# implied-from-CPU runs below measured); only the impossible direction (more CPU than cores have) is
# flagged.
COST_PINNED_CORES = 4
COST_UTIL_TOLERANCE = 3.0


def num(v):
    if isinstance(v, dict):
        v = v.get("value")
    return v if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def scalar_fields(block):
    """Every leaf NUMBER in a metric block, skipping evidence arrays, prose, and non-numeric fields.

    The non-numeric exclusion is load-bearing: treating `num() is None` as "absent" makes every
    string/boolean field (recipes, verdict flags, reasons) look like an unexplained null, when it was
    never a measurement to begin with. Decide what a field IS before asking whether it has a value.
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
                # A WARNING, NOT A VIOLATION. `check-consistency.mjs` already owns this as an explained
                # artefact: VmHWM can't be below an observed RSS for a fixed tree, so an overshoot means
                # a child counted in the sampled peak had exited before the VmHWM sum was taken. Treating
                # it as a hard failure here would put two oracles in disagreement over the same reading.
                # A percentage threshold is the wrong instrument too - the overshoot is a roughly
                # constant absolute quantity (one transient worker), so it reads as a smaller percentage
                # on a bigger tree; use MiB, not %, to judge it.
                if hwm is not None and peak is not None and peak > hwm:
                    warnings.append(
                        f"{at}: sampled peak ({peak:.1f} MiB) exceeds kernel HWM ({hwm:.1f} MiB) by "
                        f"{(peak - hwm):.1f} MiB ({(peak / hwm - 1) * 100:.2f}%) - transient-worker artefact"
                    )

                # NO CPU-vs-CORES CROSS-CHECK HERE ANY MORE. A prior version compared gateway CPU
                # against /proc/stat's per-core accumulation, but those counters are tick-sampled and
                # false-fired on a gateway serving sub-tick (~380us) requests. `cost_core_utilisation`
                # is now derived from the gateway's own precisely-accounted CPU, so re-deriving it here
                # would just compare a number with itself - a check that can't fail is worse than none.

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
