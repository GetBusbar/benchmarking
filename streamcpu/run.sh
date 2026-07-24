#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# STREAMING (CPU-BOUND) -- pluggable across gateways (same gateways/<name>/gateway.sh manifests as
# perf/memory/stream). This is the streaming analogue of perf's "max proxy throughput" sweep.
#
# WHY THIS SUITE EXISTS
# ---------------------
# The stream/ suite paces the mock's SSE frames at a fixed inter-frame interval (~20ms), simulating
# real token cadence. Under that pace the gateway sits IDLE between frames, so its per-frame relay
# CPU cost (single-digit microseconds) hides inside the idle gap and is unmeasurable: the "added
# per-token" number is the difference of two noisy p99s and flips sign run to run. That is honest
# for TTFT/gap-under-realistic-pace, but useless as a per-frame CPU comparison.
#
# This suite instead REMOVES the pace (MOCK_STREAM_INTERVAL_MS=0, so the mock emits every content
# frame back to back) and drives MANY concurrent streams, each a long burst of token-sized frames,
# with the gateway pinned to a fixed core count (CORES). The metric is the gateway's SUSTAINED
# aggregate frames/sec: a gateway that spends less CPU per relayed frame sustains more frames/sec on
# the same cores. This turns per-frame cost into a THROUGHPUT number (a rate), which is far more
# stable than a latency difference, exactly like perf's RPS ceiling is more stable than added p99.
#
# A naive "just unpace it" firehose does NOT work on its own: if the mock or loadgen is the
# bottleneck we measure them, not the gateway. So every run FIRST measures the direct-to-mock
# frames/sec ceiling at the top concurrency (loadgen -> mock, no gateway). If a gateway's fps lands
# within 10% of that ceiling the run is flagged streamcpu_mock_bound=true -- the measurement is
# mock-bound and must not be read as a gateway comparison, exactly like perf's mock_bound guard.
# Mock and loadgen are pinned to DIFFERENT cores than the gateway (MOCKCORES/LOADCORES).
#
# WHAT IT WRITES  -> results/streamcpu/<gateway>.json
#   streamcpu_frames_per_sec      best sustained aggregate content-frames/sec across the sweep
#   streamcpu_fps_per_core        that / gateway core count (per-core relay throughput)
#   streamcpu_concurrency         concurrency at which the best fps was reached
#   streamcpu_direct_ceiling_fps  loadgen->mock direct fps at the top concurrency (the guardrail)
#   streamcpu_mock_bound          true if best fps >= 90% of the direct ceiling (measurement invalid
#                                 as a comparison -- mock/loadgen limited, flagged not hidden)
#   streamcpu_added_per_frame_us  SECONDARY: CPU-time added per relayed frame under saturation,
#                                 derived from direct vs gateway per-frame service time; null if the
#                                 direct ceiling was itself mock-bound-shaped (not cleanly derivable)
#   sweep_streamcpu[]             per-concurrency curve (fps, delivered%, stalled, fail)
#   plus served/stream_served/build/hardware/measured_at/frame_bytes/... (same style as stream/).
#
#   GATEWAY=busbar streamcpu/run.sh
#
# Knobs (env): SC_CHUNKS (content frames per stream, default 512 -- long bursts so per-frame cost
#   dominates per-stream setup), SC_FRAME_BYTES (token-sized SSE delta payload, default 16),
#   SC_SWEEP (concurrency grid, default "8 32 64 128 256"), SC_DUR (seconds per sweep point,
#   default 20), SC_STALL_MS (a content-frame gap above this marks a stream stalled, default 250 --
#   an unpaced stream that gaps 250ms is starved, not relaying), PSIZE (request bytes, default 256),
#   CORES/MOCKCORES/LOADCORES pins.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
GATEWAY="${GATEWAY:-busbar}"
export GW_DIR="$ROOT/gateways/$GATEWAY"
[ -f "$GW_DIR/gateway.sh" ] || { echo "unknown gateway '$GATEWAY'"; exit 2; }

SC_CHUNKS="${SC_CHUNKS:-512}"
SC_FRAME_BYTES="${SC_FRAME_BYTES:-16}"
SC_DUR="${SC_DUR:-20}"; PSIZE="${PSIZE:-256}"
SC_STALL_MS="${SC_STALL_MS:-250}"
SC_STALL_US=$(( SC_STALL_MS * 1000 ))
# Concurrency = simultaneously open unpaced SSE streams. Kept modest (peaks in the low hundreds):
# each stream is a firehose, so a handful already saturate a few cores. Same grid for every gateway.
SC_SWEEP="${SC_SWEEP:-8 32 64 128 256}"
# raise the fd limit so the sweep isn't capped by open sockets
ulimit -n 1048576 2>/dev/null || ulimit -n 65536 2>/dev/null || true
export CORES="${CORES:-0-3}"; LOADCORES="${LOADCORES:-0-3}"; MOCKCORES="${MOCKCORES:-0-3}"
export MOCK_PORT="${MOCK_PORT:-8000}"
RESULTS="$ROOT/results/streamcpu"; mkdir -p "$RESULTS"
log(){ echo "[$(date +%H:%M:%S)] $*"; }
command -v taskset >/dev/null || taskset(){ shift 2; "$@"; }

# gateway core count -> fps-per-core. Parse the CORES pin ("0-3" -> 4, "0" -> 1, "0-3,8-11" -> 8).
core_count() {
  local spec="$1" n=0 part lo hi
  IFS=',' read -ra _parts <<< "$spec"
  for part in "${_parts[@]}"; do
    if [[ "$part" == *-* ]]; then lo="${part%-*}"; hi="${part#*-}"; n=$(( n + hi - lo + 1 ));
    else n=$(( n + 1 )); fi
  done
  [ "$n" -gt 0 ] && echo "$n" || echo 1
}
NCORES="$(core_count "$CORES")"

log "fetching prebuilt rig (mock + loadgen) — no on-box toolchain needed"
. "$ROOT/lib/rig.sh"; fetch_rig "$ROOT" || { echo "rig fetch failed"; exit 1; }

[ -f "$ROOT/gateways/versions.env" ] && source "$ROOT/gateways/versions.env"
gw_version(){ echo unknown; }; GW_HEADERS=()
gw_diag(){ :; }
json_escape(){ printf '%s' "$1" | tr -d '\000' | head -c 1600 \
  | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read())[1:-1])' 2>/dev/null \
  || printf '%s' "$1" | tr '\n\t"\\' '    ' | head -c 1600; }
# shellcheck source=/dev/null
source "$ROOT/lib/harness.sh"
# shellcheck source=/dev/null
source "$GW_DIR/gateway.sh"
suite_deadline_start

# Start the mock UNPACED: interval 0 = every content frame emitted back to back. Long bursts.
log "starting mock :$MOCK_PORT (UNPACED stream ${SC_CHUNKS}x${SC_FRAME_BYTES}B @ 0ms)"
[ -n "$MOCK" ] && pkill -f "$MOCK" 2>/dev/null; sleep 1
setsid taskset -c "$MOCKCORES" env \
  MOCK_STREAM_CHUNKS="$SC_CHUNKS" MOCK_STREAM_INTERVAL_MS=0 \
  MOCK_STREAM_CHUNK_BYTES="$SC_FRAME_BYTES" \
  "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
sleep 1
cleanup(){ gw_stop 2>/dev/null; [ -n "$MOCK" ] && pkill -f "$MOCK" 2>/dev/null; }
trap cleanup EXIT

# run ugen in SSE mode, echo the k=v result fields in a fixed order (same parser style as stream/).
sprobe(){ # url conc dur
  tmo "$(probe_budget "$3")" taskset -c "$LOADCORES" "$UGEN" -url "$1" -model "$GW_MODEL" -auth "$GW_AUTH" -c "$2" -d "$3" \
    -psize "$PSIZE" -stream -expframes "$SC_CHUNKS" -stallus "$SC_STALL_US" "${UGEN_H[@]}" 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++){split($i,a,"=");v[a[1]]=a[2]};
        print v["streams"],v["complete"],v["fail"],v["stalled"],v["frames"],v["fps"],v["delivered"]}'
}

log "[$GATEWAY] build (pinned to cores $CORES = ${NCORES} core(s))"; gw_build || { echo "build failed"; exit 1; }
UGEN_H=(); CURL_H=(); ok=0; c=000
_scpu_ready(){
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
if harness_launch_ready gw_launch _scpu_ready; then ok=1
else SERVE_ERR="$HARNESS_SERVE_ERR"; fi

# does the gateway actually stream? one probe, then fail gracefully if not (same guard as stream/).
STREAM_OK=0; STREAM_ERR=""
if [ "$ok" = 1 ]; then
  # Retry the SSE readiness probe: a gateway can answer the non-stream warm-up 200 a beat before its
  # upstream stream pool is warm, so the very first stream request can race cold -- returning nothing,
  # or HANGING the connection open with no frames until timeout. So give the gateway a brief settle,
  # then retry up to 15 times with a SHORT per-attempt timeout (a real unpaced stream frames in well
  # under a second; a cold hang must not eat the whole retry budget) before declaring stream_served=false.
  sleep "${STREAM_SETTLE:-2}"
  sbody=""
  for i in $(seq 1 15); do
    sbody="$(curl -sN -m "${STREAM_PROBE_TO:-6}" "http://127.0.0.1:$GW_PORT$GW_PATH" -X POST \
        -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" \
        -H "accept: text/event-stream" "${CURL_H[@]}" \
        -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16,\"stream\":true}" 2>&1)"
    # here-string, not a pipe: with `set -o pipefail`, `printf ... | grep -q` reports the pipeline as
    # failed when grep matches early and printf takes SIGPIPE (141) -- a false negative even though the
    # body is full of frames. grep on a here-string reads the whole string and returns grep's own status.
    if grep -q '^data:' <<< "$sbody"; then STREAM_OK=1; break; fi
    sleep 1
  done
  if [ "$STREAM_OK" != 1 ]; then
    STREAM_ERR="no SSE frames on stream:true after 15 tries; body=[$(printf '%s' "$sbody" | head -c 400)]; diag=[$(gw_diag 2>&1 | tail -n 20)]"
    log "[$GATEWAY] WARNING stream:true produced no SSE frames -- stream_served=false"
  fi
fi

DURL="http://127.0.0.1:$MOCK_PORT$GW_PATH"; GURL="http://127.0.0.1:$GW_PORT$GW_PATH"
BEST_FPS=0; BEST_CONC=0; DIRECT_CEIL=0; MOCK_BOUND=false; ADDED_PER_FRAME_US=null; SWEEP_JSON=""; STREAMCPU_NOTE=""
FPS_PER_CORE=0
if [ "$STREAM_OK" = 1 ]; then
  # Discarded warm-up for both paths (JIT/interpreted gateways not charged cold start), mirroring perf.
  WARMUP_DUR="${WARMUP_DUR:-5}"
  log "[$GATEWAY] warm-up ${WARMUP_DUR}s (discarded, both paths)"
  sprobe "$DURL" 8 "$WARMUP_DUR" >/dev/null 2>&1; sprobe "$GURL" 8 "$WARMUP_DUR" >/dev/null 2>&1

  # Direct-to-mock ceiling at the TOP concurrency: the loadgen+mock frames/sec with no gateway. If a
  # gateway approaches this, the run is mock-bound (flagged). Also used for the added-per-frame math.
  top=8; for w in $SC_SWEEP; do top=$w; done
  read -r _s _c _f _st _fr DIRECT_CEIL _d < <(sprobe "$DURL" "$top" "$SC_DUR")
  DIRECT_CEIL=${DIRECT_CEIL:-0}
  log "[$GATEWAY] direct-to-mock ceiling = ${DIRECT_CEIL} frames/sec @ c=$top (guardrail)"
  # An empty ceiling probe (mock cold after restart, timeout, EMFILE) leaves DIRECT_CEIL=0, which
  # DISABLES the mock-bound guard below (line ~213) - so a rig-limited fps could otherwise ship as a
  # valid gateway measurement. Record it so streamcpu_valid can require a working reference (R2-H3).
  [ "${DIRECT_CEIL:-0}" -gt 0 ] || STREAMCPU_NOTE="direct-to-mock ceiling probe returned 0 fps (mock cold/timeout/EMFILE): mock-bound guard could not run, so this fps is unverified against the rig ceiling"

  # Gateway sweep: aggregate content-frames/sec sustained at each concurrency. A point counts toward
  # the best only if the gateway relayed the stream cleanly:
  #   * error rate under 0.1% (a 200 that never framed counts as failed in ugen), AND
  #   * COMPLETION rate >= 99% -- each stream reached the finish marker ([DONE]/message_stop), so the
  #     gateway did not truncate or drop the tail, AND
  #   * no stream stalled past SC_STALL_MS (a starved, not-relaying stream), AND
  #   * a LENIENT delivered-frames floor (SC_MIN_DELIVERED, default 0.5). delivered = content frames
  #     seen / frames the mock sent. It is deliberately NOT required to be ~1.0: a gateway may
  #     legitimately RE-CHUNK (coalesce) the upstream SSE into fewer, larger frames (LiteLLM does this
  #     ~256->100), which is valid relay behavior, not a fault. The floor only catches gross content
  #     loss. A coalescing gateway is then judged on its real frames/sec, not zeroed for re-chunking.
  # The best qualifying fps is the headline.
  local_min_del="${SC_MIN_DELIVERED:-0.5}"
  log "[$GATEWAY] streamcpu sweep -- sustained frames/sec (qualify: complete>=99%, stalls=0, delivered>=${local_min_del})"
  for conc in $SC_SWEEP; do
    if suite_deadline_expired; then log "[$GATEWAY] suite wall-clock ceiling reached mid-sweep -- stopping sweep, recording what we have"; break; fi
    read -r streams complete fail stalled frames fps delivered < <(sprobe "$GURL" "$conc" "$SC_DUR")
    streams=${streams:-0}; complete=${complete:-0}; fail=${fail:-1}; stalled=${stalled:-1}; fps=${fps:-0}; delivered=${delivered:-0}
    log "[$GATEWAY]   c=$conc -> fps=$fps delivered=$delivered complete=$complete stalled=$stalled fail=$fail streams=$streams"
    SWEEP_JSON="${SWEEP_JSON}${SWEEP_JSON:+,}{\"conc\":$conc,\"streams\":$streams,\"complete\":${complete},\"fail\":$fail,\"stalled\":$stalled,\"frames\":${frames:-0},\"fps\":$fps,\"delivered\":$delivered}"
    if awk -v f="$fail" -v s="$streams" -v cm="$complete" -v d="$delivered" -v st="$stalled" -v md="$local_min_del" \
         'BEGIN{exit !(s>0 && f<=0.001*s && cm>=0.99*s && d>=md && st==0)}' \
       && awk -v a="$fps" -v b="$BEST_FPS" 'BEGIN{exit !(a>b)}'; then
      BEST_FPS=$fps; BEST_CONC=$conc
    fi
  done

  # mock-bound guard (perf-style): best gateway fps within 10% of the direct ceiling -> flagged.
  if [ "${DIRECT_CEIL:-0}" -gt 0 ] && awk -v c="$BEST_FPS" -v m="$DIRECT_CEIL" 'BEGIN{exit !(c>=0.9*m)}'; then MOCK_BOUND=true; fi
  [ "$MOCK_BOUND" = true ] && log "[$GATEWAY] WARN best fps ($BEST_FPS) within 10% of direct ceiling ($DIRECT_CEIL) -- MOCK-BOUND, not a valid comparison"

  # fps per core (relay throughput per pinned gateway core).
  FPS_PER_CORE=$(awk -v f="$BEST_FPS" -v n="$NCORES" 'BEGIN{printf "%.0f", (n>0? f/n : f)}')

  # SECONDARY: added CPU-time per relayed frame under saturation. Per-frame service time is 1/fps
  # seconds of aggregate CPU; the gateway adds (1/gw_fps - 1/direct_fps) seconds per frame, in us.
  # Only meaningful when NOT mock-bound and both rates are real; otherwise null (not derivable).
  if [ "$MOCK_BOUND" = false ] && [ "$BEST_FPS" -gt 0 ] && [ "${DIRECT_CEIL:-0}" -gt 0 ]; then
    ADDED_PER_FRAME_US=$(awk -v g="$BEST_FPS" -v d="$DIRECT_CEIL" \
      'BEGIN{v=(1.0/g - 1.0/d)*1e6; if(v<0)v=0; printf "%.2f", v}')
  fi
  log "[$GATEWAY] streamcpu = ${BEST_FPS} frames/sec @ c=$BEST_CONC (${FPS_PER_CORE}/core, added/frame=${ADDED_PER_FRAME_US}us, mock_bound=${MOCK_BOUND})"
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
  "streamcpu_frames_per_sec": $BEST_FPS,
  "streamcpu_fps_per_core": $FPS_PER_CORE,
  "streamcpu_concurrency": $BEST_CONC,
  "streamcpu_direct_ceiling_fps": ${DIRECT_CEIL:-0},
  "streamcpu_mock_bound": $MOCK_BOUND,
  "streamcpu_valid": $([ "$STREAM_OK" = 1 ] && [ "${DIRECT_CEIL:-0}" -gt 0 ] && [ "$MOCK_BOUND" = false ] && [ "$BEST_FPS" -gt 0 ] && echo true || echo false),
  "streamcpu_note": "$(json_escape "$STREAMCPU_NOTE")",
  "streamcpu_added_per_frame_us": ${ADDED_PER_FRAME_US:-null},
  "streamcpu_frames_per_stream": $SC_CHUNKS,
  "streamcpu_frame_bytes": $SC_FRAME_BYTES,
  "streamcpu_cores": $NCORES,
  "streamcpu_stall_ms": $SC_STALL_MS,
  "sweep_streamcpu": [$SWEEP_JSON],
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
  echo " gateway=$GATEWAY   streamcpu = ${BEST_FPS} frames/sec @ c=${BEST_CONC}  (${FPS_PER_CORE}/core)"
  echo "   direct ceiling = ${DIRECT_CEIL} fps   mock_bound=${MOCK_BOUND}   added/frame=${ADDED_PER_FRAME_US}us"
else
  echo " gateway=$GATEWAY   did not stream (stream_served=false)"
fi
echo " -> $RESULTS/$GATEWAY.json"
echo "================================================================"
