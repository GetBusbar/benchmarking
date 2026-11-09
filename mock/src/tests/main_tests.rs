use super::{
    body_for, dialect_for, handle, json_escape, models_for, request_shape_ok, send_frame,
    state_json, wants_recording, wants_stream, Arc, Bytes, Frame, RecordFlag, Recorder,
    StreamFrames, ANTHROPIC, DIALECTS, MAX_BODY_BYTES, OPENAI, SSE_SEND_TIMEOUT,
};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// The mock must serve every dialect identically and the matrix's leg-3 body_ok must PROVE the
// gateway spoke that dialect's request shape. These tests pin request_shape_ok per dialect,
// including rejecting an unconverted OpenAI body on the bedrock/cohere legs.

#[test]
fn openai_accepts_messages() {
    assert!(request_shape_ok(
        "openai",
        br#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#
    ));
    assert!(!request_shape_ok(
        "openai",
        br#"{"model":"m","input":"hi"}"#
    ));
}

#[test]
fn bedrock_requires_converse_content_blocks_not_raw_openai() {
    // real Converse egress (the harness's bedrock ingress probe shape): messages with {"text":…}
    // content BLOCKS -> ok
    assert!(request_shape_ok(
        "bedrock",
        br#"{"messages":[{"role":"user","content":[{"text":"hello"}]}]}"#
    ));
    // an inferenceConfig marker also proves Converse
    assert!(request_shape_ok(
            "bedrock",
            br#"{"messages":[{"role":"user","content":[{"text":"x"}]}],"inferenceConfig":{"maxTokens":16}}"#
        ));
    // a RAW OpenAI chat body forwarded to /converse without building the Converse shape:
    // messages present but content is a plain string -> must be REJECTED
    assert!(!request_shape_ok(
        "bedrock",
        br#"{"model":"m","messages":[{"role":"user","content":"hello"}]}"#
    ));
}

#[test]
fn cohere_accepts_v2_and_v1_shapes() {
    assert!(request_shape_ok(
        "cohere",
        br#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#
    )); // v2 chat
    assert!(request_shape_ok(
        "cohere",
        br#"{"message":"hi","chat_history":[]}"#
    )); // v1 chat
    assert!(!request_shape_ok("cohere", br#"{"input":"hi"}"#));
}

#[test]
fn anthropic_requires_messages_and_max_tokens() {
    assert!(request_shape_ok(
        "anthropic",
        br#"{"model":"m","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#
    ));
    assert!(!request_shape_ok(
        "anthropic",
        br#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#
    ));
}

#[test]
fn responses_and_gemini_markers() {
    assert!(request_shape_ok("openai-responses", br#"{"input":"hi"}"#));
    assert!(request_shape_ok(
        "openai-responses",
        br#"{"instructions":"be brief"}"#
    ));
    assert!(request_shape_ok(
        "gemini",
        br#"{"contents":[{"parts":[{"text":"hi"}]}]}"#
    ));
    assert!(!request_shape_ok("gemini", br#"{"messages":[]}"#));
}

#[test]
fn unknown_dialect_never_ok() {
    assert!(!request_shape_ok("other", br#"{"messages":[]}"#));
}

// ── response-body routing ───────────────────────────────────────────────────────────────────
//
// Every provider base URL points at this one mock, so the PATH is the only thing that says which
// dialect a gateway forwarded in. Answering the wrong dialect's JSON is not a visible failure:
// the gateway gets a 200 and either forwards a body its client cannot parse, or errors on the
// parse and the cell is published as a gateway defect that is really ours.

#[test]
fn every_dialect_endpoint_is_answered_in_its_own_shape() {
    // Markers unique to each dialect's response body, so this checks the ROUTING and not just
    // that some JSON came back.
    for (path, marker) in [
        ("/v1/chat/completions", "chat.completion"),
        ("/v1/responses", "\"output\":["),
        ("/v1/messages", "\"stop_reason\":\"end_turn\""),
        ("/v1beta/models/m:generateContent", "candidates"),
        ("/model/m/converse", "stopReason"),
        ("/v2/chat", "\"finish_reason\":\"COMPLETE\""),
    ] {
        let body = String::from_utf8_lossy(body_for(path)).into_owned();
        assert!(
            body.contains(marker),
            "{path} must answer its own dialect, got {body}"
        );
    }
}

// An UNRECOGNIZED path still gets a valid answer rather than a broken one, because a gateway
// whose egress path we did not anticipate must not be published as failing against the mock.
// The safe default is the openai body, which is why dialect_for reports "other" instead: the
// matrix runner must not read this fallback as proof the gateway posted openai.
#[test]
fn an_unrecognized_path_falls_back_to_a_valid_body_that_is_not_recorded_as_openai() {
    assert!(String::from_utf8_lossy(body_for("/totally/unknown")).contains("chat.completion"));
    assert_eq!(dialect_for("/totally/unknown"), "other");
}

// A .../models request must be answered BEFORE the chat routing, and per provider: a shared
// catalog listing every provider's models under one base would let a gateway that routes by bare
// model name register the same name under several providers and silently measure the wrong pairing.
#[test]
fn a_model_list_is_provider_specific_and_never_advertises_another_provider() {
    for (path, mine, theirs) in [
        ("/v1/models", "gpt-4o-mini", "claude"),
        ("/anthropic/v1/models", "claude-3-5-sonnet", "gpt-4o"),
        ("/v1beta/models", "gemini-1.5-pro", "gpt-4o"),
        ("/v2/models", "command-r", "claude"),
    ] {
        let body = String::from_utf8_lossy(models_for(path)).into_owned();
        assert!(
            body.contains(mine),
            "{path} must advertise its own models, got {body}"
        );
        assert!(
            !body.contains(theirs),
            "{path} must not advertise another provider's models, got {body}"
        );
        // And the chat router must hand a models path to the model list, not to a chat body.
        assert_eq!(
            body_for(path),
            models_for(path),
            "{path} must route to the model list"
        );
    }
    // The query-string form, which is how some clients ask.
    assert!(String::from_utf8_lossy(body_for("/v1/models?limit=100")).contains("gpt-4o-mini"));
}

// ── stream detection ────────────────────────────────────────────────────────────────────────

// The whole streaming suite hangs off this one substring scan. A false negative answers plain
// JSON to a request that asked for a stream, and the gateway's streaming lane then measures a
// non-stream; a false positive opens an SSE stream to a client waiting for a JSON document,
// which reads as a gateway timeout. Both directions publish a wrong number rather than an error.
#[test]
fn a_stream_request_is_detected_with_or_without_the_space_after_the_colon() {
    assert!(wants_stream(
        br#"{"model":"m","stream":true,"messages":[]}"#
    ));
    assert!(wants_stream(
        br#"{"model":"m","stream": true,"messages":[]}"#
    ));
}

#[test]
fn a_non_stream_request_is_never_mistaken_for_a_stream() {
    assert!(!wants_stream(
        br#"{"model":"m","stream":false,"messages":[]}"#
    ));
    assert!(!wants_stream(br#"{"model":"m","messages":[]}"#));
    assert!(!wants_stream(
        br#"{"stream_options":{"include_usage":true}}"#
    ));
    // Shorter than the marker itself: the windowed scan must not panic on a tiny body.
    assert!(!wants_stream(b""));
    assert!(!wants_stream(b"{}"));
}

// Only the openai and anthropic paths synthesise a stream; the others answer their normal JSON
// even when asked to stream, and the harness records that as untestable rather than measuring
// it. The handler decides by asking whether the body it picked is the openai or the anthropic
// one, so which paths select those two bodies is the whole of the streaming routing, and it is
// pinned here where it can be checked without standing up a server.
#[test]
fn exactly_the_openai_and_anthropic_paths_select_a_streamable_body() {
    assert_eq!(body_for("/v1/chat/completions"), OPENAI);
    assert_eq!(body_for("/v1/messages"), ANTHROPIC);
    for path in [
        "/v1beta/models/m:generateContent",
        "/model/m/converse",
        "/v2/chat",
        "/v1/responses",
    ] {
        assert_ne!(
            body_for(path),
            OPENAI,
            "{path} must not be answered with a streamable body"
        );
        assert_ne!(
            body_for(path),
            ANTHROPIC,
            "{path} must not be answered with a streamable body"
        );
    }
}

// ── stream frames ───────────────────────────────────────────────────────────────────────────

// NO TWO CONSECUTIVE CONTENT FRAMES MAY BE BYTE-IDENTICAL. A gateway with a repetition or loop
// guard aborts a stream whose chunks repeat, so a single reused delta would fail that gateway on
// a property of our synthetic tokens rather than of its behaviour, and every frame must still be
// the same SIZE or the per-frame timings stop being comparable between gateways.
#[test]
fn consecutive_stream_deltas_differ_in_content_but_not_in_size() {
    let f = StreamFrames::build(8, 0, 16);
    for deltas in [&f.openai_deltas, &f.anthropic_deltas] {
        assert_eq!(deltas.len(), 8, "one prebuilt frame per chunk index");
        for w in deltas.windows(2) {
            assert_ne!(
                w[0], w[1],
                "consecutive deltas must differ or a repeat-guard trips"
            );
            assert_eq!(w[0].len(), w[1].len(), "every delta must be the same size");
        }
    }
}

// A caller can ask for a payload narrower than the frame index's own decimal width. Truncating
// rather than overflowing keeps every frame the requested size; a subtraction on the untruncated
// path would underflow instead.
#[test]
fn a_chunk_payload_narrower_than_the_frame_index_is_truncated_not_overflowed() {
    let f = StreamFrames::build(200, 0, 1);
    let sizes: std::collections::BTreeSet<usize> =
        f.openai_deltas.iter().map(|d| d.len()).collect();
    assert_eq!(
        sizes.len(),
        1,
        "a one byte payload must give every frame the same size, got {sizes:?}"
    );
    // AND STILL DISTINCT FROM ITS NEIGHBOUR. Truncation kept the LEADING digits, so frames 10..19
    // were all "1" at width 1 and a gateway's repeat-guard saw identical consecutive tokens.
    for deltas in [&f.openai_deltas, &f.anthropic_deltas] {
        for w in deltas.windows(2) {
            assert_ne!(w[0], w[1], "consecutive deltas must differ even at width 1");
        }
    }
}

// A zero-chunk stream must still build: `chunks.max(1)` keeps the delta vector non-empty, and
// the handler indexes it modulo its length, so an empty vector would divide by zero and panic
// the whole mock mid-run.
#[test]
fn a_zero_chunk_stream_still_builds_a_non_empty_delta_vector() {
    let f = StreamFrames::build(0, 0, 16);
    assert!(!f.openai_deltas.is_empty());
    assert!(!f.anthropic_deltas.is_empty());
}

// The frames the harness's SSE reader keys off: the openai stream terminates with [DONE] and the
// anthropic one with message_stop. A stream that never terminates leaves the reader waiting out
// its deadline on every streaming cell.
#[test]
fn each_stream_dialect_ends_with_the_terminator_its_clients_wait_for() {
    let f = StreamFrames::build(4, 0, 16);
    let tail = |v: &Vec<Bytes>| {
        v.iter()
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect::<String>()
    };
    assert!(tail(&f.openai_tail).contains("data: [DONE]"));
    assert!(tail(&f.anthropic_tail).contains("message_stop"));
    assert!(tail(&f.openai_head).contains("\"role\":\"assistant\""));
    assert!(tail(&f.anthropic_head).contains("message_start"));
}

// ── the recorded state document ─────────────────────────────────────────────────────────────

// The state document is hand-assembled with format!, so a body snippet carrying a quote, a
// backslash or a newline (every request body has quotes) would produce INVALID JSON and the
// matrix runner would read a parse error as "the request never arrived" rather than as a bug in
// this escaper. That inverts a proof of a working round trip into evidence of a broken one.
#[test]
fn a_body_snippet_full_of_json_metacharacters_still_escapes_to_valid_json() {
    let nasty = "{\"a\":\"b\\c\"}\n\r\t\u{1}";
    let escaped = json_escape(nasty);
    let doc = format!("{{\"s\":\"{escaped}\"}}");
    let parsed: serde_json::Value = match serde_json::from_str(&doc) {
        Ok(v) => v,
        Err(e) => panic!("the escaped snippet must be valid JSON: {e} in {doc}"),
    };
    assert_eq!(
        parsed["s"],
        serde_json::Value::String(nasty.to_string()),
        "escaping must round trip exactly"
    );
}

// Every dialect is present in the document whether or not it was hit, so a runner can tell "no
// request arrived on this dialect" (count 0) apart from "this dialect is not a thing the mock
// knows about" (key missing). And `body_ok` on an untouched dialect must be false, never a
// default that reads as a passed shape check.
#[test]
fn the_state_document_lists_every_dialect_even_untouched_ones() {
    let rec: Recorder = Recorder::default();
    let doc = state_json(&rec, true);
    let parsed: serde_json::Value = match serde_json::from_str(&doc) {
        Ok(v) => v,
        Err(e) => panic!("the state document must be valid JSON: {e} in {doc}"),
    };
    assert_eq!(parsed["recording"], serde_json::Value::Bool(true));
    for d in DIALECTS {
        assert_eq!(
            parsed["dialects"][d]["count"], 0,
            "{d} must be present with a zero count"
        );
        assert_eq!(
            parsed["dialects"][d]["body_ok"], false,
            "{d} must not claim a passed shape check"
        );
    }
}

// The `recording` flag is what lets a runner tell a mock that was never asked to record apart
// from one that recorded nothing. Collapsing the two turns a misconfigured run into what looks
// like a gateway that never forwarded a request.
#[test]
fn the_state_document_reports_whether_it_was_recording_at_all() {
    let rec: Recorder = Recorder::default();
    assert!(state_json(&rec, false).contains("\"recording\":false"));
    assert!(state_json(&rec, true).contains("\"recording\":true"));
}

// A recorded dialect must carry its count, its shape verdict and the evidence for both. The
// snippet and path are what let an operator see WHY body_ok came out false, and without them a
// failed leg-3 check is an assertion with nothing behind it.
#[test]
fn a_recorded_dialect_carries_its_count_and_the_evidence_for_its_verdict() {
    let rec: Recorder = Recorder::default();
    {
        let mut map = match rec.lock() {
            Ok(m) => m,
            Err(e) => panic!("fresh mutex must lock: {e}"),
        };
        let r = map.entry("anthropic").or_default();
        r.count = 3;
        r.body_ok = request_shape_ok("anthropic", br#"{"max_tokens":16,"messages":[]}"#);
        r.last_path = "/v1/messages".to_string();
        r.last_snippet = "{\"messages\":[]}".to_string();
    }
    let parsed: serde_json::Value = match serde_json::from_str(&state_json(&rec, true)) {
        Ok(v) => v,
        Err(e) => panic!("the state document must be valid JSON: {e}"),
    };
    assert_eq!(parsed["dialects"]["anthropic"]["count"], 3);
    assert_eq!(parsed["dialects"]["anthropic"]["body_ok"], true);
    assert_eq!(parsed["dialects"]["anthropic"]["last_path"], "/v1/messages");
    assert_eq!(
        parsed["dialects"]["anthropic"]["last_snippet"],
        "{\"messages\":[]}"
    );
    // An untouched dialect in the same document is unaffected.
    assert_eq!(parsed["dialects"]["openai"]["count"], 0);
}

// ── the recording toggle ────────────────────────────────────────────────────────────────────

// The toggle exists so the harness's LOAD WINDOWS never pay for recording. This mock's own
// throughput is the reference every gateway's number is judged against, so a slower mock
// suppresses real gateway measurements as mock-bound - honestly, and therefore invisibly.
#[test]
fn the_record_control_reads_on_and_off_in_both_json_spacings() {
    assert_eq!(wants_recording(br#"{"on":true}"#), Some(true));
    assert_eq!(wants_recording(br#"{"on": true}"#), Some(true));
    assert_eq!(wants_recording(br#"{"on":false}"#), Some(false));
    assert_eq!(wants_recording(br#"{"on": false}"#), Some(false));
}

// A BODY THAT SAYS NEITHER IS REFUSED, never defaulted. A caller that believes it enabled the
// recorder and did not would read the resulting empty record as a gateway failing to translate,
// which is a false accusation produced entirely by a defaulted control message.
#[test]
fn a_record_control_body_that_says_neither_is_refused_rather_than_defaulted() {
    for body in [
        &b""[..],
        b"{}",
        b"true",
        b"not json",
        br#"{"recording":true}"#,
        br#"{"on":"yes"}"#,
    ] {
        assert_eq!(
            wants_recording(body),
            None,
            "{:?} must be refused",
            String::from_utf8_lossy(body)
        );
    }
    // Contradictory is also refused: there is no way to know which the caller meant.
    assert_eq!(
        wants_recording(br#"{"on":true,"off":true,"on":false}"#),
        None
    );
}

// The whole point of the toggle, over a real connection: recording starts off, `/__mock/record`
// turns it on, `/__mock/state` reports it, and a request that arrives while recording is OFF is
// not recorded at all. That last clause is what guarantees a load window costs nothing - it is
// the difference between "the guard is implemented" and "the guard did not cost us the
// measurements it was meant to protect".
#[tokio::test]
async fn a_request_that_arrives_while_recording_is_off_is_never_recorded() {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) => panic!("loopback bind must succeed in a test sandbox: {e}"),
    };
    let addr = match listener.local_addr() {
        Ok(a) => a,
        Err(e) => panic!("bound listener must report its local addr: {e}"),
    };
    let frames = Arc::new(StreamFrames::build(1, 0, 1));
    let recorder: Arc<Recorder> = Arc::new(Recorder::default());
    // Starts OFF, which is what a box that never exported MOCK_RECORD looks like.
    let recording: Arc<RecordFlag> = Arc::new(RecordFlag::new(false));
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let io = TokioIo::new(stream);
            let frames = frames.clone();
            let recorder = recorder.clone();
            let recording = recording.clone();
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        io,
                        service_fn(move |r| {
                            handle(r, 0, frames.clone(), recorder.clone(), recording.clone())
                        }),
                    )
                    .await;
            });
        }
    });

    async fn call(addr: std::net::SocketAddr, method: &str, path: &str, body: &str) -> String {
        let mut c = match TcpStream::connect(addr).await {
            Ok(c) => c,
            Err(e) => panic!("test client connect must succeed: {e}"),
        };
        let req = format!(
                "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        if let Err(e) = c.write_all(req.as_bytes()).await {
            panic!("writing the request must succeed: {e}");
        }
        let mut resp = Vec::new();
        if let Err(e) = c.read_to_end(&mut resp).await {
            panic!("reading the response must succeed: {e}");
        }
        String::from_utf8_lossy(&resp).into_owned()
    }

    // A load window's worth of traffic while recording is off leaves the recorder untouched.
    let chat = r#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#;
    for _ in 0..3 {
        let r = call(addr, "POST", "/v1/chat/completions", chat).await;
        assert!(
            r.contains("chat.completion"),
            "the mock must still answer normally: {r}"
        );
    }
    let state = call(addr, "GET", "/__mock/state", "").await;
    assert!(state.contains("\"recording\":false"), "{state}");
    assert!(
        state.contains("\"openai\":{\"count\":0"),
        "a request that arrived while recording was off must not be recorded: {state}"
    );

    // Turn it on, drive ONE request, and the record appears with its evidence.
    let on = call(addr, "POST", "/__mock/record", r#"{"on":true}"#).await;
    assert!(on.contains("\"recording\":true"), "{on}");
    let _ = call(addr, "POST", "/v1/chat/completions", chat).await;
    let state = call(addr, "GET", "/__mock/state", "").await;
    assert!(state.contains("\"recording\":true"), "{state}");
    assert!(
        state.contains("\"openai\":{\"count\":1,\"body_ok\":true"),
        "{state}"
    );
    assert!(
        state.contains("/v1/chat/completions"),
        "the evidence must travel: {state}"
    );

    // And off again: the count stops moving, so the windows that follow pay nothing.
    let off = call(addr, "POST", "/__mock/record", r#"{"on":false}"#).await;
    assert!(off.contains("\"recording\":false"), "{off}");
    let _ = call(addr, "POST", "/v1/chat/completions", chat).await;
    let state = call(addr, "GET", "/__mock/state", "").await;
    assert!(
        state.contains("\"openai\":{\"count\":1,"),
        "the count must not have moved: {state}"
    );

    // A control body that says neither is refused, and leaves the flag where it was.
    let bad = call(addr, "POST", "/__mock/record", "{}").await;
    assert!(
        bad.contains("400"),
        "an unparseable control body must be refused: {bad}"
    );
    let state = call(addr, "GET", "/__mock/state", "").await;
    assert!(
        state.contains("\"recording\":false"),
        "a refused control must not flip the flag: {state}"
    );
}

#[test]
fn dialect_routing_is_unambiguous() {
    assert_eq!(dialect_for("/v1/chat/completions"), "openai");
    assert_eq!(dialect_for("/v1/responses"), "openai-responses");
    assert_eq!(dialect_for("/v1/messages"), "anthropic");
    assert_eq!(dialect_for("/model/m/converse"), "bedrock");
    assert_eq!(dialect_for("/v2/chat"), "cohere");
    assert_eq!(dialect_for("/v1beta/models/m:generateContent"), "gemini");
    assert_eq!(dialect_for("/something/else"), "other");
    // A model discovery call is not a request on a dialect's endpoint: recorded under gemini it
    // bumped that count and overwrote body_ok with false (an empty GET has no request shape).
    assert_eq!(dialect_for("/v1beta/models"), "other");
    assert_eq!(dialect_for("/v1/models"), "other");
    // Gemini's streaming method has a capital G, and a v1 path has no /v1beta/ marker.
    assert_eq!(
        dialect_for("/v1/projects/p/locations/l/models/m:streamGenerateContent"),
        "gemini"
    );
    assert_eq!(
        body_for("/v1/projects/p/locations/l/models/m:streamGenerateContent"),
        super::GEMINI
    );
}

// ── request body cap ────────────────────────────────────────────────────────────────────────

/// Serve ONE connection with the real `handle()` on loopback and return its address.
async fn serve_handle_once(frames: StreamFrames) -> std::net::SocketAddr {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(e) => panic!("loopback bind must succeed in a test sandbox: {e}"),
    };
    let addr = match listener.local_addr() {
        Ok(a) => a,
        Err(e) => panic!("bound listener must report its local addr: {e}"),
    };
    let frames = Arc::new(frames);
    let recorder: Arc<Recorder> = Arc::new(Recorder::default());
    let recording: Arc<RecordFlag> = Arc::new(RecordFlag::new(false));
    tokio::spawn(async move {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => panic!("test server accept must succeed: {e}"),
        };
        let io = TokioIo::new(stream);
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(
                io,
                service_fn(move |r| {
                    handle(r, 0, frames.clone(), recorder.clone(), recording.clone())
                }),
            )
            .await;
    });
    addr
}

/// POST `body` to `path` with Connection: close and return everything read back. The body is
/// written CONCURRENTLY with the read, and write errors are ignored: a server that answers before
/// the body is fully sent (the 413 case) may close with input unread, and a sequential
/// write-then-read lost that race to EPIPE/ECONNRESET depending on socket buffering.
async fn post_and_read(addr: std::net::SocketAddr, path: &str, body: Vec<u8>) -> Vec<u8> {
    let client = match TcpStream::connect(addr).await {
        Ok(c) => c,
        Err(e) => panic!("test client connect must succeed: {e}"),
    };
    let (mut rd, mut wr) = client.into_split();
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let writer = tokio::spawn(async move {
        let _ = wr.write_all(head.as_bytes()).await;
        let _ = wr.write_all(&body).await;
        wr
    });
    let mut resp = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match rd.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => resp.extend_from_slice(&buf[..n]),
            // A reset after the response arrived is the server closing with our body unread.
            Err(_) if !resp.is_empty() => break,
            Err(e) => panic!("reading the response must succeed: {e}"),
        }
    }
    drop(writer);
    resp
}

// A gateway (or a bug in one) sending a body past MAX_BODY_BYTES must get a real 413, not have
// this process read the whole thing into memory unbounded. Drives handle() over a real loopback
// connection - the cap lives in how handle() wraps the incoming body, so a test against Limited
// in isolation would not prove the wrapping actually happened.
#[tokio::test]
async fn a_request_body_over_the_cap_gets_413_instead_of_being_read_unbounded() {
    let addr = serve_handle_once(StreamFrames::build(1, 0, 1)).await;
    let resp = post_and_read(
        addr,
        "/v1/chat/completions",
        vec![b'a'; MAX_BODY_BYTES + 1024],
    )
    .await;
    let status_line = String::from_utf8_lossy(&resp[..resp.len().min(32)]).into_owned();
    assert!(
        status_line.contains("413"),
        "a body over the cap must get 413, got: {status_line}"
    );
}

// The boundary through the REAL wrapping in handle(): a body at exactly the cap is served, so the
// cap does not falsely reject a request the benchmark itself sends. (This used to test
// `http_body_util::Limited` on a `Full` body, which proved the library and not the mock.)
#[tokio::test]
async fn a_body_at_exactly_the_cap_is_still_accepted() {
    let addr = serve_handle_once(StreamFrames::build(1, 0, 1)).await;
    let resp = post_and_read(addr, "/v1/chat/completions", vec![b' '; MAX_BODY_BYTES]).await;
    let status_line = String::from_utf8_lossy(&resp[..resp.len().min(32)]).into_owned();
    assert!(
        status_line.contains("200"),
        "a body at exactly the cap must be served, got: {status_line}"
    );
}

/* THE WIRE STREAM, NOT THE PREBUILT VECTORS. The engine's ingress taxonomy counts on an openai
stream being N+3 events (role head, N deltas, finish, [DONE]) and an anthropic one N+5
(message_start, content_block_start, N deltas, content_block_stop, message_delta, message_stop),
in that order. Only `StreamFrames::build` was tested, so `sse_response`'s ordering and count -
and handle()'s routing of stream:true onto it - were unpinned. */
#[tokio::test]
async fn a_stream_request_gets_the_frames_in_order_on_the_wire() {
    const N: usize = 3;
    for (path, body, events, first, last) in [
        (
            "/v1/chat/completions",
            r#"{"model":"m","stream":true,"messages":[]}"#,
            N + 3,
            "\"role\":\"assistant\"",
            "[DONE]",
        ),
        (
            "/v1/messages",
            r#"{"model":"m","stream":true,"max_tokens":1,"messages":[]}"#,
            N + 5,
            "message_start",
            "message_stop",
        ),
    ] {
        let addr = serve_handle_once(StreamFrames::build(N as u32, 0, 4)).await;
        let resp = post_and_read(addr, path, body.as_bytes().to_vec()).await;
        let text = String::from_utf8_lossy(&resp).into_owned();
        assert!(
            text.to_ascii_lowercase()
                .contains("content-type: text/event-stream"),
            "{path}: {text}"
        );
        let data: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .collect();
        assert_eq!(data.len(), events, "{path}: {data:?}");
        assert!(data[0].contains(first), "{path}: {data:?}");
        assert!(data[events - 1].contains(last), "{path}: {data:?}");
        let deltas = data.iter().filter(|d| d.contains("delta")).count();
        assert!(
            deltas >= N,
            "{path}: {N} content deltas expected, got {data:?}"
        );
    }
}

// ── accept loop connection cap ──────────────────────────────────────────────────────────────

// main()'s accept loop backpressures on this semaphore rather than accumulating unbounded
// in-flight connections. This pins only the tokio PRIMITIVE the loop relies on (once every permit is
// held, no further permit is available until one is released), not main()'s wiring of it.
#[test]
fn the_connection_semaphore_backpressures_once_at_capacity() {
    let sem = tokio::sync::Semaphore::new(2);
    let p1 = match sem.try_acquire() {
        Ok(p) => p,
        Err(e) => panic!("first permit must be available: {e}"),
    };
    let _p2 = match sem.try_acquire() {
        Ok(p) => p,
        Err(e) => panic!("second permit must be available: {e}"),
    };
    assert!(
        sem.try_acquire().is_err(),
        "a third permit must not be available once the cap is reached"
    );
    drop(p1);
    assert!(
        sem.try_acquire().is_ok(),
        "releasing a permit must free capacity for the next connection"
    );
}

// ── SSE writer send timeout ─────────────────────────────────────────────────────────────────

// A peer that stalls the TCP connection without closing it never drains the mpsc receiver, so
// an unbounded tx.send().await would leak the writer task for the rest of the run. Fills the
// one-slot channel and never drains it - the receiver is kept alive (not dropped) so this proves
// the timeout branch, not the pre-existing closed-channel branch.
#[tokio::test]
async fn send_frame_gives_up_on_a_stalled_but_open_receiver_instead_of_blocking_forever() {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, Infallible>>(1);
    if let Err(e) = tx.try_send(Ok(Frame::data(Bytes::from_static(b"first")))) {
        panic!("filling the one-slot buffer must succeed: {e}");
    }
    let start = tokio::time::Instant::now();
    // Bounded well above SSE_SEND_TIMEOUT so a regression back to an unbounded .send().await
    // fails this test on the assertion below rather than hanging the whole suite.
    let sent = tokio::time::timeout(
        SSE_SEND_TIMEOUT * 20,
        send_frame(&tx, Bytes::from_static(b"second")),
    )
    .await;
    let elapsed = start.elapsed();
    assert_eq!(
        sent,
        Ok(false),
        "a send into a full channel with a stalled receiver must give up, not hang"
    );
    assert!(
        elapsed >= SSE_SEND_TIMEOUT,
        "must wait out SSE_SEND_TIMEOUT before giving up, elapsed {elapsed:?}"
    );
    drop(rx);
}

#[test]
fn cohere_response_matches_the_requested_api_version() {
    let v1: serde_json::Value = serde_json::from_slice(body_for("/v1/chat")).unwrap();
    let v2: serde_json::Value = serde_json::from_slice(body_for("/v2/chat")).unwrap();
    assert_eq!(v1["text"], "ok");
    assert_eq!(v1["meta"]["tokens"]["input_tokens"], 10);
    assert!(v1.get("message").is_none());
    assert_eq!(v2["message"]["content"][0]["text"], "ok");
    assert_eq!(v2["usage"]["tokens"]["input_tokens"], 10);
    assert!(v2.get("text").is_none());
    assert_eq!(body_for("/v1/chat/completions"), OPENAI);
}

#[test]
fn bedrock_request_validation_ignores_json_whitespace() {
    assert!(request_shape_ok(
        "bedrock",
        br#"{ "messages": [ { "role": "user", "content": [ { "text": "hello" } ] } ] }"#
    ));
    assert!(!request_shape_ok(
        "bedrock",
        br#"{"messages":[{"content":"hello"}],"inferenceConfig":{"maxTokens":16}}"#
    ));
}

#[test]
fn responses_usage_includes_required_token_details() {
    let body: serde_json::Value = serde_json::from_slice(body_for("/v1/responses")).unwrap();
    assert_eq!(body["usage"]["input_tokens_details"]["cached_tokens"], 0);
    assert_eq!(
        body["usage"]["input_tokens_details"]["cache_write_tokens"],
        0
    );
    assert_eq!(
        body["usage"]["output_tokens_details"]["reasoning_tokens"],
        0
    );
    assert_eq!(body["usage"]["total_tokens"], 12);
}

#[test]
fn responses_includes_standard_response_settings() {
    let body: serde_json::Value = serde_json::from_slice(body_for("/v1/responses")).unwrap();
    assert!(body["parallel_tool_calls"].is_boolean());
    assert!(body["tools"].is_array());
    assert!(body["tool_choice"].is_string());
    assert!(body["temperature"].is_number());
    assert!(body["top_p"].is_number());
    assert!(body["metadata"].is_object());
    assert!(body["error"].is_null());
}
