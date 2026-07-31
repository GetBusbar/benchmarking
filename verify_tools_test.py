#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# RED FIXTURES FOR THE TWO AUDITORS THAT HAD NEITHER A TEST NOR A PLACE IN THE CI GATE.
#
# `verify-latency.py` and `audit-every-metric.py` were written in one sitting during a live run and
# shipped with no test file and no CI invocation. A later audit round found that `verify-frontier.py`
# and `verify-turnover.py` - the OLDER and more important pair, and the ones that actually re-derive
# the frontier - had neither either, so fixing only the two newest was a half-fix. All four are
# covered here now. That is the same shape as the defects they exist to
# catch: an oracle nobody has shown can fail is an oracle nobody should believe. A typo'd field name
# that always resolves to `None` would leave `checked` at 0 and print PASS on every board forever, and
# nothing would notice.
#
# So each check gets a fixture that violates exactly it, and the test asserts the tool FAILS. The
# accept side is covered too, because a checker that rejects everything is equally useless.
#
# Run: python3 verify_tools_test.py

import copy
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))

# A minimal cell that violates nothing: the baseline every red fixture is a single mutation away from.
CLEAN_CELL = {
    "served": True,
    "perf": {
        "added_latency_p50_us": 95,
        "added_latency_p99_us": 105,
        "gateway_c1_p99_us": 136,
        "direct_c1_p99_us": 31,
        # ALL SIX DECLARED BOUNDS. verify-frontier.py checks the published sequence against
        # frontier.rs's P99_BOUNDS_US and rejects a frontier of a different shape, so a two-entry
        # fixture would fail for the wrong reason and hide whatever the test meant to assert.
        "frontier": [
            # At 1 ms the c=16 rung (p99 1500us) is already disqualified, so the boundary above the
            # winner is 16 - not 32. The tool caught this fixture error, which is the behaviour wanted.
            {"p99_bound_us": 1000, "rps": 100.0, "concurrency": 8, "p99_us": 900,
             "first_disqualified_conc": 16, "lower_bound": False},
            {"p99_bound_us": 5000, "rps": 120.0, "concurrency": 16, "p99_us": 1500,
             "first_disqualified_conc": 32, "lower_bound": False},
            {"p99_bound_us": 10000, "rps": 120.0, "concurrency": 16, "p99_us": 1500,
             "first_disqualified_conc": 32, "lower_bound": False},
            {"p99_bound_us": 50000, "rps": 120.0, "concurrency": 16, "p99_us": 1500,
             "first_disqualified_conc": 32, "lower_bound": False},
            {"p99_bound_us": 100000, "rps": 120.0, "concurrency": 16, "p99_us": 1500,
             "first_disqualified_conc": 32, "lower_bound": False},
            {"p99_bound_us": None, "rps": 120.0, "concurrency": 16, "p99_us": 1500,
             "first_disqualified_conc": 32, "lower_bound": False},
        ],
        "sweep_max_proxy": [
            {"conc": 8, "ok": 100, "rps": 100.0, "p99_us": 900, "fail": 0},
            {"conc": 16, "ok": 120, "rps": 120.0, "p99_us": 1500, "fail": 0},
            {"conc": 32, "ok": 110, "rps": 110.0, "p99_us": 2500, "fail": 1},
        ],
    },
    "memory": {
        "idle_rss_mib": 100.0, "steady_state_rss_mib": 200.0, "peak_rss_mib": 210.0,
        "peak_rss_hwm_mib": 210.0, "recovered_rss_mib": 205.0,
        "growth_rate_mib_per_min": 0.1, "load_s": 360, "idle_window_s": 59,
        "recovery_window_s": 30,
    },
    "stream": {"stream_served": False},
    "absences": {},
}


def snapshot(cell):
    return {
        "schema_version": 2, "gateway": "fixture", "measured_at": "2026-07-30T00:00:00Z",
        "rig": {"engine": {"commit": "0" * 12, "dirty": False}},
        "matrix": {"upstreams": {"openai": {"cells": {"openai": cell}}}},
    }


def run(tool, cell):
    """Run one auditor over a one-cell board and return (exit_code, output).

    `verify-frontier.py` deliberately PARSES engine/src/frontier.rs for the declared bounds rather than
    hardcoding them - so it cannot police a different axis than the one that ran, and going blind counts
    as a failure rather than a pass. The fixture tree therefore has to carry that file too.
    """
    d = tempfile.mkdtemp()
    try:
        os.makedirs(os.path.join(d, "results", "snapshots"))
        os.makedirs(os.path.join(d, "engine", "src"), exist_ok=True)
        shutil.copy(os.path.join(HERE, "engine", "src", "frontier.rs"),
                    os.path.join(d, "engine", "src", "frontier.rs"))
        with open(os.path.join(d, "results", "snapshots", "result_fixture_2026-07-30T00-00-00Z.json"), "w") as f:
            json.dump(snapshot(cell), f)
        shutil.copy(os.path.join(HERE, tool), d)
        p = subprocess.run([sys.executable, tool], cwd=d, capture_output=True, text=True)
        return p.returncode, p.stdout + p.stderr
    finally:
        shutil.rmtree(d, ignore_errors=True)


def mutate(**changes):
    c = copy.deepcopy(CLEAN_CELL)
    for path, val in changes.items():
        block, _, field = path.partition("__")
        c[block][field] = val
    return c


FAILURES = []


def expect(name, code, out, want_fail, needle=""):
    ok = (code != 0) if want_fail else (code == 0)
    if ok and needle and needle not in out:
        ok = False
        why = f"expected the message to mention {needle!r}"
    else:
        why = f"exit={code}, wanted {'non-zero' if want_fail else 'zero'}"
    print(f"  {'ok  ' if ok else 'FAIL'} - {name}")
    if not ok:
        FAILURES.append(f"{name}: {why}\n{out[:400]}")


def main():
    print("verify-latency.py:")
    # ACCEPT: a clean cell whose added latency really is the difference of its two legs.
    c, o = run("verify-latency.py", CLEAN_CELL)
    expect("a cell whose legs subtract correctly passes", c, o, False)

    # RED: the published difference disagrees with its own operands. This is the check's whole point,
    # and a typo'd field name would silently skip it while still printing PASS.
    c, o = run("verify-latency.py", mutate(perf__added_latency_p99_us=999))
    expect("a difference that disagrees with its legs FAILS", c, o, True, "re-derived")

    # RED: a difference published with no legs to check it against is unverifiable by anyone.
    bad = copy.deepcopy(CLEAN_CELL); del bad["perf"]["gateway_c1_p99_us"]
    c, o = run("verify-latency.py", bad)
    expect("a difference published without its legs FAILS", c, o, True)

    # RED: p50 above p99 from one distribution is impossible.
    # ACCEPTED, not failed: added_latency p50/p99 are DIFFERENCES of two distributions' percentiles,
    # so p50 > p99 is a legitimate reading, not a swap. This assertion used to demand a FAILURE, which
    # is how the unsound rule stayed green here while its twin fired on real field data.
    c, o = run("verify-latency.py", mutate(perf__added_latency_p50_us=500))
    expect("an added_latency p50 above its p99 is ACCEPTED (a difference, not one distribution)", c, o, False)

    print("audit-every-metric.py:")
    c, o = run("audit-every-metric.py", CLEAN_CELL)
    expect("a clean cell passes", c, o, False)

    # RED: a quantity that cannot be negative, published negative.
    c, o = run("audit-every-metric.py", mutate(memory__idle_rss_mib=-5.0))
    expect("a negative RSS FAILS", c, o, True, "negative")

    # RED: a NEGATIVE cost per request. The engine already refuses this at the counters - a backwards
    # counter (pid reuse) becomes Absent::HarnessError rather than a subtraction into a negative - so
    # a negative reaching the artifact means that refusal was bypassed. This is the second line of
    # defence, and it exists because a negative CPU time is an impossible number, and by this
    # project's rule an impossible number is an ENGINE bug that must never be published as a gateway
    # property.
    c, o = run("audit-every-metric.py", mutate(perf__cpu_us_per_request=-12.5))
    expect("a NEGATIVE cpu_us_per_request FAILS", c, o, True, "negative")

    # RED: a peak below the steady state it peaked from.
    c, o = run("audit-every-metric.py", mutate(memory__peak_rss_mib=50.0))
    expect("a peak below its own steady state FAILS", c, o, True)

    # RED: recovered above peak - it cannot recover to above its own maximum.
    c, o = run("audit-every-metric.py", mutate(memory__recovered_rss_mib=9999.0))
    expect("a recovered value above the peak FAILS", c, o, True)

    # ACCEPT, AND THIS ONE GUARDS AGAINST A GATE COMING BACK RATHER THAN GOING MISSING.
    # audit-every-metric.py used to assert p50 <= p99 across added_latency / added_ttft / added_gap,
    # under a comment claiming they were "pairs from one distribution". They are DIFFERENCES of two
    # distributions' percentiles (gateway leg minus direct leg), and a difference does not inherit
    # monotonicity: a constant gateway overhead under a stretching DIRECT baseline gives a smaller
    # added figure at p99 than at p50 with no percentile anywhere out of order.
    # It fired on the real 2026-07-30 field run - agentgateway anthropic>anthropic, added_gap p50=4us
    # vs p99=3us - and acting on it would have meant "fixing" an engine that was computing correctly.
    # These pin that a legitimate inverted DIFFERENCE is accepted, so the unsound rule cannot return.
    c, o = run("audit-every-metric.py", mutate(stream__added_gap_p50_us=4, stream__added_gap_p99_us=3))
    expect("an added_gap p50 above its p99 is ACCEPTED (a difference, not one distribution)", c, o, False)
    c, o = run("audit-every-metric.py", mutate(perf__added_latency_p50_us=900, perf__added_latency_p99_us=800))
    expect("an added_latency p50 above its p99 is ACCEPTED (same reason)", c, o, False)

    # THE SILENT-PASS GUARD: the tool must actually iterate cells, not print PASS over an empty walk.
    # This is the failure mode that would make every other assertion here vacuous.
    c, o = run("audit-every-metric.py", CLEAN_CELL)
    expect("it reports the cell count it actually examined", c, o, False, "1 served cells")

    print("verify-frontier.py:")
    # ACCEPT: a cell whose six readings each re-derive from its own rungs.
    c, o = run("verify-frontier.py", CLEAN_CELL)
    expect("a frontier that re-derives from its rungs passes", c, o, False)

    # RED: the published rate is not the maximum its own rungs support. This is the check that would
    # have caught the tie-break bug, and it had no test at all until now.
    bad = copy.deepcopy(CLEAN_CELL)
    bad["perf"]["frontier"][-1]["rps"] = 9999.0
    c, o = run("verify-frontier.py", bad)
    expect("a rate that does not re-derive FAILS", c, o, True, "re-derived")

    # RED: the winning CONCURRENCY disagrees with the rungs - the exact shape of the tie-break defect,
    # where the rate was right and the rung it was attributed to was not.
    bad = copy.deepcopy(CLEAN_CELL)
    bad["perf"]["frontier"][-1]["concurrency"] = 32
    c, o = run("verify-frontier.py", bad)
    expect("a concurrency that does not re-derive FAILS", c, o, True)

    # RED: a frontier of a different SHAPE is a different board.
    bad = copy.deepcopy(CLEAN_CELL)
    bad["perf"]["frontier"] = bad["perf"]["frontier"][:3]
    c, o = run("verify-frontier.py", bad)
    expect("a frontier missing declared bounds FAILS", c, o, True)

    print("verify-turnover.py:")
    # ACCEPT, AND FOR THE RIGHT REASON. This fixture used to be CLEAN_CELL, whose only rung above the
    # winner carries `fail: 1` - so verify-turnover classified it CLIFF-BUT-MOOT and NEVER entered the
    # `proved` branch. The test passed, and the tool's central classification - the thing its entire
    # docstring is about - was unexercised: inverting the `best < win_rps` comparison in the tool would
    # have left all 16 fixtures green.
    #
    # So this one carries a genuinely CLEAN, strictly slower rung above the peak, which is what a proved
    # turnover IS, and asserts the tool says so.
    proved = copy.deepcopy(CLEAN_CELL)
    proved["perf"]["sweep_max_proxy"] = [
        {"conc": 8, "ok": 100, "rps": 100.0, "p99_us": 900, "fail": 0},
        {"conc": 16, "ok": 120, "rps": 120.0, "p99_us": 1500, "fail": 0},
        {"conc": 32, "ok": 110, "rps": 110.0, "p99_us": 2500, "fail": 0},
    ]
    c, o = run("verify-turnover.py", proved)
    expect("a proved turnover passes AND is classified PROVED", c, o, False, "PROVED (1)")

    # And the moot case stays distinguishable from it: a failing rung above that was also SLOWER.
    c, o = run("verify-turnover.py", CLEAN_CELL)
    expect("a cliff whose failing rung was slower is MOOT, not proved", c, o, False, "CLIFF-BUT-MOOT (1)")

    # RED: the peak sits at the TOP of the probed ladder and the artifact does NOT disclose it, so a
    # floor is published as though it were a ceiling. This is the tool's only failing exit, and until
    # this round it could not fire at all because `lower_bound` was never read.
    bad = copy.deepcopy(CLEAN_CELL)
    bad["perf"]["sweep_max_proxy"] = [{"conc": 8, "ok": 100, "rps": 100.0, "p99_us": 900, "fail": 0}]
    for r in bad["perf"]["frontier"]:
        r["rps"], r["concurrency"], r["p99_us"], r["lower_bound"] = 100.0, 8, 900, False
        r["first_disqualified_conc"] = None
    c, o = run("verify-turnover.py", bad)
    expect("an UNDISCLOSED floor FAILS", c, o, True, "does NOT disclose")

    # ACCEPT: the same shape, but the artifact discloses it. Not a defect - and this is the half that
    # was broken in the opposite direction, failing honest boards.
    good = copy.deepcopy(bad)
    for r in good["perf"]["frontier"]:
        r["lower_bound"] = True
    c, o = run("verify-turnover.py", good)
    expect("a DISCLOSED floor passes", c, o, False, "DISCLOSED FLOOR")

    print()
    if FAILURES:
        for f in FAILURES:
            print("FAILURE: " + f)
        print(f"\n{len(FAILURES)} failure(s)")
        return 1
    print("PASS: all four auditors reject each violation they exist to catch, and accept a clean board.")
    return 0


# ---- run-on-ec2.sh: the provision payload is SINGLE-QUOTED, so it cannot contain an apostrophe ----
# This cost a full 14-box launch on 2026-07-30. The provisioning block is passed as
# `ssh host 'set -e ... '`, so the FIRST apostrophe inside it closes the string - and everything
# after runs on the ORCHESTRATOR instead of the box. The symptom is maximally misleading: the error
# reads "./run-on-ec2.sh: line 907: sudoq: command not found", naming a local line number for a
# helper that is defined and correct on the remote side, while every box reports PROVISION FAILED.
# Parity does not save it: an even number of apostrophes re-closes the string but still executes the
# span between them locally. The only safe rule is ZERO apostrophes in the payload, including prose
# in comments - which is exactly where all three came from ("the gateway's", "run.sh's", "else's").
def _test_provision_payload_has_no_apostrophe():
    import re
    src = open(os.path.join(HERE, "run-on-ec2.sh"), encoding="utf-8").read().split("\n")
    start = next((i for i, l in enumerate(src) if "ssh $SSHOPT ubuntu@" in l and l.rstrip().endswith("'set -e")), None)
    assert start is not None, "could not find the provision ssh payload opener"
    end = next((i for i in range(start + 1, len(src)) if re.match(r"^\s*.*'\s*>>\"\$glog\"", src[i])), None)
    assert end is not None, "could not find the provision ssh payload terminator"
    bad = [(i + 1, src[i]) for i in range(start + 1, end) if "'" in src[i]]
    return bad


_bad_quotes = _test_provision_payload_has_no_apostrophe()
if _bad_quotes:
    for _ln, _txt in _bad_quotes:
        print(f"FAIL - run-on-ec2.sh:{_ln} apostrophe inside the single-quoted ssh payload: {_txt.strip()[:90]}")
    print("FAIL - the provision payload must contain ZERO apostrophes (see comment above this check)")
    sys.exit(1)
print("ok   - run-on-ec2.sh provision payload contains no apostrophe (would execute locally)")


if __name__ == "__main__":
    sys.exit(main())
