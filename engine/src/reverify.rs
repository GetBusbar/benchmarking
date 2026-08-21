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
// against: a slower mock means every gateway measured against it looks slower, and a rig regression
// that shifts the reference is the worst kind, because every number stays internally consistent
// while all of them move together.
//
// The earlier version of this comment described the consequence as cells "having their real, honest
// throughput SUPPRESSED as mock-bound" within 10% of the ceiling, citing `rigbound::is_rig_bound`.
// That suppression mechanism was deleted - `rigbound.rs`'s own header records why, and the function
// named here no longer exists - so nothing is suppressed today; a slow mock now simply understates
// every gateway. The requirement below is unchanged, but the reason it protects is a different one,
// and leaving the old reason in place would have a reader looking for a threshold that is gone.
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
        Reverified {
            verified: None,
            note: Some(note.into()),
        }
    }

    fn proven() -> Self {
        Reverified {
            verified: Some(true),
            note: None,
        }
    }

    fn refuted(note: impl Into<String>) -> Self {
        Reverified {
            verified: Some(false),
            note: Some(note.into()),
        }
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
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| format!("the mock's state document did not parse: {e}"))?;
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
                    count: row
                        .get("count")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                    // A PRESENT-BUT-WRONG-TYPED body_ok is a malformed document, not a `false`. Since
                    // verdict() checks count>0 before reading body_ok, a doc with count already >0 but
                    // body_ok of the wrong JSON type used to default to false and produce a REFUTED -
                    // a false accusation of a dialect-shape failure - when the real cause is the mock's
                    // document being malformed. Refuse it so the caller falls back to `unchecked`
                    // ("THREE ANSWERS, NOT TWO"). A genuinely ABSENT body_ok stays false, the same way
                    // an absent count stays 0, for an older document that never carried the field.
                    body_ok: match row.get("body_ok") {
                        None => false,
                        Some(serde_json::Value::Bool(b)) => *b,
                        Some(other) => {
                            return Err(format!(
                                "dialect '{name}' has a non-boolean body_ok: {other}"
                            ))
                        }
                    },
                    last_path: row
                        .get("last_path")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    last_snippet: row
                        .get("last_snippet")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                },
            );
        }
    }
    Ok(MockState {
        recording,
        dialects,
    })
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
    let row = state
        .dialects
        .get(egress.as_str())
        .cloned()
        .unwrap_or_default();
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
    let forwarded_verbatim = elsewhere
        .iter()
        .any(|(n, _)| n.as_str() == ingress.as_str());
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
    //
    // ONE ENABLE, ONE DISABLE, AND NOTHING RETURNS BETWEEN THEM. The failure is a VALUE from here on,
    // never a `return`: every way this can go wrong used to leave the function on its own line, and a
    // gateway that merely timed out on this single request therefore left the recorder ON for its own
    // load windows and for every cell after it - a slower mock under the rest of the grid, with
    // nothing in the artifact saying so. `drive_and_read` hands its failures back instead, so the
    // disable below is the only way out.
    let outcome = match set_recording(cfg, true) {
        Some(why) => Err(Reverified::unchecked(format!(
            "the mock's recorder could not be enabled for this cell, so what the gateway emitted upstream was never observed: {why}"
        ))),
        // The enable answered badly, and a toggle that answered badly may still have TAKEN, so this
        // arm falls through to the same disable the success path does rather than returning here.
        None => drive_and_read(cfg, id, ingress),
    };

    // OFF AGAIN BEFORE ANYTHING ELSE RUNS, including before this function's own return path decides
    // anything: every load window on this cell happens after this point, and the guarantee they rely
    // on is that the mock is quiet by the time they start. Unconditional, and the only exit from this
    // function is past it.
    let disabled = set_recording(cfg, false);
    // A recorder that could not be turned back off has left every window that follows measuring a
    // slower mock, which is a claim about the RIG's state and belongs beside this cell's verdict even
    // though the verdict itself is sound.
    if let Some(why) = disabled {
        eprintln!("reverify: the mock's recorder could not be disabled after {}>{}, so the windows that follow are taken against a recording mock: {why}", ingress.as_str(), egress.as_str());
    }

    match outcome {
        Ok(state) => verdict(&state, ingress, egress),
        Err(unchecked) => unchecked,
    }
}

/// Drive the one re-verification request and read the recorder back.
///
/// SEPARATED SO THE DISABLE CANNOT BE SKIPPED, which is the same reason `parse_state` and `verdict`
/// above are free functions: what is left in the caller is sequencing, and sequencing with one exit
/// cannot forget a step. Its caller has turned the mock's recorder ON and owes the run a matching
/// turn-off, so every failure in here is an `Err(Reverified)` the caller carries PAST that turn-off
/// rather than a `return` that jumps over it. A new failure mode added here inherits that for free.
fn drive_and_read(cfg: &RunConfig, id: &CellId, ingress: Dialect) -> Result<MockState, Reverified> {
    // THE SAME REQUEST THE PROBE SENT: same path, same headers, and - the part that was wrong - the
    // same MODEL.
    //
    // This sent `cfg.model`, the gateway's base model name, while the path and headers were built for
    // `id.egress`. On every gateway whose upstreams are selected BY MODEL NAME - which is most of
    // them, and is exactly how this harness configures them - the base name routes to the openai
    // upstream no matter which egress the cell is about. So the gateway did the right thing, the mock
    // recorded /v1/chat/completions, and this concluded "the gateway forwarded the ingress request
    // rather than translating it".
    //
    // It said that about 18 cells across 6 gateways in the 2026-07-28 field run, including
    // litellm-python, whose entire product is protocol translation. The verdict is an accusation
    // published about someone else's software, and it was being made from a request that asked for
    // the wrong upstream. `model_for` is what every probe and load window in `run.rs` uses; using
    // anything else here means re-verifying a wire the cell never measured.
    let path = path_for(cfg, ingress, &id.egress);
    let body = ingress.body(&crate::run::model_for(cfg, &id.egress));
    let headers = crate::run::headers_for(cfg, ingress, &id.egress);
    let driven = http::post_json(
        cfg.gateway_addr,
        &path,
        body.as_bytes(),
        &headers,
        REVERIFY_TIMEOUT,
    );
    if let Some(why) = control_failed(driven) {
        return Err(Reverified::unchecked(format!(
            "the re-verification request to the gateway produced no answer, so nothing was driven upstream to observe: {why}"
        )));
    }

    match http::get(cfg.mock_addr, "/__mock/state", &[], REVERIFY_TIMEOUT) {
        Outcome::Response(r) => parse_state(r.body()).map_err(|e| {
            Reverified::unchecked(format!("the mock's recorder could not be read: {e}"))
        }),
        other => Err(Reverified::unchecked(format!(
            "the mock's recorder could not be read: {}",
            control_failed(other).unwrap_or_else(|| "no response".to_string())
        ))),
    }
}

/// Turn the mock's recorder on or off, returning why it could not be done.
///
/// Its own function because BOTH directions have to be checked and neither may be assumed: the
/// enable is what the verdict rests on, and the disable is what every load window after this cell
/// rests on.
fn set_recording(cfg: &RunConfig, on: bool) -> Option<String> {
    let body = format!("{{\"on\":{on}}}");
    control_failed(http::post_json(
        cfg.mock_addr,
        "/__mock/record",
        body.as_bytes(),
        &[],
        REVERIFY_TIMEOUT,
    ))
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
        // OUR OWN REFUSAL, and it lands here rather than anywhere near the cell's verdict for exactly
        // the reason this function exists: everything it returns is a RIG fault that makes the
        // re-verification unusable, never evidence about a gateway. The re-verify lane composes the
        // same manifest headers the load lane does, and until `http::unsendable_request` was applied
        // to `send` it was the lane that still interpolated them raw.
        Outcome::RigRefused(why) => Some(format!(
            "the rig refused to send this control-plane request: {why}"
        )),
        // Ours, not the peer's: see `Outcome::RigExhausted`. Filed the same way `RigRefused` is - a
        // reason this control-plane call is unusable, never evidence about the gateway or mock.
        Outcome::RigExhausted(e) => Some(format!(
            "the rig ran out of its own connection resources before the peer could be asked: {e}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // control_failed must attribute a rig fault to the RIG, never to the gateway/mock it was asking.
    // A rig that ran out of its own ports/descriptors (Outcome::RigExhausted) is ours: the control
    // request was never delivered, so the re-verification is unusable for a reason that says nothing
    // about the peer. This pins that the RigExhausted arm names the rig; if it were removed (falling
    // through to a bucket that reads as the peer failing) the assertion would break.
    #[test]
    fn control_failed_attributes_a_rig_exhaustion_to_the_rig_not_the_peer() {
        let why = control_failed(Outcome::RigExhausted("EMFILE (os error 24)".to_string()))
            .expect("a rig-exhausted control call is a failure, not a success");
        assert!(
            why.contains("rig ran out"),
            "a rig exhaustion must be named as the rig's own resource fault, got {why:?}"
        );
        assert!(
            !why.to_ascii_lowercase().contains("gateway"),
            "it must not read as the gateway (or mock) failing, got {why:?}"
        );
        // Filed the same way the sibling rig-side refusal is, and distinctly from a genuine peer-side
        // connection failure (which IS a claim about the peer's endpoint).
        let refused = control_failed(Outcome::RigRefused("smuggled header".to_string()))
            .expect("a rig-refused control call is a failure");
        assert!(refused.contains("rig refused"), "got {refused:?}");
        let peer = control_failed(Outcome::ConnectionFailed("refused".to_string()))
            .expect("a connection failure is a failure");
        assert!(
            peer.contains("no connection"),
            "a genuine peer-side connection failure stays a connection failure, got {peer:?}"
        );
        // A timeout is the peer not answering, distinct again from a rig-side resource fault.
        assert_eq!(
            control_failed(Outcome::TimedOut).as_deref(),
            Some("timed out")
        );
    }

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
        MockState {
            recording,
            dialects,
        }
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

    // A PRESENT-BUT-WRONG-TYPED body_ok is a malformed document, not a `false`. With count already >0,
    // defaulting a mistyped body_ok to false used to make verdict() publish a REFUTED - a false
    // accusation of a dialect-shape failure - when the real cause is the mock's document being
    // malformed. parse_state must refuse it so the caller falls back to `unchecked`.
    #[test]
    fn a_non_boolean_body_ok_is_a_parse_error_not_a_defaulted_false() {
        // count>0 with body_ok as a STRING: the exact shape that would otherwise refute.
        let doc = br#"{"recording":true,"dialects":{"openai":{"count":5,"body_ok":"yes","last_path":"/v1/chat/completions","last_snippet":""}}}"#;
        assert!(
            parse_state(doc).is_err(),
            "a non-boolean body_ok must be refused rather than read as false and refuted"
        );
        // A genuinely ABSENT body_ok still parses (older document), defaulting to false like count.
        let absent = br#"{"recording":true,"dialects":{"openai":{"count":0,"last_path":"","last_snippet":""}}}"#;
        let s = parse_state(absent).expect("an absent body_ok is not a malformed document");
        assert!(!s.dialects["openai"].body_ok);
    }

    // ── the verdict ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn a_request_in_the_egress_dialects_own_shape_proves_the_translation() {
        let v = verdict(
            &state(true, &[("anthropic", 1, true)]),
            Dialect::Openai,
            Dialect::Anthropic,
        );
        assert_eq!(v.verified, Some(true));
        assert_eq!(v.note, None, "a proven translation needs no explanation");
    }

    // THE DEFECT THIS WHOLE MODULE EXISTS FOR: the gateway forwarded the client's own ingress request
    // verbatim, the mock answered its canned body by path, and the probe saw a 200. Without this the
    // cell publishes a translation the gateway never performed.
    #[test]
    fn a_request_forwarded_verbatim_to_the_ingress_endpoint_is_refuted_with_what_was_seen() {
        let v = verdict(
            &state(true, &[("openai", 1, true)]),
            Dialect::Openai,
            Dialect::Anthropic,
        );
        assert_eq!(v.verified, Some(false));
        let note = v.note.unwrap_or_default();
        assert!(
            note.contains("openai"),
            "the note must name the endpoint that was actually hit: {note}"
        );
        assert!(
            note.contains("forwarded"),
            "the finding is a forward, not a mistranslation: {note}"
        );
        assert!(
            note.contains("/openai"),
            "the last path is the evidence and must travel: {note}"
        );
    }

    // Arrived on the right endpoint, wrong shape: the mock's own `request_shape_ok` said no. Still a
    // refutation, but a different one, and the note must not claim a verbatim forward.
    #[test]
    fn a_request_on_the_right_endpoint_in_the_wrong_shape_is_refuted_with_the_body_it_sent() {
        let v = verdict(
            &state(true, &[("anthropic", 1, false)]),
            Dialect::Openai,
            Dialect::Anthropic,
        );
        assert_eq!(v.verified, Some(false));
        let note = v.note.unwrap_or_default();
        assert!(note.contains("not the anthropic request shape"), "{note}");
        assert!(
            note.contains("seen"),
            "the body snippet is the evidence and must travel: {note}"
        );
    }

    // Landed on a third dialect entirely: not a verbatim forward, but still not this cell's egress.
    #[test]
    fn a_request_that_lands_on_some_other_dialect_is_refuted_without_calling_it_a_forward() {
        let v = verdict(
            &state(true, &[("gemini", 3, true)]),
            Dialect::Openai,
            Dialect::Anthropic,
        );
        assert_eq!(v.verified, Some(false));
        let note = v.note.unwrap_or_default();
        assert!(note.contains("gemini"), "{note}");
        assert!(
            !note.contains("forwarded"),
            "nothing was forwarded verbatim here: {note}"
        );
    }

    // ── the three ways this is UNCHECKED, none of which may read as a failed gateway ─────────────

    #[test]
    fn a_mock_that_was_never_recording_is_unchecked_never_false() {
        let v = verdict(&state(false, &[]), Dialect::Openai, Dialect::Anthropic);
        assert_eq!(
            v.verified, None,
            "a mock that was not recording says nothing about the gateway"
        );
        // The note must say the RECORDER was off, not that the gateway did anything wrong. The field
        // boots the mock quiet on purpose and the engine toggles recording around this one request,
        // so reaching here means the toggle did not take - a rig fault, and the note has to read as
        // one or a reader will take it for a gateway defect.
        let note = v.note.unwrap_or_default();
        assert!(
            note.contains("recording"),
            "the note must name the recorder as the reason: {note}"
        );
    }

    // NOTHING ARRIVED is an absence of evidence, not evidence of absence. Publishing `false` here
    // would convict a gateway because our own observation failed.
    #[test]
    fn a_recorder_that_saw_nothing_at_all_is_unchecked_never_false() {
        let v = verdict(
            &state(true, &[("openai", 0, false)]),
            Dialect::Openai,
            Dialect::Anthropic,
        );
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
        for d in [
            "openai",
            "openai-responses",
            "anthropic",
            "gemini",
            "cohere",
            "bedrock",
            "other",
        ] {
            s.dialects.insert(d.to_string(), DialectState::default());
        }
        assert_eq!(
            verdict(&s, Dialect::Openai, Dialect::Anthropic).verified,
            None
        );
    }

    // ── the diagonal ────────────────────────────────────────────────────────────────────────────

    // A same-dialect cell has no translation to prove: forwarding and translating produce the SAME
    // bytes upstream, so no observation of the mock can separate them. `None` with a reason is the
    // honest answer; `false` would mark every passthrough cell in the grid as a failure.
    #[test]
    fn a_same_dialect_cell_is_unchecked_because_there_is_nothing_to_prove() {
        let cfg = crate::run::test_fixture(
            "127.0.0.1:1"
                .parse()
                .expect("a literal loopback address parses"),
            "127.0.0.1:2"
                .parse()
                .expect("a literal loopback address parses"),
        );
        let v = reverify_cell(&cfg, &CellId::new("openai", "openai"), Dialect::Openai);
        assert_eq!(v.verified, None);
        let note = v.note.unwrap_or_default();
        assert!(note.contains("same-dialect"), "{note}");
        // And it never touched the network: the mock address above is a dead port, so a check that
        // tried to reach it would have come back with a connection failure in the note instead.
        assert!(!note.contains("connection"), "{note}");
    }

    // A minimal stand-in for the mock's control plane: tracks whether `/__mock/record` last turned
    // recording on or off, and answers `/__mock/reset` and `/__mock/state` well enough for
    // `reverify_cell` to drive its whole sequence against it. No `Content-Length` framing subtlety
    // beyond what `reverify_cell` itself sends, since this is standing in for the mock, not testing
    // this client's HTTP parsing (that is `http.rs`'s own job).
    fn fake_mock(recording: std::sync::Arc<std::sync::atomic::AtomicBool>) -> std::net::SocketAddr {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port to pick one");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                let recording = std::sync::Arc::clone(&recording);
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    // Read until the head/body this test ever sends has fully arrived: reset and
                    // record bodies are a handful of bytes, so one read is enough in practice, but
                    // loop for robustness against a split read.
                    loop {
                        match c.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => {
                                buf.extend_from_slice(&chunk[..n]);
                                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let text = String::from_utf8_lossy(&buf);
                    let first_line = text.lines().next().unwrap_or("");
                    let body = text.rsplit("\r\n\r\n").next().unwrap_or("");
                    let json = if first_line.contains("/__mock/record") {
                        recording.store(
                            body.contains("\"on\":true"),
                            std::sync::atomic::Ordering::SeqCst,
                        );
                        "{}".to_string()
                    } else if first_line.contains("/__mock/reset") {
                        "{}".to_string()
                    } else if first_line.contains("/__mock/state") {
                        format!(
                            "{{\"recording\":{},\"dialects\":{{}}}}",
                            recording.load(std::sync::atomic::Ordering::SeqCst)
                        )
                    } else {
                        "{}".to_string()
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                        json.len(),
                        json
                    );
                    let _ = c.write_all(resp.as_bytes());
                });
            }
        });
        addr
    }

    // THE DEFECT THIS PINS: if the single re-verification request to the GATEWAY fails (here,
    // nothing is listening on the gateway address at all, the cheapest way to force `control_failed`
    // to fire on the gateway send inside `drive_and_read`), the driver returns early without ever
    // reaching the matching `set_recording(cfg, false)`. The recorder the mock was told to turn ON by
    // the earlier `set_recording(cfg, true)` is left ON, so every load window the suite runs on this
    // cell (and any cell after it, until some later re-verify happens to succeed) is measured against
    // a recording - and therefore slower - mock. (Call sites named rather than line-numbered: the
    // numbers this comment used to cite had drifted from the file.)
    #[test]
    fn a_failed_gateway_request_must_not_leave_the_mocks_recorder_on() {
        let recording = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mock_addr = fake_mock(std::sync::Arc::clone(&recording));
        // Nothing listens here: the gateway leg of the re-verification request fails to connect,
        // which is `control_failed`'s cheapest, fastest-to-trigger branch.
        let dead_gateway: std::net::SocketAddr = "127.0.0.1:1".parse().expect("literal");
        let cfg = crate::run::test_fixture(dead_gateway, mock_addr);

        let v = reverify_cell(&cfg, &CellId::new("openai", "anthropic"), Dialect::Openai);

        assert_eq!(
            v.verified, None,
            "an unreachable gateway must not be published as a failed translation"
        );
        assert!(
            !recording.load(std::sync::atomic::Ordering::SeqCst),
            "the mock's recorder must be turned back OFF once the re-verification request to the \
             gateway fails, so every load window that follows measures against a quiet mock; it is \
             still ON"
        );
    }

    /// A fake GATEWAY that records the request body it was handed, and answers 200.
    ///
    /// The mock-side fake above cannot see this: the re-verification request goes to the GATEWAY
    /// address, and what this module has to get right is which model name it puts on that wire. So the
    /// only way to test it is to be the gateway.
    fn fake_gateway(seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> std::net::SocketAddr {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port to pick one");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                let seen = std::sync::Arc::clone(&seen);
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        match c.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => {
                                buf.extend_from_slice(&chunk[..n]);
                                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let body = text.rsplit("\r\n\r\n").next().unwrap_or("").to_string();
                    seen.lock().expect("seen lock").push(body);
                    let _ = c.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\n{}",
                    );
                });
            }
        });
        addr
    }

    // THE MODEL NAME THAT ACTUALLY GOES ON THE WIRE, not the one a helper would have returned.
    //
    // `the_reverify_request_names_the_cells_own_egress_model` below recomputes `model_for` and
    // `Dialect::body` by hand and asserts properties of THEIR return values. It never calls
    // `reverify_cell`, so it cannot see which body this module really sends - and reverting the
    // construction at the top of `drive_and_read` to `ingress.body(&cfg.model)`, the exact regression
    // this module's largest comment block describes, leaves all thirteen tests in this file green.
    //
    // That defect is not hypothetical: naming the base model routes a model-routed gateway to its
    // OPENAI upstream, so the mock records openai, the egress under test is never exercised, and the
    // cell publishes "the gateway forwarded rather than translated" about a translation that was never
    // requested. It did that to 18 cells across 6 gateways in the 2026-07-28 field run.
    //
    // So this one is the gateway, and reads the body off the socket.
    #[test]
    fn the_body_sent_to_the_gateway_names_the_cells_own_egress_model() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let gateway = fake_gateway(std::sync::Arc::clone(&seen));
        let recording = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mock_addr = fake_mock(std::sync::Arc::clone(&recording));
        let mut cfg = crate::run::test_fixture(gateway, mock_addr);
        cfg.model = "gpt-4o-mini".into();
        cfg.egress_models = [("anthropic".to_string(), "gpt-4o-mini-anthropic".to_string())]
            .into_iter()
            .collect();

        let _ = reverify_cell(&cfg, &CellId::new("openai", "anthropic"), Dialect::Openai);

        let bodies = seen.lock().expect("seen lock").clone();
        assert!(
            !bodies.is_empty(),
            "the re-verification request never reached the gateway, so this test proves nothing"
        );
        let sent = bodies.join("\n");
        assert!(
            sent.contains("gpt-4o-mini-anthropic"),
            "the body on the wire must name the CELL'S egress model - the anthropic upstream is what \
             this cell claims to translate to, and re-verifying any other route says nothing about \
             it. Sent: {sent}"
        );
        // And specifically NOT the base name on its own, which is the openai upstream's model and the
        // shape of the original defect.
        assert!(
            !sent.contains("\"model\":\"gpt-4o-mini\""),
            "the base model name routes a model-routed gateway to its OPENAI upstream, so the cell \
             would re-verify a route it never measured. Sent: {sent}"
        );
    }

    // An unreachable mock is a rig fault. It must not be published as the gateway failing to
    // translate, which is the same inversion `probe.rs` exists to prevent for the served verdict.
    #[test]
    fn an_unreachable_mock_is_unchecked_never_false() {
        let cfg = crate::run::test_fixture(
            "127.0.0.1:1"
                .parse()
                .expect("a literal loopback address parses"),
            "127.0.0.1:2"
                .parse()
                .expect("a literal loopback address parses"),
        );
        let v = reverify_cell(&cfg, &CellId::new("openai", "anthropic"), Dialect::Openai);
        assert_eq!(
            v.verified, None,
            "a rig failure must never convict the gateway"
        );
        assert!(v
            .note
            .unwrap_or_default()
            .contains("recorder could not be cleared"));
    }

    // RE-VERIFICATION MUST DRIVE THE CELL'S OWN WIRE.
    //
    // The path and headers were built from `id.egress` while the BODY carried the gateway's base
    // model name. Most gateways here select their upstream BY MODEL NAME, so the base name routes to
    // the openai upstream whatever the cell is about: the gateway behaved correctly, the mock
    // recorded /v1/chat/completions, and this concluded the gateway "forwarded the ingress request
    // rather than translating it". It published that about 18 cells across 6 gateways in the
    // 2026-07-28 field run - including litellm-python, whose whole product is translation.
    //
    // A capability verdict is an accusation about somebody else's software. Making one from a
    // request that asked for the wrong upstream is the worst way to be wrong.
    #[test]
    fn the_reverify_request_names_the_cells_own_egress_model() {
        let cfg = crate::run::test_fixture(
            "127.0.0.1:1".parse().expect("addr"),
            "127.0.0.1:1".parse().expect("addr"),
        );
        let mut cfg = cfg;
        cfg.model = "gpt-4o-mini".into();
        cfg.egress_models = [
            ("openai".to_string(), "gpt-4o-mini".to_string()),
            ("anthropic".to_string(), "gpt-4o-mini-anthropic".to_string()),
            ("gemini".to_string(), "gpt-4o-mini-gemini".to_string()),
        ]
        .into_iter()
        .collect();

        // What the measurement path sends for a cell, and therefore what re-verification must send:
        // anything else re-verifies a route the cell never measured.
        for egress in ["anthropic", "gemini"] {
            let measured = crate::run::model_for(&cfg, egress);
            assert_ne!(
                measured, cfg.model,
                "{egress} must resolve to its own model, or this test proves nothing"
            );
            let body = crate::ingress::Dialect::Openai.body(&measured);
            assert!(
                body.contains(&measured),
                "the re-verify body must name the cell's egress model {measured}: {body}"
            );
            // The defect: the base name routes to the openai upstream on a model-routed gateway.
            let wrong = crate::ingress::Dialect::Openai.body(&cfg.model);
            assert!(
                !wrong.contains(&measured),
                "the base model {} must NOT be what a {egress} cell drives - that is the bug",
                cfg.model
            );
        }

        // An egress with no declared model falls back to the base name, and that is correct: there
        // is no separate route to ask for.
        assert_eq!(crate::run::model_for(&cfg, "openai"), "gpt-4o-mini");
    }
}
