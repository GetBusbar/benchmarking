#!/usr/bin/env bash
# Pull busbar's partial snapshot off its box EVERY POLL, so a box that dies never costs the whole run -
# and compare the run's ETA to the box's own lifetime, so a run that cannot finish says so while there
# is still time to do something about it.
#
# WHY THIS EXISTS, THREE TIMES OVER.
#
# First: the 2026-08-01 field run lost seven gateways at once to one network event on the operator's
# machine, and the orchestrator - which does its own incremental pulls - exited with it. busbar was
# left measuring with nothing holding its results.
#
# Then this script's first version made it worse. It pulled ONLY on completion, and busbar's box hit
# its `shutdown -h` cost backstop at exactly 8h (BENCH_MAX_MIN=480) while 24 of 36 cells were done.
# The box went, and 8 hours of measurement went with it, because the only copy lived on the box.
#
# Then, on 2026-08-02, it happened AGAIN, to busbar 1.5.0, with this script running and reporting
# healthy progress every poll the whole way down. The pull worked - 24 cells survived - but the run
# still died at 27/36, because 1.5.0 measures at ~16 min/cell and a 36-cell grid needs ~10 h inside a
# box whose lifetime was 8 h. IT WAS NEVER GOING TO FINISH, and every single poll said so if anyone
# had done the arithmetic. A monitor that reports progress and not the DEADLINE THAT PROGRESS IS
# RACING is not monitoring the thing that kills runs. So this now reads the box's own scheduled
# shutdown, projects completion from the measured cell rate, and shouts when the two cross.
#
# And: pull every poll, keep the newest partial, and NEVER terminate. Termination belongs to the
# orchestrator that owns the run; this is insurance, and insurance that can destroy the thing it
# insures is not insurance.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IP="${1:?usage: watch-busbar.sh <ip> [gateway-key]}"
# The snapshot is named for the gateway KEY, and the field runs more than one busbar at a time
# (1.4.1 and 1.5.0 are separate entrants). Defaulting to `busbar` and hardcoding it are different
# things: hardcoded, this script silently watched a file the 1.5.0 box never writes.
GW="${2:-busbar}"
KEYFILE="${BENCH_STATE_DIR:-$HOME/.cache/gateway-bench}/gateway-bench-key.pem"
SSHCMD="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=25 -i $KEYFILE"
PARTIAL="$HERE/results/partial"
REMOTE_SNAP="/home/ubuntu/benchmarking/results/snapshots/$GW.json"
# 6x6 for a field run; override when the grid is narrowed (OTB_DIALECTS) so the ETA is against the
# grid that is actually running.
CELLS_TOTAL="${CELLS_TOTAL:-36}"
# Give up after this many consecutive polls that could not read the box at all (240 s apart), and
# after this many polls that saw the run over with no final snapshot to pull.
MAX_UNREADABLE="${MAX_UNREADABLE:-6}"
MAX_NO_FINAL="${MAX_NO_FINAL:-3}"

log() { printf '[%s] %s-watch: %s\n' "$(date -u +%H:%M:%S)" "$GW" "$*"; }
mkdir -p "$PARTIAL"

count_cells() {  # count measured cells in a snapshot file, on stdin's behalf
  python3 -c "
import json,sys
try:
    d=json.load(open(sys.argv[1]))
    print(sum(len(u.get('cells') or {}) for u in (d.get('matrix',{}).get('upstreams') or {}).values()))
except Exception: print(0)" "$1" 2>/dev/null || echo 0
}

t0=0; c0=0; unreadable=0; no_final=0
while :; do
  # ONE ssh, `key=value` lines, parsed BY NAME. Three separate connections per poll produced false
  # "poll unreadable" alarms, and packing three outputs onto one line and reading them positionally
  # produced a false "ENGINE GONE" on a perfectly healthy run.
  read -r alive cells deadline rundone <<<"$(
    $SSHCMD "ubuntu@$IP" "
      printf 'alive=%s\n' \"\$(pgrep -c otb || echo 0)\"
      printf 'cells=%s\n' \"\$(python3 -c '
import json
try:
    d=json.load(open(\"$REMOTE_SNAP\"))
    print(sum(len(u.get(\"cells\") or {}) for u in (d.get(\"matrix\",{}).get(\"upstreams\") or {}).values()))
except Exception: print(0)' 2>/dev/null || echo 0)\"
      # THE BOX'S OWN DEATH CLOCK. \`shutdown -h +N\` writes USEC=<epoch-microseconds> here; this is
      # the authoritative answer to 'how long does this machine have left', not our own bookkeeping
      # of when we launched it, which is the number nobody checked the night this mattered.
      printf 'deadline=%s\n' \"\$(sed -n 's/^USEC=//p' /run/systemd/shutdown/scheduled 2>/dev/null || echo 0)\"
      # The run script's own completion sentinel: lets a watcher started AFTER the run ended (or
      # restarted after a laptop sleep) see that it is over without ever having seen otb alive.
      printf 'rundone=%s\n' \"\$(cat ~/benchmarking/.run-done 2>/dev/null)\"
    " 2>/dev/null | tr -d '\r' | awk -F= '
      /^alive=/{a=$2} /^cells=/{c=$2} /^deadline=/{d=$2} /^rundone=/{r=$2}
      END{printf "%s %s %s %s", (a==""?"?":a), (c==""?"?":c), (d==""?0:d), (r==""?"-":r)}'
  )"
  # A box that cannot be read at all (terminated, IP gone) is not "still measuring": stop after a
  # bounded run of unreadable polls instead of polling a dead address forever.
  if [[ "$alive" == "?" ]]; then
    unreadable=$((unreadable + 1))
    if (( unreadable >= MAX_UNREADABLE )); then
      log "box unreadable for $unreadable polls in a row - giving up (partial kept in $PARTIAL/$GW.json)"
      exit 1
    fi
  else
    unreadable=0
  fi

  # THE PULL HAPPENS EVERY POLL, not at the end. The snapshot is the live file the engine rewrites as
  # each batch of cells lands, so this is always the most complete thing that exists.
  if rsync -az --timeout=120 -e "$SSHCMD" "ubuntu@$IP:$REMOTE_SNAP" "$PARTIAL/$GW.json" 2>/dev/null; then
    got="$(count_cells "$PARTIAL/$GW.json")"
    held="partial held locally: ${got:-0}"
  else
    held="pull failed this poll - previous partial kept"
  fi

  # DOES THIS RUN FIT IN THIS BOX? Rate comes from the first poll that saw a cell land, so it reflects
  # what this gateway actually measures at rather than what the field averaged last month.
  verdict=""
  if [[ "$cells" =~ ^[0-9]+$ ]] && (( cells > 0 )); then
    now=$(date -u +%s)
    (( t0 == 0 )) && { t0=$now; c0=$cells; }
    if (( cells > c0 && now > t0 && deadline > 0 )); then
      dl=$(( deadline / 1000000 ))
      secs_per_cell=$(( (now - t0) / (cells - c0) ))
      eta=$(( now + secs_per_cell * (CELLS_TOTAL - cells) ))
      left_min=$(( (dl - now) / 60 ))
      need_min=$(( (eta - now) / 60 ))
      if (( eta > dl )); then
        verdict=" !! WILL NOT FINISH: needs ${need_min}m, box dies in ${left_min}m"
      else
        verdict=" (eta ${need_min}m, box dies in ${left_min}m)"
      fi
    fi
  fi
  log "otb=$alive cells=$cells/$CELLS_TOTAL ($held)$verdict"
  # Loud, separate, and unmissable: the summary line above scrolls past in a log nobody reads closely.
  [ -n "$verdict" ] && case "$verdict" in *"WILL NOT FINISH"*)
    log "DEADLINE BREACH - this grid cannot complete before the box self-terminates."
    log "  Raise BENCH_MAX_MIN and relaunch NOW, or accept a partial grid. Waiting changes nothing." ;;
  esac

  # NEVER READ "NOT STARTED YET" AS "FINISHED". At launch the box is still provisioning and `otb` is
  # absent, so a bare `alive == 0` test would take the exit path before the run had begun - the same
  # shape of mistake that lost eight hours of busbar. The run is only over once it has been seen alive.
  # The run's own .run-done sentinel is the other proof it is over, and the only one available to a
  # watcher that started after otb had already exited.
  [[ "$alive" =~ ^[1-9] ]] && seen_alive=1
  if [[ "$alive" == "0" && ( "${seen_alive:-0}" == "1" || "${rundone:--}" != "-" ) ]]; then
    rsync -az --timeout=180 -e "$SSHCMD" \
      "ubuntu@$IP:benchmarking/results/snapshots/result_${GW}_*.json" "$HERE/results/snapshots/" 2>/dev/null \
      && { log "final snapshot pulled"; break; }
    no_final=$((no_final + 1))
    if (( no_final >= MAX_NO_FINAL )); then
      log "RUN ENDED WITHOUT A FINAL SNAPSHOT (run-done=${rundone}) after $no_final pulls - the engine exited before writing result_${GW}_*.json; partial kept in $PARTIAL/$GW.json"
      exit 1
    fi
    log "otb gone but no final snapshot yet - retrying ($no_final/$MAX_NO_FINAL)"
  fi
  sleep 240
done
log "done - the orchestrator owns termination, this script terminates nothing"
