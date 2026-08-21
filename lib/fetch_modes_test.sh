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
# THE LOOP UNDER TEST IS EXTRACTED VERBATIM FROM run-on-ec2.sh, not hand-retyped. This test used to
# carry its OWN copy of the read/curl/chmod loop, so an edit to the real embedded loop (dropping the
# `[ "$mode" = 100755 ] && chmod +x` line, or breaking the tab-split read) would reintroduce the
# historical bug while this test kept passing against its stale copy - the exact drift box_reachable_test.sh
# and publish_lock_test.sh avoid by awk-extracting the real function. So this now awk-extracts the real
# `while IFS=…read…done` loop out of the provision heredoc and runs THAT, with only two mechanical
# transforms: de-escape the heredoc's `\$`/`\"` shell-escaping, and repoint the curl base ($_raw, the
# GitHub raw URL) at a local file:// source so it needs no network and no EC2.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$HERE/run-on-ec2.sh"

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

# ── extract the REAL loop verbatim ────────────────────────────────────────────────────────────────
# From the `while IFS=…read -r mode f; do` line down to and including `done <<'FILELIST'`.
loop="$(awk '/while IFS=.*read -r mode f; do/{p=1} p{print} p&&/done <<.FILELIST.$/{exit}' "$SRC")"
if [ -z "$loop" ] || ! printf '%s\n' "$loop" | grep -q 'chmod +x'; then
  echo "FAIL - could not extract the fetch loop (with its chmod) from run-on-ec2.sh"; exit 1
fi
# Two mechanical transforms, nothing that touches the read/mkdir/curl/chmod logic itself:
#   1. de-escape the heredoc's shell-escaping: \$ -> $ and \" -> "
#   2. repoint the curl base $_raw (GitHub raw URL) at the local file:// source tree
loop="$(printf '%s\n' "$loop" | sed 's/\\\$/$/g; s/\\"/"/g' | sed "s#\$_raw#file://$src#g")"

# The mode+path feed, exactly as `git ls-tree -r <sha> | awk '{print $1"\t"$4}'` produces it, fed into
# the extracted loop's own `<<'FILELIST'` heredoc.
dest="$work/box"
mkdir -p "$dest"
runner="$work/run-fetch.sh"
{
  echo "cd \"$dest\" || exit 1"
  printf '%s\n' "$loop"
  printf '100755\tgateways/demo/build.sh\n'
  printf '100644\tgateways/demo/definition.json\n'
  echo "FILELIST"
} > "$runner"
bash "$runner" || true

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
# never land as an empty file the run would then try to measure with. The extracted loop uses `curl
# -fsSL`, so drive that same `-f` here against a missing path.
if ( cd "$dest" && curl -fsSL -o missing.json "file://$src/gateways/demo/not-a-real-file" 2>/dev/null ); then
  bad "a missing path succeeded - it would arrive as an empty file"
else
  ok "a path absent from the commit fails the fetch rather than arriving empty"
fi

echo
if [ "$FAIL" = 0 ]; then
  echo "fetch_modes_test: PASS - files arrive with the mode the commit recorded (loop extracted from run-on-ec2.sh)."
else
  echo "fetch_modes_test: FAIL"
fi
exit "$FAIL"
