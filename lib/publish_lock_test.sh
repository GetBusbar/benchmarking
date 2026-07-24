#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Regression guard for run-on-ec2.sh's publish-lock TOKEN OWNERSHIP (audit R4-M1 / LOW-2).
#
# Round-3's M1 fix shipped the ownership check with `$$` as the owner token — but `$$` is IDENTICAL
# across every `&` background subshell of one orchestrator (only $BASHPID differs), so "release only if
# WE own the lock" never distinguished holders and provided ZERO cross-box protection. The fix writes a
# UNIQUE per-publish token (RUN_ID:tag:BASHPID:random) and only rmdir's when it still matches. This test
# proves (a) the token is unique per publish even across subshells that share $$, and (b) a NON-owner
# release (a peer that re-acquired after we timed out) does NOT delete the real holder's lockdir.
#
# It sources ONLY the two lock functions out of run-on-ec2.sh (they are self-contained: they use
# PUBLISH_LOCK / RUN_ID / BASHPID), on a temp lock path — no AWS, no EC2, no publish.
#
# Run: bash lib/publish_lock_test.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$HERE/run-on-ec2.sh"
fail=0

# Extract the two functions verbatim (from `publish_lock_acquire() {` through the close of
# `publish_lock_release`), so the test always exercises the SHIPPED implementation, not a copy.
tmp="$(mktemp)"
awk '/^publish_lock_acquire\(\) \{/{p=1} p{print} /^publish_lock_release\(\) \{/{r=1} r&&/^\}/{print "";exit}' "$SRC" > "$tmp"
# shellcheck disable=SC1090
if ! grep -q 'publish_lock_acquire()' "$tmp" || ! grep -q 'publish_lock_release()' "$tmp"; then
  echo "FAIL - could not extract the lock functions from run-on-ec2.sh"; rm -f "$tmp"; exit 1
fi
# Force the mkdir spin-lock path (the LIVE Darwin path this fix targets) by hiding flock.
command(){ if [ "$1" = "-v" ] && [ "$2" = "flock" ]; then return 1; fi; builtin command "$@"; }
RUN_ID="testrun"
PUBLISH_LOCK="$(mktemp -u)"
# shellcheck source=/dev/null
source "$tmp"
rm -f "$tmp"

check(){ local n="$1" got="$2" want="$3"; if [ "$got" = "$want" ]; then echo "ok   - $n"; else echo "FAIL - $n: got [$got] want [$want]"; fail=1; fi; }

# (a) two acquire/release cycles produce DISTINCT tokens (the $$-collision the fix closes).
publish_lock_acquire "[gwA]" echo >/dev/null; tokA="$PUBLISH_LOCK_TOKEN"; publish_lock_release
publish_lock_acquire "[gwB]" echo >/dev/null; tokB="$PUBLISH_LOCK_TOKEN"; publish_lock_release
if [ -n "$tokA" ] && [ -n "$tokB" ] && [ "$tokA" != "$tokB" ]; then echo "ok   - the per-publish token is UNIQUE across publishes (not a shared \$\$)"; else echo "FAIL - tokens collided: A=[$tokA] B=[$tokB]"; fail=1; fi

# (b) a NON-owner release must NOT delete a real holder's lockdir. Simulate: holder acquires (writes its
# token); an impostor with PUBLISH_LOCK_OWNED=1 but a DIFFERENT token calls release — the dir must survive.
publish_lock_acquire "[holder]" echo >/dev/null
holder_tok="$PUBLISH_LOCK_TOKEN"
check "holder created the lockdir" "$([ -d "${PUBLISH_LOCK}.d" ] && echo yes || echo no)" "yes"
# impostor state (as a stale/timed-out waiter that wrongly thinks it owns the lock):
PUBLISH_LOCK_OWNED=1 PUBLISH_LOCK_TOKEN="impostor:does-not-match" publish_lock_release
check "a non-owner release does NOT remove the holder's lockdir" "$([ -d "${PUBLISH_LOCK}.d" ] && echo yes || echo no)" "yes"
check "the holder's token is still in the lockdir" "$(cat "${PUBLISH_LOCK}.d/token" 2>/dev/null)" "$holder_tok"
# the true owner releases → dir gone.
PUBLISH_LOCK_OWNED=1 PUBLISH_LOCK_TOKEN="$holder_tok" publish_lock_release
check "the true owner release removes the lockdir" "$([ -d "${PUBLISH_LOCK}.d" ] && echo yes || echo no)" "no"

if [ "$fail" = 0 ]; then echo "all publish-lock token tests passed"; exit 0; fi
echo "PUBLISH-LOCK TESTS FAILED"; exit 1
