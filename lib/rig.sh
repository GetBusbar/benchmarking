# lib/rig.sh — fetch the prebuilt bench "rig" (mock + loadgen) so a bench box needs NO build
# toolchain: bare OS + docker is enough. Downloads mock-<arch> / ugen-<arch> from the benchmarking
# `rig` GitHub release (rebuilt by .github/workflows/bench-rig.yml on every mock/ or loadgen/ change,
# for both arm64 and x86). Sets $MOCK and $UGEN. Idempotent — cached under bin/.
#
# HONESTY: which binary won is logged LOUDLY. A field/CI run must FAIL if the prebuilt rig can't be
# fetched rather than silently substituting a stale/locally-modified build that would produce numbers
# the caller believes came from the pinned reproducible rig (audit M4). The source fallback is
# therefore OPT-IN: set RIG_ALLOW_SOURCE=1 (local dev only) to permit building from the local tree.
RIG_URL="${RIG_URL:-https://github.com/GetBusbar/benchmarking/releases/download/rig}"
_rig_log(){ echo "[rig] $*" >&2; }
fetch_rig() { # <repo-root>
  local root="$1" arch="${BENCH_ARCH:-arm64}" err
  mkdir -p "$root/bin"
  # LOCAL-DEV OVERRIDE (audit: opt-in, never on a field/CI box). The prebuilt rig is a Linux ELF; on a
  # non-Linux dev host (macOS) it cannot exec natively. RIG_MOCK_CMD / RIG_UGEN_CMD let a local verifier
  # supply an already-usable mock + loadgen (e.g. the mock as a --network-host Linux container wrapper +
  # a natively-built ugen) so the SAME harness code path runs unchanged. Both must be set together; each
  # must be an executable path. When set we skip the GitHub fetch entirely and honestly log the source.
  if [ -n "${RIG_MOCK_CMD:-}" ] && [ -n "${RIG_UGEN_CMD:-}" ]; then
    MOCK="$RIG_MOCK_CMD"; UGEN="$RIG_UGEN_CMD"
    [ -x "$MOCK" ] || { _rig_log "FATAL mock: RIG_MOCK_CMD '$MOCK' is not executable"; return 1; }
    [ -x "$UGEN" ] || { _rig_log "FATAL ugen: RIG_UGEN_CMD '$UGEN' is not executable"; return 1; }
    _rig_log "mock: LOCAL OVERRIDE RIG_MOCK_CMD=$MOCK — NOT the pinned GitHub rig (local dev)"
    _rig_log "ugen: LOCAL OVERRIDE RIG_UGEN_CMD=$UGEN — NOT the pinned GitHub rig (local dev)"
    return 0
  fi
  # Cache under an ARCH-STAMPED name (audit R3-LOW-3). Keying only on "bin/mock is executable" silently
  # reused a wrong-arch binary on a reused local workdir when BENCH_ARCH was switched (an arm64 binary
  # passes -x on an arm64 host), attributing numbers to the wrong rig. The arch in the filename makes a
  # switch re-fetch instead of reuse. EC2 boxes are unaffected (rsync --exclude bin gives a clean bin/).
  MOCK="$root/bin/mock-$arch"; UGEN="$root/bin/ugen-$arch"
  if [ -x "$MOCK" ]; then _rig_log "mock: reusing cached $MOCK"; fi
  if [ ! -x "$MOCK" ]; then
    err="$(curl -fsSL "$RIG_URL/mock-$arch" -o "$MOCK" 2>&1)"
    if [ $? -eq 0 ] && [ -s "$MOCK" ]; then
      chmod +x "$MOCK"; _rig_log "mock: prebuilt mock-$arch ($RIG_URL/mock-$arch)"
    elif [ "${RIG_ALLOW_SOURCE:-0}" = 1 ] && ( cd "$root/mock" && cargo build --release >/dev/null 2>&1 ); then
      cp "$root/mock/target/release/mock" "$MOCK"
      _rig_log "mock: FELL BACK to local cargo build (RIG_ALLOW_SOURCE=1) — NOT the pinned rig"
    else
      _rig_log "FATAL mock: cannot fetch $RIG_URL/mock-$arch (${err:-download failed})"
      [ "${RIG_ALLOW_SOURCE:-0}" = 1 ] || _rig_log "  (source fallback is opt-in: set RIG_ALLOW_SOURCE=1 for local dev)"
      return 1
    fi
  fi
  if [ ! -x "$UGEN" ]; then
    err="$(curl -fsSL "$RIG_URL/ugen-$arch" -o "$UGEN" 2>&1)"
    if [ $? -eq 0 ] && [ -s "$UGEN" ]; then
      chmod +x "$UGEN"; _rig_log "ugen: prebuilt ugen-$arch ($RIG_URL/ugen-$arch)"
    elif [ "${RIG_ALLOW_SOURCE:-0}" = 1 ] && go build -o "$UGEN" "$root/loadgen/ugen.go" 2>/dev/null; then
      _rig_log "ugen: FELL BACK to local go build (RIG_ALLOW_SOURCE=1) — NOT the pinned rig"
    else
      _rig_log "FATAL ugen: cannot fetch $RIG_URL/ugen-$arch (${err:-download failed})"
      [ "${RIG_ALLOW_SOURCE:-0}" = 1 ] || _rig_log "  (source fallback is opt-in: set RIG_ALLOW_SOURCE=1 for local dev)"
      return 1
    fi
  fi
}
