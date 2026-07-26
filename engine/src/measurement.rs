// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE TYPE THIS ENGINE EXISTS FOR.
//
// The board's central discipline is that an absent measurement publishes null WITH A REASON, and is
// never substituted by 0, by a default, by a previous run's value, or by the edge of a search range.
// In the shell engine that discipline was a convention, and it broke repeatedly and silently, always
// the same way: `rps="${rps:-0}"` turned an unprobed rung into a measured zero; an unbound variable
// killed a subshell whose empty output was then read as a rig failure; a search that ran out of
// range published the bound as if it were the gateway's ceiling. Shell has no type for absence, so
// unset, empty, and "0" are the same three bytes of nothing and the difference lived only in whoever
// was reading the code that day.
//
// `Measurement<T>` makes the mistake unrepresentable rather than discouraged. There is no way to
// reach a number without matching on the absent case, and there is no constructor that produces a
// value and a reason together, or neither. A conversion to a plottable/rankable number is explicit
// and returns `Option`, so a caller that wants a default has to write the default down where a
// reviewer can see it.

use serde::{Deserialize, Serialize, Serializer};
use std::fmt;

/// Why a measurement is absent. Every variant is a thing the harness can *prove*, not a guess: the
/// board renders these to a reader, so a wrong reason is as bad as a wrong number. In particular
/// `RigLimited` and `HarnessError` must never be swapped, which is exactly the confusion that once
/// published a lost-port race as the explanation for a variable-ordering bug in our own file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Absent {
    /// The gateway does not serve this pairing at all. Measured and disclosed, never hidden.
    NotServed,
    /// The window never produced a usable sample (the probe returned nothing, or the deadline fired
    /// before anything was probed). Distinct from a measured zero, which is a real result.
    NotMeasured,
    /// The number was bounded by the test rig rather than by the gateway, so it says nothing about
    /// the gateway. It must not rank, win a comparison, or draw a bar.
    RigLimited,
    /// The search ran off the end of its range while still improving, so what it found is a LOWER
    /// BOUND, not a ceiling or a peak. Publishing the bound would report our own search range as the
    /// gateway's capability, which is how unrelated gateways come to share an identical number.
    SearchExhausted,
    /// The rig cannot pose this question at all (for example the mock does not synthesise the
    /// stream shape this dialect would need). A rig reachability limit, never a gateway fault.
    Untestable,
    /// The harness itself is misconfigured or broken. Never publishable as a gateway or rig property:
    /// a bug of ours wearing a rig failure's clothes is the worst shape a defect can take here.
    HarnessError,
}

impl Absent {
    /// The short machine-readable token published alongside the null.
    pub fn token(&self) -> &'static str {
        match self {
            Absent::NotServed => "not_served",
            Absent::NotMeasured => "not_measured",
            Absent::RigLimited => "rig_limited",
            Absent::SearchExhausted => "search_exhausted",
            Absent::Untestable => "untestable",
            Absent::HarnessError => "harness_error",
        }
    }

    /// Whether this absence is a statement about the GATEWAY. The others are statements about the
    /// rig or about us, and conflating the two is the failure this enum exists to prevent.
    pub fn is_about_gateway(&self) -> bool {
        matches!(self, Absent::NotServed)
    }
}

impl fmt::Display for Absent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

/// A quantity the harness either measured or did not.
///
/// Serialises as the bare value or as `null`. The reason travels beside it in the sealed envelope
/// rather than inside the number, so a consumer that reads only the value can never accidentally
/// read a reason as data.
#[derive(Debug, Clone, PartialEq)]
pub enum Measurement<T> {
    Measured(T),
    Absent { reason: Absent, detail: Option<String> },
}

impl<T> Measurement<T> {
    pub fn absent(reason: Absent) -> Self {
        Measurement::Absent { reason, detail: None }
    }

    /// An absence with operator-facing evidence attached. Use this whenever the reason alone would
    /// leave a reader asking "bounded by what?".
    pub fn absent_because(reason: Absent, detail: impl Into<String>) -> Self {
        Measurement::Absent { reason, detail: Some(detail.into()) }
    }

    pub fn is_measured(&self) -> bool {
        matches!(self, Measurement::Measured(_))
    }

    pub fn reason(&self) -> Option<&Absent> {
        match self {
            Measurement::Measured(_) => None,
            Measurement::Absent { reason, .. } => Some(reason),
        }
    }

    pub fn detail(&self) -> Option<&str> {
        match self {
            Measurement::Measured(_) => None,
            Measurement::Absent { detail, .. } => detail.as_deref(),
        }
    }

    /// The measured value, if there is one.
    ///
    /// Deliberately returns `Option` and is deliberately NOT `unwrap_or`-friendly at the call site:
    /// clippy denies `unwrap` and `expect` crate-wide, so substituting a default requires writing
    /// the default explicitly where review can see it. There is no `value_or_zero`, and there never
    /// should be: that function is the bug this whole type replaces.
    pub fn value(&self) -> Option<&T> {
        match self {
            Measurement::Measured(v) => Some(v),
            Measurement::Absent { .. } => None,
        }
    }

    pub fn into_value(self) -> Option<T> {
        match self {
            Measurement::Measured(v) => Some(v),
            Measurement::Absent { .. } => None,
        }
    }

    pub fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Measurement<U> {
        match self {
            Measurement::Measured(v) => Measurement::Measured(f(v)),
            Measurement::Absent { reason, detail } => Measurement::Absent { reason, detail },
        }
    }

    /// Demote a measured value to absent. The measured case is discarded ON PURPOSE: this is how a
    /// value that was really taken, but cannot honestly be published (rig-limited, or the winner of
    /// an exhausted search), stops being a number without becoming a zero.
    pub fn suppress(self, reason: Absent) -> Self {
        match self {
            Measurement::Measured(_) => Measurement::absent(reason),
            other => other,
        }
    }

    pub fn suppress_because(self, reason: Absent, detail: impl Into<String>) -> Self {
        match self {
            Measurement::Measured(_) => Measurement::absent_because(reason, detail),
            other => other,
        }
    }
}

impl<T: Copy> Measurement<T> {
    pub fn copied(&self) -> Option<T> {
        match self {
            Measurement::Measured(v) => Some(*v),
            Measurement::Absent { .. } => None,
        }
    }
}

/// The default is ABSENT, never a zero.
///
/// A `Default` on a measurement is only safe because this is the one it can be: a struct built with
/// `..Default::default()` then starts every metric as "not measured" and has to be explicitly given
/// a value to publish one. A `Default` of `Measured(0)` would be the cardinal defect handed out for
/// free by the derive macro.
impl<T> Default for Measurement<T> {
    fn default() -> Self {
        Measurement::Absent { reason: Absent::NotMeasured, detail: None }
    }
}

/// Serialises to the value or to `null`. Never to 0, and never to a string carrying the reason.
impl<T: Serialize> Serialize for Measurement<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Measurement::Measured(v) => v.serialize(s),
            Measurement::Absent { .. } => s.serialize_none(),
        }
    }
}

/// A `null` on the wire deserialises to `NotMeasured` rather than to a zero. Reading a historical
/// snapshot must not invent a value that the run never took.
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Measurement<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let opt = Option::<T>::deserialize(d)?;
        Ok(match opt {
            Some(v) => Measurement::Measured(v),
            None => Measurement::absent(Absent::NotMeasured),
        })
    }
}

#[cfg(test)]
mod tests {
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
    // from an absence. In the shell engine these two collapsed into the same bytes, which is how an
    // unprobed rung was published as "0 rps with 0 failures".
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
        assert_eq!(serde_json::to_string(&absent).ok(), Some("null".to_string()));
    }

    #[test]
    fn absent_serialises_as_null_never_zero() {
        for reason in [
            Absent::NotServed,
            Absent::NotMeasured,
            Absent::RigLimited,
            Absent::SearchExhausted,
            Absent::Untestable,
            Absent::HarnessError,
        ] {
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
        let v: Measurement<f64> = serde_json::from_str("12.5").unwrap_or(Measurement::absent(Absent::HarnessError));
        assert_eq!(v.copied(), Some(12.5));
    }

    // Suppression is how a rig-limited or range-exhausted number stops being published. It must
    // DISCARD the value: keeping it recoverable is how a raw ungated number escapes to a renderer.
    #[test]
    fn suppress_discards_the_value_and_records_why() {
        let m = Measurement::Measured(334_838.0_f64).suppress_because(
            Absent::SearchExhausted,
            "fps still rising at the top of the search range",
        );
        assert_eq!(m.copied(), None);
        assert_eq!(m.reason(), Some(&Absent::SearchExhausted));
        assert!(m.detail().unwrap_or_default().contains("still rising"));
        assert_eq!(serde_json::to_string(&m).ok(), Some("null".to_string()));
    }

    // Suppressing something already absent must not overwrite the original, more specific reason.
    #[test]
    fn suppress_does_not_relabel_an_existing_absence() {
        let m: Measurement<f64> = Measurement::absent(Absent::NotServed).suppress(Absent::RigLimited);
        assert_eq!(m.reason(), Some(&Absent::NotServed));
    }

    #[test]
    fn map_preserves_the_reason_and_detail() {
        let m: Measurement<f64> = Measurement::absent_because(Absent::Untestable, "no SSE for this dialect");
        let mapped = m.map(|v| v as i64);
        assert_eq!(mapped.reason(), Some(&Absent::Untestable));
        assert_eq!(mapped.detail(), Some("no SSE for this dialect"));
    }

    // A reason about the RIG or about US must never read as a statement about the gateway. This is
    // the distinction that got inverted in the field, publishing a harness bug as a rig race.
    #[test]
    fn only_not_served_is_a_statement_about_the_gateway() {
        assert!(Absent::NotServed.is_about_gateway());
        for r in [
            Absent::NotMeasured,
            Absent::RigLimited,
            Absent::SearchExhausted,
            Absent::Untestable,
            Absent::HarnessError,
        ] {
            assert!(!r.is_about_gateway(), "{r} is not a gateway property");
        }
    }

    /// Every reason, so a variant added later cannot quietly escape the invariants below.
    const ALL: [Absent; 6] = [
        Absent::NotServed,
        Absent::NotMeasured,
        Absent::RigLimited,
        Absent::SearchExhausted,
        Absent::Untestable,
        Absent::HarnessError,
    ];

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
            let back: Absent =
                serde_json::from_str(&json).unwrap_or(Absent::HarnessError);
            assert_eq!(back, reason, "a reason must survive a snapshot round trip unchanged");
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
        assert_eq!(back.ceiling.copied(), None, "a null must never read back as a number");
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

    // Suppression without a detail still discards the value and still records why. The value-less
    // form exists for the cases where the reason alone is the whole story, and it must not become a
    // quiet no-op that leaves a rig-limited number publishable.
    #[test]
    fn suppress_without_a_detail_still_discards_the_value() {
        for reason in ALL {
            let m = Measurement::Measured(48_000.0_f64).suppress(reason.clone());
            assert_eq!(m.copied(), None, "{reason} must discard the value it suppresses");
            assert_eq!(m.reason(), Some(&reason));
            assert_eq!(m.detail(), None);
            assert_eq!(serde_json::to_string(&m).ok(), Some("null".to_string()));
        }
    }

    #[test]
    fn tokens_are_stable_and_distinct() {
        let all = [
            Absent::NotServed,
            Absent::NotMeasured,
            Absent::RigLimited,
            Absent::SearchExhausted,
            Absent::Untestable,
            Absent::HarnessError,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for r in &all {
            assert!(seen.insert(r.token()), "duplicate token {}", r.token());
            assert!(!r.token().is_empty());
        }
        assert_eq!(seen.len(), all.len());
    }
}
