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
# to the official image if/when api7 ships an arm64 manifest. The source refs are pinned below, in
# this file.
#
# OOTB posture. The bootstrap config is three settings and nothing else (see _aisix_write_config for
# the local evidence behind each):
#   - STANDALONE resources_file source (crates/aisix-core/src/config.rs:47 + filesource/mod.rs) — no
#     etcd, no managed control plane, because we do not run etcd. The bootstrap config points at one
#     resources.yaml that declares provider_keys + models + api_keys.
#   - proxy.addr, the port the harness drives.
#   - admin.admin_keys, one dummy key. Not a choice: with no admin block the binary refuses to load
#     its config ("admin.admin_keys must contain at least one key"). Admin is on by default and that
#     field has no serde default, so this is the cost of booting.
#   - NOTHING about observability. The block is gone and metrics are still served on :9090, because
#     that is aisix's default. We neither enabled nor disabled them.
#   - cache/guardrails/ratelimit are OPT-IN per policy resource (guardrails need a guardrail_attachment,
#     not even one of the nine file-source kinds; cache needs a cache_policy; ratelimit needs a
#     rate_limit_policy or a per-key rate_limit). They are OFF BY DEFAULT — not stripped — so a stock
#     resources file has none, and the request path is a plain proxy. No feature was disabled to get here.
#   - TWO provider_keys, both with api_base -> the mock and a dummy api_key, declared together in the
#     ONE static resources file (plus the two client-facing models that select them). This is the house
#     one-static-config standard: the matrix picks an egress column by swapping the request's model
#     name, never by re-rendering the config. openai adapter builds
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
# the bearer - the same static-token posture other keyed gateways here use. The per-request cost is a single SHA-256 +
# O(1) map lookup on the hot path; no rate-limit/budget/governance runs. See the report note: this is
# a keyed proxy, not a rate-limited one.
GW_KIND=native
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="AISIX (api7)"                 # label in charts + report tables — disambiguates from Apache APISIX
GW_LANG=Rust                             # implementation language → bar color bucket
GW_CLASS="AI gateway"   # the project's OWN self-description (README: 'the open-source, Rust-native AI gateway for LLMs and AI agents'), not our editorial
GW_REPO=https://github.com/api7/aisix    # linked from the gateway name in the report table
# ── SOURCE PIN ────────────────────────────────────────────────────────────────────────────────────
# EXACTLY what this gateway is built from, pinned HERE in its own directory so adding or re-pinning
# a gateway touches nothing else in the tree. Override in the environment to build a different ref;
# the runner records the commit ACTUALLY built into the run JSON (`build` field). Pinned to the
# released tag v0.5.0; an empty commit floats to branch HEAD.
AISIX_REPO="${AISIX_REPO:-https://github.com/api7/aisix}"
AISIX_COMMIT="${AISIX_COMMIT:-0f90a98ec8c43864d43e740e3ab66fe1d639c143}"   # tag v0.5.0
GW_PORT=3000
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=bench-token

# ── CONFIG NECESSITY (lib/gateway_config_lint.sh) ─────────────────────────────────────────────────
# Every setting this manifest writes, and the ONE reason it is here. The lint fails on a setting with
# no claim AND on a claim with no setting, so this block cannot drift from the config in either
# direction. Reasons: boot (it will not run without this) | upstream (points an upstream at the test
# mock) | ingress (an ingress path the 6x6 matrix drives) | bind (the port or CPU pin the rig needs).
GW_CONFIG_WHY="
resources_file boot   # standalone source; without it aisix demands an etcd cluster
proxy          bind
addr           bind   # the port the harness drives
admin          boot   # with no admin block the binary refuses to load its config:
admin_keys     boot   #   \"admin.admin_keys must contain at least one key\" (tested, see below)
_format_version boot  # the resources file will not load without it
provider_keys  boot
display_name   boot   # a resource without a name cannot be referenced
provider       upstream  # which upstream dialect this key speaks
adapter        upstream  # and which adapter builds its URL
api_base       upstream  # -> the mock
api_key        boot      # dummy; the mock ignores it
models         ingress   # the client-facing model table
model_name     upstream  # the model id sent upstream
provider_key   upstream  # which provider_key (and so which egress) this model resolves to
api_keys       boot      # aisix has NO anonymous mode: every proxy handler 401s without a known key
key_env        boot      # where the plaintext comes from
allowed_models boot      # an api_key with no allowance can reach nothing
BENCH_AISIX_KEY boot     # that plaintext. The name must NOT start with AISIX_ - see _aisix_launch
"
# The two CLIENT-facing model ids declared in the ONE static resources file (see _aisix_write_config).
# gw_matrix_egress selects an egress column by swapping GW_MODEL between these — it never rewrites the
# config. NOTE THE `GW_` PREFIX: any shell var named `AISIX_*` that reaches the binary's environment is
# swallowed by aisix's config scrape and kills boot (see the launch-env note in _aisix_launch), so
# nothing rig-side may use that prefix.
GW_MODEL_OPENAI=gpt-4o-mini
GW_MODEL_ANTHROPIC=claude-3-5-sonnet-20241022
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
    git clone "$AISIX_REPO" "$AISIX_SRC" || return 1
  fi
  # Re-pin on EVERY build, not just on a fresh clone. This checkout used to live inside the clone
  # branch above, so a REUSED box (a re-run, or a box whose $AISIX_SRC survived) kept whatever ref the
  # first build left behind and silently ignored a changed AISIX_COMMIT — the run would then record a
  # `build` string that did not match the pin. Fresh EC2 boxes never hit it; reused ones drift.
  [ -n "${AISIX_COMMIT:-}" ] && { git -C "$AISIX_SRC" fetch -q origin 2>/dev/null; git -C "$AISIX_SRC" checkout -q "$AISIX_COMMIT" || return 1; }
  # Release build (LTO etc.) — fine on the 8-core bench box. Only the `aisix` binary is needed.
  ( cd "$AISIX_SRC" && cargo build --release --bin aisix ) || return 1
  AISIX_BIN="$AISIX_SRC/target/release/aisix"
  [ -x "$AISIX_BIN" ] || { echo "aisix binary not found after build"; return 1; }
}

# NOT A PIN BUG — the "(v0.3.0)" this prints is upstream's own stale self-report. AISIX_COMMIT
# 0f90a98ec8c43864d43e740e3ab66fe1d639c143 IS tag v0.5.0; api7 did not bump the workspace Cargo.toml
# `version` at that tag, so `aisix --version` and every crate line in the build log still say 0.3.0.
# The COMMIT is what pins the build and what this string leads with — the version suffix is cosmetic.
# Don't "fix" the pin on the strength of the v0.3.0 text.
gw_version() {
  local sha; sha="$(git -C "$AISIX_SRC" rev-parse --short HEAD 2>/dev/null)"
  local v; v="$("$AISIX_BIN" --version 2>/dev/null | awk '{print $2}')"
  echo "api7/aisix@${sha:-?}${v:+ (v$v)} (source build)"
}

# ── ONE STATIC CONFIG (house standard) ────────────────────────────────────────────────────────────
# _aisix_write_config takes NO arguments and renders the SAME BYTES on every call, for every lane and
# every matrix egress column. That is the standard the board depends on: a reader installs this gateway
# version with THIS ONE published config on their own box and sees these numbers. A per-column config
# rewrite would mean the published row was assembled from several configs, only one of which is
# published — so it is banned here.
#
# It previously took <adapter> <api_base> and gw_matrix_egress re-rendered a DIFFERENT single-provider
# resources file per column (openai -> api_base .../v1, anthropic -> api_base ...). Replaced by the
# one-config pattern most of the field already uses: ONE resources file declaring BOTH upstream provider_keys and
# BOTH client-facing models at once; gw_matrix_egress changes only GW_MODEL, the CLIENT-side selector.
# aisix routes a request by its model display_name -> models[].provider_key -> that provider_key's
# adapter + api_base, so which upstream dialect is exercised is chosen by the model name in the request
# body, with no reconfiguration. Multiple provider_keys/models in one resources_file is aisix's own
# documented standalone shape (crates/aisix-core filesource), not a rig trick.
#
# LOCALLY PROVEN (aisix built from source at the pinned AISIX_COMMIT 0f90a98, run against the pinned
# mock bin/mock-arm64 with MOCK_RECORD=1). ONE config, rendered by THIS function, no reconfiguration
# between probes — "resources loaded from file resources=5", proxy bound, and all six declared cells
# 200 with the mock recording the right upstream dialect and body_ok=true:
#   openai    <- openai            /v1/chat/completions   openai
#   anthropic <- openai            /v1/messages           anthropic
#   openai    <- anthropic         /v1/chat/completions   openai
#   anthropic <- anthropic         /v1/messages           anthropic
#   openai    <- openai-responses  /v1/responses          openai-responses
#   anthropic <- openai-responses  /v1/messages           anthropic
# (The openai-responses ingress row egressed on /v1/responses for the openai column — the field's
# probe-first leg-3 evidence records where each request actually landed; nothing is pre-judged here.)
# The same binary + config REPRODUCED the field failure byte-for-byte when the env scrub was removed:
#   AISIX_BENCH_KEY=... -> unknown field `bench_key`   (the exact 2026-07-25 error)
#   AISIX_COMMIT=...    -> unknown field `commit`      (the latent override-path wipeout)
#
# Per-adapter api_base shapes (unchanged, just now coexisting):
#   openai    — bridge builds {api_base}/chat/completions (aisix-provider-openai/src/bridge.rs:400), so
#               api_base ends in /v1 and the mock's /v1/chat/completions is hit.
#   anthropic — bridge builds {api_base}/v1/messages (aisix-provider-anthropic/src/bridge.rs:273) with
#               x-api-key: <dummy>, so api_base is the bare mock origin.
_aisix_write_config() {
  # THE WHOLE BOOTSTRAP CONFIG IS THREE SETTINGS. Everything the earlier draft argued about - admin
  # on/off, metrics on/off, otlp off, a service_name - turned out to be settings we did not need to
  # write at all, which is a better answer than either side of that argument. Tested locally against
  # the binary built at the pinned AISIX_COMMIT:
  #   * admin.enabled REMOVED: it restated aisix's own default (config.rs default_enabled()==true).
  #   * admin_keys KEPT, and it is genuinely load-bearing. With the admin block absent entirely the
  #     binary refuses to start:
  #       Error: config load failed: admin.admin_keys must contain at least one key
  #              (required when managed.enabled is false)
  #     Admin is on by default and admin_keys has no serde default, so ONE dummy key is the price of
  #     booting at all. It needs no external infra and addr defaults to loopback 127.0.0.1:0.
  #   * the entire observability block REMOVED. Deleting it does NOT turn metrics off: with no
  #     observability config at all the binary still logs `aisix listening (http) addr=0.0.0.0:9090
  #     label="metrics"` and /metrics answers 200. So `prometheus.enabled: true` was restating a
  #     default, `tracing.otlp.enabled: false` was us turning a feature off, and `service_name` was a
  #     label for our own convenience. Metrics stay on because that is what aisix ships, not because
  #     we asked for them - which is the whole point.
  cat > "$GW_DIR/config.gen.yaml" <<YAML
resources_file: "$GW_DIR/resources.gen.yaml"
proxy:
  addr: "0.0.0.0:$GW_PORT"
admin:
  admin_keys:
    - "aisix-admin-dummy"
YAML
  cat > "$GW_DIR/resources.gen.yaml" <<YAML
_format_version: "1"
provider_keys:
  - display_name: mock-openai
    provider: openai
    adapter: openai
    api_base: "http://127.0.0.1:$MOCK_PORT/v1"
    api_key: "sk-mock"
  - display_name: mock-anthropic
    provider: anthropic
    adapter: anthropic
    api_base: "http://127.0.0.1:$MOCK_PORT"
    api_key: "sk-mock"
models:
  - display_name: $GW_MODEL_OPENAI
    provider: openai
    model_name: gpt-4o-mini
    provider_key: mock-openai
  - display_name: $GW_MODEL_ANTHROPIC
    provider: anthropic
    model_name: claude-3-5-sonnet-20241022
    provider_key: mock-anthropic
api_keys:
  - display_name: bench
    key_env: BENCH_AISIX_KEY
    allowed_models: ["*"]
YAML
}

# _aisix_launch: render THE one config (above) then boot the binary. Takes no arguments — every caller
# gets identical bytes; the egress column is selected client-side by GW_MODEL, never by a rewrite.
_aisix_launch() {
  _aisix_write_config
  pkill -f 'target/release/aisix' 2>/dev/null; sleep 1
  # BENCH_AISIX_KEY holds the plaintext bearer; aisix hashes it (SHA-256) into key_hash at load. The
  # bench sends this same value as Authorization: Bearer / x-api-key (GW_AUTH).
  #
  # ***THE NAME MUST NOT START WITH `AISIX_`.*** This var was previously AISIX_BENCH_KEY, on the
  # (WRONG) assumption that aisix's env-override scrape only claims names containing `__` and would
  # ignore it. It does not: the loader strips the `AISIX_` prefix, lowercases the remainder and feeds
  # it to the bootstrap-config deserializer as a TOP-LEVEL field, which is deny-unknown-fields. Every
  # boot therefore died at config load, before binding :3000 — while the launch hook still returned 0,
  # because the binary is backgrounded and the failure is a later non-zero exit. That is exactly the
  # 2026-07-25 field signature: `launch_rc=0, not ready; port 3000 not listening` on all 3 attempts of
  # all 6 egress columns, with the real cause only in gw_diag's launch.log tail
  # (results/fanout-aisix.log:1467):
  #   Error: config load failed: failed to load configuration: deserialize: unknown field `bench_key`,
  #   expected one of `etcd`, `resources_file`, `proxy`, `admin`, `observability`, `cache`,
  #   `ratelimit`, `upstream`, `managed`, `bedrock_endpoint_url`
  # — 36/36 cells lost to a variable NAME. `BENCH_AISIX_KEY` carries no `AISIX_` prefix, so the scrape
  # never sees it and only the api_key's key_env consumes it. Keep it that way.
  #
  # THE SAME TRAP IS WIDER THAN ONE VARIABLE, so the launch env SCRUBS the rig's own AISIX_* names.
  # This manifest defines AISIX_REPO, AISIX_COMMIT and AISIX_SRC. The runner `source`s this manifest
  # with no `set -a`, so they stay unexported and never
  # reach the binary — but the DOCUMENTED override path (`AISIX_COMMIT=<sha> ./run.sh ...`) EXPORTS
  # them, and aisix would then reject `commit` / `repo` / `src` as unknown top-level config fields and
  # wipe out all 36 cells again, with the same misleading `launch_rc=0 / port not listening` surface.
  # `env -u` removes them from the child environment only; gw_build/gw_version still see them.
  setsid taskset -c "$CORES" env \
    -u AISIX_REPO -u AISIX_COMMIT -u AISIX_SRC \
    BENCH_AISIX_KEY="$GW_AUTH" \
    "$AISIX_BIN" --config "$GW_DIR/config.gen.yaml" </dev/null >"$GW_DIR/launch.log" 2>&1 &
}

gw_launch() {
  # The ONE static config (both provider_keys, both models). Nothing here varies by lane or column.
  _aisix_launch
}

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config this gateway launches with. AISIX is file-driven (a
# bootstrap config.gen.yaml that references a resources.gen.yaml), so the artifact is BOTH RENDERED
# files (exactly what --config loads) plus the one non-secret key env. The suite runner captures this
# once per run into results/config/aisix.txt and the board publishes it, so "fresh source build +
# these files → these numbers" is reproducible. The files are read from what gw_launch just rendered
# (falls back to rendering it if absent), so they can never drift from what aisix loaded. Because the
# config is STATIC — one file, both provider_keys, both models, identical bytes for every lane and
# every matrix egress column — this artifact IS the config every published aisix number was taken
# under; there is no second, unpublished config anywhere in the run.
# OOTB posture: three bootstrap settings (resources_file, proxy.addr, admin.admin_keys), and no
# observability block at all - metrics still bind :9090 because that is aisix's own default. Auth is
# mandatory (aisix has no anonymous mode) - one api_key, its plaintext via BENCH_AISIX_KEY, shown as
# its dummy value; provider api_key + admin_keys are dummy on the rig (no live secrets).
gw_config() {
  local cfg="$GW_DIR/config.gen.yaml" res="$GW_DIR/resources.gen.yaml"
  [ -f "$cfg" ] || _aisix_write_config
  echo "# ── config.gen.yaml (rendered; loaded via --config) ──"
  cat "$cfg"
  echo
  echo "# ── resources.gen.yaml (rendered; standalone resources_file) ──"
  cat "$res"
  echo
  echo "# ── launch env (non-secret; BENCH_AISIX_KEY is the mandatory api_key plaintext, dummy on the rig) ──"
  cat <<ENV
BENCH_AISIX_KEY=$GW_AUTH
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

# ONE STATIC CONFIG: both upstreams are already wired in the single resources file, so a column is
# selected by the CLIENT-facing model id alone — GW_MODEL picks the models[] entry, whose provider_key
# picks the adapter + api_base. The relaunch below runs byte-identical config to every other column and
# to gw_launch; nothing is rewritten. (Same shape as agentgateway/gomodel: flip the model, not the file.)
gw_matrix_egress() {
  case "$1" in
    openai)    GW_MODEL="$GW_MODEL_OPENAI";;
    anthropic) GW_MODEL="$GW_MODEL_ANTHROPIC";;
    *) return 1;;
  esac
  _aisix_launch
}

# Memory: BOTH readers cover the SAME process set (the target/release/aisix process tree), via the shared
# lib/harness.sh pair — the identical method + units the docker manifests get from
# container_rss_mib/container_hwm_mib. They used to disagree: gw_rss read a SINGLE pid's
# /proc/<pid>/status while gw_hwm walked the whole tree, so idle/peak/recovered and
# peak_rss_hwm_mib described different populations of the same gateway.
gw_rss() { native_rss_mib 'target/release/aisix'; }  # summed process-tree VmRSS
gw_hwm() { native_hwm_mib 'target/release/aisix'; }  # summed process-tree VmHWM (kernel high-water mark)

gw_diag() {
  echo "proc: $(pgrep -af 'target/release/aisix' | head -c 200)"
  echo "launch.log:"; tail -n 25 "$GW_DIR/launch.log" 2>/dev/null
}

gw_stop() { pkill -f 'target/release/aisix' 2>/dev/null; }

# gw_matrix_egress + the declared capability matrix are defined above (before gw_launch). The
# anthropic egress column + the openai-responses/anthropic ingress rows are wired-pending-field-
# verification; the EC2 field run turns each declared-1 cell green or red. Every grey cell is a cited
# capability limit (no ingress route, or an egress adapter with no dummy-key path).
