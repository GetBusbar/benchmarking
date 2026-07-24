#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# BULLETPROOF collector guard: decide whether an INCOMING result file may overwrite the EXISTING
# committed one. A neutral board must never publish a 0 that came from OUR environment failing to
# build or boot a gateway, and must never let such a non-result overwrite previously-good data.
#
#   promote_guard.py <suite> <existing_path> <incoming_path>
#   exit 0  -> PROMOTE (incoming is a real result, or existing was absent/also-not-served)
#   exit 1  -> KEEP EXISTING (incoming is a boot/build failure that would clobber a good result)
#
# Rule: a gateway that failed to BUILD or never became READY (status "000", "failed to boot",
# "No such file or directory", "not listening") is a HARNESS/environment failure, not a measured
# gateway limitation. If the existing file was a real served result, that incoming non-result is
# refused. A gateway that BOOTED and then genuinely refused the probe (a real HTTP status) is a
# legitimate not-served and is allowed through.
import json
import sys

# A boot/build failure is our ENVIRONMENT never getting the gateway to answer at all - it must be
# distinguishable from a gateway that BOOTED and then honestly refused the probe (a real HTTP status),
# because refusing the honest not-served result and republishing last run's stale served=true biases
# the board in that gateway's favor (audit R4-M2).
#
# The ONLY authoritative in-band boot-failure sentinel is the one lib/harness.sh:127 emits when
# harness_launch_ready() exhausts every attempt: HARNESS_SERVE_ERR = "failed to boot after N attempts:
# ...". That string is anchored (it always LEADS the serve_error) and subsumes the per-attempt
# "not ready;" / "port ... not listening" diagnostics. The bare tokens we used to match - "venv",
# "not ready", "not listening", "no such file or directory" - are NOT harness sentinels: they only
# ever appear inside the verbatim `gw_diag=[...]` tail (or a gateway's own captured stderr), so a
# genuinely-booted gateway whose diagnostic body happens to contain any of them was misclassified as
# "never booted" and had its honest failure discarded. ("build failed" exits the suite with `exit 1`
# and never reaches a JSON field, so it was dead weight.) We anchor to the real sentinel; a
# connection-level failure (last_http_status "000", no HTTP response at all) is handled separately.
BOOT_FAILURE_MARKERS = (
    "failed to boot after",
)


def served_field(suite):
    return "governed_served" if suite == "governed" else "served"


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except Exception:
        return None


def is_served(doc, suite):
    if doc is None:
        return False
    if suite == "matrix":
        # matrix: "served" at all if the gateway configured+served any upstream cell.
        ups = doc.get("upstreams") or {}
        for u in ups.values():
            if isinstance(u, dict) and u.get("served"):
                return True
        return bool(doc.get("cells"))
    return doc.get(served_field(suite)) is True


def _matrix_cells(doc):
    """Yield every cell dict in a matrix result: the top-level v1-compat `cells` row plus every
    `upstreams.<egress>.cells.<ingress>` cell. Cells may be duplicated across the two shapes; that is
    fine — we only inspect their status/served fields."""
    cells = doc.get("cells")
    if isinstance(cells, dict):
        for c in cells.values():
            if isinstance(c, dict):
                yield c
    ups = doc.get("upstreams")
    if isinstance(ups, dict):
        for u in ups.values():
            if isinstance(u, dict):
                ucells = u.get("cells")
                if isinstance(ucells, dict):
                    for c in ucells.values():
                        if isinstance(c, dict):
                            yield c


def _matrix_all_dead(doc):
    """MEDIUM-R2-3: matrix emits NO top-level `last_http_status` (status lives per-cell,
    matrix/run.sh emit_cell), so the guard's `status == "000"` connection-level branch is dead for the
    sole producer. A genuinely all-dead matrix run — every cell reached a dead socket (status "000") and
    NOT ONE served — whose top-level serve_error happens to lack the anchored "failed to boot after"
    marker would otherwise promote OVER a prior served=true result (the R4-M2 bias this guard exists to
    prevent). Detect that connection-level boot failure directly from the per-cell statuses.

    CONSERVATIVE by design: require BOTH (a) no served cell anywhere, AND (b) at least one cell present
    with a status, AND (c) EVERY cell that carries a status shows "000". A legitimately-partial matrix
    (some cell served, or some cell answered with a real HTTP status like 404/501) is NOT all-dead and
    is allowed through as an honest not-served result."""
    saw_status = False
    for c in _matrix_cells(doc):
        if c.get("served") is True or c.get("served") == "verified":
            return False  # something served → not all-dead
        st = c.get("status")
        if st in (None, ""):
            continue
        saw_status = True
        if str(st) != "000":
            return False  # a cell got a real HTTP response → gateway booted, honest not-served
    return saw_status


def is_boot_failure(doc, suite):
    """True when the incoming non-result is our environment failing to build/boot the gateway,
    as opposed to a gateway that booted and honestly refused the probe."""
    if doc is None:
        return True  # no/garbage file is not a publishable result
    if is_served(doc, suite):
        return False
    status = str(doc.get("last_http_status", "") or "")
    err = " ".join(
        str(doc.get(k, "") or "")
        for k in ("serve_error", "error", "governed_note", "verdict_note")
    ).lower()
    if status == "000":
        return True
    if any(m in err for m in BOOT_FAILURE_MARKERS):
        return True
    # matrix has no top-level last_http_status, so mirror the "000" branch at the per-cell level: an
    # all-dead matrix (every cell a connection-level 000, none served) is a boot/transport failure.
    if suite == "matrix" and _matrix_all_dead(doc):
        return True
    return False


def main():
    if len(sys.argv) != 4:
        print("usage: promote_guard.py <suite> <existing> <incoming>", file=sys.stderr)
        return 0  # fail-open on misuse: do not block the pipeline, just promote
    suite, existing_path, incoming_path = sys.argv[1], sys.argv[2], sys.argv[3]
    incoming = load(incoming_path)
    if incoming is None:
        # no incoming result at all -> nothing to promote, keep whatever exists
        return 1
    existing = load(existing_path)
    # If the incoming file is a genuine boot/build failure AND the existing file was a real
    # served result, REFUSE the overwrite. Otherwise promote.
    if is_boot_failure(incoming, suite) and is_served(existing, suite):
        print(
            f"GUARD: refusing to overwrite served={existing.get(served_field(suite))} "
            f"{suite} result with a boot/build failure "
            f"(status={incoming.get('last_http_status')}, "
            f"err={(incoming.get('serve_error') or '')[:80]})",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
