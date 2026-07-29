// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE ARTIFACT CONTRACT AS AN EXECUTABLE SPEC.
//
// The published cell is the boundary between this engine and everything that reads it (site/, the
// python audit, a human with jq). The contract the readers assume, stated as tests:
//
//   1. every declared metric field is PRESENT on the wire - measured as a number, absent as an
//      explicit null key, never omitted (a dropped key is indistinguishable from a metric the
//      engine does not know about);
//   2. every null among the declared metrics carries exactly one entry in the cell's `absences`
//      map, under its own prefixed name, with the reason and detail that were recorded - and no
//      absences entry points at a field that is actually a number;
//   3. the served/stream_served vocabulary is closed: `true`, `false`, or a documented reader-facing
//      token - and the DEFAULT is the conservative "not_probed", never a `false` that reads as a
//      measured refusal;
//   4. a cell that round-trips through the wire keeps measured values measured and absences absent.
//
// The defect class this file exists for is the record.rs rename that left all engine tests green
// while the site silently read `undefined`: nothing held the producer to the shape the consumers
// parse. These tests are that hold, from the producer side.

// The crate denies unwrap/expect/panic so a measurement defect can never abort a run. A test is the
// opposite case: failures must be loud. Scoped to this file, same as engine/tests/end_to_end.rs.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use otb_engine::cell::{CellId, CellOutcome, Evidence, Served as ProbeServed, Skipped};
use otb_engine::measurement::{Absent, Measurement};
use otb_engine::probe::Verdict;
use otb_engine::record::{
    Cell, CellMemory, CellPerf, CellStream, Served, StreamServed, SweepPoint,
};

// The declared metric fields, LITERALLY. Mirrors of record.rs's own absences_of! lists, kept as
// strings on purpose: a test that asked the struct for its field names would agree with any rename
// and hold nothing. If a Measurement field is added to a block without being added here, the
// every-field-absent test below fails on the extra absences entry, so the list cannot go stale.
const PERF_METRICS: [&str; 10] = [
    "added_latency_p50_us",
    "added_latency_p99_us",
    "gateway_c1_p99_us",
    "direct_c1_p99_us",
    "rps_sustained_20ms",
    "rps_sustained_20ms_concurrency",
    "conc_at_sustained",
    "rps_max_proxy",
    "rps_max_proxy_concurrency",
    "conc_at_peak",
];
const STREAM_METRICS: [&str; 8] = [
    "added_ttft_p50_us",
    "added_ttft_p99_us",
    "added_gap_p50_us",
    "added_gap_p99_us",
    "streams_sustained",
    "streams_sustained_fps",
    "cpu_fps",
    "cpu_fps_concurrency",
];
// `plateaued` and `load_s` joined this list when they stopped being bare `Option`s that collapsed
// the memory group's reason on the way out: a window that could not judge the plateau published two
// nulls nothing could explain. They are `Measurement`s now, so they ride in the absences map like
// every other number and are held to the same contract here.
/// CONDITIONAL FIELDS: absent BECAUSE the measurement succeeded.
///
/// `shape` and `idle_shape` describe HOW a window failed to settle. On a window that DID settle there
/// is no unsettled shape to describe, so the honest publication is an absence carrying exactly that
/// reason. That is the opposite of a hole - the absence IS the result.
///
/// They are therefore excluded from MEMORY_METRICS, which asserts "measured means a number", and
/// named here instead so the fully-measured test still holds them to carrying a reason. Forcing them
/// into the numeric list would make that fixture fake an unsettled window in order to satisfy a
/// contract about settled ones.
const CONDITIONAL_MEMORY_METRICS: [&str; 2] = ["shape", "idle_shape"];

const MEMORY_METRICS: [&str; 9] = [
    "idle_rss_mib",
    "steady_state_rss_mib",
    "recovered_rss_mib",
    "peak_rss_mib",
    "peak_rss_hwm_mib",
    "plateaued",
    "load_s",
    "time_to_plateau_s",
    "growth_rate_mib_per_min",
];

fn measured_perf() -> CellPerf {
    CellPerf {
        added_latency_p50_us: Measurement::Measured(120),
        added_latency_p99_us: Measurement::Measured(480),
        gateway_c1_p99_us: Measurement::Measured(2_000),
        direct_c1_p99_us: Measurement::Measured(1_520),
        rps_sustained_20ms: Measurement::Measured(28_000),
        rps_sustained_20ms_concurrency: Measurement::Measured(96),
        conc_at_sustained: Measurement::Measured(96),
        rps_max_proxy: Measurement::Measured(31_000),
        rps_max_proxy_concurrency: Measurement::Measured(128),
        conc_at_peak: Measurement::Measured(128),
        ..CellPerf::default()
    }
}

fn measured_stream() -> CellStream {
    CellStream {
        stream_served: StreamServed::Bool(true),
        added_ttft_p50_us: Measurement::Measured(40),
        added_ttft_p99_us: Measurement::Measured(90),
        added_gap_p50_us: Measurement::Measured(5),
        added_gap_p99_us: Measurement::Measured(12),
        streams_sustained: Measurement::Measured(1_300),
        streams_sustained_fps: Measurement::Measured(39_000.0),
        cpu_fps: Measurement::Measured(48_000.0),
        cpu_fps_concurrency: Measurement::Measured(256),
        ..CellStream::default()
    }
}

fn measured_memory() -> CellMemory {
    CellMemory {
        served: true,
        idle_rss_mib: Measurement::Measured(42.5),
        steady_state_rss_mib: Measurement::Measured(120.0),
        recovered_rss_mib: Measurement::Measured(60.0),
        peak_rss_mib: Measurement::Measured(180.0),
        peak_rss_hwm_mib: Measurement::Measured(185.0),
        time_to_plateau_s: Measurement::Measured(72.0),
        growth_rate_mib_per_min: Measurement::Measured(0.3),
        plateaued: Measurement::Measured(true),
        load_s: Measurement::Measured(120),
        ..CellMemory::default()
    }
}

fn as_json(cell: &Cell) -> serde_json::Value {
    serde_json::to_value(cell).expect("a cell must serialise")
}

/// Contract 1: A fully-measured cell publishes EVERY declared field as a number and an EMPTY absences map:
/// there is nothing to explain, and a leftover entry would accuse a measured field of being a hole.
#[test]
fn a_fully_measured_cell_has_every_declared_field_present_and_no_absences() {
    let cell = Cell {
        served: Served::Bool(true),
        perf: Some(measured_perf()),
        stream: Some(measured_stream()),
        memory: Some(measured_memory()),
        ..Cell::default()
    };
    let v = as_json(&cell);

    for (block, keys) in [
        ("perf", &PERF_METRICS[..]),
        ("stream", &STREAM_METRICS[..]),
        ("memory", &MEMORY_METRICS[..]),
    ] {
        for key in keys {
            let field = &v[block][key];
            // PRESENT AND NOT NULL is the contract, not "is a number". Most measurements are
            // numeric, but `memory.plateaued` is a `Measurement<bool>` and publishes `true` - a
            // real measured value. Demanding a number here would have forced that field back into
            // the bare-`Option` shape whose whole defect was that it could not carry a reason.
            assert!(
                !field.is_null() && (field.is_number() || field.is_boolean()),
                "{block}.{key} was measured and must publish its value, got {field}"
            );
        }
    }
    let absences = v["absences"]
        .as_object()
        .expect("absences must always be an object, even when empty");
    // The conditional fields are allowed to be absent on a SETTLED window - that is what they mean -
    // and are still required to say why. Split them out rather than ignoring them, so "settled, so
    // there is no unsettled shape" has to be a stated reason and not a silent gap.
    for f in CONDITIONAL_MEMORY_METRICS {
        let key = format!("memory.{f}");
        let entry = absences
            .get(&key)
            .unwrap_or_else(|| panic!("{key} must be a reasoned absence on a settled window"));
        assert!(
            entry.get("reason").is_some(),
            "{key} is absent and must carry a reason: {entry:?}"
        );
    }
    let unexplained: std::collections::BTreeMap<_, _> = absences
        .iter()
        .filter(|(k, _)| {
            !CONDITIONAL_MEMORY_METRICS
                .iter()
                .any(|f| *k == &format!("memory.{f}"))
        })
        .collect();
    assert!(
        unexplained.is_empty(),
        "a fully measured cell has nothing left to explain, but absences = {unexplained:?}"
    );
}

/// Contract 2: THE NULL<->ABSENCE BIJECTION. Every declared metric that is null on the wire has exactly one
/// prefixed absences entry, and every absences entry points at a field that really is null. Driven
/// with all three blocks fully absent (the `..Default::default()` cell), so a field added to a
/// block but forgotten in absences_of! - or vice versa - breaks the bijection and fails here.
#[test]
fn every_null_metric_carries_exactly_one_absence_and_no_absence_points_at_a_number() {
    let cell = Cell {
        served: Served::Bool(true),
        perf: Some(CellPerf::default()),
        stream: Some(CellStream::default()),
        memory: Some(CellMemory::default()),
        ..Cell::default()
    };
    let v = as_json(&cell);
    let absences = v["absences"].as_object().expect("absences object");

    let mut expected: Vec<String> = Vec::new();
    for (block, keys) in [
        ("perf", &PERF_METRICS[..]),
        ("stream", &STREAM_METRICS[..]),
        ("memory", &MEMORY_METRICS[..]),
        // The conditional fields are exempt from "measured means a number", NOT from the bijection.
        // On this all-defaulted cell nothing was measured at all, so they are null here like every
        // other field and must still carry a reason - drop them from this walk and a shape field
        // added to the block but forgotten in absences_of! would become a bare hole nothing catches.
        ("memory", &CONDITIONAL_MEMORY_METRICS[..]),
    ] {
        for key in keys {
            assert!(
                v[block][key].is_null(),
                "{block}.{key} defaulted and must be an explicit null key, got {}",
                v[block][key]
            );
            assert!(
                v[block].as_object().is_some_and(|o| o.contains_key(*key)),
                "{block}.{key} must be PRESENT as a null key, not omitted"
            );
            expected.push(format!("{block}.{key}"));
        }
    }
    for name in &expected {
        let entry = absences
            .get(name)
            .unwrap_or_else(|| panic!("null {name} has no absences entry: a bare hole"));
        assert_eq!(
            entry["reason"], "not_measured",
            "a defaulted measurement's reason is the weakest honest one"
        );
    }
    for name in absences.keys() {
        assert!(
            expected.contains(name),
            "absences entry {name} does not correspond to a declared null metric \
             (either it points at a measured field, or the declared lists above are stale)"
        );
    }
    assert_eq!(
        absences.len(),
        expected.len(),
        "exactly one absences entry per null metric"
    );
}

/// A mixed cell: the absences carry the SPECIFIC reason and detail each suppression recorded, under
/// the right prefixed key, while the measured fields beside them stay numbers with no entry.
#[test]
fn each_absence_travels_under_its_own_key_with_its_own_reason_and_detail() {
    let perf = CellPerf {
        rps_max_proxy: Measurement::absent_because(
            Absent::SearchExhausted,
            "still climbing at c=4096",
        ),
        rps_max_proxy_concurrency: Measurement::absent(Absent::SearchExhausted),
        conc_at_peak: Measurement::absent(Absent::SearchExhausted),
        ..measured_perf()
    };
    let stream = CellStream {
        added_ttft_p50_us: Measurement::absent_because(
            Absent::BelowResolution,
            "gateway leg read under the direct leg",
        ),
        cpu_fps: Measurement::absent(Absent::RigLimited),
        ..measured_stream()
    };
    let cell = Cell {
        served: Served::Bool(true),
        perf: Some(perf),
        stream: Some(stream),
        ..Cell::default()
    };
    let v = as_json(&cell);
    let absences = v["absences"].as_object().expect("absences object");

    assert_eq!(absences["perf.rps_max_proxy"]["reason"], "search_exhausted");
    assert_eq!(
        absences["perf.rps_max_proxy"]["detail"], "still climbing at c=4096",
        "the operator-facing evidence must survive to the wire"
    );
    assert_eq!(
        absences["stream.added_ttft_p50_us"]["reason"],
        "below_resolution"
    );
    assert_eq!(absences["stream.cpu_fps"]["reason"], "rig_limited");
    assert!(
        v["perf"]["rps_max_proxy"].is_null() && v["stream"]["cpu_fps"].is_null(),
        "a suppressed metric is null on the wire, never the number it discarded"
    );
    assert!(
        !absences.contains_key("perf.rps_sustained_20ms"),
        "a measured field must not carry an absences entry"
    );
    assert!(
        v["perf"]["rps_sustained_20ms"].is_number(),
        "the measured neighbours stay numbers"
    );
}

/// Contract 3: THE SERVED VOCABULARY IS CLOSED AND ITS DEFAULT IS CONSERVATIVE. `true`/`false` are measured
/// verdicts; the status strings are the documented reader-facing tokens; and a cell nobody probed
/// says "not_probed" - a default of `false` would claim a measured refusal by omission, and a
/// default of `true` would claim a capability by omission (the exact defect record.rs names).
#[test]
fn served_and_stream_served_publish_only_the_documented_vocabulary() {
    // The measured verdicts are bare JSON booleans.
    assert_eq!(
        serde_json::to_value(Served::Bool(true)).expect("serialise"),
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        serde_json::to_value(StreamServed::Bool(false)).expect("serialise"),
        serde_json::Value::Bool(false)
    );
    // The documented status tokens pass through as strings, verbatim.
    for token in [
        "not_configured",
        "not_configurable",
        "unprobed_auth",
        "not_verified",
        "untestable",
    ] {
        assert_eq!(
            serde_json::to_value(Served::Status(token.into())).expect("serialise"),
            serde_json::Value::String(token.into())
        );
    }
    // The defaults: conservative, and NOT booleans.
    let default_cell = as_json(&Cell::default());
    assert_eq!(
        default_cell["served"], "not_probed",
        "an unprobed cell must not read as served OR as a measured refusal"
    );
    let default_stream = serde_json::to_value(CellStream::default()).expect("serialise");
    assert_eq!(
        default_stream["stream_served"], "not_probed",
        "an unprobed stream lane must not read as a gateway that does not stream"
    );
    assert_ne!(
        default_stream["stream_served"],
        serde_json::Value::Bool(false),
        "false is a claim about the gateway; a default may never make it"
    );
}

/// Contract 4: The wire round-trip: measured stays measured, absent stays absent (reason downgraded to the
/// bare-null NotMeasured, which is the documented loss - the reason travels in `absences`, not
/// inside the number), and nothing becomes a zero.
#[test]
fn a_cell_round_trips_without_inventing_or_losing_values() {
    let cell = Cell {
        served: Served::Bool(true),
        perf: Some(CellPerf {
            rps_max_proxy: Measurement::absent(Absent::SearchExhausted),
            ..measured_perf()
        }),
        ..Cell::default()
    };
    let json = serde_json::to_string(&cell).expect("serialise");
    let back: Cell = serde_json::from_str(&json).expect("a cell we wrote must read back");
    let perf = back.perf.expect("perf block survives");
    assert_eq!(
        perf.rps_sustained_20ms.copied(),
        Some(28_000),
        "a measured value must survive the round trip"
    );
    assert_eq!(
        perf.rps_max_proxy.copied(),
        None,
        "a null must never read back as a number"
    );
    assert_eq!(
        perf.rps_max_proxy.reason(),
        Some(&Absent::NotMeasured),
        "a bare null reads back as the weakest reason; the specific one lives in absences"
    );
}

/// A sweep rung's own measurements follow the same discipline: absent serialises null, never zero,
/// and null reads back absent. A rung is the evidence a headline number is judged against, so a
/// zero invented here corrupts the audit that uses it.
#[test]
fn a_sweep_point_publishes_nulls_for_what_it_could_not_sample() {
    let point = SweepPoint {
        conc: 64,
        rps: Measurement::Measured(10_000),
        p99_us: Measurement::absent(Absent::NotMeasured),
        fail: Measurement::Measured(0),
    };
    let v = serde_json::to_value(&point).expect("serialise");
    assert_eq!(v["rps"], 10_000);
    assert!(v["p99_us"].is_null(), "an unsampled p99 is null, not 0");
    assert_eq!(v["fail"], 0, "a measured zero failure count stays a zero");

    let back: SweepPoint = serde_json::from_value(v).expect("a rung we wrote must read back");
    assert_eq!(back.p99_us.copied(), None);
    assert_eq!(back.fail.copied(), Some(0));
}

// ---- the probe/cell vocabulary the artifact is assembled from -----------------------------------

/// probe::Verdict's hand-written token() and its serde wire form are two spellings of one name;
/// they must be the same string, exactly as measurement.rs already holds for Absent. A verdict
/// renamed on one side would publish under one name and render under another.
#[test]
fn every_probe_verdict_serialises_as_exactly_its_published_token() {
    for v in [
        Verdict::NotConfigured,
        Verdict::Failed,
        Verdict::NotVerified,
    ] {
        let json = serde_json::to_string(&v).expect("serialise");
        assert_eq!(
            json,
            format!("\"{}\"", v.token()),
            "the wire form and the published token must be the same string"
        );
        let back: Verdict = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, v);
    }
}

/// The Skipped vocabulary publishes under a stable token, because a consumer branches on it.
///
/// It used to assert two tokens. `SuiteDeadline` is gone: nothing in the grid walk ever tracked
/// elapsed suite time or built one, so no artifact could ever contain it, and a variant nothing can
/// emit makes the enum claim a distinction the run cannot draw. What actually protects an interrupted
/// run is that cells stream to disk as they finish.
#[test]
fn skipped_reasons_publish_under_a_stable_token() {
    assert_eq!(
        serde_json::to_string(&Skipped::NotServed).ok(),
        Some("\"not_served\"".to_string())
    );
}

/// Evidence::snippet bounds the body without ever splitting a UTF-8 character: the snippet is
/// reader-facing, and a snippet cut mid-character is not printable evidence.
#[test]
fn an_evidence_snippet_is_bounded_and_never_cut_mid_character() {
    // 100 two-byte characters = 200 bytes exactly; adding one more crosses the cap with a
    // multi-byte char straddling the boundary.
    let body: String = "\u{00e9}".repeat(150); // 300 bytes of 2-byte chars
    let snippet = Evidence::snippet(&body);
    assert!(
        snippet.len() <= 200,
        "snippet must stay within its byte bound, got {} bytes",
        snippet.len()
    );
    assert!(
        snippet.chars().all(|c| c == '\u{00e9}'),
        "a boundary cut corrupted a character: {snippet:?}"
    );
    assert!(!snippet.is_empty(), "a non-empty body yields evidence");

    // A short body passes through whole, trimmed.
    assert_eq!(Evidence::snippet("  short body  "), "short body");
}

/// An auth-refused cell's note reads as OUR limit, never as the gateway's answer: the wording is
/// the difference between "this gateway fails" and "we cannot sign this dialect's requests", and
/// the constructor is the only place that wording is produced.
#[test]
fn an_unprobed_auth_cell_blames_the_harness_not_the_gateway() {
    let out = CellOutcome::unprobed_auth(
        CellId::new("openai", "bedrock"),
        Evidence {
            status: 403,
            body_snippet: "signature required".into(),
        },
    );
    assert!(!out.served.is_measurable());
    let note = out.note.expect("the refusal must carry its explanation");
    assert!(
        note.contains("does not forge signatures"),
        "the note must name the rig's own limit: {note}"
    );
    assert!(
        note.contains("403"),
        "the evidence status must be quoted in the note: {note}"
    );
    assert!(
        matches!(out.served, ProbeServed::UnprobedAuth(ref e) if e.status == 403),
        "the evidence itself must travel with the verdict"
    );
}

/// A not-served verdict CARRIES its evidence through serialisation: status and body snippet are
/// what let a reader distinguish a rig-side 4xx epidemic from a gateway that serves nothing.
#[test]
fn a_not_served_cell_keeps_its_evidence_on_the_wire() {
    let out = CellOutcome::not_served(
        CellId::new("openai", "anthropic"),
        Verdict::Failed,
        Evidence {
            status: 500,
            body_snippet: "upstream exploded".into(),
        },
        "persisted across the retry budget",
    );
    let v = serde_json::to_value(&out).expect("serialise");
    let no = &v["served"]["no"];
    assert_eq!(no[0], "failed", "the verdict token travels");
    assert_eq!(no[1]["status"], 500, "the status travels");
    assert_eq!(
        no[1]["body_snippet"], "upstream exploded",
        "the body evidence travels"
    );
    assert_eq!(v["skipped"], "not_served");
}
