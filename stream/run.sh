#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# STREAMING — pluggable across gateways (same gateways/<name>/gateway.sh manifests as perf/memory).
# The mock answers stream:true with a paced SSE stream (role chunk, then N content deltas every
# INTERVAL ms, then finish + [DONE]); the suite measures what the GATEWAY adds on top of that pace:
#   * added TTFT (µs) at concurrency 1 = gateway first-content-frame time − direct-to-mock TTFT
#   * added inter-frame latency (µs)   = gateway content-frame gap − direct-to-mock gap (p50/p99)
#   * streams sustained = max concurrent streams where >=99% of expected content frames deliver and
#     the stream error rate stays under 0.1%. Pacing is NOT part of this gate: a gateway that
#     re-chunks/coalesces the upstream SSE (Kong's documented parse-and-reframe pipeline, for
#     example) still DELIVERS everything, and streamcpu/run.sh already treats coalescing as
#     legitimate relay behavior - so a ~100%-delivery run must never publish a zero here. Pacing
#     fidelity is published separately: stream_stallfree_streams is the max concurrency with the
#     old strict gate (>=99.9% delivery, ZERO streams with any inter-frame gap > STALL_X x the
#     interval), and the added-gap percentiles price the coalescing directly.
#   * frames/sec at that concurrency
# and writes results/stream/<gateway>.json. A gateway that answers 200 but never frames (buffers
# the whole response) is recorded stream_served=false — measured, not hidden.
#
#   GATEWAY=busbar stream/run.sh
#
# Knobs (env): STREAM_CHUNKS (content deltas per stream, default 64), STREAM_INTERVAL_MS (pace,
#   default 20), STREAM_CHUNK_BYTES (delta payload, default 16), STALL_X (stall = gap > X×interval,
#   default 2), C1_DUR (c1 run seconds, default 30), SWEEP ("1 8 32 128 512 1024"), SWEEP_DUR
#   (seconds per sweep point, default 15), PSIZE (request payload bytes, default 256), CORES pin.
# Manifest hook (optional, generic): GW_STREAM_NOTE - a cited, human-readable note the manifest
#   attaches to this gateway's stream result (e.g. a link to the project's own open issue when a
#   stream failure is a known upstream bug); emitted verbatim as "stream_note" in the JSON.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
GATEWAY="${GATEWAY:-busbar}"
export GW_DIR="$ROOT/gateways/$GATEWAY"
[ -f "$GW_DIR/gateway.sh" ] || { echo "unknown gateway '$GATEWAY'"; exit 2; }

STREAM_CHUNKS="${STREAM_CHUNKS:-64}"
STREAM_INTERVAL_MS="${STREAM_INTERVAL_MS:-20}"
STREAM_CHUNK_BYTES="${STREAM_CHUNK_BYTES:-16}"
STALL_X="${STALL_X:-2}"
STALL_US=$(( STREAM_INTERVAL_MS * STALL_X * 1000 ))
C1_DUR="${C1_DUR:-30}"; SWEEP_DUR="${SWEEP_DUR:-15}"; PSIZE="${PSIZE:-256}"
# Concurrency = simultaneously open SSE streams. Spans low→high so every gateway — fast or slow —
# is offered a load it can hold; the qualifying maximum is the sustained figure. Same grid for all.
SWEEP="${SWEEP:-1 8 32 128 256 512 1024 2048}"
# raise the fd limit so high-concurrency sweeps aren't capped by open sockets
ulimit -n 1048576 2>/dev/null || ulimit -n 65536 2>/dev/null || true
export CORES="${CORES:-0-3}"; LOADCORES="${LOADCORES:-0-3}"; MOCKCORES="${MOCKCORES:-0-3}"
export MOCK_PORT="${MOCK_PORT:-8000}"
RESULTS="$ROOT/results/stream"; mkdir -p "$RESULTS"
log(){ echo "[$(date +%H:%M:%S)] $*"; }
command -v taskset >/dev/null || taskset(){ shift 2; "$@"; }

log "fetching prebuilt rig (mock + loadgen) — no on-box toolchain needed"
. "$ROOT/lib/rig.sh"; fetch_rig "$ROOT" || { echo "rig fetch failed"; exit 1; }

[ -f "$ROOT/gateways/versions.env" ] && source "$ROOT/gateways/versions.env"
gw_version(){ echo unknown; }; GW_HEADERS=()
gw_diag(){ :; }
json_escape(){ printf '%s' "$1" | python3 -c 'import json,sys
d=sys.stdin.buffer.read()[:1600].decode("utf-8","replace")
sys.stdout.write(json.dumps(d)[1:-1])'; }
# shellcheck source=/dev/null
source "$ROOT/lib/harness.sh"
# shellcheck source=/dev/null
source "$GW_DIR/gateway.sh"
suite_deadline_start

log "starting mock :$MOCK_PORT (stream ${STREAM_CHUNKS}x${STREAM_CHUNK_BYTES}B @ ${STREAM_INTERVAL_MS}ms)"
[ -n "$MOCK" ] && pkill -f "$MOCK" 2>/dev/null; sleep 1
setsid taskset -c "$MOCKCORES" env \
  MOCK_STREAM_CHUNKS="$STREAM_CHUNKS" MOCK_STREAM_INTERVAL_MS="$STREAM_INTERVAL_MS" \
  MOCK_STREAM_CHUNK_BYTES="$STREAM_CHUNK_BYTES" \
  "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
sleep 1
cleanup(){ gw_stop 2>/dev/null; [ -n "$MOCK" ] && pkill -f "$MOCK" 2>/dev/null; }
trap cleanup EXIT

# Mock readiness gate (audit R3-H1 / R5-#3, ported from lib/sweep.sh:_sw_mock_ready). After a blind
# `sleep 1` the mock may not yet be bound (or, after a high-concurrency phase, may still be losing the
# bind race to the dying one): the ceiling probe then reads 0 fps, the mock-bound guard silently no-ops,
# and an overstated ceiling ships as a clean mock_bound=false. Poll a cheap 1-stream probe until it
# frames (fps>0) before trusting the ceiling reference. MOCK_READY=0 => the reference is unusable and
# stream_mock_bound is published as JSON null ("unknown"), never a trustworthy-looking false.
_stream_mock_ready(){ # tries
  local i="${1:-30}" _s _c _f _st _fr fps _rest
  while [ "$i" -gt 0 ]; do
    read -r _s _c _f _st _fr fps _rest < <(sprobe "$DURL" 1 1)
    [ "${fps:-0}" -gt 0 ] 2>/dev/null && return 0
    i=$((i-1)); sleep 1
  done
  return 1
}

# run ugen in SSE mode, echo the k=v result fields in a fixed order. HARD TIMEOUT (tmo): an
# unresponsive gateway that leaves streams open must not block on ugen's 120s tail client timeout
# across every hung worker; cap the invocation at dur+grace and treat empty output as not-served.
sprobe(){ # url conc dur
  tmo "$(probe_budget "$3")" taskset -c "$LOADCORES" "$UGEN" -url "$1" -model "$GW_MODEL" -auth "$GW_AUTH" -c "$2" -d "$3" \
    -psize "$PSIZE" -stream -expframes "$STREAM_CHUNKS" -stallus "$STALL_US" "${UGEN_H[@]}" 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++){split($i,a,"=");v[a[1]]=a[2]};
        print v["streams"],v["complete"],v["fail"],v["stalled"],v["frames"],v["fps"],v["delivered"],
              v["ttft_p50us"],v["ttft_p99us"],v["gap_p50us"],v["gap_p99us"]}'
}

log "[$GATEWAY] build"; gw_build || { echo "build failed"; exit 1; }
UGEN_H=(); CURL_H=(); ok=0; c=000
_stream_ready(){
  UGEN_H=(); CURL_H=()
  for h in "${GW_HEADERS[@]:-}"; do [ -n "$h" ] && { UGEN_H+=(-H "$h"); CURL_H+=(-H "$h"); }; done
  local i
  for i in $(seq 1 60); do
    c=$(curl -s -m3 -o /dev/null -w "%{http_code}" "http://127.0.0.1:$GW_PORT$GW_PATH" -X POST \
        -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" "${CURL_H[@]}" \
        -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}")
    [ "$c" = 200 ] && return 0; sleep 1
  done
  return 1
}
log "[$GATEWAY] launch + wait 200 on $GW_PATH (robust boot, up to $HARNESS_BOOT_ATTEMPTS attempts)"
SERVE_ERR=""
if harness_launch_ready gw_launch _stream_ready; then ok=1
else SERVE_ERR="$HARNESS_SERVE_ERR"; fi

# ── does the gateway actually stream? one probe, then fail gracefully if not ──────────────────────
# A gateway may 200 the non-stream request yet buffer or reject stream:true. Probe with curl -N and
# require SSE frames in the body; a non-streaming gateway gets stream_served=false, not a crash.
STREAM_OK=0; STREAM_ERR=""
if [ "$ok" = 1 ]; then
  probe_to=$(( STREAM_CHUNKS * STREAM_INTERVAL_MS / 1000 + 10 ))
  sbody="$(curl -sN -m "$probe_to" "http://127.0.0.1:$GW_PORT$GW_PATH" -X POST \
      -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" \
      -H "accept: text/event-stream" "${CURL_H[@]}" \
      -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16,\"stream\":true}" 2>&1)"
  # here-string, not a pipe: with `set -o pipefail`, `printf ... | grep -q` reports the pipeline as
  # failed when grep matches early and printf takes SIGPIPE (141) -- a false negative even though the
  # body is full of frames. grep on a here-string reads the whole string and returns grep's own status
  # (same fix as streamcpu/run.sh).
  if grep -q '^data:' <<< "$sbody"; then STREAM_OK=1
  else STREAM_ERR="no SSE frames on stream:true; body=[$(printf '%s' "$sbody" | head -c 400)]; diag=[$(gw_diag 2>&1 | tail -n 20)]"
       log "[$GATEWAY] WARNING stream:true produced no SSE frames - stream_served=false"
  fi
fi

DURL="http://127.0.0.1:$MOCK_PORT$GW_PATH"; GURL="http://127.0.0.1:$GW_PORT$GW_PATH"
DT50=0; DT99=0; DG50=0; DG99=0; GT50=0; GT99=0; GG50=0; GG99=0
ADD_T50=0; ADD_T99=0; ADD_G50=0; ADD_G99=0
SUST_STREAMS=0; SUST_FPS=0; STALLFREE_STREAMS=0; STALLFREE_FPS=0; MOCK_FPS=0; MOCK_FPS_CONC=0; MOCK_READY=1; MOCK_BOUND=false; SWEEP_JSON=""
if [ "$STREAM_OK" = 1 ]; then
  # ── c1: direct baseline + gateway → added TTFT / added inter-frame gap (µs) ─────────────────────
  # Same discarded warm-up for both paths, mirroring perf/run.sh.
  WARMUP_DUR="${WARMUP_DUR:-5}"
  log "[$GATEWAY] warm-up ${WARMUP_DUR}s (discarded, both paths)"
  sprobe "$DURL" 1 "$WARMUP_DUR" >/dev/null 2>&1; sprobe "$GURL" 1 "$WARMUP_DUR" >/dev/null 2>&1
  log "[$GATEWAY] c1 stream baseline (direct→mock) ${C1_DUR}s"
  read -r _ds _dc _df _dst _dfr _dfp _dd DT50 DT99 DG50 DG99 < <(sprobe "$DURL" 1 "$C1_DUR")
  log "[$GATEWAY] c1 stream gateway ${C1_DUR}s"
  read -r _gs _gc _gf _gst _gfr _gfp _gd GT50 GT99 GG50 GG99 < <(sprobe "$GURL" 1 "$C1_DUR")
  # Gate the streaming-latency lane on c1 honesty. ugen counts a 200-that-never-framed as fail and only
  # timestamps content frames, so TTFT/gap percentiles already reflect only real streams; but a window
  # that mostly errored (buffered/429/5xx) must not publish an error-path added-TTFT as a win. Require
  # real frames on both paths and a stream error rate under 0.1%, else demote stream_served=false.
  if [ "${_gfr:-0}" -le 0 ] || [ "${_dfr:-0}" -le 0 ] || [ "${GT99:-0}" -le 0 ] || [ "${DT99:-0}" -le 0 ] \
     || ! awk -v f="${_gf:-1}" -v s="${_gs:-0}" 'BEGIN{exit !(s>0 && f<=0.001*s)}' \
     || ! awk -v f="${_df:-1}" -v s="${_ds:-0}" 'BEGIN{exit !(s>0 && f<=0.001*s)}'; then
    STREAM_OK=0
    STREAM_ERR="${STREAM_ERR:+$STREAM_ERR; }c1 stream window unreliable: gw streams=${_gs:-0} fail=${_gf:-?} frames=${_gfr:-0} ttft_p99=${GT99:-0}us; direct streams=${_ds:-0} fail=${_df:-?} frames=${_dfr:-0}"
    GT50=0; GT99=0; DT50=0; DT99=0; GG50=0; GG99=0; DG50=0; DG99=0
    log "[$GATEWAY] WARNING stream c1 window had errors / no frames - stream_served=false"
  fi
  ADD_T50=$(( ${GT50:-0} - ${DT50:-0} )); ADD_T99=$(( ${GT99:-0} - ${DT99:-0} ))
  ADD_G50=$(( ${GG50:-0} - ${DG50:-0} )); ADD_G99=$(( ${GG99:-0} - ${DG99:-0} ))
  log "[$GATEWAY] c1: added TTFT p99=${ADD_T99}µs (p50=${ADD_T50}µs)  added gap p99=${ADD_G99}µs (p50=${ADD_G50}µs)"
fi
if [ "$STREAM_OK" = 1 ]; then

  # ── streams-sustained sweep ─────────────────────────────────────────────────────────────────────
  # HEADLINE gate (delivery): a point qualifies when >=99% of expected content frames were delivered
  # and the stream error rate is under 0.1%. Stall counts are RECORDED per point (and priced by the
  # added-gap percentiles) but do NOT zero the headline: a gateway that re-chunks/coalesces its SSE
  # relay still delivered the content, and streamcpu/run.sh already treats coalescing as legitimate.
  # The former absolute gate (>=99.9% delivered AND zero stalled streams) zeroed gateways that
  # delivered ~100% of frames (one stalled stream of thousands disqualified the point); it is kept
  # as the SECONDARY stall-free figure below, never as the headline.
  top=1; for w in $SWEEP; do top=$w; done
  # Mock readiness gate (R5-#3): do not trust the ceiling reference until the fresh mock actually frames.
  # If it never comes up the reference is unusable and stream_mock_bound must publish JSON null, not false.
  MOCK_READY=1
  if ! _stream_mock_ready 30; then
    MOCK_READY=0
    log "[$GATEWAY] WARNING stream mock did not become ready: the mock-ceiling reference is unreliable; stream_mock_bound cannot be decided this run (null)"
  fi
  # Mock-ceiling reference. Cap the reference concurrency the same way perf's peak lane does
  # (lib/sweep.sh SWEEP_MOCKCEIL_CONC:-2048): at very high concurrency the loadgen+mock are both
  # overloaded and the reference comes out artificially LOW, firing the guard against a degraded ceiling.
  mock_conc=$top; [ "$mock_conc" -gt "${SWEEP_MOCKCEIL_CONC:-2048}" ] && mock_conc="${SWEEP_MOCKCEIL_CONC:-2048}"
  MOCK_FPS_CONC=$mock_conc
  read -r _s _c _f _st _fr MOCK_FPS _d _t1 _t2 _g1 _g2 < <(sprobe "$DURL" "$mock_conc" "$SWEEP_DUR")
  MOCK_FPS=${MOCK_FPS:-0}
  log "[$GATEWAY] sweep — streams sustained (mock ceiling ${MOCK_FPS} fps @ c=$mock_conc)"
  for conc in $SWEEP; do
    if suite_deadline_expired; then log "[$GATEWAY] suite wall-clock ceiling reached mid-sweep - stopping sweep, recording what we have"; break; fi
    read -r streams complete fail stalled frames fps delivered t50 t99 g50 g99 < <(sprobe "$GURL" "$conc" "$SWEEP_DUR")
    streams=${streams:-0}; fail=${fail:-1}; stalled=${stalled:-1}; fps=${fps:-0}; delivered=${delivered:-0}
    log "[$GATEWAY]   c=$conc → streams=$streams fps=$fps delivered=$delivered stalled=$stalled fail=$fail gap_p99=$((${g99:-0}/1000))ms"
    SWEEP_JSON="${SWEEP_JSON}${SWEEP_JSON:+,}{\"conc\":$conc,\"streams\":$streams,\"complete\":${complete:-0},\"fail\":$fail,\"stalled\":$stalled,\"fps\":$fps,\"delivered\":$delivered,\"ttft_p99_us\":${t99:-0},\"gap_p99_us\":${g99:-0}}"
    if awk -v f="$fail" -v s="$streams" -v d="$delivered" \
         'BEGIN{exit !(s>0 && f<=0.001*s && d>=0.99)}' \
       && [ "$conc" -gt "$SUST_STREAMS" ]; then
      SUST_STREAMS=$conc; SUST_FPS=$fps
    fi
    if awk -v f="$fail" -v s="$streams" -v d="$delivered" -v st="$stalled" \
         'BEGIN{exit !(s>0 && f<=0.001*s && d>=0.999 && st==0)}' \
       && [ "$conc" -gt "$STALLFREE_STREAMS" ]; then
      STALLFREE_STREAMS=$conc; STALLFREE_FPS=$fps
    fi
  done
  # Fair-ceiling re-probe (audit R5-#1, ported from lib/sweep.sh:_sw_ceil_ref_ok). The headline pairs
  # the stall-free stream count (STALLFREE_STREAMS) with the fps measured AT that same operating point
  # (STALLFREE_FPS), typically a LOWER concurrency than the ladder top. But MOCK_FPS was probed at the
  # capped reference concurrency (mock_conc). Under 20ms SSE pacing fps is Little's-law-bound and scales
  # ~linearly with concurrency, so a reference measured at a DIFFERENT concurrency than the winner is
  # not a fair ceiling: at a HIGHER reference conc it over-measures (winner never within 10% -> a
  # genuinely rig-limited stream ships mock_bound=false, audit R2-H2 residue); at a LOWER one it
  # under-measures. Re-probe the rig ONCE at the winner's own concurrency (capped at 4x the reference,
  # never the multi-thousand rail the cap exists to avoid) and adopt the LARGER fps as the fair ceiling
  # before deciding bound - exactly as _sw_ceil_ref_ok does for the perf lane.
  if [ "$MOCK_READY" = 1 ] && [ "${STALLFREE_STREAMS:-0}" -gt 0 ] && [ "${STALLFREE_STREAMS}" -ne "${MOCK_FPS_CONC:-0}" ]; then
    reprobe=$STALLFREE_STREAMS
    capc=$(( MOCK_FPS_CONC>0 ? MOCK_FPS_CONC*4 : STALLFREE_STREAMS )); [ "$reprobe" -gt "$capc" ] && reprobe=$capc
    read -r _s _c _f _st _fr _rm _d _t1 _t2 _g1 _g2 < <(sprobe "$DURL" "$reprobe" "$SWEEP_DUR"); _rm=${_rm:-0}
    if [ "$_rm" -gt "${MOCK_FPS:-0}" ]; then
      log "[$GATEWAY] mock-ceiling re-probed at winner c=$reprobe: ${MOCK_FPS} -> $_rm fps (winner c=$STALLFREE_STREAMS vs reference c=$MOCK_FPS_CONC)"
      MOCK_FPS=$_rm; MOCK_FPS_CONC=$reprobe
    fi
  fi
  # Set the mock-bound flag from the winner's fps vs the (now fair) ceiling. Emit JSON null ("unknown")
  # - NOT a clean false - when the reference is unusable (mock never became ready, or the ceiling probe
  # read 0), so an overstated ceiling is never silently published as a trustworthy mock_bound=false
  # (audit R5-#3). This mirrors lib/sweep.sh:_sw_set_bound and streamcpu's streamcpu_valid honesty.
  if [ "$MOCK_READY" != 1 ] || [ "${MOCK_FPS:-0}" -le 0 ]; then
    MOCK_BOUND=null
  else
    MOCK_BOUND=false
    awk -v c="$STALLFREE_FPS" -v m="$MOCK_FPS" 'BEGIN{exit !(c>=0.9*m)}' && MOCK_BOUND=true
  fi
  [ "$MOCK_BOUND" = true ] && log "[$GATEWAY] ⚠ sustained fps ($STALLFREE_FPS) within 10% of mock ($MOCK_FPS @ c=$MOCK_FPS_CONC) — MOCK-BOUND floor"
  [ "$MOCK_BOUND" = null ] && log "[$GATEWAY] stream_mock_bound=null: mock-ceiling reference unusable (ready=$MOCK_READY, ceiling=${MOCK_FPS} fps)"
  log "[$GATEWAY] streams sustained (clean, no stall/fail) = $STALLFREE_STREAMS; delivered (stalls ignored) = $SUST_STREAMS"
fi

BUILD="$(gw_version 2>/dev/null | tr -d '\n' | sed 's/"/\\"/g')"
cat > "$RESULTS/$GATEWAY.json" <<JSON
{
  "gateway": "$GATEWAY",
  "build": "$BUILD",
  "served": $([ "$ok" = 1 ] && echo true || echo false),
  "stream_served": $([ "$STREAM_OK" = 1 ] && echo true || echo false),
  "last_http_status": "$c",
  "serve_error": "$(json_escape "$SERVE_ERR")",
  "stream_error": "$(json_escape "$STREAM_ERR")",
  "stream_note": "$(json_escape "${GW_STREAM_NOTE:-}")",
  "stream_added_ttft_p50_us": $ADD_T50,
  "stream_added_ttft_p99_us": $ADD_T99,
  "stream_gateway_ttft_p50_us": ${GT50:-0},
  "stream_gateway_ttft_p99_us": ${GT99:-0},
  "stream_direct_ttft_p50_us": ${DT50:-0},
  "stream_direct_ttft_p99_us": ${DT99:-0},
  "stream_added_gap_p50_us": $ADD_G50,
  "stream_added_gap_p99_us": $ADD_G99,
  "stream_gateway_gap_p50_us": ${GG50:-0},
  "stream_gateway_gap_p99_us": ${GG99:-0},
  "stream_direct_gap_p50_us": ${DG50:-0},
  "stream_direct_gap_p99_us": ${DG99:-0},
  "stream_sustained_streams": $STALLFREE_STREAMS,
  "stream_sustained_fps": $STALLFREE_FPS,
  "stream_sustained_gate": "clean streaming: the highest concurrency where EVERY stream stayed clean - zero errored/dropped streams AND zero streams with any inter-frame gap > ${STALL_X}x the ${STREAM_INTERVAL_MS}ms pacing interval (a stall), >=99.9% frames delivered. This is the honest 'streams held without stalling' number the column shows.",
  "stream_delivered_streams": $SUST_STREAMS,
  "stream_delivered_gate": "looser: >=99% of frames delivered and <0.1% errored streams, IGNORING stalls (a stream that delivered every frame but late still counts). Kept for transparency; stalls are priced by the added-gap p99.",
  "stream_mock_ceiling_fps": ${MOCK_FPS:-0},
  "stream_mock_bound": $MOCK_BOUND,
  "stream_chunks": $STREAM_CHUNKS,
  "stream_interval_ms": $STREAM_INTERVAL_MS,
  "stream_chunk_bytes": $STREAM_CHUNK_BYTES,
  "stream_stall_x": $STALL_X,
  "sweep_streams": [$SWEEP_JSON],
  "payload_bytes": $PSIZE,
  "endpoint": "$GW_PATH",
  "model": "$GW_MODEL",
  "cores": "gateway=${CORES} loadgen=${LOADCORES} mock=${MOCKCORES}",
  "mock_proto": "h1+h2c",
  "arch": "${BENCH_ARCH:-$(uname -m)}",
  "hardware": "${BENCH_HARDWARE:-$(uname -m) $(nproc 2>/dev/null || echo '?')vCPU}",
  "measured_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON
echo "================================================================"
if [ "$STREAM_OK" = 1 ]; then
  echo " gateway=$GATEWAY   added TTFT p99=${ADD_T99}µs   added inter-frame p99=${ADD_G99}µs"
  echo "   streams sustained (stall-free) = ${STALLFREE_STREAMS} (${STALLFREE_FPS} frames/sec, mock_bound=${MOCK_BOUND}; delivered = ${SUST_STREAMS} @ ${SUST_FPS} fps)"
else
  echo " gateway=$GATEWAY   did not stream (stream_served=false)"
fi
echo " -> $RESULTS/$GATEWAY.json"
echo "================================================================"
