#!/usr/bin/env bash
# Live per-gateway status of an in-flight run — so we are never "blind". Shows each gateway's current
# stage (last meaningful line from its fanout log): launched / installing / rsync / a suite / DONE /
# a probe line. Usage: bash bench-status.sh   (or `watch -n5 bash bench-status.sh`).
cd "$(dirname "${BASH_SOURCE[0]}")"
GWS="agentgateway apisix arch bifrost busbar gomodel helicone kong litellm-python litellm-rust one-api portkey tensorzero"
printf '%-16s  %s\n' "GATEWAY" "STAGE (now)"
printf '%-16s  %s\n' "----------------" "-------------------------------------------"
done=0
for g in $GWS; do
  f="results/fanout-$g.log"
  # last line that is a stage marker, a suite header, a probe, or a DONE/INCOMPLETE verdict
  s=$(grep -hE "\[$g\] (launched|installing|rsync|running|pulling|DONE|INCOMPLETE)|══ $g ·|ttft=|max proxy throughput =|sustained RPS @20ms =|building|fetching prebuilt rig" "$f" 2>/dev/null \
      | tail -1 | sed -E "s/.*══ $g · /suite: /; s/.*\] \[$g\] //; s/^\[[0-9:]+\] //" | cut -c1-58)
  echo "$s" | grep -qi "DONE" && done=$((done+1))
  printf '%-16s  %s\n' "$g" "${s:-pending}"
done
echo "----------------  -------------------------------------------"
echo "$done/13 DONE"
