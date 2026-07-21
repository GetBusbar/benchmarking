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

gw_version() { echo "${ONE_API_IMAGE:-justsong/one-api:latest}"; }

gw_build() {
  sudo docker pull "${ONE_API_IMAGE:-justsong/one-api:latest}" >/dev/null 2>&1 || true
}

gw_launch() {
  sudo docker rm -f one-api-bench >/dev/null 2>&1; sleep 1
  sudo docker run -d --name one-api-bench --network host --cpuset-cpus="$CORES" \
    "${ONE_API_IMAGE:-justsong/one-api:latest}" >/dev/null 2>&1
  # NOTE: channel + token bootstrap over One-API's admin API goes here. It requires an initial admin
  # login (default root/123456), POST /api/channel with an OpenAI channel whose base_url is the mock
  # (http://127.0.0.1:$MOCK_PORT), then POST /api/token to mint a key → GW_AUTH. Not yet scripted;
  # see the header. Until then this gateway records served=false rather than a fabricated number.
  echo "[one-api] channel/token bootstrap not yet scripted — will record served=false" >&2
}

gw_rss() {
  local m; m=$(sudo docker stats --no-stream --format '{{.MemUsage}}' one-api-bench 2>/dev/null | awk '{print $1}')
  case "$m" in *GiB) awk -v x="${m%GiB}" 'BEGIN{printf "%.1f", x*1024}';; *MiB) echo "${m%MiB}";; *) echo 0;; esac
}
gw_stop() { sudo docker rm -f one-api-bench >/dev/null 2>&1; }
