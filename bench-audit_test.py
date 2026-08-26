#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# EVERY AUDIT CHECK MUST BE ABLE TO FAIL.
#
# A check that can't fire guards nothing: bench-audit's first draft had one that read
# `cell["sweep_max_proxy"]` where the sweep actually lives at `cell["perf"]["sweep_max_proxy"]`, so it
# returned early and reported PASS on data that violated it everywhere. So each check here gets a cell
# it MUST reject and a cell it MUST accept.
#
#   python3 bench-audit_test.py
import contextlib
import importlib.util
import json
import os
import sys
import tempfile

# Same loader the dashboard's own test uses: the module's filename is hyphenated, so it cannot be
# imported by name.
HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location("bench_audit", os.path.join(HERE, "bench-audit.py"))
audit = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(audit)


# A rung ladder built so every one of the six frontier readings is a distinct, checkable answer: each
# doubling of concurrency both raises the rate and pushes the tail into the next bound. `ok` is set
# positive on every rung (value arbitrary; only `ok > 0` matters) so every rung is provably clean
# under `rung_served_cleanly`, not merely clean by `fail: 0`.
#
#   conc   rps    p99_us   qualifies under (strict <)
#   8      500      800    1ms, 5ms, 10ms, 50ms, 100ms, unbounded
#   32    1500     3000          5ms, 10ms, 50ms, 100ms, unbounded
#   128   3000     8000                10ms, 50ms, 100ms, unbounded
#   512   5000    40000                      50ms, 100ms, unbounded
#   2048  6000    90000                            100ms, unbounded
#   8192  6500   200000                                    unbounded
_BASE_RUNGS = [
    {"conc": 8, "rps": 500, "p99_us": 800, "fail": 0, "ok": 500},
    {"conc": 32, "rps": 1_500, "p99_us": 3_000, "fail": 0, "ok": 1_500},
    {"conc": 128, "rps": 3_000, "p99_us": 8_000, "fail": 0, "ok": 3_000},
    {"conc": 512, "rps": 5_000, "p99_us": 40_000, "fail": 0, "ok": 5_000},
    {"conc": 2_048, "rps": 6_000, "p99_us": 90_000, "fail": 0, "ok": 6_000},
    {"conc": 8_192, "rps": 6_500, "p99_us": 200_000, "fail": 0, "ok": 6_500},
]

# The frontier re-derived from `_BASE_RUNGS` by hand, per `frontier::read_at`. Only the unbounded
# reading wins at the ladder's top rung, so it is the only one with `lower_bound: True`.
_BASE_FRONTIER = [
    {"p99_bound_us": 1_000, "rps": 500, "concurrency": 8, "p99_us": 800,
     "first_disqualified_conc": 32, "lower_bound": False},
    {"p99_bound_us": 5_000, "rps": 1_500, "concurrency": 32, "p99_us": 3_000,
     "first_disqualified_conc": 128, "lower_bound": False},
    {"p99_bound_us": 10_000, "rps": 3_000, "concurrency": 128, "p99_us": 8_000,
     "first_disqualified_conc": 512, "lower_bound": False},
    {"p99_bound_us": 50_000, "rps": 5_000, "concurrency": 512, "p99_us": 40_000,
     "first_disqualified_conc": 2_048, "lower_bound": False},
    {"p99_bound_us": 100_000, "rps": 6_000, "concurrency": 2_048, "p99_us": 90_000,
     "first_disqualified_conc": 8_192, "lower_bound": False},
    {"p99_bound_us": None, "rps": 6_500, "concurrency": 8_192, "p99_us": 200_000,
     "first_disqualified_conc": None, "lower_bound": True},
]


def cell(**over):
    """A cell that violates nothing, as the baseline every check is proven against."""
    c = {
        "served": True,
        "perf": {
            "sweep_max_proxy": json.loads(json.dumps(_BASE_RUNGS)),
            "frontier": json.loads(json.dumps(_BASE_FRONTIER)),
        },
        "stream": {
            "added_ttft_p50_us": 100,
            "added_ttft_p99_us": 400,
            "streams_sustained": 128,
        },
    }
    for k, v in over.items():
        section, _, field = k.partition("__")
        if field:
            c[section][field] = v
        else:
            c[section] = v
    return c


def frontier_with(i, **field_over):
    """A deep copy of `_BASE_FRONTIER` with reading `i` mutated - a red fixture that trips exactly one
    frontier check without disturbing the readings around it."""
    fr = json.loads(json.dumps(_BASE_FRONTIER))
    fr[i].update(field_over)
    return fr


# Each entry: the check, a cell it must REJECT, and what the violation is about.
#
# REMOVED: check_sustained_not_above_peak and check_peak_came_from_its_own_sweep compared the deleted
# `rps_sustained_20ms`/`rps_max_proxy` scalars. Their successors (tested below) are strictly stronger:
# check_frontier_rates_are_monotone_in_the_bound asserts ordering across all six readings, and
# check_frontier_is_rederivable_from_its_sweep recomputes every reading from the rungs both ways.

# ── traces, for the SEARCH-TRACE checks ───────────────────────────────────────────────────────────
#
# `cell()` carries no `sweep_streams`, so every trace check returns early on it - a valid but weak
# accept fixture. These build real ladders so both sides are proven against a trace the check reads.
def rung(conc, passed=True, **over):
    r = {"conc": conc, "passed": passed, "streams": conc, "stream_errors": 0 if passed else conc,
         "frames": 64 * conc, "frames_expected": 64 * conc, "stalls": 0}
    r.update(over)
    return r


def trace_cell(rungs, sustained=128, absence_detail=None):
    """A served streaming cell whose search left `rungs` behind."""
    c = cell()
    c["stream"] = {"stream_served": True, "streams_sustained": sustained, "sweep_streams": rungs}
    if absence_detail is not None:
        c["absences"] = {"stream.streams_sustained": {"reason": "not_measured", "detail": absence_detail}}
    return c


# A ladder that climbs cleanly and stops at a confirmed rung - the shape every trace check must accept.
CLEAN_TRACE = [rung(1), rung(2), rung(4), rung(8), rung(16, passed=False),
               rung(12), rung(12), rung(12)]

REJECTS = [
    # ── the SEARCH TRACE's own reject cases ───────────────────────────────────────────────────────
    #
    # Every other check reads the published NUMBER; a number never produced has nothing to check.
    # These five read what the search actually DID, catching a cell that silently gave up.
    (audit.check_search_spent_its_budget,
     trace_cell([rung(1), rung(2), rung(4), rung(8), rung(16, passed=False), rung(15, passed=False)],
                sustained=None, absence_detail="the stepped-down rung failed its first window"),
     "a search that published nothing but stopped at c=15 having already carried c=8, probing "
     "nothing in between - plano anthropic>anthropic"),
    (audit.check_no_rung_fails_below_one_already_carried,
     trace_cell([rung(1), rung(2), rung(4), rung(8), rung(16, passed=False), rung(6, passed=False)],
                sustained=None, absence_detail="it did not hold on re-measurement"),
     "a rung failing BELOW one the same cell already carried, with the absence naming nothing else "
     "that changed - impossible for the gateway alone"),
    (audit.check_published_rung_held_a_majority,
     trace_cell([rung(1), rung(2), rung(4), rung(4, passed=False), rung(4, passed=False)], sustained=4),
     "a published rung that LOST its own windows - the summary disagreeing with its evidence"),
    (audit.check_a_wedged_gateway_is_named_as_one,
     trace_cell([rung(1), rung(2), rung(4), rung(8, passed=False), rung(6, passed=False),
                 rung(5, passed=False), rung(4, passed=False), rung(3, passed=False)],
                sustained=None, absence_detail="the bisection did not hold"),
     "a gateway that stopped serving and never resumed, published as silence - aisix"),
    (audit.check_no_rig_side_error_is_charged_to_the_gateway,
     trace_cell([rung(1), rung(2, passed=False, stream_errors_connect_failed=2)], sustained=None,
                absence_detail="x"),
     "connections THIS HOST could not make, counted against the gateway"),
    (audit.check_sweep_carries_its_latency,
     cell(perf__sweep_max_proxy=[{"conc": 64, "rps": 10_000, "p99_us": None, "fail": None}]),
     "throughput windows published without the p99 they measured"),
    (audit.check_no_bare_absence, cell(stream__added_gap_p50_us=None),
     "a null metric with no reason in absences (a bare hole)"),
    # The other half of the hole: `check_no_bare_absence` reads `f in blk and blk[f] is None`, so a
    # field DROPPED from the block is invisible to it. cell() already omits most declared fields, so it
    # rejects here while the accept-side pairs below supply a fully-carried cell.
    (audit.check_declared_fields_are_carried, cell(),
     "a served cell whose blocks omit declared fields entirely"),
    (audit.check_stream_capacity_is_a_number,
     cell(stream__stream_served=True, stream__streams_sustained=None),
     "a served streaming cell whose capacity metric is a hole with nothing explaining it"),
    # REMOVED: check_frames_have_a_stream_behind_them watched for a frames/sec rate over zero streams.
    # `cpu_fps`/`cpu_fps_concurrency`/`sweep_cpu_fps` are deleted from `CellStream`; the replacement
    # `streams_sustained_fps` is the frame rate OF `streams_sustained`, measured in the same window, so
    # it can no longer be a rate over an unmeasured population.
    # ── the frontier's own REJECTS entries ────────────────────────────────────────────────────────
    #
    # check_frontier_is_complete and check_every_absent_frontier_reading_has_a_reason's rejects live in
    # dedicated blocks below (they need more shapes exercised). The five here each prove one failure mode.
    (audit.check_frontier_rates_are_monotone_in_the_bound,
     cell(perf__frontier=frontier_with(1, rps=100)),  # 5ms now reads 100, BELOW 1ms's own 500
     "a looser bound reading a lower rate than a tighter one"),
    (audit.check_frontier_is_rederivable_from_its_sweep,
     cell(perf__frontier=frontier_with(2, rps=3_037)),  # +37 off the 3,000 its qualifying rungs carry
     "a reading whose rate its own rungs do not carry"),
    (audit.check_frontier_disclosure_agrees_with_the_ladder,
     cell(perf__frontier=frontier_with(5, lower_bound=False)),  # winner IS the top rung; flipped false
     "lower_bound flipped false at the reading whose winner IS the top of the ladder"),
    (audit.check_frontier_p99_is_the_observed_tail,
     cell(perf__frontier=frontier_with(2, p99_us=15_000)),  # above its own 10ms bound
     "a reading's p99 published above its own bound"),
    (audit.check_frontier_rate_is_physically_possible,
     cell(perf__frontier=frontier_with(5, rps=5_000_000, concurrency=1)),
     "5,000,000 rps on a single connection"),
]


@contextlib.contextmanager
def isolated(failures, section):
    """PER-BLOCK EXCEPTION ISOLATION (round-2 audit finding).

    A check that raises (rather than returning violations) would otherwise abort the whole file at
    that line, taking every later block down uncounted. This records the raise as a failure for THAT
    block only so every subsequent block still runs - "ordering must not decide coverage"
    (site/test.mjs:35-39).
    """
    try:
        yield
    except Exception as e:  # noqa: BLE001 - a check raising IS the finding, not a bug in the test
        failures.append(f"[{section}] RAISED instead of completing: {e!r} - isolated, run continues")


def main():
    failures = []

    for check, bad, what in REJECTS:
        with isolated(failures, f"REJECTS reject-side: {check.__name__}"):
            got = list(check("t", bad))
            if not got:
                failures.append(f"{check.__name__} accepted {what} - it cannot fail, so it guards nothing")

    # The other half: a check that rejects everything is equally useless. cell() cannot satisfy
    # check_declared_fields_are_carried (its subject is the FULL field list, cell() carries only a few),
    # so that one's accept side is proven below against a cell built from ABSENCE_CARRYING_FIELDS itself.
    clean = cell()
    for check, _bad, _what in REJECTS:
        if check is audit.check_declared_fields_are_carried:
            continue
        with isolated(failures, f"REJECTS accept-side: {check.__name__}"):
            got = list(check("t", clean))
            if got:
                failures.append(f"{check.__name__} rejected a clean cell: {got}")

    # The other side of the two new definition-of-done checks: an absence WITH its reason, and a
    # measured zero, must both be accepted - the checks forbid bare holes, not honest absences.
    with isolated(failures, "definition-of-done: absence-with-reason / measured-zero accept side"):
        with_reason = cell(stream__added_gap_p50_us=None,
                           absences={"stream.added_gap_p50_us": {"reason": "below_resolution", "detail": "x"}})
        if list(audit.check_no_bare_absence("t", with_reason)):
            failures.append("check_no_bare_absence rejected a null that carries its reason")
        # `cpu_fps` is gone from check_stream_capacity_is_a_number (deleted from `CellStream`); its
        # field list is just `("streams_sustained",)` now, so these fixtures are about that one field.
        zeroed = cell(stream__stream_served=True, stream__streams_sustained=0)
        if list(audit.check_stream_capacity_is_a_number("t", zeroed)):
            failures.append("check_stream_capacity_is_a_number rejected a measured 0")
        excused = cell(stream__stream_served=True, stream__streams_sustained=None,
                       absences={"stream.streams_sustained": {"reason": "untestable"}})
        if list(audit.check_stream_capacity_is_a_number("t", excused)):
            failures.append("check_stream_capacity_is_a_number rejected a rig-class absence")
        # A search that RAN, failed to establish a ceiling, and said WHY is not a silent yield;
        # demanding a measured 0 there fabricates an answer to a question that was never settled.
        explained = cell(stream__stream_served=True, stream__streams_sustained=None,
                         absences={"stream.streams_sustained": {
                             "reason": "not_measured",
                             "detail": "the bisection proved c=6144, but that concurrency did not "
                                       "hold the stream gate on re-measurement and stepping down "
                                       "found none that did within 4 attempts"}})
        if list(audit.check_stream_capacity_is_a_number("t", explained)):
            failures.append("check_stream_capacity_is_a_number rejected an absence that explains itself")

    # ── the omitted-field check, both ways (ledger TOOL-04) ───────────────────────────────────────
    #
    # ACCEPT: a cell carrying every declared field passes, number or explicit null. Generated FROM
    # ABSENCE_CARRYING_FIELDS so it tracks the list the check enforces instead of pinning a schema snapshot.
    carried = {"served": True}
    with isolated(failures, "omitted-field check (ledger TOOL-04)"):
        for _b, _fs in audit.ABSENCE_CARRYING_FIELDS.items():
            carried[_b] = {_f: 1.0 for _f in _fs}
        if list(audit.check_declared_fields_are_carried("t", carried)):
            failures.append("check_declared_fields_are_carried rejected a cell that carries every field")
        # A field to null out / drop, picked off the LIVE list rather than hardcoded: a hardcoded name
        # (this used to name `perf.conc_at_peak`) gets deleted out from under the fixture and it raises
        # KeyError instead of testing anything.
        _perf_field = audit.ABSENCE_CARRYING_FIELDS["perf"][0]
        carried_with_null = json.loads(json.dumps(carried))
        carried_with_null["perf"][_perf_field] = None
        if list(audit.check_declared_fields_are_carried("t", carried_with_null)):
            failures.append("check_declared_fields_are_carried rejected an explicit null - it polices "
                            "OMISSION, and a null-with-reason is the honest shape it exists to require")

        # REJECT, precisely: dropping ONE key must yield exactly one violation naming that key - pins
        # that the check sees the missing KEY, not merely a small block.
        one_short = json.loads(json.dumps(carried))
        del one_short["perf"][_perf_field]
        fired = list(audit.check_declared_fields_are_carried("t", one_short))
        if len(fired) != 1 or f"perf.{_perf_field}" not in fired[0]:
            failures.append(f"check_declared_fields_are_carried must name the one omitted key, got {fired!r}")

        # REJECT: a whole block deleted is the same claim with the evidence removed, not a quieter cell.
        no_block = json.loads(json.dumps(carried))
        del no_block["memory"]
        fired = list(audit.check_declared_fields_are_carried("t", no_block))
        if len(fired) != 1 or "NO memory block" not in fired[0]:
            failures.append(f"check_declared_fields_are_carried must reject a served cell with no memory "
                            f"block at all, got {fired!r}")

    # ── check_frontier_is_complete: shape and ordering, both ways ─────────────────────────────────
    #
    # ACCEPT: the clean cell's frontier is all six readings, declared bounds ascending then unbounded,
    # every field present - whether or not the caller claims the producer knew about the metric.
    with isolated(failures, "frontier: check_frontier_is_complete accept side"):
        if list(audit.check_frontier_is_complete("t", cell())):
            failures.append("check_frontier_is_complete rejected a complete, correctly-ordered frontier")
        if list(audit.check_frontier_is_complete("t", cell(), frontier_known=True)):
            failures.append("check_frontier_is_complete rejected a complete frontier with "
                            "frontier_known=True")

        # ACCEPT (the "Also" case in the brief): a snapshot that PREDATES the metric publishes no
        # frontier anywhere, and that must read as "nothing to audit here" rather than a violation -
        # frontier_known False is how the caller tells this apart from a cell that dropped it.
        no_metric = cell(perf__frontier=[])
        if list(audit.check_frontier_is_complete("t", no_metric, frontier_known=False)):
            failures.append("check_frontier_is_complete rejected a cell from a pre-frontier snapshot "
                            "(frontier_known=False) - demanding the metric from an engine that never "
                            "had it")

        # RED: the other half of the same case - SOME cell in this snapshot knows the metric, so a
        # cell with no frontier at all dropped it rather than never measuring it.
        fired = list(audit.check_frontier_is_complete("t", no_metric, frontier_known=True))
        if not fired:
            failures.append("check_frontier_is_complete accepted a served cell with NO frontier "
                            "while sibling cells in the same snapshot publish one - it cannot fail, "
                            "so it guards nothing")

        # ── WITHHELD IS NOT DROPPED ───────────────────────────────────────────────────────────────
        #
        # When egress re-verification proves a served cell never translated, the engine withholds the
        # whole perf group and publishes `egress_reverified: false`. An empty frontier is the CORRECT
        # artifact there - demanding one would demand a throughput figure for a translation that never
        # happened - even though the cell otherwise looks exactly like the dropped-frontier RED case.
        withheld = cell(perf__frontier=[], perf__egress_reverified=False)
        if list(audit.check_frontier_is_complete("t", withheld, frontier_known=True)):
            failures.append("check_frontier_is_complete flagged a cell whose perf was WITHHELD because "
                            "egress re-verification proved the gateway did not translate - an empty "
                            "frontier is correct there, and calling it a drop cries wolf on the one "
                            "case where the harness behaved best")

        # RED (the half that matters): the exemption is keyed on the DISCLOSURE, not trust in the
        # producer. A cell that merely lost its frontier has no `egress_reverified: false` and must
        # still fail, in both the reverified-true and field-absent shapes.
        if not list(audit.check_frontier_is_complete(
                "t", cell(perf__frontier=[], perf__egress_reverified=True), frontier_known=True)):
            failures.append("the withheld-cell exemption swallowed a REAL dropped frontier on a cell "
                            "that re-verified TRUE - it must key on the disclosure, not on the field "
                            "merely being present")

        # RED: and the exemption covers an EMPTY frontier only. A withheld cell that publishes a
        # malformed one is still a malformed frontier.
        if not list(audit.check_frontier_is_complete(
                "t", cell(perf__frontier=[{"p99_bound_us": 1000}], perf__egress_reverified=False),
                frontier_known=True)):
            failures.append("the withheld-cell exemption swallowed a MALFORMED frontier - it must "
                            "cover an absent frontier, not excuse a wrong one")

        # RED: drop one reading (5, not 6). Nothing else in this file would notice a shrunk board.
        five = json.loads(json.dumps(_BASE_FRONTIER))[:5]
        fired = list(audit.check_frontier_is_complete("t", cell(perf__frontier=five)))
        if not fired:
            failures.append("check_frontier_is_complete accepted a frontier with only 5 of 6 readings")

        # RED: omit the `lower_bound` key entirely (present-but-null is a different, legal shape for
        # the OTHER fields, but every key must at least be present - see check_frontier_is_complete's
        # own docstring on key-missing vs. measured).
        no_lb = json.loads(json.dumps(_BASE_FRONTIER))
        del no_lb[0]["lower_bound"]
        fired = list(audit.check_frontier_is_complete("t", cell(perf__frontier=no_lb)))
        if not any("OMITS `lower_bound`" in v for v in fired):
            failures.append(f"check_frontier_is_complete must name a reading that omits "
                            f"`lower_bound`, got {fired!r}")

        # RED: reorder two readings. The order IS the invariant (bounds ascending, unbounded last), so
        # a permutation must trip completeness - AND, because the two checks share one sequence,
        # monotonicity too: swapping 1ms and 5ms puts a 1500-rps reading before a 500-rps one.
        reordered = json.loads(json.dumps(_BASE_FRONTIER))
        reordered[0], reordered[1] = reordered[1], reordered[0]
        reordered_cell = cell(perf__frontier=reordered)
        if not list(audit.check_frontier_is_complete("t", reordered_cell)):
            failures.append("check_frontier_is_complete accepted a frontier with two readings swapped")
        if not list(audit.check_frontier_rates_are_monotone_in_the_bound("t", reordered_cell)):
            failures.append("check_frontier_rates_are_monotone_in_the_bound accepted a frontier with "
                            "two readings swapped - reordering put a lower rate ahead of a higher one")

    # ── check_frontier_rates_are_monotone_in_the_bound: the degenerate form ───────────────────────
    #
    # A looser bound with NO rate beside a tighter one that has a rate is the same violation: the
    # looser's qualifying set contains the tighter's, so it cannot be empty when the tighter's was not.
    with isolated(failures, "frontier: monotonicity degenerate form (absent looser, present tighter)"):
        absent_looser = frontier_with(1, rps=None, concurrency=None, p99_us=None,
                                       first_disqualified_conc=None)
        fired = list(audit.check_frontier_rates_are_monotone_in_the_bound(
            "t", cell(perf__frontier=absent_looser)))
        if not any("published NO rate while the tighter" in v for v in fired):
            failures.append(f"check_frontier_rates_are_monotone_in_the_bound must catch a looser "
                            f"reading gone absent beside a tighter one that read, got {fired!r}")

    # ── check_frontier_is_rederivable_from_its_sweep: the re-derivation must check EVERY field ─────
    #
    # The REJECTS entry proves a wrong RATE is caught; these two prove the concurrency and the boundary
    # proof are independently re-derived, not trusted once the rate matches.
    with isolated(failures, "frontier: re-derivation catches bad concurrency / bad boundary proof"):
        bad_conc = frontier_with(2, concurrency=99_999)  # no rung at c=99999 carries the 10ms rate
        fired = list(audit.check_frontier_is_rederivable_from_its_sweep(
            "t", cell(perf__frontier=bad_conc)))
        if not any("no rung at that concurrency" in v for v in fired):
            failures.append(f"check_frontier_is_rederivable_from_its_sweep must catch a concurrency "
                            f"no qualifying rung carries, got {fired!r}")

        bad_fd = frontier_with(0, first_disqualified_conc=999_999)
        fired = list(audit.check_frontier_is_rederivable_from_its_sweep(
            "t", cell(perf__frontier=bad_fd)))
        if not any("first_disqualified_conc=999999" in v for v in fired):
            failures.append(f"check_frontier_is_rederivable_from_its_sweep must catch a fabricated "
                            f"first_disqualified_conc, got {fired!r}")

    # ── rung_served_cleanly: exact now, `ok` is read directly off the rung ───────────────────────────
    #
    # `ok > 0` guards against "0 of 0 looks clean": `fail == 0` alone can't tell a window that completed
    # nothing from one that completed everything it accepted. Unverifiable before `ok` was published.
    with isolated(failures, "rung_served_cleanly: exact ok/fail rule"):
        # ACCEPT: the ordinary clean rung - ok > 0, fail == 0.
        if not audit.rung_served_cleanly({"conc": 8, "rps": 500, "p99_us": 800, "fail": 0, "ok": 500}):
            failures.append("rung_served_cleanly rejected an ordinary clean rung (ok=500, fail=0)")
        # RED: `ok: 0` alongside `fail: 0` - a window that completed NOTHING. The ambiguity `ok > 0`
        # exists to break, unreachable before `ok` was published.
        if audit.rung_served_cleanly({"conc": 999, "rps": None, "p99_us": None, "fail": 0, "ok": 0}):
            failures.append("rung_served_cleanly accepted ok=0, fail=0 as clean - a window that "
                            "completed nothing must not count as having served cleanly")
        # GREEN: `ok` absent entirely (snapshots predating the field) FALLS BACK to a positive rate or
        # a p99 as proof the window completed something. Wider than the engine's `ok > 0` (a completion
        # always leaves a latency sample), so its residual error is a missed catch, not a false alarm.
        # Refusing instead would force the re-derivation to skip whole boards, weaker than approximating.
        if not audit.rung_served_cleanly({"conc": 8, "rps": 500, "p99_us": 800, "fail": 0}):
            failures.append("rung_served_cleanly refused a rung with no `ok` but a real rate and tail - "
                            "it must fall back rather than refuse, or the re-derivation skips whole "
                            "pre-`ok` boards and the strongest check goes unrun on what ships")
        # AND THE FALLBACK STILL HAS A FLOOR: no `ok`, no rate, no tail proves nothing at all.
        if audit.rung_served_cleanly({"conc": 8, "rps": None, "p99_us": None, "fail": 0}):
            failures.append("rung_served_cleanly accepted a rung with no ok, no rate and no tail - "
                            "nothing there evidences a completion, so the fallback must still refuse")
        # RED, the sibling: `fail` absent with `ok` present must not read as clean either.
        if audit.rung_served_cleanly({"conc": 8, "rps": 500, "p99_us": 800, "ok": 500}):
            failures.append("rung_served_cleanly accepted a rung with no `fail` field as clean")

    # ── the ok:0/fail:0 and ok-absent defects must not let a rung WIN a reading ──────────────────────
    #
    # The real bar is that a rung which can't prove it served cleanly must not be the rung a frontier
    # reading is built on. Rung 2 (c=128) wins the 10ms reading; knocking out its cleanliness must drop
    # the winner to rung 1 (c=32, 1,500 rps), leaving the published 3,000 unbacked.
    with isolated(failures, "ok:0/fail:0 and ok-absent rungs must not win a frontier reading"):
        ok_zero_rungs = json.loads(json.dumps(_BASE_RUNGS))
        ok_zero_rungs[2]["ok"] = 0  # was 3_000; the rung still publishes fail: 0 and a real rate/p99
        fired = list(audit.check_frontier_is_rederivable_from_its_sweep(
            "t", cell(perf__sweep_max_proxy=ok_zero_rungs)))
        if not any("is 1500" in v for v in fired):
            failures.append(f"check_frontier_is_rederivable_from_its_sweep let a rung with ok=0, "
                            f"fail=0 win the 10ms reading - re-derived from the rungs that can PROVE "
                            f"they served cleanly the answer is 1500, got {fired!r}")

        # The absent-`ok` sibling: with the fallback, a rung missing `ok` but carrying a real rate and
        # tail is proven clean by that rate, so it legitimately KEEPS the reading - asserting silence
        # here proves the fallback re-derives rather than merely declining to look.
        ok_absent_rungs = json.loads(json.dumps(_BASE_RUNGS))
        del ok_absent_rungs[2]["ok"]
        fired = list(audit.check_frontier_is_rederivable_from_its_sweep(
            "t", cell(perf__sweep_max_proxy=ok_absent_rungs)))
        if fired:
            failures.append(f"a rung with no `ok` but a real rate and tail must keep its reading through "
                            f"the fallback, not be treated as unproven: got {fired!r}")

    # ── a snapshot that predates `ok` is RE-DERIVED ANYWAY, via the wider fallback ────────────────────
    #
    # `ok_known=False` no longer skips the re-derivation: skipping meant the strongest check in the file
    # went unrun on a whole board, weaker than approximating. With `rung_served_cleanly`'s fallback
    # (rate-or-p99 as proof of completion), a pre-`ok` cell whose readings match its rungs must PASS -
    # a stronger property, since it proves the fallback re-derives correctly rather than declining to look.
    with isolated(failures, "re-derivation runs on a pre-`ok` snapshot instead of skipping it"):
        no_ok_rungs = [{k: v for k, v in r.items() if k != "ok"} for r in json.loads(json.dumps(_BASE_RUNGS))]
        preok_cell = cell(perf__sweep_max_proxy=no_ok_rungs)
        fired = list(audit.check_frontier_is_rederivable_from_its_sweep("t", preok_cell))
        if fired:
            failures.append(f"a pre-`ok` cell whose frontier matches its own rungs must re-derive clean "
                            f"through the fallback, not be flagged: got {fired!r}")
        # And the fallback must still catch a real defect - what distinguishes "wider than the engine's
        # rule" from "switched off".
        broken = cell(perf__sweep_max_proxy=no_ok_rungs,
                      perf__frontier=frontier_with(2, rps=_BASE_FRONTIER[2]["rps"] + 37))
        if not list(audit.check_frontier_is_rederivable_from_its_sweep("t", broken)):
            failures.append("the pre-`ok` fallback accepted a reading 37 rps off its own rungs - a "
                            "fallback that catches nothing is a disabled check, not a wider one")
        # `ok_known` is disclosure now, not a gate: passing it False must not change the verdict.
        as_false = list(audit.check_frontier_is_rederivable_from_its_sweep("t", broken, ok_known=False))
        if not as_false:
            failures.append("ok_known=False still suppresses the re-derivation - it is meant to record "
                            "which cells were checked approximately, never to skip them")

    # ── producer_knew_ok: the artifact is asked, same mechanism as producer_knew_the_frontier ─────────
    def _snapshot(c):
        """The minimal shape `served_cells` actually reads: `matrix.upstreams.<eg>.cells.<ing>`, per
        that function's own contract - not a guess at a snapshot's full schema."""
        return {"matrix": {"upstreams": {"eg1": {"cells": {"ing1": c}}}}}

    with isolated(failures, "producer_knew_ok"):
        d_with_ok = _snapshot(cell())
        d_without_ok = _snapshot(cell(perf__sweep_max_proxy=no_ok_rungs))
        if not audit.producer_knew_ok(d_with_ok):
            failures.append("producer_knew_ok said False for a snapshot whose rungs carry `ok`")
        if audit.producer_knew_ok(d_without_ok):
            failures.append("producer_knew_ok said True for a snapshot with no `ok` anywhere - "
                            "it must ask the artifact, not assume the newer shape")

    # ── check_frontier_p99_is_the_observed_tail: the bound-copied-into-the-answer signature ────────
    #
    # The REJECTS entry proves a p99 ABOVE its bound is caught; this proves p99 EXACTLY EQUAL to the
    # bound is reported as its own distinct finding (the bound restated as the answer).
    with isolated(failures, "frontier: p99 exactly equal to its own bound"):
        on_the_nose = frontier_with(2, p99_us=10_000)  # the 10ms reading's own bound, verbatim
        fired = list(audit.check_frontier_p99_is_the_observed_tail(
            "t", cell(perf__frontier=on_the_nose)))
        if not any("its own bound" in v and "exactly" in v for v in fired):
            failures.append(f"check_frontier_p99_is_the_observed_tail must name p99-equals-bound as "
                            f"its own signature, distinct from p99-above-bound, got {fired!r}")

    # ── check_every_absent_frontier_reading_has_a_reason ──────────────────────────────────────────
    with isolated(failures, "frontier: every absent reading has a reason"):
        # ACCEPT: nothing is absent.
        if list(audit.check_every_absent_frontier_reading_has_a_reason("t", cell())):
            failures.append("check_every_absent_frontier_reading_has_a_reason rejected a frontier "
                            "with no absent readings")
        # ACCEPT: a reading absent WITH its reason filed under the BOUND-keyed name.
        absent_reading = frontier_with(0, rps=None, concurrency=None, p99_us=None,
                                        first_disqualified_conc=None)
        with_reasons = cell(
            perf__frontier=absent_reading,
            absences={f"perf.frontier.1ms.{f}": {"reason": "below_resolution", "detail": "x"}
                      for f in ("rps", "concurrency", "p99_us")})
        if list(audit.check_every_absent_frontier_reading_has_a_reason("t", with_reasons)):
            failures.append("check_every_absent_frontier_reading_has_a_reason rejected an absent "
                            "reading whose reasons are filed under perf.frontier.1ms.*")
        # RED: the same absent reading with NO entry in the absences map at all - the bare hole this
        # whole check exists to close. Must name all three bare fields, not just report "something".
        bare = cell(perf__frontier=absent_reading)
        fired = list(audit.check_every_absent_frontier_reading_has_a_reason("t", bare))
        named = {f for f in ("rps", "concurrency", "p99_us")
                if any(f"perf.frontier.1ms.{f}" in v for v in fired)}
        if named != {"rps", "concurrency", "p99_us"}:
            failures.append(f"check_every_absent_frontier_reading_has_a_reason must name all three "
                            f"bare fields (rps, concurrency, p99_us) under perf.frontier.1ms.*, "
                            f"named {named}, got {fired!r}")

    # ── board-level: check_frontier_bounds_agree_with_the_engine (ledger TOOL-02 shape) ────────────
    #
    # ACCEPT: the real engine/src/frontier.rs on disk must agree with python's P99_BOUNDS_US - the
    # assertion that runs in CI.
    with isolated(failures, "frontier bounds mirror the engine"):
        live = list(audit.check_frontier_bounds_agree_with_the_engine())
        if live:
            failures.append(f"python P99_BOUNDS_US and engine/src/frontier.rs's disagree right now: "
                            f"{live}")

        # REJECT #1: the parser must read the ENGINE'S bounds, not echo python's own list back.
        if audit.parse_rust_frontier_bounds(
                "pub const P99_BOUNDS_US: [u64; 3] = [1_000, 2_000, 3_000];") != [1_000, 2_000, 3_000]:
            failures.append("parse_rust_frontier_bounds does not actually read the engine's literal "
                            "array - a cross-check that returns its own side's value agrees with "
                            "everything")

        # RED: the engine GAINS a bound the python mirror lacks - a column the board would publish but
        # this audit would never check, because it doesn't know the column exists.
        tmp4 = tempfile.mkdtemp()
        os.makedirs(os.path.join(tmp4, "engine", "src"))
        with open(os.path.join(tmp4, "engine", "src", "frontier.rs"), "w") as fh:
            fh.write("pub const P99_BOUNDS_US: [u64; 6] = "
                    "[1_000, 5_000, 10_000, 50_000, 100_000, 500_000];\n")
        old_here = audit.HERE
        try:
            audit.HERE = tmp4
            drifted = list(audit.check_frontier_bounds_agree_with_the_engine())
            if len(drifted) != 1 or "disagree" not in drifted[0]:
                failures.append(f"check_frontier_bounds_agree_with_the_engine must reject an engine "
                                f"that declares a 500ms bound python's P99_BOUNDS_US does not have, "
                                f"got {drifted!r}")
            # RED: going BLIND (declaration restated in an unrecognised shape) must fail, not skip.
            with open(os.path.join(tmp4, "engine", "src", "frontier.rs"), "w") as fh:
                fh.write("// the array moved to a different name\n")
            blind = list(audit.check_frontier_bounds_agree_with_the_engine())
            if not any("went blind" in v for v in blind):
                failures.append(f"check_frontier_bounds_agree_with_the_engine must fail when "
                                f"frontier.rs no longer declares P99_BOUNDS_US where this can read "
                                f"it, got {blind!r}")
            # RED: an unreadable frontier.rs must also fail, not skip.
            os.remove(os.path.join(tmp4, "engine", "src", "frontier.rs"))
            unreadable = list(audit.check_frontier_bounds_agree_with_the_engine())
            if not any("cannot read" in v for v in unreadable):
                failures.append(f"check_frontier_bounds_agree_with_the_engine must fail when it "
                                f"cannot read frontier.rs at all, got {unreadable!r}")
        finally:
            audit.HERE = old_here

    # REMOVED: the C6 cross-language bar test (ledger TOOL-02). It pinned bench-audit's `C6_GROSS_PCT`
    # to check-consistency.mjs's copy; both the constant and the gate are gone (the frontier makes the
    # inversion they policed unrepresentable). Monotonicity and re-derivation, above, cover it now.
    # ── ABSENCE_CARRYING_FIELDS must mirror record.rs's absences_of!() lists, field for field ───────
    #
    # ACCEPT: the real engine/src/record.rs, as it stands on disk, must agree with the python lists.
    # This is the assertion that actually runs in CI.
    with isolated(failures, "ABSENCE_CARRYING_FIELDS mirrors record.rs"):
        live_fields = list(audit.check_absence_fields_mirror_the_engine())
        if live_fields:
            failures.append(f"ABSENCE_CARRYING_FIELDS has drifted from the live engine/src/record.rs: "
                            f"{live_fields}")

        # REJECT #1: the parser must read the ENGINE'S fields, not echo python's list. Fed a synthetic
        # absences_of!() call it must report exactly those identifiers (skipping the comment between them).
        _fixture_rs = (
            "impl CellPerf {\n"
            "    pub fn absences(&self) -> BTreeMap<String, AbsentEntry> {\n"
            "        absences_of!(\n"
            "            self,\n"
            "            added_latency_p50_us,\n"
            "            added_latency_p99_us,\n"
            "            // a comment sitting between two fields must not become a fake identifier\n"
            "            rps_max_proxy,\n"
            "        )\n"
            "    }\n"
            "}\n"
        )
        parsed = audit.parse_rust_absences(_fixture_rs, "CellPerf")
        if parsed != ["added_latency_p50_us", "added_latency_p99_us", "rps_max_proxy"]:
            failures.append(f"parse_rust_absences does not actually read the engine's field list - got "
                            f"{parsed!r}")

    # REJECT #2: drop a field from the PYTHON list while the engine keeps carrying it. The accept-side
    # fixture above shrinks with the list, so this must fire from the ENGINE's side, which never shrank.
    with isolated(failures, "ABSENCE_CARRYING_FIELDS mirrors record.rs: REJECT #2-4"):
        tmp3 = tempfile.mkdtemp()
        os.makedirs(os.path.join(tmp3, "engine", "src"))
        with open(os.path.join(tmp3, "engine", "src", "record.rs"), "w") as fh:
            fh.write(
                "impl CellPerf {\n    fn absences(&self) -> X {\n        absences_of!(self, added_latency_p99_us,)\n    }\n}\n"
                "impl CellStream {\n    fn absences(&self) -> X {\n        absences_of!(self, added_ttft_p99_us, cpu_fps_concurrency,)\n    }\n}\n"
                "impl CellMemory {\n    fn absences(&self) -> X {\n        absences_of!(self, idle_rss_mib, plateaued, load_s,)\n    }\n}\n"
            )
        old_here = audit.HERE
        old_fields = audit.ABSENCE_CARRYING_FIELDS
        try:
            audit.HERE = tmp3
            # First pin that the narrowed-to-fixture lists agree, so the failure proven next is caused
            # by the ONE deletion below and nothing else.
            audit.ABSENCE_CARRYING_FIELDS = {
                "perf": ["added_latency_p99_us"],
                "stream": ["added_ttft_p99_us", "cpu_fps_concurrency"],
                "memory": ["idle_rss_mib", "plateaued", "load_s"],
            }
            agreeing = list(audit.check_absence_fields_mirror_the_engine())
            if agreeing:
                failures.append(f"check_absence_fields_mirror_the_engine rejected a python list that "
                                f"matches its fixture engine exactly: {agreeing!r}")
            # RED: now shrink ONLY the python side, exactly as the round-2 audit describes.
            audit.ABSENCE_CARRYING_FIELDS = {
                "perf": ["added_latency_p99_us"],
                "stream": ["added_ttft_p99_us"],  # cpu_fps_concurrency deleted here, not from the engine
                "memory": ["idle_rss_mib", "plateaued", "load_s"],
            }
            shrunk = list(audit.check_absence_fields_mirror_the_engine())
            if not any("cpu_fps_concurrency" in v for v in shrunk):
                failures.append(f"check_absence_fields_mirror_the_engine did not catch a field deleted "
                                f"from the python list while the engine still carries it, got {shrunk!r}")
            # And the mirror image: python claims a field the engine does not carry.
            audit.ABSENCE_CARRYING_FIELDS = {
                "perf": ["added_latency_p99_us", "a_field_the_engine_dropped"],
                "stream": ["added_ttft_p99_us", "cpu_fps_concurrency"],
                "memory": ["idle_rss_mib", "plateaued", "load_s"],
            }
            overclaimed = list(audit.check_absence_fields_mirror_the_engine())
            if not any("a_field_the_engine_dropped" in v for v in overclaimed):
                failures.append(f"check_absence_fields_mirror_the_engine did not catch python claiming a "
                                f"field the engine's absences() no longer carries, got {overclaimed!r}")
            # REJECT #3: going BLIND is a violation, not a pass - record.rs restated in an unrecognised
            # shape (struct renamed) must fail rather than let the audit quietly agree.
            audit.ABSENCE_CARRYING_FIELDS = old_fields
            with open(os.path.join(tmp3, "engine", "src", "record.rs"), "w") as fh:
                fh.write("impl CellPerfRenamed {\n    fn absences(&self) -> X {\n        absences_of!(self, x,)\n    }\n}\n")
            blind = list(audit.check_absence_fields_mirror_the_engine())
            if not any("went blind" in v for v in blind):
                failures.append(f"check_absence_fields_mirror_the_engine must fail when record.rs no "
                                f"longer declares the expected impl block, got {blind!r}")
            # REJECT #4: an unreadable record.rs must also fail, not skip.
            os.remove(os.path.join(tmp3, "engine", "src", "record.rs"))
            unreadable = list(audit.check_absence_fields_mirror_the_engine())
            if not any("cannot read" in v for v in unreadable):
                failures.append(f"check_absence_fields_mirror_the_engine must fail when it cannot read "
                                f"record.rs at all, got {unreadable!r}")
        finally:
            audit.HERE = old_here
            audit.ABSENCE_CARRYING_FIELDS = old_fields

    # ── paths are anchored to the script, not the cwd (ledger TOOL-03) ────────────────────────────
    #
    # RED: the loader must find snapshots from any cwd. A relative glob resolved against the caller's
    # shell and returned nothing; asserting "same answer from /" is the only form that can't pass by accident.
    with isolated(failures, "snapshot_paths is cwd-independent (ledger TOOL-03)"):
        from_root = None
        old_cwd = os.getcwd()
        try:
            os.chdir(os.sep)
            from_root = audit.snapshot_paths()
        finally:
            os.chdir(old_cwd)
        from_here = audit.snapshot_paths()
        if from_root != from_here:
            failures.append(f"snapshot_paths() is cwd-dependent: {len(from_here)} files from the repo "
                            f"root, {len(from_root)} from / - the audit must be runnable from any cwd")
        if from_here and not all(os.path.isabs(p) for p in from_here):
            failures.append("snapshot_paths() returned a relative path - it will re-resolve against cwd")

    # The per-gateway invariant is driven off real definitions, since its subject is what the repo
    # actually declares. Pre-declared so the final summary can still read `declared_and_untestable`
    # even if this block's isolated() catches a raise partway through.
    declared_and_untestable = []
    bifrost_clean = []
    with isolated(failures, "per-gateway: declaration vs. untestable"):
        declared_and_untestable = list(audit.check_declaration_matches_what_we_measured("one-api"))
        bifrost_clean = list(audit.check_declaration_matches_what_we_measured("bifrost"))
        if bifrost_clean:
            failures.append(f"bifrost declares nothing it marks untestable, but the check fired: {bifrost_clean}")

    # RED half: the real tree's declared/untestable intersection is empty, so the accept side above
    # can't prove the check still fires. A fabricated gateway that declares openai/openai AND marks it
    # untestable must yield exactly one violation. Pointed at the fixture by moving audit.HERE, not
    # chdir: paths are anchored to the file now (TOOL-03), so chdir would read the real gateways/ dir.
    with isolated(failures, "per-gateway: RED half (fabricated declared+untestable gateway)"):
        tmp = tempfile.mkdtemp()
        old_here = audit.HERE
        try:
            os.makedirs(os.path.join(tmp, "gateways", "fake"))
            with open(os.path.join(tmp, "gateways", "fake", "definition.json"), "w") as fh:
                json.dump({
                    "matrix": ["100000", "000000", "000000", "000000", "000000", "000000"],
                    "untestable": ["openai/openai"],
                }, fh)
            audit.HERE = tmp
            fired = list(audit.check_declaration_matches_what_we_measured("fake"))
            if len(fired) != 1:
                failures.append(
                    f"check_declaration_matches_what_we_measured must yield exactly one violation for a "
                    f"gateway that declares openai/openai and marks it untestable, got {fired!r} - "
                    f"it cannot fail, so it guards nothing")
        finally:
            audit.HERE = old_here

    # A malformed definition.json must not crash the whole audit: one truncated file for any gateway
    # must not take every other gateway's report down with it.
    with isolated(failures, "per-gateway: malformed definition.json must not crash the audit"):
        tmp = tempfile.mkdtemp()
        old_here = audit.HERE
        try:
            os.makedirs(os.path.join(tmp, "gateways", "broken"))
            with open(os.path.join(tmp, "gateways", "broken", "definition.json"), "w") as fh:
                fh.write("{not valid json")
            audit.HERE = tmp
            fired = list(audit.check_declaration_matches_what_we_measured("broken"))
            if fired:
                failures.append(
                    f"check_declaration_matches_what_we_measured must not fabricate violations for "
                    f"an unreadable definition.json, got {fired!r}")
        finally:
            audit.HERE = old_here

    for f in failures:
        print(f"FAIL: {f}")
    if failures:
        return 1
    print(f"PASS: {len(REJECTS)} checks each reject their own violation and accept a clean cell.")
    if declared_and_untestable:
        print(f"note: one-api still declares {len(declared_and_untestable)} cell(s) it marks untestable")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
