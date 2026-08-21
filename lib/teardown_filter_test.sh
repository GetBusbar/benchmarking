#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Regression guard for teardown()'s shared-key lifetime filter in run-on-ec2.sh.
#
# The shared keypair must outlive every box that uses it, not merely the invocation that created it.
# teardown() deletes the key ONLY when no OTHER run's bench box is still alive, and it decides "other
# run" with an awk filter over `describe-instances` output of the form `<InstanceId>\t<run-tag-value>`:
#
#     awk -v r="$RUN_ID" 'NF && $2!=r {print $1}'
#
# i.e. print the instance id of every live bench box whose run tag is NOT this run's. This is the exact
# guard the 2026-08-21 key-stranding incident was fixed with (a 6-cell run deleted the key out from
# under a concurrent 36-cell run). A future edit that swapped `$2!=r` for `$1!=r`, or dropped the `NF`
# guard, would silently reintroduce it with no test failure - so this pins the filter itself.
#
# It extracts the awk PROGRAM verbatim out of run-on-ec2.sh (same technique as publish_lock_test.sh /
# box_reachable_test.sh) and runs it against synthetic describe-instances rows - no AWS, no EC2.
#
# Run: bash lib/teardown_filter_test.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$HERE/run-on-ec2.sh"
fail=0
check(){ local n="$1" got="$2" want="$3"; if [ "$got" = "$want" ]; then echo "ok   - $n"; else echo "FAIL - $n: got [$got] want [$want]"; fail=1; fi; }

# Extract the single-quoted awk program verbatim from the _live_others filter line.
prog="$(grep -m1 "awk -v r=" "$SRC" | sed "s/.*awk -v r=\"[^\"]*\" '\([^']*\)'.*/\1/")"
if [ -z "$prog" ] || [ "$prog" = "$(grep -m1 'awk -v r=' "$SRC")" ]; then
  echo "FAIL - could not extract the _live_others awk program from run-on-ec2.sh"; exit 1
fi
echo "     (extracted filter: $prog)"

# Synthetic describe-instances output: <InstanceId>\t<run-tag-value>. Our run is "mine".
# Two OTHER-run boxes are live, one of OUR-run boxes is live (already terminated above in real teardown,
# but present here to prove it is excluded), plus a blank line the NF guard must drop.
input="$(printf 'i-other1\tpeerA\ni-mine1\tmine\ni-other2\tpeerB\n\ni-notag\tNone\n')"
out="$(printf '%s\n' "$input" | awk -v r="mine" "$prog" | tr '\n' ' ')"

check "OUR OWN run's box is EXCLUDED (never treated as an 'other' still using the key)" \
      "$(printf '%s' "$out" | grep -c 'i-mine1')" "0"
check "another run's live box IS kept as a reason to preserve the shared key" \
      "$(printf '%s' "$out" | grep -c 'i-other1')" "1"
check "a second other-run box is kept too" \
      "$(printf '%s' "$out" | grep -c 'i-other2')" "1"
check "a box with no run tag (None) is 'not ours' and kept" \
      "$(printf '%s' "$out" | grep -c 'i-notag')" "1"
check "the blank line is dropped by the NF guard (no empty id printed)" \
      "$(printf '%s\n' "$input" | awk -v r="mine" "$prog" | grep -c '^$')" "0"

# The whole point: with ONLY our own boxes live, the filter yields nothing, so the key would be deleted.
only_ours="$(printf 'i-mine1\tmine\ni-mine2\tmine\n' | awk -v r="mine" "$prog")"
check "with only OUR run's boxes live, the filter is empty (key is safe to delete)" \
      "$([ -z "$only_ours" ] && echo empty || echo "$only_ours")" "empty"

if [ "$fail" = 0 ]; then echo "teardown_filter_test.sh: PASS"; exit 0; fi
echo "teardown_filter_test.sh: FAIL"; exit 1
