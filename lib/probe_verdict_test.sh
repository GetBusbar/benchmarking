#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Regression guard for the PERSISTENT-TRANSIENT choke point (lib/probe_verdict.sh).
#
# THE FAIRNESS DEFECT THIS EXISTS FOR: matrix/run.sh's run_cell branched on the gateway's OWN advisory
# capability declaration (`cap <ingress> <egress>`, from the manifest's GW_MATRIX_CAP) at two points
# on the persistent-000/5xx path, so the IDENTICAL observation produced different published output:
#
#   observation                                  UNDECLARED cell        DECLARED cell
#   persistent 5xx, mock verifiably healthy      not_configured         not_verified
#   retry budget spent getting there             2 attempts x 10 s      3 attempts x 120 s
#
# Both halves are unfair. The verdict half published two different claims about the same evidence
# (and hid a real 5xx behind "we could not read it"); the budget half tried a self-declared cell 36x
# harder than its neighbour, so the gateway's own unverified claim bought it more measurement effort.
# Neither is visible in the published JSON, because both verdict classes are legitimate ones.
#
# THE CONTRACT under test:
#   1. The verdict depends ONLY on observables — final HTTP status + whether the recording mock was
#      healthy. Same observation -> same verdict, always.
#   2. `not_configured`/`probe_failed` is the ONE vocabulary for "the gateway answered a real status,
#      persistently, over a healthy rig". `not_verified`/`upstream_unreachable` is reserved for
#      curl-000 (no answer at all) and for a mock we could not confirm healthy.
#   3. There is exactly ONE retry budget, it takes no arguments, and every cell gets it.
#   4. matrix/run.sh keeps no second budget and no `cap` call on the transient path (structural: the
#      choke point cannot be bypassed by a call site that re-adds the branch).
#
# Run: bash lib/probe_verdict_test.sh   (no network, no docker, no gateway; instant)
set -u
B="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$B/lib/probe_verdict.sh"

FAILED=0
check(){ # label expected actual
  if [ "$2" = "$3" ]; then
    printf '  ok   %s\n' "$1"
  else
    printf '  FAIL %s: expected [%s] got [%s]\n' "$1" "$2" "$3"; FAILED=1
  fi
}
lacks(){ # label needle haystack
  case "$3" in
    *"$2"*) printf '  FAIL %s: still contains [%s]\n' "$1" "$2"; FAILED=1;;
    *) printf '  ok   %s\n' "$1";;
  esac
}

echo "probe_verdict_test: probe_transient — which statuses are retryable at all"
for s in 000 500 502 503 599; do
  LAST_STATUS="$s"; probe_transient && r=transient || r=answer
  check "status $s -> transient" "transient" "$r"
done
for s in 200 201 301 400 401 403 404 405 429; do
  LAST_STATUS="$s"; probe_transient && r=transient || r=answer
  check "status $s -> real answer (never retried)" "answer" "$r"
done

echo "probe_verdict_test: the verdict is a function of the OBSERVATION only"
# THE regression, stated directly: the function's whole signature is the observation. There is no
# third argument for the declaration, so a declared and an undeclared cell CANNOT diverge.
check "5xx + healthy mock -> the probe's honest answer" \
  "not_configured probe_failed" "$(persistent_transient_verdict 503 1)"
check "5xx + healthy mock, second gateway, same call" \
  "not_configured probe_failed" "$(persistent_transient_verdict 503 1)"
for s in 500 502 503 504 599; do
  check "HTTP $s + healthy mock -> not_configured" \
    "not_configured probe_failed" "$(persistent_transient_verdict "$s" 1)"
done
# THE equal-treatment assertion, stated the way the old code could fail it: hand the function a THIRD
# argument — the advisory declaration bit the old run_cell branched on (`cap "$cell" "$egress"`) — in
# both states. A declaration-aware implementation returns not_verified for the declared case; this one
# cannot even see it. Any observation, any claim, one verdict.
check "declared=0 alongside the same observation" \
  "not_configured probe_failed" "$(persistent_transient_verdict 503 1 0)"
check "declared=1 alongside the same observation" \
  "not_configured probe_failed" "$(persistent_transient_verdict 503 1 1)"
check "declared=1, HTTP 500"  "not_configured probe_failed" "$(persistent_transient_verdict 500 1 1)"
check "declared=1, HTTP 000 still cautious" \
  "not_verified upstream_unreachable" "$(persistent_transient_verdict 000 1 1)"
# ...and the cell's identity is equally inert.
check "cell identity is not an input (openai/openai)" \
  "not_configured probe_failed" "$(persistent_transient_verdict 503 1 openai openai)"
echo "probe_verdict_test: what stays not_verified (still purely observational)"
check "000 (no HTTP answer at all) + healthy mock" \
  "not_verified upstream_unreachable" "$(persistent_transient_verdict 000 1)"
check "5xx but the mock could not be confirmed healthy" \
  "not_verified upstream_unreachable" "$(persistent_transient_verdict 503 0)"
check "000 + sick mock" \
  "not_verified upstream_unreachable" "$(persistent_transient_verdict 000 0)"
# Defensive: a caller that loses an argument must fall back to the CAUTIOUS class, never to a
# published capability verdict on evidence we do not have.
check "no arguments at all -> cautious" \
  "not_verified upstream_unreachable" "$(persistent_transient_verdict)"
check "status only, no mock evidence -> cautious" \
  "not_verified upstream_unreachable" "$(persistent_transient_verdict 503)"

echo "probe_verdict_test: ONE budget, for every cell"
check "probe_transient_budget is the field default" "3 30" "$(probe_transient_budget)"
# It takes no arguments: passing a cell/egress cannot change what comes back. This is the structural
# guarantee — a future call site literally has nothing to hand it that could buy extra attempts.
check "budget ignores a cell identity (openai/openai)"   "3 30" "$(probe_transient_budget openai openai)"
check "budget ignores a cell identity (cohere/bedrock)"  "3 30" "$(probe_transient_budget cohere bedrock)"
check "budget ignores a declared flag"                   "3 30" "$(probe_transient_budget 1)"
# The whole budget is still overridable as a unit (verify-local.sh shrinks it for a local run) —
# but as a unit, never per cell.
( MATRIX_TRANSIENT_RETRIES=1 MATRIX_TRANSIENT_PAUSE=2
  source "$B/lib/probe_verdict.sh"
  check "env override applies to the ONE budget" "1 2" "$(probe_transient_budget)" ) || FAILED=1

echo "probe_verdict_test: matrix/run.sh keeps no second budget and no cap on the transient path"
RUN="$(cat "$B/matrix/run.sh")"
lacks "no MATRIX_PROBE_TRANSIENT_* budget survives"      "MATRIX_PROBE_TRANSIENT" "$RUN"
lacks "run.sh no longer defines probe_transient itself"  "probe_transient(){"     "$RUN"
# The transient block of run_cell must contain zero `cap` calls: extract it (from the retry comment to
# the emit_cell that closes the persistent-transient branch) and assert the declaration is absent.
BLOCK="$(awk '/TRANSIENT dead-socket retry/{f=1} f{print} f&&/^    return$/{exit}' "$B/matrix/run.sh")"
[ -n "$BLOCK" ] || { printf '  FAIL could not locate the transient block in matrix/run.sh\n'; FAILED=1; }
lacks "the retry+verdict block never calls cap()" '$(cap ' "$BLOCK"
lacks "the retry+verdict block never reads CAP[]" 'CAP['                          "$BLOCK"
# ...and it must route through the choke point rather than re-deriving the verdict inline.
case "$BLOCK" in
  *persistent_transient_verdict*) printf '  ok   the block calls persistent_transient_verdict\n';;
  *) printf '  FAIL the block does not call persistent_transient_verdict\n'; FAILED=1;;
esac
case "$BLOCK" in
  *probe_transient_budget*) printf '  ok   the block takes its budget from probe_transient_budget\n';;
  *) printf '  FAIL the block does not call probe_transient_budget\n'; FAILED=1;;
esac
# The one PERMITTED branch on the declaration must still be there: warm-up cell preference. Guard it
# so this fix is not "over-corrected" into deleting the legitimate use.
case "$RUN" in
  *'if [ "$(cap "$ing" "$1")" = 1 ]'*) printf '  ok   warm-up preference still consults the declaration (the permitted branch)\n';;
  *) printf '  FAIL the permitted warm-up branch on cap() is gone\n'; FAILED=1;;
esac

if [ "$FAILED" = 0 ]; then echo "probe_verdict_test: PASS"; else echo "probe_verdict_test: FAIL"; fi
exit "$FAILED"
