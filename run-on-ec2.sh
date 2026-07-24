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
  aws ec2 create-key-pair --key-name "$KEYNAME" --query KeyMaterial --output text > "$KEYFILE"; chmod 600 "$KEYFILE"
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
  # ran for hours because the trap missed SIGTERM and the manual cleanups silently no-op'd).
  iid=$(aws ec2 run-instances --image-id "$AMI" --instance-type "$ITYPE" --key-name "$KEYNAME" \
    --security-group-ids "$SG" \
    --instance-initiated-shutdown-behavior terminate \
    --user-data "$(printf '#!/bin/bash\nshutdown -h +%s\n' "${BENCH_MAX_MIN:-150}")" \
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

  glog_echo "running $gw (latency + RPS + memory)"
  ssh $SSHOPT ubuntu@"$ip" "source ~/.cargo/env; cd ~/benchmarking
    export BENCH_HARDWARE='$HW_LABEL'
    export BENCH_ARCH='$ARCH'
    export CORES=0-3 LOADCORES=4-9 MOCKCORES=10-15
    export CAP_MIB=24000
    export SUITES=\"${SUITES:-perf memory stream streamcpu xlate governed matrix}\"
    sudo -n true 2>/dev/null && sudo chmod 666 /var/run/docker.sock || true
    bash run-all.sh $gw" >>"$glog" 2>&1
  local ssh_rc=$?
  # run-all.sh now exits non-zero if any suite crashed (audit R3-M4/M5). Because bench_gateway runs in
  # a background subshell with only `set -uo pipefail` (no errexit), an ssh/remote failure was silently
  # ignored - the log jumped straight to PULL FAILED with no "ssh FAILED" line. Record it as an issue.
  local run_failed=0
  if [ "$ssh_rc" -ne 0 ]; then
    glog_echo "run-all.sh ssh FAILED (rc=$ssh_rc) - remote crashed or a suite exited non-zero; results may be incomplete"
    run_failed=1
  fi

  glog_echo "pulling $gw results back"
  local pull_failed=0
  for suite in perf memory stream streamcpu xlate governed matrix; do
    mkdir -p "$HERE/results/$suite"
    # Pull to a staging file, then let the promote guard decide. BULLETPROOF: a boot/build failure
    # (status 000, "failed to boot", missing entrypoint) must NEVER overwrite a committed served
    # result. The guard keeps the good data and logs loudly; a real result promotes normally.
    local staged="$HERE/results/$suite/.incoming-$gw.json"
    # RETRY the rsync: a dropped SSH/rsync ("unexpected end of file") must NOT silently leave stale
    # data behind - that is how a refresh betrays trust. Try up to 4 times with a pause, and if the
    # remote file genuinely does not exist (the suite produced no result) rsync returns 23, which we
    # treat as "no fresh result for this suite" and flag loudly, NOT a transient we retry forever.
    rm -f "$staged"; local ok=0 attempt rc
    for attempt in 1 2 3 4; do
      rsync -az --timeout=60 -e "ssh $SSHOPT" "ubuntu@$ip:~/benchmarking/results/$suite/$gw.json" "$staged" >>"$glog" 2>&1
      rc=$?
      if [[ $rc -eq 0 && -f "$staged" ]]; then ok=1; break; fi
      if [[ $rc -eq 23 ]]; then break; fi   # remote file missing: no fresh result, do not retry
      glog_echo "rsync $suite/$gw.json attempt $attempt failed (rc=$rc) - retrying in 10s"
      sleep 10
    done
    if [[ $ok -eq 1 ]]; then
      if python3 "$HERE/lib/promote_guard.py" "$suite" "$HERE/results/$suite/$gw.json" "$staged" >>"$glog" 2>&1; then
        mv -f "$staged" "$HERE/results/$suite/$gw.json"
      else
        glog_echo "GUARD kept prior $suite/$gw.json (incoming was a boot/build failure)"; rm -f "$staged"; pull_failed=1
      fi
    else
      glog_echo "PULL FAILED for $suite/$gw.json (rc=$rc) - fresh result NOT retrieved; committed data for this suite is STALE"
      pull_failed=1; rm -f "$staged"
    fi
  done
  # DONE means a CLEAN, fully-pulled fresh run. If any suite's pull failed or the guard kept old data,
  # this gateway did NOT cleanly refresh - say so loudly so the freshness guard's later hard-fail is
  # never a surprise and the gateway can be re-run.
  if [[ "$pull_failed" -eq 0 && "$run_failed" -eq 0 && -f "$HERE/results/perf/$gw.json" ]]; then glog_echo "DONE"
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
