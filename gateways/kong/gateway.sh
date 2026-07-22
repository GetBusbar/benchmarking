#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Kong Gateway + the ai-proxy plugin (DB-less, docker).
#
# Kong's ai-proxy plugin fronts an OpenAI-shaped /v1/chat/completions and forwards to an upstream
# LLM; `model.options.upstream_url` overrides that upstream, so we point it straight at the mock.
# DB-less declarative config, generated in gw_launch against the runner's mock port. KONG_IMAGE is
# pinned in gateways/versions.env.
GW_KIND=docker
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="Kong"                      # label in charts + report tables
GW_LANG=Other                            # implementation language → bar color bucket
GW_CLASS="API gateway"   # the project's OWN self-description (README: 'cloud-native API gateway'), not our editorial
GW_REPO=https://github.com/Kong/kong   # linked from the gateway name in the report table
GW_PORT=8080
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=dummy

KONG_IMAGE="${KONG_IMAGE:-kong:3.8}"
gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$KONG_IMAGE" 2>/dev/null)
  echo "${KONG_IMAGE}${dg:+ (@${dg##*@})}"
}
gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=kong-bench --format '{{.Status}}' 2>/dev/null)"
  echo "logs:"; sudo docker logs --tail 25 kong-bench 2>&1
}

gw_build() {
  _kong_write_config openai "http://127.0.0.1:$MOCK_PORT/v1/chat/completions"
  sudo docker pull "${KONG_IMAGE:-kong:3.8}" >/dev/null 2>&1 || true
}

# _kong_write_config <provider> <upstream_url>: emit the DB-less declarative config. Kong 3.8
# ai-proxy always accepts the OpenAI-canonical ingress on /v1/chat/completions (route_type
# llm/v1/chat) and TRANSFORMS it into the provider's native upstream shape; model.options.upstream_url
# overrides the full egress URL so we point it at the mock's per-dialect endpoint.
_kong_write_config() {
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
            provider: $1
            name: $GW_MODEL
            options:
              upstream_url: "$2"
YAML
}

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock. Kong 3.8 ai-proxy accepts ONLY the OpenAI-canonical ingress (kong/llm/init.lua
# identify_request keys on body.messages[]/body.prompt; there is NO anthropic/gemini/bedrock/cohere
# ingress detector) and fans that one ingress out to the configured provider's native UPSTREAM shape
# via driver.to_format. So the only capable row is openai-ingress, into the egress providers whose
# native Converse/Messages/generateContent shape Kong 3.8 emits with an upstream_url override
# (kong/llm/drivers/shared.lua): anthropic (/v1/messages), gemini (:generateContent), bedrock
# (converse). NOT declared: openai-responses (no llm/v1/responses route_type in 3.8) and cohere
# (Kong 3.8 emits the Cohere *v1* /v1/chat shape, CHATBOT/chat_history, not the v2 dialect this suite
# probes) - both grey with the cited reason below.
# Evidence: kong/llm/init.lua (ingress detect + route_type enum), kong/llm/drivers/shared.lua
# (upstream_url override + per-provider paths), release/3.8.x.
GW_MATRIX_CAP="
101101
000000
000000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="Kong 3.8 ai-proxy accepts only OpenAI-canonical ingress and emits no OpenAI-Responses route_type or Cohere v2 upstream shape (kong/llm/init.lua, drivers/shared.lua)"
GW_MATRIX_EGRESS="openai anthropic gemini bedrock"
gw_matrix_egress() {
  local host="http://127.0.0.1:$MOCK_PORT" prov url
  case "$1" in
    openai)    prov=openai;    url="$host/v1/chat/completions";;
    anthropic) prov=anthropic; url="$host/v1/messages";;
    gemini)    prov=gemini;    url="$host/v1beta/models/$GW_MODEL:generateContent";;
    bedrock)   prov=bedrock;   url="$host/model/$GW_MODEL/converse";;
    *) return 1;;
  esac
  _kong_write_config "$prov" "$url"
  gw_launch
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

gw_rss() { container_rss_mib kong-bench; }  # summed process-tree VmRSS (same method as native gateways)
gw_hwm() { container_hwm_mib kong-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_stop() { sudo docker rm -f kong-bench >/dev/null 2>&1; }
# gw_matrix_egress + the declared capability matrix are defined above (before gw_launch). The
# anthropic/gemini/bedrock egress columns are wired-pending-field-verification: the dev box cannot
# reach docker host networking reliably, so the EC2 field run is what turns each declared-1 cell
# green or red. No declared-1 cell is left grey; every grey cell is a cited capability limit.
