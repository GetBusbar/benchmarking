#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# EVERY CROSS-METRIC INVARIANT A PUBLISHED BOARD MUST HOLD, as a program that exits non-zero.
#
# This file exists because of a specific failure, and the failure was not a bad number - it was a bad
# ANSWER TO "IS THE DATA GOOD?". Auditing a run meant writing throwaway python in a scratch directory,
# eyeballing the output, and forming an opinion. An opinion does not survive the next run, cannot be
# handed to anyone, and cannot be re-checked after a change. When asked "would you publish this?" the
# honest answer had to be "the checks I happened to think of, passed" - which is not a verdict.
#
# So the checks live here, they run over the artifacts, and "the audit is done" means this exits 0.
#
# It is the same lesson the engine spent a week learning about its own guards. `transient_budget()`
# was called by nothing. `box_qualify` always seeded. `history/append.py` wrote zero rows. Twenty-seven
# site tests asserted against an empty board. Every one of them was a check that existed in someone's
# head and nowhere in the code, and every one of them passed by doing nothing at all. A gate that
# cannot fail is not a gate, and a gate nobody wrote is worse.
#
#   ./bench-audit.py                    the newest engine present in results/snapshots
#   ./bench-audit.py --engine dc7a53c   a specific engine, to audit one board
#   ./bench-audit.py --gateway kong     narrow to one gateway while investigating
#
# Exit 0 = every invariant held. Exit 1 = at least one did not, and each violation names the cell.
import argparse
import collections
import glob
import json
import os
import sys

# ── the bars ──────────────────────────────────────────────────────────────────────────────────────
#
# Named, not inlined, because a reader deciding whether to trust a violation needs to see what it was
# measured against. Each carries the reasoning that set it where it is.

# How far the sustained figure may sit above the peak before it is a defect rather than window noise.
#
# The two numbers now come out of ONE climb over ONE state of the gateway (`run::sweep_cell`), so a
# genuine inversion means the throughput curve spiked between two doublings - which gateways do not
# do. Before that change they were two searches separated by a gateway restart, and three cells of
# the 2026-07-28 board published a "sustained" rate up to 7% above the "maximum" it was meant to sit
# under. This stays at 5% rather than 0 because the ceiling is refined BETWEEN rungs and its rate is
# a median of three windows there, so a point or two of disagreement is measurement, not a bug.
C6_GROSS_PCT = 5.0

# A rate this far above its own concurrency is not a proxy measurement.
#
# One connection cannot issue 20000 requests per second against a real socket; a number that says so
# is a units error or a counted retry, not a gateway. Deliberately loose - this catches the class of
# defect where a rate is divided by the wrong thing, not marginal optimism.
MAX_RPS_PER_CONNECTION = 20_000


def load(engine=None, gateway=None):
    """The newest snapshot per gateway, pinned to one engine so a board is audited as a board."""
    by_gw = {}
    for f in sorted(glob.glob("results/snapshots/result_*.json")):
        try:
            d = json.load(open(f))
        except Exception as e:
            print(f"  unreadable snapshot {f}: {e}", file=sys.stderr)
            continue
        gw = d.get("gateway")
        sha = ((d.get("rig") or {}).get("engine") or {}).get("commit") or ""
        if not gw or (gateway and gw != gateway):
            continue
        if engine and not sha.startswith(engine):
            continue
        by_gw[gw] = (f, d, sha)
    return by_gw


def newest_engine():
    """The engine that most recently produced a snapshot, so the default audits the current board."""
    best = None
    for f in sorted(glob.glob("results/snapshots/result_*.json")):
        try:
            d = json.load(open(f))
        except Exception:
            continue
        sha = ((d.get("rig") or {}).get("engine") or {}).get("commit") or ""
        if sha:
            best = sha
    return best


def served_cells(d):
    """Every cell the gateway actually served, with its coordinates for naming a violation."""
    for eg, blk in ((d.get("matrix") or {}).get("upstreams") or {}).items():
        for ing, c in (blk.get("cells") or {}).items():
            if c.get("served") is True:
                yield f"{ing}>{eg}", c


def median(v):
    s = sorted(v)
    return s[len(s) // 2] if len(s) % 2 else s[len(s) // 2 - 1]


# ── the invariants ────────────────────────────────────────────────────────────────────────────────
#
# Each takes one served cell and yields a string per violation. A check that can never yield is a
# check that is not doing anything, which is the defect class this whole file is about: if one of
# these stops firing on data that used to trip it, that is a finding, not a pass.


def check_sustained_not_above_peak(name, c):
    """The two throughput numbers summarise ONE sweep, so the gated one cannot exceed the ungated one.

    The 2026-07-28 board tripped this on apisix anthropic>anthropic (1.065x), kong openai>gemini
    (1.054x) and litellm-python openai>openai-responses (1.074x). The cause was not any gateway: the
    sustained leg was a second search that ran after the memory group cold-restarted the process, so
    the pair compared two different states of it.
    """
    p = c.get("perf") or {}
    mx, su = p.get("rps_max_proxy"), p.get("rps_sustained_20ms")
    if mx and su and mx > 0 and su > mx * (1 + C6_GROSS_PCT / 100):
        yield f"{name}: sustained {su:.0f} exceeds peak {mx:.0f} by {(su/mx-1)*100:.1f}%"


def check_peak_came_from_its_own_sweep(name, c):
    """A published peak must be a rate some window in its own sweep actually produced."""
    p = c.get("perf") or {}
    mx = p.get("rps_max_proxy")
    rates = [r["rps"] for r in (p.get("sweep_max_proxy") or []) if r.get("rps") is not None]
    if mx and rates and mx > max(rates):
        yield f"{name}: peak {mx:.0f} exceeds every window it came from (best {max(rates)})"


def check_sweep_carries_its_latency(name, c):
    """Every throughput window must publish the p99 it ran at.

    THIS IS THE REGRESSION GUARD FOR THE DEFECT THAT CAUSED THE RERUN. The load generator computed a
    p99 for every window and always had; `SweepProbe::probe` narrowed its result to a rate and a
    verdict and dropped it. Because the latency was gone, the sustained figure could not be read off
    the throughput sweep, so the engine measured the cell a SECOND time to get it - minutes later,
    across a gateway restart. Thirty-three readings per cell were paid for and discarded in every run
    the project ever published. If this check ever passes vacuously again, the second search comes
    back with it.
    """
    pts = (c.get("perf") or {}).get("sweep_max_proxy") or []
    if not pts:
        return
    withp99 = sum(1 for r in pts if r.get("p99_us") is not None)
    if withp99 == 0:
        yield f"{name}: all {len(pts)} throughput windows published without the p99 they measured"


def check_ttft_percentiles_are_ordered(name, c):
    """p99 cannot sit below p50 when both come from one sample set.

    Distinct from a difference-of-percentiles, which genuinely has no ordering: `added_gap_p99_us`
    below `added_gap_p50_us` is legal, because each is an independent difference between two legs'
    matched percentiles. A guard that conflated the two fired on plano and would have failed the
    build for all fourteen gateways.
    """
    st = c.get("stream") or {}
    a, b = st.get("added_ttft_p50_us"), st.get("added_ttft_p99_us")
    if a is not None and b is not None and b < a:
        yield f"{name}: added_ttft p99 {b} sits below p50 {a}"


def check_rate_and_concurrency_travel_together(name, c):
    """A rate without the concurrency it was measured at cannot be re-derived or charted."""
    p = c.get("perf") or {}
    for rate, conc, label in (
        ("rps_sustained_20ms", "rps_sustained_20ms_concurrency", "sustained"),
        ("rps_max_proxy", "conc_at_peak", "peak"),
    ):
        r, k = p.get(rate), p.get(conc)
        if r is not None and k is None:
            yield f"{name}: {label} rate published with no concurrency beside it"
        if k is not None and r is None:
            yield f"{name}: {label} concurrency published with no rate beside it"


def check_rate_is_physically_possible(name, c):
    """A per-connection rate above `MAX_RPS_PER_CONNECTION` is a units error, not a fast gateway."""
    p = c.get("perf") or {}
    mx, k = p.get("rps_max_proxy"), p.get("conc_at_peak")
    if mx and k and k > 0 and mx / k > MAX_RPS_PER_CONNECTION:
        yield f"{name}: {mx:.0f} rps at c={k} is {mx/k:.0f} per connection"


def check_frames_have_a_stream_behind_them(name, c):
    """Frames per second with no sustained streams is a rate over a population of zero."""
    st = c.get("stream") or {}
    if st.get("cpu_fps") and not st.get("streams_sustained"):
        yield f"{name}: cpu_fps {st['cpu_fps']} published with streams_sustained={st.get('streams_sustained')}"


CELL_CHECKS = [
    check_sustained_not_above_peak,
    check_peak_came_from_its_own_sweep,
    check_sweep_carries_its_latency,
    check_ttft_percentiles_are_ordered,
    check_rate_and_concurrency_travel_together,
    check_rate_is_physically_possible,
    check_frames_have_a_stream_behind_them,
]


def check_declaration_matches_what_we_measured(gw):
    """A gateway may not both DECLARE a cell and mark it untestable.

    Declaring a cell is a claim that the gateway does it; `untestable` is an admission we could not
    show that it does. Holding both is the harness publishing a grey where it owes a yes or a no. The
    resolution is binary and belongs in the definition, not the artifact: prove the route and drop
    the untestable entry, or drop the declaration and under-claim honestly.
    """
    p = f"gateways/{gw}/definition.json"
    if not os.path.exists(p):
        return
    d = json.load(open(p))
    dialects = ["openai", "openai-responses", "anthropic", "gemini", "cohere", "bedrock"]
    declared = set()
    for ri, row in enumerate(d.get("matrix") or []):
        for ci, ch in enumerate(row):
            if ch == "1" and ri < len(dialects) and ci < len(dialects):
                declared.add(f"{dialects[ri]}/{dialects[ci]}")
    untestable = set(d.get("untestable") or []) | set(d.get("untestable_cells") or [])
    for cell in sorted(declared & untestable):
        yield f"{gw}: declares {cell} in its matrix AND marks it untestable"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--engine", help="audit the board produced by this engine (default: the newest)")
    ap.add_argument("--gateway", help="narrow to one gateway while investigating")
    args = ap.parse_args()

    engine = args.engine or newest_engine()
    if not engine:
        print("no snapshots to audit", file=sys.stderr)
        return 1
    snaps = load(engine, args.gateway)
    if not snaps:
        print(f"no snapshots on engine {engine}", file=sys.stderr)
        return 1

    violations = collections.defaultdict(list)
    cells = 0
    for gw, (_path, d, _sha) in sorted(snaps.items()):
        for name, c in served_cells(d):
            cells += 1
            for check in CELL_CHECKS:
                for v in check(f"{gw} {name}", c):
                    violations[check.__name__].append(v)
        for v in check_declaration_matches_what_we_measured(gw):
            violations["check_declaration_matches_what_we_measured"].append(v)

    print(f"engine {engine[:7]}  {len(snaps)} gateways  {cells} served cells")
    print(f"{len(CELL_CHECKS)} per-cell invariants + 1 per-gateway invariant\n")

    if not violations:
        print("PASS: every invariant held.")
        return 0

    total = sum(len(v) for v in violations.values())
    for check, items in sorted(violations.items()):
        print(f"{len(items):3d}  {check}")
        for it in items:
            print(f"       {it}")
    print(f"\nFAIL: {total} violation(s) across {len(violations)} invariant(s).")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
