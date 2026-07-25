#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# THE ONE CHOKE POINT for "the gateway kept answering 000/5xx — how long do we keep trying, and what
# do we publish when we stop".
#
# THE FAIRNESS DEFECT THIS FILE EXISTS FOR (it corrupted published verdicts):
# matrix/run.sh's run_cell consulted the gateway's OWN advisory capability declaration
# (GW_MATRIX_CAP, read through `cap <ingress> <egress>`) at TWO points on the persistent-transient
# path:
#
#     _retries="$MATRIX_TRANSIENT_RETRIES" _pause="$MATRIX_TRANSIENT_PAUSE"
#     if [ "$(cap "$cell" "$egress")" != 1 ]; then                    # <-- effort depended on the claim
#       _retries="$MATRIX_PROBE_TRANSIENT_RETRIES"; _pause="$MATRIX_PROBE_TRANSIENT_PAUSE"
#     fi
#     ...
#     if [ "$_mock_ok" = 1 ] && [ "$LAST_STATUS" != 000 ] && [ "$(cap "$cell" "$egress")" != 1 ]; then
#       served=not_configured                                          # <-- verdict depended on the claim
#     else
#       served=not_verified
#
# So the IDENTICAL observation — the gateway answered a real 5xx, persistently, with the recording
# mock verifiably healthy — was published as `not_configured` (probe evidence, the gateway's honest
# answer) on a cell the manifest left undeclared, and as `not_verified` (harness could not read it)
# on a cell the manifest declared. And the declared cell was ALSO tried harder: 3 attempts x 120 s
# versus 2 x 10 s. A gateway's own unverified marketing claim therefore moved both the published
# verdict AND the measurement effort spent on it. That is the harness failing to treat equals
# equally, and it is invisible in the published JSON because both verdicts are legitimate classes.
#
# THE CONTRACT enforced here:
#   1. VERDICT FROM OBSERVABLES ONLY. persistent_transient_verdict() takes exactly the three things
#      the rig OBSERVED — the final HTTP status, whether the recording mock was healthy, and nothing
#      else. It has no access to `cap`, to $GW_MATRIX_CAP, or to the cell's identity, so the
#      declaration cannot reach the classification even by accident.
#   2. ONE BUDGET FOR EVERY CELL. probe_transient_budget() takes NO arguments at all. Every one of
#      the 36 cells gets the same number of attempts and the same pause between them.
#
# WHICH VERDICT (the single honest vocabulary for "probed it, persistent 5xx, mock healthy"):
# `not_configured` + reason `probe_failed`, carrying the status and first response bytes as evidence.
# Rationale: the gateway ANSWERED. A real HTTP status (not curl's 000) means the gateway accepted the
# connection, routed the request and produced a response of its own; a verifiably-recording mock means
# the failure was not our rig going away underneath it. Reproduced across the whole retry budget, that
# is a DETERMINISTIC application-level rejection of this ingress/egress pairing — an observation, and
# the most informative one we have. `not_verified` means "the harness could not get a fair reading",
# which is simply false here and would throw away the 5xx evidence rather than publish it.
# `not_configured` is grey, never a red: no gateway is failed on such a cell, the pairing just does
# not light up, and probe_note carries the raw status so a reader can judge for themselves.
#
# WHAT STAYS `not_verified` (still purely observational): curl-level 000 — no HTTP answer at all, so
# the gateway may never have been reached — and any status observed while the recording mock could
# not confirm it was healthy, because then the upstream really may have been missing.
#
# WHICH BUDGET: 3 attempts, 30 s apart, for every cell. Attempts: 3 is the number the harness already
# judged sufficient to ride out a dead upstream socket (d35cae5). Pause: 30 s comfortably outclasses
# the socket-recycle window the pause exists for (the leg-3 post-load re-verify in the same file rides
# out the same class of dead socket on 5 attempts 1 s apart), while keeping the pathological case —
# all 36 cells persistently transient — at ~36 min per gateway. The old declared-cell budget
# (2 x 120 s) would cost ~2.4 h there, which does not merely waste time: it pushes later egress
# columns past HARNESS_SUITE_CEIL_S and converts them into `suite_ceiling` not_verified. That is the
# same fairness defect wearing a different hat, so the uniform budget is chosen to fit inside the
# ceiling rather than to eat it.
#
# The one branch on the declaration that REMAINS legitimate, and is deliberately not in this file:
# whether a cell is probed at all / which cell an egress column warms up on. That is "what do we
# spend a probe on", not "what did we see" — see matrix/run.sh's cap section.
#
# Guarded by lib/probe_verdict_test.sh (CI: .github/workflows/bench-tests.yml).
# Sourced by matrix/run.sh; depends on nothing (no manifest, no globals).

# A TRANSIENT failure is a transport/reachability problem, NOT an answer: the gateway never handed us
# a real application response we could judge. In this controlled rig the upstream is always the local
# mock, so a 5xx or a curl-level 000 is the gateway failing to REACH its upstream (a dead socket after
# a mock restart, an upstream-pool hiccup, a connection reset), never the gateway "answering wrongly".
# We must retry such a probe BEFORE recording anything - never publish a transient blip as a red, then
# re-run the whole box. A 2xx/3xx/4xx is a real application response (right or wrong) and is NOT retried.
# Reads $LAST_STATUS (set by matrix/run.sh's probe()) so every call site tests it identically.
probe_transient(){ case "$LAST_STATUS" in 000|5[0-9][0-9]) return 0 ;; *) return 1 ;; esac; }

# THE budget. One pair of knobs, one value each, no per-cell variant. Overridable by the environment
# for a fast local verify (verify-local.sh) — but only as a whole, never per cell.
MATRIX_TRANSIENT_RETRIES="${MATRIX_TRANSIENT_RETRIES:-3}"   # total attempts on ANY cell
MATRIX_TRANSIENT_PAUSE="${MATRIX_TRANSIENT_PAUSE:-30}"      # seconds between them, on ANY cell

# probe_transient_budget -> "<attempts> <pause_seconds>"
# Deliberately takes NO arguments: there is nothing about a particular cell that may change how hard
# the harness tries. Callers read both fields from this one function so a per-cell budget cannot be
# reintroduced at a call site.
probe_transient_budget(){ printf '%s %s' "$MATRIX_TRANSIENT_RETRIES" "$MATRIX_TRANSIENT_PAUSE"; }

# persistent_transient_verdict <last_status> <mock_recording:1|0> -> "<served> <reason>"
# Called ONCE, after the whole budget is spent and the probe is still transient. Arguments are the
# complete set of observables it is allowed to consider; the cell's identity and the manifest's
# capability declaration are deliberately NOT among them.
persistent_transient_verdict(){
  local status="${1:-000}" mock_ok="${2:-0}"
  if [ "$mock_ok" = 1 ] && [ "$status" != 000 ]; then
    # The gateway answered, repeatedly, over a healthy rig: a deterministic application-level
    # rejection of this pairing. Grey with the evidence, never a red.
    printf 'not_configured probe_failed'
  else
    # No HTTP answer at all, or we could not confirm the mock was healthy: the harness cannot claim
    # to have read this cell. Never graded either way.
    printf 'not_verified upstream_unreachable'
  fi
}
