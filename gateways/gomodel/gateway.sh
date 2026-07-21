#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: GoModel (ENTERPILOT/GOModel, Go, docker).
#
# OpenAI + Anthropic-compatible Go gateway. We override the openai provider's base URL to the mock
# via OPENAI_BASE_URL, so /v1/chat/completions forwards there. Left unprotected (GOMODEL_MASTER_KEY
# unset) for a pure proxy-overhead measurement — the default posture. Image pinned in
# gateways/versions.env; the resolved tag is recorded in the result.
GW_KIND=docker
GW_PORT=8080
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=dummy

gw_version() { echo "${GOMODEL_IMAGE:-enterpilot/gomodel:0.1.55}"; }

gw_build() {
  sudo docker pull "${GOMODEL_IMAGE:-enterpilot/gomodel:0.1.55}" >/dev/null 2>&1 || true
}

gw_launch() {
  sudo docker rm -f gomodel-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name gomodel-bench --network host --cpuset-cpus="$CORES" \
    -e PORT="$GW_PORT" \
    -e OPENAI_BASE_URL="http://127.0.0.1:$MOCK_PORT/v1" \
    -e OPENAI_API_KEY=dummy \
    -e MODELS_ENABLED_BY_DEFAULT=true \
    -e STORAGE_TYPE=sqlite \
    -e LOGGING_ENABLED=false \
    "${GOMODEL_IMAGE:-enterpilot/gomodel:0.1.55}" >/dev/null 2>&1
}

gw_rss() {
  local m; m=$(sudo docker stats --no-stream --format '{{.MemUsage}}' gomodel-bench 2>/dev/null | awk '{print $1}')
  case "$m" in
    *GiB) awk -v x="${m%GiB}" 'BEGIN{printf "%.1f", x*1024}' ;;
    *MiB) echo "${m%MiB}" ;;
    *) echo 0 ;;
  esac
}

gw_stop() { sudo docker rm -f gomodel-bench >/dev/null 2>&1; }
