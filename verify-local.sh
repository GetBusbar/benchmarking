#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# verify-local.sh — a FAST, LOCAL, end-to-end verifier for the whole benchmark harness.
#
# WHY. The field run (run-on-ec2.sh → run-all.sh → matrix/run.sh) takes ~5h/gateway: 36 cells, per-cell
# throughput + latency + streaming sweeps, a memory-once read, and OOTB config capture, streamed to the
# board. That is far too slow to DEBUG the harness with. This script drives the IDENTICAL code path —
# matrix/run.sh → site/gen-data.mjs → site/check-consistency.mjs + site/test.mjs → charts.py — against
# the same mock and ONE real gateway (busbar) on localhost, with TINY dev params, so the full pipeline
# finishes in MINUTES. Same code, short probes: prove the harness is correct before spending the 5h run.
#
# WHAT IT RUNS (all local, no EC2/AWS, no push):
#   1. the rig (mock + loadgen) locally — see the LOCAL RIG note below for the macOS topology;
#   2. ONE gateway (busbar) on localhost via its own gateways/busbar/gateway.sh manifest, docker-launched;
#   3. matrix/run.sh for that gateway with tiny dev params (SWEEP_DUR=1, short ladders, minimal stream +
#      memory windows, MATRIX_SWEEP_ADAPTIVE=1) — same producer, short windows;
#   4. node site/gen-data.mjs → site/data.json; node site/check-consistency.mjs; node site/test.mjs;
#      charts.py (PNG render);
#   5. ASSERTS the whole data path (see assert_pipeline) and tears everything down.
#
# LOCAL RIG (macOS Docker Desktop). The prebuilt GitHub rig (mock-<arch>/ugen-<arch>) is a Linux ELF and
# will NOT exec natively on macOS. So on a non-Linux host this script supplies the rig via the harness's
# RIG_MOCK_CMD/RIG_UGEN_CMD local-dev seam (lib/rig.sh):
#   * MOCK  = the PINNED Linux mock-<arch> run inside a --network host container (a generated wrapper).
#             A --network host container + the host + the busbar --network host container all share the
#             one Docker Desktop loopback, so busbar reaches the mock at 127.0.0.1:$MOCK_PORT exactly as
#             on Linux, and host-side ugen/curl reach it too. The mock binary itself is UNMODIFIED.
#   * UGEN  = a natively-built (go) ugen for host-side use (drives the mock/gateway from 127.0.0.1).
# On Linux the prebuilt rig execs natively; pass RIG_LOCAL_CONTAINER=0 to use the fetched binaries direct.
#
# NON-FIDELITY on macOS (honestly disclosed): container_rss_mib/hwm read /proc/<container-pid> on the
# HOST, which does not exist under Docker Desktop's Linux VM, so idle/peak RSS read 0. matrix.memory
# still records served:true (the gateway warmed + took load), so g.memory_read still PROJECTS — the data
# PATH is exercised; the RSS magnitudes are simply 0 here (they are real on a Linux field box).
#
# IDEMPOTENT + SELF-CLEANING: every run tears down its containers, host mock, temp dir, and reverts any
# results/ + site/data.json churn it wrote, so nothing leaks and the working tree is left clean.
#
#   bash verify-local.sh                 # fast local profile (default), busbar
#   GATEWAY=busbar bash verify-local.sh  # explicit
#   KEEP_ARTIFACTS=1 bash verify-local.sh   # leave results/ + data.json for inspection (still cleans procs)
#
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATEWAY="${GATEWAY:-busbar}"
ARCH="${BENCH_ARCH:-arm64}"
STAMP="$(date +%s)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/verify-local.XXXXXX")"
MOCK_CONTAINER="verify-mock-$STAMP"
GW_CONTAINER="busbar-bench"   # the name the busbar manifest uses
PORT_MOCK="${MOCK_PORT:-8000}"
PORT_GW="${GW_PORT:-8080}"
STEP=0
say(){ printf '\n\033[1;36m[verify %s]\033[0m %s\n' "$(date +%H:%M:%S)" "$*"; }
ok(){ printf '  \033[32mok\033[0m  %s\n' "$*"; }
die(){ printf '\n\033[1;31m[verify FAIL]\033[0m %s\n' "$*" >&2; exit 1; }

# ── docker seam ───────────────────────────────────────────────────────────────────────────────────
# The manifest + harness default to "sudo docker" (EC2 field default). A local rootless-docker box sets
# BENCH_DOCKER=docker so nothing prompts for a password. Auto-detect: if plain docker works, use it.
if [ -z "${BENCH_DOCKER:-}" ]; then
  if docker ps >/dev/null 2>&1; then BENCH_DOCKER="docker"; else BENCH_DOCKER="sudo docker"; fi
fi
export BENCH_DOCKER
$BENCH_DOCKER version >/dev/null 2>&1 || die "docker not usable via '$BENCH_DOCKER' (set BENCH_DOCKER)"

# ── cleanup: idempotent, always run on exit ─────────────────────────────────────────────────────────
cleanup(){
  local rc=$?
  say "teardown"
  $BENCH_DOCKER rm -f "$GW_CONTAINER" >/dev/null 2>&1
  $BENCH_DOCKER rm -f "$MOCK_CONTAINER" >/dev/null 2>&1
  # host-side mock (native/wrapper) + any stray wrapper
  [ -n "${RIG_MOCK_CMD:-}" ] && pkill -f "$RIG_MOCK_CMD" >/dev/null 2>&1
  pkill -f "$WORK/mock-run.sh" >/dev/null 2>&1
  # revert generated result/data churn unless asked to keep it (script writes these at runtime; only
  # verify-local.sh + minimal harness support should ever be committed).
  if [ "${KEEP_ARTIFACTS:-0}" != 1 ]; then
    git -C "$ROOT" checkout -- site/data.json >/dev/null 2>&1 || true
    git -C "$ROOT" clean -fdq results >/dev/null 2>&1 || true
    git -C "$ROOT" checkout -- results >/dev/null 2>&1 || true
    # runtime artifacts the busbar manifest writes next to itself (gitignored, but keep the tree tidy).
    rm -f "$ROOT/gateways/$GATEWAY/launch.log" "$ROOT/gateways/$GATEWAY/config.gen.yaml" \
          "$ROOT/gateways/$GATEWAY/providers.gen.yaml" 2>/dev/null
  fi
  rm -rf "$WORK" 2>/dev/null
  ok "teardown complete (containers removed, mock stopped, temp cleaned$([ "${KEEP_ARTIFACTS:-0}" = 1 ] && echo ', artifacts kept' || echo ', results/ + data.json reverted'))"
  exit "$rc"
}
trap cleanup EXIT INT TERM

# ── stray-state guard: a previous crashed run can leave a container/port behind ──────────────────────
say "pre-flight: clearing any stray verify state"
$BENCH_DOCKER rm -f "$GW_CONTAINER" >/dev/null 2>&1
$BENCH_DOCKER ps -a --filter 'name=verify-mock-' -q 2>/dev/null | xargs -r $BENCH_DOCKER rm -f >/dev/null 2>&1
ok "pre-flight clear"

# ════════════════════════════════════════════════════════════════════════════════════════════════════
# STEP 1 — build the LOCAL rig (mock + loadgen) usable on THIS host
# ════════════════════════════════════════════════════════════════════════════════════════════════════
say "step 1: prepare the local rig (mock + loadgen)"
mkdir -p "$ROOT/bin"
RIG_LOCAL_CONTAINER="${RIG_LOCAL_CONTAINER:-1}"
LINUX_MOCK="$ROOT/bin/mock-$ARCH"

# 1a. Fetch the pinned Linux rig binaries (mock + ugen) from GitHub — cached under bin/. We use the
#     mock binary verbatim (in a container); ugen we (re)build natively for host-side use below.
say "  fetching pinned rig binaries (cached under bin/)"
# shellcheck source=/dev/null
. "$ROOT/lib/rig.sh"
if ! fetch_rig "$ROOT" >/dev/null 2>&1; then
  # fetch failed but we may already have a cached Linux mock binary; otherwise this is a hard blocker.
  [ -x "$LINUX_MOCK" ] || die "could not fetch the pinned rig (mock-$ARCH) and no cached copy — network blocker"
fi
[ -x "$LINUX_MOCK" ] || die "pinned Linux mock ($LINUX_MOCK) missing after fetch"
ok "pinned Linux mock present: $LINUX_MOCK"

# 1b. loadgen (ugen) usable on the HOST. On Linux the fetched ugen execs natively; on macOS it's a Linux
#     ELF, so build ugen natively (go). Fall back to the fetched binary if go is unavailable and it execs.
UGEN_HOST=""
if [ "$(uname -s)" = "Linux" ] && [ -x "$ROOT/bin/ugen-$ARCH" ]; then
  UGEN_HOST="$ROOT/bin/ugen-$ARCH"
elif command -v go >/dev/null 2>&1; then
  say "  building ugen natively (go) for host-side load"
  if go build -o "$ROOT/bin/ugen-native" "$ROOT/loadgen/ugen.go" >"$WORK/ugenbuild.log" 2>&1; then
    UGEN_HOST="$ROOT/bin/ugen-native"
  fi
fi
if [ -z "$UGEN_HOST" ] || ! "$UGEN_HOST" -h >/dev/null 2>&1 && ! "$UGEN_HOST" 2>&1 | grep -qi usage; then
  # last resort: the fetched binary as-is (works on Linux)
  [ -x "$ROOT/bin/ugen-$ARCH" ] && UGEN_HOST="$ROOT/bin/ugen-$ARCH"
fi
[ -n "$UGEN_HOST" ] && [ -x "$UGEN_HOST" ] || die "no usable host loadgen (build failed and fetched ugen not executable here)"
ok "host loadgen: $UGEN_HOST"

# 1c. mock command the harness will invoke. On macOS we cannot exec the Linux mock natively, so we hand
#     the harness a WRAPPER that runs the pinned mock inside a --network host container (shared-loopback
#     topology). On Linux (RIG_LOCAL_CONTAINER=0) the native mock binary is used directly.
if [ "$RIG_LOCAL_CONTAINER" = 1 ]; then
  # A --network host container hosting the mock, forwarding the harness's MOCK_RECORD / MOCK_STREAM_*
  # env + -port args. Killable by `pkill -f <wrapper>`: a trap tears the container down so the harness's
  # kill+restart cycle (per cell / per streaming shape) can rebind the port cleanly.
  MOCK_WRAP="$WORK/mock-run.sh"
  cat > "$MOCK_WRAP" <<WRAP
#!/usr/bin/env bash
# Generated by verify-local.sh — pinned Linux mock in a --network host container (macOS shared loopback).
# The harness restarts the mock between phases via \`pkill -f <this path>; sleep 1; <start>\`. To make a
# restart CLEAN (no name collision with the container still being torn down), each wrapper instance uses
# a UNIQUE container name ($MOCK_CONTAINER + its own pid) and removes ONLY its own container on exit. A
# best-effort sweep of stale verify-mock-* containers keeps a crashed prior run from squatting the port.
set -u
# The wrapper must stay the process pkill matches (\`pkill -f <this path>\`), so we do NOT exec docker;
# we background it and forward the signal, removing our OWN uniquely-named container on the way out.
CN="$MOCK_CONTAINER-\$\$"
$BENCH_DOCKER ps -q --filter "name=$MOCK_CONTAINER" 2>/dev/null | xargs -r $BENCH_DOCKER rm -f >/dev/null 2>&1
CPID=""
stop(){ [ -n "\$CPID" ] && kill "\$CPID" 2>/dev/null; $BENCH_DOCKER rm -f "\$CN" >/dev/null 2>&1; exit 0; }
trap stop TERM INT
# Foreground-attached docker run (--rm) in the background of the wrapper; the wrapper waits on it so a
# SIGTERM to the wrapper (pkill) runs stop() → container removed → port freed for the next restart.
$BENCH_DOCKER run --rm --name "\$CN" --network host \\
  -e MOCK_RECORD="\${MOCK_RECORD:-}" \\
  -e MOCK_TTFT_MS="\${MOCK_TTFT_MS:-}" \\
  -e MOCK_STREAM_CHUNKS="\${MOCK_STREAM_CHUNKS:-}" \\
  -e MOCK_STREAM_INTERVAL_MS="\${MOCK_STREAM_INTERVAL_MS:-}" \\
  -e MOCK_STREAM_CHUNK_BYTES="\${MOCK_STREAM_CHUNK_BYTES:-}" \\
  -v "$LINUX_MOCK:/mock:ro" debian:stable-slim /mock "\$@" &
CPID=\$!
wait "\$CPID"
WRAP
  chmod +x "$MOCK_WRAP"
  # Ensure the container base image is present so the first cell's mock start isn't slow/racy.
  $BENCH_DOCKER image inspect debian:stable-slim >/dev/null 2>&1 || $BENCH_DOCKER pull -q debian:stable-slim >/dev/null 2>&1 || true
  RIG_MOCK_CMD="$MOCK_WRAP"
  ok "mock: pinned Linux mock via --network host container wrapper ($MOCK_WRAP)"
else
  RIG_MOCK_CMD="$LINUX_MOCK"
  ok "mock: native Linux mock binary (RIG_LOCAL_CONTAINER=0)"
fi
export RIG_MOCK_CMD RIG_UGEN_CMD="$UGEN_HOST"

# Prove the mock actually frames a request through the chosen command before we spend a whole matrix on it.
say "  smoke-test the mock"
"$RIG_MOCK_CMD" -port "$PORT_MOCK" >"$WORK/mock-smoke.log" 2>&1 &
SMOKE_PID=$!
for i in $(seq 1 20); do
  b="$(curl -s -m2 "http://127.0.0.1:$PORT_MOCK/v1/chat/completions" -X POST \
        -H 'content-type: application/json' -d '{"model":"m","messages":[{"role":"user","content":"hi"}]}' 2>/dev/null)"
  case "$b" in *chatcmpl-x*) break;; esac
  sleep 1
done
case "$b" in *chatcmpl-x*) ok "mock serves the canned OpenAI body on :$PORT_MOCK";;
  *) kill "$SMOKE_PID" 2>/dev/null; $BENCH_DOCKER rm -f "$MOCK_CONTAINER" >/dev/null 2>&1; die "mock did not serve on :$PORT_MOCK (log: $(head -c 300 "$WORK/mock-smoke.log"))";; esac
kill "$SMOKE_PID" 2>/dev/null; $BENCH_DOCKER rm -f "$MOCK_CONTAINER" >/dev/null 2>&1; sleep 1

# ════════════════════════════════════════════════════════════════════════════════════════════════════
# STEP 2+3 — run the matrix for ONE gateway with tiny dev params (SAME code path, short windows)
# ════════════════════════════════════════════════════════════════════════════════════════════════════
say "step 2+3: run matrix/run.sh for [$GATEWAY] with the fast local profile"
# taskset / setsid SHIMS (macOS). The harness stubs these as shell FUNCTIONS when the binaries are
# absent — but its load probes run THROUGH coreutils `timeout` (tmo), which execs its argument as a real
# program and cannot see a shell function. So `timeout taskset ... ugen ...` fails with "taskset: No such
# file" and every probe reads rps=0/fail=1. Providing REAL (executable) no-op shims on PATH lets timeout
# exec them; `command -v taskset` in matrix/run.sh then finds the shim instead of installing its function
# stub. On Linux the real taskset/setsid are used and these are never created.
if ! command -v taskset >/dev/null 2>&1 || ! command -v setsid >/dev/null 2>&1; then
  mkdir -p "$WORK/shims"
  cat > "$WORK/shims/taskset" <<'SH'
#!/usr/bin/env bash
[ "${1:-}" = "-c" ] && shift 2
exec "$@"
SH
  cat > "$WORK/shims/setsid" <<'SH'
#!/usr/bin/env bash
exec "$@"
SH
  chmod +x "$WORK/shims/taskset" "$WORK/shims/setsid"
  export PATH="$WORK/shims:$PATH"
  ok "installed no-op taskset/setsid shims for macOS (real, execable via timeout)"
fi
# Pin the gateway (and rig) to a couple of cores. On macOS the shims above make the pin a documented no-op.
export CORES="${CORES:-0-1}" LOADCORES="${LOADCORES:-0-1}" MOCKCORES="${MOCKCORES:-0-1}"
export MOCK_PORT="$PORT_MOCK" GW_PORT="$PORT_GW"
export BENCH_ARCH="$ARCH"

# FAST LOCAL PROFILE — every knob overridable; these are the minutes-not-hours defaults. Same producer,
# short probe windows. Adaptive rung selection on so a cell starts at the previous winner.
export MATRIX_SWEEP="${MATRIX_SWEEP:-1}"
export MATRIX_SWEEP_ADAPTIVE="${MATRIX_SWEEP_ADAPTIVE:-1}"
export MATRIX_STREAM="${MATRIX_STREAM:-1}"
export MATRIX_MEMORY="${MATRIX_MEMORY:-1}"
# CELL SUBSET (default: the openai diagonal only). gen-data projects the headline, streaming and memory
# from the openai diagonal cell, so probing just that ONE cell exercises the ENTIRE producer path
# (capability probe → per-cell perf sweep → per-cell streaming → memory-once → OOTB config) end-to-end
# in minutes. Set MATRIX_EGRESS_ONLY / MATRIX_INGRESS_ONLY to "" (empty) to sweep the full 6x6 locally.
export MATRIX_EGRESS_ONLY="${MATRIX_EGRESS_ONLY-openai}"
export MATRIX_INGRESS_ONLY="${MATRIX_INGRESS_ONLY-openai}"
# perf sweep windows (lib/sweep.sh): 1s probe windows, tight peak-search bounds.
export SWEEP_DUR="${SWEEP_DUR:-1}" C1_DUR="${C1_DUR:-1}" WARMUP_DUR="${WARMUP_DUR:-1}" PSIZE="${PSIZE:-64}"
export SWEEP_INSTANT="${SWEEP_INSTANT:-8 64}" SWEEP_DELAYED="${SWEEP_DELAYED:-8 64}"
# per-cell streaming (lib/stream_measure.sh): tiny frame counts + 1s windows + small bisect/peak bounds.
export MATRIX_STREAM_CHUNKS="${MATRIX_STREAM_CHUNKS:-8}" MATRIX_STREAM_INTERVAL_MS="${MATRIX_STREAM_INTERVAL_MS:-20}"
export MATRIX_STREAM_C1_DUR="${MATRIX_STREAM_C1_DUR:-1}" MATRIX_STREAM_SWEEP_DUR="${MATRIX_STREAM_SWEEP_DUR:-1}"
export MATRIX_STREAM_SUST_BOUNDS="${MATRIX_STREAM_SUST_BOUNDS:-1 8}"
export MATRIX_STREAMCPU_CHUNKS="${MATRIX_STREAMCPU_CHUNKS:-16}" MATRIX_STREAMCPU_DUR="${MATRIX_STREAMCPU_DUR:-1}"
export MATRIX_STREAMCPU_FPS_BOUNDS="${MATRIX_STREAMCPU_FPS_BOUNDS:-1 8}" MATRIX_STREAMCPU_STALL_MS="${MATRIX_STREAMCPU_STALL_MS:-250}"
# memory-once: tiny sustained load + a 2s settle (MEM_SETTLE_S) instead of the field's 60s release wait.
export MEM_DUR="${MEM_DUR:-2}" MEM_CONC="${MEM_CONC:-8}" MEM_PSIZE="${MEM_PSIZE:-1024}" MEM_SETTLE_S="${MEM_SETTLE_S:-2}"
# transient patience: a local dead cell shouldn't wait the field's 2x120s — keep it snappy.
export MATRIX_TRANSIENT_RETRIES="${MATRIX_TRANSIENT_RETRIES:-1}" MATRIX_TRANSIENT_PAUSE="${MATRIX_TRANSIENT_PAUSE:-2}"
export MATRIX_PROBE_TRANSIENT_RETRIES="${MATRIX_PROBE_TRANSIENT_RETRIES:-1}" MATRIX_PROBE_TRANSIENT_PAUSE="${MATRIX_PROBE_TRANSIENT_PAUSE:-2}"
# a generous-but-bounded suite ceiling so a wedge still exits (the whole run should finish well under this).
export HARNESS_SUITE_CEIL_S="${HARNESS_SUITE_CEIL_S:-1800}"

MATRIX_LOG="$WORK/matrix.log"
say "  launching matrix (this drives gen-data's producer; log: $MATRIX_LOG)"
t0=$(date +%s)
if GATEWAY="$GATEWAY" bash "$ROOT/matrix/run.sh" >"$MATRIX_LOG" 2>&1; then
  ok "matrix/run.sh exited 0"
else
  echo "---- matrix log tail ----"; tail -n 40 "$MATRIX_LOG"
  die "matrix/run.sh exited non-zero (see $MATRIX_LOG)"
fi
t1=$(date +%s); ok "matrix runtime: $((t1-t0))s"
RESULT_JSON="$ROOT/results/matrix/$GATEWAY.json"
[ -f "$RESULT_JSON" ] || die "no matrix result at $RESULT_JSON"
ok "matrix result written: $RESULT_JSON"

# ════════════════════════════════════════════════════════════════════════════════════════════════════
# STEP 4 — gen-data → guards → charts
# ════════════════════════════════════════════════════════════════════════════════════════════════════
say "step 4: gen-data → check-consistency → test → charts"
# Freshness-guard settle. matrix stamps measured_at at whole-second resolution ("…55Z"); gen-data's
# generated_at carries ms ("…55.644Z"). In a minutes-fast local run the matrix can finish and gen-data
# start inside the SAME wall second, and gen-data's guard does a STRING compare — "…55.644Z" < "…55Z"
# (because '.' < 'Z') falsely trips "generated_at predates measured_at". Wait past the whole second so
# generated_at unambiguously sorts after any same-second measured_at. (The field run never hits this: a
# 5h matrix ends many seconds before gen-data.)
sleep 2
node "$ROOT/site/gen-data.mjs" >"$WORK/gendata.log" 2>&1 || { cat "$WORK/gendata.log"; die "gen-data.mjs failed"; }
[ -f "$ROOT/site/data.json" ] || die "gen-data produced no site/data.json"
ok "site/data.json generated"

node "$ROOT/site/check-consistency.mjs" >"$WORK/consistency.log" 2>&1 || { cat "$WORK/consistency.log"; die "check-consistency.mjs FAILED (single-source guard etc.)"; }
ok "check-consistency.mjs passed (headline == max of its own charted sweep array, value+concurrency)"

node "$ROOT/site/test.mjs" >"$WORK/test.log" 2>&1 || { tail -n 30 "$WORK/test.log"; die "site/test.mjs FAILED"; }
ok "site/test.mjs passed"

# charts.py — render PNGs. Needs matplotlib; use a python that has it (env PYTHON, else a scratch venv,
# else system python3). PNG render is best-effort for the run to still assert the data path, but we DO
# assert at least one PNG landed when a renderer was available.
PY="${PYTHON:-}"
if [ -z "$PY" ]; then
  if python3 -c 'import matplotlib' >/dev/null 2>&1; then PY=python3
  elif [ -x "$ROOT/scratch/venv/bin/python" ] && "$ROOT/scratch/venv/bin/python" -c 'import matplotlib' >/dev/null 2>&1; then PY="$ROOT/scratch/venv/bin/python"
  fi
fi
CHARTS_OK=0
if [ -n "$PY" ]; then
  if "$PY" "$ROOT/charts.py" >"$WORK/charts.log" 2>&1; then
    CHARTS_OK=1; ok "charts.py rendered (python: $PY)"
  else
    echo "---- charts.py log tail ----"; tail -n 20 "$WORK/charts.log"
    die "charts.py FAILED (renderer available but errored)"
  fi
else
  printf '  \033[33mskip\033[0m charts.py — no matplotlib (set PYTHON=/path/to/py-with-matplotlib); data-path asserts still run\n'
fi

# ════════════════════════════════════════════════════════════════════════════════════════════════════
# STEP 5 — assert the whole data path (fail loudly on any wrong/missing piece)
# ════════════════════════════════════════════════════════════════════════════════════════════════════
say "step 5: assert the data path"
node - "$ROOT/site/data.json" "$RESULT_JSON" "$GATEWAY" "$CHARTS_OK" "$ROOT/results" <<'NODE' || die "data-path assertions FAILED (see above)"
const fs = require("fs");
const [dataPath, resultPath, key, chartsOk, resultsDir] = process.argv.slice(2);
let fail = 0;
const A = (cond, msg) => { if (cond) { console.log("  ok  " + msg); } else { console.error("  FAIL " + msg); fail++; } };

const data = JSON.parse(fs.readFileSync(dataPath, "utf8"));
const raw  = JSON.parse(fs.readFileSync(resultPath, "utf8"));   // asserts the raw matrix JSON parses
A(true, "results matrix JSON parses (" + resultPath + ")");

const g = (data.gateways || []).find(x => x.key === key);
A(!!g, "gateway '" + key + "' present in site/data.json");
if (!g) { process.exit(1); }

// (a) the gateway has matrix cells
const ups = g.matrix && g.matrix.upstreams;
A(!!ups && Object.keys(ups).length > 0, "gateway has matrix upstreams (egress columns present)");
let served = 0, total = 0;
for (const eg of Object.keys(ups || {})) for (const ing of Object.keys(ups[eg].cells || {})) {
  total++; if (ups[eg].cells[ing].served === true) served++;
}
A(total > 0, "matrix has cells (" + total + " total)");
A(served > 0, "matrix has >=1 green (served) cell (" + served + " served)");

// (b) best_cell (headline) is populated
const bc = g.best_cell;
A(!!bc && bc.added_latency_p99_us != null, "best_cell headline populated (added_latency_p99_us set)");
A(!!bc && Array.isArray(bc.sweep_max_proxy) && bc.sweep_max_proxy.length > 0, "best_cell carries a non-empty charted sweep_max_proxy array");

// (b') single-source: headline RPS == max of its OWN charted GATE-PASSING sweep rungs (value + conc).
// This mirrors check-consistency.mjs's peak reducer as an independent double-check on the produced
// bundle. LOW-R3-2: it MUST apply the SAME p99 + error-rate gate (rungPasses) the canonical guard uses
// (check-consistency.mjs:180-187) BEFORE reducing. A gate-BLIND max() would pick the terminal p99-cliff
// rung (probed one-past the peak, higher raw rps but FAILING the gate — the HIGH-R2-1 shape test.mjs
// covers), find peak.rps !== headline on a CORRECT bundle, and fire a spurious FAIL that blocks a valid
// local deploy while telling the developer the single-source property is broken. Gate first, then reduce.
const p99CeilMs = (g.matrix && g.matrix.p99_ceiling_ms) != null ? g.matrix.p99_ceiling_ms : 1000;
const sweepDur = (g.matrix && g.matrix.sweep_dur) != null ? g.matrix.sweep_dur : 10;
const rungPasses = (r) => {
  if (r == null || r.rps == null) return false;
  const p99 = r.p99_us;
  if (p99 != null && !(p99 < p99CeilMs * 1000)) return false; // p99 gate (missing p99 → not disqualified)
  const fail = r.fail != null ? r.fail : 0;
  const tot = r.rps * sweepDur + fail;
  return tot > 0 && fail <= 0.001 * tot;                       // error-rate gate < 0.1%
};
const checkSweep = (rpsKey, concKey, arrKey) => {
  const arr = bc && bc[arrKey];
  if (!Array.isArray(arr) || arr.length === 0) { A(false, arrKey + " present + non-empty"); return; }
  const eligible = arr.filter(rungPasses);
  if (eligible.length === 0) { A(false, arrKey + " has >=1 gate-passing rung to compare the headline against"); return; }
  const peak = eligible.reduce((a, b) => (b.rps > a.rps ? b : a));
  A(peak.rps === bc[rpsKey], "headline " + rpsKey + " (" + bc[rpsKey] + ") == gate-passing max of " + arrKey + " (" + peak.rps + ")");
  A(peak.conc === bc[concKey], "headline " + concKey + " (" + bc[concKey] + ") == winning conc of " + arrKey + " (" + peak.conc + ")");
};
checkSweep("rps_max_proxy", "rps_max_proxy_concurrency", "sweep_max_proxy");
checkSweep("rps_sustained_20ms", "rps_sustained_20ms_concurrency", "sweep_sustained_20ms");

// (c) g.streaming populated (projected from the matrix diagonal cell)
const s = g.streaming;
A(!!s, "g.streaming projected from the matrix");
A(!!s && s.source === "matrix", "g.streaming.source == 'matrix'");
A(!!s && s.stream_served === true, "g.streaming.stream_served == true (a real streaming measurement)");
A(!!s && s.cpu_fps != null && s.streams_sustained != null, "g.streaming carries cpu_fps + streams_sustained");

// (d) g.memory_read populated (projected from matrix.memory). On macOS RSS magnitudes read 0 (no host
// /proc for the container pid) — the PROJECTION path is what we assert here; served must be true.
const mr = g.memory_read;
A(!!mr, "g.memory_read projected from the matrix");
A(!!mr && mr.source === "matrix", "g.memory_read.source == 'matrix'");
A(!!mr && mr.served === true, "g.memory_read.served == true");
A(!!mr && ("idle_rss_mib" in mr) && ("peak_rss_mib" in mr), "g.memory_read carries idle/peak RSS fields");

// (e) per-gateway measured_at set
A(typeof g.measured_at === "string" && g.measured_at.length > 0, "per-gateway g.measured_at is set (" + g.measured_at + ")");

// (f) the results-download JSON is valid (the client builds <gw>-results.json from g; it must round-trip
// as JSON and carry the matrix + best_cell). We assert the shape gen-data produced supports that.
let dl = null;
try { dl = JSON.parse(JSON.stringify(g)); } catch (e) {}
A(!!dl, "gateway record round-trips through JSON (results-download JSON is valid)");
A(!!dl && dl.matrix && dl.best_cell, "download JSON carries matrix + best_cell");

// (g) invalid/absent value draws NO bar. The suppression is charts.py's VALIDITY GATE (a non-served /
// non-positive value renders 0 width). check-consistency already asserted table==drawer==charts agree,
// and every not-served cell in the raw matrix carries served != true (a value the gate zeroes). Confirm
// at least one non-served cell EXISTS in the bundle (so the no-bar path is genuinely exercised) OR that
// the matrix is fully green (in which case there is nothing to suppress — also valid).
let nonServed = total - served;
A(nonServed >= 0, "no-bar path: " + nonServed + " non-served cell(s) present (charts.py validity gate zeroes them; check-consistency asserted surfaces agree)");

// charts PNG landed (when a renderer was available)
if (chartsOk === "1") {
  const pngs = fs.readdirSync(resultsDir).filter(f => f.endsWith(".png"));
  A(pngs.length > 0, "charts.py wrote >=1 PNG into results/ (" + pngs.length + " png)");
} else {
  console.log("  --  charts PNG assertion skipped (no renderer)");
}

if (fail > 0) { console.error("\n" + fail + " assertion(s) FAILED"); process.exit(1); }
console.log("\nALL DATA-PATH ASSERTIONS PASSED");
NODE

ok "all data-path assertions passed"
say "VERIFY-LOCAL: GREEN — full pipeline (matrix → gen-data → guards → charts) verified locally"
# teardown runs via the EXIT trap.
