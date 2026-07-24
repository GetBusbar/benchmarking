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
# Bifrost's Anthropic-format ingress is the DROP-IN integration prefix, not a bare /v1/messages:
# transports/bifrost-http/integrations/anthropic.go@v1.6.4 mounts POST /anthropic/v1/messages
# (a bare /v1/messages hits the dashboard's GET catch-all and 405s - which our xlate lane used to
# publish as a Bifrost translation failure; that was OUR wrong path, verified locally: the correct
# path translates and returns an Anthropic-shaped envelope through the openai provider).
GW_ANTHROPIC_PATH=/anthropic/v1/messages
# BIFROST_IMAGE comes from gateways/versions.env — override there.
BIFROST_IMAGE="${BIFROST_IMAGE:-maximhq/bifrost:v1.6.4}"

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$BIFROST_IMAGE" 2>/dev/null)
  echo "${BIFROST_IMAGE}${dg:+ (@${dg##*@})}"
}

gw_build() {
  # Generate Bifrost's ONE canonical config against the runner's actual mock port. Bifrost auto-resolves
  # the provider from the request's model name, so ALL mock-reachable declared-egress providers are
  # wired simultaneously (openai, anthropic, gemini, cohere) with their base_url pointed at the mock —
  # the perf/memory/throughput/stream lanes and the matrix all run this SAME multi-provider config, the
  # matrix just names a different model. (bedrock is declared UNTESTABLE, not wired: v1.6.4 hardcodes
  # the bedrock-runtime host, network_config.base_url can't redirect it — see GW_MATRIX_UNTESTABLE.)
  # Run Bifrost on its DEFAULT pool sizing — we don't inject a throughput-tuned pool (its own bench
  # config uses initial_pool_size 15000, which is what inflates memory under sustained load; scoring a
  # competitor's throughput-tuned config on memory isn't fair). Just the providers + mock base_url.
  _bifrost_write_config
  sudo docker pull "$BIFROST_IMAGE" >/dev/null 2>&1 || true
}

# _bifrost_write_config: emit config.json wiring every mock-reachable declared provider, each whose
# base_url is the mock. Bifrost auto-resolves the provider from the request's model name (the model
# lists below register which names map to which provider), and each provider's translation emits that
# dialect's native upstream shape to the mock (network_config.base_url is honoured at runtime for
# openai/anthropic/cohere/gemini — provider.go BaseURL-overridable set). sk-dummy is the required
# provider key (Bifrost needs a key entry to register a provider); never a live secret.
_bifrost_write_config() {
  mkdir -p "$GW_DIR/bfdata"
  cat > "$GW_DIR/bfdata/config.json" <<JSON
{
  "providers": {
    "openai": {
      "keys": [{ "value": "sk-dummy", "models": ["gpt-4o-mini"], "weight": 1 }],
      "network_config": { "base_url": "http://127.0.0.1:$MOCK_PORT" }
    },
    "anthropic": {
      "keys": [{ "value": "sk-dummy", "models": ["claude-3-5-sonnet-20241022"], "weight": 1 }],
      "network_config": { "base_url": "http://127.0.0.1:$MOCK_PORT" }
    },
    "gemini": {
      "keys": [{ "value": "sk-dummy", "models": ["gemini-1.5-pro"], "weight": 1 }],
      "network_config": { "base_url": "http://127.0.0.1:$MOCK_PORT" }
    },
    "cohere": {
      "keys": [{ "value": "sk-dummy", "models": ["command-r-plus"], "weight": 1 }],
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
# has a native Responses INBOUND surface (responses-ingress -> openai provider -> /v1/responses),
# and an Anthropic-Messages INBOUND surface at /anthropic/v1/messages that converts to a Bifrost
# ResponsesRequest and hits the routed provider's Responses API - so anthropic-ingress ->
# openai-responses-egress is a real translation (verified locally at v1.6.4: anthropic body in,
# anthropic envelope out, mock records the openai-responses dialect). It is NOT declared into the
# openai-chat egress column because the translation targets the Responses upstream endpoint, not
# chat/completions. So the capable cells are openai-ingress into {openai, anthropic, gemini,
# cohere}, the openai-responses diagonal, and anthropic-ingress -> openai-responses. openai-chat ->
# responses is not a declared bridge, so that off-diagonal is 0.
# Evidence: core/schemas/bifrost.go (key enum), provider.go (BaseURL-overridable set),
# transports/bifrost-http/integrations/anthropic.go (drop-in Messages ingress), tag
# transports/v1.6.4, local verification runs.
GW_MATRIX_CAP="
101110
010000
010000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="Bifrost v1.6.4 has no openai-chat -> responses bridge and no gemini/cohere/bedrock-format ingress; undeclared cells are grey by that capability limit"
# Bedrock is NOT incapability: Bifrost speaks Bedrock Converse in production, but at v1.6.4 the
# bedrock provider hardcodes bedrock-runtime.<region>.amazonaws.com (core/providers/bedrock/
# bedrock.go builds the request URL from region only; network_config.base_url is consulted for
# headers/TLS/proxy/timeouts, never the host), so this rig's localhost mock cannot stand in.
GW_MATRIX_UNTESTABLE="openai/bedrock"
GW_MATRIX_UNTESTABLE_NOTE="Bifrost v1.6.4 hardcodes the Bedrock host (bedrock-runtime.<region>.amazonaws.com, core/providers/bedrock/bedrock.go; network_config.base_url never overrides it), so the harness mock cannot stand in for the upstream; Bifrost does serve Bedrock Converse in production"
GW_MATRIX_EGRESS="openai openai-responses anthropic gemini cohere"
gw_matrix_egress() {
  # All egress providers are already wired in the ONE config (see _bifrost_write_config); Bifrost picks
  # the provider from the request model name, so the matrix only flips GW_MODEL — no config rewrite. The
  # relaunch runs the identical all-providers config.
  case "$1" in
    openai|openai-responses) GW_MODEL=gpt-4o-mini;;
    anthropic)               GW_MODEL=claude-3-5-sonnet-20241022;;
    gemini)                  GW_MODEL=gemini-1.5-pro;;
    cohere)                  GW_MODEL=command-r-plus;;
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

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config this gateway launches with — the rendered config.json
# exactly as mounted at /app/data (read from the file gw_build produced so it can never drift; falls
# back to rendering it if not present yet). The provider keys are the dummy sk-dummy Bifrost requires to
# register a provider — never a live secret. OOTB posture: DEFAULT pool sizing (no throughput-tuned
# initial_pool_size injected), all four mock-reachable providers wired; the only deviations are the
# permitted ones — provider base_urls → mock and dummy keys. GOMAXPROCS in the launch env is the CPU-
# pinning run-mechanic (= pinned core count). No feature strips, no perf/pool tuning.
gw_config() {
  local cfg="$GW_DIR/bfdata/config.json"
  echo "# ── /app/data/config.json (rendered; mounted read-write, Bifrost's data dir) ──"
  [ -f "$cfg" ] || _bifrost_write_config
  cat "$cfg"
  echo
  echo "# ── launch env (non-secret; GOMAXPROCS = pinned core count, CPU-pinning run-mechanic) ──"
  local ncore=$(( ${CORES##*-} - ${CORES%%-*} + 1 ))
  echo "GOMAXPROCS=$ncore"
}

gw_rss() { container_rss_mib bifrost; }  # summed process-tree VmRSS (same method as native gateways)
gw_hwm() { container_hwm_mib bifrost; }  # summed process-tree VmHWM (kernel high-water mark)

gw_stop() { sudo docker rm -f bifrost >/dev/null 2>&1; }

# gw_matrix_egress + the declared capability matrix are defined above (in gw_build). The non-openai
# egress columns are wired-pending-field-verification; the EC2 field run turns each declared-1 cell
# green or red. Every grey cell is a cited capability limit.
