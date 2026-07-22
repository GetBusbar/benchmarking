#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# One command → answers. Runs the memory benchmark for every listed gateway on THIS box (same mock,
# same load, same pin), one at a time, then regenerates the chart from results/. Nothing to debug.
#
#   BUSBAR_BIN=~/busbar bench/run-all.sh                       # all gateways
#   BUSBAR_BIN=~/busbar bench/run-all.sh busbar litellm-rust   # a subset
#
# Each gateway is a drop-in dir under gateways/ (see gateways/README.md). Bifrost needs Docker;
# LiteLLM (Rust/Python) build from source/pip on first run.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATEWAYS=("$@")
# Default field = every gateway with a manifest under gateways/ (each dir is a self-contained drop-in;
# add a dir → it runs here, delete it → it's gone). Discovered, not hard-coded, so the list can never
# drift from what's on disk. Alphabetical (ls), so no gateway is seated first. Envoy AI Gateway is
# intentionally absent — it is Kubernetes-native (needs a cluster), out of scope for this single-box harness.
if [ ${#GATEWAYS[@]} -eq 0 ]; then
  for d in "$HERE"/gateways/*/gateway.sh; do GATEWAYS+=("$(basename "$(dirname "$d")")"); done
fi
log(){ echo "[$(date +%H:%M:%S)] $*"; }

# Between-suite port drain. Each suite's EXIT trap kills the gateway; the next suite relaunches the
# SAME gateway on the SAME port seconds later. A gateway that binds WITHOUT SO_REUSEADDR (busbar,
# and others) then hits 'address already in use' because the prior listener sits in TIME_WAIT for up
# to a minute - the suite records served=false, a FALSE failure verdict for a working gateway. A
# connect probe can't see TIME_WAIT, so we test actual BINDABILITY: a plain bind (no SO_REUSEADDR)
# fails with EADDRINUSE exactly while the port is unbindable, which is the condition we must clear.
# Wait for GW_PORT and its two derived admin ports (matrix uses +1, governed uses +2) to be bindable.
wait_ports_bindable(){ # port [port...]
  local p; for p in "$@"; do
    local i ok=0
    for i in $(seq 1 90); do
      if python3 - "$p" <<'PY' 2>/dev/null
import socket,sys
s=socket.socket(socket.AF_INET,socket.SOCK_STREAM)
# NO SO_REUSEADDR: bind fails while a prior listener is still in TIME_WAIT, which is what we wait out.
try:
    s.bind(("127.0.0.1",int(sys.argv[1]))); s.close(); sys.exit(0)
except OSError:
    sys.exit(1)
PY
      then ok=1; break; fi
      sleep 1
    done
    [ "$ok" = 1 ] || log "WARNING port $p still not bindable after 90s - launching anyway"
  done
}

# Read a manifest's GW_PORT without running its launch/build logic (the assignment is a plain top-level
# line). Falls back to 8080 (the harness default) if the manifest doesn't set one literally.
manifest_gw_port(){ # gateway
  local port; port=$(grep -m1 -E '^GW_PORT=' "$HERE/gateways/$1/gateway.sh" 2>/dev/null | cut -d= -f2 | tr -dc '0-9')
  echo "${port:-8080}"
}

# Which suites to run (headline first): perf = latency + RPS ceiling; memory = idle/peak RSS.
# stream = SSE added-TTFT / inter-frame overhead / streams sustained; streamcpu = CPU-bound streaming
# relay throughput (unpaced frames/sec per gateway, the streaming analogue of perf's max-proxy RPS);
# governed = the same latency + sustained-RPS run with native key/limit governance active, paired
# against a plain run in one JSON; xlate = protocol translation (anthropic client -> openai upstream)
# added latency + sustained RPS. Opt in with SUITES="perf memory stream streamcpu governed xlate"
# (see stream/run.sh, streamcpu/run.sh, governed/run.sh, xlate/run.sh).
SUITES="${SUITES:-perf memory stream streamcpu xlate governed matrix}"
for gw in "${GATEWAYS[@]}"; do
  [ -f "$HERE/gateways/$gw/gateway.sh" ] || { log "skip unknown gateway '$gw'"; continue; }
  gwport="$(manifest_gw_port "$gw")"
  first=1
  for suite in $SUITES; do
    # Between suites (not before the first), wait for the prior suite's listener to fully release the
    # port so the next suite's launch can bind - avoids the TIME_WAIT false 'did not serve'. Cover the
    # data port and the two admin ports suites derive from it (matrix GW_PORT+1, governed GW_PORT+2).
    if [ "$first" != 1 ]; then
      log "$gw: waiting for ports $gwport/$((gwport+1))/$((gwport+2)) to be bindable before $suite"
      wait_ports_bindable "$gwport" "$((gwport+1))" "$((gwport+2))"
    fi
    first=0
    log "══ $gw · $suite ══"
    GATEWAY="$gw" bash "$HERE/$suite/run.sh" || log "$gw $suite run failed (continuing)"
  done
done

log "regenerating charts"
if command -v python3 >/dev/null && python3 -c 'import matplotlib' 2>/dev/null; then
  python3 "$HERE/charts.py"
else
  log "matplotlib not present — results/memory/*.json written; run 'pip install matplotlib && python3 bench/charts.py' to draw"
fi
log "done — results/ + results/memory_rss.png"
