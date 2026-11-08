// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// STARTING A GATEWAY FROM ITS MANIFEST, with the boot discipline lib/harness.sh already earned the
// hard way.
//
// TWO RULES CARRY THIS FILE.
//
// First, a launch that never becomes ready is a hard, typed error, never a "launched" value the
// caller could mistake for success. `harness_launch_ready` exists because a gateway that loses a
// port race or is merely slow to bind must not fail the whole cell on the first try; it retries a
// bounded number of times (`HARNESS_BOOT_ATTEMPTS`, default 3), kills the partial process between
// attempts, and only after every attempt fails does it report failure, with the evidence (attempt
// count, last port state) attached rather than a bare "no". `launch` below is that same shape typed:
// `Result<Launched, LaunchError>` with no way to construct a `Launched` except by actually observing
// readiness.
//
// Second, `LaunchSpec::runtime` is a `manifest::Runtime`, the SAME identity `Runtime::identity()`
// gives the memory readers and `supervise::stop_and_wait`. A docker launch's `--name` and a native
// launch's process match are read from this one field, never restated as a second string on the
// spec, so there is no way for the thing this file starts to differ from the thing the readers
// measure and the stop path targets (see `manifest.rs`'s module header for why that matters).
//
// PINNING IS NOT OPTIONAL. The comparability basis of the whole benchmark is "same box, same load,
// one gateway at a time, same cores"; an unpinned gateway is measured on different hardware than a
// pinned one. `LaunchSpec::validate` refuses to build an invocation for a spec with no CPU list.
//
// THE ESCAPE HATCH. Most manifests are pure data: an image or a binary, env, a port, cores. A few
// genuinely need one imperative step first (building a binary from source, rendering a config file a
// static mount cannot express). `PreLaunchStep` is that step, modelled as an explicit command plus
// its arguments, never a shell string: it is a field a reader can see on the spec ("this entrant runs
// a pre-launch step"), it runs once before the retry loop with its own timeout, and a failure aborts
// the launch loudly before anything is spawned. There is deliberately no field that takes an
// arbitrary script; that is exactly how a declarative model erodes back into shell.
//
// TESTABILITY. Argument construction (`build_invocation`) and the retry/readiness loop (`launch_with`)
// are pure functions over a `Launcher` trait; the actual `std::process::Command` spawn
// (`RealLauncher`) is a thin wrapper with nothing worth unit testing in it. Tests drive a fake
// `Launcher` with scripted outcomes, exactly as `supervise.rs` drives a fake `Lifecycle`.

use crate::manifest::Runtime;
use crate::supervise::{self, PortState, ReadyOutcome};
use std::fmt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Boot attempts before a launch gives up. Matches the shell's `HARNESS_BOOT_ATTEMPTS` default: a
/// gateway that loses a port race or is slow to bind gets more than one chance before the cell is
/// recorded as failed.
pub const DEFAULT_LAUNCH_ATTEMPTS: u32 = 3;

/// A file to bind-mount into a container, read-only or read-write. The declarative alternative to a
/// manifest imperatively writing a config file and then `docker cp`-ing it in: the config bytes are
/// rendered by the caller (as plain data) and handed to the container over this mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub host_path: String,
    pub container_path: String,
    pub read_only: bool,
}

/// How a container's declared port reaches the host. `Host` is what every current docker manifest in
/// this field actually uses (`--network host`, the gateway binds its own configured port directly);
/// `Published` is kept so the type does not silently assume host networking is the only mode a
/// manifest could ever need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortMapping {
    Host,
    Published { host: u16, container: u16 },
}

/// What to run and how. The kind-specific data only; identity (the container name or the process
/// match) and the CPU pin live on `LaunchSpec` itself, once, for the reason the module header
/// explains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchKind {
    Docker {
        image: String,
        env: Vec<(String, String)>,
        port: PortMapping,
        mounts: Vec<Mount>,
        /// Arguments appended after the image (a container's entrypoint args, e.g. `-f
        /// /config.yaml`). Empty for an image that needs none.
        command: Vec<String>,
    },
    Native {
        /// The executable to run. Distinct from `LaunchSpec::runtime`'s `proc_match`: the match is a
        /// pattern used to FIND the process afterward (a substring is enough, and is all `pgrep -f`
        /// needs), while this is the exact path handed to the shell to start it.
        binary: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        /// Names REMOVED from the inherited environment before the process starts.
        ///
        /// An env block can only ADD, and `std::process::Command` inherits the parent environment, so
        /// without this there is no way to express "this must not be set". One entrant needs it: its
        /// config loader claims every variable sharing its prefix and rejects unknown fields, so the
        /// harness's own override variables kill config load before the port binds - and because the
        /// binary is backgrounded, the launch reports success and the only symptom is a port that
        /// never listens.
        env_unset: Vec<String>,
    },
}

/// The smallest possible escape hatch, explicitly marked. A manifest that cannot be expressed as
/// pure launch data (a source build, a config render that needs more than a static file) declares
/// one of these; it runs once, before the retry loop, and its failure is loud and immediate. This is
/// deliberately NOT "run this shell string": the command and its arguments are typed fields, so a
/// reader can see exactly which entrants use the hatch and what they run, by grepping the manifests
/// for this type rather than reading a script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreLaunchStep {
    pub command: String,
    pub args: Vec<String>,
    pub timeout: Duration,
}

/// Everything needed to start one gateway attempt and to know whether it came up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    /// The declared identity: the SAME `Runtime` the manifest carries, that the memory readers and
    /// the stop path (`supervise::stop_and_wait`) take. There is no second name for this launch to
    /// disagree with.
    pub runtime: Runtime,
    pub kind: LaunchKind,
    /// CPU list (e.g. `"0-3"`) passed to `--cpuset-cpus` or `taskset -c`. Never optional in practice:
    /// `validate` rejects an empty value, because an unpinned gateway is measured on different
    /// hardware than a pinned one and the whole board's comparability rests on this.
    pub cores: String,
    /// The port readiness is judged on; the same port the manifest declares and the harness drives.
    pub port: u16,
    /// How long a single attempt waits for the port to answer before that attempt counts as failed.
    pub ready_budget: Duration,
    /// Backoff before the next attempt, grown by attempt number (`boot_backoff * attempt`), matching
    /// the shell's `HARNESS_BOOT_BACKOFF_S * attempt`.
    pub boot_backoff: Duration,
    /// The escape hatch. `None` for every manifest that needs nothing beyond its declared launch
    /// data.
    pub pre_launch: Option<PreLaunchStep>,
}

/// Why a `LaunchSpec` cannot be launched at all, independent of whether the gateway ever answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecError {
    /// No CPU list was declared. Pinning is not optional: an unpinned launch would be measured on
    /// different hardware than every pinned one, which breaks the comparison this whole benchmark
    /// exists to make.
    NoCpuPinning,
    Empty(&'static str),
    BadPort,
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpecError::NoCpuPinning => write!(f, "a launch spec must declare cpu pinning"),
            SpecError::Empty(field) => write!(f, "{field} must not be empty"),
            SpecError::BadPort => write!(f, "port must be non-zero"),
        }
    }
}

impl std::error::Error for SpecError {}

impl LaunchSpec {
    /// Refuse to launch a spec that is missing what a launch needs. Called by `launch_with` before
    /// anything is spawned, so an invalid spec never reaches `Launcher::spawn` at all.
    pub fn validate(&self) -> Result<(), SpecError> {
        if self.cores.trim().is_empty() {
            return Err(SpecError::NoCpuPinning);
        }
        if self.runtime.identity().trim().is_empty() {
            return Err(SpecError::Empty("runtime identity"));
        }
        if self.port == 0 {
            return Err(SpecError::BadPort);
        }
        match &self.kind {
            LaunchKind::Docker { image, .. } if image.trim().is_empty() => {
                return Err(SpecError::Empty("image"));
            }
            LaunchKind::Native { binary, .. } if binary.trim().is_empty() => {
                return Err(SpecError::Empty("binary"));
            }
            _ => {}
        }
        Ok(())
    }
}

/// The literal program and argument list a `LaunchSpec` becomes. Pure: no filesystem, no process,
/// so argument construction is testable without anything actually running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    /// Environment variables applied to the spawned process itself. Empty for a docker invocation:
    /// its env travels as `-e KEY=VALUE` arguments instead, exactly like the env a container image
    /// actually receives.
    pub env: Vec<(String, String)>,
    /// Variables removed from the inherited environment before the process starts. Empty for docker,
    /// which inherits nothing from this process in the first place.
    pub env_unset: Vec<String>,
}

/// Build the command a `LaunchSpec` runs, with no side effects. `spec.runtime.identity()` is the
/// ONLY source of a docker container's `--name`: there is no separate name field on `LaunchKind`, so
/// this invocation and `supervise::stop_and_wait`'s target cannot name two different things.
pub fn build_invocation(spec: &LaunchSpec) -> Invocation {
    match &spec.kind {
        LaunchKind::Docker {
            image,
            env,
            port,
            mounts,
            command,
        } => {
            let name = spec.runtime.identity();
            let mut args = vec![
                "run".to_string(),
                "-d".to_string(),
                "--name".to_string(),
                name,
            ];
            // LABELS SO A SWEEP CAN FIND THIS RUN'S CONTAINERS WITHOUT GUESSING AT NAMES. The name
            // is run-scoped precisely so one run cannot remove another's container, which leaves a
            // question the name alone no longer answers: which containers belong to this run, and
            // which gateway is any given one. `docker ps --filter label=otb.run=<id>` answers the
            // first and `label=otb.gateway=<name>` the second, the same way run-on-ec2.sh's teardown
            // filters boxes on `tag:run` rather than parsing instance names.
            args.push("--label".to_string());
            args.push(format!("otb.gateway={}", spec.runtime.declared_identity()));
            if let Some(scope) = spec.runtime.run_scope() {
                args.push("--label".to_string());
                args.push(format!("otb.run={scope}"));
            }
            match port {
                PortMapping::Host => {
                    args.push("--network".to_string());
                    args.push("host".to_string());
                }
                PortMapping::Published { host, container } => {
                    args.push("-p".to_string());
                    args.push(format!("{host}:{container}"));
                }
            }
            args.push("--cpuset-cpus".to_string());
            args.push(spec.cores.clone());
            for (k, v) in env {
                args.push("-e".to_string());
                args.push(format!("{k}={v}"));
            }
            for m in mounts {
                args.push("-v".to_string());
                let mode = if m.read_only { "ro" } else { "rw" };
                args.push(format!("{}:{}:{}", m.host_path, m.container_path, mode));
            }
            args.push(image.clone());
            args.extend(command.iter().cloned());
            Invocation {
                program: "docker".to_string(),
                args,
                env: Vec::new(),
                env_unset: Vec::new(),
            }
        }
        LaunchKind::Native {
            binary,
            args: bin_args,
            env,
            env_unset,
        } => {
            let mut args = vec!["-c".to_string(), spec.cores.clone(), binary.clone()];
            args.extend(bin_args.iter().cloned());
            Invocation {
                program: "taskset".to_string(),
                args,
                env: env.clone(),
                env_unset: env_unset.clone(),
            }
        }
    }
}

/// A launch the retry loop actually confirmed ready. `launch`/`launch_with` are the only functions in
/// this crate that build one, and they build it only after `Launcher::is_ready` reported true. The
/// fields are public (callers read `attempts`), so the type itself cannot stop other code from
/// constructing one: treat a value as evidence only when it came from one of those two functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launched {
    pub runtime: Runtime,
    /// How many attempts it took, 1 if the very first attempt came up.
    pub attempts: u32,
}

/// Why a launch failed. Every variant carries the evidence a caller needs to act on it, never a bare
/// "no".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// The spec itself was not launchable (missing cpu pinning, an empty identity, ...); nothing was
    /// spawned.
    InvalidSpec(SpecError),
    /// The escape hatch's pre-launch command failed or timed out. Loud, and the gateway itself was
    /// never spawned: `command` names exactly which typed step failed, `reason` carries why.
    PreLaunchFailed { command: String, reason: String },
    /// Every attempt was spawned and none became ready. `attempts` is how many were made (bounded,
    /// never unbounded); `last_port_state` is the evidence from the final attempt, so a caller can
    /// tell "nothing ever listened" from "something was listening but never answered readiness".
    NeverReady {
        attempts: u32,
        last_port_state: PortState,
        /// What the last attempt's spawn said, when it said anything. A container runtime that
        /// REFUSES the invocation - a rejected mount, a name in use, a cpuset it will not honour -
        /// reports it here and nowhere else, and without it every such refusal is indistinguishable
        /// from a gateway that started and never bound its port. An absent measurement is supposed to
        /// publish a reason; "never became ready" with nothing behind it is barely one.
        last_spawn_error: Option<String>,
        /// WHAT THE GATEWAY ITSELF SAID before it died. A container that starts, rejects its config
        /// and exits has already explained the failure in its own log, and that log was destroyed a
        /// line later by the `stop()` between attempts, which for docker is `rm -f`. The failure then
        /// reads as "never became ready; port state: Free" for a config typo, a bad mount and an
        /// unsupported flag alike. Captured BEFORE the teardown that removes it.
        last_output: Option<String>,
    },
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::InvalidSpec(e) => write!(f, "launch spec invalid: {e}"),
            LaunchError::PreLaunchFailed { command, reason } => {
                write!(f, "pre-launch step {command:?} failed: {reason}")
            }
            LaunchError::NeverReady {
                attempts,
                last_port_state,
                last_spawn_error,
                last_output,
            } => {
                let said = match last_output {
                    Some(o) if !o.trim().is_empty() => format!("; it said: {}", o.trim()),
                    _ => String::new(),
                };
                match last_spawn_error {
                Some(why) => write!(
                    f,
                    "never became ready after {attempts} attempt(s); last port state: {last_port_state:?}; the runtime refused the launch: {why}{said}"
                ),
                None => write!(f, "never became ready after {attempts} attempt(s); last port state: {last_port_state:?}{said}"),
            }
            }
        }
    }
}

impl std::error::Error for LaunchError {}

/// The seam between the retry/readiness logic and the operating system. `launch_with` is written
/// entirely against this trait, so a test drives it with scripted outcomes instead of a real process
/// and a real socket, exactly as `supervise::Lifecycle` does for the stop path.
pub trait Launcher {
    /// Run the escape hatch's pre-launch command, once, before any spawn attempt. `Ok(())` if it
    /// exited zero within its timeout; `Err(reason)` otherwise (non-zero exit, spawn failure, or a
    /// timeout), which aborts the whole launch before anything is started.
    fn run_pre_launch(&mut self, step: &PreLaunchStep) -> Result<(), String>;

    /// Start one attempt. `Ok(())` only means the process/container was successfully started, never
    /// that it is ready: readiness is `is_ready`'s job, kept separate so a spawn failure and a
    /// readiness failure are distinguishable in a test.
    fn spawn(&mut self, spec: &LaunchSpec) -> Result<(), String>;

    /// Whether this attempt came up within its own bounded wait. Must not block past
    /// `spec.ready_budget`.
    fn is_ready(&mut self, spec: &LaunchSpec) -> bool;

    /// Kill whatever a failed attempt left behind, so the next attempt is not racing a half-bound
    /// listener. Best effort: a stop that cannot confirm release still lets the loop retry, and what
    /// catches a lingering process is `launch_with`'s check that the port is NOT held before the next
    /// spawn. The readiness check cannot: it asks only whether something answers on the port, so a
    /// survivor would satisfy it.
    fn stop(&mut self, spec: &LaunchSpec);

    /// A snapshot of the port's state, used only as failure evidence on the final `NeverReady`.
    fn port_snapshot(&mut self, spec: &LaunchSpec) -> PortState;

    /// The gateway's OWN output from the attempt that just failed, read before it is torn down.
    /// Defaulted to nothing so a test launcher need not implement it; the real one reads the
    /// container log.
    fn diagnostics(&mut self, _spec: &LaunchSpec) -> Option<String> {
        None
    }
}

/// THE DIRECTORY A `commands` LINE'S RELATIVE PATHS RESOLVE AGAINST.
///
/// A commands line is transcribed from the gateway's own documentation, and such a line writes and
/// reads relative paths (`> .minted-auth`, `-f config.yaml`). Run with the engine's inherited cwd,
/// that artifact lands wherever the operator happened to launch `otb` from, which is not where
/// anything looks for it: `resolve_minted_auth` reads `<gw_dir>/.minted-auth`, so a credential minted
/// into the wrong directory is silently not found and the run authenticates with the manifest's
/// placeholder.
///
/// A PROCESS-WIDE VALUE, SET ONCE, and that is the deliberate part. The commands are replayed by
/// `run::restart_to_rest` on every memory-phase restart, and that call site has no gateway directory
/// to pass (its `RunConfig` carries the relaunch spec and the command lines, not the directory). A
/// second spelling of the directory - one for the initial launch, one for the replay - is exactly the
/// drift `manifest.rs`'s single-identity rule exists to rule out, so there is one, set by the binary
/// before the first command runs and never changed after.
static COMMANDS_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Declare the directory every `commands` line runs in. Set once, by the binary, before any command
/// runs; a second call is ignored, so a later caller cannot quietly move the ground under a replay.
pub fn set_commands_dir(dir: std::path::PathBuf) {
    let _ = COMMANDS_DIR.set(dir);
}

/// Where `run_line` will run. `None` until the binary declares one, which keeps the library's
/// behaviour explicit rather than inventing a directory of its own.
pub fn commands_dir() -> Option<&'static std::path::Path> {
    COMMANDS_DIR.get().map(std::path::PathBuf::as_path)
}

/// Run ONE line from a gateway's `commands` file, through a shell, with a hard timeout, in the
/// directory `set_commands_dir` declared.
///
/// A shell because these lines are what a gateway's own documentation tells an operator to type,
/// and those instructions use pipes, quoting and redirection. The alternative is a bespoke argv
/// encoding that every author would have to learn in order to transcribe a documented curl.
pub fn run_line(line: &str, timeout: Duration) -> Result<(), String> {
    run_line_in(commands_dir(), line, timeout)
}

/// The same, with the directory passed explicitly. `None` means the inherited cwd, which is only
/// correct for a line that touches no relative path at all.
pub fn run_line_in(
    dir: Option<&std::path::Path>,
    line: &str,
    timeout: Duration,
) -> Result<(), String> {
    run_with_timeout_in(
        dir,
        "/bin/sh",
        &["-c".to_string(), line.to_string()],
        timeout,
    )
}

/// Run a pre-launch command with its own hard timeout, using only `std::process`: poll
/// `Child::try_wait` rather than blocking on `wait`, so a hung command is killed instead of hanging
/// this call forever.
fn run_with_timeout(command: &str, args: &[String], timeout: Duration) -> Result<(), String> {
    run_with_timeout_in(None, command, args, timeout)
}

fn run_with_timeout_in(
    dir: Option<&std::path::Path>,
    command: &str,
    args: &[String],
    timeout: Duration,
) -> Result<(), String> {
    let mut cmd = Command::new(command);
    cmd.args(args).stdin(Stdio::null());
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().map_err(|e| format!("failed to start: {e}"))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("exited with {status}"))
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    // KILL THE WHOLE TREE, NOT JUST THE SHELL. `child.kill()` signals the
                    // `/bin/sh -c` this spawned and nothing below it, so a line like
                    // `curl ... | tee` or a backgrounded `docker exec` outlives the timeout and keeps
                    // configuring the gateway - or holding its port - AFTER the timeout was reported
                    // as a failure and measurement moved on. A command that reconfigures the gateway
                    // mid-window changes what is being measured while it is being measured.
                    kill_descendants(child.id());
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timed out after {timeout:?}"));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("failed to poll status: {e}")),
        }
    }
}

/// Hard-kill everything below `root` in the process tree.
///
/// FREEZE FIRST, THEN KILL. Signalling the tree one layer at a time loses a race the timeout path
/// cannot afford: kill the leaf `sleep` and its parent shell wakes up and runs the NEXT command in
/// the line (the `touch`, the `curl` that reconfigures the gateway) before the signal aimed at it
/// arrives. SIGSTOP on the whole tree first means nothing in it is scheduled again at all, so the
/// SIGKILL that follows lands on a tree that cannot have advanced.
///
/// Reads one process table snapshot (`supervise::process_table`) rather than creating a process
/// group: `unsafe_code` is forbidden in this crate, so `setsid` before exec is not available, and a
/// `setsid` binary is not present on every platform a contributor runs the tests on. The accepted
/// limitation is a grandchild already reparented away from `root` before the snapshot - its link to
/// this tree is gone from the kernel's own records at that point, so nothing could have found it.
fn kill_descendants(root: u32) {
    let table = crate::supervise::process_table();
    let mut tree: Vec<u32> = vec![root];
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for e in table.iter() {
            if e.ppid == parent && !tree.contains(&e.pid) {
                tree.push(e.pid);
                frontier.push(e.pid);
            }
        }
    }
    let descendants = &tree[1..];
    if descendants.is_empty() {
        return;
    }
    // The root is frozen along with them: it is the shell that would otherwise start the next
    // command in the line. Its own SIGKILL comes from the caller's `Child::kill`, which is the only
    // way to also reap it.
    signal_tree("-STOP", &tree);
    signal_tree("-KILL", descendants);
}

fn signal_tree(signal: &str, pids: &[u32]) {
    let _ = Command::new("kill")
        .arg(signal)
        .args(pids.iter().map(u32::to_string))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Collects a native gateway's `Child`, if there is one, so the OS releases its process table entry
/// instead of leaving a zombie behind.
///
/// KILLED THROUGH ITS OWN HANDLE BEFORE THE WAIT, so the wait can never hang on a live process. The
/// callers reach this after `supervise::stop_and_wait`, which finds the gateway by `proc_match` in the
/// process table, not by this handle - and that can miss it (a `ps` that failed, a pattern that does
/// not select the pid) or give up on it (`StillHeld`). A bare `wait()` used to follow, which blocked
/// the whole run forever on exactly those cases. A SIGKILL to a child that already exited is a no-op,
/// so the normal path is unchanged. `spawn()`'s backstop relies on the same rule: a previous child
/// this launcher still holds is ITS gateway, and one left running would keep the port and the cores
/// the replacement needs, so it is killed and reaped rather than dropped and orphaned.
fn reap_native_child(slot: &mut Option<std::process::Child>) {
    if let Some(mut child) = slot.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The real syscall layer: shells out via `std::process` exactly as the docker/taskset invocations
/// `build_invocation` describes, and delegates readiness/stop to `supervise`. Kept thin on purpose;
/// the logic worth testing lives in `launch_with`, not here.
///
/// `native_child` holds the `Child` handle for a native (non-docker) spawn. A docker launch has
/// nothing to hold here (`docker run -d` is already waited on synchronously in `spawn`, and `stop`
/// goes through `docker rm -f`); a native gateway IS the child, spawned once and left running for the
/// whole measurement, and only THIS PROCESS (its real parent) can ever reap it. `stop`'s `pkill -f`
/// kills it, but a `pkill` from another process cannot collect the exit status: the kernel leaves a
/// zombie entry until the actual parent calls `wait`. Discarding the `Child` instead of holding it
/// would leave `stop()`'s `pkill` killing the process without ever reaping it, an unbounded number of
/// zombies over an eight-hour run with retries on every flaky boot.
#[derive(Default)]
pub struct RealLauncher {
    native_child: Option<std::process::Child>,
}

impl RealLauncher {
    /// Collect whatever native child this launcher already holds. Exists for `restart_to_rest`,
    /// which stops and restarts a gateway through the SAME `RealLauncher` for every cell rather than
    /// a fresh one per call, so the `Child` a previous restart spawned is still here to be waited on
    /// before the next one replaces it. Callers stop the gateway first (`supervise::stop_and_wait`);
    /// the child is killed through its handle before the wait, so even a stop that missed it cannot
    /// turn this into a hang.
    pub fn reap_previous_native_child(&mut self) {
        reap_native_child(&mut self.native_child);
    }
}

#[cfg(test)]
#[path = "tests/launch_test_support.rs"]
mod test_support;

impl Launcher for RealLauncher {
    fn run_pre_launch(&mut self, step: &PreLaunchStep) -> Result<(), String> {
        run_with_timeout(&step.command, &step.args, step.timeout)
    }

    fn spawn(&mut self, spec: &LaunchSpec) -> Result<(), String> {
        let inv = build_invocation(spec);
        // A CONTAINER START IS WAITED ON; A NATIVE GATEWAY IS NOT.
        //
        // `docker run -d` returns as soon as the container is created, so its exit status and stderr
        // are available immediately and are the ONLY place a refusal appears - a rejected mount, a
        // cpuset the daemon will not honour, a name already in use. Dropping the child made every one
        // of those look identical to a gateway that started and never bound its port, and the
        // published absence could name the attempts but not the cause.
        //
        // A native gateway IS the child and runs for the whole measurement, so waiting on it would
        // hang the run forever.
        if matches!(spec.kind, LaunchKind::Docker { .. }) {
            let out = Command::new(&inv.program)
                .args(&inv.args)
                .output()
                .map_err(|e| format!("failed to run {}: {e}", inv.program))?;
            if !out.status.success() {
                let why = String::from_utf8_lossy(&out.stderr);
                let why = why.trim();
                return Err(if why.is_empty() {
                    format!("{} exited with {}", inv.program, out.status)
                } else {
                    why.to_string()
                });
            }
            return Ok(());
        }
        let mut cmd = Command::new(&inv.program);
        cmd.args(&inv.args).envs(inv.env.iter().cloned());
        // REMOVED BEFORE ADDED, and before the process exists: an inherited variable the target
        // rejects has to be gone, not overwritten.
        for name in &inv.env_unset {
            cmd.env_remove(name);
        }
        // Reap whatever this launcher's own last native attempt left behind before replacing it. In
        // the normal retry loop `stop()` already did this, so this is normally a no-op; it exists as
        // a backstop for any caller that spawns again without an intervening `stop()`.
        reap_native_child(&mut self.native_child);
        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn {}: {e}", inv.program))?;
        self.native_child = Some(child);
        Ok(())
    }

    fn is_ready(&mut self, spec: &LaunchSpec) -> bool {
        matches!(
            supervise::wait_until_ready(spec.port, spec.ready_budget),
            ReadyOutcome::Ready
        )
    }

    fn stop(&mut self, spec: &LaunchSpec) {
        // SAID, NOT DISCARDED. A stop that could not confirm release leaves something on the port;
        // `launch_with` refuses to spawn the next attempt onto a held port, and this line is what
        // tells the operator why that refusal is coming.
        if let Err(e) = supervise::stop_and_wait(&spec.runtime, spec.port, Duration::from_secs(5)) {
            eprintln!(
                "launch: stopping a failed attempt of {} did not confirm release: {e}",
                spec.runtime.identity()
            );
        }
        // Killed through the handle, then waited: never a hang, whatever the stop above concluded.
        reap_native_child(&mut self.native_child);
    }

    fn port_snapshot(&mut self, spec: &LaunchSpec) -> PortState {
        supervise::port_state(spec.port)
    }

    /// Read back what the container printed. The tail only: a whole log would bury the reason it is
    /// being read for. Sixty lines rather than twenty because a gateway that boots, prints a banner
    /// and a page of startup warnings, and only then fails, pushed the actual error out of a shorter
    /// window: one entrant's capture was twenty lines of nginx warnings and no error at all.
    fn diagnostics(&mut self, spec: &LaunchSpec) -> Option<String> {
        if !spec.runtime.is_docker() {
            return None;
        }
        let out = Command::new("docker")
            .args(["logs", "--tail", "60", &spec.runtime.identity()])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        // A container that died at startup usually says why on stderr, so both streams are kept.
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        let text = neutralise_control_chars(text.trim());
        (!text.is_empty()).then_some(text)
    }
}

/// The gateway's own log, made safe to print. The container under test is untrusted input, and this
/// text reaches the operator's terminal and the box's run log inside `LaunchError::NeverReady`: an
/// escape sequence in it could rewrite or hide the lines around it. Every control character except a
/// newline or a tab is written out as its escape (`\u{1b}`) instead, so the evidence is still all
/// there and none of it is interpreted.
fn neutralise_control_chars(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() && c != '\n' && c != '\t' {
            out.extend(c.escape_default());
        } else {
            out.push(c);
        }
    }
    out
}

/// Launch `spec`, retrying up to `attempts` times (a zero is treated as one: this is not a way to
/// skip trying). Never returns `Ok` without `Launcher::is_ready` having actually reported true.
pub fn launch(
    launcher: &mut impl Launcher,
    spec: &LaunchSpec,
    attempts: u32,
) -> Result<Launched, LaunchError> {
    launch_with(launcher, spec, attempts, std::thread::sleep)
}

/// Launch `spec` using the crate's default attempt bound (`DEFAULT_LAUNCH_ATTEMPTS`), matching the
/// shell's `HARNESS_BOOT_ATTEMPTS` default.
pub fn launch_default(
    launcher: &mut impl Launcher,
    spec: &LaunchSpec,
) -> Result<Launched, LaunchError> {
    launch(launcher, spec, DEFAULT_LAUNCH_ATTEMPTS)
}

/// The generic, testable core. `sleep` is injected so a test can assert "backoff happened between
/// attempts" and run instantly, exactly as `supervise::stop_and_wait_with` does.
pub fn launch_with(
    launcher: &mut impl Launcher,
    spec: &LaunchSpec,
    attempts: u32,
    mut sleep: impl FnMut(Duration),
) -> Result<Launched, LaunchError> {
    spec.validate().map_err(LaunchError::InvalidSpec)?;

    // The escape hatch, run once, before any attempt. A failure here means the gateway itself is
    // never spawned at all: this is what "aborts the launch loudly" means in practice.
    if let Some(step) = &spec.pre_launch {
        launcher
            .run_pre_launch(step)
            .map_err(|reason| LaunchError::PreLaunchFailed {
                command: step.command.clone(),
                reason,
            })?;
    }

    let bounded = attempts.max(1);
    let mut last_spawn_error = None;
    let mut last_output = None;
    for attempt in 1..=bounded {
        // THE PORT MUST BE FREE BEFORE ANYTHING IS SPAWNED ONTO IT. Readiness is "something answers
        // on the port", so a listener this launch did not start - a container a crashed run left on
        // the host network, or a previous attempt that outlived its stop - would satisfy it while the
        // fresh process died on EADDRINUSE, and the whole grid would then be measured against the
        // survivor and published under this launch's name. Only `Held` is refused: `Unknown` is a
        // probe that could not tell, and refusing on it would fail launches on our own probe's
        // account.
        let spawned = if launcher.port_snapshot(spec) == PortState::Held {
            last_spawn_error = Some(format!(
                "port {} was already held before this attempt spawned anything, so a readiness \
                 check could only have observed a listener this launch did not start; refusing to \
                 spawn onto it",
                spec.port
            ));
            false
        } else {
            match launcher.spawn(spec) {
                // The reason is the LAST attempt's: an attempt that spawned cleanly clears what an
                // earlier refusal said, or a config failure would be reported as that refusal.
                Ok(()) => {
                    last_spawn_error = None;
                    true
                }
                Err(why) => {
                    last_spawn_error = Some(why);
                    false
                }
            }
        };
        if spawned && launcher.is_ready(spec) {
            return Ok(Launched {
                runtime: spec.runtime.clone(),
                attempts: attempt,
            });
        }
        // READ ITS LOG BEFORE KILLING IT. For docker, `stop` is `rm -f`, which takes the container
        // and its log with it, so reading it after that stop would arrive with no evidence.
        // Keep the last explanation we actually got. A later attempt whose log cannot be read (the
        // runtime refused the invocation outright, so there is no container to ask) must not erase
        // the reason an earlier attempt already gave.
        if let Some(said) = launcher.diagnostics(spec) {
            last_output = Some(said);
        }
        // Kill whatever this attempt left behind before trying again, matching
        // harness_launch_ready's `gw_stop` between attempts: a half-bound listener must not be
        // allowed to wedge the retry.
        launcher.stop(spec);
        if attempt < bounded {
            sleep(spec.boot_backoff * attempt);
        }
    }

    let last_port_state = launcher.port_snapshot(spec);
    Err(LaunchError::NeverReady {
        attempts: bounded,
        last_port_state,
        last_spawn_error,
        last_output,
    })
}

#[cfg(test)]
#[path = "tests/launch_tests.rs"]
mod tests;
