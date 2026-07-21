#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: GPTRouter (Writesonic/GPTRouter), OSS LLM router.
#
# ⚠ NOT a single-container drop-in: GPTRouter ships as a docker-compose stack (the router service +
# Postgres + a queue), and providers are registered at runtime via its API. It can't be stood up by
# one `docker run` against the mock, so it is documented here but left OUT of run-all.sh's default
# list. To benchmark it, bring up its compose stack, register an OpenAI provider whose base_url is
# http://127.0.0.1:$MOCK_PORT, and set GW_PORT/GW_PATH/GW_AUTH accordingly. GPTROUTER_IMAGE pins the
# router image in gateways/versions.env.
GW_KIND=docker
GW_PORT=8090
GW_PATH=/api/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=dummy

gw_version() { echo "${GPTROUTER_IMAGE:-ghcr.io/writesonic/gptrouter:latest}"; }
gw_build() {
  echo "[gptrouter] needs its docker-compose stack (router + Postgres + queue) + runtime provider" >&2
  echo "[gptrouter] registration — not a single-container drop-in. See this manifest's header." >&2
  return 1   # skip cleanly; the runner records it as unavailable rather than faking a number
}
gw_launch() { :; }
gw_rss() { echo 0; }
gw_stop() { sudo docker rm -f gptrouter-bench >/dev/null 2>&1; }
