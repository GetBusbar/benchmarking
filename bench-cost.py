#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# WHERE A RUN'S TIME WENT, from the committed artifacts. No box, no stopwatch, no re-run.
#
# A run that is slower than the last one is a question a wall-clock total cannot answer: "thirteen
# minutes a cell" might be the TTFT sample set, a stream ladder reaching a higher rung, or a gateway
# that simply got slower, and those have nothing in common as responses. The engine records seconds
# per metric group per cell (`cell.timings_s`); this reads them back and says what to cut.
#
#   ./bench-cost.py                 every gateway on the newest engine
#   ./bench-cost.py agentgateway    one gateway
#   ./bench-cost.py --engine 8f2af5d   a specific engine, to compare two runs
import collections
import glob
import json
import sys


def newest_snapshots(gw_filter=None, engine=None):
    """The newest snapshot per gateway, optionally pinned to an engine prefix."""
    by_gw = {}
    for f in sorted(glob.glob("results/snapshots/result_*.json")):
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
        engine = args[i + 1]
        del args[i : i + 2]
    gw_filter = args[0] if args else None

    snaps = newest_snapshots(gw_filter, engine)
    if not snaps:
        print("no snapshots matched", file=sys.stderr)
        return 1

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

    # The actionable part: what a change to the most expensive group would actually buy, stated as
    # wall clock rather than as a percentage nobody can plan against.
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
