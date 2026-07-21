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
#   run-on-ec2.sh litellm-rust bifrost              # a subset
#
# Requires awscli v2 (configured), ssh, rsync. Instance is m7g.4xlarge (16 vCPU / 64 GB Graviton3) so
# no gateway OOMs the box; the in-rig watchdog still caps the load. EVERY gateway build/pulls itself
# on the box from the ref pinned in gateways/versions.env — nothing gateway-specific to pass.
set -euo pipefail
export AWS_DEFAULT_REGION="${AWS_DEFAULT_REGION:-us-east-1}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"   # this repo (benchmarking) root
GATEWAYS_ARG="$*"
# m7g.2xlarge = 8 real Graviton3 cores (no hyperthreading). The gateway-under-test is pinned to 4
# cores (= an m7g.xlarge, the class AIGatewayBench uses); the mock + load generator get the other 4,
# so the harness can never steal cycles from or bottleneck the gateway.
ITYPE="${ITYPE:-m7g.2xlarge}"
HW_LABEL="AWS ${ITYPE} (Graviton3, 8 cores / 32 GB) — gateway pinned to 4 cores (m7g.xlarge class), mock+loadgen on the other 4, Ubuntu 24.04"
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

log "running the benchmark from the fresh repo (latency + RPS + memory)"
# Every gateway self-provisions from the ref pinned in gateways/versions.env — Busbar included
# (it extracts the released image's binary). Nothing gateway-specific to pass here.
ssh $SSHOPT ubuntu@"$IP" "source ~/.cargo/env; cd ~/benchmarking
  export BENCH_HARDWARE='$HW_LABEL'
  # Gateway pinned to 4 cores (m7g.xlarge class); loadgen + mock isolated on the other 4.
  export CORES=0-3 LOADCORES=4-5 MOCKCORES=6-7
  export CAP_MIB=24000   # 32 GB box: watchdog kills the load before the box OOMs
  export SUITES=\"${SUITES:-perf memory}\"
  sudo -n true 2>/dev/null && sudo chmod 666 /var/run/docker.sock || true
  bash run-all.sh $GATEWAYS_ARG" 2>&1 | sed 's/^/  [bench] /'

log "pulling results/ back"
rsync -az -e "ssh $SSHOPT" ubuntu@"$IP":~/benchmarking/results/ "$HERE/results/" || true
log "done — results/reports/{all,top5}/README.md + results/*.png"
