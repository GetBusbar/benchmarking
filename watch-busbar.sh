#!/usr/bin/env bash
# Pull busbar's snapshot off its ORPHANED box, then terminate it.
#
# The 2026-08-01 field run lost seven gateways at once to a single network event on the operator's
# machine - every detached-ssh session dropped inside sixty seconds, which the orchestrator read as
# seven box failures - and the orchestrator then exited. busbar and agentgateway were still measuring,
# with nothing left holding their results. agentgateway had already finished and was pulled by hand;
# this is the same job for busbar, which was still mid-grid at the time.
#
# Deliberately dumb: poll, pull when `otb` is gone, terminate. It owns nothing else.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IID="${1:?usage: watch-busbar.sh <instance-id> <ip>}"
IP="${2:?usage: watch-busbar.sh <instance-id> <ip>}"
KEYFILE="${BENCH_STATE_DIR:-$HOME/.cache/gateway-bench}/gateway-bench-key.pem"
SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=25 -i "$KEYFILE")

log() { printf '[%s] busbar-watch: %s\n' "$(date -u +%H:%M:%S)" "$*"; }

while :; do
  alive="$("${SSH[@]}" "ubuntu@$IP" 'pgrep -c otb || echo 0' 2>/dev/null || echo "?")"
  cells="$("${SSH[@]}" "ubuntu@$IP" 'python3 -c "
import json
try:
    d=json.load(open(\"/home/ubuntu/benchmarking/results/snapshots/busbar.json\"))
    m=d.get(\"matrix\",{}).get(\"upstreams\",{})
    print(sum(len(u.get(\"cells\") or {}) for u in m.values()))
except Exception: print(0)
"' 2>/dev/null || echo "?")"
  log "otb=$alive cells=$cells/36"
  # `?` is an unreachable box, not a finished one - keep waiting rather than terminating a live run.
  if [[ "$alive" == "0" ]]; then break; fi
  sleep 300
done

log "run finished - pulling"
for _ in 1 2 3 4; do
  if rsync -az --timeout=180 -e "${SSH[*]}" \
      "ubuntu@$IP:benchmarking/results/snapshots/result_busbar_*.json" "$HERE/results/snapshots/"; then
    log "pulled"; break
  fi
  log "pull failed - retrying in 20s"; sleep 20
done

log "terminating $IID"
aws ec2 terminate-instances --instance-ids "$IID" >/dev/null 2>&1 || true
ls -la "$HERE/results/snapshots/" | grep busbar | tail -2
