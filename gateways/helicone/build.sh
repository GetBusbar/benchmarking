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

# Prerequisites. Unconditionally, because apt-get install is already idempotent and already
# resolves dependencies: there is nothing here for us to be clever about. It used to be wrapped in
# `if ! command -v cargo`, which is the bug: the box provisions rust for every gateway, so cargo was
# always present, the block never ran, and the packages that are not rust were never installed. The
# failure then surfaced one layer down as "failed to run custom build command for openssl-sys", which
# names a crate rather than the missing package.
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q git build-essential pkg-config libssl-dev

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
