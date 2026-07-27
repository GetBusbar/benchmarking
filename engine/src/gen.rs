// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The load generator, in Rust. `otb loadgen` (this module) is run as a subprocess by `run.rs`'s
// `load_window`.
//
// It prints a stats line (`rps=%d fail=%d ... p50us=%d ...`) that `engine/src/loadgen.rs::
// parse_ugen_line` parses on the other end; the two must stay in the same shape.
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

/// Hard ceiling on one request/response exchange, independent of the socket's per-read timeout.
const RESPONSE_BUDGET: Duration = Duration::from_secs(30);

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
    /// Set when the OS refused a thread, so the window never ran at the requested concurrency and
    /// is not a measurement of the gateway at any concurrency we could name. A worker PANIC is not
    /// represented here on purpose: thread::scope re-raises it, so it terminates the process rather
    /// than reaching any caller, and a field claiming otherwise would be dead code pretending to be
    /// a safety net.
    pub spawn_failed: bool,
    /// Every successful request's latency, microseconds. Percentiles are computed from this rather
    /// than from a running estimate: an approximate p99 is the one number nobody can check later.
    pub latencies_us: Vec<u64>,
    /// The p50/p99, already computed. `run()` leaves these `None`: its caller has `latencies_us` and
    /// can call `pct_us` itself. `run.rs::load_window_at` sets them instead, because raw per-request
    /// latencies never cross the subprocess boundary - `otb loadgen`'s stdout is the one `k=v` stats
    /// line (`loadgen.rs::parse_ugen_line`), which already carries the percentiles the child computed
    /// over samples that do not exist in this process. `pct_us(0.99)` on an empty `latencies_us` would
    /// silently read as p99=0 instead of "not measured here", so the subprocess path fills these
    /// explicitly rather than leaving a caller to rediscover that distinction.
    pub p50_us: Option<u64>,
    pub p99_us: Option<u64>,
}

impl GenStats {
    pub fn rps(&self) -> u64 {
        if self.elapsed_s <= 0.0 {
            return 0;
        }
        (self.ok as f64 / self.elapsed_s) as u64
    }

    /// Nearest-rank percentile: a convention that differs by one index from what a reader of the
    /// published numbers assumes is a silent disagreement, not a rounding difference.
    pub fn pct_us(&self, q: f64) -> u64 {
        Self::pct_of(&self.sorted_latencies(), q)
    }

    /// Sort once. `stats_line` needs two percentiles, and cloning the whole vector per call held
    /// several copies of millions of samples for a run long enough to matter.
    fn sorted_latencies(&self) -> Vec<u64> {
        let mut v = self.latencies_us.clone();
        v.sort_unstable();
        v
    }

    fn pct_of(v: &[u64], q: f64) -> u64 {
        if v.is_empty() {
            return 0;
        }
        let mut i = (v.len() as f64 * q) as usize;
        if i >= v.len() {
            i = v.len() - 1;
        }
        v[i]
    }

    /// The exact line the Go generator prints, so every existing parser reads this unchanged.
    pub fn stats_line(&self) -> String {
        let sorted = self.sorted_latencies();
        let p50 = Self::pct_of(&sorted, 0.50);
        let p99 = Self::pct_of(&sorted, 0.99);
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
    // ALLOCATED ONCE, reused across every request this worker sends: a fresh Vec per response would
    // put an allocator call in the timed hot path of every exchange, for a worker that runs for
    // hours at whatever RPS the sweep is driving. `read_response` clears it, never replaces it.
    let mut acc: Vec<u8> = Vec::with_capacity(8192);

    // Whether the connection about to be used was opened THIS iteration. A peer vanishing on a
    // brand-new connection is a real failure; the same thing on a connection we chose to reuse is
    // our reuse being stale, and counting it against the target would publish our bookkeeping as
    // its failure rate.
    let mut fresh;
    while !stop.load(Ordering::Relaxed) {
        fresh = false;
        if conn.is_none() {
            fresh = true;
            conn = TcpStream::connect_timeout(&cfg.addr, Duration::from_secs(5)).ok().inspect(|s| {
                let _ = s.set_nodelay(true);
                let _ = s.set_read_timeout(Some(Duration::from_secs(30)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(30)));
            });
            if conn.is_none() {
                // BACK OFF. Without this the loop spins at connect-refusal speed (microseconds on
                // loopback) once a gateway stops accepting, burning a core and inflating `fail`
                // into a measure of how fast connect can fail rather than a request count.
                fail += 1;
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
        }
        let Some(s) = conn.as_mut() else { continue };
        let t0 = Instant::now();
        let response_deadline = t0 + RESPONSE_BUDGET;

        let outcome = if s.write_all(req.as_bytes()).is_err() {
            // A write that fails on a REUSED connection is the same stale-connection case as a read
            // that sees nothing: the peer closed it while it sat idle.
            if fresh { Exchange::Failed } else { Exchange::ClosedBeforeAnyBytes }
        } else {
            read_response(s, response_deadline, &mut acc)
        };

        match outcome {
            Exchange::Reusable => {
                lat.push(t0.elapsed().as_micros() as u64);
                ok += 1;
            }
            // ANSWERED, and the peer said that was the last one on this connection. A success: the
            // target did exactly what it advertised. Only the connection is discarded.
            Exchange::LastOnConnection => {
                lat.push(t0.elapsed().as_micros() as u64);
                ok += 1;
                conn = None;
            }
            // A stale connection we chose to reuse. The request never reached a listening peer, so
            // it is neither a success nor the target's failure: reconnect and send it again. Not
            // counted, and no latency recorded, because nothing was measured.
            Exchange::ClosedBeforeAnyBytes if !fresh => {
                conn = None;
            }
            // The same thing on a connection opened moments ago is the target refusing to answer.
            Exchange::ClosedBeforeAnyBytes | Exchange::Failed => {
                fail += 1;
                conn = None; // a broken connection is not reused: the next request would inherit its state
            }
        }
    }

    // A POISONED LOCK MEANS A PEER WORKER PANICKED, which is a harness fault. Recover the guard so
    // THIS worker's real requests still land: skipping the merge would drop them and, if every
    // worker skipped, hand the search an empty window that reads as "the rig produced nothing"
    // rather than "our code broke". The panic itself is re-raised by thread::scope and terminates
    // the process, so there is nothing to flag here.
    let mut g = match out.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    g.ok += ok;
    g.fail += fail;
    g.latencies_us.extend_from_slice(&lat);
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

/// What one request/response exchange did to the connection.
///
/// The generator reuses connections, so a failure on a REUSED one must be told apart from a failure
/// on a fresh one: attributing a stale-connection close to the target would count a peer that
/// answered correctly and simply closed as advertised as a failed request, halving throughput and
/// failing the clean-window gate for a peer that did nothing wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exchange {
    /// A complete response, and the connection may carry another request.
    Reusable,
    /// A complete response, and the peer said this is the last one on this connection. A SUCCESS:
    /// the request was answered. The connection is simply not reused.
    LastOnConnection,
    /// The peer went away before a single byte of response arrived. On a REUSED connection that is a
    /// stale connection - the peer closed an idle one - not a failed request, because the request
    /// never reached a server that was listening. On a FRESH connection it is a real failure.
    ClosedBeforeAnyBytes,
    /// A genuine failure: a non-2xx, a malformed head, a truncated body, a timeout.
    Failed,
}

/// Whether the peer announced it will close after this response.
///
/// HTTP/1.0 defaults to close and must opt IN to keep-alive; HTTP/1.1 defaults to keep-alive and
/// opts out with `connection: close`. Getting that default backwards is what makes a well-behaved
/// HTTP/1.0 peer look like it is failing half its requests.
fn peer_will_close(head_lower: &str) -> bool {
    let says = |name: &str| head_lower.lines().any(|l| l.starts_with("connection:") && l.contains(name));
    if says("close") {
        return true;
    }
    head_lower.starts_with("http/1.0") && !says("keep-alive")
}

/// Read one response and discard it. Only success or failure matters here; the body is the mock's
/// canned reply and parsing it would charge the gateway for our own JSON cost.
///
/// `acc` is the caller's scratch buffer, reused across every request on this worker rather than
/// allocated fresh per call: cleared here, not replaced, so its capacity survives from one exchange
/// to the next instead of paying an allocator call inside the timed hot path of every request.
fn read_response(s: &mut TcpStream, deadline: Instant, acc: &mut Vec<u8>) -> Exchange {
    // A PER-READ TIMEOUT IS NOT A BOUND. The socket timeout refreshes on every byte, so a peer that
    // trickles one byte every 29s keeps a worker inside this function effectively forever. run()
    // sets the stop flag and then blocks joining every worker, so one wedged worker hangs the whole
    // sweep until the box self-terminates and the entire run is lost. This deadline is the bound.
    let mut buf = [0u8; 8192];
    acc.clear();
    let mut hdr_end: Option<usize> = None;
    // Parsed ONCE when the headers complete, then reused. Re-decoding and re-lowercasing the head
    // on every read put an allocation per read inside the timed window, which is charged to the
    // gateway. `scanned` is how far the terminator search has already looked: rescanning the whole
    // body on each read is O(N^2) in the number of reads, and chunked is the normal framing for a
    // gateway that streams, so this was the worst case rather than the rare one.
    let mut framing_kind: Option<Framing> = None;
    let mut scanned: usize = 0;
    let mut closing = false;
    let complete = |closing: bool| if closing { Exchange::LastOnConnection } else { Exchange::Reusable };
    loop {
        if hdr_end.is_none() {
            if let Some(he) = find_headers_end(acc) {
                let head = String::from_utf8_lossy(&acc[..he]).to_lowercase();
                if !head.starts_with("http/1.1 2") && !head.starts_with("http/1.0 2") {
                    return Exchange::Failed;
                }
                closing = peer_will_close(&head);
                framing_kind = Some(framing(&head));
                hdr_end = Some(he);
                scanned = he;
            }
        }
        if let (Some(he), Some(kind)) = (hdr_end, framing_kind.as_ref()) {
            match kind {
                Framing::Length(n) => match he.checked_add(*n) {
                    Some(end) if acc.len() >= end => return complete(closing),
                    Some(_) => {}
                    None => return Exchange::Failed,
                },
                Framing::Chunked => {
                    // Only the newly-arrived bytes, overlapping by the terminator length so a
                    // terminator split across two reads is still found.
                    let from = scanned.saturating_sub(4).max(he);
                    if acc[from..].windows(5).any(|w| w == b"0\r\n\r\n") {
                        return complete(closing);
                    }
                    scanned = acc.len();
                }
                // An until-close body is only complete once the peer actually closes, which the
                // n == 0 arm below handles. Returning here would call a body complete the moment the
                // headers landed and charge the gateway nothing for sending it.
                Framing::UntilClose => {}
            }
        }
        // Narrow the socket timeout to what is LEFT of the budget, the way http.rs already does.
        // A fixed per-read timeout on top of a deadline check makes the real ceiling twice the
        // advertised one: the check passes at deadline-minus-a-moment, then the read blocks for its
        // own full timeout. That extra time is also drain time the scope waits through.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Exchange::Failed;
        }
        let _ = s.set_read_timeout(Some(remaining));
        let n = match s.read(&mut buf) {
            Ok(n) => n,
            // Nothing arrived at all. The caller decides what that means: on a reused connection it
            // is a stale one, on a fresh connection it is a failure.
            Err(_) if acc.is_empty() => return Exchange::ClosedBeforeAnyBytes,
            Err(_) => return Exchange::Failed,
        };
        if n == 0 {
            if acc.is_empty() {
                return Exchange::ClosedBeforeAnyBytes;
            }
            // A closed connection completes an until-close body and truncates any other framing.
            return match hdr_end.and_then(|he| {
                let head = String::from_utf8_lossy(&acc[..he]).to_lowercase();
                matches!(framing(&head), Framing::UntilClose).then_some(())
            }) {
                // The peer closed, so there is no connection left to reuse either way.
                Some(()) => Exchange::LastOnConnection,
                None => Exchange::Failed,
            };
        }
        acc.extend_from_slice(&buf[..n]);
        if acc.len() > 1 << 20 {
            return Exchange::Failed;
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

    // STOP IS SET FROM A DROP GUARD, not from the end of the scope body.
    //
    // Scope::spawn PANICS if the OS refuses a thread, which is reachable at the concurrencies this
    // harness sweeps. That panic unwinds the scope closure, so a `stop.store` written at the end of
    // the body never runs, and `thread::scope` then blocks joining workers that were never told to
    // stop: the sweep hangs until the box self-terminates and the whole run is lost. A guard runs on
    // unwind as well as on the normal path, so any panic between here and the end still stops them.
    struct StopOnDrop<'a>(&'a AtomicBool);
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    let mut spawn_failed = false;
    // Measured around the LOAD WINDOW only. Charging the spawn ramp and the post-stop drain to the
    // denominator deflates rps in one direction, and it deflates it most at exactly the rungs where
    // the gateway is slowest, which is where the ceiling is being located.
    let load_started;
    let load_elapsed;
    {
        let stop_guard = StopOnDrop(&stop);
        load_started = Instant::now();
        std::thread::scope(|sc| {
            for i in 0..cfg.concurrency.max(1) {
                let (stop_ref, out_ref) = (Arc::clone(&stop), Arc::clone(&out));
                // Fallible spawn: the OS refusing a thread is a RIG limit, not a gateway result, and
                // it must not panic out of the scope.
                if std::thread::Builder::new()
                    .spawn_scoped(sc, move || worker(cfg, &stop_ref, &out_ref))
                    .is_err()
                {
                    eprintln!("loadgen: the OS refused a thread at worker {i} of {}", cfg.concurrency);
                    spawn_failed = true;
                    break;
                }
            }
            if !spawn_failed {
                std::thread::sleep(cfg.duration);
            }
            stop.store(true, Ordering::Relaxed);
        });
        load_elapsed = load_started.elapsed();
        drop(stop_guard);
    }

    let mut g = match out.lock() {
        Ok(g) => g.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    // The OS refusing a thread means this window never ran at the requested concurrency, so it is
    // not a measurement of the gateway at any concurrency we can name.
    g.spawn_failed = spawn_failed;
    g.elapsed_s = load_elapsed.as_secs_f64();
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn stats(lat: &[u64], ok: u64, fail: u64, elapsed: f64) -> GenStats {
        GenStats {
            ok,
            fail,
            elapsed_s: elapsed,
            latencies_us: lat.to_vec(),
            spawn_failed: false,
            p50_us: None,
            p99_us: None,
        }
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

    // A chunked response is what a real gateway sends whenever it does not buffer to compute a
    // length. A reader that stops at the header terminator instead of draining the body would leave
    // the body on the socket, so the NEXT request on the reused connection would read those bytes as
    // its status line and count a success as a failure.
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

    // A PEER THAT CLOSES IS NOT A PEER THAT FAILED.
    //
    // HTTP/1.0 defaults to closing after each response and must opt IN to keep-alive. The generator
    // reuses connections, so a failure on a reused connection must be attributed to OUR reuse of a
    // connection the peer told us it was closing, never to the target: counting it as a request
    // failure would read throughput as roughly half and fail the clean-window gate
    // (`Sample.passed` requires fail == 0) for a peer that answered every request correctly.
    #[test]
    fn a_peer_that_answers_and_closes_is_all_successes_and_no_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                std::thread::spawn(move || {
                    // Read one request, answer it in HTTP/1.0 with no keep-alive, then close. Legal,
                    // and exactly what a plain HTTP/1.0 server does.
                    let mut b = [0u8; 4096];
                    if conn.read(&mut b).unwrap_or(0) == 0 {
                        return;
                    }
                    let _ = conn.write_all(
                        b"HTTP/1.0 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{\"ok\":true}",
                    );
                });
            }
        });

        let stats = run(&GenConfig {
            addr,
            path: "/v1/chat/completions".into(),
            body: "{}".into(),
            headers: vec![],
            concurrency: 4,
            duration: Duration::from_millis(400),
            ttft_ms: 0,
        });

        assert!(stats.ok > 0, "the peer answers every request, so there must be successes");
        assert_eq!(
            stats.fail, 0,
            "a peer closing a connection it said it would close is not a failed request: ok={} fail={}",
            stats.ok, stats.fail
        );
    }

    // The other half of the same rule: a peer that closes WITHOUT answering is a real failure, and
    // must still be counted. Otherwise the fix above would silently swallow a gateway that accepts
    // connections and refuses to serve, which is a live failure mode (a container that binds its
    // port and then dies at config load looks exactly like this).
    #[test]
    fn a_peer_that_accepts_and_closes_without_answering_is_a_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                drop(conn); // accept, then close immediately, saying nothing
            }
        });

        let stats = run(&GenConfig {
            addr,
            path: "/v1/chat/completions".into(),
            body: "{}".into(),
            headers: vec![],
            concurrency: 2,
            duration: Duration::from_millis(300),
            ttft_ms: 0,
        });

        assert_eq!(stats.ok, 0, "nothing was ever answered");
        assert!(stats.fail > 0, "a peer that never answers must be recorded as failing");
    }

    // A chunked body delivered across MANY small reads. The terminator search must look only at
    // newly-arrived bytes, so a terminator split across a read boundary is still found: rescanning
    // the whole body from the start on every read is O(N^2), and every microsecond of it lands
    // inside the timed window and is charged to the gateway.
    #[test]
    fn a_chunked_body_split_across_many_reads_still_terminates() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for conn in listener.incoming().take(4) {
                let Ok(mut conn) = conn else { continue };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    while conn.read(&mut b).unwrap_or(0) > 0 {
                        if conn.write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n").is_err() {
                            return;
                        }
                        // Many small chunks, then the terminator written one byte at a time so it
                        // straddles read boundaries.
                        for _ in 0..64 {
                            if conn.write_all(b"4\r\nabcd\r\n").is_err() {
                                return;
                            }
                        }
                        for byte in b"0\r\n\r\n" {
                            if conn.write_all(&[*byte]).is_err() {
                                return;
                            }
                            std::thread::sleep(Duration::from_micros(200));
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
            duration: Duration::from_millis(400),
            ttft_ms: 0,
        };
        let stats = run(&cfg);
        assert!(stats.ok > 0, "a fragmented chunked body must complete, ok={} fail={}", stats.ok, stats.fail);
        assert_eq!(stats.fail, 0, "a terminator split across reads must not read as a failure");
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
