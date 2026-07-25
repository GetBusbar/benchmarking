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
# The gateway under test. Every field path (run-all.sh, run-on-ec2.sh) passes GATEWAY explicitly;
# this default only ever serves a bare interactive invocation. It is DISCOVERED (the first
# gateways/*/gateway.sh on disk, alphabetical), never a baked-in favourite -- naming one here
# made this runner one of the places a dropped-in gateway was not enough on its own.
# shellcheck source=../lib/gateways.sh
. "$ROOT/lib/gateways.sh"
GATEWAY="${GATEWAY:-$(gw_default "$ROOT")}"
# ── RUN CLOCK ────────────────────────────────────────────────────────────────────────────────────
# How long THIS gateway's suite took on THIS box on THIS day. matrix/run.sh is the sole producer, so
# the script's own lifetime IS the gateway's run: stamp t0 here, at the very top, before any work.
# Published as started_at / finished_at / duration_s (+ a phase_s breakdown) on the matrix json and
# the snapshot. Previously this was only recoverable by parsing the orchestrator log, which is not
# published — so a snapshot could not answer "how long did this gateway take", and a duration that
# jumps run-over-run (slow box, build regression, a sweep taking more rungs) went unnoticed.
# measured_at stays exactly what it was: the single instant the results were stamped, which the board
# ages by. These describe the RUN, not the measurement instant.
RUN_T0=$(date +%s)
RUN_STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
BUILD_S=null; MATRIX_6X6_S=null; MEMORY_WINDOW_S=null   # phases that never ran stay null, never 0
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
# CONCURRENCY SEARCH BOUNDS [min,max] - IDENTICAL for both sweeps, deliberately.
#
# These were 16-8192 for the instant sweep and 32-65536 for the delayed one. Nobody could say why,
# and an arbitrary asymmetry between two numbers printed side by side in the same table is a bug:
# rps_max_proxy is searched with EIGHT TIMES LESS headroom than rps_sustained_20ms, so the peak
# search can terminate on its BOUND rather than on a throughput plateau. That is exactly what
# happened. rps_max_proxy was published as a gateway's maximum while the delayed sweep, allowed to
# climb further, repeatedly beat it: 21 cells across the field reported sustained > max, which was
# written off as measurement noise for weeks. It was not noise. It was this file reporting that our
# own max benchmark was under-searching, and the number labelled "max" was not a max.
#
# A 20ms upstream delay cannot make a CPU-bound gateway faster. With equal headroom the instant
# sweep must therefore be >= the delayed one, which makes sustained <= max_proxy a physical property
# rather than a hope, and any violation a real defect worth failing on (see check-consistency C6).
#
# ONE constant, not two that happen to match. Two knobs required to stay equal is exactly how the
# original asymmetry appeared, so there is now nothing to drift: every sweep in this file searches
# the same range by construction. A future metric needing different bounds must add its own knob
# here WITH the reason, which makes the exception visible instead of accidental.
# mode=peak is an adaptive search, not a fixed ladder, so a low floor costs a few extra probes,
# not a linear scan (lib/sweep.sh).
SWEEP="${SWEEP:-1 65536}"
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
# Search range for the sustained bisect, not a ladder. lo=8: below it scheduler noise swamps the
# signal. hi=2048: a rig bound, not a belief about any gateway - past it the load generator's own
# per-stream cost competes for the 6 pinned load cores, so a failure stops being the gateway's. (fds
# are not the constraint; run-on-ec2.sh raises docker nofile to 1048576.) Still clean at hi means a
# LOWER BOUND was found, and :826 publishes null and says to raise this range, never hi itself.
MATRIX_STREAM_SUST_BOUNDS="${MATRIX_STREAM_SUST_BOUNDS:-8 2048}"   # [lo,hi] for the sustained bisect
MATRIX_STREAM_DELIV="${MATRIX_STREAM_DELIV:-0.999}"               # sustained gate delivered-frames floor
# Unpaced lane (streamcpu/run.sh parity): CPU-bound relay throughput (frames/sec) — long back-to-back
# bursts so per-frame relay cost dominates. cpu-fps is the PEAK of the fps-vs-concurrency curve.
MATRIX_STREAMCPU_CHUNKS="${MATRIX_STREAMCPU_CHUNKS:-512}"
MATRIX_STREAMCPU_FRAME_BYTES="${MATRIX_STREAMCPU_FRAME_BYTES:-16}"
MATRIX_STREAMCPU_STALL_MS="${MATRIX_STREAMCPU_STALL_MS:-250}"
MATRIX_STREAMCPU_DUR="${MATRIX_STREAMCPU_DUR:-16}"
# Search range for the cpu-fps peak. hi=512 is deliberately below the paced lane's 2048: this is the
# UNPACED firehose, each stream costs the load generator more, so the rig runs out sooner and 2048
# would measure the rig. Still rising at hi sets SM_FPS_AT_TOP and publishes null, never hi itself.
MATRIX_STREAMCPU_FPS_BOUNDS="${MATRIX_STREAMCPU_FPS_BOUNDS:-8 512}"  # [lo,hi] for the cpu-fps peak search

# ── memory: ONE OWN-PROCESS WINDOW PER SERVED CELL, inside the 6x6 loop ────────────────────────────
# FINAL PROTOCOL (DESIGN-per-cell-measurement.md, agreed 2026-07-25). Memory is measured for EVERY cell
# the gateway serves, each in its OWN cold-started process, separate from that cell's perf window:
#
#     cold start -> MEM_IDLE_S idle sample -> fixed load until the RSS PLATEAUS (cap MEM_PLATEAU_CAP_S)
#                -> MEM_SETTLE_S recovery sample
#
# WHY PER CELL, AND WHY NO CELL IS EVER CHOSEN. Memory used to be the only metric on the board reduced
# to ONE scalar per gateway, which forced the harness to pick a cell to produce it. Every version of
# that pick was indefensible: picking each gateway's highest-throughput cell selected on throughput and
# reported memory (two unrelated axes, and the candidate set differed per gateway - 26 served cells for
# a broad gateway, 1 for a narrow one); pinning ONE cell for the whole field fixed the workload but
# reported nothing at all for the gateways that do not serve it, and still described a gateway by a
# single point on a curve. The cell IS the workload - an identity lane does no translation, a cross-
# dialect lane pays for one on every request - so a memory column built from different cells compared
# different work. Measuring every served cell removes the selection entirely: there is no fallback
# cell, no peak cell, no preferred basis. A cell the gateway does not serve simply has no memory block,
# which reads as absent. WHAT to display (min / max / one shared dialect) is a display concern, made by
# the reader with the choice disclosed, and never by this harness.
#
# WHY ITS OWN COLD PROCESS, NOT THE PERF ONE. The two measurements want opposite conditions. Perf is an
# adaptive SEARCH over concurrency, so sampling RSS during it would let a faster gateway be driven to
# higher concurrency, hold more per-connection state, and report more memory - throughput leaking into
# the memory number. Memory needs a FIXED load and a genuinely cold process: idle sampled after any
# traffic is "recovered", not "idle". A restart costs ~10s and buys complete independence.
#
# WHY THE LOAD TERMINATES ON A PLATEAU, NOT A CLOCK. A fixed-duration load decides the answer for any
# gateway still climbing when it expires: the published number then describes when we stopped looking
# rather than the gateway, and a longer load would have produced a larger one. The load therefore runs
# until the trailing window is steady by lib/plateau.sh's two-part test (no upward trend AND not merely
# oscillating), or until the cap. HITTING THE CAP IS A PUBLISHED RESULT, NOT AN ERROR: that cell
# reports plateaued:false plus its growth rate, which is the most informative thing this metric can
# say about a leaking gateway.
#
# Per served cell we publish idle / steady_state (null if it never plateaued) / recovered / peak /
# peak-hwm / plateaued / time_to_plateau_s / growth_rate_mib_per_min / the whole rss_series. The growth
# rate is emitted UNCONDITIONALLY, including when the gateway DID plateau, because any threshold admits
# a leak slower than itself: "plateaued: true, growth 1.5 MiB/min" is how a slow leak that cleared the
# bar stays visible to a reader (see lib/plateau.sh's header for the arithmetic).
#
# The load reuses the perf sweep's own generator (lib/sweep.sh sweep_probe_raw -> ugen) against a PLAIN
# high-throughput mock — memory does NOT need the recording mock, which sidesteps the mock-norecord
# showstopper. The measurement is the gateway's own gw_rss()/gw_hwm() manifest hooks (shared /proc-tree
# helpers in lib/harness.sh); a manifest with no gw_rss (the mock-gateway fixture) degrades to null
# cleanly. NULL-SAFE: any RSS the sampler can't obtain -> null, NEVER a fabricated 0. MATRIX_MEMORY=0
# disables the per-cell window (no "memory" object is written into any cell).
#
# COST, ACCEPTED DELIBERATELY. Worst case per cell is MEM_IDLE_S + MEM_PLATEAU_CAP_S + MEM_SETTLE_S =
# 420s at the field defaults, so a gateway that serves all 36 cells and never plateaus pays ~4.2h of
# memory phase. A fast-settling gateway plateaus in tens of seconds and lands near 2.7 min/cell. That
# asymmetry is the right way round: the extra time is spent only where it buys real information.
MATRIX_MEMORY="${MATRIX_MEMORY:-1}"
MEM_CAP_MIB="${MEM_CAP_MIB:-40000}"   # watchdog: log a runaway RSS (never kills the measurement)
# Idle window: how long to sample COLD idle RSS (fresh process, no load) before the fixed load starts.
# Field default 60 s; a local dev verifier shrinks it (MEM_IDLE_S=2) so a minutes-long local run isn't
# dominated by fixed sleeps. idle_rss_mib is the MEDIAN of this window's samples (a settled idle).
MEM_IDLE_S="${MEM_IDLE_S:-60}"
# Recovery (drop-down) window AFTER the load stops: how long to wait before reading recovered RSS (does
# memory release?). Field default 60 s; the dev verifier shrinks it (MEM_SETTLE_S=2). recovered_rss_mib
# is the RSS at the END of this window; post_load_rss_mib is an ALIAS of it (back-compat). It used to be
# emitted as the PEAK value, contradicting both its name and this documented meaning (audit P1-8).
MEM_SETTLE_S="${MEM_SETTLE_S:-60}"
# Series sampling cadence across the window (idle -> load -> recovery). Cheap: one gw_rss read every
# MEM_SAMPLE_S seconds. rss_series_json downsamples to <=~120 points so the series stays bounded.
MEM_SAMPLE_S="${MEM_SAMPLE_S:-2}"
# THE FIXED LOAD RECIPE - IDENTICAL for every gateway AND every cell. A constant that DEFINES THE
# CONDITION is legitimate (the workload must be identical for a comparison to mean anything); a
# constant that decides WHEN TO STOP LOOKING is not, which is why there is no duration here. MATRIX_MEM_*
# are the canonical knobs; the older MEM_CONC/MEM_PSIZE names are honoured as aliases (verify-local
# already threads those) so an existing dev profile keeps working unchanged.
MATRIX_MEM_CONC="${MATRIX_MEM_CONC:-${MEM_CONC:-64}}"          # concurrency of the fixed memory load
MATRIX_MEM_PAYLOAD="${MATRIX_MEM_PAYLOAD:-${MEM_PSIZE:-4096}}" # per-request payload bytes (injected into the body)
# NO LOAD DURATION CONSTANT. The memory load runs until the RSS is STEADY, not for a fixed time. A
# fixed duration decides the answer for any gateway still climbing when it expires: the number then
# describes when we stopped looking rather than the gateway. See DESIGN-per-cell-measurement.md.
# 60, NOT 30. This shipped as 30 in 2eb3050 - a bare -60/+30 hunk inside an otherwise-unchanged comment
# block, unmentioned in the commit body and recorded nowhere else - while DESIGN-per-cell-measurement.md
# (agreed with Matthew) says 60 and "30 samples per window", and lib/plateau.sh's calibration argument
# quotes the slope a 60s window admits. The window sets the certification bar in BOTH gates: for a linear
# slope s, drift% = s*(w/2)/mean*100 < 1 and spread% = s*w/mean*100 < 2 give the SAME bound, so halving
# the window doubles the leak rate that passes as steady - on a 120 MiB base, 2.4 MiB/min at 60s against
# 4.8 MiB/min (~288 MiB/hour) at 30s. Nothing published was false (every artifact self-describes its own
# plateau_window_s and the growth rate is emitted either way), but a certification bar 2x looser than the
# agreed one, with no record, is not a thing to launch a 7-hour run on. Cost of restoring it: a plateau
# cannot be declared before 60s of load, so a fast-settling cell pays about +30s, roughly +18 min on a
# fully-served gateway against an 8h ceiling. Replayed against the last field run's real series, the only
# verdict this changes is portkey's, and portkey was still ramping when that 120s load stopped.
MEM_PLATEAU_WINDOW_S="${MEM_PLATEAU_WINDOW_S:-60}"   # trailing window the steadiness test runs over
MEM_PLATEAU_TREND_PCT="${MEM_PLATEAU_TREND_PCT:-1}"  # max upward drift, 2nd-half mean vs 1st-half mean
MEM_PLATEAU_RANGE_PCT="${MEM_PLATEAU_RANGE_PCT:-2}"  # max (max-min) spread inside the window
MEM_PLATEAU_CAP_S="${MEM_PLATEAU_CAP_S:-300}"        # hard cap; hitting it is a REPORTED result
# LOAD LEG LENGTH - the granularity at which the plateau decision is taken, and it exists only because
# of how the load generator reports. ugen prints its stats line (ok=, the proof that the load was
# actually delivered) ONLY when it finishes: a generator killed the instant a plateau is detected exits
# with no stats at all, and a load that delivered NOTHING produces a perfectly flat RSS curve, which is
# exactly what the steadiness test would certify as a plateau. So the load is delivered as back-to-back
# legs of this length, each one carrying its own delivery evidence, and the plateau is evaluated
# between legs. The leg is clamped to MEM_PLATEAU_WINDOW_S below so the decision can never overshoot by
# more than one steadiness window.
MEM_LOAD_LEG_S="${MEM_LOAD_LEG_S:-10}"
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
# default suite ceiling rises from harness.sh's 45 min to 8 h when the sweep+stream are on. An
# explicit HARNESS_SUITE_CEIL_S still wins, and the ceiling still backstops a wedged gateway.
#
# WHY 8 h AND NOT 6 h: the ceiling is a WEDGE BACKSTOP, not a budget. Its only job is to stop a hung
# gateway from holding a box forever; it must never be the thing that ends a HEALTHY run, because a
# ceiling-truncated suite silently drops the tail cells and publishes a partial grid as if it were a
# whole one. The slowest gateway measured ~6.1 h against the old 6 h ceiling, i.e. the backstop had
# crossed into the working range and was about to censor a gateway for being slow rather than wedged.
# 8 h restores the separation. The cost of being generous is idle box-minutes on a genuinely wedged
# run; the cost of being tight is a truncated grid we would not notice.
[ "$MATRIX_SWEEP" = 1 ] && HARNESS_SUITE_CEIL_S="${HARNESS_SUITE_CEIL_S:-28800}"
# shellcheck source=/dev/null
source "$ROOT/lib/harness.sh"
# shellcheck source=/dev/null
source "$ROOT/lib/sweep.sh"
# shellcheck source=/dev/null
source "$ROOT/lib/stream_measure.sh"
# The steadiness test that terminates every per-cell memory load (plateau_check / plateau_growth_rate /
# plateau_window). It is its own file with its own RED/GREEN suite (lib/plateau_test.sh) because the
# decision "has this stopped moving?" is the whole meaning of steady_state_rss_mib and must be testable
# without booting a gateway.
# shellcheck source=/dev/null
source "$ROOT/lib/plateau.sh"
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
# per-cell hints). It no longer gates probing. It informs exactly ONE harness-side choice:
#   - warm-up preference: each egress column warms on the first advisory-declared ingress (a cell
#     expected to 200), falling back to openai. This is "which cell do we spend a warm-up on", never
#     "what did we see" - it cannot reach any published verdict.
# It used to inform a second one, and that was a FAIRNESS DEFECT, now fixed: transient patience was
# 3x120s on a declared cell and 2x10s on an undeclared one, and the persistent-transient verdict for
# the SAME observation was not_verified when declared / not_configured when undeclared. A gateway's
# own unverified claim therefore moved both the effort spent measuring it and the verdict published
# about it. Both now come from lib/probe_verdict.sh, which cannot see the declaration at all.
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

# Per-cell ingress paths: ONE choke point, lib/ingress.sh (sourced AFTER the manifest so it sees the
# manifest's GW_PATH / GW_MODEL / GW_MATRIX_PATH_* overrides). ingress_path() resolves its defaults at
# CALL time on purpose: gemini and bedrock carry the model in the URL PATH, and gw_matrix_egress flips
# GW_MODEL per egress column, so a path frozen at init made those cells impossible to pass. See the
# header of lib/ingress.sh and the regression guard lib/ingress_path_test.sh.
# shellcheck source=/dev/null
source "$ROOT/lib/ingress.sh"

# The capability probes need the RECORDING mock (instant, MOCK_RECORD=1: leg 3 evidence); the
# per-cell perf sweep needs the exact serving conditions perf/run.sh measures under (no recording,
# run_sweep restarts it per delay). These helpers flip between the two; every capability probe is
# preceded by mock_start_record, so a sweep can never leave a non-recording mock in front of a
# leg-3 check (mock_hit would honestly report "norecord" even if one slipped through).
mock_start_record(){
  # audit #21: WAIT for the old mock to actually exit and release the port. A blind `sleep 1`
  # after an async pkill let the fresh mock panic on EADDRINUSE (48/75 cells lost their
  # streaming to "stream_mock_unready" in the 2026-07-25 run). See lib/harness.sh.
  mock_stop_wait || log "WARNING: :$MOCK_PORT still occupied after mock_stop_wait — the relaunch below may fail to bind"
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
  # audit #21: WAIT for the old mock to actually exit and release the port. A blind `sleep 1`
  # after an async pkill let the fresh mock panic on EADDRINUSE (48/75 cells lost their
  # streaming to "stream_mock_unready" in the 2026-07-25 run). See lib/harness.sh.
  mock_stop_wait || log "WARNING: :$MOCK_PORT still occupied after mock_stop_wait — the relaunch below may fail to bind"
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
# LOW-6: the memory-once RSS sampler runs as a background subshell ( … ) & whose STOP-file + PID must be
# reachable from the EXIT/INT/TERM trap. matrix_measure_memory used LOCAL vars for these, so on a ctrl-C /
# watchdog SIGKILL mid-sampling cleanup() could not stop the poller — it orphaned and busy-polled gw_rss
# every 0.3s (the retired memory/run.sh touch/kill'd exactly this; the matrix port dropped it). Promote
# the STOP file + sampler PID to FILE SCOPE so cleanup() can stop them, mirroring the retired suite.
MEM_STOP="" MEM_SP=""
cleanup(){
  gw_stop 2>/dev/null
  [ -n "$MOCK" ] && pkill -f "$MOCK" 2>/dev/null
  # Stop an in-flight memory sampler (LOW-6): signal via its STOP file, then hard-kill the poller PID.
  [ -n "$MEM_STOP" ] && touch "$MEM_STOP" 2>/dev/null
  [ -n "$MEM_SP" ] && kill "$MEM_SP" 2>/dev/null
}
trap cleanup EXIT INT TERM

_PHASE_T0=$(date +%s)
log "[$GATEWAY] build"; gw_build || { echo "build failed"; exit 1; }
BUILD_S=$(( $(date +%s) - _PHASE_T0 ))
# ── THE BUILD/MEASURE BOUNDARY (environment parity) ──────────────────────────────────────────────
# Three of the thirteen manifests build from source and pull their own toolchain in gw_prereqs (aisix
# + helicone: rust/git/build deps; litellm-rust: those plus a ~564MB python venv), because no usable
# arm64 image exists for them. That means three boxes carry software the other ten never see, on a
# benchmark whose premise is "same box, same load, one gateway at a time". Each box runs exactly ONE
# gateway and every gw_prereqs runs inside gw_build, so nothing installed is resident when we start
# measuring — but that is a claim about this box RIGHT NOW, so verify it and publish the answer here
# rather than asserting it in a comment. Uniform: every gateway crosses the same boundary and gets
# the same evidence field, and the ten with no prereqs simply record "quiesced". See the gw_prereqs
# contract in lib/harness.sh for the full per-gateway disposition.
BUILD_QUIESCE="$(harness_build_quiesce)"
case "$BUILD_QUIESCE" in
  quiesced) log "[$GATEWAY] build/measure boundary: build tooling quiesced" ;;
  *) log "[$GATEWAY] build/measure boundary: $BUILD_QUIESCE (recorded in the run JSON)" ;;
esac
# From here on the measurement environment is FROZEN: no manifest hook may install anything.
harness_seal_prereqs
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

# ── manifest BASELINE: the values every egress column starts from ────────────────────────────────
# Same bug class as the frozen ingress paths above, from the other end: gw_matrix_egress MUTATES the
# manifest's own routing state in place (GW_MODEL for most, GW_HEADERS for portkey) and nothing ever
# put it back, so column N+1 inherited column N's mutation. A manifest that derives its per-column
# model from the current value — litellm-python's `GW_MODEL="${GW_MODEL}-anthropic"` — therefore
# COMPOUNDED across the 6x6: the published run recorded model
# "gpt-4o-mini-responses-anthropic-gemini-cohere-bedrock-responses", i.e. every column after the first
# asked for a model that does not exist. Snapshot the baseline ONCE (after gw_build, before any
# launch) and restore it in launch_egress — the one place a column begins — so each column sees
# exactly what the manifest declared, and the published "model" is the manifest's, not the residue of
# whichever column happened to run last.
GW_MODEL_BASE="${GW_MODEL:-}"
GW_HEADERS_BASE=(${GW_HEADERS[@]+"${GW_HEADERS[@]}"})
restore_manifest_baseline(){
  GW_MODEL="$GW_MODEL_BASE"
  GW_HEADERS=(${GW_HEADERS_BASE[@]+"${GW_HEADERS_BASE[@]}"})
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

# TRANSIENT (000/5xx) classification, THE single retry budget, and the persistent-transient verdict:
# one choke point in lib/probe_verdict.sh, so neither the effort spent on a cell nor the verdict
# published for it can ever depend on the gateway's own advisory capability declaration again.
# (It used to: a declared cell got 3x120s and `not_verified`, an undeclared one 2x10s and
# `not_configured`, for the very same observation.) Guarded by lib/probe_verdict_test.sh.
source "$ROOT/lib/probe_verdict.sh"

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
  # THE PROBE PARAMETERS BELONG WITH THE MOCK SHAPE, NOT WITH THE LANE THAT CONSUMES THEM. These two
  # used to be set further down, at the head of the paced lane - but stream_mock_ready (immediately
  # below) probes through stream_probe, which reads SM_EXPFRAMES/SM_STALL_US. Under `set -u` the probe
  # therefore died on an unbound variable BEFORE it ever contacted the mock, printed nothing, and the
  # readiness loop read an empty fps as "not ready" for all 30 tries. Every cell the mock CAN stream was
  # then published as untestable/stream_mock_unready - a RIG excuse standing in for a bug in this file,
  # which is the worst possible failure shape: the streaming lane vanished from the whole board while
  # the artifacts explained the absence as somebody else's fault. They are set here, adjacent to the
  # stream_mock_start whose shape they describe, so a probe can never precede its own parameters.
  SM_EXPFRAMES="$MATRIX_STREAM_CHUNKS"; SM_STALL_US=$(( MATRIX_STREAM_INTERVAL_MS * MATRIX_STREAM_STALL_X * 1000 ))
  # HIGH-6 / HIGH-2: after a mock restart (which pkilled the prior cell's mock and relaunched before it
  # had necessarily exited), the fresh mock can still be racing the dying one for MOCK_PORT — a single un-retried
  # SSE probe below would then read no `data:` frames and fabricate stream_served:false BEFORE any
  # measurement. Gate on stream_mock_ready (a retried 1-stream liveness probe) first, exactly as the
  # sustained-bisect + cpu-fps lanes already do before their searches.
  #
  # HIGH-2: the old code WARN-and-PROCEEDED here — on a lost MOCK_PORT race it logged, then fell through
  # to the served-gating curl against a DEAD port, read no frames, and recorded stream_served:false for a
  # gateway that streams fine (a false "did not stream" published on the public board). That is a RIG
  # readiness failure, NOT a gateway fault. Mirror the recording-mock-restore path's FATAL/abort
  # discipline (:650-655): stream_mock_ready ALREADY retries a bounded loop internally, so if it STILL
  # cannot bind the mock, do NOT run the fabricating probe. Record the cell's streaming as UNTESTABLE
  # (rig-unavailable), leaving it unmeasured — never a gateway stream_served:false.
  if ! stream_mock_ready 30; then
    # FINDING 18: stream_mock_ready probes $DURL — the mock's DIRECT path for THIS cell's ingress dialect
    # (mock_direct_path "$cell"). The mock only synthesizes native SSE for the openai + anthropic direct
    # dialects (mock_streams_egress); for a gemini/cohere/bedrock/responses direct dialect it returns plain
    # JSON on stream:true, so the readiness probe reads fps=0 and fails EVERY time — not because of a lost
    # MOCK_PORT race. Publishing "stream_mock_unready / lost MOCK_PORT race" for such a cell mislabels an
    # ingress-dialect rig limit as a transient port race. Distinguish the two: a dialect the mock does not
    # SSE-stream at all gets its own honest reason; only a dialect the mock CAN stream gets the race reason.
    if ! mock_streams_egress "$cell"; then
      CELL_STREAM_JSON=", \"stream\": {\"stream_served\": \"untestable\", \"reason\": \"mock_no_sse_for_ingress\", \"stream_error\": \"$(json_escape "the test mock does not synthesize an SSE direct-path stream for the $cell ingress dialect (only the openai + anthropic direct paths stream; gemini/cohere/bedrock/responses return plain JSON on stream:true), so the direct-to-mock streaming baseline is unreachable through this rig — a mock-reachability limit for this ingress dialect, not a gateway fault")\"}"
      log "[$GATEWAY]   $cell : stream untestable (mock does not SSE-stream the $cell ingress direct path)"
      return 0
    fi
    log "[$GATEWAY]   $cell : stream mock did not become ready (lost MOCK_PORT race) — marking streaming UNTESTABLE (rig readiness failure), NOT fabricating stream_served:false"
    CELL_STREAM_JSON=", \"stream\": {\"stream_served\": \"untestable\", \"reason\": \"stream_mock_unready\", \"stream_error\": \"$(json_escape "the streaming mock did not rebind :$MOCK_PORT after a per-cell restart (a lost MOCK_PORT race: the previous cell's mock had not yet exited, so the fresh one could not bind); probing a dead port would fabricate a false stream_served:false, so this cell's streaming is left UNMEASURED (a rig-readiness limit, not a gateway fault)")\"}"
    return 0
  fi
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
  # SM_EXPFRAMES/SM_STALL_US are already set, up beside stream_mock_start - see the note there.
  SM_C1_DUR="$MATRIX_STREAM_C1_DUR"; SM_SWEEP_DUR="$MATRIX_STREAM_SWEEP_DUR"; SM_DELIV="$MATRIX_STREAM_DELIV"
  stream_c1
  local lat_c1_ok="$SM_C1_OK"
  local add_t50=$SM_ADD_T50 add_t99=$SM_ADD_T99 add_g50=$SM_ADD_G50 add_g99=$SM_ADD_G99 c1note=""
  if [ "$lat_c1_ok" != 1 ]; then
    add_t50=null; add_t99=null; add_g50=null; add_g99=null
    c1note=", \"stream_c1_note\": \"$(json_escape "$SM_C1_ERR")\""
  fi
  # LOW-1: symmetrize with the initial served-gate at the top of this function. If the paced mock lost
  # :MOCK_PORT between c1 and the sustained bisect (a lost MOCK_PORT race: the previous cell's mock had not yet exited, so the fresh one could not bind), the bisect
  # would probe a DEAD port and fabricate streams_sustained:0 / streams_sustained_mock_bound:null under
  # stream_served:true — a self-contradictory raw record. stream_mock_ready ALREADY retries a bounded
  # loop; if it STILL cannot bind, do NOT run the fabricating search — record the cell's streaming as
  # UNTESTABLE (rig readiness failure), exactly as the initial gate does, rather than served:true with a
  # dead-mock 0/null. Never publish a gateway stream_served:false or a dishonest 0 for a rig limit.
  if ! stream_mock_ready 30; then
    log "[$GATEWAY]   $cell : stream mock did not rebind before the sustained bisect (lost MOCK_PORT race) — marking streaming UNTESTABLE (rig readiness failure), NOT fabricating streams_sustained:0/mock_bound:null under stream_served:true"
    CELL_STREAM_JSON=", \"stream\": {\"stream_served\": \"untestable\", \"reason\": \"stream_mock_unready\", \"stream_error\": \"$(json_escape "the streaming mock did not rebind :$MOCK_PORT before the sustained bisect (a lost MOCK_PORT race: the previous cell's mock had not yet exited, so the fresh one could not bind); searching against a dead port would fabricate streams_sustained:0 with a null mock_bound under stream_served:true, so this cell's streaming is left UNMEASURED (a rig-readiness limit, not a gateway fault)")\"}"
    return 0
  fi
  local sust_lo sust_hi; read -r sust_lo sust_hi <<< "$MATRIX_STREAM_SUST_BOUNDS"
  stream_sustained_bisect "$sust_lo" "$sust_hi"
  local sust_streams=$SM_SUST_STREAMS sust_fps=$SM_SUST_FPS sust_bound=$SM_MOCK_BOUND sust_json="$SM_JSON_ACC"
  # UNMEASURED != MEASURED FAILURE (audit P1-12). stream_sustained_bisect returns 0 both when nothing
  # sustained a stall-free stream (a real, publishable gateway result) and when the suite wall-clock
  # ceiling fired before a single rung was probed (a rig limit we must not attribute to the gateway).
  # SM_SUST_MEASURED distinguishes them: unmeasured publishes null, so the board renders "not measured"
  # rather than a certified zero.
  if [ "${SM_SUST_MEASURED:-0}" != 1 ]; then
    sust_streams=null; sust_fps=null; sust_bound=null
    log "[$GATEWAY]   $cell : sustained-streams NOT MEASURED (search aborted before any rung was probed) - publishing null, not 0"
  elif [ "${SM_SUST_AT_TOP:-0}" = 1 ]; then
    # A LOWER BOUND IS NOT A CEILING. The gateway was still clean at the top of the search range, so this
    # run did not find its limit. Publishing the bound would report the SEARCH RANGE as the gateway's
    # capability, and every gateway above the bound would land on the identical number - which is exactly
    # how the retired fixed-ladder stream suite published an identical "512 streams" for five gateways of
    # different languages. Unmeasured is absent, never substituted, so it publishes null with the reason.
    log "[$GATEWAY]   $cell : sustained-streams exceeded the top of the search range (c=$sust_hi still clean) - publishing null, not the bound; raise MATRIX_STREAM_SUST_BOUNDS and re-run this gateway"
    sust_streams=null; sust_fps=null; sust_bound=null
  fi
  # ── UNPACED lane: cpu-fps peak ──
  stream_mock_start "$MATRIX_STREAMCPU_CHUNKS" 0 "$MATRIX_STREAMCPU_FRAME_BYTES"
  SM_EXPFRAMES="$MATRIX_STREAMCPU_CHUNKS"; SM_STALL_US=$(( MATRIX_STREAMCPU_STALL_MS * 1000 ))
  SM_SWEEP_DUR="$MATRIX_STREAMCPU_DUR"; SM_DELIV="${MATRIX_STREAMCPU_DELIV:-0.5}"
  # LOW-1: same symmetry for the UNPACED cpu-fps lane. If the unpaced mock did not rebind :MOCK_PORT
  # before the cpu-fps peak, running streamcpu_peak_fps against a dead port fabricates cpu_fps:0 /
  # cpu_fps_mock_bound:null under stream_served:true. Take the untestable path instead — the paced lane
  # above already returned untestable on its own dead-mock race, so a dead unpaced mock is treated the
  # same way rather than published as served:true with a dishonest 0. (The paced lane's valid c1 +
  # sustained numbers are recomputed by the field run; a rig-readiness gap is never a gateway number.)
  if ! stream_mock_ready 30; then
    log "[$GATEWAY]   $cell : unpaced mock did not rebind before the cpu-fps peak (lost MOCK_PORT race) — marking streaming UNTESTABLE (rig readiness failure), NOT fabricating cpu_fps:0/mock_bound:null under stream_served:true"
    CELL_STREAM_JSON=", \"stream\": {\"stream_served\": \"untestable\", \"reason\": \"stream_mock_unready\", \"stream_error\": \"$(json_escape "the unpaced streaming mock did not rebind :$MOCK_PORT before the cpu-fps peak (a lost MOCK_PORT race: the previous cell's mock had not yet exited, so the fresh one could not bind); searching against a dead port would fabricate cpu_fps:0 with a null mock_bound under stream_served:true, so this cell's streaming is left UNMEASURED (a rig-readiness limit, not a gateway fault)")\"}"
    return 0
  fi
  local fps_lo fps_hi; read -r fps_lo fps_hi <<< "$MATRIX_STREAMCPU_FPS_BOUNDS"
  streamcpu_peak_fps "$fps_lo" "$fps_hi"
  local fps_peak=$SM_FPS_PEAK fps_conc=$SM_FPS_PEAK_CONC fps_bound=$SM_FPS_MOCK_BOUND fps_json="$SM_JSON_ACC"
  # Same measured-vs-not-measured distinction for the cpu-fps lane (audit P1-12).
  if [ "${SM_FPS_MEASURED:-0}" != 1 ]; then
    fps_peak=null; fps_conc=null; fps_bound=null
    log "[$GATEWAY]   $cell : cpu-fps NOT MEASURED (peak search aborted before any rung was probed) - publishing null, not 0"
  elif [ "${SM_FPS_AT_TOP:-0}" = 1 ]; then
    # A LOWER BOUND IS NOT A PEAK, the exact twin of the sustained-streams case above. The fps curve was
    # still climbing when the search range ran out, so this run did not find the maximum; the winner is
    # our own fps_hi wearing the gateway's name. Publishing it would make every gateway whose curve
    # outruns the range land on the identical concurrency, which is how a shared ladder rung comes to be
    # read as a shared capability. Unmeasured is absent, never substituted.
    log "[$GATEWAY]   $cell : cpu-fps still rising at the top of the search range (c=$fps_hi) - publishing null, not the bound; raise MATRIX_STREAMCPU_FPS_BOUNDS and re-run this gateway"
    fps_peak=null; fps_conc=null; fps_bound=null
  fi
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
  # LOW-1: mock_start_plain only sleeps 1s — unlike run_sweep (_sw_mock_ready) and the stream lanes
  # (stream_mock_ready), it does NOT poll for readiness. Under prior high-concurrency load the plain mock
  # can still be racing the dying prior mock for MOCK_PORT when sweep_c1 fires its direct-to-mock c1
  # baseline, so sweep_c1's honesty gate (C1_OK=0) would spuriously null this cell's added-latency for a
  # dead-port probe. Poll the fresh mock (DURL is set above) before the c1 baseline, matching every other
  # mock (re)start site. Non-fatal: sweep_c1's own gate still nulls a genuinely-unusable window.
  _sw_mock_ready 30 || log "[$GATEWAY]   $egress <- $cell : plain mock did not become ready before the c1 baseline (a lost MOCK_PORT race); sweep_c1's honesty gate will null the c1 latency if the port is dead"
  sweep_c1
  run_sweep 0 "$SWEEP" peak
  # ONE source of truth: the charted array (SW_JSON, every ramp AND bisect probe this sweep made)
  # and the headline (SW_CEIL_RPS/SW_CEIL_CONC = max gate-passing point in THAT SAME array) come out
  # of the single run_sweep call. Carry BOTH into the cell so the drawer's headline is, by
  # construction, one of the points on its own sweep curve - never a separate perf-suite measurement.
  local prps=$SW_CEIL_RPS pconc=$SW_CEIL_CONC pbound=$SW_BOUND pjson="$SW_JSON"
  run_sweep "$SWEEP_TTFT_MS" "$SWEEP" peak
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
  # conc_at_sustained / conc_at_peak (snapshot #65): the concurrency RUNG each ceiling held at — the
  # adaptive sweep ramps concurrency then keeps only the winning rung's SW_CEIL_CONC. Same value as the
  # existing *_concurrency fields (kept for back-compat); named explicitly so the snapshot + Performance
  # tab can surface "X rps @ Y conc" next to each headline. Null-safe: 0 when the sweep recorded no rung.
  CELL_PERF_JSON=", \"perf\": {\"added_latency_p50_us\": $lat50, \"added_latency_p99_us\": $lat99, \"gateway_c1_p99_us\": $g99, \"direct_c1_p99_us\": $d99, \"rps_sustained_20ms\": $lrps, \"rps_sustained_20ms_concurrency\": $lconc, \"conc_at_sustained\": $lconc, \"rps_sustained_20ms_mock_bound\": $lbound, \"rps_max_proxy\": $prps, \"rps_max_proxy_concurrency\": $pconc, \"conc_at_peak\": $pconc, \"rps_max_proxy_mock_bound\": $pbound, \"sweep_max_proxy\": [$pjson], \"sweep_sustained_20ms\": [$ljson], \"egress_reverified\": $reverified${reverify_note:+, \"reverify_note\": \"$(json_escape "$reverify_note")\"}$c1note}"
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
      \"$1\": {\"served\": $2, ${reason:+\"reason\": \"$reason\", }\"status\": \"$3\", \"path\": \"$4\", \"verdict_note\": \"$(json_escape "$5")\", \"body_snippet\": \"$(json_escape "$6")\"$probe_json$CELL_PERF_JSON$CELL_STREAM_JSON$CELL_MEM_JSON}"
  CELL_PERF_JSON=""; CELL_STREAM_JSON=""; CELL_MEM_JSON=""; CELL_PROBE_NOTE=""
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
  # reach the local mock, never a wrong answer. ONE budget for every cell, from the choke point - the
  # gateway's advisory capability declaration must never buy a cell more attempts than its neighbour
  # (it used to: declared 3x120s, undeclared 2x10s, same observation).
  local _retries _pause
  read -r _retries _pause <<<"$(probe_transient_budget)"
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
    #   - a DETERMINISTIC application rejection (e.g. a gateway 503ing "failed to parse request" on
    #     an ingress shape it has no route type for): that IS the probe's honest answer - the pairing
    #     is not configured - and labeling it "not verified" would hide real probe evidence.
    # Discriminated ON OBSERVABLES ALONE, in lib/probe_verdict.sh: the gateway ANSWERED (a real 5xx,
    # not 000) while the mock is verifiably healthy -> not_configured with the 5xx evidence; anything
    # else (000, or a mock we could not confirm healthy) -> not_verified. The manifest's advisory
    # declaration is deliberately NOT an input: the same observation must get the same verdict for
    # every gateway, whether or not it claimed the cell.
    local _mock_ok=0
    curl -s -m3 "http://127.0.0.1:$MOCK_PORT/__mock/state" 2>/dev/null | grep -q '"recording":true' && _mock_ok=1
    snip="$(strip_ctrl "$(printf '%s' "$LAST_BODY" | head -c 200)")"
    local _served_word
    read -r _served_word reason <<<"$(persistent_transient_verdict "$LAST_STATUS" "$_mock_ok")"
    served="\"$_served_word\""
    if [ "$_served_word" = not_configured ]; then
      note="HTTP $LAST_STATUS on POST $path, persistent across $_retries attempts with the mock verifiably healthy: a deterministic application-level rejection of this ingress/egress pairing, not a transport failure"
      CELL_PROBE_NOTE="probe failed: HTTP $LAST_STATUS on POST $path (persistent across $_retries attempts, mock healthy); first bytes: $(strip_ctrl "$(printf '%s' "$LAST_BODY" | head -c 160)")"
    else
      note="HTTP $LAST_STATUS after $_retries attempts: the gateway did not complete a round trip to the upstream (transport failure, e.g. a dead upstream socket or unhealthy rig); recorded as not_verified rather than graded"
    fi
    log "[$GATEWAY]   $egress <- $cell : served=$_served_word ($note)"
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
    # THIS cell's own memory window, in its own cold-started process (matrix_cell_memory). It runs on
    # every served cell and on no other: an unserved cell has no memory block at all, which reads as
    # absent. Nothing is selected, nothing is aggregated, and nothing carries between cells.
    matrix_cell_memory "$egress" "$cell" "$path" "$data" "$@"
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
  # WAIT for the old process to actually let go of the port, do not signal and hope. A blind `sleep 1`
  # let a SIGTERM'd predecessor keep the listener, so the next cell's readiness probe (a bare TCP
  # connect) succeeded against the OLD process and its "cold idle" window sampled a just-loaded one.
  # See gw_stop_wait in lib/harness.sh for the full reasoning and for why the memory lane is where this
  # does real damage. A port that never frees is logged honestly rather than launched into.
  gw_stop_wait || log "[$GATEWAY] WARNING: :$GW_PORT still occupied after gw_stop_wait - the relaunch may bind late or fail, and any cold-idle sample taken now could belong to the previous process"
  # Every column starts from the manifest's declared state, never from the previous column's
  # mutation of it (see restore_manifest_baseline).
  restore_manifest_baseline
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

# ── memory: ONE window per SERVED cell, in that cell's own cold-started process ─────────────────────
# Called from run_cell for every green cell (see the protocol header at the top of this file). There is
# no cell selection anywhere in here and no state that survives a cell: the previous design recorded
# every served cell's rps during the loop so a post-6x6 window could pick the "peak" one, and that
# machinery is gone rather than parked behind a flag - a selection nobody can switch on cannot come
# back. MATRIX_MEMORY=0 writes no "memory" object into any cell.
CELL_MEM_JSON=""
MEMORY_WINDOW_S=0   # accumulated wall time across every per-cell window (phase_s reporting)

# The RSS null-guard choke point (mem_num_or_null / mem_rss_read / mem_hwm_read) lives in
# lib/harness.sh next to the /proc readers it guards, and is covered by lib/mem_rss_test.sh.

# _mem_port_open: a NON-ALLOCATING readiness probe - a bare TCP connect to the gateway's listen port,
# no HTTP request, no request-path allocation. This is what the cold-restarted process is gated on so
# the COLD IDLE window below is measured BEFORE the process has served a single request (audit P0-4:
# the old readiness fn was warm_up, i.e. real traffic, so "cold idle" was in fact a post-traffic
# baseline - it measured `recovered`, not `idle`, while the published methodology claimed the latter).
_mem_port_open(){
  local i
  for i in $(seq 1 60); do
    (exec 3<>"/dev/tcp/127.0.0.1/$GW_PORT") 2>/dev/null && { exec 3<&- 3>&- 2>/dev/null; return 0; }
    sleep 1
  done
  return 1
}
# _mem_pad_body <json-object-body> <pad_bytes> -> the body with exactly <pad_bytes> of payload injected
# as ONE extra top-level JSON string field. Prints nothing when the body is not a JSON object (the pad
# cannot be injected safely), which the caller treats as "the declared recipe was NOT delivered".
# WHY shell-side (audit P0-3): ugen lets -body OVERRIDE -psize, so the memory window passed a declared
# payload_bytes that was silently discarded - the "identical fixed load on every gateway" fairness
# guarantee printed on the board was false. The pad is injected here, into THIS cell's own ingress-
# dialect body, so the DECLARED recipe is byte-for-byte the DELIVERED recipe on every gateway and every
# cell, and it needs no change to the pinned prebuilt rig binary.
_mem_pad_body(){
  local body="$1" n="$2" pad rest
  case "$body" in '{'*) ;; *) return 1;; esac
  pad="$(printf '%*s' "$n" '' | tr ' ' x)"
  [ "${#pad}" -eq "$n" ] || return 1
  rest="${body#\{}"
  case "$(printf '%s' "$rest" | tr -d '[:space:]')" in
    '}') printf '{"_bench_pad":"%s"}' "$pad";;
    *)   printf '{"_bench_pad":"%s",%s' "$pad" "$rest";;
  esac
}
# _mem_median <file-of-numbers> -> the median (empty file -> null). Idle and the plateau value are both
# MEDIANS, not last samples, so a single blip inside either window cannot set the published number.
_mem_median(){
  [ -s "$1" ] || { echo null; return; }
  sort -g "$1" | awk '{a[NR]=$1} END{ if(NR==0){print "null"} else if(NR%2){printf "%.1f", a[(NR+1)/2]} else {printf "%.1f", (a[NR/2]+a[NR/2+1])/2} }'
}
# _mem_rate_or_null <value> -> the value verbatim, or the literal null when it is not a number.
# SEPARATE from mem_num_or_null on purpose: that guard rejects zero and negatives because an RSS of 0
# is always an unmeasurable read, but a growth RATE of exactly 0.000 is the most meaningful reading
# this metric produces (a gateway that is genuinely flat), and a negative rate means it is releasing.
# Routing the rate through the RSS guard would have silently converted both into "not measured".
_mem_rate_or_null(){
  local v="${1:-}"
  case "$v" in ''|*[!0-9.eE+-]*) echo null; return;; esac
  printf '%s' "$v"
}
# matrix_cell_memory <egress> <cell> <path> <body> [extra -H header...]
# THE per-cell memory window: cold start -> idle -> fixed load to plateau (or cap) -> recovery, sampled
# continuously across ONE process lifecycle so rss_series is a single meaningful curve. Sets
# CELL_MEM_JSON; emit_cell folds it into this cell's object.
matrix_cell_memory(){
  local egress="$1" cell="$2" path="$3" data="$4"; shift 4
  CELL_MEM_JSON=""
  [ "$MATRIX_MEMORY" = 1 ] || return 0
  local idle=null steady=null recovered=null peak=null hwm=null series=null
  local plateaued=null ttp=null growth=null load_s=null
  local served=false serr="" disclose=""
  local recipe="{\"concurrency\": $MATRIX_MEM_CONC, \"payload_bytes\": $MATRIX_MEM_PAYLOAD, \"plateau_window_s\": $MEM_PLATEAU_WINDOW_S, \"plateau_trend_pct\": $MEM_PLATEAU_TREND_PCT, \"plateau_range_pct\": $MEM_PLATEAU_RANGE_PCT, \"plateau_cap_s\": $MEM_PLATEAU_CAP_S, \"load_leg_s\": $MEM_LOAD_LEG_S}"
  local _t0; _t0=$(date +%s)
  if suite_deadline_expired; then
    # Same rule the perf sweep follows: a wall-clock limit is a RIG limit, so the window is recorded as
    # not measured rather than being attributed to the gateway as a memory result.
    serr="suite wall-clock ceiling (${HARNESS_SUITE_CEIL_S}s) reached before this cell's memory window; not measured"
    log "[$GATEWAY]   $egress <- $cell : suite ceiling reached - skipping the memory window (all memory fields null)"
  else
    # THE COLUMN VERDICT MUST SURVIVE THIS RELAUNCH, exactly as it does for the per-cell perf cold start
    # above: launch_egress + the readiness probe here must not let this cell's boot become the column's
    # published serve verdict.
    local _wok="$WARM_OK" _wlast="$WARM_LAST" _wserr="$SERVE_ERR"
    log "[$GATEWAY]   $egress <- $cell : memory window - cold restart, ${MEM_IDLE_S}s idle, load to plateau (cap ${MEM_PLATEAU_CAP_S}s), ${MEM_SETTLE_S}s recovery"
    if harness_launch_ready launch_egress _mem_port_open "$egress"; then
      # Per-cell temp files. The window now runs up to 36 times per gateway instead of once, so a fixed
      # per-PID name would have every cell writing the same series file, and a missed cleanup would leak
      # a file per cell. Name them by cell and remove them at the end of THIS window.
      local tag pfx SERIESF STOP PEAKF HWMF IDLEF LOADF WINF MEDF T0
      tag="$(printf '%s' "${cell}-${egress}" | tr -c 'A-Za-z0-9._-' '_')"
      pfx="${TMPDIR:-/tmp}/mtxmem.$$.$tag"
      SERIESF="$pfx.series"; STOP="$pfx.stop"; PEAKF="$pfx.peak"; HWMF="$pfx.hwm"
      IDLEF="$pfx.idle"; LOADF="$pfx.load"; WINF="$pfx.win"; MEDF="$pfx.med"
      rm -f "$SERIESF" "$STOP" "$PEAKF" "$HWMF" "$IDLEF" "$LOADF" "$WINF" "$MEDF"
      : >"$SERIESF"; : >"$PEAKF"; : >"$HWMF"; T0=$(date +%s)
      MEM_STOP="$STOP"   # file-scope so cleanup()'s trap can stop the sampler on an early exit
      # ONE background sampler for the WHOLE window (idle -> load -> recovery): appends `<t_s> <rss>`
      # every MEM_SAMPLE_S and advances a running peak. t_s is measured from THIS process's start, so
      # the series stays a single continuous timeline the board can draw as one sparkline. mem_rss_read
      # prints NOTHING for an unmeasurable read, so neither the curve nor the peak can carry a
      # fabricated 0. The running peak is RESET at the start of the load phase (below) so peak_rss_mib
      # is a load-phase max, never an idle max. Watchdog LOGS a runaway; never kills the measurement.
      #
      # HWM IS SAMPLED HERE TOO, AT THE SAME INSTANTS, and for a specific reason. VmHWM is a per-process
      # kernel high-water mark, so summing it across a process TREE is only meaningful if the tree's
      # membership is fixed. It is not: a worker alive during the load contributes to the sampled RSS
      # peak and then vanishes from a later VmHWM sum when it exits. Reading the sum ONCE at the end
      # therefore produced sampled peak > summed hwm, which is physically impossible for a fixed tree
      # and which the board's own consistency check duly flagged on three gateways (0.14-0.67%).
      # Taking the max of the sum over the SAME sample instants keeps both numbers describing the same
      # process set at the same moments, so peak <= hwm holds by construction.
      ( while [ ! -f "$STOP" ]; do
          v=$(mem_rss_read); h=$(mem_hwm_read)
          if [ -n "$v" ]; then
            echo "$(( $(date +%s) - T0 )) $v" >>"$SERIESF"
            awk -v v="$v" -v p="$(cat "$PEAKF" 2>/dev/null)" 'BEGIN{exit !(v+0>p+0)}' && echo "$v" >"$PEAKF"
            awk -v v="$v" -v c="$MEM_CAP_MIB" 'BEGIN{exit !(v+0>c+0)}' && echo "[$GATEWAY][mem watchdog] RSS $v MiB > cap $MEM_CAP_MIB (logged)"
          fi
          [ -n "$h" ] && { awk -v v="$h" -v p="$(cat "$HWMF" 2>/dev/null)" 'BEGIN{exit !(v+0>p+0)}' && echo "$h" >"$HWMF"; }
          sleep "$MEM_SAMPLE_S"
        done ) & MEM_SP=$!
      # 1) COLD IDLE - the process has served ZERO requests at this point (readiness was a bare TCP
      # connect, and this window runs in its own process, not the one the perf sweep just hammered).
      # idle_rss_mib = the MEDIAN of the samples over MEM_IDLE_S.
      local _end r; : >"$IDLEF"
      _end=$(( $(date +%s) + MEM_IDLE_S ))
      while [ "$(date +%s)" -lt "$_end" ]; do r=$(mem_rss_read); [ -n "$r" ] && echo "$r" >>"$IDLEF"; sleep "$MEM_SAMPLE_S"; done
      idle="$(mem_num_or_null "$(_mem_median "$IDLEF")")"
      # 2) THE FIXED LOAD, on the PLAIN (non-recording) mock - the first traffic this process ever sees.
      mock_start_plain; _sw_mock_ready 30 || log "[$GATEWAY]   $egress <- $cell : plain mock slow to bind before the memory load"
      local body_load="" pad_delivered=0
      if body_load="$(_mem_pad_body "$data" "$MATRIX_MEM_PAYLOAD")" && [ -n "$body_load" ]; then
        pad_delivered=$MATRIX_MEM_PAYLOAD
      else
        body_load="$data"; pad_delivered=0
      fi
      local UGEN_H=( ${CURL_H[@]+"${CURL_H[@]}"} "$@" )
      # PEAK IS SCOPED TO THE LOAD PHASE: truncate the running max the instant before the load starts
      # so an idle-window sample can never be published as a "peak under load" (audit P0-5). HWM is
      # truncated with it, so both maxima cover exactly the same window as well as the same instants.
      : >"$PEAKF"; : >"$HWMF"
      # sweep_probe_raw (lib/sweep.sh) reads SWEEP_BODY/PSIZE/UGEN_H as globals. PSIZE=0: the payload
      # already lives IN the body, and ugen's -psize is inert whenever -body is set - passing it would
      # re-declare a payload that is not sent.
      local _psize_save="$PSIZE"
      SWEEP_BODY="$body_load"; PSIZE=0
      local L0 LSTART leg elapsed rem d out leg_ok ok_total=0 legs=0 broke=0
      L0=$(date +%s); LSTART=$(( L0 - T0 ))
      leg="$MEM_LOAD_LEG_S"; [ "$leg" -gt "$MEM_PLATEAU_WINDOW_S" ] && leg="$MEM_PLATEAU_WINDOW_S"
      [ "$leg" -lt 1 ] && leg=1
      while :; do
        elapsed=$(( $(date +%s) - L0 ))
        rem=$(( MEM_PLATEAU_CAP_S - elapsed ))
        [ "$rem" -le 0 ] && break                      # THE CAP. Reaching it is a result, not an error.
        d="$leg"; [ "$d" -gt "$rem" ] && d="$rem"
        out="$(sweep_probe_raw "http://127.0.0.1:$GW_PORT$path" "$MATRIX_MEM_CONC" "$d" 2>/dev/null)"
        leg_ok="$(_mem_kv "$out" ok)"; case "$leg_ok" in ''|*[!0-9]*) leg_ok=0;; esac
        ok_total=$(( ok_total + leg_ok )); legs=$(( legs + 1 ))
        if [ "$leg_ok" -le 0 ]; then
          # A leg that delivered nothing means the load STOPPED being applied (the gateway stopped
          # answering, the mock died). Carrying on would spend up to the whole cap sampling an idle
          # process and then certify that flat curve as a plateau, so stop here and disclose it: the
          # window is reported unmeasured rather than as a steady state we did not observe.
          broke=1
          log "[$GATEWAY]   $egress <- $cell : memory load leg $legs delivered ZERO successful requests (ugen: ${out:-no output}) - stopping the load"
          break
        fi
        elapsed=$(( $(date +%s) - L0 ))
        # A plateau can only be judged once a FULL steadiness window of LOAD-PHASE samples exists. The
        # trailing window is taken from the load phase alone (t >= LSTART): including idle samples would
        # mean every load starts by looking at a flat curve and would certify a plateau immediately.
        [ "$elapsed" -ge "$MEM_PLATEAU_WINDOW_S" ] || continue
        awk -v s="$LSTART" '$1+0>=s' "$SERIESF" >"$LOADF" 2>/dev/null
        plateau_window "$LOADF" "$MEM_PLATEAU_WINDOW_S" >"$WINF" 2>/dev/null
        if [ "$(plateau_check "$WINF" "$MEM_PLATEAU_TREND_PCT" "$MEM_PLATEAU_RANGE_PCT")" = 1 ]; then
          # TIME TO PLATEAU IS WHEN THE GATEWAY WENT FLAT, NOT WHEN WE FINISHED CONFIRMING IT. The window
          # is TRAILING, so a pass at `elapsed` means the span [elapsed-window, elapsed] was steady: the
          # RSS stopped moving at the START of that window and the remaining time is our instrument
          # satisfying itself. Publishing `elapsed` overstated every settling time by exactly one window
          # (kong went flat at ~120s and would have been published as "settled after 180 s"), and worse,
          # it made a number ABOUT THE GATEWAY move whenever MEM_PLATEAU_WINDOW_S moved - the stopping
          # point deciding the answer, which is the defect this whole design exists to remove. Reporting
          # the window start makes it window-independent to within one sample. Zero is a real and
          # meaningful answer: the RSS was already steady when the load began.
          plateaued=true; ttp=$(( elapsed - MEM_PLATEAU_WINDOW_S )); [ "$ttp" -lt 0 ] && ttp=0
          log "[$GATEWAY]   $egress <- $cell : memory PLATEAUED after ${elapsed}s of load"
          break
        fi
      done
      load_s=$(( $(date +%s) - L0 ))
      SWEEP_BODY=""; PSIZE="$_psize_save"
      [ "$plateaued" = true ] || plateaued=false
      [ "$plateaued" = false ] && [ "$broke" = 0 ] && log "[$GATEWAY]   $egress <- $cell : memory did NOT plateau within the ${MEM_PLATEAU_CAP_S}s cap (a published result: plateaued:false + growth rate)"
      # THE FINAL TRAILING WINDOW - the basis for BOTH the plateau value and the growth rate.
      awk -v s="$LSTART" '$1+0>=s' "$SERIESF" >"$LOADF" 2>/dev/null
      plateau_window "$LOADF" "$MEM_PLATEAU_WINDOW_S" >"$WINF" 2>/dev/null
      # growth_rate_mib_per_min is emitted WHETHER OR NOT the gateway plateaued, and that is the point.
      # The steadiness gate has a threshold, and any threshold admits a leak slower than itself: at a 1%
      # trend gate a gateway creeping ~1.5 MiB/min on a 120 MiB base passes as steady, which is ~90 MiB
      # an hour. Publishing the rate only on the not-plateaued path would hide exactly the cases the
      # gate is too loose to catch. A row reading "plateaued: true, growth 1.5 MiB/min" is self-evident.
      growth="$(_mem_rate_or_null "$(plateau_growth_rate "$WINF")")"
      # steady_state is the MEDIAN of that same window, and it exists ONLY when the window passed the
      # steadiness test. A gateway that hit the cap has no steady state to report - substituting its
      # last sample would publish a number the measurement explicitly says it never reached.
      if [ "$plateaued" = true ]; then
        awk '{print $2}' "$WINF" 2>/dev/null >"$MEDF"
        steady="$(mem_num_or_null "$(_mem_median "$MEDF")")"
      fi
      # NO SAMPLES MEANS NO VERDICT, NOT A FAILED ONE. When the rig cannot read this gateway's RSS at all
      # (no gw_rss hook, a container whose /proc is not visible, a non-Linux dev box) the load-phase
      # window is empty, and the loop above will have run to the cap without ever being able to declare a
      # plateau. Publishing plateaued:false there would assert we WATCHED this gateway fail to settle,
      # which is a measurement we did not take. plateau_check's own rule is the same one: fewer than four
      # samples is undecidable, and undecidable is not a plateau - nor is it evidence of the opposite.
      if [ "$(awk 'END{print NR+0}' "$WINF" 2>/dev/null)" -lt 4 ]; then
        plateaued=null; ttp=null; steady=null
        disclose="$disclose; plateau verdict withheld: the trailing window holds fewer than the four RSS samples the steadiness test needs to decide (this rig could not measure this gateway's RSS), so the load ran to the cap without a verdict either way"
      fi
      # LOAD-SUCCESS + RECIPE GATE (audit P0-3/P0-5). Every load-phase number is published only when the
      # fixed load was ACTUALLY delivered for the whole window AND the payload delivered equals the
      # payload declared in load_recipe. A failed load otherwise yields a "peak" that is really the idle
      # max and a "plateau" that is really an idle process, and a silently-dropped payload makes the
      # cross-gateway comparison unfair. Either way the honest answer is null plus a disclosure.
      local load_ok=1
      if [ "$ok_total" -le 0 ] || [ "$broke" = 1 ]; then
        load_ok=0
        disclose="$disclose; load-phase results withheld: the fixed load stopped delivering successful requests after ${legs} leg(s) (${ok_total} delivered in total), so a sampled maximum would be an idle artifact and a flat curve would certify a plateau this gateway never reached"
      fi
      if [ "$pad_delivered" != "$MATRIX_MEM_PAYLOAD" ]; then
        load_ok=0
        disclose="$disclose; load-phase results withheld: declared load_recipe.payload_bytes=$MATRIX_MEM_PAYLOAD but only ${pad_delivered}B were actually delivered per request, so this cell did not receive the identical fixed load"
        log "[$GATEWAY]   $egress <- $cell : memory PAYLOAD MISMATCH declared=$MATRIX_MEM_PAYLOAD delivered=$pad_delivered"
      fi
      if [ "$load_ok" = 1 ]; then
        served=true
        # BOTH maxima come from the sampler's running values over the SAME load-phase instants. Reading
        # mem_hwm_read fresh here instead would sum VmHWM over whatever tree exists at THIS moment,
        # which is a different process set than the one that produced the sampled peak.
        hwm="$(mem_num_or_null "$(cat "$HWMF" 2>/dev/null)")"
        peak="$(mem_num_or_null "$(cat "$PEAKF" 2>/dev/null)")"
      else
        # UNMEASURED IS NOT A RESULT. Without a delivered load there is no plateau verdict, no time to
        # one and no leak rate to report, so all three go null rather than carrying the shape of an
        # idle process. plateaued is null here on purpose: false would assert we watched this gateway
        # fail to settle under load, and we did not.
        plateaued=null; ttp=null; growth=null; steady=null
        serr="the fixed memory load was not delivered on this cell"
      fi
      # 3) RECOVERY: sample for MEM_SETTLE_S; recovered_rss_mib = the RSS at the END of the window.
      _end=$(( $(date +%s) + MEM_SETTLE_S ))
      while [ "$(date +%s)" -lt "$_end" ]; do r=$(mem_rss_read); [ -n "$r" ] && recovered="$r"; sleep "$MEM_SAMPLE_S"; done
      r=$(mem_rss_read); [ -n "$r" ] && recovered="$r"
      recovered="$(mem_num_or_null "$recovered")"
      # Stop the sampler + build the single-lifecycle series (idle -> load -> recovery).
      [ -n "$MEM_STOP" ] && touch "$MEM_STOP"; [ -n "$MEM_SP" ] && kill "$MEM_SP" 2>/dev/null; MEM_STOP="" MEM_SP=""
      series="$(rss_series_json "$SERIESF")"
      rm -f "$SERIESF" "$STOP" "$PEAKF" "$HWMF" "$IDLEF" "$LOADF" "$WINF" "$MEDF" 2>/dev/null
      log "[$GATEWAY]   $egress <- $cell : memory idle=${idle} steady=${steady} peak=${peak} hwm=${hwm} recovered=${recovered} MiB plateaued=${plateaued} in ${load_s}s growth=${growth} MiB/min"
      # RESTORE THE RECORDING MOCK before the next cell's capability probe. The load above ran against
      # the plain mock, and a cell probed against a non-recording mock reads norecord (leg-3 evidence
      # unavailable) - a harness gap that would be recorded against the gateway. Same discipline, and
      # the same fatal, as the perf sweep's restore: continuing with a wedged rig silently corrupts
      # every following cell.
      if ! mock_start_record; then
        log "[$GATEWAY]   $egress <- $cell : recording mock did not rebind :$MOCK_PORT after the memory window - retrying"
        if ! mock_start_record; then
          echo "FATAL: recording mock could not be restored on :$MOCK_PORT after the $egress<-$cell memory window - every following cell would probe a dead port. Aborting rather than mislabeling them." >&2
          exit 1
        fi
      fi
    else
      serr="${HARNESS_SERVE_ERR:-$SERVE_ERR}"
      log "[$GATEWAY]   $egress <- $cell : memory window - the cold-restarted process never opened :$GW_PORT; memory reads not measured"
    fi
    WARM_OK="$_wok"; WARM_LAST="$_wlast"; SERVE_ERR="$_wserr"
  fi
  MEMORY_WINDOW_S=$(( MEMORY_WINDOW_S + $(date +%s) - _t0 ))
  # The protocol string is assembled LAST so every disclosure this window accumulated travels with the
  # numbers to the board instead of living only in the log.
  local protocol="per-cell, own cold-started process (separate from this cell's perf window): ${MEM_IDLE_S}s COLD idle sampled before the process serves any request -> fixed load on $cell>$egress (c=$MATRIX_MEM_CONC, payload=${MATRIX_MEM_PAYLOAD}B, identical for every gateway and every cell) run until the RSS is steady over a trailing ${MEM_PLATEAU_WINDOW_S}s window (drift < ${MEM_PLATEAU_TREND_PCT}%, spread < ${MEM_PLATEAU_RANGE_PCT}%) or until the ${MEM_PLATEAU_CAP_S}s cap, whichever comes first - reaching the cap is a published result (plateaued:false plus the growth rate), not an error -> ${MEM_SETTLE_S}s recovery. No cell is selected: every served cell gets its own window and nothing is aggregated across them${disclose}"
  CELL_MEM_JSON=", \"memory\": {\"protocol\": \"$(json_escape "$protocol")\", \"served\": $served, \"serve_error\": \"$(json_escape "$serr")\", \"load_recipe\": $recipe, \"idle_rss_mib\": ${idle}, \"steady_state_rss_mib\": ${steady}, \"recovered_rss_mib\": ${recovered}, \"peak_rss_mib\": ${peak}, \"peak_rss_hwm_mib\": ${hwm}, \"plateaued\": ${plateaued}, \"time_to_plateau_s\": ${ttp}, \"growth_rate_mib_per_min\": ${growth}, \"load_s\": ${load_s}, \"rss_series\": ${series}, \"idle_window_s\": $MEM_IDLE_S, \"recovery_window_s\": $MEM_SETTLE_S}"
}

UPSTREAMS_JSON=""
COMPAT_CELLS=""; COMPAT_SHAPE=""; COMPAT_SERVED=false; COMPAT_ERR=""
# Default-config reuse across columns (launch ladder case 3): the config is identical for every
# egress dialect, so the launch + warm-up verdict from the first column carries over instead of
# re-booting the same process six times.
HAVE_EGRESS_FN=0; declare -f gw_matrix_egress >/dev/null && HAVE_EGRESS_FN=1
DEFAULT_UP=0; DEFAULT_WARM_OK=0; DEFAULT_WARM_LAST=000; DEFAULT_SERVE_ERR=""; DEFAULT_WARM_CELL=openai
_PHASE_T0=$(date +%s)   # phase clock: the whole 6x6 (probe + per-cell perf sweep + per-cell streaming)
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
    # ── COLD START PER CELL ───────────────────────────────────────────────────────────────────────
    # This loop used to share ONE process across all six ingress cells of an egress column: the
    # gateway was launched once per column, then cells were swept in fixed order against it. For any
    # gateway that accumulates state per request, cell 6 was therefore measured on a process that had
    # already served five full sweeps, and because the order is fixed that is a systematic bias rather
    # than noise. It is not hypothetical: busbar's RSS is linear in requests served with no plateau, so
    # its later cells were measured on a materially more loaded process than its first.
    #
    # Restarting per cell costs about ten seconds and removes the whole class. Every cell now begins
    # from an equally clean process, so a cell's number reflects the cell rather than its position in
    # the sweep order.
    #
    # THE COLUMN VERDICT IS PRESERVED ACROSS THE RELAUNCH. _egress_ready runs warm_up, which writes
    # WARM_OK/WARM_LAST/SERVE_ERR - the variables that decide whether the COLUMN is recorded as served.
    # Letting a per-cell warm-up overwrite them would mean the last cell's warm-up silently became the
    # column's verdict. Save and restore them; a per-cell boot failure is recorded by that cell's own
    # run_cell result, which is where a per-cell failure belongs.
    _CELL_WOK="$WARM_OK"; _CELL_WLAST="$WARM_LAST"; _CELL_SERR="$SERVE_ERR"
    if ! harness_launch_ready launch_egress _egress_ready "$EGRESS"; then
      log "[$GATEWAY]   $EGRESS <- $CELL : per-cell cold start did not come ready; the cell's own result records why"
    fi
    WARM_OK="$_CELL_WOK"; WARM_LAST="$_CELL_WLAST"; SERVE_ERR="$_CELL_SERR"
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
# The 6x6 phase now CONTAINS the memory windows (one per served cell, inside the loop), so matrix_6x6
# and memory_window OVERLAP rather than partitioning the run: memory_window is the accumulated wall time
# those windows took, and it is a SUBSET of matrix_6x6. Reported that way on purpose - "where did a 5h
# run go" is answered by the subset, and inventing a separate exclusive phase would misdescribe a loop
# that interleaves the two.
MATRIX_6X6_S=$(( $(date +%s) - _PHASE_T0 ))

# ONE canonical measured_at for both the per-suite matrix json AND the snapshot artifact (same instant).
MEASURED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
# Close the run clock here, at the same instant as measured_at: everything after this point is the
# (sub-second) snapshot assembly + the printed summary table, not measurement work.
RUN_FINISHED_AT="$MEASURED_AT"
RUN_DURATION_S=$(( $(date +%s) - RUN_T0 ))

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
  "cell_stream": $([ "$MATRIX_STREAM" = 1 ] && echo true || echo false),
  "cell_memory": $([ "$MATRIX_MEMORY" = 1 ] && echo true || echo false),
  "cells": {$COMPAT_CELLS
  },
  "upstreams": {$UPSTREAMS_JSON
  },
  "model": "$GW_MODEL_BASE",
  "upstream_endpoint": "$GW_PATH",
  "ootb_config": $([ -n "$OOTB_CONFIG" ] && printf '"%s"' "$OOTB_CONFIG" || echo null),
  "arch": "${BENCH_ARCH:-$(uname -m)}",
  "hardware": "${BENCH_HARDWARE:-$(uname -m) $(nproc 2>/dev/null || echo '?')vCPU}",
  "rig": $(command -v rig_provenance_json >/dev/null 2>&1 && rig_provenance_json || echo null),
  "measured_at": "$MEASURED_AT",
  "started_at": "$RUN_STARTED_AT",
  "finished_at": "$RUN_FINISHED_AT",
  "duration_s": $RUN_DURATION_S,
  "phase_s": { "build": $BUILD_S, "matrix_6x6": $MATRIX_6X6_S, "memory_window": $MEMORY_WINDOW_S },
  "build_env": { "prereqs": $(declare -F gw_prereqs >/dev/null 2>&1 && echo true || echo false), "quiesce": "$(json_escape "${BUILD_QUIESCE:-unknown}")" }
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

# ── snapshot artifact (task #65) ────────────────────────────────────────────────────────────────
# END-OF-RUN ASSEMBLER: bundle the already-produced pieces (the matrix json + the captured config text)
# into ONE atomic, timestamped, self-describing per-gateway snapshot. Config lives INSIDE the same file as
# the numbers it produced (kills the config-drift class); every run is kept, never overwritten. The
# consumer (gen-data) reads the NEWEST snapshot per gateway. Minimal risk: reuses existing producers.
SNAP_DIR="$ROOT/results/snapshots"; mkdir -p "$SNAP_DIR"
SNAP_TS_SAFE="$(printf '%s' "$MEASURED_AT" | tr ':' '-')"   # ISO8601 with ':' -> '-' for filename safety
SNAP_FILE="$SNAP_DIR/result_${GATEWAY}_${SNAP_TS_SAFE}.json"
# FAIL LOUDLY (audit P0-7). This heredoc runs under `set -uo pipefail` with no -e, so its exit status
# used to be discarded entirely: a writer crash (unreadable config, a json.dump error, a full disk) left
# NO snapshot behind while matrix/run.sh still exited 0, run-on-ec2 reported DONE, and gen-data quietly
# republished the previous run's data as if it were this run's. The status is now checked AND the
# artifact is re-read and re-parsed before the run is allowed to report success.
if ! python3 - "$RESULTS/$GATEWAY.json" "$ROOT/results/config/$GATEWAY.txt" "$SNAP_FILE" <<'PY'
import json, os, sys
matrix_path, config_path, snap_path = sys.argv[1], sys.argv[2], sys.argv[3]
m = json.load(open(matrix_path))
# config.files: inline the run's verbatim config render (busbar may have several; other gws one). Today
# the harness writes a single results/config/<gw>.txt; inline it under its basename. Absent -> empty set.
files = {}
if os.path.exists(config_path):
    files[os.path.basename(config_path)] = open(config_path, encoding="utf-8").read()
# MEMORY IS PER CELL AND IS NOT SUMMARISED HERE. It lives in the matrix grid this snapshot already
# carries verbatim (upstreams.<egress>.cells.<ingress>.memory), one window per served cell. This key
# stays for readers that expect the field, and it stays null: producing a scalar here would mean the
# harness picking a cell, which is precisely the defect the per-cell design removed. WHICH cell (or
# which extremum across cells) to display is a reader's choice, made where the reader can see it.
memory = m.get("memory")   # absent by construction -> null
streaming = None
ups = m.get("upstreams") or {}
# best diagonal = the openai diagonal when served, else the first served diagonal that streamed.
for eg in (["openai"] + [k for k in ups if k != "openai"]):
    cell = (ups.get(eg) or {}).get("cells", {}).get(eg)
    if cell and cell.get("served") is True and (cell.get("stream") or {}).get("stream_served") is True:
        st = cell["stream"]
        streaming = {"dialect": eg, "ttft_ms": m.get("sweep_ttft_ms"),
                     "added_ttft_p99_us": st.get("added_ttft_p99_us"), "added_gap_p99_us": st.get("added_gap_p99_us"),
                     "streams_sustained": st.get("streams_sustained"), "cpu_fps": st.get("cpu_fps")}
        break
snap = {
    "schema_version": 1,
    "gateway": m.get("gateway"),
    "build": m.get("build"),
    "measured_at": m.get("measured_at"),
    # RUN TIMING: how long this gateway's suite took on this box on this day. measured_at above is the
    # instant the results were stamped (what the board ages by); these describe the RUN. phase_s shows
    # WHERE a long run went (build vs the 6x6 vs the post-6x6 memory window). All .get() -> null-safe:
    # snapshots written before this existed simply carry null, and every reader must tolerate absence.
    "started_at": m.get("started_at"),
    "finished_at": m.get("finished_at"),
    "duration_s": m.get("duration_s"),
    "phase_s": m.get("phase_s"),
    "arch": m.get("arch"),
    "hardware": m.get("hardware"),
    # RIG PROVENANCE (audit #21): WHICH measurement instrument produced these numbers. The rig binaries
    # come from a MOVING release tag, so two runs under an identical harness can disagree purely because
    # the mock was rebuilt between them (bcf9912 tightened request_shape_ok mid-week and changed cell
    # verdicts board-wide). Recording the mock+ugen sha256 and the asset updated_at INSIDE the snapshot
    # makes that detectable at a glance instead of requiring an investigation. null on older runs.
    "rig": m.get("rig"),
    "config": {"files": files},
    "matrix": m,          # the full 6x6 grid (the sole numeric source), verbatim
    "memory": memory,     # the post-6x6 memory block (may be null until the field run)
    "streaming": streaming,
}
tmp = snap_path + ".tmp"
with open(tmp, "w", encoding="utf-8") as f:
    json.dump(snap, f, indent=1)
    f.write("\n")
os.replace(tmp, snap_path)   # atomic publish
print(f" -> {snap_path}")
PY
then
  echo "FATAL: the snapshot writer failed for $GATEWAY — no results/snapshots/ artifact was produced. Failing the run rather than exiting 0 and letting gen-data republish stale data as if it were this run." >&2
  exit 1
fi
# ASSERT the artifact actually landed and parses (a zero-byte or truncated snapshot is as bad as none).
if [ ! -s "$SNAP_FILE" ]; then
  echo "FATAL: snapshot $SNAP_FILE is missing or empty after a successful writer run." >&2
  exit 1
fi
if ! python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); sys.exit(0 if d.get("gateway") and d.get("matrix") else 1)' "$SNAP_FILE"; then
  echo "FATAL: snapshot $SNAP_FILE does not parse as JSON carrying gateway+matrix — refusing to report success." >&2
  exit 1
fi
echo "================================================================"
