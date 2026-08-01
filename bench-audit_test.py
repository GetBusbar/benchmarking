#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# EVERY AUDIT CHECK MUST BE ABLE TO FAIL.
#
# This file exists because the first draft of `bench-audit.py` shipped a check that could not. Its
# regression guard for the exact defect that forced a full 14-gateway rerun - throughput windows
# published without the p99 they measured - read `cell["sweep_max_proxy"]` when the sweep lives at
# `cell["perf"]["sweep_max_proxy"]`. It found nothing, returned early, and reported PASS on data that
# violates it on all 64 cells. It was written, run against a real board, and it agreed with the board.
#
# That is the same species as `transient_budget()` called by nothing, `box_qualify` always seeding,
# and 27 site tests asserting against an empty board. An audit made of checks like that is worse than
# no audit, because it converts "nobody looked" into "it passed".
#
# So each check gets a cell it MUST reject and a cell it MUST accept. A check that cannot be made to
# fire is not protecting anything, and this file is what makes that a red test rather than a quiet
# green board.
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
# doubling of concurrency both raises the rate and pushes the tail into the next bound's territory, so
# every bound's winning rung, its boundary rung above it, and the final lower-bound reading are all
# exercised by one small ladder rather than asserted in isolation.
#
# `ok` carries a positive count on every rung (arbitrarily set equal to `rps`, since the exact value
# does not matter to any check here - only `ok > 0` does) so this ladder reads as `ok_known=True` and
# every rung is PROVABLY clean under the exact `rung_served_cleanly` rule, not merely clean by the
# `fail: 0` that used to be the whole story.
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

# The frontier re-derived from `_BASE_RUNGS` by hand, per `frontier::read_at`: the max rps among
# qualifying rungs, the concurrency it was observed at, that rung's own p99, the lowest concurrency
# above it that stopped qualifying, and whether the winner is the top of the ladder. Only the last
# reading (unbounded) wins at the ladder's top rung, so it is the only one with `lower_bound: True`.
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
    """A deep copy of `_BASE_FRONTIER` with reading `i` mutated - the standard shape for a red fixture
    that must trip exactly one frontier check without disturbing the five readings around it."""
    fr = json.loads(json.dumps(_BASE_FRONTIER))
    fr[i].update(field_over)
    return fr


# Each entry: the check, a cell it must REJECT, and what the violation is about.
#
# REMOVED: check_sustained_not_above_peak compared `perf.rps_sustained_20ms` against
# `perf.rps_max_proxy` and allowed C6_GROSS_PCT of window noise between them. BOTH FIELDS ARE DELETED
# from `CellPerf` - see `frontier.rs`'s module header, reason #3: the two scalars came off two
# different algorithms over the same rungs (a plateau search vs. a gate bisection) and could disagree,
# which is exactly the state this check policed. The frontier is one algorithm - `max(rps)` over a
# qualifying set that only grows as the bound relaxes - so the inversion is structural nonsense now,
# not merely unchecked, and its successor `check_frontier_rates_are_monotone_in_the_bound` asserts the
# ordering over all six readings instead of two. Tested below.
#
# REMOVED: check_peak_came_from_its_own_sweep held `perf.rps_max_proxy` to `max(sweep_max_proxy)` -
# one direction of one comparison against a deleted scalar. Its strictly stronger successor,
# `check_frontier_is_rederivable_from_its_sweep`, recomputes all four fields of every reading (rate,
# concurrency, tail, boundary rung) from the rungs and demands equality both ways. Tested below.

# ── traces, for the SEARCH-TRACE checks ───────────────────────────────────────────────────────────
#
# `cell()` carries no `sweep_streams`, so every trace check returns early on it - which makes it a
# valid accept-side fixture but a weak one: a check that never runs accepts everything. These build
# real ladders so both sides are proven against a trace the check actually reads.
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
    # These five are why this board could return PASS ten times while eight cells had silently given
    # up: every other check reads the published NUMBER, and a number that was never produced has
    # nothing to check. Each of these reads what the search DID.
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
    # field DROPPED from the block is invisible to it. The clean baseline cell already omits most of
    # the declared field list, which is why this fixture rejects while the accept-side pairs below
    # supply a fully-carried cell rather than reusing cell().
    (audit.check_declared_fields_are_carried, cell(),
     "a served cell whose blocks omit declared fields entirely"),
    (audit.check_stream_capacity_is_a_number,
     cell(stream__stream_served=True, stream__streams_sustained=None),
     "a served streaming cell whose capacity metric is a hole with nothing explaining it"),
    # REMOVED: check_frames_have_a_stream_behind_them fired when `stream.cpu_fps` was published beside
    # a MEASURED `streams_sustained` of 0 - a frames/sec rate over a population of zero streams.
    # `cpu_fps`, `cpu_fps_concurrency` and `sweep_cpu_fps` are deleted from `CellStream` with nothing
    # taking their place: `streams_sustained_fps` is the frame rate OF `streams_sustained` itself,
    # measured in the SAME window rather than by a separate climb, so it can no longer be a rate over a
    # population it was not measured against. There is no frame rate left in the artifact this check
    # could still be pointed at.
    # ── the frontier's own REJECTS entries ────────────────────────────────────────────────────────
    #
    # `check_frontier_is_complete` and `check_every_absent_frontier_reading_has_a_reason`'s reject
    # cases live in dedicated blocks below rather than here, because the first needs the
    # `frontier_known` kwarg exercised in both states and the second's mangle table has more than one
    # shape worth pinning individually. The five here each have exactly one failure mode to prove.
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

    Every REJECTS entry calls a check function DIRECTLY (`list(check("t", bad))`) and expects it to
    return violations, not raise. Before this, a check mutated to raise instead of returning the
    wrong thing - the exact failure mode the round-2 audit found - would blow up `list(check(...))`
    and abort this WHOLE FILE at that line, taking every other REJECTS entry and every check below
    it (the C6 bar, the absence-fields mirror, the cwd-independence proof, the per-gateway check)
    down with it, uncounted rather than failed. `python3 bench-audit_test.py` would exit non-zero
    either way, so the failure still "worked" in the crudest sense - but it printed a traceback
    instead of a punch list, and it could not tell you whether ONE check regressed or all of them
    did, because only one of them ever got the chance to run.

    Wrapping each independent block in this context manager makes a raise a recorded entry in
    `failures` for THAT block only; every block after it still runs, same as `site/test.mjs`'s
    runner documents for the same reason: "ordering must not decide coverage" (test.mjs:35-39).
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

    # And the other half: a check that rejects everything is equally useless, because a board that
    # can never pass gets the gate switched off.
    #
    # `check_declared_fields_are_carried` is the one check the minimal baseline cell cannot satisfy -
    # its whole subject is the FULL declared field list, and cell() deliberately carries only the
    # handful of fields the other checks read. Its accept side is proven below against a cell built
    # from ABSENCE_CARRYING_FIELDS itself, which is a stronger statement anyway: the accepting fixture
    # is derived from the very list the check enforces, so the two cannot drift apart.
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
        # `cpu_fps` IS GONE FROM `check_stream_capacity_is_a_number` (it is deleted from `CellStream`
        # entirely, see the REMOVED note at `check_frames_have_a_stream_behind_them`'s old site in
        # bench-audit.py) - the check's field list below it is just `("streams_sustained",)` now, so
        # every fixture here is about that one field rather than the cpu_fps/cpu_fps_concurrency pair
        # the pre-frontier version of this test exercised.
        zeroed = cell(stream__stream_served=True, stream__streams_sustained=0)
        if list(audit.check_stream_capacity_is_a_number("t", zeroed)):
            failures.append("check_stream_capacity_is_a_number rejected a measured 0")
        excused = cell(stream__stream_served=True, stream__streams_sustained=None,
                       absences={"stream.streams_sustained": {"reason": "untestable"}})
        if list(audit.check_stream_capacity_is_a_number("t", excused)):
            failures.append("check_stream_capacity_is_a_number rejected a rig-class absence")
        # A search that RAN, failed to establish a ceiling, and said WHY is not a silent yield.
        # Demanding a measured 0 there fabricates an answer to a question that was never settled.
        # This is the real shape from the 2026-07-29 field run.
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
    # ACCEPT: a cell that carries every declared field passes, whether the field holds a number or an
    # explicit null. The fixture is generated FROM ABSENCE_CARRYING_FIELDS, so if the engine's field
    # list grows and the audit's list grows with it, this accept-side fixture grows too and keeps
    # proving the same property instead of pinning a snapshot of yesterday's schema.
    carried = {"served": True}
    with isolated(failures, "omitted-field check (ledger TOOL-04)"):
        for _b, _fs in audit.ABSENCE_CARRYING_FIELDS.items():
            carried[_b] = {_f: 1.0 for _f in _fs}
        if list(audit.check_declared_fields_are_carried("t", carried)):
            failures.append("check_declared_fields_are_carried rejected a cell that carries every field")
        # A field to null out / drop below, picked off the LIVE list rather than hardcoded - a field
        # this audit used to name here (`perf.conc_at_peak`) is exactly the kind of thing that gets
        # deleted out from under a fixture, which is what made this test start raising KeyError
        # instead of testing anything.
        _perf_field = audit.ABSENCE_CARRYING_FIELDS["perf"][0]
        carried_with_null = json.loads(json.dumps(carried))
        carried_with_null["perf"][_perf_field] = None
        if list(audit.check_declared_fields_are_carried("t", carried_with_null)):
            failures.append("check_declared_fields_are_carried rejected an explicit null - it polices "
                            "OMISSION, and a null-with-reason is the honest shape it exists to require")

        # REJECT, precisely: dropping ONE key from an otherwise complete cell must yield exactly one
        # violation naming that key. The blanket reject above (a sparse cell) would still fire if the
        # check degenerated into "the block is small"; this pins that it is the missing KEY it sees.
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
        # The live case this carve-out exists for: aisix openai-responses>openai answers 200 (so
        # `served` is True and its siblings all carry frontiers - the exact shape the RED case above
        # calls a drop), but egress re-verification proved the gateway never translated. The request
        # arrived on the mock's openai-responses endpoint and nothing arrived on openai, so it
        # forwarded the ingress request unchanged. Every number taken there describes a different wire,
        # so the engine withheld the whole perf group and published `egress_reverified: false`. An
        # empty frontier is the CORRECT artifact; demanding one would demand a throughput figure for a
        # translation that did not happen.
        withheld = cell(perf__frontier=[], perf__egress_reverified=False)
        if list(audit.check_frontier_is_complete("t", withheld, frontier_known=True)):
            failures.append("check_frontier_is_complete flagged a cell whose perf was WITHHELD because "
                            "egress re-verification proved the gateway did not translate - an empty "
                            "frontier is correct there, and calling it a drop cries wolf on the one "
                            "case where the harness behaved best")

        # RED, and this is the half that matters: the exemption is keyed on the DISCLOSURE, not on
        # trusting the producer. A cell that merely lost its frontier has no `egress_reverified: false`
        # and must still fail, in both the reverified-true and field-absent shapes.
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
    # A looser bound publishing NO rate while a tighter one published one is the same violation with
    # the rate missing rather than merely low - the looser bound's qualifying set contains the
    # tighter's, so it cannot be empty when the tighter one was not.
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
    # The REJECTS entry above already proves a wrong RATE is caught; these two prove the concurrency
    # and the boundary proof are independently re-derived rather than trusted once the rate matches.
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
    # This is the property `ok` exists to make provable at all - see its docstring's plano paragraph.
    # "0 of 0 looks clean" is the defect `ok > 0` guards against, and it was UNVERIFIABLE before this
    # field was published: `fail == 0` alone cannot tell a window that completed nothing from one that
    # completed everything it accepted, and the file's old approximation (`rps > 0` or a p99) could not
    # see a window with `ok: 0` at all - it had no signal to read. Now it does.
    with isolated(failures, "rung_served_cleanly: exact ok/fail rule"):
        # ACCEPT: the ordinary clean rung - ok > 0, fail == 0.
        if not audit.rung_served_cleanly({"conc": 8, "rps": 500, "p99_us": 800, "fail": 0, "ok": 500}):
            failures.append("rung_served_cleanly rejected an ordinary clean rung (ok=500, fail=0)")
        # RED: `ok: 0` alongside `fail: 0` - a window that completed NOTHING. "0 of 0 looks clean" is
        # exactly the ambiguity `ok > 0` exists to break, and it was unreachable by this file before
        # `ok` was published - there was no field to hold a real 0 apart from an absence.
        if audit.rung_served_cleanly({"conc": 999, "rps": None, "p99_us": None, "fail": 0, "ok": 0}):
            failures.append("rung_served_cleanly accepted ok=0, fail=0 as clean - a window that "
                            "completed nothing must not count as having served cleanly")
        # GREEN, and this one INVERTED: `ok` absent entirely (every snapshot measured before the field
        # existed). It first refused these, on the reasoning that unverifiable must not read as clean -
        # but refusing meant the re-derivation had to skip whole boards, and a skipped check is weaker
        # than an approximate one. So an absent `ok` FALLS BACK: a positive rate or a p99 is proof the
        # window completed something, which is WIDER than the engine's `ok > 0` (a completion always
        # leaves a latency sample, while a rate can round away through `as i64`), so the residual error
        # is a missed catch rather than a false alarm.
        if not audit.rung_served_cleanly({"conc": 8, "rps": 500, "p99_us": 800, "fail": 0}):
            failures.append("rung_served_cleanly refused a rung with no `ok` but a real rate and tail - "
                            "it must fall back rather than refuse, or the re-derivation skips whole "
                            "pre-`ok` boards and the strongest check goes unrun on what ships")
        # AND THE FALLBACK STILL HAS A FLOOR: no `ok`, no rate, no tail proves nothing at all.
        if audit.rung_served_cleanly({"conc": 8, "rps": None, "p99_us": None, "fail": 0}):
            failures.append("rung_served_cleanly accepted a rung with no ok, no rate and no tail - "
                            "nothing there evidences a completion, so the fallback must still refuse")
        # RED, the sibling: `fail` absent with `ok` present must not read as clean either - unchanged
        # from before `ok` existed, pinned again here so the exact rewrite did not quietly drop it.
        if audit.rung_served_cleanly({"conc": 8, "rps": 500, "p99_us": 800, "ok": 500}):
            failures.append("rung_served_cleanly accepted a rung with no `fail` field as clean")

    # ── the ok:0/fail:0 and ok-absent defects must not let a rung WIN a reading ──────────────────────
    #
    # A unit-level pass on `rung_served_cleanly` is necessary but not sufficient - the actual bar is
    # that a rung which cannot prove it served cleanly must not be the rung a published frontier
    # reading is built on. Rung index 2 (c=128) is the winner of the 10ms reading in `_BASE_FRONTIER`
    # (3,000 rps at p99=8,000us); knocking out ITS cleanliness must knock its reading's re-derivation
    # loose, because the true winner drops to rung index 1 (c=32, 1,500 rps) and the published 3,000
    # no longer matches anything the sweep can back.
    with isolated(failures, "ok:0/fail:0 and ok-absent rungs must not win a frontier reading"):
        ok_zero_rungs = json.loads(json.dumps(_BASE_RUNGS))
        ok_zero_rungs[2]["ok"] = 0  # was 3_000; the rung still publishes fail: 0 and a real rate/p99
        fired = list(audit.check_frontier_is_rederivable_from_its_sweep(
            "t", cell(perf__sweep_max_proxy=ok_zero_rungs)))
        if not any("is 1500" in v for v in fired):
            failures.append(f"check_frontier_is_rederivable_from_its_sweep let a rung with ok=0, "
                            f"fail=0 win the 10ms reading - re-derived from the rungs that can PROVE "
                            f"they served cleanly the answer is 1500, got {fired!r}")

        # THE ABSENT-`ok` SIBLING INVERTED for the same reason as the unit case above: with the fallback
        # in place, a rung missing `ok` but carrying a real rate and tail is proven clean by that rate, so
        # it legitimately KEEPS the reading and nothing should fire. Asserting silence here is the
        # non-vacuous half - it proves the fallback re-derives rather than merely declining to look, and
        # the `ok: 0` case just above proves it still catches a rung that truly completed nothing.
        ok_absent_rungs = json.loads(json.dumps(_BASE_RUNGS))
        del ok_absent_rungs[2]["ok"]
        fired = list(audit.check_frontier_is_rederivable_from_its_sweep(
            "t", cell(perf__sweep_max_proxy=ok_absent_rungs)))
        if fired:
            failures.append(f"a rung with no `ok` but a real rate and tail must keep its reading through "
                            f"the fallback, not be treated as unproven: got {fired!r}")

    # ── a snapshot that predates `ok` is RE-DERIVED ANYWAY, via the wider fallback ────────────────────
    #
    # THIS TEST WAS INVERTED, and the inversion is the finding. It used to assert that `ok_known=False`
    # SKIPPED the re-derivation entirely, on the reasoning that without `ok` every rung fails
    # `rung_served_cleanly` for lack of proof, every qualifying set empties, and every honestly published
    # rate gets flagged as having no rung behind it - the whole board at once, for a field its engine
    # never had.
    #
    # That reasoning was sound and the remedy was wrong. Skipping meant the STRONGEST check in the file
    # went unrun on all 26 cells of the board that was about to ship, and a skipped check is weaker than
    # an approximate one. `rung_served_cleanly` now falls back to treating a positive rate OR a p99 as
    # proof of completion when `ok` is absent - wider than the engine's rule, since a completion always
    # leaves a latency sample while a rate can round away through `as i64` - so the residual error is a
    # missed catch rather than a false alarm, which is the only direction an approximation may err in a
    # tool whose warnings have to be trusted.
    #
    # So: a pre-`ok` cell whose readings genuinely match its rungs must now PASS, not be skipped. That is
    # a stronger property than the old one - it proves the fallback actually re-derives correctly rather
    # than merely declining to look.
    with isolated(failures, "re-derivation runs on a pre-`ok` snapshot instead of skipping it"):
        no_ok_rungs = [{k: v for k, v in r.items() if k != "ok"} for r in json.loads(json.dumps(_BASE_RUNGS))]
        preok_cell = cell(perf__sweep_max_proxy=no_ok_rungs)
        fired = list(audit.check_frontier_is_rederivable_from_its_sweep("t", preok_cell))
        if fired:
            failures.append(f"a pre-`ok` cell whose frontier matches its own rungs must re-derive clean "
                            f"through the fallback, not be flagged: got {fired!r}")
        # AND THE FALLBACK MUST STILL CATCH A REAL DEFECT. An approximation that accepts everything is
        # not a check; this is what distinguishes "wider than the engine's rule" from "switched off".
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
    # The REJECTS entry above proves a p99 ABOVE its own bound is caught; this proves the check reports
    # p99 EXACTLY EQUAL to the bound as its OWN, distinct finding - "qualification is strictly under
    # the bound" - because that shape is the specific defect worth naming (the bound restated as the
    # answer), not just another instance of the inequality.
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
    # ACCEPT: the real engine/src/frontier.rs, as it stands on disk, must agree with python's
    # P99_BOUNDS_US. This is the assertion that actually runs in CI.
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

        # RED (the mangle from the brief): the engine GAINS a bound the python mirror does not have -
        # a sixth column the board would publish that this audit would never check for completeness,
        # monotonicity or re-derivation, because it does not know the column exists.
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

    # REMOVED: the C6 cross-language bar test (ledger TOOL-02).
    #
    # It asserted that bench-audit's `C6_GROSS_PCT` and site/check-consistency.mjs's own
    # `export const C6_GROSS_PCT = 5;` were the same number, by parsing the JS literal. Both the
    # constant and the gate are gone: the ceiling capped how much window noise could excuse a
    # sustained figure sitting above the maximum it was meant to sit under, and BOTH of those fields
    # are deleted. The frontier makes that inversion unrepresentable rather than merely bounded - six
    # maxima over sets that only grow as the bound relaxes - so a gate policing agreement between two
    # dead literals would have been decoration of exactly the kind this file exists to catch.
    #
    # What covers the invariant now, both halves, above: monotonicity across all six readings, and
    # re-derivation of every reading from the raw rungs.
    # ── ABSENCE_CARRYING_FIELDS must mirror record.rs's absences_of!() lists, field for field ───────
    #
    # ACCEPT: the real engine/src/record.rs, as it stands on disk, must agree with the python lists.
    # This is the assertion that actually runs in CI.
    with isolated(failures, "ABSENCE_CARRYING_FIELDS mirrors record.rs"):
        live_fields = list(audit.check_absence_fields_mirror_the_engine())
        if live_fields:
            failures.append(f"ABSENCE_CARRYING_FIELDS has drifted from the live engine/src/record.rs: "
                            f"{live_fields}")

        # REJECT #1: the parser must read the ENGINE'S fields, not echo python's own list back. Feed it a
        # synthetic absences_of!() call and it must report exactly those identifiers, comments and all.
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

    # REJECT #2 (the round-2 audit's own scenario): drop a field from the PYTHON list while the engine
    # keeps carrying it. This is exactly "delete cpu_fps_concurrency from the stream list", the move
    # that used to leave bench-audit_test.py green because the accept-side fixture up above is
    # generated FROM ABSENCE_CARRYING_FIELDS and shrinks right along with it. This check must fire
    # from the ENGINE's side, which never shrank.
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
            # First pin that the fixture, with the field lists left untouched apart from being narrowed
            # to match the fixture's smaller engine, agrees - so the failure proven next is caused by the
            # ONE deletion below and nothing else about the fixture.
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
            # REJECT #3: going BLIND is a violation, not a pass - record.rs restated in a shape this
            # cannot recognise (struct renamed) must fail rather than let the audit quietly agree.
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
    # RED: from a different cwd the loader must still find this repo's snapshots. Before the fix
    # `glob.glob("results/snapshots/…")` resolved against the caller's shell and returned nothing, so
    # the audit reported an empty board about a populated one. Asserting "same answer from /" is the
    # only form of this assertion that cannot pass by accident.
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

    # The per-gateway invariant is driven off real definitions rather than a fixture, because its
    # whole subject is what the repo actually declares.
    #
    # Pre-declared (not just assigned inside the block below) so the final summary's reference to
    # `declared_and_untestable` still has a name to read even if this block's isolated() catches a
    # raise partway through - a raise here must not also crash the report at the bottom of the file.
    declared_and_untestable = []
    bifrost_clean = []
    with isolated(failures, "per-gateway: declaration vs. untestable"):
        declared_and_untestable = list(audit.check_declaration_matches_what_we_measured("one-api"))
        bifrost_clean = list(audit.check_declaration_matches_what_we_measured("bifrost"))
        if bifrost_clean:
            failures.append(f"bifrost declares nothing it marks untestable, but the check fired: {bifrost_clean}")

    # And its RED half. The real tree's declared/untestable intersection is currently empty, so the
    # accept-side assertions above cannot prove the check still fires: if its field names or matrix
    # orientation drifted, it would go silently inert - the exact defect class this file exists for.
    # A fabricated gateway that both declares openai/openai in its matrix AND marks it untestable
    # must yield exactly one violation.
    #
    # Pointed at the fixture tree by moving audit.HERE, not by chdir: the audit's paths are now
    # anchored to its own file (TOOL-03), so chdir would no longer redirect it - it would read the
    # real gateways/ directory, find nothing, and this RED proof would go green by doing nothing.
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

    # A malformed definition.json must not crash the whole audit before any gateway's PASS/FAIL is
    # printed. load() (see its own docstring), newest_engine(), and the skipped-gateway loop all wrap
    # json.load and degrade gracefully on a bad file; check_declaration_matches_what_we_measured()
    # does not, so one truncated definition.json for ANY gateway takes every other gateway's report
    # down with it.
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
