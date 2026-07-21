#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Helicone AI Gateway (helicone/ai-gateway, Rust, docker).
#
# OpenAI-compatible router. We override the `openai` provider's base-url to the mock and expose a
# `default` router, so /router/default/chat/completions forwards to the mock. HELICONE_IMAGE is
# pinned in gateways/versions.env. Runs without the Helicone control plane (no key) for a pure
# proxy-overhead measurement.
GW_KIND=docker
GW_PORT=8787
GW_PATH=/router/default/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=dummy

gw_version() { echo "${HELICONE_IMAGE:-helicone/ai-gateway:latest}"; }

gw_build() {
  cat > "$GW_DIR/config.gen.yaml" <<YAML
routers:
  default:
    load-balance:
      chat:
        strategy: latency
        targets:
          - openai
providers:
  openai:
    base-url: http://127.0.0.1:$MOCK_PORT
    models:
      - $GW_MODEL
YAML
  sudo docker pull "${HELICONE_IMAGE:-helicone/ai-gateway:latest}" >/dev/null 2>&1 || true
}

gw_launch() {
  sudo docker rm -f helicone-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name helicone-bench --network host --cpuset-cpus="$CORES" \
    -e AI_GATEWAY__SERVER__ADDRESS=0.0.0.0 \
    -e OPENAI_API_KEY=dummy \
    -v "$GW_DIR/config.gen.yaml:/config.yaml:ro" \
    "${HELICONE_IMAGE:-helicone/ai-gateway:latest}" --config /config.yaml >/dev/null 2>&1
}

gw_rss() {
  local m; m=$(sudo docker stats --no-stream --format '{{.MemUsage}}' helicone-bench 2>/dev/null | awk '{print $1}')
  case "$m" in
    *GiB) awk -v x="${m%GiB}" 'BEGIN{printf "%.1f", x*1024}' ;;
    *MiB) echo "${m%MiB}" ;;
    *) echo 0 ;;
  esac
}

gw_stop() { sudo docker rm -f helicone-bench >/dev/null 2>&1; }
