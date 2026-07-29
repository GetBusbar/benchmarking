// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Process lifecycle for the things the harness starts and stops: the gateway under test and the
// mock upstream. Ported from lib/harness.sh's `mock_stop_wait` and `gw_stop_wait`.
//
// THE RULE THAT CARRIES THE MOST WEIGHT. A signal only asks a process to exit; it returns before
// the process is gone, so a fixed sleep-then-relaunch can bind the fresh process onto a port the
// old one still holds, panicking on EADDRINUSE. `gw_stop_wait` matters just as much as
// `mock_stop_wait` here: the gateway relaunch path is also used for the memory window, where a warm
// process masquerading as cold idle is its own, quieter corruption.
//
// So: signal, then POLL for actual release, escalating to SIGKILL at the halfway mark of the
// budget. A stop that cannot confirm release is a hard error, never a silent success, because a
// caller that proceeds anyway measures a port owned by the process it meant to replace.
//
// TESTABILITY. The polling and escalation logic is the thing worth testing, and it must be tested
// without a real process or a real socket. `Lifecycle` is the seam: it says "signal a stop", "is
// this identity still alive", and "kill it, this hard", and every polling decision is made against
// that trait alone. The syscall layer (`RealLifecycle`, shelling out to docker/pgrep/pkill, and the
// TCP connect probe in `port_state`) is kept thin on purpose, with nothing worth unit testing in it.
//
// A DELIBERATE CHANGE FROM THE SHELL: `mock_stop_wait`/`gw_stop_wait` read a global port variable
// (`MOCK_PORT`, `GW_PORT`) set by whichever suite sourced the harness. That is exactly the kind of
// separately-spelled name manifest.rs's `Runtime::identity()` exists to rule out. Here the caller
// passes the manifest's `Runtime` and its declared port explicitly, so there is no second, global
// place either could be spelled differently from the manifest.

use crate::manifest::Runtime;
use std::fmt;
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream};
use std::process::Command;
use std::time::Duration;

/// A cheap, dependency-light snapshot of whether anything holds a TCP port. Used purely as failure
/// evidence (for a stop-budget error, or a readiness report), never as the sole gate on its own:
/// `Unknown` must never be read as either `Held` or `Free` by a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortState {
    Held,
    Free,
    Unknown,
}

fn probe_addr(addr: &str) -> PortState {
    let sock: SocketAddr = match addr.parse() {
        Ok(s) => s,
        Err(_) => return PortState::Unknown,
    };
    match TcpStream::connect_timeout(&sock, Duration::from_millis(300)) {
        Ok(_) => PortState::Held,
        // A connection actively refused means nothing is listening: free. Any other outcome (a
        // filtered port, a timeout, a routing error) is a probe failure, not evidence of freedom.
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => PortState::Free,
        Err(_) => PortState::Unknown,
    }
}

/// Snapshot whether anything answers on `port` right now. Never blocks longer than the connect
/// timeout, and never panics on a probe failure: it reports `Unknown` instead.
pub fn port_state(port: u16) -> PortState {
    probe_addr(&format!("127.0.0.1:{port}"))
}

/// What a stop budget spent waiting produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuperviseError {
    /// The budget expired and the identity (process, or whatever still holds its port) was never
    /// confirmed gone. Loud on purpose: the caller must treat this as a hard failure, not proceed
    /// and measure whatever is still bound to the port.
    StillHeld { port: u16, waited: Duration },
}

impl fmt::Display for SuperviseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuperviseError::StillHeld { port, waited } => {
                write!(
                    f,
                    "port {port} still held after waiting {waited:?}; stop budget exhausted"
                )
            }
        }
    }
}

impl std::error::Error for SuperviseError {}

/// The seam between the polling/escalation logic and the operating system. `stop_and_wait` is
/// written entirely against this trait, so a test drives it with scripted answers instead of a real
/// process tree and a real socket.
pub trait Lifecycle {
    /// Ask the identity to stop. Best effort and asynchronous: this must return promptly, without
    /// waiting to see whether the request took effect. That waiting is `stop_and_wait`'s job.
    fn signal_stop(&self, runtime: &Runtime);

    /// Whether the identity is still alive: either its process still matches, or (for the shared
    /// case where a manifest may run docker, a wrapper, or a native binary) something still holds
    /// its declared port. Either signal alone is insufficient, matching `mock_stop_wait`'s original
    /// double check: a zombie holding the socket would pass a process-only test, and a container
    /// caught mid-stop can still hold the port after its process handle already looks gone.
    fn is_alive(&self, runtime: &Runtime, port: u16) -> bool;

    /// Escalate to a hard, unignorable kill (SIGKILL for a native match, a forced remove for a
    /// container). Called at most once per `stop_and_wait` call, at the halfway mark of the budget.
    fn signal_kill(&self, runtime: &Runtime);
}

/// The real syscall layer: shells out to docker/pgrep/pkill exactly as the shell harness did. Kept
/// thin deliberately; the logic worth testing lives in `stop_and_wait`, not here.
pub struct RealLifecycle;

impl Lifecycle for RealLifecycle {
    fn signal_stop(&self, runtime: &Runtime) {
        match runtime {
            // A container's stop is already synchronous (this is why the ten docker manifests were
            // never exposed to the shell bug); `rm -f` here is also this runtime's escalation, so
            // calling it as the first signal is not a shortcut, it is simply what "stop" means for
            // a container.
            Runtime::Docker { container } => {
                let _ = Command::new("docker")
                    .args(["rm", "-f", container])
                    .status();
            }
            Runtime::Native { proc_match } => {
                let _ = Command::new("pkill").args(["-f", proc_match]).status();
            }
        }
    }

    fn is_alive(&self, runtime: &Runtime, port: u16) -> bool {
        let process_alive = match runtime {
            Runtime::Docker { container } => Command::new("docker")
                .args(["inspect", "-f", "{{.State.Running}}", container])
                .output()
                .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
                .unwrap_or(false),
            Runtime::Native { proc_match } => Command::new("pgrep")
                .args(["-f", proc_match])
                .status()
                .map(|s| s.success())
                .unwrap_or(false),
        };
        process_alive || matches!(port_state(port), PortState::Held)
    }

    fn signal_kill(&self, runtime: &Runtime) {
        match runtime {
            Runtime::Docker { container } => {
                let _ = Command::new("docker")
                    .args(["rm", "-f", container])
                    .status();
            }
            Runtime::Native { proc_match } => {
                let _ = Command::new("pkill")
                    .args(["-9", "-f", proc_match])
                    .status();
            }
        }
    }
}

/// Signal a stop, then WAIT for the identity to actually go away, escalating to a hard kill at the
/// halfway mark of `budget`. Returns `Ok(())` only once `Lifecycle::is_alive` reports false; returns
/// `SuperviseError::StillHeld` if the budget runs out first. A budget of zero still makes one
/// attempt: it is not a way to skip the check, only a way to skip the waiting between attempts.
pub fn stop_and_wait(runtime: &Runtime, port: u16, budget: Duration) -> Result<(), SuperviseError> {
    stop_and_wait_with(&RealLifecycle, runtime, port, budget, std::thread::sleep)
}

/// The generic, testable core. `sleep` is injected so a test can assert "no unnecessary sleeping"
/// and run instantly rather than for real seconds; production calls this through [`stop_and_wait`],
/// which supplies `std::thread::sleep`.
pub fn stop_and_wait_with<L: Lifecycle>(
    lifecycle: &L,
    runtime: &Runtime,
    port: u16,
    budget: Duration,
    mut sleep: impl FnMut(Duration),
) -> Result<(), SuperviseError> {
    lifecycle.signal_stop(runtime);

    let budget_secs = budget.as_secs();
    let halfway = budget_secs / 2;
    let mut killed = false;
    let mut attempt: u64 = 0;

    loop {
        if !lifecycle.is_alive(runtime, port) {
            return Ok(());
        }
        // Escalate ONCE, at the halfway mark: past that point the polite signal has demonstrably
        // not worked. A zero-second budget has halfway == 0, so this fires on the very first (and
        // only) attempt rather than never firing at all.
        if !killed && attempt >= halfway {
            lifecycle.signal_kill(runtime);
            killed = true;
        }
        if attempt >= budget_secs {
            return Err(SuperviseError::StillHeld {
                port,
                waited: Duration::from_secs(attempt + 1),
            });
        }
        sleep(Duration::from_secs(1));
        attempt += 1;
    }
}

/// What `wait_until_ready` was able to establish within its budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyOutcome {
    /// Something answered on the port before the budget ran out.
    Ready,
    /// The budget ran out and the port was confirmed free (or bounced between free/unknown) the
    /// whole time: readiness genuinely failed, not merely unchecked.
    NotReady,
    /// The port state could not be determined even once during the whole budget. This is distinct
    /// from `NotReady`: it is a statement about the probe, not about the gateway, and must not be
    /// reported as either "ready" or "not ready".
    Unmeasured,
}

/// Poll until something is listening and answering on `port`, or until `budget` runs out. Returns
/// as soon as the port answers, with no extra sleeping past that point.
pub fn wait_until_ready(port: u16, budget: Duration) -> ReadyOutcome {
    wait_until_ready_with(port, budget, port_state, std::thread::sleep)
}

/// The generic, testable core, parameterised over the probe and the sleep so a test can script
/// port states directly instead of binding a real socket.
pub fn wait_until_ready_with(
    port: u16,
    budget: Duration,
    mut probe: impl FnMut(u16) -> PortState,
    mut sleep: impl FnMut(Duration),
) -> ReadyOutcome {
    let budget_secs = budget.as_secs();
    let mut ever_determined = false;
    let mut attempt: u64 = 0;

    loop {
        match probe(port) {
            PortState::Held => return ReadyOutcome::Ready,
            PortState::Free => ever_determined = true,
            PortState::Unknown => {}
        }
        if attempt >= budget_secs {
            return if ever_determined {
                ReadyOutcome::NotReady
            } else {
                ReadyOutcome::Unmeasured
            };
        }
        sleep(Duration::from_secs(1));
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            r,
            Err(SuperviseError::StillHeld {
                port: 8081,
                waited: Duration::from_secs(4)
            })
        );
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
        // budget 4s, halfway = 2. Gone by the third check (attempt index 2).
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
}
