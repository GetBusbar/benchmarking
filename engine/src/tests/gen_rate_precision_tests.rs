use super::*;

fn s(ok: u64, elapsed_s: f64) -> GenStats {
    GenStats {
        ok,
        elapsed_s,
        ..Default::default()
    }
}

// THE CASE THAT WAS PUBLISHED AS ZERO. plano hit it twice: one request completed in four seconds
// is 0.25 req/s, and `as u64` truncation published `0` - which does not say "slow", it says the
// gateway carried NOTHING. Both external auditors had to carry a written special case to avoid
// calling a correct engine wrong.
#[test]
fn a_sub_one_per_second_rate_is_not_published_as_zero() {
    let r = s(1, 4.0).rps();
    assert!(
        (r - 0.25).abs() < 1e-9,
        "one request in four seconds is 0.25/s, got {r}"
    );
    assert!(
        r > 0.0,
        "a completed request must never publish as a zero rate"
    );
    assert!((s(1, 2.0).rps() - 0.5).abs() < 1e-9);
    assert!((s(3, 4.0).rps() - 0.75).abs() < 1e-9);
}

// AND THE OTHER HALF: nothing at or above 1/s moves. The split exists precisely so this change
// cannot quietly restate a single number the board has already published - if it rounded instead
// of truncating, every rate on the board could shift by one.
#[test]
fn rates_at_or_above_one_per_second_are_unchanged_whole_numbers() {
    assert_eq!(s(100, 1.0).rps(), 100.0);
    assert_eq!(s(44_363, 1.0).rps(), 44_363.0);
    // 19.9/s must still publish 19, exactly as `as u64` did - not 20.
    assert_eq!(
        s(199, 10.0).rps(),
        19.0,
        "truncation, not rounding, above 1/s"
    );
    assert_eq!(
        s(1, 1.0).rps(),
        1.0,
        "exactly 1/s is the boundary and stays whole"
    );
}

// A rate is undefined without elapsed time, and an infinity cast to an integer SATURATES rather
// than wrapping - which would publish 9,223,372,036,854,775,807 req/s as a measurement.
#[test]
fn a_rate_with_no_elapsed_time_is_zero_not_an_infinity() {
    assert_eq!(s(10, 0.0).rps(), 0.0);
    assert_eq!(s(0, 0.0).rps(), 0.0);
}
