#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Apache APISIX + the ai-proxy plugin (DB-less standalone, docker).
#
# APISIX runs in data-plane/standalone mode (no etcd): routes are read from conf/apisix.yaml. The
# ai-proxy plugin fronts an OpenAI-shaped route and forwards to the mock via override.endpoint. Access
# logging off, worker_processes = pinned core count, no observability plugins → pure proxy overhead.
# APISIX_IMAGE is pinned in gateways/versions.env.
GW_KIND=docker
GW_PORT=9080
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=sk-fake-benchmark-key
APISIX_IMAGE="${APISIX_IMAGE:-apache/apisix:3.17.0-debian}"

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$APISIX_IMAGE" 2>/dev/null)
  echo "${APISIX_IMAGE}${dg:+ (@${dg##*@})}"
}

gw_build() {
  local ncore=$(( ${CORES##*-} - ${CORES%%-*} + 1 ))
  cat > "$GW_DIR/config.gen.yaml" <<YAML
deployment:
  role: data_plane
  role_data_plane:
    config_provider: yaml
apisix:
  node_listen:
    - $GW_PORT
  enable_admin: false
nginx_config:
  worker_processes: $ncore
  http:
    enable_access_log: false
YAML
  # The ai-proxy route: upstream is owned by the plugin via override.endpoint (full mock URL). The
  # trailing #END marker is REQUIRED by the yaml config provider.
  cat > "$GW_DIR/apisix.gen.yaml" <<YAML
routes:
  - id: ai-proxy-bench
    uri: $GW_PATH
    methods:
      - POST
    plugins:
      ai-proxy:
        provider: openai-compatible
        auth:
          header:
            Authorization: "Bearer $GW_AUTH"
        options:
          model: $GW_MODEL
        override:
          endpoint: "http://127.0.0.1:$MOCK_PORT$GW_PATH"
#END
YAML
  sudo docker pull "$APISIX_IMAGE" >/dev/null 2>&1 || true
}

gw_launch() {
  sudo docker rm -f apisix-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name apisix-bench --network host --cpuset-cpus="$CORES" \
    -v "$GW_DIR/config.gen.yaml:/usr/local/apisix/conf/config.yaml:ro" \
    -v "$GW_DIR/apisix.gen.yaml:/usr/local/apisix/conf/apisix.yaml:ro" \
    "$APISIX_IMAGE" >"$GW_DIR/launch.log" 2>&1 || true
}

gw_rss() { container_rss_mib apisix-bench; }  # summed process-tree VmRSS (same method as native gateways)

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=apisix-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 apisix-bench 2>&1
}

gw_stop() { sudo docker rm -f apisix-bench >/dev/null 2>&1; }
