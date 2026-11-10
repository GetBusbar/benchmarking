// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Classifying what a cell probe OBSERVED, and deciding how hard to try.
//
// FAIRNESS. No gateway identity enters this rule. A researched capability declaration distinguishes
// an absent pairing from a supported pairing that failed; HTTP 501 can also mean a rejected
// parameter on an implemented route. `transient_budget()` takes no arguments, so every gateway
// gets this same rule and retry budget: a declaration never buys a cell extra attempts.

use serde::{Deserialize, Serialize};

/// The observed response and the manifest's researched expectation, without gateway identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// The final HTTP status. `None` models curl's 000: no HTTP answer at all, so the gateway may
    /// never have been reached.
    pub status: Option<u16>,
    /// Whether the recording mock confirmed it was healthy for this probe. When it did not, an
    /// upstream really may have been missing, and the observation cannot be attributed.
    pub mock_healthy: bool,
    /// The manifest explicitly declares this pairing supported. A rejection must not erase that
    /// capability and turn a real failure into an unsupported grey.
    pub pairing_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// An undeclared pairing returned 404/501. This is a routing observation, not independent
    /// proof of absent product support. Researched supported pairings never take this verdict.
    NotConfigured,
    /// The gateway rejected a supported request (including a 404/501 on a pairing the manifest
    /// declares supported), or returned another application rejection: an auth rejection, a
    /// malformed-request response, a server error. This specific attempt failed. Published as a genuine defect,
    /// not folded into the same grey "not configured" a truly absent route gets - collapsing the
    /// two let a gateway that supports a pairing but rejects the probe (wrong auth shape, a bug on
    /// its own error path) read identically to one that never built the route at all.
    Failed,
    /// The harness could not get a fair reading. A statement about the RIG, not the gateway.
    NotVerified,
}

impl Verdict {
    pub fn token(&self) -> &'static str {
        match self {
            Verdict::NotConfigured => "not_configured",
            Verdict::Failed => "failed",
            Verdict::NotVerified => "not_verified",
        }
    }
}

/// Retry budget for a persistent-transient probe. Takes no arguments ON PURPOSE: every cell gets
/// the same attempts and the same pause, so no cell can be tried harder than another.
pub const fn transient_budget() -> (u32, u32) {
    (TRANSIENT_RETRIES, TRANSIENT_PAUSE_S)
}

const TRANSIENT_RETRIES: u32 = 3; // total attempts on ANY cell
const TRANSIENT_PAUSE_S: u32 = 30; // seconds between them, on ANY cell

/// Is this status one a RETRY could plausibly change?
///
/// The verdict below is documented as "the failure persisted across the whole budget", and
/// `transient_budget()` exists to fund exactly that. This is the predicate that decides which
/// statuses are worth spending it on. A 503 says "temporarily unavailable" in words; grading one on
/// a single observation records a moment as a capability.
///
/// It matters because the harness MAKES this condition. Cells run back to back with no settle, and
/// the last metric before the next cell's probe is a heavy load, so a gateway with admission control
/// can still be shedding when we ask it whether it supports the next pairing. In the 2026-07-28
/// field run busbar answered 503 on 26 of 36 cells and was recorded as not serving them; the same
/// gateway had served all 36 the day before. Every egress lane still answered under openai ingress,
/// which is what proves the lanes were fine and the moment was not.
///
/// 404 and 501 are excluded deliberately: they are deterministic rejections, whether the pairing
/// is declared supported or not, and they do not gain retries from that declaration. 4xx that mean "your request was wrong" are equally deterministic - retrying an argument
/// the gateway already rejected on its merits just spends box-time to hear it again.
pub const fn status_is_transient(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504 | 507 | 509)
}

/// The verdict for "probed it, the failure persisted across the whole budget".
///
/// A real HTTP status means the gateway accepted the connection, routed the request and produced a
/// response of its own; a verifiably-recording mock means the rig did not go away underneath it.
/// Reproduced across the budget, that is a deterministic application-level rejection: an
/// observation, and the most informative one available. Calling it `not_verified` would throw away
/// the status evidence and assert something false about our own ability to read the cell.
///
/// A researched supported pairing that rejects the common request is Failed, including 404/501:
/// those statuses cannot erase declared support. For undeclared pairings 404 and 501 keep the
/// NotConfigured routing observation (the HTTP-standard "route/method not implemented"). Every other real status (400, 401, 403, 422, 429, 500, 502,
/// 503, ...) means the gateway recognised the request enough to evaluate and decline it for some
/// OTHER reason - the pairing is real, and this attempt is what failed.
pub fn persistent_transient_verdict(obs: Observation) -> Verdict {
    match (obs.status, obs.mock_healthy) {
        // No HTTP answer at all: the gateway may never have been reached.
        (None, _) => Verdict::NotVerified,
        // The rig could not confirm itself, so nothing observed here is attributable.
        (_, false) => Verdict::NotVerified,
        (Some(404 | 501), true) if !obs.pairing_supported => Verdict::NotConfigured,
        (Some(_), true) => Verdict::Failed,
    }
}

#[cfg(test)]
mod tests {
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
}
