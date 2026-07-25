#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# GATEWAY DISCOVERY - the ONE place anything shell-side learns which gateways exist.
#
# THE INVARIANT THIS SERVES. A gateway is defined ENTIRELY by its own directory. Adding one is
# exactly: drop in `gateways/<name>/gateway.sh`. Nothing else in the tree may name it, enumerate it,
# or special-case it - no harness file, no site file, no chart script, no test.
#
# THE DEVIATION THIS EXISTS TO KILL. The engine was already dynamic in the places that mattered
# (lib/box_qualify.sh discovers baselines out of results/snapshots/, site/gen-data.mjs projects
# whatever results/matrix/*.json it finds, run-all.sh and run-on-ec2.sh fan out over gateways/*/),
# but a handful of files had drifted back to naming a gateway or iterating a frozen list:
#
#   * bench-status.sh iterated a 13-name string, so a new gateway was simply invisible to the
#     live-status view and a removed one printed "pending" forever.
#   * matrix/run.sh, stream/run.sh, streamcpu/run.sh, xlate/run.sh, matrix/qualify-box.sh and
#     verify-local.sh each defaulted GATEWAY to one specific name.
#   * verify-local.sh tore down a hardcoded container name, so it could only ever verify that one
#     gateway even when handed another.
#
# Every one of those now calls into here. Discovery is `gateways/*/gateway.sh` on disk, alphabetical
# (glob order), so a directory IS the registration and there is no list that can drift from the tree.
#
# Sourced, never executed:  . "$ROOT/lib/gateways.sh"
# ─────────────────────────────────────────────────────────────────────────────────────────────────

# gw_root - the repo root, derived from this file's own location. Callers may pass an explicit root.
gw_root() { # [root]
  if [ -n "${1:-}" ]; then printf '%s' "$1"; return; fi
  ( cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd )
}

# gw_list - every gateway that exists, one per line, alphabetical. A gateway EXISTS iff its
# directory holds a gateway.sh; nothing else counts, so a stray dir or a leftover config cannot
# invent one. Prints nothing (and returns 1) if the tree holds no manifests at all.
gw_list() { # [root]
  local root n found=0
  root="$(gw_root "${1:-}")"
  for m in "$root"/gateways/*/gateway.sh; do
    [ -f "$m" ] || continue
    n="$(basename "$(dirname "$m")")"
    printf '%s\n' "$n"
    found=1
  done
  [ "$found" = 1 ]
}

# gw_count - how many gateways exist. Used anywhere a progress line wants a denominator, so the
# denominator can never be a stale literal.
gw_count() { # [root]
  gw_list "${1:-}" | grep -c . || true
}

# gw_exists - is this a real gateway?
gw_exists() { # name [root]
  local root; root="$(gw_root "${2:-}")"
  [ -n "${1:-}" ] && [ -f "$root/gateways/$1/gateway.sh" ]
}

# gw_default - the gateway a runner uses when the caller named none. This is DISCOVERED (the first
# manifest on disk), never a baked-in favourite: every field path (run-all.sh, run-on-ec2.sh) passes
# GATEWAY explicitly, so this only ever serves a bare interactive invocation. Alphabetical, so it is
# deterministic and no gateway is seated first by editorial choice.
gw_default() { # [root]
  gw_list "${1:-}" | head -1
}

# gw_manifest_field - read a LITERAL top-level assignment out of a manifest WITHOUT sourcing it (so
# no build/launch logic runs and no environment is disturbed). Same technique run-all.sh already used
# for GW_PORT. Quotes are stripped; a value that is a shell expansion is returned verbatim, so
# callers must treat "" as "not literally declared" and fall back.
gw_manifest_field() { # gateway VAR [root]
  local root v; root="$(gw_root "${3:-}")"
  v="$(grep -m1 -E "^$2=" "$root/gateways/$1/gateway.sh" 2>/dev/null | cut -d= -f2-)"
  v="${v%%#*}"                    # drop a trailing comment
  v="${v%"${v##*[![:space:]]}"}"  # rstrip
  v="${v#\"}"; v="${v%\"}"; v="${v#\'}"; v="${v%\'}"
  printf '%s' "$v"
}
