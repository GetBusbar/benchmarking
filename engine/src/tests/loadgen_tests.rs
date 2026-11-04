// THE FLAG THE PARENT CHECKED BUT COULD NEVER RECEIVE.
//
// The child sets `spawn_failed` when the OS refuses a thread, which means the window never ran at
// the concurrency it claims - a rig limit `SweepProbe::probe` stops the search on. But the flag
// stopped at the subprocess boundary: the stats line did not carry it and the parent hardcoded
// `spawn_failed: false`, so `if stats.spawn_failed` could not fire on the only path that matters
// and a window that never ran was read as an ordinary result of the gateway.
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

    // An OLDER generator's line has no such field. Absent means "none reported", and refusing to
    // parse an otherwise-complete line would be worse than defaulting - the same reasoning
    // `rig_refused` and `budget_exceeded` already carry.
    let got = parse_ugen_line(base)
        .into_value()
        .expect("a line without the field still parses");
    assert!(!got.spawn_failed);
}

// THE RIG'S OWN COUNTS ARE READ, NOT DEFAULTED. A non-zero `rigrefused`/`budgetexceeded` must reach
// the parent as itself - it is what discards a rig-limited window - and a PRESENT value that is not
// a count is a corrupt line, refused like any required field rather than read as 0.
#[test]
fn rig_counts_are_read_and_a_malformed_one_is_refused() {
    let base = "rps=1000 fail=4 p50=1.0 p99=2.0 p50us=1000 p99us=2000 ok=1000";
    let got = parse_ugen_line(&format!("{base} rigrefused=3 budgetexceeded=1"))
        .into_value()
        .expect("a complete line parses");
    assert_eq!((got.rig_refused, got.budget_exceeded), (3, 1));

    for bad in [
        "rigrefused=1x",
        "rigrefused=1.5",
        "budgetexceeded=-",
        "spawnfailed=yes",
        "spawnfailed=",
    ] {
        let m = parse_ugen_line(&format!("{base} {bad}"));
        assert_eq!(
            m.reason(),
            Some(&Absent::HarnessError),
            "{bad} must be refused, not defaulted"
        );
    }
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

// ── the header wire, the half that used to not exist ────────────────────────────────────────

// Every dialect's real header SHAPE has to survive the crossing, not just a token: two of the six
// do not use `authorization` at all, and anthropic sends a second, mandatory header beside its
// key. The hardcoded line this replaces got the name wrong as well as the value.
#[test]
fn every_dialects_own_header_shape_survives_the_process_boundary() {
    for d in crate::ingress::Dialect::ALL {
        let sent = d.auth_headers("a-real-token");
        let back = decode_headers(Some(&encode_headers(&sent))).expect("a list we encoded decodes");
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
    assert_eq!(decode_headers(Some(&encode_headers(&hostile))), Ok(hostile));
}

// Duplicates survive as duplicates: `http::Headers`' own doc says a map would collapse repeated
// header names that some dialects distinguish from a single comma-joined one.
#[test]
fn repeated_header_names_are_not_collapsed_in_transit() {
    let dupes = vec![
        ("x-h".to_string(), "one".to_string()),
        ("x-h".to_string(), "two".to_string()),
    ];
    assert_eq!(decode_headers(Some(&encode_headers(&dupes))), Ok(dupes));
}

// NO HEADERS, never a default credential. The placeholder that used to be hardcoded here is
// precisely what made a credential fault indistinguishable from a gateway collapsing under load.
#[test]
fn an_unset_header_variable_sends_nothing_rather_than_a_placeholder() {
    assert_eq!(decode_headers(None), Ok(Vec::new()));
    assert_eq!(decode_headers(Some("")), Ok(Vec::new()));
}

// A BROKEN HAND-OFF IS AN ERROR, never "no headers": the parent always encodes a valid list, and a
// window driven without the declared headers can still be served by a default route and published
// for a pairing that was never asked for.
#[test]
fn a_set_but_unparseable_header_variable_is_refused() {
    for bad in [
        "not json",
        "{\"authorization\":\"Bearer dummy\"}",
        "[[\"only-a-name\"]]",
    ] {
        let err = decode_headers(Some(bad)).expect_err("garbage must not decode to no headers");
        assert!(
            err.contains(HEADERS_ENV),
            "the refusal names the variable: {err}"
        );
    }
}
