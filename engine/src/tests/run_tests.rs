/* THE DRAIN WAITS ON A CONDITION, AND THE CONDITION HAS TO BE REACHABLE.

A settle that can never be satisfied turns the 60s backstop into the schedule: 1,647 windows
above the free-below threshold would become 27 HOURS of waiting in a field run. The tolerance
is what keeps it reachable on a box that is never perfectly idle, so it is asserted directly. */
#[test]
fn the_drain_target_is_reachable_on_a_box_that_is_never_perfectly_idle() {
    // The baseline is the cell's own starting count, scaled - not a number chosen here. A box
    // that starts with 200 sockets in TIME_WAIT must not be asked to return to 0.
    let quiet = 200u64;
    let target = settle_target(quiet);
    assert!(
        target > quiet,
        "the target must sit ABOVE the level the cell started at"
    );
    // The mock and the load generator hold their own sockets throughout, so a window that closes
    // cleanly still leaves the box above where it began. That has to read as drained.
    assert!(
        quiet + 100 <= target,
        "ordinary residue still counts as drained"
    );
    // And a box still holding thousands of dying sockets from a c=8,192 window does NOT.
    assert!(
        quiet + 8_000 > target,
        "a real backlog is not mistaken for quiet"
    );

    // An idle box reads 0 and must still get a usable target rather than an impossible one.
    assert!(settle_target(0) > 0, "an idle box still has headroom");
    // And the arithmetic cannot wrap into an impossible target on an absurd reading.
    assert_eq!(settle_target(u64::MAX), u64::MAX);
}

/* AND THE BACKSTOP IS ONE TIME_WAIT GENERATION, which is the point of the number. It was 10s,
deliberately UNDER TIME_WAIT, and the board showed cells failing rungs they had already carried
after 4-5s of it. A closed socket cannot need longer than a generation; past that the host is
broken rather than busy. */
#[test]
fn the_settle_backstop_covers_a_full_time_wait_generation() {
    const {
        assert!(
            STREAM_SETTLE_MAX_MS >= 60_000,
            "a backstop under one TIME_WAIT generation cannot absorb the residue it exists for"
        )
    };
    // The poll has to be fine enough that a small rung's wait is milliseconds, not seconds: the
    // whole gain over the old fixed sleep is that a rung which drains at once stops paying.
    const {
        assert!(
            STREAM_SETTLE_POLL_MS <= 100,
            "a coarse poll would reintroduce the fixed cost this replaced"
        )
    };
}

/* THE FLOOR FALLBACK'S GATE, which decides whether a cell publishes a conservative number or an
absence. The fallback re-measures on the SAME host that just failed several rungs, so it is
only meaningful when the host was not the suspect - and the flattering direction of this error
(publishing a figure where honesty published none) is the one nobody would catch. */
#[test]
fn floor_fallback_only_where_the_host_is_not_the_suspect() {
    // The search ran out of room or budget: the gateway and the rig are both innocent so far, and
    // the rung the cell already carried is a real, conservative result.
    assert!(StreamStop::BudgetExhausted.floor_fallback_ok());
    assert!(StreamStop::FloorReached { last: 4193 }.floor_fallback_ok());
    assert!(StreamStop::SteppedRungFailed { at: 4193 }.floor_fallback_ok());

    // OUR instrument is the variable. A number taken on a host we have just accused would be
    // exactly the attribution error the enum exists to prevent.
    assert!(!StreamStop::RigRanShort {
        measured: 1,
        wanted: 3
    }
    .floor_fallback_ok());
    assert!(!StreamStop::WindowUnavailable { at: 4096 }.floor_fallback_ok());
    assert!(!StreamStop::RigContaminated {
        at: 4096,
        proven: 4096
    }
    .floor_fallback_ok());

    // And a gateway that had to be restarted is no longer the process the sweep measured, so
    // whatever a fresh one carries is not this cell's ceiling. "It does not recover" is the
    // finding and must not be overwritten by a number.
    assert!(!StreamStop::GatewayDidNotRecover {
        at: 8192,
        proven: 4096,
        restart_cleared: true
    }
    .floor_fallback_ok());
    assert!(!StreamStop::GatewayDidNotRecover {
        at: 8192,
        proven: 4096,
        restart_cleared: false
    }
    .floor_fallback_ok());
}

/* AND THE FLOOR STILL HAS TO EARN IT. The fallback publishes only what the same majority rule
every other repeated measurement uses would publish, so a floor that wins one window of three -
the shape that made the bisected rung unpublishable in the first place - stays unpublished. */
#[test]
fn the_floor_is_published_only_on_the_same_majority_every_other_rate_needs() {
    assert!(stream_ceiling_confirmed(3, 3), "three of three holds");
    assert!(stream_ceiling_confirmed(3, 2), "two of three is a majority");
    assert!(
        !stream_ceiling_confirmed(3, 1),
        "one of three is the busbar c=5,652 shape"
    );
    assert!(
        !stream_ceiling_confirmed(3, 0),
        "none of three publishes nothing"
    );
    assert!(
        !stream_ceiling_confirmed(2, 2),
        "a short window set is not a confirmation"
    );
}

#[test]
fn the_stream_bound_is_physical_and_the_runaway_cap_never_participates() {
    // A CAP CHOSEN NEAR WHERE MEASUREMENTS LIVE BECOMES PART OF THE MEASUREMENT. The first version
    // of this bound was a constant 4096, picked because no gateway had cleanly exceeded c=2178.
    // The field's own sweeps said otherwise - apisix sustained c=16384 with ZERO stalls and every
    // window passing, litellm-rust c=6144, aisix c=4096 - so it would have clipped three gateways
    // and published a smaller rung as their peak. A wrong number is worse than an honest hole.
    //
    // The real stopping condition is measured (see
    // `search::a_ladder_that_ends_on_failing_rungs_publishes_the_best_passing_rung`). The cap here
    // is only a runaway backstop, and these assertions are what keep it from quietly becoming
    // anything more.
    let bench_box_fds = 1_048_576;
    let bench_box_ports = 32_768;
    assert_eq!(
        super::stream_ceiling_from(bench_box_fds, bench_box_ports),
        bench_box_ports,
        "the PHYSICAL bound must decide it - with descriptors raised far above the port range, \
             the port range is the answer and the runaway cap must not be visible in it"
    );
    // WHAT THE BACKSTOP IS FOR, asserted through the function rather than against the constant.
    // This was `STREAM_RUNAWAY_CAP >= 4 * 16_384`, which clippy correctly called a constant
    // assertion: both sides are compile-time literals, so it could never fail at runtime - a dead
    // gate in the very test written to keep a cap honest. Driving the derivation with no physical
    // limit at all shows the backstop is the thing that stops it, and that it sits several
    // doublings above the highest rung the field has cleanly sustained (apisix, c=16384).
    let unbounded_host = super::stream_ceiling_from(u32::MAX, u32::MAX);
    assert_eq!(
        unbounded_host,
        super::STREAM_RUNAWAY_CAP,
        "with no physical limit the runaway backstop must be what bounds the ladder"
    );
    assert!(
        unbounded_host >= 4 * 16_384,
        "the backstop must sit far above the highest rung the field has cleanly sustained \
             (apisix, c=16384), or it is a measurement bound wearing a safety label: {unbounded_host}"
    );
    // Descriptors still bind when they are genuinely the smaller number: a stock 1024-descriptor
    // box must not be asked for thousands of held-open streams, each costing one descriptor on the
    // rig, one on the gateway and one on the mock.
    assert!(
        super::stream_ceiling_from(1024, bench_box_ports) <= 512,
        "a stock descriptor limit is a real bound and must still bite"
    );
    let streams = super::stream_connection_ceiling();
    assert!(
        streams >= 1,
        "a ceiling of zero would measure nothing: {streams}"
    );
    assert!(
        streams.is_power_of_two(),
        "the ladder doubles, so a ceiling off the ladder is never actually reached: {streams}"
    );
}

// A REFERENCE MUST REST ON AS MANY WINDOWS AS THE NUMBER IT JUDGES.
//
// `stream_fps_at` takes WINDOWS_PER_RUNG windows and medians the clean ones - but `stats::median`
// returns a value whenever its input is non-empty, so ONE clean window out of three still produced
// a published reference. That is the single-sample bar the median was added to remove, still live
// inside the fix for it.
//
// The direction is what makes it costly: an UNDERSTATED reference is what suppresses a gateway's
// real number. The observation then clears it by more than IMPOSSIBLE_FACTOR and a measured figure
// is discarded as unvouchable - which is how seven gateway/metric pairs on the 2026-07-29 board
// published nothing at all.
#[test]
fn a_rig_reference_needs_a_full_set_of_clean_windows() {
    let want = crate::search::WINDOWS_PER_RUNG;
    // The rule `stream_fps_at` applies: fewer clean windows than a rung is measured with means no
    // reference, however many of them happened to be fast.
    assert!(
        !reference_has_full_set(1),
        "one clean window is the bar the median was supposed to raise"
    );
    assert!(
        !reference_has_full_set(want - 1),
        "one short is still short of what the observation rests on"
    );
    assert!(
        reference_has_full_set(want),
        "a full set is the same bar every other rate on the board meets"
    );
    assert!(reference_has_full_set(want + 1), "and more is fine");
}

// A PACING INTERVAL OF ZERO WOULD FAIL EVERY STREAM ON THE BOARD.
//
// `stall_bound_us()` is this interval times STREAM_STALL_MULTIPLIER, so a parsed 0 makes the stall
// bound 0 and EVERY inter-frame gap counts as a stall - including a gap of nothing. Both stream
// metrics would then fail on every rung of every cell, and the artifact would read that as thirteen
// gateways going quiet rather than as one bad environment variable.
#[test]
fn a_zero_pacing_interval_is_refused_in_favour_of_the_default() {
    // Driven through the parser `stream_pacing_interval_ms` itself uses, over raw values, so the
    // process-global the rest of the suite reads is never touched.
    let read = |raw: &str| pacing_interval_from(Some(raw));
    assert_eq!(
        pacing_interval_from(None),
        20,
        "unset is the mock's own default"
    );
    assert_eq!(
        read("0"),
        20,
        "zero is not a pace and must fall back to the default"
    );
    assert_eq!(read("  0  "), 20, "including with whitespace around it");
    assert_eq!(read("40"), 40, "a real value is still honoured");
    assert_eq!(
        read(" 40 "),
        40,
        "and still trimmed, matching the mock's own reader"
    );
    assert_eq!(
        read("nonsense"),
        20,
        "and an unparseable value still defaults"
    );
    assert!(
        super::stream_pacing_interval_ms() > 0,
        "the live reader can never return zero"
    );
}

// ONE LUCKY WINDOW MUST NOT BECOME A CONFIRMED STREAM CEILING.
//
// The confirmation loop opens `held = 1, total = 1` because the bisection's own winning window is
// a real vote, then takes WINDOWS_PER_RUNG-1 repeats. A repeat that cannot RUN is skipped WITHOUT
// incrementing `total`, so two absent repeats left 1 of 1 - and the majority test alone reads that
// as a pass, publishing the single unrepeated window as the ceiling plus its own raw fps as
// streams_sustained_fps. Both absent paths (RigExhausted on ports/descriptors, a panicked lane)
// bite hardest at the top rung, which is exactly where the published number comes from.
//
// `confirm_ceiling` refuses the same input on the throughput side and explains that one lucky
// window is how a sustained figure lands ABOVE the peak from the same sweep. The stream copy kept
// the majority half and dropped the minimum-window half, while its comment claimed to run "the
// same majority rule" - true only of the half that survived.
#[test]
fn a_stream_ceiling_needs_as_many_windows_as_any_other_rung() {
    let n = crate::search::WINDOWS_PER_RUNG;
    assert!(
        !super::stream_ceiling_confirmed(1, 1),
        "1 of 1 is the bisection's own window with both repeats absent - not a confirmation"
    );
    assert!(
        !super::stream_ceiling_confirmed(2, 2),
        "2 of 2 is still short of a climb rung: one absent repeat must not lower the bar"
    );
    // A full set that genuinely held publishes.
    assert!(
        super::stream_ceiling_confirmed(n, n),
        "every window held: publish"
    );
    // A full set with a real majority publishes; a real minority does not. This is the half that
    // already worked, asserted so a fix to the other half cannot quietly remove it.
    assert!(
        super::stream_ceiling_confirmed(3, 2),
        "2 of 3 held is a majority"
    );
    assert!(!super::stream_ceiling_confirmed(3, 1), "1 of 3 held is not");
}

// A WINDOW THAT LOST LANES MUST BE DISCARDED, LOUDLY.
//
// This rule cost a four-hour run every cpu_fps number it should have produced, and left nothing
// to diagnose it with. A panicked lane was dropped on the floor - no count, no message - and
// when every lane went, `streams == 0` returned `None` through the ONE refusal in that function
// that said nothing. The visible symptom was a metric that took 0.0s and published an absence,
// on every streamable cell, with no line anywhere in the log or the artifact explaining why.
//
// The subtler half is why survivors are not good enough: `streams` and `expected_frames` are
// both accumulated per surviving lane, so a panicked lane leaves the numerator AND the
// denominator. A window that lost half its lanes reported the survivors' delivery ratio as the
// whole window's and passed the gate on it - flattered by the removal of its own failures.
#[test]
fn a_window_that_lost_lanes_is_refused_and_says_so() {
    let why = super::window_refusal(4, 60, 64).expect("lost lanes must refuse the window");
    assert!(
        why.contains("PANICKED"),
        "the refusal must name the fault: {why}"
    );
    assert!(
        why.contains('4') && why.contains("64"),
        "and must quantify it - how many of how many: {why}"
    );

    // Even ONE lost lane. The survivors' ratio is not this window's ratio, and there is no
    // threshold below which a measurement may quietly describe a different window than it ran.
    assert!(
        super::window_refusal(1, 63, 64).is_some(),
        "one lost lane still means the window did not run at the concurrency it claims"
    );

    // Nothing came back at all, nothing panicked: unmeasured, and it must still SAY so rather
    // than returning in silence the way the old path did.
    let why = super::window_refusal(0, 0, 8).expect("an empty window must refuse");
    assert!(why.contains("no streams"), "{why}");

    // And the case that must still be PUBLISHED: every lane came home. A refusal rule that
    // refuses everything is as useless as one that refuses nothing.
    assert_eq!(
        super::window_refusal(0, 64, 64),
        None,
        "a whole window is a real measurement and must not be thrown away"
    );
    assert_eq!(super::window_refusal(0, 1, 1), None);
}

use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

pub(super) fn cfg_for(gw: SocketAddr, mock: SocketAddr) -> RunConfig {
    test_fixture(gw, mock)
}

// ---- restart_to_rest must not leak the process it replaces --------------------------------

/// `ps -o state=` reports a zombie as `Z` on both Linux and macOS. Empty output (or a nonzero
/// exit) means the OS has no process table entry for that pid at all.
fn ps_state(pid: u32) -> String {
    std::process::Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// A native "gateway" that actually listens, so `wait_until_ready` (real TCP, inside
/// `restart_to_rest`) has something honest to observe - a marker unique to this test process so
/// `pkill -f`/`pgrep -f` can never match an unrelated process on a shared runner.
fn listening_native_spec(port: u16, marker: &str) -> crate::launch::LaunchSpec {
    let script = "import socket,sys
s=socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('127.0.0.1', int(sys.argv[1])))
s.listen(1)
while True:
    conn, _ = s.accept()
    conn.close()
";
    crate::launch::LaunchSpec {
        runtime: crate::manifest::Runtime::Native {
            proc_match: marker.to_string(),
        },
        kind: crate::launch::LaunchKind::Native {
            binary: "python3".into(),
            args: vec![
                "-c".into(),
                script.into(),
                port.to_string(),
                marker.to_string(),
            ],
            env: vec![],
            env_unset: vec![],
        },
        cores: "0".into(),
        port,
        ready_budget: Duration::from_secs(5),
        boot_backoff: Duration::from_millis(100),
        pre_launch: None,
    }
}

// THE DEFECT THIS GUARDS AGAINST. `restart_to_rest` used to build a throwaway `RealLauncher` on
// every call, so the `Child` it spawned was dropped the moment the function returned. The NEXT
// restart's `pkill` then killed a process nothing could `wait()` on: a zombie, once per served
// cell, for the whole run. Two restarts against the SAME persistent launcher must not leave the
// FIRST process a zombie once the second has replaced it.
#[test]
fn restart_to_rest_reaps_the_process_it_replaces() {
    // `build_invocation` pins a native gateway with `taskset`, which is Linux-only. The field
    // runs on Linux and so does CI, so this exercises the real launch path there; on a
    // developer's macOS box there is nothing to pin with and the launch would fail for a reason
    // that has nothing to do with reaping. Say so out loud rather than failing on it: a test
    // that reports red for the wrong reason gets ignored, and an ignored test is not a gate.
    if std::process::Command::new("sh")
        .args(["-c", "command -v taskset"])
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping restart_to_rest_reaps_the_process_it_replaces: no taskset on this platform (the field and CI are Linux, where this runs for real)");
        return;
    }
    let marker = format!("otb-test-restart-to-rest-{}", std::process::id());
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port to pick one");
        l.local_addr().expect("addr").port()
    };
    let spec = listening_native_spec(port, &marker);
    let launcher: std::sync::Mutex<crate::launch::RealLauncher> = Default::default();

    restart_to_rest(&spec, &launcher, &[]).expect("the first restart must come up");
    let pid1 = launcher
        .lock()
        .expect("lock")
        .native_pid()
        .expect("a native child must be tracked");

    restart_to_rest(&spec, &launcher, &[]).expect("the second restart must come up");

    assert!(
        !ps_state(pid1).contains('Z'),
        "the process the second restart replaced must be reaped, not left as a zombie; ps state was {:?}",
        ps_state(pid1)
    );

    let _ = crate::supervise::stop_and_wait(&spec.runtime, spec.port, Duration::from_secs(5));
}

// THE UNCONFIGURED-RELAUNCH DEFECT THIS GUARDS AGAINST. A gateway with no config file is
// configured through its own admin API by the manifest's `commands`, run once after the initial
// launch. For docker a stop is `docker rm -f`, so the database those commands wrote dies with
// the container - and `restart_to_rest` relaunched WITHOUT replaying them. On the 2026-07-28
// board one-api lost its three channels at the memory group's restart: throughput (measured
// before it) published real numbers, and streaming + added latency (measured after it) failed
// 100%, publishing the missing configuration as the gateway's own failure. The restart must
// replay the commands, and a command that fails must fail the restart: a half-configured
// gateway answering probes is worse than a down one.
#[test]
fn restart_to_rest_replays_the_post_boot_commands_and_fails_when_they_do() {
    if std::process::Command::new("sh")
        .args(["-c", "command -v taskset"])
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping restart_to_rest_replays_the_post_boot_commands_and_fails_when_they_do: no taskset on this platform");
        return;
    }
    let marker = format!("otb-test-restart-commands-{}", std::process::id());
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port to pick one");
        l.local_addr().expect("addr").port()
    };
    let spec = listening_native_spec(port, &marker);
    let launcher: std::sync::Mutex<crate::launch::RealLauncher> = Default::default();

    // A witness file only the replayed command writes: its existence IS the replay.
    let witness = std::env::temp_dir().join(format!("{marker}.witness"));
    let _ = std::fs::remove_file(&witness);
    let write = format!("touch {}", witness.display());
    restart_to_rest(&spec, &launcher, &[write])
        .expect("a restart whose post-boot command succeeds must come up");
    assert!(
        witness.exists(),
        "the restart must REPLAY the post-boot commands, not just relaunch the process"
    );
    let _ = std::fs::remove_file(&witness);

    // And a failing command is the restart failing, loudly, naming the command.
    let err = restart_to_rest(&spec, &launcher, &["false".to_string()])
        .expect_err("a failing post-boot command must fail the restart");
    assert!(
        err.contains("post-boot configuration failed"),
        "the error must name the configuration stage, got: {err}"
    );

    let _ = crate::supervise::stop_and_wait(&spec.runtime, spec.port, Duration::from_secs(5));
}

// THE COLLAPSED EGRESS AXIS THIS PREVENTS. Most gateways choose the upstream from the model name
// in the request, so a grid that sends one fixed model sends a byte-identical request for all six
// egress columns of a row. Every column reaches the SAME upstream, and the artifact publishes six
// translation cells for one measurement the gateway was never asked to perform. The axis reads as
// measured and is not.
#[test]
fn every_egress_column_asks_for_its_own_model() {
    let a = "127.0.0.1:1".parse().expect("addr");
    let mut cfg = test_fixture(a, a);
    cfg.model = "canonical".into();
    cfg.egress_models = [("anthropic".to_string(), "claude-x".to_string())]
        .into_iter()
        .collect();

    assert_eq!(
        model_for(&cfg, "anthropic"),
        "claude-x",
        "the column's own name must be sent"
    );
    // An egress the manifest names nothing for falls back to the declared model: right for a
    // single-upstream gateway, and for the column whose canonical name already IS that model.
    assert_eq!(model_for(&cfg, "openai"), "canonical");
}

// The model rides in the PATH for two of the six dialects, so a per-egress model that never
// reached `path_for` would still collapse those columns even with the body correct.
#[test]
fn a_path_that_embeds_the_model_embeds_the_egress_columns_model() {
    let a = "127.0.0.1:1".parse().expect("addr");
    let mut cfg = test_fixture(a, a);
    cfg.model = "canonical".into();
    cfg.egress_models = [("bedrock".to_string(), "vendor.model-v1:0".to_string())]
        .into_iter()
        .collect();

    let p = path_for(&cfg, Dialect::Bedrock, "bedrock");
    assert!(
        p.contains("vendor.model-v1:0"),
        "bedrock's path must carry its own column's model: {p}"
    );
    assert!(
        !p.contains("canonical"),
        "the declared model must not leak into another column: {p}"
    );
}

/// An SSE "gateway" whose behaviour is scripted per CONNECTION: the `pass` predicate is handed
/// the 1-based accept-order index of each connection, and a passing connection streams the full
/// frame budget while a failing one delivers a single frame and closes (a delivery shortfall,
/// which fails the streams gate without counting as an errored stream). Windows run one after
/// another and every lane of a window connects before the next window starts, so accept order
/// maps connections to windows deterministically.
fn sse_ladder_server(pass: impl Fn(usize) -> bool + Send + Sync + 'static) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let pass = std::sync::Arc::new(pass);
    std::thread::spawn(move || {
        let mut conn_no = 0usize;
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            conn_no += 1;
            let n = conn_no;
            let pass = std::sync::Arc::clone(&pass);
            std::thread::spawn(move || {
                let mut b = [0u8; 8192];
                let _ = c.read(&mut b);
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n");
                // Openai-shaped, like `serve_sse` and like the mock: a role head that carries no
                // token, then content deltas. The delivery gate counts CONTENT frames, so a
                // ladder of bare `data: f0` events would fail every rung for the wrong reason.
                let _ = c.write_all(
                    b"data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                );
                let frames = if pass(n) {
                    crate::metric::STREAM_FRAME_BUDGET
                } else {
                    1
                };
                for i in 0..frames {
                    // STOP AT THE FIRST WRITE THAT FAILS. A dropped lane times out per write, so
                    // pushing all 64 frames anyway held the lane ~16s (64 x 250ms) and the
                    // in-flight count never came back down - the simulator's own backlog, read as
                    // the gateway being saturated.
                    if c.write_all(
                        format!(
                            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"f{i}\"}}}}]}}\n\n"
                        )
                        .as_bytes(),
                    )
                    .is_err()
                    {
                        return;
                    }
                }
                // Then close: a passing lane has already hit the client's frame budget, and a
                // failing lane's early close is what makes the shortfall visible at once.
            });
        }
    });
    addr
}

#[test]
fn a_stepped_down_stream_rung_whose_fresh_window_fails_never_votes_for_itself() {
    let gw = sse_ladder_server(|n| n <= 3 || n >= 16);
    let cfg = cfg_for(gw, gw);
    let id = CellId::new("openai", "openai");
    let r = sweep_streams_cell(&cfg, &id, 1, 4);
    assert_ne!(
        r.concurrency.value().copied(),
        Some(1),
        "the stepped-down rung must not publish on the strength of its own failing seed window"
    );
    // This fixture is scripted by ACCEPT ORDER, not by concurrency. The ascent
    // carries c=2, but both confirmation windows at c=2 fail. Then c=1's new
    // seed fails. The floor already lost its full vote (`floor_refuted`), so a
    // second confirmation would cherry-pick the later healthy connections.
    assert!(
        r.concurrency.value().is_none(),
        "a refuted floor must not be re-voted into a ceiling"
    );
    assert!(r.fps.value().is_none());
    let floor_votes: Vec<bool> = r
        .points
        .iter()
        .filter(|p| p.concurrency == 2)
        .map(|p| p.passed)
        .collect();
    assert_eq!(floor_votes, vec![true, false, false]);
    let last = r
        .points
        .last()
        .expect("the failed step-down seed is retained");
    assert_eq!(last.concurrency, 1);
    assert!(!last.passed);
}

/// A server that answers every request with a fixed status.
fn serve(status: u16) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                while c.read(&mut b).unwrap_or(0) > 0 {
                    let r = format!("HTTP/1.1 {status} X\r\ncontent-length: 2\r\n\r\nok");
                    if c.write_all(r.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    addr
}

#[test]
fn a_2xx_is_served() {
    let gw = serve(200);
    let cfg = cfg_for(gw, gw);
    assert_eq!(
        probe_cell(&cfg, &CellId::new("openai", "openai"), true),
        Served::Yes
    );
}

// A real error status with a healthy rig is the GATEWAY's own answer about this pairing.
#[test]
fn a_real_error_status_with_a_healthy_rig_is_the_gateways_answer() {
    let gw = serve(404);
    let cfg = cfg_for(gw, gw);
    let s = probe_cell(&cfg, &CellId::new("openai", "openai"), true);
    assert!(matches!(s, Served::No(..)), "got {s:?}");
}

// 404 and a genuine rejection (e.g. 401) must NOT carry the same verdict end to end through
// probe_cell: a 404 means the pairing does not exist; a 401 means the gateway reached this
// pairing and declined the specific request. Collapsing them would publish a gateway that
// supports a pairing but rejects the probe's auth as indistinguishable from one that never
// built the route at all.
#[test]
fn a_not_found_and_a_rejection_carry_different_verdicts_through_probe_cell() {
    let not_found = serve(404);
    let cfg = cfg_for(not_found, not_found);
    let s = probe_cell(&cfg, &CellId::new("openai", "openai"), true);
    assert!(
        matches!(s, Served::No(ref v, _) if *v == crate::probe::Verdict::NotConfigured),
        "404 must be NotConfigured, got {s:?}"
    );

    let rejected = serve(401);
    let cfg = cfg_for(rejected, rejected);
    let s = probe_cell(&cfg, &CellId::new("openai", "openai"), true);
    assert!(
        matches!(s, Served::No(ref v, ref ev) if *v == crate::probe::Verdict::Failed && ev.status == 401),
        "401 must be Failed with the real status carried as evidence, got {s:?}"
    );
}

// THE RED WE WOULD NOT HAVE EARNED. On a dialect whose real clients sign their requests, the
// harness sends a bearer token and refuses to forge a signature, so a gateway that checks
// credentials properly answers 401/403 - correctly. Publishing that as a refusal would state
// that somebody's product does not support a pairing, on evidence that is entirely about our own
// instrument. The SAME status on a dialect we CAN authenticate stays a real refusal, because
// there the gateway is answering about the request rather than about our credential.
#[test]
fn a_refusal_of_a_credential_the_rig_cannot_sign_is_unprobed_never_a_failure() {
    for status in [401, 403] {
        let gw = serve(status);
        let cfg = cfg_for(gw, gw);

        let s = probe_cell(&cfg, &CellId::new("bedrock", "bedrock"), true);
        assert!(
            matches!(s, Served::UnprobedAuth(ref ev) if ev.status == status),
            "a signed dialect's {status} must be unprobed with its evidence, got {s:?}"
        );

        let s = probe_cell(&cfg, &CellId::new("openai", "openai"), true);
        assert!(
            matches!(s, Served::No(ref v, _) if *v == crate::probe::Verdict::Failed),
            "a dialect the rig CAN authenticate keeps {status} as a real refusal, got {s:?}"
        );
    }
}

// The SAME status with an unhealthy rig says nothing about the gateway, so it must not be
// recorded as the gateway refusing. This is the rig/gateway distinction the board rests on.
#[test]
fn the_same_status_with_an_unhealthy_rig_is_not_blamed_on_the_gateway() {
    let gw = serve(404);
    let cfg = cfg_for(gw, gw);
    let s = probe_cell(&cfg, &CellId::new("openai", "openai"), false);
    assert!(
        !matches!(s, Served::No(..)),
        "an unconfirmed rig cannot convict the gateway: {s:?}"
    );
}

// Nothing listening is never the gateway's fault: it may never have been reached.
#[test]
fn an_unreachable_gateway_is_untestable_not_unserved() {
    let dead: SocketAddr = "127.0.0.1:1".parse().expect("literal");
    let cfg = cfg_for(dead, dead);
    let s = probe_cell(&cfg, &CellId::new("openai", "openai"), true);
    assert!(matches!(s, Served::Untestable(_)), "got {s:?}");
}

#[test]
fn an_unknown_dialect_is_untestable_rather_than_a_default_path() {
    let gw = serve(200);
    let cfg = cfg_for(gw, gw);
    let s = probe_cell(&cfg, &CellId::new("nonsense", "openai"), true);
    assert!(matches!(s, Served::Untestable(_)));
}

#[test]
fn mock_health_is_measured_not_assumed() {
    let up = serve(200);
    assert!(mock_healthy(&cfg_for(up, up)));
    let dead: SocketAddr = "127.0.0.1:1".parse().expect("literal");
    assert!(!mock_healthy(&cfg_for(dead, dead)));
}

// Every pairing appears, served or not. A dropped row hides a failure.
#[test]
fn the_grid_records_every_pairing() {
    let gw = serve(200);
    let mut cfg = cfg_for(gw, gw);
    cfg.dialects = vec![Dialect::Openai, Dialect::Anthropic];
    // An explicit, empty metric list: this test is about the SHAPE of the grid, so it must not
    // pay for every real measurement to assert that every pairing appears.
    let rows = run_grid_with(&cfg, 1, 2, &[]);
    assert_eq!(rows.len(), 4);
}

/// A relaunch spec that cannot come back up: its stop path matches no process (so stopping
/// "succeeds" instantly) and its binary does not exist, so `restart_to_rest` fails fast on any
/// platform - no taskset gating needed, because the FAILURE path is what is under test.
fn unlaunchable_spec(marker: &str) -> crate::launch::LaunchSpec {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port to pick one");
        l.local_addr().expect("addr").port()
    };
    crate::launch::LaunchSpec {
        runtime: crate::manifest::Runtime::Native {
            proc_match: marker.to_string(),
        },
        kind: crate::launch::LaunchKind::Native {
            binary: "/nonexistent-otb-gateway-binary".into(),
            args: vec![marker.to_string()],
            env: vec![],
            env_unset: vec![],
        },
        cores: "0".into(),
        port,
        ready_budget: Duration::from_millis(200),
        boot_backoff: Duration::from_millis(10),
        pre_launch: None,
    }
}

// THE UNVOUCHED-PROCESS DEFECT THIS PINS. A mid-grid restart that FAILED used to be one eprintln
// and business as usual: the loop went on probing a gateway whose stop may have half succeeded
// and whose relaunch may be up half-configured, so every later cell's failure published as the
// gateway's own. After a failed restart, every remaining cell must be recorded untestable with a
// detail naming the HARNESS's failure, and nothing after it may be probed or measured.
#[test]
fn a_failed_mid_grid_restart_marks_every_remaining_cell_untestable_naming_the_harness() {
    // Nothing listens at the gateway address, so the first cell's probe ends "no connection",
    // which is exactly the verdict that triggers the restart attempt; the spec then cannot
    // launch, so the restart fails and the grid is poisoned from cell 2 onward.
    let dead: SocketAddr = "127.0.0.1:1".parse().expect("literal");
    let mock = serve(200);
    let mut cfg = cfg_for(dead, mock);
    cfg.dialects = vec![Dialect::Openai, Dialect::Anthropic];
    let marker = format!("otb-test-grid-poison-{}", std::process::id());
    cfg.relaunch = Some(unlaunchable_spec(&marker));

    let rows = run_grid_with(&cfg, 1, 2, &[]);
    assert_eq!(rows.len(), 4, "a poisoned grid still records every pairing");

    // The cell that observed the failure keeps its own honest verdict.
    match &rows[0].outcome.served {
        Served::Untestable(why) => assert!(
            why.contains("no connection"),
            "the first cell keeps the probe's own verdict, got: {why}"
        ),
        other => panic!("the first cell must be untestable, got {other:?}"),
    }
    // Every cell after it is untestable because of OUR restart, and says so.
    for row in &rows[1..] {
        match &row.outcome.served {
            Served::Untestable(why) => {
                assert!(
                    why.contains("restart failed"),
                    "{}: a poisoned cell must name the failed restart, got: {why}",
                    row.outcome.id
                );
                assert!(
                    why.contains("vouch"),
                    "{}: the detail must say the harness cannot vouch for the gateway, got: {why}",
                    row.outcome.id
                );
            }
            other => panic!(
                "{}: every cell after a failed restart must be untestable, got {other:?}",
                row.outcome.id
            ),
        }
        assert!(
            row.metrics.is_none(),
            "{}: a poisoned cell must never be measured",
            row.outcome.id
        );
    }
}

// A cell the manifest declares OUT of its capability grid must never be probed at all, even when
// the server sitting behind it would happily answer 200: the declaration wins, unconditionally,
// because probing it anyway and grading whatever came back is exactly the defect this field
// exists to prevent (a global gate answering for a pairing that was never really asked).
#[test]
fn a_declared_incapable_cell_is_never_probed_even_when_the_server_would_serve_it() {
    let gw = serve(200);
    let mut cfg = cfg_for(gw, gw);
    cfg.dialects = vec![Dialect::Openai, Dialect::Anthropic];
    // Rows = ingress, cols = egress, axis order [openai, openai-responses, anthropic, gemini,
    // cohere, bedrock]: openai->openai capable, openai->anthropic not; anthropic row all not.
    cfg.matrix = vec![
        "100000".into(),
        "000000".into(),
        "000000".into(),
        "000000".into(),
        "000000".into(),
        "000000".into(),
    ];
    cfg.matrix_note = "test: declared capability".into();
    let rows = run_grid_with(&cfg, 1, 2, &[]);
    assert_eq!(
        rows.len(),
        4,
        "every pairing still appears, declared or not"
    );

    let openai_openai = rows
        .iter()
        .find(|r| r.outcome.id.ingress == "openai" && r.outcome.id.egress == "openai")
        .unwrap();
    assert_eq!(
        openai_openai.outcome.served,
        Served::Yes,
        "the declared-capable cell was actually probed"
    );

    for r in rows
        .iter()
        .filter(|r| !(r.outcome.id.ingress == "openai" && r.outcome.id.egress == "openai"))
    {
        assert!(
            matches!(r.outcome.served, Served::NotConfigurable(_)),
            "{} is outside the declared grid and must read not_configurable without ever being probed, got {:?}",
            r.outcome.id, r.outcome.served
        );
    }
}

// An unserved cell carries NO metrics. A number attached to a pairing the gateway does not serve
// is a number about nothing.
#[test]
fn an_unserved_cell_carries_no_metrics() {
    let gw = serve(404);
    let cfg = cfg_for(gw, gw);
    let rows = run_grid_with(&cfg, 1, 2, &[]);
    for r in &rows {
        assert!(
            r.metrics.is_none(),
            "{} must not carry metrics",
            r.outcome.id
        );
    }
}
// WHICH URL A CELL IS DRIVEN AT, in precedence order.
//
// Two real gateways mount their compatible API somewhere other than the dialect's standard path,
// and the probe ignored the manifest and used the standard one. Both answered a truthful 404 on
// every cell and the artifact published them as serving nothing at all: a false claim about
// somebody's product, produced entirely by us.
//
// A per-cell entry exists for the gateways that route a same-dialect request differently from a
// translating one. It is keyed by the full cell, so choosing it is a deliberate, visible act in
// that gateway's data rather than something the engine infers.
#[test]
fn a_cell_is_driven_at_its_own_path_then_the_declared_one_then_the_standard() {
    let mut cfg = cfg_for(
        "127.0.0.1:1".parse().unwrap(),
        "127.0.0.1:2".parse().unwrap(),
    );

    // Nothing declared: the dialect's standard path.
    assert_eq!(
        path_for(&cfg, Dialect::Openai, "openai"),
        "/v1/chat/completions"
    );

    // A declared path that is a longer form of the standard one applies to that dialect, and to
    // that dialect only: a gateway mounting its OpenAI API under a prefix has not moved anyone
    // else's API.
    cfg.declared_path = "/openai/v1/chat/completions".to_string();
    assert_eq!(
        path_for(&cfg, Dialect::Openai, "anthropic"),
        "/openai/v1/chat/completions"
    );
    assert_eq!(
        path_for(&cfg, Dialect::Anthropic, "anthropic"),
        "/v1/messages"
    );

    // A cell's own path wins over both, and ONLY for that cell. The neighbouring cell in the
    // same row keeps the declared path, which is what stops one entrant being measured on a
    // provider-pinned route while the rest of its row is measured on the unified one.
    cfg.cell_paths
        .insert("openai>openai".to_string(), "/passthrough".to_string());
    assert_eq!(path_for(&cfg, Dialect::Openai, "openai"), "/passthrough");
    assert_eq!(
        path_for(&cfg, Dialect::Openai, "anthropic"),
        "/openai/v1/chat/completions"
    );
}

// ── sustained_gate_passes: the README's own "p99 under the ceiling AND <0.1% error rate" ──────

#[test]
fn a_clean_window_comfortably_under_the_ceiling_passes() {
    assert!(sustained_gate_passes(Some(5_000), 10_000, 0));
}

#[test]
fn a_window_at_or_over_the_p99_ceiling_fails_even_with_zero_errors() {
    assert!(
        !sustained_gate_passes(Some(SUSTAINED_P99_CEILING_US), 10_000, 0),
        "the ceiling itself must not pass"
    );
    assert!(!sustained_gate_passes(
        Some(SUSTAINED_P99_CEILING_US + 1),
        10_000,
        0
    ));
    assert!(
        sustained_gate_passes(Some(SUSTAINED_P99_CEILING_US - 1), 10_000, 0),
        "just under the ceiling passes"
    );
}

#[test]
fn no_p99_reading_never_passes_regardless_of_the_error_rate() {
    assert!(
        !sustained_gate_passes(None, 10_000, 0),
        "an unmeasured latency has not earned the latency half of the gate"
    );
}

#[test]
fn the_error_rate_boundary_is_exclusive_of_the_one_in_a_thousand_bar() {
    // Exactly the README's bar (0.1% = 1/1000) must NOT pass: "under" is strict.
    assert!(
        !sustained_gate_passes(Some(1_000), 999, 1),
        "exactly 1/1000 failing is the boundary itself"
    );
    // One request short of the boundary sample (1 failure in 1001) is comfortably under 0.1% and
    // must pass.
    assert!(sustained_gate_passes(Some(1_000), 1_000, 1));
    // A single failure against a tiny sample is a large fraction and must fail.
    assert!(!sustained_gate_passes(Some(1_000), 9, 1));
}

// ── the streams-sustained gate: the README's own three-part rule ─────────────────────────────

/// A window that passes everything, as a starting point each test below breaks in one way.
fn clean_stream_window(concurrency: u32, streams: u64) -> StreamWindow {
    let budget = crate::metric::STREAM_FRAME_BUDGET as u64;
    // Shaped like a real openai window: the budget's worth of SSE events, one of which is the
    // dialect's role head, so the content counts are the budget minus that prelude. A helper
    // that set content == every event would model the very state RIG-11 was about.
    let content = budget - Dialect::Openai.stream_prelude_frames();
    StreamWindow {
        concurrency,
        streams,
        errored: 0,
        error_kinds: StreamErrorKinds::default(),
        host_before: HostState::default(),
        frames: streams * budget,
        expected_frames: streams * budget,
        content_frames: streams * content,
        expected_content_frames: streams * content,
        stalls: 0,
        elapsed_s: 1.0,
    }
}

#[test]
fn a_window_that_delivered_every_frame_cleanly_holds_the_gate() {
    assert!(streams_gate_passes(&clean_stream_window(64, 64)));
}

// EVERY FRAME, and the rung below is where the gateway's real ceiling is. A dropped frame is a
// dropped token; there is no concurrency at which losing one is the gateway succeeding.
#[test]
fn a_single_lost_frame_fails_the_rung() {
    let mut w = clean_stream_window(1000, 1000);
    assert!(
        streams_gate_passes(&w),
        "a window that delivered everything must pass: {w:?}"
    );
    w.content_frames -= 1;
    assert!(
        !streams_gate_passes(&w),
        "one lost frame must fail the rung: {w:?}"
    );
    // The old 0.999 bar waved this through, and at a 64-frame budget that is 16 frames a window.
    w.content_frames = (w.expected_content_frames as f64 * 0.999).ceil() as u64;
    assert!(
        !streams_gate_passes(&w),
        "99.9% is still a gateway losing tokens: {w:?}"
    );
}

// Both stream searches judge a window the same way. They did not: the cpu-fps probe passed on
// `errored == 0 && frames > 0`, so a window delivering 1 frame of 64 counted as a healthy rung
// and its frames/sec was published.
#[test]
fn both_stream_searches_use_the_same_definition_of_a_healthy_window() {
    let mut w = clean_stream_window(64, 64);
    w.frames = 1;
    w.content_frames = 1;
    assert!(
        !streams_gate_passes(&w),
        "1 frame of 64 is not a healthy window, although nothing errored: {w:?}"
    );
}

// "no stream stalls past 2x the pacing interval". ONE stall anywhere fails the rung: a stall is a
// user-visible gap in a token stream, not a rate to be averaged away across lanes.
#[test]
fn a_single_stall_anywhere_fails_the_whole_rung() {
    let mut w = clean_stream_window(1000, 1000);
    w.stalls = 1;
    assert!(!streams_gate_passes(&w));
}

// "the stream error rate stays under 0.1%". Strictly under, exactly as the request-side
// `sustained_gate_passes` reads its own identical bar.
#[test]
fn the_stream_error_rate_boundary_is_exclusive_of_the_one_in_a_thousand_bar() {
    let mut w = clean_stream_window(1000, 1000);
    w.errored = 1;
    assert!(
        !streams_gate_passes(&w),
        "exactly 1/1000 failing is the boundary itself"
    );
    let mut w = clean_stream_window(1001, 1001);
    w.errored = 1;
    assert!(
        streams_gate_passes(&w),
        "1 in 1001 is comfortably under the bar"
    );
}

// A window that opened nothing measured nothing. A ratio computed from zero must never read as a
// clean window by accident of floating-point division - the same trap `sustained_gate_passes`
// guards for its own all-zero window.
#[test]
fn a_window_that_opened_no_stream_never_reads_as_a_pass() {
    let mut w = clean_stream_window(4, 0);
    w.frames = 0;
    w.expected_frames = 0;
    w.content_frames = 0;
    w.expected_content_frames = 0;
    assert!(!streams_gate_passes(&w));
    assert_eq!(w.delivery_ratio(), 0.0);
    assert_eq!(w.error_ratio(), 1.0, "no streams is not a zero error rate");
}

// A window with no measurable duration measured no RATE. An infinity here would win every peak
// search it appeared in and publish a rig artefact as the gateway's frames/sec ceiling.
#[test]
fn a_window_with_no_elapsed_time_reports_no_rate_rather_than_an_infinity() {
    let mut w = clean_stream_window(4, 4);
    w.elapsed_s = 0.0;
    assert_eq!(w.fps(), 0.0);
}

// ── the stall bound itself ───────────────────────────────────────────────────────────────────

// Gaps, not the first frame's offset: a gateway that is merely slow to start the stream has not
// stalled, and charging its time-to-first-token here would fail it on the number the `Streaming`
// group already publishes as a difference.
#[test]
fn a_late_first_frame_is_not_a_stall_but_a_late_second_one_is() {
    // An explicit pace rather than the live one: the live bound is an environment read, and a test
    // asserting on it races any other test that might ever change it.
    let interval_ms = 20;
    let bound = stall_bound_for(interval_ms);
    // One enormous offset, no gaps at all: nothing to stall on.
    assert_eq!(stalls_past(&[10 * bound], bound), 0);
    // Exactly the bound is not "past" it; one microsecond more is.
    assert_eq!(
        stalls_past(&[0, bound], bound),
        0,
        "the bound itself is not a stall"
    );
    assert_eq!(stalls_past(&[0, bound + 1], bound), 1);
    // The mock's own pace (20 ms between deltas) must never register.
    let paced: Vec<u64> = (0..64).map(|i| i * interval_ms * 1_000).collect();
    assert_eq!(
        stalls_past(&paced, bound),
        0,
        "the mock's own pacing is not a stall"
    );
}

// ── a real window against a real SSE peer ───────────────────────────────────────────────────

/// A minimal SSE peer, SHAPED LIKE THE MOCK'S OPENAI STREAM: one role head frame carrying no
/// content, then `frames` content deltas `gap_ms` apart, then the connection closes. Enough to
/// drive `stream_window` for real - the gate above is pure, but nothing else proves the lanes
/// actually open, read frames, and are joined.
///
/// The head frame is not decoration. A peer that sent nothing but content deltas could not
/// distinguish the delivery ratio RIG-11 fixed from the one it replaced, because the two agree
/// exactly when no scaffolding arrives.
fn serve_sse(frames: usize, gap_ms: u64) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    l.set_nonblocking(true).expect("nonblocking SSE fixture");
    // The test exercises async stream lanes, not the runner's OS-thread quota.
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("SSE fixture runtime");
        runtime.block_on(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::from_std(l).expect("SSE listener");
            while let Ok((mut c, _)) = listener.accept().await {
                tokio::spawn(async move {
                let mut b = [0u8; 4096];
                if c.read(&mut b).await.unwrap_or(0) == 0 {
                    return;
                }
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nConnection: close\r\n\r\n";
                if c.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                // The role head: openai's first chunk, which carries no token.
                let role =
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n";
                if c.write_all(role.as_bytes()).await.is_err() {
                    return;
                }
                let _ = c.flush().await;
                for i in 0..frames {
                    if gap_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(gap_ms)).await;
                    }
                    let frame = format!(
                        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"t{i}\"}}}}]}}\n\n"
                    );
                    if c.write_all(frame.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = c.flush().await;
                }
                });
            }
        });
    });
    addr
}

#[test]
fn a_stream_window_reads_every_lane_and_counts_its_frames() {
    let sse = serve_sse(crate::metric::STREAM_FRAME_BUDGET, 0);
    let w = stream_window(sse, "/v1/chat/completions", "{}", &[], Dialect::Openai, 4)
        .expect("four lanes against a live SSE peer must produce a window");
    assert_eq!(w.streams, 4, "every lane must be joined and counted: {w:?}");
    assert_eq!(
        w.errored, 0,
        "a well-framed event stream is not an error: {w:?}"
    );
    assert_eq!(w.frames, 4 * crate::metric::STREAM_FRAME_BUDGET as u64);
    assert_eq!(
        w.expected_frames, w.frames,
        "a full-budget peer leaves no shortfall"
    );
    // NOT `stalls == 0`. A stall is a frame gap wider than twice the mock's pace, so on a
    // machine running the rest of this suite in parallel it is a fact about the machine: this
    // asserted zero and observed 2 whenever a neighbouring window test ran beside it. What the
    // window is actually being held to is that stalls do not stop a clean full-delivery window
    // from holding the gate, which the gate assertion below says directly and without depending
    // on how busy the box is.
    assert!(
        w.stalls <= w.frames,
        "a stall is a gap BETWEEN frames, so it cannot exceed them: {w:?}"
    );
    assert!(
        w.fps() > 0.0,
        "a window that read frames must carry a rate: {w:?}"
    );
    assert!(
        streams_gate_passes(&w),
        "a clean full-delivery window holds the gate: {w:?}"
    );
}

/// A peer that answers a well-formed JSON document rather than an event stream. Declares its
/// content-type, which is what lets `post_json_sse` answer immediately instead of waiting out its
/// deadline - the same short-circuit a real non-streaming gateway gets.
fn serve_json(status: u16) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                while c.read(&mut b).unwrap_or(0) > 0 {
                    let r = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\nok"
                    );
                    if c.write_all(r.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    addr
}

// A peer that answers plain JSON has not streamed. That is an ERRORED stream, not a delivery
// shortfall: the two halves of the README's gate are different findings, and folding a
// non-streaming peer into "delivered fewer frames" would hide it behind a ratio.
#[test]
fn a_peer_that_answers_json_instead_of_frames_counts_as_an_errored_stream() {
    let json = serve_json(200);
    let w = stream_window(json, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
        .expect("the window still ran");
    assert_eq!(w.streams, 2);
    assert_eq!(
        w.errored, 2,
        "a non-event-stream answer is a failed stream: {w:?}"
    );
    assert_eq!(w.frames, 0);
    assert!(!streams_gate_passes(&w));
}

// A stream that opens and then delivers SHORT is not an error, it is a delivery shortfall. The
// gate refuses it on the delivery bar, and the error count stays clean, so the published rung
// says which of the two actually happened.
#[test]
fn a_short_stream_is_a_delivery_shortfall_rather_than_an_error() {
    let short = crate::metric::STREAM_FRAME_BUDGET / 2;
    let sse = serve_sse(short, 0);
    let w = stream_window(sse, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
        .expect("the window ran");
    assert_eq!(
        w.errored, 0,
        "the stream existed; it just ended early: {w:?}"
    );
    // The role head arrives too, so the raw event count is one more per lane than the tokens.
    assert_eq!(w.frames, 2 * (short as u64 + 1));
    assert_eq!(w.content_frames, 2 * short as u64);
    assert!(w.delivery_ratio() < STREAM_MIN_DELIVERY_RATIO);
    assert!(!streams_gate_passes(&w));
}

// THE CLOCK STARTS AFTER THE LANES EXIST, and that is asserted at the clock site rather than
// through the stopwatch, because the stopwatch cannot see it.
//
// This test used to assert `elapsed_s < 0.25`. That constant is the speed of the machine that
// wrote it: green on CI, stably red at 0.355s on a developer's Mac, and red on any loaded
// runner. It was never measuring the ordering it is named for.
//
// Measured, rather than assumed: spawning the 200 lanes costs 2.4ms of a 380ms window - 0.6%.
// Starting the clock before the spawn loop instead of after it therefore moves `elapsed_s` by
// less than the run-to-run scatter, and an injected version of exactly that defect passes every
// timing bound loose enough not to fail on a slow machine. Lane setup stopped being expensive
// when it stopped being serial OS-thread creation, which is the same change that made the
// ordering matter less; the bound outlived the ramp it was written for.
//
// So this keeps what a window CAN be held to - every lane ran, every frame was counted, and the
// clock is positive and finite - and leaves the ordering to the comment at `started`, which is
// where a reader changing it will actually be standing.
#[test]
fn a_stream_window_counts_every_lane_it_was_asked_for() {
    // A fleet, but not a stampede. 200 lanes here starved a neighbouring window test into
    // reporting stalls under parallel load, and since the ramp assertion is gone the extra
    // lanes bought nothing but contention - a test that fails its neighbours is a bad test.
    let concurrency = 64;
    let sse = serve_sse(crate::metric::STREAM_FRAME_BUDGET, 0);
    let w = stream_window(
        sse,
        "/v1/chat/completions",
        "{}",
        &[],
        Dialect::Openai,
        concurrency,
    )
    .expect("a fleet of instant-answering lanes must produce a window");
    assert_eq!(
        w.streams,
        u64::from(concurrency),
        "every lane must be joined and counted"
    );
    assert_eq!(w.errored, 0, "an instant peer errors no stream: {w:?}");
    assert_eq!(
        w.frames,
        u64::from(concurrency) * crate::metric::STREAM_FRAME_BUDGET as u64
    );
    assert!(
        w.elapsed_s > 0.0 && w.elapsed_s.is_finite(),
        "the window must report a real clock: {w:?}"
    );
    // fps() divides by that clock, so a zero or negative one would publish an infinity.
    assert!(w.fps() > 0.0 && w.fps().is_finite(), "{w:?}");
}

// The gateway leg's own frames/sec is meaningless without a reference, and the reference must be
// absent rather than zero when the mock could not be reached: `is_rig_bound` reads an unusable
// reference as "unknown", and a zero would read as "the gateway beat the rig".
#[test]
fn an_unreachable_mock_yields_an_absent_stream_reference_never_a_zero() {
    let dead: SocketAddr = "127.0.0.1:1".parse().expect("literal");
    let m = stream_fps_at(dead, "m", "dummy", Dialect::Openai, 2);
    assert_eq!(m.copied(), None);
    assert_eq!(m.reason(), Some(&Absent::NotMeasured));
}

#[test]
fn a_window_with_ok_and_fail_both_zero_reads_as_all_failed_never_a_pass() {
    // `sustained_gate_passes` is only ever called after `SustainedProbe::probe` has already
    // filtered out the all-zero window as unmeasured (see its own `stats.ok == 0 && stats.fail
    // == 0` guard). This pins the function's own behaviour on that input regardless: a fail
    // ratio computed as 0/0 must never read as a CLEAN window by accident of floating-point
    // division.
    assert!(!sustained_gate_passes(Some(1), 0, 0));
}

// A SERVER THAT IS BUSY NOW AND FINE IN A MOMENT MUST NOT BE RECORDED AS INCAPABLE.
//
// This is the defect that cost a board. `transient_budget()` - 3 attempts, 30s apart - existed,
// was documented as the budget `Verdict::Failed` had spent, was unit-tested, and was called by
// nothing. So one 503 became "this gateway does not serve this pairing", permanently, in public.
//
// The harness provokes exactly this: cells run back to back with no settle and the metric before
// each probe is a heavy load, so a gateway with admission control is still shedding when the next
// cell asks whether it exists. In the 2026-07-28 field run busbar answered 503 on 26 of 36 cells
// and every one published as a red, while every egress lane answered fine under openai ingress
// in the same run - the lanes were healthy, the moment was not.
//
// The pause is shortened here through the same budget the field uses, so the test exercises the
// real loop rather than a copy of it.
fn serve_busy_then_ok(busy_times: usize) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                while c.read(&mut b).unwrap_or(0) > 0 {
                    let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let (status, body) = if n < busy_times {
                        (
                            503,
                            "{\"error\":{\"message\":\"The service is temporarily overloaded.\"}}",
                        )
                    } else {
                        (200, "{\"ok\":true}")
                    };
                    let r = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    if c.write_all(r.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    addr
}

#[test]
fn a_gateway_that_answers_503_once_is_retried_not_recorded_as_not_serving() {
    let addr = serve_busy_then_ok(1);
    let cfg = test_fixture(addr, addr);
    let served = probe_cell_within(
        &cfg,
        &CellId::new("openai", "openai"),
        true,
        3,
        Duration::from_millis(10),
    );
    assert_eq!(
        served,
        Served::Yes,
        "one 503 became a permanent capability verdict; this is the 26 busbar cells that \
             published as red while every one of its lanes was healthy"
    );
}

// The budget is bounded, and a gateway that is genuinely down stays down. Exhausting it must
// still produce the real verdict rather than an optimistic one.
#[test]
fn a_gateway_that_stays_503_across_the_whole_budget_is_still_recorded_as_failed() {
    let addr = serve_busy_then_ok(usize::MAX);
    let cfg = test_fixture(addr, addr);
    match probe_cell_within(
        &cfg,
        &CellId::new("openai", "openai"),
        true,
        3,
        Duration::from_millis(10),
    ) {
        Served::No(crate::probe::Verdict::Failed, ev) => assert_eq!(ev.status, 503),
        other => panic!("a persistently-503 gateway must still fail, got {other:?}"),
    }
}

// A GATEWAY THAT DOES NOT ANSWER IN TIME IS A MOMENT, NOT A CAPABILITY.
//
// The status door and the transport door lead to the same place. busbar lost 26 cells to a
// transient 503; litellm-python lost three to a timeout, its served count sliding 8 -> 7 -> 5
// across the 2026-07-28 runs while the lost cells recorded "the gateway accepted the connection
// and never answered". Both are the harness asking a question of a gateway still shedding the
// load the harness itself just applied.
fn serve_silent_then_ok(silent_times: usize) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                while c.read(&mut b).unwrap_or(0) > 0 {
                    let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n < silent_times {
                        // Accept, then say nothing: exactly the shape that reads as
                        // "accepted the connection and never answered".
                        std::thread::sleep(std::time::Duration::from_secs(30));
                        return;
                    }
                    let body = "{\"ok\":true}";
                    let r = format!(
                        "HTTP/1.1 200 X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    if c.write_all(r.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    addr
}

#[test]
fn a_gateway_that_does_not_answer_once_is_retried_not_written_off() {
    let addr = serve_silent_then_ok(1);
    let cfg = test_fixture(addr, addr);
    assert_eq!(
        probe_cell_within(
            &cfg,
            &CellId::new("openai", "openai"),
            true,
            3,
            Duration::from_millis(10)
        ),
        Served::Yes,
        "one timeout cost this cell entirely; these are litellm-python's three lost cells"
    );
}

// The budget is bounded on this door too: a gateway that never answers still ends up untestable
// rather than being retried forever or, worse, optimistically passed.
#[test]
fn a_gateway_that_never_answers_is_still_untestable_after_the_budget() {
    let addr = serve_silent_then_ok(usize::MAX);
    let cfg = test_fixture(addr, addr);
    match probe_cell_within(
        &cfg,
        &CellId::new("openai", "openai"),
        true,
        3,
        Duration::from_millis(10),
    ) {
        Served::Untestable(why) => assert!(why.contains("never answered"), "got {why}"),
        other => panic!("a silent gateway must stay untestable, got {other:?}"),
    }
}

// THE STALL BOUND FOLLOWS THE MOCK'S ACTUAL PACE, not a copy of its default.
//
// A stall is a frame gap wider than twice the upstream's delta interval, so the bound is only
// meaningful if it tracks the interval the mock is really using. It was a 20 hardcoded in the
// engine to match MOCK_STREAM_INTERVAL_MS's default, described as a documented coupling - which
// is the same two-places-one-truth shape as the two hand-rolled ladders, and right up until
// someone uses the knob. Turn the mock's pacing down and every streaming rung is judged against
// a cadence nothing is producing, with no error anywhere.
#[test]
fn the_stall_bound_tracks_the_pace_the_mock_was_actually_told_to_use() {
    // Driven through the pure parse and bound behind `stall_bound_us`, never through `set_var`: the
    // environment is process-global, and mutating it here changed the stall bound under every
    // stream test running beside this one (and stayed changed if an assertion failed).
    //
    // Default: the mock's own default, so the field behaviour is unchanged.
    assert_eq!(pacing_interval_from(None), 20);
    assert_eq!(
        stall_bound_for(pacing_interval_from(None)),
        20 * STREAM_STALL_MULTIPLIER * 1_000
    );

    // A slower model: gaps that were stalls at 20ms are ordinary at 100ms, and a bound that
    // stayed at 20 would call a healthy stream stalled on every frame.
    let slow = stall_bound_for(pacing_interval_from(Some("100")));
    assert_eq!(slow, 100 * STREAM_STALL_MULTIPLIER * 1_000);
    let gaps_at_100ms: Vec<u64> = (0..8).map(|i| i * 100_000).collect();
    assert_eq!(
        stalls_past(&gaps_at_100ms, slow),
        0,
        "a stream keeping the mock's own pace never stalls"
    );

    // A faster model: the bound tightens with it, so a gateway that lags is still caught.
    let fast = stall_bound_for(pacing_interval_from(Some("5")));
    assert_eq!(fast, 5 * STREAM_STALL_MULTIPLIER * 1_000);
    assert!(
        stalls_past(&gaps_at_100ms, fast) > 0,
        "at a 5ms pace those same 100ms gaps are stalls"
    );

    // Garbage is not a pace: fall back to the mock's default rather than to zero, which would
    // make every gap a stall.
    assert_eq!(pacing_interval_from(Some("not-a-number")), 20);
    // And an absurd pace saturates rather than wrapping to a tiny bound that stalls everything.
    assert_eq!(stall_bound_for(u64::MAX), u64::MAX);
}

// THE CEILING IS THE HOST'S, AND EVERY NUMBER IN IT IS READ OR DERIVED.
//
// 4096 was picked when the generator was thread-per-connection, and replacing it with a bigger
// constant only moved the arbitrary number: raising it to 65536 asked for more connections than
// a single host can make to a single destination, because a TCP connection needs a unique
// 4-tuple and the source ports run out first. Stock Linux allows about 28,000 of them.
#[test]
fn the_connection_ceiling_comes_from_the_hosts_port_range_not_a_chosen_constant() {
    // The derivation, over the ranges that matter: stock Linux, the range the orchestrator sets,
    // and the widest a host can offer. Powers of two because the ladder doubles - a ceiling
    // between rungs is reachable only by the clamp and makes the top rung a different shape.
    let ceiling_for = ceiling_for_port_range;
    assert_eq!(
        ceiling_for(32_768, 60_999),
        16_384,
        "stock Linux: ~28k ports"
    );
    assert_eq!(
        ceiling_for(16_384, 65_535),
        32_768,
        "what run-on-ec2.sh sets"
    );
    assert_eq!(
        ceiling_for(1_024, 65_535),
        32_768,
        "the widest a host can give"
    );

    // The real function agrees with that derivation on whatever host runs the test, and never
    // returns something the ladder cannot climb to.
    let c = host_connection_ceiling();
    assert!(
        c.is_power_of_two(),
        "the ceiling must be a rung the ladder actually lands on, got {c}"
    );
    // On Linux it must match the host's real range; off Linux it falls back to the stock range's
    // own derivation rather than to a number someone chose.
    if let Ok(t) = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range") {
        let mut p = t.split_whitespace().filter_map(|v| v.parse::<u32>().ok());
        if let (Some(lo), Some(hi)) = (p.next(), p.next()) {
            assert_eq!(
                c,
                ceiling_for(lo, hi),
                "the ceiling must be this host's own range"
            );
        }
    } else {
        assert_eq!(
            c,
            ceiling_for(32_768, 60_999),
            "off Linux, assume a stock host"
        );
    }
}

// THE RIG RUNNING OUT IS NOT AN ERRORED STREAM.
//
// The load generator got this treatment first; the stream path is where it bites soonest,
// because the stream searches reach high concurrency before anything else does. A lane that
// could not get an ephemeral source port never asked the gateway anything, and counting it as an
// errored stream published our own exhaustion as the gateway's concurrent-stream ceiling.
#[test]
fn a_lane_this_host_could_not_open_is_not_the_gateway_erroring() {
    let ours = crate::http::SseOutcome {
        status: None,
        frames: Vec::new(),
        frame_offsets_us: Vec::new(),
        content_frames: 0,
        end: crate::http::SseEnd::RigExhausted(
            "Cannot assign requested address (os error 99)".into(),
        ),
    };
    assert!(
        !stream_errored(&ours),
        "our own port exhaustion must not be charged to the gateway"
    );

    // The peer refusing IS the gateway's, and must still count.
    let theirs = crate::http::SseOutcome {
        status: None,
        frames: Vec::new(),
        frame_offsets_us: Vec::new(),
        content_frames: 0,
        end: crate::http::SseEnd::ConnectionFailed("Connection refused (os error 111)".into()),
    };
    assert!(
        stream_errored(&theirs),
        "a refused connection is still the gateway declining"
    );

    // So is a peer that answers something that is not a stream.
    let not_sse = crate::http::SseOutcome {
        status: Some(200),
        frames: Vec::new(),
        frame_offsets_us: Vec::new(),
        content_frames: 0,
        end: crate::http::SseEnd::NotAnEventStream("application/json".into()),
    };
    assert!(stream_errored(&not_sse));
}

// THE RIG MUST BE RE-CONFIRMED AS THE GRID RUNS, not once at the start.
//
// `mock_healthy` is the input that lets a verdict come back NotVerified: when the rig cannot
// vouch for itself, nothing observed is attributable to the gateway. Reading it once before a
// grid that runs for ninety minutes defeated that - a mock that degraded partway left every
// later cell graded as though the rig were fine, turning our failure into the gateway's verdict.
//
// Asserted on the verdict rule rather than by driving a whole grid, because the rule is where
// the consequence lives and a grid test cannot make a mock die halfway on demand.
#[test]
fn an_unconfirmed_rig_makes_the_same_observation_unattributable() {
    use crate::probe::{persistent_transient_verdict, Observation, Verdict};

    // The identical status, seen with the rig confirmed and unconfirmed, must not produce the
    // same verdict - that is the whole reason the flag is threaded down here.
    for status in [500u16, 503, 400, 404] {
        let confirmed = persistent_transient_verdict(Observation {
            status: Some(status),
            mock_healthy: true,
        });
        let unconfirmed = persistent_transient_verdict(Observation {
            status: Some(status),
            mock_healthy: false,
        });
        assert_ne!(
            confirmed, unconfirmed,
            "HTTP {status} must not read the same when the rig could not confirm itself"
        );
        assert_eq!(
            unconfirmed,
            Verdict::NotVerified,
            "an unconfirmed rig makes HTTP {status} unattributable"
        );
    }

    // And with the rig confirmed, the gateway's own answers still classify normally.
    assert_eq!(
        persistent_transient_verdict(Observation {
            status: Some(404),
            mock_healthy: true
        }),
        Verdict::NotConfigured
    );
    assert_eq!(
        persistent_transient_verdict(Observation {
            status: Some(503),
            mock_healthy: true
        }),
        Verdict::Failed
    );
}

// A CEILING THE GATEWAY HOLDS ONE TIME IN THREE IS NOT A CEILING IT SUSTAINS.
//
// The bisection walks up until ONE window fails, so it lands exactly on the boundary: the
// highest concurrency that passed once. Re-measuring the 2026-07-28 field run's own published
// ceilings found 9 of 48 held the p99 gate in only 1 of 3 windows, and the shape is unmistakable
// - the first window passes and the rest fail. agentgateway c=252 saw 19866, 20036, 20404 against
// a 20000us gate; apisix c=171 saw 19980, 21114, 22530.
#[test]
fn a_ceiling_is_confirmed_by_a_majority_of_its_own_windows() {
    // The rule the confirmation applies, with the bisection's own winning window counted as one
    // vote - it is a real measurement at that concurrency and discarding it throws away evidence.
    //
    // Asserted through `stream_ceiling_confirmed`, the predicate the stream search applies, rather
    // than a copy of it here that production could drift from.
    let holds = |repeats: &[bool]| {
        let held = 1 + repeats.iter().filter(|ok| **ok).count();
        let total = 1 + repeats.len();
        stream_ceiling_confirmed(total, held)
    };

    // The field shape: bisection window passed, both confirmations failed. 1 of 3 is not a
    // ceiling, and this is the case that was being published.
    assert!(
        !holds(&[false, false]),
        "1 of 3 windows must not confirm a ceiling"
    );
    // A genuine ceiling: the confirmations agree with the bisection.
    assert!(holds(&[true, true]), "3 of 3 must confirm");
    // The marginal case - 2 of 3 - is a majority and confirms. A gateway that holds two thirds of
    // the time is sustaining it; demanding unanimity would reject real ceilings for one unlucky
    // window, which is the opposite error.
    assert!(
        holds(&[true, false]),
        "2 of 3 is a majority and must confirm"
    );

    // Stepping down with nothing proven clean bisects toward the search floor, so the walk is
    // bounded and always moves toward a region already known to pass - driven through the real
    // step-down rule, not a halving written out here.
    let mut c = 252u32;
    for _ in 0..MAX_CEILING_STEPDOWNS {
        let next = stepdown_next(0, 1, c);
        assert!(next < c, "a step-down must make progress");
        c = next;
    }
    assert!(
        c < 252 / 8,
        "the step-down budget must reach well below the failing ceiling, got {c}"
    );
}

// The sustained ceiling's own confirmation is driven end to end by
// `every_published_ceiling_was_measured_by_a_full_rung_of_windows`, against a scripted probe.
// The stream ceiling's confirmation (`sweep_streams_cell`) applies `stream_ceiling_confirmed`,
// asserted above, and the whole search is driven end to end by `search_simulator`.

// ── the rig refuses to smuggle: the lanes run.rs owns ───────────────────────────────────────

// A PROBE WE REFUSE TO SEND IS UNTESTABLE, NEVER A CAPABILITY VERDICT.
//
// Ledger RIG-12: the probe lane composed the same manifest headers the load lane did and
// interpolated them raw. Now `http::send` refuses, and this is what the refusal must become on
// the way out - `Served::Untestable`, the spelling of `probe::Verdict::NotVerified`, which is
// "a statement about the RIG, not the gateway". `Served::No` here would convict a gateway of a
// defect in one of our own manifest files.
#[test]
fn a_probe_the_rig_refuses_to_send_is_untestable_rather_than_a_refusal() {
    // A live, healthy peer, so nothing about this verdict can come from an address with nothing
    // on it: the refusal happens before the connect, and that is the point.
    let peer = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = peer.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in peer.incoming() {
            let Ok(mut c) = c else { continue };
            let mut b = [0u8; 4096];
            let _ = c.read(&mut b);
            let _ = c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}");
        }
    });

    let mut cfg = test_fixture(addr, addr);
    cfg.static_headers = vec![("x-route".into(), "a\r\nx-injected: b".into())];
    let id = CellId::new("openai", "openai");
    let (served, retry) = probe_cell_once(&cfg, &id, true);
    match &served {
        Served::Untestable(why) => assert!(
            why.contains("never asked") || why.contains("refused to send"),
            "the verdict must say the gateway was never asked: {why}"
        ),
        other => panic!("a request we refused to send was graded as {other:?}"),
    }
    assert!(
        retry.is_none(),
        "asking again spends the budget to be refused identically"
    );

    // The same cfg without the hostile header reaches the peer, so this test proves a refusal
    // rather than a broken fixture.
    cfg.static_headers = vec![("x-route".into(), "b".into())];
    assert!(
        !matches!(probe_cell_once(&cfg, &id, true).0, Served::Untestable(_)),
        "a benign header must still be probed"
    );
}

// A STREAM WINDOW WE REFUSE TO SEND MEASURED NOTHING, so it is unmeasured - not a failing rung,
// and not an errored stream. Charging the gateway for a header of ours is the attribution
// inversion this engine refuses everywhere.
#[test]
fn a_stream_window_the_rig_refuses_to_send_is_unmeasured_not_a_failing_rung() {
    let sse = serve_sse(crate::metric::STREAM_FRAME_BUDGET, 0);
    let hostile = vec![("authorization".to_string(), "Bearer t\r\nx: y".to_string())];
    assert!(
        stream_window(
            sse,
            "/v1/chat/completions",
            "{}",
            &hostile,
            Dialect::Openai,
            4
        )
        .is_none(),
        "a window whose request we would not send has measured nothing about the gateway"
    );
    // And the lane-level rule underneath it: a refusal is not the gateway erroring.
    let refused = crate::http::SseOutcome {
        status: None,
        frames: Vec::new(),
        frame_offsets_us: Vec::new(),
        content_frames: 0,
        end: crate::http::SseEnd::RigRefused("a header we will not send".into()),
    };
    assert!(
        !stream_errored(&refused),
        "our own refusal must not be charged to the gateway's stream error rate"
    );
}

// ── delivery counts TOKENS, not events (ledger RIG-11) ──────────────────────────────────────

/// A peer that answers with a full budget of well-formed openai events, NONE of which carries a
/// token: the role head, then finish/`[DONE]` scaffolding repeated. A real gateway does not do
/// this; the point is that nothing in the old accounting could tell it apart from one that
/// streamed perfectly.
fn serve_sse_without_tokens(events: usize) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                if c.read(&mut b).unwrap_or(0) == 0 {
                    return;
                }
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nConnection: close\r\n\r\n";
                if c.write_all(head.as_bytes()).is_err() {
                    return;
                }
                for _ in 0..events {
                    let frame = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":null}]}\n\n";
                    if c.write_all(frame.as_bytes()).is_err() {
                        return;
                    }
                    let _ = c.flush();
                }
            });
        }
    });
    addr
}

// A STREAM THAT DELIVERED NO TOKENS HAS NOT DELIVERED.
//
// Ledger RIG-11. `delivery_ratio` was `frames / expected_frames` over every dispatched SSE
// event, so a peer that filled its whole frame budget with framing satisfied the gate - ratio
// 1.0, no stalls, no errors, published as the gateway carrying clean streams at that
// concurrency. The counts a reader needs to see this are on the window either way; only the
// ratio's numerator changed.
#[test]
fn a_full_budget_of_scaffolding_is_not_a_delivered_stream() {
    let peer = serve_sse_without_tokens(crate::metric::STREAM_FRAME_BUDGET);
    let w = stream_window(peer, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
        .expect("the window ran");
    assert_eq!(
        w.frames,
        2 * crate::metric::STREAM_FRAME_BUDGET as u64,
        "every event still counts in `frames`, which is what fps is for: {w:?}"
    );
    assert_eq!(
        w.errored, 0,
        "the stream existed and was well-formed: {w:?}"
    );
    assert_eq!(w.content_frames, 0, "not one token arrived: {w:?}");
    assert_eq!(w.delivery_ratio(), 0.0);
    assert!(
        !streams_gate_passes(&w),
        "a stream that delivered no tokens must not hold the delivery gate: {w:?}"
    );
    let why = streams_gate_verdict(&w).expect("a failing rung publishes a reason");
    assert!(
        why.contains("content frames"),
        "the reason must say what was counted: {why}"
    );
}

// The other half: a peer that DOES deliver every token it could still holds the gate, so the
// tightened numerator is not simply failing everything. The denominator is the budget minus the
// dialect's prelude, which is why an openai stream delivering 63 tokens of a 64-frame budget is
// complete rather than one short.
#[test]
fn a_stream_that_delivers_every_token_the_budget_allows_still_holds_the_gate() {
    let sse = serve_sse(crate::metric::STREAM_FRAME_BUDGET, 0);
    let w = stream_window(sse, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
        .expect("the window ran");
    let budget = crate::metric::STREAM_FRAME_BUDGET as u64;
    assert_eq!(w.frames, 2 * budget);
    assert_eq!(
        w.expected_content_frames,
        2 * (budget - Dialect::Openai.stream_prelude_frames())
    );
    assert_eq!(w.content_frames, w.expected_content_frames, "{w:?}");
    assert_eq!(w.delivery_ratio(), 1.0);
    assert!(streams_gate_passes(&w), "{w:?}");
}

// ── the delivery budget is counted in CONTENT, not in events ────────────────────────────────

/// An SSE peer SHAPED LIKE A GATEWAY RATHER THAN LIKE THE MOCK: openai's role head, then a
/// SECOND framing event the mock never sends, then `content` tokens with a keepalive between
/// each pair. Every token the client asks for is delivered; the stream just costs more events
/// than the mock's own layout to deliver them.
///
/// This is not a hypothetical peer. Anthropic's real SSE protocol sends `ping` events, a
/// TRANSLATION cell has the gateway re-emitting the stream in the client's dialect with its own
/// framing, and a keepalive does the same thing on any dialect.
fn serve_sse_with_gateway_framing(content: usize) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                if c.read(&mut b).unwrap_or(0) == 0 {
                    return;
                }
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nConnection: close\r\n\r\n";
                if c.write_all(head.as_bytes()).is_err() {
                    return;
                }
                // Two prelude events where the mock sends one: the gateway's framing is its own.
                let role =
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n";
                let ping = "data: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\n";
                for f in [role, ping] {
                    if c.write_all(f.as_bytes()).is_err() {
                        return;
                    }
                }
                for i in 0..content {
                    let frame = format!(
                        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"t{i}\"}}}}]}}\n\n"
                    );
                    if c.write_all(frame.as_bytes()).is_err() {
                        return;
                    }
                    if c.write_all(ping.as_bytes()).is_err() {
                        return;
                    }
                    let _ = c.flush();
                }
            });
        }
    });
    addr
}

// A GATEWAY'S OWN FRAMING IS NOT A DELIVERY SHORTFALL.
//
// The delivery denominator is `STREAM_FRAME_BUDGET - stream_prelude_frames()`, a constant read
// off the MOCK's layout, while the numerator is measured on the GATEWAY's stream. While the read
// stopped after a fixed number of EVENTS, every ping or extra prelude chunk the gateway emitted
// displaced exactly one content frame: this peer delivers every token asked of it, and an
// event-budgeted read would have collected 31 of the 63 expected content frames - a ratio of
// 0.49 against a bound of 1.0, failing AT EVERY RUNG, for a shortfall the gateway did not cause.
// Reading to the CONTENT budget instead asks the question the metric is for.
#[test]
fn a_gateway_that_spends_extra_events_on_framing_still_delivers_every_token() {
    let gw = serve_sse_with_gateway_framing(crate::metric::STREAM_FRAME_BUDGET);
    let w = stream_window(gw, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
        .expect("the window ran");
    assert_eq!(
        w.errored, 0,
        "a well-framed event stream is not an error: {w:?}"
    );
    assert_eq!(
        w.content_frames, w.expected_content_frames,
        "every token the budget asks for arrived: {w:?}"
    );
    assert_eq!(w.delivery_ratio(), 1.0, "{w:?}");
    assert!(
        streams_gate_passes(&w),
        "a gateway that inserts framing and loses nothing must hold the gate: {:?}",
        streams_gate_verdict(&w)
    );
    // And it paid for that framing in EVENTS, which is the whole point: `frames` counts every
    // event and so runs past the mock-shaped `expected_frames`, while delivery is judged on the
    // content pair beside it.
    assert!(
        w.frames > w.expected_frames,
        "the extra framing must be visible in the raw event count: {w:?}"
    );
}

// The other side of the same read, so the fix is not simply a looser gate: a gateway with the
// same extra framing that delivers one token FEWER than the budget asks for still fails. The
// read waits for content rather than for events, so a token that never arrives is the only
// reason the count can come up short.
#[test]
fn a_gateway_that_drops_a_token_fails_the_gate_however_it_frames_the_stream() {
    let content =
        crate::metric::STREAM_FRAME_BUDGET - Dialect::Openai.stream_prelude_frames() as usize - 1;
    let gw = serve_sse_with_gateway_framing(content);
    let w = stream_window(gw, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
        .expect("the window ran");
    assert_eq!(
        w.errored, 0,
        "the stream existed and was well-framed; it lost a token: {w:?}"
    );
    assert_eq!(w.content_frames, 2 * content as u64, "{w:?}");
    assert!(w.delivery_ratio() < STREAM_MIN_DELIVERY_RATIO, "{w:?}");
    assert!(!streams_gate_passes(&w), "{w:?}");
    let why = streams_gate_verdict(&w).expect("a failing rung publishes a reason");
    assert!(
        why.contains("content frames"),
        "the reason must name what came up short: {why}"
    );
}

/// A peer that frames FOREVER and never sends a token: the pathological case a content-budgeted
/// read has to be bounded against, since the tokens it is waiting for are never coming.
fn serve_sse_endless_framing() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            std::thread::spawn(move || {
                let mut b = [0u8; 4096];
                if c.read(&mut b).unwrap_or(0) == 0 {
                    return;
                }
                let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nConnection: close\r\n\r\n";
                if c.write_all(head.as_bytes()).is_err() {
                    return;
                }
                let ping = "data: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\n";
                // Until the reader hangs up, which is what the ceiling makes it do.
                while c.write_all(ping.as_bytes()).is_ok() {
                    let _ = c.flush();
                }
            });
        }
    });
    addr
}

// THE READ STAYS BOUNDED WITHOUT WAITING OUT THE TIMEOUT, and the shortfall is still a failure.
//
// A budget counted in content frames has no bound of its own against a peer that keeps framing,
// and `STREAM_TIMEOUT` alone is 20 seconds per lane - at the concurrencies these searches climb
// to that is a search that never returns. `STREAM_EVENT_CEILING` stops the read at 4x the frame
// budget; hitting it with no tokens is a real delivery shortfall, and the gate says so.
#[test]
fn an_endless_framing_stream_stops_at_the_event_ceiling_and_still_fails_the_gate() {
    let peer = serve_sse_endless_framing();
    let started = std::time::Instant::now();
    let w = stream_window(peer, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
        .expect("the window ran");
    assert!(
        started.elapsed() < crate::metric::STREAM_TIMEOUT,
        "the ceiling, not the deadline, must be what ends this read: {w:?}"
    );
    assert_eq!(
        w.frames,
        2 * crate::metric::STREAM_EVENT_CEILING as u64,
        "each lane reads exactly to the ceiling: {w:?}"
    );
    assert_eq!(w.content_frames, 0, "not one token arrived: {w:?}");
    assert_eq!(
        w.errored, 0,
        "the peer answered 200 and framed correctly - it delivered nothing, which is the \
             delivery clause's finding rather than an errored stream: {w:?}"
    );
    assert!(!streams_gate_passes(&w), "{w:?}");
}

// ── one credential header on the wire (ledger RIG-12 remainder) ─────────────────────────────

// TWO `authorization` HEADERS ON ONE REQUEST IS A MEASUREMENT NOBODY CAN ATTRIBUTE.
//
// `headers_for` composed the dialect's credential header and then appended the manifest's
// verbatim, and `gateways/litellm-rust/definition.json` declares `Authorization: Bearer
// {GW_AUTH}` today. HTTP does not define which of two same-named headers a server honours, so
// the gateway authenticated as somebody and published a clean number for a request whose tenant
// and route we could not state.
#[test]
fn only_one_copy_of_a_header_the_rig_owns_reaches_the_wire() {
    let a: SocketAddr = "127.0.0.1:1".parse().expect("addr");
    let mut cfg = test_fixture(a, a);
    // Spelled as the live manifest spells it, capital A and all: HTTP header names are
    // case-insensitive, and a rule that only caught the lowercase one would not have caught the
    // manifest that motivated it.
    cfg.static_headers = vec![
        ("Authorization".into(), "Bearer manifest-token".into()),
        ("x-route".into(), "keep-me".into()),
    ];
    cfg.egress_headers = [(
        "openai".to_string(),
        vec![("authorization".to_string(), "Bearer per-column".to_string())],
    )]
    .into_iter()
    .collect();

    let h = headers_for(&cfg, Dialect::Openai, "openai");
    let auths: Vec<&(String, String)> = h
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("authorization"))
        .collect();
    assert_eq!(
        auths.len(),
        1,
        "exactly one credential header may leave: {h:?}"
    );
    assert_eq!(
        auths[0].1, "Bearer dummy",
        "the surviving one is the rig's own, built from the token this run holds: {h:?}"
    );
    // Nothing else is dropped: a routing header is how a column selects its upstream, and losing
    // one would publish a number for a pairing that was never driven.
    assert!(
        h.contains(&("x-route".to_string(), "keep-me".to_string())),
        "{h:?}"
    );

    // The dialect decides which names it owns, so anthropic's protocol constant is protected
    // too - two `anthropic-version` headers is the same undefined resolution.
    cfg.static_headers = vec![
        ("anthropic-version".into(), "1999-01-01".into()),
        ("X-Api-Key".into(), "manifest-key".into()),
    ];
    cfg.egress_headers = Default::default();
    let h = headers_for(&cfg, Dialect::Anthropic, "anthropic");
    assert_eq!(
        h.iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("anthropic-version"))
            .count(),
        1,
        "{h:?}"
    );
    assert_eq!(
        h.iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("x-api-key"))
            .count(),
        1,
        "{h:?}"
    );
    assert!(
        h.contains(&("x-api-key".to_string(), "dummy".to_string())),
        "{h:?}"
    );
    assert!(
        h.contains(&("anthropic-version".to_string(), "2023-06-01".to_string())),
        "{h:?}"
    );
}

// ── a rung with no reading has no failure count ─────────────────────────────────────────────

// ── the derived mock ceiling, and the engine-fault check that replaced the old suppression ──────

// ARITHMETIC, NOT A MEASUREMENT, and this is the arithmetic. The mock sleeps `interval` before every
// delta except the first, so `chunks` frames take `(chunks - 1) * interval` and c concurrent streams
// carry `c * chunks` of them in that time. Both terms come from the variables the MOCK reads, so
// this test states the identity rather than a remembered number.
#[test]
fn the_mock_ceiling_is_the_mocks_own_declared_pacing_and_nothing_else() {
    let chunks = f64::from(mock_stream_chunks());
    let interval_s = stream_pacing_interval_ms() as f64 / 1000.0;
    for c in [1u32, 8, 256, 1024, 16_384] {
        let want = f64::from(c) * chunks / ((chunks - 1.0) * interval_s);
        let got = mock_frame_ceiling_fps(c);
        assert!(
            (got - want).abs() < 1e-6,
            "c={c}: {got} is not {chunks} frames per stream over {interval_s}s gaps"
        );
    }
    // Linear in concurrency, because the mock's pacing is per stream and the streams are concurrent.
    assert!(
        (mock_frame_ceiling_fps(2048) - 2.0 * mock_frame_ceiling_fps(1024)).abs() < 1e-6,
        "twice the streams is twice the frames at the same pace"
    );
}

// THE NUMBER THIS BOX ACTUALLY HAS, so a defaults change cannot quietly move the ceiling without a
// test noticing. 64 frames 20ms apart is 1.26s per stream; 1024 streams carry 65536 frames in that
// time, which is 52013 frames/sec - the figure the 2026-07-29 investigation turned on. The measured
// reference that day read 25893 (50% of physics) and the gateway leg 43297 (83%): the gateway beat
// the disadvantaged CONTROL by 1.67x while sitting comfortably under the real bound, and a chosen
// 1.5x factor discarded it on that basis.
#[test]
fn the_ceiling_reproduces_the_bench_boxs_own_measured_night() {
    // Only meaningful on the box's defaults; if either knob is set, the identity above is the test.
    if mock_stream_chunks() != 64 || stream_pacing_interval_ms() != 20 {
        return;
    }
    let ceiling = mock_frame_ceiling_fps(1024);
    assert!(
        (ceiling - 52_012.7).abs() < 1.0,
        "1024 x 64 frames over 1.26s is 52013 frames/sec, got {ceiling}"
    );
    for (leg, observed, want_share) in [
        ("direct control", 25_893.0),
        ("through the gateway", 43_297.0),
    ]
    .iter()
    .zip([0.4978, 0.8325])
    .map(|((l, o), w)| (*l, *o, w))
    {
        let share = observed / ceiling;
        assert!(
            (share - want_share).abs() < 0.001,
            "{leg} read {observed}, which is {share} of physics"
        );
        assert!(
            share < 1.0,
            "{leg} did not exceed the mock's own pacing, so nothing that night was impossible"
        );
    }
}

// A ceiling of zero rather than an infinity when there is nothing to bound. An infinity would make
// every headroom fraction 0.0 and read as "miles below the rig" on every cell.
#[test]
fn no_streams_means_no_frame_rate_to_bound_against() {
    assert_eq!(mock_frame_ceiling_fps(0), 0.0);
}

// ── engine_fault: exact, and only where an exact bound exists ───────────────────────────────────

// A window at its budget is the SUCCESS case and must never be called a fault. This is the shape
// every clean rung has (`content=64512/64512` in the field logs), and the old rig comparison
// suppressed exactly these.
#[test]
fn a_window_delivering_exactly_its_content_budget_is_not_a_fault() {
    let w = StreamWindow {
        concurrency: 1024,
        streams: 1024,
        errored: 0,
        error_kinds: StreamErrorKinds::default(),
        host_before: HostState::default(),
        frames: 65_536,
        expected_frames: 65_536,
        content_frames: 64_512,
        expected_content_frames: 64_512,
        stalls: 0,
        elapsed_s: 1.26,
    };
    assert_eq!(w.engine_fault(), None, "{w:?}");
}

// MORE MODEL OUTPUT THAN THE MOCK COULD HAVE SENT IS OUR BUG. The gateway cannot invent tokens, so
// there is no gateway behaviour that produces this and no rig capacity that explains it - the only
// honest label is a fault of ours. Exact: one frame over budget is over budget, with no tolerance
// to tune.
#[test]
fn counting_more_content_than_the_mock_can_send_is_this_engines_fault() {
    let w = StreamWindow {
        concurrency: 1024,
        streams: 1024,
        errored: 0,
        error_kinds: StreamErrorKinds::default(),
        host_before: HostState::default(),
        frames: 65_536,
        expected_frames: 65_536,
        content_frames: 64_513,
        expected_content_frames: 64_512,
        stalls: 0,
        elapsed_s: 1.26,
    };
    let why = w.engine_fault().expect("one frame over budget is a fault");
    assert!(why.contains("64513") && why.contains("64512"), "{why}");
    assert!(
        why.contains("counted wrong"),
        "it must read as OUR defect, not as a finding about the gateway: {why}"
    );
}

// EXTRA SSE EVENTS ARE LEGAL AND MUST NOT BE A FAULT. A gateway that inserts pings or re-frames a
// translated stream spends more events than the mock's own layout would - Ledger RIG-11, and the
// whole reason the delivery gate counts content frames. Bounding the event count would fail honest
// gateways for their framing style, so this asserts the bound is NOT there.
#[test]
fn a_gateway_that_adds_its_own_framing_is_not_a_fault() {
    let w = StreamWindow {
        concurrency: 256,
        streams: 256,
        errored: 0,
        error_kinds: StreamErrorKinds::default(),
        host_before: HostState::default(),
        // Well over the mock's own event layout: pings between every delta.
        frames: 40_000,
        expected_frames: 16_384,
        // Content exactly at budget - not one token more.
        content_frames: 16_128,
        expected_content_frames: 16_128,
        stalls: 0,
        elapsed_s: 1.26,
    };
    assert_eq!(
        w.engine_fault(),
        None,
        "extra framing is the gateway's style, not our miscount: {w:?}"
    );
}

// A rate that is not a rate. `fps` divides counts by a wall clock, so a non-finite result means the
// clock or the counter is broken - and an infinity would win every peak search it appeared in.
#[test]
fn a_rate_that_is_not_finite_is_this_engines_fault() {
    let w = StreamWindow {
        concurrency: 8,
        streams: 8,
        errored: 0,
        error_kinds: StreamErrorKinds::default(),
        host_before: HostState::default(),
        frames: 512,
        expected_frames: 512,
        content_frames: 504,
        expected_content_frames: 504,
        stalls: 0,
        elapsed_s: f64::NAN,
    };
    let why = w.engine_fault().expect("a NaN clock is a fault");
    assert!(why.contains("is wrong"), "{why}");
}

// A WINDOW THAT COMPLETED NOTHING HAS NO TAIL LATENCY.
//
// The child computes its percentiles over successes only and prints 0 for an empty set, so a rung
// where every request failed reached the sweep as `p99_us: Measured(0)` - the best tail on the board,
// published for a collapse.
#[test]
fn a_child_window_with_no_successes_reports_no_percentile_rather_than_zero() {
    let line = |ok: i64, p50: i64, p99: i64| crate::loadgen::UgenStats {
        rps: 0.0,
        fail: 500,
        p50_us: p50,
        p99_us: p99,
        ok,
        rig_refused: 0,
        budget_exceeded: 0,
        spawn_failed: false,
    };
    let collapsed = gen_stats_from_child(line(0, 0, 0));
    assert_eq!(collapsed.p50_us, None, "{collapsed:?}");
    assert_eq!(collapsed.p99_us, None, "{collapsed:?}");
    assert_eq!((collapsed.ok, collapsed.fail), (0, 500));

    // A window that served carries the child's percentiles through unchanged.
    let served = gen_stats_from_child(crate::loadgen::UgenStats {
        rps: 100.0,
        ..line(400, 1_200, 9_000)
    });
    assert_eq!(served.p50_us, Some(1_200));
    assert_eq!(served.p99_us, Some(9_000));
}

// ONE HEADER PER NAME holds between the manifest's own two sources too, not only against the rig:
// an always-on header and an egress header of the same name used to both reach the wire.
#[test]
fn a_name_declared_by_both_manifest_sources_reaches_the_wire_once() {
    let a: SocketAddr = "127.0.0.1:1".parse().expect("addr");
    let mut cfg = test_fixture(a, a);
    cfg.static_headers = vec![("X-Route".into(), "static".into())];
    cfg.egress_headers = [(
        "openai".to_string(),
        vec![
            ("x-route".to_string(), "per-column".to_string()),
            ("x-other".to_string(), "kept".to_string()),
        ],
    )]
    .into_iter()
    .collect();
    let h = headers_for(&cfg, Dialect::Openai, "openai");
    let routes: Vec<&(String, String)> = h
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("x-route"))
        .collect();
    assert_eq!(routes.len(), 1, "{h:?}");
    assert_eq!(
        routes[0].1, "static",
        "precedence is the documented order: {h:?}"
    );
    assert!(
        h.contains(&("x-other".to_string(), "kept".to_string())),
        "{h:?}"
    );
}

// THE FLOOR FALLBACK IS A VOTE FOR A RUNG THAT NEVER HAD ONE, NOT A SECOND VOTE.
//
// When every bisection probe above the ascending prefix failed, the bisected ceiling IS the proven
// floor. Its confirmation then fails, the rungs below it fail too, and the fallback used to re-vote
// that same rung with its failed windows discarded - publishing a ceiling above rungs that failed.
#[test]
fn a_floor_that_already_failed_its_confirmation_gets_no_second_vote() {
    let stop = StreamStop::SteppedRungFailed { at: 32 };
    assert!(
        floor_fallback_due(64, false, stop),
        "an unvoted floor gets its confirmation"
    );
    assert!(
        !floor_fallback_due(64, true, stop),
        "a floor that already lost a full confirmation must not be re-voted"
    );
    assert!(
        !floor_fallback_due(0, false, stop),
        "no floor, nothing to confirm"
    );
    assert!(
        !floor_fallback_due(64, false, StreamStop::RestartFailed { at: 32, proven: 64 }),
        "a host we could not even restart the gateway on is not one to publish from"
    );
}

// A RESTART THAT FAILS INSIDE THE STREAM SEARCH IS OURS, and it is named as a restart failure, not
// as an undrained host - and it reaches the grid, which stops measuring against the process.
#[test]
fn a_failed_stream_restart_is_a_harness_error_that_the_grid_can_see() {
    let stop = StreamStop::RestartFailed {
        at: 3088,
        proven: 4096,
    };
    assert_eq!(stop.absent_kind(), Absent::HarnessError);
    let d = stop.describe(6176, MAX_CEILING_STEPDOWNS);
    assert!(
        d.contains("restart") && d.contains("c=3088") && d.contains("c=4096"),
        "{d}"
    );
    assert!(!d.contains("drained"), "not the rig-drain story: {d}");

    let _ = take_stream_restart_failure();
    note_stream_restart_failure("stopping it failed");
    assert_eq!(
        take_stream_restart_failure().as_deref(),
        Some("stopping it failed")
    );
    assert_eq!(
        take_stream_restart_failure(),
        None,
        "taken once, then cleared"
    );
}
