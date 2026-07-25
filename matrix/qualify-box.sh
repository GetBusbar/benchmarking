#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# BOX QUALIFICATION — the ON-BOX MEASUREMENT half. Runs on a freshly launched bench box, BEFORE that
# box is trusted with a multi-hour 6x6. The VERDICT half lives in lib/box_qualify.sh and is evaluated
# by the orchestrator (run-on-ec2.sh), which is where the per-gateway baseline history lives — this
# script only MEASURES and prints JSON. That split is deliberate: the box is disposable and carries no
# history (results/ is excluded from the harness rsync), and the box must never be the thing that
# decides whether to trust itself.
#
#   matrix/qualify-box.sh stage1
#       The no-gateway FLOOR probe. Mock + loadgen only, NO gateway anywhere in the path — this is
#       the rig's own floor, the same measurement the 6x6 publishes as direct_c1_p99_us. Runs BEFORE
#       the gateway is built or launched, so a grossly bad box is rejected before we pay the build.
#       Deliberately SHORT (tens of seconds): it runs on every box of every run.
#
#   matrix/qualify-box.sh stage2 <ingress>'>'<egress> <concurrency>
#       The PEAK REPLAY. Builds + launches the gateway configured for <egress> exactly as the 6x6's
#       launch ladder does, then replays THAT gateway's own recorded peak cell at (and around) its own
#       recorded winning concurrency. Far more discriminating than the floor because it exercises the
#       real measured path. Still short: three rungs x one sweep window, not the full adaptive ladder.
#
# Both write JSON to stdout AND to results/box-qualify/stage<N>.json on the box. The orchestrator
# pulls that file, applies lib/box_qualify.sh's verdict against the gateway's rolling baseline, and
# either proceeds, or terminates this box and launches a replacement.
#
# WHY THE 6x6 CANNOT DO THIS ITSELF: by the time matrix/run.sh has a direct_c1_p99_us to compare, the
# gateway is built, launched and hours into the sweep. The whole point is to find out first.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
GATEWAY="${GATEWAY:-busbar}"
STAGE="${1:-stage1}"

# ── knobs (short by construction: this runs on every box of every run) ───────────────────────────
# Stage 1: N windows of BQ_PROBE_DUR seconds at concurrency 1, direct to the mock. Repeated windows
# (rather than one long one) are what make the JITTER signal possible: a contended box shows spread
# ACROSS windows even when the pooled median looks normal.
BQ_PROBE_SAMPLES="${BQ_PROBE_SAMPLES:-7}"
BQ_PROBE_DUR="${BQ_PROBE_DUR:-3}"
BQ_PROBE_WARMUP="${BQ_PROBE_WARMUP:-3}"
# Stage 2: one sweep window per rung, three rungs bracketing the recorded winner. The bracket exists
# because the adaptive sweep's rung granularity means the winning rung can legitimately move one step
# between runs; comparing a single fixed rung would charge that granularity to the box.
BQ_REPLAY_DUR="${BQ_REPLAY_DUR:-10}"
BQ_REPLAY_WARMUP="${BQ_REPLAY_WARMUP:-8}"

export CORES="${CORES:-0-3}"; LOADCORES="${LOADCORES:-0-3}"; MOCKCORES="${MOCKCORES:-0-3}"
export MOCK_PORT="${MOCK_PORT:-8000}"
OUT_DIR="$ROOT/results/box-qualify"; mkdir -p "$OUT_DIR"
log(){ echo "[$(date +%H:%M:%S)] [qualify/$GATEWAY] $*" >&2; }
command -v taskset >/dev/null || taskset(){ shift 2; "$@"; }
command -v setsid  >/dev/null || setsid(){ "$@"; }

# The loadgen parameters MUST match the 6x6's own c1 direct baseline, or the floor we measure is not
# the floor the run publishes and the drift comparison is meaningless. Same PSIZE, same ugen body
# shape, same mock endpoint as matrix/run.sh's openai-ingress direct baseline.
PSIZE="${PSIZE:-256}"
GW_MODEL="${GW_MODEL:-gpt-4o-mini}"; GW_AUTH="${GW_AUTH:-sk-fake-benchmark-key}"
UGEN_H=(); SWEEP_BODY=""
C1_DUR="${C1_DUR:-20}"; SWEEP_DUR="${SWEEP_DUR:-10}"; WARMUP_DUR="${WARMUP_DUR:-5}"; P99_CEIL_MS="${P99_CEIL_MS:-1000}"

# shellcheck source=/dev/null
. "$ROOT/lib/rig.sh"
fetch_rig "$ROOT" || { echo '{"error":"rig fetch failed"}'; exit 1; }
# shellcheck source=/dev/null
source "$ROOT/lib/harness.sh"
# shellcheck source=/dev/null
source "$ROOT/lib/sweep.sh"

# The PLAIN (non-recording, no-delay) mock — the identical serving conditions the 6x6 measures
# rps_max_proxy and its c1 direct baseline against. mock_stop_wait (lib/harness.sh) is the shared
# choke point that waits for a previous mock to actually release the port instead of guessing.
qb_mock_start(){
  mock_stop_wait || log "WARNING :$MOCK_PORT still occupied after mock_stop_wait — the relaunch may fail to bind"
  setsid taskset -c "$MOCKCORES" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
  local i
  for i in $(seq 1 30); do
    curl -s -m2 -o /dev/null "http://127.0.0.1:$MOCK_PORT/v1/chat/completions" -X POST \
      -H 'content-type: application/json' -d '{"model":"m","messages":[{"role":"user","content":"x"}]}' && return 0
    sleep 1
  done
  return 1
}

json_str(){ if [ -z "${1:-}" ]; then printf 'null'; else printf '"%s"' "$(printf '%s' "$1" | tr -d '"\\' | tr '\n' ' ')"; fi; }
json_num(){ case "${1:-}" in ''|*[!0-9.+-]*) printf 'null';; *) printf '%s' "$1";; esac; }
json_arr(){ local first=1 v; printf '['; for v in "$@"; do case "$v" in ''|*[!0-9.+-]*) continue;; esac
  [ "$first" = 1 ] || printf ', '; printf '%s' "$v"; first=0; done; printf ']'; }

# ═════════════════════════════════════════════════════════════════════════════════════════════════
# STAGE 1 — the no-gateway floor.
# ═════════════════════════════════════════════════════════════════════════════════════════════════
stage1(){
  local t0=$SECONDS
  log "stage 1: no-gateway floor probe (mock + loadgen only, ${BQ_PROBE_SAMPLES}x${BQ_PROBE_DUR}s at c1)"
  if ! qb_mock_start; then
    log "FATAL the mock never became ready on :$MOCK_PORT — this box cannot even run the rig"
    printf '{"stage": 1, "error": "mock never became ready on :%s", "floor_p99_us_median": null}\n' "$MOCK_PORT" | tee "$OUT_DIR/stage1.json"
    return 1
  fi
  local DURL_LOCAL="http://127.0.0.1:$MOCK_PORT/v1/chat/completions"
  # Discarded warm-up: the same discipline sweep_c1 uses, so the first window never carries
  # connection-establishment cost into the floor.
  sweep_probe "$DURL_LOCAL" 1 "$BQ_PROBE_WARMUP" >/dev/null 2>&1
  local p99s=() p50s=() rpss=() i rps fail p99 p50
  for i in $(seq 1 "$BQ_PROBE_SAMPLES"); do
    read -r rps fail p99 p50 < <(sweep_probe "$DURL_LOCAL" 1 "$BQ_PROBE_DUR")
    # A window that failed requests, or produced no pooled latency at all, is NOT a sample: recording
    # it would let a broken window masquerade as a fast one (sweep_probe only pools latencies from
    # 200s, so p99=0 means "no successful sample", never "instant").
    if [ -n "${p99:-}" ] && [ "${p99:-0}" -gt 0 ] && [ "${fail:-1}" = 0 ]; then
      p99s+=("$p99"); p50s+=("$p50"); rpss+=("$rps")
      log "  sample $i/$BQ_PROBE_SAMPLES: p99=${p99}us p50=${p50}us rps=${rps}"
    else
      log "  sample $i/$BQ_PROBE_SAMPLES: DISCARDED (rps=${rps:-?} fail=${fail:-?} p99=${p99:-?}) — not a usable floor window"
    fi
  done
  pkill -f "$MOCK" 2>/dev/null
  local med lo hi p50med rpsmed
  med="$(bq_median "${p99s[@]+"${p99s[@]}"}")"; lo="$(bq_min "${p99s[@]+"${p99s[@]}"}")"; hi="$(bq_max "${p99s[@]+"${p99s[@]}"}")"
  p50med="$(bq_median "${p50s[@]+"${p50s[@]}"}")"; rpsmed="$(bq_median "${rpss[@]+"${rpss[@]}"}")"
  log "stage 1 floor: p99 median=${med:-?}us (min ${lo:-?} / max ${hi:-?}), p50 median=${p50med:-?}us, c1 rps median=${rpsmed:-?} in $((SECONDS-t0))s"
  # Raw samples are ALWAYS emitted, gate or no gate: they are the calibration evidence from which the
  # true run-to-run distribution gets computed after the frozen-codebase repeat runs.
  printf '{"stage": 1, "arch": %s, "hardware": %s, "measured_at": "%s", "probe_s": %s, "samples": %s, "sample_dur_s": %s, "samples_p99_us": %s, "samples_p50_us": %s, "samples_rps": %s, "floor_p99_us_median": %s, "floor_p99_us_min": %s, "floor_p99_us_max": %s, "floor_p50_us_median": %s, "c1_rps_median": %s}\n' \
    "$(json_str "${BENCH_ARCH:-}")" "$(json_str "${BENCH_HARDWARE:-}")" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    "$((SECONDS-t0))" "${#p99s[@]}" "$BQ_PROBE_DUR" \
    "$(json_arr "${p99s[@]+"${p99s[@]}"}")" "$(json_arr "${p50s[@]+"${p50s[@]}"}")" "$(json_arr "${rpss[@]+"${rpss[@]}"}")" \
    "$(json_num "$med")" "$(json_num "$lo")" "$(json_num "$hi")" "$(json_num "$p50med")" "$(json_num "$rpsmed")" \
    | tee "$OUT_DIR/stage1.json"
}

# ═════════════════════════════════════════════════════════════════════════════════════════════════
# STAGE 2 — replay this gateway's own recorded peak cell.
#
# The launch ladder MIRRORS matrix/run.sh launch_egress (gw_matrix_egress <dialect>, falling back to
# gw_launch when the manifest has no writer for it) so the gateway runs under the SAME configuration
# that produced the recorded number. It is duplicated rather than shared because matrix/run.sh is a
# top-level script, not a sourceable library — if that ladder ever moves into lib/, this should call it.
# ═════════════════════════════════════════════════════════════════════════════════════════════════
stage2(){
  local cell_spec="${1:-}" conc="${2:-}"
  local ing="${cell_spec%%>*}" eg="${cell_spec##*>}"
  local t0=$SECONDS
  if [ -z "$ing" ] || [ -z "$eg" ] || [ -z "$conc" ]; then
    printf '{"stage": 2, "error": "no recorded peak cell/concurrency to replay", "replay_rps": null}\n' | tee "$OUT_DIR/stage2.json"
    return 0
  fi
  if [ -f "$ROOT/gateways/$GATEWAY/gateway.sh" ]; then export GW_DIR="$ROOT/gateways/$GATEWAY"
  elif [ -f "$HERE/$GATEWAY/gateway.sh" ]; then export GW_DIR="$HERE/$GATEWAY"
  else printf '{"stage": 2, "error": "unknown gateway %s", "replay_rps": null}\n' "$GATEWAY" | tee "$OUT_DIR/stage2.json"; return 0; fi

  [ -f "$ROOT/gateways/versions.env" ] && source "$ROOT/gateways/versions.env"
  # Defaults for the optional manifest hooks, declared BEFORE the manifest so a manifest that defines
  # them wins and one that does not cannot abort the replay on an undefined function.
  gw_version(){ echo unknown; }; gw_diag(){ :; }; gw_stop(){ :; }; gw_build(){ :; }; GW_HEADERS=()
  # shellcheck source=/dev/null
  source "$GW_DIR/gateway.sh"
  # shellcheck source=/dev/null
  source "$ROOT/lib/ingress.sh"
  local GW_MODEL_BASE="${GW_MODEL:-}"

  log "stage 2: peak replay of $ing>$eg at c=$conc (build + launch first)"
  if ! gw_build; then
    log "gw_build FAILED — cannot replay; reporting unmeasured (the orchestrator treats a build failure as a gateway fault, not a bad box)"
    printf '{"stage": 2, "error": "gw_build failed", "cell": %s, "replay_rps": null}\n' "$(json_str "$ing>$eg")" | tee "$OUT_DIR/stage2.json"
    return 0
  fi
  qb_mock_start || log "WARNING the plain mock did not become ready before the replay"

  GW_MODEL="$GW_MODEL_BASE"
  gw_stop 2>/dev/null; sleep 1
  if declare -f gw_matrix_egress >/dev/null; then
    gw_matrix_egress "$eg" || gw_launch
  else
    gw_launch
  fi
  local CURL_H=() h
  for h in "${GW_HEADERS[@]:-}"; do [ -n "$h" ] && CURL_H+=(-H "$h"); done
  local path body dh=()
  path="$(ingress_path "$ing")"
  case "$ing" in
    openai)           body="{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}],\"max_tokens\":16}";;
    openai-responses) body="{\"model\":\"$GW_MODEL\",\"input\":\"hello\"}";;
    anthropic)        body="{\"model\":\"$GW_MODEL\",\"max_tokens\":64,\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}"
                      dh=(-H "anthropic-version: 2023-06-01" -H "x-api-key: $GW_AUTH");;
    gemini)           body='{"contents":[{"parts":[{"text":"hello"}]}]}'; dh=(-H "x-goog-api-key: $GW_AUTH");;
    cohere)           body="{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}";;
    bedrock)          body='{"messages":[{"role":"user","content":[{"text":"hello"}]}]}';;
    *) body="{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}";;
  esac
  # Readiness: the same bounded warm-up loop the 6x6 gates a column on. A gateway that never 200s here
  # is a GATEWAY problem, not a box problem — say so explicitly so the orchestrator does not burn its
  # replacement-box budget on a gateway that would fail on any box.
  local i st=000
  for i in $(seq 1 45); do
    st=$(curl -s -m3 -o /dev/null -w "%{http_code}" "http://127.0.0.1:$GW_PORT$path" -X POST \
      -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" \
      ${CURL_H[@]+"${CURL_H[@]}"} ${dh[@]+"${dh[@]}"} -d "$body")
    [ "$st" = 200 ] && break
    sleep 1
  done
  if [ "$st" != 200 ]; then
    log "gateway never answered 200 on $path (last=$st) — NOT a box verdict"
    gw_stop 2>/dev/null
    printf '{"stage": 2, "error": "gateway did not become ready (last HTTP %s on %s)", "gateway_fault": true, "cell": %s, "replay_rps": null}\n' \
      "$st" "$path" "$(json_str "$ing>$eg")" | tee "$OUT_DIR/stage2.json"
    return 0
  fi

  UGEN_H=( ${CURL_H[@]+"${CURL_H[@]}"} ${dh[@]+"${dh[@]}"} )
  SWEEP_BODY="$body"
  local GURL="http://127.0.0.1:$GW_PORT$path"
  sweep_probe "$GURL" "$conc" "$BQ_REPLAY_WARMUP" >/dev/null 2>&1
  # Three rungs bracketing the recorded winner. The peak is a MAX statistic, so the replayed peak is
  # the max over the bracket — exactly how run_sweep picks its ceiling from the rungs it probed.
  local rungs=() r rps fail p99 p50 best=0 bestc="" json="" lo hi
  lo=$(( conc * 3 / 4 )); [ "$lo" -lt 1 ] && lo=1
  hi=$(( conc * 4 / 3 )); [ "$hi" -le "$conc" ] && hi=$(( conc + 1 ))
  rungs=("$lo" "$conc" "$hi")
  for r in "${rungs[@]}"; do
    read -r rps fail p99 p50 < <(sweep_probe "$GURL" "$r" "$BQ_REPLAY_DUR")
    rps="${rps:-0}"; fail="${fail:-0}"; p99="${p99:-0}"
    log "  replay rung c=$r: rps=$rps fail=$fail p99=${p99}us"
    json="${json}${json:+, }{\"conc\": $r, \"rps\": $(json_num "$rps"), \"fail\": $(json_num "$fail"), \"p99_us\": $(json_num "$p99")}"
    if [ "$rps" -gt "$best" ] 2>/dev/null; then best="$rps"; bestc="$r"; fi
  done
  gw_stop 2>/dev/null
  pkill -f "$MOCK" 2>/dev/null
  log "stage 2 replay peak: ${best} rps at c=${bestc} over rungs ${rungs[*]} in $((SECONDS-t0))s"
  printf '{"stage": 2, "arch": %s, "measured_at": "%s", "probe_s": %s, "cell": %s, "baseline_conc": %s, "rungs": [%s], "replay_rps": %s, "replay_conc": %s, "replay_dur_s": %s}\n' \
    "$(json_str "${BENCH_ARCH:-}")" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$((SECONDS-t0))" \
    "$(json_str "$ing>$eg")" "$(json_num "$conc")" "$json" "$(json_num "$best")" "$(json_num "$bestc")" "$BQ_REPLAY_DUR" \
    | tee "$OUT_DIR/stage2.json"
}

# bq_median/bq_min/bq_max come from the verdict library; the measurement half reuses them so the
# median the box reports and the median the orchestrator gates on are computed by the SAME code.
# shellcheck source=/dev/null
source "$ROOT/lib/box_qualify.sh"

case "$STAGE" in
  stage1) stage1;;
  stage2) shift; stage2 "${1:-}" "${2:-}";;
  *) echo "usage: $0 stage1 | stage2 <ingress>'>'<egress> <concurrency>" >&2; exit 2;;
esac
