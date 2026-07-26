#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Resolve the Rust engine binary. Sets $OTB.
#
# The engine is shipped PREBUILT, exactly as the rig is, because a build toolchain on every box is a
# parity problem (lib/harness.sh argues this at length: only 3 of 13 manifests get one, deliberately).
# The difference from the rig is where it comes from: the rig self-fetches a pinned release on the
# box, while the engine is cross-compiled by the orchestrator and rsynced up under engine/dist/, so
# the binary that ran the field is the binary built from the tree that was measured.
#
# THERE IS NO FALLBACK PATH, on purpose. An engine that silently reverts to a shell implementation
# when its binary is missing is two engines, and the second one rots unobserved. Missing is FATAL.
_otb_log() { echo "[otb] $*" >&2; }

otb_arch() {
  case "$(uname -m)" in
    aarch64 | arm64) echo arm64 ;;
    x86_64 | amd64) echo x86_64 ;;
    *) uname -m ;;
  esac
}

# otb_resolve <repo_root> -> sets OTB, returns non-zero if unusable
otb_resolve() {
  local root="$1" arch; arch="$(otb_arch)"
  # 1. The shipped artifact: what a field box has, and what the orchestrator cross-compiled.
  OTB="$root/engine/dist/otb-$arch"
  [ -x "$OTB" ] && { _otb_log "engine: shipped $OTB"; return 0; }
  # 2. A local cargo build, for a dev host running the shell suites without a dist step.
  for c in "$root/target/release/otb" "$root/target/debug/otb"; do
    [ -x "$c" ] && { OTB="$c"; _otb_log "engine: local build $OTB"; return 0; }
  done
  # 3. Build it, if this host can. A field box cannot and must not: it has no toolchain by design.
  if command -v cargo >/dev/null 2>&1; then
    _otb_log "engine: no binary found, building"
    ( cd "$root" && cargo build --quiet --bin otb ) || { _otb_log "FATAL: cargo build failed"; return 1; }
    OTB="$root/target/debug/otb"
    [ -x "$OTB" ] && return 0
  fi
  _otb_log "FATAL: no engine binary at engine/dist/otb-$arch and no toolchain to build one."
  _otb_log "       The engine ships prebuilt; a shell fallback would be a second engine, so this is fatal."
  return 1
}
