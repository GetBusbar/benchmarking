# lib/rig.sh - fetch the prebuilt bench "rig" (engine + mock) so a bench box needs NO build
# toolchain: bare OS + docker is enough. Downloads mock-<arch> from the benchmarking `rig` GitHub
# release (rebuilt by .github/workflows/bench-rig.yml on every mock/ change, for both arm64 and x86).
# Sets $MOCK. Idempotent - cached under bin/.
#
# HONESTY: which binary won is logged loudly. A field/CI run must fail if the prebuilt rig can't be
# fetched, rather than silently substituting a stale/locally-modified build that would produce numbers
# the caller believes came from the pinned reproducible rig. The source fallback is therefore opt-in:
# set RIG_ALLOW_SOURCE=1 (local dev only) to permit building from the local tree.
#
# RIG PROVENANCE: the measurement instrument must describe itself. `rig` is a moving tag:
# .github/workflows/bench-rig.yml force-pushes it on every mock/ or loadgen/ change, so two runs weeks
# apart can silently use different binaries under the same URL, changing cell verdicts between runs of
# an otherwise identical harness with nothing in either run's output recording which instrument
# produced it. So every run now records what it actually executed: a sha256 of the mock binary, the
# origin (release download / cached / local override / source build) and, best-effort, the release
# asset's updated_at from the GitHub API. rig_provenance_json emits that block; matrix/run.sh embeds it
# in the snapshot, so a cross-run comparison can tell immediately whether the instrument changed.
RIG_URL="${RIG_URL:-https://github.com/GetBusbar/benchmarking/releases/download/rig}"
# The API endpoint for the same moving tag, used only to read asset updated_at stamps (best-effort).
RIG_API="${RIG_API:-https://api.github.com/repos/GetBusbar/benchmarking/releases/tags/rig}"
_rig_log(){ echo "[rig] $*" >&2; }

# RIG_MOCK_ORIGIN: how the mock binary was obtained this run. Set by fetch_rig.
RIG_MOCK_ORIGIN=""

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

# BOX QUALIFICATION PROVENANCE: the same idea, one step further out. The rig binaries are one half of
# the instrument; the box they run on is the other, and a contaminated box can publish a fake gateway
# regression just as a silently-rebuilt mock can, even when its absolute numbers still look inside the
# healthy range. So the box's qualifying measurement (the no-gateway floor median + jitter stats, the
# gateway's own peak replay, and the computed drift percentages and bands they were judged against) is
# recorded inside this same provenance block rather than in a second store: one place to look when two
# runs disagree, and one place to query when the bands get recalibrated from repeat runs.
#
# NOTHING WRITES $BOX_QUALIFY_FILE. The qualification verdict is produced by the ENGINE: `otb run`
# qualifies the box against OTB_QUALIFY_BASELINE (the median of observed_rps across prior snapshots,
# computed in run-on-ec2.sh and exported into the box's run script) and publishes it as
# `rig.box_qualify` inside the snapshot. The path below is a fallback that has never been populated,
# kept only so an out-of-band verdict file would still be read if one ever appeared. This comment used
# to state the write as fact, which reads as though that file were the source of truth.
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
  # Must be initialised, not merely declared: `local mat` alone leaves the name unset, and matrix/run.sh
  # runs under `set -u`, so the "$mat" expansion below would abort the command substitution for any run
  # whose rig did not come from the release (local-override / local-build), emitting invalid JSON.
  # Empty string is what _rig_json_str turns into a literal null.
  local msha mat=""
  msha="$(_rig_sha256 "${MOCK:-}")"
  # Only ask the API about binaries that actually CAME from the release; a local build has no asset.
  case "${RIG_MOCK_ORIGIN:-}" in release|cached) mat="$(_rig_asset_updated_at "mock-$arch")";; esac
  # Empty (no qualification file / unparseable) -> the literal null, exactly like every other field
  # here: a consumer must be able to tell "this run was not qualified" from "it qualified at 0".
  local bq; bq="$(_rig_box_qualify_json)"
  local eng; eng="$(_rig_engine_json)"
  printf '{"arch": %s, "release_url": %s, "engine": %s, "mock": {"origin": %s, "sha256": %s, "asset_updated_at": %s}, "box_qualify": %s}' \
    "$(_rig_json_str "$arch")" "$(_rig_json_str "$RIG_URL")" \
    "${eng:-null}" \
    "$(_rig_json_str "${RIG_MOCK_ORIGIN:-}")" "$(_rig_json_str "$msha")" "$(_rig_json_str "$mat")" \
    "${bq:-null}"
}
# ── THE ENGINE STAMP ──────────────────────────────────────────────────────────────────────────────
# WHY. Results are only comparable if they were produced by the same harness: a fix mid-field
# invalidates every number taken before it, and a board that mixes engines is comparing gateways
# through two different instruments. Recording the commit makes a mismatch a one-line check instead of
# an investigation.
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
    # SCOPE OUT results/, exactly as run-on-ec2.sh's preflight/stamp logic does: a prior PUBLISH=0 run
    # leaves results/ uncommitted, and a bare `status --porcelain` would flag the tree dirty from that
    # churn alone and stamp rig.engine.dirty:true even though engine/mock are byte-identical to HEAD.
    if [ -n "$(git -C "$RIG_ROOT" status --porcelain -- . ':(exclude)results' 2>/dev/null)" ]; then dirty=1; else dirty=0; fi
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
  # LOCAL-DEV OVERRIDE (opt-in, never on a field/CI box). The prebuilt rig is a Linux ELF; on a
  # non-Linux dev host (macOS) it cannot exec natively. RIG_MOCK_CMD lets a local verifier supply an
  # already-usable mock (e.g. as a --network-host Linux container wrapper) so the SAME harness code
  # path runs unchanged. Must be an executable path. When set we skip the GitHub fetch entirely and
  # honestly log the source.
  if [ -n "${RIG_MOCK_CMD:-}" ]; then
    MOCK="$RIG_MOCK_CMD"
    [ -x "$MOCK" ] || { _rig_log "FATAL mock: RIG_MOCK_CMD '$MOCK' is not executable"; return 1; }
    RIG_MOCK_ORIGIN="local-override"
    _rig_log "mock: LOCAL OVERRIDE RIG_MOCK_CMD=$MOCK - NOT the pinned GitHub rig (local dev)"
    return 0
  fi
  # Cache under an arch-stamped name. Keying only on "bin/mock is executable" silently
  # reused a wrong-arch binary on a reused local workdir when BENCH_ARCH was switched (an arm64 binary
  # passes -x on an arm64 host), attributing numbers to the wrong rig. The arch in the filename makes a
  # switch re-fetch instead of reuse. EC2 boxes are unaffected (rsync --exclude bin gives a clean bin/).
  MOCK="$root/bin/mock-$arch"
  if [ -x "$MOCK" ]; then RIG_MOCK_ORIGIN="cached"; _rig_log "mock: reusing cached $MOCK"; fi
  if [ ! -x "$MOCK" ]; then
    err="$(curl -fsSL "$RIG_URL/mock-$arch" -o "$MOCK" 2>&1)"
    if [ $? -eq 0 ] && [ -s "$MOCK" ]; then
      chmod +x "$MOCK"; RIG_MOCK_ORIGIN="release"; _rig_log "mock: prebuilt mock-$arch ($RIG_URL/mock-$arch)"
    elif [ "${RIG_ALLOW_SOURCE:-0}" = 1 ] && ( cd "$root/mock" && cargo build --release >/dev/null 2>&1 ); then
      cp "$root/mock/target/release/mock" "$MOCK"
      RIG_MOCK_ORIGIN="source-build"
      _rig_log "mock: FELL BACK to local cargo build (RIG_ALLOW_SOURCE=1) - NOT the pinned rig"
    else
      _rig_log "FATAL mock: cannot fetch $RIG_URL/mock-$arch (${err:-download failed})"
      [ "${RIG_ALLOW_SOURCE:-0}" = 1 ] || _rig_log "  (source fallback is opt-in: set RIG_ALLOW_SOURCE=1 for local dev)"
      return 1
    fi
  fi
}
