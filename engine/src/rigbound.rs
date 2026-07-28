// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Was the number bounded by the GATEWAY, or by our own test rig?
//
// A throughput figure that saturated near the rig's own ceiling says nothing about the gateway, and
// publishing it as if it did would rank the rig. Worse, several fast gateways all pinned near the
// same rig ceiling would land on near-identical numbers and read as a tie they did not earn.
//
// THE COMPARISON MUST BE FAIR, which is the part that is easy to get wrong. Comparing a winner
// measured at c=740 against a rig reference measured at c=2048 compares two different operating
// points and systematically understates how close the gateway came - the rig is not equally fast at
// every concurrency. So `suite.rs::rig_ceiling` takes the rig reference as a SINGLE POINT measurement
// AT THE WINNER'S OWN CONCURRENCY (`run::measure_at`, not a search over a range), every time, from
// scratch: the one measurement `is_rig_bound` judges against is already taken at the winner's own
// operating point, so there is no separate top-of-range reference to re-probe or drag into a regime
// it was never characterised in.

use crate::measurement::{Absent, Measurement};

/// Fraction of the rig ceiling at or above which a measurement is considered rig-bound rather than
/// gateway-bound. Matches the shell's `c >= 0.9 * m`.
pub const BOUND_FRACTION: f64 = 0.9;

/// Judge a measured value against the rig's reference ceiling AT THE SAME OPERATING POINT.
///
/// Absent when the reference is unusable: an unmeasurable rig ceiling means we cannot say whether
/// the gateway was bounded by it, and guessing `false` there would quietly certify a number the rig
/// may well have produced. That is why this returns a `Measurement<bool>` rather than a bare bool.
/// How far above the reference a measurement may sit before the REFERENCE is the thing in doubt.
///
/// The rig drives the gateway, and the gateway forwards to the same mock the reference is taken
/// from, so the gateway cannot outrun it by more than measurement noise. A little over is ordinary
/// window-to-window scatter between two separately-timed legs. Several times over is not a fast
/// gateway, it is a reference that did not measure what it claims to.
pub const IMPOSSIBLE_FACTOR: f64 = 1.5;

/// Judge a measured value against the rig's reference ceiling AT THE SAME OPERATING POINT.
///
/// Absent when the reference is unusable: an unmeasurable rig ceiling means we cannot say whether
/// the gateway was bounded by it, and guessing `false` there would quietly certify a number the rig
/// may well have produced. That is why this returns a `Measurement<bool>` rather than a bare bool.
///
/// A reference the observation OVERSHOOTS is equally unusable, and used to read as the strongest
/// possible "rig-bound" because `observed >= 0.9 * reference` is trivially true when observed is
/// several times reference. That is backwards. The gateway forwards to the very mock the reference
/// measures, so it cannot beat it; an observation far above the reference is evidence about the
/// REFERENCE, not about the gateway. The 2026-07-28 field run recorded exactly this and suppressed
/// the cells as rig-limited: 192671 frames/sec against a "ceiling" of 24854 at c=512, 608 against 49
/// at c=1, 673 against 392 at c=8. Calling those unknown keeps a broken reference from masquerading
/// as a confident verdict in either direction.
pub fn is_rig_bound(observed: f64, reference: Measurement<f64>) -> Measurement<bool> {
    match reference.copied() {
        Some(r) if r > 0.0 && observed > r * IMPOSSIBLE_FACTOR => Measurement::absent_because(
            Absent::NotMeasured,
            format!(
                "the observation ({observed:.0}) is more than {IMPOSSIBLE_FACTOR}x the rig reference \
                 ({r:.0}) taken at the same operating point, which cannot happen when the gateway \
                 forwards to that same mock - the reference did not measure what it claims to, so \
                 whether this was rig-bound is unknown"
            ),
        ),
        Some(r) if r > 0.0 => Measurement::Measured(observed >= BOUND_FRACTION * r),
        _ => Measurement::absent_because(
            Absent::NotMeasured,
            "the rig reference ceiling was not measurable, so gateway-bound cannot be distinguished from rig-bound",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_or_above_nine_tenths_of_the_ceiling_is_rig_bound() {
        assert_eq!(is_rig_bound(90.0, Measurement::Measured(100.0)).copied(), Some(true));
        assert_eq!(is_rig_bound(95.4, Measurement::Measured(100.0)).copied(), Some(true));
        // The field case: 334838 fps against a 351088 fps rig ceiling is 95.4% and rig-bound.
        assert_eq!(is_rig_bound(334_838.0, Measurement::Measured(351_088.0)).copied(), Some(true));
    }

    #[test]
    fn comfortably_below_the_ceiling_is_the_gateway_s_own_number() {
        assert_eq!(is_rig_bound(50.0, Measurement::Measured(100.0)).copied(), Some(false));
        // The other field case: 169125 fps against the same rig ceiling is 48%, a real measurement.
        assert_eq!(is_rig_bound(169_125.0, Measurement::Measured(351_088.0)).copied(), Some(false));
    }

    #[test]
    fn exactly_at_the_fraction_counts_as_bound() {
        assert_eq!(is_rig_bound(90.0, Measurement::Measured(100.0)).copied(), Some(true));
    }

    // An unmeasurable ceiling must not silently certify the gateway's number. Answering false here
    // would publish "this is the gateway's own throughput" on no evidence at all.
    #[test]
    fn an_unusable_reference_yields_absent_never_false() {
        for r in [
            Measurement::absent(Absent::NotMeasured),
            Measurement::Measured(0.0),
            Measurement::Measured(-1.0),
        ] {
            let v = is_rig_bound(100.0, r);
            assert_eq!(v.copied(), None);
            assert_eq!(v.reason(), Some(&Absent::NotMeasured));
        }
    }


    // A REFERENCE THE OBSERVATION OVERSHOOTS IS EVIDENCE ABOUT THE REFERENCE.
    //
    // The gateway forwards to the same mock the reference is measured from, at the same operating
    // point, so it cannot beat it. Before this, `observed >= 0.9 * reference` was trivially true
    // whenever observed was several times reference, so the most impossible readings produced the
    // most confident "rig-bound" verdict and the cell was suppressed.
    //
    // These are the 2026-07-28 field run's own numbers.
    #[test]
    fn an_observation_that_beats_the_reference_makes_the_reference_unusable_not_the_verdict_certain() {
        for (observed, reference) in [(192_671.0, 24_854.0), (608.0, 49.0), (673.0, 392.0), (272.0, 98.0)] {
            let v = is_rig_bound(observed, Measurement::Measured(reference));
            assert_eq!(
                v.copied(), None,
                "{observed:.0} against a {reference:.0} ceiling is impossible for a proxy of that \
                 same mock, so it cannot be reported as a confident verdict either way"
            );
            assert!(v.detail().is_some_and(|d| d.contains("reference did not measure what it claims to")));
        }
    }

    // Ordinary scatter between two separately-timed legs must still resolve, or every fast gateway
    // sitting right at the ceiling would go unknown and the guard would stop meaning anything.
    #[test]
    fn a_little_over_the_reference_is_still_a_verdict_not_an_unknown() {
        // Just above the ceiling: rig-bound, which is the honest reading of a gateway pinned there.
        assert_eq!(is_rig_bound(105.0, Measurement::Measured(100.0)).copied(), Some(true));
        assert_eq!(is_rig_bound(149.0, Measurement::Measured(100.0)).copied(), Some(true));
        // The field cases this guard was built on still classify exactly as before.
        assert_eq!(is_rig_bound(334_838.0, Measurement::Measured(351_088.0)).copied(), Some(true));
        assert_eq!(is_rig_bound(169_125.0, Measurement::Measured(351_088.0)).copied(), Some(false));
    }
}
