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

fn require_i64(fields: &BTreeMap<&str, &str>, key: &str, line: &str) -> Result<i64, String> {
    match fields.get(key) {
        None => Err(format!("missing field '{key}' in stats line: '{line}'")),
        Some(v) => v
            .parse::<i64>()
            .map_err(|_| format!("non-numeric field '{key}={v}' in stats line: '{line}'")),
    }
}

/// The throughput lane's stats, from the stats line:
/// `rps=%d fail=%d p50=%.2f p99=%.2f p50us=%d p99us=%d ok=%d`.
/// The `p50`/`p99` millisecond floats are redundant with the microsecond fields actually read and are
/// dropped here; `ok` is kept because a caller can still want it.
#[derive(Debug, Clone, PartialEq)]
pub struct UgenStats {
    pub rps: i64,
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
}

fn parse_ugen_fields(line: &str) -> Result<UgenStats, String> {
    let fields = parse_kv(line);
    Ok(UgenStats {
        rps: require_i64(&fields, "rps", line)?,
        fail: require_i64(&fields, "fail", line)?,
        p50_us: require_i64(&fields, "p50us", line)?,
        p99_us: require_i64(&fields, "p99us", line)?,
        ok: require_i64(&fields, "ok", line)?,
        rig_refused: fields
            .get("rigrefused")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0),
        budget_exceeded: fields
            .get("budgetexceeded")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0),
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
/// An unset or unparseable variable yields NO headers, never a default credential. A wrong credential
/// is worse than none: no credential is a 401 the operator can read off the stats line as a total
/// failure, whereas the placeholder that used to be hardcoded here was a wrong credential that looked
/// exactly like a gateway falling over under load.
pub fn decode_headers(raw: Option<&str>) -> Vec<(String, String)> {
    raw.and_then(|s| serde_json::from_str::<Vec<(String, String)>>(s).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact format `otb loadgen` emits on its own stdout.
    const UGEN_LINE: &str = "rps=1234 fail=3 p50=12.50 p99=45.00 p50us=12500 p99us=45000 ok=14808";

    #[test]
    fn a_real_ugen_line_parses_every_field() {
        let m = parse_ugen_line(UGEN_LINE);
        assert_eq!(
            m,
            Measurement::Measured(UgenStats {
                rps: 1234,
                fail: 3,
                p50_us: 12_500,
                p99_us: 45_000,
                ok: 14_808,
                rig_refused: 0,
                budget_exceeded: 0
            })
        );
    }

    #[test]
    fn empty_ugen_output_is_not_measured_never_a_zero() {
        for empty in ["", "   ", "\n"] {
            let m = parse_ugen_line(empty);
            assert!(!m.is_measured());
            assert_eq!(m.reason(), Some(&Absent::NotMeasured));
        }
    }

    #[test]
    fn a_missing_field_is_absent_with_the_text_in_the_detail_never_a_default() {
        let line = "rps=1234 fail=3 p99us=45000 ok=14808"; // p50us dropped
        let m = parse_ugen_line(line);
        assert!(!m.is_measured());
        assert_eq!(m.reason(), Some(&Absent::HarnessError));
        assert!(
            m.detail().unwrap_or_default().contains(line),
            "detail must carry the offending line"
        );
        assert!(m.detail().unwrap_or_default().contains("p50us"));
    }

    #[test]
    fn a_non_numeric_field_is_absent_never_silently_parsed_as_zero() {
        let line = "rps=1234 fail=abc p50us=12500 p99us=45000 ok=14808";
        let m = parse_ugen_line(line);
        assert!(!m.is_measured());
        assert_eq!(m.reason(), Some(&Absent::HarnessError));
        assert!(m.detail().unwrap_or_default().contains("fail=abc"));
    }

    #[test]
    fn a_genuine_rps_zero_line_is_a_measured_zero_distinct_from_absence() {
        let line = "rps=0 fail=0 p50=0.00 p99=0.00 p50us=0 p99us=0 ok=0";
        let m = parse_ugen_line(line);
        assert!(m.is_measured());
        assert_eq!(m.value().map(|s| s.rps), Some(0));
    }

    #[test]
    fn unknown_extra_fields_are_ignored_not_a_parse_failure() {
        let line = format!("{UGEN_LINE} pad=64 futurefield=x");
        let m = parse_ugen_line(&line);
        assert!(m.is_measured());
    }

    // ── the header wire, the half that used to not exist ────────────────────────────────────────

    // Every dialect's real header SHAPE has to survive the crossing, not just a token: two of the six
    // do not use `authorization` at all, and anthropic sends a second, mandatory header beside its
    // key. The hardcoded line this replaces got the name wrong as well as the value.
    #[test]
    fn every_dialects_own_header_shape_survives_the_process_boundary() {
        for d in crate::ingress::Dialect::ALL {
            let sent = d.auth_headers("a-real-token");
            let back = decode_headers(Some(&encode_headers(&sent)));
            assert_eq!(back, sent, "{d}'s header shape must cross unchanged");
        }
    }

    // A value carrying the characters a hand-rolled `k: v` encoding would split on. A routing header
    // that arrives halved selects the wrong upstream, which publishes a number for a pairing that was
    // never driven.
    #[test]
    fn a_hostile_header_value_crosses_intact_rather_than_splitting() {
        let hostile = vec![
            (
                "authorization".to_string(),
                "Bearer a:b\nx-injected: yes\r\n".to_string(),
            ),
            (
                "x-route".to_string(),
                "  spaces and \"quotes\"  ".to_string(),
            ),
        ];
        assert_eq!(decode_headers(Some(&encode_headers(&hostile))), hostile);
    }

    // Duplicates survive as duplicates: `http::Headers`' own doc says a map would collapse repeated
    // header names that some dialects distinguish from a single comma-joined one.
    #[test]
    fn repeated_header_names_are_not_collapsed_in_transit() {
        let dupes = vec![
            ("x-h".to_string(), "one".to_string()),
            ("x-h".to_string(), "two".to_string()),
        ];
        assert_eq!(decode_headers(Some(&encode_headers(&dupes))), dupes);
    }

    // NO HEADERS, never a default credential. The placeholder that used to be hardcoded here is
    // precisely what made a credential fault indistinguishable from a gateway collapsing under load.
    #[test]
    fn an_unset_or_broken_header_variable_sends_nothing_rather_than_a_placeholder() {
        assert!(decode_headers(None).is_empty());
        assert!(decode_headers(Some("")).is_empty());
        assert!(decode_headers(Some("not json")).is_empty());
        assert!(decode_headers(Some("{\"authorization\":\"Bearer dummy\"}")).is_empty());
    }
}
