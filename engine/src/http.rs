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

/// Reads exactly `n` bytes (a known Content-Length or chunk body), honouring `deadline`.
fn read_exact_deadline(stream: &mut TcpStream, deadline: Instant, n: usize) -> ReadOutcome {
    let mut buf = vec![0u8; 0];
    buf.reserve(n);
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
        if let Some(kv) = parse_header_line(&line) {
            headers.push(kv);
        }
        // An unparseable header line is tolerated (skipped): a stray informational line here
        // should not sink a response that otherwise has a perfectly good status and body.
    }
    Ok((status, headers, raw))
}

/// Reads a chunked (`Transfer-Encoding: chunked`) body to completion.
fn read_chunked_body(stream: &mut TcpStream, deadline: Instant, raw: &[u8]) -> Outcome {
    let mut body = Vec::new();
    let mut seen = raw.to_vec();
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
    let deadline = Instant::now() + timeout;

    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        // A refused or unreachable connection is the one case that is unambiguously "never
        // reached": no bytes of ours ever left for the peer to act on.
        Err(e) => return Outcome::ConnectionFailed(e.to_string()),
    };

    let mut request = Vec::new();
    request.extend_from_slice(format!("POST {path} HTTP/1.1\r\n").as_bytes());
    request.extend_from_slice(format!("Host: {addr}\r\n").as_bytes());
    request.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseOutcome {
    pub status: Option<u16>,
    pub frames: Vec<String>,
    pub end: SseEnd,
}

/// POSTs like `post_json`, then reads Server-Sent-Event `data:` frames off the response body
/// until `frame_budget` frames have been seen or `timeout` elapses, whichever comes first.
///
/// DELIBERATELY NOT SUPPORTED: a chunked (`Transfer-Encoding: chunked`) event stream. Every SSE
/// probe this harness drives writes `data: ...\n\n` frames straight onto the connection as they
/// are produced, which is how both the recording mock and every dialect under test behave; chunk
/// framing on top of that would need chunk boundaries decoded live against the same deadline this
/// function already tracks per byte, for a case nothing in this harness produces. Should a target
/// ever chunk-encode its stream, this reads the chunk-size lines as if they were frame noise
/// (they will not start with "data:") and skips them, so the probe degrades to under-counting
/// frames rather than hanging or crashing.
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
                end: SseEnd::ConnectionFailed(e.to_string()),
            }
        }
    };

    let mut request = Vec::new();
    request.extend_from_slice(format!("POST {path} HTTP/1.1\r\n").as_bytes());
    request.extend_from_slice(format!("Host: {addr}\r\n").as_bytes());
    request.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    request.extend_from_slice(b"Connection: close\r\n");
    for (name, value) in headers {
        request.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    request.extend_from_slice(b"\r\n");
    request.extend_from_slice(body);

    let write_deadline = deadline.saturating_duration_since(Instant::now());
    if stream.set_write_timeout(Some(write_deadline)).is_err() {
        return SseOutcome {
            status: None,
            frames: Vec::new(),
            end: SseEnd::Timeout,
        };
    }
    if let Err(e) = stream.write_all(&request) {
        return if is_timeout(&e) {
            SseOutcome {
                status: None,
                frames: Vec::new(),
                end: SseEnd::Timeout,
            }
        } else {
            SseOutcome {
                status: None,
                frames: Vec::new(),
                end: SseEnd::ConnectionFailed(format!(
                    "connection dropped while sending the request: {e}"
                )),
            }
        };
    }

    let status = match read_head(&mut stream, deadline) {
        Ok((status, _headers, _raw)) => status,
        Err(Outcome::TimedOut) => {
            return SseOutcome {
                status: None,
                frames: Vec::new(),
                end: SseEnd::Timeout,
            }
        }
        Err(Outcome::ConnectionFailed(msg)) => {
            return SseOutcome {
                status: None,
                frames: Vec::new(),
                end: SseEnd::ConnectionFailed(msg),
            }
        }
        Err(Outcome::Malformed { message, .. }) => {
            return SseOutcome {
                status: None,
                frames: Vec::new(),
                end: SseEnd::Malformed(message),
            }
        }
        Err(Outcome::Response(_)) => {
            unreachable!("read_head never returns Ok wrapped as Err(Response)")
        }
    };

    let mut frames = Vec::new();
    loop {
        if frames.len() >= frame_budget {
            return SseOutcome {
                status: Some(status),
                frames,
                end: SseEnd::FrameBudgetReached,
            };
        }
        match read_line(&mut stream, deadline) {
            ReadOutcome::Full(line) => {
                let text = String::from_utf8_lossy(strip_crlf(&line));
                if let Some(data) = text.strip_prefix("data:") {
                    frames.push(data.trim_start().to_string());
                }
                // Any other line (event:, id:, a blank separator, chunk-size noise) is not a data
                // frame and is silently skipped; the probe only ever needs the data frames.
            }
            ReadOutcome::TimedOut(_) => {
                return SseOutcome {
                    status: Some(status),
                    frames,
                    end: SseEnd::Timeout,
                }
            }
            ReadOutcome::Eof(_) => {
                return SseOutcome {
                    status: Some(status),
                    frames,
                    end: SseEnd::StreamClosed,
                }
            }
            ReadOutcome::Err(_) => {
                return SseOutcome {
                    status: Some(status),
                    frames,
                    end: SseEnd::StreamClosed,
                }
            }
        }
    }
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
}
