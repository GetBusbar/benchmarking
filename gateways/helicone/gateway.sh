#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Helicone AI Gateway (Helicone/ai-gateway, Rust) — BUILT FROM SOURCE, run native.
#
# Helicone publishes no linux/arm64 image (`helicone/ai-gateway:latest` is amd64-only), so on Graviton
# we build the `ai-gateway` crate from source — exactly the pattern we use for LiteLLM-Rust — and run
# the release binary natively (real process RSS, no container overhead). Refs are pinned in
# gateways/versions.env. Pure-proxy mode: helicone.features=none → no control plane, no auth, no key
# required; the built-in `openai` provider's base-url is overridden to the mock.
GW_KIND=native
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="Helicone"                      # label in charts + report tables
GW_LANG=Rust                            # implementation language → bar color bucket
GW_CLASS="AI gateway"   # the project's OWN self-description (README: 'the fastest, lightest AI gateway'), not our editorial
GW_REPO=https://github.com/Helicone/ai-gateway   # linked from the gateway name in the report table
GW_PORT=8787
GW_PATH=/router/default/chat/completions
# Helicone's router requires the unified "{provider}/{model}" form (it errors on a bare model name);
# the mock ignores the model and answers the OpenAI shape regardless.
GW_MODEL=openai/gpt-4o-mini
GW_AUTH=dummy
HELICONE_SRC="${HELICONE_SRC:-$HOME/helicone-ai-gateway-src}"

gw_build() {
  command -v cargo >/dev/null || { echo "need cargo (rust) for helicone"; return 1; }
  if [ ! -d "$HELICONE_SRC" ]; then
    git clone "${HELICONE_REPO}" "$HELICONE_SRC" || return 1
    [ -n "${HELICONE_COMMIT:-}" ] && git -C "$HELICONE_SRC" checkout -q "$HELICONE_COMMIT"
  fi
  # Release build uses LTO + codegen-units=1 (slow, RAM-hungry) — fine on the 8-core bench box.
  ( cd "$HELICONE_SRC" && cargo build --release -p ai-gateway ) || return 1
  HELICONE_BIN="$HELICONE_SRC/target/release/ai-gateway"
  [ -x "$HELICONE_BIN" ] || { echo "helicone binary not found after build"; return 1; }
}

gw_version() {
  local sha; sha="$(git -C "$HELICONE_SRC" rev-parse --short HEAD 2>/dev/null)"
  echo "Helicone/ai-gateway@${sha:-?} (source build)"
}

# _helicone_launch <provider>: write the router config wiring exactly ONE provider (base-url ->
# mock) and boot the binary. Helicone appends the provider's native path to base-url, so pointing it
# at the mock host makes each provider POST that dialect's native upstream shape to the mock.
_helicone_launch() {
  local prov="$1"
  cat > "$GW_DIR/config.gen.yaml" <<YAML
server:
  address: 0.0.0.0
  port: $GW_PORT
helicone:
  features: none
providers:
  $prov:
    base-url: "http://127.0.0.1:$MOCK_PORT/"
routers:
  default:
    load-balance:
      chat:
        strategy: weighted
        providers:
          - provider: $prov
            weight: 1.0
YAML
  pkill -f 'target/release/ai-gateway' 2>/dev/null; sleep 1
  # AWS creds: helicone's bedrock SigV4 signer reads AWS_ACCESS_KEY / AWS_SECRET_KEY (NOT the SDK's
  # AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY - src/types/provider.rs:218-229 builds
  # ProviderKey::AwsCredentials from those two names, region from AWS_REGION). With them absent,
  # extract_and_sign_aws_headers returns AuthError::InvalidCredentials -> HTTP 401 before any
  # egress, which we mispublished as a Helicone bedrock failure. Both spellings are exported so
  # any code path resolves; the mock ignores the signature.
  setsid taskset -c "$CORES" env \
    OPENAI_API_KEY=sk-dummy ANTHROPIC_API_KEY=sk-dummy GEMINI_API_KEY=sk-dummy \
    AWS_ACCESS_KEY=AKIAMOCKACCESSKEY AWS_SECRET_KEY=mock-secret-access-key \
    AWS_ACCESS_KEY_ID=AKIAMOCKACCESSKEY AWS_SECRET_ACCESS_KEY=mock-secret-access-key AWS_REGION=us-east-1 \
    AI_GATEWAY__SERVER__PORT="$GW_PORT" \
    AI_GATEWAY__SERVER__ADDRESS=0.0.0.0 \
    "$HELICONE_BIN" -c "$GW_DIR/config.gen.yaml" </dev/null >/tmp/helicone.bench.log 2>&1 &
}

gw_launch() {
  # base-url gets "v1/chat/completions" appended by the gateway → hits the mock's OpenAI endpoint.
  _helicone_launch openai
}

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock. Helicone accepts the OpenAI-canonical ingress and translates it to the routed
# provider's NATIVE upstream shape for anthropic (AnthropicConverter -> /v1/messages) and bedrock
# (BedrockConverter -> model/{id}/converse), each provider's base-url overridable to the mock. So
# the capable row is openai-ingress into {openai, anthropic, bedrock}. NOT declared:
#   gemini - despite the endpoint's name, Google's mapping targets Gemini's OPENAI-COMPAT surface,
#     not native generateContent: endpoints/google/generate_contents.rs pins
#     PATH = "v1beta/openai/chat/completions" with OpenAI request/response types (its own test in
#     tests/single_provider.rs:62 confirms the proxied target). This suite's gemini egress means
#     the native generateContent dialect, so declaring it manufactured an 'untranslated
#     passthrough' red for behavior Helicone never claimed;
#   openai-responses - the OpenAI endpoint enum has only ChatCompletions, no Responses converter;
#   cohere - no cohere dialect (cohere appears only as a Bedrock model family).
# Evidence: ai-gateway/src/types/provider.rs (InferenceProvider enum), middleware/mapper/registry.rs
# (converters), endpoints/openai/mod.rs (ChatCompletions only), endpoints/google/generate_contents.rs
# (OpenAI-compat path), commit 9649b27.
GW_MATRIX_CAP="
101001
000000
000000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="Helicone AI Gateway has no OpenAI-Responses converter (endpoints/openai only ChatCompletions), no native Cohere dialect, and its Gemini egress targets Google's OpenAI-compat surface v1beta/openai/chat/completions rather than native generateContent (endpoints/google/generate_contents.rs); those cells are grey by that capability limit"
GW_MATRIX_EGRESS="openai anthropic bedrock"

# ── xlate lane: not declared (no anthropic-format ingress) ───────────────────────────────────────
# Ingress is OpenAI-format only: the router's EndpointRoute defines exactly (ChatCompletions,
# "chat/completions") and ApiEndpoint::new only constructs an OpenAI source endpoint
# (src/endpoints/mod.rs:58-85, src/router/service.rs:138). A bare /v1/messages parses its first
# segment as a provider name ("Unsupported provider: v1", src/router/meta.rs) and the only
# anthropic-format path is the UNMAPPED /anthropic/... direct passthrough (router/direct.rs) - no
# anthropic->openai translation exists on any route, so the lane is not a claimed capability.
GW_XLATE_CAP=0
GW_XLATE_CAP_NOTE="Helicone AI Gateway ingress is OpenAI-format only (EndpointRoute registers only chat/completions; ApiEndpoint::new constructs only an OpenAI source endpoint, endpoints/mod.rs:58-85); anthropic-format requests exist only as the unmapped /anthropic passthrough, so anthropic-in -> openai-out translation is not a claimed capability (commit 9649b27)"
gw_matrix_egress() {
  case "$1" in
    openai)    GW_MODEL="openai/gpt-4o-mini";                      _helicone_launch openai;;
    anthropic) GW_MODEL="anthropic/claude-3-5-sonnet";            _helicone_launch anthropic;;
    gemini)    GW_MODEL="gemini/gemini-2.5-flash";                _helicone_launch gemini;;
    bedrock)   GW_MODEL="bedrock/anthropic.claude-3-5-sonnet-v1:0"; _helicone_launch bedrock;;
    *) return 1;;
  esac
}

gw_rss() { awk '/VmRSS/{printf "%.1f", $2/1024}' "/proc/$(pgrep -f 'target/release/ai-gateway' | head -1)/status" 2>/dev/null; }
gw_hwm() { _hwm_tree_mib "$(pgrep -f 'target/release/ai-gateway' 2>/dev/null | head -1)"; }  # kernel VmHWM of the ai-gateway tree

gw_diag() {
  echo "proc: $(pgrep -af 'target/release/ai-gateway' | head -c 200)"
  echo "run.log:"; tail -n 25 /tmp/helicone.bench.log 2>/dev/null
}

gw_stop() { pkill -f 'target/release/ai-gateway' 2>/dev/null; }

# gw_matrix_egress + the declared capability matrix are defined above (before gw_launch). The
# anthropic/gemini/bedrock egress columns are wired-pending-field-verification; the EC2 field run
# turns each declared-1 cell green or red. Every grey cell is a cited capability limit.
