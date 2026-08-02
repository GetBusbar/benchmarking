#!/usr/bin/env python3
"""Side-by-side: busbar 1.4.1 (published board) vs busbar 1.5.0 (private evaluation).

Reads the 1.4.1 numbers from its committed snapshot and the 1.5.0 numbers from whatever the
evaluation box has written so far - the live `busbar-150.json`, pulled partial, or final snapshot -
so a cell can be compared the moment it lands rather than at the end of an 11-hour grid.

Every figure is printed with its own absence reason when it has one. A blank cell here means the
engine published a reason, not that the comparison failed to find a number: the two are different
findings and the whole board exists to keep them apart.

  python3 compare-busbar-versions.py [ingress>egress ...]      default: openai>openai
"""
import glob
import json
import os
import sys


def newest(pattern):
    fs = sorted(glob.glob(pattern))
    return fs[-1] if fs else None


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return None


def cell_of(doc, ingress, egress):
    if not doc:
        return None
    up = (doc.get("matrix", {}).get("upstreams") or {}).get(egress) or {}
    return (up.get("cells") or {}).get(ingress)


def num(cell, block, field):
    """(value, reason) for a metric, so an absence prints its own explanation."""
    if not cell:
        return None, "cell not measured yet"
    b = cell.get(block) or {}
    v = b.get(field)
    if v is not None:
        return v, None
    entry = (cell.get("absences") or {}).get(f"{block}.{field}") or {}
    return None, entry.get("reason") or "absent"


def frontier_rps(cell, bound_ms=10):
    if not cell:
        return None, "cell not measured yet"
    for r in ((cell.get("perf") or {}).get("frontier") or []):
        b = r.get("p99_bound_us")
        if (b is None and bound_ms is None) or (b is not None and b // 1000 == bound_ms):
            v = r.get("rps")
            if v is not None:
                return v, None
            return None, "no rung held this bound"
    return None, "no reading at this bound"


def fmt(v, reason, unit="", pct=None):
    if v is None:
        return f"— ({reason})"
    s = f"{v:,.0f}{unit}" if isinstance(v, (int, float)) and abs(v) >= 100 else f"{v:,.2f}{unit}"
    return s if pct is None else f"{s}  ({pct:+.1f}%)"


def delta(a, b):
    if a is None or b is None or a == 0:
        return None
    return (b - a) / a * 100.0


ROWS = [
    ("added latency p50", "perf", "added_latency_p50_us", " us", False),
    ("added latency p99", "perf", "added_latency_p99_us", " us", False),
    ("TTFT p50",          "stream", "added_ttft_p50_us",  " us", False),
    ("TTFT p99",          "stream", "added_ttft_p99_us",  " us", False),
    ("streams sustained", "stream", "streams_sustained",  "",    True),
    ("peak RSS",          "memory", "peak_rss_mib",       " MiB", False),
    ("steady RSS",        "memory", "steady_state_rss_mib", " MiB", False),
    ("idle RSS",          "memory", "idle_rss_mib",       " MiB", False),
    ("growth rate",       "memory", "growth_rate_mib_per_min", " MiB/min", False),
]

HERE = os.path.dirname(os.path.abspath(__file__))
pairs = sys.argv[1:] or ["openai>openai"]

old_doc = load(newest(os.path.join(HERE, "results/snapshots/result_busbar_2026-08-02*.json"))
               or newest(os.path.join(HERE, "results/snapshots/result_busbar_*.json")))
new_path = (newest(os.path.join(HERE, "results/snapshots/result_busbar-150_*.json"))
            or (os.path.join(HERE, "results/partial/busbar-150.json")
                if os.path.exists(os.path.join(HERE, "results/partial/busbar-150.json")) else None))
new_doc = load(new_path)

print(f"busbar 1.4.1 (published board)  <-  {os.path.basename(newest(os.path.join(HERE,'results/snapshots/result_busbar_2026-08-02*.json')) or '?')}")
print(f"busbar 1.5.0 (private eval)     <-  {os.path.basename(new_path) if new_path else 'not written yet'}")
print()

for pair in pairs:
    ing, _, eg = pair.partition(">")
    a, b = cell_of(old_doc, ing, eg), cell_of(new_doc, ing, eg)
    print(f"=== {ing} > {eg} ===")
    if not b:
        print("  1.5.0 has not written this cell yet\n")
        continue
    print(f"  {'metric':<20} {'1.4.1':<28} {'1.5.0':<28}")
    for label, block, field, unit, higher_better in ROWS:
        av, ar = num(a, block, field)
        bv, br = num(b, block, field)
        d = delta(av, bv)
        # Sign the delta by whether it is an IMPROVEMENT, not by its arithmetic direction: for
        # latency and memory lower is better, for sustained streams higher is.
        note = ""
        if d is not None:
            good = (d > 0) if higher_better else (d < 0)
            note = "  better" if good and abs(d) >= 1 else ("  worse" if not good and abs(d) >= 1 else "  ~same")
        print(f"  {label:<20} {fmt(av, ar, unit):<28} {fmt(bv, br, unit, d):<28}{note}")
    arps, arr = frontier_rps(a)
    brps, brr = frontier_rps(b)
    d = delta(arps, brps)
    note = ""
    if d is not None:
        note = "  better" if d >= 1 else ("  worse" if d <= -1 else "  ~same")
    print(f"  {'RPS @10ms p99':<20} {fmt(arps, arr):<28} {fmt(brps, brr, '', d):<28}{note}")
    print()
