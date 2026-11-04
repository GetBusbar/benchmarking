use super::*;

// ONE SENTENCE FOR FIVE OUTCOMES WAS THE BUG, so the test is that the five outcomes say five
// different things - and that the two which are OURS are attributed to the harness.
#[test]
fn each_way_the_stream_search_ends_reports_its_own_cause() {
    let proved = 3144;
    let budget = MAX_CEILING_STEPDOWNS;
    let cases = [
        StreamStop::RigRanShort {
            measured: 1,
            wanted: 3,
        },
        StreamStop::FloorReached { last: 4 },
        StreamStop::SteppedRungFailed { at: 1572 },
        StreamStop::WindowUnavailable { at: 786 },
        StreamStop::BudgetExhausted,
    ];
    let texts: Vec<String> = cases.iter().map(|c| c.describe(proved, budget)).collect();
    for (i, a) in texts.iter().enumerate() {
        for (j, b) in texts.iter().enumerate() {
            assert!(
                i == j || a != b,
                "two different endings publish the SAME sentence, which is the defect: {a}"
            );
        }
        assert!(
            a.contains("3144"),
            "every reason names the concurrency the bisection proved"
        );
    }

    // THE ATTRIBUTION, which is the half that matters. A window the RIG failed to take is not a
    // fact about the gateway, and filing it under NotMeasured would put our shortfall among the
    // gateway's results.
    assert!(
        matches!(
            StreamStop::RigRanShort {
                measured: 1,
                wanted: 3
            }
            .absent_kind(),
            Absent::HarnessError
        ),
        "a rig shortfall must be a HarnessError, not a gateway measurement"
    );
    assert!(
        matches!(
            StreamStop::WindowUnavailable { at: 1 }.absent_kind(),
            Absent::HarnessError
        ),
        "a window we could not take is our failure"
    );
    assert!(
        matches!(
            StreamStop::BudgetExhausted.absent_kind(),
            Absent::NotMeasured
        ),
        "exhausting the step-down budget IS a statement about the gateway"
    );
    assert!(
        matches!(
            StreamStop::SteppedRungFailed { at: 1 }.absent_kind(),
            Absent::NotMeasured
        ),
        "a rung that failed the gate is the gateway's result"
    );

    // And the rig case must SAY it was the rig, in words a reader of the board will see.
    let rig = StreamStop::RigRanShort {
        measured: 1,
        wanted: 3,
    }
    .describe(proved, budget);
    assert!(
        rig.contains("RIG ran short") && rig.contains("not the gateway"),
        "the rig's own shortfall must name itself rather than reading as the gateway's failure: {rig}"
    );
}
