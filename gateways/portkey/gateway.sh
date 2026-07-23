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

# ── xlate lane (anthropic-in -> openai-out) ───────────────────────────────────────────────────────
# NOT DECLARED, and previously mis-tested: the xlate lane reused this manifest's default
# GW_HEADERS (x-portkey-provider: anthropic), which routed the anthropic probe straight to the
# anthropic upstream - an untranslated passthrough manufactured by OUR config, published as a
# Portkey failure. The honest header set for the lane is provider=openai (kept below via the
# generic GW_XLATE_HEADERS hook so any future re-probe tests the right thing), but the OSS gateway
# has no messages->chatComplete bridge at all: /v1/messages with provider=openai throws
# "messages is not supported by openai" (src/services/transformToProviderRequest.ts:158; verified
# at tag v1.15.2 and main 2026-07 - the universal Messages translation exists only in Portkey's
# hosted product). So the capability is declared 0 with that citation, not probed, never a red.
GW_XLATE_CAP=0
GW_XLATE_CAP_NOTE="Portkey OSS has no anthropic-Messages -> openai-chatComplete bridge: /v1/messages is a per-provider route and providers without a messages config throw 'messages is not supported by <provider>' (transformToProviderRequest.ts:158, verified at v1.15.2 and main)"
GW_XLATE_HEADERS=(
  "x-portkey-provider: openai"
  "x-portkey-custom-host: http://127.0.0.1:${MOCK_PORT:-8000}/v1"
)

# ── stream lane: known upstream bug, cited ────────────────────────────────────────────────────────
# stream:true on the npx/node runtime fails with `tryTargetsRecursively error: immutable`
# ({"status":"failure","message":"Something went wrong"}): Portkey-AI/gateway issue #1389 (open) -
# ResponseService.updateHeaders mutates an immutable-guarded Response under @hono/node-server
# >1.14.2 (pulled by the ^1.3.3 caret pin); #1550 is the same class on /v1/responses; community
# fixes #1390/#1551 are unmerged as of 2026-07. A real streaming failure of the shipped npx
# artifact, so it is measured and published - this note carries the citation.
GW_STREAM_NOTE="stream:true fails on the Node (npx) runtime with 'tryTargetsRecursively error: immutable' - Portkey-AI/gateway#1389 (open; same class as #1550, fixes #1390/#1551 unmerged as of 2026-07): an upstream bug in the shipped artifact, not a harness gap"

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
# Declared capability (rows=ingress, cols=egress; order openai openai-responses anthropic gemini
# cohere bedrock): Portkey accepts the OpenAI ingress into any of its providers (openai, anthropic,
# google, cohere, bedrock) - that row is 1 across those five. The ANTHROPIC ingress (/v1/messages)
# is a per-provider route with NO translation layer: messagesHandler looks the `messages` fn up in
# the provider's own config (src/handlers/messagesHandler.ts) and providers without one throw
# "messages is not supported by <provider>" (src/services/transformToProviderRequest.ts:158). At
# v1.15.2 (and main as of 2026-07) only anthropic (native), bedrock (Anthropic/Converse messages
# configs, src/providers/bedrock/index.ts:114,216-236), vertex-anthropic and azure-ai-inference
# implement it - openai/google/cohere do NOT, so the anthropic-ingress row declares ONLY the
# anthropic diagonal + bedrock. (An earlier draft declared the full anthropic row and manufactured
# three reds Portkey never claimed.) No chat->responses bridge exists either, so the only responses
# cell is the responses->responses passthrough diagonal.
GW_MATRIX_CAP="
101111
010000
001001
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="Portkey's /v1/messages route serves only providers that implement a messages config (anthropic, bedrock, vertex-anthropic, azure-ai-inference at v1.15.2; openai/google/cohere throw 'messages is not supported by <provider>', transformToProviderRequest.ts:158), and it has no chat-to-Responses bridge; other cells are grey by that capability limit"
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
gw_hwm() {  # kernel VmHWM summed over the node process + its workers (same tree as gw_rss)
  local pid total=0 kb; pid="$(_pk_pid)"; [ -z "$pid" ] && { echo ""; return; }
  for p in $pid $(pgrep -P "$pid" 2>/dev/null); do
    kb=$(awk '/VmHWM/{print $2}' "/proc/$p/status" 2>/dev/null); total=$((total + ${kb:-0}))
  done
  awk -v k="$total" 'BEGIN{printf "%.1f", k/1024}'
}
gw_version() { npm view "${PORTKEY_SPEC:-@portkey-ai/gateway}" version 2>/dev/null | sed 's/^/@portkey-ai\/gateway@/' || echo "@portkey-ai/gateway (npx latest)"; }
gw_diag() {
  echo "proc: $(pgrep -af '@portkey-ai/gateway' | head -c 200)"
  echo "run.log:"; tail -n 20 /tmp/portkey.mem.log 2>/dev/null
}
gw_stop() { pkill -f '@portkey-ai/gateway' 2>/dev/null; }
