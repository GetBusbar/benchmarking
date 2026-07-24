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
# Requires awscli v2 (configured), ssh, rsync. Each box is m7g.2xlarge (8 real Graviton3 cores): the
# gateway-under-test is pinned to 4 cores (= an m7g.xlarge, the class AIGatewayBench uses); the mock +
# load generator get the other 4, so the harness can never steal cycles from the gateway. EVERY gateway
# build/pulls itself on its box from the ref pinned in gateways/versions.env.
set -uo pipefail
export AWS_DEFAULT_REGION="${AWS_DEFAULT_REGION:-us-east-1}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # this repo (benchmarking) root

# ── ARCHITECTURE: the easy flip ───────────────────────────────────────────────────────────────────
# ARCH=arm64 (default) runs the whole field on Graviton (m7g); ARCH=x86 runs it on Intel (m7i). One
# knob picks the instance family AND the matching Ubuntu AMI. Every gateway builds/pulls for that arch
# on its own box, and the arch is recorded in each result so runs from different arches never get
# confused. (To measure BOTH, run twice with RESULTS_ARCH_SUBDIR set — see the header of run-all.sh.)
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
if [[ ! -s "$KEYFILE" ]]; then
  aws ec2 delete-key-pair --key-name "$KEYNAME" >/dev/null 2>&1 || true
  aws ec2 create-key-pair --key-name "$KEYNAME" --query KeyMaterial --output text > "$KEYFILE"; chmod 600 "$KEYFILE"
fi
SG=$(aws ec2 describe-security-groups --group-names "$SGNAME" --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null || true)
[[ -z "$SG" || "$SG" == "None" ]] && SG=$(aws ec2 create-security-group --group-name "$SGNAME" --description "gateway bench SSH" --query GroupId --output text)
MYIP=$(curl -s https://checkip.amazonaws.com)
aws ec2 authorize-security-group-ingress --group-id "$SG" --protocol tcp --port 22 --cidr "${MYIP}/32" >/dev/null 2>&1 || true
AMI=$(aws ssm get-parameter --name "$SSM" --query Parameter.Value --output text)

mkdir -p "$HERE"/results/{perf,memory,stream,xlate,governed,matrix}

# ── one box, one gateway (runs in the background, self-terminates) ─────────────────────────────────
bench_gateway() {
  local gw="$1" iid="" ip=""
  local tag="gateway-bench-$gw"
  local glog="$HERE/results/fanout-$gw.log"
  : > "$glog"
  glog_echo(){ echo "[$(date +%H:%M:%S)] [$gw] $*" | tee -a "$glog"; }

  # provision
  iid=$(aws ec2 run-instances --image-id "$AMI" --instance-type "$ITYPE" --key-name "$KEYNAME" \
    --security-group-ids "$SG" \
    --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=60,VolumeType=gp3}' \
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=$tag},{Key=purpose,Value=gateway-bench}]" \
    --query 'Instances[0].InstanceId' --output text 2>>"$glog") || { glog_echo "run-instances FAILED (vCPU limit?)"; return 1; }
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

  glog_echo "rsync repo up"
  rsync -az --delete -e "ssh $SSHOPT" \
    --exclude .git --exclude '*/target' --exclude target --exclude results --exclude node_modules \
    "$HERE/" ubuntu@"$ip":~/benchmarking/ >>"$glog" 2>&1

  glog_echo "running $gw (latency + RPS + memory)"
  ssh $SSHOPT ubuntu@"$ip" "source ~/.cargo/env; cd ~/benchmarking
    export BENCH_HARDWARE='$HW_LABEL'
    export BENCH_ARCH='$ARCH'
    export CORES=0-3 LOADCORES=4-9 MOCKCORES=10-15
    export CAP_MIB=24000
    export SUITES=\"${SUITES:-perf memory stream streamcpu xlate governed matrix}\"
    sudo -n true 2>/dev/null && sudo chmod 666 /var/run/docker.sock || true
    bash run-all.sh $gw" >>"$glog" 2>&1

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
  if [[ "$pull_failed" -eq 0 && -f "$HERE/results/perf/$gw.json" ]]; then glog_echo "DONE"
  else glog_echo "INCOMPLETE (a suite failed to pull or was guard-held; this gateway did NOT fully refresh - re-run it)"; fi
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
python3 "$HERE/history/append.py" || true

# ── regenerate charts + reports locally from the collected JSONs ──────────────────────────────────
log "regenerating charts + reports locally"
VENV="${TMPDIR:-/tmp}/bench-charts-venv"
if [[ ! -d "$VENV" ]]; then python3 -m venv "$VENV" >/dev/null 2>&1 || true; fi
"$VENV/bin/pip" install -q matplotlib >/dev/null 2>&1 || true
if "$VENV/bin/python" "$HERE/charts.py"; then
  log "charts + reports regenerated"
else
  log "local chart regen failed (matplotlib?) — JSON results are still in results/; run charts.py yourself"
fi
log "done — results/reports/{all,top5}/README.md + results/*.png"
