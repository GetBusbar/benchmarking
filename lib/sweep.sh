#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# SHARED SWEEP + C1 ADDED-LATENCY LIBRARY - the ONE implementation of the throughput sweep and the
# concurrency-1 added-latency measurement, factored out of perf/run.sh so the matrix suite's
# per-cell perf (every green translation cell gets its own sweep) runs the EXACT same code with the
# same gates: 20ms sustained gate p99 < P99_CEIL_MS with error rate < 0.1%, and the mock-ceiling /
# mock_bound guard. perf/run.sh sources this and produces byte-identical result shapes.
#
# Caller contract (globals the caller must set before use; all already exist in perf/run.sh):
#   UGEN        path to the built Go load generator
#   MOCK        path to the built Rust mock binary (run_sweep restarts it per delay)
#   MOCK_PORT   mock port; MOCKCORES the mock's CPU pin
#   GW_MODEL GW_AUTH PSIZE   loadgen request parameters
#   UGEN_H      array of extra -H headers for the loadgen (may be empty)
#   SWEEP_BODY  optional: a verbatim request body for every loadgen request (matrix per-cell sweep
#               sends the cell's exact ingress-dialect probe body); empty = ugen's -shape body
#   DURL GURL   direct-to-mock and gateway URLs for the run
#   C1_DUR SWEEP_DUR WARMUP_DUR P99_CEIL_MS   window lengths + latency gate
#   GATEWAY log tmo probe_budget suite_deadline_expired   (lib/harness.sh must be sourced first)
#
# Provides:
#   sweep_probe <url> <conc> <dur>   -> echoes "rps fail p99us p50us" (hard-timeout wrapped)
#   sweep_c1                         -> sets DP50 DP99 GP50 GP99 OVER_P50 OVER_P99 C1_OK C1_ERR
#   run_sweep <ttft_ms> <conc_list>  -> sets SW_CEIL_RPS SW_CEIL_CONC SW_CEIL_P99 SW_MOCK_CEIL
#                                       SW_BOUND SW_JSON

# run ugen, echo "rps fail p99us p50us" parsed from its output line. HARD TIMEOUT: a probe against
# an unresponsive gateway (arch under load) must fail fast, not block the suite on the loadgen's
# tail request timeout across every hung worker. tmo caps the whole invocation at dur+grace; if it
# fires, ugen printed nothing and the caller's `read`/awk see empty -> rps/fail default to 0/1
# (not-served).
sweep_probe(){ # url conc dur
  local _sb=()
  [ -n "${SWEEP_BODY:-}" ] && _sb=(-body "$SWEEP_BODY")
  tmo "$(probe_budget "$3")" "$UGEN" -url "$1" -model "$GW_MODEL" -auth "$GW_AUTH" -c "$2" -d "$3" -psize "$PSIZE" \
      ${_sb[@]+"${_sb[@]}"} ${UGEN_H[@]+"${UGEN_H[@]}"} 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++){split($i,a,"=");v[a[1]]=a[2]}; print v["rps"],v["fail"],v["p99us"],v["p50us"]}'
}

# c1 added latency: direct baseline (mock, same path/body) + gateway c1 -> overhead in microseconds.
# Discarded warm-up first so JIT/interpreted gateways (Node, Python) aren't charged first-request/
# cold-start cost inside the measured window. Identical for the direct baseline and the gateway -
# same for all. Then gates the window's HONESTY: sweep_probe only pools latencies from 200s, so
# GP99=0 means no successful sample; a material error rate means the gateway 429/5xx'd the window
# and any latency we DID pool is not a trustworthy proxy latency. C1_OK=0 marks the window
# unreliable with the evidence in C1_ERR; the caller decides what that demotes (perf: the whole
# lane; matrix: this cell's latency fields).
sweep_c1(){
  WARMUP_DUR="${WARMUP_DUR:-5}"
  log "[$GATEWAY] warm-up ${WARMUP_DUR}s (discarded, both paths)"
  sweep_probe "$DURL" 1 "$WARMUP_DUR" >/dev/null 2>&1; sweep_probe "$GURL" 1 "$WARMUP_DUR" >/dev/null 2>&1
  log "[$GATEWAY] c1 baseline (direct→mock) ${C1_DUR}s"
  read -r _drps _dfail DP99 DP50 < <(sweep_probe "$DURL" 1 "$C1_DUR")
  log "[$GATEWAY] c1 gateway ${C1_DUR}s"
  read -r _grps _gfail GP99 GP50 < <(sweep_probe "$GURL" 1 "$C1_DUR")
  C1_OK=1; C1_ERR=""
  local _gtot=$(( ${_grps:-0} * C1_DUR + ${_gfail:-0} )) _dtot=$(( ${_drps:-0} * C1_DUR + ${_dfail:-0} ))
  if [ "${GP99:-0}" -le 0 ] || [ "${DP99:-0}" -le 0 ] \
     || ! awk -v f="${_gfail:-1}" -v t="$_gtot" 'BEGIN{exit !(t>0 && f<=0.001*t)}' \
     || ! awk -v f="${_dfail:-1}" -v t="$_dtot" 'BEGIN{exit !(t>0 && f<=0.001*t)}'; then
    C1_OK=0
    C1_ERR="c1 latency window unreliable: gw ok=${_grps:-0}/s fail=${_gfail:-?} p99=${GP99:-0}us; direct ok=${_drps:-0}/s fail=${_dfail:-?} p99=${DP99:-0}us"
  fi
  OVER_P99=$(( ${GP99:-0} - ${DP99:-0} )); OVER_P50=$(( ${GP50:-0} - ${DP50:-0} ))
  log "[$GATEWAY] c1: gw p99=${GP99}µs direct p99=${DP99}µs → added p99=${OVER_P99}µs (p50 added=${OVER_P50}µs)"
}

# One sweep at a given mock delay + concurrency list. Restarts the mock at that delay, measures the
# mock's OWN ceiling (load→mock direct at the top concurrency) as the guardrail reference, then ramps
# the gateway. Sets SW_CEIL_RPS / SW_CEIL_CONC / SW_CEIL_P99 / SW_MOCK_CEIL / SW_BOUND / SW_JSON.
run_sweep() { # ttft_ms  conc_list
  local ttft="$1" concs="$2"
  pkill -f "$MOCK" 2>/dev/null; sleep 1
  setsid taskset -c "$MOCKCORES" env MOCK_TTFT_MS="$ttft" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
  sleep 1
  local top=1 w; for w in $concs; do top=$w; done
  local mrps _a _b _c; read -r mrps _a _b _c < <(sweep_probe "$DURL" "$top" "$SWEEP_DUR"); SW_MOCK_CEIL=${mrps:-0}
  SW_CEIL_RPS=0; SW_CEIL_CONC=0; SW_CEIL_P99=0; SW_JSON=""
  local conc rps fail p99 _p50
  for conc in $concs; do
    if suite_deadline_expired; then log "[$GATEWAY] suite wall-clock ceiling reached mid-sweep - stopping sweep, recording what we have"; break; fi
    read -r rps fail p99 _p50 < <(sweep_probe "$GURL" "$conc" "$SWEEP_DUR")
    rps=${rps:-0}; fail=${fail:-1}; p99=${p99:-99999999}
    log "[$GATEWAY]   (ttft=${ttft}ms) c=$conc → rps=$rps p99=$((p99/1000))ms fail=$fail"
    SW_JSON="${SW_JSON}${SW_JSON:+,}{\"conc\":$conc,\"rps\":$rps,\"p99_us\":$p99,\"fail\":$fail}"
    # Gate on error RATE (< 0.1%), not literal zero - a single failure in tens of thousands shouldn't
    # zero a gateway's whole result. Plus p99 under the ceiling, and a new best.
    if awk -v f="$fail" -v r="$rps" -v d="$SWEEP_DUR" 'BEGIN{tot=r*d+f; exit !(tot>0 && f<=0.001*tot)}' \
       && [ "$p99" -lt $((P99_CEIL_MS*1000)) ] && [ "$rps" -gt "$SW_CEIL_RPS" ]; then
      SW_CEIL_RPS=$rps; SW_CEIL_CONC=$conc; SW_CEIL_P99=$p99
    fi
  done
  SW_BOUND=false
  if [ "${SW_MOCK_CEIL:-0}" -gt 0 ] && awk -v c="$SW_CEIL_RPS" -v m="$SW_MOCK_CEIL" 'BEGIN{exit !(c>=0.9*m)}'; then SW_BOUND=true; fi
}
