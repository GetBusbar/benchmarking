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
GW_CLASS="Data plane"   # the project's OWN self-description (README: 'open source data plane optimized for agentic AI connectivity'), not our editorial
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

# _agentgw_write_config <name> <pathOverride> <provider-yaml>: emit the ai-backend config. The `ai`
# backend's hostOverride/pathOverride point the upstream at the mock's per-dialect endpoint while the
# provider block selects the native egress dialect (agentgateway's per-provider translation emits
# that dialect's request shape). Indentation must match the provider: block's nesting.
_agentgw_write_config() {
  cat > "$GW_DIR/config.gen.yaml" <<YAML
binds:
- port: $GW_PORT
  listeners:
  - name: llm
    protocol: HTTP
    routes:
    - backends:
      - ai:
          name: $1
          hostOverride: "127.0.0.1:$MOCK_PORT"
          pathOverride: $2
          provider:
$3
YAML
}

gw_build() {
  _agentgw_write_config openai "$GW_PATH" "            openAI:
              model: $GW_MODEL"
  sudo docker pull "$AGENTGATEWAY_IMAGE" >/dev/null 2>&1 || true
}

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock. agentgateway v1.3.1's AIProvider enum is {openAI, gemini, vertex, anthropic,
# bedrock, azure, copilot, custom} (crates/agentgateway/src/llm/mod.rs). The ai backend accepts the
# OpenAI-canonical ingress and translates it to the routed provider's NATIVE upstream shape for
# anthropic (to_anthropic -> /v1/messages), bedrock (to_bedrock ConverseRequest -> /model/<m>/converse)
# and openai-responses (RouteType::Responses -> /v1/responses); host/pathOverride point each at the
# mock. So the capable row is openai-ingress into {openai, openai-responses, anthropic, bedrock}. NOT
# declared: gemini (v1.3.1 targets Google's OpenAI-compat surface /v1beta/openai/chat/completions,
# NOT native :generateContent, so it would not produce the gemini upstream shape this suite checks)
# and cohere (absent from the AIProvider enum; only reachable via a synthetic `custom` backend) -
# both grey with the cited reason.
# Evidence: llm/mod.rs (AIProvider enum + RouteType::Responses), llm/anthropic.rs (/v1/messages),
# llm/bedrock.rs (converse), llm/gemini.rs (OpenAI-compat path), tag v1.3.1.
GW_MATRIX_CAP="
111001
000000
000000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="agentgateway v1.3.1 emits Gemini only via Google's OpenAI-compat surface (not native generateContent) and has no Cohere provider in its AIProvider enum; those cells are grey by that capability limit (llm/mod.rs, llm/gemini.rs)"
GW_MATRIX_EGRESS="openai openai-responses anthropic bedrock"
gw_matrix_egress() {
  case "$1" in
    openai)           _agentgw_write_config openai "/v1/chat/completions" "            openAI:
              model: $GW_MODEL";;
    openai-responses) _agentgw_write_config openai "/v1/responses" "            openAI:
              model: $GW_MODEL";;
    anthropic)        _agentgw_write_config anthropic "/v1/messages" "            anthropic:
              model: claude-3-5-sonnet-20241022";;
    bedrock)          _agentgw_write_config bedrock "/model/$GW_MODEL/converse" "            bedrock:
              model: $GW_MODEL
              region: us-east-1";;
    *) return 1;;
  esac
  gw_launch
}

gw_launch() {
  sudo docker rm -f agentgateway-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name agentgateway-bench --network host --cpuset-cpus="$CORES" \
    -e ADMIN_ADDR=127.0.0.1:15000 -e STATS_ADDR=127.0.0.1:15020 -e RUST_LOG=error \
    -v "$GW_DIR/config.gen.yaml:/config.yaml:ro" \
    "$AGENTGATEWAY_IMAGE" -f /config.yaml >"$GW_DIR/launch.log" 2>&1 || true
}

gw_rss() { container_rss_mib agentgateway-bench; }  # summed process-tree VmRSS (same method as native)
gw_hwm() { container_hwm_mib agentgateway-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=agentgateway-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 agentgateway-bench 2>&1
}

gw_stop() { sudo docker rm -f agentgateway-bench >/dev/null 2>&1; }

# gw_matrix_egress + the declared capability matrix are defined above (before gw_launch). The
# non-openai egress columns are wired-pending-field-verification; the EC2 field run turns each
# declared-1 cell green or red. Every grey cell is a cited capability limit.
