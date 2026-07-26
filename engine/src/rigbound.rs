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
pub fn is_rig_bound(observed: f64, reference: Measurement<f64>) -> Measurement<bool> {
    match reference.copied() {
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

}
