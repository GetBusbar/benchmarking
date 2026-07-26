#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# GATEWAY ISOLATION - the enforcement lint for the structural invariant:
#
#     "Adding a gateway is exactly: drop in gateways/<name>/gateway.sh, and it works."
#
# A gateway's identity must appear NOWHERE in this repo except inside its own gateways/<name>/
# directory. No harness file, no site file, no chart script, no test may name a specific gateway or
# iterate a frozen list of them. The engine discovers: lib/box_qualify.sh finds each box's own
# history in results/snapshots/, site/gen-data.mjs projects whatever results/matrix/*.json exists,
# run-all.sh and run-on-ec2.sh fan out over gateways/*/, and lib/gateways.sh is the shell-side
# discovery choke point. Anything that names a gateway is a DEVIATION from that engine, and every
# such deviation is a place a new gateway silently does not work.
#
# THE FIELD IS DISCOVERED HERE TOO. This lint enumerates gateways/*/ at runtime. It NEVER carries a
# list of gateway names - a lint with a hardcoded roster would be the very defect it guards.
#
# ─────────────────────────────────────────────────────────────────────────────────────────────────
# TWO RULES
#
#   RULE 1  IDENTITY.    A gateway's name (or a declared alias of it) must not appear in the CODE of
#                        any scanned source file outside gateways/<that-name>/.
#   RULE 2  ENUMERATION. No single line anywhere may name THREE OR MORE distinct gateways. Three
#                        names on one line is a hardcoded roster whatever the syntax, and a roster is
#                        the same defect as a name: it freezes the field at the moment it was typed.
#                        This rule applies to comments and prose too, because a comment that lists
#                        the field goes stale the moment a directory is added or removed.
#
# WHAT "CODE" MEANS (rule 1). Comments are stripped before rule 1 runs, deliberately and narrowly: a
# comment recording provenance for a threshold (e.g. which box produced a measurement) cannot make a
# newly dropped-in gateway fail to run, so it is exempt. What CAN break a drop-in is executable code and
# user-visible strings, and those are scanned in full. Rule 2 still applies to comments, so provenance
# may cite a specific case but may never re-grow into a roster.
#
# ALLOWLIST (see GW_ISO_ALLOW below) - narrow, and each entry carries its reason. It covers exactly
# the operator's OWN identity (copyright line, GitHub org, domain, the site's neutrality disclosure),
# which collides with one entrant's name because the operator is also an entrant. That collision is
# disclosed on the site on purpose and cannot be renamed away.
#
# Run: bash lib/gateway_isolation_test.sh   (no network, no docker; seconds)
#      RED demo: plant a gateway name in a harness file and re-run; it must fail.
# ─────────────────────────────────────────────────────────────────────────────────────────────────
set -u
B="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$B" || exit 1
. "$B/lib/gateways.sh"

FAILED=0
VIOLATIONS=0

# ── the field, discovered ────────────────────────────────────────────────────────────────────────
mapfile -t GWS < <(gw_list "$B")
[ "${#GWS[@]}" -gt 0 ] || { echo "gateway_isolation_test: FAIL - no gateways/*/gateway.sh found"; exit 1; }

# Aliases a gateway is ALSO known by, declared by the gateway ITSELF in its own manifest, never by
# this lint. GW_DISPLAY is the label the charts print; GW_CONTAINER is the docker name it launches
# under. Both are identity: if either leaks into a shared file, that file is coupled to one gateway.
# A manifest that declares neither simply contributes no alias.
gw_aliases() { # gateway
  local a
  printf '%s\n' "$1"
  for a in GW_DISPLAY GW_CONTAINER; do
    v="$(gw_manifest_field "$1" "$a" "$B")"
    case "$v" in ""|*'$'*) continue ;; esac
    printf '%s\n' "$v"
  done
}

# ── what gets scanned ────────────────────────────────────────────────────────────────────────────
# Executable + config sources. Markdown is documentation, not engine: a README table cannot stop a
# drop-in from working, and gateways/README.md documents the contract itself. JSON under site/ and
# gateways/ is GENERATED from results (site/data.json, gateways/stars.json) - it is measurement
# output keyed by gateway, which is the whole point of the board.
scan_files() {
  git ls-files -z 2>/dev/null | tr '\0' '\n' | while IFS= read -r f; do
    [ -f "$f" ] || continue
    case "$f" in
      results/*|site/charts/*|site/data.json|gateways/stars.json|bin/*|assets/*|site/fonts/*) continue ;;
      *.md|*.json|*.svg|*.png|*.woff2|*.ttf|*.lock) continue ;;
    esac
    case "$f" in
      *.sh|*.bash|*.py|*.mjs|*.js|*.go|*.rs|*.html|*.yml|*.yaml|*.toml|*.env|*.mod|.gitignore) printf '%s\n' "$f" ;;
    esac
  done
}

# ── comment stripping (rule 1 only) ──────────────────────────────────────────────────────────────
# Line-oriented and deliberately conservative: it removes whole-line and trailing comments in the
# languages this repo uses. A block comment's interior lines are left in place rather than tracked
# with a state machine, so the lint errs toward FLAGGING, never toward missing a violation.
strip_comments() { # file
  case "$1" in
    *.sh|*.bash|*.py|*.yml|*.yaml|*.toml|*.env|.gitignore)
      sed -e 's/[[:space:]]*#.*$//' "$1" ;;
    *.mjs|*.js|*.go|*.rs|*.mod)
      sed -e 's#[[:space:]]*//.*$##' -e 's#/\*.*\*/##' "$1" ;;
    *.html)
      sed -e 's/<!--.*-->//' "$1" ;;
    *) cat "$1" ;;
  esac
}

# ── the allowlist ────────────────────────────────────────────────────────────────────────────────
# One extended regex, alternated. Every branch is the OPERATOR'S identity, not a gateway entry.
# The operator (Busbar Inc) built and runs this benchmark AND is one of the entrants; the site
# discloses exactly that. Its copyright header, its GitHub org, its domain and its team name are not
# a gateway reference and cannot be removed or renamed. Nothing gateway-specific belongs here: if a
# future entry is added to this list, it must come with the same kind of reason.
# Written in plain POSIX ERE with no interval expressions, because it is handed to awk as a dynamic
# regex and the awk on a BSD host does not have to support {n}.
GW_ISO_ALLOW='Copyright \(C\) [0-9][0-9][0-9][0-9] Busbar Inc|github\.com/GetBusbar/|api\.github\.com/repos/GetBusbar/|raw\.githubusercontent\.com/GetBusbar/|getbusbar\.com|Busbar team|Busbar Inc'

allowlisted() { printf '%s' "$1" | grep -Eq "$GW_ISO_ALLOW"; }

report() { # file line rule message text
  VIOLATIONS=$((VIOLATIONS+1)); FAILED=1
  printf '  FAIL %s:%s  [%s] %s\n' "$1" "$2" "$3" "$4"
  printf '       | %s\n' "$(printf '%s' "$5" | cut -c1-120)"
}

# ═════════════════════════════════════════════════════════════════════════════════════════════════
echo "gateway_isolation_test: field discovered from gateways/*/gateway.sh - ${#GWS[@]} gateways"

# Build the alias table once: "<gw>\t<alias>" per line.
ALIASES="$(for g in "${GWS[@]}"; do gw_aliases "$g" | while IFS= read -r a; do printf '%s\t%s\n' "$g" "$a"; done; done)"

# ── the scan ─────────────────────────────────────────────────────────────────────────────────────
# ONE awk pass over the whole tree, both rules at once. Doing this as nested shell loops with a grep
# per (file, line, gateway) is O(files x lines x gateways) processes and takes minutes; in-process
# awk regexes take about a second, which is what makes this cheap enough to run on every push.
#
# The file is read twice per line, deliberately: rule 1 sees the line with comments stripped, rule 2
# sees it whole (a roster in a comment is still a roster).
SCAN="$(scan_files)"
[ -n "$SCAN" ] || { echo "gateway_isolation_test: FAIL - no source files matched the scan"; exit 1; }

echo "gateway_isolation_test: RULE 1 - no gateway identity in code outside its own directory"
echo "gateway_isolation_test: RULE 2 - no line enumerates three or more gateways"
echo "gateway_isolation_test: RULE 1B - no gateway is named in a COMMENT outside its own directory"

# Emit "<file>\t<lineno>\t<raw>\t<code>" for every line of every scanned file, so awk needs no
# per-file knowledge of comment syntax.
feed() {
  local f
  while IFS= read -r f; do
    [ -n "$f" ] || continue
    paste -d'\t' <(printf '%s\n' "$(strip_comments "$f")" | sed 's/\t/ /g') \
                 <(sed 's/\t/ /g' "$f") 2>/dev/null |
      awk -v F="$f" -F'\t' '{ printf "%s\t%d\t%s\t%s\n", F, NR, $2, $1 }'
  done <<< "$SCAN"
}

# The tables go through the ENVIRONMENT, not through `awk -v`: -v runs its value through escape
# processing and cannot carry a newline at all (a BSD awk aborts with "newline in string"), which
# would silently reduce the table to its first row and make this lint pass on everything.
export GW_ISO_ALIASES="$ALIASES"
export GW_ISO_GWS="$(printf '%s\n' "${GWS[@]}")"
export GW_ISO_ALLOW

# The matcher, factored out so the self-test below can drive it with a KNOWN-BAD line. It reads the
# feed on stdin and prints one tab-separated finding per violation.
match_feed() { awk -F'\t' '
function esc(s,   r){ r=s; gsub(/[][\\.^$(){}?+*|\/]/, "\\\\&", r); return r }
function bounded(s){ return "(^|[^a-z0-9])" esc(tolower(s)) "([^a-z0-9]|$)" }
BEGIN{
  aliases=ENVIRON["GW_ISO_ALIASES"]; gws=ENVIRON["GW_ISO_GWS"]; allow=ENVIRON["GW_ISO_ALLOW"];
  na=split(aliases, AL, "\n");
  for (i=1; i<=na; i++) { if (AL[i]=="") continue; split(AL[i], p, "\t"); AG[i]=p[1]; AA[i]=p[2]; AR[i]=bounded(p[2]); }
  ng=split(gws, G, "\n");
  for (i=1; i<=ng; i++) { if (G[i]=="") continue; GR[i]=bounded(G[i]); }
}
{
  file=$1; ln=$2; raw=$3; code=$4;
  lraw=tolower(raw); lcode=tolower(code);
  if (raw ~ allow) next;                      # operator identity, see the allowlist note
  # RULE 1 - identity in code
  for (i=1; i<=na; i++) {
    if (AA[i]=="") continue;
    if (index(file, "gateways/" AG[i] "/")==1) continue;
    if (lcode ~ AR[i]) { printf "identity\t%s\t%s\t%s\t%s\t%s\n", file, ln, AG[i], AA[i], raw; break }
  }
  # RULE 2 - a roster on one line
  n=0; named="";
  for (i=1; i<=ng; i++) {
    if (G[i]=="") continue;
    if (index(file, "gateways/" G[i] "/")==1) continue;
    if (lraw ~ GR[i]) { n++; named=named " " G[i] }
  }
  if (n>=3) printf "enumeration\t%s\t%s\t%d\t%s\t%s\n", file, ln, n, named, raw;
  # RULE 1B - identity in a COMMENT. Rule 1 strips comments before it looks (a deliberate provenance
  # carve-out), so a gateway name can still leak into a comment without rule 1 ever seeing it; rule 1B
  # closes that gap by checking the raw, unstripped line separately.
  # A name in raw but not in code is, by construction, a name in a comment.
  # (No apostrophes in here: this block lives inside a single-quoted awk program.)
  for (i=1; i<=na; i++) {
    if (AA[i]=="") continue;
    if (index(file, "gateways/" AG[i] "/")==1) continue;
    if (index(tolower(allow), tolower(AG[i])) > 0) continue;   # the operator names itself on purpose
    if (lraw ~ AR[i] && lcode !~ AR[i]) { printf "comment-identity\t%s\t%s\t%s\t%s\t%s\n", file, ln, AG[i], AA[i], raw; break }
  }
}'; }

# ── SELF-TEST: the scanner must be PROVEN to fire before a clean run means anything ───────────────
# A lint that silently scans nothing reports PASS, which is worse than no lint (e.g. `awk -v` cannot
# carry a newline, so passing the alias table that way would collapse it to one row and the whole tree
# would come back clean). So: plant a known violation of each rule, in a file path outside every
# gateway directory, and require the matcher to report it. If these do not fire, the scanner is broken
# and the run aborts instead of printing a green result.
selftest() {
  local first="${GWS[0]}" out n
  # RULE 1: the first discovered gateway's name, in the CODE half of a harness-file line.
  out="$(printf 'lib/planted.sh\t1\tX=%s\tX=%s\n' "$first" "$first" | match_feed)"
  case "$out" in identity*) ;; *) echo "gateway_isolation_test: FAIL - self-test: rule 1 did not fire on a planted name"; exit 1 ;; esac
  # A comment-only name is RULE 3's business, not rule 1's: rule 1 must stay silent, rule 3 must fire.
  out="$(printf 'lib/planted.sh\t1\t# %s did a thing\t\n' "$first" | match_feed)"
  case "$out" in identity*) echo "gateway_isolation_test: FAIL - self-test: rule 1 fired on a comment"; exit 1 ;; esac
  case "$out" in comment-identity*) ;; *) echo "gateway_isolation_test: FAIL - self-test: rule 3 did not fire on a name in a comment"; exit 1 ;; esac
  # RULE 3 must NOT fire inside the gateway's own directory: a gateway may name itself in its own dir.
  out="$(printf 'gateways/%s/gateway.sh\t1\t# %s did a thing\t\n' "$first" "$first" | match_feed)"
  [ -z "$out" ] || { echo "gateway_isolation_test: FAIL - self-test: rule 3 fired inside the gateway's own dir"; exit 1; }
  # RULE 1 must NOT fire inside the gateway's own directory.
  out="$(printf 'gateways/%s/gateway.sh\t1\tX=%s\tX=%s\n' "$first" "$first" "$first" | match_feed)"
  [ -z "$out" ] || { echo "gateway_isolation_test: FAIL - self-test: rule 1 fired inside the gateway's own dir"; exit 1; }
  # RULE 2: three discovered names on one line, in a COMMENT, must still be a roster.
  n="${#GWS[@]}"
  if [ "$n" -ge 3 ]; then
    out="$(printf 'lib/planted.sh\t1\t# %s %s %s\t\n' "${GWS[0]}" "${GWS[1]}" "${GWS[2]}" | match_feed)"
    case "$out" in enumeration*) ;; *) echo "gateway_isolation_test: FAIL - self-test: rule 2 did not fire on a planted roster"; exit 1 ;; esac
  fi
  echo "gateway_isolation_test: self-test ok - the scanner fires on planted violations of both rules"
}
selftest

FINDINGS="$(feed | match_feed)"

if [ -n "$FINDINGS" ]; then
  while IFS=$'\t' read -r rule file ln a b txt; do
    [ -n "$rule" ] || continue
    if [ "$rule" = identity ]; then
      report "$file" "$ln" "identity" "names gateway '$a' (as '$b') outside gateways/$a/" "$txt"
    elif [ "$rule" = comment-identity ]; then
      report "$file" "$ln" "comment-identity" "names gateway '$a' (as '$b') in a COMMENT outside gateways/$a/ - anonymise it ('one gateway', 'another'), keeping every number and the reason" "$txt"
    else
      report "$file" "$ln" "enumeration" "hardcoded roster of $a gateways ($b ) - discover with lib/gateways.sh instead" "$txt"
    fi
  done <<< "$FINDINGS"
fi

echo "gateway_isolation_test: RULE 3 - a declared GW_CONTAINER matches the container the manifest launches"
# GW_CONTAINER exists so consumers OUTSIDE the gateway's directory (verify-local.sh's teardown) can
# read the name instead of hardcoding it. That only holds if the declaration is true, so pin it to the
# --name the manifest actually passes to docker. A manifest that launches no container declares none.
for g in "${GWS[@]}"; do
  decl="$(gw_manifest_field "$g" GW_CONTAINER "$B")"
  actual="$(grep -oE -- '--name[= ]+[A-Za-z0-9_.$"{}-]+' "$B/gateways/$g/gateway.sh" 2>/dev/null | head -1 | sed -E 's/^--name[= ]+//')"
  if [ -z "$actual" ]; then
    [ -z "$decl" ] || report "gateways/$g/gateway.sh" 0 "container" "declares GW_CONTAINER='$decl' but launches no container" "$decl"
    continue
  fi
  case "$actual" in *'$'*) actual="${actual//\$GW_CONTAINER/$decl}"; actual="${actual//\$\{GW_CONTAINER\}/$decl}"; actual="${actual//\"/}" ;; esac
  if [ -z "$decl" ]; then
    report "gateways/$g/gateway.sh" 0 "container" "launches container '$actual' but declares no GW_CONTAINER (verify-local.sh cannot discover it)" "--name $actual"
  elif [ "$decl" != "$actual" ]; then
    report "gateways/$g/gateway.sh" 0 "container" "GW_CONTAINER='$decl' but docker --name is '$actual'" "--name $actual"
  fi
done

echo
if [ "$FAILED" = 0 ]; then
  echo "gateway_isolation_test: PASS - every gateway's identity lives only in its own directory."
  echo "  Adding a gateway is: drop in gateways/<name>/gateway.sh. Nothing else in the tree names it."
else
  cat <<'MSG'
gateway_isolation_test: FAIL

  A gateway's name must appear NOWHERE outside its own gateways/<name>/ directory, and no line may
  enumerate the field. Every hit above is a place where dropping in a new gateways/<name>/gateway.sh
  would NOT be enough to make that gateway work.

  How to fix, in order of preference:
    1. DISCOVER it.  Shell: source lib/gateways.sh and use gw_list / gw_default / gw_manifest_field.
                     Node:  enumerate results/matrix/*.json (site/gen-data.mjs).
                     Python: enumerate gateways/*/gateway.sh (charts.py).
    2. DECLARE it.   If the value is metadata about one gateway (a label, a container name, a repo
                     link, a pinned image), move it into that gateway's own gateway.sh as a GW_*
                     field and have the consumer read it generically.
    3. PARAMETERISE it. If it is a test fixture, assert the BEHAVIOUR at the boundary value. Keep the
                     real measured numbers, they are the evidence; drop the gateway name from the
                     assertion label and keep the provenance in a comment.

  Do NOT add to the allowlist to make this pass. The allowlist covers the operator's own identity
  only, and every entry states why.
MSG
fi
echo "gateway_isolation_test: $VIOLATIONS violation(s)"
exit "$FAILED"
