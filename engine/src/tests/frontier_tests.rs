use super::*;

fn rung(concurrency: u32, rps: f64, p99_ms: u64, fail: u64) -> Rung {
    Rung {
        concurrency,
        rps,
        p99_us: Some(p99_ms * 1_000),
        ok: 10_000,
        fail,
    }
}

// THE TIE-BREAK, from gomodel openai-responses>openai-responses on the 2026-07-30 board: four
// qualifying rungs all reached 107 rps, at c=256, 512, 1024 and 2048. The published reading named
// c=1024 - a 4x overstatement of the concurrency the rate needs - because `max_by` returns the LAST
// maximum and nothing here declared a rule, so the answer came from a library's iteration order.
//
// There was no test for a tie at all, which is exactly why the behaviour could be inherited rather
// than chosen. This is that test.
#[test]
fn a_tie_on_rate_reports_the_lowest_concurrency_that_reached_it() {
    // Deliberately NOT in ascending order, so a pass cannot come from the input happening to be
    // sorted the convenient way - which would make this test agree with the bug it exists to catch.
    let rungs = vec![
        rung(1024, 107.0, 5_800, 0),
        rung(256, 107.0, 4_900, 0),
        rung(2048, 107.0, 5_900, 0),
        rung(512, 107.0, 5_500, 0),
    ];
    let r = read_at(&rungs, None).expect("four clean rungs qualify with no bound");
    assert_eq!(
        r.rps, 107.0,
        "the rate is the maximum, which the tie does not change"
    );
    assert_eq!(
        r.concurrency, 256,
        "on a tie the LOWEST concurrency that reached the rate is published: naming a higher one \
             claims the rate needs more connections than it does"
    );
    // And the tie must not corrupt the boundary proof: the winner is now c=256, so the first
    // disqualified concurrency is read relative to THAT rung, not to whichever rung won before.
    assert_eq!(
        r.p99_us,
        Some(4_900_000),
        "the published tail belongs to the rung actually named, not to a sibling that tied on rate"
    );
    // Every rung above the winner qualified too, so there is no boundary to name, and the top of
    // what was probed is still the highest rung, not the winner.
    assert_eq!(r.first_disqualified_conc, None);
    assert_eq!(r.top_probed_conc, 2048);
}

// "UNDER THE BOUND" MEANS STRICTLY UNDER. Tails sit on round values (1, 5, 10 ms) often enough that
// `<=` would quietly flatter every gateway whose p99 lands exactly on a bound.
#[test]
fn a_tail_exactly_on_the_bound_does_not_qualify_for_it() {
    let on = Rung {
        concurrency: 16,
        rps: 100.0,
        p99_us: Some(1_000),
        ok: 10,
        fail: 0,
    };
    assert_eq!(read_at(&[on], Some(1_000)), None);
    let under = Rung {
        p99_us: Some(999),
        ..on
    };
    assert_eq!(
        read_at(&[under], Some(1_000)).map(|r| r.concurrency),
        Some(16)
    );
}

// A tie must not override the rate itself: a LOWER-rate rung at lower concurrency stays a loser.
#[test]
fn the_tie_break_never_outranks_a_higher_rate() {
    let rungs = vec![rung(16, 900.0, 1_000, 0), rung(256, 1_500.0, 2_000, 0)];
    let r = read_at(&rungs, None).expect("both rungs qualify");
    assert_eq!(r.rps, 1_500.0);
    assert_eq!(
        r.concurrency, 256,
        "concurrency only breaks a tie; it must never beat a genuinely higher rate"
    );
}

// THE FIELD CURVE, from apisix anthropic>anthropic on the 2026-07-29 board: 7,015 rps at a 1ms
// tail, 15,438 at 5ms, 18,943 at 10ms, 19,284 unbounded. A single scalar reported one of those
// four and the reader could not tell which tradeoff they were being sold.
#[test]
fn the_frontier_reports_the_tradeoff_a_scalar_collapsed() {
    let rungs = vec![
        rung(16, 7_015.0, 0, 0),
        rung(64, 15_438.0, 4, 0),
        rung(256, 18_943.0, 9, 0),
        rung(1024, 19_284.0, 40, 0),
    ];
    let f = frontier(&rungs);
    assert_eq!(f.len(), 6, "five bounds plus the unbounded reading");
    let at = |ms: u64| {
        f.iter()
            .find(|r| r.p99_bound_us == Some(ms * 1_000))
            .map(|r| r.rps)
    };
    assert_eq!(at(1), Some(7_015.0));
    assert_eq!(at(5), Some(15_438.0));
    assert_eq!(at(10), Some(18_943.0));
    assert_eq!(at(50), Some(19_284.0));
    assert_eq!(f.last().unwrap().p99_bound_us, None);
    assert_eq!(f.last().unwrap().rps, 19_284.0);
}

// MONOTONICITY IS STRUCTURAL. Relaxing the bound can only add rungs to the qualifying set, and the
// reading is a max over that set, so it cannot fall. This walks deliberately hostile inputs -
// out-of-order rungs, a curve that turns over, ties, a rung with no p99 - because the property is
// meant to hold for ANY input rather than for well-shaped ones.
#[test]
fn relaxing_the_bound_can_never_lower_the_reading() {
    let cases: Vec<Vec<Rung>> = vec![
        // Ordinary rising curve.
        vec![
            rung(1, 100.0, 0, 0),
            rung(8, 500.0, 3, 0),
            rung(64, 900.0, 30, 0),
        ],
        // Probed out of order, and throughput TURNS OVER before the tail breaks 100ms.
        vec![
            rung(64, 400.0, 60, 0),
            rung(8, 900.0, 20, 0),
            rung(1, 300.0, 1, 0),
        ],
        // Ties on rate at different tails.
        vec![rung(4, 500.0, 2, 0), rung(16, 500.0, 40, 0)],
        // A rung with no latency reading: legal for the unbounded reading, disqualified from
        // every bounded one.
        vec![
            rung(4, 200.0, 2, 0),
            Rung {
                concurrency: 32,
                rps: 5_000.0,
                p99_us: None,
                ok: 10_000,
                fail: 0,
            },
        ],
        // Every rung failing something.
        vec![rung(4, 900.0, 1, 1), rung(8, 950.0, 2, 7)],
    ];
    for (i, rungs) in cases.iter().enumerate() {
        let f = frontier(rungs);
        let rates: Vec<f64> = f.iter().map(|r| r.rps).collect();
        for w in rates.windows(2) {
            assert!(
                w[1] >= w[0],
                "case {i}: relaxing the bound lowered the reading: {rates:?}"
            );
        }
    }
}

// A rung with no p99 cannot satisfy a claim about latency, but it is a perfectly good answer to
// "how much before it fails". The pair above is the case: 5000 rps with no tail reading shows up
// only in the unbounded column.
#[test]
fn a_rung_with_no_tail_reading_answers_only_the_unbounded_question() {
    let rungs = vec![
        rung(4, 200.0, 2, 0),
        Rung {
            concurrency: 32,
            rps: 5_000.0,
            p99_us: None,
            ok: 10_000,
            fail: 0,
        },
    ];
    assert_eq!(read_at(&rungs, Some(100_000)).unwrap().rps, 200.0);
    assert_eq!(read_at(&rungs, None).unwrap().rps, 5_000.0);
}

// ONE FAILED REQUEST DISQUALIFIES THE RUNG, at every bound. The gateway accepted it and did not
// serve it; the rig's own refusals never reach here (see the module note on `rig_refused`).
//
// The bounds here are all strictly ABOVE the clean rung's own 1ms tail, because the comparison is
// `p99 < bound` and a rung sitting exactly ON a bound does not clear it. That is deliberate - "under
// 1 ms" has to mean under - and it is worth stating because the first version of this test used
// `Some(1_000)` against a 1ms rung and failed for that reason rather than for the one it tests.
#[test]
fn a_rung_that_failed_a_request_it_accepted_qualifies_for_nothing() {
    let rungs = vec![rung(8, 900.0, 1, 0), rung(64, 5_000.0, 2, 1)];
    for b in [Some(5_000), Some(100_000), None] {
        let r = read_at(&rungs, b).expect("the clean rung still reads");
        assert_eq!(r.rps, 900.0, "bound {b:?} must not take the failing rung");
    }
}

// A window that completed NOTHING is not a clean window. Zero failures out of zero requests must
// never read as success by the accident that the ratio looks fine.
#[test]
fn a_rung_that_completed_nothing_is_not_a_clean_rung() {
    let rungs = vec![Rung {
        concurrency: 8,
        rps: 0.0,
        p99_us: Some(1),
        ok: 0,
        fail: 0,
    }];
    assert_eq!(read_at(&rungs, None), None);
    assert_eq!(read_at(&rungs, Some(100_000)), None);
}

// THE BOUNDARY PROOF. The reading names the lowest concurrency above it that stopped qualifying,
// so "this is the most under this bound" arrives with both halves of its evidence.
#[test]
fn a_reading_carries_the_rung_that_disqualified_above_it() {
    let rungs = vec![
        rung(8, 500.0, 2, 0),
        rung(64, 900.0, 4, 0),
        rung(256, 1_200.0, 40, 0), // breaks a 5ms bound
        rung(1024, 1_300.0, 90, 0),
    ];
    let r = read_at(&rungs, Some(5_000)).unwrap();
    assert_eq!(r.rps, 900.0);
    assert_eq!(r.concurrency, 64);
    assert_eq!(
        r.first_disqualified_conc,
        Some(256),
        "the lowest rung above the winner that broke the bound is the proof it is the boundary"
    );
    assert!(!r.is_lower_bound());
}

// WINNING AT THE TOP OF THE LADDER MEANS WE RAN OUT OF RANGE. The retired search published
// `Absent::SearchExhausted` - null - for this, discarding a real measured rate because it could not
// prove maximality. The rate is published and labelled instead.
#[test]
fn a_sweep_that_won_at_its_top_rung_reads_as_a_lower_bound_not_an_absence() {
    let rungs = vec![rung(1024, 900.0, 2, 0), rung(16384, 19_000.0, 3, 0)];
    let r = read_at(&rungs, Some(10_000)).expect("a real rate, not an absence");
    assert_eq!(r.rps, 19_000.0);
    assert_eq!(r.concurrency, 16_384);
    assert!(
        r.is_lower_bound(),
        "the best rate is the highest rung probed, so nothing establishes it as a ceiling"
    );
}

// BUT A CURVE THAT TURNED OVER INSIDE THE RANGE IS A REAL PEAK, even when every rung above the
// winner still held the bound.
//
// THE LIVE SMOKE RUN CAUGHT THIS. `is_lower_bound` was `first_disqualified_conc.is_none()`, which
// conflates "we ran out of ladder" with "throughput turned over". The 2026-07-30 local run probed to
// c=256, peaked at c=32, and every rung above still held a 5ms tail while simply being slower -
// nothing disqualified, so five of the six readings claimed to be lower bounds when the peak was
// plainly established. The honest question is whether we LOOKED higher, not whether what we found
// up there passed a latency bound.
#[test]
fn a_curve_that_turned_over_inside_the_range_is_a_peak_not_a_lower_bound() {
    let rungs = vec![
        rung(8, 90_000.0, 1, 0),
        rung(32, 187_407.0, 1, 0),  // the peak
        rung(128, 150_000.0, 2, 0), // probed, still under the bound, and SLOWER
        rung(256, 120_000.0, 3, 0),
    ];
    let r = read_at(&rungs, Some(5_000)).unwrap();
    assert_eq!(r.rps, 187_407.0);
    assert_eq!(r.concurrency, 32);
    assert_eq!(
        r.first_disqualified_conc, None,
        "nothing above broke the bound - that is a fact about the BOUND, not about the range"
    );
    assert_eq!(r.top_probed_conc, 256);
    assert!(
        !r.is_lower_bound(),
        "we probed 3 rungs past the winner and they were worse: the peak is established"
    );
}

// The winner is the highest RATE, not the highest concurrency. These differ whenever throughput
// turns over before the tail breaks the bound, and reporting the top rung's rate would publish a
// number the gateway beat at a lower concurrency.
#[test]
fn the_reading_is_the_best_rate_not_the_deepest_concurrency() {
    let rungs = vec![
        rung(64, 9_000.0, 3, 0),
        rung(256, 12_000.0, 4, 0),
        rung(1024, 7_000.0, 4, 0), // deeper, and worse
    ];
    let r = read_at(&rungs, Some(5_000)).unwrap();
    assert_eq!(r.rps, 12_000.0);
    assert_eq!(r.concurrency, 256);
}

// The two absence reasons are different findings and must not share a token.
#[test]
fn nothing_clean_anywhere_reads_differently_from_nothing_clean_under_this_bound() {
    let dirty = vec![rung(8, 900.0, 1, 3)];
    let a = absence_for(&dirty, Some(1_000));
    assert_eq!(a.reason(), Some(&Absent::NotMeasured));
    assert!(a.detail().unwrap().contains("served every request"));

    let slow = vec![rung(8, 900.0, 60, 0)];
    let b = absence_for(&slow, Some(1_000));
    assert_eq!(b.reason(), Some(&Absent::BelowResolution));
    assert!(b.detail().unwrap().contains("1ms"), "{:?}", b.detail());
}

// The bounds must be ascending and distinct, or the published sequence stops reading as a curve
// and the monotonicity a reader checks by eye no longer lines up with the columns.
#[test]
fn the_declared_bounds_are_ascending_and_distinct() {
    for w in P99_BOUNDS_US.windows(2) {
        assert!(w[0] < w[1], "bounds must ascend: {P99_BOUNDS_US:?}");
    }
}
