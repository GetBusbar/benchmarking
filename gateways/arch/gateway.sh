#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Arch (katanemo/archgw), the Envoy-based AI-native gateway.
#
# ⚠ Arch is not a single `docker run`: it is an Envoy data plane plus Arch's own model/prompt
# services, normally brought up with the `archgw` CLI (`archgw up`) from an arch_config.yaml that
# lists `llm_providers`. Pointing it at a mock means an arch_config.yaml with an OpenAI-style
# provider whose base URL is http://127.0.0.1:$MOCK_PORT, launched via the CLI/compose. Documented
# here, left OUT of run-all.sh's default list until the multi-service bring-up is scripted.
# ARCH_IMAGE / ARCH_VERSION pin the refs in gateways/versions.env.
GW_KIND=docker
GW_PORT=10000
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=dummy

gw_version() { echo "${ARCH_IMAGE:-katanemo/archgw:latest}"; }
gw_build() {
  echo "[arch] needs the archgw CLI / Envoy + Arch services (arch_config.yaml with llm_providers" >&2
  echo "[arch] → http://127.0.0.1:$MOCK_PORT). Not a single-container drop-in — see the header." >&2
  return 1
}
gw_launch() { :; }
gw_rss() { echo 0; }
gw_stop() { sudo docker rm -f arch-bench >/dev/null 2>&1; }
