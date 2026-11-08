#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Regression guard for run-on-ec2.sh's publish-lock token ownership.
#
# Each lock acquisition must write a unique per-publish token (RUN_ID:tag:BASHPID:random), not `$$`
# (which is identical across every `&` background subshell of one orchestrator), and release must only
# rmdir the lockdir when its token still matches. This test proves (a) the token is unique per publish
# even across subshells that share $$, and (b) a non-owner release (a peer that re-acquired after we
# timed out) does not delete the real holder's lockdir.
#
# It sources ONLY the two lock functions out of run-on-ec2.sh (they are self-contained: they use
# PUBLISH_LOCK / RUN_ID / BASHPID), on a temp lock path - no AWS, no EC2, no publish.
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

# (a) the token names the ACQUIRING SUBSHELL, not the shared $$. Two `&`-style subshells of one
# orchestrator share $$ and differ only in $BASHPID, so the assertion is on the pid field itself: the
# random suffix alone would keep two tokens distinct and hide a $$ regression (and so did the tag, when
# this test used two different ones). Same tag in both subshells, as two retries of one gateway would be.
if [ -n "${BASHPID:-}" ]; then
  for _i in 1 2; do
    ( publish_lock_acquire "[gw]" echo >/dev/null; printf '%s %s\n' "$PUBLISH_LOCK_TOKEN" "$BASHPID"; publish_lock_release ) > "${PUBLISH_LOCK}.sub$_i"
  done
  read -r tokA pidA < "${PUBLISH_LOCK}.sub1"; read -r tokB pidB < "${PUBLISH_LOCK}.sub2"
  rm -f "${PUBLISH_LOCK}.sub1" "${PUBLISH_LOCK}.sub2"
  _pid_of(){ local t="${1%:*}"; printf '%s' "${t##*:}"; }
  check "subshell A's token carries its own BASHPID" "$(_pid_of "$tokA")" "$pidA"
  check "subshell B's token carries its own BASHPID" "$(_pid_of "$tokB")" "$pidB"
  if [ "$pidA" != "$$" ] && [ "$pidB" != "$$" ] && [ "$tokA" != "$tokB" ]; then echo "ok   - the per-publish token is UNIQUE across subshells that share \$\$"; else echo "FAIL - tokens not distinguished by subshell: A=[$tokA] B=[$tokB] \$\$=$$"; fail=1; fi
else
  echo "skip - this bash has no BASHPID (pre-4), so the subshell-identity case cannot be constructed"
fi

# (c) a lock dir that cannot be CREATED (unwritable/missing parent) is not contention: acquire must
# fail at once, not spin the 600 s contention budget and then blame a peer.
_saved_lock="$PUBLISH_LOCK"; PUBLISH_LOCK="$(mktemp -u)/no-such-parent/lock"
_t0=$SECONDS; publish_lock_acquire "[nodir]" echo >/dev/null; _rc=$?
check "an uncreatable lock dir fails the acquire" "$_rc" "1"
check "and fails it immediately (no contention wait)" "$([ $((SECONDS - _t0)) -lt 5 ] && echo fast || echo slow)" "fast"
PUBLISH_LOCK="$_saved_lock"

# (b) a NON-owner release must NOT delete a real holder's lockdir. Simulate: holder acquires (writes its
# token); an impostor with PUBLISH_LOCK_OWNED=1 but a DIFFERENT token calls release - the dir must survive.
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
