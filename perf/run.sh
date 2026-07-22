#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# LATENCY + THROUGHPUT — pluggable across gateways (same gateways/<name>/gateway.sh manifests as
# the memory suite). On ONE box against ONE instant mock, per gateway it measures:
#   * added latency (µs) at concurrency 1 = gateway p99 − direct-to-mock p99, small payloads
#   * RPS ceiling = the highest sustained requests/sec where p99 < 1000 ms AND a <0.1% error rate
# and writes results/perf/<gateway>.json (+ a concurrency sweep for the latency-vs-load chart).
#
#   GATEWAY=busbar BUSBAR_BIN=~/busbar perf/run.sh
#
# Knobs (env): C1_DUR (c1 latency run seconds, default 20), SWEEP ("1 8 16 32 64 128 256 512 1024"),
#   SWEEP_DUR (seconds per sweep point, default 10), PSIZE (payload bytes, default 256), CORES pin.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
MEM="$ROOT/memory"                              # the memory suite's dir (shared knobs)
GATEWAY="${GATEWAY:-busbar}"
export GW_DIR="$ROOT/gateways/$GATEWAY"
[ -f "$GW_DIR/gateway.sh" ] || { echo "unknown gateway '$GATEWAY'"; exit 2; }

C1_DUR="${C1_DUR:-20}"; SWEEP_DUR="${SWEEP_DUR:-10}"; PSIZE="${PSIZE:-256}"
# Throughput sweep runs against a mock with a realistic per-request delay (SWEEP_TTFT_MS), so the
# ceiling is the gateway's concurrent-in-flight capacity (the real production bottleneck), not a race
# against an instant mock's CPU. Same delay for every gateway. Concurrency spans low→high so EVERY
# gateway — fast or slow — is offered a load it can pass; the sweep records where each holds p99 under
# the ceiling with a sub-0.1% error rate, and the ceiling is the best qualifying point.
# Two throughput sweeps per gateway (see below). Instant-mock = "max proxy throughput" (CPU-bound);
# 20ms-mock = "sustained RPS under LLM latency" (AIGatewayBench's metric, concurrency-bound → ramps
# higher). The delayed sweep starts low (8) so slow gateways get a concurrency they can hold. Same
# grid for every gateway.
SWEEP_INSTANT="${SWEEP_INSTANT:-16 32 64 128 256 512 1024}"
SWEEP_DELAYED="${SWEEP_DELAYED:-8 32 128 256 1024 4096 8192 16384}"
SWEEP_TTFT_MS="${SWEEP_TTFT_MS:-20}"
P99_CEIL_MS="${P99_CEIL_MS:-1000}"
# raise the fd limit so high-concurrency sweeps aren't capped by open sockets
ulimit -n 1048576 2>/dev/null || ulimit -n 65536 2>/dev/null || true
export CORES="${CORES:-0-3}"; LOADCORES="${LOADCORES:-0-3}"; MOCKCORES="${MOCKCORES:-0-3}"
export MOCK_PORT="${MOCK_PORT:-8000}"
RESULTS="$ROOT/results/perf"; mkdir -p "$RESULTS"
log(){ echo "[$(date +%H:%M:%S)] $*"; }
command -v taskset >/dev/null || taskset(){ shift 2; "$@"; }
command -v go >/dev/null || { echo "need Go (load generator)"; exit 1; }
command -v cargo >/dev/null || { echo "need cargo (rust mock)"; exit 1; }

log "building mock (rust) + loadgen (go)"
( cd "$ROOT/mock" && cargo build --release >/dev/null 2>&1 ) || { echo "mock build failed"; exit 1; }
MOCK="$ROOT/mock/target/release/mock"
go build -o "$ROOT/loadgen/ugen" "$ROOT/loadgen/ugen.go"
UGEN="$ROOT/loadgen/ugen"

[ -f "$ROOT/gateways/versions.env" ] && source "$ROOT/gateways/versions.env"
gw_version(){ echo unknown; }; GW_HEADERS=()
# gw_diag: a manifest MAY override this to print WHY it failed to serve (docker logs / native log
# tail). Captured verbatim into the result when served=false, so a "did not serve" row carries the
# captured status + logs as evidence rather than a bare assertion. Default: nothing to add.
gw_diag(){ :; }
# json_escape: fold arbitrary log text into a one-line JSON string value (trimmed to keep results small).
json_escape(){ printf '%s' "$1" | python3 -c 'import json,sys
d=sys.stdin.buffer.read()[:1600].decode("utf-8","replace")
sys.stdout.write(json.dumps(d)[1:-1])'; }
# shellcheck source=/dev/null
source "$ROOT/lib/harness.sh"
# shellcheck source=/dev/null
source "$GW_DIR/gateway.sh"
suite_deadline_start

log "starting mock :$MOCK_PORT (instant)"
pkill -f "$MOCK" 2>/dev/null; sleep 1
setsid taskset -c "$MOCKCORES" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
sleep 1
cleanup(){ gw_stop 2>/dev/null; pkill -f "$MOCK" 2>/dev/null; }
trap cleanup EXIT

# run ugen, echo "rps fail p99us" parsed from its output line. HARD TIMEOUT: a probe against an
# unresponsive gateway (arch under load) must fail fast, not block the suite on the loadgen's tail
# request timeout across every hung worker. tmo caps the whole invocation at dur+grace; if it fires,
# ugen printed nothing and the caller's `read`/awk see empty → rps/fail default to 0/1 (not-served).
probe(){ # url conc dur
  tmo "$(probe_budget "$3")" "$UGEN" -url "$1" -model "$GW_MODEL" -auth "$GW_AUTH" -c "$2" -d "$3" -psize "$PSIZE" "${UGEN_H[@]}" 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++){split($i,a,"=");v[a[1]]=a[2]}; print v["rps"],v["fail"],v["p99us"],v["p50us"]}'
}

log "[$GATEWAY] build"; gw_build || { echo "build failed"; exit 1; }
# Readiness probe used by the retry loop. Rebuilds the header arrays FIRST (a manifest can mint a key
# in gw_launch, e.g. busbar's vkey), then polls for a 200 with a per-request hard timeout (-m3). It
# does its OWN bounded wait; harness_launch_ready re-runs gw_launch + this probe up to N times.
UGEN_H=(); CURL_H=(); ok=0; c=000
_perf_ready(){
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
if harness_launch_ready gw_launch _perf_ready; then
  ok=1
else
  # Honest not-served after N failed boots: capture one more body + the retry diagnostics as evidence.
  body="$(curl -s -m3 "http://127.0.0.1:$GW_PORT$GW_PATH" -X POST \
      -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" "${CURL_H[@]}" \
      -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}" 2>&1 | head -c 400)"
  SERVE_ERR="$HARNESS_SERVE_ERR; last body=[$body]"
  log "[$GATEWAY] serve_error: $(printf '%s' "$SERVE_ERR" | head -c 300)"
fi

# ── direct baseline (mock, same path/body) + gateway c1 → overhead µs ──────────────────────────────
DURL="http://127.0.0.1:$MOCK_PORT$GW_PATH"; GURL="http://127.0.0.1:$GW_PORT$GW_PATH"
# Discarded warm-up so JIT/interpreted gateways (Node, Python) aren't charged first-request/cold-start
# cost inside the measured window. Identical for the direct baseline and the gateway — same for all.
WARMUP_DUR="${WARMUP_DUR:-5}"
log "[$GATEWAY] warm-up ${WARMUP_DUR}s (discarded, both paths)"
probe "$DURL" 1 "$WARMUP_DUR" >/dev/null 2>&1; probe "$GURL" 1 "$WARMUP_DUR" >/dev/null 2>&1
log "[$GATEWAY] c1 baseline (direct→mock) ${C1_DUR}s"
read -r _drps _dfail DP99 DP50 < <(probe "$DURL" 1 "$C1_DUR")
log "[$GATEWAY] c1 gateway ${C1_DUR}s"
read -r _grps _gfail GP99 GP50 < <(probe "$GURL" 1 "$C1_DUR")
# Gate the added-latency lane on the c1 window's HONESTY. probe() only pools latencies from 200s now,
# so GP99=0 means no successful sample; a material error rate means the gateway 429/5xx'd the window
# and any latency we DID pool is not a trustworthy proxy latency. In either case the added-latency
# number is fabricated - demote the whole lane to served=false with a provable reason (same convention
# as the never-got-200 path) instead of publishing a 0 or an error-path win. Baseline (direct→mock)
# must also produce a real sample, else OVER_P99 would be the gateway's full absolute latency.
if [ "$ok" = 1 ]; then
  _gtot=$(( ${_grps:-0} * C1_DUR + ${_gfail:-0} )); _dtot=$(( ${_drps:-0} * C1_DUR + ${_dfail:-0} ))
  if [ "${GP99:-0}" -le 0 ] || [ "${DP99:-0}" -le 0 ] \
     || ! awk -v f="${_gfail:-1}" -v t="$_gtot" 'BEGIN{exit !(t>0 && f<=0.001*t)}' \
     || ! awk -v f="${_dfail:-1}" -v t="$_dtot" 'BEGIN{exit !(t>0 && f<=0.001*t)}'; then
    ok=0; c="c1-err"
    SERVE_ERR="c1 latency window unreliable: gw ok=${_grps:-0}/s fail=${_gfail:-?} p99=${GP99:-0}us; direct ok=${_drps:-0}/s fail=${_dfail:-?} p99=${DP99:-0}us; diag=[$(gw_diag 2>&1 | tail -n 20)]"
    log "[$GATEWAY] WARNING c1 window had errors / no valid sample - served=false"
    log "[$GATEWAY] serve_error: $(printf '%s' "$SERVE_ERR" | head -c 300)"
  fi
fi
OVER_P99=$(( ${GP99:-0} - ${DP99:-0} )); OVER_P50=$(( ${GP50:-0} - ${DP50:-0} ))
log "[$GATEWAY] c1: gw p99=${GP99}µs direct p99=${DP99}µs → added p99=${OVER_P99}µs (p50 added=${OVER_P50}µs)"

# ── the gateway's OWN self-reported compute (Server-Timing dur), same box + same c1 condition ────────
# Neutral: we record whatever the gateway emits in `Server-Timing: <name>;dur=`. Only a gateway that
# self-reports produces a value (today only busbar, via `busbar;dur`); every other gateway → null. This
# lets the end-to-end added latency decompose on ONE run: self_reported_dur ⊂ added_latency (the rest is
# the extra network hop). Skipped if the gateway never served.
STDUR_P50=null; STDUR_P99=null; STDUR_N=0
if [ "$ok" = 1 ]; then
  ST_JSON=$(python3 "$HERE/stdur.py" "$GURL" "${STDUR_SAMPLES:-3000}" "$GW_MODEL" "$GW_AUTH" "${GW_HEADERS[@]:-}" 2>/dev/null || echo '{"n":0}')
  STDUR_N=$(printf '%s' "$ST_JSON" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("n",0))' 2>/dev/null || echo 0)
  if [ "${STDUR_N:-0}" -gt 0 ]; then
    STDUR_P50=$(printf '%s' "$ST_JSON" | python3 -c 'import sys,json;print(json.load(sys.stdin)["p50_us"])' 2>/dev/null || echo null)
    STDUR_P99=$(printf '%s' "$ST_JSON" | python3 -c 'import sys,json;print(json.load(sys.stdin)["p99_us"])' 2>/dev/null || echo null)
    log "[$GATEWAY] self-reported Server-Timing dur: p50=${STDUR_P50}µs p99=${STDUR_P99}µs (n=${STDUR_N})"
  fi
fi

# ── two throughput sweeps ──────────────────────────────────────────────────────────────────────────
# One sweep at a given mock delay + concurrency list. Restarts the mock at that delay, measures the
# mock's OWN ceiling (load→mock direct at the top concurrency) as the guardrail reference, then ramps
# the gateway. Sets SW_CEIL_RPS / SW_CEIL_CONC / SW_CEIL_P99 / SW_MOCK_CEIL / SW_BOUND / SW_JSON.
run_sweep() { # ttft_ms  conc_list
  local ttft="$1" concs="$2"
  pkill -f "$MOCK" 2>/dev/null; sleep 1
  setsid taskset -c "$MOCKCORES" env MOCK_TTFT_MS="$ttft" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
  sleep 1
  local top=1 w; for w in $concs; do top=$w; done
  local mrps _a _b _c; read -r mrps _a _b _c < <(probe "$DURL" "$top" "$SWEEP_DUR"); SW_MOCK_CEIL=${mrps:-0}
  SW_CEIL_RPS=0; SW_CEIL_CONC=0; SW_CEIL_P99=0; SW_JSON=""
  local conc rps fail p99 _p50
  for conc in $concs; do
    read -r rps fail p99 _p50 < <(probe "$GURL" "$conc" "$SWEEP_DUR")
    rps=${rps:-0}; fail=${fail:-1}; p99=${p99:-99999999}
    log "[$GATEWAY]   (ttft=${ttft}ms) c=$conc → rps=$rps p99=$((p99/1000))ms fail=$fail"
    SW_JSON="${SW_JSON}${SW_JSON:+,}{\"conc\":$conc,\"rps\":$rps,\"p99_us\":$p99,\"fail\":$fail}"
    # Gate on error RATE (< 0.1%), not literal zero — a single failure in tens of thousands shouldn't
    # zero a gateway's whole result. Plus p99 under the ceiling, and a new best.
    if awk -v f="$fail" -v r="$rps" -v d="$SWEEP_DUR" 'BEGIN{tot=r*d+f; exit !(tot>0 && f<=0.001*tot)}' \
       && [ "$p99" -lt $((P99_CEIL_MS*1000)) ] && [ "$rps" -gt "$SW_CEIL_RPS" ]; then
      SW_CEIL_RPS=$rps; SW_CEIL_CONC=$conc; SW_CEIL_P99=$p99
    fi
  done
  SW_BOUND=false
  if [ "${SW_MOCK_CEIL:-0}" -gt 0 ] && awk -v c="$SW_CEIL_RPS" -v m="$SW_MOCK_CEIL" 'BEGIN{exit !(c>=0.9*m)}'; then SW_BOUND=true; fi
}

# (A) MAX PROXY THROUGHPUT — instant mock. Raw forward speed; busbar's own published metric.
log "[$GATEWAY] sweep A — max proxy throughput (instant mock)"
run_sweep 0 "$SWEEP_INSTANT"
PROXY_RPS=$SW_CEIL_RPS; PROXY_CONC=$SW_CEIL_CONC; PROXY_MOCK=$SW_MOCK_CEIL; PROXY_BOUND=$SW_BOUND; PROXY_JSON="$SW_JSON"
[ "$PROXY_BOUND" = true ] && log "[$GATEWAY] ⚠ max-proxy ceiling ($PROXY_RPS) within 10% of mock ($PROXY_MOCK) — MOCK-BOUND floor"
log "[$GATEWAY] max proxy throughput = $PROXY_RPS rps @ c=$PROXY_CONC"

# (B) SUSTAINED RPS @ ${SWEEP_TTFT_MS}ms — AIGatewayBench's exact metric: concurrent in-flight capacity
# under realistic LLM latency. Concurrency ramps high; the delayed mock mostly sleeps so it isn't the
# limit until very high RPS (flagged if so).
log "[$GATEWAY] sweep B — sustained RPS @ ${SWEEP_TTFT_MS}ms LLM latency (AIGatewayBench metric)"
run_sweep "$SWEEP_TTFT_MS" "$SWEEP_DELAYED"
LLM_RPS=$SW_CEIL_RPS; LLM_CONC=$SW_CEIL_CONC; LLM_MOCK=$SW_MOCK_CEIL; LLM_BOUND=$SW_BOUND; LLM_JSON="$SW_JSON"
[ "$LLM_BOUND" = true ] && log "[$GATEWAY] ⚠ @${SWEEP_TTFT_MS}ms ceiling ($LLM_RPS) within 10% of mock ($LLM_MOCK) — MOCK-BOUND floor"
log "[$GATEWAY] sustained RPS @${SWEEP_TTFT_MS}ms = $LLM_RPS rps @ c=$LLM_CONC"

BUILD="$(gw_version 2>/dev/null | tr -d '\n' | sed 's/"/\\"/g')"
cat > "$RESULTS/$GATEWAY.json" <<JSON
{
  "gateway": "$GATEWAY",
  "build": "$BUILD",
  "served": $([ "$ok" = 1 ] && echo true || echo false),
  "last_http_status": "$c",
  "serve_error": "$(json_escape "$SERVE_ERR")",
  "added_latency_p50_us": $OVER_P50,
  "added_latency_p99_us": $OVER_P99,
  "gateway_c1_p99_us": ${GP99:-0},
  "direct_c1_p99_us": ${DP99:-0},
  "server_timing_dur_p50_us": ${STDUR_P50:-null},
  "server_timing_dur_p99_us": ${STDUR_P99:-null},
  "server_timing_dur_n": ${STDUR_N:-0},
  "rps_max_proxy": $PROXY_RPS,
  "rps_max_proxy_concurrency": $PROXY_CONC,
  "rps_max_proxy_mock_ceiling": $PROXY_MOCK,
  "rps_max_proxy_mock_bound": $PROXY_BOUND,
  "rps_sustained_20ms": $LLM_RPS,
  "rps_sustained_20ms_concurrency": $LLM_CONC,
  "rps_sustained_20ms_mock_ceiling": $LLM_MOCK,
  "rps_sustained_20ms_mock_bound": $LLM_BOUND,
  "sweep_ttft_ms": $SWEEP_TTFT_MS,
  "p99_ceiling_ms": $P99_CEIL_MS,
  "sweep_max_proxy": [$PROXY_JSON],
  "sweep_sustained_20ms": [$LLM_JSON],
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
echo " gateway=$GATEWAY   added latency p99=${OVER_P99}µs"
echo "   max proxy throughput = ${PROXY_RPS} rps @ c=${PROXY_CONC}  (mock_bound=${PROXY_BOUND})"
echo "   sustained @ ${SWEEP_TTFT_MS}ms      = ${LLM_RPS} rps @ c=${LLM_CONC}  (mock_bound=${LLM_BOUND})"
echo " -> $RESULTS/$GATEWAY.json"
echo "================================================================"
