#!/usr/bin/env python3
"""Side-by-side: busbar on the published board vs the busbar-151 evaluation entrant.

Reads the published numbers from busbar's committed snapshot and the evaluation numbers from whatever
the evaluation box has written so far - the final snapshot or the pulled partial `busbar-151.json` -
so a cell can be compared the moment it lands rather than at the end of an 11-hour grid. EACH COLUMN IS
LABELLED WITH THE BUILD ITS FILE SAYS IT MEASURED (`build`, e.g. getbusbar/busbar:1.5.1), never with a
version typed into this script: busbar-151 is 1.5.1, and the column used to say 1.5.0.

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
    """The parsed file, or None when there is no file. A file that EXISTS and cannot be read is not
    "not measured yet" - printing it as that would describe a corrupt artifact as a pending one - so it
    stops the comparison and says which file and why."""
    if not path:
        return None
    try:
        with open(path) as f:
            doc = json.load(f)
    except Exception as e:
        raise SystemExit(f"cannot read {path}: {e} - not comparing against an unreadable file")
    if not isinstance(doc, dict):
        raise SystemExit(f"{path} is not a JSON object - not comparing against it")
    return doc


def cell_of(doc, ingress, egress):
    if not doc:
        return None
    up = ((doc.get("matrix") or {}).get("upstreams") or {}).get(egress) or {}
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
    """Percent change from a to b, signed by the direction of the change. Divided by |a|: a signed
    baseline (growth_rate_mib_per_min can be negative) flipped the sign and inverted better/worse."""
    if a is None or b is None or a == 0:
        return None
    return (b - a) / abs(a) * 100.0


def label_of(doc, path):
    """What a column measured, from its own file: the tag of its `build`."""
    if not doc:
        return "not written yet"
    build = str(doc.get("build") or "")
    return build.rsplit(":", 1)[-1] if ":" in build else (build or os.path.basename(path or "?"))


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

# The file actually LOADED is the one named in the header - the fallback used to load the newest busbar
# snapshot while the header re-ran only the 2026-08-02 glob and printed '?' beside a hardcoded label.
old_path = (newest(os.path.join(HERE, "results/snapshots/result_busbar_2026-08-02*.json"))
            or newest(os.path.join(HERE, "results/snapshots/result_busbar_*.json")))
old_doc = load(old_path)
new_path = (newest(os.path.join(HERE, "results/snapshots/result_busbar-151_*.json"))
            or (os.path.join(HERE, "results/partial/busbar-151.json")
                if os.path.exists(os.path.join(HERE, "results/partial/busbar-151.json")) else None))
new_doc = load(new_path)
old_label, new_label = label_of(old_doc, old_path), label_of(new_doc, new_path)

print(f"busbar {old_label} (published board)  <-  {os.path.basename(old_path) if old_path else 'none found'}")
print(f"busbar {new_label} (busbar-151 eval)  <-  {os.path.basename(new_path) if new_path else 'not written yet'}")
print()

for pair in pairs:
    ing, _, eg = pair.partition(">")
    a, b = cell_of(old_doc, ing, eg), cell_of(new_doc, ing, eg)
    print(f"=== {ing} > {eg} ===")
    if not b:
        print(f"  {new_label} has not written this cell yet\n")
        continue
    print(f"  {'metric':<20} {old_label:<28} {new_label:<28}")
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
