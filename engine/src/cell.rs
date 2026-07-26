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

use crate::measurement::{Absent, Measurement};
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
    /// CARRIES THE EVIDENCE. A bare verdict said "this gateway does not serve this cell" and
    /// nothing else, so a field run in which every gateway answered 4xx for a rig-side reason was
    /// indistinguishable, in the artifact, from thirteen gateways that genuinely support nothing.
    /// That happened: three gateways published 36 not_configured cells each with no status, no body
    /// and no note, and the reason was unrecoverable once the boxes were gone.
    No(Verdict, Evidence),
    /// The rig could not pose the question, so nothing about the gateway was learned.
    Untestable(String),
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

/// The suite's wall-clock budget. A wedge backstop, not a schedule: hitting it means something is
/// wrong, and every cell after it is recorded as unmeasured rather than omitted.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    remaining_cells_budget: Option<u64>,
}

impl Deadline {
    pub fn unlimited() -> Self {
        Self { remaining_cells_budget: None }
    }
    /// A budget expressed in cells rather than seconds, so the loop is testable without a clock.
    pub fn cells(n: u64) -> Self {
        Self { remaining_cells_budget: Some(n) }
    }
    fn expired(&self) -> bool {
        matches!(self.remaining_cells_budget, Some(0))
    }
    fn spend(&mut self) {
        if let Some(n) = self.remaining_cells_budget.as_mut() {
            *n = n.saturating_sub(1);
        }
    }
}

/// Probing one cell. Implemented by the real rig, and by fixtures in tests.
pub trait CellProbe {
    fn probe(&mut self, id: &CellId) -> Served;
}

/// Walk every cell of the grid, in a deterministic order, and return one owned outcome per cell.
///
/// EVERY CELL ALWAYS APPEARS. Not served, untestable and out-of-time are all recorded rather than
/// omitted, because a dropped row hides a failure and a reader cannot tell an absent row from a
/// pairing nobody thought to try.
pub fn walk_grid<P: CellProbe>(
    ingress: &[String],
    egress: &[String],
    probe: &mut P,
    mut deadline: Deadline,
) -> Vec<CellOutcome> {
    let mut out = Vec::with_capacity(ingress.len() * egress.len());
    for eg in egress {
        for ing in ingress {
            let id = CellId::new(ing.clone(), eg.clone());
            if deadline.expired() {
                out.push(CellOutcome::out_of_time(id));
                continue;
            }
            deadline.spend();
            // The outcome is CONSTRUCTED here and moved into the vector. There is no accumulator to
            // clear, so nothing from the previous cell can survive into this one.
            let served = probe.probe(&id);
            out.push(match served {
                Served::Yes => CellOutcome::served(id),
                Served::No(v, _) => {
                    let note = format!("probed and answered {}", v.token());
                    CellOutcome::not_served(id, v, Evidence::default(), note)
                }
                Served::Untestable(r) => CellOutcome::untestable(id, r),
            });
        }
    }
    out
}

/// The count a reader needs to judge coverage: how many cells were served, and of those how many
/// actually carry measurements.
pub fn coverage(cells: &[CellOutcome]) -> Measurement<(usize, usize)> {
    if cells.is_empty() {
        return Measurement::absent_because(Absent::NotMeasured, "no cells were walked");
    }
    let served = cells.iter().filter(|c| c.served.is_measurable()).count();
    let measured = cells.iter().filter(|c| c.served.is_measurable() && c.skipped.is_none()).count();
    Measurement::Measured((served, measured))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialects() -> Vec<String> {
        ["openai", "anthropic", "gemini"].iter().map(|s| s.to_string()).collect()
    }

    /// A probe that would leak if the loop had an accumulator: it returns a DIFFERENT answer per
    /// cell, so any inheritance shows up as a wrong outcome rather than a coincidence.
    struct PerCell;
    impl CellProbe for PerCell {
        fn probe(&mut self, id: &CellId) -> Served {
            if id.is_diagonal() {
                Served::Yes
            } else if id.ingress == "gemini" {
                Served::Untestable("the rig cannot pose this pairing".into())
            } else {
                Served::No(Verdict::NotConfigured, Evidence::default())
            }
        }
    }

    // THE POINT OF THE MODULE. Each cell's outcome is its own value, so cell N+1 cannot inherit
    // cell N's. In shell these lived in file-scope globals cleared defensively at the end of every
    // emit; one missed clear on one branch and the next cell publishes the previous cell's numbers,
    // which is unfalsifiable from the artifact because both values look plausible.
    #[test]
    fn no_cell_inherits_the_previous_cells_outcome() {
        let d = dialects();
        let cells = walk_grid(&d, &d, &mut PerCell, Deadline::unlimited());
        for c in &cells {
            let expected_measurable = c.id.is_diagonal();
            assert_eq!(
                c.served.is_measurable(),
                expected_measurable,
                "{} carries the wrong outcome, which is what inheritance would look like",
                c.id
            );
        }
    }

    // Every gateway always appears, and so does every cell. A dropped row hides a failure.
    #[test]
    fn every_cell_of_the_grid_is_present_exactly_once() {
        let d = dialects();
        let cells = walk_grid(&d, &d, &mut PerCell, Deadline::unlimited());
        assert_eq!(cells.len(), d.len() * d.len());
        let mut ids: Vec<String> = cells.iter().map(|c| c.id.to_string()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), d.len() * d.len(), "every pairing appears, and none appears twice");
    }

    // Running out of time is a fact about the RUN. The cells still appear, marked unmeasured, rather
    // than vanishing and leaving a reader unable to tell them from pairings nobody tried.
    #[test]
    fn cells_past_the_deadline_are_recorded_not_dropped() {
        let d = dialects();
        let cells = walk_grid(&d, &d, &mut PerCell, Deadline::cells(4));
        assert_eq!(cells.len(), d.len() * d.len(), "the grid is complete even when the clock is not");
        let timed_out: Vec<_> = cells.iter().filter(|c| c.skipped == Some(Skipped::SuiteDeadline)).collect();
        assert_eq!(timed_out.len(), d.len() * d.len() - 4);
        for c in timed_out {
            assert!(!c.served.is_measurable(), "a cell we never reached cannot be reported as served");
        }
    }

    // An untestable cell is a statement about the RIG. It must never read as the gateway refusing.
    #[test]
    fn untestable_is_not_the_same_as_not_served() {
        let d = dialects();
        let cells = walk_grid(&d, &d, &mut PerCell, Deadline::unlimited());
        let untestable = cells.iter().find(|c| matches!(c.served, Served::Untestable(_))).expect("fixture has one");
        assert!(!matches!(untestable.served, Served::No(..)));
        assert!(untestable.note.is_some(), "an untestable cell must say why");
    }

    #[test]
    fn a_not_served_cell_carries_the_verdict_that_produced_it() {
        let d = dialects();
        let cells = walk_grid(&d, &d, &mut PerCell, Deadline::unlimited());
        let ns = cells.iter().find(|c| matches!(c.served, Served::No(..))).expect("fixture has one");
        assert_eq!(ns.served, Served::No(Verdict::NotConfigured, Evidence::default()));
        assert!(ns.note.as_deref().unwrap_or_default().contains("not_configured"));
    }

    #[test]
    fn the_walk_order_is_deterministic() {
        let d = dialects();
        let a = walk_grid(&d, &d, &mut PerCell, Deadline::unlimited());
        let b = walk_grid(&d, &d, &mut PerCell, Deadline::unlimited());
        assert_eq!(a, b, "two walks of the same grid must produce the same rows in the same order");
    }

    #[test]
    fn coverage_counts_served_and_measured_separately() {
        let d = dialects();
        let cells = walk_grid(&d, &d, &mut PerCell, Deadline::unlimited());
        let (served, measured) = coverage(&cells).copied().expect("cells were walked");
        assert_eq!(served, 3, "the three diagonals are served");
        assert_eq!(measured, 3);
    }

    #[test]
    fn coverage_of_nothing_is_absent_not_zero() {
        let c = coverage(&[]);
        assert_eq!(c.copied(), None);
        assert_eq!(c.reason(), Some(&Absent::NotMeasured));
    }

    #[test]
    fn a_diagonal_is_passthrough_and_an_off_diagonal_is_translation() {
        assert!(CellId::new("openai", "openai").is_diagonal());
        assert!(!CellId::new("openai", "anthropic").is_diagonal());
        assert_eq!(CellId::new("openai", "anthropic").to_string(), "openai>anthropic");
    }
}
