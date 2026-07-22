#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: TensorZero (tensorzero/gateway, Rust, docker).
#
# OpenAI-compatible Rust gateway. The multiarch image ships linux/arm64, so it runs natively on
# Graviton. Pure-proxy mode: observability OFF (no ClickHouse/Postgres), api_key_location = none, one
# model whose provider base_url is the mock. TENSORZERO_IMAGE is pinned in gateways/versions.env.
GW_KIND=docker
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="TensorZero"                      # label in charts + report tables
GW_LANG=Rust                            # implementation language → bar color bucket
GW_CLASS="Model gateway"   # the project's OWN self-description (docs: 'the TensorZero Gateway is a high-performance model gateway'), not our editorial
GW_REPO=https://github.com/tensorzero/tensorzero   # linked from the gateway name in the report table
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

gw_rss() { container_rss_mib tensorzero-bench; }  # summed process-tree VmRSS (same method as native gateways)
gw_hwm() { container_hwm_mib tensorzero-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=tensorzero-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 tensorzero-bench 2>&1
}

gw_stop() { sudo docker rm -f tensorzero-bench >/dev/null 2>&1; }

# matrix suite (6x6): no gw_matrix_egress hook is defined for this manifest, so every egress
# column beyond the default upstream renders "not configurable" (neutral, distinct from
# tried-and-failed). Reason: the model provider here is type = "openai" with an api_base override.
# TensorZero's config has other provider types (anthropic and more), but none has been verified
# against the recording mock from this harness, so wiring them blind would risk false tried-and-
# failed reds.
