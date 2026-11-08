/// EVERY SCALAR MEASUREMENT ON `CellPerf` MUST BE REACHABLE BY `absences()`.
///
/// This is a STRUCTURAL guard, not a list to maintain: it serialises a perf record whose scalars
/// are all absent, then asserts that every key which serialised to `null` has a reason in the
/// sibling map. A field added to the struct but forgotten in `absences_of!` publishes a bare
/// `null` with its reason nowhere - the one state the whole `Measurement` design exists to
/// prevent.
///
/// It has now happened twice. The frontier hit it (a Vec, unreachable by the macro) and the
/// comment written then did not stop me doing it again with the nine cost fields, which shipped
/// to a real box and published unexplained nulls. A comment warning about a trap is not a guard
/// against it.
#[test]
fn every_absent_perf_scalar_carries_a_reason() {
    use crate::measurement::{Absent, Measurement};
    // All-absent, each with a reason, so anything that loses one is visible.
    let mut p = CellPerf::default();
    macro_rules! blank {
            ($($f:ident),* $(,)?) => {$(
                p.$f = Measurement::absent_because(Absent::NotMeasured, concat!("test: ", stringify!($f)));
            )*};
        }
    blank!(
        added_latency_p50_us,
        added_latency_p99_us,
        gateway_c1_p99_us,
        direct_c1_p99_us,
        cpu_us_per_request,
        rps_per_cpu_second,
        cost_window_conc,
        cost_window_ok,
        cost_window_rps,
        cost_core_utilisation,
        cost_threads,
        cost_nonvol_ctxt_per_request,
        cost_majflt,
    );
    // NOT MEASUREMENTS, so they are given values rather than exempted by name: `c1_note` and
    // `reverify_note` are prose and `egress_reverified` is a verdict flag. None of the three is a
    // quantity that can be "absent for a reason", and a null there means "nothing to say", not a
    // lost number. Filling them keeps this test about MEASUREMENTS without an exclusion list that
    // a future non-measurement field would silently need adding to.
    p.c1_note = Some("test".into());
    p.reverify_note = Some("test".into());
    p.egress_reverified = Some(true);

    let reasons = p.absences();
    let json = serde_json::to_value(&p).expect("serialises");
    let obj = json.as_object().expect("an object");
    let mut orphans: Vec<&str> = Vec::new();
    for (k, v) in obj {
        // Only scalars that serialised to null are measurements that went absent; arrays and
        // strings are evidence or prose and are not this map's business.
        if v.is_null() && !reasons.contains_key(k.as_str()) {
            orphans.push(k.as_str());
        }
    }
    assert!(
            orphans.is_empty(),
            "these absent fields published a null with no reason: {orphans:?} - add them to absences_of! in CellPerf::absences"
        );
}

fn sample_perf() -> CellPerf {
    CellPerf {
        sweep_max_proxy_interrupted: false,
        added_latency_p50_us: Measurement::Measured(40_939),
        added_latency_p99_us: Measurement::Measured(40_945),
        gateway_c1_p99_us: Measurement::Measured(41_026),
        direct_c1_p99_us: Measurement::Measured(81),
        // A cost the fixture can round-trip: the point of this test is that every field survives
        // serialisation, so a field left at its default would pass vacuously.
        cpu_us_per_request: Measurement::Measured(37.5),
        rps_per_cpu_second: Measurement::Measured(26_666.0),
        cost_window_conc: Measurement::Measured(8),
        cost_window_ok: Measurement::Measured(2048.0),
        cost_window_rps: Measurement::Measured(341.0),
        cost_core_utilisation: Measurement::Measured(0.97),
        cost_threads: Measurement::Measured(9.0),
        cost_nonvol_ctxt_per_request: Measurement::Measured(0.25),
        cost_majflt: Measurement::Measured(0.0),
        // The throughput answer is the FRONTIER now. The six scalars this fixture used to carry -
        // `rps_max_proxy` / `rps_sustained_20ms` and their concurrency twins - were one sweep
        // summarised twice by a chosen ceiling, and they are gone from the artifact.
        frontier: Vec::new(),
        sweep_max_proxy: vec![SweepPoint {
            conc: 256,
            ok: Measurement::Measured(1_000),
            rps: Measurement::Measured(6_209.0),
            p99_us: Measurement::Measured(43_969),
            fail: Measurement::Measured(0),
        }],
        egress_reverified: Some(true),
        reverify_note: None,
        c1_note: None,
    }
}

// THE FRONTIER SURVIVES THE ARTIFACT, MONOTONICITY AND ALL.
//
// `frontier.rs` proves the ordering holds over rungs; this proves the published SHAPE keeps it.
// A serialization that reordered the readings, or dropped the unbounded one, would leave a board
// whose columns no longer read as a curve while every individual number stayed correct - and the
// ordering is the whole reason a reader can check us by eye.
#[test]
fn a_serialized_frontier_keeps_its_order_and_its_monotonicity() {
    use super::*;
    // apisix anthropic>anthropic, 2026-07-29: the real curve.
    let rows = [
        (Some(1_000i64), 7_015i64),
        (Some(5_000), 15_438),
        (Some(10_000), 18_943),
        (Some(50_000), 19_284),
        (Some(100_000), 19_284),
        (None, 19_284),
    ];
    let frontier: Vec<FrontierReading> = rows
        .iter()
        .map(|(b, rps)| FrontierReading {
            p99_bound_us: *b,
            rps: Measurement::Measured(*rps as f64),
            concurrency: Measurement::Measured(256),
            p99_us: Measurement::Measured(4_000),
            first_disqualified_conc: Measurement::Measured(1024),
            lower_bound: false,
        })
        .collect();
    let text = serde_json::to_string(&frontier).expect("serialize");
    let back: Vec<FrontierReading> = serde_json::from_str(&text).expect("round trip");
    assert_eq!(back.len(), 6);
    // The unbounded reading is LAST and its bound serializes as null, not as a missing key or a
    // sentinel number - "no latency bound" is a distinct state from "some bound we forgot".
    assert_eq!(back[5].p99_bound_us, None);
    assert!(text.contains("\"p99_bound_us\":null"), "{text}");
    let bounds: Vec<Option<i64>> = back.iter().map(|r| r.p99_bound_us).collect();
    assert_eq!(
        bounds,
        vec![
            Some(1_000),
            Some(5_000),
            Some(10_000),
            Some(50_000),
            Some(100_000),
            None
        ],
        "bounds must stay ascending with the unbounded reading last"
    );
    let rates: Vec<f64> = back.iter().map(|r| r.rps.copied().unwrap()).collect();
    for w in rates.windows(2) {
        assert!(
            w[1] >= w[0],
            "the published sequence must not invert: {rates:?}"
        );
    }
}

// A reading that ran out of range publishes its rate AND says the rate is a floor. The retired
// search published null here and threw the measurement away for failing to prove maximality.
#[test]
fn a_lower_bound_reading_publishes_its_rate_and_declares_itself_a_floor() {
    use super::*;
    let r = FrontierReading {
        p99_bound_us: Some(10_000),
        rps: Measurement::Measured(19_000.0),
        concurrency: Measurement::Measured(16_384),
        p99_us: Measurement::Measured(3_000),
        first_disqualified_conc: Measurement::absent(Absent::SearchExhausted),
        lower_bound: true,
    };
    let back: FrontierReading =
        serde_json::from_str(&serde_json::to_string(&r).expect("ser")).expect("de");
    assert_eq!(
        back.rps.copied(),
        Some(19_000.0),
        "the rate is real and is published"
    );
    assert!(
        back.lower_bound,
        "and it is labelled a floor rather than a ceiling"
    );
    // The REASON does not survive the envelope, and that is this artifact's convention rather than
    // a defect: an absent `Measurement` serializes as a bare null and its reason lives in the
    // CELL's sibling `absences` map. The test that matters is that the map carries it - the next
    // one - and that was a real hole until the frontier's keys were added to `CellPerf::absences`.
    // A Vec field is unreachable by `absences_of!`, so before that every absent reading published
    // a null with its reason nowhere at all: the one state `Measurement` exists to prevent.
    assert_eq!(back.first_disqualified_conc.copied(), None);
}

// THE ABSENCE MAP CARRIES EVERY READING'S REASON, KEYED BY ITS BOUND.
#[test]
fn an_absent_frontier_reading_publishes_its_reason_in_the_cells_absence_map() {
    let mut perf = sample_perf();
    perf.frontier = vec![
        FrontierReading {
            p99_bound_us: Some(1_000),
            rps: Measurement::absent_because(
                Absent::BelowResolution,
                "every cleanly-served rung had a tail at or above 1ms",
            ),
            concurrency: Measurement::absent(Absent::BelowResolution),
            p99_us: Measurement::absent(Absent::BelowResolution),
            first_disqualified_conc: Measurement::absent(Absent::BelowResolution),
            lower_bound: false,
        },
        FrontierReading {
            p99_bound_us: None,
            rps: Measurement::Measured(19_284.0),
            concurrency: Measurement::Measured(1024),
            p99_us: Measurement::Measured(40_000),
            first_disqualified_conc: Measurement::Measured(2048),
            lower_bound: false,
        },
    ];
    let abs = perf.absences();
    let e = abs
        .get("frontier.1ms.rps")
        .expect("keyed by its BOUND, so a reader need not count columns to find it");
    assert_eq!(e.reason, Absent::BelowResolution);
    assert!(e.detail.as_deref().unwrap_or_default().contains("1ms"));
    assert!(abs.contains_key("frontier.1ms.concurrency"));
    assert!(abs.contains_key("frontier.1ms.p99_us"));
    // The reading that WAS taken contributes no keys at all.
    assert!(!abs.keys().any(|k| k.starts_with("frontier.unbounded")));
}

use super::*;
use crate::measurement::Absent;

// ── the published names of the memory fields are a CONTRACT, not an implementation detail ─────
//
// These structs are the producer; `site/gen-data.mjs`, `site/seal.mjs` and
// `site/check-consistency.mjs` are the consumer, and they read these keys by literal name out of
// the JSON. Nothing in either test suite crossed that boundary: renaming `peak_rss_hwm_mib` here
// left all 257 Rust tests green, and on the site side the rename does not raise - it reads
// `undefined`, so C7's `peak_rss_mib <= peak_rss_hwm_mib` invariant silently stops checking and
// every real peak-RSS number publishes as a null. An absent measurement is supposed to mean the
// rig could not measure it; here it would mean a field got renamed.
//
// Listed literally rather than derived from the struct, on purpose: a test that asked the struct
// for its own field names would agree with any rename and hold nothing.
#[test]
fn the_memory_field_names_the_site_reads_are_pinned_to_the_wire() {
    // Every field either carries a serde default or is filled here, so this deserialize also
    // proves the defaults still cover the shape.
    let matrix_memory: MatrixMemory = serde_json::from_str(r#"{"served":true}"#).unwrap();
    let v = serde_json::to_value(&matrix_memory).unwrap();
    for key in [
        "idle_rss_mib",
        "peak_rss_mib",
        "peak_rss_hwm_mib",
        "post_load_rss_mib",
        "recovered_rss_mib",
    ] {
        assert!(
            v.get(key).is_some(),
            "matrix memory must publish {key}: site/ reads it by this exact name"
        );
    }

    let cell_memory = CellMemory::default();
    let v = serde_json::to_value(&cell_memory).unwrap();
    for key in [
        "idle_rss_mib",
        "peak_rss_mib",
        "peak_rss_hwm_mib",
        "steady_state_rss_mib",
        "recovered_rss_mib",
        "time_to_plateau_s",
        "growth_rate_mib_per_min",
        // Both changed from plain `Option` to `Measurement` so their absence can carry a reason.
        // The wire name and the wire form (bare value or null) are unchanged, and this is where
        // that is pinned.
        "plateaued",
        "load_s",
    ] {
        assert!(
            v.get(key).is_some(),
            "cell memory must publish {key}: site/ reads it by this exact name"
        );
    }
    assert_eq!(
        v["plateaued"],
        serde_json::Value::Null,
        "an unjudged plateau is a null on the wire, not a false"
    );
    assert_eq!(v["load_s"], serde_json::Value::Null);
    let back: CellMemory = serde_json::from_str(r#"{"served":true,"plateaued":true,"load_s":90}"#)
        .expect("the published form must parse back");
    assert_eq!(back.plateaued.copied(), Some(true));
    assert_eq!(back.load_s.copied(), Some(90));
}

// EVERY NULL ON A SERVED CELL CARRIES A REASON. `plateaued` and `load_s` were plain `Option`s, so
// `absences_of!` - which walks `Measurement` fields - could not see them: a served cell whose
// memory window could not judge the plateau published a bare null with nothing, anywhere in the
// artifact, saying why. That is the exact hole the absences map exists to close, and it was open
// on the two memory fields that are hardest to reason about from the numbers alone.
#[test]
fn a_memory_window_that_could_not_judge_the_plateau_publishes_why_not_a_bare_null() {
    let cell = Cell {
        served: Served::Bool(true),
        memory: Some(CellMemory {
            served: true,
            plateaued: Measurement::absent_because(
                Absent::NotMeasured,
                "too few readings fell inside the settle window to judge whether memory moved",
            ),
            load_s: Measurement::absent(Absent::HarnessError),
            ..Default::default()
        }),
        ..Default::default()
    };
    let v: serde_json::Value = serde_json::to_value(&cell)
        .expect("a cell must serialise")
        .clone();

    assert_eq!(
        v["memory"]["plateaued"],
        serde_json::Value::Null,
        "the value slot stays a bare null for existing consumers"
    );
    assert_eq!(
        v["absences"]["memory.plateaued"]["reason"], "not_measured",
        "the plateau verdict's absence must be published with its reason: got {v}"
    );
    assert!(
        v["absences"]["memory.plateaued"]["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("settle window"),
        "and with the window's own evidence: got {v}"
    );
    assert_eq!(
        v["absences"]["memory.load_s"]["reason"], "harness_error",
        "load_s must be coverable too: got {v}"
    );
}

fn sample_cell() -> Cell {
    Cell {
        served: Served::Bool(true),
        reason: None,
        status: "200".to_string(),
        path: "/openai/v1/chat/completions".to_string(),
        verdict_note: "HTTP 200, openai envelope validated".to_string(),
        body_snippet: "{\"id\":\"x\"}".to_string(),
        probe_note: None,
        perf_dropped: None,
        perf: Some(sample_perf()),
        stream: Some(CellStream {
            stream_served: StreamServed::Status("untestable".to_string()),
            // The TOKEN in `reason` and the prose in `stream_error`: `reason` used to carry the
            // detail string, which made one field name mean two different things depending on
            // which block a reader was in.
            reason: Some("untestable".to_string()),
            stream_error: Some("did not rebind the mock port".to_string()),
            // NotMeasured, not Untestable: a `null` on the wire always deserialises back to
            // NotMeasured (measurement.rs's Deserialize cannot recover a more specific reason
            // from a bare null), so a round-trippable fixture must start from the reason that
            // survives the trip. The reason-preservation behaviour itself is measurement.rs's
            // own concern and is exercised there, not duplicated here.
            added_ttft_p50_us: Measurement::absent(Absent::NotMeasured),
            added_ttft_p99_us: Measurement::absent(Absent::NotMeasured),
            ttft_gw_samples: Measurement::absent(Absent::NotMeasured),
            ttft_direct_samples: Measurement::absent(Absent::NotMeasured),
            added_gap_p50_us: Measurement::absent(Absent::NotMeasured),
            added_gap_p99_us: Measurement::absent(Absent::NotMeasured),
            streams_sustained: Measurement::absent(Absent::NotMeasured),
            streams_sustained_fps: Measurement::absent(Absent::NotMeasured),
            streams_sustained_mock_ceiling: None,
            streams_sustained_headroom: None,
            sweep_streams: vec![],
            stream_c1_note: None,
        }),
        memory: None,
        timings_s: None,
    }
}

fn sample_upstream() -> Upstream {
    let mut cells = HashMap::new();
    cells.insert("openai".to_string(), sample_cell());
    Upstream {
        configurable: true,
        served: true,
        egress_config: Some("default".to_string()),
        serve_error: String::new(),
        cells,
    }
}

fn sample_matrix() -> Matrix {
    let mut upstreams = HashMap::new();
    upstreams.insert("openai".to_string(), sample_upstream());
    let mut cells = HashMap::new();
    cells.insert("openai".to_string(), sample_cell());
    Matrix {
        gateway: "gw".to_string(),
        build: "gw:1.0.0".to_string(),
        matrix_version: Some(2),
        served: true,
        serve_error: String::new(),
        upstream_shape: "openai".to_string(),
        upstream_note: "v2: full 6x6".to_string(),
        egress_configured: "openai".to_string(),
        probe_first: true,
        capability_note: "advisory context only".to_string(),
        cell_perf_sweep: true,
        sweep_rung_selection: Some("adaptive".to_string()),
        sweep_ttft_ms: Some(500),
        p99_ceiling_ms: Some(200),
        sweep_dur: Some(10),
        cell_stream: true,
        cell_memory: Some(true),
        memory: None,
        cells,
        upstreams,
        model: Some("gpt-4o-mini".to_string()),
        upstream_endpoint: Some("/v1/chat/completions".to_string()),
        ootb_config: None,
        arch: Some("arm64".to_string()),
        hardware: Some("m7g.4xlarge".to_string()),
        rig: None,
        measured_at: "2026-07-25T08:26:15Z".to_string(),
        started_at: None,
        finished_at: None,
        duration_s: None,
        phase_s: None,
        build_env: None,
    }
}

fn sample_record() -> ResultSnapshot {
    ResultSnapshot {
        schema_version: 1,
        gateway_provenance: None,
        definitions: Default::default(),
        gateway: "gw".to_string(),
        build: "gw:1.0.0".to_string(),
        measured_at: "2026-07-25T08:26:15Z".to_string(),
        started_at: None,
        finished_at: None,
        duration_s: None,
        phase_s: None,
        arch: Some("arm64".to_string()),
        hardware: Some("m7g.4xlarge".to_string()),
        rig: None,
        config: ConfigFiles {
            files: HashMap::new(),
        },
        matrix: sample_matrix(),
        memory: None,
        streaming: None,
    }
}

// ── round-trip ───────────────────────────────────────────────────────────────────────────────

#[test]
fn record_round_trips_through_json() {
    let rec = sample_record();
    let js = serde_json::to_string(&rec).unwrap();
    let back: ResultSnapshot = serde_json::from_str(&js).unwrap();
    assert_eq!(rec, back);
}

#[test]
fn cell_perf_round_trips_through_json() {
    let perf = sample_perf();
    let js = serde_json::to_string(&perf).unwrap();
    let back: CellPerf = serde_json::from_str(&js).unwrap();
    assert_eq!(perf, back);
}

// THE REGRESSION THIS PINS: `Cell`'s hand-written `Serialize` lists its fields one by one, so
// adding a field to the struct does not add it to the wire - the compiler cannot catch a
// forgotten `st.serialize_field` call the way it would catch a forgotten struct literal field.
// `timings_s` was added to the struct, filled by the suite ("Cost belongs in the artifact"),
// and dropped by this exact omission earlier today; every other test here builds a `Cell` and
// only checks the fields it already knows to look for, so all 15 of this module's tests and all
// 11 of `artifact_contract.rs` stayed green with the line missing. This test looks at the one
// field none of them asserted on.
#[test]
fn timings_s_reaches_the_wire() {
    let mut cell = sample_cell();
    let mut timings = std::collections::BTreeMap::new();
    timings.insert("load".to_string(), 12.5);
    cell.timings_s = Some(timings);

    let v: serde_json::Value = serde_json::to_value(&cell).unwrap();
    assert_eq!(
        v.get("timings_s"),
        Some(&serde_json::json!({"load": 12.5})),
        "timings_s must be on the wire: got {v}"
    );

    let back: Cell = serde_json::from_str(&serde_json::to_string(&cell).unwrap()).unwrap();
    assert_eq!(back.timings_s, cell.timings_s);
}

#[test]
fn served_status_variant_round_trips() {
    for s in [
        Served::Bool(true),
        Served::Bool(false),
        Served::Status("untestable".to_string()),
    ] {
        let js = serde_json::to_string(&s).unwrap();
        let back: Served = serde_json::from_str(&js).unwrap();
        assert_eq!(s, back);
    }
}

// ── golden shape ─────────────────────────────────────────────────────────────────────────────

// ── absence ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn absent_stream_metric_serialises_as_null() {
    let cell = sample_cell();
    let stream = cell.stream.unwrap();
    let js = serde_json::to_string(&stream).unwrap();
    let v: serde_json::Value = serde_json::from_str(&js).unwrap();
    assert_eq!(v["added_ttft_p99_us"], serde_json::Value::Null);
}

// THE TOKEN AND THE PROSE ARE TWO FIELDS. `CellStream.reason` shares its name with `Cell.reason`,
// which every consumer reads as a stable token, while the stream block put the absence's free
// text there instead - so branching on `reason` gave a token in one block and a sentence in the
// other, and a reasonless absence published a null beside a status that plainly had a reason.
#[test]
fn a_stream_block_publishes_a_reason_token_with_its_prose_in_its_own_field() {
    let cell = sample_cell();
    let stream = cell.stream.expect("the fixture carries a stream block");
    let v: serde_json::Value = serde_json::to_value(&stream).expect("must serialise");
    let reason = v["reason"].as_str().unwrap_or_default();
    assert!(
        [
            Absent::NotServed,
            Absent::NotMeasured,
            Absent::BelowResolution,
            Absent::RigLimited,
            Absent::SearchExhausted,
            Absent::Untestable,
            Absent::HarnessError,
        ]
        .iter()
        .any(|a| a.token() == reason),
        "stream.reason must be a token from the absence vocabulary, got {reason:?}"
    );
    assert_eq!(
        v["stream_error"], "did not rebind the mock port",
        "the prose belongs in stream_error, not in reason"
    );
}

// ── escaping ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn hostile_strings_round_trip_exactly() {
    let hostile =
        "quote\" backslash\\ newline\n tab\t control\u{0007} unicode: caf\u{e9} \u{1f600}";
    let mut cell = sample_cell();
    cell.verdict_note = hostile.to_string();
    cell.body_snippet = hostile.to_string();
    cell.probe_note = Some(hostile.to_string());
    let js = serde_json::to_string(&cell).unwrap();
    let back: Cell = serde_json::from_str(&js).unwrap();
    assert_eq!(back.verdict_note, hostile);
    assert_eq!(back.body_snippet, hostile);
    assert_eq!(back.probe_note.as_deref(), Some(hostile));
}

#[test]
fn control_characters_are_escaped_not_emitted_raw() {
    let mut cell = sample_cell();
    cell.verdict_note = "line one\nline two\x01".to_string();
    let js = serde_json::to_string(&cell).unwrap();
    // A literal, unescaped control byte or raw newline inside a JSON string is invalid JSON;
    // serde_json must have escaped it, so the raw byte must not appear verbatim in the output.
    assert!(!js.contains('\u{0001}'));
    let reparsed: serde_json::Value = serde_json::from_str(&js).unwrap();
    assert_eq!(
        reparsed["verdict_note"],
        serde_json::json!("line one\nline two\u{0001}")
    );
}

// ── the shapes the published artifact must hold ─────────────────────────────────────────────

// The invariants below are properties of the TYPES, so they are asserted against a snapshot
// built right here, not by scanning results/snapshots/ and asserting on whatever is committed
// there: a test that a `git rm` can fail is measuring the wrong thing. Whether real artifacts
// parse is a stronger claim and is covered where it belongs: engine/tests/end_to_end.rs drives
// the real binary and reads back the snapshot it actually wrote.

// The other half: a cell that was not served must not arrive carrying numbers. Fabricating a
// perf block for an unserved cell would publish a capability the gateway never demonstrated.
#[test]
fn an_unserved_cell_carries_no_perf_across_the_wire() {
    let mut snap = sample_record();
    for up in snap.matrix.upstreams.values_mut() {
        up.served = false;
        for cell in up.cells.values_mut() {
            cell.served = Served::Bool(false);
            cell.perf = None;
            cell.stream = None;
        }
    }
    let js = serde_json::to_string(&snap).expect("a snapshot must serialise");
    let back: ResultSnapshot = serde_json::from_str(&js).expect("its own output must parse");
    for up in back.matrix.upstreams.values() {
        assert!(!up.served);
        for cell in up.cells.values() {
            assert!(cell.perf.is_none(), "an unserved cell must not carry perf");
            assert!(
                cell.stream.is_none(),
                "an unserved cell must not carry stream"
            );
        }
    }
}

#[test]
fn an_interrupted_sweep_round_trips_as_explicit_partial_evidence() {
    let mut perf = sample_perf();
    perf.sweep_max_proxy_interrupted = true;
    let json = serde_json::to_value(&perf).unwrap();
    assert_eq!(json["sweep_max_proxy_interrupted"], true);
    let decoded: CellPerf = serde_json::from_value(json).unwrap();
    assert!(decoded.sweep_max_proxy_interrupted);
}
