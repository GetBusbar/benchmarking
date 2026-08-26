#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Live progress for an in-flight field run: what each box is doing right now, how far through the
# grid it is, and roughly how much longer it has. The fanout log narrates setup and then says
# nothing until DONE, so without this a wedged box and a nearly-finished box look identical.
#
# ETA is built on served cells, not all cells: a not_configurable cell prints in milliseconds while
# a served one runs a full metric battery, so averaging over every cell would be confidently wrong.
# Each gateway's own definition.json declares how many cells it claims to serve, so the estimate is
# (elapsed per served cell so far) x (served cells left).
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

# Measured share of a cell's wall clock per metric group, so an ETA exists before the first cell
# completes (dividing by a completed cell alone gives no estimate for the first several minutes,
# exactly when an operator is deciding whether the run is healthy). This is one gateway's profile
# used as a prior for all; it only affects how far along the CURRENT cell is assumed to be, and
# real elapsed-per-cell takes over once a cell completes.
#
# Keys must be the engine's own metric-group names, since the only lookup is `[phase] <cell>
# <group>` off the engine's log. A test parses metric.rs's METRICS list and fails if this table
# and the engine's groups ever diverge — a phantom or stale entry here silently adds weight to
# `cell_fraction_done` and skews every ETA (this has happened twice: a `sustained_throughput`
# entry for a group that no longer existed, and later a retired `cpu_fps`).
#
# Re-derived from measured wall clock on the current engine (not redistributed old guesses), after
# the sustained-search phase was folded into throughput and memory became a fixed duration. This is
# provisional — measured on one mock-gateway cell — so shares, especially throughput's, will shift
# once re-derived from real field runs (the board emits per-phase `[cost N/M]` seconds for exactly
# that purpose).
PHASE_COST = {
    # Renormalised (not appended to) when `cost` was added: the self-test asserts these sum to
    # exactly 1.0, so a new group must be folded in rather than bolted on, or a cell would read as
    # >100% of itself and skew every ETA.
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

    0.0 for an unrecognised phase is deliberate: a group this table hasn't seen makes the ETA
    pessimistic rather than confidently wrong (the reverse — guessing a fraction for an undeclared
    name — is how a stale table entry once inflated every late-cell estimate)."""
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

# A cell whose verdict is one of these was skipped or refused, and cost almost nothing.
CHEAP = ("not_configurable", "untestable", "unprobed_auth")
# Served cells are an allowlist, not "anything not in CHEAP" — that was fail-open and let an
# unrelated line shape (e.g. a per-cell cost breakdown under the same `[cell N/M]` prefix) get
# double-counted as served, inflating the served count past the declared total.
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

    Filtered against gateways/*/definition.json (the same source the engine and site use) rather
    than trusting the log filename: orchestration logs (`fanout-validate-*.log`,
    `fanout-field-<sha>.log`, ...) are named like gateway logs and would otherwise show up as
    phantom rows reporting DONE with no cells.
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
        # When measurement actually started, not when the box was launched: provisioning and a
        # source build can take minutes, and charging those to the first cell inflates the per-cell
        # rate several-fold. ETA divides by this; ELAPSED still shows wall clock (what's billed).
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
    """The box's own run log, filtered to the lines this dashboard parses.

    Filters server-side to the two line shapes actually parsed (`^[cell` / `^[phase]`) rather than
    capping by raw byte count: a byte cap (previously `tail -c 20000`) can scroll early `[cell N/M]`
    lines out of the window on a heavily-narrated grid, silently undercounting served cells and
    inflating the ETA. `tail -n 4000` comfortably covers a 36-cell grid's ~460 matching lines;
    `parse_progress`'s completeness check catches it if that assumption is ever wrong."""
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

    `complete` guards `served_done`: the count comes from counting lines, so it's only accurate if
    every cell's line is present. If the log starts mid-grid it's a LOWER BOUND, which understates
    progress and overstates the ETA — the caller marks these as bounds rather than dropping them.
    The engine numbers cells contiguously from 1, so checking that every position 1..latest was seen
    detects a truncated log exactly, without guessing at how chatty it is. This is a set of positions
    rather than a line count because a cell can emit several `[cell ...]` lines (e.g. a restart)."""
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
        # Include the fraction of the in-flight cell; otherwise there's no estimate until it completes.
        in_flight = cell_fraction_done(phase.split()[0] if phase else None)
        progress = served_done + in_flight
        if progress > 0:
            per = measuring / progress
            left = max(expect - progress, 0.0)
            eta = per * left

    if cells is None:
        phase = phase or ("booting/building" if st.get("ip") else "launching")

    # "≥" marks served/ETA as a lower bound when the log doesn't reach back to cell 1, so an
    # operator doesn't mistake a floor for an exact reading.
    lb = "" if complete else "≥"
    served = f"{lb}{served_done}/{expect}" if expect is not None else f"{lb}{served_done}"

    return dict(
        gw=gw,
        phase=(phase or "-")[:34],
        # `is not None`, not truthiness: cell 0 is a valid position and must not print as "-".
        cells=f"{cells}/{total}" if cells is not None else "-",
        served=served,
        elapsed=hms(elapsed),
        # An ETA built on a floor carries the same "≥" mark. "~" means "nothing to divide by yet",
        # distinct from "at least this long".
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
