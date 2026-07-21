#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: LiteLLM Python proxy (the shipping `litellm[proxy]` CLI).
GW_KIND=native
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
  setsid taskset -c "$CORES" env LITELLM_MASTER_KEY="$GW_AUTH" \
    "$LP_VENV/bin/litellm" --config "$GW_DIR/config.gen.yaml" --port "$GW_PORT" \
    </dev/null >/tmp/litellm_py.mem.log 2>&1 &
}

# The proxy is uvicorn workers; sum RSS across the litellm process group for an honest peak.
gw_rss() {
  local total=0 kb
  for p in $(pgrep -f "litellm.*--port $GW_PORT"); do
    kb=$(awk '/VmRSS/{print $2}' "/proc/$p/status" 2>/dev/null); total=$((total + ${kb:-0}))
  done
  awk -v k="$total" 'BEGIN{printf "%.1f", k/1024}'
}

gw_stop() { pkill -f "litellm.*--port $GW_PORT" 2>/dev/null; }
