// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The latency-throughput frontier: how much a gateway carries at each tail latency you're willing
// to accept. One measurement (a concurrency sweep), read several ways, with nothing chosen.
//
// This replaced two scalar metrics, `rps_max_proxy` and `rps_sustained_20ms`, both computed off the
// same sweep and each collapsing it to one number:
//   1. The sustained metric's ceiling (`SUSTAINED_P99_CEILING_US`) was an arbitrary chosen bound,
//      and every published surface described it as "p99 < 1s" while the engine enforced 20ms.
//   2. A scalar can't express a tradeoff: throughput and tail latency rise together with
//      concurrency, so "the throughput" is a point on a curve, not a property of a gateway.
//   3. The two scalars came from different search algorithms over the same rungs and could
//      disagree: `rps_max_proxy`'s plateau search sometimes quit before rungs the sustained
//      figure's bisection reached, so the "maximum" came out BELOW the sustained figure.
//
// So there is one measurement - the sweep - and published numbers are READINGS of it at declared
// bounds; nothing decides when to stop looking, since nothing searches for a shape.
//
// MONOTONICITY IS STRUCTURAL, NOT A CHECK. A rung qualifies at bound B if its p99 is under B and it
// failed no request. Relaxing B can only ADD rungs to that set, and the reading is the max over the
// set, so a max over a superset can't be smaller:
//
//     rps(1ms) <= rps(5ms) <= rps(10ms) <= rps(50ms) <= rps(100ms) <= rps(no bound)
//
// holds by construction for any input. `bench-audit.py` asserts it anyway, since an invariant
// nothing checks is one nobody notices breaking.
//
// ZERO FAILURES, NOT A TOLERANCE. The retired gate allowed a 0.1% fail ratio to absorb the rig
// running out of ephemeral ports under load. But the generator already separates that case
// (`GenStats::rig_refused` / `window_refusal` discard rig-side connection failures), so a failure
// that reaches here is the gateway failing a request it accepted - never acceptable at any
// concurrency, same standard as the stream gate's `STREAM_MIN_DELIVERY_RATIO = 1.0`.

use crate::measurement::{Absent, Measurement};

/// The tail-latency bounds the frontier is reported at, in microseconds.
///
/// Tick marks on an axis, not thresholds: no element decides whether anything is published, every
/// bound is reported side by side, and a reader ranks at whichever one matters to them. Adding or
/// removing a bound changes the RESOLUTION of the published curve, never which gateway comes out
/// ahead - unlike the scalar ceiling this replaced.
///
/// Placed from the data, not round numbers: across the 2026-07-29 board's 1632 sweep rungs, p99
/// separates between 1ms and 100ms and saturates above 500ms (96% of rungs already under 1s), so
/// these five bracket the mass. The full sweep is published alongside for any bound not ticked here.
pub const P99_BOUNDS_US: [u64; 5] = [1_000, 5_000, 10_000, 50_000, 100_000];

/// One rung of the sweep, as the frontier needs to see it.
///
/// Its own type rather than borrowing `search::ProbedPoint` or `run::SustainedPoint`: those carry a
/// `passed` flag baked in as one search's verdict under one gate. A `Rung` is an observation; which
/// bounds it qualifies for is computed, repeatedly, from the observation.
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
/// Carries its own evidence so the number is checkable rather than asserted: `concurrency` is
/// where the winning rate was observed, `p99_us` is the tail it came with, and
/// `first_disqualified_conc` is the proof it's a boundary and not just where the sweep stopped.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// `None` is the failure-only reading: no latency constraint, so the number answers "how much
    /// can it carry before it starts failing requests".
    pub p99_bound_us: Option<u64>,
    pub rps: f64,
    pub concurrency: u32,
    /// The tail the winning rung actually produced - NOT the bound. A gateway holding 4ms under a
    /// 100ms bound is not the same finding as one sitting at 99ms.
    pub p99_us: Option<u64>,
    /// The lowest concurrency above `concurrency` that did NOT qualify, when the sweep probed one.
    /// `None` means every rung above also qualified - a fact about the BOUND, not the range.
    pub first_disqualified_conc: Option<u32>,
    /// The highest concurrency this sweep probed at all, qualifying or not.
    ///
    /// Lets `is_lower_bound` tell "we ran out of ladder" from "throughput turned over", which
    /// `first_disqualified_conc` alone cannot - see that method.
    pub top_probed_conc: u32,
}

impl Reading {
    /// Is this the most the gateway can do under this bound, or only the most we ASKED for?
    ///
    /// True only when the winning rung is the highest concurrency probed - nothing above it in the
    /// record, so publishing the rate as a ceiling would be publishing our own range as the answer.
    /// The retired search turned this into `Absent::SearchExhausted`, discarding a real rate for
    /// failing to prove maximality; the rate is published instead, labelled.
    ///
    /// NOT `first_disqualified_conc.is_none()` (the old, wrong rule): that conflates "ran out of
    /// ladder" with "throughput turned over inside the range" - a curve can peak and then simply get
    /// slower at every rung still above without any of them disqualifying.
    pub fn is_lower_bound(&self) -> bool {
        self.concurrency >= self.top_probed_conc
    }
}

/// Read the sweep at one bound. `None` when no rung qualified.
pub fn read_at(rungs: &[Rung], bound: Option<u64>) -> Option<Reading> {
    // Winner is the highest RATE among qualifying rungs, not the highest concurrency: those differ
    // whenever throughput turns over before the tail breaks the bound.
    //
    // ON A TIE, THE LOWEST CONCURRENCY WINS. Used to be plain `max_by` on rate, which returns the
    // LAST maximum and so silently picked the HIGHEST concurrency on a tie - gomodel
    // openai-responses>openai-responses once published "107 rps at c=1024" when c=256 reached the
    // same rate, a 4x overstatement of the connections needed. Lowest-on-tie is both the more useful
    // answer and the conservative claim.
    //
    // Ordering by rate ascending, then concurrency DESCENDING, makes the max "highest rate, lowest
    // concurrency".
    let best = rungs.iter().filter(|r| r.qualifies(bound)).max_by(|a, b| {
        a.rps
            .total_cmp(&b.rps)
            .then(b.concurrency.cmp(&a.concurrency))
    })?;
    // Boundary proof: lowest concurrency ABOVE the winner that did not qualify, read from the rungs
    // so a sweep that never probed higher reports no boundary rather than an invented one.
    //
    // A concurrency is disqualified only if NONE of its windows qualified - `rungs` are per window
    // (`climb_rungs` emits several per concurrency), so taking the min over any single non-qualifying
    // window let one unlucky window disqualify a concurrency the gateway mostly held (36 of 456
    // published readings on the 2026-07-31 board did this). The boundary is the lowest concurrency
    // above the winner where the gateway failed to qualify in ANY window.
    let mut disqualified: Vec<u32> = rungs
        .iter()
        .filter(|r| r.concurrency > best.concurrency)
        .map(|r| r.concurrency)
        .filter(|c| {
            !rungs
                .iter()
                .any(|r| r.concurrency == *c && r.qualifies(bound))
        })
        .collect();
    disqualified.sort_unstable();
    let first_disqualified_conc = disqualified.first().copied();
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
/// Two genuinely different cases, previously published as one token: nothing served cleanly
/// ANYWHERE is the gateway's own answer about this cell; nothing served cleanly under THIS BOUND
/// while a looser bound did is a statement about the bound.
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
        Some(b) => {
            /* "No rung was under the bound" and "no rung had a tail at all" are opposite findings.
            `qualifies` is false for a clean rung whose `p99_us` is `None`, so a sweep with no
            percentile reported used to land here and publish `BelowResolution` - a latency claim
            manufactured from missing data. Same distinction as the no-clean-rung case above. */
            let clean_with_tail = rungs
                .iter()
                .filter(|r| r.served_cleanly() && r.p99_us.is_some())
                .count();
            if clean_with_tail == 0 {
                return Measurement::absent_because(
                    Absent::NotMeasured,
                    format!(
                        "rungs served cleanly but not one of them reported a tail percentile, so \
                         nothing can be said about the {}ms bound - the latency was never observed, \
                         which is not the same as being above it",
                        b / 1000
                    ),
                );
            }
            Measurement::absent_because(
                Absent::BelowResolution,
                format!(
                    "every cleanly-served rung that reported a tail had it at or above {}ms, so \
                     this gateway carried no measurable throughput under that bound",
                    b / 1000
                ),
            )
        }
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

    // Four qualifying rungs tied at 107 rps (c=256/512/1024/2048); `max_by` used to return the LAST
    // maximum, naming c=1024 - a 4x overstatement of the concurrency the rate needs.
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

    // A single scalar used to report only one of these four numbers; the reader couldn't tell which
    // tradeoff they were being sold.
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

    // Walks deliberately hostile inputs - out-of-order rungs, a curve that turns over, ties, a rung
    // with no p99 - since monotonicity must hold for ANY input, not just well-shaped ones.
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

    // One failed request disqualifies the rung at every bound (rig refusals never reach here - see
    // module note on `rig_refused`). Bounds here are strictly ABOVE the clean rung's 1ms tail since
    // `p99 < bound` is strict; the first version of this test used `Some(1_000)` and failed for that
    // reason instead of the one it tests.
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

    // Winning at the top of the ladder means we ran out of range. The retired search published
    // `Absent::SearchExhausted` for this, discarding a real rate; it's published and labelled instead.
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

    // But a curve that turned over inside the range is a real peak, even when every rung above the
    // winner still held the bound. Caught live: `is_lower_bound` used to be
    // `first_disqualified_conc.is_none()`, which conflates "ran out of ladder" with "turned over" -
    // a run that peaked at c=32 and probed on to c=256 (all still under bound, just slower) had
    // every reading wrongly claim to be a lower bound.
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

#[cfg(test)]
mod absence_attribution_tests {
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

    /* A latency claim must come from observed latency. A clean rung with no percentile fails
    `qualifies` exactly as a slow one does, so both used to land in the same absence and publish a
    latency claim about a tail nobody measured. */
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
}

#[cfg(test)]
mod boundary_proof_tests {
    use super::*;

    fn r(concurrency: u32, rps: f64, p99_ms: u64, fail: u64) -> Rung {
        Rung {
            concurrency,
            rps,
            p99_us: Some(p99_ms * 1_000),
            ok: 10_000,
            fail,
        }
    }

    /* One unlucky window must not disqualify its whole concurrency. Rungs are per-window, so this
    filter used to name a boundary at a concurrency the gateway had demonstrably held. */
    #[test]
    fn a_concurrency_that_qualified_in_any_window_is_not_the_boundary() {
        let rungs = vec![
            r(64, 900.0, 4, 0),
            r(256, 850.0, 4, 0),
            r(256, 850.0, 4, 0),
            r(256, 840.0, 4, 2), // one dirty window at a concurrency that otherwise held
            r(1024, 800.0, 40, 0), // the real boundary: over the 5ms bound in every window
        ];
        let reading = read_at(&rungs, Some(5_000)).expect("a reading must resolve");
        assert_eq!(
            reading.first_disqualified_conc,
            Some(1024),
            "c=256 qualified in 2 of 3 windows, so it is not where the gateway stopped"
        );
    }

    /// And a concurrency that failed in EVERY window is still reported, or the fix has blinded the
    /// proof entirely.
    #[test]
    fn a_concurrency_that_failed_every_window_is_still_the_boundary() {
        let rungs = vec![
            r(64, 900.0, 4, 0),
            r(256, 850.0, 40, 0),
            r(256, 850.0, 40, 0),
            r(256, 840.0, 40, 0),
        ];
        let reading = read_at(&rungs, Some(5_000)).expect("a reading must resolve");
        assert_eq!(reading.first_disqualified_conc, Some(256));
    }

    /// A sweep that never probed higher reports no boundary rather than an invented one.
    #[test]
    fn a_sweep_that_stopped_at_the_winner_names_no_boundary() {
        let rungs = vec![r(64, 900.0, 4, 0), r(64, 900.0, 4, 0)];
        let reading = read_at(&rungs, Some(5_000)).expect("a reading must resolve");
        assert_eq!(reading.first_disqualified_conc, None);
    }
}
