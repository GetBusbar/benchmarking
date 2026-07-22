#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Arch (katanemo/archgw) — Envoy data plane + Arch services, one arm64 container.
#
# Brought up via the archgw CLI (pip-installed) from an arch_config.yaml. We use ONLY the egress LLM
# gateway (port 12000, OpenAI passthrough) with a single llm_provider whose base_url is the mock — no
# prompt_targets / routing / guards, so the guard/router models never run (pure proxy overhead). The
# CLI runs the container(s), which we then pin to $CORES. ARCH_VERSION is in gateways/versions.env.
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

gw_launch() {
  # Egress-only config = pure proxy. host.docker.internal (the CLI adds host-gateway) reaches the mock
  # bound on 0.0.0.0. No access_key → no real provider key needed.
  cat > "$GW_DIR/arch_config.yaml" <<YAML
version: v0.1.0
listeners:
  egress_traffic:
    address: 0.0.0.0
    port: $GW_PORT
    message_format: openai
    timeout: 30s
llm_providers:
  - model: $GW_MODEL
    base_url: http://host.docker.internal:$MOCK_PORT
    default: true
YAML
  "$ARCH_VENV/bin/archgw" down >/dev/null 2>&1
  "$ARCH_VENV/bin/archgw" up "$GW_DIR/arch_config.yaml" >"$GW_DIR/launch.log" 2>&1 || true
  # pin the arch containers to the gateway's cores once they're up (the CLI doesn't cpuset them)
  ( for _ in $(seq 1 30); do
      local cids; cids="$(_arch_cids)"
      [ -n "$cids" ] && { for c in $cids; do sudo docker update --cpuset-cpus="$CORES" "$c" >/dev/null 2>&1; done; break; }
      sleep 2
    done ) &
}

gw_diag() {
  echo "archgw ps: $(_arch_cids | tr '\n' ' ')"
  echo "launch.log:"; tail -n 20 "$GW_DIR/launch.log" 2>/dev/null
  echo "pip.log tail: $(tail -n 3 "$GW_DIR/pip.log" 2>/dev/null | tr '\n' ' ' | head -c 200)"
  for c in $(_arch_cids); do echo "-- $c --"; sudo docker logs --tail 12 "$c" 2>&1; done
}

gw_stop() { "$ARCH_VENV/bin/archgw" down >/dev/null 2>&1; }
