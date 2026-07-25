// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Box qualification: deciding whether a cloud box is fit to measure on before it measures anything.
//
// A contaminated box once ran a full 6x6 to completion and its result was published as a gateway
// regression. It was not one: the rig's own no-gateway floor on that box had drifted while the
// healthy boxes had not, and its peak throughput had collapsed. Two stages catch that. Stage 1
// compares the box's own gateway-free latency floor against a rolling baseline; stage 2 replays a
// known cell and compares throughput, which is the stronger signal of the two.

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
    /// THE BUG THIS FIXES. The shell reader accepted a snapshot only when `verdict == "pass"`,
    /// while the writer documented that a `seed` verdict means "accepted, and this run becomes the
    /// baseline". So a seed run was recorded and then permanently ignored by the very code meant to
    /// read it. For a NEW gateway that is unrecoverable: peak baselines are per gateway, so its
    /// first run seeds, is discarded, and every later run seeds again. Stage 2, the strong half of
    /// qualification, could never switch on for it, silently and with nothing logged.
    ///
    /// A `seed` is a measurement taken on a box nothing was wrong with. It qualifies. A `fail`
    /// never does, and a `skip` measured nothing to contribute.
    pub fn qualifies_as_baseline(&self) -> bool {
        matches!(self, Outcome::Pass | Outcome::Seed)
    }
}

/// Percentage drift of an observation from a baseline. Positive means the observation is higher.
fn drift_pct(observed: f64, baseline: f64) -> Option<f64> {
    if !baseline.is_finite() || baseline == 0.0 || !observed.is_finite() {
        return None;
    }
    Some((observed - baseline) / baseline * 100.0)
}

/// Judge an observation against a rolling baseline.
///
/// `baseline` absent means no history: the run seeds rather than passing, because "within band of
/// nothing" is not a measurement. That distinction is the whole reason `Seed` exists as its own
/// outcome rather than being folded into `Pass`.
pub fn judge(observed: Measurement<f64>, baseline: Measurement<f64>, band_pct: f64) -> (Outcome, Measurement<f64>) {
    let Some(&obs) = observed.value() else {
        return (Outcome::Skipped, Measurement::absent(Absent::NotMeasured));
    };
    let Some(&base) = baseline.value() else {
        return (Outcome::Seed, Measurement::absent_because(Absent::NotMeasured, "no baseline yet"));
    };
    match drift_pct(obs, base) {
        None => (Outcome::Skipped, Measurement::absent_because(Absent::NotMeasured, "baseline is not a usable number")),
        Some(d) => {
            let outcome = if d.abs() <= band_pct { Outcome::Pass } else { Outcome::Fail };
            (outcome, Measurement::Measured(d))
        }
    }
}

/// Rolling median of the baseline candidates. A single wild run must not own the baseline, which is
/// why this is a median and not the last value or a mean.
pub fn rolling_baseline(mut candidates: Vec<f64>) -> Measurement<f64> {
    candidates.retain(|v| v.is_finite());
    if candidates.is_empty() {
        return Measurement::absent_because(Absent::NotMeasured, "no qualifying history");
    }
    candidates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = candidates.len();
    let med = if n % 2 == 1 {
        candidates[n / 2]
    } else {
        (candidates[n / 2 - 1] + candidates[n / 2]) / 2.0
    };
    Measurement::Measured(med)
}

#[cfg(test)]
mod tests {
    use super::*;

    // THE OPEN BUG, as a test. A seed run is a measurement taken on a box nothing was wrong with,
    // and the writer always said it becomes the baseline. The reader disagreed, so stage 2 could
    // never switch on for a new gateway: seed, discard, seed again, forever.
    #[test]
    fn a_seed_run_qualifies_as_baseline_data() {
        assert!(Outcome::Seed.qualifies_as_baseline(), "a seed run must become the baseline it was recorded to be");
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
        let (outcome, drift) = judge(Measurement::Measured(77.5), Measurement::absent(Absent::NotMeasured), FLOOR_DRIFT_PCT);
        assert_eq!(outcome, Outcome::Seed);
        assert_eq!(drift.copied(), None, "there is no drift against a baseline that does not exist");
    }

    // Nothing observed is a statement about the stage, not about the box.
    #[test]
    fn nothing_observed_is_skipped_not_failed() {
        let (outcome, drift) = judge(Measurement::absent(Absent::NotMeasured), Measurement::Measured(77.5), FLOOR_DRIFT_PCT);
        assert_eq!(outcome, Outcome::Skipped);
        assert_eq!(drift.copied(), None);
    }

    #[test]
    fn drift_inside_the_band_passes_and_outside_fails() {
        let base = Measurement::Measured(100.0);
        let (o, d) = judge(Measurement::Measured(103.0), base.clone(), 4.0);
        assert_eq!(o, Outcome::Pass);
        assert!((d.copied().unwrap_or_default() - 3.0).abs() < 1e-9);
        let (o, _) = judge(Measurement::Measured(105.0), base.clone(), 4.0);
        assert_eq!(o, Outcome::Fail);
        // Drift is judged on magnitude: a floor that improved impossibly is as suspect as one that
        // degraded, because both mean the box is not the box the baseline was taken on.
        let (o, _) = judge(Measurement::Measured(95.0), base, 4.0);
        assert_eq!(o, Outcome::Fail);
    }

    #[test]
    fn exactly_at_the_band_edge_passes() {
        let (o, _) = judge(Measurement::Measured(104.0), Measurement::Measured(100.0), 4.0);
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
        let (o, d) = judge(Measurement::Measured(50.0), Measurement::Measured(0.0), 4.0);
        assert_eq!(o, Outcome::Skipped, "dividing by a zero baseline is not a drift measurement");
        assert_eq!(d.copied(), None);
    }

    #[test]
    fn tokens_are_the_published_vocabulary() {
        assert_eq!(Outcome::Pass.token(), "pass");
        assert_eq!(Outcome::Fail.token(), "fail");
        assert_eq!(Outcome::Seed.token(), "seed");
        assert_eq!(Outcome::Skipped.token(), "skip");
    }
}
