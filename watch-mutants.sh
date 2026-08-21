#!/usr/bin/env bash
# Watch the already-running mutants box, pull its report, and TERMINATE it.
#
# Separate from run-mutants-ec2.sh because that script's box was rescued rather than restarted: it
# had already paid for provisioning (rust + a from-source cargo-mutants build) when the baseline
# failed on the descriptor limit, so the fix was applied in place. Killing its orchestrator with
# SIGKILL kept the box alive by skipping the EXIT trap - which also means nothing is left holding the
# terminate. That is what this is for. It runs the terminate on every exit path, including its own.
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
  # `pgrep -c` prints 0 AND exits non-zero when nothing matches, so `|| echo 0` appended a SECOND zero
  # and the string became "0\n0" - which never equals "0", so a FINISHED box would be polled forever
  # (watch-orphans.sh carries this same fix). head -1 takes pgrep's own answer.
  alive="$(ssh $SSHOPT "ubuntu@$IP" 'pgrep -c cargo-mutants 2>/dev/null | head -1' 2>/dev/null | tr -d '\r\n ' || echo "?")"
  [[ -z "$alive" ]] && alive=0
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
