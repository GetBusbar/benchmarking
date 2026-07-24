#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: GoModel (ENTERPILOT/GOModel, Go, docker).
#
# OpenAI + Anthropic-compatible Go gateway. We override the openai provider's base URL to the mock
# via OPENAI_BASE_URL, so /v1/chat/completions forwards there. Left unprotected (GOMODEL_MASTER_KEY
# unset) for a pure proxy-overhead measurement — the default posture. Image pinned in
# gateways/versions.env; the resolved tag is recorded in the result.
GW_KIND=docker
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="GoModel"                      # label in charts + report tables
GW_LANG=Go                            # implementation language → bar color bucket
GW_CLASS="Gateway"   # the project's OWN self-description (no unambiguous self-description found; neutral fallback), not our editorial
GW_REPO=https://github.com/ENTERPILOT/GOModel   # linked from the gateway name in the report table
GW_PORT=8080
GW_PATH=/v1/chat/completions
# Provider-QUALIFIED model: GoModel discovers models from every configured provider's /models and
# routes a BARE name by alphabetical-first provider. Our bench points all four provider base URLs at
# one mock whose /models returns the same catalog everywhere, so a bare `gpt-4o-mini` is registered
# under multiple providers and misroutes to `anthropic`. The `openai/` prefix is GoModel's own
# disambiguation (how you run it against multiple real providers) and pins it to the openai upstream.
GW_MODEL=openai/gpt-4o-mini
GW_AUTH=dummy

GOMODEL_IMAGE="${GOMODEL_IMAGE:-enterpilot/gomodel:0.1.55}"

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$GOMODEL_IMAGE" 2>/dev/null)
  echo "${GOMODEL_IMAGE}${dg:+ (@${dg##*@})}"
}

gw_build() {
  sudo docker pull "$GOMODEL_IMAGE" >/dev/null 2>&1 || true
}

gw_launch() {
  sudo docker rm -f gomodel-bench >/dev/null 2>&1; sleep 1
  # Point EVERY provider's base URL at the mock: GoModel routes by model name to the matching native
  # adapter (OPENAI/ANTHROPIC/GEMINI/BEDROCK_BASE_URL are separate env knobs), each of which emits
  # that provider's native upstream shape. Which provider a request hits is chosen by GW_MODEL, set
  # per egress in gw_matrix_egress. Wiring all four here keeps gw_launch (openai) and the matrix
  # relaunches identical except for the model.
  # GoModel discovers models AT BOOT from each provider's model-list endpoint and 404s any request
  # whose model is not in that registry (registry_init.go fetchAllProviderModels, router.go
  # "model not found") - so the base URLs below must make the mock answer each provider's list with
  # that provider's OWN catalog, and GW_MODEL must be a name from it. Previously ANTHROPIC_BASE_URL
  # had no provider marker, so the mock answered its openai catalog and no claude model was ever
  # registered -> every anthropic-egress request 404ed at warm-up, which we mispublished as a
  # GoModel failure. /anthropic in the base path is the mock's provider marker (the anthropic
  # adapter appends /models?limit=1000 to the base, anthropic.go:229). Bedrock discovery is the
  # SigV4 control-plane ListFoundationModels call the mock does not implement; GoModel's own
  # documented escape hatch is the BEDROCK_MODELS allowlist (.env.template), used verbatim.
  # All four egress paths verified locally against enterpilot/gomodel:0.1.55 + the recording mock.
  sudo docker run -d --name gomodel-bench --network host --cpuset-cpus="$CORES" \
    -e PORT="$GW_PORT" \
    -e OPENAI_BASE_URL="http://127.0.0.1:$MOCK_PORT/v1" \
    -e OPENAI_API_KEY=dummy \
    -e ANTHROPIC_BASE_URL="http://127.0.0.1:$MOCK_PORT/anthropic" \
    -e ANTHROPIC_API_KEY=dummy \
    -e GEMINI_BASE_URL="http://127.0.0.1:$MOCK_PORT/v1beta" \
    -e GEMINI_API_KEY=dummy \
    -e BEDROCK_BASE_URL="http://127.0.0.1:$MOCK_PORT" \
    -e BEDROCK_MODELS="anthropic.claude-3-sonnet-20240229-v1:0" \
    -e AWS_ACCESS_KEY_ID=AKIAMOCKACCESSKEY -e AWS_SECRET_ACCESS_KEY=mock-secret-access-key -e AWS_REGION=us-east-1 \
    -e MODELS_ENABLED_BY_DEFAULT=true \
    -e STORAGE_TYPE=sqlite \
    "$GOMODEL_IMAGE" >"$GW_DIR/launch.log" 2>&1 || true
}

# ── OOTB config artifact (env-driven) ─────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config this gateway launches with. GoModel is env-driven, so the
# artifact is its ENV MANIFEST: the exact `KEY=value` list gw_launch passes with `-e`, one per line,
# secrets already shown as their dummy values (there are no live secrets on the isolated rig). The
# suite runner captures this once per run into results/config/gomodel.txt and the board publishes it,
# so "fresh install + this env → these numbers" is reproducible. Kept in lockstep with gw_launch by
# construction: the same values, sourced from the same $GW_PORT/$MOCK_PORT/$GW_MODEL the launch uses.
# OOTB posture: default features stay ON (no LOGGING_ENABLED/budget/ratelimit/admin/mcp strips); the
# only deviations are the four permitted ones — provider base_urls → mock, dummy keys, OpenAI-lane
# provider scope, and the STORAGE_TYPE=sqlite / MODELS_ENABLED_BY_DEFAULT run-mechanics.
gw_config() {
  cat <<ENV
PORT=$GW_PORT
OPENAI_BASE_URL=http://127.0.0.1:$MOCK_PORT/v1
OPENAI_API_KEY=dummy
ANTHROPIC_BASE_URL=http://127.0.0.1:$MOCK_PORT/anthropic
ANTHROPIC_API_KEY=dummy
GEMINI_BASE_URL=http://127.0.0.1:$MOCK_PORT/v1beta
GEMINI_API_KEY=dummy
BEDROCK_BASE_URL=http://127.0.0.1:$MOCK_PORT
BEDROCK_MODELS=anthropic.claude-3-sonnet-20240229-v1:0
AWS_ACCESS_KEY_ID=AKIAMOCKACCESSKEY
AWS_SECRET_ACCESS_KEY=mock-secret-access-key
AWS_REGION=us-east-1
MODELS_ENABLED_BY_DEFAULT=true
STORAGE_TYPE=sqlite
ENV
}

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock. GoModel 0.1.55 has native provider adapters for openai, anthropic, gemini and
# bedrock, each with its own <PROVIDER>_BASE_URL env override (internal/providers/config.go); the
# OpenAI ingress is routed by model name to the matching adapter, which emits that provider's native
# upstream shape (anthropic -> /messages, gemini native generateContent with
# USE_GOOGLE_GEMINI_NATIVE_API on by default, bedrock Converse). It also serves /v1/messages
# (Anthropic-format ingress) and /v1/responses (Responses-format ingress, via the responses->chat
# adapter, internal/providers/responses_adapter.go) as their own ingress surfaces. NOT declared:
# cohere (no cohere adapter and no COHERE_BASE_URL exists in the repo at all), and openai-chat ->
# responses-upstream (no ChatViaResponses bridge exists anywhere in the tree - the earlier declared
# 1 there manufactured a red GoModel never claimed). Declared-1 cells beyond the openai row were
# each verified locally against 0.1.55 + the recording mock:
#   responses->openai-responses (the openai provider serves Responses natively at {base}/responses),
#   responses->anthropic (ResponsesViaChat -> native /messages upstream),
#   anthropic->openai (/v1/messages ingress translated to the chat upstream),
#   anthropic->anthropic (native /messages round trip).
# Evidence: .env.template + internal/providers/config.go (per-provider BASE_URL vars, no cohere),
# internal/providers/responses_adapter.go (ResponsesViaChat, no ChatViaResponses), local runs.
GW_MATRIX_CAP="
101101
011000
101000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="GoModel 0.1.55 has no Cohere adapter (no COHERE_BASE_URL in the repo) and no chat-to-Responses bridge (responses_adapter.go implements ResponsesViaChat only); those cells are grey by that capability limit"
GW_MATRIX_EGRESS="openai openai-responses anthropic gemini bedrock"
gw_matrix_egress() {
  # Models must exist in the boot-time registry (see gw_launch): the mock's anthropic catalog lists
  # claude-3-5-sonnet (undated), and the provider/model prefix form is GoModel's own disambiguation.
  case "$1" in
    openai|openai-responses) GW_MODEL=openai/gpt-4o-mini;;
    anthropic)               GW_MODEL=anthropic/claude-3-5-sonnet;;
    gemini)                  GW_MODEL=gemini-1.5-pro;;
    bedrock)                 GW_MODEL=anthropic.claude-3-sonnet-20240229-v1:0;;
    *) return 1;;
  esac
  gw_launch
}

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=gomodel-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 gomodel-bench 2>&1
}

gw_rss() { container_rss_mib gomodel-bench; }  # summed process-tree VmRSS (same method as native gateways)
gw_hwm() { container_hwm_mib gomodel-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_stop() { sudo docker rm -f gomodel-bench >/dev/null 2>&1; }

# gw_matrix_egress + the declared capability matrix are defined above (in gw_launch). The non-openai
# egress columns are wired-pending-field-verification; the EC2 field run turns each declared-1 cell
# green or red. Every grey cell is a cited capability limit.
