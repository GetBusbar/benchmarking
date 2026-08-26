// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Minimal HTTP/1.1 client over std::net::TcpStream: every probe is a plain JSON POST to
// 127.0.0.1. No external deps on purpose - no async runtime whose own scheduler could perturb
// the latency being measured, and no TLS/redirects since none are needed here.
//
// `Outcome` deliberately keeps "never reached", "connection failed", "timed out" and "malformed"
// distinct from a real response: collapsing them would let a rig failure (nobody listening) get
// published as a gateway verdict (see `Observation.status: Option<u16>` in probe.rs).

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

/// A header, as sent or received. Kept as a plain pair list (never a map) so duplicate header
/// names survive - a map would silently collapse them before the caller could see they differed.
pub type Headers = Vec<(String, String)>;

/// A response the peer actually produced (status line + body received), whatever the status code.
/// A 5xx is still evidence about the gateway, not a "no response".
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

/// The result of one `post_json` call. Never-reached, timed-out, malformed, and answered are kept
/// as four distinct outcomes rather than one "it failed", so a caller (probe.rs) can tell "never
/// reached" apart from "answered with a 5xx" - a curl-style `000` would collapse that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The request could not be framed without smuggling something onto the wire, so nothing was
    /// sent - unlike every other variant, this is a claim about US, not the peer. Never grade it
    /// as the gateway failing; the defect is in the manifest that produced it.
    RigRefused(String),
    /// A complete, well-formed HTTP response, whatever its status.
    Response(HttpResponse),
    /// The TCP connection itself could not be made or was severed before any response existed.
    /// The gateway may genuinely never have seen the request; this must never carry a status.
    ConnectionFailed(String),
    /// The deadline passed with nothing usable read yet. Distinct from `ConnectionFailed`: the
    /// connection was live, the peer just never (yet) answered. A hung gateway must not hang the
    /// suite, so this is always reached within the caller's timeout, never later.
    TimedOut,
    /// Something was read but did not parse as a complete HTTP response. Never a success; carries
    /// the bytes actually seen so a human can inspect them.
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
/// absolute wall-clock `deadline` rather than a fixed per-call timeout. Byte-at-a-time is slow but
/// keeps the `Malformed` byte count exact rather than approximate.
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
                // Bound the line: no legitimate header/status line is this long.
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

/// Hard ceiling on any response body accumulated from the gateway under test. Response length is
/// the peer's claim, not ours; an allocation failure past this would abort() unconditionally
/// (uncatchable), so exceeding it is Malformed rather than a measurement.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Ceiling on the size of a response HEAD (status line + all header lines together). `read_line`
/// caps one line and `MAX_BODY_BYTES` caps the body, but nothing else bounds the NUMBER of header
/// lines a peer can send - an accumulation loop in a third-party gateway should cost that
/// gateway's cell, not the run's memory. 256 KiB is far above any real response head.
const MAX_HEAD_BYTES: usize = 256 * 1024;

/// Reads exactly `n` bytes (a known Content-Length or chunk body), honouring `deadline`.
fn read_exact_deadline(stream: &mut TcpStream, deadline: Instant, n: usize) -> ReadOutcome {
    // Never reserve `n` up front: it's the peer's claimed Content-Length/chunk-size, and
    // usize::MAX would panic Vec::reserve while a merely huge value could abort() the allocator.
    // Grow as bytes actually arrive; the body cap below still rejects an over-long response.
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
        // No declared length here, so MAX_BODY_BYTES must be checked against what has actually
        // accumulated rather than up front, or a peer that never closes grows this unboundedly.
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
        if raw.len() > MAX_HEAD_BYTES {
            return Err(malformed(
                &raw,
                format!("response head exceeds the {MAX_HEAD_BYTES} byte cap - refusing to keep reading headers"),
            ));
        }
        let stripped = strip_crlf(&line);
        if stripped.is_empty() {
            break;
        }
        // obs-fold (RFC 7230 3.2.4): a line starting with SP/HTAB continues the previous header's
        // value. Obsolete but legal and still emitted by some proxies; fold it here or the value
        // is silently truncated.
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
            // Trailing headers (rare, usually absent) run up to the final blank line. Capped the
            // same as the response head: nothing else bounds the number of trailer lines a peer
            // can send after the terminal `0\r\n`.
            loop {
                if seen.len() > MAX_HEAD_BYTES {
                    return malformed(
                        &seen,
                        format!("chunked trailer exceeds the {MAX_HEAD_BYTES} byte cap - refusing to keep reading"),
                    );
                }
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
        // Aggregate cap, not just per-chunk: read_exact_deadline already rejects one oversized
        // chunk, but an unbounded number of legally-sized chunks could still grow `body` forever.
        // Checked after appending so the over-cap chunk is still in the Malformed evidence.
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
/// Used only for the mock's own control plane (`/__mock/state`, the evidence behind the egress
/// re-verification verdict in `reverify.rs`), going through the same `Outcome` discipline as POST
/// so "mock unreachable" and "mock answered, recorder empty" stay distinct.
///
/// No `content-type` is sent: a GET with no body has nothing to declare a type for.
pub fn get(
    addr: SocketAddr,
    path: &str,
    headers: &[(String, String)],
    timeout: Duration,
) -> Outcome {
    send("GET", addr, path, &[], headers, timeout, false)
}

/// Why this request cannot be put on the wire, or `None` when it can.
///
/// Requests are assembled by interpolating manifest-supplied path/headers into HTTP framing with
/// `format!`; a `\r`/`\n` anywhere in those strings would inject extra headers or a second
/// request, a `:` in a header name would rename it, a space in the path would rewrite the request
/// line's HTTP version. NUL is refused for the same reason.
///
/// RIG-12: this rule must be enforced identically by all three request-building lanes (`send`
/// here, `gen.rs::build_request`, and `build_sse_request`) - it was once fixed in only one, which
/// left the others injectable. Manifests are first-party, so a hit here is a manifest defect, not
/// a gateway property.
pub fn unsendable_request(path: &str, headers: &[(String, String)]) -> Option<String> {
    // The request target is interpolated into the request LINE, where a space or a line break is
    // just as much of a second request as one in a header.
    if path.contains([' ', '\t', '\r', '\n', '\0']) {
        return Some(format!(
            "request path {path:?} carries whitespace or a line break, which would rewrite the request line"
        ));
    }
    for (name, value) in headers {
        // The name is checked too: a colon or CRLF in a name smuggles a header just as well as one
        // in a value, and a name is no more trusted than the value beside it.
        if name.contains([':', '\r', '\n', '\0']) || value.contains(['\r', '\n', '\0']) {
            return Some(format!(
                "header {name:?} carries a line break, a colon in its name, or a NUL, which would inject \
                 headers onto the wire: {value:?}"
            ));
        }
    }
    None
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
    // Refused before the connect: a request we will not send should never touch the gateway.
    if let Some(why) = unsendable_request(path, headers) {
        return Outcome::RigRefused(why);
    }

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
    // Must match gen.rs::build_request: a gateway requiring content-type on JSON would otherwise
    // 415 the probe and get published as not serving a pairing it would have loaded fine. A
    // caller may still override this below.
    if json_body
        && !headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("content-type"))
    {
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
    /// Budgeted in CONTENT frames (`SseBudget::Content`) and hit the total-event ceiling before
    /// that many tokens arrived. A delivery shortfall, not an errored stream: the peer answered
    /// 200 and kept sending, it just spent the ceiling on non-content events. `stream_errored`
    /// leaves this alone; the delivery ratio fails the gate on the count instead.
    EventCeilingReached,
    /// The deadline passed. Not an error by itself; `frames` still reports whatever arrived.
    Timeout,
    /// The peer closed the connection (a normal, deliberate end of stream).
    StreamClosed,
    /// The connection could not be made at all - by the PEER's doing (refused, unreachable, reset).
    ConnectionFailed(String),
    /// The connection could not be made because THIS HOST ran out of ephemeral source ports
    /// (EADDRNOTAVAIL) or file descriptors (EMFILE/ENFILE). Split from `ConnectionFailed` because
    /// a host with no source ports left never asked the gateway anything - conflating the two
    /// let a stream search at high concurrency publish our own exhaustion as the gateway's ceiling.
    RigExhausted(String),
    /// The rig refused to send - not a claim about the peer. Same fact as `Outcome::RigRefused` on
    /// the non-streaming lanes; must never count toward an errored stream or a failing rung.
    RigRefused(String),
    /// The response head itself did not parse (wrong status line, broken headers): there is no
    /// stream to read frames from at all.
    Malformed(String),
    /// The peer answered with something that is not an event stream. Settled immediately rather
    /// than waiting out the deadline on every plain-JSON target. Carries the type it did send.
    NotAnEventStream(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseOutcome {
    pub status: Option<u16>,
    pub frames: Vec<String>,
    /// Microseconds from the request being written to each frame arriving, one entry per frame in
    /// `frames`, in order. Measured from the write, not the connect, so a slow handshake is not
    /// charged to the gateway's first token.
    pub frame_offsets_us: Vec<u64>,
    /// How many of `frames` carried MODEL OUTPUT rather than protocol scaffolding, per the
    /// request's dialect (`ingress::Dialect::sse_event_is_content`).
    ///
    /// RIG-11: `frames.len()` counts every dispatched event, which is wrong for a delivery ratio -
    /// dialects spend a different number of events on framing, so two gateways delivering the same
    /// tokens could score differently. Equals `frames.len()` when no dialect is supplied.
    pub content_frames: u64,
    pub end: SseEnd,
}

// ─────────────────────────────────── the transport-agnostic SSE reader ───────────────────────────
//
// One decoder, fed by both transports (blocking and async), rather than two hand-rolled copies of
// the same chunked+SSE framing rules that could silently drift and produce a plausible frame count
// with corrupted timings. It is a pure byte-in state machine with no socket and no clock of its
// own: it cannot block or await, so either transport can drive it, and the arrival timestamp is
// passed in so tests can control exactly when a frame is credited.

/// What the decoder wants next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Nothing conclusive yet; feed more bytes.
    NeedMore,
    /// Finished, for the carried reason. Further feeding is ignored.
    Done(SseEnd),
}

/// What stops the read: a count of dispatched events, or a count of CONTENT frames with a ceiling
/// on the events spent getting them.
///
/// RIG-11: under plain `Events`, a non-content event (anthropic's `ping`, a translating gateway's
/// own framing, any keepalive) consumes a budget slot and displaces a content frame, so a gateway
/// that lost nothing could still fail the delivery-ratio gate on arithmetic. `Content` reads until
/// the tokens themselves arrive, bounded by `event_ceiling` so a peer that pings forever cannot
/// hang the read - hitting the ceiling short of `frames` is `SseEnd::EventCeilingReached`, a real
/// shortfall that must still fail the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseBudget {
    /// Stop after this many dispatched events, whatever they carried. What every non-delivery
    /// caller wants (gap distribution as framed, or the first event for TTFT).
    Events(usize),
    /// Stop once `frames` CONTENT-classified events have arrived, or at `event_ceiling` total
    /// events, whichever comes first. With no dialect every event counts as content, so this
    /// degenerates to `Events(frames)` under the ceiling.
    Content { frames: u64, event_ceiling: usize },
}

impl From<usize> for SseBudget {
    fn from(events: usize) -> Self {
        SseBudget::Events(events)
    }
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
    /// How far the front of `raw` / `body` has already been searched for the current phase's
    /// delimiter, so a peer trickling a few bytes per read doesn't make feed() rescan the whole
    /// buffer each time (O(fragments * bytes) inside the timed TTFT/gap window). Mirrors
    /// gen.rs::read_response's `scanned` cursor; reset to 0 when a phase hands the buffer off.
    raw_scanned: usize,
    body_scanned: usize,
    status: Option<u16>,
    /// Data lines accumulated for the event that has not been dispatched yet.
    pending: Option<String>,
    // Event boundaries seen, including ones with no `data:` line (comment keepalives, `event:
    // ping`). `frames` alone can't drive the `Content` budget's event ceiling since a peer that
    // only sends keepalives would never trip it; counting dispatches makes the ceiling hold.
    events_seen: usize,
    frames: Vec<String>,
    offsets_us: Vec<u64>,
    budget: SseBudget,
    /// Which wire dialect these events are in, when known. The decoder never inspects payloads
    /// itself; it asks the dialect (`sse_event_is_content`). `None` means every event counts.
    dialect: Option<crate::ingress::Dialect>,
    content_frames: u64,
    finished: Option<SseEnd>,
}

impl SseReader {
    /// `budget` takes a bare `usize` (an event count, the historic meaning) or an `SseBudget`.
    pub fn new(budget: impl Into<SseBudget>, dialect: Option<crate::ingress::Dialect>) -> Self {
        Self {
            phase: Phase::Head,
            raw: Vec::new(),
            body: Vec::new(),
            raw_scanned: 0,
            body_scanned: 0,
            status: None,
            pending: None,
            events_seen: 0,
            frames: Vec::new(),
            offsets_us: Vec::new(),
            budget: budget.into(),
            dialect,
            content_frames: 0,
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
                    self.raw_scanned = 0;
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
                        // SSE dispatches on the blank line, so any held `data:` lines were never
                        // dispatched and are dropped here, same as `finish` does.
                        self.pending = None;
                        return self.finish_with(SseEnd::StreamClosed);
                    }
                    ChunkPump::Bad(msg) => return self.finish_with(SseEnd::Malformed(msg)),
                },
                Phase::Ended => {
                    return Step::Done(self.finished.clone().unwrap_or(SseEnd::StreamClosed))
                }
            }
        }
    }

    /// The peer stopped sending, or the deadline passed. Whatever was already dispatched still
    /// counts. A held (undispatched) `data:` fragment is dropped, not flushed: stamping it with a
    /// fabricated arrival time (the close/deadline instant) used to enter the gap samples as a
    /// stall no real frame arrival produced. No arrival-time parameter on purpose, so it can't
    /// happen again.
    pub fn finish(mut self, end: SseEnd) -> SseOutcome {
        self.pending = None;
        let end = self.finished.clone().unwrap_or(end);
        SseOutcome {
            status: self.status,
            frames: self.frames,
            frame_offsets_us: self.offsets_us,
            content_frames: self.content_frames,
            end,
        }
    }

    fn finish_with(&mut self, end: SseEnd) -> Step {
        self.phase = Phase::Ended;
        self.finished = Some(end.clone());
        Step::Done(end)
    }

    /// `None` = the head completed and the phase moved on, so the caller should loop again.
    fn try_head(&mut self) -> Option<Step> {
        let from = self.raw_scanned.saturating_sub(3);
        let Some(cut) = find_head_end(&self.raw[from..]).map(|c| from + c) else {
            self.raw_scanned = self.raw.len();
            return Some(Step::NeedMore);
        };
        let head: Vec<u8> = take_front(&mut self.raw, &mut self.raw_scanned, cut);
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
            if let Some((name, value)) = std::str::from_utf8(text)
                .ok()
                .and_then(|t| t.split_once(':'))
            {
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
        self.phase = if chunked {
            Phase::Chunked { remaining: None }
        } else {
            Phase::Identity
        };
        None
    }

    fn pump_chunked(&mut self) -> ChunkPump {
        loop {
            let Phase::Chunked { remaining } = self.phase else {
                return ChunkPump::BodyEnded;
            };
            match remaining {
                None => {
                    let Some(nl) = self.raw[self.raw_scanned..]
                        .iter()
                        .position(|b| *b == b'\n')
                        .map(|i| self.raw_scanned + i)
                    else {
                        self.raw_scanned = self.raw.len();
                        return ChunkPump::NeedMore;
                    };
                    let line: Vec<u8> = take_front(&mut self.raw, &mut self.raw_scanned, nl + 1);
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
                    self.phase = Phase::Chunked {
                        remaining: Some(size),
                    };
                }
                Some(want) => {
                    if self.raw.is_empty() {
                        return ChunkPump::NeedMore;
                    }
                    let take = want.min(self.raw.len());
                    let data: Vec<u8> = take_front(&mut self.raw, &mut self.raw_scanned, take);
                    self.body.extend_from_slice(&data);
                    let left = want - take;
                    self.phase = Phase::Chunked {
                        remaining: if left == 0 { None } else { Some(left) },
                    };
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
        while let Some(nl) = self.body[self.body_scanned..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| self.body_scanned + i)
        {
            let line: Vec<u8> = take_front(&mut self.body, &mut self.body_scanned, nl + 1);
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
                // `MAX_BODY_BYTES` guards `raw` and `body` but not `pending`: a peer sending
                // `data:` lines and never the terminating blank line would grow this unboundedly.
                if self.pending.as_ref().map(String::len).unwrap_or(0) > MAX_BODY_BYTES {
                    return Some(self.finish_with(SseEnd::Malformed(format!(
                        "a single SSE frame exceeded the {MAX_BODY_BYTES} byte cap without a frame \
                         boundary - the peer is sending data lines and never the blank line that ends \
                         one"
                    ))));
                }
            } else if stripped.is_empty() {
                // The blank line IS the event boundary, whether or not the event carried data.
                self.events_seen += 1;
                self.flush_pending(elapsed_us);
                if let Some(end) = self.budget_reached() {
                    return Some(self.finish_with(end));
                }
            }
            // event:, id:, retry: and anything else is not a data frame and is skipped; the probe
            // only ever needs the data.
        }
        self.body_scanned = self.body.len();
        None
    }

    /// Which bound, if either, the read has now reached. Checked only after an event is DISPATCHED,
    /// so a budget can never be satisfied by a fragment the peer never terminated.
    fn budget_reached(&self) -> Option<SseEnd> {
        match self.budget {
            SseBudget::Events(n) => (self.frames.len() >= n).then_some(SseEnd::FrameBudgetReached),
            SseBudget::Content {
                frames,
                event_ceiling,
            } => {
                if self.content_frames >= frames {
                    Some(SseEnd::FrameBudgetReached)
                } else if self.events_seen >= event_ceiling {
                    // Short of the content asked for, and out of events to wait for it in. Reported
                    // as its own end so the shortfall is not read as a satisfied budget.
                    Some(SseEnd::EventCeilingReached)
                } else {
                    None
                }
            }
        }
    }

    fn flush_pending(&mut self, elapsed_us: u64) {
        if let Some(data) = self.pending.take() {
            // Classified at dispatch, by the DIALECT, never by this decoder reading the bytes for
            // itself - see the `dialect` field. With no dialect every dispatched event counts, which
            // is what `frames` has always meant.
            if self
                .dialect
                .is_none_or(|d| d.sse_event_is_content(data.as_str()))
            {
                self.content_frames += 1;
            }
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

/// Drain `..upto` off the front of `buf`, keeping `scanned` (how far the front has already been
/// searched, see `SseReader::raw_scanned`) pointing at the same bytes it did before.
fn take_front(buf: &mut Vec<u8>, scanned: &mut usize, upto: usize) -> Vec<u8> {
    let out: Vec<u8> = buf.drain(..upto).collect();
    *scanned = scanned.saturating_sub(upto);
    out
}

/// Byte offset just past the blank line that ends the response head, if it has all arrived.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    // The earlier terminator wins: the buffer may already contain body bytes from the same read,
    // so a CRLF pair later in an SSE frame must not beat an earlier bare-LF blank line that
    // actually ended the head (e.g. an LF head followed by CRLF frames arriving in one segment) -
    // that used to drop a frame and overstate TTFT. `\r\n\r\n` cannot contain `\n\n`, so `min` is
    // exactly "whichever blank line came first".
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
    // Tolerate bare-LF heads, which some minimal peers (and this repo's own test servers) emit.
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2);
    match (crlf, lf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// POSTs like `post_json`, then reads Server-Sent-Event `data:` frames off the response body
/// until `budget` is satisfied or `timeout` elapses, whichever comes first.
///
/// `budget` is a bare event count, or an `SseBudget::Content` when the caller is measuring DELIVERY
/// rather than framing - see `SseBudget`.
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
    budget: impl Into<SseBudget>,
    dialect: Option<crate::ingress::Dialect>,
) -> SseOutcome {
    // BUILT BEFORE THE CONNECT, so a request we will not send never opens a socket to the gateway.
    let request = match build_sse_request(addr, path, body, headers) {
        Ok(r) => r,
        Err(why) => {
            return SseOutcome {
                status: None,
                frames: Vec::new(),
                frame_offsets_us: Vec::new(),
                content_frames: 0,
                end: SseEnd::RigRefused(why),
            }
        }
    };
    let deadline = Instant::now() + timeout;
    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(e) => {
            return SseOutcome {
                status: None,
                frames: Vec::new(),
                frame_offsets_us: Vec::new(),
                content_frames: 0,
                end: connect_end(&e),
            }
        }
    };

    let write_deadline = deadline.saturating_duration_since(Instant::now());
    // A zero write_deadline means the connect consumed the whole timeout budget - our clock, not
    // the peer refusing. Must map to `Timeout`, not `ConnectionFailed`, matching what the blocking
    // lane already calls this case and what the caller gets one microsecond later anyway.
    if write_deadline.is_zero() {
        return SseOutcome {
            status: None,
            frames: Vec::new(),
            frame_offsets_us: Vec::new(),
            content_frames: 0,
            end: SseEnd::Timeout,
        };
    }
    if stream.set_write_timeout(Some(write_deadline)).is_err() {
        return SseOutcome {
            status: None,
            frames: Vec::new(),
            frame_offsets_us: Vec::new(),
            content_frames: 0,
            // A non-zero deadline the OS still refused is our socket, not their server.
            end: SseEnd::RigExhausted("could not set a write deadline on the socket".to_string()),
        };
    }
    if let Err(e) = stream.write_all(&request) {
        return if is_timeout(&e) {
            SseOutcome {
                status: None,
                frames: Vec::new(),
                frame_offsets_us: Vec::new(),
                content_frames: 0,
                end: SseEnd::Timeout,
            }
        } else {
            SseOutcome {
                status: None,
                frames: Vec::new(),
                frame_offsets_us: Vec::new(),
                content_frames: 0,
                end: SseEnd::ConnectionFailed(format!(
                    "connection dropped while sending the request: {e}"
                )),
            }
        };
    }

    // The clock starts at the WRITE, not the connect, so a slow handshake is not charged to the
    // gateway's first token.
    let sent_at = Instant::now();
    let mut reader = SseReader::new(budget, dialect);
    let mut buf = [0u8; 16 * 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return reader.finish(SseEnd::Timeout);
        }
        if stream.set_read_timeout(Some(left)).is_err() {
            return reader.finish(SseEnd::Timeout);
        }
        match stream.read(&mut buf) {
            Ok(0) => return reader.finish(SseEnd::StreamClosed),
            Ok(n) => {
                let at = sent_at.elapsed().as_micros() as u64;
                if let Step::Done(end) = reader.feed(&buf[..n], at) {
                    return reader.finish(end);
                }
            }
            Err(e) if is_timeout(&e) => return reader.finish(SseEnd::Timeout),
            // A peer that resets mid-stream still delivered what it delivered.
            Err(_) => return reader.finish(SseEnd::StreamClosed),
        }
    }
}

/// The same SSE read, driven by tokio instead of a blocked thread. One lane per task rather than
/// per OS thread, since a thread-per-lane design capped concurrent-stream searches far below
/// throughput searches (thousands of OS threads is scheduler thrashing, not a bigger gateway).
/// Feeds the same `SseReader` and sends the same bytes via `build_sse_request` as the blocking
/// lane, so the two differ only in who owns the waiting.
pub async fn post_json_sse_async(
    addr: SocketAddr,
    path: &str,
    body: &[u8],
    headers: &[(String, String)],
    timeout: Duration,
    budget: impl Into<SseBudget>,
    dialect: Option<crate::ingress::Dialect>,
) -> SseOutcome {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Resolved before the first await, so the lane's future carries an `SseBudget` rather than a
    // generic this task would have to hold across every read.
    let budget: SseBudget = budget.into();

    let ended = |end: SseEnd| SseOutcome {
        status: None,
        frames: Vec::new(),
        frame_offsets_us: Vec::new(),
        content_frames: 0,
        end,
    };
    let failed = |e: String| ended(SseEnd::ConnectionFailed(e));

    // BUILT BEFORE THE CONNECT, so a request we will not send never opens a socket to the gateway.
    let request = match build_sse_request(addr, path, body, headers) {
        Ok(r) => r,
        Err(why) => return ended(SseEnd::RigRefused(why)),
    };

    // One deadline for the whole lane (connect + write + reads), as the blocking lane does.
    // Giving each phase its own fresh `timeout` let a lane run to 3x what the caller asked for,
    // which `stream_window` doesn't budget for and would desync the two lanes' timeout samples.
    let lane_deadline = tokio::time::Instant::now() + timeout;

    let connect = tokio::time::timeout_at(lane_deadline, tokio::net::TcpStream::connect(addr));
    let mut stream = match connect.await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return ended(connect_end(&e)),
        Err(_) => return ended(SseEnd::Timeout),
    };

    match tokio::time::timeout_at(lane_deadline, stream.write_all(&request)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return failed(format!("connection dropped while sending the request: {e}")),
        Err(_) => return ended(SseEnd::Timeout),
    }

    // Clock from the write, as the blocking lane does, so a slow handshake isn't charged to the
    // gateway's first token. The deadline stays the lane's (set before the connect).
    let sent_at = std::time::Instant::now();
    let deadline = lane_deadline;
    let mut reader = SseReader::new(budget, dialect);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let read = tokio::time::timeout_at(deadline, stream.read(&mut buf)).await;
        let at = sent_at.elapsed().as_micros() as u64;
        match read {
            Ok(Ok(0)) => return reader.finish(SseEnd::StreamClosed),
            Ok(Ok(n)) => {
                if let Step::Done(end) = reader.feed(&buf[..n], at) {
                    return reader.finish(end);
                }
            }
            // A peer that resets mid-stream still delivered what it delivered.
            Ok(Err(_)) => return reader.finish(SseEnd::StreamClosed),
            Err(_) => return reader.finish(SseEnd::Timeout),
        }
    }
}

/// Which side failed to connect: EADDRNOTAVAIL (no ephemeral source port) or EMFILE/ENFILE (no
/// descriptor) mean this host ran out, not the gateway declining. Everything else stays a
/// connection failure.
fn connect_end(e: &std::io::Error) -> SseEnd {
    let ours = matches!(
        e.kind(),
        std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::AddrInUse
    ) || matches!(e.raw_os_error(), Some(23) | Some(24));
    if ours {
        SseEnd::RigExhausted(e.to_string())
    } else {
        SseEnd::ConnectionFailed(e.to_string())
    }
}

/// The request both SSE transports send. Written once so the blocking lane and the async lane cannot
/// authenticate or frame differently: two lanes sending different bytes would make their numbers
/// incomparable in a way nothing downstream could see.
///
/// `Err` when the request cannot be framed without smuggling - the same rule the probe and load
/// lanes apply, in the same function, so the streaming lane cannot be the one that stayed
/// injectable (it was; see `unsendable_request`).
fn build_sse_request(
    addr: SocketAddr,
    path: &str,
    body: &[u8],
    headers: &[(String, String)],
) -> Result<Vec<u8>, String> {
    if let Some(why) = unsendable_request(path, headers) {
        return Err(why);
    }
    let mut request = Vec::new();
    request.extend_from_slice(format!("POST {path} HTTP/1.1\r\n").as_bytes());
    request.extend_from_slice(format!("Host: {addr}\r\n").as_bytes());
    request.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    // Must match gen.rs::build_request, same reasoning as in `send`.
    if !headers
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case("content-type"))
    {
        request.extend_from_slice(b"content-type: application/json\r\n");
    }
    request.extend_from_slice(b"Connection: close\r\n");
    for (name, value) in headers {
        request.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    request.extend_from_slice(b"\r\n");
    request.extend_from_slice(body);
    Ok(request)
}

#[cfg_attr(test, allow(clippy::panic, clippy::unwrap_used, clippy::expect_used))]
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::{TcpListener, TcpStream as StdTcpStream};
    use std::thread;

    /// Binds an ephemeral port, hands the accepted connection to `serve` on a background thread,
    /// and returns the address to connect to. `serve` owns the whole connection and decides what,
    /// if anything, to write back and when.
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
        let _ = post_json(
            addr,
            "/v1/chat/completions",
            b"{}",
            &[],
            Duration::from_secs(2),
        );
        let req = seen.lock().map(|g| g.clone()).unwrap_or_default();
        assert!(
            req.to_lowercase()
                .contains("content-type: application/json"),
            "probe request must carry a json content-type, got:\n{req}"
        );
    }

    // A caller that supplies its own content-type must win: some dialects are not application/json.
    #[test]
    fn an_explicit_content_type_from_the_caller_is_not_duplicated() {
        let (addr, seen) = echo_request_server();
        let hdrs = vec![(
            "content-type".to_string(),
            "application/x-ndjson".to_string(),
        )];
        let _ = post_json(addr, "/x", b"{}", &hdrs, Duration::from_secs(2));
        let req = seen
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
            .to_lowercase();
        assert_eq!(
            req.matches("content-type:").count(),
            1,
            "exactly one content-type:\n{req}"
        );
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

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10, None);
        assert_eq!(outcome.status, Some(200));
        assert_eq!(outcome.frames, vec!["chunk-0", "chunk-1", "chunk-2"]);
        assert_eq!(outcome.end, SseEnd::StreamClosed);
    }

    // Published streaming numbers are timings (TTFT, inter-token gaps), not frame counts, so each
    // frame needs its own arrival timestamp. Distinct pauses before/after the first frame make a
    // fabricated or end-stamped offset list distinguishable from a real one.
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

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10, None);
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
            assert!(
                w[1] >= w[0],
                "frame arrival times must not go backwards: {:?}",
                outcome.frame_offsets_us
            );
        }

        // The gap after the first token must be much smaller than the wait for it - a single
        // reused timestamp or end-of-stream stamping would make these equal.
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
        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_millis(300), 10, None);
        assert!(outcome.frames.is_empty());
        assert!(
            outcome.frame_offsets_us.is_empty(),
            "no frames means no arrival times, not a zero"
        );
    }

    // ── FRAMING ─────────────────────────────────────────────────────────────────────────────────
    //
    // Pins how a response is framed: a silently truncated (or silently accepted) body hands the
    // caller a well-formed `Response` with wrong contents, and never looks like a failure.

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

    // HTTP/1.0 is not malformed HTTP/1.1: rejecting the version would turn a readable 503 into
    // `Malformed`, which probe.rs reads as "never reached" instead of the gateway's own verdict.
    #[test]
    fn an_http_1_0_response_is_a_real_response_not_a_malformed_one() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head("HTTP/1.0 503 Service Unavailable", &["Content-Length: 2"]).as_bytes(),
            );
            let _ = conn.write_all(b"no");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(
                    r.status, 503,
                    "an HTTP/1.0 status must be read, not defaulted"
                );
                assert_eq!(r.body(), b"no");
            }
            other => panic!("HTTP/1.0 must parse as a real response, got {other:?}"),
        }
    }

    // Neither Content-Length nor Transfer-Encoding: body runs to the close, which is what this
    // client's own `Connection: close` asks for.
    #[test]
    fn a_close_delimited_body_with_no_framing_headers_is_read_in_full() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn
                .write_all(head("HTTP/1.1 200 OK", &["Content-Type: application/json"]).as_bytes());
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

    // A short body must not surface as `Response`: a caller would parse the truncated fragment as
    // a real answer. Byte counts in the message tell an operator crash-mid-write from a lied length.
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
                assert!(
                    seen.ends_with(b"short"),
                    "the bytes actually seen must travel with the verdict"
                );
            }
            other => panic!("a truncated body must be Malformed, got {other:?}"),
        }
    }

    // Content-Length: 0 settles the framing; must not fall through to reading until close, or a
    // peer ignoring our `Connection: close` would turn an instant answer into a timeout.
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
                assert!(
                    r.body().is_empty(),
                    "a zero length body is empty, got {:?}",
                    r.body()
                );
            }
            other => panic!("Content-Length: 0 must frame the body, got {other:?}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "a declared zero length must settle the framing immediately, took {:?}",
            start.elapsed()
        );
    }

    // TCP delivers a stream, not messages; a status line split across two writes is legal and must
    // not read as `Malformed`.
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
                assert_eq!(
                    r.status, 201,
                    "the split status line must be reassembled before it is parsed"
                );
                assert_eq!(r.body(), b"ok");
            }
            other => panic!("a split status line must still parse, got {other:?}"),
        }
    }

    // Same stream property, one layer down: a split header may be the one that frames the body, so
    // a partial read here mis-frames the response, not just mis-titles it.
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
                assert_eq!(
                    r.body(),
                    b"Wikipedia",
                    "the split length header must still frame the body"
                );
                assert_eq!(r.header("x-split"), Some("yes"));
            }
            other => panic!("split headers must still parse, got {other:?}"),
        }
    }

    // usize::MAX as Content-Length would panic a reserving reader or abort() the allocator; the
    // cap must reject the declaration itself, before a byte is read.
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

    // A close-delimited body has no declared length to cap up front, so MAX_BODY_BYTES must be
    // enforced against the accumulator instead - otherwise this streams unbounded until the
    // deadline, risking the allocator's abort().
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
                assert!(
                    message.contains("cap"),
                    "must name the cap it exceeded, got {message:?}"
                )
            }
            other => panic!("a close-delimited body past the cap must be Malformed, got {other:?}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "must be refused as soon as the accumulator crosses the cap, not at the read deadline, took {:?}",
            start.elapsed()
        );
    }

    // Chunked sibling of the same defect: no single chunk exceeds MAX_BODY_BYTES, so only a check
    // on the running total across chunks catches an unbounded number of them.
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
                assert!(
                    message.contains("cap"),
                    "must name the cap it exceeded, got {message:?}"
                )
            }
            other => panic!("a chunked body past the cap must be Malformed, got {other:?}"),
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "must be refused as soon as the running total crosses the cap, not at the read deadline, took {:?}",
            start.elapsed()
        );
    }

    // RFC 7230 §3.3.3: when both are present, Transfer-Encoding wins and Content-Length is
    // ignored. Getting this backwards leaves chunk framing undecoded inside what the caller
    // believes is JSON.
    #[test]
    fn transfer_encoding_chunked_wins_over_a_content_length() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head(
                    "HTTP/1.1 200 OK",
                    &["Content-Length: 5", "Transfer-Encoding: chunked"],
                )
                .as_bytes(),
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

    // The chunk decoder returns a placeholder status of 0; the caller must fill in the head's real
    // status/headers, or a chunked 503 would arrive unclassifiable.
    #[test]
    fn a_chunked_response_carries_the_head_status_and_headers_not_the_placeholder() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head(
                    "HTTP/1.1 503 Service Unavailable",
                    &[
                        "Content-Type: application/json",
                        "Transfer-Encoding: chunked",
                    ],
                )
                .as_bytes(),
            );
            let _ = conn.write_all(b"2\r\n{}\r\n0\r\n\r\n");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(
                    r.status, 503,
                    "the head's status must survive chunk decoding"
                );
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

    // Chunk extensions ("1a;charset=utf-8") are legal; parsing the whole line as hex would fail and
    // wrongly report a target that merely annotated its chunks as `Malformed`.
    #[test]
    fn a_chunk_size_extension_is_stripped_before_the_hex_is_parsed() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ =
                conn.write_all(head("HTTP/1.1 200 OK", &["Transfer-Encoding: chunked"]).as_bytes());
            let _ = conn.write_all(b"4;charset=utf-8\r\nWiki\r\n0\r\n\r\n");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => assert_eq!(r.body(), b"Wiki"),
            other => panic!("a chunk extension must not sink the response, got {other:?}"),
        }
    }

    // Trailing headers after the zero chunk are rare but legal; must be consumed and never land in
    // the body, or a JSON-parsing caller chokes on the trailer glued to the end.
    #[test]
    fn chunked_trailers_are_consumed_and_never_land_in_the_body() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ =
                conn.write_all(head("HTTP/1.1 200 OK", &["Transfer-Encoding: chunked"]).as_bytes());
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

    // Mirror of the truncated Content-Length case: the peer vanished before the terminating zero
    // chunk, so what we hold is a prefix and must not surface as a complete `Response`.
    #[test]
    fn a_chunked_body_that_ends_before_its_terminating_chunk_is_malformed() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ =
                conn.write_all(head("HTTP/1.1 200 OK", &["Transfer-Encoding: chunked"]).as_bytes());
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

    // Dying inside the trailer (after the zero chunk) is tempting to accept since the body is
    // already complete, but the response was never terminated - indistinguishable from a crash.
    #[test]
    fn a_chunked_stream_that_dies_inside_its_trailer_is_malformed() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ =
                conn.write_all(head("HTTP/1.1 200 OK", &["Transfer-Encoding: chunked"]).as_bytes());
            let _ = conn.write_all(b"4\r\nWiki\r\n0\r\nX-Trailer: half");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        assert!(
            matches!(outcome, Outcome::Malformed { .. }),
            "an unterminated trailer must not be read as a complete response, got {outcome:?}"
        );
    }

    // "Bad chunk size" alone tells an operator nothing; the bytes actually seen must be in the
    // message.
    #[test]
    fn an_unparseable_chunk_size_names_what_it_saw() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ =
                conn.write_all(head("HTTP/1.1 200 OK", &["Transfer-Encoding: chunked"]).as_bytes());
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

    // A stray non-header line must be skipped, not sink an otherwise-good response - failing the
    // whole thing over one cosmetic line turns a real answer into "never reached".
    #[test]
    fn a_header_line_with_no_colon_is_skipped_rather_than_sinking_the_response() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head(
                    "HTTP/1.1 200 OK",
                    &["this line has no colon", "Content-Length: 2"],
                )
                .as_bytes(),
            );
            let _ = conn.write_all(b"ok");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => {
                assert_eq!(r.status, 200);
                assert_eq!(
                    r.body(),
                    b"ok",
                    "the framing header after the stray line must still be read"
                );
            }
            other => panic!("a stray head line must not sink the response, got {other:?}"),
        }
    }

    // Header values can contain colons (Date, a Location URL); must split on the FIRST colon only.
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

    // Headers are a pair list, never a map, so repeated names survive rather than collapsing.
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
                assert_eq!(
                    r.header("x-rate"),
                    Some("first"),
                    "the accessor returns the first, in wire order"
                );
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    // obs-fold (RFC 7230 3.2.4): a leading-space/tab continuation extends the previous header's
    // value rather than being its own header; dropping it as unparseable truncates the value.
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
                head(
                    "HTTP/1.1 200 OK",
                    &["Content-Length: 2", "Content-Length: 4"],
                )
                .as_bytes(),
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
                head(
                    "HTTP/1.1 200 OK",
                    &["Content-Length: 2", "Content-Length: 2"],
                )
                .as_bytes(),
            );
            let _ = conn.write_all(b"ok");
        });

        let outcome = post_json(addr, "/x", b"{}", &[], Duration::from_secs(5));
        match outcome {
            Outcome::Response(r) => assert_eq!(r.body(), b"ok"),
            other => {
                panic!("identical duplicate Content-Length must not be rejected, got {other:?}")
            }
        }
    }

    // ── SSE framing ─────────────────────────────────────────────────────────────────────────────

    // A present, non-event-stream content-type is definitive: waiting out the deadline would learn
    // nothing more, and most cells reply with plain JSON.
    #[test]
    fn an_sse_probe_against_a_plain_json_answer_returns_at_once_and_names_the_type() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn
                .write_all(head("HTTP/1.1 200 OK", &["Content-Type: application/json"]).as_bytes());
            // Then hold the connection open: only the content-type may end this probe.
            thread::sleep(Duration::from_secs(30));
            drop(conn);
        });

        let start = Instant::now();
        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(3), 10, None);
        assert_eq!(
            outcome.end,
            SseEnd::NotAnEventStream("application/json".to_string())
        );
        assert_eq!(
            outcome.status,
            Some(200),
            "the peer answered, so its status is evidence about it"
        );
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "a non-stream content-type must end the probe immediately, took {:?}",
            start.elapsed()
        );
    }

    // A missing content-type is not a refusal; the frames settle whether this is a stream.
    #[test]
    fn a_stream_that_never_declares_a_content_type_is_still_read() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
            let _ = conn.write_all(b"data: undeclared\n\n");
        });

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10, None);
        assert_eq!(
            outcome.frames,
            vec!["undeclared"],
            "an unannounced stream must still be read"
        );
    }

    // The budget is a ceiling, not a target: an off-by-one reads one extra frame and silently
    // inflates every published streaming duration.
    #[test]
    fn sse_stops_at_the_frame_budget_and_says_so() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head("HTTP/1.1 200 OK", &["Content-Type: text/event-stream"]).as_bytes(),
            );
            for i in 0..20 {
                let _ = conn.write_all(format!("data: f{i}\n\n").as_bytes());
            }
            thread::sleep(Duration::from_secs(30));
            drop(conn);
        });

        let start = Instant::now();
        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 3, None);
        assert_eq!(
            outcome.frames,
            vec!["f0", "f1", "f2"],
            "exactly the budget, in order"
        );
        assert_eq!(outcome.end, SseEnd::FrameBudgetReached);
        assert_eq!(outcome.frame_offsets_us.len(), 3);
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "reaching the budget must end the probe rather than run to the deadline, took {:?}",
            start.elapsed()
        );
    }

    // hyper chunk-encodes any SSE response (no Content-Length on a live stream). Deliberately
    // splits a chunk mid-frame so a reader that doesn't actually decode chunk framing would
    // truncate or miscount.
    #[test]
    fn a_chunk_boundary_landing_mid_frame_does_not_truncate_or_corrupt_it() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head(
                    "HTTP/1.1 200 OK",
                    &[
                        "Content-Type: text/event-stream",
                        "Transfer-Encoding: chunked",
                    ],
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

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10, None);
        assert_eq!(
            outcome.frames,
            vec!["hello world", "second"],
            "a frame split across chunk boundaries must be reassembled whole, got {:?}",
            outcome.frames
        );
        assert_eq!(outcome.end, SseEnd::StreamClosed);
    }

    // Only `data:` lines are frames - counting `event:`, `id:`, comments, or blank separators would
    // fabricate inter-token gaps. The `data:` prefix may be followed by any amount of leading
    // space, or none.
    #[test]
    fn sse_counts_only_data_frames_and_trims_their_leading_space() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head("HTTP/1.1 200 OK", &["Content-Type: text/event-stream"]).as_bytes(),
            );
            let _ =
                conn.write_all(b": a comment\nevent: content_block_delta\nid: 7\ndata:tight\n\n");
            let _ = conn.write_all(b"retry: 1000\ndata:    padded\n\n");
        });

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10, None);
        assert_eq!(
            outcome.frames,
            vec!["tight", "padded"],
            "only data lines are frames, and the payload starts after the optional space"
        );
    }

    // No status either if the head never parsed: reporting one would assert the peer answered.
    #[test]
    fn an_sse_probe_against_a_broken_head_is_malformed_and_carries_no_status() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"GARBAGE\r\n\r\n");
        });

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10, None);
        assert!(
            matches!(outcome.end, SseEnd::Malformed(_)),
            "a broken head must be Malformed, got {:?}",
            outcome.end
        );
        assert_eq!(
            outcome.status, None,
            "a status here would claim the peer answered"
        );
        assert!(outcome.frames.is_empty());
    }

    // WHATWG SSE: consecutive `data:` lines with no blank line between them are ONE event, joined
    // with "\n" - treating each line as its own frame would fabricate inter-token gaps.
    #[test]
    fn consecutive_data_lines_before_a_blank_line_join_into_one_frame() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                head("HTTP/1.1 200 OK", &["Content-Type: text/event-stream"]).as_bytes(),
            );
            let _ = conn.write_all(b"data: line one\ndata: line two\n\ndata: second event\n\n");
        });

        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_secs(5), 10, None);
        assert_eq!(
            outcome.frames,
            vec!["line one\nline two", "second event"],
            "consecutive data lines before a blank must join into one frame; the blank line starts the next event"
        );
        assert_eq!(
            outcome.frame_offsets_us.len(),
            2,
            "one arrival time per EVENT, not per line"
        );
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
        let outcome = post_json_sse(addr, "/x", b"{}", &[], Duration::from_millis(300), 10, None);
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
    // These boundaries occur rarely and unpredictably on a live socket; the decoder takes bytes
    // instead of a socket so they can be produced on demand here.

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
        let mut r = SseReader::new(budget, None);
        let mut t = 0u64;
        for piece in bytes.chunks(split.max(1)) {
            t += 1;
            if let Step::Done(end) = r.feed(piece, t) {
                return r.finish(end);
            }
        }
        r.finish(SseEnd::StreamClosed)
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

    // drain_body (like pump_chunked and try_head) must not rescan the accumulated buffer from
    // index 0 on every feed(), or a slow peer costs O(fragments * bytes) inside the timed
    // TTFT/gap window. Mirrors gen.rs::read_response's `scanned` cursor.
    #[test]
    fn feeding_the_same_bytes_in_many_small_fragments_is_not_quadratically_slower() {
        const IDENTITY_SSE_HEAD: &str =
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n";

        // One un-terminated `data:` line delivered a few bytes at a time; no '\n' ever appears.
        let payload = vec![b'x'; 50_000];

        let mut fragmented = SseReader::new(usize::MAX, None);
        assert_eq!(
            fragmented.feed(IDENTITY_SSE_HEAD.as_bytes(), 0),
            Step::NeedMore
        );
        let start = Instant::now();
        for chunk in payload.chunks(10) {
            assert_eq!(fragmented.feed(chunk, 0), Step::NeedMore);
        }
        let fragmented_elapsed = start.elapsed();

        // Same bytes delivered whole: one scan instead of 5,000.
        let mut whole = SseReader::new(usize::MAX, None);
        assert_eq!(whole.feed(IDENTITY_SSE_HEAD.as_bytes(), 0), Step::NeedMore);
        let start = Instant::now();
        assert_eq!(whole.feed(&payload, 0), Step::NeedMore);
        let whole_elapsed = start.elapsed();

        // A resuming cursor costs the same total work regardless of fragmentation; rescanning from
        // 0 on every feed() would be orders of magnitude slower here.
        assert!(
            fragmented_elapsed < whole_elapsed * 100 + Duration::from_millis(50),
            "fragmented feed took {fragmented_elapsed:?} vs {whole_elapsed:?} for the same bytes \
             delivered whole - drain_body looks like it is rescanning from index 0 on every feed()"
        );
    }

    // How the bytes were split across reads must not change what was decoded - a chunk header or
    // `data:` line split mid-way is ordinary on a real socket.
    #[test]
    fn how_the_bytes_arrive_cannot_change_what_was_decoded() {
        let body = chunked(&["data: hel", "lo\n\ndata: wor", "ld\n\n", "data: third\n\n"]);
        let mut bytes = SSE_HEAD.as_bytes().to_vec();
        bytes.extend_from_slice(&body);
        let whole = read_all(&bytes, 64, bytes.len());
        assert_eq!(
            whole.frames,
            vec![
                "hello".to_string(),
                "world".to_string(),
                "third".to_string()
            ]
        );
        // Every fragmentation, down to one byte at a time, must agree with it.
        for split in [1, 2, 3, 5, 7, 13, 64, 500] {
            let got = read_all(&bytes, 64, split);
            assert_eq!(
                got.frames, whole.frames,
                "fragmenting every {split} bytes changed the frames"
            );
            assert_eq!(got.status, whole.status);
            assert_eq!(got.end, whole.end);
        }
    }

    #[test]
    fn identity_framing_is_read_the_same_way() {
        let bytes =
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: x\n\ndata: y\n\n"
                .to_vec();
        for split in [1, 4, 999] {
            let out = read_all(&bytes, 64, split);
            assert_eq!(
                out.frames,
                vec!["x".to_string(), "y".to_string()],
                "split {split}"
            );
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
        let body = chunked(&[
            "event: ping\nid: 7\ndata: a\n\n",
            "data: b\n\n",
            "data: c\n\n",
        ]);
        let mut bytes = SSE_HEAD.as_bytes().to_vec();
        bytes.extend_from_slice(&body);
        let out = read_all(&bytes, 2, 5);
        assert_eq!(
            out.frames,
            vec!["a".to_string(), "b".to_string()],
            "the budget stops at two"
        );
        assert_eq!(out.end, SseEnd::FrameBudgetReached);
    }

    #[test]
    fn a_peer_that_is_not_streaming_is_answered_immediately() {
        let bytes = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{}".to_vec();
        let out = read_all(&bytes, 64, 7);
        assert!(
            matches!(out.end, SseEnd::NotAnEventStream(ref ct) if ct.contains("application/json"))
        );
        assert_eq!(
            out.status,
            Some(200),
            "the status is still what the peer said"
        );
        assert!(out.frames.is_empty());
    }

    #[test]
    fn a_broken_head_is_malformed_not_a_silent_empty_stream() {
        let out = read_all(b"NOT-HTTP\r\n\r\n", 64, 3);
        assert!(matches!(out.end, SseEnd::Malformed(_)), "got {:?}", out.end);
        assert_eq!(out.status, None);
    }

    // A stream that goes quiet mid-event keeps what already arrived.
    #[test]
    fn a_deadline_keeps_the_frames_that_already_arrived() {
        let body = chunked(&["data: a\n\ndata: b\n\n"]);
        let mut bytes = SSE_HEAD.as_bytes().to_vec();
        bytes.extend_from_slice(&body[..body.len() - 5]); // truncated: no terminal chunk
        let mut r = SseReader::new(64, None);
        assert_eq!(r.feed(&bytes, 10), Step::NeedMore);
        let out = r.finish(SseEnd::Timeout);
        assert_eq!(out.frames, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(out.end, SseEnd::Timeout);
    }

    // Each frame is credited the moment its own bytes landed; a shared timestamp would flatten
    // the gaps to zero.
    #[test]
    fn each_frame_is_credited_the_arrival_of_its_own_bytes() {
        let mut r = SseReader::new(64, None);
        assert_eq!(r.feed(SSE_HEAD.as_bytes(), 0), Step::NeedMore);
        assert_eq!(r.feed(&chunked_open("data: a\n\n"), 100), Step::NeedMore);
        assert_eq!(r.feed(&chunked_open("data: b\n\n"), 250), Step::NeedMore);
        let out = r.finish(SseEnd::Timeout);
        assert_eq!(out.frames, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(
            out.frame_offsets_us,
            vec![100, 250],
            "the gap between frames is the measurement"
        );
    }

    // An event the peer never terminated is not a delivered frame: `finish` must not flush the
    // held `data:` fragment stamped with a fabricated close/deadline arrival time.
    #[test]
    fn an_event_the_peer_never_terminated_is_not_counted_as_a_delivered_frame() {
        let mut r = SseReader::new(64, None);
        assert_eq!(r.feed(SSE_HEAD.as_bytes(), 0), Step::NeedMore);
        assert_eq!(r.feed(&chunked_open("data: a\n\n"), 100), Step::NeedMore);
        // A COMPLETE data line that no blank line ever dispatched: the event the peer was still
        // in the middle of when it went quiet.
        assert_eq!(
            r.feed(&chunked_open("data: half-writ\n"), 250),
            Step::NeedMore
        );
        let out = r.finish(SseEnd::Timeout);
        assert_eq!(
            out.frames,
            vec!["a".to_string()],
            "only the dispatched event counts, got {:?}",
            out.frames
        );
        assert_eq!(
            out.frame_offsets_us,
            vec![100],
            "and no frame is stamped at the deadline, which would be a fabricated gap"
        );
        assert_eq!(out.end, SseEnd::Timeout);
    }

    // How the stream ended must not decide whether an un-dispatched fragment counts.
    #[test]
    fn a_fragment_left_by_a_terminal_chunk_is_dropped_the_same_way() {
        let mut r = SseReader::new(64, None);
        assert_eq!(r.feed(SSE_HEAD.as_bytes(), 0), Step::NeedMore);
        assert_eq!(r.feed(&chunked_open("data: a\n\n"), 100), Step::NeedMore);
        assert_eq!(
            r.feed(&chunked_open("data: cut off\n"), 250),
            Step::NeedMore
        );
        assert_eq!(r.feed(b"0\r\n\r\n", 300), Step::Done(SseEnd::StreamClosed));
        let out = r.finish(SseEnd::StreamClosed);
        assert_eq!(out.frames, vec!["a".to_string()]);
        assert_eq!(out.frame_offsets_us, vec![100]);
    }

    fn chunked_open(payload: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(format!("{:x}\r\n", payload.len()).as_bytes());
        out.extend_from_slice(payload.as_bytes());
        out.extend_from_slice(b"\r\n");
        out
    }

    // The two lanes must agree against the same peer: they share the decoder and request builder,
    // so this asserts that "who owns the waiting" is the only real difference between them.
    #[test]
    fn the_async_lane_reads_exactly_what_the_blocking_lane_reads() {
        let addr = sse_server_for_diff();
        let path = "/v1/chat/completions";
        let headers: Vec<(String, String)> = vec![("authorization".into(), "Bearer dummy".into())];

        let blocking = post_json_sse(
            addr,
            path,
            b"{}",
            &headers,
            Duration::from_secs(5),
            8,
            Some(crate::ingress::Dialect::Openai),
        );

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime for the async lane");
        let asynced = rt.block_on(post_json_sse_async(
            addr,
            path,
            b"{}",
            &headers,
            Duration::from_secs(5),
            8,
            Some(crate::ingress::Dialect::Openai),
        ));

        assert_eq!(
            asynced.status, blocking.status,
            "the two lanes disagree on the status"
        );
        assert_eq!(
            asynced.frames, blocking.frames,
            "the two lanes decoded different frames"
        );
        // The content classification travels with the decode, so a lane that classified differently
        // would publish a different delivery ratio from identical bytes.
        assert_eq!(
            asynced.content_frames, blocking.content_frames,
            "the two lanes classified different numbers of content frames"
        );
        assert_eq!(
            asynced.end, blocking.end,
            "the two lanes ended for different reasons"
        );
        assert_eq!(
            asynced.frame_offsets_us.len(),
            blocking.frame_offsets_us.len(),
            "the two lanes credited a different number of frames"
        );
        assert!(
            !blocking.frames.is_empty(),
            "the fixture must actually stream, or this proves nothing"
        );
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

    // ── the rig refuses to smuggle, on EVERY lane ────────────────────────────────────────────────

    /// The header and path shapes that would put something on the wire nobody asked for. Shared by
    /// the probe-lane and stream-lane tests below so the two cannot be given different hostile input
    /// and both look covered.
    fn smuggling_shapes() -> Vec<(&'static str, Vec<(String, String)>)> {
        let h = |n: &str, v: &str| vec![(n.to_string(), v.to_string())];
        vec![
            (
                "/v1/chat/completions",
                h("authorization", "Bearer t\r\nx-injected: yes"),
            ),
            (
                "/v1/chat/completions",
                h("authorization", "Bearer t\nx-injected: yes"),
            ),
            ("/v1/chat/completions", h("x-route\r\nx-injected", "yes")),
            ("/v1/chat/completions", h("x-route:extra", "yes")),
            ("/v1/chat/completions", h("x-route", "a\0b")),
            ("/v1/chat HTTP/1.1\r\nx-injected: yes", Vec::new()),
            ("/v1/chat completions", Vec::new()),
        ]
    }

    // RIG-12's other half: `send` (probe/re-verify) used to interpolate manifest headers raw even
    // after `gen.rs::build_request` was hardened. Must refuse as `RigRefused`, never
    // `ConnectionFailed`/`Malformed` - those describe the peer, and this is a fault of ours.
    #[test]
    fn the_probe_lane_refuses_a_request_it_would_have_to_smuggle() {
        // A live, well-behaved peer, so the refusal can't be an accident of nothing listening.
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}");
        });
        for (path, headers) in smuggling_shapes() {
            let outcome = post_json(addr, path, b"{}", &headers, Duration::from_secs(2));
            match &outcome {
                Outcome::RigRefused(why) => assert!(
                    why.contains("inject") || why.contains("request line"),
                    "the refusal must name what it prevented: {why}"
                ),
                other => {
                    panic!("{path:?} with {headers:?} was sent rather than refused: {other:?}")
                }
            }
        }
        // A GET goes through the same builder and gets the same answer.
        assert!(matches!(
            get(
                addr,
                "/__mock/state",
                &[("x\r\ny".into(), "z".into())],
                Duration::from_secs(2)
            ),
            Outcome::RigRefused(_)
        ));
    }

    // The rule must not refuse ordinary requests: failing closed on everything measures nothing.
    #[test]
    fn an_ordinary_request_still_goes_out() {
        assert_eq!(
            unsendable_request(
                "/v1/chat/completions",
                &[
                    ("authorization".into(), "Bearer sk-abc.def-123".into()),
                    (
                        "x-portkey-custom-host".into(),
                        "http://127.0.0.1:9099/v1".into()
                    ),
                    ("anthropic-version".into(), "2023-06-01".into()),
                ],
            ),
            None
        );
    }

    // The streaming lane was the third: `build_sse_request` is shared by both stream transports,
    // so one unenforced rule there covered both.
    #[test]
    fn the_streaming_lanes_refuse_a_request_they_would_have_to_smuggle() {
        let addr = spawn_server(|mut conn| {
            let _ = read_request_head(&conn);
            let _ = conn.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: {}\n\n",
            );
        });
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        for (path, headers) in smuggling_shapes() {
            let blocking = post_json_sse(
                addr,
                path,
                b"{}",
                &headers,
                Duration::from_secs(2),
                4,
                Some(crate::ingress::Dialect::Openai),
            );
            assert!(
                matches!(blocking.end, SseEnd::RigRefused(_)),
                "the blocking stream lane sent {path:?} with {headers:?}: {blocking:?}"
            );
            assert!(blocking.frames.is_empty() && blocking.status.is_none());

            let asynced = rt.block_on(post_json_sse_async(
                addr,
                path,
                b"{}",
                &headers,
                Duration::from_secs(2),
                4,
                Some(crate::ingress::Dialect::Openai),
            ));
            assert!(
                matches!(asynced.end, SseEnd::RigRefused(_)),
                "the async stream lane sent {path:?} with {headers:?}: {asynced:?}"
            );
        }
    }

    // ── content frames vs every event (ledger RIG-11) ────────────────────────────────────────────

    // The decoder counts content frames by asking the dialect, never by sniffing `[DONE]` itself -
    // the taxonomy belongs with the protocol.
    #[test]
    fn the_reader_counts_content_frames_by_asking_the_dialect() {
        // Exactly the shapes mock/src/main.rs emits for openai: role head, content deltas, the
        // finish_reason tail, then the [DONE] sentinel.
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"a\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"b\"}}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let bytes = format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n{body}");

        let mut r = SseReader::new(64, Some(crate::ingress::Dialect::Openai));
        let _ = r.feed(bytes.as_bytes(), 1);
        let o = r.finish(SseEnd::StreamClosed);
        assert_eq!(
            o.frames.len(),
            5,
            "every event is still counted in `frames`"
        );
        assert_eq!(
            o.content_frames, 2,
            "only the two deltas carried a token: {:?}",
            o.frames
        );

        // Without a dialect nothing is claimed, so this reads exactly as `frames` does.
        let mut r = SseReader::new(64, None);
        let _ = r.feed(bytes.as_bytes(), 1);
        let o = r.finish(SseEnd::StreamClosed);
        assert_eq!(o.content_frames, o.frames.len() as u64);
    }

    // ── a budget counted in CONTENT frames ───────────────────────────────────────────────────────

    /// A stream with `ping`-shaped framing between its tokens (openai spelling) - the shape a
    /// gateway with a keepalive, or one re-emitting a translated stream, puts on the wire.
    fn framed_stream(tokens: usize) -> String {
        let mut s = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n".to_string();
        s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n");
        for i in 0..tokens {
            s.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\n");
            s.push_str(&format!(
                "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"t{i}\"}}}}]}}\n\n"
            ));
        }
        s
    }

    // A content budget counts tokens, so framing cannot displace them. Under `Events(8)` this same
    // stream would stop having seen only three tokens (the rest spent on the role head and pings),
    // reading as a gateway that lost frames it never lost.
    #[test]
    fn a_content_budget_reads_past_framing_until_the_tokens_arrive() {
        let bytes = framed_stream(16);

        let mut r = SseReader::new(8usize, Some(crate::ingress::Dialect::Openai));
        let _ = r.feed(bytes.as_bytes(), 1);
        let o = r.finish(SseEnd::StreamClosed);
        assert_eq!(o.frames.len(), 8, "an event budget stops at 8 events");
        assert_eq!(
            o.content_frames, 3,
            "of which only three carried a token, the rest being the head and the pings: {:?}",
            o.frames
        );

        let mut r = SseReader::new(
            SseBudget::Content {
                frames: 8,
                event_ceiling: 64,
            },
            Some(crate::ingress::Dialect::Openai),
        );
        let step = r.feed(bytes.as_bytes(), 1);
        assert_eq!(step, Step::Done(SseEnd::FrameBudgetReached));
        let o = r.finish(SseEnd::StreamClosed);
        assert_eq!(o.content_frames, 8, "the tokens asked for arrived");
        assert_eq!(
            o.frames.len(),
            17,
            "and the framing was paid for in events, not in tokens: {:?}",
            o.frames
        );
    }

    // The ceiling is the only thing bounding a content budget; hitting it is a shortfall, so
    // `EventCeilingReached` must stay distinct from `FrameBudgetReached`.
    #[test]
    fn a_content_budget_stops_at_the_event_ceiling_and_says_which_bound_stopped_it() {
        // Framing only: the tokens this budget is waiting for never come.
        let mut bytes = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n".to_string();
        for _ in 0..100 {
            bytes.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\n");
        }
        let mut r = SseReader::new(
            SseBudget::Content {
                frames: 8,
                event_ceiling: 10,
            },
            Some(crate::ingress::Dialect::Openai),
        );
        let step = r.feed(bytes.as_bytes(), 1);
        assert_eq!(step, Step::Done(SseEnd::EventCeilingReached));
        let o = r.finish(SseEnd::StreamClosed);
        assert_eq!(o.frames.len(), 10, "the read is bounded by the ceiling");
        assert_eq!(o.content_frames, 0, "and it delivered nothing");
    }

    // A dialect with no taxonomy of its own calls every event content (`sse_event_is_content`), so a
    // content budget must degenerate to exactly the event budget it replaced - the four dialects the
    // mock never streams natively cannot be moved by this change.
    #[test]
    fn a_content_budget_without_a_dialect_reads_exactly_as_an_event_budget_does() {
        let bytes = framed_stream(16);
        let mut events = SseReader::new(8usize, None);
        let _ = events.feed(bytes.as_bytes(), 1);
        let events = events.finish(SseEnd::StreamClosed);

        let mut content = SseReader::new(
            SseBudget::Content {
                frames: 8,
                event_ceiling: 64,
            },
            None,
        );
        let _ = content.feed(bytes.as_bytes(), 1);
        let content = content.finish(SseEnd::StreamClosed);
        assert_eq!(events, content);
    }
}

#[cfg(test)]
mod head_cap_tests {
    use super::*;

    // Drives the actual reader against a peer that does the thing the cap exists for: endless
    // short, legal header lines.
    #[test]
    fn an_endless_header_stream_is_refused_rather_than_accumulated() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            // Consume the request head so the client is not blocked on its own write.
            let mut r = BufReader::new(sock.try_clone().expect("clone"));
            let mut line = String::new();
            while r.read_line(&mut line).unwrap_or(0) > 0 {
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                line.clear();
            }
            let _ = sock.write_all(b"HTTP/1.1 200 OK\r\n");
            // Endless legal headers. If the cap does not fire the client reads until its deadline.
            loop {
                if sock
                    .write_all(b"X-Pad: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n")
                    .is_err()
                {
                    break;
                }
            }
        });

        let mut stream = std::net::TcpStream::connect(addr).expect("connect");
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("write");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let got = read_head(&mut stream, deadline);
        drop(stream);
        let _ = server.join();

        let err = got.expect_err("an endless header stream must be refused, not accumulated");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("exceeds the") && msg.contains("byte cap"),
            "the refusal must name the cap that fired rather than reading to the deadline: {msg}"
        );
    }
}

#[cfg(test)]
mod head_terminator_tests {
    use super::*;

    // A CRLF pair in the body must not be mistaken for the end of the head: an LF-terminated head
    // followed by CRLF-terminated frames used to lose its first frame and credit TTFT to the second.
    #[test]
    fn the_earlier_blank_line_ends_the_head_whichever_form_it_takes() {
        let mixed = b"HTTP/1.1 200 OK\nContent-Type: text/event-stream\n\ndata: alpha\r\n\r\ndata: beta\n\n";
        let end = find_head_end(mixed).expect("head must be found");
        let body = &mixed[end..];
        assert!(
            body.starts_with(b"data: alpha"),
            "the first frame must survive; body began {:?}",
            String::from_utf8_lossy(&body[..body.len().min(24)])
        );
    }

    /// The ordinary all-CRLF head must still end at its own blank line, not at a later one.
    #[test]
    fn a_normal_crlf_head_ends_at_its_own_blank_line() {
        let normal = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\ndata: one\r\n\r\n";
        let end = find_head_end(normal).expect("head must be found");
        assert!(normal[end..].starts_with(b"data: one"));
    }

    /// And an all-LF stream is unchanged.
    #[test]
    fn an_all_lf_head_is_unchanged() {
        let lf = b"HTTP/1.1 200 OK\nx: 1\n\ndata: one\n\n";
        let end = find_head_end(lf).expect("head must be found");
        assert!(lf[end..].starts_with(b"data: one"));
    }

    /// An incomplete head is still "not yet", not a wrong answer.
    #[test]
    fn a_head_that_has_not_arrived_yet_is_none() {
        assert_eq!(
            find_head_end(b"HTTP/1.1 200 OK\r\ncontent-type: text/"),
            None
        );
    }
}
