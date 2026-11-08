use super::*;

#[test]
fn a_cell_id_renders_as_the_pairing_a_reader_greps_for() {
    assert_eq!(
        CellId::new("openai", "anthropic").to_string(),
        "openai>anthropic"
    );
}

// EVERY CONSTRUCTOR `run.rs` REALLY USES, pinned here so their shape stays covered without a
// second, parallel grid-walking loop.
//
// The list is derived from run.rs rather than remembered: it builds `CellOutcome` through
// `served`, `not_served`, `untestable`, `not_configurable` and `unprobed_auth` - five, not the
// three this comment used to claim. The two it forgot were also the two this test never touched,
// so a change that left `skipped` at `None` on either of them would have passed while the comment
// asserted they were covered. A comment claiming coverage is worth less than nothing when the
// coverage is not there, because it stops the next reader from checking.
#[test]
fn cell_outcome_constructors_carry_the_right_served_and_skipped_shape() {
    let id = CellId::new("openai", "anthropic");

    let served = CellOutcome::served(id.clone());
    assert!(served.served.is_measurable());
    assert_eq!(served.skipped, None);

    let ev = Evidence {
        status: 404,
        body_snippet: "not found".into(),
    };
    let ns = CellOutcome::not_served(id.clone(), Verdict::NotConfigured, ev.clone(), "declined");
    assert_eq!(ns.served, Served::No(Verdict::NotConfigured, ev.clone()));
    assert_eq!(ns.skipped, Some(Skipped::NotServed));
    assert_eq!(ns.note.as_deref(), Some("declined"));

    let ut = CellOutcome::untestable(id.clone(), "rig cannot pose this pairing");
    assert!(matches!(ut.served, Served::Untestable(_)));
    assert!(!ut.served.is_measurable());

    // The two the old comment forgot. Both are rig- or config-side outcomes, so both must carry a
    // `skipped` reason: a cell that reads as unmeasured with nothing saying why is the bare hole
    // this whole type exists to prevent.
    let nc = CellOutcome::not_configurable(id.clone(), "no base-url override");
    assert!(!nc.served.is_measurable());
    assert!(
        nc.skipped.is_some(),
        "a not-configurable cell must say why it was skipped, not merely that it was"
    );

    let ua = CellOutcome::unprobed_auth(id, ev.clone());
    assert!(!ua.served.is_measurable());
    assert!(
        ua.skipped.is_some(),
        "an unprobed-auth cell must say why it was skipped, not merely that it was"
    );
}
