#!/usr/bin/env bash
# Pull snapshots off boxes whose orchestrator is gone, then terminate each box once its result is in.
#
# Twice now an orchestrator has died with boxes still measuring: once to a network event that dropped
# every ssh session at once, and once to my own `pkill -f run-on-ec2`, which matched BOTH running
# orchestrators when I meant to stop one. A box with nothing holding its result is a box whose hours
# are already spent and whose data is one shutdown away from gone.
#
# Pulls the live partial every poll, so a box lost mid-run costs the cells since the last poll rather
# than everything - the lesson from busbar's 8-hour loss.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KEYFILE="${BENCH_STATE_DIR:-$HOME/.cache/gateway-bench}/gateway-bench-key.pem"
SSH="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=25 -i $KEYFILE"
GWS=("$@")
[[ ${#GWS[@]} -gt 0 ]] || { echo "usage: watch-orphans.sh <gateway> [gateway...]"; exit 1; }

log() { printf '[%s] orphans: %s\n' "$(date -u +%H:%M:%S)" "$*"; }
mkdir -p "$HERE/results/partial"

declare -A done_gw=()
while :; do
  left=0
  for gw in "${GWS[@]}"; do
    [[ -n "${done_gw[$gw]:-}" ]] && continue
    ip="$(aws ec2 describe-instances --filters "Name=tag:Name,Values=gateway-bench-$gw" \
          "Name=instance-state-name,Values=running" \
          --query 'Reservations[].Instances[].PublicIpAddress' --output text 2>/dev/null)"
    if [[ -z "$ip" || "$ip" == "None" ]]; then
      log "$gw: box gone"; done_gw[$gw]=1; continue
    fi
    left=$((left + 1))
    # `pgrep -c` prints 0 AND exits non-zero when nothing matches, so `|| echo 0` appended a second
    # zero and the string became "00" - which equals neither "0" nor a live count, so a FINISHED box
    # would have been polled forever. head -1 takes pgrep's own answer and lets the fallback cover
    # only the case where pgrep printed nothing at all.
    alive="$($SSH "ubuntu@$ip" 'pgrep -c otb 2>/dev/null | head -1' 2>/dev/null | tr -d '\r\n ' || echo "?")"
    [[ -z "$alive" ]] && alive=0
    # Keep the newest partial regardless of state.
    rsync -az --timeout=90 -e "$SSH" \
      "ubuntu@$ip:benchmarking/results/snapshots/$gw.json" "$HERE/results/partial/$gw.json" 2>/dev/null
    if [[ "$alive" == "0" ]]; then
      if rsync -az --timeout=180 -e "$SSH" \
           "ubuntu@$ip:benchmarking/results/snapshots/result_${gw}_*.json" "$HERE/results/snapshots/" 2>/dev/null; then
        log "$gw: FINAL pulled - terminating its box"
        aws ec2 describe-instances --filters "Name=tag:Name,Values=gateway-bench-$gw" \
          "Name=instance-state-name,Values=running" --query 'Reservations[].Instances[].InstanceId' \
          --output text 2>/dev/null | tr '\t' '\n' | grep -E '^i-' \
          | xargs -r aws ec2 terminate-instances --instance-ids >/dev/null 2>&1
        done_gw[$gw]=1
      else
        log "$gw: otb gone, final snapshot not there yet"
      fi
    else
      log "$gw: measuring (otb=$alive)"
    fi
  done
  [[ $left -eq 0 ]] && break
  sleep 180
done
log "all orphans resolved"
