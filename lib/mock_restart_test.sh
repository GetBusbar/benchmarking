#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# lib/mock_restart_test.sh — the mock-restart choke point (audit #21).
#
# THE BUG THIS PINS. Four sites restarted the mock with the same line:
#
#     [ -n "$MOCK" ] && pkill -f "$MOCK" 2>/dev/null; sleep 1
#
# then relaunched immediately. `pkill` only SENDS a signal and returns at once, so `sleep 1` was a blind
# guess at how long the old mock needed to exit. When the guess was wrong the fresh mock hit
# `TcpListener::bind(..).expect("bind")`, panicked on EADDRINUSE and died; the harness then probed a
# dead port and recorded the cell "untestable / stream_mock_unready". In the 2026-07-25 field run that
# happened on 48 of 75 served cells, which emptied the matrix streaming lane for ALL 13 gateways and
# forced every streaming column on the board back onto the weeks-old standalone stream suite.
#
# mock_stop_wait replaces the blind sleep: signal, then POLL until the process is really gone AND the
# port really refuses a connection, escalating to SIGKILL halfway through the budget.
#
# (The call sites used to blame "no SO_REUSEADDR". That diagnosis was wrong and pointed at a fix that
# could not work: SO_REUSEADDR permits binding over a TIME_WAIT socket, never over a live LISTEN socket
# still held by a running process. These tests pin the behaviour that actually matters — WAITING.)
set -uo pipefail
B="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$B/lib/harness.sh"

fails=0
check(){ # label expected actual
  if [ "$2" = "$3" ]; then echo "ok   - $1"; else echo "FAIL - $1: expected '$2' got '$3'"; fails=$((fails+1)); fi
}

MOCK_PORT=9   # discard/never-listening in these tests unless a stub says otherwise
MOCK_STOP_WAIT_S=4

# ---- 1. no MOCK configured -> a no-op success (never a spurious failure) -------------------------
MOCK=""
mock_stop_wait; check "no MOCK configured -> success (no-op)" 0 $?

# ---- 2. the process is already gone and the port is closed -> immediate success ------------------
MOCK=/bin/true
PKILL_CALLS=0
pkill(){ PKILL_CALLS=$((PKILL_CALLS+1)); return 0; }
pgrep(){ return 1; }                    # nothing matching the mock binary
tmo(){ return 1; }                      # the /dev/tcp connect probe fails => port closed
sleep(){ :; }
mock_stop_wait; check "already-exited mock -> success" 0 $?
check "the polite signal is still sent exactly once" 1 "$PKILL_CALLS"

# ---- 3. THE REGRESSION: the process lingers, then exits — we must WAIT, not return early ---------
# This is the case the blind `sleep 1` got wrong. The old mock is still alive for the first few polls;
# mock_stop_wait must keep waiting and only report success once the port is genuinely free.
LINGER=3
pgrep(){ LINGER=$((LINGER-1)); [ "$LINGER" -gt 0 ]; }   # alive for 3 polls, then gone
tmo(){ return 1; }
mock_stop_wait; check "a lingering mock is WAITED for, then reported free" 0 $?

# ---- 4. the process is gone but SOMETHING still holds the port -> NOT free -----------------------
# Either signal alone is insufficient: a zombie holding the socket passes the pgrep test. Requiring
# both is what stops the harness relaunching onto an occupied port.
pgrep(){ return 1; }
tmo(){ return 0; }                      # a connect SUCCEEDS => something is listening
mock_stop_wait; check "port still accepting connections -> reported NOT free" 1 $?

# ---- 5. a mock that ignores SIGTERM is escalated to SIGKILL, once --------------------------------
SIG9=0
pkill(){ case " $* " in *" -9 "*) SIG9=$((SIG9+1));; esac; return 0; }
pgrep(){ return 0; }                    # never exits
tmo(){ return 1; }
mock_stop_wait; check "an unkillable mock eventually reports failure (never a silent pass)" 1 $?
check "SIGTERM is escalated to SIGKILL exactly once" 1 "$SIG9"

# ---- 6. the budget is honoured (a hung mock cannot stall the run forever) ------------------------
# With sleep stubbed the loop is instant; assert it TERMINATED rather than looping unbounded, which
# the exit above already proves. Assert the knob is respected by shrinking it to zero.
MOCK_STOP_WAIT_S=0
pgrep(){ return 0; }
mock_stop_wait; check "a zero budget returns immediately as not-free" 1 $?

[ "$fails" = 0 ] && echo "all mock-restart choke-point tests passed" || echo "$fails FAILURE(S)"
exit "$fails"
