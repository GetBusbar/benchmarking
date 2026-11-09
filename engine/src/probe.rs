// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Classifying what a cell probe OBSERVED, and deciding how hard to try.
//
// FAIRNESS. No gateway identity enters this rule. A researched capability declaration distinguishes
// an absent pairing from a supported pairing that failed; HTTP 501 can also mean a rejected
// parameter on an implemented route. Every gateway gets this same rule and retry budget.

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
    /// The gateway rejected a supported request, or returned another application rejection:
    /// an auth rejection, a malformed-request response, a server error. This records a failed
    /// attempt, not independent proof that the pairing exists or that the
    /// product is defective: configuration errors and unsupported-operation responses may use
    /// these statuses too. Retain the response evidence so attribution can be investigated.
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
/// 404 and 501 are deterministic rejections, whether the pairing is declared supported or not.
/// They do not gain retries from that declaration. Other 4xx are equally deterministic - retrying an argument
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
/// A researched supported pairing that rejects the common request is Failed, including 404/501.
/// For undeclared pairings those two statuses retain the legacy NotConfigured routing observation.
/// Every other real status records a rejected attempt. Configuration must be audited separately:
/// status alone cannot distinguish a product defect from a harness configuration error.
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
#[path = "tests/probe_tests.rs"]
mod tests;
