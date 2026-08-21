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
SHARD_DIR="$HERE/results/shards/$GW"
rm -rf "$SHARD_DIR"; mkdir -p "$SHARD_DIR"

# One single-egress box per column, concurrently. Each is an ordinary run-on-ec2.sh invocation for THIS
# gateway with OTB_EGRESS set, PUBLISH=0 (shards never publish individually), and its OWN RUN_ID so a
# per-invocation teardown never terminates a sibling shard's box. The shared key/SG lifetime is robust
# to concurrent runs (teardown keeps the key while sibling boxes live), which is exactly this case.
pids=()
for E in "${EGRESSES[@]}"; do
  log="$HERE/results/fanout-$GW-shard-$E.log"
  OTB_EGRESS="$E" PUBLISH=0 RUN_ID="shard-$GW-$E-$START_EPOCH" \
    "$HERE/run-on-ec2.sh" "$GW" >"$log" 2>&1 &
  pids+=($!)
  sleep 3   # stagger the AWS API calls
done
fail=0
for p in "${pids[@]}"; do wait "$p" || fail=$((fail+1)); done
echo "[shard] all $GW shard boxes joined ($fail box(es) reported an issue - see results/fanout-$GW-shard-*.log)"

# Collect the shard snapshots THIS run produced (result_<gw>_*.json newer than START), and MOVE them
# out of the canonical dir into scratch: a single-column partial must never be left where the board or
# a later merge could read it as a finished row. Only the merged result goes back to canonical.
mapfile -t FRESH < <(find "$HERE/results/snapshots" -name "result_${GW}_*.json" -newermt "@$START_EPOCH" 2>/dev/null | sort)
if [ "${#FRESH[@]}" -eq 0 ]; then
  echo "[shard] no shard snapshots produced - nothing to merge. The board is unchanged; see the logs above." >&2
  exit 1
fi
if [ "${#FRESH[@]}" -ne "${#EGRESSES[@]}" ]; then
  echo "[shard] WARNING: expected ${#EGRESSES[@]} shard snapshots, got ${#FRESH[@]}. Merging what arrived; a missing" >&2
  echo "        egress column will simply be absent from the merged row (not silently zero)." >&2
fi
mv "${FRESH[@]}" "$SHARD_DIR/"

# Merge -> one board snapshot in the canonical dir. `otb merge` refuses on any invariant mismatch or
# overlapping egress column (the safety net if a box ran the full grid on an OTB_EGRESS-blind engine).
echo "[shard] merging ${#FRESH[@]} shard snapshot(s) via local otb"
if ! "$OTB" merge "$SHARD_DIR" "$HERE/results/snapshots"; then
  echo "[shard] merge REFUSED - not publishing. Shard snapshots kept at $SHARD_DIR for inspection." >&2
  exit 1
fi

# The merge wrote results/snapshots/<gw>.json (the board's current file) + a timestamped historical
# copy. Publish exactly those, gated - same shape as a single-box gateway publish.
if [[ "${PUBLISH:-1}" == "1" ]]; then
  MERGED_HIST=$(find "$HERE/results/snapshots" -name "result_${GW}_*.json" -newermt "@$START_EPOCH" | sort | tail -1)
  echo "[shard] publishing merged $GW snapshot"
  git -C "$HERE" add "results/snapshots/$GW.json" "${MERGED_HIST#$HERE/}" "results/history/$GW.jsonl" 2>/dev/null || true
  if git -C "$HERE" diff --cached --quiet; then
    echo "[shard] nothing changed vs HEAD - not committing"
  else
    git -C "$HERE" commit -q -m "results: $GW sharded run (${#FRESH[@]} egress columns merged on one pin)"
    git -C "$HERE" push "${PUBLISH_REMOTE:-origin}" HEAD 2>&1 | tail -2 \
      || echo "[shard] push failed - the merged snapshot is committed locally; push by hand"
  fi
else
  echo "[shard] PUBLISH=0 - merged snapshot is in results/snapshots/$GW.json, uncommitted, for review."
fi
exit "$fail"
