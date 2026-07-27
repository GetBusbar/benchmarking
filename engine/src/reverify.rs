// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// DID THE GATEWAY ACTUALLY TRANSLATE, OR DID IT JUST FORWARD OUR BYTES?
//
// This is not a metric. It is an ANTI-FALSE-POSITIVE GUARD, and it exists because of one property of
// the rig: the mock answers all six dialects BY PATH. A gateway that proxies the client's ingress
// path verbatim to the upstream - doing no translation whatsoever - still gets a plausible 200 back
// from the mock's canned response for that path, and `probe_cell` has no way to tell that apart from
// a real translation. The cell would then publish `served: true` for, say, anthropic->openai: a
// translation capability the gateway does not have, asserted by us, about somebody's product. A false
// negative embarrasses the board; a false POSITIVE that flatters a gateway is the error this project
// exists to not make.
//
// THE MOCK ALREADY OWNS THE DETECTION. Under `MOCK_RECORD=1` it records, per dialect, how many
// requests landed on that dialect's endpoint and whether the LAST body passed that dialect's own
// request-shape check (`request_shape_ok`), plus the path and a body snippet as evidence.
// `GET /__mock/state` returns the record and `POST /__mock/reset` clears it. So the whole of this
// module is: clear the recorder, drive exactly one request through the gateway, and read back which
// dialect the mock saw and whether the body was that dialect's shape.
//
// RECORDING IS TURNED ON FOR THIS ONE REQUEST AND OFF AGAIN AFTERWARDS, and that is a
// measurement-integrity requirement rather than tidiness. A recorded request takes a process-wide
// lock in the mock, and the mock's own throughput is the reference every gateway's number is judged
// against (`suite::rig_ceiling` + `rigbound::is_rig_bound`): a slower mock means MORE cells land
// within 10% of its ceiling and have their real, honest throughput suppressed as mock-bound. That is
// a regression that hides inside a correct-looking suppression, which is the worst way for one to
// hide.
//
// So the invariant is: recording is off for every load window, on for exactly one request per cell.
// It applies to the gateway's windows AND to the mock's own reference window, which is what keeps
// both sides of the mock-bound comparison on identical mock behaviour - if the reference were taken
// against a recording mock and the gateway's number against a quiet one, the two would be different
// instruments and the verdict between them would mean nothing.
//
// THREE ANSWERS, NOT TWO. `Some(true)` is proof of translation. `Some(false)` is proof of ITS ABSENCE
// - the mock saw the request and it was not the egress dialect's shape. `None` is "not checked", and
// it is a first-class answer rather than a failure: a diagonal cell has no translation to prove, a
// mock started without MOCK_RECORD cannot answer at all, and a mock we could not reach tells us
// nothing about the gateway. Collapsing any of those three into `false` would convict a gateway on
// our own configuration, which is the same class of error in the opposite direction.

use crate::cell::CellId;
use crate::http::{self, Outcome};
use crate::ingress::Dialect;
use crate::run::{path_for, RunConfig};
use std::time::Duration;

/// How long the control-plane calls and the single re-verification request may take. Generous
/// relative to what they do (one loopback round trip each): this runs once per served cell and a
/// timeout here costs an unchecked cell, so the deadline is set to outlast a busy box rather than to
/// be tight.
const REVERIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// The verdict, and the evidence behind it.
///
/// `verified` and `note` travel together because neither is usable alone: a bare `false` says a
/// gateway failed a check without saying what was seen instead, and a bare note is prose nothing can
/// act on. `note` is `None` only for the one case that needs no explanation - a proven translation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reverified {
    pub verified: Option<bool>,
    pub note: Option<String>,
}

impl Reverified {
    /// Not checked, for a stated reason. Never `false`: "we did not ask" and "we asked and it failed"
    /// are different claims and only one of them is about the gateway.
    fn unchecked(note: impl Into<String>) -> Self {
        Reverified { verified: None, note: Some(note.into()) }
    }

    fn proven() -> Self {
        Reverified { verified: Some(true), note: None }
    }

    fn refuted(note: impl Into<String>) -> Self {
        Reverified { verified: Some(false), note: Some(note.into()) }
    }
}

/// One dialect's row out of `/__mock/state`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DialectState {
    pub count: u64,
    pub body_ok: bool,
    pub last_path: String,
    pub last_snippet: String,
}

/// The mock's recorder, as read back off `/__mock/state`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MockState {
    /// Whether the mock was started with `MOCK_RECORD=1` at all. Served regardless of that flag
    /// precisely so a caller can tell "recording off" from "no requests arrived" - two documents that
    /// would otherwise be byte-identical and mean opposite things.
    pub recording: bool,
    pub dialects: std::collections::BTreeMap<String, DialectState>,
}

/// Parse `/__mock/state`'s document.
///
/// A free function over bytes so the whole verdict below it can be tested against real documents (and
/// against malformed ones) with no mock, no socket and no gateway - the same reason
/// `run::sustained_gate_passes` is a free function rather than logic inside a probe.
pub fn parse_state(body: &[u8]) -> Result<MockState, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("the mock's state document did not parse: {e}"))?;
    // A MISSING `recording` KEY IS NOT `false`. False is the mock telling us it was not recording;
    // missing means this is not the document we think it is, and reading it as "not recording" would
    // publish a parse failure of ours as a configuration fact about the run.
    let recording = v
        .get("recording")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "the mock's state document carries no `recording` flag".to_string())?;
    let mut dialects = std::collections::BTreeMap::new();
    if let Some(map) = v.get("dialects").and_then(serde_json::Value::as_object) {
        for (name, row) in map {
            dialects.insert(
                name.clone(),
                DialectState {
                    count: row.get("count").and_then(serde_json::Value::as_u64).unwrap_or(0),
                    body_ok: row.get("body_ok").and_then(serde_json::Value::as_bool).unwrap_or(false),
                    last_path: row.get("last_path").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
                    last_snippet: row
                        .get("last_snippet")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                },
            );
        }
    }
    Ok(MockState { recording, dialects })
}

/// THE VERDICT ITSELF, over an already-read recorder state. Pure.
///
/// Separate from the driving below for the reason `apply_peak_verdict` is separate from `judge_cell`:
/// every branch here is a claim the board publishes about somebody's product, and none of them can be
/// reached on demand from a fixture where one loopback server plays both the gateway and the mock.
///
/// `ingress` is needed even though only `egress` is being proven, because WHICH WRONG DIALECT the
/// request landed on is the difference between two findings: the ingress dialect's own endpoint means
/// the gateway forwarded our bytes verbatim, which is precisely the passthrough this guard exists to
/// catch, and any other endpoint means it translated to something that is not what this cell claims.
pub fn verdict(state: &MockState, ingress: Dialect, egress: Dialect) -> Reverified {
    if !state.recording {
        return Reverified::unchecked(
            "the mock reports it was not recording for this request, so it recorded nothing and cannot \
             say which dialect the gateway spoke upstream",
        );
    }
    let row = state.dialects.get(egress.as_str()).cloned().unwrap_or_default();
    if row.count > 0 {
        return if row.body_ok {
            Reverified::proven()
        } else {
            Reverified::refuted(format!(
                "a request reached the mock's {} endpoint at {:?}, but its body is not the {} request shape: {:?}",
                egress.as_str(),
                row.last_path,
                egress.as_str(),
                row.last_snippet
            ))
        };
    }

    // Nothing arrived on the egress endpoint. WHERE IT DID ARRIVE is the finding.
    let elsewhere: Vec<(&String, &DialectState)> =
        state.dialects.iter().filter(|(_, d)| d.count > 0).collect();
    if elsewhere.is_empty() {
        // The mock saw nothing at all. This says nothing about the gateway: the request may have been
        // answered from a cache, refused before the upstream call, or never have left. Publishing
        // `false` here would convict on an absence of evidence.
        return Reverified::unchecked(format!(
            "no request reached the mock on any dialect while re-verifying {}>{}, so what the gateway \
             emitted upstream was never observed",
            ingress.as_str(),
            egress.as_str()
        ));
    }
    let names: Vec<String> = elsewhere
        .iter()
        .map(|(n, d)| format!("{n} (count {}, last path {:?})", d.count, d.last_path))
        .collect();
    let forwarded_verbatim = elsewhere.iter().any(|(n, _)| n.as_str() == ingress.as_str());
    Reverified::refuted(if forwarded_verbatim {
        format!(
            "the request arrived on the mock's {} endpoint - the client's own ingress dialect - and \
             nothing arrived on {}, so the gateway forwarded the ingress request rather than \
             translating it: {}",
            ingress.as_str(),
            egress.as_str(),
            names.join(", ")
        )
    } else {
        format!(
            "nothing arrived on the mock's {} endpoint; the request landed on {}, so this cell's \
             egress dialect was not what the gateway emitted",
            egress.as_str(),
            names.join(", ")
        )
    })
}

/// Drive the re-verification for one served cell: clear the recorder, send exactly ONE request
/// through the gateway, read the recorder back.
///
/// ONE request, not the load window's thousands. The recorder keeps only the LAST body per dialect,
/// so a window would make `body_ok` a statement about whichever request happened to land last, and a
/// single request is all the proof this needs anyway: translation is a property of the code path, not
/// of how many times it ran.
///
/// Run BEFORE the metrics on a cell rather than after, because the metrics drive millions of requests
/// through the same recorder and the reset would then be racing an eight-minute memory window.
pub fn reverify_cell(cfg: &RunConfig, id: &CellId, ingress: Dialect) -> Reverified {
    let Ok(egress) = id.egress.parse::<Dialect>() else {
        return Reverified::unchecked(format!(
            "the egress dialect {:?} is not one this build knows, so there is no request shape to prove",
            id.egress
        ));
    };
    // A DIAGONAL CELL HAS NOTHING TO PROVE. Ingress and egress are the same dialect, so "the gateway
    // forwarded our bytes" and "the gateway translated openai to openai" are the same wire, and no
    // observation of the mock can separate them. `false` here would mark every passthrough cell in the
    // grid as a failed translation it was never asked to perform.
    if ingress == egress {
        return Reverified::unchecked(format!(
            "{}>{} is a same-dialect cell: there is no translation to prove, so a request-shape check \
             upstream cannot distinguish a translating gateway from a forwarding one",
            ingress.as_str(),
            egress.as_str()
        ));
    }

    if let Some(why) = control_failed(http::post_json(
        cfg.mock_addr,
        "/__mock/reset",
        b"",
        &[],
        REVERIFY_TIMEOUT,
    )) {
        return Reverified::unchecked(format!("the mock's recorder could not be cleared, so anything it holds may predate this cell: {why}"));
    }
    // FAIL CLOSED. A toggle that could not be set means the recorder's state is unknown, and the only
    // two ways to be wrong here are both unacceptable: assuming it is ON yields a refutation drawn
    // from an empty record, and assuming it is OFF leaves recording enabled for every load window
    // that follows. Reporting unchecked, with the reason, is the answer that asserts neither.
    if let Some(why) = set_recording(cfg, true) {
        return Reverified::unchecked(format!(
            "the mock's recorder could not be enabled for this cell, so what the gateway emitted upstream was never observed: {why}"
        ));
    }

    // The SAME request the probe sent, at the SAME path with the SAME headers: a re-verification that
    // drove a different request would prove something about a wire this cell never measured.
    let path = path_for(cfg, ingress, &id.egress);
    let body = ingress.body(&cfg.model);
    let headers = crate::run::headers_for(cfg, ingress, &id.egress);
    let driven = http::post_json(cfg.gateway_addr, &path, body.as_bytes(), &headers, REVERIFY_TIMEOUT);
    if let Some(why) = control_failed(driven) {
        return Reverified::unchecked(format!(
            "the re-verification request to the gateway produced no answer, so nothing was driven upstream to observe: {why}"
        ));
    }

    let read = http::get(cfg.mock_addr, "/__mock/state", &[], REVERIFY_TIMEOUT);
    // OFF AGAIN BEFORE ANYTHING ELSE RUNS, including before this function's own return path decides
    // anything: every load window on this cell happens after this point, and the guarantee they rely
    // on is that the mock is quiet by the time they start.
    let disabled = set_recording(cfg, false);
    let state = match read {
        Outcome::Response(r) => match parse_state(r.body()) {
            Ok(s) => s,
            Err(e) => return Reverified::unchecked(format!("the mock's recorder could not be read: {e}")),
        },
        other => {
            return Reverified::unchecked(format!(
                "the mock's recorder could not be read: {}",
                control_failed(other).unwrap_or_else(|| "no response".to_string())
            ))
        }
    };
    // A recorder that could not be turned back off has left every window that follows measuring a
    // slower mock, which is a claim about the RIG's state and belongs beside this cell's verdict even
    // though the verdict itself is sound.
    if let Some(why) = disabled {
        eprintln!("reverify: the mock's recorder could not be disabled after {}>{}, so the windows that follow are taken against a recording mock: {why}", ingress.as_str(), egress.as_str());
    }
    verdict(&state, ingress, egress)
}

/// Turn the mock's recorder on or off, returning why it could not be done.
///
/// Its own function because BOTH directions have to be checked and neither may be assumed: the
/// enable is what the verdict rests on, and the disable is what every load window after this cell
/// rests on.
fn set_recording(cfg: &RunConfig, on: bool) -> Option<String> {
    let body = format!("{{\"on\":{on}}}");
    control_failed(http::post_json(cfg.mock_addr, "/__mock/record", body.as_bytes(), &[], REVERIFY_TIMEOUT))
}

/// Leave the mock's recorder OFF before a run's measurements begin.
///
/// The field boots the mock quiet, so this is normally a no-op that costs one request. It is kept
/// because the mock also honours MOCK_RECORD=1 as a starting state for local debugging, and because
/// a mock left recording by a previous run's crash would otherwise taint this one: the
/// box-qualification window runs before the first cell is ever re-verified, so the run's very first
/// load window - the one whose rate becomes the baseline every later run on this box is judged
/// against - would be taken against a recording mock while every window after it is not.
///
/// Best effort by design: a mock that will not answer this is a rig fault the suite reports through
/// its own per-cell verdicts, not a reason to refuse a run that may still produce honest numbers.
pub fn quiesce_recorder(cfg: &RunConfig) -> Option<String> {
    set_recording(cfg, false)
}

/// Why a control-plane call is unusable, or `None` if it answered. A non-2xx from the MOCK's own
/// control plane is a rig fault, never a gateway one, so it reads the same way a connection failure
/// does here rather than becoming evidence about the cell.
fn control_failed(outcome: Outcome) -> Option<String> {
    match outcome {
        Outcome::Response(r) if (200..300).contains(&r.status) => None,
        Outcome::Response(r) => Some(format!("HTTP {}", r.status)),
        Outcome::ConnectionFailed(e) => Some(format!("no connection: {e}")),
        Outcome::TimedOut => Some("timed out".to_string()),
        Outcome::Malformed { message, .. } => Some(format!("unparseable response: {message}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(recording: bool, rows: &[(&str, u64, bool)]) -> MockState {
        let mut dialects = std::collections::BTreeMap::new();
        for (name, count, body_ok) in rows {
            dialects.insert(
                (*name).to_string(),
                DialectState {
                    count: *count,
                    body_ok: *body_ok,
                    last_path: format!("/{name}"),
                    last_snippet: format!("{{\"seen\":\"{name}\"}}"),
                },
            );
        }
        MockState { recording, dialects }
    }

    // ── the document ────────────────────────────────────────────────────────────────────────────

    // The exact shape mock/src/main.rs's `state_json` writes, pinned here rather than approximated:
    // this is a cross-binary contract, and the mock's own tests cannot see this side of it.
    #[test]
    fn the_mocks_own_state_document_parses_into_the_shape_the_verdict_reads() {
        let doc = br#"{"recording":true,"dialects":{"openai":{"count":2,"body_ok":true,"last_path":"/v1/chat/completions","last_snippet":"{\"messages\":[]}"},"anthropic":{"count":0,"body_ok":false,"last_path":"","last_snippet":""}}}"#;
        let s = parse_state(doc).expect("the mock's own document must parse");
        assert!(s.recording);
        let openai = s.dialects.get("openai").expect("the openai row");
        assert_eq!(openai.count, 2);
        assert!(openai.body_ok);
        assert_eq!(openai.last_path, "/v1/chat/completions");
        assert_eq!(openai.last_snippet, "{\"messages\":[]}");
        assert_eq!(s.dialects.get("anthropic").map(|d| d.count), Some(0));
    }

    // A document with no `recording` key is not a mock state document. Reading it as "not recording"
    // would turn a parse failure of ours into a claim about how the run was configured.
    #[test]
    fn a_document_without_the_recording_flag_is_an_error_not_a_silent_false() {
        assert!(parse_state(br#"{"dialects":{}}"#).is_err());
        assert!(parse_state(b"not json at all").is_err());
    }

    // ── the verdict ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn a_request_in_the_egress_dialects_own_shape_proves_the_translation() {
        let v = verdict(&state(true, &[("anthropic", 1, true)]), Dialect::Openai, Dialect::Anthropic);
        assert_eq!(v.verified, Some(true));
        assert_eq!(v.note, None, "a proven translation needs no explanation");
    }

    // THE DEFECT THIS WHOLE MODULE EXISTS FOR: the gateway forwarded the client's own ingress request
    // verbatim, the mock answered its canned body by path, and the probe saw a 200. Without this the
    // cell publishes a translation the gateway never performed.
    #[test]
    fn a_request_forwarded_verbatim_to_the_ingress_endpoint_is_refuted_with_what_was_seen() {
        let v = verdict(&state(true, &[("openai", 1, true)]), Dialect::Openai, Dialect::Anthropic);
        assert_eq!(v.verified, Some(false));
        let note = v.note.unwrap_or_default();
        assert!(note.contains("openai"), "the note must name the endpoint that was actually hit: {note}");
        assert!(note.contains("forwarded"), "the finding is a forward, not a mistranslation: {note}");
        assert!(note.contains("/openai"), "the last path is the evidence and must travel: {note}");
    }

    // Arrived on the right endpoint, wrong shape: the mock's own `request_shape_ok` said no. Still a
    // refutation, but a different one, and the note must not claim a verbatim forward.
    #[test]
    fn a_request_on_the_right_endpoint_in_the_wrong_shape_is_refuted_with_the_body_it_sent() {
        let v = verdict(&state(true, &[("anthropic", 1, false)]), Dialect::Openai, Dialect::Anthropic);
        assert_eq!(v.verified, Some(false));
        let note = v.note.unwrap_or_default();
        assert!(note.contains("not the anthropic request shape"), "{note}");
        assert!(note.contains("seen"), "the body snippet is the evidence and must travel: {note}");
    }

    // Landed on a third dialect entirely: not a verbatim forward, but still not this cell's egress.
    #[test]
    fn a_request_that_lands_on_some_other_dialect_is_refuted_without_calling_it_a_forward() {
        let v = verdict(&state(true, &[("gemini", 3, true)]), Dialect::Openai, Dialect::Anthropic);
        assert_eq!(v.verified, Some(false));
        let note = v.note.unwrap_or_default();
        assert!(note.contains("gemini"), "{note}");
        assert!(!note.contains("forwarded"), "nothing was forwarded verbatim here: {note}");
    }

    // ── the three ways this is UNCHECKED, none of which may read as a failed gateway ─────────────

    #[test]
    fn a_mock_that_was_never_recording_is_unchecked_never_false() {
        let v = verdict(&state(false, &[]), Dialect::Openai, Dialect::Anthropic);
        assert_eq!(v.verified, None, "a mock that was not recording says nothing about the gateway");
        // The note must say the RECORDER was off, not that the gateway did anything wrong. The field
        // boots the mock quiet on purpose and the engine toggles recording around this one request,
        // so reaching here means the toggle did not take - a rig fault, and the note has to read as
        // one or a reader will take it for a gateway defect.
        let note = v.note.unwrap_or_default();
        assert!(note.contains("recording"), "the note must name the recorder as the reason: {note}");
    }

    // NOTHING ARRIVED is an absence of evidence, not evidence of absence. Publishing `false` here
    // would convict a gateway because our own observation failed.
    #[test]
    fn a_recorder_that_saw_nothing_at_all_is_unchecked_never_false() {
        let v = verdict(&state(true, &[("openai", 0, false)]), Dialect::Openai, Dialect::Anthropic);
        assert_eq!(v.verified, None);
        assert!(v.note.unwrap_or_default().contains("never observed"));
    }

    // The mock records `body_ok: false` for every dialect it has not seen, so a recorder that saw
    // nothing must not be read off that flag: `count` is what says whether there is a verdict to
    // read at all. This is the exact confusion that would turn an unconfigured run into a field of
    // gateways that all "failed to translate".
    #[test]
    fn an_untouched_dialects_default_body_ok_false_is_never_read_as_a_refutation() {
        let mut s = state(true, &[]);
        for d in ["openai", "openai-responses", "anthropic", "gemini", "cohere", "bedrock", "other"] {
            s.dialects.insert(d.to_string(), DialectState::default());
        }
        assert_eq!(verdict(&s, Dialect::Openai, Dialect::Anthropic).verified, None);
    }

    // ── the diagonal ────────────────────────────────────────────────────────────────────────────

    // A same-dialect cell has no translation to prove: forwarding and translating produce the SAME
    // bytes upstream, so no observation of the mock can separate them. `None` with a reason is the
    // honest answer; `false` would mark every passthrough cell in the grid as a failure.
    #[test]
    fn a_same_dialect_cell_is_unchecked_because_there_is_nothing_to_prove() {
        let cfg = crate::run::test_fixture(
            "127.0.0.1:1".parse().expect("a literal loopback address parses"),
            "127.0.0.1:2".parse().expect("a literal loopback address parses"),
        );
        let v = reverify_cell(&cfg, &CellId::new("openai", "openai"), Dialect::Openai);
        assert_eq!(v.verified, None);
        let note = v.note.unwrap_or_default();
        assert!(note.contains("same-dialect"), "{note}");
        // And it never touched the network: the mock address above is a dead port, so a check that
        // tried to reach it would have come back with a connection failure in the note instead.
        assert!(!note.contains("connection"), "{note}");
    }

    // An unreachable mock is a rig fault. It must not be published as the gateway failing to
    // translate, which is the same inversion `probe.rs` exists to prevent for the served verdict.
    #[test]
    fn an_unreachable_mock_is_unchecked_never_false() {
        let cfg = crate::run::test_fixture(
            "127.0.0.1:1".parse().expect("a literal loopback address parses"),
            "127.0.0.1:2".parse().expect("a literal loopback address parses"),
        );
        let v = reverify_cell(&cfg, &CellId::new("openai", "anthropic"), Dialect::Openai);
        assert_eq!(v.verified, None, "a rig failure must never convict the gateway");
        assert!(v.note.unwrap_or_default().contains("recorder could not be cleared"));
    }
}
