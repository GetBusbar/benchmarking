#!/usr/bin/env bash
# Watch the already-running mutants box, pull its report, and TERMINATE it.
#
# Separate from run-mutants-ec2.sh because a box whose orchestrator was killed with SIGKILL survives
# (SIGKILL skips the EXIT trap that would otherwise terminate it) - this reattaches to that live box
# and runs the terminate on every exit path, including its own, so nothing is left holding it.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IID="${1:?usage: watch-mutants.sh <instance-id> <ip>}"
IP="${2:?usage: watch-mutants.sh <instance-id> <ip>}"
STATE="${BENCH_STATE_DIR:-$HOME/.cache/gateway-bench}"
KEYFILE="$STATE/gateway-bench-key.pem"
KNOWN_HOSTS="$STATE/$IID.known_hosts"
SSHOPT=(-o BatchMode=yes -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$KNOWN_HOSTS" -o ConnectTimeout=15 -i "$KEYFILE")
printf -v RSYNC_SSH '%q ' ssh "${SSHOPT[@]}"
OUT="$HERE/results/mutants"

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
finish() {
  log "terminating $IID"
  aws ec2 terminate-instances --instance-ids "$IID" >/dev/null \
    || log "TERMINATE FAILED for $IID - it is still billing; terminate it by hand"
}
# EXIT does the terminate; a signal only has to exit. A handler that terminated and returned would
# drop straight back into the poll loop against a dead box, and the script could never be stopped.
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$OUT"
mkdir -p "$STATE" || exit 1
python3 "$HERE/lib/ssh_trust.py" "$IID" "$IP" "$KNOWN_HOSTS" || exit 1
# `pgrep -c` prints 0 AND exits 1 on no match, so `|| echo 0` made "0\n0" and a finished run was polled
# forever. `|| true` keeps pgrep's own answer. ssh failure reads as "?"; 6 in a row (30 min) ends it.
unreachable=0
while :; do
  sleep 300
  alive="$(ssh "${SSHOPT[@]}" "ubuntu@$IP" 'pgrep -c cargo-mutants || true' 2>/dev/null | head -1 | tr -d '\r\n ')"
  [[ -n "$alive" ]] || alive="?"
  line="$(ssh "${SSHOPT[@]}" "ubuntu@$IP" 'tail -1 ~/mutants.log 2>/dev/null' 2>/dev/null || true)"
  log "running=$alive | $line"
  # Pull the partial report as we go, so a box that dies late still leaves evidence behind.
  rsync -az -e "$RSYNC_SSH" "ubuntu@$IP:bench/engine/mutants.out/" "$OUT/" 2>/dev/null || true
  [[ "$alive" == "0" ]] && break
  if [[ "$alive" == "?" ]]; then
    unreachable=$((unreachable + 1))
    [[ $unreachable -ge 6 ]] && break
  else
    unreachable=0
  fi
done

log "final pull"
pulled=0
for _ in 1 2 3; do
  if rsync -az -e "$RSYNC_SSH" "ubuntu@$IP:bench/engine/mutants.out/" "$OUT/"; then pulled=1; break; fi
  sleep 10
done
rsync -az -e "$RSYNC_SSH" "ubuntu@$IP:mutants.log" "$OUT/mutants.log" || log "WARNING mutants.log not pulled"
if [[ $pulled != 1 ]]; then
  log "REPORT NOT RETRIEVED: the final pull of mutants.out failed; $OUT holds only the last partial pull"
  exit 1
fi
log "report in $OUT"
