#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Regression guard for the RSS MEASUREMENT-FAILURE contract (lib/harness.sh).
#
# THE BUG THIS EXISTS FOR (audit P0-2): _rss_tree_mib returned a literal `0` for a missing pid and
# printed "0.0" whenever /proc could not be read (container not running, `docker inspect` failed, a PID
# namespace we cannot see, a non-Linux host with no /proc at all). container_rss_mib / container_hwm_mib
# propagated that. Downstream, matrix/run.sh's idle / recovered / hwm guards were
# `case "$x" in ''|*[!0-9.]*)` — which does NOT match "0" or "0.0" — so a rig-side MEASUREMENT FAILURE
# was published as a CERTIFIED idle_rss_mib: 0.0 / recovered_rss_mib: 0.0 / peak_rss_hwm_mib: 0. The
# board ranks memory ASCENDING (lower is better), so a gateway we simply failed to measure took first
# place on memory with a fabricated number.
#
# THE CONTRACT: a live process cannot be 0 MiB resident, therefore 0 IS "not measured". Every reader
# prints NOTHING when it cannot measure, and mem_num_or_null — the ONE guard all four lanes
# (idle/peak/recovered/hwm) share — turns empty / 0 / 0.0 / garbage into the literal JSON `null`.
#
# Run: bash lib/mem_rss_test.sh   (no network, no docker, no root; seconds)
set -u
B="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$B/lib/harness.sh"

FAILED=0
check(){ # label expected actual
  if [ "$2" = "$3" ]; then
    printf '  ok   %s\n' "$1"
  else
    printf '  FAIL %s: expected [%s] got [%s]\n' "$1" "$2" "$3"; FAILED=1
  fi
}

echo "mem_rss_test: mem_num_or_null — the single null-guard"
check "empty        -> null" "null" "$(mem_num_or_null "")"
check "0            -> null" "null" "$(mem_num_or_null "0")"          # THE regression: was published as 0
check "0.0          -> null" "null" "$(mem_num_or_null "0.0")"        # THE regression: was published as 0.0
check "0.00         -> null" "null" "$(mem_num_or_null "0.00")"
check "non-numeric  -> null" "null" "$(mem_num_or_null "abc")"
check "-1           -> null" "null" "$(mem_num_or_null "-1")"
check "'null'       -> null" "null" "$(mem_num_or_null "null")"
check "12.5         -> 12.5" "12.5" "$(mem_num_or_null "12.5")"       # a real reading passes through verbatim
check "1024         -> 1024" "1024" "$(mem_num_or_null "1024")"

echo "mem_rss_test: /proc readers — unmeasurable prints NOTHING (never 0)"
check "_rss_tree_mib ''        -> empty" "" "$(_rss_tree_mib "")"
check "_rss_tree_mib 0         -> empty" "" "$(_rss_tree_mib 0)"
check "_hwm_tree_mib ''        -> empty" "" "$(_hwm_tree_mib "")"
check "_hwm_tree_mib 0         -> empty" "" "$(_hwm_tree_mib 0)"
# A pid whose /proc/<pid>/status is unreadable/absent — the "unreadable /proc" path proper. pid 2^22+1
# is above every Linux pid_max default and does not exist on any host; on macOS /proc is absent entirely,
# so this covers both flavours of "cannot read the kernel's numbers".
check "_rss_tree_mib <no /proc>-> empty" "" "$(_rss_tree_mib 4194305)"
check "_hwm_tree_mib <no /proc>-> empty" "" "$(_hwm_tree_mib 4194305)"

echo "mem_rss_test: docker path — a failed \`docker inspect\` must not fabricate a 0"
BENCH_DOCKER="/usr/bin/false"                      # simulate: docker missing / daemon down / no such container
check "container_rss_mib (inspect fails) -> empty" "" "$(container_rss_mib no-such-container-bench)"
check "container_hwm_mib (inspect fails) -> empty" "" "$(container_hwm_mib no-such-container-bench)"
# A stopped container: `docker inspect -f '{{.State.Pid}}'` prints a literal 0 for it.
BENCH_DOCKER="_fake_docker_stopped"
_fake_docker_stopped(){ echo 0; }
check "container_rss_mib (stopped ctr, pid 0) -> empty" "" "$(container_rss_mib stopped-bench)"
check "container_hwm_mib (stopped ctr, pid 0) -> empty" "" "$(container_hwm_mib stopped-bench)"

echo "mem_rss_test: end-to-end — an unmeasurable gw_rss/gw_hwm hook yields null, not 0"
gw_rss(){ echo 0.0; }    # exactly what the old container_rss_mib returned when /proc was unreadable
gw_hwm(){ echo 0; }
check "mem_rss_read (hook says 0.0) -> empty"      "" "$(mem_rss_read)"
check "mem_hwm_read (hook says 0)   -> empty"      "" "$(mem_hwm_read)"
check "lane value (hook says 0.0)   -> null"   "null" "$(mem_num_or_null "$(mem_rss_read)")"
check "lane value (hook says 0)     -> null"   "null" "$(mem_num_or_null "$(mem_hwm_read)")"
gw_rss(){ :; }           # the mock-gateway fixture: no measurement at all
check "lane value (hook silent)     -> null"   "null" "$(mem_num_or_null "$(mem_rss_read)")"
gw_rss(){ echo 187.4; }  # a real reading still travels through untouched
check "lane value (real reading)    -> 187.4" "187.4" "$(mem_num_or_null "$(mem_rss_read)")"

echo "mem_rss_test: _mem_kv — the load-success/payload gate reads ugen's stats line"
LINE="rps=1234 fail=0 p50=1.20 p99=9.90 p50us=1200 p99us=9900 ok=45678"
check "_mem_kv ok"      "45678" "$(_mem_kv "$LINE" ok)"
check "_mem_kv rps"      "1234" "$(_mem_kv "$LINE" rps)"
check "_mem_kv missing"      "" "$(_mem_kv "$LINE" nope)"
check "_mem_kv no output"    "" "$(_mem_kv "" ok)"

if [ "$FAILED" = 0 ]; then echo "mem_rss_test: PASS"; else echo "mem_rss_test: FAIL"; fi
exit "$FAILED"
