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
# arm64. The source refs are pinned in THIS file (below), where every other fact about this
# gateway already lives.
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
# One router per wired provider (see _helicone_write_config): the router is chosen by URL path, so the
# egress upstream is DETERMINISTIC. GW_PATH is the openai-provider router — the canonical lane every
# non-matrix suite (latency / throughput / streaming / memory) is measured on.
GW_PATH=/router/openai/chat/completions
# Helicone's router requires the unified "{provider}/{model}" form (it errors on a bare model name);
# the mock ignores the model and answers the OpenAI shape regardless.
GW_MODEL=openai/gpt-4o-mini
GW_AUTH=dummy

# ── SOURCE PIN ────────────────────────────────────────────────────────────────────────────────────
# EXACTLY what this gateway is built from. It lives HERE, in the gateway's own directory, so that
# adding or re-pinning a gateway touches nothing else in the tree. Override either var in the
# environment to build a different ref; the runner records the commit ACTUALLY built into the run
# JSON (`build` field), so the output always discloses what was measured. Empty commit = branch HEAD.
HELICONE_REPO="${HELICONE_REPO:-https://github.com/Helicone/ai-gateway}"
HELICONE_COMMIT="${HELICONE_COMMIT:-9649b27bdc9fb0907d359e899894102a15f3a085}"
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
# whose base-url the mock can stand in for is wired here (all → mock), one NAMED ROUTER per provider.
# Helicone appends each provider's native path to base-url, so pointing every provider at the mock host
# makes each POST that dialect's native upstream shape. Providers wired: openai, anthropic, bedrock —
# Helicone's first-class translating dialects reachable via a base-url override (types/provider.rs
# InferenceProvider). Gemini is deliberately NOT wired: Helicone's Gemini egress targets Google's
# OpenAI-COMPAT surface (v1beta/openai/chat/completions), not native generateContent, so it is a
# capability limit the matrix records grey — wiring it would manufacture an 'untranslated' red for
# behavior Helicone never claims. Cohere has no dialect at this commit. features omitted-equivalent
# (=none, unprotected default); telemetry pinned to stdout (its own default → no OTLP egress).
#
# WHY ONE ROUTER PER PROVIDER, NOT ONE FAN-OUT ROUTER (the 2026-07-25 field-run regression):
# this config used to declare a single `default` router load-balancing chat across
# [openai, anthropic, bedrock] with `strategy: latency`. That strategy is
# BalanceConfigInner::BalancedLatency, whose own rustdoc says "there is an element of randomness in
# the selection of the provider" (ai-gateway/src/config/balance.rs @9649b27) — the provider is chosen
# by the latency balancer, NOT by the provider prefix on the request's model. So the matrix could
# never pin an egress column: results/fanout-helicone.log shows the SAME openai-ingress probe landing
# on the anthropic upstream in the openai/openai-responses/gemini columns and on the openai upstream
# in the anthropic/cohere/bedrock columns — 0/36 cells, and, worse, a benchmark nobody could
# reproduce, because which upstream serves a request was a coin flip. Helicone's own shipped
# config/local.yaml declares several named routers side by side, so this is the project's documented
# pattern, not a bench-only contortion: one config, three routers, selected by URL path
# (/router/{id}/chat/completions, router/router_details.rs). NOTHING is rewritten between columns —
# the file below is byte-identical for every lane and is what gw_config publishes.
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
  openai:
    load-balance:
      chat:
        strategy: latency
        providers:
          - openai
  anthropic:
    load-balance:
      chat:
        strategy: latency
        providers:
          - anthropic
  bedrock:
    load-balance:
      chat:
        strategy: latency
        providers:
          - bedrock
YAML
}

_helicone_spawn() {
  pkill -f 'target/release/ai-gateway' 2>/dev/null; sleep 1
  # AWS creds: helicone's bedrock SigV4 signer reads AWS_ACCESS_KEY / AWS_SECRET_KEY (NOT the SDK's
  # AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY - src/types/provider.rs builds ProviderKey::AwsCredentials
  # from those two names). With them absent, extract_and_sign_aws_headers returns
  # AuthError::InvalidCredentials -> HTTP 401 before any egress. Both spellings are exported so any code
  # path resolves; the mock ignores the signature. All keys are dummy on the isolated rig.
  #
  # AWS_REGION IS DELIBERATELY *NOT* EXPORTED, and that is load-bearing. Config::try_read
  # (config/mod.rs:164-173 @9649b27) does this AFTER the config file is parsed:
  #     if let Ok(bedrock_region) = std::env::var("AWS_REGION") { ...
  #         bedrock_provider.base_url = "https://bedrock-runtime.{region}.amazonaws.com" }
  # i.e. AWS_REGION SILENTLY OVERWRITES providers.bedrock.base-url from the config file. We used to
  # export AWS_REGION=us-east-1, so every bedrock-routed request left the box for the real AWS endpoint
  # instead of the mock — reproduced locally 2026-07-25 against a source build at this exact commit:
  #     target_url: https://bedrock-runtime.us-east-1.amazonaws.com/model/...:0/converse -> 403
  #     ERROR Could not deserialize ...::bedrock::converse::ConverseError -> HTTP 500 to the client
  # and the mock recorded ZERO requests. On the network-isolated rig that same egress is simply
  # unreachable, which is where results/fanout-helicone.log's recurring
  # "transient status 500 (upstream unreachable)" came from — whenever the (then latency-balanced)
  # router happened to pick bedrock. Dropping AWS_REGION lets the config file's base-url stand and the
  # bedrock lane reaches the mock: verified locally, POST /model/anthropic.claude-3-5-sonnet-v1:0/
  # converse, valid Converse body, HTTP 200 translated back to an OpenAI envelope, 3/3 runs.
  # AWS_REGION is not otherwise consulted: the SigV4 signer derives its signing region from the request
  # HOST (dispatcher/bedrock_client.rs), not from the environment, so nothing else regresses.
  setsid taskset -c "$CORES" env \
    OPENAI_API_KEY=sk-dummy ANTHROPIC_API_KEY=sk-dummy GEMINI_API_KEY=sk-dummy \
    AWS_ACCESS_KEY=AKIAMOCKACCESSKEY AWS_SECRET_KEY=mock-secret-access-key \
    AWS_ACCESS_KEY_ID=AKIAMOCKACCESSKEY AWS_SECRET_ACCESS_KEY=mock-secret-access-key \
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
# (BedrockConverter -> model/{id}/converse) — both wired to the mock in the single OOTB config above,
# each behind its own named router so the column's egress provider is deterministic rather than
# latency-balanced. So the capable row is openai-ingress into {openai, anthropic, bedrock}.
# The other five INGRESS rows are all-zero for one reason, not five: Helicone mounts no native
# ingress for them at all. /router/{id}/... is the only translating surface and ApiEndpoint::new
# accepts only chat/completions under it, so /v1/responses, /v1/messages, /v1beta/models/...,
# /v2/chat and /model/.../converse fall through to the DIRECT-proxy pattern /{provider}/... and are
# rejected by first path segment — exactly what the field run recorded ("Unsupported provider: v1",
# "…: v1beta", "…: v2", "…: model"; router/router_details.rs + router/meta.rs). NOT declared:
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
# The single OOTB config already wires every reachable egress dialect (all → mock) under one named
# router each, so a matrix column selects its egress by REQUEST ADDRESSING only — the router in the
# URL path plus the provider-prefixed model in the body. NO config rewrite: _helicone_write_config
# takes no arguments and emits the identical file for every column (it is re-rendered only because the
# harness stops the process between columns and the launch path owns the render).
#
# Both knobs are set on EVERY call, including the reject path, so a column can never inherit the
# previous column's addressing (the compounding-state bug class fixed in 471193b). GW_MATRIX_PATH_
# OPENAI — not GW_PATH — carries the per-column router, because lib/ingress.sh honours the
# GW_MATRIX_PATH_* override at CALL time while GW_PATH stays the manifest's published canonical lane
# (matrix/run.sh publishes it as upstream_endpoint, and the latency/throughput/stream suites use it).
#
# The model must be one the TARGET provider actually offers: MapperService::map_model
# (middleware/mapper/model.rs) passes the source model straight through when the target provider lists
# it (config/embedded/providers.yaml), and otherwise falls back to the default model-mapping table.
# Each model below is in its provider's offered list at 9649b27, so each column asks the mock for a
# model that provider genuinely serves.
gw_matrix_egress() {
  GW_MATRIX_PATH_OPENAI="$GW_PATH"
  case "$1" in
    openai)    GW_MODEL="openai/gpt-4o-mini";              GW_MATRIX_PATH_OPENAI=/router/openai/chat/completions;;
    anthropic) GW_MODEL="anthropic/claude-3-5-sonnet";     GW_MATRIX_PATH_OPENAI=/router/anthropic/chat/completions;;
    bedrock)   GW_MODEL="bedrock/anthropic.claude-3-5-sonnet-v1:0"; GW_MATRIX_PATH_OPENAI=/router/bedrock/chat/completions;;
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
# openai+anthropic+bedrock all wired to the mock behind one named router each; no feature strip or
# perf knob. The file is identical for every lane and every egress column — nothing here is rewritten
# per column, so this artifact IS the config every published number was produced on.
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
# AWS_REGION is deliberately UNSET (see _helicone_spawn): exporting it makes Config::try_read
# overwrite providers.bedrock.base-url with the real AWS endpoint, silently un-wiring the mock.
ENV
}

# Memory: BOTH readers cover the SAME process set (the target/release/ai-gateway process tree), via the shared
# lib/harness.sh pair — the identical method + units the docker manifests get from
# container_rss_mib/container_hwm_mib. They used to disagree: gw_rss read a SINGLE pid's
# /proc/<pid>/status while gw_hwm walked the whole tree, so idle/peak/recovered and
# peak_rss_hwm_mib described different populations of the same gateway.
gw_rss() { native_rss_mib 'target/release/ai-gateway'; }  # summed process-tree VmRSS
gw_hwm() { native_hwm_mib 'target/release/ai-gateway'; }  # summed process-tree VmHWM (kernel high-water mark)

gw_diag() {
  echo "proc: $(pgrep -af 'target/release/ai-gateway' | head -c 200)"
  echo "run.log:"; tail -n 25 /tmp/helicone.bench.log 2>/dev/null
}

gw_stop() { pkill -f 'target/release/ai-gateway' 2>/dev/null; }
