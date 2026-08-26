#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Where a run's time went, from the committed artifacts - no box, no stopwatch, no re-run.
#
# Reads seconds per metric group per cell (`cell.timings_s`) and says what to cut, since a
# wall-clock total can't tell a bigger TTFT sample set from a gateway that just got slower.
#
#   ./bench-cost.py                 every gateway on the newest engine
#   ./bench-cost.py agentgateway    one gateway
#   ./bench-cost.py --engine 8f2af5d   a specific engine, to compare two runs
#
# Runnable from any directory, and pinned to ONE engine, for the same reasons as bench-audit: see
# the notes on `_audit` and on the default `engine` in main().
import collections
import importlib.util
import json
import os
import sys

# Snapshot location and "which engine is current" are borrowed from bench-audit.py, the tool
# responsible for being right about the board, rather than retyped (see TOOL-02 on rules that
# exist twice). Imported by path since the filename is hyphenated; side-effect free because
# bench-audit's argparse lives under `if __name__ == "__main__"`.
_HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location("bench_audit", os.path.join(_HERE, "bench-audit.py"))
_audit = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(_audit)


def newest_snapshots(gw_filter=None, engine=None):
    """The newest snapshot per gateway, optionally pinned to an engine prefix."""
    by_gw = {}
    for f in _audit.snapshot_paths():
        try:
            d = json.load(open(f))
        except Exception:
            continue
        gw = d.get("gateway")
        if not gw or (gw_filter and gw != gw_filter):
            continue
        sha = ((d.get("rig") or {}).get("engine") or {}).get("commit") or ""
        if engine and not sha.startswith(engine):
            continue
        by_gw[gw] = (f, d, sha)
    return by_gw


def main():
    args = [a for a in sys.argv[1:]]
    engine = None
    if "--engine" in args:
        i = args.index("--engine")
        if i + 1 >= len(args):
            print("--engine needs a commit prefix", file=sys.stderr)
            return 1
        engine = args[i + 1]
        del args[i : i + 2]
    gw_filter = args[0] if args else None

    # Default to one engine: without pinning, a rerun that covers only part of the field would mix
    # two engines' timings into one cost profile and answer "what got slower" confidently wrong.
    # Same recency rule as bench-audit (snapshot's own measured_at, not filename order).
    if engine is None:
        engine = _audit.newest_engine()
        if engine is None:
            print("no snapshot carries an engine commit - nothing to attribute cost to", file=sys.stderr)
            return 1

    snaps = newest_snapshots(gw_filter, engine)
    if not snaps:
        print(f"no snapshots on engine {engine}", file=sys.stderr)
        return 1
    print(f"engine {engine[:7]}  {len(snaps)} gateways\n")

    grand = collections.Counter()
    cells_seen = 0
    per_gw = {}

    for gw, (path, d, sha) in sorted(snaps.items()):
        totals = collections.Counter()
        n = 0
        for _eg, blk in ((d.get("matrix") or {}).get("upstreams") or {}).items():
            for _ing, c in (blk.get("cells") or {}).items():
                t = c.get("timings_s")
                if not t:
                    continue
                n += 1
                for group, secs in t.items():
                    totals[group] += secs
        if n:
            per_gw[gw] = (totals, n, sha)
            grand.update(totals)
            cells_seen += n

    if not cells_seen:
        print("no cell timings in these snapshots - they predate `timings_s`.")
        print("Re-run on the current engine; every measured cell records its own cost from then on.")
        return 1

    print(f"{'gateway':16s} {'cells':>5s} {'total':>9s}  slowest groups")
    for gw, (totals, n, sha) in sorted(per_gw.items(), key=lambda kv: -sum(kv[1][0].values())):
        tot = sum(totals.values())
        top = "  ".join(f"{g}={s:.0f}s" for g, s in totals.most_common(3))
        print(f"{gw:16s} {n:5d} {tot/60:8.1f}m  {top}")

    print()
    print(f"ACROSS {cells_seen} MEASURED CELLS")
    total = sum(grand.values())
    print(f"{'group':22s} {'total':>10s} {'share':>7s} {'per cell':>10s}")
    for group, secs in grand.most_common():
        print(f"{group:22s} {secs/60:9.1f}m {secs/total*100:6.1f}% {secs/cells_seen:9.1f}s")
    print(f"{'TOTAL':22s} {total/60:9.1f}m {100.0:6.1f}% {total/cells_seen:9.1f}s")

    # What a change to the most expensive group would buy, in wall clock rather than percentage.
    if grand:
        worst, secs = grand.most_common(1)[0]
        print()
        print(f"The most expensive group is {worst}, at {secs/cells_seen:.1f}s per cell.")
        print(f"  halving it saves {secs/2/60:.1f}m over these {cells_seen} cells "
              f"({secs/2/cells_seen:.1f}s per cell)")
        print(f"  removing it saves {secs/60:.1f}m ({secs/cells_seen:.1f}s per cell)")
        print(f"  a 36-cell gateway would save {secs/cells_seen*36/60:.1f}m by halving it")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
