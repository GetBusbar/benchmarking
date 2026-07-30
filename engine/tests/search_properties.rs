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

use otb_engine::measurement::Absent;
use otb_engine::search::{bisect_ceiling, climb_rungs, BisectResult, Probe, ProbedPoint, Sample};
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


    // ── climb_rungs: the ladder walk that replaced `saturation_plateau` ──────────────────────────
    //
    // The retired search's properties were about a VERDICT it reached - the plateau level, the rung it
    // published, whether it flagged itself exhausted. `climb_rungs` reaches no verdict, so its
    // invariants are all about COVERAGE and TERMINATION: it must probe every rung the frontier could
    // read, and it must stop.

    // THE CLIMB REACHES THE FAILURE BOUNDARY, WHATEVER THE CURVE'S SHAPE DOES.
    //
    // This is the property the retired flat-run counter broke. It stopped after three rungs that did
    // not improve by more than their own measured noise, so a curve creeping up inside its wobble - or
    // one that had genuinely plateaued but kept serving cleanly far above the knee - was abandoned
    // early and the rungs above it never entered the record. kong published 15909 that way while the
    // sustained reading of the same windows reached 17898.
    //
    // Now the only stopping condition is a rung that serves nothing cleanly, so for ANY curve shape
    // the climb must reach the gateway's real failure point (or the top of the range).
    #[test]
    fn the_climb_always_reaches_the_failure_boundary_whatever_the_curve_does(
        limit in 1u32..4096,
        knee in 2u32..64,
        level in 100.0f64..1_000_000.0,
    ) {
        // Serves cleanly up to `limit`, and its RATE plateaus at `knee` - far below `limit`. A search
        // that stopped on flatness would quit near the knee and never see the boundary.
        struct PlateauThenFail { knee: u32, level: f64, limit: u32 }
        impl Probe for PlateauThenFail {
            fn probe(&mut self, c: u32) -> Option<Sample> {
                let v = if c >= self.knee { self.level } else { self.level * f64::from(c) / f64::from(self.knee) };
                Some(Sample::new(v, c <= self.limit))
            }
        }
        let max_conc = 65_536;
        let mut probe = PlateauThenFail { knee, level, limit };
        let points = climb_rungs(&mut probe, 1, max_conc);
        let top = points.iter().map(|p| p.concurrency).max().unwrap_or(0);
        prop_assert!(
            top > limit || top == max_conc,
            "climb stopped at c={top} with a clean limit of {limit} and a knee at {knee}: a rung that \
             still served was never probed"
        );
        // Every rung it recorded below the limit served cleanly, so the frontier can read them.
        prop_assert!(points.iter().filter(|p| p.concurrency <= limit).all(|p| p.passed));
    }

    // IT ALWAYS TERMINATES, and only ever inside the range. An unbounded climb would hang a cell; a
    // climb that probed past `max_conc` would ask the box for load the caller ruled out.
    #[test]
    fn the_climb_terminates_within_the_range_for_any_limit(
        floor in 1u32..64,
        span in 1u32..8192,
        limit in 0u32..16_384,
    ) {
        let max_conc = floor + span;
        let mut probe = MonotoneGate { ceiling: limit };
        let points = climb_rungs(&mut probe, floor, max_conc);
        assert_points_within(&points, floor, max_conc);
        assert_no_ladder_leap(&points, floor);
    }

    // A CURVE THAT NEVER SERVES CLEANLY YIELDS RUNGS BUT NO CLEAN ONE - never an invented zero, and
    // never a climb that keeps going. The floor failed; nothing above it can un-fail it.
    #[test]
    fn a_gateway_that_fails_at_the_floor_is_probed_once_and_no_further(
        floor in 1u32..64,
        span in 1u32..4096,
    ) {
        let mut probe = MonotoneGate { ceiling: 0 };
        let points = climb_rungs(&mut probe, floor, floor + span);
        prop_assert!(!points.is_empty(), "the floor is still probed and still recorded");
        prop_assert!(points.iter().all(|p| p.concurrency == floor),
            "nothing above a failing floor may be probed");
        prop_assert!(points.iter().all(|p| !p.passed));
    }
}
