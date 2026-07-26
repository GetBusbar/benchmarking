#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: TensorZero (tensorzero/gateway, Rust, docker).
#
# OpenAI-compatible Rust gateway. The multiarch image ships linux/arm64, so it runs natively on
# Graviton. Pure-proxy mode: observability OFF (no ClickHouse/Postgres). TENSORZERO_IMAGE is pinned
# below in this file.
#
# ── ONE STATIC CONFIG (the standard) ──────────────────────────────────────────────────────────────
# ONE tensorzero.toml, rendered IDENTICALLY for every lane and every egress column, published verbatim
# as the artifact: install this toml with that image on that box and you reproduce the board. The
# config declares ALL FOUR upstream models at once (openai, openai-responses, anthropic, aws_bedrock);
# the matrix changes only WHAT THE CLIENT ASKS FOR - the `model` field in the request body.
#
# Upstream evidence that one config is enough (tensorzero @2026.6.0):
#   * multiple models, provider-agnostic: docs/gateway/configuration-reference.mdx:362-367 - "You can
#     define multiple models by including multiple [models.model_name] sections. A model is provider
#     agnostic." No limit of one provider type per config exists in the source.
#   * client-side selection with NO config change: docs/gateway/api-reference/
#     inference-openai-compatible.mdx:575-616 - a model declared as [models.my_model] is addressed
#     from the OpenAI-compatible endpoint as `tensorzero::model_name::my_model`. That is exactly what
#     GW_MODEL carries, and the only thing gw_matrix_egress touches.
#   * provider blocks + their native upstream paths: openai `api_type` enum {chat_completions,
#     responses} (crates/tensorzero-core/src/providers/openai/mod.rs:94-103; get_chat_url joins
#     `chat/completions` @mod.rs:1263, get_responses_url joins `responses` @responses.rs:298);
#     anthropic api_base is the API ROOT and the gateway appends `messages`
#     (providers/anthropic.rs:57-61 + get_messages_url:116-126 - note the configuration-reference
#     docs are STALE here and say to include /messages; the source strips it with a deprecation
#     warning, so we pass the root); aws_bedrock builds `{endpoint_url}/model/{urlencoded model_id}/
#     converse` (aws_bedrock.rs:234-238).
#   * credentials are validated EAGERLY at boot for EVERY declared model (model_table.rs:568-590
#     load_credential -> ApiKeyMissing; skip_credential_validation is test/relay-only), so a
#     four-model config only boots if every model's credential resolves - the launch env below
#     supplies exactly those dummies, and the local verification at the bottom of this file is the
#     proof that it does boot and serve all four.
GW_KIND=docker
# The docker container this manifest launches under. DECLARED here so that anything outside this
# directory which needs the name (the local verifier's teardown, for one) READS it from the
# manifest instead of hardcoding it. lib/gateway_isolation_test.sh checks it against the --name
# below, so the two cannot drift.
GW_CONTAINER=tensorzero-bench
# Self-describing manifest metadata - charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="TensorZero"                      # label in charts + report tables
GW_LANG=Rust                            # implementation language → bar color bucket
GW_CLASS="Model gateway"   # the project's OWN self-description (docs: 'the TensorZero Gateway is a high-performance model gateway'), not our editorial
GW_REPO=https://github.com/tensorzero/tensorzero   # linked from the gateway name in the report table
GW_PORT=3000
GW_PATH=/openai/v1/chat/completions
# The client-facing model selector. Every lane's baseline is the canonical OpenAI model; the matrix's
# egress columns swap ONLY this string (all four models live in the one config below).
GW_MODEL=tensorzero::model_name::mock-openai
GW_AUTH=dummy

# ── CONFIG NECESSITY (lib/gateway_config_lint.sh) ─────────────────────────────────────────────────
# Every setting this manifest writes, and the ONE reason it is here. The lint fails on a setting with
# no claim AND on a claim with no setting, so this block cannot drift from the config in either
# direction. Reasons: boot (it will not run without this) | upstream (points an upstream at the test
# mock) | ingress (an ingress path the 6x6 matrix drives) | bind (the port or CPU pin the rig needs).
GW_CONFIG_WHY="
enabled  boot     # gateway.observability: TensorZero requires a ClickHouse URL when this is on, and
                  # we run no ClickHouse on a single box, so it cannot boot with the default
routing  boot     # a model block without a routing list is not a model
type     boot     # the provider type is required
api_base    upstream  # -> the mock
endpoint_url upstream # bedrock's own base-URL override -> the mock
api_type    upstream  # selects the Responses upstream shape on the openai provider
model_name  upstream  # the model id sent upstream
model_id    upstream  # bedrock spells the same thing this way
api_key_location boot # \"none\" is the only way to declare an openai model with no key in the env
region           boot # the bedrock signer fails without one
ANTHROPIC_API_KEY     boot  # credentials are validated EAGERLY at boot for EVERY declared model
AWS_ACCESS_KEY_ID     boot  # (model_table.rs load_credential), and the anthropic provider REJECTS
AWS_SECRET_ACCESS_KEY boot  # api_key_location=\"none\", so a dummy value here is what lets the
AWS_REGION            boot  # four-model config exist at all
"
TENSORZERO_IMAGE="${TENSORZERO_IMAGE:-tensorzero/gateway:2026.6.0}"

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$TENSORZERO_IMAGE" 2>/dev/null)
  echo "${TENSORZERO_IMAGE}${dg:+ (@${dg##*@})}"
}

# _tz_write_config: emit THE tensorzero.toml. NO ARGUMENTS - there is one config and the render is
# column-independent by construction: its only input is $MOCK_PORT (the rig's mock port, constant for
# a whole run). All four models the matrix probes are declared here at once; the client picks one per
# request with `tensorzero::model_name::<name>`, so no egress column ever rewrites these bytes.
_tz_write_config() {
  mkdir -p "$GW_DIR/config"
  cat > "$GW_DIR/config/tensorzero.toml" <<TOML
# ONE static config, four upstream models. The client selects the upstream per request with the
# OpenAI-compatible \`model\` field: tensorzero::model_name::mock-openai | mock-openai-responses |
# mock-anthropic | mock-bedrock. Nothing here changes between egress columns.
[gateway.observability]
enabled = false

# openai -> POST {api_base}/chat/completions
[models.mock-openai]
routing = ["mock"]
[models.mock-openai.providers.mock]
type = "openai"
api_base = "http://127.0.0.1:$MOCK_PORT/v1"
model_name = "gpt-4o-mini"
api_key_location = "none"

# openai + api_type="responses" -> POST {api_base}/responses
[models.mock-openai-responses]
routing = ["mock"]
[models.mock-openai-responses.providers.mock]
type = "openai"
api_type = "responses"
api_base = "http://127.0.0.1:$MOCK_PORT/v1"
model_name = "gpt-4o-mini"
api_key_location = "none"

# anthropic -> POST {api_base}messages  (api_base is the API ROOT; the gateway appends \`messages\`).
# api_key_location is left at its default env::ANTHROPIC_API_KEY: the anthropic provider REJECTS
# "none" (providers/anthropic.rs:189-208 has no Credential::None arm), and credentials are validated
# at boot, so the dummy ANTHROPIC_API_KEY in gw_launch is what lets this model exist in the config.
[models.mock-anthropic]
routing = ["mock"]
[models.mock-anthropic.providers.mock]
type = "anthropic"
api_base = "http://127.0.0.1:$MOCK_PORT/v1/"
model_name = "claude-3-5-sonnet-20241022"

# aws_bedrock -> POST {endpoint_url}/model/{urlencoded model_id}/converse (SigV4-signed with the
# dummy AWS keys in gw_launch; the mock ignores the signature).
[models.mock-bedrock]
routing = ["mock"]
[models.mock-bedrock.providers.mock]
type = "aws_bedrock"
endpoint_url = "http://127.0.0.1:$MOCK_PORT"
model_id = "anthropic.claude-3-sonnet-20240229-v1:0"
region = "us-east-1"
TOML
}

gw_build() {
  _tz_write_config
  sudo docker pull "$TENSORZERO_IMAGE" >/dev/null 2>&1 || true
}

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock. TensorZero 2026.6.0's provider `type` enum includes {openai, anthropic, aws_bedrock,
# ...} (crates/tensorzero-core/src/model.rs). The OpenAI-compat ingress (/openai/v1/chat/completions)
# is translated to the routed provider's NATIVE upstream shape: anthropic (type="anthropic", api_base
# -> {base}/messages), aws_bedrock (endpoint_url -> {url}/model/<id>/converse), and openai-responses
# (type="openai" + api_type="responses" + api_base -> {base}/responses). So the capable row is
# openai-ingress into {openai, openai-responses, anthropic, bedrock}. NOT declared: gemini (both
# google_ai_studio_gemini and gcp_vertex_gemini hardcode *.googleapis.com with NO api_base override,
# so the mock is unreachable) and cohere (no cohere provider type exists at all) - both grey with the
# cited reason.
# Evidence: model.rs (type enum + no cohere), configuration-reference.mdx (api_base/endpoint_url),
# google_ai_studio_gemini.rs (hardcoded host, no api_base), tag 2026.6.0. Wired-pending-field-verify.
GW_MATRIX_CAP="
111001
000000
000000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="TensorZero 2026.6.0 has no Cohere provider type; that cell is grey by that capability limit (model.rs)"
# Gemini is NOT incapability: TensorZero speaks Gemini in production, but both its Gemini provider
# types hardcode *.googleapis.com with no api_base override, so this rig's localhost mock cannot
# stand in - a mock-reachability limit, recorded as untestable (distinct from declared-incapable).
GW_MATRIX_UNTESTABLE="openai/gemini"
GW_MATRIX_UNTESTABLE_NOTE="TensorZero's google_ai_studio_gemini / gcp_vertex_gemini providers hardcode googleapis.com with no api_base override (google_ai_studio_gemini.rs @2026.6.0), so the harness mock cannot stand in for the upstream; TensorZero does serve Gemini in production"
GW_MATRIX_EGRESS="openai openai-responses anthropic bedrock"

# ── xlate lane: not declared (no anthropic-format ingress exists) ────────────────────────────────
# The gateway's complete external route set (crates/gateway/src/routes/external.rs @2026.6.0) is
# /inference, /batch_inference, /feedback plus the OpenAI-compatible /openai/v1/chat/completions +
# /openai/v1/embeddings (endpoints/openai_compatible/mod.rs) - there is NO /v1/messages or any
# Anthropic-Messages-format ingress, so anthropic-in -> openai-out translation is not a claimed
# capability; the 404 the probe used to publish as a failure was the router's correct answer.
GW_XLATE_CAP=0
GW_XLATE_CAP_NOTE="TensorZero exposes no Anthropic-Messages-format ingress (external.rs + openai_compatible/mod.rs @2026.6.0 register only /inference, /batch_inference, /feedback and the OpenAI-compatible chat/embeddings routes), so anthropic-in translation is not a claimed capability"
# gw_matrix_egress <dialect>: change ONLY what the CLIENT asks for. All four upstream models are
# already wired in the ONE config (_tz_write_config, rendered identically for every column); the column
# just names one of them in the request body. The config is NOT re-rendered here - the same bytes
# gw_build wrote (and gw_config publishes) serve every column.
gw_matrix_egress() {
  case "$1" in
    openai)           GW_MODEL=tensorzero::model_name::mock-openai;;
    openai-responses) GW_MODEL=tensorzero::model_name::mock-openai-responses;;
    anthropic)        GW_MODEL=tensorzero::model_name::mock-anthropic;;
    bedrock)          GW_MODEL=tensorzero::model_name::mock-bedrock;;
    *) return 1;;
  esac
  gw_launch
}

gw_launch() {
  sudo docker rm -f tensorzero-bench >/dev/null 2>&1; sleep 1
  # Credential env, required per provider type (verified against tensorzero-core @2026.6.0):
  #  - ANTHROPIC_API_KEY: the anthropic provider does NOT accept api_key_location="none"
  #    (TryFrom<Credential> in providers/anthropic.rs rejects Credential::None: "Invalid
  #    api_key_location for Anthropic provider", asserted by its own unit test) - the config
  #    default is env::ANTHROPIC_API_KEY, so a dummy value here is the supported no-real-key path.
  #    Omitting it made the gateway refuse to start, which we then mispublished as a boot failure.
  #  - AWS_*: aws_bedrock hand-signs SigV4 via the SDK default credential chain (aws_common.rs);
  #    with no resolvable credentials every request fails and surfaces as the generic 502
  #    AllVariantsFailed wrapper (tensorzero-error/src/lib.rs). Dummy keys sign fine; the mock
  #    ignores the signature. endpoint_url accepts plain http:// (no allow_http knob needed).
  # TENSORZERO_DISABLE_PSEUDONYMOUS_USAGE_ANALYTICS is DELIBERATELY ABSENT. TensorZero ships
  # pseudonymous usage analytics ON, and we measure defaults: turning a shipped-on feature off is a
  # config change we do not make in either direction. The old justification (an isolated rig where
  # the report would hang) was false - the bench boxes have full internet access, since cloud-init
  # apt-gets and pulls images over it, so the report succeeds. Consequence, disclosed rather than
  # designed around: with per-cell cold starts the gateway boots ~36 times a run, so a field run
  # sends TensorZero a few hundred install reports. That is its shipped behaviour, which is the
  # thing this benchmark measures.
  sudo docker run -d --name tensorzero-bench --network host --cpuset-cpus="$CORES" \
    -e ANTHROPIC_API_KEY=dummy \
    -e AWS_ACCESS_KEY_ID=AKIAMOCKACCESSKEY -e AWS_SECRET_ACCESS_KEY=mock-secret-access-key \
    -e AWS_REGION=us-east-1 \
    -v "$GW_DIR/config:/app/config:ro" \
    "$TENSORZERO_IMAGE" --config-file config/tensorzero.toml >"$GW_DIR/launch.log" 2>&1 || true
}

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config this gateway launches with. TensorZero is file-driven, so
# the artifact is the RENDERED tensorzero.toml (exactly what --config-file loads) PLUS the non-secret
# launch env (the credential env is dummy-only and required per provider type; shown for reproducibility,
# never a live secret - there are none on the isolated rig). The suite runner captures this once per run
# into results/config/tensorzero.txt and the board publishes it, so "fresh install + this config → these
# numbers" is reproducible - and because there is now exactly ONE config, what is published IS what ran
# in every lane and every egress column, byte for byte. The toml is read from the file
# gw_build/_tz_write_config rendered (re-rendered with the same no-argument function if the file isn't
# present yet), so it can never drift from
# what the gateway actually loaded. OOTB posture: the only non-provider line is observability=false, and
# the gateway will not boot with it on because observability requires a ClickHouse URL we do not run.
# Usage analytics are left at their shipped default (on) - see the note in gw_launch. No feature strips
# or perf tuning are present.
gw_config() {
  local toml="$GW_DIR/config/tensorzero.toml"
  echo "# ── tensorzero.toml (rendered; loaded via --config-file config/tensorzero.toml) ──"
  [ -f "$toml" ] || _tz_write_config
  cat "$toml"
  echo
  echo "# ── launch env (non-secret; credential values are dummy on the isolated rig) ──"
  cat <<ENV
ANTHROPIC_API_KEY=dummy
AWS_ACCESS_KEY_ID=AKIAMOCKACCESSKEY
AWS_SECRET_ACCESS_KEY=mock-secret-access-key
AWS_REGION=us-east-1
ENV
}

gw_rss() { container_rss_mib tensorzero-bench; }  # summed process-tree VmRSS (same method as native gateways)
gw_hwm() { container_hwm_mib tensorzero-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=tensorzero-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 tensorzero-bench 2>&1
}

gw_stop() { sudo docker rm -f tensorzero-bench >/dev/null 2>&1; }

# gw_matrix_egress + the declared capability matrix are defined above (before gw_launch).
#
# LOCAL VERIFICATION of the one-config standard (tensorzero/gateway:2026.6.0 + the pinned recording
# mock, --network host, THIS manifest's rendered tensorzero.toml, config bytes untouched between the
# four probes - same file sha256 before and after): one POST /openai/v1/chat/completions per column,
# differing ONLY in the body's `model` string, read back from the mock's /__mock/state recorder:
#   tensorzero::model_name::mock-openai           -> HTTP 200  upstream openai
#                                                    body_ok=true  /v1/chat/completions
#   tensorzero::model_name::mock-openai-responses -> HTTP 200  upstream openai-responses
#                                                    body_ok=true  /v1/responses
#   tensorzero::model_name::mock-anthropic        -> HTTP 200  upstream anthropic
#                                                    body_ok=true  /v1/messages
#   tensorzero::model_name::mock-bedrock          -> HTTP 200  upstream bedrock
#                                                    body_ok=true  /model/anthropic.claude-3-sonnet-20240229-v1%3A0/converse
# Four native upstream dialects, ONE config, zero reconfiguration. The EC2 field run still turns each
# declared-1 CELL green or red under load; every grey cell is a cited capability limit.
