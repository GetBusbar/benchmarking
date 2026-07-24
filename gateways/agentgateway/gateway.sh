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

# _agentgw_write_config <name> <pathOverride|""> <provider-yaml>: emit the ai-backend config. The
# `ai` backend's hostOverride (and optional pathOverride) point the upstream at the mock while the
# provider block selects the native egress dialect (agentgateway's per-provider translation emits
# that dialect's request shape). Indentation must match the provider: block's nesting.
#
# INGRESS CLASSIFICATION (the fix that unlocks v1.3.1's real translation support): a request's
# ingress format is the route's llm-policy `routes` map (path suffix -> RouteType,
# llm/policy/mod.rs resolve_route, config key policies.ai.routes). WITHOUT it every request
# defaults to RouteType::Completions (proxy/httpproxy.rs `.unwrap_or(RouteType::Completions)`),
# which is why /v1/messages ingress previously came back as an untranslated OpenAI envelope (an
# Anthropic Messages body PARSES as a chat.completions request - both carry `messages`) and
# /v1/responses 503'd with "missing field messages" (a Responses body has `input`). With the
# classifier the v1.3.1 dispatch (llm/mod.rs:980) genuinely supports: Completions ingress -> all
# providers; Messages -> Anthropic/OpenAI/Bedrock/Gemini/...; Responses -> OpenAI/Bedrock/Gemini.
# PATH SELECTION: with hostOverride and NO pathPrefix, v1.3.1 keeps the CLIENT's original path
# verbatim (llm/mod.rs set_default_path early-returns), so a translated body lands on the ingress
# path's endpoint - e.g. a Messages->Completions translation still POSTs /v1/messages. Setting
# pathPrefix ("/v1", the provider's own DEFAULT_BASE_PATH) re-enables the provider's native
# per-route-type path (openai: /chat/completions for Completions+Messages, /responses for
# Responses; anthropic: /messages for everything), which is what the mock's per-dialect endpoints
# verify. A non-empty $2 instead forces ONE exact upstream path via pathOverride (bedrock's
# /model/<m>/converse); pass "" to use pathPrefix routing.
_agentgw_write_config() {
  local pathline="          pathPrefix: /v1"
  [ -n "$2" ] && pathline="          pathOverride: $2"
  cat > "$GW_DIR/config.gen.yaml" <<YAML
binds:
- port: $GW_PORT
  listeners:
  - name: llm
    protocol: HTTP
    routes:
    - policies:
        ai:
          routes:
            "/v1/chat/completions": completions
            "/v1/messages": messages
            "/v1/responses": responses
      backends:
      - ai:
          name: $1
          hostOverride: "127.0.0.1:$MOCK_PORT"
$pathline
          provider:
$3
YAML
}

gw_build() {
  _agentgw_write_config openai "" "            openAI:
              model: $GW_MODEL"
  sudo docker pull "$AGENTGATEWAY_IMAGE" >/dev/null 2>&1 || true
}

# ── matrix suite: advisory capability + egress wiring (probe-first: every cell is probed) ────────
# Advisory 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock - the cells v1.3.1's dispatch (llm/mod.rs:980, WITH the ingress classifier above)
# genuinely supports against this rig's native-dialect mock:
#   openai ingress (Completions)      -> openai, anthropic, bedrock egress (Completions -> all
#                                        providers; each emits its provider's native wire);
#   openai-responses ingress          -> openai-responses egress (OpenAI provider sends Responses
#                                        wire to /v1/responses) and bedrock (Responses -> Converse);
#   anthropic ingress (Messages)      -> openai ("messages via translation to chat completions"),
#                                        anthropic, bedrock egress.
# NOT expected (probe-first records each with its own evidence, never a red):
#   gemini egress   - v1.3.1's gemini provider targets Google's OPENAI-COMPAT surface
#                     (/v1beta/openai/chat/completions, llm/gemini.rs), never the native
#                     :generateContent wire this rig's mock verifies, so no gemini-egress writer
#                     is defined: the column probes under the default config and greys honestly;
#   cohere egress   - no Cohere provider in the AIProvider enum;
#   gemini/cohere/bedrock ingress - no RouteType exists for those request shapes (llm/mod.rs
#                     RouteType enum: Completions/Messages/Responses/...), unclassified paths
#                     default to Completions and fail parse;
#   responses -> anthropic - (Anthropic, InputFormat::Responses) is the UnsupportedConversion arm.
GW_MATRIX_CAP="
101001
010001
101001
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="agentgateway v1.3.1 (with per-route ingress classification via policies.ai.routes): Completions/Messages/Responses ingress translate to the openAI/anthropic/bedrock providers' native wire; gemini egress exists only via Google's OpenAI-compat surface (not native generateContent), cohere is absent from the AIProvider enum, and gemini/cohere/bedrock request shapes have no ingress RouteType"
GW_MATRIX_EGRESS="openai openai-responses anthropic bedrock"
gw_matrix_egress() {
  case "$1" in
    # openai + openai-responses egress share the ONE OpenAI-provider config with NO pathOverride:
    # each classified route type takes the provider's native path (Completions+Messages ->
    # /v1/chat/completions, Responses -> /v1/responses), so the leg-3 record attributes every cell
    # to the endpoint the gateway actually spoke. A forced pathOverride would smash Responses onto
    # the chat path and manufacture failures.
    openai|openai-responses) _agentgw_write_config openai "" "            openAI:
              model: $GW_MODEL";;
    anthropic)        _agentgw_write_config anthropic "" "            anthropic:
              model: claude-3-5-sonnet-20241022";;
    bedrock)          _agentgw_write_config bedrock "/model/$GW_MODEL/converse" "            bedrock:
              model: $GW_MODEL
              region: us-east-1";;
    *) return 1;;   # no writer: the probe-first runner probes this column under the default config
  esac
  gw_launch
}

# ── xlate lane note ───────────────────────────────────────────────────────────────────────────────
# The historical xlate failure at v1.3.1 (Anthropic ingress answered with an untranslated OpenAI
# envelope) was THIS manifest's config gap, not the gateway's: without policies.ai.routes every
# request was classified Completions (an Anthropic Messages body parses as chat.completions - both
# carry `messages`), so no translation was ever attempted. The default config now classifies
# /v1/messages as Messages ingress; the field run re-verifies the lane end to end.

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
