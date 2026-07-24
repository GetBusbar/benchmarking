#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# One-click, FAIR-BY-ISOLATION: launch ONE fresh Graviton box PER GATEWAY, all in parallel, each from
# a fresh copy of THIS repo. Every gateway is measured on a pristine machine — no chance one gateway's
# leftover page cache, disk, or docker state skews the next. Same total cost as a single sequential box
# (N boxes for ~1/N the wall-clock), and much faster end to end.
#
#   run-on-ec2.sh                                   # all gateways, one box each, in parallel
#   run-on-ec2.sh litellm-rust bifrost              # a subset, one box each
#
# Requires awscli v2 (configured), ssh, rsync. Each box is m7g.4xlarge (16 real Graviton3 cores): the
# gateway-under-test is pinned to 4 cores (= an m7g.xlarge, the class AIGatewayBench uses); the mock +
# load generator get 6 cores each, so the harness can never steal cycles from the gateway. EVERY
# gateway build/pulls itself on its box from the ref pinned in gateways/versions.env.
set -uo pipefail
export AWS_DEFAULT_REGION="${AWS_DEFAULT_REGION:-us-east-1}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # this repo (benchmarking) root

# Per-invocation run id: every box THIS run launches is tagged run=$RUN_ID, and teardown filters on
# it so a second (or concurrent) invocation NEVER terminates the first run's boxes / pulls the rug on
# its results (audit H4). The global `kill` subcommand stays the cross-run cleanup.
RUN_ID="${RUN_ID:-$(date +%Y%m%d-%H%M%S)-$$}"
CREATED_KEY=0; CREATED_SG=0   # only delete the shared key/SG on exit if THIS invocation created them

# Box self-terminate safety net (audit R5-#4). This `shutdown -h +N` is the LEAKED-BOX backstop - it
# must fire only when the orchestrator has lost the box, NEVER during a legitimate run. The matrix
# suite alone raises its OWN wall-clock ceiling to 14400s = 240 min (matrix/run.sh: HARNESS_SUITE_CEIL_S
# default 14400 when MATRIX_SWEEP=1), and it runs LAST after 6 other suites. A 150-min box timer was
# SHORTER than that single ceiling, so a heavy gateway's matrix sweep could still believe it had
# headroom while AWS terminated the box mid-run - discarding every already-written suite JSON. Set the
# net strictly ABOVE the longest legitimate run: matrix 240 min + a generous 120-min margin for the
# other six suites = 360 min. Overridable, but the default can never fire during a real run.
BENCH_MAX_MIN="${BENCH_MAX_MIN:-360}"

# ── INCREMENTAL PER-GATEWAY PUBLISH (matrix-sole-source) ──────────────────────────────────────────
# Each gateway's ENTIRE benchmark is now ONE atomic matrix run, and gateways publish INDEPENDENTLY (the
# relaxed freshness guard in site/gen-data.mjs no longer hard-fails a board with mixed per-gateway
# ages). So instead of the operator publishing everything by hand at the very end, we commit + push
# EACH gateway's result the moment its box finishes cleanly (DONE, all suites pulled, promote guard
# passed). The board then fills in gateway-by-gateway; the Pages deploy regenerates data.json from all
# committed results/ on every push, so pushing one fresh gateway updates just its row.
#
# The SINGLE-GATEWAY path falls straight out of this: `run-on-ec2.sh busbar` re-runs only busbar, and
# only busbar's result is committed + pushed (the "new busbar version → update just busbar" flow).
#
# PUBLISH gates the auto-push. Default ON for the field run; set PUBLISH=0 for a local/dry run so a
# development run never pushes. When off, results are still pulled + committed-nothing (left in the
# working tree) exactly as before — the operator can inspect and publish by hand.
PUBLISH="${PUBLISH:-1}"
# Branch to push results to (the Pages deploy watches this). Overridable for a test branch.
PUBLISH_BRANCH="${PUBLISH_BRANCH:-$(git -C "$HERE" rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)}"
PUBLISH_REMOTE="${PUBLISH_REMOTE:-origin}"
# Serialize all git operations across the parallel per-gateway boxes: commit + push touch the shared
# index/refs, so two boxes finishing at once would race (one's `git add` sees the other's half-staged
# tree, or two concurrent pushes collide). A single lock dir makes publish strictly one-at-a-time.
PUBLISH_LOCK="${TMPDIR:-/tmp}/gateway-bench-publish-${RUN_ID}.lock"

# `run-on-ec2.sh kill` — terminate EVERY gateway-bench box right now, reliably. Uses xargs so the
# instance IDs are split into separate args (piping `--output text` straight into `--instance-ids`
# passes one tab-joined blob → InvalidInstanceID.Malformed → a silent no-op, which is exactly how 48
# boxes leaked on 2026-07-24). Run this if a run is ever interrupted and you want a guaranteed cleanup.
if [[ "${1:-}" == "kill" || "${1:-}" == "--kill" ]]; then
  echo "terminating all gateway-bench instances in $AWS_DEFAULT_REGION ..."
  aws ec2 describe-instances --filters "Name=tag:purpose,Values=gateway-bench" \
    "Name=instance-state-name,Values=running,pending,stopping,stopped" \
    --query 'Reservations[].Instances[].InstanceId' --output text 2>/dev/null \
    | tr '\t' '\n' | grep -E '^i-' \
    | xargs -r -n 25 aws ec2 terminate-instances --output text --instance-ids >/dev/null 2>&1
  left=$(aws ec2 describe-instances --filters "Name=tag:purpose,Values=gateway-bench" \
    "Name=instance-state-name,Values=running,pending" --query 'length(Reservations[].Instances[])' --output text 2>/dev/null)
  echo "done — running/pending remaining: ${left:-?}"
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
log(){ echo "[$(date +%H:%M:%S)] $*"; }

# Default field: every gateway with a manifest under gateways/ (discovered from disk, alphabetical;
# same source as run-all.sh — add/remove a dir and both follow). Envoy AI Gateway is absent (k8s-native).
DEFAULT_GATEWAYS=()
for d in "$HERE"/gateways/*/gateway.sh; do DEFAULT_GATEWAYS+=("$(basename "$(dirname "$d")")"); done
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
  # default umask between create and chmod (audit R5-NIT). The chmod stays as a belt-and-braces backstop.
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
# records a field-wide false "did not serve" while burning N boxes (audit R2-M2). Retry, then fail
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
# current IP ends up with NO SSH ingress and every ssh/rsync times out (audit R4-LOW-6). So: treat the
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
# launched — filtered by tag:run=$RUN_ID, NOT the shared purpose=gateway-bench tag — so a second or
# concurrent invocation never terminates another run's still-live boxes before their results are
# pulled (audit H4). (SIGKILL can't be trapped; the boxes' own `shutdown -h` timer is the backstop.)
# IDs are split via xargs — piping --output text straight into --instance-ids passes a tab-joined
# blob that no-ops. The shared keypair/SG are deleted only if THIS invocation created them (otherwise
# a concurrent run's ssh/rsync would break with "Permission denied (publickey)"); `run-on-ec2.sh kill`
# stays the global cleanup for the shared key/SG.
teardown() {
  aws ec2 describe-instances --filters "Name=tag:run,Values=$RUN_ID" \
    "Name=instance-state-name,Values=running,pending" --query 'Reservations[].Instances[].InstanceId' --output text 2>/dev/null \
    | tr '\t' '\n' | grep -E '^i-' | xargs -r -n25 aws ec2 terminate-instances --output text --instance-ids >/dev/null 2>&1
  if [[ "$CREATED_KEY" == 1 ]]; then aws ec2 delete-key-pair --key-name "$KEYNAME" >/dev/null 2>&1 || true; rm -f "$KEYFILE"; fi
  if [[ "$CREATED_SG" == 1 ]]; then aws ec2 delete-security-group --group-id "$SG" >/dev/null 2>&1 || true; fi
}
trap teardown EXIT INT TERM

AMI=$(aws ssm get-parameter --name "$SSM" --query Parameter.Value --output text)

mkdir -p "$HERE"/results/{perf,memory,stream,xlate,governed,matrix}

# ── commit + push ONE gateway's result (incremental publish) ──────────────────────────────────────
# Called from bench_gateway the moment that box has cleanly finished (DONE). Commits ONLY this
# gateway's freshly-pulled result files (its per-suite JSONs, its append-only history line, its OOTB
# config sidecar, and any regenerated per-gateway chart) and pushes them, so the board updates just
# this row. No-op (returns 0) when PUBLISH=0 so a local/dry run never pushes. Serialized under a flock
# so the parallel boxes commit + push strictly one-at-a-time (shared index/refs). Best-effort: a push
# failure is logged loudly and returns non-zero (counted as a run issue) but never aborts other boxes.
publish_gateway() { # gw glog_echo_fn
  local gw="$1"
  [[ "$PUBLISH" == "1" ]] || { echo "[$gw] PUBLISH=0 — not committing/pushing (result left in the working tree)"; return 0; }
  # Serialize: only one box commits/pushes at a time. flock on a lock fd; fall back to a mkdir spin-lock
  # on hosts without util-linux flock (macOS orchestrator). The subshell holds the lock for its body.
  (
    if command -v flock >/dev/null 2>&1; then
      exec 9>"$PUBLISH_LOCK"; flock 9
    else
      # mkdir spin-lock: atomic create; wait (bounded) for a peer box to finish its push.
      local _spun=0
      until mkdir "${PUBLISH_LOCK}.d" 2>/dev/null; do sleep 2; _spun=$((_spun+2)); [ "$_spun" -ge 600 ] && break; done
      trap 'rmdir "${PUBLISH_LOCK}.d" 2>/dev/null || true' EXIT
    fi
    # Stage ONLY this gateway's artifacts (never a sibling box's in-flight files):
    #   - its per-suite result JSONs (results/<suite>/<gw>.json)
    #   - its append-only history line (results/history/<gw>.jsonl)
    #   - its OOTB config sidecar (results/config/<gw>.txt)
    #   - any per-gateway chart the local regen produced for it (results/*<gw>*.png) — usually charts
    #     are regenerated field-wide at the very end, but staging a per-gw one here is harmless.
    local -a paths=()
    local f
    for f in "$HERE"/results/*/"$gw".json "$HERE"/results/history/"$gw".jsonl "$HERE"/results/config/"$gw".txt; do
      [ -e "$f" ] && paths+=("$f")
    done
    for f in "$HERE"/results/*"$gw"*.png; do [ -e "$f" ] && paths+=("$f"); done
    if [ "${#paths[@]}" -eq 0 ]; then echo "[$gw] publish: no result files to commit (nothing pulled?)"; exit 0; fi
    git -C "$HERE" add -- "${paths[@]}" 2>/dev/null || true
    # Nothing actually changed vs HEAD (identical re-run) → skip the empty commit, still try a push in
    # case a prior push failed and left commits unpushed.
    if git -C "$HERE" diff --cached --quiet; then
      echo "[$gw] publish: no content change vs HEAD — skipping commit"
    else
      git -C "$HERE" commit -q -m "bench($gw): publish matrix run result

Incremental per-gateway publish: $gw's box finished cleanly, committing only
its result so the board updates just this row (matrix-sole-source)." \
        || { echo "[$gw] publish: git commit FAILED"; exit 1; }
      echo "[$gw] committed $gw's result"
    fi
    if git -C "$HERE" push "$PUBLISH_REMOTE" "HEAD:$PUBLISH_BRANCH" >/dev/null 2>&1; then
      echo "[$gw] pushed to $PUBLISH_REMOTE/$PUBLISH_BRANCH — the board will regenerate data.json and update $gw's row"
    else
      echo "[$gw] publish: git push to $PUBLISH_REMOTE/$PUBLISH_BRANCH FAILED (commit is local; retry the push by hand or re-run)"; exit 1
    fi
  )
}

# ── one box, one gateway (runs in the background, self-terminates) ─────────────────────────────────
bench_gateway() {
  local gw="$1" iid="" ip=""
  local tag="gateway-bench-$gw"
  local glog="$HERE/results/fanout-$gw.log"
  : > "$glog"
  glog_echo(){ echo "[$(date +%H:%M:%S)] [$gw] $*" | tee -a "$glog"; }

  # provision. COST SAFETY NET: the box self-terminates after BENCH_MAX_MIN minutes no matter what -
  # even if this orchestrator is killed (so its RETURN-trap never fires) the box shuts itself down and
  # `instance-initiated-shutdown-behavior=terminate` makes that a TERMINATE, not a stop. A leaked box
  # can therefore bleed cost for at most BENCH_MAX_MIN, never indefinitely (2026-07-24: 48 leaked boxes
  # ran for hours because the trap missed SIGTERM and the manual cleanups silently no-op'd). BENCH_MAX_MIN
  # is set ABOVE the matrix suite's own 240-min ceiling (see top of file) so it never fires mid-run.
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
  glog_echo "ip=$ip — waiting for ssh"
  local ok=0; for _ in $(seq 1 40); do ssh $SSHOPT ubuntu@"$ip" true 2>/dev/null && { ok=1; break; } || sleep 8; done
  [[ $ok == 1 ]] || { glog_echo "ssh never came up"; return 1; }

  glog_echo "installing deps (bare base: docker + psutil; the rig is a prebuilt download, and each"
  glog_echo "gateway installs its OWN prereqs via gw_prereqs — no blanket build toolchain on every box)"
  ssh $SSHOPT ubuntu@"$ip" 'set -e
    sudo apt-get update -q
    # BARE base only: docker (for the image gateways), curl (fetch the prebuilt rig), jq, and python3
    # + psutil (the memory suite reads RSS). NO build-essential/rust/go/node here — the mock+loadgen
    # are prebuilt binaries pulled from the rig release, and the 2 source-built gateways pull their
    # own toolchain via gw_prereqs() on their box ALONE. Docker-image gateways are up in ~2 min.
    sudo apt-get install -y -q docker.io curl ca-certificates jq python3-pip
    sudo usermod -aG docker ubuntu || true
    # FAIRNESS: a container inherits the docker DAEMON fd limit, NOT the host-shell ulimit that
    # perf/run.sh raises for native gateways + the loadgen/mock. Left at the ~1024 default, a
    # containerised gateway fast enough to hold >1024 concurrent connections hits EMFILE and
    # COLLAPSES at exactly c=1024 (busbar did: ~850k conn failures, sustained@20ms fell to 1/3).
    echo "{ \"default-ulimits\": { \"nofile\": { \"Name\": \"nofile\", \"Hard\": 1048576, \"Soft\": 1048576 } } }" | sudo tee /etc/docker/daemon.json >/dev/null
    sudo systemctl restart docker || sudo service docker restart || true
    python3 -m pip install --user -q --break-system-packages psutil 2>/dev/null || pip3 install -q psutil || true' >>"$glog" 2>&1

  # Ship ONLY the harness (scripts + configs, a few MB). Exclude every build/runtime artifact: the box
  # fetches the 2 rig binaries from the release (lib/rig.sh) and builds its own gateway (docker pull, or
  # gw_build for the 2 source gateways). A stray local venv (litellm's 564MB) or bin/ must never be
  # uploaded to 13 boxes. Log the payload size + transfer time so a slow rsync is never a silent hang.
  # Payload size for the tripwire (added after the 564MB-venv incident so a slow rsync is never a
  # silent hang). GNU `du --exclude` is rejected by the BSD `du` on the darwin orchestrator (always
  # logged "?"), defeating the check on the real host. Derive the size from a LOCAL rsync DRY RUN with
  # the SAME excludes the real transfer uses (below): portable, no network, and exactly the bytes about
  # to ship. `--stats` prints "Total file size: N bytes"; humanise it (a number beats "?").
  # Dedicated per-gateway sizecheck dst under a mktemp -d, removed right after we read --stats: the old
  # fixed path (${TMPDIR:-/tmp}/bench-rsync-sizecheck-dst/) was NEVER cleaned, so on macOS /tmp (not
  # cleared on reboot) it accreted the harness skeleton for every gateway x every run (audit R4-LOW-7).
  local _szdst; _szdst=$(mktemp -d "${TMPDIR:-/tmp}/bench-rsync-sizecheck-XXXXXX")
  local _pl; _pl=$(rsync -an --stats \
    --exclude .git --exclude '*/target' --exclude target --exclude results --exclude node_modules \
    --exclude '*/venv' --exclude venv --exclude __pycache__ --exclude bin --exclude '*.pem' --exclude '*.log' --exclude '.incoming-*' \
    "$HERE/" "$_szdst/" 2>/dev/null \
    | awk -F: '/Total file size/{gsub(/[^0-9]/,"",$2); b=$2+0; if(b>=1073741824)printf "%.1fG",b/1073741824; else if(b>=1048576)printf "%.1fM",b/1048576; else if(b>=1024)printf "%.1fK",b/1024; else printf "%dB",b}')
  rm -rf "$_szdst"
  glog_echo "rsync harness up (${_pl:-?}) ..."; local _t0=$SECONDS
  rsync -az --delete -e "ssh $SSHOPT" \
    --exclude .git --exclude '*/target' --exclude target --exclude results --exclude node_modules \
    --exclude '*/venv' --exclude venv --exclude __pycache__ --exclude bin --exclude '*.pem' --exclude '*.log' --exclude '.incoming-*' \
    "$HERE/" ubuntu@"$ip":~/benchmarking/ >>"$glog" 2>&1
  # bench_gateway runs under `set -uo pipefail` (no errexit), so a transient SSH drop that makes the
  # UPWARD rsync exit non-zero would otherwise be ignored and "rsync done" logged regardless - leaving
  # the box running whatever partial/stale harness tree survived from a prior run. Measuring the wrong
  # code is indistinguishable from a correct run and passes the promote guard. Abort this box instead:
  # a stale/partial tree must never be measured (audit R4-LOW-4). RETURN trap terminates the box.
  local _up_rc=$?
  if [ "$_up_rc" -ne 0 ]; then
    glog_echo "rsync UP FAILED (rc=$_up_rc) - harness upload incomplete; refusing to measure a partial/stale tree, tearing down this box"
    return 1
  fi
  glog_echo "rsync done (${_pl:-?} in $((SECONDS-_t0))s)"

  # ── per-suite staged pull + promote guard, factored out so it can run INCREMENTALLY during the run
  # AND once more at the end (audit R5-#4b). Idempotent: pulls results/<suite>/<gw>.json to a staging
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

  local ALL_SUITES="${SUITES:-perf memory stream streamcpu xlate governed matrix}"
  declare -A _pull_state=() _pull_rc=(); local suite
  for suite in $ALL_SUITES; do _pull_state[$suite]=unset; _pull_rc[$suite]=0; done

  glog_echo "running $gw (latency + RPS + memory) — detached on box; pulling each suite as it completes"
  # Launch run-all.sh DETACHED on the box (setsid + nohup) writing a sentinel with its real exit code on
  # completion, instead of a single BLOCKING ssh. Why (audit R5-#4b): the old blocking ssh returned only
  # when run-all.sh finished, and EVERY per-suite pull happened AFTER it returned - so if the box
  # self-terminated mid-run (a heavy matrix sweep outliving the box timer), the ssh died and ALL SEVEN
  # already-written suite JSONs were forfeited, not just the in-flight one. Detaching lets us stream each
  # suite's result OFF-box as run-all.sh writes it, so a late box death loses at most the running suite.
  ssh $SSHOPT ubuntu@"$ip" "cd ~/benchmarking && rm -f .run-done .run.log && \
    setsid nohup bash -lc '
      source ~/.cargo/env 2>/dev/null || true
      export BENCH_HARDWARE=\"$HW_LABEL\"
      export BENCH_ARCH=\"$ARCH\"
      export CORES=0-3 LOADCORES=4-9 MOCKCORES=10-15
      export CAP_MIB=24000
      export SUITES=\"$ALL_SUITES\"
      sudo -n true 2>/dev/null && sudo chmod 666 /var/run/docker.sock || true
      bash run-all.sh $gw; echo \$? > .run-done
    ' > .run.log 2>&1 < /dev/null &" >>"$glog" 2>&1
  local launch_rc=$?
  local run_failed=0
  if [ "$launch_rc" -ne 0 ]; then
    glog_echo "detached run-all.sh launch FAILED (ssh rc=$launch_rc) - could not start the remote run"
    run_failed=1
  fi

  # ── incremental pull loop: while the remote run is alive (no .run-done sentinel yet) and the box is
  # still reachable, pull any suite whose result has landed but not yet been promoted. A box that dies
  # mid-run (self-terminate, spot reclaim) then still leaves us every suite that had written its JSON.
  # Cap the total wait at the box's own self-terminate ceiling + a margin so a wedged box can never hang
  # the orchestrator forever; the box timer (BENCH_MAX_MIN) is the ultimate cost backstop underneath.
  local sentinel="" reachable=1 waited=0
  local max_wait_s=$(( (BENCH_MAX_MIN + 30) * 60 ))
  if [ "$run_failed" -eq 0 ]; then
    while :; do
      # sentinel present? read the remote exit code and stop.
      sentinel="$(ssh $SSHOPT ubuntu@"$ip" 'cat ~/benchmarking/.run-done 2>/dev/null' 2>/dev/null)"
      if [ -n "$sentinel" ]; then break; fi
      # box gone? one cheap liveness ssh; a failure here means the box is unreachable (terminated/spot
      # reclaim) - stop polling and salvage whatever was already pulled.
      if ! ssh $SSHOPT ubuntu@"$ip" true 2>/dev/null; then reachable=0; break; fi
      # opportunistic incremental pull of any suite not yet captured.
      for suite in $ALL_SUITES; do
        [ "${_pull_state[$suite]}" = ok ] && continue
        pull_suite "$suite" || true
      done
      [ "$waited" -ge "$max_wait_s" ] && { glog_echo "incremental pull loop hit max wait (${max_wait_s}s) - giving up on the run"; reachable=0; break; }
      sleep 30; waited=$((waited+30))
    done
  fi

  # Interpret the run outcome. A present sentinel = run-all.sh finished; its value is the exit code
  # (non-zero = a suite crashed, audit R3-M4/M5). No sentinel = the box died before finishing.
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
  # DONE means a CLEAN, fully-pulled fresh run. If any suite's pull failed or the guard kept old data,
  # this gateway did NOT cleanly refresh - say so loudly so the freshness guard's later hard-fail is
  # never a surprise and the gateway can be re-run.
  if [[ "$pull_failed" -eq 0 && "$run_failed" -eq 0 && -f "$HERE/results/perf/$gw.json" ]]; then
    glog_echo "DONE"
    # INCREMENTAL PUBLISH: this box finished cleanly and the promote guard passed for every suite, so
    # commit + push ONLY this gateway's result now (gated on PUBLISH, serialized across boxes). The
    # board fills in gateway-by-gateway; a single-gateway invocation pushes just that one row. We do
    # NOT fail the box on a publish hiccup — the result is safely on disk and the operator can push by
    # hand — but a publish failure is logged and counted as a run issue via the || below.
    if ! publish_gateway "$gw" 2>&1 | tee -a "$glog"; then
      glog_echo "publish reported an issue for $gw (result IS committed/on disk; push may need a manual retry)"
    fi
  else glog_echo "INCOMPLETE (a suite crashed, failed to pull, or was guard-held; this gateway did NOT fully refresh - re-run it)"; fi
  # Propagate the issue to the caller's `wait "$p" || fail=…` so the summary's issue count is accurate
  # and a run missing whole suites is never reported as "0 issues" (audit R3-M4/M5).
  if [[ "$pull_failed" -ne 0 || "$run_failed" -ne 0 || ! -f "$HERE/results/perf/$gw.json" ]]; then return 1; fi
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
log "all boxes done ($fail job(s) reported an issue — check results/fanout-*.log)"

# ── append this run to the append-only history (results/history/<gw>.jsonl) ─────────────────────
# Do NOT swallow a failure with `|| true` (audit R3-LOW-4): a malformed result JSON or an unwritable
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
# visible warning rather than a soft-logged no-op that leaves a "completed" run with no charts (R3-LOW-4).
"$VENV/bin/python" -c 'import matplotlib' 2>/dev/null || log "WARNING matplotlib not importable in the charts venv - charts will NOT be regenerated this run"
if "$VENV/bin/python" "$HERE/charts.py"; then
  log "charts + reports regenerated"
else
  log "local chart regen failed (matplotlib?) — JSON results are still in results/; run charts.py yourself"
fi
log "done — results/reports/{all,top5}/README.md + results/*.png"

# ── final publish sweep: history + regenerated charts/reports ─────────────────────────────────────
# The per-gateway incremental publishes above push each gateway's result as its box finishes, but the
# APPEND-ONLY HISTORY (history/append.py) and the FIELD-WIDE CHARTS/REPORTS (charts.py) are produced
# HERE, after all boxes are done — so they are not yet committed. Push them now (gated on PUBLISH) so
# the board's charts + reports are fresh too. Uses the same serialized commit/push discipline; by now
# the boxes are joined so there is no contention. A single-gateway invocation still lands here and
# pushes only the artifacts that changed (typically that gateway's history line + the charts it moved).
if [[ "$PUBLISH" == "1" ]]; then
  git -C "$HERE" add -- "$HERE/results/history" "$HERE"/results/*.png "$HERE/results/reports" 2>/dev/null || true
  if git -C "$HERE" diff --cached --quiet; then
    log "final publish: no history/chart changes to push"
  elif git -C "$HERE" commit -q -m "bench: publish run history + regenerated charts/reports

Field-wide artifacts produced after all boxes finished (append-only history + charts.py output)."; then
    if git -C "$HERE" push "$PUBLISH_REMOTE" "HEAD:$PUBLISH_BRANCH" >/dev/null 2>&1; then
      log "final publish: pushed history + charts to $PUBLISH_REMOTE/$PUBLISH_BRANCH"
    else
      log "WARNING final publish: git push FAILED (history + charts committed locally; push by hand)"; fail=$((fail+1))
    fi
  else
    log "WARNING final publish: git commit FAILED for history + charts"; fail=$((fail+1))
  fi
else
  log "PUBLISH=0 — not pushing history/charts (left in the working tree)"
fi
# Clean up the publish lock artifacts this run created.
rm -f "$PUBLISH_LOCK" 2>/dev/null || true; rmdir "${PUBLISH_LOCK}.d" 2>/dev/null || true

exit "$fail"
