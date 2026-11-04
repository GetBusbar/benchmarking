// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE STATS-LINE SHAPE: `otb loadgen`'s own stdout (`gen.rs`) is a `k=v` line, and `run.rs`'s
// `load_window` parses it through `parse_ugen_line`, the wire contract between the loadgen child
// process and the engine that reads its stdout. Empty output is `Absent(NotMeasured)`, never a
// zero: that is what a timeout kill, a crash, or a hard-down gateway all look like, and none of
// them measured anything.

use crate::measurement::{Absent, Measurement};
use std::collections::BTreeMap;

/// Split a stats line into its `k=v` pairs. Unrecognised keys are kept (and simply never looked up),
/// which is how an unfamiliar future field is ignored rather than rejected.
fn parse_kv(line: &str) -> BTreeMap<&str, &str> {
    line.split_whitespace()
        .filter_map(|tok| tok.split_once('='))
        .collect()
}

/// The rate is the one field that is legitimately fractional - see `UgenStats::rps`. Kept beside
/// `require_i64` rather than replacing it: every OTHER field on this line is a count, and a count that
/// arrives fractional is a corrupt line that should still be refused.
fn require_f64(fields: &BTreeMap<&str, &str>, key: &str, line: &str) -> Result<f64, String> {
    match fields.get(key) {
        None => Err(format!("missing field '{key}' in stats line: '{line}'")),
        Some(v) => v
            .parse::<f64>()
            .map_err(|_| format!("non-numeric field '{key}={v}' in stats line: '{line}'"))
            .and_then(|n| {
                // A non-finite rate is the child reporting something impossible; refuse it here rather
                // than letting an infinity reach a cast that would SATURATE into a plausible-looking
                // 9,223,372,036,854,775,807.
                if n.is_finite() {
                    Ok(n)
                } else {
                    Err(format!(
                        "non-finite field '{key}={v}' in stats line: '{line}'"
                    ))
                }
            }),
    }
}

/// A count an older generator may not print. ABSENT is 0 ("none reported", see `UgenStats`); PRESENT
/// must be a count like every required field. A malformed value used to read as 0 too, so a garbled
/// `rigrefused=` stopped the window being discarded as rig-limited and charged the rig's refused
/// connects to the gateway.
fn optional_i64(fields: &BTreeMap<&str, &str>, key: &str, line: &str) -> Result<i64, String> {
    match fields.get(key) {
        None => Ok(0),
        Some(_) => require_i64(fields, key, line),
    }
}

fn require_i64(fields: &BTreeMap<&str, &str>, key: &str, line: &str) -> Result<i64, String> {
    match fields.get(key) {
        None => Err(format!("missing field '{key}' in stats line: '{line}'")),
        Some(v) => v
            .parse::<i64>()
            .map_err(|_| format!("non-numeric field '{key}={v}' in stats line: '{line}'")),
    }
}

/// The throughput lane's stats, from the stats line:
/// `rps=<f64> fail=%d p50=%.2f p99=%.2f p50us=%d p99us=%d ok=%d`,
/// optionally followed by `rigrefused=%d budgetexceeded=%d spawnfailed=%d`.
///
/// `rps` IS NOT AN INTEGER ON THE WIRE - it is printed from an f64 with `{}`, so a window that
/// completed one request in four seconds sends `rps=0.25`. This line is the protocol's only
/// specification, and it declared `%d` here long after the producer stopped emitting one; a second
/// generator written against the stale text would print `rps=0` for that window and be parsed
/// without complaint, reintroducing across the process boundary exactly the truncation the f64 on
/// `UgenStats::rps` exists to carry.
/// The `p50`/`p99` millisecond floats are redundant with the microsecond fields actually read and are
/// dropped here; `ok` is kept because a caller can still want it.
#[derive(Debug, Clone, PartialEq)]
pub struct UgenStats {
    /// FRACTIONAL BELOW 1/s, so this is f64 and not i64.
    ///
    /// `GenStats::rps` publishes a fraction under 1/s (a rung completing one request in four seconds is
    /// 0.25/s, not 0), and the child prints it with `{}` - so the wire carries "rps=0.25". Parsed as i64
    /// that FAILS, and `parse_ugen_line` classifies the whole window as a HarnessError: the sub-1/s
    /// rungs the fractional rate was introduced for would be DISCARDED rather than published, which is
    /// strictly worse than the false `0` it replaced. The type has to follow the value across the
    /// process boundary, not just inside the child.
    pub rps: f64,
    pub fail: i64,
    pub p50_us: i64,
    pub p99_us: i64,
    pub ok: i64,
    /// Connections the CHILD could not make (ephemeral ports or descriptors exhausted) rather than
    /// requests the gateway refused. Optional on the wire: a stats line from an older generator
    /// simply has no such count, and defaulting to 0 is right - it means "none reported", not "none
    /// happened", and the alternative is refusing to parse a line that is otherwise complete.
    pub rig_refused: i64,
    /// Requests that exhausted the generator's response budget. Optional on the wire for the same
    /// reason as `rig_refused`.
    pub budget_exceeded: i64,
    /// Whether the CHILD could not pose this window at the concurrency it reports - its async runtime
    /// would not build, its request could not be built, or a connection task was lost - the rig
    /// failing, not the gateway. Optional on the wire for the same reason as the two above: an older
    /// generator's line simply has no such field, and `false` means "none reported". Present, it must
    /// be `0` or `1`.
    pub spawn_failed: bool,
}

fn parse_ugen_fields(line: &str) -> Result<UgenStats, String> {
    let fields = parse_kv(line);
    Ok(UgenStats {
        rps: require_f64(&fields, "rps", line)?,
        fail: require_i64(&fields, "fail", line)?,
        p50_us: require_i64(&fields, "p50us", line)?,
        p99_us: require_i64(&fields, "p99us", line)?,
        ok: require_i64(&fields, "ok", line)?,
        rig_refused: optional_i64(&fields, "rigrefused", line)?,
        budget_exceeded: optional_i64(&fields, "budgetexceeded", line)?,
        spawn_failed: match fields.get("spawnfailed").copied() {
            None | Some("0") => false,
            Some("1") => true,
            Some(v) => {
                return Err(format!(
                    "non-flag field 'spawnfailed={v}' in stats line: '{line}'"
                ))
            }
        },
    })
}

/// Parse one throughput stats line. Empty input is `Absent(NotMeasured)`: nothing printed, which is
/// what a timeout kill, a crash, or a hard-down gateway all look like, and none of them is a zero. A
/// present line missing a required field, or carrying a non-numeric one, is `Absent(HarnessError)`
/// with the offending text in the detail: a stats line in an unexpected shape is a contract break,
/// never a value to default.
pub fn parse_ugen_line(raw: &str) -> Measurement<UgenStats> {
    let line = raw.trim();
    if line.is_empty() {
        return Measurement::absent(Absent::NotMeasured);
    }
    match parse_ugen_fields(line) {
        Ok(s) => Measurement::Measured(s),
        Err(detail) => Measurement::absent_because(Absent::HarnessError, detail),
    }
}

/// THE OTHER HALF OF THE SAME WIRE CONTRACT: how the engine tells the loadgen child what headers to
/// send.
///
/// It has to cross a process boundary because the generator runs as its own pinned child (see
/// `run::load_window_at` for why), and until this existed it did not cross at all: the child
/// hardcoded `authorization: Bearer dummy`. The probe authenticated with the manifest's real
/// credential and in the right per-dialect header shape, the LOAD authenticated as the literal string
/// "dummy" with a header name two of the six dialects do not even use, and the two paths were tested
/// separately so nothing noticed. Every gateway whose declared auth is not literally "dummy" passed
/// its probe and then failed every request of every load window, and the absence that reached the
/// artifact blamed the SEARCH ("no probed concurrency passed the gate") for a credential fault.
///
/// AN ENVIRONMENT VARIABLE, NOT AN ARGUMENT. A credential passed on the command line is visible in
/// `ps` to every user on the box for the whole life of the window; a child's environment is readable
/// only by its owner. This is the same reason the minted credential is read from a file rather than
/// exported into a command line.
pub const HEADERS_ENV: &str = "OTB_LOADGEN_HEADERS";

/// Encode a header list for `HEADERS_ENV`.
///
/// JSON rather than a `k: v` per line, because header VALUES are arbitrary bytes chosen by the
/// gateway's manifest - a bearer token with a newline or a colon in it would silently split into two
/// headers under a hand-rolled format, and sending half a routing header selects the wrong upstream
/// and publishes a number for a pairing that was never driven.
pub fn encode_headers(headers: &[(String, String)]) -> String {
    serde_json::to_string(headers).unwrap_or_else(|_| "[]".to_string())
}

/// Decode what `encode_headers` wrote.
///
/// An unset (or empty) variable yields NO headers, never a default credential. A wrong credential is worse than
/// none: no credential is a 401 the operator can read off the stats line as a total failure, whereas
/// the placeholder that used to be hardcoded here was a wrong credential that looked exactly like a
/// gateway falling over under load.
///
/// A SET BUT UNPARSEABLE VALUE IS AN ERROR, not "no headers". The parent always encodes a valid list,
/// so this means the hand-off broke; treated as empty, a gateway that routes by header (or needs no
/// auth) would serve the window from a default route and publish a number for a pairing that was
/// never driven with its declared headers.
pub fn decode_headers(raw: Option<&str>) -> Result<Vec<(String, String)>, String> {
    match raw.filter(|s| !s.trim().is_empty()) {
        None => Ok(Vec::new()),
        Some(s) => serde_json::from_str::<Vec<(String, String)>>(s)
            .map_err(|e| format!("{HEADERS_ENV} is set but is not a header list ({e})")),
    }
}

#[cfg(test)]
#[path = "tests/loadgen_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/loadgen_sub_one_rate_roundtrip_tests.rs"]
mod sub_one_rate_roundtrip_tests;
