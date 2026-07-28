// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// A minimal HTTP/1.1 client over std::net::TcpStream, sufficient for what this harness actually
// does and nothing more: every probe is a plain JSON POST to 127.0.0.1.
//
// NO EXTERNAL DEPENDENCIES, on purpose. An HTTP stack would drag an async runtime and dozens of
// transitive crates into a statically-shipped binary whose whole job is producing numbers people
// are asked to trust, and whose own scheduler could perturb the latency this harness is
// simultaneously measuring. std is enough for one host, one dialect, no TLS, no redirects.
//
// THE DISTINCTION THIS FILE EXISTS TO PRESERVE. probe.rs classifies a persistent-transient probe
// on `Observation.status: Option<u16>`: `None` means the gateway may never have been reached,
// `Some(status)` means it answered. Collapsing "connection refused" and "the gateway sent a 503"
// into the same shape would let a rig failure (nobody listening on the port) get published as a
// gateway verdict. `Outcome` keeps a real response, a connection failure, a timeout and a
// malformed/partial response as four distinct things so that distinction survives past this file.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// A header, as sent or received. Kept as a plain pair list (never a map) so duplicate header
/// names survive: some dialects distinguish repeated headers from a single comma-joined one, and a
/// map would silently collapse them before the caller ever saw that they differed.
pub type Headers = Vec<(String, String)>;

/// A response the peer actually produced: it accepted the connection, sent a status line, and (at
/// least started to) send a body. Whether that status is 200 or 503 is irrelevant here; both are
/// evidence ABOUT THE GATEWAY. Sorting a 5xx into "no response" is exactly the bug this type
/// prevents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    headers: Headers,
    body: Vec<u8>,
}

impl HttpResponse {
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// First header matching `name`, case-insensitively (HTTP header names are case-insensitive).
    /// Used to tell a JSON body from an event-stream body without guessing from the bytes.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
}

/// The result of one `post_json` call. Never reached, timed out, malformed, and answered are four
/// outcomes, not shades of one "it failed" outcome: a caller (probe.rs) needs to tell "the gateway
/// was never reached" apart from "the gateway answered with a 5xx", and a curl-style `000` collapses
/// exactly that distinction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A complete, well-formed HTTP response, whatever its status.
    Response(HttpResponse),
    /// The TCP connection itself could not be made or was severed before any response existed.
    /// The gateway may genuinely never have seen the request; this must never carry a status.
    ConnectionFailed(String),
    /// The deadline passed with nothing usable read yet. Distinct from `ConnectionFailed`: the
    /// connection was live, the peer just never (yet) answered. A hung gateway must not hang the
    /// suite, so this is always reached within the caller's timeout, never later.
    TimedOut,
    /// Something was read, but it was not a complete, parseable HTTP response: a garbled status
    /// line, a stream that closed mid-header, a chunk length that will not parse. This is not a
    /// success and must never be read as one; the bytes actually seen travel with it for a human
    /// to inspect, because throwing them away is how a rig defect masquerades as a clean failure.
    Malformed { seen: Vec<u8>, message: String },
}

/// Bytes that behave as an EOF as far as `Outcome` classification is concerned: fewer bytes were
/// available than requested.
enum ReadOutcome {
    Full(Vec<u8>),
    Eof(Vec<u8>),
    TimedOut(Vec<u8>),
    Err(io::Error),
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// Reads one line (through and including the trailing `\n`, if any) a byte at a time, honouring an
/// absolute wall-clock `deadline` rather than a fixed per-call timeout. One byte at a time is slow
/// for a general HTTP client; for a control-plane probe that reads a few hundred bytes it is
/// immaterial, and it makes "how many bytes had we seen when this went wrong" exact rather than
/// approximate, which matters for `Malformed`.
fn read_line(stream: &mut TcpStream, deadline: Instant) -> ReadOutcome {
    let mut buf = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return ReadOutcome::TimedOut(buf);
        }
        if stream.set_read_timeout(Some(remaining)).is_err() {
            return ReadOutcome::TimedOut(buf);
        }
        let mut byte = [0u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => return ReadOutcome::Eof(buf),
            Ok(_) => {
                buf.push(byte[0]);
                if buf.ends_with(b"\n") {
                    return ReadOutcome::Full(buf);
                }
                // A line with no bound would let a peer that never sends "\n" grow this
                // unboundedly; a probe response has no legitimate reason to have a header or
                // status line this long, so treat it as malformed rather than as an allocator.
                if buf.len() > 64 * 1024 {
                    return ReadOutcome::Eof(buf);
                }
            }
            Err(e) => {
                if is_timeout(&e) {
                    return ReadOutcome::TimedOut(buf);
                }
                return ReadOutcome::Err(e);
            }
        }
    }
}

/// Hard ceiling on any response body we will accumulate from the gateway under test.
///
/// The gateway is arbitrary third-party software and its response length is ITS claim, not ours. An
/// allocation failure calls abort() unconditionally: not a panic, nothing catches it, and an
/// eight-hour run dies with no operator watching. A probe response has no legitimate reason to
/// approach this, so exceeding it is Malformed rather than a measurement.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Reads exactly `n` bytes (a known Content-Length or chunk body), honouring `deadline`.
fn read_exact_deadline(stream: &mut TcpStream, deadline: Instant, n: usize) -> ReadOutcome {
    // NEVER RESERVE WHAT THE PEER ASKED FOR. `n` arrives from the gateway under test, as a
    // Content-Length header or a chunk-size line. A declared length of usize::MAX makes
    // Vec::reserve panic on capacity overflow, and a merely enormous one reaches the allocator,
    // whose failure handler calls abort() unconditionally: not a panic, so nothing can catch it,
    // and the eight-hour run dies with no operator watching. The one component we are measuring is
    // the one we must not trust with our address space. Grow as bytes actually arrive instead, and
    // let the existing body cap reject an over-long response as malformed.
    let mut buf: Vec<u8> = Vec::with_capacity(n.min(64 * 1024));
    if n > MAX_BODY_BYTES {
        return ReadOutcome::Err(io::Error::other(format!(
            "declared body of {n} bytes exceeds the {MAX_BODY_BYTES} byte cap"
        )));
    }
    while buf.len() < n {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return ReadOutcome::TimedOut(buf);
        }
        if stream.set_read_timeout(Some(remaining)).is_err() {
            return ReadOutcome::TimedOut(buf);
        }
        let mut chunk = vec![0u8; (n - buf.len()).min(64 * 1024)];
        match stream.read(&mut chunk) {
            Ok(0) => return ReadOutcome::Eof(buf),
            Ok(read) => buf.extend_from_slice(&chunk[..read]),
            Err(e) => {
                if is_timeout(&e) {
                    return ReadOutcome::TimedOut(buf);
                }
                return ReadOutcome::Err(e);
            }
        }
    }
    ReadOutcome::Full(buf)
}

/// Reads bytes until the peer closes the connection (used for a body with neither
/// Content-Length nor chunked framing, signalled purely by the close), honouring `deadline`.
fn read_to_close(stream: &mut TcpStream, deadline: Instant) -> ReadOutcome {
    let mut buf = Vec::new();
    loop {
        // THE CAP APPLIES HERE TOO. This framing has no declared length to check up front - the
        // peer just streams until it closes the connection, or doesn't - so MAX_BODY_BYTES must be
        // enforced against what has actually accumulated instead, checking only the declared
        // Content-Length path would let this loop and the chunked one below grow without limit for
        // as long as the deadline allowed: on loopback, tens of seconds is enough to reach
        // gigabytes. The allocator's failure handler calls abort() unconditionally, so that is not a
        // panic this harness can catch - it is the eight-hour run dying outright, and it is exactly
        // what MAX_BODY_BYTES exists to prevent.
        if buf.len() > MAX_BODY_BYTES {
            return ReadOutcome::Err(io::Error::other(format!(
                "close-delimited body exceeded the {MAX_BODY_BYTES} byte cap before the peer closed the connection"
            )));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return ReadOutcome::TimedOut(buf);
        }
        if stream.set_read_timeout(Some(remaining)).is_err() {
            return ReadOutcome::TimedOut(buf);
        }
        let mut chunk = [0u8; 64 * 1024];
        match stream.read(&mut chunk) {
            Ok(0) => return ReadOutcome::Full(buf),
            Ok(read) => buf.extend_from_slice(&chunk[..read]),
            Err(e) => {
                if is_timeout(&e) {
                    return ReadOutcome::TimedOut(buf);
                }
                return ReadOutcome::Err(e);
            }
        }
    }
}

fn malformed(seen: &[u8], message: impl Into<String>) -> Outcome {
    Outcome::Malformed {
        seen: seen.to_vec(),
        message: message.into(),
    }
}

fn strip_crlf(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// Parses a status line of the form "HTTP/1.1 200 OK". Returns the status code, or `None` if the
/// line does not look like a status line at all (used to build a `Malformed` message that names
/// what was actually seen instead of just saying "bad status line").
fn parse_status_line(line: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(strip_crlf(line)).ok()?;
    let mut parts = text.splitn(3, ' ');
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    let code = parts.next()?;
    code.parse::<u16>().ok()
}

fn parse_header_line(line: &[u8]) -> Option<(String, String)> {
    let text = std::str::from_utf8(strip_crlf(line)).ok()?;
    let (name, value) = text.split_once(':')?;
    Some((name.trim().to_string(), value.trim().to_string()))
}

fn header_value<'a>(headers: &'a Headers, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Reads the status line and headers off `stream`. On success, returns the parsed status and
/// headers plus the raw bytes consumed so far (needed by the caller to report `Malformed` with
/// what was actually seen if the body then goes wrong). On failure, returns an `Outcome` already
/// fully formed, since every failure path here has a specific, distinct meaning.
fn read_head(
    stream: &mut TcpStream,
    deadline: Instant,
) -> Result<(u16, Headers, Vec<u8>), Outcome> {
    let mut raw = Vec::new();
    let status_line = match read_line(stream, deadline) {
        ReadOutcome::Full(line) => line,
        ReadOutcome::TimedOut(partial) if partial.is_empty() => return Err(Outcome::TimedOut),
        ReadOutcome::TimedOut(partial) => {
            return Err(malformed(
                &partial,
                "timed out while reading the status line",
            ))
        }
        // The peer accepted the connection and then closed it without a byte of response. That is
        // reachable-but-silent, not "never reached": the connection succeeded.
        ReadOutcome::Eof(partial) if partial.is_empty() => {
            return Err(malformed(
                &partial,
                "connection closed before any response was sent",
            ))
        }
        ReadOutcome::Eof(partial) => {
            return Err(malformed(&partial, "connection closed mid status line"))
        }
        ReadOutcome::Err(e) => {
            return Err(malformed(&[], format!("error reading status line: {e}")))
        }
    };
    raw.extend_from_slice(&status_line);
    let status = match parse_status_line(&status_line) {
        Some(s) => s,
        None => {
            return Err(malformed(
                &raw,
                "status line did not parse as HTTP/x.y CODE ...",
            ))
        }
    };

    let mut headers = Headers::new();
    loop {
        let line = match read_line(stream, deadline) {
            ReadOutcome::Full(line) => line,
            ReadOutcome::TimedOut(partial) => {
                raw.extend_from_slice(&partial);
                return Err(malformed(&raw, "timed out while reading headers"));
            }
            ReadOutcome::Eof(partial) => {
                raw.extend_from_slice(&partial);
                return Err(malformed(&raw, "connection closed mid headers"));
            }
            ReadOutcome::Err(e) => {
                return Err(malformed(&raw, format!("error reading headers: {e}")))
            }
        };
        raw.extend_from_slice(&line);
        let stripped = strip_crlf(&line);
        if stripped.is_empty() {
            break;
        }
        // obs-fold (RFC 7230 3.2.4): a line starting with SP/HTAB continues the PREVIOUS header's
        // value rather than starting a new one. Obsolete, but legal, and real front ends (some
        // proxies wrapping a long Location or a multi-line Warning) still emit it; a continuation
        // line does not fit parse_header_line's "name: value" shape, so it must be folded here
        // rather than dropped, or that header's value would be silently truncated.
        if stripped.first().is_some_and(|b| *b == b' ' || *b == b'\t') {
            if let Some(last) = headers.last_mut() {
                let cont = String::from_utf8_lossy(stripped).trim().to_string();
                if !cont.is_empty() {
                    last.1.push(' ');
                    last.1.push_str(&cont);
                }
            }
            // A continuation with no prior header (a malformed lead line) has nothing to fold onto;
            // tolerated the same as any other unparseable head line.
            continue;
        }
        if let Some((name, value)) = parse_header_line(&line) {
            if name.eq_ignore_ascii_case("content-length") {
                if let Some(existing) = header_value(&headers, "content-length") {
                    if existing != value {
                        return Err(malformed(
                            &raw,
                            format!(
                                "conflicting Content-Length headers: {existing:?} and {value:?}"
                            ),
                        ));
                    }
                    // Identical duplicates are tolerated (some servers double-send the same value);
                    // only a genuine mismatch is a smuggling-relevant error.
                    continue;
                }
            }
            headers.push((name, value));
        }
        // Any other unparseable header line is tolerated (skipped): a stray informational line here
        // should not sink a response that otherwise has a perfectly good status and body.
    }
    Ok((status, headers, raw))
}

/// Reads a chunked (`Transfer-Encoding: chunked`) body to completion.
fn read_chunked_body(stream: &mut TcpStream, deadline: Instant, raw: &[u8]) -> Outcome {
    let mut body = Vec::new();
    let mut seen = raw.to_vec();
    let mut chunk_count: u64 = 0;
    loop {
        let size_line = match read_line(stream, deadline) {
            ReadOutcome::Full(line) => line,
            ReadOutcome::TimedOut(partial) => {
                seen.extend_from_slice(&partial);
                return malformed(&seen, "timed out reading a chunk size");
            }
            ReadOutcome::Eof(partial) => {
                seen.extend_from_slice(&partial);
                return malformed(&seen, "connection closed mid chunked body");
            }
            ReadOutcome::Err(e) => {
                return malformed(&seen, format!("error reading chunk size: {e}"))
            }
        };
        seen.extend_from_slice(&size_line);
        let size_text = std::str::from_utf8(strip_crlf(&size_line)).unwrap_or("");
        // Chunk extensions ("1a;foo=bar") are legal; only the hex size before ';' matters here.
        let size_hex = size_text.split(';').next().unwrap_or("").trim();
        let size = match usize::from_str_radix(size_hex, 16) {
            Ok(n) => n,
            Err(_) => return malformed(&seen, format!("unparseable chunk size {size_hex:?}")),
        };
        if size == 0 {
            // Trailing headers (rare, usually absent) run up to the final blank line.
            loop {
                match read_line(stream, deadline) {
                    ReadOutcome::Full(line) => {
                        seen.extend_from_slice(&line);
                        if strip_crlf(&line).is_empty() {
                            break;
                        }
                    }
                    ReadOutcome::TimedOut(partial) => {
                        seen.extend_from_slice(&partial);
                        return malformed(&seen, "timed out reading chunked trailer");
                    }
                    ReadOutcome::Eof(partial) => {
                        seen.extend_from_slice(&partial);
                        return malformed(&seen, "connection closed mid chunked trailer");
                    }
                    ReadOutcome::Err(e) => {
                        return malformed(&seen, format!("error reading chunked trailer: {e}"))
                    }
                }
            }
            return Outcome::Response(HttpResponse {
                status: 0,
                headers: Headers::new(),
                body,
            });
        }
        let chunk = match read_exact_deadline(stream, deadline, size) {
            ReadOutcome::Full(bytes) => bytes,
            ReadOutcome::TimedOut(partial) => {
                seen.extend_from_slice(&partial);
                body.extend_from_slice(&partial);
                return malformed(&seen, "timed out reading a chunk body");
            }
            ReadOutcome::Eof(partial) => {
                seen.extend_from_slice(&partial);
                body.extend_from_slice(&partial);
                return malformed(&seen, "connection closed mid chunk body");
            }
            ReadOutcome::Err(e) => {
                return malformed(&seen, format!("error reading chunk body: {e}"))
            }
        };
        seen.extend_from_slice(&chunk);
        body.extend_from_slice(&chunk);
        chunk_count += 1;
        // THE AGGREGATE, not just each chunk. read_exact_deadline above already rejects any SINGLE
        // chunk declared larger than MAX_BODY_BYTES, but nothing stopped an unbounded NUMBER of
        // legally-sized chunks: a peer sending chunks just under the cap, back to back, for as long
        // as the deadline allows, grew `body` without limit. Checked after appending so the final
        // over-cap chunk is still visible in the Malformed evidence.
        if body.len() > MAX_BODY_BYTES {
            return malformed(
                &seen,
                format!("chunked body exceeded the {MAX_BODY_BYTES} byte cap across {chunk_count} chunk(s)"),
            );
        }
        // Each chunk body is followed by a bare CRLF before the next chunk size line.
        match read_line(stream, deadline) {
            ReadOutcome::Full(_) => {}
            ReadOutcome::TimedOut(partial) => {
                seen.extend_from_slice(&partial);
                return malformed(&seen, "timed out reading chunk trailer CRLF");
            }
            ReadOutcome::Eof(partial) => {
                seen.extend_from_slice(&partial);
                return malformed(&seen, "connection closed after a chunk body");
            }
            ReadOutcome::Err(e) => return malformed(&seen, format!("error after chunk body: {e}")),
        }
    }
}

/// POSTs `body` as the request payload to `path` on `addr`, with `headers` sent verbatim
/// (duplicates and all: this function does not deduplicate or reorder what the caller supplies).
/// `timeout` bounds the whole call: connect, write, and read together, so a gateway that accepts a
/// connection and never answers cannot hang the caller past it.
pub fn post_json(
    addr: SocketAddr,
    path: &str,
    body: &[u8],
    headers: &[(String, String)],
    timeout: Duration,
) -> Outcome {
    send("POST", addr, path, body, headers, timeout, true)
}

/// The same client, issuing a GET with no body.
///
/// EXISTS FOR THE MOCK'S OWN CONTROL PLANE, not for the gateways: `/__mock/state` is the only thing
/// this harness reads with a GET, and it is the evidence behind the egress re-verification verdict
/// (see `reverify.rs`). It goes through the same `Outcome` discipline as every POST rather than a
/// second, looser reader, because "the mock could not be reached" and "the mock answered, and its
/// recorder is empty" are the two answers that verdict turns on, and a client that collapsed them
/// would publish a rig failure as proof a gateway did not translate.
///
/// No `content-type` is sent: a GET with no body has no type to declare, and `post_json`'s default
/// exists for the opposite reason (a gateway that 415s a typeless JSON body).
pub fn get(addr: SocketAddr, path: &str, headers: &[(String, String)], timeout: Duration) -> Outcome {
    send("GET", addr, path, &[], headers, timeout, false)
}

/// One request/response exchange. Shared by `post_json` and `get` so the two cannot drift in their
/// framing, deadline handling, or `Outcome` classification - the distinctions this module's header
/// describes are the whole point of it, and a second hand-rolled request builder is a second place
/// they can be lost.
fn send(
    method: &str,
    addr: SocketAddr,
    path: &str,
    body: &[u8],
    headers: &[(String, String)],
    timeout: Duration,
    json_body: bool,
) -> Outcome {
    let deadline = Instant::now() + timeout;

    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        // A refused or unreachable connection is the one case that is unambiguously "never
        // reached": no bytes of ours ever left for the peer to act on.
        Err(e) => return Outcome::ConnectionFailed(e.to_string()),
    };

    let mut request = Vec::new();
    request.extend_from_slice(format!("{method} {path} HTTP/1.1\r\n").as_bytes());
    request.extend_from_slice(format!("Host: {addr}\r\n").as_bytes());
    request.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    // THE PROBE AND THE LOAD MUST SEND THE SAME REQUEST, matching what gen.rs's build_request sets:
    // a gateway that requires content-type on a JSON body would otherwise answer 415 to the probe
    // and be published as NOT SERVING a pairing it would have loaded fine, a gateway property
    // asserted from a malformed request of ours, the worst direction for this error to run. A
    // caller may still override it below.
    if json_body && !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("content-type")) {
        request.extend_from_slice(b"content-type: application/json\r\n");
    }
    // Always close: this client never pools connections, so there is nothing to keep alive for.
    request.extend_from_slice(b"Connection: close\r\n");
    for (name, value) in headers {
        request.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    request.extend_from_slice(b"\r\n");
    request.extend_from_slice(body);

    let write_deadline = deadline.saturating_duration_since(Instant::now());
    if stream.set_write_timeout(Some(write_deadline)).is_err() {
        return Outcome::TimedOut;
    }
    if let Err(e) = stream.write_all(&request) {
        // The connection was already live, so this is not "never reached" in the connect sense;
        // treat it the same as any other broken-mid-flight case, distinct from a status code.
        return if is_timeout(&e) {
            Outcome::TimedOut
        } else {
            Outcome::ConnectionFailed(format!("connection dropped while sending the request: {e}"))
        };
    }

    let (status, resp_headers, raw) = match read_head(&mut stream, deadline) {
        Ok(v) => v,
        Err(outcome) => return outcome,
    };

    let chunked = header_value(&resp_headers, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);

    if chunked {
        return read_chunked_body(&mut stream, deadline, &raw).map_status(status, &resp_headers);
    }

    if let Some(len) =
        header_value(&resp_headers, "content-length").and_then(|v| v.trim().parse::<usize>().ok())
    {
        return match read_exact_deadline(&mut stream, deadline, len) {
            ReadOutcome::Full(body) => Outcome::Response(HttpResponse {
                status,
                headers: resp_headers,
                body,
            }),
            ReadOutcome::TimedOut(partial) => {
                let mut seen = raw;
                seen.extend_from_slice(&partial);
                malformed(
                    &seen,
                    format!(
                        "timed out after {} of {len} declared body bytes",
                        partial.len()
                    ),
                )
            }
            ReadOutcome::Eof(partial) => {
                let mut seen = raw;
                seen.extend_from_slice(&partial);
                malformed(
                    &seen,
                    format!(
                        "connection closed after {} of {len} declared body bytes",
                        partial.len()
                    ),
                )
            }
            ReadOutcome::Err(e) => malformed(&raw, format!("error reading body: {e}")),
        };
    }

    // Neither Content-Length nor chunked: the only remaining well-defined framing is "the body
    // runs until the connection closes", which we already send (Connection: close) and expect.
    match read_to_close(&mut stream, deadline) {
        ReadOutcome::Full(body) => Outcome::Response(HttpResponse {
            status,
            headers: resp_headers,
            body,
        }),
        ReadOutcome::TimedOut(partial) => {
            let mut seen = raw;
            seen.extend_from_slice(&partial);
            malformed(&seen, "timed out reading a close-delimited body")
        }
        // read_to_close treats its own EOF as Full; Eof here would mean an I/O error path, kept
        // for exhaustiveness though read_to_close never returns it.
        ReadOutcome::Eof(partial) => Outcome::Response(HttpResponse {
            status,
            headers: resp_headers,
            body: partial,
        }),
        ReadOutcome::Err(e) => malformed(&raw, format!("error reading body: {e}")),
    }
}

/// Small helper so `read_chunked_body` (which does not know the already-parsed status/headers)
/// can hand back a properly filled-in `Outcome::Response` on success, without duplicating the
/// chunk-decoding loop per call site.
trait FillStatus {
    fn map_status(self, status: u16, headers: &Headers) -> Outcome;
}

impl FillStatus for Outcome {
    fn map_status(self, status: u16, headers: &Headers) -> Outcome {
        match self {
            Outcome::Response(mut r) => {
                r.status = status;
                r.headers = headers.clone();
                Outcome::Response(r)
            }
            other => other,
        }
    }
}

/// How an SSE probe's read loop ended. All four are informative; none of them is "success" or
/// "failure" on its own; the caller reads `frames` alongside this to judge that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEnd {
    /// Collected as many frames as the caller asked for; the stream may have had more.
    FrameBudgetReached,
    /// The deadline passed. On a stream that goes quiet this is expected and is not an error by
    /// itself: `frames` still reports whatever arrived before then, which must not be discarded
    /// just because the stream never explicitly finished.
    Timeout,
    /// The peer closed the connection (a normal, deliberate end of stream).
    StreamClosed,
    /// The connection could not be made at all.
    ConnectionFailed(String),
    /// The response head itself did not parse (wrong status line, broken headers): there is no
    /// stream to read frames from at all.
    Malformed(String),
    /// The peer answered, and answered with something that is not an event stream.
    ///
    /// An immediate, informative answer rather than a wait. Without this the probe sits until its
    /// deadline on every target that replies with plain JSON - which is most cells, since a gateway
    /// only streams where it is configured to - so a twenty second timeout was being burned twice per
    /// cell to learn something the content-type stated up front. Carries the type it did send.
    NotAnEventStream(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseOutcome {
    pub status: Option<u16>,
    pub frames: Vec<String>,
    /// Microseconds from the request being written to each frame arriving, one entry per frame in
    /// `frames`, in order.
    ///
    /// Frames alone cannot answer a single question the board asks about streaming: every published
    /// streaming field is a TIMING (time to first token, and the gaps between tokens after it), so
    /// the reader must carry a timestamp alongside each frame, not just the frame.
    ///
    /// Measured from the write, not from the connect, so a slow DNS or TCP handshake is not charged
    /// to the gateway's first token.
    pub frame_offsets_us: Vec<u64>,
    pub end: SseEnd,
}


// ─────────────────────────────────── the transport-agnostic SSE reader ───────────────────────────
//
// ONE DECODER, FED BY BOTH TRANSPORTS.
//
// The framing this has to get right is not small: HTTP head parsing, `Transfer-Encoding: chunked`
// (sizes in hex, extensions after `;`, the CRLF after each chunk's data, the terminal zero chunk and
// its trailers), and WHATWG SSE event assembly on top of that, where consecutive `data:` lines join
// with "\n" and a blank line dispatches the event. Writing that twice - once against a blocking
// socket and once against an async one - is two copies of the same intricate rules, and the copy
// that drifts produces a plausible frame count with corrupted timings rather than an error. Nothing
// would fail loudly.
//
// So it is written ONCE here, over bytes, with no socket in it at all. It cannot block, so the async
// lane can drive it; it cannot await, so the blocking lane can drive it; and it is a pure state
// machine, so it can be tested directly with hand-written byte sequences including the boundaries a
// live socket almost never produces on demand - a chunk header split across two reads, a `data:`
// line split across two chunks, a frame completed by the very last byte before EOF.
//
// The arrival timestamp is passed IN rather than read from a clock, for the same reason: the
// published streaming numbers are timings, so the moment a frame is credited has to be controllable
// by a test rather than whatever the machine did.

/// What the decoder wants next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing conclusive yet; feed more bytes.
    NeedMore,
    /// Finished, for the carried reason. Further feeding is ignored.
    Done(SseEnd),
}

/// Where the decoder is in the response.
#[derive(Debug, PartialEq, Eq)]
enum Phase {
    /// Still accumulating the response head.
    Head,
    /// Reading an identity-framed (close-delimited) body.
    Identity,
    /// Reading a chunked body; `Some(n)` = n bytes of the current chunk still to come, `None` =
    /// expecting a chunk-size line.
    Chunked { remaining: Option<usize> },
    /// The body ended (terminal chunk seen) or the decoder finished.
    Ended,
}

/// Reads an SSE response from bytes, with no transport of its own.
pub struct SseReader {
    phase: Phase,
    /// Bytes received but not yet consumed by the current phase.
    raw: Vec<u8>,
    /// Decoded body bytes not yet split into lines.
    body: Vec<u8>,
    status: Option<u16>,
    /// Data lines accumulated for the event that has not been dispatched yet.
    pending: Option<String>,
    frames: Vec<String>,
    offsets_us: Vec<u64>,
    budget: usize,
    finished: Option<SseEnd>,
}

impl SseReader {
    pub fn new(frame_budget: usize) -> Self {
        Self {
            phase: Phase::Head,
            raw: Vec::new(),
            body: Vec::new(),
            status: None,
            pending: None,
            frames: Vec::new(),
            offsets_us: Vec::new(),
            budget: frame_budget,
            finished: None,
        }
    }

    pub fn status(&self) -> Option<u16> {
        self.status
    }

    /// Feed bytes that arrived `elapsed_us` after the request was written. Frames completed by these
    /// bytes are credited that arrival time.
    pub fn feed(&mut self, bytes: &[u8], elapsed_us: u64) -> Step {
        if let Some(end) = &self.finished {
            return Step::Done(end.clone());
        }
        self.raw.extend_from_slice(bytes);
        // The cap every other framing in this client honours: an arbitrary peer must never be
        // trusted with an unbounded allocation, and a stream is the easiest place to forget it.
        if self.raw.len() > MAX_BODY_BYTES || self.body.len() > MAX_BODY_BYTES {
            return self.finish_with(SseEnd::Malformed(format!(
                "SSE body exceeded the {MAX_BODY_BYTES} byte cap"
            )));
        }
        loop {
            match self.phase {
                Phase::Head => match self.try_head() {
                    Some(Step::NeedMore) => return Step::NeedMore,
                    Some(other) => return other,
                    None => continue,
                },
                Phase::Identity => {
                    let taken = std::mem::take(&mut self.raw);
                    self.body.extend_from_slice(&taken);
                    if let Some(step) = self.drain_body(elapsed_us) {
                        return step;
                    }
                    return Step::NeedMore;
                }
                Phase::Chunked { .. } => match self.pump_chunked() {
                    ChunkPump::Progress => {
                        if let Some(step) = self.drain_body(elapsed_us) {
                            return step;
                        }
                        continue;
                    }
                    ChunkPump::NeedMore => {
                        if let Some(step) = self.drain_body(elapsed_us) {
                            return step;
                        }
                        return Step::NeedMore;
                    }
                    ChunkPump::BodyEnded => {
                        if let Some(step) = self.drain_body(elapsed_us) {
                            return step;
                        }
                        self.flush_pending(elapsed_us);
                        return self.finish_with(SseEnd::StreamClosed);
                    }
                    ChunkPump::Bad(msg) => return self.finish_with(SseEnd::Malformed(msg)),
                },
                Phase::Ended => return Step::Done(self.finished.clone().unwrap_or(SseEnd::StreamClosed)),
            }
        }
    }

    /// The peer stopped sending, or the deadline passed. Whatever arrived still counts: a stream
    /// that goes quiet is not an error, and discarding its frames would publish nothing for a
    /// gateway that streamed perfectly well up to that point.
    pub fn finish(mut self, end: SseEnd, elapsed_us: u64) -> SseOutcome {
        if self.finished.is_none() {
            self.flush_pending(elapsed_us);
        }
        let end = self.finished.clone().unwrap_or(end);
        SseOutcome { status: self.status, frames: self.frames, frame_offsets_us: self.offsets_us, end }
    }

    fn finish_with(&mut self, end: SseEnd) -> Step {
        self.phase = Phase::Ended;
        self.finished = Some(end.clone());
        Step::Done(end)
    }

    /// `None` = the head completed and the phase moved on, so the caller should loop again.
    fn try_head(&mut self) -> Option<Step> {
        let Some(cut) = find_head_end(&self.raw) else { return Some(Step::NeedMore) };
        let head: Vec<u8> = self.raw.drain(..cut).collect();
        let mut lines = head.split_inclusive(|b| *b == b'\n');
        let Some(status_line) = lines.next() else {
            return Some(self.finish_with(SseEnd::Malformed("empty response head".into())));
        };
        let Some(status) = parse_status_line(status_line) else {
            return Some(self.finish_with(SseEnd::Malformed(
                "status line did not parse as HTTP/x.y CODE ...".into(),
            )));
        };
        self.status = Some(status);
        let mut headers = Headers::new();
        for line in lines {
            let text = strip_crlf(line);
            if text.is_empty() {
                break;
            }
            if let Some((name, value)) = std::str::from_utf8(text).ok().and_then(|t| t.split_once(':')) {
                headers.push((name.trim().to_string(), value.trim().to_string()));
            }
        }
        // A content-type that is present and is not an event stream settles it immediately: waiting
        // out the deadline would learn nothing. A MISSING content-type is not a refusal - the frames
        // decide - so a peer that streams without announcing it is still read.
        if let Some(ct) = header_value(&headers, "content-type") {
            if !ct.to_ascii_lowercase().contains("text/event-stream") {
                return Some(self.finish_with(SseEnd::NotAnEventStream(ct.to_string())));
            }
        }
        let chunked = header_value(&headers, "transfer-encoding")
            .map(|v| v.to_ascii_lowercase().contains("chunked"))
            .unwrap_or(false);
        self.phase = if chunked { Phase::Chunked { remaining: None } } else { Phase::Identity };
        None
    }

    fn pump_chunked(&mut self) -> ChunkPump {
        loop {
            let Phase::Chunked { remaining } = self.phase else { return ChunkPump::BodyEnded };
            match remaining {
                None => {
                    let Some(nl) = self.raw.iter().position(|b| *b == b'\n') else {
                        return ChunkPump::NeedMore;
                    };
                    let line: Vec<u8> = self.raw.drain(..=nl).collect();
                    let text = String::from_utf8_lossy(strip_crlf(&line)).to_string();
                    if text.trim().is_empty() {
                        // The bare CRLF that follows a chunk's data, before the next size line.
                        continue;
                    }
                    // Chunk extensions ("1a;foo=bar") are legal; only the hex size matters.
                    let hex = text.split(';').next().unwrap_or("").trim();
                    let Ok(size) = usize::from_str_radix(hex, 16) else {
                        return ChunkPump::Bad(format!("unparseable chunk size {hex:?}"));
                    };
                    if size == 0 {
                        return ChunkPump::BodyEnded;
                    }
                    if size > MAX_BODY_BYTES {
                        return ChunkPump::Bad(format!(
                            "chunked SSE body exceeded the {MAX_BODY_BYTES} byte cap"
                        ));
                    }
                    self.phase = Phase::Chunked { remaining: Some(size) };
                }
                Some(want) => {
                    if self.raw.is_empty() {
                        return ChunkPump::NeedMore;
                    }
                    let take = want.min(self.raw.len());
                    let data: Vec<u8> = self.raw.drain(..take).collect();
                    self.body.extend_from_slice(&data);
                    let left = want - take;
                    self.phase = Phase::Chunked { remaining: if left == 0 { None } else { Some(left) } };
                    if left == 0 {
                        return ChunkPump::Progress;
                    }
                    return ChunkPump::NeedMore;
                }
            }
        }
    }

    /// Split whole lines out of the decoded body and assemble events. `Some` when the frame budget
    /// was reached, which is a completed read rather than an interruption.
    fn drain_body(&mut self, elapsed_us: u64) -> Option<Step> {
        while let Some(nl) = self.body.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.body.drain(..=nl).collect();
            let stripped = strip_crlf(&line);
            let text = String::from_utf8_lossy(stripped);
            if let Some(data) = text.strip_prefix("data:") {
                let data = data.trim_start().to_string();
                match &mut self.pending {
                    Some(acc) => {
                        acc.push('\n');
                        acc.push_str(&data);
                    }
                    None => self.pending = Some(data),
                }
            } else if stripped.is_empty() {
                self.flush_pending(elapsed_us);
                if self.frames.len() >= self.budget {
                    return Some(self.finish_with(SseEnd::FrameBudgetReached));
                }
            }
            // event:, id:, retry: and anything else is not a data frame and is skipped; the probe
            // only ever needs the data.
        }
        None
    }

    fn flush_pending(&mut self, elapsed_us: u64) {
        if let Some(data) = self.pending.take() {
            self.offsets_us.push(elapsed_us);
            self.frames.push(data);
        }
    }
}

enum ChunkPump {
    Progress,
    NeedMore,
    BodyEnded,
    Bad(String),
}

/// Byte offset just past the blank line that ends the response head, if it has all arrived.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4).or_else(|| {
        // Tolerate bare-LF heads, which some minimal peers (and this repo's own test servers) emit.
        buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2)
    })
}

/// POSTs like `post_json`, then reads Server-Sent-Event `data:` frames off the response body
/// until `frame_budget` frames have been seen or `timeout` elapses, whichever comes first.
///
/// Decodes `Transfer-Encoding: chunked` framing (see `ChunkedLineSource`) before splitting SSE
/// lines out of the body, since that is the framing hyper (and thus the mock, and thus this
/// harness's own recorded fixtures) actually uses for an open-ended stream.
pub fn post_json_sse(
    addr: SocketAddr,
    path: &str,
    body: &[u8],
    headers: &[(String, String)],
    timeout: Duration,
    frame_budget: usize,
) -> SseOutcome {
    let deadline = Instant::now() + timeout;
    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(e) => {
            return SseOutcome {
                status: None,
                frames: Vec::new(),
                frame_offsets_us: Vec::new(),
                end: SseEnd::ConnectionFailed(e.to_string()),
            }
        }
    };

    let request = build_sse_request(addr, path, body, headers);
    let write_deadline = deadline.saturating_duration_since(Instant::now());
    if stream.set_write_timeout(Some(write_deadline)).is_err() {
        return SseOutcome {
            status: None,
            frames: Vec::new(),
            frame_offsets_us: Vec::new(),
            end: SseEnd::ConnectionFailed("could not set a write deadline".to_string()),
        };
    }
    if let Err(e) = stream.write_all(&request) {
        return if is_timeout(&e) {
            SseOutcome { status: None, frames: Vec::new(), frame_offsets_us: Vec::new(), end: SseEnd::Timeout }
        } else {
            SseOutcome {
                status: None,
                frames: Vec::new(),
                frame_offsets_us: Vec::new(),
                end: SseEnd::ConnectionFailed(format!("connection dropped while sending the request: {e}")),
            }
        };
    }

    // The clock starts at the WRITE, not the connect, so a slow handshake is not charged to the
    // gateway's first token.
    let sent_at = Instant::now();
    let mut reader = SseReader::new(frame_budget);
    let mut buf = [0u8; 16 * 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return reader.finish(SseEnd::Timeout, sent_at.elapsed().as_micros() as u64);
        }
        if stream.set_read_timeout(Some(left)).is_err() {
            return reader.finish(SseEnd::Timeout, sent_at.elapsed().as_micros() as u64);
        }
        match stream.read(&mut buf) {
            Ok(0) => return reader.finish(SseEnd::StreamClosed, sent_at.elapsed().as_micros() as u64),
            Ok(n) => {
                let at = sent_at.elapsed().as_micros() as u64;
                if let Step::Done(end) = reader.feed(&buf[..n], at) {
                    return reader.finish(end, at);
                }
            }
            Err(e) if is_timeout(&e) => {
                return reader.finish(SseEnd::Timeout, sent_at.elapsed().as_micros() as u64)
            }
            // A peer that resets mid-stream still delivered what it delivered.
            Err(_) => return reader.finish(SseEnd::StreamClosed, sent_at.elapsed().as_micros() as u64),
        }
    }
}


/// The same SSE read, driven by tokio instead of a blocked thread.
///
/// ONE LANE PER TASK, NOT PER OS THREAD. `run::stream_window` used to spawn a thread per lane, which
/// is why the concurrent-stream searches were capped far below the throughput searches: 65536
/// threads is scheduler thrashing, not a bigger gateway, and a field run that tried it sat at a
/// 1-minute load average over 24,000 and never converged. That cap was OUR limit reaching the board
/// as the gateway's - 15 cells of the 2026-07-28 run published no cpu_fps because the search "was
/// still climbing" when the harness stopped.
///
/// The decoding is NOT duplicated here: this feeds the same `SseReader` the blocking lane feeds, and
/// sends the same bytes via the same `build_sse_request`. The only thing that differs between the
/// two lanes is who owns the waiting, which is the only thing that should.
pub async fn post_json_sse_async(
    addr: SocketAddr,
    path: &str,
    body: &[u8],
    headers: &[(String, String)],
    timeout: Duration,
    frame_budget: usize,
) -> SseOutcome {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let failed = |e: String| SseOutcome {
        status: None,
        frames: Vec::new(),
        frame_offsets_us: Vec::new(),
        end: SseEnd::ConnectionFailed(e),
    };

    let connect = tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr));
    let mut stream = match connect.await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return failed(e.to_string()),
        Err(_) => {
            return SseOutcome {
                status: None,
                frames: Vec::new(),
                frame_offsets_us: Vec::new(),
                end: SseEnd::Timeout,
            }
        }
    };

    let request = build_sse_request(addr, path, body, headers);
    match tokio::time::timeout(timeout, stream.write_all(&request)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return failed(format!("connection dropped while sending the request: {e}")),
        Err(_) => {
            return SseOutcome {
                status: None,
                frames: Vec::new(),
                frame_offsets_us: Vec::new(),
                end: SseEnd::Timeout,
            }
        }
    }

    // Clock from the WRITE, exactly as the blocking lane does, so a slow handshake is not charged to
    // the gateway's first token and the two lanes' numbers mean the same thing.
    let sent_at = std::time::Instant::now();
    let deadline = tokio::time::Instant::now() + timeout;
    let mut reader = SseReader::new(frame_budget);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let read = tokio::time::timeout_at(deadline, stream.read(&mut buf)).await;
        let at = sent_at.elapsed().as_micros() as u64;
        match read {
            Ok(Ok(0)) => return reader.finish(SseEnd::StreamClosed, at),
            Ok(Ok(n)) => {
                if let Step::Done(end) = reader.feed(&buf[..n], at) {
                    return reader.finish(end, at);
                }
            }
            // A peer that resets mid-stream still delivered what it delivered.
            Ok(Err(_)) => return reader.finish(SseEnd::StreamClosed, at),
            Err(_) => return reader.finish(SseEnd::Timeout, at),
        }
    }
}

/// The request both SSE transports send. Written once so the blocking lane and the async lane cannot
/// authenticate or frame differently: two lanes sending different bytes would make their numbers
/// incomparable in a way nothing downstream could see.
fn build_sse_request(addr: SocketAddr, path: &str, body: &[u8], headers: &[(String, String)]) -> Vec<u8> {
    let mut request = Vec::new();
    request.extend_from_slice(format!("POST {path} HTTP/1.1\r\n").as_bytes());
    request.extend_from_slice(format!("Host: {addr}\r\n").as_bytes());
    request.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    // THE PROBE AND THE LOAD MUST SEND THE SAME REQUEST, matching gen.rs's build_request: a gateway
    // that requires content-type on a JSON body would otherwise answer 415 to the probe and be
    // published as NOT SERVING a pairing it would have loaded fine.
    if !headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("content-type")) {
        request.extend_from_slice(b"content-type: application/json\r\n");
    }
    request.extend_from_slice(b"Connection: close\r\n");
    for (name, value) in headers {
        request.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    request.extend_from_slice(b"\r\n");
    request.extend_from_slice(body);
    request
}

#[cfg_attr(test, allow(clippy::panic, clippy::unwrap_used, clippy::expect_used))]
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::{TcpListener, TcpStream as StdTcpStream};
    use std::thread;

    /// Binds an ephemeral port, hands the accepted connection to `serve` on a background thread,
    /// and returns the address to connect to. `serve` gets the raw request bytes read so far are
    /// not parsed for it; it owns the whole connection and decides what, if anything, to write
    /// back and when.
    fn spawn_server<F>(serve: F) -> SocketAddr
    where
        F: FnOnce(StdTcpStream) + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            if let Ok((conn, _)) = listener.accept() {
                serve(conn);
            }
        });
        addr
    }

    fn read_request_head(conn: &StdTcpStream) -> Vec<String> {
        let mut reader = io::BufReader::new(conn.try_clone().expect("clone"));
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).unwrap_or(0);
            if n == 0 || line == "\r\n" || line == "\n" {
                break;
            }
            lines.push(line.trim_end().to_string());
        }
        lines
    }

    #[test]
    fn content_length_response_parses_status_and_body() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let body = b"{\"ok\":true}";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = conn.write_all(resp.as_bytes());
            let _ = conn.write_all(body);
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(r.status, 200);
                assert_eq!(r.body(), b"{\"ok\":true}");
                assert_eq!(r.header("content-type"), Some("application/json"));
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn chunked_response_body_is_dechunked_across_multiple_chunks() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n\
                  4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n",
            );
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(r.status, 200);
                assert_eq!(r.body(), b"Wikipedia");
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn a_4xx_and_a_5xx_both_parse_as_real_responses() {
        for (code, reason) in [(404u16, "Not Found"), (503, "Service Unavailable")] {
            let addr = spawn_server(move |mut conn| {
                let _ = read_request_head(&conn);
                let body = b"err";
                let resp = format!(
                    "HTTP/1.1 {code} {reason}\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = conn.write_all(resp.as_bytes());
                let _ = conn.write_all(body);
            });

            let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
            match outcome {
                Outcome::Response(r) => assert_eq!(r.status, code),
                other => panic!("status {code} must be a Response, got {other:?}"),
            }
        }
    }

    /// Captures the raw request bytes the client sent, so a test can assert on what went out.
    fn echo_request_server() -> (SocketAddr, std::sync::Arc<std::sync::Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = std::sync::Arc::clone(&seen);
        std::thread::spawn(move || {
            if let Some(Ok(mut conn)) = listener.incoming().next() {
                let mut buf = [0u8; 8192];
                let n = conn.read(&mut buf).unwrap_or(0);
                if let Ok(mut g) = sink.lock() {
                    g.push_str(&String::from_utf8_lossy(&buf[..n]));
                }
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
            }
        });
        (addr, seen)
    }

    // The probe and the load must send the SAME request: a gateway requiring content-type on a JSON
    // body would otherwise answer 415 to the probe and be published as not serving a pairing it
    // would have loaded fine, a gateway property asserted from a malformed request of ours.
    #[test]
    fn the_probe_sends_a_json_content_type_like_the_load_generator_does() {
        let (addr, seen) = echo_request_server();
        let _ = post_json(addr, "/v1/chat/completions", b"{}", &[], Duration::from_secs(2));
        let req = seen.lock().map(|g| g.clone()).unwrap_or_default();
        assert!(
            req.to_lowercase().contains("content-type: application/json"),
            "probe request must carry a json content-type, got:\n{req}"
        );
    }

    // A caller that supplies its own content-type must win: some dialects are not application/json.
    #[test]
    fn an_explicit_content_type_from_the_caller_is_not_duplicated() {
        let (addr, seen) = echo_request_server();
        let hdrs = vec![("content-type".to_string(), "application/x-ndjson".to_string())];
        let _ = post_json(addr, "/x", b"{}", &hdrs, Duration::from_secs(2));
        let req = seen.lock().map(|g| g.clone()).unwrap_or_default().to_lowercase();
        assert_eq!(req.matches("content-type:").count(), 1, "exactly one content-type:\n{req}");
        assert!(req.contains("application/x-ndjson"));
    }

    #[test]
    fn a_closed_port_is_a_connection_failure_never_a_status() {
        // Bind, read the ephemeral port, then drop the listener so the port refuses connections.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        drop(listener);

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(2));
        assert!(
            matches!(outcome, Outcome::ConnectionFailed(_)),
            "expected ConnectionFailed, got {outcome:?}"
        );
    }

    #[test]
    fn a_server_that_never_responds_times_out_within_budget_and_does_not_hang() {
        let addr = spawn_server(|conn| {
            // Accept and hold the connection open, sending nothing, until the test's own timeout
            // fires and the thread is torn down with the process.
            thread::sleep(Duration::from_secs(30));
            drop(conn);
        });

        let start = Instant::now();
        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_millis(300));
        assert_eq!(outcome, Outcome::TimedOut);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "post_json must return near its own timeout, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn a_malformed_status_line_reports_what_was_seen_never_a_default_status() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"NOT A STATUS LINE\r\n\r\n");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Malformed { seen, .. } => {
                assert!(seen.starts_with(b"NOT A STATUS LINE"));
            }
            other => panic!("expected Malformed carrying the bytes seen, got {other:?}"),
        }
    }

    #[test]
    fn headers_are_sent_exactly_as_supplied_including_duplicates() {
        let addr = spawn_server(|mut conn| {
            let lines = read_request_head(&conn);
            let count = lines
                .iter()
                .filter(|l| l.eq_ignore_ascii_case("x-probe: one"))
                .count();
            let body = format!("{{\"dupes\":{count}}}");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = conn.write_all(resp.as_bytes());
        });

        let headers = vec![
            ("X-Probe".to_string(), "one".to_string()),
            ("X-Probe".to_string(), "one".to_string()),
        ];
        let outcome = post_json(addr, "/x", b"{}", &headers, Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => assert_eq!(r.body(), b"{\"dupes\":2}"),
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn sse_counts_several_data_frames() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
            for i in 0..3 {
                let _ = conn.write_all(format!("data: chunk-{i}\n\n").as_bytes());
                thread::sleep(Duration::from_millis(20));
            }
            // Then close cleanly.
        });

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10);
        assert_eq!(outcome.status, Some(200));
        assert_eq!(outcome.frames, vec!["chunk-0", "chunk-1", "chunk-2"]);
        assert_eq!(outcome.end, SseEnd::StreamClosed);
    }

    // EVERY PUBLISHED STREAMING NUMBER IS A TIMING. Time to first token, and the gaps between
    // tokens after it - the frames themselves are never published, so the reader must carry a
    // timestamp alongside each frame, not just the frame.
    //
    // A server that holds a known pause before the first frame and a different known pause between
    // the rest is what makes the two quantities separable: if the offsets were fabricated, or all
    // stamped at once at the end, the first gap and the later gaps would not differ.
    #[test]
    fn sse_records_when_each_frame_arrived_not_just_that_it_did() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
            // A deliberately long wait for the FIRST token, then quick ones after it.
            thread::sleep(Duration::from_millis(150));
            let _ = conn.write_all(b"data: first\n\n");
            for i in 0..2 {
                thread::sleep(Duration::from_millis(30));
                let _ = conn.write_all(format!("data: next-{i}\n\n").as_bytes());
            }
        });

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10);
        assert_eq!(outcome.frames.len(), 3);
        assert_eq!(
            outcome.frame_offsets_us.len(),
            outcome.frames.len(),
            "one arrival time per frame, or the two lists cannot be zipped by a caller"
        );

        // Time to first token reflects the server's pause. Bounds are wide on purpose: this asserts
        // the clock is real, not that the machine is fast.
        let ttft_us = outcome.frame_offsets_us[0];
        assert!(
            (100_000..2_000_000).contains(&ttft_us),
            "first frame should land near the server's 150ms pause, got {ttft_us}us"
        );

        // Offsets are cumulative from the request, so they only ever increase.
        for w in outcome.frame_offsets_us.windows(2) {
            assert!(w[1] >= w[0], "frame arrival times must not go backwards: {:?}", outcome.frame_offsets_us);
        }

        // THE DISCRIMINATING CHECK. The gap after the first token is much smaller than the wait for
        // it. A single timestamp reused for every frame, or offsets stamped once at the end, would
        // make these equal - so this is what distinguishes a real per-frame clock from a plausible
        // looking one.
        let first_gap = outcome.frame_offsets_us[1] - outcome.frame_offsets_us[0];
        assert!(
            first_gap < ttft_us,
            "the 30ms inter-frame gap ({first_gap}us) must be clearly smaller than the 150ms time to first token ({ttft_us}us)"
        );
    }

    #[test]
    fn a_stream_that_never_yields_a_frame_records_no_arrival_times() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
            // Head, then nothing. There is no first token, so there is no time to first token: an
            // empty list, never a zero, which would read as an instant response.
        });
        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_millis(300), 10);
        assert!(outcome.frames.is_empty());
        assert!(outcome.frame_offsets_us.is_empty(), "no frames means no arrival times, not a zero");
    }

    // ── FRAMING ─────────────────────────────────────────────────────────────────────────────────
    //
    // Everything below pins how a response is FRAMED, which is the class of defect that costs a
    // whole cell without ever looking like a failure: a body that is silently truncated, or a
    // truncation silently accepted as a body, both hand the caller a well-formed `Response` whose
    // contents are wrong. The gateway under test is arbitrary third-party software, so every one of
    // these shapes is something a real target can and does emit.

    /// A minimal head builder, so a framing test states only the thing it is about.
    fn head(status_line: &str, headers: &[&str]) -> String {
        let mut s = String::from(status_line);
        s.push_str("\r\n");
        for h in headers {
            s.push_str(h);
            s.push_str("\r\n");
        }
        s.push_str("\r\n");
        s
    }

    // HTTP/1.0 is not a malformed HTTP/1.1. Several proxies and a few gateway front ends still
    // answer 1.0 on an error path, and rejecting the version would turn a perfectly readable 503
    // into `Malformed`, which probe.rs reads as "we may never have reached the gateway" rather than
    // as the gateway's own verdict. That is the exact collapse this file exists to prevent.
    #[test]
    fn an_http_1_0_response_is_a_real_response_not_a_malformed_one() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.0 503 Service Unavailable", &["Content-Length: 2"]).as_bytes());
            let _ = conn.write_all(b"no");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(r.status, 503, "an HTTP/1.0 status must be read, not defaulted");
                assert_eq!(r.body(), b"no");
            }
            other => panic!("HTTP/1.0 must parse as a real response, got {other:?}"),
        }
    }

    // Neither Content-Length nor Transfer-Encoding: the body runs to the close. This is the framing
    // this client actually asks for (it sends `Connection: close`), so a bug here silently empties
    // the body of every target that does not announce a length.
    #[test]
    fn a_close_delimited_body_with_no_framing_headers_is_read_in_full() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Content-Type: application/json"]).as_bytes());
            let _ = conn.write_all(b"{\"closed\":\"delimited\"}");
            // Dropping the connection here IS the framing signal.
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(r.status, 200);
                assert_eq!(r.body(), b"{\"closed\":\"delimited\"}");
            }
            other => panic!("a close-delimited body must be a response, got {other:?}"),
        }
    }

    // A SHORT BODY IS NOT A BODY. The peer declared a length and then closed early, so what arrived
    // is a fragment of a JSON document. Handing that back as `Response` lets a caller parse a
    // truncated payload, or worse, read the truncation as a semantic answer from the gateway. The
    // byte counts belong in the message because "how much of it arrived" is what tells an operator
    // whether this was a crash mid-write or a peer that lied about the length.
    #[test]
    fn a_body_shorter_than_its_declared_content_length_is_malformed_never_a_short_success() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Content-Length: 100"]).as_bytes());
            let _ = conn.write_all(b"short");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Malformed { seen, message } => {
                assert!(message.contains('5') && message.contains("100"), "the message must state how much of the declared length arrived, got {message:?}");
                assert!(seen.ends_with(b"short"), "the bytes actually seen must travel with the verdict");
            }
            other => panic!("a truncated body must be Malformed, got {other:?}"),
        }
    }

    // A declared length of zero is a COMPLETE body, and the length header settles the framing: the
    // client must not fall through to reading until the close, because a peer that keeps the
    // connection open (a keep-alive front end that ignored our `Connection: close`) would then hold
    // the probe until its deadline and turn an instant 204-shaped answer into a timeout.
    #[test]
    fn a_content_length_of_zero_is_an_empty_body_and_does_not_wait_for_the_close() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Content-Length: 0"]).as_bytes());
            // Deliberately hold the connection open well past the client's own timeout.
            thread::sleep(Duration::from_secs(30));
            drop(conn);
        });

        let start = Instant::now();
        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(r.status, 200);
                assert!(r.body().is_empty(), "a zero length body is empty, got {:?}", r.body());
            }
            other => panic!("Content-Length: 0 must frame the body, got {other:?}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "a declared zero length must settle the framing immediately, took {:?}",
            start.elapsed()
        );
    }

    // TCP delivers a stream, not messages. A peer that flushes its status line in two writes (a
    // proxy that prepends the version, a slow-loris front end) is entirely legal, and reading only
    // what happened to be in the first packet would report `Malformed` for a perfectly good 200.
    #[test]
    fn a_status_line_split_across_reads_is_reassembled() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"HTTP/1.1 2");
            thread::sleep(Duration::from_millis(20));
            let _ = conn.write_all(b"01 Created\r\nContent-Length: 2\r\n\r\nok");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(r.status, 201, "the split status line must be reassembled before it is parsed");
                assert_eq!(r.body(), b"ok");
            }
            other => panic!("a split status line must still parse, got {other:?}"),
        }
    }

    // The same stream property, one layer down: a header split mid-NAME. This matters more than the
    // status line because the header that gets split may be the one that frames the body, so a
    // partial read here does not merely mis-title the response, it mis-frames it.
    #[test]
    fn headers_split_across_reads_are_reassembled_and_still_frame_the_body() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Le");
            thread::sleep(Duration::from_millis(20));
            let _ = conn.write_all(b"ngth: 9\r\nX-Split: ");
            thread::sleep(Duration::from_millis(20));
            let _ = conn.write_all(b"yes\r\n\r\nWikipedia");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(r.body(), b"Wikipedia", "the split length header must still frame the body");
                assert_eq!(r.header("x-split"), Some("yes"));
            }
            other => panic!("split headers must still parse, got {other:?}"),
        }
    }

    // THE LENGTH IS THE PEER'S CLAIM, NOT OURS. usize::MAX as a Content-Length makes a reserving
    // reader panic on capacity overflow, and a merely enormous one reaches the allocator, whose
    // failure handler calls abort(): not a panic, nothing catches it, and an eight hour run dies
    // with no operator watching. The cap must be applied to the DECLARATION, before a byte is read,
    // and the verdict must say so rather than blaming a timeout.
    #[test]
    fn an_absurd_content_length_is_rejected_on_the_declaration_never_allocated() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", usize::MAX).as_bytes(),
            );
        });

        let start = Instant::now();
        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Malformed { message, .. } => assert!(
                message.contains("cap"),
                "the verdict must name the cap the declaration exceeded rather than blaming the read, got {message:?}"
            ),
            other => panic!("an absurd declared length must be Malformed, got {other:?}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "the declaration must be refused without waiting on the read, took {:?}",
            start.elapsed()
        );
    }

    // A CLOSE-DELIMITED BODY HAS NO DECLARATION TO CAP - the peer just streams until it closes the
    // connection, so the only place left to enforce MAX_BODY_BYTES is against what has actually
    // accumulated. This cap was only ever checked against a declared Content-Length, so a peer that
    // omits any framing header (legal: RFC 7230 permits close-delimited responses) and streams past
    // MAX_BODY_BYTES before the deadline grew the buffer without limit until the wall-clock timeout,
    // tens of seconds away - long enough on loopback to reach gigabytes and risk the allocator's
    // unconditional abort() this whole cap exists to avoid.
    #[test]
    fn a_close_delimited_body_past_the_cap_is_rejected_without_waiting_for_the_deadline() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
            // Stream comfortably past MAX_BODY_BYTES (8 MiB) in 64 KiB writes, then stop without
            // closing: if the cap is not enforced against the accumulator, this blocks until the
            // read deadline instead of failing immediately on the byte that crosses the cap.
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..170 {
                if conn.write_all(&chunk).is_err() {
                    return;
                }
            }
        });

        let start = Instant::now();
        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(20));
        match outcome {
            Outcome::Malformed { message, .. } => {
                assert!(message.contains("cap"), "must name the cap it exceeded, got {message:?}")
            }
            other => panic!("a close-delimited body past the cap must be Malformed, got {other:?}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "must be refused as soon as the accumulator crosses the cap, not at the read deadline, took {:?}",
            start.elapsed()
        );
    }

    // The chunked sibling of the same defect: no single chunk here exceeds MAX_BODY_BYTES (each is
    // legally sized), so the per-chunk check in read_exact_deadline never fires. Only a check on the
    // running total across chunks catches an unbounded NUMBER of legally-sized chunks.
    #[test]
    fn a_chunked_body_past_the_cap_is_rejected_without_waiting_for_the_deadline() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..170 {
                let head = format!("{:x}\r\n", chunk.len());
                if conn.write_all(head.as_bytes()).is_err() {
                    return;
                }
                if conn.write_all(&chunk).is_err() {
                    return;
                }
                if conn.write_all(b"\r\n").is_err() {
                    return;
                }
            }
        });

        let start = Instant::now();
        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(20));
        match outcome {
            Outcome::Malformed { message, .. } => {
                assert!(message.contains("cap"), "must name the cap it exceeded, got {message:?}")
            }
            other => panic!("a chunked body past the cap must be Malformed, got {other:?}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "must be refused as soon as the running total crosses the cap, not at the read deadline, took {:?}",
            start.elapsed()
        );
    }

    // RFC 7230 section 3.3.3: when both are present, Transfer-Encoding wins and Content-Length is
    // ignored. Getting this backwards truncates the body to the (bogus) declared length AND leaves
    // the chunk framing undecoded, so the caller gets chunk-size lines inside what it believes is
    // JSON. Real gateways emit both when a buffering proxy sits in front of a streaming origin.
    #[test]
    fn transfer_encoding_chunked_wins_over_a_content_length() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head("HTTP/1.1 200 OK", &["Content-Length: 5", "Transfer-Encoding: chunked"]).as_bytes(),
            );
            let _ = conn.write_all(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => assert_eq!(
                r.body(),
                b"Wikipedia",
                "chunked framing must win over the content-length, or the body is both truncated and left encoded"
            ),
            other => panic!("expected a response, got {other:?}"),
        }
    }

    // The chunk decoder builds its own `HttpResponse` with a placeholder status of 0 and no headers,
    // and relies on the caller to fill in what the head already parsed. If that hand-off is dropped,
    // every chunked response arrives as status 0, which is not a status any peer can send: a chunked
    // 503 would stop being a gateway verdict and become an unclassifiable number.
    #[test]
    fn a_chunked_response_carries_the_head_status_and_headers_not_the_placeholder() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head(
                    "HTTP/1.1 503 Service Unavailable",
                    &["Content-Type: application/json", "Transfer-Encoding: chunked"],
                )
                .as_bytes(),
            );
            let _ = conn.write_all(b"2\r\n{}\r\n0\r\n\r\n");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(r.status, 503, "the head's status must survive chunk decoding");
                assert_eq!(
                    r.header("content-type"),
                    Some("application/json"),
                    "the head's headers must survive chunk decoding"
                );
                assert_eq!(r.body(), b"{}");
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    // Chunk extensions ("1a;charset=utf-8") are legal and some proxies emit them. Parsing the whole
    // line as hex fails, and the failure surfaces as `Malformed`, so a target that merely annotated
    // its chunks would be reported as having sent a broken response.
    #[test]
    fn a_chunk_size_extension_is_stripped_before_the_hex_is_parsed() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Transfer-Encoding: chunked"]).as_bytes());
            let _ = conn.write_all(b"4;charset=utf-8\r\nWiki\r\n0\r\n\r\n");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => assert_eq!(r.body(), b"Wiki"),
            other => panic!("a chunk extension must not sink the response, got {other:?}"),
        }
    }

    // Trailing headers after the terminating zero chunk are rare but legal, and they must be
    // consumed up to the final blank line and never appear in the body: a caller that JSON-parses
    // the body would otherwise choke on a trailer glued to the end of a valid document.
    #[test]
    fn chunked_trailers_are_consumed_and_never_land_in_the_body() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Transfer-Encoding: chunked"]).as_bytes());
            let _ = conn.write_all(b"4\r\nWiki\r\n0\r\nX-Trailer: served\r\n\r\n");
            // Hold the connection open afterwards: the terminating blank line, not the close, is
            // what must end the read.
            thread::sleep(Duration::from_secs(30));
            drop(conn);
        });

        let start = Instant::now();
        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => assert_eq!(
                r.body(),
                b"Wiki",
                "a trailer must be consumed, not appended to the body"
            ),
            other => panic!("trailers must not sink the response, got {other:?}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "the trailer's blank line ends the response, so this must not wait for the close, took {:?}",
            start.elapsed()
        );
    }

    // The mirror of the truncated Content-Length case, for the other framing. Bytes arrived and the
    // peer vanished before the terminating zero chunk, so what we hold is a prefix. Returning it as
    // a `Response` would publish a partial body as the gateway's complete answer.
    #[test]
    fn a_chunked_body_that_ends_before_its_terminating_chunk_is_malformed() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Transfer-Encoding: chunked"]).as_bytes());
            let _ = conn.write_all(b"4\r\nWiki\r\n");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Malformed { seen, .. } => assert!(
                seen.ends_with(b"Wiki"),
                "the partial bytes must travel with the verdict for a human to inspect"
            ),
            other => panic!("a chunked stream cut short must be Malformed, got {other:?}"),
        }
    }

    // A chunked stream can also die inside the TRAILER, after the zero chunk was sent. Everything
    // that will ever be in the body has arrived by then, which is exactly what makes this tempting
    // to accept, and exactly why it must not be: the response was never terminated, so we cannot
    // tell a finished stream from a peer that crashed while writing.
    #[test]
    fn a_chunked_stream_that_dies_inside_its_trailer_is_malformed() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Transfer-Encoding: chunked"]).as_bytes());
            let _ = conn.write_all(b"4\r\nWiki\r\n0\r\nX-Trailer: half");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        assert!(
            matches!(outcome, Outcome::Malformed { .. }),
            "an unterminated trailer must not be read as a complete response, got {outcome:?}"
        );
    }

    // The evidence, not just the verdict. "Bad chunk size" tells an operator nothing; the bytes the
    // peer actually sent are what distinguishes a gateway emitting decimal sizes from a proxy that
    // double-encoded the body, and throwing them away is how a rig defect masquerades as a clean
    // gateway failure.
    #[test]
    fn an_unparseable_chunk_size_names_what_it_saw() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Transfer-Encoding: chunked"]).as_bytes());
            let _ = conn.write_all(b"nonsense\r\n");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Malformed { message, .. } => assert!(
                message.contains("nonsense"),
                "the unparseable size itself must be in the message, got {message:?}"
            ),
            other => panic!("an unparseable chunk size must be Malformed, got {other:?}"),
        }
    }

    // A stray non-header line in the head (a proxy's informational banner, an obs-fold continuation)
    // must be skipped rather than sink a response that otherwise has a perfectly good status, body,
    // and framing. Failing the whole response over one cosmetic line converts a gateway's real
    // answer into "we may never have reached it".
    #[test]
    fn a_header_line_with_no_colon_is_skipped_rather_than_sinking_the_response() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head("HTTP/1.1 200 OK", &["this line has no colon", "Content-Length: 2"]).as_bytes(),
            );
            let _ = conn.write_all(b"ok");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(r.status, 200);
                assert_eq!(r.body(), b"ok", "the framing header after the stray line must still be read");
            }
            other => panic!("a stray head line must not sink the response, got {other:?}"),
        }
    }

    // Header values legitimately contain colons (Date, and any URL in a Location or Link). Splitting
    // on the LAST colon rather than the first silently truncates such a value, and a content-type
    // read that way would misclassify a stream as JSON.
    #[test]
    fn a_header_value_containing_colons_survives_whole() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head(
                    "HTTP/1.1 200 OK",
                    &["Date: Mon, 01 Jan 2026 03:04:05 GMT", "Content-Length: 2"],
                )
                .as_bytes(),
            );
            let _ = conn.write_all(b"ok");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => assert_eq!(
                r.header("date"),
                Some("Mon, 01 Jan 2026 03:04:05 GMT"),
                "the value must be split at the FIRST colon, not the last"
            ),
            other => panic!("expected a response, got {other:?}"),
        }
    }

    // Headers are kept as a pair list and never a map, precisely so repeated names survive: some
    // dialects distinguish repeated headers from a single comma-joined one, and collapsing them
    // would hide that a peer sent two conflicting values before the caller ever saw the difference.
    #[test]
    fn duplicate_response_headers_both_survive_because_headers_are_a_pair_list() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head(
                    "HTTP/1.1 200 OK",
                    &["X-Rate: first", "X-Rate: second", "Content-Length: 2"],
                )
                .as_bytes(),
            );
            let _ = conn.write_all(b"ok");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                let rates: Vec<&str> = r
                    .headers()
                    .iter()
                    .filter(|(k, _)| k.eq_ignore_ascii_case("x-rate"))
                    .map(|(_, v)| v.as_str())
                    .collect();
                assert_eq!(rates, vec!["first", "second"], "both values must survive");
                assert_eq!(r.header("x-rate"), Some("first"), "the accessor returns the first, in wire order");
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    // obs-fold (RFC 7230 3.2.4): a continuation line starting with a space or tab extends the value
    // of the header immediately before it, rather than being a header of its own. A reader that
    // just fails to parse it as "name: value" and drops it silently truncates whatever value was
    // folded, which for something like a folded Warning or Location is exactly the part an operator
    // needed to see.
    #[test]
    fn an_obs_folded_header_line_is_unfolded_onto_the_previous_header() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                b"HTTP/1.1 200 OK\r\nX-Warn: primary reason\r\n \tfolded continuation\r\nContent-Length: 2\r\n\r\nok",
            );
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => assert_eq!(
                r.header("x-warn"),
                Some("primary reason folded continuation"),
                "the folded line must be appended to the previous header's value, not dropped"
            ),
            other => panic!("an obs-folded header must not sink the response, got {other:?}"),
        }
    }

    // RFC 7230 3.3.2: multiple Content-Length headers with DIFFERING values is a request-smuggling
    // shaped ambiguity and must be rejected outright, not silently resolved by taking the first one
    // and ignoring the rest.
    #[test]
    fn conflicting_content_length_headers_are_malformed() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head("HTTP/1.1 200 OK", &["Content-Length: 2", "Content-Length: 4"]).as_bytes(),
            );
            let _ = conn.write_all(b"ok");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Malformed { message, .. } => {
                assert!(
                    message.contains('2') && message.contains('4'),
                    "the message must name both conflicting values, got {message:?}"
                );
            }
            other => panic!("conflicting Content-Length headers must be Malformed, got {other:?}"),
        }
    }

    // Two IDENTICAL Content-Length headers are not a conflict (some servers double-send the same
    // value) and must not be rejected: only a genuine mismatch is a smuggling-relevant error.
    #[test]
    fn identical_duplicate_content_length_headers_are_not_rejected() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head("HTTP/1.1 200 OK", &["Content-Length: 2", "Content-Length: 2"]).as_bytes(),
            );
            let _ = conn.write_all(b"ok");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => assert_eq!(r.body(), b"ok"),
            other => panic!("identical duplicate Content-Length must not be rejected, got {other:?}"),
        }
    }

    // ── SSE framing ─────────────────────────────────────────────────────────────────────────────

    // A content-type that is present and is not an event stream is a DEFINITIVE answer: this peer is
    // not streaming, and waiting out the deadline learns nothing more. Most cells reply with plain
    // JSON, so without this a twenty second timeout is burned twice per cell to discover something
    // the head stated in its first few bytes.
    #[test]
    fn an_sse_probe_against_a_plain_json_answer_returns_at_once_and_names_the_type() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Content-Type: application/json"]).as_bytes());
            // Then hold the connection open: only the content-type may end this probe.
            thread::sleep(Duration::from_secs(30));
            drop(conn);
        });

        let start = Instant::now();
        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(3), 10);
        assert_eq!(outcome.end, SseEnd::NotAnEventStream("application/json".to_string()));
        assert_eq!(outcome.status, Some(200), "the peer answered, so its status is evidence about it");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "a non-stream content-type must end the probe immediately, took {:?}",
            start.elapsed()
        );
    }

    // A MISSING content-type is not a refusal. The frames are what settle whether this is a stream,
    // so a peer that streams without announcing it must still be read: treating absence as a
    // negative would publish "does not stream" about a gateway that demonstrably does.
    #[test]
    fn a_stream_that_never_declares_a_content_type_is_still_read() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
            let _ = conn.write_all(b"data: undeclared\n\n");
        });

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10);
        assert_eq!(outcome.frames, vec!["undeclared"], "an unannounced stream must still be read");
    }

    // The budget is a CEILING, not a target: an off-by-one here reads one extra frame off every
    // stream, which on a paced stream costs an inter-frame interval per probe and silently inflates
    // every streaming duration the suite publishes.
    #[test]
    fn sse_stops_at_the_frame_budget_and_says_so() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Content-Type: text/event-stream"]).as_bytes());
            for i in 0..20 {
                let _ = conn.write_all(format!("data: f{i}\n\n").as_bytes());
            }
            thread::sleep(Duration::from_secs(30));
            drop(conn);
        });

        let start = Instant::now();
        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 3);
        assert_eq!(outcome.frames, vec!["f0", "f1", "f2"], "exactly the budget, in order");
        assert_eq!(outcome.end, SseEnd::FrameBudgetReached);
        assert_eq!(outcome.frame_offsets_us.len(), 3);
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "reaching the budget must end the probe rather than run to the deadline, took {:?}",
            start.elapsed()
        );
    }

    // hyper (the mock's own server) chunk-encodes any SSE response, since a live event stream has
    // no Content-Length. A chunk-unaware reader only "worked" by coincidence of a chunk boundary
    // landing on a frame boundary; this deliberately splits a chunk MID-FRAME (inside the `data:`
    // line itself, and again inside its payload) so a reader that treats chunk-size lines as frame
    // noise instead of decoding them would read a truncated/corrupted frame or miscount entirely.
    #[test]
    fn a_chunk_boundary_landing_mid_frame_does_not_truncate_or_corrupt_it() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head(
                    "HTTP/1.1 200 OK",
                    &["Content-Type: text/event-stream", "Transfer-Encoding: chunked"],
                )
                .as_bytes(),
            );
            // "data: hello world\n\n" split across three chunks, the second split lands inside the
            // payload ("hello wo" | "rld\n\n"), and a fourth frame is split right after the "data:"
            // prefix itself so the prefix and payload arrive in separate chunks too.
            let _ = conn.write_all(b"6\r\ndata: \r\n");
            let _ = conn.write_all(b"8\r\nhello wo\r\n");
            let _ = conn.write_all(b"5\r\nrld\n\n\r\n");
            let _ = conn.write_all(b"5\r\ndata:\r\n");
            let _ = conn.write_all(b"7\r\nsecond\n\r\n");
            let _ = conn.write_all(b"1\r\n\n\r\n");
            let _ = conn.write_all(b"0\r\n\r\n");
        });

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10);
        assert_eq!(
            outcome.frames,
            vec!["hello world", "second"],
            "a frame split across chunk boundaries must be reassembled whole, got {:?}",
            outcome.frames
        );
        assert_eq!(outcome.end, SseEnd::StreamClosed);
    }

    // An SSE stream carries more line kinds than `data:`. Counting `event:`, `id:`, comments or the
    // blank separators as frames would inflate the frame count, and since every published streaming
    // number is a per-frame timing, an inflated count fabricates inter-token gaps that never
    // happened. The `data:` prefix may also be followed by any amount of leading space, or none.
    #[test]
    fn sse_counts_only_data_frames_and_trims_their_leading_space() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Content-Type: text/event-stream"]).as_bytes());
            let _ = conn.write_all(b": a comment\nevent: content_block_delta\nid: 7\ndata:tight\n\n");
            let _ = conn.write_all(b"retry: 1000\ndata:    padded\n\n");
        });

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10);
        assert_eq!(
            outcome.frames,
            vec!["tight", "padded"],
            "only data lines are frames, and the payload starts after the optional space"
        );
    }

    // There is no stream to read frames from if the head never parsed, and there is no status
    // either: reporting one would assert the peer answered when what it sent was not an answer.
    #[test]
    fn an_sse_probe_against_a_broken_head_is_malformed_and_carries_no_status() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"GARBAGE\r\n\r\n");
        });

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10);
        assert!(
            matches!(outcome.end, SseEnd::Malformed(_)),
            "a broken head must be Malformed, got {:?}",
            outcome.end
        );
        assert_eq!(outcome.status, None, "a status here would claim the peer answered");
        assert!(outcome.frames.is_empty());
    }

    // WHATWG SSE: consecutive `data:` lines with no blank line between them are ONE logical event,
    // joined with "\n". Treating each line as its own frame would split a multi-line payload (a
    // formatted message, a multi-line JSON delta) into several frames that were never separate
    // events, and would fabricate inter-token gaps between lines that arrived in the same event.
    #[test]
    fn consecutive_data_lines_before_a_blank_line_join_into_one_frame() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(head("HTTP/1.1 200 OK", &["Content-Type: text/event-stream"]).as_bytes());
            let _ = conn.write_all(b"data: line one\ndata: line two\n\ndata: second event\n\n");
        });

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10);
        assert_eq!(
            outcome.frames,
            vec!["line one\nline two", "second event"],
            "consecutive data lines before a blank must join into one frame; the blank line starts the next event"
        );
        assert_eq!(outcome.frame_offsets_us.len(), 2, "one arrival time per EVENT, not per line");
    }

    #[test]
    fn sse_on_a_quiet_stream_ends_on_timeout_with_frames_seen_so_far() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
            let _ = conn.write_all(b"data: only-one\n\n");
            // Then go quiet forever (from the client's point of view): hold the connection open
            // well past the client's own timeout instead of closing it.
            thread::sleep(Duration::from_secs(30));
            drop(conn);
        });

        let start = Instant::now();
        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_millis(300), 10);
        assert_eq!(outcome.end, SseEnd::Timeout);
        assert_eq!(outcome.frames, vec!["only-one"]);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "an SSE probe on a quiet stream must not hang, took {:?}",
            start.elapsed()
        );
    }

    // ── the transport-agnostic SSE reader ───────────────────────────────────────────────────────
    //
    // These are the boundaries a live socket produces rarely and unpredictably, which is exactly why
    // the decoder takes bytes instead of a socket: they can be produced on demand here.

    fn chunked(parts: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in parts {
            out.extend_from_slice(format!("{:x}\r\n", p.len()).as_bytes());
            out.extend_from_slice(p.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    const SSE_HEAD: &str =
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";

    fn read_all(bytes: &[u8], budget: usize, split: usize) -> SseOutcome {
        let mut r = SseReader::new(budget);
        let mut t = 0u64;
        for piece in bytes.chunks(split.max(1)) {
            t += 1;
            if let Step::Done(end) = r.feed(piece, t) {
                return r.finish(end, t);
            }
        }
        r.finish(SseEnd::StreamClosed, t)
    }

    #[test]
    fn the_reader_assembles_frames_from_chunked_framing() {
        let body = chunked(&["data: a\n\n", "data: b\n\n"]);
        let mut bytes = SSE_HEAD.as_bytes().to_vec();
        bytes.extend_from_slice(&body);
        let out = read_all(&bytes, 64, bytes.len());
        assert_eq!(out.status, Some(200));
        assert_eq!(out.frames, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(out.frame_offsets_us.len(), 2);
        assert_eq!(out.end, SseEnd::StreamClosed);
    }

    // THE PROPERTY THAT MATTERS MOST: how the bytes were split across reads must not change what was
    // read. A chunk header landing across two TCP segments, or a `data:` line split down the middle,
    // is ordinary on a real socket and used to be the difference between a correct frame and a
    // silently corrupted one.
    #[test]
    fn how_the_bytes_arrive_cannot_change_what_was_decoded() {
        let body = chunked(&["data: hel", "lo\n\ndata: wor", "ld\n\n", "data: third\n\n"]);
        let mut bytes = SSE_HEAD.as_bytes().to_vec();
        bytes.extend_from_slice(&body);
        let whole = read_all(&bytes, 64, bytes.len());
        assert_eq!(whole.frames, vec!["hello".to_string(), "world".to_string(), "third".to_string()]);
        // Every fragmentation, down to one byte at a time, must agree with it.
        for split in [1, 2, 3, 5, 7, 13, 64, 500] {
            let got = read_all(&bytes, 64, split);
            assert_eq!(got.frames, whole.frames, "fragmenting every {split} bytes changed the frames");
            assert_eq!(got.status, whole.status);
            assert_eq!(got.end, whole.end);
        }
    }

    #[test]
    fn identity_framing_is_read_the_same_way() {
        let bytes = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: x\n\ndata: y\n\n".to_vec();
        for split in [1, 4, 999] {
            let out = read_all(&bytes, 64, split);
            assert_eq!(out.frames, vec!["x".to_string(), "y".to_string()], "split {split}");
        }
    }

    // WHATWG: consecutive data lines are ONE event joined with a newline, dispatched by the blank
    // line. A gateway writing a multi-line JSON delta must not read as several frames.
    #[test]
    fn consecutive_data_lines_are_one_event() {
        let body = chunked(&["data: one\ndata: two\n\n"]);
        let mut bytes = SSE_HEAD.as_bytes().to_vec();
        bytes.extend_from_slice(&body);
        let out = read_all(&bytes, 64, 3);
        assert_eq!(out.frames, vec!["one\ntwo".to_string()]);
    }

    #[test]
    fn non_data_lines_are_skipped_and_the_budget_stops_the_read() {
        let body = chunked(&["event: ping\nid: 7\ndata: a\n\n", "data: b\n\n", "data: c\n\n"]);
        let mut bytes = SSE_HEAD.as_bytes().to_vec();
        bytes.extend_from_slice(&body);
        let out = read_all(&bytes, 2, 5);
        assert_eq!(out.frames, vec!["a".to_string(), "b".to_string()], "the budget stops at two");
        assert_eq!(out.end, SseEnd::FrameBudgetReached);
    }

    #[test]
    fn a_peer_that_is_not_streaming_is_answered_immediately() {
        let bytes = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{}".to_vec();
        let out = read_all(&bytes, 64, 7);
        assert!(matches!(out.end, SseEnd::NotAnEventStream(ref ct) if ct.contains("application/json")));
        assert_eq!(out.status, Some(200), "the status is still what the peer said");
        assert!(out.frames.is_empty());
    }

    #[test]
    fn a_broken_head_is_malformed_not_a_silent_empty_stream() {
        let out = read_all(b"NOT-HTTP\r\n\r\n", 64, 3);
        assert!(matches!(out.end, SseEnd::Malformed(_)), "got {:?}", out.end);
        assert_eq!(out.status, None);
    }

    // A stream that goes quiet mid-event keeps what arrived: the frames are real, and discarding
    // them would publish nothing for a gateway that streamed fine until the deadline.
    #[test]
    fn a_deadline_keeps_the_frames_that_already_arrived() {
        let body = chunked(&["data: a\n\ndata: b\n\n"]);
        let mut bytes = SSE_HEAD.as_bytes().to_vec();
        bytes.extend_from_slice(&body[..body.len() - 5]); // truncated: no terminal chunk
        let mut r = SseReader::new(64);
        assert_eq!(r.feed(&bytes, 10), Step::NeedMore);
        let out = r.finish(SseEnd::Timeout, 10);
        assert_eq!(out.frames, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(out.end, SseEnd::Timeout);
    }

    // Each frame is credited the moment its own bytes landed, because every published streaming
    // number is a timing and a shared timestamp would flatten the gaps to zero.
    #[test]
    fn each_frame_is_credited_the_arrival_of_its_own_bytes() {
        let mut r = SseReader::new(64);
        assert_eq!(r.feed(SSE_HEAD.as_bytes(), 0), Step::NeedMore);
        assert_eq!(r.feed(&chunked_open("data: a\n\n"), 100), Step::NeedMore);
        assert_eq!(r.feed(&chunked_open("data: b\n\n"), 250), Step::NeedMore);
        let out = r.finish(SseEnd::Timeout, 250);
        assert_eq!(out.frames, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(out.frame_offsets_us, vec![100, 250], "the gap between frames is the measurement");
    }

    fn chunked_open(payload: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{:x}\r\n", payload.len()).as_bytes());
        out.extend_from_slice(payload.as_bytes());
        out.extend_from_slice(b"\r\n");
        out
    }

    // THE TWO LANES MUST AGREE, AGAINST THE SAME PEER.
    //
    // The blocking lane and the tokio lane share the decoder and the request builder, so this is
    // asserting that the only thing that differs - who owns the waiting - does not change what was
    // read. Without it, "we ported the generator" is a claim resting on the two implementations
    // looking similar.
    #[test]
    fn the_async_lane_reads_exactly_what_the_blocking_lane_reads() {
        let addr = sse_server_for_diff();
        let path = "/v1/chat/completions";
        let headers: Vec<(String, String)> = vec![("authorization".into(), "Bearer dummy".into())];

        let blocking = post_json_sse(addr, path, b"{}", &headers, Duration::from_secs(5), 8);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime for the async lane");
        let asynced = rt.block_on(post_json_sse_async(addr, path, b"{}", &headers, Duration::from_secs(5), 8));

        assert_eq!(asynced.status, blocking.status, "the two lanes disagree on the status");
        assert_eq!(asynced.frames, blocking.frames, "the two lanes decoded different frames");
        assert_eq!(asynced.end, blocking.end, "the two lanes ended for different reasons");
        assert_eq!(
            asynced.frame_offsets_us.len(),
            blocking.frame_offsets_us.len(),
            "the two lanes credited a different number of frames"
        );
        assert!(!blocking.frames.is_empty(), "the fixture must actually stream, or this proves nothing");
    }

    /// A peer that streams in awkward pieces: chunk boundaries that fall inside `data:` lines, a
    /// multi-line event, and non-data lines between frames.
    fn sse_server_for_diff() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    let _ = c.read(&mut b);
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
                    if c.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    // Deliberately ugly splits, written as separate chunks AND separate writes.
                    for piece in [
                        "event: open\ndata: he",
                        "llo\n\ndata: multi\ndata: line\n\n",
                        "id: 3\ndata: last\n\n",
                    ] {
                        let framed = format!("{:x}\r\n{piece}\r\n", piece.len());
                        if c.write_all(framed.as_bytes()).is_err() {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    let _ = c.write_all(b"0\r\n\r\n");
                });
            }
        });
        addr
    }
}
