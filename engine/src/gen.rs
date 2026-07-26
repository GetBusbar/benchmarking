// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The load generator, in Rust.
//
// It emits the SAME stats line the Go generator emits, so it is drop-in for every existing parser
// and, more importantly, so the two can be run against the same gateway and diffed. This one does
// not become the instrument until that diff agrees on rps and p99: every published number on the
// board was taken with the Go generator, so swapping instruments without proof would make a real
// throughput change indistinguishable from a measurement change.
//
// std only, and threads rather than async on purpose. This process is pinned to its own cores and
// its job is to saturate a loopback socket; an async runtime would add a scheduler between the
// measurement and the clock for no benefit at this concurrency, and every dependency here is
// something that has to be audited before anyone trusts the numbers.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct GenConfig {
    pub addr: SocketAddr,
    pub path: String,
    pub body: String,
    pub headers: Vec<(String, String)>,
    pub concurrency: u32,
    pub duration: Duration,
    /// Time the upstream takes to first byte, mirrored into the request so the mock paces itself.
    pub ttft_ms: u32,
}

#[derive(Debug, Default, Clone)]
pub struct GenStats {
    pub ok: u64,
    pub fail: u64,
    pub elapsed_s: f64,
    /// Every successful request's latency, microseconds. Percentiles are computed from this rather
    /// than from a running estimate: an approximate p99 is the one number nobody can check later.
    pub latencies_us: Vec<u64>,
}

impl GenStats {
    pub fn rps(&self) -> u64 {
        if self.elapsed_s <= 0.0 {
            return 0;
        }
        (self.ok as f64 / self.elapsed_s) as u64
    }

    /// Nearest-rank, matching the Go generator's `pct()` exactly. Verified against loadgen/ugen.go
    /// rather than inferred, because a percentile convention that differs by one index is a silent
    /// disagreement between two instruments that both look right.
    pub fn pct_us(&self, q: f64) -> u64 {
        if self.latencies_us.is_empty() {
            return 0;
        }
        let mut v = self.latencies_us.clone();
        v.sort_unstable();
        let mut i = (v.len() as f64 * q) as usize;
        if i >= v.len() {
            i = v.len() - 1;
        }
        v[i]
    }

    /// The exact line the Go generator prints, so every existing parser reads this unchanged.
    pub fn stats_line(&self) -> String {
        let p50 = self.pct_us(0.50);
        let p99 = self.pct_us(0.99);
        format!(
            "rps={} fail={} p50={:.2} p99={:.2} p50us={} p99us={} ok={}",
            self.rps(),
            self.fail,
            p50 as f64 / 1000.0,
            p99 as f64 / 1000.0,
            p50,
            p99,
            self.ok
        )
    }
}

/// One worker's request loop. Opens a connection and reuses it, reconnecting on failure, because a
/// fresh TCP handshake per request would measure the kernel rather than the gateway.
fn worker(cfg: &GenConfig, stop: &AtomicBool, out: &Mutex<GenStats>) {
    let mut lat: Vec<u64> = Vec::with_capacity(1024);
    let (mut ok, mut fail) = (0u64, 0u64);
    let req = build_request(cfg);
    let mut conn: Option<TcpStream> = None;

    while !stop.load(Ordering::Relaxed) {
        if conn.is_none() {
            conn = TcpStream::connect_timeout(&cfg.addr, Duration::from_secs(5)).ok().inspect(|s| {
                let _ = s.set_nodelay(true);
                let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(30)));
            });
            if conn.is_none() {
                fail += 1;
                continue;
            }
        }
        let Some(s) = conn.as_mut() else { continue };
        let t0 = Instant::now();
        if s.write_all(req.as_bytes()).is_err() || read_response(s).is_err() {
            fail += 1;
            conn = None; // a broken connection is not reused: the next request would inherit its state
            continue;
        }
        lat.push(t0.elapsed().as_micros() as u64);
        ok += 1;
    }

    if let Ok(mut g) = out.lock() {
        g.ok += ok;
        g.fail += fail;
        g.latencies_us.extend_from_slice(&lat);
    }
}

fn build_request(cfg: &GenConfig) -> String {
    let mut h = String::new();
    for (k, v) in &cfg.headers {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{}\r\n{}",
        cfg.path,
        cfg.addr,
        cfg.body.len(),
        h,
        cfg.body
    )
}

/// Read one response and discard it. Only success or failure matters here; the body is the mock's
/// canned reply and parsing it would charge the gateway for our own JSON cost.
fn read_response(s: &mut TcpStream) -> std::io::Result<()> {
    let mut buf = [0u8; 8192];
    let mut acc: Vec<u8> = Vec::with_capacity(8192);
    let mut hdr_end: Option<usize> = None;
    loop {
        if hdr_end.is_none() {
            hdr_end = find_headers_end(&acc);
        }
        if let Some(he) = hdr_end {
            let head = String::from_utf8_lossy(&acc[..he]).to_lowercase();
            if !head.starts_with("http/1.1 2") && !head.starts_with("http/1.0 2") {
                return Err(std::io::Error::other("non-2xx"));
            }
            // THE BODY MUST BE DRAINED, and an absent Content-Length is NOT zero.
            //
            // This read `content_length(..).unwrap_or(0)` and returned the moment the headers
            // arrived. On a chunked response (what a gateway sends whenever it does not buffer to
            // compute a length up front) that stopped the latency clock before the body existed,
            // AND left the body on the socket. The connection is deliberately reused, so the next
            // request then read those leftover bytes as its status line, failed to match "http/1.1
            // 2", and counted a successful request as a failure. The desync persists for the rest
            // of the run. Under-reported latency, under-reported rps, over-reported failures, all
            // silent, in the instrument every published number comes from.
            match framing(&head) {
                Framing::Length(n) => {
                    if acc.len() >= he + n {
                        return Ok(());
                    }
                }
                Framing::Chunked => {
                    // The terminal chunk. Cheap and sufficient: we only need to know the body
                    // finished, never what it said.
                    if acc[he..].windows(5).any(|w| w == b"0\r\n\r\n") {
                        return Ok(());
                    }
                }
                // Neither header: HTTP/1.1 says the body then runs to connection close, so there is
                // nothing to wait for and nothing left to desync the next request.
                Framing::UntilClose => return Ok(()),
            }
        }
        let n = s.read(&mut buf)?;
        if n == 0 {
            // A closed connection completes an until-close body and truncates any other framing.
            return match hdr_end.and_then(|he| {
                let head = String::from_utf8_lossy(&acc[..he]).to_lowercase();
                matches!(framing(&head), Framing::UntilClose).then_some(())
            }) {
                Some(()) => Ok(()),
                None => Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "closed mid-body")),
            };
        }
        acc.extend_from_slice(&buf[..n]);
        if acc.len() > 1 << 20 {
            return Err(std::io::Error::other("response too large"));
        }
    }
}

enum Framing {
    Length(usize),
    Chunked,
    UntilClose,
}

/// How the body's end is signalled. Absent framing is its own case, never a zero-length body.
fn framing(head_lower: &str) -> Framing {
    if head_lower.lines().any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked")) {
        return Framing::Chunked;
    }
    match content_length(head_lower) {
        Some(n) => Framing::Length(n),
        None => Framing::UntilClose,
    }
}

fn find_headers_end(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn content_length(head_lower: &str) -> Option<usize> {
    head_lower
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
}

/// Run the load and return the aggregate.
pub fn run(cfg: &GenConfig) -> GenStats {
    let stop = Arc::new(AtomicBool::new(false));
    let out = Arc::new(Mutex::new(GenStats::default()));
    let started = Instant::now();

    std::thread::scope(|sc| {
        for _ in 0..cfg.concurrency.max(1) {
            let (stop, out) = (Arc::clone(&stop), Arc::clone(&out));
            sc.spawn(move || worker(cfg, &stop, &out));
        }
        std::thread::sleep(cfg.duration);
        stop.store(true, Ordering::Relaxed);
    });

    let mut g = out.lock().map(|g| g.clone()).unwrap_or_default();
    g.elapsed_s = started.elapsed().as_secs_f64();
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn stats(lat: &[u64], ok: u64, fail: u64, elapsed: f64) -> GenStats {
        GenStats { ok, fail, elapsed_s: elapsed, latencies_us: lat.to_vec() }
    }

    // The percentile convention must match the Go generator EXACTLY. A one-index difference is a
    // silent disagreement between two instruments that both look correct, and it would surface as an
    // unexplained step change in published p99 the day the instrument was swapped.
    #[test]
    fn percentiles_are_nearest_rank_matching_the_go_generator() {
        let s = stats(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100], 10, 0, 1.0);
        assert_eq!(s.pct_us(0.50), 60, "index (10*0.5)=5 -> the 6th value");
        assert_eq!(s.pct_us(0.99), 100, "index (10*0.99)=9 -> the last value");
        assert_eq!(s.pct_us(0.0), 10);
        assert_eq!(s.pct_us(1.0), 100, "q=1.0 clamps to the last index rather than overflowing");
    }

    #[test]
    fn an_empty_sample_has_no_percentile_rather_than_a_wrong_one() {
        assert_eq!(stats(&[], 0, 0, 1.0).pct_us(0.99), 0);
    }

    #[test]
    fn rps_is_successes_over_measured_elapsed_not_nominal_duration() {
        // Measured elapsed, because a run that took longer than asked must not report the rate it
        // would have had. The Go generator makes the same choice.
        assert_eq!(stats(&[1], 1000, 0, 10.0).rps(), 100);
        assert_eq!(stats(&[1], 1000, 0, 0.0).rps(), 0, "no elapsed time is no rate, not a division");
    }

    #[test]
    fn the_stats_line_is_byte_compatible_with_the_existing_parser() {
        let s = stats(&[1000, 2000, 3000, 4000], 4, 1, 2.0);
        let line = s.stats_line();
        for k in ["rps=", "fail=", "p50=", "p99=", "p50us=", "p99us=", "ok="] {
            assert!(line.contains(k), "missing {k} in {line}");
        }
        // The existing Rust parser must read our own output.
        let parsed = crate::loadgen::parse_ugen_line(&line);
        assert!(parsed.is_measured(), "our own stats line must parse: {line}");
    }

    #[test]
    fn a_failed_request_counts_as_a_failure_and_contributes_no_latency() {
        let s = stats(&[], 0, 5, 1.0);
        assert_eq!(s.fail, 5);
        assert!(s.latencies_us.is_empty(), "a failure has no latency to report");
        assert_eq!(s.rps(), 0, "failures are not throughput");
    }

    #[test]
    fn the_request_carries_the_body_and_every_supplied_header() {
        let cfg = GenConfig {
            addr: "127.0.0.1:1".parse().expect("literal"),
            path: "/v1/chat/completions".into(),
            body: r#"{"a":1}"#.into(),
            headers: vec![("x-one".into(), "1".into()), ("x-two".into(), "2".into())],
            concurrency: 1,
            duration: Duration::from_millis(1),
            ttft_ms: 0,
        };
        let r = build_request(&cfg);
        assert!(r.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(r.contains("x-one: 1\r\n") && r.contains("x-two: 2\r\n"));
        assert!(r.ends_with(r#"{"a":1}"#));
        assert!(r.contains("content-length: 7\r\n"), "length must match the body exactly");
    }

    // End to end against a real socket: the generator must actually move requests and record them.
    #[test]
    fn drives_a_real_socket_and_records_what_it_measured() {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = Arc::clone(&stop);
        std::thread::spawn(move || {
            for c in l.incoming() {
                if s2.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(mut c) = c else { continue };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    while c.read(&mut b).unwrap_or(0) > 0 {
                        if c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok").is_err() {
                            return;
                        }
                    }
                });
            }
        });

        let cfg = GenConfig {
            addr,
            path: "/x".into(),
            body: "{}".into(),
            headers: vec![],
            concurrency: 2,
            duration: Duration::from_millis(250),
            ttft_ms: 0,
        };
        let g = run(&cfg);
        stop.store(true, Ordering::Relaxed);
        assert!(g.ok > 0, "the generator must actually complete requests");
        assert_eq!(g.latencies_us.len() as u64, g.ok, "every success contributes exactly one latency");
        assert!(g.elapsed_s > 0.0);
        assert!(g.rps() > 0, "a run that completed requests has a rate");
    }

    // RED-BEFORE. Every fixture in this file hardcoded content-length, so nothing here could see
    // that an absent one was being read as a zero-length body. A chunked response is what a real
    // gateway sends whenever it does not buffer to compute a length, and the old code returned the
    // instant it saw the header terminator: clock stopped before the body, body left on the socket,
    // and the NEXT request on the reused connection read those bytes as its status line and counted
    // a success as a failure.
    #[test]
    fn a_chunked_response_is_drained_so_the_next_request_is_not_corrupted() {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming().take(8) {
                let Ok(mut c) = c else { continue };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    // Answer every request on this connection with a CHUNKED body, so a reader that
                    // does not drain will desync on the second one.
                    while c.read(&mut b).unwrap_or(0) > 0 {
                        let r = "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n\
                                 4\r\nabcd\r\n4\r\nefgh\r\n0\r\n\r\n";
                        if c.write_all(r.as_bytes()).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        let cfg = GenConfig {
            addr,
            path: "/x".into(),
            body: "{}".into(),
            headers: vec![],
            concurrency: 1,
            duration: Duration::from_millis(250),
            ttft_ms: 0,
        };
        let g = run(&cfg);
        assert!(g.ok > 1, "chunked responses must complete, got ok={} fail={}", g.ok, g.fail);
        // The real tell: with an undrained body the connection desyncs and every request after the
        // first is misread as a non-2xx, so failures would dominate.
        assert_eq!(g.fail, 0, "a drained connection must not manufacture failures, got {}", g.fail);
    }

    // A body with no length header and no chunking runs to connection close. That is a legitimate
    // framing, NOT a zero-length body, and it must not be confused with one.
    #[test]
    fn a_response_with_no_framing_header_is_not_treated_as_an_empty_body() {
        assert!(matches!(framing("http/1.1 200 ok\r\n"), Framing::UntilClose));
        assert!(matches!(framing("http/1.1 200 ok\r\ncontent-length: 12\r\n"), Framing::Length(12)));
        assert!(matches!(framing("http/1.1 200 ok\r\ntransfer-encoding: chunked\r\n"), Framing::Chunked));
    }

    // A non-2xx is a FAILURE, not a fast success. Counting an error page as throughput is how a
    // broken gateway posts its best number.
    #[test]
    fn a_non_2xx_response_is_a_failure_not_throughput() {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming().take(64) {
                let Ok(mut c) = c else { continue };
                let mut b = [0u8; 4096];
                let _ = c.read(&mut b);
                let _ = c.write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n");
            }
        });
        let cfg = GenConfig {
            addr,
            path: "/x".into(),
            body: "{}".into(),
            headers: vec![],
            concurrency: 1,
            duration: Duration::from_millis(150),
            ttft_ms: 0,
        };
        let g = run(&cfg);
        assert_eq!(g.ok, 0, "a 503 is never a success");
        assert!(g.fail > 0, "and it must be counted as a failure");
    }
}
