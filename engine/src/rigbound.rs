// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// How close a measurement came to the rig's own ceiling. A fact we publish, not a verdict we reach.
//
// This used to suppress any measurement at or above `BOUND_FRACTION = 0.9` of the reference, replacing
// it with `null`/`rig_limited` on the theory that publishing it would rank the rig. That discarded
// correct measurements over an unrelated fact (what the number meant), and the 0.9/1.5 thresholds
// were arbitrary - separating cases only by incidental reference-window timing, not by anything about
// the gateway. Now the cell publishes its measured value plus `headroom`, the fraction of the ceiling
// it reached, and lets the reader draw the conclusion instead of the engine deleting the number.
//
// An observation that exceeds what the mock can physically emit is a bug in this engine, not a rig
// limit or a fast gateway; that exact-bound check needs raw counts, not a ratio, so it lives in
// `run::StreamWindow::engine_fault` instead of here.
//
// The reference must be taken at the observation's own concurrency (the rig isn't equally fast at
// every concurrency) - see `suite::stream_rig_ceiling` and `suite::rig_ceiling`.

use crate::measurement::Measurement;

/// The fraction of the rig's ceiling this observation reached, or `None` when there is no usable
/// ceiling to state it against (reference absent, non-positive, or non-finite).
///
/// `None` never suppresses the measured value itself - only the headroom figure beside it. Values
/// above 1.0 are returned as-is: separately-timed legs scatter, and a gateway may legitimately carry
/// more events/sec than the mock's own layout implies, so a ratio > 1 is ordinary, not an error (an
/// actually-impossible count is caught on the raw counts by `run::StreamWindow::engine_fault`).
pub fn headroom(observed: f64, reference: &Measurement<f64>) -> Option<f64> {
    match reference.copied() {
        Some(r) if r > 0.0 && r.is_finite() && observed.is_finite() => Some(observed / r),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::Absent;

    // Both sides of the old 0.9 threshold now yield their fraction; neither is withheld.
    #[test]
    fn the_fraction_is_reported_for_both_sides_of_the_old_threshold() {
        let near = headroom(334_838.0, &Measurement::Measured(351_088.0)).unwrap();
        assert!((near - 0.9537).abs() < 0.001, "{near}");
        let far = headroom(169_125.0, &Measurement::Measured(351_088.0)).unwrap();
        assert!((far - 0.4817).abs() < 0.001, "{far}");
    }

    // Regression guard: a near-ceiling observation (99.3%, which old code suppressed at >= 0.9) must
    // still yield its fact.
    #[test]
    fn keeping_pace_with_the_mock_reports_its_ratio_rather_than_vanishing() {
        let h = headroom(12_275.0, &Measurement::Measured(12_360.0)).unwrap();
        assert!(h > 0.99 && h < 1.0, "{h}");
    }

    // An unusable reference costs only the headroom figure, never the measurement itself.
    #[test]
    fn an_unusable_reference_yields_no_fraction_and_nothing_else() {
        for r in [
            Measurement::absent(Absent::NotMeasured),
            Measurement::Measured(0.0),
            Measurement::Measured(-1.0),
            Measurement::Measured(f64::NAN),
            Measurement::Measured(f64::INFINITY),
        ] {
            assert_eq!(headroom(100.0, &r), None, "{r:?}");
        }
    }

    // Observations over the reference are reported, not clamped or rejected.
    #[test]
    fn an_observation_over_the_reference_reports_its_ratio() {
        for (observed, reference, want) in [
            (192_671.0, 24_854.0, 7.75),
            (608.0, 49.0, 12.4),
            (673.0, 392.0, 1.72),
            (272.0, 98.0, 2.78),
        ] {
            let h = headroom(observed, &Measurement::Measured(reference)).unwrap();
            assert!(
                (h - want).abs() < 0.01,
                "{observed} over {reference} is {h}, not {want}"
            );
        }
    }

    // A non-finite observation yields no fraction rather than a NaN that would serialize into the
    // artifact; `engine_fault` is what flags it as a fault, on the counts.
    #[test]
    fn a_non_finite_observation_yields_no_fraction() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(headroom(v, &Measurement::Measured(100.0)), None);
        }
    }
}
