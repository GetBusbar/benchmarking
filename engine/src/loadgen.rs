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
    line.split_whitespace().filter_map(|tok| tok.split_once('=')).collect()
}

fn require_i64(fields: &BTreeMap<&str, &str>, key: &str, line: &str) -> Result<i64, String> {
    match fields.get(key) {
        None => Err(format!("missing field '{key}' in stats line: '{line}'")),
        Some(v) => v.parse::<i64>().map_err(|_| format!("non-numeric field '{key}={v}' in stats line: '{line}'")),
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
}

fn parse_ugen_fields(line: &str) -> Result<UgenStats, String> {
    let fields = parse_kv(line);
    Ok(UgenStats {
        rps: require_i64(&fields, "rps", line)?,
        fail: require_i64(&fields, "fail", line)?,
        p50_us: require_i64(&fields, "p50us", line)?,
        p99_us: require_i64(&fields, "p99us", line)?,
        ok: require_i64(&fields, "ok", line)?,
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
            Measurement::Measured(UgenStats { rps: 1234, fail: 3, p50_us: 12_500, p99_us: 45_000, ok: 14_808 })
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
        assert!(m.detail().unwrap_or_default().contains(line), "detail must carry the offending line");
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
}
