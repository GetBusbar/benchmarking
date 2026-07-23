#!/usr/bin/env bash
# File the 11 approved maintainer-outreach issues as MattJackson (gh must be authed as MattJackson).
# APPROVED template (2026-07-23). Run ONLY after the live board is verified 100% (no asterisks,
# audit-clean, deep-links 200). Each issue links the gateway's gateway.sh + its results deep-link.
# Prints the created issue URL per repo; skips (does not crash) if a repo has issues disabled.
set -uo pipefail
GH_BASE="https://github.com/GetBusbar/benchmarking/blob/main/gateways"
SITE="https://onthebench.ai"
METHOD="$SITE/gateways/method"

body() { # $1=display $2=key  (single gateway)
cat <<EOF
Hi, I run [onthebench.ai]($SITE), an open benchmark that measures LLM-gateway overhead (latency, throughput, memory, streaming, protocol translation) on neutral hardware. **$1** is one of the gateways on the board, and I want to make sure I'm testing it fairly.

How it works, so there are no surprises:
- Every gateway runs on the **same rig, same mock upstream, same load, same CPU pinning**, no per-gateway special-casing.
- Each gateway is defined by a single file, [\`gateways/$2/gateway.sh\`]($GH_BASE/$2/gateway.sh), which declares how to build, launch, and probe it. That file is the whole story of how I configured yours.
- Every number regenerates from committed JSON; the [method is documented here]($METHOD) and the whole thing is open source and re-runnable.

My guiding rule is that **a failure is my bug until proven the gateway's**. If a cell shows red or a number looks off, I'd rather find out I configured **$1** wrong than publish something unfair. So two asks, both optional:
1. **Look at your results** ([your page]($SITE/gateways?gw=$2)) and your \`gateway.sh\`. If I've mis-set a flag, a version, an endpoint, or declared a capability you don't claim, tell me or open a PR, you're welcome to own your own \`gateway.sh\`.
2. If the setup looks fair, a thumbs-up is genuinely useful too.

Full disclosure: this is built and operated by the Busbar team, and busbar is also one of the entrants. Same harness for everyone, fully open, so you can verify exactly what I'm running.

Thanks for building **$1**.
EOF
}

litellm_body() {
cat <<EOF
Hi, I run [onthebench.ai]($SITE), an open benchmark that measures LLM-gateway overhead (latency, throughput, memory, streaming, protocol translation) on neutral hardware. LiteLLM appears on the board twice, the Python proxy and the Rust \`/v1/messages\` beta, each benchmarked separately. I want to make sure I'm testing both fairly.

How it works, so there are no surprises:
- Every gateway runs on the **same rig, same mock upstream, same load, same CPU pinning**, no per-gateway special-casing.
- Each entry is defined by a single file: [\`gateways/litellm-python/gateway.sh\`]($GH_BASE/litellm-python/gateway.sh) and [\`gateways/litellm-rust/gateway.sh\`]($GH_BASE/litellm-rust/gateway.sh). Those files are the whole story of how I configured them.
- Every number regenerates from committed JSON; the [method is documented here]($METHOD) and the whole thing is open source and re-runnable.

My guiding rule is that **a failure is my bug until proven the gateway's**. If a cell shows red or a number looks off, I'd rather find out I configured LiteLLM wrong than publish something unfair. So two asks, both optional:
1. **Look at your results** ([Python]($SITE/gateways?gw=litellm-python), [Rust]($SITE/gateways?gw=litellm-rust)) and the two \`gateway.sh\` files. If I've mis-set a flag, a version, an endpoint, or declared a capability you don't claim, tell me or open a PR, you're welcome to own your own \`gateway.sh\`.
2. If the setup looks fair, a thumbs-up is genuinely useful too.

Full disclosure: this is built and operated by the Busbar team, and busbar is also one of the entrants. Same harness for everyone, fully open, so you can verify exactly what I'm running.

Thanks for building LiteLLM.
EOF
}

file_one() { # $1=repo $2=display $3=key
  local title="onthebench.ai benchmarks $2: a review of our setup for fairness"
  echo "=== $1 ==="
  gh issue create -R "$1" --title "$title" --body "$(body "$2" "$3")" 2>&1 | tail -2
}

file_one "agentgateway/agentgateway" "agentgateway" "agentgateway"
file_one "apache/apisix"             "APISIX"       "apisix"
file_one "katanemo/archgw"           "Arch"         "arch"
file_one "maximhq/bifrost"           "Bifrost"      "bifrost"
file_one "ENTERPILOT/GOModel"        "GoModel"      "gomodel"
file_one "Helicone/ai-gateway"       "Helicone"     "helicone"
file_one "Kong/kong"                 "Kong"         "kong"
file_one "songquanpeng/one-api"      "One-API"      "one-api"
file_one "Portkey-AI/gateway"        "Portkey"      "portkey"
file_one "tensorzero/tensorzero"     "TensorZero"   "tensorzero"
# LiteLLM: one combined issue for both entries
echo "=== BerriAI/litellm (combined) ==="
gh issue create -R "BerriAI/litellm" --title "onthebench.ai benchmarks LiteLLM: a review of our setup for fairness" --body "$(litellm_body)" 2>&1 | tail -2
