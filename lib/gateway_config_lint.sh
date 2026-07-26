#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# CONFIG NECESSITY - the enforcement lint for the standard:
#
#     "Every gateway config is the bare minimum required to run. If we do not need a setting, it does
#      not go in the config. We do not turn features on and we do not turn them off. Whatever the
#      gateway ships as its default is what gets measured."
#
# WHY THIS FILE EXISTS. A manifest can silently accumulate settings nobody can still justify, including
# lines that only restate a default. A line that restates a default is still a line we wrote, and it
# becomes silently wrong the day upstream changes that default. Each such line is typically defensible
# in the commit that added it, which is exactly why prose review does not hold this line and a lint has
# to.
#
# The standard this enforces found a real memory bug: one gateway's default install has metrics on
# with nothing draining them, so RSS grows with every request served. That measurement only exists
# because the config was the default. Softening a measurement because of what it shows is backwards,
# so the rule is symmetric - nothing gets turned on either.
#
# ─────────────────────────────────────────────────────────────────────────────────────────────────
# THE RULE
#
# A setting is NECESSARY only if it is required to:
#
#   boot      1. run at all - a schema-mandatory field, a required listen address, a credential the
#                gateway demands before it will serve a request, or an external-store dependency we
#                cannot run on a single box.
#   upstream  2. point an upstream at the test mock instead of a real provider endpoint.
#   ingress   3. expose an ingress path the 6x6 matrix exercises.
#   bind      4. bind the port or the CPU pinning the harness requires.
#
# Every setting a manifest writes must be claimed by one of those four words, in that gateway's own
# `why` file (gateways/<name>/why - shared with lib/gateway_deploy_lint.sh, which claims env vars,
# headers and commands from the same file). A setting with no claim is a lint failure.
#
# THE FIELD IS DISCOVERED HERE TOO. This lint enumerates gateways/*/ at runtime and never carries a
# gateway name - a lint with a hardcoded roster would be the defect lib/gateway_isolation_test.sh
# exists to catch, reintroduced in the file meant to prevent rot.
#
# ─────────────────────────────────────────────────────────────────────────────────────────────────
# WHAT COUNTS AS A SETTING (extract_settings below)
#
# A manifest is shell, but the CONFIG it writes is YAML / TOML / JSON / env. The extractor keys on
# the shape of an assignment rather than on any one gateway's file format:
#
#   key:            a YAML mapping key, anywhere but a comment (also `- key:` inside a list, and the
#                   `"key: ` form that opens a quoted multi-line printf template)
#   "key":          a JSON object key
#   key = / key=    a TOML or env assignment, INSIDE A HEREDOC ONLY. Outside a heredoc that shape is
#                   ordinary shell (`BUSBAR_ADMIN_PORT=$((...))`, a `local`), which is rig plumbing,
#                   not a setting handed to the gateway.
#   -e KEY=         a docker environment flag, anywhere
#
# DELIBERATELY NOT SETTINGS:
#   * column-0 shell assignments - the manifest's own metadata and pins (GW_PORT, <NAME>_IMAGE,
#     <NAME>_COMMIT). Those describe WHAT is measured, not how it is configured, and they are already
#     governed by lib/gateway_isolation_test.sh.
#   * anything named GW_* - the harness contract (lib/gateways.sh, lib/ingress.sh), not gateway config.
#   * shell builtins and keywords appearing as the first token of a line.
#
# The extractor errs toward FLAGGING: a shape it is unsure about is reported, and the fix is one line
# in that gateway's own `why` file. It is not a parser for six config languages and does not need
# to be - it needs to make an unjustified setting impossible to add quietly.
#
# ─────────────────────────────────────────────────────────────────────────────────────────────────
# HOW A GATEWAY DECLARES ITS JUSTIFICATIONS - gateways/<name>/why:
#
#     listen        bind      # the harness drives this port
#     error_map     boot      # REQUIRED per provider, no serde default
#     base_url      upstream
#     mock-*        upstream  # trailing * is a prefix match
#
# One `<key-or-prefix> <reason>` per line; `#` starts a comment. A trailing `*` is a prefix match and
# needs at least three literal characters before it, so `*` alone cannot be used to wave the lint
# through. Reasons are exactly the four words above. `why` is shared with gateway_deploy_lint.sh, so
# it may also carry env/header/command claims that are not config-file keys - this lint ignores those.
#
# Run: bash lib/gateway_config_lint.sh   (no network, no docker; seconds)
#      RED demo: add an untagged setting to any manifest heredoc and re-run; it must fail.
# ─────────────────────────────────────────────────────────────────────────────────────────────────
set -u
B="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$B" || exit 1
. "$B/lib/gateways.sh"

FAILED=0
VIOLATIONS=0

REASONS="boot upstream ingress bind"

report() { # file message detail
  VIOLATIONS=$((VIOLATIONS+1)); FAILED=1
  printf '  FAIL %s  %s\n' "$1" "$2"
  [ -n "${3:-}" ] && printf '       | %s\n' "$(printf '%s' "$3" | cut -c1-120)"
  return 0
}

# ── extract_settings <manifest> -> one "<key>\t<lineno>\t<text>" per setting occurrence ──────────
# Factored out so the self-test can drive it with KNOWN-BAD and KNOWN-GOOD fixtures. A lint whose
# scanner silently matches nothing reports PASS, which is worse than having no lint at all.
extract_settings() { # manifest-path
  # TWO PASSES. The first collects the manifest's own column-0 shell assignments - its metadata and
  # its pins (<NAME>_IMAGE, <NAME>_COMMIT, <NAME>_SRC). Those names are then excluded EVERYWHERE,
  # including where gw_config echoes a pin into the published artifact: a pin says WHAT was measured,
  # which lib/gateway_isolation_test.sh already governs, not how it was configured.
  awk '
  function emit(k){ if (k=="" ) return; if (k ~ /^GW_/) return; if (k in SKIP) return
                    if (k in META) return
                    printf "%s\t%d\t%s\n", k, FNR, $0 }
  BEGIN{
    # shell keywords and builtins, plus the python keywords that appear in the inline `python3 -c`
    # helpers a couple of manifests use to parse a bootstrap response. Neither is a setting.
    split("local if then else elif fi for do done case esac while until return echo cat printf sudo \
           export set eval exit function declare read mkdir rm cp mv sleep source \
           try except finally def class import print pass raise with lambda", W, /[ \t\n]+/)
    for (i in W) if (W[i] != "") SKIP[W[i]] = 1
    inhd = 0
  }
  # heredoc bookkeeping, shared by both passes. The BODY of a heredoc is config bytes; the shell
  # around it is not. Both passes need this: a heredoc body is written at column 0, so without it the
  # metadata pass would mistake every launch-env line for a top-level shell assignment and exclude the
  # whole env from the scan.
  function hd(state, l, tt,   mk) {
    if (state) { if (tt == HDMARK[0]) return 0; return 1 }
    if (match(l, /<<-?[ \t]*['"'"'"]?[A-Za-z_][A-Za-z0-9_]*['"'"'"]?/)) {
      mk = substr(l, RSTART, RLENGTH)
      sub(/^<<-?[ \t]*/, "", mk); gsub(/['"'"'"]/, "", mk)
      HDMARK[0] = mk
      return 1
    }
    return 0
  }
  NR == FNR {
    t = $0; sub(/^[ \t]+/, "", t)
    was = inhd; inhd = hd(inhd, $0, t)
    if (was || inhd) next                    # inside (or opening) a heredoc: not shell metadata
    if ($0 ~ /^[A-Za-z_][A-Za-z0-9_]*=/) { m = $0; sub(/=.*$/, "", m); META[m] = 1 }
    next
  }
  FNR == 1 { inhd = 0 }                      # second pass starts outside every heredoc
  {
    line = $0
    t = line; sub(/^[ \t]+/, "", t)
    fw = t; sub(/[ \t].*$/, "", fw)
    was = inhd; inhd = hd(inhd, line, t)
    if (was && !inhd) next                   # the closing marker line itself

    if (t ~ /^#/) next                       # a comment is prose, never a setting. This check comes
                                             # BEFORE every rule below, including the docker -e scan:
                                             # a manifest that documents a flag it deliberately does
                                             # NOT pass must not be read as passing it.

    # ── a docker environment flag is a setting wherever it appears
    s = line
    while (match(s, /-e[ \t]+[A-Za-z_][A-Za-z0-9_]*[=]/)) {
      k = substr(s, RSTART, RLENGTH); sub(/^-e[ \t]+/, "", k); sub(/=$/, "", k)
      emit(k)
      s = substr(s, RSTART + RLENGTH)
    }

    # ── YAML / JSON mapping keys
    u = t
    sub(/^-[ \t]+/, "", u)                   # `- name: x` declares `name`
    if (u ~ /^"[A-Za-z_][A-Za-z0-9_.-]*"[ \t]*:/) {
      k = u; sub(/^"/, "", k); sub(/".*$/, "", k); emit(k); next
    }
    if (u ~ /^[A-Za-z_][A-Za-z0-9_.-]*[ \t]*:([ \t]|$)/) {
      k = u; sub(/[ \t]*:.*$/, "", k); emit(k); next
    }
    # The `"key: value"` quoted form, for the two places a config is NOT written as a heredoc: one
    # manifest renders its plugin block with a multi-line printf, and one gateway is configured
    # entirely by request headers held in a shell array. Restricted to those two shapes (a printf, or
    # a line that IS a quoted string) so it cannot mistake a diagnostic `echo "container: ..."` or a
    # curl bootstrap header for a setting.
    if ((fw == "printf" || t ~ /^"/) && line !~ /curl/ && match(line, /"[A-Za-z_][A-Za-z0-9_.-]*:[ \t]/)) {
      k = substr(line, RSTART + 1, RLENGTH - 3); emit(k); next
    }

    # ── TOML / env assignments, heredoc bodies only (outside one, this shape is plain shell)
    if (inhd && u ~ /^[A-Za-z_][A-Za-z0-9_.-]*[ \t]*=/) {
      k = u; sub(/[ \t]*=.*$/, "", k); emit(k); next
    }
  }' "$1" "$1"
}

# ── read_why <gateway-dir> -> one "<pattern>\t<reason>" per declared justification ─────────────────
# Reads gateways/<name>/why, the same claims file lib/gateway_deploy_lint.sh's claims_of() reads.
# `why` is a SUPERSET: it also carries env/header/command claims that are not this lint's domain.
# That is fine here - this lint only ever asks "does some pattern in `why` cover this config key",
# never "does every line in `why` belong to me".
read_why() { # gateway-dir
  [ -f "$1/why" ] || return 0
  grep -vE '^[ \t]*(#|$)' "$1/why" | awk '{print $1"\t"$2}'
}

# ── matches <key> <pattern> ──────────────────────────────────────────────────────────────────────
matches() { # key pattern
  case "$2" in
    *'*') case "$1" in "${2%\*}"*) return 0;; *) return 1;; esac ;;
    *)    [ "$1" = "$2" ] ;;
  esac
}

# ── check_manifest <gateway> <manifest-path> ─────────────────────────────────────────────────────
# The whole rule for one manifest, so the self-test can run it against a fixture directory.
check_manifest() { # gateway manifest-path
  local g="$1" m="$2" dir keys why pat reason k line n claimed
  dir="$(dirname "$m")"
  keys="$(extract_settings "$m" | cut -f1 | sort -u)"
  why="$(read_why "$dir")"

  if [ -z "$keys" ]; then
    # A manifest that writes no config at all (per-request header routing, an image that needs none)
    # has nothing to justify here.
    return 0
  fi

  if [ -z "$why" ]; then
    report "gateways/$g/why" "writes $(printf '%s\n' "$keys" | grep -c .) settings but gateways/$g/why does not exist or is empty" ""
    return 0
  fi

  # every declared reason must be one of the four
  while IFS=$'\t' read -r pat reason; do
    [ -n "$pat" ] || continue
    case " $REASONS " in
      *" $reason "*) ;;
      *) report "gateways/$g/why" "'$pat' claims reason '$reason', which is not one of: $REASONS" "$pat $reason" ;;
    esac
    case "$pat" in
      *'*') [ "${#pat}" -ge 4 ] || report "gateways/$g/why" "pattern '$pat' is too broad (a prefix needs at least three literal characters)" "$pat" ;;
    esac
  done <<< "$why"

  # every setting must be claimed
  while IFS= read -r k; do
    [ -n "$k" ] || continue
    claimed=0
    while IFS=$'\t' read -r pat reason; do
      [ -n "$pat" ] || continue
      if matches "$k" "$pat"; then claimed=1; break; fi
    done <<< "$why"
    if [ "$claimed" = 0 ]; then
      line="$(extract_settings "$m" | awk -F'\t' -v K="$k" '$1==K {print $2": "$3; exit}')"
      report "gateways/$g/why" "setting '$k' has no necessity claim - add '$k <${REASONS// /|}>' to gateways/$g/why, or delete the setting" "$line"
    fi
  done <<< "$keys"

  # NOTE: unlike this lint's previous behavior, a `why` entry matching no CONFIG-FILE setting is NOT
  # flagged stale here. `why` is a SHARED claims file also read by lib/gateway_deploy_lint.sh for
  # env/header/command claims (a domain this lint never scans) - a claim this lint can't match might
  # legitimately belong there instead. Mirrors gateway_deploy_lint.sh's own NOTE on the same tradeoff.
}

# ═════════════════════════════════════════════════════════════════════════════════════════════════
# SELF-TEST: the extractor must be PROVEN to fire before a clean run means anything.
# Two layers, mirroring lib/gateway_isolation_test.sh:
#   1. the extractor, driven directly with a synthetic manifest whose every shape is known;
#   2. an ACCEPTANCE test that drops a real fixture gateway DIRECTORY into gateways/ and requires the
#      lint's own discovery + check to fail on it, then to pass once the fixture declares its
#      justifications. That proves discovery, extraction and reporting are wired to each other, not
#      just that each works alone.
# ═════════════════════════════════════════════════════════════════════════════════════════════════
selftest_extractor() {
  local f out
  f="$(mktemp "${TMPDIR:-/tmp}/gwcfg.XXXXXX")"
  cat > "$f" <<'FIXTURE'
#!/usr/bin/env bash
GW_PORT=9999
SOMEGATEWAY_IMAGE="someimage:1"
# commented_key: not a setting
_fx_write() {
  cat > "$D/config.gen.yaml" <<YAML
listen: "0.0.0.0:$GW_PORT"
providers:
  - name: mock
    base_url: http://127.0.0.1:$MOCK_PORT
YAML
  cat > "$D/c.toml" <<TOML
enabled = false
TOML
}
_fx_run() {
  local scratch=1
  SCRATCH_VAR=2
  docker run -e SOME_KEY=dummy -e OTHER_KEY=x img
}
FIXTURE
  out="$(extract_settings "$f" | cut -f1 | sort -u | tr '\n' ' ')"
  rm -f "$f"
  # present: the YAML keys, the list-item key, the TOML key, both docker -e flags
  local want
  for want in listen providers name base_url enabled SOME_KEY OTHER_KEY; do
    case " $out " in *" $want "*) ;; *) echo "gateway_config_lint: FAIL - self-test: extractor missed '$want' (got: $out)"; exit 1 ;; esac
  done
  # absent: column-0 shell metadata, the GW_ contract, shell locals, a commented key
  for want in GW_PORT SOMEGATEWAY_IMAGE scratch SCRATCH_VAR commented_key local; do
    case " $out " in *" $want "*) echo "gateway_config_lint: FAIL - self-test: extractor wrongly reported '$want' (got: $out)"; exit 1 ;; esac
  done
  echo "gateway_config_lint: self-test ok - the extractor finds every setting shape and no shell plumbing"
}

# The acceptance test plants a REAL gateway directory, so it exercises gw_list discovery too.
FIXTURE_DIR=""
cleanup_fixture() { [ -n "$FIXTURE_DIR" ] && rm -rf "$FIXTURE_DIR"; FIXTURE_DIR=""; }
trap cleanup_fixture EXIT INT TERM

selftest_acceptance() {
  local name out before after
  name="zz-lint-fixture-$$"
  FIXTURE_DIR="$B/gateways/$name"
  mkdir -p "$FIXTURE_DIR"
  # RED: one justified setting, one that nobody claimed.
  cat > "$FIXTURE_DIR/gateway.sh" <<'FIXTURE'
#!/usr/bin/env bash
GW_KIND=docker
GW_PORT=9999
_fx_write() {
  cat > "$GW_DIR/config.gen.yaml" <<YAML
listen: "0.0.0.0:$GW_PORT"
emit_vanity_header: true
YAML
}
FIXTURE
  cat > "$FIXTURE_DIR/why" <<'FIXTURE'
listen   bind
FIXTURE
  # discovery must see the fixture at all
  gw_list "$B" | grep -qx "$name" || { echo "gateway_config_lint: FAIL - self-test: gw_list did not discover the planted fixture"; exit 1; }
  before="$VIOLATIONS"
  check_manifest "$name" "$FIXTURE_DIR/gateway.sh" >/dev/null
  after="$VIOLATIONS"
  [ "$after" -gt "$before" ] || { echo "gateway_config_lint: FAIL - self-test: the lint did NOT fail on an unjustified setting"; exit 1; }
  # GREEN: claim it, and the same manifest must come back clean.
  cat > "$FIXTURE_DIR/why" <<'FIXTURE'
listen             bind
emit_vanity_header boot
FIXTURE
  before="$VIOLATIONS"
  check_manifest "$name" "$FIXTURE_DIR/gateway.sh" >/dev/null
  [ "$VIOLATIONS" = "$before" ] || { echo "gateway_config_lint: FAIL - self-test: a fully claimed fixture still failed"; exit 1; }
  # `why` is shared with gateway_deploy_lint.sh's domain (env/headers/commands): a claim here that
  # matches no config-file setting must NOT be flagged, since it may legitimately belong there instead.
  cat > "$FIXTURE_DIR/why" <<'FIXTURE'
listen             bind
emit_vanity_header boot
GW_SOME_ENV_VAR     boot
FIXTURE
  before="$VIOLATIONS"
  check_manifest "$name" "$FIXTURE_DIR/gateway.sh" >/dev/null
  [ "$VIOLATIONS" = "$before" ] || { echo "gateway_config_lint: FAIL - self-test: a why claim outside this lint's domain (config-file settings) must not be flagged"; exit 1; }
  cleanup_fixture
  # the counter must not carry the self-test's deliberate violations into the real run
  FAILED=0; VIOLATIONS=0
  echo "gateway_config_lint: acceptance ok - a planted gateway directory is discovered, fails unjustified, passes justified, and does not false-flag an out-of-domain claim"
}

# ═════════════════════════════════════════════════════════════════════════════════════════════════
# `--settings <gateway>` prints what the extractor sees for one manifest. Not decoration: the first
# question anyone asks when this lint fails is "what does it think a setting IS here", and answering
# it by re-deriving the awk in a scratch file is how a second, drifting copy of the rule gets born.
if [ "${1:-}" = "--settings" ]; then
  gw_exists "${2:-}" "$B" || { echo "gateway_config_lint: no gateways/${2:-}/gateway.sh" >&2; exit 1; }
  extract_settings "$B/gateways/$2/gateway.sh" | sort -u -k1,1 | awk -F'\t' '{printf "%-28s line %-5s %s\n", $1, $2, substr($3,1,80)}'
  exit 0
fi

mapfile -t GWS < <(gw_list "$B")
[ "${#GWS[@]}" -gt 0 ] || { echo "gateway_config_lint: FAIL - no gateways/*/gateway.sh found"; exit 1; }
echo "gateway_config_lint: field discovered from gateways/*/gateway.sh - ${#GWS[@]} gateways"
echo "gateway_config_lint: a setting is necessary only to $REASONS - everything else comes out"

selftest_extractor
selftest_acceptance

# discovery again: the fixture is gone, so this is the real field
mapfile -t GWS < <(gw_list "$B")
for g in "${GWS[@]}"; do
  check_manifest "$g" "$B/gateways/$g/gateway.sh"
done

echo
if [ "$FAILED" = 0 ]; then
  echo "gateway_config_lint: PASS - every setting in every manifest is claimed by one of the four necessity reasons."
  echo "  Nothing is turned on, nothing is turned off, and nothing merely restates a default."
else
  cat <<'MSG'
gateway_config_lint: FAIL

  Every setting a manifest writes must be required to boot the gateway, to point an upstream at the
  mock, to expose an ingress path the matrix drives, or to bind the port / CPU pin the harness needs.

  How to fix, in order of preference:
    1. DELETE it.   If the gateway runs without it, it does not belong in the config. That includes a
                    line whose value equals the application's own default: we did not need to write
                    it, and it becomes wrong the day upstream changes that default.
    2. CLAIM it.    If the gateway genuinely will not run without it, add
                    `<key> <boot|upstream|ingress|bind>` to gateways/<name>/why, and put the
                    evidence in a comment next to the setting - the failure it prevents, not a
                    rationale for keeping it.

  Do NOT claim a setting you have not tested without. "It was probably needed" is how the list grew.
  Do NOT turn a feature on, and do NOT turn one off, in either direction, for any reason: the
  benchmark measures genuine default installs, and a default that looks bad is a finding, not a bug
  in the measurement.
MSG
fi
echo "gateway_config_lint: $VIOLATIONS violation(s)"
exit "$FAILED"
