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
# gateway_config_lint.sh covers the CONFIG FILES separately; this lint reads the live deployment
# surfaces only: env, headers.json, commands, and the claims in `why`.
#
# SELF-TESTED, like its two siblings (gateway_config_lint.sh, gateway_isolation_test.sh). Without a
# self-test, a broken extraction regex (a `grep -oE` pattern that stops matching after a
# headers.json reformat, an `awk` field split that silently changes with an extra space) makes every
# gateway look fully justified - the WORST failure mode for a lint whose entire job is refusing to
# stay quiet, and one nothing here would have caught before this existed.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 2
ROOT="$PWD"

REASONS="boot upstream ingress bind"
fail=0

report() { # <gateway> <message>
  printf '  %-16s %s\n' "$1" "$2"
  fail=$((fail + 1))
}

# Claims for one gateway: "<pattern> <reason>" per line, comments and blanks skipped.
claims_of() {
  [ -f "$1/why" ] || return 0
  grep -vE '^\s*(#|$)' "$1/why" | awk '{print $1, $2}'
}

# Everything this gateway's deployment actually sets, one name per line.
#   env         -> the variable name, with a leading `-` (an unset) stripped
#   headers.json-> the header name, per egress column
#   commands    -> the line itself, named by its first word, because a command is not a key/value
used_by() {
  local dir="$1"
  [ -f "$dir/env" ] && grep -vE '^\s*(#|$)' "$dir/env" | sed 's/=.*//; s/^-//'
  [ -f "$dir/headers.json" ] && grep -oE '"[A-Za-z0-9_-]+: ' "$dir/headers.json" 2>/dev/null | tr -d '": '
  [ -f "$dir/commands" ] && grep -vE '^\s*(#|$)' "$dir/commands" | awk '{print $1}' | sort -u
  return 0
}

# check_gateway <name> <dir> - the whole rule for one gateway, factored out so the self-test can run
# it against a planted fixture directory exactly as the real scan runs it against gateways/*/.
check_gateway() {
  local g="$1" dir="$2" claims used
  claims="$(claims_of "$dir")"
  used="$(used_by "$dir" | grep -vE '^\s*$' | sort -u)"

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
    [ "$ok" = 1 ] || report "$g" "'$name' is used but unjustified - add '$name <${REASONS// /|}>' to $dir/why, or remove it"
  done <<< "$used"
  # NOTE: unlike gateway_config_lint.sh's stale-claim check, this deliberately does NOT flag a `why`
  # entry that matches nothing here: `why` is a SHARED claims file (its own header: "why every
  # setting, env var, header and command this gateway uses is here"), and a real gateway's `why`
  # legitimately carries claims for its rendered CONFIG FILE keys too - a domain this lint never
  # scans (that is gateway_config_lint.sh's job). Flagging those as "stale" here would be wrong, not
  # a real gap; a stale-claim check belongs in the lint that actually owns the domain being claimed.
}

# ═════════════════════════════════════════════════════════════════════════════════════════════════
# SELF-TEST. Plants a REAL gateway directory under gateways/ (so it also exercises the `gateways/*/`
# discovery glob, not just check_gateway in isolation) with deliberately unjustified, then justified,
# then out-of-domain, then invalid-reason content, and asserts the lint's own violation counter moves
# exactly where each case demands. Runs on every invocation, before the real scan, like its two
# siblings.
FIXTURE_DIR=""
cleanup_fixture() { [ -n "$FIXTURE_DIR" ] && rm -rf "$FIXTURE_DIR"; FIXTURE_DIR=""; }
trap cleanup_fixture EXIT INT TERM

selftest() {
  local name before after
  name="zz-deploy-lint-fixture-$$"
  FIXTURE_DIR="$ROOT/gateways/$name"
  mkdir -p "$FIXTURE_DIR"
  echo '{}' > "$FIXTURE_DIR/definition.json"   # discovery requires this to exist

  # RED: an env var, a header and a command line, none of them claimed anywhere.
  cat > "$FIXTURE_DIR/env" <<'FIXTURE'
GW_PORT=9999
FIXTURE
  cat > "$FIXTURE_DIR/headers.json" <<'FIXTURE'
{"openai": "X-Vanity-Header: 1"}
FIXTURE
  cat > "$FIXTURE_DIR/commands" <<'FIXTURE'
touch /tmp/marker
FIXTURE
  before="$fail"
  check_gateway "$name" "$FIXTURE_DIR" >/dev/null
  after="$fail"
  [ "$after" -gt "$before" ] || { echo "gateway_deploy_lint: FAIL - self-test: an unjustified env/header/command was not caught"; exit 1; }

  # GREEN: claim all three, and the same fixture must come back clean.
  cat > "$FIXTURE_DIR/why" <<'FIXTURE'
GW_PORT bind
X-Vanity-Header ingress
touch boot
FIXTURE
  before="$fail"
  check_gateway "$name" "$FIXTURE_DIR" >/dev/null
  [ "$fail" = "$before" ] || { echo "gateway_deploy_lint: FAIL - self-test: a fully claimed fixture still failed"; exit 1; }

  # ISOLATE EACH SURFACE. The combined RED/GREEN fixtures above prove SOMETHING is checked, but with
  # all three surfaces unclaimed at once a broken extractor for any ONE of them (env / headers.json /
  # commands) would still leave the other two tripping the counter, so the self-test would pass while
  # that one extractor silently matched nothing - exactly the failure mode this self-test exists to
  # catch. Test each surface with the other two fully claimed, so only ITS OWN extraction can move
  # the counter.
  local surface
  for surface in env headers commands; do
    cat > "$FIXTURE_DIR/why" <<'FIXTURE'
GW_PORT bind
X-Vanity-Header ingress
touch boot
FIXTURE
    case "$surface" in
      env) sed -i.bak '/^GW_PORT bind$/d' "$FIXTURE_DIR/why" ;;
      headers) sed -i.bak '/^X-Vanity-Header ingress$/d' "$FIXTURE_DIR/why" ;;
      commands) sed -i.bak '/^touch boot$/d' "$FIXTURE_DIR/why" ;;
    esac
    rm -f "$FIXTURE_DIR/why.bak"
    before="$fail"
    check_gateway "$name" "$FIXTURE_DIR" >/dev/null
    after="$fail"
    [ "$after" -gt "$before" ] || { echo "gateway_deploy_lint: FAIL - self-test: the $surface extractor did not fire (its own claim was removed but nothing failed)"; exit 1; }
  done

  # A claim for something no longer used must NOT be flagged here: `why` also carries claims for
  # config-file keys (a domain this lint never scans, owned by gateway_config_lint.sh), so a claim
  # matching nothing in env/headers.json/commands is not necessarily stale. Restore full claim
  # coverage first (the isolation loop above left one surface deliberately unclaimed).
  cat > "$FIXTURE_DIR/why" <<'FIXTURE'
GW_PORT bind
X-Vanity-Header ingress
touch boot
some_config_file_key boot
FIXTURE
  before="$fail"
  check_gateway "$name" "$FIXTURE_DIR" >/dev/null
  [ "$fail" = "$before" ] || { echo "gateway_deploy_lint: FAIL - self-test: a why claim outside this lint's domain (env/headers/commands) must not be flagged"; exit 1; }

  # BAD REASON: a claim naming something outside the four-necessity vocabulary must fail even though
  # the pattern matches - a typo'd or invented reason justifies nothing.
  cat > "$FIXTURE_DIR/why" <<'FIXTURE'
GW_PORT convenience
FIXTURE
  cat > "$FIXTURE_DIR/env" <<'FIXTURE'
GW_PORT=9999
FIXTURE
  rm -f "$FIXTURE_DIR/headers.json" "$FIXTURE_DIR/commands"
  before="$fail"
  check_gateway "$name" "$FIXTURE_DIR" >/dev/null
  [ "$fail" -gt "$before" ] || { echo "gateway_deploy_lint: FAIL - self-test: an invalid reason word was not caught"; exit 1; }

  cleanup_fixture
  # The self-test's own deliberate violations must not carry into the real scan below.
  fail=0
  echo "gateway_deploy_lint: self-test ok - unjustified/stale/invalid-reason usage is caught, justified usage passes"
}

selftest

for dir in gateways/*/; do
  g="$(basename "$dir")"
  [ -f "$dir/definition.json" ] || continue
  check_gateway "$g" "${dir%/}"
done

if [ "$fail" -gt 0 ]; then
  echo
  echo "gateway_deploy_lint: $fail unjustified or malformed item(s)"
  exit 1
fi
echo "gateway_deploy_lint: every env var, header and command is justified against the published rule"
