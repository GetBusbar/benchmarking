#!/usr/bin/env bash
# Run cargo-mutants for the engine on ONE big EC2 box and bring the report home.
#
# WHY NOT LOCALLY: 2,318 mutants x a ~60s test suite is 38 hours serial, and the parallelism that
# makes it tractable pinned a laptop hard enough to reboot it (load average 58). Mutation testing is
# embarrassingly parallel and completely stateless, which is exactly the shape a big short-lived box
# is for. It also keeps the machine you are working on usable.
#
# The box is terminated on every exit path, including failure and Ctrl-C.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE="${BENCH_STATE_DIR:-$HOME/.cache/gateway-bench}"
KEYNAME="gateway-bench-key"; KEYFILE="$STATE/${KEYNAME}.pem"
SGNAME="gateway-bench-sg"
ITYPE="${MUTANT_ITYPE:-m7g.8xlarge}"          # 32 vCPU Graviton; same arch family as the bench boxes
JOBS="${MUTANT_JOBS:-12}"                      # test processes in flight; see the note by --jobs below
SSM="/aws/service/canonical/ubuntu/server/24.04/stable/current/arm64/hvm/ebs-gp3/ami-id"
SSHOPT="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=15 -i $KEYFILE"
OUT="$HERE/results/mutants"

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }

SHA="$(git -C "$HERE" rev-parse HEAD)"
if ! git -C "$HERE" branch -r --contains "$SHA" >/dev/null 2>&1; then
  echo "PREFLIGHT: $SHA is not pushed; the box fetches by SHA and would fail." >&2; exit 1
fi
[[ -s "$KEYFILE" ]] || { echo "no key at $KEYFILE" >&2; exit 1; }

SG="$(aws ec2 describe-security-groups --filters "Name=group-name,Values=$SGNAME" \
      --query 'SecurityGroups[].GroupId' --output text 2>/dev/null)"
[[ -n "$SG" && "$SG" != "None" ]] || { echo "no security group $SGNAME" >&2; exit 1; }
AMI="$(aws ssm get-parameter --name "$SSM" --query Parameter.Value --output text)"

IID=""
cleanup() {
  if [[ -n "$IID" ]]; then
    log "terminating $IID"
    aws ec2 terminate-instances --instance-ids "$IID" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT INT TERM

log "launching $ITYPE for mutants @ $SHA"
IID="$(aws ec2 run-instances --image-id "$AMI" --instance-type "$ITYPE" --key-name "$KEYNAME" \
  --security-group-ids "$SG" \
  --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=60,VolumeType=gp3}' \
  --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=engine-mutants}]' \
  --query 'Instances[0].InstanceId' --output text)" || exit 1
log "instance $IID"
aws ec2 wait instance-running --instance-ids "$IID"
IP="$(aws ec2 describe-instances --instance-ids "$IID" \
      --query 'Reservations[].Instances[].PublicIpAddress' --output text)"
log "ip $IP - waiting for ssh"
for _ in $(seq 1 40); do
  ssh $SSHOPT "ubuntu@$IP" true 2>/dev/null && break; sleep 10
done

log "provisioning (rust + cargo-mutants); this compiles cargo-mutants once, a few minutes"
ssh $SSHOPT "ubuntu@$IP" bash -s <<'REMOTE'
set -e
sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential pkg-config libssl-dev git >/dev/null
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
. "$HOME/.cargo/env"
cargo install cargo-mutants --locked >/dev/null 2>&1 || cargo install cargo-mutants --locked
echo "rustc $(rustc --version) / $(cargo mutants --version)"
REMOTE

log "cloning @ $SHA"
ssh $SSHOPT "ubuntu@$IP" "git clone -q https://github.com/GetBusbar/benchmarking.git bench && git -C bench checkout -q $SHA && echo cloned"

# --jobs, not --test-threads: cargo-mutants runs whole `cargo test` invocations in parallel, and the
# suite contains timing-sensitive socket tests (the stream search simulator). Oversubscribing makes
# those flake, and a flaky test reports a mutant as CAUGHT when it was not - a false pass, which is
# worse than a slow run. 12 on 32 vCPU leaves each test process real headroom.
log "running mutants (-j $JOBS) - this is the long part"
ssh $SSHOPT "ubuntu@$IP" bash -s <<REMOTE
. "\$HOME/.cargo/env"
# RAISE THE DESCRIPTOR LIMIT FIRST. Ubuntu ships nofile=1024, and this suite binds a listener per
# socket test with many tests in flight; under cargo-mutants there are \$JOBS whole suites at once.
# The first attempt died at the BASELINE with EMFILE across ~40 tests - "Too many open files" even
# reading a manifest template - which reads exactly like a portability defect and is not one. The
# bench boxes already do this for the same reason (run-on-ec2.sh sets 1048576).
ulimit -n 1048576 2>/dev/null || ulimit -n "\$(ulimit -Hn)" 2>/dev/null || true
echo "[mutants] nofile soft=\$(ulimit -Sn) hard=\$(ulimit -Hn)"
cd ~/bench/engine
nohup cargo mutants --jobs $JOBS --timeout 300 > ~/mutants.log 2>&1 &
echo started
REMOTE

log "waiting for completion (polling every 5m)"
while :; do
  sleep 300
  done_now="$(ssh $SSHOPT "ubuntu@$IP" 'pgrep -c cargo-mutants || echo 0' 2>/dev/null || echo "?")"
  tail_now="$(ssh $SSHOPT "ubuntu@$IP" 'tail -1 ~/mutants.log 2>/dev/null' 2>/dev/null || true)"
  log "mutants running=$done_now | $tail_now"
  [[ "$done_now" == "0" ]] && break
done

log "pulling report"
mkdir -p "$OUT"
rsync -az -e "ssh $SSHOPT" "ubuntu@$IP:~/bench/engine/mutants.out/" "$OUT/" 2>/dev/null || true
rsync -az -e "ssh $SSHOPT" "ubuntu@$IP:~/mutants.log" "$OUT/mutants.log" 2>/dev/null || true
log "report in $OUT"
ls -la "$OUT" | head
