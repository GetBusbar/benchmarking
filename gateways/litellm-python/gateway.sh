#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: LiteLLM Python proxy (official ghcr.io/berriai/litellm image, docker).
#
# Runs the official proxy image (LITELLM_PY_IMAGE in versions.env, multi-arch amd64+arm64, pinned
# to the benchmarked litellm==1.93.0) with the same uniform launch shape as the other docker
# gateways: host network, --cpuset-cpus pin, config mounted read-only. The image's entrypoint is
# the litellm CLI, so the --config/--port args are identical to the old pip-venv launch.
# RSS/HWM are read from the container's host-pid process tree (container_rss_mib), which sums the
# uvicorn workers exactly like the old _rss_tree_mib fix (m11).
#
# ── OOTB posture (one-config standard) ────────────────────────────────────────────────────────────
# This is the config a real user deploys, used unchanged for EVERY lane (latency/throughput/memory/
# stream/matrix). Only the permitted deviations are applied:
#   * provider base_urls → the mock (all six egress dialects wired below — the matrix exercises them
#     and memory/throughput are measured on this same all-providers config; NOT scoped per-lane);
#   * dummy api keys where a provider signer needs *some* credential (the mock ignores them);
#   * `--telemetry False` — the ONLY run-mechanic flag: litellm's proxy CLI defaults `--telemetry
#     True` (proxy_cli.py) and pings home; disabling outbound telemetry on the isolated rig is a
#     disclosed run-mechanic (same class as tensorzero's analytics-off env).
# REMOVED as forbidden deviations from a prior config:
#   * `--num_workers <core-count>`: worker-scaling is perf tuning. LiteLLM's documented default is
#     ONE uvicorn worker (constants.py DEFAULT_NUM_WORKERS_LITELLM_PROXY=1; proxy_cli.py --num_workers
#     default=1; prod-best-practices "Run one Uvicorn worker per pod ... this is the default"). OOTB =
#     the single-worker default, so the flag is dropped.
#   * LITELLM_MASTER_KEY: LiteLLM auth is OFF by default — with no master key the proxy serves
#     /v1/chat/completions UNPROTECTED (it accepts all requests; the master key is an opt-in admin
#     credential). Setting it ADDS an auth layer litellm does not ship on, which the standard forbids
#     ("don't add auth it doesn't default to"). Dropped; the gateway runs unprotected as shipped and
#     GW_AUTH is a dummy bearer the open endpoint ignores (same posture as the other unprotected
#     gateways here — gomodel/tensorzero/helicone).
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
# OOTB litellm serves unprotected (no master key). GW_AUTH is a dummy bearer the open endpoint
# ignores — the same convention every unprotected gateway in this bench uses (gomodel/tensorzero).
GW_AUTH=dummy
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

# _lp_write_config: render the ONE OOTB model_list. Every egress dialect the matrix probes is wired
# here (all → mock), so the SAME config serves perf/memory/throughput AND every matrix column — the
# config is never scoped per-lane. Each entry keeps the client-facing model name $GW_MODEL so the six
# ingress probes never change; the litellm_params `model:` prefix selects the upstream dialect and its
# api_base override points at the mock. Provider prefixes verified against litellm 1.93.0:
#   openai            openai/<model>, api_base <mock>/v1                     -> /v1/chat/completions
#   openai-responses  openai/responses/<model> (Responses bridge), <mock>/v1 -> /v1/responses
#   anthropic         anthropic/<claude>, api_base <mock>  (appends the path)-> /v1/messages
#   gemini            gemini/<model>, api_base <mock>                        -> /models/<m>:generateContent
#   cohere            cohere_chat/<model>, api_base <mock>/v2/chat (v2 chat) -> /v2/chat
#   bedrock           bedrock/converse/<model> + aws_bedrock_runtime_endpoint (dummy static creds;
#                     the mock ignores the SigV4 signature)                  -> /model/<m>/converse
# The perf lane sends $GW_MODEL, which resolves to the first matching model_name entry (openai) — the
# canonical OpenAI path, the real deployment's default — while the other five sit ready for the matrix.
_lp_write_config() {
  cat > "$GW_DIR/config.gen.yaml" <<YAML
model_list:
  - model_name: $GW_MODEL
    litellm_params:
      model: openai/$GW_MODEL
      api_base: http://127.0.0.1:$MOCK_PORT/v1
      api_key: dummy
  - model_name: $GW_MODEL-responses
    litellm_params:
      model: openai/responses/$GW_MODEL
      api_base: http://127.0.0.1:$MOCK_PORT/v1
      api_key: dummy
  - model_name: $GW_MODEL-anthropic
    litellm_params:
      model: anthropic/claude-3-5-sonnet-20241022
      api_base: http://127.0.0.1:$MOCK_PORT
      api_key: dummy
  - model_name: $GW_MODEL-gemini
    litellm_params:
      model: gemini/gemini-1.5-flash
      api_base: http://127.0.0.1:$MOCK_PORT
      api_key: dummy
  - model_name: $GW_MODEL-cohere
    litellm_params:
      model: cohere_chat/command-r
      api_base: http://127.0.0.1:$MOCK_PORT/v2/chat
      api_key: dummy
  - model_name: $GW_MODEL-bedrock
    litellm_params:
      model: bedrock/converse/anthropic.claude-3-sonnet-20240229-v1:0
      aws_bedrock_runtime_endpoint: http://127.0.0.1:$MOCK_PORT
      aws_access_key_id: AKIAMOCKACCESSKEY
      aws_secret_access_key: mock-secret-access-key
      aws_region_name: us-east-1
YAML
}

_lp_spawn() {
  sudo docker rm -f litellm-python-bench >/dev/null 2>&1; sleep 1
  # OOTB single-worker default (no --num_workers). --telemetry False is the disclosed telemetry-off
  # run-mechanic (proxy_cli.py defaults it True). container_rss_mib sums the whole host-pid tree.
  sudo docker run -d --name litellm-python-bench --network host --cpuset-cpus="$CORES" \
    -v "$GW_DIR/config.gen.yaml:/config.gen.yaml:ro" \
    "$LITELLM_PY_IMAGE" --config /config.gen.yaml --port "$GW_PORT" --telemetry False \
    >"$GW_DIR/launch.log" 2>&1 || true
}

gw_launch() {
  _lp_write_config
  _lp_spawn
}

# ── matrix suite: full 6x6 egress support ─────────────────────────────────────────────────────────
# LiteLLM's model_list selects the upstream dialect by provider prefix, each with an api_base
# override, so all six egress dialects are wired IN THE SINGLE CONFIG above (not per-lane). Every
# mapping was verified against the recording mock (litellm 1.93.0): the request landed on the intended
# dialect endpoint with that dialect's request shape.
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
# The single OOTB config already wires every egress dialect (all → mock), so each matrix column just
# selects the matching model_name; no per-lane relaunch or config rewrite is needed. Rendering the
# same config keeps the artifact identical to what perf/memory ran.
gw_matrix_egress() {
  case "$1" in
    openai)           GW_MODEL="$GW_MODEL";;  # canonical entry; client keeps sending $GW_MODEL
    openai-responses) GW_MODEL="${GW_MODEL}-responses";;
    anthropic)        GW_MODEL="${GW_MODEL}-anthropic";;
    gemini)           GW_MODEL="${GW_MODEL}-gemini";;
    cohere)           GW_MODEL="${GW_MODEL}-cohere";;
    bedrock)          GW_MODEL="${GW_MODEL}-bedrock";;
    *) return 1;;
  esac
  _lp_write_config
  _lp_spawn
}

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config this gateway launches with. LiteLLM is file-driven, so the
# artifact is the rendered model_list config (exactly what --config loads) PLUS the non-secret launch
# argv. The suite runner captures this once per run into results/config/litellm-python.txt and the
# board publishes it, so "fresh install + this config → these numbers" is reproducible. The config is
# read from the file _lp_write_config just rendered (falls back to rendering it if absent), so it can
# never drift from what the proxy loaded. OOTB posture: no master key (unprotected as shipped), single
# worker (default), all six providers wired to the mock; the only run-mechanic is --telemetry False.
gw_config() {
  local cfg="$GW_DIR/config.gen.yaml"
  echo "# ── config.gen.yaml (rendered; loaded via --config /config.gen.yaml) ──"
  [ -f "$cfg" ] || _lp_write_config
  cat "$cfg"
  echo
  echo "# ── launch argv (non-secret; provider api keys above are dummy on the isolated rig) ──"
  echo "litellm --config /config.gen.yaml --port $GW_PORT --telemetry False"
}

# container_rss_mib sums the container's whole host-pid process tree (same _rss_tree_mib method as
# native gateways), so any uvicorn workers are counted — preserving the m11 fix.
gw_rss() { container_rss_mib litellm-python-bench; }  # summed process-tree VmRSS (same method as native)
gw_hwm() { container_hwm_mib litellm-python-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_stop() { sudo docker rm -f litellm-python-bench >/dev/null 2>&1; }
