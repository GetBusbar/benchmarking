#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Gateway manifest: Envoy AI Gateway (envoyproxy/ai-gateway).
#
# ⚠ Envoy AI Gateway is a Kubernetes-native control plane: it configures Envoy Gateway via CRDs
# (AIGatewayRoute / AIServiceBackend) and expects a cluster, not a `docker run`. It cannot be stood
# up as a single-box drop-in against the mock without a k8s (kind/minikube) environment and CRDs
# whose AIServiceBackend points at http://127.0.0.1:$MOCK_PORT. Documented here for completeness and
# left OUT of run-all.sh's default list. ENVOY_AI_VERSION pins the ref in gateways/versions.env.
GW_KIND=docker
GW_PORT=10080
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=dummy

gw_version() { echo "envoyproxy/ai-gateway@${ENVOY_AI_VERSION:-latest}"; }
gw_build() {
  echo "[envoy-ai] needs Kubernetes + Envoy Gateway + AIGatewayRoute/AIServiceBackend CRDs" >&2
  echo "[envoy-ai] (backend → http://127.0.0.1:$MOCK_PORT). Not a single-box drop-in — see header." >&2
  return 1
}
gw_launch() { :; }
gw_rss() { echo 0; }
gw_stop() { :; }
