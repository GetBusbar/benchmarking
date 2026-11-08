// AN ORDER STATISTIC OVER A NON-FINITE SAMPLE IS REFUSED, NOT GUESSED.
//
// The comparator was `a.partial_cmp(b).unwrap_or(Ordering::Equal)`, under which NaN is equal to
// everything - not a total order, so the sort's output is an unspecified permutation of the whole
// slice. The consequence was not a misplaced NaN but an ARRIVAL-ORDER-DEPENDENT ANSWER, which is
// disqualifying on a board whose premise is that every number regenerates from committed JSON.
#[test]
fn a_non_finite_sample_is_refused_rather_than_sorted() {
    let with_nan = [10.0, 20.0, 30.0, f64::NAN, 50.0];
    match percentile(&with_nan, 0.99) {
        Measurement::Absent { reason, detail } => {
            assert!(
                matches!(reason, Absent::HarnessError),
                "a non-finite latency is the RIG malfunctioning, so it must be a HarnessError - \
                     NotMeasured would file the rig's fault under the gateway's results"
            );
            let detail = detail.unwrap_or_default();
            assert!(
                detail.contains("not finite"),
                "the refusal must say what was wrong with the input, got: {detail}"
            );
        }
        other => panic!("percentile published {other:?} over a slice containing NaN"),
    }
    assert!(
        matches!(median(&with_nan), Measurement::Absent { .. }),
        "median must refuse the same input percentile refuses; they share the broken comparator"
    );
    assert!(
        matches!(
            percentile(&[1.0, f64::INFINITY], 0.5),
            Measurement::Absent { .. }
        ),
        "an infinity is equally unorderable in practice and equally a rig fault"
    );

    // MIN AND MAX TOO, and they failed differently and far more quietly. `f64::min` and `f64::max`
    // IGNORE NaN - they return the other operand - so `reduce(f64::min)` silently DROPPED the bad
    // sample and reported a minimum over the remaining ones as though the window were clean. There
    // is no sort to corrupt and no disagreement for anyone to notice, which is why this outlived
    // the two order statistics that were fixed first. Every statistic over one window has to reach
    // the same verdict about whether that window is usable.
    assert!(
        matches!(min(&with_nan), Measurement::Absent { .. }),
        "min silently skipped the NaN and reported a minimum over the remaining samples"
    );
    assert!(
        matches!(max(&with_nan), Measurement::Absent { .. }),
        "max silently skipped the NaN and reported a maximum over the remaining samples"
    );
    assert_eq!(
        min(&[3.0, 1.0, 2.0]),
        Measurement::Measured(1.0),
        "the clean path must still answer"
    );
    assert_eq!(
        max(&[3.0, 1.0, 2.0]),
        Measurement::Measured(3.0),
        "the clean path must still answer"
    );
}

// THE PROPERTY THE BUG ACTUALLY VIOLATED, pinned directly: a permutation of one sample set must
// never change the answer. This is what makes the number a measurement rather than an artifact of
// arrival order, and it is the assertion that fails loudest if the refusal is ever removed - the
// old code returned 30 for one ordering and 20 for the other.
#[test]
fn an_order_statistic_never_depends_on_the_order_samples_arrived_in() {
    let a = [10.0, 20.0, 30.0, f64::NAN, 50.0];
    let b = [f64::NAN, 10.0, 20.0, 30.0, 50.0];
    assert_eq!(
        format!("{:?}", median(&a)),
        format!("{:?}", median(&b)),
        "the same five samples in a different order produced different medians"
    );

    // And the clean path keeps its ordering-independence, so the fix is not merely refusing
    // everything: a real window must still answer, and answer identically under permutation.
    let clean = [50.0, 10.0, 40.0, 20.0, 30.0];
    let rotated = [30.0, 50.0, 10.0, 40.0, 20.0];
    assert_eq!(median(&clean), Measurement::Measured(30.0));
    assert_eq!(median(&clean), median(&rotated));
    assert_eq!(percentile(&clean, 1.0), Measurement::Measured(50.0));
    assert_eq!(percentile(&clean, 1.0), percentile(&rotated, 1.0));
}

// A STEADY WINDOW STILL HAS A SLOPE, AND PUBLISHES IT.
//
// `Verdict::Steady` was a unit variant, and `metric.rs` answered it with
// `growth_rate_mib_per_min: Measured(0.0)` - substituting an exact zero for the rate this function
// had just fitted. So a window drifting a little under `trend_pct` (steady, correctly) published a
// growth rate of precisely 0.000, and the number a reader would use to spot a SLOW leak was a
// constant chosen by the threshold rather than anything measured. Reverting the variant to a unit
// and the caller to `Measured(0.0)` fails on the second assertion below.
#[test]
fn a_steady_verdict_carries_the_slope_it_fitted_rather_than_an_assumed_zero() {
    // Ten readings climbing gently: about 0.5% across the window, inside a 1% trend bar, so this is
    // genuinely Steady - and genuinely still moving.
    let s: Vec<super::Sample> = (0..10)
        .map(|i| super::Sample {
            t_s: i as f64 * 6.0,
            mib: 100.0 + i as f64 * 0.05,
        })
        .collect();
    let v = super::plateau_check(&s, f64::INFINITY, 1.0, 2.0);
    assert!(
        v.is_steady(),
        "a 0.5% drift inside a 1% bar is steady: {v:?}"
    );
    let super::Verdict::Steady {
        growth_rate_mib_per_min,
    } = &v
    else {
        unreachable!("just asserted steady")
    };
    let rate = growth_rate_mib_per_min
        .copied()
        .expect("ten readings over a minute fit a slope");
    assert!(
        rate > 0.0,
        "the window IS climbing at {rate} MiB/min - publishing 0.0 here would tell a reader the \
             memory was flat when this function measured that it was not"
    );
    // And it is the same slope `growth_rate` computes standalone: the verdict must not carry a
    // second, differently-derived rate.
    let direct = super::growth_rate(&s).copied().expect("a slope");
    assert!((rate - direct).abs() < 1e-9, "{rate} vs {direct}");
}

use super::*;
use proptest::prelude::*;

// Builds a series the way lib/plateau_test.sh's mkseries does: value v0 + d*i at t = 2*i, with an
// optional alternating +/-j jitter. Kept identical to the shell helper so every ported case below
// reproduces the exact numbers the shell test asserted.
fn mkseries_jitter(v0: f64, d: f64, n: usize, j: f64) -> Vec<Sample> {
    (0..n)
        .map(|i| {
            let w = if j > 0.0 {
                if i % 2 == 1 {
                    j
                } else {
                    -j
                }
            } else {
                0.0
            };
            Sample::new((i * 2) as f64, v0 + d * i as f64 + w)
        })
        .collect()
}
fn mkseries(v0: f64, d: f64, n: usize) -> Vec<Sample> {
    mkseries_jitter(v0, d, n, 0.0)
}
// A window big enough to keep an entire mkseries-built series, so `plateau_check` sees the whole
// thing, matching the shell tests which pass plateau_check an already-whole file.
const WHOLE: f64 = 9_999.0;

// ---- plateau_check: ported from plateau_test.sh -----------------------------------------------

#[test]
fn a_genuinely_flat_series_is_a_plateau() {
    let s = mkseries(100.0, 0.0, 30);
    assert!(matches!(
        plateau_check(&s, WHOLE, 1.0, 2.0),
        Verdict::Steady { .. }
    ));
}

#[test]
fn a_steadily_rising_series_is_not_a_plateau() {
    let s = mkseries(100.0, 1.0, 30);
    assert!(!matches!(
        plateau_check(&s, WHOLE, 1.0, 2.0),
        Verdict::Steady { .. }
    ));
}

// THE CASE THE TREND TEST EXISTS FOR: a leak levelling off. 30 samples, +0.05 MiB each, around 118
// MiB. Spread is ~1.2%, under a 2% range gate, so a spread-only test calls it settled. It is not:
// the second-half mean sits above the first-half mean by more than a 0.5% trend gate can allow.
#[test]
fn red_a_spread_only_test_wrongly_calls_the_asymptoting_leak_steady() {
    let s = mkseries(118.0, 0.05, 30);
    let values: Vec<f64> = s.iter().map(|p| p.mib).collect();
    let lo = min(&values).copied().unwrap_or(0.0);
    let hi = max(&values).copied().unwrap_or(0.0);
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let spread_pct = (hi - lo) / mean * 100.0;
    assert!(
        spread_pct < 2.0,
        "the RED case: spread alone looks settled ({spread_pct}%)"
    );
}

#[test]
fn green_the_trend_test_rejects_the_asymptoting_leak() {
    let s = mkseries(118.0, 0.05, 30);
    assert!(!matches!(
        plateau_check(&s, WHOLE, 0.5, 2.0),
        Verdict::Steady { .. }
    ));
}

#[test]
fn oscillation_around_a_flat_mean_is_not_a_plateau() {
    let s = mkseries_jitter(100.0, 0.0, 30, 5.0);
    assert!(!matches!(
        plateau_check(&s, WHOLE, 1.0, 2.0),
        Verdict::Steady { .. }
    ));
}

// The trend gate is two-sided (`drift.abs()`), so this passes only because the drift is small in
// magnitude - well inside the gate in EITHER direction - not because falling is exempt. The
// near-the-gate symmetry is pinned by the declining/climbing test further down.
#[test]
fn a_slight_downward_drift_within_the_range_gate_is_a_plateau() {
    let s = mkseries(100.0, -0.02, 30);
    assert!(matches!(
        plateau_check(&s, WHOLE, 1.0, 2.0),
        Verdict::Steady { .. }
    ));
}

#[test]
fn too_few_samples_is_undecidable_not_a_plateau() {
    assert!(matches!(
        plateau_check(&[], WHOLE, 1.0, 2.0),
        Verdict::Undecidable(Undecidable::TooFewReadings { .. })
    ));
}

#[test]
fn three_samples_is_still_too_few_to_judge() {
    let s = mkseries(100.0, 0.0, 3);
    assert!(matches!(
        plateau_check(&s, WHOLE, 1.0, 2.0),
        Verdict::Undecidable(Undecidable::TooFewReadings { .. })
    ));
}

#[test]
fn exactly_four_samples_is_enough_to_judge() {
    let s = mkseries(100.0, 0.0, 4);
    assert!(matches!(
        plateau_check(&s, WHOLE, 1.0, 2.0),
        Verdict::Steady { .. }
    ));
}

// Boundary: drift and spread comparisons are strict "<", so sitting EXACTLY on either threshold
// must fail, not pass. Two samples per half, chosen so drift lands at precisely 1% of the mean.
// A SAWTOOTH IS NOT A LEAK, AND THE VERDICT HAS TO SAY WHICH IT SAW.
//
// Both windows below fail the steadiness test, and before `Shape` existed both published as the
// same word - which the board renders as "NEVER SETTLES" in red beside a leak rate. One of them
// is a garbage collector doing its job and returning to the level it started at; the other never
// has a level. Branding the first as a leak is the accusation this distinction prevents.
//
// The bifrost cells on the 2026-07-29 board are the climbing shape, measured: 449 -> 537 -> 631
// -> 792 -> 867 MiB across the load window, still rising in the final fifth.
#[test]
fn a_sawtooth_and_a_climb_both_fail_to_settle_and_are_told_apart() {
    // A wave: swings 40 MiB peak to trough, ends where it began. Wide spread, no drift.
    let wave: Vec<Sample> = (0..40)
        .map(|i| Sample {
            t_s: i as f64,
            mib: 100.0 + if i % 2 == 0 { 20.0 } else { -20.0 },
        })
        .collect();
    match plateau_check(&wave, WHOLE, 1.0, 2.0) {
        Verdict::NotSteady { shape, .. } => assert_eq!(
            shape,
            Shape::Oscillating,
            "a window that returns to where it started is a wave, never a leak"
        ),
        other => panic!("a 40 MiB swing is not settled: {other:?}"),
    }

    // A climb: rises steadily and is still rising at the end.
    let climb: Vec<Sample> = (0..40)
        .map(|i| Sample {
            t_s: i as f64,
            mib: 100.0 + i as f64 * 5.0,
        })
        .collect();
    match plateau_check(&climb, WHOLE, 1.0, 2.0) {
        Verdict::NotSteady { shape, .. } => {
            assert_eq!(
                shape,
                Shape::Climbing,
                "unbounded growth is the real defect"
            )
        }
        other => panic!("a 200 MiB rise is not settled: {other:?}"),
    }

    // And the mirror image: still RELEASING when the window closed. Not steady, but the opposite
    // of a leak, and it must never be labelled as one.
    let falling: Vec<Sample> = (0..40)
        .map(|i| Sample {
            t_s: i as f64,
            mib: 300.0 - i as f64 * 5.0,
        })
        .collect();
    match plateau_check(&falling, WHOLE, 1.0, 2.0) {
        Verdict::NotSteady { shape, .. } => assert_eq!(
            shape,
            Shape::Falling,
            "a window still giving memory back is not a leak"
        ),
        other => panic!("a 200 MiB decline is not settled: {other:?}"),
    }
}

#[test]
fn drift_exactly_at_the_trend_threshold_is_not_steady() {
    // halves: {100, 100} then {101, 101}; mean1=100 mean2=101 mean=100.5, drift = 1/100.5*100 ~ 0.995%.
    // Pick values so drift is exactly 1.0% of the mean: mean=100, mean2-mean1=1.0 -> drift=1.0.
    let s = vec![
        Sample::new(0.0, 99.5),
        Sample::new(1.0, 99.5),
        Sample::new(2.0, 100.5),
        Sample::new(3.0, 100.5),
    ];
    // mean1=99.5, mean2=100.5, mean=100.0, drift=(100.5-99.5)/100.0*100=1.0 exactly.
    assert_eq!(
        plateau_check(&s, WHOLE, 1.0, 1_000.0),
        Verdict::NotSteady {
            growth_rate_mib_per_min: growth_rate(&s),
            // drift is +1.0, exactly the trend bar, so this is the boundary case for CLIMBING.
            shape: Shape::Climbing,
        }
    );
}

#[test]
fn spread_exactly_at_the_range_threshold_is_not_steady() {
    // mean = 100, hi - lo = 2.0 -> spread = 2.0% exactly.
    let s = vec![
        Sample::new(0.0, 99.0),
        Sample::new(1.0, 100.0),
        Sample::new(2.0, 100.0),
        Sample::new(3.0, 101.0),
    ];
    assert!(!matches!(
        plateau_check(&s, WHOLE, 1_000.0, 2.0),
        Verdict::Steady { .. }
    ));
}

// ---- growth_rate: ported from plateau_test.sh --------------------------------------------------

#[test]
fn growth_rate_is_mib_per_minute_from_the_fitted_slope() {
    // 0.5 MiB per 2s sample = 15 MiB/min.
    let s = mkseries(100.0, 0.5, 30);
    let v = growth_rate(&s).copied();
    assert!(matches!(v, Some(x) if (x - 15.0).abs() < 1e-6), "got {v:?}");
}

#[test]
fn a_flat_series_has_a_zero_growth_rate() {
    let s = mkseries(100.0, 0.0, 30);
    let v = growth_rate(&s).copied();
    assert!(matches!(v, Some(x) if x.abs() < 1e-9), "got {v:?}");
}

#[test]
fn an_unmeasurable_rate_is_absent_not_zero() {
    assert!(!growth_rate(&[]).is_measured());
}

#[test]
fn a_single_sample_yields_no_rate() {
    let s = mkseries(100.0, 0.0, 1);
    assert!(!growth_rate(&s).is_measured());
}

// A single wild endpoint must not set the reported rate; that is why this is a fit and not
// last-minus-first. Flat series with one +50 MiB spike appended at the end.
#[test]
fn one_wild_endpoint_does_not_dominate_the_fitted_rate() {
    let mut s = mkseries(100.0, 0.0, 29);
    s.push(Sample::new(58.0, 150.0));
    let v = growth_rate(&s).copied();
    assert!(
        matches!(v, Some(x) if x < 60.0),
        "endpoint dominated the fit: {v:?}"
    );
}

// ---- window: ported from plateau_test.sh ----------------------------------------------------

#[test]
fn the_window_keeps_only_the_trailing_seconds() {
    let s = mkseries(100.0, 0.0, 60); // 60 samples at 2s apart = 118s of history
    assert_eq!(window(&s, 60.0).len(), 31);
}

#[test]
fn a_window_longer_than_the_series_keeps_everything() {
    let s = mkseries(100.0, 0.0, 60);
    assert_eq!(window(&s, 9_999.0).len(), 60);
}

#[test]
fn an_empty_series_yields_an_empty_window() {
    assert_eq!(window(&[], 60.0).len(), 0);
}

// ---- the calibration relationship: the point of the port ---------------------------------------

fn linear_series(base: f64, rate_mib_per_min: f64, dt_s: f64, n: usize) -> Vec<Sample> {
    let per_sample = rate_mib_per_min / 60.0 * dt_s;
    (0..n)
        .map(|i| Sample::new(i as f64 * dt_s, base + per_sample * i as f64))
        .collect()
}

// A DECLINE MUST FAIL THE TREND TEST EXACTLY AS A CLIMB DOES. `drift` is signed; a window whose
// second half runs measurably below its first half is not "steady", it is declining, and
// Verdict::Steady's own doc says "not moving in any direction that matters". A comparison that
// only bounds positive drift would call every decline steady regardless of how fast it fell.
#[test]
fn a_declining_window_fails_the_trend_test_at_the_same_magnitude_as_a_climbing_one() {
    // Gentle on purpose, and NEAR THE GATE: the mean stays well clear of zero (so the non-positive
    // mean arm cannot answer for an unrelated reason), and the drift is just past the 1% trend gate
    // with a wide range gate (so only the trend test is in play). 2 MiB/min at 6 s spacing over the
    // 60 s window is ~99.8 -> ~97.8 MiB, a half-to-half drift of about -1.11%.
    let declining = linear_series(100.0, -2.0, 6.0, 12);
    let verdict = plateau_check(&declining, 60.0, 1.0, 1.0e9);
    assert!(
            matches!(verdict, Verdict::NotSteady { .. }),
            "a real decline bigger than the trend gate must not be published as settled, got {verdict:?}"
        );

    // The mirror image at the identical magnitude climbs instead, and must fail the same way -
    // proving the test is symmetric rather than just asserting decline is special-cased.
    let climbing = linear_series(100.0, 2.0, 6.0, 12);
    let climbing_verdict = plateau_check(&climbing, 60.0, 1.0, 1.0e9);
    assert!(matches!(climbing_verdict, Verdict::NotSteady { .. }));

    // And just inside the gate (about 0.8% either way) both directions are steady: the bar is the
    // same height on both sides, not merely "some decline fails".
    for rate in [-1.5, 1.5] {
        let v = plateau_check(&linear_series(100.0, rate, 6.0, 12), 60.0, 1.0, 1.0e9);
        assert!(v.is_steady(), "{rate} MiB/min is inside a 1% gate: {v:?}");
    }
}

// A RIG NaN OR INFINITY IN A MEMORY WINDOW IS OURS. It must come back Undecidable (published as a
// harness error), never as a NotSteady or Oscillating verdict about the gateway's memory - and a
// `-inf` used to be classified by the non-positive-mean arm before the non-finite check could fire.
#[test]
fn a_non_finite_sample_in_the_window_is_undecidable_not_a_verdict_about_the_gateway() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut s = linear_series(100.0, 0.0, 6.0, 12);
        s[7].mib = bad;
        let v = plateau_check(&s, 60.0, 1.0, 2.0);
        assert_eq!(
            v,
            Verdict::Undecidable(Undecidable::NonFiniteSample),
            "a {bad} sample must not be judged"
        );
    }
}

// The fitted rate refuses the same input rather than publishing a non-finite number.
#[test]
fn growth_rate_over_a_non_finite_sample_is_a_harness_error_not_nan() {
    let mut s = linear_series(100.0, 1.0, 6.0, 12);
    s[3].mib = f64::NAN;
    assert_eq!(growth_rate(&s).reason(), Some(&Absent::HarnessError));
}

// Largest rate (MiB/min) that still certifies Steady at this window, found by bisection with the
// range gate disabled (huge range_pct) so only the trend test is in play.
fn boundary_rate(window_s: f64, n: usize, trend_pct: f64, base: f64) -> f64 {
    let dt = window_s / (n as f64 - 1.0);
    let mut lo = 0.0_f64;
    let mut hi = 10_000.0_f64;
    for _ in 0..80 {
        let mid = (lo + hi) / 2.0;
        let series = linear_series(base, mid, dt, n);
        let steady = matches!(
            plateau_check(&series, window_s, trend_pct, 1.0e9),
            Verdict::Steady { .. }
        );
        if steady {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

// THE CALIBRATION ARGUMENT, pinned exactly: for a linear ramp, halving the window halves how much
// elapsed time the trend test can see, so it takes double the rate to produce the same measured
// drift. This is not a rounding artifact of one example; it holds for any base and threshold.
#[test]
fn halving_the_window_doubles_the_admissible_slope() {
    let base = 120.0;
    let n = 400; // dense sampling so the discrete boundary tracks the continuous one closely
    let b60 = boundary_rate(60.0, n, 1.0, base);
    let b30 = boundary_rate(30.0, n, 1.0, base);
    assert!(
        (b30 - 2.0 * b60).abs() / b60 < 0.01,
        "b60={b60} b30={b30}, expected b30 ~= 2*b60"
    );
    // matches the worked example in the calibration note: ~2.4 MiB/min at 60s vs ~4.8 at 30s on a 120 MiB base.
    assert!((b60 - 2.4).abs() < 0.05, "b60={b60}");
    assert!((b30 - 4.8).abs() < 0.1, "b30={b30}");
}

// ---- median / min / max / percentile ----------------------------------------------------------

#[test]
fn median_of_an_odd_count_is_the_middle_value() {
    assert_eq!(median(&[3.0, 1.0, 2.0]).copied(), Some(2.0));
}

#[test]
fn median_of_an_even_count_averages_the_two_middle_values() {
    assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]).copied(), Some(2.5));
}

#[test]
fn median_min_max_percentile_are_absent_on_empty_input() {
    assert!(!median(&[]).is_measured());
    assert!(!min(&[]).is_measured());
    assert!(!max(&[]).is_measured());
    assert!(!percentile(&[], 0.5).is_measured());
}

#[test]
fn min_and_max_of_a_series() {
    let v = [3.0, 1.0, 4.0, 1.0, 5.0];
    assert_eq!(min(&v).copied(), Some(1.0));
    assert_eq!(max(&v).copied(), Some(5.0));
}

#[test]
fn percentile_by_nearest_rank() {
    let v: Vec<f64> = (1..=10).map(|i| i as f64).collect(); // 1..10
    assert_eq!(percentile(&v, 0.0).copied(), Some(1.0));
    assert_eq!(percentile(&v, 0.5).copied(), Some(5.0)); // ceil(10*0.5)=5 -> index 4 -> value 5
    assert_eq!(percentile(&v, 0.99).copied(), Some(10.0));
    assert_eq!(percentile(&v, 1.0).copied(), Some(10.0)); // clamped to the last index
}

// ONE CONVENTION, OR THE SAME WORD MEANS TWO THINGS ON ONE BOARD.
//
// Ledger SRCH-04: `metric.rs` resolved a rank with ceil while `gen.rs`, `stats.rs` and
// `search.rs` used floor, and comments in three files claimed they agreed. Over the sample
// counts this rig actually chooses - 100 TTFT samples a leg - floor puts p99 at index 99 of
// 100, which is the MAXIMUM. This pins the ranks directly so a future edit cannot quietly
// reintroduce the split.
#[test]
fn one_nearest_rank_convention_and_a_p99_that_is_not_the_maximum() {
    // The whole-number cases are exactly where floor and ceil disagreed.
    assert_eq!(nearest_rank_index(100, 0.99), 98);
    assert_eq!(nearest_rank_index(100, 0.50), 49);
    assert_eq!(nearest_rank_index(10, 0.50), 4);
    assert!(
        nearest_rank_index(100, 0.99) < 99,
        "a p99 that lands on the last index is the maximum, not a percentile"
    );
    // Fractional n*p: both conventions always agreed here, and the answer must not move.
    assert_eq!(nearest_rank_index(5, 0.5), 2);
    assert_eq!(nearest_rank_index(7, 0.9), 6);
    // The ends stay in range: p=0 is the smallest sample, p=1 the largest, never an out-of-bounds
    // rank or a rank of zero.
    for n in 1..200usize {
        assert_eq!(nearest_rank_index(n, 0.0), 0);
        assert_eq!(nearest_rank_index(n, 1.0), n - 1);
        for p in [0.5, 0.9, 0.95, 0.99] {
            assert!(nearest_rank_index(n, p) < n);
        }
    }
}

// ---- properties ---------------------------------------------------------------------------------

proptest! {
    // A rise steep enough to matter is never certified steady, at any window size, sample count,
    // or threshold pair in a realistic range. "Steep enough to matter" is necessary and honest:
    // the gate is a threshold, not a leak detector, and plateau.sh documents the same limit (a
    // rate below the certifying boundary genuinely does pass, on purpose, at 1.5 MiB/min on a 120
    // MiB base with the real 1%/60s defaults). This proves the trend test actually fires for
    // anything past its own boundary, for any window/sample-count combination, not just the one
    // worked example above.
    #[test]
    fn rising_series_above_the_trend_boundary_is_never_steady(
        window_s in 10.0f64..300.0,
        n in 20usize..200,
        trend_pct in 0.1f64..5.0,
        range_pct in 0.1f64..5.0,
        margin in 3.0f64..20.0,
    ) {
        let base = 120.0;
        let dt = window_s / (n as f64 - 1.0);
        // Continuous-approximation boundary from the calibration note; go well past it (margin
        // 3x-20x) so discretization error at small n can never flip the assertion.
        let boundary = 1.2 * base * trend_pct / window_s;
        let rate = boundary * margin;
        let series = linear_series(base, rate, dt, n);
        let verdict = plateau_check(&series, window_s, trend_pct, range_pct);
        prop_assert!(!verdict.is_steady());
    }

    // Too few samples is always Undecidable, never a false NotSteady or Steady, regardless of what
    // the samples actually contain.
    #[test]
    fn fewer_than_four_samples_is_always_undecidable(
        n in 0usize..4,
        base in -100.0f64..1000.0,
        d in -50.0f64..50.0,
    ) {
        let s = mkseries(base, d, n);
        // Bound first: `prop_assert!` stringifies its expression as a FORMAT STRING, so a
        // `{ .. }` pattern inside the macro is parsed as a placeholder and fails to compile.
        let too_few = matches!(
            plateau_check(&s, 9_999.0, 1.0, 2.0),
            Verdict::Undecidable(Undecidable::TooFewReadings { .. })
        );
        prop_assert!(too_few);
    }

    // growth_rate on an exactly linear series recovers that slope: a least-squares fit of exactly
    // linear data is exact, not merely close, up to floating-point error.
    #[test]
    fn growth_rate_recovers_a_known_slope(
        rate in -500.0f64..500.0,
        dt in 0.1f64..10.0,
        n in 2usize..100,
    ) {
        let s = linear_series(120.0, rate, dt, n);
        match growth_rate(&s) {
            Measurement::Measured(v) => prop_assert!((v - rate).abs() < 1e-6 * rate.abs().max(1.0)),
            other => prop_assert!(false, "expected a measured rate, got {other:?}"),
        }
    }
}
