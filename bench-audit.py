#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Every cross-metric invariant a published board must hold, checked as a program that exits non-zero
# so the checks are re-runnable rather than ad hoc eyeballing.
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
# Anchored to this file, not the invoker's cwd: a relative glob would silently find zero files unless
# run from the repo root. HERE is a module global (not baked into each call) so tests can point the
# whole audit at a fixture tree instead of the real repo.
HERE = os.path.dirname(os.path.abspath(__file__))


def snapshot_paths():
    """Every snapshot file on this board, sorted, resolved against HERE rather than the cwd."""
    return sorted(glob.glob(os.path.join(HERE, "results", "snapshots", "result_*.json")))


# ── the bars ──────────────────────────────────────────────────────────────────────────────────────
#
# Named, not inlined, so a reader deciding whether to trust a violation can see what it was measured
# against. The old `rps_sustained_20ms` / `rps_max_proxy` scalars are retired in favour of the frontier
# below: six maxima over rung sets that only grow as the bound relaxes, so a looser reading below a
# tighter one is unrepresentable. `check_frontier_rates_are_monotone_in_the_bound` asserts that
# ordering; `check_frontier_is_rederivable_from_its_sweep` recomputes every reading from the raw rungs.

# record.rs's own declaration of the fields ABSENCE_CARRYING_FIELDS mirrors, parsed rather than
# imported since there is no build step to share it across languages. See
# check_absence_fields_mirror_the_engine() for what happens when the two disagree.
RECORD_RS_PATH = os.path.join("engine", "src", "record.rs")
RECORD_RS_STRUCTS = {"perf": "CellPerf", "stream": "CellStream", "memory": "CellMemory"}
_RUST_IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")

# ── the frontier ──────────────────────────────────────────────────────────────────────────────────
#
# The tail-latency bounds every served cell must publish a reading at, in microseconds, ascending, with
# the UNBOUNDED reading (`p99_bound_us: null`, failures only, no latency claim) last. Mirrors the
# engine's `frontier::P99_BOUNDS_US`; `check_frontier_bounds_agree_with_the_engine` parses the rust
# declaration and fails on drift. Length and ordering are themselves the invariant, not just the
# values - see `check_frontier_is_complete`.
P99_BOUNDS_US = [1_000, 5_000, 10_000, 50_000, 100_000]
FRONTIER_RS_PATH = os.path.join("engine", "src", "frontier.rs")
FRONTIER_RS_RE = re.compile(r"pub const P99_BOUNDS_US:\s*\[u64;\s*\d+\]\s*=\s*\[([^\]]*)\]\s*;")

# A rate this far above its own concurrency is not a proxy measurement.
#
# One connection cannot issue 20000 requests per second against a real socket; a number that says so
# is a units error or a counted retry, not a gateway. Deliberately loose - this catches the class of
# defect where a rate is divided by the wrong thing, not marginal optimism.
MAX_RPS_PER_CONNECTION = 20_000


def _instrument_map():
    """commit sha -> instrument id, from site/instrument-equivalence.json.

    Groups by instrument, not raw commit (same grouping C8 and gen-data use): two commits whose built
    binaries are byte-identical are one instrument, so pinning to a raw sha would drop rows measured
    by the same binary but stamped differently. An entry is only honoured if it carries the artifact
    evidence its own file demands.
    """
    p = os.path.join(HERE, "site", "instrument-equivalence.json")
    out = {}
    try:
        doc = json.load(open(p))
    except Exception:
        return out
    for inst in doc.get("instruments") or []:
        commits = inst.get("commits") or []
        ev = ((inst.get("evidence") or {}).get("otb_release_sha256")) or {}
        hashes = list(ev.values())
        if not inst.get("id") or not commits:
            continue
        # No artifact evidence, or hashes that disagree, means the entry proves nothing.
        if len(hashes) < len(commits) or len(set(hashes)) != 1:
            continue
        for c in commits:
            out[c] = inst["id"]
    return out


def _same_instrument(sha, engine, imap):
    """Does `sha` belong to the instrument identified by `engine` (a sha prefix or an instrument id)?"""
    if not engine:
        return True
    if sha.startswith(engine):
        return True
    mine = imap.get(sha)
    if mine is None:
        return False
    # `engine` may be an instrument id, or any sha belonging to that instrument.
    if mine == engine:
        return True
    return any(mine == inst for c, inst in imap.items() if c.startswith(engine))


def load(engine=None, gateway=None):
    """The newest snapshot per gateway, pinned to one INSTRUMENT so a board is audited as a board."""
    imap = _instrument_map()
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
        if not _same_instrument(sha, engine, imap):
            continue
        by_gw[gw] = (f, d, sha)
    return by_gw


def newest_engine():
    """The engine commit of the snapshot with the newest `measured_at`, so the default audits the
    current board. Recency is the snapshot's own timestamp, not filename order (files sort
    alphabetically by gateway). A missing/unparseable `measured_at` falls back to file mtime.
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
# Each takes one served cell and yields a string per violation. If one of these stops firing on data
# that used to trip it, that's a finding, not a pass - a check that never yields is dead weight.

# check_sustained_not_above_peak and check_peak_came_from_its_own_sweep are removed along with the
# `rps_sustained_20ms` / `rps_max_proxy` scalars they compared. Their successors,
# `check_frontier_rates_are_monotone_in_the_bound` and `check_frontier_is_rederivable_from_its_sweep`,
# are strictly stronger: they assert ordering across all six frontier readings and recompute every one
# from the raw rungs in both directions, rather than one-directional scalar comparisons. See
# `engine/src/frontier.rs`'s module note.


def check_sweep_carries_its_latency(name, c):
    """Every throughput window must publish the p99 it ran at.

    Regression guard: `SweepProbe::probe` once dropped the p99 it had already computed, forcing a
    second measurement pass (minutes later, across a gateway restart) just to recover latency. If this
    check ever passes vacuously again, that wasteful second search comes back with it.
    """
    pts = (c.get("perf") or {}).get("sweep_max_proxy") or []
    if not pts:
        return
    withp99 = sum(1 for r in pts if r.get("p99_us") is not None)
    if withp99 == 0:
        yield f"{name}: all {len(pts)} throughput windows published without the p99 they measured"


# REMOVED: check_ttft_percentiles_are_ordered asserted p50 <= p99 for `added_ttft`, but that field is a
# difference of two legs' percentiles, not a single population, so no ordering guarantee holds; it fired
# on real, correct data. Nothing replaces it.
#
# REMOVED: check_rate_and_concurrency_travel_together paired the retired `rps_sustained_20ms`/
# `rps_max_proxy` scalars with their concurrency fields; all four are deleted, and a rate/concurrency
# pair is now one `FrontierReading`, unreachable without both by construction.
#
# RE-POINTED, NOT DROPPED: check_rate_is_physically_possible read `rps_max_proxy / conc_at_peak`. Both
# fields are gone, but the defect class - a rate divided by the wrong thing - now covers all six
# frontier readings via `check_frontier_rate_is_physically_possible` below.
#
# REMOVED: check_frames_have_a_stream_behind_them fired when `stream.cpu_fps` was published beside a
# measured `streams_sustained` of 0 (a rate over a zero population). `cpu_fps` is deleted;
# `streams_sustained_fps` measures the same window it rates, so this can't recur.


# Every metric a served cell's block may publish; a null on any of these with no `absences` entry
# beside it is a bare hole. Mirrors the engine's `absences()` lists in `engine/src/record.rs` (CellPerf,
# CellStream, CellMemory) field for field; see `check_absence_fields_mirror_the_engine` for the
# cross-check that keeps it that way.
ABSENCE_CARRYING_FIELDS = {
    "perf": [
        # The retired throughput scalars (rps_sustained_20ms, rps_max_proxy, and their concurrency
        # fields) are deliberately NOT listed: they're gone from CellPerf. Their replacement,
        # `perf.frontier`, is a Vec and unreachable by the engine's `absences_of!` macro, so it's
        # policed separately by `check_every_absent_frontier_reading_has_a_reason` instead of by name
        # here.
        "added_latency_p50_us", "added_latency_p99_us", "gateway_c1_p99_us", "direct_c1_p99_us",
        # The cost group. Two absence reasons are REFUSALS rather than gaps: a window with any failure
        # publishes no cost (dividing CPU by only the successes would describe the failures, not the
        # work), and a window on a swapping box is marked a harness fault. A null with no reason reads
        # as "not implemented" when the truth is "measured, and deliberately withheld".
        "cpu_us_per_request", "rps_per_cpu_second", "cost_window_conc", "cost_window_ok",
        "cost_window_rps", "cost_core_utilisation", "cost_threads",
        "cost_nonvol_ctxt_per_request", "cost_majflt",
    ],
    "stream": [
        # cpu_fps and its concurrency field are deleted from CellStream (with sweep_cpu_fps), same as
        # the retired perf scalars above.
        "added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us", "added_gap_p99_us",
        "streams_sustained", "streams_sustained_fps",
        # Sample counts behind the two added-TTFT percentiles: a p99 over three lucky samples looks
        # identical to one over a hundred without the count beside it to weigh it against.
        "ttft_gw_samples", "ttft_direct_samples",
    ],
    "memory": [
        "idle_rss_mib", "steady_state_rss_mib", "recovered_rss_mib", "peak_rss_mib",
        "peak_rss_hwm_mib", "time_to_plateau_s", "growth_rate_mib_per_min",
        "plateaued", "load_s",
        # Absent BECAUSE the measurement succeeded: these describe HOW a window failed to settle, so a
        # window that DID settle has nothing to publish here. Listed (a null must still carry a reason)
        # but exempted from check_no_bare_absence's "a served cell publishes a number" rule via
        # SHAPE_FIELDS below, which lets a reasoned absence stand.
        "shape", "idle_shape",
        # The idle window's own verdict and fitted slope - ordinary numeric metrics (a settled window
        # publishes 1.0 and its slope), so unlike the shape fields above these ARE held to "a served
        # cell publishes a number".
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

    A null with no stated cause is indistinguishable from a cell that never ran. The engine writes the
    reason into the cell's `absences` map; this pins that the map actually covers every null the cell
    publishes, so no consumer has to render an unexplained blank.
    """
    absences = c.get("absences") or {}
    for block, fields in ABSENCE_CARRYING_FIELDS.items():
        blk = c.get(block) or {}
        for f in fields:
            if f in blk and blk[f] is None and f"{block}.{f}" not in absences:
                yield f"{name}: {block}.{f} is null with NO reason in absences (a bare hole)"


def check_declared_fields_are_carried(name, c, known=None):
    """A served cell must CARRY every field it declares - as a number, or as a null with a reason.

    Closes the hole `check_no_bare_absence` cannot see: that check requires `f in blk`, so it polices
    a field that is present-and-null but is blind to a field OMITTED from the block entirely - and so
    is every other check here, since `.get()` returns None whether the serializer wrote null or wrote
    nothing. A `#[serde(skip_serializing_if = "Option::is_none")]` on any Measurement field would drop
    the key and silently evaporate the whole absence discipline while this audit kept printing PASS.
    A missing key is neither "measured" nor "not tested", so it's always a violation.

    The block itself is held to the same standard: a served cell with no `stream` object at all is the
    same claim as a block full of unexplained nulls, with the evidence deleted.
    """
    for block, fields in ABSENCE_CARRYING_FIELDS.items():
        blk = c.get(block)
        if not isinstance(blk, dict):
            yield (f"{name}: served cell publishes NO {block} block at all - a served cell carries "
                   f"its declared fields or states why they are absent, it does not omit them")
            continue
        for f in fields:
            if f not in blk:
                # A field the producing engine never had differs from one it dropped. `known` is
                # computed per snapshot from that snapshot's own cells, so demanding a field no cell
                # in it carries would demand yesterday's artifact contain tomorrow's field. This is
                # all-or-nothing per snapshot: a serializer dropping a key on SOME cells still fails
                # loudly since sibling cells carry it; a field missing everywhere is disclosed at the
                # end of the run instead.
                if known is not None and f not in known.get(block, ()):
                    continue
                yield (f"{name}: {block}.{f} is OMITTED from the block (not null-with-reason, "
                       f"absent) - key-missing and measured are indistinguishable to every check")


def check_stream_capacity_is_a_number(name, c):
    """A streaming cell's capacity metrics are numbers (0 included), or a rig-class absence.

    Guards against a search that quietly stops producing: an absence whose reason is `not_measured`
    here means the bisection gave up rather than converging, the silent-yield defect that once shipped
    a board with cpu_fps on 1 of 16 served cells. cpu_fps itself is deleted from CellStream; the
    remaining risk is `streams_sustained`, still a bisection that can stop producing.
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
            # An absence that explains itself is not a silent yield: "the bisection proved c=6144 and
            # could not reconfirm it" is a search that ran and said why, not one that gave up silently.
            # Forcing a measured 0 here would fabricate an answer to a question left genuinely unsettled.
            if detail:
                continue
            yield (f"{name}: stream.{f} is absent with reason {reason!r} and NO detail on a served "
                   f"streaming cell - a search that stops producing must say why")


# ── the SEARCH TRACE's invariants ────────────────────────────────────────────────────────────────
#
# The checks above only read the published number, never `sweep_streams` - the trace of the search
# that produced it - so a search that gave up with budget unspent could still publish a prose absence
# that reads as an honest failure and pass every check above.
#
# These checks close that by re-deriving from the trace itself and demanding agreement, the same bar
# as the frontier's checks. The trace is published on every streaming cell, so the search's behaviour
# is auditable from committed JSON alone.
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
    first up to and including it passed, before anything in this cell had failed.

    Stops at the first failed window by design, so the result is a concurrency reached before
    anything went wrong. Callers decide what counts as a violation relative to that top.
    """
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

    A rung is confirmed by MAJORITY, not by its first window (one-api's published 266 came from
    `[pass, pass, fail]`), so one failing window is a vote, not a verdict. The signature of giving up
    early is a gap: the search stopped at a rung strictly above a concurrency it had already carried,
    having probed nothing in between - room it declined to use, paid for with an absence where a
    number was available.
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
    # WHERE THE SEARCH STOPPED, not the smallest rung it ever probed - taking the min over every rung
    # (including the ascending prefix's own c=1, c=2) always satisfies `lowest <= clean + 1` and this
    # check would never fire. Regression-covered by a reject fixture in bench-audit_test.py.
    lowest = rungs[-1].get("conc") or 0
    if lowest <= clean + 1:
        return                                   # it walked down to (or below) what it had carried
    # A search that spent its declared step-down budget (MAX_CEILING_STEPDOWNS rungs) ran out, it did
    # not give up - that's a different finding, not a defect. Counting distinct concurrencies probed
    # below the bisected ceiling is the budget as the trace shows it.
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

    It's either our rig failing to drain between windows or a gateway that stopped serving after an
    overload - both are findings, but publishing it as an ordinary failure with no attribution is not
    acceptable.
    """
    rungs = stream_trace(c)
    if not rungs:
        return
    clean = proven_clean_top(rungs)
    if clean <= 0:
        return
    # BELOW, NOT AT. `<= clean` would flag the search's own terminating condition (a rung passes once,
    # fails confirmation at the same concurrency, and the engine steps down) as a violation - that's
    # how a ceiling is legitimately found. Only a failure strictly below an already-carried concurrency
    # has no gateway-only explanation.
    below = [r.get("conc") for r in rungs if r.get("passed") is not True and (r.get("conc") or 0) < clean]
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
    published rung that lost its own windows means the summary and the evidence disagree.
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

    "It does not recover from overload" is the most useful thing a reader can learn from such a cell;
    silence is a worse answer than a number.
    """
    rungs = stream_trace(c)
    if not rungs or len(rungs) < 6:
        return
    tail = [r.get("passed") is True for r in rungs[-5:]]
    if any(tail):
        return
    # A WEDGE IS A FAILURE BELOW WHAT THE CELL ALREADY CARRIED, not merely a run of failures at the
    # end - a budget-exhausted step-down always ends on a run of failures, which is ordinary
    # non-convergence and not a wedge. The impossible signature - and the only one meaning the process
    # stopped serving - is a tail failure at or under the top of the uncontaminated ascending prefix.
    clean = proven_clean_top(rungs)
    if clean <= 0 or not any((r.get("conc") or 0) <= clean for r in rungs[-5:]):
        return
    entry = (c.get("absences") or {}).get("stream.streams_sustained") or {}
    detail = (entry.get("detail") or "").lower()
    # An absence already reasoned as the rig's (harness_error/rig_limited) has already answered this -
    # the engine's own contamination guard reached the same conclusion by a stricter route, so
    # keyword-matching the prose on top of that produces false positives.
    if entry.get("reason") in ("harness_error", "rig_limited"):
        return
    if "recover" not in detail and "restart" not in detail and "already carried" not in detail:
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
# (`perf.sweep_max_proxy`), so every reading is re-derivable rather than merely sanity-checked. The
# frontier's arithmetic is `max(rps) over {rungs that qualify}`, and every input is published, so this
# file can run that arithmetic itself and demand the same answer.
#
# The bar is "recompute and compare", not "looks plausible": `frontier.rs` derives monotonicity from
# the algorithm, but nothing in the artifact proves the published readings actually came from running
# it over these rungs. That proof lives here.


def bound_key(us):
    """The name a reading's absences are filed under: `10ms`, or `unbounded` for the failure-only one.

    Must mirror `CellPerf::absences`'s `format!("{}ms", us / 1000)` exactly, or every absent reading on
    the board looks like it's missing its reason - a false FAIL.
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

    Zero failures, not a tolerance: the rig's own refused connects never reach the rung, so a failure
    here is the gateway failing a request it accepted. Exact test is `ok > 0 and fail == 0`; a plain
    `rps > 0` test false-positived when a slow window's rate rounded down to 0 despite completing a
    request. An absent `fail` never counts as clean ("measured nothing" != "measured no failures").

    When `ok` is absent (pre-`SweepPoint.ok` snapshots), falls back to p99-as-proof-of-completion
    rather than refusing outright, so the strongest check in the file still runs, just approximately -
    the fallback is wider than the engine's rule, so its error is a missed catch, never a false alarm.
    See `producer_knew_ok`.
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

    `frontier::Rung::qualifies`, restated: clean, and STRICTLY under the bound (`p99 < bound`, so a
    rung sitting exactly on a bound does not clear it). A rung with no p99 is disqualified from every
    bounded reading (no latency reading, no latency claim), but not the unbounded one.
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

    A cell publishing four would silently shrink the board unnoticed, since every other check just
    iterates whatever readings are present. Order is part of the invariant, not presentation:
    `check_frontier_rates_are_monotone_in_the_bound` reads it positionally.

    `frontier_known` tells a pre-frontier artifact from a shrunk one by asking the artifact itself: if
    no served cell in the snapshot carries a frontier, the engine didn't have the metric yet; if some
    cell does, one that doesn't dropped it.
    """
    fr = frontier_of(c)
    if not fr:
        # WITHHELD IS NOT DROPPED. A cell whose egress re-verification proved the gateway never
        # translated (request forwarded unchanged rather than translated) correctly withholds its
        # whole perf group and publishes `egress_reverified: false` - an empty frontier there is
        # correct, not a defect. The exemption is keyed on that disclosure being present, not on
        # trusting the producer: a cell that simply lost its frontier has no such flag and still fails.
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

    Structural in the engine, asserted anyway: relaxing the bound only adds rungs to the qualifying
    set, and each reading is a max over that set, so a max over a superset cannot be smaller. A
    violation here means the published readings did not come from the rungs they claim to summarise -
    the one thing the structure cannot rule out on its own.

    An ABSENT looser reading beside a PRESENT tighter one is the same violation in degenerate form: the
    looser bound's qualifying set contains the tighter one's, so it cannot be empty when that one wasn't.
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

    The strongest check in this file: it verifies the engine's ARITHMETIC, not just the plausibility of
    its summary. Each reading is `max(rps)` over qualifying rungs, plus the concurrency, tail, and next
    disqualifying concurrency - all four inputs are published per rung, so all four outputs are
    recomputable, and a summary that disagrees with the rungs behind it fails here.

    Rungs may repeat a concurrency, so ties are matched against the SET of rungs carrying the winning
    rate at the published concurrency, not against one assumed argmax.

    `ok_known` is disclosure only now, it doesn't skip anything: `rung_served_cleanly` falls back to an
    approximate rule when `ok` is absent (see its docstring), so the re-derivation always runs, exactly
    or approximately, and `ok_known` just records which.
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
            # `best` is exact now, including 0: a rung with `ok > 0` and a rate that rounded down to 0
            # is a proven-clean rung with a real reading, not a hole, so a published absence beside it
            # is a genuine disagreement.
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
                # The tail must be the winning rung's own; publishing anything else (the bound
                # especially) restates the question as though it were the answer.
                measured = sorted((w.get("p99_us") for w in at_conc),
                                  key=lambda v: (v is None, v))
                yield (f"{name}: frontier.{key} publishes p99 {pub_p99} but the qualifying rung(s) at "
                       f"c={pub_conc} measured {measured}")
        # THE BOUNDARY PROOF, re-derived: the lowest concurrency above the winner that stopped
        # qualifying. Absent is not a hole here - it's the positive finding that the sweep ran out of
        # range while this bound still held, which is why `first_disqualified_conc` is deliberately
        # kept out of the absences map (see `CellPerf::absences`).
        #
        # RE-DERIVED PER CONCURRENCY, NOT PER RUNG: rungs are per window (each concurrency appears
        # WINDOWS_PER_RUNG times), so taking the min over any single non-qualifying rung let one
        # unlucky window disqualify a concurrency the gateway had demonstrably held.
        #
        # "Any qualifying window" matches how the winner itself is chosen (`read_at` maximises over
        # qualifying rungs) - a stricter boundary rule could name a concurrency the same reading might
        # have been taken at.
        if isinstance(pub_conc, (int, float)):
            concs_above = {q["conc"] for q in rungs
                           if isinstance(q.get("conc"), (int, float)) and q["conc"] > pub_conc}
            above = [c for c in concs_above
                     if not any(rung_qualifies(q, bound) for q in rungs if q.get("conc") == c)]
            expect_fd = min(above) if above else None
            got_fd = r.get("first_disqualified_conc")
            if got_fd != expect_fd:
                yield (f"{name}: frontier.{key} names first_disqualified_conc={got_fd}, re-derived "
                       f"from the rungs above c={pub_conc} it is {expect_fd} - the half of the "
                       f"reading's proof that says this really is the boundary")


def check_frontier_disclosure_agrees_with_the_ladder(name, c):
    """`lower_bound` is true exactly when the winning rung is the highest concurrency probed.

    A disclosure that disagrees with the data is worse than none: true means "we ran out of ladder,
    this rate is a floor not a ceiling"; false means "we probed higher and it was worse, the peak is
    established". Getting it backwards overstates or understates the finding on every surface that
    repeats it.

    An ABSENT reading cannot be a lower bound of anything - there's no rate to qualify - so its flag
    must be false. A stray true would put a "measured at least this much" label on an unmeasured column.
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

    Qualification is strict (`p99 < bound`), so for a reading that published a rate:

      * a tail ABOVE its own bound is a violation - that rung couldn't have qualified;
      * a tail EQUAL to its own bound is the same violation, reported separately since it's the
        signature of a specific defect: the bound copied into the answer slot;
      * no tail at all on a BOUNDED reading is a latency claim with no reading behind it. Only the
        unbounded reading may omit its tail, since it makes no such claim.
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

    Runs once per frontier reading (up to six per cell), since each carries its own rate and
    concurrency.
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

    A `Measurement` serializes an absence as a bare `null` with its reason in the sibling `absences`
    map; `perf.frontier` is a Vec, unreachable by the engine's field-walking macro, so `CellPerf::
    absences` populates these keys in a hand-written loop instead. This check pins that the loop
    actually covers every null reading.

    KEYED BY BOUND, NOT INDEX (`perf.frontier.10ms.rps`): the bound is the identity, the index is just
    ordering. If the engine ever switched to indices, every reason would be filed under a key nothing
    looks up.

    `first_disqualified_conc` is deliberately exempt: its absence is the positive finding that the
    sweep ran out of range while this bound still held (stated directly by `lower_bound: true`), so
    `CellPerf::absences` keeps it out of the map on purpose.
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

    True when any served cell carries a non-empty `perf.frontier`. Same mechanism as
    `fields_the_producer_knew`: ask the artifact rather than trust a commit-to-field table. False means
    the snapshot predates the metric; those cells are disclosed as unaudited rather than flagged.
    """
    return any(frontier_of(c) for _name, c in served_cells(d))


def producer_knew_ok(d):
    """Did the engine that wrote THIS snapshot publish `ok` per rung? Same mechanism as
    `producer_knew_the_frontier`: ask the artifact.

    True when any rung on any served cell carries the key at all (type is deliberately not checked - a
    wrong-typed `ok` is a shape violation for something else to catch). False means every rung predates
    `ok`, so `rung_served_cleanly` can't be proven either way for them, and
    `check_frontier_is_rederivable_from_its_sweep` stays silent rather than flagging honest rates as
    unbacked.
    """
    return any("ok" in r for _name, c in served_cells(d) for r in sweep_rungs(c))


def parse_rust_frontier_bounds(text):
    """The engine's own `P99_BOUNDS_US`, read out of frontier.rs. None when the declaration isn't in
    the expected shape - the caller must treat that as a failure, never as agreement."""
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
    """`P99_BOUNDS_US` here must be exactly the engine's `frontier::P99_BOUNDS_US`.

    The bounds decide how many columns a board HAS. If the engine dropped a bound and this list kept
    it, the completeness check would fail the whole board for a column nobody publishes - and trimming
    the python list to "fix" that would let the audit stop noticing a shrinking board. Parse the
    sibling, fail on drift, and treat "cannot find the declaration" as drift, not agreement.
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

    A board isn't always written by one engine, so this asks the artifact rather than trusting a
    commit-to-field table: a field appearing on ANY served cell was known to the producer, so every
    OTHER cell must carry it too. That keeps a key dropped on only some cells failing loudly, while a
    field uniformly absent is reported as an unaudited gap instead of drowning everything in identical
    violations.
    """
    known = {b: set() for b in ABSENCE_CARRYING_FIELDS}
    for _name, c in served_cells(d):
        for block, fields in ABSENCE_CARRYING_FIELDS.items():
            blk = c.get(block)
            if isinstance(blk, dict):
                known[block].update(f for f in fields if f in blk)
    return known


# Listed apart from CELL_CHECKS so the run can say how many were skipped on a pre-frontier snapshot.
# `check_frontier_is_complete` goes first: it decides whether this cell has a frontier to talk about at
# all, so its violation reads before the others' silence on an empty one.
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
    check_no_bare_absence,
    check_declared_fields_are_carried,
    check_stream_capacity_is_a_number,
] + FRONTIER_CHECKS + TRACE_CHECKS

def parse_rust_absences(text, struct_name):
    """The exact field list `struct_name::absences()` walks, read out of record.rs's own
    `absences_of!(self, ...)` invocation. None when the expected shape - one
    `impl <struct_name> { ... absences_of!(self, a, b, c, ...) ... }` block, comma-separated fields,
    comments allowed between them - isn't found; the caller must treat None as a failure, never as
    agreement. The macro's argument list has no parentheses in it, so a non-greedy match up to the
    first `)` after `absences_of!(self,` is the call's real closing paren, not one hiding in a comment.
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

    bench-audit_test.py's accept-side fixture is generated FROM ABSENCE_CARRYING_FIELDS, so a field
    quietly deleted from that list shrinks the fixture in step and stays green - only a check that reads
    the engine's own declaration, independent of this file's list, catches a field that stopped being
    policed (or one the engine grew that this list never learned about).

    Modeled on the retired C6-bar drift gate (ledger TOOL-02): parse the sibling's declaration rather
    than importing it, and treat "cannot find the shape expected" as a violation, not a pass.
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


def check_the_cost_window_obeys_its_own_arithmetic():
    """Every cell's cost fields must cross-check to the SAME core count the rig pinned.

    The board publishes a peak rate at one concurrency and a CPU-per-request at another
    (COST_WINDOW_CONCURRENCY, identical for every entrant); multiplying the two makes an impossible
    number checkable without any new measurement:

        cpu_us_per_request * cost_window_rps / 1e6  ==  cores * cost_core_utilisation

    so `rps * cpu / 1e6 / utilisation` is the core count each cell implies, and every cell on the board
    must imply the same one since the rig pins every gateway to the same cores. A cell whose three
    independently-measured fields disagree has one of them wrong, with no way to tell which from the
    outside, which is why this fires rather than picking a winner.

    Two exclusions, both counted rather than silently dropped:

      * LOW UTILISATION - it's the denominator, so a barely-loaded cell turns rounding in the numerator
        into whole cores of error.
      * TOO FEW REQUESTS - `cost_window_rps` is rounded to a whole number, so a slow cell's rate carries
        up to half a request per second of quantisation; a small window can't check a one-percent
        identity.
    """
    UTIL_FLOOR = 0.20
    # Half a request per second of rounding is under a tenth of a percent by here, which is an order
    # of magnitude inside the tolerance below.
    MIN_WINDOW_REQUESTS = 500
    TOLERANCE = 0.08
    implied = []
    skipped = 0
    for gw, (f, d, sha) in load(engine=None).items():
        ups = ((d.get("matrix") or {}).get("upstreams")) or {}
        for eg, u in ups.items():
            for ing, c in (u.get("cells") or {}).items():
                if c.get("served") is not True:
                    continue
                p = c.get("perf") or {}
                rps, cpu, util = (p.get("cost_window_rps"), p.get("cpu_us_per_request"),
                                  p.get("cost_core_utilisation"))
                if not all(isinstance(x, (int, float)) for x in (rps, cpu, util)):
                    continue
                ok = p.get("cost_window_ok")
                if util < UTIL_FLOOR or rps <= 0 or not isinstance(ok, (int, float)) or ok < MIN_WINDOW_REQUESTS:
                    skipped += 1
                    continue
                implied.append((f"{gw} {ing}>{eg}", rps * cpu / 1e6 / util))
    if len(implied) < 2:
        return
    med = sorted(x for _, x in implied)[len(implied) // 2]
    off = [(name, v) for name, v in implied if abs(v - med) / med > TOLERANCE]
    if off:
        worst = sorted(off, key=lambda x: -abs(x[1] - med))[:4]
        yield (f"the cost window's own arithmetic disagrees on {len(off)} of {len(implied)} cell(s): "
               f"cpu_us_per_request x cost_window_rps / utilisation implies {med:.2f} cores across the "
               f"board but " + ", ".join(f"{n} implies {v:.2f}" for n, v in worst) +
               f" - three independently measured fields that cannot all be right"
               + (f" ({skipped} cell(s) excluded as uncheckable: under {UTIL_FLOOR:.0%} utilisation or "
                  f"under {MIN_WINDOW_REQUESTS} requests in the window)" if skipped else ""))


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
        # Not a bare `return`: silently skipping this gateway's check would let a bad definition.json
        # produce "PASS: every invariant held" over a gateway nobody actually checked. Not yielded
        # either - a declaration we can't read gives nothing to compare, and a violation here would be
        # a claim about the gateway's DATA when the defect is in our own repo. Surfaced instead by
        # `check_every_declaration_is_readable` at board level, under its own name.
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

    # An empty board is a clean skip; a broken one is a failure. Gated on the FILES being absent, not
    # on the loader finding nothing - snapshots that exist and can't be read/matched still fail, or a
    # renamed directory would turn this into a permanent vacuous pass.
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

    # A gateway this audit did not look at must be named, not silently dropped. `load` pins the board
    # to one engine (two engines' numbers aren't comparable), but gateways on other engines are still
    # on disk and live on the site - printing "5 gateways, PASS" while skipping them would be a clean
    # verdict over unchecked data. Reachable by design: the board updates gateway by gateway, so it is
    # legitimately mixed-engine for as long as a run takes.
    skipped = {}
    unreadable = {}
    for f in snapshot_paths():
        try:
            d = json.load(open(f))
        except Exception as e:
            # An unparseable file (truncated write, disk-full, partial rsync) must not vanish from both
            # `snaps` and `skipped` - that let main() print "PASS: every invariant held" over a board
            # where one gateway was never examined. The name comes from the filename, since the file's
            # own copy is unreadable. Recorded as a VIOLATION, not a disclosure: an unreadable artifact
            # in results/snapshots/ is itself a defect.
            base = os.path.basename(f)
            gw = base[len("result_"):].rsplit("_", 1)[0] if base.startswith("result_") else base
            unreadable[gw] = f"{base}: {e}"
            continue
        gw = d.get("gateway")
        sha = ((d.get("rig") or {}).get("engine") or {}).get("commit") or ""
        if not gw or (args.gateway and gw != args.gateway) or gw in snaps:
            continue
        if not _same_instrument(sha, engine, _instrument_map()):
            skipped[gw] = sha[:7] or "no engine stamp"

    violations = collections.defaultdict(list)
    cells = 0
    # Fields no snapshot's producer knew about: disclosed below, never silently skipped.
    unknown_fields = {}
    # Cells whose snapshot PREDATES the frontier, so its seven checks had nothing to run on. Counted per
    # cell/gateway and printed with the other NOT AUDITED disclosures, so "not checked" never reads as
    # "held" - a silent skip here would be the whole new invariant set quietly doing nothing.
    prefrontier_cells = 0
    prefrontier_gws = []
    # Cells whose snapshot PREDATES `ok` (see `producer_knew_ok`), so `check_frontier_is_rederivable_
    # from_its_sweep` has nothing sound to re-derive with and stays silent rather than call every rung
    # dirty. Same reasoning as `prefrontier_*` above.
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

    # BOARD-LEVEL, NOT PER-CELL: agreement with a sibling in another language (record.rs's absence
    # field lists, frontier.rs's declared bounds), or which inputs could be read at all - never any one
    # cell's numbers. Run after the per-gateway loop since check_every_declaration_is_readable reports
    # what that loop populated.
    board_checks = (
                    check_absence_fields_mirror_the_engine,
                    check_frontier_bounds_agree_with_the_engine,
                    check_every_declaration_is_readable,
                    check_the_cost_window_obeys_its_own_arithmetic)
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
        # Audited, but approximately, not "NOT AUDITED" - the re-derivation did run, just via the wider
        # fallback rule (see rung_served_cleanly), so these cells risk a missed catch, never a false
        # alarm. Worth disclosing so a reader knows which cells were checked exactly vs. approximately.
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
