#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# THE ONE CHOKE POINT for "which URL does the matrix POST this ingress dialect to".
#
# THE BUG THIS FILE EXISTS FOR (harness bug, published-board corruption):
# these paths used to be EAGERLY expanded once, at matrix/run.sh init, into P_OPENAI/P_GEMINI/...
# globals that ingress_path() then echoed back verbatim:
#
#     P_GEMINI="${GW_MATRIX_PATH_GEMINI:-/v1beta/models/$GW_MODEL:generateContent}"   # init-time $GW_MODEL
#     P_BEDROCK="${GW_MATRIX_PATH_BEDROCK:-/model/$GW_MODEL/converse}"                # init-time $GW_MODEL
#
# gemini and bedrock are the ONLY two dialects whose model rides in the URL PATH; every other dialect
# carries "model" in the request BODY, and ingress_body() re-expands the body per call. Since commit
# 02345b4 ("one-config OOTB standard") several manifests switch GW_MODEL PER EGRESS COLUMN inside
# gw_matrix_egress (most of the field does this now)
# instead of rewriting the gateway config. Those later GW_MODEL changes could never reach a path that
# had already been frozen at init — so every gemini/bedrock probe, in every column, kept asking for the
# INITIAL model, which routes to the openai provider. Result: {gemini,bedrock} x {every non-openai
# egress} was IMPOSSIBLE to pass - 10 unreachable cells on a single gateway, published as red with
# "the mock received the request on the openai endpoint instead", plus 400/404s elsewhere.
# gemini->openai and bedrock->openai still passed, because there the frozen model was genuinely right.
#
# THE CONTRACT: ingress_path resolves its defaults at CALL time, so it always reflects the CURRENT
# GW_MODEL (and GW_PATH / GW_ANTHROPIC_PATH) — whatever gw_matrix_egress last set for this column —
# while an explicit GW_MATRIX_PATH_* manifest override still wins exactly as before. One choke point:
# every consumer (the warm-up, the per-cell probe loop, the memory window) calls this function, so a
# manifest that flips GW_MODEL is honoured everywhere or nowhere — never in some call sites only.
#
# Guarded by lib/ingress_path_test.sh (CI: .github/workflows/bench-tests.yml).
#
# Sourced by matrix/run.sh AFTER the manifest (gateways/<gw>/gateway.sh), which supplies GW_PATH,
# GW_MODEL and the optional GW_ANTHROPIC_PATH / GW_MATRIX_PATH_* overrides.

# ingress_path <ingress-dialect> -> the gateway URL path to POST that dialect to.
# Manifest override (GW_MATRIX_PATH_*) always wins; otherwise the protocol's canonical default,
# expanded HERE, NOW, against the live GW_MODEL. The anthropic cell reuses xlate's
# GW_ANTHROPIC_PATH so a manifest already wired for xlate needs nothing new.
ingress_path(){ case "$1" in
  openai)           echo "${GW_MATRIX_PATH_OPENAI:-$GW_PATH}";;
  openai-responses) echo "${GW_MATRIX_PATH_RESPONSES:-/v1/responses}";;
  anthropic)        echo "${GW_MATRIX_PATH_ANTHROPIC:-${GW_ANTHROPIC_PATH:-/v1/messages}}";;
  gemini)           echo "${GW_MATRIX_PATH_GEMINI:-/v1beta/models/$GW_MODEL:generateContent}";;
  cohere)           echo "${GW_MATRIX_PATH_COHERE:-/v2/chat}";;
  bedrock)          echo "${GW_MATRIX_PATH_BEDROCK:-/model/$GW_MODEL/converse}";;
esac; }

# The cohere v1 fallback some gateways mount instead of /v2/chat. A literal constant (no interpolated
# manifest state), so there is nothing here that can go stale.
P_COHERE_FB="/v1/chat"
