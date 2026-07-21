#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: One-API (songquanpeng/one-api), OpenAI-compatible OSS gateway, docker.
#
# One-API is a single Go container (SQLite by default) but it has no declarative upstream config:
# providers ("channels") are added at runtime via its admin API after login, and requests need a
# generated per-user token. gw_build brings the container up and scripts a channel that points its
# OpenAI base_url at the mock, then mints a token. This is best-effort and NOT yet verified serving
# end-to-end against the mock — left OUT of run-all.sh's default list until a box run confirms it.
# ONE_API_IMAGE is pinned in gateways/versions.env.
GW_KIND=docker
GW_PORT=3000
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=""   # filled with a minted token in gw_launch
ONE_API_IMAGE="${ONE_API_IMAGE:-justsong/one-api:latest}"
OA_JAR="$GW_DIR/cookies.txt"; OA_LOG="$GW_DIR/bootstrap.log"

gw_version() {
  local dg; dg=$(sudo docker inspect --format '{{index .RepoDigests 0}}' "$ONE_API_IMAGE" 2>/dev/null)
  echo "${ONE_API_IMAGE}${dg:+ (@${dg##*@})}"
}

gw_build() { sudo docker pull "$ONE_API_IMAGE" >/dev/null 2>&1 || true; }

# tiny JSON field reader (python3 is always present on the rig)
_oa_get(){ python3 -c 'import json,sys
try:
  d=json.load(sys.stdin)
except Exception:
  print(""); sys.exit()
for k in sys.argv[1:]:
  d=d.get(k,{}) if isinstance(d,dict) else {}
print(d if isinstance(d,(str,int)) else "")' "$@" 2>/dev/null; }

gw_launch() {
  : > "$OA_LOG"; rm -f "$OA_JAR"
  sudo docker rm -f one-api-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name one-api-bench --network host --cpuset-cpus="$CORES" \
    -e SQL_DSN="" "$ONE_API_IMAGE" >>"$OA_LOG" 2>&1
  local base="http://127.0.0.1:$GW_PORT"
  # 1) wait for the admin UI/API to answer
  local up=0 i; for i in $(seq 1 40); do
    [ "$(curl -s -m3 -o /dev/null -w '%{http_code}' "$base/api/status")" = 200 ] && { up=1; break; }; sleep 1
  done
  [ "$up" = 1 ] || { echo "one-api /api/status never came up" >>"$OA_LOG"; return 0; }
  # Endpoints below use TRAILING SLASHES and singular "group" — that's what One-API v0.6.10 binds
  # (verified against model/channel.go, model/token.go, router/api.go).
  # 2) admin login (default root/123456) → session cookie
  curl -s -c "$OA_JAR" -X POST "$base/api/user/login" \
    -H 'content-type: application/json' -d '{"username":"root","password":"123456"}' >>"$OA_LOG" 2>&1
  # 3) raise root quota so a long throughput run can't exhaust it (UpdateUser needs a full-ish object)
  curl -s -b "$OA_JAR" -X PUT "$base/api/user/" -H 'content-type: application/json' \
    -d '{"id":1,"username":"root","display_name":"Root User","role":100,"status":1,"quota":1000000000000,"group":"default"}' >>"$OA_LOG" 2>&1
  # 4) create an OpenAI-type channel (type=1) whose upstream base_url is the mock (it appends /v1/...)
  curl -s -b "$OA_JAR" -X POST "$base/api/channel/" -H 'content-type: application/json' \
    -d "{\"name\":\"mock\",\"type\":1,\"key\":\"sk-mock\",\"base_url\":\"http://127.0.0.1:$MOCK_PORT\",\"models\":\"$GW_MODEL\",\"group\":\"default\",\"status\":1}" >>"$OA_LOG" 2>&1
  # 5) mint an unlimited token — AddToken generates the key itself and returns it in .data.key
  local key; key=$(curl -s -b "$OA_JAR" -X POST "$base/api/token/" -H 'content-type: application/json' \
    -d '{"name":"bench","expired_time":-1,"remain_quota":0,"unlimited_quota":true}' | _oa_get data key)
  # fall back to listing tokens if the create response didn't echo the key
  [ -z "$key" ] && key=$(curl -s -b "$OA_JAR" "$base/api/token/?p=0&size=10" | python3 -c 'import json,sys
try: d=json.load(sys.stdin)
except Exception: sys.exit()
items=d.get("data") or {}
if isinstance(items,dict): items=items.get("records") or items.get("list") or []
for t in (items or []):
  if t.get("key"): print(t["key"]); break' 2>/dev/null)
  if [ -n "$key" ]; then GW_AUTH="sk-$key"; echo "one-api token minted (len=${#key})" >>"$OA_LOG"
  else echo "one-api token mint FAILED (see above)" >>"$OA_LOG"; fi
}

gw_diag() {
  echo "container: $(sudo docker ps -a --filter name=one-api-bench --format '{{.Status}}' 2>/dev/null)"
  echo "bootstrap.log:"; tail -n 25 "$OA_LOG" 2>/dev/null
  echo "logs:"; sudo docker logs --tail 15 one-api-bench 2>&1
}

gw_rss() {
  local m; m=$(sudo docker stats --no-stream --format '{{.MemUsage}}' one-api-bench 2>/dev/null | awk '{print $1}')
  case "$m" in *GiB) awk -v x="${m%GiB}" 'BEGIN{printf "%.1f", x*1024}';; *MiB) echo "${m%MiB}";; *) echo 0;; esac
}
gw_stop() { sudo docker rm -f one-api-bench >/dev/null 2>&1; }
