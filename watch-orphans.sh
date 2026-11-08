#!/usr/bin/env bash
# Pull snapshots off boxes whose orchestrator is gone, then terminate each box once its result is in.
#
# An orchestrator can die with boxes still measuring (a dropped network session, or a `pkill` pattern
# that matches more running orchestrators than intended) - a box with nothing holding its result is a
# box whose hours are already spent and whose data is one shutdown away from gone.
#
# Pulls the live partial every poll, so a box lost mid-run costs only the cells since the last poll,
# not everything.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE="${BENCH_STATE_DIR:-$HOME/.cache/gateway-bench}"
KEYFILE="$STATE/gateway-bench-key.pem"
GWS=("$@")
[[ ${#GWS[@]} -gt 0 ]] || { echo "usage: watch-orphans.sh <gateway> [gateway...]"; exit 1; }

log() { printf '[%s] orphans: %s\n' "$(date -u +%H:%M:%S)" "$*"; }
mkdir -p "$STATE" || exit 1
mkdir -p "$HERE/results/partial"

# A space-delimited list, not `declare -A`: this runs on the operator's laptop, and macOS /bin/bash 3.2
# has no associative arrays (an indexed array would arithmetic-evaluate "litellm-python" and abort).
done_list=" "
is_done() { case "$done_list" in *" $1 "*) return 0 ;; esac; return 1; }
mark_done() { is_done "$1" || done_list="$done_list$1 "; }
while :; do
  left=0
  for gw in "${GWS[@]}"; do
    is_done "$gw" && continue
    # IDENTIFY THE BOX BY INSTANCE ID, and tell an AWS failure apart from "no box". A failed describe
    # (expired credentials, throttling) used to read as "box gone" and drop the gateway for good; two
    # boxes with the same Name tag used to become one tab-joined "ip" and a terminate of every box
    # carrying that name, including a fresh re-run's.
    if ! rows="$(aws ec2 describe-instances --filters "Name=tag:Name,Values=gateway-bench-$gw" \
          "Name=instance-state-name,Values=running" \
          --query 'Reservations[].Instances[].[InstanceId,PublicIpAddress]' --output text)"; then
      log "$gw: describe-instances FAILED - not treating that as 'box gone'; retrying next poll"
      left=$((left + 1)); continue
    fi
    rows="$(printf '%s\n' "$rows" | grep -E '^i-' || true)"
    if [[ -z "$rows" ]]; then
      log "$gw: box gone"; mark_done "$gw"; continue
    fi
    if [[ "$(printf '%s\n' "$rows" | grep -c .)" -gt 1 ]]; then
      log "$gw: AMBIGUOUS - $(printf '%s\n' "$rows" | grep -c .) running boxes are named gateway-bench-$gw ($(printf '%s' "$rows" | awk '{print $1}' | tr '\n' ' ')); not pulling or terminating any of them - resolve by hand"
      mark_done "$gw"; continue
    fi
    read -r iid ip <<<"$rows"
    if [[ -z "$ip" || "$ip" == "None" ]]; then
      log "$gw: $iid has no public IP yet"; left=$((left + 1)); continue
    fi
    left=$((left + 1))
    KNOWN_HOSTS="$STATE/$iid.known_hosts"
    if ! python3 "$HERE/lib/ssh_trust.py" "$iid" "$ip" "$KNOWN_HOSTS"; then
      log "$gw: host key authentication failed - not pulling or terminating this box"
      continue
    fi
    SSHOPT=(-o BatchMode=yes -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$KNOWN_HOSTS" -o ConnectTimeout=25 -i "$KEYFILE")
    printf -v RSYNC_SSH '%q ' ssh "${SSHOPT[@]}"
    # `pgrep -c` prints 0 AND exits non-zero when nothing matches, so `|| echo 0` appended a second
    # zero ("00", equal to neither "0" nor a live count). `|| true` keeps pgrep's own answer. The ssh
    # status is checked on its own: a pipeline ending in `tr` always succeeded, so a dropped
    # connection used to read as "otb finished" on a box still mid-grid. The run's .run-done sentinel
    # is required too, because the engine checkpoints result_<gw>_*.json while the grid is running.
    if st="$(ssh "${SSHOPT[@]}" "ubuntu@$ip" 'pgrep -c otb 2>/dev/null || true; printf "rundone=%s\n" "$(cat ~/benchmarking/.run-done 2>/dev/null)"' 2>/dev/null)"; then
      alive="$(printf '%s\n' "$st" | head -1 | tr -d '\r\n ')"
      rundone="$(printf '%s\n' "$st" | sed -n 's/^rundone=//p' | tr -d '\r\n ')"
      [[ -n "$alive" ]] || alive="?"
    else
      alive="?"; rundone=""
    fi
    # Keep the newest partial regardless of state.
    rsync -az --timeout=90 -e "$RSYNC_SSH" \
      "ubuntu@$ip:benchmarking/results/snapshots/$gw.json" "$HERE/results/partial/$gw.json" 2>/dev/null
    if [[ "$alive" == "0" && -n "$rundone" ]]; then
      if rsync -az --timeout=180 -e "$RSYNC_SSH" \
           "ubuntu@$ip:benchmarking/results/snapshots/result_${gw}_*.json" "$HERE/results/snapshots/" 2>/dev/null; then
        log "$gw: FINAL pulled (run-done=$rundone) - terminating its box $iid"
        if aws ec2 terminate-instances --instance-ids "$iid" >/dev/null; then
          mark_done "$gw"
        else
          log "$gw: TERMINATE FAILED for $iid - it is still billing; retrying next poll"
        fi
      else
        log "$gw: run over (run-done=$rundone), final snapshot not pullable yet"
      fi
    elif [[ "$alive" == "0" ]]; then
      log "$gw: otb gone but no .run-done yet - not treating the checkpoint as final"
    elif [[ "$alive" == "?" ]]; then
      log "$gw: box $iid not reachable over ssh this poll"
    else
      log "$gw: measuring (otb=$alive)"
    fi
  done
  [[ $left -eq 0 ]] && break
  sleep 180
done
log "all orphans resolved"
