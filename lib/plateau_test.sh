#!/usr/bin/env bash
# Tests for the steadiness test (lib/plateau.sh). RED-before proofs where it matters: the asymptoting
# leak case FAILS against a spread-only implementation, which is why the trend test exists.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
. "$HERE/otb.sh"
otb_resolve "$(cd "$HERE/.." && pwd)" || exit 1
. "$HERE/plateau.sh"

PASS=0; FAIL=0
ok(){ if [ "$2" = "$3" ]; then PASS=$((PASS+1)); else FAIL=$((FAIL+1)); echo "  FAIL: $1 (want '$3', got '$2')"; fi; }
# Builds a "<t> <rss>" series file. Args: start, per-sample delta, count, [jitter amplitude].
mkseries(){ local f="$1" v="$2" d="$3" n="$4" j="${5:-0}" i t=0
  : >"$f"
  for ((i=0;i<n;i++)); do
    awk -v t="$t" -v v="$v" -v i="$i" -v j="$j" 'BEGIN{ w = (j>0 ? ((i%2)?j:-j) : 0); printf "%d %.3f\n", t, v+w }' >>"$f"
    v=$(awk -v v="$v" -v d="$d" 'BEGIN{printf "%.3f", v+d}'); t=$((t+2))
  done
}
T="${TMPDIR:-/tmp}/plateautest.$$"; mkdir -p "$T"; trap 'rm -rf "$T"' EXIT

echo "plateau_check"
mkseries "$T/flat" 100 0 30
ok "a genuinely flat series is a plateau" "$(plateau_check "$T/flat" 1 2)" "1"

mkseries "$T/rise" 100 1 30
ok "a steadily rising series is NOT a plateau" "$(plateau_check "$T/rise" 1 2)" "0"

# THE CASE THE TREND TEST EXISTS FOR. Busbar's shape near the tail of its climb: still rising, but
# slowly enough that the total spread across a 60s window is under the range threshold. A spread-only
# implementation calls this settled. It is not settled - it is a leak that has not finished leaking.
# 30 samples, +0.05 MiB each, around 118 MiB: spread 1.45/118 = 1.2%, under a 2% range gate; but the
# second-half mean sits ~0.63% above the first-half mean, which the ONE-SIDED trend gate at 1% must
# still catch when tightened. Use a trend gate of 0.5 to assert the mechanism directly.
mkseries "$T/asymptote" 118 0.05 30
spread_only(){ awk '{n++;v[n]=$2+0} END{lo=v[1];hi=v[1];for(i=2;i<=n;i++){if(v[i]<lo)lo=v[i];if(v[i]>hi)hi=v[i];s+=v[i]}s+=v[1];m=s/n; print ((hi-lo)/m*100 < 2) ? 1 : 0}' "$1"; }
ok "RED: a spread-only test wrongly calls the asymptoting leak steady" "$(spread_only "$T/asymptote")" "1"
ok "GREEN: the trend test rejects the asymptoting leak" "$(plateau_check "$T/asymptote" 0.5 2)" "0"

# Oscillation around a flat mean: no trend, but sampling it gives a value that depends on the instant.
mkseries "$T/osc" 100 0 30 5
ok "oscillation around a flat mean is NOT a plateau" "$(plateau_check "$T/osc" 1 2)" "0"

# Falling memory is allowed through: the drift gate is one-sided. A gateway settling DOWN onto its
# plateau would otherwise be pinned at the cap forever, and releasing memory is not the failure this
# gate is for. The range gate still bounds how far it may move, so this only passes when the fall is
# small.
mkseries "$T/fall" 100 -0.02 30
ok "a slight downward drift within the range gate is a plateau" "$(plateau_check "$T/fall" 1 2)" "1"

ok "too few samples is NOT a plateau (undecidable is not steady)" "$(plateau_check "$T/short" 1 2)" "0"
mkseries "$T/three" 100 0 3
ok "three samples is still too few to judge" "$(plateau_check "$T/three" 1 2)" "0"
ok "a missing file is not a plateau" "$(plateau_check "$T/nope" 1 2)" "0"
ok "an empty argument is not a plateau" "$(plateau_check "" 1 2)" "0"

echo "plateau_growth_rate"
# 0.5 MiB per 2s sample = 15 MiB/min.
mkseries "$T/grow" 100 0.5 30
ok "growth rate is MiB per minute from the fitted slope" "$(plateau_growth_rate "$T/grow")" "15.000"
mkseries "$T/flat2" 100 0 30
ok "a flat series has a zero growth rate" "$(plateau_growth_rate "$T/flat2")" "0.000"
ok "an unmeasurable rate is EMPTY, not 0 (null must differ from zero)" "$(plateau_growth_rate "$T/nope")" ""
mkseries "$T/one" 100 0 1
ok "a single sample yields no rate" "$(plateau_growth_rate "$T/one")" ""
# A single wild endpoint must not set the reported leak rate - that is why this is a fit, not
# last-minus-first. Flat series with one 50 MiB spike at the end: endpoint arithmetic would report a
# huge rate, the fit reports a modest one.
mkseries "$T/spike" 100 0 29; echo "58 150" >>"$T/spike"
ok "one wild endpoint does not dominate the fitted rate" \
  "$(awk -v r="$(plateau_growth_rate "$T/spike")" 'BEGIN{print (r+0 < 60) ? "bounded" : "dominated"}')" "bounded"

echo "plateau_window"
mkseries "$T/long" 100 0 60      # 60 samples at 2s = 118s of history
ok "the window keeps only the trailing seconds" "$(plateau_window "$T/long" 60 | wc -l | tr -d ' ')" "31"
ok "a window longer than the series keeps everything" "$(plateau_window "$T/long" 9999 | wc -l | tr -d ' ')" "60"
ok "a missing series yields an empty window" "$(plateau_window "$T/nope" 60 | wc -l | tr -d ' ')" "0"

echo
echo "plateau: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
