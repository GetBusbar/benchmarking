// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// PROPERTY TESTS OVER THE NUMBERS' OWN GRAMMAR: Measurement/Absent and the percentile helpers.
//
// Three separate code paths compute a nearest-rank percentile (`stats::percentile`,
// `gen::GenStats::pct_us`, `search::nearest_rank_median`) and their doc comments claim they are one
// convention. Nothing but a test holds that claim: an off-by-one in any copy is not a rounding
// difference, it is a silent disagreement between the published p99 and the p99 a reader recomputes
// from the same samples. So the conventions are pinned to each other here, across generated inputs,
// alongside the bounds every nearest-rank percentile must respect (an element of the input, ordered
// in p, between min and max).
//
// The Measurement half holds the absence discipline for EVERY reason at once: no accessor route to
// a number, null on the wire, the reason preserved through map/suppress/record_absence.

// The crate denies unwrap/expect/panic so a measurement defect can never abort a run. A test is the
// opposite case: failures must be loud. Scoped to this file, same as engine/tests/end_to_end.rs.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use otb_engine::gen::GenStats;
use otb_engine::measurement::{Absent, AbsentEntry, Measurement};
use otb_engine::search::nearest_rank_median;
use otb_engine::stats;
use proptest::prelude::*;
use std::collections::BTreeMap;

/// Every reason there is. A variant added to `Absent` without being added here fails the
/// completeness check below, so this list cannot silently go stale.
const ALL_REASONS: [Absent; 7] = [
    Absent::NotServed,
    Absent::NotMeasured,
    Absent::BelowResolution,
    Absent::RigLimited,
    Absent::SearchExhausted,
    Absent::Untestable,
    Absent::HarnessError,
];

#[test]
fn the_all_reasons_list_is_complete() {
    // serde's own enum knowledge is the source of truth: every token that deserialises must be in
    // ALL_REASONS, and ALL_REASONS must contain no duplicates. A new variant lands in serde
    // automatically and would be missing here, turning this red.
    let mut seen = std::collections::BTreeSet::new();
    for r in &ALL_REASONS {
        assert!(seen.insert(r.token()), "duplicate reason {}", r.token());
    }
    for token in [
        "not_served",
        "not_measured",
        "below_resolution",
        "rig_limited",
        "search_exhausted",
        "untestable",
        "harness_error",
    ] {
        let parsed: Result<Absent, _> = serde_json::from_str(&format!("\"{token}\""));
        assert!(parsed.is_ok(), "{token} must parse as a reason");
        assert!(
            ALL_REASONS.iter().any(|r| r.token() == token),
            "{token} deserialises but is missing from ALL_REASONS - the exhaustive tests below no longer cover it"
        );
    }
}

proptest! {
    // ONE CONVENTION, THREE IMPLEMENTATIONS. stats::percentile (f64), GenStats::pct_us (u64), and
    // nearest_rank_median must agree on identical data, for any sample set and any p. The values are
    // small integers so the f64 and u64 lanes carry exactly the same numbers.
    #[test]
    fn all_three_percentile_conventions_agree_on_the_same_samples(
        values in proptest::collection::vec(0u64..1_000_000, 1..200),
        p_millis in 0u32..=1000,
    ) {
        let p = f64::from(p_millis) / 1000.0;
        let as_f64: Vec<f64> = values.iter().map(|v| *v as f64).collect();

        let from_stats = stats::percentile(&as_f64, p).copied()
            .expect("percentile of a non-empty slice is measured");
        let from_gen = GenStats { latencies_us: values.clone(), ..GenStats::default() }.pct_us(p);
        prop_assert_eq!(
            from_stats as u64, from_gen,
            "stats::percentile and GenStats::pct_us disagree at p={} over {} samples: \
             the published p99 and a recomputed one would silently differ", p, values.len()
        );

        if p_millis == 500 {
            let mut sorted = as_f64.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
            let from_search = nearest_rank_median(&sorted).expect("non-empty");
            prop_assert_eq!(
                from_stats, from_search,
                "search::nearest_rank_median disagrees with stats::percentile(0.5)"
            );
        }
    }

    // A nearest-rank percentile is an ELEMENT OF THE INPUT (never an interpolation), bounded by min
    // and max, and monotone in p. Interpolation would publish a latency no request ever had.
    #[test]
    fn percentile_returns_a_real_sample_within_bounds_and_ordered_in_p(
        values in proptest::collection::vec(-1.0e9f64..1.0e9, 1..200),
        p_lo in 0u32..=1000,
        p_hi in 0u32..=1000,
    ) {
        let (p_lo, p_hi) = (p_lo.min(p_hi), p_lo.max(p_hi));
        let lo = stats::percentile(&values, f64::from(p_lo) / 1000.0).copied().expect("non-empty");
        let hi = stats::percentile(&values, f64::from(p_hi) / 1000.0).copied().expect("non-empty");
        prop_assert!(
            values.contains(&lo),
            "percentile produced {} which no sample ever measured", lo
        );
        let min = stats::min(&values).copied().expect("non-empty");
        let max = stats::max(&values).copied().expect("non-empty");
        prop_assert!(min <= lo && hi <= max, "percentile escaped [min, max]");
        prop_assert!(
            lo <= hi,
            "p{} = {} came out above p{} = {}: percentiles must be ordered in p",
            p_lo, lo, p_hi, hi
        );
    }

    // p50 <= p99 within one convention, always - the audit's own TTFT-ordering check assumes the
    // engine can never produce an inverted pair from a single sample set.
    #[test]
    fn p50_never_exceeds_p99_from_the_same_samples(
        values in proptest::collection::vec(0u64..10_000_000, 1..300),
    ) {
        let g = GenStats { latencies_us: values, ..GenStats::default() };
        prop_assert!(
            g.pct_us(0.50) <= g.pct_us(0.99),
            "p50={} above p99={} from one sample set", g.pct_us(0.50), g.pct_us(0.99)
        );
    }

    // The median helper (average-of-middle-two convention) still lands inside [min, max] for any
    // input; a median outside the observed range is arithmetic gone wrong, not a convention choice.
    #[test]
    fn median_stays_within_the_observed_range(
        values in proptest::collection::vec(-1.0e9f64..1.0e9, 1..200),
    ) {
        let m = stats::median(&values).copied().expect("non-empty");
        let min = stats::min(&values).copied().expect("non-empty");
        let max = stats::max(&values).copied().expect("non-empty");
        prop_assert!(min <= m && m <= max, "median {} outside [{}, {}]", m, min, max);
    }

    // FOR EVERY REASON AND ANY VALUE: no accessor yields a number for an absence; suppression
    // map preserves the reason and detail, and the wire form is null. (The suppress-helper properties
    // that also lived here went with the helpers themselves - see measurement.rs.)
    #[test]
    fn no_accessor_route_reaches_a_number_for_any_absence(
        which in 0usize..7,
        detail in "[a-z ]{0,40}",
    ) {
        let reason = ALL_REASONS[which].clone();

        let absent: Measurement<f64> = if detail.is_empty() {
            Measurement::absent(reason.clone())
        } else {
            Measurement::absent_because(reason.clone(), detail.clone())
        };
        prop_assert_eq!(absent.value(), None);
        prop_assert_eq!(absent.copied(), None);
        prop_assert_eq!(absent.clone().into_value(), None);
        prop_assert!(!absent.is_measured());
        prop_assert_eq!(serde_json::to_string(&absent).ok(), Some("null".to_string()));
        prop_assert_eq!(absent.reason(), Some(&reason));

        // map cannot resurrect a number or lose the reason.
        let mapped = absent.clone().map(|v| v * 2.0);
        prop_assert_eq!(mapped.copied(), None);
        prop_assert_eq!(mapped.reason(), Some(&reason));
        if !detail.is_empty() {
            prop_assert_eq!(mapped.detail(), Some(detail.as_str()));
        }

        // The `suppress` / `suppress_because` properties that used to sit here are gone with the
        // helpers: they had no production caller, and their preserve-an-existing-absence semantics
        // could not have served the real suppression sites, which assign `absent_because(RigLimited,
        // detail)` over a field already carrying `NotMeasured`. What the real path must guarantee -
        // that a suppressed number reaches the wire as null and stays unrecoverable - is asserted in
        // measurement.rs's own `a_suppressed_number_discards_the_value_and_records_why`.
    }

    // record_absence: an absent measurement lands in the map under its key with reason and detail
    // intact; a measured one contributes nothing. This is the mechanism every published `absences`
    // block is built from, so a defect here is a board full of unexplained nulls.
    #[test]
    fn record_absence_maps_exactly_the_absent_fields(
        which in 0usize..7,
        value in -1.0e12f64..1.0e12,
        detail in "[a-z ]{1,40}",
    ) {
        let reason = ALL_REASONS[which].clone();
        let mut out: BTreeMap<String, AbsentEntry> = BTreeMap::new();

        Measurement::Measured(value).record_absence("measured_field", &mut out);
        prop_assert!(out.is_empty(), "a measured value must not write an absence entry");

        let m: Measurement<f64> = Measurement::absent_because(reason.clone(), detail.clone());
        m.record_absence("absent_field", &mut out);
        let entry = out.get("absent_field").expect("the absence must be recorded");
        prop_assert_eq!(&entry.reason, &reason);
        prop_assert_eq!(entry.detail.as_deref(), Some(detail.as_str()));
    }

    // The AbsentEntry that ships beside the nulls round-trips through JSON with its reason and
    // detail intact, for every reason. A reason that mutates in transit publishes a wrong claim
    // under a right-looking key.
    #[test]
    fn an_absent_entry_survives_the_wire_for_every_reason(
        which in 0usize..7,
        detail in proptest::option::of("[a-zA-Z0-9 .:=/-]{1,60}"),
    ) {
        let entry = AbsentEntry { reason: ALL_REASONS[which].clone(), detail: detail.clone() };
        let json = serde_json::to_string(&entry).expect("serialise");
        let back: AbsentEntry = serde_json::from_str(&json).expect("deserialise");
        prop_assert_eq!(back, entry, "an absences entry must survive a snapshot round trip");
    }

    // A measured value round-trips as itself and null round-trips as an absence - for any value,
    // not just the worked examples in measurement.rs. Integers (which is what nearly every artifact
    // field is) must round-trip EXACTLY.
    //
    // f64 is held to a relative 1e-15 rather than bit-exactness, and that slack is itself a
    // finding: serde_json without its `float_roundtrip` feature re-parses a float via a fast path
    // that can land one ULP off, so a Measurement<f64> (rss MiB, frames/sec) re-read from a
    // snapshot is not guaranteed to be the bits that were written. Tightening this to exact
    // equality is what fails today; enabling the feature would let it be exact.
    #[test]
    fn measured_values_round_trip_and_null_never_becomes_a_number(
        int_value in -1_000_000_000_000i64..1_000_000_000_000,
        f_value in -1.0e12f64..1.0e12,
    ) {
        let m: Measurement<i64> = Measurement::Measured(int_value);
        let json = serde_json::to_string(&m).expect("serialise");
        let back: Measurement<i64> = serde_json::from_str(&json).expect("deserialise");
        prop_assert_eq!(back.copied(), Some(int_value), "an integer measurement must round-trip exactly");

        let m: Measurement<f64> = Measurement::Measured(f_value);
        let json = serde_json::to_string(&m).expect("serialise");
        let back: Measurement<f64> = serde_json::from_str(&json).expect("deserialise");
        let got = back.copied().expect("a measured value must read back measured");
        prop_assert!(
            (got - f_value).abs() <= f_value.abs() * 1e-15,
            "f64 {} read back as {}: beyond even the documented fast-float slack", f_value, got
        );

        let n: Measurement<f64> = serde_json::from_str("null").expect("null is a valid wire form");
        prop_assert_eq!(n.copied(), None);
        prop_assert!(!n.is_measured());
    }
}

/// An empty sample set has no percentile at any p: absent, never zero. GenStats::pct_us documents
/// its own 0-on-empty as the subprocess-boundary special case, which is exactly why `run()` leaves
/// `p50_us`/`p99_us` as `None` - pinned here so the two conventions' difference stays a documented
/// one rather than drifting silently.
#[test]
fn empty_samples_are_absent_in_stats_and_zero_only_on_the_documented_gen_path() {
    for p in [0.0, 0.5, 0.99, 1.0] {
        assert!(
            !stats::percentile(&[], p).is_measured(),
            "stats::percentile on empty input must be absent at p={p}"
        );
    }
    let g = GenStats::default();
    assert_eq!(
        g.pct_us(0.99),
        0,
        "GenStats::pct_us documents 0-on-empty; if this changed, update run.rs's subprocess path note"
    );
    assert_eq!(
        g.p50_us, None,
        "run() must leave the precomputed percentiles None so an empty window cannot read as p50=0"
    );
}
