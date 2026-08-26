// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The wire contract between the loadgen child process and the engine: `otb loadgen`'s stdout
// (`gen.rs`) is a `k=v` line, parsed here via `parse_ugen_line`. Empty output is
// `Absent(NotMeasured)`, never a zero - that's what a timeout kill, crash, or hard-down gateway all
// look like, and none of them measured anything.

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

fn require_i64(fields: &BTreeMap<&str, &str>, key: &str, line: &str) -> Result<i64, String> {
    match fields.get(key) {
        None => Err(format!("missing field '{key}' in stats line: '{line}'")),
        Some(v) => v
            .parse::<i64>()
            .map_err(|_| format!("non-numeric field '{key}={v}' in stats line: '{line}'")),
    }
}

/// The throughput lane's stats, parsed from:
/// `rps=<f64> fail=%d p50=%.2f p99=%.2f p50us=%d p99us=%d ok=%d`,
/// optionally followed by `rigrefused=%d budgetexceeded=%d spawnfailed=%d`.
///
/// The millisecond `p50`/`p99` floats are redundant with the microsecond fields actually read and
/// are dropped here.
#[derive(Debug, Clone, PartialEq)]
pub struct UgenStats {
    /// Fractional below 1/s (one request in four seconds is 0.25/s, printed with `{}`). Must stay
    /// f64: parsing it as i64 fails and `parse_ugen_line` discards the whole window as a
    /// HarnessError, silently dropping the sub-1/s rungs this exists to carry.
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
    /// Whether the CHILD could not spawn the threads for this window - the OS refusing, not the gateway.
    /// Optional on the wire for the same reason as the two above: an older generator's line simply has
    /// no such field, and `false` means "none reported".
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
        rig_refused: fields
            .get("rigrefused")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0),
        budget_exceeded: fields
            .get("budgetexceeded")
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(0),
        spawn_failed: fields
            .get("spawnfailed")
            .map(|v| v.trim() != "0" && !v.trim().is_empty())
            .unwrap_or(false),
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

/// How the engine tells the loadgen child (a separate pinned process, see `run::load_window_at`)
/// what headers to send. Without this the child hardcoded a dummy credential in the wrong shape,
/// so every real-auth gateway passed its probe and then failed every load-window request silently.
///
/// An environment variable, not an argument: a command-line credential is visible in `ps` to every
/// user on the box, while a child's environment is readable only by its owner.
pub const HEADERS_ENV: &str = "OTB_LOADGEN_HEADERS";

/// Encode a header list for `HEADERS_ENV`.
///
/// JSON rather than a `k: v` per line: header values are arbitrary bytes from the gateway's
/// manifest, and a token containing a newline or colon would silently split under a hand-rolled
/// format.
pub fn encode_headers(headers: &[(String, String)]) -> String {
    serde_json::to_string(headers).unwrap_or_else(|_| "[]".to_string())
}

/// Decode what `encode_headers` wrote.
///
/// An unset or unparseable variable yields NO headers, never a default credential: a wrong
/// credential produces a 401 that looks like a gateway falling over under load, whereas no
/// credential reads correctly as a total auth failure.
pub fn decode_headers(raw: Option<&str>) -> Vec<(String, String)> {
    raw.and_then(|s| serde_json::from_str::<Vec<(String, String)>>(s).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    // The child sets `spawn_failed` when the OS refuses a thread (a rig limit `SweepProbe::probe`
    // stops the search on). It must survive onto the stats line, or a window that never ran at its
    // claimed concurrency reads as an ordinary gateway result.
    #[test]
    fn spawn_failed_survives_the_subprocess_boundary() {
        let base = "rps=1000 fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=1000";
        let got = parse_ugen_line(&format!("{base} spawnfailed=1"))
            .into_value()
            .expect("a complete line parses");
        assert!(
            got.spawn_failed,
            "the child said it could not spawn; the parent must hear it"
        );

        let got = parse_ugen_line(&format!("{base} spawnfailed=0"))
            .into_value()
            .expect("a complete line parses");
        assert!(
            !got.spawn_failed,
            "and a healthy window must not read as a spawn failure"
        );

        // An older generator's line has no such field; absent means "none reported", not "none
        // happened".
        let got = parse_ugen_line(base)
            .into_value()
            .expect("a line without the field still parses");
        assert!(!got.spawn_failed);
    }

    use super::*;

    // The exact format `otb loadgen` emits on its own stdout.
    const UGEN_LINE: &str = "rps=1234 fail=3 p50=12.50 p99=45.00 p50us=12500 p99us=45000 ok=14808";

    #[test]
    fn a_real_ugen_line_parses_every_field() {
        let m = parse_ugen_line(UGEN_LINE);
        assert_eq!(
            m,
            Measurement::Measured(UgenStats {
                rps: 1234.0,
                fail: 3,
                p50_us: 12_500,
                p99_us: 45_000,
                ok: 14_808,
                rig_refused: 0,
                budget_exceeded: 0,
                spawn_failed: false
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
        assert_eq!(m.value().map(|s| s.rps), Some(0.0));
    }

    #[test]
    fn unknown_extra_fields_are_ignored_not_a_parse_failure() {
        let line = format!("{UGEN_LINE} pad=64 futurefield=x");
        let m = parse_ugen_line(&line);
        assert!(m.is_measured());
    }

    // ── the header wire ─────────────────────────────────────────────────────────────────────────

    // Every dialect's header SHAPE must survive the crossing, not just a token: two of six don't
    // use `authorization` at all, and anthropic sends a second, mandatory header beside its key.
    #[test]
    fn every_dialects_own_header_shape_survives_the_process_boundary() {
        for d in crate::ingress::Dialect::ALL {
            let sent = d.auth_headers("a-real-token");
            let back = decode_headers(Some(&encode_headers(&sent)));
            assert_eq!(back, sent, "{d}'s header shape must cross unchanged");
        }
    }

    // A value carrying characters a hand-rolled `k: v` encoding would split on; a routing header
    // that arrives halved selects the wrong upstream.
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

    // No headers, never a default credential - a hardcoded placeholder would make a credential
    // fault indistinguishable from a gateway collapsing under load.
    #[test]
    fn an_unset_or_broken_header_variable_sends_nothing_rather_than_a_placeholder() {
        assert!(decode_headers(None).is_empty());
        assert!(decode_headers(Some("")).is_empty());
        assert!(decode_headers(Some("not json")).is_empty());
        assert!(decode_headers(Some("{\"authorization\":\"Bearer dummy\"}")).is_empty());
    }
}

#[cfg(test)]
mod sub_one_rate_roundtrip_tests {
    use super::*;

    // Pins the subprocess boundary for the fractional rate: `GenStats::rps` prints sub-1/s values
    // like `rps=0.25`, and a parser reading `rps` as i64 would fail, making `parse_ugen_line`
    // discard the whole window as a HarnessError instead of publishing it.
    #[test]
    fn a_sub_one_per_second_rate_survives_the_stats_line_round_trip() {
        for (line, want) in [
            (
                "rps=0.25 fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=1",
                0.25_f64,
            ),
            (
                "rps=0.5 fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=2",
                0.5,
            ),
            (
                "rps=19 fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=76",
                19.0,
            ),
            (
                "rps=44363 fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=177452",
                44_363.0,
            ),
        ] {
            let got = parse_ugen_line(line)
                .into_value()
                .unwrap_or_else(|| panic!("a well-formed stats line must parse: {line}"));
            assert!(
                (got.rps - want).abs() < 1e-9,
                "rps round-tripped as {} from {line}, wanted {want}",
                got.rps
            );
        }
    }

    // The rate is the ONLY field allowed to be fractional. A fractional COUNT is a corrupt line and
    // must still be refused - relaxing every field to f64 would have hidden that.
    #[test]
    fn a_fractional_count_is_still_refused() {
        assert!(
            parse_ugen_line("rps=10 fail=0.5 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=40")
                .into_value()
                .is_none(),
            "a fractional failure COUNT is a corrupt line, not a measurement"
        );
    }

    // A non-finite rate must not reach a cast that would SATURATE it into a plausible-looking integer.
    #[test]
    fn a_non_finite_rate_is_refused_rather_than_saturated() {
        for bad in ["rps=inf", "rps=NaN"] {
            let line = format!("{bad} fail=0 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=1");
            assert!(
                parse_ugen_line(&line).into_value().is_none(),
                "a non-finite rate must be refused: {line}"
            );
        }
    }
}
