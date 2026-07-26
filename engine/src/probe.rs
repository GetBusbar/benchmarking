// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Classifying what a cell probe OBSERVED, and deciding how hard to try.
//
// FAIRNESS. `Observation` carries only what the rig saw, no cell identity and no capability claim,
// and `transient_budget()` takes no arguments at all, so neither a gateway's declaration nor a
// cell's identity can reach the verdict or the measurement effort spent earning it, even by
// accident: two cells that differ only in what the gateway claims about them must classify
// identically and get the same number of attempts.

use serde::{Deserialize, Serialize};

/// Exactly what the rig observed. There is deliberately no cell identity and no capability claim
/// here: a field that does not exist cannot be consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// The final HTTP status. `None` models curl's 000: no HTTP answer at all, so the gateway may
    /// never have been reached.
    pub status: Option<u16>,
    /// Whether the recording mock confirmed it was healthy for this probe. When it did not, an
    /// upstream really may have been missing, and the observation cannot be attributed.
    pub mock_healthy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The gateway answered, deterministically, that this pairing does not exist at all (a
    /// 404/501-shaped response). Grey on the board, never a red: no gateway is failed for a
    /// pairing it never claimed to serve.
    NotConfigured,
    /// The gateway answered, deterministically, but declined the request for a reason OTHER than
    /// "this route does not exist": an auth rejection, a malformed-request response, a server
    /// error. The pairing is real; this specific attempt failed. Published as a genuine defect,
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

/// The verdict for "probed it, the failure persisted across the whole budget".
///
/// A real HTTP status means the gateway accepted the connection, routed the request and produced a
/// response of its own; a verifiably-recording mock means the rig did not go away underneath it.
/// Reproduced across the budget, that is a deterministic application-level rejection: an
/// observation, and the most informative one available. Calling it `not_verified` would throw away
/// the status evidence and assert something false about our own ability to read the cell.
///
/// NotConfigured vs Failed is decided from the status ALONE, never from what a gateway declares it
/// supports (that would violate the fairness rule this module's header states): 404 and 501 are the
/// HTTP-standard ways a server says "this route/method is not implemented at all", so those two
/// mean the pairing itself is absent. Every other real status (400, 401, 403, 422, 429, 500, 502,
/// 503, ...) means the gateway recognised the request enough to evaluate and decline it for some
/// OTHER reason - the pairing is real, and this attempt is what failed.
pub fn persistent_transient_verdict(obs: Observation) -> Verdict {
    match (obs.status, obs.mock_healthy) {
        // No HTTP answer at all: the gateway may never have been reached.
        (None, _) => Verdict::NotVerified,
        // The rig could not confirm itself, so nothing observed here is attributable.
        (_, false) => Verdict::NotVerified,
        (Some(404), true) | (Some(501), true) => Verdict::NotConfigured,
        (Some(_), true) => Verdict::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(status: Option<u16>, mock_healthy: bool) -> Observation {
        Observation { status, mock_healthy }
    }

    // The verdict must depend only on what was observed, so two cells that differ ONLY in what the
    // gateway claimed about them must classify identically. There is no capability argument to
    // pass: the API makes that unrepresentable, not merely unlikely.
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
    fn not_found_and_not_implemented_mean_the_pairing_is_absent() {
        for status in [404u16, 501] {
            assert_eq!(
                persistent_transient_verdict(obs(Some(status), true)),
                Verdict::NotConfigured,
                "status {status} with a healthy rig means the pairing does not exist"
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
        assert_eq!(persistent_transient_verdict(obs(None, true)), Verdict::NotVerified);
        assert_eq!(persistent_transient_verdict(obs(None, false)), Verdict::NotVerified);
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
}
