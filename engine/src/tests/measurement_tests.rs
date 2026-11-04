use super::*;

// The whole point, stated as a test: an absence has no number in it, by any route the API offers.
#[test]
fn absent_yields_no_number_on_any_accessor() {
    let m: Measurement<f64> = Measurement::absent(Absent::RigLimited);
    assert_eq!(m.value(), None);
    assert_eq!(m.copied(), None);
    assert_eq!(m.clone().into_value(), None);
    assert!(!m.is_measured());
}

// A measured zero is a REAL result (served, but nothing held the gate) and must stay distinct
// from an absence, or an unprobed rung would be indistinguishable from one that genuinely served
// "0 rps with 0 failures".
#[test]
fn measured_zero_is_not_absence() {
    let zero: Measurement<f64> = Measurement::Measured(0.0);
    let absent: Measurement<f64> = Measurement::absent(Absent::NotMeasured);
    assert!(zero.is_measured());
    assert!(!absent.is_measured());
    assert_eq!(zero.copied(), Some(0.0));
    assert_eq!(absent.copied(), None);
    assert_ne!(zero, absent);
    assert_eq!(serde_json::to_string(&zero).ok(), Some("0.0".to_string()));
    assert_eq!(
        serde_json::to_string(&absent).ok(),
        Some("null".to_string())
    );
}

#[test]
fn absent_serialises_as_null_never_zero() {
    for reason in ALL {
        let m: Measurement<f64> = Measurement::absent(reason);
        let js = serde_json::to_string(&m).unwrap_or_default();
        assert_eq!(js, "null", "an absence must serialise as null");
        assert!(!js.contains('0'), "an absence must not carry a zero");
    }
}

// Reading a snapshot back must not invent a value. null in, absent out.
#[test]
fn null_deserialises_to_absent_not_zero() {
    let m: Measurement<f64> = serde_json::from_str("null").unwrap_or(Measurement::Measured(1.0));
    assert!(!m.is_measured());
    assert_eq!(m.copied(), None);
    let v: Measurement<f64> =
        serde_json::from_str("12.5").unwrap_or(Measurement::absent(Absent::HarnessError));
    assert_eq!(v.copied(), Some(12.5));
}

// Suppression is how a rig-limited or range-exhausted number stops being published. It must
// DISCARD the value: keeping it recoverable is how a raw ungated number escapes to a renderer.
//
// Driven through `absent_because`, which is what the real suppression paths call
// (`suite::apply_peak_verdict` and its siblings assign one of these to the field). This test used
// to drive `suppress_because` - a helper with no production caller - so it asserted this invariant
// about a function no run ever executed while reading as coverage of the path that matters.
#[test]
fn a_suppressed_number_discards_the_value_and_records_why() {
    let m: Measurement<f64> = Measurement::absent_because(
        Absent::SearchExhausted,
        "fps still rising at the top of the search range",
    );
    assert_eq!(m.copied(), None, "the value must not remain recoverable");
    assert_eq!(m.reason(), Some(&Absent::SearchExhausted));
    assert!(m.detail().unwrap_or_default().contains("still rising"));
    assert_eq!(
        serde_json::to_string(&m).ok(),
        Some("null".to_string()),
        "a suppressed number must reach the wire as null, never as its raw value"
    );
}

#[test]
fn map_preserves_the_reason_and_detail() {
    let m: Measurement<f64> =
        Measurement::absent_because(Absent::Untestable, "no SSE for this dialect");
    let mapped = m.map(|v| v as i64);
    assert_eq!(mapped.reason(), Some(&Absent::Untestable));
    assert_eq!(mapped.detail(), Some("no SSE for this dialect"));
}

// A reason about the RIG or about US must never read as a statement about the gateway.
#[test]
fn only_not_served_is_a_statement_about_the_gateway() {
    assert!(Absent::NotServed.is_about_gateway());
    for r in [
        Absent::NotMeasured,
        Absent::BelowResolution,
        Absent::RigLimited,
        Absent::SearchExhausted,
        Absent::Untestable,
        Absent::HarnessError,
    ] {
        assert!(!r.is_about_gateway(), "{r} is not a gateway property");
    }
}

/// Every reason, so a variant added later cannot quietly escape the invariants below. The list alone
/// cannot enforce that - a new variant compiles without being added to it - so `listed_in_all`
/// matches exhaustively: adding a variant fails to compile until it is placed in that match, next
/// to this list, where the missing entry is in plain view.
const ALL: [Absent; 7] = [
    Absent::NotServed,
    Absent::NotMeasured,
    Absent::BelowResolution,
    Absent::RigLimited,
    Absent::SearchExhausted,
    Absent::Untestable,
    Absent::HarnessError,
];

/// Exhaustive by construction: see `ALL`.
fn listed_in_all(a: &Absent) -> usize {
    match a {
        Absent::NotServed => 0,
        Absent::NotMeasured => 1,
        Absent::BelowResolution => 2,
        Absent::RigLimited => 3,
        Absent::SearchExhausted => 4,
        Absent::Untestable => 5,
        Absent::HarnessError => 6,
    }
}

#[test]
fn all_lists_every_variant_exactly_once_in_order() {
    for (i, a) in ALL.iter().enumerate() {
        assert_eq!(listed_in_all(a), i, "{a} is out of place in ALL");
    }
}

// A MEASURED NaN IS A NULL WITH NO REASON unless it is recorded: serde_json writes a non-finite float
// as null, indistinguishable on the wire from an absence, and the artifact's rule is that every null
// carries a reason.
#[test]
fn a_measured_non_finite_float_is_recorded_as_a_harness_error() {
    let mut out = std::collections::BTreeMap::new();
    Measurement::Measured(f64::NAN).record_absence("nan", &mut out);
    Measurement::Measured(f64::INFINITY).record_absence("inf", &mut out);
    Measurement::Measured(1.5f64).record_absence("finite", &mut out);
    Measurement::Measured(7i64).record_absence("integer", &mut out);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(out["nan"].reason, Absent::HarnessError);
    assert_eq!(out["inf"].reason, Absent::HarnessError);
}

// THE TOKEN AND THE WIRE FORM ARE THE SAME STRING, OR THE BOARD LIES ABOUT ITSELF. `token()` is
// hand-written and the serialised form comes from the derive, so nothing but a test holds the
// two together: a variant renamed on one side and not the other would publish a reason under one
// name in the snapshot and render it under another, and a reader has no way to tell that the
// "rig_limited" they are looking at and the "rig_limited" in the file are the same claim.
#[test]
fn every_reason_serialises_as_exactly_its_published_token_and_reads_back_unchanged() {
    for reason in ALL {
        let json = serde_json::to_string(&reason).unwrap_or_default();
        assert_eq!(
            json,
            format!("\"{}\"", reason.token()),
            "the wire form and the published token must be the same string"
        );
        let back: Absent = serde_json::from_str(&json).unwrap_or(Absent::HarnessError);
        assert_eq!(
            back, reason,
            "a reason must survive a snapshot round trip unchanged"
        );
    }
}

// A snapshot is a struct of measurements, not a bare number, so the field-level behaviour is
// what actually ships: an absence must appear as an explicit `null` KEY rather than a missing
// one. A dropped key is indistinguishable from a metric the harness does not know about, and a
// consumer that fills missing keys with a default is exactly how a zero gets invented for a
// measurement that was never taken.
#[test]
fn an_absent_field_is_an_explicit_null_key_never_an_omitted_one() {
    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Row {
        rps: Measurement<f64>,
        ceiling: Measurement<u32>,
    }

    let row = Row {
        rps: Measurement::Measured(1234.5),
        ceiling: Measurement::absent_because(Absent::SearchExhausted, "still passing at the top"),
    };
    let json = serde_json::to_string(&row).unwrap_or_default();
    assert_eq!(json, r#"{"rps":1234.5,"ceiling":null}"#);

    // Reading it back keeps the number and refuses to invent one for the null. The REASON does
    // not survive: it travels beside the value in the sealed envelope rather than inside it, so
    // a consumer reading only the value can never read a reason as data.
    let back: Row = match serde_json::from_str(&json) {
        Ok(r) => r,
        Err(e) => panic!("a row we just wrote must read back: {e}"),
    };
    assert_eq!(back.rps.copied(), Some(1234.5));
    assert_eq!(
        back.ceiling.copied(),
        None,
        "a null must never read back as a number"
    );
    assert_eq!(
        back.ceiling.reason(),
        Some(&Absent::NotMeasured),
        "a bare null carries no reason of its own, so it reads back as the weakest one"
    );
}

// The default is what `..Default::default()` hands out for free to every metric a struct does
// not explicitly set. A derive-supplied `Measured(0)` would be the cardinal defect of this whole
// engine, granted silently to every field anyone forgets.
#[test]
fn the_default_measurement_is_absent_and_unmeasured_never_a_zero() {
    let m: Measurement<f64> = Measurement::default();
    assert!(!m.is_measured());
    assert_eq!(m.copied(), None);
    assert_eq!(m.reason(), Some(&Absent::NotMeasured));
    assert_eq!(serde_json::to_string(&m).ok(), Some("null".to_string()));
}

// `map` on a measured value must carry the value through the conversion. The absent case is
// already pinned above; this is the other half, and without it a `map` that dropped the value
// (returning the default absence) would turn every converted metric into a null and look like a
// conservative, correct refusal rather than the data loss it is.
#[test]
fn map_carries_a_measured_value_through_the_conversion() {
    let m: Measurement<u32> = Measurement::Measured(1300);
    let mapped = m.map(|v| v as f64 * 2.0);
    assert_eq!(mapped.copied(), Some(2600.0));
    assert!(mapped.is_measured());
}

// An absence WITHOUT a detail still records why, and still carries no number. The value-less form
// exists for the cases where the reason alone is the whole story, and it must not become a quiet
// no-op that leaves a rig-limited number publishable. Driven through `absent`, which is what the
// real paths call - this used to drive the `suppress` helper, which had no production caller, so
// the invariant was asserted about a function no run executed.
#[test]
fn an_absence_without_a_detail_still_carries_no_number() {
    for reason in ALL {
        let m: Measurement<f64> = Measurement::absent(reason.clone());
        assert_eq!(m.copied(), None, "{reason} must never expose a number");
        assert_eq!(m.reason(), Some(&reason));
        assert_eq!(m.detail(), None);
        assert_eq!(serde_json::to_string(&m).ok(), Some("null".to_string()));
    }
}

#[test]
fn tokens_are_stable_and_distinct() {
    let mut seen = std::collections::BTreeSet::new();
    for r in &ALL {
        assert!(seen.insert(r.token()), "duplicate token {}", r.token());
        assert!(!r.token().is_empty());
    }
    assert_eq!(seen.len(), ALL.len());
}
