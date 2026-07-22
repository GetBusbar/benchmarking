#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: One-API (songquanpeng/one-api), OpenAI-compatible OSS gateway, docker.
#
# One-API is a single Go container (SQLite by default) with no declarative upstream config: providers
# ("channels") are added at runtime via its admin API after login, and requests need a generated
# per-user token. gw_launch scripts the login → channel (base_url = mock) → token bootstrap. It's in
# the default field (bootstrap verified on the rig). NOTE ON ITS NUMBERS: One-API writes a per-request
# usage/quota row to its DB on EVERY call by design — that accounting is intrinsic to how it works and
# is NOT a disableable "logging" knob, so its latency/throughput reflect a gateway that bills each
# request, not a bare proxy. That's the honest measurement of One-API as it ships. Pin in versions.env.
GW_KIND=docker
# Self-describing manifest metadata — charts.py + the run lists read these, so a gateway
# is fully defined by its own dir (add/remove a dir → it appears/disappears everywhere).
GW_DISPLAY="One-API"                      # label in charts + report tables
GW_LANG=Go                            # implementation language → bar color bucket
GW_CLASS="API distribution system"   # the project's OWN self-description (README: OpenAI key management & distribution system), not our editorial
GW_REPO=https://github.com/songquanpeng/one-api   # linked from the gateway name in the report table
GW_PORT=3000
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=""   # filled with a minted token in gw_launch
ONE_API_IMAGE="${ONE_API_IMAGE:-justsong/one-api:v0.6.10}"
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

gw_rss() { container_rss_mib one-api-bench; }  # summed process-tree VmRSS (same method as native gateways)
gw_stop() { sudo docker rm -f one-api-bench >/dev/null 2>&1; }

# matrix suite (6x6): no gw_matrix_egress hook is defined for this manifest, so every egress
# column beyond the default upstream renders "not configurable" (neutral, distinct from
# tried-and-failed). Reason: channels are provisioned at runtime over the admin API with type=1
# (OpenAI). Other channel types exist (Anthropic, Gemini, ...), but each has its own base_url
# semantics and none has been verified against the recording mock from this harness, so wiring them
# blind would risk false tried-and-failed reds.
