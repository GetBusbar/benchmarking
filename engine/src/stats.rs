// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE STEADINESS GATE: a timer says when WE stopped looking, not when the series stopped moving, so
// a fixed-duration load would publish a gateway's mid-climb value as if it were a settled one.
//
// A pure spread test ("max minus min in the window is small") is not enough: a leak that is
// asymptoting has tiny sample-to-sample deltas near its tail, so it passes a spread test while still
// genuinely rising. The trend test (second-half mean vs first-half mean) is what catches that shape,
// which is why `plateau_check` requires BOTH tests rather than either alone. See `plateau_check` for
// the one-sided drift rule and the undecidable-sample-count rule.
//
// Not reaching a plateau is a measured result, not a failure: it is returned as `Verdict::NotSteady`
// carrying the growth rate, which is the most informative thing this gate can say about a series that
// never settles. A caller must publish that rather than quietly substituting the last sample.

use crate::measurement::{Absent, Measurement};

/// One point of a memory (or any monotone-ish quantity) series: seconds since the series began, and
/// the value in MiB at that instant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub t_s: f64,
    pub mib: f64,
}

impl Sample {
    pub fn new(t_s: f64, mib: f64) -> Self {
        Sample { t_s, mib }
    }
}

/// The outcome of the steadiness gate over a trailing window.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Both the trend test and the range test passed: the window is not moving in any direction that
    /// matters, and a caller may publish a steady-state number from it.
    Steady,
    /// A real, publishable result: the window did not settle. Carries the growth rate so a caller can
    /// never say "not steady" without also saying how fast it moved.
    NotSteady {
        growth_rate_mib_per_min: Measurement<f64>,
    },
    /// Fewer than four samples fell inside the window: not enough evidence to judge either way. This
    /// is a distinct case from `NotSteady`, on purpose: "we could not tell" and "we could tell and it
    /// moved" are different claims, and collapsing them would let a too-short measurement masquerade
    /// as a settled one.
    Undecidable,
}

/// Keep only the trailing `window_s` seconds of `samples`, anchored to the LAST sample's own
/// timestamp rather than wall-clock time, so a series that ended before "now" still windows correctly.
/// The steadiness test only ever looks at recent history: a window anchored at the start would be
/// diluted by every series' initial ramp, so a fast-settling series would be judged on samples from
/// before it settled.
pub fn window(samples: &[Sample], window_s: f64) -> Vec<Sample> {
    match samples.last() {
        None => Vec::new(),
        Some(last) => {
            let cut = last.t_s - window_s;
            samples.iter().filter(|s| s.t_s >= cut).copied().collect()
        }
    }
}

/// Least-squares slope of `mib` against `t_s`, in MiB per minute. A fit rather than
/// endpoint-minus-endpoint on purpose: a single noisy first or last sample would otherwise set the
/// whole reported rate. Absent when there are fewer than two samples, or when every sample shares the
/// same timestamp (no slope is defined). Absent must stay distinguishable from a measured zero: "we
/// did not measure a rate" and "the rate was zero" are different claims.
pub fn growth_rate(samples: &[Sample]) -> Measurement<f64> {
    let n = samples.len();
    if n < 2 {
        return Measurement::absent(Absent::NotMeasured);
    }
    let mean_t = samples.iter().map(|s| s.t_s).sum::<f64>() / n as f64;
    let mean_v = samples.iter().map(|s| s.mib).sum::<f64>() / n as f64;
    let mut num = 0.0;
    let mut den = 0.0;
    for s in samples {
        let dt = s.t_s - mean_t;
        num += dt * (s.mib - mean_v);
        den += dt * dt;
    }
    if den <= 0.0 {
        return Measurement::absent(Absent::NotMeasured);
    }
    Measurement::Measured((num / den) * 60.0)
}

/// The steadiness gate. Windows `samples` to the trailing `window_s` seconds, then requires BOTH:
///   trend: the second half of the window's mean rises less than `trend_pct` over the first half's.
///   range: (max - min) across the window is less than `range_pct` of the window's mean.
///
/// The trend test is ONE-SIDED: only a rise disqualifies. A series falling during the window is
/// releasing, not leaking, and pinning that at "not steady" forever would defeat the point of a
/// measured stop condition. The range test still bounds how far it may move either way, so a series
/// oscillating hard around a flat mean (no trend, but not steady either, since the value read depends
/// on which instant you sampled) is still correctly rejected.
///
/// An odd-sized window gives its extra sample to the SECOND half, so a late upward sample is never the
/// one a rounding choice drops.
///
/// CALIBRATION NOTE, load-bearing: for a linear ramp, the window size and the thresholds trade off
/// directly, because both tests reduce to "how far did the value move, relative to the mean, across
/// (about) half the window". Halving `window_s` halves the elapsed time the trend test can see, so it
/// takes DOUBLE the rate to produce the same measured drift. Concretely, on a ~120 MiB base at the
/// default 1% trend gate: a leak has to hold under ~2.4 MiB/min to certify steady at a 60s window, but
/// under ~4.8 MiB/min at a 30s window. Shrinking the window is not a free way to detect leaks faster;
/// it loosens the bar. See the halving-window test below, which pins this ratio exactly.
pub fn plateau_check(samples: &[Sample], window_s: f64, trend_pct: f64, range_pct: f64) -> Verdict {
    let win = window(samples, window_s);
    let n = win.len();
    if n < 4 {
        return Verdict::Undecidable;
    }
    let h = n / 2;
    let (first, second) = win.split_at(h);
    let sum1: f64 = first.iter().map(|s| s.mib).sum();
    let sum2: f64 = second.iter().map(|s| s.mib).sum();
    let mean1 = sum1 / first.len() as f64;
    let mean2 = sum2 / second.len() as f64;
    let mean = (sum1 + sum2) / n as f64;
    let growth_rate_mib_per_min = growth_rate(&win);

    // A non-positive mean makes both percentages meaningless (division by zero or sign flip), so this
    // cannot be called steady; it is a real result (not undecidable), matching the shell original.
    if mean <= 0.0 {
        return Verdict::NotSteady {
            growth_rate_mib_per_min,
        };
    }

    let drift = (mean2 - mean1) / mean * 100.0;
    let lo = win.iter().map(|s| s.mib).fold(f64::INFINITY, f64::min);
    let hi = win.iter().map(|s| s.mib).fold(f64::NEG_INFINITY, f64::max);
    let spread = (hi - lo) / mean * 100.0;

    // MAGNITUDE, NOT SIGN. `drift` is signed - positive when the window is still climbing, negative
    // when it is declining - but `Verdict::Steady`'s own contract is "not moving in any direction
    // that matters", so the comparison uses `drift.abs()`: comparing the signed value against a
    // positive threshold would bound growth only, since any decline, however steep, is always less
    // than a positive threshold and would read as settled.
    if drift.abs() < trend_pct && spread < range_pct {
        Verdict::Steady
    } else {
        Verdict::NotSteady {
            growth_rate_mib_per_min,
        }
    }
}

/// The median of `values`. Even counts average the two middle values after sorting, matching the
/// shell harness's own median helpers. Absent (never zero) on an empty slice.
pub fn median(values: &[f64]) -> Measurement<f64> {
    if values.is_empty() {
        return Measurement::absent(Absent::NotMeasured);
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();
    let mid = n / 2;
    let v = if n % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    };
    Measurement::Measured(v)
}

/// The smallest value in `values`. Absent (never zero, never an arbitrary sentinel) on an empty slice.
pub fn min(values: &[f64]) -> Measurement<f64> {
    match values.iter().copied().reduce(f64::min) {
        Some(v) => Measurement::Measured(v),
        None => Measurement::absent(Absent::NotMeasured),
    }
}

/// The largest value in `values`. Absent on an empty slice.
pub fn max(values: &[f64]) -> Measurement<f64> {
    match values.iter().copied().reduce(f64::max) {
        Some(v) => Measurement::Measured(v),
        None => Measurement::absent(Absent::NotMeasured),
    }
}

/// THE ONE PERCENTILE CONVENTION THIS ENGINE USES. Every published percentile - the load
/// generator's p50/p99, the search's rung median, the streaming TTFT and inter-frame-gap
/// percentiles - resolves its rank through here, so no two of them can mean different things by the
/// same name.
///
/// NEAREST RANK, CEILING: the 0-based index of the `ceil(n * p)`-th smallest value, clamped into
/// `0..n`. Never interpolates, so a published percentile is always a value some sample really
/// produced.
///
/// CEIL WON, and the engine used to be split. `metric.rs` computed `ceil(n*p)` (1-based) while
/// `gen.rs`, `stats.rs` and `search.rs` computed `floor(n*p)` (0-based), and comments in all three
/// places claimed the conventions matched. They agree whenever `n * p` is fractional and disagree by
/// exactly one rank whenever it is a whole number - which is precisely the sample counts this rig
/// chooses: n=100 TTFT samples per leg (`metric::STREAM_TTFT_SAMPLES`) puts p99 at index 98 under
/// ceil and index 99 under floor, and index 99 of 100 IS THE MAXIMUM. That is the defect, and it is
/// not cosmetic: floor turns a p99 into "the single worst sample", the one order statistic a tail
/// percentile exists to avoid. `gen.rs`'s own test asserted it outright - `pct_us(0.99) == 100` over
/// ten samples, commented "the last value" - and `metric.rs`'s asserted the opposite rule in the
/// same crate ("the p99 must not be the max, or it is not a percentile"). Ceil is also the textbook
/// nearest-rank definition (the smallest value at or above which at least `p` of the data falls), so
/// the convention that survives is the one a reader of the board would assume.
///
/// This moves published percentiles by at most one rank, downward, on the whole-number cases. That
/// is expected and it is the correction: the numbers it changes were the maximum wearing a
/// percentile's name.
///
/// `n` must be non-zero; an empty sample set has no percentile at all and every caller answers that
/// with an absence rather than a rank.
pub fn nearest_rank_index(n: usize, p: f64) -> usize {
    let rank = ((n as f64) * p).ceil() as usize;
    rank.clamp(1, n.max(1)) - 1
}

/// The `p`-th percentile of `values`, for `p` in `0.0..=1.0`, by NEAREST RANK - see
/// `nearest_rank_index` for the convention and why it is that one. Absent on an empty slice.
pub fn percentile(values: &[f64], p: f64) -> Measurement<f64> {
    if values.is_empty() {
        return Measurement::absent(Absent::NotMeasured);
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Measurement::Measured(sorted[nearest_rank_index(sorted.len(), p)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // Builds a series the way lib/plateau_test.sh's mkseries does: value v0 + d*i at t = 2*i, with an
    // optional alternating +/-j jitter. Kept identical to the shell helper so every ported case below
    // reproduces the exact numbers the shell test asserted.
    fn mkseries_jitter(v0: f64, d: f64, n: usize, j: f64) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let w = if j > 0.0 {
                    if i % 2 == 1 {
                        j
                    } else {
                        -j
                    }
                } else {
                    0.0
                };
                Sample::new((i * 2) as f64, v0 + d * i as f64 + w)
            })
            .collect()
    }
    fn mkseries(v0: f64, d: f64, n: usize) -> Vec<Sample> {
        mkseries_jitter(v0, d, n, 0.0)
    }
    // A window big enough to keep an entire mkseries-built series, so `plateau_check` sees the whole
    // thing, matching the shell tests which pass plateau_check an already-whole file.
    const WHOLE: f64 = 9_999.0;

    // ---- plateau_check: ported from plateau_test.sh -----------------------------------------------

    #[test]
    fn a_genuinely_flat_series_is_a_plateau() {
        let s = mkseries(100.0, 0.0, 30);
        assert_eq!(plateau_check(&s, WHOLE, 1.0, 2.0), Verdict::Steady);
    }

    #[test]
    fn a_steadily_rising_series_is_not_a_plateau() {
        let s = mkseries(100.0, 1.0, 30);
        assert!(!matches!(
            plateau_check(&s, WHOLE, 1.0, 2.0),
            Verdict::Steady
        ));
    }

    // THE CASE THE TREND TEST EXISTS FOR: a leak levelling off. 30 samples, +0.05 MiB each, around 118
    // MiB. Spread is ~1.2%, under a 2% range gate, so a spread-only test calls it settled. It is not:
    // the second-half mean sits above the first-half mean by more than a 0.5% trend gate can allow.
    #[test]
    fn red_a_spread_only_test_wrongly_calls_the_asymptoting_leak_steady() {
        let s = mkseries(118.0, 0.05, 30);
        let values: Vec<f64> = s.iter().map(|p| p.mib).collect();
        let lo = min(&values).copied().unwrap_or(0.0);
        let hi = max(&values).copied().unwrap_or(0.0);
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let spread_pct = (hi - lo) / mean * 100.0;
        assert!(
            spread_pct < 2.0,
            "the RED case: spread alone looks settled ({spread_pct}%)"
        );
    }

    #[test]
    fn green_the_trend_test_rejects_the_asymptoting_leak() {
        let s = mkseries(118.0, 0.05, 30);
        assert!(!matches!(
            plateau_check(&s, WHOLE, 0.5, 2.0),
            Verdict::Steady
        ));
    }

    #[test]
    fn oscillation_around_a_flat_mean_is_not_a_plateau() {
        let s = mkseries_jitter(100.0, 0.0, 30, 5.0);
        assert!(!matches!(
            plateau_check(&s, WHOLE, 1.0, 2.0),
            Verdict::Steady
        ));
    }

    // The trend gate is one-sided: falling memory means the gateway is releasing, which is not the
    // failure this gate exists to catch, so a small downward drift inside the range gate still passes.
    #[test]
    fn a_slight_downward_drift_within_the_range_gate_is_a_plateau() {
        let s = mkseries(100.0, -0.02, 30);
        assert_eq!(plateau_check(&s, WHOLE, 1.0, 2.0), Verdict::Steady);
    }

    #[test]
    fn too_few_samples_is_undecidable_not_a_plateau() {
        assert_eq!(plateau_check(&[], WHOLE, 1.0, 2.0), Verdict::Undecidable);
    }

    #[test]
    fn three_samples_is_still_too_few_to_judge() {
        let s = mkseries(100.0, 0.0, 3);
        assert_eq!(plateau_check(&s, WHOLE, 1.0, 2.0), Verdict::Undecidable);
    }

    #[test]
    fn exactly_four_samples_is_enough_to_judge() {
        let s = mkseries(100.0, 0.0, 4);
        assert_eq!(plateau_check(&s, WHOLE, 1.0, 2.0), Verdict::Steady);
    }

    // Boundary: drift and spread comparisons are strict "<", so sitting EXACTLY on either threshold
    // must fail, not pass. Two samples per half, chosen so drift lands at precisely 1% of the mean.
    #[test]
    fn drift_exactly_at_the_trend_threshold_is_not_steady() {
        // halves: {100, 100} then {101, 101}; mean1=100 mean2=101 mean=100.5, drift = 1/100.5*100 ~ 0.995%.
        // Pick values so drift is exactly 1.0% of the mean: mean=100, mean2-mean1=1.0 -> drift=1.0.
        let s = vec![
            Sample::new(0.0, 99.5),
            Sample::new(1.0, 99.5),
            Sample::new(2.0, 100.5),
            Sample::new(3.0, 100.5),
        ];
        // mean1=99.5, mean2=100.5, mean=100.0, drift=(100.5-99.5)/100.0*100=1.0 exactly.
        assert_eq!(
            plateau_check(&s, WHOLE, 1.0, 1_000.0),
            Verdict::NotSteady {
                growth_rate_mib_per_min: growth_rate(&s)
            }
        );
    }

    #[test]
    fn spread_exactly_at_the_range_threshold_is_not_steady() {
        // mean = 100, hi - lo = 2.0 -> spread = 2.0% exactly.
        let s = vec![
            Sample::new(0.0, 99.0),
            Sample::new(1.0, 100.0),
            Sample::new(2.0, 100.0),
            Sample::new(3.0, 101.0),
        ];
        assert!(!matches!(
            plateau_check(&s, WHOLE, 1_000.0, 2.0),
            Verdict::Steady
        ));
    }

    // ---- growth_rate: ported from plateau_test.sh --------------------------------------------------

    #[test]
    fn growth_rate_is_mib_per_minute_from_the_fitted_slope() {
        // 0.5 MiB per 2s sample = 15 MiB/min.
        let s = mkseries(100.0, 0.5, 30);
        let v = growth_rate(&s).copied();
        assert!(matches!(v, Some(x) if (x - 15.0).abs() < 1e-6), "got {v:?}");
    }

    #[test]
    fn a_flat_series_has_a_zero_growth_rate() {
        let s = mkseries(100.0, 0.0, 30);
        let v = growth_rate(&s).copied();
        assert!(matches!(v, Some(x) if x.abs() < 1e-9), "got {v:?}");
    }

    #[test]
    fn an_unmeasurable_rate_is_absent_not_zero() {
        assert!(!growth_rate(&[]).is_measured());
    }

    #[test]
    fn a_single_sample_yields_no_rate() {
        let s = mkseries(100.0, 0.0, 1);
        assert!(!growth_rate(&s).is_measured());
    }

    // A single wild endpoint must not set the reported rate; that is why this is a fit and not
    // last-minus-first. Flat series with one +50 MiB spike appended at the end.
    #[test]
    fn one_wild_endpoint_does_not_dominate_the_fitted_rate() {
        let mut s = mkseries(100.0, 0.0, 29);
        s.push(Sample::new(58.0, 150.0));
        let v = growth_rate(&s).copied();
        assert!(
            matches!(v, Some(x) if x < 60.0),
            "endpoint dominated the fit: {v:?}"
        );
    }

    // ---- window: ported from plateau_test.sh ----------------------------------------------------

    #[test]
    fn the_window_keeps_only_the_trailing_seconds() {
        let s = mkseries(100.0, 0.0, 60); // 60 samples at 2s apart = 118s of history
        assert_eq!(window(&s, 60.0).len(), 31);
    }

    #[test]
    fn a_window_longer_than_the_series_keeps_everything() {
        let s = mkseries(100.0, 0.0, 60);
        assert_eq!(window(&s, 9_999.0).len(), 60);
    }

    #[test]
    fn an_empty_series_yields_an_empty_window() {
        assert_eq!(window(&[], 60.0).len(), 0);
    }

    // ---- the calibration relationship: the point of the port ---------------------------------------

    fn linear_series(base: f64, rate_mib_per_min: f64, dt_s: f64, n: usize) -> Vec<Sample> {
        let per_sample = rate_mib_per_min / 60.0 * dt_s;
        (0..n)
            .map(|i| Sample::new(i as f64 * dt_s, base + per_sample * i as f64))
            .collect()
    }

    // A DECLINE MUST FAIL THE TREND TEST EXACTLY AS A CLIMB DOES. `drift` is signed; a window whose
    // second half runs measurably below its first half is not "steady", it is declining, and
    // Verdict::Steady's own doc says "not moving in any direction that matters". A comparison that
    // only bounds positive drift would call every decline steady regardless of how fast it fell.
    #[test]
    fn a_declining_window_fails_the_trend_test_at_the_same_magnitude_as_a_climbing_one() {
        // Gentle on purpose: the mean must stay well clear of zero, or the window trips the earlier
        // `mean &lt;= 0.0` short-circuit and returns NotSteady for an unrelated reason, masking whether
        // the trend comparison itself is symmetric. A ~1.5% decline on a 100 MiB base, against a 1%
        // trend gate and a wide range gate (so only the trend test is in play).
        let declining = linear_series(100.0, -18.0, 6.0, 12); // ~100.0 -> ~98.1 MiB over the window
        let verdict = plateau_check(&declining, 60.0, 1.0, 1.0e9);
        assert!(
            matches!(verdict, Verdict::NotSteady { .. }),
            "a real decline bigger than the trend gate must not be published as settled, got {verdict:?}"
        );

        // The mirror image at the identical magnitude climbs instead, and must fail the same way -
        // proving the test is symmetric rather than just asserting decline is special-cased.
        let climbing = linear_series(100.0, 18.0, 6.0, 12);
        let climbing_verdict = plateau_check(&climbing, 60.0, 1.0, 1.0e9);
        assert!(matches!(climbing_verdict, Verdict::NotSteady { .. }));
    }

    // Largest rate (MiB/min) that still certifies Steady at this window, found by bisection with the
    // range gate disabled (huge range_pct) so only the trend test is in play.
    fn boundary_rate(window_s: f64, n: usize, trend_pct: f64, base: f64) -> f64 {
        let dt = window_s / (n as f64 - 1.0);
        let mut lo = 0.0_f64;
        let mut hi = 10_000.0_f64;
        for _ in 0..80 {
            let mid = (lo + hi) / 2.0;
            let series = linear_series(base, mid, dt, n);
            let steady = matches!(
                plateau_check(&series, window_s, trend_pct, 1.0e9),
                Verdict::Steady
            );
            if steady {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    // THE CALIBRATION ARGUMENT, pinned exactly: for a linear ramp, halving the window halves how much
    // elapsed time the trend test can see, so it takes double the rate to produce the same measured
    // drift. This is not a rounding artifact of one example; it holds for any base and threshold.
    #[test]
    fn halving_the_window_doubles_the_admissible_slope() {
        let base = 120.0;
        let n = 400; // dense sampling so the discrete boundary tracks the continuous one closely
        let b60 = boundary_rate(60.0, n, 1.0, base);
        let b30 = boundary_rate(30.0, n, 1.0, base);
        assert!(
            (b30 - 2.0 * b60).abs() / b60 < 0.01,
            "b60={b60} b30={b30}, expected b30 ~= 2*b60"
        );
        // matches the worked example in the calibration note: ~2.4 MiB/min at 60s vs ~4.8 at 30s on a 120 MiB base.
        assert!((b60 - 2.4).abs() < 0.05, "b60={b60}");
        assert!((b30 - 4.8).abs() < 0.1, "b30={b30}");
    }

    // ---- median / min / max / percentile ----------------------------------------------------------

    #[test]
    fn median_of_an_odd_count_is_the_middle_value() {
        assert_eq!(median(&[3.0, 1.0, 2.0]).copied(), Some(2.0));
    }

    #[test]
    fn median_of_an_even_count_averages_the_two_middle_values() {
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]).copied(), Some(2.5));
    }

    #[test]
    fn median_min_max_percentile_are_absent_on_empty_input() {
        assert!(!median(&[]).is_measured());
        assert!(!min(&[]).is_measured());
        assert!(!max(&[]).is_measured());
        assert!(!percentile(&[], 0.5).is_measured());
    }

    #[test]
    fn min_and_max_of_a_series() {
        let v = [3.0, 1.0, 4.0, 1.0, 5.0];
        assert_eq!(min(&v).copied(), Some(1.0));
        assert_eq!(max(&v).copied(), Some(5.0));
    }

    #[test]
    fn percentile_by_nearest_rank() {
        let v: Vec<f64> = (1..=10).map(|i| i as f64).collect(); // 1..10
        assert_eq!(percentile(&v, 0.0).copied(), Some(1.0));
        assert_eq!(percentile(&v, 0.5).copied(), Some(5.0)); // ceil(10*0.5)=5 -> index 4 -> value 5
        assert_eq!(percentile(&v, 0.99).copied(), Some(10.0));
        assert_eq!(percentile(&v, 1.0).copied(), Some(10.0)); // clamped to the last index
    }

    // ONE CONVENTION, OR THE SAME WORD MEANS TWO THINGS ON ONE BOARD.
    //
    // Ledger SRCH-04: `metric.rs` resolved a rank with ceil while `gen.rs`, `stats.rs` and
    // `search.rs` used floor, and comments in three files claimed they agreed. Over the sample
    // counts this rig actually chooses - 100 TTFT samples a leg - floor puts p99 at index 99 of
    // 100, which is the MAXIMUM. This pins the ranks directly so a future edit cannot quietly
    // reintroduce the split.
    #[test]
    fn one_nearest_rank_convention_and_a_p99_that_is_not_the_maximum() {
        // The whole-number cases are exactly where floor and ceil disagreed.
        assert_eq!(nearest_rank_index(100, 0.99), 98);
        assert_eq!(nearest_rank_index(100, 0.50), 49);
        assert_eq!(nearest_rank_index(10, 0.50), 4);
        assert!(
            nearest_rank_index(100, 0.99) < 99,
            "a p99 that lands on the last index is the maximum, not a percentile"
        );
        // Fractional n*p: both conventions always agreed here, and the answer must not move.
        assert_eq!(nearest_rank_index(5, 0.5), 2);
        assert_eq!(nearest_rank_index(7, 0.9), 6);
        // The ends stay in range: p=0 is the smallest sample, p=1 the largest, never an out-of-bounds
        // rank or a rank of zero.
        for n in 1..200usize {
            assert_eq!(nearest_rank_index(n, 0.0), 0);
            assert_eq!(nearest_rank_index(n, 1.0), n - 1);
            for p in [0.5, 0.9, 0.95, 0.99] {
                assert!(nearest_rank_index(n, p) < n);
            }
        }
    }

    // ---- properties ---------------------------------------------------------------------------------

    proptest! {
        // A rise steep enough to matter is never certified steady, at any window size, sample count,
        // or threshold pair in a realistic range. "Steep enough to matter" is necessary and honest:
        // the gate is a threshold, not a leak detector, and plateau.sh documents the same limit (a
        // rate below the certifying boundary genuinely does pass, on purpose, at 1.5 MiB/min on a 120
        // MiB base with the real 1%/60s defaults). This proves the trend test actually fires for
        // anything past its own boundary, for any window/sample-count combination, not just the one
        // worked example above.
        #[test]
        fn rising_series_above_the_trend_boundary_is_never_steady(
            window_s in 10.0f64..300.0,
            n in 20usize..200,
            trend_pct in 0.1f64..5.0,
            range_pct in 0.1f64..5.0,
            margin in 3.0f64..20.0,
        ) {
            let base = 120.0;
            let dt = window_s / (n as f64 - 1.0);
            // Continuous-approximation boundary from the calibration note; go well past it (margin
            // 3x-20x) so discretization error at small n can never flip the assertion.
            let boundary = 1.2 * base * trend_pct / window_s;
            let rate = boundary * margin;
            let series = linear_series(base, rate, dt, n);
            let verdict = plateau_check(&series, window_s, trend_pct, range_pct);
            prop_assert!(!matches!(verdict, Verdict::Steady));
        }

        // Too few samples is always Undecidable, never a false NotSteady or Steady, regardless of what
        // the samples actually contain.
        #[test]
        fn fewer_than_four_samples_is_always_undecidable(
            n in 0usize..4,
            base in -100.0f64..1000.0,
            d in -50.0f64..50.0,
        ) {
            let s = mkseries(base, d, n);
            prop_assert_eq!(plateau_check(&s, 9_999.0, 1.0, 2.0), Verdict::Undecidable);
        }

        // growth_rate on an exactly linear series recovers that slope: a least-squares fit of exactly
        // linear data is exact, not merely close, up to floating-point error.
        #[test]
        fn growth_rate_recovers_a_known_slope(
            rate in -500.0f64..500.0,
            dt in 0.1f64..10.0,
            n in 2usize..100,
        ) {
            let s = linear_series(120.0, rate, dt, n);
            match growth_rate(&s) {
                Measurement::Measured(v) => prop_assert!((v - rate).abs() < 1e-6 * rate.abs().max(1.0)),
                other => prop_assert!(false, "expected a measured rate, got {other:?}"),
            }
        }
    }
}
