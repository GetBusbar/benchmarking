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
    /// THE RIG REFUSED TO SEND, and this is the one variant that describes US.
    ///
    /// Every other variant is a claim about the PEER - it refused the connection, it never answered,
    /// it sent bytes that do not parse. This one is the opposite: the request could not be framed
    /// without smuggling something onto the wire, so nothing was sent and the gateway was never
    /// asked. Reusing `ConnectionFailed` or `Malformed` for it would charge a gateway with a fault
    /// of ours, which is the exact attribution inversion this engine refuses everywhere else.
    ///
    /// A caller must therefore never grade this as the gateway failing. It is loud on purpose: the
    /// manifest that caused it is a first-party file with a defect in it, and the run should stop
    /// pointing at that rather than publish a red.
    RigRefused(String),
    /// A complete, well-formed HTTP response, whatever its status.
    Response(HttpResponse),
    /// The TCP connection itself could not be made or was severed before any response existed.
    /// The gateway may genuinely never have seen the request; this must never carry a status.
    ConnectionFailed(String),
    /// THIS HOST could not open the connection: no ephemeral source port or no descriptor left
    /// (see `connect_error_is_ours`). Like `RigRefused` it describes US - the gateway was never
    /// asked - and the streaming lane already says so (`SseEnd::RigExhausted`); folding it into
    /// `ConnectionFailed` worded our exhaustion as "no connection to the gateway", which the grid
    /// reads as a gateway that died and restarts it for.
    RigExhausted(String),
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
                // Reported as the cap firing, NOT as Eof: every caller words Eof as "connection
                // closed", which would name the wrong fault.
                if buf.len() > MAX_LINE_BYTES {
                    return ReadOutcome::Err(io::Error::other(format!(
                        "a line exceeded the {MAX_LINE_BYTES} byte cap without a newline"
                    )));
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

/// Ceiling on ONE line `read_line` will accumulate (status line, header, chunk-size line).
const MAX_LINE_BYTES: usize = 64 * 1024;

/// Ceiling on the chunk-size lines of one chunked body, summed. 1 MiB is ~200,000 ordinary size
/// lines; a probe body is kilobytes, so only a peer padding its framing (extensions on tiny chunks)
/// gets near it.
const MAX_CHUNK_FRAMING_BYTES: usize = 1024 * 1024;

/// Hard ceiling on any response body we will accumulate from the gateway under test.
///
/// The gateway is arbitrary third-party software and its response length is ITS claim, not ours. An
/// allocation failure calls abort() unconditionally: not a panic, nothing catches it, and an
/// eight-hour run dies with no operator watching. A probe response has no legitimate reason to
/// approach this, so exceeding it is Malformed rather than a measurement.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Ceiling on the size of a response HEAD - status line plus all header lines together.
///
/// `read_line` caps ONE line at 64 KiB and `MAX_BODY_BYTES` caps every body framing, but nothing capped
/// the NUMBER of header lines: a peer answering with a status line followed by an endless stream of
/// short, legal headers grew `raw` and the `headers` Vec until the probe deadline, with only that
/// 10-second timeout standing between the engine and its own memory.
///
/// The peer here is one of fourteen third-party gateways, several built from upstream at whatever
/// revision the manifest pins - this file already treats it as untrusted with the engine's address
/// space. A header-accumulation loop (a proxy echoing Via or Set-Cookie on an internal retry) is a
/// mundane bug in somebody else's code that should cost that gateway its cell, not the whole run.
///
/// 256 KiB is far above any real response head and far below anything that matters to the box.
const MAX_HEAD_BYTES: usize = 256 * 1024;

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
    // One scratch buffer for the whole read, not a fresh zero-filled one per iteration.
    let mut chunk = vec![0u8; n.min(64 * 1024)];
    while buf.len() < n {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return ReadOutcome::TimedOut(buf);
        }
        if stream.set_read_timeout(Some(remaining)).is_err() {
            return ReadOutcome::TimedOut(buf);
        }
        let want = (n - buf.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
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

/// A Content-Length header value as a length: one decimal number, or a comma list of identical
/// ones (RFC 9110 section 8.6). `None` for anything else - negative, non-numeric, or disagreeing.
fn content_length_value(v: &str) -> Option<usize> {
    let mut parts = v.split(',').map(|p| p.trim().parse::<usize>().ok());
    let first = parts.next()??;
    parts.all(|p| p == Some(first)).then_some(first)
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
        // THE FRAMING IS CAPPED TOO, not only the decoded `body`. Each size line may be up to
        // MAX_LINE_BYTES (chunk extensions included), and a peer sending 1-byte chunks behind 64 KiB
        // size lines grows `seen` ~65,000 times faster than `body`, so the body cap alone would let
        // it reach gigabytes before firing.
        if seen.len() - raw.len() - body.len() > MAX_CHUNK_FRAMING_BYTES {
            return malformed(
                &seen,
                format!(
                    "chunk framing exceeded the {MAX_CHUNK_FRAMING_BYTES} byte cap across {chunk_count} chunk(s)"
                ),
            );
        }
        let size_text = std::str::from_utf8(strip_crlf(&size_line)).unwrap_or("");
        // Chunk extensions ("1a;foo=bar") are legal; only the hex size before ';' matters here.
        let size_hex = size_text.split(';').next().unwrap_or("").trim();
        let size = match usize::from_str_radix(size_hex, 16) {
            Ok(n) => n,
            Err(_) => return malformed(&seen, format!("unparseable chunk size {size_hex:?}")),
        };
        if size == 0 {
            // Trailing headers (rare, usually absent) run up to the final blank line.
            //
            // CAPPED FOR THE SAME REASON THE RESPONSE HEAD IS, and this loop was missed when that cap
            // was added: `read_line` bounds ONE line at 64 KiB and `MAX_BODY_BYTES` bounds `body`, but
            // nothing bounded the NUMBER of trailer lines or the growth of `seen`. A peer that sends a
            // terminal `0\r\n` and then legal trailers forever, never sending the closing blank line,
            // grows this at loopback speed for the whole deadline. Fixing read_head's loop and leaving
            // this one is the half-fix shape this audit keeps finding.
            //
            // THE TRAILER ONLY. `seen` already holds the head, every chunk and every size line, so
            // comparing all of it to the head cap refused every valid chunked response whose body
            // was over ~256 KiB the moment its terminal chunk arrived.
            let trailer_from = seen.len();
            loop {
                if seen.len() - trailer_from > MAX_HEAD_BYTES {
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
        // Each chunk body is followed by a bare CRLF before the next chunk size line. Anything else
        // means the declared size did not match the data, and reading on would decode the rest of
        // the body out of step with its framing.
        match read_line(stream, deadline) {
            ReadOutcome::Full(line) if strip_crlf(&line).is_empty() => {}
            ReadOutcome::Full(line) => {
                seen.extend_from_slice(&line);
                return malformed(
                    &seen,
                    "a chunk body was not followed by CRLF - its declared size does not match its data",
                );
            }
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
pub fn get(
    addr: SocketAddr,
    path: &str,
    headers: &[(String, String)],
    timeout: Duration,
) -> Outcome {
    send("GET", addr, path, &[], headers, timeout, false)
}

/// WHY THIS REQUEST CANNOT BE PUT ON THE WIRE, or `None` when it can.
///
/// Every request this engine sends is assembled by interpolating a manifest-supplied path and
/// manifest-supplied header pairs into the HTTP framing with `format!`. HTTP/1.1 has exactly one
/// terminator for a header and one for the request line, so a `\r` or `\n` anywhere in those strings
/// does not produce a header with a strange value - it produces EXTRA HEADERS, or a whole second
/// request, chosen by whoever wrote the manifest rather than by the harness. A `:` inside a header
/// NAME renames the header and turns the rest into a value. A space in the path rewrites the request
/// line's HTTP version. NUL is refused for the same reason a name is: nothing downstream of this
/// process has to agree with us about where a C string ends.
///
/// ONE VALIDATOR, THREE LANES. Ledger RIG-12 was closed for the load lane only: `gen.rs`'s
/// `build_request` grew these rules while `send` (the probe and re-verify lanes) and
/// `build_sse_request` (the streaming lane) kept interpolating the SAME manifest headers raw. A rule
/// enforced on one of three lanes is not enforced; it is a lane a reader thinks is covered. This is
/// the rule, and all three call it.
///
/// The answer is prose because the only honest thing to do with it is refuse loudly and name the
/// header - manifests are first-party files, so this is a defect to fix in one, never a gateway
/// property to publish.
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
    // REFUSED BEFORE THE CONNECT, so a request we will not send never even touches the gateway: a
    // connection opened and abandoned is a connection the peer logged and had to clean up for a
    // request of ours that was never valid.
    if let Some(why) = unsendable_request(path, headers) {
        return Outcome::RigRefused(why);
    }

    let deadline = Instant::now() + timeout;

    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        // OUR OWN EXHAUSTION FIRST: no port or descriptor on this host is not the peer declining
        // anything, and it must not be worded as the gateway being unreachable.
        Err(e) if connect_error_is_ours(&e) => return Outcome::RigExhausted(e.to_string()),
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

    // A Content-Length that is PRESENT but not a length is a framing fault of the peer's, not an
    // absent header: reading to close instead would accept whatever arrived as a complete body.
    // A list of identical values ("5, 5") is the one recoverable form (RFC 9110 section 8.6).
    let content_length = match header_value(&resp_headers, "content-length") {
        None => None,
        Some(v) => match content_length_value(v) {
            Some(n) => Some(n),
            None => {
                return malformed(&raw, format!("unparseable Content-Length {v:?}"));
            }
        },
    };
    if let Some(len) = content_length {
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
    /// The read was budgeted in CONTENT frames (`SseBudget::Content`) and hit the total-event ceiling
    /// before that many tokens arrived.
    ///
    /// A DELIVERY SHORTFALL, NOT AN ERRORED STREAM, and the two must stay apart: the peer answered
    /// 200, framed correctly and kept sending - it just spent the whole ceiling on events that
    /// carried no token. `stream_errored` therefore leaves it alone and the delivery ratio fails the
    /// gate on the count, which is the honest reading. It is one of two ends that say the read
    /// stopped for OUR bound rather than the stream's own (`FrameBudgetReached` is the other, and
    /// the satisfied version of it): a caller seeing this knows the ceiling
    /// binds here and can weigh the counts accordingly.
    EventCeilingReached,
    /// The deadline passed. On a stream that goes quiet this is expected and is not an error by
    /// itself: `frames` still reports whatever arrived before then, which must not be discarded
    /// just because the stream never explicitly finished.
    Timeout,
    /// The peer closed the connection (a normal, deliberate end of stream).
    StreamClosed,
    /// The connection could not be made at all - by the PEER's doing (refused, unreachable, reset).
    ConnectionFailed(String),
    /// The connection could not be made because THIS HOST ran out: ephemeral source ports
    /// (EADDRNOTAVAIL) or file descriptors (EMFILE/ENFILE).
    ///
    /// Split from `ConnectionFailed` because they are opposite claims. A refused connection is the
    /// gateway declining; a host with no source ports left never asked it anything. Both used to
    /// count as an errored stream, so a stream search at high concurrency could exhaust the rig and
    /// publish the exhaustion as the gateway's stream ceiling - the same defect the load generator
    /// had, in the path that reaches high concurrency soonest.
    RigExhausted(String),
    /// THE RIG REFUSED TO SEND. Not a claim about the peer at all - see `Outcome::RigRefused`, which
    /// is the same fact on the non-streaming lanes.
    ///
    /// Distinct from `RigExhausted`, which is also ours but is a resource limit reached honestly
    /// mid-run. This one is a request the harness would have had to smuggle to send, so it did not
    /// send it: no connection was made and the gateway was never asked. It must never count toward
    /// an errored stream or a failing rung.
    RigRefused(String),
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
    /// How many of `frames` carried MODEL OUTPUT rather than protocol scaffolding, as the request's
    /// own dialect classifies them (`ingress::Dialect::sse_event_is_content`).
    ///
    /// Ledger RIG-11: `frames` counts every dispatched SSE event, which is right for anything that
    /// wants the whole stream (fps, gap timings, "did it stream at all") and wrong for a DELIVERY
    /// ratio - openai spends 3 events and anthropic 5 on framing, so a stream could satisfy part of
    /// a frame budget having delivered no tokens, and the two dialects differed by two.
    ///
    /// Equals `frames.len()` when the caller passed no dialect: no taxonomy was supplied, so nothing
    /// is claimed about which events were content and this reads exactly as `frames` does.
    pub content_frames: u64,
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

/// WHAT STOPS THE READ: a count of dispatched events, or a count of CONTENT frames with a ceiling on
/// the events spent getting them.
///
/// Ledger RIG-11's remainder, and the half a classifier alone could not fix. `Events` was the only
/// mode, and `run::stream_window` divides the content frames it collected by a denominator computed
/// as `STREAM_FRAME_BUDGET - Dialect::stream_prelude_frames()` - a CONSTANT read off the mock's own
/// layout. The numerator, though, is measured on the GATEWAY's stream, and under `Events` every
/// non-content event the gateway emits beyond the mock's prelude consumes a budget slot and displaces
/// exactly one content frame. Against `STREAM_MIN_DELIVERY_RATIO = 1.0` that is not a rounding
/// difference, it is a rung that fails on arithmetic: anthropic's real SSE protocol sends `ping`
/// events, a TRANSLATION cell has the gateway re-emitting the stream in the client's dialect with
/// framing that is ITS own rather than the mock's, and any gateway with a keepalive does the same.
/// A gateway that lost nothing failed at every rung, and the delivery shortfall the board published
/// was ours.
///
/// `Content` asks the question the metric is actually asking - "did every token arrive" - by reading
/// until the tokens arrive. A gateway that inserts framing then spends more EVENTS to deliver the
/// same content, and the ratio reflects delivery instead of the gateway's framing style. It is the
/// same correction `STREAM_STALL_MULTIPLIER` (2 -> 10) got on the other clause of the same gate: a
/// bound calibrated on the mock's behaviour, applied to gateways that do not share it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseBudget {
    /// Stop after this many dispatched events, whatever they carried. What every non-delivery caller
    /// wants: the `Streaming` group's gap distribution reads the stream AS FRAMED, and a TTFT sample
    /// wants the first event off the wire.
    Events(usize),
    /// Stop once `frames` events the dialect classifies as CONTENT have arrived, or at `event_ceiling`
    /// total events, whichever comes first.
    ///
    /// BOUNDED, because the ceiling is the only thing between this and a peer that pings forever.
    /// `SseEnd::EventCeilingReached` says which bound stopped the read, and hitting it short of
    /// `frames` is a real delivery shortfall that must still fail the gate.
    ///
    /// With no dialect every event counts as content (`SseReader::dialect`), so this degenerates to
    /// `Events(frames)` under the ceiling - which is exactly right for the four dialects the mock
    /// never streams.
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
    /// delimiter and proven not to contain one. Searches resume here instead of at index 0,
    /// because a peer that trickles a few bytes per read otherwise makes every feed() rescan
    /// the whole accumulated buffer - O(fragments * bytes) inside the timed streaming window
    /// whose TTFT/gap numbers are published. Mirrors gen.rs::read_response's `scanned` cursor.
    /// The stored value is the full scanned length; the terminator-length overlap is applied at
    /// read time, same as gen.rs. Reused by whichever phase owns the buffer: the head is fully
    /// drained (cursor back to 0) before Phase::Chunked/Identity begins.
    raw_scanned: usize,
    body_scanned: usize,
    status: Option<u16>,
    /// Data lines accumulated for the event that has not been dispatched yet.
    pending: Option<String>,
    /* EVENT BOUNDARIES SEEN, INCLUDING THE ONES CARRYING NO `data:`.
    `frames` only grows for events that carried a data line, so the `Content` budget's event
    ceiling - the thing standing between this read and a peer that pings forever - could not see
    a comment keepalive (`:\n\n`) or a bare `event: ping\n\n` at all. Those are the canonical SSE
    keepalive and what most reverse proxies inject, so against such a peer the ceiling never
    fired and every lane in the window burned the full STREAM_TIMEOUT instead of returning
    `EventCeilingReached`. Counting dispatches rather than data frames makes the documented bound
    actually hold. */
    events_seen: usize,
    frames: Vec<String>,
    /// Total bytes retained across `frames`, capped at `MAX_BODY_BYTES` like every other buffer.
    frames_bytes: usize,
    offsets_us: Vec<u64>,
    budget: SseBudget,
    /// Which wire dialect these events are in, when the caller knows. The decoder does NOT inspect
    /// payloads itself - it asks the dialect (`sse_event_is_content`), because a taxonomy of events
    /// belongs to the protocol and this state machine is deliberately ignorant of both transport and
    /// protocol. `None` means no taxonomy was supplied and every event counts, exactly as `frames`
    /// does.
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
            frames_bytes: 0,
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
                        // The terminal chunk ends the BODY, and any `data:` lines still held were
                        // never dispatched: SSE dispatches on the blank line. Held lines are dropped
                        // here for the same reason `finish` drops them - a fragment the peer never
                        // terminated is not a frame it delivered - so which way the stream ended
                        // cannot change whether the fragment counts.
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

    /// The peer stopped sending, or the deadline passed. Whatever arrived still counts: a stream
    /// that goes quiet is not an error, and discarding its DISPATCHED frames would publish nothing
    /// for a gateway that streamed perfectly well up to that point.
    ///
    /// AN EVENT THE PEER NEVER TERMINATED IS NOT A DELIVERED FRAME. This used to flush the held
    /// `data:` lines as one more frame, stamped at the close or the deadline - so a gateway that
    /// died mid-event was credited with delivering the fragment that killed it, and the fabricated
    /// arrival time (the timeout instant, up to the whole stream timeout after the last real byte)
    /// entered the gap samples as a stall no frame arrival ever produced. Dropped rather than
    /// stamped: SSE dispatches on the blank line, so a fragment is an event that never happened, and
    /// there is no honest arrival time for a frame the peer never finished writing.
    ///
    /// Takes no arrival time on purpose: there is nothing left here that could honestly be stamped
    /// with one, and a parameter for it is an invitation to stamp something again.
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
                // THE ONE BUFFER `MAX_BODY_BYTES` DID NOT COVER.
                //
                // That cap guards `raw` and `body`, and both DRAIN INTO this one via `take_front` - so
                // the bound did not transfer. A peer that sends `data:` lines and never the blank line
                // that ends a frame grows `pending` for as long as the deadline allows, and this
                // file's own note on the close-delimited path spells out where that goes: the
                // allocator's failure handler calls abort() unconditionally, so it is not a panic the
                // harness can catch, it is the whole run dying. Malformed rather than a new variant:
                // an event stream with no frame boundary is exactly a framing fault of the peer's.
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
                // THE AGGREGATE, not only each frame: `pending` is capped per frame, but every
                // dispatched frame is kept until the budget ends, so a peer sending near-cap frames
                // held budget x 8 MiB per lane.
                if self.frames_bytes > MAX_BODY_BYTES {
                    return Some(self.finish_with(SseEnd::Malformed(format!(
                        "the SSE frames retained exceeded the {MAX_BODY_BYTES} byte cap across {} \
                         frame(s)",
                        self.frames.len()
                    ))));
                }
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
            self.frames_bytes += data.len();
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
    /* THE EARLIER TERMINATOR WINS, because both searches run over a buffer that already contains
    BODY bytes from the same read. This used to consult the bare-LF form only when no `\r\n\r\n`
    existed ANYWHERE, so a CRLF pair occurring later in the SSE body beat the bare-LF blank line
    that actually ended the head - and everything between them was drained as if it were head.

    Concretely, for a peer that sends an LF-terminated head and CRLF-terminated frames (both legal;
    SSE permits CRLF line endings) arriving in one segment:

        HTTP/1.1 200 OK\nContent-Type: text/event-stream\n\ndata: alpha\r\n\r\ndata: beta\n\n

    the head was taken as ending after `data: alpha`, that frame was never seen by the body
    reader, and time-to-first-token was credited to the SECOND frame. A silently lost frame and an
    overstated TTFT, on a gateway doing nothing wrong.

    `\r\n\r\n` is `0D 0A 0D 0A` and does not contain `0A 0A`, so the two searches cannot match
    inside one another and `min` is exactly "whichever blank line came first". */
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
/// Decodes `Transfer-Encoding: chunked` framing (see `SseReader::pump_chunked`) before splitting SSE
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
    /* RUNNING OUT OF OUR OWN CLOCK IS A TIMEOUT, NOT THE PEER REFUSING.
    `set_write_timeout` rejects a zero duration (`InvalidInput`, "cannot set a 0 duration
    timeout"), and `write_deadline` is exactly zero whenever the connect consumed the whole
    budget - routine at the concurrencies `stream_window` drives. This returned
    `ConnectionFailed`, whose own doc two hundred lines up says "by the PEER's doing (refused,
    unreachable, reset)", so `run::StreamErrorKinds` filed it under `connect_failed` and
    `stream_errored` counted it against the gateway. A lane that ran out of OUR clock was charged
    to them.

    The blocking non-streaming lane already calls this same event `Outcome::TimedOut`; the two
    lanes simply disagreed. `Timeout` is also what the caller gets if the deadline expires one
    microsecond later, so this removes a discontinuity as well as a misattribution. */
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

    /* ONE DEADLINE FOR THE WHOLE LANE, exactly as the blocking lane does.
    This gave `timeout` to the connect, another `timeout` to the write, and then started a FRESH
    `timeout` for the reads - so a lane could legitimately run to 3x what its caller asked for,
    while this function's own doc says "the only thing that differs between the two lanes is who
    owns the waiting". `stream_window` sizes its search on the assumption that a lane is bounded
    by STREAM_TIMEOUT, so an overrunning window costs the whole run wall-clock the schedule never
    budgeted, and the two lanes' timeout-derived samples were not the same quantity. */
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

    // Clock from the WRITE, exactly as the blocking lane does, so a slow handshake is not charged to
    // the gateway's first token and the two lanes' numbers mean the same thing. The DEADLINE is the
    // lane's, set before the connect; only the TTFT origin is the write.
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

/// WHICH SIDE FAILED TO CONNECT.
///
/// EADDRNOTAVAIL means this host has no ephemeral source port left; EMFILE/ENFILE mean it has no
/// descriptor left. Neither is the gateway declining anything - it was never asked. Everything else
/// (refused, unreachable, reset) is the peer's, and stays a connection failure.
fn connect_end(e: &std::io::Error) -> SseEnd {
    if connect_error_is_ours(e) {
        SseEnd::RigExhausted(e.to_string())
    } else {
        SseEnd::ConnectionFailed(e.to_string())
    }
}

/// Whether a connect error is THIS HOST running out rather than the peer: EADDRNOTAVAIL/EADDRINUSE
/// (no ephemeral source port left), EMFILE/ENFILE (no descriptor left). One rule for the streaming
/// lane (`connect_end`) and the request lane (`send`), so the two cannot disagree about whose fault
/// a failed connect was.
fn connect_error_is_ours(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::AddrNotAvailable | std::io::ErrorKind::AddrInUse
    ) || matches!(e.raw_os_error(), Some(23) | Some(24))
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
    // THE PROBE AND THE LOAD MUST SEND THE SAME REQUEST, matching gen.rs's build_request: a gateway
    // that requires content-type on a JSON body would otherwise answer 415 to the probe and be
    // published as NOT SERVING a pairing it would have loaded fine.
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
#[path = "tests/http_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/http_head_cap_tests.rs"]
mod head_cap_tests;

#[cfg(test)]
#[path = "tests/http_head_terminator_tests.rs"]
mod head_terminator_tests;
