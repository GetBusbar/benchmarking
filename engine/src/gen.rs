// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The load generator, in Rust. `otb loadgen` (this module) is run as a subprocess by `run.rs`'s
// `load_window`.
//
// It prints a stats line (`rps=<f64> fail=%d ... p50us=%d ...`) that `engine/src/loadgen.rs::
// parse_ugen_line` parses on the other end; the two must stay in the same shape. `rps` is a FLOAT,
// not a count: below 1/s the rate is fractional and printing it as an integer would send `rps=0`
// for a window that carried requests. See `UgenStats::rps` for the full reasoning.
//
// ONE ASYNC TASK PER UNIT OF CONCURRENCY, NOT ONE OS THREAD.
//
// This module used to say "std only, and threads rather than async on purpose ... an async runtime
// would add a scheduler between the measurement and the clock for no benefit AT THIS CONCURRENCY".
// That last clause was the load-bearing one, and the concurrency search outgrew it. A thread per
// connection means the instrument cannot honour the number it is asked for: a field run sweeping
// toward 32k held tens of thousands of native threads on the six cores `taskset` pins this process
// to, sat at a 1-minute load average over 24,000 for 45 minutes, and never converged. Every probe
// past that point measured the rig's own scheduler thrashing rather than the gateway, and the
// search had no way to tell the two apart - it just kept climbing. A task costs a few KB against a
// thread's full stack, so the ramp reaches a real ceiling instead of collapsing into one.
//
// The dependency bar the old comment set ("every dependency here has to be audited before anyone
// trusts the numbers") is MET, not waived: the mock - the reference instrument every published
// number is judged against, and the thing whose throughput decides whether a result is suppressed
// as mock-bound - has always run on tokio. This adds no crate the measurement path did not already
// rest on.
//
// What did NOT change: the wire handling. Connection reuse, the fresh-vs-reused failure
// attribution, HTTP framing, the response budget and the non-2xx rule are the same logic, because
// those are what the published numbers mean.

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
    /// NOT SENT. Nothing in `build_request` reads this, so the mock is never told to pace itself and a
    /// non-zero value changes nothing about the window; every caller passes 0. Kept only because the
    /// struct is built by callers outside this module - do not set it expecting a delayed upstream.
    pub ttft_ms: u32,
}

#[derive(Debug, Default, Clone)]
pub struct GenStats {
    pub ok: u64,
    pub fail: u64,
    pub elapsed_s: f64,
    /// Set when the window never ran at the requested concurrency, so it is not a measurement of the
    /// gateway at any concurrency we could name. Under threads that meant the OS refusing a thread;
    /// under tasks it means the runtime itself could not be built, which is the same class of fact:
    /// the rig failed to pose the question, and that is never a gateway result.
    pub spawn_failed: bool,
    /// Connections the RIG could not make, as distinct from requests the gateway refused.
    ///
    /// A TCP connection needs a unique (src ip, src port, dst ip, dst port). Every window here talks
    /// to ONE destination, so simultaneous connections are bounded by this host's ephemeral source
    /// ports - `net.ipv4.ip_local_port_range`, which defaults to about 28,000 - and running out
    /// raises EADDRNOTAVAIL on connect. That is our limit, not the gateway's, and it used to land in
    /// `fail` beside a genuine refusal where nothing downstream could tell them apart: the search
    /// would read its own port exhaustion as the gateway's ceiling and publish it.
    ///
    /// Counted separately so a window that ran out of rig can be recognised as one. EMFILE/ENFILE
    /// (out of file descriptors) are the same class and counted here too.
    pub rig_refused: u64,
    /// Requests a bound of OURS cut short: `RESPONSE_BUDGET` on an exchange, or `CONNECT_BUDGET` on
    /// a connect that never completed. Also counted in `fail`, because a caller waiting thirty
    /// seconds got nothing - but separable, so a window failing for our reason cannot look identical
    /// to one failing for the gateway's.
    ///
    /// A CONNECT TIMEOUT IS NOT `rig_refused`. `rig_refused` is the claim "this host had no
    /// ephemeral port or descriptor left", and `run.rs` treats a window containing any of those as
    /// UNMEASURED, so filing a hung gateway there would erase the very failure it caused. A connect
    /// timeout carries no evidence about which side ran out; what is certainly true is that our
    /// five-second bound fired, which is exactly what this counter says.
    pub budget_exceeded: u64,
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
    /// Completed requests per second. A WHOLE NUMBER AT OR ABOVE 1/s, AND FRACTIONAL BELOW IT.
    ///
    /// This returned `u64` via `(ok as f64 / elapsed_s) as u64`, which truncates toward zero - so a
    /// rung that completed one request in four seconds published `0`. That is not a rounding
    /// difference, it is a false statement: `0` says the gateway carried nothing when it carried
    /// 0.25/s. plano hit it twice (c=256 on two cells, `rps: 0` sitting beside a `p99_us` of 3.4 s),
    /// and both external auditors had to carry a documented special case - "a percentile cannot exist
    /// without a completed request, so the rate merely rounded down" - to avoid calling a correct
    /// engine wrong.
    ///
    /// THE SPLIT IS DELIBERATE, rather than simply returning the exact float everywhere. Below 1/s,
    /// truncation destroys the entire magnitude of the answer, so precision there is the whole point.
    /// At or above 1/s, `trunc()` reproduces every number this engine has ever published EXACTLY -
    /// so the change cannot quietly move a single existing figure while fixing the one case that was
    /// wrong, and 44,363.7 req/s is not made truer by carrying a tenth.
    ///
    /// Sub-1/s rates are not a curiosity. They are what a gateway collapsing under concurrency looks
    /// like, which is precisely where the board must not round the evidence away to zero.
    pub fn rps(&self) -> f64 {
        if self.elapsed_s <= 0.0 {
            return 0.0;
        }
        let exact = self.ok as f64 / self.elapsed_s;
        // A non-finite rate is the rig malfunctioning, and `as i64` on an infinity SATURATES rather
        // than wrapping - publishing 9,223,372,036,854,775,807 req/s as though it were a measurement.
        // Guarded the same way `stats` now guards its order statistics.
        if !exact.is_finite() {
            return 0.0;
        }
        if exact >= 1.0 {
            exact.trunc()
        } else {
            exact
        }
    }

    /// Nearest-rank percentile, through `stats::nearest_rank_index` so this cannot drift from the
    /// streaming percentiles it is published beside: a convention that differs by one index from
    /// what a reader of the published numbers assumes is a silent disagreement, not a rounding
    /// difference, and this engine shipped exactly that disagreement (ledger SRCH-04) until the
    /// rank moved into one function.
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
            // ACROSS THE SUBPROCESS BOUNDARY TOO. The load generator is a child process and this
            // line is everything the parent learns from it, so a count that stops here would leave
            // the parent unable to tell its own port exhaustion from the gateway refusing - which is
            // the whole reason the count exists.
            self.rig_refused,
            self.budget_exceeded,
            // AND `spawn_failed`, for the same reason as the two above. The OS refusing a thread means
            // the window never ran at the concurrency it claims - a RIG limit the parent's search must
            // stop on rather than read as a turnover. The parent had a check for it
            // (`if stats.spawn_failed` in run.rs) that could NEVER fire on this path: the flag stopped
            // at this boundary and the parent hardcoded it to false, so a child that could not spawn
            // its threads reported a perfectly ordinary window.
            u8::from(self.spawn_failed)
        )
    }
}

/// What one connection-holder measured. Owned per task and merged once at the end, so the hot path
/// never touches a shared lock: under a thread per connection a single `Mutex<GenStats>` was only
/// contended at join time, but with tasks the same pattern would serialise thousands of wakeups.
#[derive(Default)]
struct WorkerStats {
    ok: u64,
    fail: u64,
    /// Connections this HOST could not make (ephemeral ports or descriptors exhausted), as opposed
    /// to requests the gateway refused. See `GenStats::rig_refused`.
    rig_refused: u64,
    /// Requests a bound of ours cut short (`RESPONSE_BUDGET` or `CONNECT_BUDGET`). Counted as
    /// failures AND counted here, so a window whose failures are really our own timeout can be
    /// recognised as one.
    budget_exceeded: u64,
    lat: Vec<u64>,
}

impl WorkerStats {
    /// Charge a connect that produced no connection. Always a failure - nothing was answered - and
    /// always attributed, because "our port range ran out", "our connect bound fired" and "the peer
    /// refused" are three different claims that used to arrive as one number.
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
    /// EADDRNOTAVAIL/EADDRINUSE (no ephemeral source port) or EMFILE/ENFILE (no descriptor): THIS
    /// HOST ran out, and the gateway was never asked anything.
    RigExhausted,
    /// `CONNECT_BUDGET` elapsed with no answer. Ours in the same sense `RESPONSE_BUDGET` is: the
    /// peer may be wedged, may be blackholing SYNs, may be behind a full accept backlog, and this
    /// side cannot tell - but it CAN say that the thirty seconds a caller would have waited was cut
    /// at five by us. Filed as a failure with that attribution rather than as a bare `fail`, which
    /// used to read exactly like a refusal.
    OurConnectBound,
    /// Refused, unreachable, reset: the peer's answer, and the only one of the three that is the
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

/// THE MEASURED WINDOW, so the rate's numerator and its denominator describe the same interval.
///
/// `elapsed_s` is the sleep between these two instants. Everything a task did outside them used to
/// land in `ok` anyway: the spawn ramp before `start`, and - the one that matters - the drain after
/// `end`, where every lane's in-flight response completed and was counted against a denominator that
/// had already stopped. At high concurrency with slow responses that is one extra success per lane
/// for free, which biases the peak search toward exactly the high rungs it is climbing toward.
///
/// Published as the two instants happen, never guessed: `start` once every task exists, `end` when
/// the sleep returns and BEFORE the stop flag is set, so no task can be past the end without being
/// able to see it.
#[derive(Default)]
struct Window {
    start: OnceLock<Instant>,
    end: OnceLock<Instant>,
}

impl Window {
    /// Whether an exchange that COMPLETED at `at` belongs to the measured window. Completion, not
    /// start: a request is a success when its response arrived, and that is the instant the rate
    /// counts.
    fn contains(&self, at: Instant) -> bool {
        match (self.start.get(), self.end.get()) {
            // The clock has not started, so this is the spawn ramp: real work, but outside the
            // interval `elapsed_s` measures, and counting it inflates the same rate from the other
            // end.
            (None, _) => false,
            // The window is open. `end` is published before the stop flag, so a task that has not
            // seen an end has not passed one.
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
    // ALLOCATED ONCE, reused across every request this task sends: a fresh Vec per response would
    // put an allocator call in the timed hot path of every exchange, for a task that runs for the
    // whole window at whatever RPS the sweep is driving. `read_response` clears it, never replaces it.
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
            // Both a connect error and a connect timeout mean no connection, and neither is a
            // request the target answered. WHICH SIDE ran out matters, though, and all three
            // answers - the rig out of ports/descriptors, our own connect bound firing, the peer
            // refusing - used to arrive as one undifferentiated `fail`.
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
                // BACK OFF. Without this the loop spins at connect-refusal speed (microseconds on
                // loopback) once a gateway stops accepting, burning a core and inflating `fail`
                // into a measure of how fast connect can fail rather than a request count.
                tokio::time::sleep(Duration::from_millis(5)).await;
                continue;
            }
        }
        let Some(s) = conn.as_mut() else { continue };
        let t0 = Instant::now();
        let response_deadline = t0 + RESPONSE_BUDGET;

        let outcome = if s.write_all(req.as_bytes()).await.is_err() {
            // A write that fails on a REUSED connection is the same stale-connection case as a read
            // that sees nothing: the peer closed it while it sat idle.
            if fresh {
                Exchange::Failed
            } else {
                Exchange::ClosedBeforeAnyBytes
            }
        } else {
            read_response(s, response_deadline, &mut acc).await
        };

        // WHEN the exchange finished decides whether it is part of the measurement, and the
        // connection bookkeeping happens either way: a broken connection is still broken after the
        // window closes.
        let done = Instant::now();
        let counted = window.contains(done);
        match outcome {
            Exchange::Reusable => {
                if counted {
                    w.lat.push(done.duration_since(t0).as_micros() as u64);
                    w.ok += 1;
                }
            }
            // ANSWERED, and the peer said that was the last one on this connection. A success: the
            // target did exactly what it advertised. Only the connection is discarded.
            Exchange::LastOnConnection => {
                if counted {
                    w.lat.push(done.duration_since(t0).as_micros() as u64);
                    w.ok += 1;
                }
                conn = None;
            }
            // A stale connection we chose to reuse. The request never reached a listening peer, so
            // it is neither a success nor the target's failure: reconnect and send it again. Not
            // counted, and no latency recorded, because nothing was measured.
            Exchange::ClosedBeforeAnyBytes if !fresh => {
                conn = None;
            }
            // Out of budget is a failure the caller would have felt, and also a bound of ours, so it
            // is counted in both places rather than hidden in one.
            Exchange::BudgetExceeded => {
                if counted {
                    w.fail += 1;
                    w.budget_exceeded += 1;
                }
                conn = None;
            }
            // The same thing on a connection opened moments ago is the target refusing to answer.
            Exchange::ClosedBeforeAnyBytes | Exchange::Failed => {
                if counted {
                    w.fail += 1;
                }
                conn = None; // a broken connection is not reused: the next request would inherit its state
            }
        }
    }

    w
}

/// Build the one request every task in this window sends, or refuse to build it.
///
/// A HEADER IS INTERPOLATED INTO THE WIRE FORMAT, so a value carrying CRLF is not a header value at
/// all: `x-route: a\r\nx-other: b` puts a second header on the wire, and `\r\n\r\n` ends the head
/// and makes the rest of the value a second request on the connection. The header list comes across
/// a process boundary from the manifest (`loadgen::decode_headers`), which is exactly the kind of
/// input that must not be able to choose what bytes this generator sends.
///
/// REFUSED, NOT SANITISED. Stripping the CRLF would send a header the manifest did not write -
/// silently a different credential or a different routing key - and publish the resulting numbers
/// under the pairing the manifest asked for. A window that cannot pose the question honestly does
/// not run: `run` turns this into `spawn_failed`, the same "the rig never asked" answer a runtime
/// that would not build gets.
fn build_request(cfg: &GenConfig) -> Result<String, String> {
    // ONE RULE, SHARED WITH THE PROBE AND STREAMING LANES. This check used to live here, spelled
    // out, and only here: `http::send` and `http::build_sse_request` interpolated the same
    // manifest-supplied path and headers raw, so the probe, streaming and re-verify lanes stayed
    // injectable while this one was not (ledger RIG-12). A rule enforced on one of three lanes is a
    // lane a reader thinks is covered, so it moved to `http::unsendable_request` and all three call
    // it.
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

/// The most response body the generator will accumulate for one exchange.
///
/// A rig bound, and named so it can be reasoned about: it was a bare `1 << 20` buried in the read
/// loop. Generous by three orders of magnitude against what this rig actually asks for - every body
/// it sends caps the response at a handful of tokens, so the mock's replies are around a kilobyte -
/// which is what makes exceeding it a signal worth printing rather than a routine truncation.
const RESPONSE_ACC_CAP: usize = 1 << 20;

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
    /// A genuine failure: a non-2xx, a malformed head, a truncated body.
    Failed,
    /// The request did not complete inside `RESPONSE_BUDGET`.
    ///
    /// STILL A FAILURE - a gateway that cannot answer one request in thirty seconds has failed any
    /// caller that was waiting - but a failure whose bound is OURS, and the artifact has to be able
    /// to say so. Every other rig limit that ever bound was invisible for exactly this reason: it
    /// landed in the same counter as a genuine refusal and nothing downstream could separate them.
    /// The budget is roughly a thousand times the slowest gateway this field has measured, so this
    /// should never fire; "should never fire" is what was true of the ephemeral port range until it
    /// did.
    BudgetExceeded,
}

/// Whether the peer announced it will close after this response.
///
/// HTTP/1.0 defaults to close and must opt IN to keep-alive; HTTP/1.1 defaults to keep-alive and
/// opts out with `connection: close`. Getting that default backwards is what makes a well-behaved
/// HTTP/1.0 peer look like it is failing half its requests.
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

/// Read one response and discard it. Only success or failure matters here; the body is the mock's
/// canned reply and parsing it would charge the gateway for our own JSON cost.
///
/// `acc` is the caller's scratch buffer, reused across every request on this task rather than
/// allocated fresh per call: cleared here, not replaced, so its capacity survives from one exchange
/// to the next instead of paying an allocator call inside the timed hot path of every request.
async fn read_response(s: &mut TcpStream, deadline: Instant, acc: &mut Vec<u8>) -> Exchange {
    // A PER-READ TIMEOUT IS NOT A BOUND. A peer that trickles one byte at a time keeps a task
    // inside this function effectively forever, and `run()` waits on every task before it can
    // report, so one wedged task delays the whole window. This deadline is the bound.
    let mut buf = [0u8; 8192];
    acc.clear();
    let mut hdr_end: Option<usize> = None;
    // Parsed ONCE when the headers complete, then reused. Re-decoding and re-lowercasing the head
    // on every read put an allocation per read inside the timed window, which is charged to the
    // gateway. `scanned` is where the chunked parse resumes (the next unparsed chunk-size line):
    // rescanning the whole body on each read is O(N^2) in the number of reads, and chunked is the normal framing for a
    // gateway that streams, so this was the worst case rather than the rare one.
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
                    // PARSED BY CHUNK SIZE, not by searching for `0\r\n\r\n`. A byte search ended
                    // the response early on a chunk whose DATA held that sequence (leaving the rest on
                    // a connection about to be reused, so the next exchange read body bytes as its
                    // head), and never ended one whose last chunk carried trailers or an extension
                    // (`0\r\nX: y\r\n\r\n`, `0;ext\r\n\r\n`), so a correct gateway waited out the
                    // whole budget. `scanned` is where the next unparsed chunk-size line starts, so
                    // each read resumes there instead of rescanning the body.
                    match scan_chunks(acc, scanned.max(he)) {
                        ChunkScan::Complete => return complete(closing),
                        ChunkScan::NeedMore(at) => scanned = at,
                        ChunkScan::Malformed => return Exchange::Failed,
                    }
                }
                // An until-close body is only complete once the peer actually closes, which the
                // n == 0 arm below handles. Returning here would call a body complete the moment the
                // headers landed and charge the gateway nothing for sending it.
                Framing::UntilClose => {}
            }
        }
        // Bound the read by what is LEFT of the budget. A fixed per-read timeout on top of a
        // deadline check makes the real ceiling twice the advertised one: the check passes at
        // deadline-minus-a-moment, then the read blocks for its own full timeout.
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Exchange::BudgetExceeded;
        }
        let n = match tokio::time::timeout(remaining, s.read(&mut buf)).await {
            // THE DEADLINE ELAPSED, and that is our bound firing, never a stale connection. This
            // read is bounded by what is left of `RESPONSE_BUDGET`, so `Err` here means the peer
            // held a live connection for the whole budget and said nothing - a gateway that hangs.
            // Folding it into `ClosedBeforeAnyBytes` made it invisible on a reused connection (the
            // caller retries that case silently: no ok, no fail, nothing but depressed rps) and a
            // bare `fail` on a fresh one, where "our thirty seconds fired" read exactly like a
            // refusal. A timeout is always accounted and always attributed to the bound that fired.
            Err(_) => return Exchange::BudgetExceeded,
            // A read ERROR with nothing received is the peer going away, which the caller reads by
            // freshness: on a reused connection it is a stale connection of ours, on a fresh one it
            // is a failure.
            Ok(Err(_)) if acc.is_empty() => return Exchange::ClosedBeforeAnyBytes,
            Ok(Err(_)) => return Exchange::Failed,
            Ok(Ok(n)) => n,
        };
        if n == 0 {
            if acc.is_empty() {
                return Exchange::ClosedBeforeAnyBytes;
            }
            // A closed connection completes an until-close body and truncates any other framing.
            // The framing decided when the headers completed is the one used here: a second parse of
            // the head would be a second place the rule could drift, and an allocation in the window.
            return match framing_kind {
                // The peer closed, so there is no connection left to reuse either way.
                Some(Framing::UntilClose) => Exchange::LastOnConnection,
                _ => Exchange::Failed,
            };
        }
        acc.extend_from_slice(&buf[..n]);
        // OUR BUFFER BOUND IS OURS, NOT THE GATEWAY'S FAILURE.
        //
        // This was a bare `1 << 20` returning `Exchange::Failed`: an unnamed constant, no message,
        // and counted in `fail` alongside a genuine non-2xx or a malformed head. Every other bound in
        // this module is deliberately kept apart from a gateway failure - `rig_refused` and
        // `budget_exceeded` exist for exactly that, because folding "our own limit fired" into "the
        // gateway failed" is how a rig's ephemeral port range once got published as a gateway's
        // ceiling. A response over the cap would have been indistinguishable from a broken gateway,
        // and because `SweepProbe` requires `fail == 0` for a clean window it could discard an
        // otherwise-good rung and understate that gateway's throughput with no diagnostic anywhere.
        //
        // `BudgetExceeded` is the existing seam for "a bound WE set stopped this exchange", and it is
        // already counted separately from `fail`, so this needs no new variant. Loud, because a
        // silent bound is the defect and not the size.
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

/// Where a chunked body stands after the bytes received so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkScan {
    /// The last chunk and its trailer section have arrived.
    Complete,
    /// More bytes are needed; the next chunk-size line starts at this offset.
    NeedMore(usize),
    /// Not chunked framing (a size that is not hex, a chunk not followed by CRLF): a broken response.
    Malformed,
}

/// Walk a chunked body (RFC 9112 section 7.1) from the chunk-size line starting at `at`.
///
/// Each chunk is `<hex size>[;extensions]\r\n<size bytes>\r\n`; the last chunk has size 0 and is
/// followed by optional trailer fields and an empty line. Only sizes are read - the data itself is
/// skipped, never searched - so no byte sequence inside a chunk can end the body.
fn scan_chunks(acc: &[u8], mut at: usize) -> ChunkScan {
    loop {
        let Some(line_len) = find_crlf(&acc[at..]) else {
            return ChunkScan::NeedMore(at);
        };
        let line = &acc[at..at + line_len];
        let size_field = line.split(|b| *b == b';').next().unwrap_or(line);
        let Some(size) = std::str::from_utf8(size_field)
            .ok()
            .map(|t| t.trim_matches([' ', '\t']))
            .filter(|t| !t.is_empty())
            .and_then(|t| usize::from_str_radix(t, 16).ok())
        else {
            return ChunkScan::Malformed;
        };
        let data = at + line_len + 2;
        if size == 0 {
            // The trailer section: zero or more field lines, then an empty line.
            let mut t = data;
            loop {
                let Some(n) = find_crlf(&acc[t..]) else {
                    return ChunkScan::NeedMore(at);
                };
                if n == 0 {
                    return ChunkScan::Complete;
                }
                t += n + 2;
            }
        }
        let Some(end) = data.checked_add(size).and_then(|e| e.checked_add(2)) else {
            return ChunkScan::Malformed;
        };
        if acc.len() < end {
            return ChunkScan::NeedMore(at);
        }
        if acc[end - 2..end] != *b"\r\n" {
            return ChunkScan::Malformed;
        }
        at = end;
    }
}

/// Offset of the first CRLF in `b`, if any.
fn find_crlf(b: &[u8]) -> Option<usize> {
    b.windows(2).position(|w| w == b"\r\n")
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
/// Stays a BLOCKING function: `otb loadgen`'s contract with `run.rs::load_window_at` is a
/// subprocess that prints one stats line, and nothing above this call is async. The runtime is
/// built here, used, and dropped, so async is an implementation detail of the generator rather than
/// something the engine has to adopt.
pub fn run(cfg: &GenConfig) -> GenStats {
    // Worker threads default to `available_parallelism`, which honours the CPU affinity mask
    // `taskset -c $LOADCORES` sets, so the generator still runs on exactly the cores it is pinned
    // to. That pinning is the comparability basis of every published number.
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            // The rig could not pose the question at all. Reported the same way a refused thread
            // was: a window that never ran is not a measurement of the gateway at any concurrency.
            eprintln!("loadgen: could not build the async runtime: {e}");
            return GenStats {
                spawn_failed: true,
                ..Default::default()
            };
        }
    };

    let req = match build_request(cfg) {
        Ok(r) => Arc::new(r),
        // The rig could not pose the question honestly, which is the same class of fact as a runtime
        // it could not build: reported as a window that never ran, never as a window of failures the
        // gateway would be charged with.
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

        // THE WINDOW IS THE SLEEP, and it starts once every task exists.
        //
        // The thread version started this clock BEFORE spawning and stopped it AFTER joining, so
        // the spawn ramp and the post-stop drain both landed in the denominator - which deflated
        // rps hardest at exactly the high rungs where spawning was slowest, biasing the search
        // against the concurrencies it was climbing toward. Spawning tasks is cheap enough that the
        // ramp is no longer a meaningful share of the window, and timing the sleep alone is what
        // the original comment always said this measured.
        // AND THE SAME WINDOW IS WHAT COUNTS. `start` opens the interval the tasks credit their
        // completions to; `end` closes it, published BEFORE the stop flag so no task can be past the
        // end without being able to see it. Everything that completes in the drain after `end` is
        // real work that finished outside the interval `elapsed_s` measures, and counting it into
        // `ok` raised rps by one free success per lane - worst at high concurrency with slow
        // responses, which is precisely where the peak search is looking.
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
                // A task that panicked is a HARNESS fault, and its requests are gone. Say so on
                // stderr rather than folding a silent hole into a published rate: the release
                // profile aborts on panic, so this is reachable only in a debug build, and a
                // quietly smaller `ok` would read as the gateway being slower.
                //
                // AND MARK THE WINDOW. A lost lane means the window did not run at the concurrency
                // it reports, which is exactly what `spawn_failed` tells the parent; a stderr line
                // alone left a normal-looking stats line to be scored as the gateway's result.
                Err(e) => {
                    eprintln!("loadgen: a connection task did not finish cleanly: {e}");
                    g.spawn_failed = true;
                }
            }
        }
        g
    })
}

#[cfg(test)]
#[path = "tests/gen_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/gen_rate_precision_tests.rs"]
mod rate_precision_tests;
