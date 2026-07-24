#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: TensorZero (tensorzero/gateway, Rust, docker).
#
# OpenAI-compatible Rust gateway. The multiarch image ships linux/arm64, so it runs natively on
# Graviton. Pure-proxy mode: observability OFF (no ClickHouse/Postgres), api_key_location = none, one
# model whose provider base_url is the mock. TENSORZERO_IMAGE is pinned in gateways/versions.env.
GW_KIND=docker
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="TensorZero"                      # label in charts + report tables
GW_LANG=Rust                            # implementation language → bar color bucket
GW_CLASS="Model gateway"   # the project's OWN self-description (docs: 'the TensorZero Gateway is a high-performance model gateway'), not our editorial
GW_REPO=https://github.com/tensorzero/tensorzero   # linked from the gateway name in the report table
GW_PORT=3000
GW_PATH=/openai/v1/chat/completions
GW_MODEL=tensorzero::model_name::mock
GW_AUTH=dummy
TENSORZERO_IMAGE="${TENSORZERO_IMAGE:-tensorzero/gateway:2026.6.0}"

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$TENSORZERO_IMAGE" 2>/dev/null)
  echo "${TENSORZERO_IMAGE}${dg:+ (@${dg##*@})}"
}

# _tz_write_config <provider-block-body>: emit tensorzero.toml with the given provider block for the
# single model "mock". The provider block selects the native egress dialect + its api_base/
# endpoint_url override to the mock.
_tz_write_config() {
  mkdir -p "$GW_DIR/config"
  cat > "$GW_DIR/config/tensorzero.toml" <<TOML
[gateway.observability]
enabled = false

[models.mock]
routing = ["mock"]

[models.mock.providers.mock]
$1
TOML
}

gw_build() {
  _tz_write_config 'type = "openai"
api_base = "http://127.0.0.1:'"$MOCK_PORT"'/v1"
model_name = "gpt-4o-mini"
api_key_location = "none"'
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
gw_matrix_egress() {
  case "$1" in
    openai)           _tz_write_config 'type = "openai"
api_base = "http://127.0.0.1:'"$MOCK_PORT"'/v1"
model_name = "gpt-4o-mini"
api_key_location = "none"';;
    openai-responses) _tz_write_config 'type = "openai"
api_type = "responses"
api_base = "http://127.0.0.1:'"$MOCK_PORT"'/v1"
model_name = "gpt-4o-mini"
api_key_location = "none"';;
    anthropic)        _tz_write_config 'type = "anthropic"
api_base = "http://127.0.0.1:'"$MOCK_PORT"'/v1/"
model_name = "claude-3-5-sonnet-20241022"';;
    bedrock)          _tz_write_config 'type = "aws_bedrock"
endpoint_url = "http://127.0.0.1:'"$MOCK_PORT"'"
model_id = "anthropic.claude-3-sonnet-20240229-v1:0"
region = "us-east-1"';;
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
  sudo docker run -d --name tensorzero-bench --network host --cpuset-cpus="$CORES" \
    -e TENSORZERO_DISABLE_PSEUDONYMOUS_USAGE_ANALYTICS=1 \
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
# never a live secret — there are none on the isolated rig). The suite runner captures this once per run
# into results/config/tensorzero.txt and the board publishes it, so "fresh install + this config → these
# numbers" is reproducible. The toml is read from the file gw_build/_tz_write_config just rendered (falls
# back to rendering the openai-lane default if the file isn't present yet), so it can never drift from
# what the gateway actually loaded. OOTB posture: the only non-default line is observability=false, which
# is the required run-mechanic to avoid a ClickHouse/Postgres dependency (embedded/no external store);
# TENSORZERO_DISABLE_PSEUDONYMOUS_USAGE_ANALYTICS=1 is the allowed telemetry-off run-mechanic. No feature
# strips or perf tuning are present.
gw_config() {
  local toml="$GW_DIR/config/tensorzero.toml"
  echo "# ── tensorzero.toml (rendered; loaded via --config-file config/tensorzero.toml) ──"
  if [ -f "$toml" ]; then
    cat "$toml"
  else
    _tz_write_config 'type = "openai"
api_base = "http://127.0.0.1:'"$MOCK_PORT"'/v1"
model_name = "gpt-4o-mini"
api_key_location = "none"'
    cat "$toml"
  fi
  echo
  echo "# ── launch env (non-secret; credential values are dummy on the isolated rig) ──"
  cat <<ENV
TENSORZERO_DISABLE_PSEUDONYMOUS_USAGE_ANALYTICS=1
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

# gw_matrix_egress + the declared capability matrix are defined above (before gw_launch). The
# non-openai egress columns are wired-pending-field-verification; the EC2 field run turns each
# declared-1 cell green or red. Every grey cell is a cited capability limit.
