use super::*;

fn r(concurrency: u32, rps: f64, p99_ms: u64, fail: u64) -> Rung {
    Rung {
        concurrency,
        rps,
        p99_us: Some(p99_ms * 1_000),
        ok: 10_000,
        fail,
    }
}

/* ONE UNLUCKY WINDOW MUST NOT DISQUALIFY ITS WHOLE CONCURRENCY. Rungs are per-window, so this
filter used to name a boundary at a concurrency the gateway had demonstrably held - 36 of 456
published readings on the 2026-07-31 board did exactly that. The boundary is a reading's
PROOF that it stopped where it says; naming a concurrency that qualified inverts it. */
#[test]
fn a_concurrency_that_qualified_in_any_window_is_not_the_boundary() {
    let rungs = vec![
        r(64, 900.0, 4, 0),
        r(256, 850.0, 4, 0),
        r(256, 850.0, 4, 0),
        r(256, 840.0, 4, 2), // one dirty window at a concurrency that otherwise held
        r(1024, 800.0, 40, 0), // the real boundary: over the 5ms bound in every window
    ];
    let reading = read_at(&rungs, Some(5_000)).expect("a reading must resolve");
    assert_eq!(
        reading.first_disqualified_conc,
        Some(1024),
        "c=256 qualified in 2 of 3 windows, so it is not where the gateway stopped"
    );
}

/// And a concurrency that failed in EVERY window is still reported, or the fix has blinded the
/// proof entirely.
#[test]
fn a_concurrency_that_failed_every_window_is_still_the_boundary() {
    let rungs = vec![
        r(64, 900.0, 4, 0),
        r(256, 850.0, 40, 0),
        r(256, 850.0, 40, 0),
        r(256, 840.0, 40, 0),
    ];
    let reading = read_at(&rungs, Some(5_000)).expect("a reading must resolve");
    assert_eq!(reading.first_disqualified_conc, Some(256));
}

/// A sweep that never probed higher reports no boundary rather than an invented one.
#[test]
fn a_sweep_that_stopped_at_the_winner_names_no_boundary() {
    let rungs = vec![r(64, 900.0, 4, 0), r(64, 900.0, 4, 0)];
    let reading = read_at(&rungs, Some(5_000)).expect("a reading must resolve");
    assert_eq!(reading.first_disqualified_conc, None);
}
