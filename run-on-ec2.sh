#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# One-click, FAIR-BY-ISOLATION: launch ONE fresh Graviton box PER GATEWAY, all in parallel, each from
# a fresh copy of THIS repo. Every gateway is measured on a pristine machine - no chance one gateway's
# leftover page cache, disk, or docker state skews the next. Same total cost as a single sequential box
# (N boxes for ~1/N the wall-clock), and much faster end to end.
#
#   run-on-ec2.sh                                   # all gateways, one box each, in parallel
#   run-on-ec2.sh <name> <name>                     # a subset, one box each
#
# Requires awscli v2 (configured), ssh, rsync. Each box is m7g.4xlarge (16 real Graviton3 cores): the
# gateway-under-test is pinned to 4 cores (= an m7g.xlarge, the class AIGatewayBench uses); the mock +
# load generator get 6 cores each, so the harness can never steal cycles from the gateway. EVERY
# gateway build/pulls itself on its box from the ref pinned in its own gateways/<name>/definition.json.
set -uo pipefail
export AWS_DEFAULT_REGION="${AWS_DEFAULT_REGION:-us-east-1}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # this repo (benchmarking) root

# Per-invocation run id: every box THIS run launches is tagged run=$RUN_ID, and teardown filters on
# it so a second (or concurrent) invocation never terminates the first run's boxes or pulls the rug on
# its results. The global `kill` subcommand stays the cross-run cleanup.
RUN_ID="${RUN_ID:-$(date +%Y%m%d-%H%M%S)-$$}"
CREATED_KEY=0; CREATED_SG=0   # only delete the shared key/SG on exit if THIS invocation created them

# Box self-terminate safety net: `shutdown -h +N` is the leaked-box backstop, armed at cloud-init
# (minutes before the matrix clock starts, since apt + docker + rsync + gateway build take ~5-10 min
# first) so the box clock leads the matrix clock by that same startup lead. Matrix raises its own
# wall-clock ceiling to the same 480 min (matrix/run.sh: HARNESS_SUITE_CEIL_S default 28800 when
# MATRIX_SWEEP=1), deliberately equal to this box net: on a genuinely wedged gateway, AWS terminates the
# box a few minutes before the matrix ceiling would have tripped, so a wedged box forfeits its partial
# results to AWS termination rather than exiting cleanly with a capped grid, which is intended, since a
# partial grid from a wedged run is not something worth publishing anyway. Both are overridable; raise
# BOTH together, keeping the box net at or above the matrix ceiling, if a gateway legitimately needs
# more than 8 h.
BENCH_MAX_MIN="${BENCH_MAX_MIN:-480}"
# How long a FINISHED box keeps itself alive so its results can still be pulled. The boot-time
# backstop is sized for the work; this one is sized for the harvest, and the box swaps to it as soon as
# it writes `.run-done`. Bounded, so a forgotten box still cannot bleed cost indefinitely.
HARVEST_GRACE_MIN="${HARVEST_GRACE_MIN:-120}"

# WHAT THE BOXES MEASURE, resolved ONCE for the whole run.
#
# Every box clones this repo at this exact commit. Resolving per box would let a push mid-run split
# the field across two revisions and publish them beside each other as though they were comparable.
# The default is the orchestrator's own HEAD, which must be pushed: a commit that exists only here
# cannot be cloned, and that failure is loud rather than silently measuring something else.
BENCH_REPO="${BENCH_REPO:-https://github.com/GetBusbar/benchmarking.git}"
BENCH_COMMIT="${BENCH_COMMIT:-$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse HEAD 2>/dev/null)}"
# The archive GitHub serves wraps everything in <repo>-<sha>/, so the box needs the repo's own name
# to name the members it wants. Derived from the clone URL rather than hardcoded.
_REPO_NAME="$(basename "${BENCH_REPO%.git}")"

# ── LAUNCH PRECONDITIONS: two git facts, checked before a single box is created ───────────────────
#
# Both failures below happened on 2026-07-30, back to back, and each cost a full fan-out:
#
#   DIRTY TREE   Two agents held uncommitted edits when the run launched, so the snapshots recorded
#                `engine.commit=<sha> with uncommitted edits`. C8 rejected the whole board on arrival
#                - correctly, because a commit that does not describe the working tree does not
#                identify the instrument, and an unidentifiable instrument is not reproducible. The
#                measurement itself was fine; it was simply unusable.
#
#   UNPUSHED     HEAD was committed but not pushed. Every box fetches the harness BY SHA from origin,
#                so all 14 died with `FETCH FAILED (rc=22)` about ninety seconds in. The comment above
#                already said the commit "must be pushed" and that the failure is loud - it is, but it
#                is loud AFTER fourteen instances exist and have been paid for.
#
# Both are one git command. Checking them here turns two lost fan-outs into two lines of output. Skip
# with BENCH_SKIP_PREFLIGHT=1 for a deliberate experiment on a local revision.
if [ -z "${BENCH_SKIP_PREFLIGHT:-}" ] && [ -n "$BENCH_COMMIT" ]; then
  _here="$(dirname "${BASH_SOURCE[0]}")"
  if [ -n "$(git -C "$_here" status --porcelain 2>/dev/null)" ]; then
    echo "PREFLIGHT FAILED: the harness tree is DIRTY, so \`engine.commit\` would not identify what ran." >&2
    echo "  C8 rejects a board measured on a dirty tree - it is not reproducible. Commit or stash first:" >&2
    git -C "$_here" status --short 2>/dev/null | sed 's/^/    /' >&2
    exit 1
  fi
  if ! git -C "$_here" branch -r --contains HEAD 2>/dev/null | grep -q .; then
    echo "PREFLIGHT FAILED: HEAD ($BENCH_COMMIT) is not on any remote branch." >&2
    echo "  Every box fetches the harness BY SHA from $BENCH_REPO, so all of them would fail to fetch" >&2
    echo "  a commit that exists only on this machine. Push first." >&2
    exit 1
  fi
fi

# ── INCREMENTAL PER-GATEWAY PUBLISH (matrix-sole-source) ──────────────────────────────────────────
# Each gateway's entire benchmark is one atomic matrix run, and gateways publish independently: we
# commit + push each gateway's result the moment its box finishes cleanly (DONE, all suites pulled,
# promote guard passed), rather than the operator publishing everything by hand at the end. The board
# fills in gateway-by-gateway; the Pages deploy regenerates data.json from all committed results/ on
# every push. `run-on-ec2.sh busbar` re-runs and publishes only busbar, following the same path.
#
# PUBLISH gates the auto-push. Default ON for the field run; set PUBLISH=0 for a local/dry run so a
# development run never pushes. When off, results are still pulled and left uncommitted in the working
# tree for the operator to inspect and publish by hand.
PUBLISH="${PUBLISH:-1}"
# Branch to push results to (the Pages deploy watches this). Overridable for a test branch.
PUBLISH_BRANCH="${PUBLISH_BRANCH:-$(git -C "$HERE" rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)}"
PUBLISH_REMOTE="${PUBLISH_REMOTE:-origin}"
# Serialize all git operations across the parallel per-gateway boxes: commit + push touch the shared
# index/refs, so two boxes finishing at once would race (one's `git add` sees the other's half-staged
# tree, or two concurrent pushes collide). A single lock dir makes publish strictly one-at-a-time.
#
# Keyed on the checkout path ($HERE), not on RUN_ID: two invocations against the same working tree
# (e.g. a field run still finishing while an operator re-runs one gateway) must contend for the SAME
# lock, since they would otherwise `git add`/`git commit`/`git rebase`/`git push` against the same
# index and HEAD concurrently.
PUBLISH_LOCK="${TMPDIR:-/tmp}/gateway-bench-publish-$(printf '%s' "$HERE" | tr -c 'A-Za-z0-9' '_').lock"

# push_with_rebase <tag> <log_fn>: fetch/rebase-then-push, retried in a bounded loop. With many boxes
# plus the render-charts.yml bot pushing to the same branch, the remote ref moves constantly, so we fetch
# the remote tip and rebase our local commit(s) onto it before each push, retrying up to 5 times (re-fetch
# and re-rebase each pass) to survive a ref that moves again between our fetch and our push. MUST be
# called while holding the publish lock (callers already do). Prints via $2, returns 0 on a successful
# push, 1 if all attempts failed (commit stays local, logged loudly, never stranded silently).
# Conflict safety: each gateway commits only its OWN result paths, so a rebase rarely conflicts; the one
# realistic overlap is a bot chart commit touching results/*.png. We rebase with -X theirs so the rebase
# can never halt mid-way leaving a detached, conflicted, un-pushable state; a genuine conflict is logged
# loudly rather than stranding the publish. During a rebase, "ours" is the upstream being replayed onto
# and "theirs" is the local commit being replayed, so `-X theirs` keeps our freshly-committed side on
# overlap, the desired outcome for per-gateway result JSON (this run's fresh result wins). The one cost:
# an overlapping bot-regenerated results/*.png keeps our stale local PNG rather than the bot's newer one,
# harmless because charts are regenerated field-wide in the final sweep.
push_with_rebase() {
  local _tag="$1" _log="$2" _attempt=0 _max=5
  while [ "$_attempt" -lt "$_max" ]; do
    _attempt=$((_attempt+1))
    # Pull the remote tip in and replay our local commit(s) on top. Non-interactive; -X theirs so an
    # overlapping bot chart commit never aborts the rebase.
    if ! git -C "$HERE" fetch "$PUBLISH_REMOTE" "$PUBLISH_BRANCH" >/dev/null 2>&1; then
      "$_log" "$_tag publish: fetch $PUBLISH_REMOTE/$PUBLISH_BRANCH FAILED (attempt $_attempt/$_max) - retrying"
      sleep 3; continue
    fi
    # `git rebase` refuses to start with a dirty tracked file ("cannot rebase: You have unstaged
    # changes."). All bench_gateway jobs share ONE repo at $HERE, and the incremental pull loops
    # `mv -f` a fresh results/matrix/<other>.json over a previously-committed tracked file outside the
    # publish lock, so while this box holds the lock and rebases, a peer box can leave an unstaged
    # tracked change that would otherwise abort our rebase. `-c rebase.autostash=true` stashes the
    # peer's unstaged change before the rebase and pops it after (autostash is unset globally/locally on
    # the rig, so set it per invocation rather than depend on repo config).
    if ! _rebase_err="$(git -C "$HERE" -c rebase.autostash=true rebase -X theirs "$PUBLISH_REMOTE/$PUBLISH_BRANCH" 2>&1)"; then
      # Distinguish a GENUINE merge conflict (a rebase is left in progress - .git/rebase-merge exists)
      # from a start-time refusal. With autostash on, the "unstaged changes" refusal no longer happens,
      # but if a stash POP fails after a clean rebase the tree can carry a conflicted stash - either way,
      # if a rebase is mid-flight abort it to a clean pushable HEAD; else it never started, so nothing to
      # abort. Both retry (a transient peer-dirty state clears on the next pass).
      local _gitdir; _gitdir="$(git -C "$HERE" rev-parse --git-dir 2>/dev/null)"
      if [ -d "$HERE/$_gitdir/rebase-merge" ] || [ -d "$HERE/$_gitdir/rebase-apply" ] || [ -d "$_gitdir/rebase-merge" ] || [ -d "$_gitdir/rebase-apply" ]; then
        git -C "$HERE" rebase --abort >/dev/null 2>&1 || true
        "$_log" "$_tag publish: rebase onto $PUBLISH_REMOTE/$PUBLISH_BRANCH CONFLICTED (attempt $_attempt/$_max) - aborted rebase, retrying"
      else
        "$_log" "$_tag publish: rebase could not start (attempt $_attempt/$_max; likely a peer's unstaged results write - autostash should absorb it): ${_rebase_err%%$'\n'*} - retrying"
      fi
      sleep 3; continue
    fi
    if git -C "$HERE" push "$PUBLISH_REMOTE" "HEAD:$PUBLISH_BRANCH" >/dev/null 2>&1; then
      return 0
    fi
    # Push rejected - the ref moved again between our fetch and our push. Loop to re-fetch + re-rebase.
    "$_log" "$_tag publish: push rejected (ref moved; attempt $_attempt/$_max) - re-fetch + rebase + retry"
    sleep 3
  done
  "$_log" "$_tag publish: push to $PUBLISH_REMOTE/$PUBLISH_BRANCH FAILED after $_max attempts (commit is local; retry by hand or re-run)"
  return 1
}

# ── serialize the publish critical section ──────────────────────────────────────────────────────
# The Darwin orchestrator has no util-linux `flock`, so the mkdir spin-lock is the live publish path on
# it. On a timeout, abort (return non-zero, counted as a publish issue) rather than proceeding unlocked.
# The cleanup rmdir only fires when this publish owns the lock, verified by a unique per-publish token
# written into the lockdir (RUN_ID:tag:BASHPID:random, distinct per publish subshell), so one holder can
# never rmdir a peer's lock out from under it. flock stays the fast path where available (Linux boxes /
# any host with util-linux).
#
# Usage:  publish_lock_acquire "<tag>" <log_fn>  || return 1   # (subshell: `exit 1`)
#         ... critical section ...
#         publish_lock_release
# Sets PUBLISH_LOCK_FD (flock path) or PUBLISH_LOCK_OWNED=1 + PUBLISH_LOCK_TOKEN (mkdir path) for release.
publish_lock_acquire() {
  local _tag="$1" _log="$2"
  PUBLISH_LOCK_FD=""; PUBLISH_LOCK_OWNED=0; PUBLISH_LOCK_TOKEN=""
  if command -v flock >/dev/null 2>&1; then
    # Bound the flock wait (matching the mkdir path's 600s ceiling): without -w a hung lock holder would
    # block every peer indefinitely. On timeout, abort this publish (return 1, counted as an issue)
    # rather than blocking forever; close the fd so no half-open lock leaks.
    exec 9>"$PUBLISH_LOCK"
    if flock -w 600 9; then
      PUBLISH_LOCK_FD=9
      return 0
    fi
    "$_log" "$_tag publish: could NOT acquire the publish flock after 600s (a peer box is holding it), ABORTING this publish rather than blocking forever (retry by hand or re-run)"
    eval "exec 9>&-" 2>/dev/null || true
    return 1
  fi
  # mkdir spin-lock: `mkdir` is atomic, so exactly one waiter wins the create. Bounded wait for a peer.
  local _spun=0
  until mkdir "${PUBLISH_LOCK}.d" 2>/dev/null; do
    sleep 2; _spun=$((_spun+2))
    if [ "$_spun" -ge 600 ]; then
      "$_log" "$_tag publish: could NOT acquire the publish lock after ${_spun}s (a peer box is holding it), ABORTING this publish rather than pushing UNLOCKED (retry by hand or re-run)"
      return 1
    fi
  done
  # The owner token must be unique per publish, not just per process: `$$` is identical across every `&`
  # background subshell of one orchestrator (only $BASHPID differs), so it cannot distinguish holders on
  # its own. Token is RUN_ID + gateway tag + $BASHPID + a random id, distinct for every bench_gateway
  # subshell of the same parent. Release only rmdir's when the token in the lockdir still matches ours,
  # so a stale/handed-off dir is never deleted out from under a real holder.
  local _rand=""
  _rand="$(od -An -N8 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  [ -n "$_rand" ] || _rand="${RANDOM}${RANDOM}"
  PUBLISH_LOCK_TOKEN="${RUN_ID:-run}:${_tag}:${BASHPID:-$$}:${_rand}"
  printf '%s\n' "$PUBLISH_LOCK_TOKEN" > "${PUBLISH_LOCK}.d/token" 2>/dev/null || true
  PUBLISH_LOCK_OWNED=1
  return 0
}

# Release only a lock this process owns. For the mkdir path, re-verify the unique token inside the
# lockdir still matches the one this publish wrote before rmdir'ing, so a stale/handed-off dir (a peer
# that re-acquired after we timed out and lost ownership) is never deleted out from under its real holder.
publish_lock_release() {
  if [ -n "${PUBLISH_LOCK_FD:-}" ]; then
    flock -u "$PUBLISH_LOCK_FD" 2>/dev/null || true
    eval "exec ${PUBLISH_LOCK_FD}>&-" 2>/dev/null || true
    PUBLISH_LOCK_FD=""
    return 0
  fi
  if [ "${PUBLISH_LOCK_OWNED:-0}" = 1 ]; then
    local _owner=""; _owner=$(cat "${PUBLISH_LOCK}.d/token" 2>/dev/null || echo "")
    if [ -n "${PUBLISH_LOCK_TOKEN:-}" ] && [ "$_owner" = "$PUBLISH_LOCK_TOKEN" ]; then
      rm -f "${PUBLISH_LOCK}.d/token" 2>/dev/null || true
      rmdir "${PUBLISH_LOCK}.d" 2>/dev/null || true
    fi
    PUBLISH_LOCK_OWNED=0; PUBLISH_LOCK_TOKEN=""
  fi
}

# `run-on-ec2.sh kill` - terminate every gateway-bench box right now, reliably. Uses xargs so the
# instance IDs are split into separate args: piping `--output text` straight into `--instance-ids`
# passes one tab-joined blob, which AWS rejects as InvalidInstanceID.Malformed and silently no-ops. Run
# this if a run is ever interrupted and you want a guaranteed cleanup.
if [[ "${1:-}" == "kill" || "${1:-}" == "--kill" ]]; then
  echo "terminating all gateway-bench instances in $AWS_DEFAULT_REGION ..."
  aws ec2 describe-instances --filters "Name=tag:purpose,Values=gateway-bench" \
    "Name=instance-state-name,Values=running,pending,stopping,stopped" \
    --query 'Reservations[].Instances[].InstanceId' --output text 2>/dev/null \
    | tr '\t' '\n' | grep -E '^i-' \
    | xargs -r -n 25 aws ec2 terminate-instances --output text --instance-ids >/dev/null 2>&1
  left=$(aws ec2 describe-instances --filters "Name=tag:purpose,Values=gateway-bench" \
    "Name=instance-state-name,Values=running,pending" --query 'length(Reservations[].Instances[])' --output text 2>/dev/null)
  echo "done - running/pending remaining: ${left:-?}"
  # Local side of an interrupted run: stray per-gateway fanout logs, cached rig binaries (in case the
  # next run needs a different arch or a fresh rig), and any left-behind pull-staging files. None of
  # this is committed data - real results only ever land via a promote-guarded mv - so it is always
  # safe to clear.
  rm -f "$HERE"/results/fanout-*.log
  rm -f "$HERE"/results/*/.incoming-*.json "$HERE"/results/config/.incoming-*.txt
  rm -rf "$HERE"/bin
  echo "local cleanup: fanout logs, cached rig binaries, and pull-staging files cleared"
  exit 0
fi

# ── ARCHITECTURE: the easy flip ───────────────────────────────────────────────────────────────────
# ARCH=arm64 (default) runs the whole field on Graviton (m7g); ARCH=x86 runs it on Intel (m7i). One
# knob picks the instance family AND the matching Ubuntu AMI. Every gateway builds/pulls for that arch
# on its own box, and the arch is recorded INSIDE each result JSON ("arch": …) so runs from different
# arches never get confused. NOTE: results paths are NOT arch-namespaced (they are results/<suite>/<gw>.json
# for every arch), so a back-to-back run on the other arch OVERWRITES the file; the arch tag inside the
# JSON is the dedupe key. To keep both arches' data, copy results/ aside between the two runs.
ARCH="${ARCH:-arm64}"

# ── THE ENGINE STAMP ──────────────────────────────────────────────────────────────────────────────
# Every box in a field run must measure with the SAME harness, because a defect in the instrument is
# a defect in all thirteen columns at once. Captured HERE, once, from the tree that is about to be
# rsynced, and carried to each box as an env var: the copy that lands on the box has no .git, so it
# cannot work its own commit out. lib/rig.sh folds this into each snapshot's rig provenance, and
# scripts/check-engine.sh refuses a board whose snapshots disagree.
#
# A DIRTY TREE IS RECORDED AS DIRTY. Uncommitted edits mean the commit does not identify what ran, so
# the run is marked non-reproducible rather than being filed under a commit it does not match. The
# site's C8 check refuses to publish a dirty run, which is right: nobody can reproduce it.
#
# results/ IS EXCLUDED, because it holds this run's own OUTPUT. A whole field run was stamped dirty -
# and correctly refused by C8 - because the operator had created results/runlog-<stamp>/ to record
# the run's provenance before launching it. The directory that existed to make the run reproducible
# was the thing that marked it irreproducible. Run artefacts cannot change what the engine does, so
# they cannot make a commit stop identifying it.
#
# Everything else still counts, including UNTRACKED files: a new gateways/<name>/ directory is
# untracked and absolutely does change what runs.
BENCH_ENGINE_COMMIT="$(git -C "$HERE" rev-parse HEAD 2>/dev/null || echo '')"
if [ -n "$(git -C "$HERE" status --porcelain -- . ':(exclude)results' 2>/dev/null)" ]; then BENCH_ENGINE_DIRTY=1; else BENCH_ENGINE_DIRTY=0; fi
export BENCH_ENGINE_COMMIT BENCH_ENGINE_DIRTY

# THE BOX HAS NO HISTORY, SO THE ORCHESTRATOR HANDS IT ONE.
#
# The engine qualifies its box by driving the mock directly and comparing against a rolling baseline
# of previous observations, which it reads from the results directory. That is right where the engine
# runs beside the record; in the field it does not. Every gateway gets a fresh instance that fetches
# its manifest and the rig and nothing else, so that directory is empty, the baseline is absent, and
# the qualification SEEDS instead of judging - which is what every snapshot in results/ shows, on
# every box, for every gateway, since the check was written. An inert guard: a box running well under
# the field's rate would have seeded a fresh baseline, passed, and had a whole gateway column
# measured on it.
#
# The observation is the RIG's own loopback throughput at a fixed concurrency, not the gateway's, so
# the baseline pools across ALL gateways rather than being per gateway: it is the box being
# qualified, and the boxes are identical by construction. Median, so one anomalous past box cannot
# move it. Empty (and skipped) when there is no history yet, which is exactly the first run.
OTB_QUALIFY_BASELINE="$(
  find "$HERE/results/snapshots" -name 'result_*.json' -type f 2>/dev/null \
    | xargs -r grep -ho '"observed_rps"[[:space:]]*:[[:space:]]*[0-9.]*' 2>/dev/null \
    | sed 's/.*:[[:space:]]*//' \
    | sort -n \
    | awk '{ v[NR] = $1 } END { if (NR) printf "%.0f", (NR % 2) ? v[(NR + 1) / 2] : (v[NR / 2] + v[NR / 2 + 1]) / 2 }'
)"
if [ -n "$OTB_QUALIFY_BASELINE" ]; then
  export OTB_QUALIFY_BASELINE
  echo "box qualification baseline: ${OTB_QUALIFY_BASELINE} rps (median of prior observations)"
else
  echo "box qualification: no prior observations, this run seeds the baseline"
fi

case "$ARCH" in
  arm64|aarch64|graviton)
    ARCH=arm64
    # 4xlarge (16 cores): the gateway-under-test still gets EXACTLY 4 pinned cores (the fair,
    # comparable basis - perf/RPS/memory are unchanged vs the old 2xlarge), but the mock + load
    # generator get 6 cores each instead of 2. At 2 cores the mock topped out ~48k frames/sec, so a
    # 1024-stream sweep (~51k fps needed) saturated the MOCK, not the gateway, and mock-late frames
    # showed up as gateway "stalls". With 6 mock cores the ceiling is ~3x, so the high-concurrency
    # streaming rungs measure the gateway, not the rig.
    ITYPE="${ITYPE:-m7g.4xlarge}"
    SSM="/aws/service/canonical/ubuntu/server/24.04/stable/current/arm64/hvm/ebs-gp3/ami-id"
    CPU_LABEL="Graviton3" ;;
  x86|x86_64|amd64|intel)
    ARCH=x86
    ITYPE="${ITYPE:-m7i.4xlarge}"
    SSM="/aws/service/canonical/ubuntu/server/24.04/stable/current/amd64/hvm/ebs-gp3/ami-id"
    CPU_LABEL="Intel (Sapphire Rapids)" ;;
  *) echo "unknown ARCH='$ARCH' (use arm64 or x86)"; exit 2 ;;
esac
HW_LABEL="AWS ${ITYPE} (${CPU_LABEL}, 16 cores / 64 GB). Gateway-under-test pinned to 4 cores (the comparable basis); mock and load generator on 6 cores each so the mock never bottlenecks the streaming sweep. Ubuntu 24.04. One dedicated box per gateway."
KEYNAME="gateway-bench-key"; KEYFILE="${TMPDIR:-/tmp}/${KEYNAME}.pem"; SGNAME="gateway-bench-sg"
SSHOPT="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=12 -i $KEYFILE"

# NOTE ON PLACEMENT: `harvest` sits HERE, below KEYFILE/SSHOPT, not up with `kill`. It needs the ssh
# identity to reach a box; `kill` only needs the AWS API and can therefore live earlier. The first
# version of this block was written beside `kill` and would have run every rsync with an empty $SSHOPT
# - the same shape as the dead gates this script's own audit keeps turning up.
# `run-on-ec2.sh harvest [gw ...]` - PULL FROM BOXES A DEAD ORCHESTRATOR LEFT BEHIND.
#
# Every pull in this script lives inside the foreground process that launched the boxes. The boxes do
# not: they run detached under setsid, so they finish the grid and write their snapshots whether or not
# anyone is still listening. If the orchestrator dies - a closed terminal, a slept laptop, a dropped
# SSH session - nothing pulls, and the box's own shutdown timer eventually terminates it with
# DeleteOnTermination=true on the root volume. Finished measurements, deleted on a timer.
#
# That happened on 2026-07-29: four gateways completed 36-cell runs and sat unharvested because the
# publisher had stopped. There was no way to reattach, so they were pulled by hand with ad-hoc rsync.
# This is that recovery, written down: find the live boxes by their shared tag, pull whatever each one
# has on disk, and say plainly what it found. Safe to run at any time, including mid-run - it copies,
# it never terminates, and a partial snapshot is worth having now that the engine writes them
# incrementally.
if [[ "${1:-}" == "harvest" ]]; then
  shift
  want=("$@")
  echo "harvest: looking for gateway-bench boxes in $AWS_DEFAULT_REGION ..."
  rows=$(aws ec2 describe-instances --filters "Name=tag:purpose,Values=gateway-bench" \
    "Name=instance-state-name,Values=running" \
    --query 'Reservations[].Instances[].[Tags[?Key==`Name`]|[0].Value,PublicIpAddress]' \
    --output text 2>/dev/null | grep -E '^gateway-bench-' || true)
  if [ -z "$rows" ]; then echo "harvest: no running boxes found"; exit 0; fi

  mkdir -p "$HERE/results/snapshots" "$HERE/results/config" "$HERE/results/history"
  found=0
  while read -r tag ip; do
    [ -n "$ip" ] || continue
    gw="${tag#gateway-bench-}"
    # An explicit gateway list narrows it; no list means every box that is up.
    if [ "${#want[@]}" -gt 0 ]; then
      case " ${want[*]} " in *" $gw "*) ;; *) continue ;; esac
    fi
    echo "harvest: $gw at $ip"
    # `|| true` on each: one unreachable box must not stop the others being rescued, which is the whole
    # point of running this at all. rc=23 is rsync's "source absent" - a box with nothing written yet.
    rsync -az --timeout=60 -e "ssh $SSHOPT" \
      "ubuntu@$ip:~/benchmarking/results/snapshots/result_${gw}_*.json" \
      "$HERE/results/snapshots/" 2>/dev/null || true
    rsync -az --timeout=60 -e "ssh $SSHOPT" \
      "ubuntu@$ip:~/benchmarking/results/config/$gw.txt" "$HERE/results/config/" 2>/dev/null || true
    rsync -az --timeout=60 -e "ssh $SSHOPT" \
      "ubuntu@$ip:~/benchmarking/results/history/$gw.jsonl" "$HERE/results/history/" 2>/dev/null || true
    # The run log is the only record of WHY a metric came back absent, and it is not in the snapshot.
    rsync -az --timeout=60 -e "ssh $SSHOPT" \
      "ubuntu@$ip:~/benchmarking/.run.log" "$HERE/results/history/${gw}.run.log" 2>/dev/null || true
    n=$(ls -1 "$HERE"/results/snapshots/result_"$gw"_*.json 2>/dev/null | wc -l | tr -d ' ')
    done_rc=$(ssh $SSHOPT ubuntu@"$ip" 'cat ~/benchmarking/.run-done 2>/dev/null' 2>/dev/null || true)
    cells=$(ssh $SSHOPT ubuntu@"$ip" 'grep -cE "^\[cell " ~/benchmarking/.run.log 2>/dev/null' 2>/dev/null || echo 0)
    echo "harvest: $gw -> $n snapshot(s) on disk, ${cells:-0} cells logged, run-done=${done_rc:-<still running>}"
    found=$((found + 1))
  done <<< "$rows"

  echo "harvest: pulled from $found box(es). Nothing was terminated - use 'run-on-ec2.sh kill' for that."
  echo "harvest: regenerate and check the board before publishing:"
  echo "  node site/gen-data.mjs && node site/check-consistency.mjs && python3 bench-audit.py"
  exit 0
fi

log(){ echo "[$(date +%H:%M:%S)] $*"; }

# Default field: every gateway with a manifest under gateways/ (discovered from disk, alphabetical;
# same source as run-all.sh - add/remove a dir and both follow). Envoy AI Gateway is absent (k8s-native).
DEFAULT_GATEWAYS=()
for d in "$HERE"/gateways/*/definition.json; do DEFAULT_GATEWAYS+=("$(basename "$(dirname "$d")")"); done
if [[ $# -gt 0 ]]; then GATEWAYS=("$@"); else GATEWAYS=("${DEFAULT_GATEWAYS[@]}"); fi

# ── shared AWS setup (key + SG), done once ────────────────────────────────────────────────────────
# The keypair + local private key are created together and then REUSED across invocations. We must
# NOT delete-and-recreate on every run: a second (or concurrent) invocation that recreates the AWS
# keypair invalidates the private key that boxes from a still-running invocation were launched with,
# so every later `ssh`/rsync to those boxes fails with "Permission denied (publickey)" and their
# results can never be pulled. Reuse the existing keyfile when present; only (re)create the pair when
# the local key is missing (first run, or a wiped $TMPDIR), keeping AWS + local in lockstep.
# (Re)create the pair when the local key is missing OR the AWS keypair no longer exists - the latter
# happens when the keypair was cleaned up out-of-band (teardown, `kill`, manual) while the local .pem
# lingered; reusing that stale local key launches every box into "key pair does not exist". Checking
# AWS too keeps them in lockstep.
if [[ ! -s "$KEYFILE" ]] || ! aws ec2 describe-key-pairs --key-names "$KEYNAME" >/dev/null 2>&1; then
  aws ec2 delete-key-pair --key-name "$KEYNAME" >/dev/null 2>&1 || true
  rm -f "$KEYFILE"
  # Create the private key under a 077 umask so it is 600 from birth - no sub-millisecond window at the
  # default umask between create and chmod. The chmod stays as a belt-and-braces backstop.
  ( umask 077; aws ec2 create-key-pair --key-name "$KEYNAME" --query KeyMaterial --output text > "$KEYFILE" ); chmod 600 "$KEYFILE"
  CREATED_KEY=1
fi
SG=$(aws ec2 describe-security-groups --group-names "$SGNAME" --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null || true)
if [[ -z "$SG" || "$SG" == "None" ]]; then
  SG=$(aws ec2 create-security-group --group-name "$SGNAME" --description "gateway bench SSH" --query GroupId --output text)
  CREATED_SG=1
fi
# Fetch our public IP for the SSH ingress rule. A transient checkip hiccup that returns an empty/
# malformed MYIP would make `--cidr "/32"` get rejected by AWS and swallowed by `|| true`; on a
# freshly-created SG that leaves NO port-22 rule, so ssh to every box times out and the whole run
# records a field-wide false "did not serve" while burning N boxes. Retry, then fail
# loudly if we still don't have a valid IPv4 - do NOT authorize a malformed CIDR.
MYIP=""
for _try in 1 2 3; do
  MYIP=$(curl -fsS --max-time 10 https://checkip.amazonaws.com | tr -d '[:space:]')
  [[ "$MYIP" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] && break
  MYIP=""; sleep 2
done
[[ -n "$MYIP" ]] || { echo "FATAL: could not determine a valid public IPv4 from checkip.amazonaws.com (3 tries) - refusing to launch boxes into an SG with no SSH ingress rule" >&2; exit 1; }
# Add the port-22 ingress for THIS IP idempotently. On a REUSED SG (CREATED_SG=0, the norm after any
# SIGKILL'd run) each run from a new IP would otherwise accrete a /32 rule that is never revoked; at
# the AWS default 60-rule cap `authorize` starts failing and, if that failure is swallowed, the
# current IP ends up with NO SSH ingress and every ssh/rsync times out. So: treat the
# EXPECTED "rule already present" (InvalidPermission.Duplicate) as success, but a GENUINE failure
# (anything else - malformed CIDR, RulesPerSecurityGroupLimitExceeded at the cap) as FATAL rather than
# a soft note, since a box fleet launched into an SG with no reachable SSH just burns cost.
_sg_err=$(aws ec2 authorize-security-group-ingress --group-id "$SG" --protocol tcp --port 22 --cidr "${MYIP}/32" 2>&1) \
  && echo "authorized SSH ingress for ${MYIP}/32 on $SG" \
  || { case "$_sg_err" in
         *InvalidPermission.Duplicate*) echo "SSH ingress for ${MYIP}/32 already present on $SG (ok)" ;;
         *) echo "FATAL: authorize-security-group-ingress for ${MYIP}/32 failed: $_sg_err" >&2
            echo "       (SG rule cap reached, or malformed CIDR - refusing to launch boxes into an SG the current IP cannot reach)" >&2
            exit 1 ;;
       esac; }

# TIDINESS + COST: on ANY exit (normal, error, Ctrl-C, SIGTERM) terminate ONLY the boxes THIS run
# launched - filtered by tag:run=$RUN_ID, NOT the shared purpose=gateway-bench tag - so a second or
# concurrent invocation never terminates another run's still-live boxes before their results are
# pulled. (SIGKILL can't be trapped; the boxes' own `shutdown -h` timer is the backstop.)
# IDs are split via xargs - piping --output text straight into --instance-ids passes a tab-joined
# blob that no-ops. The shared keypair/SG are deleted only if THIS invocation created them (otherwise
# a concurrent run's ssh/rsync would break with "Permission denied (publickey)"); `run-on-ec2.sh kill`
# stays the global cleanup for the shared key/SG.
teardown() {
  aws ec2 describe-instances --filters "Name=tag:run,Values=$RUN_ID" \
    "Name=instance-state-name,Values=running,pending" --query 'Reservations[].Instances[].InstanceId' --output text 2>/dev/null \
    | tr '\t' '\n' | grep -E '^i-' | xargs -r -n25 aws ec2 terminate-instances --output text --instance-ids >/dev/null 2>&1
  if [[ "$CREATED_KEY" == 1 ]]; then aws ec2 delete-key-pair --key-name "$KEYNAME" >/dev/null 2>&1 || true; rm -f "$KEYFILE"; fi
  if [[ "$CREATED_SG" == 1 ]]; then
    # The just-terminated instances are `shutting-down`, not `running/pending`, but they STILL hold
    # ENI associations to this SG for a short window - so an IMMEDIATE delete-security-group fails with
    # DependencyViolation (swallowed by `|| true`) and the SG persists (a first-run-only cost leak). Wait
    # for the instances THIS run launched to reach `terminated` (ENIs detached) before deleting the SG.
    # Best-effort + time-bounded so teardown never hangs; the SG is shared/reused so a leaked one is minor.
    local _tids
    _tids=$(aws ec2 describe-instances --filters "Name=tag:run,Values=$RUN_ID" \
      --query 'Reservations[].Instances[].InstanceId' --output text 2>/dev/null | tr '\t' '\n' | grep -E '^i-' | tr '\n' ' ')
    if [[ -n "${_tids// }" ]]; then
      # aws ec2 wait has its own ~600s ceiling; wrap in a timeout where available (Linux) so a wedged wait
      # can't hang teardown. macOS has no `timeout` - fall back to the bare wait (its own ceiling applies).
      if command -v timeout >/dev/null 2>&1; then
        timeout 300 aws ec2 wait instance-terminated --instance-ids $_tids >/dev/null 2>&1 || true
      else
        aws ec2 wait instance-terminated --instance-ids $_tids >/dev/null 2>&1 || true
      fi
    fi
    aws ec2 delete-security-group --group-id "$SG" >/dev/null 2>&1 || true
  fi
}
trap teardown EXIT INT TERM

AMI=$(aws ssm get-parameter --name "$SSM" --query Parameter.Value --output text)

mkdir -p "$HERE"/results/{perf,memory,stream,xlate,governed,matrix,snapshots}

# ── commit + push ONE gateway's result (incremental publish) ──────────────────────────────────────
# Called from bench_gateway the moment that box has cleanly finished (DONE). Commits ONLY this
# gateway's freshly-pulled result files (its per-suite JSONs, its append-only history line, its OOTB
# config sidecar, and any regenerated per-gateway chart) and pushes them, so the board updates just
# this row. No-op (returns 0) when PUBLISH=0 so a local/dry run never pushes. Serialized under a flock
# so the parallel boxes commit + push strictly one-at-a-time (shared index/refs). Best-effort: a push
# failure is logged loudly and returns non-zero (counted as a run issue) but never aborts other boxes.
publish_gateway() { # gw glog_echo_fn
  local gw="$1"
  [[ "$PUBLISH" == "1" ]] || { echo "[$gw] PUBLISH=0 - not committing/pushing (result left in the working tree)"; return 0; }
  # Serialize: only one box commits/pushes at a time. flock on a lock fd; fall back to a mkdir spin-lock
  # on hosts without util-linux flock (macOS orchestrator). The subshell holds the lock for its body.
  (
    # Acquire the publish lock (flock fast-path, else PID-owned mkdir spin-lock). On timeout ABORT this
    # publish (exit 1 - counted as an issue) rather than pushing UNLOCKED. The release trap
    # only removes a lock THIS subshell owns.
    trap 'publish_lock_release' EXIT
    publish_lock_acquire "[$gw]" echo || exit 1
    # Stage ONLY this gateway's artifacts (never a sibling box's in-flight files):
    #   - its per-suite result JSONs (results/<suite>/<gw>.json)
    #   - its append-only history line (results/history/<gw>.jsonl)
    #   - its OOTB config sidecar (results/config/<gw>.txt)
    #   - any per-gateway chart the local regen produced for it (results/*<gw>*.png) - usually charts
    #     are regenerated field-wide at the very end, but staging a per-gw one here is harmless.
    local -a paths=()
    local f
    for f in "$HERE"/results/*/"$gw".json "$HERE"/results/history/"$gw".jsonl "$HERE"/results/config/"$gw".txt; do
      [ -e "$f" ] && paths+=("$f")
    done
    # The snapshot artifact is not named <gw>.json (matrix/run.sh writes
    # results/snapshots/result_<gw>_<measured_at>.json), so the `results/*/<gw>.json` glob above never
    # matches it; stage the per-gateway snapshots separately by their real filename shape.
    for f in "$HERE"/results/snapshots/result_"$gw"_*.json; do [ -e "$f" ] && paths+=("$f"); done
    for f in "$HERE"/results/*"$gw"*.png; do [ -e "$f" ] && paths+=("$f"); done
    if [ "${#paths[@]}" -eq 0 ]; then echo "[$gw] publish: no result files to commit (nothing pulled?)"; exit 0; fi
    # Do NOT swallow a staging failure: a failed `git add` (index.lock held, disk full,
    # perms) would otherwise leave an EMPTY stage, `diff --cached --quiet` would "skip commit", the
    # unchanged HEAD would push (trivially succeeds), and the gateway's row would silently NOT update
    # while the publish reported success. On a stage failure, log loudly and exit non-zero so the box's
    # return code counts it as a real publish issue.
    if ! git -C "$HERE" add -- "${paths[@]}"; then
      echo "[$gw] publish: git add FAILED (result staged nothing; NOT pushing an empty change) - aborting this publish"
      exit 1
    fi
    # Nothing actually changed vs HEAD (identical re-run) → skip the empty commit, still try a push in
    # case a prior push failed and left commits unpushed.
    if git -C "$HERE" diff --cached --quiet; then
      echo "[$gw] publish: no content change vs HEAD - skipping commit"
    else
      git -C "$HERE" commit -q -m "bench($gw): publish matrix run result

Incremental per-gateway publish: $gw's box finished cleanly, committing only
its result so the board updates just this row (matrix-sole-source)." \
        || { echo "[$gw] publish: git commit FAILED"; exit 1; }
      echo "[$gw] committed $gw's result"
    fi
    # Multiple boxes plus the render-charts bot move the remote ref constantly, so a bare push of our
    # stale HEAD is rejected non-fast-forward; fetch/rebase-then-push in a bounded retry loop (still
    # inside the flock so the whole fetch-rebase-push is serialized across boxes).
    #
    # PUSHED AS IT LANDS, not held to the end of the run.
    #
    # This used to commit here and push only in the final sweep, so nothing appeared until every box
    # had finished - a fourteen-gateway run went dark for its whole duration, and a run that died at
    # box twelve published nothing at all despite eleven gateways having been measured cleanly.
    #
    # The reason given for holding it was that a half-finished run would leave some rows fresh and
    # others stale. That is true and it is also what the board is built to show: every row carries its
    # own measured_at, gen-data ages each row against it, and app.js badges a stale one. A partial
    # board that says which rows are new is honest; a board that shows nothing for an hour is just
    # less useful.
    #
    # What actually made this unsafe was C8 - a board must not mix harness engines, and a run
    # spanning an engine change would have published a mixed board one row at a time. That guard is
    # in the publish path now (site/check-consistency.mjs) and it fails the BUILD rather than the
    # push, so a mixed board never reaches the site regardless of when rows are pushed.
    if push_with_rebase "[$gw]" echo; then
      echo "[$gw] published"
    else
      echo "[$gw] publish: push failed; the commit is local and the final sweep will retry it"
    fi
  )
}

# ═════════════════════════════════════════════════════════════════════════════════════════════════
# BOX QUALIFICATION: measure a new box before trusting it with a multi-hour run, and replace it if it
# fails. A bad box's absolute floor can sit inside the healthy population's range even while its own
# drift and peak throughput are badly off, so no static threshold on the absolute numbers alone can
# catch it; qualification instead compares the box against its own prior-run baseline.
#   stage 1  before the gateway is built or launched: the no-gateway floor probe (tens of seconds).
#   stage 2  after the gateway boots, before the 6x6: replay this gateway's own recorded peak cell.
# The measurement half runs on the box (matrix/qualify-box.sh); the verdict is decided here, because
# this is where the per-gateway baseline history lives (results/snapshots/) and because a suspect box
# must never be the thing that clears itself.
#
# On failure the box is terminated and a replacement is launched for that gateway alone, up to
# BENCH_QUALIFY_ATTEMPTS times. Every box this run launches, replacements included, is tagged
# run=$RUN_ID, so a replacement is torn down by the same RUN_ID filter and can never disturb a peer
# box or a concurrent invocation's fleet. If every attempt fails the gateway is not published: the
# honest failure is recorded and reported, mirroring how the promote guard refuses to overwrite good
# data with a boot failure.
# ═════════════════════════════════════════════════════════════════════════════════════════════════
BENCH_QUALIFY="${BENCH_QUALIFY:-1}"                       # 0 disables the whole gate (local/dry runs)
BENCH_QUALIFY_ATTEMPTS="${BENCH_QUALIFY_ATTEMPTS:-3}"     # boxes to try per gateway before giving up
BQ_RC_REPLACE=75                                          # bench_gateway_once: "this BOX is bad, retry"
QUALIFY_DIR="$HERE/results/box-qualify"; mkdir -p "$QUALIFY_DIR"
# Where a gateway that exhausted its box budget is recorded. bench_gateway runs in a background
# subshell, so a file is the only way the final summary can see it.
QUALIFY_SKIPPED="$QUALIFY_DIR/skipped-$RUN_ID.txt"; : > "$QUALIFY_SKIPPED"
# One line per gateway that produced a NEW snapshot in THIS run. Written by the boxes (which are
# subshells, so a variable could not carry it back) and read by the field-wide publish at the end.
# Empty means this run measured nothing anywhere, and nothing it derived may be pushed.
FRESH_SNAPSHOTS="$QUALIFY_DIR/measured-$RUN_ID.txt"; : > "$FRESH_SNAPSHOTS"

# _bq_json_field <file> <key> [subkey] -> the scalar, or EMPTY. Never fabricates: a missing key, a
# null, a non-scalar and an unparseable file all read as empty, which every gate treats as unmeasured.
_bq_json_field() {
  python3 - "$1" "$2" "${3:-}" <<'PY' 2>/dev/null
import json, sys
path, key, sub = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    with open(path) as fh:
        j = json.load(fh)
except Exception:
    sys.exit(0)
v = j.get(key)
if sub:
    v = (v or {}).get(sub) if isinstance(v, dict) else None
if isinstance(v, bool):
    sys.stdout.write("true" if v else "false")
elif isinstance(v, (int, float)):
    sys.stdout.write(("%d" % v) if float(v).is_integer() else ("%.10g" % v))
elif isinstance(v, str):
    sys.stdout.write(v)
PY
}

# qualify_box <gw> <ip> <glog> <log_fn> <attempt>
#   0  the box is qualified (or the gate is disabled / the fault is the gateway's, not the box's)
#   1  the BOX is bad - terminate it and launch a replacement
# Writes the qualification provenance onto the box either way it proceeds, so the snapshot records the
# instrument's state (lib/rig.sh _rig_box_qualify_json folds it in; matrix/run.sh needs no change).
qualify_box() {
  # Box qualification is performed by the engine: `otb run` qualifies the box itself, before the grid,
  # against the median of the same observation from this box's previous runs, and publishes the
  # verdict as rig.box_qualify inside the snapshot. This stays a function so the retry/replace-the-box
  # machinery around it keeps its shape for when a rejecting gate is wired back to the engine's verdict.
  local gw="$1" ip="$2" glog="$3" _log="$4" attempt="$5"
  "$_log" "qualify: performed by the engine (published as rig.box_qualify in the snapshot)"
  return 0
}

# ── one gateway, up to BENCH_QUALIFY_ATTEMPTS boxes ───────────────────────────────────────────────
# The replacement loop. bench_gateway_once holds exactly one box for its whole lifetime and terminates
# it on return (its RETURN trap), so "launch a replacement" is simply calling it again - the new box
# gets a fresh instance id under the same run=$RUN_ID tag and no peer box is touched.
bench_gateway() {
  local gw="$1" attempt rc
  local glog="$HERE/results/fanout-$gw.log"
  # One log per gateway run holds everything: glog_echo writes the orchestrator narration here and the
  # box's own .run.log is appended before teardown. This subshell also runs with stderr teed into the
  # same file, so bash's own errors (a quoting mistake, an unbound variable, a failed command) land
  # beside the narration instead of going only to the orchestrator's terminal.
  exec 2> >(tee -a "$glog" >&2)
  for attempt in $(seq 1 "$BENCH_QUALIFY_ATTEMPTS"); do
    bench_gateway_once "$gw" "$attempt"; rc=$?
    [ "$rc" = "$BQ_RC_REPLACE" ] || return "$rc"
    if [ "$attempt" -lt "$BENCH_QUALIFY_ATTEMPTS" ]; then
      echo "[$(date +%H:%M:%S)] [$gw] box FAILED qualification on attempt $attempt/$BENCH_QUALIFY_ATTEMPTS - terminated it, launching a replacement box" | tee -a "$glog"
    fi
  done
  # Budget exhausted. Publishing anything now would mean publishing a number measured on hardware we
  # have positively identified as bad - the exact thing this gate exists to prevent. Record the honest
  # failure; the committed result for this gateway stays whatever it was, untouched and unrefreshed.
  echo "[$(date +%H:%M:%S)] [$gw] SKIPPED - $BENCH_QUALIFY_ATTEMPTS boxes in a row failed box qualification; NOT publishing $gw this run (its committed result is unchanged, not overwritten). See the qualify lines above for the measured drift." | tee -a "$glog"
  echo "$gw: $BENCH_QUALIFY_ATTEMPTS/$BENCH_QUALIFY_ATTEMPTS boxes failed qualification - not published" >> "$QUALIFY_SKIPPED"
  return 1
}

# Liveness probe with retry: a single dropped/timed-out ssh (packet loss, momentarily overloaded
# sshd) on a healthy box must not be read as proof the box is gone - it must be as resilient as
# every other network op in this file. Mirrors the box-ready wait's retry shape (line ~563:
# `ssh $SSHOPT ubuntu@"$ip" true`, retried), not a new mechanism. Self-contained: only SSHOPT (global)
# and its ip argument, so it can be sourced and tested in isolation like publish_lock_acquire/_release.
box_reachable() { # ip
  local ip="$1" try
  for try in 1 2 3; do
    ssh $SSHOPT ubuntu@"$ip" true 2>/dev/null && return 0
    sleep 5
  done
  return 1
}

# ── one box, one gateway (runs in the background, self-terminates) ─────────────────────────────────
bench_gateway_once() {
  local gw="$1" attempt="${2:-1}" iid="" ip=""
  local tag="gateway-bench-$gw"
  local glog="$HERE/results/fanout-$gw.log"
  [ "$attempt" = 1 ] && : > "$glog"
  glog_echo(){ echo "[$(date +%H:%M:%S)] [$gw] $*" | tee -a "$glog"; }

  # provision. COST SAFETY NET: the box self-terminates after BENCH_MAX_MIN minutes no matter what, so
  # even if this orchestrator is killed (its RETURN-trap never fires), the box shuts itself down and
  # `instance-initiated-shutdown-behavior=terminate` makes that a terminate, not a stop. A leaked box can
  # therefore bleed cost for at most BENCH_MAX_MIN, never indefinitely. BENCH_MAX_MIN is set equal to the
  # matrix suite's own 480-min ceiling (see the BENCH_MAX_MIN block at the top of this file for why they
  # are deliberately equal, and which of the two fires first on a wedged box).
  iid=$(aws ec2 run-instances --image-id "$AMI" --instance-type "$ITYPE" --key-name "$KEYNAME" \
    --security-group-ids "$SG" \
    --instance-initiated-shutdown-behavior terminate \
    --user-data "$(printf '#!/bin/bash\nshutdown -h +%s\n' "$BENCH_MAX_MIN")" \
    --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=60,VolumeType=gp3,DeleteOnTermination=true}' \
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$tag},{Key=purpose,Value=gateway-bench},{Key=run,Value=$RUN_ID}]" \
    --query 'Instances[0].InstanceId' --output text 2>>"$glog") || { glog_echo "run-instances FAILED: $(tail -1 "$glog" | sed 's/.*: //' | cut -c1-140)"; return 1; }
  glog_echo "launched $iid"
  # self-terminate this box no matter how we exit
  trap 'aws ec2 terminate-instances --instance-ids "'"$iid"'" >/dev/null 2>&1 || true' RETURN

  aws ec2 wait instance-running --instance-ids "$iid" 2>>"$glog" || { glog_echo "wait running FAILED"; return 1; }
  ip=$(aws ec2 describe-instances --instance-ids "$iid" --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
  glog_echo "ip=$ip - waiting for ssh"
  local ok=0; for _ in $(seq 1 40); do ssh $SSHOPT ubuntu@"$ip" true 2>/dev/null && { ok=1; break; } || sleep 8; done
  [[ $ok == 1 ]] || { glog_echo "ssh never came up"; return 1; }

  glog_echo "installing deps (bare base: docker + psutil; the rig is a prebuilt download, and each"
  glog_echo "gateway installs its OWN prereqs via gw_prereqs - no blanket build toolchain on every box)"
  ssh $SSHOPT ubuntu@"$ip" 'set -e
    # WAIT FOR THE BOX TO FINISH BOOTING BEFORE ASKING IT FOR ROOT.
    #
    # sshd accepts connections before cloud-init has finished writing the sudoers drop-in that gives
    # `ubuntu` passwordless sudo. `sudo -n` does not wait - it fails immediately - so a box that is
    # still provisioning loses every apt step, ends up with no docker, and then reports each gateway
    # as "failed to run docker: No such file or directory", which reads as a broken ENTRANT rather
    # than a box that never finished coming up. It is a race, so it takes a random subset of the
    # field each run: one of fourteen lost it on 2026-07-27.
    #
    # Same class as the un-retried ssh liveness probe (58293c3): a one-shot operation against a
    # machine that is still settling, treated as though its first answer were final.
    cloud-init status --wait >/dev/null 2>&1 || true
    # Belt and braces: cloud-init may be absent or already done, so confirm sudo actually works
    # before relying on it, rather than discovering it three commands later.
    for _ in 1 2 3 4 5 6 7 8 9 10; do sudo -n true 2>/dev/null && break; sleep 3; done
    sudo -n true 2>/dev/null || { echo "PROVISION FAILED: passwordless sudo never became available"; exit 1; }
    # apt itself is contended at boot (unattended-upgrades holds the dpkg lock), so retry rather
    # than fail the whole box on a lock we only had to wait for.
    for _ in 1 2 3; do sudo -n apt-get update -q && break; sleep 10; done
    # BARE base only: docker (for the image gateways), curl (fetch the prebuilt rig), jq, and python3
    # + psutil (the memory suite reads RSS). NO build-essential/rust/go/node here - the mock+loadgen
    # are prebuilt binaries pulled from the rig release, and the 2 source-built gateways pull their
    # own toolchain via gw_prereqs() on their box ALONE. Docker-image gateways are up in ~2 min.
    # RETRIED, for the same reason `apt-get update` above is: the install contends with
    # unattended-upgrades for the dpkg lock, and a single attempt turns a lock we only had to wait for
    # into a lost box. Without `set -e` tripping on the first two tries.
    for _ in 1 2 3; do
      sudo -n DEBIAN_FRONTEND=noninteractive apt-get install -y -q docker.io curl ca-certificates jq python3-pip git build-essential && break
      sleep 10
    done
    # A box that came up without docker cannot measure most entrants, and every launch on it fails
    # with \"failed to run docker: No such file or directory\", which reads as a broken gateway rather
    # than a box that never finished provisioning. Better to lose the box here than to publish its
    # verdicts.
    #
    # RECOVER BEFORE GIVING UP. If the binary is absent after three attempts, the dpkg state is the
    # usual reason (a half-finished install from unattended-upgrades leaves the lock released but the
    # package unconfigured, and further `apt-get install` calls then no-op while returning 0 - which is
    # how a box reaches the measurement phase with no docker and a provisioning step that reported
    # success). `--fix-broken` and `dpkg --configure -a` are the two repairs that address exactly that,
    # so try them once rather than losing the box to a condition that is routinely fixable.
    if ! command -v docker >/dev/null; then
      echo "docker missing after install; attempting dpkg repair"
      sudo -n dpkg --configure -a >/dev/null 2>&1 || true
      sudo -n DEBIAN_FRONTEND=noninteractive apt-get install -y -q --fix-broken >/dev/null 2>&1 || true
      sudo -n DEBIAN_FRONTEND=noninteractive apt-get install -y -q --reinstall docker.io >/dev/null 2>&1 || true
    fi
    command -v docker >/dev/null || { echo "PROVISION FAILED: docker did not install on this box (after retry and dpkg repair)"; exit 1; }
    # The engine is BUILT ON THE BOX from the cloned commit, once, before any measurement starts.
    # It is not shipped from the orchestrator: a binary from a laptop has no provenance, and the
    # whole point of cloning a revision is that the thing doing the measuring came from it too.
    # Installed here, at provision time, so no toolchain work happens inside a measurement window -
    # the same reason the source-built gateways build before the memory baseline is taken.
    command -v cargo >/dev/null || (curl -sSf https://sh.rustup.rs | sh -s -- -y -q >/dev/null 2>&1)
    sudo usermod -aG docker ubuntu || true
    # FAIRNESS: a container inherits the docker DAEMON fd limit, NOT the host-shell ulimit. Left at
    # the ~1024 default, a containerised gateway fast enough to hold >1024 concurrent connections
    # hits EMFILE and collapses at exactly c=1024. The native side (loadgen, mock, native gateways)
    # is raised to match in the remote run script below - it used to be perf/run.sh's job, and when
    # that was retired the raise went with it while this comment kept citing it.
    echo "{ \"default-ulimits\": { \"nofile\": { \"Name\": \"nofile\", \"Hard\": 1048576, \"Soft\": 1048576 } } }" | sudo tee /etc/docker/daemon.json >/dev/null
    sudo systemctl restart docker || sudo service docker restart || true
    # THE BINARY EXISTING IS NOT EVIDENCE THE DAEMON WILL ANSWER, and it was the daemon that was
    # missing when this last bit.
    #
    # litellm-python lost its box on 2026-07-30 to `chmod: cannot access /var/run/docker.sock` followed
    # by `failed to run docker: No such file or directory`, ten seconds into its run - AFTER the
    # `command -v docker` guard above had passed. That guard proves a file is on PATH; it proves
    # nothing about whether dockerd is up and accepting connections on its socket. `systemctl restart
    # docker` is asynchronous and the `|| true` on it means even an outright failure to restart is
    # swallowed, so provisioning could report success while the socket never appeared.
    #
    # So wait for the thing actually needed - a daemon that answers - and fail the box here if it never
    # does. Losing a box during provisioning costs one re-run; publishing INCOMPLETE for a gateway
    # whose box had no docker reads as a broken ENTRANT, which is a false statement about somebody
    # else's software.
    # And it RECOVERS rather than only detecting: three rounds of (wait 30s for the socket, then try a
    # different way of starting it). `systemctl enable --now` is included because a daemon that is
    # installed but not enabled comes back dead after any restart, and `service` covers a box where
    # systemd is not the thing managing it. Only after all three rounds fail is the box lost - the
    # earlier version failed on the first 30s timeout, which would have turned a slow-starting daemon
    # into a discarded gateway.
    _dockok=0
    for _round in 1 2 3; do
      for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
        if sudo -n docker info >/dev/null 2>&1; then _dockok=1; break; fi
        sleep 2
      done
      [ "$_dockok" = 1 ] && break
      echo "docker daemon not answering (round $_round); attempting to start it"
      sudo -n systemctl enable --now docker >/dev/null 2>&1 \
        || sudo -n systemctl restart docker >/dev/null 2>&1 \
        || sudo -n service docker restart >/dev/null 2>&1 || true
    done
    if [ "$_dockok" != 1 ]; then
      # The box is lost either way, so spend a second saying WHY - a bare "the daemon never answered"
      # sends the next person to ssh into a machine that has already been terminated.
      echo "PROVISION FAILED: docker installed but the daemon never answered on /var/run/docker.sock after 3 rounds (~90s)"
      echo "  docker binary : $(command -v docker || echo MISSING)"
      echo "  systemctl     : $(sudo -n systemctl is-active docker 2>&1 | head -1)"
      echo "  socket        : $(ls -l /var/run/docker.sock 2>&1 | head -1)"
      echo "  last journal  : $(sudo -n journalctl -u docker -n 5 --no-pager 2>&1 | tail -5)"
      exit 1
    fi
    python3 -m pip install --user -q --break-system-packages psutil 2>/dev/null || pip3 install -q psutil || true' >>"$glog" 2>&1
  local prov_rc=$?
  # A FAILED PROVISION MUST END THE BOX, not be discovered as a broken gateway later.
  #
  # This exit code was previously ignored. The remote block runs under `set -e`, so a failed apt
  # aborts it before the `command -v docker` guard above can fire - and the run then continued onto a
  # box with no docker, spent minutes on it, and published INCOMPLETE with "never became ready",
  # which points at the entrant instead of at the box. Losing the box here is the honest outcome:
  # nothing was measured, so nothing about the gateway was learned.
  #
  # Captured on the line immediately after the ssh: anything between them could overwrite $?.
  if [ "$prov_rc" -ne 0 ]; then
    glog_echo "PROVISION FAILED (deps did not install; see above) - tearing down this box rather than measuring on it"
    return 1
  fi

  # Ship ONLY the harness (scripts + configs, a few MB). Exclude every build/runtime artifact: the box
  # fetches the rig binaries from the release (lib/rig.sh) and builds its own gateway (docker pull, or
  # gw_build for source-built gateways). A stray local venv or bin/ must never be uploaded to every box.
  # Log the payload size + transfer time so a slow rsync is never a silent hang. GNU `du --exclude` is
  # rejected by the BSD `du` on the darwin orchestrator (always logs "?"), so the size is derived from a
  # local rsync dry run with the same excludes the real transfer uses (below): portable, no network, and
  # exactly the bytes about to ship. `--stats` prints "Total file size: N bytes"; humanise it. Dedicated
  # per-gateway sizecheck dst under a mktemp -d, removed right after we read --stats.
  #
  # THE BOX FETCHES THE FILES IT NEEDS, AT AN EXACT COMMIT - not a copy of this laptop's tree, and
  # not the whole repository.
  #
  # Provenance first: every URL below names the pinned SHA, so a published number can always be
  # traced to the revision that produced it. An rsync of the orchestrator's working tree could not
  # be, which is why that was abandoned.
  #
  # But the tarball that replaced it downloads the ENTIRE repository to keep two directories. This
  # tree carries ~46 MB of results/ - charts, reports, snapshots - and a box needs about 44 KB: its
  # OWN gateway directory and lib/rig.sh. Every box paid for all of it, fourteen times a run, to
  # extract a thousandth of it. It also downloaded the other thirteen gateways' manifests, which a
  # box has no business holding.
  #
  # The file list comes from `git ls-tree` against BENCH_COMMIT - the commit itself, not the working
  # tree - so a file staged locally but not committed cannot reach a box, and each raw URL is pinned
  # to the same SHA, so a path that is not in that commit 404s rather than silently arriving from
  # some other revision.
  glog_echo "fetching $gw + rig.sh @ ${BENCH_COMMIT:0:12} ..."; local _t0=$SECONDS
  local _files
  # MODE AND PATH, not path alone. A raw fetch writes whatever the umask says, and the executable
  # bit is part of what the commit records: the three source-built entrants ship a build.sh the
  # launcher runs directly, and fetching it 0644 kills the run with "build script ... is not
  # executable". The tarball this replaced preserved modes for free, so dropping to a mode-blind
  # copy lost them silently - it took out exactly the three native gateways and nothing else.
  _files="$(git -C "$HERE" ls-tree -r "$BENCH_COMMIT" -- "gateways/$gw" lib/rig.sh 2>/dev/null | awk '{print $1"\t"$4}')"
  if [ -z "$_files" ]; then
    glog_echo "FETCH FAILED: commit ${BENCH_COMMIT:0:12} contains no gateways/$gw - refusing to measure"
    return 1
  fi
  local _raw="https://raw.githubusercontent.com/${BENCH_REPO#https://github.com/}"
  _raw="${_raw%.git}/$BENCH_COMMIT"
  ssh $SSHOPT ubuntu@"$ip" "set -e
    rm -rf ~/benchmarking
    mkdir -p ~/benchmarking
    cd ~/benchmarking
    while IFS=\$'\t' read -r mode f; do
      [ -n \"\$f\" ] || continue
      mkdir -p \"\$(dirname \"\$f\")\"
      # -f so a path missing from this commit is an error, never a silently empty file the run would
      # then try to measure with.
      curl -fsSL -o \"\$f\" '$_raw/'\"\$f\"
      # Restore the mode the commit recorded. 100755 is the only executable mode git stores.
      [ \"\$mode\" = 100755 ] && chmod +x \"\$f\"
    done <<'FILELIST'
$_files
FILELIST
    # Refuse to measure if the fetch did not produce what the run needs.
    test -d 'gateways/$gw' && test -r lib/rig.sh
    # results/ on a box is how a previous run's numbers got recycled; nothing above can create it.
    test ! -e results
  " >>"$glog" 2>&1
  local _up_rc=$?
  if [ "$_up_rc" -ne 0 ]; then
    glog_echo "FETCH FAILED (rc=$_up_rc) - harness incomplete; refusing to measure a partial tree, tearing down this box"
    return 1
  fi
  local _n; _n="$(printf '%s\n' "$_files" | grep -c .)"
  glog_echo "fetched $_n file(s) in $((SECONDS-_t0))s"

  # ── BOX QUALIFICATION, right here: the harness is on the box, nothing has been built or launched
  # yet, and the 6x6 has not started. This is the last moment at which rejecting the box is cheap.
  if [ "$BENCH_QUALIFY" = 1 ]; then
    if ! qualify_box "$gw" "$ip" "$glog" glog_echo "$attempt"; then
      return "$BQ_RC_REPLACE"          # RETURN trap terminates this box; the caller launches a replacement
    fi
  else
    glog_echo "BENCH_QUALIFY=0 - box qualification SKIPPED; this run has no evidence that the hardware it measured on was sound"
  fi

  # ── per-suite staged pull + promote guard, factored out so it can run INCREMENTALLY during the run
  # AND once more at the end. Idempotent: pulls results/<suite>/<gw>.json to a staging
  # file and lets the promote guard decide. Sets three caller-scope maps by suite: _pull_state (unset |
  # ok | stale | missing) and _pull_rc. Returns 0 when a fresh result was promoted (so the incremental
  # loop can stop re-pulling a suite it already captured); 1 when there is nothing (new) to promote.
  #   0 = promoted a fresh result   1 = no fresh result this call (missing/guard-held/transient)
  pull_suite() { # suite
    local suite="$1"
    mkdir -p "$HERE/results/$suite"
    local staged="$HERE/results/$suite/.incoming-$gw.json"
    # RETRY the rsync: a dropped SSH/rsync ("unexpected end of file") must NOT silently leave stale
    # data behind. Try up to 4 times with a pause; rsync 23 = remote file genuinely absent (the suite
    # has not produced a result YET, or produced none), which we treat as "nothing to pull", NOT a
    # transient to retry forever.
    rm -f "$staged"; local ok=0 attempt rc
    for attempt in 1 2 3 4; do
      rsync -az --timeout=60 -e "ssh $SSHOPT" "ubuntu@$ip:~/benchmarking/results/$suite/$gw.json" "$staged" >>"$glog" 2>&1
      rc=$?
      if [[ $rc -eq 0 && -f "$staged" ]]; then ok=1; break; fi
      if [[ $rc -eq 23 ]]; then break; fi   # remote file missing: no result yet, do not retry
      glog_echo "rsync $suite/$gw.json attempt $attempt failed (rc=$rc) - retrying in 10s"
      sleep 10
    done
    _pull_rc[$suite]=$rc
    if [[ $ok -eq 1 ]]; then
      # Pull to a staging file, then let the promote guard decide. BULLETPROOF: a boot/build failure
      # (status 000, "failed to boot", missing entrypoint) must NEVER overwrite a committed served
      # result. The guard keeps the good data and logs loudly; a real result promotes normally.
      if python3 "$HERE/lib/promote_guard.py" "$suite" "$HERE/results/$suite/$gw.json" "$staged" >>"$glog" 2>&1; then
        mv -f "$staged" "$HERE/results/$suite/$gw.json"; _pull_state[$suite]=ok
        glog_echo "pulled $suite/$gw.json"; return 0
      else
        glog_echo "GUARD kept prior $suite/$gw.json (incoming was a boot/build failure)"; rm -f "$staged"
        _pull_state[$suite]=stale; return 1
      fi
    else
      rm -f "$staged"; _pull_state[$suite]=missing; return 1
    fi
  }

  # The OOTB config sidecar is written on the box by harness_write_config to
  # ~/benchmarking/results/config/<gw>.txt (lib/harness.sh:74-77); pull it alongside the suite JSONs,
  # since publish_gateway stages it separately from them. rc=23 (remote absent: a gateway with no
  # gw_config hook writes no sidecar) is treated as "no config", exactly like a missing suite JSON, not
  # an error, not retried forever.
  pull_config() {
    mkdir -p "$HERE/results/config"
    local dest="$HERE/results/config/$gw.txt" staged="$HERE/results/config/.incoming-$gw.txt"
    rm -f "$staged"; local attempt rc
    for attempt in 1 2 3 4; do
      rsync -az --timeout=60 -e "ssh $SSHOPT" "ubuntu@$ip:~/benchmarking/results/config/$gw.txt" "$staged" >>"$glog" 2>&1
      rc=$?
      if [[ $rc -eq 0 && -f "$staged" ]]; then mv -f "$staged" "$dest"; glog_echo "pulled config/$gw.txt"; return 0; fi
      if [[ $rc -eq 23 ]]; then rm -f "$staged"; return 1; fi   # no sidecar on the box (no gw_config hook)
      glog_echo "rsync config/$gw.txt attempt $attempt failed (rc=$rc) - retrying in 10s"
      sleep 10
    done
    rm -f "$staged"; return 1
  }

  # The snapshot artifacts are written on the box at
  # ~/benchmarking/results/snapshots/result_<gw>_<measured_at>.json, a different path/filename shape than
  # the per-suite results, so they need their own pull. Every run's snapshot is kept, never overwritten.
  # rc=23 (no snapshot on the box, e.g. MATRIX_MEMORY=0 or an aborted run) is "nothing to pull", not an
  # error, exactly like a missing suite JSON.
  pull_snapshots() {
    mkdir -p "$HERE/results/snapshots"
    local attempt rc before after
    before=$(ls -1 "$HERE"/results/snapshots/result_"$gw"_*.json 2>/dev/null | wc -l | tr -d ' ')
    for attempt in 1 2 3 4; do
      # Filter with rsync's OWN --include/--exclude rather than a wildcard in the remote path: a remote
      # glob only works if the remote shell expands it, which rsync's --protect-args (default-on in some
      # builds) suppresses. The filter is evaluated by rsync itself, so it behaves identically either
      # way, and it can never pull a sibling gateway's snapshot into this box's publish.
      rsync -az --timeout=60 -e "ssh $SSHOPT" \
        --include="result_${gw}_*.json" --exclude='*' \
        "ubuntu@$ip:~/benchmarking/results/snapshots/" "$HERE/results/snapshots/" >>"$glog" 2>&1
      rc=$?
      if [[ $rc -eq 0 ]]; then
        after=$(ls -1 "$HERE"/results/snapshots/result_"$gw"_*.json 2>/dev/null | wc -l | tr -d ' ')
        if [[ "${after:-0}" -gt "${before:-0}" ]]; then
          glog_echo "pulled snapshots/result_${gw}_*.json ($(( after - before )) new, ${after} on disk)"; return 0
        fi
        glog_echo "no NEW snapshot artifact on the box for $gw (${after} already on disk)"; return 1
      fi
      if [[ $rc -eq 23 ]]; then glog_echo "no results/snapshots/ on the box for $gw (nothing to pull)"; return 1; fi
      glog_echo "rsync snapshots/result_${gw}_*.json attempt $attempt failed (rc=$rc) - retrying in 10s"
      sleep 10
    done
    return 1
  }

  # The snapshot is the artifact; there are no per-suite JSONs any more. The engine writes only
  # results/snapshots/<gw>.json and the timestamped result_<gw>_<measured_at>.json. No legacy suite is
  # pulled by default, and freshness is judged on the snapshot alone (below). An explicit SUITES=... still
  # drives the old per-suite pull for an ad-hoc re-run of a retired suite.
  local ALL_SUITES="${SUITES:-}"
  declare -A _pull_state=() _pull_rc=(); local suite
  for suite in $ALL_SUITES; do _pull_state[$suite]=unset; _pull_rc[$suite]=0; done

  glog_echo "running $gw (latency + RPS + memory) - detached on box; pulling each suite as it completes"
  # Launch run-all.sh detached on the box (setsid + nohup) writing a sentinel with its real exit code on
  # completion, instead of a single blocking ssh: a blocking ssh only returns when run-all.sh finishes,
  # so a box that self-terminates mid-run would forfeit every already-written suite JSON, not just the
  # in-flight one. Detaching lets us stream each suite's result off-box as run-all.sh writes it, so a
  # late box death loses at most the running suite.
  #
  # The remote script is uploaded as a quoted heredoc, not interpolated into the ssh command line:
  # nothing in it expands locally, ever. The orchestrator's values are prepended as a printf %q export
  # preamble, and the finished script is written to the box and then launched detached. This matters
  # because an unquoted body would let the ORCHESTRATOR's own shell expand quotes, backticks, and
  # variables like $PWD inside the remote script before the box ever saw it.
  local _run_sh
  _run_sh="$(
    printf 'export BENCH_HARDWARE=%q BENCH_ARCH=%q\n' "$HW_LABEL" "$ARCH"
    printf 'export BENCH_ENGINE_COMMIT=%q BENCH_ENGINE_DIRTY=%q\n' "$BENCH_ENGINE_COMMIT" "$BENCH_ENGINE_DIRTY"
    # THE BOX GETS WHAT IS WRITTEN HERE, and nothing else. `export` in this orchestrator sets a
    # variable in THIS shell; the box runs a different shell on a different machine, so a value that
    # is not printed into this script simply does not exist there.
    #
    # That is how the box qualification stayed inert after being "fixed": the baseline was computed,
    # exported locally, logged locally, and never shipped - so the engine read no baseline, seeded,
    # and reported outcome=seed with samples=0 exactly as it always had. The orchestrator's own log
    # line said 497862 while the snapshot said null, which is the kind of disagreement that makes a
    # guard look alive when it is not.
    printf 'export OTB_QUALIFY_BASELINE=%q\n' "${OTB_QUALIFY_BASELINE:-}"
    printf 'export SUITES=%q\n' "$ALL_SUITES"
    # Narrow the grid for HARNESS iteration. The grid is dialects x dialects, so one dialect is one
    # cell and a debugging run costs seconds instead of the full 6x6. Unset for a real run, which is
    # what a field measurement always uses.
    printf 'export OTB_DIALECTS=%q OTB_MIN_CONC=%q OTB_MAX_CONC=%q\n' \
      "${OTB_DIALECTS:-}" "${OTB_MIN_CONC:-}" "${OTB_MAX_CONC:-}"
    # gw is the orchestrator's loop variable and is not otherwise exported to the box, so it must be
    # written literally here (not `\$gw`, which would reach the box as an unset name).
    printf 'gw=%q\n' "$gw"
    cat <<'REMOTE'
# Every relative path below is relative to the repo, so anchor it rather than inheriting a cwd from
# the login shell that starts this script.
cd ~/benchmarking || exit 1
# THE MEASURING INSTRUMENT NEEDS AS MANY SOCKETS AS THE THING IT MEASURES.
#
# The load generator opens ONE connection per unit of concurrency, so a sweep to c=4096 needs 4096
# file descriptors in THIS process. Ubuntu's default soft limit is 1024, and the hard limit is
# 1048576 - so the cap is ours to lift and costs nothing.
#
# Left unlifted this is not a slow measurement, it is a WRONG one: every connection past ~1020 fails
# instantly with EMFILE, the generator counts those as failed requests, and the failure is
# attributed to the gateway. A whole field run showed all ten gateways clean at c=512 and failing at
# exactly c=1024 - ten unrelated projects in Go, Rust, Python and Lua do not share a ceiling, and
# that number is the default this line raises. Every sustained@20ms figure in that run was our own
# fd limit wearing the gateway's name.
#
# The unfairness was the sharpest part: the docker daemon config above already grants CONTAINERS
# 1048576, so the gateways under test had a thousand times the sockets of the harness measuring them.
# This restores what perf/run.sh used to do for the native side before it was retired in the Rust
# rewrite (commit d7fc1f4), which removed the raise and left the comment describing it.
ulimit -n 1048576 2>/dev/null || ulimit -n "$(ulimit -Hn)" 2>/dev/null || true
echo "[rig] loadgen/mock fd limit: $(ulimit -Sn) (hard $(ulimit -Hn))"

# EPHEMERAL PORTS ARE THE REAL CONCURRENCY CEILING, so widen them deliberately rather than inherit
# a default nobody chose.
#
# A TCP connection needs a unique (src ip, src port, dst ip, dst port). Every load window drives ONE
# destination, so simultaneous connections cannot exceed this host's ephemeral source ports. Stock
# Linux gives 32768-60999, about 28,000 - below what a fast gateway can be driven to, and the moment
# it is reached `connect` returns EADDRNOTAVAIL, which the generator used to count as the gateway
# refusing. Raising fd limits alone never helped: descriptors were never the binding constraint.
#
# 16384 as the floor because every port this rig binds is below it (mock 8000; gateways 3000, 8080,
# 8101, 8102, 8787, 9080, 12000; plano's envoy internals up to 12001) - an ephemeral range reaching
# down into those could steal a port before the service binds it, which would look like a gateway
# that failed to start. The engine derives its search ceiling from whatever this ends up being
# (`run::host_connection_ceiling`), so this is the only place the number is decided.
#
# tcp_tw_reuse because a closed connection holds its port through TIME_WAIT: without recycling, a
# window that cycles connections exhausts the range well below its size. This is the safe direction
# of that knob - it permits reuse for OUTBOUND connections only.
sudo sysctl -w net.ipv4.ip_local_port_range="16384 65535" >/dev/null 2>&1 || true
sudo sysctl -w net.ipv4.tcp_tw_reuse=1 >/dev/null 2>&1 || true
echo "[rig] ephemeral ports: $(cat /proc/sys/net/ipv4/ip_local_port_range 2>/dev/null || echo unknown) (tw_reuse=$(cat /proc/sys/net/ipv4/tcp_tw_reuse 2>/dev/null || echo unknown))"
# The checkout is sparse (gateways + lib only), so results/ does not exist here and the snapshot
# writer does not create its own output directory - it reports an error and writes nothing.
mkdir -p results/snapshots
source ~/.cargo/env 2>/dev/null || true
export CORES=0-3 LOADCORES=4-9 MOCKCORES=10-15
export CAP_MIB=24000
sudo -n true 2>/dev/null && sudo chmod 666 /var/run/docker.sock || true
# PULL THE RIG. Nothing is built here.
#
# The engine, the mock and the load generator are all prebuilt by CI for this arch and published to
# the rolling rig release, so a bench box is a bare OS plus docker. That is what makes every box
# identical, and it is why the box installs no toolchain: a build on the box is a difference between
# boxes, and every difference between boxes is a difference in the numbers.
#
# The only thing that ever builds here is a gateway with no official artifact for this arch, and
# that is a matter for the gateway itself, done before the memory baseline is taken.
source lib/rig.sh
fetch_rig "$PWD" || { echo rig fetch FAILED; echo 126 > .run-done; exit 0; }
# Record which mock this run used. rig is a moving tag, so the same URL can serve different binaries
# over time; rig.sh has just fetched it and can hash it, and the engine cannot work that out for itself,
# so hand it over the same way the commit is handed over. Empty stays empty: the engine publishes an
# absent block rather than inventing one.
export OTB_RIG_MOCK_ORIGIN="$RIG_MOCK_ORIGIN"
export OTB_RIG_MOCK_SHA256="$(_rig_sha256 "$MOCK")"
export OTB_RIG_MOCK_UPDATED_AT="$(_rig_asset_updated_at "mock-$BENCH_ARCH")"
export OTB_RIG_URL="$RIG_URL"
# rig.sh puts what it fetches in bin/; the run invokes ./otb. RIG_URL comes from rig.sh, so the
# engine is fetched from the same release the rest of the rig came from, named once.
#
# Use BENCH_ARCH, not ARCH: ARCH belongs to the orchestrator and is not exported to the box, so it
# would be unset here and the URL would end in "otb-" (404).
curl -fsSL -o ./otb "$RIG_URL/otb-$BENCH_ARCH" && chmod +x ./otb
if [ ! -x ./otb ]; then
  echo "engine binary not fetched: otb-$BENCH_ARCH missing from the rig release"
  echo 126 > .run-done
  exit 0
fi
# THE MOCK RUNS PINNED, IN ITS OWN PROCESS, on its own cores. The three-way split - gateway 0-3,
# load generator 4-9, mock 10-15 - IS the comparability basis of every published number, so a mock
# sharing cores with either of the others measures a different machine.
pkill -f "bin/mock-$BENCH_ARCH" 2>/dev/null; sleep 1
# The mock answers all six dialects by path, so a gateway that forwards the client's ingress request
# VERBATIM still gets a plausible 200 back and would be scored as having a translation it does not
# have. Recording is what makes that detectable: the mock keeps, per dialect, whether the body it
# actually received matched that dialect's request shape, and the runner reads it off /__mock/state.
# Without this the reverification is dead code and every capability verdict is a status-code guess.
#
# RECORDING IS NOT TURNED ON HERE. The mock boots quiet and the engine turns recording on around its
# one re-verification request per cell, then off again (POST /__mock/record). That is deliberate:
# this mock's own throughput is the reference every gateway's number is judged against, and a result
# within 10% of it is SUPPRESSED as mock-bound. A recorded request takes a process-wide lock, so
# leaving recording on for the millions of requests in the throughput and memory windows would slow
# the reference instrument and quietly convert real gateway measurements into suppressed ones. The
# mock is also the least-touched, most-trusted code in the harness; it stays in exactly the state
# every previously published number was taken against, and pays the recording cost once per cell.
setsid taskset -c $MOCKCORES ./bin/mock-$BENCH_ARCH --port 8000 </dev/null >mock.log 2>&1 &
# Give it a moment to bind, then refuse to measure anything if it did not: every not-served verdict
# is conditioned on the mock being up, so a run against a dead mock publishes rig failures as
# gateway capability denials.
for i in $(seq 1 30); do
  curl -s -m2 -o /dev/null -X POST 127.0.0.1:8000/v1/chat/completions \
    -H "content-type: application/json" -d "{}" && break
  sleep 1
done
# Refuse the run outright if the setup is wrong, rather than discovering it as a gateway that will
# not boot after the box-hours are already spent.
./otb validate "gateways/$gw" || { echo 127 > .run-done; exit 0; }
OTB_GW_CORES=$CORES LOADCORES=$LOADCORES \
  ./otb run "gateways/$gw" 127.0.0.1:8000 results/snapshots
echo $? > .run-done

# A FINISHED BOX MUST NOT BE DESTROYED BY A TIMER SIZED FOR THE WORK.
#
# The boot-time backstop is `shutdown -h +BENCH_MAX_MIN`, and BENCH_MAX_MIN is deliberately EQUAL to
# the suite's own ceiling. That is right for a wedged box and wrong for a finished one: a gateway that
# uses most of its budget crosses the line with almost no margin left, and the root volume is
# DeleteOnTermination=true, so when the timer fires AWS deletes the disk carrying results nobody pulled.
#
# That is not hypothetical. On 2026-07-29 four gateways completed full 36-cell runs and sat unharvested
# because the orchestrator process had stopped; they were found by hand. Had they not been, the timer
# would have destroyed all four finished runs.
#
# So the moment the work is done, the deadline stops being about the work and starts being about the
# harvest: cancel the boot timer and re-arm a bounded window. Cost stays capped either way, and a dead
# orchestrator now has a known amount of time to come back (see `run-on-ec2.sh harvest`) instead of
# racing whatever happened to be left of the work budget.
sudo shutdown -c 2>/dev/null || true
sudo shutdown -h +__HARVEST_GRACE_MIN__ 2>/dev/null || true
echo "[box] run finished; results held for __HARVEST_GRACE_MIN__ min for harvest, then self-terminate"
REMOTE
  )"
  # The grace window is substituted here rather than expanded on the box: the run script is a QUOTED
  # heredoc precisely so nothing in it is interpreted locally, which also means it cannot read an
  # orchestrator variable.
  _run_sh="${_run_sh//__HARVEST_GRACE_MIN__/$HARVEST_GRACE_MIN}"
  printf '%s\n' "$_run_sh" | ssh $SSHOPT ubuntu@"$ip" "cat > ~/benchmarking/.run.sh" >>"$glog" 2>&1
  local launch_rc=$?
  if [ "$launch_rc" -eq 0 ]; then
    ssh $SSHOPT ubuntu@"$ip" "cd ~/benchmarking && rm -f .run-done .run.log && \
      setsid nohup bash -l .run.sh > .run.log 2>&1 < /dev/null &" >>"$glog" 2>&1
    launch_rc=$?
  else
    glog_echo "could not upload the remote run script to the box"
  fi
  local run_failed=0
  if [ "$launch_rc" -ne 0 ]; then
    glog_echo "detached otb launch FAILED (ssh rc=$launch_rc) - could not start the remote run"
    run_failed=1
  fi

  # ── incremental pull loop: while the remote run is alive (no .run-done sentinel yet) and the box is
  # still reachable, pull any suite whose result has landed but not yet been promoted. A box that dies
  # mid-run (self-terminate, spot reclaim) then still leaves us every suite that had written its JSON.
  # Cap the total wait at the box's own self-terminate ceiling + a margin so a wedged box can never hang
  # the orchestrator forever; the box timer (BENCH_MAX_MIN) is the ultimate cost backstop underneath.
  local sentinel="" reachable=1 waited=0
  local max_wait_s=$(( (BENCH_MAX_MIN + 30) * 60 ))
  # The engine already prints "[cell N/M] ingress>egress: verdict" to its own stdout as it works the
  # grid (see run.rs's grid walk); that stream lands in .run.log on the box but, until now, nothing
  # ever read it back here. Without it the fanout log goes silent from "running" to "DONE"/"INCOMPLETE"
  # with no way to tell a box that is 30/36 cells in from one that is wedged. Surface only the latest
  # line, and only when it changes, so a 30s poll doesn't spam a duplicate cell every tick.
  local _last_cell=""
  if [ "$run_failed" -eq 0 ]; then
    while :; do
      # sentinel present? read the remote exit code and stop.
      sentinel="$(ssh $SSHOPT ubuntu@"$ip" 'cat ~/benchmarking/.run-done 2>/dev/null' 2>/dev/null)"
      if [ -n "$sentinel" ]; then break; fi
      # box gone? retried liveness probe; only 3-for-3 failures means the box is unreachable
      # (terminated/spot reclaim) - stop polling and salvage whatever was already pulled. A lone
      # transient failure on a healthy box must not abandon a run that may still finish.
      if ! box_reachable "$ip"; then reachable=0; break; fi
      # opportunistic incremental pull of any suite not yet captured.
      for suite in $ALL_SUITES; do
        [ "${_pull_state[$suite]}" = ok ] && continue
        pull_suite "$suite" || true
      done
      local _cell
      _cell="$(ssh $SSHOPT ubuntu@"$ip" "grep -o '\[cell [0-9]*/[0-9]*\][^\$]*' ~/benchmarking/.run.log 2>/dev/null | tail -1" 2>/dev/null)"
      if [ -n "$_cell" ] && [ "$_cell" != "$_last_cell" ]; then glog_echo "$_cell"; _last_cell="$_cell"; fi
      [ "$waited" -ge "$max_wait_s" ] && { glog_echo "incremental pull loop hit max wait (${max_wait_s}s) - giving up on the run"; reachable=0; break; }
      sleep 30; waited=$((waited+30))
    done
  fi

  # Interpret the run outcome. A present sentinel = run-all.sh finished; its value is the exit code
  # (non-zero = a suite crashed). No sentinel = the box died before finishing.
  if [ "$run_failed" -eq 0 ]; then
    if [ -n "$sentinel" ]; then
      if [ "$sentinel" != 0 ]; then
        glog_echo "run-all.sh finished non-zero (exit=$sentinel) - a suite crashed or exited non-zero; results may be incomplete"
        run_failed=1
      fi
    else
      glog_echo "run-all.sh did NOT complete (no .run-done sentinel; box unreachable at ${waited}s) - salvaging suites already pulled"
      run_failed=1
    fi
  fi

  # Pull the remote run log into the fanout log (best-effort) so run-all.sh's output is preserved for
  # debugging even though the run was detached rather than streamed over the blocking ssh.
  if [ "$reachable" -eq 1 ]; then
    ssh $SSHOPT ubuntu@"$ip" 'cat ~/benchmarking/.run.log 2>/dev/null' >>"$glog" 2>/dev/null || true
  fi

  # ── final pull pass: catch any suite written just before completion (and re-attempt any still-missing
  # one while the box may briefly linger). Incremental + final passes together mean a mid-run box death
  # forfeits at most the one in-flight suite, never the six already on disk.
  glog_echo "final pull pass for $gw results"
  local pull_failed=0
  for suite in $ALL_SUITES; do
    if [ "${_pull_state[$suite]}" != ok ] && [ "$reachable" -eq 1 ]; then pull_suite "$suite" || true; fi
    case "${_pull_state[$suite]}" in
      ok)      : ;;
      stale)   glog_echo "GUARD held $suite/$gw.json (incoming was a boot/build failure) - committed data for this suite is STALE"; pull_failed=1 ;;
      *)       glog_echo "PULL FAILED for $suite/$gw.json (rc=${_pull_rc[$suite]}) - fresh result NOT retrieved; committed data for this suite is STALE"; pull_failed=1 ;;
    esac
  done
  # Pull the OOTB config sidecar too (best-effort). A gateway with no gw_config hook writes none - that
  # is "no config", not a pull failure, so it never contributes to pull_failed.
  if [ "$reachable" -eq 1 ]; then pull_config || true; fi
  # Pull the run's snapshot artifact(s) before the teardown trap terminates the box; this is the only
  # chance, since the box and everything on it are gone right after.
  #
  # This is the run's success criterion, not a best-effort extra: pull_snapshots returns 0 only when the
  # count of result_<gw>_*.json on disk actually grew, so it answers "did THIS run produce a
  # measurement?" and cannot be satisfied by an artifact that was already here. Its return value must
  # gate `snap_fresh` below, not be discarded, or a box that aborted before measuring anything could
  # still reach the DONE branch and publish.
  local snap_fresh=0
  if [ "$reachable" -eq 1 ] && pull_snapshots; then snap_fresh=1; fi
  if [ "$snap_fresh" -eq 0 ]; then
    glog_echo "NO FRESH SNAPSHOT for $gw - this run measured nothing, so it publishes nothing and the board keeps whatever it already had"
  fi
  # DONE means a CLEAN run that MEASURED something and was fully pulled. Anything less is INCOMPLETE, so
  # the freshness guard's later hard-fail is never a surprise and the gateway can be re-run.
  local publish_failed=0
  if [[ "$pull_failed" -eq 0 && "$run_failed" -eq 0 && "$snap_fresh" -eq 1 ]]; then
    glog_echo "DONE"
    # Record that at least one gateway measured something this run. The field-wide history + charts
    # publish at the very end reads this: charts.py regenerates from whatever is in results/, so a run
    # where every box died would otherwise rebuild the PREVIOUS run's numbers and push them, which is
    # indistinguishable from a successful run to anyone reading the board.
    echo "$gw" >> "$FRESH_SNAPSHOTS"
    # INCREMENTAL PUBLISH: this box finished cleanly and the promote guard passed for every suite, so
    # commit + push ONLY this gateway's result now (gated on PUBLISH, serialized across boxes). The
    # board fills in gateway-by-gateway; a single-gateway invocation pushes just that one row. The
    # result is safely on disk (operator can push by hand), but a publish failure - a stranded/unpushed
    # commit - MUST be counted in the run's issue tally so the summary never reads
    # "0 issues" while a row is missing from the pushed board.
    #   NB: `publish_gateway | tee` makes `$?` reflect `tee` (always 0), so key on PIPESTATUS[0].
    publish_gateway "$gw" 2>&1 | tee -a "$glog"
    if [[ "${PIPESTATUS[0]}" -ne 0 ]]; then
      publish_failed=1
      glog_echo "publish reported an issue for $gw (result IS committed/on disk; push may need a manual retry) - counting it as a run issue"
    fi
  else
    glog_echo "INCOMPLETE (the run crashed, measured nothing, or failed to pull; this gateway did NOT refresh - re-run it)"
    # DO NOT LEAVE A DIRTY TRACKED FILE BEHIND. An INCOMPLETE gateway never calls publish_gateway, but
    # pull_suite() may still have `mv -f`'d a fresh results/<suite>/$gw.json over a PREVIOUSLY-COMMITTED
    # tracked file for whichever suites DID succeed before a later suite failed - that file is now
    # modified-but-uncommitted in $HERE and nobody is ever going to commit it this run. Left in place, it
    # sits there until the final publish sweep's `git rebase --autostash` runs, which stashes it, and if
    # anything else (a peer box, the render-charts bot) touched that same path upstream in the meantime,
    # the stash POP conflicts - a rebase that already finished reporting failure, misread by
    # push_with_rebase as "could not start" and retried uselessly since the same conflict recurs every
    # attempt, eventually failing the WHOLE run's push, stranding every OTHER gateway's already-committed
    # result too. Revert it back to its last-committed state (exactly "did not refresh"): this path is
    # this gateway's own, so no peer box ever writes it and no lock is needed, same as pull_suite's own
    # unlocked mv -f above.
    for suite in $ALL_SUITES; do
      if [ "${_pull_state[$suite]:-}" = ok ]; then
        git -C "$HERE" checkout -- "results/$suite/$gw.json" 2>/dev/null || true
      fi
    done
  fi
  # Propagate the issue to the caller's `wait "$p" || fail=…` so the summary's issue count is accurate
  # and a run that measured nothing - OR a gateway whose publish never reached the remote - is never
  # reported as "0 issues".
  if [[ "$pull_failed" -ne 0 || "$run_failed" -ne 0 || "$publish_failed" -ne 0 || "$snap_fresh" -eq 0 ]]; then return 1; fi
  return 0
}

log "fanning out ${#GATEWAYS[@]} boxes (one per gateway): ${GATEWAYS[*]}"
pids=()
for gw in "${GATEWAYS[@]}"; do
  bench_gateway "$gw" &
  pids+=($!)
  sleep 3   # stagger the AWS API calls a touch
done
fail=0
for p in "${pids[@]}"; do wait "$p" || fail=$((fail+1)); done
log "all boxes done ($fail job(s) reported an issue - check results/fanout-*.log)"

# ── gateways that never got a sound box ───────────────────────────────────────────────────────────
# Say it LOUDLY and by name. A gateway skipped for box qualification is not a silent gap: its committed
# result is deliberately STALE (untouched, never overwritten by a number measured on bad hardware), and
# whoever reads the board has to know that. Already counted in `fail` via bench_gateway's return.
if [ -s "$QUALIFY_SKIPPED" ]; then
  log "BOX QUALIFICATION: $(wc -l < "$QUALIFY_SKIPPED" | tr -d ' ') gateway(s) were NOT published this run -"
  while IFS= read -r _line; do [ -n "$_line" ] && log "  $_line"; done < "$QUALIFY_SKIPPED"
  log "  (re-run those gateways; their committed results are the PREVIOUS run's, not this one's)"
fi

# ── A RUN THAT MEASURED NOTHING CHANGES NOTHING ───────────────────────────────────────────────────
# Everything below this line (the append-only history, charts.py, and the push) derives from whatever
# happens to be sitting in results/; none of it re-reads the boxes. So if every box failed,
# history/append.py would re-append the old numbers and charts.py would rebuild the previous run's
# charts and push them, making a total failure indistinguishable from a successful run from the
# outside. A failed run must leave the board exactly as it found it, so stop here and leave the working
# tree alone. `fail` is already non-zero from the boxes themselves, so the exit code still reports it.
if [ ! -s "$FRESH_SNAPSHOTS" ]; then
  log "NO GATEWAY MEASURED ANYTHING THIS RUN - not appending history, not regenerating charts, not pushing."
  log "  The board keeps exactly what it had. Re-run the gateways above; their fanout logs say why each failed."
  rm -f "$PUBLISH_LOCK" 2>/dev/null || true; rmdir "${PUBLISH_LOCK}.d" 2>/dev/null || true
  exit "$fail"
fi
log "$(wc -l < "$FRESH_SNAPSHOTS" | tr -d ' ') gateway(s) produced a fresh snapshot this run - regenerating and publishing from them"

# ── append this run to the append-only history (results/history/<gw>.jsonl) ─────────────────────
# Do NOT swallow a failure with `|| true`: a malformed result JSON or an unwritable
# results/history/ would otherwise complete the run "successfully" with the append-only history
# silently missing the whole run. Log loudly and count it as a run-level issue instead.
if ! python3 "$HERE/history/append.py"; then
  log "WARNING history/append.py FAILED - the append-only history was NOT updated for this run (investigate results/ JSON validity + results/history writability)"
  fail=$((fail+1))
fi

# ── regenerate charts + reports locally from the collected JSONs ──────────────────────────────────
log "regenerating charts + reports locally"
VENV="${TMPDIR:-/tmp}/bench-charts-venv"
if [[ ! -d "$VENV" ]]; then python3 -m venv "$VENV" >/dev/null 2>&1 || log "WARNING python3 -m venv failed - charts may not render (is python3-venv installed?)"; fi
"$VENV/bin/pip" install -q matplotlib >/dev/null 2>&1 || log "WARNING pip install matplotlib failed in the charts venv - charts.py will likely fail below"
# Warn loudly if matplotlib is genuinely absent BEFORE invoking charts.py, so a broken toolchain is a
# visible warning rather than a soft-logged no-op that leaves a "completed" run with no charts.
"$VENV/bin/python" -c 'import matplotlib' 2>/dev/null || log "WARNING matplotlib not importable in the charts venv - charts will NOT be regenerated this run"
if "$VENV/bin/python" "$HERE/charts.py"; then
  log "charts + reports regenerated"
else
  log "local chart regen failed (matplotlib?) - JSON results are still in results/; run charts.py yourself"
fi
log "done - results/reports/{all,top5}/README.md + results/*.png"

# ── final publish sweep: history + regenerated charts/reports ─────────────────────────────────────
# The per-gateway incremental publishes above push each gateway's result as its box finishes, but the
# APPEND-ONLY HISTORY (history/append.py) and the FIELD-WIDE CHARTS/REPORTS (charts.py) are produced
# HERE, after all boxes are done - so they are not yet committed. Push them now (gated on PUBLISH) so
# the board's charts + reports are fresh too. Uses the same serialized commit/push discipline; by now
# the boxes are joined so there is no contention. A single-gateway invocation still lands here and
# pushes only the artifacts that changed (typically that gateway's history line + the charts it moved).
if [[ "$PUBLISH" == "1" ]]; then
  # Same serialized commit + fetch/rebase/push discipline as publish_gateway: the boxes are joined by
  # now so there is no box-vs-box contention, but the render-charts bot can still move the remote ref,
  # so a bare push of our local HEAD is rejected non-fast-forward. Hold the same flock and push via
  # push_with_rebase (bounded fetch-rebase-push retry) so history + charts never strand locally.
  (
    # Same PID-owned lock + abort-on-timeout discipline as publish_gateway. Abort rather than pushing
    # unlocked; release only a lock we own.
    trap 'publish_lock_release' EXIT
    publish_lock_acquire "final publish:" log || exit 1
    # Do not swallow a staging failure: a failed add here would push an empty/unchanged HEAD and
    # silently drop the run's history + regenerated charts while reporting success. Build the path list
    # explicitly so an empty png glob (no charts this run, a benign case) is not mistaken for a staging
    # failure; a genuine `git add` error still aborts.
    _final_paths=( "$HERE/results/history" "$HERE/results/reports" )
    for _f in "$HERE"/results/*.png; do [ -e "$_f" ] && _final_paths+=("$_f"); done
    if ! git -C "$HERE" add -- "${_final_paths[@]}"; then
      log "WARNING final publish: git add FAILED for history + charts - NOT pushing an empty change"; exit 2
    fi
    if git -C "$HERE" diff --cached --quiet; then
      # An empty staged diff does not mean there is nothing to push: if every per-gateway push failed,
      # each gateway's commit is local-only, and a same-measured_at re-run adds no new history line or
      # changed charts, so the final stage is empty too. Mirror the per-gateway path (which pushes even
      # on an empty staged diff): still call push_with_rebase so any locally-stranded HEAD gets pushed
      # and the board recovers.
      log "final publish: no history/chart changes to stage - checking for locally-stranded commits to push"
      if push_with_rebase "final publish (recover-stranded):" log; then
        log "final publish: pushed (recovered any stranded local commits) to $PUBLISH_REMOTE/$PUBLISH_BRANCH"
      else
        exit 1
      fi
    elif git -C "$HERE" commit -q -m "bench: publish run history + regenerated charts/reports

Field-wide artifacts produced after all boxes finished (append-only history + charts.py output)."; then
      if push_with_rebase "final publish:" log; then
        log "final publish: pushed history + charts to $PUBLISH_REMOTE/$PUBLISH_BRANCH"
      else
        exit 1
      fi
    else
      log "WARNING final publish: git commit FAILED for history + charts"; exit 2
    fi
  )
  case $? in
    0) : ;;
    *) fail=$((fail+1)) ;;
  esac
else
  log "PUBLISH=0 - not pushing history/charts (left in the working tree)"
fi
# Clean up the publish lock artifacts this run created.
rm -f "$PUBLISH_LOCK" 2>/dev/null || true; rmdir "${PUBLISH_LOCK}.d" 2>/dev/null || true

exit "$fail"
