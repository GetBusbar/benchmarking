use super::*;

/* THE WHOLE POINT IS THAT THIS ONE IS THEIRS. A gateway that stops serving after an overload and
does not come back is a finding about the gateway, so filing it under HarnessError would be
the attribution error running in reverse - hiding a real gateway limitation behind our own
fault, which is just as dishonest as the inverse and rather more flattering. */
#[test]
fn a_gateway_that_never_recovers_is_the_gateways_result() {
    assert_eq!(
        StreamStop::GatewayDidNotRecover {
            at: 4096,
            proven: 8192,
            restart_cleared: true
        }
        .absent_kind(),
        Absent::NotMeasured,
        "a gateway that will not serve again is not a rig fault"
    );
    // And the rig-side ones must stay rig-side, or this variant has laundered them.
    assert_eq!(
        StreamStop::RigContaminated {
            at: 3088,
            proven: 4096
        }
        .absent_kind(),
        Absent::HarnessError
    );
}

/// "Wedged until restarted" and "wedged and stayed wedged" are different claims about the
/// gateway, and a reader deciding whether to run it in production needs to know which.
#[test]
fn the_reason_distinguishes_a_restart_that_helped_from_one_that_did_not() {
    let cleared = StreamStop::GatewayDidNotRecover {
        at: 4096,
        proven: 8192,
        restart_cleared: true,
    }
    .describe(8192, 6);
    let stuck = StreamStop::GatewayDidNotRecover {
        at: 4096,
        proven: 8192,
        restart_cleared: false,
    }
    .describe(8192, 6);
    assert!(
        cleared.contains("only after the harness restarted it"),
        "{cleared}"
    );
    assert!(stuck.contains("a restart did not bring it back"), "{stuck}");
    assert_ne!(cleared, stuck, "the two outcomes must not read identically");
    for d in [&cleared, &stuck] {
        assert!(d.contains("c=8192"), "must name what it had carried: {d}");
        assert!(d.contains("c=4096"), "must name the rung that failed: {d}");
    }
}
