// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// HOW CLOSE A MEASUREMENT CAME TO THE RIG'S OWN CEILING. A fact we publish, not a verdict we reach.
//
// This module used to answer "was the number bounded by the GATEWAY, or by our own test rig?" and act
// on the answer: a measurement at or above `BOUND_FRACTION = 0.9` of the reference was replaced with
// `null` and the reason `rig_limited`, on the argument that publishing it would rank the rig.
//
// THAT WAS THE WRONG TRADE, and it cost real data. The measurement in those cells was correct - the
// gateway really did carry that many requests or frames. What was uncertain was only what the number
// MEANT, and the engine resolved that uncertainty by deleting the number. On the 2026-07-28 board
// that discarded `streams_sustained` for 13 cells and `cpu_fps` for 11; agentgateway carried 12275
// frames/sec against a 12360 reference - keeping pace to within 0.7%, the best available outcome - and
// published nothing at all.
//
// The two constants were also the tell. `0.9` and `1.5` decided whether a real number reached the
// board, and neither was derived from anything: on the 2026-07-29 box a ratio of 1.71 was suppressed
// and 1.28 was published, and what separated them was not the gateway but whether one reference window
// happened to take 2.5s or 3.4s.
//
// So the judgement is gone and the EVIDENCE is published in its place. A cell now carries its measured
// value, the ceiling it was measured against, and `headroom` - the fraction of that ceiling it reached.
// 0.83 says "close to our rig's limit, weigh it accordingly"; 0.2 says "plainly the gateway". A reader
// gets the same fact the suppression was reacting to, and gets it without having the number taken away.
// Nothing here chooses a threshold, because nothing here reaches a conclusion.
//
// WHAT IS STILL AN ERROR, and where it moved to. An observation that EXCEEDS what the mock can
// physically emit is not a fast gateway and not a rig limit - it is a bug in this engine, and the only
// honest label for it is `HarnessError`. That check does not live here, because it cannot be done on a
// ratio: it needs the raw counts. `run::StreamWindow::engine_fault` states it exactly, against the
// mock's own declared frame budget, with no factor and no tolerance. See its doc for why an exact
// bound is available on content frames and not on the total event count.
//
// THE COMPARISON MUST STILL BE FAIR, for the reason it always had to be. A winner measured at c=740
// against a reference measured at c=2048 compares two different operating points, and the rig is not
// equally fast at every concurrency. So the reference is always taken AT THE OBSERVATION'S OWN
// CONCURRENCY - `suite::stream_rig_ceiling` derives it there from the mock's declared pacing, and
// `suite::rig_ceiling` measures it there for the throughput metrics. A headroom figure computed across
// two operating points would be a misleading fact rather than an honest one, which is worse than the
// verdict it replaced.

use crate::measurement::Measurement;

/// The fraction of the rig's ceiling this observation reached, or `None` when there is no usable
/// ceiling to state it against.
///
/// A FACT WITH NO THRESHOLD IN IT. This is the whole of what `is_rig_bound` used to compute a verdict
/// from, published raw so the reader draws the conclusion the engine has no standing to draw.
///
/// `None` when the reference is absent or non-positive, and that is a real answer rather than a
/// failure: a ceiling we could not establish gives no fraction, so the cell publishes its measured
/// value with no headroom beside it. It does NOT suppress the value, which is the entire change - an
/// unmeasurable reference used to make the gateway's own number disappear.
///
/// Values above 1.0 are returned as they are, deliberately. Two separately-timed legs scatter, and a
/// gateway that inserts its own SSE framing can carry more events per second than the mock's own
/// layout would - so 1.04 is ordinary and hiding it would hide the reader's best clue that the
/// reference is the soft number in the comparison. What is genuinely impossible is caught on the
/// counts by `run::StreamWindow::engine_fault`, not on this ratio.
pub fn headroom(observed: f64, reference: &Measurement<f64>) -> Option<f64> {
    match reference.copied() {
        Some(r) if r > 0.0 && r.is_finite() && observed.is_finite() => Some(observed / r),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::Absent;

    // The two field cases the old verdict split on. Both now yield their fraction and neither is
    // withheld: 0.954 and 0.482 are the same evidence the 0.9 threshold was reading, minus the
    // decision it made.
    #[test]
    fn the_fraction_is_reported_for_both_sides_of_the_old_threshold() {
        let near = headroom(334_838.0, &Measurement::Measured(351_088.0)).unwrap();
        assert!((near - 0.9537).abs() < 0.001, "{near}");
        let far = headroom(169_125.0, &Measurement::Measured(351_088.0)).unwrap();
        assert!((far - 0.4817).abs() < 0.001, "{far}");
    }

    // THE REGRESSION THIS MODULE EXISTS TO PREVENT. agentgateway's 12275 frames/sec against a 12360
    // reference is 99.3% - the best outcome a proxy of a paced mock can have - and it published
    // nothing, because 0.993 >= 0.9. There is no input to this function that suppresses anything, so
    // the only assertion available is that a near-ceiling observation still yields its fact.
    #[test]
    fn keeping_pace_with_the_mock_reports_its_ratio_rather_than_vanishing() {
        let h = headroom(12_275.0, &Measurement::Measured(12_360.0)).unwrap();
        assert!(h > 0.99 && h < 1.0, "{h}");
    }

    // An unusable reference costs the HEADROOM, never the measurement. The caller has the value in
    // hand and publishes it either way; this only says there is no fraction to state.
    #[test]
    fn an_unusable_reference_yields_no_fraction_and_nothing_else() {
        for r in [
            Measurement::absent(Absent::NotMeasured),
            Measurement::Measured(0.0),
            Measurement::Measured(-1.0),
            Measurement::Measured(f64::NAN),
            Measurement::Measured(f64::INFINITY),
        ] {
            assert_eq!(headroom(100.0, &r), None, "{r:?}");
        }
    }

    // Over the reference is REPORTED, not clamped and not rejected. These are the 2026-07-28 run's
    // own numbers, every one of which the old code read as the most confident possible "rig-bound"
    // (because `observed >= 0.9 * reference` is trivially true when observed is several times
    // reference) and suppressed on that basis.
    #[test]
    fn an_observation_over_the_reference_reports_its_ratio() {
        for (observed, reference, want) in [
            (192_671.0, 24_854.0, 7.75),
            (608.0, 49.0, 12.4),
            (673.0, 392.0, 1.72),
            (272.0, 98.0, 2.78),
        ] {
            let h = headroom(observed, &Measurement::Measured(reference)).unwrap();
            assert!(
                (h - want).abs() < 0.01,
                "{observed} over {reference} is {h}, not {want}"
            );
        }
    }

    // A non-finite observation has no honest fraction. It is also an engine fault, but that is
    // `engine_fault`'s finding on the counts - here it simply produces no number rather than a NaN
    // that would serialize into the artifact.
    #[test]
    fn a_non_finite_observation_yields_no_fraction() {
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(headroom(v, &Measurement::Measured(100.0)), None);
        }
    }
}
