#!/usr/bin/env bash
# Regression guard for the PEAK-SEARCH sustained sweep (lib/sweep.sh, run_sweep ... peak). Stubs the
# loadgen + rig with synthetic gateway curves and asserts the search finds each gateway's TRUE peak
# sustained rps. The load-bearing case is `peak_between`: the real maximum sits BETWEEN two doubling
# rungs, so any doubling-only method (the old ladder/bisect) understates it - peak search must not.
# Also guards the fd-collapse case (a hard cliff past the ceiling must yield the plateau, not zero).
# Requires bash 4+ (associative arrays). Run: bash lib/sweep_peak_test.sh
set -u
B="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MOCK=/bin/true; MOCKCORES=0; LOADCORES=0; MOCK_PORT=9; GURL=http://gw; DURL=http://mock
SWEEP_DUR=10; P99_CEIL_MS=1000; GATEWAY=test
declare -A SWEEP_PRIOR=(); declare -A SWEEP_MOCKCEIL_CACHE=()
source "$B/lib/sweep.sh"
# Override externals AFTER sourcing so the stubs win.
pkill(){ :; }; setsid(){ :; }; taskset(){ :; }; sleep(){ :; }
suite_deadline_expired(){ return 1; }
log(){ :; }
sqr(){ echo $(( $1 * $1 )); }
sweep_probe(){ # url conc dur -> "rps fail p99us p50us"
  local url="$1" c="$2"
  if [ "$url" = "$DURL" ]; then echo "58000 0 500000 400000"; return; fi
  case "$CURVE" in
    cliff1000)     if [ "$c" -le 1000 ]; then local r=$(( c*50 )); [ "$r" -gt 45000 ] && r=45000; echo "$r 0 $(( 20000 + c*20 )) 15000"; else echo "40 900000 90000 80000"; fi ;;
    slow200)       if [ "$c" -le 200 ];  then local r=$(( c*25 )); [ "$r" -gt 5000 ]  && r=5000;  echo "$r 0 $(( 20000 + c*30 )) 15000"; else echo "10 500000 95000 90000"; fi ;;
    mockbound)     local r=$(( c*60 )); [ "$r" -gt 57000 ] && r=57000; echo "$r 0 $(( 20000 + c*15 )) 14000" ;;
    peak_between)  # true peak 40000 @ c=1300; declines both sides; ALL rungs pass the p99 gate, so a
                   # method that stops at a p99 threshold (bisect) never sees the peak. Doublings give
                   # at best 38810 (@c=1024); peak search must beat that and land near c=1300.
                   local d=$(( (c-1300)/8 )); local r=$(( 40000 - $(sqr "$d") )); [ "$r" -lt 500 ] && r=500
                   echo "$r 0 $(( 20000 + c*100 )) 15000" ;;
    peak_low)      # max-proxy shape: peak 45000 @ c=64, BELOW the default start (256). Exercises the
                   # bidirectional ramp walking DOWN to the peak. All rungs pass the gate.
                   local d=$(( (c-64)/2 )); local r=$(( 45000 - $(sqr "$d") )); [ "$r" -lt 500 ] && r=500
                   echo "$r 0 $(( 20000 + c*50 )) 14000" ;;
    peak_between_low) # peak 45000 @ c=96 — BETWEEN the low doublings 64 and 128. With a FIXED TOL=128
                   # the refine bracket [a=32,hi=128] (width 96) never opens, so c=96 is never probed
                   # and the search settles on c=64/128. The relative TOL (audit H5) must resolve it.
                   local d=$(( (c-96) )); local r=$(( 45000 - $(sqr "$d") )); [ "$r" -lt 500 ] && r=500
                   echo "$r 0 $(( 20000 + c*30 )) 14000" ;;
    cliff_peaklow) # true peak 40000 @ c=200, declining above; HARD p99 cliff for c>1024. Seeded with a
                   # stale prior ABOVE the cliff (2048), the search ramps DOWN to the first passing rung
                   # near the cliff. The ramp-down low bound must be _min (audit R2-H1) or the true peak
                   # at c=200 is below the clipped bracket and never probed (was ~31164 @ c=576).
                   if [ "$c" -le 1024 ]; then
                     local d=$(( (c-200)/6 )); local r=$(( 40000 - $(sqr "$d") )); [ "$r" -lt 500 ] && r=500
                     echo "$r 0 $(( 20000 + c*50 )) 15000"
                   else echo "40 900000 90000 80000"; fi ;;
  esac
}
fail=0
assert(){ # name  got_rps got_bound got_conc  rlo rhi  want_bound  clo chi
  local n="$1" r="$2" b="$3" cc="$4" rlo="$5" rhi="$6" wb="$7" clo="${8:-0}" chi="${9:-999999}"
  if [ "$r" -ge "$rlo" ] && [ "$r" -le "$rhi" ] && [ "$b" = "$wb" ] && [ "$cc" -ge "$clo" ] && [ "$cc" -le "$chi" ]; then
    echo "ok   - $n (rps=$r @ c=$cc bound=$b)";
  else echo "FAIL - $n: rps=$r @ c=$cc bound=$b (wanted rps $rlo..$rhi, c $clo..$chi, bound=$wb)"; fail=1; fi
}
run_case(){ CURVE="$1"; local bounds="${2:-32 65536}"; unset 'SWEEP_PRIOR[20]'; run_sweep 20 "$bounds" peak; }
run_case cliff1000;    assert "cliff/collapse -> plateau, not zero" "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 44000 45000 false
run_case slow200;      assert "slow gateway (ramp-down)"            "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC"  4500  5000 false
run_case mockbound;    assert "mock-bound flagged"                  "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 51000 57000 true
run_case peak_between; assert "peak BETWEEN doublings (not low)"    "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 39500 40000 false 1150 1450
run_case peak_low;     assert "peak BELOW start (ramp down)"        "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 44000 45000 false 32 128
run_case peak_between_low "16 8192"; assert "peak BETWEEN low doublings (relative TOL, H5)" "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 44550 45000 false 88 104

# ── M5: adaptive / SWEEP_PRIOR-seeded path (the matrix suite's production config) ──────────────────
# matrix runs run_sweep ... peak with SWEEP_ADAPTIVE=1, seeding SWEEP_PRIOR[ttft] from the previous
# cell's winner. A stale prior from a latency-shifted previous cell must NOT bias the search onto a
# local rung: cell 1 (peak_low, peak @ c=64) seeds SWEEP_PRIOR[20]=64; cell 2 uses a DIFFERENT curve
# whose true peak is at c=1300 with the SAME ttft, so the prior seed (64) is far below the real peak.
# The prior-seeded ramp must still climb to the true peak. Covers lib/sweep.sh:206 (read prior as the
# search start) and :267 (write-back) - the single most-run production configuration.
SWEEP_ADAPTIVE=1
CURVE=peak_low;     unset 'SWEEP_PRIOR[20]'; run_sweep 20 "16 8192" peak
assert "adaptive cell 1 seeds the prior (peak_low)" "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 44000 45000 false 32 128
[ "${SWEEP_PRIOR[20]:-unset}" = 64 ] && echo "ok   - adaptive prior seeded to c=64" \
  || { echo "FAIL - adaptive prior not seeded (got '${SWEEP_PRIOR[20]:-unset}', wanted 64)"; fail=1; }
CURVE=peak_between; run_sweep 20 "32 65536" peak     # KEEP the stale prior=64 from cell 1
assert "adaptive cell 2 (stale prior=64) still finds true peak" "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 39500 40000 false 1150 1450
# cell 3: prior seeded ABOVE the p99 cliff (2048) on a curve whose true peak is LOW (c=200). The
# ramp-down must bracket the whole passing region below the cliff, not stop at ~c/2 near the cliff
# (audit R2-H1: was 31164 @ c=576, a 22% understatement of 40000 @ c=200).
SWEEP_PRIOR[20]=2048
CURVE=cliff_peaklow; run_sweep 20 "32 65536" peak
assert "adaptive prior ABOVE cliff still finds low true peak (R2-H1)" "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 39500 40000 false 150 260
SWEEP_ADAPTIVE=0

# ── M6: sweep_c1() added-latency + C1_OK/C1_ERR honesty gate ───────────────────────────────────────
# sweep_c1 measures the concurrency-1 added latency (gateway p99 - direct p99) and gates the window's
# honesty (C1_OK=0 when the gateway errored the window or produced no successful sample). Covered:
# OVER_P99 subtraction (lib/sweep.sh:114) and the error-rate gate (:103-107). Stub direct + gateway
# probes with a clean pair, then a dirty gateway window, asserting OVER_P99 and C1_OK both ways.
C1_DUR=10; WARMUP_DUR=1; SWEEP_CACHE_KEY=""
_c1mode=clean
sweep_probe(){ # url conc dur -> "rps fail p99us p50us"
  if [ "$1" = "$DURL" ]; then echo "5000 0 300000 200000"; return; fi   # direct baseline: p99=300000us
  case "$_c1mode" in
    clean) echo "4800 0 450000 320000" ;;                               # gateway clean: p99=450000us
    dirty) echo "10 90000 470000 330000" ;;                             # gateway mostly-errored window
  esac
}
_c1mode=clean; sweep_c1
# added p99 = 450000 - 300000 = 150000us; window clean -> C1_OK=1
if [ "$OVER_P99" = 150000 ] && [ "$C1_OK" = 1 ]; then echo "ok   - sweep_c1 clean (added p99=${OVER_P99}us, C1_OK=$C1_OK)";
else echo "FAIL - sweep_c1 clean: OVER_P99=$OVER_P99 (want 150000) C1_OK=$C1_OK (want 1)"; fail=1; fi
_c1mode=dirty; sweep_c1
# gateway window is ~90000 fails over ~10*10 successes -> error rate >> 0.1% -> C1_OK=0 with a reason
if [ "$C1_OK" = 0 ] && [ -n "$C1_ERR" ]; then echo "ok   - sweep_c1 dirty gate (C1_OK=$C1_OK, reason set)";
else echo "FAIL - sweep_c1 dirty: C1_OK=$C1_OK (want 0), C1_ERR='$C1_ERR'"; fail=1; fi

[ "$fail" = 0 ] && echo "all peak-search sweep tests passed" || { echo "PEAK-SEARCH SWEEP TESTS FAILED"; exit 1; }
