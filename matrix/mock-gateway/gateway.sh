#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Graceful-path FIXTURE, not a contender: a second instance of the mock posing as the gateway.
# It lives under matrix/ (not gateways/) precisely so run-all.sh / charts.py discovery never
# fields it. What it proves: the matrix runner's verdicts stay honest against a server that
# answers a plausible 200 on EVERY protocol path without translating anything.
#
# Expected verdicts (matrix v2): EVERY cell false. The non-openai ingress cells echo the mock's
# canned bodies and are flagged as UNTRANSLATED passthrough; the openai cell, which v1 passed
# incidentally, now fails too because the v2 diagonal rule demands a proven round trip to the
# upstream mock and this fixture answers from its own canned constants without ever calling it.
# All non-openai egress columns render not_configurable (no gw_matrix_egress here, on purpose).
# If this fixture ever goes green on any cell, the guard has a hole.
GW_KIND=native
GW_DISPLAY="Mock (fixture)"
GW_LANG=Rust
GW_REPO=https://github.com/GetBusbar/busbar
GW_PORT=8010
GW_PATH=/v1/chat/completions
GW_MODEL=gpt-4o-mini
GW_AUTH=bench-token

MOCKGW_BIN=""
gw_build() {
  MOCKGW_BIN="$ROOT/mock/target/release/mock"
  [ -x "$MOCKGW_BIN" ] || ( cd "$ROOT/mock" && cargo build --release >/dev/null 2>&1 )
  [ -x "$MOCKGW_BIN" ]
}
gw_version() { echo "mock-as-gateway fixture"; }
gw_launch() {
  # Scope the kill to THIS port: the runner's upstream mock on $MOCK_PORT is the same binary.
  pkill -f "mock -port $GW_PORT" 2>/dev/null; sleep 1
  setsid taskset -c "$CORES" "$MOCKGW_BIN" -port "$GW_PORT" </dev/null >/dev/null 2>&1 &
}
gw_rss() { :; }
gw_stop() { pkill -f "mock -port $GW_PORT" 2>/dev/null; }
