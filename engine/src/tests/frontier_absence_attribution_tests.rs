use super::*;

fn tailless(concurrency: u32, rps: f64) -> Rung {
    Rung {
        concurrency,
        rps,
        p99_us: None,
        ok: 10_000,
        fail: 0,
    }
}
fn with_tail(concurrency: u32, rps: f64, p99_ms: u64) -> Rung {
    Rung {
        concurrency,
        rps,
        p99_us: Some(p99_ms * 1_000),
        ok: 10_000,
        fail: 0,
    }
}

/* A LATENCY CLAIM MUST COME FROM OBSERVED LATENCY. A clean rung with no percentile fails
`qualifies` exactly as a slow one does, so both landed in the same absence - and the sentence
it published, "every cleanly-served rung had a tail latency at or above 1ms", is a statement
about a tail nobody measured. On a board whose premise is that every number re-derives from
its evidence, inventing the evidence is the worst available failure. */
#[test]
fn a_sweep_with_no_percentile_at_all_is_not_reported_as_being_over_the_bound() {
    let rungs = vec![tailless(32, 5_000.0), tailless(64, 9_000.0)];
    let a = absence_for(&rungs, Some(1_000));
    assert_eq!(
        a.reason(),
        Some(&Absent::NotMeasured),
        "no rung reported a tail, so this is not-measured, never below-resolution"
    );
    let d = a.detail().unwrap_or_default();
    assert!(
        d.contains("never observed"),
        "the reason must say the latency was never observed, not that it was over the bound: {d}"
    );
}

/// And the real below-resolution case must still read as one, or the fix has simply moved the
/// lie to the other side.
#[test]
fn a_sweep_whose_tails_are_all_over_the_bound_is_still_below_resolution() {
    let rungs = vec![with_tail(32, 5_000.0, 40), with_tail(64, 9_000.0, 80)];
    let a = absence_for(&rungs, Some(1_000));
    assert_eq!(a.reason(), Some(&Absent::BelowResolution));
}

/// A mix must read as below-resolution too - some tails WERE observed and all were over - but
/// the sentence must not claim every clean rung had a tail, because one did not.
#[test]
fn a_mixed_sweep_speaks_only_for_the_rungs_that_reported_a_tail() {
    let rungs = vec![tailless(32, 5_000.0), with_tail(64, 9_000.0, 40)];
    let a = absence_for(&rungs, Some(1_000));
    assert_eq!(a.reason(), Some(&Absent::BelowResolution));
    let d = a.detail().unwrap_or_default();
    assert!(
        d.contains("that reported a tail"),
        "the sentence must be scoped to the rungs it can actually speak for: {d}"
    );
}
