#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# GOVERNED lane — the same c1 added-latency + sustained-RPS-@20ms measurements as perf/, but with
# the gateway's native key/limit governance ACTIVE: per-request virtual-key resolution, rate-limit
# accounting, and budget check-and-charge on the hot path. Each run also repeats the identical
# sweep against the PLAIN (ungoverned) launch, so results/governed/<gateway>.json self-contains
# the governance overhead (governed_vs_plain_sustained_pct etc.) from ONE box on ONE day — no
# cross-run subtraction.
#
#   GATEWAY=busbar BUSBAR_BIN=~/busbar governed/run.sh
#
# A gateway opts in by defining TWO OPTIONAL manifest hooks in gateways/<name>/gateway.sh:
#   gw_governed_launch  — launch with governance active AND provision a caller credential for the
#                         run (e.g. busbar mints a virtual key over its admin API). The minted
#                         limits must be generous enough that no cap ever trips at benchmark rates:
#                         we measure the cost of the CHECK, not the limit.
#   gw_governed_token   — echo that caller credential (used as the bearer token by the load gen).
# A manifest without the hooks gets a valid results/governed/<gateway>.json with
# governed_served=false and a note — never a crash, never a fabricated number.
#
# Knobs (env): C1_DUR (c1 latency run seconds, default 20), SWEEP_DELAYED, SWEEP_DUR (seconds per
#   sweep point, default 10), SWEEP_TTFT_MS (default 20), PSIZE (payload bytes, default 256), CORES.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
GATEWAY="${GATEWAY:-busbar}"
export GW_DIR="$ROOT/gateways/$GATEWAY"
[ -f "$GW_DIR/gateway.sh" ] || { echo "unknown gateway '$GATEWAY'"; exit 2; }

C1_DUR="${C1_DUR:-20}"; SWEEP_DUR="${SWEEP_DUR:-10}"; PSIZE="${PSIZE:-256}"
# Same delayed-mock grid + gates as perf/run.sh sweep B (sustained RPS @ 20ms) — identical load,
# so the governed and plain numbers differ ONLY by what the gateway does per request.
SWEEP_DELAYED="${SWEEP_DELAYED:-8 32 128 256 1024 4096 8192 16384}"
SWEEP_TTFT_MS="${SWEEP_TTFT_MS:-20}"
P99_CEIL_MS="${P99_CEIL_MS:-1000}"
ulimit -n 1048576 2>/dev/null || ulimit -n 65536 2>/dev/null || true
export CORES="${CORES:-0-3}"; LOADCORES="${LOADCORES:-0-3}"; MOCKCORES="${MOCKCORES:-0-3}"
export MOCK_PORT="${MOCK_PORT:-8000}"
RESULTS="$ROOT/results/governed"; mkdir -p "$RESULTS"
log(){ echo "[$(date +%H:%M:%S)] $*"; }
command -v taskset >/dev/null || taskset(){ shift 2; "$@"; }
command -v setsid  >/dev/null || setsid(){ "$@"; }
command -v go >/dev/null || { echo "need Go (load generator)"; exit 1; }
command -v cargo >/dev/null || { echo "need cargo (rust mock)"; exit 1; }

log "building mock (rust) + loadgen (go)"
( cd "$ROOT/mock" && cargo build --release >/dev/null 2>&1 ) || { echo "mock build failed"; exit 1; }
MOCK="$ROOT/mock/target/release/mock"
go build -o "$ROOT/loadgen/ugen" "$ROOT/loadgen/ugen.go"
UGEN="$ROOT/loadgen/ugen"

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
BUILD_STR(){ gw_version 2>/dev/null | tr -d '\n' | sed 's/"/\\"/g'; }

# write_unserved <note> — valid JSON + exit 0 for a gateway with no governed mode (or one that
# failed to serve), so run-all keeps rolling and the result file states WHY.
write_unserved(){
  cat > "$RESULTS/$GATEWAY.json" <<JSON
{
  "gateway": "$GATEWAY",
  "build": "$(BUILD_STR)",
  "governed_served": false,
  "governed_note": "$(json_escape "$1")",
  "measured_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON
  log "[$GATEWAY] governed_served=false — $1"
  log "-> $RESULTS/$GATEWAY.json"
}

if ! declare -F gw_governed_launch >/dev/null || ! declare -F gw_governed_token >/dev/null; then
  write_unserved "manifest defines no gw_governed_launch/gw_governed_token (no native key/limit governance, or not yet wired)"
  exit 0
fi

DURL="http://127.0.0.1:$MOCK_PORT$GW_PATH"; GURL="http://127.0.0.1:$GW_PORT$GW_PATH"
cleanup(){ gw_stop 2>/dev/null; pkill -f "$MOCK" 2>/dev/null; }
trap cleanup EXIT

# run ugen with an explicit bearer token, echo "rps fail p99us p50us". Hard-timeout wrapped: a
# gateway that stops responding mid-window fails fast at dur+grace instead of blocking the phase.
probe(){ # url conc dur token
  tmo "$(probe_budget "$3")" "$UGEN" -url "$1" -model "$GW_MODEL" -auth "$4" -c "$2" -d "$3" -psize "$PSIZE" ${UGEN_H[@]+"${UGEN_H[@]}"} 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++){split($i,a,"=");v[a[1]]=a[2]}; print v["rps"],v["fail"],v["p99us"],v["p50us"]}'
}

start_mock(){ # ttft_ms
  pkill -f "$MOCK" 2>/dev/null; sleep 1
  setsid taskset -c "$MOCKCORES" env MOCK_TTFT_MS="$1" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
  sleep 1
}

wait_200(){ # token → 0 if the gateway answers 200 within 60s (sets W_CODE)
  local i; W_CODE=000
  for i in $(seq 1 60); do
    W_CODE=$(curl -s -m3 -o /dev/null -w "%{http_code}" "$GURL" -X POST \
        -H "content-type: application/json" -H "authorization: Bearer $1" ${CURL_H[@]+"${CURL_H[@]}"} \
        -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}")
    [ "$W_CODE" = 200 ] && return 0; sleep 1
  done
  return 1
}

# measure_phase <token> — c1 added latency (instant mock) + sustained sweep @ SWEEP_TTFT_MS.
# Sets PH_OVER_P50 PH_OVER_P99 PH_GP99 PH_DP99 PH_RPS PH_CONC PH_P99 PH_MOCK PH_BOUND PH_JSON.
measure_phase(){
  local token="$1"
  start_mock 0
  log "[$GATEWAY] warm-up ${WARMUP_DUR:-5}s (discarded, both paths)"
  probe "$DURL" 1 "${WARMUP_DUR:-5}" "$token" >/dev/null 2>&1
  probe "$GURL" 1 "${WARMUP_DUR:-5}" "$token" >/dev/null 2>&1
  # Reset every output BEFORE the reads: `read` on empty probe output (gateway died mid-c1) leaves the
  # variables at their PREVIOUS values, and measure_phase runs twice (plain, then governed) - without
  # this reset the governed phase would silently copy the plain phase's numbers and report ~0 overhead.
  local drps dfail grps gfail _dp50=0 _gp50=0
  PH_DP99=0; PH_GP99=0; PH_OVER_P99=0; PH_OVER_P50=0; PH_C1_OK=1
  log "[$GATEWAY] c1 baseline (direct->mock) ${C1_DUR}s"
  read -r drps dfail PH_DP99 _dp50 < <(probe "$DURL" 1 "$C1_DUR" "$token")
  log "[$GATEWAY] c1 gateway ${C1_DUR}s"
  read -r grps gfail PH_GP99 _gp50 < <(probe "$GURL" 1 "$C1_DUR" "$token")
  # Gate on c1 honesty: probe() only pools 200 latencies, so PH_GP99=0 means no successful sample and a
  # material error rate means the window was 429/5xx'd (e.g. a minted key that stopped working) - the
  # added-latency number would be fabricated. Flag it so the caller records governed_served=false.
  local _gtot=$(( ${grps:-0} * C1_DUR + ${gfail:-0} )) _dtot=$(( ${drps:-0} * C1_DUR + ${dfail:-0} ))
  if [ "${PH_GP99:-0}" -le 0 ] || [ "${PH_DP99:-0}" -le 0 ] \
     || ! awk -v f="${gfail:-1}" -v t="$_gtot" 'BEGIN{exit !(t>0 && f<=0.001*t)}' \
     || ! awk -v f="${dfail:-1}" -v t="$_dtot" 'BEGIN{exit !(t>0 && f<=0.001*t)}'; then
    PH_C1_OK=0
    PH_C1_ERR="c1 latency window unreliable: gw ok=${grps:-0}/s fail=${gfail:-?} p99=${PH_GP99:-0}us; direct ok=${drps:-0}/s fail=${dfail:-?} p99=${PH_DP99:-0}us"
    PH_DP99=0; PH_GP99=0; _dp50=0; _gp50=0
    log "[$GATEWAY] WARNING c1 window had errors / no valid sample"
  fi
  PH_OVER_P99=$(( ${PH_GP99:-0} - ${PH_DP99:-0} )); PH_OVER_P50=$(( ${_gp50:-0} - ${_dp50:-0} ))
  log "[$GATEWAY] c1: gw p99=${PH_GP99}us direct p99=${PH_DP99}us -> added p99=${PH_OVER_P99}us"

  log "[$GATEWAY] sustained sweep @ ${SWEEP_TTFT_MS}ms"
  start_mock "$SWEEP_TTFT_MS"
  local top=1 w; for w in $SWEEP_DELAYED; do top=$w; done
  local mrps _a _b _c; read -r mrps _a _b _c < <(probe "$DURL" "$top" "$SWEEP_DUR" "$token"); PH_MOCK=${mrps:-0}
  PH_RPS=0; PH_CONC=0; PH_P99=0; PH_JSON=""
  local conc rps fail p99 _p50
  for conc in $SWEEP_DELAYED; do
    read -r rps fail p99 _p50 < <(probe "$GURL" "$conc" "$SWEEP_DUR" "$token")
    rps=${rps:-0}; fail=${fail:-1}; p99=${p99:-99999999}
    log "[$GATEWAY]   c=$conc -> rps=$rps p99=$((p99/1000))ms fail=$fail"
    PH_JSON="${PH_JSON}${PH_JSON:+,}{\"conc\":$conc,\"rps\":$rps,\"p99_us\":$p99,\"fail\":$fail}"
    if awk -v f="$fail" -v r="$rps" -v d="$SWEEP_DUR" 'BEGIN{tot=r*d+f; exit !(tot>0 && f<=0.001*tot)}' \
       && [ "$p99" -lt $((P99_CEIL_MS*1000)) ] && [ "$rps" -gt "$PH_RPS" ]; then
      PH_RPS=$rps; PH_CONC=$conc; PH_P99=$p99
    fi
  done
  PH_BOUND=false
  if [ "${PH_MOCK:-0}" -gt 0 ] && awk -v c="$PH_RPS" -v m="$PH_MOCK" 'BEGIN{exit !(c>=0.9*m)}'; then PH_BOUND=true; fi
  log "[$GATEWAY] sustained @${SWEEP_TTFT_MS}ms = $PH_RPS rps @ c=$PH_CONC (mock_bound=$PH_BOUND)"
}

log "[$GATEWAY] build"; gw_build || { echo "build failed"; exit 1; }

# ── phase 1: PLAIN (ungoverned) reference — the exact perf/ launch, same box, same minute ─────────
# Readiness rebuilds the headers (a manifest can mint a key in gw_launch), then waits for 200 with the
# plain token. harness_launch_ready re-runs gw_launch + this probe up to N attempts before giving up.
_plain_ready(){
  UGEN_H=(); CURL_H=()
  for h in "${GW_HEADERS[@]:-}"; do [ -n "$h" ] && { UGEN_H+=(-H "$h"); CURL_H+=(-H "$h"); }; done
  wait_200 "$GW_AUTH"
}
log "[$GATEWAY] phase 1/2 — PLAIN launch (ungoverned reference; robust boot, up to $HARNESS_BOOT_ATTEMPTS attempts)"
start_mock 0
if ! harness_launch_ready gw_launch _plain_ready; then
  write_unserved "plain (ungoverned) $HARNESS_SERVE_ERR (last=$W_CODE)"
  exit 0
fi
measure_phase "$GW_AUTH"
if [ "${PH_C1_OK:-1}" != 1 ]; then
  write_unserved "plain (ungoverned) c1 latency window unreliable: ${PH_C1_ERR:-}; diag=[$(gw_diag 2>&1 | tail -n 20)]"
  exit 0
fi
PL_OVER_P50=$PH_OVER_P50; PL_OVER_P99=$PH_OVER_P99; PL_GP99=$PH_GP99; PL_DP99=$PH_DP99
PL_RPS=$PH_RPS; PL_CONC=$PH_CONC; PL_MOCK=$PH_MOCK; PL_BOUND=$PH_BOUND; PL_JSON="$PH_JSON"
gw_stop; sleep 1
# The governed launch binds the same ports; a lingering plain-phase process makes the new bind fail
# ("address already in use") and the mint hit the OLD token-less admin plane (401). Wait until the
# data port is actually free before phase 2, killing again if the first stop did not land.
for _ in $(seq 1 20); do
  if ! (exec 3<>/dev/tcp/127.0.0.1/${GW_PORT} ) 2>/dev/null; then break; fi
  exec 3>&- 2>/dev/null || true
  gw_stop 2>/dev/null; pkill -x busbar 2>/dev/null; sleep 1
done

# ── phase 2: GOVERNED launch — governance active, caller is a minted/provisioned key ──────────────
# Fresh ports for phase 2: even with the plain process dead, its listener can sit in TIME_WAIT for
# up to a minute (busbar binds without SO_REUSEADDR), so re-binding the same port fails with
# "address already in use" while a connect probe reports it free. Shifting +2 sidesteps the state
# entirely (admin plane derives +1 from GW_PORT inside gw_governed_launch).
GW_PORT=$(( GW_PORT + 2 ))
GURL="http://127.0.0.1:$GW_PORT$GW_PATH"
log "[$GATEWAY] phase 2/2 — GOVERNED launch (virtual-key resolution + rate + budget on the hot path; robust boot, up to $HARNESS_BOOT_ATTEMPTS attempts)"
start_mock 0
# Readiness for the governed phase: the caller credential is minted INSIDE gw_governed_launch, so the
# probe reads it fresh via gw_governed_token each attempt (a re-run of gw_governed_launch mints a new
# key), rebuilds headers, then waits for 200 with that minted key. A launch that fails to mint (empty
# token) or never answers 200 is a failed attempt → harness_launch_ready retries the whole launch.
GOV_TOKEN=""
_gov_ready(){
  GOV_TOKEN="$(gw_governed_token)"
  [ -n "$GOV_TOKEN" ] || { log "[$GATEWAY] governed: gw_governed_token produced no credential"; return 1; }
  UGEN_H=(); CURL_H=()
  for h in "${GW_HEADERS[@]:-}"; do [ -n "$h" ] && { UGEN_H+=(-H "$h"); CURL_H+=(-H "$h"); }; done
  wait_200 "$GOV_TOKEN"
}
if ! harness_launch_ready gw_governed_launch _gov_ready; then
  write_unserved "governed $HARNESS_SERVE_ERR (last=${W_CODE:-n/a})"
  exit 0
fi
measure_phase "$GOV_TOKEN"
if [ "${PH_C1_OK:-1}" != 1 ]; then
  write_unserved "governed c1 latency window unreliable (the minted key served warm-up but errored the window): ${PH_C1_ERR:-}; diag=[$(gw_diag 2>&1 | tail -n 20)]"
  exit 0
fi
GV_OVER_P50=$PH_OVER_P50; GV_OVER_P99=$PH_OVER_P99; GV_GP99=$PH_GP99; GV_DP99=$PH_DP99
GV_RPS=$PH_RPS; GV_CONC=$PH_CONC; GV_MOCK=$PH_MOCK; GV_BOUND=$PH_BOUND; GV_JSON="$PH_JSON"

# governance overhead, self-contained: negative pct = governed slower than plain.
GV_VS_PL_PCT=$(awk -v g="$GV_RPS" -v p="$PL_RPS" 'BEGIN{ if(p>0) printf "%.2f",(g-p)*100.0/p; else print "null" }')
GV_ADDED_DELTA_P99=$(( GV_OVER_P99 - PL_OVER_P99 ))
GV_ADDED_DELTA_P50=$(( GV_OVER_P50 - PL_OVER_P50 ))

cat > "$RESULTS/$GATEWAY.json" <<JSON
{
  "gateway": "$GATEWAY",
  "build": "$(BUILD_STR)",
  "governed_served": true,
  "governed_note": "",
  "governed_added_latency_p50_us": $GV_OVER_P50,
  "governed_added_latency_p99_us": $GV_OVER_P99,
  "governed_gateway_c1_p99_us": ${GV_GP99:-0},
  "governed_direct_c1_p99_us": ${GV_DP99:-0},
  "governed_rps_sustained_20ms": $GV_RPS,
  "governed_rps_sustained_20ms_concurrency": $GV_CONC,
  "governed_rps_sustained_20ms_mock_ceiling": $GV_MOCK,
  "governed_rps_sustained_20ms_mock_bound": $GV_BOUND,
  "governed_sweep_sustained_20ms": [$GV_JSON],
  "plain_added_latency_p50_us": $PL_OVER_P50,
  "plain_added_latency_p99_us": $PL_OVER_P99,
  "plain_rps_sustained_20ms": $PL_RPS,
  "plain_rps_sustained_20ms_concurrency": $PL_CONC,
  "plain_rps_sustained_20ms_mock_ceiling": $PL_MOCK,
  "plain_rps_sustained_20ms_mock_bound": $PL_BOUND,
  "plain_sweep_sustained_20ms": [$PL_JSON],
  "governed_vs_plain_sustained_pct": $GV_VS_PL_PCT,
  "governed_vs_plain_added_p99_delta_us": $GV_ADDED_DELTA_P99,
  "governed_vs_plain_added_p50_delta_us": $GV_ADDED_DELTA_P50,
  "sweep_ttft_ms": $SWEEP_TTFT_MS,
  "p99_ceiling_ms": $P99_CEIL_MS,
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
echo " gateway=$GATEWAY  GOVERNED lane"
echo "   governed added latency p99 = ${GV_OVER_P99}us  (plain ${PL_OVER_P99}us, delta ${GV_ADDED_DELTA_P99}us)"
echo "   governed sustained @${SWEEP_TTFT_MS}ms = ${GV_RPS} rps @ c=${GV_CONC}  (plain ${PL_RPS} rps, ${GV_VS_PL_PCT}%)"
echo " -> $RESULTS/$GATEWAY.json"
echo "================================================================"
