#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Kong Gateway + the ai-proxy plugin (DB-less, docker).
#
# Kong's ai-proxy plugin fronts an OpenAI-shaped /v1/chat/completions and forwards to an upstream
# LLM; `model.options.upstream_url` overrides that upstream, so we point it straight at the mock.
# DB-less declarative config, generated against the runner's mock port. KONG_IMAGE is pinned in
# gateways/versions.env.
#
# ── OOTB posture (one-config standard) ────────────────────────────────────────────────────────────
# This is the config a real user deploys, used unchanged for EVERY lane. Kong's ai-proxy binds ONE
# upstream provider per route (multi-provider fan-out is a DIFFERENT plugin, ai-proxy-advanced), so —
# exactly like TensorZero, whose provider block is likewise single-upstream-per-config — the canonical
# OpenAI provider is what perf/latency/throughput/memory run on, and the matrix re-renders the SAME
# real-world config shape per egress column (only the permitted provider + base_url swap; all → mock).
# Every provider ai-proxy 3.8 supports and this suite probes is covered by GW_MATRIX_EGRESS below.
# Permitted deviations only: provider upstream_url → mock, dummy auth/AWS signing (the mock ignores
# it), the per-provider REQUIRED fields (anthropic_version, bedrock region+creds), and two disclosed
# run-mechanics — KONG_DATABASE=off (DB-less: no external Postgres dependency) and KONG_ANONYMOUS_
# REPORTS=off (telemetry/phone-home suppression). The client always hits the uniform /v1/chat/
# completions route; no special passthrough route is added.
#
# FAIRNESS AUDIT (Kong 3.8.0 source):
#   * REMOVED KONG_ADMIN_LISTEN=off: that DISABLED a default-on feature. Kong's Admin API is ON by
#     default (kong.conf.default @3.8.0: admin_listen = 127.0.0.1:8001 ... + 127.0.0.1:8444 ssl),
#     bound to localhost, and DB-less only makes it read-only — it does not turn it off. Turning it
#     off was a feature-strip; restored to the default (the var is simply not set). Harmless on a
#     dedicated single-box bench (localhost-only, no port clash, proxy traffic unaffected).
#   * ADDED KONG_ANONYMOUS_REPORTS=off: anonymous_reports defaults to `on` (kong.conf.default @3.8.0)
#     — Kong phones home usage/error data by default. Suppressing outbound telemetry is a permitted
#     disclosed run-mechanic (not a functional strip).
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
#
# Per-provider REQUIRED config (kong/llm/schemas/init.lua @3.8.0 - omitting these was OUR bug that
# published boot failures as Kong reds):
#   anthropic - model.options.anthropic_version is entity-check REQUIRED
#               (conditional_at_least_one_of: "must set %s for anthropic provider"); without it the
#               declarative config fails validation and Kong never boots.
#   bedrock   - configure_request SigV4-signs every request (drivers/bedrock.lua); with no
#               auth.aws_access_key_id/aws_secret_access_key and no ambient AWS credentials the
#               signer fails ("failed to sign AWS request") -> HTTP 500. Dummy keys +
#               model.options.bedrock.aws_region satisfy the signer; the mock ignores the signature.
#   cohere    - a first-class ai-proxy 3.8 provider (schema enum includes cohere). Kong 3.8 emits the
#               Cohere v1 /v1/chat shape; upstream_url override points it at the mock. (The matrix's
#               cohere egress probes the v2 dialect, so that cell stays grey — see GW_MATRIX_CAP —
#               but the provider is genuinely supported and wired for completeness.)
# All fixed configs verified locally against kong:3.8 + the recording mock.
_kong_write_config() {
  local prov="$1" url="$2" auth extra
  case "$prov" in
    bedrock)
      auth='auth:
            aws_access_key_id: "AKIAMOCKACCESSKEY"
            aws_secret_access_key: "mock-secret-access-key"'
      extra='
              bedrock:
                aws_region: "us-east-1"';;
    anthropic)
      auth='auth:
            header_name: Authorization
            header_value: "Bearer dummy"'
      extra='
              anthropic_version: "2023-06-01"';;
    *)
      auth='auth:
            header_name: Authorization
            header_value: "Bearer dummy"'
      extra='';;
  esac
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
          $auth
          model:
            provider: $prov
            name: $GW_MODEL
            options:$extra
              upstream_url: "$url"
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
# (converse). NOT declared: openai-responses (no llm/v1/responses route_type in 3.8 — the enum is
# {llm/v1/chat, llm/v1/completions, preserve}) and cohere-v2 (Kong 3.8's cohere driver emits the
# Cohere *v1* /v1/chat shape, CHATBOT/chat_history, not the v2 dialect this suite probes) - both grey
# with the cited reason. cohere IS a supported 3.8 provider (schema enum), just at the v1 dialect.
# Evidence: kong/llm/init.lua (ingress detect + route_type enum), kong/llm/drivers/shared.lua
# (upstream_url override + per-provider paths), kong/llm/schemas/init.lua (provider enum), 3.8.0.
GW_MATRIX_CAP="
101101
000000
000000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="Kong 3.8 ai-proxy accepts only OpenAI-canonical ingress; it emits no OpenAI-Responses route_type (enum: llm/v1/chat|completions|preserve) and its Cohere driver emits the Cohere v1 /v1/chat shape, not the v2 dialect this suite probes (kong/llm/init.lua, drivers/shared.lua, schemas/init.lua @3.8.0)"
GW_MATRIX_EGRESS="openai anthropic gemini bedrock"

# ── xlate lane: not declared (no anthropic-format ingress at 3.8) ────────────────────────────────
# Kong 3.8's ai-proxy ingress detector (kong/llm/init.lua identify_request) keys on the
# OpenAI-canonical body only (messages[]/prompt); there is no Anthropic-Messages ingress detector
# and no /v1/messages route in this manifest's declarative config, so the probe's 404 "no Route
# matched" was Kong's correct answer, not a failed translation.
GW_XLATE_CAP=0
GW_XLATE_CAP_NOTE="Kong 3.8 ai-proxy accepts only OpenAI-canonical ingress (llm/init.lua identify_request has no Anthropic-Messages detector), so anthropic-in -> openai-out translation is not a claimed capability"
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
  # KONG_DATABASE=off = DB-less declarative (no external Postgres — a disclosed run-mechanic).
  # KONG_ANONYMOUS_REPORTS=off suppresses Kong's default-on telemetry phone-home (run-mechanic).
  # The Admin API listener is left at its default (ON, localhost:8001/8444) — not disabled.
  sudo docker run -d --name kong-bench --network host --cpuset-cpus="$CORES" \
    -e KONG_DATABASE=off \
    -e KONG_ANONYMOUS_REPORTS=off \
    -e KONG_DECLARATIVE_CONFIG=/kong/kong.yml \
    -e "KONG_PROXY_LISTEN=0.0.0.0:$GW_PORT" \
    -v "$GW_DIR/kong.gen.yml:/kong/kong.yml:ro" \
    "${KONG_IMAGE:-kong:3.8}" >/dev/null 2>&1
}

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config Kong launches with. Kong is file-driven, so the artifact
# is the rendered DB-less declarative config (exactly what KONG_DECLARATIVE_CONFIG loads) PLUS the
# non-secret launch env (any auth/AWS values in the config are dummy on the isolated rig). Read from
# the file _kong_write_config just rendered (falls back to the openai-lane default if absent — the
# canonical perf config), so it can never drift from what Kong loaded. OOTB posture: ai-proxy on the
# uniform /v1/chat/completions route, admin API left at its default (not disabled); the only run-
# mechanics are KONG_DATABASE=off (DB-less) and KONG_ANONYMOUS_REPORTS=off (telemetry).
gw_config() {
  local cfg="$GW_DIR/kong.gen.yml"
  echo "# ── kong.gen.yml (rendered DB-less declarative; loaded via KONG_DECLARATIVE_CONFIG) ──"
  [ -f "$cfg" ] || _kong_write_config openai "http://127.0.0.1:$MOCK_PORT/v1/chat/completions"
  cat "$cfg"
  echo
  echo "# ── launch env (non-secret) ──"
  cat <<ENV
KONG_DATABASE=off
KONG_ANONYMOUS_REPORTS=off
KONG_DECLARATIVE_CONFIG=/kong/kong.yml
KONG_PROXY_LISTEN=0.0.0.0:$GW_PORT
ENV
}

gw_rss() { container_rss_mib kong-bench; }  # summed process-tree VmRSS (same method as native gateways)
gw_hwm() { container_hwm_mib kong-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_stop() { sudo docker rm -f kong-bench >/dev/null 2>&1; }
# gw_matrix_egress + the declared capability matrix are defined above (before gw_launch). The
# anthropic/gemini/bedrock egress columns are wired-pending-field-verification: the dev box cannot
# reach docker host networking reliably, so the EC2 field run is what turns each declared-1 cell
# green or red. No declared-1 cell is left grey; every grey cell is a cited capability limit.
