#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Standalone Rust server from the official v1.108.3 source release.
set -euo pipefail
GW_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$GW_DIR/src"
COMMIT=b7d579fab2818968322ef96e5b8e98dc8178fafc
sudo -n DEBIAN_FRONTEND=noninteractive apt-get install -y -q git build-essential pkg-config libssl-dev cmake
[ -d "$SRC/.git" ] || git clone --filter=blob:none --no-checkout https://github.com/BerriAI/litellm.git "$SRC"
git -C "$SRC" fetch --depth=1 origin tag v1.108.3
git -C "$SRC" checkout --detach "$COMMIT"
[ "$(git -C "$SRC" rev-parse HEAD)" = "$COMMIT" ]
cargo build --manifest-path "$SRC/litellm-rust/Cargo.toml" --release --locked -p litellm-gateway --bin litellm-gateway
python3 "$GW_DIR/../../lib/gateway_build_provenance.py" "$SRC" "$SRC/litellm-rust/target/release/litellm-gateway" "$GW_DIR/.build-provenance.json"
