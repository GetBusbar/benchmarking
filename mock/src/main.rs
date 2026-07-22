// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Deterministic, blazing-fast mock upstream for the gateway benchmark. Answers a 200 with a valid,
// minimal response body for EVERY wire protocol a gateway might forward in — chosen by request path —
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
// GATEWAY's ceiling, so the mock must never be the ceiling — this sustains 100s of k RPS, and the
// harness records the mock's own ceiling each run so mock-boundedness can't hide.
//
//   mock -port 8000                    # instant responses
//   MOCK_TTFT_MS=20 mock -port 8000    # add a fixed delay (latency-isolation runs)
//
// RECORDING (matrix suite): with MOCK_RECORD=1 the mock additionally records, per protocol
// dialect, how many requests arrived on that dialect's endpoint and whether the LAST request body
// looked like that dialect's request shape (loose marker check). GET /__mock/state returns the
// record as JSON; POST /__mock/reset zeroes it. This lets the matrix runner prove a request
// actually round-tripped through the gateway to the intended egress dialect. The recording is
// entirely skipped (one branch on a bool) when MOCK_RECORD is unset, so the perf suites' hot path
// is untouched.
//
// STREAMING: when (and only when) the request body says "stream":true, the OpenAI and Anthropic
// paths answer a valid SSE stream instead — role/message_start, then N content deltas paced at a
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

use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use tokio::net::TcpListener;
use tokio_stream::wrappers::ReceiverStream;

const OPENAI: &[u8] = br#"{"id":"chatcmpl-x","object":"chat.completion","created":1,"model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}"#;
const RESPONSES: &[u8] = br#"{"id":"resp_x","object":"response","created_at":1,"status":"completed","model":"mock","output":[{"type":"message","id":"msg_x","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}"#;
const ANTHROPIC: &[u8] = br#"{"id":"msg_x","type":"message","role":"assistant","model":"mock","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2}}"#;
const GEMINI: &[u8] = br#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2,"totalTokenCount":12}}"#;
const BEDROCK: &[u8] = br#"{"output":{"message":{"role":"assistant","content":[{"text":"ok"}]}},"stopReason":"end_turn","usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}"#;
const COHERE: &[u8] = br#"{"id":"x","finish_reason":"COMPLETE","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]},"usage":{"tokens":{"input_tokens":10,"output_tokens":2}}}"#;
// GET /v1/models — a real OpenAI-shaped model list. Some gateways (e.g. GoModel) discover routable
// models by calling the upstream's /models at boot and register nothing if it isn't a proper list.
const MODELS: &[u8] = br#"{"object":"list","data":[{"id":"gpt-4o-mini","object":"model","created":1,"owned_by":"mock"},{"id":"gpt-4o","object":"model","created":1,"owned_by":"mock"},{"id":"gpt-3.5-turbo","object":"model","created":1,"owned_by":"mock"},{"id":"claude-3-5-sonnet","object":"model","created":1,"owned_by":"mock"}]}"#;

/// Pick the response body from the request path — protocol detection, ordered so specific paths win.
fn body_for(path: &str) -> &'static [u8] {
    if path.ends_with("/models") || path.contains("/models?") {
        MODELS
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

/// The dialect NAME a request path lands on — same routing as body_for, but only for paths that
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
/// dialect must send? Deliberately shallow (substring, no JSON parse) — the matrix runner only
/// needs "the gateway sent something recognizably shaped like that dialect's request".
fn request_shape_ok(dialect: &str, body: &[u8]) -> bool {
    let has = |needle: &str| body.windows(needle.len()).any(|w| w == needle.as_bytes());
    match dialect {
        "openai" | "bedrock" | "cohere" => has("\"messages\""),
        "openai-responses" => has("\"input\"") || has("\"instructions\""),
        "anthropic" => has("\"messages\"") && has("\"max_tokens\""),
        "gemini" => has("\"contents\""),
        _ => false,
    }
}

const DIALECTS: [&str; 7] =
    ["openai", "openai-responses", "anthropic", "gemini", "cohere", "bedrock", "other"];

/// Per-dialect request record (matrix suite, MOCK_RECORD=1 only): request count, whether the last
/// body passed the dialect's shape check, and the last path + a body snippet as evidence.
#[derive(Default, Clone)]
struct DialectRecord {
    count: u64,
    body_ok: bool,
    last_path: String,
    last_snippet: String,
}

type Recorder = std::sync::Mutex<std::collections::HashMap<&'static str, DialectRecord>>;

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
/// gateway with a repetition/loop guard (e.g. LiteLLM aborts a stream on identical consecutive
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

/// Does the request body ask for streaming? Cheap substring scan — no JSON parse on the hot path.
fn wants_stream(body: &[u8]) -> bool {
    body.windows(13).any(|w| w == b"\"stream\":true") || body.windows(14).any(|w| w == b"\"stream\": true")
}

type OutBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

fn sse_response(frames: Arc<StreamFrames>, anthropic: bool, ttft_ms: u64) -> Response<OutBody> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Frame<Bytes>, Infallible>>(8);
    tokio::spawn(async move {
        if ttft_ms > 0 {
            tokio::time::sleep(Duration::from_millis(ttft_ms)).await;
        }
        let (head, deltas, tail) = if anthropic {
            (&frames.anthropic_head, &frames.anthropic_deltas, &frames.anthropic_tail)
        } else {
            (&frames.openai_head, &frames.openai_deltas, &frames.openai_tail)
        };
        for f in head {
            if tx.send(Ok(Frame::data(f.clone()))).await.is_err() { return; }
        }
        for i in 0..frames.chunks {
            if i > 0 {
                tokio::time::sleep(frames.interval).await;
            }
            // distinct frame per index (index embedded in the pad) so a gateway repeat-guard is fair
            let delta = &deltas[(i as usize) % deltas.len()];
            if tx.send(Ok(Frame::data(delta.clone()))).await.is_err() { return; }
        }
        for f in tail {
            if tx.send(Ok(Frame::data(f.clone()))).await.is_err() { return; }
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
    recording: bool,
) -> Result<Response<OutBody>, Infallible> {
    let path = req.uri().path().to_string();
    // Matrix-runner control endpoints — served regardless of MOCK_RECORD so the runner can tell
    // recording apart from "no requests arrived" (state carries a `recording` flag).
    if path == "/__mock/state" {
        return Ok(Response::builder()
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(state_json(&recorder, recording))).boxed())
            .unwrap());
    }
    if path == "/__mock/reset" {
        recorder.lock().unwrap().clear();
        return Ok(Response::builder()
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from_static(b"{\"ok\":true}")).boxed())
            .unwrap());
    }
    let body = body_for(&path);
    // Drain the request body so the connection stays keep-alive; only the stream flag is looked at.
    let reqbody = req.into_body().collect().await.map(|c| c.to_bytes()).unwrap_or_default();
    if recording {
        let d = dialect_for(&path);
        let mut map = recorder.lock().unwrap();
        let r = map.entry(d).or_default();
        r.count += 1;
        r.body_ok = request_shape_ok(d, &reqbody);
        r.last_path = path.clone();
        r.last_snippet = String::from_utf8_lossy(&reqbody[..reqbody.len().min(200)]).into_owned();
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
    let ttft_ms: u64 = std::env::var("MOCK_TTFT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let envn = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let s_chunks = envn("MOCK_STREAM_CHUNKS", 64) as u32;
    let s_interval = envn("MOCK_STREAM_INTERVAL_MS", 20);
    let s_bytes = envn("MOCK_STREAM_CHUNK_BYTES", 16) as usize;
    let frames = Arc::new(StreamFrames::build(s_chunks, s_interval, s_bytes));
    let recording = std::env::var("MOCK_RECORD").map(|v| v == "1").unwrap_or(false);
    let recorder: Arc<Recorder> = Arc::new(Recorder::default());

    // Bind 0.0.0.0 (not just loopback) so container-networked gateways (Arch via host.docker.internal,
    // Envoy AI via the kind bridge IP) can reach the mock — the loopback path 127.0.0.1 that the
    // --network-host and native gateways use is unchanged.
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await.expect("bind");
    eprintln!("mock listening on {addr} (ttft={ttft_ms}ms, proto=h1+h2c, stream={s_chunks}x{s_bytes}B@{s_interval}ms on stream:true) — OpenAI/Responses/Anthropic/Gemini/Bedrock/Cohere");
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };
        let _ = stream.set_nodelay(true);
        let io = TokioIo::new(stream);
        let frames = frames.clone();
        let recorder = recorder.clone();
        tokio::spawn(async move {
            // auto::Builder sniffs the HTTP/2 preface and serves h2c to clients that speak it, h1 to
            // those that don't — so gateways that multiplex to the upstream (like a real HTTP/2
            // provider) exercise that path, while h1-only gateways are served exactly as before. No
            // TLS: keeps the mock cheap so it stays off the critical path. (An opt-in TLS+ALPN variant
            // can be added later for a separate full-realism column.)
            let _ = auto::Builder::new(TokioExecutor::new())
                .serve_connection(
                    io,
                    service_fn(move |r| {
                        handle(r, ttft_ms, frames.clone(), recorder.clone(), recording)
                    }),
                )
                .await;
        });
    }
}
