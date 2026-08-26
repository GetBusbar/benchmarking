// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The load generator, in Rust. `otb loadgen` (this module) is run as a subprocess by `run.rs`'s
// `load_window`.
//
// Prints a stats line (`rps=<f64> fail=%d ... p50us=%d ...`) that `engine/src/loadgen.rs::
// parse_ugen_line` parses on the other end; the two must stay in the same shape. `rps` is a FLOAT:
// below 1/s the rate is fractional, and printing it as an integer would send `rps=0` for a window
// that carried requests (see `UgenStats::rps`).
//
// Uses one async task per unit of concurrency, not one OS thread. Thread-per-connection could not
// hold the connection counts a sweep asks for (a run toward 32k concurrency pinned this process's
// six cores at a 1-min load average over 24,000 and never converged, measuring the rig's own
// scheduler thrashing rather than the gateway). A task costs a few KB against a thread's full
// stack. The mock reference instrument already runs on tokio, so this adds no new dependency to
// the measurement path.
//
// Wire handling (connection reuse, fresh-vs-reused failure attribution, HTTP framing, response
// budget, non-2xx rule) is unchanged, since that is what the published numbers mean.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Hard ceiling on one request/response exchange, independent of the socket's per-read timeout.
const RESPONSE_BUDGET: Duration = Duration::from_secs(30);

/// Hard ceiling on ONE connect attempt. A bound of ours exactly as `RESPONSE_BUDGET` is, and named
/// so the code that charges a connect timeout can say which bound fired.
const CONNECT_BUDGET: Duration = Duration::from_secs(5);

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
    /// Set when the window never ran at the requested concurrency (thread refused, or async
    /// runtime failed to build): the rig failed to pose the question, which is never a gateway
    /// result.
    pub spawn_failed: bool,
    /// Connections the RIG could not make (EADDRNOTAVAIL from exhausting this host's ephemeral
    /// source ports, or EMFILE/ENFILE), as distinct from requests the gateway refused. Counted
    /// separately so a window that ran out of rig, rather than measuring the gateway, can be
    /// recognised as one.
    pub rig_refused: u64,
    /// Requests a bound of OURS cut short: `RESPONSE_BUDGET` on an exchange, or `CONNECT_BUDGET` on
    /// a connect that never completed. Also counted in `fail`, but kept separable from a genuine
    /// gateway failure. Not `rig_refused`: a connect timeout carries no evidence the host ran out of
    /// anything, only that our five-second bound fired, and `run.rs` treats any `rig_refused` window
    /// as UNMEASURED, which a hung gateway must not become.
    pub budget_exceeded: u64,
    /// Every successful request's latency, microseconds. Kept raw (not a running percentile
    /// estimate) so percentiles are exact rather than approximate.
    pub latencies_us: Vec<u64>,
    /// Precomputed p50/p99. `run()` leaves these `None` (callers with `latencies_us` can call
    /// `pct_us` directly); `run.rs::load_window_at` fills them because raw latencies never cross the
    /// subprocess boundary — only the `k=v` stats line does — and an empty `latencies_us` on this
    /// side would otherwise read as p99=0 instead of "not measured here".
    pub p50_us: Option<u64>,
    pub p99_us: Option<u64>,
}

impl GenStats {
    /// Completed requests per second: fractional below 1/s, truncated to a whole number at or above
    /// it. A plain `as u64` truncates toward zero, so a rate like 0.25/s used to publish as `0` — a
    /// false statement, not a rounding difference. Truncating (not rounding) at/above 1/s keeps
    /// every previously published whole-number rate unchanged; the fix only affects the sub-1/s case
    /// where truncation would otherwise erase the entire answer.
    pub fn rps(&self) -> f64 {
        if self.elapsed_s <= 0.0 {
            return 0.0;
        }
        let exact = self.ok as f64 / self.elapsed_s;
        // Guard against a non-finite rate: casting an infinity to an integer saturates rather than
        // wrapping, which would publish it as a huge but plausible-looking number.
        if !exact.is_finite() {
            return 0.0;
        }
        if exact >= 1.0 {
            exact.trunc()
        } else {
            exact
        }
    }

    /// Nearest-rank percentile via `stats::nearest_rank_index`, shared with the streaming
    /// percentiles so the two conventions cannot drift apart again (ledger SRCH-04).
    pub fn pct_us(&self, q: f64) -> u64 {
        Self::pct_of(&self.sorted_latencies(), q)
    }

    /// Sort once and reuse: `stats_line` needs two percentiles, and cloning the sample vector per
    /// call is wasteful for a long run's millions of samples.
    fn sorted_latencies(&self) -> Vec<u64> {
        let mut v = self.latencies_us.clone();
        v.sort_unstable();
        v
    }

    fn pct_of(v: &[u64], q: f64) -> u64 {
        if v.is_empty() {
            return 0;
        }
        v[crate::stats::nearest_rank_index(v.len(), q)]
    }

    /// The exact line the Go generator prints, so every existing parser reads this unchanged.
    pub fn stats_line(&self) -> String {
        let sorted = self.sorted_latencies();
        let p50 = Self::pct_of(&sorted, 0.50);
        let p99 = Self::pct_of(&sorted, 0.99);
        format!(
            "rps={} fail={} p50={:.2} p99={:.2} p50us={} p99us={} ok={} rigrefused={} budgetexceeded={} spawnfailed={}",
            self.rps(),
            self.fail,
            p50 as f64 / 1000.0,
            p99 as f64 / 1000.0,
            p50,
            p99,
            self.ok,
            // This line is everything the parent process learns from the child, so rig_refused and
            // budget_exceeded must cross it too, or the parent cannot tell its own limits from the
            // gateway's failures.
            self.rig_refused,
            self.budget_exceeded,
            // Likewise spawn_failed: `run.rs` checks `if stats.spawn_failed`, which needs this field
            // on the wire to ever fire.
            u8::from(self.spawn_failed)
        )
    }
}

/// What one connection-holder measured. Owned per task and merged once at the end so the hot path
/// never touches a shared lock (a `Mutex<GenStats>` per exchange would serialise thousands of
/// wakeups under high task concurrency).
#[derive(Default)]
struct WorkerStats {
    ok: u64,
    fail: u64,
    /// Connections this HOST could not make (ports/descriptors exhausted), distinct from requests
    /// the gateway refused. See `GenStats::rig_refused`.
    rig_refused: u64,
    /// Requests a bound of ours cut short (`RESPONSE_BUDGET` or `CONNECT_BUDGET`). Also counted in
    /// `fail`, kept separate so a window failing on our own timeout can be recognised as one.
    budget_exceeded: u64,
    lat: Vec<u64>,
}

impl WorkerStats {
    /// Charge a connect that produced no connection: always a failure, always attributed to which
    /// side caused it (rig, our bound, or the peer).
    fn charge_connect_fault(&mut self, fault: ConnectFault) {
        self.fail += 1;
        match fault {
            ConnectFault::RigExhausted => self.rig_refused += 1,
            ConnectFault::OurConnectBound => self.budget_exceeded += 1,
            ConnectFault::PeerRefused => {}
        }
    }
}

/// Which side a connect attempt that produced no connection belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectFault {
    /// EADDRNOTAVAIL/EADDRINUSE (no ephemeral source port) or EMFILE/ENFILE (no descriptor): this
    /// host ran out, and the gateway was never asked anything.
    RigExhausted,
    /// `CONNECT_BUDGET` elapsed with no answer. We can't tell if the peer is wedged, blackholing, or
    /// behind a full backlog, only that our five-second bound cut off what would have been a
    /// thirty-second wait. Filed as an attributed failure rather than a bare `fail`.
    OurConnectBound,
    /// Refused, unreachable, reset: the peer's own answer, the only one of the three that is the
    /// gateway's own failure.
    PeerRefused,
}

/// Classify a connect that produced no connection. `None` means our own `CONNECT_BUDGET` elapsed
/// rather than the OS answering at all.
fn connect_fault(err: Option<&std::io::Error>) -> ConnectFault {
    let Some(e) = err else {
        return ConnectFault::OurConnectBound;
    };
    let ours = matches!(
        e.kind(),
        std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::AddrInUse
    ) || matches!(e.raw_os_error(), Some(23) | Some(24));
    if ours {
        ConnectFault::RigExhausted
    } else {
        ConnectFault::PeerRefused
    }
}

/// The measured window, so the rate's numerator and denominator describe the same interval.
/// `elapsed_s` is the sleep between `start` and `end`; anything a task completes outside that
/// interval (spawn ramp before `start`, drain after `end`) must not count toward `ok`, or it biases
/// the rate — worst at high concurrency with slow responses. `end` is published before the stop
/// flag, so no task can be past the end without seeing it.
#[derive(Default)]
struct Window {
    start: OnceLock<Instant>,
    end: OnceLock<Instant>,
}

impl Window {
    /// Whether an exchange that completed at `at` belongs to the measured window. Completion, not
    /// start, is what counts: a request is a success when its response arrived.
    fn contains(&self, at: Instant) -> bool {
        match (self.start.get(), self.end.get()) {
            (None, _) => false,
            (Some(s), None) => at >= *s,
            (Some(s), Some(e)) => at >= *s && at <= *e,
        }
    }
}

/// One connection-holder's request loop. Opens a connection and reuses it, reconnecting on failure,
/// because a fresh TCP handshake per request would measure the kernel rather than the gateway.
async fn worker(
    addr: SocketAddr,
    req: Arc<String>,
    stop: Arc<AtomicBool>,
    window: Arc<Window>,
) -> WorkerStats {
    let mut w = WorkerStats {
        ok: 0,
        fail: 0,
        rig_refused: 0,
        budget_exceeded: 0,
        lat: Vec::with_capacity(1024),
    };
    let mut conn: Option<TcpStream> = None;
    // Allocated once and reused (cleared, never replaced, by `read_response`) to keep an allocator
    // call out of the timed hot path.
    let mut acc: Vec<u8> = Vec::with_capacity(8192);

    // Whether the connection about to be used was opened THIS iteration. A peer vanishing on a
    // brand-new connection is a real failure; the same thing on a reused connection is our reuse
    // being stale, not the target's fault.
    let mut fresh;
    while !stop.load(Ordering::Relaxed) {
        fresh = false;
        if conn.is_none() {
            fresh = true;
            // Attribute which side failed: rig out of ports/descriptors, our connect bound, or the
            // peer refusing.
            let fault = match tokio::time::timeout(CONNECT_BUDGET, TcpStream::connect(addr)).await {
                Ok(Ok(s)) => {
                    let _ = s.set_nodelay(true);
                    conn = Some(s);
                    None
                }
                Ok(Err(e)) => Some(connect_fault(Some(&e))),
                Err(_) => Some(connect_fault(None)),
            };
            if let Some(fault) = fault {
                if window.contains(Instant::now()) {
                    w.charge_connect_fault(fault);
                }
                // Back off, or the loop spins at connect-refusal speed once a gateway stops
                // accepting, burning a core and inflating `fail` into a measure of retry speed.
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            }
        }
        let Some(s) = conn.as_mut() else { continue };
        let t0 = Instant::now();
        let response_deadline = t0 + RESPONSE_BUDGET;

        let outcome = if s.write_all(req.as_bytes()).await.is_err() {
            // A write failing on a reused connection is the same stale-connection case as a read
            // seeing nothing: the peer closed it while idle.
            if fresh {
                Exchange::Failed
            } else {
                Exchange::ClosedBeforeAnyBytes
            }
        } else {
            read_response(s, response_deadline, &mut acc).await
        };

        // Whether the exchange counts depends on when it finished; connection bookkeeping happens
        // either way.
        let done = Instant::now();
        let counted = window.contains(done);
        match outcome {
            Exchange::Reusable => {
                if counted {
                    w.lat.push(done.duration_since(t0).as_micros() as u64);
                    w.ok += 1;
                }
            }
            // Answered, and the peer said this was the last exchange on the connection: a success,
            // only the connection is discarded.
            Exchange::LastOnConnection => {
                if counted {
                    w.lat.push(done.duration_since(t0).as_micros() as u64);
                    w.ok += 1;
                }
                conn = None;
            }
            // A stale reused connection: the request never reached a listening peer, so reconnect
            // and resend rather than counting it as any kind of result.
            Exchange::ClosedBeforeAnyBytes if !fresh => {
                conn = None;
            }
            // Out of budget is both a failure the caller would have felt and a bound of ours, so
            // it's counted in both places.
            Exchange::BudgetExceeded => {
                if counted {
                    w.fail += 1;
                    w.budget_exceeded += 1;
                }
                conn = None;
            }
            // The same close on a freshly opened connection is the target refusing to answer.
            Exchange::ClosedBeforeAnyBytes | Exchange::Failed => {
                if counted {
                    w.fail += 1;
                }
                conn = None; // a broken connection is not reused
            }
        }
    }

    w
}

/// Build the one request every task in this window sends, or refuse to build it.
///
/// Headers are interpolated directly into the wire format, so a value carrying CRLF is not a
/// header value at all — it can inject a second header or a second request. Headers arrive from
/// the manifest across a process boundary and must not be able to choose what bytes go on the
/// wire.
///
/// Refused, not sanitised: stripping the CRLF would silently send a different credential/routing
/// key than the manifest specified. `run` turns a refusal into `spawn_failed` — the window never
/// ran, rather than running dishonestly.
fn build_request(cfg: &GenConfig) -> Result<String, String> {
    // Shared with the probe and streaming lanes via `http::unsendable_request` so the injection
    // check can't be applied to one lane and forgotten on another (ledger RIG-12).
    if let Some(why) = crate::http::unsendable_request(&cfg.path, &cfg.headers) {
        return Err(why);
    }
    let mut h = String::new();
    for (k, v) in &cfg.headers {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    Ok(format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{}\r\n{}",
        cfg.path,
        cfg.addr,
        cfg.body.len(),
        h,
        cfg.body
    ))
}

/// The most response body the generator will accumulate for one exchange. Named rather than a bare
/// `1 << 20`. Generous versus what this rig actually sends (replies are around a kilobyte), so
/// exceeding it is a signal worth printing, not a routine truncation.
const RESPONSE_ACC_CAP: usize = 1 << 20;

/// What one request/response exchange did to the connection.
///
/// The generator reuses connections, so a failure on a reused one must be told apart from a
/// failure on a fresh one: attributing a stale-connection close to the target would count a peer
/// that answered correctly and closed as advertised as a failed request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exchange {
    /// A complete response; the connection may carry another request.
    Reusable,
    /// A complete response, and the peer said this is the last one on this connection. A success:
    /// only the connection is discarded.
    LastOnConnection,
    /// The peer went away before a single byte of response arrived. On a reused connection this is
    /// a stale idle connection, not a failed request (it never reached a listening server). On a
    /// fresh connection it is a real failure.
    ClosedBeforeAnyBytes,
    /// A genuine failure: a non-2xx, a malformed head, a truncated body.
    Failed,
    /// The request did not complete inside `RESPONSE_BUDGET`. Still a failure — a gateway that
    /// cannot answer in thirty seconds has failed the caller — but attributed to our own bound
    /// rather than folded into a generic refusal. Should essentially never fire.
    BudgetExceeded,
}

/// Whether the peer announced it will close after this response.
///
/// HTTP/1.0 defaults to close and must opt IN to keep-alive; HTTP/1.1 defaults to keep-alive and
/// opts out via `connection: close`. Getting this backwards makes a well-behaved HTTP/1.0 peer look
/// like it's failing half its requests.
fn peer_will_close(head_lower: &str) -> bool {
    let says = |name: &str| {
        head_lower
            .lines()
            .any(|l| l.starts_with("connection:") && l.contains(name))
    };
    if says("close") {
        return true;
    }
    head_lower.starts_with("http/1.0") && !says("keep-alive")
}

/// Read one response and discard it. Only success/failure matters; the body is the mock's canned
/// reply, and parsing it would charge the gateway for our own JSON cost.
///
/// `acc` is the caller's scratch buffer, reused across requests on this task: cleared here, not
/// replaced, so its capacity survives instead of paying an allocator call per request.
async fn read_response(s: &mut TcpStream, deadline: Instant, acc: &mut Vec<u8>) -> Exchange {
    // A per-read timeout is not a bound: a peer trickling one byte at a time would keep this task
    // alive forever and delay the whole window (`run()` waits on every task). `deadline` is the
    // actual bound.
    let mut buf = [0u8; 8192];
    acc.clear();
    let mut hdr_end: Option<usize> = None;
    // The head is decoded once, when headers complete, and reused; `scanned` tracks how far the
    // chunk terminator search has already looked, so it doesn't rescan the whole body on each read
    // (which would be O(N^2) and charged to the gateway's timed window).
    let mut framing_kind: Option<Framing> = None;
    let mut scanned: usize = 0;
    let mut closing = false;
    let complete = |closing: bool| {
        if closing {
            Exchange::LastOnConnection
        } else {
            Exchange::Reusable
        }
    };
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
                    // Scan only newly-arrived bytes, overlapping by the terminator length so a
                    // terminator split across two reads is still found.
                    let from = scanned.saturating_sub(4).max(he);
                    if acc[from..].windows(5).any(|w| w == b"0\r\n\r\n") {
                        return complete(closing);
                    }
                    scanned = acc.len();
                }
                // Handled by the n == 0 arm below: an until-close body isn't complete until the peer
                // actually closes.
                Framing::UntilClose => {}
            }
        }
        // Bound the read by what's LEFT of the budget, not a fixed per-read timeout — otherwise the
        // real ceiling becomes twice the advertised one.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Exchange::BudgetExceeded;
        }
        let n = match tokio::time::timeout(remaining, s.read(&mut buf)).await {
            // The deadline elapsing is our bound firing, never a stale connection: the peer held a
            // live connection for the whole budget and said nothing. Always attributed here rather
            // than folded into `ClosedBeforeAnyBytes` (invisible on reuse) or a bare failure.
            Err(_) => return Exchange::BudgetExceeded,
            // A read error with nothing received is the peer going away; the caller distinguishes a
            // stale reused connection from a real failure by freshness.
            Ok(Err(_)) if acc.is_empty() => return Exchange::ClosedBeforeAnyBytes,
            Ok(Err(_)) => return Exchange::Failed,
            Ok(Ok(n)) => n,
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
                Some(()) => Exchange::LastOnConnection,
                None => Exchange::Failed,
            };
        }
        acc.extend_from_slice(&buf[..n]);
        // Exceeding our own accumulator cap is our bound, not the gateway's failure — reuse
        // `BudgetExceeded` rather than folding it into a generic `Failed`, which would let an
        // oversized-but-otherwise-fine response silently trip `SweepProbe`'s `fail == 0` gate.
        if acc.len() > RESPONSE_ACC_CAP {
            eprintln!(
                "loadgen: a response exceeded the rig's own {RESPONSE_ACC_CAP}-byte accumulator, so \
                 this exchange was stopped by OUR bound rather than by the gateway"
            );
            return Exchange::BudgetExceeded;
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
    if head_lower
        .lines()
        .any(|l| l.starts_with("transfer-encoding:") && l.contains("chunked"))
    {
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
///
/// Stays a blocking function: `otb loadgen`'s contract with `run.rs::load_window_at` is a
/// subprocess printing one stats line, so async is an implementation detail — the runtime is
/// built, used, and dropped here.
pub fn run(cfg: &GenConfig) -> GenStats {
    // Worker threads default to `available_parallelism`, which honours the CPU affinity mask
    // `taskset -c $LOADCORES` sets, keeping the generator on the cores its published numbers are
    // pinned to.
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            // A window that never ran is not a measurement at any concurrency, same as a refused
            // thread.
            eprintln!("loadgen: could not build the async runtime: {e}");
            return GenStats {
                spawn_failed: true,
                ..Default::default()
            };
        }
    };

    let req = match build_request(cfg) {
        Ok(r) => Arc::new(r),
        // Same class of fact as a runtime that wouldn't build: report as a window that never ran,
        // not as a window of failures charged to the gateway.
        Err(why) => {
            eprintln!("loadgen: refusing to send this window's request: {why}");
            return GenStats {
                spawn_failed: true,
                ..Default::default()
            };
        }
    };
    let addr = cfg.addr;
    let duration = cfg.duration;
    let concurrency = cfg.concurrency.max(1);

    rt.block_on(async move {
        let stop = Arc::new(AtomicBool::new(false));
        let window = Arc::new(Window::default());
        let mut handles = Vec::with_capacity(concurrency as usize);
        for _ in 0..concurrency {
            handles.push(tokio::spawn(worker(
                addr,
                Arc::clone(&req),
                Arc::clone(&stop),
                Arc::clone(&window),
            )));
        }

        // The window is the sleep itself, timed after every task exists: including the spawn ramp
        // or the post-stop drain in `elapsed_s` would bias rps, worst at high concurrency with slow
        // responses. `end` is published before the stop flag so no task can be past it unseen.
        let started = Instant::now();
        let _ = window.start.set(started);
        tokio::time::sleep(duration).await;
        let ended = Instant::now();
        let _ = window.end.set(ended);
        stop.store(true, Ordering::Relaxed);
        let load_elapsed = ended.duration_since(started);

        let mut g = GenStats {
            elapsed_s: load_elapsed.as_secs_f64(),
            ..Default::default()
        };
        for h in handles {
            match h.await {
                Ok(w) => {
                    g.ok += w.ok;
                    g.fail += w.fail;
                    g.rig_refused += w.rig_refused;
                    g.budget_exceeded += w.budget_exceeded;
                    g.latencies_us.extend_from_slice(&w.lat);
                }
                // A panicked task is a harness fault (release profile aborts on panic, so this is
                // debug-build-only); log it rather than silently shrinking `ok`.
                Err(e) => eprintln!("loadgen: a connection task did not finish cleanly: {e}"),
            }
        }
        g
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // Test peers are plain blocking servers on their own threads, standing in for a gateway
    // independent of the generator's own async shape.
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn stats(lat: &[u64], ok: u64, fail: u64, elapsed: f64) -> GenStats {
        GenStats {
            ok,
            fail,
            elapsed_s: elapsed,
            latencies_us: lat.to_vec(),
            spawn_failed: false,
            rig_refused: 0,
            budget_exceeded: 0,
            p50_us: None,
            p99_us: None,
        }
    }

    // Pins the engine-wide nearest-rank percentile convention (not this file's own): this engine
    // once shipped a one-index disagreement between the load generator (floor) and streaming
    // percentiles (ceil) while both claimed to agree (ledger SRCH-04).
    #[test]
    fn percentiles_are_nearest_rank_on_the_engines_one_convention() {
        let s = stats(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100], 10, 0, 1.0);
        assert_eq!(
            s.pct_us(0.50),
            50,
            "ceil(10*0.5)=5 -> index 4 -> the 5th value"
        );
        assert_eq!(s.pct_us(0.0), 10);
        assert_eq!(
            s.pct_us(1.0),
            100,
            "q=1.0 clamps to the last index rather than overflowing"
        );
        // Every rank goes through the one function, so this cannot drift back apart.
        for q in [0.0, 0.5, 0.9, 0.95, 0.99, 1.0] {
            let sorted = s.sorted_latencies();
            assert_eq!(
                s.pct_us(q),
                sorted[crate::stats::nearest_rank_index(sorted.len(), q)]
            );
        }
        // Over a hundred samples - the count the streaming legs take - a p99 must not be the max.
        let big: Vec<u64> = (1..=100).collect();
        let s100 = stats(&big, 100, 0, 1.0);
        assert_eq!(s100.pct_us(0.99), 99);
        assert_eq!(s100.pct_us(0.50), 50);
        assert!(
            s100.pct_us(0.99) < s100.pct_us(1.0),
            "a p99 that equals the maximum is the worst sample wearing a percentile's name"
        );
    }

    #[test]
    fn an_empty_sample_has_no_percentile_rather_than_a_wrong_one() {
        assert_eq!(stats(&[], 0, 0, 1.0).pct_us(0.99), 0);
    }

    #[test]
    fn rps_is_successes_over_measured_elapsed_not_nominal_duration() {
        assert_eq!(stats(&[1], 1000, 0, 10.0).rps(), 100.0);
        assert_eq!(
            stats(&[1], 1000, 0, 0.0).rps(),
            0.0,
            "no elapsed time is no rate, not a division"
        );
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
        assert!(
            parsed.is_measured(),
            "our own stats line must parse: {line}"
        );
    }

    #[test]
    fn a_failed_request_counts_as_a_failure_and_contributes_no_latency() {
        let s = stats(&[], 0, 5, 1.0);
        assert_eq!(s.fail, 5);
        assert!(
            s.latencies_us.is_empty(),
            "a failure has no latency to report"
        );
        assert_eq!(s.rps(), 0.0, "failures are not throughput");
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
        let r = build_request(&cfg).expect("a well-formed header list must build");
        assert!(r.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(r.contains("x-one: 1\r\n") && r.contains("x-two: 2\r\n"));
        assert!(r.ends_with(r#"{"a":1}"#));
        assert!(
            r.contains("content-length: 7\r\n"),
            "length must match the body exactly"
        );
    }

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
                        if c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                            .is_err()
                        {
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
        assert_eq!(
            g.latencies_us.len() as u64,
            g.ok,
            "every success contributes exactly one latency"
        );
        assert!(g.elapsed_s > 0.0);
        assert!(g.rps() > 0.0, "a run that completed requests has a rate");
    }

    // A reader that stops at the header terminator instead of draining a chunked body would leave
    // bytes on the socket, corrupting the next request on the reused connection.
    #[test]
    fn a_chunked_response_is_drained_so_the_next_request_is_not_corrupted() {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming().take(8) {
                let Ok(mut c) = c else { continue };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    // Chunked on every request, so a non-draining reader desyncs on the second one.
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
        assert!(
            g.ok > 1,
            "chunked responses must complete, got ok={} fail={}",
            g.ok,
            g.fail
        );
        assert_eq!(
            g.fail, 0,
            "a drained connection must not manufacture failures, got {}",
            g.fail
        );
    }

    // HTTP/1.0 defaults to closing after each response (must opt IN to keep-alive). Reusing a
    // connection the peer said it was closing must not be counted as the target's failure, or
    // throughput reads as roughly half and a correct peer fails the clean-window gate.
    #[test]
    fn a_peer_that_answers_and_closes_is_all_successes_and_no_failures() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                std::thread::spawn(move || {
                    // Answer in HTTP/1.0 with no keep-alive, then close: what a plain HTTP/1.0
                    // server does.
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

        assert!(
            stats.ok > 0,
            "the peer answers every request, so there must be successes"
        );
        assert_eq!(
            stats.fail, 0,
            "a peer closing a connection it said it would close is not a failed request: ok={} fail={}",
            stats.ok, stats.fail
        );
    }

    // The other half of the same rule: a peer that closes WITHOUT answering must still count as a
    // failure — e.g. a container that binds its port and then dies at config load.
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
        assert!(
            stats.fail > 0,
            "a peer that never answers must be recorded as failing"
        );
    }

    // A chunked terminator split across many small reads must still be found without rescanning the
    // whole body each time (O(N^2), charged to the timed window).
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
                        if conn
                            .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n")
                            .is_err()
                        {
                            return;
                        }
                        // Terminator written one byte at a time so it straddles read boundaries.
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
        assert!(
            stats.ok > 0,
            "a fragmented chunked body must complete, ok={} fail={}",
            stats.ok,
            stats.fail
        );
        assert_eq!(
            stats.fail, 0,
            "a terminator split across reads must not read as a failure"
        );
    }

    // No length header and no chunking means run-to-close framing, not a zero-length body.
    #[test]
    fn a_response_with_no_framing_header_is_not_treated_as_an_empty_body() {
        assert!(matches!(
            framing("http/1.1 200 ok\r\n"),
            Framing::UntilClose
        ));
        assert!(matches!(
            framing("http/1.1 200 ok\r\ncontent-length: 12\r\n"),
            Framing::Length(12)
        ));
        assert!(matches!(
            framing("http/1.1 200 ok\r\ntransfer-encoding: chunked\r\n"),
            Framing::Chunked
        ));
    }

    // A non-2xx is a failure, not throughput.
    #[test]
    fn a_non_2xx_response_is_a_failure_not_throughput() {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming().take(64) {
                let Ok(mut c) = c else { continue };
                let mut b = [0u8; 4096];
                let _ = c.read(&mut b);
                let _ =
                    c.write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n");
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

    // Pins the point of the async rewrite: 2048 concurrent holders must be cheap tasks, not OS
    // threads (which would start losing to the scheduler at this count on a CI runner). Asserts
    // completion, not rate — proving the instrument can hold the connections it claims.
    #[test]
    fn a_high_concurrency_window_is_held_by_tasks_rather_than_os_threads() {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    while c.read(&mut b).unwrap_or(0) > 0 {
                        if c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });

        let started = Instant::now();
        let g = run(&GenConfig {
            addr,
            path: "/x".into(),
            body: "{}".into(),
            headers: vec![],
            concurrency: 2048,
            duration: Duration::from_millis(500),
            ttft_ms: 0,
        });
        let wall = started.elapsed();

        assert!(
            g.ok > 0,
            "2048 concurrent holders must still complete requests, ok={} fail={}",
            g.ok,
            g.fail
        );
        // Generous bound: a regression guard against wall-clock collapse, not a performance
        // assertion.
        assert!(
            wall < Duration::from_secs(20),
            "a 500ms window at c=2048 took {wall:?}; the holders are not cheap"
        );
    }

    // rig_refused and budget_exceeded must cross the subprocess boundary in the stats line, or the
    // parent can't tell a rig limit from a genuine gateway failure.
    #[test]
    fn the_stats_line_carries_our_own_limits_apart_from_the_gateways_failures() {
        let g = GenStats {
            ok: 100,
            fail: 7,
            elapsed_s: 1.0,
            spawn_failed: false,
            rig_refused: 3,
            budget_exceeded: 2,
            latencies_us: vec![1000, 2000, 3000],
            p50_us: None,
            p99_us: None,
        };
        let line = g.stats_line();
        assert!(
            line.contains("rigrefused=3"),
            "our port/descriptor exhaustion must cross the wire: {line}"
        );
        assert!(
            line.contains("budgetexceeded=2"),
            "our response budget must cross the wire: {line}"
        );
        assert!(
            line.contains("fail=7"),
            "and the gateway's own failure count is unchanged: {line}"
        );

        let parsed = crate::loadgen::parse_ugen_line(&line)
            .into_value()
            .expect("a line this generator wrote must parse");
        assert_eq!(parsed.rig_refused, 3);
        assert_eq!(parsed.budget_exceeded, 2);
        assert_eq!(parsed.fail, 7);

        // An older generator's line lacks these fields; absent must mean "not reported", not "did
        // not happen", and must still parse.
        let old = "rps=1000 fail=1 p50=1.00 p99=2.00 p50us=1000 p99us=2000 ok=999";
        let legacy = crate::loadgen::parse_ugen_line(old)
            .into_value()
            .expect("an older line must still parse");
        assert_eq!(legacy.rig_refused, 0);
        assert_eq!(legacy.budget_exceeded, 0);
        assert_eq!(legacy.ok, 999);
    }

    // A read timeout must report as `BudgetExceeded` on every connection, not silently retried as
    // `ClosedBeforeAnyBytes` (which used to make a hung gateway show up only as depressed rps).
    #[test]
    fn a_read_timeout_on_a_live_connection_is_our_budget_firing_not_a_stale_connection() {
        let hangs = TcpListener::bind("127.0.0.1:0").expect("bind");
        let hang_addr = hangs.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in hangs.incoming() {
                let Ok(c) = c else { continue };
                // Accept, say nothing, hold the connection open.
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(5));
                    drop(c);
                });
            }
        });

        let closes = TcpListener::bind("127.0.0.1:0").expect("bind");
        let close_addr = closes.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in closes.incoming() {
                drop(c); // a peer going away, not a peer hanging
            }
        });

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime to drive one exchange");
        rt.block_on(async move {
            let mut acc: Vec<u8> = Vec::new();
            let mut hung = TcpStream::connect(hang_addr).await.expect("connect");
            let out = read_response(
                &mut hung,
                Instant::now() + Duration::from_millis(150),
                &mut acc,
            )
            .await;
            assert_eq!(
                out,
                Exchange::BudgetExceeded,
                "a peer holding a live connection past the deadline is our bound firing, got {out:?}"
            );

            // A peer that goes away with nothing sent is still the stale-connection retry case, not
            // a failure.
            let mut gone = TcpStream::connect(close_addr).await.expect("connect");
            let out = read_response(
                &mut gone,
                Instant::now() + Duration::from_secs(30),
                &mut acc,
            )
            .await;
            assert_eq!(
                out,
                Exchange::ClosedBeforeAnyBytes,
                "a peer that closed without answering is not our budget, got {out:?}"
            );
        });
    }

    // The five-second connect timeout is filed as our own bound firing, not `rig_refused`
    // (EADDRNOTAVAIL/EMFILE) or a bare `fail`: `run.rs` treats any `rig_refused` window as
    // UNMEASURED, so misfiling a hung gateway there would erase the failure it caused.
    #[test]
    fn a_connect_timeout_is_charged_to_our_own_bound_never_to_rig_exhaustion() {
        assert_eq!(connect_fault(None), ConnectFault::OurConnectBound);
        assert_eq!(
            connect_fault(Some(&std::io::Error::from_raw_os_error(24))),
            ConnectFault::RigExhausted,
            "EMFILE is this host out of descriptors"
        );
        assert_eq!(
            connect_fault(Some(&std::io::Error::from(
                std::io::ErrorKind::AddrNotAvailable
            ))),
            ConnectFault::RigExhausted,
            "EADDRNOTAVAIL is this host out of ephemeral ports"
        );
        assert_eq!(
            connect_fault(Some(&std::io::Error::from(
                std::io::ErrorKind::ConnectionRefused
            ))),
            ConnectFault::PeerRefused
        );

        let charged = |f: ConnectFault| {
            let mut w = WorkerStats::default();
            w.charge_connect_fault(f);
            (w.fail, w.rig_refused, w.budget_exceeded)
        };
        assert_eq!(
            charged(ConnectFault::OurConnectBound),
            (1, 0, 1),
            "a connect timeout is a failure the caller felt AND our bound, never rig exhaustion"
        );
        assert_eq!(charged(ConnectFault::RigExhausted), (1, 1, 0));
        assert_eq!(
            charged(ConnectFault::PeerRefused),
            (1, 0, 0),
            "a refusal is the gateway's alone"
        );
    }

    // The rate's numerator and denominator must describe the same window: a response landing during
    // the post-stop drain must not count into `ok` while `elapsed_s` stays frozen at the sleep.
    #[test]
    fn responses_that_land_after_the_window_closes_are_not_counted_into_its_rate() {
        let served = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let counter = Arc::clone(&served);
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                let counter = Arc::clone(&counter);
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    if c.read(&mut b).unwrap_or(0) == 0 {
                        return;
                    }
                    counter.fetch_add(1, Ordering::Relaxed);
                    // Answer well after the load window has closed.
                    std::thread::sleep(Duration::from_millis(700));
                    let _ = c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
                });
            }
        });

        let g = run(&GenConfig {
            addr,
            path: "/x".into(),
            body: "{}".into(),
            headers: vec![],
            concurrency: 4,
            duration: Duration::from_millis(200),
            ttft_ms: 0,
        });

        assert!(
            served.load(Ordering::Relaxed) > 0,
            "the window must actually have driven requests, or this proves nothing"
        );
        assert_eq!(
            g.ok, 0,
            "no response arrived inside the window, so none of them is throughput it sustained"
        );
        assert!(
            g.latencies_us.is_empty(),
            "a completion outside the window contributes no latency either"
        );
        assert_eq!(g.rps(), 0.0);
    }

    // A header value must not be able to choose what bytes go on the wire: a CRLF appends a header
    // the manifest never wrote, and a double CRLF starts a second request on the connection.
    #[test]
    fn a_header_carrying_crlf_is_refused_rather_than_smuggled_onto_the_wire() {
        let with = |headers: Vec<(String, String)>, path: &str| GenConfig {
            addr: "127.0.0.1:1".parse().expect("literal"),
            path: path.into(),
            body: r#"{"a":1}"#.into(),
            headers,
            concurrency: 1,
            duration: Duration::from_millis(1),
            ttft_ms: 0,
        };

        for (name, value) in [
            ("authorization", "Bearer t\r\nx-injected: yes"),
            ("authorization", "Bearer t\r\n\r\nPOST /other HTTP/1.1"),
            ("authorization", "Bearer t\nx-injected: yes"),
            ("x-route\r\nx-injected", "yes"),
            ("x-route: x", "yes"),
        ] {
            let cfg = with(vec![(name.into(), value.into())], "/v1/chat/completions");
            let err = build_request(&cfg)
                .expect_err("a header that would inject must not build a request");
            assert!(
                err.contains("inject"),
                "the refusal must name the defect: {err}"
            );
        }

        // The request line is the same wire format and the same manifest source.
        assert!(build_request(&with(vec![], "/v1/chat HTTP/1.1\r\nx: y")).is_err());

        // And an ordinary header list still builds, so the rule is about line breaks rather than
        // about anything a real credential contains.
        let ok = build_request(&with(
            vec![("authorization".into(), "Bearer sk-abc.DEF_123-+/=".into())],
            "/v1/chat/completions",
        ))
        .expect("a real credential must still send");
        assert!(ok.contains("authorization: Bearer sk-abc.DEF_123-+/=\r\n"));
    }

    // A window built from an injecting manifest never runs (`spawn_failed`), rather than sanitising
    // the header or charging the gateway for our refusal.
    #[test]
    fn a_window_whose_request_would_inject_never_runs_and_charges_the_gateway_nothing() {
        let g = run(&GenConfig {
            addr: "127.0.0.1:1".parse().expect("literal"),
            path: "/x".into(),
            body: "{}".into(),
            headers: vec![("x-route".into(), "a\r\nx-injected: b".into())],
            concurrency: 2,
            duration: Duration::from_millis(50),
            ttft_ms: 0,
        });
        assert!(g.spawn_failed, "the rig could not pose the question");
        assert_eq!((g.ok, g.fail), (0, 0), "and nothing is charged to anyone");
    }
}

#[cfg(test)]
mod rate_precision_tests {
    use super::*;

    fn s(ok: u64, elapsed_s: f64) -> GenStats {
        GenStats {
            ok,
            elapsed_s,
            ..Default::default()
        }
    }

    // The case that used to publish as zero: `as u64` truncation turned 0.25 req/s into `0`, which
    // says "carried nothing" rather than "slow".
    #[test]
    fn a_sub_one_per_second_rate_is_not_published_as_zero() {
        let r = s(1, 4.0).rps();
        assert!(
            (r - 0.25).abs() < 1e-9,
            "one request in four seconds is 0.25/s, got {r}"
        );
        assert!(
            r > 0.0,
            "a completed request must never publish as a zero rate"
        );
        assert!((s(1, 2.0).rps() - 0.5).abs() < 1e-9);
        assert!((s(3, 4.0).rps() - 0.75).abs() < 1e-9);
    }

    // At or above 1/s, nothing moves: truncating (not rounding) keeps every previously published
    // rate unchanged.
    #[test]
    fn rates_at_or_above_one_per_second_are_unchanged_whole_numbers() {
        assert_eq!(s(100, 1.0).rps(), 100.0);
        assert_eq!(s(44_363, 1.0).rps(), 44_363.0);
        // 19.9/s must still publish 19, exactly as `as u64` did - not 20.
        assert_eq!(
            s(199, 10.0).rps(),
            19.0,
            "truncation, not rounding, above 1/s"
        );
        assert_eq!(
            s(1, 1.0).rps(),
            1.0,
            "exactly 1/s is the boundary and stays whole"
        );
    }

    // A rate is undefined without elapsed time; an infinity cast to an integer would saturate
    // rather than wrap, publishing a huge bogus number.
    #[test]
    fn a_rate_with_no_elapsed_time_is_zero_not_an_infinity() {
        assert_eq!(s(10, 0.0).rps(), 0.0);
        assert_eq!(s(0, 0.0).rps(), 0.0);
    }
}
