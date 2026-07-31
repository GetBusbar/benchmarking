#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Live progress for an in-flight field run: what each box is doing RIGHT NOW, how far through the
# grid it is, and roughly how much longer it has.
#
# WHY IT EXISTS. A field run is 13 boxes billing by the hour and going quiet for long stretches. The
# fanout log narrates setup and then says nothing until DONE, so a wedged box, a box grinding through
# a slow sweep, and a box that finished ten minutes ago all look identical from here. That is how a
# run burns hours before anyone notices - which is exactly what happened on the run this was written
# after.
#
# ETA IS BUILT ON SERVED CELLS, NOT ON CELLS. Cell cost is wildly uneven: a not_configurable cell
# prints in milliseconds while a served one runs a full throughput/stream/memory battery. Averaging
# over all 36 would report a confident number that is simply wrong, and would swing wildly as the
# grid walked through a block of greys. Each gateway's own definition.json declares how many cells it
# claims to serve, so the estimate is (elapsed per served cell so far) x (served cells left).
#
# Usage:  python3 bench-dashboard.py            # one snapshot
#         python3 bench-dashboard.py --watch    # refresh until every box is done

import concurrent.futures
import json
import os
import re
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
KEY = os.path.join(os.environ.get("TMPDIR", "/tmp"), "gateway-bench-key.pem")
SSH = ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
       "-o", "ConnectTimeout=6", "-o", "BatchMode=yes", "-i", KEY]


def _configure_stdout():
    for kwargs in ({"encoding": "utf-8", "errors": "backslashreplace"},
                   {"errors": "backslashreplace"}):
        try:
            sys.stdout.reconfigure(**kwargs)
            return
        except (AttributeError, ValueError, OSError):
            continue


_configure_stdout()

CELL_RE = re.compile(r"\[cell (\d+)/(\d+)\] (\S+): (.+)")
PHASE_RE = re.compile(r"\[phase\] (\S+) (\S+)")

# WHERE A CELL'S TIME ACTUALLY GOES, so an ETA exists before the first cell finishes.
#
# The ETA used to need a completed SERVED cell to divide by, which on a gateway whose cells take ten
# minutes means no estimate for the first ten minutes - exactly when an operator is deciding whether
# the run is healthy. These weights are the measured share of a cell's wall clock per metric group,
# from the per-cell cost breakdown the engine now records (agentgateway, 5 cells, 2026-07-28):
# memory dominates at ~40%, and everything streaming-related is under 6% combined.
#
# They are one gateway's profile used as a prior for all, which is honest for an ETA and would not be
# for a published number. It only ever affects how far along the CURRENT cell is assumed to be; once
# a cell completes, real elapsed-per-cell takes over.
#
# THE KEYS ARE THE ENGINE'S GROUP NAMES, AND THEY HAVE TO BE, because the only thing that ever looks
# them up is `[phase] <cell> <group>` off the engine's own log. This table used to carry a seventh
# entry, `sustained_throughput` at 0.198, for a metric group `engine/src/metric.rs` does not have -
# the sustained figure moved INTO `throughput` when it stopped being a search of its own and became a
# summary of the same sweep (metric.rs says so in as many words, in the test that used to assert the
# group existed). Nothing ever emitted `[phase] <cell> sustained_throughput`, so the entry was not
# merely unused: `cell_fraction_done` walks PHASE_ORDER accumulating weight until it reaches the
# phase it was given, so the phantom's 0.198 was ADDED to the progress of every cell that reached
# `streams_sustained` or the since-retired `cpu_fps`. A cell reported 89.8% done when 70% of its declared
# weight had elapsed, which shortened the ETA of every gateway in the last third of every cell - a
# fabricated number, arrived at by counting work that does not exist.
#
# The 0.198 is folded back into `throughput`, which is where that wall clock is actually spent now,
# rather than dropped: the sustained search did not get faster, it changed owners. bench-dashboard's
# test parses metric.rs's METRICS list and fails if this table and the engine's groups ever diverge
# again, because "the dashboard's phase table matches the engine" is exactly the kind of fact that is
# true when written and silently false a refactor later.
# RE-MEASURED, AND `cpu_fps` IS GONE - which is the same defect this comment block describes, recurring.
#
# `cpu_fps` was retired from the engine (see run.rs: of the 16 cells that published both it and
# `streams_sustained_fps`, 4 had it INVERTED below the proven delivery boundary, 5 were redundant, and 7
# were measured where the delivery gate did not hold). Its 0.102 then became exactly the phantom weight
# described above: `cell_fraction_done` accumulates PHASE_ORDER weight until it reaches the given phase,
# so a cell in `streams_sustained` would have reported progress including 10.2% of work that no longer
# exists. The test that parses metric.rs's METRICS caught it, which is what it is for.
#
# The remaining five are re-derived from MEASURED wall clock on the new engine rather than by
# redistributing the old guesses, because two things changed size at once: the sustained bisection was
# deleted (its windows folded into the climb) and the memory phase became a FIXED duration instead of
# stopping when the RSS trace looked flat. From a 2026-07-30 single-cell run:
#
#     memory 360.5s | throughput 162.2s | streams_sustained 12.3s | added_latency 12.0s | streaming 2.7s
#     total 549.7s
#
# PROVISIONAL, and honestly so: that is one cell, on one box, with the mock standing in for the gateway,
# so the throughput share in particular will move on a real gateway whose climb runs further. The
# in-flight 14-gateway board emits a `[cost N/M] <cell>: ...` line per cell with exactly these
# per-phase seconds, so the table can be re-derived from the real field distribution once it lands.
# These are strictly better than what they replace either way: the old numbers predated both changes.
PHASE_COST = {
    # RENORMALISED WHEN `cost` WAS ADDED, not appended to. The weights are a fraction of ONE cell and
    # the self-test asserts they sum to exactly 1.0, so a sixth group cannot simply be bolted on -
    # doing that would claim a cell is 101.1% of itself and quietly skew every ETA.
    #
    # `cost` runs ONE load window; `added_latency` runs TWO (gateway leg and direct leg) for 0.022, so
    # one window is ~0.011. The other five keep their measured proportions exactly - each is its old
    # weight over the new total - and the rounding drift is absorbed into the largest, `memory`,
    # where a 1e-6 adjustment is far below the noise in the estimate it feeds.
    "throughput": 0.29179,
    "memory": 0.648862,
    "streaming": 0.004946,
    "added_latency": 0.021761,
    "streams_sustained": 0.021761,
    "cost": 0.01088,
}
# Fraction of a cell already done when a given phase STARTS, in the order the engine runs them
# (metric::METRICS, in that order).
PHASE_ORDER = ["throughput", "memory", "streaming", "added_latency", "streams_sustained", "cost"]


def cell_fraction_done(phase_name):
    """How much of the current cell is behind us, given the phase it is in. 0.0 when unknown.

    0.0 for an unrecognised phase is deliberate and is the safe direction: a group this table has
    never heard of contributes no claimed progress, so a new metric group in the engine makes the ETA
    pessimistic rather than making it confidently wrong. The reverse - guessing a fraction for a name
    nobody declared - is how the retired `sustained_throughput` entry inflated every late-cell
    estimate for as long as it sat in the table."""
    if not phase_name:
        return 0.0
    done = 0.0
    for name in PHASE_ORDER:
        if name == phase_name:
            return done
        done += PHASE_COST.get(name, 0.0)
    return 0.0
IP_RE = re.compile(r"ip=([0-9.]+)")
TS_RE = re.compile(r"^\[(\d\d):(\d\d):(\d\d)\]")

# A cell whose verdict is one of these was skipped or refused, and cost almost nothing. Anything else
# was measured, and is what the remaining time is actually made of.
CHEAP = ("not_configurable", "untestable", "unprobed_auth")
# A CELL IS SERVED ONLY IF THE ENGINE SAID SO. This used to count anything whose verdict was not one
# of CHEAP, which is fail-open: a new line shaped like `[cell N/M] <id>: <anything>` silently became a
# served cell. That happened the day a per-cell cost breakdown was added under the same prefix - every
# measured cell counted twice, gateways showed "10/8 served", and the ETA hit zero while they were
# still running. Listing what DOES count means an unrecognised line is ignored rather than believed.
MEASURED = ("served", "failed")


def declared_served(gw):
    """How many cells this gateway's own manifest claims to serve: the '1's in its matrix."""
    try:
        with open(os.path.join(HERE, "gateways", gw, "definition.json")) as f:
            rows = json.load(f).get("matrix") or []
        return sum(row.count("1") for row in rows)
    except Exception:
        return None


def fanout_logs():
    """The per-gateway logs of the current fanout, keyed by gateway.

    A GATEWAY IS ONE THAT EXISTS, not one whose name happens to be in a filename. The log name is
    chosen by whoever launched the run, so an orchestration log - `fanout-validate-agentgateway.log`,
    `fanout-rerun-searchfix.log`, `fanout-field-<sha>.log` - used to appear as its own row, reported
    DONE with no cells, and read exactly like a gateway that finished measuring nothing. Four phantom
    rows in a 14-gateway table is not a cosmetic problem: it is the status board saying something
    about a gateway that does not exist.

    Discovered against gateways/*/definition.json, which is the same source the engine and the site
    use, so a name only appears here if there is something to measure.
    """
    d = os.path.join(HERE, "results")
    if not os.path.isdir(d):
        return {}
    gwdir = os.path.join(HERE, "gateways")
    real = {
        name
        for name in os.listdir(gwdir)
        if os.path.isfile(os.path.join(gwdir, name, "definition.json"))
    } if os.path.isdir(gwdir) else set()
    out = {}
    for f in sorted(os.listdir(d)):
        if not (f.startswith("fanout-") and f.endswith(".log")):
            continue
        name = f[len("fanout-"):-len(".log")]
        if name in real:
            out[name] = os.path.join(d, f)
    return out


def local_state(path):
    """Everything the orchestrator wrote here: start stamp, ip, and any terminal verdict."""
    try:
        with open(path, errors="replace") as f:
            lines = f.read().split("\n")
    except OSError:
        return {}
    st = {"ip": None, "start": None, "measuring": None, "terminal": None}
    for ln in lines:
        m = IP_RE.search(ln)
        if m:
            st["ip"] = m.group(1)
        if st["start"] is None:
            m = TS_RE.match(ln)
            if m:
                h, mi, se = (int(x) for x in m.groups())
                st["start"] = h * 3600 + mi * 60 + se
        # WHEN MEASUREMENT ACTUALLY STARTED, which is not when the box was launched. Provisioning and
        # a source build can take minutes, and charging those to the first cell made the per-cell rate
        # look several times worse than it is - aisix read as 44 minutes a cell when its cells take
        # eight to twelve. The ETA divides by measuring time; ELAPSED still shows wall clock, because
        # that is what an operator is paying for.
        if st["measuring"] is None and "running " in ln:
            m = TS_RE.match(ln)
            if m:
                h, mi, se = (int(x) for x in m.groups())
                st["measuring"] = h * 3600 + mi * 60 + se
        if "] DONE" in ln:
            st["terminal"] = "DONE"
        elif "INCOMPLETE" in ln:
            st["terminal"] = "INCOMPLETE"
    return st


def remote_tail(ip):
    """The box's own run log, FILTERED to the lines this dashboard parses.

    IT USED TO BE `tail -c 20000`, WHICH IS A CAP ON HISTORY, NOT ON NOISE. The engine narrates
    heavily - probes, sweep rungs, restarts, cost breakdowns - and a 36-cell grid produces far more
    than 20 KB of it. `parse_progress` counts served cells by counting `[cell N/M]` verdict lines, so
    once the earliest ones scrolled out of that window the count silently fell: the SERVED column read
    "3/8" on a gateway that had finished seven, and the ETA divided the elapsed time by a progress
    figure it knew to be too small, reporting hours of remaining work on a run that was nearly done.
    Both numbers are on the screen an operator uses to decide whether to kill a box.

    Filtering server-side is the fix rather than raising the cap, because it bounds the transfer by
    what is actually parsed (two line shapes, both anchored at column 0) instead of by bytes of prose.
    A 36-cell grid emits 36-ish cell lines and about 430 phase lines, so `tail -n 4000` keeps the whole
    history of any real run while still refusing to stream an unbounded file. The completeness check
    in `parse_progress` is what catches it if that assumption is ever wrong."""
    if not ip:
        return ""
    try:
        r = subprocess.run(
            SSH + [f"ubuntu@{ip}",
                   "grep -a -E '^\\[(cell|phase)' ~/benchmarking/.run.log 2>/dev/null | tail -n 4000"],
            capture_output=True, text=True, timeout=15)
        return r.stdout
    except Exception:
        return ""


def parse_progress(text):
    """Latest cell position, latest phase, how many measured cells completed, and whether the log we
    read covers the whole run.

    `complete` is the guard on `served_done`. The count is derived by counting lines, so it is only a
    count of cells if every cell's line is in front of us; if the log we read starts mid-grid, it is a
    LOWER BOUND and everything computed from it (the SERVED column, the ETA) is wrong in the flattering
    direction - fewer cells done than really are means a longer estimate and a run that looks less
    finished than it is. The engine numbers its cells absolutely and contiguously from 1
    (`[cell {done}/{total}]`, run.rs), so "did we see every position from 1 to the latest?" answers the
    question exactly, with no heuristic about how chatty the log is. A cell can emit several `[cell …]`
    lines (the mid-grid restart narrates under the same prefix), which is why this is a set of
    positions and not a line count.

    The caller marks the derived figures as bounds rather than dropping them: a floor on progress is
    still worth showing, and silently presenting a floor as a measurement is the thing to avoid."""
    cells, phase, served_done, total = None, None, 0, None
    positions = set()
    for ln in text.split("\n"):
        m = CELL_RE.search(ln)
        if m:
            done, tot, cid, verdict = int(m.group(1)), int(m.group(2)), m.group(3), m.group(4)
            cells, total = done, tot
            positions.add(done)
            if verdict.startswith(MEASURED):
                served_done += 1
            continue
        m = PHASE_RE.search(ln)
        if m:
            phase = f"{m.group(2)} {m.group(1)}"
    complete = cells is None or positions == set(range(1, cells + 1))
    return cells, total, served_done, phase, complete


def hms(sec):
    if sec is None or sec < 0:
        return "-"
    sec = int(sec)
    if sec < 3600:
        return f"{sec // 60}m{sec % 60:02d}s"
    return f"{sec // 3600}h{(sec % 3600) // 60:02d}m"


def row_for(gw, path, now_s):
    st = local_state(path)
    if st.get("terminal"):
        return dict(gw=gw, phase=st["terminal"], cells="-", served="-", elapsed="-", eta="-",
                    done=st["terminal"] == "DONE", bad=st["terminal"] == "INCOMPLETE")

    text = remote_tail(st.get("ip"))
    cells, total, served_done, phase, complete = parse_progress(text)
    expect = declared_served(gw)

    elapsed = None
    if st.get("start") is not None:
        elapsed = now_s - st["start"]
        if elapsed < 0:
            elapsed += 86400  # the run crossed midnight

    # Time spent MEASURING, which is what the per-cell rate must divide by. Falls back to the box's
    # whole lifetime when the "running <gw>" line has not appeared yet (or a caller supplies no such
    # stamp): a slightly pessimistic estimate beats none, and it corrects itself the moment
    # measurement starts.
    measuring = elapsed
    if st.get("measuring") is not None:
        measuring = now_s - st["measuring"]
        if measuring < 0:
            measuring += 24 * 3600
    eta = None
    if measuring and expect:
        # Progress in CELLS, including the fraction of the one in flight. Without the fraction there
        # is no estimate at all until a cell completes.
        in_flight = cell_fraction_done(phase.split()[0] if phase else None)
        progress = served_done + in_flight
        if progress > 0:
            per = measuring / progress
            left = max(expect - progress, 0.0)
            eta = per * left

    if cells is None:
        phase = phase or ("booting/building" if st.get("ip") else "launching")

    # WHEN THE HISTORY IS SHORT, SAY SO IN THE CELL RATHER THAN IN A COMMENT. `served_done` is a count
    # of lines we saw; if the log did not reach back to cell 1 it is a floor, and so is the ETA built
    # on it. "≥" is four pixels of honesty that stop an operator reading a lower bound as a reading.
    lb = "" if complete else "≥"
    served = f"{lb}{served_done}/{expect}" if expect is not None else f"{lb}{served_done}"

    return dict(
        gw=gw,
        phase=(phase or "-")[:34],
        # `if cells is not None`, not `if cells`: the engine's counter is 1-based today so a 0 cannot
        # appear, but the falsy-zero reading is the same bug the ETA column already had (an eta of
        # exactly 0.0 rendering as "no estimate"), and a position of 0 would print "-" - "no cell
        # information at all" - about a run that had told us exactly where it was.
        cells=f"{cells}/{total}" if cells is not None else "-",
        served=served,
        elapsed=hms(elapsed),
        # An ETA built on a floor is itself a floor, so it carries the same mark as the count it came
        # from. "~" still means "nothing to divide by yet", which is a different statement from "at
        # least this long".
        eta=(lb + hms(eta)) if eta is not None else ("~" if served_done == 0 else "-"),
        done=False, bad=False,
    )


def render(rows):
    now = time.strftime("%H:%M:%S")
    print(f"onthebench field run - {len(rows)} gateways{' ' * 28}{now}")
    print(f"{'GATEWAY':<16} {'PHASE':<34} {'CELLS':<8} {'SERVED':<8} {'ELAPSED':<9} ETA")
    print("-" * 92)
    for r in rows:
        print(f"{r['gw']:<16} {r['phase']:<34} {r['cells']:<8} {r['served']:<8} {r['elapsed']:<9} {r['eta']}")
    print("-" * 92)
    d = sum(1 for r in rows if r["done"])
    b = sum(1 for r in rows if r["bad"])
    print(f"{d} done · {len(rows) - d - b} running · {b} incomplete")
    return d + b == len(rows)


def main():
    watch = "--watch" in sys.argv
    logs = fanout_logs()
    if not logs:
        print("no results/fanout-*.log - is a run-on-ec2.sh fanout in progress?")
        return 1
    while True:
        t = time.localtime()
        now_s = t.tm_hour * 3600 + t.tm_min * 60 + t.tm_sec
        # One SSH per box, all at once: serially this would take longer than the refresh interval.
        with concurrent.futures.ThreadPoolExecutor(max_workers=16) as ex:
            rows = list(ex.map(lambda kv: row_for(kv[0], kv[1], now_s), sorted(logs.items())))
        if watch:
            os.system("clear")
        finished = render(rows)
        if not watch or finished:
            return 0
        time.sleep(20)


if __name__ == "__main__":
    sys.exit(main())
