#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Regression guard for lib/rig.sh - the PROVENANCE mechanism RULES.md calls the project's core: which
# instrument (engine + mock binary) a run actually executed, recorded inside every snapshot. Nothing
# in lib/ exercised any of it, so an edit that inverted the RIG_ALLOW_SOURCE gate, mislabelled
# RIG_MOCK_ORIGIN, or reintroduced the false-dirty bug in the engine stamp would publish mis-attributed
# provenance with zero test failure. This sources rig.sh and drives its functions directly - no network
# (curl is stubbed), no cargo, no EC2.
#
# Run: bash lib/rig_test.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$HERE/lib/rig.sh"
fail=0
check(){ local n="$1" got="$2" want="$3"; if [ "$got" = "$want" ]; then echo "ok   - $n"; else echo "FAIL - $n: got [$got] want [$want]"; fail=1; fi; }
contains(){ local n="$1" hay="$2" needle="$3"; case "$hay" in *"$needle"*) echo "ok   - $n";; *) echo "FAIL - $n: [$hay] lacks [$needle]"; fail=1;; esac; }

# ── _rig_engine_json: the orchestrator-exported path (BENCH_ENGINE_COMMIT/_DIRTY) ───────────────────
out="$(BENCH_ENGINE_COMMIT="abc123" BENCH_ENGINE_DIRTY=0 RIG_ROOT="" _rig_engine_json)"
check "engine stamp: an exported clean commit reports dirty:false" "$out" '{"commit": "abc123", "dirty": false}'
out="$(BENCH_ENGINE_COMMIT="abc123" BENCH_ENGINE_DIRTY=1 RIG_ROOT="" _rig_engine_json)"
check "engine stamp: an exported dirty commit reports dirty:true" "$out" '{"commit": "abc123", "dirty": true}'

# ── _rig_engine_json: the local-git fallback, and its results/-scoped dirty check (u9-conformance-1) ─
if command -v git >/dev/null 2>&1; then
  repo="$(mktemp -d)"
  git -C "$repo" init -q
  git -C "$repo" config user.email t@t >/dev/null 2>&1
  git -C "$repo" config user.name  t   >/dev/null 2>&1
  mkdir -p "$repo/engine"; printf 'x\n' > "$repo/engine/f"
  git -C "$repo" add -A >/dev/null 2>&1
  git -C "$repo" commit -qm init >/dev/null 2>&1
  head="$(git -C "$repo" rev-parse HEAD)"

  out="$(BENCH_ENGINE_COMMIT="" BENCH_ENGINE_DIRTY="" RIG_ROOT="$repo" _rig_engine_json)"
  contains "engine stamp (git fallback): reports the tree's HEAD commit" "$out" "$head"
  contains "engine stamp (git fallback): a clean tree is dirty:false" "$out" '"dirty": false'

  # THE u9-conformance-1 ASSERTION: uncommitted churn UNDER results/ must NOT flag the tree dirty.
  mkdir -p "$repo/results"; printf 'junk\n' > "$repo/results/partial.json"
  out="$(BENCH_ENGINE_COMMIT="" BENCH_ENGINE_DIRTY="" RIG_ROOT="$repo" _rig_engine_json)"
  contains "engine stamp (git fallback): results/ churn alone is NOT dirty (scoped out)" "$out" '"dirty": false'

  # A real change to a TRACKED, non-results file IS dirty.
  printf 'y\n' >> "$repo/engine/f"
  out="$(BENCH_ENGINE_COMMIT="" BENCH_ENGINE_DIRTY="" RIG_ROOT="$repo" _rig_engine_json)"
  contains "engine stamp (git fallback): a real engine/ edit IS dirty:true" "$out" '"dirty": true'
  rm -rf "$repo"
else
  echo "skip - git not installed; the engine-stamp git-fallback pass needs it"
fi

# ── fetch_rig: RIG_MOCK_ORIGIN on each path, and the RIG_ALLOW_SOURCE gate ───────────────────────────
# (1) LOCAL OVERRIDE: RIG_MOCK_CMD wins outright, origin=local-override, no fetch.
root="$(mktemp -d)"
mockcmd="$root/mymock"; printf '#!/bin/sh\n:\n' > "$mockcmd"; chmod +x "$mockcmd"
RIG_MOCK_ORIGIN=""; MOCK=""
RIG_MOCK_CMD="$mockcmd" fetch_rig "$root" >/dev/null 2>&1; rc=$?
check "fetch_rig: RIG_MOCK_CMD override returns 0" "$rc" "0"
check "fetch_rig: override sets origin=local-override" "$RIG_MOCK_ORIGIN" "local-override"
check "fetch_rig: override points MOCK at the supplied command" "$MOCK" "$mockcmd"
rm -rf "$root"

# (2) CACHED: an already-executable bin/mock-<arch> is reused, origin=cached, curl never called.
root="$(mktemp -d)"; arch="${BENCH_ARCH:-arm64}"
mkdir -p "$root/bin"; printf 'bin\n' > "$root/bin/mock-$arch"; chmod +x "$root/bin/mock-$arch"
curl(){ echo "CURL-SHOULD-NOT-RUN" >&2; return 99; }   # if reached, the download path was wrong
RIG_MOCK_ORIGIN=""; MOCK=""; unset RIG_MOCK_CMD
fetch_rig "$root" >/dev/null 2>&1; rc=$?
check "fetch_rig: a cached mock is reused (rc 0)" "$rc" "0"
check "fetch_rig: cached mock sets origin=cached" "$RIG_MOCK_ORIGIN" "cached"
unset -f curl
rm -rf "$root"

# (3) RELEASE DOWNLOAD: curl succeeds -> a fresh binary, origin=release.
root="$(mktemp -d)"
curl(){ local out=""; while [ $# -gt 0 ]; do case "$1" in -o) out="$2"; shift 2;; *) shift;; esac; done; printf 'ELF\n' > "$out"; return 0; }
RIG_MOCK_ORIGIN=""; MOCK=""; unset RIG_MOCK_CMD
fetch_rig "$root" >/dev/null 2>&1; rc=$?
check "fetch_rig: a successful download returns 0" "$rc" "0"
check "fetch_rig: a successful download sets origin=release" "$RIG_MOCK_ORIGIN" "release"
unset -f curl
rm -rf "$root"

# (4) DOWNLOAD FAILS + RIG_ALLOW_SOURCE=0: FAIL CLOSED (rc 1), no silent source substitution.
root="$(mktemp -d)"
curl(){ return 22; }   # download fails, writes nothing
RIG_MOCK_ORIGIN=""; MOCK=""; unset RIG_MOCK_CMD
RIG_ALLOW_SOURCE=0 fetch_rig "$root" >/dev/null 2>&1; rc=$?
check "fetch_rig: a failed download with RIG_ALLOW_SOURCE=0 returns 1 (fails closed)" "$rc" "1"
check "fetch_rig: a failed fetch does NOT claim origin=release/source-build" \
      "$([ "$RIG_MOCK_ORIGIN" = release ] || [ "$RIG_MOCK_ORIGIN" = source-build ] && echo claimed || echo honest)" "honest"
unset -f curl
rm -rf "$root"

if [ "$fail" = 0 ]; then echo "rig_test.sh: PASS"; exit 0; fi
echo "rig_test.sh: FAIL"; exit 1
