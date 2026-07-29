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
# The two numbers now come out of ONE climb over ONE state of the gateway (`run::sweep_cell`), so a
# genuine inversion means the throughput curve spiked between two doublings - which gateways do not
# do. Before that change they were two searches separated by a gateway restart, and three cells of
# the 2026-07-28 board published a "sustained" rate up to 7% above the "maximum" it was meant to sit
# under. This stays at 5% rather than 0 because the ceiling is refined BETWEEN rungs and its rate is
# a median of three windows there, so a point or two of disagreement is measurement, not a bug.
#
# THIS IS THE DECLARED SOURCE, AND THE SITE CARRIES ITS OWN COPY (ledger TOOL-02). The same ceiling is
# spelled `export const C6_GROSS_PCT = 5;` in site/check-consistency.mjs, because that gate runs in
# node with no python in reach and no build step to share a constant through. Two literals, one
# invariant, and nothing that noticed when they disagreed - tune one and the two gates quietly start
# policing different bars for the same inversion.
#
# The options were: emit a shared constants file from one side (a build step, and the site is not
# mine to re-point at it), or hand-sync and hope. The least-magic third option is the one taken here:
# both literals stay where they are, each readable on its own, and this audit PARSES the site's
# declaration and refuses to pass when the two disagree (`check_c6_bar_agrees_with_the_site`). No
# generated file, no import machinery, no cross-language build - just a gate that fails on drift, in
# the tool whose entire job is to fail on drift. The coupling is documented at this end; the site end
# carries the same note in its own comment block.
C6_GROSS_PCT = 5.0

# The site's copy of the same bar, and how to find it. Parsed, not imported: this is python reading a
# javascript literal, and the narrowness of the pattern is the point - if the site restates the
# constant in a shape this does not match, the check reports "could not find it", which is a
# violation, not a pass. A cross-check that goes quiet when it stops being able to look is the exact
# defect class this file exists to prevent.
SITE_C6_PATH = os.path.join("site", "check-consistency.mjs")
SITE_C6_RE = re.compile(r"^export const C6_GROSS_PCT\s*=\s*([0-9]+(?:\.[0-9]+)?)\s*;", re.M)

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


def median(v):
    s = sorted(v)
    return s[len(s) // 2] if len(s) % 2 else s[len(s) // 2 - 1]


# ── the invariants ────────────────────────────────────────────────────────────────────────────────
#
# Each takes one served cell and yields a string per violation. A check that can never yield is a
# check that is not doing anything, which is the defect class this whole file is about: if one of
# these stops firing on data that used to trip it, that is a finding, not a pass.


def check_sustained_not_above_peak(name, c):
    """The two throughput numbers summarise ONE sweep, so the gated one cannot exceed the ungated one.

    The 2026-07-28 board tripped this on apisix anthropic>anthropic (1.065x), kong openai>gemini
    (1.054x) and litellm-python openai>openai-responses (1.074x). The cause was not any gateway: the
    sustained leg was a second search that ran after the memory group cold-restarted the process, so
    the pair compared two different states of it.
    """
    p = c.get("perf") or {}
    mx, su = p.get("rps_max_proxy"), p.get("rps_sustained_20ms")
    if mx and su and mx > 0 and su > mx * (1 + C6_GROSS_PCT / 100):
        yield f"{name}: sustained {su:.0f} exceeds peak {mx:.0f} by {(su/mx-1)*100:.1f}%"


def check_peak_came_from_its_own_sweep(name, c):
    """A published peak must be a rate some window in its own sweep actually produced."""
    p = c.get("perf") or {}
    mx = p.get("rps_max_proxy")
    rates = [r["rps"] for r in (p.get("sweep_max_proxy") or []) if r.get("rps") is not None]
    if mx and rates and mx > max(rates):
        yield f"{name}: peak {mx:.0f} exceeds every window it came from (best {max(rates)})"


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


def check_rate_and_concurrency_travel_together(name, c):
    """A rate without the concurrency it was measured at cannot be re-derived or charted."""
    p = c.get("perf") or {}
    for rate, conc, label in (
        ("rps_sustained_20ms", "rps_sustained_20ms_concurrency", "sustained"),
        ("rps_max_proxy", "conc_at_peak", "peak"),
    ):
        r, k = p.get(rate), p.get(conc)
        if r is not None and k is None:
            yield f"{name}: {label} rate published with no concurrency beside it"
        if k is not None and r is None:
            yield f"{name}: {label} concurrency published with no rate beside it"


def check_rate_is_physically_possible(name, c):
    """A per-connection rate above `MAX_RPS_PER_CONNECTION` is a units error, not a fast gateway."""
    p = c.get("perf") or {}
    mx, k = p.get("rps_max_proxy"), p.get("conc_at_peak")
    if mx and k and k > 0 and mx / k > MAX_RPS_PER_CONNECTION:
        yield f"{name}: {mx:.0f} rps at c={k} is {mx/k:.0f} per connection"


def check_frames_have_a_stream_behind_them(name, c):
    """Frames per second with no sustained streams is a rate over a population of zero."""
    st = c.get("stream") or {}
    if st.get("cpu_fps") and not st.get("streams_sustained"):
        yield f"{name}: cpu_fps {st['cpu_fps']} published with streams_sustained={st.get('streams_sustained')}"


# THE DEFINITION OF DONE, as fields. Every metric a served cell's block may publish; a null on any
# of these with no `absences` entry beside it is a bare hole, which the board's owner has ruled out:
# "either this cell is measured and all data must be reported, or this cell wasn't tested and empty
# is expected - not a combo". Mirrors the engine's `absences()` lists in `engine/src/record.rs`
# (CellPerf, CellStream, CellMemory), field for field.
ABSENCE_CARRYING_FIELDS = {
    "perf": [
        "added_latency_p50_us", "added_latency_p99_us", "gateway_c1_p99_us", "direct_c1_p99_us",
        "rps_sustained_20ms", "rps_sustained_20ms_concurrency", "conc_at_sustained",
        "rps_max_proxy", "rps_max_proxy_concurrency", "conc_at_peak",
    ],
    "stream": [
        "added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us", "added_gap_p99_us",
        "streams_sustained", "streams_sustained_fps", "cpu_fps", "cpu_fps_concurrency",
    ],
    "memory": [
        "idle_rss_mib", "steady_state_rss_mib", "recovered_rss_mib", "peak_rss_mib",
        "peak_rss_hwm_mib", "time_to_plateau_s", "growth_rate_mib_per_min",
        # Newly coverable: these were bare `Option`s that collapsed the metric group's reason on the
        # way out, so a memory window that could not judge the plateau published two nulls nothing
        # could explain. They are `Measurement`s now and ride in the cell's absences map like every
        # other number, which is what lets this list hold them to the same bar.
        "plateaued", "load_s",
    ],
}

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


def check_declared_fields_are_carried(name, c):
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
                yield (f"{name}: {block}.{f} is OMITTED from the block (not null-with-reason, "
                       f"absent) - key-missing and measured are indistinguishable to every check")


def check_stream_capacity_is_a_number(name, c):
    """A streaming cell's capacity metrics are numbers (0 included), or a rig-class absence.

    The yield gate: streams_sustained and cpu_fps produced values (or measured zeroes) on every
    served streaming cell once the gate published failures as 0. An absence whose reason is
    `not_measured` here means a search quietly stopped producing - the exact silent-yield defect
    that shipped a board with cpu_fps on 1 of 16 served cells.
    """
    st = c.get("stream") or {}
    if st.get("stream_served") is not True:
        return
    absences = c.get("absences") or {}
    for f in ("streams_sustained", "cpu_fps"):
        if st.get(f) is None:
            reason = (absences.get(f"stream.{f}") or {}).get("reason")
            if reason not in CAPACITY_ABSENCE_OK:
                yield (f"{name}: stream.{f} is absent with reason {reason!r} on a served streaming "
                       f"cell - a gate failing everywhere is a measured 0, not a hole")


CELL_CHECKS = [
    check_sustained_not_above_peak,
    check_peak_came_from_its_own_sweep,
    check_sweep_carries_its_latency,
    check_ttft_percentiles_are_ordered,
    check_rate_and_concurrency_travel_together,
    check_rate_is_physically_possible,
    check_frames_have_a_stream_behind_them,
    check_no_bare_absence,
    check_declared_fields_are_carried,
    check_stream_capacity_is_a_number,
]


def parse_site_c6(text):
    """The site's declared C6 ceiling, read out of its source. None when it is not declared as
    expected - which the caller must treat as a failure, never as agreement."""
    m = SITE_C6_RE.search(text or "")
    return float(m.group(1)) if m else None


def check_c6_bar_agrees_with_the_site():
    """The two copies of the gross-inversion ceiling must be the same number (ledger TOOL-02).

    `C6_GROSS_PCT` is declared here and again in site/check-consistency.mjs, in two languages that
    share no build step. Both read 5 today and both were written by someone who believed 5 was the
    bar; nothing in either tree would have noticed if one of them had been tuned to 7 while the other
    stayed at 5, and the two gates would then have been policing different definitions of the same
    inversion - the site passing a cell this audit fails, or worse, the reverse.

    Parsing the sibling's literal is deliberately the whole mechanism. It needs no generated file, no
    codegen step and no edit to the site (which this tool does not own), and it fails in the one place
    a drift would be looked for. If the site is not on disk at all, or has restated the constant in a
    shape the pattern does not recognise, that is reported as a violation too: an audit that cannot
    see the thing it is comparing against has not agreed with it.
    """
    p = os.path.join(HERE, SITE_C6_PATH)
    try:
        with open(p) as fh:
            text = fh.read()
    except OSError as e:
        yield (f"C6 bar: cannot read the site's copy at {SITE_C6_PATH} ({e}) - the python bar is "
               f"{C6_GROSS_PCT}, and an unverifiable twin is not an agreeing twin")
        return
    site = parse_site_c6(text)
    if site is None:
        yield (f"C6 bar: {SITE_C6_PATH} no longer declares `export const C6_GROSS_PCT = <n>;` where "
               f"this can read it - the cross-check went blind, which is a drift, not a pass")
        return
    if site != C6_GROSS_PCT:
        yield (f"C6 bar: python C6_GROSS_PCT={C6_GROSS_PCT} but {SITE_C6_PATH} declares {site} - "
               f"two gates, one invariant, different bars")


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
    d = json.load(open(p))
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

    violations = collections.defaultdict(list)
    cells = 0
    for gw, (_path, d, _sha) in sorted(snaps.items()):
        for name, c in served_cells(d):
            cells += 1
            for check in CELL_CHECKS:
                for v in check(f"{gw} {name}", c):
                    violations[check.__name__].append(v)
        for v in check_declaration_matches_what_we_measured(gw):
            violations["check_declaration_matches_what_we_measured"].append(v)

    # Board-level, not per-cell: the C6 bar is a property of the two gates, not of any one reading.
    for v in check_c6_bar_agrees_with_the_site():
        violations["check_c6_bar_agrees_with_the_site"].append(v)

    print(f"engine {engine[:7]}  {len(snaps)} gateways  {cells} served cells")
    print(f"{len(CELL_CHECKS)} per-cell invariants + 1 per-gateway + 1 board-level invariant\n")

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
