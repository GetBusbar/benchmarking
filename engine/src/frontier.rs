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
    let best = rungs.iter().filter(|r| r.qualifies(bound)).max_by(|a, b| {
        a.rps
            .total_cmp(&b.rps)
            .then(b.concurrency.cmp(&a.concurrency))
    })?;
    // The boundary proof: the lowest concurrency ABOVE the winner that did not qualify. Read from the
    // rungs rather than assumed, so a sweep that never probed higher reports no boundary instead of
    // an invented one.
    /* A CONCURRENCY IS DISQUALIFIED ONLY IF NONE OF ITS WINDOWS QUALIFIED.
    `rungs` are PER WINDOW - `climb_rungs` emits WINDOWS_PER_RUNG of them at each concurrency, and
    every published sweep shows each concurrency three times. Taking the min over any single
    non-qualifying rung therefore let ONE unlucky window disqualify its whole concurrency: on the
    2026-07-31 board, 36 of 456 published readings named a boundary at a concurrency where the
    majority of windows had qualified (busbar openai-responses>bedrock and both aisix cells name
    c=1024, which qualified in 2 of its 3 windows).

    That number is the reading's boundary PROOF - the evidence for "this is where it stopped" -
    so naming a concurrency the gateway demonstrably held is exactly the claim it exists to
    support, inverted. The true boundary is the lowest concurrency above the winner at which the
    gateway did not qualify in ANY window. */
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
        Some(b) => {
            /* "NO RUNG WAS UNDER THE BOUND" AND "NO RUNG HAD A TAIL AT ALL" ARE OPPOSITE FINDINGS.
            `qualifies` is false for a clean rung whose `p99_us` is `None` (`is_some_and` on
            `None`), so a sweep whose windows reported no percentile lands here and used to
            publish `BelowResolution` - the sentence "every cleanly-served rung had a tail latency
            at or above 1ms" about a cell where NO tail latency was ever observed. That is a
            latency claim manufactured from missing data, on a board whose entire premise is that
            a number must re-derive from its evidence.

            The distinction is the same one this file already draws for the no-clean-rung case,
            and the same one `read_at`'s own tests exist to protect. */
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
#[path = "tests/frontier_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/frontier_absence_attribution_tests.rs"]
mod absence_attribution_tests;

#[cfg(test)]
#[path = "tests/frontier_boundary_proof_tests.rs"]
mod boundary_proof_tests;
