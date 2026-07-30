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
// comments instead of stated in one place does when the code beneath it changes. A recorded request takes a process-wide lock, and the harness
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
// Other protocols (Gemini/Bedrock/Cohere) ignore stream:true and answer their normal JSON.

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

const OPENAI: &[u8] = br#"{"id":"chatcmpl-x","object":"chat.completion","created":1,"model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}"#;
const RESPONSES: &[u8] = br#"{"id":"resp_x","object":"response","created_at":1,"status":"completed","model":"mock","output":[{"type":"message","id":"msg_x","status":"completed","role":"assistant","content":[{"type":"output_text","text":"ok","annotations":[]}]}],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}"#;
const ANTHROPIC: &[u8] = br#"{"id":"msg_x","type":"message","role":"assistant","model":"mock","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2}}"#;
const GEMINI: &[u8] = br#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2,"totalTokenCount":12}}"#;
const BEDROCK: &[u8] = br#"{"output":{"message":{"role":"assistant","content":[{"text":"ok"}]}},"stopReason":"end_turn","usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}"#;
const COHERE: &[u8] = br#"{"id":"x","finish_reason":"COMPLETE","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]},"usage":{"tokens":{"input_tokens":10,"output_tokens":2}}}"#;
// GET .../models - a model list, discovered at boot by gateways that register routable models by
// calling the upstream's /models. PROVIDER-SPECIFIC (keyed off the request path, see models_for): a
// catalog shared across providers would let a gateway that routes by bare model name register the
// same name under multiple providers and pick one arbitrarily, so each base advertises only its own.
const MODELS_OPENAI: &[u8] = br#"{"object":"list","data":[{"id":"gpt-4o-mini","object":"model","created":1,"owned_by":"openai"},{"id":"gpt-4o","object":"model","created":1,"owned_by":"openai"},{"id":"gpt-3.5-turbo","object":"model","created":1,"owned_by":"openai"}]}"#;
const MODELS_ANTHROPIC: &[u8] = br#"{"data":[{"id":"claude-3-5-sonnet","type":"model","display_name":"Claude 3.5 Sonnet"},{"id":"claude-3-5-haiku","type":"model","display_name":"Claude 3.5 Haiku"}],"has_more":false}"#;
const MODELS_GEMINI: &[u8] = br#"{"models":[{"name":"models/gemini-1.5-pro","displayName":"Gemini 1.5 Pro"},{"name":"models/gemini-1.5-flash","displayName":"Gemini 1.5 Flash"}]}"#;
const MODELS_COHERE: &[u8] = br#"{"models":[{"name":"command-r","endpoints":["chat"]},{"name":"command-r-plus","endpoints":["chat"]}]}"#;

/// The model-list body for a .../models request. All provider base URLs point at this one mock, so
/// the provider is inferred from the request PATH: a gateway configured for the anthropic/gemini/
/// cohere base uses that provider's models path shape, which carries a distinguishing marker. Order
/// matters: match the specific provider markers before the generic openai /v1/models fallback.
fn models_for(path: &str) -> &'static [u8] {
    if path.contains("/v1beta/") || path.contains("generateContent") {
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
    if path.ends_with("/models") || path.contains("/models?") {
        models_for(path)
    } else if path.contains("/chat/completions") {
        OPENAI
    } else if path.contains("/responses") {
        RESPONSES
    } else if path.contains("/messages") {
        ANTHROPIC
    } else if path.contains("generateContent") || path.contains("/v1beta/") {
        GEMINI
    } else if path.contains("/converse") || path.contains("/model/") || path.contains("/invoke") {
        BEDROCK
    } else if path.contains("/v2/chat") || path.contains("/v1/chat") {
        COHERE
    } else {
        OPENAI
    }
}

/// The dialect NAME a request path lands on - same routing as body_for, but only for paths that
/// unambiguously belong to a dialect. The fallback default ("anything else answers OPENAI") is
/// deliberately NOT reported as openai here: the matrix runner needs "the gateway posted to the
/// openai chat endpoint" to mean exactly that, so unrecognized paths record under "other".
fn dialect_for(path: &str) -> &'static str {
    if path.contains("/chat/completions") {
        "openai"
    } else if path.contains("/responses") {
        "openai-responses"
    } else if path.contains("/messages") {
        "anthropic"
    } else if path.contains("generateContent") || path.contains("/v1beta/") {
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
                tag[..width].to_string()
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
    // gateway's number is judged against, so it would suppress real gateway measurements as
    // mock-bound. A body that says neither on nor off is REFUSED rather than defaulted: a caller who
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
    if wants_stream(&reqbody) && (std::ptr::eq(body, OPENAI) || std::ptr::eq(body, ANTHROPIC)) {
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

#[tokio::main]
async fn main() {
    let mut port: u16 = 8000;
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "-port" || a == "--port") {
        if let Some(v) = args.get(i + 1) {
            port = v.parse().unwrap_or(8000);
        }
    }
    // TRIMMED, for the reason the block below spells out at length: this knob sat three lines above a
    // comment explaining that exact bug for its neighbours and did not have the fix itself. A value
    // carrying whitespace - a shell export, a CI variable, a generated env file's trailing newline -
    // made `parse()` fail, `.ok()` swallow it, and the mock silently keep 0 while the operator believed
    // the TTFT they set was in effect.
    let ttft_ms: u64 = std::env::var("MOCK_TTFT_MS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    // TRIMMED, BECAUSE THE ENGINE TRIMS. These knobs are read on BOTH sides of the measurement: the
    // mock paces frames by them, and the engine reads MOCK_STREAM_INTERVAL_MS to know what pace to
    // judge a stall against. The engine's reader is `v.trim().parse()`; this one was `v.parse()`, so a
    // value carrying any whitespace - trivially easy through a shell export or a CI variable - made the
    // mock silently keep its DEFAULT while the engine believed the new number. Nothing reports that:
    // the two sides simply measure against different cadences, and every streaming rung is then judged
    // by a pace nothing is producing. One truth read two ways is the drift this repo keeps finding.
    let envn = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(d)
    };
    let s_chunks = envn("MOCK_STREAM_CHUNKS", 64) as u32;
    let s_interval = envn("MOCK_STREAM_INTERVAL_MS", 20);
    let s_bytes = envn("MOCK_STREAM_CHUNK_BYTES", 16) as usize;
    let frames = Arc::new(StreamFrames::build(s_chunks, s_interval, s_bytes));
    // The STARTING state only. `/__mock/record` changes it at runtime, which is how the harness keeps
    // its load windows running against exactly the mock behaviour every previously published number
    // was taken against.
    let recording: Arc<RecordFlag> = Arc::new(RecordFlag::new(
        std::env::var("MOCK_RECORD")
            .map(|v| v == "1")
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
            match TcpListener::bind(addr).await {
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
                eprintln!("mock: accept failed: {e}");
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
mod tests {
    use super::{
        body_for, dialect_for, handle, json_escape, models_for, request_shape_ok, send_frame,
        state_json, wants_recording, wants_stream, Arc, BodyExt, Bytes, Frame, Full, Limited,
        RecordFlag, Recorder, StreamFrames, ANTHROPIC, DIALECTS, MAX_BODY_BYTES, OPENAI,
        SSE_SEND_TIMEOUT,
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
    }

    // ── request body cap ────────────────────────────────────────────────────────────────────────

    // A gateway (or a bug in one) sending a body past MAX_BODY_BYTES must get a real 413, not have
    // this process read the whole thing into memory unbounded. Drives handle() over a real loopback
    // connection - the cap lives in how handle() wraps the incoming body, so a test against Limited
    // in isolation would not prove the wrapping actually happened.
    #[tokio::test]
    async fn a_request_body_over_the_cap_gets_413_instead_of_being_read_unbounded() {
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

        let mut client = match TcpStream::connect(addr).await {
            Ok(c) => c,
            Err(e) => panic!("test client connect must succeed: {e}"),
        };
        let oversized = vec![b'a'; MAX_BODY_BYTES + 1024];
        let req_head = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            oversized.len()
        );
        if let Err(e) = client.write_all(req_head.as_bytes()).await {
            panic!("writing the request head must succeed: {e}");
        }
        if let Err(e) = client.write_all(&oversized).await {
            panic!("writing the oversized body must succeed: {e}");
        }

        let mut resp = Vec::new();
        if let Err(e) = client.read_to_end(&mut resp).await {
            panic!("reading the response must succeed: {e}");
        }
        let status_line = String::from_utf8_lossy(&resp[..resp.len().min(32)]).into_owned();
        assert!(
            status_line.contains("413"),
            "a body over the cap must get 413, got: {status_line}"
        );
    }

    // The library primitive the fix relies on: a body at exactly the cap is unaffected, so the cap
    // does not falsely reject a request the benchmark itself sends (the largest real payloads in
    // this repo's suites are nowhere near MAX_BODY_BYTES, but the boundary must not be off-by-one).
    #[tokio::test]
    async fn a_body_at_exactly_the_cap_is_still_accepted() {
        let ok = Full::new(Bytes::from(vec![0u8; MAX_BODY_BYTES]));
        let res = Limited::new(ok, MAX_BODY_BYTES).collect().await;
        assert!(
            res.is_ok(),
            "a body at exactly the cap must not be rejected"
        );
    }

    // ── accept loop connection cap ──────────────────────────────────────────────────────────────

    // main()'s accept loop backpressures on this semaphore rather than accumulating unbounded
    // in-flight connections. This pins the mechanism itself: once every permit is held, no further
    // permit is available until one is released.
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
}
