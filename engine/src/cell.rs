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
        Self {
            ingress: ingress.into(),
            egress: egress.into(),
        }
    }
    // NO `is_diagonal`. It compared this id's two strings and had no production caller: the one place
    // that needs the fact - `reverify::reverify_cell` - compares the DIALECT that actually built the
    // request against the parsed egress, after the parse guard, which is the stronger question. A
    // helper duplicating it on the raw strings could disagree with the bytes that went on the wire,
    // and its unit test read as coverage of diagonal detection in the real pipeline when nothing in
    // that pipeline called it.
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
    /// THE RIG COULD NOT AUTHENTICATE, so the gateway's refusal says nothing about the pairing.
    ///
    /// A real client of this dialect signs its requests (AWS SigV4 for Bedrock); the harness sends a
    /// bearer token and does not forge signatures. A gateway that answers 401/403 to that is behaving
    /// CORRECTLY, and recording it as a failure publishes a red it did not earn - a false claim about
    /// somebody's product, produced entirely by us, which is the worst error this board can make.
    ///
    /// Distinct from `Untestable`, which says the rig could not pose the question at all: here the
    /// question was posed and the answer is real, it just answers "you are not authenticated" rather
    /// than anything about whether the pairing works. It carries the evidence for the same reason
    /// `No` does - a reader must be able to see the status and body it was decided from.
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
    // NO `SuiteDeadline`. It existed, was serialized, was documented as the mechanism for recording
    // cells lost to a suite wall-clock timeout, and had a unit test - and nothing in the grid walk
    // ever tracked elapsed suite time or built one, so no artifact could contain it. Its own doc
    // promised a reader would see "we ran out of time" for the untouched remainder of an overrunning
    // run; what actually happened was a short artifact with no explanation.
    //
    // Removed rather than tested: dead safety code is worth removing, because a variant nothing can
    // emit makes the enum claim a distinction the run cannot draw. The real protection against losing
    // an interrupted run is that cells now stream to disk as they finish
    // (`run::run_grid_streaming`), which is a guarantee the code actually keeps. If an in-process
    // deadline is ever wanted, build the clock and the constructor in the same change.
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
    ///
    /// The doc here used to read "present only when the cell was measurable AND the suite had time for
    /// it", which is the opposite of what the field means and would lead a reader to treat every
    /// measured cell as skipped and every skipped one as measured. It also referred to a suite
    /// wall-clock that no longer exists: `Skipped::SuiteDeadline` was removed once it turned out
    /// nothing ever tracked elapsed suite time or could emit it.
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

    /// The gateway refused a credential the rig cannot legitimately produce. Not measurable, and
    /// deliberately NOT `not_served`: the note has to read as our limit, because a reader who takes
    /// it for the gateway's answer has been told something false about somebody's product.
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

    // EVERY CONSTRUCTOR `run.rs` REALLY USES, pinned here so their shape stays covered without a
    // second, parallel grid-walking loop.
    //
    // The list is derived from run.rs rather than remembered: it builds `CellOutcome` through
    // `served`, `not_served`, `untestable`, `not_configurable` and `unprobed_auth` - five, not the
    // three this comment used to claim. The two it forgot were also the two this test never touched,
    // so a change that left `skipped` at `None` on either of them would have passed while the comment
    // asserted they were covered. A comment claiming coverage is worth less than nothing when the
    // coverage is not there, because it stops the next reader from checking.
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

        // The two the old comment forgot. Both are rig- or config-side outcomes, so both must carry a
        // `skipped` reason: a cell that reads as unmeasured with nothing saying why is the bare hole
        // this whole type exists to prevent.
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
