#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# THE FETCH MUST LAND FILES WITH THE MODE THE COMMIT RECORDS.
#
# run-on-ec2.sh fetches each file individually via curl (rather than a tarball, which preserves modes
# for free) so a box downloads only its own gateway directory instead of the whole repo. A raw
# download writes whatever the umask says, so the executable bit must be restored explicitly - a
# source-built gateway's build.sh, run directly by the launcher, fails opaquely
# ("... is not executable") if this is missed, while container gateways still work fine. The defect is
# invisible to any check that only asks "did the file arrive" (bytes are fine, mode is wrong), so this
# asserts the MODE on both sides - executable stays executable, plain stays plain.
#
# Self-contained: it reconstructs the same read/curl/chmod loop the orchestrator sends to the box,
# against a local file:// source, so it needs no network and no EC2.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1

FAIL=0
ok()   { echo "ok   - $1"; }
bad()  { echo "FAIL - $1"; FAIL=1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A source tree standing in for the commit: one executable, one plain.
src="$work/src"
mkdir -p "$src/gateways/demo"
printf '#!/bin/sh\necho build\n' > "$src/gateways/demo/build.sh"
printf '{"name":"demo"}\n'       > "$src/gateways/demo/definition.json"
chmod +x "$src/gateways/demo/build.sh"

# The mode+path feed, exactly as `git ls-tree -r <sha> | awk '{print $1"\t"$4}'` produces it.
feed=$(printf '100755\tgateways/demo/build.sh\n100644\tgateways/demo/definition.json\n')

# The loop the orchestrator sends to the box, with curl pointed at the local tree instead of raw
# GitHub. Everything else - the tab-split read, the mkdir, the -f, the chmod - is the shipped logic.
dest="$work/box"
mkdir -p "$dest"
( cd "$dest"
  while IFS=$'\t' read -r mode f; do
    [ -n "$f" ] || continue
    mkdir -p "$(dirname "$f")"
    curl -fsSL -o "$f" "file://$src/$f"
    [ "$mode" = 100755 ] && chmod +x "$f"
  done <<FILELIST
$feed
FILELIST
) || true

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
