#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors
#
# BOX QUALIFICATION — the verdict engine. "Detect a bad EC2 box UP FRONT and replace it, instead of
# spending a multi-hour 6x6 run on contaminated hardware and publishing a fake gateway regression."
#
# ─────────────────────────────────────────────────────────────────────────────────────────────────
# THE INCIDENT THIS EXISTS FOR
# `direct_c1_p99_us` is the rig's OWN floor: the load generator hitting the MOCK directly, with NO
# gateway anywhere in the path. It measures the BOX, not the gateway. In the 2026-07-25 field run vs
# the run before it, the per-box median drift of that floor was:
#
#     -2.6%  -2.6%  -2.5%  -1.2%  +0.6%  +1.3%  +1.3%  +2.7%  +3.8%   (ten healthy boxes)
#     +5.4%                                                             (one suspect box)
#
# One box per gateway, one gateway per box; which entrant happened to sit on the bad box is not what
# the gate keys on and is deliberately not recorded here. The suspect box was contaminated - and its
# ABSOLUTE floor was 77-81us, INSIDE the healthy population's 73-82us. A static absolute threshold
# could not have caught it, and did not: the run published a BOX fault as a gateway regression when
# the honest finding was "we measured it on a bad box".
# That is the single fact that shapes this whole file: the gate must be RELATIVE TO THAT BOX'S OWN
# HISTORY, not to a global constant. The absolute envelope below is retained only as a coarse
# catch-all for grossly bad hardware.
#
# ─────────────────────────────────────────────────────────────────────────────────────────────────
# TWO STAGES, TWO DIFFERENT MEASUREMENTS
#
# STAGE 1 — the no-gateway FLOOR probe. Runs on the box BEFORE the gateway is built or launched, so a
#   grossly bad box is rejected before we pay the build cost. Cheap (tens of seconds). Three signals:
#     (a) DRIFT of the floor median vs this gateway+arch's own rolling baseline   [PRIMARY]
#     (b) JITTER/stability across the repeated samples                            [SECONDARY]
#     (c) an ABSOLUTE sanity envelope + a throughput sanity floor                 [CATCH-ALL]
#   (b) exists because a contended or throttled box often shows an elevated tail spread while its
#   median still looks normal — the exact case (a) can miss on a FIRST run, where there is no history.
#
# STAGE 2 — the PEAK REPLAY. Runs after the gateway boots but BEFORE the 6x6 begins: replay THAT
#   gateway's own last peak cell at its own recorded winning concurrency and compare achieved
#   throughput to the recorded value. It exercises the real measured path (gateway + mock + loadgen)
#   and is FAR more discriminating than the floor. Same run pair, rps_max_proxy:
#
#     healthy: -13%  -12%  -8%  -4%  -3%  -0%  +8%  +13%      -> max |delta| 13%
#     suspect: -86%
#
#   Floor separates healthy-from-bad by 3.8% vs 5.4% (thin). Peak replay separates 13% vs 86%
#   (enormous). Stage 2 is therefore the strong gate; stage 1 is the cheap early one.
#
# ─────────────────────────────────────────────────────────────────────────────────────────────────
# THRESHOLDS — ALL PROVISIONAL. READ THIS BEFORE CHANGING ONE.
#
#   current defaults:  stage 1 = +/-4%   stage 2 = +/-25%
#
# Both were sized against variance measured on a harness WITH KNOWN BUGS (frozen gemini/bedrock probe
# paths, the mock-relaunch EADDRINUSE race fixed in 4b548bf, gateway boot failures, ...). A material
# fraction of the observed spread may BE those bugs rather than genuine run-to-run noise.
#
#   TARGET after recalibration:  < 2.5% for BOTH, pending the frozen-codebase repeat runs.
#
#   THE RULE: if the repeat runs come in ABOVE 2.5%, the correct response is to HUNT the remaining
#   non-determinism — NOT to widen the band to accommodate it. A band that is widened to fit whatever
#   the rig happens to do is a rubber stamp, not a measurement-quality gate.
#
# Why the two bands differ by so much: they measure different things with different natural variance.
# A 4% band on stage 2 would false-reject 6 of the 8 HEALTHY boxes above and put the rig into
# permanent relaunch churn; 25% still catches the -86% class with an enormous margin.
#
# Stage 1 at 4% is DELIBERATELY TIGHT and the margin is THIN: the worst healthy box drifted
# +3.8% against a 4.0% band — 0.2 percentage points of headroom. That is accepted on purpose. A
# deviation larger than 4% must never be shrugged off as normal box variance; a false reject is cheap
# (bounded box relaunch, self-healing) while a false accept publishes bad data. Err tight.
#
# RECALIBRATION IS A ONE-LINE CHANGE PLUS A DATA QUERY: every knob below is env-overridable with its
# default in this one block, and every run records its RAW measurement + computed drift into the
# snapshot provenance (see bq_provenance_json) whether it passes or fails — so after N repeat runs the
# true per-metric distribution can be computed from results/snapshots/ directly.
# ─────────────────────────────────────────────────────────────────────────────────────────────────

# ── STAGE 1 (no-gateway floor) ───────────────────────────────────────────────────────────────────
# Per-run drift band vs the ROLLING baseline. PROVISIONAL (see above); target <2.5%.
BQ_FLOOR_DRIFT_PCT="${BQ_FLOOR_DRIFT_PCT:-4.0}"
# ANTI-RATCHET. A box could drift 3% every run, always pass the per-run band, and creep arbitrarily far
# from the original floor. So a second, longer-horizon bound against the OLDEST retained qualified
# median catches cumulative creep even when every individual step passed. 2x the per-run band.
BQ_FLOOR_RATCHET_PCT="${BQ_FLOOR_RATCHET_PCT:-8.0}"
# ABSOLUTE sanity envelope (us). Coarse catch-all for grossly bad hardware and the ONLY floor gate
# available on a first run with no history. The healthy population sat at 73-82us on arm64
# m7g.4xlarge; this envelope is deliberately much wider so it never false-rejects — it exists to catch
# a box that is 2x slow, not to catch the inside-the-envelope class above (which it demonstrably cannot).
BQ_FLOOR_ABS_MIN_US="${BQ_FLOOR_ABS_MIN_US:-45}"
BQ_FLOOR_ABS_MAX_US="${BQ_FLOOR_ABS_MAX_US:-115}"
# JITTER / stability. UNCALIBRATED — we have no published per-sample spread from the historical runs,
# only per-run medians, so these two are set LOOSE on purpose (a false reject costs a box relaunch).
# They are recorded on every run so they can be calibrated from the repeat runs like everything else.
#   spread = (max - min) / median of the repeated per-sample p99 floors, in percent.
BQ_JITTER_SPREAD_PCT="${BQ_JITTER_SPREAD_PCT:-25}"
#   ratio  = median(p99) / median(p50) within the floor windows. A contended/throttled box shows an
#   elevated TAIL while its median looks normal — the case the drift gate misses on a first run.
BQ_JITTER_P99_P50_MAX="${BQ_JITTER_P99_P50_MAX:-4.0}"
# Throughput sanity floor for the c1 direct window (req/s). LOOSE: a 77us p99 floor implies well over
# 5000/s, so 2000 only catches a box that cannot deliver load at all.
BQ_MIN_C1_RPS="${BQ_MIN_C1_RPS:-2000}"

# ── STAGE 2 (peak replay) ────────────────────────────────────────────────────────────────────────
# Band vs the rolling baseline peak. PROVISIONAL; see the block above for why it is not 4%.
BQ_PEAK_DRIFT_PCT="${BQ_PEAK_DRIFT_PCT:-25}"
# Anti-ratchet for the same reason as stage 1, against the oldest retained qualified peak.
BQ_PEAK_RATCHET_PCT="${BQ_PEAK_RATCHET_PCT:-40}"

# ── baseline window ──────────────────────────────────────────────────────────────────────────────
# ROLLING baseline = the MEDIAN of the last N qualified measurements for this gateway+arch, NOT the
# single previous run. Comparing against only the immediately-previous run means one contaminated run
# silently becomes the next run's reference and the gate normalizes the contamination.
BQ_BASELINE_WINDOW="${BQ_BASELINE_WINDOW:-5}"
# The FLOOR window is separate and wider, because the floor baseline is POOLED across every gateway
# on the arch rather than kept per gateway (direct_c1_p99_us is measured with NO gateway in the path,
# so it describes the rig, not the gateway). Pooling yields roughly one sample per gateway per run,
# so a window of 15 spans about one full field run's box population and the median describes the
# population rather than one lucky or unlucky draw.
BQ_FLOOR_WINDOW="${BQ_FLOOR_WINDOW:-15}"
# How far back the anti-ratchet reference reaches (records, not runs of this window).
BQ_BASELINE_RETAIN="${BQ_BASELINE_RETAIN:-12}"
# Seed the baseline from PRE-FEATURE snapshots (which carry no qualification block) by reading their
# published direct_c1_p99_us / rps_max_proxy. Without this the gate is inert until it has accumulated
# its own history; with it the gate is live from the first run after this ships. A single contaminated
# legacy run cannot dominate because the baseline is a MEDIAN over the window.
BQ_ALLOW_LEGACY_BASELINE="${BQ_ALLOW_LEGACY_BASELINE:-1}"

# ── numeric helpers (awk: no bc/python dependency on the measurement path) ────────────────────────
# bq_median <v> [v...] -> the median, or EMPTY when there is no usable value. Never prints 0 for
# "unmeasured" (same discipline as lib/harness.sh's mem_num_or_null: 0 is a real number, not a gap).
bq_median(){
  [ "$#" -gt 0 ] || return 0
  printf '%s\n' "$@" | awk '
    /^-?[0-9]+(\.[0-9]+)?$/ { v[n++]=$1 }
    END{ if(n==0) exit
         for(i=0;i<n;i++) for(j=i+1;j<n;j++) if(v[j]<v[i]){t=v[i];v[i]=v[j];v[j]=t}
         if(n%2) printf "%.10g", v[(n-1)/2]; else printf "%.10g", (v[n/2-1]+v[n/2])/2 }'
}
bq_min(){ printf '%s\n' "$@" | awk '/^-?[0-9]+(\.[0-9]+)?$/{if(!s||$1<m){m=$1;s=1}} END{if(s)printf "%.10g",m}'; }
bq_max(){ printf '%s\n' "$@" | awk '/^-?[0-9]+(\.[0-9]+)?$/{if(!s||$1>m){m=$1;s=1}} END{if(s)printf "%.10g",m}'; }
# bq_drift_pct <observed> <baseline> -> signed percent drift, or EMPTY when it cannot be computed
# (missing value, or a baseline of 0 — never a fabricated 0% which would read as "no drift").
bq_drift_pct(){
  awk -v o="${1:-}" -v b="${2:-}" 'BEGIN{
    if (o=="" || b=="" || o+0!=o || b+0!=b || b+0==0) exit
    printf "%.2f", (o-b)/b*100 }'
}
# bq_abs <n> -> |n| (empty in, empty out)
bq_abs(){ awk -v v="${1:-}" 'BEGIN{ if(v=="") exit; if(v+0<0) v=-v; printf "%.2f", v }'; }
# bq_regression <drift_pct> <sense> -> the magnitude of the deviation in the DEGRADING direction only,
# or 0 when the deviation is an improvement. THE BANDS ARE ONE-SIDED, and this is why:
#   a box cannot randomly get FASTER. Contention, throttling and noisy neighbours only ever ADD
#   latency and REMOVE throughput. So a floor that beats its baseline is not a contamination signal —
#   it is the box showing its true clean-hardware speed (and it implies the BASELINE was the noisy
#   measurement, not this run). Rejecting it would terminate good boxes, burn the replacement budget,
#   and eventually skip the gateway entirely.
#   sense=lat -> higher is worse (p99 latency): a POSITIVE drift is the regression.
#   sense=rps -> higher is better (throughput): a NEGATIVE drift is the regression.
# Absurd values in the improving direction are still bounded by the absolute envelope
# (BQ_FLOOR_ABS_MIN_US/BQ_FLOOR_ABS_MAX_US), which is two-sided on purpose.
bq_regression(){ awk -v v="${1:-}" -v s="${2:-lat}" 'BEGIN{ if(v=="") exit; if(s=="rps") v=-v; if(v+0<0) v=0; printf "%.2f", v }'; }
# bq_le <a> <b> -> 0 when a <= b. Empty a is treated as "not measured" -> NOT a violation (the caller
# decides what an unmeasurable signal means; a gate must never fail on a value it never obtained).
bq_le(){ [ -n "${1:-}" ] || return 0; awk -v a="$1" -v b="$2" 'BEGIN{exit !(a+0<=b+0)}'; }

# ── the JSON-safe scalar emitter (the ONE null-safe choke point, mirroring lib/rig.sh) ────────────
bq_json_num(){ case "${1:-}" in ''|*[!0-9.+-]*) printf 'null';; *) printf '%s' "$1";; esac; }
bq_json_str(){ if [ -z "${1:-}" ]; then printf 'null'; else printf '"%s"' "$(printf '%s' "$1" | tr -d '"\\' | tr '\n' ' ')"; fi; }

# ═════════════════════════════════════════════════════════════════════════════════════════════════
# BASELINES — read from the EXISTING store, never a third one.
#
# The per-run snapshots (results/snapshots/result_<gw>_<measured_at>.json, the ebd1c07 instrument
# provenance lives inside them) are the append-only, git-tracked, one-file-per-run history this needs:
# they carry arch, measured_at, the full matrix, and — from this feature on — the qualification block
# under rig.box_qualify. Only records whose qualification PASSED (or, when BQ_ALLOW_LEGACY_BASELINE=1,
# pre-feature records that predate the gate entirely) may seed a baseline.
#
# bq_read_baselines <gateway> <arch> <snapshots_dir> -> prints KEY=VALUE lines for `eval`-free reading:
#   BQ_BL_FLOOR_US / BQ_BL_FLOOR_OLDEST_US / BQ_BL_FLOOR_N / BQ_BL_FLOOR_SRC
#   BQ_BL_PEAK_RPS / BQ_BL_PEAK_OLDEST_RPS / BQ_BL_PEAK_N / BQ_BL_PEAK_CELL / BQ_BL_PEAK_CONC / BQ_BL_PEAK_SRC
# Any field that cannot be derived is emitted EMPTY (= no history: the caller must then SEED, never
# block — see bq_stage1_verdict / bq_stage2_verdict).
# ═════════════════════════════════════════════════════════════════════════════════════════════════
bq_read_baselines(){ # gateway arch snapshots_dir
  python3 - "$1" "$2" "$3" "$BQ_BASELINE_WINDOW" "$BQ_BASELINE_RETAIN" "$BQ_ALLOW_LEGACY_BASELINE" "$BQ_FLOOR_WINDOW" <<'PY'
import glob, json, os, statistics, sys

gw, arch, snapdir, win, retain, allow_legacy, floor_win = sys.argv[1:8]
win, retain, allow_legacy, floor_win = int(win), int(retain), allow_legacy == "1", int(floor_win)

def med(vs):
    return statistics.median(vs) if vs else None

def fmt(v):
    # Plain decimal, never scientific notation: these values are read back by shell string
    # comparisons and folded into JSON, where "1e+04" is both unreadable and not a shell number.
    return "%d" % round(v) if abs(v - round(v)) < 1e-9 else ("%.10g" % v)

def num(x):
    return x if isinstance(x, (int, float)) and not isinstance(x, bool) else None

recs = []
# EVERY gateway's snapshots are read, not just this one's. The FLOOR is pooled across the whole
# field (see the lane() calls below); only the PEAK is filtered back down to this gateway.
for path in sorted(glob.glob(os.path.join(snapdir, "result_*.json"))):
    try:
        with open(path) as fh:
            j = json.load(fh)
    except Exception:
        continue                      # a corrupt snapshot is not a baseline, and never fatal here
    if arch and j.get("arch") and j.get("arch") != arch:
        continue                      # a baseline is always per arch
    m = j.get("matrix") or {}
    bq = ((j.get("rig") or {}).get("box_qualify")) or {}
    qualified = (bq.get("verdict") == "pass")
    legacy = not bq                   # predates the gate entirely
    if not qualified and not (legacy and allow_legacy):
        continue                      # a FAILED qualification must never seed the baseline
    rec = {"measured_at": j.get("measured_at") or "", "legacy": legacy,
           "gw": j.get("gateway") or ""}
    # ── floor: this run's own stage-1 median when it has one; else the published direct_c1_p99_us,
    # which is the SAME measurement (loadgen -> mock, c1, no gateway) taken by the 6x6 itself.
    f = num(((bq.get("stage1") or {}).get("floor_p99_us_median")))
    if f is None and legacy:
        # The stage-1 probe measures the OPENAI direct-to-mock window specifically (same path, same
        # body shape, same psize), so seed from the openai-ingress cells alone when there are any —
        # the per-dialect direct baselines differ slightly and mixing them would bias the baseline.
        floors, any_floors = [], []
        for eg, up in (m.get("upstreams") or {}).items():
            for ing, cell in ((up or {}).get("cells") or {}).items():
                d = num(((cell or {}).get("perf") or {}).get("direct_c1_p99_us"))
                if d:
                    any_floors.append(d)
                    if ing == "openai":
                        floors.append(d)
        f = med(floors or any_floors)
        rec["floor_src"] = "legacy-direct-c1"
    elif f is not None:
        rec["floor_src"] = "stage1"
    rec["floor"] = f
    # ── peak: the run's OWN peak cell (the memory window's load_cell = highest certified
    # rps_max_proxy) and that cell's throughput + winning concurrency. Replaying THE SAME CELL is
    # what makes stage 2 a comparison of like with like.
    cell = ((m.get("memory") or {}).get("load_cell")) or ""
    eg = ing = ""
    if isinstance(cell, str) and ">" in cell:
        ing, eg = [p.strip() for p in cell.split(">", 1)]
    perf = (((m.get("upstreams") or {}).get(eg) or {}).get("cells") or {}).get(ing) or {}
    perf = perf.get("perf") or {}
    rec["peak"] = num(perf.get("rps_max_proxy"))
    rec["peak_conc"] = num(perf.get("rps_max_proxy_concurrency"))
    rec["peak_cell"] = cell if (rec["peak"] and ing and eg) else ""
    rec["peak_src"] = "legacy-matrix" if legacy else "matrix"
    recs.append(rec)

recs.sort(key=lambda r: r["measured_at"])
out = {}

def lane(key, cell_key=None, conc_key=None, src_key=None, prefix="", only_gw=False, window=None):
    src = [r for r in recs if not only_gw or r.get("gw") == gw]
    vals = [(r[key], r) for r in src if r.get(key)]
    w = window or win
    # Retain must never starve the window, or the median would silently be taken over fewer samples
    # than asked for (the pooled floor window is wider than the default retain).
    keep = vals[-max(retain, w):]
    sample = [v for v, _ in keep[-w:]]
    out[prefix + "N"] = str(len(sample))
    out[prefix + "VAL"] = fmt(med(sample)) if sample else ""
    out[prefix + "OLDEST"] = fmt(keep[0][0]) if keep else ""
    out[prefix + "SRC"] = keep[-1][1].get(src_key, "") if keep else ""
    return keep

# FLOOR: POOLED ACROSS EVERY GATEWAY on this arch, deliberately.
#   direct_c1_p99_us is the load generator hitting the mock with NO gateway in the path. It measures
#   the RIG and the INSTANCE TYPE, not the gateway, so every gateway is sampling the same population
#   and a per-gateway baseline is a category error. It is also actively harmful: with one or two
#   historical samples each, a gateway's baseline is a random draw from that population, so a gateway
#   that happened to draw a fast box rejects perfectly normal ones while a gateway that drew a slow
#   one accepts almost anything. Observed live on 2026-07-25: one box carried a 73us baseline and was
#   rejected at 77us, a value squarely inside that same run's healthy spread of 70-80us, while a
#   genuinely bad box at 89us was correctly caught. Pooling gives ~one sample per gateway per run, so
#   the reference converges immediately and means the same thing for everyone.
# PEAK: per gateway, because throughput genuinely IS a property of the gateway under test.
lane("floor", src_key="floor_src", prefix="FLOOR_", window=floor_win)
peak_keep = lane("peak", src_key="peak_src", prefix="PEAK_", only_gw=True)
# The cell + concurrency to REPLAY come from the MOST RECENT qualified record, not the median: they
# are identifiers, not measurements, and replaying the cell that produced the newest baseline is what
# keeps the comparison like-for-like.
cell = conc = ""
if peak_keep:
    cell = peak_keep[-1][1].get("peak_cell") or ""
    c = peak_keep[-1][1].get("peak_conc")
    conc = str(int(c)) if c else ""

print("BQ_BL_FLOOR_US=%s" % out["FLOOR_VAL"])
print("BQ_BL_FLOOR_OLDEST_US=%s" % out["FLOOR_OLDEST"])
print("BQ_BL_FLOOR_N=%s" % out["FLOOR_N"])
print("BQ_BL_FLOOR_SRC=%s" % out["FLOOR_SRC"])
print("BQ_BL_PEAK_RPS=%s" % out["PEAK_VAL"])
print("BQ_BL_PEAK_OLDEST_RPS=%s" % out["PEAK_OLDEST"])
print("BQ_BL_PEAK_N=%s" % out["PEAK_N"])
print("BQ_BL_PEAK_SRC=%s" % out["PEAK_SRC"])
print("BQ_BL_PEAK_CELL=%s" % cell)
print("BQ_BL_PEAK_CONC=%s" % conc)
PY
}

# bq_load_baselines <gateway> <arch> <snapshots_dir> — run bq_read_baselines and assign the BQ_BL_*
# variables in the CALLER's scope. Parsed line-by-line (never `eval`): the values come from a
# git-tracked JSON tree, but a store that can inject shell is a store that can own the orchestrator.
bq_load_baselines(){
  BQ_BL_FLOOR_US=""; BQ_BL_FLOOR_OLDEST_US=""; BQ_BL_FLOOR_N=0; BQ_BL_FLOOR_SRC=""
  BQ_BL_PEAK_RPS=""; BQ_BL_PEAK_OLDEST_RPS=""; BQ_BL_PEAK_N=0; BQ_BL_PEAK_SRC=""
  BQ_BL_PEAK_CELL=""; BQ_BL_PEAK_CONC=""
  local line k v
  while IFS= read -r line; do
    k="${line%%=*}"; v="${line#*=}"
    # Only ever accept the ten keys we asked for, and only safe characters.
    case "$v" in *[!A-Za-z0-9._\>+-]*) continue;; esac
    case "$k" in
      BQ_BL_FLOOR_US)        BQ_BL_FLOOR_US="$v";;
      BQ_BL_FLOOR_OLDEST_US) BQ_BL_FLOOR_OLDEST_US="$v";;
      BQ_BL_FLOOR_N)         BQ_BL_FLOOR_N="${v:-0}";;
      BQ_BL_FLOOR_SRC)       BQ_BL_FLOOR_SRC="$v";;
      BQ_BL_PEAK_RPS)        BQ_BL_PEAK_RPS="$v";;
      BQ_BL_PEAK_OLDEST_RPS) BQ_BL_PEAK_OLDEST_RPS="$v";;
      BQ_BL_PEAK_N)          BQ_BL_PEAK_N="${v:-0}";;
      BQ_BL_PEAK_SRC)        BQ_BL_PEAK_SRC="$v";;
      BQ_BL_PEAK_CELL)       BQ_BL_PEAK_CELL="$v";;
      BQ_BL_PEAK_CONC)       BQ_BL_PEAK_CONC="$v";;
    esac
  done < <(bq_read_baselines "$1" "$2" "$3" 2>/dev/null)
}

# ═════════════════════════════════════════════════════════════════════════════════════════════════
# STAGE 1 VERDICT — the no-gateway floor.
#
#   bq_stage1_verdict <floor_median_us> <floor_min_us> <floor_max_us> <p50_median_us> <c1_rps> \
#                     <baseline_us> <baseline_oldest_us>
#
# Sets: BQ_VERDICT   pass | fail | seed   ("seed" = accepted, and this run becomes the baseline)
#       BQ_REASON    a short machine-ish token
#       BQ_DETAIL    the honest human sentence, WITH the numbers
#       BQ_DRIFT_PCT / BQ_RATCHET_PCT / BQ_JITTER_PCT / BQ_JITTER_RATIO  (recorded either way)
#
# NO-HISTORY: a new gateway, a new arch, or the first run after this ships cannot compute drift. That
# is never a reason to block a run: fall back to the absolute envelope + the jitter gate, ACCEPT, and
# record this floor as the seed baseline (verdict "seed", which downstream treats as a pass and stores).
# ═════════════════════════════════════════════════════════════════════════════════════════════════
bq_stage1_verdict(){
  local med="${1:-}" lo="${2:-}" hi="${3:-}" p50="${4:-}" rps="${5:-}" base="${6:-}" oldest="${7:-}"
  BQ_VERDICT=pass; BQ_REASON=ok; BQ_DETAIL=""
  BQ_DRIFT_PCT=""; BQ_RATCHET_PCT=""; BQ_JITTER_PCT=""; BQ_JITTER_RATIO=""
  # Always COMPUTE and RECORD the derived signals first, gate second: the raw numbers are calibration
  # evidence and must be persisted whether or not a band trips.
  BQ_JITTER_PCT="$(awk -v a="$hi" -v b="$lo" -v m="$med" 'BEGIN{ if(a==""||b==""||m==""||m+0==0) exit; printf "%.2f",(a-b)/m*100 }')"
  BQ_JITTER_RATIO="$(awk -v a="$med" -v b="$p50" 'BEGIN{ if(a==""||b==""||b+0==0) exit; printf "%.2f", a/b }')"
  BQ_DRIFT_PCT="$(bq_drift_pct "$med" "$base")"
  BQ_RATCHET_PCT="$(bq_drift_pct "$med" "$oldest")"

  # 0. the measurement itself must exist. An unmeasurable floor is NOT a pass-by-default: we cannot
  #    qualify a box we failed to measure, and running the 6x6 on it is exactly the incident again.
  if [ -z "$med" ]; then
    BQ_VERDICT=fail; BQ_REASON=floor_unmeasured
    BQ_DETAIL="the no-gateway floor probe produced no usable p99 sample, so this box could not be qualified at all"
    return 0
  fi
  # 1. ABSOLUTE envelope — grossly bad hardware, and the only floor signal available with no history.
  if ! bq_le "$med" "$BQ_FLOOR_ABS_MAX_US" || ! bq_le "$BQ_FLOOR_ABS_MIN_US" "$med"; then
    BQ_VERDICT=fail; BQ_REASON=floor_out_of_envelope
    BQ_DETAIL="floor p99 ${med}us is outside the ${BQ_FLOOR_ABS_MIN_US}-${BQ_FLOOR_ABS_MAX_US}us sanity envelope for this arch"
    return 0
  fi
  # 2. THROUGHPUT sanity (loose): a box that cannot deliver load at all.
  if [ -n "$rps" ] && ! bq_le "$BQ_MIN_C1_RPS" "$rps"; then
    BQ_VERDICT=fail; BQ_REASON=floor_no_throughput
    BQ_DETAIL="the c1 direct-to-mock window delivered only ${rps} req/s (< ${BQ_MIN_C1_RPS}/s): the box cannot drive the rig"
    return 0
  fi
  # 3. JITTER / stability — the signal a static median check misses. A contended or throttled box
  #    shows elevated tail spread even when its median looks normal.
  if ! bq_le "$BQ_JITTER_PCT" "$BQ_JITTER_SPREAD_PCT"; then
    BQ_VERDICT=fail; BQ_REASON=floor_jitter_spread
    BQ_DETAIL="floor samples spread ${BQ_JITTER_PCT}% (max-min over median; ${lo}-${hi}us) > ${BQ_JITTER_SPREAD_PCT}%: the box is unstable under a constant load"
    return 0
  fi
  if ! bq_le "$BQ_JITTER_RATIO" "$BQ_JITTER_P99_P50_MAX"; then
    BQ_VERDICT=fail; BQ_REASON=floor_jitter_tail
    BQ_DETAIL="floor p99/p50 ratio ${BQ_JITTER_RATIO} > ${BQ_JITTER_P99_P50_MAX}: the tail is contended while the median looks normal"
    return 0
  fi
  # 4. NO HISTORY -> seed. Never block a first run for lack of history.
  if [ -z "$base" ]; then
    BQ_VERDICT=seed; BQ_REASON=no_baseline
    BQ_DETAIL="no qualified floor history for this gateway+arch: accepted on the absolute envelope + jitter gates, and recorded as the seed baseline (${med}us)"
    return 0
  fi
  # 5. PRIMARY: drift vs the ROLLING baseline (median of the last N qualified runs, not the last run).
  if ! bq_le "$(bq_regression "$BQ_DRIFT_PCT" lat)" "$BQ_FLOOR_DRIFT_PCT"; then
    BQ_VERDICT=fail; BQ_REASON=floor_drift
    BQ_DETAIL="floor p99 ${med}us is ${BQ_DRIFT_PCT}% SLOWER than this gateway+arch's rolling baseline ${base}us (one-sided band +${BQ_FLOOR_DRIFT_PCT}%; a faster floor never fails)"
    return 0
  fi
  # 6. ANTI-RATCHET: cumulative creep against the oldest retained qualified median.
  if [ -n "$oldest" ] && ! bq_le "$(bq_regression "$BQ_RATCHET_PCT" lat)" "$BQ_FLOOR_RATCHET_PCT"; then
    BQ_VERDICT=fail; BQ_REASON=floor_ratchet
    BQ_DETAIL="floor p99 ${med}us is ${BQ_RATCHET_PCT}% SLOWER than the OLDEST retained qualified floor ${oldest}us (one-sided anti-ratchet bound +${BQ_FLOOR_RATCHET_PCT}%): the box population has crept slower even though each step passed"
    return 0
  fi
  BQ_DETAIL="floor p99 ${med}us, drift ${BQ_DRIFT_PCT}% vs baseline ${base}us (band +/-${BQ_FLOOR_DRIFT_PCT}%), spread ${BQ_JITTER_PCT}%, p99/p50 ${BQ_JITTER_RATIO}"
  return 0
}

# ═════════════════════════════════════════════════════════════════════════════════════════════════
# STAGE 2 VERDICT — the peak replay.
#
#   bq_stage2_verdict <observed_rps> <baseline_rps> <baseline_oldest_rps> <cell>
#
# Same rolling-baseline + anti-ratchet discipline as stage 1, a different band (see the header), and
# the same never-block-a-first-run rule: no recorded peak for this gateway+arch -> "seed".
# ═════════════════════════════════════════════════════════════════════════════════════════════════
bq_stage2_verdict(){
  local obs="${1:-}" base="${2:-}" oldest="${3:-}" cell="${4:-}"
  BQ_VERDICT=pass; BQ_REASON=ok; BQ_DETAIL=""
  BQ_DRIFT_PCT=""; BQ_RATCHET_PCT=""
  BQ_DRIFT_PCT="$(bq_drift_pct "$obs" "$base")"
  BQ_RATCHET_PCT="$(bq_drift_pct "$obs" "$oldest")"

  if [ -z "$base" ] || [ -z "$cell" ]; then
    BQ_VERDICT=seed; BQ_REASON=no_baseline
    BQ_DETAIL="no recorded peak cell for this gateway+arch: stage 2 skipped, this run's peak becomes the seed baseline"
    return 0
  fi
  if [ -z "$obs" ] || [ "$obs" = 0 ]; then
    BQ_VERDICT=fail; BQ_REASON=peak_unmeasured
    BQ_DETAIL="the peak replay of $cell delivered no successful throughput sample (baseline ${base} rps): the gateway or the box is not serving"
    return 0
  fi
  if ! bq_le "$(bq_regression "$BQ_DRIFT_PCT" rps)" "$BQ_PEAK_DRIFT_PCT"; then
    BQ_VERDICT=fail; BQ_REASON=peak_drift
    BQ_DETAIL="peak replay of $cell reached ${obs} rps, ${BQ_DRIFT_PCT}% BELOW the rolling baseline ${base} rps (one-sided band -${BQ_PEAK_DRIFT_PCT}%; exceeding the baseline never fails)"
    return 0
  fi
  if [ -n "$oldest" ] && ! bq_le "$(bq_regression "$BQ_RATCHET_PCT" rps)" "$BQ_PEAK_RATCHET_PCT"; then
    BQ_VERDICT=fail; BQ_REASON=peak_ratchet
    BQ_DETAIL="peak replay of $cell reached ${obs} rps, ${BQ_RATCHET_PCT}% BELOW the OLDEST retained qualified peak ${oldest} rps (one-sided anti-ratchet bound -${BQ_PEAK_RATCHET_PCT}%)"
    return 0
  fi
  BQ_DETAIL="peak replay of $cell reached ${obs} rps, drift ${BQ_DRIFT_PCT}% vs baseline ${base} rps (band +/-${BQ_PEAK_DRIFT_PCT}%)"
  return 0
}

# ═════════════════════════════════════════════════════════════════════════════════════════════════
# PROVENANCE — the qualifying measurement, recorded so a future cross-run comparison can see the
# instrument's state. This EXTENDS the ebd1c07 rig/instrument provenance (lib/rig.sh
# rig_provenance_json folds this object in under "box_qualify"); it is deliberately NOT a second
# mechanism. Every field is null-safe, so a consumer that predates it simply sees no key.
#
# THE RAW DATA IS ALWAYS WRITTEN, pass or fail — that is what makes recalibration a data query. After
# N frozen-codebase repeat runs, the true per-metric run-to-run distribution can be computed straight
# out of results/snapshots/*.json without re-running anything.
# ═════════════════════════════════════════════════════════════════════════════════════════════════
bq_provenance_json(){ # <verdict> <attempts> <stage1_json_or_empty> <stage2_json_or_empty> <reason> <detail>
  printf '{"verdict": %s, "attempts": %s, "reason": %s, "detail": %s, "thresholds": {"floor_drift_pct": %s, "floor_ratchet_pct": %s, "floor_abs_us": [%s, %s], "jitter_spread_pct": %s, "jitter_p99_p50_max": %s, "peak_drift_pct": %s, "peak_ratchet_pct": %s, "baseline_window": %s, "provisional": true, "recalibration_target_pct": 2.5}, "stage1": %s, "stage2": %s}' \
    "$(bq_json_str "${1:-}")" "$(bq_json_num "${2:-}")" "$(bq_json_str "${5:-}")" "$(bq_json_str "${6:-}")" \
    "$(bq_json_num "$BQ_FLOOR_DRIFT_PCT")" "$(bq_json_num "$BQ_FLOOR_RATCHET_PCT")" \
    "$(bq_json_num "$BQ_FLOOR_ABS_MIN_US")" "$(bq_json_num "$BQ_FLOOR_ABS_MAX_US")" \
    "$(bq_json_num "$BQ_JITTER_SPREAD_PCT")" "$(bq_json_num "$BQ_JITTER_P99_P50_MAX")" \
    "$(bq_json_num "$BQ_PEAK_DRIFT_PCT")" "$(bq_json_num "$BQ_PEAK_RATCHET_PCT")" \
    "$(bq_json_num "$BQ_BASELINE_WINDOW")" \
    "${3:-null}" "${4:-null}"
}
