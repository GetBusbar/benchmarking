#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# LATENCY + THROUGHPUT — pluggable across gateways (same gateways/<name>/gateway.sh manifests as
# the memory suite). On ONE box against ONE instant mock, per gateway it measures:
#   * added latency (µs) at concurrency 1 = gateway p99 − direct-to-mock p99, small payloads
#   * RPS ceiling = the highest sustained requests/sec where p99 < 1000 ms AND zero errors
# and writes results/perf/<gateway>.json (+ a concurrency sweep for the latency-vs-load chart).
#
#   GATEWAY=busbar BUSBAR_BIN=~/busbar perf/run.sh
#
# Knobs (env): C1_DUR (c1 latency run seconds, default 20), SWEEP ("1 8 16 32 64 128 256 512 1024"),
#   SWEEP_DUR (seconds per sweep point, default 10), PSIZE (payload bytes, default 256), CORES pin.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
MEM="$ROOT/memory"                              # reuse its mock.go + ugen.go
GATEWAY="${GATEWAY:-busbar}"
export GW_DIR="$ROOT/gateways/$GATEWAY"
[ -f "$GW_DIR/gateway.sh" ] || { echo "unknown gateway '$GATEWAY'"; exit 2; }

C1_DUR="${C1_DUR:-20}"; SWEEP_DUR="${SWEEP_DUR:-10}"; PSIZE="${PSIZE:-256}"
# Throughput sweep runs against a mock with a realistic per-request delay (SWEEP_TTFT_MS), so the
# ceiling is the gateway's concurrent-in-flight capacity (the real production bottleneck), not a race
# against an instant mock's CPU. Same delay for every gateway. Concurrency ramps high to find the
# ceiling of the fast (Rust) gateways; the slow (Python) ones collapse early.
# Two throughput sweeps per gateway (see below). Instant-mock = "max proxy throughput" (CPU-bound,
# busbar's own published metric); 20ms-mock = "sustained RPS under LLM latency" (AIGatewayBench's
# exact metric, concurrency-bound → ramp higher). Same for every gateway.
SWEEP_INSTANT="${SWEEP_INSTANT:-16 32 64 128 256 512 1024}"
SWEEP_DELAYED="${SWEEP_DELAYED:-256 1024 4096 8192 16384}"
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
# tail). Captured verbatim into the result when served=false, so "did not serve" is evidence, not an
# assertion a competitor can wave away. Default: nothing to add.
gw_diag(){ :; }
# json_escape: fold arbitrary log text into a one-line JSON string value (trimmed to keep results small).
json_escape(){ printf '%s' "$1" | tr -d '\000' | head -c 1600 \
  | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read())[1:-1])' 2>/dev/null \
  || printf '%s' "$1" | tr '\n\t"\\' '    ' | head -c 1600; }
# shellcheck source=/dev/null
source "$GW_DIR/gateway.sh"

log "starting mock :$MOCK_PORT (instant)"
pkill -f "$MOCK" 2>/dev/null; sleep 1
setsid taskset -c "$MOCKCORES" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
sleep 1
cleanup(){ gw_stop 2>/dev/null; pkill -f "$MOCK" 2>/dev/null; }
trap cleanup EXIT

# run ugen, echo "rps fail p99us" parsed from its output line
probe(){ # url conc dur
  "$UGEN" -url "$1" -model "$GW_MODEL" -auth "$GW_AUTH" -c "$2" -d "$3" -psize "$PSIZE" "${UGEN_H[@]}" 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++){split($i,a,"=");v[a[1]]=a[2]}; print v["rps"],v["fail"],v["p99us"],v["p50us"]}'
}

log "[$GATEWAY] build + launch"; gw_build || { echo "build failed"; exit 1; }; gw_launch
# Header arrays built AFTER launch so a manifest can mint a key in gw_launch (busbar vkey).
UGEN_H=(); CURL_H=()
for h in "${GW_HEADERS[@]:-}"; do [ -n "$h" ] && { UGEN_H+=(-H "$h"); CURL_H+=(-H "$h"); }; done
log "[$GATEWAY] wait 200 on $GW_PATH"; ok=0; c=000
for i in $(seq 1 60); do
  c=$(curl -s -m3 -o /dev/null -w "%{http_code}" "http://127.0.0.1:$GW_PORT$GW_PATH" -X POST \
      -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" "${CURL_H[@]}" \
      -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}")
  [ "$c" = 200 ] && { ok=1; break; }; sleep 1
done
SERVE_ERR=""
if [ "$ok" != 1 ]; then
  # Capture the response body of one more attempt + the gateway's own logs, so the failure is provable.
  body="$(curl -s -m3 "http://127.0.0.1:$GW_PORT$GW_PATH" -X POST \
      -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" "${CURL_H[@]}" \
      -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}" 2>&1 | head -c 400)"
  SERVE_ERR="HTTP $c on POST $GW_PATH; body=[$body]; diag=[$(gw_diag 2>&1 | tail -n 20)]"
  log "[$GATEWAY] WARNING never got 200 (last=$c) — served=false"
  log "[$GATEWAY] serve_error: $(printf '%s' "$SERVE_ERR" | head -c 300)"
fi

# ── direct baseline (mock, same path/body) + gateway c1 → overhead µs ──────────────────────────────
DURL="http://127.0.0.1:$MOCK_PORT$GW_PATH"; GURL="http://127.0.0.1:$GW_PORT$GW_PATH"
log "[$GATEWAY] c1 baseline (direct→mock) ${C1_DUR}s"
read -r _drps _dfail DP99 DP50 < <(probe "$DURL" 1 "$C1_DUR")
log "[$GATEWAY] c1 gateway ${C1_DUR}s"
read -r _grps _gfail GP99 GP50 < <(probe "$GURL" 1 "$C1_DUR")
OVER_P99=$(( ${GP99:-0} - ${DP99:-0} )); OVER_P50=$(( ${GP50:-0} - ${DP50:-0} ))
log "[$GATEWAY] c1: gw p99=${GP99}µs direct p99=${DP99}µs → added p99=${OVER_P99}µs (p50 added=${OVER_P50}µs)"

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
    if [ "$fail" -eq 0 ] && [ "$p99" -lt $((P99_CEIL_MS*1000)) ] && [ "$rps" -gt "$SW_CEIL_RPS" ]; then
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
