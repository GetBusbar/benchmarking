#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# TRANSLATION — pluggable across gateways (same gateways/<name>/gateway.sh manifests as perf/memory/
# stream). The client speaks ANTHROPIC (POST /v1/messages, Messages body, anthropic-version +
# x-api-key) while the upstream mock speaks OPENAI on the manifest's GW_PATH — so the gateway must
# translate anthropic-in to openai-out and translate the response back. The mock is untouched; that
# is the point. Per gateway it measures on the translation path:
#   * added latency (µs) at concurrency 1 = gateway p99 − direct-to-mock p99
#   * sustained RPS @ SWEEP_TTFT_MS ms LLM latency (p99 < 1 s, <0.1% error rate)
# and writes results/xlate/<gateway>.json.
#
# HONEST ASYMMETRY: the mock does not translate, so the direct baseline is the OPENAI shape straight
# to the mock (the same baseline perf/run.sh subtracts). The gateway side carries the anthropic
# ingress + both protocol conversions; the added-latency figure therefore INCLUDES the translation
# work — which is exactly what this lane exists to price. Recorded as baseline_shape=openai.
#
# FAIRNESS: many gateways cannot serve anthropic ingress against an openai upstream at all. One probe
# decides: a non-2xx, a body without the Anthropic message envelope, or the mock's own canned
# /messages body (id "msg_x" — meaning the gateway proxied the path through UNTRANSLATED) writes
# xlate_served=false with the probe status + body snippet as evidence, valid JSON, exit 0.
#
#   GATEWAY=busbar xlate/run.sh
#
# Manifest overrides (optional): GW_ANTHROPIC_PATH (default /v1/messages), GW_ANTHROPIC_AUTH_HEADER
# (a full "Name: value" header added on the anthropic side only; the loadgen already sends the token
# as BOTH `authorization: Bearer` and `x-api-key`, so this is for gateways needing something else).
#
# PER-LANE HOOKS (optional, generic - any manifest may use them, the runner special-cases nobody):
#   gw_xlate_env       - a function the runner calls right after sourcing the manifest, BEFORE the
#                        launch: the manifest adjusts its own knobs for THIS lane (e.g. swap a
#                        provider-selecting header set, point GW_ANTHROPIC_PATH at its anthropic
#                        ingress route). Needed because a manifest's default GW_HEADERS are chosen
#                        for the perf lanes and may pin a different upstream provider.
#   GW_XLATE_HEADERS   - an array of "Name: value" headers that REPLACES GW_HEADERS for this lane
#                        only (warm-up + probes + loadgen), for manifests whose provider routing
#                        rides in headers (the portkey pattern). Unset = GW_HEADERS as before.
#   GW_XLATE_CAP=0     - the gateway does NOT claim anthropic-in -> openai-out translation; the
#                        runner records xlate_declared=false with the manifest's cited
#                        GW_XLATE_CAP_NOTE instead of probing, so a never-claimed capability can
#                        never be published as a failure (same rule as the matrix capability grid).
# Knobs (env): C1_DUR (default 20), SWEEP_DELAYED ("8 32 128 256 1024 4096 8192 16384"), SWEEP_DUR
#   (seconds per point, default 10), SWEEP_TTFT_MS (default 20), PSIZE (payload bytes, 256), CORES pin.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
GATEWAY="${GATEWAY:-busbar}"
export GW_DIR="$ROOT/gateways/$GATEWAY"
[ -f "$GW_DIR/gateway.sh" ] || { echo "unknown gateway '$GATEWAY'"; exit 2; }

C1_DUR="${C1_DUR:-20}"; SWEEP_DUR="${SWEEP_DUR:-10}"; PSIZE="${PSIZE:-256}"
# The mock sleeps SWEEP_TTFT_MS per request so the ceiling is concurrent-in-flight capacity on the
# TRANSLATION path, not a race against mock CPU. NOTE (estimator drift, audit M2): this lane still
# walks a FIXED delayed ladder here, whereas perf/matrix migrated to lib/sweep.sh's peak search
# (run_sweep ... peak). The two conversion probe shapes (openai direct vs anthropic gateway) make a
# drop-in run_sweep adoption invasive, so the ladder is kept for now; the mock-ceiling probe below is
# capped to match perf (audit M3). The gates (p99 < ceiling, <0.1% err) are identical.
SWEEP_DELAYED="${SWEEP_DELAYED:-8 32 128 256 1024 4096 8192 16384}"
SWEEP_TTFT_MS="${SWEEP_TTFT_MS:-20}"
P99_CEIL_MS="${P99_CEIL_MS:-1000}"
ulimit -n 1048576 2>/dev/null || ulimit -n 65536 2>/dev/null || true
export CORES="${CORES:-0-3}"; LOADCORES="${LOADCORES:-0-3}"; MOCKCORES="${MOCKCORES:-0-3}"
export MOCK_PORT="${MOCK_PORT:-8000}"
RESULTS="$ROOT/results/xlate"; mkdir -p "$RESULTS"
log(){ echo "[$(date +%H:%M:%S)] $*"; }
command -v taskset >/dev/null || taskset(){ shift 2; "$@"; }
command -v setsid  >/dev/null || setsid(){ "$@"; }

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
# Per-lane hook: the manifest adapts its own knobs for the translation lane (headers, anthropic
# ingress path, env). Generic: the runner calls it for ANY manifest that defines it.
if declare -f gw_xlate_env >/dev/null; then gw_xlate_env; fi
XPATH_A="${GW_ANTHROPIC_PATH:-/v1/messages}"

# Declared-capability gate: a manifest that declares GW_XLATE_CAP=0 (with a cited GW_XLATE_CAP_NOTE)
# does not claim this translation at all - record that honestly and exit; never probe, so a
# never-claimed capability can never be published as a red failure.
GW_XLATE_CAP="${GW_XLATE_CAP:-1}"
if [ "$GW_XLATE_CAP" != 1 ]; then
  CAPNOTE="${GW_XLATE_CAP_NOTE:-this gateway does not declare anthropic-ingress to openai-upstream translation}"
  BUILD="$(gw_version 2>/dev/null | tr -d '\n' | sed 's/"/\\"/g')"
  log "[$GATEWAY] xlate not declared (capability limit): $CAPNOTE"
  cat > "$RESULTS/$GATEWAY.json" <<JSON
{
  "gateway": "$GATEWAY",
  "build": "$BUILD",
  "served": false,
  "xlate_served": false,
  "xlate_declared": false,
  "xlate_passthrough": false,
  "last_http_status": "",
  "xlate_probe_status": "",
  "serve_error": "",
  "xlate_error": "not declared: $(json_escape "$CAPNOTE")",
  "xlate_cap_note": "$(json_escape "$CAPNOTE")",
  "xlate_added_latency_p50_us": 0,
  "xlate_added_latency_p99_us": 0,
  "xlate_gateway_c1_p99_us": 0,
  "xlate_direct_c1_p99_us": 0,
  "xlate_baseline_shape": "openai",
  "xlate_rps_sustained_20ms": 0,
  "xlate_rps_sustained_20ms_concurrency": 0,
  "xlate_rps_sustained_20ms_mock_ceiling": 0,
  "xlate_rps_sustained_20ms_mock_bound": false,
  "sweep_ttft_ms": $SWEEP_TTFT_MS,
  "p99_ceiling_ms": $P99_CEIL_MS,
  "sweep_sustained_20ms": [],
  "payload_bytes": $PSIZE,
  "endpoint": "$XPATH_A",
  "upstream_endpoint": "$GW_PATH",
  "model": "$GW_MODEL",
  "arch": "${BENCH_ARCH:-$(uname -m)}",
  "hardware": "${BENCH_HARDWARE:-$(uname -m) $(nproc 2>/dev/null || echo '?')vCPU}",
  "measured_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON
  echo " -> $RESULTS/$GATEWAY.json (xlate not declared - capability limit, cited)"
  exit 0
fi

log "starting mock :$MOCK_PORT (instant, openai upstream on $GW_PATH)"
pkill -f "$MOCK" 2>/dev/null; sleep 1
setsid taskset -c "$MOCKCORES" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
sleep 1
cleanup(){ gw_stop 2>/dev/null; pkill -f "$MOCK" 2>/dev/null; }
trap cleanup EXIT

# openai-shape probe (direct baseline + mock guardrail) — identical to perf/run.sh's probe
oprobe(){ # url conc dur
  tmo "$(probe_budget "$3")" taskset -c "$LOADCORES" "$UGEN" -url "$1" -model "$GW_MODEL" -auth "$GW_AUTH" -c "$2" -d "$3" -psize "$PSIZE" ${UGEN_H[@]+"${UGEN_H[@]}"} 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++){split($i,a,"=");v[a[1]]=a[2]}; print v["rps"],v["fail"],v["p99us"],v["p50us"]}'
}
# anthropic-shape probe (the translation path through the gateway). Hard-timeout wrapped: a gateway
# that stops responding under load must fail this probe fast, not block the suite (the arch failure).
aprobe(){ # url conc dur
  tmo "$(probe_budget "$3")" taskset -c "$LOADCORES" "$UGEN" -url "$1" -shape anthropic -model "$GW_MODEL" -auth "$GW_AUTH" -c "$2" -d "$3" -psize "$PSIZE" \
    ${UGEN_H[@]+"${UGEN_H[@]}"} ${XH[@]+"${XH[@]}"} 2>/dev/null \
    | awk '{for(i=1;i<=NF;i++){split($i,a,"=");v[a[1]]=a[2]}; print v["rps"],v["fail"],v["p99us"],v["p50us"]}'
}

log "[$GATEWAY] build"; gw_build || { echo "build failed"; exit 1; }
# Header arrays rebuilt inside the readiness probe (AFTER launch) so a manifest can mint a key in
# gw_launch (busbar vkey). harness_launch_ready re-runs gw_launch + this probe up to N attempts.
UGEN_H=(); CURL_H=(); XH=(); ok=0; c=000
_xlate_ready(){
  UGEN_H=(); CURL_H=(); XH=()
  # Per-lane header hook: GW_XLATE_HEADERS (when set, non-empty) replaces GW_HEADERS for this lane
  # only - generic, for any manifest whose upstream-provider routing rides in request headers.
  local _hsrc=()
  if [ -n "${GW_XLATE_HEADERS+x}" ] && [ "${#GW_XLATE_HEADERS[@]}" -gt 0 ]; then
    _hsrc=("${GW_XLATE_HEADERS[@]}")
  else
    _hsrc=(${GW_HEADERS[@]+"${GW_HEADERS[@]}"})
  fi
  local h
  for h in ${_hsrc[@]+"${_hsrc[@]}"}; do [ -n "$h" ] && { UGEN_H+=(-H "$h"); CURL_H+=(-H "$h"); }; done
  [ -n "${GW_ANTHROPIC_AUTH_HEADER:-}" ] && XH+=(-H "$GW_ANTHROPIC_AUTH_HEADER")
  local i
  for i in $(seq 1 60); do
    c=$(curl -s -m3 -o /dev/null -w "%{http_code}" "http://127.0.0.1:$GW_PORT$GW_PATH" -X POST \
        -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" ${CURL_H[@]+"${CURL_H[@]}"} \
        -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}")
    [ "$c" = 200 ] && return 0; sleep 1
  done
  return 1
}
log "[$GATEWAY] launch + wait 200 on $GW_PATH (openai warm; robust boot, up to $HARNESS_BOOT_ATTEMPTS attempts)"
SERVE_ERR=""
if harness_launch_ready gw_launch _xlate_ready; then ok=1
else SERVE_ERR="$HARNESS_SERVE_ERR"; fi

# ── can the gateway translate at all? one probe, then fail gracefully if not ──────────────────────
# Requires: 2xx, an Anthropic message envelope ("type":"message" or a content array) in the body,
# and NOT the mock's canned /messages body (id "msg_x") — that would mean the gateway proxied the
# path through verbatim instead of translating (the openai upstream never emits msg_x).
XLATE_OK=0; XLATE_ERR=""; XLATE_PASSTHROUGH=false; XC=000
ABODY="{\"model\":\"$GW_MODEL\",\"max_tokens\":64,\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}]}"
AH=(-H "content-type: application/json" -H "anthropic-version: 2023-06-01" \
    -H "x-api-key: $GW_AUTH" -H "authorization: Bearer $GW_AUTH")
if [ "$ok" = 1 ]; then
  xbody="$(curl -s -m5 -w '\n%{http_code}' "http://127.0.0.1:$GW_PORT$XPATH_A" -X POST \
      "${AH[@]}" ${CURL_H[@]+"${CURL_H[@]}"} ${XH[@]+"${XH[@]}"} -d "$ABODY" 2>&1)"
  XC="${xbody##*$'\n'}"; xbody="${xbody%$'\n'*}"
  if printf '%s' "$xbody" | grep -q '"id":"msg_x"'; then
    XLATE_PASSTHROUGH=true
    XLATE_ERR="UNTRANSLATED passthrough: gateway returned the mock's canned /messages body (id msg_x) — it proxied the path, it did not translate; body=[$(printf '%s' "$xbody" | head -c 400)]"
    log "[$GATEWAY] WARNING passthrough, not translation — xlate_served=false"
  elif [ "${XC#2}" != "$XC" ] && printf '%s' "$xbody" | grep -Eq '"type"[[:space:]]*:[[:space:]]*"message"|"content"[[:space:]]*:[[:space:]]*\['; then
    XLATE_OK=1
    log "[$GATEWAY] translation probe OK (HTTP $XC, anthropic envelope): $(printf '%s' "$xbody" | head -c 160)"
  else
    XLATE_ERR="HTTP $XC on POST $XPATH_A (anthropic shape); body=[$(printf '%s' "$xbody" | head -c 400)]; diag=[$(gw_diag 2>&1 | tail -n 20)]"
    log "[$GATEWAY] WARNING no anthropic-shaped 2xx on $XPATH_A — xlate_served=false"
  fi
fi

DURL="http://127.0.0.1:$MOCK_PORT$GW_PATH"; XURL="http://127.0.0.1:$GW_PORT$XPATH_A"
DP99=0; DP50=0; GP99=0; GP50=0; OVER_P99=0; OVER_P50=0
LLM_RPS=0; LLM_CONC=0; LLM_MOCK=0; LLM_BOUND=false; LLM_JSON=""
if [ "$XLATE_OK" = 1 ]; then
  # ── c1: direct baseline (openai → mock) + gateway (anthropic → gateway) → added µs ──────────────
  WARMUP_DUR="${WARMUP_DUR:-5}"
  log "[$GATEWAY] warm-up ${WARMUP_DUR}s (discarded, both paths)"
  oprobe "$DURL" 1 "$WARMUP_DUR" >/dev/null 2>&1; aprobe "$XURL" 1 "$WARMUP_DUR" >/dev/null 2>&1
  log "[$GATEWAY] c1 baseline (direct→mock, OPENAI shape — the mock does not translate) ${C1_DUR}s"
  read -r _drps _dfail DP99 DP50 < <(oprobe "$DURL" 1 "$C1_DUR")
  log "[$GATEWAY] c1 gateway (ANTHROPIC shape on $XPATH_A) ${C1_DUR}s"
  read -r _grps _gfail GP99 GP50 < <(aprobe "$XURL" 1 "$C1_DUR")
  # Gate the translation-latency lane on c1 honesty: aprobe/oprobe only pool 200 latencies, so GP99=0
  # means no successful sample and a material error rate means the window was 429/5xx'd - either way
  # the added-latency number is fabricated. Demote xlate_served=false with a provable reason instead
  # of publishing 0 or an error-path win (matches the no-2xx-envelope path above).
  _gtot=$(( ${_grps:-0} * C1_DUR + ${_gfail:-0} )); _dtot=$(( ${_drps:-0} * C1_DUR + ${_dfail:-0} ))
  if [ "${GP99:-0}" -le 0 ] || [ "${DP99:-0}" -le 0 ] \
     || ! awk -v f="${_gfail:-1}" -v t="$_gtot" 'BEGIN{exit !(t>0 && f<=0.001*t)}' \
     || ! awk -v f="${_dfail:-1}" -v t="$_dtot" 'BEGIN{exit !(t>0 && f<=0.001*t)}'; then
    XLATE_OK=0
    XLATE_ERR="${XLATE_ERR:+$XLATE_ERR; }c1 latency window unreliable: gw ok=${_grps:-0}/s fail=${_gfail:-?} p99=${GP99:-0}us; direct ok=${_drps:-0}/s fail=${_dfail:-?} p99=${DP99:-0}us"
    GP99=0; GP50=0; DP99=0; DP50=0
    log "[$GATEWAY] WARNING xlate c1 window had errors / no valid sample - xlate_served=false"
  fi
  OVER_P99=$(( ${GP99:-0} - ${DP99:-0} )); OVER_P50=$(( ${GP50:-0} - ${DP50:-0} ))
  log "[$GATEWAY] c1: gw p99=${GP99}µs direct p99=${DP99}µs → added (incl. translation) p99=${OVER_P99}µs (p50=${OVER_P50}µs)"
fi
if [ "$XLATE_OK" = 1 ]; then

  # ── sustained RPS @ ${SWEEP_TTFT_MS}ms on the translation path ──────────────────────────────────
  log "[$GATEWAY] sweep — sustained RPS @ ${SWEEP_TTFT_MS}ms LLM latency (translation path)"
  pkill -f "$MOCK" 2>/dev/null; sleep 1
  setsid taskset -c "$MOCKCORES" env MOCK_TTFT_MS="$SWEEP_TTFT_MS" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
  sleep 1
  top=1; for w in $SWEEP_DELAYED; do top=$w; done
  # Mock-ceiling reference: cap the probe concurrency the same way perf's peak lane does
  # (lib/sweep.sh SWEEP_MOCKCEIL_CONC:-2048). At the ladder top (16384) the loadgen + mock are both
  # overloaded, so the reference comes out LOW and the mock_bound guard fires against a degraded
  # ceiling; capping keeps the reference honest and comparable to perf's for the identical rig.
  mock_conc=$top; [ "$mock_conc" -gt "${SWEEP_MOCKCEIL_CONC:-2048}" ] && mock_conc="${SWEEP_MOCKCEIL_CONC:-2048}"
  read -r mrps _a _b _c2 < <(oprobe "$DURL" "$mock_conc" "$SWEEP_DUR"); LLM_MOCK=${mrps:-0}
  for conc in $SWEEP_DELAYED; do
    if suite_deadline_expired; then log "[$GATEWAY] suite wall-clock ceiling reached mid-sweep - stopping sweep, recording what we have"; break; fi
    read -r rps fail p99 _p50 < <(aprobe "$XURL" "$conc" "$SWEEP_DUR")
    rps=${rps:-0}; fail=${fail:-1}; p99=${p99:-99999999}
    log "[$GATEWAY]   (ttft=${SWEEP_TTFT_MS}ms) c=$conc → rps=$rps p99=$((p99/1000))ms fail=$fail"
    LLM_JSON="${LLM_JSON}${LLM_JSON:+,}{\"conc\":$conc,\"rps\":$rps,\"p99_us\":$p99,\"fail\":$fail}"
    if awk -v f="$fail" -v r="$rps" -v d="$SWEEP_DUR" 'BEGIN{tot=r*d+f; exit !(tot>0 && f<=0.001*tot)}' \
       && [ "$p99" -lt $((P99_CEIL_MS*1000)) ] && [ "$rps" -gt "$LLM_RPS" ]; then
      LLM_RPS=$rps; LLM_CONC=$conc
    fi
  done
  if [ "${LLM_MOCK:-0}" -gt 0 ] && awk -v c="$LLM_RPS" -v m="$LLM_MOCK" 'BEGIN{exit !(c>=0.9*m)}'; then LLM_BOUND=true; fi
  [ "$LLM_BOUND" = true ] && log "[$GATEWAY] ⚠ ceiling ($LLM_RPS) within 10% of mock ($LLM_MOCK) — MOCK-BOUND floor"
  log "[$GATEWAY] sustained RPS @${SWEEP_TTFT_MS}ms (translating) = $LLM_RPS rps @ c=$LLM_CONC"
fi

BUILD="$(gw_version 2>/dev/null | tr -d '\n' | sed 's/"/\\"/g')"
cat > "$RESULTS/$GATEWAY.json" <<JSON
{
  "gateway": "$GATEWAY",
  "build": "$BUILD",
  "served": $([ "$ok" = 1 ] && echo true || echo false),
  "xlate_served": $([ "$XLATE_OK" = 1 ] && echo true || echo false),
  "xlate_declared": true,
  "xlate_passthrough": $XLATE_PASSTHROUGH,
  "last_http_status": "$c",
  "xlate_probe_status": "$XC",
  "serve_error": "$(json_escape "$SERVE_ERR")",
  "xlate_error": "$(json_escape "$XLATE_ERR")",
  "xlate_added_latency_p50_us": $OVER_P50,
  "xlate_added_latency_p99_us": $OVER_P99,
  "xlate_gateway_c1_p99_us": ${GP99:-0},
  "xlate_direct_c1_p99_us": ${DP99:-0},
  "xlate_baseline_shape": "openai",
  "xlate_baseline_note": "direct baseline is the OPENAI shape straight to the mock (the mock does not translate); the gateway figure carries anthropic ingress + both conversions, so added latency includes the translation work by design",
  "xlate_rps_sustained_20ms": $LLM_RPS,
  "xlate_rps_sustained_20ms_concurrency": $LLM_CONC,
  "xlate_rps_sustained_20ms_mock_ceiling": $LLM_MOCK,
  "xlate_rps_sustained_20ms_mock_bound": $LLM_BOUND,
  "sweep_ttft_ms": $SWEEP_TTFT_MS,
  "p99_ceiling_ms": $P99_CEIL_MS,
  "sweep_sustained_20ms": [$LLM_JSON],
  "payload_bytes": $PSIZE,
  "endpoint": "$XPATH_A",
  "upstream_endpoint": "$GW_PATH",
  "model": "$GW_MODEL",
  "cores": "gateway=${CORES} loadgen=${LOADCORES} mock=${MOCKCORES}",
  "mock_proto": "h1+h2c",
  "arch": "${BENCH_ARCH:-$(uname -m)}",
  "hardware": "${BENCH_HARDWARE:-$(uname -m) $(nproc 2>/dev/null || echo '?')vCPU}",
  "measured_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON
echo "================================================================"
if [ "$XLATE_OK" = 1 ]; then
  echo " gateway=$GATEWAY   translation added latency p99=${OVER_P99}µs (anthropic-in → openai-out)"
  echo "   sustained @ ${SWEEP_TTFT_MS}ms translating = ${LLM_RPS} rps @ c=${LLM_CONC}  (mock_bound=${LLM_BOUND})"
else
  echo " gateway=$GATEWAY   did not translate (xlate_served=false, passthrough=$XLATE_PASSTHROUGH)"
fi
echo " -> $RESULTS/$GATEWAY.json"
echo "================================================================"
