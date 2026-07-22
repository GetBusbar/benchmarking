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

gw_launch() {
  # base-url gets "v1/chat/completions" appended by the gateway → hits the mock's OpenAI endpoint.
  cat > "$GW_DIR/config.gen.yaml" <<YAML
server:
  address: 0.0.0.0
  port: $GW_PORT
helicone:
  features: none
providers:
  openai:
    base-url: "http://127.0.0.1:$MOCK_PORT/"
routers:
  default:
    load-balance:
      chat:
        strategy: weighted
        providers:
          - provider: openai
            weight: 1.0
YAML
  pkill -f 'target/release/ai-gateway' 2>/dev/null; sleep 1
  setsid taskset -c "$CORES" env \
    OPENAI_API_KEY=sk-dummy \
    AI_GATEWAY__SERVER__PORT="$GW_PORT" \
    AI_GATEWAY__SERVER__ADDRESS=0.0.0.0 \
    "$HELICONE_BIN" -c "$GW_DIR/config.gen.yaml" </dev/null >/tmp/helicone.bench.log 2>&1 &
}

gw_rss() { awk '/VmRSS/{printf "%.1f", $2/1024}' "/proc/$(pgrep -f 'target/release/ai-gateway' | head -1)/status" 2>/dev/null; }

gw_diag() {
  echo "proc: $(pgrep -af 'target/release/ai-gateway' | head -c 200)"
  echo "run.log:"; tail -n 25 /tmp/helicone.bench.log 2>/dev/null
}

gw_stop() { pkill -f 'target/release/ai-gateway' 2>/dev/null; }
