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
  # Run Bifrost on its DEFAULT pool sizing — we don't inject a throughput-tuned pool (its own bench
  # config uses initial_pool_size 15000, which is what inflates memory under sustained load; scoring a
  # competitor's throughput-tuned config on memory isn't fair). Just the provider + mock base_url.
  mkdir -p "$GW_DIR/bfdata"
  cat > "$GW_DIR/bfdata/config.json" <<JSON
{
  "providers": {
    "openai": {
      "keys": [{ "value": "sk-dummy", "models": ["gpt-4o-mini"], "weight": 1 }],
      "network_config": { "base_url": "http://127.0.0.1:$MOCK_PORT" }
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

gw_rss() { container_rss_mib bifrost; }  # summed process-tree VmRSS (same method as native gateways)

gw_stop() { sudo docker rm -f bifrost >/dev/null 2>&1; }
