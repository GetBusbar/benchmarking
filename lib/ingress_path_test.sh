#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Regression guard for the INGRESS-PATH choke point (lib/ingress.sh).
#
# THE BUG THIS EXISTS FOR (harness bug; it corrupted a published board): matrix/run.sh built the
# per-ingress probe paths ONCE, at script init, into globals that ingress_path() echoed back:
#
#     P_GEMINI="${GW_MATRIX_PATH_GEMINI:-/v1beta/models/$GW_MODEL:generateContent}"
#     P_BEDROCK="${GW_MATRIX_PATH_BEDROCK:-/model/$GW_MODEL/converse}"
#
# gemini and bedrock are the ONLY two dialects that carry the model in the URL PATH (every other
# dialect puts "model" in the BODY, which ingress_body() re-expands per call). Since 02345b4 several
# manifests switch GW_MODEL PER EGRESS COLUMN inside gw_matrix_egress instead of rewriting the gateway
# config — so those two paths kept requesting the model from BEFORE the switch, which routes to the
# openai provider. {gemini,bedrock} x {every non-openai egress} became impossible to pass: busbar
# published exactly 10 such reds ("the mock received the request on the openai endpoint instead"),
# one gateway published HTTP 400 on /v1beta/models/gpt-4o-mini:generateContent and HTTP 404 on
# /model/gpt-4o-mini/converse. gemini->openai and bedrock->openai still passed — the frozen model
# genuinely was the openai one — which is why the failure looked like a gateway limitation.
#
# THE CONTRACT under test:
#   1. ingress_path resolves its defaults at CALL time: after GW_MODEL changes (what gw_matrix_egress
#      does), gemini + bedrock paths contain the NEW model, and never the old one.
#   2. an explicit GW_MATRIX_PATH_* manifest override still wins, and is NOT model-rewritten.
#   3. the same lateness holds for every other dialect's manifest-supplied default (GW_PATH,
#      GW_ANTHROPIC_PATH) — the whole family, not just the two that bit us.
#   4. run.sh keeps exactly ONE definition of these paths (no re-frozen P_* global sneaking back).
#
# Run: bash lib/ingress_path_test.sh   (no network, no docker, no gateway; instant)
set -u
B="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

FAILED=0
check(){ # label expected actual
  if [ "$2" = "$3" ]; then
    printf '  ok   %s\n' "$1"
  else
    printf '  FAIL %s: expected [%s] got [%s]\n' "$1" "$2" "$3"; FAILED=1
  fi
}
contains(){ # label needle haystack
  case "$3" in
    *"$2"*) printf '  ok   %s\n' "$1";;
    *) printf '  FAIL %s: [%s] does not contain [%s]\n' "$1" "$3" "$2"; FAILED=1;;
  esac
}
lacks(){ # label needle haystack
  case "$3" in
    *"$2"*) printf '  FAIL %s: [%s] still contains [%s]\n' "$1" "$3" "$2"; FAILED=1;;
    *) printf '  ok   %s\n' "$1";;
  esac
}

# The manifest baseline, exactly as gateways/<gw>/gateway.sh declares it before any column runs.
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
# shellcheck source=/dev/null
source "$B/lib/ingress.sh"

echo "ingress_path_test: baseline (the manifest's declared GW_MODEL)"
check "openai           -> GW_PATH"     "/v1/chat/completions"                          "$(ingress_path openai)"
check "openai-responses -> canonical"   "/v1/responses"                                 "$(ingress_path openai-responses)"
check "anthropic        -> canonical"   "/v1/messages"                                  "$(ingress_path anthropic)"
check "gemini           -> model in path" "/v1beta/models/gpt-4o-mini:generateContent"  "$(ingress_path gemini)"
check "cohere           -> canonical"   "/v2/chat"                                      "$(ingress_path cohere)"
check "bedrock          -> model in path" "/model/gpt-4o-mini/converse"                 "$(ingress_path bedrock)"

# THE REGRESSION. This is precisely what gw_matrix_egress does for the gemini column of a
# one-config gateway (busbar: `gemini) GW_MODEL=gpt-4o-mini-gemini`).
echo "ingress_path_test: after gw_matrix_egress flips GW_MODEL — paths must follow"
GW_MODEL=gpt-4o-mini-gemini
check "gemini  follows the new model" "/v1beta/models/gpt-4o-mini-gemini:generateContent" "$(ingress_path gemini)"
lacks "gemini  drops the stale model" "models/gpt-4o-mini:"                               "$(ingress_path gemini)"
GW_MODEL=gpt-4o-mini-bedrock
check "bedrock follows the new model" "/model/gpt-4o-mini-bedrock/converse"               "$(ingress_path bedrock)"
lacks "bedrock drops the stale model" "/model/gpt-4o-mini/converse"                       "$(ingress_path bedrock)"

# Every one of the six columns in turn, on the shape one gateway uses (suffix per column). A frozen
# path passes column 1 (openai, model unchanged) and fails all five others — the exact published
# signature: gemini->openai / bedrock->openai green, the other ten red.
echo "ingress_path_test: all six egress columns, one after another (no cross-column bleed)"
GW_MODEL_BASE=gpt-4o-mini
for col in openai openai-responses anthropic gemini cohere bedrock; do
  case "$col" in
    openai) GW_MODEL="$GW_MODEL_BASE";;
    *)      GW_MODEL="$GW_MODEL_BASE-$col";;
  esac
  contains "column $col: gemini  path carries $GW_MODEL"  "/v1beta/models/$GW_MODEL:generateContent" "$(ingress_path gemini)"
  contains "column $col: bedrock path carries $GW_MODEL"  "/model/$GW_MODEL/converse"                "$(ingress_path bedrock)"
done

echo "ingress_path_test: an explicit GW_MATRIX_PATH_* manifest override still wins"
GW_MODEL=some-other-model
GW_MATRIX_PATH_GEMINI=/custom/gemini/endpoint
GW_MATRIX_PATH_BEDROCK=/custom/bedrock/endpoint
GW_MATRIX_PATH_OPENAI=/custom/openai
GW_MATRIX_PATH_RESPONSES=/custom/responses
GW_MATRIX_PATH_ANTHROPIC=/custom/anthropic
GW_MATRIX_PATH_COHERE=/custom/cohere
check "override gemini    wins" "/custom/gemini/endpoint"  "$(ingress_path gemini)"
check "override bedrock   wins" "/custom/bedrock/endpoint" "$(ingress_path bedrock)"
check "override openai    wins" "/custom/openai"           "$(ingress_path openai)"
check "override responses wins" "/custom/responses"        "$(ingress_path openai-responses)"
check "override anthropic wins" "/custom/anthropic"        "$(ingress_path anthropic)"
check "override cohere    wins" "/custom/cohere"           "$(ingress_path cohere)"
GW_MODEL=changed-again
check "override is NOT model-rewritten" "/custom/gemini/endpoint" "$(ingress_path gemini)"
unset GW_MATRIX_PATH_GEMINI GW_MATRIX_PATH_BEDROCK GW_MATRIX_PATH_OPENAI \
      GW_MATRIX_PATH_RESPONSES GW_MATRIX_PATH_ANTHROPIC GW_MATRIX_PATH_COHERE

echo "ingress_path_test: the rest of the family is late-bound too (GW_PATH / GW_ANTHROPIC_PATH)"
GW_PATH=/router/default/chat/completions
GW_ANTHROPIC_PATH=/anthropic/v1/messages
check "openai    reflects a later GW_PATH"           "/router/default/chat/completions" "$(ingress_path openai)"
check "anthropic reflects a later GW_ANTHROPIC_PATH" "/anthropic/v1/messages"           "$(ingress_path anthropic)"
unset GW_ANTHROPIC_PATH
check "anthropic falls back with no manifest path"   "/v1/messages"                     "$(ingress_path anthropic)"

echo "ingress_path_test: cohere v1 fallback constant is intact"
check "P_COHERE_FB" "/v1/chat" "$P_COHERE_FB"

echo "ingress_path_test: ONE choke point — no eagerly-frozen path global back in matrix/run.sh"
# The standing rule: one definition, not N. A re-introduced `P_GEMINI=.../$GW_MODEL/...` style global
# in run.sh is the bug coming back, so fail on any assignment there that bakes $GW_MODEL into a path.
STALE="$(grep -nE '^[[:space:]]*P_[A-Z_]+=.*\$\{?GW_MODEL' "$B/matrix/run.sh" || true)"
check "no init-time \$GW_MODEL path global in matrix/run.sh" "" "$STALE"
DEFS="$(grep -cE '^[[:space:]]*ingress_path\(\)' "$B/matrix/run.sh" "$B/lib/ingress.sh" | awk -F: '{s+=$2} END{print s}')"
check "exactly one ingress_path() definition" "1" "$DEFS"

[ "$FAILED" = 0 ] && echo "ingress_path_test: ALL PASS" || echo "ingress_path_test: FAILURES"
exit "$FAILED"
