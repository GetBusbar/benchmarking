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


def cell(**over):
    """A cell that violates nothing, as the baseline every check is proven against."""
    c = {
        "served": True,
        "perf": {
            "rps_max_proxy": 10_000,
            "conc_at_peak": 64,
            "rps_sustained_20ms": 9_000,
            "rps_sustained_20ms_concurrency": 40,
            "sweep_max_proxy": [
                {"conc": 64, "rps": 10_000, "p99_us": 5_000, "fail": 0},
                {"conc": 128, "rps": 9_500, "p99_us": 30_000, "fail": 0},
            ],
        },
        "stream": {
            "added_ttft_p50_us": 100,
            "added_ttft_p99_us": 400,
            "streams_sustained": 128,
            "cpu_fps": 6_400,
        },
    }
    for k, v in over.items():
        section, _, field = k.partition("__")
        if field:
            c[section][field] = v
        else:
            c[section] = v
    return c


# Each entry: the check, a cell it must REJECT, and what the violation is about.
REJECTS = [
    (audit.check_sustained_not_above_peak, cell(perf__rps_sustained_20ms=11_000),
     "a sustained figure above the peak it shares a sweep with"),
    (audit.check_peak_came_from_its_own_sweep, cell(perf__rps_max_proxy=99_000),
     "a peak no window in its own sweep produced"),
    (audit.check_sweep_carries_its_latency,
     cell(perf__sweep_max_proxy=[{"conc": 64, "rps": 10_000, "p99_us": None, "fail": None}]),
     "throughput windows published without the p99 they measured"),
    (audit.check_ttft_percentiles_are_ordered, cell(stream__added_ttft_p99_us=50),
     "a p99 below the p50 from the same sample set"),
    (audit.check_rate_and_concurrency_travel_together, cell(perf__rps_sustained_20ms_concurrency=None),
     "a rate with no concurrency beside it"),
    # Past MAX_RPS_PER_CONNECTION, deliberately: the bar is loose on purpose (it catches a rate
    # divided by the wrong thing, not marginal optimism), so the fixture has to clear it rather than
    # the bar being lowered to meet the fixture.
    (audit.check_rate_is_physically_possible, cell(perf__conc_at_peak=1, perf__rps_max_proxy=50_000),
     "50000 rps on a single connection"),
    (audit.check_frames_have_a_stream_behind_them, cell(stream__streams_sustained=0),
     "frames per second over a population of zero"),
    (audit.check_no_bare_absence, cell(stream__added_gap_p50_us=None),
     "a null metric with no reason in absences (a bare hole)"),
    # The other half of the hole: `check_no_bare_absence` reads `f in blk and blk[f] is None`, so a
    # field DROPPED from the block is invisible to it. The clean baseline cell already omits most of
    # the declared field list, which is why this fixture rejects while the accept-side pairs below
    # supply a fully-carried cell rather than reusing cell().
    (audit.check_declared_fields_are_carried, cell(),
     "a served cell whose blocks omit declared fields entirely"),
    (audit.check_stream_capacity_is_a_number,
     cell(stream__stream_served=True, stream__cpu_fps=None),
     "a served streaming cell whose capacity metric is a hole instead of a measured 0"),
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
        zeroed = cell(stream__stream_served=True, stream__cpu_fps=0, stream__streams_sustained=0)
        zeroed["stream"].pop("cpu_fps_concurrency", None)
        if list(audit.check_stream_capacity_is_a_number("t", zeroed)):
            failures.append("check_stream_capacity_is_a_number rejected a measured 0")
        excused = cell(stream__stream_served=True, stream__cpu_fps=None,
                       absences={"stream.cpu_fps": {"reason": "untestable"}})
        if list(audit.check_stream_capacity_is_a_number("t", excused)):
            failures.append("check_stream_capacity_is_a_number rejected a rig-class absence")

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
        carried_with_null = json.loads(json.dumps(carried))
        carried_with_null["stream"]["cpu_fps"] = None
        if list(audit.check_declared_fields_are_carried("t", carried_with_null)):
            failures.append("check_declared_fields_are_carried rejected an explicit null - it polices "
                            "OMISSION, and a null-with-reason is the honest shape it exists to require")

        # REJECT, precisely: dropping ONE key from an otherwise complete cell must yield exactly one
        # violation naming that key. The blanket reject above (a sparse cell) would still fire if the
        # check degenerated into "the block is small"; this pins that it is the missing KEY it sees.
        one_short = json.loads(json.dumps(carried))
        del one_short["perf"]["conc_at_peak"]
        fired = list(audit.check_declared_fields_are_carried("t", one_short))
        if len(fired) != 1 or "perf.conc_at_peak" not in fired[0]:
            failures.append(f"check_declared_fields_are_carried must name the one omitted key, got {fired!r}")

        # REJECT: a whole block deleted is the same claim with the evidence removed, not a quieter cell.
        no_block = json.loads(json.dumps(carried))
        del no_block["memory"]
        fired = list(audit.check_declared_fields_are_carried("t", no_block))
        if len(fired) != 1 or "NO memory block" not in fired[0]:
            failures.append(f"check_declared_fields_are_carried must reject a served cell with no memory "
                            f"block at all, got {fired!r}")

    # ── the C6 cross-language bar (ledger TOOL-02) ────────────────────────────────────────────────
    #
    # ACCEPT: the real site file, as it stands on disk, must agree with the python constant. This is
    # the assertion that actually runs in CI, and it is the one that fires the day someone tunes
    # either literal.
    with isolated(failures, "C6 cross-language bar (ledger TOOL-02)"):
        live = list(audit.check_c6_bar_agrees_with_the_site())
        if live:
            failures.append(f"the two C6_GROSS_PCT literals disagree right now: {live}")

        # REJECT #1: the parser must read the site's NUMBER, not echo python's. Feed it a source that
        # declares a different bar and it must report that different bar.
        if audit.parse_site_c6("export const C6_GROSS_PCT = 7.5;\n") != 7.5:
            failures.append("parse_site_c6 does not actually read the site's literal - a cross-check that "
                            "returns its own side's value agrees with everything")
        # REJECT #2: with the site pointed at a fixture declaring 7, the check must fire.
        tmp2 = tempfile.mkdtemp()
        os.makedirs(os.path.join(tmp2, "site"))
        with open(os.path.join(tmp2, "site", "check-consistency.mjs"), "w") as fh:
            fh.write("// drifted\nexport const C6_GROSS_PCT = 7;\n")
        old_here = audit.HERE
        try:
            audit.HERE = tmp2
            drifted = list(audit.check_c6_bar_agrees_with_the_site())
            if len(drifted) != 1 or "different bars" not in drifted[0]:
                failures.append(f"check_c6_bar_agrees_with_the_site must reject a site declaring 7 while "
                                f"python declares {audit.C6_GROSS_PCT}, got {drifted!r}")
            # REJECT #3: going BLIND is a violation, not a pass. A missing/renamed site file must fail
            # rather than let the audit quietly stop comparing.
            os.remove(os.path.join(tmp2, "site", "check-consistency.mjs"))
            blind = list(audit.check_c6_bar_agrees_with_the_site())
            if len(blind) != 1 or "unverifiable twin" not in blind[0]:
                failures.append(f"check_c6_bar_agrees_with_the_site must fail when it cannot read the "
                                f"site's copy, got {blind!r}")
        finally:
            audit.HERE = old_here

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
