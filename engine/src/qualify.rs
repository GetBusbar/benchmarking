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
    /// or the throughput qualification can never switch on for it. A `fail` never
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
    /* PRESENT IS NOT THE SAME AS USABLE. This screened `observed` for presence only, so a
    `Measured(NaN)` - a value this crate constructs directly - passed straight through, reached
    `drift_pct`, tripped its `!observed.is_finite()` arm, and came back `None`. `judge` then
    reported the ONE thing that branch is allowed to mean: "baseline is not a usable number",
    blaming a baseline that was perfectly fine for a broken observation.

    And it did so as `Skipped`, which means the gate does not fire. A qualification check that
    silently declines to run on a NaN observation is worse than a wrong reason: the box passes.
    A non-finite observation is exactly "the stage produced no usable observation", so it takes
    that branch, with that outcome. */
    /* AND POSITIVE. A box that measured 0 (or less) - no mock reachable, a direct leg that completed
    nothing - is not a box with a slow baseline, it is a box that did not measure. With no history
    it SEEDED, so a dead box was recorded as qualified and its 0 entered the rolling baseline, where
    `drift_pct`'s zero-baseline arm then skipped the gate on that gateway for good. Every observation
    this judges (throughput, or a latency floor) is strictly positive when it exists at all. */
    let Some(&obs) = observed.value().filter(|v| v.is_finite() && **v > 0.0) else {
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
        // Now unambiguous: `obs` is finite by construction above, so the only way `drift_pct`
        // declines is an unusable BASELINE, which is what this says.
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
    // Non-positive entries are dropped for the same reason `judge` refuses them: a 0 in history is a
    // run that measured nothing, and a median that lands on it disables the gate.
    candidates.retain(|v| v.is_finite() && *v > 0.0);
    if candidates.is_empty() {
        return Measurement::absent_because(Absent::NotMeasured, "no qualifying history");
    }
    crate::stats::median(&candidates)
}

#[cfg(test)]
#[path = "tests/qualify_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/qualify_nonfinite_observation_tests.rs"]
mod nonfinite_observation_tests;
