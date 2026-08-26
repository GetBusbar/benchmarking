// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Re-verifies that a gateway actually translated a request rather than forwarding it verbatim.
// The mock answers all dialects by path, so a passthrough gateway that does no translation at all
// can still get a plausible 200 back - `probe_cell` alone can't tell that apart from a real
// translation, and publishing `served: true` for a capability the gateway doesn't have is the
// failure mode this module exists to prevent.
//
// Under `MOCK_RECORD=1` the mock records, per dialect, how many requests hit its endpoint and
// whether the LAST body matched that dialect's own shape (`GET /__mock/state`, `POST
// /__mock/reset`). This module clears the recorder, drives exactly one request through the
// gateway, and reads back which dialect the mock saw and whether the body was that dialect's shape.
//
// Recording is turned on for that one request and off again immediately after: a recorded request
// takes a process-wide lock in the mock, and the mock's own throughput is the reference every
// gateway's number is judged against, so a recording mock left running would understate every
// gateway measured against it.
//
// The verdict is THREE-VALUED, not boolean. `Some(true)` proves translation, `Some(false)` proves
// its absence, and `None` means "not checked" (diagonal cell, mock not recording, mock
// unreachable) - a first-class answer, since collapsing it into `false` would convict a gateway on
// our own configuration.

use crate::cell::CellId;
use crate::http::{self, Outcome};
use crate::ingress::Dialect;
use crate::run::{path_for, RunConfig};
use std::time::Duration;

/// How long the control-plane calls and the single re-verification request may take. Generous,
/// since a timeout here costs an unchecked cell rather than the deadline being tight.
const REVERIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// The verdict, and the evidence behind it. `note` is `None` only for the one case that needs no
/// explanation - a proven translation; every other outcome carries the reason.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reverified {
    pub verified: Option<bool>,
    pub note: Option<String>,
}

impl Reverified {
    /// Not checked, for a stated reason. Never `false`: "we did not ask" and "we asked and it
    /// failed" are different claims and only one is about the gateway.
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
    /// Whether the mock was started with `MOCK_RECORD=1`. Served regardless of the flag so a
    /// caller can tell "recording off" from "no requests arrived" apart.
    pub recording: bool,
    pub dialects: std::collections::BTreeMap<String, DialectState>,
}

/// Parse `/__mock/state`'s document.
///
/// A free function over bytes so the verdict logic can be tested against real and malformed
/// documents with no mock, socket, or gateway involved.
pub fn parse_state(body: &[u8]) -> Result<MockState, String> {
    let v: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| format!("the mock's state document did not parse: {e}"))?;
    // A missing `recording` key is not `false`: that would turn a parse failure of ours into a
    // false configuration claim about the run.
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
                    body_ok: row
                        .get("body_ok")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
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

/// The verdict itself, over an already-read recorder state. Pure, so it's testable without a live
/// gateway/mock pair.
///
/// `ingress` matters even though only `egress` is being proven: landing on the ingress dialect's own
/// endpoint means the gateway forwarded bytes verbatim (the passthrough this guard exists to catch),
/// while landing elsewhere means it translated to the wrong thing.
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
        // Nothing arrived at all: says nothing about the gateway (cached, refused before upstream,
        // or never sent). Publishing `false` here would convict on absent evidence.
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
/// One request, not the load window's thousands: the recorder keeps only the LAST body per dialect,
/// so a window would just capture whichever request landed last, and translation is a property of
/// the code path, not of how many times it ran.
///
/// Run before the metrics windows, since those drive millions of requests through the same recorder
/// and a reset would race an eight-minute memory window.
pub fn reverify_cell(cfg: &RunConfig, id: &CellId, ingress: Dialect) -> Reverified {
    let Ok(egress) = id.egress.parse::<Dialect>() else {
        return Reverified::unchecked(format!(
            "the egress dialect {:?} is not one this build knows, so there is no request shape to prove",
            id.egress
        ));
    };
    // A diagonal cell has nothing to prove: ingress and egress are the same dialect, so forwarding
    // and translating are the same wire, and no mock observation can separate them.
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
    // Fail closed: if the enable's result is unknown, assuming ON risks refuting from an empty
    // record and assuming OFF risks leaving recording enabled for every window after this cell -
    // `unchecked` asserts neither.
    //
    // Failures below are carried as a VALUE, never an early `return`, so the disable at the end of
    // this function always runs - a request that merely timed out must not leave the recorder ON for
    // every window that follows.
    let outcome = match set_recording(cfg, true) {
        Some(why) => Err(Reverified::unchecked(format!(
            "the mock's recorder could not be enabled for this cell, so what the gateway emitted upstream was never observed: {why}"
        ))),
        // A bad answer may still have taken effect, so fall through to the same disable as success.
        None => drive_and_read(cfg, id, ingress),
    };

    // Disable unconditionally before returning: every load window on this cell depends on the mock
    // being quiet by the time it starts.
    let disabled = set_recording(cfg, false);
    // A recorder that won't turn off leaves later windows measuring a slower mock - a rig-state fact
    // worth surfacing beside this cell's (otherwise sound) verdict.
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
/// Separated so the caller's mandatory disable can't be skipped: every failure here returns as
/// `Err(Reverified)` rather than an early `return`, letting the caller carry it past the turn-off.
fn drive_and_read(cfg: &RunConfig, id: &CellId, ingress: Dialect) -> Result<MockState, Reverified> {
    // Must use the same model the probe used (`model_for`, keyed by egress), not `cfg.model`: most
    // gateways route their upstream by model name, so the base model routes every cell to the
    // openai upstream regardless of egress, silently re-verifying the wrong wire.
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

/// Turn the mock's recorder on or off, returning why it could not be done. Both directions are
/// checked: the enable is what the verdict rests on, the disable is what every window after this
/// cell rests on.
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
/// Normally a no-op (the field boots the mock quiet), but guards against a `MOCK_RECORD=1` debug
/// start or a previous run's crash leaving it on, which would taint the box-qualification baseline
/// window taken before the first cell is ever re-verified.
///
/// Best effort: an unresponsive mock is a rig fault reported through per-cell verdicts, not a reason
/// to refuse a run that may still produce honest numbers.
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
        // Our own refusal, not the gateway's: a rig fault that makes the re-verification unusable,
        // never evidence about a gateway.
        Outcome::RigRefused(why) => Some(format!(
            "the rig refused to send this control-plane request: {why}"
        )),
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

    // The defect this module exists for: a verbatim-forwarded request still gets a plausible 200
    // from the mock's canned response, and without this check the cell would publish a translation
    // the gateway never performed.
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
        // Reaching this state means the recording toggle didn't take - a rig fault - so the note
        // must name the recorder, not read as a gateway defect.
        let note = v.note.unwrap_or_default();
        assert!(
            note.contains("recording"),
            "the note must name the recorder as the reason: {note}"
        );
    }

    // Nothing arrived is an absence of evidence, not evidence of absence; must not be `false`.
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

    // The mock defaults `body_ok: false` for every unseen dialect, so `count`, not `body_ok`, is
    // what says whether there's a verdict to read at all.
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
        // Never touches the network: the mock address is a dead port, so a real attempt would show
        // up as a connection failure in the note.
        assert!(!note.contains("connection"), "{note}");
    }

    // Minimal stand-in for the mock's control plane: tracks whether `/__mock/record` last turned
    // recording on or off. Not testing HTTP parsing (that's `http.rs`'s job).
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
                    // Loop for robustness against a split read; one read is enough in practice.
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

    // Pins: if the gateway request fails, the recorder must still be turned back off before
    // returning, or every window after this cell measures against a recording (slower) mock.
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

    /// A fake gateway that records the request body it receives. The mock-side fake above can't see
    /// this: what matters here is the model name on the wire to the gateway.
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

    // Guards against reverting `drive_and_read`'s model construction back to
    // `ingress.body(&cfg.model)`: the test below recomputes `model_for`/`Dialect::body` by hand and
    // never calls `reverify_cell`, so it can't see that regression. This one reads the body directly
    // off the gateway socket to catch it.
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

    // Unreachable mock is a rig fault, not a failed translation - the same inversion `probe.rs`
    // guards against for the served verdict.
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

    // Same invariant as the socket-level test above (drive the cell's own wire), verified directly
    // via `model_for`/`Dialect::body` instead of a live gateway.
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
