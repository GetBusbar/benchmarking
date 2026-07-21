#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: agentgateway (agentgateway/agentgateway, Rust data plane, docker).
#
# OpenAI-compatible bare proxy: the `ai` backend's openAI provider takes hostOverride + pathOverride
# to point the upstream at the mock. No backendAuth (no key added) and no backendTLS (plain HTTP to the
# mock). Tracing is off unless an OTLP endpoint is set; metrics are pull-only; admin/stats pinned to
# loopback and RUST_LOG=error → clean overhead. AGENTGATEWAY_IMAGE is pinned in gateways/versions.env.
GW_KIND=docker
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="agentgateway"                      # label in charts + report tables
GW_LANG=Rust                            # implementation language → bar color bucket
GW_REPO=https://github.com/agentgateway/agentgateway   # linked from the gateway name in the report table
GW_PORT=8080
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=dummy
AGENTGATEWAY_IMAGE="${AGENTGATEWAY_IMAGE:-ghcr.io/agentgateway/agentgateway:v1.3.1}"

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$AGENTGATEWAY_IMAGE" 2>/dev/null)
  echo "${AGENTGATEWAY_IMAGE}${dg:+ (@${dg##*@})}"
}

gw_build() {
  cat > "$GW_DIR/config.gen.yaml" <<YAML
binds:
- port: $GW_PORT
  listeners:
  - name: llm
    protocol: HTTP
    routes:
    - backends:
      - ai:
          name: openai
          hostOverride: "127.0.0.1:$MOCK_PORT"
          pathOverride: $GW_PATH
          provider:
            openAI:
              model: $GW_MODEL
YAML
  sudo docker pull "$AGENTGATEWAY_IMAGE" >/dev/null 2>&1 || true
}

gw_launch() {
  sudo docker rm -f agentgateway-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name agentgateway-bench --network host --cpuset-cpus="$CORES" \
    -e ADMIN_ADDR=127.0.0.1:15000 -e STATS_ADDR=127.0.0.1:15020 -e RUST_LOG=error \
    -v "$GW_DIR/config.gen.yaml:/config.yaml:ro" \
    "$AGENTGATEWAY_IMAGE" -f /config.yaml >"$GW_DIR/launch.log" 2>&1 || true
}

gw_rss() { container_rss_mib agentgateway-bench; }  # summed process-tree VmRSS (same method as native)

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=agentgateway-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 agentgateway-bench 2>&1
}

gw_stop() { sudo docker rm -f agentgateway-bench >/dev/null 2>&1; }
