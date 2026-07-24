#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: LiteLLM Python proxy (official ghcr.io/berriai/litellm image, docker).
#
# Runs the official proxy image (LITELLM_PY_IMAGE in versions.env, multi-arch amd64+arm64, pinned
# to the benchmarked litellm==1.93.0) with the same uniform launch shape as the other docker
# gateways: host network, --cpuset-cpus pin, config mounted read-only. The image's entrypoint is
# the litellm CLI, so the --config/--port/--num_workers args are identical to the old pip-venv
# launch. RSS/HWM are read from the container's host-pid process tree (container_rss_mib), which
# sums the uvicorn workers exactly like the old _rss_tree_mib fix (m11).
GW_KIND=docker
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="LiteLLM · Python"                      # label in charts + report tables
GW_LANG=Python                            # implementation language → bar color bucket
GW_CLASS="LLM gateway"   # the project's OWN self-description (README: 'Proxy Server (LLM Gateway)'), not our editorial
GW_REPO=https://github.com/BerriAI/litellm   # linked from the gateway name in the report table
GW_PORT=8102
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=gwbench
LITELLM_PY_IMAGE="${LITELLM_PY_IMAGE:-ghcr.io/berriai/litellm:v1.93.0}"

gw_build() {
  sudo docker pull "$LITELLM_PY_IMAGE" >/dev/null 2>&1 || true
}

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$LITELLM_PY_IMAGE" 2>/dev/null)
  echo "${LITELLM_PY_IMAGE}${dg:+ (@${dg##*@})}"
}

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=litellm-python-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 litellm-python-bench 2>&1
}

_lp_spawn() {
  sudo docker rm -f litellm-python-bench >/dev/null 2>&1; sleep 1
  # Scale uvicorn workers to the pinned core count so LiteLLM uses all 4 cores it's given, not one
  # (single-worker on a 4-core pin under-serves it — fairness M5). The gw_rss sums the whole group.
  local ncore=$(( ${CORES##*-} - ${CORES%%-*} + 1 ))
  sudo docker run -d --name litellm-python-bench --network host --cpuset-cpus="$CORES" \
    -e LITELLM_MASTER_KEY="$GW_AUTH" \
    -v "$GW_DIR/config.gen.yaml:/config.gen.yaml:ro" \
    "$LITELLM_PY_IMAGE" --config /config.gen.yaml --port "$GW_PORT" --num_workers "$ncore" \
    >"$GW_DIR/launch.log" 2>&1 || true
}

gw_launch() {
  cat > "$GW_DIR/config.gen.yaml" <<YAML
model_list:
  - model_name: $GW_MODEL
    litellm_params:
      model: openai/$GW_MODEL
      api_base: http://127.0.0.1:$MOCK_PORT/v1
      api_key: dummy
YAML
  _lp_spawn
}

# ── matrix suite: full 6x6 egress support ─────────────────────────────────────────────────────────
# LiteLLM's model_list selects the upstream dialect by provider prefix, each with an api_base
# override, so all six egress dialects are one config rewrite each. Every mapping below was
# verified against the recording mock (litellm 1.93.0): the request landed on the intended
# dialect endpoint with that dialect's request shape.
#   openai            openai/<model>, api_base <mock>/v1                     -> /v1/chat/completions
#   openai-responses  openai/responses/<model> (Responses bridge), same base -> /v1/responses
#   anthropic         anthropic/<claude>, api_base <mock>  (appends the path)-> /v1/messages
#   gemini            gemini/<model>, api_base <mock>                        -> /models/<m>:generateContent
#   cohere            cohere_chat/<model>, api_base <mock>/v2/chat (verbatim)-> /v2/chat
#   bedrock           bedrock/converse/<model> + aws_bedrock_runtime_endpoint (dummy static creds;
#                     the mock ignores the SigV4 signature)                  -> /model/<m>/converse
# The client-side model name stays $GW_MODEL in every case, so the six ingress probes never change.
# Declared capability (rows=ingress, cols=egress; order openai openai-responses anthropic gemini
# cohere bedrock): LiteLLM's core value is the OpenAI-canonical ingress translated to ANY provider
# upstream, so the openai row is 1 across all six egress dialects. LiteLLM also exposes native
# Anthropic (/v1/messages) and Responses (/v1/responses) ingress surfaces, so those two diagonals are
# 1. Gemini/cohere/bedrock INGRESS are not declared here (grey) - LiteLLM's documented translation is
# OpenAI-in, not a full ingress cross-product.
GW_MATRIX_CAP="
111111
010000
001000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="LiteLLM accepts OpenAI ingress into any provider plus native Anthropic/Responses ingress diagonals; other ingress rows are grey by declaration"
GW_MATRIX_EGRESS="openai openai-responses anthropic gemini cohere bedrock"
gw_matrix_egress() {
  local params
  case "$1" in
    openai) params="model: openai/$GW_MODEL
      api_base: http://127.0.0.1:$MOCK_PORT/v1
      api_key: dummy";;
    openai-responses) params="model: openai/responses/$GW_MODEL
      api_base: http://127.0.0.1:$MOCK_PORT/v1
      api_key: dummy";;
    anthropic) params="model: anthropic/claude-3-5-sonnet-20241022
      api_base: http://127.0.0.1:$MOCK_PORT
      api_key: dummy";;
    gemini) params="model: gemini/gemini-1.5-flash
      api_base: http://127.0.0.1:$MOCK_PORT
      api_key: dummy";;
    cohere) params="model: cohere_chat/command-r
      api_base: http://127.0.0.1:$MOCK_PORT/v2/chat
      api_key: dummy";;
    bedrock) params="model: bedrock/converse/anthropic.claude-3-sonnet-20240229-v1:0
      aws_bedrock_runtime_endpoint: http://127.0.0.1:$MOCK_PORT
      aws_access_key_id: AKIAMOCKACCESSKEY
      aws_secret_access_key: mock-secret-access-key
      aws_region_name: us-east-1";;
    *) return 1;;
  esac
  cat > "$GW_DIR/config.gen.yaml" <<YAML
model_list:
  - model_name: $GW_MODEL
    litellm_params:
      $params
YAML
  _lp_spawn
}

# --num_workers spawns uvicorn WORKER children; container_rss_mib sums the container's whole
# host-pid process tree (same _rss_tree_mib method as native gateways), so the workers are counted
# — preserving the m11 fix (a parent-only match reported flat idle==peak because the workers were
# invisible).
gw_rss() { container_rss_mib litellm-python-bench; }  # summed process-tree VmRSS (same method as native)
gw_hwm() { container_hwm_mib litellm-python-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_stop() { sudo docker rm -f litellm-python-bench >/dev/null 2>&1; }
