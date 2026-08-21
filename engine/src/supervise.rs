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
// that trait alone. The syscall layer (`RealLifecycle`, shelling out to docker/ps/kill, and the
// TCP connect probe in `port_state`) is kept thin on purpose, with nothing worth unit testing in it -
// with ONE exception: `select_matches`, which decides which pids a `proc_match` names, is pure and
// tested here, because getting it wrong means signalling the harness instead of the gateway.
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

/// One live process, as the harness sees it. Enough to decide whether a command line that contains a
/// manifest's `proc_match` is the gateway, the harness itself, or a bystander.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: u32,
    pub ppid: u32,
    /// The FULL command line, argv joined by spaces, exactly what `pgrep -f` would have matched.
    pub cmdline: String,
}

/// Command names whose command line quotes somebody else's. The harness runs every `commands` line
/// as `/bin/sh -c "<line>"`, and those lines routinely name the gateway (a curl at its admin API, a
/// `docker exec` into it), so the shell's own argv contains the gateway's `proc_match` while the
/// shell is not the gateway. Signalling it would kill the harness's own setup step; counting it as
/// alive would make `stop_and_wait` wait for a process that is not the one being stopped. No gateway
/// in this field is started through a shell: `launch::build_invocation` execs `docker` or `taskset`
/// directly.
const WRAPPER_COMMANDS: [&str; 6] = ["sh", "bash", "dash", "zsh", "ash", "ksh"];

/// Every live process, read once. `ps` rather than `pgrep -f`: this way the SUBSTRING match a
/// manifest means is done here, on text the harness can also inspect for who owns it, instead of
/// inside `pgrep`, which matches a REGEX (a `proc_match` containing `.` or `+` silently means
/// something else there) and offers no way to exclude the harness from its own answer.
/// AN EMPTY TABLE AND AN ABSENT `ps` USED TO LOOK IDENTICAL, and they mean opposite things.
///
/// Both returned `Vec::new()`, so a box without `ps` on PATH reported "no process matched" for a
/// gateway that was running perfectly - the machine-as-untrusted-input class that already cost a full
/// gateway when `docker` was silently absent. There is no honest empty answer here: this host always
/// has processes, so an empty table means the QUESTION failed, not that the answer is none.
///
/// It still returns a Vec, because every caller reasonably treats "no match" as a fact about the
/// gateway and rewriting four call sites to thread a Result would spread the concern. What changed is
/// that the failure is no longer SILENT: it says which of the two happened, on stderr, where the box's
/// own fanout log captures it. A run that then reports "no running process found" has a line above it
/// naming the real cause.
pub fn process_table() -> Vec<ProcEntry> {
    let out = match Command::new("ps")
        .args(["-Ao", "pid=,ppid=,args="])
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            eprintln!(
                "supervise: could not run `ps` ({e}) - the process table is UNKNOWN, not empty. Any \
                 'no running process found' below is this failure, not a gateway that exited."
            );
            return Vec::new();
        }
    };
    if !out.status.success() {
        eprintln!(
            "supervise: `ps` exited {} - the process table is UNKNOWN, not empty. Any 'no running \
             process found' below is this failure, not a gateway that exited.",
            out.status
        );
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_ps_line)
        .collect()
}

fn parse_ps_line(line: &str) -> Option<ProcEntry> {
    let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
    let (ppid, cmdline) = rest.trim_start().split_once(char::is_whitespace)?;
    Some(ProcEntry {
        pid: pid.parse().ok()?,
        ppid: ppid.parse().ok()?,
        cmdline: cmdline.trim().to_string(),
    })
}

fn program_basename(cmdline: &str) -> &str {
    let argv0 = cmdline.split_whitespace().next().unwrap_or("");
    argv0.rsplit('/').next().unwrap_or(argv0)
}

/// Which pids a manifest's `proc_match` may legitimately name, given the whole process table.
///
/// PURE, AND THE ONLY PLACE THE MATCH RULE LIVES, because the rule is what makes the difference
/// between stopping the gateway and stopping the harness. `pkill -f <pattern>` matches a bare
/// substring against every full command line on the box, so three families of wrong process used to
/// qualify:
///
///  - THE HARNESS ITSELF. The engine's own argv names the gateway directory it was invoked with, so a
///    `proc_match` that is a substring of it made `signal_stop` kill the run, and made `is_alive`
///    report the gateway alive forever (it was reading the engine's own command line), so every
///    `stop_and_wait` spent its whole budget and returned `StillHeld` and every restart failed.
///  - THE SHELL RUNNING A `commands` LINE, which quotes the gateway's name without being it.
///  - A SECOND ENGINE on the same box, whose argv looks exactly like ours.
///
/// So: self and every ancestor of self are excluded, other instances of this same program are
/// excluded, and shell wrappers are excluded. What remains is a process whose own command line names
/// the pattern and which is not part of the measuring apparatus. An EMPTY pattern selects NOTHING,
/// never everything: "no identity was declared" must resolve to an absence, and `pgrep -f ""` matches
/// every process on the box including init.
pub fn select_matches(table: &[ProcEntry], pattern: &str, self_pid: u32) -> Vec<u32> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Vec::new();
    }
    let ppid_of: std::collections::BTreeMap<u32, u32> =
        table.iter().map(|e| (e.pid, e.ppid)).collect();

    // Self and its ancestors: the engine, the shell that started it, the ssh session above that.
    let mut apparatus = std::collections::BTreeSet::new();
    let mut walk = Some(self_pid);
    while let Some(pid) = walk {
        if !apparatus.insert(pid) {
            break; // a cycle in a reported ppid chain must not loop forever.
        }
        walk = ppid_of.get(&pid).copied().filter(|p| *p != 0);
    }

    let self_program = table
        .iter()
        .find(|e| e.pid == self_pid)
        .map(|e| program_basename(&e.cmdline).to_string());

    table
        .iter()
        .filter(|e| e.cmdline.contains(pattern))
        .filter(|e| !apparatus.contains(&e.pid))
        .filter(|e| !WRAPPER_COMMANDS.contains(&program_basename(&e.cmdline)))
        .filter(|e| self_program.as_deref() != Some(program_basename(&e.cmdline)))
        .map(|e| e.pid)
        .collect()
}

/// The pids a `proc_match` names on this box right now, harness excluded. Shared by the stop path
/// here and by `rss::RealPids`, so the process a memory reading is taken from is the same process
/// this file signals.
///
/// KNOWN, ACCEPTED TOCTOU: this snapshot is taken from `ps` and the pids are handed to `kill` a moment
/// later. Between the two, a matched gateway pid can exit and the kernel can recycle that pid number
/// onto an unrelated process, which would then receive the signal instead. The window is microseconds
/// wide and the box would have to be spawning many short-lived processes into exactly that gap, so the
/// residual risk is accepted rather than closed with a re-check that has its own (narrower) race;
/// noted explicitly because the failure mode - signalling the wrong process - is the rig-vs-gateway
/// mis-attribution class this repo treats as its core failure, and a silent race is worse than a
/// disclosed one.
pub fn matching_pids(pattern: &str) -> Vec<u32> {
    select_matches(&process_table(), pattern, std::process::id())
}

/// Signal an explicit list of pids. Never `pkill -f`: a pattern re-matched at signal time can select
/// a process the caller never inspected, and the caller has already decided which pids are the
/// gateway's.
fn signal_pids(signal: &str, pids: &[u32]) {
    if pids.is_empty() {
        return;
    }
    let mut cmd = Command::new("kill");
    cmd.arg(signal);
    for pid in pids {
        cmd.arg(pid.to_string());
    }
    let _ = cmd.status();
}

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

/// The real syscall layer: docker for a container, an explicit pid list for a native process. Kept
/// thin deliberately; the logic worth testing lives in `stop_and_wait` and `select_matches`.
pub struct RealLifecycle;

impl Lifecycle for RealLifecycle {
    fn signal_stop(&self, runtime: &Runtime) {
        match runtime {
            // A container's stop is already synchronous (this is why the ten docker manifests were
            // never exposed to the shell bug); `rm -f` here is also this runtime's escalation, so
            // calling it as the first signal is not a shortcut, it is simply what "stop" means for
            // a container.
            // The container this run started, under its run-scoped name (`Runtime::identity`), so a
            // concurrent run's container of the same gateway is not the one removed.
            Runtime::Docker { .. } => {
                let _ = Command::new("docker")
                    .args(["rm", "-f", &runtime.identity()])
                    .status();
            }
            // Resolved to explicit pids first: `pkill -f` would re-match the pattern against every
            // command line on the box, harness included.
            Runtime::Native { proc_match } => signal_pids("-TERM", &matching_pids(proc_match)),
        }
    }

    fn is_alive(&self, runtime: &Runtime, port: u16) -> bool {
        let process_alive = match runtime {
            Runtime::Docker { .. } => {
                match Command::new("docker")
                    .args(["inspect", "-f", "{{.State.Running}}", &runtime.identity()])
                    .output()
                {
                    // docker RAN and answered: a successful "true" is alive; a successful "false" or a
                    // non-zero exit ("No such object" after this run's own `rm -f`) is confirmed gone.
                    Ok(o) => {
                        o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true"
                    }
                    // docker COULD NOT BE INVOKED at all (exec error, ENOMEM spawning it): we did not
                    // determine the container's state. "Could not determine" must NOT collapse into
                    // "confirmed dead" the way it used to via `.unwrap_or(false)` - a false "gone" lets
                    // stop_and_wait return Ok prematurely and the retry loop start a fresh container on
                    // the same host-networked port a still-alive prior one may hold. Treat unknown as
                    // alive so the supervisor keeps waiting/escalating rather than racing a new start.
                    Err(_) => true,
                }
            }
            Runtime::Native { proc_match } => !matching_pids(proc_match).is_empty(),
        };
        process_alive || matches!(port_state(port), PortState::Held)
    }

    fn signal_kill(&self, runtime: &Runtime) {
        match runtime {
            Runtime::Docker { .. } => {
                let _ = Command::new("docker")
                    .args(["rm", "-f", &runtime.identity()])
                    .status();
            }
            Runtime::Native { proc_match } => signal_pids("-KILL", &matching_pids(proc_match)),
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
