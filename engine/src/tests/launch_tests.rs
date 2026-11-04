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

    reap_native_child(&mut slot);

    assert!(slot.is_none(), "the slot must be cleared once reaped");
    assert!(
        ps_state(pid).is_empty(),
        "the zombie must be gone from the process table after reaping, ps state was {:?}",
        ps_state(pid)
    );
}

// A LIVE CHILD MUST NOT HANG THE REAP. The callers reach the reap after `stop_and_wait`, which
// finds the gateway by `proc_match`, not by this handle - so a stop that missed it (a failed `ps`, a
// pattern that does not select the pid) or gave up on it used to be followed by a bare `wait()` on a
// process that was still running, and the whole run hung. The reap kills through the handle first.
#[test]
fn reaping_a_live_native_child_kills_it_rather_than_hanging() {
    let child = Command::new("/bin/sh")
        .args(["-c", "sleep 600"])
        .spawn()
        .expect("spawn a real process");
    let pid = child.id();
    let mut slot = Some(child);
    let started = Instant::now();
    reap_native_child(&mut slot);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the reap waited on a live process instead of killing it"
    );
    assert!(slot.is_none(), "the slot must be cleared once reaped");
    assert!(
        ps_state(pid).is_empty(),
        "the child must be gone, not left running or as a zombie, ps state was {:?}",
        ps_state(pid)
    );
}

// UNTRUSTED LOG TEXT REACHES THE OPERATOR'S TERMINAL. A gateway's own output travels inside
// `NeverReady` to stderr and the box's run log; an escape sequence in it must arrive as evidence, not
// as a command to the terminal.
#[test]
fn control_characters_in_a_gateway_log_are_escaped_not_interpreted() {
    let said = neutralise_control_chars("boot failed\x1b]0;owned\x07\x1b[2K\r\nline two\tok");
    assert!(!said.contains('\x1b'), "an ESC survived: {said:?}");
    assert!(!said.contains('\x07'), "a BEL survived: {said:?}");
    assert!(!said.contains('\r'), "a carriage return survived: {said:?}");
    assert!(
        said.contains("\\u{1b}"),
        "the escape is still visible as evidence: {said:?}"
    );
    assert!(
        said.contains("\nline two\tok"),
        "newlines and tabs stay readable: {said:?}"
    );
}

// ---- commands run in the gateway's own directory -----------------------------------------------

fn scratch(name: &str) -> std::path::PathBuf {
    // Shell-safe by construction: this path is interpolated into a `/bin/sh -c` line below, and
    // a thread id (`ThreadId(7)`) would put parentheses into it.
    let dir = std::env::temp_dir().join(format!("otb-launch-test-{name}-{}", std::process::id()));
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
    // The declared read-only mount is passed read-only, and the gateway's own arguments come AFTER
    // the image: before it, docker would parse them as its own flags.
    assert!(
        inv.args
            .windows(2)
            .any(|w| w == ["-v".to_string(), "/tmp/gw.yaml:/config.yaml:ro".to_string()]),
        "a read-only mount must be passed as :ro: {:?}",
        inv.args
    );
    let image_at = inv
        .args
        .iter()
        .position(|a| a == "gw:1.0")
        .expect("the image is present");
    assert_eq!(
        &inv.args[image_at + 1..],
        &["-f".to_string(), "/config.yaml".to_string()],
        "the command must follow the image, and nothing else may: {:?}",
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
    /// What each spawn returns, in order; once empty, every spawn succeeds.
    spawn_script: Vec<Result<(), String>>,
    /// What each port snapshot reads, in order; once empty, `port_state` is read.
    port_script: Vec<PortState>,
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
            spawn_script: Vec::new(),
            port_script: Vec::new(),
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
        if self.spawn_script.is_empty() {
            Ok(())
        } else {
            self.spawn_script.remove(0)
        }
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
        if self.port_script.is_empty() {
            self.port_state
        } else {
            self.port_script.remove(0)
        }
    }
}

fn no_sleep(_: Duration) {}

// ---- a listener this launch did not start is never its readiness ------------------------------

// THE DEFECT: readiness is "something answers on the port", and nothing checked the port was free
// first. A container a crashed run left on the host network kept answering, the fresh gateway died
// on EADDRINUSE, and `launch` returned `Launched` - so the whole grid measured the survivor under
// this launch's name. A port already held before the spawn is refused, every attempt, and the reason
// travels in the error.
#[test]
fn an_attempt_never_spawns_onto_a_port_something_else_already_holds() {
    let mut fake = FakeLauncher::ready_after(vec![true, true, true]);
    fake.port_state = PortState::Held;
    let err = launch_with(&mut fake, &docker_spec(), 3, no_sleep)
        .expect_err("a port held by something else must never read as this launch being ready");
    assert_eq!(
        fake.spawn_calls, 0,
        "nothing may be spawned onto a held port"
    );
    let LaunchError::NeverReady {
        attempts,
        last_port_state,
        last_spawn_error,
        ..
    } = &err
    else {
        panic!("expected NeverReady, got {err:?}");
    };
    assert_eq!(*attempts, 3);
    assert_eq!(*last_port_state, PortState::Held);
    assert!(
        last_spawn_error
            .as_deref()
            .is_some_and(|w| w.contains("already held")),
        "the refusal must say why: {err}"
    );
}

// The check is per attempt: a survivor that finally releases the port between attempts lets the
// next one spawn and come up normally.
#[test]
fn a_port_that_frees_up_between_attempts_is_used() {
    let mut fake = FakeLauncher::ready_after(vec![true]);
    fake.port_script = vec![PortState::Held, PortState::Free];
    let launched = launch_with(&mut fake, &docker_spec(), 3, no_sleep).expect("it comes up");
    assert_eq!(launched.attempts, 2);
    assert_eq!(
        fake.spawn_calls, 1,
        "only the attempt with a free port spawns"
    );
}

// A REFUSED SPAWN IS NEVER READY, even when the readiness probe would say yes: the `spawned &&`
// guard is what keeps a rejected `docker run` from being reported as a launch of whatever answers.
#[test]
fn a_refused_spawn_is_never_read_as_ready_even_if_the_port_answers() {
    let mut fake = FakeLauncher::ready_after(vec![true]);
    fake.spawn_script = vec![Err("name already in use".to_string())];
    fake.port_state = PortState::Free;
    let err = launch_with(&mut fake, &docker_spec(), 1, no_sleep)
        .expect_err("a refused spawn cannot be a launch");
    assert_eq!(
        err,
        LaunchError::NeverReady {
            attempts: 1,
            last_port_state: PortState::Free,
            last_spawn_error: Some("name already in use".to_string()),
            last_output: None,
        }
    );
}

// THE REASON IS THE LAST ATTEMPT'S. An earlier refusal used to survive a later clean spawn, so a
// gateway that started and then failed on its config was reported as the runtime refusing it.
#[test]
fn a_later_clean_spawn_clears_an_earlier_refusal() {
    let mut fake = FakeLauncher::ready_after(vec![false, false]);
    fake.spawn_script = vec![Err("name already in use".to_string()), Ok(())];
    fake.port_state = PortState::Free;
    let err = launch_with(&mut fake, &docker_spec(), 2, no_sleep).expect_err("never ready");
    let LaunchError::NeverReady {
        last_spawn_error, ..
    } = &err
    else {
        panic!("expected NeverReady, got {err:?}");
    };
    assert_eq!(
        *last_spawn_error, None,
        "the last attempt spawned cleanly, so no refusal may be blamed: {err}"
    );
}

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
