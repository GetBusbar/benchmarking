#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Re-derives the frontier from raw rungs, independently of the engine AND of bench-audit.py, using
# the PUBLISHED DEFINITION (`ResultSnapshot.definitions["perf.frontier"]`) rather than `frontier.rs` -
# so a divergence between the published prose and the implementation gets caught, not just a
# divergence between the engine and a sibling audit written by the same hand.
#
# Also usable live: point it at a snapshot mid-run to print the curve and anything that fails to
# re-derive.
#
# THE RULE, quoted from the published definition: for each declared bound, the most requests/sec the
# cell carried while 99% of requests finished under that bound AND it failed none it accepted, plus
# one unbounded reading. Monotone non-decreasing across bounds (relaxing a bound only adds rungs).
#
# DELIBERATELY WIDER THAN THE ENGINE: a published `SweepPoint` has no `ok` count, so "served cleanly"
# is approximated as `rps > 0 and fail == 0` rather than the engine's `ok > 0 and fail == 0`. This can
# only accept more than the engine would, so any disagreement is real, never hidden by the approximation.

import glob
import os
import json
import sys

# Mirrors frontier::P99_BOUNDS_US. Parsed from the engine rather than hardcoded, so this can't
# quietly police a different axis than the one that ran; going blind counts as a failure, not a pass.
ENGINE_FRONTIER = "engine/src/frontier.rs"


def declared_bounds_us():
    import re

    try:
        src = open(ENGINE_FRONTIER).read()
    except OSError as e:
        raise SystemExit(f"cannot read {ENGINE_FRONTIER} to learn the declared bounds: {e}")
    m = re.search(r"pub const P99_BOUNDS_US:\s*\[u64;\s*\d+\]\s*=\s*\[([^\]]+)\]", src)
    if not m:
        raise SystemExit(
            f"could not find P99_BOUNDS_US in {ENGINE_FRONTIER}; refusing to guess the axis"
        )
    return [int(x.strip().replace("_", "")) for x in m.group(1).split(",") if x.strip()]


def _rate(v):
    """Rate as text for a PROBLEM message. `:.0f` would print a genuine 0.25 as "0", making a caught
    defect read as noise, so sub-1 rates get two decimals."""
    if v is None:
        return "none"
    return f"{v:.2f}" if v < 1 else f"{v:,.0f}"


def num(v):
    """A published Measurement is either a bare number or null. Absence is None, never 0."""
    if isinstance(v, dict):
        v = v.get("value")
    return v if isinstance(v, (int, float)) else None


def rungs_of(perf):
    out = []
    for p in perf.get("sweep_max_proxy") or []:
        out.append(
            {
                "conc": num(p.get("conc")),
                "rps": num(p.get("rps")),
                "p99_us": num(p.get("p99_us")),
                "fail": num(p.get("fail")),
            }
        )
    return [r for r in out if r["conc"] is not None]


def clean(r):
    """Did this rung serve everything it accepted? Approximates the engine's `ok > 0 and fail == 0`.

    A p99 counts as proof of completion even when `rps` rounds down to 0 (e.g. one request over four
    seconds truncates through the engine's `as i64` to 0 rps but still has a p99) - `rps > 0` alone
    would wrongly call such a rung dirty. `ok` isn't published, so this stays an approximation, but a
    wider one than the engine's rule, so any residual error is a missed catch, not a false alarm.
    """
    if r["fail"] != 0:
        return False
    return (r["rps"] is not None and r["rps"] > 0) or r["p99_us"] is not None


def qualifies(r, bound_us):
    if not clean(r):
        return False
    if bound_us is None:
        return True
    return r["p99_us"] is not None and r["p99_us"] < bound_us


def derive(rungs, bound_us):
    ok = [r for r in rungs if qualifies(r, bound_us)]
    if not ok:
        return None
    # THE TIE-BREAK IS PART OF THE RULE: highest rate wins; among rungs tied on rate, the lowest
    # concurrency wins. Spelled out explicitly rather than left to `max`, because Python's `max` and
    # Rust's `max_by` break ties at opposite ends - relying on either language's default would agree
    # only by coincidence of input order.
    #
    # A rung can qualify with no rate (see `clean()`: a null rate there means the rate rounded away,
    # not that nothing was served), so the key treats an absent rate as the lowest candidate rather
    # than crashing on `-None`.
    best = min(ok, key=lambda r: (-(r["rps"] if r["rps"] is not None else 0.0), r["conc"]))
    # A CONCURRENCY is disqualified, not a rung: each concurrency is sampled across WINDOWS_PER_RUNG=3
    # windows, so one unlucky window shouldn't condemn the level. Disqualify only if NO window at that
    # concurrency qualified.
    above = [c for c in sorted({r["conc"] for r in rungs if r["conc"] > best["conc"]})
             if not any(r["conc"] == c and qualifies(r, bound_us) for r in rungs)]
    top = max((r["conc"] for r in rungs), default=0)
    return {
        "rps": best["rps"],
        "conc": best["conc"],
        "p99_us": best["p99_us"],
        "disq": min(above) if above else None,
        "lower_bound": best["conc"] >= top,
    }


def check(path, bounds_us, verbose):
    d = json.load(open(path))
    gw = d.get("gateway", "?")
    eng = ((d.get("rig") or {}).get("engine") or {}).get("commit", "?")[:7]
    problems = []
    cells = 0
    curves = []
    for eg, up in ((d.get("matrix") or {}).get("upstreams") or {}).items():
        for ing, cell in (up.get("cells") or {}).items():
            if cell.get("served") is not True:
                continue
            perf = cell.get("perf") or {}
            fr = perf.get("frontier")
            at = f"{gw} {ing}>{eg}"
            if not fr:
                # A cell with no frontier is only honest if NO cell in this snapshot has one (the
                # artifact predates the metric). Mixed is a dropped reading.
                continue
            cells += 1
            rungs = rungs_of(perf)
            if not rungs:
                problems.append(f"{at}: publishes a frontier but no sweep - nothing can be re-derived")
                continue
            want_bounds = list(bounds_us) + [None]
            got_bounds = [r.get("p99_bound_us") for r in fr]
            if got_bounds != want_bounds:
                problems.append(f"{at}: bounds {got_bounds} != declared {want_bounds}")
            rates = []
            row = []
            for r in fr:
                b = r.get("p99_bound_us")
                label = "unbounded" if b is None else f"{b // 1000}ms"
                pub_rps = num(r.get("rps"))
                mine = derive(rungs, b)
                rates.append(pub_rps)
                row.append((label, pub_rps, r.get("concurrency"), num(r.get("p99_us")), r.get("lower_bound")))
                if mine is None and pub_rps is not None:
                    problems.append(f"{at} {label}: published {pub_rps} but no rung qualifies")
                    continue
                if mine is not None and pub_rps is None:
                    problems.append(
                        f"{at} {label}: published nothing but rungs qualify (best {_rate(mine['rps'])} at c={mine['conc']})"
                    )
                    continue
                if mine is None:
                    continue
                # Compared EXACTLY, not within tolerance: `derive` returns a published rung's rate
                # verbatim, so a mismatch means the engine disagrees with its own rungs. A prior 1.0 rps
                # tolerance made this unable to fail below 1 req/s - exactly the domain fractional rates
                # matter for.
                if mine["rps"] is None or pub_rps is None or mine["rps"] != pub_rps:
                    problems.append(f"{at} {label}: published {pub_rps} rps, re-derived {_rate(mine['rps'])}")
                if num(r.get("concurrency")) != mine["conc"]:
                    problems.append(
                        f"{at} {label}: published c={num(r.get('concurrency'))}, re-derived c={mine['conc']}"
                    )
                if bool(r.get("lower_bound")) != mine["lower_bound"]:
                    problems.append(
                        f"{at} {label}: lower_bound={r.get('lower_bound')}, re-derived {mine['lower_bound']}"
                    )
                # first_disqualified_conc is the field that proves a reading is a BOUNDARY rather than
                # just a maximum; comparing rate/concurrency/tail/floor alone would miss disagreements
                # here.
                pub_disq = num(r.get("first_disqualified_conc"))
                if pub_disq != mine["disq"]:
                    problems.append(
                        f"{at} {label}: first_disqualified_conc={pub_disq}, re-derived {mine['disq']}"
                        " - this is the half of the reading's proof that says it really is the boundary"
                    )
                if b is not None and mine["p99_us"] is not None and mine["p99_us"] >= b:
                    problems.append(f"{at} {label}: winning rung's tail {mine['p99_us']}us is not under its own bound")
            present = [x for x in rates if x is not None]
            for a, bb in zip(present, present[1:]):
                if bb < a:
                    problems.append(f"{at}: frontier inverts across bounds: {rates}")
                    break
            spread = (max(present) / min(present)) if present and min(present) else None
            curves.append((at, row, spread, max((r["conc"] for r in rungs), default=0), len(rungs)))
    if verbose:
        for at, row, spread, top, nrungs in curves:
            print(f"\n--- {at}   ({nrungs} rungs, top c={top}"
                  + (f", spread {spread:.2f}x)" if spread else ")"))
            for label, rps, conc, p99, lb in row:
                pl = "-" if p99 is None else f"{p99/1000:.2f}ms"
                print(f"    {label:>9} {str(rps):>9} rps @c={str(conc):>6}  p99={pl:>10}"
                      + ("  FLOOR" if lb else ""))
    return gw, eng, cells, problems


def equivalent_instrument(engines):
    """True when every engine seen is attested to the SAME built binary in instrument-equivalence.json."""
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "site", "instrument-equivalence.json")
    try:
        with open(path) as f:
            doc = json.load(f)
    except Exception:
        return False
    groups = doc.get("instruments") or doc.get("equivalent") or []
    for grp in (groups.values() if isinstance(groups, dict) else groups):
        commits = grp.get("commits") if isinstance(grp, dict) else None
        if not commits:
            continue
        short = {c[:7] for c in commits}
        if all(e[:7] in short for e in engines):
            return True
    return False


def published_paths():
    """The snapshots the BOARD publishes, not every artifact that has ever landed on disk. Globbing
    everything would mix in superseded runs measured under rules since corrected (e.g. an old
    per-RUNG vs per-CONCURRENCY `first_disqualified_conc`), which would fail permanently on data no
    reader can see and train the operator to treat FAIL as noise. Superseded files aren't deleted or
    silently skipped: the count is printed, and an explicit path on the command line is still checked.
    """
    board = os.path.join(os.path.dirname(os.path.abspath(__file__)), "site", "data.json")
    every = sorted(glob.glob("results/snapshots/*.json"))
    try:
        with open(board) as f:
            want = {os.path.basename(g["snapshot_file"])
                    for g in json.load(f).get("gateways", []) if g.get("snapshot_file")}
    except Exception:
        return every
    if not want:
        return every
    keep = [p for p in every if os.path.basename(p) in want]
    skipped = len(every) - len(keep)
    if skipped:
        print(f"checking the {len(keep)} snapshot(s) the board publishes "
              f"({skipped} superseded file(s) on disk not checked - pass a path to check one)")
    return keep


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("-")]
    verbose = "-v" in sys.argv or "--verbose" in sys.argv
    paths = args or published_paths()
    bounds = declared_bounds_us()
    print(f"declared bounds (from {ENGINE_FRONTIER}): {[b//1000 for b in bounds]} ms + unbounded")
    total_cells = 0
    all_problems = []
    engines = {}
    for p in paths:
        try:
            gw, eng, cells, problems = check(p, bounds, verbose)
        except Exception as e:  # a snapshot mid-write during a live run is not a defect
            print(f"  SKIP {p}: {e}")
            continue
        if cells == 0:
            continue
        engines.setdefault(eng, []).append(gw)
        total_cells += cells
        all_problems += problems
        print(f"\n{gw:16s} engine {eng}  {cells} cell(s) with a frontier"
              + ("" if not problems else f"  <-- {len(problems)} PROBLEM(S)"))
    print(f"\n{'=' * 78}")
    print(f"re-derived {total_cells} cell(s) carrying a frontier")
    if len(engines) > 1:
        # Different commits aren't automatically different instruments: instrument-equivalence.json
        # admits several commits as one instrument only on identical `otb --release` sha256 (a built
        # binary), per C8.
        if equivalent_instrument(engines):
            print(f"engines {sorted(engines)} - one instrument by attested identical binary (C8)")
        else:
            print(f"MIXED ENGINES: {dict((k, len(v)) for k, v in engines.items())} - not comparable")
    for x in all_problems:
        print(f"  PROBLEM: {x}")
    if all_problems:
        print(f"\nFAIL: {len(all_problems)} problem(s)")
        return 1
    print("\nPASS: every published reading re-derives from its own rungs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
