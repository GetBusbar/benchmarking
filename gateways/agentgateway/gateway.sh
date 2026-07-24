#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: agentgateway (agentgateway/agentgateway, Rust data plane, docker).
#
# OpenAI-compatible LLM gateway using agentgateway's canonical MULTI-PROVIDER surface — the top-level
# `llm:` block (LocalLLMConfig), NOT the hand-wired single-provider `binds` route. `llm.models[]` wires
# every mock-reachable declared-egress provider at once (openAI, anthropic, bedrock) and the built-in
# ModelRouter dispatches each request to the provider whose model name it carries — so the SAME config
# serves the perf/memory/throughput/stream lanes and every matrix egress, all on the uniform /v1 route.
# The `llm:` block auto-populates the SAME path→RouteType ingress classifier the old policies.ai.routes
# map provided (/v1/chat/completions→Completions, /v1/messages→Messages, /v1/responses→Responses;
# local.rs llm_route_types + policy/mod.rs resolve_route), so the verified matrix ingress classification
# is preserved. Each provider's baseUrl points at the mock (plaintext http://). No feature strips: no
# RUST_LOG/admin/stats overrides (see gw_launch) — tracing stays off only because no OTLP endpoint is
# set (its real default), metrics/admin/readiness bind as a fresh install does. AGENTGATEWAY_IMAGE is
# pinned in gateways/versions.env.
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

# _agentgw_write_config: emit agentgateway's ONE canonical multi-provider `llm:` config. Each
# llm.models[] entry names the request model that selects it (ModelRouter, model_router.rs) and a
# provider enum (bare string) plus a params block whose baseUrl points that provider's upstream at the
# mock over plaintext http://. All mock-reachable declared providers are wired at once:
#   - openAI     (model gpt-4o-mini) — the /v1 OpenAI-SDK default;
#   - anthropic  (claude-3-5-sonnet-20241022);
#   - bedrock    (a claude bedrock model id) — params.awsRegion is REQUIRED or boot fails
#     (local.rs "bedrock requires aws_region"); SigV4 creds come from the AWS env (dummy AWS_* in
#     gw_launch), the mock ignores the signature.
# INGRESS CLASSIFICATION is auto-populated by the llm: block: llm_route_types() hardcodes
# /v1/chat/completions→Completions, /v1/messages→Messages, /v1/responses→Responses on the synthesized
# catch-all route, and Policy::resolve_route matches by path suffix (policy/mod.rs) — the exact
# classifier the old policies.ai.routes map gave, so Messages/Responses ingress translate as verified.
# apiKey values are the dummy keys agentgateway auto-detects from config (never a live key); the mock
# ignores auth. gemini is NOT wired: its provider targets Google's OpenAI-compat surface, not the native
# generateContent wire this rig's mock verifies (declared grey in GW_MATRIX_CAP) — wiring it would imply
# a capability the matrix greys. cohere has no provider in the AIProvider enum.
_agentgw_write_config() {
  cat > "$GW_DIR/config.gen.yaml" <<YAML
llm:
  port: $GW_PORT
  models:
  - name: gpt-4o-mini
    provider: openAI
    params:
      model: gpt-4o-mini
      apiKey: "sk-dummy-openai"
      baseUrl: "http://127.0.0.1:$MOCK_PORT"
  - name: claude-3-5-sonnet-20241022
    provider: anthropic
    params:
      model: claude-3-5-sonnet-20241022
      apiKey: "sk-ant-dummy"
      baseUrl: "http://127.0.0.1:$MOCK_PORT"
  - name: anthropic.claude-3-sonnet-20240229-v1:0
    provider: bedrock
    params:
      model: anthropic.claude-3-sonnet-20240229-v1:0
      awsRegion: us-east-1
      baseUrl: "http://127.0.0.1:$MOCK_PORT"
YAML
}

gw_build() {
  _agentgw_write_config
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
GW_MATRIX_CAP_NOTE="agentgateway v1.3.1 (llm: block auto-populates the same path→RouteType ingress classifier, local.rs llm_route_types): Completions/Messages/Responses ingress translate to the openAI/anthropic/bedrock providers' native wire; gemini egress exists only via Google's OpenAI-compat surface (not native generateContent), cohere is absent from the AIProvider enum, and gemini/cohere/bedrock request shapes have no ingress RouteType"
GW_MATRIX_EGRESS="openai openai-responses anthropic bedrock"
gw_matrix_egress() {
  # All egress providers are already wired in the ONE llm: config (see _agentgw_write_config); the
  # ModelRouter picks the provider from the request model name, so the matrix only flips GW_MODEL — no
  # config rewrite. The relaunch runs the identical all-providers config. openai + openai-responses
  # share the openAI model (the path classifier routes Completions→/v1/chat/completions,
  # Responses→/v1/responses on the same backend), so both use gpt-4o-mini.
  case "$1" in
    openai|openai-responses) GW_MODEL=gpt-4o-mini;;
    anthropic)               GW_MODEL=claude-3-5-sonnet-20241022;;
    bedrock)                 GW_MODEL=anthropic.claude-3-sonnet-20240229-v1:0;;
    *) return 1;;
  esac
  gw_launch
}

# ── xlate lane note ───────────────────────────────────────────────────────────────────────────────
# The historical xlate failure at v1.3.1 (Anthropic ingress answered with an untranslated OpenAI
# envelope) was a config gap, not the gateway's: without a path→RouteType classifier every request was
# classified Completions (an Anthropic Messages body parses as chat.completions - both carry `messages`),
# so no translation was attempted. The llm: block now auto-classifies /v1/messages as Messages ingress
# (llm_route_types); the field run re-verifies the lane end to end.

gw_launch() {
  sudo docker rm -f agentgateway-bench >/dev/null 2>&1; sleep 1
  # OOTB posture — no feature-strip env overrides. Previously this passed RUST_LOG=error (agentgateway's
  # default log level is info, telemetry.rs default_filter; error SUPPRESSES its default info-level
  # request logging — a logging strip, removed) and ADMIN_ADDR/STATS_ADDR pins (admin already defaults to
  # localhost, but the stats/metrics server defaults to 0.0.0.0:15020 — pinning it to loopback narrowed
  # the default bind posture; both removed so admin, stats and readiness bind exactly as a fresh install
  # does). The only env passed is dummy AWS creds — the bedrock provider signs SigV4 from the AWS env
  # (there is no per-model AWS key field at the llm: surface); the mock ignores the signature. Nothing is
  # disabled: all default servers stay on.
  sudo docker run -d --name agentgateway-bench --network host --cpuset-cpus="$CORES" \
    -e AWS_ACCESS_KEY_ID=AKIAMOCKACCESSKEY -e AWS_SECRET_ACCESS_KEY=mock-secret-access-key -e AWS_REGION=us-east-1 \
    -v "$GW_DIR/config.gen.yaml:/config.yaml:ro" \
    "$AGENTGATEWAY_IMAGE" -f /config.yaml >"$GW_DIR/launch.log" 2>&1 || true
}

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config this gateway launches with — the rendered config.gen.yaml
# (the top-level llm: block) exactly as mounted at /config.yaml (read from the file gw_build produced so
# it can never drift; falls back to rendering it if not present yet) PLUS the non-secret launch env (the
# dummy AWS creds bedrock's SigV4 needs). apiKey values are dummy; there are no live secrets on the
# isolated rig. OOTB posture: no feature strips — no RUST_LOG/admin/stats overrides in gw_launch; the
# llm: block is the gateway's own multi-provider surface, wiring all mock-reachable declared providers.
# The only deviations are the permitted ones — provider baseUrls → mock and dummy keys.
gw_config() {
  local cfg="$GW_DIR/config.gen.yaml"
  echo "# ── /config.yaml (rendered; loaded via agentgateway -f /config.yaml) ──"
  [ -f "$cfg" ] || _agentgw_write_config
  cat "$cfg"
  echo
  echo "# ── launch env (non-secret; dummy AWS creds for the bedrock provider's SigV4) ──"
  cat <<ENV
AWS_ACCESS_KEY_ID=AKIAMOCKACCESSKEY
AWS_SECRET_ACCESS_KEY=mock-secret-access-key
AWS_REGION=us-east-1
ENV
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
