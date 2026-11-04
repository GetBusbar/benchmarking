use super::*;

/* A NaN OBSERVATION MUST FAIL THE GATE, NOT SKIP IT. `judge` screened `observed` for presence
only, so `Measured(NaN)` reached `drift_pct`, came back `None`, and was reported as
"baseline is not a usable number" with outcome `Skipped` - blaming a healthy baseline, and
letting the box qualify on a measurement that does not exist. */
#[test]
fn a_non_finite_observation_fails_and_does_not_blame_the_baseline() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let (outcome, m) = judge(
            Measurement::Measured(bad),
            Measurement::Measured(100.0),
            4.0,
            Sense::LowerIsBetter,
        );
        assert_eq!(
            outcome,
            Outcome::Fail,
            "{bad} must fail the gate, not skip it"
        );
        let d = m.detail().unwrap_or_default();
        assert!(
            d.contains("no usable observation"),
            "a broken observation must not be reported as a baseline problem: {d}"
        );
    }
}

/// And a genuinely unusable BASELINE must still read as one, or the fix has just swapped the lie.
#[test]
fn an_unusable_baseline_still_reads_as_a_baseline_problem() {
    let (outcome, m) = judge(
        Measurement::Measured(100.0),
        Measurement::Measured(0.0),
        4.0,
        Sense::LowerIsBetter,
    );
    assert_eq!(outcome, Outcome::Skipped);
    assert!(m
        .detail()
        .unwrap_or_default()
        .contains("baseline is not a usable number"));
}

/// A healthy pair must be unaffected.
#[test]
fn a_finite_observation_against_a_finite_baseline_still_judges_normally() {
    let (outcome, m) = judge(
        Measurement::Measured(101.0),
        Measurement::Measured(100.0),
        4.0,
        Sense::LowerIsBetter,
    );
    assert_eq!(outcome, Outcome::Pass);
    assert!(m.value().is_some(), "a judged pair must publish its drift");
}

/* A BOX THAT MEASURED NOTHING DOES NOT SEED. A finite 0 passed the old screen; with no history it
became `Seed` (which qualifies as a baseline), the dead box was recorded as qualified, and its 0
then sat in the rolling baseline where the zero-baseline arm skipped the gate for good. */
#[test]
fn a_zero_or_negative_observation_fails_rather_than_seeding_a_baseline() {
    for bad in [0.0, -1.0] {
        for baseline in [
            Measurement::absent(Absent::NotMeasured),
            Measurement::Measured(100.0),
        ] {
            let (outcome, _) = judge(
                Measurement::Measured(bad),
                baseline,
                PEAK_DRIFT_PCT,
                Sense::HigherIsBetter,
            );
            assert_eq!(outcome, Outcome::Fail, "an observation of {bad} must fail");
        }
    }
}

#[test]
fn a_zero_in_the_history_never_becomes_the_baseline() {
    assert_eq!(
        rolling_baseline(vec![0.0, 0.0, 100.0]).copied(),
        Some(100.0),
        "a run that measured nothing must not be able to move the median"
    );
    assert!(!rolling_baseline(vec![0.0, -5.0]).is_measured());
}
