#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# Regression guard for the BOX QUALIFICATION verdict engine (lib/box_qualify.sh).
#
# THE INCIDENT THIS EXISTS FOR. On 2026-07-25 one box (gomodel's) was contaminated. Its no-gateway
# floor — direct_c1_p99_us, the loadgen hitting the mock with NO gateway in the path — read 77-81us,
# INSIDE the healthy population's 73-82us, so no static absolute threshold could have caught it. What
# WAS anomalous was the movement against its OWN prior run (+5.4% while the healthiest 9 boxes moved
# -2.6%..+3.8%) and, far more starkly, its peak throughput (-86% while healthy gateways moved
# -13%..+13%). The run published that as a gateway regression. This test pins the two gates that turn
# those facts into an up-front verdict, using the REAL measured numbers as the fixtures.
#
# WHAT IT ASSERTS
#   stage 1  every healthy box PASSES at the shipped +/-4% band; gomodel's +5.4% FAILS
#   stage 1  a naive static-absolute check would NOT have caught gomodel (the calibration fact itself)
#   stage 1  jitter: an elevated tail spread fails even when the median is perfectly normal
#   stage 1  no history -> "seed" (a first run is never blocked for lack of a baseline)
#   stage 1  anti-ratchet: creep that passes every per-run step still fails the long-horizon bound
#   stage 2  every healthy gateway PASSES at the shipped +/-25% band; gomodel's -86% FAILS
#   stage 2  no recorded peak -> "seed"
#   baseline the rolling baseline is a MEDIAN of the last N, so ONE contaminated run cannot become
#            the next run's reference; and a FAILED qualification never seeds the baseline at all
#
# Run: bash lib/box_qualify_test.sh   (no network, no AWS, no docker, no rig binaries; seconds)
set -u
B="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$B/lib/box_qualify.sh"

FAILED=0
check(){ # label expected actual
  if [ "$2" = "$3" ]; then
    printf '  ok   %s\n' "$1"
  else
    printf '  FAIL %s: expected [%s] got [%s]\n' "$1" "$2" "$3"; FAILED=1
  fi
}

# ── helpers ──────────────────────────────────────────────────────────────────────────────────────
# A synthetic floor sample set at a given median with a given +/- spread, so a case can be described
# as "this box's floor moved X% with Y% jitter" rather than as a pile of magic numbers.
s1(){ # median_us drift_pct spread_pct baseline_us [oldest_us]
  local base="$4" drift="$2" spread="$3" med lo hi p50
  med="$(awk -v b="$base" -v d="$drift" 'BEGIN{printf "%.1f", b*(1+d/100)}')"
  lo="$(awk -v m="$med" -v s="$spread" 'BEGIN{printf "%.1f", m*(1-s/200)}')"
  hi="$(awk -v m="$med" -v s="$spread" 'BEGIN{printf "%.1f", m*(1+s/200)}')"
  p50="$(awk -v m="$med" 'BEGIN{printf "%.1f", m/2.2}')"   # a healthy p99/p50 ratio
  bq_stage1_verdict "$med" "$lo" "$hi" "$p50" 20000 "$base" "${5:-}"
  printf '%s' "$BQ_VERDICT"
}

echo "box_qualify_test: numeric core"
check "bq_median odd"            "77" "$(bq_median 81 77 74)"
check "bq_median even"           "76" "$(bq_median 74 78 81 74)"
check "bq_median ignores junk"   "77" "$(bq_median 77 '' abc)"
check "bq_median nothing->empty"   "" "$(bq_median '' abc)"
check "bq_drift_pct +5.4"      "5.40" "$(bq_drift_pct 81 76.85)"
check "bq_drift_pct -86"     "-86.00" "$(bq_drift_pct 1400 10000)"
# A zero/absent baseline must yield EMPTY, never a fabricated 0% ("no drift") — the same
# unmeasured-is-not-zero discipline lib/harness.sh's mem_num_or_null enforces for RSS.
check "bq_drift_pct base 0 -> empty"  "" "$(bq_drift_pct 77 0)"
check "bq_drift_pct no obs -> empty"  "" "$(bq_drift_pct '' 77)"

# ═════════════════════════════════════════════════════════════════════════════════════════════════
echo "box_qualify_test: STAGE 1 — the real 2026-07-25 per-box floor drift, at the shipped +/-4% band"
# The measured per-box median drift of direct_c1_p99_us, this run vs the prior run. Nine healthy
# boxes and the one suspect box. A healthy floor of ~77us is used as the baseline for all of them.
BASE=77
check "agentgateway  -2.6% -> pass" pass "$(s1 x -2.6 4 "$BASE")"
check "apisix        -2.6% -> pass" pass "$(s1 x -2.6 4 "$BASE")"
check "litellm-rust  -2.5% -> pass" pass "$(s1 x -2.5 4 "$BASE")"
check "one-api       -1.2% -> pass" pass "$(s1 x -1.2 4 "$BASE")"
check "litellm-py    +0.6% -> pass" pass "$(s1 x  0.6 4 "$BASE")"
check "busbar        +1.3% -> pass" pass "$(s1 x  1.3 4 "$BASE")"
check "portkey       +1.3% -> pass" pass "$(s1 x  1.3 4 "$BASE")"
check "bifrost       +2.7% -> pass" pass "$(s1 x  2.7 4 "$BASE")"
# The THINNEST healthy margin in the whole calibration set: 3.8% against a 4.0% band = 0.2 points of
# headroom. If this case ever flips to fail, the band was tightened past the evidence.
check "tensorzero    +3.8% -> pass (0.2pt headroom)" pass "$(s1 x  3.8 4 "$BASE")"
# THE ONE THE GATE EXISTS FOR.
check "gomodel       +5.4% -> FAIL" fail "$(s1 x  5.4 4 "$BASE")"
s1 x 5.4 4 "$BASE" >/dev/null; check "gomodel reason" "floor_drift" "$BQ_REASON"

echo "box_qualify_test: STAGE 1 — the band is ONE-SIDED (a box cannot get faster from contention)"
# THE INCIDENT THIS EXISTS FOR (2026-07-25, second run). The band shipped SYMMETRIC, so the gate
# terminated boxes for BEATING their baseline: one-api read 75us against a 79us baseline (-5.06%) and
# tensorzero 75us against 81us (-7.41%). Both were rejected and replaced, and with 7 of 13 baselines
# sitting above that run's box population the gate was on course to burn every replacement and skip
# gateways outright — including busbar.
# WHY ONE-SIDED IS CORRECT, not a loosening: contention, throttling and noisy neighbours only ever ADD
# latency. A box cannot randomly get FASTER. A floor that beats its baseline is the box showing its
# true clean-hardware speed, and it implies the BASELINE was the noisy measurement. The absolute
# envelope still bounds absurd values on both sides.
check "one-api      -5.06% (faster) -> pass" pass "$(s1 x -5.06 4 "$BASE")"
check "tensorzero   -7.41% (faster) -> pass" pass "$(s1 x -7.41 4 "$BASE")"
check "far faster    -40%  (faster) -> pass" pass "$(s1 x -40   4 "$BASE")"
# ...while the SLOW side keeps biting at exactly the shipped band.
check "just over    +4.01% (slower) -> FAIL" fail "$(s1 x  4.01 4 "$BASE")"

echo "box_qualify_test: STAGE 1 — why a static absolute threshold could NOT have caught it"
# gomodel's absolute floor was 77-81us; the healthy population was 73-82us. Every one of those values
# is INSIDE the shipped sanity envelope, so the envelope alone returns pass for the bad box too —
# which is precisely why the PRIMARY gate is per-box drift and not a constant.
for v in 73 74 77 78 81 82; do
  bq_stage1_verdict "$v" "$v" "$v" "$(awk -v x="$v" 'BEGIN{printf "%.1f",x/2.2}')" 20000 "" ""
  check "absolute ${v}us with NO history -> seed (envelope alone cannot judge it)" seed "$BQ_VERDICT"
done
# ...but the envelope still does its own job: grossly bad hardware.
bq_stage1_verdict 240 235 245 100 20000 "" ""
check "240us floor -> fail (grossly slow box)" fail "$BQ_VERDICT"
check "240us reason" "floor_out_of_envelope" "$BQ_REASON"
bq_stage1_verdict 77 74 81 35 12 "" ""
check "12 rps c1 window -> fail (box cannot drive the rig)" fail "$BQ_VERDICT"
check "no-throughput reason" "floor_no_throughput" "$BQ_REASON"
# An unmeasurable floor is NOT a pass-by-default: we cannot qualify a box we failed to measure.
bq_stage1_verdict "" "" "" "" "" 77 77
check "no floor sample -> fail" fail "$BQ_VERDICT"
check "no floor sample reason" "floor_unmeasured" "$BQ_REASON"

echo "box_qualify_test: STAGE 1 — the JITTER gate (median normal, tail contended)"
# THE CASE THE DRIFT GATE MISSES on a first run: the median sits dead on the baseline, so drift is
# 0.0% and the absolute envelope is happy, but the samples are all over the place.
check "median on baseline, 40% spread -> FAIL" fail "$(s1 x 0 40 "$BASE")"
s1 x 0 40 "$BASE" >/dev/null; check "jitter reason" "floor_jitter_spread" "$BQ_REASON"
check "median on baseline, 8% spread  -> pass" pass "$(s1 x 0 8 "$BASE")"
# The other jitter face: a fat p99/p50 ratio at a normal median.
bq_stage1_verdict 77 75 79 12 20000 77 77
check "p99/p50 = 6.4 -> FAIL (contended tail)" fail "$BQ_VERDICT"
check "tail reason" "floor_jitter_tail" "$BQ_REASON"

echo "box_qualify_test: STAGE 1 — first run / no history is SEEDED, never blocked"
bq_stage1_verdict 77 75 79 35 20000 "" ""
check "no baseline -> seed"        seed "$BQ_VERDICT"
check "no baseline reason" "no_baseline" "$BQ_REASON"
check "no baseline drift is EMPTY, not 0" "" "$BQ_DRIFT_PCT"

echo "box_qualify_test: STAGE 1 — the ANTI-RATCHET bound (every step passed, the total did not)"
# 3% per run, five runs: each individual step is inside the +/-4% per-run band, but the box has now
# crept ~15% from where it started. Without the long-horizon bound this passes forever.
bq_stage1_verdict 88.5 87 90 40 20000 86 77
check "crept +15% from the oldest -> FAIL" fail "$BQ_VERDICT"
check "ratchet reason" "floor_ratchet" "$BQ_REASON"
# ...and a run that is genuinely close to both references still passes.
bq_stage1_verdict 78 77 79 35 20000 77 76
check "close to rolling AND oldest -> pass" pass "$BQ_VERDICT"

# ═════════════════════════════════════════════════════════════════════════════════════════════════
echo "box_qualify_test: STAGE 2 — the real 2026-07-25 peak replay deltas, at the shipped +/-25% band"
# rps_max_proxy, this run vs the prior run. Baseline 10000 rps for readability; the deltas are the
# measured ones. Stage 2 separates healthy-from-bad by 13% vs 86% — an enormous margin next to
# stage 1's 3.8% vs 5.4%, which is exactly why it is the strong gate.
p2(){ # delta_pct baseline [oldest]
  local base="$2" obs
  obs="$(awk -v b="$base" -v d="$1" 'BEGIN{printf "%d", b*(1+d/100)}')"
  bq_stage2_verdict "$obs" "$base" "${3:-}" "openai>openai"
  printf '%s' "$BQ_VERDICT"
}
check "agentgateway -13% -> pass" pass "$(p2 -13 10000)"
check "tensorzero   -12% -> pass" pass "$(p2 -12 10000)"
check "apisix        -8% -> pass" pass "$(p2  -8 10000)"
check "bifrost       -4% -> pass" pass "$(p2  -4 10000)"
check "busbar        -0% -> pass" pass "$(p2   0 10000)"
check "portkey       -3% -> pass" pass "$(p2  -3 10000)"
check "litellm-rust  +8% -> pass" pass "$(p2   8 10000)"
check "kong         +13% -> pass" pass "$(p2  13 10000)"
# THE ONE THE GATE EXISTS FOR — and note the margin: -86% against a -25% band.
check "gomodel      -86% -> FAIL" fail "$(p2 -86 10000)"
# ONE-SIDED here too, mirrored: throughput is inverted (higher is better), so only a SHORTFALL is a
# regression. A gateway that beats its recorded peak must never cost a box relaunch.
check "kong         +13% (faster) -> pass" pass "$(p2  13 10000)"
check "far faster  +200% (faster) -> pass" pass "$(p2 200 10000)"
check "just under   -25.1% -> FAIL"  fail "$(p2 -25.1 10000)"
p2 -86 10000 >/dev/null; check "gomodel stage-2 reason" "peak_drift" "$BQ_REASON"
check "gomodel stage-2 drift recorded" "-86.00" "$BQ_DRIFT_PCT"
# The band is why stage 2 is NOT 4%: six of the eight healthy gateways above would be rejected.
BQ_PEAK_DRIFT_PCT=4 check "at a 4% band a healthy -13% would FAIL (why the bands differ)" fail \
  "$(BQ_PEAK_DRIFT_PCT=4 p2 -13 10000)"

echo "box_qualify_test: STAGE 2 — no history, dead replay, anti-ratchet"
bq_stage2_verdict 9000 "" "" "openai>openai"
check "no baseline -> seed"        seed "$BQ_VERDICT"
bq_stage2_verdict 9000 10000 "" ""
check "no recorded cell -> seed"   seed "$BQ_VERDICT"
bq_stage2_verdict 0 10000 "" "openai>openai"
check "replay delivered nothing -> fail" fail "$BQ_VERDICT"
check "dead replay reason" "peak_unmeasured" "$BQ_REASON"
# Anti-ratchet on the peak lane: -20% per run passes the band every time, but the cumulative slide
# from the oldest retained peak does not.
bq_stage2_verdict 5000 6200 10000 "openai>openai"
check "crept -50% from the oldest -> FAIL" fail "$BQ_VERDICT"
check "peak ratchet reason" "peak_ratchet" "$BQ_REASON"

# ═════════════════════════════════════════════════════════════════════════════════════════════════
echo "box_qualify_test: BASELINES — rolling median, arch filter, and FAILED runs never seed"
TMPD="$(mktemp -d)"; trap 'rm -rf "$TMPD"' EXIT
mksnap(){ # file measured_at arch floor verdict [peak] [conc]
  local f="$TMPD/result_gw_$2.json"
  python3 - "$f" "$2" "$3" "$4" "$5" "${6:-}" "${7:-}" <<'PY'
import json, sys
path, at, arch, floor, verdict, peak, conc = sys.argv[1:8]
snap = {"gateway": "gw", "measured_at": at, "arch": arch,
        "rig": {"box_qualify": {"verdict": verdict,
                                "stage1": {"floor_p99_us_median": float(floor)}}},
        "matrix": {}}
if peak:
    snap["matrix"] = {"memory": {"load_cell": "openai>openai"},
                      "upstreams": {"openai": {"cells": {"openai": {"perf": {
                          "rps_max_proxy": float(peak),
                          "rps_max_proxy_concurrency": int(conc or 0)}}}}}}
json.dump(snap, open(path, "w"))
PY
}
# Five qualified runs plus ONE contaminated run that FAILED qualification. The contaminated value is
# wild (120us / 1400 rps): if it were allowed to seed, both baselines would move visibly.
mksnap x 2026-07-20T00-00-00Z arm64 76 pass 10000 208
mksnap x 2026-07-21T00-00-00Z arm64 77 pass 10200 208
mksnap x 2026-07-22T00-00-00Z arm64 78 pass  9900 208
mksnap x 2026-07-23T00-00-00Z arm64 77 pass 10100 192
mksnap x 2026-07-24T00-00-00Z arm64 76 pass  9800 208
mksnap x 2026-07-25T00-00-00Z arm64 120 fail 1400 208
mksnap x 2026-07-25T01-00-00Z x86   200 pass  2000 64     # a DIFFERENT arch: must never mix in
bq_load_baselines gw arm64 "$TMPD"
check "rolling floor baseline (median of last 5 GOOD)" "77" "$BQ_BL_FLOOR_US"
check "floor baseline count"                            "5" "$BQ_BL_FLOOR_N"
check "oldest retained floor (anti-ratchet reference)" "76" "$BQ_BL_FLOOR_OLDEST_US"
check "rolling peak baseline"                       "10000" "$BQ_BL_PEAK_RPS"
check "replay cell comes from the newest GOOD run" "openai>openai" "$BQ_BL_PEAK_CELL"
check "replay concurrency comes from the newest GOOD run" "208" "$BQ_BL_PEAK_CONC"
# THE POINT OF THE ROLLING WINDOW: the contaminated run above did not become the reference. Prove the
# same protection holds even when a bad run somehow qualifies — a median over 5 absorbs one outlier.
mksnap x 2026-07-25T02-00-00Z arm64 120 pass 1400 208
bq_load_baselines gw arm64 "$TMPD"
check "one wild PASSED run cannot own the floor baseline" "77" "$BQ_BL_FLOOR_US"
check "one wild PASSED run cannot own the peak baseline" "9900" "$BQ_BL_PEAK_RPS"
# A GATEWAY WITH NO SNAPSHOTS AT ALL. The two lanes deliberately answer differently, and this is the
# "drop a folder in and it works" property:
#   FLOOR  is POOLED across every gateway on the arch, because direct_c1_p99_us is measured with NO
#          gateway in the path: it describes the rig and the instance type, not the gateway. So a
#          brand new gateway inherits the field's floor history and is protected from a bad box on
#          its very FIRST run, with no per-gateway history of its own.
#   PEAK   is per gateway, because throughput genuinely is a property of the gateway. A new gateway
#          has none, so stage 2 seeds instead of blocking.
bq_load_baselines nosuchgw arm64 "$TMPD"
check "unknown gateway -> INHERITS the pooled floor" "77" "$BQ_BL_FLOOR_US"
check "unknown gateway -> no peak baseline"   "" "$BQ_BL_PEAK_RPS"
check "unknown gateway -> no replay cell"     "" "$BQ_BL_PEAK_CELL"
# ...and the pooled floor must still be arch-scoped: a different arch shares nothing.
bq_load_baselines nosuchgw x86_64 "$TMPD"
check "unknown gateway, other arch -> no floor" "" "$BQ_BL_FLOOR_US"

echo "box_qualify_test: PROVENANCE — the run's raw evidence is always recorded, pass or fail"
bq_stage1_verdict 81 79 83 36 20000 76.85 76
P="$(bq_provenance_json fail 3 '{"floor_p99_us_median": 81}' 'null' "$BQ_REASON" "$BQ_DETAIL")"
check "provenance parses as JSON" "ok" "$(printf '%s' "$P" | python3 -c 'import json,sys; json.load(sys.stdin); print("ok")' 2>/dev/null)"
check "provenance carries the verdict" "fail" "$(printf '%s' "$P" | python3 -c 'import json,sys; print(json.load(sys.stdin)["verdict"])')"
check "provenance carries the attempts" "3" "$(printf '%s' "$P" | python3 -c 'import json,sys; print(json.load(sys.stdin)["attempts"])')"
check "provenance flags the bands PROVISIONAL" "True" "$(printf '%s' "$P" | python3 -c 'import json,sys; print(json.load(sys.stdin)["thresholds"]["provisional"])')"
check "provenance records the recalibration target" "2.5" "$(printf '%s' "$P" | python3 -c 'import json,sys; print(json.load(sys.stdin)["thresholds"]["recalibration_target_pct"])')"
check "provenance records the raw stage-1 measurement" "81" "$(printf '%s' "$P" | python3 -c 'import json,sys; print(json.load(sys.stdin)["stage1"]["floor_p99_us_median"])')"
# A run with no stage-2 lane records null there — never a fabricated zero.
check "absent stage 2 -> null" "None" "$(printf '%s' "$P" | python3 -c 'import json,sys; print(json.load(sys.stdin)["stage2"])')"

if [ "$FAILED" = 0 ]; then echo "box_qualify_test: PASS"; else echo "box_qualify_test: FAIL"; fi
exit "$FAILED"
