#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Arch (katanemo/archgw) — Envoy data plane + Arch services, one arm64 container.
#
# Brought up via the archgw CLI (pip-installed) from an arch_config.yaml. This is archgw's canonical
# egress LLM-gateway config (port 12000, OpenAI ingress): a plain `llm_providers` list whose base_urls
# are the mock. prompt_targets / routing / guards are OPT-IN features you ADD by configuring them (they
# ship absent, not on-by-default), so an egress-only config is archgw's real-world default posture for
# "use me as an LLM proxy", not a feature-strip — nothing that ships enabled is being disabled. ALL the
# mock-reachable declared-egress providers are wired at once (openai default + amazon_bedrock), so the
# perf/memory/throughput/stream lanes and the matrix run the SAME config; the matrix only varies which
# provider-prefixed model the request body names. The CLI runs the container(s), which we then pin to
# $CORES. ARCH_VERSION is in gateways/versions.env.
GW_KIND=docker
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="Arch"                      # label in charts + report tables
GW_LANG=Other                            # implementation language → bar color bucket
GW_CLASS="AI-native proxy"   # the project's OWN self-description (README: 'AI-native proxy server for agents'), not our editorial
GW_REPO=https://github.com/katanemo/archgw   # linked from the gateway name in the report table
GW_PORT=12000
GW_PATH=/v1/chat/completions
GW_MODEL=openai/gpt-4o-mini
GW_AUTH=dummy
ARCH_VERSION="${ARCH_VERSION:-0.3.22}"
ARCH_VENV="${ARCH_VENV:-$GW_DIR/venv}"

gw_version() { echo "katanemo/archgw:$ARCH_VERSION (archgw CLI)"; }

gw_build() {
  [ -x "$ARCH_VENV/bin/archgw" ] && return 0
  python3 -m venv "$ARCH_VENV"
  "$ARCH_VENV/bin/pip" install -q --upgrade pip "archgw==$ARCH_VERSION" >"$GW_DIR/pip.log" 2>&1
  [ -x "$ARCH_VENV/bin/archgw" ] || { echo "archgw CLI install failed"; return 1; }
}

# Every archgw container (Envoy + bundled services); sum their process-tree VmRSS (same method as all).
_arch_cids() { sudo docker ps -q --filter "ancestor=katanemo/archgw:$ARCH_VERSION" 2>/dev/null; }
gw_rss() {
  local total=0 pid c
  for c in $(_arch_cids); do
    pid=$(sudo docker inspect -f '{{.State.Pid}}' "$c" 2>/dev/null)
    total=$(awk -v a="$total" -v b="$(_rss_tree_mib "$pid")" 'BEGIN{printf "%.1f", a+b}')
  done
  echo "$total"
}
gw_hwm() {  # kernel VmHWM summed over every archgw container's process tree (same set as gw_rss)
  local total=0 pid c any=0
  for c in $(_arch_cids); do
    any=1; pid=$(sudo docker inspect -f '{{.State.Pid}}' "$c" 2>/dev/null)
    total=$(awk -v a="$total" -v b="$(_hwm_tree_mib "$pid")" 'BEGIN{printf "%.1f", a+b}')
  done
  [ "$any" = 1 ] && echo "$total" || echo ""
}

# _arch_write_config: write archgw's ONE canonical egress config wiring every mock-reachable declared
# provider at once. archgw picks the upstream provider (and thus the native egress dialect its hermesllm
# transforms emit) from the request model-name prefix (openai/, amazon_bedrock/), so both providers are
# listed here simultaneously and the matrix just names one in the request body — no per-lane config
# swap. base_url is honoured by every provider (config_generator.py parses it into an explicit Envoy
# cluster); amazon_bedrock REQUIRES base_url, which we point at the mock (the SigV4/Converse request
# shape still applies, the mock ignores the signature). host.docker.internal (the CLI adds host-gateway)
# reaches the mock bound on 0.0.0.0. No access_key → no real provider key needed (dummy is implicit).
# openai is default:true so a bare/unprefixed model still routes somewhere sane.
#
# KNOWN LIMITATION (disclosed, not silently unfair): arch runs Envoy via the archgw CLI, and Envoy's
# worker concurrency defaults to std::thread::hardware_concurrency() = the HOST cpu count, blind to
# --cpuset-cpus (the same cpuset-blindness the nginx gateways have with `worker_processes auto` and Go
# had with GOMAXPROCS). Envoy's fix would be its `--concurrency <ncore>` CLI flag, but that flag lives
# in the Envoy bootstrap the archgw CLI generates and controls — there is no clean env we can inject
# through `archgw up` to pin it, and hacking archgw's internal bootstrap is out of scope. So Envoy's
# worker concurrency is NOT pinned to the cpuset here (unlike apisix/kong nginx worker_processes and the
# Go GOMAXPROCS pin). This is disclosed as a known measurement caveat, not silently ignored; arch serves
# only one matrix cell (openai->bedrock), bounding the impact.
_arch_write_config() {
  cat > "$GW_DIR/arch_config.yaml" <<YAML
version: v0.1.0
listeners:
  egress_traffic:
    address: 0.0.0.0
    port: $GW_PORT
    message_format: openai
    timeout: 30s
llm_providers:
  - model: openai/gpt-4o-mini
    base_url: http://host.docker.internal:$MOCK_PORT
    default: true
  - model: amazon_bedrock/anthropic.claude-3-sonnet-20240229-v1:0
    base_url: http://host.docker.internal:$MOCK_PORT
YAML
}

_arch_launch() {
  _arch_write_config
  "$ARCH_VENV/bin/archgw" down >/dev/null 2>&1
  "$ARCH_VENV/bin/archgw" up "$GW_DIR/arch_config.yaml" >"$GW_DIR/launch.log" 2>&1 || true
  # pin the arch containers to the gateway's cores once they're up (the CLI doesn't cpuset them)
  ( for _ in $(seq 1 30); do
      local cids; cids="$(_arch_cids)"
      [ -n "$cids" ] && { for c in $cids; do sudo docker update --cpuset-cpus="$CORES" "$c" >/dev/null 2>&1; done; break; }
      sleep 2
    done ) &
}

gw_launch() { _arch_launch; }

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config this gateway launches with — the rendered arch_config.yaml
# exactly as the archgw CLI loads it (read from the file _arch_write_config produced, so it can never
# drift; falls back to rendering it if not present yet). archgw needs no provider API key for this mock
# wiring, so there is no secret to dummy out. OOTB posture: an egress-only llm_providers config is
# archgw's real-world "LLM proxy" default (prompt_targets/routing/guards ship absent, not stripped); the
# only deviations are the permitted ones — provider base_urls → mock. No feature strips or perf tuning.
gw_config() {
  local cfg="$GW_DIR/arch_config.yaml"
  echo "# ── arch_config.yaml (rendered; loaded via 'archgw up arch_config.yaml') ──"
  [ -f "$cfg" ] || _arch_write_config
  cat "$cfg"
}

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock. archgw 0.3.22 routes by MODEL PREFIX (config_generator.py) and its dialect matrix
# (crates/hermesllm/src/providers/id.rs compatible_api_for_client) decides the upstream API per
# (provider, client-API) pair. For OpenAI-chat ingress the ONLY native translation that exists is
# Bedrock ((prefix amazon_bedrock) -> ConverseRequest, /model/<id>/converse); the Anthropic
# provider maps (Anthropic, OpenAIChatCompletions) -> OpenAIChatCompletions, i.e. archgw BY DESIGN
# targets Anthropic's OpenAI-compat surface and never emits native /v1/messages for openai-chat
# ingress (endpoints.rs target_endpoint_for_provider default arm -> /v1/chat/completions; native
# messages egress happens only when the CLIENT speaks the Anthropic Messages API). Likewise the
# Responses API exists only as its own ingress (OpenAI passthrough /responses); chat ingress is
# never upgraded to a Responses upstream. The earlier grid declared openai->anthropic and
# openai->responses anyway, and both published as 'untranslated passthrough' reds for behavior the
# project never claimed - they are grey capability limits, with the anthropic-INGRESS translation
# (anthropic->openai, which archgw does implement) exercised by the xlate lane instead. So the
# capable row is openai-ingress into {openai, bedrock}. NOT declared: openai-responses egress,
# anthropic egress (OpenAI-compat only), gemini (no native generateContent) and cohere (absent).
# Evidence: hermesllm providers/id.rs (compatible_api_for_client matrix), clients/endpoints.rs
# (target_endpoint_for_provider), transforms/request/from_openai.rs (Bedrock only), tag 0.3.22.
GW_MATRIX_CAP="
100001
000000
000000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="archgw 0.3.22 translates openai-chat ingress natively only to Bedrock Converse; Anthropic egress rides Anthropic's OpenAI-compat surface (id.rs maps (Anthropic, OpenAIChatCompletions) -> OpenAIChatCompletions, never native /v1/messages), Responses is ingress-only, and Gemini/Cohere have no native module; those cells are grey by that capability limit"
GW_MATRIX_EGRESS="openai bedrock"
gw_matrix_egress() {
  # Both providers are already wired in the ONE config (see _arch_write_config); GW_MODEL rides in the
  # ingress request body (openai message_format) and selects which by its provider prefix, so no config
  # rewrite is needed — the relaunch runs the identical all-providers config, just naming a new model.
  case "$1" in
    openai)  GW_MODEL="openai/gpt-4o-mini";;
    bedrock) GW_MODEL="amazon_bedrock/anthropic.claude-3-sonnet-20240229-v1:0";;
    *) return 1;;
  esac
  gw_launch
}

gw_diag() {
  echo "archgw ps: $(_arch_cids | tr '\n' ' ')"
  echo "launch.log:"; tail -n 20 "$GW_DIR/launch.log" 2>/dev/null
  echo "pip.log tail: $(tail -n 3 "$GW_DIR/pip.log" 2>/dev/null | tr '\n' ' ' | head -c 200)"
  for c in $(_arch_cids); do echo "-- $c --"; sudo docker logs --tail 12 "$c" 2>&1; done
}

gw_stop() { "$ARCH_VENV/bin/archgw" down >/dev/null 2>&1; }

# gw_matrix_egress + the declared capability matrix are defined above (before gw_launch). The
# non-openai egress columns are wired-pending-field-verification; the EC2 field run turns each
# declared-1 cell green or red. Every grey cell is a cited capability limit.
