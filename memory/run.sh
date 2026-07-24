#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# MEMORY under sustained load — pluggable across gateways. Adding a gateway = dropping a
# `gateways/<name>/gateway.sh` manifest (see gateways/README.md); this runner is gateway-agnostic.
#
# It records, on ONE box against ONE mock with ONE load profile:
#   * idle RSS      — resident memory right after the gateway answers 200, before any load
#   * peak RSS      — highest resident memory sampled during sustained load
#   * post-load RSS — resident memory 60 s after load stops (does it release, or stay pinned?)
# and writes results/memory/<gateway>.json for the chart generator.
#
#   GATEWAY=busbar        memory/run.sh
#   GATEWAY=bifrost                             memory/run.sh
#   GATEWAY=litellm-rust                        memory/run.sh
#   GATEWAY=litellm-python                      memory/run.sh
#
# Knobs (env): PSIZE (payload bytes, default 150000), CONC (default 1500), DUR (seconds, default 120),
#   CAP_MIB (watchdog ceiling — kills the load if RSS crosses it, default 40000), CORES (gateway pin).
# SAFETY: an unbounded gateway will OOM the box; the watchdog kills the load at CAP_MIB.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
GATEWAY="${GATEWAY:-busbar}"
export GW_DIR="$ROOT/gateways/$GATEWAY"
[ -f "$GW_DIR/gateway.sh" ] || { echo "unknown gateway '$GATEWAY' (no $GW_DIR/gateway.sh)"; exit 2; }

PSIZE="${PSIZE:-150000}"; CONC="${CONC:-1500}"; DUR="${DUR:-120}"; CAP_MIB="${CAP_MIB:-40000}"
export CORES="${CORES:-0-3}"; LOADCORES="${LOADCORES:-0-3}"; MOCKCORES="${MOCKCORES:-0-3}"
export MOCK_PORT="${MOCK_PORT:-8000}"
RESULTS="$ROOT/results/memory"; mkdir -p "$RESULTS"
log(){ echo "[$(date +%H:%M:%S)] $*"; }

# Raise the fd ceiling to match the other six suites (perf/stream/streamcpu/xlate/governed/matrix):
# native gateways (helicone, litellm-rust) inherit this shell's RLIMIT_NOFILE, so a ~1024 default
# caps them at ~1024 concurrent connections at CONC=1500 (EMFILE) while docker gateways inherit the
# daemon's 1048576 default-ulimit - biasing native peak-RSS downward (audit R2-M1).
ulimit -n 1048576 2>/dev/null || ulimit -n 65536 2>/dev/null || true

# taskset may be absent (macOS); shim it to a no-op wrapper so the rig still runs locally.
command -v taskset >/dev/null || taskset(){ shift 2; "$@"; }

log "fetching prebuilt rig (mock + loadgen) — no on-box toolchain needed"
. "$ROOT/lib/rig.sh"; fetch_rig "$ROOT" || { echo "rig fetch failed"; exit 1; }
MOCK_BIN="$MOCK"; UGEN_BIN="$UGEN"

# Source refs (branches/tags/versions) are pinned + overridable in ONE place, and recorded below.
# shellcheck source=/dev/null
[ -f "$ROOT/gateways/versions.env" ] && source "$ROOT/gateways/versions.env"
gw_version() { echo "unknown"; }  # default; the manifest below may override
gw_diag(){ :; }  # a manifest may override to print WHY it failed to serve (docker logs / native log tail)

# ONE memory-measurement method for every gateway, native or docker: sum the resident memory (VmRSS)
# of the whole process tree from /proc — the SAME thing the native manifests do. We deliberately do
# NOT use `docker stats`, whose cgroup MemUsage includes page cache and everything else in the
# container and is not comparable to a native process's VmRSS. This is the M1 fairness fix.
_rss_tree_mib() { # root_pid  → summed VmRSS of pid + all descendants, in MiB
  local root="$1"; [ -z "$root" ] || [ "$root" = 0 ] && { echo 0; return; }
  local pids="$root" frontier="$root" next total=0 kb p c
  while [ -n "$frontier" ]; do
    next=""
    for p in $frontier; do for c in $(pgrep -P "$p" 2>/dev/null); do pids="$pids $c"; next="$next $c"; done; done
    frontier="$next"
  done
  for p in $pids; do kb=$(awk '/VmRSS/{print $2}' "/proc/$p/status" 2>/dev/null); total=$((total + ${kb:-0})); done
  awk -v k="$total" 'BEGIN{printf "%.1f", k/1024}'
}
container_rss_mib() { # container_name → its process tree's VmRSS via the host PID (same units as native)
  local pid; pid=$(sudo docker inspect -f '{{.State.Pid}}' "$1" 2>/dev/null)
  _rss_tree_mib "$pid"
}
container_hwm_mib() { # container_name → its process tree's VmHWM via the host PID (same units as native)
  local pid; pid=$(sudo docker inspect -f '{{.State.Pid}}' "$1" 2>/dev/null)
  _hwm_tree_mib "$pid"
}
# VmHWM = the kernel's own per-process high-water mark. The 0.3 s VmRSS poll above can miss a
# sub-interval allocation spike entirely; VmHWM cannot (the kernel updates it on every charge), so
# it is the honest PEAK for the memory story. Read at teardown (it survives until process exit).
_hwm_tree_mib() { # root_pid → summed VmHWM of pid + descendants, in MiB
  local root="$1"; [ -z "$root" ] || [ "$root" = 0 ] && { echo 0; return; }
  local pids="$root" frontier="$root" next total=0 kb p c
  while [ -n "$frontier" ]; do
    next=""
    for p in $frontier; do for c in $(pgrep -P "$p" 2>/dev/null); do pids="$pids $c"; next="$next $c"; done; done
    frontier="$next"
  done
  for p in $pids; do kb=$(awk '/VmHWM/{print $2}' "/proc/$p/status" 2>/dev/null); total=$((total + ${kb:-0})); done
  awk -v k="$total" 'BEGIN{printf "%.1f", k/1024}'
}
# Kernel high-water mark (VmHWM) for the ACTUAL gateway process(es) this run launched.
# This is a manifest hook, exactly like gw_rss: each gateways/<name>/gateway.sh overrides gw_hwm()
# to sum VmHWM over the SAME process(es) its gw_rss() sums VmRSS over (native pid tree, or the
# container's host-pid tree via container_hwm_mib). The default below is a safe no-op that returns
# empty (recorded as null downstream) so a manifest without the hook never fabricates a number and
# never trips `set -u` - it is gateway-agnostic and never references a hardcoded process name.
gw_hwm() { echo ""; }
json_escape(){ printf '%s' "$1" | python3 -c 'import json,sys
d=sys.stdin.buffer.read()[:1600].decode("utf-8","replace")
sys.stdout.write(json.dumps(d)[1:-1])'; }
GW_HEADERS=()  # a manifest may set extra request headers (e.g. Portkey routing, or a minted busbar vkey)
# shellcheck source=/dev/null
source "$ROOT/lib/harness.sh"
# shellcheck source=/dev/null
source "$GW_DIR/gateway.sh"
suite_deadline_start

log "starting mock on :$MOCK_PORT"
pkill -f "$MOCK_BIN" 2>/dev/null; sleep 1
setsid taskset -c "$MOCKCORES" "$MOCK_BIN" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
sleep 1

# On a trap-triggered EXIT before the sampler is stopped normally (line ~164), the sampler subshell
# `while [ ! -f "$STOP" ]` must be broken and reaped or it orphans and busy-loops calling gw_rss every
# 0.3s (audit R5-NIT). So CREATE the STOP file (its presence ends the loop) and kill the sampler pid
# ($SP, may be unset if we exit before it launches) BEFORE removing the temp files - never rm STOP while
# the loop still watches for it. Harmless on EC2 (the box self-terminates) but leaks a poller on local dev.
cleanup(){ gw_stop 2>/dev/null; pkill -f "$MOCK_BIN" 2>/dev/null
  touch "${STOP:-}" 2>/dev/null || true; kill "${SP:-}" 2>/dev/null || true
  rm -f "${STOP:-}" "${PEAKF:-}" "${LOADPIDF:-}" 2>/dev/null; }
trap cleanup EXIT

log "[$GATEWAY] build"; gw_build || { echo "build failed"; exit 1; }
# Readiness probe (rebuilds headers first so a gw_launch-minted key is picked up), bounded 60x1s with
# a -m3 per-request timeout. harness_launch_ready re-runs gw_launch + this probe up to N attempts.
CURL_H=(); UGEN_H=(); ok=0; c=000
_mem_ready(){
  CURL_H=(); UGEN_H=()
  for h in "${GW_HEADERS[@]:-}"; do [ -n "$h" ] && { CURL_H+=(-H "$h"); UGEN_H+=(-H "$h"); }; done
  local i
  for i in $(seq 1 60); do
    c=$(curl -s -m3 -o /dev/null -w "%{http_code}" "http://127.0.0.1:$GW_PORT$GW_PATH" \
        -X POST -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" "${CURL_H[@]}" \
        -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}")
    [ "$c" = "200" ] && return 0; sleep 1
  done
  return 1
}
log "[$GATEWAY] launch + wait 200 on $GW_PATH (robust boot, up to $HARNESS_BOOT_ATTEMPTS attempts)"
SERVE_ERR=""
if harness_launch_ready gw_launch _mem_ready; then
  ok=1
else
  body="$(curl -s -m3 "http://127.0.0.1:$GW_PORT$GW_PATH" -X POST \
      -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" "${CURL_H[@]}" \
      -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}" 2>&1 | head -c 400)"
  SERVE_ERR="$HARNESS_SERVE_ERR; last body=[$body]"
  log "[$GATEWAY] serve_error: $(printf '%s' "$SERVE_ERR" | head -c 300)"
fi
IDLE=$(gw_rss); log "[$GATEWAY] idle RSS: ${IDLE:-?} MiB (served=$([ "$ok" = 1 ] && echo true || echo false))"

# ── sampler + watchdog ──────────────────────────────────────────────────────────────────────────
# Temp paths + the watchdog kill are scoped to THIS run's PID (not host-global /tmp/mem.{stop,peak}
# and `pkill -x ugen`): two concurrent memory/run.sh on one host would otherwise reset each other's
# peak file mid-sample and kill each other's loadgen by name, fabricating peak_rss_mib (audit R2-M3).
PEAK=0; STOP="${TMPDIR:-/tmp}/mem.$$.stop"; PEAKF="${TMPDIR:-/tmp}/mem.$$.peak"; LOADPIDF="${TMPDIR:-/tmp}/mem.$$.loadpid"
rm -f "$STOP" "$PEAKF" "$LOADPIDF"; echo 0 >"$PEAKF"
( while [ ! -f "$STOP" ]; do
    v=$(gw_rss); [ -z "$v" ] && v=0
    awk -v v="$v" -v p="$PEAK" 'BEGIN{exit !(v+0>p+0)}' && { PEAK=$v; echo "$PEAK" >"$PEAKF"; }
    awk -v v="$v" -v c="$CAP_MIB" 'BEGIN{exit !(v+0>c+0)}' && { echo "[watchdog] $v MiB > cap $CAP_MIB — killing load"; lp=$(cat "$LOADPIDF" 2>/dev/null); [ -n "$lp" ] && kill "$lp" 2>/dev/null; touch "$STOP"; }
    sleep 0.3
  done ) & SP=$!

log "[$GATEWAY] load: ${PSIZE}B payloads, c=$CONC, ${DUR}s (watchdog cap ${CAP_MIB} MiB)"
# Run the loadgen in the background so the watchdog can kill THIS run's loadgen by recorded PID (not
# `pkill -x ugen`, which would hit a concurrent run's loadgen too).
taskset -c "$LOADCORES" "$UGEN_BIN" -url "http://127.0.0.1:$GW_PORT$GW_PATH" \
  -model "$GW_MODEL" -auth "$GW_AUTH" -c "$CONC" -d "$DUR" -psize "$PSIZE" "${UGEN_H[@]}" &
LOAD_PID=$!; echo "$LOAD_PID" >"$LOADPIDF"; wait "$LOAD_PID" 2>/dev/null || true
touch "$STOP"; kill "$SP" 2>/dev/null
PEAK=$(cat "$PEAKF")

# VmHWM must be read BEFORE the gateway stops (the counter dies with the process).
HWM=$(gw_hwm)
log "[$GATEWAY] kernel high-water mark: ${HWM:-n/a} MiB (VmHWM; sampled peak: ${PEAK:-0})"

log "[$GATEWAY] load stopped — waiting 60s to see if memory releases"
sleep 60
POST=$(gw_rss)

MEASURED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
HW="${BENCH_HARDWARE:-$(uname -m) $(nproc 2>/dev/null || echo '?')vCPU}"
BUILD="$(gw_version 2>/dev/null | tr -d '\n' | sed 's/"/\\"/g')"
log "[$GATEWAY] built: $BUILD"
cat > "$RESULTS/$GATEWAY.json" <<JSON
{
  "gateway": "$GATEWAY",
  "build": "$BUILD",
  "served": $([ "$ok" = 1 ] && echo true || echo false),
  "last_http_status": "$c",
  "serve_error": "$(json_escape "$SERVE_ERR")",
  "idle_rss_mib": ${IDLE:-0},
  "peak_rss_mib": ${PEAK:-0},
  "peak_rss_hwm_mib": ${HWM:-null},
  "post_load_rss_mib": ${POST:-0},
  "payload_bytes": $PSIZE,
  "concurrency": $CONC,
  "duration_s": $DUR,
  "endpoint": "$GW_PATH",
  "model": "$GW_MODEL",
  "arch": "${BENCH_ARCH:-$(uname -m)}",
  "hardware": "$HW",
  "measured_at": "$MEASURED_AT"
}
JSON

echo "================================================================"
echo " gateway=$GATEWAY  payload=${PSIZE}B  conc=$CONC  dur=${DUR}s"
echo "   idle RSS:      ${IDLE:-?} MiB"
echo "   PEAK RSS:      ${PEAK:-?} MiB   (under load)"
echo "   post-load RSS: ${POST:-?} MiB   (60s after load stops)"
echo " -> $RESULTS/$GATEWAY.json"
echo "================================================================"
