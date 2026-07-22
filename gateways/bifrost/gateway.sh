#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Bifrost (maximhq/bifrost, docker), its documented pool config.
GW_KIND=docker
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="Bifrost"                      # label in charts + report tables
GW_LANG=Go                            # implementation language → bar color bucket
GW_CLASS="LLM gateway"   # the project's OWN self-description (README: 'The fastest LLM gateway'), not our editorial
GW_REPO=https://github.com/maximhq/bifrost   # linked from the gateway name in the report table
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
  _bifrost_write_config openai gpt-4o-mini
  sudo docker pull "$BIFROST_IMAGE" >/dev/null 2>&1 || true
}

# _bifrost_write_config <provider> <model>: emit config.json wiring one provider whose base_url is
# the mock. Bifrost auto-resolves the provider from the request's model name, so GW_MODEL is set to a
# model registered under <provider>; the provider's translation emits that dialect's native upstream
# shape to the mock (network_config.base_url is honoured at runtime for openai/anthropic/cohere/gemini).
_bifrost_write_config() {
  mkdir -p "$GW_DIR/bfdata"
  cat > "$GW_DIR/bfdata/config.json" <<JSON
{
  "providers": {
    "$1": {
      "keys": [{ "value": "sk-dummy", "models": ["$2"], "weight": 1 }],
      "network_config": { "base_url": "http://127.0.0.1:$MOCK_PORT" }
    }
  }
}
JSON
}

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock. Bifrost v1.6.4's provider keys with a RUNTIME-honoured network_config.base_url are
# {openai, anthropic, cohere, gemini, mistral, ollama, vllm, sgl} (core/schemas/bifrost.go,
# provider.go). The OpenAI ingress fans out to the resolved provider's NATIVE upstream shape:
# anthropic -> /v1/messages, cohere -> /v2/chat, gemini -> /models/<m>:generateContent. Bifrost also
# has a native Responses INBOUND surface (responses-ingress -> openai provider -> /v1/responses). So
# the capable cells are openai-ingress into {openai, anthropic, gemini, cohere} plus the
# openai-responses diagonal. NOT declared: bedrock (the bedrock key IS native Converse but its host
# is hardcoded to bedrock-runtime.<region>.amazonaws.com - NetworkConfig.BaseURL is never read at
# runtime, so the mock is unreachable) - grey with the cited reason. openai-chat -> responses is not
# a declared bridge, so that off-diagonal is 0.
# Evidence: core/schemas/bifrost.go (key enum), provider.go (BaseURL-overridable set),
# bedrock/bedrock.go (hardcoded host, no BaseURL), tag transports/v1.6.4. Wired-pending-field-verify.
GW_MATRIX_CAP="
101110
010000
000000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="Bifrost v1.6.4 ignores network_config.base_url for the bedrock provider (host hardcoded to bedrock-runtime.<region>.amazonaws.com), so a custom Bedrock upstream is unreachable; that cell is grey by that capability limit (bedrock/bedrock.go)"
GW_MATRIX_EGRESS="openai openai-responses anthropic gemini cohere"
gw_matrix_egress() {
  case "$1" in
    openai)           GW_MODEL=gpt-4o-mini;             _bifrost_write_config openai    gpt-4o-mini;;
    openai-responses) GW_MODEL=gpt-4o-mini;             _bifrost_write_config openai    gpt-4o-mini;;
    anthropic)        GW_MODEL=claude-3-5-sonnet-20241022; _bifrost_write_config anthropic claude-3-5-sonnet-20241022;;
    gemini)           GW_MODEL=gemini-1.5-pro;          _bifrost_write_config gemini    gemini-1.5-pro;;
    cohere)           GW_MODEL=command-r-plus;          _bifrost_write_config cohere    command-r-plus;;
    *) return 1;;
  esac
  gw_launch
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
gw_hwm() { container_hwm_mib bifrost; }  # summed process-tree VmHWM (kernel high-water mark)

gw_stop() { sudo docker rm -f bifrost >/dev/null 2>&1; }

# gw_matrix_egress + the declared capability matrix are defined above (in gw_build). The non-openai
# egress columns are wired-pending-field-verification; the EC2 field run turns each declared-1 cell
# green or red. Every grey cell is a cited capability limit.
