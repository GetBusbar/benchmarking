// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The steadiness gate decides whether a series has settled, not just whether a timer stopped
// watching it. A spread test alone misses an asymptoting leak (tiny deltas near the tail still pass
// a spread check while genuinely rising), so `plateau_check` requires both a trend test and a range
// test. A series that never settles publishes `Verdict::NotSteady` with its growth rate rather than
// having the caller substitute the last sample.

use crate::measurement::{Absent, Measurement};
use serde::{Deserialize, Serialize};

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

/// How a window failed to settle. An oscillating gateway (e.g. GC cycling) is not leaking however far
/// it swings; a climbing one has no level at all. Both fail the steadiness test, so they must not
/// share one "not steady" label — that would brand a healthy sawtooth as a leak. `drift` (net
/// movement between halves) and `spread` (range) separate the two: a climb has large drift, a wave
/// has large spread and near-zero drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// Trending in one direction across the window and still going at the end. Unbounded growth is
    /// the defect this metric exists to catch.
    Climbing,
    /// Ranging widely but with no net trend: it returns to where it was. Not a leak.
    Oscillating,
    /// Trending downward across the window — still releasing memory when it closed. Not steady, but
    /// the opposite of a leak, and must never be labelled as one.
    Falling,
}

/// The outcome of the steadiness gate over a trailing window.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Both the trend test and the range test passed: the window is not moving in any direction that
    /// matters, and a caller may publish a steady-state number from it.
    ///
    /// Carries the fitted growth rate rather than letting a caller substitute an assumed zero: a
    /// window drifting just under the trend bar is still measurably moving, and that rate is the
    /// number a reader needs to spot a slow leak.
    Steady {
        growth_rate_mib_per_min: Measurement<f64>,
    },
    /// The window did not settle. Carries the growth rate and the `Shape` of the movement, because
    /// "did not settle" covers two very different gateways and only one of them is a defect.
    NotSteady {
        growth_rate_mib_per_min: Measurement<f64>,
        shape: Shape,
    },
    /// The window could not be judged, with why: there are two distinct causes (a coverage gap vs. a
    /// harness fault) and a caller must not conflate them — see `Undecidable`.
    Undecidable(Undecidable),
}

/// Why a window could not be judged. One is a coverage gap in the measurement, the other is the
/// harness malfunctioning; never attribute the rig's fault to the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undecidable {
    /// Fewer than four samples fell inside the window: not enough evidence to judge either way.
    /// Distinct from `NotSteady` — "we could not tell" and "we could tell and it moved" differ.
    TooFewReadings { got: usize, need: usize },
    /// A sample was NaN or an infinity, so no order statistic over the window is answerable. A rig
    /// fault; must reach the artifact as `Absent::HarnessError`.
    NonFiniteSample,
    /// Enough readings, but not enough elapsed time to call a plateau. Distinct from
    /// `TooFewReadings` since a dense short window and a sparse long one fail for different reasons.
    WindowTooShort,
}

impl Undecidable {
    /// The absence variant a caller must publish for this cause. Centralized so the two causes can't
    /// be swapped by a caller.
    pub fn absent_kind(&self) -> Absent {
        match self {
            Undecidable::TooFewReadings { .. } | Undecidable::WindowTooShort => Absent::NotMeasured,
            Undecidable::NonFiniteSample => Absent::HarnessError,
        }
    }

    /// The reason string, so callers cannot describe the same cause differently.
    pub fn detail(&self, window_label: &str) -> String {
        match self {
            Undecidable::TooFewReadings { got, need } => format!(
                "only {got} of the {need} readings needed fell inside the {window_label}, so whether \
                 memory moved cannot be judged either way"
            ),
            Undecidable::NonFiniteSample => format!(
                "a sample in the {window_label} was not finite, so no order statistic over it is \
                 answerable - this is the RIG malfunctioning, not a property of the gateway"
            ),
            Undecidable::WindowTooShort => format!(
                "the {window_label} did not span enough time to call a plateau, so \"it settled\" \
                 would be a claim about a window too brief for settling to mean anything"
            ),
        }
    }
}

impl Verdict {
    /// Did the window settle? A named predicate rather than `matches!(v, Verdict::Steady { .. })` at
    /// each site: a brace-pattern inside `prop_assert!` breaks that macro's stringification.
    pub fn is_steady(&self) -> bool {
        matches!(self, Verdict::Steady { .. })
    }
}

/// Keep only the trailing `window_s` seconds of `samples`, anchored to the last sample's own
/// timestamp (not wall-clock time), so a series that ended before "now" still windows correctly and a
/// fast-settling series isn't diluted by its own initial ramp.
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
/// endpoint-minus-endpoint, so a single noisy first or last sample can't set the whole reported rate.
/// Absent (not a measured zero) when fewer than two samples, or all samples share one timestamp.
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
///   trend: the two halves' means differ by less than `trend_pct`, in either direction.
///   range: (max - min) across the window is less than `range_pct` of the window's mean.
///
/// The trend test is two-sided (compares `drift.abs()`): a signed value against a positive threshold
/// would bound growth only, letting any decline however steep read as settled, but `Verdict::Steady`
/// means "not moving in any direction that matters". A falling window is not treated as leaking
/// either — it publishes as `Shape::Falling` rather than being waved through as steady.
///
/// The range test then bounds how far the series may travel regardless of trend, so oscillation
/// around a flat mean (no net drift, but the reading depends on which instant you sampled) is still
/// rejected.
///
/// An odd-sized window gives its extra sample to the second half, so a late upward sample is never
/// the one a rounding choice drops.
///
/// Calibration: for a linear ramp, window size and thresholds trade off directly, since both tests
/// reduce to "how far did the value move, relative to the mean, across about half the window".
/// Halving `window_s` halves the elapsed time the trend test can see, so it takes double the rate to
/// produce the same measured drift — e.g. on a ~120 MiB base at a 1% trend gate, a leak must hold
/// under ~2.4 MiB/min to certify steady at 60s but under ~4.8 MiB/min at 30s. Shrinking the window
/// loosens the bar, it doesn't detect leaks faster. See the halving-window test below.
pub fn plateau_check(samples: &[Sample], window_s: f64, trend_pct: f64, range_pct: f64) -> Verdict {
    let win = window(samples, window_s);
    let n = win.len();
    if n < 4 {
        return Verdict::Undecidable(Undecidable::TooFewReadings { got: n, need: 4 });
    }
    let h = n / 2;
    let (first, second) = win.split_at(h);
    let sum1: f64 = first.iter().map(|s| s.mib).sum();
    let sum2: f64 = second.iter().map(|s| s.mib).sum();
    let mean1 = sum1 / first.len() as f64;
    let mean2 = sum2 / second.len() as f64;
    let mean = (sum1 + sum2) / n as f64;
    let growth_rate_mib_per_min = growth_rate(&win);

    // A non-positive mean makes both percentages meaningless, so this is a real NotSteady result, not
    // undecidable.
    if mean <= 0.0 {
        return Verdict::NotSteady {
            growth_rate_mib_per_min,
            // No usable mean means no usable drift percentage, so the shape can't be told from these
            // samples. `Oscillating` is the conservative answer: it doesn't accuse the gateway of leaking.
            shape: Shape::Oscillating,
        };
    }

    let drift = (mean2 - mean1) / mean * 100.0;
    // Goes through `min`/`max` (which refuse a non-finite sample and return `Absent::HarnessError`)
    // rather than an inline fold, so a NaN/infinity in the window is reported as a rig fault
    // (Undecidable) instead of silently producing a bogus spread.
    let samples: Vec<f64> = win.iter().map(|s| s.mib).collect();
    let (Some(lo), Some(hi)) = (min(&samples).copied(), max(&samples).copied()) else {
        return Verdict::Undecidable(Undecidable::NonFiniteSample);
    };
    let spread = (hi - lo) / mean * 100.0;

    // `drift` is signed; comparing it directly against a positive threshold would bound growth only
    // (any decline would read as settled). `Verdict::Steady` means "not moving in any direction that
    // matters", hence `drift.abs()`.
    if drift.abs() < trend_pct && spread < range_pct {
        Verdict::Steady {
            growth_rate_mib_per_min,
        }
    } else {
        // Shape is decided by drift (net movement between halves), not spread: a window that ranged
        // 40% but ended where it started is a wave, not a climb.
        let shape = if drift >= trend_pct {
            Shape::Climbing
        } else if drift <= -trend_pct {
            Shape::Falling
        } else {
            Shape::Oscillating
        };
        Verdict::NotSteady {
            growth_rate_mib_per_min,
            shape,
        }
    }
}

/// A non-finite sample makes an order statistic unanswerable, so it is refused rather than sorted.
/// `partial_cmp(..).unwrap_or(Equal)` treats NaN as equal to everything, which is not a total order:
/// it leaves the whole slice in an unspecified permutation, so the same samples can sort to a
/// different median/percentile depending on arrival order.
///
/// Reported as `HarnessError`, not `NotMeasured`: a non-finite latency is the rig malfunctioning, not
/// a property of the gateway. `run.rs` refuses a non-finite stream rate the same way
/// (`StreamWindow::engine_fault`).
fn refuse_non_finite(values: &[f64], what: &str) -> Option<Measurement<f64>> {
    let bad = values.iter().filter(|v| !v.is_finite()).count();
    if bad == 0 {
        return None;
    }
    Some(Measurement::absent_because(
        Absent::HarnessError,
        format!(
            "{bad} of {} samples were not finite, so the {what} cannot be computed: ordering a slice \
             containing NaN or an infinity leaves it in an unspecified permutation, and the answer \
             would depend on the order the samples arrived in rather than on the samples",
            values.len()
        ),
    ))
}

/// The median of `values`. Even counts average the two middle values after sorting. Absent (never
/// zero) on an empty slice; refused if any sample is non-finite (see `refuse_non_finite`).
pub fn median(values: &[f64]) -> Measurement<f64> {
    if values.is_empty() {
        return Measurement::absent(Absent::NotMeasured);
    }
    if let Some(refusal) = refuse_non_finite(values, "median") {
        return refusal;
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

/// The smallest value in `values`. Absent (never zero or a sentinel) on an empty slice, and refused on
/// a non-finite one for the same reason `median`/`percentile` refuse it: `f64::min`/`f64::max` ignore
/// NaN and return the other operand, so `reduce(f64::min)` would silently drop a bad sample and report
/// a min over the rest as though the window were clean.
pub fn min(values: &[f64]) -> Measurement<f64> {
    if let Some(refusal) = refuse_non_finite(values, "minimum") {
        return refusal;
    }
    match values.iter().copied().reduce(f64::min) {
        Some(v) => Measurement::Measured(v),
        None => Measurement::absent(Absent::NotMeasured),
    }
}

/// The largest value in `values`. Absent on an empty slice, refused on a non-finite one - see `min`.
pub fn max(values: &[f64]) -> Measurement<f64> {
    if let Some(refusal) = refuse_non_finite(values, "maximum") {
        return refusal;
    }
    match values.iter().copied().reduce(f64::max) {
        Some(v) => Measurement::Measured(v),
        None => Measurement::absent(Absent::NotMeasured),
    }
}

/// The one percentile convention this engine uses. Every published percentile (load generator,
/// search, streaming TTFT/inter-frame-gap) resolves its rank through here, so none can mean something
/// different by the same name.
///
/// Nearest rank, ceiling: the 0-based index of the `ceil(n * p)`-th smallest value, clamped into
/// `0..n`. Never interpolates, so a published percentile is always a value some sample really
/// produced.
///
/// Ceil, not floor: at whole-number `n * p` the two disagree by exactly one rank, and floor puts p99
/// at the last index — i.e. the maximum, not a tail percentile. Ceil is also the textbook nearest-rank
/// definition (smallest value at or above which at least `p` of the data falls).
///
/// `n` must be non-zero; an empty sample set has no percentile, and callers answer that with an
/// absence rather than a rank.
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
    // Same refusal as `median`: with a NaN present the sort output is an unspecified permutation.
    if let Some(refusal) = refuse_non_finite(values, "percentile") {
        return refusal;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Measurement::Measured(sorted[nearest_rank_index(sorted.len(), p)])
}

#[cfg(test)]
mod tests {
    // An order statistic over a non-finite sample is refused, not guessed: sorting with NaN present
    // gives an arrival-order-dependent answer, which is disqualifying for reproducible results.
    #[test]
    fn a_non_finite_sample_is_refused_rather_than_sorted() {
        let with_nan = [10.0, 20.0, 30.0, f64::NAN, 50.0];
        match percentile(&with_nan, 0.99) {
            Measurement::Absent { reason, detail } => {
                assert!(
                    matches!(reason, Absent::HarnessError),
                    "a non-finite latency is the RIG malfunctioning, so it must be a HarnessError - \
                     NotMeasured would file the rig's fault under the gateway's results"
                );
                let detail = detail.unwrap_or_default();
                assert!(
                    detail.contains("not finite"),
                    "the refusal must say what was wrong with the input, got: {detail}"
                );
            }
            other => panic!("percentile published {other:?} over a slice containing NaN"),
        }
        assert!(
            matches!(median(&with_nan), Measurement::Absent { .. }),
            "median must refuse the same input percentile refuses; they share the broken comparator"
        );
        assert!(
            matches!(
                percentile(&[1.0, f64::INFINITY], 0.5),
                Measurement::Absent { .. }
            ),
            "an infinity is equally unorderable in practice and equally a rig fault"
        );

        // min/max fail more quietly: `f64::min`/`f64::max` ignore NaN and silently drop the bad sample
        // rather than corrupt a sort. Every statistic over a window must reach the same verdict about
        // whether that window is usable.
        assert!(
            matches!(min(&with_nan), Measurement::Absent { .. }),
            "min silently skipped the NaN and reported a minimum over the remaining samples"
        );
        assert!(
            matches!(max(&with_nan), Measurement::Absent { .. }),
            "max silently skipped the NaN and reported a maximum over the remaining samples"
        );
        assert_eq!(
            min(&[3.0, 1.0, 2.0]),
            Measurement::Measured(1.0),
            "the clean path must still answer"
        );
        assert_eq!(
            max(&[3.0, 1.0, 2.0]),
            Measurement::Measured(3.0),
            "the clean path must still answer"
        );
    }

    // A permutation of the same sample set must never change the answer — that's what makes the
    // number a measurement rather than an artifact of arrival order.
    #[test]
    fn an_order_statistic_never_depends_on_the_order_samples_arrived_in() {
        let a = [10.0, 20.0, 30.0, f64::NAN, 50.0];
        let b = [f64::NAN, 10.0, 20.0, 30.0, 50.0];
        assert_eq!(
            format!("{:?}", median(&a)),
            format!("{:?}", median(&b)),
            "the same five samples in a different order produced different medians"
        );

        // And the clean path keeps its ordering-independence, so the fix is not merely refusing
        // everything: a real window must still answer, and answer identically under permutation.
        let clean = [50.0, 10.0, 40.0, 20.0, 30.0];
        let rotated = [30.0, 50.0, 10.0, 40.0, 20.0];
        assert_eq!(median(&clean), Measurement::Measured(30.0));
        assert_eq!(median(&clean), median(&rotated));
        assert_eq!(percentile(&clean, 1.0), Measurement::Measured(50.0));
        assert_eq!(percentile(&clean, 1.0), percentile(&rotated, 1.0));
    }

    // A steady window still has a slope, and publishes it rather than an assumed zero — otherwise a
    // window drifting just under `trend_pct` would report a growth rate of exactly 0.000, hiding a
    // slow leak.
    #[test]
    fn a_steady_verdict_carries_the_slope_it_fitted_rather_than_an_assumed_zero() {
        // Ten readings climbing gently: about 0.5% across the window, inside a 1% trend bar, so this is
        // genuinely Steady - and genuinely still moving.
        let s: Vec<super::Sample> = (0..10)
            .map(|i| super::Sample {
                t_s: i as f64 * 6.0,
                mib: 100.0 + i as f64 * 0.05,
            })
            .collect();
        let v = super::plateau_check(&s, f64::INFINITY, 1.0, 2.0);
        assert!(
            v.is_steady(),
            "a 0.5% drift inside a 1% bar is steady: {v:?}"
        );
        let super::Verdict::Steady {
            growth_rate_mib_per_min,
        } = &v
        else {
            unreachable!("just asserted steady")
        };
        let rate = growth_rate_mib_per_min
            .copied()
            .expect("ten readings over a minute fit a slope");
        assert!(
            rate > 0.0,
            "the window IS climbing at {rate} MiB/min - publishing 0.0 here would tell a reader the \
             memory was flat when this function measured that it was not"
        );
        // And it is the same slope `growth_rate` computes standalone: the verdict must not carry a
        // second, differently-derived rate.
        let direct = super::growth_rate(&s).copied().expect("a slope");
        assert!((rate - direct).abs() < 1e-9, "{rate} vs {direct}");
    }

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
        assert!(matches!(
            plateau_check(&s, WHOLE, 1.0, 2.0),
            Verdict::Steady { .. }
        ));
    }

    #[test]
    fn a_steadily_rising_series_is_not_a_plateau() {
        let s = mkseries(100.0, 1.0, 30);
        assert!(!matches!(
            plateau_check(&s, WHOLE, 1.0, 2.0),
            Verdict::Steady { .. }
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
            Verdict::Steady { .. }
        ));
    }

    #[test]
    fn oscillation_around_a_flat_mean_is_not_a_plateau() {
        let s = mkseries_jitter(100.0, 0.0, 30, 5.0);
        assert!(!matches!(
            plateau_check(&s, WHOLE, 1.0, 2.0),
            Verdict::Steady { .. }
        ));
    }

    // The trend gate is one-sided: falling memory means the gateway is releasing, which is not the
    // failure this gate exists to catch, so a small downward drift inside the range gate still passes.
    #[test]
    fn a_slight_downward_drift_within_the_range_gate_is_a_plateau() {
        let s = mkseries(100.0, -0.02, 30);
        assert!(matches!(
            plateau_check(&s, WHOLE, 1.0, 2.0),
            Verdict::Steady { .. }
        ));
    }

    #[test]
    fn too_few_samples_is_undecidable_not_a_plateau() {
        assert!(matches!(
            plateau_check(&[], WHOLE, 1.0, 2.0),
            Verdict::Undecidable(Undecidable::TooFewReadings { .. })
        ));
    }

    #[test]
    fn three_samples_is_still_too_few_to_judge() {
        let s = mkseries(100.0, 0.0, 3);
        assert!(matches!(
            plateau_check(&s, WHOLE, 1.0, 2.0),
            Verdict::Undecidable(Undecidable::TooFewReadings { .. })
        ));
    }

    #[test]
    fn exactly_four_samples_is_enough_to_judge() {
        let s = mkseries(100.0, 0.0, 4);
        assert!(matches!(
            plateau_check(&s, WHOLE, 1.0, 2.0),
            Verdict::Steady { .. }
        ));
    }

    // A sawtooth and a climb both fail the steadiness test but must be told apart: one is a GC cycle
    // returning to its starting level, the other never has a level, and branding the first as a leak
    // is the exact accusation `Shape` prevents.
    #[test]
    fn a_sawtooth_and_a_climb_both_fail_to_settle_and_are_told_apart() {
        // A wave: swings 40 MiB peak to trough, ends where it began. Wide spread, no drift.
        let wave: Vec<Sample> = (0..40)
            .map(|i| Sample {
                t_s: i as f64,
                mib: 100.0 + if i % 2 == 0 { 20.0 } else { -20.0 },
            })
            .collect();
        match plateau_check(&wave, WHOLE, 1.0, 2.0) {
            Verdict::NotSteady { shape, .. } => assert_eq!(
                shape,
                Shape::Oscillating,
                "a window that returns to where it started is a wave, never a leak"
            ),
            other => panic!("a 40 MiB swing is not settled: {other:?}"),
        }

        // A climb: rises steadily and is still rising at the end.
        let climb: Vec<Sample> = (0..40)
            .map(|i| Sample {
                t_s: i as f64,
                mib: 100.0 + i as f64 * 5.0,
            })
            .collect();
        match plateau_check(&climb, WHOLE, 1.0, 2.0) {
            Verdict::NotSteady { shape, .. } => {
                assert_eq!(
                    shape,
                    Shape::Climbing,
                    "unbounded growth is the real defect"
                )
            }
            other => panic!("a 200 MiB rise is not settled: {other:?}"),
        }

        // And the mirror image: still RELEASING when the window closed. Not steady, but the opposite
        // of a leak, and it must never be labelled as one.
        let falling: Vec<Sample> = (0..40)
            .map(|i| Sample {
                t_s: i as f64,
                mib: 300.0 - i as f64 * 5.0,
            })
            .collect();
        match plateau_check(&falling, WHOLE, 1.0, 2.0) {
            Verdict::NotSteady { shape, .. } => assert_eq!(
                shape,
                Shape::Falling,
                "a window still giving memory back is not a leak"
            ),
            other => panic!("a 200 MiB decline is not settled: {other:?}"),
        }
    }

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
                growth_rate_mib_per_min: growth_rate(&s),
                // drift is +1.0, exactly the trend bar, so this is the boundary case for CLIMBING.
                shape: Shape::Climbing,
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
            Verdict::Steady { .. }
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

    // A decline must fail the trend test at the same magnitude as a climb, since `drift` is signed
    // and `Verdict::Steady` means "not moving in any direction that matters".
    #[test]
    fn a_declining_window_fails_the_trend_test_at_the_same_magnitude_as_a_climbing_one() {
        // Gentle on purpose: the mean must stay well clear of zero, or the earlier `mean <= 0.0`
        // short-circuit masks whether the trend comparison itself is symmetric.
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
                Verdict::Steady { .. }
            );
            if steady {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    // The calibration argument, pinned exactly: halving the window halves the elapsed time the trend
    // test can see, so it takes double the rate to produce the same measured drift.
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

    // Pins the ranks directly (ledger SRCH-04) so a future edit cannot reintroduce a ceil/floor split:
    // at n=100, floor would put p99 at index 99 — the maximum, not a tail percentile.
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
        // A rise steep enough to matter is never certified steady, for any window/sample-count/
        // threshold combination — not just the one worked example above. The gate is a threshold, not
        // a leak detector: a rate below the boundary genuinely does pass, on purpose.
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
            prop_assert!(!verdict.is_steady());
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
            // Bound first: `prop_assert!` stringifies its expression as a FORMAT STRING, so a
            // `{ .. }` pattern inside the macro is parsed as a placeholder and fails to compile.
            let too_few = matches!(
                plateau_check(&s, 9_999.0, 1.0, 2.0),
                Verdict::Undecidable(Undecidable::TooFewReadings { .. })
            );
            prop_assert!(too_few);
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
