#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: LiteLLM Python proxy (the shipping `litellm[proxy]` CLI).
GW_KIND=native
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="LiteLLM · Python"                      # label in charts + report tables
GW_LANG=Python                            # implementation language → bar color bucket
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

gw_launch() {
  cat > "$GW_DIR/config.gen.yaml" <<YAML
model_list:
  - model_name: $GW_MODEL
    litellm_params:
      model: openai/$GW_MODEL
      api_base: http://127.0.0.1:$MOCK_PORT/v1
      api_key: dummy
YAML
  pkill -f "litellm.*--port $GW_PORT" 2>/dev/null; sleep 1
  # Scale uvicorn workers to the pinned core count so LiteLLM uses all 4 cores it's given, not one
  # (single-worker on a 4-core pin under-serves it — fairness M5). The gw_rss sums the whole group.
  local ncore=$(( ${CORES##*-} - ${CORES%%-*} + 1 ))
  setsid taskset -c "$CORES" env LITELLM_MASTER_KEY="$GW_AUTH" \
    "$LP_VENV/bin/litellm" --config "$GW_DIR/config.gen.yaml" --port "$GW_PORT" --num_workers "$ncore" \
    </dev/null >/tmp/litellm_py.mem.log 2>&1 &
}

# --num_workers spawns uvicorn WORKER children whose cmdlines don't contain "--port", so a pattern
# match catches only the parent (constant RSS) and misses where memory actually grows. Sum the whole
# process tree from the parent PID — the SAME method (_rss_tree_mib, in memory/run.sh) as every other
# gateway. (m11 fix: was reporting a flat idle==peak because the workers were invisible.)
gw_rss() {
  local master; master=$(pgrep -f "litellm.*--port $GW_PORT" | head -1)
  _rss_tree_mib "$master"
}

gw_stop() { pkill -f "litellm.*--port $GW_PORT" 2>/dev/null; }
