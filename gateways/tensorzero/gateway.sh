#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: TensorZero (tensorzero/gateway, Rust, docker).
#
# OpenAI-compatible Rust gateway. The multiarch image ships linux/arm64, so it runs natively on
# Graviton. Pure-proxy mode: observability OFF (no ClickHouse/Postgres), api_key_location = none, one
# model whose provider base_url is the mock. TENSORZERO_IMAGE is pinned in gateways/versions.env.
GW_KIND=docker
GW_PORT=3000
GW_PATH=/openai/v1/chat/completions
GW_MODEL=tensorzero::model_name::mock
GW_AUTH=dummy
TENSORZERO_IMAGE="${TENSORZERO_IMAGE:-tensorzero/gateway:2026.6.0}"

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$TENSORZERO_IMAGE" 2>/dev/null)
  echo "${TENSORZERO_IMAGE}${dg:+ (@${dg##*@})}"
}

gw_build() {
  mkdir -p "$GW_DIR/config"
  cat > "$GW_DIR/config/tensorzero.toml" <<TOML
[gateway.observability]
enabled = false

[models.mock]
routing = ["mock"]

[models.mock.providers.mock]
type = "openai"
api_base = "http://127.0.0.1:$MOCK_PORT/v1"
model_name = "gpt-4o-mini"
api_key_location = "none"
TOML
  sudo docker pull "$TENSORZERO_IMAGE" >/dev/null 2>&1 || true
}

gw_launch() {
  sudo docker rm -f tensorzero-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name tensorzero-bench --network host --cpuset-cpus="$CORES" \
    -e TENSORZERO_DISABLE_PSEUDONYMOUS_USAGE_ANALYTICS=1 \
    -v "$GW_DIR/config:/app/config:ro" \
    "$TENSORZERO_IMAGE" --config-file config/tensorzero.toml >"$GW_DIR/launch.log" 2>&1 || true
}

gw_rss() {
  local m; m=$(sudo docker stats --no-stream --format '{{.MemUsage}}' tensorzero-bench 2>/dev/null | awk '{print $1}')
  case "$m" in *GiB) awk -v x="${m%GiB}" 'BEGIN{printf "%.1f", x*1024}';; *MiB) echo "${m%MiB}";; *) echo 0;; esac
}

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=tensorzero-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 tensorzero-bench 2>&1
}

gw_stop() { sudo docker rm -f tensorzero-bench >/dev/null 2>&1; }
