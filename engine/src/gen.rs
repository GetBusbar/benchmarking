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
    /// Time the upstream takes to first byte, mirrored into the request so the mock paces itself.
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
            // The window is open. `end` is published before the stop flag, so a task that has already
            // seen the stop flag has necessarily seen the end too. BOUNDED IMPRECISION, NOT zero: a
            // worker whose completion `at` lands in the narrow gap AFTER the main task read `ended`
            // but BEFORE `window.end.set(ended)` becomes visible still observes `(Some, None)` and is
            // counted, even though its timestamp is logically just past the end. The window is
            // microseconds wide and the effect is at most a handful of extra `ok`s per lane; it is
            // accepted by design rather than closed with a fence on the hot path (which would perturb
            // the very rate this measures).
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
    // gateway. `scanned` is how far the terminator search has already looked: rescanning the whole
    // body on each read is O(N^2) in the number of reads, and chunked is the normal framing for a
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
        // completions to; `end` closes it, published BEFORE the stop flag so any task that has seen
        // the stop has also seen the end (a task past the end is bounded-racy only in the microsecond
        // gap before this `set` is visible - see `Window::contains`). Everything that completes in the
        // drain after `end` is
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
                Err(e) => eprintln!("loadgen: a connection task did not finish cleanly: {e}"),
            }
        }
        g
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // The test peers below are plain blocking servers on their own threads: they stand in for a
    // gateway, and holding them to the generator's own async shape would test the harness twice
    // instead of testing it against something independent.
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

    // THE PERCENTILE CONVENTION IS THE ENGINE'S, NOT THIS FILE'S. A one-index difference is a silent
    // disagreement between two instruments that both look correct, and this engine shipped one
    // (ledger SRCH-04): the load generator resolved a rank with floor while the streaming
    // percentiles used ceil, with comments in both files claiming they agreed.
    //
    // The floor convention this test used to pin is exactly what makes the case below wrong: it put
    // p50 at index 5 of ten (the SIXTH value, above the middle) and p99 at index 9 (the MAXIMUM),
    // and the assertion literally read "the last value". A percentile that is the maximum is not a
    // percentile, which `metric.rs`'s own test asserted in the same crate.
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
        // Measured elapsed, because a run that took longer than asked must not report the rate it
        // would have had. The Go generator makes the same choice.
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
        assert!(
            g.ok > 1,
            "chunked responses must complete, got ok={} fail={}",
            g.ok,
            g.fail
        );
        // The real tell: with an undrained body the connection desyncs and every request after the
        // first is misread as a non-2xx, so failures would dominate.
        assert_eq!(
            g.fail, 0,
            "a drained connection must not manufacture failures, got {}",
            g.fail
        );
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
        assert!(
            stats.fail > 0,
            "a peer that never answers must be recorded as failing"
        );
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
                        if conn
                            .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n")
                            .is_err()
                        {
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

    // A body with no length header and no chunking runs to connection close. That is a legitimate
    // framing, NOT a zero-length body, and it must not be confused with one.
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

    // THE POINT OF THE REWRITE, pinned as a test: a concurrency that a thread-per-connection
    // generator could not honour on a small box must now cost tasks, not OS threads. 2048 native
    // threads on a CI runner is where the old design started losing to its own scheduler; 2048
    // tasks is unremarkable. The assertion is deliberately about COMPLETION, not about a rate: this
    // proves the instrument can hold the connections it claims, which is exactly what the failed
    // field run disproved for threads.
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
        // The old failure mode was wall clock exploding far past the requested window because
        // spawning and joining the holders dwarfed the window itself. A generous bound: this is a
        // regression guard against collapse, not a performance assertion.
        assert!(
            wall < Duration::from_secs(20),
            "a 500ms window at c=2048 took {wall:?}; the holders are not cheap"
        );
    }

    // A RIG LIMIT MUST NEVER BE INDISTINGUISHABLE FROM A GATEWAY FAILURE.
    //
    // Both of these used to land in `fail` beside a genuine refusal, where nothing downstream could
    // separate them - which is how the search came to publish this host's ephemeral port range as a
    // gateway's ceiling. They are counted apart now, and the stats line carries them across the
    // subprocess boundary because that line is everything the parent learns from the child.
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

        // The parent reads them back.
        let parsed = crate::loadgen::parse_ugen_line(&line)
            .into_value()
            .expect("a line this generator wrote must parse");
        assert_eq!(parsed.rig_refused, 3);
        assert_eq!(parsed.budget_exceeded, 2);
        assert_eq!(parsed.fail, 7);

        // A line from an older generator has neither field and must still parse: absent means "not
        // reported", which is not the same as "did not happen", and refusing an otherwise complete
        // line would turn a cosmetic version skew into a lost measurement.
        let old = "rps=1000 fail=1 p50=1.00 p99=2.00 p50us=1000 p99us=2000 ok=999";
        let legacy = crate::loadgen::parse_ugen_line(old)
            .into_value()
            .expect("an older line must still parse");
        assert_eq!(legacy.rig_refused, 0);
        assert_eq!(legacy.budget_exceeded, 0);
        assert_eq!(legacy.ok, 999);
    }

    // A GATEWAY THAT ACCEPTS AND THEN HANGS MUST BE VISIBLE.
    //
    // A read timeout used to be reported as `ClosedBeforeAnyBytes`, which the worker retries in
    // silence on a reused connection - no ok, no fail, no budget count, so a hung gateway showed up
    // only as mysteriously low rps - and counted as a bare `fail` on a fresh one, where our own
    // thirty-second bound firing read exactly like the gateway refusing. The deadline passing is our
    // bound, on every connection, and it says so.
    #[test]
    fn a_read_timeout_on_a_live_connection_is_our_budget_firing_not_a_stale_connection() {
        let hangs = TcpListener::bind("127.0.0.1:0").expect("bind");
        let hang_addr = hangs.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in hangs.incoming() {
                let Ok(c) = c else { continue };
                // Accept, then say nothing and hold the connection open.
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
                drop(c); // accept and close at once: a peer going away, not a peer hanging
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

            // The other half of the rule, unchanged: a peer that GOES AWAY with nothing sent is
            // still the stale-connection case the worker retries on a reused connection, so this fix
            // must not turn every idle keep-alive close into a failure.
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

    // OUR CONNECT BOUND IS NOT THE GATEWAY REFUSING, AND IT IS NOT THE RIG RUNNING OUT EITHER.
    //
    // The five-second connect timeout used to land in `fail` with no attribution at all, beside
    // EADDRNOTAVAIL/EMFILE which do get `rig_refused`. It is filed as our bound firing:
    // `rig_refused` is the claim "this host had no port or descriptor left", and run.rs turns a
    // window containing any of those into an UNMEASURED one - so filing a hung gateway there would
    // erase the failure it caused.
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

    // THE RATE'S NUMERATOR AND DENOMINATOR MUST DESCRIBE THE SAME WINDOW.
    //
    // Every lane's in-flight response used to complete during the post-stop drain and count into
    // `ok` while `elapsed_s` stayed frozen at the sleep - one free success per lane, largest exactly
    // where the peak search is climbing (high concurrency, slow responses). Here nothing can
    // possibly answer inside the window, so an honest window reports nothing rather than a rate
    // built entirely out of the drain.
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
                    // Answer well after the load window has closed: a real response, delivered
                    // during the drain.
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

    // A HEADER VALUE IS NOT ALLOWED TO CHOOSE WHAT BYTES GO ON THE WIRE.
    //
    // Header values arrive from the manifest across a process boundary, and they are interpolated
    // straight into the request head: a CRLF in one appends a header the manifest never wrote, and a
    // double CRLF ends the head and makes the rest a second request on the connection.
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

    // And the window built from such a manifest NEVER RUNS: a sanitised header would send a
    // credential or a routing key the manifest did not write and publish the result under the
    // pairing it asked for, while a window of failures would charge the gateway for our refusal.
    // `spawn_failed` is the existing "the rig never posed the question" answer.
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

    // THE CASE THAT WAS PUBLISHED AS ZERO. plano hit it twice: one request completed in four seconds
    // is 0.25 req/s, and `as u64` truncation published `0` - which does not say "slow", it says the
    // gateway carried NOTHING. Both external auditors had to carry a written special case to avoid
    // calling a correct engine wrong.
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

    // AND THE OTHER HALF: nothing at or above 1/s moves. The split exists precisely so this change
    // cannot quietly restate a single number the board has already published - if it rounded instead
    // of truncating, every rate on the board could shift by one.
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

    // A rate is undefined without elapsed time, and an infinity cast to an integer SATURATES rather
    // than wrapping - which would publish 9,223,372,036,854,775,807 req/s as a measurement.
    #[test]
    fn a_rate_with_no_elapsed_time_is_zero_not_an_infinity() {
        assert_eq!(s(10, 0.0).rps(), 0.0);
        assert_eq!(s(0, 0.0).rps(), 0.0);
    }
}
