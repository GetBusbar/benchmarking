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

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

const OPENAI: &[u8] = br#"{"id":"chatcmpl-x","object":"chat.completion","created":1,"model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}"#;
const RESPONSES: &[u8] = br#"{"id":"resp_x","object":"response","created_at":1,"status":"completed","model":"mock","output":[{"type":"message","id":"msg_x","role":"assistant","content":[{"type":"output_text","text":"ok"}]}],"usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12}}"#;
const ANTHROPIC: &[u8] = br#"{"id":"msg_x","type":"message","role":"assistant","model":"mock","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2}}"#;
const GEMINI: &[u8] = br#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP","index":0}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":2,"totalTokenCount":12}}"#;
const BEDROCK: &[u8] = br#"{"output":{"message":{"role":"assistant","content":[{"text":"ok"}]}},"stopReason":"end_turn","usage":{"inputTokens":10,"outputTokens":2,"totalTokens":12}}"#;
const COHERE: &[u8] = br#"{"id":"x","finish_reason":"COMPLETE","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]},"usage":{"tokens":{"input_tokens":10,"output_tokens":2}}}"#;

/// Pick the response body from the request path — protocol detection, ordered so specific paths win.
fn body_for(path: &str) -> &'static [u8] {
    if path.contains("/chat/completions") {
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

async fn handle(req: Request<Incoming>, ttft_ms: u64) -> Result<Response<Full<Bytes>>, Infallible> {
    let body = body_for(req.uri().path());
    // Drain the request body so the connection stays keep-alive; we never look at it.
    let _ = req.into_body().collect().await;
    if ttft_ms > 0 {
        tokio::time::sleep(Duration::from_millis(ttft_ms)).await;
    }
    Ok(Response::builder()
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(body)))
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

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await.expect("bind");
    eprintln!("mock listening on {addr} (ttft={ttft_ms}ms) — OpenAI/Responses/Anthropic/Gemini/Bedrock/Cohere");
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };
        let _ = stream.set_nodelay(true);
        let io = TokioIo::new(stream);
        tokio::spawn(async move {
            let _ = http1::Builder::new()
                .keep_alive(true)
                .serve_connection(io, service_fn(move |r| handle(r, ttft_ms)))
                .await;
        });
    }
}
