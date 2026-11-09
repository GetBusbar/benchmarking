use super::*;

fn obs(status: Option<u16>, mock_healthy: bool) -> Observation {
    Observation {
        status,
        mock_healthy,
        pairing_supported: false,
    }
}

// Identical observations and support declarations use the same rule for every gateway.
#[test]
fn the_same_observation_always_gets_the_same_verdict() {
    let a = persistent_transient_verdict(obs(Some(503), true));
    let b = persistent_transient_verdict(obs(Some(503), true));
    assert_eq!(a, b);
    assert_eq!(a, Verdict::Failed);
}

// Every cell gets the same effort: a budget that varied by cell would let a gateway's own claim
// buy it more attempts at a longer pause than an undeclared cell gets.
#[test]
fn the_budget_is_the_same_for_every_cell() {
    let (r1, p1) = transient_budget();
    let (r2, p2) = transient_budget();
    assert_eq!((r1, p1), (r2, p2));
    assert!(r1 > 0 && p1 > 0);
}

// A real answer plus a healthy rig is evidence about the GATEWAY, and is published as such -
// but a 404/501 (route not implemented at all) is a different claim from every other status
// (the route exists and this attempt was declined), and they must not collapse into one label.
#[test]
fn undeclared_not_found_and_not_implemented_retain_the_routing_observation() {
    for status in [404u16, 501] {
        assert_eq!(
            persistent_transient_verdict(obs(Some(status), true)),
            Verdict::NotConfigured,
            "an undeclared status {status} retains the routing observation"
        );
    }
}

#[test]
fn a_supported_pairing_rejecting_the_request_is_failed_even_for_404_or_501() {
    for status in [404, 501] {
        let observation = Observation {
            status: Some(status),
            mock_healthy: true,
            pairing_supported: true,
        };
        assert_eq!(persistent_transient_verdict(observation), Verdict::Failed);
        assert_eq!(
            persistent_transient_verdict(Observation {
                mock_healthy: false,
                ..observation
            }),
            Verdict::NotVerified,
        );
        assert!(
            !status_is_transient(status),
            "support must not buy extra retries"
        );
    }
}

#[test]
fn every_other_real_status_means_the_pairing_is_real_but_this_attempt_failed() {
    for status in [400u16, 401, 403, 405, 422, 429, 500, 502, 503, 504] {
        assert_eq!(
            persistent_transient_verdict(obs(Some(status), true)),
            Verdict::Failed,
            "status {status} with a healthy rig means the gateway reached and declined the \
                 request, not that the pairing is absent"
        );
    }
}

// No HTTP answer at all cannot be attributed to the gateway: it may never have been reached.
#[test]
fn no_http_answer_is_never_blamed_on_the_gateway() {
    assert_eq!(
        persistent_transient_verdict(obs(None, true)),
        Verdict::NotVerified
    );
    assert_eq!(
        persistent_transient_verdict(obs(None, false)),
        Verdict::NotVerified
    );
}

// If the rig could not confirm itself, nothing observed through it is attributable, whatever
// status came back. The rig's own health gates the reading, not the gateway's behaviour.
#[test]
fn an_unconfirmed_rig_makes_every_status_unattributable() {
    for status in [200u16, 404, 500, 503] {
        assert_eq!(
            persistent_transient_verdict(obs(Some(status), false)),
            Verdict::NotVerified,
            "status {status} with an unhealthy rig says nothing about the gateway"
        );
    }
}

#[test]
fn tokens_are_the_published_vocabulary() {
    assert_eq!(Verdict::NotConfigured.token(), "not_configured");
    assert_eq!(Verdict::Failed.token(), "failed");
    assert_eq!(Verdict::NotVerified.token(), "not_verified");
}

// WHICH STATUSES ARE WORTH THE BUDGET.
//
// The verdict claims the failure "persisted across the whole budget". That claim is only true
// for statuses a retry could plausibly change, and false for the ones the gateway answered on
// the merits. 503 is the case that cost a whole board: busbar answered it on 26 of 36 cells in
// the 2026-07-28 field run, every one was published as a red, and the same gateway had served
// all 36 the day before.
#[test]
fn a_status_that_says_temporarily_is_retried_and_a_settled_answer_is_not() {
    // "Temporarily unavailable", "too many requests", "timed out" - all moments, not capabilities.
    for s in [408, 425, 429, 500, 502, 503, 504, 507, 509] {
        assert!(
            status_is_transient(s),
            "HTTP {s} describes a moment and must not be graded as a capability"
        );
    }
    // A route that does not exist is an ANSWER. NotConfigured already carries it, and spending a
    // 60-second budget to hear it twice more just burns box-time.
    for s in [404, 501] {
        assert!(
            !status_is_transient(s),
            "HTTP {s} is the gateway stating the route does not exist"
        );
    }
    // Neither is a request the gateway rejected on its merits: the same bytes get the same answer.
    for s in [400, 401, 403, 405, 409, 413, 415, 422] {
        assert!(
            !status_is_transient(s),
            "HTTP {s} is deterministic - a retry sends the same request"
        );
    }
    // And success is never a retry case.
    for s in [200, 201, 204] {
        assert!(!status_is_transient(s));
    }
}

// The budget is what makes the retry fair AND bounded: the same attempts for every cell, and a
// pause long enough that a gateway shedding load from the previous cell has actually recovered
// rather than being asked again inside the same backpressure.
#[test]
fn the_budget_funds_more_than_one_attempt_and_pauses_between_them() {
    let (attempts, pause_s) = transient_budget();
    assert!(
        attempts > 1,
        "a budget of {attempts} attempt(s) cannot outlast anything transient"
    );
    assert!(
        pause_s > 0,
        "retrying with no pause re-asks inside the same moment that failed"
    );
}
