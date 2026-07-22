#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Busbar (main, unreleased).
#
# The current development head of Busbar, built from source on the box - a PREVIEW entrant so the
# field always shows where the next release lands next to the shipped image. Same config and launch
# as the released busbar manifest; only the provenance differs (git main vs Docker image).
GW_KIND=native
GW_DISPLAY="Busbar (main)"
GW_CLASS="Control plane"
GW_LANG=Rust
GW_REPO=https://github.com/GetBusbar/busbar
GW_PORT=8080
GW_ANTHROPIC_PATH=/v1/messages
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=bench-token

gw_build() {
  if [ -z "${BUSBAR_BIN:-}" ]; then
    command -v cargo >/dev/null || { echo "[busbar-main] need a rust toolchain"; return 1; }
    if [ ! -d "$GW_DIR/src-main" ]; then
      git clone --depth 1 https://github.com/GetBusbar/busbar "$GW_DIR/src-main" >/dev/null 2>&1 \
        || { echo "[busbar-main] clone failed"; return 1; }
    fi
    (cd "$GW_DIR/src-main" && cargo build --release -p busbar >/dev/null 2>&1) \
      || { echo "[busbar-main] build failed"; return 1; }
    BUSBAR_BIN="$GW_DIR/src-main/target/release/busbar"
  fi
}
gw_version() { local v; v="$("$BUSBAR_BIN" --version 2>/dev/null | head -1)"; echo "${v:-main} (git main)"; }

gw_launch() {
  cat > "$GW_DIR/config.gen.yaml" <<YAML
listen: "127.0.0.1:$GW_PORT"
observability:
  emit_server_timing: true
auth:
  chain: [tokens]
  upstream_credentials: own
  client_tokens:
    - "bench-token"
providers:
  mock:
    api_key_env: BENCH_MOCK_KEY
models:
  gpt-4o-mini:
    # max_concurrent is a REQUIRED field in busbar's model config (no default; see the config schema),
    # so it must be set. 8000 is above the sweep's winning concurrency band so it's a ceiling, not the
    # limiter — the equivalent of running every other gateway on its defaults (unbounded), not a tuned
    # advantage. max_requests: -1 = unmetered.
    provider: mock
    max_concurrent: 8000
    max_requests: -1
pools:
  bench-pool:
    members:
      - target: gpt-4o-mini
YAML
  cat > "$GW_DIR/providers.gen.yaml" <<YAML
mock:
  protocol: openai
  base_url: http://127.0.0.1:$MOCK_PORT
  error_map: {}
YAML
  pkill -x busbar 2>/dev/null; sleep 1
  setsid taskset -c "$CORES" env \
    BUSBAR_WORKER_THREADS="$(( ${CORES##*-} + 1 ))" \
    BUSBAR_PROVIDERS="$GW_DIR/providers.gen.yaml" \
    BUSBAR_CONFIG="$GW_DIR/config.gen.yaml" \
    BENCH_MOCK_KEY=x \
    "$BUSBAR_BIN" </dev/null >/tmp/busbar.bench.log 2>&1 &
}

gw_rss() { awk '/VmRSS/{printf "%.1f", $2/1024}' "/proc/$(pgrep -x busbar)/status" 2>/dev/null; }
gw_stop() { pkill -x busbar 2>/dev/null; }

# ── governed lane (governed/run.sh) ────────────────────────────────────────────────────────────────
# Busbar's governance layer is always compiled in but INERT until governance.admin_token is set.
# Setting it activates enforcement: static auth tokens are superseded and EVERY inference request
# must resolve to a busbar-minted virtual key (per-request key resolution + RPM/TPM accounting +
# an atomic budget check-and-charge on the hot path). gw_governed_launch writes the same config as
# gw_launch plus the governance block, boots busbar, then mints one virtual key over the admin API
# (POST /api/v1/admin/keys, x-admin-token auth). The key is minted with NO rpm/tpm/budget caps
# (absent = unlimited) and no pool ACL (empty allowed_pools = all pools): at 40k+ rps nothing can
# ever trip, so the lane measures the cost of the CHECK, not a limit. Store: memory (the default),
# so per-run keys never leak into a durable store.
BUSBAR_ADMIN_TOKEN="${BUSBAR_ADMIN_TOKEN:-bench-admin-token}"
BUSBAR_VKEY=""

gw_governed_launch() {
  # The admin plane always runs on its OWN listener (admin_listen, default loopback :8081), never
  # on the data listener — pin it explicitly so the mint below can't drift from a default change.
  BUSBAR_ADMIN_PORT=$(( GW_PORT + 1 ))
  cat > "$GW_DIR/config.gen.yaml" <<YAML
listen: "127.0.0.1:$GW_PORT"
admin_listen: "127.0.0.1:$BUSBAR_ADMIN_PORT"
observability:
  emit_server_timing: true
auth:
  chain: [tokens]
  upstream_credentials: own
  client_tokens:
    - "bench-token"
governance:
  # store: memory is the default (ephemeral). admin_token ACTIVATES enforcement:
  # every request below must carry a minted virtual key, checked per request.
  admin_token: "$BUSBAR_ADMIN_TOKEN"
providers:
  mock:
    api_key_env: BENCH_MOCK_KEY
models:
  gpt-4o-mini:
    # Same rationale as gw_launch: max_concurrent is a required field; 8000 sits above the sweep's
    # winning band so it is a ceiling, not the limiter. max_requests: -1 = unmetered.
    provider: mock
    max_concurrent: 8000
    max_requests: -1
pools:
  bench-pool:
    members:
      - target: gpt-4o-mini
YAML
  cat > "$GW_DIR/providers.gen.yaml" <<YAML
mock:
  protocol: openai
  base_url: http://127.0.0.1:$MOCK_PORT
  error_map: {}
YAML
  pkill -x busbar 2>/dev/null; sleep 1
  setsid taskset -c "$CORES" env \
    BUSBAR_WORKER_THREADS="$(( ${CORES##*-} + 1 ))" \
    BUSBAR_PROVIDERS="$GW_DIR/providers.gen.yaml" \
    BUSBAR_CONFIG="$GW_DIR/config.gen.yaml" \
    BENCH_MOCK_KEY=x \
    "$BUSBAR_BIN" </dev/null >/tmp/busbar.governed.log 2>&1 &
  # Wait for the admin plane, then mint the run's virtual key. The secret (sk-bb-<32 hex>) is
  # returned exactly once in the 201 body; it becomes the bench bearer token.
  local i resp
  for i in $(seq 1 60); do
    resp="$(curl -s -m3 -X POST "http://127.0.0.1:$BUSBAR_ADMIN_PORT/api/v1/admin/keys" \
      -H "x-admin-token: $BUSBAR_ADMIN_TOKEN" -H "content-type: application/json" \
      -d '{"name":"governed-bench"}' 2>/dev/null)"
    BUSBAR_VKEY="$(printf '%s' "$resp" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("secret",""))' 2>/dev/null)"
    [ -n "$BUSBAR_VKEY" ] && return 0
    sleep 1
  done
  echo "[busbar] governed: never minted a key; last admin response: $(printf '%s' "${resp:-}" | head -c 300)" >&2
  return 1
}

gw_governed_token() { echo "$BUSBAR_VKEY"; }
