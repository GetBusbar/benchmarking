#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Build this gateway from source, once, before the first launch.
#
# The engine runs this as the launch spec's pre-launch step, so it happens before anything is
# measured. That ordering is deliberate: this installs a toolchain and compiles a release binary,
# and neither may happen inside a measurement window.
#
# The output path is not chosen here. definition.json declares where the binary will be, and this
# script has to put it there; the two are checked against each other by `otb validate`.
set -euo pipefail

GW_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# PINNED to a revision rather than a branch, so two runs weeks apart measure the same code.
REPO="${HELICONE_REPO:-https://github.com/Helicone/ai-gateway}"
COMMIT="${HELICONE_COMMIT:-9649b27bdc9fb0907d359e899894102a15f3a085}"
SRC="$GW_DIR/src"

if ! command -v cargo >/dev/null; then
  sudo apt-get install -y -q git build-essential pkg-config libssl-dev >/dev/null 2>&1 || true
  (curl -sSf https://sh.rustup.rs | sh -s -- -y >/dev/null 2>&1) || true
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env" 2>/dev/null || true
fi
command -v cargo >/dev/null || { echo "build: rust toolchain unavailable"; exit 1; }

[ -d "$SRC" ] || git clone -q "$REPO" "$SRC"
# Re-pin every time, not only on a fresh clone: a surviving source tree would otherwise keep an old
# ref and the run would record a build string that does not match what it measured.
git -C "$SRC" fetch -q origin 2>/dev/null || true
git -C "$SRC" checkout -q "$COMMIT"

# The release profile here uses LTO and codegen-units=1, so this is slow and memory hungry. It is
# fine on the bench box and it is what the project itself ships, which is the configuration we are
# supposed to be measuring.
( cd "$SRC" && cargo build --release -p ai-gateway )

BIN="$SRC/target/release/ai-gateway"
[ -x "$BIN" ] || { echo "build: finished but $BIN is not there"; exit 1; }
echo "build: $BIN at $(git -C "$SRC" rev-parse --short HEAD)"
