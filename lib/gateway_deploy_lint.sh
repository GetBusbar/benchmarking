#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# PRE-DEPLOY: every env var, header and command a gateway uses must be justified.
#
# The published methodology makes a promise about what is in a gateway's deployment:
#
#   A setting may appear only if it is needed to boot the process at all, to point an upstream at
#   the test mock instead of a real provider, to expose an ingress path the matrix exercises, or to
#   bind the port and cores the rig requires.
#
# That promise is what separates "a configuration an operator would deploy" from "a configuration
# tuned to win a benchmark", and an unchecked promise is just a sentence on a web page.
#
# There was already a lint for the CONFIG FILES. It read its claims out of GW_CONFIG_WHY inside each
# gateway.sh, and gateway.sh is dead code that nothing else loads. So the claims lived in a file the
# deployment no longer used, and the parts of the deployment that grew afterwards - the env block,
# the headers, and now the commands - were covered by nothing at all. That is how two gateways lost
# their entire env block in the port to the engine while every lint stayed green.
#
# This reads the live files only: env, headers.json, commands, and the claims in `why`.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 2

REASONS="boot upstream ingress bind"
fail=0

report() { # <gateway> <message>
  printf '  %-16s %s\n' "$1" "$2"
  fail=$((fail + 1))
}

# Claims for one gateway: "<pattern> <reason>" per line, comments and blanks skipped.
claims_of() {
  [ -f "gateways/$1/why" ] || return 0
  grep -vE '^\s*(#|$)' "gateways/$1/why" | awk '{print $1, $2}'
}

# Everything this gateway's deployment actually sets, one name per line.
#   env         -> the variable name, with a leading `-` (an unset) stripped
#   headers.json-> the header name, per egress column
#   commands    -> the line itself, named by its first word, because a command is not a key/value
used_by() {
  local g="$1"
  [ -f "gateways/$g/env" ] && grep -vE '^\s*(#|$)' "gateways/$g/env" | sed 's/=.*//; s/^-//'
  [ -f "gateways/$g/headers.json" ] && grep -oE '"[A-Za-z0-9_-]+: ' "gateways/$g/headers.json" 2>/dev/null | tr -d '": '
  [ -f "gateways/$g/commands" ] && grep -vE '^\s*(#|$)' "gateways/$g/commands" | awk '{print $1}' | sort -u
  return 0
}

for dir in gateways/*/; do
  g="$(basename "$dir")"
  [ -f "$dir/definition.json" ] || continue

  claims="$(claims_of "$g")"
  used="$(used_by "$g" | grep -vE '^\s*$' | sort -u)"

  # Every reason must be one of the four. A typo'd reason is a claim that means nothing.
  while read -r pat reason; do
    [ -n "${pat:-}" ] || continue
    case " $REASONS " in
      *" $reason "*) : ;;
      *) report "$g" "why: '$pat' claims reason '${reason:-<none>}', which is not one of: $REASONS" ;;
    esac
  done <<< "$claims"

  # Everything used must be claimed. A prefix claim ending in * covers a family.
  while read -r name; do
    [ -n "${name:-}" ] || continue
    ok=0
    while read -r pat _reason; do
      [ -n "${pat:-}" ] || continue
      case "$pat" in
        *'*') [ "${name#"${pat%\*}"}" != "$name" ] && ok=1 ;;
        *) [ "$name" = "$pat" ] && ok=1 ;;
      esac
    done <<< "$claims"
    [ "$ok" = 1 ] || report "$g" "'$name' is used but unjustified - add '$name <${REASONS// /|}>' to gateways/$g/why, or remove it"
  done <<< "$used"
done

if [ "$fail" -gt 0 ]; then
  echo
  echo "gateway_deploy_lint: $fail unjustified or malformed item(s)"
  exit 1
fi
echo "gateway_deploy_lint: every env var, header and command is justified against the published rule"
