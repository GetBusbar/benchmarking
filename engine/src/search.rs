// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE TWO SEARCH SHAPES, PORTED FROM SHELL AND MADE PURE.
//
// GATE metrics (sustained rps, sustained concurrent streams) are pass/fail and monotone in
// concurrency: `bisect_ceiling` finds the true integer ceiling, proven by the ceiling passing and
// ceiling+1 having been measured and failing. MAX metrics (peak throughput, cpu-bound frames/sec)
// rise then fall: `peak_max` proves a maximum by watching the curve turn over inside the search
// range, never by pinning a single unit (adjacent concurrencies near a real peak differ by less
// than run-to-run noise).
//
// Both searches are generic over `Probe` so they run against a synthetic curve in tests with no
// process, mock, or network involved. A `None` from the probe is a stopped clock (a deadline, or a
// window that produced nothing), never a failed gate; the search halts and reports what it proved,
// never a fabricated number.

use crate::measurement::{Absent, Measurement};
use serde::Serialize;
use std::collections::BTreeMap;

/// One probe outcome at a concurrency: whether the gate passed, and the value it produced (rps,
/// fps, or 0.0 for a gate with no scalar output beyond pass/fail).
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub value: f64,
    pub passed: bool,
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
        Self { probe, cache: BTreeMap::new(), points: Vec::new() }
    }

    fn sample(&mut self, c: u32) -> Option<Sample> {
        if let Some(s) = self.cache.get(&c) {
            return Some(s.clone());
        }
        let sample = self.probe.probe(c)?;
        self.points.push(ProbedPoint { concurrency: c, passed: sample.passed, value: sample.value });
        self.cache.insert(c, sample.clone());
        Some(sample)
    }
}

// ─────────────────────────────────────────── bisect_ceiling ───────────────────────────────────────

/// The result of a gate-ceiling bisection: `Measured(n)` iff `n` passes and `n+1` was measured and
/// failed (or `n == 0`, the measured "nothing sustains this gate" answer). `Absent(SearchExhausted)`
/// iff the top of the range still passed -- the true ceiling is at least `max_conc`, but that is a lower
/// bound the search chose, not a ceiling the gate proved, so it is never published as one.
#[derive(Debug, Clone, Serialize)]
pub struct BisectResult {
    pub ceiling: Measurement<u32>,
    pub points: Vec<ProbedPoint>,
}

/// Bisect `[min_conc, max_conc]` to the true integer ceiling of a pass/fail gate assumed monotone in
/// concurrency (everything at or below the ceiling passes, everything above fails). `min_conc` and `max_conc`
/// are normalised (swapped) if given reversed.
pub fn bisect_ceiling<P: Probe>(probe: &mut P, min_conc: u32, max_conc: u32) -> BisectResult {
    let (min_conc, max_conc) = if min_conc <= max_conc { (min_conc, max_conc) } else { (max_conc, min_conc) };
    let mut s = Search::new(probe);

    let lo_sample = match s.sample(min_conc) {
        Some(v) => v,
        None => return BisectResult { ceiling: Measurement::absent(Absent::NotMeasured), points: s.points },
    };
    if !lo_sample.passed {
        // The floor already fails: a measured "nothing sustains this gate" (0), a real result, not
        // an absence -- distinct from never having probed at all.
        return BisectResult { ceiling: Measurement::Measured(0), points: s.points };
    }

    let hi_sample = match s.sample(max_conc) {
        Some(v) => v,
        None => {
            // Interrupted with a confirmed pass already in hand. The highest rung KNOWN to sustain
            // the gate is a real, if partial, measurement, and the shell publishes it rather than
            // discarding the run. Nulling here would report "we never measured this" about a cell
            // we demonstrably did.
            return BisectResult { ceiling: highest_confirmed_pass(&s.points), points: s.points };
        }
    };
    if hi_sample.passed {
        // No failure was ever observed inside the range: publishing max_conc would report our own search
        // bound as the gate's ceiling.
        let detail = format!("c={max_conc} still passes at the top of the search range; the true ceiling is at least {max_conc}");
        return BisectResult { ceiling: Measurement::absent_because(Absent::SearchExhausted, detail), points: s.points };
    }

    // Invariant from here: a passes, b fails. Bisect to +-1; b stays the recorded proof of failure.
    let mut a = min_conc;
    let mut b = max_conc;
    while b - a > 1 {
        let mid = a + (b - a) / 2;
        match s.sample(mid) {
            Some(sample) if sample.passed => a = mid,
            Some(_) => b = mid,
            None => {
                let detail = format!("probe interrupted mid-bisect; last known pass={a}, last known fail={b}");
                return BisectResult { ceiling: Measurement::absent_because(Absent::NotMeasured, detail), points: s.points };
            }
        }
    }
    BisectResult { ceiling: Measurement::Measured(a), points: s.points }
}

// ────────────────────────────────────────────── peak_max ──────────────────────────────────────────

/// The winning concurrency and value of a peak search.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PeakPoint {
    pub concurrency: u32,
    pub value: f64,
}

/// The result of a unimodal max-search: `Measured(point)` iff the curve was seen to turn over
/// inside the range (a proven interior maximum). `Absent(SearchExhausted)` iff the ramp ran off
/// either end of the range while the value was still rising -- only a lower bound was found, never
/// a peak. `exhausted` mirrors `peak.reason() == Some(&Absent::SearchExhausted)` as a plain bool for
/// callers that do not want to match on the reason.
#[derive(Debug, Clone, Serialize)]
pub struct PeakResult {
    pub peak: Measurement<PeakPoint>,
    pub points: Vec<ProbedPoint>,
    pub exhausted: bool,
}

/// An interruption (a deadline, or a window that produced nothing) after real probes have already
/// landed must NOT throw away what was measured. The shell latches this deliberately: once a rung
/// has been judged, "every later abort still leaves a genuinely measured, if partial, answer
/// behind". Discarding it publishes null for a cell we did in fact measure, which is the same class
/// of loss as publishing a zero for one we did not.
///
/// Only a search that was cut off before ANY gate-passing point is genuinely unmeasured.
fn interrupted<P: Probe>(s: Search<P>) -> PeakResult {
    let mut ordered: Vec<&ProbedPoint> = s.points.iter().collect();
    ordered.sort_by_key(|p| p.concurrency);
    let mut winner: Option<PeakPoint> = None;
    for p in ordered {
        if p.passed && winner.as_ref().is_none_or(|w| p.value > w.value) {
            winner = Some(PeakPoint { concurrency: p.concurrency, value: p.value });
        }
    }
    let peak = match winner {
        Some(w) => Measurement::Measured(w),
        None => Measurement::absent_because(
            Absent::NotMeasured,
            "the search was interrupted before any concurrency passed the gate",
        ),
    };
    PeakResult { peak, points: s.points, exhausted: false }
}


/// The highest concurrency confirmed to PASS among everything actually probed. Used when a search is
/// cut short: a confirmed pass is a real lower bound on the ceiling, and throwing it away would
/// publish null for a cell that was measured.
fn highest_confirmed_pass(points: &[ProbedPoint]) -> Measurement<u32> {
    match points.iter().filter(|p| p.passed).map(|p| p.concurrency).max() {
        Some(c) => Measurement::Measured(c),
        None => Measurement::absent_because(
            Absent::NotMeasured,
            "the search was interrupted before any concurrency passed the gate",
        ),
    }
}

fn eff(sample: &Sample) -> f64 {
    if sample.passed { sample.value } else { 0.0 }
}

/// Doubles (or halves) away from `from_c` (already known to beat `base_v`) while the curve keeps
/// rising, guarding `bound`. Returns `(bracket_low, best_c, best_v, bracket_high, exhausted)`, or
/// `None` if the probe was interrupted. `exhausted = true` means the ramp reached `bound` while
/// still rising: only a lower bound, no interior turnover.
#[allow(clippy::too_many_arguments)]
fn ramp<P: Probe>(
    s: &mut Search<P>,
    bound: u32,
    base_c: u32,
    from_c: u32,
    from_v: f64,
    upward: bool,
) -> Option<(u32, u32, f64, u32, bool)> {
    let mut edge = base_c; // the point just inside `best_c`, toward where the ramp started
    let mut best_c = from_c;
    let mut best_v = from_v;
    loop {
        if best_c == bound {
            return Some((0, best_c, best_v, 0, true));
        }
        let next = if upward { best_c.saturating_mul(2).min(bound) } else { (best_c / 2).max(bound) };
        let nv = s.eff(next)?;
        if nv > best_v {
            edge = best_c;
            best_c = next;
            best_v = nv;
        } else {
            // Turned over: bracket [edge, next] contains the peak, in numeric order regardless of
            // which way the ramp walked.
            let (min_conc, max_conc) = if upward { (edge, next) } else { (next, edge) };
            return Some((min_conc, best_c, best_v, max_conc, false));
        }
    }
}

impl<'p, P: Probe> Search<'p, P> {
    fn eff(&mut self, c: u32) -> Option<f64> {
        self.sample(c).map(|s| eff(&s))
    }
}

/// Search `[min_conc, max_conc]` for the peak of a curve assumed unimodal in concurrency (rises then falls),
/// starting at `start` (clamped into range) and learning direction before ramping, so a peak either
/// above or below `start` is found by the same search. `tol` bounds the final refine bracket's
/// width (an absolute concurrency count, not scaled to the bracket like the shell original's `a/4`
/// heuristic). NOTE: the RPS lane's relative tolerance existed to stop a LOW-concurrency peak
/// being left unresolved, so a caller searching a low-concurrency peak must pass a small `tol`
/// rather than inherit a large default. `min_conc`/`max_conc` are normalised if given reversed.
pub fn peak_max<P: Probe>(probe: &mut P, min_conc: u32, max_conc: u32, start: u32, tol: u32) -> PeakResult {
    let (min_conc, max_conc) = if min_conc <= max_conc { (min_conc, max_conc) } else { (max_conc, min_conc) };
    let mut s = Search::new(probe);
    let start = start.clamp(min_conc, max_conc);

    let start_value = match s.eff(start) {
        Some(v) => v,
        None => return interrupted(s),
    };

    let above_start = if start < max_conc { start.saturating_mul(2).min(max_conc) } else { start };
    let below_start = if start > min_conc { (start / 2).max(min_conc) } else { start };

    let above_value = if above_start != start {
        match s.eff(above_start) {
            Some(v) => Some(v),
            None => return interrupted(s),
        }
    } else {
        None
    };

    let (bracket_low, best_c, best_v, bracket_high, exhausted) = if let Some(uv) = above_value.filter(|v| *v > start_value) {
        match ramp(&mut s, max_conc, start, above_start, uv, true) {
            Some(t) => t,
            None => return interrupted(s),
        }
    } else {
        let below_value = if below_start != start {
            match s.eff(below_start) {
                Some(v) => Some(v),
                None => return interrupted(s),
            }
        } else {
            None
        };
        if let Some(dv) = below_value.filter(|v| *v > start_value) {
            match ramp(&mut s, min_conc, start, below_start, dv, false) {
                Some(t) => t,
                None => return interrupted(s),
            }
        } else if start_value == 0.0 {
            // START FAILED THE GATE, so keep halving until SOMETHING passes, and then open the
            // whole range below it to the refine. Stepping down only once (the bug this replaces)
            // strands the search above a p99 cliff whenever `start` sits several halvings past it,
            // for instance when a stale adaptive prior seeded it high: every later probe stays in
            // the failing region and the true peak below is never sampled at all. The shell fixed
            // exactly this once already (its audit R2-H1) and its comment is explicit that the low
            // bound must reopen to `min_conc`, not to c/2, or the refine bracket clips the real peak out.
            let mut c = below_start;
            let mut found = None;
            loop {
                // PROBE FIRST, THEN STEP. The guard used to be `while c > min_conc`, which exits the moment
                // c reaches min_conc and so never probes min_conc itself: the one point this branch exists to
                // reopen. A gate that passes only at the very bottom of the range was then never
                // sampled at all, and the search returned "no concurrency passed" about a region it
                // had not looked at.
                let v = match s.eff(c) {
                    Some(v) => v,
                    None => return interrupted(s),
                };
                if v > 0.0 {
                    found = Some((c, v));
                    break;
                }
                if c <= min_conc {
                    break;
                }
                c = (c / 2).max(min_conc);
            }
            match found {
                // Reopen the bracket all the way down to `min_conc`: the true peak can sit anywhere below
                // the first rung that passed.
                Some((c, v)) => (min_conc, c, v, start, false),
                None => (min_conc, start, start_value, above_start, false),
            }
        } else {
            // Neither neighbour beats `start`: it is a local (possibly flat) max candidate already.
            (below_start, start, start_value, above_start, false)
        }
    };

    if exhausted {
        let detail = format!(
            "curve was still rising at c={best_c} (value={best_v}) when the search range ran out; no interior turnover found"
        );
        return PeakResult { peak: Measurement::absent_because(Absent::SearchExhausted, detail), points: s.points, exhausted: true };
    }

    // Refine the bracketed interior maximum (ternary-style unimodal search) to within `tol`.
    let mut a = bracket_low;
    let mut b = best_c;
    let mut top = bracket_high;
    let mut best_value = best_v;
    while top.saturating_sub(a) > tol {
        let x = if b - a >= top - b { a + (b - a) / 2 } else { b + (top - b) / 2 };
        if x == a || x == b || x == top {
            break;
        }
        let midpoint_value = match s.eff(x) {
            Some(v) => v,
            None => return interrupted(s),
        };
        if x < b {
            if midpoint_value > best_value {
                top = b;
                b = x;
                best_value = midpoint_value;
            } else {
                a = x;
            }
        } else if midpoint_value > best_value {
            a = b;
            b = x;
            best_value = midpoint_value;
        } else {
            top = x;
        }
    }

    // The winner is the highest-value GATE-PASSING point across everything actually probed, read
    // back from the trace rather than trusted from the ramp/refine bookkeeping above (which tracks
    // `eff`, not `passed`): this is the same safety net the shell original used, and it is what
    // keeps a peak search from ever answering with a point whose gate failed.
    // Scanned in ASCENDING concurrency, not probe order, so an exact tie resolves to the LOWEST
    // concurrency and the answer does not depend on the path the search happened to walk. Integer
    // frame and request counts tie genuinely often on a saturated plateau.
    let mut ordered: Vec<&ProbedPoint> = s.points.iter().collect();
    ordered.sort_by_key(|p| p.concurrency);
    let mut winner: Option<PeakPoint> = None;
    for p in ordered {
        if p.passed && winner.as_ref().is_none_or(|w| p.value > w.value) {
            winner = Some(PeakPoint { concurrency: p.concurrency, value: p.value });
        }
    }
    match winner {
        Some(w) => PeakResult { peak: Measurement::Measured(w), points: s.points, exhausted: false },
        None => PeakResult {
            peak: Measurement::absent_because(Absent::NotMeasured, "no probed concurrency passed the gate"),
            points: s.points,
            exhausted: false,
        },
    }
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
            Some(Sample { value: c as f64, passed: c <= self.ceiling })
        }
    }

    #[test]
    fn bisect_finds_ceiling_between_rungs() {
        // The concrete shell fixture: sustained bisect lands on the true ceiling 1300, strictly
        // between the doubling rungs 1024 and 2048, over [8, 4096].
        let mut probe = MonotoneGate { ceiling: 1300 };
        let r = bisect_ceiling(&mut probe, 8, 4096);
        assert_eq!(r.ceiling, Measurement::Measured(1300));
        assert!(r.points.contains(&ProbedPoint { concurrency: 1301, passed: false, value: 1301.0 }));
    }

    #[test]
    fn bisect_bottom_already_failing_is_a_measured_zero() {
        let mut probe = MonotoneGate { ceiling: 0 };
        let r = bisect_ceiling(&mut probe, 1, 64);
        assert_eq!(r.ceiling, Measurement::Measured(0));
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
            Some(Sample { value: c as f64, passed: c <= 50 })
        }
    }

    // Interrupted BEFORE anything passed: genuinely unmeasured, so absent.
    #[test]
    fn bisect_interrupted_before_any_pass_is_unmeasured() {
        // fires_after: 0 means even the floor probe returns nothing.
        let mut probe = Interrupter { fires_after: 0, calls: 0 };
        let r = bisect_ceiling(&mut probe, 1, 1000);
        assert_eq!(r.ceiling.copied(), None);
        assert_eq!(r.ceiling.reason(), Some(&Absent::NotMeasured));
    }

    // Interrupted AFTER a confirmed pass: the shell keeps that partial answer on purpose, because
    // "every later abort still leaves a genuinely measured, if partial, answer behind". Reporting a
    // rung we watched sustain the gate is not fabrication; nulling it would claim we never measured
    // a cell we demonstrably did. This test previously asserted the opposite and was wrong.
    #[test]
    fn bisect_interrupted_after_a_pass_keeps_the_confirmed_rung() {
        let mut probe = Interrupter { fires_after: 1, calls: 0 };
        let r = bisect_ceiling(&mut probe, 1, 1000);
        assert_eq!(r.ceiling.copied(), Some(1), "c=1 was watched to pass before the interruption");
        assert!(r.points.iter().any(|p| p.concurrency == 1 && p.passed));
    }

    // The partial answer is still a LOWER BOUND, never the search range: an interrupted run must
    // not report the top of the range it never confirmed.
    #[test]
    fn an_interrupted_bisect_never_reports_the_unconfirmed_top() {
        let mut probe = Interrupter { fires_after: 1, calls: 0 };
        let r = bisect_ceiling(&mut probe, 1, 1000);
        assert_ne!(r.ceiling.copied(), Some(1000));
    }

    // ── peak_max ────────────────────────────────────────────────────────────────────────────────

    struct Unimodal {
        peak_c: u32,
        peak_v: f64,
        width: f64,
    }
    impl Probe for Unimodal {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            let d = (c as f64 - self.peak_c as f64) / self.width;
            let v = (self.peak_v - d * d).max(0.0);
            Some(Sample { value: v, passed: true })
        }
    }

    #[test]
    fn peak_finds_true_maximum_between_doublings() {
        // The concrete shell fixture: cpu-fps peak sits at 768, strictly between the doublings 512
        // and 1024, over [8, 8192].
        let mut probe = Unimodal { peak_c: 768, peak_v: 48_000.0, width: 8.0 };
        let r = peak_max(&mut probe, 8, 8192, 256, 4);
        assert!(!r.exhausted);
        assert!(r.peak.is_measured(), "expected a measured peak");
        let w = match r.peak.value().cloned() {
            Some(w) => w,
            None => return,
        };
        assert!((640..=900).contains(&w.concurrency), "got c={}", w.concurrency);
        assert!(w.value >= 46_000.0, "got value={}", w.value);
    }

    struct MonotoneRising;
    impl Probe for MonotoneRising {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            Some(Sample { value: c as f64, passed: true })
        }
    }

    /// A gate that passes ONLY at the very bottom of the range, with a start well above it that
    /// fails. This is the case the `while c > min_conc` guard could never reach: it stepped down to `min_conc`
    /// and then exited before probing it.
    struct PassesOnlyAtFloor {
        floor: u32,
    }
    impl Probe for PassesOnlyAtFloor {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            let passed = c <= self.floor;
            Some(Sample { value: if passed { 100.0 - c as f64 } else { 0.0 }, passed })
        }
    }

    // RED-BEFORE. The down-ramp is entered when the start fails the gate, and its whole purpose is
    // to reopen the bracket all the way to `min_conc`. With the old guard it never probed `min_conc`, so a
    // gateway whose only passing region sat at the floor was reported as passing nowhere: a claim
    // about a region the search had not looked at.
    #[test]
    fn the_down_ramp_probes_the_floor_it_exists_to_reopen() {
        let mut p = PassesOnlyAtFloor { floor: 8 };
        let r = peak_max(&mut p, 8, 1000, 512, 4);
        assert!(
            r.points.iter().any(|pt| pt.concurrency == 8),
            "the floor must actually be probed, probed set: {:?}",
            r.points.iter().map(|pt| pt.concurrency).collect::<Vec<_>>()
        );
        assert!(r.peak.is_measured(), "a real passing region must not be reported as passing nowhere");
    }

    #[test]
    fn peak_monotone_rising_is_exhausted_never_publishes_the_bound() {
        // The real field case: 8 -> 7442, doubling to 512 -> 334838, never turning over.
        let mut probe = MonotoneRising;
        let r = peak_max(&mut probe, 8, 512, 8, 4);
        assert!(r.exhausted);
        assert_eq!(r.peak.copied(), None);
        assert_eq!(r.peak.reason(), Some(&Absent::SearchExhausted));
    }

    struct MonotoneFalling {
        top: u32,
    }
    impl Probe for MonotoneFalling {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            Some(Sample { value: (self.top - c) as f64, passed: true })
        }
    }

    #[test]
    fn peak_monotone_falling_is_exhausted_the_mirrored_case() {
        let mut probe = MonotoneFalling { top: 100_000 };
        let r = peak_max(&mut probe, 8, 512, 512, 4);
        assert!(r.exhausted);
        assert_eq!(r.peak.copied(), None);
    }

    #[test]
    fn peak_probe_none_never_fabricates_a_number() {
        let mut probe = Interrupter { fires_after: 1, calls: 0 };
        let r = peak_max(&mut probe, 1, 1000, 100, 4);
        assert_eq!(r.peak.copied(), None);
        assert_eq!(r.peak.reason(), Some(&Absent::NotMeasured));
    }

    #[test]
    fn peak_below_start_is_found_by_ramping_down() {
        let mut probe = Unimodal { peak_c: 64, peak_v: 30_000.0, width: 2.0 };
        let r = peak_max(&mut probe, 8, 8192, 256, 4);
        assert!(r.peak.is_measured(), "expected a measured peak");
        let w = match r.peak.value().cloned() {
            Some(w) => w,
            None => return,
        };
        assert!((32..=128).contains(&w.concurrency), "got c={}", w.concurrency);
    }
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
            Some(Sample { value: c as f64, passed: c <= self.ceiling })
        }
    }

    struct AlwaysPasses;
    impl Probe for AlwaysPasses {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            Some(Sample { value: c as f64, passed: true })
        }
    }

    struct Unimodal {
        peak_c: u32,
        peak_v: f64,
        width: f64,
    }
    impl Probe for Unimodal {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            // Deliberately UNCLAMPED: a curve that flattens to 0 far from the peak has no gradient
            // for a local hill-climb to follow, which is a property of that curve, not a defect in
            // the search. A genuine single-humped parabola keeps a gradient everywhere.
            let d = (c as f64 - self.peak_c as f64) / self.width;
            let v = self.peak_v - d * d;
            Some(Sample { value: v, passed: true })
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

        // A unimodal curve with its maximum strictly inside [min_conc, max_conc] is found within a documented
        // tolerance, regardless of which side of `start` it sits on, and is never flagged exhausted.
        //
        // `peak_c` is capped well short of `max_conc`: a doubling ramp only ever samples two points per
        // step, so with the peak too close to the range edge the endpoint sample can still read
        // higher than the previous rung even though the true peak (unsampled, between the two) has
        // already passed. That is a real property of doubling search, ported faithfully from the
        // shell original, not a defect the port introduces; the shell's own tests give the same
        // headroom (peaks at ~3000 against ranges up to 65536).
        #[test]
        fn peak_finds_any_interior_maximum(
            peak_c in 200u32..6_000u32,
            start in 200u32..9_800u32,
        ) {
            let mut probe = Unimodal { peak_c, peak_v: 100_000.0, width: 16.0 };
            let r = peak_max(&mut probe, 1, 10_000, start, 8);
            prop_assert!(!r.exhausted);
            let w = r.peak.value();
            prop_assert!(w.is_some());
            if let Some(w) = w {
                // Tolerance: the refine bracket is narrowed to `tol` concurrency units either side
                // of the true peak, plus the quadratic curve's own flatness near the top; 32 units
                // comfortably covers both for this width.
                let dist = w.concurrency.abs_diff(peak_c);
                prop_assert!(dist <= 32, "peak_c={} winner={} dist={}", peak_c, w.concurrency, dist);
            }
        }

        // A probe that returns None partway never produces a fabricated number, for either search.
        #[test]
        fn none_partway_never_fabricates_a_number(fires_after in 0u32..5u32) {
            struct Interrupter { fires_after: u32, calls: u32 }
            impl Probe for Interrupter {
                fn probe(&mut self, c: u32) -> Option<Sample> {
                    self.calls += 1;
                    if self.calls > self.fires_after { return None; }
                    Some(Sample { value: c as f64, passed: c <= 50 })
                }
            }
            let mut p1 = Interrupter { fires_after, calls: 0 };
            let br = bisect_ceiling(&mut p1, 1, 1000);
            if br.ceiling.reason() == Some(&Absent::NotMeasured) {
                prop_assert_eq!(br.ceiling.copied(), None);
            }

            let mut p2 = Interrupter { fires_after, calls: 0 };
            let best_value = peak_max(&mut p2, 1, 1000, 100, 4);
            if best_value.peak.reason() == Some(&Absent::NotMeasured) {
                prop_assert_eq!(best_value.peak.copied(), None);
            }
        }
    }
}
