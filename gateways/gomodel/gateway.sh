#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: GoModel (ENTERPILOT/GOModel, Go, docker).
#
# OpenAI + Anthropic-compatible Go gateway. We override the openai provider's base URL to the mock
# via OPENAI_BASE_URL, so /v1/chat/completions forwards there. Left unprotected (GOMODEL_MASTER_KEY
# unset) for a pure proxy-overhead measurement — the default posture. Image pinned in
# gateways/versions.env; the resolved tag is recorded in the result.
GW_KIND=docker
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="GoModel"                      # label in charts + report tables
GW_LANG=Go                            # implementation language → bar color bucket
GW_REPO=https://github.com/ENTERPILOT/GOModel   # linked from the gateway name in the report table
GW_PORT=8080
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=dummy

GOMODEL_IMAGE="${GOMODEL_IMAGE:-enterpilot/gomodel:0.1.55}"

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$GOMODEL_IMAGE" 2>/dev/null)
  echo "${GOMODEL_IMAGE}${dg:+ (@${dg##*@})}"
}

gw_build() {
  sudo docker pull "$GOMODEL_IMAGE" >/dev/null 2>&1 || true
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
    "$GOMODEL_IMAGE" >"$GW_DIR/launch.log" 2>&1 || true
}

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=gomodel-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 gomodel-bench 2>&1
}

gw_rss() { container_rss_mib gomodel-bench; }  # summed process-tree VmRSS (same method as native gateways)

gw_stop() { sudo docker rm -f gomodel-bench >/dev/null 2>&1; }
