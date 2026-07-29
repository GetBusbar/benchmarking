// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// PROPERTY TESTS OVER THE SEARCHES. The unit tests in `search.rs` pin single worked examples; each
// property here holds an invariant across thousands of generated gates and curves, because every
// defect this module has actually shipped (publishing the range bound, publishing the harness's own
// floor, publishing the last probed rung) was an invariant broken at a shape no worked example
// happened to cover.
//
// The invariants, stated once:
//   - a bisected ceiling is exact: pass proven at n, failure measured at n+1, for ANY true ceiling
//     anywhere in the range - and never a number when the truth lies outside the range;
//   - no search ever probes outside [floor, max_conc], and no probe leaps past the doubling ladder;
//   - `exhausted` and `reason() == SearchExhausted` are the same claim, never allowed to disagree;
//   - a measured peak is a rung that really ran, carrying that rung's own value - never the last
//     rung probed, never a pair assembled from two different rungs;
//   - a strictly-rising curve at the bound is always an absence, never a published number.

// The crate denies unwrap/expect/panic so a measurement defect can never abort a run. A test is the
// opposite case: failures must be loud. Scoped to this file, same as engine/tests/end_to_end.rs.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use otb_engine::measurement::{Absent, Measurement};
use otb_engine::search::{
    bisect_ceiling, saturation_plateau, saturation_plateau_gated, BisectResult, PeakResult, Probe,
    ProbedPoint, Reading, Sample,
};
use proptest::prelude::*;

// ---- probes -------------------------------------------------------------------------------------

/// A gate that is exactly monotone: passes iff c <= ceiling. The value reported is the concurrency
/// itself, so a leaked search-internal number is recognisable in the output.
struct MonotoneGate {
    ceiling: u32,
}
impl Probe for MonotoneGate {
    fn probe(&mut self, c: u32) -> Option<Sample> {
        Some(Sample::new(c as f64, c <= self.ceiling))
    }
}

/// A throughput curve that rises linearly to a knee and then holds a flat plateau - the honest
/// shape `saturation_plateau`'s own doc says a healthy gateway has. Deterministic (zero window
/// spread), so a rung's median is exactly f(c).
struct StepCurve {
    knee: u32,
    level: f64,
}
impl StepCurve {
    fn f(&self, c: u32) -> f64 {
        if c >= self.knee {
            self.level
        } else {
            self.level * c as f64 / self.knee as f64
        }
    }
}
impl Probe for StepCurve {
    fn probe(&mut self, c: u32) -> Option<Sample> {
        Some(Sample::new(self.f(c), true))
    }
}

/// A curve that never stops rising: f(c) = c. At the top of any range it is still improving by a
/// full doubling, so the only honest verdict is SearchExhausted.
struct EverRising;
impl Probe for EverRising {
    fn probe(&mut self, c: u32) -> Option<Sample> {
        Some(Sample::new(c as f64, true))
    }
}

/// An arbitrary-but-deterministic probe: pass/fail and value are a pure function of c (seeded), so
/// repeated windows at one rung agree and the global invariants can be checked against any shape,
/// including non-monotone ones no real fixture would produce on demand.
struct HashedProbe {
    seed: u64,
}
impl HashedProbe {
    fn at(&self, c: u32) -> (f64, bool) {
        // splitmix64: cheap, deterministic, and spreads a seed+c pair across the whole shape space.
        let mut z = self.seed ^ (u64::from(c)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        let passed = z & 3 != 0; // pass ~75% of rungs, so both verdict shapes occur often
        let value = ((z >> 8) & 0xFFFF) as f64 + 1.0;
        (value, passed)
    }
}
impl Probe for HashedProbe {
    fn probe(&mut self, c: u32) -> Option<Sample> {
        let (value, passed) = self.at(c);
        Some(Sample::new(value, passed))
    }
}

// ---- shared assertions --------------------------------------------------------------------------

/// Every probe of any search stays inside the caller's range: below-floor and above-ceiling probes
/// are requests the range never authorised, and an above-ceiling one is the "opened at 32768" defect.
fn assert_points_within(points: &[ProbedPoint], floor: u32, max_conc: u32) {
    for p in points {
        assert!(
            p.concurrency >= floor && p.concurrency <= max_conc,
            "probed c={} outside the authorised range [{floor}, {max_conc}]",
            p.concurrency
        );
    }
}

/// No probe leaps past the doubling ladder: every probed concurrency is either the floor or at most
/// double something already probed. This is `no_search_leaps_past_the_ladder_it_climbed` as a
/// reusable check, holding bisection mids to the same rule as climb rungs.
fn assert_no_ladder_leap(points: &[ProbedPoint], floor: u32) {
    let mut seen_max = floor.max(1);
    for p in points {
        assert!(
            p.concurrency <= seen_max.saturating_mul(2),
            "probe at c={} leapt past the ladder (highest previously probed: {seen_max})",
            p.concurrency
        );
        seen_max = seen_max.max(p.concurrency);
    }
}

/// `exhausted` is a plain-bool mirror of `reason() == SearchExhausted`; the two must never diverge,
/// or a caller reading the bool suppresses a different absence than the reason names.
fn assert_exhausted_coherent(r: &PeakResult) {
    let by_reason = r.peak.reason() == Some(&Absent::SearchExhausted);
    assert_eq!(
        r.exhausted,
        by_reason,
        "exhausted={} but reason={:?}: the bool and the reason are one claim and must agree",
        r.exhausted,
        r.peak.reason()
    );
    if r.peak.is_measured() {
        assert!(!r.exhausted, "a measured peak cannot also claim exhaustion");
    }
}

// ---- properties ---------------------------------------------------------------------------------

proptest! {
    // THE EXACTNESS CONTRACT: for a monotone gate whose true ceiling lies strictly inside the range,
    // the bisection publishes exactly that ceiling - and the trace carries the proof: a pass at n
    // and a measured failure at n+1. Anything else is a fabricated number.
    #[test]
    fn bisect_publishes_the_exact_interior_ceiling_with_its_proof(
        lo in 1u32..64,
        span in 2u32..4096,
        offset in 0u32..4095,
    ) {
        let hi = lo + span;
        let ceiling = lo + (offset % span); // in [lo, hi-1]: strictly inside
        let mut probe = MonotoneGate { ceiling };
        let r: BisectResult = bisect_ceiling(&mut probe, lo, hi);
        prop_assert_eq!(
            r.ceiling.copied(), Some(ceiling),
            "true ceiling {} in [{}, {}] must be found exactly", ceiling, lo, hi
        );
        prop_assert!(
            r.points.iter().any(|p| p.concurrency == ceiling && p.passed),
            "the published ceiling must have been probed and passed"
        );
        prop_assert!(
            r.points.iter().any(|p| p.concurrency == ceiling + 1 && !p.passed),
            "a ceiling is only a ceiling with a measured failure at n+1 in the trace"
        );
        assert_points_within(&r.points, lo, hi);
        assert_no_ladder_leap(&r.points, lo);
    }

    // A ceiling AT OR ABOVE the top of the range is never published: the top passing proves only a
    // lower bound the search itself chose, and publishing it is how unrelated gateways come to share
    // an identical number. Holds for every (range, ceiling >= hi) pair, not one example.
    #[test]
    fn bisect_never_publishes_a_number_when_the_truth_is_at_or_past_the_range(
        lo in 1u32..64,
        span in 1u32..4096,
        beyond in 0u32..10_000,
    ) {
        let hi = lo + span;
        let mut probe = MonotoneGate { ceiling: hi + beyond };
        let r = bisect_ceiling(&mut probe, lo, hi);
        prop_assert_eq!(r.ceiling.copied(), None, "the range bound is ours, not the gateway's");
        prop_assert_eq!(r.ceiling.reason(), Some(&Absent::SearchExhausted));
        assert_points_within(&r.points, lo, hi);
    }

    // A ceiling BELOW a floor above 1 was never probed, so no specific number may be published for
    // it - 0 there would be the same fabrication as the range bound at the top end. Only a floor of
    // 1 can prove the measured "nothing sustains this gate" zero.
    #[test]
    fn bisect_below_floor_is_only_a_measured_zero_when_the_floor_is_one(
        lo in 2u32..256,
        span in 1u32..1024,
        below in 0u32..255,
    ) {
        let hi = lo + span;
        let ceiling = below % lo; // in [0, lo-1]: strictly below the floor
        let mut probe = MonotoneGate { ceiling };
        let r = bisect_ceiling(&mut probe, lo, hi);
        prop_assert_eq!(
            r.ceiling.copied(), None,
            "a floor above 1 that fails proves only 'below the floor'; publishing a number invents one"
        );
        prop_assert_eq!(r.ceiling.reason(), Some(&Absent::SearchExhausted));

        // The floor-of-one contrast, same gate: 0 becomes a real measured result.
        let mut probe = MonotoneGate { ceiling: 0 };
        let r1 = bisect_ceiling(&mut probe, 1, hi);
        prop_assert_eq!(r1.ceiling.copied(), Some(0));
    }

    // The memo contract: bisection never probes one concurrency twice. A duplicate window is a paid
    // cost with no new information, and the trace would double-count evidence.
    #[test]
    fn bisect_never_probes_the_same_concurrency_twice(
        lo in 1u32..64,
        span in 2u32..4096,
        offset in 0u32..4095,
    ) {
        let hi = lo + span;
        let mut probe = MonotoneGate { ceiling: lo + (offset % span) };
        let r = bisect_ceiling(&mut probe, lo, hi);
        let mut seen = std::collections::BTreeSet::new();
        for p in &r.points {
            prop_assert!(seen.insert(p.concurrency), "c={} probed twice", p.concurrency);
        }
    }

    // Reversed bounds are normalised, not obeyed: the same gate must yield the same ceiling whether
    // the caller wrote (lo, hi) or (hi, lo). An un-normalised range would make the search's answer a
    // function of argument order rather than of the gateway.
    #[test]
    fn bisect_is_indifferent_to_argument_order(
        lo in 1u32..64,
        span in 2u32..2048,
        offset in 0u32..2047,
    ) {
        let hi = lo + span;
        let ceiling = lo + (offset % span);
        let fwd = bisect_ceiling(&mut MonotoneGate { ceiling }, lo, hi);
        let rev = bisect_ceiling(&mut MonotoneGate { ceiling }, hi, lo);
        prop_assert_eq!(fwd.ceiling.copied(), rev.ceiling.copied());
    }

    // THE PLATEAU CONTRACT on the honest curve shape: rises to a knee, then holds. The published
    // peak is the plateau LEVEL, at a rung that really ran, and never the last rung probed. The
    // ranges here guarantee the plateau is observable well inside the range (knee*16 <= max), so a
    // published absence would be the search failing to see a plateau that was there.
    #[test]
    fn a_reachable_plateau_is_published_at_its_own_level_and_never_on_the_last_rung(
        knee in 2u32..64,
        mult in 16u32..64,
        level in 100.0f64..1_000_000.0,
    ) {
        let max_conc = knee * mult;
        let mut probe = StepCurve { knee, level };
        let r = saturation_plateau(&mut probe, 1, max_conc);
        assert_exhausted_coherent(&r);
        let peak = match r.peak {
            Measurement::Measured(p) => p,
            ref other => panic!("a plateau inside the range must be measured, got {other:?}"),
        };
        // The value is the plateau's own level - a reading the curve actually produced - and the
        // rung it is paired with really measured it (one measurement, not a pair from two rungs).
        prop_assert!(
            (peak.value - level).abs() < 1e-9,
            "published {} but the plateau level is {}", peak.value, level
        );
        prop_assert!(
            peak.concurrency >= knee,
            "the winning rung c={} sits below the knee {} and cannot have measured the level",
            peak.concurrency, knee
        );
        prop_assert!(peak.concurrency <= max_conc, "published above the range");
        prop_assert!(
            r.points.iter().any(|p| p.concurrency == peak.concurrency
                && (p.value - peak.value).abs() < 1e-9 && p.passed),
            "the published (value, concurrency) pair must be a probe that ran"
        );
        // Never the last rung: the last rung is by construction inside the flat-stop's noise band,
        // and publishing it is the "sweep won at the highest concurrency it probed" defect.
        let last = r.rungs.last().expect("a measured peak implies rungs").concurrency;
        prop_assert!(r.rungs.len() >= 2);
        prop_assert!(
            peak.concurrency != last,
            "the peak was published on the final probed rung c={last}"
        );
        // The knee travels as a rung that ran, inside the range.
        prop_assert!(
            r.rungs.iter().any(|s| s.concurrency == peak.knee_concurrency),
            "knee c={} is not a rung the climb measured", peak.knee_concurrency
        );
        assert_points_within(&r.points, 1, max_conc);
        assert_no_ladder_leap(&r.points, 1);
    }

    // A curve still rising at the bound is ALWAYS an absence: the bound is the harness's choice, and
    // for f(c)=c every range top would otherwise become a published "peak" equal to our own config.
    #[test]
    fn still_rising_at_the_bound_is_always_exhausted_never_a_number(
        min_conc in 1u32..16,
        max_conc in 64u32..8192,
    ) {
        let mut probe = EverRising;
        let r = saturation_plateau(&mut probe, min_conc, max_conc);
        prop_assert_eq!(
            r.peak.copied(), None,
            "a strictly rising curve has no interior peak; a number here is the range bound"
        );
        prop_assert_eq!(r.peak.reason(), Some(&Absent::SearchExhausted));
        prop_assert!(r.exhausted);
        // The evidence still travels: the probed rungs are not thrown away with the verdict.
        prop_assert!(!r.points.is_empty());
        assert_points_within(&r.points, min_conc, max_conc);
        assert_no_ladder_leap(&r.points, min_conc);
    }

    // THE GLOBAL INVARIANTS AGAINST ARBITRARY SHAPES: whatever the curve does - non-monotone,
    // gappy, adversarial - the search never probes outside its range, never leaps the ladder, never
    // lets `exhausted` and the reason disagree, and any measured peak is a probe that really ran
    // with that value, never on the final rung of a multi-rung climb.
    #[test]
    fn plateau_invariants_hold_for_arbitrary_curve_shapes(
        seed in any::<u64>(),
        min_conc in 1u32..32,
        span in 1u32..4096,
        gated in any::<bool>(),
    ) {
        let max_conc = min_conc + span;
        let mut probe = HashedProbe { seed };
        let gate = |r: &Reading| r.fail == 0 && r.p99_us.unwrap_or(u64::MAX) < 20_000;
        let r = if gated {
            saturation_plateau_gated(&mut probe, min_conc, max_conc, Some(&gate))
        } else {
            saturation_plateau(&mut probe, min_conc, max_conc)
        };
        assert_exhausted_coherent(&r);
        assert_points_within(&r.points, min_conc, max_conc);
        assert_no_ladder_leap(&r.points, min_conc);
        if let Measurement::Measured(p) = &r.peak {
            prop_assert!(p.concurrency >= min_conc && p.concurrency <= max_conc);
            prop_assert!(p.knee_concurrency >= min_conc && p.knee_concurrency <= max_conc);
            prop_assert!(
                r.points.iter().any(|pt| pt.concurrency == p.concurrency
                    && (pt.value - p.value).abs() < 1e-9 && pt.passed),
                "measured peak (c={}, v={}) is not a probe that ran and passed", p.concurrency, p.value
            );
            if r.rungs.len() >= 2 {
                let last = r.rungs.last().expect("len checked").concurrency;
                prop_assert!(
                    p.concurrency != last,
                    "peak published on the final probed rung c={last}"
                );
            }
        }
    }

    // An all-failing curve is NotMeasured, never a zero and never exhausted: no rung established any
    // throughput, and claiming exhaustion would say the range was too small when the gateway simply
    // failed everywhere.
    #[test]
    fn a_curve_that_never_passes_is_unmeasured_not_zero_and_not_exhausted(
        min_conc in 1u32..32,
        span in 1u32..2048,
    ) {
        struct AlwaysFails;
        impl Probe for AlwaysFails {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                Some(Sample::new(c as f64, false))
            }
        }
        let r = saturation_plateau(&mut AlwaysFails, min_conc, min_conc + span);
        prop_assert_eq!(r.peak.copied(), None);
        prop_assert_eq!(r.peak.reason(), Some(&Absent::NotMeasured));
        prop_assert!(!r.exhausted);
    }
}

// ---- targeted examples that a property cannot state cleanly -------------------------------------

/// A gate supplied to the gated climb keeps the search going past the throughput knee: the gate
/// ceiling routinely sits far above saturation, and stopping at the knee publishes the knee under
/// the ceiling's name. The ungated climb on the same curve stops near the knee, which is the
/// contrast that proves the gate changed the stopping rule rather than being ignored.
#[test]
fn the_gate_extends_the_climb_past_the_throughput_knee() {
    let knee = 8u32;
    let level = 10_000.0;
    // Ungated: stops a few flat rungs past the knee.
    let mut probe = StepCurve { knee, level };
    let ungated = saturation_plateau(&mut probe, 1, 4096);
    let ungated_top = ungated
        .rungs
        .iter()
        .map(|r| r.concurrency)
        .max()
        .expect("rungs");

    // Gated, with a gate that only breaks at c > 512: the climb must keep going until it sees the
    // gate fail, not stop where throughput went flat.
    struct GatedCurve {
        inner: StepCurve,
        gate_ceiling: u32,
    }
    impl Probe for GatedCurve {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            let v = self.inner.f(c);
            let reading = Reading {
                p99_us: Some(if c <= self.gate_ceiling {
                    5_000
                } else {
                    50_000
                }),
                ok: 100,
                fail: 0,
            };
            Some(Sample::new(v, true).with_reading(reading))
        }
    }
    let mut probe = GatedCurve {
        inner: StepCurve { knee, level },
        gate_ceiling: 512,
    };
    let gate = |r: &Reading| r.p99_us.is_some_and(|p| p < 20_000) && r.fail == 0;
    let gated = saturation_plateau_gated(&mut probe, 1, 4096, Some(&gate));
    let gated_top = gated
        .rungs
        .iter()
        .map(|r| r.concurrency)
        .max()
        .expect("rungs");

    assert!(
        gated_top > 512,
        "the gated climb stopped at c={gated_top} without ever seeing the gate break at 512"
    );
    assert!(
        gated_top > ungated_top,
        "the gate did not extend the climb (gated top c={gated_top}, ungated top c={ungated_top})"
    );
    // And the rung evidence tells the caller which rungs held the gate: rungs at or below the gate
    // ceiling hold, rungs above it do not.
    for r in &gated.rungs {
        if r.concurrency <= 512 {
            assert!(
                r.gate_holds,
                "rung c={} held a 5ms p99 against a 20ms gate and must be marked holding",
                r.concurrency
            );
        } else {
            assert!(
                !r.gate_holds,
                "rung c={} blew the gate and must not be marked holding",
                r.concurrency
            );
        }
    }
}

/// A probe interruption after real rungs landed keeps the evidence and publishes an absence whose
/// detail names the best passing point - never a number, because a turnover was never observed.
#[test]
fn an_interrupted_climb_keeps_its_evidence_but_publishes_no_number() {
    struct DiesAfter {
        calls: u32,
        budget: u32,
    }
    impl Probe for DiesAfter {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            self.calls += 1;
            if self.calls > self.budget {
                return None;
            }
            Some(Sample::new(c as f64 * 10.0, true))
        }
    }
    let mut probe = DiesAfter {
        calls: 0,
        budget: 7,
    };
    let r = saturation_plateau(&mut probe, 1, 4096);
    assert_eq!(r.peak.copied(), None, "no turnover was ever observed");
    // RigLimited, not NotMeasured. An interruption is the RIG failing to finish asking (a refused
    // window, an exhausted port range), never a fact about the gateway - and the distinction is
    // load-bearing: `sweep_cpu_fps_cell` publishes a MEASURED 0 when every rung genuinely failed
    // the gate, keyed on NotMeasured. Under one shared reason a rig abort became the gateway's zero.
    assert_eq!(r.peak.reason(), Some(&Absent::RigLimited));
    assert!(
        r.peak.detail().unwrap_or_default().contains("interrupted"),
        "and it says so, so the absence can be read without re-deriving it from the rungs"
    );
    assert!(!r.exhausted, "an interruption is not exhaustion");
    assert!(
        !r.points.is_empty(),
        "the rungs that ran before the interruption are evidence and must travel"
    );
    assert!(
        r.peak.detail().unwrap_or_default().contains("lower bound"),
        "the absence must say the best passing point is a lower bound, got {:?}",
        r.peak.detail()
    );
}
