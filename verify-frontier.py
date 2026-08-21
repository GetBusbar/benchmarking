#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# RE-DERIVE THE FRONTIER FROM THE RAW RUNGS, INDEPENDENTLY OF THE ENGINE AND OF bench-audit.py.
#
# bench-audit.py already re-derives each reading from `sweep_max_proxy`. This exists anyway, and the
# reason is not redundancy: that audit and the engine were written by the same hand in the same
# sitting, so an agreement between them is one mind checking its own arithmetic. This file is written
# from the PUBLISHED DEFINITION - `ResultSnapshot.definitions["perf.frontier"]`, the prose the board
# shows a reader - rather than from `frontier.rs`. If the definition and the implementation ever say
# different things, this is what notices.
#
# It is also the operator's tool during a run: point it at a snapshot the moment it harvests and it
# prints the curve, the per-bound arithmetic, and anything that does not add up - which is how the
# `lower_bound` semantics bug was caught on the first live run rather than on the published board.
#
# THE RULE, quoted from the published definition: for each declared bound, the most requests/sec the
# cell carried while 99% of requests finished under that bound AND it failed none it accepted. Plus
# one reading with no latency bound at all. Monotone non-decreasing across the bounds, because
# relaxing a bound only ADDS rungs to the set the maximum is taken over.
#
# WHERE THIS IS DELIBERATELY LESS PRECISE THAN THE ENGINE, and why that is safe: a published
# `SweepPoint` carries no `ok` count, so "served cleanly" cannot be spelled `ok > 0 and fail == 0` as
# the engine spells it. It is approximated as `rps > 0 and fail == 0`. The approximation can only ever
# be WIDER than the engine's (a window with ok == 0 also has rps == 0), so a reading this file accepts
# and the engine rejected would be reported here as a disagreement to investigate, never hidden.

import glob
import os
import json
import sys

# Mirrors frontier::P99_BOUNDS_US. Parsed from the engine rather than hardcoded, so this cannot
# quietly police a different axis than the one that ran - the same anti-drift reasoning bench-audit.py
# uses for its own mirrors, and going blind counts as a failure rather than a pass.
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
    """A rate as text for a PROBLEM message. `:.0f` printed a genuine 0.25 as "0", so an operator read
    the tool's own finding as "the re-derived reading is nothing" and dismissed it as noise - the
    message describing a caught defect made the defect invisible."""
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

    A P99 COUNTS AS PROOF OF COMPLETION. `rps > 0` alone was wrong, and it cost a real catch: plano at
    c=256 published `rps: 0, p99_us: 3398432, fail: 0`, which `rps > 0` reads as dirty - but a percentile
    cannot exist without a completed, timed request, so ok >= 1 and the rate merely rounded down through
    the engine's `as i64` (one request over four seconds is 0.25 rps). Treating it as dirty makes this
    tool disagree with a correct engine.

    `ok` is the one input `SweepPoint` does not publish, so this stays an approximation - but a wider one
    than the engine's rule rather than a narrower one, which means its residual error is a missed catch
    rather than a false alarm.
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
    # THE TIE-BREAK IS PART OF THE RULE, so it is spelled out here rather than left to `max`.
    #
    # This read `max(ok, key=lambda r: r["rps"])`, which returns the FIRST maximum, while the engine used
    # `max_by` in Rust, which returns the LAST. On gomodel that disagreement surfaced as "published
    # c=512, re-derived c=256" on a cell where four rungs tied at 109 rps - and it was the ENGINE that
    # was wrong: naming a higher concurrency claims the rate needs more connections than it does.
    #
    # Both sides now state the same rule: highest rate, and among rungs tied on rate, the lowest
    # concurrency. Relying on either language's max-adapter would leave this agreeing by coincidence of
    # input order, which is not agreement at all.
    # A RUNG CAN QUALIFY WITH NO RATE, so the tie-break must not assume one.
    #
    # `clean()` deliberately admits a rung carrying a p99 but no rps - its docstring says a percentile
    # cannot exist without a completed request, so a null rate there means the rate rounded away, not
    # that nothing was served. The key then did `-r["rps"]`, which raises TypeError on None and takes
    # the whole tool down mid-board. Treating an absent rate as the lowest candidate keeps the rung
    # eligible (which is the point of admitting it) without letting it win a maximum it cannot state.
    best = min(ok, key=lambda r: (-(r["rps"] if r["rps"] is not None else 0.0), r["conc"]))
    # A CONCURRENCY IS DISQUALIFIED, NOT A RUNG. Rungs are recorded PER WINDOW (WINDOWS_PER_RUNG=3),
    # so every concurrency appears three times and one unlucky window is not a verdict on the level.
    # Asking "is there any failing rung above best" condemned concurrencies that two of their three
    # windows carried cleanly - the engine and bench-audit.py were both corrected to ask instead
    # "did NO window at this concurrency qualify", and this oracle was left behind on the old rule.
    # It re-derived 2 where the engine published 4 on portkey and called the engine wrong.
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


def check(d, bounds_us, verbose):
    gw = d.get("gateway", "?")
    eng = ((d.get("rig") or {}).get("engine") or {}).get("commit", "?")[:7]
    problems = []
    cells = 0
    curves = []
    frontierless = []  # served cells with no frontier - honest ONLY if the whole snapshot lacks one
    for eg, up in ((d.get("matrix") or {}).get("upstreams") or {}).items():
        for ing, cell in (up.get("cells") or {}).items():
            if cell.get("served") is not True:
                continue
            perf = cell.get("perf") or {}
            fr = perf.get("frontier")
            at = f"{gw} {ing}>{eg}"
            if not fr:
                # A cell with no frontier is only honest if NO cell in this snapshot has one (the
                # artifact predates the metric). Mixed is a dropped reading - defer the verdict until
                # we know whether any sibling cell DID publish a frontier.
                frontierless.append(at)
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
                # EXACT, NOT WITHIN 1.0 req/s. No arithmetic happens on either side - `derive` returns
                # a published rung's rate verbatim and `pub_rps` is the published reading - so these are
                # the same float or the engine disagrees with its own rungs. The old tolerance of 1.0
                # made this oracle UNABLE TO FAIL anywhere below 1 req/s, which is the entire domain the
                # fractional rate was introduced for: a regression republishing 0.25 as 0 scored
                # abs(0.25) < 1.0 and passed. bench-audit.py compares exactly, so the two independent
                # re-derivations disagreed about the rule - the same signature as both engine bugs.
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
                # THE BOUNDARY PROOF, which this tool did not check at all and should have.
                #
                # bench-audit.py caught a first_disqualified_conc disagreement on plano that this file
                # walked straight past, because it compared the rate, the concurrency, the tail and the
                # floor flag - and not the one field that makes a reading a BOUNDARY rather than just a
                # maximum. Two independent checkers are only worth two if they check the same claims.
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
    # Mixed = a dropped reading. If ANY served cell in this snapshot published a frontier, then every
    # served cell that DID NOT is an omission, not an honest metric-predates-the-artifact absence.
    if cells > 0:
        for at in frontierless:
            problems.append(f"{at}: served but publishes no frontier while sibling cells do - mixed is a dropped reading")
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
    """The snapshots the BOARD publishes - not every artifact that has ever landed in the directory.

    This globbed `results/snapshots/*.json`, which mixes the current board in with every superseded
    run still on disk. The 2026-07-31 field run was measured before `first_disqualified_conc` was
    corrected from a per-RUNG to a per-CONCURRENCY rule, so its snapshots genuinely carry the old
    wrong value - 36 of them. Re-deriving those against the corrected rule made this tool fail
    permanently on data no reader can see, which trains the operator to treat FAIL as background
    noise. That is worse than no oracle: the next real problem arrives into a channel already
    dismissed.

    Superseded files are not deleted and not silently skipped - the count is printed, and any path
    passed explicitly on the command line is still checked, so the old run stays auditable on demand.
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
            d = json.load(open(p))
        except Exception as e:  # a snapshot mid-write during a live run is not a defect
            print(f"  SKIP {p}: {e}")
            continue
        # An exception INSIDE the re-derivation is a real failure, not a mid-write race - let it surface.
        gw, eng, cells, problems = check(d, bounds, verbose)
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
        # DIFFERENT COMMITS ARE NOT AUTOMATICALLY DIFFERENT INSTRUMENTS. `instrument-equivalence.json`
        # admits several commits as one instrument only on identical `otb --release` sha256 - a built
        # binary, not a reasoned diff. Calling an attested set "not comparable" would contradict the
        # board's own C8 evidence and read as a defect where there is none.
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
