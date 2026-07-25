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

# BOX QUALIFICATION PROVENANCE (the same idea, one step further out). The rig binaries are one half of
# the instrument; the BOX they run on is the other. A contaminated box publishes a fake gateway
# regression exactly as a silently-rebuilt mock does: that happened to one gateway on 2026-07-25,
# whose no-gateway floor (77-81us) sat INSIDE the healthy population's absolute range (73-82us) yet was
# 5.4% off its own prior run, and whose peak throughput collapsed 86%. So the box's qualifying
# measurement — the no-gateway floor median + jitter stats, and the gateway's own peak replay, with the
# computed drift percentages and the bands they were judged against — is recorded INSIDE this same
# provenance block rather than in a second store: one place to look when two runs disagree, and one
# place to query when the (deliberately provisional) bands get recalibrated from repeat runs.
#
# The orchestrator (run-on-ec2.sh) writes the qualification verdict to $BOX_QUALIFY_FILE on the box
# BEFORE the 6x6 starts; matrix/run.sh's snapshot then carries it with no change of its own. Wholly
# best-effort: no file (a local run, an older harness) -> the key is simply absent, never fabricated.
# The content is emitted only after python confirms it PARSES as a JSON object, so a truncated or
# garbage file can never corrupt the snapshot it is folded into.
_rig_box_qualify_json(){
  local f="${BOX_QUALIFY_FILE:-${RIG_ROOT:-.}/results/box-qualify/qualification.json}"
  [ -r "$f" ] || return 0
  python3 - "$f" <<'PY' 2>/dev/null
import json, sys
try:
    with open(sys.argv[1]) as fh:
        j = json.load(fh)
except Exception:
    sys.exit(0)
if isinstance(j, dict):
    sys.stdout.write(json.dumps(j))
PY
}

# rig_provenance_json -> the "rig" object describing THE INSTRUMENT THIS RUN USED. Every field is
# null-safe; a consumer that predates this block simply sees no "rig" key at all.
rig_provenance_json(){
  local arch="${BENCH_ARCH:-arm64}"
  # INITIALISED, not merely declared: `local mat` leaves the name UNSET, and matrix/run.sh runs under
  # `set -u`, so the "$mat" expansion below aborted the command substitution and printed NOTHING —
  # emitting `"asset_updated_at": }`, i.e. INVALID JSON, for every run whose rig did not come from the
  # release (any local-override / local-build rig). The snapshot writer then failed to parse its own
  # output and the whole run aborted. Empty string is what _rig_json_str turns into a literal null.
  local msha usha mat="" uat=""
  msha="$(_rig_sha256 "${MOCK:-}")"; usha="$(_rig_sha256 "${UGEN:-}")"
  # Only ask the API about binaries that actually CAME from the release; a local build has no asset.
  case "${RIG_MOCK_ORIGIN:-}" in release|cached) mat="$(_rig_asset_updated_at "mock-$arch")";; esac
  case "${RIG_UGEN_ORIGIN:-}" in release|cached) uat="$(_rig_asset_updated_at "ugen-$arch")";; esac
  # Empty (no qualification file / unparseable) -> the literal null, exactly like every other field
  # here: a consumer must be able to tell "this run was not qualified" from "it qualified at 0".
  local bq; bq="$(_rig_box_qualify_json)"
  local eng; eng="$(_rig_engine_json)"
  printf '{"arch": %s, "release_url": %s, "engine": %s, "mock": {"origin": %s, "sha256": %s, "asset_updated_at": %s}, "ugen": {"origin": %s, "sha256": %s, "asset_updated_at": %s}, "box_qualify": %s}' \
    "$(_rig_json_str "$arch")" "$(_rig_json_str "$RIG_URL")" \
    "${eng:-null}" \
    "$(_rig_json_str "${RIG_MOCK_ORIGIN:-}")" "$(_rig_json_str "$msha")" "$(_rig_json_str "$mat")" \
    "$(_rig_json_str "${RIG_UGEN_ORIGIN:-}")" "$(_rig_json_str "$usha")" "$(_rig_json_str "$uat")" \
    "${bq:-null}"
}
# ── THE ENGINE STAMP ──────────────────────────────────────────────────────────────────────────────
# WHY. Results are only comparable if they were produced by the SAME harness. A memory bug on one
# gateway is a memory bug on all thirteen, so a fix mid-field invalidates every number taken before
# it, and a board that mixes engines is comparing gateways through two different instruments. That
# already happened here: a mock rebuild landed between two runs and silently changed cell verdicts
# across the whole field, and it cost hours to work out that the instrument had moved rather than the
# gateways. Recording the commit makes that a one-line check instead of an investigation.
#
# CAPTURED ORCHESTRATOR-SIDE. run-on-ec2.sh exports BENCH_ENGINE_COMMIT/_DIRTY before the harness is
# rsynced, because the copy that lands on the box has no .git to interrogate. The local git fallback
# below is for verify-local.sh and any in-tree run.
#
# DIRTY IS RECORDED, NOT HIDDEN. A run from a modified working tree is not identified by its commit,
# so `dirty: true` marks it as not reproducible. The board can then refuse to compare it rather than
# quietly treating it as if it were the commit it sits on.
_rig_engine_json(){
  local sha="${BENCH_ENGINE_COMMIT:-}" dirty="${BENCH_ENGINE_DIRTY:-}"
  if [ -z "$sha" ] && [ -n "${RIG_ROOT:-}" ] && git -C "$RIG_ROOT" rev-parse --git-dir >/dev/null 2>&1; then
    sha="$(git -C "$RIG_ROOT" rev-parse HEAD 2>/dev/null)"
    if [ -n "$(git -C "$RIG_ROOT" status --porcelain 2>/dev/null)" ]; then dirty=1; else dirty=0; fi
  fi
  [ -n "$sha" ] || { printf 'null'; return; }
  printf '{"commit": %s, "dirty": %s}' "$(_rig_json_str "$sha")" "$([ "$dirty" = 1 ] && echo true || echo false)"
}
fetch_rig() { # <repo-root>
  local root="$1" arch="${BENCH_ARCH:-arm64}" err
  # Remember the tree we were pointed at so rig_provenance_json can find the box-qualification file
  # relative to it without every caller having to thread a path through.
  RIG_ROOT="$root"
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
