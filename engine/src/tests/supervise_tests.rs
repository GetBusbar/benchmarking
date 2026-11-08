use super::*;
use std::cell::RefCell;
use std::net::TcpListener;

/// A scripted `Lifecycle`: `alive` is popped once per `is_alive` call (the last value repeats
/// once exhausted), and every call is counted so a test can assert exactly what happened rather
/// than only the final outcome.
struct FakeLifecycle {
    alive: RefCell<Vec<bool>>,
    stop_calls: RefCell<u32>,
    kill_calls: RefCell<u32>,
    is_alive_calls: RefCell<u32>,
}

impl FakeLifecycle {
    fn new(alive: Vec<bool>) -> Self {
        FakeLifecycle {
            alive: RefCell::new(alive),
            stop_calls: RefCell::new(0),
            kill_calls: RefCell::new(0),
            is_alive_calls: RefCell::new(0),
        }
    }
}

impl Lifecycle for FakeLifecycle {
    fn signal_stop(&self, _runtime: &Runtime) {
        *self.stop_calls.borrow_mut() += 1;
    }
    fn is_alive(&self, _runtime: &Runtime, _port: u16) -> bool {
        *self.is_alive_calls.borrow_mut() += 1;
        let mut a = self.alive.borrow_mut();
        if a.len() > 1 {
            a.remove(0)
        } else {
            *a.last().unwrap_or(&false)
        }
    }
    fn signal_kill(&self, _runtime: &Runtime) {
        *self.kill_calls.borrow_mut() += 1;
    }
}

fn native() -> Runtime {
    Runtime::Native {
        proc_match: "target/release/mock".into(),
    }
}

// ---- which processes a proc_match may name -----------------------------------------------------

fn entry(pid: u32, ppid: u32, cmdline: &str) -> ProcEntry {
    ProcEntry {
        pid,
        ppid,
        cmdline: cmdline.to_string(),
    }
}

/// A plausible box mid-run: an ssh session, the engine under it, the shell running a `commands`
/// line, the gateway itself, and a bystander. Every command line here contains the gateway's
/// `proc_match`, which is exactly what `pkill -f`/`pgrep -f` could not tell apart.
///
/// The ssh session's own command line (pid 100) genuinely carries the pattern here - some sshd
/// builds rewrite their process title to show the command the session is running, so `ps` shows
/// the child's invocation inside the parent's own line. That is deliberate: an earlier version of
/// this fixture gave pid 100 a line ("sshd: ubuntu@pts/0") that did NOT contain the pattern, so
/// the "an ancestor must never be signalled" assertion below passed because pid 100 failed the
/// substring filter first, never because `select_matches`'s ancestor walk excluded it. Deleting
/// that walk entirely left every test in this module green. With the pattern genuinely present
/// here, only the ancestor walk keeps pid 100 out of the result, so this fixture now exercises the
/// logic its doc claims to.
fn a_box_mid_run() -> Vec<ProcEntry> {
    vec![
        entry(1, 0, "/sbin/init"),
        entry(
            100,
            1,
            "sshd: ubuntu@pts/0 [otb run gateways/target/release/aisix 127.0.0.1:8000]",
        ),
        entry(
            200,
            100,
            "/usr/bin/otb run gateways/target/release/aisix 127.0.0.1:8000",
        ),
        entry(
            300,
            200,
            "/bin/sh -c curl -s localhost:8080/admin -d target/release/aisix",
        ),
        entry(400, 200, "target/release/aisix --config /etc/aisix.toml"),
        entry(500, 1, "grep -r target/release/aisix /home/ubuntu"),
    ]
}

// THE DEFECT: the engine's own argv names the gateway directory it was invoked with, so a
// substring match selected the harness. `signal_stop` then killed the run, and `is_alive` read
// the engine's own command line as proof the gateway was still up - so every `stop_and_wait`
// burned its whole budget and returned StillHeld, and every restart after it failed.
#[test]
fn the_harness_is_never_selected_by_the_gateways_own_proc_match() {
    let table = a_box_mid_run();
    let picked = select_matches(&table, "target/release/aisix", 200);
    assert!(
        !picked.contains(&200),
        "the engine selected itself for its own stop signal: {picked:?}"
    );
    assert!(
        !picked.contains(&100),
        "an ancestor of the engine (the ssh session) must never be signalled: {picked:?}"
    );
    assert!(
        picked.contains(&400),
        "the gateway itself must still be found: {picked:?}"
    );
}

// The shell running a `commands` line quotes the gateway's name without being the gateway.
#[test]
fn the_shell_running_a_commands_line_is_not_the_gateway() {
    let picked = select_matches(&a_box_mid_run(), "target/release/aisix", 200);
    assert!(
        !picked.contains(&300),
        "the /bin/sh running a setup line was selected as the gateway: {picked:?}"
    );
}

// A second engine on the same box looks exactly like this one. Killing it (or waiting for it) is
// a co-located run destroying its peer.
#[test]
fn a_second_engine_on_the_box_is_not_this_gateway() {
    let mut table = a_box_mid_run();
    table.push(entry(
        600,
        1,
        "/usr/bin/otb run gateways/target/release/aisix 127.0.0.1:9000",
    ));
    let picked = select_matches(&table, "target/release/aisix", 200);
    assert!(
        !picked.contains(&600),
        "another engine process was selected as this gateway: {picked:?}"
    );
}

// A bystander whose command line merely mentions the pattern is still selected, and that is the
// limit of what a substring match can promise - which is why `manifest::proc_match_problem`
// refuses an indistinct pattern at load time rather than pretending this layer can fix it.
#[test]
fn a_distinctive_pattern_selects_the_gateway_and_the_match_is_a_substring_not_a_regex() {
    let table = vec![entry(10, 1, "target/release/ai-gateway --port 8080")];
    assert_eq!(select_matches(&table, "target/release/ai-gateway", 1), [10]);
    // `.` is a literal here. Under `pgrep -f` it was a regex metacharacter matching any byte.
    assert!(select_matches(&table, "release.ai-gateway", 1).is_empty());
}

// AN EMPTY MATCH RESOLVES TO NOTHING. `pgrep -f ""` matches every process on the box, so an
// undeclared identity used to make init's tree a candidate for the gateway's memory, and made
// `is_alive` true forever.
#[test]
fn an_empty_proc_match_selects_no_process_rather_than_every_process() {
    let table = a_box_mid_run();
    assert!(select_matches(&table, "", 200).is_empty());
    assert!(select_matches(&table, "   ", 200).is_empty());
    // And against the real box, through the same entry point the memory reader uses.
    assert!(matching_pids("").is_empty());
}

#[test]
fn a_ps_line_parses_into_pid_parent_and_the_whole_command_line() {
    let e = parse_ps_line("  4242  1 /usr/bin/gw --flag a b").unwrap();
    assert_eq!(e.pid, 4242);
    assert_eq!(e.ppid, 1);
    assert_eq!(e.cmdline, "/usr/bin/gw --flag a b");
    assert!(parse_ps_line("not a process line").is_none());
}

// The real table on this box must at least contain this test process, or every match decision
// above is being made against nothing.
#[test]
fn the_real_process_table_can_be_read_and_contains_this_process() {
    let table = process_table();
    assert!(
        table.iter().any(|e| e.pid == std::process::id()),
        "the process table did not include the reader itself"
    );
}

fn counting_sleep(count: &RefCell<u32>) -> impl FnMut(Duration) + '_ {
    move |_| *count.borrow_mut() += 1
}

// ---- required: a process that exits immediately is waited for, no unnecessary sleeping -------
#[test]
fn already_stopped_is_reported_stopped_with_no_sleep() {
    let lc = FakeLifecycle::new(vec![false]);
    let sleeps = RefCell::new(0u32);
    let r = stop_and_wait_with(
        &lc,
        &native(),
        8081,
        Duration::from_secs(15),
        counting_sleep(&sleeps),
    );
    assert_eq!(r, Ok(()));
    assert_eq!(*lc.is_alive_calls.borrow(), 1);
    assert_eq!(
        *sleeps.borrow(),
        0,
        "a process already gone must not cost a single sleep"
    );
    assert_eq!(*lc.kill_calls.borrow(), 0, "nothing to escalate against");
    assert_eq!(
        *lc.stop_calls.borrow(),
        1,
        "the polite signal is still sent exactly once"
    );
}

// ---- required: a lingering process gets the SIGKILL escalation, asserted directly -------------
#[test]
fn a_lingering_process_is_escalated_past_the_halfway_mark() {
    // budget 10s, halfway = 5. Alive for attempts 0..=5 (six checks), gone on the 7th.
    let lc = FakeLifecycle::new(vec![true, true, true, true, true, true, false]);
    let sleeps = RefCell::new(0u32);
    let r = stop_and_wait_with(
        &lc,
        &native(),
        8081,
        Duration::from_secs(10),
        counting_sleep(&sleeps),
    );
    assert_eq!(r, Ok(()));
    assert_eq!(
        *lc.kill_calls.borrow(),
        1,
        "escalation must actually have fired, not just succeeded eventually"
    );
}

// ---- required: a process that never dies exhausts the budget and returns a LOUD error --------
#[test]
fn a_process_that_never_dies_is_a_hard_error_never_a_silent_pass() {
    let lc = FakeLifecycle::new(vec![true]); // always alive
    let sleeps = RefCell::new(0u32);
    let r = stop_and_wait_with(
        &lc,
        &native(),
        8081,
        Duration::from_secs(3),
        counting_sleep(&sleeps),
    );
    // `waited` is the time actually slept: the 3 s budget, in three one-second sleeps. It used to
    // report 4 s.
    assert_eq!(
        r,
        Err(SuperviseError::StillHeld {
            port: 8081,
            waited: Duration::from_secs(3)
        })
    );
    assert_eq!(*sleeps.borrow(), 3, "the budget is what was slept, no more");
    assert_eq!(
        *lc.kill_calls.borrow(),
        1,
        "the budget running out must still have tried to escalate"
    );
}

// ---- required: a zero budget still makes at least one attempt ---------------------------------
#[test]
fn a_zero_budget_still_makes_one_attempt() {
    let lc = FakeLifecycle::new(vec![true]);
    let sleeps = RefCell::new(0u32);
    let r = stop_and_wait_with(
        &lc,
        &native(),
        8081,
        Duration::from_secs(0),
        counting_sleep(&sleeps),
    );
    assert!(
        r.is_err(),
        "an unresponsive identity with no budget must still fail, never silently pass"
    );
    assert_eq!(
        *lc.is_alive_calls.borrow(),
        1,
        "exactly one attempt, not zero"
    );
    assert_eq!(
        *lc.kill_calls.borrow(),
        1,
        "halfway of a zero budget is zero, so escalation fires immediately"
    );
    assert_eq!(*sleeps.borrow(), 0, "a zero budget must not sleep");
}

// ---- required: a process that dies right at the halfway mark, escalation and death overlap ----
#[test]
fn dying_exactly_at_the_halfway_check_still_stops_cleanly() {
    // budget 4s, halfway = 2. Still alive AT the halfway check (attempt index 2), so the kill fires
    // there, and gone by the next one: escalation and death overlap at the boundary.
    let lc = FakeLifecycle::new(vec![true, true, true, false]);
    let sleeps = RefCell::new(0u32);
    let r = stop_and_wait_with(
        &lc,
        &native(),
        8081,
        Duration::from_secs(4),
        counting_sleep(&sleeps),
    );
    assert_eq!(r, Ok(()));
    assert_eq!(
        *lc.kill_calls.borrow(),
        1,
        "the escalation is due exactly at the halfway check, not before or after it"
    );
}

// A process that dies one check BEFORE the halfway mark is never escalated against.
#[test]
fn dying_before_the_halfway_check_is_never_escalated() {
    let lc = FakeLifecycle::new(vec![true, true, false]);
    let sleeps = RefCell::new(0u32);
    let r = stop_and_wait_with(
        &lc,
        &native(),
        8081,
        Duration::from_secs(4),
        counting_sleep(&sleeps),
    );
    assert_eq!(r, Ok(()));
    assert_eq!(*lc.kill_calls.borrow(), 0);
}

// AN UNKNOWN PORT IS NOT A FREE ONE. Reading the probe's `Unknown` as "nothing there" let a stop
// return `Ok` on its first poll when the process check could not see the survivor.
#[test]
fn only_a_port_confirmed_free_lets_a_stop_conclude() {
    assert!(!still_there(false, PortState::Free));
    assert!(still_there(false, PortState::Unknown));
    assert!(still_there(false, PortState::Held));
    assert!(still_there(true, PortState::Free));
}

// ---- required: readiness returns as soon as the port answers, no extra polling ----------------
#[test]
fn readiness_returns_as_soon_as_the_port_answers() {
    let calls = RefCell::new(0u32);
    let mut probe = |_p: u16| {
        *calls.borrow_mut() += 1;
        PortState::Held
    };
    let sleeps = RefCell::new(0u32);
    let r = wait_until_ready_with(
        8080,
        Duration::from_secs(30),
        &mut probe,
        counting_sleep(&sleeps),
    );
    assert_eq!(r, ReadyOutcome::Ready);
    assert_eq!(*calls.borrow(), 1);
    assert_eq!(*sleeps.borrow(), 0);
}

// ---- required: readiness reports Unmeasured, not NotReady, if it could never check -----------
#[test]
fn readiness_reports_unmeasured_when_it_could_never_check() {
    let mut probe = |_p: u16| PortState::Unknown;
    let sleeps = RefCell::new(0u32);
    let r = wait_until_ready_with(
        8080,
        Duration::from_secs(2),
        &mut probe,
        counting_sleep(&sleeps),
    );
    assert_eq!(r, ReadyOutcome::Unmeasured);
}

#[test]
fn readiness_reports_not_ready_when_the_port_was_confirmed_free_throughout() {
    let mut probe = |_p: u16| PortState::Free;
    let sleeps = RefCell::new(0u32);
    let r = wait_until_ready_with(
        8080,
        Duration::from_secs(2),
        &mut probe,
        counting_sleep(&sleeps),
    );
    assert_eq!(r, ReadyOutcome::NotReady);
}

// ---- required: port_state distinguishes held, free and could-not-determine --------------------
#[test]
fn port_state_reports_held_for_a_real_listener() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    assert_eq!(port_state(port), PortState::Held);
}

#[test]
fn port_state_reports_free_once_the_listener_is_dropped() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    assert_eq!(port_state(port), PortState::Free);
}

#[test]
fn port_state_reports_unknown_when_it_cannot_even_parse_the_target() {
    // Exercises the same branch port_state(port) can never reach (a u16 always parses), so the
    // probe is driven directly to prove Unknown exists and is distinct from the other two.
    assert_eq!(probe_addr("not-an-address"), PortState::Unknown);
}

#[test]
fn unknown_is_never_equal_to_held_or_free() {
    assert_ne!(PortState::Unknown, PortState::Held);
    assert_ne!(PortState::Unknown, PortState::Free);
}
