#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Regression guard for run-on-ec2.sh's poll-loop liveness probe.
#
# A single transient ssh failure (packet loss, momentarily overloaded sshd) on a box that is still
# alive must NOT be read as "box gone". box_reachable() must retry before giving up, and must still
# report unreachable when the box really is gone (every attempt fails).
#
# It sources ONLY box_reachable() out of run-on-ec2.sh (self-contained: uses only SSHOPT and its ip
# argument), with `ssh` and `sleep` stubbed - no AWS, no EC2, no real network.
#
# Run: bash lib/box_reachable_test.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$HERE/run-on-ec2.sh"
fail=0

tmp="$(mktemp)"
awk '/^box_reachable\(\) \{/{p=1} p{print} p&&/^\}/{exit}' "$SRC" > "$tmp"
if ! grep -q 'box_reachable()' "$tmp"; then
  echo "FAIL - could not extract box_reachable from run-on-ec2.sh"; rm -f "$tmp"; exit 1
fi
SSHOPT="-fake-opt"
sleep(){ :; }   # skip real waiting between retries in the test
# shellcheck source=/dev/null
source "$tmp"
rm -f "$tmp"

check(){ local n="$1" got="$2" want="$3"; if [ "$got" = "$want" ]; then echo "ok   - $n"; else echo "FAIL - $n: got [$got] want [$want]"; fail=1; fi; }

# (a) transient: first ssh call fails, second succeeds -> box_reachable must still report reachable.
_ssh_calls=0
ssh(){ _ssh_calls=$((_ssh_calls+1)); [ "$_ssh_calls" -eq 1 ] && return 255; return 0; }
box_reachable "10.0.0.1"; rc=$?
check "one transient ssh failure does not mark the box unreachable" "$rc" "0"

# (b) genuinely gone: every ssh call fails -> box_reachable must report unreachable.
ssh(){ return 255; }
box_reachable "10.0.0.1"; rc=$?
check "a box that never answers ssh is reported unreachable" "$rc" "1"

if [ "$fail" -eq 0 ]; then echo "box_reachable_test.sh: PASS"; else echo "box_reachable_test.sh: FAIL"; fi
exit "$fail"
