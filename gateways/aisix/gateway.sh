#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: AISIX (api7/aisix, Rust) — BUILT FROM SOURCE, run native.
#
# api7's Rust AI gateway, by the original creators of Apache APISIX. DISTINCT from Apache APISIX (the
# `apisix` entry — their Lua/Nginx gateway): different repo, different language, different binary.
#
# EXCEPTION to the everything-runs-its-official-image rule, kept deliberately (same as Helicone /
# LiteLLM-Rust): the only published image (ghcr.io/api7/aisix, checked 2026-07-24) is linux/amd64-ONLY
# — the docker-image.yml workflow's build-push step has no `platforms:` key, so buildx on the amd64 GH
# runner emits a single-arch amd64 image, and the 0.5.0 image config self-reports
# "architecture":"amd64". The bench boxes are Graviton arm64, so on them we build the `aisix` binary
# from source (rust-toolchain.toml pins rustc 1.93.1; protoc is a build dep for the vertex/bedrock
# tonic crates) and run the release binary natively (real process RSS, no container overhead). Convert
# to the official image if/when api7 ships an arm64 manifest. Refs pinned in gateways/versions.env.
#
# OOTB posture (default features stay ON; only permitted run-mechanics deviate — reconciled from an
# earlier "lean" draft that stripped admin + metrics):
#   - STANDALONE resources_file source (crates/aisix-core/src/config.rs:47 + filesource/mod.rs) — no
#     etcd, no managed control plane: a required run-mechanic (we do not run etcd). The bootstrap
#     config points at one resources.yaml that declares provider_keys + models + api_keys.
#   - cache/guardrails/ratelimit are OPT-IN per policy resource (guardrails need a guardrail_attachment,
#     not even one of the nine file-source kinds; cache needs a cache_policy; ratelimit needs a
#     rate_limit_policy or a per-key rate_limit). They are OFF BY DEFAULT — not stripped — so a stock
#     resources file has none, and the request path is a plain proxy. No feature was disabled to get here.
#   - admin.enabled=true (aisix's DEFAULT, config.rs default_enabled()==true) — RE-ENABLED from the lean
#     draft. The admin listener is a plain in-process HTTP CRUD surface that does NOT require etcd
#     (etcd is a separate optional config-provider crate; standalone runs without it), so admin-off is
#     NOT a forced run-mechanic and would be a forbidden strip. admin_keys is required when admin is on
#     (no serde default); one dummy key satisfies it with no external infra; addr defaults to loopback
#     ephemeral (127.0.0.1:0).
#   - observability.metrics.prometheus.enabled=true (aisix's DEFAULT, PrometheusConfig::default()==true)
#     — RE-ENABLED from the lean draft. It serves a pure in-process /metrics scrape endpoint on its own
#     listener (default 0.0.0.0:9090), no push/no external infra, so metrics-off would be a forbidden
#     strip. tracing.otlp stays off — OTLP is a PUSH exporter to an external collector we do not run
#     (disclosed external-infra run-mechanic). ***FLAGGED FOR REVIEW: the metrics-off decision from the
#     original lean entry is REVERSED here — metrics is default-on and needs no external infra, so under
#     OOTB it must stay on (binds a second port :9090). See gw_config note + the reconciliation summary.***
#   - ONE provider_key whose api_base is the mock, with a dummy api_key. openai adapter builds
#     {api_base}/chat/completions (aisix-provider-openai/src/bridge.rs:400) — api_base ends in /v1 so
#     the mock's /v1/chat/completions is hit; arbitrary http:// localhost is accepted (not
#     googleapis-hardcoded). anthropic adapter builds {api_base}/v1/messages
#     (aisix-provider-anthropic/src/bridge.rs:273) with x-api-key: <dummy>.
#
# AUTH IS MANDATORY (source: crates/aisix-proxy/src/auth.rs) — aisix has NO anonymous/no-auth mode.
# Every proxy handler takes the AuthenticatedKey extractor, which 401s an unauthenticated/unknown-key
# request BEFORE the handler runs (extract_bearer requires Authorization: Bearer <k> or x-api-key:
# <k>; the key is SHA-256-hashed and looked up in the snapshot; miss → 401, error.rs:246). So we
# register ONE api_key (its plaintext supplied via key_env, hashed at load) and the bench sends it as
# the bearer — identical static-token posture to busbar. The per-request cost is a single SHA-256 +
# O(1) map lookup on the hot path; no rate-limit/budget/governance runs. See the report note: this is
# a keyed proxy, not a rate-limited one.
GW_KIND=native
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="AISIX (api7)"                 # label in charts + report tables — disambiguates from Apache APISIX
GW_LANG=Rust                             # implementation language → bar color bucket
GW_CLASS="AI gateway"   # the project's OWN self-description (README: 'the open-source, Rust-native AI gateway for LLMs and AI agents'), not our editorial
GW_REPO=https://github.com/api7/aisix    # linked from the gateway name in the report table
GW_PORT=3000
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=bench-token
# Translation lane (xlate suite): aisix serves Anthropic ingress natively on /v1/messages
# (crates/aisix-proxy/src/lib.rs build_router) and translates to the OpenAI upstream configured below.
# extract_bearer accepts the SAME token via either Authorization: Bearer or x-api-key, so no auth
# override is needed for the anthropic-carrier probe.
GW_ANTHROPIC_PATH=/v1/messages

AISIX_SRC="${AISIX_SRC:-$HOME/aisix-src}"
AISIX_BIN=""

# This gateway builds from source, so ITS box (and only its box) installs the Rust toolchain + git +
# protoc + build deps. The base image ships bare OS + docker only (see run-on-ec2.sh); docker gateways
# never pay for this. protoc (protobuf-compiler) is required by the vertex/bedrock tonic/prost deps —
# the aisix Dockerfile installs it for the same reason. rustup honors rust-toolchain.toml (1.93.1).
# Idempotent — a no-op once the toolchain + protoc are present.
gw_prereqs() {
  command -v cargo >/dev/null && command -v git >/dev/null && command -v protoc >/dev/null && return 0
  sudo apt-get install -y -q git build-essential pkg-config libssl-dev protobuf-compiler >/dev/null 2>&1 || true
  command -v cargo >/dev/null || (curl -sSf https://sh.rustup.rs | sh -s -- -y >/dev/null 2>&1)
  . "$HOME/.cargo/env" 2>/dev/null || true
  command -v cargo >/dev/null || { echo "aisix: rust toolchain unavailable"; return 1; }
  command -v protoc >/dev/null || { echo "aisix: protoc (protobuf-compiler) unavailable — required by tonic/prost build deps"; return 1; }
}

gw_build() {
  gw_prereqs || return 1
  if [ ! -d "$AISIX_SRC" ]; then
    git clone "${AISIX_REPO:-https://github.com/api7/aisix}" "$AISIX_SRC" || return 1
    [ -n "${AISIX_COMMIT:-}" ] && git -C "$AISIX_SRC" checkout -q "$AISIX_COMMIT"
  fi
  # Release build (LTO etc.) — fine on the 8-core bench box. Only the `aisix` binary is needed.
  ( cd "$AISIX_SRC" && cargo build --release --bin aisix ) || return 1
  AISIX_BIN="$AISIX_SRC/target/release/aisix"
  [ -x "$AISIX_BIN" ] || { echo "aisix binary not found after build"; return 1; }
}

gw_version() {
  local sha; sha="$(git -C "$AISIX_SRC" rev-parse --short HEAD 2>/dev/null)"
  local v; v="$("$AISIX_BIN" --version 2>/dev/null | awk '{print $2}')"
  echo "api7/aisix@${sha:-?}${v:+ (v$v)} (source build)"
}

# _aisix_launch <adapter> <api_base>: write the standalone bootstrap config + resources file wiring
# exactly ONE provider_key (adapter + api_base → mock) and boot the binary. The client-facing model
# id is always GW_MODEL (display_name); model_name is the upstream id sent on. api_base is the mock
# host — openai appends /chat/completions to {base}/v1, anthropic appends /v1/messages to {base}.
# _aisix_write_config <adapter> <api_base>: RENDER the bootstrap + resources files ONLY (no boot, no
# pkill) — so gw_config can materialize the exact OOTB config without a side effect. _aisix_launch
# calls this then boots the binary.
_aisix_write_config() {
  local adapter="$1" api_base="$2"
  # OOTB reconciliation (see the "OOTB posture" note above gw_config for the full rationale + citations):
  #  - admin.enabled: true — aisix's DEFAULT (config.rs default_enabled()==true); the admin listener is
  #    a plain in-process HTTP CRUD surface that does NOT require etcd (etcd is a separate optional
  #    config-provider crate; standalone mode runs without it), so admin-off would be a forbidden strip.
  #    admin_keys is a REQUIRED field when admin is enabled (no serde default) — one dummy key satisfies
  #    it with no external infra. addr defaults to loopback 127.0.0.1:0 (ephemeral), so no fixed extra port.
  #  - metrics.prometheus.enabled: true — aisix's DEFAULT (PrometheusConfig::default()==true); serving
  #    /metrics is a pure in-process scrape endpoint on its own listener (default 0.0.0.0:9090), no push,
  #    no external infra — so metrics-off would be a forbidden strip. Left ON, as it ships.
  #  - tracing.otlp.enabled: false — OTLP is a PUSH exporter to an external collector we do not run; this
  #    is a disclosed run-mechanic (external-infra dependency), the only observability line held off.
  cat > "$GW_DIR/config.gen.yaml" <<YAML
resources_file: "$GW_DIR/resources.gen.yaml"
proxy:
  addr: "0.0.0.0:$GW_PORT"
admin:
  enabled: true
  admin_keys:
    - "aisix-admin-dummy"
observability:
  service_name: "aisix-bench"
  metrics:
    prometheus:
      enabled: true
  tracing:
    otlp:
      enabled: false
YAML
  cat > "$GW_DIR/resources.gen.yaml" <<YAML
_format_version: "1"
provider_keys:
  - display_name: mock
    provider: $adapter
    adapter: $adapter
    api_base: "$api_base"
    api_key: "sk-mock"
models:
  - display_name: $GW_MODEL
    provider: $adapter
    model_name: gpt-4o-mini
    provider_key: mock
api_keys:
  - display_name: bench
    key_env: AISIX_BENCH_KEY
    allowed_models: ["*"]
YAML
}

# _aisix_launch <adapter> <api_base>: render the config (above) then boot the binary.
_aisix_launch() {
  _aisix_write_config "$1" "$2"
  pkill -f 'target/release/aisix' 2>/dev/null; sleep 1
  # AISIX_BENCH_KEY holds the plaintext bearer; aisix hashes it (SHA-256) into key_hash at load. The
  # bench sends this same value as Authorization: Bearer / x-api-key (GW_AUTH). AISIX_* env vars are
  # ALSO scraped by aisix's config loader as field overrides, but AISIX_BENCH_KEY is not a config
  # field path (no __), so it is ignored by the deserializer and only consumed by the api_key key_env.
  setsid taskset -c "$CORES" env \
    AISIX_BENCH_KEY="$GW_AUTH" \
    "$AISIX_BIN" --config "$GW_DIR/config.gen.yaml" </dev/null >"$GW_DIR/launch.log" 2>&1 &
}

gw_launch() {
  # openai egress: api_base ends in /v1 so the bridge's {base}/chat/completions hits the mock's
  # /v1/chat/completions (aisix-provider-openai/src/bridge.rs:400).
  _aisix_launch openai "http://127.0.0.1:$MOCK_PORT/v1"
}

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config this gateway launches with. AISIX is file-driven (a
# bootstrap config.gen.yaml that references a resources.gen.yaml), so the artifact is BOTH RENDERED
# files (exactly what --config loads) plus the one non-secret key env. The suite runner captures this
# once per run into results/config/aisix.txt and the board publishes it, so "fresh source build +
# these files → these numbers" is reproducible. The files are read from what gw_launch just rendered
# (falls back to rendering the openai-lane default), so they can never drift from what aisix loaded.
# OOTB posture (reconciled from a lean draft): admin.enabled=true and metrics.prometheus.enabled=true
# are aisix's DEFAULTS and need no external infra, so both are ON (re-enabled — off would be forbidden
# strips). Held off as disclosed run-mechanics: standalone resources_file (no etcd) and tracing.otlp
# (external push collector). Auth is mandatory (aisix has no anonymous mode) — one api_key, its
# plaintext via AISIX_BENCH_KEY, shown as its dummy value; provider api_key + admin_keys are dummy on
# the isolated rig (no live secrets). ***The metrics-off REVERSAL is flagged for review — see header.***
gw_config() {
  local cfg="$GW_DIR/config.gen.yaml" res="$GW_DIR/resources.gen.yaml"
  [ -f "$cfg" ] || _aisix_write_config openai "http://127.0.0.1:$MOCK_PORT/v1"
  echo "# ── config.gen.yaml (rendered; loaded via --config) ──"
  cat "$cfg"
  echo
  echo "# ── resources.gen.yaml (rendered; standalone resources_file) ──"
  cat "$res"
  echo
  echo "# ── launch env (non-secret; AISIX_BENCH_KEY is the mandatory api_key plaintext, dummy on the rig) ──"
  cat <<ENV
AISIX_BENCH_KEY=$GW_AUTH
ENV
}

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock.
#
# INGRESS (rows): aisix registers exactly three of the six wire dialects as ingress routes
# (crates/aisix-proxy/src/lib.rs build_router / dispatch.rs ROUTE_PATHS): /v1/chat/completions
# (openai), /v1/responses (openai-responses), /v1/messages (anthropic). There is NO gemini
# (:generateContent), cohere (/v2/chat), or bedrock (/model/../converse) INGRESS route — those exist
# only as EGRESS adapters — so rows 4/5/6 are entirely undeclared (an honest ingress subset, not a
# pretend-all).
#
# EGRESS (cols): the two adapters that are plain HTTP with a single dummy key are wired to the mock:
#   openai    — provider_key.api_base fully overridable; {base}/v1/chat/completions.
#   anthropic — provider_key.api_base overridable; {base}/v1/messages; x-api-key: <dummy>.
# NOT wired: openai-responses egress (no separate egress adapter — /v1/responses is an ingress-side
# API bridged onto the openai upstream, so it is not a distinct upstream dialect column here), gemini
# (vertex adapter targets *aiplatform.googleapis.com and needs an OAuth2 token — no dummy-key path),
# cohere (no cohere adapter exists: the Adapter enum is openai|anthropic|bedrock|vertex|azure-openai,
# provider_key.schema.json), and bedrock (the bedrock adapter's api_key must be a JSON-encoded AWS
# creds blob {access_key_id,secret_access_key,region} and every request is SigV4-signed — not a simple
# dummy key; declining to declare it keeps this entry conservative). Under probe-first the field run
# still ATTEMPTS all 36 cells against the default config and records each honestly; this GW_MATRIX_CAP
# is advisory citation only.
#
# So the capable rows are {openai, openai-responses, anthropic}-INGRESS into the {openai, anthropic}
# egress cols. anthropic-ingress→openai-egress (and the reverse) are the cross-dialect translation
# claims; the same-dialect cells are faithful passthrough.
# Evidence: lib.rs build_router (ingress routes), dispatch.rs ROUTE_PATHS, provider_key.schema.json
# (Adapter enum), aisix-provider-{openai,anthropic}/src/bridge.rs (api_base override + URL build),
# aisix-provider-bedrock/src/bridge.rs:12-17 (SigV4 + JSON creds), tag v0.5.0. Wired-pending-field-verify.
GW_MATRIX_CAP="
101000
101000
101000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="AISIX v0.5.0 registers only openai (/v1/chat/completions), openai-responses (/v1/responses) and anthropic (/v1/messages) INGRESS routes — no gemini/cohere/bedrock ingress (lib.rs build_router) — and only its openai + anthropic EGRESS adapters take a plain overridable api_base with a dummy key; gemini (vertex, OAuth2), cohere (no adapter) and bedrock (SigV4 + JSON AWS creds) are grey by that capability/auth limit"
GW_MATRIX_EGRESS="openai anthropic"

gw_matrix_egress() {
  case "$1" in
    openai)    _aisix_launch openai    "http://127.0.0.1:$MOCK_PORT/v1";;
    anthropic) _aisix_launch anthropic "http://127.0.0.1:$MOCK_PORT";;
    *) return 1;;
  esac
}

gw_rss() { awk '/VmRSS/{printf "%.1f", $2/1024}' "/proc/$(pgrep -f 'target/release/aisix' | head -1)/status" 2>/dev/null; }
gw_hwm() { _hwm_tree_mib "$(pgrep -f 'target/release/aisix' 2>/dev/null | head -1)"; }  # kernel VmHWM of the aisix tree

gw_diag() {
  echo "proc: $(pgrep -af 'target/release/aisix' | head -c 200)"
  echo "launch.log:"; tail -n 25 "$GW_DIR/launch.log" 2>/dev/null
}

gw_stop() { pkill -f 'target/release/aisix' 2>/dev/null; }

# gw_matrix_egress + the declared capability matrix are defined above (before gw_launch). The
# anthropic egress column + the openai-responses/anthropic ingress rows are wired-pending-field-
# verification; the EC2 field run turns each declared-1 cell green or red. Every grey cell is a cited
# capability limit (no ingress route, or an egress adapter with no dummy-key path).
