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
  if [ "$url" = "$DURL" ]; then
    # The mock-ceiling reference probe (and _sw_mock_ready's 1-conn liveness probe) hit DURL. Two
    # curves need a NON-default DURL response to exercise the R3 fixes the base suite never touches:
    case "$CURVE" in
      # (a) R3-H1 dead-mock: the mock NEVER answers -> _sw_mock_ready's 1-conn probe returns rps=0 for
      # all 30 tries -> SW_MOCK_READY=0 -> SW_BOUND must be null (unknown), never false/true. A dead
      # mock that still reported a ceiling would ship an overstated mock_bound=false.
      deadmock) echo "0 0 0 0"; return ;;
      # (b) R3-M1 winner-above-reference re-probe: the mock ceiling is conc-dependent. At the reference
      # conc (<=2048) it reads LOW (30000); when _sw_ceil_ref_ok re-probes at the winner's higher conc
      # it must read HIGHER (up to 60000) and ADOPT the larger ceiling. A base fixed DURL would hide
      # the re-probe entirely (the winner never exceeds 2048 in the 8 base cases).
      peak_high) local m=$(( 50000 + c*20 )); [ "$m" -gt 130000 ] && m=130000; echo "$m 0 400000 300000"; return ;;
      *) echo "58000 0 500000 400000"; return ;;
    esac
  fi
  case "$CURVE" in
    deadmock)      local r=$(( c*50 )); [ "$r" -gt 40000 ] && r=40000; echo "$r 0 $(( 20000 + c*20 )) 15000" ;;
    peak_high)     # true peak ~55000 @ c=3000 (ABOVE the 2048 reference conc): forces _sw_ceil_ref_ok
                   # to re-probe the rig at the winner concurrency for a fair, higher ceiling (R3-M1).
                   local d=$(( (c-3000)/16 )); local r=$(( 55000 - $(sqr "$d") )); [ "$r" -lt 500 ] && r=500
                   echo "$r 0 $(( 20000 + c*8 )) 15000" ;;
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

# ── R3-H1: dead/never-ready mock -> SW_BOUND=null (the mock_bound=null-when-unready guard) ──────────
# The 8 cases above always answer DURL with rps>0, so _sw_mock_ready never fails and SW_BOUND=null is
# never asserted. Here the mock NEVER answers (DURL rps=0): _sw_mock_ready exhausts its tries,
# SW_MOCK_READY=0, and _sw_set_bound MUST emit null (unknown) - never false/true - so an overstated
# ceiling from a dead-mock reference is never published as a trustworthy mock_bound. A regression that
# shipped `false` instead of `null` on a dead mock would fail this case (audit R4 NIT).
run_case deadmock; assert "dead mock -> mock_bound=null (never false)" "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 39000 40000 null
[ "${SW_MOCK_READY:-x}" = 0 ] && echo "ok   - dead mock flagged SW_MOCK_READY=0" \
  || { echo "FAIL - dead mock did not clear SW_MOCK_READY (got '${SW_MOCK_READY:-unset}')"; fail=1; }

# ── R3-M1: winner concurrency ABOVE the reference -> _sw_ceil_ref_ok re-probes + adopts larger ceiling
# The base cases peak at c<=~1300, never above the 2048 reference conc, so _sw_ceil_ref_ok always
# early-exits and the re-probe branch is dead. Here the true peak sits at c~3000 (> 2048): the initial
# mock ceiling is measured LOW at the reference conc (~2048 -> ~30000), then re-probed at the winner's
# higher conc where the rig reports HIGHER (~60000). The guard MUST adopt the larger ceiling, and
# SW_MOCK_CEIL_CONC must move to the re-probe concurrency. A regression that inverted the comparison
# (kept the smaller reference) would fire mock_bound spuriously and truncate the fastest gateways.
run_case peak_high "32 65536"
assert "winner above reference conc (re-probe branch)" "$SW_CEIL_RPS" "$SW_BOUND" "$SW_CEIL_CONC" 54000 55000 false 2500 3500
[ "${SW_MOCK_CEIL:-0}" -gt 30000 ] && echo "ok   - re-probe adopted larger mock ceiling (SW_MOCK_CEIL=$SW_MOCK_CEIL > reference 30000)" \
  || { echo "FAIL - re-probe did NOT adopt the larger ceiling (SW_MOCK_CEIL=${SW_MOCK_CEIL:-unset}, wanted > 30000)"; fail=1; }
[ "${SW_MOCK_CEIL_CONC:-0}" -gt 2048 ] && echo "ok   - re-probe moved the ceiling conc above the reference (SW_MOCK_CEIL_CONC=$SW_MOCK_CEIL_CONC)" \
  || { echo "FAIL - re-probe ceiling conc not moved above 2048 (got '${SW_MOCK_CEIL_CONC:-unset}')"; fail=1; }

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

# ── R5-NIT: SW_JSON aggregation is populated + well-formed ─────────────────────────────────────────
# The base cases assert SW_CEIL_RPS/CONC/BOUND but NEVER SW_JSON — the per-rung sweep array charts.py
# and check-consistency read to prove the headline == max of the charted rungs. A regression that built
# an empty or malformed SW_JSON (e.g. an aggregation-loop off-by-one) would pass every case above and
# only surface at deploy via check-consistency. Assert it here: after a normal peak run SW_JSON must be
# non-empty and every rung must carry the four numeric keys the downstream reducer requires.
SWEEP_CACHE_KEY=""; SWEEP_ADAPTIVE=0
CURVE=peak_between; unset 'SWEEP_PRIOR[20]'; run_sweep 20 "32 65536" peak
if [ -n "${SW_JSON:-}" ]; then echo "ok   - SW_JSON aggregation populated (${#SW_JSON} chars)";
else echo "FAIL - SW_JSON empty after a normal peak run (aggregation produced no rungs)"; fail=1; fi
# Each rung object must carry conc/rps/p99_us/fail (the exact shape rungPasses + the peak reducer read).
_rungs=$(printf '%s' "${SW_JSON:-}" | grep -o '"conc":' | wc -l | tr -d ' ')
_shaped=$(printf '%s' "${SW_JSON:-}" | grep -o '{"conc":[0-9]*,"rps":[0-9]*,"p99_us":[0-9]*,"fail":[0-9]*}' | wc -l | tr -d ' ')
if [ "${_rungs:-0}" -ge 1 ] && [ "${_shaped:-0}" = "${_rungs:-0}" ]; then
  echo "ok   - SW_JSON rungs well-formed ($_shaped/$_rungs carry conc/rps/p99_us/fail)";
else echo "FAIL - SW_JSON malformed: $_shaped of $_rungs rungs carry the full conc/rps/p99_us/fail shape"; fail=1; fi

# ── R5-NIT: mock-ceiling CACHE-HIT branch (lib/sweep.sh:244-245) ────────────────────────────────────
# The cache-hit branch is never exercised above: with SWEEP_CACHE_KEY empty _mk is empty, so every run
# re-probes DURL for the ceiling (:247). A staleness/keying bug in the cache read would go uncaught. Here
# we prime the cache under a key, then re-run with a sweep_probe that would report a WILDLY DIFFERENT
# ceiling for DURL — if the cache HIT works, SW_MOCK_CEIL must be REUSED (unchanged), not re-measured.
SWEEP_CACHE_KEY="ck-test"; SWEEP_ADAPTIVE=0
CURVE=mockbound; unset 'SWEEP_PRIOR[20]'; run_sweep 20 "32 65536" peak    # cold: probes + caches the ceiling
_cached_ceil="${SW_MOCK_CEIL:-0}"
if [ -n "${SWEEP_MOCKCEIL_CACHE[ck-test|20]:-}" ] && [ "${_cached_ceil:-0}" -gt 0 ]; then
  echo "ok   - cold run cached the mock ceiling under the key (SW_MOCK_CEIL=$_cached_ceil)";
else echo "FAIL - cold run did NOT cache the mock ceiling (cache='${SWEEP_MOCKCEIL_CACHE[ck-test|20]:-unset}', SW_MOCK_CEIL=${_cached_ceil})"; fail=1; fi
# Warm run: swap in a DURL response an order of magnitude different. A cache HIT ignores it (reuses the
# primed value); a cache MISS/keying bug would re-probe and adopt the new number, which this catches.
sweep_probe(){ # url conc dur -> "rps fail p99us p50us"
  if [ "$1" = "$DURL" ]; then echo "999999 0 400000 300000"; return; fi        # would-be NEW (huge) ceiling
  local r=$(( $2*60 )); [ "$r" -gt 57000 ] && r=57000; echo "$r 0 $(( 20000 + $2*15 )) 14000"
}
CURVE=mockbound; unset 'SWEEP_PRIOR[20]'; run_sweep 20 "32 65536" peak
if [ "${SW_MOCK_CEIL:-0}" = "$_cached_ceil" ]; then
  echo "ok   - warm run REUSED the cached ceiling (SW_MOCK_CEIL=$SW_MOCK_CEIL, ignored the 999999 re-probe)";
else echo "FAIL - warm run did NOT reuse the cache (SW_MOCK_CEIL=${SW_MOCK_CEIL:-unset}, wanted the cached $_cached_ceil)"; fail=1; fi
SWEEP_CACHE_KEY=""

[ "$fail" = 0 ] && echo "all peak-search sweep tests passed" || { echo "PEAK-SEARCH SWEEP TESTS FAILED"; exit 1; }
