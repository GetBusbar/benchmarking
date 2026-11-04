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
    // A gateway that passes everything until the rig stops answering, so every floor - 64 included
    // - takes the INTERRUPTED path. `Interrupter` passes only c <= 50, which sent floor=64 down the
    // failing-floor path instead and let the detail check pass on the floor's digits by accident.
    struct PassesUntilInterrupted {
        calls: u32,
    }
    impl Probe for PassesUntilInterrupted {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            self.calls += 1;
            (self.calls <= 1).then(|| Sample::new(c as f64, true))
        }
    }
    for floor in [1u32, 8, 16, 64] {
        let mut probe = PassesUntilInterrupted { calls: 0 };
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
        let detail = r.ceiling.detail().unwrap_or_default();
        assert!(
            detail.contains("interrupted") && detail.contains(&format!("at least {floor}")),
            "the lower bound belongs in the evidence, not in the value: {detail}"
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
    // Not merely "not 1000": an interrupted search publishes NO value at all.
    assert_eq!(r.ceiling.copied(), None);
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
    // The rig misses ONE window, the first at c=32, and would answer every later one. Ending the
    // climb there is the rule; treating the miss as a failed window would keep climbing on the two
    // repeats that answer, and that is what this must be able to tell apart.
    struct MissesOnceAt {
        at: u32,
        missed: bool,
        probed: Vec<u32>,
    }
    impl Probe for MissesOnceAt {
        fn probe(&mut self, c: u32) -> Option<Sample> {
            self.probed.push(c);
            if c == self.at && !self.missed {
                self.missed = true;
                return None;
            }
            Some(Sample::new(f64::from(c), true))
        }
    }
    let mut probe = MissesOnceAt {
        at: 32,
        missed: false,
        probed: Vec::new(),
    };
    let points = climb_rungs(&mut probe, 1, 4096);
    assert!(
        probe.missed,
        "the climb must have reached the rung the rig missed"
    );
    assert!(
        probe.probed.iter().all(|c| *c <= 32),
        "the climb must END at the window the rig could not run, not carry on: {:?}",
        probe.probed
    );
    assert!(!points.is_empty() && points.iter().all(|p| p.concurrency < 32));
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

#[test]
fn interrupted_climb_reports_partial_evidence_without_a_completed_boundary() {
    struct StopsAfterOne(bool);
    impl Probe for StopsAfterOne {
        fn probe(&mut self, _: u32) -> Option<Sample> {
            if self.0 {
                return None;
            }
            self.0 = true;
            Some(Sample::new(10.0, true))
        }
    }
    let interrupted = climb_rungs_report(&mut StopsAfterOne(false), 1, 64);
    assert!(interrupted.interrupted);
    assert_eq!(interrupted.points.len(), 1);
    let complete = climb_rungs_report(&mut MonotoneGate { ceiling: 4 }, 1, 64);
    assert!(!complete.interrupted);
    assert!(!complete.points.is_empty());
}
