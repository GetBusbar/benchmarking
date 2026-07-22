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

# Which suites to run (headline first): perf = latency + RPS ceiling; memory = idle/peak RSS.
# stream = SSE added-TTFT / inter-frame overhead / streams sustained; governed = the same latency +
# sustained-RPS run with native key/limit governance active, paired against a plain run in one JSON.
# Opt in with SUITES="perf memory stream governed" (see stream/run.sh, governed/run.sh).
SUITES="${SUITES:-perf memory}"
for gw in "${GATEWAYS[@]}"; do
  [ -f "$HERE/gateways/$gw/gateway.sh" ] || { log "skip unknown gateway '$gw'"; continue; }
  for suite in $SUITES; do
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
