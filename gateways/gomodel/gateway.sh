#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: GoModel (ENTERPILOT/GOModel, Go, docker).
#
# OpenAI + Anthropic-compatible Go gateway. We override the openai provider's base URL to the mock
# via OPENAI_BASE_URL, so /v1/chat/completions forwards there. Left unprotected (GOMODEL_MASTER_KEY
# unset) for a pure proxy-overhead measurement - the default posture. Image pinned in
# below in this file; the resolved tag is recorded in the result.
#
# ── NOTE ON ITS NUMBERS: GoModel AUDIT-LOGS EVERY REQUEST BY DEFAULT ─────────────────────────────
# Same disclosure another entry carries, for the same reason. GoModel ships audit logging ON: LOGGING_ENABLED
# defaults true, and so do LOGGING_LOG_BODIES and LOGGING_LOG_HEADERS (.env.template:275-281 - "When
# enabled, all requests and responses are logged to the configured storage"; config/logging.go). With
# the default STORAGE_TYPE=sqlite that means a per-request entry - full request AND response body, plus
# headers - captured on the request path (internal/auditlog/middleware.go captureLoggedRequestBody /
# captureLoggedResponseBody) and batch-written by the flush loop (internal/auditlog/logger.go). Its
# latency/throughput therefore reflect A GATEWAY THAT AUDIT-LOGS EVERY CALL WITH BODIES, not a bare
# proxy - the honest measurement of GoModel as it ships. There is no external infra involved (sqlite in
# the image's own /app/data, which the upstream Dockerfile creates and does not mount as a volume), so
# turning it off would be a forbidden feature strip, not a permitted run-mechanic.
#
# ***RUN-OVER-RUN COMPARISONS ACROSS 2026-07-24 ARE INVALID FOR GOMODEL - DO NOT PUBLISH THE DELTA AS A
# GOMODEL REGRESSION.*** Every run up to and including 2026-07-24T01:20Z was measured with an
# `-e LOGGING_ENABLED=false` FEATURE STRIP that no other gateway in the field received; commit 2951b97
# ("bench(ootb): config-transparency mechanism") removed that strip later the same day, correctly. The
# image never changed - results/history/gomodel.jsonl records the identical
# enterpilot/gomodel:0.1.55 @sha256:606151f9…b562ac digest on BOTH sides of the boundary - and the
# 2026-07-25 rig floor was healthy (direct_c1_p99_us 74→81us, +7..9%). So the observed
# rps_max_proxy 16000→2576 and added p99 306→2552us is NOT a GoModel regression and NOT box noise: it
# is the cost of the audit-log feature the strip used to hide. The pre-07-24 numbers were the
# flattering ones. The load-sweep signature agrees - throughput pins at ~2500rps flat from c=32 to
# c=512 (results/fanout-gomodel.log), a saturation ceiling, not a scheduling artifact.
# STILL UNEXPLAINED (flagged, not fixed): the openai-responses→openai-responses cell measured ~100rps
# with p50 added ≈10.09ms, ~25x worse than the other served cells and flat across concurrency. That is
# a second, distinct serialization we have not root-caused; treat that ONE cell's perf as suspect until
# a targeted re-measure explains it.
GW_KIND=docker
# The docker container this manifest launches under. DECLARED here so that anything outside this
# directory which needs the name (the local verifier's teardown, for one) READS it from the
# manifest instead of hardcoding it. lib/gateway_isolation_test.sh checks it against the --name
# below, so the two cannot drift.
GW_CONTAINER=gomodel-bench
# Self-describing manifest metadata - charts.py + the run lists read these, so a gateway
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

# ── CONFIG NECESSITY (lib/gateway_config_lint.sh) ─────────────────────────────────────────────────
# Every setting this manifest writes, and the ONE reason it is here. The lint fails on a setting with
# no claim AND on a claim with no setting, so this block cannot drift from the config in either
# direction. Reasons: boot (it will not run without this) | upstream (points an upstream at the test
# mock) | ingress (an ingress path the 6x6 matrix drives) | bind (the port or CPU pin the rig needs).
GW_CONFIG_WHY="
PORT       bind      # the port the harness drives
GOMAXPROCS bind      # Go pre-1.25 reads the HOST cpu count, not the --cpuset-cpus limit
OPENAI_BASE_URL    upstream  # each provider adapter has its own base-URL override; all -> the mock
ANTHROPIC_BASE_URL upstream  # the path segment is the mock's provider marker, so boot-time model
GEMINI_BASE_URL    upstream  # discovery gets that provider's OWN catalog (see gw_launch)
BEDROCK_BASE_URL   upstream
BEDROCK_MODELS     upstream  # bedrock discovery is a SigV4 control-plane call the mock does not
                             # implement; this allowlist is GoModel's own documented escape hatch
OPENAI_API_KEY    boot   # a provider with no key is not configured (dummy; the mock ignores it)
ANTHROPIC_API_KEY boot
GEMINI_API_KEY    boot
AWS_ACCESS_KEY_ID     boot
AWS_SECRET_ACCESS_KEY boot
AWS_REGION            boot
"

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
  # SINGLE SOURCE OF TRUTH: _gomodel_env() is the ONE definition of the OOTB env. gw_launch turns each
  # KEY=value into a docker -e flag; gw_config publishes the SAME bytes. The benchmarked config and the
  # website-published config are therefore identical by construction and cannot drift.
  local args=() line
  while IFS= read -r line; do [ -n "$line" ] && args+=(-e "$line"); done < <(_gomodel_env)
  sudo docker run -d --name gomodel-bench --network host --cpuset-cpus="$CORES" \
    "${args[@]}" \
    "$GOMODEL_IMAGE" >"$GW_DIR/launch.log" 2>&1 || true
}

# ── OOTB config (SINGLE SOURCE: the benchmark run and the published website artifact both read this) ─
# _gomodel_env is the ONE canonical OOTB env manifest. gw_launch consumes it as docker -e flags;
# gw_config prints it verbatim into results/config/gomodel.txt, which the board publishes - so
# "fresh install + this env → these numbers" is reproducible and the run can never differ from the
# published config. Everything is env-driven, so this function IS the whole config.
#   GOMAXPROCS = pinned core count (0-3 → 4): GoModel is Go, and Go (pre-1.25) reads the HOST cpu count
#     for GOMAXPROCS, NOT the --cpuset-cpus limit - so without it GoModel runs 16 Ps thrashing 4 pinned
#     cores, a scheduler-contention HANDICAP the Rust gateways (tokio available_parallelism respects
#     cpuset) never pay. Pinning to the cpuset count emulates the same 4-core box every gateway is
#     measured on, the identical CPU-pinning run-mechanic another Go entry also uses (field parity).
#   OOTB posture: default features stay ON (no LOGGING_ENABLED/budget/ratelimit/admin/mcp strips); the
#     only deviations are the permitted ones - provider base_urls → mock and dummy keys.
#     STORAGE_TYPE=sqlite and MODELS_ENABLED_BY_DEFAULT=true were REMOVED: .env.template documents
#     both values as GoModel's own defaults ("Storage type: sqlite (default)"; "default: true"), so
#     each line configured nothing and would have become a silent override the day upstream moved
#     its default. Storage and model-enablement now come from the application, not from us.
#     LOGGING_ENABLED is DELIBERATELY ABSENT so GoModel's default audit logging (bodies + headers →
#     sqlite, on the request path) stays on. Re-adding LOGGING_ENABLED=false here would restore the
#     pre-2026-07-24 feature strip and silently re-inflate its throughput ~6x - see the "NOTE ON ITS
#     NUMBERS" block at the top of this file before touching this list.
_gomodel_env() {
  local ncore=$(( ${CORES##*-} - ${CORES%%-*} + 1 ))
  cat <<ENV
GOMAXPROCS=$ncore
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
ENV
}

gw_config() { _gomodel_env; }

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
