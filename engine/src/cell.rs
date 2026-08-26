// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// One cell of the protocol matrix, and the loop over them.
//
// Unlike the shell version this ported from, which accumulated each cell's result into file-scope
// globals that had to be cleared defensively between invocations, a cell here owns its outcome: there
// is no shared mutable state for a later cell to silently inherit.

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
        Self {
            ingress: ingress.into(),
            egress: egress.into(),
        }
    }
    // No `is_diagonal`: `reverify::reverify_cell` compares the dialect that actually built the request
    // against the parsed egress instead, which can't disagree with the bytes on the wire the way a
    // helper on these raw strings could.
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
    /// Carries the evidence (status + body) so a rig-side 4xx isn't indistinguishable, in the
    /// artifact, from a gateway that genuinely supports nothing.
    No(Verdict, Evidence),
    /// The rig could not pose the question, so nothing about the gateway was learned.
    Untestable(String),
    /// The pairing is outside the gateway's own declared capability matrix, so it was never probed at
    /// all - distinct from `No`, which is the gateway's actual answer about a request that was sent.
    /// See `Manifest::matrix`.
    NotConfigurable(String),
    /// The rig could not authenticate (it sends a bearer token, not a signed request e.g. AWS SigV4),
    /// so a 401/403 here is the gateway behaving correctly, not evidence against it. Distinct from
    /// `Untestable`: the question was posed and answered, just not about whether the pairing works.
    /// Carries evidence for the same reason `No` does.
    UnprobedAuth(Evidence),
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
    // No `SuiteDeadline`: it was never constructible (nothing tracked elapsed suite time), so it was
    // removed rather than kept as a variant the run could never actually emit. An interrupted run is
    // instead protected by cells streaming to disk as they finish (`run::run_grid_streaming`).
    /// The cell is not served, so there is nothing to measure.
    NotServed,
}

/// One cell's complete, OWNED result. Nothing here is shared with any other cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellOutcome {
    pub id: CellId,
    pub served: Served,
    /// Why this cell carries NO measurements - present exactly when it was not measured, and absent
    /// when it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<Skipped>,
    /// Free-text evidence for a reader: the probe's own words about what it saw.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl CellOutcome {
    pub fn served(id: CellId) -> Self {
        Self {
            id,
            served: Served::Yes,
            skipped: None,
            note: None,
        }
    }

    pub fn not_served(
        id: CellId,
        verdict: Verdict,
        evidence: Evidence,
        note: impl Into<String>,
    ) -> Self {
        Self {
            id,
            served: Served::No(verdict, evidence),
            skipped: Some(Skipped::NotServed),
            note: Some(note.into()),
        }
    }

    /// This pairing is outside the gateway's own declared capability matrix, so it was never probed.
    pub fn not_configurable(id: CellId, reason: impl Into<String>) -> Self {
        let r = reason.into();
        Self {
            id,
            served: Served::NotConfigurable(r.clone()),
            skipped: Some(Skipped::NotServed),
            note: Some(r),
        }
    }

    pub fn untestable(id: CellId, reason: impl Into<String>) -> Self {
        let r = reason.into();
        Self {
            id,
            served: Served::Untestable(r.clone()),
            skipped: Some(Skipped::NotServed),
            note: Some(r),
        }
    }

    /// The gateway refused a credential the rig cannot legitimately produce. Deliberately not
    /// `not_served`: the note must read as our limit, not the gateway's answer.
    pub fn unprobed_auth(id: CellId, evidence: Evidence) -> Self {
        let n = format!(
            "answered HTTP {} to a credential this dialect's real clients would have signed; the \
             harness does not forge signatures, so nothing was learned about this pairing",
            evidence.status
        );
        Self {
            id,
            served: Served::UnprobedAuth(evidence),
            skipped: Some(Skipped::NotServed),
            note: Some(n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cell_id_renders_as_the_pairing_a_reader_greps_for() {
        assert_eq!(
            CellId::new("openai", "anthropic").to_string(),
            "openai>anthropic"
        );
    }

    // Every constructor `run.rs` actually uses (`served`, `not_served`, `untestable`,
    // `not_configurable`, `unprobed_auth`), pinned here so their shape stays covered.
    #[test]
    fn cell_outcome_constructors_carry_the_right_served_and_skipped_shape() {
        let id = CellId::new("openai", "anthropic");

        let served = CellOutcome::served(id.clone());
        assert!(served.served.is_measurable());
        assert_eq!(served.skipped, None);

        let ev = Evidence {
            status: 404,
            body_snippet: "not found".into(),
        };
        let ns =
            CellOutcome::not_served(id.clone(), Verdict::NotConfigured, ev.clone(), "declined");
        assert_eq!(ns.served, Served::No(Verdict::NotConfigured, ev.clone()));
        assert_eq!(ns.skipped, Some(Skipped::NotServed));
        assert_eq!(ns.note.as_deref(), Some("declined"));

        let ut = CellOutcome::untestable(id.clone(), "rig cannot pose this pairing");
        assert!(matches!(ut.served, Served::Untestable(_)));
        assert!(!ut.served.is_measurable());

        // Both are rig- or config-side outcomes, so both must carry a `skipped` reason.
        let nc = CellOutcome::not_configurable(id.clone(), "no base-url override");
        assert!(!nc.served.is_measurable());
        assert!(
            nc.skipped.is_some(),
            "a not-configurable cell must say why it was skipped, not merely that it was"
        );

        let ua = CellOutcome::unprobed_auth(id, ev.clone());
        assert!(!ua.served.is_measurable());
        assert!(
            ua.skipped.is_some(),
            "an unprobed-auth cell must say why it was skipped, not merely that it was"
        );
    }
}
