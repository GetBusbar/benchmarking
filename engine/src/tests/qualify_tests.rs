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
