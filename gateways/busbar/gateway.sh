#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Busbar.
#
# Self-provisions like every other gateway — pulls the RELEASED image (BUSBAR_IMAGE in
# versions.env, e.g. getbusbar/busbar:1.4.1), extracts the static binary, and runs it as a native
# process so we measure the real process RSS/latency. Override with BUSBAR_BIN to point at a local
# working-tree build instead. Config: token auth, one model to the mock — no special setup.
GW_KIND=native
GW_PORT=8080
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=bench-token

gw_build() {
  if [ -z "${BUSBAR_BIN:-}" ]; then
    command -v docker >/dev/null || { echo "[busbar] need docker, or set BUSBAR_BIN"; return 1; }
    docker pull "${BUSBAR_IMAGE}" >/dev/null 2>&1 || { echo "[busbar] cannot pull ${BUSBAR_IMAGE}"; return 1; }
    docker rm -f busbar-extract >/dev/null 2>&1
    docker create --name busbar-extract "${BUSBAR_IMAGE}" >/dev/null 2>&1
    docker cp busbar-extract:/busbar "$GW_DIR/busbar" >/dev/null 2>&1 \
      || { echo "[busbar] no /busbar in ${BUSBAR_IMAGE}"; docker rm -f busbar-extract >/dev/null 2>&1; return 1; }
    docker rm -f busbar-extract >/dev/null 2>&1
    chmod +x "$GW_DIR/busbar"; BUSBAR_BIN="$GW_DIR/busbar"
  fi
}
gw_version() { local v; v="$("$BUSBAR_BIN" --version 2>/dev/null | head -1)"; echo "${v:-${BUSBAR_IMAGE:-busbar}}"; }

gw_launch() {
  cat > "$GW_DIR/config.gen.yaml" <<YAML
listen: "127.0.0.1:$GW_PORT"
observability:
  emit_server_timing: true
auth:
  chain: [tokens]
  upstream_credentials: own
  client_tokens:
    - "bench-token"
providers:
  mock:
    api_key_env: BENCH_MOCK_KEY
models:
  gpt-4o-mini:
    provider: mock
    max_concurrent: 8000
    max_requests: -1
pools:
  bench-pool:
    members:
      - target: gpt-4o-mini
YAML
  cat > "$GW_DIR/providers.gen.yaml" <<YAML
mock:
  protocol: openai
  base_url: http://127.0.0.1:$MOCK_PORT
  error_map: {}
YAML
  pkill -x busbar 2>/dev/null; sleep 1
  setsid taskset -c "$CORES" env \
    BUSBAR_WORKER_THREADS="$(( ${CORES##*-} + 1 ))" \
    BUSBAR_PROVIDERS="$GW_DIR/providers.gen.yaml" \
    BUSBAR_CONFIG="$GW_DIR/config.gen.yaml" \
    BENCH_MOCK_KEY=x \
    "$BUSBAR_BIN" </dev/null >/tmp/busbar.bench.log 2>&1 &
}

gw_rss() { awk '/VmRSS/{printf "%.1f", $2/1024}' "/proc/$(pgrep -x busbar)/status" 2>/dev/null; }
gw_stop() { pkill -x busbar 2>/dev/null; }
