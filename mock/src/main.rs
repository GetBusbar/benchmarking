// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Deterministic mock upstream for the gateway benchmark. Answers ANY path with a 200 and a fixed
// small JSON body — OpenAI chat-completion shape by default, Anthropic Messages shape for a path
// containing `/messages` (so a gateway whose only working path is the Anthropic Messages API — e.g.
// LiteLLM-Rust's azure_ai route — gets a response it can actually parse).
//
// It is deliberately dumb and deliberately fast: hyper on a multi-threaded tokio runtime, static
// response bytes, the request body drained but never processed. The point of a throughput benchmark
// is to find the GATEWAY's ceiling — so the mock must never be the ceiling. On a few cores this
// sustains hundreds of thousands of requests/sec; the harness also records the mock's own ceiling
// each run and flags any gateway result that gets close to it, so mock-boundedness can't hide.
//
//   mock -port 8000            # instant responses
//   MOCK_TTFT_MS=20 mock -port 8000   # add a fixed 20 ms delay (latency-isolation runs)

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

const OPENAI: &[u8] = br#"{"id":"chatcmpl-x","object":"chat.completion","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}"#;
const ANTHROPIC: &[u8] = br#"{"id":"msg_x","type":"message","role":"assistant","model":"claude","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":2}}"#;

async fn handle(req: Request<Incoming>, ttft_ms: u64) -> Result<Response<Full<Bytes>>, Infallible> {
    let is_messages = req.uri().path().contains("/messages");
    // Drain the request body so the connection stays keep-alive; we never look at it.
    let _ = req.into_body().collect().await;
    if ttft_ms > 0 {
        tokio::time::sleep(Duration::from_millis(ttft_ms)).await;
    }
    let body = if is_messages { ANTHROPIC } else { OPENAI };
    Ok(Response::builder()
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from_static(body)))
        .unwrap())
}

#[tokio::main]
async fn main() {
    // args: -port <n>  (default 8000);  env MOCK_TTFT_MS (default 0)
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
    eprintln!("mock listening on {addr} (ttft={ttft_ms}ms)");
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
