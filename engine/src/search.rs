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
    // gate by construction. `saturation_plateau_gated` already carries the same floor for the same
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
    // streams gate was the whole range at once. That is the same defect `saturation_plateau` already
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

/// The winning concurrency and value of a peak search.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PeakPoint {
    /// The concurrency the peak was measured AT. Paired with `value`, these are one measurement -
    /// a reader can find them together in the published sweep.
    pub concurrency: u32,
    pub value: f64,
    /// THE KNEE: the lowest concurrency whose own reading reached the plateau, which is the
    /// operational fact "how much concurrency before more stops helping".
    ///
    /// Kept as its own field rather than folded into `concurrency`, because the two answer different
    /// questions and pairing the plateau LEVEL with the KNEE produced a (value, concurrency) pair
    /// that no single measurement made: agentgateway published "25182 @ c=16" when c=16 measured
    /// 24932, c=32 measured 25182 and c=64 measured 25278. A field named `rps_max_proxy`, guarded by
    /// C6 as a maximum, has to be the highest thing measured - otherwise our own sweep contains
    /// rungs that beat it and the guard fires on our own data.
    pub knee_concurrency: u32,
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
    /// Every rung the climb judged, with what its windows said about the caller's gate.
    ///
    /// Handed back so the caller can answer its OWN question off the same windows - "and how much
    /// while p99 held?" - instead of running a second search for it later, against a gateway the
    /// intervening groups have since restarted.
    pub rungs: Vec<RungSummary>,
}

/// The rungs, in the order they were climbed, for a caller that owns the gate.
fn summarize(rungs: &[Rung]) -> Vec<RungSummary> {
    rungs
        .iter()
        .map(|r| RungSummary {
            concurrency: r.concurrency,
            median: r.median,
            median_reading: r.median_reading,
            windows: r.windows,
            observed_median: r.observed_median,
            observed_median_reading: r.observed_median_reading,
            gate_median: r.gate.map(|g| g.median),
            gate_median_reading: r.gate.and_then(|g| g.median_reading),
            gate_holds: r.gate.is_some_and(|g| g.holds()),
        })
        .collect()
}

/// An interruption (a deadline, or a window that produced nothing) after real probes have already
/// landed must NOT throw away what was measured: once a rung has been judged, every later abort
/// still leaves a genuinely measured, if partial, answer behind. Discarding it publishes null for a
/// cell we did in fact measure, which is the same class of loss as publishing a zero for one we did
/// not.
///
/// Only a search that was cut off before ANY gate-passing point is genuinely unmeasured.
fn interrupted<P: Probe>(s: Search<P>, rungs: &[Rung]) -> PeakResult {
    let mut ordered: Vec<&ProbedPoint> = s.points.iter().collect();
    ordered.sort_by_key(|p| p.concurrency);
    let mut winner: Option<PeakPoint> = None;
    for p in ordered {
        if p.passed && winner.as_ref().is_none_or(|w| p.value > w.value) {
            // An interrupted search never established a plateau, so there is no knee to report:
            // the best passing point is its own knee, which is the honest degenerate answer.
            winner = Some(PeakPoint {
                concurrency: p.concurrency,
                value: p.value,
                knee_concurrency: p.concurrency,
            });
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
    PeakResult {
        // RigLimited, not NotMeasured: an interruption is the RIG failing to finish asking (a
        // refused window, an exhausted port range), never a fact about the gateway. It must also
        // stay distinguishable from "every rung genuinely failed the gate", which
        // `sweep_cpu_fps_cell` publishes as a measured 0 - under one shared reason a rig
        // interruption would have become the gateway's zero.
        peak: Measurement::absent_because(Absent::RigLimited, detail),
        points: s.points,
        exhausted: false,
        rungs: summarize(rungs),
    }
}

/// Windows taken at every rung. Three is the smallest sample with a middle value, and the median of
/// three is what makes a rung's number resistant to one unlucky window.
pub const WINDOWS_PER_RUNG: usize = 3;

/// A floor under the measured wobble. Three windows can agree closely by luck, and a threshold near
/// zero would let any flutter read as a real gain.
const WOBBLE_FLOOR: f64 = 0.02;

/// How many consecutive rungs must fail to improve before the curve is called saturated.
///
/// THREE, BECAUSE TWO IS NOT ENOUGH EVIDENCE ON A NOISY GATEWAY. The old value was two, reasoned as
/// "one flat rung can be a downward draw; two in a row is the curve". kong disproved that on real
/// hardware: its throughput climbs all the way to c~94, but each DOUBLING only buys 2-5% while its
/// window spread is 19-26%, so the noise bar (spread over root-n) is larger than the real per-step
/// gain. Two consecutive rungs read as flat while the curve was still rising, the search stopped at
/// c=32, and the published "maximum" of 15909 was beaten by the sustained search - which does not
/// stop on flatness - at 17898 on the same box.
///
/// That is the whole C6 inversion class. Replayed against kong's own recorded windows:
///
///   openai>bedrock     FLAT=2 stops c=32, publishes 15909 (sustained 17898, 1.13x INVERTED)
///                      FLAT=3 stops c=256, publishes 17829 (1.00x)
///   openai>anthropic   FLAT=2 stops c=32, publishes 20619 (sustained 24486, 1.19x INVERTED)
///                      FLAT=3 stops c=256, publishes 24496 (1.00x)
///
/// The cost is one extra rung on a gateway that really has saturated - three windows, a few seconds
/// against a cell that takes minutes. The cost of stopping early is publishing a maximum that
/// another measurement on the same box beats, which is not a maximum.
const FLAT_RUNGS_TO_STOP: usize = 3;

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

/// The window BEHIND a rung's median, not just the number it produced.
///
/// Nearest-rank returns a value some window actually measured, so the rate and the latency and loss
/// published beside it can be ONE window's evidence rather than three windows' numbers assembled
/// into a row no window ever produced. The caller needs it because a rung's evidence row used to
/// carry `p99_us: None, fail: 0` beside a correct FAILED verdict - a row saying the rung served
/// cleanly at a rate it had just been judged not to sustain.
fn median_window(mut windows: Vec<(f64, Option<Reading>)>) -> Option<(f64, Option<Reading>)> {
    windows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let vals: Vec<f64> = windows.iter().map(|(v, _)| *v).collect();
    let m = nearest_rank_median(&vals)?;
    windows.into_iter().find(|(v, _)| *v == m)
}

/// One rung, measured: its median throughput and the spread of the windows behind it.
struct Rung {
    concurrency: u32,
    median: f64,
    /// What the window behind `median` observed. Absent only when the probe measures no latency at
    /// all (the stream searches), never as a stand-in for "measured nothing".
    median_reading: Option<Reading>,
    spread: f64,
    /// How many windows actually passed and went into the median. The bar below divides by its root,
    /// so a rung that lost windows to failures is judged on the evidence it really has.
    windows: usize,
    /// The median window over EVERY window at this rung, passing or not, and what it read.
    ///
    /// Separate from `median` deliberately, exactly as `gate` is: `median` covers only the windows
    /// that PASSED, and a failed window must stay out of it because letting one in made the spread
    /// ~100% and froze an earlier version of this search near the floor. This answers a third
    /// question - "what was this rung observed serving at all" - which is the only honest number for
    /// the EVIDENCE row of a rung where nothing passed. `None` only if the rung produced no window.
    observed_median: Option<f64>,
    /// What the window behind `observed_median` read, so a published row's rate, p99 and loss come
    /// from one window rather than three windows' numbers mixed together.
    observed_median_reading: Option<Reading>,
    /// What this rung's windows said about the CALLER'S gate, when one was supplied.
    ///
    /// Separate from `windows`/`median` above, which answer "how fast", because this answers "how
    /// fast while still under the ceiling" - and the whole reason it lives on the same rung is that
    /// the two must be answers about the SAME windows. When they were two searches, the second one
    /// ran minutes after the first, on the far side of a gateway restart.
    gate: Option<GateEvidence>,
}

/// One rung's verdict against the caller's gate, over the same windows that produced its median.
#[derive(Debug, Clone, Copy, PartialEq)]
struct GateEvidence {
    /// Windows at this rung that carried a reading and held the gate.
    held: usize,
    /// Windows at this rung that carried a reading at all.
    judged: usize,
    /// The median rate over the gate-HOLDING windows. A window that blew the ceiling sustained
    /// nothing, so folding its rate into a sustained number would publish throughput the gate had
    /// just refused.
    median: f64,
    /// What the window behind `median` observed, so a published sustained rung's p99 and fail count
    /// come from the same window as its rate.
    median_reading: Option<Reading>,
}

impl GateEvidence {
    /// A rung holds the gate when a MAJORITY of its judged windows did. The bisection needs one
    /// window to decide, and that is the right shape for a search; publishing needs more than one,
    /// because the boundary rung is by construction the one that passed exactly once.
    fn holds(&self) -> bool {
        self.judged > 0 && self.held * 2 > self.judged
    }
}

/// A rung's summary, published for the caller that owns the gate.
///
/// `search` climbs ladders and judges rungs; it does not know what 20ms means. The gate predicate
/// and the ceiling's confirmation belong to the caller, so the climb hands back what it measured and
/// lets `run` decide what counts as sustained.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RungSummary {
    pub concurrency: u32,
    /// The median rate over every window that passed at this rung.
    ///
    /// `0.0` when `windows` is 0: no window passed, so there was no median to take and this is a
    /// sentinel rather than a rate. A caller PUBLISHING such a rung must read `observed_median`
    /// instead - a fabricated 0 in an evidence row says the gateway served nothing at a rate nothing
    /// ever observed it serving.
    pub median: f64,
    /// How many windows passed and went into `median`. Zero is the sentinel condition above.
    ///
    /// Carried because dropping it here was the whole defect: `Rung` knew the rung had no passing
    /// window and the summary did not, so the 0.0 arrived downstream indistinguishable from a rate.
    pub windows: usize,
    /// The median rate over EVERY window at this rung, passing or not. `None` only when the rung
    /// produced no window at all.
    pub observed_median: Option<f64>,
    /// What the window behind `observed_median` read.
    pub observed_median_reading: Option<Reading>,
    /// What the window behind `median` observed.
    ///
    /// Carried so a caller can publish a rung's REAL latency and loss beside its rate. The sustained
    /// sweep used to publish `p99_us: None, fail: 0` for every climb rung, including the ones it had
    /// just failed, so a reader re-deriving the verdict from the published evidence saw a rung that
    /// had served nothing when it had in fact served at a rate the gate refused.
    pub median_reading: Option<Reading>,
    /// The median rate over the windows that held the caller's gate, when one was supplied.
    pub gate_median: Option<f64>,
    /// What the window behind `gate_median` observed.
    pub gate_median_reading: Option<Reading>,
    /// Whether a majority of judged windows held the gate.
    pub gate_holds: bool,
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
/// THE GATE IS JUDGED OVER THE SAME WINDOWS AS THE MEDIAN, not over a later re-measurement. A window
/// that held the gate contributes its rate to `gate.median` as well as to `median`; one that blew it
/// contributes to neither, exactly as a window that failed outright contributes to neither.
fn measure_rung<P: Probe>(s: &mut Search<P>, c: u32, gate: Option<&Gate>) -> Option<Rung> {
    // Each window travels WITH the reading it produced, so the rung's published rate and the p99 and
    // fail count published beside it are one window's evidence rather than three windows' numbers
    // mixed into a row none of them measured.
    let mut vals: Vec<(f64, Option<Reading>)> = Vec::with_capacity(WINDOWS_PER_RUNG);
    let mut held: Vec<(f64, Option<Reading>)> = Vec::with_capacity(WINDOWS_PER_RUNG);
    let mut all: Vec<(f64, Option<Reading>)> = Vec::with_capacity(WINDOWS_PER_RUNG);
    let mut judged = 0usize;
    for i in 0..WINDOWS_PER_RUNG {
        // The first window may come from the memo; the repeats must not, since the whole point is
        // that identical conditions produce different numbers.
        let sample = if i == 0 {
            s.sample(c)?
        } else {
            s.sample_repeat(c)?
        };
        // A window is judged against the gate whether or not it passed the search's own verdict: the
        // gate is a claim about latency and loss, and a window with failures has already told us
        // something about loss. Only a window with no reading at all is unjudgeable.
        if let (Some(g), Some(r)) = (gate, sample.reading) {
            judged += 1;
            if g(&r) {
                held.push((sample.value, sample.reading));
            }
        }
        all.push((sample.value, sample.reading));
        if sample.passed {
            vals.push((sample.value, sample.reading));
        }
    }
    let held_count = held.len();
    let gate_window = median_window(held);
    let observed = median_window(all);
    let evidence = gate.map(|_| GateEvidence {
        held: held_count,
        judged,
        median: gate_window.map(|(v, _)| v).unwrap_or(0.0),
        median_reading: gate_window.and_then(|(_, r)| r),
    });
    if vals.is_empty() {
        return Some(Rung {
            concurrency: c,
            median: 0.0,
            median_reading: None,
            spread: 0.0,
            windows: 0,
            observed_median: observed.map(|(v, _)| v),
            observed_median_reading: observed.and_then(|(_, r)| r),
            gate: evidence,
        });
    }
    let rates: Vec<f64> = vals.iter().map(|(v, _)| *v).collect();
    let window = median_window(vals);
    Some(Rung {
        concurrency: c,
        median: window.map(|(v, _)| v).unwrap_or(0.0),
        median_reading: window.and_then(|(_, r)| r),
        spread: if rates.len() >= 2 {
            relative_spread(&rates)
        } else {
            0.0
        },
        windows: rates.len(),
        observed_median: observed.map(|(v, _)| v),
        observed_median_reading: observed.and_then(|(_, r)| r),
        gate: evidence,
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
    saturation_plateau_gated(probe, min_conc, max_conc, None)
}

/// WALK THE LADDER AND PROBE. DECIDE NOTHING.
///
/// Returns every rung probed, in probe order, and no verdict. The caller reads whatever answer it
/// wants off them - see `frontier.rs`, which reads the throughput answer at six different tail-latency
/// bounds from one call to this.
///
/// THIS REPLACED `saturation_plateau`, and the difference is the whole point. That function climbed AND
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

/// A predicate over one window's reading: does this window hold the caller's ceiling?
///
/// `search` does not know what 20ms means, and should not. It climbs, it judges rungs against their
/// own noise, and it hands back what each rung's windows said about whatever gate the caller cares
/// about. The gate's definition and the ceiling's confirmation stay in `run`, next to the constants
/// that give them meaning.
pub type Gate = dyn Fn(&Reading) -> bool;

/// THE SAME CLIMB, told what the caller's ceiling is.
///
/// One implementation, not two. The engine has been burned twice by a rule that lived in one copy of
/// a loop while a second copy went on doing the old thing - the ladder itself became a type for that
/// reason, and this is the same lesson applied to the stopping rule. `saturation_plateau` is this
/// function with no gate.
///
/// WHAT THE GATE CHANGES: the stopping rule becomes a UNION. Without a gate the climb stops when
/// throughput has been flat for `FLAT_RUNGS_TO_STOP` rungs, which is the right answer to "where does
/// this saturate". With one it must ALSO keep climbing until a rung fails the gate, because those are
/// different questions and the second one's answer sits well above the first's. Past saturation a
/// gateway keeps accepting concurrency and converting it to queueing, and queueing that stays under
/// the ceiling is still inside the gate: in the 2026-07-28 run five aisix cells plateaued at c=64 and
/// went on holding a 20ms p99 out to c≈180. A climb that stopped at the plateau would have published
/// a third of their real ceiling.
/// The index of the FIRST rung whose median reaches the best of all rungs (ignoring zero rungs), or
/// None when no rung produced a positive median. First, not last: a later rung that ties the best has
/// not beaten it, so on an exact-tie plateau the winner stays interior rather than drifting to
/// whichever equal rung happened to be probed last.
fn winner_index(rungs: &[Rung]) -> Option<usize> {
    let best = rungs.iter().map(|r| r.median).fold(0.0_f64, f64::max);
    if best <= 0.0 {
        return None;
    }
    rungs.iter().position(|r| r.median >= best)
}

/// The rung the peak PUBLISHES, which must never be the last rung probed.
///
/// A curve creeping up by less than its own wobble never resets `flat_run`, but its best median
/// keeps landing on the newest rung - kong's shape on the 2026-07-28 board, which published the
/// search's own final rung as the gateway's maximum ("the max_proxy sweep WON at the highest
/// concurrency it probed", the site's structural invariant). When the flat-stop has fired, the last
/// rung is by construction one of `FLAT_RUNGS_TO_STOP` consecutive non-improvers, so if the global
/// first-max sits on it the creep stayed inside the wobble band and the best rung BELOW it is the
/// honest ceiling: a real measured rung, with the final rung measured above it and failing to beat
/// it by more than the noise. Drift inside the noise band is not throughput.
fn published_winner(rungs: &[Rung]) -> Option<&Rung> {
    let i = winner_index(rungs)?;
    if i + 1 == rungs.len() && rungs.len() >= 2 {
        return winner_index(&rungs[..rungs.len() - 1]).map(|j| &rungs[j]);
    }
    Some(&rungs[i])
}

pub fn saturation_plateau_gated<P: Probe>(
    probe: &mut P,
    min_conc: u32,
    max_conc: u32,
    gate: Option<&Gate>,
) -> PeakResult {
    let (min_conc, max_conc) = if min_conc <= max_conc {
        (min_conc, max_conc)
    } else {
        (max_conc, min_conc)
    };
    // ZERO CONCURRENCY DOES NOT EXIST. The climb step is `c.saturating_mul(2)`, and doubling zero is
    // still zero, so a caller-supplied floor of 0 (e.g. `OTB_MIN_CONC=0`) pinned the ladder at c=0
    // forever instead of climbing.
    let mut ladder = Ladder::from_floor(min_conc, max_conc);
    let min_conc = ladder.floor();
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
        let rung = match measure_rung(&mut s, c, gate) {
            Some(r) => r,
            None => return interrupted(s, &rungs),
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
        // mean anything - AND, when a gate was supplied, a rung that has actually failed it. See
        // this function's own note on why the second condition cannot be dropped: the gate's ceiling
        // routinely sits far above the throughput knee, and stopping at the knee would publish the
        // knee under the ceiling's name.
        let gate_broken = gate.is_none()
            || rungs
                .last()
                .and_then(|r| r.gate)
                .is_some_and(|g| g.judged > 0 && !g.holds());
        if flat_run >= FLAT_RUNGS_TO_STOP && c >= MIN_SATURATION_CONC && gate_broken {
            break;
        }
        c = match ladder.next() {
            Some(next) => next,
            None => break,
        };
    }

    let best = rungs.iter().map(|r| r.median).fold(0.0_f64, f64::max);
    if best <= 0.0 {
        let detail = format!(
            "no concurrency from {min_conc} to {max_conc} passed the gate, so no throughput was established at any rung"
        );
        return PeakResult {
            peak: Measurement::absent_because(Absent::NotMeasured, detail),
            points: s.points,
            exhausted: false,
            rungs: summarize(&rungs),
        };
    }

    // STILL CLIMBING AT THE BOUND is a lower bound, not a plateau. The range is our choice; reporting
    // it as the gateway's ceiling would be publishing our own search bound as its answer.
    //
    // BUT A LADDER THAT ENDED ON A FAILING RUNG DID NOT RUN OUT OF RANGE - IT FOUND THE LIMIT.
    //
    // `flat_run` counts rungs that did not improve, and a rung whose every window FAILED the gate has
    // a median of zero, so it cannot improve and is counted as flat. Those two are not the same
    // finding. "We asked for more and got more, then ran out of ladder" is genuinely a lower bound.
    // "We asked for more and the windows stopped holding" is the ceiling, and the best passing rung
    // below it is a real measurement of it.
    //
    // Bifrost's cpu_fps on 2026-07-29: c=1024 passed at 43,404 frames/sec with 0 stalls, then c=2048
    // failed all three windows with ~5,000 stalls and c=4096 failed all three with ~7,000. The
    // measurement was sitting there, and the search published nothing because it needed a THIRD flat
    // rung and the ladder ended one short. Raising the ceiling only moves where that happens; the
    // rungs above are failing either way, and failing rungs are evidence rather than absence of it.
    let ended_on_a_failure = rungs.last().is_some_and(|r| r.windows == 0);
    if hit_bound && flat_run < FLAT_RUNGS_TO_STOP && !ended_on_a_failure {
        let top_wobble = rungs
            .last()
            .map(|r| improvement_bar(r.spread, r.windows))
            .unwrap_or(WOBBLE_FLOOR);
        let detail = format!(
            "throughput was still climbing by more than the measured {:.1}% window-to-window wobble at c={max_conc} ({best:.0}) when the search range ran out, so saturation was never observed and no plateau was established",
            top_wobble * 100.0
        );
        return PeakResult {
            peak: Measurement::absent_because(Absent::SearchExhausted, detail),
            points: s.points,
            exhausted: true,
            rungs: summarize(&rungs),
        };
    }

    // THE PUBLISHED PAIR MUST BE ONE MEASUREMENT: the best rung's own median, and that rung's own
    // concurrency.
    //
    // This used to publish the median of the plateau BAND's medians, paired with the KNEE - the
    // lowest concurrency in the band. Two different rungs, published as one (value, concurrency)
    // pair under names that say "the peak, and the concurrency that peak happened at".
    // agentgateway anthropic>anthropic in the 2026-07-28 run published "25182 @ c=16" when c=16
    // measured 24932, c=32 measured 25182, and c=64 measured 25278. No single measurement produced
    // that pair, and the highest rung the gateway actually reached was published nowhere.
    //
    // The band median is a defensible statistic and the knee is a useful fact, but neither is what
    // `rps_max_proxy`/`conc_at_peak` claim to be, and a reader cannot re-derive the pair from the
    // sweep that ships beside it. Each rung's median is already the median of `WINDOWS_PER_RUNG`
    // windows, so the best of them is a robust reading rather than a lucky draw - and it is a
    // reading that actually happened.
    // The band is still computed, because the knee is still worth publishing - it is just no longer
    // welded to a value measured at a different rung.
    //
    // The winning rung comes FIRST, and the band is drawn around IT rather than around the raw
    // global maximum. Drawn around the maximum, the band described a rung the search had already
    // decided not to publish: a noisy final rung (big wobble, so it fails to "improve" and gets
    // demoted by `published_winner`) still set the band's threshold, and a quiet published rung
    // 14% below it then fell OUTSIDE the band annotating it. Worse, the knee - the band's lowest
    // concurrency - was taken over ALL rungs including that final one, so a band containing only
    // rungs ABOVE the published peak produced `knee > conc_at_peak`: a published pair claiming more
    // concurrency was needed to reach the plateau than the rung the plateau's own value came from.
    // Around the published winner the band contains it by construction, and capping the knee at the
    // published concurrency makes the pair readable in one direction only, which is the only
    // direction it means anything in.
    let peak = published_winner(&rungs);
    let Some(peak) = peak else {
        let detail = format!(
            "no concurrency from {min_conc} to {max_conc} produced a usable rung, so no peak was established"
        );
        return PeakResult {
            peak: Measurement::absent_because(Absent::NotMeasured, detail),
            points: s.points,
            exhausted: false,
            rungs: summarize(&rungs),
        };
    };

    // THE KNEE IS THE LOWEST RUNG INDISTINGUISHABLE FROM WHAT WE PUBLISHED, and it can never sit
    // above the rung we published. `<= peak.concurrency` is the whole of that guarantee.
    let knee = rungs
        .iter()
        .filter(|r| {
            r.concurrency <= peak.concurrency
                && r.median > 0.0
                && r.median >= peak.median * (1.0 - improvement_bar(r.spread, r.windows))
        })
        .map(|r| r.concurrency)
        .min()
        .unwrap_or(peak.concurrency);

    let summary = summarize(&rungs);
    PeakResult {
        peak: Measurement::Measured(PeakPoint {
            concurrency: peak.concurrency,
            value: peak.median,
            knee_concurrency: knee,
        }),
        rungs: summary,
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

    // THE TIE-BREAK EXPERIMENT, kept as a test because it is the cheapest possible statement of the
    // rule. The gate is FIXED; only the harness's own search floor moves. If an interrupted search
    // published a confirmed rung, the answer would track the FLOOR (1, 8, 16, 64) and carry zero
    // bits about the gateway, which is precisely how unrelated gateways come to share a number.
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
            // The measurement is NOT lost: every probed rung still travels.
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

    // Interrupted BEFORE anything passed: genuinely unmeasured, so absent.
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

    // Interrupted AFTER a confirmed pass must still report no ceiling, not the confirmed rung: the
    // probed trace survives on every path including this one, so nothing is discarded, and at this
    // point the only successful probe IS the floor, so returning it would publish our own search
    // configuration as the gateway's ceiling. The lower bound belongs in the evidence, not the value.
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

    // The partial answer is still a LOWER BOUND, never the search range: an interrupted run must
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

    // A FAILING FLOOR ABOVE ONE PROVES ONLY THAT THE CEILING IS BELOW THE FLOOR, and not one value
    // in [0, floor-1] was probed. Returning 0 there publishes a specific number the search never
    // established, which is the same fabrication as publishing the range bound at the top end, just
    // at the other end of the range. The `Measured(0)` answer is reserved for a floor of one, where
    // there is genuinely nowhere left to look.
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
        // The refusal is about the FLOOR, not about the gate: with a floor of one the identical gate
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

    // A RANGE OF ONE PROVES NOTHING ABOUT A CEILING. The single rung passed, and nothing above it
    // was ever probed, so the answer is a lower bound: publishing it would report the caller's own
    // one-point range as the gateway's ceiling.
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

    // THE LADDER IS WALKED WHOLE, and the climb stops on the predicate flip: the first rung where no
    // window served cleanly. Nothing counts flat rungs, so nothing can stop short of a rung the
    // frontier would have read.
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
        // The first failing rung IS probed and IS returned - it is the evidence that the one below it
        // is a boundary rather than a place we stopped. `frontier` needs it for
        // `first_disqualified_conc`.
        assert!(points.iter().any(|p| p.concurrency == 2048 && !p.passed));
        assert!(
            !points.iter().any(|p| p.concurrency == 4096),
            "and nothing above it: more concurrency cannot un-fail those requests"
        );
    }

    // THE REGRESSION THE OLD STOP RULE CAUSED. kong's curve creeps up by less than its own
    // window-to-window wobble, so `FLAT_RUNGS_TO_STOP = 3` fired while throughput was still rising and
    // the climb stopped at c=32 - publishing 15909 as the maximum, which the sustained reading of the
    // same windows then beat at 17898. Here the same shape must be climbed to the end.
    #[test]
    fn a_curve_that_creeps_inside_its_own_noise_is_still_climbed_to_the_end() {
        struct Creeping;
        impl Probe for Creeping {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                // Gains ~3% per doubling - smaller than a noisy gateway's window spread, which is
                // exactly the shape the flat-run counter mistook for saturation.
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

    // A rung that fails EVERY window ends the climb; a rung that fails only some does not. The stop
    // condition is "no clean window at all", because a single clean window is still a real observation
    // the frontier can read.
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

    // Every rung gets `WINDOWS_PER_RUNG` windows, so the frontier reads a rate backed by repeats rather
    // than by one lucky window. The memo may serve the FIRST window; the repeats must be real.
    #[test]
    fn every_rung_is_probed_the_full_number_of_windows() {
        let mut probe = CleanToLimit { limit: 64 };
        let points = climb_rungs(&mut probe, 1, 8);
        for c in [1u32, 2, 4, 8] {
            let n = points.iter().filter(|p| p.concurrency == c).count();
            assert_eq!(n, WINDOWS_PER_RUNG, "c={c} got {n} windows");
        }
    }

    // A RIG THAT CANNOT RUN A WINDOW ENDS THE CLIMB WITH WHAT IT HAS, and does not read as a ceiling.
    // The distinction is the project's central rule: our failure is never their result.
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
            let level = if c >= self.knee {
                self.plateau
            } else {
                self.plateau * (c as f64 / self.knee as f64)
            };
            Some(Sample::new(level * (1.0 + sign * self.wobble), true))
        }
    }

    // THE FIELD BUG, PINNED. A gateway that saturates early and then holds flat for many doublings
    // must be reported at its plateau, NOT walked to the top of the search range. The search this
    // replaced did exactly that: on the flat part it asked "is the next rung higher?", noise
    // answered yes, and it published the range bound as the gateway's maximum.
    #[test]
    fn a_flat_curve_saturates_and_never_walks_to_the_top_of_the_range() {
        let mut probe = Saturating {
            knee: 64,
            plateau: 6000.0,
            wobble: 0.01,
            calls: 0,
        };
        let r = saturation_plateau(&mut probe, 1, 4096);
        assert!(
            !r.exhausted,
            "a curve that plateaued must not report the range as exhausted"
        );
        let w = r
            .peak
            .value()
            .expect("a saturated curve has a plateau to publish");
        // The KNEE is what "saturation is at c=64" means. The summit can land on any rung in the
        // plateau band by chance - on a flat curve with 1% wobble that is exactly what it does - so
        // asserting the summit here would be asserting which way the noise fell.
        assert!(
            w.knee_concurrency <= 256,
            "saturation is at c=64; reporting knee c={} means the search kept climbing on noise",
            w.knee_concurrency
        );
        // And the peak, wherever it landed, is a rung that was actually probed.
        assert!(
            r.points
                .iter()
                .any(|p| p.concurrency == w.concurrency && p.passed),
            "published c={} was never probed",
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
            Some(Sample::new(level, true))
        }
    }

    #[test]
    fn a_zero_floor_still_climbs_and_terminates() {
        let mut probe = CountBudgetedSaturating {
            knee: 64,
            plateau: 6000.0,
            calls: 0,
            max_calls: 200,
        };
        let r = saturation_plateau(&mut probe, 0, 4096);
        assert!(
            !r.exhausted,
            "a curve that plateaued must not report the range as exhausted"
        );
        let w = r
            .peak
            .value()
            .expect("a saturated curve has a plateau to publish");
        assert!(
            w.concurrency >= 1,
            "knee reported at c={}, which is not a real concurrency",
            w.concurrency
        );
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

        let mut p = ReplayWindows {
            seq,
            seen: Default::default(),
        };
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
            Some(Sample::new(xs[n.min(xs.len() - 1)], true))
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

        let mut p = ReplayWindows {
            seq,
            seen: Default::default(),
        };
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
            Some(Sample::new(v, !(c == 1 && n == 1)))
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
        let mut p = FlakyFloor {
            seq,
            seen: Default::default(),
        };
        let r = saturation_plateau(&mut p, 1, 4096);
        let w = r
            .peak
            .value()
            .expect("a climbing curve with one bad floor window still has a plateau");
        assert!(
            w.value > 50.0,
            "published {} - the search froze near the floor because a failed window poisoned the \
             wobble; this curve climbs to 60",
            w.value
        );
        assert!(
            w.concurrency >= 64,
            "reported c={} is still on the rising part",
            w.concurrency
        );
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
                return Some(Sample::new(
                    self.plateau * (c as f64 / self.knee as f64),
                    true,
                ));
            }
            let doublings = (c as f64 / self.knee as f64).log2().max(0.0);
            Some(Sample::new(
                self.plateau * (1.0 + self.drift * doublings),
                true,
            ))
        }
    }

    #[test]
    fn a_plateau_that_drifts_upward_inside_the_noise_is_still_saturated() {
        // 0.4% per doubling: eleven doublings of it is under 5%, and no gateway "gains throughput"
        // that way. A search comparing against zero climbs every one of them.
        let mut probe = DriftingPlateau {
            knee: 32,
            plateau: 6000.0,
            drift: 0.004,
        };
        let r = saturation_plateau(&mut probe, 1, 65_536);
        assert!(
            !r.exhausted,
            "drift inside the noise band must not read as failing to saturate"
        );
        let w = r
            .peak
            .value()
            .expect("a drifting plateau is still a plateau");
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
        let mut probe = Saturating {
            knee: 32,
            plateau: 1000.0,
            wobble: 0.05,
            calls: 0,
        };
        let r = saturation_plateau(&mut probe, 1, 2048);
        let w = r.peak.value().expect("saturated");
        let best_seen = r
            .points
            .iter()
            .filter(|p| p.passed)
            .map(|p| p.value)
            .fold(f64::MIN, f64::max);
        assert!(
            w.value < best_seen,
            "published {} is the best rung seen ({}), so the luckiest window won",
            w.value,
            best_seen
        );
        // ... and it is still a real plateau figure, not something dragged down by the rising part.
        assert!(
            w.value > 1000.0 * 0.9,
            "published {} is far below the plateau level",
            w.value
        );
    }

    // The reported concurrency is the KNEE - the lowest rung that reached the plateau - because that
    // is the answer to "how much concurrency do I need before more stops helping". With a median
    // value there is no single winning rung for a summit to point at anyway.
    #[test]
    fn the_knee_is_reported_separately_from_the_peak_that_was_measured() {
        let mut probe = Saturating {
            knee: 64,
            plateau: 5000.0,
            wobble: 0.01,
            calls: 0,
        };
        let r = saturation_plateau(&mut probe, 1, 4096);
        let w = r.peak.value().expect("saturated");
        let highest_probed = r.points.iter().map(|p| p.concurrency).max().unwrap_or(0);

        // The KNEE is still reported - "how much concurrency before more stops helping" is the
        // operational fact this search exists to find, and it is nowhere near the top of the range.
        assert!(
            w.knee_concurrency < highest_probed,
            "knee c={} equals the highest rung probed ({}), which is a summit, not a knee",
            w.knee_concurrency,
            highest_probed
        );

        // THE PUBLISHED PAIR IS ONE MEASUREMENT. `value` and `concurrency` must be findable together
        // in the sweep that ships beside them: the pair used to be the plateau band's median labelled
        // with the knee's concurrency, which no single rung ever produced.
        let at_that_rung: Vec<f64> = r
            .points
            .iter()
            .filter(|p| p.concurrency == w.concurrency && p.passed)
            .map(|p| p.value)
            .collect();
        assert!(
            !at_that_rung.is_empty(),
            "published c={} has no passing window in the sweep at all",
            w.concurrency
        );
        let lo = at_that_rung.iter().cloned().fold(f64::MAX, f64::min);
        let hi = at_that_rung.iter().cloned().fold(f64::MIN, f64::max);
        assert!(
            w.value >= lo && w.value <= hi,
            "published {} @ c={} but that rung's own windows span {lo}..{hi} - the pair is not a \
             measurement anyone took",
            w.value,
            w.concurrency
        );
    }

    /// A KNEE ABOVE THE PEAK RUNG IS AN INCOHERENT PUBLISHED PAIR.
    ///
    /// The knee is "how much concurrency before more stops helping" and the peak concurrency is the
    /// rung the published value was measured at, so a knee ABOVE it claims the plateau began after
    /// the rung whose reading is the plateau's own value. It was reachable: the band was drawn around
    /// the raw global maximum, `published_winner` demoted off a noisy final rung, and the demoted
    /// winner then sat outside the band annotating it - leaving the final rung as the band's only
    /// member and its concurrency as the knee.
    #[test]
    fn the_knee_never_sits_above_the_rung_the_peak_was_published_from() {
        // A quiet climb into a plateau, and one final rung whose three windows scatter 800/1150/1150.
        // Its median beats every rung below it, but by less than its own 17.6% wobble, so the climb
        // calls it saturation and the winner is demoted off it - while a band drawn at 1150 excluded
        // every rung the search was actually willing to publish.
        struct NoisyFinalRung {
            calls_at: std::collections::BTreeMap<u32, u32>,
        }
        impl Probe for NoisyFinalRung {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                let n = self.calls_at.entry(c).or_insert(0);
                *n += 1;
                let v = match c {
                    1 => 100.0,
                    2 => 200.0,
                    4 => 400.0,
                    8 => 800.0,
                    16 => 1000.0,
                    32 => 1005.0,
                    64 => 1010.0,
                    _ if *n == 1 => 800.0,
                    _ => 1150.0,
                };
                Some(Sample::new(v, true))
            }
        }
        let mut probe = NoisyFinalRung {
            calls_at: Default::default(),
        };
        let r = saturation_plateau(&mut probe, 1, 256);
        let w = r.peak.value().expect("the curve saturated at c=128");
        assert_eq!(
            w.concurrency, 64,
            "the noisy final rung must be demoted off, leaving c=64 as the published rung"
        );
        assert!(
            w.knee_concurrency <= w.concurrency,
            "knee c={} sits above the rung the peak was measured at (c={}), which is a pair no \
             reading supports",
            w.knee_concurrency,
            w.concurrency
        );
        assert_eq!(
            w.knee_concurrency, 16,
            "the knee is the lowest rung indistinguishable from what was PUBLISHED (1010), not from \
             a maximum the search declined to publish"
        );
    }

    // A curve that never stops climbing has no plateau, and the range bound is OUR choice, not the
    // gateway's ceiling. Publishing it would be the same fabrication at the other end of the search.
    #[test]
    fn a_curve_still_climbing_at_the_bound_is_exhausted_never_the_bound_itself() {
        struct Rising;
        impl Probe for Rising {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                Some(Sample::new(c as f64 * 100.0, true))
            }
        }
        let r = saturation_plateau(&mut Rising, 1, 512);
        assert!(r.exhausted);
        assert_eq!(
            r.peak.value(),
            None,
            "a lower bound must never be published as a plateau"
        );
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
        let mut probe = Saturating {
            knee: 16,
            plateau: 2000.0,
            wobble: 0.10,
            calls: 0,
        };
        let r = saturation_plateau(&mut probe, 1, 8192);
        assert!(!r.exhausted, "a flat-but-noisy curve must still saturate");
        let w = r.peak.value().expect("saturated");
        assert!(
            w.concurrency <= 512,
            "noise carried the search to c={}",
            w.concurrency
        );
    }

    // The calibration must actually re-probe: a memoised repeat would hand back the first window's
    // answer, report a spread of zero, and quietly restore the guessed-threshold bug.
    #[test]
    fn the_calibration_really_reprobes_rather_than_reading_the_memo() {
        let mut probe = Saturating {
            knee: 8,
            plateau: 900.0,
            wobble: 0.03,
            calls: 0,
        };
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
                Some(Sample::new(0.0, false))
            }
        }
        let r = saturation_plateau(&mut NeverPasses, 1, 64);
        assert_eq!(r.peak.value(), None);
        assert_eq!(r.peak.reason(), Some(&Absent::NotMeasured));
        assert!(r
            .peak
            .detail()
            .unwrap_or_default()
            .contains("passed the gate"));
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
                Some(Sample::new(c as f64 * 10.0, true))
            }
        }
        let r = saturation_plateau(&mut Interrupter { calls: 0 }, 1, 4096);
        assert_eq!(r.peak.value(), None);
        assert!(
            !r.exhausted,
            "an interruption is not the same fact as running out of range"
        );
        assert!(
            !r.points.is_empty(),
            "the probes that did land are still evidence"
        );
        // RigLimited, not NotMeasured: the interruption is the RIG failing to finish asking, never
        // a fact about the gateway, and it must stay distinguishable from "every rung genuinely
        // failed the gate" (NotMeasured), which callers may publish as a measured zero.
        assert_eq!(
            r.peak.reason(),
            Some(&Absent::RigLimited),
            "an interrupted search is a rig limit, not an unmeasured gateway"
        );
        assert!(
            r.peak.detail().unwrap_or_default().contains("interrupted"),
            "the absence must say the search was interrupted: {:?}",
            r.peak.detail()
        );
    }

    // THE ZERO-BY-COLLISION DEFECT THIS PINS. `run::sweep_cpu_fps_cell` publishes a measured 0 when
    // the peak is absent with reason NotMeasured and every probed point failed its gate - the
    // honest "the gateway carried nothing" verdict. An interruption that lands AFTER failing rungs
    // produces exactly that point shape, so when `interrupted()` also said NotMeasured the rig's
    // own abort (a refused window, an exhausted port range) was published as the gateway's zero.
    // The interrupted reason must stay RigLimited even when every point seen so far had failed.
    #[test]
    fn an_interruption_after_only_failing_rungs_stays_rig_limited_never_the_gateways_zero() {
        struct FailsThenDies {
            calls: u32,
        }
        impl Probe for FailsThenDies {
            fn probe(&mut self, _c: u32) -> Option<Sample> {
                self.calls += 1;
                if self.calls > WINDOWS_PER_RUNG as u32 {
                    return None;
                }
                // One full rung of gate-failing windows before the rig gives out.
                Some(Sample::new(0.0, false))
            }
        }
        let r = saturation_plateau(&mut FailsThenDies { calls: 0 }, 1, 64);
        assert!(
            !r.points.is_empty() && r.points.iter().all(|p| !p.passed),
            "the fixture must produce the all-points-failed shape the measured-zero rule keys on"
        );
        assert_eq!(r.peak.value(), None);
        assert_eq!(
            r.peak.reason(),
            Some(&Absent::RigLimited),
            "an interruption after failing rungs must not collapse into the NotMeasured that reads \
             as a measured gateway zero downstream"
        );
        assert!(
            r.peak.detail().unwrap_or_default().contains("interrupted"),
            "the absence must say the search was interrupted: {:?}",
            r.peak.detail()
        );
    }

    // The range given backwards is the same interval, so it must produce the same answer rather than
    // silently searching nothing.
    #[test]
    fn a_plateau_range_given_backwards_searches_the_same_interval() {
        let mut a = Saturating {
            knee: 32,
            plateau: 4000.0,
            wobble: 0.01,
            calls: 0,
        };
        let mut b = Saturating {
            knee: 32,
            plateau: 4000.0,
            wobble: 0.01,
            calls: 0,
        };
        let fwd = saturation_plateau(&mut a, 1, 2048);
        let rev = saturation_plateau(&mut b, 2048, 1);
        assert_eq!(
            fwd.peak.value().map(|w| w.concurrency),
            rev.peak.value().map(|w| w.concurrency)
        );
    }

    // THE START IS THE FLOOR, ALWAYS, AND DOES NOT MOVE WITH THE RANGE. A start derived from the
    // range is what made a 1..65536 run open by asking for 32768 concurrent connections, and it also
    // made every gateway's published evidence begin at a different, arbitrary place.
    #[test]
    fn the_search_always_opens_at_the_floor_however_wide_the_range() {
        for hi in [64u32, 4096, 65536] {
            let mut probe = Saturating {
                knee: 16,
                plateau: 700.0,
                wobble: 0.01,
                calls: 0,
            };
            let r = saturation_plateau(&mut probe, 1, hi);
            let first = r.points.first().map(|p| p.concurrency);
            assert_eq!(
                first,
                Some(1),
                "with hi={hi} the search opened at {first:?} instead of the floor"
            );
        }
    }

    // Every probed rung travels, in probe order, whichever way the search went: the published sweep
    // is what lets a reader re-derive the plateau instead of trusting it.
    #[test]
    fn the_probe_trace_travels_with_the_result() {
        let mut probe = Saturating {
            knee: 64,
            plateau: 3000.0,
            wobble: 0.02,
            calls: 0,
        };
        let r = saturation_plateau(&mut probe, 1, 4096);
        assert!(
            r.points.len() > 4,
            "too few points to re-derive anything: {:?}",
            r.points.len()
        );
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
            Some(Sample::new(c as f64, c <= self.ceiling))
        }
    }

    struct AlwaysPasses;
    impl Probe for AlwaysPasses {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            Some(Sample::new(c as f64, true))
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
            let level = if c >= self.knee {
                self.plateau
            } else {
                self.plateau * (c as f64 / self.knee as f64)
            };
            Some(Sample::new(level * (1.0 + sign * self.wobble), true))
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
                // The knee travels in its own field, at or above the true knee but nowhere near the
                // top of a range 30+ doublings wide.
                prop_assert!(w.knee_concurrency <= knee.saturating_mul(8).max(64),
                    "knee={} reported knee c={}", knee, w.knee_concurrency);
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
                    Some(Sample::new(c as f64, c <= 50))
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
            Some(Sample::new(v, true))
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
        RecordedWindows {
            by_conc,
            taken: Default::default(),
            beyond: 26098.0,
        }
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
        assert!(
            top > 32,
            "the ladder stopped at c={top}, the same rung the field run stopped at"
        );
    }

    // The bar is the uncertainty of the rung's MEDIAN, so more windows behind a median make it
    // stricter, not looser. Without the divisor a rung's bar is its raw window range, which is what
    // let scatter masquerade as a plateau.
    #[test]
    fn the_improvement_bar_tightens_as_a_rung_gathers_more_windows() {
        let spread = 0.20;
        assert!(improvement_bar(spread, 9) < improvement_bar(spread, 3));
        assert!(
            improvement_bar(spread, 3) < spread,
            "the median is steadier than its windows' range"
        );
        // Never below the floor, however tidy the windows look: three can agree by luck.
        assert_eq!(improvement_bar(0.0, 3), WOBBLE_FLOOR);
        assert_eq!(improvement_bar(0.001, 100), WOBBLE_FLOOR);
        // A rung with no passing windows cannot divide by zero.
        assert_eq!(improvement_bar(0.0, 0), WOBBLE_FLOOR);
    }

    // ── THE INVARIANT BOTH SEARCHES MUST HOLD, ASSERTED ON BOTH ─────────────────────────────────
    //
    // This exists because the same defect was found twice, a day apart, in two functions.
    //
    // `saturation_plateau` used to derive its opening probe from the range, so widening the range
    // made the FIRST request bigger - a 1..65536 run began by asking for 32768 concurrent
    // connections. That was fixed, and the reason was written down in a comment inside
    // `saturation_plateau`. `bisect_ceiling` had the same defect the whole time, worse: it probed
    // `max_conc` outright as its second move. Nobody looked, because what was recorded was a note in
    // the function that got fixed rather than a rule both functions have to obey. Raising the engine
    // ceiling to 65536 then turned the untouched one into "open by asking a gateway for 65536
    // concurrent streams".
    //
    // A comment cannot fail. This can, and it runs against every search in the module, so a third
    // search added later is held to it without anyone remembering this happened.
    //
    // THE RULE: a load search may never ask for a concurrency the gateway has not already justified.
    // It opens at the floor, and no probe exceeds twice the highest concurrency that has passed so
    // far. That bounds the blast radius on a fragile gateway, and it keeps the failure attributable:
    // a rig that opens beyond what the gateway can carry learns only that the top failed, and the
    // failure may be its own - load generator, mock and gateway all hit the wall together and the
    // result cannot say which was first.
    struct RecordingProbe {
        ceiling: u32,
        asked: std::rc::Rc<std::cell::RefCell<Vec<u32>>>,
    }
    impl Probe for RecordingProbe {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            self.asked.borrow_mut().push(c);
            Some(Sample::new(
                f64::from(c.min(self.ceiling)),
                c <= self.ceiling,
            ))
        }
    }

    /// THE LADDER NEVER LEAPS: it opens at the floor, and no probe is more than double the highest
    /// concurrency already asked for.
    ///
    /// Stated over ASKED rungs rather than passing ones on purpose. The two searches disagree about
    /// what a failed rung means - a gate search learns the ceiling is below it, while the plateau
    /// search deliberately climbs to MIN_SATURATION_CONC before it will call anything saturated,
    /// because a rung that low would have its verdict decided by its own scatter. Both of those are
    /// right, and a rule written around "passed" would have to pick one and be wrong about the
    /// other. What both must obey is that the NEXT request is never more than twice the last, which
    /// is exactly what the defect broke: jumping straight to the top of the range.
    fn assert_the_ladder_never_leaps(asked: &[u32], min_conc: u32, who: &str) {
        assert!(
            !asked.is_empty(),
            "{who}: a search that probed nothing proves nothing"
        );
        assert_eq!(
            asked[0], min_conc,
            "{who}: opened at c={} instead of the floor c={min_conc} - the first request a gateway \
             sees must not be a function of how wide we set the range",
            asked[0]
        );
        let mut highest = min_conc;
        for (i, &c) in asked.iter().enumerate() {
            assert!(
                c <= highest.saturating_mul(2),
                "{who}: probe {i} leapt to c={c} from a ladder that had only reached c={highest} - \
                 a search must climb, never jump to the top of its range (asked: {asked:?})"
            );
            highest = highest.max(c);
        }
    }

    #[test]
    fn no_search_leaps_past_the_ladder_it_climbed() {
        // Wide ranges are exactly where this bites: the wider the range, the worse the old opening
        // probe got, which is the property that made it invisible on narrow test ranges.
        for (min_conc, max_conc) in [(1u32, 64u32), (1, 4096), (1, 65536), (8, 65536)] {
            for ceiling in [1u32, 3, 100, 5000, 60000] {
                let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                let mut p = RecordingProbe {
                    ceiling,
                    asked: std::rc::Rc::clone(&asked),
                };
                let _ = bisect_ceiling(&mut p, min_conc, max_conc);
                assert_the_ladder_never_leaps(
                    &asked.borrow(),
                    min_conc,
                    &format!("bisect_ceiling({min_conc}..{max_conc}, gateway ceiling {ceiling})"),
                );

                let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                let mut p = RecordingProbe {
                    ceiling,
                    asked: std::rc::Rc::clone(&asked),
                };
                let _ = saturation_plateau(&mut p, min_conc, max_conc);
                assert_the_ladder_never_leaps(
                    &asked.borrow(),
                    min_conc,
                    &format!(
                        "saturation_plateau({min_conc}..{max_conc}, gateway ceiling {ceiling})"
                    ),
                );
            }
        }
    }

    // A SLOW CLIMB UNDER NOISE IS STILL A CLIMB.
    //
    // REPLAY OF kong openai>bedrock, 2026-07-28 field run, its own recorded windows. Throughput
    // rises all the way to c~94, but each doubling buys only 2-5% while the window spread is 19-26%,
    // so the noise bar is LARGER than the real per-step gain and every rung reads as flat. With two
    // flat rungs as the stopping rule the search quit at c=32 and published 15909 as a maximum; the
    // sustained search, which does not stop on flatness, measured 17898 on the same box. A maximum
    // another measurement beats is not a maximum, and this is the whole C6 inversion class.
    struct KongCurve {
        rungs: std::collections::BTreeMap<u32, Vec<f64>>,
        taken: std::collections::BTreeMap<u32, usize>,
    }
    impl Probe for KongCurve {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            let i = self.taken.entry(c).or_insert(0);
            // Above what the max search actually probed, the sustained search's own readings at the
            // same concurrencies stand in: same box, same mock, same cell, minutes apart.
            let v = self
                .rungs
                .get(&c)
                .map(|w| w[(*i).min(w.len() - 1)])
                .unwrap_or(17_800.0);
            *i += 1;
            Some(Sample::new(v, true))
        }
    }

    #[test]
    fn a_curve_that_climbs_slower_than_its_own_noise_is_not_saturated() {
        let mut rungs = std::collections::BTreeMap::new();
        rungs.insert(1u32, vec![3347.0, 3348.0, 3351.0]);
        rungs.insert(2, vec![5198.0, 5208.0, 6554.0]);
        rungs.insert(4, vec![7253.0, 8368.0, 10886.0]);
        rungs.insert(8, vec![12154.0, 14834.0, 15052.0]);
        rungs.insert(16, vec![12434.0, 15556.0, 16160.0]);
        rungs.insert(32, vec![12262.0, 15909.0, 16548.0]);
        // What the sustained search measured above where the max search gave up.
        rungs.insert(64, vec![17466.0; 3]);
        rungs.insert(128, vec![17829.0; 3]);

        let mut p = KongCurve {
            rungs,
            taken: Default::default(),
        };
        let r = saturation_plateau(&mut p, 1, 4096);
        let peak = match r.peak {
            Measurement::Measured(pt) => pt.value,
            ref other => panic!("kong's curve must produce a peak, got {other:?}"),
        };

        // The sustained search measured 17898 on this cell. A published maximum below that is one
        // another measurement already beat.
        assert!(
            peak > 17_898.0 * 0.95,
            "published {peak:.0} as the maximum, but the sustained search measured 17898 on the same \
             box - the search stopped while the curve was still climbing"
        );
        // Concretely: it must get past the rung where two-flat-rungs used to quit.
        let top = r.points.iter().map(|x| x.concurrency).max().unwrap_or(0);
        assert!(
            top >= 128,
            "the ladder stopped at c={top}; kong keeps climbing to c~94"
        );
    }

    // The other half of the same rule: a curve that really HAS levelled off must still stop, and
    // promptly. Requiring more evidence to call saturation must not mean never calling it.
    #[test]
    fn a_genuinely_flat_curve_still_stops_quickly() {
        struct Flat;
        impl Probe for Flat {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                // Rises to c=16 then dead flat, with clean windows.
                Some(Sample::new(f64::from(c.min(16)) * 1000.0, true))
            }
        }
        let r = saturation_plateau(&mut Flat, 1, 65536);
        let top = r.points.iter().map(|x| x.concurrency).max().unwrap_or(0);
        assert!(
            top <= 256,
            "a flat curve ran to c={top} - three flat rungs must still be a prompt stop, not a licence \
             to climb the whole range"
        );
        match r.peak {
            Measurement::Measured(pt) => assert_eq!(pt.value, 16_000.0),
            ref other => panic!("a flat curve has a plateau, got {other:?}"),
        }
    }

    // A curve that creeps up by LESS than its own wobble on every rung never resets flat_run, but its
    // best median keeps landing on the newest rung - kong's shape on the 2026-07-28 board, which
    // published the search's own final rung as the gateway's maximum ("the max_proxy sweep WON at the
    // highest concurrency it probed", the site's structural invariant, blocked the deploy on it).
    // Drift inside the noise band is saturation (`a_plateau_that_drifts_upward_inside_the_noise_is_
    // still_saturated` pins that), so this must still PUBLISH - but from an interior rung: the final
    // rung is a proven non-improver and stays as the observed rung above the winner.
    // A LADDER THAT ENDS ON FAILING RUNGS FOUND THE CEILING; IT DID NOT RUN OUT OF RANGE.
    //
    // Bifrost's cpu_fps, 2026-07-29: c=1024 passed at 43,404 frames/sec with zero stalls, then c=2048
    // failed all three windows (~5,000 stalls) and c=4096 failed all three (~7,000). The search
    // published NOTHING - `SearchExhausted`, "still climbing when the range ran out" - because a
    // rung whose windows all fail has a median of zero, is therefore counted as merely "flat", and
    // only two such rungs accumulated where the stop rule wants three.
    //
    // Raising the ceiling only moves where that happens: the rungs above are failing either way, and
    // failing rungs are evidence, not the absence of it. The best passing rung IS the measurement of
    // the ceiling those failures prove exists.
    #[test]
    fn a_ladder_that_ends_on_failing_rungs_publishes_the_best_passing_rung() {
        // Bifrost's shape: healthy and climbing to c=1024, then the rig comes apart above it.
        struct RigCollapse;
        impl Probe for RigCollapse {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                if c > 1024 {
                    // Windows that FAIL the gate. They still carried frames - that is what made this
                    // look like "still climbing" - but nothing they measured is the gateway's.
                    Some(Sample::new(70_000.0, false))
                } else {
                    Some(Sample::new(f64::from(c) * 42.0, true))
                }
            }
        }
        let r = saturation_plateau(&mut RigCollapse, 1, 4096);
        assert!(
            !r.exhausted,
            "the ladder found a ceiling rather than running out of range, so this is not exhaustion"
        );
        let pt = match r.peak {
            Measurement::Measured(pt) => pt,
            ref other => panic!(
                "the best passing rung is a real measurement and must be published, got {other:?}"
            ),
        };
        assert_eq!(
            pt.concurrency, 1024,
            "the published peak is the highest rung that actually held, not one of the failures"
        );
        assert!(
            (pt.value - 43_008.0).abs() < 1.0,
            "and its own median, not a number borrowed from a failing rung: {}",
            pt.value
        );

        // THE CASE THAT MUST STILL BE EXHAUSTION: every rung passes and throughput is still rising
        // when the ladder ends. Our range is our choice, and calling its top the gateway's ceiling
        // would publish the search's own bound under the gateway's name.
        struct StillClimbing;
        impl Probe for StillClimbing {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                Some(Sample::new(f64::from(c) * 42.0, true))
            }
        }
        let r = saturation_plateau(&mut StillClimbing, 1, 4096);
        assert!(
            r.exhausted,
            "a ladder whose every rung passed and kept improving really did run out of range"
        );
    }

    #[test]
    fn the_published_peak_never_sits_on_the_last_probed_rung() {
        struct Creep;
        impl Probe for Creep {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                // +0.5% per doubling: under the wobble floor, so every rung reads "flat" while the
                // best median still lands on the newest rung every time.
                let doublings = 31 - c.leading_zeros();
                Some(Sample::new(
                    10_000.0 * (1.0 + 0.005 * f64::from(doublings)),
                    true,
                ))
            }
        }
        let r = saturation_plateau(&mut Creep, 1, 65_536);
        assert!(!r.exhausted, "drift inside the noise band is a plateau");
        let top_probed = r.points.iter().map(|p| p.concurrency).max().unwrap_or(0);
        let pt = match r.peak {
            Measurement::Measured(pt) => pt,
            ref other => panic!("a creeping plateau still publishes a peak, got {other:?}"),
        };
        assert!(
            pt.concurrency < top_probed,
            "published the peak at c={} with nothing probed above it (top probed c={top_probed}) - \
             that is the search's own stopping point wearing the gateway's name",
            pt.concurrency
        );
    }
}
