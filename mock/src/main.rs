// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Deterministic, blazing-fast mock upstream for the gateway benchmark. Answers a 200 with a valid,
// minimal response body for EVERY wire protocol a gateway might forward in - chosen by request path -
// so any gateway works against it regardless of which provider API it speaks upstream:
//
//   /chat/completions      -> OpenAI chat.completion
//   /responses             -> OpenAI Responses
//   /messages              -> Anthropic Messages
//   …:generateContent      -> Google Gemini
//   /converse | /model/…   -> AWS Bedrock (Converse)
//   /v2/chat | /v1/chat    -> Cohere
//   (anything else)        -> OpenAI chat.completion (safe default)
//
// It is deliberately dumb and deliberately fast: hyper on a multi-threaded tokio runtime, static
// response bytes, the request body drained but never processed. A throughput benchmark must find the
// GATEWAY's ceiling, so the mock must never be the ceiling - this sustains 100s of k RPS, and the
// harness records the mock's own ceiling each run so mock-boundedness can't hide.
//
//   mock -port 8000                    # instant responses
//   MOCK_TTFT_MS=20 mock -port 8000    # add a fixed delay (latency-isolation runs)
//
// RECORDING (matrix suite): the mock can record, per protocol dialect, how many requests arrived on
// that dialect's endpoint and whether the LAST request body looked like that dialect's request shape
// (loose marker check). GET /__mock/state returns the record as JSON; POST /__mock/reset zeroes it.
// This lets the matrix runner prove a request actually round-tripped through the gateway to the
// intended egress dialect.
//
// RECORDING IS RUNTIME-TOGGLABLE, AND THAT IS A MEASUREMENT-INTEGRITY DECISION, not a convenience.
// MOCK_RECORD=1 sets the STARTING state; POST /__mock/record with `{"on":true|false}` changes it
// while the mock runs, and GET /__mock/state reports it.
//
// The reason is what recording costs and what that cost would do to the published board. This mock's
// own throughput is the reference every gateway's number is judged against, so anything that slows the
// mock down understates EVERY gateway measured against it - and it does so consistently, which is what
// makes it dangerous: all the numbers stay internally plausible while all of them move together.
//
// This paragraph used to say a result within 10% of the mock's ceiling was "suppressed as mock-bound"
// via `is_rig_bound`. That suppression mechanism and that function were both deleted (see
// `rigbound.rs`'s own header); nothing is suppressed today. It was the third copy of the same stale
// claim - `run.rs` and `reverify.rs` carried the other two - which is what a fact repeated in three
// comments instead of stated in one place does when the code beneath it changes.
//
// A recorded request takes a process-wide lock, and the harness
// needs the record for exactly ONE request per cell while it drives millions through the same
// process for the throughput and memory windows.
//
// With the toggle the harness turns recording on around its one re-verification request and off for
// every load window, so every published number is taken against the same mock behaviour the perf
// suites have always run against - INCLUDING the mock's own reference ceiling, which is measured
// with this same process and would otherwise be a different instrument from the one the gateway was
// measured against.
//
// The unrecorded path is still one branch on an atomic load, and the recorded path now does its
// shape check and its allocations OUTSIDE the critical section, so the lock is held for a handful of
// moves rather than for a substring scan over the body plus two heap allocations.
//
// STREAMING: when (and only when) the request body says "stream":true, the OpenAI and Anthropic
// paths answer a valid SSE stream instead - role/message_start, then N content deltas paced at a
// fixed interval, then finish + [DONE] (message_stop for Anthropic). The pacing is the "model
// generating tokens"; the stream suite measures what a gateway ADDS on top of it. Knobs:
//   MOCK_STREAM_CHUNKS=64        content-delta frames per stream
//   MOCK_STREAM_INTERVAL_MS=20   pause before each content delta after the first
//   MOCK_STREAM_CHUNK_BYTES=16   text payload bytes per content delta
// Other protocols (OpenAI Responses, Gemini, Bedrock, Cohere) ignore stream:true and answer their
// normal JSON.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;
use tokio_stream::wrappers::ReceiverStream;

// Streaming dispatch compares these two response allocations by identity. Static storage keeps
// that identity stable across codegen units; const values may be instantiated separately.
static OPENAI: &[u8] = br#"{"id":"chatcmpl-x","object":"chat.completion","created":1,"model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}"#;
const RESPONSES: &[u8] = br#"{"id":"resp_x","object":"response","created_at":1,"status":"completed","model":"mock","output":[{"type":"message","id":"msg_x","status":"completed","role":"assistant","content":[{"type":"output_text","text":"ok","annotations":[]}]}],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}"#;
static ANTHROPIC: &[u8] = br#"{"id":"msg_x","type":"message","role":"assistant","model":"mock","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2}}"#;
const GEMINI: &[u8] = br#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2,"totalTokenCount":12}}"#;
const BEDROCK: &[u8] = br#"{"output":{"message":{"role":"assistant","content":[{"text":"ok"}]}},"stopReason":"end_turn","usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}"#;
const COHERE: &[u8] = br#"{"id":"x","finish_reason":"COMPLETE","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]},"usage":{"tokens":{"input_tokens":10,"output_tokens":2}}}"#;
// Cohere v1 adapters read top-level text and meta.tokens, not the v2 message envelope.
const COHERE_V1: &[u8] = br#"{"response_id":"x","generation_id":"x","text":"ok","finish_reason":"COMPLETE","meta":{"api_version":{"version":"1"},"tokens":{"input_tokens":10,"output_tokens":2},"billed_units":{"input_tokens":10,"output_tokens":2}}}"#;
// GET .../models - a model list, discovered at boot by gateways that register routable models by
// calling the upstream's /models. PROVIDER-SPECIFIC (keyed off the request path, see models_for): a
// catalog shared across providers would let a gateway that routes by bare model name register the
// same name under multiple providers and pick one arbitrarily, so each base advertises only its own.
const MODELS_OPENAI: &[u8] = br#"{"object":"list","data":[{"id":"gpt-4o-mini","object":"model","created":1,"owned_by":"openai"},{"id":"gpt-4o","object":"model","created":1,"owned_by":"openai"},{"id":"gpt-3.5-turbo","object":"model","created":1,"owned_by":"openai"}]}"#;
const MODELS_ANTHROPIC: &[u8] = br#"{"data":[{"id":"claude-3-5-sonnet","type":"model","display_name":"Claude 3.5 Sonnet"},{"id":"claude-3-5-haiku","type":"model","display_name":"Claude 3.5 Haiku"}],"has_more":false}"#;
const MODELS_GEMINI: &[u8] = br#"{"models":[{"name":"models/gemini-1.5-pro","displayName":"Gemini 1.5 Pro"},{"name":"models/gemini-1.5-flash","displayName":"Gemini 1.5 Flash"}]}"#;
const MODELS_COHERE: &[u8] = br#"{"models":[{"name":"command-r","endpoints":["chat"]},{"name":"command-r-plus","endpoints":["chat"]}]}"#;

/// A model-list request (`GET .../models`). ONE predicate for `body_for` and `dialect_for`: a model
/// discovery call is not a request on a dialect's chat endpoint and must never be recorded as one.
/// (`req.uri().path()` carries no query string; the `?` form is kept for callers passing a full
/// request target.)
fn is_model_list(path: &str) -> bool {
    path.ends_with("/models") || path.contains("/models?")
}

/// A Gemini path. Case matters in the method name: `:generateContent` and
/// `:streamGenerateContent` (capital G) are both Gemini, and a v1 (Vertex-style) path carries no
/// `/v1beta/` marker to fall back on.
fn is_gemini(path: &str) -> bool {
    path.contains("generateContent")
        || path.contains("GenerateContent")
        || path.contains("/v1beta/")
}

/// The model-list body for a .../models request. All provider base URLs point at this one mock, so
/// the provider is inferred from the request PATH: a gateway configured for the anthropic/gemini/
/// cohere base uses that provider's models path shape, which carries a distinguishing marker. Order
/// matters: match the specific provider markers before the generic openai /v1/models fallback.
fn models_for(path: &str) -> &'static [u8] {
    if is_gemini(path) {
        MODELS_GEMINI
    } else if path.contains("/anthropic") || path.contains("/v1/messages") {
        MODELS_ANTHROPIC
    } else if path.contains("/v2/") || path.contains("/cohere") {
        MODELS_COHERE
    } else {
        MODELS_OPENAI
    }
}

/// Pick the response body from the request path - protocol detection, ordered so specific paths win.
fn body_for(path: &str) -> &'static [u8] {
    if is_model_list(path) {
        models_for(path)
    } else if path.contains("/chat/completions") {
        OPENAI
    } else if path.contains("/responses") {
        RESPONSES
    } else if path.contains("/messages") {
        ANTHROPIC
    } else if is_gemini(path) {
        GEMINI
    } else if path.contains("/converse") || path.contains("/model/") || path.contains("/invoke") {
        BEDROCK
    } else if path.contains("/v2/chat") {
        COHERE
    } else if path.contains("/v1/chat") {
        COHERE_V1
    } else {
        OPENAI
    }
}

/// The dialect NAME a request path lands on - same routing as body_for, but only for paths that
/// unambiguously belong to a dialect. The fallback default ("anything else answers OPENAI") is
/// deliberately NOT reported as openai here: the matrix runner needs "the gateway posted to the
/// openai chat endpoint" to mean exactly that, so unrecognized paths record under "other".
fn dialect_for(path: &str) -> &'static str {
    if is_model_list(path) {
        // A gateway's model discovery or refresh (`GET /v1beta/models`, `/v1/models`) is not a
        // request on any dialect's endpoint. Recorded under a dialect it bumped that dialect's count
        // and overwrote its body_ok with false (an empty GET body has no request shape).
        "other"
    } else if path.contains("/chat/completions") {
        "openai"
    } else if path.contains("/responses") {
        "openai-responses"
    } else if path.contains("/messages") {
        "anthropic"
    } else if is_gemini(path) {
        "gemini"
    } else if path.contains("/converse") || path.contains("/model/") || path.contains("/invoke") {
        "bedrock"
    } else if path.contains("/v2/chat") || path.contains("/v1/chat") {
        "cohere"
    } else {
        "other"
    }
}

/// Loose request-shape marker check per dialect: does the body carry the fields a client of that
/// dialect must send? Deliberately shallow (substring, no JSON parse) - the matrix runner only
/// needs "the gateway sent something recognizably shaped like that dialect's request".
fn request_shape_ok(dialect: &str, body: &[u8]) -> bool {
    let has = |needle: &str| body.windows(needle.len()).any(|w| w == needle.as_bytes());
    match dialect {
        "openai" => has("\"messages\""),
        // Bedrock Converse: the body carries `messages` whose content is an ARRAY of content BLOCKS
        // ({"text":…}), not OpenAI's `"content":"<string>"`. Requiring the block marker (or an
        // `inferenceConfig`) rejects a raw OpenAI chat body forwarded to /converse without being
        // translated into the Converse shape.
        "bedrock" => {
            let block_content = has("\"content\":[") && has("\"text\":");
            has("\"messages\"") && (block_content || has("\"inferenceConfig\""))
        }
        // Cohere: v2 chat carries `messages`; v1 chat carries `message`/`chat_history`.
        //
        // KNOWN WEAK, AND SAID SO RATHER THAN IMPLIED. A cohere v2 body and an OpenAI chat body are
        // near-identical at this depth - both are `{"model":…,"messages":[{"role","content"}]}` - so a
        // gateway that forwarded the client's OpenAI body VERBATIM to the cohere endpoint would satisfy
        // this arm. `reverify` still catches the common case, because it checks WHICH ENDPOINT the
        // request landed on and a verbatim forward hits the ingress dialect's own path (that is what
        // caught aisix's openai-responses>openai cell). What this arm cannot catch is a gateway that
        // routes correctly and translates nothing, and pretending otherwise would be worse than saying
        // it. Compare the `bedrock` arm above, which CAN discriminate because Converse genuinely
        // reshapes the body into content blocks.
        "cohere" => has("\"messages\"") || has("\"message\"") || has("\"chat_history\""),
        "openai-responses" => has("\"input\"") || has("\"instructions\""),
        // Anthropic: `max_tokens` is REQUIRED here where OpenAI treats it as optional, so requiring
        // both is the strongest marker available at this depth. Same caveat as `cohere`: the OpenAI
        // probe body carries a `max_tokens` too, so a verbatim forward would satisfy this arm and is
        // caught by the ENDPOINT check rather than by this one.
        "anthropic" => has("\"messages\"") && has("\"max_tokens\""),
        "gemini" => has("\"contents\""),
        _ => false,
    }
}

const DIALECTS: [&str; 7] = [
    "openai",
    "openai-responses",
    "anthropic",
    "gemini",
    "cohere",
    "bedrock",
    "other",
];

/// Per-dialect request record (recording only): request count, whether the last body passed the
/// dialect's shape check, and the last path + a body snippet as evidence.
#[derive(Default, Clone)]
struct DialectRecord {
    count: u64,
    body_ok: bool,
    last_path: String,
    last_snippet: String,
}

type Recorder = std::sync::Mutex<std::collections::HashMap<&'static str, DialectRecord>>;

/// Whether recording is on RIGHT NOW. An atomic rather than the plain `bool` this used to be, because
/// the state is no longer fixed at boot: `/__mock/record` flips it while requests are in flight, and
/// the unrecorded hot path must still pay nothing more than a relaxed load to find that out.
type RecordFlag = std::sync::atomic::AtomicBool;

/// Does the control body ask for recording on or off? `None` for a body that says neither, which is
/// a request the caller got wrong and must be refused rather than defaulted: silently reading an
/// unparseable body as "off" would leave a harness believing it had enabled the recorder it is about
/// to draw a conclusion from, and the conclusion it draws from an empty recorder is about somebody's
/// product.
fn wants_recording(body: &[u8]) -> Option<bool> {
    let has = |needle: &str| body.windows(needle.len()).any(|w| w == needle.as_bytes());
    match (
        has("\"on\":true"),
        has("\"on\": true"),
        has("\"on\":false"),
        has("\"on\": false"),
    ) {
        (true, _, false, false) | (_, true, false, false) => Some(true),
        (false, false, true, _) | (false, false, _, true) => Some(false),
        _ => None,
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn state_json(rec: &Recorder, recording: bool) -> String {
    let map = rec.lock().unwrap();
    let mut out = format!("{{\"recording\":{recording},\"dialects\":{{");
    for (i, d) in DIALECTS.iter().enumerate() {
        let r = map.get(d).cloned().unwrap_or_default();
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            "\"{}\":{{\"count\":{},\"body_ok\":{},\"last_path\":\"{}\",\"last_snippet\":\"{}\"}}",
            d,
            r.count,
            r.body_ok,
            json_escape(&r.last_path),
            json_escape(&r.last_snippet)
        ));
    }
    out.push_str("}}");
    out
}

/// The SSE frames for one stream, prebuilt once at boot (Bytes clones are refcount bumps).
/// `head` goes out immediately, then each `delta` after an interval sleep (first delta is
/// unpaced so direct-to-mock TTFT stays near zero), then `tail`.
///
/// The deltas are prebuilt as a VECTOR of `chunks` distinct frames, one per index, with the frame
/// index embedded in the padding text so no two consecutive content frames are byte-identical. This
/// costs nothing on the hot path (still a refcount bump per send) but keeps every gateway fair: a
/// gateway with a repetition/loop guard (e.g. one gateway aborts a stream on identical consecutive
/// chunks) is not tripped by synthetic identical tokens the way a single reused delta would trip it.
struct StreamFrames {
    openai_head: Vec<Bytes>,
    openai_deltas: Vec<Bytes>,
    openai_tail: Vec<Bytes>,
    anthropic_head: Vec<Bytes>,
    anthropic_deltas: Vec<Bytes>,
    anthropic_tail: Vec<Bytes>,
    chunks: u32,
    interval: Duration,
}

impl StreamFrames {
    fn build(chunks: u32, interval_ms: u64, chunk_bytes: usize) -> Self {
        let b = |s: String| Bytes::from(s);
        let width = chunk_bytes.max(1);
        // One distinct payload per frame index: the index (as decimal) followed by 'x' padding to
        // `width` bytes, so frame i differs from frame i-1 but every frame is the same size.
        let pad_for = |i: u32| -> String {
            let tag = i.to_string();
            if tag.len() >= width {
                // The LOW digits, which always differ between consecutive indices. Keeping the
                // leading ones made frames 10..19 identical at width 1.
                tag[tag.len() - width..].to_string()
            } else {
                let mut s = tag;
                s.push_str(&"x".repeat(width - s.len()));
                s
            }
        };
        let openai_deltas: Vec<Bytes> = (0..chunks.max(1)).map(|i| {
            let pad = pad_for(i);
            b(format!("data: {{\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"mock\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{pad}\"}},\"finish_reason\":null}}]}}\n\n"))
        }).collect();
        let anthropic_deltas: Vec<Bytes> = (0..chunks.max(1)).map(|i| {
            let pad = pad_for(i);
            b(format!("event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":\"{pad}\"}}}}\n\n"))
        }).collect();
        StreamFrames {
            openai_head: vec![b(r#"data: {"id":"chatcmpl-x","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}"#.to_string() + "\n\n")],
            openai_deltas,
            openai_tail: vec![
                b(r#"data: {"id":"chatcmpl-x","object":"chat.completion.chunk","created":1,"model":"mock","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#.to_string() + "\n\n"),
                b("data: [DONE]\n\n".to_string()),
            ],
            anthropic_head: vec![
                b("event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_x\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"mock\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n".to_string()),
                b("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n".to_string()),
            ],
            anthropic_deltas,
            anthropic_tail: vec![
                b("event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_string()),
                b(format!("event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":{chunks}}}}}\n\n")),
                b("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string()),
            ],
            chunks,
            interval: Duration::from_millis(interval_ms),
        }
    }
}

/// Does the request body ask for streaming? Cheap substring scan - no JSON parse on the hot path.
fn wants_stream(body: &[u8]) -> bool {
    body.windows(13).any(|w| w == b"\"stream\":true")
        || body.windows(14).any(|w| w == b"\"stream\": true")
}

// Same cap engine/src/http.rs enforces on the client side (MAX_BODY_BYTES there). A gateway (or a
// bug in one) sending an enormous body must not exhaust this process's memory across an 8-hour run.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

type OutBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

// How long the spawned SSE writer waits on a single tx.send before giving up on a stalled peer.
// A peer whose TCP connection stalls without closing never drains the channel, so an unbounded
// .send().await would leak this task for the rest of the 8-hour run. Overridable in test builds so
// the timeout test doesn't need to burn the production duration.
#[cfg(not(test))]
const SSE_SEND_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const SSE_SEND_TIMEOUT: Duration = Duration::from_millis(50);

/// Send one SSE frame, giving up after SSE_SEND_TIMEOUT. Returns false on either a closed receiver
/// (peer gone, the pre-existing case) or a timed-out send (peer stalled without closing, the new
/// case) - both mean the caller must stop writing rather than block this task forever.
async fn send_frame(
    tx: &tokio::sync::mpsc::Sender<Result<Frame<Bytes>, Infallible>>,
    frame: Bytes,
) -> bool {
    matches!(
        tokio::time::timeout(SSE_SEND_TIMEOUT, tx.send(Ok(Frame::data(frame)))).await,
        Ok(Ok(()))
    )
}

fn sse_response(frames: Arc<StreamFrames>, anthropic: bool, ttft_ms: u64) -> Response<OutBody> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, Infallible>>(8);
    tokio::spawn(async move {
        if ttft_ms > 0 {
            tokio::time::sleep(Duration::from_millis(ttft_ms)).await;
        }
        let (head, deltas, tail) = if anthropic {
            (
                &frames.anthropic_head,
                &frames.anthropic_deltas,
                &frames.anthropic_tail,
            )
        } else {
            (
                &frames.openai_head,
                &frames.openai_deltas,
                &frames.openai_tail,
            )
        };
        for f in head {
            if !send_frame(&tx, f.clone()).await {
                return;
            }
        }
        for i in 0..frames.chunks {
            if i > 0 {
                tokio::time::sleep(frames.interval).await;
            }
            // distinct frame per index (index embedded in the pad) so a gateway repeat-guard is fair
            let delta = &deltas[(i as usize) % deltas.len()];
            if !send_frame(&tx, delta.clone()).await {
                return;
            }
        }
        for f in tail {
            if !send_frame(&tx, f.clone()).await {
                return;
            }
        }
    });
    Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(StreamBody::new(ReceiverStream::new(rx)).boxed())
        .unwrap()
}

async fn handle(
    req: Request<Incoming>,
    ttft_ms: u64,
    frames: Arc<StreamFrames>,
    recorder: Arc<Recorder>,
    recording: Arc<RecordFlag>,
) -> Result<Response<OutBody>, Infallible> {
    let path = req.uri().path().to_string();
    // Matrix-runner control endpoints - served whether or not recording is on, so the runner can tell
    // recording apart from "no requests arrived" (state carries a `recording` flag).
    if path == "/__mock/state" {
        let on = recording.load(std::sync::atomic::Ordering::Relaxed);
        return Ok(Response::builder()
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(state_json(&recorder, on))).boxed())
            .unwrap());
    }
    if path == "/__mock/reset" {
        recorder.lock().unwrap().clear();
        return Ok(Response::builder()
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from_static(b"{\"ok\":true}")).boxed())
            .unwrap());
    }
    // TURN RECORDING ON AND OFF WHILE THE MOCK RUNS. The harness needs the recorder for one request
    // per cell and drives millions more through this process for its load windows; leaving recording
    // on for those would slow the mock, and the mock's own throughput is the reference every
    // gateway's number is judged against, so it would understate every gateway measured against it.
    // A body that says neither on nor off is REFUSED rather than defaulted: a caller who
    // believes it enabled the recorder and did not would read an empty record as a gateway failing to
    // translate.
    if path == "/__mock/record" {
        let body = match Limited::new(req.into_body(), MAX_BODY_BYTES)
            .collect()
            .await
        {
            Ok(c) => c.to_bytes(),
            Err(_) => Bytes::new(),
        };
        return Ok(match wants_recording(&body) {
            Some(on) => {
                recording.store(on, std::sync::atomic::Ordering::Relaxed);
                Response::builder()
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(format!("{{\"ok\":true,\"recording\":{on}}}"))).boxed())
                    .unwrap()
            }
            None => Response::builder()
                .status(hyper::StatusCode::BAD_REQUEST)
                .header("content-type", "application/json")
                .body(
                    Full::new(Bytes::from_static(
                        b"{\"ok\":false,\"error\":\"body must say {\\\"on\\\":true} or {\\\"on\\\":false}\"}",
                    ))
                    .boxed(),
                )
                .unwrap(),
        });
    }
    let body = body_for(&path);
    // Drain the request body so the connection stays keep-alive; only the stream flag is looked at.
    // Capped at MAX_BODY_BYTES: an unbounded read here lets one oversized (or buggy) gateway request
    // exhaust this process's memory over an 8-hour run.
    let reqbody = match Limited::new(req.into_body(), MAX_BODY_BYTES)
        .collect()
        .await
    {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return Ok(Response::builder()
                .status(hyper::StatusCode::PAYLOAD_TOO_LARGE)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from_static(b"{\"error\":\"payload too large\"}")).boxed())
                .unwrap());
        }
    };
    if recording.load(std::sync::atomic::Ordering::Relaxed) {
        // THE SHAPE CHECK AND THE ALLOCATIONS HAPPEN OUTSIDE THE LOCK. They used to happen inside it,
        // which held a process-wide mutex across a substring scan of the body plus two heap
        // allocations on every request - a serialization point in the one process whose throughput is
        // the reference every gateway's number is judged against. The critical section is now three
        // moves.
        let d = dialect_for(&path);
        let body_ok = request_shape_ok(d, &reqbody);
        let last_path = path.clone();
        let last_snippet = String::from_utf8_lossy(&reqbody[..reqbody.len().min(200)]).into_owned();
        let mut map = recorder.lock().unwrap();
        let r = map.entry(d).or_default();
        r.count += 1;
        r.body_ok = body_ok;
        r.last_path = last_path;
        r.last_snippet = last_snippet;
    }
    // The pointer comparison first: it is free, and only these two dialects can stream, so the body
    // scan is skipped for every other request.
    if (std::ptr::eq(body, OPENAI) || std::ptr::eq(body, ANTHROPIC)) && wants_stream(&reqbody) {
        return Ok(sse_response(frames, std::ptr::eq(body, ANTHROPIC), ttft_ms));
    }
    if ttft_ms > 0 {
        tokio::time::sleep(Duration::from_millis(ttft_ms)).await;
    }
    Ok(Response::builder()
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(body)).boxed())
        .unwrap())
}

/// A numeric knob from the environment: unset or blank is the default, anything else must parse.
///
/// Trimmed (see the comment in `main`). A value that does not parse STOPS THE MOCK, naming it: the
/// old reader swallowed it into the default, and the operator believed a setting was in effect that
/// nothing was producing.
fn env_knob<T: std::str::FromStr>(name: &str, default: T) -> T
where
    T::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Err(_) => default,
        Ok(v) if v.trim().is_empty() => default,
        Ok(v) => match v.trim().parse() {
            Ok(n) => n,
            Err(e) => refuse_knob(name, &v, &e.to_string()),
        },
    }
}

/// Exit with a named refusal for a setting that cannot be honoured.
fn refuse_knob(name: &str, value: &str, why: &str) -> ! {
    eprintln!("mock: {name}={value:?} is not usable ({why}); refusing to start on a setting it cannot honour");
    std::process::exit(2)
}

/// The listen backlog. tokio's `TcpListener::bind` uses 1024, and a gateway that does not pool
/// upstream connections opens about one mock connection per in-flight request - tens of thousands at
/// the top of the ladder - so a burst overflowed the queue and the kernel dropped SYNs, which the
/// client retries a second later and the load generator charges to the GATEWAY. The kernel clamps
/// this to `net.core.somaxconn`.
const LISTEN_BACKLOG: u32 = 65_535;

/// Bind the listener with `LISTEN_BACKLOG` (SO_REUSEADDR as `TcpListener::bind` sets it).
fn bind_listener(addr: SocketAddr) -> std::io::Result<TcpListener> {
    let socket = if addr.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(LISTEN_BACKLOG)
}

#[tokio::main]
async fn main() {
    let mut port: u16 = 8000;
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 {
        match args[1].as_str() {
            "engine-commit" => {
                println!("{}", env!("OTB_ENGINE_COMMIT"));
                return;
            }
            "build-commit" => {
                println!("{}", env!("OTB_SOURCE_COMMIT"));
                return;
            }
            _ => {}
        }
    }
    // A PORT THAT DOES NOT PARSE IS REFUSED, not replaced by 8000: the operator would believe the
    // mock is on the port they asked for while it listens on another - the silent-default class the
    // knob comments below call a bug.
    if let Some(i) = args.iter().position(|a| a == "-port" || a == "--port") {
        port = match args.get(i + 1).map(|v| v.trim().parse::<u16>()) {
            Some(Ok(p)) => p,
            Some(Err(e)) => refuse_knob(&args[i], &args[i + 1], &e.to_string()),
            None => refuse_knob(&args[i], "", "no value follows it"),
        };
    }
    // TRIMMED, for the reason the block below spells out at length: this knob sat three lines above a
    // comment explaining that exact bug for its neighbours and did not have the fix itself. A value
    // carrying whitespace - a shell export, a CI variable, a generated env file's trailing newline -
    // made `parse()` fail, `.ok()` swallow it, and the mock silently keep 0 while the operator believed
    // the TTFT they set was in effect.
    let ttft_ms: u64 = env_knob("MOCK_TTFT_MS", 0);
    // TRIMMED, BECAUSE THE ENGINE TRIMS. These knobs are read on BOTH sides of the measurement: the
    // mock paces frames by them, and the engine reads MOCK_STREAM_INTERVAL_MS to know what pace to
    // judge a stall against. The engine's reader is `v.trim().parse()`; this one was `v.parse()`, so a
    // value carrying any whitespace - trivially easy through a shell export or a CI variable - made the
    // mock silently keep its DEFAULT while the engine believed the new number. Nothing reports that:
    // the two sides simply measure against different cadences, and every streaming rung is then judged
    // by a pace nothing is producing. One truth read two ways is the drift this repo keeps finding.
    //
    // AND A VALUE THAT DOES NOT PARSE IS REFUSED rather than swallowed into the default (see
    // `env_knob`); each knob parses straight into its own type, so nothing is truncated by a cast.
    let s_chunks: u32 = env_knob("MOCK_STREAM_CHUNKS", 64);
    let s_interval: u64 = env_knob("MOCK_STREAM_INTERVAL_MS", 20);
    let s_bytes: usize = env_knob("MOCK_STREAM_CHUNK_BYTES", 16);
    let frames = Arc::new(StreamFrames::build(s_chunks, s_interval, s_bytes));
    // The STARTING state only. `/__mock/record` changes it at runtime, which is how the harness keeps
    // its load windows running against exactly the mock behaviour every previously published number
    // was taken against.
    let recording: Arc<RecordFlag> = Arc::new(RecordFlag::new(
        // Trimmed like every other knob: "1\n" from a generated env file must not read as off.
        std::env::var("MOCK_RECORD")
            .map(|v| v.trim() == "1")
            .unwrap_or(false),
    ));
    let recorder: Arc<Recorder> = Arc::new(Recorder::default());

    // Bind 0.0.0.0 (not just loopback) so container-networked gateways (Arch via host.docker.internal,
    // Envoy AI via the kind bridge IP) can reach the mock; the loopback path 127.0.0.1 that the
    // --network-host and native gateways use is unchanged.
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    // Retry the bind for up to 20s on failure: the harness may relaunch the mock (after pkill'ing the
    // previous one) before the old process has released the port, so a transient EADDRINUSE here must
    // not be fatal on the first try. lib/harness.sh's mock_stop_wait is the primary defense (it waits
    // for the port to actually free); this retry only covers the race it occasionally still loses.
    // Still fatal if the port never frees within the deadline.
    let listener = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            match bind_listener(addr) {
                Ok(l) => break l,
                Err(e) if std::time::Instant::now() < deadline => {
                    eprintln!("mock: bind {addr} failed ({e}); the previous mock may still hold the port - retrying");
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                Err(e) => panic!("mock: could not bind {addr} within 20s: {e}"),
            }
        }
    };
    eprintln!("mock listening on {addr} (ttft={ttft_ms}ms, proto=h1+h2c, stream={s_chunks}x{s_bytes}B@{s_interval}ms on stream:true) - OpenAI/Responses/Anthropic/Gemini/Bedrock/Cohere");
    // Backstop against unbounded in-flight connections over an 8-hour run (a leak or a runaway
    // client), not a tight operational limit.
    //
    // THE OLD JUSTIFICATION IS VOID AND THE CAP CAN NOW BIND. It read "OTB_MAX_CONC's own ceiling
    // elsewhere in this repo defaults to 512, so this is set far above anything the benchmark's own
    // concurrency produces". That default is gone: `otb.rs` now defaults max_conc to
    // `host_connection_ceiling()`, which on the bench box (ip_local_port_range 16384-65535) is 32,768 -
    // above this cap, not far below it.
    //
    // Why that matters here rather than being a tidy-up: a gateway that does NOT pool upstream
    // connections opens roughly one mock connection per in-flight request. At the top of the ladder
    // that exceeds 20,000, the accept loop parks on `acquire_owned().await`, and every further connect
    // either stalls until CONNECT_BUDGET or completes seconds late - and the load generator charges all
    // of that to the GATEWAY. A cap on the reference instrument that binds during a measurement stops
    // being a backstop and becomes part of the number.
    //
    // Raised to sit above the host's own connection ceiling so it is once again the thing it claims to
    // be: reachable only by a leak, never by the benchmark running as designed.
    let conn_permits = Arc::new(tokio::sync::Semaphore::new(40_000));
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                // BACK OFF, don't spin. The common cause is EMFILE/ENFILE, which persists until a
                // connection closes; retrying instantly burns the cores this reference instrument
                // shares with nothing else and floods the log.
                eprintln!("mock: accept failed: {e}");
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
        };
        // Wait for a permit before accepting further work: caps concurrent in-flight connections so
        // a leak or a runaway client can't accumulate unboundedly over an 8-hour run. acquire_owned
        // only fails if the semaphore is closed, which never happens here.
        let permit = match conn_permits.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => continue,
        };
        let _ = stream.set_nodelay(true);
        let io = TokioIo::new(stream);
        let frames = frames.clone();
        let recorder = recorder.clone();
        let recording = recording.clone();
        tokio::spawn(async move {
            // Held for the life of the connection; dropped (releasing the permit) when this task ends.
            let _permit = permit;
            // auto::Builder sniffs the HTTP/2 preface and serves h2c to clients that speak it, h1 to
            // those that don't - so gateways that multiplex to the upstream (like a real HTTP/2
            // provider) exercise that path, while h1-only gateways are served exactly as before. No
            // TLS: keeps the mock cheap so it stays off the critical path. (An opt-in TLS+ALPN variant
            // can be added later for a separate full-realism column.)
            let _ = auto::Builder::new(TokioExecutor::new())
                .serve_connection(
                    io,
                    service_fn(move |r| {
                        handle(
                            r,
                            ttft_ms,
                            frames.clone(),
                            recorder.clone(),
                            recording.clone(),
                        )
                    }),
                )
                .await;
        });
    }
}

#[cfg(test)]
#[path = "tests/main_tests.rs"]
mod tests;
