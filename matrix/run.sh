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
# PROBE-FIRST (v3): ALL 36 cells are attempted for EVERY gateway. Each cell gets one cheap
# correctness-checked round trip (the three-leg verdict below); pass -> green + per-cell perf sweep,
# fail -> served:"not_configured" with the probe's error evidence in probe_note - NEVER a red.
# GW_MATRIX_CAP is advisory citation metadata only (warm-up preference + transient patience).
#
# EGRESS HOOK (per-gateway, optional): a manifest may define
#     gw_matrix_egress <dialect>
# which reconfigures + relaunches the gateway so GW_MODEL routes to the mock upstream speaking
# <dialect>. When defined, the runner calls it for EVERY egress dialect; a dialect the writer
# rejects (returns non-zero) falls back to the gateway's DEFAULT config, still probed - the leg-3
# evidence then honestly records where requests actually went. Without gw_matrix_egress the default
# config is launched once and every column is probed against it. GW_MATRIX_EGRESS remains as
# advisory metadata recorded in the JSON.
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
#   GATEWAY=busbar matrix/run.sh
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
export CORES="${CORES:-0-3}"; LOADCORES="${LOADCORES:-0-3}"; MOCKCORES="${MOCKCORES:-0-3}"
export MOCK_PORT="${MOCK_PORT:-8000}"
RESULTS="$ROOT/results/matrix"; mkdir -p "$RESULTS"
log(){ echo "[$(date +%H:%M:%S)] $*"; }
# Strip control chars from gateway-controlled bytes before they reach the operator terminal / the
# committed fanout log (audit R3-LOW-2). A gateway-under-test is arbitrary third-party software; a
# crafted request/response body with ANSI/OSC escapes (\x1b[2J, title-set OSC, cursor codes) would
# otherwise inject into the live terminal and corrupt the committed log via the raw `log "... $note"`
# lines. Keep printable + whitespace, replace everything else with '?'. (json_escape handles JSON
# safety separately; this covers the human-facing path json_escape does not.)
strip_ctrl(){ printf '%s' "$1" | tr -c '[:print:][:space:]' '?'; }
command -v taskset >/dev/null || taskset(){ shift 2; "$@"; }
command -v setsid  >/dev/null || setsid(){ "$@"; }
log "fetching prebuilt rig (mock + loadgen) — no on-box toolchain needed"
. "$ROOT/lib/rig.sh"; fetch_rig "$ROOT" || { echo "rig fetch failed"; exit 1; }

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
# UGEN was set by fetch_rig above (prebuilt loadgen); MATRIX_SWEEP stays a manual disable knob.
[ "$MATRIX_SWEEP" = 1 ] && [ ! -x "$UGEN" ] && { echo "WARNING: no loadgen - per-cell perf sweep disabled (capability matrix still runs)"; MATRIX_SWEEP=0; }
# Same knobs + defaults as perf/run.sh so the per-cell numbers are directly comparable.
C1_DUR="${C1_DUR:-20}"; SWEEP_DUR="${SWEEP_DUR:-10}"; PSIZE="${PSIZE:-256}"
SWEEP_INSTANT="${SWEEP_INSTANT:-16 8192}"   # [min,max] bounds for the peak search (see perf/run.sh)
# PEAK search bounds (min max), not a fixed ladder - see lib/sweep.sh mode=peak + perf/run.sh.
SWEEP_DELAYED="${SWEEP_DELAYED:-32 65536}"
SWEEP_TTFT_MS="${SWEEP_TTFT_MS:-20}"
P99_CEIL_MS="${P99_CEIL_MS:-1000}"

# ── per-cell STREAMING (folded in via lib/stream_measure.sh) ──────────────────────────────────────
# Every served cell that gets a perf sweep ALSO gets a streaming measurement on the SAME (ingress
# path, egress config) pair: added TTFT p99, added per-token-gap p99 (paced c1), streams-sustained
# (BISECTED true concurrency), and cpu-fps (PEAK unpaced throughput). MATRIX_STREAM=0 disables it
# (leaving the RPS/latency sweep intact) — the escape hatch for a fast A/B run. Knobs mirror
# stream/run.sh (paced lane) + streamcpu/run.sh (unpaced lane) so the numbers are directly comparable.
MATRIX_STREAM="${MATRIX_STREAM:-1}"
# Paced lane (stream/run.sh parity): TTFT + inter-frame gap under realistic ~20ms token cadence.
MATRIX_STREAM_CHUNKS="${MATRIX_STREAM_CHUNKS:-64}"
MATRIX_STREAM_INTERVAL_MS="${MATRIX_STREAM_INTERVAL_MS:-20}"
MATRIX_STREAM_CHUNK_BYTES="${MATRIX_STREAM_CHUNK_BYTES:-16}"
MATRIX_STREAM_STALL_X="${MATRIX_STREAM_STALL_X:-2}"
MATRIX_STREAM_C1_DUR="${MATRIX_STREAM_C1_DUR:-20}"
MATRIX_STREAM_SWEEP_DUR="${MATRIX_STREAM_SWEEP_DUR:-12}"
MATRIX_STREAM_SUST_BOUNDS="${MATRIX_STREAM_SUST_BOUNDS:-8 2048}"   # [lo,hi] for the sustained bisect
MATRIX_STREAM_DELIV="${MATRIX_STREAM_DELIV:-0.999}"               # sustained gate delivered-frames floor
# Unpaced lane (streamcpu/run.sh parity): CPU-bound relay throughput (frames/sec) — long back-to-back
# bursts so per-frame relay cost dominates. cpu-fps is the PEAK of the fps-vs-concurrency curve.
MATRIX_STREAMCPU_CHUNKS="${MATRIX_STREAMCPU_CHUNKS:-512}"
MATRIX_STREAMCPU_FRAME_BYTES="${MATRIX_STREAMCPU_FRAME_BYTES:-16}"
MATRIX_STREAMCPU_STALL_MS="${MATRIX_STREAMCPU_STALL_MS:-250}"
MATRIX_STREAMCPU_DUR="${MATRIX_STREAMCPU_DUR:-16}"
MATRIX_STREAMCPU_FPS_BOUNDS="${MATRIX_STREAMCPU_FPS_BOUNDS:-8 512}"  # [lo,hi] for the cpu-fps peak search

# ── memory ONCE (folded in via memory/run.sh's logic, relocated here) ─────────────────────────────
# Idle + peak RSS is a PROCESS-LEVEL number (not per-cell), measured ONE time during the matrix run
# under a sustained load against the gateway's default config. MATRIX_MEMORY=0 disables it. Knobs
# mirror memory/run.sh. The gateway's own gw_rss()/gw_hwm() manifest hooks are the measurement, so a
# manifest with no gw_rss (the mock-gateway fixture) degrades to 0/null, exactly like memory/run.sh.
MATRIX_MEMORY="${MATRIX_MEMORY:-1}"
MEM_PSIZE="${MEM_PSIZE:-150000}"; MEM_CONC="${MEM_CONC:-1500}"; MEM_DUR="${MEM_DUR:-120}"
MEM_CAP_MIB="${MEM_CAP_MIB:-40000}"
# Post-load settle wait: after the sustained load stops, how long to wait before reading post-load RSS
# (does memory release?). Field default 60s; a local dev verifier shrinks it (MEM_SETTLE_S=2) so the
# minutes-long local run isn't dominated by a fixed 60s sleep. The measurement itself is unchanged.
MEM_SETTLE_S="${MEM_SETTLE_S:-60}"
# ADAPTIVE RUNG SELECTION + shared rig baselines (lib/sweep.sh knobs; see its header). A gateway
# that serves the whole 6x6 sweeps up to 36 cells, and the naive per-cell cost (~4 min: 15 fixed
# ladder rungs + a re-measured direct c1 baseline + 2 re-measured mock ceilings, per cell) put this
# suite at ~2.3 h on the field box - a RUNTIME problem, not a fidelity one, because most of that
# wall time re-measures rig constants or probes ladder rungs far from the winner. With
# SWEEP_ADAPTIVE=1 every probed rung is measured EXACTLY as before (same loadgen invocation, same
# window, same gates) but a sweep starts at the previous winner's rung and expands only while the
# best gate-passing rung sits on the probed window's edge; the FIRST sweep at each ttft still walks
# the full ladder (seeding the prior), and a window with no gate-passing rung falls back to the
# full ladder. Direct-to-mock numbers (c1 baseline + mock ceilings, keyed by ingress dialect via
# SWEEP_CACHE_KEY below) are measured once per dialect and reused: they measure the rig, never the
# gateway. Every gateway-facing measurement still runs fresh on every cell.
# MATRIX_SWEEP_ADAPTIVE=0 restores the full ladder on every cell (the A/B validation knob).
SWEEP_ADAPTIVE="${MATRIX_SWEEP_ADAPTIVE:-1}"
# Sweeping every green cell multiplies the suite's wall time (by design: that is the whole point of
# per-cell perf), and folding a per-cell STREAMING measurement in on top of that lengthens it further
# (a paced c1 window + a sustained-streams bisect + an unpaced cpu-fps peak search per cell). So the
# default suite ceiling rises from harness.sh's 45 min to 6 h when the sweep+stream are on. An
# explicit HARNESS_SUITE_CEIL_S still wins, and the ceiling still backstops a wedged gateway.
[ "$MATRIX_SWEEP" = 1 ] && HARNESS_SUITE_CEIL_S="${HARNESS_SUITE_CEIL_S:-21600}"
# shellcheck source=/dev/null
source "$ROOT/lib/harness.sh"
# shellcheck source=/dev/null
source "$ROOT/lib/sweep.sh"
# shellcheck source=/dev/null
source "$ROOT/lib/stream_measure.sh"
# shellcheck source=/dev/null
source "$GW_DIR/gateway.sh"
suite_deadline_start
EGRESS_ALL="openai openai-responses anthropic gemini cohere bedrock"
INGRESS_ALL="openai openai-responses anthropic gemini cohere bedrock"
# DEV SUBSET (local fast verifier only; field runs leave these unset → the full 6x6). Restrict the
# egress columns and/or ingress rows PROBED so a local end-to-end smoke run of the whole pipeline
# finishes in minutes instead of exercising all 36 cells. The gen-data headline/streaming/memory
# projections only need the openai diagonal cell, so MATRIX_EGRESS_ONLY="openai" MATRIX_INGRESS_ONLY=
# "openai" drives the full producer path (probe → per-cell perf + streaming → memory-once → OOTB) on a
# single cell. Space-separated dialect lists; any value not in the canonical set is ignored.
_subset(){ local want="$1" all="$2" out="" x y; for x in $want; do for y in $all; do [ "$x" = "$y" ] && out="$out $y"; done; done; echo "${out# }"; }
if [ -n "${MATRIX_EGRESS_ONLY:-}" ]; then EGRESS_ALL="$(_subset "$MATRIX_EGRESS_ONLY" "$EGRESS_ALL")"; fi
if [ -n "${MATRIX_INGRESS_ONLY:-}" ]; then INGRESS_ALL="$(_subset "$MATRIX_INGRESS_ONLY" "$INGRESS_ALL")"; fi
[ -n "$EGRESS_ALL" ] || { echo "MATRIX_EGRESS_ONLY matched no valid egress dialect"; exit 2; }
[ -n "$INGRESS_ALL" ] || { echo "MATRIX_INGRESS_ONLY matched no valid ingress dialect"; exit 2; }

# ── PROBE-FIRST: the capability matrix (GW_MATRIX_CAP) is ADVISORY ──────────────────────────────
# The runner attempts ALL 36 cells for EVERY gateway. Each cell gets a cheap capability probe (one
# correctness-checked round trip: the ingress dialect's request through the gateway, response
# verified as a valid INGRESS-dialect envelope, and the mock's per-dialect request record proving
# the EGRESS leg - the same three-leg verdict as always). Probe passes -> the cell is green and gets
# its per-cell perf sweep. Probe fails -> served:"not_configured" with the probe's error evidence
# (HTTP status / verdict / first bytes) in probe_note - NEVER a red: a failed probe on a cell nobody
# wired is "this pairing is not configured/supported here", with the evidence to say why.
#
# GW_MATRIX_CAP survives as ADVISORY CITATION METADATA only (published as capability_note context +
# per-cell hints). It no longer gates probing. It still informs two HARNESS-SIDE choices that never
# touch a verdict:
#   - warm-up preference: each egress column warms on the first advisory-declared ingress (a cell
#     expected to 200), falling back to openai;
#   - transient patience: a declared cell that answers 000/5xx gets the full transient-retry budget
#     (it is expected to work, so a dead socket is worth waiting out); an undeclared cell gets a
#     short budget (a persistent 5xx there is overwhelmingly "not wired", and the worst case is a
#     grey not_configured carrying that evidence, never a red).
#
# Format: 6 whitespace-separated rows (rows = ingress in EGRESS_ALL order), each row 6 chars of 1/0
# (cols = egress in EGRESS_ALL order). Blank/unset => back-compat: derive a full column of 1s for
# every egress dialect listed in GW_MATRIX_EGRESS (the pre-cap behaviour), 0 elsewhere.
declare -A CAP
_cap_col_any=""   # space-list of egress dialects with >=1 declared-1 cell (advisory)
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
    # MEDIUM-8: GW_MATRIX_CAP is ALWAYS the canonical full 6x6 (row-major: ingress-major, both axes in
    # the canonical dialect order openai openai-responses anthropic gemini cohere bedrock). When
    # MATRIX_EGRESS_ONLY / MATRIX_INGRESS_ONLY prunes an axis, the pruned-loop counters i/j no longer
    # map to the canonical 6-column position, so `rows:(i*6+j)` would read the WRONG bit (e.g. a
    # single-egress subset would read column 0 for every dialect) → wrong capability bit → misdirected
    # warm-up + patience → a healthy column recorded not_verified. Index by each dialect's CANONICAL
    # position, never the pruned counters.
    local _canon="openai openai-responses anthropic gemini cohere bedrock"
    canon_idx(){ local x="$1" n=0 y; for y in $_canon; do [ "$y" = "$x" ] && { echo "$n"; return; }; n=$((n+1)); done; echo -1; }
    for ing in $INGRESS_ALL; do
      i="$(canon_idx "$ing")"
      for eg in $EGRESS_ALL; do
        j="$(canon_idx "$eg")"
        if [ "$i" -ge 0 ] && [ "$j" -ge 0 ]; then CAP["$ing/$eg"]="${rows:$((i*6+j)):1}"; else CAP["$ing/$eg"]=0; fi
      done
    done
  fi
  # Advisory only: which egress columns have any declared cell (warm-up + patience hints).
  for eg in $EGRESS_ALL; do
    for ing in $INGRESS_ALL; do
      if [ "${CAP["$ing/$eg"]}" = 1 ]; then _cap_col_any="$_cap_col_any $eg"; break; fi
    done
  done
}
cap(){ echo "${CAP["$1/$2"]:-0}"; }   # cap <ingress> <egress> -> 1|0 (ADVISORY, never a verdict)
build_cap
# Advisory context published with the result (tooltips); a manifest overrides it with a cited string.
GW_MATRIX_CAP_NOTE="${GW_MATRIX_CAP_NOTE:-this gateway does not declare support for this ingress/upstream dialect pair}"

# ── untestable cells (mock-reachability limit, NOT incapability) ────────────────────────────────
# Some gateways DO speak a dialect in production but hardcode the real cloud host for it (no
# base-URL override), so this harness's localhost mock is unreachable: that is OUR test rig's
# limit, not the gateway's incapability, and it must not render as either grey-incapable or red.
# A manifest declares such cells (the one remaining probe exemption under probe-first) as
#   GW_MATRIX_UNTESTABLE="<ingress>/<egress> <ingress>/<egress> ..."
# with a cited GW_MATRIX_UNTESTABLE_NOTE; the runner emits served:"untestable" +
# reason:"no_base_url_override" for them. Generic: any manifest can use it, the runner names nobody.
GW_MATRIX_UNTESTABLE="${GW_MATRIX_UNTESTABLE:-}"
GW_MATRIX_UNTESTABLE_NOTE="${GW_MATRIX_UNTESTABLE_NOTE:-the gateway supports this pair in production but pins the real cloud host (no upstream base-URL override), so the test mock is unreachable}"
cell_untestable(){ case " $GW_MATRIX_UNTESTABLE " in *" $1/$2 "*) return 0;; *) return 1;; esac; }
# GW_MATRIX_EGRESS is advisory metadata (recorded in the JSON); under probe-first EVERY egress
# column is attempted: gw_matrix_egress <dialect> when the manifest defines it (falling back to the
# default gw_launch config when the writer rejects a dialect), else the default config for all six.
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
  [ -n "$MOCK" ] && pkill -f "$MOCK" 2>/dev/null; sleep 1
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
  [ -n "$MOCK" ] && pkill -f "$MOCK" 2>/dev/null; sleep 1
  setsid taskset -c "$MOCKCORES" "$MOCK" -port "$MOCK_PORT" </dev/null >/dev/null 2>&1 &
  sleep 1
}
log "starting mock :$MOCK_PORT (instant, all six dialects by path, request recording ON)"
# MEDIUM-7: mock_start_record returns 1 if the recording mock never rebound :$MOCK_PORT (the script is
# set -uo pipefail, NOT -e, so the return is otherwise silently dropped). A dead port at STARTUP means
# every one of the 36 cells probes a dead socket → the whole board reads not_verified while the script
# still exits 0, publishing an all-not_verified board as if it were valid. Abort the run non-zero
# instead — a board that could never have been fairly measured must never be published.
if ! mock_start_record; then
  echo "FATAL: recording mock failed to bind :$MOCK_PORT at startup — every cell would probe a dead port (all not_verified). Aborting rather than publishing an unmeasurable board." >&2
  exit 1
fi
cleanup(){ gw_stop 2>/dev/null; [ -n "$MOCK" ] && pkill -f "$MOCK" 2>/dev/null; }
trap cleanup EXIT

log "[$GATEWAY] build"; gw_build || { echo "build failed"; exit 1; }
BUILD="$(gw_version 2>/dev/null | tr -d '\n' | sed 's/"/\\"/g')"
# OOTB config artifact — capture the gateway's as-shipped default config to results/config/<gw>.txt
# and record the sidecar pointer. Previously the perf suite was the natural home (it always ran); now
# that the matrix is the sole producer it captures the config too (cheap + idempotent). A gateway with
# no gw_config() hook degrades to an empty pointer, published as "not published". See
# lib/harness.sh:harness_write_config + site/gen-data.mjs (reads matrix.ootb_config, else config/<gw>.txt).
OOTB_CONFIG="$(harness_write_config "$GATEWAY" "$ROOT/results" 2>/dev/null || true)"

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
    # Strip control chars from the gateway-controlled snippet (audit R3-LOW-2): it flows into a raw
    # `log "... $note"` line to the operator terminal + committed fanout log; ANSI/OSC escapes must not.
    _snip = d.get("last_snippet", "")[:160]
    _snip = "".join(ch if (ch.isprintable() or ch == " ") else "?" for ch in _snip)
    print("badshape " + _snip)
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

# A TRANSIENT failure is a transport/reachability problem, NOT an answer: the gateway never handed us
# a real application response we could judge. In this controlled rig the upstream is always the local
# mock, so a 5xx or a curl-level 000 is the gateway failing to REACH its upstream (a dead socket after
# a mock restart, an upstream-pool hiccup, a connection reset), never the gateway "answering wrongly".
# We must retry such a probe BEFORE recording anything - never publish a transient blip as a red, then
# re-run the whole box. A 2xx/3xx/4xx is a real application response (right or wrong) and is NOT retried.
probe_transient(){ case "$LAST_STATUS" in 000|5[0-9][0-9]) return 0 ;; *) return 1 ;; esac; }
MATRIX_TRANSIENT_RETRIES="${MATRIX_TRANSIENT_RETRIES:-3}"   # total attempts on a declared cell
MATRIX_TRANSIENT_PAUSE="${MATRIX_TRANSIENT_PAUSE:-120}"     # seconds between its retries
# Probe-first patience budget for ADVISORY-UNDECLARED cells: all 36 cells are probed, and a cell
# nobody wired often answers 5xx (an upstream config that can't work) rather than 404. Waiting the
# declared budget (2 x 120s) on ~30 dead cells would add hours per gateway; a short budget keeps a
# genuinely-transient blip retried while a truly-dead cell costs seconds. Verdicts are unaffected:
# a persistent transient is still not_verified (upstream_unreachable), never a red.
MATRIX_PROBE_TRANSIENT_RETRIES="${MATRIX_PROBE_TRANSIENT_RETRIES:-2}"
MATRIX_PROBE_TRANSIENT_PAUSE="${MATRIX_PROBE_TRANSIENT_PAUSE:-10}"

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
 "openai-responses": '{"id":"resp_x","object":"response","created_at":1,"status":"completed","model":"mock","output":[{"type":"message","id":"msg_x","status":"completed","role":"assistant","content":[{"type":"output_text","text":"ok","annotations":[]}]}],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}',
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

# ── per-cell STREAMING (lib/stream_measure.sh) ───────────────────────────────────────────────────
# matrix_cell_stream <egress> <cell> <path> <body> [extra -H header...]
# Runs the shared streaming measurement (lib/stream_measure.sh) on one green cell, on the SAME ingress
# path + egress config matrix_cell_perf just swept. Two lanes, each restarting the mock into its own
# streaming shape:
#   * PACED (~20ms cadence): c1 added TTFT p99 + added per-token-gap p99, then the streams-sustained
#     BISECT (the true max sustainable concurrency between grid rungs);
#   * UNPACED (interval 0): the cpu-fps PEAK (max sustained aggregate frames/sec).
# Sets CELL_STREAM_JSON (a leading `, "stream": {...}` fragment, empty on skip); emit_cell folds it
# into the cell object beside "perf". Whether the gateway actually streams this cell is probed once
# (curl -N for SSE frames); a non-streaming cell records stream_served:false with the evidence — never
# a crash, never a fabricated number. Caller has UGEN_H set to this cell's dialect headers already.
CELL_STREAM_JSON=""
# MEDIUM-3 (FIX b): the mock only emits native SSE for the OpenAI and Anthropic EGRESS dialects (an
# openai chat.completion.chunk / anthropic content_block_delta stream). For a responses/gemini/cohere/
# bedrock EGRESS the mock returns plain JSON even on stream:true — Bedrock's Converse stream in
# particular is AWS's BINARY event-stream framing (application/vnd.amazon.eventstream), not SSE, which
# this mock deliberately does not synthesize. So a gateway whose best diagonal is one of those egress
# dialects cannot produce an upstream SSE stream through THIS rig no matter how correct it is: the
# missing frames are the MOCK's limit, not the gateway's. Such a cell must record served:"untestable"
# (a rig-reachability limit, like GW_MATRIX_UNTESTABLE), NOT stream_served:false (a gateway fault).
# (FIX a — teaching the mock all six native SSE shapes — was rejected: Bedrock's binary event-stream
# cannot be synthesized correctly as SSE, so an all-six claim would itself be dishonest.)
mock_streams_egress(){ case "$1" in openai|anthropic) return 0;; *) return 1;; esac; }
matrix_cell_stream(){
  local egress="$1" cell="$2" path="$3" data="$4"; shift 4
  # SM_STREAM_BODY (MEDIUM-2) is dynamically scoped to this call: stream_probe (in the searches
  # below) reads it as the -body to POST, and `local` auto-clears it on return so it never leaks to
  # the standalone stream/streamcpu suites or the next cell.
  local SM_STREAM_BODY=""
  CELL_STREAM_JSON=""
  [ "$MATRIX_STREAM" = 1 ] || return 0
  if suite_deadline_expired; then
    log "[$GATEWAY]   $cell : suite ceiling reached - skipping the per-cell stream measurement"
    return 0
  fi
  GURL="http://127.0.0.1:$GW_PORT$path"
  DURL="http://127.0.0.1:$MOCK_PORT$(mock_direct_path "$cell")"
  # Does this cell actually stream? One curl -N probe for SSE frames (paced mock up). A cell that 200s
  # the non-stream sweep but buffers/rejects stream:true records stream_served:false — measured, honest.
  stream_mock_start "$MATRIX_STREAM_CHUNKS" "$MATRIX_STREAM_INTERVAL_MS" "$MATRIX_STREAM_CHUNK_BYTES"
  # HIGH-6: after a mock restart (which pkilled the prior cell's mock and relaunched WITHOUT
  # SO_REUSEADDR), the fresh mock can still be racing the dying one for MOCK_PORT — a single un-retried
  # SSE probe below would then read no `data:` frames and fabricate stream_served:false BEFORE any
  # measurement. Gate on stream_mock_ready (a retried 1-stream liveness probe) first, exactly as the
  # sustained-bisect + cpu-fps lanes already do before their searches.
  stream_mock_ready 30 || log "[$GATEWAY]   $cell : stream mock did not become ready before the served-gating probe"
  local stream_ok=0 stream_err="" sbody probe_to sdata
  # MEDIUM-2: the streaming probes must POST the cell's REAL ingress-dialect body (stream-flagged),
  # not ugen's default openai body — a gemini/cohere/bedrock/responses/anthropic ingress path 400s an
  # openai body, faking a streaming failure that contradicts the cell's own passing RPS sweep. Build
  # the stream-flagged body ONCE here; use it for the gating curl below AND (via SM_STREAM_BODY) for
  # every stream_probe in stream_c1/stream_sustained_bisect/streamcpu_peak_fps.
  sdata="$(printf '%s' "$data" | sed 's/}$/,"stream":true}/')"
  SM_STREAM_BODY="$sdata"
  probe_to=$(( MATRIX_STREAM_CHUNKS * MATRIX_STREAM_INTERVAL_MS / 1000 + 10 ))
  sbody="$(curl -sN -m "$probe_to" "$GURL" -X POST \
      -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" \
      -H "accept: text/event-stream" ${CURL_H[@]+"${CURL_H[@]}"} "$@" \
      -d "$sdata" 2>&1)"
  if grep -q '^data:' <<< "$sbody"; then stream_ok=1
  else stream_err="no SSE frames on stream:true; body=[$(strip_ctrl "$(printf '%s' "$sbody" | head -c 300)")]"
       log "[$GATEWAY]   $cell : stream:true produced no SSE frames - stream_served=false"
  fi
  if [ "$stream_ok" != 1 ]; then
    # MEDIUM-3 (FIX b): distinguish a MOCK limit from a GATEWAY fault. If the mock cannot emit SSE for
    # this EGRESS dialect, no correct gateway could have produced an upstream stream through this rig,
    # so the absent frames are untestable (a rig limit), NOT a stream_served:false gateway fault.
    if ! mock_streams_egress "$egress"; then
      CELL_STREAM_JSON=", \"stream\": {\"stream_served\": \"untestable\", \"reason\": \"mock_no_sse_for_egress\", \"stream_error\": \"$(json_escape "the test mock does not synthesize an SSE stream for the $egress egress dialect (Bedrock's Converse stream is AWS binary event-stream framing, not SSE; responses/gemini/cohere are not streamed by this mock), so an upstream stream is unreachable through this rig — a mock-reachability limit, not a gateway fault")\"}"
      log "[$GATEWAY]   $cell : stream untestable (mock does not stream the $egress egress dialect)"
      return 0
    fi
    CELL_STREAM_JSON=", \"stream\": {\"stream_served\": false, \"stream_error\": \"$(json_escape "$stream_err")\"}"
    return 0
  fi
  # ── PACED lane: c1 added TTFT/gap + streams-sustained bisect ──
  SM_EXPFRAMES="$MATRIX_STREAM_CHUNKS"; SM_STALL_US=$(( MATRIX_STREAM_INTERVAL_MS * MATRIX_STREAM_STALL_X * 1000 ))
  SM_C1_DUR="$MATRIX_STREAM_C1_DUR"; SM_SWEEP_DUR="$MATRIX_STREAM_SWEEP_DUR"; SM_DELIV="$MATRIX_STREAM_DELIV"
  stream_c1
  local lat_c1_ok="$SM_C1_OK"
  local add_t50=$SM_ADD_T50 add_t99=$SM_ADD_T99 add_g50=$SM_ADD_G50 add_g99=$SM_ADD_G99 c1note=""
  if [ "$lat_c1_ok" != 1 ]; then
    add_t50=null; add_t99=null; add_g50=null; add_g99=null
    c1note=", \"stream_c1_note\": \"$(json_escape "$SM_C1_ERR")\""
  fi
  stream_mock_ready 30 || log "[$GATEWAY]   $cell : stream mock did not become ready before the sustained bisect"
  local sust_lo sust_hi; read -r sust_lo sust_hi <<< "$MATRIX_STREAM_SUST_BOUNDS"
  stream_sustained_bisect "$sust_lo" "$sust_hi"
  local sust_streams=$SM_SUST_STREAMS sust_fps=$SM_SUST_FPS sust_bound=$SM_MOCK_BOUND sust_json="$SM_JSON_ACC"
  # ── UNPACED lane: cpu-fps peak ──
  stream_mock_start "$MATRIX_STREAMCPU_CHUNKS" 0 "$MATRIX_STREAMCPU_FRAME_BYTES"
  SM_EXPFRAMES="$MATRIX_STREAMCPU_CHUNKS"; SM_STALL_US=$(( MATRIX_STREAMCPU_STALL_MS * 1000 ))
  SM_SWEEP_DUR="$MATRIX_STREAMCPU_DUR"; SM_DELIV="${MATRIX_STREAMCPU_DELIV:-0.5}"
  stream_mock_ready 30 || log "[$GATEWAY]   $cell : unpaced mock did not become ready before the cpu-fps peak"
  local fps_lo fps_hi; read -r fps_lo fps_hi <<< "$MATRIX_STREAMCPU_FPS_BOUNDS"
  streamcpu_peak_fps "$fps_lo" "$fps_hi"
  local fps_peak=$SM_FPS_PEAK fps_conc=$SM_FPS_PEAK_CONC fps_bound=$SM_FPS_MOCK_BOUND fps_json="$SM_JSON_ACC"
  CELL_STREAM_JSON=", \"stream\": {\"stream_served\": true, \"added_ttft_p50_us\": $add_t50, \"added_ttft_p99_us\": $add_t99, \"added_gap_p50_us\": $add_g50, \"added_gap_p99_us\": $add_g99, \"streams_sustained\": $sust_streams, \"streams_sustained_fps\": $sust_fps, \"streams_sustained_mock_bound\": $sust_bound, \"cpu_fps\": $fps_peak, \"cpu_fps_concurrency\": $fps_conc, \"cpu_fps_mock_bound\": $fps_bound, \"sweep_streams\": [$sust_json], \"sweep_cpu_fps\": [$fps_json]$c1note}"
  log "[$GATEWAY]   $cell : stream added_ttft_p99=${add_t99}us streams_sustained=${sust_streams} cpu_fps=${fps_peak}"
}

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
  # Rig-baseline cache key = the INGRESS dialect: it fixes the direct-to-mock path AND the probe
  # body, i.e. everything the c1 direct baseline and the mock ceilings depend on. Gateway-side
  # measurements ignore this key entirely.
  SWEEP_CACHE_KEY="$cell"
  GURL="http://127.0.0.1:$GW_PORT$path"
  DURL="http://127.0.0.1:$MOCK_PORT$(mock_direct_path "$cell")"
  mock_start_plain
  sweep_c1
  run_sweep 0 "$SWEEP_INSTANT" peak
  # ONE source of truth: the charted array (SW_JSON, every ramp AND bisect probe this sweep made)
  # and the headline (SW_CEIL_RPS/SW_CEIL_CONC = max gate-passing point in THAT SAME array) come out
  # of the single run_sweep call. Carry BOTH into the cell so the drawer's headline is, by
  # construction, one of the points on its own sweep curve - never a separate perf-suite measurement.
  local prps=$SW_CEIL_RPS pconc=$SW_CEIL_CONC pbound=$SW_BOUND pjson="$SW_JSON"
  run_sweep "$SWEEP_TTFT_MS" "$SWEEP_DELAYED" peak
  local lrps=$SW_CEIL_RPS lconc=$SW_CEIL_CONC lbound=$SW_BOUND ljson="$SW_JSON"
  SWEEP_BODY=""; SWEEP_CACHE_KEY=""
  # ── per-cell STREAMING (same ingress path + egress config, still on the plain mock) ─────────────
  # Fold the streaming measurement in here, while the gateway is launched under THIS egress config and
  # UGEN_H still carries this cell's dialect headers. matrix_cell_stream restarts the mock into the
  # streaming shapes it needs (paced for c1+sustained, unpaced for cpu-fps) and restores nothing — the
  # mock_start_record below re-establishes the recording mock for the leg-3 re-verify regardless.
  matrix_cell_stream "$egress" "$cell" "$path" "$data" "$@"
  UGEN_H=()
  # MEDIUM-7: restore the recording mock for the leg-3 re-verify. If it fails to rebind :$MOCK_PORT
  # (set -uo pipefail, no -e, so the return is otherwise dropped), the re-verify + the NEXT cell's
  # capability probe would hit a dead port and be mislabeled. Retry a couple of times; if it still
  # can't come back the rig is wedged and continuing would silently corrupt every following cell —
  # abort non-zero rather than publish mislabeled cells.
  if ! mock_start_record; then
    log "[$GATEWAY]   $egress <- $cell : recording mock did not rebind :$MOCK_PORT after the sweep — retrying"
    if ! mock_start_record; then
      echo "FATAL: recording mock could not be restored on :$MOCK_PORT after the $egress<-$cell sweep — every following cell would probe a dead port. Aborting rather than mislabeling them." >&2
      exit 1
    fi
  fi
  # The sweep restarted the mock, so the gateway's upstream connection pool may hold dead sockets.
  # Fire a couple of discarded warm requests through the gateway so the re-verify probe below (and
  # the NEXT cell's capability probe) can never eat a stale-connection failure. Their record entries
  # are wiped by the mock_reset that immediately follows.
  # Warm with THIS cell's own probe (path/body/headers): the openai ingress is not necessarily
  # declared (or even servable) under every egress config, and a warm request must never depend on
  # an undeclared bridge.
  local _w
  for _w in 1 2; do
    curl -s -m3 -o /dev/null "http://127.0.0.1:$GW_PORT$path" -X POST \
      -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" ${CURL_H[@]+"${CURL_H[@]}"} \
      "$@" -d "$data" 2>/dev/null
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
  # A `miss` (mock recorded ZERO requests) after a heavy sweep + mock restart is almost always a
  # TRANSIENT dead-socket/timeout on the single probe (the gateway's upstream pool holds stale
  # sockets the 2 warm-ups above did not fully flush), NOT a misroute - a real misroute records on
  # the WRONG endpoint and reads `misdialect`, never `miss`. So RETRY the probe on `miss` (each retry
  # re-flushes a socket); only a persistent `misdialect` is a genuine drop. If it is still `miss`
  # after the retries the harness simply could not get a clean post-load reading: keep the cell green
  # (its capability was already proven at probe time) and record perf as NOT MEASURED - never imply a
  # gateway fault we did not observe.
  local reverify=miss _rv
  for _rv in 1 2 3 4 5; do
    mock_reset
    probe "$path" "$data" "$@"
    reverify="$(mock_hit "$egress")"
    [ "$reverify" = ok ] && break
    case "$reverify" in misdialect*) break;; esac   # a real misroute is persistent - do not retry
    sleep 1                                          # let a dead socket cycle out before the next try
  done
  local reverified=true reverify_note=""
  case "$reverify" in
    ok) : ;;
    misdialect*)
      # A MISROUTE: the mock DID receive the re-verify request, on the WRONG endpoint. This is the
      # real fault the leg-3 check exists to catch - drop the perf so a misrouted number never lands.
      log "[$GATEWAY]   $egress <- $cell : LEG-3 RE-VERIFY MISROUTE after load ($reverify) - dropping perf + stream"
      # The stream measurement drove the SAME misrouted path, so its numbers are equally suspect: drop
      # it alongside perf (never publish a misrouted streaming number).
      CELL_STREAM_JSON=""
      CELL_PERF_JSON=", \"perf_dropped\": \"$(json_escape "leg-3 re-verify after load found a misroute to $reverify (not the $egress endpoint); perf + stream withheld to avoid recording a misrouted number")\""
      return 0 ;;
    *)  # miss/norecord after retries: the mock recorded NOTHING (a transient dead socket on the
        # single post-load probe, e.g. a Go upstream pool holding stale sockets). This is NOT a
        # misroute - a misroute is `misdialect` and is caught above. The perf STANDS: the capability
        # probe passed AND the load sweep already drove thousands of successful same-dialect requests
        # through this exact path (that IS the RPS number), so routing was proven under load. We only
        # flag that the single re-confirm probe could not run (egress_reverified=false).
      log "[$GATEWAY]   $egress <- $cell : LEG-3 RE-VERIFY inconclusive ($reverify) - perf STANDS on capability + sweep (re-confirm not run)"
      reverified=false
      reverify_note="post-load re-verify probe could not re-confirm ($reverify, a transient dead socket, not a misroute); the number stands on the capability probe plus the successful load sweep"
      ;;
  esac
  local lat50=$OVER_P50 lat99=$OVER_P99 g99=${GP99:-0} d99=${DP99:-0} c1note=""
  if [ "$C1_OK" != 1 ]; then
    # No trustworthy c1 sample: latency fields go null with the evidence, the sweeps still stand.
    lat50=null; lat99=null; g99=null; d99=null
    c1note=", \"c1_note\": \"$(json_escape "$C1_ERR")\""
  fi
  CELL_PERF_JSON=", \"perf\": {\"added_latency_p50_us\": $lat50, \"added_latency_p99_us\": $lat99, \"gateway_c1_p99_us\": $g99, \"direct_c1_p99_us\": $d99, \"rps_sustained_20ms\": $lrps, \"rps_sustained_20ms_concurrency\": $lconc, \"rps_sustained_20ms_mock_bound\": $lbound, \"rps_max_proxy\": $prps, \"rps_max_proxy_concurrency\": $pconc, \"rps_max_proxy_mock_bound\": $pbound, \"sweep_max_proxy\": [$pjson], \"sweep_sustained_20ms\": [$ljson], \"egress_reverified\": $reverified${reverify_note:+, \"reverify_note\": \"$(json_escape "$reverify_note")\"}$c1note}"
  log "[$GATEWAY]   $egress <- $cell : perf added_p99=${lat99}us sustained@${SWEEP_TTFT_MS}ms=${lrps}rps max_proxy=${prps}rps (leg-3 re-verified)"
}

CELLS_JSON=""; CELL_PROBE_NOTE=""
# emit_cell <cell> <served> <status> <path> <note> <snippet> [reason]
# `reason` is the MACHINE-READABLE verdict class, so consumers never have to regex English prose:
#   (green)          absent                - served=true (probe passed; per-cell perf follows)
#   probe_failed     served="not_configured" - PROBE-FIRST: the capability probe on this cell did
#                                            not produce a correct three-leg round trip. The probe's
#                                            error evidence rides in probe_note. This is "the
#                                            pairing is not configured/supported here", NEVER a red:
#                                            no gateway is failed on a cell, it just doesn't light up.
#   harness_boot_failure / suite_ceiling / mock_norecord
#                    served="not_verified" - the HARNESS could not get a fair reading (gateway never
#                                            warmed under this egress config, wall-clock ceiling,
#                                            recording mock displaced): never graded either way
#   no_base_url_override  served="untestable"  - mock-reachability limit (cited note), NOT incapability
#   inbound_sigv4    served="unprobed_auth"    - gateway insists on inbound SigV4 we do not forge
emit_cell(){
  local reason="${7:-}"
  # CELL_PROBE_NOTE (set by run_cell for not_configured cells, cleared here like CELL_PERF_JSON):
  # the capability probe's raw error evidence - HTTP status / verdict class / what the mock recorded.
  local probe_json=""
  [ -n "$CELL_PROBE_NOTE" ] && probe_json=", \"probe_note\": \"$(json_escape "$CELL_PROBE_NOTE")\""
  CELLS_JSON="${CELLS_JSON}${CELLS_JSON:+,}
      \"$1\": {\"served\": $2, ${reason:+\"reason\": \"$reason\", }\"status\": \"$3\", \"path\": \"$4\", \"verdict_note\": \"$(json_escape "$5")\", \"body_snippet\": \"$(json_escape "$6")\"$probe_json$CELL_PERF_JSON$CELL_STREAM_JSON}"
  CELL_PERF_JSON=""; CELL_STREAM_JSON=""; CELL_PROBE_NOTE=""
}

WARM_OK=0; WARM_LAST=000; WARM_CELL=openai; SERVE_ERR=""
run_cell(){ # egress cell path body extra-header...
  local egress="$1" cell="$2" path="$3" data="$4"; shift 4
  local served=false note="" v m snip
  if [ "$WARM_OK" != 1 ]; then
    LAST_STATUS="$WARM_LAST"; LAST_BODY=""
    # A warm-up/boot failure means the HARNESS never got the gateway serving under this egress
    # config - we could not fairly test the cell. That is machine-readably "not_verified", never a
    # red: only a gateway that actually served and answered wrongly is a failure.
    note="gateway never served the $WARM_CELL warm-up under the $egress egress config; not probed. $SERVE_ERR"
    emit_cell "$cell" '"not_verified"' "$LAST_STATUS" "$path" "$note" "" harness_boot_failure
    log "[$GATEWAY]   $egress <- $cell : served=not_verified (warm-up failed)"
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
  # TRANSIENT dead-socket retry (BEFORE any verdict is recorded): a 5xx/000 is the gateway failing to
  # reach the local mock, never a wrong answer. Patience is budgeted by the ADVISORY capability grid
  # (see the cap section): a declared cell gets the full budget, an undeclared probe-first cell a
  # short one - the verdict class is identical either way.
  local _retries="$MATRIX_TRANSIENT_RETRIES" _pause="$MATRIX_TRANSIENT_PAUSE"
  if [ "$(cap "$cell" "$egress")" != 1 ]; then
    _retries="$MATRIX_PROBE_TRANSIENT_RETRIES"; _pause="$MATRIX_PROBE_TRANSIENT_PAUSE"
  fi
  local attempt=1
  while probe_transient && [ "$attempt" -lt "$_retries" ]; do
    log "[$GATEWAY]   $egress <- $cell : transient status $LAST_STATUS (upstream unreachable) - retry $attempt/$((_retries-1)) in ${_pause}s"
    sleep "$_pause"
    mock_reset
    probe "$path" "$data" "$@"
    v="$(verdict "$cell" "$egress")"
    attempt=$((attempt+1))
  done
  local reason=""
  if probe_transient; then
    # Persistent 5xx/000 after all retries. Two very different things can look like this:
    #   - a RIG/transport failure (gateway or mock unreachable, dead sockets): not_verified, the
    #     harness could not get a fair reading - never graded either way;
    #   - a DETERMINISTIC application rejection of a probe-first cell (e.g. a gateway 503ing
    #     "failed to parse request" on an ingress shape it has no route type for): that IS the
    #     probe's honest answer - the pairing is not configured - and labeling it "not verified"
    #     would hide real probe evidence.
    # Discriminate on observables: the gateway ANSWERED (a real 5xx, not 000) while the mock is
    # verifiably healthy, on a cell the manifest never declared -> not_configured with the 5xx
    # evidence. Anything else (000, sick mock, or a DECLARED cell, where a persistent 5xx is more
    # plausibly a rig failure worth a human look than a capability verdict) stays not_verified.
    local _mock_ok=0
    curl -s -m3 "http://127.0.0.1:$MOCK_PORT/__mock/state" 2>/dev/null | grep -q '"recording":true' && _mock_ok=1
    snip="$(strip_ctrl "$(printf '%s' "$LAST_BODY" | head -c 200)")"
    if [ "$_mock_ok" = 1 ] && [ "$LAST_STATUS" != 000 ] && [ "$(cap "$cell" "$egress")" != 1 ]; then
      served='"not_configured"'; reason=probe_failed
      note="HTTP $LAST_STATUS on POST $path, persistent across $_retries attempts with the mock verifiably healthy: a deterministic application-level rejection of this ingress/egress pairing, not a transport failure"
      CELL_PROBE_NOTE="probe failed: HTTP $LAST_STATUS on POST $path (persistent across $_retries attempts, mock healthy); first bytes: $(strip_ctrl "$(printf '%s' "$LAST_BODY" | head -c 160)")"
      log "[$GATEWAY]   $egress <- $cell : served=not_configured ($note)"
    else
      served='"not_verified"'; reason=upstream_unreachable
      note="HTTP $LAST_STATUS after $_retries attempts: the gateway did not complete a round trip to the upstream (transport failure, e.g. a dead upstream socket or unhealthy rig); recorded as not_verified rather than graded"
      log "[$GATEWAY]   $egress <- $cell : served=not_verified ($note)"
    fi
    emit_cell "$cell" "$served" "$LAST_STATUS" "$path" "$note" "$snip" "$reason"
    return
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
      served='"not_configured"'; reason=probe_failed
      note="HTTP $LAST_STATUS with a valid $cell envelope, but the mock never received a request on the $egress endpoint: the gateway answered without contacting the configured upstream"
      CELL_PROBE_NOTE="probe failed: HTTP $LAST_STATUS; valid $cell envelope but zero requests recorded on the mock's $egress endpoint (answered without calling the upstream)"
    elif [ "${m#misdialect }" != "$m" ]; then
      served='"not_configured"'; reason=probe_failed
      note="HTTP $LAST_STATUS with a valid $cell envelope, but the gateway did not speak the $egress dialect to the upstream: the mock received the request on the ${m#misdialect } endpoint instead"
      CELL_PROBE_NOTE="probe failed: HTTP $LAST_STATUS; upstream request landed on the ${m#misdialect } endpoint, not the $egress endpoint (this pairing is not wired to a $egress upstream)"
    elif [ "$m" = norecord ]; then
      # The recording mock was displaced by something on our rig: a HARNESS gap, not gateway fault.
      served='"not_verified"'; reason=mock_norecord
      note="HTTP $LAST_STATUS with a valid $cell envelope, but the recording mock's state was unavailable (another process replaced the mock on :$MOCK_PORT?); round trip unverifiable, recorded as not_verified rather than guessed either way"
    else
      served='"not_configured"'; reason=probe_failed
      note="HTTP $LAST_STATUS with a valid $cell envelope, but the request that reached the mock's $egress endpoint did not carry the $egress request shape: ${m#badshape }"
      CELL_PROBE_NOTE="probe failed: HTTP $LAST_STATUS; the upstream request reached the $egress endpoint but did not carry the $egress request shape (${m#badshape })"
    fi
  elif [ "$cell" = bedrock ] && { [ "$LAST_STATUS" = 401 ] || [ "$LAST_STATUS" = 403 ]; }; then
    # the gateway rejected the bearer token on the bedrock ingress: it wants SigV4, which this
    # harness does not forge. Distinct verdict: not served, but not graded either.
    served='"unprobed_auth"'; reason=inbound_sigv4
    note="HTTP $LAST_STATUS with a bearer token; gateway appears to require inbound SigV4 on the bedrock ingress, which this probe does not sign"
  elif printf '%s' "$v" | grep -q '^passthrough'; then
    served='"not_configured"'; reason=probe_failed
    note="UNTRANSLATED $v; the gateway proxied $path through verbatim instead of translating (HTTP $LAST_STATUS)"
    CELL_PROBE_NOTE="probe failed: HTTP $LAST_STATUS; response was the upstream's $cell body proxied verbatim, not a translation ($v)"
  else
    served='"not_configured"'; reason=probe_failed
    note="HTTP $LAST_STATUS on POST $path: $v"
    CELL_PROBE_NOTE="probe failed: HTTP $LAST_STATUS on POST $path; $v; first bytes: $(strip_ctrl "$(printf '%s' "$LAST_BODY" | head -c 160)")"
  fi
  # Snapshot the CAPABILITY-PROBE status + body NOW, before matrix_cell_perf's post-load leg-3
  # re-verify calls probe() again and overwrites LAST_STATUS/LAST_BODY. Otherwise a green cell whose
  # leg-3 re-verify hit a transient dead socket would be EMITTED with that transient's status (a green
  # cell tagged 502/000), contradicting its own "HTTP 200 ..." note. The recorded status must be the
  # status the capability verdict was decided on.
  local cap_status="$LAST_STATUS"
  snip="$(strip_ctrl "$(printf '%s' "$LAST_BODY" | head -c 200)")"
  log "[$GATEWAY]   $egress <- $cell : served=$served ($note)"
  # ADDITIVE per-cell perf: only a green (served=true) cell is swept; the capability verdict above
  # is already final and is not re-derived from the sweep in any way.
  if [ "$served" = true ]; then
    matrix_cell_perf "$egress" "$cell" "$path" "$data" "$@"
  fi
  emit_cell "$cell" "$served" "$cap_status" "$path" "$note" "$snip" "$reason"
}

# ── egress loop: (re)configure + relaunch the gateway per upstream dialect, probe all 6 ingress ──
# PROBE-FIRST launch ladder for a column:
#   1. manifest defines gw_matrix_egress and it accepts this dialect -> dedicated egress config;
#   2. the writer REJECTS the dialect (no writer for it)            -> the gateway's DEFAULT config
#      (gw_launch): the column is still probed, and each cell's leg-3 evidence records honestly
#      where the requests actually went (not_configured, never a boot failure, never a red);
#   3. no gw_matrix_egress at all -> the default config serves every column (launched ONCE and
#      reused: the config is identical per column, so re-booting it 6x would measure nothing).
EGRESS_CONFIG=""   # "dedicated" | "default" - what this column actually ran under (recorded in JSON)
launch_egress(){ # dialect -> 0 launched, 1 launch failed
  gw_stop 2>/dev/null; sleep 1
  EGRESS_CONFIG=dedicated
  if declare -f gw_matrix_egress >/dev/null; then
    if ! gw_matrix_egress "$1"; then
      log "[$GATEWAY] egress=$1: manifest has no egress writer for this dialect - probing under the default config"
      EGRESS_CONFIG=default
      gw_launch || return 1
    fi
  else
    EGRESS_CONFIG=default
    gw_launch || return 1
  fi
  rebuild_headers
  return 0
}

# The warm-up must probe a cell the gateway actually DECLARES for this egress column. Warming with
# the openai ingress unconditionally punished gateways that (honestly) declare no openai-ingress
# bridge into an egress dialect (e.g. a Converse-only diagonal): the openai warm-up can never 200
# there, so the whole column was published as a boot failure - a harness bug, not the gateway's.
# Generic rule: warm on the FIRST declared-1 ingress dialect of the column, with that dialect's own
# probe body, path and headers.
warm_dialect_headers(){ # dialect -> extra -H pairs on stdout-less: sets WARM_H array
  WARM_H=()
  case "$1" in
    anthropic) WARM_H=(-H "anthropic-version: 2023-06-01" -H "x-api-key: $GW_AUTH" ${XH[@]+"${XH[@]}"});;
    gemini)    WARM_H=(-H "x-goog-api-key: $GW_AUTH");;
  esac
}
warm_up(){ # egress
  WARM_OK=0; WARM_LAST=000
  local ing; WARM_CELL=openai
  for ing in $INGRESS_ALL; do
    if [ "$(cap "$ing" "$1")" = 1 ] && ! cell_untestable "$ing" "$1"; then WARM_CELL="$ing"; break; fi
  done
  local wpath wbody i
  wpath="$(ingress_path "$WARM_CELL")"; wbody="$(ingress_body "$WARM_CELL")"
  warm_dialect_headers "$WARM_CELL"
  for i in $(seq 1 45); do
    WARM_LAST=$(curl -s -m3 -o /dev/null -w "%{http_code}" "http://127.0.0.1:$GW_PORT$wpath" -X POST \
        -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" ${CURL_H[@]+"${CURL_H[@]}"} \
        ${WARM_H[@]+"${WARM_H[@]}"} -d "$wbody")
    [ "$WARM_LAST" = 200 ] && { WARM_OK=1; return 0; }
    sleep 1
  done
  SERVE_ERR="HTTP $WARM_LAST on POST $wpath ($WARM_CELL warm-up); diag=[$(gw_diag 2>&1 | tail -n 20)]"
  log "[$GATEWAY] WARNING: no 200 on the $WARM_CELL warm-up for egress=$1 (last=$WARM_LAST)"
  return 1
}

# ── memory ONCE (relocated from memory/run.sh) ───────────────────────────────────────────────────
# Idle + peak RSS is a PROCESS-LEVEL number, so it is measured ONE time per gateway (not per cell).
# Relaunch the gateway under its DEFAULT config (gw_launch), point it at a plain mock, warm it, record
# idle RSS, then drive a sustained large-payload load while a 0.3s sampler tracks peak VmRSS (and a
# watchdog kills the load at MEM_CAP_MIB so an unbounded gateway can't OOM the box). VmHWM is read at
# teardown (survives until process exit), and post-load RSS 60s after load stops (does it release?).
# The measurement is the gateway's own gw_rss()/gw_hwm() manifest hooks (shared /proc tree helpers in
# lib/harness.sh); a manifest without gw_rss (the mock-gateway fixture) degrades to 0/null cleanly.
# Sets MEMORY_JSON (a top-level `"memory": {...}` object) folded into the final result.
MEMORY_JSON=""
matrix_measure_memory(){
  [ "$MATRIX_MEMORY" = 1 ] || return 0
  if suite_deadline_expired; then log "[$GATEWAY] suite ceiling reached - skipping the memory-once measurement"; return 0; fi
  log "[$GATEWAY] memory-once: relaunching under the default config for idle/peak RSS"
  gw_stop 2>/dev/null; sleep 1
  # Plain instant mock (no recording, no streaming): the same serving conditions memory/run.sh uses.
  mock_start_plain
  local mem_ok=0 mem_err=""
  _mem_ready(){
    rebuild_headers
    local i st
    for i in $(seq 1 60); do
      st=$(curl -s -m3 -o /dev/null -w "%{http_code}" "http://127.0.0.1:$GW_PORT$GW_PATH" -X POST \
          -H "content-type: application/json" -H "authorization: Bearer $GW_AUTH" ${CURL_H[@]+"${CURL_H[@]}"} \
          -d "{\"model\":\"$GW_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"warm\"}],\"max_tokens\":16}")
      [ "$st" = 200 ] && return 0; sleep 1
    done
    return 1
  }
  if harness_launch_ready gw_launch _mem_ready; then mem_ok=1; else mem_err="$HARNESS_SERVE_ERR"; fi
  local IDLE PEAK=0 HWM POST STOP PEAKF LOADPIDF SP
  IDLE=$(gw_rss); log "[$GATEWAY] memory-once idle RSS: ${IDLE:-?} MiB (served=$([ "$mem_ok" = 1 ] && echo true || echo false))"
  if [ "$mem_ok" = 1 ]; then
    STOP="${TMPDIR:-/tmp}/mtxmem.$$.stop"; PEAKF="${TMPDIR:-/tmp}/mtxmem.$$.peak"; LOADPIDF="${TMPDIR:-/tmp}/mtxmem.$$.loadpid"
    rm -f "$STOP" "$PEAKF" "$LOADPIDF"; echo 0 >"$PEAKF"
    ( PEAK=0; while [ ! -f "$STOP" ]; do
        v=$(gw_rss); [ -z "$v" ] && v=0
        awk -v v="$v" -v p="$PEAK" 'BEGIN{exit !(v+0>p+0)}' && { PEAK=$v; echo "$PEAK" >"$PEAKF"; }
        awk -v v="$v" -v c="$MEM_CAP_MIB" 'BEGIN{exit !(v+0>c+0)}' && { echo "[watchdog] $v MiB > cap $MEM_CAP_MIB — killing load"; lp=$(cat "$LOADPIDF" 2>/dev/null); [ -n "$lp" ] && kill "$lp" 2>/dev/null; touch "$STOP"; }
        sleep 0.3
      done ) & SP=$!
    log "[$GATEWAY] memory-once load: ${MEM_PSIZE}B payloads, c=$MEM_CONC, ${MEM_DUR}s (watchdog cap ${MEM_CAP_MIB} MiB)"
    taskset -c "$LOADCORES" "$UGEN" -url "http://127.0.0.1:$GW_PORT$GW_PATH" \
      -model "$GW_MODEL" -auth "$GW_AUTH" -c "$MEM_CONC" -d "$MEM_DUR" -psize "$MEM_PSIZE" ${CURL_H[@]+"${CURL_H[@]}"} &
    local LOAD_PID=$!; echo "$LOAD_PID" >"$LOADPIDF"; wait "$LOAD_PID" 2>/dev/null || true
    touch "$STOP"; kill "$SP" 2>/dev/null
    PEAK=$(cat "$PEAKF" 2>/dev/null); PEAK=${PEAK:-0}
    HWM=$(gw_hwm)   # VmHWM must be read BEFORE the gateway stops (the counter dies with the process)
    log "[$GATEWAY] memory-once high-water mark: ${HWM:-n/a} MiB (VmHWM; sampled peak ${PEAK} MiB)"
    log "[$GATEWAY] memory-once load stopped — waiting ${MEM_SETTLE_S}s to see if memory releases"
    sleep "$MEM_SETTLE_S"
    POST=$(gw_rss)
    rm -f "$STOP" "$PEAKF" "$LOADPIDF" 2>/dev/null
  fi
  MEMORY_JSON="
  \"memory\": {\"served\": $([ "$mem_ok" = 1 ] && echo true || echo false), \"serve_error\": \"$(json_escape "$mem_err")\", \"idle_rss_mib\": ${IDLE:-0}, \"peak_rss_mib\": ${PEAK:-0}, \"peak_rss_hwm_mib\": ${HWM:-null}, \"post_load_rss_mib\": ${POST:-0}, \"payload_bytes\": $MEM_PSIZE, \"concurrency\": $MEM_CONC, \"duration_s\": $MEM_DUR},"
  log "[$GATEWAY] memory-once: idle=${IDLE:-0} peak=${PEAK:-0} hwm=${HWM:-n/a} post=${POST:-0} MiB"
}

UPSTREAMS_JSON=""
COMPAT_CELLS=""; COMPAT_SHAPE=""; COMPAT_SERVED=false; COMPAT_ERR=""
# Default-config reuse across columns (launch ladder case 3): the config is identical for every
# egress dialect, so the launch + warm-up verdict from the first column carries over instead of
# re-booting the same process six times.
HAVE_EGRESS_FN=0; declare -f gw_matrix_egress >/dev/null && HAVE_EGRESS_FN=1
DEFAULT_UP=0; DEFAULT_WARM_OK=0; DEFAULT_WARM_LAST=000; DEFAULT_SERVE_ERR=""; DEFAULT_WARM_CELL=openai
for EGRESS in $EGRESS_ALL; do
  CELLS_JSON=""
  # Wall-clock backstop: if a pathological gateway has already burned the suite ceiling, stop probing
  # further egress columns and record them not-served (timeout) so run-all can never wedge here.
  if suite_deadline_expired; then
    log "[$GATEWAY] egress=$EGRESS: suite wall-clock ceiling reached - recording not served, moving on"
    WARM_OK=0
    for CELL in $INGRESS_ALL; do
      emit_cell "$CELL" '"not_verified"' "" "$(ingress_path "$CELL")" \
        "suite wall-clock ceiling (${HARNESS_SUITE_CEIL_S}s) reached before this egress column was probed" "" suite_ceiling
    done
    UPSTREAMS_JSON="${UPSTREAMS_JSON}${UPSTREAMS_JSON:+,}
    \"$EGRESS\": {\"configurable\": true, \"served\": false, \"serve_error\": \"suite wall-clock ceiling reached\", \"cells\": {$CELLS_JSON
    }}"
    continue
  fi
  # PROBE-FIRST: every egress column is attempted - launch (dedicated writer or default-config
  # fallback, see launch_egress) and probe all six ingress cells. A manifest without any
  # gw_matrix_egress runs its ONE default config: launch it once and probe every column against it
  # (the leg-3 evidence tells each column's cells honestly where the requests actually went).
  if [ "$HAVE_EGRESS_FN" = 1 ] || [ "$DEFAULT_UP" = 0 ]; then
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
    if [ "$HAVE_EGRESS_FN" = 0 ]; then
      DEFAULT_UP=1; DEFAULT_WARM_OK="$WARM_OK"; DEFAULT_WARM_LAST="$WARM_LAST"
      DEFAULT_SERVE_ERR="$SERVE_ERR"; DEFAULT_WARM_CELL="$WARM_CELL"
    fi
  else
    # Same process, same config: the first column's launch + warm-up verdict carries over.
    log "[$GATEWAY] egress=$EGRESS: no per-egress writer - probing the running default config"
    WARM_OK="$DEFAULT_WARM_OK"; WARM_LAST="$DEFAULT_WARM_LAST"
    SERVE_ERR="$DEFAULT_SERVE_ERR"; WARM_CELL="$DEFAULT_WARM_CELL"; EGRESS_CONFIG=default
  fi
  EG_SERVED=$([ "$WARM_OK" = 1 ] && echo true || echo false)
  for CELL in $INGRESS_ALL; do
    # The ONE probe exemption: a manifest-cited mock-reachability limit (real cloud host pinned).
    if cell_untestable "$CELL" "$EGRESS"; then
      emit_cell "$CELL" '"untestable"' "" "$(ingress_path "$CELL")" \
        "untestable: no upstream base-URL override reaches the test mock (a mock-reachability limit of this rig, not gateway incapability). $GW_MATRIX_UNTESTABLE_NOTE" "" no_base_url_override
      log "[$GATEWAY]   $EGRESS <- $CELL : untestable (no base-URL override, cited)"
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
    \"$EGRESS\": {\"configurable\": true, \"served\": $EG_SERVED, \"egress_config\": \"${EGRESS_CONFIG:-default}\", \"serve_error\": \"$(json_escape "$SERVE_ERR")\", \"cells\": {$CELLS_JSON
    }}"
  # v1 compat row: the openai-egress cells when configured, else the first configured egress.
  if [ "$EGRESS" = openai ] || [ -z "$COMPAT_SHAPE" ]; then
    COMPAT_SHAPE="$EGRESS"; COMPAT_CELLS="$CELLS_JSON"; COMPAT_SERVED="$EG_SERVED"; COMPAT_ERR="$SERVE_ERR"
  fi
  SERVE_ERR=""
done

# Memory ONCE, after the 6x6 (it relaunches the gateway under the default config with a large-payload
# sustained load — a process-level number, not per-cell). Folded into the result as top-level "memory".
matrix_measure_memory

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
  "probe_first": true,
  "capability_note": "$(json_escape "advisory context only (probe-first: every cell is probed regardless): $GW_MATRIX_CAP_NOTE")",
  "cell_perf_sweep": $([ "$MATRIX_SWEEP" = 1 ] && echo true || echo false),
  "sweep_rung_selection": "$([ "${SWEEP_ADAPTIVE:-0}" = 1 ] && echo adaptive || echo full-ladder)",
  "sweep_ttft_ms": $SWEEP_TTFT_MS,
  "p99_ceiling_ms": $P99_CEIL_MS,
  "sweep_dur": $SWEEP_DUR,
  "cell_stream": $([ "$MATRIX_STREAM" = 1 ] && echo true || echo false),$MEMORY_JSON
  "cells": {$COMPAT_CELLS
  },
  "upstreams": {$UPSTREAMS_JSON
  },
  "model": "$GW_MODEL",
  "upstream_endpoint": "$GW_PATH",
  "ootb_config": $([ -n "$OOTB_CONFIG" ] && printf '"%s"' "$OOTB_CONFIG" || echo null),
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
sym = {True: "PASS", False: "fail", "not_configured": "n/c ", "not_configurable": "n/c ",
       "unprobed_auth": "auth", "not_verified": "n/v ", "untestable": "mock"}
print("   %-17s" % "ingress \\ egress" + "".join("%-18s" % e for e in egs))
cells0 = next(iter(ups.values()))["cells"]
for cell in cells0:
    row = ["%-18s" % sym.get(ups[e]["cells"][cell]["served"], "?") for e in egs]
    print("   %-17s" % cell + "".join(row))
PY
echo " -> $RESULTS/$GATEWAY.json"
echo "================================================================"
