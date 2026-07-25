#!/usr/bin/env bash
# ── THE STEADINESS TEST ────────────────────────────────────────────────────────────────────────────
# Decides when a memory load has run long enough: not "120 seconds have passed" but "the RSS has
# stopped moving".
#
# WHY THIS REPLACED A FIXED DURATION. The old memory window ran a fixed 120s load and published the
# peak RSS observed. For any gateway still climbing when the timer expired, that number described when
# we stopped looking rather than the gateway: a longer load would have produced a larger number. In the
# 2026-07-25 field run two of thirteen gateways were still rising at the moment the load stopped
# (busbar climbed for 111s of the 120s window; bifrost for 126s of it), so their published peaks were
# duration artifacts. Terminating on a measured condition instead makes the number mean something you
# can name: the steady-state RSS under this load.
#
# WHY THE TREND TEST AND NOT JUST A RANGE. The obvious implementation - "max minus min inside the
# window is under X%" - passes on exactly the case that matters. A slow leak that is asymptoting has a
# small per-sample delta near the tail, so a pure spread test declares a plateau while memory is still
# genuinely climbing. Busbar's curve is the worked example: by t=100s its samples are close together
# and a 1% spread test would have called it settled at a value it had not settled at. Comparing the
# second half of the window against the first half measures DIRECTION, which is the thing a leak has
# and a steady state does not.
#
# WHY BOTH TESTS AND NOT ONLY THE TREND. A gateway oscillating hard around a flat mean has no trend but
# is not steady either, and sampling it produces a number that depends on which instant you looked. The
# range test rejects that. So: no upward drift AND not swinging about.
#
# NOT PLATEAUING IS A RESULT, NOT A FAILURE. A gateway that never satisfies this test inside the cap is
# reported as `plateaued: false` with its growth rate, which is the most informative thing the memory
# metric can say about it. The caller must publish that rather than substituting the last sample and
# calling it steady.
#
# THE GATE IS NOT THE ONLY DEFENCE, AND MUST NOT BE TREATED AS ONE. Any threshold admits a leak slower
# than itself: at a 1% trend gate over a 60s window, a gateway creeping ~1.5 MiB/min on a 120 MiB base
# passes as steady, and over an hour that is ~90 MiB. Tightening the gate is not the fix - it would
# just pin noisy-but-genuinely-steady gateways at the cap and cost hours per field run. The fix is that
# the CALLER PUBLISHES growth_rate_mib_per_min UNCONDITIONALLY, including for gateways that plateaued.
# A row reading "plateaued: true, growth 1.5 MiB/min" is self-evidently a slow leak that happened to
# clear the bar, and a reader can see it. Emitting the rate only on the not-plateaued path would hide
# exactly the cases this threshold is too loose to catch.

# plateau_check <window_file> <trend_pct> <range_pct>
#   window_file: lines of "<t_s> <rss_mib>", ALREADY trimmed to the trailing window by the caller.
#   Prints "1" if the series is steady by both tests, "0" otherwise. Prints "0" when there are too few
#   samples to judge (fewer than 4, i.e. under two per half) - an undecidable window is NOT a plateau,
#   because treating "not enough evidence" as "steady" is how a short measurement gets published as a
#   settled one.
plateau_check(){
  local f="${1:-}" trend="${2:-1}" range="${3:-2}"
  [ -n "$f" ] && [ -s "$f" ] || { printf '0'; return; }
  awk -v trend="$trend" -v range="$range" '
    { n++; v[n]=$2+0 }
    END{
      if (n < 4) { print 0; exit }
      # Split into halves. An odd count gives the extra sample to the SECOND half, so a late upward
      # sample is never the one dropped - the test must not be able to miss a rise by rounding.
      h = int(n/2)
      s1=0; for (i=1;   i<=h; i++) s1+=v[i]
      s2=0; for (i=h+1; i<=n; i++) s2+=v[i]
      m1 = s1/h; m2 = s2/(n-h)
      mean = (s1+s2)/n
      if (mean <= 0) { print 0; exit }
      # Drift is ONE-SIDED: only an upward trend disqualifies. Memory falling during the window means
      # the gateway is releasing, which is not a steady state either, but it is also not the failure
      # mode this gate exists to catch, and a gateway settling DOWN toward its plateau would otherwise
      # be held at the cap forever. The range test still bounds how far it may be moving.
      drift = (m2 - m1) / mean * 100
      lo=v[1]; hi=v[1]
      for (i=2;i<=n;i++){ if(v[i]<lo) lo=v[i]; if(v[i]>hi) hi=v[i] }
      spread = (hi - lo) / mean * 100
      print (drift < trend && spread < range) ? 1 : 0
    }' "$f"
}

# plateau_growth_rate <window_file>
#   MiB per minute across the window, from a least-squares fit rather than endpoint-minus-endpoint: a
#   single noisy first or last sample would otherwise set the whole reported leak rate. Prints "" when
#   there is nothing to fit, which the caller renders as the literal null - "we did not measure a rate"
#   must stay distinguishable from "the rate was zero".
plateau_growth_rate(){
  local f="${1:-}"
  [ -n "$f" ] && [ -s "$f" ] || return 0
  awk '
    { n++; x[n]=$1+0; y[n]=$2+0; sx+=x[n]; sy+=y[n] }
    END{
      if (n < 2) exit
      mx=sx/n; my=sy/n
      for (i=1;i<=n;i++){ dx=x[i]-mx; num+=dx*(y[i]-my); den+=dx*dx }
      if (den <= 0) exit          # every sample at the same instant: no slope is defined
      printf "%.3f", (num/den)*60 # MiB/s -> MiB/min
    }' "$f"
}

# plateau_window <series_file> <window_s>
#   Emits the trailing <window_s> seconds of a "<t_s> <rss_mib>" series. The steadiness test only ever
#   looks at recent history: a window anchored at the start would be diluted by the ramp every gateway
#   has on the way up, so a fast-settling gateway would be judged on samples from before it settled.
plateau_window(){
  local f="${1:-}" w="${2:-60}"
  [ -n "$f" ] && [ -s "$f" ] || return 0
  awk -v w="$w" '{ t[NR]=$1+0; l[NR]=$0 } END{ if(!NR) exit; cut=t[NR]-w; for(i=1;i<=NR;i++) if(t[i]>=cut) print l[i] }' "$f"
}
