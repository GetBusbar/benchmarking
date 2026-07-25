#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# THE PER-CELL MEMORY WINDOW, driven end to end against a SYNTHETIC RSS curve.
#
# WHY THIS EXISTS. lib/plateau_test.sh proves the steadiness ARITHMETIC (given a window of samples, is
# it steady, and at what rate is it growing). It cannot prove the thing that actually decides what gets
# published: the harness WIRING around it - that the trailing window is taken from the LOAD phase alone,
# that a plateau is never declared before a full window of load samples exists, that a gateway which
# never settles is published as plateaued:false WITH its growth rate rather than as a gap, and that a
# load which delivered nothing is published as nothing rather than as a beautifully flat plateau.
#
# THE FAILURE THAT MOTIVATES EVERY CASE BELOW is the same one: a memory window measures an IDLE process
# and certifies it. An idle curve is perfectly steady, so a trailing window that includes idle samples,
# or a load that silently stopped delivering, both produce "plateaued: true" on a gateway nobody
# measured under load. That is worse than a missing number, because it is a confident wrong one.
#
# WHY IT DRIVES THE REAL FUNCTION. matrix/run.sh executes a whole benchmark when sourced, so the window
# cannot simply be imported. The function text is extracted verbatim from matrix/run.sh and eval'd here
# against stubs for everything outside it (gateway launch, mock, load generator) and a SYNTHETIC gw_rss
# whose shape each case chooses. Nothing about the decision logic is re-implemented in this file: if the
# extraction ever stops finding the function, the test fails loudly rather than passing vacuously.
#
# WHY SYNTHETIC RSS AT ALL. The measurement reads /proc through the gateway's own gw_rss hook, so on any
# host without /proc (a macOS dev box) every reading is legitimately unmeasurable and the plateau lanes
# are all null - which means a local end-to-end run can prove the plumbing but can NEVER exercise a
# plateau, a leak rate or a steady state. Feeding a chosen curve in through the same hook exercises all
# three deterministically, on any host, in seconds.
#
# Run: bash lib/matrix_memory_test.sh   (no network, no docker; ~1 minute of real sleeps)
set -u
B="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$B/lib/harness.sh"
source "$B/lib/plateau.sh"

FAILED=0
check(){ # label expected actual
  if [ "$2" = "$3" ]; then printf '  ok   %s\n' "$1"
  else printf '  FAIL %s: expected [%s] got [%s]\n' "$1" "$2" "$3"; FAILED=1; fi
}
checkf(){ # label predicate-description actual  (predicate evaluated by the caller)
  if [ "$2" = 1 ]; then printf '  ok   %s\n' "$1"
  else printf '  FAIL %s (got [%s])\n' "$1" "$3"; FAILED=1; fi
}

# ── the function under test, extracted verbatim ──────────────────────────────────────────────────
extract_fn(){ # name -> its definition from matrix/run.sh, or nothing
  awk -v n="$1(){" 'index($0,n)==1{p=1} p{print} p&&/^}$/{exit}' "$B/matrix/run.sh"
}
SRC=""
for fn in _mem_pad_body _mem_median _mem_rate_or_null matrix_cell_memory; do
  t="$(extract_fn "$fn")"
  case "$t" in
    *"$fn(){"*) SRC="$SRC
$t";;
    *) echo "matrix_memory_test: FAIL - could not extract $fn() from matrix/run.sh (has it been renamed?)"; exit 1;;
  esac
done
eval "$SRC"

# ── stubs for everything OUTSIDE the window ──────────────────────────────────────────────────────
# Each one stands for a piece of rig the window drives but does not decide: the launch, the mock, the
# load generator. They are deliberately trivial so that anything this test observes comes from the
# window's own logic.
log(){ :; }
# json_escape lives in matrix/run.sh's JSON writer, not in the shared layer; the window only uses it on
# prose (the protocol string), so a pass-through with the quote/backslash escaping is enough here.
json_escape(){ printf '%s' "${1:-}" | sed 's/\\/\\\\/g; s/"/\\"/g'; }
suite_deadline_expired(){ return 1; }
harness_launch_ready(){ return 0; }        # the cold restart "succeeds" - a real one is a rig concern
launch_egress(){ return 0; }
mock_start_plain(){ :; }
mock_start_record(){ return 0; }
_sw_mock_ready(){ return 0; }
# The load generator: sleeps the leg it was asked for (so the sampler accumulates real samples over real
# time) and reports FAKE_OK successful requests, which is exactly the evidence the delivery gate reads.
sweep_probe_raw(){ sleep "$3"; printf 'rps=100 fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=%s\n' "$FAKE_OK"; }

# ── the synthetic RSS curve, fed through the gateway's own hook ───────────────────────────────────
# CURVE=climb : rises forever (a leak: never plateaus, so the cap is reached and the rate is the finding)
# CURVE=settle: rises for RISE_S seconds from the window's start, then holds flat (a working set)
CURVE=settle; RISE_S=4; T_ORIGIN=0; FAKE_OK=1000
gw_rss(){
  awk -v now="$(date +%s)" -v t0="$T_ORIGIN" -v mode="$CURVE" -v rise="$RISE_S" 'BEGIN{
    e = now - t0; if (e < 0) e = 0
    if (mode == "settle" && e > rise) e = rise
    printf "%.1f", 100 + 5*e
  }'
}
gw_hwm(){ awk -v v="$(gw_rss)" 'BEGIN{ printf "%.1f", v + 10 }'; }

# ── the fast local profile: same code, seconds instead of minutes ────────────────────────────────
MATRIX_MEMORY=1
MEM_IDLE_S=2; MEM_SETTLE_S=2; MEM_SAMPLE_S=1
MEM_PLATEAU_WINDOW_S=4; MEM_PLATEAU_TREND_PCT=1; MEM_PLATEAU_RANGE_PCT=2
MEM_PLATEAU_CAP_S=12; MEM_LOAD_LEG_S=2
MEM_CAP_MIB=40000
MATRIX_MEM_CONC=8; MATRIX_MEM_PAYLOAD=64
MEM_STOP=""; MEM_SP=""; MEMORY_WINDOW_S=0; CELL_MEM_JSON=""
GATEWAY=test-fixture; GW_PORT=1; PSIZE=0; SWEEP_BODY=""; CURL_H=(); WARM_OK=1; WARM_LAST=200; SERVE_ERR=""
HARNESS_SUITE_CEIL_S=99999

# field <key> -> that key's value out of the CELL_MEM_JSON just produced (JSON-parsed, never regexed).
field(){ printf '%s' "{${CELL_MEM_JSON#, }}" | python3 -c '
import json,sys
d=json.load(sys.stdin)["memory"]
v=d.get(sys.argv[1])
print("null" if v is None else ("true" if v is True else ("false" if v is False else v)))' "$1"; }
series_ok(){ # 1 when t_s is present, starts at 0-ish and increases monotonically across the lifecycle
  printf '%s' "{${CELL_MEM_JSON#, }}" | python3 -c '
import json,sys
s=json.load(sys.stdin)["memory"]["rss_series"]
ts=[p["t_s"] for p in s]
print(1 if len(ts)>=6 and ts[0]<=1 and all(b>a for a,b in zip(ts,ts[1:])) else 0)'
}

# ── CASE 1: a gateway whose working set settles ──────────────────────────────────────────────────
echo "matrix_memory_test: a settling gateway plateaus, and the plateau value is published"
CURVE=settle; RISE_S=4; FAKE_OK=1000; T_ORIGIN=$(date +%s)
matrix_cell_memory openai openai /v1/chat/completions '{"model":"m"}'
check "plateaued"                    "true" "$(field plateaued)"
# time_to_plateau_s is WHEN THE RSS WENT FLAT, not when the steadiness test finished confirming it. The
# window is trailing, so a pass at `elapsed` means [elapsed-window, elapsed] was steady and the gateway
# stopped moving at the START of it. Reporting `elapsed` inflated every settling time by exactly one
# window (a gateway flat at 120s published as 180s) and, worse, tied a number ABOUT THE GATEWAY to the
# size of our instrument: change MEM_PLATEAU_WINDOW_S and every published settling time moves. This
# fixture rises for RISE_S then holds, so the reported time must be BELOW the declaration time, and 0 is
# a legitimate answer (already steady when the load began).
checkf "time_to_plateau_s is a non-negative number" \
  "$(awk -v v="$(field time_to_plateau_s)" 'BEGIN{print (v+0>=0)?1:0}')" "$(field time_to_plateau_s)"
checkf "time_to_plateau_s EXCLUDES the confirmation window (settling point, not declaration point)" \
  "$(awk -v v="$(field time_to_plateau_s)" -v w="$MEM_PLATEAU_WINDOW_S" 'BEGIN{print (v+0 <= 2*w)?1:0}')" \
  "$(field time_to_plateau_s) (window ${MEM_PLATEAU_WINDOW_S}s)"
checkf "steady_state_rss_mib is the settled value (120.0 by construction)" \
  "$(awk -v v="$(field steady_state_rss_mib)" 'BEGIN{print (v+0>=119 && v+0<=121)?1:0}')" "$(field steady_state_rss_mib)"
# THE LOAD-BEARING ONE: the rate is emitted even though this gateway DID plateau. A threshold admits any
# leak slower than itself, so "plateaued: true" without a rate hides exactly what the gate cannot catch.
checkf "growth_rate_mib_per_min emitted on the PLATEAUED path" \
  "$([ "$(field growth_rate_mib_per_min)" = null ] && echo 0 || echo 1)" "$(field growth_rate_mib_per_min)"
checkf "idle_rss_mib measured" "$([ "$(field idle_rss_mib)" = null ] && echo 0 || echo 1)" "$(field idle_rss_mib)"
checkf "peak_rss_mib measured" "$([ "$(field peak_rss_mib)" = null ] && echo 0 || echo 1)" "$(field peak_rss_mib)"
checkf "recovered_rss_mib measured" "$([ "$(field recovered_rss_mib)" = null ] && echo 0 || echo 1)" "$(field recovered_rss_mib)"
check "served" "true" "$(field served)"
checkf "rss_series is ONE continuous timeline across idle -> load -> recovery" "$(series_ok)" "$(field rss_series)"
# The idle window is sampled BEFORE any request, so it must sit below the loaded peak, never at it.
checkf "idle < peak (idle really was sampled cold)" \
  "$(awk -v i="$(field idle_rss_mib)" -v p="$(field peak_rss_mib)" 'BEGIN{print (i+0<p+0)?1:0}')" "$(field idle_rss_mib)/$(field peak_rss_mib)"

# ── CASE 2: a gateway that never settles ─────────────────────────────────────────────────────────
# THE CAP IS A RESULT, NOT AN ERROR. This is the case the whole design exists for: the number to publish
# is not a peak (which would just describe when we stopped) but the RATE, alongside plateaued:false.
echo "matrix_memory_test: a leaking gateway reaches the cap and publishes its leak rate"
CURVE=climb; FAKE_OK=1000; T_ORIGIN=$(date +%s)
matrix_cell_memory openai openai /v1/chat/completions '{"model":"m"}'
check "plateaued"                       "false" "$(field plateaued)"
check "steady_state_rss_mib withheld"   "null"  "$(field steady_state_rss_mib)"
check "time_to_plateau_s withheld"      "null"  "$(field time_to_plateau_s)"
# 5 MiB per synthetic second = 300 MiB/min. A wide band: the sampler's cadence is wall-clock.
checkf "growth_rate_mib_per_min is the observed leak rate (~300)" \
  "$(awk -v v="$(field growth_rate_mib_per_min)" 'BEGIN{print (v+0>200 && v+0<400)?1:0}')" "$(field growth_rate_mib_per_min)"
checkf "load ran to the cap, not to a false plateau" \
  "$(awk -v v="$(field load_s)" -v c="$MEM_PLATEAU_CAP_S" 'BEGIN{print (v+0>=c-2)?1:0}')" "$(field load_s)"
check "served" "true" "$(field served)"

# ── CASE 3: the load delivered nothing ───────────────────────────────────────────────────────────
# An unloaded process has a PERFECTLY steady RSS, so this is the case where a naive implementation
# publishes its most confident plateau. Everything the load phase would have said must be null, and
# plateaued must be null rather than false: false would claim we watched this gateway fail to settle.
echo "matrix_memory_test: a load that delivered nothing publishes nothing (never a flat-line plateau)"
CURVE=settle; RISE_S=0; FAKE_OK=0; T_ORIGIN=$(date +%s)
matrix_cell_memory openai openai /v1/chat/completions '{"model":"m"}'
check "served"                        "false" "$(field served)"
check "plateaued withheld (null, not false)" "null" "$(field plateaued)"
check "steady_state_rss_mib withheld" "null"  "$(field steady_state_rss_mib)"
check "growth_rate_mib_per_min withheld" "null" "$(field growth_rate_mib_per_min)"
check "peak_rss_mib withheld"         "null"  "$(field peak_rss_mib)"
check "peak_rss_hwm_mib withheld"     "null"  "$(field peak_rss_hwm_mib)"
# The cold idle read is still honest: the process booted and was sampled before any traffic. Only the
# LOAD-phase lanes are withheld, because only those depend on a load that never arrived.
checkf "idle_rss_mib still measured" "$([ "$(field idle_rss_mib)" = null ] && echo 0 || echo 1)" "$(field idle_rss_mib)"

# ── CASE 4: the rig cannot read RSS at all ───────────────────────────────────────────────────────
# A gateway with no gw_rss hook, a container whose /proc the harness cannot see, a non-Linux dev box:
# the load IS delivered but no sample ever lands. plateaued:false would claim we watched this gateway
# fail to settle; we watched nothing. The distinction is the same one the RSS lanes have always made
# between "not measured" and a measured zero, applied to the verdict itself.
echo "matrix_memory_test: an unmeasurable rig withholds the plateau VERDICT, it does not report false"
gw_rss(){ :; }; gw_hwm(){ :; }
FAKE_OK=1000; T_ORIGIN=$(date +%s)
matrix_cell_memory openai openai /v1/chat/completions '{"model":"m"}'
check "plateaued withheld (null, not false)" "null" "$(field plateaued)"
check "steady_state_rss_mib withheld" "null" "$(field steady_state_rss_mib)"
check "growth_rate_mib_per_min withheld" "null" "$(field growth_rate_mib_per_min)"
check "idle_rss_mib unmeasured (null, never 0)" "null" "$(field idle_rss_mib)"
check "peak_rss_mib unmeasured (null, never 0)" "null" "$(field peak_rss_mib)"

# ── CASE 5: no temp files survive a window ───────────────────────────────────────────────────────
# 36 cells means 36 windows per gateway; a window that leaks its scratch files leaks 36 of them.
echo "matrix_memory_test: every window cleans up after itself"
check "no mtxmem.$$.* scratch files left" "0" "$(ls "${TMPDIR:-/tmp}"/mtxmem.$$.* 2>/dev/null | wc -l | tr -d ' ')"

[ "$FAILED" = 0 ] && echo "matrix_memory_test: PASS" || echo "matrix_memory_test: FAIL"
exit "$FAILED"
