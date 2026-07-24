#!/usr/bin/env bash
# Regression guard for lib/stream_measure.sh — the BISECT sustained-streams search and the PEAK
# cpu-fps max-search. Stubs stream_probe with synthetic gateway curves and asserts each search finds
# the TRUE answer, mirroring lib/sweep_peak_test.sh's method. The load-bearing cases:
#   * sustained bisect lands on the true integer ceiling BETWEEN grid rungs (a ladder would quantise);
#   * cpu-fps peak finds the throughput MAX (not fps at a concurrency threshold), including a peak
#     that sits BETWEEN doublings.
# Requires bash 4+ (associative arrays). Run: bash lib/stream_measure_test.sh
set -u
B="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MOCK=/bin/true; MOCKCORES=0; LOADCORES=0; MOCK_PORT=9; GURL=http://gw; DURL=http://mock
GW_MODEL=m; GW_AUTH=a; PSIZE=256; UGEN_H=(); GATEWAY=test
SM_EXPFRAMES=64; SM_STALL_US=40000; SM_SWEEP_DUR=10; SM_C1_DUR=10
SM_DELIV=0.999; SM_MOCKCEIL_CONC=2048
source "$B/lib/stream_measure.sh"
# Override externals AFTER sourcing so the stubs win.
pkill(){ :; }; setsid(){ :; }; taskset(){ :; }; sleep(){ :; }; env(){ :; }
suite_deadline_expired(){ return 1; }
log(){ :; }
probe_budget(){ echo 1; }
tmo(){ :; }   # not used: stream_probe is stubbed wholesale below
sqr(){ echo $(( $1 * $1 )); }

# stream_probe stub: url conc dur -> the 11 stream fields. CURVE selects the gateway's behaviour.
# Fields: streams complete fail stalled frames fps delivered ttft_p50 ttft_p99 gap_p50 gap_p99
stream_probe(){ # url conc dur
  local url="$1" c="$2"
  if [ "$url" = "$DURL" ]; then
    # Direct-to-mock reference (ceiling / mock-ready liveness). A generous rig ceiling that scales
    # with concurrency (Little's-law shape) so a gateway peak never spuriously reads mock-bound.
    local m=$(( 200 + c*400 )); [ "$m" -gt 400000 ] && m=400000
    echo "$c $c 0 0 $((c*SM_EXPFRAMES)) $m 1.0 100 200 20 40"; return
  fi
  case "$CURVE" in
    sust_1300)  # sustained ceiling = true concurrency 1300: gate passes (delivered 1.0, 0 stalls, 0
                # fail) at/below 1300, fails ABOVE. fps rises with conc up to the ceiling. A grid of
                # doublings would land the ladder on 1024; the bisect must find 1300.
      if [ "$c" -le 1300 ]; then echo "$c $c 0 0 $((c*SM_EXPFRAMES)) $((c*30)) 1.0 100 200 20 40"
      else echo "$c $((c/2)) $((c/3)) 5 $((c*SM_EXPFRAMES/2)) $((c*10)) 0.60 100 900000 20 900000"; fi ;;
    sust_grid_top)  # gate passes everywhere in the grid: the ceiling is at/above the top rung, so the
                    # search must report the grid top (not stop early on a mid rung).
      echo "$c $c 0 0 $((c*SM_EXPFRAMES)) $((c*25)) 1.0 100 200 20 40" ;;
    sust_none)  # even the low bound fails the gate (a gateway that stalls at any concurrency) -> 0.
      echo "$c 0 $c $c 0 0 0.0 100 900000 20 900000" ;;
    fpspeak_768) # cpu-fps: fps unimodal, peak ~48000 at c=768 (BETWEEN doublings 512 and 1024).
                 # Declines both sides. All probed rungs pass the gate. A doubling-only method tops out
                 # below the true peak; the peak-search must land near 768.
      local d=$(( (c-768)/8 )); local f=$(( 48000 - $(sqr "$d") )); [ "$f" -lt 200 ] && f=200
      echo "$c $c 0 0 $((c*SM_EXPFRAMES)) $f 1.0 100 200 20 40" ;;
    fpspeak_low) # peak ~30000 at c=64, declining above. Exercises stop-at-first-non-rise.
      local d=$(( (c-64)/2 )); local f=$(( 30000 - $(sqr "$d") )); [ "$f" -lt 200 ] && f=200
      echo "$c $c 0 0 $((c*SM_EXPFRAMES)) $f 1.0 100 200 20 40" ;;
  esac
}

fail=0
assert_range(){ # name got lo hi
  if [ "$2" -ge "$3" ] && [ "$2" -le "$4" ]; then echo "ok   - $1 (=$2)"; else echo "FAIL - $1: got $2 (want $3..$4)"; fail=1; fi
}

# ── sustained-streams BISECT ──────────────────────────────────────────────────────────────────────
CURVE=sust_1300;     stream_sustained_bisect 8 4096; assert_range "sustained bisect finds true ceiling 1300 between rungs" "$SM_SUST_STREAMS" 1280 1320
[ "$SM_SUST_FPS" -gt 0 ] && echo "ok   - sustained carries fps at the winning concurrency ($SM_SUST_FPS)" || { echo "FAIL - sustained fps not set"; fail=1; }
CURVE=sust_grid_top; stream_sustained_bisect 8 2048; assert_range "sustained ceiling at/above grid top -> reports top" "$SM_SUST_STREAMS" 2048 2048
CURVE=sust_none;     stream_sustained_bisect 8 2048; assert_range "no sustainable concurrency -> 0" "$SM_SUST_STREAMS" 0 0

# ── cpu-fps PEAK max-search ───────────────────────────────────────────────────────────────────────
CURVE=fpspeak_768; streamcpu_peak_fps 8 8192
assert_range "cpu-fps peak between doublings (~768)" "$SM_FPS_PEAK_CONC" 640 900
assert_range "cpu-fps peak value (~48000)" "$SM_FPS_PEAK" 46000 48000
CURVE=fpspeak_low; streamcpu_peak_fps 8 8192
assert_range "cpu-fps peak below start (~64)" "$SM_FPS_PEAK_CONC" 32 128
assert_range "cpu-fps peak value low (~30000)" "$SM_FPS_PEAK" 28000 30000
[ "$SM_FPS_MOCK_BOUND" = false ] && echo "ok   - cpu-fps mock_bound=false against a generous ceiling" || { echo "FAIL - cpu-fps mock_bound=$SM_FPS_MOCK_BOUND (want false)"; fail=1; }

[ "$fail" = 0 ] && echo "all stream-measure tests passed" || { echo "STREAM-MEASURE TESTS FAILED"; exit 1; }
