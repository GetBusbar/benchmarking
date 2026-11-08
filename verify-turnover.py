#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
"""Is each published peak a real TURNOVER, or could it be a failure CLIFF the rig caused?

The rig-ceiling audit showed no reading won at the top of its ladder. That rules out "we ran out of
rungs" but NOT the subtler case: a peak whose next rung up FAILED. If the ladder reads

    c=256  24,097 rps  fail=0     <- published as the maximum
    c=512       -      fail=900   <- the climb stops here

then 24,097 is only the gateway's maximum if those failures are the GATEWAY's. If they are the box's
(ephemeral ports, file descriptors, the mock), the published number is where the RIG stopped and we
have attributed the rig to the gateway - the one thing the whole board must never do.

The case is UNAMBIGUOUS when a clean rung above the winner served MORE SLOWLY:

    c=256  24,097 rps  fail=0     <- published
    c=512  23,456 rps  fail=0     <- clean, and slower: throughput genuinely turned over
    c=1024      -      fail=294   <- irrelevant, the decision was already made below it

Here the peak is proved by two clean rungs and any rig limit further up cannot have influenced it.

So classify every unbounded reading:
  PROVED    - a clean rung above the winner served strictly slower. Turnover is measured.
  PLATEAU   - no clean rung above was slower, but the next one up was clean at the SAME rate: the peak
              does not rest on a failure, though no turnover was measured either.
  CLIFF     - the winner's next probed rung up was not clean, with no clean slower rung above it.
              NOT necessarily wrong, but the number rests on those failures being the gateway's.
              Split into MOOT (the failing rung was slower anyway) and THAT-MATTERS (it was faster).
  DISCLOSED FLOOR - nothing above was probed, and `lower_bound` says so.
  TOP       - nothing above was probed and the artifact does NOT disclose it: the one failing exit.

A rung is CLEAN only if every window at its concurrency served cleanly by the engine's own rule
(`frontier::Rung::served_cleanly`: `ok > 0 and fail == 0`, mirrored by bench-audit.py's
`rung_served_cleanly`). An absent `fail` is not zero failures - "measured nothing" and "measured no
failures" are different facts - and a window that completed nothing (`ok == 0`, rate 0) is not a clean
rung that "served more slowly", which would otherwise prove a turnover out of an empty window.
"""
import glob
import json
import os
import sys

# Anchored to this file, not the caller's cwd (bench-audit.py's ledger TOOL-03).
HERE = os.path.dirname(os.path.abspath(__file__))


def _rate(v):
    """A rate as text. Every rate this tool printed used `,.0f`, so a cell whose peak is 0.75 and whose
    slower rung above is 0.25 printed "peak 0 ... then clean 0 (slower, fail=0)" - two rates the tool had
    just PROVED unequal, displayed as the same number. The verdicts were right; the evidence shown for
    them was unreadable, which is worse than no evidence because it reads as a contradiction."""
    if v is None:
        return "none"
    return f"{v:.2f}" if v < 1 else f"{v:,.0f}"


def num(v):
    """A bare number or None. A JSON bool is not a number (Python's `True` would read as 1)."""
    if isinstance(v, dict):
        v = v.get("value")
    return v if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def window_clean(s):
    """The engine's `ok > 0 and fail == 0` for one window; on a snapshot that predates `ok`, the same
    p99-or-positive-rate fallback bench-audit.py uses (wider than the engine's rule, never narrower)."""
    fail, ok = num(s.get("fail")), num(s.get("ok"))
    if fail is None or fail != 0:
        return False
    if ok is not None:
        return ok > 0
    rps = num(s.get("rps"))
    return (rps is not None and rps > 0) or num(s.get("p99_us")) is not None


def published_paths():
    """The snapshots the BOARD publishes (site/data.json), as verify-frontier.py does - not every
    superseded run still on disk, which fails permanently on data no reader can see. Falls back to
    every snapshot when the board names none."""
    every = sorted(glob.glob(os.path.join(HERE, "results", "snapshots", "*.json")))
    try:
        with open(os.path.join(HERE, "site", "data.json")) as f:
            want = {os.path.basename(g["snapshot_file"])
                    for g in json.load(f).get("gateways", []) if g.get("snapshot_file")}
    except Exception:
        return every
    keep = [p for p in every if os.path.basename(p) in want] if want else every
    if len(keep) != len(every):
        print(f"checking the {len(keep)} snapshot(s) the board publishes "
              f"({len(every) - len(keep)} superseded file(s) on disk not checked)")
    return keep


proved, plateau, cliff, top, disclosed_floor = [], [], [], [], []
unreadable = []

for p in published_paths():
    try:
        with open(p) as fh:
            d = json.load(fh)
    except (OSError, ValueError):
        # A snapshot being written by a live run is not a defect in the DATA - but nothing in it was
        # examined, so it may not be covered by any "EVERY published maximum" claim below, and the run
        # exits non-zero. Re-run once the write lands.
        print(f"  SKIP {p}: not readable as JSON yet (a snapshot mid-write is not a defect)")
        unreadable.append(p)
        continue
    gw = d.get("gateway", "?")
    for eg, up in ((d.get("matrix") or {}).get("upstreams") or {}).items():
        for ing, cell in (up.get("cells") or {}).items():
            if cell.get("served") is not True:
                continue
            perf = cell.get("perf") or {}
            fr = perf.get("frontier") or []
            sweep = perf.get("sweep_max_proxy") or []
            if not fr or not sweep:
                continue
            last = fr[-1]
            if last.get("p99_bound_us") is not None:
                continue
            win_rps, win_c = num(last.get("rps")), num(last.get("concurrency"))
            if win_rps is None or win_c is None:
                continue
            at = f"{gw} {ing}>{eg}"

            # Collapse repeated windows at one concurrency: a rung is clean only if EVERY window at
            # that concurrency served cleanly (see `window_clean`), and its rate is the best window there.
            # `fails` is the failures counted, for display only - it never decides cleanliness.
            by_c = {}
            for s in sweep:
                c = num(s.get("conc"))
                if c is None:
                    continue
                e = by_c.setdefault(c, {"fails": 0, "clean": True, "best": None})
                e["fails"] += num(s.get("fail")) or 0
                e["clean"] = e["clean"] and window_clean(s)
                r = num(s.get("rps"))
                if r is not None and (e["best"] is None or r > e["best"]):
                    e["best"] = r

            above = sorted(c for c in by_c if c > win_c)
            if not above:
                # NOTHING WAS PROBED ABOVE THE PEAK - so this reading established no boundary. Whether
                # that is a DEFECT depends entirely on whether the artifact SAYS SO, which is what
                # `lower_bound` is for: it is set exactly when the winning rung is the highest probed,
                # and the site renders such a rate as a floor (">= N") rather than a ceiling.
                #
                # This used to append unconditionally and exit non-zero, while the closing comment
                # claimed the failure condition was "one appears here while the artifact does not flag
                # it". The flag was never read - `lower_bound` appeared nowhere in the code, only in
                # that comment - so the tool both cried wolf on correctly-disclosed floors AND could
                # not detect the single disagreement it said it existed to catch. A gate that cannot
                # fail on the thing it names is worse than no gate.
                if last.get("lower_bound") is True:
                    disclosed_floor.append((at, win_rps, win_c))
                else:
                    top.append((at, win_rps, win_c))
                continue
            slower_clean = [
                (c, by_c[c]["best"])
                for c in above
                if by_c[c]["clean"] and by_c[c]["best"] is not None and by_c[c]["best"] < win_rps
            ]
            nxt = above[0]
            if slower_clean:
                c, r = slower_clean[0]
                proved.append((at, win_rps, win_c, c, r))
            elif by_c[nxt]["clean"] and by_c[nxt]["best"] is not None and by_c[nxt]["best"] >= win_rps:
                # A CLEAN next rung at the winner's rate (a tie the engine breaks toward the lower
                # concurrency). Nothing failed, so this is no cliff - reporting it as a "cliff that
                # matters" at +0.0% claimed a failure that never happened.
                plateau.append((at, win_rps, win_c, nxt, by_c[nxt]["best"]))
            else:
                cliff.append((at, win_rps, win_c, nxt, by_c[nxt]["fails"], by_c[nxt]["best"]))

tot = len(proved) + len(plateau) + len(cliff) + len(top) + len(disclosed_floor)
print("=" * 96)
print(f"TURNOVER PROOF for {tot} unbounded readings")
print("=" * 96)
print(f"\nPROVED ({len(proved)}) - a clean rung above the peak served strictly slower:")
for at, wr, wc, c, r in proved:
    print(f"  {at[:44]:44s} peak {_rate(wr):>9} @c={wc:<6} then clean {_rate(r):>9} @c={c} (slower, fail=0)")

print(f"\nPLATEAU ({len(plateau)}) - the next rung up was clean at the same rate; no failure, no turnover:")
if not plateau:
    print("  NONE.")
for at, wr, wc, c, r in plateau:
    print(f"  {at[:44]:44s} peak {_rate(wr):>9} @c={wc:<6} then clean {_rate(r):>9} @c={c} (not slower)")

# THE CLIFFS SPLIT IN TWO, and only one half has anything at stake.
#
# A failing rung above the peak only MATTERS if that rung was also FASTER. If it failed but served
# more slowly anyway, throughput had already turned over and the peak stands no matter whose fault the
# failures were - the failures are not doing any of the work of establishing the number.
#
# Where the failing rung was FASTER, the published maximum genuinely depends on excluding it. That is
# still exactly the published definition ("...and it failed none it accepted"), so the number is not
# wrong - but a reader deserves to know that this particular cell would read higher if those failures
# were the rig's rather than the gateway's, and by how much.
moot = [x for x in cliff if x[5] is not None and x[5] < x[1]]
live = [x for x in cliff if not (x[5] is not None and x[5] < x[1])]

print(f"\nCLIFF-BUT-MOOT ({len(moot)}) - the next rung failed AND was slower, so turnover stands regardless:")
if not moot:
    print("  NONE.")
for at, wr, wc, c, f, b in moot:
    print(f"  {at[:44]:44s} peak {_rate(wr):>9} @c={wc:<6} next c={c} failed {f:,.0f} but only reached "
          f"{_rate(b)} - slower than the peak, so the peak is a turnover either way")

print(f"\nCLIFF-THAT-MATTERS ({len(live)}) - the excluded rung was FASTER; the number depends on the exclusion:")
if not live:
    print("  NONE.")
for at, wr, wc, c, f, b in live:
    # THE RUNG ABOVE MAY HAVE NO RATE AT ALL, and this printed it with `,.0f` regardless.
    #
    # `live` is the complement of `moot`, and `moot` requires `b is not None` - so every cliff whose
    # failing rung produced NO rate lands here with b=None and crashed the report with a TypeError.
    # That is not an exotic input: the docstring's own worked example of a cliff is `c=512 - fail=900`,
    # a rung that failed everything and therefore has no rate to report.
    if b is None:
        print(f"  {at[:44]:44s} published {_rate(wr):>9} @c={wc:<6} | c={c} produced NO rate at all "
              f"and failed {f:,.0f} - the exclusion cannot be compared, only disclosed")
        continue
    delta = (b - wr) / wr * 100 if b and wr else 0
    print(f"  {at[:44]:44s} published {_rate(wr):>9} @c={wc:<6} | c={c} reached {_rate(b)} "
          f"(+{delta:.1f}%) but failed {f:,.0f}")

print(f"\nDISCLOSED FLOOR ({len(disclosed_floor)}) - nothing probed above the peak, and the artifact SAYS SO")
print("  (lower_bound=true, so the site renders these as \">= N\" rather than as a ceiling - not a defect)")
for at, wr, wc in disclosed_floor:
    print(f"  {at[:44]:44s} >= {_rate(wr):>9} @c={wc} (top of the probed ladder)")
if not disclosed_floor:
    print("  NONE.")

print(f"\nTOP ({len(top)}) - nothing probed above the peak AND the artifact does NOT disclose it:")
if not top:
    print("  NONE.")
for at, wr, wc in top:
    print(f"  {at[:44]:44s} peak {_rate(wr):>9} @c={wc} - lower_bound is not set, so this rate is published")
    print(f"  {'':44s}   as a ceiling when nothing above it was ever measured")

# THE "EVERY" CLAIMS ARE ONLY MADE ABOUT WHAT WAS EXAMINED, AND ONLY WHEN TRUE OF ALL OF IT. A
# disclosed floor established no maximum at all (nothing above it was probed), an unreadable snapshot
# was not looked at, and with nothing examined "every" is vacuous - each of those suppresses the claims.
covered = tot > 0 and not unreadable and not disclosed_floor
print()
if covered and not live and not top:
    print(f"EVERY published maximum is established without depending on a failure: {len(proved)} by a")
    print(f"clean slower rung above the peak, {len(plateau)} on a clean plateau, {len(moot)} where the")
    print("failing rung was slower anyway.")
print()
if covered and not cliff and not top and not plateau:
    print("EVERY published maximum on this board is a MEASURED TURNOVER: for each one, the rig was")
    print("pushed past the peak and the gateway served cleanly but slower. No published rate depends")
    print("on interpreting a failure, so no published rate can be the rig wearing a gateway's name.")
if unreadable:
    print(f"NOT EXAMINED: {len(unreadable)} snapshot(s) could not be read (see SKIP above) - exiting non-zero")

# EXIT CODE: this is a DISCLOSURE tool, not a gate, with one exception.
#
# A cliff is not a defect. The published definition says "...and it failed none it accepted", so
# excluding a failing rung is the rule working exactly as written and as published. Failing the build
# over it would be failing the build for obeying the definition.
#
# TOP is different: a maximum with nothing probed above it has established no boundary at all, and the
# frontier's own `lower_bound` flag exists to disclose precisely that. If one appears here while the
# artifact does not flag it, the two disagree and something is wrong - so that exits non-zero, and so
# does a snapshot this run could not read, because nothing above may be read as covering it.
sys.exit(1 if top or unreadable else 0)
