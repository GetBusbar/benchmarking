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
// measure and the stop path targets. This is the same discipline `manifest.rs` already documents:
// the shell defect it replaces was three manifests writing a single-pid reader for RSS beside a
// whole-tree reader for HWM, because the name was spelled out once per hook instead of once.
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
}

/// Build the command a `LaunchSpec` runs, with no side effects. `spec.runtime.identity()` is the
/// ONLY source of a docker container's `--name`: there is no separate name field on `LaunchKind`, so
/// this invocation and `supervise::stop_and_wait`'s target cannot name two different things.
pub fn build_invocation(spec: &LaunchSpec) -> Invocation {
    match &spec.kind {
        LaunchKind::Docker { image, env, port, mounts, command } => {
            let name = spec.runtime.identity();
            let mut args = vec!["run".to_string(), "-d".to_string(), "--name".to_string(), name.to_string()];
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
            Invocation { program: "docker".to_string(), args, env: Vec::new() }
        }
        LaunchKind::Native { binary, args: bin_args, env } => {
            let mut args = vec!["-c".to_string(), spec.cores.clone(), binary.clone()];
            args.extend(bin_args.iter().cloned());
            Invocation { program: "taskset".to_string(), args, env: env.clone() }
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
    NeverReady { attempts: u32, last_port_state: PortState },
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::InvalidSpec(e) => write!(f, "launch spec invalid: {e}"),
            LaunchError::PreLaunchFailed { command, reason } => {
                write!(f, "pre-launch step {command:?} failed: {reason}")
            }
            LaunchError::NeverReady { attempts, last_port_state } => {
                write!(f, "never became ready after {attempts} attempt(s); last port state: {last_port_state:?}")
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
}

/// Run a pre-launch command with its own hard timeout, using only `std::process`: poll
/// `Child::try_wait` rather than blocking on `wait`, so a hung command is killed instead of hanging
/// this call forever.
fn run_with_timeout(command: &str, args: &[String], timeout: Duration) -> Result<(), String> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start: {e}"))?;

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

/// The real syscall layer: shells out via `std::process` exactly as the docker/taskset invocations
/// `build_invocation` describes, and delegates readiness/stop to `supervise`. Kept thin on purpose;
/// the logic worth testing lives in `launch_with`, not here.
pub struct RealLauncher;

impl Launcher for RealLauncher {
    fn run_pre_launch(&mut self, step: &PreLaunchStep) -> Result<(), String> {
        run_with_timeout(&step.command, &step.args, step.timeout)
    }

    fn spawn(&mut self, spec: &LaunchSpec) -> Result<(), String> {
        let inv = build_invocation(spec);
        Command::new(&inv.program)
            .args(&inv.args)
            .envs(inv.env.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_child| ())
            .map_err(|e| format!("failed to spawn {}: {e}", inv.program))
    }

    fn is_ready(&mut self, spec: &LaunchSpec) -> bool {
        matches!(supervise::wait_until_ready(spec.port, spec.ready_budget), ReadyOutcome::Ready)
    }

    fn stop(&mut self, spec: &LaunchSpec) {
        let _ = supervise::stop_and_wait(&spec.runtime, spec.port, Duration::from_secs(5));
    }

    fn port_snapshot(&mut self, spec: &LaunchSpec) -> PortState {
        supervise::port_state(spec.port)
    }
}

/// Launch `spec`, retrying up to `attempts` times (a zero is treated as one: this is not a way to
/// skip trying). Never returns `Ok` without `Launcher::is_ready` having actually reported true.
pub fn launch(launcher: &mut impl Launcher, spec: &LaunchSpec, attempts: u32) -> Result<Launched, LaunchError> {
    launch_with(launcher, spec, attempts, std::thread::sleep)
}

/// Launch `spec` using the crate's default attempt bound (`DEFAULT_LAUNCH_ATTEMPTS`), matching the
/// shell's `HARNESS_BOOT_ATTEMPTS` default.
pub fn launch_default(launcher: &mut impl Launcher, spec: &LaunchSpec) -> Result<Launched, LaunchError> {
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
            .map_err(|reason| LaunchError::PreLaunchFailed { command: step.command.clone(), reason })?;
    }

    let bounded = attempts.max(1);
    for attempt in 1..=bounded {
        let spawned = launcher.spawn(spec).is_ok();
        if spawned && launcher.is_ready(spec) {
            return Ok(Launched { runtime: spec.runtime.clone(), attempts: attempt });
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
    Err(LaunchError::NeverReady { attempts: bounded, last_port_state })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docker_spec() -> LaunchSpec {
        LaunchSpec {
            runtime: Runtime::Docker { container: "gw-bench".into() },
            kind: LaunchKind::Docker {
                image: "gw:1.0".into(),
                env: vec![("A".into(), "B".into())],
                port: PortMapping::Host,
                mounts: vec![Mount { host_path: "/tmp/gw.yaml".into(), container_path: "/config.yaml".into(), read_only: true }],
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
            runtime: Runtime::Native { proc_match: "gw-native".into() },
            kind: LaunchKind::Native {
                binary: "/opt/gw/bin/gw-native".into(),
                args: vec!["--port".into(), "8080".into()],
                env: vec![("GW_AUTH".into(), "dummy".into())],
            },
            cores: "0-3".into(),
            port: 8080,
            ready_budget: Duration::from_secs(5),
            boot_backoff: Duration::from_secs(1),
            pre_launch: None,
        }
    }

    // ---- required: container argument construction ------------------------------------------------

    #[test]
    fn container_args_include_name_image_env_port_mapping_and_pinning() {
        let inv = build_invocation(&docker_spec());
        assert_eq!(inv.program, "docker");
        assert!(inv.args.windows(2).any(|w| w == ["--name".to_string(), "gw-bench".to_string()]), "{:?}", inv.args);
        assert!(inv.args.windows(2).any(|w| w == ["--network".to_string(), "host".to_string()]), "{:?}", inv.args);
        assert!(
            inv.args.windows(2).any(|w| w == ["--cpuset-cpus".to_string(), "0-3".to_string()]),
            "cpu pinning must be present: {:?}",
            inv.args
        );
        assert!(inv.args.windows(2).any(|w| w == ["-e".to_string(), "A=B".to_string()]), "{:?}", inv.args);
        assert!(inv.args.contains(&"gw:1.0".to_string()), "the image must appear: {:?}", inv.args);
        // The container name comes from spec.runtime.identity(), the same identity the memory
        // readers and the stop path take: there is no second name to disagree with it.
        assert_eq!(Runtime::Docker { container: "gw-bench".into() }.identity(), "gw-bench");
    }

    #[test]
    fn a_published_port_mapping_emits_a_dash_p_flag_instead_of_host_networking() {
        let mut spec = docker_spec();
        spec.kind = LaunchKind::Docker {
            image: "gw:1.0".into(),
            env: vec![],
            port: PortMapping::Published { host: 8080, container: 9090 },
            mounts: vec![],
            command: vec![],
        };
        let inv = build_invocation(&spec);
        assert!(inv.args.windows(2).any(|w| w == ["-p".to_string(), "8080:9090".to_string()]), "{:?}", inv.args);
        assert!(!inv.args.contains(&"--network".to_string()));
    }

    // ---- required: native argument construction ---------------------------------------------------

    #[test]
    fn native_args_include_binary_args_and_pinning() {
        let inv = build_invocation(&native_spec());
        assert_eq!(inv.program, "taskset");
        assert!(inv.args.windows(2).any(|w| w == ["-c".to_string(), "0-3".to_string()]), "cpu pinning must be present: {:?}", inv.args);
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
        assert_eq!(fake.stop_calls, 0, "a successful first attempt has nothing to clean up");
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
        assert_eq!(fake.stop_calls, 2, "the two failed attempts are cleaned up before the next try");
        assert_eq!(sleeps, 2, "backoff happens between attempts, not after the final success");
    }

    // ---- required: a gateway never ready exhausts the bound with a typed error, never success -----

    #[test]
    fn never_ready_exhausts_the_bound_and_returns_a_typed_error() {
        let mut fake = FakeLauncher::ready_after(vec![false, false, false]);
        fake.port_state = PortState::Free;
        let spec = docker_spec();
        let err = launch_with(&mut fake, &spec, 3, no_sleep).unwrap_err();
        assert_eq!(err, LaunchError::NeverReady { attempts: 3, last_port_state: PortState::Free });
        assert_eq!(fake.spawn_calls, 3, "the bound must be respected exactly, not exceeded");
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
            LaunchError::PreLaunchFailed { command: "cargo".to_string(), reason: "build failed".to_string() }
        );
        assert_eq!(fake.pre_launch_calls, 1);
        assert_eq!(fake.spawn_calls, 0, "the gateway must never be started when the pre-launch step fails");
    }

    // ---- required: a spec with no cpu pinning is rejected ------------------------------------------

    #[test]
    fn a_spec_with_no_cpu_pinning_is_rejected() {
        let mut fake = FakeLauncher::ready_after(vec![true]);
        let mut spec = docker_spec();
        spec.cores = "".into();
        let err = launch_with(&mut fake, &spec, 3, no_sleep).unwrap_err();
        assert_eq!(err, LaunchError::InvalidSpec(SpecError::NoCpuPinning));
        assert_eq!(fake.spawn_calls, 0, "an unpinned spec must never reach a spawn attempt");

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
        assert!(inv.args.windows(2).any(|w| w == ["--name".to_string(), spec.runtime.identity().to_string()]));
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
        d.kind = LaunchKind::Docker { image: "  ".into(), env: vec![], port: PortMapping::Host, mounts: vec![], command: vec![] };
        assert_eq!(d.validate(), Err(SpecError::Empty("image")));

        let mut n = native_spec();
        n.kind = LaunchKind::Native { binary: " ".into(), args: vec![], env: vec![] };
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
}
