use super::*;

/* THE RESTART IS AN ATTRIBUTION TEST AND ITS TWO OUTCOMES BLAME OPPOSITE THINGS. Getting this
backwards is not a cosmetic error: "a restart fixed it" means the gateway was wedged, while
"a restart did not fix it" means a brand-new process still fails, which leaves our host as the
only thing that changed. The second is the flattering direction - it would let us print a
gateway limitation we caused - so it is the one worth a test. */
fn stop_for(restart_cleared: bool, at: u32, proven: u32) -> StreamStop {
    // The mapping `sweep_streams_cell` uses, not a restatement of it.
    restart_stop(restart_cleared, at, proven)
}

#[test]
fn a_restart_that_helps_blames_the_gateway_and_one_that_does_not_blames_us() {
    assert_eq!(
        stop_for(true, 4096, 8192).absent_kind(),
        Absent::NotMeasured,
        "a restart clearing it proves the old process was wedged - that is the gateway's"
    );
    assert_eq!(
        stop_for(false, 4096, 8192).absent_kind(),
        Absent::HarnessError,
        "a FRESH gateway still failing a rung it carried leaves the host as the variable - ours"
    );
}

/// Both reasons must name the rung and what the cell had carried, because the impossibility is
/// the relation between them - either number alone reads like an ordinary failure.
#[test]
fn both_outcomes_explain_themselves_with_both_concurrencies() {
    for cleared in [true, false] {
        let d = stop_for(cleared, 4096, 8192).describe(8192, 6);
        assert!(d.contains("c=4096") && d.contains("c=8192"), "{d}");
    }
}
