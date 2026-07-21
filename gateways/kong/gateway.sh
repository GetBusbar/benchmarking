#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Kong Gateway + the ai-proxy plugin (DB-less, docker).
#
# Kong's ai-proxy plugin fronts an OpenAI-shaped /v1/chat/completions and forwards to an upstream
# LLM; `model.options.upstream_url` overrides that upstream, so we point it straight at the mock.
# DB-less declarative config, generated in gw_launch against the runner's mock port. KONG_IMAGE is
# pinned in gateways/versions.env.
GW_KIND=docker
GW_PORT=8080
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=dummy

gw_version() { echo "${KONG_IMAGE:-kong:3.8}"; }

gw_build() {
  cat > "$GW_DIR/kong.gen.yml" <<YAML
_format_version: "3.0"
services:
  - name: llm
    url: http://127.0.0.1:1
    routes:
      - name: chat
        paths: ["/v1/chat/completions"]
        strip_path: false
    plugins:
      - name: ai-proxy
        config:
          route_type: llm/v1/chat
          auth:
            header_name: Authorization
            header_value: "Bearer dummy"
          model:
            provider: openai
            name: $GW_MODEL
            options:
              upstream_url: "http://127.0.0.1:$MOCK_PORT/v1/chat/completions"
YAML
  sudo docker pull "${KONG_IMAGE:-kong:3.8}" >/dev/null 2>&1 || true
}

gw_launch() {
  sudo docker rm -f kong-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name kong-bench --network host --cpuset-cpus="$CORES" \
    -e KONG_DATABASE=off \
    -e KONG_DECLARATIVE_CONFIG=/kong/kong.yml \
    -e "KONG_PROXY_LISTEN=0.0.0.0:$GW_PORT" \
    -e "KONG_ADMIN_LISTEN=off" \
    -v "$GW_DIR/kong.gen.yml:/kong/kong.yml:ro" \
    "${KONG_IMAGE:-kong:3.8}" >/dev/null 2>&1
}

gw_rss() {
  local m; m=$(sudo docker stats --no-stream --format '{{.MemUsage}}' kong-bench 2>/dev/null | awk '{print $1}')
  case "$m" in
    *GiB) awk -v x="${m%GiB}" 'BEGIN{printf "%.1f", x*1024}' ;;
    *MiB) echo "${m%MiB}" ;;
    *) echo 0 ;;
  esac
}

gw_stop() { sudo docker rm -f kong-bench >/dev/null 2>&1; }
