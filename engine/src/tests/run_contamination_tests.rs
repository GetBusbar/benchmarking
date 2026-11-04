use super::*;

/// The guard must NOT fire where draining cannot explain anything - otherwise it becomes a
/// universal excuse that relabels real gateway failures as rig faults, which flatters the
/// subject and is the more dangerous mirror of the bug it fixes.
#[test]
fn the_contamination_guard_only_applies_where_draining_could_explain_it() {
    // The real guard `sweep_streams_cell` applies, not a copy of its expression.
    let fires = stream_contaminated;
    assert!(fires(4096, 3088), "busbar's real case must be caught");
    assert!(
        !fires(2, 1),
        "a rung failing at c=1 under c=2 has no residue to blame"
    );
    assert!(
        !fires(4096, 5000),
        "a failure ABOVE the proven rung is ordinary evidence"
    );
    assert!(
        !fires(STREAM_SETTLE_FREE_BELOW, 8),
        "at or below the settle threshold, no excuse"
    );
}

/* A RUNG FAILING BELOW ONE THE SAME CELL ALREADY PASSED IS OURS, AND MUST BE FILED AS OURS.
This is the whole point of the variant: `Absent::NotMeasured` puts a finding among the
GATEWAY's results, which is what happened to 6 cells on the 2026-07-31 board. */
#[test]
fn a_contaminated_rung_is_a_harness_error_and_never_the_gateways_result() {
    assert_eq!(
        StreamStop::RigContaminated {
            at: 3088,
            proven: 4096
        }
        .absent_kind(),
        Absent::HarnessError,
        "a rung failing below a proven-clean one is the rig; filing it as NotMeasured charges \
             the gateway for our own undrained host"
    );
    // The endings that ARE about the gateway must stay that way, or this variant has just
    // laundered every real failure into a rig excuse.
    assert_eq!(
        StreamStop::SteppedRungFailed { at: 100 }.absent_kind(),
        Absent::NotMeasured
    );
    assert_eq!(
        StreamStop::BudgetExhausted.absent_kind(),
        Absent::NotMeasured
    );
    assert_eq!(
        StreamStop::FloorReached { last: 8 }.absent_kind(),
        Absent::NotMeasured
    );
}

/// The sentence has to carry BOTH concurrencies, because the impossibility is the relation
/// between them - either number alone reads like an ordinary failure.
#[test]
fn the_contaminated_reason_names_the_rung_and_what_the_cell_had_already_carried() {
    let d = StreamStop::RigContaminated {
        at: 3088,
        proven: 4096,
    }
    .describe(6176, 6);
    assert!(d.contains("c=3088"), "must name the rung that failed: {d}");
    assert!(
        d.contains("c=4096"),
        "must name what this cell already carried: {d}"
    );
    assert!(
        d.contains("c=6176"),
        "must name what the bisection proved: {d}"
    );
}

/* THE SETTLE IS PROPORTIONAL, FREE AT THE BOTTOM, AND CAPPED. Each of those is load-bearing: a
flat pause sized for c=8,192 would be pure schedule cost on the dozen small rungs every cell
walks through, and an uncapped one is a field sweep whose duration nobody predicted. */
#[test]
fn the_stream_settle_is_free_below_the_threshold_and_capped_above_it() {
    // The clock rule `settle_after_streams` falls back to, called rather than restated.
    let ms = settle_clock_ms;
    assert_eq!(ms(1), 0, "a single stream leaves nothing to drain");
    assert_eq!(
        ms(STREAM_SETTLE_FREE_BELOW),
        0,
        "at the threshold it is still free"
    );
    assert!(
        ms(STREAM_SETTLE_FREE_BELOW + 1) > 0,
        "and just above it, it is not"
    );
    assert!(
        ms(2048) > ms(1024),
        "it must scale with what the last window drove"
    );
    assert_eq!(ms(100_000), STREAM_SETTLE_MAX_MS, "and it must be bounded");
    // busbar's real case: the c=3,088 step-down that failed after c=6,144 passed clean.
    assert!(
        ms(6176) >= 6_000,
        "the rungs that actually broke must get a real pause, got {}",
        ms(6176)
    );
}
