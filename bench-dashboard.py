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

CELL_RE = re.compile(r"\[cell (\d+)/(\d+)\] (\S+): (.+)")
PHASE_RE = re.compile(r"\[phase\] (\S+) (\S+)")
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
    st = {"ip": None, "start": None, "terminal": None}
    for ln in lines:
        m = IP_RE.search(ln)
        if m:
            st["ip"] = m.group(1)
        if st["start"] is None:
            m = TS_RE.match(ln)
            if m:
                h, mi, se = (int(x) for x in m.groups())
                st["start"] = h * 3600 + mi * 60 + se
        if "] DONE" in ln:
            st["terminal"] = "DONE"
        elif "INCOMPLETE" in ln:
            st["terminal"] = "INCOMPLETE"
    return st


def remote_tail(ip):
    """The box's own run log. This is where the engine narrates; the fanout log never sees most of it."""
    if not ip:
        return ""
    try:
        r = subprocess.run(SSH + [f"ubuntu@{ip}", "tail -c 20000 ~/benchmarking/.run.log 2>/dev/null"],
                           capture_output=True, text=True, timeout=15)
        return r.stdout
    except Exception:
        return ""


def parse_progress(text):
    """Latest cell position, latest phase, and how many measured cells have completed."""
    cells, phase, served_done, total = None, None, 0, None
    for ln in text.split("\n"):
        m = CELL_RE.search(ln)
        if m:
            done, tot, cid, verdict = int(m.group(1)), int(m.group(2)), m.group(3), m.group(4)
            cells, total = done, tot
            if verdict.startswith(MEASURED):
                served_done += 1
            continue
        m = PHASE_RE.search(ln)
        if m:
            phase = f"{m.group(2)} {m.group(1)}"
    return cells, total, served_done, phase


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
    cells, total, served_done, phase = parse_progress(text)
    expect = declared_served(gw)

    elapsed = None
    if st.get("start") is not None:
        elapsed = now_s - st["start"]
        if elapsed < 0:
            elapsed += 86400  # the run crossed midnight

    eta = None
    if elapsed and served_done > 0 and expect:
        per = elapsed / served_done
        left = max(expect - served_done, 0)
        eta = per * left

    if cells is None:
        phase = phase or ("booting/building" if st.get("ip") else "launching")

    return dict(
        gw=gw,
        phase=(phase or "-")[:34],
        cells=f"{cells}/{total}" if cells else "-",
        served=f"{served_done}/{expect}" if expect is not None else str(served_done),
        elapsed=hms(elapsed),
        eta=hms(eta) if eta is not None else ("~" if served_done == 0 else "-"),
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
