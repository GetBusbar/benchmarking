// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The two search shapes, ported from shell and made pure. Exactly two, and every metric goes
// through one of them — a third shape living in a metric's own module is how two measurements
// that should agree stop agreeing.
//
// GATE metrics (sustained rps, sustained concurrent streams) are pass/fail and monotone in
// concurrency: `bisect_ceiling` finds the true integer ceiling, proven by the ceiling passing and
// ceiling+1 having been measured and failing.
//
// CEILING metrics (peak throughput, cpu-bound frames/sec) rise and then plateau: past saturation
// more concurrency buys queueing rather than throughput (Little's Law), so the curve wobbles
// around a level instead of falling away from a summit. `climb_rungs` measures every rung the same
// way (same windows, same median, same spread) and stops on the predicate flip described below,
// never on a heuristic "flatness" judgment.
//
// This replaced an earlier peak search (calibration step, medians, confirm step, each updating the
// running best differently) that was cheaper but wrong on real gateways repeatedly — publishing a
// search-range bound as one gateway's maximum, and a third of the curve as another's. Uniform
// measurement is what makes the result predictable from the curve alone.
//
// Both searches are generic over `Probe` so they run against a synthetic curve in tests with no
// process, mock, or network involved. A `None` from the probe is a stopped clock (deadline, or a
// window that produced nothing), never a failed gate; the search halts and reports what it proved,
// never a fabricated number.

use crate::measurement::{Absent, Measurement};
use serde::Serialize;
use std::collections::BTreeMap;

/// What a load window observed besides its rate: the latency it ran at and the requests it lost.
///
/// Carrying this lets one sweep answer both the throughput and sustained-latency questions from a
/// single set of windows. Without it, `rps_sustained_20ms` had to re-drive the whole cell in a
/// second search minutes later — after the memory group had restarted the gateway — so the two
/// published throughput numbers described two different, non-simultaneous states of the gateway.
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
/// own wall clock ran out), distinct from `Some(Sample { passed: false, .. })`, a real, measured
/// gate failure.
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
    /// can re-derive both throughput answers from the one sweep rather than taking the pair on trust.
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

    /// A deliberate re-probe of a concurrency, bypassing the memo.
    ///
    /// Used to measure this rig's own noise, where identical conditions do NOT produce identical
    /// numbers — something the cache would hide by returning the first window's answer. Each
    /// repeat is recorded as its own point so the sweep carries the evidence, not just the number.
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

/// The one ladder both searches climb.
///
/// Two searches used to hand-roll the same doubling loop separately, and drifted: one was fixed to
/// climb from the floor instead of opening at the top of the range, the other wasn't, until the
/// range widened and it started asking a gateway for 65536 concurrent streams outright. Shared here
/// so "where do we probe next" has one answer. `no_search_leaps_past_the_ladder_it_climbed` holds
/// every search in this module to it, including any added later.
struct Ladder {
    current: u32,
    max: u32,
}

impl Ladder {
    /// Starts at the floor, always: the opening request a gateway sees must never be a function of
    /// how wide the range was set.
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
/// `Absent(SearchExhausted)` covers two opposite endings, both carrying a `detail` that says which:
///   * the top of the range still passed — the true ceiling is at least `max_conc`, a LOWER bound
///     the search chose rather than a ceiling the gate proved; or
///   * the search floor already failed — the ceiling is below `min_conc`, an UPPER bound.
///
/// KNOWN GAP: the `SearchExhausted` token itself doesn't distinguish these two cases (only
/// `detail`'s text does), the same ambiguity `frontier.rs` refuses by name for its own pair of
/// absences. Splitting it needs a new vocabulary variant threaded through `token()`, the python
/// field lists, and the site's seal — not done yet.
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
    // Zero concurrency is not a load window: `Ladder::from_floor` floors the climb at 1, but the
    // opening probe here is taken at `min_conc` before the ladder exists, so a configured floor of 0
    // would ask for zero concurrent requests and anchor the bisection on a "pass" that sent and lost
    // nothing — passing any gate by construction. `climb_rungs` applies the same floor.
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
        // A failing floor only proves the ceiling is below the floor. At floor=1 there's nowhere
        // left to look, so 0 is a real measured result. At any higher floor it only proves the
        // ceiling is somewhere in [0, min_conc-1], none of which was probed — returning 0 there
        // would fabricate a number the search never established.
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

    // Climb to the failure; never open at the top of the range. A rig that opens beyond what the
    // gateway can carry learns only that the top failed, and the failure recorded may be the rig's
    // own (load generator, mock, and gateway all hitting the wall together with nothing to say who
    // hit it first). Doubling from the floor never asks for more than twice what's already been
    // shown to sustain. Contract unchanged: `Measured` requires a pass at n and a measured failure
    // above it; a range whose top still passes stays `SearchExhausted`.
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

/// Nearest-rank p50 over a sorted slice: the same convention every other published percentile in
/// this engine uses, resolved through the one shared `stats::nearest_rank_index` rather than
/// reimplemented, so it returns a value some window actually produced rather than an average.
///
/// SRCH-04: this file's floor and `metric.rs`'s ceil used to disagree by one rank on every even
/// window count despite comments claiming they matched — moved here to share the one function.
pub fn nearest_rank_median(sorted: &[f64]) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    Some(sorted[crate::stats::nearest_rank_index(sorted.len(), 0.5)])
}

/// Walks the ladder and probes; decides nothing. Returns every rung probed, in probe order, and no
/// verdict — see `frontier.rs`, which reads the throughput answer at six different tail-latency
/// bounds from one call to this.
///
/// This replaced `saturation_plateau`, which climbed AND decided: judged each rung against its own
/// measured wobble, counted consecutive non-improvers, required a minimum concurrency before
/// believing saturation, then picked a winner. Every one of those thresholds could move a published
/// number — e.g. kong's climb once stopped at c=32 and published 15909, which the sustained reading
/// of the very same windows then beat at 17898. Once nothing judges the shape of the curve, every
/// probed rung is a rung the frontier can consider; there's no peak to locate or flatness to detect.
///
/// The stopping rule is a predicate flip, not a count: the climb stops when a rung produces no
/// clean window at all (`SweepProbe`'s `passed` is `fail == 0`). Past that point more concurrency
/// can't un-fail those requests, and a rung that fails every window contributes to no reading at
/// any bound (`frontier::Rung::served_cleanly`), so continuing costs load for nothing, and stopping
/// can never lower a published number.
///
/// Still climbs to `max_conc` when nothing fails — that's a LOWER BOUND, not a ceiling;
/// `frontier::Reading::is_lower_bound` reports it as one instead of discarding the rate.
pub fn climb_rungs<P: Probe>(probe: &mut P, min_conc: u32, max_conc: u32) -> Vec<ProbedPoint> {
    // Normalised the same way every other search here normalises, so a reversed range is a caller's
    // typo rather than a silently empty climb.
    let (min_conc, max_conc) = if min_conc <= max_conc {
        (min_conc, max_conc)
    } else {
        (max_conc, min_conc)
    };
    let mut s = Search::new(probe);
    let mut ladder = Ladder::from_floor(min_conc, max_conc);
    // The floor is probed first: `Ladder::next` doubles before it yields, so skipping this would
    // open the climb at twice the intended concurrency.
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
                // The rig could not run this window; not a finding about the gateway, so the climb
                // ends with what it has rather than reading the failure as a ceiling.
                None => return s.points,
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
    s.points
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── bisect_ceiling ──────────────────────────────────────────────────────────────────────────

    struct MonotoneGate {
        ceiling: u32,
    }
    impl Probe for MonotoneGate {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            Some(Sample::new(c as f64, c <= self.ceiling))
        }
    }

    #[test]
    fn bisect_finds_ceiling_between_rungs() {
        // The concrete shell fixture: sustained bisect lands on the true ceiling 1300, strictly
        // between the doubling rungs 1024 and 2048, over [8, 4096].
        let mut probe = MonotoneGate { ceiling: 1300 };
        let r = bisect_ceiling(&mut probe, 8, 4096);
        assert_eq!(r.ceiling, Measurement::Measured(1300));
        assert!(r.points.contains(&ProbedPoint {
            concurrency: 1301,
            passed: false,
            value: 1301.0,
            reading: None,
        }));
    }

    #[test]
    fn bisect_bottom_already_failing_is_a_measured_zero() {
        let mut probe = MonotoneGate { ceiling: 0 };
        let r = bisect_ceiling(&mut probe, 1, 64);
        assert_eq!(r.ceiling, Measurement::Measured(0));
    }

    /// ZERO CONCURRENCY IS NOT A LOAD WINDOW, and this search took one.
    ///
    /// `Ladder::from_floor` floors the climb at 1, but the opening probe was taken at `min_conc`
    /// before the ladder existed, so a configured floor of 0 asked the gateway for zero concurrent
    /// requests. That window sends nothing and loses nothing, so it passes any gate by construction,
    /// and the whole bisection is then anchored on a "pass" that measured nothing at all.
    #[test]
    fn the_gate_bisection_never_opens_with_a_zero_concurrency_window() {
        struct RecordsWhatItWasAsked {
            seen: Vec<u32>,
            ceiling: u32,
        }
        impl Probe for RecordsWhatItWasAsked {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                self.seen.push(c);
                Some(Sample::new(f64::from(c), c <= self.ceiling))
            }
        }
        let mut probe = RecordsWhatItWasAsked {
            seen: Vec::new(),
            ceiling: 100,
        };
        let r = bisect_ceiling(&mut probe, 0, 4096);
        assert!(
            !probe.seen.contains(&0),
            "a floor of 0 must never be probed literally, asked: {:?}",
            probe.seen
        );
        assert_eq!(
            probe.seen.first(),
            Some(&1),
            "the search opens at the floored floor"
        );
        assert_eq!(
            r.ceiling,
            Measurement::Measured(100),
            "and the floor being clamped does not change the ceiling it finds"
        );
    }

    #[test]
    fn bisect_top_still_passing_is_exhausted_never_a_number() {
        let mut probe = MonotoneGate { ceiling: 100_000 };
        let r = bisect_ceiling(&mut probe, 8, 4096);
        assert_eq!(r.ceiling.reason(), Some(&Absent::SearchExhausted));
        assert_eq!(r.ceiling.copied(), None);
    }

    struct Interrupter {
        fires_after: u32,
        calls: u32,
    }
    impl Probe for Interrupter {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            self.calls += 1;
            if self.calls > self.fires_after {
                return None;
            }
            Some(Sample::new(c as f64, c <= 50))
        }
    }

    // The gate is fixed; only the harness's own search floor moves. If an interrupted search
    // published a confirmed rung, the answer would track the floor (1, 8, 16, 64) and carry zero
    // bits about the gateway.
    #[test]
    fn an_interrupted_search_never_publishes_the_harness_own_floor() {
        for floor in [1u32, 8, 16, 64] {
            let mut probe = Interrupter {
                fires_after: 1,
                calls: 0,
            };
            let r = bisect_ceiling(&mut probe, floor, 4096);
            assert_eq!(
                r.ceiling.copied(),
                None,
                "floor={floor} leaked into the published ceiling, which is a readout of our config"
            );
            // The measurement is not lost: every probed rung still travels.
            assert!(
                !r.points.is_empty(),
                "the probed trace must survive an absent verdict"
            );
            assert!(
                r.ceiling
                    .detail()
                    .unwrap_or_default()
                    .contains(&floor.to_string()),
                "the lower bound belongs in the evidence, not in the value"
            );
        }
    }

    // Two different gateways, same harness floor. If an interrupted search published a rung, both
    // would report the identical number despite behaving completely differently.
    #[test]
    fn two_unlike_gateways_do_not_collapse_to_the_same_interrupted_answer() {
        let mut slow = Interrupter {
            fires_after: 1,
            calls: 0,
        };
        let mut fast = Interrupter {
            fires_after: 1,
            calls: 0,
        };
        let a = bisect_ceiling(&mut slow, 8, 4096);
        let b = bisect_ceiling(&mut fast, 8, 4096);
        assert_eq!(a.ceiling.copied(), None);
        assert_eq!(b.ceiling.copied(), None);
    }

    // Interrupted before anything passed: genuinely unmeasured, so absent.
    #[test]
    fn bisect_interrupted_before_any_pass_is_unmeasured() {
        // fires_after: 0 means even the floor probe returns nothing.
        let mut probe = Interrupter {
            fires_after: 0,
            calls: 0,
        };
        let r = bisect_ceiling(&mut probe, 1, 1000);
        assert_eq!(r.ceiling.copied(), None);
        assert_eq!(r.ceiling.reason(), Some(&Absent::NotMeasured));
    }

    // Interrupted after a confirmed pass must still report no ceiling, not the confirmed rung: the
    // only successful probe here is the floor, so returning it would publish our own search
    // configuration as the gateway's ceiling.
    #[test]
    fn bisect_interrupted_after_a_pass_is_absent_with_the_bound_as_evidence() {
        let mut probe = Interrupter {
            fires_after: 1,
            calls: 0,
        };
        let r = bisect_ceiling(&mut probe, 1, 1000);
        assert_eq!(
            r.ceiling.copied(),
            None,
            "no failure was measured, so no ceiling was proven"
        );
        assert_eq!(r.ceiling.reason(), Some(&Absent::NotMeasured));
        assert!(
            r.points.iter().any(|p| p.concurrency == 1 && p.passed),
            "the pass still travels"
        );
        assert!(r
            .ceiling
            .detail()
            .unwrap_or_default()
            .contains("at least 1"));
    }

    // The partial answer is still a lower bound, never the search range: an interrupted run must
    // not report the top of the range it never confirmed.
    #[test]
    fn an_interrupted_bisect_never_reports_the_unconfirmed_top() {
        let mut probe = Interrupter {
            fires_after: 1,
            calls: 0,
        };
        let r = bisect_ceiling(&mut probe, 1, 1000);
        assert_ne!(r.ceiling.copied(), Some(1000));
    }

    // A failing floor above one proves only that the ceiling is below the floor; no value in
    // [0, floor-1] was probed, so returning 0 there would fabricate a number. `Measured(0)` is
    // reserved for a floor of one, where there is genuinely nowhere left to look.
    #[test]
    fn a_failing_floor_above_one_is_absent_never_the_measured_zero_reserved_for_c_equals_one() {
        let mut probe = MonotoneGate { ceiling: 0 };
        let r = bisect_ceiling(&mut probe, 8, 4096);
        assert_eq!(
            r.ceiling.copied(),
            None,
            "no concurrency in [0, 7] was probed, so 0 was never measured"
        );
        assert_eq!(r.ceiling.reason(), Some(&Absent::SearchExhausted));
        assert!(
            r.ceiling.detail().unwrap_or_default().contains("c=8"),
            "the reason must name the floor that failed, got {:?}",
            r.ceiling.detail()
        );
        // The refusal is about the floor, not the gate: with a floor of one the identical gate
        // yields a real measured zero, and the two answers must not be interchangeable.
        let mut same_gate = MonotoneGate { ceiling: 0 };
        assert_eq!(
            bisect_ceiling(&mut same_gate, 1, 4096).ceiling,
            Measurement::Measured(0)
        );
    }

    // The range bounds are normalised, so a caller that passes them reversed gets the same answer
    // rather than a search over an empty or inverted interval. A silently inverted range would make
    // the floor probe the top rung and the top probe the floor, which inverts the pass/fail
    // invariant the bisection is built on and can only produce nonsense.
    #[test]
    fn a_bisect_range_given_backwards_searches_the_same_interval() {
        let mut forwards = MonotoneGate { ceiling: 1300 };
        let mut backwards = MonotoneGate { ceiling: 1300 };
        let a = bisect_ceiling(&mut forwards, 8, 4096);
        let b = bisect_ceiling(&mut backwards, 4096, 8);
        assert_eq!(a.ceiling.copied(), Some(1300));
        assert_eq!(
            b.ceiling.copied(),
            a.ceiling.copied(),
            "an inverted range must be normalised, not searched inverted"
        );
    }

    // A range of one proves nothing about a ceiling: the single rung passed, nothing above it was
    // probed, so the answer is a lower bound, not the gateway's ceiling.
    #[test]
    fn a_single_point_range_that_passes_is_exhausted_not_a_ceiling() {
        let mut probe = MonotoneGate { ceiling: 1000 };
        let r = bisect_ceiling(&mut probe, 64, 64);
        assert_eq!(
            r.ceiling.copied(),
            None,
            "one passing rung is a lower bound, not a proven ceiling"
        );
        assert_eq!(r.ceiling.reason(), Some(&Absent::SearchExhausted));
    }

    // The narrowest range that can actually prove a ceiling: two adjacent rungs, the lower passing
    // and the upper failing. The bisection loop never runs (b - a is already 1), so this is the one
    // path where the answer comes straight from the two bracket probes.
    #[test]
    fn two_adjacent_rungs_with_a_pass_and_a_fail_prove_the_ceiling_without_bisecting() {
        let mut probe = MonotoneGate { ceiling: 4 };
        let r = bisect_ceiling(&mut probe, 4, 5);
        assert_eq!(r.ceiling, Measurement::Measured(4));
        assert!(
            r.points.iter().any(|p| p.concurrency == 5 && !p.passed),
            "the measured failure at 5 is the proof, and it must be in the trace"
        );
    }

    // ── climb_rungs: walk the ladder, decide nothing ────────────────────────────────────────────

    /// A gateway that keeps serving cleanly to a limit, then starts losing requests above it. `passed`
    /// is `fail == 0`, which is what `SweepProbe` means by it.
    struct CleanToLimit {
        limit: u32,
    }
    impl Probe for CleanToLimit {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            Some(Sample::new(f64::from(c) * 10.0, c <= self.limit))
        }
    }

    // The ladder is walked whole; the climb stops on the predicate flip (first rung where no window
    // served cleanly), never on a flat-rung count.
    #[test]
    fn the_climb_stops_when_requests_start_failing_not_when_the_curve_flattens() {
        let mut probe = CleanToLimit { limit: 1024 };
        let points = climb_rungs(&mut probe, 1, 65_536);
        let probed: Vec<u32> = {
            let mut v: Vec<u32> = points.iter().map(|p| p.concurrency).collect();
            v.dedup();
            v
        };
        assert_eq!(
            probed,
            vec![1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048],
            "every doubling from the floor, plus the first failing rung, and then stop"
        );
        // The first failing rung is probed and returned as evidence the rung below is a real
        // boundary, not just where we stopped; `frontier` needs it for `first_disqualified_conc`.
        assert!(points.iter().any(|p| p.concurrency == 2048 && !p.passed));
        assert!(
            !points.iter().any(|p| p.concurrency == 4096),
            "and nothing above it: more concurrency cannot un-fail those requests"
        );
    }

    // Regression test: kong's curve creeps up by less than its own window-to-window wobble, so the
    // old flat-run counter fired early and stopped at c=32, publishing 15909 as the maximum — beaten
    // by the sustained reading of the same windows at 17898. This shape must climb to the end.
    #[test]
    fn a_curve_that_creeps_inside_its_own_noise_is_still_climbed_to_the_end() {
        struct Creeping;
        impl Probe for Creeping {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                // Gains ~3% per doubling, smaller than a noisy gateway's window spread.
                Some(Sample::new(
                    15_000.0 * (1.0 + 0.03 * f64::from(c.ilog2())),
                    true,
                ))
            }
        }
        let mut probe = Creeping;
        let points = climb_rungs(&mut probe, 1, 4096);
        let top = points.iter().map(|p| p.concurrency).max().unwrap();
        assert_eq!(
            top, 4096,
            "a creeping curve must be climbed to the top of the range"
        );
        let best = points.iter().map(|p| p.value).fold(0.0_f64, f64::max);
        let at_32 = points
            .iter()
            .filter(|p| p.concurrency == 32)
            .map(|p| p.value)
            .fold(0.0_f64, f64::max);
        assert!(
            best > at_32,
            "the rungs above c=32 are better and must be in the record: {best} vs {at_32}"
        );
    }

    // A rung that fails every window ends the climb; one that fails only some does not, since a
    // single clean window is still a real observation the frontier can read.
    #[test]
    fn a_rung_with_one_clean_window_does_not_end_the_climb() {
        struct FlakyAtOneRung {
            seen: std::collections::BTreeMap<u32, usize>,
        }
        impl Probe for FlakyAtOneRung {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                let n = self.seen.entry(c).or_insert(0);
                *n += 1;
                // At c=16 the first two windows fail and the third is clean.
                let passed = if c == 16 { *n >= 3 } else { true };
                Some(Sample::new(f64::from(c), passed))
            }
        }
        let mut probe = FlakyAtOneRung {
            seen: Default::default(),
        };
        let points = climb_rungs(&mut probe, 1, 64);
        assert!(
            points.iter().any(|p| p.concurrency == 64),
            "one clean window at c=16 keeps the climb going: {:?}",
            points.iter().map(|p| p.concurrency).collect::<Vec<_>>()
        );
    }

    // Every rung gets `WINDOWS_PER_RUNG` windows, so the frontier reads a rate backed by repeats
    // rather than one lucky window. The memo may serve the first window; the repeats must be real.
    #[test]
    fn every_rung_is_probed_the_full_number_of_windows() {
        let mut probe = CleanToLimit { limit: 64 };
        let points = climb_rungs(&mut probe, 1, 8);
        for c in [1u32, 2, 4, 8] {
            let n = points.iter().filter(|p| p.concurrency == c).count();
            assert_eq!(n, WINDOWS_PER_RUNG, "c={c} got {n} windows");
        }
    }

    // A rig that cannot run a window ends the climb with what it has, never reading its own failure
    // as the gateway's ceiling.
    #[test]
    fn a_rig_that_stops_answering_ends_the_climb_without_inventing_a_limit() {
        struct DiesAt {
            at: u32,
        }
        impl Probe for DiesAt {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                if c >= self.at {
                    return None;
                }
                Some(Sample::new(f64::from(c), true))
            }
        }
        let mut probe = DiesAt { at: 32 };
        let points = climb_rungs(&mut probe, 1, 4096);
        assert!(points.iter().all(|p| p.concurrency < 32));
        assert!(
            points.iter().all(|p| p.passed),
            "nothing recorded a failure, so no reading may later read one as the gateway's limit"
        );
    }

    // Normalised like every other search here: a reversed range is a typo, not an empty climb.
    #[test]
    fn a_climb_range_given_backwards_walks_the_same_interval() {
        let mut a = CleanToLimit { limit: 4096 };
        let mut b = CleanToLimit { limit: 4096 };
        let fwd: Vec<u32> = climb_rungs(&mut a, 1, 64)
            .iter()
            .map(|p| p.concurrency)
            .collect();
        let rev: Vec<u32> = climb_rungs(&mut b, 64, 1)
            .iter()
            .map(|p| p.concurrency)
            .collect();
        assert_eq!(fwd, rev);
    }

    // ── climb_rungs ─────────────────────────────────────────────────────────────────────────────
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    struct MonotoneGate {
        ceiling: u32,
    }
    impl Probe for MonotoneGate {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            Some(Sample::new(c as f64, c <= self.ceiling))
        }
    }

    struct AlwaysPasses;
    impl Probe for AlwaysPasses {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            Some(Sample::new(c as f64, true))
        }
    }

    proptest! {
        // Any monotone pass/fail curve with a true ceiling inside [min_conc, max_conc) lands on exactly that
        // ceiling, and the recorded proof is ceiling+1, measured and failing.
        #[test]
        fn bisect_lands_on_any_true_ceiling(ceiling in 1u32..999u32) {
            let min_conc = 1u32;
            let max_conc = 1000u32;
            let mut probe = MonotoneGate { ceiling };
            let r = bisect_ceiling(&mut probe, min_conc, max_conc);
            prop_assert_eq!(r.ceiling.copied(), Some(ceiling));
            prop_assert!(r.points.iter().any(|p| p.concurrency == ceiling + 1 && !p.passed));
        }

        // A curve that still passes at the top of the range never yields a number.
        #[test]
        fn bisect_top_passing_is_always_exhausted(min_conc in 1u32..100u32, max_conc in 100u32..10_000u32) {
            let mut probe = AlwaysPasses;
            let r = bisect_ceiling(&mut probe, min_conc, max_conc);
            prop_assert_eq!(r.ceiling.copied(), None);
            prop_assert_eq!(r.ceiling.reason(), Some(&Absent::SearchExhausted));
        }

    }
}
