#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Bifrost (maximhq/bifrost, docker), its documented pool config.
GW_KIND=docker
GW_PORT=8080
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=sk-dummy
# BIFROST_IMAGE comes from gateways/versions.env — override there.
BIFROST_IMAGE="${BIFROST_IMAGE:-maximhq/bifrost:v1.6.4}"

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$BIFROST_IMAGE" 2>/dev/null)
  echo "${BIFROST_IMAGE}${dg:+ (@${dg##*@})}"
}

gw_build() {
  # Generate Bifrost's config against the runner's actual mock port (the openai provider's base_url is
  # the mock; the model gpt-4o-mini is registered so Bifrost can auto-resolve it to this provider —
  # without a registered provider Bifrost returns "could not auto resolve a provider").
  mkdir -p "$GW_DIR/bfdata"
  cat > "$GW_DIR/bfdata/config.json" <<JSON
{
  "providers": {
    "openai": {
      "keys": [{ "value": "sk-dummy", "models": ["gpt-4o-mini"], "weight": 1 }],
      "network_config": { "base_url": "http://127.0.0.1:$MOCK_PORT" },
      "concurrency_and_buffer_size": { "initial_pool_size": 15000, "buffer_size": 20000 }
    }
  }
}
JSON
  sudo docker pull "$BIFROST_IMAGE" >/dev/null 2>&1 || true
}

gw_launch() {
  sudo docker rm -f bifrost >/dev/null 2>&1; sleep 1
  # GOMAXPROCS = number of pinned cores (e.g. 0-3 → 4), not the last core index.
  local ncore=$(( ${CORES##*-} - ${CORES%%-*} + 1 ))
  sudo docker run -d --name bifrost --network host --cpuset-cpus="$CORES" \
    -e GOMAXPROCS="$ncore" -v "$GW_DIR/bfdata:/app/data" "$BIFROST_IMAGE" >"$GW_DIR/launch.log" 2>&1 || true
}

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=bifrost --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 bifrost 2>&1
}

gw_rss() {
  local m; m=$(sudo docker stats --no-stream --format '{{.MemUsage}}' bifrost 2>/dev/null | awk '{print $1}')
  case "$m" in
    *GiB) awk -v x="${m%GiB}" 'BEGIN{printf "%.1f", x*1024}' ;;
    *MiB) echo "${m%MiB}" ;;
    *) echo 0 ;;
  esac
}

gw_stop() { sudo docker rm -f bifrost >/dev/null 2>&1; }
