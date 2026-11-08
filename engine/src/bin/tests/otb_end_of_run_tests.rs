use super::{end_of_run_stop_failure, stale_minted_auth_refusal};
use otb_engine::supervise::SuperviseError;
use std::time::Duration;

// THE DEFECT: the final stop's result was dropped with `let _`, so a gateway that outlived the
// 15s budget kept the port and the cores, and the only report of it was the NEXT gateway on the
// box failing to boot - attributed to that gateway rather than to this one.
#[test]
fn a_gateway_that_survives_the_stop_budget_is_reported_loudly() {
    let stop = Err(SuperviseError::StillHeld {
        port: 8080,
        waited: Duration::from_secs(15),
    });
    let said = end_of_run_stop_failure("gw-bench-4242", &stop).expect("a survivor is a failure");
    assert!(
        said.contains("gw-bench-4242"),
        "the message must name what is still running: {said}"
    );
    assert!(
        said.contains("8080"),
        "and the port it is still holding: {said}"
    );
    assert!(
        said.contains("NEXT gateway"),
        "and what it will break next: {said}"
    );
}

#[test]
fn a_gateway_that_actually_stopped_reports_nothing() {
    assert_eq!(end_of_run_stop_failure("gw-bench-4242", &Ok(())), None);
}

// THE DEFECT: minted auth is resolved once, after the initial launch, and the memory phase then
// restarts the gateway and replays `commands`. A gateway that mints per boot would hand out a new
// credential there while every later request kept using the first one, and the whole second half
// of the grid would read as the gateway refusing its own traffic.
#[test]
fn a_minting_gateway_the_harness_restarts_is_refused_before_it_is_measured() {
    let why = stale_minted_auth_refusal(Some("sk-minted-1"), true)
        .expect("this combination cannot be measured honestly");
    assert!(
        why.contains("stale"),
        "the refusal must name the defect: {why}"
    );
}

#[test]
fn a_gateway_that_mints_nothing_is_measured_as_before() {
    assert_eq!(stale_minted_auth_refusal(None, true), None);
}

// Someone else's gateway, someone else's lifetime: nothing here restarts it, so the credential it
// minted at ITS boot stays valid for the whole run.
#[test]
fn a_minting_gateway_the_harness_never_restarts_is_fine() {
    assert_eq!(stale_minted_auth_refusal(Some("sk-minted-1"), false), None);
}
