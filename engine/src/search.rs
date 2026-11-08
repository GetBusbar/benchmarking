// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE TWO SEARCH SHAPES, PORTED FROM SHELL AND MADE PURE.
//
// THERE ARE EXACTLY TWO, AND EVERY METRIC GOES THROUGH ONE OF THEM. A third shape living in one
// metric's own module is how two measurements that should agree stop agreeing, so the rule is one
// function per shape, used everywhere that shape occurs.
//
// GATE metrics (sustained rps, sustained concurrent streams) are pass/fail and monotone in
// concurrency: `bisect_ceiling` finds the true integer ceiling, proven by the ceiling passing and
// ceiling+1 having been measured and failing.
//
// CEILING metrics (peak throughput, cpu-bound frames/sec) rise and then PLATEAU: past saturation
// more concurrency buys queueing rather than throughput (Little's Law), so the curve wobbles around
// a level instead of falling away from a summit. `climb_rungs` measures every rung the same way -
// `WINDOWS_PER_RUNG` windows at each concurrency - and DECIDES NOTHING: it stops only when a rung
// produces no clean window at all, or the ladder tops out, or the rig cannot run a window. There is
// no plateau or wobble rule; the readings are taken off the rungs afterwards (`frontier.rs`).
//
// It replaced a unimodal peak search that demanded a turnover a healthy gateway never produces, and
// then a cleverer version of itself: one with a single-window fast path, a separate calibration
// step, an escalation to medians and a confirm step, each updating the running best differently.
// That version was cheaper and it was wrong on real gateways four times running, in ways that could
// not be traced by reading it - it published a search-range bound as one gateway's maximum, and a
// third of the curve as another's. Uniform measurement is the property that makes this predictable
// from the curve alone.
//
// Both searches are generic over `Probe` so they run against a synthetic curve in tests with no
// process, mock, or network involved. A `None` from the probe is a stopped clock (a deadline, or a
// window that produced nothing), never a failed gate; the search halts and reports what it proved,
// never a fabricated number.

use crate::measurement::{Absent, Measurement};
use serde::Serialize;
use std::collections::BTreeMap;

/// What a load window observed BESIDE its rate: the latency it ran at and the requests it lost.
///
/// The load generator computes both for every window it runs and always has. They used to die at
/// this boundary, because `Sample` carried a rate and a verdict and nothing else - so a sweep of 33
/// windows published 33 rates and threw away 33 latency readings it had already paid for. The cost
/// of that was not the lost evidence, it was the second search: `rps_sustained_20ms` could not be
/// read off the throughput sweep, so it re-drove the whole cell minutes later, after the memory
/// group had restarted the gateway. The two published throughput numbers then described two
/// different states of the same gateway, and on three cells of the 2026-07-28 run the "sustained"
/// number came out ABOVE the "maximum" one. Neither reading was wrong; they were not simultaneous.
///
/// Carrying this is what lets one sweep answer both questions from one set of windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Reading {
    /// `None` when the window ran but reported no percentile. Absent, not zero: a gate that needs a
    /// latency has not earned one from a window that never produced it.
    pub p99_us: Option<u64>,
    pub ok: u64,
    pub fail: u64,
}

/// One probe outcome at a concurrency: whether the gate passed, and the value it produced (rps,
/// fps, or 0.0 for a gate with no scalar output beyond pass/fail).
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub value: f64,
    pub passed: bool,
    /// The latency and loss behind `passed`, for probes that measure them.
    ///
    /// `None` is a real state and not a placeholder: the stream searches judge frames per second and
    /// never take an HTTP latency percentile, so they have no reading to give. A consumer that needs
    /// one must handle its absence rather than read a zero as "measured no failures".
    pub reading: Option<Reading>,
}

impl Sample {
    /// A probe outcome with no latency reading behind it.
    pub fn new(value: f64, passed: bool) -> Self {
        Sample {
            value,
            passed,
            reading: None,
        }
    }

    /// The same outcome, carrying what the window observed.
    pub fn with_reading(self, reading: Reading) -> Self {
        Sample {
            reading: Some(reading),
            ..self
        }
    }
}

/// A concurrency probe. `None` means the window produced nothing (deadline fired, or the caller's
/// own wall clock ran out) -- distinct from `Some(Sample { passed: false, .. })`, which is a real,
/// measured gate failure.
pub trait Probe {
    fn probe(&mut self, concurrency: u32) -> Option<Sample>;
}

/// One probed point, kept for the published sweep trace regardless of which way the search went.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProbedPoint {
    pub concurrency: u32,
    pub passed: bool,
    pub value: f64,
    /// What the window observed, when the probe measures it. Published beside the rate so a reader
    /// can re-derive BOTH throughput answers from the one sweep rather than taking the pair on trust.
    pub reading: Option<Reading>,
}

/// Memoised probe access shared by both searches: a concurrency is never probed twice, and every
/// probe that does run (deadline or not) is recorded once, in probe order.
struct Search<'p, P: Probe> {
    probe: &'p mut P,
    cache: BTreeMap<u32, Sample>,
    points: Vec<ProbedPoint>,
}

impl<'p, P: Probe> Search<'p, P> {
    fn new(probe: &'p mut P) -> Self {
        Self {
            probe,
            cache: BTreeMap::new(),
            points: Vec::new(),
        }
    }

    fn sample(&mut self, c: u32) -> Option<Sample> {
        if let Some(s) = self.cache.get(&c) {
            return Some(s.clone());
        }
        let sample = self.probe.probe(c)?;
        self.points.push(ProbedPoint {
            concurrency: c,
            passed: sample.passed,
            value: sample.value,
            reading: sample.reading,
        });
        self.cache.insert(c, sample.clone());
        Some(sample)
    }

    /// A DELIBERATE re-probe of a concurrency, bypassing the memo.
    ///
    /// Used only to calibrate this rig's own measurement noise, where the whole point is that
    /// identical conditions do NOT produce identical numbers - something the cache hides by
    /// construction, since it would hand back the first window's answer and report a spread of zero.
    /// Each repeat is recorded as its own point, so the published sweep carries the evidence the
    /// noise floor was derived from rather than asking a reader to take it on trust.
    fn sample_repeat(&mut self, c: u32) -> Option<Sample> {
        let sample = self.probe.probe(c)?;
        self.points.push(ProbedPoint {
            concurrency: c,
            passed: sample.passed,
            value: sample.value,
            reading: sample.reading,
        });
        self.cache.insert(c, sample.clone());
        Some(sample)
    }
}

/// THE ONE LADDER BOTH SEARCHES CLIMB.
///
/// Two searches hand-rolled the same doubling loop, and that is how they came to disagree: the
/// plateau search was fixed to climb from the floor after it opened a 1..65536 run by asking for
/// 32768 concurrent connections, the fix was written as a comment inside that function, and the gate
/// search kept probing the top of its range outright for another day. Raising the engine ceiling to
/// 65536 then made the untouched one open by asking a gateway for 65536 concurrent streams.
///
/// A rule that lives in one function protects one function. This is the rule as code, used by both,
/// so "where do we probe next" has a single answer and cannot drift again. `no_search_leaps_past_the
/// _ladder_it_climbed` holds every search in this module to it, including any added later.
struct Ladder {
    current: u32,
    max: u32,
}

impl Ladder {
    /// Starts AT THE FLOOR, always. The opening request a gateway sees must never be a function of
    /// how wide the range was set: that is what makes a wider search a more dangerous one, and it is
    /// the defect this type exists to make unrepresentable.
    fn from_floor(min_conc: u32, max_conc: u32) -> Self {
        Self {
            current: min_conc.max(1),
            max: max_conc,
        }
    }

    fn floor(&self) -> u32 {
        self.current
    }

    /// The next rung: double, never past the top. `None` once the top has been reached, so a caller
    /// cannot loop forever on a saturating multiply. Doubling zero is zero, which is why the floor is
    /// clamped to 1 above rather than trusted from the caller.
    fn next(&mut self) -> Option<u32> {
        if self.current >= self.max {
            return None;
        }
        self.current = self.current.saturating_mul(2).min(self.max);
        Some(self.current)
    }
}

// ─────────────────────────────────────────── bisect_ceiling ───────────────────────────────────────

/// The result of a gate-ceiling bisection: `Measured(n)` iff `n` passes and `n+1` was measured and
/// failed (or `n == 0`, the measured "nothing sustains this gate" answer).
///
/// `Absent(SearchExhausted)` covers TWO OPPOSITE ENDINGS, and the "iff" that stood here claimed one:
///   * the top of the range still passed - the true ceiling is at least `max_conc`, a LOWER bound
///     the search chose rather than a ceiling the gate proved; and
///   * the search FLOOR already failed - the ceiling is below `min_conc`, an UPPER bound.
///
/// Both are honestly absent and both carry a `detail` that says which happened, so no number is
/// wrong today. But the token is the only machine-readable field, and a consumer branching on it
/// cannot tell "carries fewer than the floor" from "carries more than the ceiling" - which is the
/// same defect `frontier.rs` refuses by name for its own pair of absences. Splitting it needs a new
/// vocabulary variant carried through `token()`, the python field lists and the site's seal, so it
/// is recorded rather than done in the hour before a field run.
#[derive(Debug, Clone, Serialize)]
pub struct BisectResult {
    pub ceiling: Measurement<u32>,
    pub points: Vec<ProbedPoint>,
}

/// Bisect `[min_conc, max_conc]` to the true integer ceiling of a pass/fail gate assumed monotone in
/// concurrency (everything at or below the ceiling passes, everything above fails). `min_conc` and `max_conc`
/// are normalised (swapped) if given reversed.
pub fn bisect_ceiling<P: Probe>(probe: &mut P, min_conc: u32, max_conc: u32) -> BisectResult {
    let (min_conc, max_conc) = if min_conc <= max_conc {
        (min_conc, max_conc)
    } else {
        (max_conc, min_conc)
    };
    // ZERO CONCURRENCY IS NOT A LOAD WINDOW, and this search probed it literally.
    //
    // `Ladder::from_floor` floors the CLIMB at 1, but the opening probe here is taken at `min_conc`
    // before the ladder is built, so a configured floor of 0 (`OTB_MIN_CONC=0`) asked the gateway
    // for zero concurrent requests and then anchored the whole bisection on whatever that window
    // claimed - a "pass" at c=0 is a window that sent nothing and lost nothing, so it passes any
    // gate by construction. `climb_rungs` already carries the same floor for the same
    // reason; this is that rule applied to the search that was still missing it.
    let min_conc = min_conc.max(1);
    let max_conc = max_conc.max(1);
    let mut s = Search::new(probe);

    let lo_sample = match s.sample(min_conc) {
        Some(v) => v,
        None => {
            return BisectResult {
                ceiling: Measurement::absent(Absent::NotMeasured),
                points: s.points,
            }
        }
    };
    if !lo_sample.passed {
        // A FAILING FLOOR ONLY PROVES THE CEILING IS BELOW THE FLOOR.
        //
        // When the floor is 1 there is nowhere left to look, so nothing sustains the gate and 0 is a
        // real measured result. For any higher floor it proves only that the ceiling lies somewhere
        // in [0, min_conc-1], and not one of those values was probed. Returning 0 there would
        // publish a specific number the search never established, which is the same fabrication as
        // publishing the range bound at the top end, just at the other end of the range.
        return if min_conc <= 1 {
            BisectResult {
                ceiling: Measurement::Measured(0),
                points: s.points,
            }
        } else {
            let detail = format!(
                "the search floor c={min_conc} already failed the gate, so the ceiling is below it and was never probed"
            );
            BisectResult {
                ceiling: Measurement::absent_because(Absent::SearchExhausted, detail),
                points: s.points,
            }
        };
    }

    // CLIMB TO THE FAILURE; NEVER OPEN AT THE TOP OF THE RANGE.
    //
    // This probed `max_conc` as its first move after the floor, so the opening request of the
    // streams gate was the whole range at once. That is the same defect the plateau climb already
    // carries a comment about - "a start derived from the range made a WIDER range open with a
    // HIGHER first probe, which is how a 1..65536 run began by asking for 32768 concurrent
    // connections" - and raising the engine ceiling to 65536 made this one strictly worse: the first
    // thing a gateway would have been asked for is sixty-five thousand concurrent streams.
    //
    // It is not only unkind to a fragile gateway, it measures the wrong thing. A rig that opens
    // beyond what the gateway can carry learns only that the top failed, and the failure it records
    // may be its own: the load generator, the mock and the gateway all meet the wall together and
    // nothing in the result says which one hit it first.
    //
    // Doubling from the floor never asks for more than twice what the gateway has ALREADY been shown
    // to sustain, so every probe is one the previous probe justified. The contract is unchanged: a
    // ceiling is only `Measured` with a pass at n and a measured failure above it, and a range whose
    // top still passes is still `SearchExhausted` rather than a published bound of ours.
    let mut a = min_conc;
    let mut b = None;
    let mut ladder = Ladder::from_floor(min_conc, max_conc);
    while let Some(c) = ladder.next() {
        match s.sample(c) {
            Some(sample) if sample.passed => a = c,
            Some(_) => {
                b = Some(c);
                break;
            }
            None => {
                let detail = format!(
                    "probe interrupted while climbing at c={c}; the ceiling is at least {a}, and nothing above it was tested"
                );
                return BisectResult {
                    ceiling: Measurement::absent_because(Absent::NotMeasured, detail),
                    points: s.points,
                };
            }
        }
    }
    let Some(b_fail) = b else {
        // No failure anywhere in the range: publishing max_conc would report our own search bound as
        // the gate's ceiling.
        let detail = format!("c={max_conc} still passes at the top of the search range; the true ceiling is at least {max_conc}");
        return BisectResult {
            ceiling: Measurement::absent_because(Absent::SearchExhausted, detail),
            points: s.points,
        };
    };

    // Invariant from here: a passes, b fails. Bisect to +-1; b stays the recorded proof of failure.
    let mut b = b_fail;
    while b - a > 1 {
        let mid = a + (b - a) / 2;
        match s.sample(mid) {
            Some(sample) if sample.passed => a = mid,
            Some(_) => b = mid,
            None => {
                let detail = format!(
                    "probe interrupted mid-bisect; last known pass={a}, last known fail={b}"
                );
                return BisectResult {
                    ceiling: Measurement::absent_because(Absent::NotMeasured, detail),
                    points: s.points,
                };
            }
        }
    }
    BisectResult {
        ceiling: Measurement::Measured(a),
        points: s.points,
    }
}

// ────────────────────────────────────────────── peak_max ──────────────────────────────────────────

/// Windows taken at every rung. Three is the smallest sample with a middle value, and the median of
/// three is what makes a rung's number resistant to one unlucky window.
pub const WINDOWS_PER_RUNG: usize = 3;

/// Nearest-rank p50 over a sorted slice: the SAME convention every other published percentile in
/// this engine uses, because it resolves its rank through the one function they all call
/// (`stats::nearest_rank_index`) rather than reimplementing it. It returns a value some window
/// actually produced rather than the average of two that none did.
///
/// The rank moved out of here as part of ledger SRCH-04: this file's floor and `metric.rs`'s ceil
/// disagreed by one rank on every even window count, while three comments claimed they matched.
pub fn nearest_rank_median(sorted: &[f64]) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    Some(sorted[crate::stats::nearest_rank_index(sorted.len(), 0.5)])
}

/// WALK THE LADDER AND PROBE. DECIDE NOTHING.
///
/// Returns every rung probed, in probe order, and no verdict. The caller reads whatever answer it
/// wants off them - see `frontier.rs`, which reads the throughput answer at six different tail-latency
/// bounds from one call to this.
///
/// THIS REPLACED the old `saturation_plateau` (since deleted; these are the only references left to
/// it, and they are historical). The difference is the whole point: that function climbed AND
/// decided: it judged each rung against its own measured wobble floored at `WOBBLE_FLOOR = 0.02`,
/// counted `FLAT_RUNGS_TO_STOP = 3` consecutive non-improvers, required `MIN_SATURATION_CONC = 16`
/// before believing saturation, and then picked a winner with `published_winner`. Four chosen numbers,
/// and every one of them could move a published throughput figure:
///
///   - Stopping early publishes a smaller number as the gateway's maximum. kong's own case, recorded in
///     the retired constant's doc: at FLAT=2 the climb stopped at c=32 and published 15909, which the
///     sustained reading of the SAME windows then beat at 17898. A maximum another reading beats is not
///     a maximum.
///   - Judging "improvement" against a noise floor decided which of two real rungs got published.
///   - And a climb that ran to the top still improving was converted into an ABSENCE
///     (`Absent::SearchExhausted`), discarding a real measured rate for failing to prove maximality.
///
/// None of that is needed once nothing is looking for the shape of a curve. Every rung probed is a rung
/// the frontier considers, so there is no "peak" to locate and no flatness to detect.
///
/// THE STOPPING RULE IS A PREDICATE FLIP, NOT A COUNT. The climb stops when a rung produces no clean
/// window at all - `SweepProbe`'s `passed` is `fail == 0`, so that means every window at this
/// concurrency lost at least one request the gateway had accepted. Past that point more concurrency
/// cannot un-fail those requests, and a rung that fails every window contributes to no reading at any
/// bound (see `frontier::Rung::served_cleanly`). So continuing would cost load and add nothing, and
/// stopping cannot lower a published number - which is exactly what could not be said of the flat-run
/// counter it replaces.
///
/// It also still climbs to `max_conc` when nothing fails, and that is a LOWER BOUND rather than a
/// ceiling. `frontier::Reading::is_lower_bound` reports it as one instead of throwing the rate away.
pub fn climb_rungs<P: Probe>(probe: &mut P, min_conc: u32, max_conc: u32) -> Vec<ProbedPoint> {
    climb_rungs_report(probe, min_conc, max_conc).points
}

/// A rig interruption preserves observations but cannot establish a throughput boundary.
pub struct ClimbResult {
    pub points: Vec<ProbedPoint>,
    pub interrupted: bool,
}

pub fn climb_rungs_report<P: Probe>(probe: &mut P, min_conc: u32, max_conc: u32) -> ClimbResult {
    // Normalised the same way every other search here normalises, so a reversed range is a caller's
    // typo rather than a silently empty climb.
    let (min_conc, max_conc) = if min_conc <= max_conc {
        (min_conc, max_conc)
    } else {
        (max_conc, min_conc)
    };
    let mut s = Search::new(probe);
    let mut ladder = Ladder::from_floor(min_conc, max_conc);
    // The floor is probed FIRST: `Ladder::next` doubles before it yields, so reading it before the
    // floor would skip the floor entirely and open the climb at twice the intended concurrency.
    let mut c = ladder.floor();
    loop {
        let mut any_clean = false;
        for i in 0..WINDOWS_PER_RUNG {
            // The first window may come from the memo; the repeats must not, since the whole reason
            // there are three is that identical conditions produce different numbers.
            let sample = if i == 0 {
                s.sample(c)
            } else {
                s.sample_repeat(c)
            };
            match sample {
                Some(sm) => any_clean |= sm.passed,
                // The RIG could not run this window. Not a finding about the gateway, so the climb
                // ends with what it has rather than reading the failure as a ceiling.
                None => {
                    return ClimbResult {
                        points: s.points,
                        interrupted: true,
                    }
                }
            }
        }
        if !any_clean {
            break;
        }
        c = match ladder.next() {
            Some(next) => next,
            None => break,
        };
    }
    ClimbResult {
        points: s.points,
        interrupted: false,
    }
}

#[cfg(test)]
#[path = "tests/search_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/search_proptests.rs"]
mod proptests;
