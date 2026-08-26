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
    /// What this ran on: instance type, core count, memory, and the gateway/mock/loadgen core
    /// split. Handed in orchestrator-side like `arch`, since the box cannot describe the shape it
    /// was launched with. `None` when never supplied, published as a literal null rather than a guess.
    pub hardware: Option<String>,
    /// Which commit produced this run. Resolved orchestrator-side (the box's clone is a detached
    /// checkout and the engine binary is a download, so neither can self-identify) and handed in
    /// like `arch`, keeping the snapshot writer a pure function of its config.
    ///
    /// `None` publishes as a literal null rather than an omitted key, so "not reproducible" is
    /// distinguishable from "predates provenance".
    pub engine_stamp: Option<crate::record::EngineStamp>,
    /// Which mock binary took the readings. `rig` is a moving release tag, so two runs can use
    /// different binaries behind the same URL; recording it lets a verdict change be attributed to
    /// the instrument rather than the gateway. Resolved orchestrator-side like `engine_stamp`.
    ///
    /// Mock only: the load generator is `otb loadgen`, already identified by `rig.engine.commit`.
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
        // The frontier is filled from the metric group's series (see `cell_perf`), not here: this is
        // the empty shape every field starts absent in.
        frontier: Vec::new(),
        ..Default::default()
    }
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

/// Measure the rig's own stream ceiling on the same cell: the mock's frames/sec at the concurrency
/// the gateway's own stream number was taken at, so the reference matches the operating point
/// instead of understating headroom at the top of the range.
fn stream_rig_ceiling(_cfg: &SuiteConfig, _dialect: Dialect, at_conc: u32) -> Measurement<f64> {
    // Derived, not measured (see `run::mock_frame_ceiling_fps`): we own the mock, so its ceiling is
    // arithmetic (it cannot emit frames faster than it sleeps) rather than a live reference, which
    // would be systematically slower than the gateway leg and understate the gateway.
    let ceiling = run::mock_frame_ceiling_fps(at_conc);
    if ceiling <= 0.0 {
        return Measurement::absent_because(
            Absent::NotMeasured,
            "the mock's declared pacing yields no frame rate to bound against".to_string(),
        );
    }
    Measurement::Measured(ceiling)
}

/// Build a cell's perf block from the metric surface.
///
/// The only judgement left here is added-latency/cost carry-through; throughput is the frontier,
/// read off the sweep's own rungs by the metric group (see `frontier.rs`), so nothing here decides
/// anything about it.
fn judge_cell(
    _cfg: &SuiteConfig,
    _dialect: Dialect,
    metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>,
) -> Judged {
    let mut out = empty_perf();
    judge_added_latency(&mut out, metrics);
    judge_cost(&mut out, metrics);
    Judged { perf: out }
}

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

/// Carry the cost group's fields onto the record with no judgement applied: suppression (failed
/// windows, swap-fault windows) is already decided at the source, where the evidence is.
fn judge_cost(
    out: &mut CellPerf,
    metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>,
) {
    let f = |k: &str| -> Measurement<f64> {
        metrics.get(k).cloned().unwrap_or_else(|| {
            Measurement::absent_because(
                Absent::NotMeasured,
                format!("the cost group published no {k} for this cell"),
            )
        })
    };
    out.cpu_us_per_request = f("cpu_us_per_request");
    out.rps_per_cpu_second = f("rps_per_cpu_second");
    out.cost_window_conc = as_i64(metrics.get("cost_window_conc"));
    out.cost_window_ok = f("cost_window_ok");
    out.cost_window_rps = f("cost_window_rps");
    out.cost_core_utilisation = f("cost_core_utilisation");
    out.cost_threads = f("cost_threads");
    out.cost_nonvol_ctxt_per_request = f("cost_nonvol_ctxt_per_request");
    out.cost_majflt = f("cost_majflt");
}

/// Sample count behind each c=1 leg, since a p99 over four thousand round trips and one over eleven
/// are the same published field carrying very different weight. `None` when either leg produced no
/// count: the added-latency fields are already absent with their own reason in that case.
fn c1_note(metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>) -> Option<String> {
    let gw = metrics.get("gateway_c1_samples")?.copied()?;
    let direct = metrics.get("direct_c1_samples")?.copied()?;
    Some(format!(
        "the c=1 percentiles are taken over {gw:.0} successful gateway round trip(s) and {direct:.0} \
         direct-to-mock round trip(s), each leg a single clean window with no failures"
    ))
}

/// The published per-cell memory window, from the numbers the memory group took.
///
/// This is the sole source `site/gen-data.mjs` reads memory from — no fallback, no per-gateway
/// scalar. Absences travel intact: a window that could not find the gateway's process tree publishes
/// null with the reason naming the identity it looked for, never a zero, since a benchmark that
/// ranks memory ascending would otherwise certify an unmeasured gateway as the winner.
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
    // Median of the trailing part of the window, where the process has stopped growing, as distinct
    // from the peak (which one spike can set). Bounded to the load window's own readings, not the
    // recovery window that follows it in `rss_series` — mixing the two would blend this with
    // `recovered_rss_mib`, a field the record deliberately keeps distinct.
    let load_end = metrics.get("memory_load_s").and_then(|m| m.copied());
    let steady = steady_state(&rss_series, load_end);
    // The plateau verdict travels as f64 across the metric surface; turned back into a tri-state
    // Measurement here (not `Option<bool>`) so "did not settle" (a gateway claim) stays distinct from
    // "could not judge" (a window claim), and an absent verdict keeps its reason. Same for `load_s`.
    let plateaued = carry(&take("memory_plateaued"), |v| v != 0.0);
    let load_s = carry(&take("memory_load_s"), |v| v as i64);
    crate::record::CellMemory {
        // `served` here means the cell was served, which is the only reason a window ran at all.
        served: true,
        // Formatted from the constants that actually ran, so it can't drift from them. Every cell
        // gets the same fixed-length load; the duration does not depend on when a gateway settles.
        protocol: format!(
            "cold restart, {}s idle read at rest, then load at c={} for {}s, then {}s with the load \
             removed. `plateaued` is a verdict over the trailing {}s, taken at the END of the load: \
             steady means drift under {}% and range under {}% of the mean. The verdict does not end the \
             load - every cell is loaded for the full duration.",
            crate::metric::MEMORY_IDLE_S,
            crate::metric::MEMORY_WINDOW_CONCURRENCY,
            crate::metric::MEMORY_LOAD_S,
            crate::metric::MEMORY_RECOVERY_S,
            crate::metric::MEMORY_PLATEAU_WINDOW_S,
            crate::metric::MEMORY_TREND_PCT,
            crate::metric::MEMORY_RANGE_PCT,
        ),
        idle_rss_mib: take("memory_idle_mib"),
        peak_rss_mib: take("memory_peak_mib"),
        peak_rss_hwm_mib: take("memory_hwm_mib"),
        recovered_rss_mib: take("memory_recovered_mib"),
        growth_rate_mib_per_min: take("memory_growth_rate_mib_per_min"),
        time_to_plateau_s: take("memory_time_to_plateau_s"),
        load_s,
        plateaued,
        idle_window_s: Some(crate::metric::MEMORY_IDLE_S as i64),
        // The slice `recovered_rss_mib` is a median OVER, not the full recovery wait: the median is
        // taken over the trailing `MEMORY_RECOVERY_MEDIAN_S`, since the first half still holds the
        // descent from peak.
        recovery_window_s: Some(crate::metric::MEMORY_RECOVERY_MEDIAN_S as i64),
        steady_state_rss_mib: steady,
        rss_series,
        idle_rss_series,
        shape: take("memory_shape"),
        idle_shape: take("memory_idle_shape"),
        idle_static: take("memory_idle_static"),
        idle_growth_rate_mib_per_min: take("memory_idle_growth_rate_mib_per_min"),
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
    let tail: Vec<f64> = under_load
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
    // Delegated to `stats::median` (which refuses non-finite input) rather than hand-rolled, so
    // there is one place the board's median rule lives.
    crate::stats::median(&tail)
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

    // Counts as served when a difference was obtained, or when it's below what the rig can resolve
    // (both legs delivered frames; only the difference was too small to weigh) - not a plain `false`,
    // which would misread as "the stream did not flow".
    let flowed = |k: &str| {
        metrics
            .get(k)
            .is_some_and(|m| m.is_measured() || matches!(m.reason(), Some(Absent::BelowResolution)))
    };
    let ttft = metrics.get("added_ttft_p50_us");
    // The verdict covers every streaming comparison, not just TTFT: a cell whose gap figures
    // measured cleanly must not read as unserved just because the TTFT leg alone was absent.
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
        // A token here, matching `Cell::reason`; free-text detail goes in `stream_error` instead.
        reason,
        stream_error,
        added_ttft_p50_us: us("added_ttft_p50_us"),
        added_ttft_p99_us: us("added_ttft_p99_us"),
        added_gap_p50_us: us("added_gap_p50_us"),
        added_gap_p99_us: us("added_gap_p99_us"),
        stream_c1_note: stream_c1_note(metrics),
        ttft_gw_samples: as_i64(metrics.get("ttft_gw_samples")),
        ttft_direct_samples: as_i64(metrics.get("ttft_direct_samples")),
        // Rungs the stream searches walked, published regardless of verdict - the only thing that
        // explains a ceiling suppressed as mock-bound or absent from running out of range.
        sweep_streams: series.map(|s| s.sweep_streams.clone()).unwrap_or_default(),
        ..Default::default()
    };
    judge_streams_sustained(cfg, dialect, &mut out, metrics);
    out
}

/// What the concurrency-1 streaming legs were actually taken over: how many frames each single
/// stream produced, since the published gap p50 is a median over the intervals between them and a
/// leg with three frames carries very different weight than one with sixty-four. `None` when the
/// group took no stream at all.
fn stream_c1_note(
    metrics: &std::collections::BTreeMap<&'static str, Measurement<f64>>,
) -> Option<String> {
    let gw = metrics.get("gateway_c1_frames")?.copied()?;
    let direct = metrics.get("direct_c1_frames")?.copied()?;
    if gw <= 0.0 && direct <= 0.0 {
        return None;
    }
    // Added-TTFT is a percentile over STREAM_TTFT_SAMPLES separate probes, NOT over one stream; only
    // added-GAP comes from the intervals inside a single stream (STREAM_FRAME_BUDGET frames deep).
    let ttft_n = |k: &str| {
        metrics
            .get(k)
            .and_then(|m| m.copied())
            .map(|v| format!("{v:.0}"))
            .unwrap_or_else(|| "no".to_string())
    };
    Some(format!(
        "the c=1 streaming legs read {gw:.0} frame(s) through the gateway and {direct:.0} direct to \
         the mock, out of a {} frame budget. The added-GAP figures are percentiles over the intervals \
         inside those two streams. The added-TTFT figures are percentiles over separate probes - {} \
         through the gateway and {} direct, out of {} attempted per leg - so they carry that weight \
         and no more",
        crate::metric::STREAM_FRAME_BUDGET,
        ttft_n("ttft_gw_samples"),
        ttft_n("ttft_direct_samples"),
        crate::metric::STREAM_TTFT_SAMPLES
    ))
}

/// Judge the streams-sustained ceiling against the mock's own frames/sec at the same concurrency.
/// Deliberately the same shape as `judge_sustained`: one cell must not answer "was this the rig or
/// the gateway" two different ways.
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
    // c == 0 is `bisect_ceiling`'s own measured "nothing sustains this gate": no concurrency to
    // take a reference at, so the mock was never asked to do anything.
    if conc == 0 {
        out.streams_sustained = Measurement::Measured(0);
        out.streams_sustained_fps = Measurement::Measured(0.0);
        out.streams_sustained_mock_ceiling = None;
        out.streams_sustained_headroom = None;
        return;
    }
    apply_streams_sustained_verdict(out, value, conc, stream_rig_ceiling(cfg, dialect, conc));
}

/// Re-wrap an absence so its reason AND detail survive a narrowing, rather than flattening to a bare
/// null. Generic in the source type too, so it also serves companion fields like
/// `rps_max_proxy_concurrency` / `conc_at_peak` and their sustained/stream equivalents - one absence,
/// one story, at every key that carries it.
///
/// A measured input yields `NotMeasured`, unreachable from the call sites (all inside an absent
/// branch) and the conservative answer if one ever stops being.
fn carry_absence<A, T>(m: &Measurement<A>) -> Measurement<T> {
    match (m.reason().cloned(), m.detail()) {
        (Some(r), Some(d)) => Measurement::absent_because(r, d),
        (Some(r), None) => Measurement::absent(r),
        (None, _) => Measurement::absent(Absent::NotMeasured),
    }
}

/// Fill the streams-sustained fields from the measurement and the mock's derived ceiling. Pure, and
/// separate from its judge (like `apply_peak_verdict`), so the decision isn't welded to a live
/// reference and stays testable.
///
/// Matching the mock's rate IS the gateway succeeding: the mock's frames/sec is a target rate (c
/// streams times one frame per interval), not a capacity it ran out of, so a gateway keeping pace at
/// ~99% must not be suppressed as rig-limited. Whether it should have kept pace is a delivery
/// question, asked separately by the gate.
fn apply_streams_sustained_verdict(
    out: &mut crate::record::CellStream,
    value: f64,
    conc: u32,
    reference: Measurement<f64>,
) {
    out.streams_sustained_fps = Measurement::Measured(value);
    out.streams_sustained = Measurement::Measured(i64::from(conc));
    out.streams_sustained_mock_ceiling = reference.copied();
    out.streams_sustained_headroom = rigbound::headroom(value, &reference);
}

/// What each published metric means, built from the constants that define it (never hand-written,
/// so the description can't drift from what actually ran). Every entry answers what quantity this
/// is, which observations counted, and how the measurement knew to stop.
fn metric_definitions() -> std::collections::BTreeMap<String, String> {
    let mut d = std::collections::BTreeMap::new();
    let bounds: Vec<String> = crate::frontier::P99_BOUNDS_US
        .iter()
        .map(|us| format!("{}ms", us / 1000))
        .collect();
    d.insert(
        "perf.cost".to_string(),
        format!(
            "WHAT A REQUEST COST, in microseconds of gateway CPU. User plus system time, summed \
             across the gateway's whole PROCESS TREE (a gateway that forks workers keeps its CPU in \
             the children, and a parent-only reading would report a busy gateway as idle), sampled \
             as the DIFFERENCE across one load window and divided by the requests that window \
             actually completed. \
             \
             WHY IT EXISTS BESIDE THROUGHPUT. Peak rps is a SATURATION number: once a gateway fills \
             the cores it is pinned to, the ladder stops describing the gateway and starts describing \
             the box, and two gateways at that wall read the same however different they are. Cost per \
             request has no such ceiling - at saturation both serve the same rate by definition, and \
             the one doing less work per request still reads lower. It is also the figure that maps to \
             money, since half the CPU serves the same traffic on half the instance. \
             \
             MEASURED AT ONE CONCURRENCY, c={COST_CONC}, HELD IDENTICAL FOR EVERY GATEWAY and published \
             as `cost_window_conc`. No single concurrency is below saturation for a field spanning \
             double-digit to five-figure rps, so matched LOAD is impossible and matched CONCURRENCY is \
             the honest substitute - and it is stated rather than assumed. \
             \
             REFUSED, NOT ESTIMATED, IN TWO CASES. A window with ANY failure publishes no cost at all: \
             CPU spent refusing requests is real, but dividing it by only the successes would describe \
             the failures rather than the work. And a window with major page faults means the box was \
             SWAPPING, so what was timed is the disk - those numbers publish with `cost_majflt` \
             non-zero and the cost itself marked a harness fault, so a reader sees why the row looks \
             wrong instead of finding a hole. \
             \
             `cost_core_utilisation` is the fraction of the PINNED cores busy across that same window. \
             It is what makes a peak interpretable: near 1.0 the gateway had filled the cores it was \
             given and its throughput number is a real ceiling; well below it, the limit is somewhere \
             else and the peak means something else.",
            COST_CONC = crate::metric::COST_WINDOW_CONCURRENCY
        ),
    );
    d.insert(
        "perf.frontier".to_string(),
        format!(
            "THROUGHPUT AT A TAIL LATENCY YOU ACCEPT. For each declared bound, the most requests/sec \
             this cell carried while 99% of requests finished under that bound AND it failed none it \
             accepted. Bounds: {} plus one reading with no latency bound at all (failures only). \
             \
             ONE MEASUREMENT, READ SIX WAYS: a single concurrency sweep, published as `sweep_max_proxy`, \
             so every reading is re-derivable from the rungs rather than taken on trust. Monotone \
             non-decreasing across the bounds BY CONSTRUCTION - relaxing a bound only adds rungs to the \
             set a maximum is taken over - so a reading cannot exceed a looser one. \
             \
             ZERO FAILURES, not a tolerance: the load generator counts connections the RIG could not \
             open separately (`rig_refused`, and those windows are discarded), so a failure reaching a \
             reading is the gateway losing a request it had accepted. \
             \
             TERMINATION: the sweep climbs a doubling ladder from the floor and stops at the first \
             concurrency where no window served cleanly, because more concurrency cannot un-fail those \
             requests. Each reading names `first_disqualified_conc`, the lowest concurrency above it that \
             stopped qualifying - that pair is the proof it is a boundary. When nothing above it \
             disqualified, the sweep ran out of range instead of finding a limit, and the reading sets \
             `lower_bound: true`: the rate is real, it is simply a floor rather than a ceiling. \
             \
             The `p99_us` on a reading is the tail the winning rung ACTUALLY produced, never the bound - \
             4ms under a 100ms bound is not the same finding as 99ms.",
            bounds.join(", ")
        ),
    );
    d.insert(
        "perf.added_latency".to_string(),
        "WHAT THE GATEWAY ADDS, at concurrency 1. The gateway leg's percentile minus the same request \
         taken straight to the mock, one at a time, both legs in the same window. No search and no \
         threshold: there is nothing to stop early and nothing to decide. A difference at or below what \
         the rig can resolve publishes `below_resolution` - the best result the comparison can express - \
         and every surface renders that apart from a never-measured hole."
            .to_string(),
    );
    d.insert(
        "stream.streams_sustained".to_string(),
        format!(
            "THE MOST CONCURRENT SSE STREAMS THIS CELL CARRIES CLEANLY, and the frames/sec at that \
             point. A concurrency holds the gate when every expected content frame arrives (ratio \
             {:.1}, i.e. no tolerance - a proxy that drops a frame has dropped a user's token), no \
             inter-frame gap exceeds {}x the mock's own pacing interval, and under {:.1}% of streams \
             error. \
             \
             TERMINATION: the gate is monotone in concurrency, so the ceiling is a boundary found by \
             bisection - a pass proven at n and a failure measured above it. `sweep_streams` carries \
             every rung probed. A range whose top still passed publishes an absence rather than a \
             ceiling, because the top of the range is our choice and not the gateway's answer.",
            crate::run::STREAM_MIN_DELIVERY_RATIO,
            crate::run::STREAM_STALL_MULTIPLIER,
            crate::run::STREAM_MAX_ERROR_RATIO * 100.0,
        ),
    );
    d.insert(
        "stream.added_ttft_and_gap".to_string(),
        "WHAT THE GATEWAY ADDS TO A STREAM, at concurrency 1: time to the first event, and the gaps \
         between frames inside one stream, each as the gateway leg minus the same stream taken straight \
         to the mock. Time-to-first-EVENT rather than first token, deliberately and by the same \
         definition on both legs: a frame budget of one is satisfied by a dialect's opening scaffolding \
         (openai sends a role delta, anthropic a `message_start`), so the quantity is the same on both \
         sides and the difference still isolates what the gateway added."
            .to_string(),
    );
    // Memory's definition is the `protocol` string on the memory block itself, where it has been since
    // before this map existed. Duplicating it here would create the second place for one statement to
    // drift that this map's own doc warns about.
    d.insert(
        "memory".to_string(),
        "See `matrix.memory.protocol` and each cell's `memory.protocol`, which state the durations and \
         both plateau thresholds formatted from the constants that ran."
            .to_string(),
    );
    d
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
        gw_cores: cfg.gw_cores.clone(),
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
        // The artifact's own token vocabulary (`.token()`), not a Rust Debug tag - every other null
        // in this snapshot uses the same tokens.
        "observed_absent_reason": observed.reason().map(|r| r.token()),
        "observed_absent_detail": observed.detail(),
        "baseline_rps": baseline.value().copied(),
        "drift_pct": drift.value().copied(),
        "baseline_samples": history.len(),
    })
}

/// Every previous box-qualification observation available to this run.
///
/// Two sources, because the box has no history of its own: reading the results directory works when
/// the engine runs where the record lives, but in the field each gateway gets a fresh EC2 instance
/// with an empty directory, so `OTB_QUALIFY_BASELINE` is how the orchestrator hands over what it
/// knows. The env value is appended to (not replacing) whatever the directory yields.
///
/// The observation is the rig's own loopback throughput, not the gateway's, so one pooled baseline
/// across gateways is the right comparison, not a per-gateway one - it is the box being qualified.
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
/// Only runs that `Outcome::qualifies_as_baseline()` contribute: a failed qualification's rps must
/// not enter the median that decides whether the NEXT run fails, or a contaminated box could drag
/// the band down until nothing ever fails the gate. A record with no readable outcome is treated as
/// not qualifying, same reasoning.
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
    // Ledger RIG-12: warn once per gateway (not once per cell) when the manifest declares a header
    // the rig already sends itself. The wire picks the rig's copy and drops the manifest's, but a
    // silent precedence rule is the same ambiguity as two headers racing - so this surfaces it.
    // Deliberately not in `Manifest::problems` (the hard gate): a manifest with this is not
    // misconfigured, just carrying a line that should be deleted.
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
        gw_cores: cfg.gw_cores.clone(),
        // How to put this gateway back at rest, so the memory group can read an idle that is
        // actually idle. Built from the same manifest declaration the initial launch used. `None`
        // for a manifest with no launch: the harness does not own that gateway's lifetime.
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
        // The gateway's own headers, resolved once for the run. Refused via `?` rather than
        // defaulted: a column whose headers can't resolve must fail loud, not fall back to an empty
        // set and silently measure the whole run without auth/routing (which then publishes
        // `served: false` as if the gateway itself were broken). `manifest.validate()` doesn't
        // exercise `{...}` substitution, so a bad placeholder reaches here undetected.
        static_headers: cfg
            .manifest
            .headers_for("", &cfg.gw_cores, cfg.mock_addr.port(), &cfg.gw_dir)
            .map_err(|e| crate::snapshot::SnapshotError::UnresolvableHeader {
                detail: e.to_string(),
            })?,
        // Same refusal as `static_headers` above: must fail loud (`?`), not silently drop a dialect
        // whose headers can't resolve, which would record `served: false` as if the gateway declined
        // it rather than the harness losing the headers. Collected into a Result so the first
        // failure propagates - a resolution failure is a manifest defect, not a declined dialect.
        egress_headers: cfg
            .dialects
            .iter()
            .map(|d| {
                let mut h = cfg
                    .manifest
                    .headers_for(d.as_str(), &cfg.gw_cores, cfg.mock_addr.port(), &cfg.gw_dir)
                    .map_err(|e| crate::snapshot::SnapshotError::UnresolvableHeader {
                        detail: format!("egress {}: {e}", d.as_str()),
                    })?;
                // headers_for prepends the always-on set; strip it here since the run config carries
                // it separately. Resolved again just to subtract, so it too must fail loud (`?`).
                let statics = cfg
                    .manifest
                    .headers_for("", &cfg.gw_cores, cfg.mock_addr.port(), &cfg.gw_dir)
                    .map_err(|e| crate::snapshot::SnapshotError::UnresolvableHeader {
                        detail: e.to_string(),
                    })?;
                h.retain(|x| !statics.contains(x));
                Ok((d.as_str().to_string(), h))
            })
            .collect::<Result<_, crate::snapshot::SnapshotError>>()?,
        runtime: cfg.manifest.runtime.clone(),
    };

    // The mock's recorder must be off before box qualification (its baseline window runs before the
    // first cell): a box booted with MOCK_RECORD=1 would take that baseline against a recording mock
    // while every later window runs against a quiet one. `reverify_cell` holds this per-cell already.
    if let Some(why) = crate::reverify::quiesce_recorder(&rc) {
        eprintln!("suite: the mock's recorder could not be quiesced before the run: {why}");
    }

    // Qualified before the grid, not after: the verdict is about the machine everything below is
    // measured on, so it must be taken before the run has loaded the box.
    let box_qualify = qualify_box(cfg, &qualify_history(Path::new(&cfg.results_dir)));

    let mut upstreams: HashMap<String, Upstream> = HashMap::new();
    let mut any_served = false;
    let mut last_egress: Option<String> = None;
    let mut written: Option<Paths> = None;

    // Written incrementally after every egress column, not buffered to one write at the end: these
    // runs take hours on a box with a hard self-termination timer, so an interrupted run must not
    // lose cells already measured. A promote-guard trip on a mid-run checkpoint is expected (thinner
    // than the finished run) and is logged and skipped; only the FINAL flush below may be fatal.
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
                    // A checkpoint write failure must not abandon the rest of the grid - the final
                    // flush will attempt the same write again and is what decides whether the run
                    // ultimately failed.
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

        // `status`/`body_snippet` are recorded on every cell so the artifact can say what the
        // gateway actually answered, not just "does not serve".
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
            // The path this cell was actually driven at, not the dialect's standard one recomputed
            // after the fact - a provider-pinned route and the unified route are different
            // measurements and must not read as the same number.
            path: ing
                .parse::<Dialect>()
                .map(|d| run::path_for(&rc, d, &eg))
                .unwrap_or_default(),
            status,
            body_snippet: snippet,
            // A failed re-verification must be visible without opening the perf block, so it's
            // surfaced through `verdict_note`, the evidence string every consumer already reads.
            verdict_note: match (result.reverify.verified, &result.reverify.note) {
                (Some(false), Some(note)) => format!("egress not re-verified: {note}"),
                _ => result.outcome.note.clone().unwrap_or_default(),
            },
            perf,
            /* Memory must be withheld on a refuted cell too, same as perf/stream: a cell proven to
            have forwarded rather than translated the request had its RSS measured under that same
            wrong wire, so "CPU burned serving a wire that is not this pairing is not this pairing's
            cost" applies verbatim to memory. `site/gen-data.mjs` reads memory from the per-cell
            window gated only on `served === true`, so an untouched value here would still reach
            the board. */
            memory: result
                .metrics
                .as_ref()
                .map(|m| cell_memory(m, result.series.as_ref()))
                .map(
                    |mem| match (result.reverify.verified, &result.reverify.note) {
                        (Some(false), note) => withhold_refuted_memory(
                            mem,
                            note.as_deref()
                                .unwrap_or("no evidence was recorded with the refutation"),
                        ),
                        _ => mem,
                    },
                ),
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
            // `configurable` is whether this gateway can be pointed at this upstream at all, read
            // from the manifest's `egress` list rather than hardcoded `true` (which would falsely
            // claim every column configured, including ones the manifest never wired).
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

/// A refuted re-verification withholds the numbers, not just annotates them. `Some(false)` is proof
/// (`reverify.rs`) that the request did not reach the mock as this cell's egress dialect, so every
/// perf/stream number was taken over the wrong wire and publishing them would state a translation
/// throughput for a translation that never happened. `perf_dropped` records that the numbers were
/// withheld; `verdict_note`/`egress_reverified`/`reverify_note` still carry the evidence. `None`
/// (diagonal cell, recording off, mock unreachable) and `Some(true)` leave the blocks alone - only
/// proof of a misroute withholds, not suspicion.
///
/// Kept as one pure function (separate from the grid loop) so the withholding is independently
/// testable: it was previously inline in a function that needs a live gateway/mock/socket to reach,
/// so its tests could pass green while the call site itself was silently deleted.
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
            // Rungs published whatever the verdict: when a peak is suppressed as rig-bound or
            // absent, the sweep is the only thing that explains why.
            if let Some(series) = result.series.as_ref() {
                // One frontier reading per tail-latency bound, off the same rungs `sweep_max_proxy`
                // carries, so a reader can re-derive it rather than take it on trust.
                p.frontier = series.frontier.clone();
                p.sweep_max_proxy = series.sweep.clone();
            }
            // The anti-false-positive guard, published beside the numbers it qualifies: a cell
            // whose egress was not proven (`None`, see `reverify.rs`) still carries real
            // measurements but must not read as an unqualified translation claim.
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

/// Strip a refuted cell's measurements, keeping the block (not `None`) so `egress_reverified` and
/// `reverify_note` can still say WHY there are no numbers. `NotServed` is the reason on every field:
/// the refutation is a statement about the gateway, not the rig. Sweeps are cleared too - a rung is
/// as much a published number as the ceiling drawn from it.
fn withhold_refuted_perf(p: CellPerf, why: &str) -> CellPerf {
    let detail = format!(
        "withheld: re-verification proved the request did not reach the mock as this cell's egress \
         dialect, so this number was taken over a wire that is not this pairing: {why}"
    );
    let withheld = || Measurement::absent_because(Absent::NotServed, detail.clone());
    // The same withholding for the f64-typed fields; one detail string, two element types.
    let withheld_f =
        || -> Measurement<f64> { Measurement::absent_because(Absent::NotServed, detail.clone()) };
    CellPerf {
        added_latency_p50_us: withheld(),
        added_latency_p99_us: withheld(),
        gateway_c1_p99_us: withheld(),
        direct_c1_p99_us: withheld(),
        // Cost is withheld too: CPU burned serving a wire that is not this pairing is not this
        // pairing's cost.
        cpu_us_per_request: withheld_f(),
        rps_per_cpu_second: withheld_f(),
        cost_window_conc: withheld(),
        cost_window_ok: withheld_f(),
        cost_window_rps: withheld_f(),
        cost_core_utilisation: withheld_f(),
        cost_threads: withheld_f(),
        cost_nonvol_ctxt_per_request: withheld_f(),
        cost_majflt: withheld_f(),
        frontier: Vec::new(),
        sweep_max_proxy: Vec::new(),
        // Kept verbatim: the whole reason the block survives.
        egress_reverified: p.egress_reverified,
        reverify_note: p.reverify_note,
        c1_note: None,
    }
}

/// The streaming half of the same withholding: same rule as `withhold_refuted_perf` (keep the block,
/// not `None`, so `stream_served`/reason/rungs can still say why).
///
/// Every measured field of a refuted cell's memory window, withheld with the reason - same rule as
/// `withhold_refuted_perf`. Series are cleared too: a curve drawn from a misrouted window is the same
/// false claim as a scalar taken from it.
fn withhold_refuted_memory(m: crate::record::CellMemory, why: &str) -> crate::record::CellMemory {
    let detail = format!(
        "withheld: re-verification proved the request did not reach the mock as this cell's egress \
         dialect, so this window measured a wire that is not this pairing: {why}"
    );
    fn withheld<T>(detail: &str) -> Measurement<T> {
        Measurement::absent_because(Absent::NotServed, detail.to_string())
    }
    crate::record::CellMemory {
        idle_rss_mib: withheld(&detail),
        steady_state_rss_mib: withheld(&detail),
        recovered_rss_mib: withheld(&detail),
        peak_rss_mib: withheld(&detail),
        peak_rss_hwm_mib: withheld(&detail),
        time_to_plateau_s: withheld(&detail),
        growth_rate_mib_per_min: withheld(&detail),
        plateaued: withheld(&detail),
        load_s: withheld(&detail),
        shape: withheld(&detail),
        idle_shape: withheld(&detail),
        idle_static: withheld(&detail),
        idle_growth_rate_mib_per_min: withheld(&detail),
        rss_series: Vec::new(),
        idle_rss_series: Vec::new(),
        ..m
    }
}

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
        ttft_gw_samples: withheld(&detail),
        ttft_direct_samples: withheld(&detail),
        added_gap_p50_us: withheld(&detail),
        added_gap_p99_us: withheld(&detail),
        streams_sustained: withheld(&detail),
        streams_sustained_fps: withheld(&detail),
        streams_sustained_mock_ceiling: None,
        streams_sustained_headroom: None,
        sweep_streams: Vec::new(),
        stream_c1_note: None,
        // Kept verbatim: the whole reason the block survives rather than becoming a None.
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
        // A gateway built from source has no image to name; fall back to the runtime identity the
        // manifest declares, which the memory reader and the stop path already agree on.
        _ => Some(spec.runtime.identity().to_string()),
    }
}

/// Build the record from what has been measured so far and write it.
///
/// The config the gateway actually ran from, captured into the artifact as evidence the gateway was
/// measured as it ships, not as tuned. Read at flush time from the gateway's own directory so it's
/// the text the process actually started with, not a template re-rendered later. A file that cannot
/// be read is skipped, not fatal: a finished measurement must not be discarded over its provenance.
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
    // The rig block exists if any part of it does: box_qualify, engine stamp and mock provenance are
    // independent facts, so keying it off just one would drop the others when that one is missing.
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
    // Resolved once and used at both the root and matrix levels so the two never disagree.
    let build =
        gateway_build(cfg).unwrap_or_else(|| format!("otb-engine {}", env!("CARGO_PKG_VERSION")));
    let snap = ResultSnapshot {
        schema_version: 1,
        definitions: metric_definitions(),
        config: rendered_config(cfg),
        gateway: cfg.manifest.name.clone(),
        // The gateway's own build string, not the engine's (which lives in rig.engine) - otherwise
        // every artifact would claim the same build. Falls back to the engine string only when the
        // manifest declares no launch and the harness genuinely doesn't know the build.
        build: build.clone(),
        measured_at: cfg.measured_at.clone(),
        arch: Some(cfg.arch.clone()),
        hardware: cfg.hardware.clone(),
        rig: rig.clone(),
        matrix: Matrix {
            gateway: cfg.manifest.name.clone(),
            served: any_served,
            // Which phases this run measured. The consumer reads these to detect a degraded run and
            // refuse to publish it over a complete one, so this must reflect reality, not
            // `..Default::default()` (which would falsely claim streaming was never measured).
            cell_perf_sweep: true,
            cell_stream: true,
            cell_memory: Some(true),
            upstreams: upstreams.clone(),
            // Mirrored onto the matrix as well as the snapshot root: record.rs carries the field in
            // both places, and a reader that finds it in one and not the other cannot tell which is
            // authoritative.
            rig,
            // Mirrored one level down for the same reason as `rig` above: the matrix is exported and
            // diffed on its own, so it needs its own build/arch/hardware, not just the root's.
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
    // A published definition that drifts from the code is worse than no definition, so this asserts
    // the text is generated FROM the constants by checking it says what they currently say.
    #[test]
    fn the_cost_definition_names_the_concurrency_that_was_actually_used() {
        // Generated from the constant, not retyped beside it, so it cannot drift from the code.
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

    // A run must not declare itself degraded while carrying the data it says it skipped - the
    // consumer reads these three flags to spot a probe-only run.
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

    /// A metric that measures nothing and watches the results directory instead: this test can't
    /// interrupt a run mid-flight, so it observes from inside a cell whether earlier cells already
    /// reached disk, which is deterministic and equivalent.
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

    // The config is the evidence for the claim the board rests on - that each gateway ran as it
    // ships - so `ResultSnapshot.config` must actually be filled, not left at its zero value.
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

        // And that it is actually wired into the snapshot - the assertion that matters is on what
        // reached disk, not merely on what `rendered_config` can compute.
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

    // A partial run must leave its measurements on disk: a snapshot must flush at each egress-column
    // boundary as the grid runs, not only after the whole grid completes, or an interrupted run loses
    // everything measured so far. This watches the results directory from inside the cells themselves
    // so the checkpoint ordering is asserted directly rather than assumed.
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

    // A checkpoint promote-guard trip must not abort the whole run: the incremental flush after an
    // egress column is a thinner, unfinished view, so tripping the guard against a fuller prior
    // snapshot is expected mid-run and must not stop the remaining columns. Only the final flush's
    // guard trip may be fatal.
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
        // site/gen-data.mjs reads matrix.measured_at, not the snapshot root's - a matrix with no
        // stamp of its own renders as "never measured" regardless of how fresh the run was.
        assert_eq!(
            back.matrix.measured_at, back.measured_at,
            "matrix.measured_at must mirror the snapshot root's, or the board reads this run as unmeasured"
        );
        assert!(!back.matrix.measured_at.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A number that cannot be traced to the code that produced it is not evidence. The engine stamp
    // and box qualification are independent facts about the instrument; box_qualify: None here checks
    // the stamp still reaches the artifact when no qualification runs beside it.
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

    // Not covered here by design: an end-to-end suite test can't reach the verdict path because
    // `cfg_for`'s tight fixture never lets `search::saturation_plateau` complete a probe. That
    // behaviour is covered directly by the pure-function tests below instead (e.g.
    // `a_peak_at_the_rig_ceiling_is_published_with_its_headroom_not_withheld`), which is
    // deterministic where a live end-to-end version would be flaky for no extra coverage.

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
        // Matching the mock's paced rate (c x 1000/20ms by construction) is the success case, not a
        // rig-bound suppression - a gateway keeping pace within 0.7% must still publish its rate.
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
        // `added_ttft_p99_us` is published on every healthy streaming cell (taken over
        // STREAM_TTFT_SAMPLES separate probes, not one stream), so the note must not claim it's absent.
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
            note.contains("97"),
            "the note must state the gateway leg's sample count: {note}"
        );
        assert!(note.contains("100"), "and the direct leg's: {note}");

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

    // A declined cell must publish status, body_snippet and verdict_note, not defaults - otherwise a
    // rig-side failure that makes every probe return 4xx is indistinguishable from a gateway that
    // genuinely supports nothing.
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

    // The box has no history of its own - each gateway gets a fresh EC2 instance - so the baseline
    // must be handed to it via OTB_QUALIFY_BASELINE, or the qualification only ever seeds and can
    // never catch a bad box.
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

        // A healthy box within the band passes.
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
        // Far enough under the baseline that the box itself, not the gateway, is the problem.
        assert_eq!(judge_at(300_000.0), "fail");

        std::env::remove_var("OTB_QUALIFY_BASELINE");
    }

    // What the run executed on must reach the artifact: a board whose whole claim is that only the
    // gateway differs between columns cannot leave the hardware unstated.
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

    // The block holding the numbers must name what produced them too, not just the snapshot root -
    // `matrix` is exported, diffed and archived on its own.
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

    // A failed qualification must never seed the baseline it failed against, or the gate would
    // slowly drift toward whatever a sick box was doing until nothing failed it at all.
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

    // A cell whose stream demonstrably flowed must not read as one that did not: `stream_served` must
    // reflect a clean gap measurement even when the TTFT leg alone failed.
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

    // A stream that produced nothing still publishes the reason as the status, and also as a token in
    // `reason` (a consumer branches on that field, so it must never be prose-or-nothing).
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

    // Steady state is what it costs UNDER LOAD, not after. The sampler runs load then recovery as one
    // series; taking the trailing half unbounded reports the post-load level instead, which is
    // `recovered_rss_mib`'s question. This fixture (400 MiB under load, releases to 50) is the shape
    // that makes the two answers differ.
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

    // Every null on a served cell must carry a reason reachable from the absences map, not a bare
    // `Option` that silently drops the group's own explanation.
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
}
