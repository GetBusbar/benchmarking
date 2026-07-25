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
printf '%-16s  %s\n' "GATEWAY" "STAGE (now)"
printf '%-16s  %s\n' "----------------" "-------------------------------------------"
done=0
total=0
while read -r g; do
  [ -n "$g" ] || continue
  total=$((total+1))
  f="results/fanout-$g.log"
  # last line that is a stage marker, a suite header, a probe, or a DONE/INCOMPLETE verdict
  s=$(grep -hE "\[$g\] (launched|installing|rsync|running|pulling|DONE|INCOMPLETE)|══ $g ·|ttft=|max proxy throughput =|sustained RPS @20ms =|building|fetching prebuilt rig" "$f" 2>/dev/null \
      | tail -1 | sed -E "s/.*══ $g · /suite: /; s/.*\] \[$g\] //; s/^\[[0-9:]+\] //" | cut -c1-58)
  echo "$s" | grep -qi "DONE" && done=$((done+1))
  printf '%-16s  %s\n' "$g" "${s:-pending}"
done < <(gw_list .)
echo "----------------  -------------------------------------------"
echo "$done/$total DONE"
