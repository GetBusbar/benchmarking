#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Live per-gateway status of an in-flight run — so we are never "blind". Shows each gateway's current
# stage (last meaningful line from its fanout log): launched / installing / rsync / a suite / DONE /
# a probe line. Usage: bash bench-status.sh   (or `watch -n5 bash bench-status.sh`).
#
# The field is DISCOVERED (lib/gateways.sh -> gateways/*/gateway.sh), never listed here. This file
# used to carry a frozen name string, so a newly dropped-in gateway was invisible to the live view and
# a deleted one printed "pending" forever, and the "n/13" footer was a literal that went wrong the
# moment the field changed size.
cd "$(dirname "${BASH_SOURCE[0]}")" || exit 1
# shellcheck source=lib/gateways.sh
. "./lib/gateways.sh"

# SSH fallback for a box whose fanout log has no "[cell N/M]" line yet (an orchestrator process
# that started before that logging existed still won't ever write one locally - it already has its
# functions parsed - so the only way to see this run's real grid position is to ask the box
# directly). Cheap and best-effort: a dead/unreachable box just times out and we fall back to the
# stage line instead.
KEYFILE="${TMPDIR:-/tmp}/gateway-bench-key.pem"
SSHOPT="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o BatchMode=yes -i $KEYFILE"

# Cell cost is wildly non-uniform: a not_configurable cell prints in milliseconds, a served cell
# runs a full RPS bisect + stream + memory suite before its verdict prints. Averaging elapsed/N
# across both would report a confident-looking ETA that is fiction. Instead this tracks, per
# gateway, the wall-clock moment the cell count last CHANGED (a tiny state file across invocations,
# since this script is one-shot) - so a frozen count reads as "stalled Ns on the current cell", not
# as an extrapolated countdown built from a rate that no longer applies.
STATE="${TMPDIR:-/tmp}/bench-status-state"
mkdir -p "$STATE"

printf '%-16s  %-8s  %-26s  %s\n' "GATEWAY" "CELL" "ETA" "STAGE (now)"
printf '%-16s  %-8s  %-26s  %s\n' "----------------" "--------" "--------------------------" "-------------------------------------------"
done=0
total=0
while read -r g; do
  [ -n "$g" ] || continue
  total=$((total+1))
  f="results/fanout-$g.log"
  # last line that is a stage marker, a suite header, a probe, or a DONE/INCOMPLETE verdict
  s=$(grep -hE "\[$g\] (launched|installing|rsync|running|pulling|DONE|INCOMPLETE)|\[cell [0-9]+/[0-9]+\]|══ $g ·|ttft=|max proxy throughput =|sustained RPS @20ms =|building|fetching prebuilt rig" "$f" 2>/dev/null \
      | tail -1 | sed -E "s/.*══ $g · /suite: /; s/.*\] \[$g\] //; s/^\[[0-9:]+\] //" | cut -c1-58)
  echo "$s" | grep -qi "DONE" && done=$((done+1))

  # A cell only prints once its ENTIRE measurement finishes - probe, then (if served) the full
  # RPS/stream/memory sweep - never before. So "no cell line yet" commonly means cell 1's sweep is
  # still running, not that nothing has happened; say that plainly instead of a bare dash. But only
  # once the box has actually reached the grid walk (stage says "running") - before that it is
  # still cloning/installing/qualifying and "mid-sweep" would be a claim we have no evidence for.
  if echo "$s" | grep -qE "running $g "; then
    cell="0/?"; eta="no cell finished yet (likely mid-sweep on the 1st served cell)"
  else
    cell="-/-"; eta="-"
  fi
  if ! echo "$s" | grep -qiE "DONE|INCOMPLETE"; then
    cellline=$(grep -hoE '\[cell [0-9]+/[0-9]+\]' "$f" 2>/dev/null | tail -1)
    ip=$(grep -hoE 'ip=[0-9.]+' "$f" 2>/dev/null | tail -1 | cut -d= -f2)
    if [ -z "$cellline" ] && [ -n "$ip" ]; then
      cellline=$(ssh $SSHOPT ubuntu@"$ip" \
        "grep -o '\[cell [0-9]*/[0-9]*\]' ~/benchmarking/.run.log 2>/dev/null | tail -1" </dev/null 2>/dev/null)
    fi
    if [ -n "$cellline" ]; then
      n=$(echo "$cellline" | grep -oE '[0-9]+' | sed -n 1p)
      m=$(echo "$cellline" | grep -oE '[0-9]+' | sed -n 2p)
      cell="${n}/${m}"
      now_epoch=$(date +%s)
      statefile="$STATE/$g"
      prev_n="-1"; prev_epoch="$now_epoch"
      [ -f "$statefile" ] && read -r prev_n prev_epoch < "$statefile"
      if [ "$n" != "$prev_n" ]; then
        echo "$n $now_epoch" > "$statefile"
        since=0
      else
        since=$((now_epoch - prev_epoch))
      fi
      if [ "${n:-0}" -ge "${m:-1}" ] && [ "${m:-0}" -gt 0 ]; then
        eta="grid done; sweep/stream/mem ${since}s"
      elif [ "$since" -eq 0 ]; then
        eta="advancing"
      else
        eta="on this cell ${since}s (probe or sweep)"
      fi
    fi
  fi
  printf '%-16s  %-8s  %-20s  %s\n' "$g" "$cell" "$eta" "${s:-pending}"
done < <(gw_list .)
echo "----------------  --------  --------------------------  -------------------------------------------"
echo "$done/$total DONE"
