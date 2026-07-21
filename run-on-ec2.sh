#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# One-click: launch a fresh Graviton box, run the WHOLE benchmark FROM A FRESH COPY OF THIS REPO
# (latency + RPS + memory for every gateway), pull results/ back, and TERMINATE the box. Running from
# a fresh copy is the point — it proves the repo works standalone on a cold machine, exactly as a
# stranger cloning it would experience.
#
#   run-on-ec2.sh                                   # all gateways, all metrics
#   run-on-ec2.sh busbar litellm-rust               # a subset
#   BUSBAR_REPO=/path/to/busbar run-on-ec2.sh       # also build busbar from source (for the busbar row)
#
# Requires awscli v2 (configured), ssh, rsync. Instance is m7g.4xlarge (16 vCPU / 64 GB Graviton3) so
# no gateway OOMs the box; the in-rig watchdog still caps the load. Competitor gateways build/pull
# themselves on the box from the refs pinned in gateways/versions.env.
set -euo pipefail
export AWS_DEFAULT_REGION="${AWS_DEFAULT_REGION:-us-east-1}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # this repo (benchmarking) root
BUSBAR_REPO="${BUSBAR_REPO:-}"                          # busbar source to build (optional; for the busbar row)
GATEWAYS_ARG="$*"
ITYPE="${ITYPE:-m7g.4xlarge}"
HW_LABEL="AWS ${ITYPE} (Graviton3, 16 vCPU / 64 GB), Ubuntu 24.04"
SSM="/aws/service/canonical/ubuntu/server/24.04/stable/current/arm64/hvm/ebs-gp3/ami-id"
KEYNAME="gateway-bench-key"; KEYFILE="${TMPDIR:-/tmp}/${KEYNAME}.pem"; SGNAME="gateway-bench-sg"
SSHOPT="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=12 -i $KEYFILE"
log(){ echo "[$(date +%H:%M:%S)] $*"; }

if [[ ! -f "$KEYFILE" ]]; then
  aws ec2 delete-key-pair --key-name "$KEYNAME" >/dev/null 2>&1 || true
  aws ec2 create-key-pair --key-name "$KEYNAME" --query KeyMaterial --output text > "$KEYFILE"; chmod 600 "$KEYFILE"
fi
SG=$(aws ec2 describe-security-groups --group-names "$SGNAME" --query 'SecurityGroups[0].GroupId' --output text 2>/dev/null || true)
[[ -z "$SG" || "$SG" == "None" ]] && SG=$(aws ec2 create-security-group --group-name "$SGNAME" --description "gateway bench SSH" --query GroupId --output text)
MYIP=$(curl -s https://checkip.amazonaws.com)
aws ec2 authorize-security-group-ingress --group-id "$SG" --protocol tcp --port 22 --cidr "${MYIP}/32" >/dev/null 2>&1 || true

AMI=$(aws ssm get-parameter --name "$SSM" --query Parameter.Value --output text)
log "launching $ITYPE ($AMI)"
IID=$(aws ec2 run-instances --image-id "$AMI" --instance-type "$ITYPE" --key-name "$KEYNAME" \
  --security-group-ids "$SG" \
  --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=60,VolumeType=gp3}' \
  --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=gateway-bench},{Key=purpose,Value=gateway-bench}]' \
  --query 'Instances[0].InstanceId' --output text)
trap 'log "TERMINATING $IID"; aws ec2 terminate-instances --instance-ids "$IID" >/dev/null 2>&1 || true' EXIT
aws ec2 wait instance-running --instance-ids "$IID"
IP=$(aws ec2 describe-instances --instance-ids "$IID" --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
log "ip=$IP — waiting for ssh"
for _ in $(seq 1 40); do ssh $SSHOPT ubuntu@"$IP" true 2>/dev/null && break || sleep 8; done

log "installing deps (rust, go, docker, python, node)"
ssh $SSHOPT ubuntu@"$IP" 'set -e
  sudo apt-get update -q
  sudo apt-get install -y -q build-essential pkg-config libssl-dev python3-venv python3-pip golang-go docker.io git nodejs npm
  sudo usermod -aG docker ubuntu || true
  command -v cargo >/dev/null || (curl -sSf https://sh.rustup.rs | sh -s -- -y)
  python3 -m pip install --user -q --break-system-packages matplotlib psutil 2>/dev/null || pip3 install -q matplotlib psutil || true' 2>&1 | sed 's/^/  [setup] /'

log "rsync THIS repo up (fresh copy) → ~/benchmarking"
rsync -az --delete -e "ssh $SSHOPT" \
  --exclude .git --exclude '*/target' --exclude target --exclude results --exclude node_modules \
  "$HERE/" ubuntu@"$IP":~/benchmarking/

BB_ENV=""
if [[ -n "$BUSBAR_REPO" ]]; then
  log "rsync busbar source + build (release, jemalloc)"
  rsync -az --delete -e "ssh $SSHOPT" --exclude target --exclude .git --exclude node_modules \
    "$BUSBAR_REPO/" ubuntu@"$IP":~/busbar-src/
  ssh $SSHOPT ubuntu@"$IP" 'source ~/.cargo/env; cd ~/busbar-src && cargo build --release -p busbar 2>&1 | tail -3' 2>&1 | sed 's/^/  [busbar] /'
  BB_ENV='export BUSBAR_BIN=~/busbar-src/target/release/busbar'
fi

log "running the benchmark from the fresh repo (latency + RPS + memory)"
ssh $SSHOPT ubuntu@"$IP" "source ~/.cargo/env; cd ~/benchmarking
  $BB_ENV
  export BENCH_HARDWARE='$HW_LABEL'
  export CORES=0-7 LOADCORES=8-13 MOCKCORES=14-15
  export SUITES=\"${SUITES:-perf memory}\"
  sudo -n true 2>/dev/null && sudo chmod 666 /var/run/docker.sock || true
  bash run-all.sh $GATEWAYS_ARG" 2>&1 | sed 's/^/  [bench] /'

log "pulling results/ back"
rsync -az -e "ssh $SSHOPT" ubuntu@"$IP":~/benchmarking/results/ "$HERE/results/" || true
log "done — results/reports/{all,top5}/README.md + results/*.png"
