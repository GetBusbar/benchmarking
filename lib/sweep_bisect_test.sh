#!/usr/bin/env bash
# Regression guard for the BISECT sustained sweep (lib/sweep.sh, run_sweep ... bisect). Stubs the
# loadgen + rig with synthetic gateway curves and asserts the search finds each gateway's OWN true
# ceiling - in particular that a HARD COLLAPSE past the ceiling (the fd-cap / high-concurrency cliff
# that once zeroed busbar's sustained@20ms) yields the real plateau, NOT zero. Requires bash 4+
# (associative arrays). Run: bash lib/sweep_bisect_test.sh
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
sweep_probe(){ # url conc dur -> "rps fail p99us p50us"
  local url="$1" c="$2"
  if [ "$url" = "$DURL" ]; then echo "58000 0 500000 400000"; return; fi
  case "$CURVE" in
    cliff1000) if [ "$c" -le 1000 ]; then local r=$(( c*50 )); [ "$r" -gt 45000 ] && r=45000; echo "$r 0 $(( 20000 + c*20 )) 15000"; else echo "40 900000 90000 80000"; fi ;;
    slow200)   if [ "$c" -le 200 ];  then local r=$(( c*25 )); [ "$r" -gt 5000 ]  && r=5000;  echo "$r 0 $(( 20000 + c*30 )) 15000"; else echo "10 500000 95000 90000"; fi ;;
    mockbound) local r=$(( c*60 )); [ "$r" -gt 57000 ] && r=57000; echo "$r 0 $(( 20000 + c*15 )) 14000" ;;
  esac
}
fail=0
assert(){ # name  got_rps got_bound  lo hi  want_bound
  local n="$1" r="$2" b="$3" lo="$4" hi="$5" wb="$6"
  if [ "$r" -ge "$lo" ] && [ "$r" -le "$hi" ] && [ "$b" = "$wb" ]; then echo "ok   - $n (ceiling=$r bound=$b)";
  else echo "FAIL - $n: ceiling=$r bound=$b (wanted $lo..$hi bound=$wb)"; fail=1; fi
}
run_case(){ CURVE="$1"; unset 'SWEEP_PRIOR[20]'; run_sweep 20 "32 65536" bisect; }
run_case cliff1000; assert "cliff/collapse finds plateau, not zero" "$SW_CEIL_RPS" "$SW_BOUND" 44000 45000 false
run_case slow200;   assert "slow gateway (ramp-down)"               "$SW_CEIL_RPS" "$SW_BOUND"  4500  5000 false
run_case mockbound; assert "mock-bound flagged"                     "$SW_CEIL_RPS" "$SW_BOUND" 51000 57000 true
[ "$fail" = 0 ] && echo "all bisect sweep tests passed" || { echo "BISECT SWEEP TESTS FAILED"; exit 1; }
