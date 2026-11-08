/* A GATEWAY HANDING MEMORY BACK MUST NEVER READ LIKE ONE LEAKING IT.

39 cells on the 2026-08-03 board carry no time-to-plateau, and they got one sentence between
them: "memory reached no steady state inside the load cap". busbar has the most (11), and its
openai>openai cell ends the window at -0.43 MiB/min - RELEASING memory - which is the opposite
of the reading that sentence invites. `Shape` already separated these three cases; the string
a reader actually sees did not. */
#[test]
fn the_no_plateau_reason_says_which_way_memory_was_moving() {
    use crate::stats::{Shape, Verdict};
    let rate = crate::measurement::Measurement::Measured(-0.43);
    let falling = no_plateau_detail(&Verdict::NotSteady {
        growth_rate_mib_per_min: rate.clone(),
        shape: Shape::Falling,
    });
    assert!(falling.contains("falling"), "{falling}");
    assert!(
        falling.contains("not leaking it"),
        "a falling gateway must be told apart from a leak: {falling}"
    );

    let climbing = no_plateau_detail(&Verdict::NotSteady {
        growth_rate_mib_per_min: rate.clone(),
        shape: Shape::Climbing,
    });
    assert!(climbing.contains("climbing"), "{climbing}");
    assert!(
        !climbing.contains("not leaking"),
        "a climbing gateway must NOT be excused: {climbing}"
    );

    let osc = no_plateau_detail(&Verdict::NotSteady {
        growth_rate_mib_per_min: rate,
        shape: Shape::Oscillating,
    });
    assert!(osc.contains("oscillating"), "{osc}");
    assert!(osc.contains("returns to where it was"), "{osc}");

    // The three must not read alike: that identical wording is the whole defect.
    assert_ne!(falling, climbing);
    assert_ne!(falling, osc);
    assert_ne!(climbing, osc);

    // A window that could not be JUDGED is a different claim from one that moved: it must say it
    // could not be judged, not that memory "reached no steady state", and must not borrow a shape
    // it never established.
    let undecidable = no_plateau_detail(&Verdict::Undecidable(
        crate::stats::Undecidable::WindowTooShort,
    ));
    assert!(undecidable.contains("could not be judged"), "{undecidable}");
    assert!(!undecidable.contains("no steady state"), "{undecidable}");
    assert!(
        !undecidable.contains("climbing") && !undecidable.contains("falling"),
        "{undecidable}"
    );
}

/* A TIME-TO-PLATEAU IS ONLY PUBLISHED BESIDE A WINDOW THAT SETTLED. The load loop notes the first
flat mid-load reading and keeps loading; a cell that read flat at 90s and then climbed for 200 more
ends `NotSteady`, and it published `memory_time_to_plateau_s = 90` beside `memory_plateaued = 0` -
two contradictory facts about one window. */
#[test]
fn a_time_to_plateau_needs_the_final_verdict_to_have_settled() {
    use crate::stats::{Shape, Undecidable, Verdict};
    let steady = Verdict::Steady {
        growth_rate_mib_per_min: Measurement::Measured(0.1),
    };
    assert_eq!(time_to_plateau(Some(90), &steady).copied(), Some(90.0));

    let climbed_after = Verdict::NotSteady {
        growth_rate_mib_per_min: Measurement::Measured(2.0),
        shape: Shape::Climbing,
    };
    let t = time_to_plateau(Some(90), &climbed_after);
    assert_eq!(
        t.copied(),
        None,
        "a mid-load flat reading must not survive a final NotSteady verdict"
    );
    assert!(t.detail().is_some_and(|d| d.contains("climbing")), "{t:?}");

    let unjudged = time_to_plateau(Some(90), &Verdict::Undecidable(Undecidable::WindowTooShort));
    assert_eq!(unjudged.copied(), None);
    assert!(
        unjudged
            .detail()
            .is_some_and(|d| d.contains("could not be judged")),
        "{unjudged:?}"
    );

    // Settled overall but never read flat mid-load: when it settled is unknown, not zero.
    let unknown = time_to_plateau(None, &steady);
    assert_eq!(unknown.copied(), None);
    assert_eq!(unknown.reason(), Some(&Absent::NotMeasured));
}

/* RECOVERED IS READ FROM THE RECOVERY WINDOW, NEVER FROM THE LOAD. It was the median of the 30s
before the series' LAST sample with no check where that sample fell, so a gateway whose tree stopped
being readable once the load ended published its loaded readings as "recovered". */
#[test]
fn recovered_memory_comes_only_from_readings_taken_after_the_load_stopped() {
    let at = |t_s: f64, mib: f64| crate::stats::Sample { t_s, mib };
    // Load until t=300 at ~120 MiB, recovery from t=300 settling at ~10 MiB.
    let mut series: Vec<crate::stats::Sample> = (0..300).map(|t| at(f64::from(t), 120.0)).collect();
    let load_only = series.clone();
    series.extend((300..360).map(|t| at(f64::from(t), 10.0)));

    assert_eq!(recovered_median(&series, 300.0).copied(), Some(10.0));

    // No reading succeeded after t=300: the trailing samples are load readings, so there is no
    // recovered figure - before the fix this published 120 MiB as "recovered".
    let none = recovered_median(&load_only, 300.0);
    assert_eq!(none.copied(), None, "load readings published as recovered");
    assert_eq!(none.reason(), Some(&Absent::NotMeasured));

    // A recovery window with only a few late readings uses those alone, not the load before them.
    let mut sparse = load_only;
    sparse.push(at(355.0, 12.0));
    assert_eq!(recovered_median(&sparse, 300.0).copied(), Some(12.0));
}

// EVERY `Series` FIELD MUST SURVIVE THE ACCUMULATOR.
//
// `process_cell_with` merges each group's Series into the cell's with a hand-written chain, one
// clause per field - and it had no clause for `idle_rss`. The memory group measured a full idle
// window, returned it, and the accumulator dropped it, so `CellMemory.idle_rss_series` published
// empty on every cell and the site's idle sparkline had nothing to draw. Nothing failed, because an
// accumulator that forgets a field is indistinguishable from a group that produced none.
//
// This drives a metric that returns EVERY field populated and asserts every one arrives. A field
// added to `Series` and forgotten in the merge now fails here instead of publishing silence.
#[test]
fn no_series_field_is_dropped_by_the_accumulator() {
    struct FullSeries;
    impl Metric for FullSeries {
        fn name(&self) -> &'static str {
            "full_series"
        }
        fn fields(&self) -> &'static [&'static str] {
            &[]
        }
        fn measure(&self, _ctx: &CellCtx<'_>) -> Measured {
            let rss = vec![crate::record::RssSample {
                t_s: 1,
                rss_mib: Measurement::Measured(10.0),
            }];
            let pt = || crate::record::SweepPoint {
                conc: 1,
                ok: Measurement::Measured(1_000),
                rps: Measurement::Measured(10.0),
                p99_us: Measurement::Measured(20),
                fail: Measurement::Measured(0),
            };
            Measured {
                fields: vec![],
                series: Series {
                    sweep_interrupted: true,
                    frontier: vec![crate::record::FrontierReading::default()],
                    sweep: vec![pt()],
                    sweep_sustained: vec![pt()],
                    rss: rss.clone(),
                    idle_rss: rss,
                    sweep_streams: vec![serde_json::Value::Null],
                },
            }
        }
    }
    let cfg = crate::run::test_fixture(
        "127.0.0.1:1".parse().expect("addr"),
        "127.0.0.1:1".parse().expect("addr"),
    );
    let id = crate::cell::CellId::new("openai", "openai");
    let ctx = CellCtx {
        cfg: &cfg,
        id: &id,
        dialect: crate::ingress::Dialect::Openai,
        min_conc: 1,
        max_conc: 2,
    };
    let metrics: Vec<&dyn Metric> = vec![&FullSeries];
    let (_fields, series, _timings) = process_cell_with(&ctx, &metrics);
    assert!(!series.frontier.is_empty(), "frontier was dropped");
    assert!(!series.sweep.is_empty(), "sweep was dropped");
    assert!(
        !series.sweep_sustained.is_empty(),
        "sweep_sustained was dropped"
    );
    assert!(!series.rss.is_empty(), "rss was dropped");
    assert!(
        !series.idle_rss.is_empty(),
        "idle_rss was dropped - this is the field that was silently discarded on every cell"
    );
    assert!(
        !series.sweep_streams.is_empty(),
        "sweep_streams was dropped"
    );
}

// A SIX-SECOND WINDOW CANNOT SETTLE A SIXTY-SECOND QUESTION.
//
// `stats::window` selects by timestamp, so a series only six seconds long yields a "sixty second
// window" holding six seconds of data - and `plateau_check`'s `n < 4` guard waves it through,
// because at ten readings a second six seconds is sixty samples. Those six seconds were then
// judged against thresholds chosen for a full minute, and a gateway still climbing slowly barely
// drifts across six seconds, so the FIRST load window could declare a plateau and publish a steady
// state the gateway had not reached.
#[test]
fn a_plateau_verdict_needs_a_window_that_actually_lasted() {
    assert!(
        !super::window_is_long_enough(6.0),
        "six seconds cannot answer a question posed over sixty"
    );
    assert!(
        !super::window_is_long_enough(super::MEMORY_PLATEAU_WINDOW_S - 0.1),
        "just short is still short: the thresholds were chosen for the full span"
    );
    assert!(
        super::window_is_long_enough(super::MEMORY_PLATEAU_WINDOW_S),
        "the full window is exactly what the thresholds were calibrated on"
    );
    assert!(super::window_is_long_enough(300.0), "and longer is fine");
}

// A DEAD SAMPLER MUST NOT READ AS A SETTLED GATEWAY.
//
// The load loop snapshots the shared RSS series between windows and breaks the moment
// `plateau_check` says `Steady`. When the sampler thread dies - a panic on an unexpected /proc
// shape for one gateway's tree is enough - the series stops growing, so every later snapshot is
// the same frozen tail. A frozen tail has zero drift and zero spread, which is exactly what
// steady looks like. The loop would then publish "settled after N seconds" and a peak that is
// really "whatever was captured before the thread died", about a gateway that may have gone on
// climbing for minutes - and `let _ = sampler.join()` had already thrown away the panic, so
// nothing in the log or the artifact said the readings had stopped.
//
// The discriminator is growth. At ten readings a second a LIVE sampler adds samples between
// windows, and a genuinely settled gateway still produces new samples that happen to be flat.
// No new samples at all is not a measurement of the gateway.
#[test]
fn a_frozen_rss_series_is_not_a_settled_gateway() {
    assert!(
        !super::steady_is_believable(120, 120),
        "a series that did not grow between windows is the sampler's death, not the gateway's calm"
    );
    assert!(
        !super::steady_is_believable(120, 119),
        "a series that SHRANK cannot be evidence of anything"
    );
    assert!(
        super::steady_is_believable(120, 121),
        "one new sample is a live sampler, and a settled gateway still produces flat samples"
    );
    assert!(
        super::steady_is_believable(0, 40),
        "the first window has nothing to compare against and must be allowed to settle"
    );
}

use super::*;

/// A group that lies: it declares two fields and returns one. The engine must fill the gap with
/// an absence carrying a reason, never leave the key out, because a missing key and a null are
/// different statements and only one of them is true.
struct Forgetful;
impl Metric for Forgetful {
    fn name(&self) -> &'static str {
        "forgetful"
    }
    fn fields(&self) -> &'static [&'static str] {
        &["present", "forgotten"]
    }
    fn measure(&self, _ctx: &CellCtx<'_>) -> Measured {
        let f: Filled = vec![("present", Measurement::Measured(1.0))];
        f.into()
    }
}

fn ctx_for<'a>(cfg: &'a RunConfig, id: &'a CellId) -> CellCtx<'a> {
    CellCtx {
        cfg,
        id,
        dialect: Dialect::Openai,
        min_conc: 1,
        max_conc: 2,
    }
}

fn a_config() -> RunConfig {
    RunConfig {
        probe_timeout: std::time::Duration::from_millis(1),
        ..crate::run::test_fixture(
            "127.0.0.1:1"
                .parse()
                .expect("a literal loopback address parses"),
            "127.0.0.1:2"
                .parse()
                .expect("a literal loopback address parses"),
        )
    }
}

#[test]
fn a_declared_field_that_a_group_does_not_return_becomes_an_absence_not_a_missing_key() {
    let cfg = a_config();
    let id = CellId::new("openai", "openai");
    let ctx = ctx_for(&cfg, &id);

    // The PRODUCTION accumulator, not a retyped copy of its fill loop.
    let (out, _series, _timings) = process_cell_with(&ctx, &[&Forgetful]);

    assert!(
        out.contains_key("forgotten"),
        "the key must exist even though the group skipped it"
    );
    assert_eq!(out["forgotten"].reason(), Some(&Absent::NotMeasured));
    assert!(
        out["forgotten"]
            .detail()
            .is_some_and(|d| d.contains("forgetful")),
        "the absence must name the group that failed to fill it: {:?}",
        out["forgotten"].detail()
    );
    assert_eq!(out["present"].value(), Some(&1.0));
}

/// The list is the engine's measurement surface, so a group appearing in it twice, or two groups
/// claiming the same artifact field, would publish one number under two procedures.
#[test]
fn no_two_groups_claim_the_same_artifact_field() {
    let mut seen: BTreeMap<&'static str, &'static str> = BTreeMap::new();
    for m in METRICS {
        for f in m.fields() {
            if let Some(other) = seen.insert(f, m.name()) {
                panic!("field {f} is claimed by both {other} and {}", m.name());
            }
        }
    }
    assert!(
        !seen.is_empty(),
        "the engine must declare at least one metric"
    );
}

/// Every group must be OBSERVABLE - it declares scalar fields, or it fills a series lane.
///
/// It used to demand scalar fields from every group, on the reasoning that a group with none is a
/// procedure with no way to be observed. That reasoning is right and the test was too narrow: the
/// `throughput` group's entire output is the FRONTIER, which is a sequence and so travels on the
/// series rather than in `Filled` (see `frontier.rs`). It is observable - more so than the two
/// scalars it replaced, since every reading carries its own evidence - just not through `fields()`.
///
/// So the property is unchanged in spirit and widened in letter: a group must produce SOMETHING.
/// The series-only exception is named explicitly rather than allowed by a blanket, so a group that
/// silently stops filling anything still fails here.
const SERIES_ONLY_GROUPS: &[&str] = &["throughput"];

#[test]
fn every_group_declares_what_it_fills() {
    for m in METRICS {
        assert!(!m.name().is_empty());
        if SERIES_ONLY_GROUPS.contains(&m.name()) {
            assert!(
                m.fields().is_empty(),
                "{} is listed series-only but declares scalar fields - one or the other is stale",
                m.name()
            );
            continue;
        }
        assert!(!m.fields().is_empty(), "{} declares no fields", m.name());
    }
}

// IDLE MUST COME FROM A PROCESS AT REST.
//
// METRICS runs Throughput before Memory on the same process, so by the time Memory reads RSS the
// gateway has just been driven through a full peak-finding sweep, and a post-load reading
// published as "idle" would badly overstate a cold process's footprint.
//
// When the harness does not own the gateway's lifetime (relaunch: None) it cannot put it back at
// rest, and the only honest answer is an absence carrying that reason - never the post-load
// number. This pins the absence so the polluted reading cannot come back as a silent default.
//
// It drives `idle_at_rest` directly: through `Memory::measure` the fixture's process tree does not
// exist, so the group returned at the "no process" check before ever reaching this branch, and the
// test passed on an absence that had nothing to do with the gateway's lifetime.
#[test]
fn idle_memory_is_absent_when_the_gateway_cannot_be_returned_to_rest() {
    let cfg = a_config();
    assert!(
        cfg.relaunch.is_none(),
        "this fixture owns no gateway lifetime"
    );
    let mut pid = 4242u32;
    let mut idle_series = Vec::new();
    let idle = idle_at_rest(&cfg, &mut pid, &mut idle_series)
        .expect("not owning the lifetime is an idle absence, not an abort of the group");
    assert_eq!(
        idle.copied(),
        None,
        "idle must not be published from a process that served load"
    );
    assert_eq!(idle.reason(), Some(&Absent::NotMeasured));
    assert!(
        idle.detail()
            .is_some_and(|d| d.contains("does not own this gateway's lifetime")),
        "the absence must carry the reason it could not be taken: {:?}",
        idle.detail()
    );
    assert_eq!(pid, 4242, "no restart, so the measured pid must not move");
    assert!(idle_series.is_empty(), "no idle window was opened");
}

// A FAILED RESTART ABORTS THE WHOLE MEMORY GROUP. The old behaviour marked only idle absent
// (NotMeasured) and fell through to the sampler and the load window - against a gateway in an
// unknown state, with the pre-restart pid still in hand - so every number that window produced
// was the rig's own failure wearing the gateway's name. This pins the fix: EVERY declared field
// is absent with reason HarnessError, the shared detail says the window never ran, and no
// series is produced (the sampler and load window are never started).
//
// The fixture: a real marker process stands in for the gateway tree so `root_pid` resolves, and
// the relaunch spec's stop path matches nothing (stopping "succeeds" instantly) while its
// binary does not exist, so `restart_to_rest` fails fast on any platform - the FAILURE path
// needs no taskset, unlike run.rs's restart tests that need the launch to SUCCEED.
#[test]
fn a_failed_restart_to_rest_makes_every_memory_field_a_harness_error_and_skips_the_window() {
    if std::process::Command::new("sh")
        .args(["-c", "command -v python3"])
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skipping a_failed_restart_to_rest_makes_every_memory_field_a_harness_error_and_skips_the_window: no python3 on this platform");
        return;
    }
    let marker = format!("otb-test-memory-restart-abort-{}", std::process::id());
    // Killed on EVERY exit path, a failing assertion included: a leaked marker would outlive the
    // test and could make a later run's `root_pid` match two processes.
    struct KillOnDrop(std::process::Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let child = KillOnDrop(
        std::process::Command::new("python3")
            .args(["-c", "import time,sys; time.sleep(120)", &marker])
            .spawn()
            .expect("spawn a marker process to stand in for the gateway tree"),
    );

    let mut cfg = a_config();
    cfg.runtime = crate::manifest::Runtime::Native {
        proc_match: marker.clone(),
    };
    let port = {
        let l =
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port to pick one");
        l.local_addr().expect("addr").port()
    };
    cfg.relaunch = Some(crate::launch::LaunchSpec {
        runtime: crate::manifest::Runtime::Native {
            // Deliberately NOT the marker: the stop path must succeed (nothing to stop) so the
            // failure under test is the relaunch itself, and the marker process survives to
            // prove the load window never drove anything.
            proc_match: format!("{marker}-relaunch-matches-nothing"),
        },
        kind: crate::launch::LaunchKind::Native {
            binary: "/nonexistent-otb-gateway-binary".into(),
            args: vec![marker.clone()],
            env: vec![],
            env_unset: vec![],
        },
        cores: "0".into(),
        port,
        ready_budget: std::time::Duration::from_millis(200),
        boot_backoff: std::time::Duration::from_millis(10),
        pre_launch: None,
    });

    // The marker process must be visible before the group runs, or the test would exercise the
    // earlier "no process tree" path instead of the restart failure.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while crate::rss::root_pid(&cfg.runtime).value().is_none()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        crate::rss::root_pid(&cfg.runtime).value().is_some(),
        "the marker process never became visible to root_pid"
    );

    let id = CellId::new("openai", "openai");
    let ctx = ctx_for(&cfg, &id);
    let produced = Memory.measure(&ctx);
    drop(child);

    let filled: BTreeMap<_, _> = produced.fields.into_iter().collect();
    for field in Memory.fields() {
        let m = filled
            .get(field)
            .unwrap_or_else(|| panic!("the memory group declares {field} and must fill it"));
        assert_eq!(
            m.copied(),
            None,
            "{field}: nothing may be measured against a gateway in an unknown state"
        );
        assert_eq!(
            m.reason(),
            Some(&Absent::HarnessError),
            "{field}: a failed restart is the HARNESS's failure, and every field says so"
        );
        assert!(
            m.detail()
                .unwrap_or_default()
                .contains("could not be restarted to rest"),
            "{field}: the detail must name the restart failure: {:?}",
            m.detail()
        );
        assert!(
            m.detail().unwrap_or_default().contains("did not run"),
            "{field}: the detail must say the memory window never ran: {:?}",
            m.detail()
        );
    }
    assert!(
        produced.series.rss.is_empty(),
        "no sampler ran, so there is no series to carry"
    );
}

// ── AddedLatency's pure helpers ─────────────────────────────────────────────────────────────

#[test]
fn a_clean_leg_needs_at_least_one_success_and_zero_failures() {
    assert!(clean_c1_leg(1, 0));
    assert!(clean_c1_leg(500, 0));
    assert!(
        !clean_c1_leg(0, 0),
        "no requests completed at all is not a clean reading"
    );
    assert!(!clean_c1_leg(0, 3), "all failures is not a clean reading");
    assert!(
        !clean_c1_leg(497, 3),
        "even one failure disqualifies the leg"
    );
}

#[test]
fn added_latency_diff_publishes_below_resolution_never_a_clamped_zero() {
    assert_eq!(
        added_latency_diff(1_200, 80).copied(),
        Some(1_120.0),
        "the ordinary case is a plain subtraction"
    );
    assert_eq!(
        added_latency_diff(0, 0).copied(),
        Some(0.0),
        "an exact tie is a measured zero, not an absence"
    );
    // The gateway leg reading BELOW the direct leg is rig noise (two separate windows on a real
    // box), not the gateway outrunning the upstream it proxies. It must publish as
    // BelowResolution - the same rule as added_ttft/added_gap - never as a clamped 0 that claims
    // a precision the rig does not have, and never as a wrap or a negative.
    let below = added_latency_diff(50, 200);
    assert_eq!(below.copied(), None);
    assert_eq!(below.reason(), Some(&Absent::BelowResolution));
    assert!(
        below
            .detail()
            .unwrap_or_default()
            .contains("below what this rig can resolve"),
        "the absence must say it is a resolution limit, not a hole"
    );
}

// ── the reachability list itself carries the two new groups ────────────────────────────────
//
// `no_two_groups_claim_the_same_artifact_field` and `every_group_declares_what_it_fills` above
// already run over `METRICS`, so being IN that list is what makes a group reachable per this
// file's own module doc ("a metric is in `METRICS` or it does not exist"). This test names the
// field those tests would silently miss if a future edit dropped `AddedLatency` back out of the
// list without anything else failing.
//
// It used to say "adding `AddedLatency`/`SustainedThroughput` to that list", which was false for
// the second: `SustainedThroughput` was never in `METRICS`, had no `impl Metric`, and has now
// been deleted along with the group the frontier replaced.
// ── the two concurrent-stream groups ────────────────────────────────────────────────────────

// The same gate for the concurrent-stream group. `CellStream` declared these fields and NOTHING in
// the engine ever filled them: the artifact carried the keys, always null, on every cell of every
// gateway ever published. A group that falls back out of `METRICS` returns the board to exactly
// that state with nothing else failing, which is what this holds.
//
// ONE GROUP, NOT TWO. `cpu_fps` is retired - see `run.rs`'s own note for the field evidence: 4 of
// 16 cells had it INVERTED below the proven delivery boundary, 5 were redundant with
// `streams_sustained_fps`, and 7 were measured at a concurrency where the delivery gate did not
// hold at all.
#[test]
fn the_stream_group_is_reachable_from_metrics() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name()).collect();
    assert!(names.contains(&"streams_sustained"), "METRICS = {names:?}");
    let all_fields: Vec<&str> = METRICS
        .iter()
        .flat_map(|m| m.fields().iter().copied())
        .collect();
    for f in ["streams_sustained", "streams_sustained_fps"] {
        assert!(
            all_fields.contains(&f),
            "{f} is not declared by any group in METRICS: {all_fields:?}"
        );
    }
    // The two advisory-note inputs are on the surface too: `c1_note`/`stream_c1_note` are built
    // from them in `suite.rs`, and a group that stopped filling them would silently drop the note
    // rather than fail, since a note is a plain `Option<String>` with no absence to carry.
    for f in [
        "gateway_c1_samples",
        "direct_c1_samples",
        "gateway_c1_frames",
        "direct_c1_frames",
    ] {
        assert!(
            all_fields.contains(&f),
            "{f} is not declared by any group in METRICS: {all_fields:?}"
        );
    }
}

// THE FRAMES COME FROM THE EGRESS, SO THE EGRESS DECIDES WHETHER THERE ARE ANY.
//
// All three stream groups guarded on the INGRESS dialect alone. In the 2026-07-28 field run that
// let 20 served cells through with an ingress that streams (openai, anthropic) and an egress the
// mock cannot stream (bedrock, cohere, gemini). Every window came back with
// `stream_errors == streams` and `frames: 0` at every concurrency from 1 to 4096, and the cells
// published "no concurrency from 1 to 4096 passed the gate" - the rig's own limit, recorded as
// the gateway failing to stream.
#[test]
fn a_cell_whose_egress_cannot_stream_is_the_rigs_limit_not_the_gateways() {
    let cfg = crate::run::test_fixture(
        "127.0.0.1:1".parse().expect("addr"),
        "127.0.0.1:1".parse().expect("addr"),
    );
    let ctx = |ing: Dialect, eg: &str| CellCtx {
        cfg: &cfg,
        id: Box::leak(Box::new(crate::cell::CellId::new(ing.as_str(), eg))),
        dialect: ing,
        min_conc: 1,
        max_conc: 4,
    };

    // The exact field pairings, and the end that blocks each one.
    assert_eq!(
        stream_blocked_by(&ctx(Dialect::Openai, "bedrock")).as_deref(),
        Some("bedrock")
    );
    assert_eq!(
        stream_blocked_by(&ctx(Dialect::Anthropic, "bedrock")).as_deref(),
        Some("bedrock")
    );
    assert_eq!(
        stream_blocked_by(&ctx(Dialect::Openai, "cohere")).as_deref(),
        Some("cohere")
    );
    assert_eq!(
        stream_blocked_by(&ctx(Dialect::Openai, "gemini")).as_deref(),
        Some("gemini")
    );

    // The ingress end still blocks, and is named when it is the one at fault.
    assert_eq!(
        stream_blocked_by(&ctx(Dialect::Gemini, "openai")).as_deref(),
        Some("gemini")
    );
    assert_eq!(
        stream_blocked_by(&ctx(Dialect::Bedrock, "openai")).as_deref(),
        Some("bedrock")
    );

    // Both ends streamable: the question is real and must actually be asked.
    assert_eq!(stream_blocked_by(&ctx(Dialect::Openai, "openai")), None);
    assert_eq!(
        stream_blocked_by(&ctx(Dialect::Anthropic, "anthropic")),
        None
    );
    assert_eq!(stream_blocked_by(&ctx(Dialect::Openai, "anthropic")), None);
}

// THE GAP DISTRIBUTION IS INSIDE THE STREAM.
//
// `added_gap_p99_us` was absent on all 69 served cells of the 2026-07-28 run, suppressed with
// "one stream was taken, which cannot support a 99th percentile". That is true of TTFT, which a
// stream produces exactly one of. It is not true of gaps: a stream carrying STREAM_FRAME_BUDGET
// frames yields that many gaps minus one. charts.py draws two charts from these fields, so the
// board had a permanently empty chart while the data sat in the frame offsets.
#[test]
fn the_gap_percentiles_come_from_the_gaps_inside_one_stream() {
    // Offsets in us: gaps of 10, 10, 10, 10, 100. The tail is the whole point of a p99, and a
    // missing one costs a reader exactly that.
    let offs: [u64; 6] = [0, 10, 20, 30, 40, 140];
    assert_eq!(gap_percentile_us(&offs, 0.50), Some(10.0));
    assert_eq!(
        gap_percentile_us(&offs, 0.99),
        Some(100.0),
        "the p99 must reach the tail, not repeat the median"
    );
    // Nearest-rank never interpolates: every published percentile is a gap that really occurred.
    let real: Vec<f64> = vec![10.0, 100.0];
    for p in [0.5, 0.9, 0.99, 1.0] {
        let v = gap_percentile_us(&offs, p).expect("five gaps have a percentile");
        assert!(
            real.contains(&v),
            "p{p} returned {v}, which no pair of frames produced"
        );
    }
    // A stream with one frame has no inter-frame time. Absent, never a zero - a zero would read
    // as instant delivery.
    assert_eq!(gap_percentile_us(&[0], 0.99), None);
    assert_eq!(gap_percentile_us(&[], 0.5), None);
}

// A COLUMN THAT CAN NEVER HOLD A NUMBER IS EITHER MEASURED OR DELETED.
//
// `added_ttft_p99_us` was absent on all 69 served cells of the 2026-07-28 run - and on every run
// before it - because one stream yields exactly one time-to-first-token. The excuse was true;
// keeping the field anyway was the defect. charts.py draws from it.
//
// The fix is samples, and they are cheap: a TTFT reads ONE frame and stops, so a sample is
// milliseconds rather than the ~1.3s a full 64-frame paced stream takes.
#[test]
fn a_ttft_percentile_needs_samples_and_the_sample_count_makes_one_real() {
    // 100 is the smallest count where a 99th percentile is a real order statistic rather than a
    // restatement of the maximum: nearest-rank puts it at index 98 of 0..=99, not at the top.
    // (No `assert!(SAMPLES >= 100)` here: an assertion over a constant cannot fail, which is the
    // exact species of dead guard this audit spent the day removing. The rank checks below use
    // the constant and would break if it were lowered, which is the real protection.)
    // The rank is the ENGINE's, not this module's: `stats::nearest_rank_index` is what
    // `ttft_pct` calls, and calling it here too is what makes this a check on production rather
    // than on a formula retyped in a test. It used to be retyped, and it was retyped with the
    // ceil convention while `gen.rs` and `search.rs` used floor - ledger SRCH-04, the split this
    // now cannot come back from.
    let idx_of = crate::stats::nearest_rank_index;
    assert_eq!(idx_of(STREAM_TTFT_SAMPLES, 0.99), 98);
    assert!(
        idx_of(STREAM_TTFT_SAMPLES, 0.99) < STREAM_TTFT_SAMPLES - 1,
        "the p99 must not be the max, or it is not a percentile"
    );
    // One sample cannot support one: that was the whole problem, and it is why the field was
    // empty rather than wrong.
    assert_eq!(
        idx_of(1, 0.99),
        0,
        "with a single sample the p99 IS that sample"
    );

    // Nearest-rank on the one convention, so a published percentile is always a value some
    // stream actually produced and the same word means the same rank everywhere on the board.
    let v: Vec<u64> = (1..=100).collect();
    assert_eq!(v[idx_of(v.len(), 0.99)], 99);
    assert_eq!(v[idx_of(v.len(), 0.50)], 50);
}

// WHAT A CELL COST, PER GROUP, IN THE ARTIFACT.
//
// A wall-clock total cannot answer the only question worth asking about a slow run. "Thirteen
// minutes a cell" might be the TTFT sample set, a stream ladder reaching a higher rung, or a
// gateway that got slower, and those have nothing in common as responses. Without per-group
// seconds the answer is another run with a stopwatch; with them it is arithmetic on committed
// JSON.
#[test]
fn every_group_that_runs_reports_what_it_cost() {
    let cfg = crate::run::test_fixture(
        "127.0.0.1:1".parse().expect("addr"),
        "127.0.0.1:1".parse().expect("addr"),
    );
    let id = crate::cell::CellId::new("openai", "openai");
    let ctx = CellCtx {
        cfg: &cfg,
        id: &id,
        dialect: Dialect::Openai,
        min_conc: 1,
        max_conc: 2,
    };

    // A group that measures nothing still took time and still reports it: a zero-cost group and
    // an unreported one are different facts, and only one of them is true.
    let (_, _, timings) = process_cell_with(&ctx, &[&Streaming]);
    assert_eq!(
        timings.len(),
        1,
        "one group ran, so one cost is reported: {timings:?}"
    );
    assert!(
        timings.contains_key("streaming"),
        "keyed by the group's own name: {timings:?}"
    );
    assert!(timings["streaming"] >= 0.0 && timings["streaming"].is_finite());

    // Every group in the list is accounted for, so a breakdown always sums to the whole - a
    // missing group would silently make the expensive one look cheaper than it was.
    // Two cheap groups rather than the real METRICS list: the property is the accumulator's, and
    // driving every real group (throughput sweeps, stream ladders, 100 TTFT probes per leg) in a
    // unit test made its runtime depend on each group's refused-connection behaviour.
    let groups: [&dyn Metric; 2] = [&Forgetful, &Streaming];
    let (_, _, all) = process_cell_with(&ctx, &groups);
    assert_eq!(all.len(), groups.len(), "every group must report: {all:?}");
    for m in groups {
        assert!(all.contains_key(m.name()), "{} reported no cost", m.name());
    }
}

// A p99 BELOW ITS OWN p50 IS TWO POPULATIONS WEARING ONE NAME.
//
// The 2026-07-28 validation run published exactly that on every streaming cell it measured:
// 523/428, 514/451, 501/359, 513/461. No percentile pair over one distribution can do it. The
// cause was that the p50 came from a single full stream while the p99 came from a hundred
// single-token samples - both defensible numbers, neither comparable to the other.
//
// Asserted as an ordering over one sample set, which is the property that was violated.
#[test]
fn the_ttft_percentiles_come_from_one_sample_set_so_p99_can_never_sit_below_p50() {
    // The PRODUCTION percentile and difference (`sorted_sample_pct`, `added_at`), not a retyped
    // rank formula: the retyped one carried the ceil convention production abandoned (SRCH-04).
    let pct = |v: &[u64], p: f64| sorted_sample_pct(v, p).expect("non-empty sample set");
    // A realistic spread: most tokens fast, a slow tail. The tail is what a p99 is FOR, and the
    // old shape hid it behind a median taken from a different measurement.
    let mut samples: Vec<u64> = (0..99).map(|i| 400 + i % 30).collect();
    samples.push(9_000);
    samples.sort_unstable();

    let p50 = pct(&samples, 0.50);
    let p99 = pct(&samples, 0.99);
    assert!(
        p99 >= p50,
        "p99 {p99} sits below p50 {p50}, which one distribution cannot produce"
    );
    assert!(
        p99 > p50,
        "and on a distribution with a real tail it must be strictly above"
    );

    // The differencing keeps that ordering: both legs are percentiles of their own sample set at
    // the same rank, so a gateway that adds a constant adds it at every percentile.
    let direct: Vec<u64> = samples.iter().map(|v| v / 2).collect();
    let add = |p: f64| {
        added_at(
            sorted_sample_pct(&samples, p),
            sorted_sample_pct(&direct, p),
            |_, _| "below".into(),
            || "missing".into(),
        )
        .copied()
        .expect("the gateway leg is slower at every rank, so the difference is measured")
    };
    assert!(
        add(0.99) >= add(0.50),
        "the ADDED figures must hold the same ordering"
    );

    // One sample cannot support a p99 that means anything: it is that sample, and it equals the
    // p50 rather than sitting below it. No samples is no percentile, never a zero.
    assert_eq!(pct(&[500], 0.99), pct(&[500], 0.50));
    assert_eq!(sorted_sample_pct(&[], 0.5), None);
}

// A DIFFERENCE THE RIG CANNOT SEE IS NOT A MEASURED ZERO.
//
// Both streaming legs carry the mock's ~20ms pacing, so an added-gap figure is a microsecond
// signal extracted by differencing two ~20,000us numbers. When the gateway's tail at a percentile
// lands under the mock's, the raw difference is negative - impossible for a proxy, therefore
// noise. Clamping it to 0 published "added nothing" with precision this rig does not have, and
// produced pairs that cannot exist: aisix p50=4 p99=0, helicone p50=3 p99=0, plano p50=1 p99=0,
// tensorzero p50=1 p99=0 in the 2026-07-28 run.
#[test]
fn a_percentile_difference_below_the_rigs_resolution_is_absent_not_zero() {
    // The PRODUCTION rule (`added_at`, which both the TTFT and gap families publish through).
    let judge = |gw: f64, mock: f64| {
        added_at(
            Some(gw),
            Some(mock),
            |_, _| "below".into(),
            || "missing".into(),
        )
    };

    // A real addition survives, unchanged.
    assert_eq!(judge(20_015.0, 20_000.0).copied(), Some(15.0));
    // Equal legs are a genuine, measurable zero: the gateway added nothing detectable AND the
    // comparison was valid. That must still publish 0, not absent.
    assert_eq!(judge(20_000.0, 20_000.0).copied(), Some(0.0));
    // The gateway "faster" than the upstream it proxies is the impossible case: below
    // resolution, never a clamped zero.
    let faster = judge(19_996.0, 20_000.0);
    assert_eq!(
        faster.copied(),
        None,
        "a proxy cannot beat its own upstream"
    );
    assert_eq!(faster.reason(), Some(&Absent::BelowResolution));
    // A missing leg is not a measured difference either.
    let missing = added_at(None, Some(1.0), |_, _| "below".into(), || "missing".into());
    assert_eq!(missing.reason(), Some(&Absent::NotMeasured));

    // And the property that was violated: a p99 that IS published can never sit below a p50
    // published from the same distribution. Both are asserted published, so this cannot pass
    // vacuously.
    let mut samples: Vec<u64> = (0..100).map(|i| 20_000 + i % 7).collect();
    samples.sort_unstable();
    let mock: Vec<u64> = samples.iter().map(|v| v - 3).collect();
    let at = |p: f64| {
        judge(
            sorted_sample_pct(&samples, p).expect("samples"),
            sorted_sample_pct(&mock, p).expect("samples"),
        )
        .copied()
        .expect("the gateway leg is 3us slower at every rank")
    };
    let (p50, p99) = (at(0.50), at(0.99));
    assert!(p99 >= p50, "p99 {p99} sits below p50 {p50}");
}

/* AN ERROR ANSWER IS NOT A STREAM. The streaming group timed whatever frames came back and never
looked at the status, so a gateway answering 500 with an event-stream body (one error frame) had
that frame's arrival published as its time to first event - how fast it refused, scored as how
fast it streamed. The stream search in run.rs already files non-2xx as an errored stream. */
#[test]
fn a_stream_answered_with_an_error_status_is_not_timed() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(mut sock) = sock else { continue };
            // Read the request head and its declared body, so closing does not reset the client.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = match sock.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&chunk[..n]);
                if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let _ = sock.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\ncontent-type: text/event-stream\r\n\
                  connection: close\r\n\r\ndata: {\"error\":\"upstream down\"}\n\n",
            );
        }
    });
    let cfg = crate::run::test_fixture(addr, addr);
    let id = CellId::new("openai", "openai");
    let ctx = ctx_for(&cfg, &id);
    let filled: BTreeMap<_, _> = Streaming.measure(&ctx).fields.into_iter().collect();
    for f in [
        "added_ttft_p50_us",
        "added_ttft_p99_us",
        "added_gap_p50_us",
        "added_gap_p99_us",
    ] {
        let m = &filled[f];
        assert_eq!(m.copied(), None, "{f}: an error answer was timed: {m:?}");
        assert!(
            m.detail().is_some_and(|d| d.contains("HTTP 500")),
            "{f}: the absence must name the status it refused: {m:?}"
        );
    }
}

// A RESTART THAT FAILED INSIDE THE STREAM SEARCH STOPS THE CELL'S LATER GROUPS. The stream search
// records the failure on this thread and the grid poisons the rest of the run on it, but the groups
// after Streams in the SAME cell used to run anyway, against a process the harness could no longer
// vouch for, and publish what they read under the gateway's name.
struct FailsItsRestart;
impl Metric for FailsItsRestart {
    fn name(&self) -> &'static str {
        "fails-its-restart"
    }
    fn fields(&self) -> &'static [&'static str] {
        &["stream_field"]
    }
    fn measure(&self, _ctx: &CellCtx<'_>) -> Measured {
        crate::run::note_stream_restart_failure("the relaunch never became ready");
        let f: Filled = vec![(
            "stream_field",
            Measurement::absent_because(Absent::HarnessError, "restart failed"),
        )];
        f.into()
    }
}

struct MustNotRun(std::sync::atomic::AtomicBool);
impl Metric for MustNotRun {
    fn name(&self) -> &'static str {
        "must-not-run"
    }
    fn fields(&self) -> &'static [&'static str] {
        &["later_field"]
    }
    fn measure(&self, _ctx: &CellCtx<'_>) -> Measured {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        let f: Filled = vec![("later_field", Measurement::Measured(42.0))];
        f.into()
    }
}

#[test]
fn a_failed_stream_restart_stops_the_cells_later_groups() {
    let cfg = a_config();
    let id = CellId::new("openai", "openai");
    let ctx = ctx_for(&cfg, &id);
    let later = MustNotRun(std::sync::atomic::AtomicBool::new(false));
    let (out, _series, _timings) = process_cell_with(&ctx, &[&FailsItsRestart, &later]);
    assert!(
        !later.0.load(std::sync::atomic::Ordering::SeqCst),
        "a group after a failed restart must not measure the unvouched process"
    );
    let m = &out["later_field"];
    assert_eq!(m.reason(), Some(&Absent::HarnessError), "{m:?}");
    assert!(
        m.detail().is_some_and(|d| d.contains("never became ready")),
        "the absence must carry the restart's own reason: {m:?}"
    );
    // Peeked, not taken: the grid still sees it and poisons the rest of the run.
    assert!(crate::run::stream_restart_failure_pending().is_some());
}
