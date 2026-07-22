#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Portkey OSS gateway (npx @portkey-ai/gateway).
#
# Routes to the mock via Portkey's own headers: x-portkey-provider + x-portkey-custom-host
# (the same way AIGatewayBench drives it). Anthropic Messages path.
GW_KIND=native
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="Portkey"                      # label in charts + report tables
GW_LANG=Node                            # implementation language → bar color bucket
GW_CLASS="AI gateway"   # the project's OWN self-description (README: 'blazing fast AI Gateway'), not our editorial
GW_REPO=https://github.com/Portkey-AI/gateway   # linked from the gateway name in the report table
GW_PORT=8787
GW_PATH=/v1/messages
GW_MODEL=anthropic/mock
GW_AUTH=dummy
# PORTKEY_SPEC comes from gateways/versions.env.
GW_HEADERS=(
  "x-portkey-provider: anthropic"
  "x-portkey-custom-host: http://127.0.0.1:${MOCK_PORT:-8000}/v1"
)

gw_build() { command -v npx >/dev/null || { echo "need node/npx for portkey"; return 1; }; }

# ── matrix suite: egress support ──────────────────────────────────────────────────────────────────
# Portkey selects the upstream provider PER REQUEST via x-portkey-provider + x-portkey-custom-host,
# so an egress "relaunch" is just a header swap. Every mapping below was verified against the
# recording mock: the request landed on the intended dialect endpoint with that dialect's request
# shape (google -> /v1beta/models/<m>:generateContent, cohere -> /v2/chat, bedrock (SigV4 with the
# dummy creds below, which the mock ignores) -> /model/<m>/converse, anthropic -> /v1/messages,
# openai -> /v1/chat/completions). openai-responses shares the openai provider: portkey's
# /v1/responses route passes through to the upstream Responses endpoint (its legitimate diagonal),
# but it has no chat->responses bridge, so those off-diagonal cells fail with the evidence.
# The openai ingress path override matters: this manifest's GW_PATH is /v1/messages (the perf
# suites drive portkey's anthropic lane), but the matrix's openai cell and warm-up need the chat
# route, which portkey serves on every provider.
GW_MATRIX_PATH_OPENAI=/v1/chat/completions
GW_MATRIX_EGRESS="openai openai-responses anthropic gemini cohere bedrock"
gw_matrix_egress() {
  local host="http://127.0.0.1:${MOCK_PORT:-8000}"
  case "$1" in
    openai|openai-responses)
      GW_HEADERS=("x-portkey-provider: openai" "x-portkey-custom-host: $host/v1");;
    anthropic)
      GW_HEADERS=("x-portkey-provider: anthropic" "x-portkey-custom-host: $host/v1");;
    gemini)
      GW_HEADERS=("x-portkey-provider: google" "x-portkey-custom-host: $host");;
    cohere)
      GW_HEADERS=("x-portkey-provider: cohere" "x-portkey-custom-host: $host");;
    bedrock)
      GW_HEADERS=("x-portkey-provider: bedrock" "x-portkey-custom-host: $host"
        "x-portkey-aws-access-key-id: AKIAMOCKACCESSKEY"
        "x-portkey-aws-secret-access-key: mock-secret-access-key"
        "x-portkey-aws-region: us-east-1");;
    *) return 1;;
  esac
  gw_launch
}

gw_launch() {
  pkill -f '@portkey-ai/gateway' 2>/dev/null; sleep 1
  setsid taskset -c "$CORES" npx -y "${PORTKEY_SPEC:-@portkey-ai/gateway}" \
    </dev/null >/tmp/portkey.mem.log 2>&1 &
}

_pk_pid() { ss -ltnpH "sport = :$GW_PORT" 2>/dev/null | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2; }
gw_rss() {
  local pid total=0 kb; pid="$(_pk_pid)"; [ -z "$pid" ] && { echo 0; return; }
  # node + any workers under the same process group
  for p in $pid $(pgrep -P "$pid" 2>/dev/null); do
    kb=$(awk '/VmRSS/{print $2}' "/proc/$p/status" 2>/dev/null); total=$((total + ${kb:-0}))
  done
  awk -v k="$total" 'BEGIN{printf "%.1f", k/1024}'
}
gw_version() { npm view "${PORTKEY_SPEC:-@portkey-ai/gateway}" version 2>/dev/null | sed 's/^/@portkey-ai\/gateway@/' || echo "@portkey-ai/gateway (npx latest)"; }
gw_diag() {
  echo "proc: $(pgrep -af '@portkey-ai/gateway' | head -c 200)"
  echo "run.log:"; tail -n 20 /tmp/portkey.mem.log 2>/dev/null
}
gw_stop() { pkill -f '@portkey-ai/gateway' 2>/dev/null; }
