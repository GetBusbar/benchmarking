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
const RESPONSES: &[u8] = br#"{"id":"resp_x","object":"response","created_at":1,"status":"completed","model":"mock","output":[{"type":"message","id":"msg_x","status":"completed","role":"assistant","content":[{"type":"output_text","text":"ok","annotations":[]}]}],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}"#;
const ANTHROPIC: &[u8] = br#"{"id":"msg_x","type":"message","role":"assistant","model":"mock","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2}}"#;
const GEMINI: &[u8] = br#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2,"totalTokenCount":12}}"#;
const BEDROCK: &[u8] = br#"{"output":{"message":{"role":"assistant","content":[{"text":"ok"}]}},"stopReason":"end_turn","usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}"#;
const COHERE: &[u8] = br#"{"id":"x","finish_reason":"COMPLETE","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]},"usage":{"tokens":{"input_tokens":10,"output_tokens":2}}}"#;
// GET .../models - a model list, discovered at boot by gateways (e.g. one gateway) that register
// routable models by calling the upstream's /models. PROVIDER-SPECIFIC (keyed off the request path,
// see models_for) so no bare model name is ambiguous across providers. A single shared catalog that
// listed BOTH gpt-4o-mini AND claude-3-5-sonnet on every provider base made a gateway that routes by
// bare model name misroute: it registered the same name under multiple providers and then picked one
// arbitrarily. Each base now advertises only its own models.
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

/// Pick the response body from the request path — protocol detection, ordered so specific paths win.
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
        "openai" => has("\"messages\""),
        // Bedrock Converse (audit R3-M6): the body carries `messages` whose content is an ARRAY of
        // content BLOCKS ({"text":…}) — NOT OpenAI's `"content":"<string>"`. Requiring the block marker
        // (a `{"text":` content block, or an `inferenceConfig`) rejects a raw OpenAI chat body that a
        // gateway forwarded to /converse WITHOUT building the Converse shape, which the old blanket
        // `"messages"` check accepted as a false body_ok=true.
        "bedrock" => {
            let block_content = has("\"content\":[") && has("\"text\":");
            has("\"messages\"") && (block_content || has("\"inferenceConfig\""))
        }
        // Cohere: v2 chat carries `messages`; v1 chat carries `message`/`chat_history`. Accept either
        // dialect's marker (still shallow, but at least a per-dialect arm rather than the shared one).
        "cohere" => has("\"messages\"") || has("\"message\"") || has("\"chat_history\""),
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
    // BIND WITH RETRY (audit #21). This was `TcpListener::bind(addr).await.expect("bind")`, so a single
    // transient EADDRINUSE was instantly FATAL. The harness restarts the mock between cells by pkill'ing
    // the previous one and relaunching after a blind `sleep 1`; pkill returns before the old process has
    // exited, so the fresh mock regularly landed on a port its predecessor still held, panicked, and left
    // the harness probing a dead socket — 48 of 75 served cells in the 2026-07-25 field run recorded
    // "untestable / stream_mock_unready", emptying the matrix streaming lane for every gateway.
    //
    // lib/harness.sh mock_stop_wait now waits for the port to be genuinely free, which is the real fix.
    // This is the second layer: a brief retry means a lost race COSTS A FEW SECONDS instead of silently
    // destroying a cell's measurement. The failure is still fatal if the port never frees — a mock that
    // is not listening must never look like a mock that is.
    let listener = {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            match TcpListener::bind(addr).await {
                Ok(l) => break l,
                Err(e) if std::time::Instant::now() < deadline => {
                    eprintln!("mock: bind {addr} failed ({e}); the previous mock may still hold the port — retrying");
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                Err(e) => panic!("mock: could not bind {addr} within 20s: {e}"),
            }
        }
    };
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

#[cfg(test)]
mod tests {
    use super::{
        body_for, dialect_for, json_escape, models_for, request_shape_ok, state_json, wants_stream,
        Bytes, Recorder, StreamFrames, DIALECTS, ANTHROPIC, OPENAI,
    };

    // The mock must serve every dialect identically and the matrix's leg-3 body_ok must PROVE the
    // gateway spoke that dialect's request shape. These tests pin request_shape_ok per dialect,
    // including the R3-M6 tightening that rejects an unconverted OpenAI body on the bedrock/cohere
    // legs (previously any {"messages":[…]} satisfied all three).

    #[test]
    fn openai_accepts_messages() {
        assert!(request_shape_ok("openai", br#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#));
        assert!(!request_shape_ok("openai", br#"{"model":"m","input":"hi"}"#));
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
        // messages present but content is a plain string -> must be REJECTED (the M6 fix)
        assert!(!request_shape_ok(
            "bedrock",
            br#"{"model":"m","messages":[{"role":"user","content":"hello"}]}"#
        ));
    }

    #[test]
    fn cohere_accepts_v2_and_v1_shapes() {
        assert!(request_shape_ok("cohere", br#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#)); // v2 chat
        assert!(request_shape_ok("cohere", br#"{"message":"hi","chat_history":[]}"#)); // v1 chat
        assert!(!request_shape_ok("cohere", br#"{"input":"hi"}"#));
    }

    #[test]
    fn anthropic_requires_messages_and_max_tokens() {
        assert!(request_shape_ok("anthropic", br#"{"model":"m","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#));
        assert!(!request_shape_ok("anthropic", br#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#));
    }

    #[test]
    fn responses_and_gemini_markers() {
        assert!(request_shape_ok("openai-responses", br#"{"input":"hi"}"#));
        assert!(request_shape_ok("openai-responses", br#"{"instructions":"be brief"}"#));
        assert!(request_shape_ok("gemini", br#"{"contents":[{"parts":[{"text":"hi"}]}]}"#));
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
            assert!(body.contains(marker), "{path} must answer its own dialect, got {body}");
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

    // A .../models request must be answered BEFORE the chat routing, and per provider. A single
    // shared catalog that listed every provider's models on every base made a gateway that routes by
    // bare model name register the same name under several providers and then pick one arbitrarily,
    // which silently measured a different pairing than the one the cell claims.
    #[test]
    fn a_model_list_is_provider_specific_and_never_advertises_another_provider() {
        for (path, mine, theirs) in [
            ("/v1/models", "gpt-4o-mini", "claude"),
            ("/anthropic/v1/models", "claude-3-5-sonnet", "gpt-4o"),
            ("/v1beta/models", "gemini-1.5-pro", "gpt-4o"),
            ("/v2/models", "command-r", "claude"),
        ] {
            let body = String::from_utf8_lossy(models_for(path)).into_owned();
            assert!(body.contains(mine), "{path} must advertise its own models, got {body}");
            assert!(!body.contains(theirs), "{path} must not advertise another provider's models, got {body}");
            // And the chat router must hand a models path to the model list, not to a chat body.
            assert_eq!(body_for(path), models_for(path), "{path} must route to the model list");
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
        assert!(wants_stream(br#"{"model":"m","stream":true,"messages":[]}"#));
        assert!(wants_stream(br#"{"model":"m","stream": true,"messages":[]}"#));
    }

    #[test]
    fn a_non_stream_request_is_never_mistaken_for_a_stream() {
        assert!(!wants_stream(br#"{"model":"m","stream":false,"messages":[]}"#));
        assert!(!wants_stream(br#"{"model":"m","messages":[]}"#));
        assert!(!wants_stream(br#"{"stream_options":{"include_usage":true}}"#));
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
        for path in ["/v1beta/models/m:generateContent", "/model/m/converse", "/v2/chat", "/v1/responses"] {
            assert_ne!(body_for(path), OPENAI, "{path} must not be answered with a streamable body");
            assert_ne!(body_for(path), ANTHROPIC, "{path} must not be answered with a streamable body");
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
                assert_ne!(w[0], w[1], "consecutive deltas must differ or a repeat-guard trips");
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
        let sizes: std::collections::BTreeSet<usize> = f.openai_deltas.iter().map(|d| d.len()).collect();
        assert_eq!(sizes.len(), 1, "a one byte payload must give every frame the same size, got {sizes:?}");
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
        let tail = |v: &Vec<Bytes>| v.iter().map(|b| String::from_utf8_lossy(b).into_owned()).collect::<String>();
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
        assert_eq!(parsed["s"], serde_json::Value::String(nasty.to_string()), "escaping must round trip exactly");
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
            assert_eq!(parsed["dialects"][d]["count"], 0, "{d} must be present with a zero count");
            assert_eq!(parsed["dialects"][d]["body_ok"], false, "{d} must not claim a passed shape check");
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
        assert_eq!(parsed["dialects"]["anthropic"]["last_snippet"], "{\"messages\":[]}");
        // An untouched dialect in the same document is unaffected.
        assert_eq!(parsed["dialects"]["openai"]["count"], 0);
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
}
