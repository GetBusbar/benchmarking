#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Build this gateway from source, once, before the first launch.
#
# The engine runs this as the launch spec's pre-launch step, so it happens before anything is
# measured. That ordering is deliberate and predates the engine: this installs a toolchain and
# compiles a release binary, and neither may happen inside a measurement window.
#
# The output path is not chosen here. definition.json declares where the binary will be, and this
# script has to put it there; the two are checked against each other by `otb validate`.
set -euo pipefail

# The gateway's own directory, whatever it is called and wherever it is checked out.
GW_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# PINNED. A revision, not a branch: a moving ref would make two runs weeks apart incomparable while
# reporting the same build string. Overridable for a local experiment, never in the field.
REPO="${AISIX_REPO:-https://github.com/api7/aisix}"
COMMIT="${AISIX_COMMIT:-0f90a98ec8c43864d43e740e3ab66fe1d639c143}"   # tag v0.5.0
SRC="$GW_DIR/src"

# Prerequisites. Unconditionally, because apt-get install is already idempotent and already
# resolves dependencies: there is nothing here for us to be clever about. It used to be wrapped in
# `if ! command -v cargo`, which is the bug: the box provisions rust for every gateway, so cargo was
# always present, the block never ran, and the packages that are not rust were never installed. The
# failure then surfaced one layer down as "failed to run custom build command for openssl-sys", which
# names a crate rather than the missing package.
# -n so this fails immediately rather than blocking on a password prompt. The bench box gives the
# ubuntu user passwordless sudo, which the harness already relies on elsewhere; if that ever stops
# being true this must fail loudly at the missing package rather than hang the run waiting on a tty
# that is not there.
sudo -n DEBIAN_FRONTEND=noninteractive apt-get install -y -q git build-essential pkg-config libssl-dev protobuf-compiler
command -v protoc >/dev/null || { echo "build: protoc is required by this gateway's build dependencies and is not installed"; exit 1; }

[ -d "$SRC" ] || git clone -q "$REPO" "$SRC"
# RE-PIN ON EVERY BUILD, not only on a fresh clone. A box whose source tree survived a previous run
# would otherwise keep whatever ref the first build left behind and silently ignore a changed pin,
# and the run would then record a build string that does not match the code it measured.
git -C "$SRC" fetch -q origin 2>/dev/null || true
git -C "$SRC" checkout -q "$COMMIT"

( cd "$SRC" && cargo build --release --bin aisix )

BIN="$SRC/target/release/aisix"
[ -x "$BIN" ] || { echo "build: finished but $BIN is not there"; exit 1; }
echo "build: $BIN at $(git -C "$SRC" rev-parse --short HEAD)"
