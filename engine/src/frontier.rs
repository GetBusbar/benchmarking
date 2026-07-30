// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE LATENCY-THROUGHPUT FRONTIER: how much a gateway carries, at each tail latency you are willing
// to accept. One measurement, read several ways, with nothing chosen.
//
// WHY THIS REPLACED TWO SCALAR METRICS. The board used to publish `rps_max_proxy` ("peak throughput")
// and `rps_sustained_20ms` ("sustained throughput with p99 under a ceiling"). Both came off the SAME
// concurrency sweep - `metric.rs` says so: "THE SECOND QUESTION, OFF THE SAME RUNGS" - and each
// collapsed that sweep to one number. Three things were wrong with it, and all three are the same
// thing:
//
//   1. THE CEILING WAS CHOSEN. `SUSTAINED_P99_CEILING_US` decided which rungs counted, and no
//      argument picked it over any other value. Worse, every published surface described it as
//      "p99 < 1 s" while the engine enforced 20 ms - a bar 96% of all 1632 recorded rungs pass,
//      versus 57% for the real one. Readers reasoned about a test we did not run.
//   2. A SCALAR CANNOT EXPRESS A TRADEOFF. Throughput and tail latency rise together as concurrency
//      climbs, so "the throughput" is a point on a curve, not a property of a gateway. On the
//      2026-07-29 data agentgateway carried 23,630 rps with a 1 ms tail and gained only 7% by
//      dropping the bound entirely; apisix needed 5 ms to nearly double and 10 ms to reach 19k.
//      Published as one number those look comparable. They are not remotely the same machine, and
//      the difference was in the data the whole time.
//   3. TWO ALGORITHMS OVER ONE DATASET PRODUCED AN IMPOSSIBLE PAIR. `rps_max_proxy` came from a
//      plateau search that stopped when three consecutive rungs looked flat; the sustained figure
//      bisected the gate boundary over those same rungs. The bisection reached rungs the plateau
//      search had already quit before, so the "maximum" came out BELOW the sustained figure on
//      aisix openai-responses>anthropic (16,232 vs 16,610) and bifrost
//      openai-responses>openai-responses (5,113 vs 5,174). A maximum another reading of the same
//      windows beats is not a maximum.
//
// So there is one measurement - the sweep - and the published numbers are READINGS of it at declared
// bounds. Nothing decides when to stop looking, because nothing is searching for a shape: every rung
// probed is a rung considered.
//
// WHY MONOTONICITY IS STRUCTURAL AND NOT A CHECK. A rung qualifies at bound B if its p99 is under B
// and it failed no request. Relaxing B can only ADD rungs to that set, never remove one, and the
// reading is the maximum over the set - so a max over a superset cannot be smaller. Therefore
//
//     rps(1ms) <= rps(5ms) <= rps(10ms) <= rps(50ms) <= rps(100ms) <= rps(no bound)
//
// holds by construction, for any input, including nonsense input. The inversion class that produced
// the two field cases above is not "guarded against" here; it is unrepresentable. `bench-audit.py`
// asserts it anyway, because an invariant nothing checks is an invariant nobody notices breaking.
//
// WHY ZERO FAILURES RATHER THAN A TOLERANCE. The retired gate allowed `SUSTAINED_MAX_FAIL_RATIO =
// 0.001`, reasoned as not wanting one dropped connection in ten thousand to fail a whole rung. That
// reasoning was about OUR rig: at high concurrency the box runs out of ephemeral ports and the OS
// starts refusing connects. But the generator already separates that case - `GenStats::rig_refused`
// counts connections the RIG could not open, and `window_refusal` discards those windows - so a
// failure that reaches here is the gateway failing a request it accepted. There is no concurrency at
// which that is the gateway succeeding, which is the same standard the stream gate already holds
// (`STREAM_MIN_DELIVERY_RATIO = 1.0`, "a proxy that drops a frame has dropped a user's token"). A
// tolerance here would be one more chosen number, and this one has no rig artifact left to excuse it.

use crate::measurement::{Absent, Measurement};

/// The tail-latency bounds the frontier is reported at, in microseconds.
///
/// TICK MARKS ON AN AXIS, NOT THRESHOLDS. This is the one place where a set of numbers appears in the
/// measurement path without being derived, and the distinction that makes it acceptable is that no
/// element of this array decides whether anything is published: every bound is reported, side by side,
/// and a reader ranks at whichever one matters to them. Adding or removing a bound changes the
/// RESOLUTION of the published curve, never which gateway comes out ahead. Compare the constant this
/// replaced, where moving 20 ms to 1 s would have changed the ranking and the board would not have
/// said so.
///
/// PLACED FROM THE DATA, not from round numbers. Across the 1632 sweep rungs recorded on the
/// 2026-07-29 board the p99 distribution is min 0.13 ms, p25 2.0 ms, median 12.4 ms, p75 43.6 ms,
/// max 5980 ms. Rungs holding each bound: 1 ms 16%, 5 ms 37%, 10 ms 47%, 50 ms 78%, 100 ms 88%,
/// 500 ms 94%, 1 s 96%, 5 s 98%. So the population separates between 1 ms and 100 ms and is
/// saturated above 500 ms - a 1 s column would put 96% of rungs on the same side of it and
/// discriminate between almost nothing. These five bracket the mass.
///
/// The full sweep is published alongside, so a reader who wants a bound we did not tick can compute
/// it: every rung carries its own concurrency, rate, p99 and failure count.
pub const P99_BOUNDS_US: [u64; 5] = [1_000, 5_000, 10_000, 50_000, 100_000];

/// One rung of the sweep, as the frontier needs to see it.
///
/// Its own type rather than borrowing `search::ProbedPoint` or `run::SustainedPoint`: those carry a
/// `passed` flag that is one search's verdict under one gate, and the whole point here is that the
/// verdict is not baked into the rung. A rung is an observation; which bounds it qualifies for is
/// computed, repeatedly, from the observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rung {
    pub concurrency: u32,
    pub rps: f64,
    /// The tail latency this rung actually produced. `None` when no window behind this rung reported
    /// one, which disqualifies it from every LATENCY-bounded reading - a rung with no latency reading
    /// has not earned a claim about latency - but not from the failure-only reading, which makes no
    /// latency claim to earn.
    pub p99_us: Option<u64>,
    pub ok: u64,
    pub fail: u64,
}

impl Rung {
    /// Did the gateway serve every request it accepted at this rung? See the module note on why this
    /// is zero rather than a tolerance.
    fn served_cleanly(&self) -> bool {
        // A rung that completed nothing has not served cleanly - it has not served. An empty window
        // must never read as "no failures" by the accident that zero divided by nothing looks fine.
        self.ok > 0 && self.fail == 0
    }

    /// Does this rung qualify for a reading at `bound`? `None` means the failure-only reading.
    fn qualifies(&self, bound: Option<u64>) -> bool {
        if !self.served_cleanly() {
            return false;
        }
        match bound {
            None => true,
            Some(b) => self.p99_us.is_some_and(|p| p < b),
        }
    }
}

/// One published reading of the sweep: the most throughput carried while the tail stayed under
/// `p99_bound_us` and no accepted request failed.
///
/// It carries its own EVIDENCE, which is what makes the number checkable rather than asserted. The
/// old scalars published a value and a concurrency; a reader had no way to see whether the search had
/// actually established a boundary or merely stopped. Here `concurrency` is where the winning rate was
/// observed, `p99_us` is the tail it came with, and `first_disqualified_conc` is the lowest
/// concurrency ABOVE it that failed to qualify - so "this is the most it can do under this bound" is
/// a claim with both halves of its proof attached.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// The bound this reading was taken under. `None` is the failure-only reading: no latency
    /// constraint, so the number answers "how much can it carry before it starts failing requests".
    pub p99_bound_us: Option<u64>,
    pub rps: f64,
    pub concurrency: u32,
    /// The tail the winning rung actually produced - NOT the bound. Publishing the bound here would
    /// restate the question as though it were the answer, and the difference matters: a gateway
    /// holding 4 ms under a 100 ms bound is not the same finding as one sitting at 99 ms.
    pub p99_us: Option<u64>,
    /// The lowest concurrency above `concurrency` that did NOT qualify, when the sweep probed one.
    /// `None` means every rung above this one also qualified - which is a fact about the BOUND, not
    /// about the range: the gateway held this tail all the way up, and the reading is simply the best
    /// RATE among those rungs.
    pub first_disqualified_conc: Option<u32>,
    /// The highest concurrency this sweep probed at all, qualifying or not.
    ///
    /// Carried so `is_lower_bound` can tell "we ran out of ladder" from "throughput turned over",
    /// which `first_disqualified_conc` alone cannot - see that method.
    pub top_probed_conc: u32,
}

impl Reading {
    /// Is this the most the gateway can do under this bound, or only the most we ASKED for?
    ///
    /// True only when the winning rung is the highest concurrency the sweep probed. Then there is
    /// nothing above it in the record, we stopped because the range ended, and publishing the rate as a
    /// ceiling would be publishing our own range as the gateway's answer. The retired search turned
    /// this state into an ABSENCE (`Absent::SearchExhausted`, value null), throwing away a real
    /// measured rate for failing to prove maximality; the rate is right either way, so it is published
    /// and the word for it is published with it.
    ///
    /// NOT `first_disqualified_conc.is_none()`, which is what this was and which was wrong. That
    /// conflates two different findings, and the live smoke run showed the difference immediately: the
    /// sweep probed to c=256, the best rate landed at c=32, and every rung above it still held a 5ms
    /// tail while simply being SLOWER. Nothing disqualified, so the old test said "lower bound" - but
    /// throughput had visibly turned over inside the range and the peak was established. The honest
    /// question is whether we looked higher, not whether what we found up there passed a latency bound.
    pub fn is_lower_bound(&self) -> bool {
        self.concurrency >= self.top_probed_conc
    }
}

/// Read the sweep at one bound. `None` when no rung qualified.
pub fn read_at(rungs: &[Rung], bound: Option<u64>) -> Option<Reading> {
    // The winning rung is the one with the highest RATE among those that qualify - not the highest
    // concurrency. Those differ whenever a gateway's throughput turns over before its tail breaks the
    // bound, and the question is how much it carried, not how many connections it held.
    //
    // ON A TIE, THE LOWEST CONCURRENCY WINS, AND THIS HAS TO BE SAID RATHER THAN INHERITED.
    //
    // It used to be `max_by(|a, b| a.rps.total_cmp(&b.rps))` alone, and `max_by` returns the LAST
    // maximum, so a tie silently resolved to the HIGHEST concurrency - deciding on concurrency, at its
    // worst end, in the two lines directly under a comment promising it does not decide on concurrency
    // at all. gomodel openai-responses>openai-responses published "107 rps at c=1024" when c=256 reached
    // the same 107: a 4x overstatement of the connections the rate needs. Nothing was wrong with the
    // rate; the reading simply named the wrong rung as the place it happened.
    //
    // Publishing the lowest is both the more useful fact (a reader choosing an operating point wants the
    // cheapest concurrency that reaches the rate) and the conservative claim about the gateway. The real
    // point is that a tie-break AUTHORS A PUBLISHED NUMBER, so it must be a stated rule with a reason -
    // not a by-product of which end of a sequence a standard-library adapter happens to keep.
    //
    // Ordering by rate ascending, then concurrency DESCENDING, makes the maximum "highest rate, lowest
    // concurrency". Rungs identical in both are repeated windows at one concurrency, where either choice
    // reports the same pair.
    let best = rungs
        .iter()
        .filter(|r| r.qualifies(bound))
        .max_by(|a, b| a.rps.total_cmp(&b.rps).then(b.concurrency.cmp(&a.concurrency)))?;
    // The boundary proof: the lowest concurrency ABOVE the winner that did not qualify. Read from the
    // rungs rather than assumed, so a sweep that never probed higher reports no boundary instead of
    // an invented one.
    let first_disqualified_conc = rungs
        .iter()
        .filter(|r| r.concurrency > best.concurrency && !r.qualifies(bound))
        .map(|r| r.concurrency)
        .min();
    // The top of what we ASKED FOR, over every rung probed - qualifying or not, since a rung that failed
    // is still a rung we looked at and is exactly what proves we did not stop early.
    let top_probed_conc = rungs.iter().map(|r| r.concurrency).max().unwrap_or(0);
    Some(Reading {
        p99_bound_us: bound,
        rps: best.rps,
        concurrency: best.concurrency,
        p99_us: best.p99_us,
        first_disqualified_conc,
        top_probed_conc,
    })
}

/// The whole frontier: every declared bound, then the failure-only reading, in that order.
///
/// Bounds ascending and the unbounded reading last, so the published sequence reads as the tradeoff it
/// is and the monotonicity the module note derives is visible by eye in the artifact.
pub fn frontier(rungs: &[Rung]) -> Vec<Reading> {
    P99_BOUNDS_US
        .iter()
        .map(|b| Some(*b))
        .chain(std::iter::once(None))
        .filter_map(|b| read_at(rungs, b))
        .collect()
}

/// Why a bound yielded no reading, for the artifact's absence entry.
///
/// The two cases are genuinely different and the old code published one token for both. Nothing
/// served cleanly ANYWHERE is the gateway's own answer about this cell; nothing served cleanly under
/// THIS BOUND while a looser bound did is a statement about the bound, and a reader comparing columns
/// needs to know which they are looking at.
pub fn absence_for(rungs: &[Rung], bound: Option<u64>) -> Measurement<i64> {
    let any_clean = rungs.iter().any(|r| r.served_cleanly());
    if !any_clean {
        return Measurement::absent_because(
            Absent::NotMeasured,
            format!(
                "no concurrency in this sweep served every request it accepted, across {} rung(s) probed",
                rungs.len()
            ),
        );
    }
    match bound {
        None => Measurement::absent_because(
            Absent::HarnessError,
            "rungs served cleanly but the unbounded reading found none, which cannot happen - the \
             unbounded reading's qualifying set is every cleanly-served rung"
                .to_string(),
        ),
        Some(b) => Measurement::absent_because(
            Absent::BelowResolution,
            format!(
                "every cleanly-served rung had a tail latency at or above {}ms, so this gateway \
                 carried no measurable throughput under that bound",
                b / 1000
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(r.rps, 107.0, "the rate is the maximum, which the tie does not change");
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
}
