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
// a level instead of falling away from a summit. `saturation_plateau` measures every rung the same
// way - the same windows, the same median, the same spread - and stops when two consecutive rungs
// stop buying more than that rung's own measured wobble.
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

    /// A DELIBERATE re-probe of a concurrency, bypassing the memo.
    ///
    /// Used only to calibrate this rig's own measurement noise, where the whole point is that
    /// identical conditions do NOT produce identical numbers - something the cache hides by
    /// construction, since it would hand back the first window's answer and report a spread of zero.
    /// Each repeat is recorded as its own point, so the published sweep carries the evidence the
    /// noise floor was derived from rather than asking a reader to take it on trust.
    fn sample_repeat(&mut self, c: u32) -> Option<Sample> {
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
        // A FAILING FLOOR ONLY PROVES THE CEILING IS BELOW THE FLOOR.
        //
        // When the floor is 1 there is nowhere left to look, so nothing sustains the gate and 0 is a
        // real measured result. For any higher floor it proves only that the ceiling lies somewhere
        // in [0, min_conc-1], and not one of those values was probed. Returning 0 there would
        // publish a specific number the search never established, which is the same fabrication as
        // publishing the range bound at the top end, just at the other end of the range.
        return if min_conc <= 1 {
            BisectResult { ceiling: Measurement::Measured(0), points: s.points }
        } else {
            let detail = format!(
                "the search floor c={min_conc} already failed the gate, so the ceiling is below it and was never probed"
            );
            BisectResult { ceiling: Measurement::absent_because(Absent::SearchExhausted, detail), points: s.points }
        };
    }

    let hi_sample = match s.sample(max_conc) {
        Some(v) => v,
        None => {
            // A LOWER BOUND IS NOT A CEILING, and at this point the only successful probe is the
            // floor itself, so returning it would publish the harness's own search floor as the
            // gateway's ceiling. Nothing is lost by refusing: `points` carries every probed rung on
            // this path exactly as it does on the measured ones, and the detail states the bound in
            // prose. `Measured` is not a neutral container, it is a publication claim: the board
            // renders it as a bare rankable number with no "at least" form.
            let detail = format!(
                "probe interrupted after c={min_conc} passed and before any failure was measured; the ceiling is at least {min_conc}, and nothing above it was tested"
            );
            return BisectResult { ceiling: Measurement::absent_because(Absent::NotMeasured, detail), points: s.points };
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
/// landed must NOT throw away what was measured: once a rung has been judged, every later abort
/// still leaves a genuinely measured, if partial, answer behind. Discarding it publishes null for a
/// cell we did in fact measure, which is the same class of loss as publishing a zero for one we did
/// not.
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
    // An interruption before a turnover was observed leaves a LOWER BOUND, not a peak, and
    // `exhausted: false` would additionally assert that a proven interior maximum was found. The
    // best passing point travels as evidence in the detail rather than as the value.
    let detail = match winner {
        Some(w) => format!(
            "probe interrupted; the best passing point seen was c={} at value={}, but the curve was never observed to turn over, so this is a lower bound rather than a peak",
            w.concurrency, w.value
        ),
        None => "the search was interrupted before any concurrency passed the gate".to_string(),
    };
    PeakResult { peak: Measurement::absent_because(Absent::NotMeasured, detail), points: s.points, exhausted: false }
}

/// Windows taken at every rung. Three is the smallest sample with a middle value, and the median of
/// three is what makes a rung's number resistant to one unlucky window.
pub const WINDOWS_PER_RUNG: usize = 3;

/// A floor under the measured wobble. Three windows can agree closely by luck, and a threshold near
/// zero would let any flutter read as a real gain.
const WOBBLE_FLOOR: f64 = 0.02;

/// How many consecutive rungs must fail to improve before the curve is called saturated. One flat
/// rung can be a downward draw; two in a row is the curve.
const FLAT_RUNGS_TO_STOP: usize = 2;

/// The lowest rung at which "more concurrency does not help" is a credible thing to conclude.
/// Saturation at c=1 or c=2 would be decided by the noisiest windows the harness takes - one
/// connection's serial rate, nothing averaging its variance.
const MIN_SATURATION_CONC: u32 = 16;

/// The relative spread across repeated windows at one rung: how far the answer moves when nothing
/// about the question changed. `(max - min) / max`, so it is a fraction of the value being compared
/// against rather than an absolute rate meaning different things at 40 and at 40,000.
fn relative_spread(v: &[f64]) -> f64 {
    let max = v.iter().copied().fold(f64::MIN, f64::max);
    let min = v.iter().copied().fold(f64::MAX, f64::min);
    if max <= 0.0 {
        return 0.0;
    }
    (max - min) / max
}

/// Nearest-rank p50 over a sorted slice: the SAME convention `gen::GenStats::pct_of` uses for the
/// published latency percentiles, and it returns a value some window actually produced rather than
/// the average of two that none did.
pub fn nearest_rank_median(sorted: &[f64]) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let mut i = (sorted.len() as f64 * 0.5) as usize;
    if i >= sorted.len() {
        i = sorted.len() - 1;
    }
    Some(sorted[i])
}

/// One rung, measured: its median throughput and the spread of the windows behind it.
struct Rung {
    concurrency: u32,
    median: f64,
    spread: f64,
    /// How many windows actually passed and went into the median. The bar below divides by its root,
    /// so a rung that lost windows to failures is judged on the evidence it really has.
    windows: usize,
}

/// THE BAR A RUNG MUST CLEAR TO COUNT AS AN IMPROVEMENT, and the half-width of the plateau band.
///
/// `spread` is the range of INDIVIDUAL windows, and the thing being compared is the MEDIAN of them.
/// Those are not the same quantity: the median of several windows is far steadier than the gap
/// between the luckiest and unluckiest of them, so charging the median the full window range asks a
/// climbing curve to beat noise it does not have. A cell whose windows scatter reads as saturated
/// while it is still climbing, and the ladder stops early with the gateway's real ceiling above it.
///
/// kong openai>openai measured exactly that: at c=16 the windows ran 19837..24740 (a 19.8% range)
/// while the median rose 18819 -> 21065, a real 11.9% gain that the raw range refused. The ladder
/// stopped at c=32 and published 20,871 as a MAXIMUM, and the sustained-throughput leg then found
/// 26,098 at c=131 - a rung this search never sampled. A maximum another measurement beats on the
/// same box against the same mock is not a maximum, which is what C6 refuses to publish.
///
/// Dividing by the root of the window count is the standard shape for the uncertainty of an estimate
/// against the scatter of its samples. It stays conservative: still floored, and a rung must still
/// beat it outright.
fn improvement_bar(spread: f64, windows: usize) -> f64 {
    let n = windows.max(1) as f64;
    (spread / n.sqrt()).max(WOBBLE_FLOOR)
}

/// Measure one rung properly: `WINDOWS_PER_RUNG` windows, median of the ones that passed their gate.
///
/// A window that FAILED its gate measured no throughput, so it is excluded from both the median and
/// the spread - it is still recorded in `points` as evidence about the rung. Letting one in made the
/// spread ~100% and froze an earlier version of this search near the floor.
fn measure_rung<P: Probe>(s: &mut Search<P>, c: u32) -> Option<Rung> {
    let mut vals = Vec::with_capacity(WINDOWS_PER_RUNG);
    for i in 0..WINDOWS_PER_RUNG {
        // The first window may come from the memo; the repeats must not, since the whole point is
        // that identical conditions produce different numbers.
        let sample = if i == 0 { s.sample(c)? } else { s.sample_repeat(c)? };
        if sample.passed {
            vals.push(sample.value);
        }
    }
    if vals.is_empty() {
        return Some(Rung { concurrency: c, median: 0.0, spread: 0.0, windows: 0 });
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(Rung {
        concurrency: c,
        median: nearest_rank_median(&vals).unwrap_or(0.0),
        spread: if vals.len() >= 2 { relative_spread(&vals) } else { 0.0 },
        windows: vals.len(),
    })
}

/// Find where throughput SATURATES, and report the plateau it settles on.
///
/// THROUGHPUT AGAINST CONCURRENCY IS A PLATEAU, NOT A BELL CURVE. A proxy climbs while it is
/// latency-bound, reaches a knee when it saturates, and then holds flat: past saturation more
/// concurrency buys queueing, not throughput (Little's Law). A healthy gateway never turns over, so
/// a search demanding a fall-off before it will believe a number will never believe a good gateway.
///
/// THE SHAPE OF THIS SEARCH IS DELIBERATELY BORING. Every rung is measured the same way - the same
/// number of windows, the same median, the same spread - and the stopping rule reads off those
/// numbers and nothing else. An earlier version had a cheap single-window fast path, a separate
/// calibration step, an escalation to medians, and a confirm step, each updating the running best
/// differently. It was faster on paper and it was wrong on real gateways four times in a row, in
/// ways its author could not trace by reading it. Uniform measurement costs a few more windows and
/// buys a search whose behaviour can be predicted from the curve alone.
///
/// The reported value is the plateau's MEDIAN rung, not its best: on a plateau the rungs differ only
/// by luck, so publishing the best hands the win to whichever gateway drew the kindest window. The
/// reported concurrency is the KNEE - the lowest rung that reached the plateau - which is the answer
/// to "how much concurrency do I need before more stops helping".
///
/// `min_conc`/`max_conc` are normalised if given reversed, and `min_conc` is floored at 1 (there
/// is no such thing as zero concurrency).
pub fn saturation_plateau<P: Probe>(probe: &mut P, min_conc: u32, max_conc: u32) -> PeakResult {
    let (min_conc, max_conc) = if min_conc <= max_conc { (min_conc, max_conc) } else { (max_conc, min_conc) };
    // ZERO CONCURRENCY DOES NOT EXIST. The climb step is `c.saturating_mul(2)`, and doubling zero is
    // still zero, so a caller-supplied floor of 0 (e.g. `OTB_MIN_CONC=0`) pinned the ladder at c=0
    // forever instead of climbing.
    let min_conc = min_conc.max(1);
    let mut s = Search::new(probe);

    // CLIMB FROM THE FLOOR, ALWAYS. The start is not derived from the range: a start that moves with
    // the bound makes the ladder arbitrary and turns a wider range into a more dangerous first probe
    // - which is how a 1..65536 run once opened by asking for 32768 concurrent connections. Starting
    // at the floor also means the published sweep shows the whole curve, rise and plateau both, so a
    // reader can see the knee rather than being told it.
    let mut rungs: Vec<Rung> = Vec::new();
    let mut c = min_conc;
    let mut flat_run = 0usize;
    let mut hit_bound = false;

    loop {
        let rung = match measure_rung(&mut s, c) {
            Some(r) => r,
            None => return interrupted(s),
        };
        let best_so_far = rungs.iter().map(|r| r.median).fold(0.0_f64, f64::max);
        // The bar is this rung's own measured wobble, floored. Judging a rung against the noise of a
        // DIFFERENT rung is what let a noisy floor set an impossible bar for the whole ladder.
        let wobble = improvement_bar(rung.spread, rung.windows);
        let improved = rung.median > best_so_far * (1.0 + wobble);
        rungs.push(rung);

        if improved {
            flat_run = 0;
        } else {
            flat_run += 1;
        }

        if c >= max_conc {
            hit_bound = true;
            break;
        }
        // Saturation needs consecutive flat rungs AND a rung high enough for "more does not help" to
        // mean anything.
        if flat_run >= FLAT_RUNGS_TO_STOP && c >= MIN_SATURATION_CONC {
            break;
        }
        c = c.saturating_mul(2).min(max_conc);
    }

    let best = rungs.iter().map(|r| r.median).fold(0.0_f64, f64::max);
    if best <= 0.0 {
        let detail = format!(
            "no concurrency from {min_conc} to {max_conc} passed the gate, so no throughput was established at any rung"
        );
        return PeakResult { peak: Measurement::absent_because(Absent::NotMeasured, detail), points: s.points, exhausted: false };
    }

    // STILL CLIMBING AT THE BOUND is a lower bound, not a plateau. The range is our choice; reporting
    // it as the gateway's ceiling would be publishing our own search bound as its answer.
    if hit_bound && flat_run < FLAT_RUNGS_TO_STOP {
        let top_wobble = rungs.last().map(|r| improvement_bar(r.spread, r.windows)).unwrap_or(WOBBLE_FLOOR);
        let detail = format!(
            "throughput was still climbing by more than the measured {:.1}% window-to-window wobble at c={max_conc} ({best:.0}) when the search range ran out, so saturation was never observed and no plateau was established",
            top_wobble * 100.0
        );
        return PeakResult { peak: Measurement::absent_because(Absent::SearchExhausted, detail), points: s.points, exhausted: true };
    }

    // THE PLATEAU is every rung within its own wobble of the best one. The knee is the lowest of
    // them: rungs from the climb sit well below and are excluded by the same comparison that decided
    // saturation.
    let band = rungs
        .iter()
        .filter(|r| r.median > 0.0 && r.median >= best * (1.0 - improvement_bar(r.spread, r.windows)))
        .collect::<Vec<_>>();
    let knee = band.iter().map(|r| r.concurrency).min().unwrap_or(min_conc);
    let mut vals: Vec<f64> = band.iter().map(|r| r.median).collect();
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let plateau = nearest_rank_median(&vals).unwrap_or(best);

    PeakResult {
        peak: Measurement::Measured(PeakPoint { concurrency: knee, value: plateau }),
        points: s.points,
        exhausted: false,
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

    // THE TIE-BREAK EXPERIMENT, kept as a test because it is the cheapest possible statement of the
    // rule. The gate is FIXED; only the harness's own search floor moves. If an interrupted search
    // published a confirmed rung, the answer would track the FLOOR (1, 8, 16, 64) and carry zero
    // bits about the gateway, which is precisely how unrelated gateways come to share a number.
    #[test]
    fn an_interrupted_search_never_publishes_the_harness_own_floor() {
        for floor in [1u32, 8, 16, 64] {
            let mut probe = Interrupter { fires_after: 1, calls: 0 };
            let r = bisect_ceiling(&mut probe, floor, 4096);
            assert_eq!(
                r.ceiling.copied(),
                None,
                "floor={floor} leaked into the published ceiling, which is a readout of our config"
            );
            // The measurement is NOT lost: every probed rung still travels.
            assert!(!r.points.is_empty(), "the probed trace must survive an absent verdict");
            assert!(
                r.ceiling.detail().unwrap_or_default().contains(&floor.to_string()),
                "the lower bound belongs in the evidence, not in the value"
            );
        }
    }

    // Two different gateways, same harness floor. If an interrupted search published a rung, both
    // would report the identical number despite behaving completely differently.
    #[test]
    fn two_unlike_gateways_do_not_collapse_to_the_same_interrupted_answer() {
        let mut slow = Interrupter { fires_after: 1, calls: 0 };
        let mut fast = Interrupter { fires_after: 1, calls: 0 };
        let a = bisect_ceiling(&mut slow, 8, 4096);
        let b = bisect_ceiling(&mut fast, 8, 4096);
        assert_eq!(a.ceiling.copied(), None);
        assert_eq!(b.ceiling.copied(), None);
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

    // Interrupted AFTER a confirmed pass must still report no ceiling, not the confirmed rung: the
    // probed trace survives on every path including this one, so nothing is discarded, and at this
    // point the only successful probe IS the floor, so returning it would publish our own search
    // configuration as the gateway's ceiling. The lower bound belongs in the evidence, not the value.
    #[test]
    fn bisect_interrupted_after_a_pass_is_absent_with_the_bound_as_evidence() {
        let mut probe = Interrupter { fires_after: 1, calls: 0 };
        let r = bisect_ceiling(&mut probe, 1, 1000);
        assert_eq!(r.ceiling.copied(), None, "no failure was measured, so no ceiling was proven");
        assert_eq!(r.ceiling.reason(), Some(&Absent::NotMeasured));
        assert!(r.points.iter().any(|p| p.concurrency == 1 && p.passed), "the pass still travels");
        assert!(r.ceiling.detail().unwrap_or_default().contains("at least 1"));
    }

    // The partial answer is still a LOWER BOUND, never the search range: an interrupted run must
    // not report the top of the range it never confirmed.
    #[test]
    fn an_interrupted_bisect_never_reports_the_unconfirmed_top() {
        let mut probe = Interrupter { fires_after: 1, calls: 0 };
        let r = bisect_ceiling(&mut probe, 1, 1000);
        assert_ne!(r.ceiling.copied(), Some(1000));
    }

    // A FAILING FLOOR ABOVE ONE PROVES ONLY THAT THE CEILING IS BELOW THE FLOOR, and not one value
    // in [0, floor-1] was probed. Returning 0 there publishes a specific number the search never
    // established, which is the same fabrication as publishing the range bound at the top end, just
    // at the other end of the range. The `Measured(0)` answer is reserved for a floor of one, where
    // there is genuinely nowhere left to look.
    #[test]
    fn a_failing_floor_above_one_is_absent_never_the_measured_zero_reserved_for_c_equals_one() {
        let mut probe = MonotoneGate { ceiling: 0 };
        let r = bisect_ceiling(&mut probe, 8, 4096);
        assert_eq!(r.ceiling.copied(), None, "no concurrency in [0, 7] was probed, so 0 was never measured");
        assert_eq!(r.ceiling.reason(), Some(&Absent::SearchExhausted));
        assert!(
            r.ceiling.detail().unwrap_or_default().contains("c=8"),
            "the reason must name the floor that failed, got {:?}",
            r.ceiling.detail()
        );
        // The refusal is about the FLOOR, not about the gate: with a floor of one the identical gate
        // yields a real measured zero, and the two answers must not be interchangeable.
        let mut same_gate = MonotoneGate { ceiling: 0 };
        assert_eq!(bisect_ceiling(&mut same_gate, 1, 4096).ceiling, Measurement::Measured(0));
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
        assert_eq!(b.ceiling.copied(), a.ceiling.copied(), "an inverted range must be normalised, not searched inverted");
    }

    // A RANGE OF ONE PROVES NOTHING ABOUT A CEILING. The single rung passed, and nothing above it
    // was ever probed, so the answer is a lower bound: publishing it would report the caller's own
    // one-point range as the gateway's ceiling.
    #[test]
    fn a_single_point_range_that_passes_is_exhausted_not_a_ceiling() {
        let mut probe = MonotoneGate { ceiling: 1000 };
        let r = bisect_ceiling(&mut probe, 64, 64);
        assert_eq!(r.ceiling.copied(), None, "one passing rung is a lower bound, not a proven ceiling");
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

    // ── saturation_plateau ──────────────────────────────────────────────────────────────────────

    /// A gateway: throughput climbs in proportion to concurrency until it saturates, then holds flat
    /// with a deterministic wobble. The wobble alternates sign per probe so a repeated rung really
    /// does return different numbers, which is what the calibration exists to discover.
    struct Saturating {
        knee: u32,
        plateau: f64,
        wobble: f64,
        calls: u32,
    }
    impl Probe for Saturating {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            self.calls += 1;
            // THREE levels, not two: with a two-valued wobble the plateau has an even number of
            // equal halves and a nearest-rank median always returns the upper one, so a median would
            // be indistinguishable from a maximum and the test could not tell them apart.
            let sign = match self.calls % 3 {
                0 => 1.0,
                1 => -1.0,
                _ => 0.0,
            };
            let level =
                if c >= self.knee { self.plateau } else { self.plateau * (c as f64 / self.knee as f64) };
            Some(Sample { value: level * (1.0 + sign * self.wobble), passed: true })
        }
    }

    // THE FIELD BUG, PINNED. A gateway that saturates early and then holds flat for many doublings
    // must be reported at its plateau, NOT walked to the top of the search range. The search this
    // replaced did exactly that: on the flat part it asked "is the next rung higher?", noise
    // answered yes, and it published the range bound as the gateway's maximum.
    #[test]
    fn a_flat_curve_saturates_and_never_walks_to_the_top_of_the_range() {
        let mut probe = Saturating { knee: 64, plateau: 6000.0, wobble: 0.01, calls: 0 };
        let r = saturation_plateau(&mut probe, 1, 4096);
        assert!(!r.exhausted, "a curve that plateaued must not report the range as exhausted");
        let w = r.peak.value().expect("a saturated curve has a plateau to publish");
        assert!(
            w.concurrency <= 256,
            "saturation is at c=64; reporting c={} means the search kept climbing on noise",
            w.concurrency
        );
        assert!(
            !r.points.iter().any(|p| p.concurrency == 4096),
            "the search reached the top of the range on a curve that stopped improving at c=64"
        );
    }

    // A ZERO FLOOR MUST NOT PIN THE LADDER AT ZERO. The climb step is `c.saturating_mul(2)`, and
    // doubling zero is still zero, so `min_conc: 0` (e.g. `OTB_MIN_CONC=0`) never advanced past c=0
    // and the search never reached `max_conc`, let alone a plateau. A probe budget stands in for a
    // wall clock here: it fails fast and deterministically instead of hanging the test suite the way
    // the real bug hangs `otb run`.
    struct CountBudgetedSaturating {
        knee: u32,
        plateau: f64,
        calls: u32,
        max_calls: u32,
    }
    impl Probe for CountBudgetedSaturating {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            self.calls += 1;
            assert!(
                self.calls <= self.max_calls,
                "saturation_plateau did not terminate: exceeded {} probes, still stuck at concurrency {}",
                self.max_calls,
                c
            );
            let level = if c >= self.knee {
                self.plateau
            } else {
                self.plateau * (c as f64 / self.knee as f64)
            };
            Some(Sample { value: level, passed: true })
        }
    }

    #[test]
    fn a_zero_floor_still_climbs_and_terminates() {
        let mut probe = CountBudgetedSaturating { knee: 64, plateau: 6000.0, calls: 0, max_calls: 200 };
        let r = saturation_plateau(&mut probe, 0, 4096);
        assert!(!r.exhausted, "a curve that plateaued must not report the range as exhausted");
        let w = r.peak.value().expect("a saturated curve has a plateau to publish");
        assert!(w.concurrency >= 1, "knee reported at c={}, which is not a real concurrency", w.concurrency);
    }

    // THE WHOLE CLIMB, FROM A REAL RUN THAT GOT IT WRONG.
    //
    // These are one entrant's actual recorded windows. The search that produced them published
    // 38 rps at c=16 while its own sweep, in the same run, measured 55-59 at c=64 - it stopped a
    // third of the way up its own curve. The rungs above are where that gateway actually settles.
    //
    // This is the case that retired the previous search. That one had a cheap single-window fast
    // path, a separate calibration step, an escalation to medians and a confirm step, each updating
    // the running best differently; it was mis-traced by its own author four times and shipped two
    // understated field runs. Uniform measurement at every rung costs more windows and is
    // predictable from the curve alone, which is the property that matters here.
    #[test]
    fn the_published_plateau_is_the_one_the_curve_actually_reaches() {
        let mut seq = std::collections::BTreeMap::new();
        seq.insert(1u32, vec![34.0, 29.0, 28.0]);
        seq.insert(2, vec![30.0, 29.0, 30.0]);
        seq.insert(4, vec![31.0, 31.0, 31.0]);
        seq.insert(8, vec![34.0, 34.0, 35.0]);
        seq.insert(16, vec![38.0, 39.0, 38.0]);
        seq.insert(32, vec![45.0, 47.0, 44.0]);
        seq.insert(64, vec![55.0, 56.0, 59.0]);
        // where it flattens
        seq.insert(128, vec![60.0, 61.0, 60.0]);
        seq.insert(256, vec![61.0, 60.0, 61.0]);
        seq.insert(512, vec![60.0, 61.0, 60.0]);
        seq.insert(1024, vec![61.0, 60.0, 61.0]);

        let mut p = ReplayWindows { seq, seen: Default::default() };
        let r = saturation_plateau(&mut p, 1, 4096);
        let w = r.peak.value().expect("a curve that flattens has a plateau");
        assert!(
            w.value >= 58.0,
            "published {} rps - the curve reaches 60 and this stopped short of it, which is what \
             the live run did at 38",
            w.value
        );
        assert!(
            w.concurrency >= 64,
            "knee reported at c={} - the curve is still climbing hard there",
            w.concurrency
        );
    }

    // SATURATION MUST NOT BE DECLARED FROM THE FLOOR.
    //
    // These are one entrant's EXACT recorded windows from a live run, replayed in the order its box
    // produced them. Rung one drew high (35) and its repeats came back low (30, 30), which does two
    // things at once: the high first window makes every later rung look like no improvement, and the
    // spread makes the bar 14% - wider than the ~10%-per-doubling this gateway actually gains. The
    // search concluded "more concurrency does not help" from the two noisiest rungs it will ever
    // measure and published a single connection's rate as the plateau: 33 rps at c=1.
    //
    // The same search runs on all thirteen entrants. A fast gateway escapes this because its early
    // doublings are steep enough to clear any bar; that is luck, not correctness, and it is why this
    // is pinned with the real numbers rather than a model of them.
    struct ReplayWindows {
        seq: std::collections::BTreeMap<u32, Vec<f64>>,
        seen: std::collections::BTreeMap<u32, usize>,
    }
    impl Probe for ReplayWindows {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            let i = self.seen.entry(c).or_insert(0);
            let n = *i;
            *i += 1;
            let xs = self.seq.get(&c)?;
            Some(Sample { value: xs[n.min(xs.len() - 1)], passed: true })
        }
    }

    #[test]
    fn saturation_is_never_concluded_from_the_floors_own_noise() {
        let mut seq = std::collections::BTreeMap::new();
        // The live windows, verbatim.
        seq.insert(1u32, vec![35.0, 30.0, 30.0]);
        seq.insert(2, vec![30.0, 33.0, 31.0]);
        seq.insert(4, vec![33.0, 34.0, 33.0]);
        // Where that same gateway went on an earlier run, when the search did keep climbing: it is
        // still gaining at every doubling out to c=32.
        seq.insert(8, vec![37.0, 37.0, 38.0]);
        seq.insert(16, vec![39.0, 40.0, 41.0]);
        seq.insert(32, vec![45.0, 46.0, 47.0]);
        seq.insert(64, vec![48.0, 49.0, 48.0]);
        seq.insert(128, vec![49.0, 48.0, 49.0]);
        seq.insert(256, vec![49.0, 48.0, 49.0]);
        seq.insert(512, vec![48.0, 49.0, 48.0]);
        seq.insert(1024, vec![49.0, 48.0, 49.0]);
        seq.insert(2048, vec![48.0, 49.0, 48.0]);
        seq.insert(4096, vec![49.0, 48.0, 49.0]);

        let mut p = ReplayWindows { seq, seen: Default::default() };
        let r = saturation_plateau(&mut p, 1, 4096);
        let w = r.peak.value().expect("a climbing curve has a plateau");
        assert!(
            w.concurrency > 1,
            "the search stopped on rung one and published {} rps - a single connection's rate, \
             decided by that rung's own scatter",
            w.value
        );
        assert!(
            w.value > 40.0,
            "published {} rps, but this gateway is measured going on to 47+ at higher rungs",
            w.value
        );
    }

    // A FAILING WINDOW DURING CALIBRATION MUST NOT FREEZE THE SEARCH.
    //
    // `eff` scores a failed window 0.0, so one of them inside the calibration sample makes the
    // spread ~100%, makes "materially better" mean "twice as fast", and nothing is ever twice as
    // fast as the rung below it. The search then stops on whatever rung it was standing on and
    // publishes a number from the bottom of the climb as the plateau.
    //
    // This is one entrant's REAL recorded windows: it does produce failing windows at c=1, and the
    // live run published it at 41 rps while its own sweep was still climbing through 47.
    struct FlakyFloor {
        seq: std::collections::BTreeMap<u32, Vec<f64>>,
        seen: std::collections::BTreeMap<u32, usize>,
    }
    impl Probe for FlakyFloor {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            let i = self.seen.entry(c).or_insert(0);
            let n = *i;
            *i += 1;
            let vals = self.seq.get(&c).cloned().unwrap_or_else(|| vec![60.0]);
            let v = vals[n.min(vals.len() - 1)];
            // The floor's SECOND window fails its gate, exactly as the field box produced.
            Some(Sample { value: v, passed: !(c == 1 && n == 1) })
        }
    }

    #[test]
    fn a_failing_window_during_calibration_does_not_freeze_the_climb() {
        let mut seq = std::collections::BTreeMap::new();
        seq.insert(1u32, vec![30.0, 29.0, 29.0]);
        seq.insert(2, vec![31.0, 31.0, 33.0]);
        seq.insert(4, vec![35.0, 34.0, 34.0]);
        seq.insert(8, vec![37.0, 37.0, 38.0]);
        seq.insert(16, vec![39.0, 40.0, 41.0]);
        seq.insert(32, vec![45.0, 46.0, 47.0]);
        seq.insert(64, vec![52.0, 52.0, 53.0]);
        seq.insert(128, vec![58.0, 58.0, 59.0]);
        let mut p = FlakyFloor { seq, seen: Default::default() };
        let r = saturation_plateau(&mut p, 1, 4096);
        let w = r.peak.value().expect("a climbing curve with one bad floor window still has a plateau");
        assert!(
            w.value > 50.0,
            "published {} - the search froze near the floor because a failed window poisoned the \
             wobble; this curve climbs to 60",
            w.value
        );
        assert!(w.concurrency >= 64, "reported c={} is still on the rising part", w.concurrency);
    }

    // THE FIELD FAILURE, REPRODUCED EXACTLY. A saturated gateway whose rungs DRIFT UPWARD inside the
    // noise band - each doubling reading a whisker higher than the last, none of it real - is what
    // walks a noise-blind search to the top of the range one honest-looking step at a time. This is
    // the adversarial case: it is flat in every way that matters and rising in the only way a
    // threshold of zero can see.
    struct DriftingPlateau {
        knee: u32,
        plateau: f64,
        /// Fraction added per doubling above the knee. Far below any real saturation step, so a
        /// search that follows it is following noise by construction.
        drift: f64,
    }
    impl Probe for DriftingPlateau {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            if c < self.knee {
                return Some(Sample { value: self.plateau * (c as f64 / self.knee as f64), passed: true });
            }
            let doublings = (c as f64 / self.knee as f64).log2().max(0.0);
            Some(Sample { value: self.plateau * (1.0 + self.drift * doublings), passed: true })
        }
    }

    #[test]
    fn a_plateau_that_drifts_upward_inside_the_noise_is_still_saturated() {
        // 0.4% per doubling: eleven doublings of it is under 5%, and no gateway "gains throughput"
        // that way. A search comparing against zero climbs every one of them.
        let mut probe = DriftingPlateau { knee: 32, plateau: 6000.0, drift: 0.004 };
        let r = saturation_plateau(&mut probe, 1, 65_536);
        assert!(!r.exhausted, "drift inside the noise band must not read as failing to saturate");
        let w = r.peak.value().expect("a drifting plateau is still a plateau");
        assert!(
            w.concurrency <= 512,
            "the search followed {:.1}%-per-doubling drift up to c={}; that is noise, not throughput",
            0.4,
            w.concurrency
        );
        assert!(
            !r.points.iter().any(|p| p.concurrency >= 32_768),
            "the search walked into the top of the range on a curve that stopped improving at c=32"
        );
    }

    // The published figure is the MEDIAN of the plateau, not the best rung on it. Taking the best
    // hands the win to whichever gateway drew the luckiest window, and on a plateau the rungs differ
    // only by noise, so "best" is a measure of luck rather than of the gateway.
    #[test]
    fn the_published_value_is_the_plateau_median_not_its_luckiest_rung() {
        let mut probe = Saturating { knee: 32, plateau: 1000.0, wobble: 0.05, calls: 0 };
        let r = saturation_plateau(&mut probe, 1, 2048);
        let w = r.peak.value().expect("saturated");
        let best_seen = r.points.iter().filter(|p| p.passed).map(|p| p.value).fold(f64::MIN, f64::max);
        assert!(
            w.value < best_seen,
            "published {} is the best rung seen ({}), so the luckiest window won",
            w.value,
            best_seen
        );
        // ... and it is still a real plateau figure, not something dragged down by the rising part.
        assert!(w.value > 1000.0 * 0.9, "published {} is far below the plateau level", w.value);
    }

    // The reported concurrency is the KNEE - the lowest rung that reached the plateau - because that
    // is the answer to "how much concurrency do I need before more stops helping". With a median
    // value there is no single winning rung for a summit to point at anyway.
    #[test]
    fn the_reported_concurrency_is_the_knee_not_the_highest_rung_probed() {
        let mut probe = Saturating { knee: 64, plateau: 5000.0, wobble: 0.01, calls: 0 };
        let r = saturation_plateau(&mut probe, 1, 4096);
        let w = r.peak.value().expect("saturated");
        let highest_probed = r.points.iter().map(|p| p.concurrency).max().unwrap_or(0);
        assert!(
            w.concurrency < highest_probed,
            "reported c={} equals the highest rung probed ({}), which is a summit, not a knee",
            w.concurrency,
            highest_probed
        );
    }

    // A curve that never stops climbing has no plateau, and the range bound is OUR choice, not the
    // gateway's ceiling. Publishing it would be the same fabrication at the other end of the search.
    #[test]
    fn a_curve_still_climbing_at_the_bound_is_exhausted_never_the_bound_itself() {
        struct Rising;
        impl Probe for Rising {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                Some(Sample { value: c as f64 * 100.0, passed: true })
            }
        }
        let r = saturation_plateau(&mut Rising, 1, 512);
        assert!(r.exhausted);
        assert_eq!(r.peak.value(), None, "a lower bound must never be published as a plateau");
        assert_eq!(r.peak.reason(), Some(&Absent::SearchExhausted));
        assert!(
            r.peak.detail().unwrap_or_default().contains("wobble"),
            "the refusal must state the threshold it was judged against: {:?}",
            r.peak.detail()
        );
    }

    // THE WOBBLE IS MEASURED, NOT ASSUMED. A rig whose repeated windows disagree by 10% must not
    // read a 10% flutter as a real climb; a search with a hardcoded tighter threshold would.
    #[test]
    fn a_noisy_rig_does_not_read_its_own_wobble_as_a_climb() {
        let mut probe = Saturating { knee: 16, plateau: 2000.0, wobble: 0.10, calls: 0 };
        let r = saturation_plateau(&mut probe, 1, 8192);
        assert!(!r.exhausted, "a flat-but-noisy curve must still saturate");
        let w = r.peak.value().expect("saturated");
        assert!(w.concurrency <= 512, "noise carried the search to c={}", w.concurrency);
    }

    // The calibration must actually re-probe: a memoised repeat would hand back the first window's
    // answer, report a spread of zero, and quietly restore the guessed-threshold bug.
    #[test]
    fn the_calibration_really_reprobes_rather_than_reading_the_memo() {
        let mut probe = Saturating { knee: 8, plateau: 900.0, wobble: 0.03, calls: 0 };
        let r = saturation_plateau(&mut probe, 1, 1024);
        assert!(r.peak.value().is_some());
        let mut seen = std::collections::BTreeMap::new();
        for p in &r.points {
            *seen.entry(p.concurrency).or_insert(0) += 1;
        }
        assert!(
            seen.values().any(|n| *n >= WINDOWS_PER_RUNG),
            "no concurrency was probed {WINDOWS_PER_RUNG} times, so the wobble was never measured: {seen:?}"
        );
    }

    // Nothing anywhere in the range passed its gate. That is a real, measured "this gateway served
    // nothing", and it must carry its reason rather than arriving as a bare null.
    #[test]
    fn a_gate_that_never_passes_is_unmeasured_with_its_reason() {
        struct NeverPasses;
        impl Probe for NeverPasses {
            fn probe(&mut self, _c: u32) -> Option<Sample> {
                Some(Sample { value: 0.0, passed: false })
            }
        }
        let r = saturation_plateau(&mut NeverPasses, 1, 64);
        assert_eq!(r.peak.value(), None);
        assert_eq!(r.peak.reason(), Some(&Absent::NotMeasured));
        assert!(r.peak.detail().unwrap_or_default().contains("passed the gate"));
    }

    // A stopped clock is not a measurement. Whatever was proved before it stopped may travel as
    // prose, but never as a number.
    #[test]
    fn an_interrupted_plateau_search_never_fabricates_a_number() {
        struct Interrupter {
            calls: u32,
        }
        impl Probe for Interrupter {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                self.calls += 1;
                if self.calls > 3 {
                    return None;
                }
                Some(Sample { value: c as f64 * 10.0, passed: true })
            }
        }
        let r = saturation_plateau(&mut Interrupter { calls: 0 }, 1, 4096);
        assert_eq!(r.peak.value(), None);
        assert!(!r.exhausted, "an interruption is not the same fact as running out of range");
        assert!(!r.points.is_empty(), "the probes that did land are still evidence");
    }

    // The range given backwards is the same interval, so it must produce the same answer rather than
    // silently searching nothing.
    #[test]
    fn a_plateau_range_given_backwards_searches_the_same_interval() {
        let mut a = Saturating { knee: 32, plateau: 4000.0, wobble: 0.01, calls: 0 };
        let mut b = Saturating { knee: 32, plateau: 4000.0, wobble: 0.01, calls: 0 };
        let fwd = saturation_plateau(&mut a, 1, 2048);
        let rev = saturation_plateau(&mut b, 2048, 1);
        assert_eq!(fwd.peak.value().map(|w| w.concurrency), rev.peak.value().map(|w| w.concurrency));
    }

    // THE START IS THE FLOOR, ALWAYS, AND DOES NOT MOVE WITH THE RANGE. A start derived from the
    // range is what made a 1..65536 run open by asking for 32768 concurrent connections, and it also
    // made every gateway's published evidence begin at a different, arbitrary place.
    #[test]
    fn the_search_always_opens_at_the_floor_however_wide_the_range() {
        for hi in [64u32, 4096, 65536] {
            let mut probe = Saturating { knee: 16, plateau: 700.0, wobble: 0.01, calls: 0 };
            let r = saturation_plateau(&mut probe, 1, hi);
            let first = r.points.first().map(|p| p.concurrency);
            assert_eq!(first, Some(1), "with hi={hi} the search opened at {first:?} instead of the floor");
        }
    }

    // Every probed rung travels, in probe order, whichever way the search went: the published sweep
    // is what lets a reader re-derive the plateau instead of trusting it.
    #[test]
    fn the_probe_trace_travels_with_the_result() {
        let mut probe = Saturating { knee: 64, plateau: 3000.0, wobble: 0.02, calls: 0 };
        let r = saturation_plateau(&mut probe, 1, 4096);
        assert!(r.points.len() > 4, "too few points to re-derive anything: {:?}", r.points.len());
        assert!(r.points.iter().all(|p| p.concurrency >= 1));
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

    /// A saturating curve: proportional to concurrency below the knee, flat above it, with a
    /// deterministic wobble that alternates sign so repeated windows really do disagree.
    struct Saturating {
        knee: u32,
        plateau: f64,
        wobble: f64,
        calls: u32,
    }
    impl Probe for Saturating {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            self.calls += 1;
            // THREE levels, not two: with a two-valued wobble the plateau has an even number of
            // equal halves and a nearest-rank median always returns the upper one, so a median would
            // be indistinguishable from a maximum and the test could not tell them apart.
            let sign = match self.calls % 3 {
                0 => 1.0,
                1 => -1.0,
                _ => 0.0,
            };
            let level =
                if c >= self.knee { self.plateau } else { self.plateau * (c as f64 / self.knee as f64) };
            Some(Sample { value: level * (1.0 + sign * self.wobble), passed: true })
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

        // ANY saturating curve whose knee sits inside the range is reported at its plateau, never
        // walked to the top of the range, and never flagged exhausted. The wobble is varied too:
        // the threshold is measured per run, so a noisier rig must not change the verdict, only the
        // precision of where it lands.
        #[test]
        fn a_saturating_curve_is_always_reported_at_its_plateau(
            knee in 8u32..2_000u32,
            wobble in 0.0f64..0.08f64,
        ) {
            let plateau = 50_000.0;
            let mut probe = Saturating { knee, plateau, wobble, calls: 0 };
            let r = saturation_plateau(&mut probe, 1, 65_536);
            prop_assert!(!r.exhausted, "a curve that flattens must never read as still climbing");
            let w = r.peak.value();
            prop_assert!(w.is_some());
            if let Some(w) = w {
                // The published figure is a plateau rung, so it sits within the wobble of the real
                // plateau level rather than anywhere on the rising part below it.
                let off = (w.value - plateau).abs() / plateau;
                prop_assert!(off <= wobble + 0.01, "value {} is {:.3} off the plateau {}", w.value, off, plateau);
                // The knee is reported, so the concurrency is at or above the true knee but nowhere
                // near the top of a range 30+ doublings wide.
                prop_assert!(w.concurrency <= knee.saturating_mul(8).max(64),
                    "knee={} reported c={}", knee, w.concurrency);
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
            let best_value = saturation_plateau(&mut p2, 1, 1000);
            if best_value.peak.reason() == Some(&Absent::NotMeasured) {
                prop_assert_eq!(best_value.peak.copied(), None);
            }
        }
    }

    // ── the improvement bar: a climbing curve must not read as saturated ────────────────────────
    //
    // REPLAY OF A REAL FIELD FAILURE. These are kong openai>openai's own recorded windows from the
    // 2026-07-28 field run, in the order the rig took them. The published result was a MAXIMUM of
    // 20,871 rps, and the sustained-throughput leg on the same box against the same mock then
    // measured 26,098 rps at c=131. A maximum that another measurement beats is not a maximum, which
    // is what C6 refuses to publish.
    //
    // The cause is entirely in the stopping rule. Judging the MEDIAN against the range of individual
    // WINDOWS charges a real gain against noise the median does not carry: at c=16 the windows ran
    // 19837..24740 (19.8%) while the median rose 18819 -> 21065 (+11.9%), so a genuine climb read as
    // flat, a second flat rung followed, and the ladder stopped at c=32 with the ceiling above it.
    struct RecordedWindows {
        by_conc: std::collections::BTreeMap<u32, Vec<f64>>,
        taken: std::collections::BTreeMap<u32, usize>,
        /// Rungs above what the field actually sampled. The real curve kept climbing to c=131, where
        /// the sustained leg found 26,098; this continues it so the test can assert the search now
        /// REACHES that ground rather than merely taking one more step toward it.
        beyond: f64,
    }
    impl Probe for RecordedWindows {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            let i = self.taken.entry(c).or_insert(0);
            let v = match self.by_conc.get(&c) {
                Some(vals) => vals[(*i).min(vals.len() - 1)],
                None => self.beyond,
            };
            *i += 1;
            Some(Sample { value: v, passed: true })
        }
    }

    fn kong_openai_openai() -> RecordedWindows {
        let mut by_conc = std::collections::BTreeMap::new();
        by_conc.insert(1u32, vec![5007.0, 5099.0, 5103.0]);
        by_conc.insert(2, vec![7392.0, 7676.0, 9927.0]);
        by_conc.insert(4, vec![12343.0, 15541.0, 16648.0]);
        by_conc.insert(8, vec![22466.0, 18819.0, 17755.0]);
        by_conc.insert(16, vec![24740.0, 21065.0, 19837.0]);
        by_conc.insert(32, vec![20871.0, 20732.0, 26506.0]);
        RecordedWindows { by_conc, taken: Default::default(), beyond: 26098.0 }
    }

    #[test]
    fn a_climbing_curve_with_scattered_windows_is_not_called_saturated() {
        let mut probe = kong_openai_openai();
        let r = saturation_plateau(&mut probe, 1, 4096);
        let peak = match r.peak {
            Measurement::Measured(p) => p.value,
            ref other => panic!("kong's curve must produce a peak, got {other:?}"),
        };
        // The published number was 20,871 while a window on the same rung reached 26,506 and the
        // sustained leg reached 26,098. Anything at or below the old answer means the ladder stopped
        // in the same place for the same reason.
        assert!(
            peak > 20871.0,
            "the search published {peak:.0}, which is no better than the 20,871 that C6 rejected as a \
             maximum another measurement beat"
        );
        // It must actually climb past where the field stopped, not just wobble one rung further.
        let top = r.points.iter().map(|p| p.concurrency).max().unwrap_or(0);
        assert!(top > 32, "the ladder stopped at c={top}, the same rung the field run stopped at");
    }

    // The bar is the uncertainty of the rung's MEDIAN, so more windows behind a median make it
    // stricter, not looser. Without the divisor a rung's bar is its raw window range, which is what
    // let scatter masquerade as a plateau.
    #[test]
    fn the_improvement_bar_tightens_as_a_rung_gathers_more_windows() {
        let spread = 0.20;
        assert!(improvement_bar(spread, 9) < improvement_bar(spread, 3));
        assert!(improvement_bar(spread, 3) < spread, "the median is steadier than its windows' range");
        // Never below the floor, however tidy the windows look: three can agree by luck.
        assert_eq!(improvement_bar(0.0, 3), WOBBLE_FLOOR);
        assert_eq!(improvement_bar(0.001, 100), WOBBLE_FLOOR);
        // A rung with no passing windows cannot divide by zero.
        assert_eq!(improvement_bar(0.0, 0), WOBBLE_FLOOR);
    }
}
