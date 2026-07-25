// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Classifying what a cell probe OBSERVED, and deciding how hard to try.
//
// THE FAIRNESS DEFECT THIS EXISTS FOR. Both of these once depended on the gateway's own advisory
// capability declaration, so the identical observation (a real persistent 5xx with the recording
// mock verifiably healthy) was published as `not_configured` on an undeclared cell and as
// `not_verified` on a declared one, and the declared cell was also tried harder: 3 attempts at 120s
// against 2 at 10s. An unverified claim moved both the verdict and the measurement effort spent
// earning it. That is the harness failing to treat equals equally, and it is invisible in the
// published JSON because both verdicts are legitimate classes.
//
// The type system now enforces what the shell enforced by discipline: `Observation` carries only
// what the rig saw, and `transient_budget()` takes no arguments at all, so neither the declaration
// nor the cell's identity can reach either decision even by accident.

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
    /// The gateway answered, deterministically, that this pairing does not light up. Grey on the
    /// board, never a red: no gateway is failed for a cell it does not serve.
    NotConfigured,
    /// The harness could not get a fair reading. A statement about the RIG, not the gateway.
    NotVerified,
}

impl Verdict {
    pub fn token(&self) -> &'static str {
        match self {
            Verdict::NotConfigured => "not_configured",
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
pub fn persistent_transient_verdict(obs: Observation) -> Verdict {
    match (obs.status, obs.mock_healthy) {
        // No HTTP answer at all: the gateway may never have been reached.
        (None, _) => Verdict::NotVerified,
        // The rig could not confirm itself, so nothing observed here is attributable.
        (_, false) => Verdict::NotVerified,
        (Some(_), true) => Verdict::NotConfigured,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(status: Option<u16>, mock_healthy: bool) -> Observation {
        Observation { status, mock_healthy }
    }

    // THE DEFECT, as a test. The verdict must depend only on what was observed, so two cells that
    // differ ONLY in what the gateway claimed about them must classify identically. There is no
    // capability argument to pass, which is the point: this test documents that the API makes the
    // old bug unrepresentable rather than merely unlikely.
    #[test]
    fn the_same_observation_always_gets_the_same_verdict() {
        let a = persistent_transient_verdict(obs(Some(503), true));
        let b = persistent_transient_verdict(obs(Some(503), true));
        assert_eq!(a, b);
        assert_eq!(a, Verdict::NotConfigured);
    }

    // Every cell gets the same effort. A budget that varied by cell is how a gateway's own claim
    // once bought itself 3 attempts at 120s where an undeclared cell got 2 at 10s.
    #[test]
    fn the_budget_is_the_same_for_every_cell() {
        let (r1, p1) = transient_budget();
        let (r2, p2) = transient_budget();
        assert_eq!((r1, p1), (r2, p2));
        assert!(r1 > 0 && p1 > 0);
    }

    // A real answer plus a healthy rig is evidence about the GATEWAY, and is published as such.
    #[test]
    fn a_real_status_with_a_healthy_rig_is_a_gateway_observation() {
        for status in [400u16, 404, 422, 500, 503] {
            assert_eq!(
                persistent_transient_verdict(obs(Some(status), true)),
                Verdict::NotConfigured,
                "status {status} with a healthy rig is an observation about the gateway"
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
        assert_eq!(Verdict::NotVerified.token(), "not_verified");
    }
}
