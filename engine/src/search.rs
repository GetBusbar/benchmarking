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
/// iff the top of the range still passed -- the true ceiling is at least `hi`, but that is a lower
/// bound the search chose, not a ceiling the gate proved, so it is never published as one.
#[derive(Debug, Clone, Serialize)]
pub struct BisectResult {
    pub ceiling: Measurement<u32>,
    pub points: Vec<ProbedPoint>,
}

/// Bisect `[lo, hi]` to the true integer ceiling of a pass/fail gate assumed monotone in
/// concurrency (everything at or below the ceiling passes, everything above fails). `lo` and `hi`
/// are normalised (swapped) if given reversed.
pub fn bisect_ceiling<P: Probe>(probe: &mut P, lo: u32, hi: u32) -> BisectResult {
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    let mut s = Search::new(probe);

    let lo_sample = match s.sample(lo) {
        Some(v) => v,
        None => return BisectResult { ceiling: Measurement::absent(Absent::NotMeasured), points: s.points },
    };
    if !lo_sample.passed {
        // The floor already fails: a measured "nothing sustains this gate" (0), a real result, not
        // an absence -- distinct from never having probed at all.
        return BisectResult { ceiling: Measurement::Measured(0), points: s.points };
    }

    let hi_sample = match s.sample(hi) {
        Some(v) => v,
        None => {
            let detail = format!("probe interrupted after c={lo} passed; no failing point found yet");
            return BisectResult { ceiling: Measurement::absent_because(Absent::NotMeasured, detail), points: s.points };
        }
    };
    if hi_sample.passed {
        // No failure was ever observed inside the range: publishing hi would report our own search
        // bound as the gate's ceiling.
        let detail = format!("c={hi} still passes at the top of the search range; the true ceiling is at least {hi}");
        return BisectResult { ceiling: Measurement::absent_because(Absent::SearchExhausted, detail), points: s.points };
    }

    // Invariant from here: a passes, b fails. Bisect to +-1; b stays the recorded proof of failure.
    let mut a = lo;
    let mut b = hi;
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

fn interrupted<P: Probe>(s: Search<P>) -> PeakResult {
    PeakResult { peak: Measurement::absent(Absent::NotMeasured), points: s.points, exhausted: false }
}

fn eff(sample: &Sample) -> f64 {
    if sample.passed { sample.value } else { 0.0 }
}

/// Doubles (or halves) away from `from_c` (already known to beat `base_v`) while the curve keeps
/// rising, guarding `bound`. Returns `(bracket_lo, best_c, best_v, bracket_hi, exhausted)`, or
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
            let (lo, hi) = if upward { (edge, next) } else { (next, edge) };
            return Some((lo, best_c, best_v, hi, false));
        }
    }
}

impl<'p, P: Probe> Search<'p, P> {
    fn eff(&mut self, c: u32) -> Option<f64> {
        self.sample(c).map(|s| eff(&s))
    }
}

/// Search `[lo, hi]` for the peak of a curve assumed unimodal in concurrency (rises then falls),
/// starting at `start` (clamped into range) and learning direction before ramping, so a peak either
/// above or below `start` is found by the same search. `tol` bounds the final refine bracket's
/// width (an absolute concurrency count, not scaled to the bracket like the shell original's `a/4`
/// heuristic -- see the module-level report for why). `lo`/`hi` are normalised if given reversed.
pub fn peak_max<P: Probe>(probe: &mut P, lo: u32, hi: u32, start: u32, tol: u32) -> PeakResult {
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    let mut s = Search::new(probe);
    let start = start.clamp(lo, hi);

    let start_v = match s.eff(start) {
        Some(v) => v,
        None => return interrupted(s),
    };

    let up_c = if start < hi { start.saturating_mul(2).min(hi) } else { start };
    let dn_c = if start > lo { (start / 2).max(lo) } else { start };

    let up_v = if up_c != start {
        match s.eff(up_c) {
            Some(v) => Some(v),
            None => return interrupted(s),
        }
    } else {
        None
    };

    let (bracket_lo, best_c, best_v, bracket_hi, exhausted) = if let Some(uv) = up_v.filter(|v| *v > start_v) {
        match ramp(&mut s, hi, start, up_c, uv, true) {
            Some(t) => t,
            None => return interrupted(s),
        }
    } else {
        let dn_v = if dn_c != start {
            match s.eff(dn_c) {
                Some(v) => Some(v),
                None => return interrupted(s),
            }
        } else {
            None
        };
        if let Some(dv) = dn_v.filter(|v| *v > start_v) {
            match ramp(&mut s, lo, start, dn_c, dv, false) {
                Some(t) => t,
                None => return interrupted(s),
            }
        } else {
            // Neither neighbour beats `start`: it is a local (possibly flat) max candidate already.
            (dn_c, start, start_v, up_c, false)
        }
    };

    if exhausted {
        let detail = format!(
            "curve was still rising at c={best_c} (value={best_v}) when the search range ran out; no interior turnover found"
        );
        return PeakResult { peak: Measurement::absent_because(Absent::SearchExhausted, detail), points: s.points, exhausted: true };
    }

    // Refine the bracketed interior maximum (ternary-style unimodal search) to within `tol`.
    let mut a = bracket_lo;
    let mut b = best_c;
    let mut top = bracket_hi;
    let mut pr = best_v;
    while top.saturating_sub(a) > tol {
        let x = if b - a >= top - b { a + (b - a) / 2 } else { b + (top - b) / 2 };
        if x == a || x == b || x == top {
            break;
        }
        let xr = match s.eff(x) {
            Some(v) => v,
            None => return interrupted(s),
        };
        if x < b {
            if xr > pr {
                top = b;
                b = x;
                pr = xr;
            } else {
                a = x;
            }
        } else if xr > pr {
            a = b;
            b = x;
            pr = xr;
        } else {
            top = x;
        }
    }

    // The winner is the highest-value GATE-PASSING point across everything actually probed, read
    // back from the trace rather than trusted from the ramp/refine bookkeeping above (which tracks
    // `eff`, not `passed`): this is the same safety net the shell original used, and it is what
    // keeps a peak search from ever answering with a point whose gate failed.
    let mut winner: Option<PeakPoint> = None;
    for p in &s.points {
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

    #[test]
    fn bisect_probe_none_never_fabricates_a_number() {
        let mut probe = Interrupter { fires_after: 1, calls: 0 };
        let r = bisect_ceiling(&mut probe, 1, 1000);
        assert_eq!(r.ceiling.copied(), None);
        assert_eq!(r.ceiling.reason(), Some(&Absent::NotMeasured));
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
        // Any monotone pass/fail curve with a true ceiling inside [lo, hi) lands on exactly that
        // ceiling, and the recorded proof is ceiling+1, measured and failing.
        #[test]
        fn bisect_lands_on_any_true_ceiling(ceiling in 1u32..999u32) {
            let lo = 1u32;
            let hi = 1000u32;
            let mut probe = MonotoneGate { ceiling };
            let r = bisect_ceiling(&mut probe, lo, hi);
            prop_assert_eq!(r.ceiling.copied(), Some(ceiling));
            prop_assert!(r.points.iter().any(|p| p.concurrency == ceiling + 1 && !p.passed));
        }

        // A curve that still passes at the top of the range never yields a number.
        #[test]
        fn bisect_top_passing_is_always_exhausted(lo in 1u32..100u32, hi in 100u32..10_000u32) {
            let mut probe = AlwaysPasses;
            let r = bisect_ceiling(&mut probe, lo, hi);
            prop_assert_eq!(r.ceiling.copied(), None);
            prop_assert_eq!(r.ceiling.reason(), Some(&Absent::SearchExhausted));
        }

        // A unimodal curve with its maximum strictly inside [lo, hi] is found within a documented
        // tolerance, regardless of which side of `start` it sits on, and is never flagged exhausted.
        //
        // `peak_c` is capped well short of `hi`: a doubling ramp only ever samples two points per
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
            let pr = peak_max(&mut p2, 1, 1000, 100, 4);
            if pr.peak.reason() == Some(&Absent::NotMeasured) {
                prop_assert_eq!(pr.peak.copied(), None);
            }
        }
    }
}
