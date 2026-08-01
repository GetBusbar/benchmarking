#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# EVERY CROSS-METRIC INVARIANT A PUBLISHED BOARD MUST HOLD, as a program that exits non-zero.
#
# This file exists because of a specific failure, and the failure was not a bad number - it was a bad
# ANSWER TO "IS THE DATA GOOD?". Auditing a run meant writing throwaway python in a scratch directory,
# eyeballing the output, and forming an opinion. An opinion does not survive the next run, cannot be
# handed to anyone, and cannot be re-checked after a change. When asked "would you publish this?" the
# honest answer had to be "the checks I happened to think of, passed" - which is not a verdict.
#
# So the checks live here, they run over the artifacts, and "the audit is done" means this exits 0.
#
# It is the same lesson the engine spent a week learning about its own guards. `transient_budget()`
# was called by nothing. `box_qualify` always seeded. `history/append.py` wrote zero rows. Twenty-seven
# site tests asserted against an empty board. Every one of them was a check that existed in someone's
# head and nowhere in the code, and every one of them passed by doing nothing at all. A gate that
# cannot fail is not a gate, and a gate nobody wrote is worse.
#
#   ./bench-audit.py                    the newest engine present in results/snapshots
#   ./bench-audit.py --engine dc7a53c   a specific engine, to audit one board
#   ./bench-audit.py --gateway kong     narrow to one gateway while investigating
#
# Runnable from any directory: every path it reads is anchored to this file, not to the caller's cwd.
#
# Exit 0 = every invariant held. Exit 1 = at least one did not, and each violation names the cell.
import argparse
import collections
import datetime
import glob
import json
import os
import re
import sys

# ── where the board lives ─────────────────────────────────────────────────────────────────────────
#
# ANCHORED TO THIS FILE, NOT TO THE INVOKER'S CWD (ledger TOOL-03). `glob.glob("results/snapshots/…")`
# resolves against whatever directory the audit was started from, so running it from anywhere but the
# repo root found zero files. That used to be a silent vacuous pass; the empty-board skip in main()
# now prints and exits 0, which is only marginally better - it still reports "nothing published" about
# a board that is right there on disk. An audit whose answer depends on the caller's shell state is
# not a verdict, so the paths come off this script's own location and the tool is runnable from any
# cwd, which is also what CI, a git hook and an operator poking at it from ~ all want.
#
# HERE is a module global rather than a constant baked into each call precisely so the tests can point
# the whole audit at a fixture tree: a check that can only be exercised against the real repo is a
# check whose RED half cannot be written.
HERE = os.path.dirname(os.path.abspath(__file__))


def snapshot_paths():
    """Every snapshot file on this board, sorted, resolved against HERE rather than the cwd."""
    return sorted(glob.glob(os.path.join(HERE, "results", "snapshots", "result_*.json")))


# ── the bars ──────────────────────────────────────────────────────────────────────────────────────
#
# Named, not inlined, because a reader deciding whether to trust a violation needs to see what it was
# measured against. Each carries the reasoning that set it where it is.

# How far the sustained figure may sit above the peak before it is a defect rather than window noise.
#
# NO PER-CELL CONSUMER ANY MORE. `check_sustained_not_above_peak` read this bar; the pair it policed
# (`rps_max_proxy` vs `rps_sustained_20ms`) is deleted, so the bar's only remaining job in this file is
# the retired drift gate (see the block below), which was about two literals in two
# languages and not about any cell's data. It stays declared, with its reasoning intact, because the
# site still declares its own copy and a gate that stops being able to compare is the defect class this
# file exists to prevent. See the removal note at `check_sustained_not_above_peak`'s old site.
#
# The two numbers now come out of ONE climb over ONE state of the gateway (`run::sweep_cell`), so a
# genuine inversion means the throughput curve spiked between two doublings - which gateways do not
# do. Before that change they were two searches separated by a gateway restart, and three cells of
# the 2026-07-28 board published a "sustained" rate up to 7% above the "maximum" it was meant to sit
# under. This stays at 5% rather than 0 because the ceiling is refined BETWEEN rungs and its rate is
# a median of three windows there, so a point or two of disagreement is measurement, not a bug.
#
# THE GROSS-INVERSION CEILING IS RETIRED, and with it the cross-language drift gate that policed it.
#
# `C6_GROSS_PCT = 5.0` capped how much window noise could excuse a `rps_sustained_20ms` figure sitting
# ABOVE the `rps_max_proxy` it was meant to sit under. Both fields are deleted. The inversion is not
# merely bounded now, it is UNREPRESENTABLE: the frontier is six maxima over sets that only grow as the
# bound relaxes, so a looser reading cannot come out below a tighter one for any input at all.
#
# The gate went with it, and that is the point rather than an omission. `check_c6_bar_agrees_with_the_site`
# parsed the site's own `export const C6_GROSS_PCT = 5;` and failed on disagreement - a good mechanism,
# for a constant that governed something. Kept after the constant stopped governing anything, it would
# have been a gate policing agreement between two dead literals: exactly the decoration this file's
# whole job is to find, and which it has caught twice elsewhere (a phantom phase weight, a trigger
# listing a step no workflow ran).
#
# What replaced the invariant: `check_frontier_rates_are_monotone_in_the_bound` asserts the ordering
# over all six readings, and `check_frontier_is_rederivable_from_its_sweep` recomputes every one of them
# from the raw rungs - which catches a mis-ordered pair and much more besides, with no tolerance to tune.

# The engine's own declaration of the field list ABSENCE_CARRYING_FIELDS claims to mirror. Same
# reasoning as SITE_C6_PATH/SITE_C6_RE just above: this is python parsing a sibling in another
# language rather than importing it, because there is no build step to share the list through
# either. See check_absence_fields_mirror_the_engine() for what happens when the two disagree.
RECORD_RS_PATH = os.path.join("engine", "src", "record.rs")
RECORD_RS_STRUCTS = {"perf": "CellPerf", "stream": "CellStream", "memory": "CellMemory"}
_RUST_IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")

# ── the frontier ──────────────────────────────────────────────────────────────────────────────────
#
# The tail-latency bounds every served cell must publish a reading at, in microseconds, ascending, with
# the UNBOUNDED reading (`p99_bound_us: null`, failures only, no latency claim) last. This is the
# engine's `frontier::P99_BOUNDS_US`, restated here for the same reason ABSENCE_CARRYING_FIELDS restates
# record.rs's field lists: there is no build step to share a constant across the two languages. And for
# the same reason it is CROSS-CHECKED rather than trusted - `check_frontier_bounds_agree_with_the_engine`
# parses the rust declaration and fails on drift, because a python copy that fell behind would let a
# board publish five columns while this file cheerfully audited four.
#
# WHY A LIST AND NOT "whatever the artifact carries": the length and the ordering are the invariant. A
# cell publishing four readings, or the same five bounds in a different order, silently changes what the
# board means without any single number being wrong - see `check_frontier_is_complete`.
P99_BOUNDS_US = [1_000, 5_000, 10_000, 50_000, 100_000]
FRONTIER_RS_PATH = os.path.join("engine", "src", "frontier.rs")
FRONTIER_RS_RE = re.compile(r"pub const P99_BOUNDS_US:\s*\[u64;\s*\d+\]\s*=\s*\[([^\]]*)\]\s*;")

# A rate this far above its own concurrency is not a proxy measurement.
#
# One connection cannot issue 20000 requests per second against a real socket; a number that says so
# is a units error or a counted retry, not a gateway. Deliberately loose - this catches the class of
# defect where a rate is divided by the wrong thing, not marginal optimism.
MAX_RPS_PER_CONNECTION = 20_000


def load(engine=None, gateway=None):
    """The newest snapshot per gateway, pinned to one engine so a board is audited as a board."""
    by_gw = {}
    for f in snapshot_paths():
        try:
            d = json.load(open(f))
        except Exception as e:
            print(f"  unreadable snapshot {f}: {e}", file=sys.stderr)
            continue
        gw = d.get("gateway")
        sha = ((d.get("rig") or {}).get("engine") or {}).get("commit") or ""
        if not gw or (gateway and gw != gateway):
            continue
        if engine and not sha.startswith(engine):
            continue
        by_gw[gw] = (f, d, sha)
    return by_gw


def newest_engine():
    """The engine commit of the snapshot with the newest `measured_at`, so the default audits the
    current board. Recency is the snapshot's own timestamp, not filename order - the files sort
    alphabetically by gateway, so "last file" is just "last gateway in the alphabet". A snapshot
    whose `measured_at` is missing or unparseable falls back to its file mtime.
    """
    best = None  # (when, sha)
    for f in snapshot_paths():
        try:
            d = json.load(open(f))
        except Exception:
            continue
        sha = ((d.get("rig") or {}).get("engine") or {}).get("commit") or ""
        if not sha:
            continue
        when = None
        raw = d.get("measured_at")
        if isinstance(raw, str):
            try:
                when = datetime.datetime.fromisoformat(raw.rstrip("Zz"))
            except ValueError:
                when = None
        if when is None:
            when = datetime.datetime.fromtimestamp(os.path.getmtime(f), datetime.timezone.utc)
        # Normalise to naive UTC so a mix of offset-carrying stamps and mtime fallbacks compares.
        if when.tzinfo is not None:
            when = when.astimezone(datetime.timezone.utc).replace(tzinfo=None)
        if best is None or when > best[0]:
            best = (when, sha)
    return best[1] if best else None


def served_cells(d):
    """Every cell the gateway actually served, with its coordinates for naming a violation."""
    for eg, blk in ((d.get("matrix") or {}).get("upstreams") or {}).items():
        for ing, c in (blk.get("cells") or {}).items():
            if c.get("served") is True:
                yield f"{ing}>{eg}", c


# ── the invariants ────────────────────────────────────────────────────────────────────────────────
#
# Each takes one served cell and yields a string per violation. A check that can never yield is a
# check that is not doing anything, which is the defect class this whole file is about: if one of
# these stops firing on data that used to trip it, that is a finding, not a pass.


# REMOVED: check_sustained_not_above_peak. It compared `perf.rps_sustained_20ms` against
# `perf.rps_max_proxy` and allowed C6_GROSS_PCT of window noise between them. BOTH FIELDS ARE DELETED,
# and the state it guarded - a gated reading coming out ABOVE the ungated one over the same windows - is
# now unrepresentable rather than unchecked. It was reachable because the two numbers came from two
# different algorithms over the same rungs (a plateau search that quit on three flat rungs, and a gate
# bisection that walked past where the plateau search had stopped), so the "maximum" could be a max over
# a SUBSET of what the "sustained" figure searched. It fired for real: aisix openai-responses>anthropic
# published 16,232 max against 16,610 sustained, and bifrost openai-responses>openai-responses 5,113
# against 5,174. The frontier is six readings of ONE rung set, each a maximum over a set that only GROWS
# as the bound relaxes, so the inversion class is structural nonsense now - and the ordering it implied
# is asserted anyway, over all six readings rather than two, by
# `check_frontier_rates_are_monotone_in_the_bound`. See `engine/src/frontier.rs`'s module note.

# REMOVED: check_peak_came_from_its_own_sweep. It held `perf.rps_max_proxy` to `max(sweep_max_proxy)`,
# which is one direction of one comparison against one deleted scalar. Its successor is strictly
# stronger and not a rewrite of it: `check_frontier_is_rederivable_from_its_sweep` recomputes EVERY
# reading from the rungs - the qualifying set, the maximum over it, the concurrency it was observed at,
# the tail that came with it and the boundary rung above it - and demands equality in both directions,
# where this only caught a peak that was too HIGH. A peak that was too LOW (the actual field defect: a
# plateau search that stopped early) passed this check every time.


def check_sweep_carries_its_latency(name, c):
    """Every throughput window must publish the p99 it ran at.

    THIS IS THE REGRESSION GUARD FOR THE DEFECT THAT CAUSED THE RERUN. The load generator computed a
    p99 for every window and always had; `SweepProbe::probe` narrowed its result to a rate and a
    verdict and dropped it. Because the latency was gone, the sustained figure could not be read off
    the throughput sweep, so the engine measured the cell a SECOND time to get it - minutes later,
    across a gateway restart. Thirty-three readings per cell were paid for and discarded in every run
    the project ever published. If this check ever passes vacuously again, the second search comes
    back with it.
    """
    pts = (c.get("perf") or {}).get("sweep_max_proxy") or []
    if not pts:
        return
    withp99 = sum(1 for r in pts if r.get("p99_us") is not None)
    if withp99 == 0:
        yield f"{name}: all {len(pts)} throughput windows published without the p99 they measured"


def check_ttft_percentiles_are_ordered(name, c):
    """p99 cannot sit below p50 when both come from one sample set.

    Distinct from a difference-of-percentiles, which genuinely has no ordering: `added_gap_p99_us`
    below `added_gap_p50_us` is legal, because each is an independent difference between two legs'
    matched percentiles. A guard that conflated the two fired on plano and would have failed the
    build for all fourteen gateways.
    """
    st = c.get("stream") or {}
    a, b = st.get("added_ttft_p50_us"), st.get("added_ttft_p99_us")
    if a is not None and b is not None and b < a:
        yield f"{name}: added_ttft p99 {b} sits below p50 {a}"


# REMOVED: check_rate_and_concurrency_travel_together. It paired `rps_sustained_20ms` with
# `rps_sustained_20ms_concurrency` and `rps_max_proxy` with `conc_at_peak`, and the failure it guarded was
# a rate published with the concurrency it was observed at MISSING - unre-derivable, unchartable. All
# four fields are deleted, and the state is unreachable by construction rather than by policing: a rate
# and its concurrency are no longer two sibling keys that can drift apart, they are two fields of ONE
# `FrontierReading` object emitted from one `frontier::Reading`, which cannot exist without both. What
# CAN still go wrong is a reading whose halves disagree with the rungs, and that is checked head-on by
# `check_frontier_is_rederivable_from_its_sweep` (the concurrency must name a rung that actually carried
# the winning rate at that bound), plus `check_every_absent_frontier_reading_has_a_reason` for the
# absent case.

# REMOVED (RE-POINTED, NOT DROPPED): check_rate_is_physically_possible read `rps_max_proxy / conc_at_peak`
# against MAX_RPS_PER_CONNECTION. Both fields are deleted, but the defect class - a rate divided by the
# wrong thing - is a property of ANY published rate/concurrency pair and did not go away with them, so it
# is re-pointed at all six readings by `check_frontier_rate_is_physically_possible` below. That is more
# coverage than before, not less: the old check looked at one pair per cell, the new one at up to six.

# REMOVED: check_frames_have_a_stream_behind_them. It fired when `stream.cpu_fps` was published beside a
# MEASURED `streams_sustained` of 0 - a frames/sec rate over a population of zero streams. `cpu_fps` and
# its `cpu_fps_concurrency` / `sweep_cpu_fps` are deleted, so there is no longer any frame rate in the
# artifact that can be divided by a stream count: `streams_sustained_fps` is the frame rate OF
# `streams_sustained` itself, measured in the same window rather than by a separate climb, so it cannot
# be a rate over a population it was not measured against. Note the check had already been narrowed once
# (litellm-rust anthropic>anthropic 2026-07-29) to a MEASURED 0 rather than a reasoned absence - the
# residual risk that survived that narrowing was exactly the two-searches-two-populations shape, and the
# second search is gone.


# THE DEFINITION OF DONE, as fields. Every metric a served cell's block may publish; a null on any
# of these with no `absences` entry beside it is a bare hole, which the board's owner has ruled out:
# "either this cell is measured and all data must be reported, or this cell wasn't tested and empty
# is expected - not a combo". Mirrors the engine's `absences()` lists in `engine/src/record.rs`
# (CellPerf, CellStream, CellMemory), field for field.
ABSENCE_CARRYING_FIELDS = {
    "perf": [
        # THE THROUGHPUT SCALARS ARE NOT LISTED HERE ANY MORE, and their absence is checked, not
        # assumed: `rps_sustained_20ms` / `rps_sustained_20ms_concurrency` / `conc_at_sustained` /
        # `rps_max_proxy` / `rps_max_proxy_concurrency` / `conc_at_peak` are deleted from `CellPerf`, so
        # keeping them here would make `check_absence_fields_mirror_the_engine` fail as "policing fields
        # the engine no longer defines" - which is exactly what it did before this list was cut, and is
        # the reason that check exists.
        #
        # WHAT REPLACES THEM IS NOT A FIELD ON THIS LIST. `perf.frontier` is a Vec, unreachable by the
        # engine's `absences_of!` macro (which walks scalar `Measurement` fields), so `CellPerf::absences`
        # populates its keys in a hand-written loop under BOUND-keyed names - `perf.frontier.10ms.rps`,
        # `perf.frontier.unbounded.rps`. Adding "frontier" here would break the mirror check in the other
        # direction and would police the wrong shape anyway; the frontier's own null-carries-a-reason
        # invariant is `check_every_absent_frontier_reading_has_a_reason`.
        "added_latency_p50_us", "added_latency_p99_us", "gateway_c1_p99_us", "direct_c1_p99_us",
        # THE COST GROUP. Every one of these can be absent for a reason a reader must see, and two of
        # those reasons are REFUSALS rather than gaps: a window with any failure publishes no cost
        # (CPU divided by only the successes would describe the failures, not the work), and a window
        # on a swapping box has its cost marked a harness fault. A null with the reason missing reads
        # as "not implemented" when the truth is "measured, and deliberately withheld".
        #
        # This list fell behind the engine when the fields were added, and the mirror check caught it
        # exactly as designed - reporting that every check built on this list was silently blind to
        # nine fields. That is the second cross-file mirror the cost work has broken (bench-dashboard
        # PHASE_ORDER was the first), and both were caught by their own guard rather than by review.
        "cpu_us_per_request", "rps_per_cpu_second", "cost_window_conc", "cost_window_ok",
        "cost_window_rps", "cost_core_utilisation", "cost_threads",
        "cost_nonvol_ctxt_per_request", "cost_majflt",
    ],
    "stream": [
        # `cpu_fps` and `cpu_fps_concurrency` are deleted from `CellStream` (with `sweep_cpu_fps`), same
        # reasoning as the perf block above: a field the engine no longer defines cannot be policed here.
        "added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us", "added_gap_p99_us",
        "streams_sustained", "streams_sustained_fps",
        # The WEIGHT behind the two added-TTFT percentiles. A failed probe used to be dropped inside a
        # filter_map, so a p99 over three lucky samples published identically to one over a hundred -
        # and with a single survivor the p50 and p99 ranks collapse to the same index, which reads as a
        # coherent pair. These carry the count so a reader can weigh the percentile beside them.
        "ttft_gw_samples", "ttft_direct_samples",
    ],
    "memory": [
        "idle_rss_mib", "steady_state_rss_mib", "recovered_rss_mib", "peak_rss_mib",
        "peak_rss_hwm_mib", "time_to_plateau_s", "growth_rate_mib_per_min",
        # Newly coverable: these were bare `Option`s that collapsed the metric group's reason on the
        # way out, so a memory window that could not judge the plateau published two nulls nothing
        # could explain. They are `Measurement`s now and ride in the cell's absences map like every
        # other number, which is what lets this list hold them to the same bar.
        "plateaued", "load_s",
        # Absent BECAUSE the measurement succeeded: these describe HOW a window failed to settle, so a
        # window that DID settle has no shape to publish and says exactly that. They are listed here
        # for the reason every other field is - a null must carry a reason - and NOT held to
        # check_no_bare_absence's "a served cell publishes a number", which is why that check reads
        # SHAPE_FIELDS below and lets a reasoned absence stand.
        "shape", "idle_shape",
        # The idle window's own verdict and its fitted slope. The memory group measured both on every
        # served cell and CellMemory had nowhere to put them, so they reached the artifact as neither
        # a number nor a null - a missing key, invisible to every check built on this list. They are
        # ordinary numeric metrics (a settled idle window publishes 1.0 and its slope), so unlike the
        # shape fields they ARE held to 'a served cell publishes a number'.
        "idle_static", "idle_growth_rate_mib_per_min",
    ],
}

# The fields whose absence is a RESULT rather than a gap. See the note in ABSENCE_CARRYING_FIELDS.
SHAPE_FIELDS = {"memory": {"shape", "idle_shape"}}

# The absence reasons that legitimately excuse a CAPACITY metric from being a number. Everything
# else must be a number - a gateway that failed the gate at every rung is a measured 0, not a hole.
CAPACITY_ABSENCE_OK = {"untestable", "rig_limited", "search_exhausted", "harness_error", "not_served"}


def check_no_bare_absence(name, c):
    """0 is a number; a bare n/a is not. Every null metric on a served cell carries its reason.

    The 2026-07-28 board's defining defect: holes with no stated cause, indistinguishable from cells
    that never ran. The engine has always written the reason into the cell's `absences` map; this
    pins that the map actually covers every null the cell publishes, so no consumer ever has to
    render an unexplained blank.
    """
    absences = c.get("absences") or {}
    for block, fields in ABSENCE_CARRYING_FIELDS.items():
        blk = c.get(block) or {}
        for f in fields:
            if f in blk and blk[f] is None and f"{block}.{f}" not in absences:
                yield f"{name}: {block}.{f} is null with NO reason in absences (a bare hole)"


def check_declared_fields_are_carried(name, c, known=None):
    """A served cell must CARRY every field it declares - as a number, or as a null with a reason.

    THE HOLE THIS CLOSES IS THE ONE `check_no_bare_absence` CANNOT SEE (ledger TOOL-04). That check
    reads `f in blk and blk[f] is None`, so it polices a field that is PRESENT and null. A field
    OMITTED FROM THE BLOCK ENTIRELY is invisible to it - and to every other check here, all of which
    reach for values with `.get()` and get None back whether the serializer wrote a null or wrote
    nothing at all. "Key missing" and "measured" are indistinguishable to the whole file, which means
    the audit's answer to "is every metric accounted for?" was really "is every metric that happened
    to be serialized accounted for?".

    That is not hypothetical arithmetic: today the engine serializes all of these unconditionally (no
    `skip_serializing` anywhere in record.rs), so the board is honest. One `#[serde(skip_serializing_if
    = "Option::is_none")]` on a Measurement field - the single most ordinary thing anyone would add to
    trim an artifact - would drop the key, and the entire absence discipline would evaporate silently
    while this audit kept printing PASS. The board's rule is "either this cell is measured and all
    data must be reported, or this cell wasn't tested"; a missing key is neither, so it is a
    violation, and the shape of the artifact stops being a matter of which code path the serializer
    took.

    The block itself is held to the same standard. A served cell with no `stream` object at all is not
    a quieter version of a cell whose streaming legs failed - it is the same claim with the evidence
    deleted, and `c.get(block) or {}` elsewhere in this file would read it as a block full of nulls
    with nothing to explain them.
    """
    for block, fields in ABSENCE_CARRYING_FIELDS.items():
        blk = c.get(block)
        if not isinstance(blk, dict):
            yield (f"{name}: served cell publishes NO {block} block at all - a served cell carries "
                   f"its declared fields or states why they are absent, it does not omit them")
            continue
        for f in fields:
            if f not in blk:
                # A field the PRODUCING ENGINE never had is a different thing from one it dropped.
                # `known` is computed per snapshot from that snapshot's own cells: a field no cell in
                # it carries was not in the engine that wrote it, and demanding it would be demanding
                # that yesterday's artifact contain tomorrow's field. Crucially this is all-or-
                # nothing per snapshot, so the defect this check exists for - a serializer that
                # started dropping a key on SOME cells - still fails loudly, because those cells sit
                # beside cells that carry it. A snapshot that omits a field everywhere is DISCLOSED
                # at the end of the run instead, never silently forgiven.
                if known is not None and f not in known.get(block, ()):
                    continue
                yield (f"{name}: {block}.{f} is OMITTED from the block (not null-with-reason, "
                       f"absent) - key-missing and measured are indistinguishable to every check")


def check_stream_capacity_is_a_number(name, c):
    """A streaming cell's capacity metrics are numbers (0 included), or a rig-class absence.

    The yield gate: streams_sustained and cpu_fps produced values (or measured zeroes) on every
    served streaming cell once the gate published failures as 0. An absence whose reason is
    `not_measured` here means a search quietly stopped producing - the exact silent-yield defect
    that shipped a board with cpu_fps on 1 of 16 served cells.

    `cpu_fps` IS NO LONGER IN THE FIELD LIST BELOW - it is deleted from `CellStream`. The silent-yield
    defect it was watched for is still live for `streams_sustained`, which is a bisection that can stop
    producing, so this check keeps its job with a shorter list rather than being removed. The historical
    1-of-16 board is what motivated it and is left in the paragraph above because it is why the bar is
    "a number or a reasoned absence" rather than "a value if the search felt like it".
    """
    st = c.get("stream") or {}
    if st.get("stream_served") is not True:
        return
    absences = c.get("absences") or {}
    for f in ("streams_sustained",):
        if st.get(f) is None:
            entry = absences.get(f"stream.{f}") or {}
            reason, detail = entry.get("reason"), entry.get("detail")
            if reason in CAPACITY_ABSENCE_OK:
                continue
            # AN ABSENCE THAT EXPLAINS ITSELF IS NOT A SILENT YIELD. The defect this catches is a
            # search that stopped producing and said nothing - the board that shipped cpu_fps on 1 of
            # 16 served cells. "The bisection proved c=6144 and could not reconfirm it" is the
            # opposite: a search that ran, failed to establish a ceiling, and published why. Requiring
            # a measured 0 there would force a number onto a question that was genuinely not settled,
            # which is the fabrication this whole file exists to prevent.
            if detail:
                continue
            yield (f"{name}: stream.{f} is absent with reason {reason!r} and NO detail on a served "
                   f"streaming cell - a search that stops producing must say why")


# ── the SEARCH TRACE's invariants ────────────────────────────────────────────────────────────────
#
# EVERY CHECK ABOVE READS THE PUBLISHED NUMBER. None of them read `sweep_streams`, the trace of the
# search that produced it - and that is exactly how this file returned PASS ten times over a board in
# which eight cells had silently given up. `check_stream_capacity_is_a_number` even accepts those on
# purpose: an absence carrying prose ("the bisection proved c=6144 and could not reconfirm it") reads
# as a search that ran, failed honestly, and said why. It cannot tell that apart from a search that
# quit with most of its budget unspent, because it never looks at what the search actually did.
#
# These checks close that. The bar is the same as the frontier's: re-derive from the evidence and
# demand agreement, rather than trust the summary. The trace is published on every streaming cell, so
# the search's own behaviour is auditable from committed JSON with no rig and no box - which is where
# every defect of this class should have been caught, and was not.
#
# Old snapshots predate the typed-error and host-state fields; a check that needs them skips rather
# than fails, because absent evidence is not evidence of a defect.


def stream_trace(c):
    """The rungs a streaming cell's search actually probed, in order, or None."""
    st = c.get("stream") or {}
    if st.get("stream_served") is not True:
        return None
    sw = st.get("sweep_streams")
    return sw if isinstance(sw, list) and sw else None


def proven_clean_top(rungs):
    """The highest concurrency the UNCONTAMINATED ascending prefix carried - every rung from the
    first up to and including it passed, before anything in this cell had failed."""
    top = 0
    for r in rungs:
        if r.get("passed") is not True:
            break
        top = max(top, r.get("conc") or 0)
    return top


# Mirrors engine/src/run.rs's MAX_CEILING_STEPDOWNS. A copy rather than a read because this file
# audits committed artifacts and must not depend on the engine source that produced them.
MAX_CEILING_STEPDOWNS = 4


def check_search_spent_its_budget(name, c):
    """A search that publishes nothing must have RUN OUT, not stopped early.

    plano `anthropic>anthropic` is the case: the ascending sweep carried c=64, the bisection settled
    on c=79, confirmation failed, the step-down bisected to c=71, its first window failed - and the
    search ended there, with every concurrency between 64 and 71 untried and most of its step-down
    budget unspent. A rung is confirmed by MAJORITY, not by its first window (one-api's published 266
    came from `[pass, pass, fail]`), so one failing window is a vote, not a verdict.

    The signature is a gap: the search stopped at a rung strictly above a concurrency it had already
    carried, having probed nothing in between. That is room it declined to use, and the cell paid for
    it with an absence where a number was available.
    """
    rungs = stream_trace(c)
    if not rungs:
        return
    st = c.get("stream") or {}
    if st.get("streams_sustained") is not None:
        return
    clean = proven_clean_top(rungs)
    if clean <= 0:
        return
    # WHERE THE SEARCH STOPPED, not the smallest rung it ever probed. Taking the minimum over every
    # rung included the ascending prefix's own c=1 and c=2, so `lowest` was always <= clean + 1 and
    # this check returned early on every cell in existence - it had never fired once, including on
    # the plano cell it was written from. Caught by its own reject fixture in bench-audit_test.py,
    # which is the entire reason that file demands one.
    lowest = rungs[-1].get("conc") or 0
    if lowest <= clean + 1:
        return                                   # it walked down to (or below) what it had carried
    # A SEARCH THAT SPENT ITS DECLARED BUDGET DID NOT GIVE UP - it ran out, which is a different
    # finding and not a defect. plano anthropic>anthropic on the 2026-08-01 run is the case: the
    # bisection proved c=224, confirmation failed, and the step-down walked 176 -> 152 -> 140 -> 134,
    # four rungs, exactly MAX_CEILING_STEPDOWNS. This check could only see the gap between the last
    # rung and the carried one, so it read a fully-spent budget as an abandoned one and would have
    # stopped a 14-box run for it.
    #
    # Counting distinct concurrencies probed BELOW the bisected ceiling is the budget as the trace
    # shows it. At the cap the honest reading is "our search range was too small for this cell",
    # which belongs in the run's write-up, not in a defect report.
    stepped = {r.get("conc") for r in rungs if clean < (r.get("conc") or 0)}
    walked_down = len([c for c in stepped if c < max(stepped)])
    if walked_down >= MAX_CEILING_STEPDOWNS:
        return
    between = [r for r in rungs if clean < (r.get("conc") or 0) < lowest]
    if not between:
        yield (f"{name}: the search published nothing but stopped at c={lowest} while it had already "
               f"carried c={clean}, probing none of the {lowest - clean - 1} concurrencies between - "
               f"it gave up with budget unspent, and a majority-confirmed rung may well be in there")


def check_no_rung_fails_below_one_already_carried(name, c):
    """A rung cannot fail below one the same cell has already carried cleanly - unless something
    other than the gateway changed, and then the absence must SAY so.

    This is the signature that took six cells off the 2026-07-31 board. It is either our rig failing
    to drain between windows or a gateway that stopped serving after an overload, and both are
    findings; what is not acceptable is publishing it as an ordinary failure with no attribution.
    """
    rungs = stream_trace(c)
    if not rungs:
        return
    clean = proven_clean_top(rungs)
    if clean <= 0:
        return
    below = [r.get("conc") for r in rungs if r.get("passed") is not True and (r.get("conc") or 0) <= clean]
    if not below:
        return
    absences = c.get("absences") or {}
    detail = ((absences.get("stream.streams_sustained") or {}).get("detail") or "").lower()
    named = any(k in detail for k in ("already carried", "did not recover", "drain", "rig", "restart"))
    if not named:
        yield (f"{name}: rung(s) {sorted(set(below))} failed at or below c={clean}, which this cell had "
               f"already carried cleanly - impossible for the gateway alone, and the absence does not "
               f"name what else changed")


def check_published_rung_held_a_majority(name, c):
    """The published sustained figure must be a rung whose windows in the trace actually held.

    The engine confirms by majority and the trace records every window, so this is re-derivable. A
    published rung that lost its own windows would mean the summary and the evidence disagree - the
    one defect the design cannot rule out structurally.
    """
    rungs = stream_trace(c)
    if not rungs:
        return
    st = c.get("stream") or {}
    sus = st.get("streams_sustained")
    if sus is None:
        return
    at = [r.get("passed") is True for r in rungs if (r.get("conc") or 0) == sus]
    if not at:
        yield (f"{name}: publishes streams_sustained={sus} but the trace contains no window at that "
               f"concurrency - the number did not come from this sweep")
        return
    if sum(at) * 2 <= len(at):
        yield (f"{name}: publishes streams_sustained={sus} on {sum(at)} of {len(at)} windows - the "
               f"engine's own rule is a majority, so this rung was not confirmed")


def check_a_wedged_gateway_is_named_as_one(name, c):
    """A gateway that stops serving and never resumes must be reported as that, not as a bare absence.

    aisix carried every rung to c=8,192, was pushed to c=16,384, then failed seventeen consecutive
    windows including c=4,096 which it had just served. "It does not recover from overload" is the
    single most useful thing a reader could learn from that cell, and it was published as silence.
    """
    rungs = stream_trace(c)
    if not rungs or len(rungs) < 6:
        return
    tail = [r.get("passed") is True for r in rungs[-5:]]
    if any(tail):
        return
    absences = c.get("absences") or {}
    detail = ((absences.get("stream.streams_sustained") or {}).get("detail") or "").lower()
    if "recover" not in detail and "restart" not in detail:
        yield (f"{name}: the last 5 rungs all failed - the gateway stopped serving and did not come "
               f"back - but the absence does not say so")


def check_no_rig_side_error_is_charged_to_the_gateway(name, c):
    """A connection this HOST could not make is never the gateway's error.

    The engine discards a whole window on `RigExhausted`, so a counted connect-failure means the peer
    refused - but the typed breakdown is what makes that checkable rather than assumed. Skipped on
    snapshots that predate the typed fields.
    """
    rungs = stream_trace(c)
    if not rungs:
        return
    if not any("stream_errors_connect_failed" in r for r in rungs):
        return
    bad = [(r.get("conc"), r.get("stream_errors_connect_failed")) for r in rungs
           if (r.get("stream_errors_connect_failed") or 0) > 0]
    for conc, n in bad:
        yield (f"{name}: c={conc} counted {n} connect-failure(s) against the gateway - a connection "
               f"that was never made is not a stream the gateway failed to serve")


TRACE_CHECKS = [
    check_search_spent_its_budget,
    check_no_rung_fails_below_one_already_carried,
    check_published_rung_held_a_majority,
    check_a_wedged_gateway_is_named_as_one,
    check_no_rig_side_error_is_charged_to_the_gateway,
]


# ── the frontier's invariants ─────────────────────────────────────────────────────────────────────
#
# `perf.frontier` is SIX READINGS OF ONE RUNG SET, published beside the rung set itself
# (`perf.sweep_max_proxy`), which is what makes every one of them re-derivable rather than asserted -
# and re-derivation is the check that matters here. The old scalars could only be sanity-checked
# (is the peak at least as big as the sustained figure? is the peak a rate some window produced?)
# because the algorithm that produced them had thrown away the evidence: a plateau search's stopping
# point is not in the artifact. The frontier's arithmetic is `max(rps) over {rungs that qualify}`, every
# input to it is published, so this file can run that arithmetic itself and demand the same answer.
#
# The bar for these is deliberately "recompute and compare", not "looks plausible". A summary that
# disagrees with the rungs it claims to summarise is the one defect the frontier design cannot rule out
# structurally - `frontier.rs` derives monotonicity from the algorithm, but nothing in the artifact
# proves the published readings CAME from that algorithm over these rungs. That proof is here.


def bound_key(us):
    """The name a reading's absences are filed under: `10ms`, or `unbounded` for the failure-only one.

    Mirrors `CellPerf::absences`'s `format!("{}ms", us / 1000)` exactly, because a key this file computes
    differently from the engine would look like a missing reason for every absent reading on the board -
    a false FAIL, which costs as much trust as a false pass.
    """
    return "unbounded" if us is None else f"{us // 1000}ms"


def frontier_of(c):
    """The cell's frontier readings, or [] when it has none (a pre-frontier artifact, or a shrunk one -
    `check_frontier_is_complete` is the only place allowed to tell those two apart)."""
    fr = (c.get("perf") or {}).get("frontier")
    return fr if isinstance(fr, list) else []


def sweep_rungs(c):
    """The rungs the frontier is read from. Every reading must be re-derivable from exactly these."""
    return [r for r in ((c.get("perf") or {}).get("sweep_max_proxy") or []) if isinstance(r, dict)]


def rung_served_cleanly(r):
    """Did the gateway serve every request it accepted at this rung? `frontier::Rung::served_cleanly`.

    ZERO FAILURES, NOT A TOLERANCE - the engine's rule, and the reason is that the rig's own refused
    connects never reach the rung (`GenStats::rig_refused` discards those windows), so a failure here is
    the gateway failing a request it accepted.

    EXACT NOW: `ok > 0 and fail == 0`, both read directly off the rung, because `SweepPoint` publishes
    `ok`. It did not used to: `SweepPoint` carried conc/rps/p99_us/fail only, so this file had to
    APPROXIMATE the engine's `ok > 0` half - first as `rps > 0` alone, which produced a real FALSE
    POSITIVE on live data. Plano at c=256 published `rps: 0, p99_us: 3398432, fail: 0`, and `rps > 0`
    called that window dirty. But a percentile cannot exist without a completed, timed request - so
    `ok >= 1`, the window served cleanly, and its rate merely rounded down through the engine's `as
    i64` (one request over a four-second window is 0.25 rps, published as 0). The engine was right and
    this check was wrong, and it mattered: the approximation then re-derived a
    `first_disqualified_conc` the engine had correctly left absent, and the disagreement between the
    two checkers had to be settled BY HAND. `SweepPoint` publishing `ok` is what removes that class of
    disagreement rather than merely widening the approximation again - there is no approximation left
    to widen.

    AND WHEN `ok` IS ABSENT ENTIRELY - a snapshot measured before the field existed - this FALLS BACK to
    the p99 rule rather than refusing. Refusing was the first shape of this fix, and it was worse: with
    `ok` unavailable every rung on such a board fails cleanliness-for-lack-of-proof, so the re-derivation
    check had to be skipped wholesale, and the STRONGEST invariant in this file went unaudited on every
    cell of the board about to ship. A skipped check is weaker than an approximate one. The fallback is
    the p99-as-proof-of-completion rule, which is WIDER than the engine's (a completion always leaves a
    latency sample; a rate can round away), so its residual error is a missed catch and never a false
    alarm - which is the only direction an approximation may err in a tool whose warnings must be
    trusted.

    An ABSENT `fail` is still not a clean rung: "measured nothing" and "measured no failures"
    are different facts, and a rung this file cannot prove clean does not get to count as one. Every
    snapshot on disk as of 2026-07-29 predates `ok`, so this returns False for ALL of their rungs - see
    `producer_knew_ok` for how that is kept from reading as "every old rung was dirty" at the one place
    (`check_frontier_is_rederivable_from_its_sweep`) that would otherwise turn it into a false alarm.
    """
    ok, fail, p99 = r.get("ok"), r.get("fail"), r.get("p99_us")
    if not isinstance(fail, (int, float)) or fail != 0:
        return False
    if isinstance(ok, (int, float)):
        return ok > 0
    # `ok` absent: this rung predates the field. Fall back rather than refuse - see the docstring.
    return (isinstance(r.get("rps"), (int, float)) and r.get("rps") > 0) or isinstance(p99, (int, float))


def rung_qualifies(r, bound_us):
    """Does this rung count toward the reading at `bound_us`? `None` = the failure-only reading.

    `frontier::Rung::qualifies`, restated: clean, and under the bound STRICTLY (`p99 < bound`, so a rung
    sitting exactly on a bound does not clear it - "under 1 ms" means under). A rung with no p99 is
    disqualified from every BOUNDED reading, because a rung with no latency reading has not earned a
    claim about latency - but not from the unbounded one, which makes no latency claim to earn.
    """
    if not rung_served_cleanly(r):
        return False
    if bound_us is None:
        return True
    p99 = r.get("p99_us")
    return isinstance(p99, (int, float)) and p99 < bound_us


def top_probed_conc(rungs):
    """The highest concurrency the sweep asked for, qualifying or not - `Reading::top_probed_conc`. A
    rung that failed is still a rung we looked at, and is exactly what proves we did not stop early."""
    concs = [r.get("conc") for r in rungs if isinstance(r.get("conc"), (int, float))]
    return max(concs) if concs else None


def check_frontier_is_complete(name, c, frontier_known=False):
    """Every served cell publishes ALL SIX readings: the declared bounds ascending, unbounded last.

    A CELL PUBLISHING FOUR WOULD SILENTLY SHRINK THE BOARD. Nothing else here would notice: every other
    check iterates whatever readings are present and would pass over a cell missing its 1ms and 5ms
    columns, while the site would render the gateway as though those bounds had never been asked about.
    The engine writes an absent-with-reason reading for a bound nothing qualified at, precisely so that
    "no throughput under 1ms" and "we did not report 1ms" stay distinguishable - and this is the check
    that makes the difference visible instead of merely intended.

    THE ORDER IS PART OF THE INVARIANT, not presentation. `frontier.rs` derives monotonicity in the
    BOUND; a reader (and `check_frontier_rates_are_monotone_in_the_bound`) reads that off the sequence
    positionally, so a permuted sequence turns a true property into a false alarm or hides a real one.

    `frontier_known` IS HOW A PRE-FRONTIER ARTIFACT IS TOLD FROM A SHRUNK ONE - the same mechanism, and
    the same reasoning, as `fields_the_producer_knew`: ask the artifact. A snapshot where NO served cell
    carries a frontier was written by an engine that did not have the metric, and demanding it would be
    demanding that yesterday's artifact contain tomorrow's field; those are disclosed by count at the end
    of the run, never silently forgiven. A snapshot where SOME cell carries one knew how, so a cell
    without it dropped it, which is the defect.
    """
    fr = frontier_of(c)
    if not fr:
        # WITHHELD IS NOT DROPPED, and conflating them made this check cry wolf on the one cell on the
        # board where the harness behaved best.
        #
        # aisix openai-responses>openai answers 200, so `served` is True and its siblings all carry a
        # frontier - which is exactly the shape this check calls a drop. But EGRESS RE-VERIFICATION
        # PROVED THE GATEWAY NEVER TRANSLATED: the request arrived on the mock's openai-responses
        # endpoint and nothing arrived on openai, so it forwarded the ingress request unchanged. Every
        # number taken there describes a wire that is not openai-responses>openai, and the engine
        # therefore withheld the whole perf group and published `egress_reverified: false` plus the
        # evidence in `perf_dropped`. An empty frontier is the CORRECT artifact for that cell; demanding
        # one would be demanding a throughput figure for a translation that did not happen.
        #
        # So the exemption is not "trust the producer" - it is keyed on the producer having published
        # the DISCLOSURE. A cell that simply lost its frontier has no `egress_reverified: false` and
        # still fails, which is the case this check exists for.
        perf = (c.get("perf") or {}) if isinstance(c, dict) else {}
        if perf.get("egress_reverified") is False:
            return
        if frontier_known:
            yield (f"{name}: served cell publishes NO frontier while other cells in the same snapshot "
                   f"do - the producing engine had the metric, so this cell's throughput answer was "
                   f"dropped rather than never measured")
        return
    expected = list(P99_BOUNDS_US) + [None]
    got = [r.get("p99_bound_us") if isinstance(r, dict) else "<not an object>" for r in fr]
    if got != expected:
        yield (f"{name}: frontier publishes bounds {got} but the board is defined at {expected} "
               f"(the declared bounds ascending, then the unbounded reading) - a frontier of a "
               f"different shape is a different board")
    # EVERY FIELD PRESENT, not merely non-null. Same reasoning as check_declared_fields_are_carried: a
    # key the serializer omitted and a key it wrote as null are indistinguishable to every `.get()` in
    # this file, so a `skip_serializing_if` on any of these would evaporate the absence discipline while
    # the audit kept printing PASS.
    for r in fr:
        if not isinstance(r, dict):
            continue
        for f in ("p99_bound_us", "rps", "concurrency", "p99_us", "first_disqualified_conc",
                  "lower_bound"):
            if f not in r:
                yield (f"{name}: frontier reading at {bound_key(r.get('p99_bound_us'))} OMITS `{f}` - "
                       f"key-missing and measured are indistinguishable to every check here")


def check_frontier_rates_are_monotone_in_the_bound(name, c):
    """Relaxing the bound can never lower the reading.

    THIS IS STRUCTURAL IN THE ENGINE AND ASSERTED ANYWAY. A rung qualifies at bound B if its tail is
    under B and it failed nothing, so relaxing B only ADDS rungs to the qualifying set, and each reading
    is a maximum over that set - a max over a superset cannot be smaller. `frontier.rs` says so in its
    module note and its own test walks hostile inputs. So a violation here does NOT mean "the gateway
    behaved strangely"; it means the published readings did not come from the rungs they claim to
    summarise, which is the one thing the structure cannot rule out. An invariant nothing checks is an
    invariant nobody notices breaking - and the inversion class it descends from (`rps_max_proxy` below
    `rps_sustained_20ms`) shipped on two real cells before it was structural.

    An ABSENT looser reading beside a PRESENT tighter one is the same violation in its degenerate form:
    the looser bound's qualifying set contains the tighter one's, so it cannot be empty when that one
    was not.
    """
    prev_rps = prev_key = None
    for r in frontier_of(c):
        if not isinstance(r, dict):
            continue
        key = bound_key(r.get("p99_bound_us"))
        rps = r.get("rps")
        if rps is None:
            if prev_rps is not None:
                yield (f"{name}: frontier.{key} published NO rate while the tighter frontier."
                       f"{prev_key} published {prev_rps} - a looser bound's qualifying set contains "
                       f"the tighter one's, so it cannot be empty when that one was not")
            continue
        if prev_rps is not None and rps < prev_rps:
            yield (f"{name}: frontier.{key} reads {rps} rps, BELOW frontier.{prev_key}'s {prev_rps} - "
                   f"relaxing the bound lowered the reading, which the max-over-a-superset structure "
                   f"makes impossible, so these readings are not the rungs they claim")
        prev_rps, prev_key = rps, key


def check_frontier_is_rederivable_from_its_sweep(name, c, ok_known=True):
    """Recompute every reading from `sweep_max_proxy` and demand the published one matches.

    THE STRONGEST CHECK IN THIS FILE, and the only one that verifies the engine's ARITHMETIC rather than
    the plausibility of its summary. Each reading is `max(rps)` over the rungs that qualify at its bound,
    the concurrency that maximum was observed at, the tail THAT rung produced, and the lowest
    concurrency above it that stopped qualifying. All four inputs are published per rung, so all four
    outputs are recomputable, and a summary that disagrees with the rungs behind it fails here.

    It replaces `check_peak_came_from_its_own_sweep`, which only caught a peak that was too HIGH. The
    actual field defect was the opposite - a plateau search that stopped three flat rungs in and reported
    a maximum BELOW what its own sibling search reached - and that passed the old check every time.

    ONE PLACE WHERE THE ARTIFACT IS GENUINELY SHORT OF INFORMATION, handled rather than assumed away:
    rungs may repeat a concurrency, so ties are matched against the SET of rungs carrying the winning
    rate at the published concurrency rather than against one assumed argmax.

    `ok_known` IS NOW DISCLOSURE ONLY - IT NO LONGER SKIPS ANYTHING, and that reversal is the point.
    It first gated the whole re-derivation, on the reasoning that `rung_served_cleanly` reads `ok` and
    every snapshot predating that field would fail cleanliness for LACK OF PROOF, emptying every
    qualifying set and flagging honest rates as unbacked.

    That was true of the first shape of the fix and it traded one failure for a worse one: THE STRONGEST
    CHECK IN THIS FILE went unrun on every cell of the board about to ship, and a skipped check is weaker
    than an approximate one. `rung_served_cleanly` now FALLS BACK to the p99-as-proof-of-completion rule
    when `ok` is absent, which is wider than the engine's rule rather than narrower, so its residual
    error is a missed catch and never a false alarm. With that in place the re-derivation runs on every
    board, exactly or approximately, and `ok_known` only records WHICH it was so the run can say so.
"""
    fr = frontier_of(c)
    if not fr:
        return
    rungs = sweep_rungs(c)
    if not rungs:
        # A frontier with no rungs under it is a summary with its evidence deleted. The engine cannot
        # produce it - no rungs means every reading is an absence - so a published rate here is a number
        # from nowhere, which is worse than a missing one.
        published = [bound_key(r.get("p99_bound_us")) for r in fr
                     if isinstance(r, dict) and r.get("rps") is not None]
        if published:
            yield (f"{name}: frontier publishes rates at {published} but sweep_max_proxy carries NO "
                   f"rungs - the readings cannot be re-derived from anything")
        return
    for r in fr:
        if not isinstance(r, dict):
            continue
        bound, key = r.get("p99_bound_us"), bound_key(r.get("p99_bound_us"))
        quals = [q for q in rungs if rung_qualifies(q, bound)]
        best = max((q["rps"] for q in quals), default=None)
        pub_rps, pub_conc, pub_p99 = r.get("rps"), r.get("concurrency"), r.get("p99_us")
        if best is None:
            if pub_rps is not None:
                yield (f"{name}: frontier.{key} publishes {pub_rps} rps but NO rung in sweep_max_proxy "
                       f"qualifies at that bound (clean and under it) - the reading has no rung behind "
                       f"it")
            continue
        if pub_rps is None:
            # `best` IS EXACT NOW - including 0, which used to be the `ok` ambiguity this branch had to
            # excuse. A rung with `ok > 0` and a rate that rounded down to 0 (plano at c=256) is a
            # PROVEN clean rung with a real reading of 0, not a hole, so a published absence beside it
            # is a genuine disagreement rather than something this file cannot see.
            yield (f"{name}: frontier.{key} publishes no rate, but rung(s) in sweep_max_proxy "
                   f"qualify at that bound and the best carries {best} rps")
            continue
        if pub_rps != best:
            yield (f"{name}: frontier.{key} publishes {pub_rps} rps, re-derived from its own rungs it "
                   f"is {best} - the summary disagrees with the sweep it claims to summarise")
            continue
        winners = [q for q in quals if q.get("rps") == best]
        if pub_conc is None:
            yield (f"{name}: frontier.{key} publishes {pub_rps} rps with NO concurrency - the rate "
                   f"cannot be placed on the ladder it was observed on")
        else:
            at_conc = [w for w in winners if w.get("conc") == pub_conc]
            if not at_conc:
                yield (f"{name}: frontier.{key} says {pub_rps} rps at c={pub_conc}, but no rung at "
                       f"that concurrency both qualifies at the bound and carries that rate "
                       f"(qualifying winners sit at c="
                       f"{sorted({w.get('conc') for w in winners})})")
            elif all(w.get("p99_us") != pub_p99 for w in at_conc):
                # The tail must be the WINNING RUNG'S OWN. See `Reading::p99_us`: publishing anything
                # else - the bound especially - restates the question as though it were the answer.
                measured = sorted((w.get("p99_us") for w in at_conc),
                                  key=lambda v: (v is None, v))
                yield (f"{name}: frontier.{key} publishes p99 {pub_p99} but the qualifying rung(s) at "
                       f"c={pub_conc} measured {measured}")
        # THE BOUNDARY PROOF, re-derived: the lowest concurrency ABOVE the winner that stopped
        # qualifying. Absent is not a hole here - it is the positive finding that the sweep ran out of
        # range while this bound still held, which is why `first_disqualified_conc` is the one reading
        # field the engine deliberately keeps OUT of the absences map (see `CellPerf::absences`).
        if isinstance(pub_conc, (int, float)):
            above = [q["conc"] for q in rungs
                     if isinstance(q.get("conc"), (int, float)) and q["conc"] > pub_conc
                     and not rung_qualifies(q, bound)]
            expect_fd = min(above) if above else None
            got_fd = r.get("first_disqualified_conc")
            if got_fd != expect_fd:
                yield (f"{name}: frontier.{key} names first_disqualified_conc={got_fd}, re-derived "
                       f"from the rungs above c={pub_conc} it is {expect_fd} - the half of the "
                       f"reading's proof that says this really is the boundary")


def check_frontier_disclosure_agrees_with_the_ladder(name, c):
    """`lower_bound` is true exactly when the winning rung is the highest concurrency probed.

    IT IS A DISCLOSURE, AND A DISCLOSURE THAT DISAGREES WITH THE DATA IS WORSE THAN NONE. True means "we
    ran out of ladder, so this rate is a floor and not a ceiling"; false means "we probed higher and it
    was worse, so the peak is established". Publishing false where the sweep topped out overstates the
    finding on every surface that repeats it, and publishing true where throughput visibly turned over
    understates a real peak - which is exactly the bug `is_lower_bound` had when it read
    `first_disqualified_conc.is_none()`: the 2026-07-30 smoke run probed to c=256, peaked at c=32 with
    every rung above still holding a 5ms tail, and five of six readings claimed to be lower bounds.

    An ABSENT reading cannot be a lower bound of anything - there is no rate to qualify - so its flag
    must be false. The engine writes false there; this pins it, because a stray true would put a
    "measured at least this much" label on a column with no measurement in it.
    """
    fr = frontier_of(c)
    if not fr:
        return
    top = top_probed_conc(sweep_rungs(c))
    for r in fr:
        if not isinstance(r, dict):
            continue
        key, flag, conc = bound_key(r.get("p99_bound_us")), r.get("lower_bound"), r.get("concurrency")
        if r.get("rps") is None:
            if flag:
                yield (f"{name}: frontier.{key} has no rate but claims lower_bound=true - an absence "
                       f"is not a floor under anything")
            continue
        if conc is None or top is None:
            continue  # already a violation elsewhere; nothing to compare the ladder against
        expect = conc >= top
        if bool(flag) != expect:
            yield (f"{name}: frontier.{key} says lower_bound={flag} at c={conc} with the sweep's top "
                   f"rung at c={top} - " + ("the winner IS the top of the ladder, so the rate is a "
                   "floor and must say so" if expect else "the sweep probed past the winner, so the "
                   "peak is established and must not be published as a floor"))


def check_frontier_p99_is_the_observed_tail(name, c):
    """A bounded reading's `p99_us` is the tail its winning rung PRODUCED, never the bound restated.

    Qualification is strict (`p99 < bound`, `frontier::Rung::qualifies`), so for a reading that published
    a rate:

      * a tail ABOVE its own bound is a straight violation - that rung could not have qualified, so
        either the tail or the rate belongs to some other rung;
      * a tail EQUAL to its own bound is the same violation arithmetically (strict `<` excludes it) and
        is reported separately because it is also the signature of the specific defect worth naming: the
        bound being copied into the answer slot. `Reading::p99_us` exists to keep those apart - "a
        gateway holding 4 ms under a 100 ms bound is not the same finding as one sitting at 99 ms";
      * no tail at all, on a BOUNDED reading, is a latency claim with no latency reading behind it. Only
        the unbounded reading may publish an absent tail, and it may because it makes no such claim.
    """
    for r in frontier_of(c):
        if not isinstance(r, dict):
            continue
        bound = r.get("p99_bound_us")
        if bound is None or r.get("rps") is None:
            continue
        key, p99 = bound_key(bound), r.get("p99_us")
        if p99 is None:
            yield (f"{name}: frontier.{key} publishes a rate with NO p99 - a rung with no tail reading "
                   f"cannot qualify for a latency-bounded reading")
        elif p99 > bound:
            yield (f"{name}: frontier.{key} publishes p99 {p99}us, ABOVE its own {bound}us bound - the "
                   f"winning rung could not have qualified")
        elif p99 == bound:
            yield (f"{name}: frontier.{key} publishes p99 exactly {p99}us, its own bound - "
                   f"qualification is strictly under the bound, so this is the question restated as "
                   f"the answer, not a tail any rung measured")


def check_frontier_rate_is_physically_possible(name, c):
    """A per-connection rate above `MAX_RPS_PER_CONNECTION` is a units error, not a fast gateway.

    Inherited from `check_rate_is_physically_possible`, which read the deleted `rps_max_proxy` /
    `conc_at_peak` pair. Every reading carries its own rate and the concurrency it was observed at, so
    the same arithmetic now runs up to six times per cell instead of once.
    """
    for r in frontier_of(c):
        if not isinstance(r, dict):
            continue
        rps, conc = r.get("rps"), r.get("concurrency")
        if rps and conc and conc > 0 and rps / conc > MAX_RPS_PER_CONNECTION:
            yield (f"{name}: frontier.{bound_key(r.get('p99_bound_us'))} reads {rps:.0f} rps at "
                   f"c={conc}, which is {rps/conc:.0f} per connection")


def check_every_absent_frontier_reading_has_a_reason(name, c):
    """A null frontier reading carries its reason in the cell's absences map, keyed by its BOUND.

    THIS IS THE ONE STATE THE WHOLE `Measurement` DESIGN EXISTS TO PREVENT, and the frontier reopened it
    once already: a `Measurement` serializes an absence as a bare `null` and its reason lives in the
    cell's sibling `absences` map, populated by the engine's `absences_of!` macro - which walks scalar
    fields and so could not see a Vec. Until `CellPerf::absences` grew its hand-written loop, a bound
    nothing qualified at published `null` with its reason nowhere in the artifact at all.

    KEYED BY BOUND, NOT INDEX (`perf.frontier.10ms.rps`, `perf.frontier.unbounded.rps`): the index is an
    artifact of ordering, the bound is the identity. This check is therefore also what pins the two
    naming schemes together - if the engine ever switched to indices, every reason would be filed under a
    key nothing looks up, and a reader asking why the 10ms column is empty would find nothing.

    `first_disqualified_conc` IS DELIBERATELY EXEMPT. Its absence is not a hole but the positive finding
    that the sweep ran out of range while this bound still held, which the reading's own
    `lower_bound: true` states directly - and `CellPerf::absences` keeps it out of the map for exactly
    that reason, so requiring an entry here would fail every honest lower-bound reading on the board.
    """
    fr = frontier_of(c)
    if not fr:
        return
    absences = c.get("absences") or {}
    for r in fr:
        if not isinstance(r, dict):
            continue
        key = bound_key(r.get("p99_bound_us"))
        for f in ("rps", "concurrency", "p99_us"):
            if r.get(f) is not None:
                continue
            entry = absences.get(f"perf.frontier.{key}.{f}") or {}
            if not entry.get("reason"):
                yield (f"{name}: frontier.{key}.{f} is null with NO reason in absences under "
                       f"`perf.frontier.{key}.{f}` (a bare hole)")


def producer_knew_the_frontier(d):
    """Did the engine that wrote THIS snapshot publish frontiers at all?

    True when any served cell carries a non-empty `perf.frontier`. Same mechanism and same reasoning as
    `fields_the_producer_knew`: the artifact is asked, rather than a commit-to-field table nothing would
    keep honest. False means the snapshot predates the metric, its cells are disclosed as unaudited-for-
    the-frontier at the end of the run, and `check_frontier_is_complete` stays silent instead of printing
    a violation per cell for a field that could not have existed when the file was written.
    """
    return any(frontier_of(c) for _name, c in served_cells(d))


def producer_knew_ok(d):
    """Did the engine that wrote THIS snapshot publish `ok` per rung? Same mechanism and reasoning as
    `producer_knew_the_frontier` immediately above, one field lower: the artifact is asked, rather than
    a commit-to-field table nothing would keep honest.

    True when any rung on any served cell carries the key at all - `isinstance` is deliberately not
    checked here, an `ok` of the wrong TYPE is a shape violation for something else to catch, this
    function only asks whether the field exists in this snapshot's vocabulary. False means every rung
    in the snapshot predates `ok`, so `rung_served_cleanly` cannot be proven true OR false for any of
    them, and `check_frontier_is_rederivable_from_its_sweep` stays silent on that cell instead of
    flagging every honest published rate as unbacked by a qualifying set it can never fill. Every
    snapshot on disk as of 2026-07-29 is in this bucket, the same as every snapshot was pre-frontier
    before that metric shipped - and it clears the same way, cell by cell, as engines re-measure.
    """
    return any("ok" in r for _name, c in served_cells(d) for r in sweep_rungs(c))


def parse_rust_frontier_bounds(text):
    """The engine's own `P99_BOUNDS_US`, read out of frontier.rs. None when the declaration is not in
    the shape this expects - which the caller must treat as a failure, never as agreement, the same rule
    parse_site_c6 and parse_rust_absences follow."""
    m = FRONTIER_RS_RE.search(text or "")
    if not m:
        return None
    out = []
    for tok in m.group(1).split(","):
        tok = tok.strip().replace("_", "")
        if not tok:
            continue
        if not tok.isdigit():
            return None  # not a plain literal - the shape drifted, go blind rather than guess
        out.append(int(tok))
    return out or None


def check_frontier_bounds_agree_with_the_engine():
    """`P99_BOUNDS_US` here must be exactly the engine's `frontier::P99_BOUNDS_US` (ledger TOOL-02 shape).

    The third instance of this file's cross-language pattern, after the site's C6 bar and record.rs's
    absence field lists, and it exists for the sharpest version of the reason: the bounds decide how many
    columns a board HAS. If the engine added a 500ms bound and this list stayed at five, every cell would
    trip `check_frontier_is_complete` (loud, fine); if the engine DROPPED one and this list kept it, the
    completeness check would fail the whole board for a column nobody publishes any more - and if someone
    then "fixed" it by trimming the python list, the audit would stop noticing a shrinking board, which
    is the failure mode. Parse the sibling, fail on drift, and treat "cannot find the declaration" as
    drift rather than as agreement.
    """
    p = os.path.join(HERE, FRONTIER_RS_PATH)
    try:
        with open(p) as fh:
            text = fh.read()
    except OSError as e:
        yield (f"frontier bounds: cannot read the engine's declaration at {FRONTIER_RS_PATH} ({e}) - "
               f"the python bounds are {P99_BOUNDS_US}, and an unverifiable twin is not an agreeing one")
        return
    engine = parse_rust_frontier_bounds(text)
    if engine is None:
        yield (f"frontier bounds: {FRONTIER_RS_PATH} no longer declares "
               f"`pub const P99_BOUNDS_US: [u64; N] = [...];` where this can read it - the cross-check "
               f"went blind, which is a drift, not a pass")
        return
    if engine != P99_BOUNDS_US:
        yield (f"frontier bounds: python P99_BOUNDS_US={P99_BOUNDS_US} but {FRONTIER_RS_PATH} declares "
               f"{engine} - the audit and the engine disagree about how many columns the board has")


def fields_the_producer_knew(d):
    """Which declared fields THIS snapshot's engine actually serializes, read from the snapshot.

    The board is not always written by one engine - run N's artifacts outlive the commit that made
    them, and a field added afterwards cannot appear in them. Rather than trusting a commit-to-field
    mapping that nothing would keep honest, this asks the artifact: a field that appears on ANY served
    cell was known to the producer, so every OTHER cell must carry it too. That keeps the real defect
    (a key dropped on some cells but not others) failing, while a field uniformly absent is reported
    as an unaudited gap rather than 64 identical violations that drown out everything else.
    """
    known = {b: set() for b in ABSENCE_CARRYING_FIELDS}
    for _name, c in served_cells(d):
        for block, fields in ABSENCE_CARRYING_FIELDS.items():
            blk = c.get(block)
            if isinstance(blk, dict):
                known[block].update(f for f in fields if f in blk)
    return known


# The frontier's own invariants, listed apart from the rest so the run can SAY how many of them it
# skipped on a snapshot that predates the metric. `check_frontier_is_complete` is first on purpose: it is
# the one that decides whether this cell has a frontier to talk about at all, so its violation reads
# before the others' silence on an empty one.
FRONTIER_CHECKS = [
    check_frontier_is_complete,
    check_frontier_rates_are_monotone_in_the_bound,
    check_frontier_is_rederivable_from_its_sweep,
    check_frontier_disclosure_agrees_with_the_ladder,
    check_frontier_p99_is_the_observed_tail,
    check_frontier_rate_is_physically_possible,
    check_every_absent_frontier_reading_has_a_reason,
]

CELL_CHECKS = [
    check_sweep_carries_its_latency,
    check_ttft_percentiles_are_ordered,
    check_no_bare_absence,
    check_declared_fields_are_carried,
    check_stream_capacity_is_a_number,
] + FRONTIER_CHECKS + TRACE_CHECKS

def parse_rust_absences(text, struct_name):
    """The exact field list `struct_name::absences()` walks, read out of record.rs's own
    `absences_of!(self, ...)` invocation. None when the shape this expects - one
    `impl <struct_name> { ... absences_of!(self, a, b, c, ...) ... }` block, fields separated by
    commas, comments allowed between them - is not found. The caller must treat None as a failure,
    never as agreement: the same rule parse_site_c6's caller follows, and for the same reason. The
    macro's own argument list has no parentheses in it (only field identifiers and `//` comments), so
    a non-greedy match up to the first `)` after `absences_of!(self,` is exactly the call's closing
    paren, not a premature one hiding inside a comment.
    """
    m = re.search(rf"impl\s+{re.escape(struct_name)}\s*\{{.*?absences_of!\(\s*self\s*,(.*?)\)",
                  text or "", re.S)
    if not m:
        return None
    body = re.sub(r"//[^\n]*", "", m.group(1))  # strip line comments before splitting on commas
    fields = []
    for tok in body.split(","):
        tok = tok.strip()
        if not tok:
            continue
        if not _RUST_IDENT_RE.match(tok):
            return None  # not a bare identifier - the shape drifted, go blind rather than guess
        fields.append(tok)
    return fields


def check_absence_fields_mirror_the_engine():
    """ABSENCE_CARRYING_FIELDS must name EXACTLY the fields `CellPerf`/`CellStream`/`CellMemory`'s own
    `absences()` walk in the engine - not a superset, not a subset.

    THE HOLE (round-2 audit): the comment above ABSENCE_CARRYING_FIELDS has always claimed it mirrors
    record.rs "field for field", and nothing checked that claim. Deleting `cpu_fps_concurrency` from
    the stream list, or `plateaued`/`load_s` from the memory list, left bench-audit_test.py green,
    because that file's accept-side fixture is GENERATED FROM ABSENCE_CARRYING_FIELDS - it shrinks
    exactly in step with the list it is meant to be proving against, so a shrunk list always "agrees
    with itself" and check_declared_fields_are_carried never notices the field it stopped looking
    for. Only a check that reads the engine's OWN declaration, independent of this file's list, can
    catch that a field quietly stopped being policed - or that the engine grew one this list never
    learned about, which is the same hole from the other side: a field the engine now reports
    absences for, that this audit silently never checks for a bare null.

    Modeled on the retired C6-bar drift gate (ledger TOOL-02): parse the sibling's
    declaration rather than importing it, and treat "cannot find the shape expected" as a violation,
    not a pass. Going blind is a drift, not agreement - the established rule in this file.
    """
    p = os.path.join(HERE, RECORD_RS_PATH)
    try:
        with open(p) as fh:
            text = fh.read()
    except OSError as e:
        yield (f"absence fields: cannot read the engine's declaration at {RECORD_RS_PATH} ({e}) - "
               f"an unreadable twin is not an agreeing twin")
        return
    for block, struct_name in RECORD_RS_STRUCTS.items():
        engine_fields = parse_rust_absences(text, struct_name)
        if engine_fields is None:
            yield (f"absence fields: {RECORD_RS_PATH} no longer declares "
                   f"`impl {struct_name} {{ ... absences_of!(self, ...) ... }}` where this can read "
                   f"it - the cross-check went blind, which is a drift, not a pass")
            continue
        engine_set, python_set = set(engine_fields), set(ABSENCE_CARRYING_FIELDS.get(block, []))
        missing = engine_set - python_set  # the engine carries a field this audit never learned about
        extra = python_set - engine_set    # this audit polices a field the engine no longer carries
        if missing:
            yield (f"absence fields: {struct_name}::absences() carries {sorted(missing)} that "
                   f"ABSENCE_CARRYING_FIELDS['{block}'] does not - the python list has fallen behind "
                   f"the engine's own definition of done, and every check built on that list is "
                   f"silently blind to those field(s)")
        if extra:
            yield (f"absence fields: ABSENCE_CARRYING_FIELDS['{block}'] carries {sorted(extra)} that "
                   f"{struct_name}::absences() does not - the python list is policing field(s) the "
                   f"engine no longer defines")


# Declarations this run could not parse, keyed by gateway. Populated by the check below and reported by
# `check_every_declaration_is_readable`, so an unparseable file is named rather than silently skipped.
UNREADABLE_DECLARATIONS: dict = {}


def check_every_declaration_is_readable():
    """A gateway whose definition.json will not parse had its declaration check SKIPPED.

    Separate from the check that reads it, because the two are different claims: that one is about
    whether a gateway's measurements match what it promised, and this one is about whether we could
    read the promise at all. Folding them together would publish a finding about a gateway's data on
    the strength of a file WE failed to parse.
    """
    for gw, why in sorted(UNREADABLE_DECLARATIONS.items()):
        yield (
            f"{gw}: definition.json could not be parsed ({why}), so what this gateway DECLARES it "
            f"serves was never compared against what it was measured doing"
        )


def check_declaration_matches_what_we_measured(gw):
    """A gateway may not both DECLARE a cell and mark it untestable.

    Declaring a cell is a claim that the gateway does it; `untestable` is an admission we could not
    show that it does. Holding both is the harness publishing a grey where it owes a yes or a no. The
    resolution is binary and belongs in the definition, not the artifact: prove the route and drop
    the untestable entry, or drop the declaration and under-claim honestly.
    """
    p = os.path.join(HERE, "gateways", gw, "definition.json")
    if not os.path.exists(p):
        return
    try:
        d = json.load(open(p))
    except Exception as e:
        # NOT A BARE `return`. Crashing the whole audit over one bad file was the defect; skipping that
        # gateway's declaration check in silence is a smaller version of the same one, and this file has
        # already been bitten by it once today - an unreadable SNAPSHOT used to vanish from both the
        # checked set and the not-audited list, so the run printed "PASS: every invariant held" over a
        # gateway nobody had looked at. A declaration we could not read is the same shape: the check
        # that compares what a gateway CLAIMS to serve against what it was measured doing simply did
        # not run, and only a stderr line said so while the exit code stayed 0.
        # Reported, not YIELDED. This check compares a declaration against a measurement, so a
        # declaration it cannot read gives it nothing to compare - and a violation here would be a
        # finding about the GATEWAY'S DATA, which may be perfectly fine. The defect is in our own repo.
        #
        # But it must not vanish either: an unreadable SNAPSHOT used to disappear from both the checked
        # set and the not-audited list, so the run printed "PASS: every invariant held" over a gateway
        # nobody had examined. So this is surfaced by `check_every_declaration_is_readable` at board
        # level instead, under its own name, where it cannot be mistaken for a claim about the numbers.
        print(f"  unreadable {gw} definition.json {p}: {e}", file=sys.stderr)
        UNREADABLE_DECLARATIONS[gw] = str(e)
        return
    dialects = ["openai", "openai-responses", "anthropic", "gemini", "cohere", "bedrock"]
    declared = set()
    for ri, row in enumerate(d.get("matrix") or []):
        for ci, ch in enumerate(row):
            if ch == "1" and ri < len(dialects) and ci < len(dialects):
                declared.add(f"{dialects[ri]}/{dialects[ci]}")
    untestable = set(d.get("untestable") or []) | set(d.get("untestable_cells") or [])
    for cell in sorted(declared & untestable):
        yield f"{gw}: declares {cell} in its matrix AND marks it untestable"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--engine", help="audit the board produced by this engine (default: the newest)")
    ap.add_argument("--gateway", help="narrow to one gateway while investigating")
    args = ap.parse_args()

    # AN EMPTY BOARD IS A CLEAN SKIP; A BROKEN BOARD IS A FAILURE. With zero snapshot files on disk
    # nothing is published and there is nothing to lie, so exiting non-zero here only blocks CI on
    # the empty board that follows every board-drop. But the skip is gated on the FILES being
    # absent, not on the loader finding nothing: snapshots that exist and cannot be read or matched
    # still fail, or a renamed directory would turn this audit into a permanent vacuous pass.
    if not snapshot_paths():
        print("SKIP: no snapshots present (empty board) - nothing published, nothing to audit")
        return 0
    engine = args.engine or newest_engine()
    if not engine:
        print("snapshot files exist but none carries an engine commit - refusing to skip", file=sys.stderr)
        return 1
    snaps = load(engine, args.gateway)
    if not snaps:
        print(f"no snapshots on engine {engine}", file=sys.stderr)
        return 1

    # A GATEWAY THIS AUDIT DID NOT LOOK AT MUST BE NAMED, NOT SILENTLY DROPPED.
    #
    # `load` pins the board to ONE engine so a board is audited as a board, which is right: two
    # engines' numbers are not comparable. But the gateways on the OTHER engine are still on disk and
    # still live on the site, and printing "5 gateways, PASS" while quietly skipping them is the exact
    # shape of dishonesty this file exists to prevent - a clean verdict over data nobody checked.
    #
    # It is reachable by design, not by accident: the board is updated gateway by gateway over the
    # previous run so readers are never shown an empty page, which means the field is legitimately
    # mixed-engine for as long as the run takes.
    skipped = {}
    unreadable = {}
    for f in snapshot_paths():
        try:
            d = json.load(open(f))
        except Exception as e:
            # A SNAPSHOT WE COULD NOT READ IS THE ONE CASE THAT USED TO VANISH COMPLETELY.
            #
            # This `except` fired BEFORE `gw` was read, so an unparseable file - a truncated write, a
            # disk-full, a partial rsync left behind by a race - landed in neither `snaps` (checked)
            # nor `skipped` (named as unaudited). It disappeared from the audit's output entirely, and
            # if every other gateway passed, main() printed "PASS: every invariant held" over a board
            # where one gateway's data was never examined. That is precisely the clean-verdict-over-
            # unchecked-data dishonesty the comment above says this file exists to prevent.
            #
            # The name comes from the FILENAME, because the only copy inside the file is in the thing
            # that failed to parse. Recorded as a VIOLATION rather than a disclosure: "I could not read
            # one of my inputs" must never reduce to a PASS, and an unreadable artifact sitting in
            # results/snapshots/ is itself a defect worth someone's attention.
            base = os.path.basename(f)
            gw = base[len("result_"):].rsplit("_", 1)[0] if base.startswith("result_") else base
            unreadable[gw] = f"{base}: {e}"
            continue
        gw = d.get("gateway")
        sha = ((d.get("rig") or {}).get("engine") or {}).get("commit") or ""
        if not gw or (args.gateway and gw != args.gateway) or gw in snaps:
            continue
        if not sha.startswith(engine):
            skipped[gw] = sha[:7] or "no engine stamp"

    violations = collections.defaultdict(list)
    cells = 0
    # Fields no snapshot's producer knew about: disclosed below, never silently skipped.
    unknown_fields = {}
    # Cells whose snapshot PREDATES the frontier, so its seven checks had nothing to run on. Counted per
    # cell and per gateway and printed with the other NOT AUDITED disclosures: "this metric was not
    # checked here" is a fact about the run and must not read as "this metric held". Every snapshot on
    # disk on 2026-07-29 is in this bucket - the frontier replaced the throughput scalars after they were
    # written - so a silent skip here would be the whole new invariant set quietly doing nothing.
    prefrontier_cells = 0
    prefrontier_gws = []
    # Cells whose snapshot PREDATES `ok` (see `producer_knew_ok`), so `check_frontier_is_rederivable_
    # from_its_sweep` has nothing sound to re-derive with and stays silent on them rather than call
    # every rung dirty. Every snapshot on disk on 2026-07-29 is in this bucket too - `ok` shipped after
    # every column currently published - so, same as `prefrontier_*` above, a silent skip here would
    # hide that the strongest check in the file is not actually running yet.
    preok_cells = 0
    preok_gws = []
    for gw, (_path, d, _sha) in sorted(snaps.items()):
        known = fields_the_producer_knew(d)
        frontier_known = producer_knew_the_frontier(d)
        if not frontier_known:
            prefrontier_gws.append(gw)
        ok_known = producer_knew_ok(d)
        if not ok_known:
            preok_gws.append(gw)
        for block, fields in ABSENCE_CARRYING_FIELDS.items():
            missing = [f for f in fields if f not in known.get(block, ())]
            if missing:
                unknown_fields.setdefault(gw, []).extend(f"{block}.{f}" for f in missing)
        for name, c in served_cells(d):
            cells += 1
            if not frontier_known:
                prefrontier_cells += 1
            if not ok_known:
                preok_cells += 1
            for check in CELL_CHECKS:
                kw = {}
                if check is check_declared_fields_are_carried:
                    kw = {"known": known}
                elif check is check_frontier_is_complete:
                    kw = {"frontier_known": frontier_known}
                elif check is check_frontier_is_rederivable_from_its_sweep:
                    kw = {"ok_known": ok_known}
                for v in check(f"{gw} {name}", c, **kw):
                    violations[check.__name__].append(v)
        for v in check_declaration_matches_what_we_measured(gw):
            violations["check_declaration_matches_what_we_measured"].append(v)

    # An input we could not parse is a board-level violation: see the note where `unreadable` is built.
    for gw, why in sorted(unreadable.items()):
        violations["check_every_snapshot_is_readable"].append(
            f"{gw}: snapshot could not be parsed, so this gateway was NOT audited at all - {why}"
        )

    # BOARD-LEVEL, NOT PER-CELL. Each of these is a property of this file's agreement with a sibling in
    # another language (the site's C6 bar, record.rs's absence field lists, frontier.rs's declared
    # bounds), or of which of this run's inputs could be read at all - never of any one cell's numbers.
    # They run AFTER the per-gateway loop because `check_every_declaration_is_readable` reports what that
    # loop populated.
    board_checks = (
                    check_absence_fields_mirror_the_engine,
                    check_frontier_bounds_agree_with_the_engine,
                    check_every_declaration_is_readable)
    for check in board_checks:
        for v in check():
            violations[check.__name__].append(v)

    print(f"engine {engine[:7]}  {len(snaps)} gateways  {cells} served cells")
    if skipped:
        listed = ", ".join(f"{g} (engine {e})" for g, e in sorted(skipped.items()))
        print(f"NOT AUDITED: {len(skipped)} gateway(s) measured by a different engine: {listed}")
        print("  They are published but were NOT checked by this run. Audit that engine explicitly")
        print("  with --engine <sha>, or re-measure them. A board is fully audited only when one")
        print("  engine produced all of it.")
    if unknown_fields:
        n = len(unknown_fields)
        every = sorted({f for fs in unknown_fields.values() for f in fs})
        print(f"NOT AUDITED: {n} snapshot(s) predate {len(every)} declared field(s): {', '.join(every)}")
        print("  No cell in those snapshots carries them, so the engine that wrote them did not have")
        print("  them yet and this run could not check them. They are checked the moment a snapshot")
        print("  from an engine that DOES publish them lands - re-measure to audit them.")
    if prefrontier_cells:
        print(f"NOT AUDITED: the frontier on {prefrontier_cells} served cell(s) across "
              f"{len(prefrontier_gws)} snapshot(s) that predate it: {', '.join(prefrontier_gws)}")
        print("  No cell in those snapshots publishes perf.frontier, so the engine that wrote them still")
        print(f"  had the rps_max_proxy / rps_sustained_20ms scalars and these {len(FRONTIER_CHECKS)} "
              f"invariants had nothing")
        print("  to run on. They run the moment a snapshot from an engine that DOES publish readings")
        print("  lands - re-measure to audit them.")
    if preok_cells:
        # AUDITED, BUT APPROXIMATELY - and saying "NOT AUDITED" here would be the same class of
        # misstatement this file exists to catch. The re-derivation DID run on these cells; what it could
        # not do is apply the engine's `ok > 0` half exactly, because no rung in these snapshots carries
        # `sweep_max_proxy.ok`. It fell back to treating a positive rate OR a p99 as proof of completion,
        # which is WIDER than the engine's rule (a completion always leaves a latency sample, while a
        # rate can round away through `as i64`), so what these cells risk is a MISSED catch, never a
        # false alarm. The distinction is worth the four extra lines: a reader deciding how much this
        # PASS is worth needs to know which cells were checked exactly and which approximately.
        print(f"AUDITED APPROXIMATELY: check_frontier_is_rederivable_from_its_sweep on {preok_cells} "
              f"served cell(s) across {len(preok_gws)} snapshot(s) that predate `sweep_max_proxy.ok`: "
              f"{', '.join(preok_gws)}")
        print("  The re-derivation ran on all of them. Without `ok` it could not apply the engine's")
        print("  `ok > 0` half exactly, so it treated a positive rate or a p99 as proof that the window")
        print("  completed something - wider than the engine's rule, so a missed catch rather than a")
        print("  false alarm. An engine that publishes `ok` makes these exact; nothing here is skipped.")
    print(f"{len(CELL_CHECKS)} per-cell invariants ({len(FRONTIER_CHECKS)} of them the frontier's) + 1 "
          f"per-gateway + {len(board_checks)} board-level invariants\n")

    if not violations:
        print("PASS: every invariant held.")
        return 0

    total = sum(len(v) for v in violations.values())
    for check, items in sorted(violations.items()):
        print(f"{len(items):3d}  {check}")
        for it in items:
            print(f"       {it}")
    print(f"\nFAIL: {total} violation(s) across {len(violations)} invariant(s).")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
