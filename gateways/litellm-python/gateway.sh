#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: LiteLLM Python proxy (official ghcr.io/berriai/litellm image, docker).
#
# Runs the official proxy image (LITELLM_PY_IMAGE, pinned below in this file; multi-arch amd64+arm64,
# to the benchmarked litellm==1.93.0) with the same uniform launch shape as the other docker
# gateways: host network, --cpuset-cpus pin, config mounted read-only. The image's entrypoint is
# the litellm CLI, so the --config/--port args are identical to the old pip-venv launch.
# RSS/HWM are read from the container's host-pid process tree (container_rss_mib), which sums the
# uvicorn workers exactly like the old _rss_tree_mib fix (m11).
#
# ── OOTB posture (one-config standard) ────────────────────────────────────────────────────────────
# This is the config a real user deploys, used unchanged for EVERY lane (latency/throughput/memory/
# stream/matrix). Only the permitted deviations are applied:
#   * provider base_urls → the mock (all six egress dialects wired below - the matrix exercises them
#     and memory/throughput are measured on this same all-providers config; NOT scoped per-lane);
#   * dummy api keys where a provider signer needs *some* credential (the mock ignores them).
# There are no other flags. The launch is `litellm --config <file> --port <port>`: the config points
# the upstreams at the mock and the port is the one the harness drives.
# REMOVED as forbidden deviations from a prior config:
#   * `--telemetry False`: litellm's proxy CLI defaults `--telemetry True` (proxy_cli.py), so a stock
#     install pings home. We measure defaults, and turning a shipped-on feature off is a config
#     change we do not make in either direction. The old justification (an isolated rig where the
#     ping would hang) was false: the bench boxes have full internet access - cloud-init apt-gets and
#     pulls images over it - so the ping succeeds. Consequence, disclosed rather than designed
#     around: with per-cell cold starts the proxy boots ~36 times a run, so a field run sends BerriAI
#     a few hundred install pings. That is litellm's shipped behaviour, which is what we measure.
#   * `--num_workers <core-count>`: worker-scaling is perf tuning. LiteLLM's documented default is
#     ONE uvicorn worker (constants.py DEFAULT_NUM_WORKERS_LITELLM_PROXY=1; proxy_cli.py --num_workers
#     default=1; prod-best-practices "Run one Uvicorn worker per pod ... this is the default"). OOTB =
#     the single-worker default, so the flag is dropped.
#   * LITELLM_MASTER_KEY: LiteLLM auth is OFF by default - with no master key the proxy serves
#     /v1/chat/completions UNPROTECTED (it accepts all requests; the master key is an opt-in admin
#     credential). Setting it ADDS an auth layer litellm does not ship on, which the standard forbids
#     ("don't add auth it doesn't default to"). Dropped; the gateway runs unprotected as shipped and
#     GW_AUTH is a dummy bearer the open endpoint ignores (same posture as the other unprotected
#     gateways in this bench).
GW_KIND=docker
# The docker container this manifest launches under. DECLARED here so that anything outside this
# directory which needs the name (the local verifier's teardown, for one) READS it from the
# manifest instead of hardcoding it. lib/gateway_isolation_test.sh checks it against the --name
# below, so the two cannot drift.
GW_CONTAINER=litellm-python-bench
# Self-describing manifest metadata - charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="LiteLLM · Python"                      # label in charts + report tables
GW_LANG=Python                            # implementation language → bar color bucket
GW_CLASS="LLM gateway"   # the project's OWN self-description (README: 'Proxy Server (LLM Gateway)'), not our editorial
GW_REPO=https://github.com/BerriAI/litellm   # linked from the gateway name in the report table
GW_PORT=8102
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
# OOTB litellm serves unprotected (no master key). GW_AUTH is a dummy bearer the open endpoint
# ignores - the same convention every unprotected gateway in this bench uses.
GW_AUTH=dummy

# ── CONFIG NECESSITY (lib/gateway_config_lint.sh) ─────────────────────────────────────────────────
# Every setting this manifest writes, and the ONE reason it is here. The lint fails on a setting with
# no claim AND on a claim with no setting, so this block cannot drift from the config in either
# direction. Reasons: boot (it will not run without this) | upstream (points an upstream at the test
# mock) | ingress (an ingress path the 6x6 matrix drives) | bind (the port or CPU pin the rig needs).
GW_CONFIG_WHY="
model_list     boot      # the proxy serves nothing without one
model_name     ingress   # the client-facing name each matrix column asks for
litellm_params boot
model          upstream  # the provider prefix selects the upstream dialect
api_base       upstream  # -> the mock
api_key        boot      # a provider entry with no key is rejected (dummy; the mock ignores it)
aws_bedrock_runtime_endpoint upstream  # bedrock's own base-URL override -> the mock
aws_access_key_id     boot   # the SigV4 signer needs some credential; the mock ignores the signature
aws_secret_access_key boot
aws_region_name       boot
"
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
# here (all → mock), so the SAME config serves perf/memory/throughput AND every matrix column - the
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
# The perf lane sends $GW_MODEL, which resolves to the first matching model_name entry (openai) - the
# canonical OpenAI path, the real deployment's default - while the other five sit ready for the matrix.
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
  # OOTB single-worker default (no --num_workers), OOTB telemetry (no --telemetry flag at all - see
  # the header). The argv is exactly the config and the port. container_rss_mib sums the whole
  # host-pid tree.
  sudo docker run -d --name litellm-python-bench --network host --cpuset-cpus="$CORES" \
    -v "$GW_DIR/config.gen.yaml:/config.gen.yaml:ro" \
    "$LITELLM_PY_IMAGE" --config /config.gen.yaml --port "$GW_PORT" \
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
# ORDER MATTERS (same bug class as the frozen ingress paths): _lp_write_config derives ALL SIX
# model_name entries from $GW_MODEL, so it must be rendered from the CANONICAL name - i.e. BEFORE this
# column's selection mutates GW_MODEL. Rendering it afterwards (as this did) wrote a model_list whose
# entries were derived from the already-suffixed value ("gpt-4o-mini-anthropic", "…-anthropic-responses",
# …), so the client's per-column name only ever matched the FIRST (openai) entry and every non-openai
# column silently egressed to openai. The runner restores GW_MODEL to the manifest baseline before each
# column (matrix/run.sh:restore_manifest_baseline), so $GW_MODEL below is always the canonical name and
# the suffix can never compound across the 6x6.
gw_matrix_egress() {
  local canon="$GW_MODEL" sel
  case "$1" in
    openai)           sel="$canon";;            # canonical entry; client keeps sending $GW_MODEL
    openai-responses) sel="${canon}-responses";;
    anthropic)        sel="${canon}-anthropic";;
    gemini)           sel="${canon}-gemini";;
    cohere)           sel="${canon}-cohere";;
    bedrock)          sel="${canon}-bedrock";;
    *) return 1;;
  esac
  _lp_write_config          # rendered while GW_MODEL is still the canonical name
  GW_MODEL="$sel"           # …then select this column's client-facing model name
  _lp_spawn
}

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config this gateway launches with. LiteLLM is file-driven, so the
# artifact is the rendered model_list config (exactly what --config loads) PLUS the non-secret launch
# argv. The suite runner captures this once per run into results/config/litellm-python.txt and the
# board publishes it, so "fresh install + this config → these numbers" is reproducible. The config is
# read from the file _lp_write_config just rendered (falls back to rendering it if absent), so it can
# never drift from what the proxy loaded. OOTB posture: no master key (unprotected as shipped), single
# worker (default), telemetry at its default (on), all six providers wired to the mock. Nothing else.
gw_config() {
  local cfg="$GW_DIR/config.gen.yaml"
  echo "# ── config.gen.yaml (rendered; loaded via --config /config.gen.yaml) ──"
  [ -f "$cfg" ] || _lp_write_config
  cat "$cfg"
  echo
  echo "# ── launch argv (non-secret; provider api keys above are dummy on the isolated rig) ──"
  echo "litellm --config /config.gen.yaml --port $GW_PORT"
}

# container_rss_mib sums the container's whole host-pid process tree (same _rss_tree_mib method as
# native gateways), so any uvicorn workers are counted - preserving the m11 fix.
gw_rss() { container_rss_mib litellm-python-bench; }  # summed process-tree VmRSS (same method as native)
gw_hwm() { container_hwm_mib litellm-python-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_stop() { sudo docker rm -f litellm-python-bench >/dev/null 2>&1; }
