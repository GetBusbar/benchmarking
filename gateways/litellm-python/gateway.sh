#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: LiteLLM Python proxy (the shipping `litellm[proxy]` CLI).
GW_KIND=native
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
LP_VENV="${LP_VENV:-$GW_DIR/venv}"

gw_build() {
  [ -x "$LP_VENV/bin/litellm" ] && return 0
  python3 -m venv "$LP_VENV"
  # Keep the install log — a silent pip failure is why the version once showed up as "?".
  "$LP_VENV/bin/pip" install -q --upgrade pip "${LITELLM_PY_SPEC:-litellm[proxy]}" >"$GW_DIR/pip.log" 2>&1
}

# Prefer `pip show` (works even if `import litellm` emits warnings that trip -c); fall back to import.
gw_version() {
  local v
  v=$("$LP_VENV/bin/pip" show litellm 2>/dev/null | awk '/^Version:/{print $2}')
  [ -z "$v" ] && v=$("$LP_VENV/bin/python" -c 'import litellm;print(litellm.__version__)' 2>/dev/null)
  echo "litellm==${v:-?}"
}

gw_diag() {
  echo "proc: $(pgrep -af "litellm.*--port $GW_PORT" | head -c 200)"
  echo "pip.log tail: $(tail -n 3 "$GW_DIR/pip.log" 2>/dev/null | tr '\n' ' ' | head -c 200)"
  echo "run.log:"; tail -n 20 /tmp/litellm_py.mem.log 2>/dev/null
}

_lp_spawn() {
  pkill -f "litellm.*--port $GW_PORT" 2>/dev/null; sleep 1
  # Scale uvicorn workers to the pinned core count so LiteLLM uses all 4 cores it's given, not one
  # (single-worker on a 4-core pin under-serves it — fairness M5). The gw_rss sums the whole group.
  local ncore=$(( ${CORES##*-} - ${CORES%%-*} + 1 ))
  setsid taskset -c "$CORES" env LITELLM_MASTER_KEY="$GW_AUTH" \
    "$LP_VENV/bin/litellm" --config "$GW_DIR/config.gen.yaml" --port "$GW_PORT" --num_workers "$ncore" \
    </dev/null >/tmp/litellm_py.mem.log 2>&1 &
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

# --num_workers spawns uvicorn WORKER children whose cmdlines don't contain "--port", so a pattern
# match catches only the parent (constant RSS) and misses where memory actually grows. Sum the whole
# process tree from the parent PID — the SAME method (_rss_tree_mib, in memory/run.sh) as every other
# gateway. (m11 fix: was reporting a flat idle==peak because the workers were invisible.)
gw_rss() {
  local master; master=$(pgrep -f "litellm.*--port $GW_PORT" | head -1)
  _rss_tree_mib "$master"
}
gw_hwm() {  # kernel VmHWM summed over the master + uvicorn worker tree (same tree as gw_rss)
  local master; master=$(pgrep -f "litellm.*--port $GW_PORT" | head -1)
  _hwm_tree_mib "$master"
}

gw_stop() { pkill -f "litellm.*--port $GW_PORT" 2>/dev/null; }
