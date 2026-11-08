use super::*;
use crate::measurement::Absent;

// The two field cases the old verdict split on. Both now yield their fraction and neither is
// withheld: 0.954 and 0.482 are the same evidence the 0.9 threshold was reading, minus the
// decision it made.
#[test]
fn the_fraction_is_reported_for_both_sides_of_the_old_threshold() {
    let near = headroom(334_838.0, &Measurement::Measured(351_088.0)).unwrap();
    assert!((near - 0.9537).abs() < 0.001, "{near}");
    let far = headroom(169_125.0, &Measurement::Measured(351_088.0)).unwrap();
    assert!((far - 0.4817).abs() < 0.001, "{far}");
}

// THE REGRESSION THIS MODULE EXISTS TO PREVENT. agentgateway's 12275 frames/sec against a 12360
// reference is 99.3% - the best outcome a proxy of a paced mock can have - and it published
// nothing, because 0.993 >= 0.9. There is no input to this function that suppresses anything, so
// the only assertion available is that a near-ceiling observation still yields its fact.
#[test]
fn keeping_pace_with_the_mock_reports_its_ratio_rather_than_vanishing() {
    let h = headroom(12_275.0, &Measurement::Measured(12_360.0)).unwrap();
    assert!(h > 0.99 && h < 1.0, "{h}");
}

// An unusable reference costs the HEADROOM, never the measurement. The caller has the value in
// hand and publishes it either way; this only says there is no fraction to state.
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

// Over the reference is REPORTED, not clamped and not rejected. These are the 2026-07-28 run's
// own numbers, every one of which the old code read as the most confident possible "rig-bound"
// (because `observed >= 0.9 * reference` is trivially true when observed is several times
// reference) and suppressed on that basis.
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

// A non-finite observation has no honest fraction. It is also an engine fault, but that is
// `engine_fault`'s finding on the counts - here it simply produces no number rather than a NaN
// that would serialize into the artifact.
#[test]
fn a_non_finite_observation_yields_no_fraction() {
    for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(headroom(v, &Measurement::Measured(100.0)), None);
    }
}
