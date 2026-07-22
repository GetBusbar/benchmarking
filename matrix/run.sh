#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# PROTOCOL SUPPORT MATRIX v2: the FULL 6x6. Pluggable across gateways (same gateways/<name>/gateway.sh
# manifests as perf/memory/stream/xlate). ONE gateway is probed across SIX ingress protocol shapes
# for EACH of the SIX upstream (egress) dialects it can be configured for: 36 cells per gateway.
# This is a CAPABILITY suite, not a latency suite: one probe per cell, envelope validation (not just
# the status code), valid JSON always, exit 0 always.
#
# The six dialects (both axes):
#   openai            POST /v1/chat/completions        verdict: choices[0].message present
#   openai-responses  POST /v1/responses               verdict: Responses envelope (output / output_text / type=response)
#   anthropic         POST /v1/messages                verdict: "type":"message" + content array (same probe as xlate/)
#   gemini            POST /v1beta/models/{m}:generateContent   verdict: candidates[0].content present
#   cohere            POST /v2/chat (fallback /v1/chat)         verdict: message.content or text (v2 chat shape)
#   bedrock           POST /model/{m}/converse         verdict: output.message.content (Converse shape)
#
# EGRESS HOOK (per-gateway, optional): a manifest declares which upstream dialects it can be
# configured for with GW_MATRIX_EGRESS="openai anthropic ..." (default: "openai", the classic
# launch) and, when the list has more than the default, defines
#     gw_matrix_egress <dialect>
# which reconfigures + relaunches the gateway so GW_MODEL routes to the mock upstream speaking
# <dialect>. When gw_matrix_egress is defined the runner calls it for EVERY dialect in the list
# (including openai); when it is not, only the openai column runs via the plain gw_launch. A dialect
# absent from the list renders every cell in that column "not_configurable": an honest "this
# manifest defines no way to route to that upstream shape", distinct from tried-and-failed.
#
# FAIRNESS RULE (diagonal): a cell where ingress dialect == egress dialect requires NO translation.
# Faithful passthrough is a PASS there: the verdict is (a) the response is a valid envelope of the
# dialect and (b) the request actually round-tripped through the gateway to the mock, proven by the
# mock's per-dialect request record (MOCK_RECORD=1: GET /__mock/state, POST /__mock/reset). A
# diagonal pass whose body is the mock's canned constant is annotated "passthrough (same dialect,
# no translation required)".
#
# OFF-DIAGONAL cells are translation claims, all three legs checked:
#   1. the response is a valid envelope of the INGRESS dialect;
#   2. it is NOT the mock's canned body for that ingress dialect (byte-identical, or the canned
#      sentinel ids chatcmpl-x / resp_x / msg_x in their own dialect) - the passthrough guard;
#   3. the mock RECEIVED a request on the EGRESS dialect's endpoint carrying that dialect's request
#      shape (the mock records last-request shape per endpoint; the runner resets + reads it per cell).
# Leg 3 is what makes the 6x6 honest: a gateway that answers from its own canned logic without ever
# calling the upstream fails with that evidence.
#
# BEDROCK AUTH HONESTY (ingress): real Bedrock SDK clients sign with AWS SigV4, and gateways differ
# in whether they also accept a bearer-style token on that ingress. The probe sends the bearer
# token; if the gateway answers 401/403, i.e. it insists on a signature this harness does not forge,
# the cell records served="unprobed_auth" (distinct from false) with the evidence.
#
#   GATEWAY=busbar BUSBAR_BIN=~/busbar matrix/run.sh
#   GATEWAY=mock-gateway matrix/run.sh     # graceful-path fixture: a second mock posing as the gateway
#
# Manifest overrides (all optional): GW_MATRIX_PATH_OPENAI, GW_MATRIX_PATH_RESPONSES,
# GW_MATRIX_PATH_ANTHROPIC (defaults to GW_ANTHROPIC_PATH, i.e. shared with xlate/),
# GW_MATRIX_PATH_GEMINI, GW_MATRIX_PATH_COHERE, GW_MATRIX_PATH_BEDROCK, GW_MATRIX_EGRESS,
# gw_matrix_egress, and GW_ANTHROPIC_AUTH_HEADER (anthropic cell only, same as xlate/).
# Results: results/matrix/<gateway>.json - v2 shape {upstreams:{<egress>:{cells:{<ingress>:...}}}}
# plus the v1 compat keys (top-level cells = the openai-egress row, or the first configured egress).
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
json_escape(){ printf '%s' "$1" | python3 -c 'import json,sys
d=sys.stdin.buffer.read()[:1600].decode("utf-8","replace")
sys.stdout.write(json.dumps(d)[1:-1])'; }

# ── per-cell perf sweep (ADDITIVE: capability verdicts above are untouched) ──────────────────────
# Every cell that PASSES the capability verdict as served (green) additionally gets a throughput +
# latency sweep on that exact (ingress path, egress config) pair: the SAME sweep implementation as
# perf/run.sh (lib/sweep.sh - same concurrency grids, same 20ms sustained gate p99 < 1000ms with
# < 0.1% errors, same mock-ceiling mock_bound guard), driving the cell's exact ingress-dialect
# probe body against the cell's ingress path. The added-latency baseline is direct-to-mock on the
# matching dialect endpoint, exactly as perf/xlate subtract theirs. Grey (not declared), red
# (served-but-wrong), unprobed_auth and boot-failed cells carry NO perf. MATRIX_SWEEP=0 disables.
MATRIX_SWEEP="${MATRIX_SWEEP:-1}"
if [ "$MATRIX_SWEEP" = 1 ] && ! command -v go >/dev/null; then
  echo "WARNING: no Go toolchain - per-cell perf sweep disabled (capability matrix still runs)"
  MATRIX_SWEEP=0
fi
UGEN="$ROOT/loadgen/ugen"
if [ "$MATRIX_SWEEP" = 1 ]; then
  log "building loadgen (go) for the per-cell perf sweep"
  go build -o "$UGEN" "$ROOT/loadgen/ugen.go" || { echo "WARNING: loadgen build failed - per-cell perf sweep disabled"; MATRIX_SWEEP=0; }
fi
# Same knobs + defaults as perf/run.sh so the per-cell numbers are directly comparable.
C1_DUR="${C1_DUR:-20}"; SWEEP_DUR="${SWEEP_DUR:-10}"; PSIZE="${PSIZE:-256}"
SWEEP_INSTANT="${SWEEP_INSTANT:-16 32 64 128 256 512 1024}"
SWEEP_DELAYED="${SWEEP_DELAYED:-8 32 128 256 1024 4096 8192 16384}"
SWEEP_TTFT_MS="${SWEEP_TTFT_MS:-20}"
P99_CEIL_MS="${P99_CEIL_MS:-1000}"
# Sweeping every green cell multiplies the suite's wall time (by design: that is the whole point of
# per-cell perf), so the default suite ceiling rises from harness.sh's 45 min to 4 h when sweeps are
# on. An explicit HARNESS_SUITE_CEIL_S still wins, and the ceiling still backstops a wedged gateway.
[ "$MATRIX_SWEEP" = 1 ] && HARNESS_SUITE_CEIL_S="${HARNESS_SUITE_CEIL_S:-14400}"
# shellcheck source=/dev/null
source "$ROOT/lib/harness.sh"
# shellcheck source=/dev/null
source "$ROOT/lib/sweep.sh"
# shellcheck source=/dev/null
source "$GW_DIR/gateway.sh"
suite_deadline_start
EGRESS_ALL="openai openai-responses anthropic gemini cohere bedrock"
INGRESS_ALL="openai openai-responses anthropic gemini cohere bedrock"

# ── declared capability matrix (GW_MATRIX_CAP) ──────────────────────────────────────────────────
# Every manifest declares a 6x6 (ingress x egress) matrix of 1/0: the gateway's OWN claim, per its
# project documentation, that it can translate that ingress dialect into that upstream (egress)
# dialect. This is the maintainer's declaration (populated by us from each project's docs as a
# stand-in until real maintainers PR their own). The rule the site enforces:
#   declared 1 -> we PROBE the cell -> PASS (green) or FAIL (red). A red can ONLY appear for a cell
#                 the gateway CLAIMED and then failed on the field run, so we can never be accused of
#                 unfairly failing a gateway on something it never claimed.
#   declared 0 -> NOT PROBED -> not_configurable (grey): the maintainer says "we do not do this".
# Grey is thus always a cited capability limit (GW_MATRIX_CAP_NOTE names why), never our generosity
# and never "we didn't get to it".
#
# Format: 6 whitespace-separated rows (rows = ingress in EGRESS_ALL order), each row 6 chars of 1/0
# (cols = egress in EGRESS_ALL order). Blank/unset => back-compat: derive a full column of 1s for
# every egress dialect listed in GW_MATRIX_EGRESS (the pre-cap behaviour), 0 elsewhere.
declare -A CAP
_cap_col_any=""   # space-list of egress dialects with >=1 declared-1 cell (drives which cols launch)
build_cap(){
  local rows i j r ing eg
  # normalise: strip whitespace to a flat 36-char string in row-major (ingress-major) order.
  rows="$(printf '%s' "${GW_MATRIX_CAP:-}" | tr -cd '01')"
  if [ "${#rows}" -ne 36 ]; then
    # No (or malformed) declaration: fall back to GW_MATRIX_EGRESS as a full-column claim.
    local egr="${GW_MATRIX_EGRESS:-openai}"
    i=0
    for ing in $INGRESS_ALL; do
      for eg in $EGRESS_ALL; do
        case " $egr " in *" $eg "*) CAP["$ing/$eg"]=1;; *) CAP["$ing/$eg"]=0;; esac
      done
    done
  else
    i=0
    for ing in $INGRESS_ALL; do
      j=0
      for eg in $EGRESS_ALL; do
        CAP["$ing/$eg"]="${rows:$((i*6+j)):1}"; j=$((j+1))
      done
      i=$((i+1))
    done
  fi
  # Which egress columns have any capable cell? Those (and only those) get launched + probed.
  for eg in $EGRESS_ALL; do
    for ing in $INGRESS_ALL; do
      if [ "${CAP["$ing/$eg"]}" = 1 ]; then _cap_col_any="$_cap_col_any $eg"; break; fi
    done
  done
}
cap(){ echo "${CAP["$1/$2"]:-0}"; }   # cap <ingress> <egress> -> 1|0
col_capable(){ case " $_cap_col_any " in *" $1 "*) return 0;; *) return 1;; esac; }
build_cap
# Reason shown on a declared-0 (grey) cell's tooltip; a manifest overrides it with a cited string.
GW_MATRIX_CAP_NOTE="${GW_MATRIX_CAP_NOTE:-this gateway does not declare support for this ingress/upstream dialect pair}"
# GW_MATRIX_EGRESS still drives the actual relaunch wiring; ensure every capable column is launchable.
GW_MATRIX_EGRESS="${GW_MATRIX_EGRESS:-openai}"

# Per-cell ingress paths: manifest override wins, else the protocol's canonical default. The
# anthropic cell reuses xlate's GW_ANTHROPIC_PATH so a manifest wired for xlate needs nothing new.
P_OPENAI="${GW_MATRIX_PATH_OPENAI:-$GW_PATH}"
P_RESPONSES="${GW_MATRIX_PATH_RESPONSES:-/v1/responses}"
P_ANTHROPIC="${GW_MATRIX_PATH_ANTHROPIC:-${GW_ANTHROPIC_PATH:-/v1/messages}}"
P_GEMINI="${GW_MATRIX_PATH_GEMINI:-/v1beta/models/$GW_MODEL:generateContent}"
P_COHERE="${GW_MATRIX_PATH_COHERE:-/v2/chat}"
P_COHERE_FB="/v1/chat"
P_BEDROCK="${GW_MATRIX_PATH_BEDROCK:-/model/$GW_MODEL/converse}"
ingress_path(){ case "$1" in
  openai) echo "$P_OPENAI";; openai-responses) echo "$P_RESPONSES";;
  anthropic) echo "$P_ANTHROPIC";; gemini) echo "$P_GEMINI";;
  cohere) echo "$P_COHERE";; bedrock) echo "$P_BEDROCK";; esac; }

# The capability probes need the RECORDING mock (instant, MOCK_RECORD=1: leg 3 evidence); the
# per-cell perf sweep needs the exact serving conditions perf/run.sh measures under (no recording,
# run_sweep restarts it per delay). These helpers flip between the two; every capability probe is
# preceded by mock_start_record, so a sweep can never leave a non-recording mock in front of a
# leg-3 check (mock_hit would honestly report "norecord" even if one slipped through).
mock_start_record(){
  pkill -f "$MOCK" 2>/dev/null; sleep 1
  setsid taskset -c "$MOCKCORES" env MOCK_RECORD=1 "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
  local i
  for i in $(seq 1 15); do
    curl -s -m2 "http://127.0.0.1:$MOCK_PORT/__mock/state" 2>/dev/null | grep -q '"recording":true' && return 0
    sleep 1
  done
  log "WARNING: recording mock did not come back on :$MOCK_PORT"
  return 1
}
mock_start_plain(){ # instant, NO recording: the identical mock perf/run.sh measures c1 against
  pkill -f "$MOCK" 2>/dev/null; sleep 1
  setsid taskset -c "$MOCKCORES" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
  sleep 1
}
log "starting mock :$MOCK_PORT (instant, all six dialects by path, request recording ON)"
mock_start_record
cleanup(){ gw_stop 2>/dev/null; pkill -f "$MOCK" 2>/dev/null; }
trap cleanup EXIT

log "[$GATEWAY] build"; gw_build || { echo "build failed"; exit 1; }
BUILD="$(gw_version 2>/dev/null | tr -d '\n' | sed 's/"/\\"/g')"

# Header arrays are rebuilt after EVERY (re)launch: a manifest can mint a key in gw_launch (busbar
# vkey) or swap provider-selecting headers per egress (portkey style).
CURL_H=(); XH=()
rebuild_headers(){
  CURL_H=(); XH=()
  local h
  for h in "${GW_HEADERS[@]:-}"; do [ -n "$h" ] && CURL_H+=(-H "$h"); done
  [ -n "${GW_ANTHROPIC_AUTH_HEADER:-}" ] && XH+=(-H "$GW_ANTHROPIC_AUTH_HEADER")
}

# ── mock record helpers: reset before each cell, read the egress-dialect record after the probe ──
mock_reset(){ curl -s -m3 -X POST "http://127.0.0.1:$MOCK_PORT/__mock/reset" >/dev/null 2>&1; }
mock_hit(){ # egress-dialect -> "ok" | "badshape <snippet>" | "miss"
  # State rides in an env var: python reads its program from stdin (heredoc), so stdin is taken,
  # exactly the same constraint verdict() documents for MATRIX_BODY.
  MOCK_STATE_JSON="$(curl -s -m3 "http://127.0.0.1:$MOCK_PORT/__mock/state" 2>/dev/null)" \
  python3 - "$1" <<'PY'
import json, os, sys
try:
    s = json.loads(os.environ.get("MOCK_STATE_JSON", ""))
except Exception:
    print("norecord"); sys.exit(0)
if not s.get("recording"):
    # A mock without MOCK_RECORD=1 answered on our port: some other run replaced our mock. Fail
    # loudly rather than reporting a false "the gateway never called the upstream".
    print("norecord"); sys.exit(0)
ds = s.get("dialects", {})
d = ds.get(sys.argv[1], {})
if not d.get("count"):
    hit = [k for k, v in ds.items() if v.get("count")]
    # Distinguish "never called the upstream at all" from "called it, but on a DIFFERENT dialect's
    # endpoint" (e.g. a gateway that forwards Responses ingress to the upstream Responses endpoint
    # even when configured for the chat-completions dialect): both fail this cell, but the evidence
    # differs and the note should say what actually happened.
    print("misdialect " + ",".join(hit) if hit else "miss")
elif d.get("body_ok"):
    print("ok")
else:
    print("badshape " + d.get("last_snippet", "")[:160].replace("\n", " "))
PY
}

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
# name + egress dialect; prints one line: "ok", "ok passthrough" (diagonal, canned body),
# "passthrough <why>" (off-diagonal guard tripped), or "bad <why>".
verdict(){ # cell egress  (body in $MATRIX_BODY)
  MATRIX_BODY="$LAST_BODY" python3 - "$1" "$2" <<'PY'
import json, os, sys
cell, egress = sys.argv[1], sys.argv[2]
raw = os.environ.get("MATRIX_BODY", "")
# The mock's canned constants (mock/src/main.rs). A gateway that proxied the ingress path verbatim
# hands the client one of these BYTE-IDENTICAL; a translating gateway reserializes and never does.
CANNED = {
 "openai": '{"id":"chatcmpl-x","object":"chat.completion","created":1,"model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}',
 "openai-responses": '{"id":"resp_x","object":"response","created_at":1,"status":"completed","model":"mock","output":[{"type":"message","id":"msg_x","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}',
 "anthropic": '{"id":"msg_x","type":"message","role":"assistant","model":"mock","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2}}',
 "gemini": '{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2,"totalTokenCount":12}}',
 "bedrock": '{"output":{"message":{"role":"assistant","content":[{"text":"ok"}]}},"stopReason":"end_turn","usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}',
 "cohere": '{"id":"x","finish_reason":"COMPLETE","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]},"usage":{"tokens":{"input_tokens":10,"output_tokens":2}}}',
}
body = raw.strip()
canned_own = body == CANNED.get(cell, "\0")
sentinel_own = ((cell == "anthropic" and '"id":"msg_x"' in body)
                or (cell == "openai-responses" and '"id":"resp_x"' in body)
                or (cell == "openai" and '"id":"chatcmpl-x"' in body))
if cell != egress:
    # Off-diagonal: this cell CLAIMS a translation, so the mock's own canned ingress-dialect body
    # (or its sentinel id, which no translation of a different upstream body would carry) is proof
    # of untranslated proxying.
    if canned_own:
        print("passthrough byte-identical to the mock's canned %s body" % cell); sys.exit(0)
    if sentinel_own:
        print("passthrough mock canned %s body (sentinel id present)" % cell); sys.exit(0)
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
if ok and cell == egress and (canned_own or sentinel_own):
    print("ok passthrough")
elif ok:
    print("ok")
else:
    print("bad " + why)
PY
}

ingress_body(){ # dialect -> probe body on stdout
  case "$1" in
    openai) echo "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}],\"max_tokens\":16}";;
    openai-responses) echo "{\"model\":\"$GW_MODEL\",\"input\":\"hello\"}";;
    anthropic) echo "{\"model\":\"$GW_MODEL\",\"max_tokens\":64,\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}";;
    gemini) echo '{"contents":[{"parts":[{"text":"hello"}]}]}';;
    cohere) echo "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}";;
    bedrock) echo '{"messages":[{"role":"user","content":[{"text":"hello"}]}]}';;
  esac
}

# ── per-cell perf sweep (green cells only) ──────────────────────────────────────────────────────
# The direct-to-mock baseline endpoint for each ingress dialect: the mock speaks every dialect by
# path, so the added-latency subtraction is direct-on-the-SAME-shape, exactly as perf (openai) and
# xlate (anthropic) compute theirs. Canonical mock paths, independent of any per-gateway ingress
# path override (the mock routes by these markers; a manifest's custom gateway path may not).
mock_direct_path(){ case "$1" in
  openai) echo "/v1/chat/completions";; openai-responses) echo "/v1/responses";;
  anthropic) echo "/v1/messages";; gemini) echo "/v1beta/models/$GW_MODEL:generateContent";;
  cohere) echo "/v2/chat";; bedrock) echo "/model/$GW_MODEL/converse";; esac; }

# matrix_cell_perf <egress> <cell> <path> <body> [extra -H header...]
# Runs the shared c1 added-latency measurement + BOTH throughput sweeps (lib/sweep.sh, the same
# implementation perf/run.sh runs) on one green cell: the cell's exact probe body at load, against
# the cell's ingress path, under the egress config the gateway is CURRENTLY launched with. Sets
# CELL_PERF_JSON (empty on skip); emit_cell folds it into the cell object. The mock is switched to
# the plain non-recording build for the measurement (perf's exact serving conditions) and the
# recording mock is restored before the next capability probe.
CELL_PERF_JSON=""
matrix_cell_perf(){
  local egress="$1" cell="$2" path="$3" data="$4"; shift 4
  CELL_PERF_JSON=""
  [ "$MATRIX_SWEEP" = 1 ] || return 0
  if suite_deadline_expired; then
    log "[$GATEWAY]   $egress <- $cell : suite ceiling reached - skipping the perf sweep (capability verdict stands)"
    return 0
  fi
  log "[$GATEWAY]   $egress <- $cell : green - measuring per-cell perf (c1 added latency + 2 sweeps)"
  # Loadgen headers = the manifest's GW_HEADERS (already -H pairs in CURL_H) + this cell's dialect
  # headers (anthropic-version/x-api-key, x-goog-api-key, ...), matching the capability probe.
  UGEN_H=( ${CURL_H[@]+"${CURL_H[@]}"} "$@" )
  SWEEP_BODY="$data"
  GURL="http://127.0.0.1:$GW_PORT$path"
  DURL="http://127.0.0.1:$MOCK_PORT$(mock_direct_path "$cell")"
  mock_start_plain
  sweep_c1
  run_sweep 0 "$SWEEP_INSTANT"
  local prps=$SW_CEIL_RPS pbound=$SW_BOUND
  run_sweep "$SWEEP_TTFT_MS" "$SWEEP_DELAYED"
  local lrps=$SW_CEIL_RPS lbound=$SW_BOUND
  SWEEP_BODY=""; UGEN_H=()
  mock_start_record
  # The sweep restarted the mock, so the gateway's upstream connection pool may hold dead sockets.
  # Fire a couple of discarded warm requests through the gateway so the re-verify probe below (and
  # the NEXT cell's capability probe) can never eat a stale-connection failure. Their record entries
  # are wiped by the mock_reset that immediately follows.
  local _w
  for _w in 1 2; do
    curl -s -m3 -o /dev/null "http://127.0.0.1:$GW_PORT$P_OPENAI" -X POST \
      -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" ${CURL_H[@]+"${CURL_H[@]}"} \
      -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}" 2>/dev/null
    sleep 1
  done
  # C1b - LEG-3 RE-VERIFY AFTER LOAD. The capability probe proved this cell hits the intended egress
  # dialect endpoint at concurrency 1, but perf/stream historically recorded HTTP-200-only numbers
  # that hid a misroute (the gomodel-class bug: an openai request served from the mock's anthropic
  # endpoint). The sweep just hammered the gateway on a NON-recording mock, so we re-run THIS cell's
  # exact probe (same body + headers, same ingress path) against the recording mock and re-assert
  # leg 3: the mock must have received a request on the $egress endpoint carrying the $egress shape.
  # If the gateway misroutes under (or after) load, perf is DROPPED for this cell with the evidence -
  # a misrouted cell never carries a green perf number.
  mock_reset
  probe "$path" "$data" "$@"
  local reverify; reverify="$(mock_hit "$egress")"
  if [ "$reverify" != ok ]; then
    log "[$GATEWAY]   $egress <- $cell : LEG-3 RE-VERIFY FAILED after load ($reverify) - dropping perf (possible misroute)"
    CELL_PERF_JSON=", \"perf_dropped\": \"$(json_escape "leg-3 re-verify after load did not confirm the $egress egress endpoint: $reverify; perf withheld to avoid recording a misrouted number")\""
    return 0
  fi
  local lat50=$OVER_P50 lat99=$OVER_P99 g99=${GP99:-0} d99=${DP99:-0} c1note=""
  if [ "$C1_OK" != 1 ]; then
    # No trustworthy c1 sample: latency fields go null with the evidence, the sweeps still stand.
    lat50=null; lat99=null; g99=null; d99=null
    c1note=", \"c1_note\": \"$(json_escape "$C1_ERR")\""
  fi
  CELL_PERF_JSON=", \"perf\": {\"added_latency_p50_us\": $lat50, \"added_latency_p99_us\": $lat99, \"gateway_c1_p99_us\": $g99, \"direct_c1_p99_us\": $d99, \"rps_sustained_20ms\": $lrps, \"rps_sustained_20ms_mock_bound\": $lbound, \"rps_max_proxy\": $prps, \"rps_max_proxy_mock_bound\": $pbound, \"egress_reverified\": true$c1note}"
  log "[$GATEWAY]   $egress <- $cell : perf added_p99=${lat99}us sustained@${SWEEP_TTFT_MS}ms=${lrps}rps max_proxy=${prps}rps (leg-3 re-verified)"
}

CELLS_JSON=""
emit_cell(){ # cell served status path note snippet  (+ CELL_PERF_JSON, cleared after use)
  CELLS_JSON="${CELLS_JSON}${CELLS_JSON:+,}
      \"$1\": {\"served\": $2, \"status\": \"$3\", \"path\": \"$4\", \"verdict_note\": \"$(json_escape "$5")\", \"body_snippet\": \"$(json_escape "$6")\"$CELL_PERF_JSON}"
  CELL_PERF_JSON=""
}

WARM_OK=0; WARM_LAST=000; SERVE_ERR=""
run_cell(){ # egress cell path body extra-header...
  local egress="$1" cell="$2" path="$3" data="$4"; shift 4
  local served=false note="" v m snip
  if [ "$WARM_OK" != 1 ]; then
    LAST_STATUS="$WARM_LAST"; LAST_BODY=""
    note="gateway never served the openai warm-up under the $egress egress config; not probed. $SERVE_ERR"
    emit_cell "$cell" "$served" "$LAST_STATUS" "$path" "$note" ""
    log "[$GATEWAY]   $egress <- $cell : served=false (warm-up failed)"
    return
  fi
  mock_reset
  probe "$path" "$data" "$@"
  v="$(verdict "$cell" "$egress")"
  if [ "$cell" = cohere ] && { [ "$LAST_STATUS" = 404 ] || [ "$LAST_STATUS" = 405 ]; }; then
    # cohere fallback: some gateways mount v1 chat only
    probe "$P_COHERE_FB" "$data" "$@"
    v="$(verdict "$cell" "$egress")"
    path="$P_COHERE_FB"
  fi
  m="$(mock_hit "$egress")"
  if [ "${LAST_STATUS#2}" != "$LAST_STATUS" ] && { [ "$v" = ok ] || [ "$v" = "ok passthrough" ]; }; then
    if [ "$m" = ok ]; then
      served=true
      if [ "$v" = "ok passthrough" ]; then
        note="HTTP $LAST_STATUS, passthrough (same dialect, no translation required); round trip to the mock's $egress endpoint confirmed"
      elif [ "$cell" = "$egress" ]; then
        note="HTTP $LAST_STATUS, $cell envelope validated (same dialect, no translation required); round trip to the mock's $egress endpoint confirmed"
      else
        note="HTTP $LAST_STATUS, $cell envelope validated; mock received a $egress-shaped request on its $egress endpoint"
      fi
    elif [ "$m" = miss ]; then
      note="HTTP $LAST_STATUS with a valid $cell envelope, but the mock never received a request on the $egress endpoint: the gateway answered without contacting the configured upstream"
    elif [ "${m#misdialect }" != "$m" ]; then
      note="HTTP $LAST_STATUS with a valid $cell envelope, but the gateway did not speak the $egress dialect to the upstream: the mock received the request on the ${m#misdialect } endpoint instead"
    elif [ "$m" = norecord ]; then
      note="HTTP $LAST_STATUS with a valid $cell envelope, but the recording mock's state was unavailable (another process replaced the mock on :$MOCK_PORT?); round trip unverifiable, recorded as not served rather than guessed"
    else
      note="HTTP $LAST_STATUS with a valid $cell envelope, but the request that reached the mock's $egress endpoint did not carry the $egress request shape: ${m#badshape }"
    fi
  elif [ "$cell" = bedrock ] && { [ "$LAST_STATUS" = 401 ] || [ "$LAST_STATUS" = 403 ]; }; then
    # the gateway rejected the bearer token on the bedrock ingress: it wants SigV4, which this
    # harness does not forge. Distinct verdict: not served, but not a red either.
    served='"unprobed_auth"'
    note="HTTP $LAST_STATUS with a bearer token; gateway appears to require inbound SigV4 on the bedrock ingress, which this probe does not sign"
  elif printf '%s' "$v" | grep -q '^passthrough'; then
    note="UNTRANSLATED $v; the gateway proxied $path through verbatim instead of translating (HTTP $LAST_STATUS)"
  else
    note="HTTP $LAST_STATUS on POST $path: $v"
  fi
  snip="$(printf '%s' "$LAST_BODY" | head -c 200)"
  log "[$GATEWAY]   $egress <- $cell : served=$served ($note)"
  # ADDITIVE per-cell perf: only a green (served=true) cell is swept; the capability verdict above
  # is already final and is not re-derived from the sweep in any way.
  if [ "$served" = true ]; then
    matrix_cell_perf "$egress" "$cell" "$path" "$data" "$@"
  fi
  emit_cell "$cell" "$served" "$LAST_STATUS" "$path" "$note" "$snip"
}

# ── egress loop: (re)configure + relaunch the gateway per upstream dialect, probe all 6 ingress ──
launch_egress(){ # dialect -> 0 launched, 1 launch failed
  gw_stop 2>/dev/null; sleep 1
  if declare -f gw_matrix_egress >/dev/null; then gw_matrix_egress "$1" || return 1
  else gw_launch || return 1; fi
  rebuild_headers
  return 0
}

warm_up(){ # egress
  WARM_OK=0; WARM_LAST=000
  local i
  for i in $(seq 1 45); do
    WARM_LAST=$(curl -s -m3 -o /dev/null -w "%{http_code}" "http://127.0.0.1:$GW_PORT$P_OPENAI" -X POST \
        -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" ${CURL_H[@]+"${CURL_H[@]}"} \
        -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}")
    [ "$WARM_LAST" = 200 ] && { WARM_OK=1; return 0; }
    sleep 1
  done
  SERVE_ERR="HTTP $WARM_LAST on POST $P_OPENAI; diag=[$(gw_diag 2>&1 | tail -n 20)]"
  log "[$GATEWAY] WARNING: no 200 on the openai warm-up for egress=$1 (last=$WARM_LAST)"
  return 1
}


UPSTREAMS_JSON=""
COMPAT_CELLS=""; COMPAT_SHAPE=""; COMPAT_SERVED=false; COMPAT_ERR=""
for EGRESS in $EGRESS_ALL; do
  CELLS_JSON=""
  # Wall-clock backstop: if a pathological gateway has already burned the suite ceiling, stop probing
  # further egress columns and record them not-served (timeout) so run-all can never wedge here.
  if col_capable "$EGRESS" && suite_deadline_expired; then
    log "[$GATEWAY] egress=$EGRESS: suite wall-clock ceiling reached - recording not served, moving on"
    WARM_OK=0
    for CELL in $INGRESS_ALL; do
      emit_cell "$CELL" false "" "$(ingress_path "$CELL")" \
        "suite wall-clock ceiling (${HARNESS_SUITE_CEIL_S}s) reached before this egress column was probed" ""
    done
    UPSTREAMS_JSON="${UPSTREAMS_JSON}${UPSTREAMS_JSON:+,}
    \"$EGRESS\": {\"configurable\": true, \"served\": false, \"serve_error\": \"suite wall-clock ceiling reached\", \"cells\": {$CELLS_JSON
    }}"
    continue
  fi
  if ! col_capable "$EGRESS"; then
    # No cell in this egress column is declared capable: the gateway does not claim ANY translation
    # into this upstream dialect. Emit every cell grey with its cited capability-limit reason; never
    # launch or probe. Grey here is a declared limit, not untested generosity.
    log "[$GATEWAY] egress=$EGRESS: not declared (no capability claim for this upstream dialect)"
    for CELL in $INGRESS_ALL; do
      emit_cell "$CELL" '"not_configurable"' "" "$(ingress_path "$CELL")" \
        "$GW_MATRIX_CAP_NOTE" ""
    done
    UPSTREAMS_JSON="${UPSTREAMS_JSON}${UPSTREAMS_JSON:+,}
    \"$EGRESS\": {\"configurable\": false, \"served\": false, \"cap_note\": \"$(json_escape "$GW_MATRIX_CAP_NOTE")\", \"cells\": {$CELLS_JSON
    }}"
    continue
  fi
  log "[$GATEWAY] egress=$EGRESS: launching (robust boot, up to $HARNESS_BOOT_ATTEMPTS attempts)"
  # Ready fn for this egress column: warm_up sets WARM_OK/WARM_LAST/SERVE_ERR and returns non-zero if
  # the gateway never answered 200 under this egress config. harness_launch_ready re-runs
  # launch_egress <dialect> + this probe up to N attempts, so a transient per-egress boot failure
  # doesn't zero the whole column; only after all attempts fail is the column recorded not-served.
  _egress_ready(){ warm_up "$EGRESS"; }
  if ! harness_launch_ready launch_egress _egress_ready "$EGRESS"; then
    # WARM_OK is already 0 and SERVE_ERR carries warm_up's last diagnostic; annotate with the retry note.
    WARM_OK=0; WARM_LAST="${WARM_LAST:-000}"
    SERVE_ERR="${HARNESS_SERVE_ERR}${SERVE_ERR:+; }${SERVE_ERR:-}"
    log "[$GATEWAY] egress=$EGRESS: not ready after $HARNESS_BOOT_ATTEMPTS attempts"
  fi
  EG_SERVED=$([ "$WARM_OK" = 1 ] && echo true || echo false)
  for CELL in $INGRESS_ALL; do
    # Per-cell capability gate: a declared-0 (ingress,egress) pair is grey without probing even when
    # the egress column is launched (the gateway claims SOME translations into this upstream but not
    # this ingress). Only declared-1 cells are probed for a real pass/fail.
    if [ "$(cap "$CELL" "$EGRESS")" != 1 ]; then
      emit_cell "$CELL" '"not_configurable"' "" "$(ingress_path "$CELL")" "$GW_MATRIX_CAP_NOTE" ""
      log "[$GATEWAY]   $EGRESS <- $CELL : not declared (grey)"
      continue
    fi
    BODY="$(ingress_body "$CELL")"
    case "$CELL" in
      anthropic) run_cell "$EGRESS" "$CELL" "$(ingress_path "$CELL")" "$BODY" \
        -H "anthropic-version: 2023-06-01" -H "x-api-key: $GW_AUTH" ${XH[@]+"${XH[@]}"};;
      gemini) run_cell "$EGRESS" "$CELL" "$(ingress_path "$CELL")" "$BODY" \
        -H "x-goog-api-key: $GW_AUTH";;
      *) run_cell "$EGRESS" "$CELL" "$(ingress_path "$CELL")" "$BODY";;
    esac
  done
  UPSTREAMS_JSON="${UPSTREAMS_JSON}${UPSTREAMS_JSON:+,}
    \"$EGRESS\": {\"configurable\": true, \"served\": $EG_SERVED, \"serve_error\": \"$(json_escape "$SERVE_ERR")\", \"cells\": {$CELLS_JSON
    }}"
  # v1 compat row: the openai-egress cells when configured, else the first configured egress.
  if [ "$EGRESS" = openai ] || [ -z "$COMPAT_SHAPE" ]; then
    COMPAT_SHAPE="$EGRESS"; COMPAT_CELLS="$CELLS_JSON"; COMPAT_SERVED="$EG_SERVED"; COMPAT_ERR="$SERVE_ERR"
  fi
  SERVE_ERR=""
done

cat > "$RESULTS/$GATEWAY.json" <<JSON
{
  "gateway": "$GATEWAY",
  "build": "$BUILD",
  "matrix_version": 2,
  "served": $COMPAT_SERVED,
  "serve_error": "$(json_escape "$COMPAT_ERR")",
  "upstream_shape": "$COMPAT_SHAPE",
  "upstream_note": "v2: full 6x6. Top-level cells are the $COMPAT_SHAPE-egress row for v1 compatibility; the full grid is under upstreams.<egress>.cells keyed by ingress dialect.",
  "egress_configured": "$GW_MATRIX_EGRESS",
  "cap_note": "$(json_escape "$GW_MATRIX_CAP_NOTE")",
  "cell_perf_sweep": $([ "$MATRIX_SWEEP" = 1 ] && echo true || echo false),
  "sweep_ttft_ms": $SWEEP_TTFT_MS,
  "p99_ceiling_ms": $P99_CEIL_MS,
  "cells": {$COMPAT_CELLS
  },
  "upstreams": {$UPSTREAMS_JSON
  },
  "model": "$GW_MODEL",
  "upstream_endpoint": "$GW_PATH",
  "arch": "${BENCH_ARCH:-$(uname -m)}",
  "hardware": "${BENCH_HARDWARE:-$(uname -m) $(nproc 2>/dev/null || echo '?')vCPU}",
  "measured_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
JSON
echo "================================================================"
echo " gateway=$GATEWAY   protocol support matrix (rows=ingress, cols=egress)"
python3 - "$RESULTS/$GATEWAY.json" <<'PY'
import json, sys
j = json.load(open(sys.argv[1]))
ups = j.get("upstreams", {})
egs = list(ups.keys())
sym = {True: "PASS", False: "fail", "not_configurable": "n/c ", "unprobed_auth": "auth"}
print("   %-17s" % "ingress \\ egress" + "".join("%-18s" % e for e in egs))
cells0 = next(iter(ups.values()))["cells"]
for cell in cells0:
    row = ["%-18s" % sym.get(ups[e]["cells"][cell]["served"], "?") for e in egs]
    print("   %-17s" % cell + "".join(row))
PY
echo " -> $RESULTS/$GATEWAY.json"
echo "================================================================"
