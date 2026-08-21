#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# THE SECOND WAY TO RUN A GATEWAY (the first is run-on-ec2.sh, one box, unchanged).
#
# SHARDED: measure ONE gateway's 36-cell grid across N boxes IN PARALLEL - one box per egress
# upstream, each measuring that egress column (its full ingress row) while the gateway keeps its FULL
# config so routing is identical to a single-box run. Then merge the N partial snapshots into one
# board snapshot. ~1/N wall-clock at ~the same box-hours; see docs/DESIGN-sharded-field-run.md.
#
#   run-sharded.sh <gateway>             # shard by egress, merge, publish the merged result
#   PUBLISH=0 run-sharded.sh <gateway>   # measure + merge, leave the merged snapshot uncommitted
#
# COMPARABILITY: every shard box builds the SAME gateway from the SAME pin, renders the SAME full
# config, measures on the SAME frozen ENGINE_PIN, and passes box_qualify INDEPENDENTLY. The merge
# records each box's qualification per egress column (rig-per-Upstream) as the evidence, and REFUSES
# to merge shards that disagree on gateway/build/arch/config/engine or that overlap an egress column.
#
# REQUIRES a rig built from an engine that understands OTB_EGRESS (this tree's engine/), pinned in
# ENGINE_PIN. On an older rig each box ignores OTB_EGRESS and walks the FULL grid, and the merge then
# refuses (overlapping egress columns) rather than publishing a wrong number - loud, not silent.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

GW="${1:?usage: run-sharded.sh <gateway>}"
DEF="$HERE/gateways/$GW/definition.json"
[ -f "$DEF" ] || { echo "no gateways/$GW/definition.json" >&2; exit 1; }

# Shard axis = the gateway's declared egress upstreams (one box per column).
mapfile -t EGRESSES < <(python3 -c "import json;print('\n'.join(json.load(open('$DEF'))['egress']))")
[ "${#EGRESSES[@]}" -ge 1 ] || { echo "$GW declares no egress upstreams to shard" >&2; exit 1; }
echo "[shard] $GW -> ${#EGRESSES[@]} egress column(s): ${EGRESSES[*]}"

# A LOCAL otb for the merge. The rig otb the boxes download is Linux/arm64 and will not run on this
# (orchestrator) machine, so build the tree's engine natively once. The merge is a pure function of
# the shard JSONs - no gateway, mock, or network - so a debug build is fine and fast.
# Workspace target lands at the repo root, not under engine/.
OTB="$HERE/target/release/otb"
if [ ! -x "$OTB" ]; then
  echo "[shard] building a local otb for the merge (cargo build --release --bin otb)"
  ( cd "$HERE/engine" && cargo build --release --bin otb ) || { echo "local otb build failed" >&2; exit 1; }
fi

START_EPOCH=$(date +%s)
# Namespace the whole invocation: a per-run RUN_ID (also the publish-lock token owner) and a PRIVATE
# per-run shard-staging dir. Two run-sharded.sh runs for the SAME gateway can now overlap without
# colliding on a shared results/shards/<gw> dir or scooping each other's snapshots.
RUN_ID="sharded-$GW-$START_EPOCH-$$"
SHARD_DIR="$HERE/results/shards/$GW-$START_EPOCH-$$"
rm -rf "$SHARD_DIR"; mkdir -p "$SHARD_DIR"

# The publish critical section is SHARED with run-on-ec2.sh: a field run may still be pushing this same
# checkout while we merge-publish. Source run-on-ec2.sh's OWN publish-lock functions (extracted verbatim,
# the same technique lib/publish_lock_test.sh uses) and key the lock on the checkout path EXACTLY as
# run-on-ec2.sh does, so our merge-publish serializes against every per-gateway publish on this tree.
PUBLISH_LOCK="${TMPDIR:-/tmp}/gateway-bench-publish-$(printf '%s' "$HERE" | tr -c 'A-Za-z0-9' '_').lock"
_lockfns="$(mktemp)"
awk '/^publish_lock_acquire\(\) \{/{p=1} p{print} /^publish_lock_release\(\) \{/{r=1} r&&/^\}/{print "";exit}' \
  "$HERE/run-on-ec2.sh" > "$_lockfns"
if ! grep -q 'publish_lock_acquire()' "$_lockfns" || ! grep -q 'publish_lock_release()' "$_lockfns"; then
  echo "[shard] could not extract the publish-lock functions from run-on-ec2.sh - refusing to publish unserialized" >&2
  rm -f "$_lockfns"; exit 1
fi
# shellcheck source=/dev/null
source "$_lockfns"; rm -f "$_lockfns"

# One single-egress box per column, concurrently. Each is an ordinary run-on-ec2.sh invocation for THIS
# gateway with OTB_EGRESS set, PUBLISH=0 (shards never publish OR append their single-column partials),
# its OWN RUN_ID so a per-invocation teardown never terminates a sibling shard's box, and OTB_SHARD_STAGE
# pointing at our private dir so each box hands its pulled snapshot straight there (RUN_ID-scoped
# collection: never left in the shared canonical dir, never collected by mtime). The shared key/SG
# lifetime is robust to concurrent runs (teardown keeps the key while sibling boxes live).
pids=()
for E in "${EGRESSES[@]}"; do
  log="$HERE/results/fanout-$GW-shard-$E.log"
  OTB_EGRESS="$E" PUBLISH=0 RUN_ID="shard-$GW-$E-$START_EPOCH" OTB_SHARD_STAGE="$SHARD_DIR" \
    "$HERE/run-on-ec2.sh" "$GW" >"$log" 2>&1 &
  pids+=($!)
  sleep 3   # stagger the AWS API calls
done
fail=0
for p in "${pids[@]}"; do wait "$p" || fail=$((fail+1)); done
echo "[shard] all $GW shard boxes joined ($fail box(es) reported an issue - see results/fanout-$GW-shard-*.log)"

# Collect the shard snapshots THIS run produced. Each box already staged its own pulled snapshot into
# SHARD_DIR (out of the canonical dir), so this is a plain read of OUR private dir - no mtime window, so
# a concurrent same-gateway run's snapshot can never be swept in.
mapfile -t FRESH < <(find "$SHARD_DIR" -name "result_${GW}_*.json" 2>/dev/null | sort)
if [ "${#FRESH[@]}" -eq 0 ]; then
  echo "[shard] no shard snapshots produced - nothing to merge. The board is unchanged; see the logs above." >&2
  exit 1
fi
if [ "${#FRESH[@]}" -ne "${#EGRESSES[@]}" ]; then
  echo "[shard] WARNING: expected ${#EGRESSES[@]} shard snapshots, got ${#FRESH[@]}. Merging what arrived; a missing" >&2
  echo "        egress column will simply be absent from the merged row (not silently zero)." >&2
fi

# Merge -> one board snapshot in the canonical dir. `otb merge` refuses on any invariant mismatch or
# overlapping egress column (the safety net if a box ran the full grid on an OTB_EGRESS-blind engine).
# Capture which timestamped historical file the merge ADDS to the canonical dir (deterministic - the one
# new result_<gw>_*.json), rather than guessing by mtime.
echo "[shard] merging ${#FRESH[@]} shard snapshot(s) via local otb"
_before_merge=$(ls -1 "$HERE"/results/snapshots/result_"$GW"_*.json 2>/dev/null | sort)
if ! "$OTB" merge "$SHARD_DIR" "$HERE/results/snapshots"; then
  echo "[shard] merge REFUSED - not publishing. Shard snapshots kept at $SHARD_DIR for inspection." >&2
  exit 1
fi
_after_merge=$(ls -1 "$HERE"/results/snapshots/result_"$GW"_*.json 2>/dev/null | sort)
MERGED_HIST=$(comm -13 <(printf '%s\n' "$_before_merge") <(printf '%s\n' "$_after_merge") | head -1)

# The merge wrote results/snapshots/<gw>.json (the board's current file) + the timestamped historical
# copy above. Publish exactly those, gated - same shape as a single-box gateway publish, serialized
# under the shared publish lock.
if [[ "${PUBLISH:-1}" == "1" ]]; then
  # Append the MERGED snapshot's row to results/history/<gw>.jsonl ONCE. The shard sub-invocations ran
  # PUBLISH=0 and deliberately did NOT append their partials, so the history gets exactly one complete
  # row for this run - not N-1 single-egress rows plus the real one.
  if ! python3 "$HERE/history/append.py"; then
    echo "[shard] WARNING history/append.py failed over the merged snapshot - history not updated for this run" >&2
    fail=$((fail+1))
  fi
  echo "[shard] publishing merged $GW snapshot"
  (
    trap 'publish_lock_release' EXIT
    publish_lock_acquire "[shard $GW]" echo || exit 1
    # Do NOT swallow the add: a staging failure (index.lock, disk full, an empty MERGED_HIST) must abort
    # loudly, not read as the benign "nothing changed" no-op below.
    if ! git -C "$HERE" add "results/snapshots/$GW.json" ${MERGED_HIST:+"${MERGED_HIST#"$HERE"/}"} "results/history/$GW.jsonl"; then
      echo "[shard] git add FAILED - the merged snapshot was NOT staged; refusing to report success. Investigate index.lock/disk." >&2
      exit 1
    fi
    if git -C "$HERE" diff --cached --quiet; then
      echo "[shard] nothing changed vs HEAD - not committing"
    else
      git -C "$HERE" commit -q -m "results: $GW sharded run (${#FRESH[@]} egress columns merged on one pin)"
      # PIPESTATUS[0], not the pipeline status: `git push | tail` would otherwise report tail's 0 and
      # treat a REJECTED push as success, stranding the commit local-only while the board keeps the old row.
      git -C "$HERE" push "${PUBLISH_REMOTE:-origin}" HEAD 2>&1 | tail -2
      if [[ "${PIPESTATUS[0]}" -ne 0 ]]; then
        echo "[shard] push FAILED - the merged snapshot is committed locally; push it by hand" >&2
        exit 1
      fi
    fi
  ) || fail=$((fail+1))
else
  echo "[shard] PUBLISH=0 - merged snapshot is in results/snapshots/$GW.json, uncommitted, for review."
fi
exit "$fail"
