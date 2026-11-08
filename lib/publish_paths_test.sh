#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Regression guard for run-on-ec2.sh's publish_paths: a gateway's publish stages ONLY the snapshot files
# THIS run pulled for it. It used to stage every results/snapshots/result_<gw>_*.json on disk, so a run
# held back with BENCH_NO_PUBLISH=1 (or a harvested, unreviewed one) was committed and pushed by that
# gateway's next normal publish.
#
# Sources ONLY publish_paths out of run-on-ec2.sh, against a temp results tree - no git, no AWS.
#
# Run: bash lib/publish_paths_test.sh
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/run-on-ec2.sh"
fail=0

tmp="$(mktemp)"
awk '/^publish_paths\(\) \{/{p=1} p{print} p&&/^\}/{exit}' "$SRC" > "$tmp"
if ! grep -q 'publish_paths()' "$tmp"; then
  echo "FAIL - could not extract publish_paths from run-on-ec2.sh"; rm -f "$tmp"; exit 1
fi
# shellcheck source=/dev/null
source "$tmp"
rm -f "$tmp"

HERE="$(mktemp -d)"
mkdir -p "$HERE/results/snapshots" "$HERE/results/history" "$HERE/results/config"
held="$HERE/results/snapshots/result_gw_2026-01-01T00-00-00Z.json"     # BENCH_NO_PUBLISH=1, earlier
fresh="$HERE/results/snapshots/result_gw_2026-02-01T00-00-00Z.json"    # pulled by THIS run
other="$HERE/results/snapshots/result_gw2_2026-02-01T00-00-00Z.json"   # another gateway's
for f in "$held" "$fresh" "$other" "$HERE/results/history/gw.jsonl" "$HERE/results/config/gw.txt"; do echo '{}' > "$f"; done

check(){ if [ "$2" = "$3" ]; then echo "ok   - $1"; else echo "FAIL - $1: got [$2] want [$3]"; fail=1; fi; }
has(){ printf '%s\n' "$1" | grep -qxF "$2" && echo yes || echo no; }

out="$(publish_paths gw "$fresh"$'\n')"
check "the snapshot this run pulled is staged" "$(has "$out" "$fresh")" yes
check "a held-back snapshot of the same gateway is NOT staged" "$(has "$out" "$held")" no
check "another gateway's snapshot is NOT staged" "$(has "$out" "$other")" no
check "the gateway's history line is still staged" "$(has "$out" "$HERE/results/history/gw.jsonl")" yes
check "the gateway's config sidecar is still staged" "$(has "$out" "$HERE/results/config/gw.txt")" yes

# A list naming another gateway's file, or a file that is gone, stages nothing extra.
out="$(publish_paths gw "$other"$'\n'"$HERE/results/snapshots/result_gw_gone.json"$'\n')"
check "a pulled list cannot smuggle in another gateway's snapshot" "$(has "$out" "$other")" no
check "an empty pulled list stages no snapshot at all" \
  "$(publish_paths gw "" | grep -c '/results/snapshots/' | tr -d ' ')" 0

rm -rf "$HERE"
if [ "$fail" -eq 0 ]; then echo "all publish-paths tests passed"; exit 0; fi
echo "PUBLISH-PATHS TESTS FAILED"; exit 1
