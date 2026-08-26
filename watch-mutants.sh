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
KEYFILE="${BENCH_STATE_DIR:-$HOME/.cache/gateway-bench}/gateway-bench-key.pem"
SSHOPT="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=15 -i $KEYFILE"
OUT="$HERE/results/mutants"

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }
finish() {
  log "terminating $IID"
  aws ec2 terminate-instances --instance-ids "$IID" >/dev/null 2>&1 || true
}
trap finish EXIT INT TERM

mkdir -p "$OUT"
while :; do
  sleep 300
  alive="$(ssh $SSHOPT "ubuntu@$IP" 'pgrep -c cargo-mutants || echo 0' 2>/dev/null || echo "?")"
  line="$(ssh $SSHOPT "ubuntu@$IP" 'tail -1 ~/mutants.log 2>/dev/null' 2>/dev/null || true)"
  log "running=$alive | $line"
  # Pull the partial report as we go, so a box that dies late still leaves evidence behind.
  rsync -az -e "ssh $SSHOPT" "ubuntu@$IP:bench/engine/mutants.out/" "$OUT/" 2>/dev/null || true
  [[ "$alive" == "0" ]] && break
done

log "final pull"
rsync -az -e "ssh $SSHOPT" "ubuntu@$IP:bench/engine/mutants.out/" "$OUT/" 2>/dev/null || true
rsync -az -e "ssh $SSHOPT" "ubuntu@$IP:mutants.log" "$OUT/mutants.log" 2>/dev/null || true
log "report in $OUT"
