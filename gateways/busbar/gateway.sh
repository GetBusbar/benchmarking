#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Busbar (docker).
#
# Runs the RELEASED image users download (BUSBAR_IMAGE in versions.env, e.g. getbusbar/busbar:1.4.1,
# multi-arch amd64+arm64) as its own container — the same uniform launch shape as every other docker
# gateway (host network, pinned cpuset, config mounted read-only). RSS/HWM are read from the
# container's host-pid process tree (container_rss_mib), the same units as a native process.
# Override BUSBAR_IMAGE to benchmark a locally-built image.
#
# ── OOTB posture (one-config standard — busbar is the harness author's gateway; held to the SAME bar,
#    NO favoritism) ─────────────────────────────────────────────────────────────────────────────────
# This is the config a real user deploys, used unchanged for EVERY lane (latency/throughput/memory/
# stream/matrix). All SIX upstream protocols busbar supports are wired here (→ mock), because the
# matrix exercises all of them and memory/throughput are measured on this same all-providers config —
# NOT scoped per-lane. Permitted deviations only:
#   * each provider's base_url → the mock;
#   * dummy signing material where a protocol's signer needs it (bedrock SigV4 → ACCESS:SECRET pair;
#     the mock ignores the signature);
#   * client-token auth KEPT (see below) — busbar's own posture, not added-for-the-bench.
# Run-mechanic: BUSBAR_STATE_FILE= (empty) disables the health/audit state snapshot so every run is
# deterministic and stateless (a tripped breaker never carries over) — the equivalent of an in-memory
# store; state persistence carries no request-path behavior. Disclosed.
#
# FAIRNESS AUDIT (v1.4.1 released tag; each line checked against that tag's source):
#   * REMOVED BUSBAR_WORKER_THREADS=<core-count>: that was worker-scaling PERF TUNING, forbidden by
#     the standard. v1.4.1's default (main.rs, worker_threads_from_env → available_parallelism) is
#     already one worker per available core, and available_parallelism respects the --cpuset-cpus
#     pin, so removing the override yields the SAME thread count via the default — no behavior change,
#     just no favoritism. This was the one clear special-deal knob; it is gone.
#   * emit_server_timing: true — KEPT. busbar's default is FALSE (config/mod.rs DEFAULT_EMIT_SERVER_
#     TIMING=false, an indistinguishability choice). Turning it ON only ADDS a Server-Timing response
#     header — it discloses busbar to the client and costs it nothing/slightly-hurts; it can never be
#     favoritism. Left on for transparency.
#   * max_concurrent: 8000 — KEPT. In v1.4.1 ModelCfg.max_concurrent is `usize` with NO serde default
#     (config/mod.rs) — a REQUIRED field; omitting it fails config load. 8000 is a neutral high
#     ceiling (well above the sweep's winning band), i.e. not the limiter — the forced-name equivalent
#     of every other gateway's unbounded default, not a tuned throttle. (Later busbar makes this
#     Option/unbounded; on 1.4.1 a value must be named.)
#   * auth.chain:[tokens] + client_tokens — KEPT. busbar's DATA-PLANE auth default is OPEN (empty
#     chain = open relay, config/mod.rs). Keeping client-token auth is therefore ADDING auth in the
#     SAFE direction (busbar carries the per-request auth cost its competitors don't), never a
#     favorable strip. The task specifies busbar keeps its client token; done.
#   * No default feature is disabled: /metrics, /stats, /healthz and the admin plane are mounted
#     unconditionally in 1.4.1 (no config flag gates them); the circuit breaker is opt-in per pool
#     (omitting a breaker: block is the genuine default, not a strip); request-log webhook / OTLP
#     default to None (omitting = default). Nothing here turns anything off.
GW_KIND=docker
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

BUSBAR_IMAGE="${BUSBAR_IMAGE:-getbusbar/busbar:1.4.1}"

gw_build() {
  sudo docker pull "$BUSBAR_IMAGE" >/dev/null 2>&1 || true
}

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$BUSBAR_IMAGE" 2>/dev/null)
  echo "${BUSBAR_IMAGE}${dg:+ (@${dg##*@})}"
}

# ── matrix suite: full 6x6 egress support ─────────────────────────────────────────────────────────
# Busbar's provider config is protocol-first: `protocol: <dialect>` + `base_url` per provider, and all
# six upstream dialects are wired IN THE SINGLE OOTB CONFIG (not per-lane). The matrix runner calls
# gw_matrix_egress <dialect> per column; it just selects the model backed by that protocol's provider.
# Declared capability: busbar claims ALL 36 (rows=ingress, cols=egress) - it accepts every ingress
# dialect and translates to every upstream dialect. Every cell is probed for a real pass/fail.
# The six protocol wire-strings (proto/mod.rs): openai, responses, anthropic, gemini, cohere, bedrock.
# NOTE the OpenAI-Responses wire value is `responses` (NOT `openai-responses`).
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

# _busbar_run: the single docker launch every lane shares. The image's entrypoint is /busbar and its
# baked-in defaults already point BUSBAR_CONFIG/BUSBAR_PROVIDERS at /etc/busbar/{config,providers}.yaml
# — the generated configs are mounted read-only onto exactly those paths. Host network keeps
# 127.0.0.1:$MOCK_PORT/$GW_PORT semantics identical to a native launch; --cpuset-cpus is the core pin.
# BENCH_MOCK_KEY carries a bearer for the body-model protocols; BENCH_BEDROCK_KEY carries the bedrock
# SigV4 ACCESS:SECRET pair (the mock ignores both). Worker threads are NOT pinned — busbar defaults to
# one-per-core via available_parallelism, which already honors --cpuset-cpus (fairness: no scaling
# knob). BUSBAR_STATE_FILE= disables state persistence for a stateless deterministic run.
_busbar_run() {
  sudo docker rm -f busbar-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name busbar-bench --network host --cpuset-cpus="$CORES" \
    -e BUSBAR_STATE_FILE= \
    -e BENCH_MOCK_KEY=dummy \
    -e BENCH_BEDROCK_KEY="AKIAMOCKACCESSKEY:mock-secret-access-key" \
    -v "$GW_DIR/config.gen.yaml:/etc/busbar/config.yaml:ro" \
    -v "$GW_DIR/providers.gen.yaml:/etc/busbar/providers.yaml:ro" \
    "$BUSBAR_IMAGE" >"$GW_DIR/launch.log" 2>&1 || true
}

# _busbar_write_config: render the ONE OOTB config pair. providers.gen.yaml declares all six protocol
# providers (each base_url → mock); config.gen.yaml declares one model per provider + a pool per model,
# with the client-token auth chain. Used verbatim for perf/memory AND every matrix column.
_busbar_write_config() {
  cat > "$GW_DIR/providers.gen.yaml" <<YAML
mock-openai:
  protocol: openai
  base_url: http://127.0.0.1:$MOCK_PORT
  api_key_env: BENCH_MOCK_KEY
  error_map: {}
mock-responses:
  protocol: responses
  base_url: http://127.0.0.1:$MOCK_PORT
  api_key_env: BENCH_MOCK_KEY
  error_map: {}
mock-anthropic:
  protocol: anthropic
  base_url: http://127.0.0.1:$MOCK_PORT
  api_key_env: BENCH_MOCK_KEY
  error_map: {}
mock-gemini:
  protocol: gemini
  base_url: http://127.0.0.1:$MOCK_PORT
  api_key_env: BENCH_MOCK_KEY
  error_map: {}
mock-cohere:
  protocol: cohere
  base_url: http://127.0.0.1:$MOCK_PORT
  api_key_env: BENCH_MOCK_KEY
  error_map: {}
mock-bedrock:
  protocol: bedrock
  base_url: http://127.0.0.1:$MOCK_PORT
  api_key_env: BENCH_BEDROCK_KEY
  error_map: {}
YAML
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
  mock-openai:
    api_key_env: BENCH_MOCK_KEY
  mock-responses:
    api_key_env: BENCH_MOCK_KEY
  mock-anthropic:
    api_key_env: BENCH_MOCK_KEY
  mock-gemini:
    api_key_env: BENCH_MOCK_KEY
  mock-cohere:
    api_key_env: BENCH_MOCK_KEY
  mock-bedrock:
    api_key_env: BENCH_BEDROCK_KEY
models:
  gpt-4o-mini:
    # OOTB canonical (OpenAI) lane — the perf/memory/throughput default. max_concurrent is a REQUIRED
    # field in v1.4.1 (no serde default); 8000 sits above the sweep's winning band so it is a ceiling,
    # not the limiter — the forced-name equivalent of an unbounded default, not a tuned advantage.
    # max_requests: -1 = unmetered.
    provider: mock-openai
    max_concurrent: 8000
    max_requests: -1
  gpt-4o-mini-responses:
    provider: mock-responses
    max_concurrent: 8000
    max_requests: -1
  gpt-4o-mini-anthropic:
    provider: mock-anthropic
    max_concurrent: 8000
    max_requests: -1
  gpt-4o-mini-gemini:
    provider: mock-gemini
    max_concurrent: 8000
    max_requests: -1
  gpt-4o-mini-cohere:
    provider: mock-cohere
    max_concurrent: 8000
    max_requests: -1
  gpt-4o-mini-bedrock:
    provider: mock-bedrock
    max_concurrent: 8000
    max_requests: -1
pools:
  bench-pool:
    members:
      - target: gpt-4o-mini
YAML
}

# Each matrix column selects the model backed by that dialect's provider; the client keeps sending the
# name below. openai keeps the canonical $GW_MODEL (what perf runs). No config rewrite/relaunch needed
# beyond re-rendering the same all-providers config, so the artifact stays identical to perf/memory.
gw_matrix_egress() {
  case "$1" in
    openai)           GW_MODEL=gpt-4o-mini;;
    openai-responses) GW_MODEL=gpt-4o-mini-responses;;
    anthropic)        GW_MODEL=gpt-4o-mini-anthropic;;
    gemini)           GW_MODEL=gpt-4o-mini-gemini;;
    cohere)           GW_MODEL=gpt-4o-mini-cohere;;
    bedrock)          GW_MODEL=gpt-4o-mini-bedrock;;
    *) return 1;;
  esac
  _busbar_write_config
  _busbar_run
}

gw_launch() {
  _busbar_write_config
  _busbar_run
}

# ── OOTB config artifact (file-driven) ────────────────────────────────────────────────────────────
# gw_config prints the canonical OOTB config busbar launches with. busbar is file-driven, so the
# artifact is the rendered providers.yaml + config.yaml pair (exactly what /etc/busbar/{providers,
# config}.yaml load) PLUS the non-secret launch env (signing values are dummy on the isolated rig —
# never a live key). Read from the files _busbar_write_config just rendered (falls back to rendering
# them if absent), so it can never drift from what busbar loaded. OOTB posture: all six protocols
# wired to the mock, client-token auth kept (busbar's added-in-the-safe-direction posture),
# emit_server_timing on (anti-favoritism disclosure), no worker-thread pin (default one-per-core), no
# feature disabled; the only run-mechanic is BUSBAR_STATE_FILE= (stateless).
gw_config() {
  [ -f "$GW_DIR/providers.gen.yaml" ] && [ -f "$GW_DIR/config.gen.yaml" ] || _busbar_write_config
  echo "# ── providers.yaml (rendered; mounted at /etc/busbar/providers.yaml) ──"
  cat "$GW_DIR/providers.gen.yaml"
  echo
  echo "# ── config.yaml (rendered; mounted at /etc/busbar/config.yaml) ──"
  cat "$GW_DIR/config.gen.yaml"
  echo
  echo "# ── launch env (non-secret; signing values are dummy on the isolated rig) ──"
  cat <<ENV
BUSBAR_STATE_FILE=
BENCH_MOCK_KEY=dummy
BENCH_BEDROCK_KEY=AKIAMOCKACCESSKEY:mock-secret-access-key
ENV
}

gw_rss() { container_rss_mib busbar-bench; }  # summed process-tree VmRSS (same method as native)
gw_hwm() { container_hwm_mib busbar-bench; }  # summed process-tree VmHWM (kernel high-water mark)

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=busbar-bench --format '{{.Status}}' 2>/dev/null)"
  echo "run.log: $(cat "$GW_DIR/launch.log" 2>/dev/null | tr '\n' ' ' | head -c 300)"
  echo "logs:"; sudo docker logs --tail 25 busbar-bench 2>&1
}

gw_stop() { sudo docker rm -f busbar-bench >/dev/null 2>&1; }

# ── governed lane (governed/run.sh) ────────────────────────────────────────────────────────────────
# Busbar's governance layer is always compiled in but INERT until governance.admin_token is set.
# Setting it activates enforcement: static auth tokens are superseded and EVERY inference request
# must resolve to a busbar-minted virtual key (per-request key resolution + RPM/TPM accounting +
# an atomic budget check-and-charge on the hot path). gw_governed_launch writes the same OOTB config
# as gw_launch plus the governance block, boots busbar, then mints one virtual key over the admin API
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
  # Start from the same OOTB provider set + config, then append the governance block. Re-render first
  # so the governed lane uses the identical all-providers OOTB config as every other lane.
  _busbar_write_config
  cat >> "$GW_DIR/config.gen.yaml" <<YAML
admin_listen: "127.0.0.1:$BUSBAR_ADMIN_PORT"
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
YAML
  _busbar_run
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
