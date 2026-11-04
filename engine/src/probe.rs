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
/// 404 and 501 are excluded deliberately: those are the gateway stating the route does not exist,
/// which is an answer, not a hiccup, and `NotConfigured` already carries it without spending a
/// budget. 4xx that mean "your request was wrong" are equally deterministic - retrying an argument
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
#[path = "tests/probe_tests.rs"]
mod tests;
