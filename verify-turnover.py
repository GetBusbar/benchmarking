#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
"""Is each published peak a real TURNOVER, or could it be a failure CLIFF the rig caused?

A peak whose next rung up FAILED is only the gateway's maximum if those failures are the gateway's,
not the rig's (ephemeral ports, file descriptors, the mock) - attributing a rig limit to the gateway
is the one thing the board must never do. The case is unambiguous when a clean rung above the winner
served strictly slower: e.g. c=256 wins at 24,097 rps, c=512 is clean but slower at 23,456 rps -
throughput genuinely turned over regardless of what a failing c=1024 does above it.

Classify every unbounded reading:
  PROVED - a clean rung above the winner served strictly slower. Turnover is measured.
  CLIFF  - the winner's next probed rung up failed, with no clean slower rung above it. Not
           necessarily wrong, but the number rests on those failures being the gateway's.
  TOP    - nothing above was probed (should be zero; the other audit checks this).
"""
import glob
import json


def _rate(v):
    """Rate as text. `,.0f` alone would print both 0.75 and 0.25 as "0", making two rates just proved
    unequal display as the same number, so sub-1 rates get two decimals."""
    if v is None:
        return "none"
    return f"{v:.2f}" if v < 1 else f"{v:,.0f}"


def num(v):
    if isinstance(v, dict):
        v = v.get("value")
    return v if isinstance(v, (int, float)) else None


proved, cliff, top, disclosed_floor = [], [], [], []

for p in sorted(glob.glob("results/snapshots/*.json")):
    try:
        d = json.load(open(p))
    except (OSError, ValueError):
        # A snapshot being written by a live run is not a defect - skip it rather than crash, same as
        # every sibling auditor.
        print(f"  SKIP {p}: not readable as JSON yet (a snapshot mid-write is not a defect)")
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
            # that concurrency failed nothing, and its rate is the best clean window there.
            by_c = {}
            for s in sweep:
                c = num(s.get("conc"))
                if c is None:
                    continue
                e = by_c.setdefault(c, {"fails": 0, "best": None})
                e["fails"] += num(s.get("fail")) or 0
                r = num(s.get("rps"))
                if r is not None and (e["best"] is None or r > e["best"]):
                    e["best"] = r

            above = sorted(c for c in by_c if c > win_c)
            if not above:
                # Nothing probed above the peak, so this reading established no boundary. Whether that's
                # a defect depends on whether the artifact discloses it via `lower_bound` (set when the
                # winning rung is the highest probed; the site renders it as a floor, ">= N").
                if last.get("lower_bound") is True:
                    disclosed_floor.append((at, win_rps, win_c))
                else:
                    top.append((at, win_rps, win_c))
                continue
            slower_clean = [
                (c, by_c[c]["best"])
                for c in above
                if by_c[c]["fails"] == 0 and by_c[c]["best"] is not None and by_c[c]["best"] < win_rps
            ]
            nxt = above[0]
            if slower_clean:
                c, r = slower_clean[0]
                proved.append((at, win_rps, win_c, c, r))
            else:
                cliff.append((at, win_rps, win_c, nxt, by_c[nxt]["fails"], by_c[nxt]["best"]))

tot = len(proved) + len(cliff) + len(top)
print("=" * 96)
print(f"TURNOVER PROOF for {tot} unbounded readings")
print("=" * 96)
print(f"\nPROVED ({len(proved)}) - a clean rung above the peak served strictly slower:")
for at, wr, wc, c, r in proved:
    print(f"  {at[:44]:44s} peak {_rate(wr):>9} @c={wc:<6} then clean {_rate(r):>9} @c={c} (slower, fail=0)")

# Cliffs split in two: a failing rung above the peak only matters if it was also FASTER. If it failed
# but was slower anyway, the peak stands regardless of whose fault the failures were. If it was faster,
# the published maximum genuinely depends on excluding it - not wrong (matches the published
# definition), but worth knowing how much higher it'd read if those failures were the rig's.
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
    # The rung above may have no rate at all (a rung that failed everything reports none) - `live` is
    # the complement of `moot`, which requires `b is not None`, so this branch must handle b=None.
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

print()
if not live and not top:
    print(f"EVERY published maximum is established without depending on a failure: {len(proved)} by a")
    print(f"clean slower rung above the peak, {len(moot)} where the failing rung was slower anyway.")
print()
if not cliff and not top:
    print("EVERY published maximum on this board is a MEASURED TURNOVER: for each one, the rig was")
    print("pushed past the peak and the gateway served cleanly but slower. No published rate depends")
    print("on interpreting a failure, so no published rate can be the rig wearing a gateway's name.")

# Exit code: a disclosure tool, not a gate, with one exception. A cliff is not a defect - excluding a
# failing rung is the published rule working as written. TOP is different: a maximum with nothing
# probed above it and no `lower_bound` disclosure is a real disagreement, so that alone exits non-zero.
import sys

sys.exit(1 if top else 0)
