#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Busbar.
#
# Self-provisions like every other gateway — pulls the RELEASED image (BUSBAR_IMAGE in
# versions.env, e.g. getbusbar/busbar:1.4.1), extracts the static binary, and runs it as a native
# process so we measure the real process RSS/latency. Override with BUSBAR_BIN to point at a local
# working-tree build instead. Config: token auth, one model to the mock — no special setup.
GW_KIND=native
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="Busbar"                      # label in charts + report tables
GW_LANG=Rust                            # implementation language → bar color bucket
GW_CLASS="Control plane"   # the project's OWN self-description (Busbar's own site: control plane), not our editorial
GW_REPO=https://github.com/GetBusbar/busbar   # linked from the gateway name in the report table
GW_PORT=8080
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=bench-token
# Translation lane (xlate suite): busbar serves Anthropic ingress natively on /v1/messages and
# translates to the OpenAI upstream configured below. Same client token — busbar accepts it via
# either carrier (Authorization: Bearer or x-api-key), so no auth override is needed.
GW_ANTHROPIC_PATH=/v1/messages

gw_build() {
  if [ -z "${BUSBAR_BIN:-}" ]; then
    command -v docker >/dev/null || { echo "[busbar] need docker, or set BUSBAR_BIN"; return 1; }
    docker pull "${BUSBAR_IMAGE}" >/dev/null 2>&1 || { echo "[busbar] cannot pull ${BUSBAR_IMAGE}"; return 1; }
    docker rm -f busbar-extract >/dev/null 2>&1
    docker create --name busbar-extract "${BUSBAR_IMAGE}" >/dev/null 2>&1
    docker cp busbar-extract:/busbar "$GW_DIR/busbar" >/dev/null 2>&1 \
      || { echo "[busbar] no /busbar in ${BUSBAR_IMAGE}"; docker rm -f busbar-extract >/dev/null 2>&1; return 1; }
    docker rm -f busbar-extract >/dev/null 2>&1
    chmod +x "$GW_DIR/busbar"; BUSBAR_BIN="$GW_DIR/busbar"
  fi
}
gw_version() { local v; v="$("$BUSBAR_BIN" --version 2>/dev/null | head -1)"; echo "${v:-${BUSBAR_IMAGE:-busbar}}"; }

# ── matrix suite: full 6x6 egress support ─────────────────────────────────────────────────────────
# Busbar's provider config is protocol-first: `protocol: <dialect>` + `base_url` is the whole story,
# so all six upstream dialects are one providers.gen.yaml rewrite each. The matrix runner calls
# gw_matrix_egress <dialect> per column; it relaunches with the mock as an upstream of that shape.
# Declared capability: busbar claims ALL 36 (rows=ingress, cols=egress) - it accepts every ingress
# dialect and translates to every upstream dialect. Every cell is probed for a real pass/fail.
GW_MATRIX_CAP="
111111
111111
111111
111111
111111
111111
"
GW_MATRIX_CAP_NOTE="busbar declares full 6x6 translation support"
GW_MATRIX_EGRESS="openai openai-responses anthropic gemini cohere bedrock"

_busbar_launch_common() { # proto mock-key
  # Note on state: busbar 1.5+ snapshots runtime state (breaker cells included) to
  # busbar-state.json next to the config and restores it on boot. A bench launch must be
  # deterministic and stateless, a tripped breaker from a previous run must never carry over,
  # so every launch here sets BUSBAR_STATE_FILE= (empty disables persistence).
  local proto="$1" key="$2"
  cat > "$GW_DIR/providers.gen.yaml" <<YAML
mock:
  protocol: $proto
  base_url: http://127.0.0.1:$MOCK_PORT
  error_map: {}
YAML
  pkill -x busbar 2>/dev/null; sleep 1
  setsid taskset -c "$CORES" env \
    BUSBAR_WORKER_THREADS="$(( ${CORES##*-} + 1 ))" \
    BUSBAR_PROVIDERS="$GW_DIR/providers.gen.yaml" \
    BUSBAR_CONFIG="$GW_DIR/config.gen.yaml" \
    BUSBAR_STATE_FILE= \
    BENCH_MOCK_KEY="$key" \
    "$BUSBAR_BIN" </dev/null >/tmp/busbar.bench.log 2>&1 &
}

gw_matrix_egress() {
  local dialect="$1" proto="$1" key=x
  case "$dialect" in
    # busbar's name for the OpenAI Responses protocol is `responses`
    openai-responses) proto=responses ;;
    # bedrock egress is SigV4-signed; api_key_env carries ACCESS:SECRET signing material.
    # The mock ignores auth entirely, the pair just has to exist for the signer.
    bedrock) key="AKIAMOCKACCESSKEY:mock-secret-access-key" ;;
  esac
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
  _busbar_launch_common "$proto" "$key"
}

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
    BUSBAR_STATE_FILE= \
    BENCH_MOCK_KEY=x \
    "$BUSBAR_BIN" </dev/null >/tmp/busbar.bench.log 2>&1 &
}

gw_rss() { awk '/VmRSS/{printf "%.1f", $2/1024}' "/proc/$(pgrep -x busbar)/status" 2>/dev/null; }
gw_hwm() { _hwm_tree_mib "$(pgrep -x busbar 2>/dev/null | head -1)"; }  # kernel VmHWM of the busbar tree
gw_stop() { pkill -x busbar 2>/dev/null; }

# ── governed lane (governed/run.sh) ────────────────────────────────────────────────────────────────
# Busbar's governance layer is always compiled in but INERT until governance.admin_token is set.
# Setting it activates enforcement: static auth tokens are superseded and EVERY inference request
# must resolve to a busbar-minted virtual key (per-request key resolution + RPM/TPM accounting +
# an atomic budget check-and-charge on the hot path). gw_governed_launch writes the same config as
# gw_launch plus the governance block, boots busbar, then mints one virtual key over the admin API
# (POST /api/v1/admin/keys, x-admin-token auth). The key is minted with NO rpm/tpm/budget caps
# (absent = unlimited) and no pool ACL (empty allowed_pools = all pools): at 40k+ rps nothing can
# ever trip, so the lane measures the cost of the CHECK, not a limit. Store: sqlite :memory: (the
# entrant 1.4.1 governance store is SQLite; :memory: is ephemeral), so per-run keys never persist.
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
  # The released entrant (getbusbar/busbar:1.4.1) still has the governance on/off switch and a
  # SQLite-backed store: `enabled: true` ACTIVATES enforcement (without it admin_token is inert and
  # the admin API rejects the mint with 401 - the "never minted a key" failure), and db_path must be
  # ":memory:" because the default (busbar-governance.db next to the config) cannot be opened on the
  # bench's read-only working dir. :memory: is ephemeral, so per-run keys never leak across runs.
  enabled: true
  db_path: ":memory:"
  # admin_token ACTIVATES the admin plane: every request below must carry a minted virtual key.
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
    BUSBAR_STATE_FILE= \
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
