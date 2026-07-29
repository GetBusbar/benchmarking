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
#
# Runnable from any directory, and pinned to ONE engine, for the same two reasons bench-audit is: see
# the notes on `_audit` and on the default `engine` in main().
import collections
import importlib.util
import json
import os
import sys

# ONE DECLARATION OF "WHERE THE SNAPSHOTS ARE" AND "WHICH ENGINE IS CURRENT", BORROWED RATHER THAN
# RETYPED. Both facts already live in bench-audit.py, which is the tool whose whole job is to be right
# about the board; a second copy here would be a second thing to keep in step, and the ledger already
# has an entry (TOOL-02) about what happens to a rule that exists twice. The import is by path because
# the filename is hyphenated, and it is side-effect free: bench-audit's argparse lives under
# `if __name__ == "__main__"`.
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

    # DEFAULT TO ONE ENGINE, because the header of this file promises "every gateway on the newest
    # engine" and the code used to leave `engine` as None, which means "every gateway's newest
    # snapshot, whatever engine produced it". Those differ the moment a rerun covers part of the
    # field: the per-group shares, the "slowest groups" ranking and every "halving it saves N minutes"
    # line would then be arithmetic over two different engines' timings, presented as one run's cost
    # profile. The whole point of this tool is to answer "what got slower", and mixing engines is the
    # one way to get a confident wrong answer to it. Same recency rule bench-audit uses (the
    # snapshot's own measured_at, not filename order), from the same function.
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
