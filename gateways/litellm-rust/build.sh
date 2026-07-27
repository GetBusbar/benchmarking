#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Build this gateway from source, once, before the first launch.
#
# The engine runs this as the launch spec's pre-launch step, so it happens before anything is
# measured. That ordering is deliberate: this installs a toolchain, a python environment and a
# release binary, and none of it may happen inside a measurement window.
#
# The output path is not chosen here. definition.json declares the candidate binary names and this
# script has to produce one of them.
set -euo pipefail

GW_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

REPO="${LITELLM_RUST_REPO:-https://github.com/BerriAI/litellm}"
BRANCH="${LITELLM_RUST_BRANCH:-litellm_rust_gateway_v1_messages_route}"
COMMIT="${LITELLM_RUST_COMMIT:-698072308b}"
PY_SPEC="${LITELLM_PY_SPEC:-litellm[proxy]==1.93.0}"
SRC="$GW_DIR/src"
VENV="$GW_DIR/venv"

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
sudo -n DEBIAN_FRONTEND=noninteractive apt-get install -y -q git build-essential pkg-config libssl-dev python3-venv python3-pip

# THE PYTHON PACKAGE IS NOT OPTIONAL AND ITS FAILURE IS SILENT.
#
# This build enables a feature that LOADS the python package at runtime to read the gateway's
# config, so a pip install that half worked produces a gateway that cannot boot. That used to be
# recorded as a published served=false with zero throughput, which reads as a gateway that does not
# work rather than a rig that did not finish installing it. So the import is verified, retried, and
# the build fails loudly rather than handing a broken binary to the measurement.
for attempt in 1 2 3; do
  [ -x "$VENV/bin/python" ] || python3 -m venv "$VENV"
  "$VENV/bin/pip" install -q --upgrade pip >/dev/null 2>&1 || true
  rc=0
  "$VENV/bin/pip" install -q "$PY_SPEC" >/dev/null 2>&1 || rc=$?
  if [ "$rc" = 0 ] && "$VENV/bin/python" -c 'import litellm' >/dev/null 2>&1; then
    break
  fi
  echo "build: python config package attempt $attempt failed (pip rc=$rc)" >&2
  [ "$attempt" = 3 ] && { echo "build: the python config package would not import after 3 attempts"; exit 1; }
  rm -rf "$VENV"
  sleep $((attempt * 5))
done

# HAND THE VENV TO THE EMBEDDED INTERPRETER, UNDER A STABLE NAME.
#
# The binary embeds CPython and imports `litellm` to read LITELLM_CONFIG_PATH. It is launched as a
# bare process with no venv activated, so nothing puts this venv on the embedded interpreter's
# import path unless PYTHONPATH does - and without the import the config reader loads no model_list,
# which the gateway answers as HTTP 400 "no deployment registered" on every request. The old shell
# manifest set PYTHONPATH at launch by asking the venv where its site-packages was; `env` is a
# static file with no shell in it and cannot, and the real path embeds the venv's python MINOR
# version, so it cannot be written literally either. Pin it to a version-independent name here and
# let `env` name that. The link lives inside the venv so it is covered by the venv's existing
# ignore rule rather than needing a new one for a build artifact.
SITE="$("$VENV/bin/python" -c 'import site;print(site.getsitepackages()[0])')"
[ -d "$SITE" ] || { echo "build: the venv has no site-packages directory ($SITE)"; exit 1; }
ln -sfn "$SITE" "$VENV/site-packages"

if [ ! -d "$SRC" ]; then
  git clone -q -b "$BRANCH" "$REPO" "$SRC"
fi
git -C "$SRC" fetch -q origin 2>/dev/null || true
git -C "$SRC" checkout -q "$COMMIT"

( cd "$SRC/litellm-rust" && cargo build --release -p litellm-ai-gateway --features server,python-config )

# The crate does not emit a stable output name, which is why definition.json declares candidates
# rather than one path. Accept the first that exists, and say which so the artifact's build string
# and the binary that ran cannot silently disagree.
BIN=""
for cand in litellm-ai-gateway litellm_ai_gateway server; do
  if [ -x "$SRC/litellm-rust/target/release/$cand" ]; then
    BIN="$SRC/litellm-rust/target/release/$cand"
    break
  fi
done
[ -n "$BIN" ] || { echo "build: finished but none of the declared binary names exist"; exit 1; }
echo "build: $BIN at $(git -C "$SRC" rev-parse --short HEAD) with $("$VENV/bin/pip" show litellm 2>/dev/null | awk '/^Version:/{print $2}')"
