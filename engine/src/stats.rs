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
// the two-sided drift rule and the undecidable-sample-count rule.
//
// Not reaching a plateau is a measured result, not a failure: it is returned as `Verdict::NotSteady`
// carrying the growth rate, which is the most informative thing this gate can say about a series that
// never settles. A caller must publish that rather than quietly substituting the last sample.

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

/// HOW a window failed to settle. A gateway whose memory oscillates around a stable level - a
/// garbage collector doing its job, an allocator returning and reclaiming arenas - is not leaking,
/// however far it swings. A gateway whose memory trends upward for the whole window and is still
/// rising at the end has no level at all, and that is the finding a reader needs.
///
/// Both fail the steadiness test, so publishing them under one word ("NEVER SETTLES", in red, beside
/// a leak rate) brands a healthy sawtooth as a leak. The two are separable from numbers the test
/// already computes: `drift` is the net movement between the window's halves, `spread` is how far it
/// ranged. A climb has large drift. A wave has large spread and near-zero drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// Trending in one direction across the window and still going at the end. Unbounded growth is
    /// the real defect this metric exists to catch.
    Climbing,
    /// Ranging widely but with no net trend: it returns to where it was. Not a leak.
    Oscillating,
    /// Trending DOWNWARD across the window - still releasing memory when the window closed. Not
    /// steady, but the opposite of a leak, and it must never be labelled as one.
    Falling,
}

/// The outcome of the steadiness gate over a trailing window.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Both the trend test and the range test passed: the window is not moving in any direction that
    /// matters, and a caller may publish a steady-state number from it.
    ///
    /// IT CARRIES THE MEASURED RATE TOO, and that is the point. This was a unit variant, and the caller
    /// answered a Steady verdict by publishing `growth_rate_mib_per_min: Measured(0.0)` - substituting an
    /// exact zero for the slope this function had just fitted. So a gateway drifting 0.9% across the
    /// window (inside `MEMORY_TREND_PCT`, therefore "steady") published a growth rate of exactly zero,
    /// and the number a reader would use to judge a slow leak was a constant chosen by a threshold rather
    /// than anything measured. "Steady" is a verdict; the rate is a measurement; the artifact publishes
    /// both, in separate fields, and neither may be derived from the other here.
    Steady {
        growth_rate_mib_per_min: Measurement<f64>,
    },
    /// A real, publishable result: the window did not settle. Carries the growth rate so a caller can
    /// never say "not steady" without also saying how fast it moved, and the SHAPE of the movement,
    /// because "did not settle" describes two very different gateways and only one of them is a
    /// defect. See `Shape`.
    NotSteady {
        growth_rate_mib_per_min: Measurement<f64>,
        shape: Shape,
    },
    /// The window could not be judged - AND IT CARRIES WHY, because there are now two causes and they
    /// belong to different parties.
    ///
    /// This was a unit variant documented as meaning only "fewer than four samples". Then a second
    /// producer appeared: `min`/`max` refuse a non-finite sample and return `Absent::HarnessError`,
    /// which `plateau_check` turned into this same bare `Undecidable`. Both of its callers in
    /// `metric.rs` then published `Absent::NotMeasured` with the reason "too few readings fell inside
    /// the window" - on a FULL window, for a failure that was OURS. The evidence naming it as the rig's
    /// was generated by `refuse_non_finite` and discarded one line later, which is the exact shape the
    /// refusal was added to prevent.
    ///
    /// A unit variant cannot be published honestly by a caller that has two things to distinguish, so
    /// the cause rides with the verdict - the same reasoning as `NotSteady` carrying its growth rate and
    /// `StreamStop` carrying its ending.
    Undecidable(Undecidable),
}

/// Why a window could not be judged. The two are not interchangeable: one is a coverage gap in the
/// measurement, the other is the harness malfunctioning, and the board must never file the second as
/// the first (invariant: never attribute the rig to the gateway).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undecidable {
    /// Fewer than four samples fell inside the window. A fact about the MEASUREMENT: not enough
    /// evidence to judge either way, and distinct from `NotSteady` on purpose - "we could not tell"
    /// and "we could tell and it moved" are different claims.
    TooFewReadings { got: usize, need: usize },
    /// A sample was NaN or an infinity, so no order statistic over this window is answerable. A fact
    /// about the RIG, and it must reach the artifact as `Absent::HarnessError`.
    NonFiniteSample,
    /// The window held enough readings but did not span enough TIME to call a plateau. Distinct from
    /// `TooFewReadings` because a dense short window and a sparse long one fail for different reasons
    /// and a reader deserves to know which - `metric.rs` demotes a `Steady` verdict here rather than
    /// publish "it settled" about a window too brief for settling to mean anything.
    WindowTooShort,
}

impl Undecidable {
    /// The absence variant a caller must publish for this cause. Provided here rather than left to each
    /// caller because getting it backwards is the failure this enum exists to prevent, and there are
    /// two callers who both got it wrong when the choice was theirs.
    pub fn absent_kind(&self) -> Absent {
        match self {
            Undecidable::TooFewReadings { .. } | Undecidable::WindowTooShort => Absent::NotMeasured,
            Undecidable::NonFiniteSample => Absent::HarnessError,
        }
    }

    /// The reason string, so the two callers cannot describe the same cause differently.
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
    /// Did the window settle? A NAMED PREDICATE rather than `matches!(v, Verdict::Steady { .. })` at each
    /// site: `Steady` gained a field (the fitted slope it used to make its caller invent), and a
    /// brace-pattern inside `prop_assert!` breaks that macro's stringification of the expression. Callers
    /// that only ask the yes/no question should not have to spell the payload they are ignoring.
    pub fn is_steady(&self) -> bool {
        matches!(self, Verdict::Steady { .. })
    }
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
    // A NaN or an infinity anywhere in the series poisons the fit; the order statistics below refuse
    // that input as OUR malfunction, and so does this, rather than publishing a non-finite rate.
    let rate = (num / den) * 60.0;
    if !rate.is_finite() {
        return Measurement::absent_because(
            Absent::HarnessError,
            "the series holds a non-finite sample or timestamp, so no rate can be fitted",
        );
    }
    Measurement::Measured(rate)
}

/// The steadiness gate. Windows `samples` to the trailing `window_s` seconds, then requires BOTH:
///   trend: the two halves' means differ by less than `trend_pct`, IN EITHER DIRECTION.
///   range: (max - min) across the window is less than `range_pct` of the window's mean.
///
/// The trend test is TWO-SIDED, and this comment used to say the opposite - that only a rise
/// disqualified, on the reasoning that a falling series is releasing rather than leaking. The code
/// deliberately compares `drift.abs()` instead, and the note at that comparison explains why: a
/// signed value against a positive threshold bounds growth ONLY, so any decline however steep is
/// always less than the threshold and would read as settled. `Verdict::Steady`'s own contract is "not
/// moving in any direction that matters", and a window still handing memory back at speed is moving.
/// Falling is not treated as leaking either - it is published as its own shape (`Shape::Falling`), so
/// the honest reading survives without the gate having to wave it through.
///
/// The range test then bounds how far the series may travel within the window regardless of trend, so
/// one oscillating hard around a flat mean - no net drift, but the value read depends on which instant
/// you sampled - is still correctly rejected.
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

    // A NON-FINITE SAMPLE IS UNDECIDABLE BEFORE ANYTHING IS DECIDED FROM IT. A `-inf` drags the mean
    // to `-inf` and used to be classified by the non-positive-mean arm below as an `Oscillating`
    // gateway, before the non-finite check further down could fire.
    if win.iter().any(|s| !s.mib.is_finite()) {
        return Verdict::Undecidable(Undecidable::NonFiniteSample);
    }

    // A non-positive mean makes both percentages meaningless (division by zero or sign flip), so this
    // cannot be called steady; it is a real result (not undecidable), matching the shell original.
    if mean <= 0.0 {
        return Verdict::NotSteady {
            growth_rate_mib_per_min,
            // No usable mean means no usable drift percentage either, so the shape cannot be told
            // from these samples. `Oscillating` is the conservative answer: it is the one that does
            // not accuse the gateway of leaking.
            shape: Shape::Oscillating,
        };
    }

    let drift = (mean2 - mean1) / mean * 100.0;
    // THROUGH `min`/`max`, not a second inline fold. These two lines used to roll their own
    // `fold(f64::INFINITY, f64::min)` while `stats::min` and `stats::max` sat unused a hundred lines
    // below with unit tests of their own - so the tests read as coverage of this engine's min/max
    // handling while the number that actually reaches the board's spread and plateau figures came from
    // code they never touched.
    //
    // THE ABSENT CASE IS REACHABLE NOW, and this comment used to say it was not. It read "`win` is
    // non-empty here ... so the absent case cannot arise" - true when min/max only failed on an empty
    // slice. They now REFUSE a non-finite sample (a NaN or an infinity out of a /proc read or a
    // division in the sampler) and return `Absent::HarnessError` naming how many samples were bad.
    // `unwrap_or(±INFINITY)` threw that away: `hi - lo` became negative infinity, and the function went
    // on to publish a `NotSteady` verdict about the GATEWAY'S memory for a window the engine had just
    // proved was OUR malfunction. The one piece of evidence naming it as ours was generated and
    // discarded on the same line - which is the defect shape this whole audit keeps finding.
    //
    // Undecidable is the honest answer: the window cannot be judged, and that is not a fact about the
    // gateway. The caller already publishes an absence with a reason for this verdict.
    let samples: Vec<f64> = win.iter().map(|s| s.mib).collect();
    let (Some(lo), Some(hi)) = (min(&samples).copied(), max(&samples).copied()) else {
        return Verdict::Undecidable(Undecidable::NonFiniteSample);
    };
    let spread = (hi - lo) / mean * 100.0;

    // MAGNITUDE, NOT SIGN. `drift` is signed - positive when the window is still climbing, negative
    // when it is declining - but `Verdict::Steady`'s own contract is "not moving in any direction
    // that matters", so the comparison uses `drift.abs()`: comparing the signed value against a
    // positive threshold would bound growth only, since any decline, however steep, is always less
    // than a positive threshold and would read as settled.
    if drift.abs() < trend_pct && spread < range_pct {
        // The fitted slope travels WITH the verdict. It is already computed above for every path, and it
        // was being dropped on exactly the path where the caller would otherwise invent a zero.
        Verdict::Steady {
            growth_rate_mib_per_min,
        }
    } else {
        // The shape is decided by DRIFT, the net movement between the two halves, not by spread.
        // A window that ranged 40% but ended where it started is a wave; one that drifted upward is
        // a climb even if it did so smoothly. The same `trend_pct` bar that decides "settled"
        // decides "has a direction", so a window cannot be called directionless by one test and
        // directional by the other.
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

/// A NON-FINITE SAMPLE MAKES AN ORDER STATISTIC UNANSWERABLE, SO IT IS REFUSED RATHER THAN SORTED.
///
/// `median` and `percentile` both sorted with `a.partial_cmp(b).unwrap_or(Ordering::Equal)`, which
/// makes a NaN compare EQUAL TO EVERYTHING. That is not a total order, and an inconsistent comparator
/// does not merely misplace the NaN - it leaves the whole slice in an unspecified permutation. Measured
/// on this comparator:
///
/// ```text
/// one NaN in ten samples          -> [10,20,30,40,50,60,70,80,90,NaN], p99 reads NaN
/// the same values, NaN rotated    -> [80,90,NaN,10,20,30,40,50,60,70], not sorted at all
/// median of the same five values, NaN first vs NaN in the middle -> 30 vs 20
/// ```
///
/// (Fenced as `text` deliberately: an indented block in a doc comment is a DOCTEST, and rustdoc tried
/// to compile this table as Rust. CI caught it - `expected one of ! or ::, found NaN` - on the very
/// commit that fixed a correctness bug, which is how a real fix gets reverted for a cosmetic reason.)
///
/// The last line is the one that matters: THE SAME SET OF SAMPLES PRODUCED A DIFFERENT PUBLISHED NUMBER
/// DEPENDING ON THE ORDER THEY ARRIVED IN. A board whose premise is that every number regenerates from
/// committed JSON cannot contain a figure that depends on arrival order, and "NaN sorts as equal to
/// everything" is exactly the kind of undeclared rule that has no business authoring a measurement -
/// the same fault as the frontier's tie-break falling out of `max_by` keeping the last maximum.
///
/// So this is a HarnessError, not a NotMeasured: a non-finite latency is the rig malfunctioning, and it
/// must be reported as the rig's failure rather than quietly absorbed into a percentile or blamed on the
/// gateway. `run.rs` already refuses a non-finite stream rate the same way (`StreamWindow::engine_fault`).
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

/// The median of `values`. Even counts average the two middle values after sorting, matching the
/// shell harness's own median helpers. Absent (never zero) on an empty slice, and refused outright if
/// any sample is non-finite - see `refuse_non_finite`.
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

/// The smallest value in `values`. Absent (never zero, never an arbitrary sentinel) on an empty slice,
/// and REFUSED on a non-finite one for the same reason `median` and `percentile` refuse it.
///
/// `f64::min` and `f64::max` IGNORE NaN - they return the other operand - so `reduce(f64::min)` over a
/// slice containing one silently drops it and reports a min over the remaining samples as though the
/// window were clean. That is quieter than the sort corruption those two had, and worse in one respect:
/// there is no disagreement for anyone to notice. A rig that produced a non-finite latency is
/// malfunctioning, and every order statistic over that window must say so rather than each deciding
/// separately how much of the window to believe.
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
    // Same refusal as `median`, and for the same reason: with a NaN present the sort's output is an
    // unspecified permutation, so `sorted[nearest_rank_index(..)]` would return whichever element the
    // arrival order happened to leave at that index. Every p50 and p99 on the board comes through here.
    if let Some(refusal) = refuse_non_finite(values, "percentile") {
        return refusal;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Measurement::Measured(sorted[nearest_rank_index(sorted.len(), p)])
}

#[cfg(test)]
#[path = "tests/stats_tests.rs"]
mod tests;
