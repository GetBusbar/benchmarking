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

/// A launch the retry loop actually confirmed ready. There is no public constructor other than
/// `launch`/`launch_with`, so a value of this type is evidence, not an assumption.
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
    /// listener. Best effort: a stop that cannot confirm release still lets the loop retry, and the
    /// next readiness check is what actually catches a lingering process.
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
/// instead of leaving a zombie behind. `blocking`: `stop()` calls this only once the process is
/// already confirmed dead (or the kill budget gave up), so a `wait` there reaps instantly rather than
/// hanging; `spawn()`'s own backstop cleanup uses `try_wait` instead, since a stale child there might
/// genuinely still be alive and must never block a fresh spawn.
/// Strip ANSI/OSC escape sequences and other non-printable control bytes from text captured from the
/// GATEWAY-UNDER-TEST (its `docker logs`), before that text is embedded in a `LaunchError` Display and
/// printed to the operator's terminal via `eprintln!` during an unattended multi-hour run. RULES.md
/// treats the gateway's own output as potentially hostile: a gateway that prints an OSC 52
/// clipboard-write or a cursor/title escape on a startup failure would otherwise have that sequence
/// executed against the operator's terminal emulator. Newlines and tabs are kept so a multi-line log
/// tail stays readable; every other C0/C1 control and the CSI/OSC sequences that begin with ESC are
/// dropped.
fn strip_terminal_controls(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                // CSI: ESC [ params/intermediates ... final byte in 0x40..=0x7e.
                Some('[') => {
                    chars.next();
                    while let Some(&p) = chars.peek() {
                        chars.next();
                        if ('\u{40}'..='\u{7e}').contains(&p) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] ... terminated by BEL (0x07) or ST (ESC \).
                Some(']') => {
                    chars.next();
                    while let Some(c2) = chars.next() {
                        if c2 == '\x07' {
                            break;
                        }
                        if c2 == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Any other two-character escape: drop ESC and the byte that follows it.
                _ => {
                    chars.next();
                }
            },
            '\n' | '\t' => out.push(c),
            // Drop every other C0 control (including CR and BEL), DEL, and the C1 range.
            c if (c as u32) < 0x20 || c as u32 == 0x7f || (0x80..=0x9f).contains(&(c as u32)) => {}
            c => out.push(c),
        }
    }
    out
}

fn reap_native_child(slot: &mut Option<std::process::Child>, blocking: bool) {
    if let Some(mut child) = slot.take() {
        if blocking {
            let _ = child.wait();
        } else {
            let _ = child.try_wait();
        }
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
    /// before the next one replaces it. Callers must confirm the process is actually gone first (a
    /// `supervise::stop_and_wait` that returned `Ok`) - as `stop()` does above - so the blocking
    /// `wait()` here always reaps instantly rather than risking a hang on a live process.
    pub(crate) fn reap_previous_native_child(&mut self) {
        reap_native_child(&mut self.native_child, true);
    }
}

#[cfg(test)]
impl RealLauncher {
    pub(crate) fn native_pid(&self) -> Option<u32> {
        self.native_child.as_ref().map(std::process::Child::id)
    }
}

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
        reap_native_child(&mut self.native_child, false);
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
        // ONLY BLOCKING-REAP ONCE THE PROCESS IS CONFIRMED GONE. A blocking `wait()` on a process the
        // kernel has not released hangs forever - and stop_and_wait returns Err(StillHeld) exactly
        // when it could NOT confirm death (e.g. a native child stuck in an uninterruptible D state on
        // an overloaded box, where even SIGKILL cannot reap it until its I/O completes). This used to
        // discard that Err and then blocking-`wait()` unconditionally, so a single stuck gateway could
        // hang the whole run (launch's retry loop calls stop() between attempts) until the box's
        // external cost timer tore it down. On Err, reap non-blocking instead: the zombie is collected
        // on a later attempt or at process exit, but the run is never wedged.
        match supervise::stop_and_wait(&spec.runtime, spec.port, Duration::from_secs(5)) {
            Ok(()) => reap_native_child(&mut self.native_child, true),
            Err(_still_held) => reap_native_child(&mut self.native_child, false),
        }
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
        // Sanitize before this reaches any error Display / eprintln: the gateway's own log bytes are
        // untrusted and must not carry terminal escape sequences to the operator's console.
        let text = strip_terminal_controls(&text).trim().to_string();
        (!text.is_empty()).then_some(text)
    }
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
        let spawned = match launcher.spawn(spec) {
            Ok(()) => true,
            Err(why) => {
                last_spawn_error = Some(why);
                false
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
mod tests {
    use super::*;

    fn docker_spec() -> LaunchSpec {
        LaunchSpec {
            runtime: Runtime::Docker {
                container: "gw-bench".into(),
                run_scope: None,
            },
            kind: LaunchKind::Docker {
                image: "gw:1.0".into(),
                env: vec![("A".into(), "B".into())],
                port: PortMapping::Host,
                mounts: vec![Mount {
                    host_path: "/tmp/gw.yaml".into(),
                    container_path: "/config.yaml".into(),
                    read_only: true,
                }],
                command: vec!["-f".into(), "/config.yaml".into()],
            },
            cores: "0-3".into(),
            port: 8080,
            ready_budget: Duration::from_secs(5),
            boot_backoff: Duration::from_secs(1),
            pre_launch: None,
        }
    }

    fn native_spec() -> LaunchSpec {
        LaunchSpec {
            runtime: Runtime::Native {
                proc_match: "gw-native".into(),
            },
            kind: LaunchKind::Native {
                binary: "/opt/gw/bin/gw-native".into(),
                args: vec!["--port".into(), "8080".into()],
                env: vec![("GW_AUTH".into(), "dummy".into())],
                env_unset: vec![],
            },
            cores: "0-3".into(),
            port: 8080,
            ready_budget: Duration::from_secs(5),
            boot_backoff: Duration::from_secs(1),
            pre_launch: None,
        }
    }

    // ---- native child reaping -----------------------------------------------------------------------

    /// `ps -o state=` reports a zombie as `Z` on both Linux and macOS (unlike `taskset`, which this
    /// test deliberately avoids depending on, so it runs on a contributor's Mac as well as CI). Empty
    /// output (or a nonzero exit) means the OS has no process table entry for that pid at all.
    fn ps_state(pid: u32) -> String {
        Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    // A `Child` a launcher never waits on becomes exactly the zombie this whole mechanism exists to
    // prevent: the process finishes, the OS keeps its exit status pending collection, and nothing
    // ever asks for it. This spawns a real, instantly-exiting process, confirms (as a sanity check on
    // the test itself, not the fix) that it really does sit as a zombie before anyone reaps it, then
    // proves `reap_native_child` collects it.
    #[test]
    fn reap_native_child_collects_a_finished_process_not_just_the_handle() {
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn a real process");
        let pid = child.id();
        let mut slot = Some(child);

        // Give it time to actually exit before anyone has reaped it.
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ps_state(pid).contains('Z') && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ps_state(pid).contains('Z'),
            "test precondition: the process must sit as a zombie before it is reaped, ps state was {:?}",
            ps_state(pid)
        );

        reap_native_child(&mut slot, true);

        assert!(slot.is_none(), "the slot must be cleared once reaped");
        assert!(
            ps_state(pid).is_empty(),
            "the zombie must be gone from the process table after reaping, ps state was {:?}",
            ps_state(pid)
        );
    }

    // ---- commands run in the gateway's own directory -----------------------------------------------

    fn scratch(name: &str) -> std::path::PathBuf {
        // Shell-safe by construction: this path is interpolated into a `/bin/sh -c` line below, and
        // a thread id (`ThreadId(7)`) would put parentheses into it.
        let dir =
            std::env::temp_dir().join(format!("otb-launch-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    // THE DEFECT: a `commands` line is transcribed from a gateway's own docs and writes relative
    // paths (`> .minted-auth` is the one that already matters). Run with the engine's inherited cwd,
    // that artifact landed wherever otb was started from, and `resolve_minted_auth` - which reads
    // `<gw_dir>/.minted-auth` - never saw it, so the run silently authenticated with the manifest's
    // placeholder.
    #[test]
    fn a_commands_line_writes_its_relative_artifact_into_the_gateway_directory() {
        let dir = scratch("cwd");
        run_line_in(
            Some(&dir),
            "printf 'sk-minted-123' > .minted-auth",
            Duration::from_secs(10),
        )
        .expect("the line runs");
        assert_eq!(
            std::fs::read_to_string(dir.join(".minted-auth")).unwrap(),
            "sk-minted-123",
            "the credential must land in the gateway directory, not in the engine's cwd"
        );
    }

    // A LINE THAT HANGS MUST NOT LEAVE ITS GRANDCHILDREN RUNNING. Killing only the spawned shell let
    // a backgrounded curl or `docker exec` outlive the timeout and keep configuring the gateway after
    // the timeout had been reported and measurement had moved on - a gateway reconfigured mid-window
    // is not the gateway that was measured.
    #[test]
    fn a_timed_out_commands_line_takes_its_grandchildren_with_it() {
        let dir = scratch("tree-kill");
        let marker = dir.join("grandchild-survived");
        // The shell backgrounds a grandchild that would touch the marker a second from now, then
        // waits. `child.kill()` alone reaches the shell only; the grandchild goes on to write.
        let line = format!(
            "/bin/sh -c 'sleep 1; touch {}' & wait",
            marker.to_string_lossy()
        );
        let err = run_line_in(Some(&dir), &line, Duration::from_millis(250))
            .expect_err("the line must time out");
        assert!(err.contains("timed out"), "{err}");

        std::thread::sleep(Duration::from_millis(2500));
        assert!(
            !marker.exists(),
            "a grandchild of the timed-out line survived the kill and went on working"
        );
    }

    // ---- required: container argument construction ------------------------------------------------

    #[test]
    fn container_args_include_name_image_env_port_mapping_and_pinning() {
        let inv = build_invocation(&docker_spec());
        assert_eq!(inv.program, "docker");
        assert!(
            inv.args
                .windows(2)
                .any(|w| w == ["--name".to_string(), "gw-bench".to_string()]),
            "{:?}",
            inv.args
        );
        assert!(
            inv.args
                .windows(2)
                .any(|w| w == ["--network".to_string(), "host".to_string()]),
            "{:?}",
            inv.args
        );
        assert!(
            inv.args
                .windows(2)
                .any(|w| w == ["--cpuset-cpus".to_string(), "0-3".to_string()]),
            "cpu pinning must be present: {:?}",
            inv.args
        );
        assert!(
            inv.args
                .windows(2)
                .any(|w| w == ["-e".to_string(), "A=B".to_string()]),
            "{:?}",
            inv.args
        );
        assert!(
            inv.args.contains(&"gw:1.0".to_string()),
            "the image must appear: {:?}",
            inv.args
        );
        // The container name comes from spec.runtime.identity(), the same identity the memory
        // readers and the stop path take: there is no second name to disagree with it.
        assert_eq!(
            Runtime::Docker {
                container: "gw-bench".into(),
                run_scope: None,
            }
            .identity(),
            "gw-bench"
        );
    }

    // A RUN-SCOPED NAME, AND LABELS SO THE RUN CAN STILL FIND ITS OWN. Two overlapping runs of the
    // same gateway used to name one container, so the second run's retry-loop `docker rm -f` deleted
    // the first run's container mid-measurement.
    #[test]
    fn a_containers_name_is_scoped_to_the_run_that_created_it() {
        let mut spec = docker_spec();
        spec.runtime = spec.runtime.scoped_to_run("run-7");
        let inv = build_invocation(&spec);
        assert!(
            inv.args
                .windows(2)
                .any(|w| w == ["--name".to_string(), "gw-bench-run-7".to_string()]),
            "{:?}",
            inv.args
        );
        assert!(
            !inv.args.contains(&"gw-bench".to_string()),
            "the unscoped name must not be what gets created: {:?}",
            inv.args
        );
        // The stop path targets this same identity, so teardown removes its own container and only
        // its own; the labels are how a cross-run sweep finds them without parsing names.
        assert_eq!(spec.runtime.identity(), "gw-bench-run-7");
        assert!(inv
            .args
            .windows(2)
            .any(|w| w == ["--label".to_string(), "otb.run=run-7".to_string()]));
        assert!(inv
            .args
            .windows(2)
            .any(|w| w == ["--label".to_string(), "otb.gateway=gw-bench".to_string()]));
    }

    #[test]
    fn a_published_port_mapping_emits_a_dash_p_flag_instead_of_host_networking() {
        let mut spec = docker_spec();
        spec.kind = LaunchKind::Docker {
            image: "gw:1.0".into(),
            env: vec![],
            port: PortMapping::Published {
                host: 8080,
                container: 9090,
            },
            mounts: vec![],
            command: vec![],
        };
        let inv = build_invocation(&spec);
        assert!(
            inv.args
                .windows(2)
                .any(|w| w == ["-p".to_string(), "8080:9090".to_string()]),
            "{:?}",
            inv.args
        );
        assert!(!inv.args.contains(&"--network".to_string()));
    }

    // ---- required: native argument construction ---------------------------------------------------

    #[test]
    fn native_args_include_binary_args_and_pinning() {
        let inv = build_invocation(&native_spec());
        assert_eq!(inv.program, "taskset");
        assert!(
            inv.args
                .windows(2)
                .any(|w| w == ["-c".to_string(), "0-3".to_string()]),
            "cpu pinning must be present: {:?}",
            inv.args
        );
        assert!(inv.args.contains(&"/opt/gw/bin/gw-native".to_string()));
        assert!(inv.args.contains(&"--port".to_string()));
        assert!(inv.args.contains(&"8080".to_string()));
        assert_eq!(inv.env, vec![("GW_AUTH".to_string(), "dummy".to_string())]);
    }

    // ---- fake launcher for the retry/readiness loop -----------------------------------------------

    struct FakeLauncher {
        ready_script: Vec<bool>,
        spawn_calls: u32,
        stop_calls: u32,
        pre_launch_calls: u32,
        pre_launch_result: Result<(), String>,
        port_state: PortState,
    }

    impl FakeLauncher {
        fn ready_after(script: Vec<bool>) -> Self {
            FakeLauncher {
                ready_script: script,
                spawn_calls: 0,
                stop_calls: 0,
                pre_launch_calls: 0,
                pre_launch_result: Ok(()),
                port_state: PortState::Unknown,
            }
        }
    }

    impl Launcher for FakeLauncher {
        fn run_pre_launch(&mut self, _step: &PreLaunchStep) -> Result<(), String> {
            self.pre_launch_calls += 1;
            self.pre_launch_result.clone()
        }
        fn spawn(&mut self, _spec: &LaunchSpec) -> Result<(), String> {
            self.spawn_calls += 1;
            Ok(())
        }
        fn is_ready(&mut self, _spec: &LaunchSpec) -> bool {
            if self.ready_script.is_empty() {
                false
            } else {
                self.ready_script.remove(0)
            }
        }
        fn stop(&mut self, _spec: &LaunchSpec) {
            self.stop_calls += 1;
        }
        fn port_snapshot(&mut self, _spec: &LaunchSpec) -> PortState {
            self.port_state
        }
    }

    fn no_sleep(_: Duration) {}

    // ---- required: ready on the first attempt costs exactly one attempt ---------------------------

    #[test]
    fn ready_on_first_attempt_launches_with_exactly_one_attempt() {
        let mut fake = FakeLauncher::ready_after(vec![true]);
        let spec = docker_spec();
        let launched = launch_with(&mut fake, &spec, 3, no_sleep).unwrap();
        assert_eq!(launched.attempts, 1);
        assert_eq!(fake.spawn_calls, 1);
        assert_eq!(
            fake.stop_calls, 0,
            "a successful first attempt has nothing to clean up"
        );
    }

    // ---- required: ready on the third attempt succeeds, asserting three attempts happened ---------

    #[test]
    fn ready_on_third_attempt_succeeds_after_exactly_three_attempts() {
        let mut fake = FakeLauncher::ready_after(vec![false, false, true]);
        let spec = docker_spec();
        let mut sleeps = 0u32;
        let launched = launch_with(&mut fake, &spec, 3, |_| sleeps += 1).unwrap();
        assert_eq!(launched.attempts, 3);
        assert_eq!(fake.spawn_calls, 3);
        assert_eq!(
            fake.stop_calls, 2,
            "the two failed attempts are cleaned up before the next try"
        );
        assert_eq!(
            sleeps, 2,
            "backoff happens between attempts, not after the final success"
        );
    }

    // ---- required: a gateway never ready exhausts the bound with a typed error, never success -----

    #[test]
    fn never_ready_exhausts_the_bound_and_returns_a_typed_error() {
        let mut fake = FakeLauncher::ready_after(vec![false, false, false]);
        fake.port_state = PortState::Free;
        let spec = docker_spec();
        let err = launch_with(&mut fake, &spec, 3, no_sleep).unwrap_err();
        assert_eq!(
            err,
            LaunchError::NeverReady {
                attempts: 3,
                last_port_state: PortState::Free,
                last_spawn_error: None,
                last_output: None
            }
        );
        assert_eq!(
            fake.spawn_calls, 3,
            "the bound must be respected exactly, not exceeded"
        );
    }

    // ---- required: a failing pre-launch step aborts loudly and the gateway is never started --------

    #[test]
    fn a_failing_pre_launch_step_aborts_before_any_spawn() {
        let mut fake = FakeLauncher::ready_after(vec![true]);
        fake.pre_launch_result = Err("build failed".to_string());
        let mut spec = docker_spec();
        spec.pre_launch = Some(PreLaunchStep {
            command: "cargo".into(),
            args: vec!["build".into(), "--release".into()],
            timeout: Duration::from_secs(60),
        });
        let err = launch_with(&mut fake, &spec, 3, no_sleep).unwrap_err();
        assert_eq!(
            err,
            LaunchError::PreLaunchFailed {
                command: "cargo".to_string(),
                reason: "build failed".to_string()
            }
        );
        assert_eq!(fake.pre_launch_calls, 1);
        assert_eq!(
            fake.spawn_calls, 0,
            "the gateway must never be started when the pre-launch step fails"
        );
    }

    // ---- required: a spec with no cpu pinning is rejected ------------------------------------------

    #[test]
    fn a_spec_with_no_cpu_pinning_is_rejected() {
        let mut fake = FakeLauncher::ready_after(vec![true]);
        let mut spec = docker_spec();
        spec.cores = "".into();
        let err = launch_with(&mut fake, &spec, 3, no_sleep).unwrap_err();
        assert_eq!(err, LaunchError::InvalidSpec(SpecError::NoCpuPinning));
        assert_eq!(
            fake.spawn_calls, 0,
            "an unpinned spec must never reach a spawn attempt"
        );

        let mut whitespace_only = docker_spec();
        whitespace_only.cores = "   ".into();
        assert_eq!(whitespace_only.validate(), Err(SpecError::NoCpuPinning));
    }

    #[test]
    fn a_spec_with_cpu_pinning_and_a_declared_identity_validates() {
        assert!(docker_spec().validate().is_ok());
        assert!(native_spec().validate().is_ok());
    }

    // ---- required: the stop identity equals Runtime::identity() for both runtime kinds ------------

    #[test]
    fn the_stop_identity_equals_runtime_identity_for_docker() {
        let spec = docker_spec();
        // RealLauncher::stop calls supervise::stop_and_wait(&spec.runtime, ...) directly, and
        // build_invocation's --name comes from the same spec.runtime.identity(): there is no
        // second field either could read a different name from.
        assert_eq!(spec.runtime.identity(), "gw-bench");
        let inv = build_invocation(&spec);
        assert!(inv
            .args
            .windows(2)
            .any(|w| w == ["--name".to_string(), spec.runtime.identity().to_string()]));
    }

    #[test]
    fn the_stop_identity_equals_runtime_identity_for_native() {
        let spec = native_spec();
        assert_eq!(spec.runtime.identity(), "gw-native");
        // The native launch's proc_match need not equal the exact binary path (a substring is all
        // pgrep -f needs), but it must be the SAME string stop_and_wait would be handed: there is
        // no separate "stop name" field on LaunchSpec at all, only runtime.
        assert!(matches!(spec.runtime, Runtime::Native { .. }));
    }

    #[test]
    fn validate_rejects_an_empty_image_or_binary() {
        let mut d = docker_spec();
        d.kind = LaunchKind::Docker {
            image: "  ".into(),
            env: vec![],
            port: PortMapping::Host,
            mounts: vec![],
            command: vec![],
        };
        assert_eq!(d.validate(), Err(SpecError::Empty("image")));

        let mut n = native_spec();
        n.kind = LaunchKind::Native {
            binary: " ".into(),
            args: vec![],
            env: vec![],
            env_unset: vec![],
        };
        assert_eq!(n.validate(), Err(SpecError::Empty("binary")));
    }

    #[test]
    fn validate_rejects_a_zero_port() {
        let mut spec = docker_spec();
        spec.port = 0;
        assert_eq!(spec.validate(), Err(SpecError::BadPort));
    }

    #[test]
    fn a_zero_attempt_bound_is_treated_as_one_not_a_way_to_skip_trying() {
        let mut fake = FakeLauncher::ready_after(vec![true]);
        let launched = launch_with(&mut fake, &docker_spec(), 0, no_sleep).unwrap();
        assert_eq!(launched.attempts, 1);
        assert_eq!(fake.spawn_calls, 1);
    }

    // THE EVIDENCE MUST OUTLIVE THE TEARDOWN.
    //
    // A failed attempt is torn down before the next one, and for docker that teardown is `rm -f`,
    // which takes the container's log with it. Without reading the log BEFORE that stop, a config
    // typo, a rejected mount and an unsupported flag would all surface identically as "never became
    // ready; last port state: Free", so the launcher reads the log first and the reason travels in
    // the error.
    #[test]
    fn a_gateway_that_never_came_up_carries_what_it_said_before_it_died() {
        struct Dying {
            said: &'static str,
            stopped: bool,
        }
        impl Launcher for Dying {
            fn run_pre_launch(&mut self, _s: &PreLaunchStep) -> Result<(), String> {
                Ok(())
            }
            fn spawn(&mut self, _s: &LaunchSpec) -> Result<(), String> {
                // A fresh container, so a fresh log: this is what makes reading it after the
                // teardown look like it works on a retry and fail on the last attempt.
                self.stopped = false;
                Ok(())
            }
            fn is_ready(&mut self, _s: &LaunchSpec) -> bool {
                false
            }
            fn stop(&mut self, _s: &LaunchSpec) {
                // Once stopped, the log is gone. Reading it after this point is the defect.
                self.stopped = true;
            }
            fn port_snapshot(&mut self, _s: &LaunchSpec) -> PortState {
                PortState::Free
            }
            fn diagnostics(&mut self, _s: &LaunchSpec) -> Option<String> {
                if self.stopped {
                    return None;
                }
                Some(self.said.to_string())
            }
        }
        let mut l = Dying {
            said: "config: unknown field `nope`",
            stopped: false,
        };
        let err = launch(&mut l, &docker_spec(), 2).expect_err("it never becomes ready");
        let LaunchError::NeverReady { last_output, .. } = &err else {
            panic!("expected NeverReady, got {err:?}");
        };
        assert_eq!(
            last_output.as_deref(),
            Some("config: unknown field `nope`"),
            "the gateway's own explanation must survive the teardown"
        );
        assert!(
            format!("{err}").contains("unknown field"),
            "and it must reach the operator in the rendered message"
        );
    }

    // A REFUSAL FROM `spawn` MUST CARRY ITS REASON. For docker, `spawn`'s own stderr is (per its doc)
    // the ONLY place a rejected mount, a cpuset the daemon will not honour, or a name collision ever
    // appears - the container never gets far enough to have a log. Every launcher double used to
    // return Ok(()) from spawn, so `last_spawn_error`'s capture and its rendering in NeverReady were
    // untested; a regression dropping `last_spawn_error = Some(why)` would leave every test green
    // while a real refusal silently lost its explanation.
    #[test]
    fn a_spawn_refusal_is_carried_into_the_never_ready_error() {
        struct Refusing;
        impl Launcher for Refusing {
            fn run_pre_launch(&mut self, _s: &PreLaunchStep) -> Result<(), String> {
                Ok(())
            }
            fn spawn(&mut self, _s: &LaunchSpec) -> Result<(), String> {
                Err("docker: invalid mount config: /tmp/gw.yaml not found".to_string())
            }
            fn is_ready(&mut self, _s: &LaunchSpec) -> bool {
                true // never reached: a refused spawn is not ready-checked
            }
            fn stop(&mut self, _s: &LaunchSpec) {}
            fn port_snapshot(&mut self, _s: &LaunchSpec) -> PortState {
                PortState::Free
            }
        }
        let err =
            launch(&mut Refusing, &docker_spec(), 2).expect_err("a refused spawn never readies");
        let LaunchError::NeverReady {
            last_spawn_error, ..
        } = &err
        else {
            panic!("expected NeverReady, got {err:?}");
        };
        assert_eq!(
            last_spawn_error.as_deref(),
            Some("docker: invalid mount config: /tmp/gw.yaml not found"),
            "the spawn refusal's reason must survive into the error"
        );
        assert!(
            format!("{err}").contains("invalid mount config"),
            "and it must reach the operator in the rendered message"
        );
    }

    // AN EARLIER ATTEMPT'S EXPLANATION MUST NOT BE ERASED BY A LATER ATTEMPT THAT CANNOT BE INSPECTED.
    // `last_output` is updated only `if let Some(said) = diagnostics(..)`, so a later attempt whose
    // container could not be read (diagnostics returns None) must PRESERVE the first attempt's text
    // rather than overwrite it. A regression to an unconditional overwrite would erase the real first
    // reason on a later uninspectable attempt.
    #[test]
    fn a_later_uninspectable_attempt_does_not_erase_an_earlier_reason() {
        struct FirstOnly {
            attempt: u32,
        }
        impl Launcher for FirstOnly {
            fn run_pre_launch(&mut self, _s: &PreLaunchStep) -> Result<(), String> {
                Ok(())
            }
            fn spawn(&mut self, _s: &LaunchSpec) -> Result<(), String> {
                self.attempt += 1;
                Ok(())
            }
            fn is_ready(&mut self, _s: &LaunchSpec) -> bool {
                false
            }
            fn stop(&mut self, _s: &LaunchSpec) {}
            fn port_snapshot(&mut self, _s: &LaunchSpec) -> PortState {
                PortState::Free
            }
            fn diagnostics(&mut self, _s: &LaunchSpec) -> Option<String> {
                // Attempt 1 has a log; attempt 2's container could not be inspected.
                if self.attempt == 1 {
                    Some("config: unknown field `nope`".to_string())
                } else {
                    None
                }
            }
        }
        let err = launch(&mut FirstOnly { attempt: 0 }, &docker_spec(), 2)
            .expect_err("it never becomes ready");
        let LaunchError::NeverReady { last_output, .. } = &err else {
            panic!("expected NeverReady, got {err:?}");
        };
        assert_eq!(
            last_output.as_deref(),
            Some("config: unknown field `nope`"),
            "attempt 1's reason must survive an attempt 2 whose log could not be read"
        );
    }

    // ---- untrusted gateway log sanitisation --------------------------------------------------------

    // The gateway-under-test's own `docker logs` output is captured verbatim and reaches the
    // operator's terminal via eprintln!. RULES.md treats it as potentially hostile, so escape
    // sequences must be stripped before that happens: an OSC 52 clipboard-write or a title/cursor CSI
    // printed on a startup failure must not be executed against the operator's terminal emulator.
    #[test]
    fn terminal_control_sequences_are_stripped_but_text_and_newlines_survive() {
        // A CSI colour sequence, an OSC 52 clipboard write (BEL-terminated), a bare BEL, and a raw
        // ESC, wrapped around real log text spanning two lines.
        let hostile =
            "boot ok\n\x1b[31mERROR\x1b[0m: \x1b]52;c;cGVk=\x07bad flag\x07\x1bX done\ttab";
        let clean = strip_terminal_controls(hostile);
        assert!(!clean.contains('\x1b'), "no ESC may survive: {clean:?}");
        assert!(!clean.contains('\x07'), "no BEL may survive: {clean:?}");
        assert!(
            clean.contains("ERROR"),
            "visible text must survive: {clean:?}"
        );
        assert!(
            clean.contains("bad flag"),
            "visible text must survive: {clean:?}"
        );
        assert!(
            clean.contains('\n'),
            "newlines are kept so the tail stays readable: {clean:?}"
        );
        assert!(clean.contains('\t'), "tabs are kept: {clean:?}");
        // The OSC payload between ESC] and BEL must be gone, not just the framing.
        assert!(
            !clean.contains("52;c"),
            "the OSC body must be dropped too: {clean:?}"
        );
    }
}
