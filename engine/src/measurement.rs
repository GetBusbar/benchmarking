// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE TYPE THIS ENGINE EXISTS FOR.
//
// The board's central discipline is that an absent measurement publishes null WITH A REASON, and is
// never substituted by 0, by a default, by a previous run's value, or by the edge of a search range.
// Shell has no type for absence, so unset, empty, and "0" are the same three bytes of nothing: a
// convention enforced by discipline alone is one `rps="${rps:-0}"` away from silently turning an
// unprobed rung into a measured zero.
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
/// `RigLimited` and `HarnessError` must never be swapped: doing so would publish a bug of our own as
/// a rig race, or a rig race as a bug of our own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Absent {
    /// The gateway does not serve this pairing at all. Measured and disclosed, never hidden.
    NotServed,
    /// The window never produced a usable sample (the probe returned nothing, or the deadline fired
    /// before anything was probed). Distinct from a measured zero, which is a real result.
    NotMeasured,
    /// The quantity is a DIFFERENCE of two legs and the raw difference came out at or below what the
    /// rig can resolve (the gateway leg read at or under the direct leg, which a proxy cannot really
    /// do). This is the BEST result the comparison can express - "no overhead this rig can detect" -
    /// and it must never be conflated with `NotMeasured`: one is a hole, the other is a win too small
    /// to weigh. Still absent rather than a clamped 0, because 0 would claim a precision we lack.
    BelowResolution,
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
            Absent::BelowResolution => "below_resolution",
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

/// A published reason, standing in for one absent metric on the wire. `Measurement<T>` itself still
/// serialises to the bare value or `null` (see below) so nothing that already reads a plain number
/// changes shape; this is the sibling record a container publishes ALONGSIDE its fields precisely so
/// the reason is not thrown away the moment the value is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbsentEntry {
    pub reason: Absent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl<T: 'static> Measurement<T> {
    /// If absent, inserts `key` -> this measurement's reason and detail into `out`. A no-op for a
    /// measured value: there is nothing to explain - with ONE exception.
    ///
    /// A `Measured` float that is NaN or infinite serialises as `null` (serde_json has no other way to
    /// write it), which on the wire is indistinguishable from an absence but arrived with no reason.
    /// Every null this artifact publishes is supposed to carry one, so a non-finite measured float is
    /// recorded as a `HarnessError`: an impossible number is the engine's fault by this project's own
    /// rule, and saying so beats a silent null.
    pub fn record_absence(
        &self,
        key: &str,
        out: &mut std::collections::BTreeMap<String, AbsentEntry>,
    ) {
        match self {
            Measurement::Absent { reason, detail } => {
                out.insert(
                    key.to_string(),
                    AbsentEntry {
                        reason: reason.clone(),
                        detail: detail.clone(),
                    },
                );
            }
            Measurement::Measured(v) => {
                if let Some(x) = (v as &dyn std::any::Any).downcast_ref::<f64>() {
                    if !x.is_finite() {
                        out.insert(
                            key.to_string(),
                            AbsentEntry {
                                reason: Absent::HarnessError,
                                detail: Some(format!(
                                    "the engine computed a non-finite value ({x}), which publishes as null"
                                )),
                            },
                        );
                    }
                }
            }
        }
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
    Absent {
        reason: Absent,
        detail: Option<String>,
    },
}

impl<T> Measurement<T> {
    pub fn absent(reason: Absent) -> Self {
        Measurement::Absent {
            reason,
            detail: None,
        }
    }

    /// An absence with operator-facing evidence attached. Use this whenever the reason alone would
    /// leave a reader asking "bounded by what?".
    pub fn absent_because(reason: Absent, detail: impl Into<String>) -> Self {
        Measurement::Absent {
            reason,
            detail: Some(detail.into()),
        }
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

    // NO `suppress` / `suppress_because`. They were documented as "how a value that was really taken,
    // but cannot honestly be published, stops being a number" and had no production caller: every real
    // suppression path (`suite::apply_peak_verdict` and its siblings) ASSIGNS
    // `absent_because(RigLimited, detail)` to the field instead. Their semantics could not have served
    // those sites even if wired - they preserve an existing absence, and those fields arrive already
    // absent carrying `NotMeasured`, so suppressing would have kept the weaker reason and thrown away
    // the rig-limited one. A tested helper standing beside the path it claims to be reads as coverage
    // of that path, which is worse than no helper at all.
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
        Measurement::Absent {
            reason: Absent::NotMeasured,
            detail: None,
        }
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
#[path = "tests/measurement_tests.rs"]
mod tests;
