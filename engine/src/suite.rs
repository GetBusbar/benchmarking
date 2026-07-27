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
        runtime: crate::manifest::Runtime::Native { proc_match: String::new() },
        // The reference drives the MOCK directly. There is no gateway process behind it, so there is
        // nothing to restart, and a spec here would let a reference measurement bounce the gateway.
        relaunch: None,
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
        Some(m) => match m.value() {
            Some(v) => Measurement::Measured(*v as i64),
            None => match (m.reason().cloned(), m.detail()) {
                (Some(r), Some(d)) => Measurement::absent_because(r, d),
                (Some(r), None) => Measurement::absent(r),
                (None, _) => Measurement::absent(Absent::NotMeasured),
            },
        },
        None => Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field"),
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
    let missing = || Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field");
    let rps = metrics.get("rps_max_proxy").cloned().unwrap_or_else(missing);
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
            let absent = match (rps.reason().cloned(), rps.detail()) {
                (Some(r), Some(d)) => Measurement::absent_because(r, d),
                (Some(r), None) => Measurement::absent(r),
                (None, _) => Measurement::absent(Absent::NotMeasured),
            };
            // The concurrency travels WITH the peak, present or absent. Leaving it at empty_perf()'s
            // default published a different reason for the two halves of one fact.
            out.rps_max_proxy_concurrency = match absent.reason().cloned() {
                Some(r) => Measurement::absent(r),
                None => Measurement::absent(Absent::NotMeasured),
            };
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
    run::stream_fps_at(cfg.mock_addr, &cfg.manifest.model, &cfg.manifest.auth, dialect, at_conc)
}

/// Fill the added-latency fields straight from the metric surface.
///
/// Unlike the peak's `rps_max_proxy`, there is no separate rig-bound verdict to compute here: the
/// group's own two-leg comparison (gateway leg minus a direct-to-mock leg, both taken at c=1) already
/// IS the rig correction, the same way `Streaming::measure`'s `added_ttft`/`added_gap` need no second
/// rig judgement layered on top. So this is a plain "take", exactly the pattern `cell_memory` and
/// `cell_stream` already use for fields with no rig-bound question to ask.
fn judge_added_latency(out: &mut CellPerf, metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>) {
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
    let missing = || Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field");
    let rps = metrics.get("rps_sustained_20ms").cloned().unwrap_or_else(missing);
    let conc_m = metrics.get("rps_sustained_20ms_concurrency").cloned().unwrap_or_else(missing);

    let (Some(&value), Some(&conc_f)) = (rps.value(), conc_m.value()) else {
        let absent = match (rps.reason().cloned(), rps.detail()) {
            (Some(r), Some(d)) => Measurement::absent_because(r, d),
            (Some(r), None) => Measurement::absent(r),
            (None, _) => Measurement::absent(Absent::NotMeasured),
        };
        out.rps_sustained_20ms_concurrency = match absent.reason().cloned() {
            Some(r) => Measurement::absent(r),
            None => Measurement::absent(Absent::NotMeasured),
        };
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
    match rigbound::is_rig_bound(value, reference.clone()).copied() {
        Some(true) => {
            let detail = match reference.copied() {
                Some(r) => format!("sustained {value:.0} against a rig ceiling of {r:.0} at c={conc}"),
                None => format!("sustained {value:.0} at c={conc} with an unusable rig reference"),
            };
            out.rps_sustained_20ms = Measurement::absent_because(Absent::RigLimited, detail);
            out.rps_sustained_20ms_mock_bound = Some(true);
            out.rps_sustained_20ms_concurrency = Measurement::absent(Absent::RigLimited);
            out.conc_at_sustained = Measurement::absent(Absent::RigLimited);
        }
        Some(false) => {
            out.rps_sustained_20ms = Measurement::Measured(value as i64);
            out.rps_sustained_20ms_mock_bound = Some(false);
            out.rps_sustained_20ms_concurrency = Measurement::Measured(i64::from(conc));
            out.conc_at_sustained = Measurement::Measured(i64::from(conc));
        }
        None => {
            out.rps_sustained_20ms = Measurement::absent_because(
                Absent::RigLimited,
                format!(
                    "sustained {value:.0} at c={conc}, but the rig reference could not be measured, so it is unknown whether the gateway or the rig set this"
                ),
            );
            out.rps_sustained_20ms_mock_bound = None;
            out.rps_sustained_20ms_concurrency = Measurement::absent(Absent::RigLimited);
            out.conc_at_sustained = Measurement::absent(Absent::RigLimited);
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
    match rigbound::is_rig_bound(value, reference.clone()).copied() {
        // Bounded by our own rig: this says nothing about the gateway, so it must not rank, win a
        // comparison, or draw a bar. Two fast gateways both pinned here would otherwise read as a
        // tie they did not earn.
        Some(true) => {
            let detail = match reference.copied() {
                Some(r) => format!("reached {value:.0} against a rig ceiling of {r:.0} at c={conc}"),
                None => format!("reached {value:.0} at c={conc} with an unusable rig reference"),
            };
            out.rps_max_proxy = Measurement::absent_because(Absent::RigLimited, detail);
            out.rps_max_proxy_mock_bound = Some(true);
            // Suppressed WITH its peak: a concurrency left behind would be the operating point of a
            // number the board is refusing to publish.
            out.rps_max_proxy_concurrency = Measurement::absent(Absent::RigLimited);
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
            out.rps_max_proxy = Measurement::absent_because(
                Absent::RigLimited,
                format!("reached {value:.0} at c={conc}, but the rig reference could not be measured, so it is unknown whether the gateway or the rig set this"),
            );
            out.rps_max_proxy_mock_bound = None;
            out.rps_max_proxy_concurrency = Measurement::absent(Absent::RigLimited);
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
        metrics
            .get(k)
            .cloned()
            .unwrap_or_else(|| Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field"))
    };
    let rss_series: Vec<crate::record::RssSample> =
        series.map(|s| s.rss.clone()).unwrap_or_default();
    // STEADY STATE, derived from the readings rather than left null. The trailing part of the window
    // is where the process has stopped growing, so its median is what the gateway actually costs
    // under sustained load, as distinct from the peak, which one spike can set. Absent when there
    // are too few readings to have a trailing part at all, because a "steady state" computed from
    // one sample would be the sample.
    let steady = steady_state(&rss_series);
    // The plateau verdict travels as a number across the metric surface (every group speaks f64), so
    // it is turned back into the tri-state it really is here: settled, did not settle, or could not
    // be judged. An absent verdict must stay absent rather than collapsing to false, because "it did
    // not settle" is a claim about the gateway and "we could not tell" is a claim about the window.
    let plateaued = take("memory_plateaued").copied().map(|v| v != 0.0);
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
        load_s: take("memory_load_s").copied().map(|v| v as i64),
        plateaued,
        recovery_window_s: Some(crate::metric::MEMORY_RECOVERY_S as i64),
        steady_state_rss_mib: steady,
        rss_series,
        ..Default::default()
    }
}

/// The median of the trailing half of the window's readings.
///
/// Median, not mean: one allocator spike at the end of a window would drag a mean and misreport the
/// level the process settled at. Trailing half, because the start of the window is the ramp, and
/// including the ramp measures how fast it grew rather than where it stopped.
fn steady_state(series: &[crate::record::RssSample]) -> Measurement<f64> {
    let mut tail: Vec<f64> = series
        .iter()
        .skip(series.len() / 2)
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
    let median =
        if tail.len().is_multiple_of(2) { (tail[mid - 1] + tail[mid]) / 2.0 } else { tail[mid] };
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

    let ttft = metrics.get("added_ttft_p50_us");
    let (stream_served, reason) = match ttft.map(|m| (m.is_measured(), m.reason().cloned(), m.detail())) {
        Some((true, _, _)) => (crate::record::StreamServed::Bool(true), None),
        // Probed, and the answer was not a number. The reason travels as the status so a reader is
        // never left to infer a gateway property from a rig limit.
        Some((false, Some(r), detail)) => (
            crate::record::StreamServed::Status(r.token().to_string()),
            detail.map(str::to_string),
        ),
        _ => (crate::record::StreamServed::default(), None),
    };

    let mut out = crate::record::CellStream {
        stream_served,
        reason,
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
    let missing = || Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field");
    let fps = metrics.get("streams_sustained_fps").cloned().unwrap_or_else(missing);
    let conc_m = metrics.get("streams_sustained").cloned().unwrap_or_else(missing);

    let (Some(&value), Some(&conc_f)) = (fps.value(), conc_m.value()) else {
        let absent = carry_absence(&fps);
        out.streams_sustained = match absent.reason().cloned() {
            Some(r) => Measurement::absent(r),
            None => Measurement::absent(Absent::NotMeasured),
        };
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
    let missing = || Measurement::absent_because(Absent::NotMeasured, "no metric group fills this field");
    let fps = metrics.get("cpu_fps").cloned().unwrap_or_else(missing);
    let conc_m = metrics.get("cpu_fps_concurrency").cloned().unwrap_or_else(missing);

    let (Some(&value), Some(&conc_f)) = (fps.value(), conc_m.value()) else {
        let absent = carry_absence(&fps);
        out.cpu_fps_concurrency = match absent.reason().cloned() {
            Some(r) => Measurement::absent(r),
            None => Measurement::absent(Absent::NotMeasured),
        };
        out.cpu_fps = absent;
        return;
    };
    let conc = conc_f as u32;
    apply_cpu_fps_verdict(out, value, conc, stream_rig_ceiling(cfg, dialect, conc));
}

/// Re-wrap an absence so its reason AND its detail survive a narrowing. The searches attach their
/// lower bound as prose, and flattening that to a bare null is the one place "the engine discards the
/// measurement" was literally true.
fn carry_absence<T>(m: &Measurement<f64>) -> Measurement<T> {
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
    match rigbound::is_rig_bound(value, reference.clone()).copied() {
        Some(true) => {
            let detail = match reference.copied() {
                Some(r) => format!("carried {value:.0} frames/sec against a mock ceiling of {r:.0} at c={conc}"),
                None => format!("carried {value:.0} frames/sec at c={conc} with an unusable mock reference"),
            };
            out.streams_sustained_fps = Measurement::absent_because(Absent::RigLimited, detail);
            out.streams_sustained_mock_bound = Some(true);
            // Suppressed WITH its rate: a stream count left behind would be the operating point of a
            // number the board is refusing to publish.
            out.streams_sustained = Measurement::absent(Absent::RigLimited);
        }
        Some(false) => {
            out.streams_sustained_fps = Measurement::Measured(value);
            out.streams_sustained_mock_bound = Some(false);
            out.streams_sustained = Measurement::Measured(i64::from(conc));
        }
        None => {
            out.streams_sustained_fps = Measurement::absent_because(
                Absent::RigLimited,
                format!(
                    "carried {value:.0} frames/sec at c={conc}, but the mock reference could not be measured, so it is unknown whether the gateway or the rig set this"
                ),
            );
            out.streams_sustained_mock_bound = None;
            out.streams_sustained = Measurement::absent(Absent::RigLimited);
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
    match rigbound::is_rig_bound(value, reference.clone()).copied() {
        Some(true) => {
            let detail = match reference.copied() {
                Some(r) => format!("peaked at {value:.0} frames/sec against a mock ceiling of {r:.0} at c={conc}"),
                None => format!("peaked at {value:.0} frames/sec at c={conc} with an unusable mock reference"),
            };
            out.cpu_fps = Measurement::absent_because(Absent::RigLimited, detail);
            out.cpu_fps_mock_bound = Some(true);
            out.cpu_fps_concurrency = Measurement::absent(Absent::RigLimited);
        }
        Some(false) => {
            out.cpu_fps = Measurement::Measured(value);
            out.cpu_fps_mock_bound = Some(false);
            out.cpu_fps_concurrency = Measurement::Measured(i64::from(conc));
        }
        None => {
            out.cpu_fps = Measurement::absent_because(
                Absent::RigLimited,
                format!(
                    "peaked at {value:.0} frames/sec at c={conc}, but the mock reference could not be measured, so it is unknown whether the gateway or the rig set this"
                ),
            );
            out.cpu_fps_mock_bound = None;
            out.cpu_fps_concurrency = Measurement::absent(Absent::RigLimited);
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
        auth: cfg.manifest.auth.clone(),
        dialects: vec![Dialect::Openai],
        sweep_duration_s: cfg.sweep_duration_s,
        probe_timeout: Duration::from_secs(10),
        load_cores: cfg.load_cores.clone(),
        // No gateway is in this path at all, so there is no gateway process to attribute anything to.
        static_headers: Vec::new(),
        egress_headers: Default::default(),
        runtime: crate::manifest::Runtime::Native { proc_match: String::new() },
        // The reference drives the MOCK directly. There is no gateway process behind it, so there is
        // nothing to restart, and a spec here would let a reference measurement bounce the gateway.
        relaunch: None,
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
    let (outcome, drift) =
        crate::qualify::judge(observed.clone(), baseline.clone(), QUALIFY_BAND_PCT, crate::qualify::Sense::HigherIsBetter);

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

/// Every previous box-qualification observation this results directory holds.
///
/// Read from the historical snapshots rather than a side file, so the baseline cannot drift from the
/// runs that produced it: a snapshot IS the record. An unreadable or old-shaped file contributes
/// nothing rather than a zero, which would drag the median toward a value no run ever observed.
fn qualify_history(results_dir: &Path) -> Vec<f64> {
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
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if let Some(rps) = value.pointer("/rig/box_qualify/observed_rps").and_then(serde_json::Value::as_f64) {
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
    let rc = RunConfig {
        gateway_addr,
        mock_addr: cfg.mock_addr,
        model: cfg.manifest.model.clone(),
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
        cell_paths: cfg.manifest.cell_paths.clone(),
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
                let mut h = cfg.manifest.headers_for(d.as_str(), &cfg.gw_cores, cfg.mock_addr.port(), &cfg.gw_dir).ok()?;
                // headers_for prepends the always-on set; the run config carries those separately, so
                // strip them here rather than sending each one twice.
                let statics = cfg.manifest.headers_for("", &cfg.gw_cores, cfg.mock_addr.port(), &cfg.gw_dir).ok()?;
                h.retain(|x| !statics.contains(x));
                Some((d.as_str().to_string(), h))
            })
            .collect(),
        runtime: cfg.manifest.runtime.clone(),
    };

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
    // is worth more than a complete result that might not arrive, and the promote guard already
    // refuses to let a thinner snapshot overwrite a fuller one, so re-writing is safe by
    // construction rather than by care here.
    for result in run::run_grid_with(&rc, cfg.min_conc, cfg.max_conc, metrics) {
        let id = &result.outcome.id;
        let ing = id.ingress.clone();
        let eg = id.egress.clone();

        if last_egress.as_deref() != Some(eg.as_str()) {
            if last_egress.is_some() {
                written = Some(flush(cfg, &upstreams, any_served, Some(box_qualify.clone()))?);
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
        };
        if matches!(served, RecServed::Bool(true)) {
            any_served = true;
        }

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
                // whose egress dialect was NOT proven still carries its measurements - they are real
                // observations of what the gateway did - but it can no longer be read as an
                // unqualified translation claim, because the flag beside them says so and the note
                // names what the mock actually received instead. See `reverify.rs` for why `None`
                // (diagonal cell, recording off, mock unreachable) is a first-class third answer
                // rather than a failure.
                p.egress_reverified = result.reverify.verified;
                p.reverify_note = result.reverify.note.clone();
                Some(p)
            }
            _ => None,
        };

        let cell = Cell {
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
            memory: result.metrics.as_ref().map(|m| cell_memory(m, result.series.as_ref())),
            stream: match (&result.metrics, ing.parse::<Dialect>()) {
                (Some(m), Ok(d)) => Some(cell_stream(cfg, d, m, result.series.as_ref())),
                _ => None,
            },
            ..Default::default()
        };

        upstreams
            .entry(eg)
            .or_insert_with(|| Upstream { configurable: true, served: true, ..Default::default() })
            .cells
            .insert(ing, cell);
    }

    // The final write always happens, so a grid with a single egress column is not lost.
    let _ = written;
    flush(cfg, &upstreams, any_served, Some(box_qualify))
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
    let snap = ResultSnapshot {
        schema_version: 1,
        gateway: cfg.manifest.name.clone(),
        // WHAT WAS MEASURED, not what measured it: the engine identifies itself in rig.engine,
        // where it belongs, so this field must be the gateway's own build string, not the engine's,
        // or every gateway's artifact would claim the same build and a reader could not tell which
        // image produced a number. Falls back to the engine string only when the manifest declares
        // no launch, i.e. when the harness did not start the thing it measured and genuinely does
        // not know its build.
        build: gateway_build(cfg)
            .unwrap_or_else(|| format!("otb-engine {}", env!("CARGO_PKG_VERSION"))),
        measured_at: cfg.measured_at.clone(),
        arch: Some(cfg.arch.clone()),
        rig: rig.clone(),
        matrix: Matrix {
            gateway: cfg.manifest.name.clone(),
            served: any_served,
            cell_perf_sweep: true,
            upstreams: upstreams.clone(),
            // Mirrored onto the matrix as well as the snapshot root: record.rs carries the field in
            // both places, and a reader that finds it in one and not the other cannot tell which is
            // authoritative.
            rig,
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
            engine_stamp: None,
            rig_mock: None,
            rig_release_url: None,
            load_cores: None,
        }
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
        assert!(paths.historical.exists(), "the timestamped copy must land too");
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
        cfg.engine_stamp = Some(crate::record::EngineStamp { commit: "deadbeef".into(), dirty: true });
        let up = HashMap::new();
        let paths = flush(&cfg, &up, false, None).expect("the snapshot should write");
        let text = std::fs::read_to_string(&paths.current).expect("current file");
        let back: ResultSnapshot = serde_json::from_str(&text).expect("its own output must parse");
        let rig = back.rig.expect("a run with a commit must carry a rig block");
        let eng = rig.engine.expect("rig.engine must survive with no box qualification beside it");
        assert_eq!(eng.commit, "deadbeef");
        assert!(eng.dirty, "a dirty tree must be published as dirty, not quietly cleaned");
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
        let up = back.matrix.upstreams.get("openai").expect("the egress row exists");
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
        assert_eq!(out.rps_max_proxy.copied(), Some(46_863), "a gateway-bound peak is published");
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
        assert_eq!(out.rps_max_proxy.copied(), None, "a rig-bound peak is suppressed");
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
        assert_eq!(out.rps_sustained_20ms.copied(), None, "a rig-bound sustained rate is suppressed");
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
        apply_sustained_verdict(&mut out, 11_968.0, 1024, Measurement::absent(Absent::NotMeasured));
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
        let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> = std::collections::BTreeMap::new();
        metrics.insert("rps_sustained_20ms", Measurement::Measured(0.0));
        metrics.insert("rps_sustained_20ms_concurrency", Measurement::Measured(0.0));
        judge_sustained(&cfg, Dialect::Openai, &mut out, &metrics);
        assert_eq!(out.rps_sustained_20ms.copied(), Some(0));
        assert_eq!(out.rps_sustained_20ms_concurrency.copied(), Some(0));
        assert_eq!(out.conc_at_sustained.copied(), Some(0));
        assert_eq!(out.rps_sustained_20ms_mock_bound, Some(false), "0 cannot be rig-bound: the rig was never asked to do anything");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── judge_added_latency: a plain take, straight off the metric surface ─────────────────────────

    #[test]
    fn added_latency_fields_are_taken_straight_from_the_metric_surface() {
        let mut out = empty_perf();
        let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> = std::collections::BTreeMap::new();
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
        let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> = std::collections::BTreeMap::new();
        metrics.insert(
            "added_latency_p99_us",
            Measurement::absent_because(Absent::NotMeasured, "the gateway leg at c=1 was not clean: 0 ok, 4 fail"),
        );
        judge_added_latency(&mut out, &metrics);
        assert_eq!(out.added_latency_p99_us.copied(), None);
        assert!(out.added_latency_p99_us.detail().unwrap_or_default().contains("not clean"));
        // gateway_c1_p99_us was never inserted into the map at all: still a key, still an absence.
        assert_eq!(out.gateway_c1_p99_us.copied(), None);
    }

    // NOT COVERED HERE BY DESIGN: an end-to-end suite test driving the whole pipeline with the
    // gateway and mock pointed at the same server, asserting suppression IF
    // `rps_max_proxy_mock_bound == Some(true)`, would be vacuous in practice - `cfg_for`'s fixture
    // (sweep_duration_s: 1, max_conc: 2) is too tight for `search::peak_max` to complete even one
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
    fn a_mock_bound_stream_ceiling_is_suppressed_with_its_stream_count() {
        let mut out = crate::record::CellStream::default();
        // Reference equal to the observation: the mock, not the gateway, set this.
        apply_streams_sustained_verdict(&mut out, 12_400.0, 256, Measurement::Measured(12_400.0));
        assert_eq!(out.streams_sustained_fps.copied(), None, "a mock-bound rate is suppressed");
        assert_eq!(out.streams_sustained.copied(), None, "its operating point must be suppressed with it");
        assert_eq!(out.streams_sustained_mock_bound, Some(true));
        assert_eq!(out.streams_sustained_fps.reason(), Some(&Absent::RigLimited));
    }

    #[test]
    fn an_unusable_stream_reference_is_unknown_never_a_guessed_false() {
        let mut out = crate::record::CellStream::default();
        apply_streams_sustained_verdict(&mut out, 12_400.0, 256, Measurement::absent(Absent::NotMeasured));
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
    fn a_mock_bound_cpu_fps_peak_is_suppressed_with_its_concurrency() {
        let mut out = crate::record::CellStream::default();
        apply_cpu_fps_verdict(&mut out, 334_838.0, 1024, Measurement::Measured(351_088.0));
        assert_eq!(out.cpu_fps.copied(), None);
        assert_eq!(out.cpu_fps_concurrency.copied(), None);
        assert_eq!(out.cpu_fps_mock_bound, Some(true));
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
        let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> = std::collections::BTreeMap::new();
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
        let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> = std::collections::BTreeMap::new();
        metrics.insert(
            "streams_sustained_fps",
            Measurement::absent_because(Absent::SearchExhausted, "c=65536 still passes at the top of the search range"),
        );
        metrics.insert("streams_sustained", Measurement::absent(Absent::SearchExhausted));
        judge_streams_sustained(&cfg, Dialect::Openai, &mut out, &metrics);
        assert_eq!(out.streams_sustained_fps.reason(), Some(&Absent::SearchExhausted));
        assert!(out.streams_sustained_fps.detail().unwrap_or_default().contains("65536"));
        assert_eq!(out.streams_sustained.reason(), Some(&Absent::SearchExhausted));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── the two advisory notes ───────────────────────────────────────────────────────────────────

    // A p99 over four thousand round trips and a p99 over eleven are the same field carrying utterly
    // different weight, and nothing else in the artifact says which one a reader is holding.
    #[test]
    fn the_c1_note_says_how_many_round_trips_each_percentile_was_taken_over() {
        let mut out = empty_perf();
        let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> = std::collections::BTreeMap::new();
        metrics.insert("gateway_c1_samples", Measurement::Measured(4_812.0));
        metrics.insert("direct_c1_samples", Measurement::Measured(5_003.0));
        judge_added_latency(&mut out, &metrics);
        let note = out.c1_note.unwrap_or_default();
        assert!(note.contains("4812"), "the gateway leg's own count must appear: {note}");
        assert!(note.contains("5003"), "the direct leg's own count must appear: {note}");
    }

    // No counts means the group never completed a c=1 window, and the added-latency fields are
    // already absent WITH the group's reason. A note restating that would publish one fact twice, in
    // two wordings, which is what a `Measurement`'s reason exists to prevent.
    #[test]
    fn the_c1_note_is_absent_rather_than_prose_about_nothing() {
        let mut out = empty_perf();
        let metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> = std::collections::BTreeMap::new();
        judge_added_latency(&mut out, &metrics);
        assert_eq!(out.c1_note, None);
    }

    #[test]
    fn the_stream_c1_note_says_how_many_frames_each_single_stream_produced() {
        let mut metrics: std::collections::BTreeMap<&'static str, Measurement<f64>> = std::collections::BTreeMap::new();
        metrics.insert("gateway_c1_frames", Measurement::Measured(64.0));
        metrics.insert("direct_c1_frames", Measurement::Measured(64.0));
        let note = stream_c1_note(&metrics).unwrap_or_default();
        assert!(note.contains("64 frame(s) through the gateway"), "{note}");
        assert!(note.contains("99th percentile"), "the note must say why the p99 fields are absent: {note}");

        // A dialect the mock cannot stream took no stream at all: no note, because the absent fields
        // already carry the reason.
        let empty: std::collections::BTreeMap<&'static str, Measurement<f64>> = std::collections::BTreeMap::new();
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
                let Some(perf) = cell.perf.as_ref() else { continue };
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
            diagonal.reverify_note.clone().unwrap_or_default().contains("same-dialect"),
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
        assert_eq!(cell.status, "404", "the observed status must reach the artifact");
        assert!(
            cell.verdict_note.contains("404"),
            "the note must name what was observed, got {:?}",
            cell.verdict_note
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
