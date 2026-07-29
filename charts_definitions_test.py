#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# EVERY CHART DEFINITION, HELD TO THE SAME RULES AT ONCE.
#
# charts_test.py proves each honesty gate on the lane where its defect happened. What nothing held
# until now is the CLASS: a chart added to CHARTS tomorrow gets the same eligibility discipline as
# the thirteen that exist today, or these tests go red. Each check below runs over every Chart in
# CHARTS, driven through the real _topn_keys with synthetic rows, so "the served gate fires", "a
# null is never a served zero on a null_not_served chart", and "the ranking direction matches
# higher_better" are properties of the whole board rather than of the lanes someone remembered.
#
#   python3 charts_definitions_test.py
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
_DATA = os.path.join(HERE, "site", "data.json")

_created_data = False
if not os.path.exists(_DATA):
    os.makedirs(os.path.dirname(_DATA), exist_ok=True)
    with open(_DATA, "w") as f:
        json.dump({"gateways": []}, f)  # minimal valid canonical bundle so _canonical() imports
    _created_data = True

sys.path.insert(0, HERE)
try:
    import charts  # noqa: E402
finally:
    if _created_data:
        os.remove(_DATA)

_fail = 0


def check(name, got, want):
    global _fail
    if got == want:
        print(f"ok   - {name}")
    else:
        print(f"FAIL - {name}: got {got!r}, want {want!r}")
        _fail = 1


import contextlib


@contextlib.contextmanager
def isolated(section):
    """PER-CHECK / PER-CHART EXCEPTION ISOLATION (round-2 audit finding). Every check below is
    written `check(name, some_charts_function(...), want)`, which evaluates the charts.py call
    BEFORE check() is entered - so a mutation that makes that function raise (instead of returning
    a wrong value) used to abort this whole file at that line, silently skipping every chart and
    every section after it with no FAIL printed for any of them. That reads as "ran clean" from the
    exit code and the last line printed, which is the opposite of what a suite that gates a publish
    owes a reader.

    Wrapping each chart's checks (and each top-level section) in this context manager makes a raise
    a recorded FAILURE for that one chart/section, not a dead script - the same discipline
    `site/test.mjs`'s runner documents for the same reason: "ordering must not decide coverage"
    (test.mjs:35-39). Every chart and section after the one that raised still runs.
    """
    global _fail
    try:
        yield
    except Exception as e:  # noqa: BLE001 - a check raising IS the finding, not a bug in the test
        _fail = 1
        print(f"FAIL - [{section}] raised {e!r} instead of completing - isolated, run continues")


# ── structural: the definitions themselves ───────────────────────────────────────────────────────

with isolated("structural: board-level"):
    names = [c.name for c in charts.CHARTS]
    check("every chart has a unique name (a duplicate overwrites a PNG silently)",
          len(names), len(set(names)))
    check("the board is not empty", len(charts.CHARTS) > 0, True)

for c in charts.CHARTS:
    with isolated(f"structural: {c.name}"):
        check(f"{c.name}: has at least one series", len(c.series) > 0, True)
        check(f"{c.name}: the primary series names a real field", bool(c.series and c.series[0].field), True)
        for s in c.series:
            check(f"{c.name}/{s.field}: the legend labels the series", bool(s.legend), True)
            # A chart with two bars per gateway spends colour on implementation language, so colour
            # cannot also say which bar is which: without a tag beside each number the two bars are
            # indistinguishable to anyone who does not read charts.py.
            if len(c.series) > 1:
                check(f"{c.name}/{s.field}: a multi-series chart tags each bar with its metric",
                      bool(s.tag), True)
        check(f"{c.name}: a not-served row has a label (a blank label is a bar with no story)",
              bool(c.not_served_text), True)
        check(f"{c.name}: a served zero has a label", bool(c.zero_text), True)
        check(f"{c.name}: subtitle is prose or a renderer",
              isinstance(c.subtitle, str) or callable(c.subtitle), True)
        check(f"{c.name}: annot is a callable or absent",
              c.annot is None or callable(c.annot), True)
        # THE MEDIUM-R3-3 CLASS, held for every future chart: on a null_not_served chart the label a
        # null row gets must never be the served-zero sentence, or an unmeasured metric is captioned
        # with a fabricated explanation (the added_latency defect, generalised).
        if c.null_not_served:
            effective = c.not_measured_text or c.not_served_text
            check(f"{c.name}: a null row's label exists and is not the served-zero sentence",
                  bool(effective) and effective != c.zero_text, True)

# ── behavioural: the eligibility gates, for EVERY chart, through the real _topn_keys ─────────────

_orig_load = charts._load


def _with_rows(rows, fn):
    charts._load = lambda suite: [dict(r) for r in rows]
    try:
        return fn()
    finally:
        charts._load = _orig_load


for c in charts.CHARTS:
    with isolated(f"behavioural (_topn_keys): {c.name}"):
        field = c.series[0].field
        rows = [
            {"_key": "valid", field: 100.0, c.served_field: True},
            {"_key": "unserved", field: 200.0, c.served_field: False},
            {"_key": "nullrow", field: None, c.served_field: True},
            {"_key": "zero", field: 0.0, c.served_field: True},
        ]
        topn = _with_rows(rows, lambda: charts._topn_keys(c, n=10))

        check(f"{c.name}: a valid served value is eligible", "valid" in topn, True)
        check(f"{c.name}: the served gate ({c.served_field}) excludes an unserved row even at a winning raw value",
              "unserved" in topn, False)
        if c.null_not_served:
            check(f"{c.name}: a NULL primary metric is unmeasured, never a served 0 that ranks",
                  "nullrow" in topn, False)
        else:
            # Without the flag a null coerces to 0 and is eligible exactly when a served 0 is. This is
            # the documented behaviour; a chart that acquires a nullable metric must set the flag, and
            # this line is where the difference stays visible.
            check(f"{c.name}: a null coerces to 0 and follows the zero_ok rule ({c.zero_ok})",
                  "nullrow" in topn, c.zero_ok)
        check(f"{c.name}: a measured 0 is eligible iff zero_ok ({c.zero_ok})",
              "zero" in topn, c.zero_ok)

        # Ranking direction: with two clean rows the winner must sit at the end higher_better names.
        pair = [
            {"_key": "hi", field: 100.0, c.served_field: True},
            {"_key": "lo", field: 50.0, c.served_field: True},
        ]
        top1 = _with_rows(pair, lambda: charts._topn_keys(c, n=1))
        want = {"hi"} if c.higher_better else {"lo"}
        check(f"{c.name}: top-1 respects higher_better={c.higher_better}", top1, want)

        # n truncates: the ranking can never return more keys than asked for.
        both = _with_rows(pair, lambda: charts._topn_keys(c, n=2))
        check(f"{c.name}: n bounds the ranking", (len(top1), len(both)), (1, 2))

# ── every declared suite is loadable: a chart naming a suite with no projection path would only ──
# fail at render time on a box mid-run. With an empty canonical bundle every suite must load to an
# empty list, not raise.
charts.CANON = {}
charts.GATEWAYS = {}
for suite in sorted({c.suite for c in charts.CHARTS}):
    with isolated(f"suite loadable: {suite}"):
        try:
            rows = charts._load(suite)
            check(f"suite {suite}: loads (empty board -> no rows, no crash)", rows, [])
        except Exception as e:  # noqa: BLE001 - the finding IS the exception
            check(f"suite {suite}: loads without raising", repr(e), "no exception")

# ── _fmt: the bar label formatter's documented boundaries ────────────────────────────────────────
with isolated("_fmt boundaries"):
    check("_fmt: sub-10 keeps a decimal", charts._fmt(9.5), "9.5")
    check("_fmt: 10..999 is integral", charts._fmt(999.4), "999")
    check("_fmt: 1000 turns k with a decimal", charts._fmt(1000), "1.0k")
    check("_fmt: 100k drops the decimal", charts._fmt(100000), "100k")
    check("_fmt: zero is 0, never blank", charts._fmt(0), "0")
    check("_fmt: a negative clamps to 0 (bars have no negative length)", charts._fmt(-3), "0")

if _fail == 0:
    print("all charts.py definition-class tests passed")
    sys.exit(0)
print("CHARTS DEFINITION TESTS FAILED")
sys.exit(1)
