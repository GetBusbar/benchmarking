// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The suite: one gateway, start to finish, in one place.
//
// This is what the deleted shell driver did. It owns SEQUENCING only. Every decision belongs to the
// module that already owns it, so nothing here judges anything: the searches find the peaks, the
// rig-bound check decides what is publishable, the record types decide the shape, and the snapshot
// writer decides what may overwrite what.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use crate::cell::Served;
use crate::ingress::Dialect;
use crate::manifest::Manifest;
use crate::measurement::{Absent, Measurement};
use crate::record::{Cell, CellPerf, Matrix, ResultSnapshot, Served as RecServed, Upstream};
use crate::rigbound;
use crate::run::{self, RunConfig};
use crate::snapshot::{self, Paths, SnapshotError};

pub struct SuiteConfig {
    pub manifest: Manifest,
    pub mock_addr: SocketAddr,
    pub results_dir: std::path::PathBuf,
    pub dialects: Vec<Dialect>,
    pub sweep_duration_s: u64,
    pub min_conc: u32,
    pub max_conc: u32,
    /// Stamped into the artifact so a number can always be traced to the engine that produced it.
    pub measured_at: String,
    pub arch: String,
    /// WHAT THIS RAN ON, in the orchestrator's own words: instance type, core count, memory, and how
    /// the cores were split between gateway, mock and load generator.
    ///
    /// Handed in exactly like `arch`, and for the same reason: the box cannot describe the shape it
    /// was launched with. `run-on-ec2.sh` has exported it as BENCH_HARDWARE all along and
    /// `record.rs` has always had the field, but nothing connected the two - so every published
    /// snapshot carried `"hardware": null` while the harness knew the answer. A board whose whole
    /// claim is that only the gateway differs between columns should be able to say what the
    /// hardware was without asking someone to remember.
    ///
    /// `None` when it was never supplied, published as a literal null rather than a guess.
    pub hardware: Option<String>,
    /// WHICH COMMIT PRODUCED THIS RUN. Resolved orchestrator-side (the box's clone is checked out at
    /// a detached commit and the engine binary is a download, so neither can work it out on the box)
    /// and handed in like `arch` is, rather than read from the environment down here: the snapshot
    /// writer stays a pure function of its config, which is what lets the tests assert on it.
    ///
    /// `None` when the harness could not identify itself, and that is published as a literal null.
    /// The alternative - omitting the key - would make "this run is not reproducible" look exactly
    /// like "this artifact predates provenance", and the whole point of the stamp is telling those
    /// two apart when two runs disagree.
    pub engine_stamp: Option<crate::record::EngineStamp>,
    /// WHICH INSTRUMENT TOOK THE READINGS. The mock and the load generator are half the measuring
    /// apparatus, and `rig` is a MOVING release tag: two runs weeks apart can use different binaries
    /// behind the same URL, so a verdict change between them could be the instrument moving rather
    /// than the gateway, unless the run itself records which mock produced it.
    ///
    /// Resolved orchestrator-side and handed in, exactly like `engine_stamp`: the box's own rig.sh
    /// is what fetched this binary and hashed it, and the engine cannot re-derive that.
    ///
    /// The MOCK only: the load generator is `otb loadgen`, a subcommand of this engine, so
    /// `rig.engine.commit` already identifies it and a second record here would be the same fact
    /// twice.
    pub rig_mock: Option<crate::record::BinaryProvenance>,
    /// The release the rig came from, recorded beside the digests it produced.
    pub rig_release_url: Option<String>,
    /// The gateway's own directory, for resolving its config templates and mounts.
    pub gw_dir: std::path::PathBuf,
    /// The CPU list the GATEWAY is pinned to. Distinct from the generator's: the split is the
    /// comparability basis.
    pub gw_cores: String,
    /// The CPU list the load generator is pinned to. The gateway, the generator and the mock each
    /// get their own cores, and that split is what makes two gateways comparable at all.
    pub load_cores: Option<String>,
}

/// A cell's throughput, already judged against the rig.
struct Judged {
    perf: CellPerf,
}

fn empty_perf() -> CellPerf {
    CellPerf {
        added_latency_p50_us: Measurement::absent(Absent::NotMeasured),
        added_latency_p99_us: Measurement::absent(Absent::NotMeasured),
        gateway_c1_p99_us: Measurement::absent(Absent::NotMeasured),
        direct_c1_p99_us: Measurement::absent(Absent::NotMeasured),
        rps_sustained_20ms: Measurement::absent(Absent::NotMeasured),
        rps_sustained_20ms_concurrency: Measurement::absent(Absent::NotMeasured),
        conc_at_sustained: Measurement::absent(Absent::NotMeasured),
        rps_sustained_20ms_mock_bound: None,
        rps_max_proxy: Measurement::absent(Absent::NotMeasured),
        rps_max_proxy_concurrency: Measurement::absent(Absent::NotMeasured),
        conc_at_peak: Measurement::absent(Absent::NotMeasured),
        ..Default::default()
    }
}

/// Measure the RIG's own ceiling on the same cell, so a gateway's peak can be judged against a
/// reference taken at the same operating point rather than at the top of the grid.
fn rig_ceiling(cfg: &SuiteConfig, dialect: Dialect, at_conc: u32) -> Measurement<f64> {
    let direct = RunConfig {
        gateway_addr: cfg.mock_addr,
        mock_addr: cfg.mock_addr,
        model: cfg.manifest.model.clone(),
        // EMPTY ON PURPOSE: this reference drives the MOCK directly on one dialect, so there is no
        // egress column to select and no gateway to route. A per-egress model here would ask the
        // mock for a name only the gateway's own router understands.
        egress_models: Default::default(),
        auth: cfg.manifest.auth.clone(),
        dialects: vec![dialect],
        sweep_duration_s: cfg.sweep_duration_s,
        probe_timeout: Duration::from_secs(10),
        load_cores: cfg.load_cores.clone(),
        // The reference drives the MOCK directly: there is no gateway process behind it, so the
        // identity here must not be the gateway's. Naming the gateway would let a memory reader
        // attribute the gateway's tree to a run that never touched it.
        static_headers: Vec::new(),
        egress_headers: Default::default(),
        runtime: crate::manifest::Runtime::Native {
            proc_match: String::new(),
        },
        // The reference drives the MOCK directly. There is no gateway process behind it, so there is
        // nothing to restart, and a spec here would let a reference measurement bounce the gateway.
        relaunch: None,
        relaunch_commands: Vec::new(),
        relaunch_launcher: Default::default(),
        // The reference drives the MOCK, which serves every dialect at its standard path. A
        // gateway's prefix must not follow it here or the reference would probe a path the mock
        // does not have and the ceiling would read as unmeasurable.
        declared_path: String::new(),
        // The reference drives the MOCK at its standard paths; a gateway's override must not follow.
        cell_paths: Default::default(),
        // The reference is a single diagonal cell against the mock, never gated by any gateway's
        // declared capability - undeclared means always probed, which is what this needs.
        matrix: Vec::new(),
        matrix_note: String::new(),
        untestable_cells: Vec::new(),
        untestable_note: String::new(),
    };
    let id = crate::cell::CellId::new(dialect.as_str(), dialect.as_str());
    // A single point AT THE WINNER's concurrency, not a search: the reference must be taken where
    // the gateway's number was taken, or the comparison is between two different operating points.
    // `measure_at`, not a one-wide peak search - a point makes no turnover claim, and a search over a
    // range of one cannot honestly answer "what is the maximum".
    run::measure_at(&direct, &id, at_conc)
}

/// Narrow a metric-surface `f64` into the artifact's published `i64`, carrying the reason and detail
/// intact when absent. Every group's numbers here are microseconds or whole counts; the metric
/// surface is f64 only because ALL groups share one type (`metric.rs`'s module doc explains why), so
/// this is the one narrowing point shared by every place a metric field becomes a record field.
fn as_i64(m: Option<&Measurement<f64>>) -> Measurement<i64> {
    match m {
        Some(m) => carry(m, |v| v as i64),
        None => {
            Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field")
        }
    }
}

/// Narrow a metric-surface reading into whatever type the record publishes it as, keeping the reason
/// AND the detail when it is absent. What matters is that the absent branch goes through
/// `carry_absence` rather than through `.copied().map(...)`, which silently converts "absent, and
/// here is why" into "absent".
fn carry<T>(m: &Measurement<f64>, f: impl Fn(f64) -> T) -> Measurement<T> {
    match m.value() {
        Some(&v) => Measurement::Measured(f(v)),
        None => carry_absence(m),
    }
}

/// Judge one cell's throughput and suppress it if the rig, not the gateway, set it.
/// Turn the metrics the engine took on one cell into the published perf block.
///
/// Reads the map by the SAME field names `metric::Metric::fields()` declares, so a group that stops
/// filling a field surfaces here as an absence with the group's own reason rather than as a silently
/// missing number. `metric::process_cell` guarantees every declared field is present, so a lookup
/// that misses means the field was never declared by any group at all.
fn judge_cell(
    cfg: &SuiteConfig,
    dialect: Dialect,
    metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>,
) -> Judged {
    let mut out = empty_perf();
    let missing =
        || Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field");
    let rps = metrics
        .get("rps_max_proxy")
        .cloned()
        .unwrap_or_else(missing);
    let conc_m = metrics.get("conc_at_peak").cloned().unwrap_or_else(missing);

    match (rps.value(), conc_m.value()) {
        (Some(&value), Some(&conc_f)) => {
            // Concurrency travels as f64 so every metric has one type; it is only ever a whole rung
            // of the search, so this narrowing cannot lose anything a search could have produced.
            let conc = conc_f as u32;
            let reference = rig_ceiling(cfg, dialect, conc);
            apply_peak_verdict(&mut out, value, conc, reference);
        }
        _ => {
            // Carry the search's own reason and evidence rather than flattening it.
            let absent: Measurement<i64> = carry_absence(&rps);
            // The concurrency travels WITH the peak, present or absent, and carries the same detail:
            // leaving it at empty_perf()'s default published a different reason for the two halves of
            // one fact, and re-deriving it from the reason alone published a thinner one.
            out.rps_max_proxy_concurrency = carry_absence(&absent);
            // conc_at_peak is the third face of the same fact and moves with it.
            out.conc_at_peak = carry_absence(&absent);
            out.rps_max_proxy = absent;
        }
    }

    judge_added_latency(&mut out, metrics);
    judge_sustained(cfg, dialect, &mut out, metrics);

    Judged { perf: out }
}

/// Measure the RIG's own STREAM ceiling on the same cell: the mock's frames/sec at the concurrency
/// the gateway's own stream number was taken at.
///
/// The streaming analogue of `rig_ceiling`, and it takes its reference the same way and for the same
/// reason (`rigbound.rs`'s header): the rig is not equally fast at every concurrency, so a reference
/// taken at the top of the range would systematically understate how close the gateway came to it.
///
/// It does NOT build a mock-facing `RunConfig` the way `rig_ceiling` has to. That dance exists so the
/// request generator's search plumbing can be pointed at the mock; the stream window takes its
/// address, path and headers as arguments, so the direct leg is expressed directly instead of as a
/// gateway config with the gateway parts blanked out one field at a time.
fn stream_rig_ceiling(cfg: &SuiteConfig, dialect: Dialect, at_conc: u32) -> Measurement<f64> {
    run::stream_fps_at(
        cfg.mock_addr,
        &cfg.manifest.model,
        &cfg.manifest.auth,
        dialect,
        at_conc,
    )
}

/// Fill the added-latency fields straight from the metric surface.
///
/// Unlike the peak's `rps_max_proxy`, there is no separate rig-bound verdict to compute here: the
/// group's own two-leg comparison (gateway leg minus a direct-to-mock leg, both taken at c=1) already
/// IS the rig correction, the same way `Streaming::measure`'s `added_ttft`/`added_gap` need no second
/// rig judgement layered on top. So this is a plain "take", exactly the pattern `cell_memory` and
/// `cell_stream` already use for fields with no rig-bound question to ask.
fn judge_added_latency(
    out: &mut CellPerf,
    metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>,
) {
    out.added_latency_p50_us = as_i64(metrics.get("added_latency_p50_us"));
    out.added_latency_p99_us = as_i64(metrics.get("added_latency_p99_us"));
    out.gateway_c1_p99_us = as_i64(metrics.get("gateway_c1_p99_us"));
    out.direct_c1_p99_us = as_i64(metrics.get("direct_c1_p99_us"));
    out.c1_note = c1_note(metrics);
}

/// HOW MUCH IS BEHIND THE c=1 PERCENTILES.
///
/// `c1_note` was declared and never set. The one thing worth saying there, and the one thing nothing
/// else in the artifact says, is the SAMPLE COUNT behind each leg: a p99 over four thousand round
/// trips and a p99 over eleven are the same published field carrying completely different weight, and
/// a reader deciding whether to trust an added-latency figure has no other way to tell them apart.
///
/// `None`, not a note about zeroes, when either leg produced no count: the added-latency fields are
/// then already absent WITH the group's own reason for it, and a second sentence restating that would
/// be the same fact published twice in two wordings, which is precisely what `Measurement`'s reason
/// exists to prevent.
fn c1_note(metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>) -> Option<String> {
    let gw = metrics.get("gateway_c1_samples")?.copied()?;
    let direct = metrics.get("direct_c1_samples")?.copied()?;
    Some(format!(
        "the c=1 percentiles are taken over {gw:.0} successful gateway round trip(s) and {direct:.0} \
         direct-to-mock round trip(s), each leg a single clean window with no failures"
    ))
}

/// Judge the sustained-throughput ceiling exactly as `judge_cell` judges the peak: the SAME rig
/// reference (`rig_ceiling`, the mock's own throughput at the winning concurrency) and the SAME
/// fraction (`rigbound::is_rig_bound`), so the two "was this the rig or the gateway" verdicts in one
/// cell are computed one way rather than one gate reusing the peak's machinery and the other
/// inventing its own threshold.
fn judge_sustained(
    cfg: &SuiteConfig,
    dialect: Dialect,
    out: &mut CellPerf,
    metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>,
) {
    let missing =
        || Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field");
    let rps = metrics
        .get("rps_sustained_20ms")
        .cloned()
        .unwrap_or_else(missing);
    let conc_m = metrics
        .get("rps_sustained_20ms_concurrency")
        .cloned()
        .unwrap_or_else(missing);

    let (Some(&value), Some(&conc_f)) = (rps.value(), conc_m.value()) else {
        let absent: Measurement<i64> = carry_absence(&rps);
        out.rps_sustained_20ms_concurrency = carry_absence(&absent);
        out.conc_at_sustained = carry_absence(&absent);
        out.rps_sustained_20ms = absent;
        return;
    };
    let conc = conc_f as u32;

    // c == 0 is `bisect_ceiling`'s own MEASURED "nothing sustains this gate" answer - there is no
    // concurrency to take a rig reference AT, and a gateway that cannot sustain the gate even at the
    // floor cannot be rig-bound by construction (the rig was never asked to do anything), so this
    // publishes directly rather than through the rig-bound judgement below.
    if conc == 0 {
        out.rps_sustained_20ms = Measurement::Measured(0);
        out.rps_sustained_20ms_concurrency = Measurement::Measured(0);
        out.conc_at_sustained = Measurement::Measured(0);
        out.rps_sustained_20ms_mock_bound = Some(false);
        return;
    }

    let reference = rig_ceiling(cfg, dialect, conc);
    apply_sustained_verdict(out, value, conc, reference);
}

/// Fill the sustained fields from the rig verdict. PURE, and separate from `judge_sustained`, for the
/// identical reason `apply_peak_verdict` is separate from `judge_cell`: a live rig measurement cannot
/// be driven to the rig-bound and gateway-bound branches on demand from a fixture where the gateway
/// and the mock are the same server, so the verdict logic has to be testable independent of
/// `rig_ceiling` actually running one.
fn apply_sustained_verdict(out: &mut CellPerf, value: f64, conc: u32, reference: Measurement<f64>) {
    let verdict = rigbound::is_rig_bound(value, reference.clone());
    match verdict.copied() {
        Some(true) => {
            let detail = match reference.copied() {
                Some(r) => {
                    format!("sustained {value:.0} against a rig ceiling of {r:.0} at c={conc}")
                }
                None => format!("sustained {value:.0} at c={conc} with an unusable rig reference"),
            };
            out.rps_sustained_20ms = Measurement::absent_because(Absent::RigLimited, detail);
            out.rps_sustained_20ms_mock_bound = Some(true);
            // Suppressed WITH their number, and carrying its evidence: a concurrency left behind
            // would be the operating point of a number the board is refusing to publish, and one
            // left carrying the bare reason would name the suppression without the ceiling that
            // caused it.
            out.rps_sustained_20ms_concurrency = carry_absence(&out.rps_sustained_20ms);
            out.conc_at_sustained = carry_absence(&out.rps_sustained_20ms);
        }
        Some(false) => {
            out.rps_sustained_20ms = Measurement::Measured(value as i64);
            out.rps_sustained_20ms_mock_bound = Some(false);
            out.rps_sustained_20ms_concurrency = Measurement::Measured(i64::from(conc));
            out.conc_at_sustained = Measurement::Measured(i64::from(conc));
        }
        None => {
            let why = verdict.detail().map(str::to_string).unwrap_or_else(||
                "the rig reference could not be measured, so it is unknown whether the gateway or the rig set this".to_string());
            let reason = verdict.reason().cloned().unwrap_or(Absent::RigLimited);
            out.rps_sustained_20ms = Measurement::absent_because(
                reason,
                format!("sustained {value:.0} at c={conc}: {why}"),
            );
            out.rps_sustained_20ms_mock_bound = None;
            out.rps_sustained_20ms_concurrency = carry_absence(&out.rps_sustained_20ms);
            // conc_at_sustained was assigned twice here, and the second assignment overwrote the
            // verdict's own reason with a flat `RigLimited` - so a cell whose rig reference was
            // unmeasurable published "the rig bounded this" beside a peak that says no such thing.
            out.conc_at_sustained = carry_absence(&out.rps_sustained_20ms);
        }
    }
}

/// Fill the peak fields from the rig verdict. PURE, and separate from `judge_cell`, for one reason:
/// the peak and the concurrency it happened at must agree about whether they exist, and that could
/// not be tested while the decision was welded to a live rig measurement. The fixture the suite
/// tests use points the gateway and the mock at the SAME server, so every cell comes back rig-bound
/// and the measured branch was unreachable - a test written against `judge_cell` passed identically
/// with the fix reverted, which is a test that is not testing anything.
fn apply_peak_verdict(out: &mut CellPerf, value: f64, conc: u32, reference: Measurement<f64>) {
    let verdict = rigbound::is_rig_bound(value, reference.clone());
    match verdict.copied() {
        // Bounded by our own rig: this says nothing about the gateway, so it must not rank, win a
        // comparison, or draw a bar. Two fast gateways both pinned here would otherwise read as a
        // tie they did not earn.
        Some(true) => {
            let detail = match reference.copied() {
                Some(r) => {
                    format!("reached {value:.0} against a rig ceiling of {r:.0} at c={conc}")
                }
                None => format!("reached {value:.0} at c={conc} with an unusable rig reference"),
            };
            out.rps_max_proxy = Measurement::absent_because(Absent::RigLimited, detail);
            out.rps_max_proxy_mock_bound = Some(true);
            // Suppressed WITH its peak, and with its peak's evidence: a concurrency left behind would
            // be the operating point of a number the board is refusing to publish, and one carrying
            // the bare reason names the suppression without the ceiling that caused it.
            // conc_at_peak is the same fact again.
            out.rps_max_proxy_concurrency = carry_absence(&out.rps_max_proxy);
            out.conc_at_peak = carry_absence(&out.rps_max_proxy);
        }
        Some(false) => {
            out.rps_max_proxy = Measurement::Measured(value as i64);
            out.rps_max_proxy_mock_bound = Some(false);
            out.conc_at_peak = Measurement::Measured(i64::from(conc));
            // THE PUBLISHED FIELD. conc_at_peak alone was set here, so every measured peak shipped
            // with rps_max_proxy_concurrency still null - a peak with no operating point beside it,
            // while the value sat in the very next field. This is what consumers read.
            out.rps_max_proxy_concurrency = Measurement::Measured(i64::from(conc));
        }
        // The reference itself was unusable, so whether the rig bounded this is UNKNOWN. Publishing
        // the number would assert it was gateway-bound, which was never established.
        None => {
            // The verdict's OWN reason and evidence: "the reference was unmeasurable" and "the
            // reference measured something impossible" are different findings, and re-labelling
            // both as rig_limited with an invented sentence published two lies about one state.
            let why = verdict.detail().map(str::to_string).unwrap_or_else(||
                "the rig reference could not be measured, so it is unknown whether the gateway or the rig set this".to_string());
            let reason = verdict.reason().cloned().unwrap_or(Absent::RigLimited);
            out.rps_max_proxy = Measurement::absent_because(
                reason,
                format!("reached {value:.0} at c={conc}: {why}"),
            );
            out.rps_max_proxy_mock_bound = None;
            out.rps_max_proxy_concurrency = carry_absence(&out.rps_max_proxy);
            out.conc_at_peak = carry_absence(&out.rps_max_proxy);
        }
    }
}

/// The published per-cell memory window, from the numbers the memory group took.
///
/// This is the field `site/gen-data.mjs` reads memory from, and it reads it from NOWHERE ELSE: its
/// own comment says memory comes solely from the per-cell window, "No fallback, and NO per-gateway
/// memory scalar". While nothing filled this, every board built from this engine published no memory
/// for any gateway, which is a headline metric missing entirely rather than a number being wrong.
///
/// Absences travel intact. A window that could not find the gateway's process tree publishes null
/// with the reason naming the identity it looked for, never a zero: a benchmark that ranks memory
/// ascending would otherwise certify the gateway it failed to measure as the winner.
fn cell_memory(
    metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>,
    series: Option<&crate::metric::Series>,
) -> crate::record::CellMemory {
    let take = |k: &str| {
        metrics.get(k).cloned().unwrap_or_else(|| {
            Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field")
        })
    };
    let rss_series: Vec<crate::record::RssSample> =
        series.map(|s| s.rss.clone()).unwrap_or_default();
    let idle_rss_series: Vec<crate::record::RssSample> =
        series.map(|s| s.idle_rss.clone()).unwrap_or_default();
    // STEADY STATE, derived from the readings rather than left null. The trailing part of the window
    // is where the process has stopped growing, so its median is what the gateway actually costs
    // under sustained load, as distinct from the peak, which one spike can set. Absent when there
    // are too few readings to have a trailing part at all, because a "steady state" computed from
    // one sample would be the sample.
    //
    // ONLY THE LOAD WINDOW'S OWN READINGS. The sampler runs from before the load through the whole
    // recovery window, so `rss_series` is load THEN recovery, and taking the trailing half of the
    // WHOLE series mixes the two: on the 2026-07-29 run one-api's trailing half was 38s of load
    // against 60s of recovery, and the published figure came out 1.3 MiB under what the load window
    // measured. For a gateway that actually releases - 400 MiB under load, 50 after - the blend lands
    // between the two and is neither, while `recovered_rss_mib` beside it already answers "does it
    // give the memory back". Two fields the record deliberately distinguishes must not collapse into
    // each other.
    let load_end = metrics.get("memory_load_s").and_then(|m| m.copied());
    let steady = steady_state(&rss_series, load_end);
    // The plateau verdict travels as a number across the metric surface (every group speaks f64), so
    // it is turned back into the tri-state it really is here: settled, did not settle, or could not
    // be judged. An absent verdict must stay absent rather than collapsing to false, because "it did
    // not settle" is a claim about the gateway and "we could not tell" is a claim about the window.
    //
    // AS A MEASUREMENT, not an `Option<bool>`. `.copied().map(...)` threw the group's reason away on
    // the way through, and the record field it landed in was a plain `Option` the absences map could
    // not see - so a served cell whose window could not judge the plateau published a bare `null`
    // with nothing anywhere saying why, which is the one thing every other null in this artifact
    // cannot do. Same for `load_s` below.
    let plateaued = carry(&take("memory_plateaued"), |v| v != 0.0);
    let load_s = carry(&take("memory_load_s"), |v| v as i64);
    crate::record::CellMemory {
        // `served` here means the cell was served, which is the only reason a window ran at all.
        served: true,
        protocol: format!(
            "cold restart, idle read at rest, then load at c={} in repeated windows until the              trailing {}s is flat (cap {}s), then {}s with the load removed",
            crate::metric::MEMORY_WINDOW_CONCURRENCY,
            crate::metric::MEMORY_PLATEAU_WINDOW_S,
            crate::metric::MEMORY_MAX_LOAD_S,
            crate::metric::MEMORY_RECOVERY_S,
        ),
        idle_rss_mib: take("memory_idle_mib"),
        peak_rss_mib: take("memory_peak_mib"),
        peak_rss_hwm_mib: take("memory_hwm_mib"),
        recovered_rss_mib: take("memory_recovered_mib"),
        growth_rate_mib_per_min: take("memory_growth_rate_mib_per_min"),
        time_to_plateau_s: take("memory_time_to_plateau_s"),
        load_s,
        plateaued,
        // Both windows disclose their own length. idle_window_s has been null since the field was
        // declared, because idle was one instantaneous read and there was no window to name.
        idle_window_s: Some(crate::metric::MEMORY_IDLE_S as i64),
        recovery_window_s: Some(crate::metric::MEMORY_RECOVERY_S as i64),
        steady_state_rss_mib: steady,
        rss_series,
        idle_rss_series,
        shape: take("memory_shape"),
        idle_shape: take("memory_idle_shape"),
        ..Default::default()
    }
}

/// The median of the trailing half of the LOAD window's readings.
///
/// Median, not mean: one allocator spike at the end of a window would drag a mean and misreport the
/// level the process settled at. Trailing half, because the start of the window is the ramp, and
/// including the ramp measures how fast it grew rather than where it stopped.
///
/// `load_end_s` bounds which readings count. The series it is given spans the load AND the recovery
/// window that follows it, so without the bound the "trailing half" is mostly post-load samples and
/// this reports where memory settled AFTER the load stopped - which is `recovered_rss_mib`'s
/// question, not this one. `None` means the load duration was never established, and then the whole
/// series is all there is to work with.
fn steady_state(series: &[crate::record::RssSample], load_end_s: Option<f64>) -> Measurement<f64> {
    let under_load: Vec<&crate::record::RssSample> = match load_end_s {
        Some(end) => series.iter().filter(|s| (s.t_s as f64) <= end).collect(),
        None => series.iter().collect(),
    };
    let mut tail: Vec<f64> = under_load
        .iter()
        .skip(under_load.len() / 2)
        .filter_map(|s| s.rss_mib.copied())
        .collect();
    if tail.len() < 2 {
        return Measurement::absent_because(
            Absent::NotMeasured,
            format!(
                "the window produced {} usable reading(s), too few to say where memory settled",
                tail.len()
            ),
        );
    }
    tail.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = tail.len() / 2;
    let median = if tail.len().is_multiple_of(2) {
        (tail[mid - 1] + tail[mid]) / 2.0
    } else {
        tail[mid]
    };
    Measurement::Measured(median)
}

/// The published per-cell streaming block, from the numbers the streaming group took.
///
/// `stream_served` is derived from whether a difference was actually obtained, never asserted. A
/// dialect the mock cannot stream natively, or a leg that produced no frame, carries the group's own
/// reason as a STATUS rather than a `false`: `false` would say the gateway does not stream, which is
/// a claim about the gateway, and the reason we have is usually a claim about the rig.
fn cell_stream(
    cfg: &SuiteConfig,
    dialect: Dialect,
    metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>,
    series: Option<&crate::metric::Series>,
) -> crate::record::CellStream {
    // The record carries these as integer microseconds; the metric surface is f64 so every group has
    // one type. Truncation here loses at most a microsecond off a latency difference.
    let us = |k: &str| -> Measurement<i64> { as_i64(metrics.get(k)) };

    // A comparison COUNTS AS SERVED when it produced a number, and equally when the difference came
    // out below what the rig can resolve: both legs delivered frames and the comparison ran; only the
    // difference was too small to weigh. Publishing that as a status would read as "the stream did not
    // flow", the opposite of what happened.
    let flowed = |k: &str| {
        metrics
            .get(k)
            .is_some_and(|m| m.is_measured() || matches!(m.reason(), Some(Absent::BelowResolution)))
    };
    let ttft = metrics.get("added_ttft_p50_us");
    // THE VERDICT IS OVER EVERY STREAMING COMPARISON, NOT THE TTFT ONE ALONE. It read
    // `added_ttft_p50_us` and nothing else, so a cell whose gap figures measured cleanly - which can
    // only happen if frames flowed through the gateway and were timed - published `stream_served` as
    // the TTFT leg's absence token and read as a cell that did not stream, while carrying measured
    // streaming numbers two fields down.
    let (stream_served, reason, stream_error) = match ttft {
        Some(_) if flowed("added_ttft_p50_us") => {
            (crate::record::StreamServed::Bool(true), None, None)
        }
        // Probed, the TTFT half produced nothing, but the gap half did: served, with the TTFT
        // absence's own reason beside it so the partial is stated rather than implied.
        Some(m) if flowed("added_gap_p50_us") || flowed("added_gap_p99_us") => (
            crate::record::StreamServed::Bool(true),
            m.reason().map(|r| r.token().to_string()),
            m.detail().map(str::to_string),
        ),
        // Probed, and no comparison was a number. The reason travels as the status so a reader is
        // never left to infer a gateway property from a rig limit.
        Some(m) => match m.reason() {
            Some(r) => (
                crate::record::StreamServed::Status(r.token().to_string()),
                Some(r.token().to_string()),
                m.detail().map(str::to_string),
            ),
            None => (crate::record::StreamServed::default(), None, None),
        },
        None => (crate::record::StreamServed::default(), None, None),
    };

    let mut out = crate::record::CellStream {
        stream_served,
        // THE TOKEN, and the prose in the field that is for prose. `reason` carried the absence's
        // free-text detail while the identically named `Cell::reason` carries a token, so one name
        // meant two things in one artifact and a cell whose absence had no detail published a null
        // reason beside a status that plainly had one.
        reason,
        stream_error,
        added_ttft_p50_us: us("added_ttft_p50_us"),
        added_ttft_p99_us: us("added_ttft_p99_us"),
        added_gap_p50_us: us("added_gap_p50_us"),
        added_gap_p99_us: us("added_gap_p99_us"),
        stream_c1_note: stream_c1_note(metrics),
        // THE RUNGS THE TWO STREAM SEARCHES WALKED, published whatever the verdict, for the same
        // reason `sweep_max_proxy` is: when a ceiling is suppressed as mock-bound or absent because
        // the search ran out of range, the rungs are the only thing that explains why.
        sweep_streams: series.map(|s| s.sweep_streams.clone()).unwrap_or_default(),
        sweep_cpu_fps: series.map(|s| s.sweep_cpu_fps.clone()).unwrap_or_default(),
        ..Default::default()
    };
    judge_streams_sustained(cfg, dialect, &mut out, metrics);
    judge_cpu_fps(cfg, dialect, &mut out, metrics);
    out
}

/// What the concurrency-1 streaming legs were actually taken over.
///
/// `stream_c1_note` was declared and never set. What is worth saying there, and is said NOWHERE else
/// in the artifact, is how many frames each of the two single streams produced: the gap p50 this
/// block publishes is a median over the intervals BETWEEN those frames, so a leg that yielded three
/// frames and one that yielded sixty-four give the same field wildly different weight, and a reader
/// looking at a suspicious gap has no other way to tell which they are holding. `None` when the group
/// took no stream at all - there is nothing to describe, and a note saying "0 frames" beside an
/// absence that already carries its own reason would be the same fact twice.
fn stream_c1_note(
    metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>,
) -> Option<String> {
    let gw = metrics.get("gateway_c1_frames")?.copied()?;
    let direct = metrics.get("direct_c1_frames")?.copied()?;
    if gw <= 0.0 && direct <= 0.0 {
        return None;
    }
    Some(format!(
        "the c=1 streaming legs read {gw:.0} frame(s) through the gateway and {direct:.0} direct to \
         the mock, out of a {} frame budget; the added-gap figures are medians over the intervals \
         between those frames, and the p99 fields are absent because one stream cannot support a \
         99th percentile",
        crate::metric::STREAM_FRAME_BUDGET
    ))
}

/// Judge the streams-sustained ceiling against the MOCK's own frames/sec at the same concurrency.
///
/// The same shape as `judge_sustained`, and deliberately so: one cell must not contain two different
/// answers to "was this the rig or the gateway", one of them computed with `rigbound::is_rig_bound`
/// against a reference at the operating point and the other with a threshold invented here.
fn judge_streams_sustained(
    cfg: &SuiteConfig,
    dialect: Dialect,
    out: &mut crate::record::CellStream,
    metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>,
) {
    let missing =
        || Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field");
    let fps = metrics
        .get("streams_sustained_fps")
        .cloned()
        .unwrap_or_else(missing);
    let conc_m = metrics
        .get("streams_sustained")
        .cloned()
        .unwrap_or_else(missing);

    let (Some(&value), Some(&conc_f)) = (fps.value(), conc_m.value()) else {
        let absent: Measurement<f64> = carry_absence(&fps);
        out.streams_sustained = carry_absence(&absent);
        out.streams_sustained_fps = absent;
        return;
    };
    let conc = conc_f as u32;
    // c == 0 is `bisect_ceiling`'s own MEASURED "nothing sustains this gate": there is no concurrency
    // to take a reference AT, and a gateway that cannot carry a single clean stream cannot have been
    // bounded by the mock, because the mock was never asked to do anything.
    if conc == 0 {
        out.streams_sustained = Measurement::Measured(0);
        out.streams_sustained_fps = Measurement::Measured(0.0);
        out.streams_sustained_mock_bound = Some(false);
        return;
    }
    apply_streams_sustained_verdict(out, value, conc, stream_rig_ceiling(cfg, dialect, conc));
}

/// Judge the cpu-frames/sec peak against the same reference at the concurrency it peaked at.
fn judge_cpu_fps(
    cfg: &SuiteConfig,
    dialect: Dialect,
    out: &mut crate::record::CellStream,
    metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>,
) {
    let missing =
        || Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field");
    let fps = metrics.get("cpu_fps").cloned().unwrap_or_else(missing);
    let conc_m = metrics
        .get("cpu_fps_concurrency")
        .cloned()
        .unwrap_or_else(missing);

    let (Some(&value), Some(&conc_f)) = (fps.value(), conc_m.value()) else {
        let absent: Measurement<f64> = carry_absence(&fps);
        out.cpu_fps_concurrency = carry_absence(&absent);
        out.cpu_fps = absent;
        return;
    };
    let conc = conc_f as u32;
    // c == 0 is the search's own MEASURED "no concurrency carried a clean frame": the load was
    // offered and every rung failed the gate. Same semantics as `judge_streams_sustained`'s zero -
    // a real 0, never mock-bound, because the mock was never the limit of nothing.
    if conc == 0 {
        out.cpu_fps = Measurement::Measured(0.0);
        out.cpu_fps_concurrency = Measurement::Measured(0);
        out.cpu_fps_mock_bound = Some(false);
        return;
    }
    apply_cpu_fps_verdict(out, value, conc, stream_rig_ceiling(cfg, dialect, conc));
}

/// Re-wrap an absence so its reason AND its detail survive a narrowing. The searches attach their
/// lower bound as prose, and flattening that to a bare null is the one place "the engine discards the
/// measurement" was literally true.
///
/// GENERIC IN THE SOURCE TYPE as well as the target, so it also serves the companion fields:
/// `rps_max_proxy_concurrency` and `conc_at_peak` beside `rps_max_proxy`, and their sustained and
/// stream equivalents. Those were built with `Measurement::absent(reason)`, which keeps the label and
/// drops the evidence - "rig_limited" with no "against a rig ceiling of N at c=M" - so a reader who
/// opened the absences map at a twin's key got strictly less than at the primary's, about a value the
/// board is refusing to publish. One absence, one story, at every key that carries it.
///
/// A measured input yields `NotMeasured`, which is unreachable from the call sites (all inside an
/// absent branch) and is the conservative answer if one ever stops being.
fn carry_absence<A, T>(m: &Measurement<A>) -> Measurement<T> {
    match (m.reason().cloned(), m.detail()) {
        (Some(r), Some(d)) => Measurement::absent_because(r, d),
        (Some(r), None) => Measurement::absent(r),
        (None, _) => Measurement::absent(Absent::NotMeasured),
    }
}

/// Fill the streams-sustained fields from the mock verdict. PURE, and separate from its judge, for
/// the identical reason `apply_peak_verdict` is separate from `judge_cell`: the suite's own fixture
/// points the gateway and the mock at one server, so every cell comes back bound and the measured
/// branch would be unreachable from an end-to-end test.
fn apply_streams_sustained_verdict(
    out: &mut crate::record::CellStream,
    value: f64,
    conc: u32,
    reference: Measurement<f64>,
) {
    let verdict = rigbound::is_rig_bound(value, reference.clone());
    match verdict.copied() {
        // MATCHING THE MOCK'S RATE IS THE GATEWAY SUCCEEDING, NOT THE RIG LIMITING.
        //
        // The mock paces its deltas at a fixed interval and says so in its own header comment: "the
        // pacing is the model generating tokens; the stream suite measures what a gateway ADDS on
        // top of it". So the mock's frames/sec here is not a capacity it ran out of, it is the
        // TARGET RATE - c streams times one frame per interval. A gateway that forwards every frame
        // as it arrives lands at ~99% of it, which is the best possible outcome, and suppressing it
        // as rig-limited threw away the number for the gateways that did best.
        //
        // It cost 13 cells their streams_sustained and 11 their cpu_fps in the 2026-07-28 run.
        // agentgateway carried 12275 frames/sec against a "ceiling" of 12360 at c=256 - keeping pace
        // to within 0.7% - and published nothing. The arithmetic gives it away: every one of those
        // ceilings is c x (1000/20ms) to within 4%.
        //
        // So the rate is published, with the flag kept as the signal it always was: the gateway
        // matched the paced upstream. Whether it should have is what the GATE decides, and the gate
        // is about delivery - frames arriving, nothing stalling, streams not erroring - which is the
        // right question and is asked separately. A gateway that cannot keep pace falls short here
        // and that shortfall is its own, published rather than hidden.
        //
        // This is NOT the same judgement the throughput metrics make. There the mock's capacity
        // really can be the limit and publishing would rank the rig, so `apply_peak_verdict` keeps
        // suppressing. The difference is that a paced stream has a target and a saturating load does
        // not.
        Some(true) => {
            out.streams_sustained_fps = Measurement::Measured(value);
            out.streams_sustained_mock_bound = Some(true);
            out.streams_sustained = Measurement::Measured(i64::from(conc));
        }
        Some(false) => {
            out.streams_sustained_fps = Measurement::Measured(value);
            out.streams_sustained_mock_bound = Some(false);
            out.streams_sustained = Measurement::Measured(i64::from(conc));
        }
        None => {
            let why = verdict.detail().map(str::to_string).unwrap_or_else(||
                "the mock reference could not be measured, so it is unknown whether the gateway or the rig set this".to_string());
            let reason = verdict.reason().cloned().unwrap_or(Absent::RigLimited);
            out.streams_sustained_fps = Measurement::absent_because(
                reason,
                format!("carried {value:.0} frames/sec at c={conc}: {why}"),
            );
            out.streams_sustained_mock_bound = None;
            out.streams_sustained = carry_absence(&out.streams_sustained_fps);
        }
    }
}

/// The same verdict for the cpu-frames/sec peak.
fn apply_cpu_fps_verdict(
    out: &mut crate::record::CellStream,
    value: f64,
    conc: u32,
    reference: Measurement<f64>,
) {
    let verdict = rigbound::is_rig_bound(value, reference.clone());
    match verdict.copied() {
        // Same reasoning as the streams verdict above: the mock's frames/sec is a paced target, not
        // a capacity, so reaching it is the gateway keeping up rather than the rig running out.
        Some(true) => {
            out.cpu_fps = Measurement::Measured(value);
            out.cpu_fps_mock_bound = Some(true);
            out.cpu_fps_concurrency = Measurement::Measured(i64::from(conc));
        }
        Some(false) => {
            out.cpu_fps = Measurement::Measured(value);
            out.cpu_fps_mock_bound = Some(false);
            out.cpu_fps_concurrency = Measurement::Measured(i64::from(conc));
        }
        None => {
            let why = verdict.detail().map(str::to_string).unwrap_or_else(||
                "the mock reference could not be measured, so it is unknown whether the gateway or the rig set this".to_string());
            let reason = verdict.reason().cloned().unwrap_or(Absent::RigLimited);
            out.cpu_fps = Measurement::absent_because(
                reason,
                format!("peaked at {value:.0} frames/sec at c={conc}: {why}"),
            );
            out.cpu_fps_mock_bound = None;
            out.cpu_fps_concurrency = carry_absence(&out.cpu_fps);
        }
    }
}

/// The concurrency the box-qualification observation is taken at, and the band it must hold.
///
/// Both constants, for the same reason the memory window is: this number is compared against the
/// SAME box's previous runs, so anything that moves between runs makes the comparison meaningless.
const QUALIFY_CONCURRENCY: u32 = 32;
const QUALIFY_BAND_PCT: f64 = 20.0;

/// Qualify the BOX, before believing anything it measured about a gateway.
///
/// The observation is the rig's own throughput straight to the mock, with no gateway in the path, so
/// it is a property of the machine rather than of whatever is being benchmarked on it. Judged against
/// the median of the same observation from this box's previous runs.
///
/// `Sense::HigherIsBetter` and the one-sided band are `qualify`'s own decision, and its comment
/// explains why it must stay one-sided: a box cannot randomly get faster, so an improvement means the
/// BASELINE was the noisy run, and failing on it would terminate healthy boxes and burn the
/// replacement budget. Only degradation counts.
///
/// This RECORDS the verdict; it does not yet stop a failing run. `qualify`'s own header says the
/// incident it exists to prevent is running a full matrix on a bad box, so gating the run on a Fail
/// is the natural next step - but that changes what a run DOES, and it is a decision to take
/// deliberately rather than as a side effect of wiring the module in.
fn qualify_box(cfg: &SuiteConfig, history: &[f64]) -> serde_json::Value {
    let direct = RunConfig {
        gateway_addr: cfg.mock_addr,
        mock_addr: cfg.mock_addr,
        model: cfg.manifest.model.clone(),
        // Empty for the same reason as the rig reference: this drives the MOCK, not the gateway.
        egress_models: Default::default(),
        auth: cfg.manifest.auth.clone(),
        dialects: vec![Dialect::Openai],
        sweep_duration_s: cfg.sweep_duration_s,
        probe_timeout: Duration::from_secs(10),
        load_cores: cfg.load_cores.clone(),
        // No gateway is in this path at all, so there is no gateway process to attribute anything to.
        static_headers: Vec::new(),
        egress_headers: Default::default(),
        runtime: crate::manifest::Runtime::Native {
            proc_match: String::new(),
        },
        // The reference drives the MOCK directly. There is no gateway process behind it, so there is
        // nothing to restart, and a spec here would let a reference measurement bounce the gateway.
        relaunch: None,
        relaunch_commands: Vec::new(),
        relaunch_launcher: Default::default(),
        // The reference drives the MOCK, which serves every dialect at its standard path. A
        // gateway's prefix must not follow it here or the reference would probe a path the mock
        // does not have and the ceiling would read as unmeasurable.
        declared_path: String::new(),
        // The reference drives the MOCK at its standard paths; a gateway's override must not follow.
        cell_paths: Default::default(),
        matrix: Vec::new(),
        matrix_note: String::new(),
        untestable_cells: Vec::new(),
        untestable_note: String::new(),
    };
    let id = crate::cell::CellId::new(Dialect::Openai.as_str(), Dialect::Openai.as_str());
    let observed = run::measure_at(&direct, &id, QUALIFY_CONCURRENCY);

    let baseline = crate::qualify::rolling_baseline(history.to_vec());
    let (outcome, drift) = crate::qualify::judge(
        observed.clone(),
        baseline.clone(),
        QUALIFY_BAND_PCT,
        crate::qualify::Sense::HigherIsBetter,
    );

    serde_json::json!({
        "outcome": outcome.token(),
        "band_pct": QUALIFY_BAND_PCT,
        "concurrency": QUALIFY_CONCURRENCY,
        "observed_rps": observed.value().copied(),
        "observed_absent_reason": observed.reason().map(|r| format!("{r:?}")),
        "baseline_rps": baseline.value().copied(),
        "drift_pct": drift.value().copied(),
        "baseline_samples": history.len(),
    })
}

/// Every previous box-qualification observation available to this run.
///
/// TWO SOURCES, BECAUSE THE BOX HAS NO HISTORY OF ITS OWN.
///
/// Reading the results directory is right when the engine runs where the record lives. In the field
/// it does not: every gateway gets a FRESH EC2 instance that fetches its own manifest and the rig,
/// and nothing else. That directory is empty there, so the baseline was absent, so the qualification
/// seeded instead of judging - every box, every gateway, every run since the check was written. A
/// box running 30% slow would have seeded a fresh baseline and passed, and its gateway's whole
/// column would have been measured on a rig nothing compared against anything.
///
/// `OTB_QUALIFY_BASELINE` is how the orchestrator hands over what it knows: it holds the history and
/// the box does not. A single observation is enough to judge against - `rolling_baseline` takes the
/// median of what it is given - and the env value is appended to whatever the directory yields
/// rather than replacing it, so running where the record does live still uses the record.
///
/// The observation is the RIG's own loopback throughput at a fixed concurrency, not the gateway's,
/// which is why one pooled baseline across gateways is the right comparison and not a per-gateway
/// one: it is the box being qualified.
fn qualify_history(results_dir: &Path) -> Vec<f64> {
    let mut out = qualify_history_on_disk(results_dir);
    if let Some(v) = std::env::var("OTB_QUALIFY_BASELINE")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
    {
        if v > 0.0 {
            out.push(v);
        }
    }
    out
}

/// The part that reads the directory, separated so the env contribution above is testable without a
/// filesystem and this stays a pure function of what is on disk.
///
/// ONLY RUNS THAT QUALIFY CONTRIBUTE. `Outcome::qualifies_as_baseline` states the rule - a pass or a
/// seed is a measurement taken on a box nothing was wrong with, a fail is a box that was already
/// judged contaminated, and a skip measured nothing - but this harvested every record it could parse,
/// so a failed qualification's rps went straight into the median that decides whether the NEXT run
/// fails. A contaminated box would have spent its whole history dragging the band down toward itself
/// until nothing failed the gate at all, which is the gate quietly switching itself off.
///
/// A record with no readable outcome does not contribute either. "Not known to qualify" is treated as
/// "does not qualify" because the alternative is admitting a fail on the strength of it being
/// unlabelled, and the whole point of the filter is what it keeps out.
fn qualify_history_on_disk(results_dir: &Path) -> Vec<f64> {
    let Ok(entries) = std::fs::read_dir(results_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("result_") || !name.ends_with(".json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let qualifies = value
            .pointer("/rig/box_qualify/outcome")
            .and_then(serde_json::Value::as_str)
            .and_then(crate::qualify::Outcome::from_token)
            .is_some_and(|o| o.qualifies_as_baseline());
        if !qualifies {
            continue;
        }
        if let Some(rps) = value
            .pointer("/rig/box_qualify/observed_rps")
            .and_then(serde_json::Value::as_f64)
        {
            out.push(rps);
        }
    }
    out
}

/// Run the whole suite for one gateway and write its snapshot.
pub fn run_suite(cfg: &SuiteConfig, gateway_addr: SocketAddr) -> Result<Paths, SnapshotError> {
    run_suite_with(cfg, gateway_addr, crate::metric::METRICS)
}

/// The same suite over an explicit metric list, so a test can drive it end to end without paying for
/// every real measurement. `run_suite` passes the engine's real surface.
pub fn run_suite_with(
    cfg: &SuiteConfig,
    gateway_addr: SocketAddr,
    metrics: &[&dyn crate::metric::Metric],
) -> Result<Paths, SnapshotError> {
    // A HEADER THIS MANIFEST DECLARES THAT THE RIG ALREADY SENDS ITSELF, said once per gateway
    // before anything is measured.
    //
    // Ledger RIG-12's remainder. `run::headers_for` used to emit both copies, and HTTP does not
    // define which of two same-named headers a server honours - so a gateway authenticated as
    // somebody and published a clean number for a request whose credential nobody could name. The
    // wire is unambiguous now (the rig's copy wins, the manifest's is dropped), and this is the
    // other half of that fix: a precedence rule nobody is told about is the same silence as the
    // ambiguity it replaced.
    //
    // Here rather than in `Manifest::problems`, which is the "this gateway is misconfigured" gate a
    // real entrant must pass clean. This is not a setup mistake that breaks a launch: it is a line
    // to delete from a first-party file, and `gateways/litellm-rust/definition.json` has it today
    // (`Authorization: Bearer {GW_AUTH}`). Once per gateway, not once per cell, because a note
    // repeated 36 times is a note nobody reads.
    for note in cfg.manifest.rig_owned_headers_declared() {
        eprintln!("[manifest] {}: {note}", cfg.manifest.name);
    }
    let rc = RunConfig {
        gateway_addr,
        mock_addr: cfg.mock_addr,
        model: cfg.manifest.model.clone(),
        egress_models: cfg.manifest.egress_models.clone(),
        auth: cfg.manifest.auth.clone(),
        dialects: cfg.dialects.clone(),
        sweep_duration_s: cfg.sweep_duration_s,
        probe_timeout: Duration::from_secs(10),
        load_cores: cfg.load_cores.clone(),
        // How to put this gateway back at rest, so the memory group can read an idle that is
        // actually idle. Built from the SAME manifest declaration the initial launch used, so a
        // restart cannot differ from the launch it is repeating. `None` for a manifest that declares
        // no launch: the harness does not own that gateway's lifetime and must not bounce it.
        declared_path: cfg.manifest.path.clone(),
        cell_paths: cfg
            .manifest
            .cell_paths_for(&cfg.gw_cores, cfg.mock_addr.port(), &cfg.gw_dir),
        matrix: cfg.manifest.matrix.clone(),
        matrix_note: cfg.manifest.matrix_note.clone(),
        untestable_cells: cfg.manifest.untestable.clone(),
        untestable_note: cfg.manifest.untestable_note.clone(),
        relaunch: cfg
            .manifest
            .launch_spec(
                &cfg.gw_cores,
                cfg.mock_addr.port(),
                &cfg.gw_dir,
                Duration::from_secs(60),
                Duration::from_secs(2),
            )
            .and_then(|r| r.ok()),
        // Replayed on every restart: a docker stop destroys the writable layer, and with it any
        // configuration these commands wrote (one-api's channels and quota live in an in-container
        // database). See `RunConfig::relaunch_commands`.
        relaunch_commands: cfg.manifest.commands.clone(),
        // The launcher `restart_to_rest` reuses across every cell this run measures, so a native
        // child it spawns is still reachable - and reapable - the next time this same gateway is
        // put back at rest.
        relaunch_launcher: Default::default(),
        // The gateway's own headers, resolved once for the run. A column whose headers cannot be
        // resolved gets NONE rather than a partial set: sending half a routing header selects the
        // wrong upstream and publishes a number for a pairing that was never driven.
        static_headers: cfg
            .manifest
            .headers_for("", &cfg.gw_cores, cfg.mock_addr.port(), &cfg.gw_dir)
            .unwrap_or_default(),
        egress_headers: cfg
            .dialects
            .iter()
            .filter_map(|d| {
                let mut h = cfg
                    .manifest
                    .headers_for(d.as_str(), &cfg.gw_cores, cfg.mock_addr.port(), &cfg.gw_dir)
                    .ok()?;
                // headers_for prepends the always-on set; the run config carries those separately, so
                // strip them here rather than sending each one twice.
                let statics = cfg
                    .manifest
                    .headers_for("", &cfg.gw_cores, cfg.mock_addr.port(), &cfg.gw_dir)
                    .ok()?;
                h.retain(|x| !statics.contains(x));
                Some((d.as_str().to_string(), h))
            })
            .collect(),
        runtime: cfg.manifest.runtime.clone(),
    };

    // THE MOCK IS PUT BACK IN ITS MEASURING STATE BEFORE ANYTHING IS MEASURED.
    //
    // The mock's recorder is a per-request lock in the one process whose throughput is the reference
    // every gateway's number is judged against, so it must be off for every load window and on only
    // for the single re-verification request per cell. `reverify_cell` already holds that invariant
    // per cell, but the box-qualification window below runs BEFORE the first cell - and a box that
    // boots the mock with MOCK_RECORD=1 would take that one window, the one whose rate becomes this
    // box's rolling baseline, against a recording mock while every window after it runs against a
    // quiet one.
    if let Some(why) = crate::reverify::quiesce_recorder(&rc) {
        eprintln!("suite: the mock's recorder could not be quiesced before the run: {why}");
    }

    // THE BOX IS QUALIFIED BEFORE THE GRID, not after. The verdict is about the machine every
    // number below is measured on, so taking it afterwards would judge a box using a reading taken
    // once the run had already finished loading it.
    let box_qualify = qualify_box(cfg, &qualify_history(Path::new(&cfg.results_dir)));

    let mut upstreams: HashMap<String, Upstream> = HashMap::new();
    let mut any_served = false;
    let mut last_egress: Option<String> = None;
    let mut written: Option<Paths> = None;

    // WRITTEN INCREMENTALLY, after every egress column, not held in memory and written once at the
    // end: these runs take hours on a box with a hard self-termination timer, so a run interrupted
    // partway through must not lose every cell it already measured. Partial progress that survives
    // is worth more than a complete result that might not arrive.
    //
    // A PROMOTE-GUARD TRIP ON ONE OF THESE CHECKPOINTS IS NOT THE SAME EVENT AS ON THE FINAL WRITE.
    // Every checkpoint before the last is, by construction, thinner than the finished run it is
    // partway through, so tripping the guard against a fuller prior snapshot already on disk is
    // expected mid-run, not a sign the run went bad. Propagating it with `?` used to abort this whole
    // function - discarding every cell already measured for the rest of the grid and never reaching
    // the final flush that would have carried the complete run. So a checkpoint trip is logged and
    // skipped; only the FINAL flush below, once every column has been measured, may make the guard's
    // refusal fatal.
    // STREAMED, so the checkpoint above is a checkpoint. This was `for result in run_grid_with(...)`,
    // which returns a Vec: the whole grid had to finish before the loop began, so the per-column
    // flush - and the promise in the comment above it - could never fire on an interrupted run.
    // Busbar measured 16 of 36 cells over four hours and not one of them reached disk.
    run::run_grid_streaming(&rc, cfg.min_conc, cfg.max_conc, metrics, &mut |result| {
        let id = &result.outcome.id;
        let ing = id.ingress.clone();
        let eg = id.egress.clone();

        if last_egress.as_deref() != Some(eg.as_str()) {
            if let Some(finished_egress) = &last_egress {
                match flush(cfg, &upstreams, any_served, Some(box_qualify.clone())) {
                    Ok(paths) => written = Some(paths),
                    Err(SnapshotError::PromoteGuard {
                        existing_served,
                        incoming_served,
                    }) => {
                        eprintln!(
                            "suite: checkpoint after egress column {finished_egress} not written yet \
                             ({incoming_served} served so far vs {existing_served} on disk) - \
                             continuing to measure the rest of the grid"
                        );
                    }
                    // A CHECKPOINT THAT FAILS TO WRITE MUST NOT DISCARD THE CELLS STILL TO COME.
                    // This used to `return Err`, abandoning the rest of the grid over a write that
                    // the FINAL flush is about to attempt again anyway - the same reasoning the
                    // promote-guard arm above already applies, arriving by a different error. The
                    // failure is remembered, not swallowed: if the final write also fails, that is
                    // the error the run reports, and if it succeeds the run really is fine and this
                    // was transient. Either way, hours of measurement are not thrown away over a
                    // mid-run disk hiccup.
                    Err(e) => {
                        eprintln!(
                            "suite: checkpoint after egress column {finished_egress} failed to \
                             write ({e}) - continuing to measure; the final write decides the run"
                        );
                    }
                }
            }
            last_egress = Some(eg.clone());
        }

        // THE EVIDENCE FOR THE VERDICT, not just the verdict: `status` and `body_snippet` are
        // recorded on every cell, so an artifact can say what the gateway actually answered instead
        // of just "does not serve" 36 times over. Otherwise a whole field declining for one
        // rig-side reason looks exactly like a field of gateways that support nothing.
        let (served, reason, status, snippet) = match &result.outcome.served {
            Served::Yes => (RecServed::Bool(true), None, String::new(), String::new()),
            Served::No(v, ev) => (
                RecServed::Status(v.token().to_string()),
                Some(v.token().to_string()),
                ev.status.to_string(),
                ev.body_snippet.clone(),
            ),
            // A rig limit is NOT the gateway refusing. It keeps its own label all the way out.
            Served::Untestable(r) => {
                (RecServed::Status("untestable".into()), Some(r.clone()), String::new(), String::new())
            }
            // Outside the manifest's own declared capability grid: never probed, so there is no
            // status/body evidence to carry - the note IS the evidence here.
            Served::NotConfigurable(r) => {
                (RecServed::Status("not_configurable".into()), Some(r.clone()), String::new(), String::new())
            }
            // The gateway refused a credential we cannot legitimately produce. The evidence travels
            // because a reader has to be able to see it was an auth refusal and not a capability
            // answer, and the label stays distinct from `failed` so the board never renders it red.
            Served::UnprobedAuth(ev) => (
                RecServed::Status("unprobed_auth".into()),
                Some(format!(
                    "this dialect's real clients sign their requests and the harness does not forge \
                     signatures, so its {} is a refusal of our credential rather than an answer about \
                     the pairing",
                    ev.status
                )),
                ev.status.to_string(),
                ev.body_snippet.clone(),
            ),
        };
        if matches!(served, RecServed::Bool(true)) {
            any_served = true;
        }

        let (perf, stream, perf_dropped) = assemble_cell_measurements(cfg, &result, &ing, &eg);

        let cell = Cell {
            perf_dropped,
            served,
            reason,
            // THE PATH THIS CELL WAS ACTUALLY DRIVEN AT, not the dialect's standard one recomputed
            // after the fact. A cell measured on a provider-pinned route and a cell measured on the
            // unified route are different measurements, and the artifact has to say which it was or
            // the board presents them as the same number.
            path: ing
                .parse::<Dialect>()
                .map(|d| run::path_for(&rc, d, &eg))
                .unwrap_or_default(),
            status,
            body_snippet: snippet,
            // A FAILED RE-VERIFICATION MUST BE VISIBLE WITHOUT OPENING THE PERF BLOCK. `verdict_note`
            // is the per-cell evidence string every consumer already reads, so a cell that answered
            // 200 without translating says so where "does this gateway serve this pairing" is
            // answered, not only in a field a reader has to know to look for.
            verdict_note: match (result.reverify.verified, &result.reverify.note) {
                (Some(false), Some(note)) => format!("egress not re-verified: {note}"),
                _ => result.outcome.note.clone().unwrap_or_default(),
            },
            perf,
            memory: result
                .metrics
                .as_ref()
                .map(|m| cell_memory(m, result.series.as_ref())),
            stream,
            // What this cell COST, per metric group. Owned strings because the record is
            // deserialisable and a &'static str key cannot come back off disk.
            timings_s: result
                .timings_s
                .as_ref()
                .map(|t| t.iter().map(|(k, v)| ((*k).to_string(), *v)).collect()),
            ..Default::default()
        };

        // Read before `entry` takes ownership of the key.
        let configurable = cfg.manifest.egress.iter().any(|e| e == &eg);
        upstreams
            .entry(eg)
            // `configurable` is whether this gateway can be POINTED at this upstream at all, which
            // is exactly what the manifest's `egress` list declares, so it is read from there rather
            // than hardcoded. It was `true` unconditionally, which made the field a constant: every
            // egress column claimed to be configured, including ones the manifest never wired, and a
            // constant that is published as an observation is the shape of a false claim even when
            // it happens to be right. `served` stays true here because this entry is only created
            // when a cell for this egress produced a row at all.
            .or_insert_with(|| Upstream {
                configurable,
                served: true,
                ..Default::default()
            })
            .cells
            .insert(ing, cell);
    });

    // The final write always happens, so a grid with a single egress column is not lost.
    let _ = written;
    flush(cfg, &upstreams, any_served, Some(box_qualify))
}

/// A REFUTED RE-VERIFICATION WITHHOLDS THE NUMBERS, IT DOES NOT MERELY ANNOTATE THEM.
///
/// `Some(false)` is leg 3 PROVING the request did not reach the mock as this cell's egress dialect
/// (`reverify.rs`): either the gateway forwarded the ingress bytes verbatim or it emitted some third
/// dialect. Every perf and stream number on the cell was then taken over that same wrong wire, so
/// publishing them under `<ingress>-><egress>` states a translation throughput for a translation that
/// did not happen - the exact false positive the guard exists to prevent, arriving through the
/// numbers instead of through `served`.
///
/// `perf_dropped` was documented as the mechanism for precisely this and no writer ever set it, so a
/// refutation published the refuted figures untouched with a note beside them. THE EVIDENCE ALL
/// STAYS: `verdict_note` still names the refutation, `egress_reverified` still reads `false`,
/// `reverify_note` still says what the mock actually received. What goes is the numbers, and
/// `perf_dropped` says so in one string a consumer can branch on.
///
/// `None` (not checked: a diagonal cell, a mock that was not recording, a mock we could not reach)
/// and `Some(true)` both leave the blocks alone. Only PROOF of a misroute withholds; suspicion does
/// not, or every unrecordable run would silently stop publishing.
///
/// Pure, and separate from the grid loop, because this is the one branch in the suite that decides
/// whether a measured number reaches the board at all, and the loop around it cannot be driven to a
/// refutation from a fixture where one loopback server plays both the gateway and the mock.
/// Build a cell's perf and stream blocks and apply the refutation withholding to BOTH, as one step.
///
/// Extracted from `run_suite_with` so the composition is drivable. `withhold_if_refuted` had thorough
/// tests, but nothing tested that it was CALLED: the whole block lived inline in a function that needs
/// a gateway, a mock and a socket to reach, so deleting the call left every test green while a cell
/// whose egress was PROVEN MISROUTED published its numbers as an unqualified translation claim. A
/// guard that cannot be shown to run is not a guard, which is the defect class this codebase treats as
/// its worst, so the guard and its invocation now live behind one pure function a test can drive.
fn assemble_cell_measurements(
    cfg: &SuiteConfig,
    result: &crate::run::CellResult,
    ing: &str,
    eg: &str,
) -> (
    Option<crate::record::CellPerf>,
    Option<crate::record::CellStream>,
    Option<String>,
) {
    let perf = match (&result.metrics, ing.parse::<Dialect>()) {
        (Some(m), Ok(d)) => {
            let mut p = judge_cell(cfg, d, m).perf;
            // THE RUNGS THE SEARCH WALKED. Published whatever the verdict: when the peak is
            // suppressed as rig-bound, or absent because the search never found a turnover, the
            // sweep is the only thing that explains WHY, and a bare null with no points beside
            // it is unreviewable.
            if let Some(series) = result.series.as_ref() {
                p.sweep_max_proxy = series.sweep.clone();
                p.sweep_sustained_20ms = series.sweep_sustained.clone();
            }
            // THE ANTI-FALSE-POSITIVE GUARD, published beside the numbers it qualifies. A cell
            // whose egress dialect was NOT PROVEN (`None`) still carries its measurements - they
            // are real observations of what the gateway did - but it can no longer be read as an
            // unqualified translation claim, because the flag beside them says so and the note
            // names what the mock actually received instead. See `reverify.rs` for why `None`
            // (diagonal cell, recording off, mock unreachable) is a first-class third answer
            // rather than a failure. A `Some(false)` - proof of a MISROUTE - is handled below;
            // that one does not get to publish numbers at all.
            p.egress_reverified = result.reverify.verified;
            p.reverify_note = result.reverify.note.clone();
            Some(p)
        }
        _ => None,
    };

    let stream = match (&result.metrics, ing.parse::<Dialect>()) {
        (Some(m), Ok(d)) => Some(cell_stream(cfg, d, m, result.series.as_ref())),
        _ => None,
    };

    withhold_if_refuted(&result.reverify, perf, stream, ing, eg)
}

fn withhold_if_refuted(
    reverify: &crate::reverify::Reverified,
    perf: Option<CellPerf>,
    stream: Option<crate::record::CellStream>,
    ingress: &str,
    egress: &str,
) -> (
    Option<CellPerf>,
    Option<crate::record::CellStream>,
    Option<String>,
) {
    if reverify.verified != Some(false) {
        return (perf, stream, None);
    }
    let why = reverify
        .note
        .clone()
        .unwrap_or_else(|| "no evidence was recorded with the refutation".to_string());
    (
        perf.map(|p| withhold_refuted_perf(p, &why)),
        stream.map(|s| withhold_refuted_stream(s, &why)),
        Some(format!(
            "perf and stream withheld: re-verification proved this cell's request did not reach the \
             mock as {egress}, so every number taken here belongs to a wire that is not \
             {ingress}>{egress}: {why}"
        )),
    )
}

/// Strip a refuted cell's measurements, keeping the block and the evidence that refuted it.
///
/// The block is kept rather than dropped to `None` because `egress_reverified` and `reverify_note`
/// live in it, and they are the finding: a cell with no perf block at all cannot say WHY it has no
/// numbers, which is how a refutation would come to look like a cell that was never measured.
///
/// `NotServed` is the reason on every field, and it is the honest one out of the vocabulary: the
/// refutation is a statement about the GATEWAY (it did not serve this pairing - it answered by
/// speaking some other dialect upstream), not about the rig and not about a search that ran out of
/// room. The detail carries the mock's own evidence so the null is never bare.
///
/// The sweeps go too. They are rps against concurrency on the same misrouted wire, and a rung is as
/// much a published number as the ceiling drawn from it.
fn withhold_refuted_perf(p: CellPerf, why: &str) -> CellPerf {
    let detail = format!(
        "withheld: re-verification proved the request did not reach the mock as this cell's egress \
         dialect, so this number was taken over a wire that is not this pairing: {why}"
    );
    let withheld = || Measurement::absent_because(Absent::NotServed, detail.clone());
    CellPerf {
        added_latency_p50_us: withheld(),
        added_latency_p99_us: withheld(),
        gateway_c1_p99_us: withheld(),
        direct_c1_p99_us: withheld(),
        rps_sustained_20ms: withheld(),
        rps_sustained_20ms_concurrency: withheld(),
        conc_at_sustained: withheld(),
        rps_sustained_20ms_mock_bound: None,
        rps_max_proxy: withheld(),
        rps_max_proxy_concurrency: withheld(),
        conc_at_peak: withheld(),
        rps_max_proxy_mock_bound: None,
        sweep_max_proxy: Vec::new(),
        sweep_sustained_20ms: Vec::new(),
        // THE EVIDENCE, kept verbatim: this is the whole reason the block survives.
        egress_reverified: p.egress_reverified,
        reverify_note: p.reverify_note,
        // A sample-count note about legs that measured the wrong wire would describe the weight of a
        // number nobody is publishing.
        c1_note: None,
    }
}

/// The streaming half of the same withholding, and it exists because the block used to be DROPPED.
///
/// `withhold_if_refuted` set `stream` to `None` while keeping the perf block, which contradicts the
/// reasoning directly above: a cell with no block at all cannot say WHY it has no numbers, so a
/// refuted stream read as a cell that was never probed rather than one whose numbers were withheld.
/// The evidence went with it - `stream_served`, the reason, the prose and the rungs all vanished,
/// leaving `perf_dropped`'s sentence as the only thing that said otherwise, and that is prose.
///
/// Same rule as the perf half, for the same reason: `NotServed` on every measurement (the refutation
/// is a statement about the GATEWAY - it answered by speaking some other dialect upstream), the
/// mock's own evidence as the detail so no null is bare, and the sweeps dropped because a rung
/// measured on a misrouted wire is as much a published number as the ceiling drawn from it.
fn withhold_refuted_stream(s: crate::record::CellStream, why: &str) -> crate::record::CellStream {
    let detail = format!(
        "withheld: re-verification proved the request did not reach the mock as this cell's egress \
         dialect, so this number was taken over a wire that is not this pairing: {why}"
    );
    // A generic fn rather than a closure: this block mixes `Measurement<i64>` (the percentiles, the
    // concurrencies) with `Measurement<f64>` (the rates), and a closure would fix itself to whichever
    // it saw first.
    fn withheld<T>(detail: &str) -> Measurement<T> {
        Measurement::absent_because(Absent::NotServed, detail.to_string())
    }
    crate::record::CellStream {
        added_ttft_p50_us: withheld(&detail),
        added_ttft_p99_us: withheld(&detail),
        added_gap_p50_us: withheld(&detail),
        added_gap_p99_us: withheld(&detail),
        streams_sustained: withheld(&detail),
        streams_sustained_fps: withheld(&detail),
        streams_sustained_mock_bound: None,
        cpu_fps: withheld(&detail),
        cpu_fps_concurrency: withheld(&detail),
        cpu_fps_mock_bound: None,
        sweep_streams: Vec::new(),
        sweep_cpu_fps: Vec::new(),
        // A note about how many frames the c=1 legs read would describe the weight of a number
        // nobody is publishing, exactly as the perf half's sample-count note would.
        stream_c1_note: None,
        // THE EVIDENCE, kept verbatim: whether it streamed at all, and why it says what it says.
        // This is the whole reason the block survives rather than becoming a None.
        stream_served: s.stream_served,
        reason: s.reason,
        stream_error: s.stream_error,
    }
}

/// What the harness actually launched, as the artifact's `build` string.
///
/// Read back off the resolved LaunchSpec rather than off the manifest text, so it names the image
/// that was really started after placeholder substitution, not the template that described it.
fn gateway_build(cfg: &SuiteConfig) -> Option<String> {
    let spec = cfg
        .manifest
        .launch_spec(
            &cfg.gw_cores,
            cfg.mock_addr.port(),
            &cfg.gw_dir,
            Duration::from_secs(60),
            Duration::from_secs(2),
        )?
        .ok()?;
    match &spec.kind {
        crate::launch::LaunchKind::Docker { image, .. } => Some(image.clone()),
        // A gateway built from source on the box has no image to name. Its identity is the runtime
        // the manifest declares, which is at least the thing the memory reader and the stop path
        // agree on, rather than a version string invented here.
        _ => Some(spec.runtime.identity().to_string()),
    }
}

/// Build the record from what has been measured so far and write it.
/// THE CONFIG THE GATEWAY ACTUALLY RAN FROM, captured into the same artifact as the numbers.
///
/// `ResultSnapshot.config` existed, was serialized, was read by `gen-data.mjs` as its FIRST choice of
/// config source, and nothing ever filled it: the field fell through to `Default`, so every gateway
/// on the board published `config: {files: {}}` and every drawer read "Config: not published".
///
/// That is not cosmetic. The config is the evidence for the claim the whole board rests on - that
/// each gateway was measured as it ships, not as we tuned it. Without it a reader is asked to take
/// that on trust, which is the one thing this project refuses to ask.
///
/// Read at flush time from the gateway's own directory, so what lands in the artifact is the text
/// the process was actually started with rather than a template re-rendered later - the exact class
/// of bug this field's own doc comment describes ("a chart read against a config that was since
/// overwritten on disk"). A file that cannot be read is skipped rather than failing the run: a
/// finished measurement must not be discarded over its own provenance, and an absent config still
/// renders honestly as "not published".
fn rendered_config(cfg: &SuiteConfig) -> crate::record::ConfigFiles {
    let mut files = std::collections::HashMap::new();
    for f in &cfg.manifest.config_files {
        let path = cfg.gw_dir.join(&f.output);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                files.insert(f.output.clone(), text);
            }
            Err(e) => eprintln!(
                "suite: config {} could not be read for the artifact ({e}) - the run stands, but \
                 this gateway publishes no config",
                path.display()
            ),
        }
    }
    crate::record::ConfigFiles { files }
}

fn flush(
    cfg: &SuiteConfig,
    upstreams: &HashMap<String, Upstream>,
    any_served: bool,
    box_qualify: Option<serde_json::Value>,
) -> Result<Paths, SnapshotError> {
    // The rig block exists if ANY part of it does: box_qualify, the engine stamp and the mock
    // provenance are independent facts about the instrument, so keying the whole block off just one
    // of them (box_qualify) would let a run with no qualification file drop the engine commit too.
    let rig = (box_qualify.is_some() || cfg.engine_stamp.is_some() || cfg.rig_mock.is_some()).then(
        || crate::record::RigProvenance {
            arch: Some(cfg.arch.clone()),
            engine: cfg.engine_stamp.clone(),
            release_url: cfg.rig_release_url.clone(),
            mock: cfg.rig_mock.clone(),
            // The load generator is this engine's own `loadgen` subcommand, so rig.engine names it.
            ugen: None,
            box_qualify,
        },
    );
    // Resolved once and used at BOTH levels. `gateway_build` re-resolves the manifest's launch spec,
    // and the matrix must name the same build the root does or the two levels of one artifact would
    // disagree about what was measured.
    let build =
        gateway_build(cfg).unwrap_or_else(|| format!("otb-engine {}", env!("CARGO_PKG_VERSION")));
    let snap = ResultSnapshot {
        schema_version: 1,
        config: rendered_config(cfg),
        gateway: cfg.manifest.name.clone(),
        // WHAT WAS MEASURED, not what measured it: the engine identifies itself in rig.engine,
        // where it belongs, so this field must be the gateway's own build string, not the engine's,
        // or every gateway's artifact would claim the same build and a reader could not tell which
        // image produced a number. Falls back to the engine string only when the manifest declares
        // no launch, i.e. when the harness did not start the thing it measured and genuinely does
        // not know its build.
        build: build.clone(),
        measured_at: cfg.measured_at.clone(),
        arch: Some(cfg.arch.clone()),
        hardware: cfg.hardware.clone(),
        rig: rig.clone(),
        matrix: Matrix {
            gateway: cfg.manifest.name.clone(),
            served: any_served,
            // WHICH PHASES THIS RUN WAS TOLD TO MEASURE. The consumer reads these to detect a
            // degraded run and refuses to publish one over a complete one, so an inaccurate flag
            // here is a claim about the run itself. All three phases are in `metric::METRICS` and
            // every served cell goes through all of them, so all three are on. Leaving them to
            // `..Default::default()` published `cell_stream: false` on every snapshot the engine has
            // ever written - the run declaring it had not measured streaming while carrying a full
            // streaming block on every cell.
            cell_perf_sweep: true,
            cell_stream: true,
            cell_memory: Some(true),
            upstreams: upstreams.clone(),
            // Mirrored onto the matrix as well as the snapshot root: record.rs carries the field in
            // both places, and a reader that finds it in one and not the other cannot tell which is
            // authoritative.
            rig,
            // THE SAME REASONING, FOR THE SAME REASON, one level down. The root's hardware/arch/build
            // were filled and the matrix's were left at Default, so the block that holds every number
            // could not say what machine or build produced them: exactly the "hardware: null" episode
            // the root's own field comment records, repeating inside the block a reader exports and
            // diffs on its own. There is nothing to derive here - the values are already in hand.
            build,
            arch: Some(cfg.arch.clone()),
            hardware: cfg.hardware.clone(),
            measured_at: cfg.measured_at.clone(),
            ..Default::default()
        },
        ..Default::default()
    };

    snapshot::write_snapshot(Path::new(&cfg.results_dir), &snap)
}

#[cfg(test)]
mod tests {
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
        let paths =
            run_suite_with(&cfg, gw, crate::metric::METRICS).expect("the suite should write");
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
        std::fs::write(gw_dir.join("config.gen.yaml"), "listen: 8080\nupstream: mock\n")
            .expect("config");
        cfg.gw_dir = gw_dir;
        cfg.manifest.config_files = vec![crate::manifest::ConfigFile {
            template: "config.gen.yaml.tmpl".into(),
            output: "config.gen.yaml".into(),
        }];

        let got = rendered_config(&cfg);
        assert_eq!(got.files.len(), 1, "the rendered config must reach the artifact: {got:?}");
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
        assert!(rendered_config(&cfg).files.is_empty(), "an unreadable config is absent, not fatal");

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
        let cfg = cfg_for(&dir, gw);
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The rig judgement is WIRED. A gateway measured against itself as the reference is by
    // definition at 100% of the reference, so it must be suppressed rather than published.
    // A peak with no operating point beside it is not a measurement anyone can reproduce. The first
    // real EC2 run published rps_max_proxy=46863 with rps_max_proxy_concurrency=null while the very
    // next field, conc_at_peak, held 116: the measured branch set conc_at_peak and left the PUBLISHED
    // field at empty_perf()'s default. The two halves of one fact must move together.
    #[test]
    fn a_measured_peak_publishes_the_concurrency_it_happened_at() {
        let mut out = empty_perf();
        // A reference far ABOVE the observation, so the rig plainly did not set this number and the
        // verdict is the measured branch. Driving this through judge_cell instead is what made the
        // first version of this test vacuous: its fixture is always rig-bound.
        apply_peak_verdict(&mut out, 46_863.0, 116, Measurement::Measured(400_000.0));
        assert_eq!(
            out.rps_max_proxy.copied(),
            Some(46_863),
            "a gateway-bound peak is published"
        );
        assert_eq!(
            out.rps_max_proxy_concurrency.copied(),
            Some(116),
            "the published peak must carry the concurrency it happened at"
        );
        assert_eq!(out.conc_at_peak.copied(), Some(116));
    }

    // The other direction: a suppressed peak must not leave its operating point behind, or the board
    // shows the concurrency of a number it is deliberately refusing to publish.
    #[test]
    fn a_suppressed_peak_leaves_no_concurrency_behind() {
        let mut out = empty_perf();
        // Reference equal to the observation: the rig, not the gateway, set this.
        apply_peak_verdict(&mut out, 46_863.0, 116, Measurement::Measured(46_863.0));
        assert_eq!(
            out.rps_max_proxy.copied(),
            None,
            "a rig-bound peak is suppressed"
        );
        assert_eq!(
            out.rps_max_proxy_concurrency.copied(),
            None,
            "its operating point must be suppressed with it"
        );
    }

    // ── apply_sustained_verdict: the same rig-bound machinery as the peak, applied to the gate ──────

    #[test]
    fn a_measured_sustained_ceiling_publishes_the_concurrency_it_held_at() {
        let mut out = empty_perf();
        // Comfortably below the reference: the gateway's own gate, not the rig's.
        apply_sustained_verdict(&mut out, 11_968.0, 1024, Measurement::Measured(400_000.0));
        assert_eq!(out.rps_sustained_20ms.copied(), Some(11_968));
        assert_eq!(
            out.rps_sustained_20ms_concurrency.copied(),
            Some(1024),
            "the published rate must carry the concurrency it was sustained at"
        );
        assert_eq!(out.conc_at_sustained.copied(), Some(1024));
        assert_eq!(out.rps_sustained_20ms_mock_bound, Some(false));
    }

    #[test]
    fn a_rig_bound_sustained_ceiling_is_suppressed_with_its_concurrency() {
        let mut out = empty_perf();
        // Reference equal to the observation: the rig, not the gateway, set this ceiling.
        apply_sustained_verdict(&mut out, 11_968.0, 1024, Measurement::Measured(11_968.0));
        assert_eq!(
            out.rps_sustained_20ms.copied(),
            None,
            "a rig-bound sustained rate is suppressed"
        );
        assert_eq!(
            out.rps_sustained_20ms_concurrency.copied(),
            None,
            "its operating point must be suppressed with it"
        );
        assert_eq!(out.conc_at_sustained.copied(), None);
        assert_eq!(out.rps_sustained_20ms_mock_bound, Some(true));
    }

    #[test]
    fn an_unusable_sustained_reference_is_unknown_never_a_guessed_false() {
        let mut out = empty_perf();
        apply_sustained_verdict(
            &mut out,
            11_968.0,
            1024,
            Measurement::absent(Absent::NotMeasured),
        );
        assert_eq!(out.rps_sustained_20ms.copied(), None);
        assert_eq!(
            out.rps_sustained_20ms_mock_bound, None,
            "an unmeasurable rig reference must not be guessed as gateway-bound"
        );
    }

    // c == 0 is bisect_ceiling's own MEASURED "nothing sustains this gate" answer, and it is handled
    // in `judge_sustained` itself (there is no concurrency to take a rig reference at), so this drives
    // the whole function rather than `apply_sustained_verdict`.
    #[test]
    fn nothing_sustaining_the_gate_publishes_a_real_measured_zero_never_rig_bound() {
        let dir = tmpdir("sustained-zero");
        let gw = serve(200);
        let cfg = cfg_for(&dir, gw);
        let mut out = empty_perf();
        let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
            std::collections::BTreeMap::new();
        metrics.insert("rps_sustained_20ms", Measurement::Measured(0.0));
        metrics.insert("rps_sustained_20ms_concurrency", Measurement::Measured(0.0));
        judge_sustained(&cfg, Dialect::Openai, &mut out, &metrics);
        assert_eq!(out.rps_sustained_20ms.copied(), Some(0));
        assert_eq!(out.rps_sustained_20ms_concurrency.copied(), Some(0));
        assert_eq!(out.conc_at_sustained.copied(), Some(0));
        assert_eq!(
            out.rps_sustained_20ms_mock_bound,
            Some(false),
            "0 cannot be rig-bound: the rig was never asked to do anything"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    // NOT COVERED HERE BY DESIGN: an end-to-end suite test driving the whole pipeline with the
    // gateway and mock pointed at the same server, asserting suppression IF
    // `rps_max_proxy_mock_bound == Some(true)`, would be vacuous in practice - `cfg_for`'s fixture
    // (sweep_duration_s: 1, max_conc: 2) is too tight for `search::saturation_plateau` to complete even one
    // probe before its own search deadline interrupts it, so the search always returns zero sweep
    // points and `mock_bound` is always `None`, never `Some(true)`, and the guarded assertion body
    // would never run. The behavior ("a value at the rig ceiling must not be published, and its
    // concurrency must be suppressed with it") is already covered directly and
    // deterministically by `a_suppressed_peak_leaves_no_concurrency_behind` above, which drives
    // `apply_peak_verdict` with a reference equal to the observation, and by rigbound.rs's
    // `at_or_above_nine_tenths_of_the_ceiling_is_rig_bound` / `exactly_at_the_fraction_counts_as_bound`,
    // which cover the 90% threshold itself. Making the end-to-end version actually reach `Some(true)`
    // would mean growing the fixture's sweep duration until a real search completes, which trades a
    // fast, deterministic unit test for a slow one whose pass/fail would still depend on two live
    // measurement passes against the same loopback server landing within 10% of each other - a real
    // flakiness risk for no additional coverage over what the two pure-function suites above already
    // give.

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
        assert_eq!(out.streams_sustained_mock_bound, Some(false));
    }

    #[test]
    fn a_stream_rate_that_matches_the_paced_mock_is_published_with_the_flag() {
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
        assert_eq!(
            out.streams_sustained_mock_bound,
            Some(true),
            "flagged as matching the paced upstream"
        );
    }

    #[test]
    fn an_unusable_stream_reference_is_unknown_never_a_guessed_false() {
        let mut out = crate::record::CellStream::default();
        apply_streams_sustained_verdict(
            &mut out,
            12_400.0,
            256,
            Measurement::absent(Absent::NotMeasured),
        );
        assert_eq!(out.streams_sustained_fps.copied(), None);
        assert_eq!(
            out.streams_sustained_mock_bound, None,
            "an unmeasurable mock reference must not be guessed as gateway-bound"
        );
    }

    #[test]
    fn a_measured_cpu_fps_peak_publishes_the_concurrency_it_peaked_at() {
        let mut out = crate::record::CellStream::default();
        apply_cpu_fps_verdict(&mut out, 169_125.0, 1024, Measurement::Measured(351_088.0));
        assert_eq!(out.cpu_fps.copied(), Some(169_125.0));
        assert_eq!(out.cpu_fps_concurrency.copied(), Some(1024));
        assert_eq!(out.cpu_fps_mock_bound, Some(false));
    }

    // The field case from rigbound.rs's own tests, in the lane it came from: 334838 fps against a
    // 351088 fps mock ceiling is 95.4% and says nothing about the gateway.
    #[test]
    fn a_cpu_fps_peak_that_matches_the_paced_mock_is_published_with_the_flag() {
        let mut out = crate::record::CellStream::default();
        // The mock paces its deltas, so its frames/sec is a TARGET, not a capacity it ran out of.
        // Reaching it is the gateway forwarding every frame as it arrives - the best outcome there
        // is - and this used to delete the number for it.
        apply_cpu_fps_verdict(&mut out, 334_838.0, 1024, Measurement::Measured(351_088.0));
        assert_eq!(
            out.cpu_fps.copied(),
            Some(334_838.0),
            "keeping pace is a measurement, not a rig limit"
        );
        assert_eq!(
            out.cpu_fps_concurrency.copied(),
            Some(1024),
            "and it has an operating point"
        );
        assert_eq!(
            out.cpu_fps_mock_bound,
            Some(true),
            "the flag stays as the signal it always was"
        );
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
            out.streams_sustained_mock_bound,
            Some(false),
            "0 cannot be mock-bound: the mock was never asked to carry anything"
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
    fn the_stream_c1_note_says_how_many_frames_each_single_stream_produced() {
        let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> =
            std::collections::BTreeMap::new();
        metrics.insert("gateway_c1_frames", Measurement::Measured(64.0));
        metrics.insert("direct_c1_frames", Measurement::Measured(64.0));
        let note = stream_c1_note(&metrics).unwrap_or_default();
        assert!(note.contains("64 frame(s) through the gateway"), "{note}");
        assert!(
            note.contains("99th percentile"),
            "the note must say why the p99 fields are absent: {note}"
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
        // What the field had: nothing on disk, and nothing handed over.
        std::env::remove_var("OTB_QUALIFY_BASELINE");
        assert!(
            qualify_history(empty).is_empty(),
            "with no history the baseline is absent and the qualification can only seed"
        );
        let (outcome, drift) = crate::qualify::judge(
            Measurement::Measured(500_000.0),
            crate::qualify::rolling_baseline(qualify_history(empty)),
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
        std::env::set_var("OTB_QUALIFY_BASELINE", "497862");
        assert_eq!(qualify_history(empty), vec![497_862.0]);

        // A healthy box - the widest real deviation across the 2026-07-28 field was 4.4% - passes.
        let judge_at = |rps: f64| {
            crate::qualify::judge(
                Measurement::Measured(rps),
                crate::qualify::rolling_baseline(qualify_history(empty)),
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

        std::env::remove_var("OTB_QUALIFY_BASELINE");
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

    // A REFUTED CELL PUBLISHES NO NUMBERS UNDER A NAME THE WIRE DID NOT EARN.
    //
    // `perf_dropped` was documented as the mechanism that withholds perf and stream when leg-3
    // re-verification proves a misroute, and no writer ever set it: a cell whose request demonstrably
    // never reached the mock as its own egress dialect still published a full throughput and
    // streaming block under `<ingress>-><egress>`, which is a translation claim the gateway did not
    // earn - the exact false positive reverify.rs exists to prevent, arriving by another door.
    #[test]
    fn a_refuted_reverification_withholds_the_numbers_and_keeps_the_evidence() {
        let mut perf = empty_perf();
        perf.rps_max_proxy = Measurement::Measured(46_863);
        perf.rps_max_proxy_concurrency = Measurement::Measured(116);
        perf.sweep_max_proxy = vec![crate::record::SweepPoint {
            conc: 116,
            rps: Measurement::Measured(46_863),
            p99_us: Measurement::Measured(900),
            fail: Measurement::Measured(0),
        }];
        perf.egress_reverified = Some(false);
        perf.reverify_note = Some("the request arrived on the mock's anthropic endpoint".into());
        let stream = crate::record::CellStream {
            stream_served: crate::record::StreamServed::Bool(true),
            added_ttft_p50_us: Measurement::Measured(1_200),
            ..Default::default()
        };
        let refuted = crate::reverify::Reverified {
            verified: Some(false),
            note: Some("the request arrived on the mock's anthropic endpoint".into()),
        };

        let (perf, stream, dropped) =
            withhold_if_refuted(&refuted, Some(perf), Some(stream), "anthropic", "openai");

        let perf = perf.expect("the block stays: it carries the evidence");
        assert_eq!(
            perf.rps_max_proxy.copied(),
            None,
            "a peak measured over a misrouted wire must not publish under this cell's name"
        );
        assert_eq!(perf.rps_max_proxy_concurrency.copied(), None);
        assert!(
            perf.sweep_max_proxy.is_empty(),
            "the rungs are numbers on that same wire and go with it"
        );
        assert_eq!(
            perf.rps_max_proxy.reason(),
            Some(&Absent::NotServed),
            "the withholding is a finding about the gateway, not about the rig"
        );
        assert!(perf
            .rps_max_proxy
            .detail()
            .unwrap_or_default()
            .contains("anthropic endpoint"));
        // THE STREAM BLOCK IS WITHHELD, NOT DELETED - the same rule as the perf half, and it used to
        // be the opposite. Setting it to None took the evidence with it, so a refuted cell read as
        // one that was never probed rather than one whose numbers were withheld.
        let stream = stream.expect("the stream block stays: it carries its own evidence");
        assert_eq!(
            stream.added_ttft_p50_us.copied(),
            None,
            "a TTFT measured over a misrouted wire must not publish under this cell's name"
        );
        assert_eq!(
            stream.added_ttft_p50_us.reason(),
            Some(&Absent::NotServed),
            "and it is withheld for the same reason the perf numbers are"
        );
        assert!(stream
            .added_ttft_p50_us
            .detail()
            .unwrap_or_default()
            .contains("anthropic endpoint"));
        assert!(
            stream.sweep_streams.is_empty() && stream.sweep_cpu_fps.is_empty(),
            "the stream rungs are numbers on that same wire and go with it"
        );
        assert_eq!(
            stream.stream_served,
            crate::record::StreamServed::Bool(true),
            "whether it streamed at all is evidence, not a measurement, and must survive"
        );
        assert_eq!(
            perf.egress_reverified,
            Some(false),
            "the evidence that refuted it must survive"
        );
        assert!(perf.reverify_note.is_some());
        let dropped = dropped.expect("perf_dropped is the marker a consumer branches on");
        assert!(
            dropped.contains("anthropic>openai"),
            "the marker must name the pairing whose claim was withheld, got {dropped:?}"
        );
    }

    // THE CALL SITE, not the guard. The three tests around this one drive `withhold_if_refuted`
    // directly and pin its behaviour thoroughly - and every one of them stayed green while the
    // question "is it ever called on a real cell?" went unasked. The call lived inline in
    // `run_suite_with`, which needs a gateway process, a mock and a socket to reach, so nothing
    // could reach it: deleting the line published a PROVEN MISROUTE's numbers as an unqualified
    // translation claim with the whole suite passing. This drives the composition instead.
    #[test]
    fn a_refuted_cell_is_withheld_by_the_assembly_not_only_by_the_guard() {
        let dir = tmpdir("assemble-withhold");
        let cfg = cfg_for(&dir, "127.0.0.1:1".parse().expect("addr"));
        let mut metrics = std::collections::BTreeMap::new();
        metrics.insert("rps_max_proxy", Measurement::Measured(1_200.0));
        metrics.insert("conc_at_peak", Measurement::Measured(64.0));
        // The assertions below are about the WITHHOLDING DECISION, not about any published rate.
        // This fixture points the gateway and the mock at the same address, so every peak comes back
        // rig-bound and absent either way - asserting on a number here would pass identically with
        // the withholding deleted, which is the exact defect this test exists to rule out. What
        // separates the two cases is the marker, the stream block, and the surviving evidence.
        let refuted = crate::run::CellResult {
            outcome: crate::cell::CellOutcome::served(crate::cell::CellId {
                ingress: "anthropic".into(),
                egress: "openai".into(),
            }),
            metrics: Some(metrics),
            series: None,
            timings_s: None,
            reverify: crate::reverify::Reverified {
                verified: Some(false),
                note: Some("the mock received an anthropic request on the openai leg".into()),
            },
        };

        let (perf, stream, dropped) =
            assemble_cell_measurements(&cfg, &refuted, "anthropic", "openai");
        let perf = perf.expect("the evidence must survive - withholding is not deletion");
        assert_eq!(
            perf.egress_reverified,
            Some(false),
            "the proof that refuted the cell must reach the artifact"
        );
        let marker = dropped.expect("the assembly must publish the marker a consumer branches on");
        assert!(
            marker.contains("anthropic>openai"),
            "the marker names the pairing whose claim was withheld, got {marker:?}"
        );
        assert!(
            stream.is_some(),
            "a refuted cell keeps its stream block, withheld - deleting it would erase the evidence"
        );

        // The mirror, so this is not "the assembly always drops" passing for the wrong reason: an
        // UNCHECKED re-verification (diagonal cell, mock not recording, mock unreachable) is not a
        // refutation, and withholding on it would let our own configuration erase real measurements.
        let unchecked = crate::run::CellResult {
            reverify: crate::reverify::Reverified {
                verified: None,
                note: None,
            },
            ..refuted
        };
        let (perf, _stream, dropped) =
            assemble_cell_measurements(&cfg, &unchecked, "anthropic", "openai");
        assert!(
            dropped.is_none(),
            "an unchecked cell withholds nothing, got {dropped:?}"
        );
        assert_eq!(
            perf.expect("perf survives").egress_reverified,
            None,
            "and it must not be recorded as having been checked"
        );
    }

    // The other half, and the reason this is not "drop whenever it is not proven": `None` is NOT
    // CHECKED (a diagonal cell, a mock that was not recording, a mock we could not reach), and
    // withholding on it would let our own configuration erase a gateway's real measurements.
    #[test]
    fn an_unchecked_reverification_withholds_nothing() {
        let mut perf = empty_perf();
        perf.rps_max_proxy = Measurement::Measured(46_863);
        for verified in [None, Some(true)] {
            let r = crate::reverify::Reverified {
                verified,
                note: Some("openai>openai is a same-dialect cell".into()),
            };
            let (kept, stream, dropped) = withhold_if_refuted(
                &r,
                Some(perf.clone()),
                Some(crate::record::CellStream::default()),
                "openai",
                "openai",
            );
            assert_eq!(
                kept.and_then(|p| p.rps_max_proxy.copied()),
                Some(46_863),
                "an unproven egress still publishes what was really measured"
            );
            assert!(stream.is_some());
            assert_eq!(
                dropped, None,
                "nothing was withheld, so nothing says it was"
            );
        }
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

    // A SUPPRESSED NUMBER'S TWINS CARRY ITS EVIDENCE, NOT JUST ITS LABEL. The companion fields were
    // built with `Measurement::absent(reason)`, so a reader who opened the absences map at
    // `perf.conc_at_peak` got "rig_limited" with no sign of the ceiling that caused it, while the
    // primary two keys away carried the whole story.
    #[test]
    fn a_suppressed_peaks_twins_carry_the_same_evidence_the_peak_does() {
        let mut out = empty_perf();
        apply_peak_verdict(&mut out, 46_863.0, 116, Measurement::Measured(46_863.0));
        let detail = out
            .rps_max_proxy
            .detail()
            .expect("the suppressed peak states what bounded it")
            .to_string();
        assert!(detail.contains("rig ceiling"));
        assert_eq!(
            out.rps_max_proxy_concurrency.detail(),
            Some(detail.as_str()),
            "the twin must carry the same evidence, not a bare reason"
        );
        assert_eq!(out.conc_at_peak.detail(), Some(detail.as_str()));
    }

    // AND THE TWINS MUST CARRY THE SAME REASON. `conc_at_sustained` was assigned twice in the
    // unknown-verdict branch, and the second assignment overwrote the verdict's own reason with a
    // flat `RigLimited` - so a cell whose rig reference was unmeasurable published "the rig bounded
    // this" on one field beside "we could not tell" on the two next to it.
    #[test]
    fn an_unknown_sustained_verdict_does_not_relabel_its_concurrency_as_rig_bound() {
        let mut out = empty_perf();
        apply_sustained_verdict(
            &mut out,
            11_968.0,
            1024,
            Measurement::absent(Absent::NotMeasured),
        );
        assert_eq!(out.rps_sustained_20ms.reason(), Some(&Absent::NotMeasured));
        assert_eq!(
            out.conc_at_sustained.reason(),
            Some(&Absent::NotMeasured),
            "the concurrency must carry the verdict's reason, not a relabelled rig limit"
        );
        assert_eq!(
            out.conc_at_sustained.detail(),
            out.rps_sustained_20ms.detail(),
            "one absence, one story, at every key that carries it"
        );
        assert_eq!(
            out.rps_sustained_20ms_concurrency.reason(),
            Some(&Absent::NotMeasured)
        );
    }
}
