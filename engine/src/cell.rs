// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// One cell of the protocol matrix, and the loop over them.
//
// THE STATE-LEAK SURFACE THIS REMOVES. The shell accumulates each cell's result into file-scope
// globals (CELL_PERF_JSON, CELL_STREAM_JSON, CELL_MEM_JSON, CELL_PROBE_NOTE), assigns them from a
// dozen scattered branches, and relies on emit_cell clearing all four at the end of every
// invocation. Miss one clear on one path and cell N+1 silently inherits cell N's numbers, which is
// unfalsifiable from the published artifact because both values are plausible. The shell knows this
// and clears them defensively; the discipline is real but it is a discipline.
//
// Here a cell OWNS its outcome. There is no shared mutable state to forget to clear, so inheritance
// is not a bug that has to be prevented, it is a thing that cannot be written.

use crate::probe::Verdict;
use serde::{Deserialize, Serialize};

/// An ingress-to-egress pairing: the client speaks `ingress`, the upstream speaks `egress`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CellId {
    pub ingress: String,
    pub egress: String,
}

impl CellId {
    pub fn new(ingress: impl Into<String>, egress: impl Into<String>) -> Self {
        Self { ingress: ingress.into(), egress: egress.into() }
    }
    /// True when both sides speak the same dialect: strict passthrough, no translation.
    pub fn is_diagonal(&self) -> bool {
        self.ingress == self.egress
    }
}

impl std::fmt::Display for CellId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}>{}", self.ingress, self.egress)
    }
}

/// What the probe established about this pairing. Measured, never taken from a declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Served {
    /// The gateway served the pairing and it is eligible for measurement.
    Yes,
    /// The gateway answered, deterministically, that it does not serve this pairing.
    ///
    /// CARRIES THE EVIDENCE: a bare verdict would say "this gateway does not serve this cell" and
    /// nothing else, making a field run in which every gateway answered 4xx for a rig-side reason
    /// indistinguishable, in the artifact, from gateways that genuinely support nothing, with the
    /// reason unrecoverable once the box that produced it is gone.
    No(Verdict, Evidence),
    /// The rig could not pose the question, so nothing about the gateway was learned.
    Untestable(String),
    /// The pairing is outside the gateway's OWN declared capability matrix, so it was never probed
    /// at all. Distinct from `No`: this is not the gateway's answer about THIS pairing (no request
    /// was ever sent for it), it is the manifest's prior, cited declaration that this pairing does
    /// not exist for it - a status returned for an undeclared pairing would be a global gate (auth,
    /// rate limit) firing before routing, not evidence about the pairing. See `Manifest::matrix`.
    NotConfigurable(String),
}

/// What the gateway actually said when it declined. Small on purpose: a status and the first of the
/// body, which together separate "it rejected the model", "it rejected the auth" and "it could not
/// reach the upstream" without carrying a whole response into every cell of a 36 cell grid.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Evidence {
    pub status: u16,
    pub body_snippet: String,
}

impl Evidence {
    /// The first `MAX` bytes of a body, on a char boundary so the snippet is always printable.
    pub fn snippet(body: &str) -> String {
        const MAX: usize = 200;
        if body.len() <= MAX {
            return body.trim().to_string();
        }
        let mut end = MAX;
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        body[..end].trim().to_string()
    }
}

impl Served {
    pub fn is_measurable(&self) -> bool {
        matches!(self, Served::Yes)
    }
}

/// Why a cell carries no measurements. Kept distinct from Served so the artifact can say "the
/// gateway serves this, and we still did not measure it, and here is why".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Skipped {
    /// The suite wall clock ran out before this cell was reached.
    SuiteDeadline,
    /// The cell is not served, so there is nothing to measure.
    NotServed,
}

/// One cell's complete, OWNED result. Nothing here is shared with any other cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellOutcome {
    pub id: CellId,
    pub served: Served,
    /// Present only when the cell was measurable AND the suite had time for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<Skipped>,
    /// Free-text evidence for a reader: the probe's own words about what it saw.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CellOutcome {
    pub fn served(id: CellId) -> Self {
        Self { id, served: Served::Yes, skipped: None, note: None }
    }

    pub fn not_served(id: CellId, verdict: Verdict, evidence: Evidence, note: impl Into<String>) -> Self {
        Self { id, served: Served::No(verdict, evidence), skipped: Some(Skipped::NotServed), note: Some(note.into()) }
    }

    /// This pairing is outside the gateway's own declared capability matrix, so it was never probed.
    pub fn not_configurable(id: CellId, reason: impl Into<String>) -> Self {
        let r = reason.into();
        Self { id, served: Served::NotConfigurable(r.clone()), skipped: Some(Skipped::NotServed), note: Some(r) }
    }

    pub fn untestable(id: CellId, reason: impl Into<String>) -> Self {
        let r = reason.into();
        Self { id, served: Served::Untestable(r.clone()), skipped: Some(Skipped::NotServed), note: Some(r) }
    }

    /// Reached after the suite clock expired. The cell is recorded as PRESENT and unmeasured, never
    /// dropped: a missing row hides a failure, and a row that says "we ran out of time" is a fact
    /// about the run rather than a claim about the gateway.
    pub fn out_of_time(id: CellId) -> Self {
        Self {
            id,
            served: Served::Untestable("the suite wall clock expired before this cell was measured".into()),
            skipped: Some(Skipped::SuiteDeadline),
            note: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diagonal_is_passthrough_and_an_off_diagonal_is_translation() {
        assert!(CellId::new("openai", "openai").is_diagonal());
        assert!(!CellId::new("openai", "anthropic").is_diagonal());
        assert_eq!(CellId::new("openai", "anthropic").to_string(), "openai>anthropic");
    }

    // `run.rs`'s real grid walk builds `CellOutcome` through exactly these three constructors
    // (`served`/`not_served`/`untestable`); pinned here directly so their shape stays covered
    // without going through a second, parallel grid-walking loop.
    #[test]
    fn cell_outcome_constructors_carry_the_right_served_and_skipped_shape() {
        let id = CellId::new("openai", "anthropic");

        let served = CellOutcome::served(id.clone());
        assert!(served.served.is_measurable());
        assert_eq!(served.skipped, None);

        let ev = Evidence { status: 404, body_snippet: "not found".into() };
        let ns = CellOutcome::not_served(id.clone(), Verdict::NotConfigured, ev.clone(), "declined");
        assert_eq!(ns.served, Served::No(Verdict::NotConfigured, ev));
        assert_eq!(ns.skipped, Some(Skipped::NotServed));
        assert_eq!(ns.note.as_deref(), Some("declined"));

        let ut = CellOutcome::untestable(id.clone(), "rig cannot pose this pairing");
        assert!(matches!(ut.served, Served::Untestable(_)));
        assert!(!ut.served.is_measurable());

        let oot = CellOutcome::out_of_time(id);
        assert_eq!(oot.skipped, Some(Skipped::SuiteDeadline));
        assert!(!oot.served.is_measurable(), "a cell never reached cannot read as served");
    }
}
