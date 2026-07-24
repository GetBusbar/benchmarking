#!/usr/bin/env bash
# Regression guard for the PEAK-SEARCH sustained sweep (lib/sweep.sh, run_sweep ... peak). Stubs the
# loadgen + rig with synthetic gateway curves and asserts the search finds each gateway's TRUE peak
# sustained rps. The load-bearing case is `peak_between`: the real maximum sits BETWEEN two doubling
# rungs, so any doubling-only method (the old ladder/bisect) understates it - peak search must not.
# Also guards the fd-collapse case (a hard cliff past the ceiling must yield the plateau, not zero).
# Requires bash 4+ (associative arrays). Run: bash lib/sweep_peak_test.sh
set -u
B="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MOCK=/bin/true; MOCKCORES=0; MOCK_PORT=9; GURL=http://gw; DURL=http://mock
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
  esac
}
fail=0
assert(){ # name  got_rps got_bound got_conc  rlo rhi  want_bound  clo chi
  local n="$1" r="$2" b="$3" cc="$4" rlo="$5" rhi="$6" wb="$7" clo="${8:-0}" chi="${9:-999999}"
  if [ "$r" -ge "$rlo" ] && [ "$r" -le "$rhi" ] && [ "$b" = "$wb" ] && [ "$cc" -ge "$clo" ] && [ "$cc" -le "$chi" ]; then
    echo "ok   - $n (rps=$r @ c=$cc bound=$b)";
  else echo "FAIL - $n: rps=$r @ c=$cc bound=$b (wanted rps $rlo..$rhi, c $clo..$chi, bound=$wb)"; fail=1; fi
}
run_case(){ CURVE="$1"; unset 'SWEEP_PRIOR[20]'; run_sweep 20 "32 65536" peak; }
run_case cliff1000;    assert "cliff/collapse -> plateau, not zero" "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 44000 45000 false
run_case slow200;      assert "slow gateway (ramp-down)"            "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC"  4500  5000 false
run_case mockbound;    assert "mock-bound flagged"                  "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 51000 57000 true
run_case peak_between; assert "peak BETWEEN doublings (not low)"    "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 39500 40000 false 1150 1450
run_case peak_low;     assert "peak BELOW start (ramp down)"        "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 44000 45000 false 32 128
[ "$fail" = 0 ] && echo "all peak-search sweep tests passed" || { echo "PEAK-SEARCH SWEEP TESTS FAILED"; exit 1; }
