#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Helicone AI Gateway (Helicone/ai-gateway, Rust) — BUILT FROM SOURCE, run native.
#
# EXCEPTION to the everything-runs-its-official-docker-image rule, kept deliberately: Helicone
# publishes no linux/arm64 image at all. Re-verified 2026-07-23 — EVERY tag on
# docker.io/helicone/ai-gateway (latest, main, all sha-<commit> and versioned tags, including the
# sha tag for the pinned commit below) is linux/amd64-only, and ghcr.io/helicone/ai-gateway serves
# no public image. The bench boxes are Graviton arm64, so on them we build the `ai-gateway` crate
# from source — exactly the pattern we use for LiteLLM-Rust — and run the release binary natively
# (real process RSS, no container overhead). Convert to the official image if/when Helicone ships
# arm64. Refs are pinned in gateways/versions.env.
#
# ── OOTB posture (one-config standard) ────────────────────────────────────────────────────────────
# This is the config a real user deploys, used unchanged for EVERY lane. Helicone runs at its as-
# shipped defaults; the only deviations are the permitted ones:
#   * each provider's base-url → the mock (all mock-reachable dialects wired below; the matrix
#     exercises them and memory/throughput are measured on this same all-providers config, NOT scoped
#     per-lane);
#   * dummy provider keys / AWS signing material (the mock ignores them);
#   * telemetry.exporter: stdout — the disclosed telemetry-off run-mechanic (it is ALSO the default,
#     so this only pins that no OTLP egress can happen on the isolated rig).
# NOT a strip — kept because it IS the default: `helicone.features: none`. Verified against the config
# structs at the pinned commit (config/helicone.rs): HeliconeFeatures defaults to None whether the key
# is present or omitted. None = no auth checks and NO control-plane/websocket calls to api.helicone.ai,
# so the gateway boots UNPROTECTED and makes no outbound connection except to the upstream provider —
# exactly the OOTB default. It is written explicitly here only for transparency; deleting it would be
# behavior-identical. Helicone ships unprotected, so we keep it unprotected (GW_AUTH is a dummy bearer
# the open router ignores). No feature is disabled to save RAM/latency; no perf knob is set.
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

# This gateway builds from source, so ITS box (and only its box) installs the Rust toolchain + git +
# build deps. The base image ships bare OS + docker only (see run-on-ec2.sh); docker gateways never
# pay for this. Idempotent - a no-op once the toolchain is present.
gw_prereqs() {
  command -v cargo >/dev/null && command -v git >/dev/null && return 0
  sudo apt-get install -y -q git build-essential pkg-config libssl-dev >/dev/null 2>&1 || true
  command -v cargo >/dev/null || (curl -sSf https://sh.rustup.rs | sh -s -- -y >/dev/null 2>&1)
  . "$HOME/.cargo/env" 2>/dev/null || true
  command -v cargo >/dev/null || { echo "helicone: rust toolchain unavailable"; return 1; }
}

gw_build() {
  gw_prereqs || return 1
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

# _helicone_write_config: render the ONE OOTB router config. Every dialect Helicone can translate AND
# whose base-url the mock can stand in for is wired here (all → mock), load-balanced under one router.
# Helicone appends each provider's native path to base-url, so pointing every provider at the mock host
# makes each POST that dialect's native upstream shape. Providers wired: openai, anthropic, bedrock —
# Helicone's first-class translating dialects reachable via a base-url override (types/provider.rs
# InferenceProvider). Gemini is deliberately NOT wired: Helicone's Gemini egress targets Google's
# OpenAI-COMPAT surface (v1beta/openai/chat/completions), not native generateContent, so it is a
# capability limit the matrix records grey — wiring it would manufacture an 'untranslated' red for
# behavior Helicone never claims. Cohere has no dialect at this commit. features omitted-equivalent
# (=none, unprotected default); telemetry pinned to stdout (its own default → no OTLP egress).
_helicone_write_config() {
  cat > "$GW_DIR/config.gen.yaml" <<YAML
server:
  address: 0.0.0.0
  port: $GW_PORT
helicone:
  features: none
telemetry:
  exporter: stdout
providers:
  openai:
    base-url: "http://127.0.0.1:$MOCK_PORT/"
  anthropic:
    base-url: "http://127.0.0.1:$MOCK_PORT/"
  bedrock:
    base-url: "http://127.0.0.1:$MOCK_PORT/"
routers:
  default:
    load-balance:
      chat:
        strategy: latency
        providers:
          - openai
          - anthropic
          - bedrock
YAML
}

_helicone_spawn() {
  pkill -f 'target/release/ai-gateway' 2>/dev/null; sleep 1
  # AWS creds: helicone's bedrock SigV4 signer reads AWS_ACCESS_KEY / AWS_SECRET_KEY (NOT the SDK's
  # AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY - src/types/provider.rs builds ProviderKey::AwsCredentials
  # from those two names, region from AWS_REGION). With them absent, extract_and_sign_aws_headers
  # returns AuthError::InvalidCredentials -> HTTP 401 before any egress. Both spellings are exported so
  # any code path resolves; the mock ignores the signature. All keys are dummy on the isolated rig.
  setsid taskset -c "$CORES" env \
    OPENAI_API_KEY=sk-dummy ANTHROPIC_API_KEY=sk-dummy GEMINI_API_KEY=sk-dummy \
    AWS_ACCESS_KEY=AKIAMOCKACCESSKEY AWS_SECRET_KEY=mock-secret-access-key \
    AWS_ACCESS_KEY_ID=AKIAMOCKACCESSKEY AWS_SECRET_ACCESS_KEY=mock-secret-access-key AWS_REGION=us-east-1 \
    AI_GATEWAY__SERVER__PORT="$GW_PORT" \
    AI_GATEWAY__SERVER__ADDRESS=0.0.0.0 \
    "$HELICONE_BIN" -c "$GW_DIR/config.gen.yaml" </dev/null >/tmp/helicone.bench.log 2>&1 &
}

gw_launch() {
  _helicone_write_config
  _helicone_spawn
}

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock. Helicone accepts the OpenAI-canonical ingress and translates it to the routed
# provider's NATIVE upstream shape for anthropic (AnthropicConverter -> /v1/messages) and bedrock
# (BedrockConverter -> model/{id}/converse) — both wired to the mock in the single OOTB config above.
# So the capable row is openai-ingress into {openai, anthropic, bedrock}. NOT declared:
#   gemini - despite the endpoint's name, Google's mapping targets Gemini's OPENAI-COMPAT surface,
#     not native generateContent: endpoints/google/generate_contents.rs pins
#     PATH = "v1beta/openai/chat/completions" with OpenAI request/response types. This suite's gemini
#     egress means the native generateContent dialect, so declaring it manufactured an 'untranslated
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
# The single OOTB config already load-balances every reachable egress dialect (all → mock), so each
# matrix column just selects the provider-prefixed model; no per-lane relaunch or config rewrite. The
# gateway is launched once and every column is probed against the same all-providers config.
gw_matrix_egress() {
  case "$1" in
    openai)    GW_MODEL="openai/gpt-4o-mini";;
    anthropic) GW_MODEL="anthropic/claude-3-5-sonnet";;
    bedrock)   GW_MODEL="bedrock/anthropic.claude-3-5-sonnet-v1:0";;
    *) return 1;;
  esac
  _helicone_write_config
  _helicone_spawn
}

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config Helicone launches with. It is file-driven, so the artifact
# is the rendered router config (exactly what -c loads) PLUS the non-secret launch env (all provider /
# AWS keys are dummy on the isolated rig — never a live secret). Read from the file _helicone_write_
# config just rendered (falls back to rendering it if absent), so it can never drift from what the
# gateway loaded. OOTB posture: features=none (unprotected default), telemetry=stdout (no egress),
# openai+anthropic+bedrock all wired to the mock; no feature strip or perf knob.
gw_config() {
  local cfg="$GW_DIR/config.gen.yaml"
  echo "# ── config.gen.yaml (rendered; loaded via -c config.gen.yaml) ──"
  [ -f "$cfg" ] || _helicone_write_config
  cat "$cfg"
  echo
  echo "# ── launch env (non-secret; provider/AWS keys are dummy on the isolated rig) ──"
  cat <<ENV
OPENAI_API_KEY=sk-dummy
ANTHROPIC_API_KEY=sk-dummy
GEMINI_API_KEY=sk-dummy
AWS_ACCESS_KEY=AKIAMOCKACCESSKEY
AWS_SECRET_KEY=mock-secret-access-key
AWS_ACCESS_KEY_ID=AKIAMOCKACCESSKEY
AWS_SECRET_ACCESS_KEY=mock-secret-access-key
AWS_REGION=us-east-1
ENV
}

gw_rss() { awk '/VmRSS/{printf "%.1f", $2/1024}' "/proc/$(pgrep -f 'target/release/ai-gateway' | head -1)/status" 2>/dev/null; }
gw_hwm() { _hwm_tree_mib "$(pgrep -f 'target/release/ai-gateway' 2>/dev/null | head -1)"; }  # kernel VmHWM of the ai-gateway tree

gw_diag() {
  echo "proc: $(pgrep -af 'target/release/ai-gateway' | head -c 200)"
  echo "run.log:"; tail -n 25 /tmp/helicone.bench.log 2>/dev/null
}

gw_stop() { pkill -f 'target/release/ai-gateway' 2>/dev/null; }
