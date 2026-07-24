#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Apache APISIX + the ai-proxy plugin (DB-less standalone, docker).
#
# APISIX runs in data-plane/standalone mode (no etcd): routes are read from conf/apisix.yaml. The
# ai-proxy plugin fronts an OpenAI-shaped route and forwards to the mock via override.endpoint. OOTB
# config.yaml carries ONLY the DB-less standalone run-mechanic (data_plane role + yaml config_provider)
# + the port binding; admin API, worker_processes and access logging are all left at APISIX's shipped
# defaults (enable_admin on / worker_processes auto / enable_access_log on). APISIX_IMAGE pinned in
# gateways/versions.env.
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
  # OOTB config.yaml: ONLY the run-mechanic that lets APISIX run without an external etcd — the DB-less
  # standalone data_plane role with the yaml config_provider (routes from conf/apisix.yaml) — plus the
  # port binding. Everything else is left at APISIX's shipped defaults, honestly:
  #   * enable_admin: DEFAULT true (kept) — the Admin API boots fine in DB-less yaml mode with no etcd
  #     (ops.lua skips init_etcd for the data_plane role; admin/init.lua runs its standalone branch and
  #     returns before any etcd sync). The shipped default admin_key satisfies the token check, so no
  #     extra config is needed. Previously we set it false — a gratuitous feature-strip, removed.
  #   * nginx_config.worker_processes: DEFAULT "auto" (kept) — previously pinned to the core count, a
  #     perf/concurrency tuning knob; removed so APISIX self-sizes like every fresh install.
  #   * nginx_config.http.enable_access_log: DEFAULT true (kept) — previously false, which suppressed
  #     APISIX's default HTTP request/access logging; that logging is on by default and stays on.
  cat > "$GW_DIR/config.gen.yaml" <<YAML
deployment:
  role: data_plane
  role_data_plane:
    config_provider: yaml
apisix:
  node_listen:
    - $GW_PORT
YAML
  _apisix_write_routes
  sudo docker pull "$APISIX_IMAGE" >/dev/null 2>&1 || true
}

# _apisix_plugcfg <provider> <model>: emit one ai-proxy plugin block for the given provider, correctly
# authed. override.endpoint overrides the HOST (documented for PrivateLink/reverse-proxy) while ai-proxy
# keeps the provider's native upstream PATH, so we point endpoint at the mock host and the plugin posts
# to the dialect's own path. The bedrock provider's schema REQUIRES auth.aws (access_key_id +
# secret_access_key) and provider_conf.region for SigV4 (ai-proxy/schema.lua + validate_provider_
# requirements @3.17.0); a schema-invalid route is silently DROPPED by the yaml config provider
# (core/config_yaml.lua logs and skips it, APISIX still boots). Dummy AWS keys sign fine; the mock
# ignores the signature. Non-bedrock providers take a mandatory Bearer header (dummy key) — ai-proxy's
# schema requires an auth block, so this is the gateway's own required-auth posture, kept (not added).
_apisix_plugcfg() {
  local prov="$1" model="$2" host="http://127.0.0.1:$MOCK_PORT"
  if [ "$prov" = bedrock ]; then
    printf '%s' "provider: $prov
        provider_conf: { region: \"us-east-1\" }
        auth:
          aws:
            access_key_id: \"AKIAMOCKACCESSKEY\"
            secret_access_key: \"mock-secret-access-key\"
        options: { model: $model }
        override: { endpoint: \"$host\" }"
  else
    printf '%s' "provider: $prov
        auth: { header: { Authorization: \"Bearer $GW_AUTH\" } }
        options: { model: $model }
        override: { endpoint: \"$host\" }"
  fi
}

# _apisix_write_routes: emit APISIX's ONE canonical ai-proxy config wiring EVERY mock-reachable declared
# provider at once, each on its native ingress URI: /v1/chat/completions -> openai-compatible (the
# standard /v1 OpenAI-SDK route, GW_PATH), /v1/responses -> openai-compatible (Responses egress),
# /v1/messages -> anthropic, /model/<m>/converse -> bedrock. All four routes are live simultaneously, so
# the perf/memory/throughput/stream lanes and the matrix run the SAME config; the matrix does not swap
# providers per lane, it just drives a different ingress URI. APISIX auto-detects the client protocol
# from body+URI (ai-protocols/init.lua) and passes it native to the provider. The trailing #END marker
# is REQUIRED by the yaml config provider. (Cohere/Gemini native wire aren't emittable at 3.17.0 — see
# GW_MATRIX_CAP — so they are honestly absent, not silently dropped.)
_apisix_write_routes() {
  cat > "$GW_DIR/apisix.gen.yaml" <<YAML
routes:
  - id: ai-proxy-chat
    uri: /v1/chat/completions
    methods: [POST]
    plugins:
      ai-proxy:
        $(_apisix_plugcfg openai-compatible gpt-4o-mini)
  - id: ai-proxy-responses
    uri: /v1/responses
    methods: [POST]
    plugins:
      ai-proxy:
        $(_apisix_plugcfg openai-compatible gpt-4o-mini)
  - id: ai-proxy-messages
    uri: /v1/messages
    methods: [POST]
    plugins:
      ai-proxy:
        $(_apisix_plugcfg anthropic claude-3-5-sonnet-20241022)
  - id: ai-proxy-converse
    uri: /model/anthropic.claude-3-sonnet-20240229-v1:0/converse
    methods: [POST]
    plugins:
      ai-proxy:
        $(_apisix_plugcfg bedrock anthropic.claude-3-sonnet-20240229-v1:0)
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
  # All four egress providers are already wired as simultaneous routes in the ONE config
  # (_apisix_write_routes), each on its native ingress URI — so no per-lane route rewrite is needed. The
  # matrix runner drives the URI for the requested ingress/egress; we just validate the egress is one we
  # wired and (re)launch the identical all-providers config.
  case "$1" in
    openai|openai-responses|anthropic|bedrock) gw_launch;;
    *) return 1;;
  esac
}

gw_launch() {
  sudo docker rm -f apisix-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name apisix-bench --network host --cpuset-cpus="$CORES" \
    -v "$GW_DIR/config.gen.yaml:/usr/local/apisix/conf/config.yaml:ro" \
    -v "$GW_DIR/apisix.gen.yaml:/usr/local/apisix/conf/apisix.yaml:ro" \
    "$APISIX_IMAGE" >"$GW_DIR/launch.log" 2>&1 || true
}

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config this gateway launches with — both files APISIX loads,
# rendered exactly as mounted: config.yaml (the DB-less standalone run-mechanic + port) and apisix.yaml
# (the ai-proxy routes wiring every mock-reachable declared provider). Read from the files gw_build just
# produced so they can never drift; falls back to rendering them if not present yet. Secrets are dummy:
# the openai/anthropic routes carry the dummy Bearer key ai-proxy's schema requires, bedrock the dummy
# AWS SigV4 keys its schema requires — never a live key. OOTB posture: config.yaml carries ONLY the
# etcd-avoidance run-mechanic (data_plane + yaml config_provider) + port; enable_admin, worker_processes
# and access_log are all left at their shipped defaults (on/auto/on). No feature strips, no perf tuning.
gw_config() {
  local cfg="$GW_DIR/config.gen.yaml" routes="$GW_DIR/apisix.gen.yaml"
  [ -f "$cfg" ]    || { cat > "$cfg" <<YAML
deployment:
  role: data_plane
  role_data_plane:
    config_provider: yaml
apisix:
  node_listen:
    - $GW_PORT
YAML
  }
  [ -f "$routes" ] || _apisix_write_routes
  echo "# ── conf/config.yaml (rendered; mounted read-only) ──"
  cat "$cfg"
  echo
  echo "# ── conf/apisix.yaml (rendered; ai-proxy routes, mounted read-only) ──"
  cat "$routes"
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
