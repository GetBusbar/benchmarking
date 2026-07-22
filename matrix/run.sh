#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# PROTOCOL SUPPORT MATRIX — pluggable across gateways (same gateways/<name>/gateway.sh manifests as
# perf/memory/stream/xlate). ONE gateway is probed across SIX ingress protocol shapes while the
# upstream mock stays fixed on the OPENAI shape at the manifest's GW_PATH — so every non-openai cell
# forces the gateway to translate the request out and the response back. The mock is untouched; that
# is the point. This is a CAPABILITY suite, not a latency suite: one probe per cell, envelope
# validation (not just the status code), served true/false per cell with evidence, valid JSON always,
# exit 0 always. (v1 records no per-cell latency: the load generator only speaks the openai and
# anthropic shapes, so a fair c1 number for the other four cells needs loadgen work first.)
#
# The six ingress cells (client speaks X → gateway → openai upstream):
#   openai            POST /v1/chat/completions        verdict: choices[0].message present
#   openai-responses  POST /v1/responses               verdict: Responses envelope (output / output_text / type=response)
#   anthropic         POST /v1/messages                verdict: "type":"message" + content array (same probe as xlate/)
#   gemini            POST /v1beta/models/{m}:generateContent   verdict: candidates[0].content present
#   cohere            POST /v2/chat (fallback /v1/chat)         verdict: message.content or text (v2 chat shape)
#   bedrock           POST /model/{m}/converse         verdict: output.message.content (Converse shape)
#
# PASSTHROUGH GUARD (generalizes xlate's msg_x check): the mock answers ALL six protocols by path, so
# a gateway that merely proxies /v1/messages (or /v2/chat, or …) verbatim gets a plausible-looking
# body back — from the MOCK's canned constant, not from a translation of the openai upstream. Every
# non-openai cell therefore rejects the mock's own canned body (byte-identical match, plus the canned
# ids resp_x / msg_x where they exist) as UNTRANSLATED passthrough → served=false with the evidence.
# The openai cell is exempt: the upstream IS openai, so a straight proxy is the correct behavior.
#
# BEDROCK AUTH HONESTY: real Bedrock SDK clients sign with AWS SigV4, and gateways differ in whether
# they also accept a bearer-style token on that ingress (busbar does: its auth chain reads
# `Authorization: Bearer` first and only enters SigV4 verification when the request actually carries
# an AWS4-HMAC-SHA256 header). The probe sends the bearer token; if the gateway answers 401/403 —
# i.e. it insists on a signature this harness does not forge — the cell records
# served="unprobed_auth" (distinct from false) with the evidence. Honesty over a false red.
#
#   GATEWAY=busbar BUSBAR_BIN=~/busbar matrix/run.sh
#   GATEWAY=mock-gateway matrix/run.sh     # graceful-path fixture: a second mock posing as the gateway
#
# Manifest overrides (all optional): GW_MATRIX_PATH_OPENAI, GW_MATRIX_PATH_RESPONSES,
# GW_MATRIX_PATH_ANTHROPIC (defaults to GW_ANTHROPIC_PATH, i.e. shared with xlate/),
# GW_MATRIX_PATH_GEMINI, GW_MATRIX_PATH_COHERE, GW_MATRIX_PATH_BEDROCK, and
# GW_ANTHROPIC_AUTH_HEADER (anthropic cell only, same as xlate/). Results: results/matrix/<gateway>.json.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
GATEWAY="${GATEWAY:-busbar}"
# Manifests live in gateways/<name>/ as everywhere else; matrix/mock-gateway/ is the one extra
# fixture (kept OUT of gateways/ on purpose so run-all discovery never fields it as a contender).
if [ -f "$ROOT/gateways/$GATEWAY/gateway.sh" ]; then export GW_DIR="$ROOT/gateways/$GATEWAY"
elif [ -f "$HERE/$GATEWAY/gateway.sh" ]; then export GW_DIR="$HERE/$GATEWAY"
else echo "unknown gateway '$GATEWAY'"; exit 2; fi

ulimit -n 1048576 2>/dev/null || ulimit -n 65536 2>/dev/null || true
export CORES="${CORES:-0-3}"; MOCKCORES="${MOCKCORES:-0-3}"
export MOCK_PORT="${MOCK_PORT:-8000}"
RESULTS="$ROOT/results/matrix"; mkdir -p "$RESULTS"
log(){ echo "[$(date +%H:%M:%S)] $*"; }
command -v taskset >/dev/null || taskset(){ shift 2; "$@"; }
command -v setsid  >/dev/null || setsid(){ "$@"; }
command -v cargo >/dev/null || { echo "need cargo (rust mock)"; exit 1; }

log "building mock (rust)"
( cd "$ROOT/mock" && cargo build --release >/dev/null 2>&1 ) || { echo "mock build failed"; exit 1; }
MOCK="$ROOT/mock/target/release/mock"

[ -f "$ROOT/gateways/versions.env" ] && source "$ROOT/gateways/versions.env"
gw_version(){ echo unknown; }; GW_HEADERS=()
gw_diag(){ :; }
json_escape(){ printf '%s' "$1" | tr -d '\000' | head -c 1600 \
  | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read())[1:-1])' 2>/dev/null \
  || printf '%s' "$1" | tr '\n\t"\\' '    ' | head -c 1600; }
# shellcheck source=/dev/null
source "$GW_DIR/gateway.sh"

# Per-cell ingress paths — manifest override wins, else the protocol's canonical default. The
# anthropic cell reuses xlate's GW_ANTHROPIC_PATH so a manifest wired for xlate needs nothing new.
P_OPENAI="${GW_MATRIX_PATH_OPENAI:-$GW_PATH}"
P_RESPONSES="${GW_MATRIX_PATH_RESPONSES:-/v1/responses}"
P_ANTHROPIC="${GW_MATRIX_PATH_ANTHROPIC:-${GW_ANTHROPIC_PATH:-/v1/messages}}"
P_GEMINI="${GW_MATRIX_PATH_GEMINI:-/v1beta/models/$GW_MODEL:generateContent}"
P_COHERE="${GW_MATRIX_PATH_COHERE:-/v2/chat}"
P_COHERE_FB="/v1/chat"
P_BEDROCK="${GW_MATRIX_PATH_BEDROCK:-/model/$GW_MODEL/converse}"

log "starting mock :$MOCK_PORT (instant, openai upstream on $GW_PATH)"
pkill -f "$MOCK" 2>/dev/null; sleep 1
setsid taskset -c "$MOCKCORES" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
sleep 1
cleanup(){ gw_stop 2>/dev/null; pkill -f "$MOCK" 2>/dev/null; }
trap cleanup EXIT

log "[$GATEWAY] build + launch"; gw_build || { echo "build failed"; exit 1; }; gw_launch
# Header arrays built AFTER launch so a manifest can mint a key in gw_launch (busbar vkey).
CURL_H=(); XH=()
for h in "${GW_HEADERS[@]:-}"; do [ -n "$h" ] && CURL_H+=(-H "$h"); done
[ -n "${GW_ANTHROPIC_AUTH_HEADER:-}" ] && XH+=(-H "$GW_ANTHROPIC_AUTH_HEADER")

log "[$GATEWAY] wait 200 on $P_OPENAI (openai warm — is the gateway up at all?)"; ok=0; c=000
for i in $(seq 1 60); do
  c=$(curl -s -m3 -o /dev/null -w "%{http_code}" "http://127.0.0.1:$GW_PORT$P_OPENAI" -X POST \
      -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" ${CURL_H[@]+"${CURL_H[@]}"} \
      -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}")
  [ "$c" = 200 ] && { ok=1; break; }; sleep 1
done
SERVE_ERR=""
[ "$ok" != 1 ] && { SERVE_ERR="HTTP $c on POST $P_OPENAI; diag=[$(gw_diag 2>&1 | tail -n 20)]"; \
  log "[$GATEWAY] WARNING never got 200 (last=$c) — every cell will carry that evidence"; }

# ── one probe per cell: POST, capture status + body, verdict on the ENVELOPE ─────────────────────
LAST_STATUS=000; LAST_BODY=""
probe(){ # path body extra-header...
  local path="$1" data="$2"; shift 2
  local out
  out="$(curl -s -m5 -w '\n%{http_code}' "http://127.0.0.1:$GW_PORT$path" -X POST \
      -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" \
      ${CURL_H[@]+"${CURL_H[@]}"} "$@" -d "$data" 2>&1)"
  LAST_STATUS="${out##*$'\n'}"; LAST_BODY="${out%$'\n'*}"
}

# Envelope verdicts + passthrough guard, in one place (python: nested-field checks beat grep here).
# Body rides in $MATRIX_BODY (python reads its program from stdin, so stdin is taken); argv = cell
# name; prints one line: "ok", "passthrough <why>", or "bad <why>".
verdict(){ # cell  (body in $MATRIX_BODY)
  MATRIX_BODY="$LAST_BODY" python3 - "$1" <<'PY'
import json, os, sys
cell = sys.argv[1]; raw = os.environ.get("MATRIX_BODY", "")
# The mock's canned constants (mock/src/main.rs). A gateway that proxied the ingress path verbatim
# hands the client one of these BYTE-IDENTICAL — a translating gateway reserializes and never does.
CANNED = {
 "openai-responses": '{"id":"resp_x","object":"response","created_at":1,"status":"completed","model":"mock","output":[{"type":"message","id":"msg_x","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}',
 "anthropic": '{"id":"msg_x","type":"message","role":"assistant","model":"mock","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2}}',
 "gemini": '{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2,"totalTokenCount":12}}',
 "bedrock": '{"output":{"message":{"role":"assistant","content":[{"text":"ok"}]}},"stopReason":"end_turn","usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}',
 "cohere": '{"id":"x","finish_reason":"COMPLETE","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]},"usage":{"tokens":{"input_tokens":10,"output_tokens":2}}}',
}
body = raw.strip()
if cell != "openai":  # openai upstream: a straight proxy IS the served behavior for that cell
    if body == CANNED.get(cell, "\0"):
        print("passthrough byte-identical to the mock's canned %s body" % cell); sys.exit(0)
    if cell == "anthropic" and '"id":"msg_x"' in body:
        print("passthrough mock canned /messages body (id msg_x)"); sys.exit(0)
    if cell == "openai-responses" and '"id":"resp_x"' in body:
        print("passthrough mock canned /responses body (id resp_x)"); sys.exit(0)
try:
    j = json.loads(body)
except Exception:
    print("bad not JSON"); sys.exit(0)
def has(d, *ks):
    for k in ks:
        if isinstance(d, dict) and k in d: d = d[k]
        elif isinstance(d, list) and isinstance(k, int) and len(d) > k: d = d[k]
        else: return False
    return True
ok, why = False, ""
if cell == "openai":
    ok = has(j, "choices", 0, "message"); why = "no choices[0].message"
elif cell == "openai-responses":
    ok = ("output" in j or "output_text" in j
          or j.get("type") == "response" or j.get("object") == "response")
    why = "no Responses envelope (output/output_text/type=response)"
elif cell == "anthropic":
    ok = j.get("type") == "message" and isinstance(j.get("content"), list)
    why = 'no anthropic envelope ("type":"message" + content array)'
elif cell == "gemini":
    ok = has(j, "candidates", 0, "content"); why = "no candidates[0].content"
elif cell == "cohere":
    ok = has(j, "message", "content") or "text" in j
    why = "no cohere v2 chat envelope (message.content or text)"
elif cell == "bedrock":
    ok = has(j, "output", "message", "content"); why = "no output.message.content (Converse)"
print("ok" if ok else "bad " + why)
PY
}

CELLS_JSON=""
run_cell(){ # cell path body extra-header...
  local cell="$1" path="$2" data="$3"; shift 3
  local served=false note="" v snip
  if [ "$ok" != 1 ]; then
    LAST_STATUS="$c"; LAST_BODY=""
    note="gateway never served the openai warm-up; not probed. $SERVE_ERR"
  else
    probe "$path" "$data" "$@"
    v="$(verdict "$cell")"
    if [ "${LAST_STATUS#2}" != "$LAST_STATUS" ] && [ "$v" = ok ]; then
      served=true; note="HTTP $LAST_STATUS, $cell envelope validated"
    elif [ "$cell" = cohere ] && { [ "$LAST_STATUS" = 404 ] || [ "$LAST_STATUS" = 405 ]; }; then
      # cohere fallback: some gateways mount v1 chat only
      probe "$P_COHERE_FB" "$data" "$@"
      v="$(verdict "$cell")"
      if [ "${LAST_STATUS#2}" != "$LAST_STATUS" ] && [ "$v" = ok ]; then
        served=true; path="$P_COHERE_FB"; note="HTTP $LAST_STATUS on fallback $P_COHERE_FB, cohere envelope validated"
      else
        note="HTTP 404/405 on $path, then HTTP $LAST_STATUS on fallback $P_COHERE_FB: $v"; path="$P_COHERE_FB"
      fi
    elif [ "$cell" = bedrock ] && { [ "$LAST_STATUS" = 401 ] || [ "$LAST_STATUS" = 403 ]; }; then
      # the gateway rejected the bearer token on the bedrock ingress — it wants SigV4, which this
      # harness does not forge. Distinct verdict: not served, but not a red either.
      served='"unprobed_auth"'
      note="HTTP $LAST_STATUS with a bearer token; gateway appears to require inbound SigV4 on the bedrock ingress, which this probe does not sign"
    elif printf '%s' "$v" | grep -q '^passthrough'; then
      note="UNTRANSLATED $v — the gateway proxied $path through verbatim instead of translating (HTTP $LAST_STATUS)"
    else
      note="HTTP $LAST_STATUS on POST $path: $v"
    fi
  fi
  snip="$(printf '%s' "$LAST_BODY" | head -c 200)"
  log "[$GATEWAY]   $cell → served=$served ($note)"
  CELLS_JSON="${CELLS_JSON}${CELLS_JSON:+,}
    \"$cell\": {\"served\": $served, \"status\": \"$LAST_STATUS\", \"path\": \"$path\", \"verdict_note\": \"$(json_escape "$note")\", \"body_snippet\": \"$(json_escape "$snip")\"}"
}

log "[$GATEWAY] probing the six ingress cells (upstream fixed: openai)"
run_cell openai "$P_OPENAI" \
  "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}],\"max_tokens\":16}"
run_cell openai-responses "$P_RESPONSES" \
  "{\"model\":\"$GW_MODEL\",\"input\":\"hello\"}"
run_cell anthropic "$P_ANTHROPIC" \
  "{\"model\":\"$GW_MODEL\",\"max_tokens\":64,\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}" \
  -H "anthropic-version: 2023-06-01" -H "x-api-key: $GW_AUTH" ${XH[@]+"${XH[@]}"}
run_cell gemini "$P_GEMINI" \
  '{"contents":[{"parts":[{"text":"hello"}]}]}' \
  -H "x-goog-api-key: $GW_AUTH"
run_cell cohere "$P_COHERE" \
  "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}"
run_cell bedrock "$P_BEDROCK" \
  '{"messages":[{"role":"user","content":[{"text":"hello"}]}]}'

BUILD="$(gw_version 2>/dev/null | tr -d '\n' | sed 's/"/\\"/g')"
cat > "$RESULTS/$GATEWAY.json" <<JSON
{
  "gateway": "$GATEWAY",
  "build": "$BUILD",
  "served": $([ "$ok" = 1 ] && echo true || echo false),
  "serve_error": "$(json_escape "$SERVE_ERR")",
  "upstream_shape": "openai",
  "upstream_note": "v1 fixes the upstream to the OpenAI shape on $GW_PATH; every non-openai cell is a translation claim. The full 6x6 (all upstream dialects) is future work.",
  "cells": {$CELLS_JSON
  },
  "model": "$GW_MODEL",
  "upstream_endpoint": "$GW_PATH",
  "arch": "${BENCH_ARCH:-$(uname -m)}",
  "hardware": "${BENCH_HARDWARE:-$(uname -m) $(nproc 2>/dev/null || echo '?')vCPU}",
  "measured_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON
echo "================================================================"
echo " gateway=$GATEWAY   protocol support matrix (ingress → openai upstream)"
python3 - "$RESULTS/$GATEWAY.json" <<'PY'
import json, sys
j = json.load(open(sys.argv[1]))
for cell, r in j["cells"].items():
    print("   %-17s served=%-14s status=%s" % (cell, r["served"], r["status"]))
PY
echo " -> $RESULTS/$GATEWAY.json"
echo "================================================================"
