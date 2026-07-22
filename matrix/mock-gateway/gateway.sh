#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Graceful-path FIXTURE, not a contender: a second instance of the mock posing as the gateway.
# It lives under matrix/ (not gateways/) precisely so run-all.sh / charts.py discovery never
# fields it. What it proves: the matrix runner's verdicts stay honest against a server that
# answers a plausible 200 on EVERY protocol path without translating anything.
#
# Expected verdicts: openai=true (incidental — the "gateway" IS an openai-shaped server, and the
# openai cell rightly accepts a straight proxy), every other cell false, with the non-openai cells
# that echo the mock's canned bodies flagged as UNTRANSLATED passthrough. If this fixture ever goes
# green on a translation cell, the passthrough guard has a hole.
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
