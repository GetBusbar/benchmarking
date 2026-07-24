#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# SHARED STREAMING-MEASUREMENT LIBRARY — the ONE implementation of the SSE streaming measurements,
# factored out of stream/run.sh + streamcpu/run.sh so the matrix suite can fold a per-cell streaming
# measurement into every served cell running the EXACT same code and gates. The standalone
# stream/streamcpu suites remain (unchanged callers of the ugen -stream probe); this library is the
# reusable core the MATRIX cell loop calls.
#
# TWO METRICS, TWO SEARCH SHAPES (this is the important part of the consolidation):
#   * streams-sustained — the max sustainable TRUE concurrency, found by BISECTION (not a ladder
#     rung). The stream gate (>=DELIV of expected frames delivered, error rate < 0.1%, zero stalled
#     streams) is MONOTONE in concurrency: below some ceiling every gate passes, above it they fail.
#     A ladder quantises the answer onto a preset rung (the old stream/run.sh SWEEP); bisection lands
#     on the true integer ceiling BETWEEN rungs, exactly like lib/sweep.sh finds the RPS peak between
#     doublings. stream_sustained_bisect sets SM_SUST_STREAMS / SM_SUST_FPS (fps AT that concurrency).
#   * cpu-fps — the PEAK sustained aggregate frames/sec (a throughput MAX, NOT fps-at-a-concurrency-
#     threshold). Unpaced streaming fps is unimodal in concurrency (climbs while the gateway keeps up,
#     plateaus/declines past saturation); the answer is the peak of that curve. streamcpu_peak_fps
#     runs a unimodal max-search (ramp until fps stops rising, then refine) and sets SM_FPS_PEAK /
#     SM_FPS_PEAK_CONC — the same peak-search shape lib/sweep.sh uses for the RPS max, adapted to the
#     fps-returning stream probe.
#
# Caller contract (globals the caller must set before use; all exist in stream/streamcpu/matrix):
#   UGEN        path to the built Go load generator
#   MOCK MOCK_PORT MOCKCORES LOADCORES   rig binary + pins (stream_mock_start restarts the mock)
#   GW_MODEL GW_AUTH PSIZE   loadgen request parameters
#   UGEN_H      array of extra -H headers for the loadgen (may be empty)
#   GURL DURL   gateway and direct-to-mock stream URLs
#   GATEWAY log tmo probe_budget suite_deadline_expired   (lib/harness.sh must be sourced first)
#   SM_EXPFRAMES  expected content frames per stream (stream: STREAM_CHUNKS; streamcpu: SC_CHUNKS)
#   SM_STALL_US   inter-frame gap above which a stream is "stalled" (µs)
#   SM_SWEEP_DUR  seconds per probe window
#   SM_C1_DUR     seconds for the concurrency-1 latency window (stream_c1 only)
#   SM_DELIV      delivered-frames floor for the sustained gate (fraction, e.g. 0.999)
#   SM_MOCKCEIL_CONC  cap on the reference-ceiling probe concurrency (default 2048)
#
# Provides:
#   stream_probe <url> <conc> <dur>   -> echoes the 11 stream fields (hard-timeout wrapped):
#       "streams complete fail stalled frames fps delivered ttft_p50us ttft_p99us gap_p50us gap_p99us"
#   stream_mock_ready [tries]         -> 0 once the fresh mock frames a 1-stream probe (fps>0)
#   stream_c1                         -> sets SM_ADD_T50 SM_ADD_T99 SM_ADD_G50 SM_ADD_G99
#                                        SM_GT50/99 SM_DT50/99 SM_GG50/99 SM_DG50/99 SM_C1_OK SM_C1_ERR
#   stream_sustained_bisect <lo> <hi> -> BISECT max sustained conc: SM_SUST_STREAMS SM_SUST_FPS
#                                        SM_SUST_JSON (probed points) SM_MOCK_FPS SM_MOCK_BOUND
#   streamcpu_peak_fps <lo> <hi>      -> PEAK fps max-search: SM_FPS_PEAK SM_FPS_PEAK_CONC
#                                        SM_FPS_JSON (probed points) SM_DIRECT_CEIL SM_FPS_MOCK_BOUND

# Memoisation for the searches (bash>=4 assoc arrays; `|| true` keeps this sourceable on bash 3.2).
declare -gA SM_PROBE_STREAMS 2>/dev/null || true
declare -gA SM_PROBE_FPS 2>/dev/null || true
declare -gA SM_PROBE_OK 2>/dev/null || true

# ── stream_mock_start: (re)start the mock in a streaming shape ────────────────────────────────────
# chunks/interval_ms/chunk_bytes select the SSE shape: stream/ paces at ~20ms; streamcpu/ + the matrix
# cpu-fps measurement UNPACE (interval 0) so per-frame relay cost dominates. Idempotent restart.
stream_mock_start(){ # chunks interval_ms chunk_bytes
  [ -n "$MOCK" ] && pkill -f "$MOCK" 2>/dev/null; sleep 1
  setsid taskset -c "$MOCKCORES" env \
    MOCK_STREAM_CHUNKS="$1" MOCK_STREAM_INTERVAL_MS="$2" MOCK_STREAM_CHUNK_BYTES="$3" \
    "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
  sleep 1
}

# ── stream_probe: one ugen SSE probe, fixed-order k=v fields (same parser as stream/streamcpu) ─────
# HARD TIMEOUT (tmo): an unresponsive gateway that leaves streams open must not block on ugen's tail
# client timeout across every hung worker; cap at dur+grace and treat empty output as not-served.
stream_probe(){ # url conc dur
  tmo "$(probe_budget "$3")" taskset -c "$LOADCORES" "$UGEN" -url "$1" -model "$GW_MODEL" -auth "$GW_AUTH" -c "$2" -d "$3" \
    -psize "$PSIZE" -stream -expframes "$SM_EXPFRAMES" -stallus "$SM_STALL_US" ${UGEN_H[@]+"${UGEN_H[@]}"} 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++){split($i,a,"=");v[a[1]]=a[2]};
        print v["streams"],v["complete"],v["fail"],v["stalled"],v["frames"],v["fps"],v["delivered"],
              v["ttft_p50us"],v["ttft_p99us"],v["gap_p50us"],v["gap_p99us"]}'
}

# ── stream_mock_ready: poll a 1-stream probe until the fresh mock frames (fps>0) ───────────────────
# Ported from lib/sweep.sh:_sw_mock_ready / stream/run.sh:_stream_mock_ready. After a blind `sleep 1`
# the mock may not be bound; the ceiling probe then reads 0 fps and the mock-bound guard silently
# no-ops. A 1-stream probe both proves the port is bound and is the rig-shaped request the ceiling
# probe uses. Returns non-zero if the mock never came up (caller flags the reference unusable).
stream_mock_ready(){ # tries
  local i="${1:-30}" _s _c _f _st _fr fps _rest
  while [ "$i" -gt 0 ]; do
    read -r _s _c _f _st _fr fps _rest < <(stream_probe "$DURL" 1 1)
    [ "${fps:-0}" -gt 0 ] 2>/dev/null && return 0
    i=$((i-1)); sleep 1
  done
  return 1
}

# ── stream_c1: concurrency-1 added TTFT + inter-frame gap, with an honesty gate ───────────────────
# Discarded warm-up on both paths (JIT/interpreted gateways not charged cold start), mirroring
# perf/run.sh + stream/run.sh. Then gates the c1 window: real frames on both paths and stream error
# rate < 0.1%, else SM_C1_OK=0 with the evidence in SM_C1_ERR (the caller nulls its latency fields).
stream_c1(){
  local WARMUP_DUR="${WARMUP_DUR:-5}"
  log "[$GATEWAY] warm-up ${WARMUP_DUR}s (discarded, both paths)"
  stream_probe "$DURL" 1 "$WARMUP_DUR" >/dev/null 2>&1; stream_probe "$GURL" 1 "$WARMUP_DUR" >/dev/null 2>&1
  local _ds _dc _df _dst _dfr _dfp _dd _gs _gc _gf _gst _gfr _gfp _gd
  log "[$GATEWAY] c1 stream baseline (direct→mock) ${SM_C1_DUR}s"
  read -r _ds _dc _df _dst _dfr _dfp _dd SM_DT50 SM_DT99 SM_DG50 SM_DG99 < <(stream_probe "$DURL" 1 "$SM_C1_DUR")
  log "[$GATEWAY] c1 stream gateway ${SM_C1_DUR}s"
  read -r _gs _gc _gf _gst _gfr _gfp _gd SM_GT50 SM_GT99 SM_GG50 SM_GG99 < <(stream_probe "$GURL" 1 "$SM_C1_DUR")
  SM_C1_OK=1; SM_C1_ERR=""
  if [ "${_gfr:-0}" -le 0 ] || [ "${_dfr:-0}" -le 0 ] || [ "${SM_GT99:-0}" -le 0 ] || [ "${SM_DT99:-0}" -le 0 ] \
     || ! awk -v f="${_gf:-1}" -v s="${_gs:-0}" 'BEGIN{exit !(s>0 && f<=0.001*s)}' \
     || ! awk -v f="${_df:-1}" -v s="${_ds:-0}" 'BEGIN{exit !(s>0 && f<=0.001*s)}'; then
    SM_C1_OK=0
    SM_C1_ERR="c1 stream window unreliable: gw streams=${_gs:-0} fail=${_gf:-?} frames=${_gfr:-0} ttft_p99=${SM_GT99:-0}us; direct streams=${_ds:-0} fail=${_df:-?} frames=${_dfr:-0}"
    SM_GT50=0; SM_GT99=0; SM_DT50=0; SM_DT99=0; SM_GG50=0; SM_GG99=0; SM_DG50=0; SM_DG99=0
    log "[$GATEWAY] WARNING stream c1 window had errors / no frames"
  fi
  SM_ADD_T50=$(( ${SM_GT50:-0} - ${SM_DT50:-0} )); SM_ADD_T99=$(( ${SM_GT99:-0} - ${SM_DT99:-0} ))
  SM_ADD_G50=$(( ${SM_GG50:-0} - ${SM_DG50:-0} )); SM_ADD_G99=$(( ${SM_GG99:-0} - ${SM_DG99:-0} ))
  log "[$GATEWAY] c1: added TTFT p99=${SM_ADD_T99}µs (p50=${SM_ADD_T50}µs)  added gap p99=${SM_ADD_G99}µs (p50=${SM_ADD_G50}µs)"
}

# ── memoised concurrency-keyed stream probes (for the BISECT + PEAK searches) ─────────────────────
# Probe an ARBITRARY concurrency once, memoised so a re-probe is free. _sm_ok<c> = 1 iff the stream
# gate passed (>=SM_DELIV delivered, error rate < 0.1%, zero stalled streams); _sm_fps<c> = its fps.
# Every probed conc is appended (with its full record) to SM_JSON_ACC. Returns 1 if the deadline fired.
_sm_probe_c(){ # conc
  local c="$1"
  if [ -z "${SM_PROBE_OK[$c]:-}" ]; then
    if suite_deadline_expired; then log "[$GATEWAY] suite wall-clock ceiling reached mid-stream-search - stopping"; return 1; fi
    local streams complete fail stalled frames fps delivered t50 t99 g50 g99
    read -r streams complete fail stalled frames fps delivered t50 t99 g50 g99 < <(stream_probe "$GURL" "$c" "$SM_SWEEP_DUR")
    streams=${streams:-0}; complete=${complete:-0}; fail=${fail:-1}; stalled=${stalled:-1}; fps=${fps:-0}; delivered=${delivered:-0}
    SM_PROBE_FPS[$c]=$fps
    if awk -v f="$fail" -v s="$streams" -v d="$delivered" -v st="$stalled" -v md="$SM_DELIV" \
         'BEGIN{exit !(s>0 && f<=0.001*s && d>=md && st==0)}'; then SM_PROBE_OK[$c]=1; else SM_PROBE_OK[$c]=0; fi
    SM_PROBE_STREAMS[$c]=$streams
    SM_JSON_ACC="${SM_JSON_ACC}${SM_JSON_ACC:+,}{\"conc\":$c,\"streams\":$streams,\"complete\":$complete,\"fail\":$fail,\"stalled\":$stalled,\"frames\":${frames:-0},\"fps\":$fps,\"delivered\":$delivered,\"ttft_p99_us\":${t99:-0},\"gap_p99_us\":${g99:-0}}"
    log "[$GATEWAY]   c=$c → streams=$streams fps=$fps delivered=$delivered stalled=$stalled fail=$fail (gate=${SM_PROBE_OK[$c]})"
  fi
  return 0
}

# ── stream_sustained_bisect: BISECT the max sustainable true concurrency ──────────────────────────
# The gate is monotone in concurrency, so we bracket [lo(passes), hi(fails)] then bisect to the true
# integer ceiling. First establish lo passes and hi fails: probe lo; if it already fails, there is no
# sustainable concurrency (SM_SUST_STREAMS=0). Probe hi; if it PASSES, the ceiling is at/above hi so
# hi IS the answer (the search grid can't offer more). Otherwise bisect [lo,hi] to +-1. The answer is
# the highest probed concurrency whose gate passed; SM_SUST_FPS is that concurrency's fps.
stream_sustained_bisect(){ # lo hi
  local lo="$1" hi="$2"
  SM_PROBE_STREAMS=(); SM_PROBE_FPS=(); SM_PROBE_OK=(); SM_JSON_ACC=""
  SM_SUST_STREAMS=0; SM_SUST_FPS=0
  # Mock-ceiling reference (paced streaming), capped like the perf lane so a very-high-conc reference
  # is not artificially low. Fair-ceiling re-probe at the winner mirrors lib/sweep.sh:_sw_ceil_ref_ok.
  local mock_conc=$hi; [ "$mock_conc" -gt "${SM_MOCKCEIL_CONC:-2048}" ] && mock_conc="${SM_MOCKCEIL_CONC:-2048}"
  SM_MOCK_FPS_CONC=$mock_conc
  local _mf _a _b _c _d
  read -r _a _b _c _d _e SM_MOCK_FPS _rest < <(stream_probe "$DURL" "$mock_conc" "$SM_SWEEP_DUR")
  SM_MOCK_FPS=${SM_MOCK_FPS:-0}
  log "[$GATEWAY] sustained-streams bisect [$lo,$hi] (mock ceiling ${SM_MOCK_FPS} fps @ c=$mock_conc)"
  _sm_probe_c "$lo" || { _sm_finish_bound; return 0; }
  if [ "${SM_PROBE_OK[$lo]}" != 1 ]; then
    log "[$GATEWAY] sustained-streams: even c=$lo did not pass the gate → 0"
    _sm_finish_bound; return 0
  fi
  SM_SUST_STREAMS=$lo; SM_SUST_FPS=${SM_PROBE_FPS[$lo]}
  _sm_probe_c "$hi" || { _sm_finish_bound; return 0; }
  if [ "${SM_PROBE_OK[$hi]}" = 1 ]; then
    SM_SUST_STREAMS=$hi; SM_SUST_FPS=${SM_PROBE_FPS[$hi]}   # ceiling at/above the grid top
    log "[$GATEWAY] sustained-streams: c=$hi passes (ceiling at/above grid top) → $hi"
    _sm_finish_bound; return 0
  fi
  # Invariant: lo passes, hi fails. Bisect to +-1.
  local a=$lo b=$hi mid
  while [ $(( b - a )) -gt 1 ]; do
    mid=$(( (a + b) / 2 ))
    _sm_probe_c "$mid" || break
    if [ "${SM_PROBE_OK[$mid]}" = 1 ]; then a=$mid; SM_SUST_STREAMS=$mid; SM_SUST_FPS=${SM_PROBE_FPS[$mid]}; else b=$mid; fi
  done
  log "[$GATEWAY] sustained-streams bisect → $SM_SUST_STREAMS (fps=$SM_SUST_FPS)"
  _sm_finish_bound
}

# Fair-ceiling re-probe + mock-bound decision for the sustained figure (ported from stream/run.sh).
# Re-probe the rig once at the winner's concurrency (capped 4x the reference) and adopt the LARGER fps
# as the fair ceiling; then SM_MOCK_BOUND=true iff the winner's fps is within 10% of it. null when the
# reference is unusable (0 ceiling) — never a trustworthy-looking false over a dead reference.
_sm_finish_bound(){
  if [ "${SM_SUST_STREAMS:-0}" -gt 0 ] && [ "${SM_SUST_STREAMS}" -ne "${SM_MOCK_FPS_CONC:-0}" ] && [ "${SM_MOCK_FPS:-0}" -gt 0 ]; then
    local reprobe=$SM_SUST_STREAMS capc=$(( SM_MOCK_FPS_CONC>0 ? SM_MOCK_FPS_CONC*4 : SM_SUST_STREAMS ))
    [ "$reprobe" -gt "$capc" ] && reprobe=$capc
    local _a _b _c _d _e _rm _rest
    read -r _a _b _c _d _e _rm _rest < <(stream_probe "$DURL" "$reprobe" "$SM_SWEEP_DUR"); _rm=${_rm:-0}
    if [ "$_rm" -gt "${SM_MOCK_FPS:-0}" ]; then
      log "[$GATEWAY] mock-ceiling re-probed at winner c=$reprobe: ${SM_MOCK_FPS} -> $_rm fps"
      SM_MOCK_FPS=$_rm; SM_MOCK_FPS_CONC=$reprobe
    fi
  fi
  if [ "${SM_MOCK_FPS:-0}" -le 0 ]; then SM_MOCK_BOUND=null
  else
    SM_MOCK_BOUND=false
    awk -v c="${SM_SUST_FPS:-0}" -v m="$SM_MOCK_FPS" 'BEGIN{exit !(c>=0.9*m)}' && SM_MOCK_BOUND=true
  fi
}

# ── streamcpu_peak_fps: PEAK sustained fps via a unimodal max-search ──────────────────────────────
# fps is unimodal in concurrency (climbs while the gateway keeps up, plateaus/declines past
# saturation, all while the gate still passes). The answer is the PEAK. Ramp up (double) while a
# probed rung both passes the gate AND raises fps; when fps stops rising the peak is bracketed and we
# refine the wider gap, keeping the side holding the higher fps, to +-SM_FPS_TOL. This mirrors
# lib/sweep.sh mode=peak's max-search, on the fps-returning stream probe (unpaced mock).
# _sm_eff <c>: the value the search maximises — fps if the gate passed, else 0 (a gate-failing rung
# can never be the peak, so the search moves away from it).
streamcpu_peak_fps(){ # lo hi
  local lo="$1" hi="$2"
  SM_PROBE_STREAMS=(); SM_PROBE_FPS=(); SM_PROBE_OK=(); SM_JSON_ACC=""
  SM_FPS_PEAK=0; SM_FPS_PEAK_CONC=0
  # Direct-to-mock ceiling at the grid top (the guardrail): if a gateway approaches it the run is
  # mock-bound. Measured at the top like streamcpu/run.sh (unpaced firehose saturates the rig here).
  local _a _b _c _d _e _rest
  read -r _a _b _c _d _e SM_DIRECT_CEIL _rest < <(stream_probe "$DURL" "$hi" "$SM_SWEEP_DUR")
  SM_DIRECT_CEIL=${SM_DIRECT_CEIL:-0}
  log "[$GATEWAY] cpu-fps peak-search [$lo,$hi] (direct ceiling ${SM_DIRECT_CEIL} fps @ c=$hi)"
  _sm_eff(){ if [ "${SM_PROBE_OK[$1]:-0}" = 1 ]; then printf '%s' "${SM_PROBE_FPS[$1]:-0}"; else printf 0; fi; }
  local a=$lo b=$lo pr=0 c cr
  _sm_probe_c "$lo" || { _sm_fps_finish_bound; return 0; }
  pr=$(_sm_eff "$lo"); b=$lo
  c=$lo
  while [ "$c" -lt "$hi" ]; do
    c=$(( c*2 > hi ? hi : c*2 ))
    _sm_probe_c "$c" || break
    cr=$(_sm_eff "$c")
    if [ "$cr" -gt "$pr" ]; then a=$b; b=$c; pr=$cr; else break; fi
  done
  local top=$c                                        # first rung that did not raise fps (or hi)
  # Refine the peak in [a, top] (unimodal max-search), stopping within SM_FPS_TOL.
  local TOL="${SM_FPS_TOL:-4}" x xr
  while [ $(( top - a )) -gt "$TOL" ]; do
    if [ $(( b - a )) -ge $(( top - b )) ]; then x=$(( (a + b)/2 )); else x=$(( (b + top)/2 )); fi
    if [ "$x" = "$a" ] || [ "$x" = "$b" ] || [ "$x" = "$top" ]; then break; fi
    _sm_probe_c "$x" || break; xr=$(_sm_eff "$x")
    if [ "$x" -lt "$b" ]; then
      if [ "$xr" -gt "$pr" ]; then top=$b; b=$x; pr=$xr; else a=$x; fi
    else
      if [ "$xr" -gt "$pr" ]; then a=$b; b=$x; pr=$xr; else top=$x; fi
    fi
  done
  # The winner is the max gate-passing fps across every probed concurrency (ascending, deterministic).
  local k
  for k in $(printf '%s\n' "${!SM_PROBE_OK[@]}" | sort -n); do
    if [ "${SM_PROBE_OK[$k]}" = 1 ] && [ "${SM_PROBE_FPS[$k]}" -gt "$SM_FPS_PEAK" ]; then
      SM_FPS_PEAK=${SM_PROBE_FPS[$k]}; SM_FPS_PEAK_CONC=$k
    fi
  done
  log "[$GATEWAY] cpu-fps peak → ${SM_FPS_PEAK} fps @ c=$SM_FPS_PEAK_CONC"
  _sm_fps_finish_bound
}

# mock-bound decision for the cpu-fps peak (direct ceiling was measured at the grid top).
_sm_fps_finish_bound(){
  if [ "${SM_DIRECT_CEIL:-0}" -le 0 ]; then SM_FPS_MOCK_BOUND=null
  else
    SM_FPS_MOCK_BOUND=false
    awk -v c="${SM_FPS_PEAK:-0}" -v m="$SM_DIRECT_CEIL" 'BEGIN{exit !(c>=0.9*m)}' && SM_FPS_MOCK_BOUND=true
  fi
}
