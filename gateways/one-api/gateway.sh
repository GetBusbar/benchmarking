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

# ── SINGLE SOURCE OF TRUTH ────────────────────────────────────────────────────────────────────────
# _oa_channels() is the ONE definition of the provider channels this gateway wires. Each line is
#   <type> <model-csv>  (type selects the native egress dialect: 1=OpenAI, 14=Anthropic, 24=Gemini).
# gw_launch reads these lines and POSTs each as a /api/channel/ create; gw_config reads the SAME lines
# and prints them into the published artifact. The benchmarked channels and the website-published
# channels are therefore identical by construction and cannot drift. Model lists are STRICTLY DISJOINT
# (gpt-* | claude-* | gemini-*) so each model resolves 1:1 to its channel (see routing note in gw_launch).
_oa_channels() {
  cat <<CH
1 gpt-4o-mini
14 claude-3-5-sonnet-20240620
24 gemini-1.5-pro
CH
}
# ncore = pinned core count (0-3 → 4). One-API is Go, and Go (pre-1.25) reads the HOST cpu count for
# GOMAXPROCS, NOT the --cpuset-cpus limit — so without it One-API runs a P per host core thrashing the
# few pinned cores, a scheduler-contention HANDICAP the Rust gateways (tokio available_parallelism
# respects cpuset) never pay. Pinning to the cpuset count emulates the 4-core box every gateway is
# measured on — the same Go-field parity fix gomodel and bifrost carry. Defined once, read by both
# gw_launch (docker -e flag) and gw_config (published artifact).
_oa_ncore() { echo $(( ${CORES##*-} - ${CORES%%-*} + 1 )); }

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
    -e GOMAXPROCS="$(_oa_ncore)" -e SQL_DSN="" "$ONE_API_IMAGE" >>"$OA_LOG" 2>&1
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
  # 4) wire ALL supported upstream providers as channels, base_url = mock (One-API appends each
  #    provider's native path). This is the OOTB deployment a real user runs: every provider they
  #    support is a live channel, all present simultaneously — so memory/throughput/latency measure
  #    the full multi-provider gateway, not a single-channel slice. The channel TYPE selects the
  #    native egress dialect (1=OpenAI, 14=Anthropic, 24=Gemini). Routing keys on (group=default,
  #    model) via the abilities table — channel TYPE is NOT part of selection
  #    (model/ability.go GetRandomSatisfiedChannel: WHERE group=? AND model=? … ORDER BY RANDOM()).
  #    So the three model lists are kept STRICTLY DISJOINT (gpt-* | claude-* | gemini-*): each model
  #    resolves 1:1 to its channel with no equal-priority random tiebreak. gw_matrix_egress sets
  #    GW_MODEL per column to a model unique to one channel, exercising exactly that provider.
  _oa_channel(){ # <type> <model-csv>
    curl -s -b "$OA_JAR" -X POST "$base/api/channel/" -H 'content-type: application/json' \
      -d "{\"name\":\"mock-$1\",\"type\":$1,\"key\":\"sk-mock\",\"base_url\":\"http://127.0.0.1:$MOCK_PORT\",\"models\":\"$2\",\"group\":\"default\",\"status\":1}" >>"$OA_LOG" 2>&1
  }
  # Wire each channel from the SINGLE-SOURCE _oa_channels list (type=1 OpenAI, 14 Anthropic -> /v1/messages,
  # 24 Gemini -> :generateContent). gw_config publishes the SAME list, so run and artifact cannot drift.
  local ctype cmodels
  while read -r ctype cmodels; do
    [ -n "$ctype" ] && _oa_channel "$ctype" "$cmodels"
  done < <(_oa_channels)
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
gw_hwm() { container_hwm_mib one-api-bench; }  # summed process-tree VmHWM (kernel high-water mark)
gw_stop() { sudo docker rm -f one-api-bench >/dev/null 2>&1; }

# ── matrix suite: declared capability + egress wiring ─────────────────────────────────────────────
# Declared 6x6 (rows=ingress, cols=egress), axis order: openai openai-responses anthropic gemini
# cohere bedrock. One-API v0.6.10 provisions upstream "channels" at runtime; the channel TYPE selects
# the native egress dialect and its base_url is overridable to the mock: type=1 OpenAI, type=14
# Anthropic (-> /v1/messages), type=24 Gemini (-> :generateContent) (relay/channeltype/define.go +
# per-provider relay/adaptor/*). The OpenAI-canonical ingress is translated to the channel's native
# upstream shape. So the capable row is openai-ingress into {openai, anthropic, gemini}. NOT declared:
# openai-responses (no responses relay mode/adaptor in v0.6.10), cohere (type=35 emits the Cohere *v1*
# /v1/chat shape, not the v2 dialect this suite probes) and bedrock (the AWS channel type=33 ignores
# base_url and signs SigV4 to the real regional host - the mock is unreachable) - all grey with the
# cited reason.
# Evidence: relay/channeltype/define.go (type constants), relay/adaptor/{anthropic,gemini,cohere,aws},
# relay/relaymode/define.go (no responses), tag v0.6.10. Wired-pending-field-verification.
GW_MATRIX_CAP="
101100
000000
000000
000000
000000
000000
"
GW_MATRIX_CAP_NOTE="One-API v0.6.10 has no OpenAI-Responses relay mode and its Cohere channel emits the Cohere v1 /v1/chat shape, not the v2 dialect this suite probes (relay/channeltype/define.go, relay/adaptor/cohere/adaptor.go GetRequestURL); those cells are grey by that capability limit"
# Bedrock is NOT incapability: One-API relays Bedrock in production, but its AWS channel (type 33)
# builds an aws-sdk-go-v2 bedrockruntime client with no BaseEndpoint override and bypasses the
# generic base_url relay entirely (relay/adaptor/aws/adaptor.go Init(); GetRequestURL returns "").
# SigV4 goes to the real bedrock-runtime.<region>.amazonaws.com, so this rig's mock cannot stand
# in - recorded as untestable, distinct from declared-incapable.
GW_MATRIX_UNTESTABLE="openai/bedrock"
GW_MATRIX_UNTESTABLE_NOTE="One-API v0.6.10's AWS/Bedrock channel constructs the aws-sdk bedrockruntime client with no endpoint override and skips the base_url relay path (relay/adaptor/aws/adaptor.go), so the harness mock cannot stand in for the upstream; One-API does relay Bedrock in production"
GW_MATRIX_EGRESS="openai anthropic gemini"

# ── xlate lane: not declared (no /v1/messages ingress at v0.6.10) ────────────────────────────────
# router/relay.go at tag v0.6.10 registers only OpenAI-shaped relay paths (/v1/chat/completions,
# /v1/completions, embeddings, audio, images, ...); the only "messages" routes are the OpenAI
# Assistants thread stubs bound to RelayNotImplemented. There is no Claude-Messages ingress (the
# new-api FORK added one; upstream one-api main still has none), so the probe's 404 "Invalid URL"
# was the router's correct answer, not a failed translation.
GW_XLATE_CAP=0
GW_XLATE_CAP_NOTE="One-API v0.6.10 has no Anthropic /v1/messages ingress route (router/relay.go registers only OpenAI-shaped relay paths; the Claude-Messages ingress exists only in the new-api fork), so anthropic-in translation is not a claimed capability"
# All three channels are wired in every launch (see gw_launch step 4); the matrix just selects which
# provider a probe exercises by setting GW_MODEL to a model unique to one channel. gw_launch re-runs
# the identical all-provider bootstrap.
gw_matrix_egress() {
  case "$1" in
    openai)    GW_MODEL="gpt-4o-mini";;
    anthropic) GW_MODEL="claude-3-5-sonnet-20240620";;
    gemini)    GW_MODEL="gemini-1.5-pro";;
    *) return 1;;
  esac
  gw_launch
}

# ── OOTB config artifact (runtime admin-API state, not a file) ────────────────────────────────────
# gw_config prints the canonical OOTB config this gateway launches with. One-API has NO declarative
# config file — its upstream providers are runtime admin-API state (channels + a token), scripted in
# gw_launch. So the artifact is that provisioned state, rendered as the exact admin-API calls the
# bootstrap makes: the container run flags, then the three channels (all providers wired, base_url =
# mock, disjoint model lists) and the minted token. The suite runner captures this once per run into
# results/config/one-api.txt and the board publishes it, so "fresh container + these admin calls →
# these numbers" is reproducible. Kept in lockstep with gw_launch by construction (same values).
# OOTB posture: nothing is stripped or tuned. One-API's per-request usage/quota accounting is
# structural (preConsumeQuota/postConsumeQuota run unconditionally — NOT a disableable knob), and
# consume-logging is default-ON (LogConsumeEnabled=true); both stay on, exactly as it ships. The only
# deviations are the permitted ones: provider base_urls → mock, dummy channel key (sk-mock), the
# minted bench token (its own mandatory auth — every relay request needs a token), and SQL_DSN=""
# (the embedded-SQLite run-mechanic: no external DB). Auth values are dummy on the isolated rig.
gw_config() {
  # DERIVED from the SAME single-source values gw_launch runs: the run flags (image + GOMAXPROCS core-pin
  # + SQL_DSN run-mechanic) and the channel lines (from _oa_channels). Nothing here is hand-restated, so
  # the published artifact is exactly what gw_launch provisioned.
  cat <<ENV
# container (embedded SQLite — SQL_DSN="" is the no-external-DB run-mechanic; GOMAXPROCS = pinned core count)
docker run --network host --cpuset-cpus=$CORES -e GOMAXPROCS=$(_oa_ncore) -e SQL_DSN="" $ONE_API_IMAGE
# admin bootstrap (default root/123456; token auth is One-API's own mandatory per-request auth)
POST /api/user/login              {"username":"root","password":"123456"}
# all supported providers wired as channels (base_url = mock; model lists disjoint for 1:1 routing)
ENV
  local ctype cmodels
  while read -r ctype cmodels; do
    [ -n "$ctype" ] && printf 'POST /api/channel/  type=%-3s models=%-32s base_url=http://127.0.0.1:%s  key=sk-mock  group=default\n' \
      "$ctype" "$cmodels" "$MOCK_PORT"
  done < <(_oa_channels)
  cat <<ENV
# minted per-request token (unlimited quota) -> sent as the bench bearer
POST /api/token/                  {"name":"bench","expired_time":-1,"unlimited_quota":true}
ENV
}
