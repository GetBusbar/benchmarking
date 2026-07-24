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
#
# ACCELERATION KNOBS (all OFF by default: perf/run.sh sets none of them and its behaviour + result
# shapes are byte-identical to before these knobs existed). The matrix suite sets them because it
# runs this library up to 36 times per gateway and the per-cell cost is dominated by measurements
# that do not depend on the cell:
#   SWEEP_CACHE_KEY   when non-empty, the DIRECT-TO-MOCK measurements are cached under this key and
#                     reused by later calls carrying the same key: the c1 direct baseline (cached
#                     per key) and run_sweep's mock-ceiling reference (cached per key + ttft). Both
#                     measure the RIG - the mock + load generator on their own pinned cores, same
#                     path/body/psize, no gateway anywhere in the loop - so re-measuring them per
#                     cell re-measures a constant. The matrix keys by ingress dialect (which fixes
#                     path AND body). A baseline is only cached when its window was reliable
#                     (< 0.1% errors, nonzero p99); an unreliable window is re-measured next call.
#                     Every GATEWAY-side measurement still runs fresh on every call.
#   SWEEP_ADAPTIVE=1  adaptive rung selection in run_sweep. The measurement at every probed rung is
#                     byte-identical to the full ladder (same loadgen invocation, same window, same
#                     gates); only WHICH rungs are probed changes. The first sweep at a given ttft
#                     walks the FULL ladder and seeds SWEEP_PRIOR[ttft] with the winning
#                     concurrency; later sweeps probe the prior rung and its immediate neighbours,
#                     then keep expanding outward while the best gate-passing rung sits on the edge
#                     of the probed window - so the reported winner is always bracketed by two
#                     probed, strictly-worse rungs (or the grid boundary), exactly the evidence the
#                     full ladder provides for a unimodal profile. If NO probed rung passes the
#                     gate, it falls back to probing the full ladder. SW_JSON carries the probed
#                     rungs (ascending conc).

# Caches + priors backing the acceleration knobs. bash>=4 associative arrays; the `|| true` keeps
# this file sourceable on bash 3.2 rigs, where the knobs are simply never set (perf/run.sh default).
declare -gA SWEEP_C1_CACHE 2>/dev/null || true
declare -gA SWEEP_MOCKCEIL_CACHE 2>/dev/null || true
declare -gA SWEEP_PRIOR 2>/dev/null || true

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
  local _ck="${SWEEP_CACHE_KEY:-}" _dfresh=1
  if [ -n "$_ck" ] && [ -n "${SWEEP_C1_CACHE[$_ck]:-}" ]; then
    # Direct-baseline reuse: the cache key encodes the dialect (same mock endpoint, same body, same
    # psize, same pins), and the direct path never touches the gateway, so this window is the same
    # rig constant every time. The GATEWAY side below still gets its own warm-up + fresh window.
    read -r DP99 DP50 _drps _dfail <<<"${SWEEP_C1_CACHE[$_ck]}"
    _dfresh=0
    log "[$GATEWAY] warm-up ${WARMUP_DUR}s (discarded, gateway; direct baseline reused for '$_ck': p99=${DP99}µs)"
    sweep_probe "$GURL" 1 "$WARMUP_DUR" >/dev/null 2>&1
  else
    log "[$GATEWAY] warm-up ${WARMUP_DUR}s (discarded, both paths)"
    sweep_probe "$DURL" 1 "$WARMUP_DUR" >/dev/null 2>&1; sweep_probe "$GURL" 1 "$WARMUP_DUR" >/dev/null 2>&1
    log "[$GATEWAY] c1 baseline (direct→mock) ${C1_DUR}s"
    read -r _drps _dfail DP99 DP50 < <(sweep_probe "$DURL" 1 "$C1_DUR")
  fi
  log "[$GATEWAY] c1 gateway ${C1_DUR}s"
  read -r _grps _gfail GP99 GP50 < <(sweep_probe "$GURL" 1 "$C1_DUR")
  C1_OK=1; C1_ERR=""
  local _gtot=$(( ${_grps:-0} * C1_DUR + ${_gfail:-0} )) _dtot=$(( ${_drps:-0} * C1_DUR + ${_dfail:-0} ))
  local _dok=1
  { [ "${DP99:-0}" -gt 0 ] && awk -v f="${_dfail:-1}" -v t="$_dtot" 'BEGIN{exit !(t>0 && f<=0.001*t)}'; } || _dok=0
  if [ "${GP99:-0}" -le 0 ] || [ "$_dok" != 1 ] \
     || ! awk -v f="${_gfail:-1}" -v t="$_gtot" 'BEGIN{exit !(t>0 && f<=0.001*t)}'; then
    C1_OK=0
    C1_ERR="c1 latency window unreliable: gw ok=${_grps:-0}/s fail=${_gfail:-?} p99=${GP99:-0}us; direct ok=${_drps:-0}/s fail=${_dfail:-?} p99=${DP99:-0}us"
  fi
  # Cache the direct baseline only when it was measured FRESH this call and its window was reliable;
  # an unreliable direct window is never reused (the next call with this key re-measures it).
  if [ -n "$_ck" ] && [ "$_dfresh" = 1 ] && [ "$_dok" = 1 ]; then
    SWEEP_C1_CACHE[$_ck]="$DP99 $DP50 ${_drps:-0} ${_dfail:-0}"
  fi
  OVER_P99=$(( ${GP99:-0} - ${DP99:-0} )); OVER_P50=$(( ${GP50:-0} - ${DP50:-0} ))
  log "[$GATEWAY] c1: gw p99=${GP99}µs direct p99=${DP99}µs → added p99=${OVER_P99}µs (p50 added=${OVER_P50}µs)"
}

# One sweep at a given mock delay + concurrency list. Restarts the mock at that delay, measures the
# mock's OWN ceiling (load→mock direct at the top concurrency) as the guardrail reference, then ramps
# the gateway. Sets SW_CEIL_RPS / SW_CEIL_CONC / SW_CEIL_P99 / SW_MOCK_CEIL / SW_BOUND / SW_JSON.
# _sw_probe_rung <idx>: measure ladder rung <idx> once (idempotent) into the caller's _rc/_rr/_rf/_rp
# arrays (bash dynamic scope: run_sweep's locals). The measurement is EXACTLY the full ladder's:
# same sweep_probe, same window, same log line. Returns 1 only when the suite deadline fired.
_sw_probe_rung(){
  local i="$1"
  [ -n "${_rr[$i]:-}" ] && return 0
  if suite_deadline_expired; then log "[$GATEWAY] suite wall-clock ceiling reached mid-sweep - stopping sweep, recording what we have"; return 1; fi
  local rps fail p99 _p50
  read -r rps fail p99 _p50 < <(sweep_probe "$GURL" "${_rc[$i]}" "$SWEEP_DUR")
  _rr[$i]=${rps:-0}; _rf[$i]=${fail:-1}; _rp[$i]=${p99:-99999999}
  log "[$GATEWAY]   (ttft=${ttft}ms) c=${_rc[$i]} → rps=${_rr[$i]} p99=$(( ${_rp[$i]} / 1000 ))ms fail=${_rf[$i]}"
  return 0
}
# _sw_pass <idx>: the ONE sustained gate, identical to the original inline check - error RATE
# < 0.1% (not literal zero: a single failure in tens of thousands shouldn't zero a gateway's whole
# result) AND p99 under the ceiling.
_sw_pass(){
  local i="$1"
  awk -v f="${_rf[$i]}" -v r="${_rr[$i]}" -v d="$SWEEP_DUR" 'BEGIN{tot=r*d+f; exit !(tot>0 && f<=0.001*tot)}' \
    && [ "${_rp[$i]}" -lt $((P99_CEIL_MS*1000)) ]
}

run_sweep() { # ttft_ms  conc_list
  local ttft="$1" concs="$2"
  pkill -f "$MOCK" 2>/dev/null; sleep 1
  setsid taskset -c "$MOCKCORES" env MOCK_TTFT_MS="$ttft" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
  sleep 1
  local top=1 w; for w in $concs; do top=$w; done
  # Mock-ceiling reference: a property of the RIG (mock + loadgen on their own pinned cores, at this
  # ttft/path/body/psize), never of the gateway - so under a cache key it is measured once and reused.
  local _mk="${SWEEP_CACHE_KEY:-}"; [ -n "$_mk" ] && _mk="$_mk|$ttft"
  if [ -n "$_mk" ] && [ -n "${SWEEP_MOCKCEIL_CACHE[$_mk]:-}" ]; then
    SW_MOCK_CEIL="${SWEEP_MOCKCEIL_CACHE[$_mk]}"
  else
    local mrps _a _b _c; read -r mrps _a _b _c < <(sweep_probe "$DURL" "$top" "$SWEEP_DUR"); SW_MOCK_CEIL=${mrps:-0}
    [ -n "$_mk" ] && [ "${SW_MOCK_CEIL:-0}" -gt 0 ] && SWEEP_MOCKCEIL_CACHE[$_mk]="$SW_MOCK_CEIL"
  fi
  SW_CEIL_RPS=0; SW_CEIL_CONC=0; SW_CEIL_P99=0; SW_JSON=""
  # The ladder as indexed arrays; _rr[i] set = rung i probed (helpers see these via dynamic scope).
  local -a _rc=() _rr=() _rf=() _rp=()
  local _n=0 c i
  for c in $concs; do _rc[$_n]="$c"; _n=$((_n+1)); done
  local adaptive=0
  [ "${SWEEP_ADAPTIVE:-0}" = 1 ] && [ -n "${SWEEP_PRIOR[$ttft]:-}" ] && adaptive=1
  if [ "$adaptive" = 1 ]; then
    # ADAPTIVE RUNG SELECTION (see the header): probe the previous winner's rung + its neighbours,
    # expand outward while the best gate-passing rung sits on the probed window's edge. The winner
    # is therefore always bracketed by probed strictly-worse rungs (or the grid boundary) - the same
    # evidence the full ladder yields on a unimodal profile. No gate-passing rung probed => fall
    # back to the full ladder, so adaptive can never report a zero the ladder wouldn't.
    local prior="${SWEEP_PRIOR[$ttft]}" pi=0 lo hi besti ok=1
    for i in $(seq 0 $((_n-1))); do [ "${_rc[$i]}" -le "$prior" ] && pi=$i; done
    lo=$(( pi>0 ? pi-1 : 0 )); hi=$(( pi<_n-1 ? pi+1 : _n-1 ))
    for i in $(seq "$lo" "$hi"); do _sw_probe_rung "$i" || { ok=0; break; }; done
    while [ "$ok" = 1 ]; do
      besti=-1
      for i in $(seq "$lo" "$hi"); do
        [ -n "${_rr[$i]:-}" ] && _sw_pass "$i" || continue
        if [ "$besti" -lt 0 ] || [ "${_rr[$i]}" -gt "${_rr[$besti]}" ]; then besti=$i; fi
      done
      if [ "$besti" -lt 0 ]; then
        log "[$GATEWAY]   (ttft=${ttft}ms) no gate-passing rung in the adaptive window - probing the full ladder"
        for i in $(seq 0 $((_n-1))); do _sw_probe_rung "$i" || break; done
        break
      fi
      if [ "$besti" = "$lo" ] && [ "$lo" -gt 0 ]; then
        lo=$((lo-1)); _sw_probe_rung "$lo" || break
      elif [ "$besti" = "$hi" ] && [ "$hi" -lt $((_n-1)) ]; then
        hi=$((hi+1)); _sw_probe_rung "$hi" || break
      else
        break
      fi
    done
  else
    for i in $(seq 0 $((_n-1))); do _sw_probe_rung "$i" || break; done
  fi
  # Aggregate over every probed rung (ascending conc, so full-ladder SW_JSON and tie-breaks are
  # byte-identical to the original incremental loop).
  for i in $(seq 0 $((_n-1))); do
    [ -n "${_rr[$i]:-}" ] || continue
    SW_JSON="${SW_JSON}${SW_JSON:+,}{\"conc\":${_rc[$i]},\"rps\":${_rr[$i]},\"p99_us\":${_rp[$i]},\"fail\":${_rf[$i]}}"
    if _sw_pass "$i" && [ "${_rr[$i]}" -gt "$SW_CEIL_RPS" ]; then
      SW_CEIL_RPS=${_rr[$i]}; SW_CEIL_CONC=${_rc[$i]}; SW_CEIL_P99=${_rp[$i]}
    fi
  done
  # Seed/refresh the prior for the next sweep at this ttft (adaptive callers only).
  if [ "${SWEEP_ADAPTIVE:-0}" = 1 ] && [ "$SW_CEIL_RPS" -gt 0 ]; then SWEEP_PRIOR[$ttft]="$SW_CEIL_CONC"; fi
  SW_BOUND=false
  if [ "${SW_MOCK_CEIL:-0}" -gt 0 ] && awk -v c="$SW_CEIL_RPS" -v m="$SW_MOCK_CEIL" 'BEGIN{exit !(c>=0.9*m)}'; then SW_BOUND=true; fi
}
