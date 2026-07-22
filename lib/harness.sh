#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# SHARED HARNESS ROBUSTNESS LAYER — sourced by every suite (perf/memory/stream/streamcpu/xlate/
# governed/matrix). It exists so a flaky gateway boot or an unresponsive gateway under load can NEVER
# (a) zero out a gateway on a single transient boot failure, or (b) hang a suite indefinitely. Both
# guarantees are implemented ONCE here and applied uniformly to every gateway — no per-manifest
# special-casing, the same retry count and timeout policy for all.
#
# WHAT IT PROVIDES
#   harness_launch_ready <launch_fn> <ready_fn> [launch_arg...]
#       Robust boot with retry. Runs the gateway's OWN launch hook (gw_launch, gw_governed_launch,
#       gw_matrix_egress <dialect>, ...) then the caller's readiness probe. On failure it kills any
#       partial process (gw_stop), captures diagnostics (gw_diag tail + port state), backs off, and
#       RETRIES the full launch up to HARNESS_BOOT_ATTEMPTS times. Only after all attempts fail does
#       it return non-zero with an honest diagnostic in HARNESS_SERVE_ERR. A gateway that boots on a
#       later attempt proceeds normally and produces real data. Never fabricates a served result.
#
#   tmo <seconds> <cmd...>
#       Hard-timeout wrapper for a single gateway-facing probe (a ugen invocation, a matrix cell
#       curl, ...). If <cmd> does not finish within <seconds> it is killed and tmo returns non-zero,
#       so a probe against an unresponsive gateway fails FAST instead of blocking the suite. Portable:
#       uses coreutils `timeout` when present, else a pure-bash watchdog fallback.
#
#   probe_budget <dur_seconds>
#       The hard-timeout budget (seconds) to give a single probe whose load window is <dur_seconds>.
#       = dur + HARNESS_PROBE_GRACE (slack for warm-up/connection teardown + the loadgen's own tail
#       request timeout). A gateway that stops responding mid-window is thus bounded to dur+grace, not
#       the loadgen's full 30s/120s client timeout multiplied across every hung worker.
#
#   suite_deadline_start   /   suite_deadline_expired
#       Overall per-suite wall-clock ceiling backstop (HARNESS_SUITE_CEIL_S, default 45 min). A suite
#       polls suite_deadline_expired at each sweep point; if the ceiling is crossed it stops probing,
#       records what it has, and moves on — so a pathological gateway can never wedge run-all.sh.
#
# TUNABLES (env, same for every gateway so the field stays fair):
#   HARNESS_BOOT_ATTEMPTS   boot attempts before served=false            (default 3)
#   HARNESS_BOOT_BACKOFF_S  seconds between boot attempts (grows x attempt) (default 3)
#   HARNESS_PROBE_GRACE     seconds added to a probe's -d for its hard timeout (default 45)
#   HARNESS_SUITE_CEIL_S    per-suite wall-clock ceiling seconds         (default 2700 = 45 min)

HARNESS_BOOT_ATTEMPTS="${HARNESS_BOOT_ATTEMPTS:-3}"
HARNESS_BOOT_BACKOFF_S="${HARNESS_BOOT_BACKOFF_S:-3}"
HARNESS_PROBE_GRACE="${HARNESS_PROBE_GRACE:-45}"
HARNESS_SUITE_CEIL_S="${HARNESS_SUITE_CEIL_S:-2700}"

# ── tmo: hard timeout around one command ────────────────────────────────────────────────────────
# Kills the command (and, via a fresh process group when possible, its children — a ugen probe forks
# nothing but a docker/native gateway probe path might) if it runs longer than <seconds>. Returns the
# command's own status on completion, or non-zero (124, matching coreutils timeout) if it timed out.
if command -v timeout >/dev/null 2>&1; then
  tmo(){ local s="$1"; shift; timeout -k 5 "${s}s" "$@"; }
else
  # Pure-bash fallback: run the command in the background, start a killer that fires after <seconds>,
  # wait on the command, then cancel the killer. SIGTERM first, SIGKILL after a short grace.
  tmo(){
    local s="$1"; shift
    "$@" & local cmd_pid=$!
    ( sleep "$s"; kill -TERM "$cmd_pid" 2>/dev/null; sleep 5; kill -KILL "$cmd_pid" 2>/dev/null ) & local k=$!
    local rc=0; wait "$cmd_pid" 2>/dev/null || rc=$?
    kill "$k" 2>/dev/null; wait "$k" 2>/dev/null
    return "$rc"
  }
fi

# ── probe_budget: seconds a single -d<dur> probe is allowed before we call it hung ────────────────
probe_budget(){ echo $(( ${1:-10} + HARNESS_PROBE_GRACE )); }

# ── per-suite wall-clock ceiling backstop ─────────────────────────────────────────────────────────
suite_deadline_start(){ HARNESS_SUITE_DEADLINE=$(( $(date +%s) + HARNESS_SUITE_CEIL_S )); }
suite_deadline_expired(){
  [ -n "${HARNESS_SUITE_DEADLINE:-}" ] || return 1
  [ "$(date +%s)" -ge "$HARNESS_SUITE_DEADLINE" ]
}

# ── _harness_port_state: cheap, dependency-light snapshot of the gateway data port ────────────────
# Used purely as failure evidence (is anything listening on GW_PORT at all?). Tries ss, then a bash
# /dev/tcp connect probe. Never blocks: the connect probe is itself timeout-wrapped.
_harness_port_state(){
  local port="${GW_PORT:-8080}"
  if command -v ss >/dev/null 2>&1; then
    local l; l="$(ss -ltnH "( sport = :$port )" 2>/dev/null | head -1)"
    [ -n "$l" ] && { echo "port $port LISTEN"; return; }
  fi
  if tmo 2 bash -c "exec 3<>/dev/tcp/127.0.0.1/$port" 2>/dev/null; then echo "port $port connectable"
  else echo "port $port not listening"; fi
}

# ── harness_launch_ready: THE robust boot-with-retry, shared by every suite ───────────────────────
# Usage: harness_launch_ready <launch_fn> <ready_fn> [launch_arg...]
#   <launch_fn>  a shell function that (re)launches the gateway. It is the gateway's OWN hook —
#                gw_launch / gw_governed_launch / gw_matrix_egress — so this works for docker, pip,
#                cargo, native alike; we never assume a process model, we just re-run the hook.
#   <ready_fn>   a shell function returning 0 once the gateway answers its readiness probe. It must
#                do its OWN internal wait (the existing per-suite curl-200 loop, already -m3 bounded),
#                and must NOT loop forever — a bounded ~60x1s loop is expected.
#   launch_arg   optional args passed through to <launch_fn> (e.g. the matrix egress dialect).
# Returns 0 on the first attempt that both launches AND passes readiness. On total failure returns 1
# and sets HARNESS_SERVE_ERR to an honest "failed to boot after N attempts" string with the captured
# per-attempt diagnostics. Between attempts it calls gw_stop (partial-process kill) + backs off.
harness_launch_ready(){
  local launch_fn="$1" ready_fn="$2"; shift 2
  local attempt rc diag port note
  HARNESS_SERVE_ERR=""
  local tries=""
  for attempt in $(seq 1 "$HARNESS_BOOT_ATTEMPTS"); do
    [ "$attempt" -gt 1 ] && log "[${GATEWAY:-gw}] boot retry $attempt/$HARNESS_BOOT_ATTEMPTS (previous attempt did not become ready)"
    # Re-run the gateway's own launch hook. A nonzero launch (config/build/mint failure) is itself a
    # failed attempt — capture it and retry rather than bailing.
    rc=0; "$launch_fn" "$@" || rc=$?
    if [ "$rc" = 0 ] && "$ready_fn"; then
      [ "$attempt" -gt 1 ] && log "[${GATEWAY:-gw}] booted on attempt $attempt"
      return 0
    fi
    port="$(_harness_port_state 2>/dev/null)"
    diag="$(gw_diag 2>&1 | tail -n 12 | tr '\n' ' ' | head -c 500)"
    note="attempt $attempt: launch_rc=$rc, not ready; $port; diag=[$diag]"
    log "[${GATEWAY:-gw}] boot $note"
    tries="${tries}${tries:+ || }$note"
    # Kill any partial process before the next attempt so a half-bound listener can't wedge the retry.
    gw_stop 2>/dev/null || true
    if [ "$attempt" -lt "$HARNESS_BOOT_ATTEMPTS" ]; then
      sleep "$(( HARNESS_BOOT_BACKOFF_S * attempt ))"
    fi
  done
  HARNESS_SERVE_ERR="failed to boot after $HARNESS_BOOT_ATTEMPTS attempts: $tries"
  log "[${GATEWAY:-gw}] WARNING served=false — $HARNESS_SERVE_ERR"
  return 1
}
