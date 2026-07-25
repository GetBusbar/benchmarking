# lib/rig.sh — fetch the prebuilt bench "rig" (mock + loadgen) so a bench box needs NO build
# toolchain: bare OS + docker is enough. Downloads mock-<arch> / ugen-<arch> from the benchmarking
# `rig` GitHub release (rebuilt by .github/workflows/bench-rig.yml on every mock/ or loadgen/ change,
# for both arm64 and x86). Sets $MOCK and $UGEN. Idempotent — cached under bin/.
#
# HONESTY: which binary won is logged LOUDLY. A field/CI run must FAIL if the prebuilt rig can't be
# fetched rather than silently substituting a stale/locally-modified build that would produce numbers
# the caller believes came from the pinned reproducible rig (audit M4). The source fallback is
# therefore OPT-IN: set RIG_ALLOW_SOURCE=1 (local dev only) to permit building from the local tree.
#
# RIG PROVENANCE (audit #21 — the measurement instrument must describe itself). `rig` is a MOVING tag:
# .github/workflows/bench-rig.yml force-pushes it on every mock/ or loadgen/ change, so two runs weeks
# apart can silently use DIFFERENT binaries under the same URL. That is not hypothetical — the mock's
# request_shape_ok was tightened mid-week (bcf9912: bedrock/cohere began rejecting a raw OpenAI body
# forwarded verbatim) and the assets were rebuilt 2026-07-24T19:03Z, which changed cell VERDICTS between
# two runs of an otherwise identical harness. Establishing that took a long investigation, because
# nothing in either run's output recorded which instrument produced it.
#
# So every run now records what it actually executed: a sha256 of the mock and ugen binaries, the origin
# (release download / cached / local override / source build) and, best-effort, the release asset's
# updated_at from the GitHub API. rig_provenance_json emits that block; matrix/run.sh embeds it in the
# snapshot. A future cross-run comparison can then tell IMMEDIATELY whether the instrument changed,
# instead of inferring it from a behaviour change weeks later.
RIG_URL="${RIG_URL:-https://github.com/GetBusbar/benchmarking/releases/download/rig}"
# The API endpoint for the same moving tag, used only to read asset updated_at stamps (best-effort).
RIG_API="${RIG_API:-https://api.github.com/repos/GetBusbar/benchmarking/releases/tags/rig}"
_rig_log(){ echo "[rig] $*" >&2; }

# RIG_MOCK_ORIGIN / RIG_UGEN_ORIGIN: how each binary was obtained this run. Set by fetch_rig.
RIG_MOCK_ORIGIN="" RIG_UGEN_ORIGIN=""

# _rig_sha256 <file> -> the hex digest, or EMPTY when it cannot be computed. Never a placeholder: an
# unknown digest must read as null downstream, never as a digest that happens to match nothing.
_rig_sha256(){
  [ -n "${1:-}" ] && [ -r "$1" ] || return 0
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" 2>/dev/null | awk '{print $1}'
  elif command -v shasum   >/dev/null 2>&1; then shasum -a 256 "$1" 2>/dev/null | awk '{print $1}'
  fi
}

# _rig_asset_updated_at <asset-name> -> the release asset's updated_at (ISO), or EMPTY. Best-effort and
# strictly non-fatal: no network, no jq, a rate-limited API or a private repo all yield EMPTY -> null.
# The sha256 above is the AUTHORITATIVE identity; this stamp is human-readable corroboration.
_rig_asset_updated_at(){
  command -v curl >/dev/null 2>&1 || return 0
  local body; body="$(curl -fsSL --max-time 10 "$RIG_API" 2>/dev/null)" || return 0
  [ -n "$body" ] || return 0
  if command -v jq >/dev/null 2>&1; then
    printf '%s' "$body" | jq -r --arg n "$1" '.assets[]? | select(.name==$n) | .updated_at // empty' 2>/dev/null | head -1
  else
    # jq-less fallback: the assets array is one object per asset; find the one naming this file.
    printf '%s' "$body" | tr '}' '\n' | grep -F "\"name\":\"$1\"" \
      | sed -n 's/.*"updated_at":"\([^"]*\)".*/\1/p' | head -1
  fi
}

# _rig_json_str <value> -> a JSON string, or the literal null when empty. The ONE null-safe choke point
# so an unmeasurable field can never be emitted as "" (which downstream would read as a real value).
_rig_json_str(){ if [ -z "${1:-}" ]; then printf 'null'; else printf '"%s"' "$1"; fi; }

# rig_provenance_json -> the "rig" object describing THE INSTRUMENT THIS RUN USED. Every field is
# null-safe; a consumer that predates this block simply sees no "rig" key at all.
rig_provenance_json(){
  local arch="${BENCH_ARCH:-arm64}"
  local msha usha mat uat
  msha="$(_rig_sha256 "${MOCK:-}")"; usha="$(_rig_sha256 "${UGEN:-}")"
  # Only ask the API about binaries that actually CAME from the release; a local build has no asset.
  case "${RIG_MOCK_ORIGIN:-}" in release|cached) mat="$(_rig_asset_updated_at "mock-$arch")";; esac
  case "${RIG_UGEN_ORIGIN:-}" in release|cached) uat="$(_rig_asset_updated_at "ugen-$arch")";; esac
  printf '{"arch": %s, "release_url": %s, "mock": {"origin": %s, "sha256": %s, "asset_updated_at": %s}, "ugen": {"origin": %s, "sha256": %s, "asset_updated_at": %s}}' \
    "$(_rig_json_str "$arch")" "$(_rig_json_str "$RIG_URL")" \
    "$(_rig_json_str "${RIG_MOCK_ORIGIN:-}")" "$(_rig_json_str "$msha")" "$(_rig_json_str "$mat")" \
    "$(_rig_json_str "${RIG_UGEN_ORIGIN:-}")" "$(_rig_json_str "$usha")" "$(_rig_json_str "$uat")"
}
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
    RIG_MOCK_ORIGIN="local-override"; RIG_UGEN_ORIGIN="local-override"
    _rig_log "mock: LOCAL OVERRIDE RIG_MOCK_CMD=$MOCK — NOT the pinned GitHub rig (local dev)"
    _rig_log "ugen: LOCAL OVERRIDE RIG_UGEN_CMD=$UGEN — NOT the pinned GitHub rig (local dev)"
    return 0
  fi
  # Cache under an ARCH-STAMPED name (audit R3-LOW-3). Keying only on "bin/mock is executable" silently
  # reused a wrong-arch binary on a reused local workdir when BENCH_ARCH was switched (an arm64 binary
  # passes -x on an arm64 host), attributing numbers to the wrong rig. The arch in the filename makes a
  # switch re-fetch instead of reuse. EC2 boxes are unaffected (rsync --exclude bin gives a clean bin/).
  MOCK="$root/bin/mock-$arch"; UGEN="$root/bin/ugen-$arch"
  if [ -x "$MOCK" ]; then RIG_MOCK_ORIGIN="cached"; _rig_log "mock: reusing cached $MOCK"; fi
  if [ -x "$UGEN" ]; then RIG_UGEN_ORIGIN="cached"; fi
  if [ ! -x "$MOCK" ]; then
    err="$(curl -fsSL "$RIG_URL/mock-$arch" -o "$MOCK" 2>&1)"
    if [ $? -eq 0 ] && [ -s "$MOCK" ]; then
      chmod +x "$MOCK"; RIG_MOCK_ORIGIN="release"; _rig_log "mock: prebuilt mock-$arch ($RIG_URL/mock-$arch)"
    elif [ "${RIG_ALLOW_SOURCE:-0}" = 1 ] && ( cd "$root/mock" && cargo build --release >/dev/null 2>&1 ); then
      cp "$root/mock/target/release/mock" "$MOCK"
      RIG_MOCK_ORIGIN="source-build"
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
      chmod +x "$UGEN"; RIG_UGEN_ORIGIN="release"; _rig_log "ugen: prebuilt ugen-$arch ($RIG_URL/ugen-$arch)"
    elif [ "${RIG_ALLOW_SOURCE:-0}" = 1 ] && go build -o "$UGEN" "$root/loadgen/ugen.go" 2>/dev/null; then
      RIG_UGEN_ORIGIN="source-build"
      _rig_log "ugen: FELL BACK to local go build (RIG_ALLOW_SOURCE=1) — NOT the pinned rig"
    else
      _rig_log "FATAL ugen: cannot fetch $RIG_URL/ugen-$arch (${err:-download failed})"
      [ "${RIG_ALLOW_SOURCE:-0}" = 1 ] || _rig_log "  (source fallback is opt-in: set RIG_ALLOW_SOURCE=1 for local dev)"
      return 1
    fi
  fi
}
