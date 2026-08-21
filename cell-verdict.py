#!/usr/bin/env python3
"""Per-cell live verdict: compare a running gateway's partial snapshot to a baseline, cell by cell.

Emits one verdict line per NEWLY-measured cell (throughput headline = frontier RPS at the 10ms p99
bound, higher-is-better), so an operator watching a 36-cell grid gets "cell N: same/better/worse
(was X now Y) - proceeding" the moment each cell lands, instead of waiting out the whole grid.

Numbering is append-only and persisted in --state (a JSON list of reported "ingress>egress" keys):
a cell is numbered the first time it is seen measured, and never renumbered, so cell 1/2/3 are stable
even as later cells complete out of matrix order. Re-run against the same --state to get only what is
new since last call.

  cell-verdict.py --baseline <snap.json> --new <partial.json> --state <state.json> [--count-only]

A cell counts as "measured" once it has a frontier RPS at the 10ms p99 bound; latency (added p50/p99,
c=1 p99, cpu us/req) rides along as corroboration. An absent corroboration figure prints an em-dash
(—) rather than a zero, so a missing measurement is never shown as a real value.
"""
import argparse
import json
import os
import sys

BOUND_US = 10_000          # headline: frontier rung whose p99 bound is 10ms
SAME_BAND_PCT = 3.0        # |delta| < this -> "same"; else better/worse (throughput, higher better)


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return None


def cells_in_order(doc):
    """Yield (ingress, egress, cell) in the snapshot's own upstream/ingress insertion order."""
    if not doc:
        return
    ups = (doc.get("matrix") or {}).get("upstreams") or {}
    for egress, u in ups.items():
        for ingress, cell in ((u or {}).get("cells") or {}).items():
            yield ingress, egress, cell


def frontier_rps(cell, bound_us=BOUND_US):
    if not cell:
        return None
    for r in ((cell.get("perf") or {}).get("frontier") or []):
        if r.get("p99_bound_us") == bound_us:
            return r.get("rps")
    return None


def perf_num(cell, field):
    if not cell:
        return None
    return ((cell.get("perf") or {}) or {}).get(field)


def cell_map(doc):
    return {f"{ing}>{eg}": c for ing, eg, c in cells_in_order(doc)}


def measured_keys_in_order(doc):
    """Keys of cells that have a headline frontier RPS, in snapshot order."""
    out = []
    for ing, eg, c in cells_in_order(doc):
        if frontier_rps(c) is not None:
            out.append(f"{ing}>{eg}")
    return out


def verdict_of(old_rps, new_rps):
    if new_rps is None:
        return "unmeasured", None
    if old_rps is None or old_rps == 0:
        return "new", None
    pct = (new_rps - old_rps) / old_rps * 100.0
    if abs(pct) < SAME_BAND_PCT:
        return "same", pct
    return ("better" if pct > 0 else "worse"), pct


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--baseline", required=True)
    ap.add_argument("--new", required=True)
    ap.add_argument("--state", required=True)
    ap.add_argument("--count-only", action="store_true")
    a = ap.parse_args()

    new_doc = load(a.new)
    measured = measured_keys_in_order(new_doc) if new_doc else []
    if a.count_only:
        print(len(measured))
        return

    base = cell_map(load(a.baseline))
    newm = cell_map(new_doc)

    state = load(a.state) or []
    reported = list(state)
    reported_set = set(reported)

    fresh = [k for k in measured if k not in reported_set]
    for k in fresh:
        idx = len(reported) + 1
        oc, nc = base.get(k), newm.get(k)
        o_rps, n_rps = frontier_rps(oc), frontier_rps(nc)
        v, pct = verdict_of(o_rps, n_rps)
        o_lat, n_lat = perf_num(oc, "added_latency_p50_us"), perf_num(nc, "added_latency_p50_us")
        o_cpu, n_cpu = perf_num(oc, "cpu_us_per_request"), perf_num(nc, "cpu_us_per_request")

        def r(x):
            return f"{x:,.0f}" if isinstance(x, (int, float)) else "—"

        pcts = f" ({pct:+.1f}%)" if pct is not None else ""
        lat = ""
        if o_lat is not None or n_lat is not None:
            lat = f"; added-lat p50 {r(o_lat)}->{r(n_lat)}us"
        cpu = ""
        if o_cpu is not None or n_cpu is not None:
            cpu = f"; cpu {r(o_cpu)}->{r(n_cpu)}us/req"
        # Human relay line (the operator copies this to the user verbatim).
        print(f"RELAY: cell {idx} ({k}): {v} — RPS@10ms was {r(o_rps)} now {r(n_rps)}{pcts}{lat}{cpu}")
        reported.append(k)

    with open(a.state, "w") as f:
        json.dump(reported, f)

    # Machine tail so the caller knows counts without reparsing.
    print(f"COUNT: measured={len(measured)} reported={len(reported)} new={len(fresh)}")


if __name__ == "__main__":
    main()
