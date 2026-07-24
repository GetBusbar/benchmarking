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
    return any(m in err for m in BOOT_FAILURE_MARKERS)


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
