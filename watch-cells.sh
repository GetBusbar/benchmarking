#!/usr/bin/env bash
# Wait for the NEXT measured cell(s) on a running field box, emit their verdict lines, then exit.
#
# Blocks (polling the box's partial snapshot over ssh) until at least one new cell has a headline
# frontier RPS, or the run finishes, or a STOP file appears - then prints and returns. Designed to be
# relaunched in a loop by an operator/agent: each return delivers the newly-completed cells so they
# can be relayed one at a time, until RUN-DONE.
#
#   IP=1.2.3.4 KEY=~/.cache/gateway-bench/gateway-bench-key.pem GW=busbar \
#   BASELINE=results/snapshots/result_busbar-151_....json \
#   STATE=/tmp/busbar.state.json WORK=/tmp/busbarwatch  ./watch-cells.sh
#
# Exit: 0 new cells emitted (RELAY: lines) | 2 run finished (RUN-DONE) | 3 STOP file | 4 unreachable-too-long
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
: "${IP:?need IP}"; : "${KEY:?need KEY}"; : "${GW:?need GW}"; : "${BASELINE:?need BASELINE}"
: "${STATE:?need STATE}"; : "${WORK:?need WORK}"
POLL="${POLL:-45}"; MAXUNREACH="${MAXUNREACH:-40}"
SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=12 -i "$KEY")
mkdir -p "$WORK"; NEW="$WORK/${GW}.partial.json"
unreach=0
while :; do
  [ -f "$WORK/STOP" ] && { echo "STOP file present"; exit 3; }
  # newest partial snapshot path on the box (engine writes/updates it as cells land)
  remote="$("${SSH[@]}" ubuntu@"$IP" "ls -t ~/benchmarking/results/snapshots/result_${GW}_*.json 2>/dev/null | head -1" 2>/dev/null)"
  done_rc="$("${SSH[@]}" ubuntu@"$IP" 'cat ~/benchmarking/.run-done 2>/dev/null' 2>/dev/null)"
  if [ -z "$remote" ] && [ -z "$done_rc" ]; then
    # box up but nothing written yet, or a transient ssh miss
    if ! "${SSH[@]}" ubuntu@"$IP" true 2>/dev/null; then
      unreach=$((unreach+1)); [ "$unreach" -ge "$MAXUNREACH" ] && { echo "UNREACHABLE for $unreach polls"; exit 4; }
    else
      unreach=0
    fi
    sleep "$POLL"; continue
  fi
  unreach=0
  if [ -n "$remote" ]; then
    rsync -az --timeout=60 -e "${SSH[*]}" "ubuntu@$IP:$remote" "$NEW" 2>/dev/null || true
  fi
  out="$(python3 "$HERE/cell-verdict.py" --baseline "$HERE/$BASELINE" --new "$NEW" --state "$STATE" 2>/dev/null)"
  relay="$(printf '%s\n' "$out" | grep '^RELAY:' || true)"
  if [ -n "$relay" ]; then
    printf '%s\n' "$out"
    exit 0
  fi
  if [ -n "$done_rc" ]; then
    echo "RUN-DONE=$done_rc"
    printf '%s\n' "$out"    # COUNT: line for final tally
    exit 2
  fi
  sleep "$POLL"
done
