// A DEFINITION THAT DRIFTS FROM THE CODE IS WORSE THAN NO DEFINITION.
//
// The retired throughput gate is the case: every surface said "p99 < 1 s" while the engine enforced
// 20 ms - a bar 96% of the 1632 recorded rungs pass, against 57% for the real one - so a reader
// reasoning carefully about our numbers reasoned about a test we never ran. This asserts the
// published text is generated FROM the constants, by checking it says what those constants
// currently say. Change a constant without regenerating and this fails.
#[test]
fn the_cost_definition_names_the_concurrency_that_was_actually_used() {
    // GENERATED FROM THE CONSTANT, not retyped beside it. The whole value of the cost column is
    // that every gateway was measured at the SAME concurrency; a definition naming a different
    // number than the code used would describe a comparison that never happened, and is exactly
    // the drift this file's definitions exist to make impossible.
    let d = super::metric_definitions();
    let c = d.get("perf.cost").expect("cost has a definition");
    assert!(
        c.contains(&format!("c={}", crate::metric::COST_WINDOW_CONCURRENCY)),
        "the cost definition must name the concurrency the window ran at"
    );
    // It must also state the two refusals, because a reader who does not know a failed window
    // publishes NO cost will read an absent cell as a missing measurement rather than a refusal.
    assert!(
        c.contains("failure"),
        "must state that a failed window publishes no cost"
    );
    assert!(c.contains("SWAPPING"), "must state the major-fault case");
}

#[test]
fn the_published_definitions_are_generated_from_the_constants_that_ran() {
    let d = super::metric_definitions();
    let f = d
        .get("perf.frontier")
        .expect("the frontier has a definition");
    for us in crate::frontier::P99_BOUNDS_US {
        let label = format!("{}ms", us / 1000);
        assert!(
            f.contains(&label),
            "the frontier definition must name every declared bound; missing {label}"
        );
    }
    assert!(
        f.contains("no latency bound"),
        "the unbounded reading must be described, not left implied: {f}"
    );
    // The two fields a reader needs in order to CHECK a reading rather than trust it.
    assert!(f.contains("lower_bound"), "{f}");
    assert!(f.contains("first_disqualified_conc"), "{f}");

    let st = d
        .get("stream.streams_sustained")
        .expect("the stream gate has a definition");
    assert!(
        st.contains(&format!("{}x", crate::run::STREAM_STALL_MULTIPLIER)),
        "the stall bound must come from the constant: {st}"
    );
    assert!(
        st.contains(&format!(
            "{:.1}%",
            crate::run::STREAM_MAX_ERROR_RATIO * 100.0
        )),
        "the error bar must come from the constant: {st}"
    );

    // NOTHING may claim a 1 s tail bound anywhere. That number described a gate this engine never
    // enforced, and it must not come back through prose.
    for (k, v) in &d {
        assert!(
            !v.contains("p99 < 1 s") && !v.contains("under 1 s"),
            "{k} states a 1 s bound, which no gate in this engine has used: {v}"
        );
    }
}

use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

// A RUN MUST NOT DECLARE ITSELF DEGRADED WHILE CARRYING THE DATA IT SAYS IT SKIPPED. The
// consumer reads these three flags to spot a probe-only run and refuses to publish one over a
// complete one, so they are a claim about the run. `..Default::default()` left cell_stream
// false on every snapshot the engine has ever written: the run stating it did not measure
// streaming while every served cell carried a full streaming block.
#[test]
fn a_full_run_declares_every_phase_it_actually_measured() {
    let dir = tmpdir("phases");
    let gw = serve(200);
    let cfg = cfg_for(&dir, gw);
    let paths = run_suite_with(&cfg, gw, crate::metric::METRICS).expect("the suite should write");
    let text = std::fs::read_to_string(&paths.current).expect("current file");
    let back: ResultSnapshot = serde_json::from_str(&text).expect("its own output must parse");

    assert!(back.matrix.cell_perf_sweep, "the perf sweep ran");
    assert!(
        back.matrix.cell_stream,
        "the streaming group is in METRICS and ran"
    );
    assert_eq!(
        back.matrix.cell_memory,
        Some(true),
        "the memory group is in METRICS and ran"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

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

fn cfg_for(dir: &Path, mock: SocketAddr) -> SuiteConfig {
    SuiteConfig {
        gw_dir: std::path::PathBuf::from("."),
        gw_cores: "0-3".into(),
        manifest: Manifest {
            port: 1,
            egress: vec![],
            ..crate::manifest::test_fixture()
        },
        mock_addr: mock,
        results_dir: dir.to_path_buf(),
        dialects: vec![Dialect::Openai],
        sweep_duration_s: 1,
        min_conc: 1,
        max_conc: 2,
        measured_at: "2026-07-26T00:00:00Z".into(),
        arch: "arm64".into(),
        hardware: Some("test box".into()),
        engine_stamp: None,
        rig_mock: None,
        rig_release_url: None,
        load_cores: None,
    }
}

/// A metric that measures nothing and watches the RESULTS DIRECTORY instead.
///
/// The only way to prove a checkpoint is a checkpoint is to look at the disk while the grid is
/// still running. Interrupting a run mid-flight inside a test is not something this harness can
/// do; observing, from inside a cell, whether earlier cells have already reached disk is exactly
/// equivalent and is deterministic.
struct SnapshotWatcher {
    dir: std::path::PathBuf,
    seen: std::sync::Arc<std::sync::Mutex<Vec<bool>>>,
}

impl crate::metric::Metric for SnapshotWatcher {
    fn name(&self) -> &'static str {
        "snapshot_watcher"
    }
    fn fields(&self) -> &'static [&'static str] {
        &[]
    }
    fn measure(&self, _ctx: &crate::metric::CellCtx<'_>) -> crate::metric::Measured {
        let any = std::fs::read_dir(&self.dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .any(|e| e.file_name().to_string_lossy().ends_with(".json"))
            })
            .unwrap_or(false);
        self.seen.lock().expect("watcher lock").push(any);
        let f: crate::metric::Filled = Vec::new().into_iter().collect();
        f.into()
    }
}

// THE CONFIG IS THE EVIDENCE FOR THE CLAIM THE BOARD RESTS ON.
//
// `ResultSnapshot.config` existed, serialized, and was gen-data's FIRST choice of config source -
// and nothing ever filled it. Every gateway published `config: {files: {}}`, so every drawer read
// "Config: not published", on all 13 gateways of a finished run. Without it the claim that each
// gateway ran as it ships is something a reader has to take on trust.
#[test]
fn the_snapshot_carries_the_config_the_gateway_ran_from() {
    let dir = tmpdir("config-capture");
    let gw = serve(200);
    let mut cfg = cfg_for(&dir, gw);
    // A gateway directory with one rendered config, exactly as run.sh leaves it.
    let gw_dir = dir.join("gw-under-test");
    std::fs::create_dir_all(&gw_dir).expect("gw dir");
    std::fs::write(
        gw_dir.join("config.gen.yaml"),
        "listen: 8080\nupstream: mock\n",
    )
    .expect("config");
    cfg.gw_dir = gw_dir;
    cfg.manifest.config_files = vec![crate::manifest::ConfigFile {
        template: "config.gen.yaml.tmpl".into(),
        output: "config.gen.yaml".into(),
    }];

    let got = rendered_config(&cfg);
    assert_eq!(
        got.files.len(),
        1,
        "the rendered config must reach the artifact: {got:?}"
    );
    assert!(
        got.files["config.gen.yaml"].contains("listen: 8080"),
        "verbatim, not a re-render: {got:?}"
    );

    // A config that cannot be read must not take the run down with it: a finished measurement is
    // not discarded over its own provenance, it just publishes no config.
    cfg.manifest.config_files = vec![crate::manifest::ConfigFile {
        template: "gone.tmpl".into(),
        output: "not-written.yaml".into(),
    }];
    assert!(
        rendered_config(&cfg).files.is_empty(),
        "an unreadable config is absent, not fatal"
    );

    // AND THAT IT IS ACTUALLY CALLED. Everything above passes with the one line that wires this
    // into the snapshot deleted - which is exactly how the field came to be empty on a finished
    // board in the first place. The assertion that matters is on what reached the disk.
    cfg.manifest.config_files = vec![crate::manifest::ConfigFile {
        template: "config.gen.yaml.tmpl".into(),
        output: "config.gen.yaml".into(),
    }];
    let paths = run_suite_with(&cfg, gw, &[]).expect("the run should complete");
    let text = std::fs::read_to_string(&paths.current).expect("current file");
    let back: ResultSnapshot = serde_json::from_str(&text).expect("its own output must parse");
    assert_eq!(
        back.config.files.get("config.gen.yaml").map(String::as_str),
        Some("listen: 8080\nupstream: mock\n"),
        "the WRITTEN artifact must carry the config, not merely be able to compute it"
    );
}

// A PARTIAL RUN MUST LEAVE ITS MEASUREMENTS ON DISK.
//
// suite.rs has flushed a snapshot at every egress-column boundary for a long time, under a
// comment promising that "a run interrupted partway through must not lose every cell it already
// measured". The promise could not be kept: the loop iterated `run_grid_with`, which returns a
// `Vec`, so the ENTIRE grid had to finish before the first flush could run. Nothing enforced the
// difference, because every test ran the grid to completion, where both shapes look identical.
//
// busbar measured 16 of 36 cells across four hours on a box with a self-termination timer. The
// loop never started. Zero snapshots were written, and every one of those measurements died with
// the instance - the run log records only where the time went, never a single value.
//
// This watches the results directory from inside the cells themselves: with two dialects the
// grid is two egress columns, so by the time the second column is being measured the first
// column's checkpoint must already be on disk. Collecting the grid first makes every cell see an
// empty directory, which is precisely the bug.
#[test]
fn cells_reach_disk_while_the_grid_is_still_running() {
    let dir = tmpdir("partial-run-survives");
    let gw = serve(200);
    let mut cfg = cfg_for(&dir, gw);
    cfg.dialects = vec![Dialect::Openai, Dialect::Anthropic];

    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let watcher = SnapshotWatcher {
        dir: dir.clone(),
        seen: std::sync::Arc::clone(&seen),
    };
    let metrics: Vec<&dyn crate::metric::Metric> = vec![&watcher];
    run_suite_with(&cfg, gw, &metrics).expect("the run should complete");

    let seen = seen.lock().expect("watcher lock").clone();
    assert_eq!(seen.len(), 4, "two dialects is a 2x2 grid: {seen:?}");
    assert!(
        !seen[0],
        "nothing can be on disk before the first cell has been measured: {seen:?}"
    );
    assert!(
        seen.iter().any(|s| *s),
        "at least one cell must have found an earlier cell's snapshot already written - with \
             none, the whole grid was collected before anything reached disk and an interrupted run \
             loses everything it measured: {seen:?}"
    );
    assert!(
        seen[3],
        "by the LAST cell, the first egress column's checkpoint must certainly be on disk: {seen:?}"
    );
}

// A CHECKPOINT PROMOTE-GUARD TRIP MUST NOT ABORT THE WHOLE RUN. The incremental flush after
// an egress column is a thinner, unfinished view of the run, so tripping the guard against a
// fuller prior snapshot on disk is expected mid-run - it must not stop the remaining columns
// from being measured and written, only the FINAL flush's guard trip may be fatal.
#[test]
fn a_checkpoint_promote_guard_trip_does_not_abort_the_rest_of_the_grid() {
    let dir = tmpdir("checkpoint-guard");
    let gw = serve(200);
    let mut cfg = cfg_for(&dir, gw);
    cfg.dialects = vec![Dialect::Openai, Dialect::Anthropic];

    // Seed the directory with a prior snapshot serving 3 cells - more than the first egress
    // column's checkpoint (2 cells) will carry, but fewer than the finished run's 4.
    let mut seeded_upstreams = std::collections::HashMap::new();
    let mut cells = std::collections::HashMap::new();
    cells.insert(
        "openai".to_string(),
        crate::record::Cell {
            served: RecServed::Bool(true),
            ..Default::default()
        },
    );
    cells.insert(
        "anthropic".to_string(),
        crate::record::Cell {
            served: RecServed::Bool(true),
            ..Default::default()
        },
    );
    seeded_upstreams.insert(
        "openai".to_string(),
        Upstream {
            configurable: true,
            served: true,
            cells,
            ..Default::default()
        },
    );
    let mut cells2 = std::collections::HashMap::new();
    cells2.insert(
        "openai".to_string(),
        crate::record::Cell {
            served: RecServed::Bool(true),
            ..Default::default()
        },
    );
    seeded_upstreams.insert(
        "anthropic".to_string(),
        Upstream {
            configurable: true,
            served: true,
            cells: cells2,
            ..Default::default()
        },
    );
    let seed = ResultSnapshot {
        schema_version: 1,
        definitions: Default::default(),
        gateway: "gw".into(),
        measured_at: "2020-01-01T00-00-00Z".into(),
        matrix: Matrix {
            gateway: "gw".into(),
            served: true,
            measured_at: "2020-01-01T00-00-00Z".into(),
            upstreams: seeded_upstreams,
            ..Default::default()
        },
        ..Default::default()
    };
    crate::snapshot::write_snapshot(&dir, &seed).expect("seed snapshot should write");

    let result = run_suite_with(&cfg, gw, &[]);
    assert!(
        result.is_ok(),
        "a checkpoint guard trip must not abort the run: {result:?}"
    );
    let paths = result.expect("checked above");
    let text = std::fs::read_to_string(&paths.current).expect("current file");
    let back: ResultSnapshot = serde_json::from_str(&text).expect("its own output must parse");
    let served: usize = back
        .matrix
        .upstreams
        .values()
        .flat_map(|u| u.cells.values())
        .filter(|c| matches!(c.served, RecServed::Bool(true)))
        .count();
    assert_eq!(
        served, 4,
        "both egress columns must have been measured and published"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let mut d = std::env::temp_dir();
    d.push(format!("otb-suite-{}-{tag}", std::process::id()));
    // EMPTY, not merely present: a directory left by an earlier failed run under a recycled pid
    // carries its result_*.json snapshots, and tests here assert on what the directory holds.
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::create_dir_all(&d);
    d
}

// The whole suite, end to end: probe, sweep, judge, write. This is what had no caller.
#[test]
fn a_suite_run_writes_a_snapshot_that_parses_back() {
    let dir = tmpdir("ok");
    let gw = serve(200);
    let cfg = cfg_for(&dir, gw);
    let paths = run_suite_with(&cfg, gw, &[]).expect("the suite should write a snapshot");
    let text = std::fs::read_to_string(&paths.current).expect("current file");
    let back: ResultSnapshot = serde_json::from_str(&text).expect("its own output must parse");
    assert_eq!(back.gateway, "gw");
    assert!(back.matrix.served, "a 2xx gateway serves its diagonal");
    assert!(
        back.matrix
            .upstreams
            .get("openai")
            .is_some_and(|u| u.served),
        "a column that served a cell came up"
    );
    assert!(
        paths.historical.exists(),
        "the timestamped copy must land too"
    );
    // THE CASE THAT WAS BROKEN: site/gen-data.mjs reads matrix.measured_at, not the snapshot
    // root's - a snapshot whose matrix carries no stamp of its own renders as "never measured"
    // on the board no matter how fresh the run actually was.
    assert_eq!(
        back.matrix.measured_at, back.measured_at,
        "matrix.measured_at must mirror the snapshot root's, or the board reads this run as unmeasured"
    );
    assert!(!back.matrix.measured_at.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// A number that cannot be traced to the code that produced it is not evidence. The engine used
// to build the rig block solely from the box-qualification file, so the commit never reached the
// artifact at all: the first real EC2 run wrote a snapshot whose rig.engine was null even though
// the orchestrator had exported BENCH_ENGINE_COMMIT to the box.
//
// Note the box_qualify: None here. That is the half that was actually broken - the stamp has to
// survive on a run that carries no qualification, because the two are independent facts about
// the instrument and neither may suppress the other.
#[test]
fn the_commit_that_produced_a_run_reaches_the_artifact_without_a_box_qualification() {
    let dir = tmpdir("stamp");
    let gw = serve(200);
    let mut cfg = cfg_for(&dir, gw);
    cfg.engine_stamp = Some(crate::record::EngineStamp {
        commit: "deadbeef".into(),
        dirty: true,
    });
    let up = HashMap::new();
    let paths = flush(&cfg, &up, false, None).expect("the snapshot should write");
    let text = std::fs::read_to_string(&paths.current).expect("current file");
    let back: ResultSnapshot = serde_json::from_str(&text).expect("its own output must parse");
    let rig = back
        .rig
        .expect("a run with a commit must carry a rig block");
    let eng = rig
        .engine
        .expect("rig.engine must survive with no box qualification beside it");
    assert_eq!(eng.commit, "deadbeef");
    assert!(
        eng.dirty,
        "a dirty tree must be published as dirty, not quietly cleaned"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// A gateway that refuses still produces a complete artifact: the row exists, says not served,
// and carries no numbers. A dropped row would hide the failure.
#[test]
fn an_unserved_gateway_still_writes_a_complete_row_with_no_numbers() {
    let dir = tmpdir("unserved");
    let gw = serve(404);
    // A gateway refusal is distinct from an unhealthy reference machine.
    let mock = serve(200);
    let cfg = cfg_for(&dir, mock);
    let paths = run_suite_with(&cfg, gw, &[]).expect("a refusing gateway is still a result");
    let text = std::fs::read_to_string(&paths.current).expect("current file");
    let back: ResultSnapshot = serde_json::from_str(&text).expect("parse");
    let up = back
        .matrix
        .upstreams
        .get("openai")
        .expect("the egress row exists");
    let cell = up.cells.get("openai").expect("the cell row exists");
    assert!(!matches!(cell.served, RecServed::Bool(true)));
    assert!(cell.perf.is_none(), "an unserved cell carries no perf");
    // The column served nothing, so it must not claim it came up: `lib/promote_guard.py` reads
    // this flag as "the matrix served something", and a constant `true` here let a boot-failed
    // matrix pass as served.
    assert!(
        !up.served,
        "an egress column with no served cell did not come up"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── apply_sustained_verdict: the same rig-bound machinery as the peak, applied to the gate ──────

// ── judge_added_latency: a plain take, straight off the metric surface ─────────────────────────

#[test]
fn added_latency_fields_are_taken_straight_from_the_metric_surface() {
    let mut out = empty_perf();
    let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
        std::collections::BTreeMap::new();
    metrics.insert("added_latency_p50_us", Measurement::Measured(40_939.0));
    metrics.insert("added_latency_p99_us", Measurement::Measured(40_945.0));
    metrics.insert("gateway_c1_p99_us", Measurement::Measured(41_026.0));
    metrics.insert("direct_c1_p99_us", Measurement::Measured(81.0));
    judge_added_latency(&mut out, &metrics);
    assert_eq!(out.added_latency_p50_us.copied(), Some(40_939));
    assert_eq!(out.added_latency_p99_us.copied(), Some(40_945));
    assert_eq!(out.gateway_c1_p99_us.copied(), Some(41_026));
    assert_eq!(out.direct_c1_p99_us.copied(), Some(81));
}

// A field the group declined to fill must publish an absence with the group's own reason, never
// a silently-missing key nor a zero - the identical discipline `cell_stream`/`cell_memory` hold
// their "take" closures to.
#[test]
fn a_missing_added_latency_field_carries_the_groups_own_absence_reason() {
    let mut out = empty_perf();
    let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
        std::collections::BTreeMap::new();
    metrics.insert(
        "added_latency_p99_us",
        Measurement::absent_because(
            Absent::NotMeasured,
            "the gateway leg at c=1 was not clean: 0 ok, 4 fail",
        ),
    );
    judge_added_latency(&mut out, &metrics);
    assert_eq!(out.added_latency_p99_us.copied(), None);
    assert!(out
        .added_latency_p99_us
        .detail()
        .unwrap_or_default()
        .contains("not clean"));
    // gateway_c1_p99_us was never inserted into the map at all: still a key, still an absence.
    assert_eq!(out.gateway_c1_p99_us.copied(), None);
}

// NOT COVERED HERE BY DESIGN: an end-to-end suite test driving the whole pipeline with the gateway
// and mock pointed at the same server. `cfg_for`'s fixture (sweep_duration_s: 1, max_conc: 2) is too
// tight for `search::saturation_plateau` to complete even one probe before its own deadline
// interrupts it, so the search always returns zero sweep points and the verdict path is never
// reached at all. The behaviour that matters - a measurement AT the reference reaches the board,
// carrying the reference and the fraction of it - is covered directly and deterministically by
// `a_peak_at_the_rig_ceiling_is_published_with_its_headroom_not_withheld` and its sustained and
// stream counterparts, which drive the pure functions with a reference equal to the observation.
// Growing the fixture until a real search completes would trade a fast deterministic test for a
// slow one whose result depends on two live measurement passes against the same loopback server -
// real flakiness for no coverage the pure suites do not already give.

// ── the two stream verdicts: the same mock-bound machinery, one lane over ────────────────────

#[test]
fn a_measured_stream_ceiling_publishes_the_stream_count_it_held_at() {
    let mut out = crate::record::CellStream::default();
    // Comfortably below the mock's own frames/sec: the gateway's own ceiling, not the rig's.
    apply_streams_sustained_verdict(&mut out, 12_400.0, 256, Measurement::Measured(400_000.0));
    assert_eq!(out.streams_sustained_fps.copied(), Some(12_400.0));
    assert_eq!(
        out.streams_sustained.copied(),
        Some(256),
        "the published rate must carry the stream count it was carried at"
    );
    assert_eq!(out.streams_sustained_mock_ceiling, Some(400_000.0));
    assert!(out.streams_sustained_headroom.is_some());
}

#[test]
fn a_stream_rate_that_matches_the_paced_mock_is_published_with_its_headroom() {
    let mut out = crate::record::CellStream::default();
    // The field case: agentgateway carried 12275 frames/sec against a 12360 "ceiling" at c=256 -
    // within 0.7% of a mock whose rate is c x (1000 / 20ms) by construction - and published
    // nothing at all. Thirteen cells lost this metric that way in the 2026-07-28 run.
    apply_streams_sustained_verdict(&mut out, 12_275.0, 256, Measurement::Measured(12_360.0));
    assert_eq!(
        out.streams_sustained_fps.copied(),
        Some(12_275.0),
        "keeping pace is the success case"
    );
    assert_eq!(
        out.streams_sustained.copied(),
        Some(256),
        "and its operating point travels with it"
    );
    let h = out
        .streams_sustained_headroom
        .expect("the fraction of the paced target it carried");
    assert!(
        h > 0.99 && h < 1.0,
        "keeping pace to within 0.7% reads as ~0.993, not as an absence: {h}"
    );
}

#[test]
fn an_unusable_stream_reference_costs_the_headroom_and_not_the_rate() {
    let mut out = crate::record::CellStream::default();
    apply_streams_sustained_verdict(
        &mut out,
        12_400.0,
        256,
        Measurement::absent(Absent::NotMeasured),
    );
    assert_eq!(
        out.streams_sustained_fps.copied(),
        Some(12_400.0),
        "the frames really were carried; only the fraction is unavailable"
    );
    assert_eq!(out.streams_sustained.copied(), Some(256));
    assert_eq!(out.streams_sustained_mock_ceiling, None);
    assert_eq!(out.streams_sustained_headroom, None);
}

// A gate that nothing sustains is a real measured zero, not a mock-bound suppression: the mock
// was never asked to carry anything, so it cannot have been the thing that set the ceiling.
// Handled inside `judge_streams_sustained` (there is no concurrency to take a reference at), so
// this drives the whole function.
#[test]
fn nothing_sustaining_a_single_clean_stream_publishes_a_measured_zero() {
    let dir = tmpdir("streams-zero");
    let gw = serve(200);
    let cfg = cfg_for(&dir, gw);
    let mut out = crate::record::CellStream::default();
    let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
        std::collections::BTreeMap::new();
    metrics.insert("streams_sustained", Measurement::Measured(0.0));
    metrics.insert("streams_sustained_fps", Measurement::Measured(0.0));
    judge_streams_sustained(&cfg, Dialect::Openai, &mut out, &metrics);
    assert_eq!(out.streams_sustained.copied(), Some(0));
    assert_eq!(out.streams_sustained_fps.copied(), Some(0.0));
    assert_eq!(
        out.streams_sustained_headroom, None,
        "no concurrency means no reference to take, so there is no fraction"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// An absent stream measurement must carry the SEARCH's own reason and its prose bound through to
// the record, not arrive as a bare null. This is the "we discard the measurement" defect the peak
// lane already had, checked in the lane that was just added.
#[test]
fn an_absent_stream_ceiling_carries_the_searchs_own_reason_and_evidence() {
    let dir = tmpdir("streams-absent");
    let gw = serve(200);
    let cfg = cfg_for(&dir, gw);
    let mut out = crate::record::CellStream::default();
    let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
        std::collections::BTreeMap::new();
    metrics.insert(
        "streams_sustained_fps",
        Measurement::absent_because(
            Absent::SearchExhausted,
            "c=65536 still passes at the top of the search range",
        ),
    );
    metrics.insert(
        "streams_sustained",
        Measurement::absent(Absent::SearchExhausted),
    );
    judge_streams_sustained(&cfg, Dialect::Openai, &mut out, &metrics);
    assert_eq!(
        out.streams_sustained_fps.reason(),
        Some(&Absent::SearchExhausted)
    );
    assert!(out
        .streams_sustained_fps
        .detail()
        .unwrap_or_default()
        .contains("65536"));
    assert_eq!(
        out.streams_sustained.reason(),
        Some(&Absent::SearchExhausted)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── the two advisory notes ───────────────────────────────────────────────────────────────────

// A p99 over four thousand round trips and a p99 over eleven are the same field carrying utterly
// different weight, and nothing else in the artifact says which one a reader is holding.
#[test]
fn the_c1_note_says_how_many_round_trips_each_percentile_was_taken_over() {
    let mut out = empty_perf();
    let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
        std::collections::BTreeMap::new();
    metrics.insert("gateway_c1_samples", Measurement::Measured(4_812.0));
    metrics.insert("direct_c1_samples", Measurement::Measured(5_003.0));
    judge_added_latency(&mut out, &metrics);
    let note = out.c1_note.unwrap_or_default();
    assert!(
        note.contains("4812"),
        "the gateway leg's own count must appear: {note}"
    );
    assert!(
        note.contains("5003"),
        "the direct leg's own count must appear: {note}"
    );
}

// No counts means the group never completed a c=1 window, and the added-latency fields are
// already absent WITH the group's reason. A note restating that would publish one fact twice, in
// two wordings, which is what a `Measurement`'s reason exists to prevent.
#[test]
fn the_c1_note_is_absent_rather_than_prose_about_nothing() {
    let mut out = empty_perf();
    let metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
        std::collections::BTreeMap::new();
    judge_added_latency(&mut out, &metrics);
    assert_eq!(out.c1_note, None);
}

#[test]
fn the_stream_c1_note_states_the_weight_behind_each_published_percentile() {
    // THIS TEST USED TO ENCODE THE BUG. It asserted the note "must say why the p99 fields are
    // absent" - but `added_ttft_p99_us` is published on every healthy streaming cell, taken over
    // STREAM_TTFT_SAMPLES separate probes rather than over one stream. So the note asserted a
    // reason for an absence that was not absent, and the test held it there.
    let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
        std::collections::BTreeMap::new();
    metrics.insert("gateway_c1_frames", Measurement::Measured(64.0));
    metrics.insert("direct_c1_frames", Measurement::Measured(64.0));
    metrics.insert("ttft_gw_samples", Measurement::Measured(97.0));
    metrics.insert("ttft_direct_samples", Measurement::Measured(100.0));
    let note = stream_c1_note(&metrics).unwrap_or_default();
    assert!(note.contains("64 frame(s) through the gateway"), "{note}");
    assert!(
        !note.contains("99th percentile"),
        "the note must not claim a p99 cannot exist when one is published: {note}"
    );
    // The weight a reader needs: how many probes each added-TTFT percentile rests on.
    assert!(
        note.contains("97 through the gateway and 100 direct"),
        "the note must state each leg's own sample count, attributed to its leg: {note}"
    );

    // A dialect the mock cannot stream took no stream at all: no note, because the absent fields
    // already carry the reason.
    let empty: std::collections::BTreeMap<&'static str, Measurement<f64>> =
        std::collections::BTreeMap::new();
    assert_eq!(stream_c1_note(&empty), None);
}

// ── the egress re-verification verdict reaches the artifact ──────────────────────────────────

// The guard is only worth anything if a reader of the published JSON can see it fired. A cell
// that answered 200 without translating must not read as an unqualified capability claim: the
// flag, the evidence, and the cell's own verdict_note all have to say so.
#[test]
fn a_cell_that_was_not_re_verified_publishes_the_flag_and_the_evidence() {
    let dir = tmpdir("reverify");
    let gw = serve(200);
    let mut cfg = cfg_for(&dir, gw);
    // Two dialects, so the grid contains a translation cell rather than only the diagonal.
    cfg.dialects = vec![Dialect::Openai, Dialect::Anthropic];
    let paths = run_suite_with(&cfg, gw, &[]).expect("the suite writes a snapshot");
    let text = std::fs::read_to_string(&paths.current).expect("current file");
    let back: ResultSnapshot = serde_json::from_str(&text).expect("its own output must parse");

    // The fixture is a bare 200 server with no mock recorder behind it, so every cell is
    // UNCHECKED with a reason - never `false`, which would convict a gateway on our own rig.
    for (eg, up) in &back.matrix.upstreams {
        for (ing, cell) in &up.cells {
            let Some(perf) = cell.perf.as_ref() else {
                continue;
            };
            assert_ne!(
                perf.egress_reverified,
                Some(false),
                "a rig that cannot record must never publish a refutation: {ing}>{eg}"
            );
            assert!(
                perf.reverify_note.is_some(),
                "an unchecked cell must say why it was not checked: {ing}>{eg} {perf:?}"
            );
        }
    }
    // And the diagonal's reason is its own: there is no translation to prove there.
    let diagonal = back
        .matrix
        .upstreams
        .get("openai")
        .and_then(|u| u.cells.get("openai"))
        .and_then(|c| c.perf.as_ref())
        .expect("the openai diagonal is served and carries perf");
    assert!(
        diagonal
            .reverify_note
            .clone()
            .unwrap_or_default()
            .contains("same-dialect"),
        "{:?}",
        diagonal.reverify_note
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// A DECLINED CELL MUST SAY WHAT IT WAS TOLD: status, body_snippet and verdict_note must be
// populated, not left at their defaults, or a not_configured verdict carries no evidence once
// the box that produced it is gone. Otherwise a rig-side failure that makes every probe return
// 4xx is indistinguishable, in the published artifact, from a gateway that genuinely supports
// nothing, which is the single most damaging thing this board can get wrong.
#[test]
fn a_declined_cell_publishes_the_status_and_body_it_was_declined_with() {
    let dir = tmpdir("declined");
    // The gateway declines every probe while the MOCK is healthy. Both matter: a 404 from the
    // gateway with a dead mock is Untestable, because nothing about the gateway was learned, and
    // only a healthy rig turns a refusal into the gateway's own answer.
    let gw = serve(404);
    let mock = serve(200);
    let mut cfg = cfg_for(&dir, gw);
    cfg.mock_addr = mock;
    let paths = run_suite_with(&cfg, gw, &[]).expect("a declining gateway still writes a row");
    let text = std::fs::read_to_string(&paths.current).expect("current file");
    let back: ResultSnapshot = serde_json::from_str(&text).expect("its own output must parse");
    let cell = back
        .matrix
        .upstreams
        .values()
        .flat_map(|u| u.cells.values())
        .find(|c| matches!(&c.served, RecServed::Status(v) if v == "not_configured"))
        .expect("a healthy rig plus a refusing gateway is the gateway's own answer");
    assert_eq!(
        cell.status, "404",
        "the observed status must reach the artifact"
    );
    assert!(
        cell.verdict_note.contains("404"),
        "the note must name what was observed, got {:?}",
        cell.verdict_note
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// THE BOX HAS NO HISTORY, SO THE BASELINE HAS TO BE HANDED TO IT.
//
// `qualify_history` read the results directory, which is right where the engine runs beside the
// record and empty where it actually runs: every gateway gets a fresh EC2 instance carrying its
// manifest and the rig and nothing else. So the baseline was absent, the qualification seeded
// instead of judging, and every snapshot in results/ says outcome "seed", baseline_samples 0,
// drift null - on every box, for every gateway, since the check was written. A guard that always
// seeds cannot catch the box it exists to catch.
#[test]
fn a_handed_baseline_lets_the_box_qualification_actually_judge() {
    let empty = std::path::Path::new("/nonexistent-results-dir-for-this-test");
    // Driven through `qualify_history_with`, the env value as an argument, never by mutating the
    // environment: it is process-global, and every other suite test reads it.
    //
    // What the field had: nothing on disk, and nothing handed over.
    assert!(
        qualify_history_with(empty, None).is_empty(),
        "with no history the baseline is absent and the qualification can only seed"
    );
    let (outcome, drift) = crate::qualify::judge(
        Measurement::Measured(500_000.0),
        crate::qualify::rolling_baseline(qualify_history_with(empty, None)),
        QUALIFY_BAND_PCT,
        crate::qualify::Sense::HigherIsBetter,
    );
    assert_eq!(outcome.token(), "seed");
    assert_eq!(
        drift.value().copied(),
        None,
        "seeding has nothing to drift against"
    );

    // What the orchestrator can hand over, since it holds the record the box does not.
    let handed = Some("497862");
    assert_eq!(qualify_history_with(empty, handed), vec![497_862.0]);
    // A mangled hand-over is ignored (and said so on stderr), never read as a rate.
    for bad in ["", "null", "497862rps", "0", "-5", "NaN", "inf"] {
        assert!(
            qualify_history_with(empty, Some(bad)).is_empty(),
            "{bad:?} is not a baseline"
        );
    }

    // A healthy box - the widest real deviation across the 2026-07-28 field was 4.4% - passes.
    let judge_at = |rps: f64| {
        crate::qualify::judge(
            Measurement::Measured(rps),
            crate::qualify::rolling_baseline(qualify_history_with(empty, handed)),
            QUALIFY_BAND_PCT,
            crate::qualify::Sense::HigherIsBetter,
        )
        .0
        .token()
    };
    assert_eq!(
        judge_at(475_906.0),
        "pass",
        "the slowest real box of the field must still pass"
    );
    assert_eq!(judge_at(509_142.0), "pass", "and so must the fastest");
    // The box this guard exists for: far enough under that its gateway's whole column would
    // have been measured on a rig nothing compared against anything.
    assert_eq!(judge_at(300_000.0), "fail");
}

// WHAT IT RAN ON MUST REACH THE ARTIFACT.
//
// `run-on-ec2.sh` has exported BENCH_HARDWARE since the field moved to dedicated boxes, and
// `record.rs` has always had the field, but nothing joined them: every snapshot of the
// 2026-07-28 run published "hardware": null. A board whose entire claim is that only the gateway
// differs between columns cannot state the hardware it differed on.
#[test]
fn the_snapshot_carries_the_hardware_it_was_handed() {
    let dir = tmpdir("hardware-provenance");
    let gw = serve(200);
    let mut cfg = cfg_for(&dir, gw);
    let label = "AWS m7g.4xlarge (16 cores / 64 GB), gateway pinned to 4";
    cfg.hardware = Some(label.into());
    let paths = run_suite_with(&cfg, gw, &[]).expect("the suite writes a snapshot");
    let snap: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.current).expect("read"))
            .expect("parse");
    assert_eq!(
        snap.get("hardware").and_then(serde_json::Value::as_str),
        Some(label),
        "the box shape the orchestrator handed in must be published, not dropped"
    );

    // Never invented: a run that was not told stays null rather than guessing an instance type.
    let dir2 = tmpdir("hardware-absent");
    let mut cfg2 = cfg_for(&dir2, gw);
    cfg2.hardware = None;
    let paths2 = run_suite_with(&cfg2, gw, &[]).expect("the suite writes a snapshot");
    let snap2: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&paths2.current).expect("read"))
            .expect("parse");
    assert!(
        snap2
            .get("hardware")
            .is_some_and(serde_json::Value::is_null),
        "absent must publish as a literal null"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&dir2);
}

// THE BLOCK THAT HOLDS THE NUMBERS MUST NAME WHAT PRODUCED THEM. The snapshot root's
// hardware/arch/build were filled and `matrix`'s were left at `Default` - null, null, empty - so
// the "hardware: null" episode the root's own comment records repeated one level down, inside the
// block that a reader exports, diffs and archives on its own.
#[test]
fn the_matrix_block_names_the_box_and_build_that_produced_its_numbers() {
    let dir = tmpdir("matrix-provenance");
    let gw = serve(200);
    let mut cfg = cfg_for(&dir, gw);
    cfg.hardware = Some("AWS m7g.4xlarge (16 cores / 64 GB)".into());
    let paths = run_suite_with(&cfg, gw, &[]).expect("the suite writes a snapshot");
    let back: ResultSnapshot =
        serde_json::from_str(&std::fs::read_to_string(&paths.current).expect("read"))
            .expect("its own output must parse");

    assert_eq!(
        back.matrix.hardware, back.hardware,
        "the matrix must name the same box the root does"
    );
    assert_eq!(
        back.matrix.hardware.as_deref(),
        Some("AWS m7g.4xlarge (16 cores / 64 GB)")
    );
    assert_eq!(back.matrix.arch, back.arch);
    assert_eq!(
        back.matrix.build, back.build,
        "and the same build: two levels of one artifact must not disagree about what was measured"
    );
    assert!(!back.matrix.build.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

// A FAILED QUALIFICATION MUST NEVER SEED THE BASELINE IT FAILED AGAINST.
//
// `Outcome::qualifies_as_baseline` stated the rule and nothing called it: the harvest took every
// record on disk, so a contaminated box's own low reading joined the median that decides whether
// the next run fails - the gate slowly moving toward whatever the sick box was doing until
// nothing failed it at all.
#[test]
fn only_a_qualifying_run_contributes_to_the_rolling_baseline() {
    let dir = tmpdir("qualify-history");
    let write = |name: &str, outcome: &str, rps: f64| {
        std::fs::write(
            dir.join(name),
            format!(
                r#"{{"rig":{{"box_qualify":{{"outcome":"{outcome}","observed_rps":{rps}}}}}}}"#
            ),
        )
        .expect("fixture write");
    };
    write("result_gw_pass.json", "pass", 500_000.0);
    write("result_gw_seed.json", "seed", 480_000.0);
    write("result_gw_fail.json", "fail", 300_000.0);
    write("result_gw_skip.json", "skip", 10.0);
    // A record whose qualification predates the outcome field, or whose token this build does
    // not know: not known to qualify, so it does not.
    std::fs::write(
        dir.join("result_gw_untagged.json"),
        r#"{"rig":{"box_qualify":{"observed_rps":1.0}}}"#,
    )
    .expect("fixture write");

    let mut history = qualify_history_on_disk(&dir);
    history.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    assert_eq!(
        history,
        vec![480_000.0, 500_000.0],
        "only pass and seed may seed the baseline, got {history:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// A CELL WHOSE STREAM DEMONSTRABLY FLOWED MUST NOT READ AS ONE THAT DID NOT. `stream_served` was
// derived from `added_ttft_p50_us` alone, so a cell whose gap figures measured cleanly - which
// can only happen if frames arrived through the gateway and were timed - published the TTFT
// leg's absence token as its status while carrying measured streaming numbers two fields down.
#[test]
fn gap_figures_that_measured_make_a_served_stream_even_when_the_ttft_legs_failed() {
    let dir = tmpdir("stream-status");
    let gw = serve(200);
    let cfg = cfg_for(&dir, gw);
    let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
        std::collections::BTreeMap::new();
    metrics.insert(
        "added_ttft_p50_us",
        Measurement::absent_because(
            Absent::RigLimited,
            "the direct leg's first frame was not timeable",
        ),
    );
    metrics.insert("added_gap_p50_us", Measurement::Measured(1_400.0));
    metrics.insert("added_gap_p99_us", Measurement::Measured(9_100.0));

    let out = cell_stream(&cfg, Dialect::Openai, &metrics, None);
    assert!(
        matches!(out.stream_served, crate::record::StreamServed::Bool(true)),
        "frames flowed and were timed: this is a served stream, got {:?}",
        out.stream_served
    );
    assert_eq!(
        out.reason.as_deref(),
        Some("rig_limited"),
        "the partial must still be stated, as the TOKEN"
    );
    assert_eq!(
        out.stream_error.as_deref(),
        Some("the direct leg's first frame was not timeable"),
        "and the prose belongs in the prose field"
    );
    assert_eq!(out.added_gap_p50_us.copied(), Some(1_400));
    let _ = std::fs::remove_dir_all(&dir);
}

// A stream that produced nothing at all still publishes the reason as the STATUS, and now also as
// a token in `reason` - which used to hold the free-text detail, and was null whenever the
// absence had none, so the one field a consumer would branch on was prose or nothing.
#[test]
fn a_stream_that_produced_nothing_publishes_its_reason_as_a_token() {
    let dir = tmpdir("stream-status-none");
    let gw = serve(200);
    let cfg = cfg_for(&dir, gw);
    let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
        std::collections::BTreeMap::new();
    metrics.insert("added_ttft_p50_us", Measurement::absent(Absent::Untestable));
    metrics.insert("added_gap_p50_us", Measurement::absent(Absent::Untestable));

    let out = cell_stream(&cfg, Dialect::Openai, &metrics, None);
    assert!(
        matches!(&out.stream_served, crate::record::StreamServed::Status(s) if s == "untestable"),
        "got {:?}",
        out.stream_served
    );
    assert_eq!(
        out.reason.as_deref(),
        Some("untestable"),
        "a status with no detail still has a reason, and must publish it"
    );
    assert_eq!(out.stream_error, None, "there was no prose to invent");
    let _ = std::fs::remove_dir_all(&dir);
}

// STEADY STATE IS WHAT IT COSTS UNDER LOAD, NOT WHAT IT COSTS AFTER.
//
// The sampler runs from before the load through the whole recovery window, so the series is load
// THEN recovery. Taking the trailing half of the WHOLE series therefore reports where memory
// settled after the load stopped, which is `recovered_rss_mib`'s question. The fixture is the
// shape that makes the two answers differ: a gateway that holds 400 MiB under load and releases
// to 50 MiB, with a recovery window longer than the load window - exactly the profile the blend
// flatters. Measured on the 2026-07-29 field run at 1.3 MiB for one-api, which barely releases;
// this fixture is what a gateway that does release looks like.
#[test]
fn steady_state_reads_the_load_window_not_the_recovery_that_follows_it() {
    let sample = |t: i64, mib: f64| crate::record::RssSample {
        t_s: t,
        rss_mib: Measurement::Measured(mib),
    };
    let mut series = Vec::new();
    for t in 0..10 {
        series.push(sample(t, if t < 2 { 100.0 } else { 400.0 })); // ramp, then flat under load
    }
    for t in 10..40 {
        series.push(sample(t, 50.0)); // recovery: released, and three times as many samples
    }

    let under_load = steady_state(&series, Some(9.0));
    assert_eq!(
        under_load.copied(),
        Some(400.0),
        "the steady state is what the process held while the load ran"
    );

    // The defect, stated as the assertion that would have passed before: with no bound the
    // trailing half is entirely recovery samples and the figure collapses onto the recovered
    // level, making two deliberately distinct fields report the same number.
    let blended = steady_state(&series, None);
    assert_eq!(blended.copied(), Some(50.0));
    assert_ne!(
        blended.copied(),
        under_load.copied(),
        "if these ever agree the fixture has stopped exercising the defect"
    );
}

// EVERY NULL ON A SERVED CELL CARRIES A REASON, including the two that used to travel as plain
// `Option`s. `.copied().map(...)` threw the memory group's own reason away on the way into the
// record, and the record field it landed in was invisible to the absences map, so a window that
// could not judge the plateau published a bare null and nothing anywhere said why.
#[test]
fn a_memory_window_carries_the_groups_reason_for_an_unjudged_plateau() {
    let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
        std::collections::BTreeMap::new();
    metrics.insert(
        "memory_plateaued",
        Measurement::absent_because(
            Absent::NotMeasured,
            "too few readings fell inside the settle window to judge whether memory moved",
        ),
    );
    metrics.insert("memory_load_s", Measurement::Measured(90.0));

    let mem = cell_memory(&metrics, None);
    assert_eq!(mem.plateaued.copied(), None);
    assert_eq!(mem.plateaued.reason(), Some(&Absent::NotMeasured));
    assert!(
        mem.plateaued
            .detail()
            .unwrap_or_default()
            .contains("settle window"),
        "the group's own evidence must survive the trip into the record"
    );
    assert_eq!(mem.load_s.copied(), Some(90));
    assert!(
        mem.absences().contains_key("plateaued"),
        "and the absence must be reachable from the published absences map"
    );
}

#[test]
fn a_failed_box_stops_the_grid_and_carries_its_actual_verdict() {
    let verdict = serde_json::json!({"outcome":"fail", "observed_rps":1.0, "baseline_rps":100.0});
    let err = require_qualified_box(&verdict).unwrap_err();
    assert!(err.to_string().contains("BOX_QUALIFICATION_FAILED"));
    match err {
        SnapshotError::BoxQualificationFailed { verdict: actual } => assert_eq!(actual, verdict),
        other => panic!("unexpected refusal: {other}"),
    }
    for outcome in ["pass", "seed", "skip"] {
        assert!(require_qualified_box(&serde_json::json!({"outcome":outcome})).is_ok());
    }
}

#[test]
fn built_gateway_identity_is_read_from_the_successful_build_receipt() {
    let dir = std::env::temp_dir().join(format!("otb-build-proof-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(".build-provenance.json");
    let proof = serde_json::json!({"source_commit":"a".repeat(40), "source_dirty":false, "binary_sha256":"b".repeat(64)});
    std::fs::write(&path, serde_json::to_vec(&proof).unwrap()).unwrap();
    assert_eq!(built_gateway_provenance(&dir).unwrap(), Some(proof.clone()));
    let mut cfg = cfg_for(&dir.join("results"), "127.0.0.1:1".parse().unwrap());
    cfg.gw_dir = dir.clone();
    std::fs::create_dir_all(&cfg.results_dir).unwrap();
    let paths = flush(&cfg, &HashMap::new(), false, None).unwrap();
    let snapshot: serde_json::Value =
        serde_json::from_slice(&std::fs::read(paths.historical).unwrap()).unwrap();
    assert_eq!(snapshot["gateway_provenance"], proof);
    std::fs::write(&path, b"broken json").unwrap();
    assert!(built_gateway_provenance(&dir).is_err());
    std::fs::remove_file(path).unwrap();
    assert_eq!(built_gateway_provenance(&dir).unwrap(), None);
    std::fs::remove_dir_all(dir).unwrap();
}
