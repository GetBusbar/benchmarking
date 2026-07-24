#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Portkey OSS gateway (official portkeyai/gateway image, docker).
#
# Runs the official image (PORTKEY_IMAGE in versions.env, multi-arch amd64+arm64, pinned to the
# benchmarked 1.15.2) with the same uniform launch shape as the other docker gateways: host
# network, --cpuset-cpus pin. The image needs no config — routing to the mock is per-request via
# Portkey's own headers: x-portkey-provider + x-portkey-custom-host (the same way AIGatewayBench
# drives it). Anthropic Messages path. The image listens on 8787, portkey's default (= GW_PORT).
GW_KIND=docker
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
# PORTKEY_IMAGE comes from gateways/versions.env.
PORTKEY_IMAGE="${PORTKEY_IMAGE:-portkeyai/gateway:1.15.2}"
GW_HEADERS=(
  "x-portkey-provider: anthropic"
  "x-portkey-custom-host: http://127.0.0.1:${MOCK_PORT:-8000}/v1"
)

gw_build() { sudo docker pull "$PORTKEY_IMAGE" >/dev/null 2>&1 || true; }

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

# ── stream lane: npx-artifact bug does NOT reproduce on the official image ───────────────────────
# The npx artifact's stream:true failed with `tryTargetsRecursively error: immutable`
# (Portkey-AI/gateway#1389, open as of 2026-07): ResponseService.updateHeaders mutates an
# immutable-guarded Response under @hono/node-server >1.14.2, pulled in at npx-install time by the
# ^1.3.3 caret pin. The official portkeyai/gateway:1.15.2 image bakes a lockfile-resolved
# node_modules, and stream:true against an SSE mock passes through cleanly (verified locally,
# 2026-07-23) - the bug was an artifact of the floating npx dependency resolution, not of the
# pinned image this manifest now runs. The note below rides into the published stream result;
# the EC2 field run re-measures the lane on the image.
GW_STREAM_NOTE="stream lane runs the official portkeyai/gateway:1.15.2 image (lockfile-resolved deps); the npx artifact's 'tryTargetsRecursively error: immutable' failure (Portkey-AI/gateway#1389, caused by the caret-pinned @hono/node-server floating past 1.14.2 at npx-install time) does not reproduce on the image - verified locally against an SSE mock, re-verified in the field run"

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
  sudo docker rm -f portkey-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name portkey-bench --network host --cpuset-cpus="$CORES" \
    "$PORTKEY_IMAGE" >"$GW_DIR/launch.log" 2>&1 || true
}

gw_rss() { container_rss_mib portkey-bench; }  # summed process-tree VmRSS (same method as native)
gw_hwm() { container_hwm_mib portkey-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$PORTKEY_IMAGE" 2>/dev/null)
  echo "${PORTKEY_IMAGE}${dg:+ (@${dg##*@})}"
}
gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=portkey-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 portkey-bench 2>&1
}
gw_stop() { sudo docker rm -f portkey-bench >/dev/null 2>&1; }
