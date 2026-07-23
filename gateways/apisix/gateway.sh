#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Apache APISIX + the ai-proxy plugin (DB-less standalone, docker).
#
# APISIX runs in data-plane/standalone mode (no etcd): routes are read from conf/apisix.yaml. The
# ai-proxy plugin fronts an OpenAI-shaped route and forwards to the mock via override.endpoint. Access
# logging off, worker_processes = pinned core count, no observability plugins → pure proxy overhead.
# APISIX_IMAGE is pinned in gateways/versions.env.
GW_KIND=docker
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="APISIX"                      # label in charts + report tables
GW_LANG=Other                            # implementation language → bar color bucket
GW_CLASS="API gateway"   # the project's OWN self-description (README: 'dynamic, real-time, high-performance cloud-native API gateway'), not our editorial
GW_REPO=https://github.com/apache/apisix   # linked from the gateway name in the report table
GW_PORT=9080
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=sk-fake-benchmark-key
APISIX_IMAGE="${APISIX_IMAGE:-apache/apisix:3.17.0-debian}"

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$APISIX_IMAGE" 2>/dev/null)
  echo "${APISIX_IMAGE}${dg:+ (@${dg##*@})}"
}

gw_build() {
  local ncore=$(( ${CORES##*-} - ${CORES%%-*} + 1 ))
  cat > "$GW_DIR/config.gen.yaml" <<YAML
deployment:
  role: data_plane
  role_data_plane:
    config_provider: yaml
apisix:
  node_listen:
    - $GW_PORT
  enable_admin: false
nginx_config:
  worker_processes: $ncore
  http:
    enable_access_log: false
YAML
  _apisix_write_routes openai-compatible
  sudo docker pull "$APISIX_IMAGE" >/dev/null 2>&1 || true
}

# _apisix_write_routes <provider>: emit the ai-proxy route(s). override.endpoint overrides the HOST
# (documented for PrivateLink/reverse-proxy) while ai-proxy keeps the provider's native upstream
# PATH, so we point endpoint at the mock host and the plugin posts to the dialect's own path
# (/v1/chat/completions, /v1/responses, /v1/messages, /model/<m>/converse). One route per ingress URI
# we probe; APISIX auto-detects the client protocol from body+URI (ai-protocols/init.lua) and either
# passes it through (native to the provider) or applies its single anthropic-messages->openai-chat
# converter. The trailing #END marker is REQUIRED by the yaml config provider.
_apisix_write_routes() {
  local prov="$1" host="http://127.0.0.1:$MOCK_PORT"
  # Provider-specific plugin config. The bedrock provider's schema REQUIRES auth.aws
  # (access_key_id + secret_access_key) and provider_conf.region for SigV4 (ai-proxy/schema.lua +
  # validate_provider_requirements @3.17.0); a schema-invalid route is silently DROPPED by the
  # yaml config provider (core/config_yaml.lua logs and skips it, APISIX still boots) - which is
  # exactly how our earlier bedrock config (header auth, no region) turned into a published 404
  # "boot failure". Dummy AWS keys sign fine; the mock ignores the signature. Verified locally
  # against apache/apisix:3.17.0-debian + the recording mock (converse ingress -> 200, bedrock
  # dialect recorded).
  local plugcfg
  if [ "$prov" = bedrock ]; then
    plugcfg="provider: $prov
        provider_conf: { region: \"us-east-1\" }
        auth:
          aws:
            access_key_id: \"AKIAMOCKACCESSKEY\"
            secret_access_key: \"mock-secret-access-key\"
        options: { model: $GW_MODEL }
        override: { endpoint: \"$host\" }"
  else
    plugcfg="provider: $prov
        auth: { header: { Authorization: \"Bearer $GW_AUTH\" } }
        options: { model: $GW_MODEL }
        override: { endpoint: \"$host\" }"
  fi
  cat > "$GW_DIR/apisix.gen.yaml" <<YAML
routes:
  - id: ai-proxy-chat
    uri: /v1/chat/completions
    methods: [POST]
    plugins:
      ai-proxy:
        $plugcfg
  - id: ai-proxy-responses
    uri: /v1/responses
    methods: [POST]
    plugins:
      ai-proxy:
        $plugcfg
  - id: ai-proxy-messages
    uri: /v1/messages
    methods: [POST]
    plugins:
      ai-proxy:
        $plugcfg
  - id: ai-proxy-converse
    uri: /model/$GW_MODEL/converse
    methods: [POST]
    plugins:
      ai-proxy:
        $plugcfg
#END
YAML
}

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock. APISIX 3.17.0 ai-proxy auto-detects the client protocol and applies a THREE-way
# rule (ai-proxy/base.lua): native passthrough when the provider's capabilities contain the detected
# protocol, else a registered converter, else HTTP 400. The converter registry
# (ai-protocols/converters/init.lua) has exactly ONE relevant bridge: anthropic-messages ->
# openai-chat. So the capable cells are the DIAGONAL of the four protocols APISIX has both a provider
# and a native protocol emitter for - openai (/v1/chat/completions), openai-responses
# (/v1/responses), anthropic (/v1/messages), bedrock (/model/<m>/converse) - PLUS the single
# off-diagonal anthropic-ingress -> openai-egress (that one converter). NOT declared: gemini (the
# gemini/vertex providers expose only an OpenAI-compat capability, no native generateContent emitter)
# and cohere (no provider at all at tag 3.17.0) - both grey with the cited reason. There is NO
# OpenAI-chat -> anthropic/bedrock fan-out (no such converter), so those off-diagonal cells are 0.
# Evidence: apisix/plugins/ai-providers/schema.lua (provider enum), ai-proxy/base.lua (3-way route),
# ai-protocols/converters/init.lua (only anthropic->openai bridge), tag 3.17.0.
GW_MATRIX_CAP="
100000
010000
101000
000000
000000
000001
"
GW_MATRIX_CAP_NOTE="APISIX 3.17.0 ai-proxy has no native Gemini generateContent or Cohere provider, and no OpenAI-to-Anthropic/Bedrock converter (only anthropic->openai); other cells are grey by that capability limit (ai-proxy/base.lua, ai-protocols/converters/init.lua)"
GW_MATRIX_EGRESS="openai openai-responses anthropic bedrock"
gw_matrix_egress() {
  local prov
  case "$1" in
    openai)           prov=openai-compatible;;
    openai-responses) prov=openai-compatible;;
    anthropic)        prov=anthropic;;
    bedrock)          prov=bedrock;;
    *) return 1;;
  esac
  _apisix_write_routes "$prov"
  gw_launch
}

gw_launch() {
  sudo docker rm -f apisix-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name apisix-bench --network host --cpuset-cpus="$CORES" \
    -v "$GW_DIR/config.gen.yaml:/usr/local/apisix/conf/config.yaml:ro" \
    -v "$GW_DIR/apisix.gen.yaml:/usr/local/apisix/conf/apisix.yaml:ro" \
    "$APISIX_IMAGE" >"$GW_DIR/launch.log" 2>&1 || true
}

gw_rss() { container_rss_mib apisix-bench; }  # summed process-tree VmRSS (same method as native gateways)
gw_hwm() { container_hwm_mib apisix-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=apisix-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 apisix-bench 2>&1
}

gw_stop() { sudo docker rm -f apisix-bench >/dev/null 2>&1; }

# gw_matrix_egress + the declared capability matrix are defined above (before gw_launch). The
# non-openai egress columns are wired-pending-field-verification (dev-box docker host networking is
# unreliable); the EC2 field run turns each declared-1 cell green or red. Every grey cell is a cited
# capability limit, never untested generosity.
