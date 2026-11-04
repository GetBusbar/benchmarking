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

// THE SAME RULE PER ROW. A missing or mistyped `body_ok` read as false, and `count > 0` with
// `body_ok == false` is a published refutation - an accusation drawn from our own parse of a mock of
// another build. A mistyped `count` read as 0, "nothing arrived".
#[test]
fn a_row_without_a_readable_count_or_body_ok_is_an_error_not_a_refutation() {
    for doc in [
        br#"{"recording":true,"dialects":{"anthropic":{"count":1,"last_path":"/v1/messages"}}}"#
            .as_slice(),
        br#"{"recording":true,"dialects":{"anthropic":{"count":1,"body_ok":"true"}}}"#.as_slice(),
        br#"{"recording":true,"dialects":{"anthropic":{"body_ok":true}}}"#.as_slice(),
        br#"{"recording":true,"dialects":{"anthropic":{"count":"1","body_ok":true}}}"#.as_slice(),
    ] {
        let err = parse_state(doc).expect_err("an unreadable row must not become a verdict");
        assert!(err.contains("anthropic"), "the error names the row: {err}");
    }
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
    fake_mock_with(recording, Default::default(), false)
}

/// `fake_mock`, also recording whether the recorder was EVER turned on (`ever_on`), and optionally
/// answering 500 to the turn-off (`fail_disable`), the rig fault a verdict must carry.
fn fake_mock_with(
    recording: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ever_on: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fail_disable: bool,
) -> std::net::SocketAddr {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let l = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port to pick one");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let recording = std::sync::Arc::clone(&recording);
            let ever_on = std::sync::Arc::clone(&ever_on);
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
                let mut status = "200 OK";
                let json = if first_line.contains("/__mock/record") {
                    let on = body.contains("\"on\":true");
                    if on {
                        ever_on.store(true, std::sync::atomic::Ordering::SeqCst);
                    } else if fail_disable {
                        status = "500 Internal Server Error";
                    }
                    if on || !fail_disable {
                        recording.store(on, std::sync::atomic::Ordering::SeqCst);
                    }
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
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
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
// to fire on the send in `drive_and_read`), `reverify_cell` used to return early without ever
// reaching its `set_recording(cfg, false)`. The recorder the mock had just been told to turn ON
// was left ON, so every load window the suite ran on this cell (and any cell after it, until some
// later re-verify happened to succeed) was measured against a recording - and therefore slower -
// mock.
#[test]
fn a_failed_gateway_request_must_not_leave_the_mocks_recorder_on() {
    let recording = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ever_on = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mock_addr = fake_mock_with(
        std::sync::Arc::clone(&recording),
        std::sync::Arc::clone(&ever_on),
        false,
    );
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
        ever_on.load(std::sync::atomic::Ordering::SeqCst),
        "the recorder must have been turned ON first, or the OFF below proves nothing"
    );
    assert!(
        !recording.load(std::sync::atomic::Ordering::SeqCst),
        "the mock's recorder must be turned back OFF once the re-verification request to the \
             gateway fails, so every load window that follows measures against a quiet mock; it is \
             still ON"
    );
}

// A RECORDER LEFT ON IS A CLAIM ABOUT EVERY WINDOW THAT FOLLOWS, and stderr is not in the artifact.
// The verdict's note is the one place the cell carries, so the rig fault is written there, beside
// whatever the verdict itself concluded.
#[test]
fn a_recorder_that_will_not_turn_off_is_named_in_the_cells_verdict() {
    let recording = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mock_addr = fake_mock_with(std::sync::Arc::clone(&recording), Default::default(), true);
    let dead_gateway: std::net::SocketAddr = "127.0.0.1:1".parse().expect("literal");
    let cfg = crate::run::test_fixture(dead_gateway, mock_addr);

    let v = reverify_cell(&cfg, &CellId::new("openai", "anthropic"), Dialect::Openai);

    assert_eq!(v.verified, None);
    let note = v.note.unwrap_or_default();
    assert!(
        note.contains("could not be disabled") && note.contains("HTTP 500"),
        "the note must name the recorder left on: {note}"
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

// A control-plane call this host could not even open is a rig fault, and says so.
#[test]
fn a_control_call_lost_to_this_hosts_exhaustion_is_named_as_ours() {
    let why = control_failed(Outcome::RigExhausted(
        "Cannot assign requested address".into(),
    ))
    .expect("an exhausted host is not a usable control call");
    assert!(why.contains("this host"), "{why}");
}
