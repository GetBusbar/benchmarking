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
# Capture the REAL stream_probe under an alias BEFORE the wholesale stub below replaces it, so the
# MEDIUM-2 body-threading test (bottom of file) can exercise the actual -body wiring.
eval "real_stream_probe() $(declare -f stream_probe | sed '1d')"
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
    # Direct-to-mock reference (ceiling / mock-ready liveness). The rig ceiling is CONCURRENCY-DEPENDENT
    # (Little's-law shape): far higher at a high concurrency than a low one — the exact property the
    # winner-concurrency re-probe (MEDIUM-4) exists to respect. The base slope is generous enough that
    # a gateway peaking well below the rig never spuriously reads mock-bound. The `mockbound_low` curve
    # below overrides this to sit NEAR the ceiling at a LOW winner conc, proving the re-probe (which
    # measures the ceiling at that low winner, not the inflated grid top) correctly flags it.
    if [ "${MOCK_DEAD:-0}" = 1 ]; then echo "0 0 0 0 0 0 0.0 0 0 0 0"; return; fi  # dead reference: fps 0
    local m=$(( 20000 + c*400 )); [ "$m" -gt 400000 ] && m=400000
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
    sust_mockbound)  # MEDIUM-R2-2 signal: gateway sustains to the grid top and its fps TRACKS the mock's
                     # own paced ceiling (m=20000+c*400, capped 400000) — i.e. rig-limited. _sm_finish_bound
                     # must set SM_MOCK_BOUND=true (winner fps within 10% of the re-probed reference). A
                     # rung-blind check against a low reference would miss it; this proves the sustained
                     # mock-bound decision fires, symmetric with the cpu-fps lane's mock-bound gate.
      local mm=$(( 20000 + c*400 )); [ "$mm" -gt 400000 ] && mm=400000
      echo "$c $c 0 0 $((c*SM_EXPFRAMES)) $mm 1.0 100 200 20 40" ;;
    c1_clean)   # stream_c1 GATEWAY path (the DURL baseline is the reference branch at the top: t99=200,
                # g99=40). Gateway frames cleanly, 0 errors, ttft_p99=520 gap_p99=60 -> SM_C1_OK=1 and
                # added = gw - direct = 320 (ttft) / 20 (gap), a positive honest subtraction.
      echo "$c $c 0 0 $((c*SM_EXPFRAMES)) $((c*30)) 1.0 300 520 30 60" ;;
    c1_dirty)   # stream_c1 honesty gate: the GATEWAY path errors on every stream and frames nothing ->
                # the window is unreliable, SM_C1_OK=0 and every added-latency field is ZEROED (never a
                # garbage negative from subtracting a 0 gw ttft off the direct baseline).
      echo "$c 0 $c $c 0 0 0.0 0 0 0 0" ;;
    fpspeak_768) # cpu-fps: fps unimodal, peak ~48000 at c=768 (BETWEEN doublings 512 and 1024).
                 # Declines both sides. All probed rungs pass the gate. A doubling-only method tops out
                 # below the true peak; the peak-search must land near 768.
      local d=$(( (c-768)/8 )); local f=$(( 48000 - $(sqr "$d") )); [ "$f" -lt 200 ] && f=200
      echo "$c $c 0 0 $((c*SM_EXPFRAMES)) $f 1.0 100 200 20 40" ;;
    fpspeak_low) # peak ~30000 at c=64, declining above. Exercises stop-at-first-non-rise.
      local d=$(( (c-64)/2 )); local f=$(( 30000 - $(sqr "$d") )); [ "$f" -lt 200 ] && f=200
      echo "$c $c 0 0 $((c*SM_EXPFRAMES)) $f 1.0 100 200 20 40" ;;
    fpspeak_mockbound_low) # peak ~44000 @ c=64. The rig ceiling at c=64 is 20000+64*400=45600, so the
      # gateway sits within 10% of the ceiling AT ITS WINNER concurrency -> genuinely mock-bound. But at
      # the grid top (c=8192) the ceiling caps at 400000, so comparing the peak against the GRID-TOP
      # ceiling (the pre-MEDIUM-4 bug) would read false. This curve passes iff the mock-bound decision
      # re-probes the ceiling at the winner concurrency (MEDIUM-4).
      local d=$(( (c-64)/2 )); local f=$(( 44000 - $(sqr "$d") )); [ "$f" -lt 200 ] && f=200
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

# ── MEDIUM-R2-2: the SUSTAINED lane's mock-bound decision (SM_MOCK_BOUND) is set and correct ─────────
# The sustained streams count is now gated on streams_sustained_mock_bound on every published surface
# (charts.py stream_sustained_valid / app.js sustainedCertified / check-consistency), so the shell
# decision that FEEDS that flag must be covered. Three cases, mirroring the cpu-fps lane:
#   (a) a fast gateway well below the rig ceiling -> mock_bound=false (a real gateway number).
CURVE=sust_1300; stream_sustained_bisect 8 4096
[ "$SM_MOCK_BOUND" = false ] && echo "ok   - sustained mock_bound=false when the gateway sits below the rig ceiling" || { echo "FAIL - sustained mock_bound=$SM_MOCK_BOUND (want false)"; fail=1; }
#   (b) a gateway whose fps TRACKS the mock's paced ceiling (rig-limited) -> mock_bound=true: the exact
#       class MEDIUM-R2-2 suppresses on the board so a mock bottleneck never draws a full sustained bar.
CURVE=sust_mockbound; stream_sustained_bisect 8 2048
[ "$SM_MOCK_BOUND" = true ] && echo "ok   - sustained mock_bound=true when the winner fps is within 10% of the rig ceiling (MEDIUM-R2-2)" || { echo "FAIL - sustained mock_bound=$SM_MOCK_BOUND (want true; a rig-limited count must be flagged)"; fail=1; }
#   (c) a DEAD reference (mock frames 0 fps) -> mock_bound=null, NEVER a trustworthy-looking false over
#       an unusable ceiling (the H6/R3-H1 analog on the sustained lane).
CURVE=sust_grid_top; MOCK_DEAD=1 stream_sustained_bisect 8 2048
[ "$SM_MOCK_BOUND" = null ] && echo "ok   - sustained mock_bound=null over a dead reference (never a false)" || { echo "FAIL - sustained mock_bound=$SM_MOCK_BOUND (want null over a 0-fps reference)"; fail=1; }

# ── stream_c1: concurrency-1 added-TTFT / added-gap + the honesty gate ───────────────────────────────
# The per-cell added-latency numbers every matrix stream cell publishes. Assert the clean subtraction
# (gw - direct, positive, no sign inversion) AND the gate that zeroes the fields on an unreliable window.
CURVE=c1_clean; stream_c1
[ "${SM_C1_OK:-x}" = 1 ] && echo "ok   - stream_c1 clean window: SM_C1_OK=1" || { echo "FAIL - stream_c1 clean window SM_C1_OK=${SM_C1_OK:-unset} (want 1)"; fail=1; }
assert_range "stream_c1 added TTFT p99 = gw520 - direct200 = 320 (no sign inversion)" "$SM_ADD_T99" 320 320
assert_range "stream_c1 added gap p99 = gw60 - direct40 = 20" "$SM_ADD_G99" 20 20
CURVE=c1_dirty; stream_c1
[ "${SM_C1_OK:-x}" = 0 ] && echo "ok   - stream_c1 dirty window trips the honesty gate: SM_C1_OK=0" || { echo "FAIL - stream_c1 dirty window SM_C1_OK=${SM_C1_OK:-unset} (want 0)"; fail=1; }
assert_range "stream_c1 dirty window zeroes added TTFT (never a garbage negative)" "$SM_ADD_T99" 0 0
assert_range "stream_c1 dirty window zeroes added gap" "$SM_ADD_G99" 0 0

# ── stream_mock_ready: the dead-mock path (the H6 analog) ────────────────────────────────────────────
# A LIVE mock frames a 1-stream probe (fps>0) -> ready (rc 0). A DEAD mock never frames -> the poll
# exhausts its tries and returns non-zero so the caller flags the reference unusable (rather than reading
# a 0 ceiling as a silently-passing mock-bound no-op).
CURVE=sust_grid_top; stream_mock_ready 3 && echo "ok   - stream_mock_ready returns 0 for a live framing mock" || { echo "FAIL - stream_mock_ready failed against a live mock"; fail=1; }
CURVE=sust_grid_top; if MOCK_DEAD=1 stream_mock_ready 2; then echo "FAIL - stream_mock_ready returned 0 for a DEAD mock (must be non-zero)"; fail=1; else echo "ok   - stream_mock_ready returns non-zero for a dead mock (reference flagged unusable)"; fi

# ── cpu-fps PEAK max-search ───────────────────────────────────────────────────────────────────────
CURVE=fpspeak_768; streamcpu_peak_fps 8 8192
assert_range "cpu-fps peak between doublings (~768)" "$SM_FPS_PEAK_CONC" 640 900
assert_range "cpu-fps peak value (~48000)" "$SM_FPS_PEAK" 46000 48000
CURVE=fpspeak_low; streamcpu_peak_fps 8 8192
assert_range "cpu-fps peak below start (~64)" "$SM_FPS_PEAK_CONC" 32 128
assert_range "cpu-fps peak value low (~30000)" "$SM_FPS_PEAK" 28000 30000
[ "$SM_FPS_MOCK_BOUND" = false ] && echo "ok   - cpu-fps mock_bound=false against a generous ceiling" || { echo "FAIL - cpu-fps mock_bound=$SM_FPS_MOCK_BOUND (want false)"; fail=1; }

# ── MEDIUM-4: mock-bound decision re-probes the ceiling at the WINNER concurrency, not the grid top ──
# A gateway peaking at a LOW concurrency where it is genuinely rig-limited must read mock_bound=true.
# The pre-fix code compared the low-conc peak against the grid-top (c=8192) ceiling — vastly higher —
# and never flagged it. The re-probe measures the ceiling at the winner (c~64) and flags it correctly.
CURVE=fpspeak_mockbound_low; streamcpu_peak_fps 8 8192
assert_range "cpu-fps mock-bound peak concentrated at low conc (~64)" "$SM_FPS_PEAK_CONC" 32 128
[ "$SM_FPS_MOCK_BOUND" = true ] && echo "ok   - cpu-fps mock_bound=true via winner-concurrency ceiling re-probe (MEDIUM-4)" || { echo "FAIL - cpu-fps mock_bound=$SM_FPS_MOCK_BOUND (want true; the grid-top ceiling would have hidden this)"; fail=1; }
[ "${SM_DIRECT_CEIL_CONC:-0}" = "$SM_FPS_PEAK_CONC" ] && echo "ok   - direct ceiling re-probed at the winner concurrency (SM_DIRECT_CEIL_CONC=$SM_DIRECT_CEIL_CONC)" || { echo "FAIL - direct ceiling not re-probed at the winner (SM_DIRECT_CEIL_CONC=${SM_DIRECT_CEIL_CONC:-unset}, winner=$SM_FPS_PEAK_CONC)"; fail=1; }

# ── MEDIUM-2: stream_probe threads the cell's ingress body through as ugen -body ────────────────────
# The streaming probes must POST the cell's real ingress-dialect body (so a gemini/cohere/bedrock/…
# ingress path is not 400'd by ugen's default openai body). stream_probe passes SM_STREAM_BODY (or its
# 4th positional arg) verbatim as `-body`, and omits -body entirely when no body is threaded (the
# standalone stream/streamcpu suites, whose -shape body construction must be unchanged). A fake ugen
# echoes its argv so we can assert on exactly what was passed. tmo runs its command (minus the budget
# arg); taskset drops its `-c <cores>` prefix and execs the rest.
( # subshell: local tmo/taskset overrides + a fake ugen, none of it leaks to the summary line
  tmo(){ shift; "$@"; }
  taskset(){ shift 2; "$@"; }
  ARGVF="$(mktemp)"
  # stream_probe pipes ugen through awk, so the fake ugen records its argv to a sidecar file (ARGVF)
  # rather than stdout; it still prints one valid stream record so the awk parser doesn't error.
  FAKEUGEN="$(mktemp)"
  printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" > "%s"\necho "1 1 0 0 64 100 1.0 100 200 20 40"\n' "$ARGVF" > "$FAKEUGEN"
  chmod +x "$FAKEUGEN"; UGEN="$FAKEUGEN"
  read_argv(){ cat "$ARGVF"; }
  # (a) with a threaded body -> argv carries `-body <that body>`
  BODY='{"contents":[{"parts":[{"text":"hi"}]}],"stream":true}'
  SM_STREAM_BODY="$BODY" real_stream_probe http://gw 4 1 >/dev/null 2>&1; argv="$(read_argv)"
  case "$argv" in
    *"-body $BODY"*) echo "ok   - stream_probe threads the ingress body as -body (MEDIUM-2)";;
    *) echo "FAIL - stream_probe did not pass -body (got: $argv)"; exit 1;;
  esac
  case "$argv" in *"-stream"*) : ;; *) echo "FAIL - stream_probe dropped -stream when a body was threaded (got: $argv)"; exit 1;; esac
  # (b) explicit 4th-positional body wins over SM_STREAM_BODY
  SM_STREAM_BODY='SHOULD_NOT_APPEAR' real_stream_probe http://gw 4 1 '{"pos":"arg"}' >/dev/null 2>&1; argv="$(read_argv)"
  case "$argv" in
    *'-body {"pos":"arg"}'*) echo "ok   - stream_probe 4th-positional body overrides SM_STREAM_BODY";;
    *) echo "FAIL - stream_probe positional body did not override SM_STREAM_BODY (got: $argv)"; exit 1;;
  esac
  # (c) no body threaded -> NO -body flag (standalone suites keep ugen's -shape body construction)
  SM_STREAM_BODY='' real_stream_probe http://gw 4 1 >/dev/null 2>&1; argv="$(read_argv)"
  case "$argv" in
    *"-body"*) echo "FAIL - stream_probe emitted -body with no threaded body (got: $argv)"; exit 1;;
    *) echo "ok   - stream_probe omits -body when none is threaded (standalone suites unchanged)";;
  esac
  rm -f "$FAKEUGEN" "$ARGVF"
) || fail=1

[ "$fail" = 0 ] && echo "all stream-measure tests passed" || { echo "STREAM-MEASURE TESTS FAILED"; exit 1; }
