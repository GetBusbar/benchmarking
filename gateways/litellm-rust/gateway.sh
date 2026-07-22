#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: LiteLLM-Rust (BerriAI's compiled AI-gateway beta).
#
# IMPORTANT (verified against their source at commit 698072308, the public
# `litellm_rust_gateway_v1_messages_route` branch): the /v1/messages route serves ONLY the
# `azure_ai` provider — messages_provider_config() returns None for `anthropic`/`openai`, and
# their own unit test asserts it. AND it only actually serves when launched with the
# `python-config` reader (LITELLM_CONFIG_PATH + an importable `litellm`); the lean env config
# returns HTTP 400. So the ONLY configuration that serves this endpoint is the one below — which
# embeds CPython and loads the full `litellm` package (~275 MB RSS). That is the honest,
# only-working launch, not a strawman.
GW_KIND=native
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="LiteLLM · Rust"                      # label in charts + report tables
GW_LANG=Rust                            # implementation language → bar color bucket
GW_CLASS="LLM gateway"   # the project's OWN self-description (README: 'Proxy Server (LLM Gateway)'), not our editorial
GW_REPO=https://github.com/BerriAI/litellm   # linked from the gateway name in the report table
GW_PORT=8101
GW_PATH=/v1/messages
GW_MODEL=azure_ai/mock
GW_AUTH=gwbench
# Source refs come from gateways/versions.env (the runner sources it first) — override there, not here.
LITELLM_SRC="${LITELLM_SRC:-$HOME/litellm-rust-src}"
LR_VENV="${LR_VENV:-$GW_DIR/venv}"

gw_build() {
  command -v cargo >/dev/null || { echo "need cargo (rust) for litellm-rust"; return 1; }
  [ -x "$LR_VENV/bin/python" ] || python3 -m venv "$LR_VENV"
  "$LR_VENV/bin/pip" install -q --upgrade pip "${LITELLM_PY_SPEC:-litellm[proxy]}" >/dev/null 2>&1 || true
  if [ ! -d "$LITELLM_SRC" ]; then
    git clone -b "${LITELLM_RUST_BRANCH}" "${LITELLM_RUST_REPO}" "$LITELLM_SRC"
    [ -n "${LITELLM_RUST_COMMIT:-}" ] && git -C "$LITELLM_SRC" checkout -q "$LITELLM_RUST_COMMIT"
  fi
  ( cd "$LITELLM_SRC/litellm-rust" && cargo build --release -p litellm-ai-gateway --features server,python-config )
  LR_BIN="$(find "$LITELLM_SRC/litellm-rust/target/release" -maxdepth 1 -type f -perm -u+x \
    \( -name 'litellm-ai-gateway' -o -name 'litellm_ai_gateway' -o -name 'server' \) 2>/dev/null | head -1)"
  [ -n "${LR_BIN:-}" ] || { echo "litellm-rust binary not found"; return 1; }
}

gw_version() {
  local sha ver
  sha="$(git -C "$LITELLM_SRC" rev-parse --short HEAD 2>/dev/null)"
  ver="$("$LR_VENV/bin/python" -c 'import litellm;print(litellm.__version__)' 2>/dev/null)"
  echo "${LITELLM_RUST_BRANCH}@${sha:-?} (python-config litellm==${ver:-?})"
}

gw_launch() {
  # azure_ai model pointing at the mock; api_base ending in /v1/messages is used verbatim by
  # complete_azure_anthropic_url, so it hits the mock's Messages endpoint directly.
  cat > "$GW_DIR/config.gen.yaml" <<YAML
model_list:
  - model_name: $GW_MODEL
    litellm_params:
      model: $GW_MODEL
      api_base: http://127.0.0.1:$MOCK_PORT/v1/messages
      api_key: dummy
YAML
  local site; site="$("$LR_VENV/bin/python" -c 'import site;print(site.getsitepackages()[0])' 2>/dev/null)"
  pkill -f litellm-ai-gateway 2>/dev/null; sleep 1
  setsid taskset -c "$CORES" env \
    PYTHONPATH="$site" \
    LITELLM_MASTER_KEY="$GW_AUTH" \
    LITELLM_CONFIG_PATH="$GW_DIR/config.gen.yaml" \
    PORT="$GW_PORT" \
    "$LR_BIN" </dev/null >/tmp/litellm_rust.mem.log 2>&1 &
}

# ── matrix suite egress support ───────────────────────────────────────────────────────────────────
# The ONLY working route in this gateway (azure_ai via python-config, see the header) targets the
# upstream's /v1/messages: an ANTHROPIC-shaped endpoint. So its one configurable egress dialect is
# anthropic, and the launch for it is exactly the normal launch. No other upstream dialect is
# reachable: messages_provider_config() serves only azure_ai, and its api_base is the /v1/messages
# URL by construction, so openai/gemini/cohere/bedrock/responses egress columns are honestly
# "not configurable" rather than tried-and-failed.
GW_MATRIX_EGRESS="anthropic"
gw_matrix_egress() {
  case "$1" in
    anthropic) gw_launch ;;
    *) return 1 ;;
  esac
}

gw_rss() { awk '/VmRSS/{printf "%.1f", $2/1024}' "/proc/$(pgrep -f litellm-ai-gateway | head -1)/status" 2>/dev/null; }
gw_hwm() { _hwm_tree_mib "$(pgrep -f litellm-ai-gateway 2>/dev/null | head -1)"; }  # kernel VmHWM of the gateway tree

gw_diag() {
  echo "proc: $(pgrep -af litellm-ai-gateway | head -c 200)"
  echo "run.log:"; tail -n 20 /tmp/litellm_rust.mem.log 2>/dev/null
}

gw_stop() { pkill -f litellm-ai-gateway 2>/dev/null; }

# Governed lane: intentionally not wired. LiteLLM's virtual-key surface (/key/generate) lives in
# the Python proxy and requires a Postgres database (DATABASE_URL) behind the master key; the Rust
# gateway beta exposes no self-contained key-mint path this harness could script against a local
# mock. governed/run.sh records governed_served=false for this manifest.
