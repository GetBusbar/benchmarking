#!/usr/bin/env bash
# Run cargo-mutants for the engine on ONE big EC2 box and bring the report home.
#
# WHY NOT LOCALLY: 2,318 mutants x a ~60s test suite is 38 hours serial, and the parallelism that
# makes it tractable pinned a laptop hard enough to reboot it (load average 58). Mutation testing is
# embarrassingly parallel and completely stateless, which is exactly the shape a big short-lived box
# is for. It also keeps the machine you are working on usable.
#
# The box is terminated on every exit path, including failure and Ctrl-C. A SIGKILL'd or vanished
# orchestrator cannot run that trap, so the box also carries its own backstop: it is launched with
# instance-initiated-shutdown-behavior=terminate and a `shutdown -h +MUTANT_MAX_MIN` in its user-data,
# and it is tagged purpose=gateway-bench so `run-on-ec2.sh kill` reaches it too.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE="${BENCH_STATE_DIR:-$HOME/.cache/gateway-bench}"
KEYNAME="gateway-bench-key"; KEYFILE="$STATE/${KEYNAME}.pem"
SGNAME="gateway-bench-sg"
ITYPE="${MUTANT_ITYPE:-m7g.8xlarge}"          # 32 vCPU Graviton; same arch family as the bench boxes
JOBS="${MUTANT_JOBS:-12}"                      # test processes in flight; see the note by --jobs below
MAX_MIN="${MUTANT_MAX_MIN:-720}"               # box self-terminates after this many minutes, whatever happens here
SSM="/aws/service/canonical/ubuntu/server/24.04/stable/current/arm64/hvm/ebs-gp3/ami-id"
OUT="$HERE/results/mutants"

log() { printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*"; }

SHA="$(git -C "$HERE" rev-parse HEAD)"
# `branch -r --contains` exits 0 even when NO remote branch contains the commit (it just prints
# nothing), so the test is on its output, the same form run-on-ec2.sh uses.
if ! git -C "$HERE" branch -r --contains "$SHA" 2>/dev/null | grep -q .; then
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
    if ! aws ec2 terminate-instances --instance-ids "$IID" >/dev/null; then
      log "TERMINATE FAILED for $IID - it is still billing; terminate it by hand (or ./run-on-ec2.sh kill)"
    fi
    IID=""
  fi
}
# EXIT does the terminate; a signal only has to EXIT. A handler that terminated the box and returned
# would resume the script - straight back into the poll loop against a dead host, forever.
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
die() { log "FATAL: $*"; exit 1; }

log "launching $ITYPE for mutants @ $SHA"
IID="$(aws ec2 run-instances --image-id "$AMI" --instance-type "$ITYPE" --key-name "$KEYNAME" \
  --security-group-ids "$SG" \
  --instance-initiated-shutdown-behavior terminate \
  --user-data "$(printf '#!/bin/bash\nshutdown -h +%s\n' "$MAX_MIN")" \
  --block-device-mappings 'DeviceName=/dev/sda1,Ebs={VolumeSize=60,VolumeType=gp3,DeleteOnTermination=true}' \
  --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=engine-mutants},{Key=purpose,Value=gateway-bench}]' \
  --query 'Instances[0].InstanceId' --output text)" || exit 1
log "instance $IID"
aws ec2 wait instance-running --instance-ids "$IID" || die "$IID never reached running"
IP="$(aws ec2 describe-instances --instance-ids "$IID" \
      --query 'Reservations[].Instances[].PublicIpAddress' --output text)"
[[ -n "$IP" && "$IP" != "None" ]] || die "$IID has no public IP"
KNOWN_HOSTS="$STATE/$IID.known_hosts"
python3 "$HERE/lib/ssh_trust.py" "$IID" "$IP" "$KNOWN_HOSTS" || die "host key authentication failed"
SSHOPT=(-o BatchMode=yes -o StrictHostKeyChecking=yes -o "UserKnownHostsFile=$KNOWN_HOSTS" -o ConnectTimeout=15 -i "$KEYFILE")
printf -v RSYNC_SSH '%q ' ssh "${SSHOPT[@]}"
log "ip $IP - waiting for ssh"
ssh_ok=0
for _ in $(seq 1 40); do
  ssh "${SSHOPT[@]}" "ubuntu@$IP" true 2>/dev/null && { ssh_ok=1; break; }; sleep 10
done
[[ $ssh_ok == 1 ]] || die "ssh to $IP never came up"

log "provisioning (rust + cargo-mutants); this compiles cargo-mutants once, a few minutes"
ssh "${SSHOPT[@]}" "ubuntu@$IP" bash -s <<'REMOTE'
set -e
sudo apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential pkg-config libssl-dev git >/dev/null
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal >/dev/null
. "$HOME/.cargo/env"
cargo install cargo-mutants --locked >/dev/null 2>&1 || cargo install cargo-mutants --locked
echo "rustc $(rustc --version) / $(cargo mutants --version)"
REMOTE
[[ $? == 0 ]] || die "provisioning failed on $IP"

log "cloning @ $SHA"
ssh "${SSHOPT[@]}" "ubuntu@$IP" "git clone -q https://github.com/GetBusbar/benchmarking.git bench && git -C bench checkout -q $SHA && echo cloned" \
  || die "clone/checkout of $SHA failed on $IP"

# --jobs, not --test-threads: cargo-mutants runs whole `cargo test` invocations in parallel, and the
# suite contains timing-sensitive socket tests (the stream search simulator). Oversubscribing makes
# those flake, and a flaky test reports a mutant as CAUGHT when it was not - a false pass, which is
# worse than a slow run. 12 on 32 vCPU leaves each test process real headroom.
log "running mutants (-j $JOBS) - this is the long part"
ssh "${SSHOPT[@]}" "ubuntu@$IP" bash -s <<REMOTE
. "\$HOME/.cargo/env"
# RAISE THE DESCRIPTOR LIMIT FIRST. Ubuntu ships nofile=1024, and this suite binds a listener per
# socket test with \$JOBS whole test-suite processes in flight under cargo-mutants - unlifted, this
# hits EMFILE ("Too many open files") even at the baseline run, before any mutant is tried. Same fix
# as run-on-ec2.sh applies for the same reason.
ulimit -n 1048576 2>/dev/null || ulimit -n "\$(ulimit -Hn)" 2>/dev/null || true
echo "[mutants] nofile soft=\$(ulimit -Sn) hard=\$(ulimit -Hn)"
cd ~/bench/engine || exit 1
nohup cargo mutants --jobs $JOBS --timeout 300 > ~/mutants.log 2>&1 &
echo started
REMOTE
[[ $? == 0 ]] || die "could not start cargo mutants on $IP"

log "waiting for completion (polling every 5m)"
# `pgrep -c` prints 0 AND exits 1 when nothing matches, so the old `|| echo 0` produced "0\n0", which
# never equals "0": a finished run was polled forever. `|| true` keeps pgrep's own count. An ssh
# failure reads as "?"; a box unreachable for 6 polls in a row (30 min) is given up on.
unreachable=0
while :; do
  sleep 300
  done_now="$(ssh "${SSHOPT[@]}" "ubuntu@$IP" 'pgrep -c cargo-mutants || true' 2>/dev/null | head -1 | tr -d '\r\n ')"
  [[ -n "$done_now" ]] || done_now="?"
  tail_now="$(ssh "${SSHOPT[@]}" "ubuntu@$IP" 'tail -1 ~/mutants.log 2>/dev/null' 2>/dev/null || true)"
  log "mutants running=$done_now | $tail_now"
  [[ "$done_now" == "0" ]] && break
  if [[ "$done_now" == "?" ]]; then
    unreachable=$((unreachable + 1))
    [[ $unreachable -ge 6 ]] && break
  else
    unreachable=0
  fi
done

log "pulling report"
mkdir -p "$OUT"
pulled=0
for _ in 1 2 3; do
  if rsync -az -e "$RSYNC_SSH" "ubuntu@$IP:~/bench/engine/mutants.out/" "$OUT/"; then pulled=1; break; fi
  sleep 10
done
rsync -az -e "$RSYNC_SSH" "ubuntu@$IP:~/mutants.log" "$OUT/mutants.log" || log "WARNING mutants.log not pulled"
if [[ $pulled != 1 ]]; then
  log "REPORT NOT RETRIEVED: mutants.out could not be pulled from $IP (box unreachable=$unreachable polls); the box is terminated on exit"
  exit 1
fi
log "report in $OUT"
ls -la "$OUT" | head
