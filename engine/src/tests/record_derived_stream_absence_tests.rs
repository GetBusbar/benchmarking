use super::*;

fn served_stream() -> CellStream {
    CellStream {
        stream_served: StreamServed::Bool(true),
        streams_sustained: Measurement::Measured(0),
        streams_sustained_mock_ceiling: None,
        streams_sustained_headroom: None,
        ..Default::default()
    }
}

/* A DERIVED FIELD IS STILL A PUBLISHED NULL. The old guard only ran when the PARENT was absent,
so the two cases `suite` actually produces - a bisect landing on conc == 0, and a mock frame
ceiling that is not positive - published `streams_sustained_mock_ceiling: null` and
`_headroom: null` on a served cell with nothing in the absences map to explain them. */
#[test]
fn a_measured_parent_does_not_excuse_its_derived_fields_from_carrying_a_reason() {
    let a = served_stream().absences();
    for k in [
        "streams_sustained_mock_ceiling",
        "streams_sustained_headroom",
    ] {
        let e = a
            .get(k)
            .unwrap_or_else(|| panic!("{k} published a bare null on a served cell"));
        assert!(
            e.detail.as_deref().unwrap_or("").contains("frame ceiling"),
            "{k}'s reason must say what could not be derived: {:?}",
            e.detail
        );
    }
}

/// When the parent IS absent they still inherit its reason, which is the behaviour that already
/// worked and must not regress.
#[test]
fn an_absent_parent_still_lends_its_reason_to_the_fields_derived_from_it() {
    let s = CellStream {
        streams_sustained: Measurement::absent_because(
            Absent::SearchExhausted,
            "still passing at the top of the range".to_string(),
        ),
        ..served_stream()
    };
    let a = s.absences();
    for k in [
        "streams_sustained_mock_ceiling",
        "streams_sustained_headroom",
    ] {
        let e = a.get(k).expect("must carry a reason");
        assert_eq!(
            e.reason,
            Absent::SearchExhausted,
            "{k} must inherit the parent's reason"
        );
    }
}

/// And a fully derived pair publishes no absence at all.
#[test]
fn measured_derived_fields_need_no_absence_entry() {
    let s = CellStream {
        streams_sustained: Measurement::Measured(128),
        streams_sustained_mock_ceiling: Some(4000.0),
        streams_sustained_headroom: Some(0.9),
        ..served_stream()
    };
    let a = s.absences();
    assert!(!a.contains_key("streams_sustained_mock_ceiling"));
    assert!(!a.contains_key("streams_sustained_headroom"));
}
