#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# THE FETCH MUST LAND FILES WITH THE MODE THE COMMIT RECORDS.
#
# A box used to receive its harness as a tarball, which carries modes for free. Replacing that with
# per-file fetches - to stop every box downloading 46 MB of results/ to keep 44 KB - lost the
# executable bit, because a raw download writes whatever the umask says. The three source-built
# entrants ship a build.sh that the launcher runs directly, so all three died with
# "build script gateways/<name>/build.sh is not executable" while the eleven container entrants ran
# fine. A whole 14-box run was thrown away for it.
#
# The defect is invisible to every check that only asks "did the file arrive": the file arrived, with
# the right bytes, and was useless. So this asserts the MODE, on both sides - an executable file
# stays executable, and a plain file is not quietly made executable to paper over it.
#
# It runs THE SHIPPED CODE, not a copy: the mode+path feed comes from a real `git ls-tree` piped through
# the awk program extracted from run-on-ec2.sh, and the read/curl/chmod loop is extracted from the
# remote fetch block in run-on-ec2.sh and expanded exactly as the orchestrator's double-quoted ssh
# string expands it, with $_raw pointed at a local file:// tree. No network, no EC2.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1
SRC_SH="$PWD/run-on-ec2.sh"

FAIL=0
ok()   { echo "ok   - $1"; }
bad()  { echo "FAIL - $1"; FAIL=1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A source tree standing in for the commit, committed for real so the feed is git's own output: one
# executable, one plain.
src="$work/src"
mkdir -p "$src/gateways/demo"
printf '#!/bin/sh\necho build\n' > "$src/gateways/demo/build.sh"
printf '{"name":"demo"}\n'       > "$src/gateways/demo/definition.json"
chmod +x "$src/gateways/demo/build.sh"
git -C "$src" init -q && git -C "$src" add -A \
  && git -C "$src" -c user.name=t -c user.email=t@t -c commit.gpgsign=false -c core.hooksPath=/dev/null commit --no-verify -qm t \
  || { echo "FAIL - could not build the fixture commit"; exit 1; }

# The shipped awk, taken from the `_files=` pipeline in run-on-ec2.sh.
awk_line="$(grep -m1 "awk -F'\\\\t' '{split(" "$SRC_SH" | sed -e 's/^[[:space:]]*|[[:space:]]*//' -e 's/)"$//')"
case "$awk_line" in awk*) ;; *) echo "FAIL - could not extract the ls-tree awk from run-on-ec2.sh"; exit 1 ;; esac
feed="$(git -C "$src" ls-tree -r HEAD -- gateways/demo | eval "$awk_line")"

# The shipped loop, from `while IFS=\$'\t' read -r mode f; do` through `done <<'FILELIST'`, expanded as
# the orchestrator's double-quoted ssh string expands it (\$ -> $, \" -> ", $_raw -> its value).
loop_src="$(awk '/while IFS=\\\$.\\t. read -r mode f; do/{p=1} p{print} p&&/done <<.FILELIST./{exit}' "$SRC_SH")"
case "$loop_src" in *"read -r mode f"*FILELIST*) ;; *) echo "FAIL - could not extract the fetch loop from run-on-ec2.sh"; exit 1 ;; esac
_raw="file://$src"
loop="$(eval "printf '%s' \"$loop_src\"")"

dest="$work/box"
mkdir -p "$dest"
( cd "$dest" && bash -c "$loop
$feed
FILELIST" ) || true

[ -x "$dest/gateways/demo/build.sh" ] \
  && ok "an executable file in the commit arrives executable" \
  || bad "build.sh arrived NOT executable - the launcher cannot run it, which is what killed all three source-built entrants"

[ -x "$dest/gateways/demo/definition.json" ] \
  && bad "a plain file was made executable - the mode is being guessed, not carried" \
  || ok "a plain file is not made executable"

[ -s "$dest/gateways/demo/definition.json" ] \
  && ok "contents arrive intact" \
  || bad "definition.json is empty"

# And the guard that matters at the other end: a path absent from the commit must FAIL the fetch,
# never land as an empty file the run would then try to measure with.
if ( cd "$dest" && curl -fsSL -o missing.json "file://$src/gateways/demo/not-a-real-file" 2>/dev/null ); then
  bad "a missing path succeeded - it would arrive as an empty file"
else
  ok "a path absent from the commit fails the fetch rather than arriving empty"
fi

echo
if [ "$FAIL" = 0 ]; then
  echo "fetch_modes_test: PASS - files arrive with the mode the commit recorded."
else
  echo "fetch_modes_test: FAIL"
fi
exit "$FAIL"
