use super::*;

/// The step-down rule `sweep_streams_cell` applies, called directly rather than mirrored here.
fn next_rung(proven_clean: u32, lo: u32, ceiling: u32) -> u32 {
    stepdown_next(proven_clean, lo, ceiling)
}

/* THE DEFECT THIS REPLACES, ON THE REAL NUMBERS. busbar proved every rung to 4,096, bisected to
6,176, could not confirm it, and halved to 3,088 - below a concurrency it had already carried
cleanly, discarding the whole bracket. A later run confirmed the true ceiling at 5,120, which
is inside the bracket halving threw away and nowhere near 3,088. */
#[test]
fn the_step_down_stays_inside_the_bracket_the_search_paid_for() {
    let next = next_rung(4096, 1, 6176);
    assert!(
        next > 4096,
        "must not step below a rung this cell already carried cleanly, got {next}"
    );
    assert!(
        next < 6176,
        "must step below the rung that failed confirmation, got {next}"
    );
    assert_eq!(next, 5136, "bisects the bracket");
    assert_ne!(next, 3088, "the old halving answer must not survive");
}

/// Halving is still right when there is NO bracket - confirmation failed at the known-good rung
/// itself, so nothing below it is known and there is nothing to bisect.
#[test]
fn halving_survives_only_where_there_is_no_information_to_bisect() {
    assert_eq!(
        next_rung(8192, 1, 8192),
        4096,
        "no bracket: fall back to halving"
    );
    assert_eq!(
        next_rung(0, 1, 1024),
        512,
        "nothing proven clean: bisect from the search floor"
    );
}

/// The search must always make progress downward, or it loops until the step-down budget runs
/// out and publishes nothing - the exact failure this whole change exists to remove.
#[test]
fn every_step_makes_downward_progress() {
    for (pc, lo, ceil) in [
        (4096u32, 1u32, 6176u32),
        (100, 1, 101),
        (2, 1, 3),
        (1, 1, 2),
        (0, 1, 2),
    ] {
        let n = next_rung(pc, lo, ceil);
        assert!(
            n < ceil,
            "step-down from {ceil} (proven {pc}) must go DOWN, got {n}"
        );
    }
}
