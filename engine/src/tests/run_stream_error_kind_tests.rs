use super::*;
use crate::http::{SseEnd, SseOutcome};

fn outcome(status: Option<u16>, frames: usize, end: SseEnd) -> SseOutcome {
    SseOutcome {
        status,
        frames: vec!["x".into(); frames],
        frame_offsets_us: vec![1; frames],
        content_frames: frames as u64,
        end,
    }
}

/* THE WHOLE POINT IS ATTRIBUTION, so the ordering inside `add` is load-bearing rather than
stylistic. A refused connection has no status and no frames; if frames were tested first,
every refused connection would be filed as "no frames" and the one distinction this type
exists to draw - the peer declining a connection vs the peer answering badly - would be
erased in exactly the case we need it. */
#[test]
fn each_error_is_filed_under_the_thing_that_actually_went_wrong() {
    let mut k = StreamErrorKinds::default();
    k.add(&outcome(
        None,
        0,
        SseEnd::ConnectionFailed("refused".into()),
    ));
    assert_eq!(
        (k.connect_failed, k.no_frames),
        (1, 0),
        "a refused connection must not be filed as 'answered 2xx but sent nothing'"
    );

    // A peer that took the connection and never answered has no status: it did not answer with
    // an error, it did not answer at all, and filing it under `status` read as an HTTP error.
    for end in [SseEnd::Timeout, SseEnd::StreamClosed] {
        let mut k = StreamErrorKinds::default();
        k.add(&outcome(None, 0, end));
        assert_eq!(
            (k.no_response, k.status, k.connect_failed),
            (1, 0, 0),
            "no response head is not a non-2xx answer"
        );
    }

    let mut k = StreamErrorKinds::default();
    k.add(&outcome(Some(503), 0, SseEnd::StreamClosed));
    assert_eq!(
        (k.status, k.no_frames),
        (1, 0),
        "a non-2xx is a status error, not a frame famine"
    );

    let mut k = StreamErrorKinds::default();
    k.add(&outcome(Some(200), 0, SseEnd::StreamClosed));
    assert_eq!(
        k.no_frames, 1,
        "a 2xx that delivered nothing is its own finding"
    );

    let mut k = StreamErrorKinds::default();
    k.add(&outcome(
        Some(200),
        3,
        SseEnd::NotAnEventStream("text/html".into()),
    ));
    assert_eq!(
        k.not_event_stream, 1,
        "a 2xx that is not SSE is a protocol error"
    );

    let mut k = StreamErrorKinds::default();
    k.add(&outcome(
        Some(200),
        3,
        SseEnd::Malformed("bad frame".into()),
    ));
    assert_eq!(
        k.not_event_stream, 1,
        "malformed shares the protocol-error class"
    );
}

/// The breakdown must account for every error, or a reader subtracting the parts from the total
/// finds a remainder with no name and learns nothing.
#[test]
fn the_kinds_sum_to_the_errored_count() {
    let outcomes = [
        outcome(None, 0, SseEnd::ConnectionFailed("reset".into())),
        outcome(Some(502), 0, SseEnd::StreamClosed),
        outcome(None, 0, SseEnd::Timeout),
        outcome(Some(200), 0, SseEnd::Timeout),
        outcome(Some(200), 2, SseEnd::Malformed("x".into())),
    ];
    let mut k = StreamErrorKinds::default();
    let mut errored = 0u64;
    for o in &outcomes {
        if stream_errored(o) {
            errored += 1;
            k.add(o);
        }
    }
    assert_eq!(
        k.total(),
        errored,
        "every counted error must land in exactly one class"
    );
    assert_eq!(
        errored, 5,
        "this fixture must actually produce errors to be testing anything"
    );
}

/// Rig-side ends never reach the classifier - `stream_window` discards the whole window - so they
/// must not be classifiable as a gateway error even if one leaked through.
#[test]
fn rig_side_ends_are_not_gateway_errors() {
    for end in [
        SseEnd::RigExhausted("EADDRNOTAVAIL".into()),
        SseEnd::RigRefused("no credential".into()),
    ] {
        assert!(
            !stream_errored(&outcome(None, 0, end)),
            "the rig running out is never the gateway's error rate"
        );
    }
}
