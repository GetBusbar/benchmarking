// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Box qualification: deciding whether a cloud box is fit to measure on before it measures anything.
//
// A contaminated box - one whose peak throughput has collapsed - must not run a full 6x6 and have the
// result published as a gateway regression. It replays a known load with no gateway in the path and
// compares the throughput against a rolling baseline (`PEAK_DRIFT_PCT`), and that verdict is published
// as `rig.box_qualify` inside the snapshot.
//
// ONE STAGE, NOT TWO. This header used to describe a stage 1 that compared the box's gateway-free
// LATENCY FLOOR against its own baseline, ahead of the throughput stage. That stage is not wired:
// `FLOOR_DRIFT_PCT` below appears nowhere outside its own definition and this module's tests, and
// `Sense::LowerIsBetter` exists only to serve it. The machinery is kept because the throughput stage
// shares `judge`, and a latency stage would be a caller away - but a reader must not be told two
// things guard the box when one does.

use crate::measurement::{Absent, Measurement};
use serde::{Deserialize, Serialize};

pub const FLOOR_DRIFT_PCT: f64 = 4.0;
pub const PEAK_DRIFT_PCT: f64 = 25.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Measured against a baseline and within band.
    Pass,
    /// Measured against a baseline and outside band. Never seeds a baseline.
    Fail,
    /// No baseline existed, so there was nothing to compare against. This run IS the baseline.
    Seed,
    /// The stage could not run (nothing to replay, no observation). Not a judgement either way.
    Skipped,
}

impl Outcome {
    pub fn token(&self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Fail => "fail",
            Outcome::Seed => "seed",
            Outcome::Skipped => "skip",
        }
    }

    /// Whether a run carrying this outcome may contribute to the rolling baseline.
    ///
    /// A `seed` is a measurement taken on a box nothing was wrong with, so it qualifies same as a
    /// `pass`: peak baselines are per gateway, so a new gateway's first run must seed the baseline
    /// or stage 2 (the strong half of qualification) can never switch on for it. A `fail` never
    /// qualifies, and a `skip` measured nothing to contribute.
    pub fn qualifies_as_baseline(&self) -> bool {
        matches!(self, Outcome::Pass | Outcome::Seed)
    }

    /// The inverse of `token`: read an outcome back off a published artifact.
    ///
    /// Written out rather than derived from serde, because the published vocabulary is `token`'s and
    /// `Skipped` publishes as `"skip"` - a `#[serde(rename_all = "snake_case")]` round trip would
    /// silently fail to parse the one token whose name is not its variant's. `None` for anything
    /// else, including a token a future engine adds: an outcome this build cannot name is one it
    /// cannot vouch for, and the caller (`suite::qualify_history_on_disk`) treats that as
    /// "not known to qualify" rather than guessing.
    pub fn from_token(token: &str) -> Option<Outcome> {
        match token {
            "pass" => Some(Outcome::Pass),
            "fail" => Some(Outcome::Fail),
            "seed" => Some(Outcome::Seed),
            "skip" => Some(Outcome::Skipped),
            _ => None,
        }
    }
}

/// Which direction of deviation is the BAD one.
///
/// THE BANDS ARE ONE-SIDED, and that is deliberate rather than an oversight to tidy up. A box cannot
/// randomly get faster: contention, throttling and noisy neighbours only ever ADD latency and REMOVE
/// throughput. A floor that beats its baseline is the box showing its true clean-hardware speed, and
/// it implies the BASELINE was the noisy measurement rather than this run. Failing it would terminate
/// healthy boxes, burn the replacement budget, and eventually skip the gateway entirely. The absolute
/// envelope, which IS two-sided, is what bounds an absurd improvement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sense {
    /// Higher is worse (p99 latency): a POSITIVE drift is the regression.
    LowerIsBetter,
    /// Higher is better (throughput): a NEGATIVE drift is the regression.
    HigherIsBetter,
}

/// Percentage drift of an observation from a baseline. Positive means the observation is higher.
fn drift_pct(observed: f64, baseline: f64) -> Option<f64> {
    if !baseline.is_finite() || baseline == 0.0 || !observed.is_finite() {
        return None;
    }
    Some((observed - baseline) / baseline * 100.0)
}

/// The magnitude of a deviation in the DEGRADING direction only, or 0 when it is an improvement.
pub fn regression(drift: f64, sense: Sense) -> f64 {
    let d = match sense {
        Sense::LowerIsBetter => drift,
        Sense::HigherIsBetter => -drift,
    };
    if d < 0.0 {
        0.0
    } else {
        d
    }
}

/// Judge an observation against a rolling baseline.
///
/// `baseline` absent means no history: the run seeds rather than passing, because "within band of
/// nothing" is not a measurement. That distinction is why `Seed` exists as its own outcome.
///
/// `observed` absent is a FAIL, not a pass and not a shrug. We cannot qualify a box we failed to
/// measure, and running a full matrix on one is exactly the incident this gate exists to prevent.
pub fn judge(
    observed: Measurement<f64>,
    baseline: Measurement<f64>,
    band_pct: f64,
    sense: Sense,
) -> (Outcome, Measurement<f64>) {
    let Some(&obs) = observed.value() else {
        return (
            Outcome::Fail,
            Measurement::absent_because(
                Absent::NotMeasured,
                "the stage produced no usable observation",
            ),
        );
    };
    let Some(&base) = baseline.value() else {
        return (
            Outcome::Seed,
            Measurement::absent_because(Absent::NotMeasured, "no baseline yet"),
        );
    };
    match drift_pct(obs, base) {
        // A gate must never fail on a value it never obtained: an unusable baseline means this
        // particular check does not fire, which is not the same as the observation being unmeasured.
        None => (
            Outcome::Skipped,
            Measurement::absent_because(Absent::NotMeasured, "baseline is not a usable number"),
        ),
        Some(d) => {
            let outcome = if regression(d, sense) <= band_pct {
                Outcome::Pass
            } else {
                Outcome::Fail
            };
            (outcome, Measurement::Measured(d))
        }
    }
}

/// Rolling median of the baseline candidates. A single wild run must not own the baseline, which is
/// why this is a median and not the last value or a mean.
///
/// DELEGATED RATHER THAN REPEATED. This sorted and took the middle itself, using the same
/// `partial_cmp(b).unwrap_or(Equal)` comparator that made `stats::median` and `stats::percentile`
/// return an ARRIVAL-ORDER-DEPENDENT answer on a slice containing NaN. It was safe only because of
/// the `retain(is_finite)` on the line above - a guard this copy happened to have and the others did
/// not. `suite::steady_state` was the same statistic written a third time, and it had no such guard.
///
/// One statistic, one implementation: a guard added to the shared one now protects every caller,
/// which is exactly what the three-way duplication prevented. The `retain` stays because it is a
/// DIFFERENT rule from the shared one - qualification history legitimately accumulates non-finite
/// entries from old or partial runs and drops them, where a non-finite LATENCY sample means the rig
/// malfunctioned and must refuse. Dropping bad history is not the same act as refusing a bad window.
pub fn rolling_baseline(mut candidates: Vec<f64>) -> Measurement<f64> {
    candidates.retain(|v| v.is_finite());
    if candidates.is_empty() {
        return Measurement::absent_because(Absent::NotMeasured, "no qualifying history");
    }
    crate::stats::median(&candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A seed run is a measurement taken on a box nothing was wrong with, so it must become the
    // baseline, same as a pass; otherwise stage 2 could never switch on for a new gateway.
    #[test]
    fn a_seed_run_qualifies_as_baseline_data() {
        assert!(
            Outcome::Seed.qualifies_as_baseline(),
            "a seed run must become the baseline it was recorded to be"
        );
        assert!(Outcome::Pass.qualifies_as_baseline());
    }

    // A failed qualification must never seed a baseline: that would let a contaminated box define
    // what healthy looks like for every run after it.
    #[test]
    fn a_failed_or_skipped_run_never_seeds_a_baseline() {
        assert!(!Outcome::Fail.qualifies_as_baseline());
        assert!(!Outcome::Skipped.qualifies_as_baseline());
    }

    // No baseline is not the same as passing. Folding seed into pass is how "within band of
    // nothing" starts reading as a real comparison.
    #[test]
    fn no_baseline_seeds_rather_than_passing() {
        let (outcome, drift) = judge(
            Measurement::Measured(77.5),
            Measurement::absent(Absent::NotMeasured),
            FLOOR_DRIFT_PCT,
            Sense::LowerIsBetter,
        );
        assert_eq!(outcome, Outcome::Seed);
        assert_eq!(
            drift.copied(),
            None,
            "there is no drift against a baseline that does not exist"
        );
    }

    // An unmeasured stage is a hard fail, never a neutral shrug: a box we failed to measure must
    // not slip past the gate and run a full matrix.
    #[test]
    fn nothing_observed_is_a_fail_not_a_shrug() {
        let (outcome, drift) = judge(
            Measurement::absent(Absent::NotMeasured),
            Measurement::Measured(77.5),
            FLOOR_DRIFT_PCT,
            Sense::LowerIsBetter,
        );
        assert_eq!(
            outcome,
            Outcome::Fail,
            "an unmeasurable stage must not bypass the gate"
        );
        assert_eq!(drift.copied(), None);
    }

    // The bands are one-sided: a box cannot randomly get faster, since contention only ever adds
    // latency and removes throughput. A floor that beats its baseline means the BASELINE was the
    // noisy measurement, and failing it would terminate healthy boxes and burn the replacement
    // budget.
    #[test]
    fn an_improvement_never_fails_the_gate() {
        // Latency: 10% FASTER than baseline, far outside a 4% band, and still a pass.
        let (o, d) = judge(
            Measurement::Measured(90.0),
            Measurement::Measured(100.0),
            4.0,
            Sense::LowerIsBetter,
        );
        assert_eq!(
            o,
            Outcome::Pass,
            "a faster floor is a clean box, not a contaminated one"
        );
        assert!(
            (d.copied().unwrap_or_default() + 10.0).abs() < 1e-9,
            "the drift is still reported as -10%"
        );
        // Throughput: 30% HIGHER than baseline, outside a 25% band, and still a pass.
        let (o, _) = judge(
            Measurement::Measured(130.0),
            Measurement::Measured(100.0),
            25.0,
            Sense::HigherIsBetter,
        );
        assert_eq!(o, Outcome::Pass, "more throughput is not a regression");
    }

    #[test]
    fn only_the_degrading_direction_counts_toward_the_band() {
        assert_eq!(regression(-10.0, Sense::LowerIsBetter), 0.0);
        assert_eq!(regression(10.0, Sense::LowerIsBetter), 10.0);
        assert_eq!(regression(10.0, Sense::HigherIsBetter), 0.0);
        assert_eq!(regression(-10.0, Sense::HigherIsBetter), 10.0);
    }

    #[test]
    fn degradation_beyond_the_band_still_fails() {
        // Slower latency and lower throughput are the real regressions, and both must trip.
        let (o, _) = judge(
            Measurement::Measured(110.0),
            Measurement::Measured(100.0),
            4.0,
            Sense::LowerIsBetter,
        );
        assert_eq!(o, Outcome::Fail);
        let (o, _) = judge(
            Measurement::Measured(70.0),
            Measurement::Measured(100.0),
            25.0,
            Sense::HigherIsBetter,
        );
        assert_eq!(o, Outcome::Fail);
    }

    #[test]
    fn drift_inside_the_band_passes() {
        let (o, d) = judge(
            Measurement::Measured(103.0),
            Measurement::Measured(100.0),
            4.0,
            Sense::LowerIsBetter,
        );
        assert_eq!(o, Outcome::Pass);
        assert!((d.copied().unwrap_or_default() - 3.0).abs() < 1e-9);
    }

    #[test]
    fn exactly_at_the_band_edge_passes() {
        let (o, _) = judge(
            Measurement::Measured(104.0),
            Measurement::Measured(100.0),
            4.0,
            Sense::LowerIsBetter,
        );
        assert_eq!(o, Outcome::Pass, "the band is inclusive at its edge");
    }

    // A single wild run must not own the baseline. This is why the baseline is a median.
    #[test]
    fn one_wild_run_cannot_own_the_baseline() {
        let b = rolling_baseline(vec![77.0, 78.0, 77.5, 9000.0]);
        let v = b.copied().unwrap_or_default();
        assert!(v > 77.0 && v < 79.0, "median resists the outlier, got {v}");
    }

    #[test]
    fn an_empty_history_yields_no_baseline_not_zero() {
        let b = rolling_baseline(vec![]);
        assert_eq!(b.copied(), None);
        assert_eq!(b.reason(), Some(&Absent::NotMeasured));
    }

    #[test]
    fn non_finite_candidates_are_excluded_not_counted() {
        let b = rolling_baseline(vec![f64::NAN, 10.0, f64::INFINITY, 20.0]);
        assert_eq!(b.copied(), Some(15.0));
    }

    #[test]
    fn a_zero_baseline_is_not_a_usable_comparison() {
        let (o, d) = judge(
            Measurement::Measured(50.0),
            Measurement::Measured(0.0),
            4.0,
            Sense::LowerIsBetter,
        );
        assert_eq!(
            o,
            Outcome::Skipped,
            "dividing by a zero baseline is not a drift measurement"
        );
        assert_eq!(d.copied(), None);
    }

    #[test]
    fn tokens_are_the_published_vocabulary() {
        assert_eq!(Outcome::Pass.token(), "pass");
        assert_eq!(Outcome::Fail.token(), "fail");
        assert_eq!(Outcome::Seed.token(), "seed");
        assert_eq!(Outcome::Skipped.token(), "skip");
    }

    // Every token this engine PUBLISHES must be one it can read back, or the baseline filter that
    // reads outcomes off disk would treat a real verdict as unrecognised. `Skipped` is the one that
    // catches a serde-derived round trip: it publishes as "skip", not "skipped".
    #[test]
    fn every_published_token_reads_back_as_the_outcome_that_wrote_it() {
        for o in [
            Outcome::Pass,
            Outcome::Fail,
            Outcome::Seed,
            Outcome::Skipped,
        ] {
            assert_eq!(
                Outcome::from_token(o.token()),
                Some(o),
                "{} must round trip through its published token",
                o.token()
            );
        }
        assert_eq!(
            Outcome::from_token("skipped"),
            None,
            "a token this build does not publish must not be guessed at"
        );
        assert_eq!(Outcome::from_token(""), None);
    }
}
