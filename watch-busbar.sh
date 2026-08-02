#!/usr/bin/env bash
# Pull busbar's partial snapshot off its box EVERY POLL, so a box that dies never costs the whole run.
#
# WHY THIS EXISTS, TWICE OVER.
#
# First: the 2026-08-01 field run lost seven gateways at once to one network event on the operator's
# machine, and the orchestrator - which does its own incremental pulls - exited with it. busbar was
# left measuring with nothing holding its results.
#
# Then this script's first version made it worse. It pulled ONLY on completion, and busbar's box hit
# its `shutdown -h` cost backstop at exactly 8h (BENCH_MAX_MIN=480) while 24 of 36 cells were done.
# The box went, and 8 hours of measurement went with it, because the only copy lived on the box. The
# ETA that said 03:45 had been computed against a machine whose own lifetime ended at 00:26; nobody
# compared the two numbers.
#
# So: pull every poll, keep the newest partial, and NEVER terminate. Termination belongs to the
# orchestrator that owns the run; this is insurance, and insurance that can destroy the thing it
# insures is not insurance.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IP="${1:?usage: watch-busbar.sh <ip>}"
KEYFILE="${BENCH_STATE_DIR:-$HOME/.cache/gateway-bench}/gateway-bench-key.pem"
SSHCMD="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=25 -i $KEYFILE"
PARTIAL="$HERE/results/partial"

log() { printf '[%s] busbar-watch: %s\n' "$(date -u +%H:%M:%S)" "$*"; }
mkdir -p "$PARTIAL"

while :; do
  alive="$($SSHCMD "ubuntu@$IP" 'pgrep -c otb || echo 0' 2>/dev/null | tr -d '\r\n ' || echo "?")"
  cells="$($SSHCMD "ubuntu@$IP" 'python3 -c "
import json
try:
    d=json.load(open(\"/home/ubuntu/benchmarking/results/snapshots/busbar.json\"))
    print(sum(len(u.get(\"cells\") or {}) for u in (d.get(\"matrix\",{}).get(\"upstreams\") or {}).values()))
except Exception: print(0)
"' 2>/dev/null | tr -d '\r\n ' || echo "?")"

  # THE PULL HAPPENS EVERY POLL, not at the end. `busbar.json` is the live file the engine rewrites
  # as each cell lands, so this is always the most complete thing that exists.
  if rsync -az --timeout=120 -e "$SSHCMD" \
       "ubuntu@$IP:benchmarking/results/snapshots/busbar.json" "$PARTIAL/busbar.json" 2>/dev/null; then
    got="$(python3 -c "
import json
try:
    d=json.load(open('$PARTIAL/busbar.json'))
    print(sum(len(u.get('cells') or {}) for u in (d.get('matrix',{}).get('upstreams') or {}).values()))
except Exception: print(0)" 2>/dev/null)"
    log "otb=$alive cells=$cells/36 (partial held locally: ${got:-0})"
  else
    log "otb=$alive cells=$cells/36 (pull failed this poll - previous partial kept)"
  fi

  # NEVER READ "NOT STARTED YET" AS "FINISHED". At launch the box is still provisioning and `otb` is
  # absent, so a bare `alive == 0` test would take the exit path before the run had begun - the same
  # shape of mistake that lost eight hours of busbar tonight. The run is only over once it has been
  # seen alive at least once.
  [[ "$alive" =~ ^[1-9] ]] && seen_alive=1
  if [[ "${seen_alive:-0}" == "1" && "$alive" == "0" ]]; then
    rsync -az --timeout=180 -e "$SSHCMD" \
      "ubuntu@$IP:benchmarking/results/snapshots/result_busbar_*.json" "$HERE/results/snapshots/" 2>/dev/null \
      && { log "final snapshot pulled"; break; }
    log "otb gone but no final snapshot yet - retrying"
  fi
  sleep 240
done
log "done - the orchestrator owns termination, this script terminates nothing"
