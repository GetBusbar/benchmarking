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

/// The mock's own STREAM rate at the concurrency the gateway's stream number was taken at - DERIVED
/// from the mock's declared pacing (`run::mock_frame_ceiling_fps`), not measured.
///
/// Taken at the operating point rather than the top of the range, for the reason `rigbound.rs`'s
/// header gives. Published beside the gateway's rate as a ratio only; nothing is withheld on it.
fn stream_rig_ceiling(_cfg: &SuiteConfig, _dialect: Dialect, at_conc: u32) -> Measurement<f64> {
    // DERIVED, NOT MEASURED. See `run::mock_frame_ceiling_fps`: the direct-to-mock leg engages fewer of
    // this box's cores than the gateway leg (the loadgen drives AND reads, with the gateway's cores
    // idle), so it is systematically slower than the path it was meant to bound - and judging a gateway
    // against it discarded real numbers on seven gateway/metric pairs. We own the mock, so its ceiling
    // is arithmetic: it cannot emit frames faster than it sleeps.
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
/// ONE JUDGEMENT LEFT. This used to also derive the four throughput scalars: it read `rps_max_proxy` /
/// `conc_at_peak` off the surface, took a live rig reference at the winning concurrency, and handed both
/// to `apply_peak_verdict` - then did it again for the sustained pair. All of that is gone. The
/// throughput answer is the FRONTIER, which the metric group reads off the sweep's own rungs and hands
/// over on the series, so nothing here decides anything about it (see `frontier.rs`).
///
/// `rig_ceiling` went with them: it existed to measure the mock's own throughput at the winner's
/// concurrency so a chosen fraction could decide whether to publish. Nothing suppresses now, so nothing
/// needs that reference.
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

/// Carry the cost group's fields onto the record.
///
/// A STRAIGHT CARRY, WITH NO JUDGEMENT APPLIED, on purpose. Every suppression this column could want
/// has already been decided where the evidence is: the metric refuses a window that had failures
/// (CPU divided by only the successes would describe the failures, not the work) and re-flags the
/// cost as a harness fault when the box was swapping. Re-deciding any of that here, away from the
/// counters, is how two rules drift apart and the artifact ends up disagreeing with itself.
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
        // THE PROTOCOL, PUBLISHED. Every duration and every threshold a reader would need to check the
        // numbers below, in the artifact, from the constants themselves - so it cannot drift from what
        // ran. It used to say "until the trailing 60s is flat (cap 300s)", which described a load whose
        // LENGTH depended on the gateway: the flatness test ended the measurement, so a gateway that
        // looked settled early was asked a shorter question than one that looked busy, and their peaks
        // were then ranked against each other. Every cell now gets the same load.
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
        // Both windows disclose their own length. idle_window_s has been null since the field was
        // declared, because idle was one instantaneous read and there was no window to name.
        idle_window_s: Some(crate::metric::MEMORY_IDLE_S as i64),
        // THE SLICE THE NUMBER CAME FROM, not the length of the wait. This published
        // `MEMORY_RECOVERY_S` (60) while `recovered_rss_mib` is the median over the trailing
        // `MEMORY_RECOVERY_MEDIAN_S` (30) - the first half still holds the descent from peak, so a median
        // across the whole minute would report a level the process never sat at. The wait is still 60 s
        // and the `protocol` string above still says so; this field names the window the published figure
        // is a median of, which is what a reader checking it needs.
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
    // DELEGATED RATHER THAN REPEATED. This was a hand-rolled sort-and-take-the-middle using
    // `partial_cmp(b).unwrap_or(Ordering::Equal)` - the identical comparator, and therefore the
    // identical defect, as `stats::median` had: a non-finite sample makes that comparator a non-total
    // order, which leaves the whole slice in an unspecified permutation and makes the answer depend on
    // the order the samples arrived in rather than on the samples.
    //
    // `stats::median` now refuses a non-finite input outright. Two implementations of one statistic
    // means a guard added to either is absent from the other, so the duplicate is gone instead of
    // separately patched: whatever the board's rule for a median is, there is one place it lives.
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
        ttft_gw_samples: as_i64(metrics.get("ttft_gw_samples")),
        ttft_direct_samples: as_i64(metrics.get("ttft_direct_samples")),
        // THE RUNGS THE TWO STREAM SEARCHES WALKED, published whatever the verdict, for the same
        // reason `sweep_max_proxy` is: when a ceiling is suppressed as mock-bound or absent because
        // the search ran out of range, the rungs are the only thing that explains why.
        sweep_streams: series.map(|s| s.sweep_streams.clone()).unwrap_or_default(),
        ..Default::default()
    };
    judge_streams_sustained(cfg, dialect, &mut out, metrics);
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
    // WHAT IS ACTUALLY TRUE OF EACH PERCENTILE. This note used to end "the p99 fields are absent
    // because one stream cannot support a 99th percentile" - false of `added_ttft_p99_us` on every
    // healthy streaming cell, because that pair is taken over STREAM_TTFT_SAMPLES separate probes, not
    // over one stream. Only the added-GAP figures come from the intervals inside a single stream, and
    // even there the claim was wrong in the other direction: a stream carries STREAM_FRAME_BUDGET
    // frames, so it yields that many gaps minus one - plenty for a percentile. A reader checking either
    // statement against the artifact would have found a published p99 sitting beside a note saying it
    // could not exist.
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

/// Carry the streams-sustained ceiling, with the MOCK's own frames/sec at the same concurrency beside
/// it as a ratio.
///
/// No judgement is made here: the rig-bound suppression this once applied is gone (`rigbound.rs`),
/// so the ceiling and its rate are published as measured and the mock's rate travels as
/// `streams_sustained_mock_ceiling` / `streams_sustained_headroom` - see
/// `apply_streams_sustained_verdict`.
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
        out.streams_sustained_mock_ceiling = None;
        out.streams_sustained_headroom = None;
        return;
    }
    apply_streams_sustained_verdict(out, value, conc, stream_rig_ceiling(cfg, dialect, conc));
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

/// Fill the streams-sustained fields from the measurement and the mock's derived ceiling. PURE, and
/// separate from its judge, for the identical reason `apply_peak_verdict` is: the suite's own fixture
/// points the gateway and the mock at one server, so a decision welded to a live reference would be
/// untestable end to end.
///
/// MATCHING THE MOCK'S RATE IS THE GATEWAY SUCCEEDING. The mock paces its deltas at a fixed interval
/// and says so in its own header: "the pacing is the model generating tokens; the stream suite measures
/// what a gateway ADDS on top of it". So the mock's frames/sec is not a capacity it ran out of, it is
/// the TARGET RATE - c streams times one frame per interval. A gateway that forwards every frame as it
/// arrives lands at ~99% of it, which is the best available outcome, and it was suppressed as
/// rig-limited: 13 cells lost `streams_sustained` and 11 lost `cpu_fps` on the 2026-07-28 board, with
/// agentgateway keeping pace to within 0.7% and publishing nothing.
///
/// Whether it SHOULD have kept pace is what the gate decides, and the gate is about delivery - frames
/// arriving, nothing stalling, streams not erroring. That is the right question and it is asked
/// separately, so a gateway that cannot keep pace falls short there and the shortfall is published as
/// its own rather than hidden behind ours.
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

/// WHAT EACH PUBLISHED METRIC MEANS, built from the constants that define it.
///
/// Every entry answers the three questions a reader needs to check a number rather than trust it:
/// what quantity is this, which observations counted, and how did the measurement know to stop. The
/// third is the one this engine got wrong for a long time - a search that stopped on a chosen flatness
/// count published a smaller number as a "maximum" and said nothing about having chosen.
///
/// Formatted from the constants, never hand-written. The retired throughput gate is why: every surface
/// described it as "p99 < 1 s" while the engine enforced 20 ms - a bar 96% of the 1632 recorded rungs
/// pass, against 57% for the real one - so readers reasoned about a test that never ran.
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
             ONE MEASUREMENT, READ {readings} WAYS: a single concurrency sweep, published as `sweep_max_proxy`, \
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
            bounds.join(", "),
            readings = crate::frontier::P99_BOUNDS_US.len() + 1
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
/// The caller rejects a Fail before starting the gateway grid; seed and skipped
/// retain their existing meaning and do not invent an extra qualification threshold.
fn require_qualified_box(verdict: &serde_json::Value) -> Result<(), SnapshotError> {
    if verdict.get("outcome").and_then(serde_json::Value::as_str) == Some("fail") {
        return Err(SnapshotError::BoxQualificationFailed {
            verdict: verdict.clone(),
        });
    }
    Ok(())
}

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
        // THE ARTIFACT'S OWN VOCABULARY, not a Rust Debug tag. Every other null in this snapshot
        // says "not_measured"; this said "NotMeasured", so a consumer branching on the published
        // token did not recognise it - and the absence's detail, the only statement of WHY the box
        // was judged, was dropped entirely.
        "observed_absent_reason": observed.reason().map(|r| r.token()),
        "observed_absent_detail": observed.detail(),
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
    qualify_history_with(
        results_dir,
        std::env::var("OTB_QUALIFY_BASELINE").ok().as_deref(),
    )
}

/// `qualify_history` over the env value as an argument (`None` when unset), so the rule is testable
/// without mutating a process-global the rest of the suite reads.
///
/// A value that is SET BUT UNUSABLE is said out loud. On a fresh box this variable is the only
/// history there is, so dropping a mangled one silently turned "judge this box" into "seed a new
/// baseline" - the qualification passing exactly the box it exists to catch - with nothing on
/// stderr or in the artifact to tell "no history" from "history handed over and unreadable".
fn qualify_history_with(results_dir: &Path, handed: Option<&str>) -> Vec<f64> {
    let mut out = qualify_history_on_disk(results_dir);
    if let Some(raw) = handed {
        match raw.trim().parse::<f64>() {
            Ok(v) if v > 0.0 && v.is_finite() => out.push(v),
            _ => eprintln!(
                "suite: OTB_QUALIFY_BASELINE={raw:?} is not a positive rate, so it is ignored - the \
                 box qualification has only the on-disk history to judge against"
            ),
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
        gw_cores: cfg.gw_cores.clone(),
        declared_path: cfg.manifest.path.clone(),
        cell_paths: cfg
            .manifest
            .cell_paths_for(&cfg.gw_cores, cfg.mock_addr.port(), &cfg.gw_dir),
        matrix: cfg.manifest.matrix.clone(),
        matrix_note: cfg.manifest.matrix_note.clone(),
        untestable_cells: cfg.manifest.untestable.clone(),
        untestable_note: cfg.manifest.untestable_note.clone(),
        // How to put this gateway back at rest, so the memory group can read an idle that is
        // actually idle. Built from the SAME manifest declaration the initial launch used, so a
        // restart cannot differ from the launch it is repeating. `None` for a manifest that declares
        // no launch: the harness does not own that gateway's lifetime and must not bounce it.
        //
        // A declaration that does not RESOLVE is also `None` - `otb run` has already refused to
        // launch it by then, so this is reachable only from other callers - but never silently:
        // without a relaunch the memory idle reading and every mid-grid restart are disabled.
        relaunch: match cfg.manifest.launch_spec(
            &cfg.gw_cores,
            cfg.mock_addr.port(),
            &cfg.gw_dir,
            Duration::from_secs(60),
            Duration::from_secs(2),
        ) {
            Some(Ok(spec)) => Some(spec),
            Some(Err(e)) => {
                eprintln!(
                    "suite: the manifest's launch declaration does not resolve ({e}), so the harness \
                     cannot restart this gateway: no idle memory reading and no mid-grid restarts"
                );
                None
            }
            None => None,
        },
        // Replayed on every restart: a docker stop destroys the writable layer, and with it any
        // configuration these commands wrote (one-api's channels and quota live in an in-container
        // database). See `RunConfig::relaunch_commands`.
        relaunch_commands: cfg.manifest.commands.clone(),
        // The launcher `restart_to_rest` reuses across every cell this run measures, so a native
        // child it spawns is still reachable - and reapable - the next time this same gateway is
        // put back at rest.
        relaunch_launcher: Default::default(),
        // The gateway's own headers, resolved once for the run.
        //
        // REFUSED, NOT DEFAULTED. This was `.unwrap_or_default()`, which turned an unresolvable
        // header into an EMPTY header set and measured the whole run without it. A comment here once
        // justified "NONE rather than a partial set" - and that reasoning is sound for a partial set -
        // but it does not cover resolution failing outright, which is what the default silently did.
        //
        // The consequence was the worst this harness can produce: a gateway whose auth header failed
        // to resolve serves nothing, every cell records `served: false`, and the board publishes that
        // the GATEWAY does not work when the harness dropped its credentials. `otb run` calls only
        // `manifest.validate()`, which checks headers for shell-style `$` and never exercises `{...}`
        // substitution, so an unknown placeholder passes the gate and arrives here.
        static_headers: cfg
            .manifest
            .headers_for("", &cfg.gw_cores, cfg.mock_addr.port(), &cfg.gw_dir)
            .map_err(|e| crate::snapshot::SnapshotError::UnresolvableHeader {
                detail: e.to_string(),
            })?,
        // THE SAME REFUSAL AS `static_headers` ABOVE, because this is the same defect one lane over.
        //
        // `static_headers` was changed from `unwrap_or_default()` to `?` so an unresolvable header
        // stops the run instead of measuring without it. This sibling still swallowed the identical
        // error with `.ok()?` - and inside a `filter_map`, which is WORSE than the empty-set failure it
        // was fixed for: the dialect is DROPPED from the map entirely, `run.rs`'s
        // `cfg.egress_headers.get(egress)` finds nothing, and every cell on that egress runs with no
        // routing or auth header at all. Each records `served: false`, and the board publishes that the
        // GATEWAY does not serve that dialect when the harness dropped its headers.
        //
        // Collected into a Result so the first failure propagates with its manifest error, rather than
        // filtered - a per-dialect resolution failure is a manifest defect, not a dialect this gateway
        // declines to serve, and those two must never look alike.
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
                // headers_for prepends the always-on set; the run config carries those separately, so
                // strip them here rather than sending each one twice.
                // Same refusal again: this resolves the always-on set purely to subtract it, but a
                // failure here would silently leave the duplicates in (or, with the old `.ok()?`, drop
                // the dialect) rather than saying the manifest cannot be resolved.
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
    require_qualified_box(&box_qualify)?;

    let mut upstreams: HashMap<String, Upstream> = HashMap::new();
    let mut any_served = false;
    let mut last_egress: Option<String> = None;

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
                    Ok(_) => {}
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
            /* MEMORY IS WITHHELD ON A REFUTED CELL TOO. `withhold_if_refuted` took only perf and
            stream, and memory was built independently right here - so a cell PROVEN to have
            forwarded the ingress request rather than translating it published every perf and
            stream number as `not_served` with evidence, and its `peak_rss_mib` untouched beside
            them. `withhold_refuted_perf`'s own rule is "CPU burned serving a wire that is not
            this pairing is not this pairing's cost", and RSS is that rule verbatim: the load
            window drove this cell's path/model/headers, which re-verification just proved is not
            this pairing.

            It reached the board, too: `site/gen-data.mjs` reads memory solely from the per-cell
            window, gates only on `cell.served === true` - still true for a refuted cell - and
            never looks at `perf_dropped`. */
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
        let cell_served = matches!(cell.served, RecServed::Bool(true));
        let upstream = upstreams
            .entry(eg)
            // `configurable` is whether this gateway can be POINTED at this upstream at all, which
            // is exactly what the manifest's `egress` list declares, so it is read from there rather
            // than hardcoded. It was `true` unconditionally, which made the field a constant: every
            // egress column claimed to be configured, including ones the manifest never wired, and a
            // constant that is published as an observation is the shape of a false claim even when
            // it happens to be right.
            //
            // `served` is OBSERVED THE SAME WAY: the column came up if at least one cell was served
            // through it. It was a constant `true` for every column with a row, including columns
            // whose every cell was untestable or refused - and `lib/promote_guard.py` reads
            // `upstreams.<egress>.served` as "this matrix served something", so the constant made a
            // boot-failed matrix look served and let it overwrite good data.
            .or_insert_with(|| Upstream {
                configurable,
                served: false,
                ..Default::default()
            });
        upstream.served |= cell_served;
        upstream.cells.insert(ing, cell);
    });

    // The final write always happens, so a grid with a single egress column is not lost.
    flush(cfg, &upstreams, any_served, Some(box_qualify))
}

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
                // THE PUBLISHED THROUGHPUT ANSWER. One reading per declared tail-latency bound, off
                // the same rungs `sweep_max_proxy` below carries - so a reader can re-derive every
                // reading from the sweep rather than taking the frontier on trust.
                p.frontier = series.frontier.clone();
                p.sweep_max_proxy = series.sweep.clone();
                p.sweep_max_proxy_interrupted = series.sweep_interrupted;
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
    // The same withholding for the f64-typed fields; one detail string, two element types.
    let withheld_f =
        || -> Measurement<f64> { Measurement::absent_because(Absent::NotServed, detail.clone()) };
    CellPerf {
        sweep_max_proxy_interrupted: p.sweep_max_proxy_interrupted,
        added_latency_p50_us: withheld(),
        added_latency_p99_us: withheld(),
        gateway_c1_p99_us: withheld(),
        direct_c1_p99_us: withheld(),
        // THE COST GOES TOO, for exactly the reason the latency legs do: CPU burned serving a wire
        // that is not this pairing is not this pairing's cost. Withholding the rate while publishing
        // what it cost would leave a cost-per-request for a request the cell never served.
        cpu_us_per_request: withheld_f(),
        rps_per_cpu_second: withheld_f(),
        cost_window_conc: withheld(),
        cost_window_ok: withheld_f(),
        cost_window_rps: withheld_f(),
        cost_core_utilisation: withheld_f(),
        cost_threads: withheld_f(),
        cost_nonvol_ctxt_per_request: withheld_f(),
        cost_majflt: withheld_f(),
        // THE FRONTIER GOES TOO, for the reason the sweeps do: every reading in it is rps against
        // concurrency measured over a wire that is not this pairing, and a reading is as much a
        // published number as a ceiling drawn from it.
        frontier: Vec::new(),
        sweep_max_proxy: Vec::new(),
        // THE EVIDENCE, kept verbatim: this is the whole reason the block survives.
        egress_reverified: p.egress_reverified,
        reverify_note: p.reverify_note,
        // A sample-count note about legs that measured the wrong wire would describe the weight of a
        // number nobody is publishing.
        c1_note: None,
    }
}

/// Every measured field of a refuted cell's memory window, withheld with the reason - same rule and
/// same wording as `withhold_refuted_perf`, applied to the resource the load window consumed.
///
/// The series are cleared as well as the scalars: a curve drawn from a window served over the wrong
/// wire is the same false claim as a number taken from it, and the board draws whatever it is given.
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
        // The counts are withheld with the percentiles they weigh: publishing a sample count beside a
        // withheld number would describe the weight of a figure this cell is refusing to state.
        ttft_gw_samples: withheld(&detail),
        ttft_direct_samples: withheld(&detail),
        added_gap_p50_us: withheld(&detail),
        added_gap_p99_us: withheld(&detail),
        streams_sustained: withheld(&detail),
        streams_sustained_fps: withheld(&detail),
        streams_sustained_mock_ceiling: None,
        streams_sustained_headroom: None,
        sweep_streams: Vec::new(),
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

fn built_gateway_provenance(dir: &Path) -> Result<Option<serde_json::Value>, SnapshotError> {
    let path = dir.join(".build-provenance.json");
    match std::fs::read(&path) {
        Ok(bytes) => Ok(Some(
            serde_json::from_slice(&bytes).map_err(SnapshotError::Json)?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SnapshotError::Io { path, source }),
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
    // Resolved once and used at BOTH levels. `gateway_build` re-resolves the manifest's launch spec,
    // and the matrix must name the same build the root does or the two levels of one artifact would
    // disagree about what was measured.
    let build =
        gateway_build(cfg).unwrap_or_else(|| format!("otb-engine {}", env!("CARGO_PKG_VERSION")));
    let snap = ResultSnapshot {
        schema_version: 1,
        gateway_provenance: built_gateway_provenance(&cfg.gw_dir)?,
        definitions: metric_definitions(),
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
#[path = "tests/suite_tests.rs"]
mod tests;
